//! Random-access single-frame decode via `ffmpeg -ss ... -vframes 1`
//! (task 2.3). Implements `frame_cache::FrameDecoder` so it can sit behind
//! the LRU cache: each call spawns a short-lived ffmpeg process seeking to
//! the requested frame's timestamp and decoding exactly one frame.
//!
//! This trades per-seek latency (process spawn + input-side seek) for O(1)
//! memory regardless of video length, which is the right tradeoff for a
//! scrub bar over a ~2000 frame video: see `frame_cache.rs` for the memory
//! math that ruled out full-video caching.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use tracker_core::Frame;

use crate::frame_cache::FrameDecoder;

/// Everything that can go wrong seeking to and decoding a single frame.
#[derive(Debug)]
pub enum SeekError {
    FfmpegNotFound,
    Io(std::io::Error),
    /// ffmpeg exited non-zero.
    ProcessFailed {
        stderr: String,
    },
    /// ffmpeg produced no frame at all (e.g. seek past end of stream).
    NoFrameDecoded,
    /// ffmpeg produced fewer bytes than one full frame.
    ShortRead {
        expected: usize,
        got: usize,
    },
}

impl std::fmt::Display for SeekError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SeekError::FfmpegNotFound => write!(f, "ffmpeg not found on PATH; install ffmpeg"),
            SeekError::Io(e) => write!(f, "I/O error decoding frame: {e}"),
            SeekError::ProcessFailed { stderr } => {
                write!(f, "ffmpeg exited with an error: {stderr}")
            }
            SeekError::NoFrameDecoded => {
                write!(f, "ffmpeg produced no frame for the requested seek")
            }
            SeekError::ShortRead { expected, got } => write!(
                f,
                "ffmpeg produced a partial frame: expected {expected} bytes, got {got}"
            ),
        }
    }
}

impl std::error::Error for SeekError {}

impl From<std::io::Error> for SeekError {
    fn from(e: std::io::Error) -> Self {
        SeekError::Io(e)
    }
}

/// Decodes single frames from a video file by index, seeking with ffmpeg's
/// `-ss` on each call. `fps` (as a rational) converts a frame index into a
/// seek timestamp in seconds.
pub struct SeekingFrameDecoder {
    path: PathBuf,
    width: u32,
    height: u32,
    fps_num: u64,
    fps_den: u64,
}

impl SeekingFrameDecoder {
    pub fn new(path: PathBuf, width: u32, height: u32, fps_num: u64, fps_den: u64) -> Self {
        Self {
            path,
            width,
            height,
            fps_num,
            fps_den,
        }
    }

    /// Seconds-since-start timestamp for the start of `frame_index`.
    fn seek_seconds(&self, frame_index: u64) -> f64 {
        if self.fps_num == 0 {
            return 0.0;
        }
        frame_index as f64 * self.fps_den as f64 / self.fps_num as f64
    }
}

impl FrameDecoder for SeekingFrameDecoder {
    type Error = SeekError;
    type Frame = Frame;

    fn decode_frame(&mut self, index: u64) -> Result<Frame, SeekError> {
        let ts = self.seek_seconds(index);
        let frame_len = self.width as usize * self.height as usize * 3;

        let mut child = Command::new("ffmpeg")
            .arg("-v")
            .arg("error")
            .arg("-ss")
            .arg(format!("{ts:.6}"))
            .arg("-i")
            .arg(&self.path)
            .arg("-frames:v")
            .arg("1")
            .arg("-f")
            .arg("rawvideo")
            .arg("-pix_fmt")
            .arg("rgb24")
            .arg("-")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| SeekError::FfmpegNotFound)?;

        let mut stdout = child.stdout.take().ok_or(SeekError::NoFrameDecoded)?;
        let mut buf = Vec::with_capacity(frame_len);
        stdout.read_to_end(&mut buf)?;

        let mut stderr_buf = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_string(&mut stderr_buf);
        }
        let status = child.wait()?;
        if !status.success() {
            return Err(SeekError::ProcessFailed { stderr: stderr_buf });
        }

        if buf.is_empty() {
            return Err(SeekError::NoFrameDecoded);
        }
        if buf.len() != frame_len {
            return Err(SeekError::ShortRead {
                expected: frame_len,
                got: buf.len(),
            });
        }

        Frame::new(self.width, self.height, buf).map_err(|_| SeekError::NoFrameDecoded)
        // buffer length already checked above
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seek_seconds_converts_frame_index_via_fps() {
        let dec = SeekingFrameDecoder::new(PathBuf::from("x.mp4"), 10, 10, 600, 19);
        // frame 19 at 600/19 fps -> 19 * 19/600 seconds
        let expected = 19.0 * 19.0 / 600.0;
        assert!((dec.seek_seconds(19) - expected).abs() < 1e-9);
    }

    #[test]
    fn seek_seconds_zero_fps_num_is_zero_not_panic() {
        let dec = SeekingFrameDecoder::new(PathBuf::from("x.mp4"), 10, 10, 0, 1);
        assert_eq!(dec.seek_seconds(5), 0.0);
    }

    #[test]
    #[ignore = "run manually: spawns real ffmpeg against test_videos/ (path contains spaces)"]
    fn decodes_a_real_frame_by_seek() {
        let path = PathBuf::from("../../test_videos/WhatsApp Video 2026-07-05 at 14.03.30.mp4");
        let mut dec = SeekingFrameDecoder::new(path, 1024, 576, 600, 19);
        let frame = dec.decode_frame(100).expect("should decode frame 100");
        assert_eq!(frame.width(), 1024);
        assert_eq!(frame.height(), 576);
    }
}
