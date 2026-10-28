//! Background seed-proposal job: runs `prepass::run_prepass` on a worker
//! thread so the UI thread never decodes (docs/gui-threading.md). Same shape
//! as `export_job.rs`: a thread, an mpsc channel, and a handle with a
//! non-blocking `drain()` polled from `AppState::poll_prepass`.
//!
//! Unlike the other jobs this one is cancelable: the handle owns a shared
//! flag the worker checks between samples. Dropping the handle cancels too,
//! so replacing the `AppState` (opening another video) or the `Job` never
//! leaves a decode loop running for a clip nobody is looking at.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;

use tracker_core::{Frame, SeedProposal};

use crate::frame_cache::FrameDecoder;
use crate::prepass::{run_prepass, DEFAULT_PREPASS_STRIDE};
use crate::seek_source::SeekingFrameDecoder;

/// One update from the pre-pass thread to the UI thread.
#[derive(Debug, Clone, PartialEq)]
pub enum PrepassMessage {
    /// `(samples_done, samples_total)`. Sent only when the whole-percent
    /// value changes, so the UI is not woken once per sample.
    Progress { done: u64, total: u64 },
    /// The pre-pass finished: a proposal, `None` (no moving plate found) or
    /// an error. Not sent when the job was cancelled.
    Done(Result<Option<SeedProposal>, String>),
}

/// Everything the worker needs to open the clip.
pub struct PrepassJob {
    pub video_path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps_num: u64,
    pub fps_den: u64,
    pub frame_count: u64,
}

/// Handle to a running pre-pass: the read side of its channel plus its cancel
/// flag. Dropping it cancels the worker.
pub struct PrepassHandle {
    messages: Receiver<PrepassMessage>,
    cancel: Arc<AtomicBool>,
}

impl PrepassHandle {
    pub fn new(messages: Receiver<PrepassMessage>, cancel: Arc<AtomicBool>) -> Self {
        Self { messages, cancel }
    }

    /// Takes every message the worker has sent so far, without blocking.
    ///
    /// If the worker's sender is gone without a `Done` ever having been
    /// delivered (the thread died), a synthetic `Done(Err(..))` is appended
    /// so the UI never waits forever for a result that cannot arrive. A
    /// cancelled handle never synthesizes it: a cancelled worker is expected
    /// to leave without a `Done`.
    pub fn drain(&self) -> Vec<PrepassMessage> {
        let mut out = Vec::new();
        let mut saw_done = false;
        loop {
            match self.messages.try_recv() {
                Ok(msg) => {
                    saw_done |= matches!(msg, PrepassMessage::Done(_));
                    out.push(msg);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !saw_done && !self.cancel.load(Ordering::Relaxed) {
                        out.push(PrepassMessage::Done(Err(WORKER_STOPPED.to_string())));
                    }
                    break;
                }
            }
        }
        out
    }

    /// Asks the worker to stop before its next sample.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// The shared flag, for tests that need to observe cancellation after the
    /// handle is gone.
    #[cfg(test)]
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }
}

impl Drop for PrepassHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Error text when the worker thread vanished without reporting a result.
pub const WORKER_STOPPED: &str = "the bar search stopped unexpectedly";

/// Spawns the pre-pass thread for `job`.
pub fn spawn_prepass(job: PrepassJob) -> PrepassHandle {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    thread::spawn(move || {
        let mut decoder = SeekingFrameDecoder::new(
            job.video_path,
            job.width,
            job.height,
            job.fps_num,
            job.fps_den,
        );
        run_worker(
            &mut decoder,
            job.frame_count,
            DEFAULT_PREPASS_STRIDE,
            &worker_cancel,
            &tx,
        );
    });
    PrepassHandle {
        messages: rx,
        cancel,
    }
}

/// The worker body: runs the pre-pass and reports through `tx`. A panic in
/// the decoder or plate finder is caught and reported as an error (and
/// logged) rather than silently killing the thread.
fn run_worker<D>(
    decoder: &mut D,
    frame_count: u64,
    stride: u64,
    cancel: &AtomicBool,
    tx: &Sender<PrepassMessage>,
) where
    D: FrameDecoder<Frame = Frame>,
    D::Error: std::fmt::Display,
{
    let mut last_percent = None;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_prepass(decoder, frame_count, stride, cancel, |done, total| {
            let percent = (done * 100).checked_div(total);
            if percent != last_percent {
                last_percent = percent;
                let _ = tx.send(PrepassMessage::Progress { done, total });
            }
        })
    }));
    let result = match outcome {
        Ok(result) => result,
        Err(payload) => {
            let detail = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            tracing::error!(detail, "pre-pass worker panicked");
            Err(WORKER_STOPPED.to_string())
        }
    };
    if cancel.load(Ordering::Relaxed) {
        return;
    }
    let _ = tx.send(PrepassMessage::Done(result));
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Panicking;
    impl FrameDecoder for Panicking {
        type Error = String;
        type Frame = Frame;
        fn decode_frame(&mut self, _index: u64) -> Result<Frame, String> {
            panic!("decoder exploded");
        }
    }

    #[test]
    fn a_panicking_decoder_surfaces_as_an_error_not_a_hang() {
        let (tx, rx) = mpsc::channel();
        let cancel = AtomicBool::new(false);
        run_worker(&mut Panicking, 8, 4, &cancel, &tx);
        drop(tx);
        let handle = PrepassHandle::new(rx, Arc::default());
        let msgs = handle.drain();
        assert!(
            matches!(msgs.last(), Some(PrepassMessage::Done(Err(e))) if e == WORKER_STOPPED),
            "{msgs:?}"
        );
    }

    #[test]
    fn normal_completion_does_not_add_a_second_done() {
        let (tx, rx) = mpsc::channel();
        tx.send(PrepassMessage::Done(Ok(None))).unwrap();
        drop(tx);
        let handle = PrepassHandle::new(rx, Arc::default());
        assert_eq!(handle.drain(), vec![PrepassMessage::Done(Ok(None))]);
    }

    #[test]
    fn a_cancelled_handle_does_not_report_a_failure() {
        let (tx, rx) = mpsc::channel::<PrepassMessage>();
        drop(tx);
        let handle = PrepassHandle::new(rx, Arc::default());
        handle.cancel();
        assert!(handle.drain().is_empty());
    }
}
