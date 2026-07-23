---
name: run-smoke-test
description: Run the image-tracker release smoke test and record an attributed, per-run report under docs/smoke/. Use when asked to run the smoke test, smoke-check a build before release, verify tracking accuracy against ground truth, or produce/commit a smoke report. Runs scripts/smoke-report.sh (5 scriptable checks incl. accuracy vs groundtruth), fills in tester + model + commit, verifies results, and commits the timestamped report.
---

# Run smoke test (image-tracker)

Runs the scriptable release smoke test, attributes the run (who + which model + which commit), verifies every check, and commits the report. GUI checks stay manual (checkboxes in the report).

## What the smoke test must do

`scripts/smoke-report.sh` is the source of truth. It builds the release binary and runs 5 scriptable checks:

1. **CLI usage** — invalid args (`track` with no seed) exit non-zero and print usage.
2. **v3 accuracy** — headless `track` on v3 (seed `296,66`, the bar sleeve end) grades **≥80%** of labelled frames within 0.1 plate-dia vs `groundtruth/v3.csv`.
3. **v4 accuracy** — v4 (seed `243,120`) grades **≥90%** vs `groundtruth/v4.csv`.
4. **Exports** — all 5 files (csv/json/reps.csv/reps.json/overlay.mp4) written.
5. **Banner + log** — version banner printed, log file on disk.

Checks 2/3 assert **accuracy against hand labels**, not row count — a row count rewards a tracker fabricating a full path while drifted onto the rack (audit F7 / PLAN 17.1), which once let a real regression pass. Do not weaken them back to row counts.

## Run it

Set attribution, then run. `SMOKE_TESTER`/`SMOKE_MODEL` land in the report header.

```bash
SMOKE_TESTER="Claude Code (agent)" SMOKE_MODEL="<your model id, e.g. claude-sonnet-5>" \
  ./scripts/smoke-report.sh
```

- Prereqs: `test_videos/` present and `groundtruth/v3.csv`/`v4.csv` present (regenerate frames with `./groundtruth/extract_frames.sh` if needed — labels themselves are committed).
- Each run writes a fresh `docs/smoke/YYYY-MM-DD-HHMMSS.md` (nothing overwritten) recording the commit (`-dirty` if uncommitted changes).
- Use your actual model id for `SMOKE_MODEL` (see the model-id note in the harness/system prompt), not a guess.

## Verify

- All 5 scriptable checks must print `PASS`. If any is `FAIL`, do **not** commit a green report — triage:
  - v2/v3 accuracy FAIL → the tracking regressed, or the seed/ground-truth drifted. Grade manually (`tracker-app grade <export.csv> groundtruth/vN.csv`) and compare against the last passing report's numbers before deciding.
  - CLI/exports/banner FAIL → a real breakage; report it, don't paper over it.
- Read the generated report; confirm Tester/Model/Commit are filled (not blank/unknown).

## Report + commit

- Fill the manual **GUI checklist** only if a human actually walked through it; otherwise leave the boxes unchecked and note "GUI pass pending (script-only run)" under Notes.
- Commit the new report: `git add docs/smoke/<file>.md && git commit`. Keep prior reports (history). Message like `smoke: <commit> — 5/5 scriptable checks pass (v3 NN%, v4 NN%)`.
- Relay the pass/fail summary and the v3/v4 accuracy numbers to the user.
