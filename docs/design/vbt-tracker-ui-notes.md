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

## egui-native implementation mapping (fable-5 exploration, 2026-07-16)

How each remaining design element maps onto egui 0.29 primitives — workers on
13.2–13.6 follow this rather than re-deriving it:

- **Rep segments on scrub bar (13.2)**: not the existing `egui::Slider`. Draw a
  custom widget: `ui.allocate_exact_size(vec2(available_width, 34.0), Sense::click_and_drag())`,
  then `ui.painter().rect_filled/rect_stroke` per rep (`left = rect.left() + start/total * rect.width()`,
  same math as the mock's `leftPct`/`widthPct`). Hit-test clicks via
  `response.interact_pointer_pos()` against segment rects. Selected segment:
  `chrome_palette().accent` at full alpha border + `accent.gamma_multiply(0.45)` fill;
  unselected `gamma_multiply(0.16)` (mirrors mock's rgba values). Playhead + in/out
  markers: 2px `rect_filled` vlines. Keep the existing Slider *below* it or replace —
  segment bar itself gives coarse seek; retain Slider for fine scrub (deviation OK).
- **Rep table (13.3)**: `egui::Grid` doesn't do row backgrounds/click; use per-row
  `ui.horizontal` inside a `Frame::none().fill(row_bg)` or paint row rect first via
  `ui.painter().rect_filled` behind an `allocate_ui_at_rect`. Simpler: one
  `Sense::click` allocated row rect, paint 3px left border (`rect_filled` of a
  3×h strip in `loss_severity_color`), then lay text with `painter().text` at fixed
  x-offsets matching the mock's grid (36/70/82/82/64px columns). Monospace via
  `TextStyle::Monospace`. ▶ button: small `ui.put(rect, Button)` at row end.
- **Velocity chart (13.4)**: plain `ui.painter()` on an allocated rect, NOT
  egui_plot (new dep, and we need loss-colored clickable dots + dashed hlines —
  easier by hand). Dashed lines: `painter.add(Shape::dashed_line(&[p1, p2], stroke, 4.0, 4.0))`.
  Trend fit: port the mock's least-squares verbatim (`renderVals()` in the .dc.html,
  lines ~298–302) into tracker-core next to `velocity_loss_percent` (TDD).
  Dots: `circle_filled` + `Sense::click` hit-test by distance < r+2. Axis ticks:
  `painter.text` with `Align2::RIGHT_CENTER`. Chart geometry = mock's: plot area
  inset ~34px left, 30px bottom; y-range from data min/max padded, thresholds at
  `v1 * (1 - 0.1/0.2/0.3)`.
- **Live overlay on video (13.6)**: video_panel already paints over the frame
  texture; add `painter.text` big monospace number (size ~20→28) top-right inside
  a `rect_filled(rgba(20,20,22,0.85))` chip. Phase pulse dot: same sine-alpha
  pattern as toolbar's `display_mode_pill` (extract a shared
  `palette::pulse_alpha(time)` helper when 13.6 lands, don't copy-paste).
- **Progress bar w/ threshold marker (13.6)**: `egui::ProgressBar` can't do the
  marker; hand-paint 8px rounded rect + fill + 2px marker vline (same custom-rect
  pattern as the scrub segments).
- **Cards** (headline, REPS/PHASE): `egui::Frame::none().fill(chrome_palette().panel_bg)
  .stroke(Stroke::new(1.0, border)).rounding(6.0).inner_margin(14.0)` — wrap in a
  `section_card(ui, |ui| ..)` helper in side_panel.rs on first reuse (13.3 likely).
- **Fonts**: stay on egui defaults (notes above); `TextStyle::Monospace` everywhere
  numbers appear. IBM Plex embedding stays optional/ROADMAP.
- **Pill placement trap** (learned from f897584): any right-aligned toolbar group
  via `Layout::right_to_left` must be the LAST child of the row — drawn earlier it
  claims all remaining width and silently erases later widgets. Screenshot-check
  every UI task (Xvfb recipe: `env -u WAYLAND_DISPLAY xvfb-run` + ImageMagick
  `import -window root`; app must be given a video arg to show full chrome).

## Design-fidelity gap audit (fable-5, 2026-07-17 — user: "GUI not even close")

Side-by-side of the mock vs our dark-mode screenshot. 13.1 shipped pieces
(pill, section labels, monospace, palette constants) but never wired the
palette into egui's global style — root cause of the whole gap. → task 13.7:

1. `ChromePalette` → `egui::Style`/`Visuals` for both themes: `panel_fill`,
   `window_fill`, `extreme_bg_color`, widget `bg_fill`/`weak_bg_fill`,
   `bg_stroke` (hairlines #2c2c31), rounding 4.0 widgets / 6.0 cards,
   selection color = accent. Dark uses the design hex exactly; light keeps
   the existing contrast-tested equivalents. One function
   (`palette::apply_chrome(ctx, dark_mode)`) called on startup + theme
   toggle — never per-frame.
2. Card chrome helper (`section_card`) wrapping every side-panel section
   (guide, status, settings, results blocks already have headline cards —
   unify) in #1f1f24 fill + border + 6.0 rounding + 14px inner margin.
3. Right panel: 460px default (design), fill #18181b (new `side_bg` field
   on `ChromePalette`).
4. Hint bar: quiet strip (#202024 bg, muted text 12.5px), not a colored
   status banner; keep severity colors only for real warnings/errors.
5. Buttons: dark #26262b fill, #3a3a40 border, 4.0 rounding via the global
   widget visuals from (1) — no per-button styling.
6. Filmstrip: design replaced it with the segment scrub + transport row.
   Keep the strip available but collapsed by default once results exist
   (segments become the primary navigation); still shown pre-tracking.
7. Verification: dark-mode Xvfb screenshot (set `dark_mode:true` in a
   scratch XDG_STATE_HOME, don't touch the user's real settings.json)
   compared against the mock at matching window size; light mode contrast
   tests must stay green.

## Deliberate deviations

- Design's "▶" implies actual clip playback; v1 in-app playback = playhead loop between in/out via existing seek decoder (real-time-ish), true decoded playback stays on ROADMAP.
- SVG chart → egui painter (or egui_plot) equivalent; visual goals (threshold dashes, colored dots, trend) preserved.
