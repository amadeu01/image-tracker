//! Pure(ish) app state (task 2.3, split out in 7.2): current frame index,
//! mode, Seed/Calibration, tracking run reducer, and the guide/status/events
//! data the side panel (7.2) renders. No egui `Context` dependency, so all of
//! this is unit-testable directly.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Instant;

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
    /// The completed `BarPath`, once a tracking run reaches clean
    /// end-of-video. Consumed by milestone 3 (overlay render / export).
    pub bar_path: Option<tracker_core::BarPath>,
    /// The tracker `suggest_tracker` recommends for the current Seed (task
    /// 4.3), computed as soon as the Seed is placed so the status bar can
    /// tell the user which tracker Track will use before they click it.
    /// `None` until a Seed has been placed and a frame is available to
    /// evaluate (`TrackerApp::ensure_texture`/click handler sets this via
    /// `note_seed_suggestion`, since `AppState` alone has no frame access).
    pub suggested_tracker: Option<tracker_core::TrackerKind>,
    /// Recent app events (task 7.2), newest last, capped at `MAX_EVENTS`.
    /// Fed from the same call sites that already emit `tracing` breadcrumbs,
    /// so the side panel gives on-screen visibility into the same history
    /// the log file has.
    pub events: VecDeque<AppEvent>,
    /// When this `AppState` was created; used to timestamp `events`.
    start_time: Instant,
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
            bar_path: None,
            suggested_tracker: None,
            events: VecDeque::new(),
            start_time: Instant::now(),
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
        tracing::info!(
            video = %self.video_path.display(),
            seed_frame = seed.frame_index,
            x = seed.position.x,
            y = seed.position.y,
            "track started"
        );
        self.push_event(
            EventLevel::Info,
            format!("track started @ frame {}", seed.frame_index),
        );
        let handle = tracking::spawn_tracking(tracking::TrackingJob {
            video_path: self.video_path.clone(),
            width: self.metadata.display_width(),
            height: self.metadata.display_height(),
            fps_num: self.metadata.fps_num,
            fps_den: self.metadata.fps_den,
            seed_frame_index: seed.frame_index,
            seed_position: seed.position,
            tracker_config: tracking::default_tracker_config(),
            session_config: tracking::default_session_config(),
            tracker_selection: tracking::TrackerSelection::Auto,
            color_tracker_config: tracking::default_color_tracker_config(),
        });
        self.tracking = Some(handle);
        self.tracking_run = TrackingRunState::started();
        self.bar_path = None;
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
            }
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
}
