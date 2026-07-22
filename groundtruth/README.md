# Ground truth

## Why it exists

Audit finding F7 (PLAN 17.1): every tracker metric `compare` reports —
`tracked_pct`, gaps, reseeds, `mean_score`, `mean_jitter_px` — measures the
tracker's *self-confidence*, not whether it is actually on the bar. A
confident false lock (e.g. onto rack steel) maximises all five: lower jitter
and higher ZNCC than real tracking of a moving object. On v3, `compare`
scored `gaussian:1.5/template` 100% tracked / 0 gaps / ZNCC 0.996, yet only
**15%** of hand-labelled frames were within 0.1 plate-diameters of the true
bar. This directory is the project's only measurement of correctness against
ground truth rather than the tracker's opinion of itself.

## What's here

- **`v{1,2,3,4}.csv`** — hand labels, the committed source of truth. Columns:
  `video,frame_index,x_px,y_px,status,target`. `status` is `visible` or
  `occluded`; a blank `x_px`/`y_px` with a non-`visible` status is expected —
  those frames test whether the tracker knows to say nothing rather than
  report a confident position. `target` is `bar_sleeve_end`, **not** the
  plate-disc centre — parallax and plate sag mean the disc centre is not the
  bar's true position; this was corrected during labelling (see PLAN 17.1).
- **`frames_index.json`** — the sampled frame indices per video; must match
  the `frame_index` column in the CSVs and the indices `extract_frames.sh`
  pulls.
- **`frames/`** (gitignored) — the sampled PNGs, regenerable from
  `test_videos/`. `label.html` displays them for labelling.
- **`label.html`** + **`serve.py`** — a small local browser tool for
  labelling/re-labelling and inspecting frames.
- **`extract_frames.sh`** — regenerates `frames/` from `test_videos/`.

## How to use (fresh clone)

1. Extract the sampled frames (needs the clips under `test_videos/`):

   ```bash
   ./groundtruth/extract_frames.sh
   ```

2. Serve the labelling tool and open it in a browser to re-label or inspect:

   ```bash
   python3 groundtruth/serve.py
   # then open http://127.0.0.1:8765/label.html?v=v3
   ```

3. Grade a tracking run against ground truth:

   ```bash
   cargo run -p tracker-app -- grade <export.csv> groundtruth/v3.csv
   ```

   `grade` takes two positional CSVs (the `track`/GUI export, then the
   ground-truth file) plus two optional flags:

   - `--plate-px N` — apparent plate diameter in pixels used to express
     error in plate-diameters. Default `134.0` (measured on the v3/v4 rig,
     464x832 footage; see `docs/design/tracking-audit-2026-07-21.md`).
   - `--tolerance-dia F` — "on the bar" tolerance as a fraction of a plate
     diameter. Default `0.1` (45mm on a 0.450m plate — tight enough that a
     lock onto adjacent rack hardware fails it, loose enough to absorb ~5px
     of human labelling uncertainty).

## Code path

`tracker-app grade` (CLI parsing and CSV I/O in
`crates/tracker-app/src/grade.rs`) calls `tracker_core::accuracy::grade`
(dependency-free scoring in `crates/tracker-core/src/accuracy.rs`). The CSV
parsing intentionally stays in `tracker-app` — `accuracy::grade` takes plain
in-memory data and knows nothing about files, keeping `tracker-core` free of
dependencies per the app/core layering in `CONTEXT.md`.
