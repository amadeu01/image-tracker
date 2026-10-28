# Auto Lift Detection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** From an unseeded clip, find the bar, infer the Lift Type (squat, deadlift, bench), segment reps with lift-aware phase order, and grade the result against the owner's hand labels.

**Architecture:** A pure `tracker-core` layer (whole-frame plate finder, seed proposal, lift classifier, lift-aware rep segmentation) over `Frame`s and tracks. A `tracker-app` pre-pass samples frames through the existing decoder and feeds it; the CLI auto-accepts the proposal (run marked unverified), the GUI shows it for one-click confirm. Grading reuses `grade` with an owner-measured `plate_px` from a per-clip manifest.

**Tech Stack:** Rust workspace (`tracker-core` dependency-free, `tracker-app` egui/ffmpeg). No new dependencies, no ML model.

**Spec:** `docs/superpowers/specs/2026-10-28-auto-lift-detection-design.md`. Vocabulary: `CONTEXT.md`.

## Global Constraints

- `tracker-core` stays pure: no I/O, no UI, no new crates. All new core logic is functions over `Frame`/tracks.
- Manually seeded runs behave exactly as today. Baseline: `cargo test --workspace` = 533 passed, 4 ignored; the count must not drop.
- Gates before every commit: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`; then `graphify update .`.
- One task = one commit, message ends `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`. Never push; never `git add -A`; never touch `test_videos/*`, untracked `state`, `crates/tracker-app/x.overlay.mp4`.
- Plate scale comes from an owner-confirmed `plate_px` per clip. Never from the detector, never the 134 px default.
- Pass bar (acceptance, Task 8): lift type right on 4/4 clips ("unknown" counts as a miss); rep count equals the owner's on every clip; at least 90% of labelled visible frames within 0.1 plate-diameters (`grade`); Gap or Lost where the bar is hidden, never a confident wrong position.
- Rep rules: squat counts only full descent and ascent (unrack, walk-out, re-rack excluded). Deadlift: a new rep starts only after the bar rests on the floor, even briefly. (superseded 2026-10-28: each pull is a rep)
- Size tag per task is S/M/L. Run `/brooks-audit` after Task 5 and again after Task 9.

## Review Focus

Inputs the spec implies but no happy-path test covers, most likely first. Each has a test in the owning task.

1. A static plate that is not the bar (floor plates in the squat clips, rack hardware) must never win the proposal. (Task 4)
2. A clip with no bar visible at the start (21.14.51) must propose on the first stable detection, not frame 0. (Task 4)
3. A clip with no plate at all: no proposal, lift `Unknown`, a clear CLI error, no panic. (Tasks 4, 6)
4. Plate hidden behind the lifter at lockout (21.03.36): the run reports Gap/Lost, not a confident position. (Task 8 acceptance)
5. Old `.json` exports without a `verified` field still load and round-trip as verified. (Task 6)

---

### Task 1: Ground-truth scaffolding for the four new clips (S)

**Files:**
- Modify: `groundtruth/extract_frames.sh`, `groundtruth/frames_index.json`
- Create: `groundtruth/clips.json`
- Test: `groundtruth/check_clips.sh` (new, cheap consistency check)

**Interfaces:**
- Produces: manifest `groundtruth/clips.json` — object keyed `"v5"`..`"v8"`, each `{"video": "<path under test_videos/>", "lift": "deadlift"|"squat"|"bench", "reps": <int|null>, "plate_px": <number|null>}`. `null` = owner has not filled it yet. Mapping: v5=21.03.36 deadlift, v6=21.14.51 deadlift, v7=21.58.17 squat, v8=21.58.30 squat.

- [ ] **Step 1:** Write `check_clips.sh`: fails if a clip key is missing from `frames_index.json`, if `frames_index.json` and `extract_frames.sh` disagree on indices, or if `clips.json` is not valid JSON. Run it: expected FAIL (keys absent).
- [ ] **Step 2:** Add v5–v8 to `frames_index.json` (about 40 evenly spaced frame indices per clip over the clip's `nb_frames`; keep the first and last 5% out) and to `extract_frames.sh`'s `grab` calls (same indices). Create `clips.json` with the four entries, `reps` and `plate_px` as `null`.
- [ ] **Step 3:** Run `./groundtruth/extract_frames.sh` and `./groundtruth/check_clips.sh`. Expected: PNGs for v5–v8 exist; check passes.
- [ ] **Step 4:** Commit `docs(groundtruth): scaffold v5-v8 sampling and clips manifest`.

### Task 2: OWNER GATE — label the four clips (human, no code)

The owner runs `python3 groundtruth/serve.py`, opens `label.html?v=v5` through `v8`, marks `bar_sleeve_end` on each sampled frame (status `occluded` where hidden), saves `groundtruth/v5.csv`–`v8.csv`, and fills `reps` and `plate_px` per clip in `clips.json` using the Rep rules above. Nothing in Tasks 7–8 can run before this. Tasks 3–6 may proceed in parallel with it. An agent may commit the files the owner saves, staged by name.

### Task 3: Lift type and lift-aware rep segmentation in core (M)

**Files:**
- Create: `crates/tracker-core/src/lift.rs`
- Modify: `crates/tracker-core/src/rep.rs`, `crates/tracker-core/src/rep_metrics.rs`, `crates/tracker-core/src/lib.rs`
- Test: unit tests in the same files

**Interfaces:**
- Produces (`lift.rs`, re-exported from `lib.rs`):
  - `pub enum LiftType { Squat, Deadlift, Bench, Unknown }` (`Debug, Clone, Copy, PartialEq, Eq`)
  - `pub struct TrackPoint { pub frame_index: u64, pub position: Point, pub plate_diameter_px: f64 }`
  - `pub struct LiftClassification { pub lift: LiftType, pub reason: String }`
  - `pub struct LiftClassifierConfig` with `builder()` like the other configs and a `default_config()`.
  - `pub fn classify_lift(track: &[TrackPoint], config: &LiftClassifierConfig) -> LiftClassification`
- Produces (`rep.rs`): `pub enum PhaseOrder { EccentricFirst, ConcentricFirst }`; `Rep` gains `pub order: PhaseOrder` (the builder greps all `Rep {` constructions and fixes them); `pub fn segment_reps_for_lift(lift: LiftType, velocity: &[VelocitySample], config: RepSegmentationConfig) -> Vec<Rep>`. `segment_reps` keeps its signature and behaviour (EccentricFirst).

- [ ] **Step 1:** Write failing `classify_lift` tests using synthetic `TrackPoint` sequences (positions in image convention, y down): `deadlift_track_starting_low_and_rising_over_one_plate_diameter_is_deadlift`; `squat_track_dipping_and_returning_to_start_height_is_squat`; `static_track_is_unknown`; `empty_track_is_unknown`; `short_dip_and_return_below_bench_range_is_unknown_not_bench` (Bench is never returned from a bar track alone until bench clips exist; the `reason` says so). Every `Unknown` has a non-empty `reason`.
- [ ] **Step 2:** Run `cargo test -p tracker-core lift` — expected FAIL (module missing).
- [ ] **Step 3:** Implement `lift.rs`. Rule shape: express all distances in `plate_diameter_px` units from the track itself. Deadlift = the lowest point is near the start and the track rises at least a threshold of plate diameters. Squat = the track descends then returns near its start height with range above a floor threshold. Everything else `Unknown`. Thresholds are named consts in `LiftClassifierConfig`, tuned on the v5–v8 labels in Task 8, not guessed constants like 134.
- [ ] **Step 4:** Write failing rep tests: `deadlift_rep_is_concentric_then_eccentric_with_floor_rest`, `deadlift_bounce_without_rest_is_one_rep`, `deadlift_with_rest_between_pulls_is_two_reps` (superseded 2026-10-28: each pull is a rep), `squat_order_is_unchanged_from_segment_reps`, `unknown_lift_falls_back_to_eccentric_first`. Run, expected FAIL.
- [ ] **Step 5:** Implement `segment_reps_for_lift` (ConcentricFirst for `Deadlift`, else EccentricFirst) reusing the existing velocity-sign run logic; make `rep_metrics` pick the concentric span from `Rep::order`. Run `cargo test --workspace`; expected 533 + new tests, 0 failed.
- [ ] **Step 6:** Commit `feat(core): lift type classifier and lift-aware rep segmentation`.

### Task 4: Whole-frame plate finder and seed proposal in core (L)

**Files:**
- Create: `crates/tracker-core/src/plate_finder.rs`, `crates/tracker-core/src/seed_proposal.rs`
- Modify: `crates/tracker-core/src/lib.rs`
- Test: unit tests in both files (synthetic `Frame`s built with `Frame::new`)

**Interfaces:**
- Consumes: `Frame`, `Point` (`geometry.rs`); `TrackPoint`, `LiftClassification`, `classify_lift` (Task 3).
- Produces (`plate_finder.rs`): `pub struct PlateCandidate { pub center: Point, pub radius_px: f64, pub sleeve_end: Option<Point>, pub score: f64 }`; `pub struct PlateFinderConfig` (builder + `default_config()`, radius range expressed as a fraction of the frame's shorter side, not pixels); `pub fn find_plates(frame: &Frame, config: &PlateFinderConfig) -> Vec<PlateCandidate>`. `sleeve_end` is the centre of the small inner collar circle when one is found; it is the point the Bar Point maps to.
- Produces (`seed_proposal.rs`): `pub struct FrameDetections { pub frame_index: u64, pub candidates: Vec<PlateCandidate> }`; `pub struct SeedProposal { pub frame_index: u64, pub position: Point, pub plate_diameter_px: f64, pub lift: LiftClassification, pub confidence: f64 }`; `pub fn propose_seed(detections: &[FrameDetections], config: &SeedProposalConfig) -> Option<SeedProposal>`.

- [ ] **Step 1:** Write failing finder tests: `finds_filled_disc_within_two_pixels_of_true_centre`; `finds_inner_collar_circle_as_sleeve_end`; `ignores_long_straight_edges_like_rack_uprights`; `two_discs_are_both_returned_best_first`; `blank_frame_returns_no_candidates`. Run — expected FAIL.
- [ ] **Step 2:** Implement `find_plates` (edge map plus circle Hough over the radius range; score by edge support). Choice of algorithm is the builder's. Run `cargo test -p tracker-core plate_finder` — PASS.
- [ ] **Step 3:** Write failing proposal tests: `static_plate_present_in_every_frame_is_never_proposed` (Review Focus 1); `proposal_starts_at_first_stable_detection_not_frame_zero` — frames 0–99 have no plate, a moving plate appears from frame 100, so `frame_index >= 100` (Review Focus 2); `no_plates_anywhere_returns_none` (Review Focus 3); `proposal_carries_lift_classification_from_the_linked_track`; `plate_diameter_comes_from_the_detections_not_a_constant`. Run — expected FAIL.
- [ ] **Step 4:** Implement `propose_seed`: link candidates frame to frame by nearest-neighbour continuity, drop tracks with negligible travel, choose the track with the largest vertical travel, take its first stable point as `position` (the `sleeve_end` when present), and call `classify_lift` on the linked track. `confidence` in [0,1] from track length and score.
- [ ] **Step 5:** `cargo test --workspace` green; commit `feat(core): whole-frame plate finder and seed proposal`.

### Task 5: Pre-pass in the app (M)

**Files:**
- Create: `crates/tracker-app/src/prepass.rs`
- Modify: `crates/tracker-app/src/lib.rs`
- Test: unit test with a scripted `FrameDecoder` (see `frame_cache.rs`); one `#[ignore]` real-clip test that runs on `v5` and asserts a proposal exists

**Interfaces:**
- Consumes: `FrameDecoder` (`frame_cache.rs`), `find_plates`, `propose_seed`, `FrameDetections`, `SeedProposal` (Task 4).
- Produces: `pub fn run_prepass<D: FrameDecoder>(decoder: &mut D, frame_count: u64, stride: u64, on_progress: impl FnMut(u64, u64)) -> Result<Option<SeedProposal>, String>`.

- [ ] **Step 1:** Write failing test `prepass_samples_every_stride_th_frame_and_reports_progress` with a scripted decoder that records requested indices; and `prepass_returns_none_for_a_clip_with_no_plate`. Run — FAIL.
- [ ] **Step 2:** Implement `run_prepass` (decode at stride, `find_plates` per frame, collect `FrameDetections`, call `propose_seed`). Run tests — PASS.
- [ ] **Step 3:** Run `/brooks-audit`; record the score in `.brooks-lint-history.json`. Commit `feat(app): unseeded pre-pass producing a seed proposal`.

### Task 6: CLI `track` without a seed; unverified marker (M)

**Files:**
- Modify: `crates/tracker-app/src/cli.rs`, `crates/tracker-core/src/export.rs`
- Test: `cli.rs` unit tests; `export.rs` unit tests; a new case in `crates/tracker-app/tests/` if an integration test already exists for `export_json`

**Interfaces:**
- Consumes: `run_prepass`, `SeedProposal` (Task 5); `segment_reps_for_lift` (Task 3).
- Produces: `TrackArgs.seed_frame: Option<u64>` and `TrackArgs.seed: Option<Point>` (both `None` means auto); `export_json` writes an optional `"verified": false` in the metadata only for auto runs; loading JSON without the field means verified.

- [ ] **Step 1:** Write failing tests: `track_args_without_seed_means_auto`; `track_args_with_seed_is_unchanged` (existing parse tests keep passing); `export_json_omits_verified_for_manual_runs`; `export_json_marks_auto_runs_unverified`; `old_json_without_verified_field_loads_as_verified` (Review Focus 5); `auto_run_on_clip_with_no_plate_returns_clear_error_not_panic` (Review Focus 3). Run — FAIL.
- [ ] **Step 2:** Implement: when no seed is given, run the pre-pass, accept the proposal, print `lift: <type> (<reason>)` and a one-line `UNVERIFIED: seed was auto-proposed` warning, track from the proposal, segment reps with `segment_reps_for_lift`, write `verified:false`. Exit status unchanged on success; a clear error string when no proposal exists.
- [ ] **Step 3:** All gates green; commit `feat(cli): track with no seed via auto proposal, mark run unverified`.

### Task 7: Grade with the manifest (S)

**Files:**
- Modify: `crates/tracker-app/src/grade.rs`
- Test: `grade.rs` unit tests

**Interfaces:**
- Consumes: `groundtruth/clips.json` (Task 1), the auto run's `.json` and `.reps.json`.
- Produces: `grade --manifest <path> --clip <id> <export.csv> [<truth.csv>]`. `plate_px` and the owner's lift/rep count come from the manifest; report prints lift-type match, rep-count match (both `PASS`/`MISS`/`UNKNOWN` with the reason), and the existing accuracy block.

- [ ] **Step 1:** Failing tests: `manifest_plate_px_overrides_the_default`; `missing_plate_px_in_manifest_is_a_clear_error_not_the_134_default`; `unknown_lift_is_reported_as_miss_with_reason`; `rep_count_mismatch_is_reported`. Run — FAIL.
- [ ] **Step 2:** Implement; run tests — PASS; commit `feat(grade): per-clip manifest, lift type and rep count checks`.

### Task 8: Acceptance run and honest report (M) — needs Task 2 done

**Files:**
- Create: `docs/e2e-results-lift-detection.md`
- Modify: `crates/tracker-core/src/lift.rs` thresholds only if the labels justify it

- [ ] **Step 1:** For each of v5–v8 run `cargo run -p tracker-app --release -- track "<video>" --out out/v<N>` (no seed) then `cargo run -p tracker-app -- grade --manifest groundtruth/clips.json --clip v<N> out/v<N>/...`.
- [ ] **Step 2:** Tune only the named thresholds in `LiftClassifierConfig`, and only from the labelled evidence, never per clip. Check the sleeve-end versus plate-centre offset on the labels before trusting the 90% bar.
- [ ] **Step 3:** Write the report: per clip lift type, rep count versus the owner's, percentage within 0.1 plate-diameters, every `Unknown` with its reason, every Gap/Lost, and an explicit PASS/FAIL against each line of the pass bar. Failures stay failures; do not loosen the bar. Review Focus 4 (21.03.36 lockout occlusion) must be visible in the report.
- [ ] **Step 4:** Commit `docs: lift detection acceptance results`.

### Task 9: GUI proposal and confirm (L)

**Files:**
- Modify: `crates/tracker-app/src/app/state/*` (via accessor methods, do not widen `AppState` fields), `crates/tracker-app/src/app/video_panel.rs`, `crates/tracker-app/src/app/banner.rs`
- Test: state-level unit tests for the new transitions; manual check of the panel

**Interfaces:**
- Consumes: `run_prepass`, `SeedProposal`; existing `Seed` and `Job`.
- Produces: a background pre-pass job with progress; a proposal view (marker at `SeedProposal.position`, lift type and confidence text); `confirm_proposal()` which becomes the normal `Seed`, `reject_proposal()` which returns to manual seeding, and scrubbing to a different frame before confirming (builder's layout).

- [ ] **Step 1:** Failing state tests: `proposal_is_not_a_seed_until_confirmed`; `confirm_sets_seed_to_proposal_position`; `reject_returns_to_manual_seeding`; `unknown_lift_shows_manual_seed_prompt_not_a_proposal`. Run — FAIL.
- [ ] **Step 2:** Implement the job, state and panel. A confirmed proposal is a verified run; an unconfirmed run is never started.
- [ ] **Step 3:** All gates green, run the app once and check the flow by hand on `v5`; commit `feat(app): seed proposal with one-click confirm`. Run `/brooks-audit`.

---

## Self-Review

- **Spec coverage:** goals map to Tasks 3–6 and 9; ground truth and grading to 1, 2, 7, 8; non-goals are in Global Constraints; owner decisions appear as constraints (rep rules, "unknown is a miss", `plate_px` from the owner, unverified field).
- **Placeholders:** none. Thresholds are named config values calibrated in Task 8, by design.
- **Type consistency:** `TrackPoint`, `LiftClassification`, `PlateCandidate`, `FrameDetections`, `SeedProposal` are defined once (Tasks 3–4) and used unchanged in 5–9.
- **Proportion:** about twice the spec's length, mostly signatures and test names.
- **Plan call to flag to the owner:** a bar track alone cannot separate squat from bench, so `classify_lift` returns `Unknown` for short dip-and-return tracks and never `Bench` until bench clips exist. This is consistent with the "unknown is better than wrong" decision but it means bench is not delivered by this plan.
