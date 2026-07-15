//! Top toolbar (task 7.2 split): Place Seed / Calibrate toggles, known
//! length field, and the Track/Resume action. Thin — all decisions
//! (mode toggling, whether Track is enabled, resuming) live on `AppState`.

use eframe::egui;

use super::state::{AppState, Mode};

pub fn show(ctx: &egui::Context, state: &mut AppState) {
    egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
        ui.horizontal(|ui| {
            let label = match state.mode {
                Mode::ViewOnly => "Place Seed",
                Mode::PlacingSeed => "Placing Seed... (click frame)",
                Mode::Calibrating { .. } => "Place Seed",
            };
            if ui.selectable_label(state.mode == Mode::PlacingSeed, label).clicked() {
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
            if ui.selectable_label(calibrating, cal_label).clicked() {
                state.toggle_calibrating();
            }
            // Key toggle, 'c' for calibration.
            if ui.ctx().input(|i| i.key_pressed(egui::Key::C)) {
                state.toggle_calibrating();
            }

            if let Mode::Calibrating {
                known_length_meters, ..
            } = state.mode
            {
                ui.label("known length (m):");
                let mut meters = known_length_meters;
                if ui
                    .add(egui::DragValue::new(&mut meters).speed(0.001).range(0.001..=10.0))
                    .changed()
                {
                    state.set_calibration_length(meters);
                }
            }

            ui.separator();

            let paused = state.tracking_run.session_state
                == Some(tracker_core::SessionState::NeedsReseed);
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
                    .clicked()
                {
                    state.resume_tracking();
                }
            } else if ui
                .add_enabled(state.can_start_tracking(), egui::Button::new("Track"))
                .clicked()
            {
                state.start_tracking();
            }
        });
    });
}
