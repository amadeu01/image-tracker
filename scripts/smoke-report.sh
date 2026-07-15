#!/usr/bin/env bash
# Manual smoke kit (PLAN.md 9.4).
#
# Runs the scriptable smoke checks for tracker-app (usage exit code, a
# quick headless track run, and version banner / log file presence), then
# prints a GUI checklist for a human to walk through by hand, and emits a
# markdown report to docs/smoke/YYYY-MM-DD.md with the script results
# auto-filled and the GUI items left as empty checkboxes.
#
# Idempotent: re-running on the same day overwrites that day's report.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

DATE="$(date +%Y-%m-%d)"
REPORT_DIR="$REPO_ROOT/docs/smoke"
REPORT_FILE="$REPORT_DIR/${DATE}.md"
mkdir -p "$REPORT_DIR"

VIDEO="test_videos/WhatsApp Video 2026-07-08 at 22.55.51.mp4"
SEED_FRAME=300
SEED="260,120"

TMP_OUT="$(mktemp -d)"
trap 'rm -rf "$TMP_OUT"' EXIT

echo "== building release binary =="
cargo build --release -p tracker-app

echo
echo "== check 1: no-args usage exit =="
CHECK1_STATUS="FAIL"
CHECK1_DETAIL=""
set +e
usage_output="$(cargo run --release -p tracker-app 2>&1)"
usage_exit=$?
set -e
if [ "$usage_exit" -ne 0 ] && echo "$usage_output" | grep -qi "usage:"; then
  CHECK1_STATUS="PASS"
  CHECK1_DETAIL="exit code $usage_exit, usage printed"
else
  CHECK1_DETAIL="exit code $usage_exit, usage line ${usage_output:+found}"
fi
echo "$CHECK1_STATUS ($CHECK1_DETAIL)"

echo
echo "== check 2: quick track run on v3 test video =="
CHECK2_STATUS="FAIL"
CHECK2_DETAIL=""
TRACK_OUT_DIR="$TMP_OUT/track-out"
mkdir -p "$TRACK_OUT_DIR"
set +e
track_output="$(cargo run --release -p tracker-app -- track \
  "$VIDEO" --seed-frame "$SEED_FRAME" --seed "$SEED" --out "$TRACK_OUT_DIR" 2>&1)"
track_exit=$?
set -e
if [ "$track_exit" -eq 0 ]; then
  csv="$(find "$TRACK_OUT_DIR" -maxdepth 1 -name '*.csv' ! -name '*.reps.csv' | head -n1)"
  if [ -n "$csv" ]; then
    rows=$(( $(wc -l < "$csv") - 1 ))
    if [ "$rows" -ge 2500 ]; then
      CHECK2_STATUS="PASS"
      CHECK2_DETAIL="CSV at $(basename "$csv"), $rows data rows"
    else
      CHECK2_DETAIL="CSV found but only $rows data rows (< 2500)"
    fi
  else
    CHECK2_DETAIL="track exited 0 but no CSV found under $TRACK_OUT_DIR"
  fi
else
  CHECK2_DETAIL="track exited $track_exit: $(echo "$track_output" | tail -n1)"
fi
echo "$CHECK2_STATUS ($CHECK2_DETAIL)"

echo
echo "== check 3: version banner + log file =="
CHECK3_STATUS="FAIL"
CHECK3_DETAIL=""
banner_line="$(echo "$usage_output" | grep -m1 '^tracker-app ' || true)"
log_line="$(echo "$usage_output" | grep -m1 'logging to ' || true)"
if [ -n "$banner_line" ] && [ -n "$log_line" ]; then
  log_dir_path="${log_line#logging to }"
  log_file="$(find "$log_dir_path" -maxdepth 1 -name 'image-tracker.log*' 2>/dev/null | head -n1)"
  if [ -n "$log_file" ] && [ -f "$log_file" ]; then
    CHECK3_STATUS="PASS"
    CHECK3_DETAIL="banner: \"$banner_line\"; log file: $log_file (exists)"
  else
    CHECK3_DETAIL="banner ok, but no image-tracker.log* file found under $log_dir_path"
  fi
else
  CHECK3_DETAIL="banner or log line missing from output"
fi
echo "$CHECK3_STATUS ($CHECK3_DETAIL)"

VERSION="$(cargo metadata --no-deps --format-version=1 2>/dev/null \
  | grep -o '"version":"[^"]*"' | head -n1 | cut -d'"' -f4)"
VERSION="${VERSION:-unknown}"

cat > "$REPORT_FILE" <<EOF
# Smoke report — $DATE

- Version: $VERSION
- Tester:
- Platform: $(uname -s) $(uname -m)

## Scriptable checks (auto-filled by \`scripts/smoke-report.sh\`)

| # | Check | Result | Detail |
|---|-------|--------|--------|
| 1 | \`tracker-app\` with no args exits non-zero and prints usage | $CHECK1_STATUS | $CHECK1_DETAIL |
| 2 | \`track\` on v3 test video (seed-frame $SEED_FRAME, seed $SEED) produces a CSV with >= 2500 rows | $CHECK2_STATUS | $CHECK2_DETAIL |
| 3 | Version banner printed and log file exists on disk | $CHECK3_STATUS | $CHECK3_DETAIL |

## GUI checklist (manual — fill in by hand)

- [ ] Open a video from the GUI file picker
- [ ] Scrub the timeline to a frame where the bar is visible
- [ ] Press \`S\` to place a seed on the bar
- [ ] Calibrate: two clicks on known plate edges register correctly
- [ ] Track runs with a live crosshair following the bar
- [ ] Side panel shows guide/status/events as tracking proceeds
- [ ] Pause mid-track, re-seed, and resume works correctly

## Notes

(free-form notes from the manual GUI pass go here)
EOF

echo
echo "== report written to $REPORT_FILE =="
