//! Auto-export on tracking `Done` (task 10.3): once a run's `BarPath` (and
//! derived `SessionResults`) is ready, write the overlay video plus
//! CSV/JSON/reps exports next to the source video, in the background so
//! the UI thread never blocks on the overlay re-encode.
//!
//! Mirrors the CLI's `run_track` exports (`cli.rs`, task 3.4) file-for-file
//! (`<stem>.csv`, `.json`, `.reps.csv`, `.reps.json`, `.overlay.mp4`), and
//! reuses the exact same overlay render loop (`overlay_export`) so the two
//! paths can't silently diverge.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use tracker_core::{
    export_csv, export_json, export_reps_csv, export_reps_json, BarPath, Calibration, Rep,
    RepMetrics, VelocitySample,
};

use crate::overlay_export::render_overlay_video;

/// One update from the background export thread to the UI thread.
#[derive(Debug, Clone)]
pub enum ExportMessage {
    /// One output file was written successfully.
    Written(PathBuf),
    /// One output failed to write; the job keeps going (best-effort, like
    /// the CLI's export step never aborts a whole run over one flag).
    Error(String),
    /// All exports attempted; no more messages follow.
    Done,
}

/// Everything `spawn_export` needs to reproduce the CLI's `run_track`
/// exports for a completed run.
pub struct ExportJob {
    pub video_path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps_num: u64,
    pub fps_den: u64,
    pub bar_path: BarPath,
    pub calibration: Option<Calibration>,
    /// `None` if `velocity_series` failed for this run (10.9's GUI seam):
    /// CSV/JSON still export positions-only, and reps/metrics are empty.
    pub velocity: Option<Vec<VelocitySample>>,
    pub metrics: Vec<RepMetrics>,
    pub reps: Vec<Rep>,
}

/// A handle to a running export job: just the read side of its progress
/// channel (there's nothing to send back — auto-export isn't cancellable).
pub struct ExportHandle {
    pub messages: Receiver<ExportMessage>,
}

/// Spawns a background thread that writes every export for `job`, next to
/// `job.video_path`.
pub fn spawn_export(job: ExportJob) -> ExportHandle {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || run_export(job, &tx));
    ExportHandle { messages: rx }
}

/// Output paths `run_export` writes to, given the source video path: next
/// to the video, named after its stem.
fn output_paths(video_path: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf, PathBuf) {
    let out_dir = video_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let stem = video_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("out");
    (
        out_dir.join(format!("{stem}.csv")),
        out_dir.join(format!("{stem}.json")),
        out_dir.join(format!("{stem}.reps.csv")),
        out_dir.join(format!("{stem}.reps.json")),
        out_dir.join(format!("{stem}.overlay.mp4")),
    )
}

fn write_file(tx: &Sender<ExportMessage>, path: &Path, contents: &str) {
    match std::fs::write(path, contents) {
        Ok(()) => {
            tracing::info!(path = %path.display(), "export file done");
            let _ = tx.send(ExportMessage::Written(path.to_path_buf()));
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "export file failed");
            let _ = tx.send(ExportMessage::Error(format!(
                "failed to write {}: {e}",
                path.display()
            )));
        }
    }
}

fn run_export(job: ExportJob, tx: &Sender<ExportMessage>) {
    let ExportJob {
        video_path,
        width,
        height,
        fps_num,
        fps_den,
        bar_path,
        calibration,
        velocity,
        metrics,
        reps,
    } = job;

    let (csv_path, json_path, reps_csv_path, reps_json_path, overlay_path) =
        output_paths(&video_path);

    tracing::info!(video = %video_path.display(), "export job started");

    write_file(
        tx,
        &csv_path,
        &export_csv(&bar_path, calibration.as_ref(), velocity.as_deref()),
    );
    write_file(
        tx,
        &json_path,
        &export_json(&bar_path, calibration.as_ref(), velocity.as_deref()),
    );
    write_file(tx, &reps_csv_path, &export_reps_csv(&metrics));
    write_file(tx, &reps_json_path, &export_reps_json(&metrics));

    match render_overlay_video(
        &video_path,
        &overlay_path,
        width,
        height,
        fps_num,
        fps_den,
        &bar_path,
        &reps,
    ) {
        Ok(()) => {
            tracing::info!(path = %overlay_path.display(), "export file done");
            let _ = tx.send(ExportMessage::Written(overlay_path));
        }
        Err(e) => {
            tracing::warn!(path = %overlay_path.display(), error = %e, "export file failed");
            let _ = tx.send(ExportMessage::Error(format!(
                "failed to write {}: {e}",
                overlay_path.display()
            )));
        }
    }

    tracing::info!(video = %video_path.display(), "export job done");
    let _ = tx.send(ExportMessage::Done);
}

// -- Task 13.3: per-rep clip export ("Export all rep clips") ---------------

/// Everything `spawn_rep_clip_export` needs: the source video plus each
/// rep's video-absolute `(start_frame, end_frame)` bounds and the timebase
/// to turn them into seconds. Frame bounds (not pre-computed seconds) are
/// carried so the seconds math lives in one tested place (`clip_time_range`).
pub struct RepClipJob {
    pub video_path: PathBuf,
    pub fps_num: u64,
    pub fps_den: u64,
    /// Per-rep `(start_frame, end_frame)`, in rep order (index 0 = rep 1).
    pub bounds: Vec<(u64, u64)>,
}

/// Timestamp (seconds) of `frame` under the `fps_num/fps_den` timebase.
/// Degenerate timebases (zero numerator/denominator) collapse to `0.0`
/// rather than dividing by zero — ffprobe should never produce one, but a
/// clip export must not be able to emit `NaN` into an ffmpeg argument.
fn frame_time_seconds(frame: u64, fps_num: u64, fps_den: u64) -> f64 {
    if fps_num == 0 || fps_den == 0 {
        return 0.0;
    }
    frame as f64 * fps_den as f64 / fps_num as f64
}

/// `(start_seconds, end_seconds)` for one rep's frame bounds.
fn clip_time_range(bounds: (u64, u64), fps_num: u64, fps_den: u64) -> (f64, f64) {
    (
        frame_time_seconds(bounds.0, fps_num, fps_den),
        frame_time_seconds(bounds.1, fps_num, fps_den),
    )
}

/// Output path for rep clip `rep_number` (1-based): `<stem>.repNN.mp4` next
/// to the video, NN zero-padded to 2 digits (`video.rep01.mp4`), matching
/// the design mock's clip label ("rep 01 clip").
fn rep_clip_output_path(video_path: &Path, rep_number: usize) -> PathBuf {
    let out_dir = video_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let stem = video_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("out");
    out_dir.join(format!("{stem}.rep{rep_number:02}.mp4"))
}

/// The ffmpeg argument list for one stream-copied clip:
/// `-y -ss <start> -to <end> -i <input> -c copy <output>` — `-ss`/`-to`
/// before `-i` (input seeking, fast) and `-c copy` (no re-encode; cuts land
/// on keyframes, accepted for v1 per the task brief).
fn rep_clip_ffmpeg_args(input: &Path, output: &Path, start_s: f64, end_s: f64) -> Vec<String> {
    vec![
        "-y".to_string(),
        "-ss".to_string(),
        format!("{start_s:.3}"),
        "-to".to_string(),
        format!("{end_s:.3}"),
        "-i".to_string(),
        input.display().to_string(),
        "-c".to_string(),
        "copy".to_string(),
        output.display().to_string(),
    ]
}

/// Spawns a background thread that writes one `<stem>.repNN.mp4` per rep in
/// `job.bounds` via ffmpeg stream copy, reporting per-file `Written`/`Error`
/// plus a final `Done` over the same `ExportMessage` channel the auto-export
/// job uses — `AppState::poll_export` drains both identically.
pub fn spawn_rep_clip_export(job: RepClipJob) -> ExportHandle {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || run_rep_clip_export(job, &tx));
    ExportHandle { messages: rx }
}

fn run_rep_clip_export(job: RepClipJob, tx: &Sender<ExportMessage>) {
    tracing::info!(
        video = %job.video_path.display(),
        clips = job.bounds.len(),
        "rep clip export started"
    );
    for (i, &bounds) in job.bounds.iter().enumerate() {
        let out_path = rep_clip_output_path(&job.video_path, i + 1);
        let (start_s, end_s) = clip_time_range(bounds, job.fps_num, job.fps_den);
        let args = rep_clip_ffmpeg_args(&job.video_path, &out_path, start_s, end_s);
        let result = std::process::Command::new("ffmpeg").args(&args).output();
        match result {
            Ok(output) if output.status.success() => {
                tracing::info!(path = %out_path.display(), "rep clip done");
                let _ = tx.send(ExportMessage::Written(out_path));
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let tail = stderr.lines().last().unwrap_or("unknown ffmpeg error");
                tracing::warn!(path = %out_path.display(), error = %tail, "rep clip failed");
                let _ = tx.send(ExportMessage::Error(format!(
                    "failed to write {}: {tail}",
                    out_path.display()
                )));
            }
            Err(e) => {
                tracing::warn!(path = %out_path.display(), error = %e, "rep clip failed");
                let _ = tx.send(ExportMessage::Error(format!(
                    "failed to run ffmpeg for {}: {e}",
                    out_path.display()
                )));
            }
        }
    }
    tracing::info!(video = %job.video_path.display(), "rep clip export done");
    let _ = tx.send(ExportMessage::Done);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_paths_are_named_after_the_video_stem_next_to_it() {
        let (csv, json, reps_csv, reps_json, overlay) =
            output_paths(Path::new("/videos/session1.mp4"));
        assert_eq!(csv, PathBuf::from("/videos/session1.csv"));
        assert_eq!(json, PathBuf::from("/videos/session1.json"));
        assert_eq!(reps_csv, PathBuf::from("/videos/session1.reps.csv"));
        assert_eq!(reps_json, PathBuf::from("/videos/session1.reps.json"));
        assert_eq!(overlay, PathBuf::from("/videos/session1.overlay.mp4"));
    }

    #[test]
    fn output_paths_falls_back_to_out_stem_when_path_has_no_file_stem() {
        let (csv, ..) = output_paths(Path::new("/"));
        assert_eq!(csv, PathBuf::from("out.csv"));
    }

    #[test]
    fn rep_clip_output_path_zero_pads_the_rep_number_next_to_the_video() {
        assert_eq!(
            rep_clip_output_path(Path::new("/videos/session1.mp4"), 1),
            PathBuf::from("/videos/session1.rep01.mp4")
        );
        assert_eq!(
            rep_clip_output_path(Path::new("/videos/session1.mp4"), 12),
            PathBuf::from("/videos/session1.rep12.mp4")
        );
    }

    #[test]
    fn frame_time_seconds_uses_the_timebase_and_survives_degenerate_fps() {
        // 600/19 fps (the project's test footage): frame 600 = 19 seconds.
        assert!((frame_time_seconds(600, 600, 19) - 19.0).abs() < 1e-9);
        assert!((frame_time_seconds(30, 30, 1) - 1.0).abs() < 1e-9);
        assert_eq!(frame_time_seconds(100, 0, 1), 0.0);
        assert_eq!(frame_time_seconds(100, 30, 0), 0.0);
    }

    #[test]
    fn clip_time_range_maps_both_bounds() {
        let (start, end) = clip_time_range((60, 90), 30, 1);
        assert!((start - 2.0).abs() < 1e-9);
        assert!((end - 3.0).abs() < 1e-9);
    }

    #[test]
    fn rep_clip_ffmpeg_args_are_input_seeked_stream_copy() {
        let args = rep_clip_ffmpeg_args(
            Path::new("/v/in.mp4"),
            Path::new("/v/in.rep01.mp4"),
            2.0,
            3.5,
        );
        assert_eq!(
            args,
            vec![
                "-y",
                "-ss",
                "2.000",
                "-to",
                "3.500",
                "-i",
                "/v/in.mp4",
                "-c",
                "copy",
                "/v/in.rep01.mp4",
            ]
        );
    }
}
