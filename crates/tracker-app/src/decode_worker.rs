//! Async frame-decode worker (task 18.1): removes the synchronous
//! `SeekingFrameDecoder::decode_frame` (ffmpeg spawn + `child.wait()`, see
//! `seek_source.rs`) from the eframe UI thread. `docs/gui-threading.md`
//! finding G1: `TrackerApp::ensure_texture` used to call
//! `FrameCache::get(current_frame)` directly from `update`; on a miss that
//! blocked the winit event loop for a full ffmpeg seek+decode, and during
//! tracking (where `current_frame` advances every processed frame) that
//! happened on nearly every rendered frame — the "application is not
//! responding" freeze.
//!
//! Mirrors `thumbnail_worker.rs`/`tracking.rs`'s spawn-thread-plus-mpsc
//! shape: the UI sends "I want frame N" over an unbounded channel, the
//! worker decodes (through its own `FrameCache`, so repeat/nearby frames
//! stay O(1) instead of a fresh ffmpeg spawn — same LRU this replaces, just
//! moved off the UI thread) and sends the result back. `update` drains
//! replies with `try_recv` and uploads the texture there, same as before.
//!
//! Coalescing: the UI can want frames faster than ffmpeg can decode them —
//! a fast scrub, or `poll_tracking` advancing `current_frame` every
//! processed frame during a run. The worker drains every queued want
//! request before decoding and keeps only the most recent one; stale
//! requests are dropped rather than decoded and discarded, so the worker
//! never falls further behind than a single decode.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

use crate::frame_cache::{FrameCache, FrameDecoder};

/// A reply from the decode worker to the UI thread.
#[derive(Debug, Clone)]
pub enum DecodeMessage<F> {
    /// `frame_index` decoded successfully.
    Decoded { frame_index: u64, frame: F },
    /// `frame_index` failed to decode (ffmpeg error, out-of-bounds seek,
    /// etc). `message` is `D::Error`'s `Display` rendering, already turned
    /// to a `String` on the worker thread so the UI thread doesn't need to
    /// know the decoder's concrete error type.
    Error { frame_index: u64, message: String },
}

/// UI-thread handle: send "want frame N" with `want`, drain replies from
/// `results` with `try_recv` (never `recv` — see `docs/gui-threading.md`
/// rule 2).
///
/// `Drop` just drops `want_tx`: the worker's next `want_rx.recv()` returns
/// `Err` and the thread exits on its own. Unlike `tracking.rs`'s streaming
/// decoder, there is no long-lived child process to kill here — the
/// worker's `SeekingFrameDecoder` spawns and `wait()`s a short-lived ffmpeg
/// per decode and never holds one open between requests, so dropping the
/// channel while idle (or mid-decode, which simply finishes before the next
/// `recv` sees the closed channel) can't deadlock the way reaping a
/// still-running streaming child can.
pub struct DecodeHandle<F> {
    want_tx: Sender<u64>,
    pub results: Receiver<DecodeMessage<F>>,
}

impl<F> DecodeHandle<F> {
    /// Requests frame `index`. Non-blocking; silently dropped if the worker
    /// has already exited (mirrors `TrackingHandle::resume`'s rationale —
    /// the UI will already be showing whatever the last reply said).
    pub fn want(&self, index: u64) {
        let _ = self.want_tx.send(index);
    }
}

/// Spawns the decode worker thread, wrapping `decoder` in its own
/// `FrameCache` (capacity `cache_capacity`) so the worker thread — never the
/// UI thread — is the sole owner/caller of the decoder.
pub fn spawn_decode_worker<D>(decoder: D, cache_capacity: usize) -> DecodeHandle<D::Frame>
where
    D: FrameDecoder + Send + 'static,
    D::Frame: Clone + Send + 'static,
    D::Error: std::fmt::Display,
{
    let (want_tx, want_rx) = mpsc::channel::<u64>();
    let (results_tx, results_rx) = mpsc::channel::<DecodeMessage<D::Frame>>();

    thread::spawn(move || {
        let mut cache = FrameCache::new(decoder, cache_capacity);
        run_decode_worker(&mut cache, &want_rx, &results_tx);
    });

    DecodeHandle {
        want_tx,
        results: results_rx,
    }
}

/// The worker loop, generic over `FrameCache<D>` so it's driveable in tests
/// without a real ffmpeg process (see `tests` below) — mirrors
/// `tracking.rs`'s `run_tracking_loop` being generic over `FrameSource` for
/// the same reason.
///
/// Blocks on the first `want_rx.recv()` (nothing to do until a request
/// arrives — a real block is fine here, this runs on the worker thread, not
/// `update`), then drains every additional already-queued request with
/// `try_recv` before deciding what to decode: only the *last* queued index
/// is decoded, coalescing a burst of requests into one decode instead of
/// working through the backlog one-decode-per-request and falling further
/// and further behind the UI.
fn run_decode_worker<D>(
    cache: &mut FrameCache<D>,
    want_rx: &Receiver<u64>,
    results_tx: &Sender<DecodeMessage<D::Frame>>,
) where
    D: FrameDecoder,
    D::Frame: Clone,
    D::Error: std::fmt::Display,
{
    loop {
        let mut wanted = match want_rx.recv() {
            Ok(idx) => idx,
            Err(_) => return, // UI dropped the handle: nothing left to do
        };
        loop {
            match want_rx.try_recv() {
                Ok(idx) => wanted = idx,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        let msg = match cache.get(wanted) {
            Ok(frame) => DecodeMessage::Decoded {
                frame_index: wanted,
                frame,
            },
            Err(e) => DecodeMessage::Error {
                frame_index: wanted,
                message: e.to_string(),
            },
        };
        if results_tx.send(msg).is_err() {
            return; // UI dropped the handle mid-decode
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    /// Fake decoder recording call counts per index via shared interior
    /// mutability, so a test can inspect call counts after the decoder has
    /// been moved into a `FrameCache`. `fail_on` lets a test exercise the
    /// `Error` reply path without a real ffmpeg failure.
    #[derive(Clone)]
    struct CountingDecoder {
        calls: Rc<RefCell<HashMap<u64, u32>>>,
        fail_on: Option<u64>,
    }

    impl CountingDecoder {
        fn new() -> Self {
            Self {
                calls: Rc::new(RefCell::new(HashMap::new())),
                fail_on: None,
            }
        }

        fn failing_on(index: u64) -> Self {
            Self {
                fail_on: Some(index),
                ..Self::new()
            }
        }

        fn call_count(&self, index: u64) -> u32 {
            *self.calls.borrow().get(&index).unwrap_or(&0)
        }
    }

    impl FrameDecoder for CountingDecoder {
        type Error = String;
        type Frame = u64; // stand-in "frame": the index itself

        fn decode_frame(&mut self, index: u64) -> Result<u64, String> {
            *self.calls.borrow_mut().entry(index).or_insert(0) += 1;
            if self.fail_on == Some(index) {
                return Err(format!("decode failed for frame {index}"));
            }
            Ok(index)
        }
    }

    /// A burst of requests queued before the worker ever looks at the
    /// channel (exactly what a fast scrub or `poll_tracking` produces
    /// faster than ffmpeg decodes) must coalesce to a single decode of the
    /// *last* one queued — this is the core of the fix: the worker never
    /// works through a backlog one decode at a time.
    #[test]
    fn coalesces_a_burst_of_queued_requests_to_only_the_latest() {
        let decoder = CountingDecoder::new();
        let mut cache = FrameCache::new(decoder.clone(), 16);
        let (want_tx, want_rx) = mpsc::channel::<u64>();
        let (results_tx, results_rx) = mpsc::channel::<DecodeMessage<u64>>();

        for idx in [1, 2, 3, 5, 10] {
            want_tx.send(idx).unwrap();
        }
        drop(want_tx); // closes the channel once drained -> worker returns

        run_decode_worker(&mut cache, &want_rx, &results_tx);

        let messages: Vec<_> = results_rx.try_iter().collect();
        assert_eq!(messages.len(), 1, "one decode for the whole burst");
        match &messages[0] {
            DecodeMessage::Decoded { frame_index, frame } => {
                assert_eq!(*frame_index, 10);
                assert_eq!(*frame, 10);
            }
            other => panic!("expected Decoded, got {other:?}"),
        }
        for skipped in [1, 2, 3, 5] {
            assert_eq!(
                decoder.call_count(skipped),
                0,
                "frame {skipped} was superseded before the worker ever decoded it"
            );
        }
        assert_eq!(decoder.call_count(10), 1);
    }

    /// A single request round-trips as `Decoded` with the right index.
    #[test]
    fn single_request_round_trips_as_decoded() {
        let mut cache = FrameCache::new(CountingDecoder::new(), 16);
        let (want_tx, want_rx) = mpsc::channel::<u64>();
        let (results_tx, results_rx) = mpsc::channel::<DecodeMessage<u64>>();
        want_tx.send(7).unwrap();
        drop(want_tx);

        run_decode_worker(&mut cache, &want_rx, &results_tx);

        let msg = results_rx.try_recv().unwrap();
        match msg {
            DecodeMessage::Decoded { frame_index, frame } => {
                assert_eq!(frame_index, 7);
                assert_eq!(frame, 7);
            }
            other => panic!("expected Decoded, got {other:?}"),
        }
    }

    /// A decode failure is reported as `Error`, not silently dropped or
    /// panicking the worker thread.
    #[test]
    fn decode_failure_is_reported_as_error() {
        let mut cache = FrameCache::new(CountingDecoder::failing_on(4), 16);
        let (want_tx, want_rx) = mpsc::channel::<u64>();
        let (results_tx, results_rx) = mpsc::channel::<DecodeMessage<u64>>();
        want_tx.send(4).unwrap();
        drop(want_tx);

        run_decode_worker(&mut cache, &want_rx, &results_tx);

        let msg = results_rx.try_recv().unwrap();
        match msg {
            DecodeMessage::Error {
                frame_index,
                message,
            } => {
                assert_eq!(frame_index, 4);
                assert!(message.contains('4'));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// Repeated requests for the same frame across separate batches are a
    /// cache hit on the second round, not a re-decode — the worker's
    /// internal `FrameCache` is doing its job exactly like it did on the UI
    /// thread before this task.
    #[test]
    fn repeated_requests_for_the_same_frame_hit_the_cache() {
        let decoder = CountingDecoder::new();
        let mut cache = FrameCache::new(decoder.clone(), 16);
        let (results_tx, results_rx) = mpsc::channel::<DecodeMessage<u64>>();

        for _ in 0..2 {
            let (want_tx, want_rx) = mpsc::channel::<u64>();
            want_tx.send(3).unwrap();
            drop(want_tx);
            run_decode_worker(&mut cache, &want_rx, &results_tx);
        }

        assert_eq!(results_rx.try_iter().count(), 2);
        assert_eq!(decoder.call_count(3), 1, "second round was a cache hit");
    }

    /// A fake decoder that sleeps briefly, `Send` so it can cross into a
    /// real spawned thread — for the one true end-to-end test of
    /// `spawn_decode_worker`/`DecodeHandle`, as opposed to the synchronous
    /// `run_decode_worker` unit tests above.
    struct SlowDecoder {
        calls: Arc<Mutex<u32>>,
    }

    impl FrameDecoder for SlowDecoder {
        type Error = String;
        type Frame = u64;

        fn decode_frame(&mut self, index: u64) -> Result<u64, String> {
            *self.calls.lock().unwrap() += 1;
            std::thread::sleep(std::time::Duration::from_millis(5));
            Ok(index)
        }
    }

    /// End-to-end smoke over the real spawned thread + channels: a request
    /// eventually gets a reply, and the handle can be dropped (idle or not)
    /// without hanging the test process — the `Drop`/teardown story this
    /// task requires (18.1 point 5).
    #[test]
    fn spawn_decode_worker_round_trips_over_a_real_thread() {
        let calls = Arc::new(Mutex::new(0));
        let decoder = SlowDecoder {
            calls: calls.clone(),
        };
        let handle = spawn_decode_worker(decoder, 4);
        handle.want(9);

        let msg = handle
            .results
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("worker should reply within 2s");
        match msg {
            DecodeMessage::Decoded { frame_index, frame } => {
                assert_eq!(frame_index, 9);
                assert_eq!(frame, 9);
            }
            other => panic!("expected Decoded, got {other:?}"),
        }
        drop(handle); // must not hang: no child process, thread just exits
    }
}
