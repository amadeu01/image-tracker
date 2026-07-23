//! Results-review concern (task 20.1 split out of the former flat
//! `state.rs`): `SessionResults`/`ResultsQuality` (derived once from a
//! finished run), the rep-selection/clip-playback/whole-path-toggle
//! `AppState` methods (`select_rep`/`toggle_rep_clip`/`advance_rep_clip`/
//! `show_path`), the Live/Results toolbar pill (`DisplayMode`), and per-rep
//! clip export.

use super::{AppState, EventLevel};
use crate::export_job;
use crate::tracking;

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

    /// Video-absolute `(start_frame, end_frame)` of rep `index` (task 13.2's
    /// scrub-bar segments). `Rep`'s fields are *indices into the velocity
    /// slice*, not frame numbers — this resolves them through the velocity
    /// samples' `frame_index` (which already carries the session's
    /// `start_frame` offset). `None` when the index is out of range or the
    /// velocity series errored (no reps exist then anyway).
    pub fn rep_frame_bounds(&self, index: usize) -> Option<(u64, u64)> {
        let velocity = self.velocity.as_ref().ok()?;
        let rep = self.reps.get(index)?;
        let start = velocity.get(rep.eccentric_start)?.frame_index;
        let end = velocity.get(rep.concentric_end)?.frame_index;
        Some((start, end))
    }

    /// The bar-path points to draw over the video (task 19.1): by default,
    /// only `selected_rep`'s frame segment (per `rep_frame_bounds`), so
    /// selecting a rep in the table/graph shows just that rep's line
    /// instead of the whole-set polyline overlapping every rep into an
    /// unreadable scribble (the user finding that motivated this task).
    /// `show_path` (15.2's transport-row toggle) is repurposed as the
    /// opt-in "whole set" view: when true, the full polyline is returned
    /// regardless of selection. With no rep selected and `show_path` off,
    /// the full path is still returned — a "no reps yet"/"nothing picked"
    /// state must show *something* rather than a blank overlay, and that
    /// matches the pre-19.1 behavior in the no-selection case.
    /// `bar_path.points()` is sorted by ascending `frame_index` (one entry
    /// per tracked/interpolated video frame), so the segment is a
    /// contiguous sub-slice found via `partition_point` — no allocation.
    pub fn path_points_to_draw(
        &self,
        selected_rep: Option<usize>,
        show_path: bool,
    ) -> &[tracker_core::PathPoint] {
        let points = self.bar_path.points();
        if show_path {
            return points;
        }
        let Some(index) = selected_rep else {
            return points;
        };
        let Some((start, end)) = self.rep_frame_bounds(index) else {
            return points;
        };
        let lo = points.partition_point(|p| p.frame_index < start);
        let hi = points.partition_point(|p| p.frame_index <= end);
        &points[lo..hi]
    }
}

/// Next playhead frame while a rep clip is looping (task 13.3): step forward
/// one frame; from the clip's end (or anywhere outside its bounds — e.g. the
/// user scrubbed away mid-loop) wrap back to `start`. Pure, so the loop's
/// wraparound is unit-testable without egui.
pub fn clip_loop_next_frame(current: u64, start: u64, end: u64) -> u64 {
    if current < start || current >= end {
        start
    } else {
        current + 1
    }
}

/// `M:SS.s` timestamp of `frame` under the `fps_num/fps_den` timebase (the
/// rep table's TIME column, task 13.3 — the mock's `fmtTime`: minutes,
/// zero-padded seconds with one decimal). Degenerate timebases render as
/// `0:00.0` rather than dividing by zero.
pub fn format_clip_time(frame: u64, fps_num: u64, fps_den: u64) -> String {
    let seconds = if fps_num == 0 || fps_den == 0 {
        0.0
    } else {
        frame as f64 * fps_den as f64 / fps_num as f64
    };
    let minutes = (seconds / 60.0).floor();
    let rest = seconds - minutes * 60.0;
    format!(
        "{}:{}{rest:.1}",
        minutes as u64,
        if rest < 10.0 { "0" } else { "" }
    )
}

impl AppState {
    /// Selects rep `index` and jumps the playhead to its start frame (the
    /// design's segment/row `onSelect`: select + jump + clear any active
    /// clip). No-op when `index` doesn't resolve to a rep in the current
    /// `results` — a stale click racing a reset must not select garbage.
    pub fn select_rep(&mut self, index: usize) {
        let Some(bounds) = self
            .results
            .as_ref()
            .and_then(|r| r.rep_frame_bounds(index))
        else {
            return;
        };
        self.selected_rep = Some(index);
        self.rep_clip = None;
        self.set_frame(bounds.0 as i64);
    }

    /// Arms/disarms rep `index`'s clip loop (the rep table's ▶ button, task
    /// 13.3 — the design's `onPlay`): arming also selects the rep and jumps
    /// the playhead to its start (mirroring the mock's `setState`); clicking
    /// ▶ on the already-armed rep toggles the loop off, leaving the
    /// selection and playhead where they are. No-op when `index` doesn't
    /// resolve to a rep in the current `results` (same stale-click guard as
    /// `select_rep`).
    pub fn toggle_rep_clip(&mut self, index: usize) {
        if self.rep_clip == Some(index) {
            self.rep_clip = None;
            return;
        }
        let Some(bounds) = self
            .results
            .as_ref()
            .and_then(|r| r.rep_frame_bounds(index))
        else {
            return;
        };
        self.selected_rep = Some(index);
        self.rep_clip = Some(index);
        self.set_frame(bounds.0 as i64);
    }

    /// Advances the playhead one frame within the armed rep clip's bounds,
    /// wrapping `end → start` (task 13.3's in-app loop: playhead cycling via
    /// the existing seek decoder, a deliberate v1 deviation from true
    /// decoded playback per the design notes). Returns `true` when a clip
    /// is armed and the frame moved — the caller (`TrackerApp::update`)
    /// schedules the next repaint one video-frame-duration later, so the
    /// loop runs at roughly video fps. `false` (no-op) when no clip is
    /// armed or its bounds can't be resolved.
    pub fn advance_rep_clip(&mut self) -> bool {
        let Some(bounds) = self
            .rep_clip
            .and_then(|i| self.results.as_ref()?.rep_frame_bounds(i))
        else {
            return false;
        };
        let next = clip_loop_next_frame(self.current_frame, bounds.0, bounds.1);
        self.set_frame(next as i64);
        true
    }

    /// Whether "Export all rep clips" (task 13.3) is currently available:
    /// results with at least one rep, and no export job (auto-export or a
    /// previous clip export) still running — both share `self.export`, so
    /// one at a time.
    pub fn can_export_rep_clips(&self) -> bool {
        self.export.is_none()
            && self
                .results
                .as_ref()
                .is_some_and(|r| !r.reps.is_empty() && r.velocity.is_ok())
    }

    /// Spawns the background per-rep clip export (`<stem>.repNN.mp4` via
    /// ffmpeg stream copy, task 13.3). Reuses the auto-export message
    /// channel/polling (`poll_export`), so per-file done/failed surface as
    /// the same events and `exported_files` rows the other exports get —
    /// without clearing the files the auto-export already wrote. No-op if
    /// `can_export_rep_clips` is false.
    pub fn start_rep_clip_export(&mut self) {
        if !self.can_export_rep_clips() {
            return;
        }
        let Some(results) = &self.results else {
            return;
        };
        let bounds: Vec<(u64, u64)> = (0..results.reps.len())
            .filter_map(|i| results.rep_frame_bounds(i))
            .collect();
        if bounds.is_empty() {
            return;
        }
        let clip_count = bounds.len();
        tracing::info!(clips = clip_count, "rep clip export started");
        self.push_event(
            EventLevel::Info,
            format!("rep clip export started ({clip_count} clips)"),
        );
        self.export = Some(export_job::spawn_rep_clip_export(export_job::RepClipJob {
            video_path: self.video_path.clone(),
            fps_num: self.metadata.fps_num,
            fps_den: self.metadata.fps_den,
            bounds,
        }));
    }

    /// Sets the Live/Results pill selection (task 13.1's toolbar toggle).
    pub fn set_display_mode(&mut self, mode: DisplayMode) {
        self.display_mode = mode;
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::test_support::{meta, one_rep_bar_path, state_in_review, two_rep_bar_path};
    use super::*;
    use crate::app::state::AppState;

    #[test]
    fn show_path_defaults_to_false_ie_per_rep_view_by_default() {
        // 19.1: the whole-set polyline is opt-in; default is the per-rep
        // segment view.
        let state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        assert!(
            !state.show_path,
            "whole-set path overlay must be opt-in, not on by default"
        );
    }

    #[test]
    fn show_path_toggles_off_and_back_on() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.show_path = !state.show_path;
        assert!(state.show_path);
        state.show_path = !state.show_path;
        assert!(!state.show_path);
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
        let samples = vec![tracker_core::Sample {
            frame_index: 0,
            position: tracker_core::Point::new(0.0, 0.0),
            source: tracker_core::Source::Tracked,
            confidence: None,
        }];
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
    fn rep_frame_bounds_resolves_velocity_indices_to_video_frames() {
        let results = SessionResults::build(one_rep_bar_path(), None, 0);
        let (start, end) = results.rep_frame_bounds(0).expect("one rep exists");
        // The synthetic rep spans (nearly) the whole 0..=20 path; whatever
        // exact indices segment_reps picks, the bounds must be ordered,
        // within the path's frame range, and genuinely apart.
        assert!(start < end);
        assert!(end <= 20);
        assert_eq!(results.rep_frame_bounds(1), None, "only one rep");
    }

    #[test]
    fn rep_frame_bounds_is_none_when_velocity_errored() {
        let tb = tracker_core::Timebase::new(30, 1).unwrap();
        let samples = vec![tracker_core::Sample {
            frame_index: 0,
            position: tracker_core::Point::new(0.0, 0.0),
            source: tracker_core::Source::Tracked,
            confidence: None,
        }];
        let results =
            SessionResults::build(tracker_core::BarPath::new(&samples, &[], tb, 0), None, 0);
        assert_eq!(results.rep_frame_bounds(0), None);
    }

    #[test]
    fn path_points_to_draw_show_path_returns_the_whole_polyline() {
        let results = SessionResults::build(two_rep_bar_path(), None, 0);
        assert_eq!(results.reps.len(), 2, "fixture must yield two reps");
        let all = results.bar_path.points();
        assert_eq!(results.path_points_to_draw(Some(0), true), all);
        assert_eq!(results.path_points_to_draw(None, true), all);
    }

    #[test]
    fn path_points_to_draw_filters_to_the_selected_reps_frames() {
        let results = SessionResults::build(two_rep_bar_path(), None, 0);
        assert_eq!(results.reps.len(), 2, "fixture must yield two reps");
        let (start, end) = results.rep_frame_bounds(1).expect("rep 1 exists");

        let segment = results.path_points_to_draw(Some(1), false);
        assert!(!segment.is_empty());
        assert!(segment
            .iter()
            .all(|p| p.frame_index >= start && p.frame_index <= end));
        assert_eq!(segment.first().unwrap().frame_index, start);
        assert_eq!(segment.last().unwrap().frame_index, end);

        // Rep 0's segment is disjoint from rep 1's.
        let rep0 = results.path_points_to_draw(Some(0), false);
        assert!(rep0.iter().all(|p| p.frame_index < start));
    }

    #[test]
    fn path_points_to_draw_with_no_selection_falls_back_to_the_whole_path() {
        let results = SessionResults::build(two_rep_bar_path(), None, 0);
        let all = results.bar_path.points();
        assert_eq!(results.path_points_to_draw(None, false), all);
    }

    #[test]
    fn select_rep_sets_selection_jumps_playhead_to_rep_start_and_clears_clip() {
        let mut state = state_in_review();
        state.rep_clip = Some(0);
        state.set_frame(20);
        state.select_rep(0);
        assert_eq!(state.selected_rep, Some(0));
        assert_eq!(state.rep_clip, None);
        let (start, _) = state.results.as_ref().unwrap().rep_frame_bounds(0).unwrap();
        assert_eq!(state.current_frame, start);
    }

    #[test]
    fn select_rep_ignores_out_of_range_index_and_missing_results() {
        let mut state = state_in_review();
        state.select_rep(5); // only one rep exists
        assert_eq!(state.selected_rep, None);

        let mut fresh = AppState::new(PathBuf::from("v.mp4"), meta(Some(100)));
        fresh.select_rep(0); // no results at all
        assert_eq!(fresh.selected_rep, None);
    }

    #[test]
    fn rep_selection_is_cleared_on_new_session_retrack_and_discard() {
        let mut state = state_in_review();
        state.select_rep(0);
        assert_eq!(state.selected_rep, Some(0));
        state.start_new_session();
        assert_eq!(state.selected_rep, None);
        assert_eq!(state.rep_clip, None);

        let mut state = state_in_review();
        state.select_rep(0);
        state.rep_clip = Some(0);
        state.retrack();
        assert_eq!(state.selected_rep, None);
        assert_eq!(state.rep_clip, None);
    }

    #[test]
    fn toggle_rep_clip_arms_selects_and_jumps_then_disarms_on_second_click() {
        let mut state = state_in_review();
        state.set_frame(20);
        state.toggle_rep_clip(0);
        assert_eq!(state.rep_clip, Some(0));
        assert_eq!(state.selected_rep, Some(0));
        let (start, _) = state.results.as_ref().unwrap().rep_frame_bounds(0).unwrap();
        assert_eq!(state.current_frame, start);

        state.set_frame(start as i64 + 2);
        state.toggle_rep_clip(0);
        assert_eq!(state.rep_clip, None, "second ▶ click stops the loop");
        assert_eq!(state.selected_rep, Some(0), "selection stays");
        assert_eq!(state.current_frame, start + 2, "playhead stays put");
    }

    #[test]
    fn toggle_rep_clip_ignores_out_of_range_index_and_missing_results() {
        let mut state = state_in_review();
        state.toggle_rep_clip(5); // only one rep exists
        assert_eq!(state.rep_clip, None);

        let mut fresh = AppState::new(PathBuf::from("v.mp4"), meta(Some(100)));
        fresh.toggle_rep_clip(0);
        assert_eq!(fresh.rep_clip, None);
    }

    #[test]
    fn clip_loop_next_frame_steps_and_wraps_at_the_end() {
        assert_eq!(clip_loop_next_frame(5, 5, 10), 6);
        assert_eq!(clip_loop_next_frame(9, 5, 10), 10);
        assert_eq!(clip_loop_next_frame(10, 5, 10), 5, "end wraps to start");
        assert_eq!(clip_loop_next_frame(2, 5, 10), 5, "before start snaps in");
        assert_eq!(clip_loop_next_frame(50, 5, 10), 5, "past end snaps in");
    }

    #[test]
    fn advance_rep_clip_cycles_the_playhead_only_while_a_clip_is_armed() {
        let mut state = state_in_review();
        assert!(!state.advance_rep_clip(), "no clip armed");

        state.toggle_rep_clip(0);
        let (start, end) = state.results.as_ref().unwrap().rep_frame_bounds(0).unwrap();
        assert_eq!(state.current_frame, start);
        assert!(state.advance_rep_clip());
        assert_eq!(state.current_frame, start + 1);
        state.set_frame(end as i64);
        assert!(state.advance_rep_clip());
        assert_eq!(state.current_frame, start, "wraps end -> start");
    }

    #[test]
    fn format_clip_time_matches_the_mock_m_ss_s_format() {
        assert_eq!(format_clip_time(0, 30, 1), "0:00.0");
        assert_eq!(format_clip_time(190, 30, 1), "0:06.3");
        assert_eq!(format_clip_time(1854, 30, 1), "1:01.8");
        assert_eq!(format_clip_time(600, 600, 19), "0:19.0");
        assert_eq!(format_clip_time(100, 0, 1), "0:00.0", "degenerate fps");
    }

    #[test]
    fn rep_clip_export_gate_requires_reps_and_no_active_export() {
        let mut state = state_in_review();
        assert!(state.can_export_rep_clips());
        state.start_rep_clip_export();
        assert!(state.export.is_some(), "clip export job spawned");
        assert!(
            !state.can_export_rep_clips(),
            "one export at a time (shared handle)"
        );

        let fresh = AppState::new(PathBuf::from("v.mp4"), meta(Some(100)));
        assert!(!fresh.can_export_rep_clips(), "no results yet");
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
}
