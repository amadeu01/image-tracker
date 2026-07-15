# 3.4 — end-to-end run results

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

| Video | Dimensions / fps | Seed (frame, x, y) | Points | Tracked | Interpolated | Gaps | Reseed events | y_px range | Verdict |
|---|---|---|---|---|---|---|---|---|---|
| `WhatsApp Video 2026-07-05 at 14.03.30.mp4` | 1024x576, 600/19 (~31.6fps) | frame 158, (420, 180) | 1848 | 1848 | 0 | 1 | 1 | 111–342 | Good. Tracked cleanly to near the end of a ~1752-frame remaining span; one reseed near the tail (plate likely left frame or was occluded at rack-in). No wild jumps. |
| `WhatsApp Video 2026-07-05 at 14.11.05.mp4` | 1024x576, 30/1 | frame 150, (420, 180) | 1962 | 1962 | 0 | 0 | 0 | 124–443 | Very good. Clean run start to finish, zero gaps, zero reseeds. Descent+ascent range (319px) plausible for a squat. |
| `WhatsApp Video 2026-07-08 at 22.55.51.mp4` | 464x832, 60/1 | frame 300, (260, 120) | 2887 | 2882 | 5 | 3 | 0 | 105–499 | Very good. 3 short gaps auto-coasted (5 interpolated points out of 2887, ~0.2%), no reseed needed, no wild jumps. First seed guess worked well here. |
| `WhatsApp Video 2026-07-08 at 22.56.32.mp4` | 464x832, 60000/1001 (~59.9fps) | frame 300, (260, 120) initial guess | 3093 | 3019 | 74 | 114 | 77 | 115–564 | Poor with the initial seed guess — frequent loss/reseed (path still plausible, no jumps, but a human would be re-placing the seed constantly). **Retuned seed to (300, 150)** (same frame): 3478 points, 3468 tracked, 10 interpolated, 6 gaps, **0 reseed events**, y range 101–602. Good after retuning; the initial coordinate hint for videos 3–4 (260, 120) was noticeably off-target for this specific clip's framing even though it worked for the other 464x832 video. |

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

## Reproducing

```
cargo build --release --bin tracker-app
BIN=./target/release/tracker-app
"$BIN" track "test_videos/WhatsApp Video 2026-07-05 at 14.03.30.mp4" --seed-frame 158 --seed 420,180 --out out/v1
"$BIN" track "test_videos/WhatsApp Video 2026-07-05 at 14.11.05.mp4" --seed-frame 150 --seed 420,180 --out out/v2
"$BIN" track "test_videos/WhatsApp Video 2026-07-08 at 22.55.51.mp4" --seed-frame 300 --seed 260,120 --out out/v3
"$BIN" track "test_videos/WhatsApp Video 2026-07-08 at 22.56.32.mp4" --seed-frame 300 --seed 300,150 --out out/v4
```
