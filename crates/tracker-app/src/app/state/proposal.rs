//! Automatic seed proposal concern: the background pre-pass that searches an
//! unseeded clip for the bar, the proposal it may return, and the user's
//! confirm / reject / correct choices.
//!
//! The rules (docs/superpowers/specs/2026-10-28-auto-lift-detection-design.md):
//! - A proposal is **not** a Seed. Tracking only ever starts from a Seed, and
//!   a Seed only comes from the user: `confirm_proposal` (one click) or a
//!   manual click (`place_seed`, the "correct" path). An unconfirmed run is
//!   never started.
//! - A confirmed proposal carries its lift type into rep segmentation; a
//!   manual seed carries `LiftType::Unknown` (today's eccentric-first
//!   behaviour), as does a proposal whose lift was not recognised.
//! - One job at a time: while the pre-pass runs (`Job::FindingBar`) tracking
//!   and the strategy benchmark are *refused* (`can_start_tracking` /
//!   `can_test_strategies` are false). A manual seed cancels the pre-pass
//!   first, so the refusal only bites if a seed exists by other means.
//! - Opening another video drops this `AppState`, and with it the job handle,
//!   which cancels the worker.

use super::{AppState, EventLevel, Job, Mode, Seed};
use crate::prepass_job::{self, PrepassHandle, PrepassMessage};
use tracker_core::{LiftType, SeedProposal};

/// Banner text when the pre-pass produced nothing usable.
pub const NO_BAR_FOUND_MESSAGE: &str =
    "Could not find the bar automatically \u{2014} click on the bar end to seed manually";

/// Human label for a lift, as shown next to a proposal.
pub fn lift_label(lift: LiftType) -> &'static str {
    match lift {
        LiftType::Squat => "Squat",
        LiftType::Deadlift => "Deadlift",
        LiftType::Bench => "Bench",
        LiftType::Unknown => "Lift type not recognised",
    }
}

impl AppState {
    /// The pending proposal, if the pre-pass found a bar and the user has not
    /// yet confirmed or rejected it.
    pub fn proposal(&self) -> Option<&SeedProposal> {
        self.proposal.as_ref()
    }

    /// The lift confirmed with the current Seed (`Unknown` for a manual seed).
    pub fn confirmed_lift(&self) -> LiftType {
        self.confirmed_lift
    }

    /// Why there is no proposal (nothing found / failed), if so.
    pub fn prepass_notice(&self) -> Option<&str> {
        self.prepass_notice.as_deref()
    }

    /// Whether the pre-pass is currently searching the clip.
    pub fn is_finding_bar(&self) -> bool {
        matches!(self.job, Job::FindingBar { .. })
    }

    /// Pre-pass progress as a whole percent, `Some` only while it runs
    /// (`0` before the first progress message).
    pub fn prepass_percent(&self) -> Option<u64> {
        match &self.job {
            Job::FindingBar { progress, .. } => Some(
                progress
                    .and_then(|(done, total)| (done * 100).checked_div(total))
                    .unwrap_or(0)
                    .min(100),
            ),
            _ => None,
        }
    }

    /// "Proposed bar . Squat . confidence 87%" for the pending proposal; an
    /// unrecognised lift reads "Lift type not recognised".
    pub fn proposal_headline(&self) -> Option<String> {
        let p = self.proposal.as_ref()?;
        Some(format!(
            "Proposed bar \u{b7} {} \u{b7} confidence {:.0}%",
            lift_label(p.lift.lift),
            p.confidence * 100.0
        ))
    }

    /// Banner text for the pre-pass states, or `None` when it has nothing to
    /// say (the caller falls through to the mode text).
    pub(super) fn prepass_banner_text(&self) -> Option<String> {
        if let Some(percent) = self.prepass_percent() {
            return Some(format!("Finding the bar\u{2026} {percent}%"));
        }
        if let Some(headline) = self.proposal_headline() {
            let p = self.proposal.as_ref()?;
            let ask = if p.lift.lift == LiftType::Unknown {
                " Confirm only if this is the bar."
            } else {
                ""
            };
            return Some(format!("{headline} \u{2014} {}.{ask}", p.lift.reason));
        }
        if self.seed.is_none() {
            if let Some(notice) = &self.prepass_notice {
                return Some(notice.clone());
            }
        }
        None
    }

    /// Whether a background pre-pass may start: no Seed, no pending proposal,
    /// no other job.
    pub fn can_start_prepass(&self) -> bool {
        self.seed.is_none() && self.proposal.is_none() && matches!(self.job, Job::Idle)
    }

    /// Starts the background pre-pass over the whole clip. No-op unless
    /// `can_start_prepass`. A clip of unknown length cannot be sampled, so it
    /// falls straight back to manual seeding.
    pub fn start_prepass(&mut self) {
        if !self.can_start_prepass() {
            return;
        }
        let Some(frame_count) = self.metadata.frame_count else {
            self.fall_back_to_manual("frame count unknown");
            return;
        };
        tracing::info!(video = %self.video_path.display(), frame_count, "pre-pass started");
        self.push_event(
            EventLevel::Info,
            "finding the bar automatically".to_string(),
        );
        let handle = prepass_job::spawn_prepass(prepass_job::PrepassJob {
            video_path: self.video_path.clone(),
            width: self.metadata.display_width(),
            height: self.metadata.display_height(),
            fps_num: self.metadata.fps_num,
            fps_den: self.metadata.fps_den,
            frame_count,
        });
        self.attach_prepass(handle);
    }

    /// Makes `handle` the running job. Split from `start_prepass` so tests can
    /// drive the job with a hand-built channel instead of ffmpeg.
    fn attach_prepass(&mut self, handle: PrepassHandle) {
        self.prepass_notice = None;
        self.job = Job::FindingBar {
            handle,
            progress: None,
        };
    }

    /// Stops a running pre-pass and returns to manual seeding (the "Cancel"
    /// button). No-op when none is running.
    pub fn cancel_prepass(&mut self) {
        if !self.is_finding_bar() {
            return;
        }
        // Replacing the job drops the handle, which sets the cancel flag.
        self.job = Job::Idle;
        self.prepass_notice = None;
        tracing::info!("pre-pass cancelled");
        self.push_event(EventLevel::Info, "automatic search cancelled".to_string());
    }

    /// Drains the pre-pass worker. Returns `true` if anything was processed
    /// (the caller should request a repaint).
    pub fn poll_prepass(&mut self) -> bool {
        let Job::FindingBar { handle, .. } = &self.job else {
            return false;
        };
        let messages = handle.drain();
        let mut any = false;
        for msg in messages {
            any = true;
            match msg {
                PrepassMessage::Progress { done, total } => {
                    if let Job::FindingBar { progress, .. } = &mut self.job {
                        *progress = Some((done, total));
                    }
                }
                PrepassMessage::Done(result) => {
                    self.job = Job::Idle;
                    self.apply_prepass_result(result);
                    break;
                }
            }
        }
        any
    }

    fn apply_prepass_result(&mut self, result: Result<Option<SeedProposal>, String>) {
        match result {
            Ok(Some(p)) => {
                tracing::info!(
                    frame = p.frame_index,
                    x = p.position.x,
                    y = p.position.y,
                    lift = ?p.lift.lift,
                    confidence = p.confidence,
                    "bar proposed"
                );
                self.push_event(
                    EventLevel::Info,
                    format!(
                        "bar proposed @ frame {} ({})",
                        p.frame_index,
                        lift_label(p.lift.lift)
                    ),
                );
                self.set_frame(p.frame_index as i64);
                self.proposal = Some(p);
            }
            Ok(None) => self.fall_back_to_manual("no moving plate found"),
            Err(e) => self.fall_back_to_manual(&e),
        }
    }

    /// No usable proposal: tell the user, arm click-to-seed, start nothing.
    fn fall_back_to_manual(&mut self, why: &str) {
        tracing::warn!(why, "no automatic bar proposal");
        self.push_event(
            EventLevel::Warn,
            format!("automatic bar search found nothing: {why}"),
        );
        self.prepass_notice = Some(NO_BAR_FOUND_MESSAGE.to_string());
        // Arm click-to-seed only from the idle mode: an in-progress
        // calibration must not be silently abandoned.
        if self.mode == Mode::ViewOnly {
            self.mode = Mode::PlacingSeed;
        }
    }

    /// Confirms the pending proposal: it becomes the Seed (frame and
    /// position) and its lift is kept for rep segmentation. Returns the new
    /// Seed so the caller can request the tracker suggestion for its frame.
    pub fn confirm_proposal(&mut self) -> Option<Seed> {
        let p = self.proposal.take()?;
        let seed = Seed {
            position: p.position,
            frame_index: p.frame_index,
        };
        self.seed = Some(seed);
        self.confirmed_lift = p.lift.lift;
        self.prepass_notice = None;
        self.mode = Mode::ViewOnly;
        self.set_frame(p.frame_index as i64);
        tracing::info!(
            frame = p.frame_index,
            lift = ?p.lift.lift,
            "bar proposal confirmed"
        );
        self.push_event(
            EventLevel::Info,
            format!(
                "bar confirmed @ frame {} ({})",
                p.frame_index,
                lift_label(p.lift.lift)
            ),
        );
        Some(seed)
    }

    /// Discards the pending proposal ("Not the bar") and returns to manual
    /// seeding. No Seed is set.
    pub fn reject_proposal(&mut self) {
        if self.proposal.take().is_none() {
            return;
        }
        self.confirmed_lift = LiftType::Unknown;
        self.mode = Mode::PlacingSeed;
        tracing::info!("bar proposal rejected");
        self.push_event(EventLevel::Info, "bar proposal rejected".to_string());
    }

    /// Drops everything automatic: cancels a running pre-pass, forgets any
    /// proposal and notice, and resets the confirmed lift to `Unknown`.
    pub(super) fn clear_seed_proposal_state(&mut self) {
        if self.is_finding_bar() {
            self.job = Job::Idle;
        }
        self.proposal = None;
        self.prepass_notice = None;
        self.confirmed_lift = LiftType::Unknown;
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};

    use super::super::test_support::{deadlift_bar_path, meta, one_rep_bar_path};
    use super::super::SessionResults;
    use super::*;
    use tracker_core::{LiftClassification, PhaseOrder, Point};

    fn proposal(lift: LiftType) -> SeedProposal {
        SeedProposal {
            frame_index: 7,
            position: Point::new(12.0, 34.0),
            plate_diameter_px: 80.0,
            lift: LiftClassification {
                lift,
                reason: "test reason".to_string(),
            },
            confidence: 0.87,
        }
    }

    /// A state whose pre-pass is driven by a hand-built channel. Returns the
    /// sender (to script worker messages) and the handle's cancel flag.
    fn state_finding_bar() -> (AppState, mpsc::Sender<PrepassMessage>, Arc<AtomicBool>) {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(30)));
        let (tx, rx) = mpsc::channel();
        let handle = PrepassHandle::new(rx, Arc::default());
        let cancel = handle.cancel_flag();
        state.attach_prepass(handle);
        (state, tx, cancel)
    }

    fn state_with_proposal(lift: LiftType) -> AppState {
        let (mut state, tx, _) = state_finding_bar();
        tx.send(PrepassMessage::Done(Ok(Some(proposal(lift)))))
            .unwrap();
        assert!(state.poll_prepass());
        state
    }

    #[test]
    fn proposal_is_not_a_seed_until_confirmed() {
        let state = state_with_proposal(LiftType::Squat);
        assert!(state.proposal().is_some());
        assert!(state.seed.is_none());
        assert!(
            !state.can_start_tracking(),
            "an unconfirmed run never starts"
        );
        assert_eq!(state.current_frame, 7, "view jumps to the proposal frame");
    }

    #[test]
    fn confirm_sets_seed_to_proposal_frame_and_position() {
        let mut state = state_with_proposal(LiftType::Squat);
        let seed = state.confirm_proposal().expect("seed");
        assert_eq!(seed.frame_index, 7);
        assert_eq!(state.seed, Some(seed));
        assert_eq!(state.seed.unwrap().position, Point::new(12.0, 34.0));
        assert!(state.proposal().is_none());
        assert_eq!(state.confirmed_lift(), LiftType::Squat);
        assert!(state.can_start_tracking());
    }

    #[test]
    fn reject_returns_to_manual_seeding_and_keeps_no_seed() {
        let mut state = state_with_proposal(LiftType::Squat);
        state.reject_proposal();
        assert!(state.proposal().is_none());
        assert!(state.seed.is_none());
        assert_eq!(state.mode(), Mode::PlacingSeed);
        assert_eq!(state.confirmed_lift(), LiftType::Unknown);
        assert!(!state.can_start_tracking());
    }

    #[test]
    fn manual_seed_cancels_a_running_prepass() {
        let (mut state, _tx, cancel) = state_finding_bar();
        assert!(state.is_finding_bar());
        state.toggle_placing_seed();
        state.place_seed(Point::new(5.0, 5.0));
        assert!(!state.is_finding_bar());
        assert!(cancel.load(Ordering::Relaxed), "worker told to stop");
        assert_eq!(state.confirmed_lift(), LiftType::Unknown);
        assert!(state.seed.is_some());
    }

    #[test]
    fn manual_seed_discards_a_pending_proposal_and_its_lift() {
        let mut state = state_with_proposal(LiftType::Deadlift);
        state.toggle_placing_seed();
        state.place_seed(Point::new(5.0, 5.0));
        assert!(state.proposal().is_none());
        assert_eq!(state.confirmed_lift(), LiftType::Unknown);
    }

    #[test]
    fn opening_another_video_cancels_a_running_prepass() {
        // Opening a video replaces the whole `AppState`; dropping it drops
        // the job handle.
        let (state, _tx, cancel) = state_finding_bar();
        assert!(!cancel.load(Ordering::Relaxed));
        drop(state);
        assert!(cancel.load(Ordering::Relaxed));
    }

    #[test]
    fn cancel_prepass_returns_to_idle_and_stops_the_worker() {
        let (mut state, _tx, cancel) = state_finding_bar();
        state.cancel_prepass();
        assert!(!state.is_finding_bar());
        assert!(cancel.load(Ordering::Relaxed));
        assert!(state.proposal().is_none());
        assert!(state.seed.is_none());
    }

    #[test]
    fn unknown_lift_requires_confirmation_and_uses_default_rep_order() {
        let mut state = state_with_proposal(LiftType::Unknown);
        assert!(state.seed.is_none());
        let headline = state.proposal_headline().unwrap();
        assert!(headline.contains("Lift type not recognised"), "{headline}");
        assert!(headline.contains("confidence 87%"), "{headline}");
        state.confirm_proposal().unwrap();
        assert_eq!(state.confirmed_lift(), LiftType::Unknown);
        let results = state.build_results(one_rep_bar_path());
        assert_eq!(results.reps.len(), 1);
        assert_eq!(results.reps[0].order, PhaseOrder::EccentricFirst);
    }

    #[test]
    fn confirmed_deadlift_proposal_threads_concentric_first_reps_into_results() {
        let mut state = state_with_proposal(LiftType::Deadlift);
        state.confirm_proposal().unwrap();
        let results = state.build_results(deadlift_bar_path());
        assert_eq!(results.reps.len(), 1, "{:?}", results.reps);
        assert_eq!(results.reps[0].order, PhaseOrder::ConcentricFirst);
        // The same path read as an unknown lift is eccentric-first.
        let default = SessionResults::build(deadlift_bar_path(), None, 0);
        assert!(default
            .reps
            .iter()
            .all(|r| r.order == PhaseOrder::EccentricFirst));
    }

    #[test]
    fn no_proposal_sets_a_clear_banner_message_not_a_seed() {
        let (mut state, tx, _) = state_finding_bar();
        tx.send(PrepassMessage::Done(Ok(None))).unwrap();
        state.poll_prepass();
        assert!(state.seed.is_none());
        assert!(state.proposal().is_none());
        assert!(!state.can_start_tracking());
        assert_eq!(state.banner_text(), NO_BAR_FOUND_MESSAGE);
        assert_eq!(state.mode(), Mode::PlacingSeed, "click-to-seed is armed");
    }

    #[test]
    fn prepass_error_is_a_banner_message_not_a_panic_or_a_run() {
        let (mut state, tx, _) = state_finding_bar();
        tx.send(PrepassMessage::Done(Err("boom".into()))).unwrap();
        state.poll_prepass();
        assert!(state.seed.is_none());
        assert_eq!(state.banner_text(), NO_BAR_FOUND_MESSAGE);
    }

    #[test]
    fn worker_that_drops_its_sender_without_done_is_reported_as_failure() {
        let (mut state, tx, _) = state_finding_bar();
        drop(tx);
        assert!(state.poll_prepass());
        assert!(!state.is_finding_bar(), "job cleared, no endless spinner");
        assert!(state.seed.is_none());
        assert_eq!(state.banner_text(), NO_BAR_FOUND_MESSAGE);
        assert_eq!(state.mode(), Mode::PlacingSeed);
    }

    #[test]
    fn reject_leaves_no_job_and_no_pending_proposal() {
        let mut state = state_with_proposal(LiftType::Squat);
        state.reject_proposal();
        assert!(matches!(state.job, Job::Idle));
        assert!(state.proposal().is_none());
        assert!(!state.is_finding_bar());
    }

    #[test]
    fn banner_cancel_leaves_no_job_and_no_pending_proposal() {
        let (mut state, _tx, _) = state_finding_bar();
        state.cancel_prepass();
        assert!(matches!(state.job, Job::Idle));
        assert!(state.proposal().is_none());
        assert!(!state.is_finding_bar());
    }

    #[test]
    fn no_proposal_does_not_abandon_an_in_progress_calibration() {
        let (mut state, tx, _) = state_finding_bar();
        state.toggle_calibrating();
        state.place_calibration_point(Point::new(1.0, 1.0));
        let before = state.mode();
        assert!(matches!(
            before,
            Mode::Calibrating {
                first_point: Some(_),
                ..
            }
        ));
        tx.send(PrepassMessage::Done(Ok(None))).unwrap();
        state.poll_prepass();
        assert_eq!(state.mode(), before, "calibration untouched");
        assert_eq!(state.prepass_notice(), Some(NO_BAR_FOUND_MESSAGE));
    }

    #[test]
    fn manual_seed_click_during_tracking_does_not_change_the_confirmed_lift() {
        let mut state = state_with_proposal(LiftType::Deadlift);
        state.confirm_proposal().unwrap();
        state.job = Job::Tracking {
            handle: super::super::test_support::dummy_tracking_handle(),
            paused: false,
        };
        state.toggle_placing_seed();
        let seed = state.seed;
        state.place_seed(Point::new(99.0, 99.0));
        assert_eq!(state.seed, seed, "seed not overwritten mid-run");
        assert_eq!(state.confirmed_lift(), LiftType::Deadlift);
        let results = state.build_results(deadlift_bar_path());
        assert_eq!(results.reps[0].order, PhaseOrder::ConcentricFirst);
    }

    #[test]
    fn progress_shows_in_the_banner() {
        let (mut state, tx, _) = state_finding_bar();
        assert_eq!(state.banner_text(), "Finding the bar\u{2026} 0%");
        tx.send(PrepassMessage::Progress { done: 3, total: 12 })
            .unwrap();
        assert!(state.poll_prepass());
        assert_eq!(state.prepass_percent(), Some(25));
        assert_eq!(state.banner_text(), "Finding the bar\u{2026} 25%");
    }

    #[test]
    fn proposal_banner_names_lift_confidence_and_reason() {
        let state = state_with_proposal(LiftType::Squat);
        let text = state.banner_text();
        assert!(text.starts_with("Proposed bar \u{b7} Squat \u{b7} confidence 87%"));
        assert!(text.contains("test reason"));
    }

    #[test]
    fn only_one_job_at_a_time_prepass_blocks_tracking_start() {
        // Decision: while the pre-pass runs, tracking and the benchmark are
        // refused (not preempting the pre-pass).
        let (mut state, _tx, cancel) = state_finding_bar();
        state.seed = Some(Seed {
            position: Point::new(1.0, 1.0),
            frame_index: 0,
        });
        assert!(!state.can_start_tracking());
        assert!(!state.can_test_strategies());
        state.start_tracking();
        assert!(state.is_finding_bar(), "pre-pass untouched");
        assert!(!state.is_tracking());
        assert!(!cancel.load(Ordering::Relaxed));
    }

    #[test]
    fn start_prepass_is_refused_when_a_seed_exists_or_a_job_runs() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(30)));
        state.toggle_placing_seed();
        state.place_seed(Point::new(1.0, 1.0));
        assert!(!state.can_start_prepass());
        let (state, _tx, _) = state_finding_bar();
        assert!(!state.can_start_prepass());
    }

    #[test]
    fn unknown_frame_count_falls_back_to_manual_seeding() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(None));
        state.start_prepass();
        assert!(!state.is_finding_bar());
        assert_eq!(state.banner_text(), NO_BAR_FOUND_MESSAGE);
    }
}
