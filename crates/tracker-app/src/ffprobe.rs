//! ffprobe metadata adapter (task 2.1).
//!
//! Shells out to the `ffprobe` binary (see ADR 0001) to read a video's
//! width, height, rational frame rate, and (when available) frame count.
//! Parsing is separated from the subprocess call so it is unit-testable
//! against canned JSON without touching the filesystem or a real binary.

use std::path::Path;
use std::process::Command;

/// Width/height/fps/frame-count read from a video file via ffprobe.
///
/// fps is kept as a rational (`fps_num`/`fps_den`) rather than an `f64`
/// because real footage reports odd rates like `600/19`; callers build a
/// `tracker_core::bar_path::Timebase` from these parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoMetadata {
    pub width: u32,
    pub height: u32,
    pub fps_num: u64,
    pub fps_den: u64,
    /// `nb_frames` as reported by ffprobe. Absent for some containers/streams
    /// (ffprobe omits the field rather than reporting zero).
    pub frame_count: Option<u64>,
}

/// Everything that can go wrong probing a video's metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    /// `ffprobe` is not on PATH (or otherwise failed to spawn).
    FfprobeNotFound,
    /// `ffprobe` ran but exited non-zero (e.g. file missing/unreadable).
    FfprobeFailed { stderr: String },
    /// ffprobe's stdout was not valid JSON.
    InvalidJson(String),
    /// JSON parsed but had no usable video stream entry.
    NoVideoStream,
    /// `r_frame_rate` was present but not a parsable `num/den` rational.
    InvalidFrameRate(String),
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProbeError::FfprobeNotFound => {
                write!(f, "ffprobe not found on PATH; install ffmpeg/ffprobe")
            }
            ProbeError::FfprobeFailed { stderr } => {
                write!(f, "ffprobe failed: {stderr}")
            }
            ProbeError::InvalidJson(msg) => write!(f, "ffprobe output was not valid JSON: {msg}"),
            ProbeError::NoVideoStream => write!(f, "no video stream found in ffprobe output"),
            ProbeError::InvalidFrameRate(raw) => {
                write!(f, "could not parse frame rate {raw:?} as num/den")
            }
        }
    }
}

impl std::error::Error for ProbeError {}

// --- JSON shape (only the fields we asked ffprobe for) ---

#[derive(serde::Deserialize)]
struct FfprobeOutput {
    #[serde(default)]
    streams: Vec<FfprobeStream>,
}

#[derive(serde::Deserialize)]
struct FfprobeStream {
    width: Option<u32>,
    height: Option<u32>,
    r_frame_rate: Option<String>,
    nb_frames: Option<String>,
}

/// Parses ffprobe's `-of json` stdout into `VideoMetadata`. Pure function,
/// no IO — this is what the unit tests exercise directly.
pub fn parse_ffprobe_json(json: &str) -> Result<VideoMetadata, ProbeError> {
    let output: FfprobeOutput =
        serde_json::from_str(json).map_err(|e| ProbeError::InvalidJson(e.to_string()))?;

    let stream = output.streams.first().ok_or(ProbeError::NoVideoStream)?;

    let width = stream.width.ok_or(ProbeError::NoVideoStream)?;
    let height = stream.height.ok_or(ProbeError::NoVideoStream)?;

    let rate_raw = stream
        .r_frame_rate
        .as_deref()
        .ok_or(ProbeError::NoVideoStream)?;
    let (fps_num, fps_den) = parse_rational_rate(rate_raw)?;

    let frame_count = stream
        .nb_frames
        .as_deref()
        .and_then(|s| s.parse::<u64>().ok());

    Ok(VideoMetadata {
        width,
        height,
        fps_num,
        fps_den,
        frame_count,
    })
}

fn parse_rational_rate(raw: &str) -> Result<(u64, u64), ProbeError> {
    let (num_str, den_str) = raw
        .split_once('/')
        .ok_or_else(|| ProbeError::InvalidFrameRate(raw.to_string()))?;
    let num: u64 = num_str
        .parse()
        .map_err(|_| ProbeError::InvalidFrameRate(raw.to_string()))?;
    let den: u64 = den_str
        .parse()
        .map_err(|_| ProbeError::InvalidFrameRate(raw.to_string()))?;
    if den == 0 {
        return Err(ProbeError::InvalidFrameRate(raw.to_string()));
    }
    Ok((num, den))
}

/// Runs `ffprobe` on `path` and returns its parsed metadata. No `unwrap`/
/// `panic`: subprocess spawn failure, non-zero exit, and unparsable output
/// all become `ProbeError` variants for the caller (UI status bar, 2.6) to
/// surface.
pub fn probe(path: &Path) -> Result<VideoMetadata, ProbeError> {
    let output = Command::new("ffprobe")
        .arg("-v")
        .arg("error")
        .arg("-select_streams")
        .arg("v:0")
        .arg("-show_entries")
        .arg("stream=width,height,r_frame_rate,nb_frames")
        .arg("-of")
        .arg("json")
        .arg(path)
        .output()
        .map_err(|_| ProbeError::FfprobeNotFound)?;

    if !output.status.success() {
        return Err(ProbeError::FfprobeFailed {
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_ffprobe_json(&stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_odd_rational_fps_with_side_data() {
        // Real ffprobe output shape from test_videos/, including the
        // `side_data_list` field we don't ask about but must ignore.
        let json = r#"{
            "programs": [],
            "streams": [
                {
                    "width": 1024,
                    "height": 576,
                    "r_frame_rate": "600/19",
                    "nb_frames": "1910",
                    "side_data_list": [{}]
                }
            ]
        }"#;

        let meta = parse_ffprobe_json(json).expect("should parse");
        assert_eq!(
            meta,
            VideoMetadata {
                width: 1024,
                height: 576,
                fps_num: 600,
                fps_den: 19,
                frame_count: Some(1910),
            }
        );
    }

    #[test]
    fn parses_ntsc_style_fractional_fps() {
        let json = r#"{"streams":[{"width":464,"height":832,"r_frame_rate":"60000/1001","nb_frames":"3778"}]}"#;

        let meta = parse_ffprobe_json(json).expect("should parse");
        assert_eq!(meta.fps_num, 60000);
        assert_eq!(meta.fps_den, 1001);
        assert_eq!(meta.frame_count, Some(3778));
    }

    #[test]
    fn frame_count_absent_becomes_none() {
        let json = r#"{"streams":[{"width":464,"height":832,"r_frame_rate":"60/1"}]}"#;

        let meta = parse_ffprobe_json(json).expect("should parse");
        assert_eq!(meta.frame_count, None);
    }

    #[test]
    fn no_streams_is_no_video_stream_error() {
        let json = r#"{"streams":[]}"#;

        let err = parse_ffprobe_json(json).unwrap_err();
        assert_eq!(err, ProbeError::NoVideoStream);
    }

    #[test]
    fn missing_width_is_no_video_stream_error() {
        let json = r#"{"streams":[{"height":576,"r_frame_rate":"30/1"}]}"#;

        let err = parse_ffprobe_json(json).unwrap_err();
        assert_eq!(err, ProbeError::NoVideoStream);
    }

    #[test]
    fn unparsable_json_is_invalid_json_error() {
        let err = parse_ffprobe_json("not json").unwrap_err();
        assert!(matches!(err, ProbeError::InvalidJson(_)));
    }

    #[test]
    fn malformed_frame_rate_is_invalid_frame_rate_error() {
        let json = r#"{"streams":[{"width":1,"height":1,"r_frame_rate":"garbage"}]}"#;

        let err = parse_ffprobe_json(json).unwrap_err();
        assert_eq!(err, ProbeError::InvalidFrameRate("garbage".to_string()));
    }

    #[test]
    fn zero_denominator_is_invalid_frame_rate_error() {
        let json = r#"{"streams":[{"width":1,"height":1,"r_frame_rate":"30/0"}]}"#;

        let err = parse_ffprobe_json(json).unwrap_err();
        assert_eq!(err, ProbeError::InvalidFrameRate("30/0".to_string()));
    }

    #[test]
    fn probe_missing_file_returns_ffprobe_failed_not_panic() {
        let err = probe(Path::new("/nonexistent/does-not-exist.mp4")).unwrap_err();
        assert!(matches!(err, ProbeError::FfprobeFailed { .. }));
    }

    #[test]
    #[ignore = "run manually: exercises real ffprobe against test_videos/ (path contains spaces)"]
    fn probe_real_test_video() {
        let path = Path::new("../../test_videos/WhatsApp Video 2026-07-05 at 14.03.30.mp4");
        let meta = probe(path).expect("ffprobe should succeed on a real test video");
        assert_eq!(meta.width, 1024);
        assert_eq!(meta.height, 576);
        assert_eq!(meta.fps_num, 600);
        assert_eq!(meta.fps_den, 19);
        assert_eq!(meta.frame_count, Some(1910));
    }
}
