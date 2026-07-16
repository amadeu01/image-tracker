//! tracker-app entry point.
//!
//! No subcommand (or an unrecognized first argument that isn't a
//! subcommand): opens the egui app shell (2.3) on the given video path, if
//! one was given. With no video path either (task 10.5), opens the GUI in
//! the empty state — "Open video…" (button, Ctrl+O, or a launcher double-
//! click with no arg at all) loads a video from inside the running app.
//!
//! `track` subcommand (task 3.4): headless CLI mode — runs the same
//! tracking pipeline the GUI drives, without a window, then writes overlay
//! MP4 + CSV/JSON exports. Lets the pipeline be exercised end-to-end against
//! every `test_videos/` clip from a script.

use std::path::PathBuf;

use tracker_app::{app, cli, compare, ffprobe, telemetry};

fn main() {
    let (_telemetry_guard, log_path) = telemetry::init();
    telemetry::install_panic_hook();

    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let ffmpeg_version = telemetry::ffmpeg_version_summary();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        os,
        arch,
        ffmpeg_version = %ffmpeg_version,
        log_path = ?log_path,
        "tracker-app starting"
    );
    println!(
        "tracker-app {} ({os}/{arch}); ffmpeg: {ffmpeg_version}",
        env!("CARGO_PKG_VERSION")
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

    if !args.is_empty() && args[0] == "advise" {
        let advise_args = match cli::parse_advise_args(&args[1..]) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("usage: tracker-app advise <video> [--top-n N]");
                eprintln!("error: {e}");
                std::process::exit(2);
            }
        };
        if let Err(e) = cli::run_advise(advise_args) {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        return;
    }

    if !args.is_empty() && args[0] == "compare" {
        let compare_args = match compare::parse_compare_args(&args[1..]) {
            Ok(a) => a,
            Err(e) => {
                eprintln!(
                    "usage: tracker-app compare <video> --seed-frame N --seed X,Y [--frames N] [--out path.json]"
                );
                eprintln!("error: {e}");
                std::process::exit(2);
            }
        };
        if let Err(e) = compare::run_compare(compare_args) {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        return;
    }

    // No CLI arg (task 10.5): open the GUI in the empty state ("Open a
    // video to begin") rather than refusing to start. Previously this was
    // a hard usage error — the app was unreachable from a plain launcher
    // click, which is exactly why it showed up as "unknown" there.
    let video = if args.is_empty() {
        None
    } else {
        let video_path = PathBuf::from(args.remove(0));
        let metadata = match ffprobe::probe(&video_path) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("failed to probe {}: {e}", video_path.display());
                std::process::exit(1);
            }
        };
        Some((video_path, metadata))
    };

    if let Err(e) = app::run(video) {
        eprintln!("app error: {e}");
        std::process::exit(1);
    }
}
