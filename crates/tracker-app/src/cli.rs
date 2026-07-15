//! Headless CLI mode (task 3.4): `tracker-app track <video> --seed-frame N
//! --seed X,Y --out <dir>` runs the same tracking pipeline the GUI drives
//! (2.6's `TemplateTracker`/`TrackingSession`, sequential `FfmpegFrameSource`)
//! without an egui window, then writes the overlay MP4 (3.1 `render_overlay`
//! + 3.2 `FfmpegVideoSink`) and CSV/JSON exports (3.3) to `--out`.
//!
//! This exists so the pipeline can be exercised end-to-end against every
//! `test_videos/` clip from a script, without a human driving the GUI for
//! each one.

use std::path::{Path, PathBuf};

use tracker_core::{export_csv, export_json, render_overlay, OverlayStyle, Point, Source, VideoSink};

use crate::ffmpeg_sink::FfmpegVideoSink;
use crate::ffmpeg_source::FfmpegFrameSource;
use crate::ffprobe;
use crate::tracking;

/// Parsed `track` subcommand arguments.
pub struct TrackArgs {
    pub video_path: PathBuf,
    pub seed_frame: u64,
    pub seed: Point,
    pub out_dir: PathBuf,
}

/// Everything that can go wrong parsing CLI args, probing, tracking, or
/// writing outputs — collapsed to a single string for `main`'s exit path.
pub type CliError = String;

/// Parses `track <video> --seed-frame N --seed X,Y --out <dir>` from the
/// args following the subcommand name itself.
pub fn parse_track_args(args: &[String]) -> Result<TrackArgs, CliError> {
    let mut video_path: Option<PathBuf> = None;
    let mut seed_frame: Option<u64> = None;
    let mut seed: Option<Point> = None;
    let mut out_dir: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--seed-frame" => {
                let v = args.get(i + 1).ok_or("--seed-frame needs a value")?;
                seed_frame = Some(v.parse().map_err(|_| format!("bad --seed-frame: {v}"))?);
                i += 2;
            }
            "--seed" => {
                let v = args.get(i + 1).ok_or("--seed needs a value (X,Y)")?;
                let (x, y) = v
                    .split_once(',')
                    .ok_or_else(|| format!("bad --seed (expected X,Y): {v}"))?;
                let x: f64 = x.trim().parse().map_err(|_| format!("bad --seed x: {v}"))?;
                let y: f64 = y.trim().parse().map_err(|_| format!("bad --seed y: {v}"))?;
                seed = Some(Point::new(x, y));
                i += 2;
            }
            "--out" => {
                let v = args.get(i + 1).ok_or("--out needs a value")?;
                out_dir = Some(PathBuf::from(v));
                i += 2;
            }
            other if video_path.is_none() && !other.starts_with("--") => {
                video_path = Some(PathBuf::from(other));
                i += 1;
            }
            other => return Err(format!("unrecognized argument: {other}")),
        }
    }

    Ok(TrackArgs {
        video_path: video_path.ok_or("missing <video> argument")?,
        seed_frame: seed_frame.ok_or("missing --seed-frame")?,
        seed: seed.ok_or("missing --seed")?,
        out_dir: out_dir.ok_or("missing --out")?,
    })
}

/// Runs the `track` subcommand: probe -> track (blocking, on this thread) ->
/// render overlay MP4 -> write CSV/JSON, all under `args.out_dir`.
pub fn run_track(args: TrackArgs) -> Result<(), CliError> {
    let metadata = ffprobe::probe(&args.video_path)
        .map_err(|e| format!("failed to probe {}: {e}", args.video_path.display()))?;

    std::fs::create_dir_all(&args.out_dir)
        .map_err(|e| format!("failed to create out dir {}: {e}", args.out_dir.display()))?;

    let handle = tracking::spawn_tracking(
        args.video_path.clone(),
        metadata.width,
        metadata.height,
        metadata.fps_num,
        metadata.fps_den,
        args.seed_frame,
        args.seed,
        tracking::default_tracker_config(),
        tracking::default_session_config(),
    );

    // Headless: no UI to place a new seed on NeedsReseed. Best-effort
    // auto-recovery instead of giving up: resume from the last known
    // position at the frame the session paused on. This is a worse seed
    // than a human would pick (no re-examination of the frame), but it lets
    // a single CLI run produce a full path + honest reseed-event count for
    // judging tracker quality end-to-end, rather than truncating the run at
    // the first loss.
    let mut run_state = tracking::TrackingRunState::started();
    let mut reseed_events: u64 = 0;
    loop {
        match handle.messages.recv() {
            Ok(msg) => {
                let needs_reseed = matches!(
                    &msg,
                    tracking::TrackingMessage::Progress {
                        state: tracker_core::SessionState::NeedsReseed,
                        ..
                    }
                );
                let done = run_state.apply(msg);
                if done {
                    break;
                }
                if needs_reseed {
                    reseed_events += 1;
                    let (Some(idx), Some(pos)) =
                        (run_state.last_frame_index, run_state.last_position)
                    else {
                        break; // shouldn't happen: Progress always sets both
                    };
                    handle.resume(idx, pos);
                }
            }
            Err(_) => break, // worker thread exited without sending Done/Error
        }
    }

    if let Some(err) = &run_state.error {
        return Err(format!("tracking error: {err}"));
    }
    let Some(bar_path) = run_state.bar_path.clone() else {
        return Err(format!(
            "tracking worker exited without completing (after {} frame(s) processed, {reseed_events} reseed event(s))",
            run_state.frames_processed
        ));
    };

    let stem = args
        .video_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("out");

    let csv_path = args.out_dir.join(format!("{stem}.csv"));
    let json_path = args.out_dir.join(format!("{stem}.json"));
    let overlay_path = args.out_dir.join(format!("{stem}.overlay.mp4"));

    std::fs::write(&csv_path, export_csv(&bar_path, None))
        .map_err(|e| format!("failed to write {}: {e}", csv_path.display()))?;
    std::fs::write(&json_path, export_json(&bar_path, None))
        .map_err(|e| format!("failed to write {}: {e}", json_path.display()))?;

    render_overlay_video(
        &args.video_path,
        &overlay_path,
        metadata.width,
        metadata.height,
        metadata.fps_num,
        metadata.fps_den,
        &bar_path,
    )?;

    let found = bar_path
        .points()
        .iter()
        .filter(|p| p.source == Source::Tracked)
        .count();
    let interpolated = bar_path.points().len() - found;
    println!(
        "{}: {} points ({found} tracked, {interpolated} interpolated), {} gap(s), {reseed_events} reseed event(s) needed -> {} / {} / {}",
        args.video_path.display(),
        bar_path.points().len(),
        bar_path.gaps().len(),
        csv_path.display(),
        json_path.display(),
        overlay_path.display(),
    );

    Ok(())
}

/// Re-decodes the source video and burns in the bar path overlay
/// (`render_overlay`, 3.1) frame by frame, encoding the result via
/// `FfmpegVideoSink` (3.2). Decodes only up to the last frame the path
/// covers, not the whole video, since tracking may have stopped early.
fn render_overlay_video(
    video_path: &Path,
    out_path: &Path,
    width: u32,
    height: u32,
    fps_num: u64,
    fps_den: u64,
    bar_path: &tracker_core::BarPath,
) -> Result<(), CliError> {
    let last_frame = bar_path
        .points()
        .last()
        .map(|p| p.frame_index)
        .unwrap_or(bar_path.start_frame());

    let mut source = FfmpegFrameSource::spawn(video_path, width, height)
        .map_err(|e| format!("failed to spawn ffmpeg decoder: {e}"))?;
    let mut sink = FfmpegVideoSink::spawn(out_path, width, height, fps_num, fps_den)
        .map_err(|e| format!("failed to spawn ffmpeg encoder: {e}"))?;

    let style = OverlayStyle::builder().build();

    let mut frame_index: u64 = 0;
    loop {
        let frame = match source
            .next_frame_checked()
            .map_err(|e| format!("decode error at frame {frame_index}: {e}"))?
        {
            Some(f) => f,
            None => break,
        };
        if frame_index > last_frame {
            break;
        }
        let mut frame = frame;
        render_overlay(&mut frame, bar_path, frame_index, &style);
        sink.write_frame(&frame)
            .map_err(|e| format!("encode error at frame {frame_index}: {e}"))?;
        frame_index += 1;
    }

    sink.finish()
        .map_err(|e| format!("failed to finalize overlay video: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_track_args() {
        let args: Vec<String> = vec![
            "video.mp4",
            "--seed-frame",
            "42",
            "--seed",
            "10.5,20.25",
            "--out",
            "out/dir",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        let parsed = parse_track_args(&args).unwrap();
        assert_eq!(parsed.video_path, PathBuf::from("video.mp4"));
        assert_eq!(parsed.seed_frame, 42);
        assert_eq!(parsed.seed, Point::new(10.5, 20.25));
        assert_eq!(parsed.out_dir, PathBuf::from("out/dir"));
    }

    #[test]
    fn missing_required_flag_is_an_error() {
        let args: Vec<String> = vec!["video.mp4", "--seed-frame", "42"]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(parse_track_args(&args).is_err());
    }

    #[test]
    fn bad_seed_format_is_an_error() {
        let args: Vec<String> = vec![
            "video.mp4",
            "--seed-frame",
            "0",
            "--seed",
            "not-a-point",
            "--out",
            "out",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert!(parse_track_args(&args).is_err());
    }
}
