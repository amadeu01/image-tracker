//! `VideoSink` port: a streaming destination for encoded video frames.
//!
//! Dependency-free (no IO) — the adapter that spawns `ffmpeg` and speaks
//! this trait lives in `tracker-app` (task 3.2), mirroring `FrameSource`
//! (task 2.2) on the decode side.

use crate::geometry::Frame;

/// A streaming sink that consumes frames and finalizes them into an
/// encoded video file.
///
/// `write_frame` is called once per frame, in order. `finish` must be
/// called exactly once after the last frame to flush/finalize the output;
/// implementations should treat writing after `finish` (or dropping
/// without calling it) as a caller bug, not something to silently paper
/// over.
pub trait VideoSink {
    type Error;

    /// Encode one frame. All frames passed to a given sink must share the
    /// dimensions the sink was constructed with; implementations should
    /// reject a mismatch rather than write garbage.
    fn write_frame(&mut self, frame: &Frame) -> Result<(), Self::Error>;

    /// Finalize the output (flush buffers, close the file/stream). Consumes
    /// the sink so it cannot be written to afterward.
    fn finish(self) -> Result<(), Self::Error>;
}
