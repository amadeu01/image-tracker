//! Worker orchestration: spawn the tracking thread, decode up to the seed,
//! resolve the tracker, and drive `run_tracking_loop`.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use tracker_core::{
    BarPath, CircleTracker, ColorModel, ColorModelConfig, ColorTracker, Frame, FrameSource, Point,
    SessionState, Source as SampleSource, TemplateTracker, Timebase, TrackerKind,
    TrackerSuggestionConfig, TrackingSession, TrackingSessionConfig,
};

use crate::ffmpeg_source::FfmpegFrameSource;

use super::config::*;
use super::run_loop::*;
use super::state::*;

/// Spawns a background thread that tracks from `job.seed_position` (placed
/// on `job.seed_frame_index`) to the end of the video, sending
/// `TrackingMessage`s as it goes.
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
pub fn spawn_tracking(job: TrackingJob) -> TrackingHandle {
    let (tx, rx) = mpsc::channel::<TrackingMessage>();
    let (reseed_tx, reseed_rx) = mpsc::channel::<ReseedCommand>();
    let (control_tx, control_rx) = mpsc::channel::<ControlCommand>();

    thread::spawn(move || {
        run_tracking_worker(job, &tx, &reseed_rx, &control_rx);
    });

    TrackingHandle {
        messages: rx,
        reseed_tx,
        control_tx,
    }
}

#[tracing::instrument(
    skip_all,
    fields(
        video = %job.video_path.display(),
        seed_frame = job.seed_frame_index,
        seed_x = job.seed_position.x,
        seed_y = job.seed_position.y,
    )
)]
fn run_tracking_worker(
    job: TrackingJob,
    tx: &Sender<TrackingMessage>,
    reseed_rx: &Receiver<ReseedCommand>,
    control_rx: &Receiver<ControlCommand>,
) {
    let TrackingJob {
        video_path,
        width,
        height,
        fps_num,
        fps_den,
        seed_frame_index,
        seed_position,
        lift: _,
        tracker_config,
        session_config,
        tracker_selection,
        color_tracker_config,
    } = job;
    let video_path: &Path = &video_path;

    tracing::info!("tracking run started");

    let mut source = match FfmpegFrameSource::spawn(video_path, width, height) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "failed to spawn ffmpeg decoder for tracking run");
            let _ = tx.send(TrackingMessage::Error(e.to_string()));
            return;
        }
    };

    let seed_frame = match decode_up_to(&mut source, seed_frame_index) {
        Ok(Some(frame)) => frame,
        Ok(None) => {
            tracing::error!("video ended before reaching the seed frame");
            let _ = tx.send(TrackingMessage::Error(
                "video ended before reaching the seed frame".to_string(),
            ));
            return;
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to decode up to seed frame");
            let _ = tx.send(TrackingMessage::Error(e.to_string()));
            return;
        }
    };

    // The circle tracker (17.5) is never chosen by `Auto`
    // (`suggest_tracker`'s heuristic has no circular-edge detector yet, see
    // `TrackerSelection::Circle`'s doc comment) — built directly here,
    // bypassing the `TrackerKind` resolution the other two trackers share,
    // but still feeding into the same session/loop/export path below.
    if tracker_selection == TrackerSelection::Circle {
        tracing::info!(kind = "circle", auto = false, "tracker kind resolved");
        let tracker =
            match CircleTracker::new(&seed_frame, seed_position, default_circle_tracker_config()) {
                Ok(t) => AnyTracker::Circle(t),
                Err(e) => {
                    tracing::error!(error = ?e, "no circle found at seed");
                    let _ = tx.send(TrackingMessage::Error(format!(
                        "no circle found at seed: {e:?}"
                    )));
                    return;
                }
            };
        return finish_tracking_run(
            tracker,
            source,
            seed_frame_index,
            seed_position,
            fps_num,
            fps_den,
            session_config,
            tx,
            reseed_rx,
            control_rx,
        );
    }

    let resolved_kind = match tracker_selection {
        TrackerSelection::Template => TrackerKind::Template,
        TrackerSelection::Color => TrackerKind::Color,
        TrackerSelection::Circle => unreachable!("handled above"),
        TrackerSelection::Auto => tracker_core::suggest_tracker(
            &seed_frame,
            seed_position,
            TrackerSuggestionConfig::default(),
        ),
    };
    tracing::info!(
        kind = ?resolved_kind,
        auto = tracker_selection == TrackerSelection::Auto,
        "tracker kind resolved"
    );

    let tracker = match resolved_kind {
        TrackerKind::Template => {
            match TemplateTracker::new(&seed_frame, seed_position, tracker_config) {
                Ok(t) => AnyTracker::Template(t),
                Err(e) => {
                    tracing::error!(error = ?e, "seed patch out of bounds");
                    let _ = tx.send(TrackingMessage::Error(format!(
                        "seed patch out of bounds: {e:?}"
                    )));
                    return;
                }
            }
        }
        TrackerKind::Color => {
            match ColorModel::learn(
                &seed_frame,
                seed_position,
                tracker_config.patch_radius(),
                ColorModelConfig::default(),
            ) {
                Ok(model) => AnyTracker::Color(ColorTracker::new(model, color_tracker_config)),
                Err(e) => {
                    tracing::error!(error = ?e, "seed patch out of bounds for color model");
                    let _ = tx.send(TrackingMessage::Error(format!(
                        "seed patch out of bounds: {e:?}"
                    )));
                    return;
                }
            }
        }
    };

    finish_tracking_run(
        tracker,
        source,
        seed_frame_index,
        seed_position,
        fps_num,
        fps_den,
        session_config,
        tx,
        reseed_rx,
        control_rx,
    );
}

/// Shared tail of `run_tracking_worker`, once a concrete `AnyTracker` has
/// been resolved and constructed (Template/Color via `TrackerKind`, or
/// Circle directly, 17.5): builds the `TrackingSession`, drives
/// `run_tracking_loop` to completion, reaps the ffmpeg child on a clean EOF,
/// and sends the final `BarPath` as `Done`.
#[allow(clippy::too_many_arguments)]
fn finish_tracking_run(
    tracker: AnyTracker,
    mut source: FfmpegFrameSource<std::process::ChildStdout>,
    seed_frame_index: u64,
    seed_position: Point,
    fps_num: u64,
    fps_den: u64,
    session_config: TrackingSessionConfig,
    tx: &Sender<TrackingMessage>,
    reseed_rx: &Receiver<ReseedCommand>,
    control_rx: &Receiver<ControlCommand>,
) {
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

    // dt (17.2): seconds between consecutive frames, fed to every
    // `TrackingSession::step` so the tracker's motion model can predict and
    // gate correctly. Falls back to a plausible 30fps if the reported fps
    // is degenerate (zero numerator/denominator) — the final `Timebase`
    // construction below still fails the run outright for that case; this
    // fallback only avoids blocking per-step motion reasoning on it.
    let dt = Timebase::new(fps_num, fps_den)
        .map(|tb| 1.0 / tb.fps())
        .unwrap_or(1.0 / 30.0);

    let outcome = match run_tracking_loop(
        &mut source,
        &mut session,
        seed_frame_index,
        dt,
        tx,
        reseed_rx,
        control_rx,
    ) {
        Ok(outcome) => outcome,
        Err(e) => {
            tracing::error!(error = %e, "decode error during tracking run");
            let _ = tx.send(TrackingMessage::Error(e.to_string()));
            return;
        }
    };
    // Only reap on a clean decode EOF. `reap_after_eof` reads stderr to EOF
    // then `wait()`s the child; that is safe *only* once ffmpeg has finished
    // writing (EOF drained the stdout pipe). On an early user Stop the child
    // is still mid-video with a full, undrained stdout pipe, so reaping here
    // would deadlock — ffmpeg blocked writing, us blocked in `wait()` — and
    // the worker would never send `Done`, leaving the GUI stuck in "paused"
    // with no results (the milestone 17 "object lost" pause made users hit
    // Finish mid-run, which is how this surfaced). On `Stopped` we skip the
    // reap and let `Drop` kill+reap the still-running child instead.
    // `run_tracking_loop` only sees the `FrameSource` port, not the
    // ffmpeg-specific reap step, so it stays generic/testable against
    // in-memory sources.
    if let LoopOutcome::Completed = outcome {
        if let Err(e) = source.reap_after_eof() {
            tracing::error!(error = %e, "ffmpeg exited with an error during tracking run");
            let _ = tx.send(TrackingMessage::Error(e.to_string()));
            return;
        }
    }

    let timebase = match Timebase::new(fps_num, fps_den) {
        Ok(tb) => tb,
        Err(_) => {
            tracing::error!("invalid fps reported by ffprobe (zero numerator/denominator)");
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
    tracing::info!(
        frames_processed = session.samples().len(),
        gaps = bar_path.gaps().len(),
        points = bar_path.points().len(),
        "tracking run done"
    );
    let _ = tx.send(TrackingMessage::Done(bar_path));
}

/// Decodes frames sequentially from `source`, discarding all but the last,
/// up to and including index `target` (0-based). Returns `Ok(None)` if the
/// source ends before reaching it. Generic over any `FrameSource` so it's
/// unit-testable against an in-memory reader, not just a real ffmpeg pipe.
pub(crate) fn decode_up_to<S: FrameSource>(
    source: &mut S,
    target: u64,
) -> Result<Option<Frame>, S::Error> {
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
}
