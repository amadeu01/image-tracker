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
| 1.5 | Template Tracker: search window around last position, best-match step | M | done (sonnet-5, 2026-07-14) | `TemplateTracker` in `tracker.rs`; `TemplateTrackerConfig::builder()` (patch_radius/search_radius/min_score); `step` returns `StepOutcome::{Found{position,score}, Miss}` for 1.6's Gap logic |
| 1.6 | Gap logic: miss threshold, coast (hold/extrapolate), reacquire, `Gap` spans in path | M | done (sonnet-5, 2026-07-14) | `TrackingSession` in `session.rs`; wraps `TemplateTracker`, `step(frame)` per frame; `Sample{frame_index,position,source:Tracked\|Interpolated}` + `Gap{start,end}` spans; coast_limit (builder) misses before `SessionState::NeedsReseed` pauses `step`; `reseed(frame_index, point)` resumes and closes trailing gap |
| 1.7 | `BarPath` aggregate: positions + gaps + timebase (per-video fps) | S | done (sonnet-5, 2026-07-14) | `Timebase` (rational fps, num/den) in `bar_path.rs`; rejects zero num/den; `BarPath::new(&[Sample], &[Gap], Timebase, start_frame)` builds `PathPoint{frame_index,t_seconds,position,source}` list, video-absolute frame indices; `duration_seconds()`, `position_at(frame_index)` |

## Milestone 2 — video IO + UI shell

| ID | Task | Size | Status | Observations |
|----|------|------|--------|--------------|
| 2.1 | ffprobe metadata adapter (w, h, fps incl. odd rates like 600/19, frame count) | S | done (sonnet-5, 2026-07-14) | `ffprobe::{VideoMetadata, ProbeError, parse_ffprobe_json, probe}` in tracker-app; fps kept as rational (num/den) for caller to build `Timebase`; `nb_frames` absent → `frame_count: None`; parsing unit-tested on canned JSON (serde/serde_json, tracker-app only); `#[ignore]`d integration test hits a real test_videos/ file (space-containing filename) — ran manually, passed: 1024x576, 600/19, 1910 frames |
| 2.2 | ffmpeg decode adapter: rawvideo pipe → `Frame` iterator (`FrameSource` port) | M | done (sonnet-5, 2026-07-14) | `FrameSource` trait (dependency-free, streaming `next_frame`) in tracker-core's `frame_source.rs`; `FfmpegFrameSource<R: Read>` in tracker-app's `ffmpeg_source.rs` — generic chunk reader unit-tested against `io::Cursor`/a trickling reader, `spawn()` wires it to a real `ffmpeg -f rawvideo -pix_fmt rgb24 -` child; partial trailing bytes at EOF are a documented `ShortRead` error (not silently dropped); child killed/reaped on `Drop`; `next_frame_checked` reaps exit status and surfaces stderr on non-zero exit. `#[ignore]`d integration test decodes first 10 frames of a real test_videos/ file (space-containing filename) — ran manually, passed at 1024x576. |
| 2.3 | egui app shell: open video, show frame, scrub bar | M | done (sonnet-5 + fable-5, 2026-07-15) | Seek-based decode (`ffmpeg -ss` per frame) + 16-frame LRU cache (~27MB) instead of full preload (~3.4GB). Worker stalled on Anthropic classifier outage; verification/commit finished by fable-5. |
| 2.4 | Seed placement: click → image-pixel coords (zoom-aware) | S | done (sonnet-5, 2026-07-15) | New `screen_map.rs`: pure `screen_to_image_px`/`fitted_image_rect` (letterbox offset + scale, capped at 1.0, out-of-image click → `None`), unit-tested without egui `Context`. `Mode::PlacingSeed` (toolbar toggle button + `S` key) added alongside `ViewOnly`; `AppState::place_seed`/`status_line` store `Seed{position: tracker_core::Point, frame_index}` and report it in the status bar. Crosshair drawn via `ui.painter()` overlay (`draw_seed_crosshair`), scaled back to screen coords from the same drawn image rect — no pixel mutation. |
| 2.5 | Calibration UI: two clicks + known length (450mm default) → px/m | S | done (sonnet-5, 2026-07-15) | `Calibration` value type (TDD'd first, no egui/UI deps) in tracker-core's new `calibration.rs`: `Calibration::new(a, b, known_length_meters)` rejects non-positive length and coincident points, exposes `px_per_meter()`/`px_to_meters()`. `Mode::Calibrating{first_point, known_length_meters}` added to app.rs (toolbar button + `C` key), reusing `screen_to_image_px`; `DragValue` field defaults to `DEFAULT_CALIBRATION_LENGTH_METERS` (0.450). Two clicks resolve into `AppState.calibration`; a third click restarts the pair (regardless of prior success/failure); coincident-point errors surface in the status bar, not a panic. Painter overlay draws the pending first point and the resolved segment line; status bar reports px/m. |
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
