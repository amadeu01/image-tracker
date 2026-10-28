# Auto lift detection and bar finding — design

Date: 2026-10-28. Status: approved in conversation, awaiting plan.
Vocabulary: see `CONTEXT.md` (Lift Type, Seed Proposal, Bar Point, Rep, Seed, Gap, Lost).

## Intent and why

Today the tracker needs a person to place a Seed on the bar and says nothing
about what lift it is watching. The owner wants to open a clip and have the
software work out the Lift Type (squat, deadlift, bench press), find the bar
by itself, and report reps and the bar path.

The earlier tracking audit (`docs/design/tracking-audit-2026-07-21.md`,
`groundtruth/README.md`) showed the tracker's own confidence metrics can read
100% while it sits on rack steel. So "works" is decided against hand labels,
never against the tracker's confidence.

## Goals

- Propose a Seed (a **Seed Proposal**) from an unseeded clip: where the bar is.
- Infer the **Lift Type**, or say "unknown" and fall back to a manual Seed.
- Segment Reps with phase order that depends on Lift Type (deadlift:
  concentric then eccentric, bounded by the floor; squat and bench: eccentric
  then concentric, bounded by lockout). One rep per pull for touch-and-go.
- GUI: show the proposal and lift type on a frame, one-click confirm or correct.
- CLI: `track` works with no seed; auto-accepts the proposal and marks the run
  **unverified**. The same engine backs both.
- Measure everything against ground truth with the existing `grade` command.

## Non-goals and anti-goals

- No pose or joint tracking. No trained neural model. No camera-motion
  compensation. No bench validation until bench clips exist (the logic is
  designed for it, shipped untested).
- Anti-goal: a run that looks right but fails the grade counts as failure.
  A confident wrong position is worse than an honest Gap or Lost.
- Do not change the meaning of the **Bar Point** (sleeve end seen in
  profile, not plate-disc centre) or the existing `grade` tolerance.

## Evidence base (the four new clips, `test_videos/*2026-10-27*`)

Portrait (display 576x1024, stored 1024x576 with `rotation=-90`, which the
decoder already applies), about 31.6 fps, 40–60 s each.

| Clip | Lift | Notes |
|---|---|---|
| 21.03.36 | deadlift | blue plate on the floor; plate hidden behind lifter at lockout |
| 21.14.51 | deadlift | lifter stands aside for the first stretch, then walks in |
| 21.58.17 | squat | rack, yellow 15 kg plate, handheld camera drifts, bar re-racked |
| 21.58.30 | squat | same rack; camera reframes mid-set; lifter walks away at the end |

The older ground-truth clips (`v1`–`v4`) are 464x832 footage with a measured
plate diameter of 134 px (`grade --plate-px` default). The new clips have a
different scale, so plate size must be measured per clip, not assumed.

## Constraints

- `tracker-core` stays dependency-free pure logic; I/O and UI stay in
  `tracker-app` (`docs/architecture.md`). Lift detection and seed proposal
  belong in core as pure functions over `Frame`s and a `BarPath`.
- Reuse existing seams (`FrameSource`, `Tracker`, `TrackingSession`,
  `segment_reps`, `suggest_tracker`). `CircleTracker` is a local tracker
  (`search_radius` around a Seed); it does **not** detect a plate anywhere in
  a frame. A whole-frame plate finder is new code.
- Behaviour-preserving for manually seeded runs: existing 533 tests must keep
  passing, and a manual Seed must still work exactly as today.
- Project workflow: size every task S/M/L, one task per commit, run
  `/brooks-audit` after a major change or a few medium ones.

## Decisions made by the owner

- Lift set is exactly squat, deadlift, bench; "unknown" falls back to a manual Seed.
- Propose then confirm in the GUI; headless auto-accepts and marks unverified.
- One target for all lifts: the Bar Point.
- Rep is one cycle between rest positions, phase order by Lift Type.
- Pass bar: lift type right on 4 of 4 clips; rep counts equal the owner's
  count on every clip; at least 90% of labelled visible frames within 0.1
  plate-diameters (`grade`); Gap or Lost reported where the bar is hidden.
- Handheld camera and plate occlusion stay in scope as stress cases.
- Rep counting rules (the owner's convention, used for the manifest and the
  segmenter target):
  - Squat: only a full descent and ascent counts. Unrack, walk-out and re-rack
    do not. A partial attempt counts only if the owner would log it as a rep.
  - Deadlift: a new rep starts only after the bar has come to rest on the
    floor, even briefly. A bounce without rest is the same rep.
- "Unknown" lift type counts as a miss on the 4-of-4 lift-type bar, but is
  preferred to a confident wrong type. Tune thresholds to say "unknown" when
  unsure, and list each "unknown" with its reason in the grading report.
- The owner labels about 40 frames per clip in `groundtruth/label.html`
  (saved as `v5`–`v8`) and records lift type and rep count per clip in a
  small manifest.

## My calls (not agreed with the owner; please check)

- Method: find the plate by its circular end-face, then judge Lift Type from
  how that point moves (start height relative to plate diameter and frame;
  vertical range; whether it begins at rest on the floor). Heuristics, no model.
- Plate scale for the 0.1-diameter tolerance comes from the detected plate in
  each clip, not the 134 px default.
- The ground-truth manifest is one small file under `groundtruth/`; its exact
  format is the builder's call.
- Pipeline (builder question 1): a whole-clip pre-pass. Sample frames at a
  stride, run the plate finder on each, link detections into a track, infer
  Lift Type from that track, then hand the first reliable point to the normal
  tracker as the Seed. The Seed Proposal is shown on the first frame where a
  plate is detected stably, not on frame 0 (clip 21.14.51 has no bar at first).
  The GUI shows progress during the pre-pass and lets the user scrub to a
  different frame before confirming.
- Plate versus Bar Point (builder question 2): the clips are filmed end-on, so
  the plate face is visible with the sleeve end at its centre. The existing
  labelling convention (`groundtruth/README.md`) already notes that the disc
  centre differs from the sleeve end through parallax and sag. So the finder
  should aim at the small inner collar or sleeve circle, not the outer disc,
  and the plan must include a check of that offset on the owner's labels
  before trusting the 90% bar.
- Plate diameter for grading (builder question 3): the manifest stores an
  owner-confirmed `plate_px` per clip (two clicks on the plate edge in the
  labelling page, or an estimate the owner confirms). `grade` receives that
  value, never the detector's own measurement, so a wrong detection cannot
  loosen its own tolerance.
- "Unverified" (builder question 5): an optional `verified` field in the saved
  `.json` metadata, defaulting to true when absent so existing files still
  load and round-trip. The CLI prints a one-line warning; the exit status is
  unchanged. Manually seeded runs are untouched.

## Left to the builder

- Plate-finder algorithm (Hough or edge-based), thresholds, and how to choose
  among several circular candidates (the clips show two plates and a second
  plate on the floor in the squat frames).
- Where in `tracker-core` the new modules live and their public shape.
- GUI layout of the proposal and confirm step.
- Test strategy: synthetic frames for unit tests; the four real clips plus
  the owner's labels as the acceptance test.

## Sequence

1. Owner labels the four clips (`v5`–`v8` + manifest). Nothing is graded before this.
2. Plan and build the engine against those labels, core first, then CLI, then GUI.
3. Grade all four clips; report honestly where it falls short of the pass bar.
