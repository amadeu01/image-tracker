//! Pure(ish) app state (task 2.3, split out in 7.2): current frame index,
//! mode, Seed/Calibration, tracking run reducer, and the guide/status/events
//! data the side panel (7.2) renders. No egui `Context` dependency, so all of
//! this is unit-testable directly.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Instant;

use crate::export_job::{self, ExportHandle, ExportMessage};
use crate::ffprobe::VideoMetadata;
use crate::frame_cache::clamp_frame_index;
use crate::tracking::{self, TrackingHandle, TrackingRunState};

/// What clicking on the frame view currently does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    /// Just look around: scrub the slider, no click handling yet.
    ViewOnly,
    /// Clicking the frame places the Seed (task 2.4).
    PlacingSeed,
    /// Clicking the frame places calibration points (task 2.5). Holds the
    /// first click (if any) and the known real-world length in meters used
    /// to derive px/m once both points are placed.
    Calibrating {
        first_point: Option<tracker_core::Point>,
        known_length_meters: f64,
    },
}

/// Default known length for the calibration reference object: a standard
/// 450mm bumper plate diameter.
pub const DEFAULT_CALIBRATION_LENGTH_METERS: f64 = 0.450;

/// Which of the design's two side-panel layouts the toolbar's Live/Results
/// pill (task 13.1) currently selects. Distinct from `Mode` (which governs
/// frame click-handling); this only decides which section of the side
/// panel is emphasized. Task 13.1 is shell-only — it wires the toggle and
/// this field, but doesn't build the dedicated Live panel described in the
/// design notes (task 13.6); `Live` reuses whatever partial live UI already
/// exists (the live rep counter in the Status section) rather than a new
/// dedicated layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayMode {
    Live,
    Results,
}

/// A user-placed Seed: image-pixel position plus the frame it was placed on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seed {
    pub position: tracker_core::Point,
    pub frame_index: u64,
}

/// Severity of an [`AppEvent`], used by the side panel to color the events
/// list (errors stand out).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventLevel {
    Info,
    Warn,
    Error,
}

/// One entry in the in-memory event ring buffer the side panel's "Events"
/// section shows (task 7.2) — an on-screen mirror of the breadcrumbs already
/// sent to `tracing`, so the user doesn't need to open the log file to see
/// what just happened.
#[derive(Debug, Clone)]
pub struct AppEvent {
    pub level: EventLevel,
    pub message: String,
    /// Seconds since this `AppState` was created. There's no wall-clock
    /// dependency in this crate (no `chrono`/`time`), and elapsed-since-start
    /// is exactly as useful for "what just happened" debugging.
    pub elapsed_secs: f64,
}

/// How many `AppEvent`s the ring buffer keeps; older ones are dropped.
const MAX_EVENTS: usize = 8;

/// The workflow step the side panel's guide should highlight as current,
/// derived purely from `AppState` (task 7.2). Steps 1 (scrub to the bar) and
/// 2 (place seed) share a single derived value since there's no scrub signal
/// to distinguish them — the guide lists both, but only step 2 is ever
/// "current"; step 1 is implicitly satisfied once the user reaches step 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowStep {
    PlaceSeed = 2,
    Calibrate = 3,
    Track = 4,
    Review = 5,
}

impl WorkflowStep {
    pub fn ordinal(self) -> u8 {
        self as u8
    }
}

/// The workflow's live phase (task 10.8), a finer-grained sibling of
/// `WorkflowStep`: `WorkflowStep` says which guide step is current,
/// `Phase` additionally carries the *progress within* the Track step (frame
/// N/M) and distinguishes "still walking frames" from "run finished, now
/// deriving velocity/reps/metrics from the path" — the two are visually
/// identical in `WorkflowStep::Track` but the status bar/banner (10.7)
/// should say something different for each.
///
/// `ComputingMetrics` is honest-but-brief by construction: `poll_tracking`
/// builds `SessionResults` synchronously, in the same call that stores
/// `bar_path`, so there's never actually an egui frame rendered with
/// `bar_path.is_some() && results.is_none()` — `phase()` still derives
/// `ComputingMetrics` from exactly that condition (rather than skipping it)
/// so the concept is correct and future-proof if that computation ever
/// becomes async/backgrounded, even though today a caller will never
/// observe it mid-render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// No run has started (or the session was reset/discarded).
    Idle,
    /// A run is actively walking frames. `total` is the best-known frame
    /// count (see `poll_tracking`'s note on `ffprobe` underestimating it);
    /// `0` if unknown.
    TrackingPath { frame: u64, total: u64 },
    /// The run reached `Done`/`Error` and `SessionResults` is being
    /// derived from the finished `BarPath` (velocity → reps → metrics).
    ComputingMetrics,
    /// Results are ready; the Review step's Results section is showing.
    Review,
}

/// User-editable tracking configuration (task 11.3): tracker kind, filter
/// chain, and the advanced `TrackerTuning` knobs, gathered on `AppState` so
/// the side panel's "Tracking settings" section can edit them before Track
/// and, in Review, before Re-track. Read fresh by `start_tracking` on every
/// run — there is no separate "apply" step, so changing a value between
/// runs simply takes effect the next time Track/Re-track is clicked; while
/// a run is active the panel renders these fields read-only (gated in
/// `side_panel.rs` on `state.tracking.is_none()`), so "locked while running"
/// is a rendering concern, not a state one.
///
/// Filter chain order is fixed for v1 (documented here and in the panel):
/// Gaussian is always applied before Median when both are enabled — no
/// reordering UI yet (PLAN 11.3 allows this: "order fixed
/// gaussian-then-median is fine for v1").
#[derive(Debug, Clone, PartialEq)]
pub struct TrackingSettings {
    pub tracker_selection: tracking::TrackerSelection,
    pub gaussian_enabled: bool,
    pub gaussian_sigma: f64,
    pub median_enabled: bool,
    /// Neighborhood size for the median filter: 3 or 5 (side panel offers a
    /// combo box over just these two, per PLAN 11.3).
    pub median_k: u32,
    pub patch_radius: u32,
    pub search_radius: u32,
    pub min_score: f64,
    pub update_threshold: f64,
    pub coast_limit: u32,
    pub reacquire_min_score: f64,
    /// Stop-set velocity-loss threshold (%, task 13.5): the Results
    /// header's "Stop set recommended" banner fires once any rep's loss
    /// (vs rep 1, `tracker_core::velocity_loss_percent`) reaches this
    /// value. Range 5-40 (side panel slider/`DragValue`), default 20 per
    /// the design spec. Persisted across restarts via
    /// `theme::load_stop_threshold`/`save_stop_threshold`, same as the
    /// theme override — it's a user preference, not run output.
    pub stop_threshold_pct: f64,
}

/// Default stop-set velocity-loss threshold (%, task 13.5's design spec).
pub const DEFAULT_STOP_THRESHOLD_PCT: f64 = 20.0;

impl Default for TrackingSettings {
    fn default() -> Self {
        Self {
            tracker_selection: tracking::TrackerSelection::Auto,
            gaussian_enabled: false,
            gaussian_sigma: 1.5,
            median_enabled: false,
            median_k: 3,
            patch_radius: tracking::DEFAULT_PATCH_RADIUS,
            search_radius: tracking::DEFAULT_SEARCH_RADIUS,
            min_score: tracking::DEFAULT_MIN_SCORE,
            update_threshold: tracking::DEFAULT_UPDATE_THRESHOLD,
            coast_limit: tracking::DEFAULT_COAST_LIMIT,
            reacquire_min_score: tracking::DEFAULT_REACQUIRE_MIN_SCORE,
            stop_threshold_pct: DEFAULT_STOP_THRESHOLD_PCT,
        }
    }
}

impl TrackingSettings {
    /// Builds the `PreprocessorChain` these settings describe, in the fixed
    /// gaussian-then-median order (see the struct doc comment).
    pub fn preprocessor_chain(&self) -> tracker_core::PreprocessorChain {
        let mut steps = Vec::new();
        if self.gaussian_enabled {
            steps.push(tracker_core::Preprocessor::GaussianBlur {
                sigma: self.gaussian_sigma,
            });
        }
        if self.median_enabled {
            steps.push(tracker_core::Preprocessor::Median { k: self.median_k });
        }
        tracker_core::PreprocessorChain::from_steps(steps)
    }

    /// Maps these settings onto a `tracking::TrackerTuning`, the shape
    /// `tracking::tracker_config`/`session_config`/`color_tracker_config`
    /// consume — the settings->`TrackingJob` mapping task 11.3 asks for.
    pub fn tuning(&self) -> tracking::TrackerTuning {
        tracking::TrackerTuning {
            patch_radius: Some(self.patch_radius),
            search_radius: Some(self.search_radius),
            min_score: Some(self.min_score),
            update_threshold: Some(self.update_threshold),
            coast_limit: Some(self.coast_limit),
            reacquire_min_score: Some(self.reacquire_min_score),
            preprocessor: self.preprocessor_chain(),
        }
    }

    /// Short human-readable summary of the resolved strategy, used for the
    /// "tracking started" event (task 11.3), e.g. `"template, gaussian σ1.5
    /// + median 3"` or `"auto"` when no filters are enabled.
    pub fn describe(&self) -> String {
        let kind = match self.tracker_selection {
            tracking::TrackerSelection::Auto => "auto",
            tracking::TrackerSelection::Template => "template",
            tracking::TrackerSelection::Color => "color",
        };
        let mut parts = Vec::new();
        if self.gaussian_enabled {
            parts.push(format!("gaussian σ{:.1}", self.gaussian_sigma));
        }
        if self.median_enabled {
            parts.push(format!("median {}", self.median_k));
        }
        if parts.is_empty() {
            kind.to_string()
        } else {
            format!("{kind}, {}", parts.join(" + "))
        }
    }
}

/// Gap/interpolation/reseed summary shown in the Results section's quality
/// line (10.3). `gap_count` and `reseed_count` are currently the same
/// number — every gap this run hit paused for a reseed (`TrackingRunState`
/// has no concept of a gap that self-heals without one) — but they're kept
/// as separate named fields since that's not a guarantee of the type, just
/// of the current session/reseed wiring.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ResultsQuality {
    pub gap_count: u64,
    pub reseed_count: u64,
    pub interpolated_points: usize,
    pub total_points: usize,
}

impl ResultsQuality {
    /// Percentage of the path's points that were interpolated (coasted
    /// over a gap) rather than directly tracked. `0.0` for an empty path.
    pub fn interpolated_percent(&self) -> f64 {
        if self.total_points == 0 {
            0.0
        } else {
            self.interpolated_points as f64 / self.total_points as f64 * 100.0
        }
    }
}

/// Everything the Review step's Results section (10.3) shows, derived once
/// from a completed run's `BarPath` and the calibration in effect when it
/// finished. Pure struct — no egui dependency — so its construction is
/// unit-testable directly (see `tests` below).
///
/// `velocity` is a `Result` rather than an already-unwrapped `Vec` on
/// purpose (10.9's GUI seam, noted in PLAN.md): a `VelocityError` (e.g. too
/// few points, non-monotonic timestamps) must be surfaced to the user —
/// here, as a Results-section message and a `Warn` event — not silently
/// swallowed into an empty reps/metrics list the way the CLI's original
/// `.ok()` mistake did before 10.9 fixed it there.
#[derive(Debug, Clone)]
pub struct SessionResults {
    pub bar_path: tracker_core::BarPath,
    pub velocity: Result<Vec<tracker_core::VelocitySample>, tracker_core::VelocityError>,
    pub reps: Vec<tracker_core::Rep>,
    pub metrics: Vec<tracker_core::RepMetrics>,
    pub unit: Option<tracker_core::VelocityUnit>,
    pub quality: ResultsQuality,
    /// Velocity loss (%) of each rep vs rep 1's mean concentric velocity
    /// (task 13.5, `tracker_core::velocity_loss_percent`), parallel to
    /// `metrics`/`reps` — index `i` here is rep `i`'s loss. Rep 1 (index 0)
    /// is always `None` ("—" in the table/chart per the design). The
    /// 13.3/13.4 rep table and velocity chart consume this directly rather
    /// than recomputing it, so both agree on the same numbers.
    pub loss_percent: Vec<Option<f64>>,
}

impl SessionResults {
    /// Builds results from a finished run's `bar_path`, the calibration (if
    /// any) in effect, and how many gaps the run needed reseeding through
    /// (`TrackingRunState::gap_count`). Smoothing window (5) and the
    /// calibrated/uncalibrated rep dead-band match the CLI's `run_track`
    /// (`cli.rs`) exactly (`tracking::rep_segmentation_config`), so GUI and
    /// CLI runs of the same video/seed never disagree on rep count.
    pub fn build(
        bar_path: tracker_core::BarPath,
        calibration: Option<tracker_core::Calibration>,
        gap_count: u64,
    ) -> Self {
        let velocity = tracker_core::velocity_series(bar_path.points(), 5, calibration.as_ref());
        let reps = match &velocity {
            Ok(v) => tracker_core::segment_reps(
                v,
                tracking::rep_segmentation_config(calibration.is_some()),
            ),
            Err(_) => Vec::new(),
        };
        let metrics = match &velocity {
            Ok(v) => {
                tracker_core::all_rep_metrics(&reps, v, bar_path.points(), calibration.as_ref())
            }
            Err(_) => Vec::new(),
        };
        let loss_percent = tracker_core::velocity_loss_percent(&metrics);
        let unit = metrics.first().map(|m| m.unit).or_else(|| {
            velocity
                .as_ref()
                .ok()
                .and_then(|v| v.first())
                .map(|s| s.unit)
        });
        let total_points = bar_path.points().len();
        let interpolated_points = bar_path
            .points()
            .iter()
            .filter(|p| p.source == tracker_core::Source::Interpolated)
            .count();
        let quality = ResultsQuality {
            gap_count,
            reseed_count: gap_count,
            interpolated_points,
            total_points,
        };
        Self {
            bar_path,
            velocity,
            reps,
            metrics,
            unit,
            quality,
            loss_percent,
        }
    }

    /// Wall-clock duration (seconds) of the set (the "SET TIME" headline
    /// card, task 13.5): `None` when there are no reps yet.
    pub fn set_duration_seconds(&self) -> Option<f64> {
        tracker_core::set_duration_seconds(&self.metrics)
    }

    /// The first rep whose loss crossed `threshold_pct`, if any (the "stop
    /// set recommended" banner's trigger, task 13.5).
    pub fn stop_set_evaluation(&self, threshold_pct: f64) -> Option<tracker_core::StopSet> {
        tracker_core::stop_set_evaluation(&self.loss_percent, threshold_pct)
    }
}

/// UI/session state, independent of egui so the index-clamping logic can be
/// unit-tested without a `Context`.
pub struct AppState {
    pub video_path: PathBuf,
    pub metadata: VideoMetadata,
    pub mode: Mode,
    pub current_frame: u64,
    /// The Seed, once placed (task 2.4). `None` until the user clicks in
    /// `Mode::PlacingSeed`.
    pub seed: Option<Seed>,
    /// The resolved Calibration (task 2.5), once both points have been
    /// clicked and the segment accepted. `None` until then; overwritten each
    /// time a new pair is completed.
    pub calibration: Option<tracker_core::Calibration>,
    /// The two points of the most recently completed calibration pair
    /// (success or failure), so the UI can draw a line between them even
    /// after the pair has reset for a potential third click.
    pub last_calibration_segment: Option<(tracker_core::Point, tracker_core::Point)>,
    /// Bottom status bar text; errors surface here rather than panicking
    /// (project rule — see PLAN.md 2.6).
    pub status: String,
    /// The active/paused tracking worker's channel handle, once "Track" has
    /// been clicked (task 2.6). `None` before a run starts and again once
    /// it finishes/errors.
    pub tracking: Option<TrackingHandle>,
    /// Pure reducer over that worker's progress messages; drives the live
    /// crosshair and status bar while `tracking` is active, and still holds
    /// the last-known state (including any error) after it finishes.
    pub tracking_run: TrackingRunState,
    /// Whether the user has paused the active run (task 10.4) — distinct
    /// from `tracking_run.session_state == NeedsReseed`, which is the
    /// tracker itself pausing because it lost the object. `false` outside
    /// an active run and reset whenever a run starts/finishes/is
    /// discarded.
    pub paused: bool,
    /// The completed `BarPath`, once a tracking run reaches clean
    /// end-of-video. Consumed by milestone 3 (overlay render / export).
    pub bar_path: Option<tracker_core::BarPath>,
    /// Velocity/reps/metrics derived from `bar_path` once a run reaches
    /// `Done` (task 10.3) — `None` until then, and again on a fresh
    /// "New session" reset. The Review step's Results section is built
    /// from this, not from re-deriving anything from `bar_path` itself.
    pub results: Option<SessionResults>,
    /// The background auto-export job's channel handle, once `results` has
    /// been computed (task 10.3). `None` before a run finishes and again
    /// once every export message has been drained.
    pub export: Option<ExportHandle>,
    /// The tracker `suggest_tracker` recommends for the current Seed (task
    /// 4.3), computed as soon as the Seed is placed so the status bar can
    /// tell the user which tracker Track will use before they click it.
    /// `None` until a Seed has been placed and a frame is available to
    /// evaluate (`TrackerApp::ensure_texture`/click handler sets this via
    /// `note_seed_suggestion`, since `AppState` alone has no frame access).
    pub suggested_tracker: Option<tracker_core::TrackerKind>,
    /// Live rep count (task 10.8), recomputed from the partial path every
    /// `LIVE_REP_RECOMPUTE_INTERVAL` processed frames while a run is active
    /// (`poll_tracking`, via `TrackingRunState::live_rep_count`). `None`
    /// before the first successful recompute (too few samples yet) or
    /// outside an active run; on a *failed* recompute (e.g. a transient
    /// `VelocityError`), the previous value is left in place rather than
    /// cleared — see `live_rep_count`'s doc comment for why.
    pub live_reps: Option<usize>,
    /// Recent app events (task 7.2), newest last, capped at `MAX_EVENTS`.
    /// Fed from the same call sites that already emit `tracing` breadcrumbs,
    /// so the side panel gives on-screen visibility into the same history
    /// the log file has.
    pub events: VecDeque<AppEvent>,
    /// When this `AppState` was created; used to timestamp `events`.
    start_time: Instant,
    /// User-editable tracker kind/filter chain/tuning (task 11.3), read by
    /// `start_tracking` on every run. Persists across "New session"/results
    /// resets (it's a user preference, not run output) so re-tracking or
    /// starting a fresh session on the same video keeps whatever the user
    /// last configured.
    pub settings: TrackingSettings,
    /// The active background strategy-benchmark worker (task 11.4, "Test
    /// strategies" button), once started. `None` before it's clicked and
    /// again once the run finishes/errors.
    pub benchmark: Option<crate::compare::BenchmarkHandle>,
    /// How many of the 6 strategies the active benchmark has started
    /// (`0..=6`), for the side panel's progress display. `None` outside an
    /// active run.
    pub benchmark_progress: Option<(usize, usize)>,
    /// The finished benchmark's rows, once `benchmark` reaches `Done`.
    /// Persists after the run finishes so the results table/"Apply winner"
    /// button stay visible until a new benchmark is started or the session
    /// resets.
    pub benchmark_rows: Option<Vec<crate::compare::BenchmarkRow>>,
    /// Every file the auto-export job has written this session (task 12.6),
    /// in write order. Fed by `poll_export`'s `ExportMessage::Written`
    /// (the paths already flowed there, just as transient events before —
    /// this is the same data, kept around instead of scrolling off the
    /// events ring buffer). Cleared whenever a fresh run starts
    /// (`start_export`) or the session resets (`start_new_session`), so it
    /// never shows a stale file from a previous run/video.
    pub exported_files: Vec<PathBuf>,
    /// The toolbar's Live/Results pill selection (task 13.1). Defaults to
    /// `Results` — a freshly opened video has no live run active yet, and
    /// `Results` is also where the "no results yet" empty state already
    /// lives, so there's nothing to switch away from until tracking starts.
    /// `start_tracking` flips it to `Live`; nothing currently flips it back
    /// automatically (left for 13.6, which owns the dedicated Live panel).
    pub display_mode: DisplayMode,
}

impl AppState {
    pub fn new(video_path: PathBuf, metadata: VideoMetadata) -> Self {
        Self {
            video_path,
            metadata,
            mode: Mode::ViewOnly,
            current_frame: 0,
            seed: None,
            calibration: None,
            last_calibration_segment: None,
            status: String::new(),
            tracking: None,
            tracking_run: TrackingRunState::default(),
            paused: false,
            bar_path: None,
            results: None,
            export: None,
            suggested_tracker: None,
            live_reps: None,
            events: VecDeque::new(),
            start_time: Instant::now(),
            settings: TrackingSettings::default(),
            benchmark: None,
            benchmark_progress: None,
            benchmark_rows: None,
            exported_files: Vec::new(),
            display_mode: DisplayMode::Results,
        }
    }

    /// Sets the Live/Results pill selection (task 13.1's toolbar toggle).
    pub fn set_display_mode(&mut self, mode: DisplayMode) {
        self.display_mode = mode;
    }

    /// Appends an event to the ring buffer, evicting the oldest if it's now
    /// over `MAX_EVENTS`.
    fn push_event(&mut self, level: EventLevel, message: impl Into<String>) {
        if self.events.len() >= MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(AppEvent {
            level,
            message: message.into(),
            elapsed_secs: self.start_time.elapsed().as_secs_f64(),
        });
    }

    /// The workflow step the guide should currently highlight (task 7.2):
    /// no Seed yet → `PlaceSeed`; Seed placed, no active/finished run →
    /// `Calibrate` (optional — needed only for m/s output); a run
    /// active/paused → `Track`; a finished `BarPath` → `Review`.
    pub fn current_step(&self) -> WorkflowStep {
        if self.bar_path.is_some() {
            WorkflowStep::Review
        } else if self.tracking.is_some() || self.tracking_run.running {
            WorkflowStep::Track
        } else if self.seed.is_some() {
            WorkflowStep::Calibrate
        } else {
            WorkflowStep::PlaceSeed
        }
    }

    /// The live phase (task 10.8) — see `Phase`'s doc comment for how this
    /// differs from `current_step`.
    pub fn phase(&self) -> Phase {
        if self.bar_path.is_some() && self.results.is_none() {
            return Phase::ComputingMetrics;
        }
        if self.results.is_some() {
            return Phase::Review;
        }
        if self.tracking.is_some() || self.tracking_run.running {
            return Phase::TrackingPath {
                frame: self.tracking_run.last_frame_index.unwrap_or(0),
                total: self.metadata.frame_count.unwrap_or(0),
            };
        }
        Phase::Idle
    }

    /// Instruction banner text for the mode strip between the toolbar and
    /// the video (task 10.7): tells the user what clicking will currently
    /// do and, for Calibrate, how many of the two points they've placed so
    /// far. Pure function of `self` — no egui dependency — so it's directly
    /// unit-testable; `mod.rs`'s banner strip just renders whatever this
    /// returns with a mode-appropriate background color.
    pub fn banner_text(&self) -> String {
        // Phase takes priority over mode once a run exists: `mode` is
        // whatever click-handler is currently armed (often stale — e.g.
        // still `PlacingSeed` from before Track was clicked, or reset to
        // `PlacingSeed` by a reseed-pause), but once tracking/results exist
        // that's what the user actually needs to hear about.
        match self.phase() {
            Phase::TrackingPath { frame, total } => {
                return if total > 0 {
                    format!("Tracking bar path… frame {frame}/{total}")
                } else {
                    format!("Tracking bar path… frame {frame}")
                };
            }
            Phase::ComputingMetrics => return "Computing metrics…".to_string(),
            Phase::Review => {
                return "Done — results in the panel. Exports written next to your video."
                    .to_string();
            }
            Phase::Idle => {}
        }
        match self.mode {
            Mode::PlacingSeed => {
                "Click the barbell — ideally the plate hub / marker. The tracker will follow it."
                    .to_string()
            }
            Mode::Calibrating { first_point, .. } => {
                let placed = if first_point.is_some() { 1 } else { 0 };
                format!(
                    "Click one edge of a plate → then the opposite edge → set its real size \
                     below (competition plate = {DEFAULT_CALIBRATION_LENGTH_METERS:.3} m). \
                     {placed} of 2 points placed."
                )
            }
            Mode::ViewOnly => {
                if self.seed.is_some() {
                    "Ready to track. Click Track when you're ready \
                     (Calibrate first if you want m/s output)."
                        .to_string()
                } else {
                    "Scrub to a frame where the bar is visible, then click Place Seed.".to_string()
                }
            }
        }
    }

    /// Toggle between `ViewOnly` and `PlacingSeed`.
    pub fn toggle_placing_seed(&mut self) {
        self.mode = match self.mode {
            Mode::PlacingSeed => Mode::ViewOnly,
            _ => Mode::PlacingSeed,
        };
        tracing::info!(mode = ?self.mode, "mode changed");
    }

    /// Toggle between `ViewOnly` and `Calibrating`. Entering `Calibrating`
    /// starts a fresh pair (no first point yet) and defaults the known
    /// length to `DEFAULT_CALIBRATION_LENGTH_METERS` (450mm plate).
    pub fn toggle_calibrating(&mut self) {
        self.mode = match self.mode {
            Mode::Calibrating { .. } => Mode::ViewOnly,
            _ => Mode::Calibrating {
                first_point: None,
                known_length_meters: DEFAULT_CALIBRATION_LENGTH_METERS,
            },
        };
        tracing::info!(mode = ?self.mode, "mode changed");
    }

    /// Update the known real-world length (in meters) used for the current
    /// calibration pair. No-op outside `Mode::Calibrating`.
    pub fn set_calibration_length(&mut self, meters: f64) {
        if let Mode::Calibrating {
            known_length_meters,
            ..
        } = &mut self.mode
        {
            *known_length_meters = meters;
        }
    }

    /// Record a Seed at the given image-pixel position on the current frame.
    /// Only takes effect in `Mode::PlacingSeed`.
    pub fn place_seed(&mut self, position: tracker_core::Point) {
        if self.mode != Mode::PlacingSeed {
            return;
        }
        self.seed = Some(Seed {
            position,
            frame_index: self.current_frame,
        });
        tracing::info!(
            frame = self.current_frame,
            x = position.x,
            y = position.y,
            "seed placed"
        );
        self.push_event(
            EventLevel::Info,
            format!(
                "seed placed at ({:.1}, {:.1}) @ frame {}",
                position.x, position.y, self.current_frame
            ),
        );
    }

    /// Records that a new video was just opened (task 10.5 — "Open video…"
    /// and the Ctrl+O shortcut both funnel here via `TrackerApp::open_video`
    /// after a fresh `AppState` is constructed for it), so the Events
    /// section shows what just happened rather than the user having to
    /// infer it from the file name in the status bar changing.
    pub fn note_video_opened(
        &mut self,
        path: &std::path::Path,
        metadata: &crate::ffprobe::VideoMetadata,
    ) {
        tracing::info!(
            video = %path.display(),
            width = metadata.display_width(),
            height = metadata.display_height(),
            fps_num = metadata.fps_num,
            fps_den = metadata.fps_den,
            rotation = metadata.rotation,
            "video opened"
        );
        self.push_event(
            EventLevel::Info,
            format!(
                "opened {} ({}x{} @ {}/{} fps, rotation {})",
                path.display(),
                metadata.display_width(),
                metadata.display_height(),
                metadata.fps_num,
                metadata.fps_den,
                metadata
                    .rotation
                    .map_or("none".to_string(), |r| r.to_string()),
            ),
        );
    }

    /// Records an arbitrary error as an `AppEvent` (task 10.5's "Open
    /// video" failure path; general-purpose enough for other adapters that
    /// want the same on-screen breadcrumb `push_event` gives internally).
    pub fn note_error(&mut self, message: impl Into<String>) {
        let message = message.into();
        tracing::error!("{message}");
        self.push_event(EventLevel::Error, message);
    }

    /// Records the tracker `suggest_tracker` recommends for the just-placed
    /// Seed (task 4.3), logging the decision. Called by `TrackerApp` right
    /// after `place_seed`, once it has the corresponding frame's pixels
    /// available from its cache — `AppState` itself never touches decoded
    /// frames.
    pub fn note_seed_suggestion(&mut self, kind: tracker_core::TrackerKind) {
        tracing::info!(kind = ?kind, "tracker auto-suggested");
        self.suggested_tracker = Some(kind);
    }

    /// Record a calibration click at the given image-pixel position. Only
    /// takes effect in `Mode::Calibrating`.
    ///
    /// The first click of a pair is just remembered. The second click
    /// attempts to build a `Calibration` from the two points and the
    /// currently configured known length; success or failure, the pair then
    /// resets so a third click starts a brand-new pair.
    pub fn place_calibration_point(&mut self, position: tracker_core::Point) {
        let Mode::Calibrating {
            first_point,
            known_length_meters,
        } = self.mode
        else {
            return;
        };

        match first_point {
            None => {
                self.mode = Mode::Calibrating {
                    first_point: Some(position),
                    known_length_meters,
                };
            }
            Some(first) => {
                self.last_calibration_segment = Some((first, position));
                match tracker_core::Calibration::new(first, position, known_length_meters) {
                    Ok(cal) => {
                        tracing::info!(px_per_meter = cal.px_per_meter(), "calibration set");
                        self.push_event(
                            EventLevel::Info,
                            format!("calibration set: {:.1} px/m", cal.px_per_meter()),
                        );
                        self.calibration = Some(cal);
                        self.status.clear();
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "calibration failed");
                        self.push_event(EventLevel::Error, format!("calibration failed: {e}"));
                        self.status = format!("calibration failed: {e}");
                    }
                }
                // Third click restarts the pair, regardless of outcome.
                self.mode = Mode::Calibrating {
                    first_point: None,
                    known_length_meters,
                };
            }
        }
    }

    /// Status-bar text reflecting mode and, once placed, the Seed and
    /// Calibration state.
    pub fn status_line(&self) -> String {
        let mode = match self.mode {
            Mode::ViewOnly => "view".to_string(),
            Mode::PlacingSeed => "placing seed (click frame)".to_string(),
            Mode::Calibrating { first_point, .. } => match first_point {
                Some(_) => "calibrating (click 2nd point)".to_string(),
                None => "calibrating (click 1st point)".to_string(),
            },
        };
        let seed_part = match &self.seed {
            Some(seed) => {
                let suggestion = match self.suggested_tracker {
                    Some(tracker_core::TrackerKind::Color) => " (suggested: Color)",
                    Some(tracker_core::TrackerKind::Template) => " (suggested: Template)",
                    None => "",
                };
                format!(
                    "seed: ({:.1}, {:.1}) @ frame {}{suggestion}",
                    seed.position.x, seed.position.y, seed.frame_index
                )
            }
            None => "seed: none".to_string(),
        };
        let calibration_part = match &self.calibration {
            Some(cal) => format!("calibration: {:.1} px/m", cal.px_per_meter()),
            None => "calibration: none".to_string(),
        };
        format!("mode: {mode}  |  {seed_part}  |  {calibration_part}")
    }

    fn frame_count(&self) -> u64 {
        self.metadata.frame_count.unwrap_or(1)
    }

    /// Set the current frame, clamped to the valid range for this video.
    pub fn set_frame(&mut self, requested: i64) {
        self.current_frame = clamp_frame_index(requested, self.frame_count());
    }

    pub fn next_frame(&mut self) {
        self.set_frame(self.current_frame as i64 + 1);
    }

    pub fn prev_frame(&mut self) {
        self.set_frame(self.current_frame as i64 - 1);
    }

    /// Whether the "Track" action should currently be available: a Seed
    /// must be placed, and no run already active.
    pub fn can_start_tracking(&self) -> bool {
        self.seed.is_some() && self.tracking.is_none()
    }

    /// Spawns a background tracking run from the current Seed, using this
    /// module's default `TemplateTracker`/`TrackingSession` tuning. No-op if
    /// `can_start_tracking` is false.
    pub fn start_tracking(&mut self) {
        if !self.can_start_tracking() {
            return;
        }
        let Some(seed) = self.seed else { return };
        let tuning = self.settings.tuning();
        tracing::info!(
            video = %self.video_path.display(),
            seed_frame = seed.frame_index,
            x = seed.position.x,
            y = seed.position.y,
            strategy = %self.settings.describe(),
            "track started"
        );
        self.push_event(
            EventLevel::Info,
            format!(
                "tracking started: {} @ frame {}",
                self.settings.describe(),
                seed.frame_index
            ),
        );
        let handle = tracking::spawn_tracking(tracking::TrackingJob {
            video_path: self.video_path.clone(),
            width: self.metadata.display_width(),
            height: self.metadata.display_height(),
            fps_num: self.metadata.fps_num,
            fps_den: self.metadata.fps_den,
            seed_frame_index: seed.frame_index,
            seed_position: seed.position,
            tracker_config: tracking::tracker_config(tuning.clone()),
            session_config: tracking::session_config(tuning.clone()),
            tracker_selection: self.settings.tracker_selection,
            color_tracker_config: tracking::color_tracker_config(tuning),
        });
        self.tracking = Some(handle);
        self.tracking_run = TrackingRunState::started();
        self.bar_path = None;
        self.live_reps = None;
        // A fresh run has no results to show yet — flip the pill to Live
        // (task 13.1) so the toolbar reflects what's actually happening.
        self.display_mode = DisplayMode::Live;
    }

    /// Drains any pending messages from the active tracking worker,
    /// applying each to `tracking_run` and advancing the display frame to
    /// follow the latest tracked/interpolated position. Returns `true` if
    /// at least one message was processed (the caller should request a
    /// repaint). Once the run finishes (or errors), stores the completed
    /// `BarPath` (if any) and drops the worker handle.
    pub fn poll_tracking(&mut self) -> bool {
        let Some(handle) = &self.tracking else {
            return false;
        };
        let mut any = false;
        let mut finished = false;
        let mut messages = Vec::new();
        while let Ok(msg) = handle.messages.try_recv() {
            messages.push(msg);
        }
        for msg in messages {
            any = true;
            if let Some(frame_index) = msg.video_frame_index() {
                // `ffprobe`'s `nb_frames` (this crate's `metadata.frame_count`)
                // is only an estimate for some containers/frame rates (this
                // project's test footage reports odd rates like `600/19`)
                // and can undercount the frames ffmpeg actually decodes
                // (PLAN 10.1). If tracking reports a frame past what we
                // thought the video's length was, that means the video
                // genuinely has more frames than ffprobe estimated: grow
                // the known length to match rather than clamping the
                // display frame to the stale (too-small) estimate, which
                // would otherwise freeze the video panel while the status
                // text kept climbing past it — exactly the "runs past
                // video end" symptom the user saw.
                let seen = frame_index + 1;
                if self.metadata.frame_count.unwrap_or(0) < seen {
                    self.metadata.frame_count = Some(seen);
                }
                self.set_frame(frame_index as i64);
            }
            let was_paused =
                self.tracking_run.session_state == Some(tracker_core::SessionState::NeedsReseed);
            if self.tracking_run.apply(msg) {
                finished = true;
            }
            let now_paused =
                self.tracking_run.session_state == Some(tracker_core::SessionState::NeedsReseed);
            if now_paused && !was_paused {
                self.push_event(
                    EventLevel::Warn,
                    format!(
                        "tracking paused @ frame {}: object lost",
                        self.tracking_run.last_frame_index.unwrap_or_default()
                    ),
                );
            }
            // Task 10.8: live rep counter, thrown every
            // `LIVE_REP_RECOMPUTE_INTERVAL` processed frames. Cheap pure
            // math over the partial path, but still guarded: a failed
            // recompute (e.g. too few samples yet) just skips the update
            // rather than touching `tracking_run`/`tracking` — it must
            // never disturb the run itself.
            if self.tracking_run.should_recompute_live_reps() {
                if let Ok(timebase) =
                    tracker_core::Timebase::new(self.metadata.fps_num, self.metadata.fps_den)
                {
                    if let Some(count) =
                        self.tracking_run.live_rep_count(timebase, self.calibration)
                    {
                        self.live_reps = Some(count);
                    }
                }
            }
        }
        if finished {
            self.bar_path = self.tracking_run.bar_path.clone();
            self.tracking = None;
            if let Some(e) = &self.tracking_run.error {
                self.push_event(EventLevel::Error, format!("tracking error: {e}"));
            } else {
                self.push_event(
                    EventLevel::Info,
                    format!(
                        "tracking complete ({} frames)",
                        self.tracking_run.frames_processed
                    ),
                );
                if let Some(bar_path) = self.bar_path.clone() {
                    let results = SessionResults::build(
                        bar_path,
                        self.calibration,
                        self.tracking_run.gap_count,
                    );
                    match &results.velocity {
                        Ok(v) => {
                            self.push_event(
                                EventLevel::Info,
                                format!(
                                    "reps detected: {} ({} velocity samples)",
                                    results.reps.len(),
                                    v.len()
                                ),
                            );
                        }
                        Err(e) => {
                            self.push_event(EventLevel::Warn, format!("velocity unavailable: {e}"));
                        }
                    }
                    self.start_export(&results);
                    self.results = Some(results);
                    // Results just became available — flip the pill back
                    // (task 13.1) so the toolbar reflects the state a
                    // finished run naturally lands in.
                    self.display_mode = DisplayMode::Results;
                }
            }
        }
        any
    }

    /// Whether the "Test strategies" button (task 11.4) should currently be
    /// available: a Seed must be placed, and neither a tracking run nor
    /// another benchmark is already active.
    pub fn can_test_strategies(&self) -> bool {
        self.seed.is_some() && self.tracking.is_none() && self.benchmark.is_none()
    }

    /// Spawns the background strategy benchmark (task 11.4): the fixed
    /// 6-strategy matrix over a `compare::DEFAULT_COMPARE_FRAMES`-frame
    /// segment starting at the current Seed. No-op if
    /// `can_test_strategies` is false. Reuses the current settings' tuning
    /// as the shared baseline (patch/search radius etc); only the filter
    /// chain and tracker kind vary per strategy, same as the CLI `compare`
    /// subcommand.
    pub fn start_strategy_benchmark(&mut self) {
        if !self.can_test_strategies() {
            return;
        }
        let Some(seed) = self.seed else { return };
        let tuning = self.settings.tuning();
        let coast_limit = self.settings.coast_limit;
        let strategy_count = crate::compare::strategy_matrix().len();
        tracing::info!(
            video = %self.video_path.display(),
            seed_frame = seed.frame_index,
            strategy_count,
            "strategy benchmark started"
        );
        self.push_event(
            EventLevel::Info,
            format!(
                "strategy benchmark started @ frame {} ({strategy_count} strategies)",
                seed.frame_index
            ),
        );
        let handle = crate::compare::spawn_benchmark(
            self.video_path.clone(),
            self.metadata.display_width(),
            self.metadata.display_height(),
            seed.frame_index,
            seed.position,
            crate::compare::DEFAULT_COMPARE_FRAMES,
            coast_limit,
            tuning,
        );
        self.benchmark = Some(handle);
        self.benchmark_progress = Some((0, 6));
        self.benchmark_rows = None;
    }

    /// Drains any pending messages from the active benchmark worker.
    /// Returns `true` if at least one message was processed (the caller
    /// should request a repaint), mirroring `poll_tracking`/`poll_export`'s
    /// shape.
    pub fn poll_benchmark(&mut self) -> bool {
        let Some(handle) = &self.benchmark else {
            return false;
        };
        let mut any = false;
        let mut messages = Vec::new();
        while let Ok(msg) = handle.messages.try_recv() {
            messages.push(msg);
        }
        for msg in messages {
            any = true;
            match msg {
                crate::compare::BenchmarkMessage::Progress {
                    strategy_index,
                    total,
                } => {
                    self.benchmark_progress = Some((strategy_index, total));
                }
                crate::compare::BenchmarkMessage::Done(rows) => {
                    let winner_label = crate::compare::recommend(
                        &rows.iter().map(|r| r.metrics).collect::<Vec<_>>(),
                    )
                    .map(|i| rows[i].strategy.label());
                    let message = match &winner_label {
                        Some(label) => format!(
                            "strategy benchmark complete ({} strategies, winner: {label})",
                            rows.len()
                        ),
                        None => format!("strategy benchmark complete ({} strategies)", rows.len()),
                    };
                    tracing::info!(
                        strategy_count = rows.len(),
                        winner = winner_label.as_deref(),
                        "strategy benchmark done"
                    );
                    self.push_event(EventLevel::Info, message);
                    self.benchmark_rows = Some(rows);
                    self.benchmark = None;
                    self.benchmark_progress = None;
                }
                crate::compare::BenchmarkMessage::Error(e) => {
                    self.push_event(EventLevel::Error, format!("strategy benchmark error: {e}"));
                    self.benchmark = None;
                    self.benchmark_progress = None;
                }
            }
        }
        any
    }

    /// Applies the benchmarked winner's filter chain + tracker kind to
    /// `self.settings` ("Apply winner" button, task 11.4). No-op if there
    /// are no benchmark results yet. Mirrors `TrackingSettings::default`'s
    /// gaussian-then-median fixed order: a strategy is always exactly one
    /// filter (or none), so at most one of `gaussian_enabled`/
    /// `median_enabled` is ever set true here.
    pub fn apply_benchmark_winner(&mut self) {
        let Some(rows) = &self.benchmark_rows else {
            return;
        };
        let metrics: Vec<crate::compare::StrategyMetrics> =
            rows.iter().map(|r| r.metrics).collect();
        let Some(winner_index) = crate::compare::recommend(&metrics) else {
            return;
        };
        let winner = rows[winner_index].strategy;
        self.settings.tracker_selection = winner.tracker;
        self.settings.gaussian_enabled = false;
        self.settings.median_enabled = false;
        match winner.filter {
            crate::compare::FilterKind::None => {}
            crate::compare::FilterKind::Gaussian1_5 => {
                self.settings.gaussian_enabled = true;
                self.settings.gaussian_sigma = 1.5;
            }
            crate::compare::FilterKind::Median3 => {
                self.settings.median_enabled = true;
                self.settings.median_k = 3;
            }
        }
        self.push_event(
            EventLevel::Info,
            format!("applied benchmark winner: {}", winner.label()),
        );
    }

    /// Kicks off the background auto-export job for a just-finished run
    /// (task 10.3): overlay MP4 + CSV/JSON/reps exports, written next to
    /// the source video. Fire-and-forget from the caller's perspective —
    /// progress/errors surface as events via `poll_export`.
    fn start_export(&mut self, results: &SessionResults) {
        let job = export_job::ExportJob {
            video_path: self.video_path.clone(),
            width: self.metadata.display_width(),
            height: self.metadata.display_height(),
            fps_num: self.metadata.fps_num,
            fps_den: self.metadata.fps_den,
            bar_path: results.bar_path.clone(),
            calibration: self.calibration,
            velocity: results.velocity.as_ref().ok().cloned(),
            metrics: results.metrics.clone(),
            reps: results.reps.clone(),
        };
        self.exported_files.clear();
        self.push_event(EventLevel::Info, "auto-export started".to_string());
        self.export = Some(export_job::spawn_export(job));
    }

    /// Drains any pending messages from the active export job, applying
    /// each as an event. Returns `true` if at least one message was
    /// processed (the caller should request a repaint). Mirrors
    /// `poll_tracking`'s drain-then-react shape.
    pub fn poll_export(&mut self) -> bool {
        let Some(handle) = &self.export else {
            return false;
        };
        let mut any = false;
        let mut done = false;
        let mut messages = Vec::new();
        while let Ok(msg) = handle.messages.try_recv() {
            messages.push(msg);
        }
        for msg in messages {
            any = true;
            match msg {
                ExportMessage::Written(path) => {
                    self.push_event(EventLevel::Info, format!("exported: {}", path.display()));
                    self.exported_files.push(path);
                }
                ExportMessage::Error(e) => {
                    self.push_event(EventLevel::Error, format!("export failed: {e}"));
                }
                ExportMessage::Done => done = true,
            }
        }
        if done {
            self.export = None;
            self.push_event(EventLevel::Info, "exports written".to_string());
        }
        any
    }

    /// Sends a reseed command to a paused tracking worker, using the
    /// current Seed (which must already be placed on the frame the run
    /// paused at — the UI only enables the Resume action once that's
    /// true). No-op if there's no active worker or no Seed.
    pub fn resume_tracking(&mut self) {
        let (Some(handle), Some(seed)) = (&self.tracking, self.seed) else {
            return;
        };
        handle.resume(seed.frame_index, seed.position);
        self.push_event(
            EventLevel::Info,
            format!("resumed tracking @ frame {}", seed.frame_index),
        );
    }

    // -- Task 10.4: session lifecycle controls -----------------------------
    //
    // User pain this fixes: "had to kill the app to run tracking twice".
    // Four run-time controls (Pause/Resume, Stop, Discard) plus two
    // Review-step controls (New session, Re-track). All go through the
    // worker's `ControlCommand` channel (`tracking.rs`) rather than being a
    // GUI-side illusion on top of a worker that keeps decoding underneath —
    // Pause genuinely blocks the worker's frame consumption, and Stop/
    // Discard tell it to stop at the next checkpoint (promptly: checked
    // before every frame read) rather than waiting for it to run to
    // completion. Every action is gated by a `can_*` predicate the toolbar
    // uses to enable/disable its buttons, and emits both a `tracing`
    // breadcrumb and an `AppEvent`.

    /// Whether Pause is currently available: an active run that isn't
    /// already paused (either by the user or by the tracker's own
    /// object-lost `NeedsReseed` state, which already halts consumption on
    /// its own and has its own Resume button in the toolbar).
    pub fn can_pause_tracking(&self) -> bool {
        self.tracking.is_some()
            && !self.paused
            && self.tracking_run.session_state != Some(tracker_core::SessionState::NeedsReseed)
    }

    /// Pauses the active run: the worker stops consuming frames until
    /// `unpause_tracking` is called. No-op if `can_pause_tracking` is false.
    pub fn pause_tracking(&mut self) {
        if !self.can_pause_tracking() {
            return;
        }
        if let Some(handle) = &self.tracking {
            handle.pause();
        }
        self.paused = true;
        tracing::info!("tracking paused (user)");
        self.push_event(EventLevel::Info, "tracking paused".to_string());
    }

    /// Whether Resume (from a user Pause, not a reseed) is currently
    /// available.
    pub fn can_unpause_tracking(&self) -> bool {
        self.tracking.is_some() && self.paused
    }

    /// Resumes a user-paused run. No-op if `can_unpause_tracking` is false.
    pub fn unpause_tracking(&mut self) {
        if !self.can_unpause_tracking() {
            return;
        }
        if let Some(handle) = &self.tracking {
            handle.unpause();
        }
        self.paused = false;
        tracing::info!("tracking resumed (user)");
        self.push_event(EventLevel::Info, "tracking resumed".to_string());
    }

    /// Whether Stop is currently available: any active (running or paused)
    /// run.
    pub fn can_stop_tracking(&self) -> bool {
        self.tracking.is_some()
    }

    /// Tells the worker to finish now, keeping whatever samples it has
    /// collected so far: the worker still sends a normal `Done`, so
    /// `poll_tracking` builds `SessionResults`/kicks off auto-export exactly
    /// as it would for a run that reached clean end-of-video — the run just
    /// lands in Review early, with partial results. No-op if
    /// `can_stop_tracking` is false.
    pub fn stop_tracking(&mut self) {
        if !self.can_stop_tracking() {
            return;
        }
        if let Some(handle) = &self.tracking {
            handle.stop();
        }
        self.paused = false;
        tracing::info!("tracking stop requested (user)");
        self.push_event(
            EventLevel::Info,
            "stop requested: finishing with results so far".to_string(),
        );
    }

    /// Whether Discard is currently available: same gate as Stop (any
    /// active run).
    pub fn can_discard_tracking(&self) -> bool {
        self.tracking.is_some()
    }

    /// Aborts the active run and throws away anything it collected: unlike
    /// `stop_tracking`, this never lands in Review — the worker is told to
    /// stop (same `ControlCommand::Stop`, so it still terminates promptly
    /// and its `FfmpegFrameSource` still gets dropped/killed) but its
    /// eventual `Done`/`Error` message is simply never read, since
    /// `self.tracking` is cleared here rather than left for `poll_tracking`
    /// to drain. Returns the app to seed placement with the Seed intact —
    /// the user re-tracks from the same seed rather than re-placing it. No-op
    /// if `can_discard_tracking` is false.
    pub fn discard_tracking(&mut self) {
        if !self.can_discard_tracking() {
            return;
        }
        if let Some(handle) = &self.tracking {
            handle.stop();
        }
        self.tracking = None;
        self.tracking_run = TrackingRunState::default();
        self.bar_path = None;
        self.results = None;
        self.export = None;
        self.paused = false;
        self.live_reps = None;
        self.mode = Mode::PlacingSeed;
        tracing::info!("tracking discarded (user)");
        self.push_event(EventLevel::Warn, "tracking discarded".to_string());
    }

    /// Whether the Review-step controls (New session / Re-track) are
    /// available: only once a run has finished (`WorkflowStep::Review`).
    fn in_review(&self) -> bool {
        self.current_step() == WorkflowStep::Review
    }

    /// Whether "New session" is currently available.
    pub fn can_start_new_session(&self) -> bool {
        self.in_review()
    }

    /// Resets everything (Seed, Calibration, results, events) and returns to
    /// step 1, keeping the same video loaded — the "New session" button. No
    /// app restart, unlike before this task. No-op if `can_start_new_session`
    /// is false.
    pub fn start_new_session(&mut self) {
        if !self.can_start_new_session() {
            return;
        }
        self.seed = None;
        self.calibration = None;
        self.last_calibration_segment = None;
        self.suggested_tracker = None;
        self.tracking = None;
        self.tracking_run = TrackingRunState::default();
        self.paused = false;
        self.bar_path = None;
        self.results = None;
        self.export = None;
        self.live_reps = None;
        self.mode = Mode::ViewOnly;
        self.status.clear();
        self.events.clear();
        self.exported_files.clear();
        tracing::info!("new session started");
        self.push_event(EventLevel::Info, "new session started".to_string());
    }

    /// Whether "Re-track" is currently available: Review step, and (since
    /// it starts a fresh run immediately) a Seed to start from.
    pub fn can_retrack(&self) -> bool {
        self.in_review() && self.seed.is_some()
    }

    /// Keeps the Seed and Calibration, clears the previous run's results,
    /// and immediately starts a new tracking run — the "Re-track" button.
    /// No-op if `can_retrack` is false.
    pub fn retrack(&mut self) {
        if !self.can_retrack() {
            return;
        }
        self.bar_path = None;
        self.results = None;
        self.export = None;
        self.tracking_run = TrackingRunState::default();
        self.paused = false;
        tracing::info!("re-track started");
        self.push_event(EventLevel::Info, "re-track started".to_string());
        self.start_tracking();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(frame_count: Option<u64>) -> VideoMetadata {
        VideoMetadata {
            width: 4,
            height: 4,
            fps_num: 30,
            fps_den: 1,
            frame_count,
            rotation: None,
        }
    }

    #[test]
    fn starts_at_frame_zero() {
        let state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        assert_eq!(state.current_frame, 0);
    }

    #[test]
    fn next_frame_advances_by_one() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.next_frame();
        assert_eq!(state.current_frame, 1);
    }

    #[test]
    fn prev_frame_at_zero_stays_at_zero() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.prev_frame();
        assert_eq!(state.current_frame, 0);
    }

    #[test]
    fn next_frame_at_last_index_stays_put() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.set_frame(9);
        state.next_frame();
        assert_eq!(state.current_frame, 9);
    }

    #[test]
    fn set_frame_clamps_out_of_range_requests() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.set_frame(1000);
        assert_eq!(state.current_frame, 9);
        state.set_frame(-5);
        assert_eq!(state.current_frame, 0);
    }

    #[test]
    fn missing_frame_count_treated_as_single_frame_video() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(None));
        state.next_frame();
        assert_eq!(state.current_frame, 0);
    }

    #[test]
    fn toggle_placing_seed_switches_modes() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        assert_eq!(state.mode, Mode::ViewOnly);
        state.toggle_placing_seed();
        assert_eq!(state.mode, Mode::PlacingSeed);
        state.toggle_placing_seed();
        assert_eq!(state.mode, Mode::ViewOnly);
    }

    #[test]
    fn place_seed_is_ignored_outside_placing_mode() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.place_seed(tracker_core::Point::new(1.0, 2.0));
        assert!(state.seed.is_none());
    }

    #[test]
    fn place_seed_records_position_and_current_frame() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.toggle_placing_seed();
        state.set_frame(3);
        state.place_seed(tracker_core::Point::new(12.5, 7.0));
        let seed = state.seed.expect("seed should be set");
        assert_eq!(seed.frame_index, 3);
        assert_eq!(seed.position, tracker_core::Point::new(12.5, 7.0));
    }

    #[test]
    fn toggle_calibrating_switches_modes_and_seeds_default_length() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.toggle_calibrating();
        match state.mode {
            Mode::Calibrating {
                first_point,
                known_length_meters,
            } => {
                assert_eq!(first_point, None);
                assert_eq!(known_length_meters, DEFAULT_CALIBRATION_LENGTH_METERS);
            }
            _ => panic!("expected Calibrating mode"),
        }
        state.toggle_calibrating();
        assert_eq!(state.mode, Mode::ViewOnly);
    }

    #[test]
    fn two_clicks_resolve_calibration_with_default_length() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.toggle_calibrating();
        state.place_calibration_point(tracker_core::Point::new(0.0, 0.0));
        assert!(state.calibration.is_none());
        state.place_calibration_point(tracker_core::Point::new(200.0, 0.0));
        let cal = state.calibration.expect("calibration should resolve");
        assert!((cal.px_per_meter() - (200.0 / DEFAULT_CALIBRATION_LENGTH_METERS)).abs() < 1e-6);
    }

    #[test]
    fn third_click_restarts_the_pair() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.toggle_calibrating();
        state.place_calibration_point(tracker_core::Point::new(0.0, 0.0));
        state.place_calibration_point(tracker_core::Point::new(200.0, 0.0));
        // Third click: starts a fresh pair rather than being treated as a
        // second point of the old one.
        state.place_calibration_point(tracker_core::Point::new(50.0, 50.0));
        match state.mode {
            Mode::Calibrating { first_point, .. } => {
                assert_eq!(first_point, Some(tracker_core::Point::new(50.0, 50.0)));
            }
            _ => panic!("expected Calibrating mode"),
        }
    }

    #[test]
    fn coincident_calibration_points_surface_error_and_restart_pair() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.toggle_calibrating();
        state.place_calibration_point(tracker_core::Point::new(10.0, 10.0));
        state.place_calibration_point(tracker_core::Point::new(10.0, 10.0));
        assert!(state.calibration.is_none());
        assert!(state.status.contains("calibration failed"));
        match state.mode {
            Mode::Calibrating { first_point, .. } => assert_eq!(first_point, None),
            _ => panic!("expected Calibrating mode"),
        }
    }

    #[test]
    fn calibration_clicks_are_ignored_outside_calibrating_mode() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.place_calibration_point(tracker_core::Point::new(0.0, 0.0));
        assert_eq!(state.mode, Mode::ViewOnly);
        assert!(state.calibration.is_none());
    }

    #[test]
    fn set_calibration_length_updates_pending_pair() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.toggle_calibrating();
        state.set_calibration_length(1.0);
        state.place_calibration_point(tracker_core::Point::new(0.0, 0.0));
        state.place_calibration_point(tracker_core::Point::new(100.0, 0.0));
        let cal = state.calibration.expect("calibration should resolve");
        assert!((cal.px_per_meter() - 100.0).abs() < 1e-6);
    }

    #[test]
    fn status_line_reports_calibration_px_per_meter() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.toggle_calibrating();
        state.place_calibration_point(tracker_core::Point::new(0.0, 0.0));
        state.place_calibration_point(tracker_core::Point::new(200.0, 0.0));
        let line = state.status_line();
        assert!(line.contains("px/m"));
    }

    #[test]
    fn status_line_reflects_mode_and_seed() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        assert!(state.status_line().contains("view"));
        assert!(state.status_line().contains("seed: none"));

        state.toggle_placing_seed();
        state.place_seed(tracker_core::Point::new(4.0, 5.0));
        let line = state.status_line();
        assert!(line.contains("placing seed"));
        assert!(line.contains("4.0"));
        assert!(line.contains("5.0"));
    }

    #[test]
    fn current_step_starts_at_place_seed() {
        let state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        assert_eq!(state.current_step(), WorkflowStep::PlaceSeed);
    }

    #[test]
    fn current_step_is_calibrate_once_seed_placed() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.toggle_placing_seed();
        state.place_seed(tracker_core::Point::new(1.0, 1.0));
        assert_eq!(state.current_step(), WorkflowStep::Calibrate);
    }

    #[test]
    fn current_step_is_track_while_tracking_run_state_is_running() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.tracking_run = TrackingRunState::started();
        assert_eq!(state.current_step(), WorkflowStep::Track);
    }

    #[test]
    fn current_step_is_review_once_bar_path_present() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        let tb = tracker_core::Timebase::new(30, 1).unwrap();
        state.bar_path = Some(tracker_core::BarPath::new(&[], &[], tb, 0));
        assert_eq!(state.current_step(), WorkflowStep::Review);
    }

    #[test]
    fn place_seed_pushes_an_info_event() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.toggle_placing_seed();
        state.place_seed(tracker_core::Point::new(1.0, 2.0));
        assert_eq!(state.events.len(), 1);
        let event = state.events.back().unwrap();
        assert_eq!(event.level, EventLevel::Info);
        assert!(event.message.contains("seed placed"));
    }

    #[test]
    fn calibration_failure_pushes_an_error_event() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.toggle_calibrating();
        state.place_calibration_point(tracker_core::Point::new(5.0, 5.0));
        state.place_calibration_point(tracker_core::Point::new(5.0, 5.0));
        let event = state.events.back().unwrap();
        assert_eq!(event.level, EventLevel::Error);
        assert!(event.message.contains("calibration failed"));
    }

    #[test]
    fn event_ring_buffer_caps_at_max_events() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.toggle_placing_seed();
        for i in 0..(MAX_EVENTS + 5) {
            state.place_seed(tracker_core::Point::new(i as f64, 0.0));
        }
        assert_eq!(state.events.len(), MAX_EVENTS);
        // Oldest events were evicted; the last pushed one is still there.
        let last = state.events.back().unwrap();
        assert!(last
            .message
            .contains(&format!("({:.1}", (MAX_EVENTS + 4) as f64)));
    }

    // -- SessionResults (10.3) ------------------------------------------

    fn sample(
        frame_index: u64,
        x: f64,
        y: f64,
        source: tracker_core::Source,
    ) -> tracker_core::Sample {
        tracker_core::Sample {
            frame_index,
            position: tracker_core::Point::new(x, y),
            source,
        }
    }

    /// A synthetic bar path with a clean single rep: descent (y 0->10) then
    /// ascent (y 10->0) across 20 tracked, evenly-spaced frames at 30fps —
    /// enough for `velocity_series`/`segment_reps` to detect exactly one
    /// rep without tripping any of `rep.rs`'s noise-robustness dead-bands.
    fn one_rep_bar_path() -> tracker_core::BarPath {
        let tb = tracker_core::Timebase::new(30, 1).unwrap();
        let mut samples = Vec::new();
        for i in 0..=10u64 {
            samples.push(sample(
                i,
                0.0,
                i as f64 * 10.0,
                tracker_core::Source::Tracked,
            ));
        }
        for i in 11..=20u64 {
            samples.push(sample(
                i,
                0.0,
                (20 - i) as f64 * 10.0,
                tracker_core::Source::Tracked,
            ));
        }
        tracker_core::BarPath::new(&samples, &[], tb, 0)
    }

    #[test]
    fn session_results_build_detects_reps_and_reports_units() {
        let results = SessionResults::build(one_rep_bar_path(), None, 0);
        assert!(results.velocity.is_ok());
        assert_eq!(results.reps.len(), 1);
        assert_eq!(results.metrics.len(), 1);
        assert_eq!(
            results.unit,
            Some(tracker_core::VelocityUnit::PixelsPerSecond)
        );
        assert_eq!(results.quality.total_points, 21);
        assert_eq!(results.quality.interpolated_points, 0);
        assert_eq!(results.quality.gap_count, 0);
    }

    #[test]
    fn session_results_build_scales_to_meters_per_second_when_calibrated() {
        let cal = tracker_core::Calibration::new(
            tracker_core::Point::new(0.0, 0.0),
            tracker_core::Point::new(100.0, 0.0),
            1.0,
        )
        .unwrap();
        let results = SessionResults::build(one_rep_bar_path(), Some(cal), 0);
        assert_eq!(
            results.unit,
            Some(tracker_core::VelocityUnit::MetersPerSecond)
        );
        assert_eq!(results.reps.len(), 1);
    }

    #[test]
    fn session_results_build_surfaces_velocity_error_instead_of_silently_empty_reps() {
        // A single-point path: too few points for `velocity_series`
        // (10.9's GUI seam -- must be an `Err`, not a silent empty Vec).
        let tb = tracker_core::Timebase::new(30, 1).unwrap();
        let samples = vec![sample(0, 0.0, 0.0, tracker_core::Source::Tracked)];
        let bar_path = tracker_core::BarPath::new(&samples, &[], tb, 0);
        let results = SessionResults::build(bar_path, None, 0);
        assert_eq!(
            results.velocity,
            Err(tracker_core::VelocityError::TooFewPoints)
        );
        assert!(results.reps.is_empty());
        assert!(results.metrics.is_empty());
    }

    #[test]
    fn results_quality_interpolated_percent_is_computed_from_point_counts() {
        let q = ResultsQuality {
            gap_count: 2,
            reseed_count: 2,
            interpolated_points: 5,
            total_points: 20,
        };
        assert!((q.interpolated_percent() - 25.0).abs() < 1e-9);
    }

    #[test]
    fn results_quality_interpolated_percent_is_zero_for_empty_path() {
        let q = ResultsQuality::default();
        assert_eq!(q.interpolated_percent(), 0.0);
    }

    #[test]
    fn poll_tracking_done_populates_results_and_starts_export() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(30)));
        state.tracking_run = TrackingRunState::started();
        state.tracking_run.gap_count = 1;
        let bar_path = one_rep_bar_path();
        state.tracking_run.bar_path = Some(bar_path);
        let finished = state.tracking_run.apply(tracking::TrackingMessage::Done(
            state.tracking_run.bar_path.clone().unwrap(),
        ));
        assert!(finished);
        // Mirror what `poll_tracking` does on a `Done` message without a
        // real worker thread/channel (unit-testable slice of the same
        // logic that method runs).
        state.bar_path = state.tracking_run.bar_path.clone();
        if let Some(bp) = state.bar_path.clone() {
            let results =
                SessionResults::build(bp, state.calibration, state.tracking_run.gap_count);
            assert_eq!(results.reps.len(), 1);
            assert_eq!(results.quality.gap_count, 1);
            state.results = Some(results);
        }
        assert!(state.results.is_some());
        assert_eq!(state.current_step(), WorkflowStep::Review);
    }

    // -- Task 10.4: session lifecycle controls ------------------------------

    /// A `TrackingHandle` for gating/reset tests: `TrackingHandle`'s fields
    /// are private outside `tracking.rs`, so the only way to get one here is
    /// `spawn_tracking` itself. The job points at a nonexistent path — the
    /// worker thread fails fast (`FfmpegFrameSource::spawn` errors) and
    /// sends a `TrackingMessage::Error`, which is fine: these tests only
    /// exercise `AppState`'s synchronous reducer logic (gating predicates,
    /// field resets) around `Some(handle)`/`None`, never the message
    /// contents.
    fn dummy_tracking_handle() -> TrackingHandle {
        tracking::spawn_tracking(tracking::TrackingJob {
            video_path: PathBuf::from("/definitely/does/not/exist-10-4.mp4"),
            width: 4,
            height: 4,
            fps_num: 30,
            fps_den: 1,
            seed_frame_index: 0,
            seed_position: tracker_core::Point::new(0.0, 0.0),
            tracker_config: tracking::default_tracker_config(),
            session_config: tracking::default_session_config(),
            tracker_selection: tracking::TrackerSelection::Auto,
            color_tracker_config: tracking::default_color_tracker_config(),
        })
    }

    fn state_with_active_run() -> AppState {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.toggle_placing_seed();
        state.set_frame(3);
        state.place_seed(tracker_core::Point::new(5.0, 5.0));
        state.tracking = Some(dummy_tracking_handle());
        state.tracking_run = TrackingRunState::started();
        state
    }

    #[test]
    fn pause_tracking_sets_paused_flag_and_is_gated_on_an_active_unpaused_run() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        assert!(!state.can_pause_tracking());
        state.pause_tracking();
        assert!(!state.paused, "no active run: pause must be a no-op");

        let mut state = state_with_active_run();
        assert!(state.can_pause_tracking());
        state.pause_tracking();
        assert!(state.paused);
        assert!(!state.can_pause_tracking(), "already paused");
        assert!(state.can_unpause_tracking());
    }

    #[test]
    fn pause_is_unavailable_while_the_tracker_itself_is_paused_for_reseed() {
        let mut state = state_with_active_run();
        state.tracking_run.session_state = Some(tracker_core::SessionState::NeedsReseed);
        assert!(
            !state.can_pause_tracking(),
            "NeedsReseed already halts consumption; the reseed flow owns Resume, not Pause"
        );
    }

    #[test]
    fn unpause_tracking_clears_paused_flag() {
        let mut state = state_with_active_run();
        state.pause_tracking();
        assert!(state.paused);
        state.unpause_tracking();
        assert!(!state.paused);
        assert!(!state.can_unpause_tracking());
    }

    #[test]
    fn stop_tracking_is_gated_on_an_active_run_and_pushes_an_event() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        assert!(!state.can_stop_tracking());
        state.stop_tracking();
        assert!(
            state.events.is_empty(),
            "no active run: stop must be a no-op"
        );

        let mut state = state_with_active_run();
        assert!(state.can_stop_tracking());
        state.stop_tracking();
        let event = state.events.back().unwrap();
        assert!(event.message.contains("stop requested"));
        // Stop is a request to the worker, not an immediate teardown: the
        // handle/tracking_run stay in place until the worker's `Done`
        // arrives via `poll_tracking`, same as a clean-EOF finish.
        assert!(state.tracking.is_some());
    }

    #[test]
    fn discard_tracking_tears_down_the_run_but_keeps_the_seed_and_returns_to_placing_seed() {
        let mut state = state_with_active_run();
        state.calibration = tracker_core::Calibration::new(
            tracker_core::Point::new(0.0, 0.0),
            tracker_core::Point::new(100.0, 0.0),
            1.0,
        )
        .ok();
        let seed_before = state.seed;

        assert!(state.can_discard_tracking());
        state.discard_tracking();

        assert!(state.tracking.is_none());
        assert!(!state.tracking_run.running);
        assert!(state.bar_path.is_none());
        assert!(state.results.is_none());
        assert!(!state.paused);
        assert_eq!(state.mode, Mode::PlacingSeed);
        // The whole point: seed survives so the user re-tracks without
        // re-placing it.
        assert_eq!(state.seed, seed_before);
        assert!(state.seed.is_some());
        // Calibration is untouched by Discard too.
        assert!(state.calibration.is_some());
        let event = state.events.back().unwrap();
        assert_eq!(event.level, EventLevel::Warn);
        assert!(event.message.contains("discarded"));
    }

    #[test]
    fn discard_tracking_is_a_noop_without_an_active_run() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        assert!(!state.can_discard_tracking());
        state.discard_tracking();
        assert!(state.events.is_empty());
    }

    fn state_in_review() -> AppState {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(30)));
        state.toggle_placing_seed();
        state.place_seed(tracker_core::Point::new(1.0, 1.0));
        state.calibration = tracker_core::Calibration::new(
            tracker_core::Point::new(0.0, 0.0),
            tracker_core::Point::new(100.0, 0.0),
            1.0,
        )
        .ok();
        let bar_path = one_rep_bar_path();
        state.bar_path = Some(bar_path.clone());
        state.results = Some(SessionResults::build(bar_path, state.calibration, 0));
        assert_eq!(state.current_step(), WorkflowStep::Review);
        state
    }

    #[test]
    fn new_session_resets_seed_calibration_results_and_events_and_returns_to_step_one() {
        let mut state = state_in_review();
        assert!(state.can_start_new_session());
        state.start_new_session();

        assert!(state.seed.is_none());
        assert!(state.calibration.is_none());
        assert!(state.last_calibration_segment.is_none());
        assert!(state.bar_path.is_none());
        assert!(state.results.is_none());
        assert!(state.tracking.is_none());
        assert_eq!(state.mode, Mode::ViewOnly);
        assert_eq!(state.current_step(), WorkflowStep::PlaceSeed);
        // Same video: `video_path`/`metadata` untouched (no app restart).
        assert_eq!(state.video_path, PathBuf::from("x.mp4"));
        // Events were cleared, but the "new session started" event itself
        // was pushed after clearing, so exactly one remains.
        assert_eq!(state.events.len(), 1);
        assert!(state.events.back().unwrap().message.contains("new session"));
    }

    #[test]
    fn new_session_is_unavailable_outside_review() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        assert!(!state.can_start_new_session());
        state.start_new_session();
        assert!(state.events.is_empty());
    }

    #[test]
    fn retrack_preserves_seed_and_calibration_clears_results_and_starts_tracking_immediately() {
        let mut state = state_in_review();
        let seed_before = state.seed;
        let cal_before = state.calibration;
        assert!(state.can_retrack());

        state.retrack();

        assert_eq!(state.seed, seed_before);
        assert_eq!(state.calibration, cal_before);
        assert!(state.bar_path.is_none());
        assert!(state.results.is_none());
        // `start_tracking` was called as part of retrack: a new run is
        // active immediately, no extra click needed.
        assert!(state.tracking.is_some());
        assert!(state.tracking_run.running);
    }

    // -- Task 10.5: open video from UI --------------------------------------

    #[test]
    fn note_video_opened_pushes_an_info_event_naming_the_file() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.note_video_opened(std::path::Path::new("/tmp/new-video.mp4"), &meta(Some(10)));
        let event = state.events.back().unwrap();
        assert_eq!(event.level, EventLevel::Info);
        assert!(event.message.contains("opened"));
        assert!(event.message.contains("new-video.mp4"));
        assert!(event.message.contains("4x4"));
        assert!(event.message.contains("30/1"));
    }

    #[test]
    fn note_error_pushes_an_error_event() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.note_error("failed to open foo.mp4: not a video file");
        let event = state.events.back().unwrap();
        assert_eq!(event.level, EventLevel::Error);
        assert!(event.message.contains("failed to open foo.mp4"));
    }

    /// `TrackerApp::open_video` (in `mod.rs`, needs a real `ffprobe`/decoder
    /// so it isn't unit-tested here) rebuilds `AppState` from scratch via
    /// `AppState::new` on every successful open, including a *second* video
    /// mid-session. This pins the part of that reset that's `AppState`'s to
    /// guarantee: a brand-new `AppState` for a different video carries none
    /// of the previous session's seed/calibration/results/events over.
    #[test]
    fn a_fresh_app_state_for_a_newly_opened_video_carries_no_prior_session_state() {
        let mut previous = state_in_review();
        previous.note_video_opened(std::path::Path::new("first.mp4"), &meta(Some(10)));
        assert!(previous.seed.is_some());
        assert!(previous.results.is_some());

        let reopened = AppState::new(PathBuf::from("second.mp4"), meta(Some(20)));
        assert_eq!(reopened.video_path, PathBuf::from("second.mp4"));
        assert!(reopened.seed.is_none());
        assert!(reopened.calibration.is_none());
        assert!(reopened.results.is_none());
        assert!(reopened.bar_path.is_none());
        assert!(reopened.events.is_empty());
        assert_eq!(reopened.current_step(), WorkflowStep::PlaceSeed);
    }

    // -- Task 10.7: instruction banners --------------------------------------

    #[test]
    fn banner_text_prompts_for_seed_before_one_is_placed() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.toggle_placing_seed();
        assert!(state.banner_text().contains("Click the barbell"));
    }

    #[test]
    fn banner_text_shows_calibration_progress_across_both_clicks() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.toggle_calibrating();
        let before = state.banner_text();
        assert!(before.contains("0 of 2 points placed"));
        assert!(before.contains("0.450"));

        state.place_calibration_point(tracker_core::Point::new(0.0, 0.0));
        let after_first = state.banner_text();
        assert!(after_first.contains("1 of 2 points placed"));

        state.place_calibration_point(tracker_core::Point::new(100.0, 0.0));
        // Third click restarts the pair (see `third_click_restarts_the_pair`).
        let after_second = state.banner_text();
        assert!(after_second.contains("0 of 2 points placed"));
    }

    #[test]
    fn banner_text_shows_tracking_progress_with_frame_and_total() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(120)));
        state.tracking_run = TrackingRunState::started();
        state
            .tracking_run
            .apply(tracking::TrackingMessage::Progress {
                video_frame_index: 42,
                position: tracker_core::Point::new(0.0, 0.0),
                source: tracker_core::Source::Tracked,
                state: tracker_core::SessionState::Tracking,
            });
        let text = state.banner_text();
        assert!(text.contains("42"));
        assert!(text.contains("120"));
        assert!(text.to_lowercase().contains("tracking"));
    }

    #[test]
    fn banner_text_reports_done_once_review_results_are_ready() {
        let state = state_in_review();
        assert!(state.banner_text().contains("Done"));
        assert!(state.banner_text().contains("Exports"));
    }

    #[test]
    fn phase_is_idle_before_a_run_starts() {
        let state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        assert_eq!(state.phase(), Phase::Idle);
    }

    #[test]
    fn phase_is_tracking_path_with_frame_and_total_while_a_run_is_active() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(50)));
        state.tracking_run = TrackingRunState::started();
        state
            .tracking_run
            .apply(tracking::TrackingMessage::Progress {
                video_frame_index: 7,
                position: tracker_core::Point::new(0.0, 0.0),
                source: tracker_core::Source::Tracked,
                state: tracker_core::SessionState::Tracking,
            });
        assert_eq!(
            state.phase(),
            Phase::TrackingPath {
                frame: 7,
                total: 50
            }
        );
    }

    #[test]
    fn phase_is_review_once_results_are_built() {
        let state = state_in_review();
        assert_eq!(state.phase(), Phase::Review);
    }

    #[test]
    fn phase_is_computing_metrics_when_bar_path_exists_without_results_yet() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        let tb = tracker_core::Timebase::new(30, 1).unwrap();
        state.bar_path = Some(tracker_core::BarPath::new(&[], &[], tb, 0));
        assert_eq!(state.phase(), Phase::ComputingMetrics);
    }

    // -- Task 10.8: live rep counter -----------------------------------------

    #[test]
    fn poll_tracking_updates_live_reps_every_30_processed_frames() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(200)));
        state.tracking = Some(dummy_tracking_handle());
        state.tracking_run = TrackingRunState::started();
        assert!(state.live_reps.is_none());

        // Feed a one-rep descent/ascent shape directly through the reducer
        // the way `poll_tracking` would (mirrors `state_with_active_run`'s
        // approach of driving `tracking_run` without a real worker thread).
        for i in 0..=10u64 {
            state
                .tracking_run
                .apply(tracking::TrackingMessage::Progress {
                    video_frame_index: i,
                    position: tracker_core::Point::new(0.0, i as f64 * 10.0),
                    source: tracker_core::Source::Tracked,
                    state: tracker_core::SessionState::Tracking,
                });
        }
        for i in 11..=20u64 {
            state
                .tracking_run
                .apply(tracking::TrackingMessage::Progress {
                    video_frame_index: i,
                    position: tracker_core::Point::new(0.0, (20 - i) as f64 * 10.0),
                    source: tracker_core::Source::Tracked,
                    state: tracker_core::SessionState::Tracking,
                });
        }
        // Not yet a multiple of 30: no recompute triggered by the reducer
        // alone (this test drives the reducer directly rather than through
        // `poll_tracking`'s channel drain, so replicate its throttle call
        // here too).
        if state.tracking_run.should_recompute_live_reps() {
            let tb = tracker_core::Timebase::new(30, 1).unwrap();
            if let Some(count) = state.tracking_run.live_rep_count(tb, state.calibration) {
                state.live_reps = Some(count);
            }
        }
        assert!(
            state.live_reps.is_none(),
            "20 frames processed: not yet a multiple of 30"
        );

        for i in 21..=29u64 {
            state
                .tracking_run
                .apply(tracking::TrackingMessage::Progress {
                    video_frame_index: i,
                    position: tracker_core::Point::new(0.0, 0.0),
                    source: tracker_core::Source::Tracked,
                    state: tracker_core::SessionState::Tracking,
                });
        }
        assert!(state.tracking_run.should_recompute_live_reps());
        let tb = tracker_core::Timebase::new(30, 1).unwrap();
        let count = state
            .tracking_run
            .live_rep_count(tb, state.calibration)
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn live_reps_resets_when_a_new_run_starts() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.live_reps = Some(3);
        state.toggle_placing_seed();
        state.place_seed(tracker_core::Point::new(1.0, 1.0));
        state.start_tracking();
        assert!(state.live_reps.is_none());
    }

    #[test]
    fn retrack_is_unavailable_without_a_seed_or_outside_review() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        assert!(!state.can_retrack());
        state.retrack();
        assert!(state.tracking.is_none());
    }
}
