#!/usr/bin/env bash
# Regenerates groundtruth/frames/*.png — the sampled frames the labelling
# tool (label.html) and the accuracy grader reference by frame index.
#
# Frames are derived artifacts (gitignored); the hand labels in v*.csv are
# the source of truth and ARE committed. Run from the repo root after a
# fresh clone so the browser tool has images to show:
#     ./groundtruth/extract_frames.sh
#
# Frame indices here MUST match the frame_index column in v*.csv.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
OUT="groundtruth/frames"
mkdir -p "$OUT"

grab() { # video, id, frames...
  local video="$1"; local id="$2"; shift 2
  for f in "$@"; do
    ffmpeg -v error -i "$video" -vf "select=eq(n\\,$f)" -vsync 0 -vframes 1 -y "$OUT/${id}_$f.png"
  done
}

grab "test_videos/WhatsApp Video 2026-07-05 at 14.03.30.mp4" v1 \
  400 550 700 850 900 1000 1150 1300 1450 1600 1750 1850 1900
grab "test_videos/WhatsApp Video 2026-07-05 at 14.11.05.mp4" v2 \
  300 480 660 840 1020 1200 1300 1380 1560 1740 1920 2050 2100
grab "test_videos/WhatsApp Video 2026-07-08 at 22.55.51.mp4" v3 \
  300 520 740 960 1180 1400 1620 1840 2060 2280 2500 2700 2900 3100
grab "test_videos/WhatsApp Video 2026-07-08 at 22.56.32.mp4" v4 \
  300 620 940 1260 1580 1900 2220 2540 2860 3180 3400 3600 3750

echo "extracted frames into $OUT"
