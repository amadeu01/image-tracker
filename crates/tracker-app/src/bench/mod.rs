//! `tracker-app compare <video> --seed-frame N --seed X,Y [--frames 200]
//! [--full] [--out results.json] [--export-overlays] [--out-dir dir]`
//! (tasks 11.4 + 14.1): runs the tracking pipeline over a
//! fixed-length segment (seed frame -> `+frames`, or the whole rest of the
//! video with `--full`) once per strategy in a
//! fixed matrix — {none, gaussian:1.5, median:3} filter chain x {template,
//! color} tracker kind (6 combinations) — and reports, per strategy: tracked
//! %, misses, gaps, auto-reseeds, mean match score (template only; the color
//! tracker's "score" is a fill-fraction, a different unit, so it's reported
//! separately and never averaged together with template scores), and mean
//! jitter (mean |delta position| between consecutive *tracked* samples).
//!
//! Each strategy re-decodes the segment from scratch (a fresh
//! `FfmpegFrameSource`, seeking up to `seed_frame_index` again): decoding
//! ~200 1080p frames once and keeping them all in memory would cost
//! ~340MB, and reusing one in-memory buffer across differently-filtered
//! trackers would be a correctness footgun (the whole point of 11.2/11.3's
//! same-space filtering invariant, see docs/theory.md §5, is that filtering
//! must be applied fresh per comparison, not smuggled in from a shared
//! decode). Six short re-decodes of one segment is cheap by comparison.
//!
//! Split the same way `tracking.rs` splits `TrackingRunState` from
//! `spawn_tracking`: `strategy_matrix`, `compute_metrics`, and `recommend`
//! are pure functions over plain data, unit-tested directly; `run_strategy`/
//! `run_benchmark` do the actual ffmpeg decode + `Tracker::step` IO and are
//! exercised by the manual `compare` runs against `test_videos/` instead.

mod command;
mod report;
mod runner;
mod strategy;

pub type CompareError = String;

pub use command::{parse_compare_args, run_compare, CompareArgs};
pub use report::{format_table, to_json};
pub use runner::{
    outcomes_to_bar_path, overlay_output_path, run_benchmark, run_strategy, spawn_benchmark,
    BenchmarkHandle, BenchmarkMessage, StrategyRun, DEFAULT_COMPARE_FRAMES,
};
pub use strategy::{
    compute_metrics, gate_reason, recommend, recommend_viable, strategy_matrix, BenchmarkRow,
    FilterKind, FrameOutcome, Strategy, StrategyMetrics,
};
