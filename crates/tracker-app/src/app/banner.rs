//! Mode instruction banner (task 10.7): a colored strip between the toolbar
//! and the video panel that tells the user what clicking will currently do.
//!
//! All the actual text/logic lives in `AppState::banner_text`/`phase`
//! (pure, unit-tested in `state.rs`); this module is just the color mapping
//! and the egui panel wiring. No-op (renders nothing) with no video loaded —
//! there's no mode to explain yet.

use eframe::egui;

use super::palette::{self, BannerKind};
use super::state::{AppState, Mode, Phase};

pub fn show(ctx: &egui::Context, state: Option<&AppState>) {
    let Some(state) = state else {
        return;
    };
    let dark_mode = ctx.style().visuals.dark_mode;
    let (bg, text) = palette::banner_colors(dark_mode, banner_kind(state));
    let border = palette::chrome_palette(dark_mode).border;
    egui::TopBottomPanel::top("mode_banner")
        .frame(
            egui::Frame::default()
                .fill(bg)
                .inner_margin(egui::Margin::symmetric(10.0, 5.0))
                .stroke(egui::Stroke::new(1.0, border)),
        )
        .show_separator_line(false)
        .show(ctx, |ui| {
            // Design's hint bar is a single contextual sentence, terser
            // than the rest of the chrome — same text/color logic as
            // before this restyle, just a smaller point size so it reads
            // as a hint strip rather than another heading-weight line.
            ui.label(
                egui::RichText::new(state.banner_text())
                    .color(text)
                    .size(12.5),
            );
        });
}

/// Which of `palette::banner_colors`'s four "temperatures" the banner
/// currently expresses, distinct per mode/phase so the strip is glanceable
/// even before reading it (task 10.7's "colored background per mode").
/// Falls back to `Neutral` for the between-steps idle state.
fn banner_kind(state: &AppState) -> BannerKind {
    match state.phase() {
        Phase::TrackingPath { .. } => BannerKind::Working,
        Phase::ComputingMetrics => BannerKind::Working,
        Phase::Review => BannerKind::Done,
        Phase::Idle => match state.mode {
            Mode::PlacingSeed => BannerKind::ActionNeeded,
            Mode::Calibrating { .. } => BannerKind::ActionNeeded,
            Mode::ViewOnly => BannerKind::Neutral,
        },
    }
}
