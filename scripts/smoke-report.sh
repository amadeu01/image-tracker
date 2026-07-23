#!/usr/bin/env bash
# Manual smoke kit (PLAN.md 9.4, accuracy checks added 2026-07-22).
#
# Runs the scriptable smoke checks for tracker-app, then prints a GUI
# checklist for a human to walk through by hand, and emits a markdown report
# to docs/smoke/YYYY-MM-DD.md with the script results auto-filled and the GUI
# items left as empty checkboxes.
#
# The headless-track checks assert ACCURACY against hand-labelled ground
# truth (groundtruth/*.csv via the `grade` subcommand), not row count: a row
# count rewards the tracker fabricating a full-length path even while it has
# drifted onto the rack (audit F7 / PLAN 17.1), which is exactly how a real
# tracking regression once passed this smoke. Seeds are the ground-truth
# target (bar sleeve end), so a stray seed can't quietly inflate the score.
#
# Per-run history: each run writes its own timestamped report
# (YYYY-MM-DD-HHMMSS.md), so nothing is overwritten and every run — including
# the exact commit it ran against — is preserved.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

DATE="$(date +%Y-%m-%d)"
STAMP="$(date +%Y-%m-%d-%H%M%S)"
REPORT_DIR="$REPO_ROOT/docs/smoke"
REPORT_FILE="$REPORT_DIR/${STAMP}.md"
mkdir -p "$REPORT_DIR"

# Commit the smoke ran against (marked -dirty if the tree has uncommitted
# changes), so a report is always traceable to exact source.
COMMIT="$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
if ! git diff --quiet 2>/dev/null || ! git diff --cached --quiet 2>/dev/null; then
  COMMIT="${COMMIT}-dirty"
fi

# video : seed-frame : seed (bar sleeve end = the groundtruth target) : truth : min within-0.1-plate-dia %
V3="test_videos/WhatsApp Video 2026-07-08 at 22.55.51.mp4"
V4="test_videos/WhatsApp Video 2026-07-08 at 22.56.32.mp4"

TMP_OUT="$(mktemp -d)"
trap 'rm -rf "$TMP_OUT"' EXIT

BIN="target/release/tracker-app"

echo "== building release binary =="
cargo build --release -p tracker-app

# --- check 1: invalid CLI invocation exits non-zero and prints usage --------
# (No-args launches the GUI since 10.5, so the old "no-args prints usage"
# assertion is obsolete; a malformed subcommand is the real usage path.)
echo
echo "== check 1: invalid CLI args exit non-zero + usage =="
CHECK1_STATUS="FAIL"
set +e
cli_output="$("$BIN" track 2>&1)"   # missing required --seed-frame/--seed
cli_exit=$?
set -e
if [ "$cli_exit" -ne 0 ] && echo "$cli_output" | grep -qi "usage:"; then
  CHECK1_STATUS="PASS"
  CHECK1_DETAIL="exit code $cli_exit, usage printed"
else
  CHECK1_DETAIL="exit code $cli_exit, usage line ${cli_output:+found}"
fi
echo "$CHECK1_STATUS ($CHECK1_DETAIL)"

# --- accuracy helper --------------------------------------------------------
# grade_track <video> <seed-frame> <seed> <truth-csv> <min-pct> -> sets
# GRADE_STATUS / GRADE_DETAIL (and GRADE_FILES: 1 if all 5 exports present).
grade_track() {
  local video="$1" sframe="$2" seed="$3" truth="$4" minpct="$5"
  local outdir="$TMP_OUT/$(basename "$truth" .csv)"
  mkdir -p "$outdir"
  GRADE_STATUS="FAIL"; GRADE_DETAIL=""; GRADE_FILES=0
  if [ ! -f "$truth" ]; then
    GRADE_DETAIL="ground truth $truth missing (run groundtruth/extract_frames.sh?)"
    return
  fi
  set +e
  "$BIN" track "$video" --seed-frame "$sframe" --seed "$seed" --out "$outdir" >/dev/null 2>&1
  local track_exit=$?
  set -e
  if [ "$track_exit" -ne 0 ]; then
    GRADE_DETAIL="track exited $track_exit"
    return
  fi
  local csv
  csv="$(find "$outdir" -maxdepth 1 -name '*.csv' ! -name '*.reps.csv' | head -n1)"
  if [ -z "$csv" ]; then GRADE_DETAIL="no track CSV produced"; return; fi
  # exports present?
  local stem="${csv%.csv}"
  if [ -f "$stem.csv" ] && [ -f "$stem.json" ] && [ -f "$stem.reps.csv" ] \
     && [ -f "$stem.reps.json" ] && [ -f "$stem.overlay.mp4" ]; then
    GRADE_FILES=1
  fi
  local grade_out pct mean
  grade_out="$("$BIN" grade "$csv" "$truth" 2>&1)"
  pct="$(echo "$grade_out" | grep -iE 'within 0\.10 plate-dia' | grep -oE '[0-9]+%' | grep -oE '[0-9]+' | head -n1)"
  mean="$(echo "$grade_out" | grep -iE 'mean error' | head -n1 | sed 's/^[[:space:]]*//')"
  if [ -n "$pct" ] && [ "$pct" -ge "$minpct" ]; then
    GRADE_STATUS="PASS"
  fi
  GRADE_DETAIL="${pct:-?}% within 0.1 plate-dia (need >= ${minpct}%); ${mean:-no mean}"
}

# --- check 2: v3 accuracy ---------------------------------------------------
echo
echo "== check 2: v3 headless track accuracy vs ground truth =="
grade_track "$V3" 300 "296,66" "groundtruth/v3.csv" 80
CHECK2_STATUS="$GRADE_STATUS"; CHECK2_DETAIL="$GRADE_DETAIL"; V3_FILES="$GRADE_FILES"
echo "$CHECK2_STATUS ($CHECK2_DETAIL)"

# --- check 3: v4 accuracy ---------------------------------------------------
echo
echo "== check 3: v4 headless track accuracy vs ground truth =="
grade_track "$V4" 300 "243,120" "groundtruth/v4.csv" 90
CHECK3_STATUS="$GRADE_STATUS"; CHECK3_DETAIL="$GRADE_DETAIL"; V4_FILES="$GRADE_FILES"
echo "$CHECK3_STATUS ($CHECK3_DETAIL)"

# --- check 4: exports present ----------------------------------------------
echo
echo "== check 4: all five export files written =="
if [ "${V3_FILES:-0}" -eq 1 ] && [ "${V4_FILES:-0}" -eq 1 ]; then
  CHECK4_STATUS="PASS"
  CHECK4_DETAIL="csv/json/reps.csv/reps.json/overlay.mp4 present for both videos"
else
  CHECK4_STATUS="FAIL"
  CHECK4_DETAIL="missing exports (v3 complete=${V3_FILES:-0}, v4 complete=${V4_FILES:-0})"
fi
echo "$CHECK4_STATUS ($CHECK4_DETAIL)"

# --- check 5: version banner + log file ------------------------------------
echo
echo "== check 5: version banner + log file =="
CHECK5_STATUS="FAIL"; CHECK5_DETAIL=""
banner_line="$(echo "$cli_output" | grep -m1 '^tracker-app ' || true)"
log_line="$(echo "$cli_output" | grep -m1 'logging to ' || true)"
if [ -n "$banner_line" ] && [ -n "$log_line" ]; then
  log_dir_path="${log_line#logging to }"
  log_file="$(find "$log_dir_path" -maxdepth 1 -name 'image-tracker.log*' 2>/dev/null | head -n1)"
  if [ -n "$log_file" ] && [ -f "$log_file" ]; then
    CHECK5_STATUS="PASS"
    CHECK5_DETAIL="banner: \"$banner_line\"; log file: $log_file (exists)"
  else
    CHECK5_DETAIL="banner ok, but no image-tracker.log* file found under $log_dir_path"
  fi
else
  CHECK5_DETAIL="banner or log line missing from output"
fi
echo "$CHECK5_STATUS ($CHECK5_DETAIL)"

VERSION="$(cargo metadata --no-deps --format-version=1 2>/dev/null \
  | grep -o '"version":"[^"]*"' | head -n1 | cut -d'"' -f4)"
VERSION="${VERSION:-unknown}"

cat > "$REPORT_FILE" <<EOF
# Smoke report — $STAMP

- Version: $VERSION
- Commit: $COMMIT
- Tester:
- Platform: $(uname -s) $(uname -m)

## Scriptable checks (auto-filled by \`scripts/smoke-report.sh\`)

| # | Check | Result | Detail |
|---|-------|--------|--------|
| 1 | Invalid CLI args (\`track\` with no seed) exit non-zero and print usage | $CHECK1_STATUS | $CHECK1_DETAIL |
| 2 | \`track\` on v3 (seed 296,66) grades >= 80% within 0.1 plate-dia vs groundtruth/v3.csv | $CHECK2_STATUS | $CHECK2_DETAIL |
| 3 | \`track\` on v4 (seed 243,120) grades >= 90% within 0.1 plate-dia vs groundtruth/v4.csv | $CHECK3_STATUS | $CHECK3_DETAIL |
| 4 | All five export files (csv/json/reps.csv/reps.json/overlay.mp4) written | $CHECK4_STATUS | $CHECK4_DETAIL |
| 5 | Version banner printed and log file exists on disk | $CHECK5_STATUS | $CHECK5_DETAIL |

## GUI checklist (manual — fill in by hand)

- [ ] Open a video from the GUI file picker
- [ ] Scrub the timeline to a frame where the bar is visible (window stays responsive)
- [ ] Press \`S\` to place a seed on the bar sleeve end
- [ ] Calibrate: two clicks on known plate edges register correctly
- [ ] Track runs to the end with a live crosshair following the bar
- [ ] Side panel shows guide/status/events as tracking proceeds
- [ ] On a genuine loss, tracking pauses for reseed (does NOT dead-end as "complete")
- [ ] Review shows per-rep breakdown after a full-set track

## Notes

(free-form notes from the manual GUI pass go here)
EOF

echo
echo "== report written to $REPORT_FILE =="
