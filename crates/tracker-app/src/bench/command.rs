use std::path::PathBuf;

use tracker_core::{Point, Timebase};

use crate::ffprobe;
use crate::overlay_export::render_overlay_video;
use crate::tracking::{self, TrackerTuning};

use super::report::{format_table, to_json};
use super::runner::{
    outcomes_to_bar_path, overlay_output_path, run_strategy, DEFAULT_COMPARE_FRAMES,
};
use super::strategy::{
    compute_metrics, gate_reason, recommend_viable, strategy_matrix, BenchmarkRow,
};
use super::CompareError;

/// Fallback dt (seconds/frame) used when a video's reported fps is
/// degenerate (zero numerator/denominator) -- keeps the benchmark runnable
/// rather than blocking all motion-model reasoning on ffprobe metadata that
/// only overlay rendering strictly needs (see the `Timebase` handling
/// below).
const FALLBACK_DT: f64 = 1.0 / 30.0;

/// Parsed `compare <video> --seed-frame N --seed X,Y [--frames N] [--full]
/// [--out path] [--export-overlays] [--out-dir dir]` arguments.
#[derive(Debug)]
pub struct CompareArgs {
    pub video_path: PathBuf,
    pub seed_frame: u64,
    pub seed: Point,
    pub frames: u64,
    pub out_path: Option<PathBuf>,
    /// 14.1: benchmark over the whole video (from the seed frame to the
    /// end) instead of the default ~200-frame segment. Overrides `--frames`
    /// when both are given.
    pub full: bool,
    /// 14.1: after each strategy's run, render its bar path as an overlay
    /// video (`<stem>.<slug>.overlay.mp4`) via the shared overlay renderer.
    pub export_overlays: bool,
    /// 14.1: where `--export-overlays` writes its videos. Defaults to the
    /// source video's directory when absent.
    pub overlay_dir: Option<PathBuf>,
}

/// Parses `compare` subcommand args, reusing the same `--seed-frame`/`--seed`
/// flag shapes as `track` (`cli::parse_track_args`).
pub fn parse_compare_args(args: &[String]) -> Result<CompareArgs, CompareError> {
    let mut video_path: Option<PathBuf> = None;
    let mut seed_frame: Option<u64> = None;
    let mut seed: Option<Point> = None;
    let mut frames = DEFAULT_COMPARE_FRAMES;
    let mut out_path: Option<PathBuf> = None;
    let mut full = false;
    let mut export_overlays = false;
    let mut overlay_dir: Option<PathBuf> = None;

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
            "--frames" => {
                let v = args.get(i + 1).ok_or("--frames needs a value")?;
                frames = v.parse().map_err(|_| format!("bad --frames: {v}"))?;
                i += 2;
            }
            "--out" => {
                let v = args.get(i + 1).ok_or("--out needs a value")?;
                out_path = Some(PathBuf::from(v));
                i += 2;
            }
            "--full" => {
                full = true;
                i += 1;
            }
            "--export-overlays" => {
                export_overlays = true;
                i += 1;
            }
            "--out-dir" => {
                let v = args.get(i + 1).ok_or("--out-dir needs a value")?;
                overlay_dir = Some(PathBuf::from(v));
                i += 2;
            }
            other if video_path.is_none() && !other.starts_with("--") => {
                video_path = Some(PathBuf::from(other));
                i += 1;
            }
            other => return Err(format!("unrecognized argument: {other}")),
        }
    }

    Ok(CompareArgs {
        video_path: video_path.ok_or("missing <video> argument")?,
        seed_frame: seed_frame.ok_or("missing --seed-frame")?,
        seed: seed.ok_or("missing --seed")?,
        frames,
        out_path,
        full,
        export_overlays,
        overlay_dir,
    })
}

/// Runs the `compare` subcommand: probes the video, runs the full strategy
/// matrix over the `--frames`-length segment starting at `--seed-frame`
/// (or the whole rest of the video with `--full`), prints the aligned table
/// and recommendation to stdout, and (if `--out` was given) writes the
/// machine-readable JSON alongside it. With `--export-overlays`, each
/// strategy's bar path is additionally rendered as its own overlay video
/// (one at a time, streaming, via the shared `render_overlay_video` — never
/// six videos' worth of frames in memory at once).
pub fn run_compare(args: CompareArgs) -> Result<(), CompareError> {
    let metadata = ffprobe::probe(&args.video_path)
        .map_err(|e| format!("failed to probe {}: {e}", args.video_path.display()))?;

    let base_tuning = TrackerTuning::default();
    let coast_limit = tracking::DEFAULT_COAST_LIMIT;

    // --full overrides --frames: run from the seed frame to the end of the
    // video. When ffprobe can't report a frame count, decode until EOF
    // (run_strategy already stops cleanly at end of stream).
    let frames = if args.full {
        metadata
            .frame_count
            .map(|n| n.saturating_sub(args.seed_frame))
            .unwrap_or(u64::MAX)
    } else {
        args.frames
    };

    if let Some(dir) = args.overlay_dir.as_deref().filter(|_| args.export_overlays) {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("failed to create out dir {}: {e}", dir.display()))?;
    }

    // Only overlay rendering needs a timebase — a video with degenerate fps
    // metadata must still be benchmarkable without --export-overlays
    // (review finding on ed48dd1).
    let timebase = if args.export_overlays {
        Some(
            Timebase::new(metadata.fps_num, metadata.fps_den)
                .map_err(|e| format!("bad video fps for {}: {e:?}", args.video_path.display()))?,
        )
    } else {
        None
    };

    // dt (17.2) for the tracker's motion model: derived from fps when
    // available, falling back to `FALLBACK_DT` rather than blocking the
    // whole benchmark on the same degenerate-fps case the timebase above
    // already tolerates when overlays aren't requested.
    let dt = Timebase::new(metadata.fps_num, metadata.fps_den)
        .map(|tb| 1.0 / tb.fps())
        .unwrap_or(FALLBACK_DT);

    tracing::info!(
        video = %args.video_path.display(),
        seed_frame = args.seed_frame,
        frames,
        full = args.full,
        export_overlays = args.export_overlays,
        strategy_count = strategy_matrix().len(),
        "compare benchmark started"
    );

    // Same loop as `run_benchmark`, unrolled here so each strategy's
    // per-frame outcomes are still in hand for the optional overlay render
    // (then dropped before the next strategy decodes -- one strategy's
    // outcomes at a time, never six).
    let matrix = strategy_matrix();
    let total = matrix.len();
    let mut rows = Vec::with_capacity(total);
    for (i, strategy) in matrix.into_iter().enumerate() {
        let run = run_strategy(
            &args.video_path,
            metadata.display_width(),
            metadata.display_height(),
            args.seed_frame,
            args.seed,
            frames,
            dt,
            strategy,
            &base_tuning,
        );
        let row = match run {
            Ok(run) => {
                let mut note = run.note;
                if let Some(timebase) = timebase.filter(|_| args.export_overlays) {
                    let slug = strategy.slug();
                    let out_path =
                        overlay_output_path(&args.video_path, args.overlay_dir.as_deref(), &slug);
                    tracing::info!(
                        strategy = %strategy.label(),
                        overlay_path = %out_path.display(),
                        "compare overlay render started"
                    );
                    let bar_path =
                        outcomes_to_bar_path(&run.outcomes, args.seed_frame, args.seed, timebase);
                    // One overlay failing must not abort the remaining
                    // strategies' overlays/rows — degrade to a note, same
                    // policy as a failed strategy run (review finding on
                    // ed48dd1).
                    match render_overlay_video(
                        &args.video_path,
                        &out_path,
                        metadata.display_width(),
                        metadata.display_height(),
                        metadata.fps_num,
                        metadata.fps_den,
                        &bar_path,
                        &[],
                    ) {
                        Ok(()) => {
                            tracing::info!(
                                strategy = %strategy.label(),
                                overlay_path = %out_path.display(),
                                "compare overlay render done"
                            );
                            println!(
                                "[{}/{}] {} — overlay written: {}",
                                i + 1,
                                total,
                                strategy.label(),
                                out_path.display()
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                strategy = %strategy.label(),
                                overlay_path = %out_path.display(),
                                error = %e,
                                "compare overlay render failed"
                            );
                            println!(
                                "[{}/{}] {} — overlay FAILED: {e}",
                                i + 1,
                                total,
                                strategy.label(),
                            );
                            let msg = format!("overlay render failed: {e}");
                            note = Some(match note {
                                Some(existing) => format!("{existing}; {msg}"),
                                None => msg,
                            });
                        }
                    }
                }
                BenchmarkRow {
                    strategy,
                    metrics: compute_metrics(&run.outcomes, coast_limit),
                    note,
                    gated_reason: run.gated_reason,
                }
            }
            Err(e) => BenchmarkRow {
                strategy,
                metrics: compute_metrics(&[], coast_limit),
                note: Some(format!("strategy failed: {e}")),
                gated_reason: gate_reason(strategy.tracker, None),
            },
        };
        rows.push(row);
    }

    let winner_label = recommend_viable(&rows).map(|i| rows[i].strategy.label());
    tracing::info!(
        strategy_count = rows.len(),
        winner = winner_label.as_deref(),
        "compare benchmark done"
    );

    let segment = if args.full {
        "whole video".to_string()
    } else {
        format!("{frames} frame segment")
    };
    println!(
        "{}: strategy benchmark, seed frame {} @ ({:.1},{:.1}), {}",
        args.video_path.display(),
        args.seed_frame,
        args.seed.x,
        args.seed.y,
        segment
    );
    print!("{}", format_table(&rows));

    if let Some(out_path) = &args.out_path {
        std::fs::write(out_path, to_json(&rows))
            .map_err(|e| format!("failed to write {}: {e}", out_path.display()))?;
        println!("wrote {}", out_path.display());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_compare_args ---

    #[test]
    fn parses_compare_args_with_default_frames_and_no_out() {
        let args: Vec<String> = vec!["video.mp4", "--seed-frame", "789", "--seed", "312,430"]
            .into_iter()
            .map(String::from)
            .collect();
        let parsed = parse_compare_args(&args).unwrap();
        assert_eq!(parsed.video_path, PathBuf::from("video.mp4"));
        assert_eq!(parsed.seed_frame, 789);
        assert_eq!(parsed.seed, Point::new(312.0, 430.0));
        assert_eq!(parsed.frames, DEFAULT_COMPARE_FRAMES);
        assert_eq!(parsed.out_path, None);
        assert!(!parsed.full);
        assert!(!parsed.export_overlays);
        assert_eq!(parsed.overlay_dir, None);
    }

    #[test]
    fn parses_full_export_overlays_and_out_dir_flags() {
        let args: Vec<String> = vec![
            "video.mp4",
            "--seed-frame",
            "0",
            "--seed",
            "1,2",
            "--full",
            "--export-overlays",
            "--out-dir",
            "overlays/",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let parsed = parse_compare_args(&args).unwrap();
        assert!(parsed.full);
        assert!(parsed.export_overlays);
        assert_eq!(parsed.overlay_dir, Some(PathBuf::from("overlays/")));
    }

    #[test]
    fn parses_compare_args_with_frames_and_out_overrides() {
        let args: Vec<String> = vec![
            "video.mp4",
            "--seed-frame",
            "0",
            "--seed",
            "1,2",
            "--frames",
            "50",
            "--out",
            "results.json",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let parsed = parse_compare_args(&args).unwrap();
        assert_eq!(parsed.frames, 50);
        assert_eq!(parsed.out_path, Some(PathBuf::from("results.json")));
    }

    #[test]
    fn compare_args_missing_required_flag_is_an_error() {
        let args: Vec<String> = vec!["video.mp4", "--seed-frame", "0"]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(parse_compare_args(&args).is_err());
    }

    #[test]
    fn compare_args_bad_seed_format_is_an_error() {
        let args: Vec<String> = vec!["video.mp4", "--seed-frame", "0", "--seed", "nope"]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(parse_compare_args(&args).is_err());
    }
}
