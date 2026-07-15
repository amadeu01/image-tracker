//! ffmpeg encode adapter (task 3.2): `VideoSink` port implementation.
//!
//! Spawns `ffmpeg -f rawvideo -pix_fmt rgb24 -i - ... -c:v libx264 <out.mp4>`
//! and streams frames to its stdin, one `width * height * 3` chunk at a
//! time. See ADR 0001 — video IO shells out to the ffmpeg binary rather
//! than linking an encode library.
//!
//! The writer logic is factored out as `FfmpegVideoSink<W>`, generic over
//! any `io::Write`, so it is unit-testable against an in-memory buffer
//! without spawning a real process (mirrors `FfmpegFrameSource<R>` in
//! `ffmpeg_source.rs` on the decode side).

use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};

use tracker_core::{Frame, VideoSink};

/// Everything that can go wrong encoding a video via ffmpeg.
#[derive(Debug)]
pub enum FfmpegEncodeError {
    /// `ffmpeg` is not on PATH (or otherwise failed to spawn).
    FfmpegNotFound,
    /// Failed to capture the child process's stdin pipe.
    NoStdin,
    /// An I/O error occurred while writing frame bytes to the pipe.
    Io(io::Error),
    /// A frame's dimensions didn't match the sink's configured width/height.
    DimensionMismatch {
        expected_width: u32,
        expected_height: u32,
        actual_width: u32,
        actual_height: u32,
    },
    /// The child process exited with a non-zero status.
    ProcessFailed { stderr: String },
}

impl std::fmt::Display for FfmpegEncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FfmpegEncodeError::FfmpegNotFound => {
                write!(f, "ffmpeg not found on PATH; install ffmpeg")
            }
            FfmpegEncodeError::NoStdin => {
                write!(f, "failed to capture ffmpeg stdin pipe")
            }
            FfmpegEncodeError::Io(e) => write!(f, "I/O error writing ffmpeg input: {e}"),
            FfmpegEncodeError::DimensionMismatch {
                expected_width,
                expected_height,
                actual_width,
                actual_height,
            } => write!(
                f,
                "frame dimensions {actual_width}x{actual_height} do not match sink dimensions {expected_width}x{expected_height}"
            ),
            FfmpegEncodeError::ProcessFailed { stderr } => {
                write!(f, "ffmpeg exited with an error: {stderr}")
            }
        }
    }
}

impl std::error::Error for FfmpegEncodeError {}

impl From<io::Error> for FfmpegEncodeError {
    fn from(e: io::Error) -> Self {
        FfmpegEncodeError::Io(e)
    }
}

/// Writes a stream of raw RGB24 frames of fixed `width x height` to any
/// `io::Write`, implementing the `VideoSink` port.
pub struct FfmpegVideoSink<W: Write> {
    writer: Option<W>,
    width: u32,
    height: u32,
    /// Held only to close stdin and reap/kill the child; `None` when
    /// constructed directly from an in-memory writer (tests).
    child: Option<Child>,
}

impl<W: Write> FfmpegVideoSink<W> {
    /// Wrap an arbitrary writer as a rawvideo RGB24 frame sink. Used
    /// directly by tests (e.g. with `Vec<u8>`); production code goes
    /// through `FfmpegVideoSink::spawn`.
    pub fn from_writer(writer: W, width: u32, height: u32) -> Self {
        Self {
            writer: Some(writer),
            width,
            height,
            child: None,
        }
    }

    fn write_frame_bytes(&mut self, frame: &Frame) -> Result<(), FfmpegEncodeError> {
        if frame.width() != self.width || frame.height() != self.height {
            return Err(FfmpegEncodeError::DimensionMismatch {
                expected_width: self.width,
                expected_height: self.height,
                actual_width: frame.width(),
                actual_height: frame.height(),
            });
        }
        let writer = self
            .writer
            .as_mut()
            .expect("writer only taken on finish, which consumes self");
        writer.write_all(frame.rgb())?;
        Ok(())
    }
}

impl FfmpegVideoSink<ChildStdin> {
    /// Spawns `ffmpeg -v error -f rawvideo -pix_fmt rgb24 -s WxH -r <fps>
    /// -i - -c:v libx264 -pix_fmt yuv420p -movflags +faststart <out>` and
    /// streams frames to its stdin.
    ///
    /// `fps_num`/`fps_den` are passed through as a rational (`"num/den"`)
    /// rather than collapsed to a float, since source videos can have odd
    /// rates (e.g. 600/19) that don't round-trip cleanly through decimal.
    pub fn spawn(
        out_path: &Path,
        width: u32,
        height: u32,
        fps_num: u64,
        fps_den: u64,
    ) -> Result<Self, FfmpegEncodeError> {
        let size = format!("{width}x{height}");
        let fps = format!("{fps_num}/{fps_den}");

        let mut child = Command::new("ffmpeg")
            .arg("-v")
            .arg("error")
            .arg("-y")
            .arg("-f")
            .arg("rawvideo")
            .arg("-pix_fmt")
            .arg("rgb24")
            .arg("-s")
            .arg(&size)
            .arg("-r")
            .arg(&fps)
            .arg("-i")
            .arg("-")
            .arg("-c:v")
            .arg("libx264")
            .arg("-pix_fmt")
            .arg("yuv420p")
            .arg("-movflags")
            .arg("+faststart")
            .arg(out_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| FfmpegEncodeError::FfmpegNotFound)?;

        let stdin = child.stdin.take().ok_or(FfmpegEncodeError::NoStdin)?;

        Ok(Self {
            writer: Some(stdin),
            width,
            height,
            child: Some(child),
        })
    }
}

impl<W: Write> VideoSink for FfmpegVideoSink<W> {
    type Error = FfmpegEncodeError;

    fn write_frame(&mut self, frame: &Frame) -> Result<(), Self::Error> {
        self.write_frame_bytes(frame)
    }

    fn finish(mut self) -> Result<(), Self::Error> {
        // Drop the writer to close stdin (EOF signal to ffmpeg), then, if
        // this is a real spawned process, wait for it to finish encoding
        // and surface a non-zero exit.
        self.writer.take();

        if let Some(mut child) = self.child.take() {
            let mut stderr_buf = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                let _ = stderr.read_to_string(&mut stderr_buf);
            }
            let status = child.wait()?;
            if !status.success() {
                return Err(FfmpegEncodeError::ProcessFailed { stderr: stderr_buf });
            }
        }
        Ok(())
    }
}

impl<W: Write> Drop for FfmpegVideoSink<W> {
    /// If `finish` was never called (e.g. an error path unwound early),
    /// kill the child rather than leave a dangling ffmpeg process waiting
    /// on stdin that will never come.
    fn drop(&mut self) {
        self.writer.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_frame(width: u32, height: u32, fill: u8) -> Frame {
        Frame::new(width, height, vec![fill; width as usize * height as usize * 3]).unwrap()
    }

    #[test]
    fn buffer_contains_exact_bytes_for_each_frame() {
        let mut sink = FfmpegVideoSink::from_writer(Vec::new(), 2, 1);
        sink.write_frame(&synthetic_frame(2, 1, 9)).unwrap();
        sink.write_frame(&synthetic_frame(2, 1, 8)).unwrap();

        // Peek at the writer before finish consumes self.
        let written = sink.writer.as_ref().unwrap().clone();
        assert_eq!(written.len(), 2 * (2 * 1 * 3));
        assert_eq!(&written[..6], &[9, 9, 9, 9, 9, 9]);
        assert_eq!(&written[6..], &[8, 8, 8, 8, 8, 8]);

        sink.finish().unwrap();
    }

    #[test]
    fn dimension_mismatch_is_rejected() {
        let mut sink = FfmpegVideoSink::from_writer(Vec::new(), 4, 3);
        let wrong = synthetic_frame(2, 2, 0);
        let err = sink.write_frame(&wrong).unwrap_err();
        match err {
            FfmpegEncodeError::DimensionMismatch {
                expected_width,
                expected_height,
                actual_width,
                actual_height,
            } => {
                assert_eq!((expected_width, expected_height), (4, 3));
                assert_eq!((actual_width, actual_height), (2, 2));
            }
            other => panic!("expected DimensionMismatch, got {other:?}"),
        }
    }

    #[test]
    fn finish_with_no_frames_succeeds() {
        let sink = FfmpegVideoSink::from_writer(Vec::new(), 4, 3);
        sink.finish().unwrap();
    }

    #[test]
    #[ignore = "run manually: spawns real ffmpeg + ffprobe, encodes a synthetic MP4"]
    fn encodes_synthetic_frames_to_real_mp4_and_ffprobe_confirms_dims_and_count() {
        let width = 64u32;
        let height = 48u32;
        let frame_count = 30u32;
        let out_dir = std::env::temp_dir().join("tracker-app-ffmpeg-sink-test");
        std::fs::create_dir_all(&out_dir).unwrap();
        let out_path = out_dir.join("synthetic.mp4");

        let mut sink =
            FfmpegVideoSink::spawn(&out_path, width, height, 30, 1).expect("ffmpeg should spawn");

        for i in 0..frame_count {
            let mut rgb = vec![0u8; width as usize * height as usize * 3];
            // Moving square: a 8x8 block that shifts right each frame.
            let sq = 8usize;
            let x0 = (i as usize * 2) % (width as usize - sq);
            let y0 = height as usize / 2 - sq / 2;
            for y in y0..y0 + sq {
                for x in x0..x0 + sq {
                    let idx = (y * width as usize + x) * 3;
                    rgb[idx] = 255;
                    rgb[idx + 1] = 0;
                    rgb[idx + 2] = 0;
                }
            }
            let frame = Frame::new(width, height, rgb).unwrap();
            sink.write_frame(&frame).unwrap();
        }
        sink.finish().expect("encode should finish cleanly");

        // ffprobe the result back: dims + frame count.
        let output = Command::new("ffprobe")
            .arg("-v")
            .arg("error")
            .arg("-select_streams")
            .arg("v:0")
            .arg("-count_frames")
            .arg("-show_entries")
            .arg("stream=width,height,nb_read_frames")
            .arg("-of")
            .arg("csv=p=0")
            .arg(&out_path)
            .output()
            .expect("ffprobe should run");
        assert!(output.status.success(), "ffprobe failed: {output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stdout = stdout.trim();
        let parts: Vec<&str> = stdout.split(',').collect();
        assert_eq!(parts.len(), 3, "unexpected ffprobe output: {stdout}");
        assert_eq!(parts[0], width.to_string());
        assert_eq!(parts[1], height.to_string());
        assert_eq!(parts[2], frame_count.to_string());

        let _ = std::fs::remove_file(&out_path);
    }
}
