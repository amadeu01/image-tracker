//! `FrameSource` port: a streaming source of decoded video frames.
//!
//! Dependency-free (no IO) — the adapter that spawns `ffmpeg` and speaks
//! this trait lives in `tracker-app` (task 2.2). Frames are streamed one at
//! a time rather than preloaded: a typical lift video is ~3800 frames of
//! 1024x576x3 bytes, too much to comfortably hold in memory at once.

use crate::geometry::Frame;

/// A streaming source of decoded frames.
///
/// `next_frame` returns `Ok(Some(frame))` for each frame in order,
/// `Ok(None)` once the source is exhausted (clean end), and `Err` if
/// decoding fails partway through. Implementations should not be assumed
/// `Send`/`Sync`/`Clone`; callers own a single mutable pass over the frames.
pub trait FrameSource {
    type Error;

    /// Advance to and return the next frame, or `None` at clean end-of-stream.
    fn next_frame(&mut self) -> Result<Option<Frame>, Self::Error>;
}
