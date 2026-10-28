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
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;

use tracker_core::SeedProposal;

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
    pub fn drain(&self) -> Vec<PrepassMessage> {
        self.messages.try_iter().collect()
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
        let mut last_percent = None;
        let result = run_prepass(
            &mut decoder,
            job.frame_count,
            DEFAULT_PREPASS_STRIDE,
            &worker_cancel,
            |done, total| {
                let percent = (done * 100).checked_div(total);
                if percent != last_percent {
                    last_percent = percent;
                    let _ = tx.send(PrepassMessage::Progress { done, total });
                }
            },
        );
        if worker_cancel.load(Ordering::Relaxed) {
            return;
        }
        let _ = tx.send(PrepassMessage::Done(result));
    });
    PrepassHandle {
        messages: rx,
        cancel,
    }
}
