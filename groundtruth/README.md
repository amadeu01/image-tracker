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

- **`v{1,2,3,4}.csv`** — hand labels, the committed source of truth. (`v5`-`v8` have sampled frames and a manifest entry but **no label CSV yet**; see "Labelling v5-v8" below.) Columns:
  `video,frame_index,x_px,y_px,status,target`. `status` is `visible` or
  `occluded`; a blank `x_px`/`y_px` with a non-`visible` status is expected —
  those frames test whether the tracker knows to say nothing rather than
  report a confident position. `target` is `bar_sleeve_end`, **not** the
  plate-disc centre — parallax and plate sag mean the disc centre is not the
  bar's true position; this was corrected during labelling (see PLAN 17.1).
  A `blurred` status is reserved for genuinely ambiguous frames: a blurred
  label is not scored and does not count against coverage, so labelling a
  stretch the tracker skipped as `blurred` would hide that miss. Use
  `visible` whenever the bar can be seen.
- **`frames_index.json`** — the sampled frame indices per video; must match
  the `frame_index` column in the CSVs and the indices `extract_frames.sh`
  pulls.
- **`frames/`** (gitignored) — the sampled PNGs, regenerable from
  `test_videos/`. `label.html` displays them for labelling.
- **`label.html`** + **`serve.py`** — a small local browser tool for
  labelling/re-labelling and inspecting frames.
- **`extract_frames.sh`** — regenerates `frames/` from `test_videos/`. It uses
  `-fps_mode passthrough` (it used to be `-vsync 0`, which ffmpeg 9 rejects);
  that flag needs **ffmpeg >= 5.1**.
- **`clips.json`** — the manifest for the auto-lift clips v5-v8 (below).
- **`check_clips.sh`** — consistency check for the manifest.

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

## Clips v5-v8 and `clips.json`

v5-v8 are the four clips the auto lift detection was built and tuned on (two
deadlifts, two squats; 40 sampled frames each in `frames_index.json`, chosen
by `extract_frames.sh`). `clips.json` is a JSON object keyed by clip id:

```json
"v5": {"video": "test_videos/WhatsApp Video 2026-10-27 at 21.03.36.mp4",
       "lift": "deadlift", "reps": null, "plate_px": null}
```

| Field | Meaning |
|-------|---------|
| `video` | path of the clip under `test_videos/` |
| `lift` | the true lift: `squat`, `deadlift` (or `bench`) |
| `reps` | the true rep count, a whole number; `null` until the owner counts it |
| `plate_px` | apparent plate diameter in pixels in that clip; `null` until the owner measures it |

`grade --manifest groundtruth/clips.json --clip v5` refuses to run while
`plate_px` is `null` (no default is assumed) and prints `REP COUNT UNKNOWN`
while `reps` is `null`. Run `./groundtruth/check_clips.sh` after editing: it
checks that every clip in `clips.json` is in `frames_index.json` and that the
indices match the `grab` call in `extract_frames.sh` (it needs no PNGs and no
videos).

## Labelling v5-v8 (owner)

1. `./groundtruth/extract_frames.sh` to produce `frames/v5_*.png` ... `v8_*.png`.
2. `python3 groundtruth/serve.py`, open
   `http://127.0.0.1:8765/label.html?v=v5` (then `v6`, `v7`, `v8`), and click
   the bar position on each frame (key `o` marks a frame `occluded` when it is not
   visible, as for v1-v4). Press "save CSV": the server writes `groundtruth/v5.csv` ... `v8.csv`
   (same columns as above). The target is the bar's sleeve end, not the plate
   centre; the software's Bar Point is currently the plate hub centre, so
   these labels are what decides whether that is close enough.
3. Count the reps of each clip and fill `reps` in `clips.json`:
   - **Squat**: count only a full descent followed by a full ascent. Unracking,
     the walk-out and the re-rack are not reps.
   - **Deadlift**: each pull is a rep. A rep ends when the bar returns to
     the floor, whether or not it rests there, so lowering straight into the
     next pull (touch-and-go) counts one rep per pull. A pull at the end of
     the clip counts if it reaches the displacement gate, even when the clip
     ends before the bar is back on the floor.
4. Fill `plate_px` per clip: the plate's diameter in pixels in that video's
   frame, measured with two clicks on opposite edges of the plate on a frame
   where it is fully visible (in the GUI calibration tool, or from the image
   coordinates in `label.html`), or an estimate that you then confirm by
   eye. Do not take it from the detector's output; the point of the
   manifest is an independent number.
5. `./groundtruth/check_clips.sh`, then grade each clip, for example:

   ```bash
   cargo run --release -p tracker-app -- track "test_videos/<clip>.mp4" --out /tmp/v5
   cargo run -p tracker-app -- grade /tmp/v5/<stem>.csv \
     --manifest groundtruth/clips.json --clip v5
   ```

   (`grade` finds the truth CSV at `groundtruth/v5.csv` and the lift in
   `/tmp/v5/<stem>.run.json`.)

Until these are filled in, nothing about the auto detection has been checked
against a human; see `docs/architecture.md` section 6b for the known limits.
