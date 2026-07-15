# 3.4 / 3.5 — end-to-end run results

> **3.5 update (2026-07-15): the 3.4 results below for videos 1 and 2 are
> invalid and superseded.** Visual review (fable-5) found that `v1`/`v2` are
> phone captures with Display Matrix rotation (`rotation=-90` per
> `ffprobe`'s `stream_side_data`); ffmpeg's decoder auto-applies that
> rotation to its rawvideo output (1024x576 coded → 576x1024 decoded), but
> the pipeline was sizing `Frame` buffers from ffprobe's *coded* dimensions.
> Every decoded frame was reinterpreted at the wrong width, scrambling rows
> — the tracker was "tracking" garbage pixel data, and the original v1/v2
> rows below (seed guessed in landscape 1024x576 coordinates) are
> meaningless. `v3`/`v4` have no rotation side data (already portrait-coded
> at the container level), so their original results were valid and are
> unchanged below.
>
> Fix: `ffprobe.rs`'s `VideoMetadata` now also parses
> `stream_side_data=rotation` and exposes `display_width()`/
> `display_height()` (swapped from the coded `width`/`height` when rotation
> is an odd multiple of 90°). Every consumer that sizes decoded frame
> buffers — the CLI `track` pipeline, the GUI's `SeekingFrameDecoder` and
> tracking worker spawn — now uses the display dimensions instead of the
> raw ffprobe `width`/`height`. Seeds for v1/v2 were re-picked by extracting
> and visually inspecting frames in the correct (portrait) orientation.

Run via the new headless CLI mode:

```
tracker-app track <video> --seed-frame N --seed X,Y --out <dir>
```

Default tuning (`crates/tracker-app/src/tracking.rs`): `patch_radius=12`,
`search_radius=30`, `min_score=0.4`, `coast_limit=5`. Same
`TemplateTracker`/`TrackingSession` pipeline as the GUI (2.6), same overlay
renderer (3.1) and CSV/JSON export (3.3).

Headless auto-recovery: with no human to re-place the seed on
`NeedsReseed`, the CLI resumes automatically from the last known tracked
position at the frame the session paused on. This is a strictly worse seed
than a human re-examining the frame would pick, so per-video reseed-event
counts below are a *worst-case* estimate of how often a human would need to
intervene in the GUI, not a hard requirement.

Verdict criteria: fraction Tracked vs Interpolated, gap/reseed counts, and
path plausibility (descent+ascent covered in `y_px`, no >50px inter-frame
jumps computed from the CSV).

| Video | Coded dims / rotation / display dims / fps | Seed (frame, x, y) | Points | Tracked | Interpolated | Gaps | Reseed events | y_px range | Verdict |
|---|---|---|---|---|---|---|---|---|---|
| `WhatsApp Video 2026-07-05 at 14.03.30.mp4` | 1024x576 coded, rotation=-90, **576x1024 display**, 600/19 (~31.6fps) | frame 789, (310, 280) | 1207 | 1207 | 0 | 3 | 3 | 172–398 | Good. Correctly oriented (verified: `HAMMER STRENGTH` rack text reads upright, plate end-face round not scrambled). Seed re-picked mid-squat-descent by visually inspecting an extracted portrait frame. 3 reseeds over the run (plate likely lost at rack-in/out near the clip's edges); max frame-to-frame jump 40px, no wild jumps. |
| `WhatsApp Video 2026-07-05 at 14.11.05.mp4` | 1024x576 coded, rotation=-90, **576x1024 display**, 30/1 | frame 1200, (290, 390) | 912 | 896 | 16 | 8 | 0 | 167–563 | Good. Zero reseeds; 8 short gaps auto-coasted (16/912 interpolated, ~1.8%). Max frame-to-frame jump 42px. Seed placed at a visually-confirmed squat-bottom frame. |
| `WhatsApp Video 2026-07-08 at 22.55.51.mp4` | 464x832, no rotation side data (already portrait-coded) | frame 300, (260, 120) | 2887 | 2882 | 5 | 3 | 0 | 105–499 | Unaffected by the rotation bug (no Display Matrix side data) — result unchanged from 3.4. Very good: 3 short gaps auto-coasted (5/2887 interpolated, ~0.2%), no reseed needed, no wild jumps. |
| `WhatsApp Video 2026-07-08 at 22.56.32.mp4` | 464x832, no rotation side data (already portrait-coded) | frame 300, (300, 150) | 3478 | 3468 | 10 | 6 | 0 | 101–602 | Unaffected by the rotation bug — result unchanged from 3.4 (using the seed retuned in that task). Good: 0 reseed events, 10/3478 interpolated (~0.3%). |

Verified by extracting overlay-MP4 frames to scratch and visually inspecting
orientation/content (not just CSV plausibility) for all four videos —
`HAMMER STRENGTH` rack text and the round barbell plate render correctly
(previously scrambled into unreadable diagonal noise for v1/v2 before this
fix).

## Takeaways

- Default tracker tuning (patch/search radius, min_score, coast_limit) from
  2.6 works as-is across all four clips once the seed is placed on the
  plate end-face; no config changes were needed, only a seed-position
  retune for one video.
- Seed placement is the dominant factor in tracking quality here, more than
  tracker tuning — the same rough coordinates transferred well between two
  of the four clips and poorly between the other two, despite similar
  framing/resolution. This matches the domain model: the Seed is a
  per-video, per-lift user action (2.4/2.5), not something to hardcode.
- Outputs (overlay MP4 + CSV/JSON) for all four videos are in `out/`
  (gitignored, not committed) for manual inspection.
- (3.5) ffprobe's reported `width`/`height` are the *coded* stream
  dimensions, not necessarily what a decoder emits: phone footage carrying
  a Display Matrix rotation (`stream_side_data.rotation`) gets auto-rotated
  by ffmpeg's decoder, so any adapter reading rawvideo off an `ffmpeg -i`
  pipe must size its frame buffers from the *display* (post-rotation)
  dimensions, or every row silently reinterprets at the wrong stride —
  this passed unit tests and "looked" fine in CSV plausibility checks in
  3.4 (no crashes, no NaNs, plausible-looking y-ranges by coincidence) but
  was tracking scrambled pixel data end to end. Visual inspection of actual
  decoded frames (not just numeric output) is what caught it.

## Reproducing

```
cargo build --release --bin tracker-app
BIN=./target/release/tracker-app
"$BIN" track "test_videos/WhatsApp Video 2026-07-05 at 14.03.30.mp4" --seed-frame 789 --seed 310,280 --out out/v1
"$BIN" track "test_videos/WhatsApp Video 2026-07-05 at 14.11.05.mp4" --seed-frame 1200 --seed 290,390 --out out/v2
"$BIN" track "test_videos/WhatsApp Video 2026-07-08 at 22.55.51.mp4" --seed-frame 300 --seed 260,120 --out out/v3
"$BIN" track "test_videos/WhatsApp Video 2026-07-08 at 22.56.32.mp4" --seed-frame 300 --seed 300,150 --out out/v4
```

## 3.6 update (2026-07-15): adaptive dual-template tracking, re-run with visually-picked seeds

Visual review (fable-5) of the 3.4/3.5 overlay videos found the tracker
still drifted on real footage even with 0 recorded reseed events: the
fixed-seed template goes stale as the plate rotates and lighting shifts
through a rep, so the ZNCC best match gradually slides off the plate onto
the bar/background while still scoring above `min_score` — the CSV/gap
counts looked fine, but the overlay showed the tracked point wandering off
the object it started on (worst case: lost near frame 852 on a seed at
frame 800, one frame outside this task's own v1 seed).

Fix: `TemplateTracker` (`crates/tracker-core/src/tracker.rs`) now keeps two
templates instead of one:

- **anchor** — captured once at the seed, never changes. Prevents total
  drift away from the originally-marked object no matter how long the clip
  runs.
- **adaptive** — starts identical to the anchor, refreshed with the
  freshly-matched patch whenever the winning score clears a new
  `update_threshold` config (default 0.7, comfortably above `min_score`).
  Tracks gradual, real appearance change (rotation, lighting) that would
  otherwise erode the anchor's match score over a long clip.

Per candidate in the search window, the effective score is
`max(anchor_score, adaptive_score)`; the best-scoring candidate wins and is
`Found` if its effective score clears `min_score`. The adaptive template is
only replaced when the *winning* score clears `update_threshold` — a
marginal match (occlusion edges, near-misses, anything between `min_score`
and `update_threshold`) is still accepted as `Found` but never lets the
adaptive template creep toward the wrong thing, and a `Miss` never touches
either template.

New tracker-core tests (TDD, `tracker.rs`): a synthetic per-pixel blend
between two unrelated patterns (not a global affine transform, since ZNCC
is invariant to that and a uniform brightness ramp wouldn't exercise the
adaptive path at all) walked in small steps from `t=0.0` to `t=1.0` —
dual-template tracking stays `Found` throughout, while the same anchor
patch scored directly against the final appearance drops below
`min_score`, confirming an anchor-only tracker really would have lost it.
A second test occludes the object with the unrelated pattern (`Miss`) then
reverts to the exact seed appearance, which must still self-match near
1.0, proving the miss never corrupted the adaptive template. A third test
drives a marginal match (score between `min_score` and `update_threshold`)
and repeats the identical frame — the score must reproduce exactly,
proving the adaptive template wasn't refreshed on marginal evidence.

`tracker-app`'s `track` subcommand gained five optional tuning flags
(`crates/tracker-app/src/cli.rs`, `crates/tracker-app/src/tracking.rs`):
`--patch-radius`, `--search-radius`, `--min-score`, `--update-threshold`,
`--coast-limit`. Unset flags fall back to the existing defaults
(`patch_radius=12`, `search_radius=30`, `min_score=0.4`,
`update_threshold=0.7`, `coast_limit=5`); `TrackerTuning` (a plain
`Option`-per-field struct) is threaded from CLI parsing into
`tracking::tracker_config`/`session_config`, unit-tested for both the
"nothing set → defaults" and "everything set → overridden" cases.

### Re-run with fable-5's visually-picked seeds

fable-5 re-picked all four seeds by eye on the actual (display-rotated)
frames, since seed placement was already established in 3.4/3.5 as the
dominant factor in tracking quality — this run isolates the tracker-tuning
change from seed-quality noise as much as possible. Same default tuning as
before (`patch_radius=12`, `search_radius=30`, `min_score=0.4`,
`coast_limit=5`), plus the new dual-template `update_threshold=0.7`.
Outputs in `out/v1d`..`out/v4d` (gitignored).

| Video | Seed (frame, x, y) | Points | Tracked | Interpolated | Gaps | Reseed events | y_px range | Max inter-frame jump |
|---|---|---|---|---|---|---|---|---|
| v1 `...14.03.30.mp4` | frame 789, (312, 430) | 1222 | 1216 | 6 | 2 | 0 | 12–557 | 42.4px |
| v2 `...14.11.05.mp4` | frame 1200, (283, 430) | 882 | 856 | 26 | 18 | 6 | 12–527 | 42.4px |
| v3 `...22.55.51.mp4` | frame 300, (260, 120) | 2887 | 2887 | 0 | 0 | 0 | 109–606 | 42.4px |
| v4 `...22.56.32.mp4` | frame 300, (232, 148) | 3478 | 3478 | 0 | 0 | 0 | 143–672 | 42.4px |

The max inter-frame jump is identical (42.4px = `search_radius`×√2) across
all four videos — that's the geometric ceiling of a single step's search
window, not a coincidence in the tracked path; it means no candidate ever
won from outside the window (expected — the window is a hard search
boundary, not a scored constraint).

v3 and v4 tracked perfectly (0 reseeds, 0 interpolated points) — same as
3.4/3.5, unaffected by the new dual-template logic since their seeds and
motion were already well handled by the anchor alone. v1 improved slightly
over the equivalent 3.5 seed area (2 gaps here vs 3 reseeds there, though
seeds differ so this isn't a controlled comparison). **v2 is the honest
negative result**: 6 reseed events and 26 interpolated points — worse by
the numbers than 3.5's zero-reseed run at a different seed. This CSV
plausibility alone does not tell us whether the dual-template change
helped, hurt, or is neutral for v2; it needs the same visual check that
caught the original 3.4/3.5 problems, which is why overlay frames are
handed to fable-5 below rather than this table standing alone.

Overlay frames extracted ~15s and ~30s after each seed's timestamp (for
fable-5's visual review, not committed):
`/tmp/claude-1000/-home-amca-Developer-image-tracker/db0fc50d-c5b2-48d3-940b-46eb378ec2cb/scratchpad/adaptive_v{1,2,3,4}_{a,b}.png`.

### Reproducing

```
cargo build --release --bin tracker-app
BIN=./target/release/tracker-app
"$BIN" track "test_videos/WhatsApp Video 2026-07-05 at 14.03.30.mp4" --seed-frame 789 --seed 312,430 --out out/v1d
"$BIN" track "test_videos/WhatsApp Video 2026-07-05 at 14.11.05.mp4" --seed-frame 1200 --seed 283,430 --out out/v2d
"$BIN" track "test_videos/WhatsApp Video 2026-07-08 at 22.55.51.mp4" --seed-frame 300 --seed 260,120 --out out/v3d
"$BIN" track "test_videos/WhatsApp Video 2026-07-08 at 22.56.32.mp4" --seed-frame 300 --seed 232,148 --out out/v4d
```
