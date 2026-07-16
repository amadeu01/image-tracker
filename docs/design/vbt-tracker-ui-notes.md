# VBT Tracker UI — design import notes

Source: Claude Design project "UI Recommendations For VBT Tracking"
(`claude.ai/design/p/7697900c-8d31-4f44-a820-2d52ad8edfe1`, file `VBT Tracker UI.dc.html`, imported 2026-07-16).
The original interactive mock is `vbt-tracker-ui.dc.html` in this directory — open in a browser to interact (Live/Results toggle, rep selection, clip markers).

## What the design specifies (mapped to plan tasks, milestone 13)

- **Live / Results mode toggle** (toolbar right side, pill toggle with pulsing live dot) — two side-panel layouts sharing one shell. → 13.1/13.6
- **Hint bar** under the toolbar (single contextual sentence, replaces nothing — our banner already does this; restyle). → 13.1
- **Rep segments on the scrub bar**: one clickable block per rep positioned by start/end frame, selected rep highlighted, in/out markers when a rep clip is active. → 13.2
- **Rep table** (Results): #, depth, peak V, mean V, loss %, time range, ▶ play-clip button; rows click-to-jump; left border + loss value colored green/amber/red by loss vs threshold/2 and threshold. → 13.3
- **Per-rep clips**: ▶ scrubs playhead to rep start with in/out markers; export writes `video.repNN.mp4` via ffmpeg stream copy (no re-encode). → 13.3
- **Velocity chart** (Results): mean concentric velocity by rep — connected line + per-rep dots colored by loss, dashed threshold lines at −10/−20/−30% of rep 1, linear trend line, y ticks in m/s (or px/s uncalibrated). → 13.4
- **Headline cards**: REPS / SET TIME / VEL. LOSS (loss red when over threshold). → 13.5
- **Stop-set threshold** (config, default 20%, range 5–40): banner "Stop set recommended — velocity loss reached X% at rep N" when exceeded. → 13.5
- **Uncalibrated warning** chip with Calibrate link when showing px/s. → 13.5
- **Live mode panel**: REPS count card ("rep 6 in progress"), PHASE card (Eccentric/Bottom/Concentric with pulsing dot), velocity-loss progress bar vs rep 1 with "stop at N%" marker, completed-reps mini table, live velocity readout overlaid on the video (big number + CONCENTRIC ▲). → 13.6
- **Status bar**: monospace one-liner (file · frame · mode · seed · calibration). Already exists; restyle. → 13.1

## Palette / typography (translate, don't copy blindly — must work in both themes)

- Dark bg #141416 / panels #1f1f24 / borders #2c2c31 — close to current egui dark; map through `palette.rs`, provide light equivalents.
- Accent blue #6ea3ec (path, selection), green #3fbf77 (ok/concentric), amber #d9a53f (warn), red #e05252 (over threshold).
- Monospace for all numbers (egui: `TextStyle::Monospace`); uppercase letter-spaced 11px section labels.
- egui has no IBM Plex by default — acceptable to ship with default fonts first; font embedding optional later.

## Deliberate deviations

- Design's "▶" implies actual clip playback; v1 in-app playback = playhead loop between in/out via existing seek decoder (real-time-ish), true decoded playback stays on ROADMAP.
- SVG chart → egui painter (or egui_plot) equivalent; visual goals (threshold dashes, colored dots, trend) preserved.
