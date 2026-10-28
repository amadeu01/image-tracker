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

/// What the user asked for through the banner's buttons. `Confirm` is handled
/// by `TrackerApp` (it also needs the frame cache for the tracker suggestion);
/// Cancel and "Not the bar" are applied directly to the state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerAction {
    ConfirmProposal,
}

pub fn show(ctx: &egui::Context, state: Option<&mut AppState>) -> Option<BannerAction> {
    let state = state?;
    let mut action = None;
    let dark_mode = ctx.style().visuals.dark_mode;
    let chrome = palette::chrome_palette(dark_mode);
    // Task 13.7: informational states render as the design's *quiet* hint
    // strip (#202024 bg / #9a9aa2 text — `chrome.hint_bg`/`hint_text`)
    // rather than a bright tinted banner; only `ActionNeeded` (a real
    // "you must click something" state) keeps its severity colors from
    // `palette::banner_colors` (whose contrast tests are unchanged).
    let kind = banner_kind(state);
    let (bg, text) = match kind {
        BannerKind::ActionNeeded => palette::banner_colors(dark_mode, kind),
        BannerKind::Working | BannerKind::Done | BannerKind::Neutral => {
            (chrome.hint_bg, chrome.hint_text)
        }
    };
    let border = chrome.border;
    egui::TopBottomPanel::top("mode_banner")
        .frame(
            egui::Frame::default()
                .fill(bg)
                .inner_margin(egui::Margin::symmetric(10.0, 5.0))
                .stroke(egui::Stroke::new(1.0f32, border)),
        )
        .show_separator_line(false)
        .show(ctx, |ui| {
            // Design's hint bar is a single contextual sentence, terser
            // than the rest of the chrome — same text/color logic as
            // before this restyle, just a smaller point size so it reads
            // as a hint strip rather than another heading-weight line.
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(state.banner_text())
                        .color(text)
                        .size(12.5),
                );
                if state.is_finding_bar() && ui.button("Cancel").clicked() {
                    state.cancel_prepass();
                }
                if state.proposal().is_some() {
                    if ui.button("Confirm").clicked() {
                        action = Some(BannerAction::ConfirmProposal);
                    }
                    if ui.button("Not the bar").clicked() {
                        state.reject_proposal();
                    }
                }
            });
        });
    action
}

/// Which of `palette::banner_colors`'s four "temperatures" the banner
/// currently expresses, distinct per mode/phase so the strip is glanceable
/// even before reading it (task 10.7's "colored background per mode").
/// Falls back to `Neutral` for the between-steps idle state.
fn banner_kind(state: &AppState) -> BannerKind {
    if state.is_finding_bar() {
        return BannerKind::Working;
    }
    if state.proposal().is_some() {
        return BannerKind::ActionNeeded;
    }
    match state.phase() {
        Phase::TrackingPath { .. } => BannerKind::Working,
        Phase::ComputingMetrics => BannerKind::Working,
        Phase::Review => BannerKind::Done,
        Phase::Idle => match state.mode() {
            Mode::PlacingSeed => BannerKind::ActionNeeded,
            Mode::ViewOnly if state.prepass_notice().is_some() && state.seed.is_none() => {
                BannerKind::ActionNeeded
            }
            Mode::Calibrating { .. } => BannerKind::ActionNeeded,
            Mode::ViewOnly => BannerKind::Neutral,
        },
    }
}
