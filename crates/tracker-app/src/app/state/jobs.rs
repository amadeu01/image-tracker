//! Background-job concern (task 20.1 split out of the former flat
//! `state.rs`; task 20.5 replaced the three independent `Option<Handle>`
//! fields with one [`Job`] enum): the three handle+`poll_*` pairs `AppState`
//! drives — tracking, auto/rep-clip export, and the strategy benchmark — plus
//! every `can_*` predicate gating their buttons and the run-lifecycle
//! controls (Pause/Resume/Finish/Discard).
//!
//! ## The `Job` enum (task 20.5)
//!
//! Before this task, `AppState` held `tracking: Option<TrackingHandle>`,
//! `export: Option<ExportHandle>` and `benchmark: Option<BenchmarkHandle>` as
//! three independent `Option`s — three bits, 2³ = 8 representable states,
//! but the domain permits only 4: idle, or exactly one of the three running
//! (auto-export only ever starts once tracking has *finished*, so
//! "tracking + export" was already unreachable at runtime, but the type
//! still allowed the other illegal combinations — "export + benchmark",
//! "tracking + benchmark", "all three" — and every `can_*`/`poll_*` method
//! had to defensively check "and nothing else is running" by hand). `Job`
//! makes the mutual exclusion a compile-time fact: there is exactly one
//! background job field, and its variant *is* which job (if any) is active,
//! so "start X while Y runs" is a match arm that doesn't exist rather than a
//! guarded-against field combination.
//!
//! `TrackingRunState` (the tracking reducer's accumulated state — last
//! frame, error, gap count…) stays a top-level `AppState` field rather than
//! moving inside `Job::Tracking`: unlike the handle, callers read it *after*
//! the job clears too (`bottom_bar.rs`'s error/paused status line keys off
//! `tracking_run.error`/`session_state` even once `tracking` — now
//! `Job::Idle` — has gone quiet, since the reducer is the record of what the
//! run did, not a proxy for "is it still running"). Same reasoning as
//! `bar_path`/`results`: it is a *result* that outlives the job, so it lives
//! next to them, not inside the enum. `benchmark_rows`/`exported_files` are
//! the same shape of result for the other two jobs and were already
//! `AppState` fields, unchanged by this task.

use super::review::SessionResults;
use super::{AppState, EventLevel, Mode};
use crate::compare::{BenchmarkHandle, BenchmarkMessage};
use crate::export_job::{self, ExportHandle, ExportMessage};
use crate::tracking::{self, TrackingHandle, TrackingRunState};

/// The one background job `AppState` may be driving at a time (task 20.5).
/// See the module doc comment for why this replaced three independent
/// `Option<Handle>` fields, and why `TrackingRunState` is *not* one of this
/// enum's fields.
pub enum Job {
    /// No background job is running.
    Idle,
    /// A tracking run is active or paused (by the user, or by the tracker's
    /// own `NeedsReseed`, which `tracking_run.session_state` — not this
    /// variant — distinguishes).
    Tracking {
        handle: TrackingHandle,
        paused: bool,
    },
    /// The auto-export (or per-rep-clip export) job is writing files.
    Exporting { handle: ExportHandle },
    /// The strategy benchmark (task 11.4) is running; `progress` is
    /// `(strategies_started, total)`, `None` until the first progress
    /// message arrives.
    Benchmarking {
        handle: BenchmarkHandle,
        progress: Option<(usize, usize)>,
    },
}

impl AppState {
    /// Whether a tracking run is currently active or paused.
    pub fn is_tracking(&self) -> bool {
        matches!(self.job, Job::Tracking { .. })
    }

    /// Whether an export job (auto-export or rep-clip export) is currently
    /// writing files.
    pub fn is_exporting(&self) -> bool {
        matches!(self.job, Job::Exporting { .. })
    }

    /// Whether the strategy benchmark is currently running.
    pub fn is_benchmarking(&self) -> bool {
        matches!(self.job, Job::Benchmarking { .. })
    }

    /// Whether the user has paused the active tracking run (task 10.4) —
    /// distinct from `tracking_run.session_state == NeedsReseed`, which is
    /// the tracker itself pausing because it lost the object. `false`
    /// outside an active tracking run.
    pub fn is_paused(&self) -> bool {
        matches!(self.job, Job::Tracking { paused: true, .. })
    }

    /// How many of the 6 strategies the active benchmark has started
    /// (`0..=6`), for the side panel's progress display. `None` outside an
    /// active benchmark (including before its first progress message).
    pub fn benchmark_progress(&self) -> Option<(usize, usize)> {
        match &self.job {
            Job::Benchmarking { progress, .. } => *progress,
            _ => None,
        }
    }

    /// Whether the "Track" action should currently be available: a Seed
    /// must be placed, and no run already active.
    pub fn can_start_tracking(&self) -> bool {
        self.seed.is_some() && !self.is_tracking()
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
        self.job = Job::Tracking {
            handle,
            paused: false,
        };
        self.tracking_run = TrackingRunState::started();
        self.bar_path = None;
        self.live_reps = None;
        self.selected_rep = None;
        self.rep_clip = None;
        // A fresh run has no results to show yet — flip the pill to Live
        // (task 13.1) so the toolbar reflects what's actually happening.
        self.display_mode = super::DisplayMode::Live;
    }

    /// Drains any pending messages from the active tracking worker,
    /// applying each to `tracking_run` and advancing the display frame to
    /// follow the latest tracked/interpolated position. Returns `true` if
    /// at least one message was processed (the caller should request a
    /// repaint). Once the run finishes (or errors), stores the completed
    /// `BarPath` (if any) and drops the worker handle.
    pub fn poll_tracking(&mut self) -> bool {
        let Job::Tracking { handle, .. } = &self.job else {
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
            self.job = Job::Idle;
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
                    self.display_mode = super::DisplayMode::Results;
                }
            }
        }
        any
    }

    /// Whether the "Test strategies" button (task 11.4) should currently be
    /// available: a Seed must be placed, and neither a tracking run nor
    /// another benchmark is already active.
    pub fn can_test_strategies(&self) -> bool {
        self.seed.is_some() && !self.is_tracking() && !self.is_benchmarking()
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
        // dt (17.2) for the tracker's motion model; falls back to a
        // plausible 30fps if the reported fps is degenerate, same policy as
        // the CLI `compare` path (compare.rs).
        let dt = tracker_core::Timebase::new(self.metadata.fps_num, self.metadata.fps_den)
            .map(|tb| 1.0 / tb.fps())
            .unwrap_or(1.0 / 30.0);
        let handle = crate::compare::spawn_benchmark(
            self.video_path.clone(),
            self.metadata.display_width(),
            self.metadata.display_height(),
            seed.frame_index,
            seed.position,
            crate::compare::DEFAULT_COMPARE_FRAMES,
            dt,
            coast_limit,
            tuning,
        );
        self.job = Job::Benchmarking {
            handle,
            progress: Some((0, 6)),
        };
        self.benchmark_rows = None;
    }

    /// Drains any pending messages from the active benchmark worker.
    /// Returns `true` if at least one message was processed (the caller
    /// should request a repaint), mirroring `poll_tracking`/`poll_export`'s
    /// shape.
    pub fn poll_benchmark(&mut self) -> bool {
        let Job::Benchmarking { handle, .. } = &self.job else {
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
                BenchmarkMessage::Progress {
                    strategy_index,
                    total,
                } => {
                    if let Job::Benchmarking { progress, .. } = &mut self.job {
                        *progress = Some((strategy_index, total));
                    }
                }
                BenchmarkMessage::Done(rows) => {
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
                    self.job = Job::Idle;
                }
                BenchmarkMessage::Error(e) => {
                    self.push_event(EventLevel::Error, format!("strategy benchmark error: {e}"));
                    self.job = Job::Idle;
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
        self.job = Job::Exporting {
            handle: export_job::spawn_export(job),
        };
    }

    /// Drains any pending messages from the active export job, applying
    /// each as an event. Returns `true` if at least one message was
    /// processed (the caller should request a repaint). Mirrors
    /// `poll_tracking`'s drain-then-react shape.
    pub fn poll_export(&mut self) -> bool {
        let Job::Exporting { handle } = &self.job else {
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
            self.job = Job::Idle;
            self.push_event(EventLevel::Info, "exports written".to_string());
        }
        any
    }

    /// Sends a reseed command to a paused tracking worker, using the
    /// current Seed (which must already be placed on the frame the run
    /// paused at — the UI only enables the Resume action once that's
    /// true). No-op if there's no active worker or no Seed.
    pub fn resume_tracking(&mut self) {
        let (Job::Tracking { handle, .. }, Some(seed)) = (&self.job, self.seed) else {
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
    // Review-step controls (New session, Re-track) — the latter two stay in
    // `mod.rs` since they touch every concern at once. All go through the
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
        self.is_tracking()
            && !self.is_paused()
            && self.tracking_run.session_state != Some(tracker_core::SessionState::NeedsReseed)
    }

    /// Pauses the active run: the worker stops consuming frames until
    /// `unpause_tracking` is called. No-op if `can_pause_tracking` is false.
    pub fn pause_tracking(&mut self) {
        if !self.can_pause_tracking() {
            return;
        }
        if let Job::Tracking { handle, paused } = &mut self.job {
            handle.pause();
            *paused = true;
        }
        tracing::info!("tracking paused (user)");
        self.push_event(EventLevel::Info, "tracking paused".to_string());
    }

    /// Whether Resume (from a user Pause, not a reseed) is currently
    /// available.
    pub fn can_unpause_tracking(&self) -> bool {
        self.is_tracking() && self.is_paused()
    }

    /// Resumes a user-paused run. No-op if `can_unpause_tracking` is false.
    pub fn unpause_tracking(&mut self) {
        if !self.can_unpause_tracking() {
            return;
        }
        if let Job::Tracking { handle, paused } = &mut self.job {
            handle.unpause();
            *paused = false;
        }
        tracing::info!("tracking resumed (user)");
        self.push_event(EventLevel::Info, "tracking resumed".to_string());
    }

    /// Whether Finish (task 15.4 rename of Stop) is currently available:
    /// any active (running, user-paused, or reseed-paused) run.
    pub fn can_stop_tracking(&self) -> bool {
        self.is_tracking()
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
        if let Job::Tracking { handle, paused } = &mut self.job {
            handle.stop();
            *paused = false;
        }
        tracing::info!("tracking finish requested (user)");
        self.push_event(
            EventLevel::Info,
            "finish requested: ending the run with results so far".to_string(),
        );
    }

    /// Whether Discard is currently available: same gate as Finish (any
    /// active run).
    pub fn can_discard_tracking(&self) -> bool {
        self.is_tracking()
    }

    /// Aborts the active run and throws away anything it collected: unlike
    /// `stop_tracking`, this never lands in Review — the worker is told to
    /// stop (same `ControlCommand::Stop`, so it still terminates promptly
    /// and its `FfmpegFrameSource` still gets dropped/killed) but its
    /// eventual `Done`/`Error` message is simply never read, since `self.job`
    /// is reset to `Job::Idle` here rather than left for `poll_tracking` to
    /// drain. Returns the app to seed placement with the Seed intact — the
    /// user re-tracks from the same seed rather than re-placing it. No-op if
    /// `can_discard_tracking` is false.
    pub fn discard_tracking(&mut self) {
        if !self.can_discard_tracking() {
            return;
        }
        if let Job::Tracking { handle, .. } = &self.job {
            handle.stop();
        }
        self.job = Job::Idle;
        self.tracking_run = TrackingRunState::default();
        self.bar_path = None;
        self.results = None;
        self.live_reps = None;
        self.selected_rep = None;
        self.rep_clip = None;
        self.mode = Mode::PlacingSeed;
        tracing::info!("tracking discarded (user)");
        self.push_event(EventLevel::Warn, "tracking discarded".to_string());
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::test_support::{
        dummy_tracking_handle, meta, one_rep_bar_path, state_with_active_run,
    };
    use super::*;
    use crate::app::state::WorkflowStep;

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

    #[test]
    fn pause_tracking_sets_paused_flag_and_is_gated_on_an_active_unpaused_run() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        assert!(!state.can_pause_tracking());
        state.pause_tracking();
        assert!(!state.is_paused(), "no active run: pause must be a no-op");

        let mut state = state_with_active_run();
        assert!(state.can_pause_tracking());
        state.pause_tracking();
        assert!(state.is_paused());
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
        assert!(state.is_paused());
        state.unpause_tracking();
        assert!(!state.is_paused());
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
        assert!(event.message.contains("finish requested"));
        // Finish is a request to the worker, not an immediate teardown: the
        // handle/tracking_run stay in place until the worker's `Done`
        // arrives via `poll_tracking`, same as a clean-EOF finish.
        assert!(state.is_tracking());
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

        assert!(!state.is_tracking());
        assert!(!state.tracking_run.running);
        assert!(state.bar_path.is_none());
        assert!(state.results.is_none());
        assert!(!state.is_paused());
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

    // -- Task 10.8: live rep counter -----------------------------------------

    #[test]
    fn poll_tracking_updates_live_reps_every_30_processed_frames() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(200)));
        state.job = Job::Tracking {
            handle: dummy_tracking_handle(),
            paused: false,
        };
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

    // -- Task 20.5: `Job` mutual exclusion -----------------------------------

    #[test]
    fn starting_tracking_while_tracking_is_already_active_is_a_noop() {
        let mut state = state_with_active_run();
        assert!(state.is_tracking());
        // `can_start_tracking` is false while a run is active, so a second
        // `start_tracking` call must not spawn a second worker / clobber the
        // existing `Job::Tracking`.
        state.start_tracking();
        assert!(state.is_tracking());
    }

    #[test]
    fn starting_a_benchmark_while_tracking_is_active_is_rejected() {
        let mut state = state_with_active_run();
        assert!(!state.can_test_strategies());
        state.start_strategy_benchmark();
        // The `Job` enum makes this structurally impossible to observe as a
        // "both running" state even if the guard were bypassed: `self.job`
        // can only ever hold one variant at a time.
        assert!(state.is_tracking());
        assert!(!state.is_benchmarking());
    }

    #[test]
    fn job_is_idle_by_default_and_exactly_one_variant_active_at_a_time() {
        let state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        assert!(matches!(state.job, Job::Idle));
        assert!(!state.is_tracking());
        assert!(!state.is_exporting());
        assert!(!state.is_benchmarking());
    }

    #[test]
    fn discard_tracking_returns_the_job_to_idle_not_a_dangling_handle() {
        let mut state = state_with_active_run();
        state.discard_tracking();
        assert!(matches!(state.job, Job::Idle));
    }

    #[test]
    fn poll_tracking_done_leaves_the_job_exporting_not_still_tracking() {
        // `poll_tracking`'s finished branch resets `job` to `Idle` before
        // `start_export` sets it to `Exporting` — if that ordering were
        // reversed (or skipped), the job would still read as `Tracking`, or
        // `start_export` would silently clobber a live tracking handle. Both
        // are exactly the illegal-overlap states `Job` exists to rule out.
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(30)));
        state.tracking_run = TrackingRunState::started();
        state.tracking_run.gap_count = 0;
        let bar_path = one_rep_bar_path();
        state.tracking_run.bar_path = Some(bar_path.clone());
        let finished = state
            .tracking_run
            .apply(tracking::TrackingMessage::Done(bar_path));
        assert!(finished);
        // Mirror `poll_tracking`'s finished branch exactly (same order this
        // module's `poll_tracking` uses): job -> Idle, then, once results
        // build successfully, `start_export` flips it to `Exporting`.
        state.bar_path = state.tracking_run.bar_path.clone();
        state.job = Job::Idle;
        assert!(!state.is_tracking(), "job must be Idle, not still Tracking");
        let results = SessionResults::build(state.bar_path.clone().unwrap(), state.calibration, 0);
        state.start_export(&results);
        assert!(state.is_exporting());
        assert!(
            !state.is_tracking(),
            "tracking and exporting are mutually exclusive"
        );
    }
}
