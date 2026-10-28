//! The generic frame loop: drives a `TrackingSession` across a `FrameSource`,
//! reporting progress and handling pause/resume/stop and reseed commands.

use std::sync::mpsc::{self, Receiver, Sender};

use tracker_core::{FrameSource, SessionState, Source as SampleSource, Tracker, TrackingSession};

use super::state::*;

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
/// How `run_tracking_loop` ended, so the caller knows whether the ffmpeg
/// child hit a clean EOF (safe to `reap_after_eof`, which reads stderr to
/// EOF then `wait()`s) or was cut short by the user (Stop/Finish/Discard, or
/// the UI dropping its handle). On an early stop the child is still mid-video
/// with an *undrained* stdout pipe: reaping it would deadlock (ffmpeg blocked
/// writing to the full pipe, us blocked in `wait()`), so the caller must skip
/// the reap and let `Drop` kill the child instead. This is what the milestone
/// 17 "object lost" pause exposed — it made users hit Finish mid-run for the
/// first time on a long real video.
pub(super) enum LoopOutcome {
    /// `source.next_frame()` returned `None` — the whole video was decoded.
    Completed,
    /// The user (or a dropped handle) ended the run before EOF.
    Stopped,
}

pub(super) fn run_tracking_loop<S: FrameSource, T: Tracker>(
    source: &mut S,
    session: &mut TrackingSession<T>,
    seed_frame_index: u64,
    dt: f64,
    tx: &Sender<TrackingMessage>,
    reseed_rx: &Receiver<ReseedCommand>,
    control_rx: &Receiver<ControlCommand>,
) -> Result<LoopOutcome, S::Error> {
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
                return Ok(LoopOutcome::Stopped);
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
                            return Ok(LoopOutcome::Stopped);
                        }
                        Ok(ControlCommand::Pause) => continue,
                        Err(_) => return Ok(LoopOutcome::Stopped),
                    }
                }
            }
            Ok(ControlCommand::Resume) | Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {}
        }
        match source.next_frame()? {
            Some(frame) => {
                session.step(&frame, dt);
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
                if session.state() == SessionState::Lost {
                    // 17.4b: terminal, unlike `NeedsReseed` below — do not
                    // block waiting for a reseed (headless has no human to
                    // supply one, and the GUI's own reseed UI targets the
                    // recoverable pause, not this). End the run here so the
                    // caller converges on `Done` with the partial samples
                    // collected up to (and including) the frame that
                    // tripped it — the same honest-partial-path path a
                    // user's Stop already takes, rather than silently
                    // decoding the rest of the video with `step` now a
                    // no-op.
                    tracing::warn!(
                        video_frame_index = seed_frame_index + session.frame_index(),
                        "tracking lost: sustained low identity confidence, ending run \
                         (not auto-resuming from an untrustworthy position)"
                    );
                    return Ok(LoopOutcome::Stopped);
                }
                if session.state() == SessionState::NeedsReseed {
                    tracing::warn!(
                        video_frame_index = seed_frame_index + session.frame_index(),
                        "tracking needs reseed: object lost, waiting for a new seed"
                    );
                    // Task 15.4: this wait must observe *both* channels.
                    // Blocking on `reseed_rx.recv()` alone left
                    // `ControlCommand::Stop` queued unread for the whole
                    // pause — the user's Stop/Finish was ignored until a
                    // reseed arrived (real session: Finish hit twice, run
                    // kept going). std mpsc has no select, and crossbeam
                    // isn't a dependency, so alternate: block on the reseed
                    // channel with a short timeout, and on each timeout
                    // drain `control_rx` for a Stop (which ends the run via
                    // the same Ok(()) -> Done-with-partial-samples path as
                    // every other checkpoint; Discard also sends Stop).
                    // Pause/Resume during a reseed-wait are no-ops: the
                    // worker is already not consuming frames, and the
                    // resume that matters is the reseed itself.
                    let reseed = loop {
                        match reseed_rx.recv_timeout(std::time::Duration::from_millis(50)) {
                            Ok(cmd) => break Some(cmd),
                            Err(mpsc::RecvTimeoutError::Timeout) => {
                                let mut stop = false;
                                while let Ok(cmd) = control_rx.try_recv() {
                                    if cmd == ControlCommand::Stop {
                                        stop = true;
                                    }
                                }
                                if stop {
                                    tracing::info!(
                                        "tracking stopped by user while awaiting reseed; \
                                         ending with samples collected so far"
                                    );
                                    return Ok(LoopOutcome::Stopped);
                                }
                            }
                            // Reseed channel closed while paused: same
                            // channel-closed semantics as before (see the
                            // doc comment above) — end the run cleanly so
                            // the caller emits Done with partial samples.
                            Err(mpsc::RecvTimeoutError::Disconnected) => break None,
                        }
                    };
                    match reseed {
                        Some(cmd) => {
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
                        None => {
                            tracing::info!(
                                "reseed channel closed while paused; ending run with samples collected so far"
                            );
                            return Ok(LoopOutcome::Stopped);
                        }
                    }
                }
            }
            // Clean decode EOF: stop regardless of session state (this is
            // reached even if the session had just resumed out of
            // `NeedsReseed` on the previous iteration and immediately hits
            // the end of the video).
            None => return Ok(LoopOutcome::Completed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffmpeg_source::FfmpegFrameSource;
    use crate::tracking::config::{session_config, TrackerTuning};
    use std::io::Cursor;
    use std::thread;
    use std::time::Duration;
    use tracker_core::TrackingSessionConfig;
    use tracker_core::{Frame, Point, StepOutcome, Track};

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
        /// Inclusive range (by call count, 0-based) of `step` calls that
        /// return `Found` with a low identity confidence (17.4b), instead
        /// of the default 1.0 — enough to drive a `TrackingSession` into
        /// the terminal `Lost` state via `sustained_suspect_limit`.
        low_confidence_range: Option<(u64, u64)>,
        frames_seen: u64,
        position: Point,
    }

    impl Tracker for ScriptedTracker {
        fn step(&mut self, _frame: &Frame, _track: &Track, _dt: f64) -> StepOutcome {
            let frame = self.frames_seen;
            self.frames_seen += 1;
            match self.miss_range {
                Some((from, to)) if frame >= from && frame <= to => StepOutcome::Miss,
                _ => {
                    let identity_confidence = match self.low_confidence_range {
                        Some((from, to)) if frame >= from && frame <= to => 0.4,
                        _ => 1.0,
                    };
                    StepOutcome::Found {
                        position: self.position,
                        score: 1.0,
                        identity_confidence,
                    }
                }
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
            low_confidence_range: None,
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
            1.0,
            &tx,
            &reseed_rx,
            &control_rx,
        );
        assert!(
            matches!(result, Ok(LoopOutcome::Completed)),
            "clean decode EOF must report Completed so the caller reaps ffmpeg"
        );

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

    /// Regression: a user Stop before decode EOF must report `Stopped`, not
    /// `Completed`. The caller reaps ffmpeg only on `Completed`; reaping a
    /// still-running child (undrained stdout pipe) deadlocks, which left the
    /// GUI stuck in "paused" with no results after Finish (surfaced once the
    /// milestone 17 "object lost" pause made Finish-mid-run common). The
    /// `source` here is *not* exhausted when the loop returns — that is the
    /// whole point: it stops early, mid-video.
    #[test]
    fn run_tracking_loop_reports_stopped_on_early_user_stop() {
        let (width, height) = (2u32, 2u32);
        let frame_count = 50u8; // plenty of frames left when we Stop
        let mut source = frame_source_with(width, height, frame_count);
        let tracker = ScriptedTracker {
            miss_range: None,
            low_confidence_range: None,
            frames_seen: 0,
            position: Point::new(1.0, 1.0),
        };
        let mut session = TrackingSession::new(
            tracker,
            0,
            Point::new(1.0, 1.0),
            session_config(TrackerTuning::default()),
        );
        let (tx, _rx) = mpsc::channel::<TrackingMessage>();
        let (_reseed_tx, reseed_rx) = mpsc::channel::<ReseedCommand>();
        let (control_tx, control_rx) = mpsc::channel::<ControlCommand>();
        // Queue Stop before the loop runs: it is observed on the very first
        // pre-frame control check, so the loop returns without exhausting
        // `source`.
        control_tx.send(ControlCommand::Stop).unwrap();

        let result = run_tracking_loop(
            &mut source,
            &mut session,
            0,
            1.0,
            &tx,
            &reseed_rx,
            &control_rx,
        );
        assert!(
            matches!(result, Ok(LoopOutcome::Stopped)),
            "an early user Stop must report Stopped so the caller skips the EOF reap"
        );
        // The source still has frames left — the loop genuinely stopped short.
        assert!(source.next_frame().unwrap().is_some());
    }

    /// 17.4b: a session that goes `Lost` (sustained low identity confidence,
    /// not a miss streak) must stop the loop immediately — like a user
    /// Stop, *not* like `NeedsReseed`'s block-and-wait-for-a-reseed — so the
    /// caller (CLI headless or GUI) converges on `Done` with the partial
    /// path instead of decoding the rest of the video for nothing or
    /// auto-resuming from an untrustworthy position.
    #[test]
    fn run_tracking_loop_stops_immediately_when_session_goes_lost() {
        let (width, height) = (2u32, 2u32);
        let frame_count = 50u8; // plenty of frames left when Lost trips
        let mut source = frame_source_with(width, height, frame_count);
        let tracker = ScriptedTracker {
            miss_range: None,
            low_confidence_range: Some((0, u64::MAX)), // low confidence from frame 1 on
            frames_seen: 0,
            position: Point::new(1.0, 1.0),
        };
        let session_config = TrackingSessionConfig::builder()
            .sustained_suspect_limit(3)
            // 17.4b Lost detection is opt-in (default off) — this test drives
            // the Lost path, so enable it explicitly.
            .lost_detection(true)
            .build();
        let mut session = TrackingSession::new(tracker, 0, Point::new(1.0, 1.0), session_config);
        let (tx, rx) = mpsc::channel::<TrackingMessage>();
        let (_reseed_tx, reseed_rx) = mpsc::channel::<ReseedCommand>();
        let (_control_tx, control_rx) = mpsc::channel::<ControlCommand>();

        let result = run_tracking_loop(
            &mut source,
            &mut session,
            0,
            1.0,
            &tx,
            &reseed_rx,
            &control_rx,
        );
        assert!(
            matches!(result, Ok(LoopOutcome::Stopped)),
            "Lost must report Stopped so the caller skips the EOF reap, same as a user Stop"
        );
        assert_eq!(session.state(), SessionState::Lost);
        // The source still has frames left: the loop genuinely stopped
        // short rather than decoding the whole video with `step` reduced to
        // a no-op.
        assert!(source.next_frame().unwrap().is_some());

        // The last Progress message reflects the terminal Lost state
        // honestly, rather than silently looking like ordinary tracking.
        let messages: Vec<_> = rx.try_iter().collect();
        let last_progress_state = messages.iter().rev().find_map(|m| match m {
            TrackingMessage::Progress { state, .. } => Some(*state),
            _ => None,
        });
        assert_eq!(last_progress_state, Some(SessionState::Lost));
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
            low_confidence_range: None,
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
            1.0,
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
            low_confidence_range: None,
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
            1.0,
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
            low_confidence_range: None,
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
            1.0,
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
            low_confidence_range: None,
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
            1.0,
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

    /// Task 15.4 regression: `Stop` (the toolbar's Finish) sent while the
    /// worker is blocked *awaiting a reseed* (`NeedsReseed`) must end the
    /// run promptly with the samples collected so far, exactly like Stop at
    /// any other checkpoint. Before the fix the reseed wait blocked on
    /// `reseed_rx.recv()` alone and never read `control_rx`, so Stop queued
    /// unread and the run stayed paused forever (real user session: Stop
    /// hit twice, run kept going).
    #[test]
    fn stop_command_is_honored_while_paused_awaiting_reseed() {
        let (width, height) = (2u32, 2u32);
        let frame_count = 8u8;
        let mut source = frame_source_with(width, height, frame_count);
        // Miss from frame 0 so the session pauses (NeedsReseed) well before
        // the source's EOF.
        let tracker = ScriptedTracker {
            miss_range: Some((0, u64::MAX)),
            low_confidence_range: None,
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
        // Keep the reseed sender alive for the whole run: the loop must end
        // because Stop was seen, not because the reseed channel closed.
        let (reseed_tx, reseed_rx) = mpsc::channel::<ReseedCommand>();
        let (control_tx, control_rx) = mpsc::channel::<ControlCommand>();
        let stopper = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            control_tx.send(ControlCommand::Stop).unwrap();
        });

        let result = run_tracking_loop(
            &mut source,
            &mut session,
            seed_frame_index,
            1.0,
            &tx,
            &reseed_rx,
            &control_rx,
        );
        stopper.join().unwrap();
        drop(reseed_tx);

        assert!(
            result.is_ok(),
            "loop must end cleanly on Stop while awaiting a reseed"
        );
        assert_eq!(
            session.state(),
            SessionState::NeedsReseed,
            "session was never reseeded: Stop, not a reseed, ended the wait"
        );
        // Progress messages were sent up to the pause; the caller will turn
        // the session's partial samples into Done. The loop must not have
        // read past the pause point.
        let messages: Vec<_> = rx.try_iter().collect();
        let max_reported = messages
            .iter()
            .filter_map(|m| m.video_frame_index())
            .max()
            .unwrap_or(0);
        assert!(
            max_reported < frame_count as u64 - 1,
            "run read past the pause point after Stop (max reported {max_reported})"
        );
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
            low_confidence_range: None,
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
            1.0,
            &tx,
            &reseed_rx,
            &control_rx,
        );
        resumer.join().unwrap();

        assert!(result.is_ok());
        assert_eq!(rx.try_iter().count(), 0, "no frames should have been read");
    }
}
