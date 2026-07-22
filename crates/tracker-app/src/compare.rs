//! `tracker-app compare <video> --seed-frame N --seed X,Y [--frames 200]
//! [--full] [--out results.json] [--export-overlays] [--out-dir dir]`
//! (tasks 11.4 + 14.1): runs the tracking pipeline over a
//! fixed-length segment (seed frame -> `+frames`, or the whole rest of the
//! video with `--full`) once per strategy in a
//! fixed matrix — {none, gaussian:1.5, median:3} filter chain x {template,
//! color} tracker kind (6 combinations) — and reports, per strategy: tracked
//! %, misses, gaps, auto-reseeds, mean match score (template only; the color
//! tracker's "score" is a fill-fraction, a different unit, so it's reported
//! separately and never averaged together with template scores), and mean
//! jitter (mean |delta position| between consecutive *tracked* samples).
//!
//! Each strategy re-decodes the segment from scratch (a fresh
//! `FfmpegFrameSource`, seeking up to `seed_frame_index` again): decoding
//! ~200 1080p frames once and keeping them all in memory would cost
//! ~340MB, and reusing one in-memory buffer across differently-filtered
//! trackers would be a correctness footgun (the whole point of 11.2/11.3's
//! same-space filtering invariant, see docs/theory.md §5, is that filtering
//! must be applied fresh per comparison, not smuggled in from a shared
//! decode). Six short re-decodes of one segment is cheap by comparison.
//!
//! Split the same way `tracking.rs` splits `TrackingRunState` from
//! `spawn_tracking`: `strategy_matrix`, `compute_metrics`, and `recommend`
//! are pure functions over plain data, unit-tested directly; `run_strategy`/
//! `run_benchmark` do the actual ffmpeg decode + `Tracker::step` IO and are
//! exercised by the manual `compare` runs against `test_videos/` instead.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use tracker_core::{
    BarPath, ColorModel, ColorModelConfig, ColorTracker, FrameSource, Point, Preprocessor,
    PreprocessorChain, Sample, Source, StepOutcome, TemplateTracker, Timebase, Track, Tracker,
    TrackerKind, TrackerSuggestionConfig,
};

use crate::ffmpeg_source::FfmpegFrameSource;
use crate::ffprobe;
use crate::overlay_export::render_overlay_video;
use crate::tracking::{self, AnyTracker, TrackerSelection, TrackerTuning};

pub type CompareError = String;

/// The filter-chain half of the strategy matrix (11.4): a fixed three
/// options, matching the ones already reachable via `--filter` (11.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKind {
    None,
    Gaussian1_5,
    Median3,
}

impl FilterKind {
    /// All three filter options, in the fixed order the matrix/table use.
    pub const ALL: [FilterKind; 3] = [
        FilterKind::None,
        FilterKind::Gaussian1_5,
        FilterKind::Median3,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FilterKind::None => "none",
            FilterKind::Gaussian1_5 => "gaussian:1.5",
            FilterKind::Median3 => "median:3",
        }
    }

    pub fn chain(self) -> PreprocessorChain {
        match self {
            FilterKind::None => PreprocessorChain::new(),
            FilterKind::Gaussian1_5 => {
                PreprocessorChain::from_steps(vec![Preprocessor::GaussianBlur { sigma: 1.5 }])
            }
            FilterKind::Median3 => {
                PreprocessorChain::from_steps(vec![Preprocessor::Median { k: 3 }])
            }
        }
    }
}

/// One cell of the strategy matrix: a filter chain paired with a forced
/// tracker kind (never `Auto` -- the whole point is comparing both kinds
/// explicitly, even where `suggest_tracker` would pick the other one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Strategy {
    pub filter: FilterKind,
    pub tracker: TrackerSelection,
}

impl Strategy {
    pub fn label(&self) -> String {
        let tracker = match self.tracker {
            TrackerSelection::Template => "template",
            TrackerSelection::Color => "color",
            TrackerSelection::Auto => "auto", // never actually produced by strategy_matrix
        };
        format!("{}/{tracker}", self.filter.label())
    }

    /// Filename-safe slug for this strategy (14.1), used in overlay video
    /// names: `<stem>.<slug>.overlay.mp4`. Derived from the same matrix
    /// labels as `label()` but with no `/`, `:`, or spaces — e.g.
    /// `gaussian1.5-template`, `median3-color`, `none-template`. Unique
    /// across `strategy_matrix()` because both halves are.
    pub fn slug(&self) -> String {
        let filter = match self.filter {
            FilterKind::None => "none",
            FilterKind::Gaussian1_5 => "gaussian1.5",
            FilterKind::Median3 => "median3",
        };
        let tracker = match self.tracker {
            TrackerSelection::Template => "template",
            TrackerSelection::Color => "color",
            TrackerSelection::Auto => "auto", // never produced by strategy_matrix
        };
        format!("{filter}-{tracker}")
    }
}

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

/// The fixed 3x2 strategy matrix (11.4): {none, gaussian:1.5, median:3} x
/// {template, color}, in a stable order so table rows/JSON entries/the
/// `recommend` tie-break are all reproducible run to run.
pub fn strategy_matrix() -> Vec<Strategy> {
    let mut out = Vec::with_capacity(6);
    for filter in FilterKind::ALL {
        out.push(Strategy {
            filter,
            tracker: TrackerSelection::Template,
        });
        out.push(Strategy {
            filter,
            tracker: TrackerSelection::Color,
        });
    }
    out
}

/// One frame's `Tracker::step` result, kept in the shape `compute_metrics`
/// needs (a plain enum over position+score, decoupled from
/// `tracker_core::StepOutcome` only so this module's pure functions don't
/// need a live `Frame`/`Tracker` to unit test against).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FrameOutcome {
    Found { position: Point, score: f64 },
    Miss,
}

/// Computed metrics for one strategy's run (11.4). `mean_score`/`mean_jitter`
/// are `None` when there weren't enough tracked samples to average (zero
/// tracked frames, or exactly one for jitter).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrategyMetrics {
    pub frames_total: u64,
    pub tracked_pct: f64,
    pub misses: u64,
    pub gaps: u64,
    pub reseeds: u64,
    pub mean_score: Option<f64>,
    pub mean_jitter: Option<f64>,
}

/// Reduces a strategy's per-frame outcomes to `StrategyMetrics`. Pure and
/// synthesizable directly (no decode/tracker needed), so this is the core of
/// 11.4's TDD surface.
///
/// - `gaps`: the number of separate miss-streaks (a transition from a
///   tracked/start frame into `Miss`), not the raw miss count.
/// - `reseeds`: how many times a miss-streak would exceed `coast_limit` and
///   force an auto-reseed (mirroring the CLI `track` subcommand's headless
///   auto-resume, `cli::run_track`) -- the streak counter resets to zero
///   each time this fires, so a very long loss is counted as a fresh gap
///   each time it triggers a reseed (a reseed conceptually closes the
///   current gap; any further misses open a new one).
/// - `mean_score`: averaged only over `Found` outcomes; callers decide the
///   unit label (correlation for template, fill-fraction for color) --
///   this function doesn't know which tracker produced the outcomes.
/// - `mean_jitter`: mean Euclidean distance between *consecutive tracked*
///   samples (skipping over any intervening misses), not consecutive
///   frames.
pub fn compute_metrics(outcomes: &[FrameOutcome], coast_limit: u32) -> StrategyMetrics {
    let total = outcomes.len() as u64;
    let mut tracked = 0u64;
    let mut misses = 0u64;
    let mut gaps = 0u64;
    let mut reseeds = 0u64;
    let mut consecutive_miss: u32 = 0;
    let mut score_sum = 0.0;
    let mut score_count = 0u64;
    let mut last_tracked: Option<Point> = None;
    let mut jitter_sum = 0.0;
    let mut jitter_count = 0u64;

    for outcome in outcomes {
        match *outcome {
            FrameOutcome::Found { position, score } => {
                tracked += 1;
                score_sum += score;
                score_count += 1;
                if let Some(prev) = last_tracked {
                    let dx = position.x - prev.x;
                    let dy = position.y - prev.y;
                    jitter_sum += (dx * dx + dy * dy).sqrt();
                    jitter_count += 1;
                }
                last_tracked = Some(position);
                consecutive_miss = 0;
            }
            FrameOutcome::Miss => {
                misses += 1;
                if consecutive_miss == 0 {
                    gaps += 1;
                }
                consecutive_miss += 1;
                if consecutive_miss > coast_limit {
                    reseeds += 1;
                    consecutive_miss = 0;
                }
            }
        }
    }

    StrategyMetrics {
        frames_total: total,
        tracked_pct: if total == 0 {
            0.0
        } else {
            tracked as f64 / total as f64 * 100.0
        },
        misses,
        gaps,
        reseeds,
        mean_score: (score_count > 0).then_some(score_sum / score_count as f64),
        mean_jitter: (jitter_count > 0).then_some(jitter_sum / jitter_count as f64),
    }
}

/// Picks the recommended strategy's index: highest `tracked_pct` wins; on an
/// exact tie, lowest `mean_jitter` wins (a strategy with no tracked samples
/// at all -- `mean_jitter: None` -- sorts as worst-possible jitter, i.e.
/// loses every tie it's part of); if *that's* also tied (including both
/// `None`), the earlier strategy in `strategy_matrix`'s fixed order wins,
/// since `best` is only replaced on a strict improvement below. Returns
/// `None` for an empty slice.
pub fn recommend(results: &[StrategyMetrics]) -> Option<usize> {
    if results.is_empty() {
        return None;
    }
    let mut best = 0usize;
    for (i, cand) in results.iter().enumerate().skip(1) {
        let best_m = &results[best];
        let better = cand.tracked_pct > best_m.tracked_pct
            || (cand.tracked_pct == best_m.tracked_pct
                && cand.mean_jitter.unwrap_or(f64::INFINITY)
                    < best_m.mean_jitter.unwrap_or(f64::INFINITY));
        if better {
            best = i;
        }
    }
    Some(best)
}

/// One strategy's full run: its `FrameOutcome`s plus an optional advisory
/// note (11.4: color strategies where the seed's color is not distinct from
/// the background per `suggest_tracker`'s own heuristic still run -- the
/// note just tells the reader why a low `tracked_pct` might be expected).
#[derive(Debug, Clone)]
pub struct StrategyRun {
    pub strategy: Strategy,
    pub outcomes: Vec<FrameOutcome>,
    pub note: Option<String>,
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
    if strategy.tracker == TrackerSelection::Color {
        let suggestion = tracker_core::suggest_tracker(
            &seed_frame,
            seed_position,
            TrackerSuggestionConfig::default(),
        );
        if suggestion == TrackerKind::Template {
            note = Some(
                "seed color is not distinct from the background per suggest_tracker \
                 (would recommend Template); running Color anyway for comparison"
                    .to_string(),
            );
        }
    }

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
    })
}

/// One row of the finished benchmark: a strategy, its metrics, and any note.
#[derive(Debug, Clone)]
pub struct BenchmarkRow {
    pub strategy: Strategy,
    pub metrics: StrategyMetrics,
    pub note: Option<String>,
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
                },
                Err(e) => BenchmarkRow {
                    strategy,
                    metrics: compute_metrics(&[], coast_limit),
                    note: Some(format!("strategy failed: {e}")),
                },
            }
        })
        .collect()
}

/// Score unit label for a strategy's tracker kind, used by both the table
/// and the JSON export so a reader never mistakes a color fill-fraction for
/// a template correlation score.
fn score_unit(tracker: TrackerSelection) -> &'static str {
    match tracker {
        TrackerSelection::Template => "correlation",
        TrackerSelection::Color => "fill-fraction",
        TrackerSelection::Auto => "?",
    }
}

/// Renders an aligned stdout table plus a recommendation line, given the
/// already-computed rows.
pub fn format_table(rows: &[BenchmarkRow]) -> String {
    let metrics: Vec<StrategyMetrics> = rows.iter().map(|r| r.metrics).collect();
    let winner = recommend(&metrics);

    let mut out = String::new();
    out.push_str(&format!(
        "{:<18} {:>10} {:>7} {:>5} {:>8} {:>12} {:>10}\n",
        "strategy", "tracked%", "misses", "gaps", "reseeds", "mean score", "jitter(px)"
    ));
    for (i, row) in rows.iter().enumerate() {
        let m = &row.metrics;
        let score = match m.mean_score {
            Some(s) => format!("{:.3} ({})", s, score_unit(row.strategy.tracker)),
            None => "-".to_string(),
        };
        let jitter = m
            .mean_jitter
            .map(|j| format!("{j:.2}"))
            .unwrap_or_else(|| "-".to_string());
        let marker = if winner == Some(i) { " *" } else { "" };
        out.push_str(&format!(
            "{:<18} {:>9.1}% {:>7} {:>5} {:>8} {:>16} {:>10}{marker}\n",
            row.strategy.label(),
            m.tracked_pct,
            m.misses,
            m.gaps,
            m.reseeds,
            score,
            jitter
        ));
        if let Some(note) = &row.note {
            out.push_str(&format!("  note: {note}\n"));
        }
    }
    if let Some(w) = winner {
        out.push_str(&format!(
            "\nrecommendation: {} (highest tracked%, tie-break: lowest jitter)\n",
            rows[w].strategy.label()
        ));
    }
    out
}

/// Machine-readable JSON export of the benchmark (11.4's `--out`).
pub fn to_json(rows: &[BenchmarkRow]) -> String {
    let metrics: Vec<StrategyMetrics> = rows.iter().map(|r| r.metrics).collect();
    let winner = recommend(&metrics);

    let entries: Vec<String> = rows
        .iter()
        .map(|row| {
            let m = &row.metrics;
            format!(
                "{{\"strategy\":\"{}\",\"filter\":\"{}\",\"tracker\":\"{}\",\
                 \"frames_total\":{},\"tracked_pct\":{},\"misses\":{},\"gaps\":{},\
                 \"reseeds\":{},\"mean_score\":{},\"score_unit\":\"{}\",\"mean_jitter_px\":{},\"note\":{}}}",
                row.strategy.label(),
                row.strategy.filter.label(),
                score_unit(row.strategy.tracker),
                m.frames_total,
                m.tracked_pct,
                m.misses,
                m.gaps,
                m.reseeds,
                m.mean_score
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "null".to_string()),
                score_unit(row.strategy.tracker),
                m.mean_jitter
                    .map(|j| j.to_string())
                    .unwrap_or_else(|| "null".to_string()),
                row.note
                    .as_ref()
                    .map(|n| format!("\"{}\"", n.replace('"', "'")))
                    .unwrap_or_else(|| "null".to_string()),
            )
        })
        .collect();

    format!(
        "{{\"strategies\":[{}],\"recommendation\":{}}}",
        entries.join(","),
        winner
            .map(|w| format!("\"{}\"", rows[w].strategy.label()))
            .unwrap_or_else(|| "null".to_string())
    )
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
    pub messages: Receiver<BenchmarkMessage>,
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
                },
                Err(e) => BenchmarkRow {
                    strategy,
                    metrics: compute_metrics(&[], coast_limit),
                    note: Some(format!("strategy failed: {e}")),
                },
            };
            rows.push(row);
        }
        let _ = tx.send(BenchmarkMessage::Done(rows));
    });

    BenchmarkHandle { messages: rx }
}

/// Default segment length (in frames) sampled by `compare` when `--frames`
/// isn't given -- the "~200-frame segment" PLAN 11.4 asks for.
pub const DEFAULT_COMPARE_FRAMES: u64 = 200;

/// Default `Track` uncertainty growth rate (px/s of coasting, 17.2) used by
/// `run_strategy`'s standalone benchmark loop -- mirrors
/// `TrackingSessionConfig`'s default (see `session.rs`).
const DEFAULT_COAST_UNCERTAINTY_GROWTH: f64 = 20.0;

/// Fallback dt (seconds/frame) used when a video's reported fps is
/// degenerate (zero numerator/denominator) -- keeps the benchmark runnable
/// rather than blocking all motion-model reasoning on ffprobe metadata that
/// only overlay rendering strictly needs (see the `Timebase` handling
/// below).
const FALLBACK_DT: f64 = 1.0 / 30.0;

/// Parsed `compare <video> --seed-frame N --seed X,Y [--frames N] [--full]
/// [--out path] [--export-overlays] [--out-dir dir]` arguments.
#[derive(Debug)]
pub struct CompareArgs {
    pub video_path: PathBuf,
    pub seed_frame: u64,
    pub seed: Point,
    pub frames: u64,
    pub out_path: Option<PathBuf>,
    /// 14.1: benchmark over the whole video (from the seed frame to the
    /// end) instead of the default ~200-frame segment. Overrides `--frames`
    /// when both are given.
    pub full: bool,
    /// 14.1: after each strategy's run, render its bar path as an overlay
    /// video (`<stem>.<slug>.overlay.mp4`) via the shared overlay renderer.
    pub export_overlays: bool,
    /// 14.1: where `--export-overlays` writes its videos. Defaults to the
    /// source video's directory when absent.
    pub overlay_dir: Option<PathBuf>,
}

/// Parses `compare` subcommand args, reusing the same `--seed-frame`/`--seed`
/// flag shapes as `track` (`cli::parse_track_args`).
pub fn parse_compare_args(args: &[String]) -> Result<CompareArgs, CompareError> {
    let mut video_path: Option<PathBuf> = None;
    let mut seed_frame: Option<u64> = None;
    let mut seed: Option<Point> = None;
    let mut frames = DEFAULT_COMPARE_FRAMES;
    let mut out_path: Option<PathBuf> = None;
    let mut full = false;
    let mut export_overlays = false;
    let mut overlay_dir: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--seed-frame" => {
                let v = args.get(i + 1).ok_or("--seed-frame needs a value")?;
                seed_frame = Some(v.parse().map_err(|_| format!("bad --seed-frame: {v}"))?);
                i += 2;
            }
            "--seed" => {
                let v = args.get(i + 1).ok_or("--seed needs a value (X,Y)")?;
                let (x, y) = v
                    .split_once(',')
                    .ok_or_else(|| format!("bad --seed (expected X,Y): {v}"))?;
                let x: f64 = x.trim().parse().map_err(|_| format!("bad --seed x: {v}"))?;
                let y: f64 = y.trim().parse().map_err(|_| format!("bad --seed y: {v}"))?;
                seed = Some(Point::new(x, y));
                i += 2;
            }
            "--frames" => {
                let v = args.get(i + 1).ok_or("--frames needs a value")?;
                frames = v.parse().map_err(|_| format!("bad --frames: {v}"))?;
                i += 2;
            }
            "--out" => {
                let v = args.get(i + 1).ok_or("--out needs a value")?;
                out_path = Some(PathBuf::from(v));
                i += 2;
            }
            "--full" => {
                full = true;
                i += 1;
            }
            "--export-overlays" => {
                export_overlays = true;
                i += 1;
            }
            "--out-dir" => {
                let v = args.get(i + 1).ok_or("--out-dir needs a value")?;
                overlay_dir = Some(PathBuf::from(v));
                i += 2;
            }
            other if video_path.is_none() && !other.starts_with("--") => {
                video_path = Some(PathBuf::from(other));
                i += 1;
            }
            other => return Err(format!("unrecognized argument: {other}")),
        }
    }

    Ok(CompareArgs {
        video_path: video_path.ok_or("missing <video> argument")?,
        seed_frame: seed_frame.ok_or("missing --seed-frame")?,
        seed: seed.ok_or("missing --seed")?,
        frames,
        out_path,
        full,
        export_overlays,
        overlay_dir,
    })
}

/// Runs the `compare` subcommand: probes the video, runs the full strategy
/// matrix over the `--frames`-length segment starting at `--seed-frame`
/// (or the whole rest of the video with `--full`), prints the aligned table
/// and recommendation to stdout, and (if `--out` was given) writes the
/// machine-readable JSON alongside it. With `--export-overlays`, each
/// strategy's bar path is additionally rendered as its own overlay video
/// (one at a time, streaming, via the shared `render_overlay_video` — never
/// six videos' worth of frames in memory at once).
pub fn run_compare(args: CompareArgs) -> Result<(), CompareError> {
    let metadata = ffprobe::probe(&args.video_path)
        .map_err(|e| format!("failed to probe {}: {e}", args.video_path.display()))?;

    let base_tuning = TrackerTuning::default();
    let coast_limit = tracking::DEFAULT_COAST_LIMIT;

    // --full overrides --frames: run from the seed frame to the end of the
    // video. When ffprobe can't report a frame count, decode until EOF
    // (run_strategy already stops cleanly at end of stream).
    let frames = if args.full {
        metadata
            .frame_count
            .map(|n| n.saturating_sub(args.seed_frame))
            .unwrap_or(u64::MAX)
    } else {
        args.frames
    };

    if let Some(dir) = args.overlay_dir.as_deref().filter(|_| args.export_overlays) {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("failed to create out dir {}: {e}", dir.display()))?;
    }

    // Only overlay rendering needs a timebase — a video with degenerate fps
    // metadata must still be benchmarkable without --export-overlays
    // (review finding on ed48dd1).
    let timebase = if args.export_overlays {
        Some(
            Timebase::new(metadata.fps_num, metadata.fps_den)
                .map_err(|e| format!("bad video fps for {}: {e:?}", args.video_path.display()))?,
        )
    } else {
        None
    };

    // dt (17.2) for the tracker's motion model: derived from fps when
    // available, falling back to `FALLBACK_DT` rather than blocking the
    // whole benchmark on the same degenerate-fps case the timebase above
    // already tolerates when overlays aren't requested.
    let dt = Timebase::new(metadata.fps_num, metadata.fps_den)
        .map(|tb| 1.0 / tb.fps())
        .unwrap_or(FALLBACK_DT);

    tracing::info!(
        video = %args.video_path.display(),
        seed_frame = args.seed_frame,
        frames,
        full = args.full,
        export_overlays = args.export_overlays,
        strategy_count = strategy_matrix().len(),
        "compare benchmark started"
    );

    // Same loop as `run_benchmark`, unrolled here so each strategy's
    // per-frame outcomes are still in hand for the optional overlay render
    // (then dropped before the next strategy decodes -- one strategy's
    // outcomes at a time, never six).
    let matrix = strategy_matrix();
    let total = matrix.len();
    let mut rows = Vec::with_capacity(total);
    for (i, strategy) in matrix.into_iter().enumerate() {
        let run = run_strategy(
            &args.video_path,
            metadata.display_width(),
            metadata.display_height(),
            args.seed_frame,
            args.seed,
            frames,
            dt,
            strategy,
            &base_tuning,
        );
        let row = match run {
            Ok(run) => {
                let mut note = run.note;
                if let Some(timebase) = timebase.filter(|_| args.export_overlays) {
                    let slug = strategy.slug();
                    let out_path =
                        overlay_output_path(&args.video_path, args.overlay_dir.as_deref(), &slug);
                    tracing::info!(
                        strategy = %strategy.label(),
                        overlay_path = %out_path.display(),
                        "compare overlay render started"
                    );
                    let bar_path =
                        outcomes_to_bar_path(&run.outcomes, args.seed_frame, args.seed, timebase);
                    // One overlay failing must not abort the remaining
                    // strategies' overlays/rows — degrade to a note, same
                    // policy as a failed strategy run (review finding on
                    // ed48dd1).
                    match render_overlay_video(
                        &args.video_path,
                        &out_path,
                        metadata.display_width(),
                        metadata.display_height(),
                        metadata.fps_num,
                        metadata.fps_den,
                        &bar_path,
                        &[],
                    ) {
                        Ok(()) => {
                            tracing::info!(
                                strategy = %strategy.label(),
                                overlay_path = %out_path.display(),
                                "compare overlay render done"
                            );
                            println!(
                                "[{}/{}] {} — overlay written: {}",
                                i + 1,
                                total,
                                strategy.label(),
                                out_path.display()
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                strategy = %strategy.label(),
                                overlay_path = %out_path.display(),
                                error = %e,
                                "compare overlay render failed"
                            );
                            println!(
                                "[{}/{}] {} — overlay FAILED: {e}",
                                i + 1,
                                total,
                                strategy.label(),
                            );
                            let msg = format!("overlay render failed: {e}");
                            note = Some(match note {
                                Some(existing) => format!("{existing}; {msg}"),
                                None => msg,
                            });
                        }
                    }
                }
                BenchmarkRow {
                    strategy,
                    metrics: compute_metrics(&run.outcomes, coast_limit),
                    note,
                }
            }
            Err(e) => BenchmarkRow {
                strategy,
                metrics: compute_metrics(&[], coast_limit),
                note: Some(format!("strategy failed: {e}")),
            },
        };
        rows.push(row);
    }

    let winner_label = recommend(&rows.iter().map(|r| r.metrics).collect::<Vec<_>>())
        .map(|i| rows[i].strategy.label());
    tracing::info!(
        strategy_count = rows.len(),
        winner = winner_label.as_deref(),
        "compare benchmark done"
    );

    let segment = if args.full {
        "whole video".to_string()
    } else {
        format!("{frames} frame segment")
    };
    println!(
        "{}: strategy benchmark, seed frame {} @ ({:.1},{:.1}), {}",
        args.video_path.display(),
        args.seed_frame,
        args.seed.x,
        args.seed.y,
        segment
    );
    print!("{}", format_table(&rows));

    if let Some(out_path) = &args.out_path {
        std::fs::write(out_path, to_json(&rows))
            .map_err(|e| format!("failed to write {}: {e}", out_path.display()))?;
        println!("wrote {}", out_path.display());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- strategy_matrix ---

    #[test]
    fn strategy_matrix_has_six_entries_in_fixed_order() {
        let matrix = strategy_matrix();
        assert_eq!(matrix.len(), 6);
        let expected = [
            (FilterKind::None, TrackerSelection::Template),
            (FilterKind::None, TrackerSelection::Color),
            (FilterKind::Gaussian1_5, TrackerSelection::Template),
            (FilterKind::Gaussian1_5, TrackerSelection::Color),
            (FilterKind::Median3, TrackerSelection::Template),
            (FilterKind::Median3, TrackerSelection::Color),
        ];
        for (strategy, (filter, tracker)) in matrix.iter().zip(expected.iter()) {
            assert_eq!(strategy.filter, *filter);
            assert_eq!(strategy.tracker, *tracker);
        }
    }

    #[test]
    fn strategy_label_includes_filter_and_tracker() {
        let s = Strategy {
            filter: FilterKind::Gaussian1_5,
            tracker: TrackerSelection::Color,
        };
        assert_eq!(s.label(), "gaussian:1.5/color");
    }

    #[test]
    fn filter_kind_chain_matches_expected_preprocessor_steps() {
        assert!(FilterKind::None.chain().is_empty());
        assert_eq!(
            FilterKind::Gaussian1_5.chain().steps(),
            &[Preprocessor::GaussianBlur { sigma: 1.5 }]
        );
        assert_eq!(
            FilterKind::Median3.chain().steps(),
            &[Preprocessor::Median { k: 3 }]
        );
    }

    // --- compute_metrics ---

    fn found(x: f64, y: f64, score: f64) -> FrameOutcome {
        FrameOutcome::Found {
            position: Point::new(x, y),
            score,
        }
    }

    #[test]
    fn compute_metrics_all_found_has_full_tracked_pct_and_no_gaps() {
        let outcomes = vec![
            found(0.0, 0.0, 0.9),
            found(1.0, 0.0, 0.9),
            found(2.0, 0.0, 0.9),
        ];
        let m = compute_metrics(&outcomes, 5);
        assert_eq!(m.frames_total, 3);
        assert_eq!(m.tracked_pct, 100.0);
        assert_eq!(m.misses, 0);
        assert_eq!(m.gaps, 0);
        assert_eq!(m.reseeds, 0);
        assert_eq!(m.mean_score, Some(0.9));
    }

    #[test]
    fn compute_metrics_empty_outcomes_is_all_zero_none() {
        let m = compute_metrics(&[], 5);
        assert_eq!(m.frames_total, 0);
        assert_eq!(m.tracked_pct, 0.0);
        assert_eq!(m.mean_score, None);
        assert_eq!(m.mean_jitter, None);
    }

    #[test]
    fn compute_metrics_counts_gaps_as_streaks_not_raw_misses() {
        // found, miss, miss, found, miss, found -- two separate miss streaks.
        let outcomes = vec![
            found(0.0, 0.0, 0.8),
            FrameOutcome::Miss,
            FrameOutcome::Miss,
            found(1.0, 0.0, 0.8),
            FrameOutcome::Miss,
            found(2.0, 0.0, 0.8),
        ];
        let m = compute_metrics(&outcomes, 5);
        assert_eq!(m.misses, 3);
        assert_eq!(m.gaps, 2);
        assert_eq!(m.reseeds, 0); // never exceeds coast_limit=5 in one streak
    }

    #[test]
    fn compute_metrics_reseeds_once_a_streak_exceeds_coast_limit() {
        // 5 consecutive misses with coast_limit 3: reseed fires once the
        // streak passes 3 (on the 4th miss), then the counter resets so a
        // 5th miss starts a fresh (non-reseeding) streak.
        let outcomes = vec![
            found(0.0, 0.0, 0.8),
            FrameOutcome::Miss,
            FrameOutcome::Miss,
            FrameOutcome::Miss,
            FrameOutcome::Miss,
            FrameOutcome::Miss,
        ];
        let m = compute_metrics(&outcomes, 3);
        assert_eq!(m.misses, 5);
        // The streak resets on the auto-reseed (4th miss), so the 5th miss
        // starts a fresh streak/gap of its own: 2 gaps, 1 reseed.
        assert_eq!(m.gaps, 2);
        assert_eq!(m.reseeds, 1);
    }

    #[test]
    fn compute_metrics_jitter_is_mean_distance_between_consecutive_tracked_samples() {
        // Tracked at (0,0), (3,4) [dist 5], (3,4) [dist 0] -- skipping a Miss
        // in between the last two, which must not count as its own step.
        let outcomes = vec![
            found(0.0, 0.0, 0.8),
            found(3.0, 4.0, 0.8),
            FrameOutcome::Miss,
            found(3.0, 4.0, 0.8),
        ];
        let m = compute_metrics(&outcomes, 5);
        assert_eq!(m.mean_jitter, Some((5.0 + 0.0) / 2.0));
    }

    #[test]
    fn compute_metrics_jitter_is_none_with_fewer_than_two_tracked_samples() {
        let outcomes = vec![found(0.0, 0.0, 0.8), FrameOutcome::Miss];
        let m = compute_metrics(&outcomes, 5);
        assert_eq!(m.mean_jitter, None);
    }

    // --- recommend ---

    fn metrics(tracked_pct: f64, jitter: Option<f64>) -> StrategyMetrics {
        StrategyMetrics {
            frames_total: 100,
            tracked_pct,
            misses: 0,
            gaps: 0,
            reseeds: 0,
            mean_score: None,
            mean_jitter: jitter,
        }
    }

    #[test]
    fn recommend_picks_highest_tracked_pct() {
        let results = vec![
            metrics(80.0, Some(1.0)),
            metrics(95.0, Some(5.0)),
            metrics(50.0, Some(0.1)),
        ];
        assert_eq!(recommend(&results), Some(1));
    }

    #[test]
    fn recommend_tie_break_is_lowest_jitter() {
        let results = vec![
            metrics(90.0, Some(5.0)),
            metrics(90.0, Some(1.0)),
            metrics(90.0, Some(3.0)),
        ];
        assert_eq!(recommend(&results), Some(1));
    }

    #[test]
    fn recommend_full_tie_keeps_earliest_matrix_entry() {
        let results = vec![metrics(90.0, Some(2.0)), metrics(90.0, Some(2.0))];
        assert_eq!(recommend(&results), Some(0));
    }

    #[test]
    fn recommend_none_jitter_loses_ties_to_any_real_jitter() {
        let results = vec![metrics(90.0, None), metrics(90.0, Some(999.0))];
        assert_eq!(recommend(&results), Some(1));
    }

    #[test]
    fn recommend_empty_is_none() {
        assert_eq!(recommend(&[]), None);
    }

    // --- table/JSON formatting ---

    fn row(strategy: Strategy, tracked_pct: f64, jitter: Option<f64>) -> BenchmarkRow {
        BenchmarkRow {
            strategy,
            metrics: metrics(tracked_pct, jitter),
            note: None,
        }
    }

    #[test]
    fn format_table_marks_the_recommended_row() {
        let rows = vec![
            row(
                Strategy {
                    filter: FilterKind::None,
                    tracker: TrackerSelection::Template,
                },
                80.0,
                Some(2.0),
            ),
            row(
                Strategy {
                    filter: FilterKind::None,
                    tracker: TrackerSelection::Color,
                },
                95.0,
                Some(1.0),
            ),
        ];
        let table = format_table(&rows);
        assert!(table.contains("recommendation: none/color"));
    }

    #[test]
    fn to_json_includes_every_strategy_and_the_recommendation() {
        let rows = vec![
            row(
                Strategy {
                    filter: FilterKind::None,
                    tracker: TrackerSelection::Template,
                },
                80.0,
                Some(2.0),
            ),
            row(
                Strategy {
                    filter: FilterKind::Median3,
                    tracker: TrackerSelection::Color,
                },
                95.0,
                Some(1.0),
            ),
        ];
        let json = to_json(&rows);
        assert!(json.contains("\"none/template\""));
        assert!(json.contains("\"median:3/color\""));
        assert!(json.contains("\"recommendation\":\"median:3/color\""));
    }

    // --- slugs (14.1) ---

    #[test]
    fn strategy_slugs_are_filename_safe_and_unique_across_the_matrix() {
        let matrix = strategy_matrix();
        let slugs: Vec<String> = matrix.iter().map(|s| s.slug()).collect();
        for slug in &slugs {
            assert!(
                slug.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.'),
                "slug not filename-safe: {slug}"
            );
            assert!(!slug.contains(':'), "slug contains colon: {slug}");
            assert!(!slug.contains(' '), "slug contains space: {slug}");
        }
        let mut deduped = slugs.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(deduped.len(), matrix.len(), "slugs not unique: {slugs:?}");
    }

    #[test]
    fn strategy_slug_matches_expected_shape() {
        let expected = [
            "none-template",
            "none-color",
            "gaussian1.5-template",
            "gaussian1.5-color",
            "median3-template",
            "median3-color",
        ];
        for (strategy, want) in strategy_matrix().iter().zip(expected.iter()) {
            assert_eq!(strategy.slug(), *want);
        }
    }

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

    // --- parse_compare_args ---

    #[test]
    fn parses_compare_args_with_default_frames_and_no_out() {
        let args: Vec<String> = vec!["video.mp4", "--seed-frame", "789", "--seed", "312,430"]
            .into_iter()
            .map(String::from)
            .collect();
        let parsed = parse_compare_args(&args).unwrap();
        assert_eq!(parsed.video_path, PathBuf::from("video.mp4"));
        assert_eq!(parsed.seed_frame, 789);
        assert_eq!(parsed.seed, Point::new(312.0, 430.0));
        assert_eq!(parsed.frames, DEFAULT_COMPARE_FRAMES);
        assert_eq!(parsed.out_path, None);
        assert!(!parsed.full);
        assert!(!parsed.export_overlays);
        assert_eq!(parsed.overlay_dir, None);
    }

    #[test]
    fn parses_full_export_overlays_and_out_dir_flags() {
        let args: Vec<String> = vec![
            "video.mp4",
            "--seed-frame",
            "0",
            "--seed",
            "1,2",
            "--full",
            "--export-overlays",
            "--out-dir",
            "overlays/",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let parsed = parse_compare_args(&args).unwrap();
        assert!(parsed.full);
        assert!(parsed.export_overlays);
        assert_eq!(parsed.overlay_dir, Some(PathBuf::from("overlays/")));
    }

    #[test]
    fn parses_compare_args_with_frames_and_out_overrides() {
        let args: Vec<String> = vec![
            "video.mp4",
            "--seed-frame",
            "0",
            "--seed",
            "1,2",
            "--frames",
            "50",
            "--out",
            "results.json",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let parsed = parse_compare_args(&args).unwrap();
        assert_eq!(parsed.frames, 50);
        assert_eq!(parsed.out_path, Some(PathBuf::from("results.json")));
    }

    #[test]
    fn compare_args_missing_required_flag_is_an_error() {
        let args: Vec<String> = vec!["video.mp4", "--seed-frame", "0"]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(parse_compare_args(&args).is_err());
    }

    #[test]
    fn compare_args_bad_seed_format_is_an_error() {
        let args: Vec<String> = vec!["video.mp4", "--seed-frame", "0", "--seed", "nope"]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(parse_compare_args(&args).is_err());
    }
}
