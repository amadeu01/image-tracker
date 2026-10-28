//! Shared run state and messages: what flows between the UI thread and the
//! tracking worker (`TrackingMessage`, commands, `TrackingRunState`,
//! `TrackingHandle`, `TrackingJob`). No threads are spawned here.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use tracker_core::{
    BarPath, Calibration, ColorTrackerConfig, Point, SessionState, Source as SampleSource,
    TemplateTrackerConfig, Timebase, TrackingSessionConfig,
};

use super::config::{rep_segmentation_config, TrackerSelection};

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
                    // Interim/preview reconstruction from Progress messages;
                    // the exported path is the session's own BarPath (Done),
                    // which carries the real per-frame confidence.
                    confidence: None,
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
    pub(super) reseed_tx: Sender<ReseedCommand>,
    pub(super) control_tx: Sender<ControlCommand>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use tracker_core::Timebase;

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
}
