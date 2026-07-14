# Plan — Image Tracker (bar path MVP)

Decisions and vocabulary: see [CONTEXT.md](CONTEXT.md) and [docs/adr/](docs/adr/).

## Rules

- Every task is T-shirt sized **S** or **M**. Anything that grows to L/XL must be split before work starts.
- Work is TDD (red → green → refactor). A task is *done* only with passing tests and a commit.
- One task = one commit (or a few small ones). Update this file's status column in the same commit that finishes the task.
- Status values: `todo` · `in-progress (worker)` · `done (worker, date)`. Observations column for surprises/decisions.

## Architecture (agreed)

Cargo workspace:
- `crates/tracker-core` — pure domain, no UI/IO deps. Geometry, trackers (Color + Template/ZNCC), gaps, calibration, kinematics, reps, color advisor.
- `crates/tracker-app` — adapters: ffmpeg subprocess IO, egui UI, overlay render, CSV/JSON export.

## Milestone 1 — core domain: template tracking

| ID | Task | Size | Status | Observations |
|----|------|------|--------|--------------|
| 1.1 | Workspace scaffold: two crates, CI-able `cargo test` passes empty | S | done (sonnet-5, 2026-07-14) | |
| 1.2 | Geometry: `Point`, `Frame` (owned RGB buffer), pixel access | S | done (sonnet-5, 2026-07-14) | |
| 1.3 | Grayscale patch extraction with bounds handling | S | done (sonnet-5, 2026-07-14) | |
| 1.4 | ZNCC metric over two patches | S | done (sonnet-5, 2026-07-14) | `CorrelationMetric` trait + `Zncc` in `metric.rs`; mismatched sizes → `None`, zero-variance patch → `Some(0.0)` (no NaN) |
| 1.5 | Template Tracker: search window around last position, best-match step | M | todo | |
| 1.6 | Gap logic: miss threshold, coast (hold/extrapolate), reacquire, `Gap` spans in path | M | todo | |
| 1.7 | `BarPath` aggregate: positions + gaps + timebase (per-video fps) | S | todo | |

## Milestone 2 — video IO + UI shell

| ID | Task | Size | Status | Observations |
|----|------|------|--------|--------------|
| 2.1 | ffprobe metadata adapter (w, h, fps incl. odd rates like 600/19, frame count) | S | todo | |
| 2.2 | ffmpeg decode adapter: rawvideo pipe → `Frame` iterator (`FrameSource` port) | M | todo | |
| 2.3 | egui app shell: open video, show frame, scrub bar | M | todo | |
| 2.4 | Seed placement: click → image-pixel coords (zoom-aware) | S | todo | |
| 2.5 | Calibration UI: two clicks + known length (450mm default) → px/m | S | todo | |
| 2.6 | Run tracking from UI, live crosshair + status bar (errors surface there, no unwrap) | M | todo | |

## Milestone 3 — outputs (works on test_videos/)

| ID | Task | Size | Status | Observations |
|----|------|------|--------|--------------|
| 3.1 | Overlay renderer: path polyline + current point + legend onto frames | M | todo | |
| 3.2 | ffmpeg encode adapter: frames → MP4 (`VideoSink` port) | S | todo | |
| 3.3 | CSV/JSON export: `t, x_px, y_px, x_m, y_m, gap_flag` | S | todo | |
| 3.4 | End-to-end run on each video in `test_videos/`; record results here | M | todo | |

## Milestone 4 — color tracking

| ID | Task | Size | Status | Observations |
|----|------|------|--------|--------------|
| 4.1 | `ColorModel` learned from seed patch (HSV median ± tolerance) | S | todo | |
| 4.2 | Color Tracker: threshold + centroid within search window | M | todo | |
| 4.3 | Tracker auto-suggestion: seed color distinct from background? → Color, else Template | S | todo | |

## Milestone 5 — kinematics + reps

| ID | Task | Size | Status | Observations |
|----|------|------|--------|--------------|
| 5.1 | Smoothing (moving average / Savitzky-Golay) over positions | S | todo | |
| 5.2 | Velocity series (m/s via Calibration); raw positions preserved in export | S | todo | |
| 5.3 | Rep segmentation from vertical velocity sign changes | M | todo | |
| 5.4 | Per-rep metrics: depth, peak/mean concentric velocity; in overlay + export | M | todo | |

## Milestone 6 — marker color advisor

| ID | Task | Size | Status | Observations |
|----|------|------|--------|--------------|
| 6.1 | Hue histogram over sampled frames | S | todo | |
| 6.2 | Recommend top marker hues (max distance from scene hues), CLI/UI report | S | todo | |
