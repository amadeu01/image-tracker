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
}
