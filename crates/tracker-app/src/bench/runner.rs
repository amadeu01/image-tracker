use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use tracker_core::{
    BarPath, CircleTracker, ColorModel, ColorModelConfig, ColorTracker, FrameSource, Point, Sample,
    Source, StepOutcome, TemplateTracker, Timebase, Track, Tracker, TrackerKind,
    TrackerSuggestionConfig,
};

use crate::ffmpeg_source::FfmpegFrameSource;
use crate::tracking::{self, AnyTracker, TrackerSelection, TrackerTuning};

use super::strategy::{
    compute_metrics, gate_reason, strategy_matrix, BenchmarkRow, FrameOutcome, Strategy,
};
use super::CompareError;

/// Default segment length (in frames) sampled by `compare` when `--frames`
/// isn't given -- the "~200-frame segment" PLAN 11.4 asks for.
pub const DEFAULT_COMPARE_FRAMES: u64 = 200;

/// Default `Track` uncertainty growth rate (px/s of coasting, 17.2) used by
/// `run_strategy`'s standalone benchmark loop -- mirrors
/// `TrackingSessionConfig`'s default (see `session.rs`).
const DEFAULT_COAST_UNCERTAINTY_GROWTH: f64 = 20.0;
/// Where a strategy's overlay video goes (14.1): `--out-dir` when given,
/// otherwise next to the source video; named `<video stem>.<slug>.overlay.mp4`.
pub fn overlay_output_path(video_path: &Path, out_dir: Option<&Path>, slug: &str) -> PathBuf {
    let stem = video_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("out");
    let dir = out_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| video_path.parent().unwrap_or(Path::new(".")).to_path_buf());
    dir.join(format!("{stem}.{slug}.overlay.mp4"))
}

/// Converts one strategy run's per-frame outcomes into a `BarPath` for the
/// shared overlay renderer (14.1). The seed itself becomes point 0 (at
/// `seed_frame`); outcome `i` corresponds to video frame `seed_frame + i + 1`
/// (`run_strategy` steps the tracker on the frames *after* the seed frame).
/// Misses are simply skipped — the overlay renderer draws whatever points
/// exist, so gaps show up as breaks in the drawn path.
pub fn outcomes_to_bar_path(
    outcomes: &[FrameOutcome],
    seed_frame: u64,
    seed_position: Point,
    timebase: Timebase,
) -> BarPath {
    let mut samples = Vec::with_capacity(outcomes.len() + 1);
    samples.push(Sample {
        frame_index: 0, // session-relative; BarPath::new shifts by start_frame
        position: seed_position,
        source: Source::Tracked,
        confidence: None,
    });
    for (i, outcome) in outcomes.iter().enumerate() {
        if let FrameOutcome::Found { position, .. } = *outcome {
            samples.push(Sample {
                frame_index: i as u64 + 1,
                position,
                source: Source::Tracked,
                confidence: None,
            });
        }
    }
    BarPath::new(&samples, &[], timebase, seed_frame)
}

/// One strategy's full run: its `FrameOutcome`s plus an optional advisory
/// note (11.4: color strategies where the seed's color is not distinct from
/// the background per `suggest_tracker`'s own heuristic still run -- the
/// note just tells the reader why a low `tracked_pct` might be expected)
/// and, separately (20.3), whether/why this strategy is gated out of the
/// benchmark's "viable candidate" set (`gate_reason`).
#[derive(Debug, Clone)]
pub struct StrategyRun {
    pub strategy: Strategy,
    pub outcomes: Vec<FrameOutcome>,
    pub note: Option<String>,
    pub gated_reason: Option<String>,
}

/// Runs one strategy over `frames` frames starting at `seed_frame_index`,
/// re-decoding the segment from scratch with a fresh `FfmpegFrameSource`
/// (see module doc for why). `base_tuning`'s `preprocessor` field is
/// overridden by `strategy.filter`; every other tuning field (patch/search
/// radius etc) is shared across all six strategies so only the filter and
/// tracker kind vary.
#[allow(clippy::too_many_arguments)]
pub fn run_strategy(
    video_path: &Path,
    width: u32,
    height: u32,
    seed_frame_index: u64,
    seed_position: Point,
    frames: u64,
    dt: f64,
    strategy: Strategy,
    base_tuning: &TrackerTuning,
) -> Result<StrategyRun, CompareError> {
    let mut tuning = base_tuning.clone();
    tuning.preprocessor = strategy.filter.chain();

    let mut source = FfmpegFrameSource::spawn(video_path, width, height)
        .map_err(|e| format!("failed to spawn ffmpeg decoder: {e}"))?;
    let seed_frame = tracking::decode_up_to(&mut source, seed_frame_index)
        .map_err(|e| format!("failed to decode up to seed frame: {e}"))?
        .ok_or_else(|| "video ended before reaching the seed frame".to_string())?;

    let mut note = None;
    let mut suggestion = None;
    if strategy.tracker == TrackerSelection::Color {
        let suggested = tracker_core::suggest_tracker(
            &seed_frame,
            seed_position,
            TrackerSuggestionConfig::default(),
        );
        suggestion = Some(suggested);
        if suggested == TrackerKind::Template {
            note = Some(
                "seed color is not distinct from the background per suggest_tracker \
                 (would recommend Template); running Color anyway for comparison"
                    .to_string(),
            );
        }
    }
    let gated_reason = gate_reason(strategy.tracker, suggestion);

    let tracker_config = tracking::tracker_config(tuning.clone());
    let color_tracker_config = tracking::color_tracker_config(tuning);

    let mut tracker = match strategy.tracker {
        TrackerSelection::Template => AnyTracker::Template(
            TemplateTracker::new(&seed_frame, seed_position, tracker_config)
                .map_err(|e| format!("seed patch out of bounds: {e:?}"))?,
        ),
        TrackerSelection::Color => {
            let model = ColorModel::learn(
                &seed_frame,
                seed_position,
                tracker_config.patch_radius(),
                ColorModelConfig::default(),
            )
            .map_err(|e| format!("seed patch out of bounds for color model: {e:?}"))?;
            AnyTracker::Color(ColorTracker::new(model, color_tracker_config))
        }
        TrackerSelection::Circle => AnyTracker::Circle(
            CircleTracker::new(
                &seed_frame,
                seed_position,
                tracking::default_circle_tracker_config(),
            )
            .map_err(|e| format!("no circle found at seed: {e:?}"))?,
        ),
        TrackerSelection::Auto => unreachable!("strategy_matrix never produces Auto"),
    };

    // `frames` may be u64::MAX for a `--full` run on a video whose frame
    // count ffprobe couldn't report; cap the pre-allocation, Vec grows fine.
    let mut outcomes = Vec::with_capacity(frames.min(DEFAULT_COMPARE_FRAMES * 40) as usize);
    // 17.2: motion state fed to the tracker, advancing by prediction
    // (`Track::coasted`) through a Miss rather than freezing, and re-derived
    // from the observation (`Track::observed`) on a Found — mirrors
    // `TrackingSession::step`'s handling, just without the gap/coast
    // bookkeeping this standalone benchmark loop doesn't need.
    let mut track = Track::new(seed_position);
    for _ in 0..frames {
        let frame = match source
            .next_frame()
            .map_err(|e| format!("decode error mid-segment: {e}"))?
        {
            Some(f) => f,
            None => break, // segment ran past end of video; report what we have
        };
        match tracker.step(&frame, &track, dt) {
            StepOutcome::Found {
                position, score, ..
            } => {
                outcomes.push(FrameOutcome::Found { position, score });
                track = track.observed(position, dt);
            }
            StepOutcome::Miss => {
                outcomes.push(FrameOutcome::Miss);
                track = track.coasted(dt, DEFAULT_COAST_UNCERTAINTY_GROWTH);
            }
        }
    }

    Ok(StrategyRun {
        strategy,
        outcomes,
        note,
        gated_reason,
    })
}

/// Runs every strategy in `strategy_matrix()` over the segment and returns
/// one `BenchmarkRow` per strategy, in matrix order. A single strategy
/// failing outright (e.g. seed patch out of bounds) is recorded as a
/// zero-frame result with the error as its note, rather than aborting the
/// whole benchmark -- the other five strategies' results are still useful.
#[allow(clippy::too_many_arguments)]
pub fn run_benchmark(
    video_path: &Path,
    width: u32,
    height: u32,
    seed_frame_index: u64,
    seed_position: Point,
    frames: u64,
    dt: f64,
    coast_limit: u32,
    base_tuning: &TrackerTuning,
) -> Vec<BenchmarkRow> {
    strategy_matrix()
        .into_iter()
        .map(|strategy| {
            match run_strategy(
                video_path,
                width,
                height,
                seed_frame_index,
                seed_position,
                frames,
                dt,
                strategy,
                base_tuning,
            ) {
                Ok(run) => BenchmarkRow {
                    strategy,
                    metrics: compute_metrics(&run.outcomes, coast_limit),
                    note: run.note,
                    gated_reason: run.gated_reason,
                },
                Err(e) => BenchmarkRow {
                    strategy,
                    metrics: compute_metrics(&[], coast_limit),
                    note: Some(format!("strategy failed: {e}")),
                    gated_reason: gate_reason(strategy.tracker, None),
                },
            }
        })
        .collect()
}

/// A message sent from the background benchmark worker (GUI "Test
/// strategies" button, task 11.4) to the UI thread.
#[derive(Debug, Clone)]
pub enum BenchmarkMessage {
    /// About to start (or just finished) strategy `strategy_index` of
    /// `total`, so the side panel can show "3/6" progress.
    Progress { strategy_index: usize, total: usize },
    /// Every strategy has run; the full set of rows, in matrix order.
    Done(Vec<BenchmarkRow>),
    /// The benchmark could not run at all (e.g. ffmpeg failed to spawn for
    /// the very first strategy's segment) -- distinct from a single
    /// strategy failing, which `run_benchmark` already records as a
    /// zero-frame row with a note rather than aborting the whole run.
    Error(String),
}

/// A handle to a running background benchmark: the read side of its
/// progress channel, mirroring `tracking::TrackingHandle`'s shape.
pub struct BenchmarkHandle {
    messages: Receiver<BenchmarkMessage>,
}

impl BenchmarkHandle {
    /// Takes every message the worker has sent so far, without blocking.
    pub fn drain(&self) -> Vec<BenchmarkMessage> {
        self.messages.try_iter().collect()
    }
}

/// Spawns a background thread that runs `strategy_matrix()`'s six strategies
/// in order over the given segment, sending `BenchmarkMessage`s as it goes
/// (task 11.4 GUI "Test strategies" button). Mirrors `tracking::spawn_tracking`'s
/// thread/channel shape so `app/state.rs` can poll it the same way
/// `poll_tracking` polls a `TrackingHandle`.
#[allow(clippy::too_many_arguments)]
pub fn spawn_benchmark(
    video_path: PathBuf,
    width: u32,
    height: u32,
    seed_frame_index: u64,
    seed_position: Point,
    frames: u64,
    dt: f64,
    coast_limit: u32,
    base_tuning: TrackerTuning,
) -> BenchmarkHandle {
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let matrix = strategy_matrix();
        let total = matrix.len();
        let mut rows = Vec::with_capacity(total);
        for (i, strategy) in matrix.into_iter().enumerate() {
            let _ = tx.send(BenchmarkMessage::Progress {
                strategy_index: i,
                total,
            });
            let row = match run_strategy(
                &video_path,
                width,
                height,
                seed_frame_index,
                seed_position,
                frames,
                dt,
                strategy,
                &base_tuning,
            ) {
                Ok(run) => BenchmarkRow {
                    strategy,
                    metrics: compute_metrics(&run.outcomes, coast_limit),
                    note: run.note,
                    gated_reason: run.gated_reason,
                },
                Err(e) => BenchmarkRow {
                    strategy,
                    metrics: compute_metrics(&[], coast_limit),
                    note: Some(format!("strategy failed: {e}")),
                    gated_reason: gate_reason(strategy.tracker, None),
                },
            };
            rows.push(row);
        }
        let _ = tx.send(BenchmarkMessage::Done(rows));
    });

    BenchmarkHandle { messages: rx }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::strategy::tests::found;

    // --- overlay_output_path (14.1) ---

    #[test]
    fn overlay_output_path_defaults_next_to_the_source_video() {
        let p = overlay_output_path(
            Path::new("/videos/my clip.mp4"),
            None,
            "gaussian1.5-template",
        );
        assert_eq!(
            p,
            PathBuf::from("/videos/my clip.gaussian1.5-template.overlay.mp4")
        );
    }

    #[test]
    fn overlay_output_path_uses_out_dir_when_given() {
        let p = overlay_output_path(
            Path::new("/videos/clip.mp4"),
            Some(Path::new("/tmp/overlays")),
            "none-color",
        );
        assert_eq!(
            p,
            PathBuf::from("/tmp/overlays/clip.none-color.overlay.mp4")
        );
    }

    // --- outcomes_to_bar_path (14.1) ---

    #[test]
    fn outcomes_to_bar_path_maps_found_outcomes_to_absolute_frames_after_seed() {
        // Seed at frame 10; outcomes cover frames 11, 12 (miss), 13.
        let outcomes = vec![
            found(1.0, 2.0, 0.9),
            FrameOutcome::Miss,
            found(3.0, 4.0, 0.9),
        ];
        let timebase = Timebase::new(30, 1).unwrap();
        let path = outcomes_to_bar_path(&outcomes, 10, Point::new(0.0, 0.0), timebase);
        let frames: Vec<u64> = path.points().iter().map(|p| p.frame_index).collect();
        assert_eq!(frames, vec![10, 11, 13]); // seed + 2 tracked; miss skipped
        assert_eq!(path.start_frame(), 10);
        assert_eq!(path.points()[0].position, Point::new(0.0, 0.0));
        assert_eq!(path.points()[2].position, Point::new(3.0, 4.0));
    }

    #[test]
    fn outcomes_to_bar_path_with_no_outcomes_still_has_the_seed_point() {
        let timebase = Timebase::new(30, 1).unwrap();
        let path = outcomes_to_bar_path(&[], 5, Point::new(7.0, 8.0), timebase);
        assert_eq!(path.points().len(), 1);
        assert_eq!(path.points()[0].frame_index, 5);
    }
}
