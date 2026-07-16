//! Bottom bars (task 7.2 split): a one-line status summary and the scrub
//! bar. Detailed status (seed/calibration/tracking breakdown, events) moved
//! to the side panel (`side_panel.rs`) — this stays intentionally terse.

use eframe::egui;

use super::palette;
use super::state::AppState;

/// Design's status bar (task 13.1) is "a monospace one-liner: file · frame ·
/// mode · seed · calibration" — `state.status_line()` already assembles the
/// mode/seed/calibration clause (see `state.rs`), so this restyle is font +
/// chrome only: every label goes through `egui::TextStyle::Monospace`
/// (numbers/paths line up instead of proportional-font jitter) and the
/// panel picks up the design's hairline top border via `chrome_palette`.
pub fn show_status_bar(ctx: &egui::Context, state: Option<&AppState>) {
    let dark_mode = ctx.style().visuals.dark_mode;
    let border = palette::chrome_palette(dark_mode).border;
    egui::TopBottomPanel::bottom("status_bar")
        .frame(egui::Frame::side_top_panel(&ctx.style()).stroke(egui::Stroke::new(1.0, border)))
        .show(ctx, |ui| {
            let Some(state) = state else {
                ui.label(
                    egui::RichText::new("no video open — Ctrl+O or \"Open video…\"").monospace(),
                );
                return;
            };
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{}  ·  frame {}/{}  ·  {}",
                        state.video_path.display(),
                        state.current_frame,
                        state.metadata.frame_count.unwrap_or(0).saturating_sub(1),
                        state.status_line(),
                    ))
                    .monospace(),
                );
                let tracking_active = state.tracking.is_some()
                    || state.tracking_run.error.is_some()
                    || state.bar_path.is_some();
                if tracking_active {
                    ui.separator();
                    let is_error = state.tracking_run.error.is_some();
                    let is_paused = state.tracking_run.session_state
                        == Some(tracker_core::SessionState::NeedsReseed);
                    let color = if is_error {
                        egui::Color32::RED
                    } else if is_paused {
                        egui::Color32::YELLOW
                    } else {
                        egui::Color32::LIGHT_GREEN
                    };
                    ui.label(
                        egui::RichText::new(state.tracking_run.status_line())
                            .monospace()
                            .color(color),
                    );
                }
                if !state.status.is_empty() {
                    ui.separator();
                    ui.label(
                        egui::RichText::new(&state.status)
                            .monospace()
                            .color(egui::Color32::RED),
                    );
                }
            });
        });
}

pub fn show_scrub_bar(ctx: &egui::Context, state: Option<&mut AppState>) {
    egui::TopBottomPanel::bottom("scrub_bar").show(ctx, |ui| {
        let Some(state) = state else {
            return;
        };
        ui.horizontal(|ui| {
            // Bigger prev/next buttons (10.6) -- easier click targets than
            // the old text-sized buttons, since these get used far more
            // often than a one-off toolbar action.
            let button_size = egui::vec2(56.0, 28.0);
            if ui
                .add_sized(button_size, egui::Button::new("<< prev"))
                .on_hover_text("previous frame (←)")
                .clicked()
            {
                state.prev_frame();
            }
            let max = state.metadata.frame_count.unwrap_or(1).saturating_sub(1);
            let mut frame_val = state.current_frame;
            let slider = ui
                .add(egui::Slider::new(&mut frame_val, 0..=max))
                .on_hover_text("scrub to a frame (←/→ = ±1, Shift+←/→ = ±10)");
            if slider.changed() {
                state.set_frame(frame_val as i64);
            }
            if ui
                .add_sized(button_size, egui::Button::new("next >>"))
                .on_hover_text("next frame (→)")
                .clicked()
            {
                state.next_frame();
            }
        });
    });
}
