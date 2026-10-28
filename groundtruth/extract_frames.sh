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
    ffmpeg -v error -i "$video" -vf "select=eq(n\\,$f)" -fps_mode passthrough -vframes 1 -y "$OUT/${id}_$f.png"
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

grab "test_videos/WhatsApp Video 2026-10-27 at 21.03.36.mp4" v5 \
  60 87 115 142 170 197 225 253 280 308 \
  335 363 390 418 445 473 501 528 556 583 \
  611 638 666 693 721 749 776 804 831 859 \
  886 914 941 969 997 1024 1052 1079 1107 1134
grab "test_videos/WhatsApp Video 2026-10-27 at 21.14.51.mp4" v6 \
  89 131 172 213 255 296 337 378 420 461 \
  502 544 585 626 667 709 750 791 833 874 \
  915 956 998 1039 1080 1122 1163 1204 1245 1287 \
  1328 1369 1411 1452 1493 1534 1576 1617 1658 1700
grab "test_videos/WhatsApp Video 2026-10-27 at 21.58.17.mp4" v7 \
  74 108 142 176 210 245 279 313 347 381 \
  415 449 484 518 552 586 620 654 688 722 \
  757 791 825 859 893 927 961 995 1030 1064 \
  1098 1132 1166 1200 1234 1269 1303 1337 1371 1405
grab "test_videos/WhatsApp Video 2026-10-27 at 21.58.30.mp4" v8 \
  64 94 124 154 183 213 243 273 302 332 \
  362 392 421 451 481 511 540 570 600 630 \
  659 689 719 749 778 808 838 868 897 927 \
  957 987 1016 1046 1076 1106 1135 1165 1195 1225

echo "extracted frames into $OUT"
