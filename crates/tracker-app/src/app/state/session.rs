//! Mode/Seed/Calibration/frame-position concern (task 20.1 split out of the
//! former flat `state.rs`): what clicking the frame view currently does
//! (`Mode`), the workflow-step ladder the guide highlights (`WorkflowStep`),
//! a placed Seed, and the `AppState` methods that mutate any of those —
//! `toggle_placing_seed`/`toggle_calibrating`/`place_seed`/
//! `place_calibration_point`/`set_calibration_length`/`set_frame`/
//! `next_frame`/`prev_frame`. `Phase` (the live-run progress ladder) stays in
//! `mod.rs` since it's derived from `jobs.rs`'s tracking state, not owned by
//! this concern.

use super::{AppState, EventLevel};
use crate::frame_cache::clamp_frame_index;

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

impl AppState {
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
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::test_support::meta;
    use super::*;
    use crate::app::state::EventLevel;

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
}
