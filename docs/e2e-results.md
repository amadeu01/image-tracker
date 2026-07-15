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
