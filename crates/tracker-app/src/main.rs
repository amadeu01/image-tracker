//! tracker-app entry point.
//!
//! No subcommand (or an unrecognized first argument that isn't a
//! subcommand): opens the egui app shell (2.3) on the given video path, as
//! before.
//!
//! `track` subcommand (task 3.4): headless CLI mode — runs the same
//! tracking pipeline the GUI drives, without a window, then writes overlay
//! MP4 + CSV/JSON exports. Lets the pipeline be exercised end-to-end against
//! every `test_videos/` clip from a script.

use std::path::PathBuf;

use tracker_app::{app, cli, ffprobe, telemetry};

fn main() {
    let (_telemetry_guard, log_path) = telemetry::init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        log_path = ?log_path,
        "tracker-app starting"
    );
    match &log_path {
        Some(p) => println!("logging to {}", p.display()),
        None => println!("file logging unavailable; console-only"),
    }

    let mut args: Vec<String> = std::env::args().skip(1).collect();

    if !args.is_empty() && args[0] == "track" {
        let track_args = match cli::parse_track_args(&args[1..]) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("usage: tracker-app track <video> --seed-frame N --seed X,Y --out <dir>");
                eprintln!("error: {e}");
                std::process::exit(2);
            }
        };
        if let Err(e) = cli::run_track(track_args) {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        return;
    }

    if args.is_empty() {
        eprintln!("usage: tracker-app <video-path>");
        eprintln!("       tracker-app track <video> --seed-frame N --seed X,Y --out <dir>");
        std::process::exit(2);
    }
    let video_path = PathBuf::from(args.remove(0));

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
