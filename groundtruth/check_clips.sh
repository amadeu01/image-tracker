#!/usr/bin/env bash
# Consistency check for the clips manifest: every clip in clips.json must be
# in frames_index.json, and its indices must match the grab call in
# extract_frames.sh. Does not need PNGs or videos.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
python3 -I - <<'PY'
import json, re, sys
try:
    clips = json.load(open("clips.json"))
except Exception as e:
    sys.exit(f"FAIL: clips.json invalid: {e}")
idx = json.load(open("frames_index.json"))
sh = open("extract_frames.sh").read()
grabs = {}
for m in re.finditer(r'^grab\s+"[^"]*"\s+(\w+)\s*\\\n((?:\s+[\d ]+\\?\n?)+)', sh, re.M):
    grabs[m.group(1)] = [int(x) for x in re.findall(r"\d+", m.group(2))]
bad = []
for k in clips:
    if k not in idx:
        bad.append(f"{k}: missing from frames_index.json"); continue
    if k not in grabs:
        bad.append(f"{k}: missing from extract_frames.sh"); continue
    if idx[k] != grabs[k]:
        bad.append(f"{k}: indices differ between frames_index.json and extract_frames.sh")
if bad:
    sys.exit("FAIL:\n  " + "\n  ".join(bad))
print(f"OK: {len(clips)} clips consistent")
PY
