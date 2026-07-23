# Manual smoke kit

`scripts/smoke-report.sh` runs the scriptable half of a release smoke test, then prints a GUI checklist for a human to walk through by hand.

Scriptable checks:

1. Invalid CLI args (`track` with no seed) exit non-zero and print usage. (No-args launches the GUI since milestone 10.5, so a malformed subcommand is the real usage path.)
2. Headless `track` on the **v3** test video (seeded at the bar sleeve end, `296,66`) grades **>= 80%** of labelled frames within 0.1 plate-diameters of ground truth (`groundtruth/v3.csv`, via the `grade` subcommand).
3. Headless `track` on **v4** (seed `243,120`) grades **>= 90%** within 0.1 plate-dia (`groundtruth/v4.csv`).
4. All five export files (`csv`/`json`/`reps.csv`/`reps.json`/`overlay.mp4`) are written.
5. Version banner printed and log file exists on disk.

Checks 2/3 assert **accuracy against hand labels**, not CSV row count — a row count rewards the tracker fabricating a full-length path even while drifted onto the rack (audit F7 / PLAN 17.1), which is exactly how a real tracking regression once passed this smoke. They need `groundtruth/*.csv` (committed) and the test videos present.

Each run writes a **timestamped, per-run** report `docs/smoke/YYYY-MM-DD-HHMMSS.md` (nothing is overwritten) that records the **commit** it ran against (marked `-dirty` if the tree had uncommitted changes), the auto-filled scriptable results, and empty GUI checkboxes. Run it with `./scripts/smoke-report.sh` from the repo root before cutting a release, then edit the generated report to check off the GUI items, fill in the tester/notes, and commit the file under `docs/smoke/`.
