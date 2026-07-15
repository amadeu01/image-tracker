//! tracker-app CLI entry point (task 2.3): probes the given video's
//! metadata (2.1) and opens the egui app shell to view/scrub it.

use std::path::PathBuf;

use tracker_app::{app, ffprobe};

fn main() {
    let mut args = std::env::args_os().skip(1);
    let Some(video_path) = args.next() else {
        eprintln!("usage: tracker-app <video-path>");
        std::process::exit(2);
    };
    let video_path = PathBuf::from(video_path);

    let metadata = match ffprobe::probe(&video_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("failed to probe {}: {e}", video_path.display());
            std::process::exit(1);
        }
    };

    if let Err(e) = app::run(video_path, metadata) {
        eprintln!("app error: {e}");
        std::process::exit(1);
    }
}
