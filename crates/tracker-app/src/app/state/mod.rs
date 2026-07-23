//! Pure(ish) app state (task 2.3, split out in 7.2; further split by concern
//! in 20.1): current frame index, mode, Seed/Calibration, tracking run
//! reducer, and the guide/status/events data the side panel (7.2) renders.
//! No egui `Context` dependency, so all of this is unit-testable directly.
//!
//! Task 20.1 split this file (2708 lines, `AppState` 27 public fields /
//! ~48 methods) into a directory by *concern*, not by sub-struct: `AppState`
//! stays one flat struct here so every existing call site
//! (`state.tracking`, `state.results`, `state.seed`, …) keeps compiling
//! unchanged — only the `impl AppState` blocks that touch each area moved
//! into their own file, plus the standalone types those areas own. See each
//! submodule's doc comment for exactly what it holds; this file keeps the
//! struct itself, construction, and the cross-cutting status/banner/session
//! plumbing that legitimately touches every area at once (`current_step`,
//! `phase`, `banner_text`, `status_line`, `note_*`, New Session, Re-track).
//!
//! Every public name the old flat `state.rs` exported is re-exported here,
//! so `super::state::X` paths elsewhere in `app/` (and the `pub use` from
//! `app/mod.rs`) don't need to change.

mod jobs;
mod review;
mod session;
#[cfg(test)]
mod test_support;

// `ResultsQuality`/`clip_loop_next_frame` aren't currently read from outside
// `state::review` (nothing else needs the un-derived quality struct or the
// clip-loop stepper directly), but both were public at `state::`'s top
// level before this split — re-exported here for path stability even though
// that makes them currently-unused imports from this binary crate's PoV.
#[allow(unused_imports)]
pub use review::{
    clip_loop_next_frame, format_clip_time, DisplayMode, ResultsQuality, SessionResults,
};
pub use session::{Mode, Seed, WorkflowStep, DEFAULT_CALIBRATION_LENGTH_METERS};

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Instant;

use crate::export_job::ExportHandle;
use crate::ffprobe::VideoMetadata;
use crate::tracking::{self, TrackingHandle, TrackingRunState};

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

/// Valid range for `TrackingSettings::patch_radius` (task 15.3): shared by
/// the Advanced DragValue in `settings_section.rs` and the scroll-to-resize
/// gesture on the video panel, so both writers clamp identically.
pub const PATCH_RADIUS_RANGE: std::ops::RangeInclusive<u32> = 4..=64;

/// The patch radius a run started right now would actually use (task 15.3):
/// the settings value clamped into [`PATCH_RADIUS_RANGE`]. Single source of
/// truth for the drawn seed-region rectangle, so the overlay always matches
/// the square the Template tracker will cut (side `2*r + 1` source pixels).
pub fn effective_patch_radius(settings: &TrackingSettings) -> u32 {
    settings
        .patch_radius
        .clamp(*PATCH_RADIUS_RANGE.start(), *PATCH_RADIUS_RANGE.end())
}

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
            anchor_floor: None,
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
            tracking::TrackerSelection::Circle => "circle",
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

/// UI/session state, independent of egui so the index-clamping logic can be
/// unit-tested without a `Context`. Fields group into the seven concerns
/// task 20.1's audit named; the grouping is now just doc-comment sections
/// (see `session.rs`/`jobs.rs`/`review.rs` for the methods that touch each
/// group) rather than separate structs, so every field stays directly on
/// `AppState` and no call site changed.
pub struct AppState {
    pub video_path: PathBuf,
    pub metadata: VideoMetadata,
    // -- session.rs: mode / seed / calibration / frame position ---------
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
    // -- jobs.rs: tracking / export / benchmark background workers ------
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
    // -- review.rs: results display/selection ----------------------------
    /// The toolbar's Live/Results pill selection (task 13.1). Defaults to
    /// `Results` — a freshly opened video has no live run active yet, and
    /// `Results` is also where the "no results yet" empty state already
    /// lives, so there's nothing to switch away from until tracking starts.
    /// `start_tracking` flips it to `Live`; nothing currently flips it back
    /// automatically (left for 13.6, which owns the dedicated Live panel).
    pub display_mode: DisplayMode,
    /// The selected rep (0-based index into `results.reps`), task 13.2 —
    /// shared selection state: the scrub bar's segment highlight now, the
    /// 13.3 table row highlight and video path segment highlight later, all
    /// read this same field. `None` when nothing is selected; cleared with
    /// `results` on retrack/new session/discard (a stale index into a new
    /// run's reps would silently select the wrong rep).
    pub selected_rep: Option<usize>,
    /// The rep whose *clip* is active (13.3's ▶ per-rep playback) — drives
    /// the scrub bar's in/out markers, which 13.2 already draws. Distinct
    /// from `selected_rep` per the design mock: clicking a segment/row
    /// selects (and clears any clip); only ▶ arms a clip. Nothing sets this
    /// yet — 13.3 owns that.
    pub rep_clip: Option<usize>,
    /// Whether the bar-path overlay draws the *whole-set* polyline instead
    /// of just `selected_rep`'s segment (task 15.2, repurposed by 19.1).
    /// Defaults to `false` — 19.1's per-rep-by-default view, since the
    /// whole-set polyline over several reps overlaps into an unreadable
    /// scribble (the motivating user finding); flipping this on is the
    /// opt-in whole-set view. Toggled from the transport row in
    /// `bottom_bar.rs` and persisted via `theme::save_show_path` /
    /// restored in `TrackerApp::load_video` (same seam as
    /// `stop_threshold_pct`, keeping `AppState::new` IO-free). The *live*
    /// tracking crosshair and the Seed marker deliberately ignore this —
    /// lock-on feedback stays visible regardless.
    pub show_path: bool,
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
            selected_rep: None,
            rep_clip: None,
            show_path: false,
        }
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
                // Last valid 0-based frame index, not the raw frame count:
                // must match the bottom status bar's "frame N/M" (which
                // shows `frame_count - 1`), or the two disagree by one for
                // the whole run (task 15.1: "541/3778" vs "541/3777").
                total: self.metadata.frame_count.unwrap_or(0).saturating_sub(1),
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
        self.selected_rep = None;
        self.rep_clip = None;
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
        self.selected_rep = None;
        self.rep_clip = None;
        self.tracking_run = TrackingRunState::default();
        self.paused = false;
        tracing::info!("re-track started");
        self.push_event(EventLevel::Info, "re-track started".to_string());
        self.start_tracking();
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::meta;
    use super::*;

    #[test]
    fn effective_patch_radius_uses_settings_value() {
        let mut settings = TrackingSettings::default();
        assert_eq!(
            effective_patch_radius(&settings),
            crate::tracking::DEFAULT_PATCH_RADIUS
        );
        settings.patch_radius = 20;
        assert_eq!(effective_patch_radius(&settings), 20);
    }

    #[test]
    fn effective_patch_radius_clamps_into_valid_range() {
        let mut settings = TrackingSettings {
            patch_radius: 0,
            ..Default::default()
        };
        assert_eq!(
            effective_patch_radius(&settings),
            *PATCH_RADIUS_RANGE.start()
        );
        settings.patch_radius = 9999;
        assert_eq!(effective_patch_radius(&settings), *PATCH_RADIUS_RANGE.end());
    }

    #[test]
    fn patch_radius_range_covers_the_default() {
        assert!(PATCH_RADIUS_RANGE.contains(&crate::tracking::DEFAULT_PATCH_RADIUS));
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

    #[test]
    fn new_session_resets_seed_calibration_results_and_events_and_returns_to_step_one() {
        let mut state = test_support::state_in_review();
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
        let mut state = test_support::state_in_review();
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

    #[test]
    fn retrack_is_unavailable_without_a_seed_or_outside_review() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        assert!(!state.can_retrack());
        state.retrack();
        assert!(state.tracking.is_none());
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
        let mut previous = test_support::state_in_review();
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
        // Total is the last valid 0-based frame index (frame_count - 1),
        // matching the bottom status bar's "frame N/M" (task 15.1: the
        // banner used to show the raw frame count, one higher).
        assert!(text.contains("119"));
        assert!(!text.contains("120"));
        assert!(text.to_lowercase().contains("tracking"));
    }

    #[test]
    fn banner_text_reports_done_once_review_results_are_ready() {
        let state = test_support::state_in_review();
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
                total: 49 // last valid 0-based index, not the raw count
            }
        );
    }

    #[test]
    fn phase_is_review_once_results_are_built() {
        let state = test_support::state_in_review();
        assert_eq!(state.phase(), Phase::Review);
    }

    #[test]
    fn phase_is_computing_metrics_when_bar_path_exists_without_results_yet() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        let tb = tracker_core::Timebase::new(30, 1).unwrap();
        state.bar_path = Some(tracker_core::BarPath::new(&[], &[], tb, 0));
        assert_eq!(state.phase(), Phase::ComputingMetrics);
    }
}
