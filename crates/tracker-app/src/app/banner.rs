//! Mode instruction banner (task 10.7): a colored strip between the toolbar
//! and the video panel that tells the user what clicking will currently do.
//!
//! All the actual text/logic lives in `AppState::banner_text`/`phase`
//! (pure, unit-tested in `state.rs`); this module is just the color mapping
//! and the egui panel wiring. No-op (renders nothing) with no video loaded —
//! there's no mode to explain yet.

use eframe::egui;

use super::state::{AppState, Mode, Phase};

pub fn show(ctx: &egui::Context, state: Option<&AppState>) {
    let Some(state) = state else {
        return;
    };
    let color = banner_color(state);
    egui::TopBottomPanel::top("mode_banner")
        .frame(
            egui::Frame::default()
                .fill(color)
                .inner_margin(egui::Margin::symmetric(10.0, 6.0)),
        )
        .show_separator_line(false)
        .show(ctx, |ui| {
            ui.colored_label(banner_text_color(color), state.banner_text());
        });
}

/// Background color for the banner: distinct per mode/phase so the strip is
/// glanceable even before reading it (task 10.7's "colored background per
/// mode"). Falls back to a neutral gray for the between-steps idle state.
fn banner_color(state: &AppState) -> egui::Color32 {
    match state.phase() {
        Phase::TrackingPath { .. } => egui::Color32::from_rgb(40, 70, 110), // blue: working
        Phase::ComputingMetrics => egui::Color32::from_rgb(40, 70, 110),
        Phase::Review => egui::Color32::from_rgb(35, 90, 55), // green: done
        Phase::Idle => match state.mode {
            Mode::PlacingSeed => egui::Color32::from_rgb(90, 70, 20), // amber: action needed
            Mode::Calibrating { .. } => egui::Color32::from_rgb(90, 70, 20),
            Mode::ViewOnly => egui::Color32::from_rgb(45, 45, 45), // neutral
        },
    }
}

/// White reads on every one of `banner_color`'s dark fills; kept as its own
/// function (rather than a hardcoded constant inline) so a future banner
/// color change stays paired with its readable text color.
fn banner_text_color(_bg: egui::Color32) -> egui::Color32 {
    egui::Color32::WHITE
}
