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
    BarPath, Frame, FrameSource, Point, SessionState, Source as SampleSource, TemplateTracker,
    TemplateTrackerConfig, Timebase, TrackingSession, TrackingSessionConfig,
};

use crate::ffmpeg_source::FfmpegFrameSource;

/// Sensible default `TemplateTracker` tuning for the test_videos/ footage.
/// Exposed as consts so 3.4 (end-to-end run on each video) can revisit them
/// without hunting through the tracking wiring.
pub const DEFAULT_PATCH_RADIUS: u32 = 12;
pub const DEFAULT_SEARCH_RADIUS: u32 = 30;
pub const DEFAULT_MIN_SCORE: f64 = 0.4;
pub const DEFAULT_COAST_LIMIT: u32 = 5;

/// Builds a `TemplateTrackerConfig` from the module's default consts.
pub fn default_tracker_config() -> TemplateTrackerConfig {
    TemplateTrackerConfig::builder()
        .patch_radius(DEFAULT_PATCH_RADIUS)
        .search_radius(DEFAULT_SEARCH_RADIUS)
        .min_score(DEFAULT_MIN_SCORE)
        .build()
}

/// Builds a `TrackingSessionConfig` from the module's default consts.
pub fn default_session_config() -> TrackingSessionConfig {
    TrackingSessionConfig::builder()
        .coast_limit(DEFAULT_COAST_LIMIT)
        .build()
}

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

/// Pure UI-facing state accumulated from a run's `TrackingMessage`s. Kept
/// separate from the thread/channel plumbing (`TrackingHandle` below) so
/// it's unit-testable without spawning anything.
#[derive(Debug, Clone, Default)]
pub struct TrackingRunState {
    pub running: bool,
    pub last_frame_index: Option<u64>,
    pub last_position: Option<Point>,
    pub last_source: Option<SampleSource>,
    pub session_state: Option<SessionState>,
    pub frames_processed: u64,
    pub error: Option<String>,
    pub bar_path: Option<BarPath>,
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
                self.last_frame_index = Some(video_frame_index);
                self.last_position = Some(position);
                self.last_source = Some(source);
                self.session_state = Some(state);
                self.frames_processed += 1;
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
            (Some(idx), Some(SessionState::NeedsReseed)) => format!(
                "tracking paused at frame {idx}: object lost, place a new seed then Resume"
            ),
            (Some(idx), _) => {
                format!("tracking… frame {idx} ({} processed)", self.frames_processed)
            }
            _ => "tracking starting…".to_string(),
        }
    }
}

/// A handle to a running/paused tracking worker: the read side of its
/// progress channel and the write side of its reseed channel.
pub struct TrackingHandle {
    pub messages: Receiver<TrackingMessage>,
    reseed_tx: Sender<ReseedCommand>,
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
}

/// Spawns a background thread that tracks from `seed_position` (placed on
/// `seed_frame_index`) to the end of the video, sending `TrackingMessage`s
/// as it goes.
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
pub fn spawn_tracking(
    video_path: PathBuf,
    width: u32,
    height: u32,
    fps_num: u64,
    fps_den: u64,
    seed_frame_index: u64,
    seed_position: Point,
    tracker_config: TemplateTrackerConfig,
    session_config: TrackingSessionConfig,
) -> TrackingHandle {
    let (tx, rx) = mpsc::channel::<TrackingMessage>();
    let (reseed_tx, reseed_rx) = mpsc::channel::<ReseedCommand>();

    thread::spawn(move || {
        run_tracking_worker(
            &video_path,
            width,
            height,
            fps_num,
            fps_den,
            seed_frame_index,
            seed_position,
            tracker_config,
            session_config,
            &tx,
            &reseed_rx,
        );
    });

    TrackingHandle { messages: rx, reseed_tx }
}

#[allow(clippy::too_many_arguments)]
fn run_tracking_worker(
    video_path: &Path,
    width: u32,
    height: u32,
    fps_num: u64,
    fps_den: u64,
    seed_frame_index: u64,
    seed_position: Point,
    tracker_config: TemplateTrackerConfig,
    session_config: TrackingSessionConfig,
    tx: &Sender<TrackingMessage>,
    reseed_rx: &Receiver<ReseedCommand>,
) {
    let mut source = match FfmpegFrameSource::spawn(video_path, width, height) {
        Ok(s) => s,
        Err(e) => {
            let _ = tx.send(TrackingMessage::Error(e.to_string()));
            return;
        }
    };

    let seed_frame = match decode_up_to(&mut source, seed_frame_index) {
        Ok(Some(frame)) => frame,
        Ok(None) => {
            let _ = tx.send(TrackingMessage::Error(
                "video ended before reaching the seed frame".to_string(),
            ));
            return;
        }
        Err(e) => {
            let _ = tx.send(TrackingMessage::Error(e.to_string()));
            return;
        }
    };

    let tracker = match TemplateTracker::new(&seed_frame, seed_position, tracker_config) {
        Ok(t) => t,
        Err(e) => {
            let _ = tx.send(TrackingMessage::Error(format!(
                "seed patch out of bounds: {e:?}"
            )));
            return;
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

    loop {
        match source.next_frame_checked() {
            Ok(Some(frame)) => {
                session.step(&frame);
                if let Some(last) = session.samples().last() {
                    let video_frame_index = seed_frame_index + last.frame_index;
                    let _ = tx.send(TrackingMessage::Progress {
                        video_frame_index,
                        position: last.position,
                        source: last.source,
                        state: session.state(),
                    });
                }
                if session.state() == SessionState::NeedsReseed {
                    match reseed_rx.recv() {
                        Ok(cmd) => {
                            let relative =
                                cmd.video_frame_index.saturating_sub(seed_frame_index);
                            session.reseed(relative, cmd.position);
                            let _ = tx.send(TrackingMessage::Progress {
                                video_frame_index: cmd.video_frame_index,
                                position: cmd.position,
                                source: SampleSource::Tracked,
                                state: SessionState::Tracking,
                            });
                        }
                        // UI dropped the handle (e.g. app closing): stop.
                        Err(_) => return,
                    }
                }
            }
            Ok(None) => break,
            Err(e) => {
                let _ = tx.send(TrackingMessage::Error(e.to_string()));
                return;
            }
        }
    }

    let timebase = match Timebase::new(fps_num, fps_den) {
        Ok(tb) => tb,
        Err(_) => {
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
    let _ = tx.send(TrackingMessage::Done(bar_path));
}

/// Decodes frames sequentially from `source`, discarding all but the last,
/// up to and including index `target` (0-based). Returns `Ok(None)` if the
/// source ends before reaching it. Generic over any `FrameSource` so it's
/// unit-testable against an in-memory reader, not just a real ffmpeg pipe.
fn decode_up_to<S: FrameSource>(source: &mut S, target: u64) -> Result<Option<Frame>, S::Error> {
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

    fn synthetic_frame_bytes(width: u32, height: u32, fill: u8) -> Vec<u8> {
        vec![fill; width as usize * height as usize * 3]
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
