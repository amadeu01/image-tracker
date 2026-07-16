# Roadmap

Features we've agreed on but deliberately deferred, so nothing gets lost. Near-term work lives in [PLAN.md](PLAN.md); this file is the longer horizon. When an item graduates, it moves into PLAN.md as sized tasks and gets a link here.

## v1 polish (next up — being planned in PLAN.md milestone 10)

- **Review state + Results panel**: explicit end-of-tracking state; rep count headline, per-rep metrics table, quality warnings; exports written next to the source video (`<video>.overlay.mp4`, `.csv`, `.json`, `.reps.csv`).
- **Stop-at-video-end bug fix** and honest "done" transition.
- **Lost-object honesty**: no wandering marker when the bar leaves frame; visible "lost" state.
- **In-app calibration guidance** and clearer controls (bigger nav, tooltips, keyboard shortcuts documented in-UI).
- **Better timeline navigation** (see Q&A in PLAN; thumbnail filmstrip candidate).
- **Live phase clarity**: show what stage the pipeline is in (tracking path → computing velocity → segmenting reps) and a live rep counter.

## Post-v1

- **Session history**: persist tracked sessions (`~/.local/share/image-tracker/sessions/`), re-open past results without re-tracking, compare sets over time.
- **Export file dialogs** (choose destination/name) as alternative to auto-write next to source.
- **End-of-set detection**: auto-trim tracking noise after the bar is racked (known limitation from 3.6 review).
- **Burned-in text on overlay video** (rep numbers, velocity) — needs font rendering; currently ticks/lines only.
- **Joint/pose tracking** (hip, knee, ankle angles) as a new `Tracker` via ONNX pose models — original phase-2 idea from the first grilling session.
- **Perspective correction** for non-perpendicular camera angles.
- **Self-hosted runner GUI smoke** in CI (PLAN 9.5, security prerequisites documented).
- **cargo-dist / homebrew tap** distribution polish (PLAN 7.3).
- **Sentry/Datadog telemetry layer** — slot into the existing `tracing` Layer stack.
- **Bar-only velocity zones / VBT targets**: configurable velocity thresholds with visual feedback per rep.
- **Full strategy parameter sweep / auto-tune** (beyond milestone 11's fixed-matrix `compare`): search over filter params + thresholds per video.
- **CLAHE / histogram equalization preprocessor**: deliberately skipped in milestone 11 (ZNCC is already contrast-invariant — see docs/theory.md); revisit only if a tracker that isn't contrast-invariant lands.
- **User-facing "how it works" explainer** (blog-style distillation of docs/theory.md).
