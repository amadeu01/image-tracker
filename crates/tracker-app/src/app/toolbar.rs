//! Top toolbar (task 7.2 split): Place Seed / Calibrate toggles, known
//! length field, and the Track/Resume action. Thin — all decisions
//! (mode toggling, whether Track is enabled, resuming) live on `AppState`.

use eframe::egui;

use super::palette;
use super::state::{DisplayMode, Mode};
use super::TrackerApp;

pub fn show(ctx: &egui::Context, app: &mut TrackerApp) {
    egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
        ui.horizontal(|ui| {
            // Theme toggle (task 12.4): always available, independent of
            // whether a video is loaded. Reflects the *effective* theme
            // (`ctx.style().visuals.dark_mode`), which follows the system
            // theme until the user overrides it here.
            let dark_mode = ctx.style().visuals.dark_mode;
            let (icon, hover) = if dark_mode {
                ("☀", "switch to light theme")
            } else {
                ("🌙", "switch to dark theme")
            };
            if ui.button(icon).on_hover_text(hover).clicked() {
                app.toggle_theme(ctx);
            }
            ui.separator();

            // Always available (10.5): opening a video works from an empty
            // window and any time afterward (loads a fresh session on the
            // new file).
            if ui
                .button("Open video…")
                .on_hover_text("open a video file to track (Ctrl+O)")
                .clicked()
            {
                app.prompt_open_video();
            }
            let ctrl_o = ui.ctx().input(|i| {
                i.key_pressed(egui::Key::O) && (i.modifiers.command || i.modifiers.ctrl)
            });
            if ctrl_o {
                app.prompt_open_video();
            }
            ui.separator();

            let Some(state) = app.state.as_mut() else {
                return;
            };

            let label = match state.mode {
                Mode::ViewOnly => "Place Seed",
                Mode::PlacingSeed => "Placing Seed... (click frame)",
                Mode::Calibrating { .. } => "Place Seed",
            };
            if ui
                .selectable_label(state.mode == Mode::PlacingSeed, label)
                .on_hover_text("click a frame to mark the object to track (S toggles this mode)")
                .clicked()
            {
                state.toggle_placing_seed();
            }
            // Key toggle, e.g. 's' for seed placement.
            if ui.ctx().input(|i| i.key_pressed(egui::Key::S)) {
                state.toggle_placing_seed();
            }

            ui.separator();

            let calibrating = matches!(state.mode, Mode::Calibrating { .. });
            let cal_label = if calibrating {
                "Calibrating... (click 2 points)"
            } else {
                "Calibrate"
            };
            if ui
                .selectable_label(calibrating, cal_label)
                .on_hover_text(
                    "click two points a known real-world distance apart, \
                     to convert pixels to meters (C toggles this mode)",
                )
                .clicked()
            {
                state.toggle_calibrating();
            }
            // Key toggle, 'c' for calibration.
            if ui.ctx().input(|i| i.key_pressed(egui::Key::C)) {
                state.toggle_calibrating();
            }

            if let Mode::Calibrating {
                known_length_meters,
                ..
            } = state.mode
            {
                ui.label("known length (m):");
                let mut meters = known_length_meters;
                if ui
                    .add(
                        egui::DragValue::new(&mut meters)
                            .speed(0.001)
                            .range(0.001..=10.0),
                    )
                    .on_hover_text(
                        "real-world distance between the two calibration points, in meters \
                         (defaults to a 0.450 m competition plate diameter)",
                    )
                    .changed()
                {
                    state.set_calibration_length(meters);
                }
            }

            ui.separator();

            let paused =
                state.tracking_run.session_state == Some(tracker_core::SessionState::NeedsReseed);
            if paused {
                // Nudge the user straight into placing a new seed on the
                // paused frame.
                if state.mode != Mode::PlacingSeed {
                    state.mode = Mode::PlacingSeed;
                }
                let ready = state
                    .seed
                    .map(|s| Some(s.frame_index) == state.tracking_run.last_frame_index)
                    .unwrap_or(false);
                ui.colored_label(egui::Color32::YELLOW, "tracking paused: click a new seed");
                if ui
                    .add_enabled(ready, egui::Button::new("Resume"))
                    .on_hover_text(
                        "resume tracking from the new seed you just placed on the paused frame",
                    )
                    .clicked()
                {
                    state.resume_tracking();
                }
            } else if state.tracking.is_some() {
                // Task 10.4: session lifecycle controls, shown while a run
                // is active (running or user-paused) instead of the Track
                // button — mirrors the reseed-pause branch above, which
                // swaps Track for its own Resume.
                if state.paused {
                    if ui
                        .button("Resume")
                        .on_hover_text("resume the paused tracking run")
                        .clicked()
                    {
                        state.unpause_tracking();
                    }
                } else if ui
                    .add_enabled(state.can_pause_tracking(), egui::Button::new("Pause"))
                    .on_hover_text("pause the active tracking run")
                    .clicked()
                {
                    state.pause_tracking();
                }
                if ui
                    .add_enabled(state.can_stop_tracking(), egui::Button::new("Stop"))
                    .on_hover_text("stop now and keep the results collected so far")
                    .clicked()
                {
                    state.stop_tracking();
                }
                if ui
                    .add_enabled(state.can_discard_tracking(), egui::Button::new("Discard"))
                    .on_hover_text("abort the run and throw away its results, keeping the seed")
                    .clicked()
                {
                    state.discard_tracking();
                }
            } else if ui
                .add_enabled(state.can_start_tracking(), egui::Button::new("Track"))
                .on_hover_text("start tracking the seeded object from its frame to the end")
                .clicked()
            {
                state.start_tracking();
            }

            if state.can_start_new_session() || state.can_retrack() {
                ui.separator();
                if ui
                    .add_enabled(state.can_retrack(), egui::Button::new("Re-track"))
                    .on_hover_text(
                        "clear this run's results and immediately start a new run \
                         from the same seed and calibration",
                    )
                    .clicked()
                {
                    state.retrack();
                }
                if ui
                    .add_enabled(
                        state.can_start_new_session(),
                        egui::Button::new("New session"),
                    )
                    .on_hover_text(
                        "clear the seed, calibration, and results and start over \
                         on the same video",
                    )
                    .clicked()
                {
                    state.start_new_session();
                }
            }

            // Live/Results pill toggle (task 13.1, design's toolbar-right
            // element). Must be the LAST child of the row: a nested
            // right-to-left layout claims all remaining row width, so
            // drawing it first starves every later widget of space and
            // makes the whole left toolbar vanish (found by fable-5 visual
            // review of f897584 — clippy/tests/smoke all passed with the
            // bug present).
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                display_mode_pill(ui, state);
            });
        });
    });
}

/// Draws the Live/Results pill toggle (task 13.1): two selectable labels
/// side by side, with a small dot next to "Live" that pulses (sine-wave
/// alpha) while `Live` is selected — the design's "recording" affordance.
/// Pure UI selection; task 13.6 owns building out the dedicated Live panel
/// content this switches to.
fn display_mode_pill(ui: &mut egui::Ui, state: &mut super::state::AppState) {
    let dark_mode = ui.visuals().dark_mode;
    let accent = palette::chrome_palette(dark_mode).accent;

    if ui
        .selectable_label(state.display_mode == DisplayMode::Results, "Results")
        .on_hover_text("show the finished set's rep table, chart, and headline cards")
        .clicked()
    {
        state.set_display_mode(DisplayMode::Results);
    }
    let live_selected = state.display_mode == DisplayMode::Live;
    if ui
        .selectable_label(live_selected, "Live")
        .on_hover_text("show the in-progress run: live rep count and phase")
        .clicked()
    {
        state.set_display_mode(DisplayMode::Live);
    }
    if live_selected {
        // Pulsing dot: alpha oscillates with wall-clock time via egui's
        // per-frame `input().time`, and `request_repaint` keeps frames
        // flowing so the pulse actually animates instead of freezing at
        // whatever alpha the last user-triggered repaint happened to catch.
        let t = ui.input(|i| i.time);
        let alpha = (0.5 + 0.5 * (t * 3.0).sin()) as f32;
        let dot_color = accent.gamma_multiply(0.4 + 0.6 * alpha);
        let (rect, _response) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, dot_color);
        ui.ctx().request_repaint();
    }
}
