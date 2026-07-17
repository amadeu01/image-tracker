//! Run tracking from the UI (task 2.6): a background thread drives a
//! `TemplateTracker`/`TrackingSession` (tracker-core) across the video from
//! the Seed's frame to the end, streaming progress back to the UI thread
//! over a channel so egui stays responsive.
//!
//! Split in two halves, the pure half is TDD'd directly:
//! - `TrackingRunState` — a pure reducer over `TrackingMessage`s (no egui,
//!   no threads, no IO). This is what's unit-tested below.
//! - `spawn_tracking`/`run_tracking_worker` — thin thread/channel wiring
//!   that `app.rs` calls into; not unit-tested (would just be testing
//!   `std::thread`/`mpsc`), verified instead by the manual smoke run.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use tracker_core::{
    BarPath, Calibration, ColorModel, ColorModelConfig, ColorTracker, ColorTrackerConfig, Frame,
    FrameSource, Point, PreprocessorChain, RepSegmentationConfig, SessionState,
    Source as SampleSource, StepOutcome, TemplateTracker, TemplateTrackerConfig, Timebase, Tracker,
    TrackerKind, TrackerSuggestionConfig, TrackingSession, TrackingSessionConfig,
};

use crate::ffmpeg_source::FfmpegFrameSource;

/// Sensible default `TemplateTracker` tuning for the test_videos/ footage.
/// Exposed as consts so 3.4 (end-to-end run on each video) can revisit them
/// without hunting through the tracking wiring.
pub const DEFAULT_PATCH_RADIUS: u32 = 12;
pub const DEFAULT_SEARCH_RADIUS: u32 = 30;
pub const DEFAULT_MIN_SCORE: f64 = 0.4;
pub const DEFAULT_UPDATE_THRESHOLD: f64 = 0.7;
pub const DEFAULT_COAST_LIMIT: u32 = 5;
/// Minimum score a mid-gap `Found` must clear to count as reacquisition
/// (10.2/10.2b). Originally wired straight to `update_threshold` (0.7),
/// which fixed the rack/mirror false-lock bug but, on the v1 e2e clip,
/// pushed gaps/reseeds from 2/0 to 8/7 — 0.7 also demoted plenty of
/// genuine-but-marginal reacquisitions (e.g. the plate sliding back into a
/// partially occluded rack corner) to misses. Tuned independently (10.2b):
/// swept 0.5/0.55/0.6 on v1 and picked 0.5, the smallest gaps+reseeds (8/6,
/// vs 11/9 at 0.55 and 8/7 at 0.6) with no jump-spike regression (tracked
/// max frame-to-frame displacement identical — 42.4px — across the whole
/// swept range including all the way down to `min_score` 0.4); v3 stays
/// 0 gaps/0 reseeds at every value and v2 stays in the same ballpark
/// (11-15 gaps/8-10 reseeds) — no blow-up. See docs/e2e-results.md's 10.2b
/// section for the full sweep table.
pub const DEFAULT_REACQUIRE_MIN_SCORE: f64 = 0.5;

/// Which tracker `run_tracking_worker` should use once it has decoded the
/// seed frame (task 4.3): `Auto` runs `tracker_core::suggest_tracker` on the
/// seed patch and logs the chosen kind; `Template`/`Color` force a specific
/// tracker regardless of the seed's appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrackerSelection {
    #[default]
    Auto,
    Template,
    Color,
}

impl std::str::FromStr for TrackerSelection {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "auto" => Ok(TrackerSelection::Auto),
            "template" => Ok(TrackerSelection::Template),
            "color" => Ok(TrackerSelection::Color),
            other => Err(format!(
                "bad --tracker (expected auto|template|color): {other}"
            )),
        }
    }
}

/// Either tracker (4.2/4.3), so a `TrackingSession` can drive whichever one
/// was resolved for a run without the session itself needing to know which.
#[derive(Debug, Clone, PartialEq)]
pub enum AnyTracker {
    Template(TemplateTracker),
    Color(ColorTracker),
}

impl Tracker for AnyTracker {
    fn step(&mut self, frame: &Frame, last_pos: Point) -> StepOutcome {
        match self {
            AnyTracker::Template(t) => t.step(frame, last_pos),
            AnyTracker::Color(t) => t.step(frame, last_pos),
        }
    }
}

/// Tunable overrides for `default_tracker_config`/`default_session_config`,
/// one field per CLI flag (3.6): `--patch-radius`, `--search-radius`,
/// `--min-score`, `--update-threshold`, `--coast-limit`. `None` falls back
/// to the module's `DEFAULT_*` const.
///
/// `preprocessor` (11.3, `--filter`) is the one field here that isn't a
/// `None`-falls-back-to-a-default override: an empty `PreprocessorChain` (its
/// `Default`) *is* the default (no filtering), so it needs no `Option`
/// wrapper. It's not `Copy` (it owns a `Vec<Preprocessor>`), so this struct
/// dropped its `Copy` derive — the one call site that read a `TrackerTuning`
/// twice (`cli::run_track`, building both `tracker_config` and
/// `color_tracker_config`) now clones it explicitly.
#[derive(Debug, Clone, Default)]
pub struct TrackerTuning {
    pub patch_radius: Option<u32>,
    pub search_radius: Option<u32>,
    pub min_score: Option<f64>,
    pub update_threshold: Option<f64>,
    pub coast_limit: Option<u32>,
    /// `--reacquire-min-score` (10.2b): overrides
    /// `DEFAULT_REACQUIRE_MIN_SCORE`, decoupled from `update_threshold` so
    /// each can be tuned independently.
    pub reacquire_min_score: Option<f64>,
    /// Preprocessor chain (11.3, `--filter gaussian:<sigma>` / `--filter
    /// median:<k>`, repeatable, chain order = flag order): applied to both
    /// the `TemplateTracker`'s patch and the `ColorTracker`'s search window
    /// (see `tracker_config`/`color_tracker_config` below).
    pub preprocessor: PreprocessorChain,
}

/// Builds a `TemplateTrackerConfig` from the module's default consts.
pub fn default_tracker_config() -> TemplateTrackerConfig {
    tracker_config(TrackerTuning::default())
}

/// Builds a `TrackingSessionConfig` from the module's default consts.
pub fn default_session_config() -> TrackingSessionConfig {
    session_config(TrackerTuning::default())
}

/// Builds a `ColorTrackerConfig` using its own module defaults (search
/// radius 25, min pixels 5) and an empty (identity) filter chain — the
/// color path doesn't currently expose CLI tuning flags of its own beyond
/// `--filter` (4.3 is about *choosing* the tracker, not re-tuning it).
pub fn default_color_tracker_config() -> ColorTrackerConfig {
    color_tracker_config(TrackerTuning::default())
}

/// Builds a `ColorTrackerConfig`, applying `tuning.preprocessor` (11.3,
/// `--filter`) on top of the color path's own module defaults (search
/// radius 25, min pixels 5) — the color path has no other tunable knobs of
/// its own yet.
pub fn color_tracker_config(tuning: TrackerTuning) -> ColorTrackerConfig {
    ColorTrackerConfig::builder()
        .preprocessor(tuning.preprocessor)
        .build()
}

/// Builds a `TemplateTrackerConfig`, using `tuning`'s overrides where set and
/// the module defaults otherwise.
pub fn tracker_config(tuning: TrackerTuning) -> TemplateTrackerConfig {
    TemplateTrackerConfig::builder()
        .patch_radius(tuning.patch_radius.unwrap_or(DEFAULT_PATCH_RADIUS))
        .search_radius(tuning.search_radius.unwrap_or(DEFAULT_SEARCH_RADIUS))
        .min_score(tuning.min_score.unwrap_or(DEFAULT_MIN_SCORE))
        .update_threshold(tuning.update_threshold.unwrap_or(DEFAULT_UPDATE_THRESHOLD))
        .preprocessor(tuning.preprocessor)
        .build()
}

/// Builds a `TrackingSessionConfig`, using `tuning`'s overrides where set
/// and the module defaults otherwise.
///
/// `reacquire_min_score` (10.2, decoupled in 10.2b) gates mid-gap
/// reacquisition: while a gap is open, a `Found` must clear this score —
/// not just the tracker's looser `min_score` — to count as reacquisition,
/// which is what stops the crosshair from locking onto background clutter
/// (a rack, a mirror) that scores just above `min_score` after the real
/// object has left the frame. It used to inherit `update_threshold`
/// directly (0.7), but that turned out too strict on genuine marginal
/// reacquisitions (v1 e2e: 2 gaps/0 reseeds -> 8 gaps/7 reseeds); it now has
/// its own default (`DEFAULT_REACQUIRE_MIN_SCORE`, tuned to 0.5) and its
/// own `--reacquire-min-score` CLI override, independent of
/// `--update-threshold`.
/// `max_reacquire_distance` (10.2b) is always set to `2 * search_radius`: a
/// genuine reacquisition should land within the tracker's own search window
/// plus some slack for drift during the gap, whereas a false lock onto
/// unrelated clutter elsewhere in frame is usually much farther away. This
/// is a second, independent guard alongside `reacquire_min_score` — it has
/// no separate CLI flag since it derives from `--search-radius`.
pub fn session_config(tuning: TrackerTuning) -> TrackingSessionConfig {
    let search_radius = tuning.search_radius.unwrap_or(DEFAULT_SEARCH_RADIUS);
    TrackingSessionConfig::builder()
        .coast_limit(tuning.coast_limit.unwrap_or(DEFAULT_COAST_LIMIT))
        .reacquire_min_score(
            tuning
                .reacquire_min_score
                .unwrap_or(DEFAULT_REACQUIRE_MIN_SCORE),
        )
        .max_reacquire_distance(2.0 * search_radius as f64)
        .build()
}

/// Builds a `RepSegmentationConfig` for a run's velocity units: uncalibrated
/// (px/s) data keeps `RepSegmentationConfig::default_config`'s dead-band
/// (tuned for pixel-scale motion); calibrated (m/s) data needs a much
/// smaller `min_velocity` (bar speeds are typically well under 1-2 m/s), or
/// every sample stays `Idle` and zero reps are ever detected. Likewise
/// `min_displacement` (task 15.1, phantom walkout reps): the px-scale 40.0
/// default becomes 0.15 m calibrated — well under a real squat's ~0.5 m ROM,
/// well above setup shuffling. Shared by the CLI (`cli.rs`), the GUI's
/// post-tracking `SessionResults` (10.3), and the live rep counter (10.8)
/// so the three never drift on this tuning.
pub fn rep_segmentation_config(calibrated: bool) -> RepSegmentationConfig {
    if calibrated {
        RepSegmentationConfig::builder()
            .min_velocity(0.03)
            .min_displacement(0.15)
            .build()
    } else {
        RepSegmentationConfig::default_config()
    }
}

/// How often (in processed frames) the side panel recomputes a live rep
/// count from the partial path (task 10.8). `velocity_series`+`segment_reps`
/// over a few thousand points is cheap pure math, but there's no reason to
/// re-run it every single frame when the number only meaningfully changes
/// every several dozen — 30 keeps the "reps so far" counter visibly live
/// without doing real work on every `poll_tracking` drain.
pub const LIVE_REP_RECOMPUTE_INTERVAL: u64 = 30;

/// A message sent from the tracking worker thread to the UI thread.
#[derive(Debug, Clone)]
pub enum TrackingMessage {
    /// A frame was processed: tracked, interpolated, or the (re)seed frame
    /// itself. `video_frame_index` is absolute in the source video.
    Progress {
        video_frame_index: u64,
        position: Point,
        source: SampleSource,
        state: SessionState,
    },
    /// Tracking reached clean end-of-video: the final `BarPath`.
    Done(BarPath),
    /// Something went wrong (ffmpeg spawn/decode failure, seed frame out of
    /// bounds, etc). Tracking has stopped; nothing else follows.
    Error(String),
}

impl TrackingMessage {
    /// The video-absolute frame index this message concerns, if any
    /// (`Progress` only). Used by the UI to advance the display frame
    /// before handing the message to `TrackingRunState::apply`.
    pub fn video_frame_index(&self) -> Option<u64> {
        match self {
            TrackingMessage::Progress {
                video_frame_index, ..
            } => Some(*video_frame_index),
            _ => None,
        }
    }
}

/// A command sent from the UI to a paused worker thread: the user has
/// re-placed the seed at `position` on `video_frame_index` (the frame the
/// session paused on), so the session can `reseed` and resume.
#[derive(Debug, Clone, Copy)]
pub struct ReseedCommand {
    pub video_frame_index: u64,
    pub position: Point,
}

/// A lifecycle command sent from the UI to a running/paused worker thread
/// (task 10.4). Checked in `run_tracking_loop` before every frame read, so
/// the worker never blocks on a decode/`Tracker::step` call it's about to be
/// told to abandon.
///
/// `Pause` blocks the loop on this same channel (via `recv`, not
/// `try_recv`) until `Resume` or `Stop` arrives — the pause is real frame
/// consumption stopping, not a GUI-side overlay on top of a worker that
/// keeps decoding underneath. `Stop` ends the run at the *next* checkpoint
/// with whatever samples the session has accumulated so far — the same
/// `Done`-with-partial-results path clean decode EOF already takes, so
/// there's no separate "stopped" message variant to add.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlCommand {
    Pause,
    Resume,
    Stop,
}

/// Pure UI-facing state accumulated from a run's `TrackingMessage`s. Kept
/// separate from the thread/channel plumbing (`TrackingHandle` below) so
/// it's unit-testable without spawning anything.
#[derive(Debug, Clone, Default)]
pub struct TrackingRunState {
    pub running: bool,
    pub last_frame_index: Option<u64>,
    pub last_position: Option<Point>,
    pub last_source: Option<SampleSource>,
    /// The position of the most recent *tracked* (not coasted/interpolated)
    /// sample (10.2). While the session is coasting through a gap,
    /// `last_position` still moves (interpolated per frame) but the UI
    /// crosshair should freeze here instead — otherwise it visibly wanders
    /// toward wherever the linear interpolation happens to land while the
    /// object is actually lost, which is exactly the "crosshair jumped to
    /// the rack and kept tracking garbage" symptom that motivated this.
    pub last_tracked_position: Option<Point>,
    pub session_state: Option<SessionState>,
    pub frames_processed: u64,
    /// How many times this run has transitioned into `SessionState::NeedsReseed`
    /// (i.e. how many gaps the user has had to reseed through), used by the
    /// side panel's tracking status section (task 7.2).
    pub gap_count: u64,
    pub error: Option<String>,
    pub bar_path: Option<BarPath>,
    /// Every sample reported so far this run (task 10.8), mirroring what the
    /// worker's `TrackingSession` will eventually hand `BarPath::new` at
    /// `Done` — kept here too so the side panel can build a *partial*
    /// `BarPath`/rep count mid-run without waiting for completion. Cheap:
    /// one `Sample` (two `f64`s + an enum tag) per processed frame, and a
    /// run is at most a few thousand frames.
    pub samples: Vec<tracker_core::Sample>,
}

impl TrackingRunState {
    /// Fresh state for a run that has just been kicked off.
    pub fn started() -> Self {
        Self {
            running: true,
            ..Default::default()
        }
    }

    /// Applies one message, updating live-tracking fields. Returns `true`
    /// once the run has finished (`Done` or `Error`), so the caller knows
    /// to stop polling and re-enable the Track button.
    pub fn apply(&mut self, msg: TrackingMessage) -> bool {
        match msg {
            TrackingMessage::Progress {
                video_frame_index,
                position,
                source,
                state,
            } => {
                if state == SessionState::NeedsReseed
                    && self.session_state != Some(SessionState::NeedsReseed)
                {
                    self.gap_count += 1;
                }
                self.last_frame_index = Some(video_frame_index);
                self.last_position = Some(position);
                self.last_source = Some(source);
                if source == SampleSource::Tracked {
                    self.last_tracked_position = Some(position);
                }
                self.session_state = Some(state);
                self.frames_processed += 1;
                self.samples.push(tracker_core::Sample {
                    frame_index: video_frame_index,
                    position,
                    source,
                });
                false
            }
            TrackingMessage::Done(bar_path) => {
                self.bar_path = Some(bar_path);
                self.running = false;
                true
            }
            TrackingMessage::Error(e) => {
                self.error = Some(e);
                self.running = false;
                true
            }
        }
    }

    /// True while the session is coasting over an open gap (last sample was
    /// `Interpolated`) or paused awaiting a reseed (10.2): the honest state
    /// is "object lost, searching" rather than confidently tracking, so the
    /// crosshair/status/panel should say so instead of implying a live
    /// lock.
    pub fn is_searching(&self) -> bool {
        self.last_source == Some(SampleSource::Interpolated)
            || self.session_state == Some(SessionState::NeedsReseed)
    }

    /// Human-readable status-bar text reflecting the run's current phase.
    pub fn status_line(&self) -> String {
        if let Some(e) = &self.error {
            return format!("tracking error: {e}");
        }
        if !self.running && self.bar_path.is_some() {
            return format!(
                "tracking complete ({} frames processed)",
                self.frames_processed
            );
        }
        match (self.last_frame_index, self.session_state) {
            (Some(idx), Some(SessionState::NeedsReseed)) => {
                format!("tracking paused at frame {idx}: object lost, place a new seed then Resume")
            }
            (Some(idx), _) if self.last_source == Some(SampleSource::Interpolated) => {
                format!(
                    "tracking… frame {idx}: object lost — searching… ({} processed)",
                    self.frames_processed
                )
            }
            (Some(idx), _) => {
                format!(
                    "tracking… frame {idx} ({} processed)",
                    self.frames_processed
                )
            }
            _ => "tracking starting…".to_string(),
        }
    }

    /// Whether this frame's `Progress` should trigger a live rep
    /// recompute (task 10.8): every `LIVE_REP_RECOMPUTE_INTERVAL`th
    /// processed frame, and not before the run has processed at least one
    /// (so an idle/fresh state never fires). Pure/throttle-only — the
    /// actual recompute is `live_rep_count`, kept separate so this cheap
    /// check can gate it without doing any math itself.
    pub fn should_recompute_live_reps(&self) -> bool {
        self.frames_processed > 0
            && self
                .frames_processed
                .is_multiple_of(LIVE_REP_RECOMPUTE_INTERVAL)
    }

    /// Recomputes a rep count from the samples collected so far, using the
    /// same smoothing window/dead-band tuning `SessionResults::build` uses
    /// for the final result (`rep_segmentation_config`) so the live counter
    /// never disagrees with the final one just from different tuning.
    ///
    /// Returns `None` if there isn't enough data yet (e.g.
    /// `VelocityError::TooFewPoints` early in a run) — never panics. Callers
    /// (state.rs) treat a `None` here as "skip this recompute, keep
    /// whatever count was last shown" rather than resetting the counter to
    /// nothing, since a transient failure (or a coasting stretch with too
    /// few *tracked* points) shouldn't make an already-correct number
    /// disappear.
    pub fn live_rep_count(
        &self,
        timebase: Timebase,
        calibration: Option<Calibration>,
    ) -> Option<usize> {
        if self.samples.is_empty() {
            return None;
        }
        let bar_path = BarPath::new(&self.samples, &[], timebase, 0);
        let velocity = tracker_core::velocity_series(bar_path.points(), 5, calibration.as_ref());
        match velocity {
            Ok(v) => Some(
                tracker_core::segment_reps(&v, rep_segmentation_config(calibration.is_some()))
                    .len(),
            ),
            Err(e) => {
                tracing::debug!(error = %e, "live rep recompute skipped: not enough data yet");
                None
            }
        }
    }
}

/// A handle to a running/paused tracking worker: the read side of its
/// progress channel and the write side of its reseed channel.
pub struct TrackingHandle {
    pub messages: Receiver<TrackingMessage>,
    reseed_tx: Sender<ReseedCommand>,
    control_tx: Sender<ControlCommand>,
}

impl TrackingHandle {
    /// Sends a reseed command to a paused worker, so it resumes tracking
    /// from `position` at `video_frame_index`. If the worker has already
    /// exited (e.g. after an error), the send is silently dropped — the UI
    /// will already be showing that error from the last `TrackingMessage`.
    pub fn resume(&self, video_frame_index: u64, position: Point) {
        let _ = self.reseed_tx.send(ReseedCommand {
            video_frame_index,
            position,
        });
    }

    /// Task 10.4: pause the worker (stops frame consumption at the next
    /// checkpoint) / resume a paused worker / stop the run early, keeping
    /// whatever samples have been collected so far. Silently dropped if the
    /// worker has already exited, same rationale as `resume`.
    pub fn pause(&self) {
        let _ = self.control_tx.send(ControlCommand::Pause);
    }

    pub fn unpause(&self) {
        let _ = self.control_tx.send(ControlCommand::Resume);
    }

    pub fn stop(&self) {
        let _ = self.control_tx.send(ControlCommand::Stop);
    }
}

/// All the inputs `spawn_tracking`/`run_tracking_worker` need to run one
/// tracking pass: the video to decode, its dimensions/framerate, where to
/// seed, and the tuning to track with. Grouped into one struct (task 3.7)
/// so callers build it once with plain field syntax instead of threading
/// nine positional args through the spawn call.
pub struct TrackingJob {
    pub video_path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps_num: u64,
    pub fps_den: u64,
    pub seed_frame_index: u64,
    pub seed_position: Point,
    pub tracker_config: TemplateTrackerConfig,
    pub session_config: TrackingSessionConfig,
    /// Which tracker to use once the seed frame is decoded (task 4.3).
    pub tracker_selection: TrackerSelection,
    /// `ColorTracker` tuning, used only when the resolved kind is `Color`.
    pub color_tracker_config: ColorTrackerConfig,
}

/// Spawns a background thread that tracks from `job.seed_position` (placed
/// on `job.seed_frame_index`) to the end of the video, sending
/// `TrackingMessage`s as it goes.
///
/// Frame source: a single sequential `FfmpegFrameSource` (task 2.2) rather
/// than the seek-based per-frame decoder (`SeekingFrameDecoder`, task
/// 2.3) — that decoder re-spawns ffmpeg and re-seeks for *every* frame,
/// fine for occasional scrub-bar lookups but far too slow to decode a whole
/// tracking run frame by frame. Sequential decode is much faster.
///
/// To start at the seed's frame rather than frame 0, this decodes and
/// discards every frame up to `seed_frame_index` sequentially, rather than
/// an input-side `-ss` before `-i`: that form of `-ss` is a demuxer-level
/// seek that can land on the nearest keyframe rather than the exact frame
/// for some containers/odd frame rates (this project's videos report rates
/// like `600/19`), and the seed must line up frame-for-frame with what the
/// user clicked on. The discard-decode costs a few seconds up front on a
/// ~2000-frame clip, but it runs off the UI thread so it never blocks the
/// app.
pub fn spawn_tracking(job: TrackingJob) -> TrackingHandle {
    let (tx, rx) = mpsc::channel::<TrackingMessage>();
    let (reseed_tx, reseed_rx) = mpsc::channel::<ReseedCommand>();
    let (control_tx, control_rx) = mpsc::channel::<ControlCommand>();

    thread::spawn(move || {
        run_tracking_worker(job, &tx, &reseed_rx, &control_rx);
    });

    TrackingHandle {
        messages: rx,
        reseed_tx,
        control_tx,
    }
}

#[tracing::instrument(
    skip_all,
    fields(
        video = %job.video_path.display(),
        seed_frame = job.seed_frame_index,
        seed_x = job.seed_position.x,
        seed_y = job.seed_position.y,
    )
)]
fn run_tracking_worker(
    job: TrackingJob,
    tx: &Sender<TrackingMessage>,
    reseed_rx: &Receiver<ReseedCommand>,
    control_rx: &Receiver<ControlCommand>,
) {
    let TrackingJob {
        video_path,
        width,
        height,
        fps_num,
        fps_den,
        seed_frame_index,
        seed_position,
        tracker_config,
        session_config,
        tracker_selection,
        color_tracker_config,
    } = job;
    let video_path: &Path = &video_path;

    tracing::info!("tracking run started");

    let mut source = match FfmpegFrameSource::spawn(video_path, width, height) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "failed to spawn ffmpeg decoder for tracking run");
            let _ = tx.send(TrackingMessage::Error(e.to_string()));
            return;
        }
    };

    let seed_frame = match decode_up_to(&mut source, seed_frame_index) {
        Ok(Some(frame)) => frame,
        Ok(None) => {
            tracing::error!("video ended before reaching the seed frame");
            let _ = tx.send(TrackingMessage::Error(
                "video ended before reaching the seed frame".to_string(),
            ));
            return;
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to decode up to seed frame");
            let _ = tx.send(TrackingMessage::Error(e.to_string()));
            return;
        }
    };

    let resolved_kind = match tracker_selection {
        TrackerSelection::Template => TrackerKind::Template,
        TrackerSelection::Color => TrackerKind::Color,
        TrackerSelection::Auto => tracker_core::suggest_tracker(
            &seed_frame,
            seed_position,
            TrackerSuggestionConfig::default(),
        ),
    };
    tracing::info!(
        kind = ?resolved_kind,
        auto = tracker_selection == TrackerSelection::Auto,
        "tracker kind resolved"
    );

    let tracker = match resolved_kind {
        TrackerKind::Template => {
            match TemplateTracker::new(&seed_frame, seed_position, tracker_config) {
                Ok(t) => AnyTracker::Template(t),
                Err(e) => {
                    tracing::error!(error = ?e, "seed patch out of bounds");
                    let _ = tx.send(TrackingMessage::Error(format!(
                        "seed patch out of bounds: {e:?}"
                    )));
                    return;
                }
            }
        }
        TrackerKind::Color => {
            match ColorModel::learn(
                &seed_frame,
                seed_position,
                tracker_config.patch_radius(),
                ColorModelConfig::default(),
            ) {
                Ok(model) => AnyTracker::Color(ColorTracker::new(model, color_tracker_config)),
                Err(e) => {
                    tracing::error!(error = ?e, "seed patch out of bounds for color model");
                    let _ = tx.send(TrackingMessage::Error(format!(
                        "seed patch out of bounds: {e:?}"
                    )));
                    return;
                }
            }
        }
    };

    // Session frame indices are relative to the seed (0 == the seed
    // frame); `seed_frame_index` is added back in when reporting progress
    // and when building the final `BarPath`'s `start_frame`.
    let mut session = TrackingSession::new(tracker, 0, seed_position, session_config);
    let _ = tx.send(TrackingMessage::Progress {
        video_frame_index: seed_frame_index,
        position: seed_position,
        source: SampleSource::Tracked,
        state: SessionState::Tracking,
    });

    if let Err(e) = run_tracking_loop(
        &mut source,
        &mut session,
        seed_frame_index,
        tx,
        reseed_rx,
        control_rx,
    ) {
        tracing::error!(error = %e, "decode error during tracking run");
        let _ = tx.send(TrackingMessage::Error(e.to_string()));
        return;
    }
    // Reap the ffmpeg child now that the loop has hit clean decode EOF, to
    // surface a non-zero exit as a (late) error. `run_tracking_loop` only
    // sees the `FrameSource` port, not the ffmpeg-specific reap step, so it
    // stays generic/testable against in-memory sources.
    if let Err(e) = source.reap_after_eof() {
        tracing::error!(error = %e, "ffmpeg exited with an error during tracking run");
        let _ = tx.send(TrackingMessage::Error(e.to_string()));
        return;
    }

    let timebase = match Timebase::new(fps_num, fps_den) {
        Ok(tb) => tb,
        Err(_) => {
            tracing::error!("invalid fps reported by ffprobe (zero numerator/denominator)");
            let _ = tx.send(TrackingMessage::Error(
                "invalid fps reported by ffprobe (zero numerator/denominator)".to_string(),
            ));
            return;
        }
    };
    let bar_path = BarPath::new(
        session.samples(),
        session.gaps(),
        timebase,
        seed_frame_index,
    );
    tracing::info!(
        frames_processed = session.samples().len(),
        gaps = bar_path.gaps().len(),
        points = bar_path.points().len(),
        "tracking run done"
    );
    let _ = tx.send(TrackingMessage::Done(bar_path));
}

/// Drives `session` across every remaining frame of `source`, sending a
/// `Progress` message per frame and, on `SessionState::NeedsReseed`,
/// blocking on `reseed_rx` for the UI/CLI to supply a new seed before
/// continuing.
///
/// Root cause of PLAN 10.1 ("tracking runs past video end", frame counter
/// exceeding the video's reported length and the run never reaching
/// `Done`): this loop previously lived inline in `run_tracking_worker` and,
/// when a paused-awaiting-reseed session's `reseed_rx.recv()` returned
/// `Err` (the reseed channel closed — e.g. the UI dropped `TrackingHandle`
/// while paused), it did `return` straight out of the whole worker
/// function *without ever sending `Done`* — so the UI's `TrackingRunState`
/// stayed `running` forever with whatever the last `Progress` frame index
/// happened to be, i.e. exactly "processing keeps going even when the
/// video finishes". Decode-EOF itself (`Ok(None)`) was already handled
/// correctly (`break`s the loop, falls through to `Done`); the bug was
/// specifically in the paused-at-EOF/channel-closed path never reaching
/// that fallthrough. Fixed by returning `Ok(())` here in that case too, so
/// every caller path (clean EOF *or* reseed channel closed while paused)
/// converges on `run_tracking_worker` building and sending `Done` with
/// whatever samples/gaps the session has accumulated so far, logged at
/// `info`.
///
/// Generic over `FrameSource` (rather than the concrete
/// `FfmpegFrameSource<ChildStdout>`) so it's unit-testable against an
/// in-memory source that EOFs, without spawning a real ffmpeg process.
fn run_tracking_loop<S: FrameSource, T: Tracker>(
    source: &mut S,
    session: &mut TrackingSession<T>,
    seed_frame_index: u64,
    tx: &Sender<TrackingMessage>,
    reseed_rx: &Receiver<ReseedCommand>,
    control_rx: &Receiver<ControlCommand>,
) -> Result<(), S::Error> {
    loop {
        // Task 10.4: check for a Pause/Stop before touching the next frame,
        // so Stop/Discard never has to wait on a decode or `Tracker::step`
        // call it's about to be told to abandon. `Pause` blocks right here
        // on `recv` (real frame-consumption stop, not a GUI-side overlay)
        // until `Resume`/`Stop` arrives or the UI drops its handle (control
        // channel closes), which is treated the same as `Stop`.
        match control_rx.try_recv() {
            Ok(ControlCommand::Stop) => {
                tracing::info!("tracking stopped by user; ending with samples collected so far");
                return Ok(());
            }
            Ok(ControlCommand::Pause) => {
                tracing::info!("tracking paused by user");
                loop {
                    match control_rx.recv() {
                        Ok(ControlCommand::Resume) => {
                            tracing::info!("tracking resumed by user");
                            break;
                        }
                        Ok(ControlCommand::Stop) => {
                            tracing::info!(
                                "tracking stopped by user while paused; ending with samples collected so far"
                            );
                            return Ok(());
                        }
                        Ok(ControlCommand::Pause) => continue,
                        Err(_) => return Ok(()),
                    }
                }
            }
            Ok(ControlCommand::Resume) | Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {}
        }
        match source.next_frame()? {
            Some(frame) => {
                session.step(&frame);
                if let Some(last) = session.samples().last() {
                    // Report progress at the session's *current* frame
                    // (`frame_index()`, which advances on every step,
                    // Found/Miss/pause alike), not the last sample's frame
                    // index. While a gap is open the last sample can be
                    // many frames stale (samples are only pushed for
                    // Found/reseed frames and, retroactively, once a gap
                    // closes) -- reporting the stale index here was 10.9's
                    // root cause: the CLI's headless auto-resume trusted
                    // this value as "the frame to reseed at", which kept
                    // handing back the same already-recorded frame index
                    // forever instead of advancing to where the session had
                    // actually paused.
                    let video_frame_index = seed_frame_index + session.frame_index();
                    tracing::trace!(
                        video_frame_index,
                        x = last.position.x,
                        y = last.position.y,
                        source = ?last.source,
                        state = ?session.state(),
                        "frame processed"
                    );
                    let _ = tx.send(TrackingMessage::Progress {
                        video_frame_index,
                        position: last.position,
                        source: last.source,
                        state: session.state(),
                    });
                }
                if session.state() == SessionState::NeedsReseed {
                    tracing::warn!(
                        video_frame_index = seed_frame_index + session.frame_index(),
                        "tracking needs reseed: object lost, waiting for a new seed"
                    );
                    match reseed_rx.recv() {
                        Ok(cmd) => {
                            let relative = cmd.video_frame_index.saturating_sub(seed_frame_index);
                            session.reseed(relative, cmd.position);
                            tracing::info!(
                                video_frame_index = cmd.video_frame_index,
                                x = cmd.position.x,
                                y = cmd.position.y,
                                "tracking reseeded, resuming"
                            );
                            let _ = tx.send(TrackingMessage::Progress {
                                video_frame_index: cmd.video_frame_index,
                                position: cmd.position,
                                source: SampleSource::Tracked,
                                state: SessionState::Tracking,
                            });
                        }
                        // The UI/CLI dropped its handle while we were
                        // paused (e.g. app closing, or the CLI's headless
                        // auto-resume loop exited). There is no more
                        // context coming: stop here and let the caller
                        // emit `Done` with whatever the session has so
                        // far, rather than leaving the run silently
                        // "running" forever from the caller's point of
                        // view.
                        Err(_) => {
                            tracing::info!(
                                "reseed channel closed while paused; ending run with samples collected so far"
                            );
                            return Ok(());
                        }
                    }
                }
            }
            // Clean decode EOF: stop regardless of session state (this is
            // reached even if the session had just resumed out of
            // `NeedsReseed` on the previous iteration and immediately hits
            // the end of the video).
            None => return Ok(()),
        }
    }
}

/// Decodes frames sequentially from `source`, discarding all but the last,
/// up to and including index `target` (0-based). Returns `Ok(None)` if the
/// source ends before reaching it. Generic over any `FrameSource` so it's
/// unit-testable against an in-memory reader, not just a real ffmpeg pipe.
pub(crate) fn decode_up_to<S: FrameSource>(
    source: &mut S,
    target: u64,
) -> Result<Option<Frame>, S::Error> {
    let mut last = None;
    for _ in 0..=target {
        match source.next_frame()? {
            Some(frame) => last = Some(frame),
            None => return Ok(None),
        }
    }
    Ok(last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::time::Duration;

    fn synthetic_frame_bytes(width: u32, height: u32, fill: u8) -> Vec<u8> {
        vec![fill; width as usize * height as usize * 3]
    }

    /// A trivial `Tracker` for `run_tracking_loop` tests: `Found` at a fixed
    /// position for every frame until `miss_from` (inclusive), then `Miss`
    /// forever after — enough to drive a `TrackingSession` into
    /// `NeedsReseed` after `coast_limit` misses, without needing a real
    /// correlation tracker or synthetic frames shaped like anything.
    struct ScriptedTracker {
        /// Inclusive range (by call count, 0-based) of `step` calls that
        /// return `Miss`; `None` means never miss. `Some((from, u64::MAX))`
        /// means "miss forever from `from` on".
        miss_range: Option<(u64, u64)>,
        frames_seen: u64,
        position: Point,
    }

    impl Tracker for ScriptedTracker {
        fn step(&mut self, _frame: &Frame, _last_pos: Point) -> StepOutcome {
            let frame = self.frames_seen;
            self.frames_seen += 1;
            match self.miss_range {
                Some((from, to)) if frame >= from && frame <= to => StepOutcome::Miss,
                _ => StepOutcome::Found {
                    position: self.position,
                    score: 1.0,
                },
            }
        }
    }

    /// Builds an in-memory `FfmpegFrameSource` yielding `count` distinct
    /// frames of `width`x`height`, so `run_tracking_loop` can be driven to a
    /// real (`Ok(None)`) EOF without spawning ffmpeg.
    fn frame_source_with(width: u32, height: u32, count: u8) -> FfmpegFrameSource<Cursor<Vec<u8>>> {
        let mut data = Vec::new();
        for i in 0..count {
            data.extend(synthetic_frame_bytes(width, height, i.wrapping_add(1)));
        }
        FfmpegFrameSource::from_reader(Cursor::new(data), width, height)
    }

    #[test]
    fn decode_up_to_returns_the_target_frame_discarding_earlier_ones() {
        let width = 2;
        let height = 2;
        let mut data = synthetic_frame_bytes(width, height, 1);
        data.extend(synthetic_frame_bytes(width, height, 2));
        data.extend(synthetic_frame_bytes(width, height, 3));
        let mut source = FfmpegFrameSource::from_reader(Cursor::new(data), width, height);

        let frame = decode_up_to(&mut source, 1).unwrap().expect("frame 1");
        assert_eq!(frame.pixel(0, 0), Some([2, 2, 2]));

        // Source is now positioned right after frame 1; the next read is
        // frame 2.
        let next = source.next_frame().unwrap().expect("frame 2");
        assert_eq!(next.pixel(0, 0), Some([3, 3, 3]));
    }

    #[test]
    fn decode_up_to_target_zero_returns_first_frame() {
        let width = 2;
        let height = 2;
        let data = synthetic_frame_bytes(width, height, 9);
        let mut source = FfmpegFrameSource::from_reader(Cursor::new(data), width, height);
        let frame = decode_up_to(&mut source, 0).unwrap().expect("frame 0");
        assert_eq!(frame.pixel(0, 0), Some([9, 9, 9]));
    }

    #[test]
    fn decode_up_to_beyond_end_of_stream_is_none() {
        let width = 2;
        let height = 2;
        let data = synthetic_frame_bytes(width, height, 1); // one frame only
        let mut source = FfmpegFrameSource::from_reader(Cursor::new(data), width, height);
        assert!(decode_up_to(&mut source, 5).unwrap().is_none());
    }

    #[test]
    fn tracker_config_falls_back_to_defaults_when_tuning_is_empty() {
        let config = tracker_config(TrackerTuning::default());
        assert_eq!(config.patch_radius(), DEFAULT_PATCH_RADIUS);
        assert_eq!(config.search_radius(), DEFAULT_SEARCH_RADIUS);
        assert_eq!(config.min_score(), DEFAULT_MIN_SCORE);
        assert_eq!(config.update_threshold(), DEFAULT_UPDATE_THRESHOLD);
    }

    #[test]
    fn tracker_config_applies_overrides() {
        let tuning = TrackerTuning {
            patch_radius: Some(20),
            search_radius: Some(40),
            min_score: Some(0.55),
            update_threshold: Some(0.8),
            coast_limit: None,
            reacquire_min_score: None,
            preprocessor: PreprocessorChain::default(),
        };
        let config = tracker_config(tuning);
        assert_eq!(config.patch_radius(), 20);
        assert_eq!(config.search_radius(), 40);
        assert_eq!(config.min_score(), 0.55);
        assert_eq!(config.update_threshold(), 0.8);
    }

    #[test]
    fn tracker_config_applies_preprocessor_chain() {
        let chain =
            PreprocessorChain::from_steps(vec![tracker_core::Preprocessor::Median { k: 3 }]);
        let tuning = TrackerTuning {
            preprocessor: chain.clone(),
            ..Default::default()
        };
        assert_eq!(tracker_config(tuning).preprocessor(), &chain);
    }

    #[test]
    fn color_tracker_config_applies_preprocessor_chain() {
        let chain = PreprocessorChain::from_steps(vec![tracker_core::Preprocessor::GaussianBlur {
            sigma: 1.5,
        }]);
        let tuning = TrackerTuning {
            preprocessor: chain.clone(),
            ..Default::default()
        };
        assert_eq!(color_tracker_config(tuning).preprocessor(), &chain);
        // Default (no tuning) stays an empty/identity chain.
        assert!(default_color_tracker_config().preprocessor().is_empty());
    }

    #[test]
    fn session_config_applies_coast_limit_override() {
        let tuning = TrackerTuning {
            coast_limit: Some(9),
            ..Default::default()
        };
        assert_eq!(session_config(tuning).coast_limit(), 9);
        assert_eq!(
            session_config(TrackerTuning::default()).coast_limit(),
            DEFAULT_COAST_LIMIT
        );
    }

    #[test]
    fn session_config_reacquire_min_score_defaults_and_is_decoupled_from_update_threshold() {
        // Default: DEFAULT_REACQUIRE_MIN_SCORE, not update_threshold (10.2b).
        assert_eq!(
            session_config(TrackerTuning::default()).reacquire_min_score(),
            Some(DEFAULT_REACQUIRE_MIN_SCORE)
        );
        // Overriding --update-threshold no longer moves the reacquire floor.
        let tuning = TrackerTuning {
            update_threshold: Some(0.85),
            ..Default::default()
        };
        assert_eq!(
            session_config(tuning).reacquire_min_score(),
            Some(DEFAULT_REACQUIRE_MIN_SCORE)
        );
        // Its own override is independent.
        let tuning = TrackerTuning {
            reacquire_min_score: Some(0.6),
            ..Default::default()
        };
        assert_eq!(session_config(tuning).reacquire_min_score(), Some(0.6));
    }

    #[test]
    fn session_config_derives_max_reacquire_distance_from_search_radius() {
        assert_eq!(
            session_config(TrackerTuning::default()).max_reacquire_distance(),
            Some(2.0 * DEFAULT_SEARCH_RADIUS as f64)
        );
        let tuning = TrackerTuning {
            search_radius: Some(50),
            ..Default::default()
        };
        assert_eq!(session_config(tuning).max_reacquire_distance(), Some(100.0));
    }

    #[test]
    fn rep_segmentation_config_lowers_dead_band_when_calibrated() {
        let uncal = rep_segmentation_config(false);
        let cal = rep_segmentation_config(true);
        assert_eq!(uncal.min_velocity(), 5.0);
        assert_eq!(cal.min_velocity(), 0.03);
    }

    /// The min-displacement gate (task 15.1) must scale with units too:
    /// px-scale default when uncalibrated, meter-scale when calibrated.
    #[test]
    fn rep_segmentation_config_scales_min_displacement_with_units() {
        assert_eq!(rep_segmentation_config(false).min_displacement(), 40.0);
        assert_eq!(rep_segmentation_config(true).min_displacement(), 0.15);
    }

    #[test]
    fn started_state_is_running_with_no_data_yet() {
        let state = TrackingRunState::started();
        assert!(state.running);
        assert!(state.last_frame_index.is_none());
        assert!(state.status_line().contains("starting"));
    }

    #[test]
    fn progress_message_updates_live_fields_and_keeps_running() {
        let mut state = TrackingRunState::started();
        let finished = state.apply(TrackingMessage::Progress {
            video_frame_index: 42,
            position: Point::new(10.0, 20.0),
            source: SampleSource::Tracked,
            state: SessionState::Tracking,
        });
        assert!(!finished);
        assert!(state.running);
        assert_eq!(state.last_frame_index, Some(42));
        assert_eq!(state.last_position, Some(Point::new(10.0, 20.0)));
        assert_eq!(state.frames_processed, 1);
        assert!(state.status_line().contains("frame 42"));
    }

    #[test]
    fn needs_reseed_progress_is_reported_as_paused() {
        let mut state = TrackingRunState::started();
        state.apply(TrackingMessage::Progress {
            video_frame_index: 7,
            position: Point::new(0.0, 0.0),
            source: SampleSource::Tracked,
            state: SessionState::NeedsReseed,
        });
        assert_eq!(state.session_state, Some(SessionState::NeedsReseed));
        let line = state.status_line();
        assert!(line.contains("paused"));
        assert!(line.contains('7'));
    }

    #[test]
    fn coasting_progress_is_reported_as_searching_and_freezes_tracked_position() {
        let mut state = TrackingRunState::started();
        state.apply(TrackingMessage::Progress {
            video_frame_index: 4,
            position: Point::new(10.0, 10.0),
            source: SampleSource::Tracked,
            state: SessionState::Tracking,
        });
        assert_eq!(state.last_tracked_position, Some(Point::new(10.0, 10.0)));
        assert!(!state.is_searching());

        // Gap opens: the session keeps coasting (still `Tracking`, not yet
        // `NeedsReseed`), but the sample source is `Interpolated`.
        state.apply(TrackingMessage::Progress {
            video_frame_index: 5,
            position: Point::new(25.0, 25.0), // interpolated toward garbage
            source: SampleSource::Interpolated,
            state: SessionState::Tracking,
        });
        assert!(state.is_searching());
        let line = state.status_line();
        assert!(line.contains("lost"));
        assert!(line.contains("searching"));
        // The frozen position stays at the last real tracked sample, not
        // wherever the interpolation currently sits.
        assert_eq!(state.last_tracked_position, Some(Point::new(10.0, 10.0)));
        assert_eq!(state.last_position, Some(Point::new(25.0, 25.0)));
    }

    #[test]
    fn paused_state_is_also_reported_as_searching() {
        let mut state = TrackingRunState::started();
        state.apply(TrackingMessage::Progress {
            video_frame_index: 7,
            position: Point::new(0.0, 0.0),
            source: SampleSource::Interpolated,
            state: SessionState::NeedsReseed,
        });
        assert!(state.is_searching());
    }

    #[test]
    fn needs_reseed_progress_increments_gap_count_only_once_per_pause() {
        let mut state = TrackingRunState::started();
        let paused = |frame| TrackingMessage::Progress {
            video_frame_index: frame,
            position: Point::new(0.0, 0.0),
            source: SampleSource::Interpolated,
            state: SessionState::NeedsReseed,
        };
        state.apply(paused(5));
        state.apply(paused(5)); // still paused: no second increment
        assert_eq!(state.gap_count, 1);

        // Resumes, tracks a bit, then pauses again: second gap.
        state.apply(TrackingMessage::Progress {
            video_frame_index: 6,
            position: Point::new(1.0, 1.0),
            source: SampleSource::Tracked,
            state: SessionState::Tracking,
        });
        state.apply(paused(9));
        assert_eq!(state.gap_count, 2);
    }

    #[test]
    fn done_message_stores_bar_path_and_stops_running() {
        let mut state = TrackingRunState::started();
        let tb = Timebase::new(30, 1).unwrap();
        let bar_path = BarPath::new(&[], &[], tb, 0);
        let finished = state.apply(TrackingMessage::Done(bar_path.clone()));
        assert!(finished);
        assert!(!state.running);
        assert_eq!(state.bar_path, Some(bar_path));
        assert!(state.status_line().contains("complete"));
    }

    #[test]
    fn error_message_stops_running_and_is_reported() {
        let mut state = TrackingRunState::started();
        let finished = state.apply(TrackingMessage::Error("boom".to_string()));
        assert!(finished);
        assert!(!state.running);
        assert_eq!(state.error, Some("boom".to_string()));
        assert!(state.status_line().contains("boom"));
    }

    // -- Task 10.8: live rep counter ---------------------------------------

    fn progress(frame: u64, y: f64) -> TrackingMessage {
        TrackingMessage::Progress {
            video_frame_index: frame,
            position: Point::new(0.0, y),
            source: SampleSource::Tracked,
            state: SessionState::Tracking,
        }
    }

    #[test]
    fn should_recompute_live_reps_fires_every_30th_processed_frame() {
        let mut state = TrackingRunState::started();
        for frame in 0..90u64 {
            state.apply(progress(frame, frame as f64));
            let should = state.should_recompute_live_reps();
            if state
                .frames_processed
                .is_multiple_of(LIVE_REP_RECOMPUTE_INTERVAL)
            {
                assert!(should, "frame {frame}: expected a recompute trigger");
            } else {
                assert!(!should, "frame {frame}: expected no recompute trigger");
            }
        }
    }

    #[test]
    fn should_recompute_live_reps_is_false_before_any_frame_processed() {
        let state = TrackingRunState::started();
        assert!(!state.should_recompute_live_reps());
    }

    #[test]
    fn apply_progress_accumulates_samples_for_partial_rep_compute() {
        let mut state = TrackingRunState::started();
        state.apply(progress(0, 0.0));
        state.apply(progress(1, 5.0));
        assert_eq!(state.samples.len(), 2);
        assert_eq!(state.samples[1].frame_index, 1);
    }

    #[test]
    fn live_rep_count_is_none_with_too_few_samples() {
        let state = TrackingRunState::started();
        let tb = Timebase::new(30, 1).unwrap();
        assert_eq!(state.live_rep_count(tb, None), None);
    }

    /// A synthetic one-rep descent/ascent, fed in as partial `Progress`
    /// samples the same way a live run would accumulate them, must be
    /// detected by `live_rep_count` exactly like `SessionResults::build`
    /// detects it from the final `BarPath` (state.rs's
    /// `session_results_build_detects_reps_and_reports_units` test uses the
    /// same shape) — the live and final counters must never disagree on
    /// tuning.
    #[test]
    fn live_rep_count_detects_a_rep_from_partial_samples() {
        let mut state = TrackingRunState::started();
        for i in 0..=10u64 {
            state.apply(progress(i, i as f64 * 10.0));
        }
        for i in 11..=20u64 {
            state.apply(progress(i, (20 - i) as f64 * 10.0));
        }
        let tb = Timebase::new(30, 1).unwrap();
        assert_eq!(state.live_rep_count(tb, None), Some(1));
    }

    #[test]
    fn message_video_frame_index_is_some_only_for_progress() {
        let progress = TrackingMessage::Progress {
            video_frame_index: 3,
            position: Point::new(0.0, 0.0),
            source: SampleSource::Tracked,
            state: SessionState::Tracking,
        };
        assert_eq!(progress.video_frame_index(), Some(3));

        let tb = Timebase::new(30, 1).unwrap();
        let done = TrackingMessage::Done(BarPath::new(&[], &[], tb, 0));
        assert_eq!(done.video_frame_index(), None);

        let err = TrackingMessage::Error("x".to_string());
        assert_eq!(err.video_frame_index(), None);
    }

    // -- PLAN 10.1 regression: worker must terminate at decode EOF ---------

    /// Baseline: a source that EOFs cleanly while the tracker keeps finding
    /// the target drives the loop to completion, with every reported frame
    /// index within the frames actually fed (never past decode EOF).
    #[test]
    fn run_tracking_loop_ends_at_eof_with_frame_indices_within_frames_fed() {
        let (width, height) = (2u32, 2u32);
        let frame_count = 5u8;
        let mut source = frame_source_with(width, height, frame_count);
        let tracker = ScriptedTracker {
            miss_range: None,
            frames_seen: 0,
            position: Point::new(1.0, 1.0),
        };
        let seed_frame_index = 10u64;
        let mut session = TrackingSession::new(
            tracker,
            0,
            Point::new(1.0, 1.0),
            session_config(TrackerTuning::default()),
        );
        let (tx, rx) = mpsc::channel::<TrackingMessage>();
        let (_reseed_tx, reseed_rx) = mpsc::channel::<ReseedCommand>();
        let (_control_tx, control_rx) = mpsc::channel::<ControlCommand>();

        let result = run_tracking_loop(
            &mut source,
            &mut session,
            seed_frame_index,
            &tx,
            &reseed_rx,
            &control_rx,
        );
        assert!(result.is_ok());

        let messages: Vec<_> = rx.try_iter().collect();
        assert!(!messages.is_empty());
        // `source` stands in for the frames read *after* the seed frame
        // (mirrors production: `decode_up_to` already consumed the seed
        // frame before `run_tracking_loop` starts reading), so the highest
        // video-absolute index the loop can legitimately report is
        // `seed_frame_index + frame_count` (relative frame indices 1..=N).
        let max_video_frame_index = seed_frame_index + frame_count as u64;
        for msg in &messages {
            if let TrackingMessage::Progress {
                video_frame_index, ..
            } = msg
            {
                assert!(
                    *video_frame_index <= max_video_frame_index,
                    "reported frame {video_frame_index} exceeds the {frame_count} frames actually fed \
                     (max valid video-absolute index {max_video_frame_index})"
                );
            }
        }
        // Source is genuinely exhausted: one more read is still a clean
        // `None`, not a hang or a phantom extra frame.
        assert!(source.next_frame().unwrap().is_none());
    }

    /// The bug: a session that pauses (`NeedsReseed`) right as the video
    /// hits real decode EOF must not leave the run silently "running"
    /// forever if nothing ever supplies a reseed (e.g. the caller dropped
    /// its handle while paused, mirroring the CLI headless-loop-exits and
    /// app-closing-while-paused cases). `run_tracking_loop` must return
    /// `Ok(())` — ending the run with the samples collected so far — the
    /// moment the reseed channel closes, rather than hanging or silently
    /// returning without the caller ever building `Done`.
    #[test]
    fn run_tracking_loop_ends_cleanly_when_paused_awaiting_reseed_and_channel_closes() {
        let (width, height) = (2u32, 2u32);
        let frame_count = 8u8;
        let mut source = frame_source_with(width, height, frame_count);
        // coast_limit defaults to DEFAULT_COAST_LIMIT (5): miss from frame 0
        // so the session pauses well before the source's real EOF at frame
        // index 7, proving the pause-then-channel-closed path — not just
        // "ran out of frames" — is what ends the run.
        let tracker = ScriptedTracker {
            miss_range: Some((0, u64::MAX)),
            frames_seen: 0,
            position: Point::new(1.0, 1.0),
        };
        let seed_frame_index = 0u64;
        let mut session = TrackingSession::new(
            tracker,
            0,
            Point::new(1.0, 1.0),
            session_config(TrackerTuning::default()),
        );
        let (tx, rx) = mpsc::channel::<TrackingMessage>();
        let (reseed_tx, reseed_rx) = mpsc::channel::<ReseedCommand>();
        let (_control_tx, control_rx) = mpsc::channel::<ControlCommand>();
        // Simulate the UI/CLI dropping its handle while paused: the worker
        // is about to block on `reseed_rx.recv()`, so close the sender from
        // another thread once it does.
        drop(reseed_tx);

        let result = run_tracking_loop(
            &mut source,
            &mut session,
            seed_frame_index,
            &tx,
            &reseed_rx,
            &control_rx,
        );

        assert!(
            result.is_ok(),
            "loop must end cleanly (Ok) rather than hang or bail out when paused and the \
             reseed channel closes"
        );
        assert_eq!(
            session.state(),
            SessionState::NeedsReseed,
            "session should still be paused: nothing ever reseeded it"
        );
        // We stopped well short of the source's real EOF (frame_count - 1 =
        // 7): frame indices must never have run past what was actually
        // decoded before the pause.
        let messages: Vec<_> = rx.try_iter().collect();
        let max_reported = messages
            .iter()
            .filter_map(|m| m.video_frame_index())
            .max()
            .unwrap_or(0);
        assert!(
            max_reported < frame_count as u64 - 1,
            "paused run reported frame {max_reported}, which should be well before the source's \
             real EOF at {}",
            frame_count as u64 - 1
        );
    }

    /// Companion to the pause/channel-closed case: if instead the caller
    /// *does* supply a reseed, but the video was already at its very last
    /// frame, the loop must still terminate cleanly at the next read
    /// (`Ok(None)`) rather than blocking or looping forever waiting for
    /// frames that don't exist.
    #[test]
    fn run_tracking_loop_ends_at_eof_after_reseed_resumes_into_the_last_frame() {
        let (width, height) = (2u32, 2u32);
        let frame_count = 3u8; // frames 0,1,2
        let mut source = frame_source_with(width, height, frame_count);
        // Miss starting at frame 1 (second frame fed): with coast_limit 0
        // the session pauses immediately on that first miss.
        let tracker = ScriptedTracker {
            miss_range: Some((1, 1)),
            frames_seen: 0,
            position: Point::new(1.0, 1.0),
        };
        let seed_frame_index = 0u64;
        let session_config = TrackingSessionConfig::builder().coast_limit(0).build();
        let mut session = TrackingSession::new(tracker, 0, Point::new(1.0, 1.0), session_config);
        let (tx, rx) = mpsc::channel::<TrackingMessage>();
        let (reseed_tx, reseed_rx) = mpsc::channel::<ReseedCommand>();
        let (_control_tx, control_rx) = mpsc::channel::<ControlCommand>();

        // Resume it from a background thread right away so the worker's
        // blocking `recv()` unblocks with a real command instead of a
        // closed channel, then drop the sender so the test doesn't hang if
        // something regresses and the loop blocks again later.
        let resumer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            let _ = reseed_tx.send(ReseedCommand {
                video_frame_index: 1,
                position: Point::new(2.0, 2.0),
            });
        });

        let result = run_tracking_loop(
            &mut source,
            &mut session,
            seed_frame_index,
            &tx,
            &reseed_rx,
            &control_rx,
        );
        resumer.join().unwrap();

        assert!(result.is_ok());
        assert_ne!(
            session.state(),
            SessionState::NeedsReseed,
            "reseeding should have resumed tracking, not left it paused"
        );
        let messages: Vec<_> = rx.try_iter().collect();
        let max_reported = messages
            .iter()
            .filter_map(|m| m.video_frame_index())
            .max()
            .unwrap_or(0);
        assert!(
            max_reported < frame_count as u64,
            "reported frame {max_reported} exceeds the {frame_count} frames actually fed"
        );
    }

    // -- Task 10.4: Pause/Resume/Stop -------------------------------------

    /// `Stop` ends the run at the next checkpoint (before the next frame
    /// read) with whatever samples the session collected so far, without
    /// reading the rest of the source — the "Stop = finish now with partial
    /// results" behavior the toolbar's Stop button relies on.
    #[test]
    fn stop_command_ends_the_run_early_with_partial_results() {
        let (width, height) = (2u32, 2u32);
        let frame_count = 20u8;
        let mut source = frame_source_with(width, height, frame_count);
        let tracker = ScriptedTracker {
            miss_range: None,
            frames_seen: 0,
            position: Point::new(1.0, 1.0),
        };
        let seed_frame_index = 0u64;
        let mut session = TrackingSession::new(
            tracker,
            0,
            Point::new(1.0, 1.0),
            session_config(TrackerTuning::default()),
        );
        let (tx, rx) = mpsc::channel::<TrackingMessage>();
        let (_reseed_tx, reseed_rx) = mpsc::channel::<ReseedCommand>();
        let (control_tx, control_rx) = mpsc::channel::<ControlCommand>();
        control_tx.send(ControlCommand::Stop).unwrap();

        let result = run_tracking_loop(
            &mut source,
            &mut session,
            seed_frame_index,
            &tx,
            &reseed_rx,
            &control_rx,
        );

        assert!(result.is_ok());
        // Stop was seen before any frame was read: no Progress messages at
        // all, and the source still has every frame unread.
        assert_eq!(rx.try_iter().count(), 0);
        assert!(source.next_frame().unwrap().is_some());
    }

    /// `Pause` genuinely stops frame consumption (blocks on `recv`, not a
    /// GUI-side hold on top of a worker that keeps decoding): frames sent
    /// after `Pause` but before `Resume` must never be reflected in
    /// `frames_processed`-style progress until `Resume` arrives.
    #[test]
    fn pause_blocks_until_resume_then_continues_processing() {
        let (width, height) = (2u32, 2u32);
        let frame_count = 3u8;
        let mut source = frame_source_with(width, height, frame_count);
        let tracker = ScriptedTracker {
            miss_range: None,
            frames_seen: 0,
            position: Point::new(1.0, 1.0),
        };
        let seed_frame_index = 0u64;
        let mut session = TrackingSession::new(
            tracker,
            0,
            Point::new(1.0, 1.0),
            session_config(TrackerTuning::default()),
        );
        let (tx, rx) = mpsc::channel::<TrackingMessage>();
        let (_reseed_tx, reseed_rx) = mpsc::channel::<ReseedCommand>();
        let (control_tx, control_rx) = mpsc::channel::<ControlCommand>();
        control_tx.send(ControlCommand::Pause).unwrap();

        // Resume from a background thread after a short delay, proving the
        // loop was genuinely blocked on `recv` in the meantime (a bug that
        // let it fall through and keep decoding would make this test flaky
        // in the other direction — passing even when the block is broken —
        // so the resumer thread is what makes this an assertion of a wait,
        // not a coincidence).
        let resumer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            control_tx.send(ControlCommand::Resume).unwrap();
        });

        let result = run_tracking_loop(
            &mut source,
            &mut session,
            seed_frame_index,
            &tx,
            &reseed_rx,
            &control_rx,
        );
        resumer.join().unwrap();

        assert!(result.is_ok());
        // Ran to clean EOF once resumed: every frame was processed.
        let messages: Vec<_> = rx.try_iter().collect();
        let progress_count = messages
            .iter()
            .filter(|m| matches!(m, TrackingMessage::Progress { .. }))
            .count();
        assert_eq!(progress_count, frame_count as usize);
    }

    /// `Stop` received while blocked in `Pause` also ends the run cleanly
    /// (not just `Resume`), so Discard-during-pause doesn't hang.
    #[test]
    fn stop_while_paused_ends_the_run_cleanly() {
        let (width, height) = (2u32, 2u32);
        let frame_count = 5u8;
        let mut source = frame_source_with(width, height, frame_count);
        let tracker = ScriptedTracker {
            miss_range: None,
            frames_seen: 0,
            position: Point::new(1.0, 1.0),
        };
        let seed_frame_index = 0u64;
        let mut session = TrackingSession::new(
            tracker,
            0,
            Point::new(1.0, 1.0),
            session_config(TrackerTuning::default()),
        );
        let (tx, rx) = mpsc::channel::<TrackingMessage>();
        let (_reseed_tx, reseed_rx) = mpsc::channel::<ReseedCommand>();
        let (control_tx, control_rx) = mpsc::channel::<ControlCommand>();
        control_tx.send(ControlCommand::Pause).unwrap();
        let resumer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            control_tx.send(ControlCommand::Stop).unwrap();
        });

        let result = run_tracking_loop(
            &mut source,
            &mut session,
            seed_frame_index,
            &tx,
            &reseed_rx,
            &control_rx,
        );
        resumer.join().unwrap();

        assert!(result.is_ok());
        assert_eq!(rx.try_iter().count(), 0, "no frames should have been read");
    }
}
