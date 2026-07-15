//! ffmpeg decode adapter (task 2.2): `FrameSource` port implementation.
//!
//! Spawns `ffmpeg -f rawvideo -pix_fmt rgb24 -` and streams frames from its
//! stdout, one `width * height * 3` chunk at a time. See ADR 0001 — video
//! IO shells out to the ffmpeg binary rather than linking a decode library.
//!
//! The chunk-reading logic is factored out as `FfmpegFrameSource<R>`,
//! generic over any `io::Read`, so it is unit-testable against an in-memory
//! buffer without spawning a real process.

use std::io::{self, Read};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};

use tracker_core::{Frame, FrameSource};

/// Everything that can go wrong decoding a video via ffmpeg.
#[derive(Debug)]
pub enum FfmpegDecodeError {
    /// `ffmpeg` is not on PATH (or otherwise failed to spawn).
    FfmpegNotFound,
    /// Failed to capture the child process's stdout pipe.
    NoStdout,
    /// An I/O error occurred while reading frame bytes from the pipe.
    Io(io::Error),
    /// The stream ended partway through a frame (fewer than
    /// `width * height * 3` bytes available before EOF). Treated as an
    /// error rather than silently dropped, since it usually means ffmpeg
    /// was killed or the source file is truncated/corrupt.
    ShortRead { expected: usize, got: usize },
    /// The child process exited with a non-zero status.
    ProcessFailed { stderr: String },
}

impl std::fmt::Display for FfmpegDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FfmpegDecodeError::FfmpegNotFound => {
                write!(f, "ffmpeg not found on PATH; install ffmpeg")
            }
            FfmpegDecodeError::NoStdout => {
                write!(f, "failed to capture ffmpeg stdout pipe")
            }
            FfmpegDecodeError::Io(e) => write!(f, "I/O error reading ffmpeg output: {e}"),
            FfmpegDecodeError::ShortRead { expected, got } => write!(
                f,
                "ffmpeg output ended mid-frame: expected {expected} bytes, got {got}"
            ),
            FfmpegDecodeError::ProcessFailed { stderr } => {
                write!(f, "ffmpeg exited with an error: {stderr}")
            }
        }
    }
}

impl std::error::Error for FfmpegDecodeError {}

impl From<io::Error> for FfmpegDecodeError {
    fn from(e: io::Error) -> Self {
        FfmpegDecodeError::Io(e)
    }
}

/// Reads a stream of raw RGB24 frames of fixed `width x height` from any
/// `io::Read`, yielding `tracker_core::Frame`s via the `FrameSource` port.
pub struct FfmpegFrameSource<R: Read> {
    reader: R,
    width: u32,
    height: u32,
    frame_len: usize,
    /// Held only to keep the child alive and reap/kill it on drop; `None`
    /// when constructed directly from an in-memory reader (tests).
    child: Option<Child>,
}

impl<R: Read> FfmpegFrameSource<R> {
    /// Wrap an arbitrary reader as a rawvideo RGB24 frame stream. Used
    /// directly by tests (e.g. with `io::Cursor`); production code goes
    /// through `FfmpegFrameSource::spawn`.
    pub fn from_reader(reader: R, width: u32, height: u32) -> Self {
        Self {
            reader,
            width,
            height,
            frame_len: width as usize * height as usize * 3,
            child: None,
        }
    }

    /// Reads exactly `frame_len` bytes, distinguishing a clean end-of-stream
    /// (zero bytes read before hitting EOF) from a short/partial read.
    fn read_frame_bytes(&mut self) -> Result<Option<Vec<u8>>, FfmpegDecodeError> {
        let mut buf = vec![0u8; self.frame_len];
        let mut filled = 0;
        while filled < self.frame_len {
            let n = self.reader.read(&mut buf[filled..])?;
            if n == 0 {
                if filled == 0 {
                    return Ok(None); // clean EOF, no partial frame started
                }
                return Err(FfmpegDecodeError::ShortRead {
                    expected: self.frame_len,
                    got: filled,
                });
            }
            filled += n;
        }
        Ok(Some(buf))
    }
}

impl FfmpegFrameSource<ChildStdout> {
    /// Spawns `ffmpeg -v error -i <path> -f rawvideo -pix_fmt rgb24 -` and
    /// streams frames from its stdout. `width`/`height` come from the
    /// ffprobe adapter (task 2.1) — rawvideo has no dimension header, so the
    /// caller must supply them.
    pub fn spawn(path: &Path, width: u32, height: u32) -> Result<Self, FfmpegDecodeError> {
        tracing::debug!(
            video = %path.display(),
            width,
            height,
            "spawning ffmpeg decode process"
        );
        let mut child = Command::new("ffmpeg")
            .arg("-v")
            .arg("error")
            .arg("-i")
            .arg(path)
            .arg("-f")
            .arg("rawvideo")
            .arg("-pix_fmt")
            .arg("rgb24")
            .arg("-")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| FfmpegDecodeError::FfmpegNotFound)?;

        let stdout = child.stdout.take().ok_or(FfmpegDecodeError::NoStdout)?;

        Ok(Self {
            reader: stdout,
            width,
            height,
            frame_len: width as usize * height as usize * 3,
            child: Some(child),
        })
    }

    /// Waits for the child to exit and surfaces a non-zero status as an
    /// error with captured stderr. Called automatically once `next_frame`
    /// observes clean EOF.
    fn reap(&mut self) -> Result<(), FfmpegDecodeError> {
        if let Some(mut child) = self.child.take() {
            // stdout was already taken/consumed; stderr may still be open.
            let mut stderr_buf = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                let _ = stderr.read_to_string(&mut stderr_buf);
            }
            let status = child.wait()?;
            if !status.success() {
                tracing::error!(stderr = %stderr_buf, "ffmpeg decode process exited with an error");
                return Err(FfmpegDecodeError::ProcessFailed { stderr: stderr_buf });
            }
        }
        Ok(())
    }
}

impl<R: Read> FrameSource for FfmpegFrameSource<R> {
    type Error = FfmpegDecodeError;

    fn next_frame(&mut self) -> Result<Option<Frame>, Self::Error> {
        match self.read_frame_bytes()? {
            Some(bytes) => {
                let frame = Frame::new(self.width, self.height, bytes)
                    // Buffer length is exactly frame_len by construction; a
                    // mismatch here would be a bug in this adapter, not a
                    // recoverable runtime condition.
                    .unwrap_or_else(|e| unreachable!("frame_len invariant violated: {e}"));
                Ok(Some(frame))
            }
            None => Ok(None),
        }
    }
}

impl FfmpegFrameSource<ChildStdout> {
    /// Like `next_frame`, but on clean EOF also reaps the child process and
    /// surfaces a non-zero exit as an error. Prefer this over the trait
    /// method when driving a spawned process to completion.
    pub fn next_frame_checked(&mut self) -> Result<Option<Frame>, FfmpegDecodeError> {
        match self.read_frame_bytes()? {
            Some(bytes) => Ok(Some(
                Frame::new(self.width, self.height, bytes)
                    .unwrap_or_else(|e| unreachable!("frame_len invariant violated: {e}")),
            )),
            None => {
                self.reap()?;
                Ok(None)
            }
        }
    }
}

impl<R: Read> Drop for FfmpegFrameSource<R> {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn synthetic_frame_bytes(width: u32, height: u32, fill: u8) -> Vec<u8> {
        vec![fill; width as usize * height as usize * 3]
    }

    #[test]
    fn yields_frames_with_correct_dimensions_from_in_memory_reader() {
        let width = 4;
        let height = 3;
        let mut data = synthetic_frame_bytes(width, height, 10);
        data.extend(synthetic_frame_bytes(width, height, 20));

        let mut source = FfmpegFrameSource::from_reader(Cursor::new(data), width, height);

        let f1 = source.next_frame().unwrap().expect("frame 1");
        assert_eq!(f1.width(), width);
        assert_eq!(f1.height(), height);
        assert_eq!(f1.pixel(0, 0), Some([10, 10, 10]));

        let f2 = source.next_frame().unwrap().expect("frame 2");
        assert_eq!(f2.pixel(0, 0), Some([20, 20, 20]));

        assert!(source.next_frame().unwrap().is_none());
        // Exhausted source stays exhausted (idempotent clean EOF).
        assert!(source.next_frame().unwrap().is_none());
    }

    #[test]
    fn clean_eof_between_frames_is_none_not_error() {
        let width = 2;
        let height = 2;
        let data = synthetic_frame_bytes(width, height, 5); // exactly one frame

        let mut source = FfmpegFrameSource::from_reader(Cursor::new(data), width, height);
        assert!(source.next_frame().unwrap().is_some());
        assert!(source.next_frame().unwrap().is_none());
    }

    #[test]
    fn partial_trailing_bytes_is_short_read_error() {
        let width = 4;
        let height = 4;
        let mut data = synthetic_frame_bytes(width, height, 1);
        data.truncate(data.len() - 5); // chop off the tail of the last frame

        let mut source = FfmpegFrameSource::from_reader(Cursor::new(data), width, height);
        let err = source.next_frame().unwrap_err();
        match err {
            FfmpegDecodeError::ShortRead { expected, got } => {
                assert_eq!(expected, width as usize * height as usize * 3);
                assert_eq!(got, expected - 5);
            }
            other => panic!("expected ShortRead, got {other:?}"),
        }
    }

    #[test]
    fn reads_frames_split_across_multiple_small_reads() {
        // A reader that only ever returns a handful of bytes per `read`
        // call, to exercise the fill loop against short reads that are
        // still eventually complete.
        struct Trickle {
            data: Vec<u8>,
            pos: usize,
        }
        impl Read for Trickle {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let remaining = self.data.len() - self.pos;
                let n = remaining.min(buf.len()).min(3);
                buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
                self.pos += n;
                Ok(n)
            }
        }

        let width = 3;
        let height = 2;
        let data = synthetic_frame_bytes(width, height, 7);
        let trickle = Trickle { data, pos: 0 };

        let mut source = FfmpegFrameSource::from_reader(trickle, width, height);
        let frame = source.next_frame().unwrap().expect("frame");
        assert_eq!(frame.pixel(width - 1, height - 1), Some([7, 7, 7]));
        assert!(source.next_frame().unwrap().is_none());
    }

    #[test]
    #[ignore = "run manually: spawns real ffmpeg against test_videos/ (path contains spaces)"]
    fn decodes_first_ten_frames_of_real_test_video() {
        let path = Path::new("../../test_videos/WhatsApp Video 2026-07-05 at 14.03.30.mp4");
        // Dimensions from the 2.1 ffprobe integration test on this file.
        let mut source = FfmpegFrameSource::spawn(path, 1024, 576).expect("ffmpeg should spawn");

        for i in 0..10 {
            let frame = source
                .next_frame_checked()
                .unwrap_or_else(|e| panic!("frame {i} failed: {e}"))
                .unwrap_or_else(|| panic!("frame {i} missing: unexpected EOF"));
            assert_eq!(frame.width(), 1024);
            assert_eq!(frame.height(), 576);
        }
    }
}
