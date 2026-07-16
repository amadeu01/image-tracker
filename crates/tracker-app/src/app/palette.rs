//! Theme-aware status colors (task 12.4).
//!
//! Before this module, `banner.rs`/`side_panel.rs` hardcoded a single set of
//! `Color32`s (tuned by eye against egui's default *dark* panel background)
//! for status/severity indicators — the green/yellow/red of the tracking
//! state row, the events feed, and the mode banner. Once a light theme is
//! selectable (12.4's toggle), those same RGB triples sit on a light-gray
//! panel instead, and several of them (bright yellow-on-white especially)
//! lose most of their contrast.
//!
//! Fix: every call site asks for a color through the functions below,
//! passing `ui.visuals().dark_mode` (or an explicit bool in the banner,
//! which owns its own background) rather than embedding a `Color32`
//! literal. Each function returns one of two fixed palettes chosen for
//! contrast against its known background (egui's default dark/light panel
//! fill), not a single compromise color — see the doc comment on each
//! variant, and the `readable_against` test helper.

use eframe::egui::Color32;

/// One severity/status kind shared by the tracking-state row, the last-error
/// line, and the events feed — the exact set `side_panel.rs` needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Error,
    Warn,
    Success,
    /// Neutral/idle/searching — deliberately muted, not a full text color.
    Neutral,
    /// Informational highlight (e.g. the guide's current-step marker).
    Info,
}

/// Returns a `Color32` for `kind` that stays readable against egui's default
/// panel background for `dark_mode`. Two independent palettes rather than
/// one shared one: the dark palette leans bright/saturated (readable on a
/// near-black fill), the light palette leans deep/desaturated (readable on
/// a near-white fill) — a single compromise color can't do both well, which
/// was exactly the light-mode bug this module fixes (e.g. the old warn
/// yellow `(230, 200, 60)` has a contrast ratio under 1.6:1 against white).
pub fn status_color(dark_mode: bool, kind: StatusKind) -> Color32 {
    if dark_mode {
        match kind {
            StatusKind::Error => Color32::from_rgb(230, 70, 70),
            StatusKind::Warn => Color32::from_rgb(230, 200, 60),
            StatusKind::Success => Color32::from_rgb(90, 200, 110),
            StatusKind::Neutral => Color32::from_rgb(150, 150, 150),
            StatusKind::Info => Color32::from_rgb(90, 170, 255),
        }
    } else {
        match kind {
            StatusKind::Error => Color32::from_rgb(180, 30, 30),
            StatusKind::Warn => Color32::from_rgb(150, 105, 0),
            StatusKind::Success => Color32::from_rgb(20, 120, 40),
            StatusKind::Neutral => Color32::from_rgb(90, 90, 90),
            StatusKind::Info => Color32::from_rgb(20, 90, 190),
        }
    }
}

/// Background/text pair for the mode banner (`banner.rs`), one per
/// "temperature" the banner currently expresses (working / done / action
/// needed / neutral). Unlike `status_color`, the banner owns its own
/// background fill (it's not sitting on the ambient panel color), so both
/// halves of the pair are chosen together per theme rather than reused from
/// `status_color`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerKind {
    Working,
    Done,
    ActionNeeded,
    Neutral,
}

/// Returns `(background, text)` for `kind`, readable in both themes because
/// each variant carries its own theme-appropriate contrast pair rather than
/// reusing one dark-tuned fill with fixed white text (the pre-12.4 bug:
/// white-on-navy is fine, but the *same* navy suddenly reads as an odd,
/// low-contrast smudge floating in an otherwise light UI — legible strictly
/// speaking, but the intent was a glanceable, theme-consistent strip).
pub fn banner_colors(dark_mode: bool, kind: BannerKind) -> (Color32, Color32) {
    if dark_mode {
        let bg = match kind {
            BannerKind::Working => Color32::from_rgb(40, 70, 110),
            BannerKind::Done => Color32::from_rgb(35, 90, 55),
            BannerKind::ActionNeeded => Color32::from_rgb(90, 70, 20),
            BannerKind::Neutral => Color32::from_rgb(45, 45, 45),
        };
        (bg, Color32::WHITE)
    } else {
        let bg = match kind {
            BannerKind::Working => Color32::from_rgb(200, 220, 245),
            BannerKind::Done => Color32::from_rgb(200, 235, 210),
            BannerKind::ActionNeeded => Color32::from_rgb(250, 230, 180),
            BannerKind::Neutral => Color32::from_rgb(225, 225, 225),
        };
        (bg, Color32::BLACK)
    }
}

/// Per-rep velocity-loss severity vs the stop-set threshold (task 13.5's
/// VBT design), a distinct classification from `StatusKind`/`BannerKind`
/// (those are generic app-status colors; this one has its own design-spec
/// hex values, see `loss_severity_color`). `Ok` is under half the
/// threshold, `Warn` is at or above half but under the full threshold,
/// `Over` has reached (or passed) the stop-set threshold itself — exactly
/// the design's "green < threshold/2 <= amber < threshold <= red" rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LossSeverity {
    Ok,
    Warn,
    Over,
}

/// Classifies a rep's velocity `loss` (%) against the stop-set
/// `threshold_pct`, per the design's banding rule (see `LossSeverity`).
pub fn loss_severity(loss: f64, threshold_pct: f64) -> LossSeverity {
    if loss >= threshold_pct {
        LossSeverity::Over
    } else if loss >= threshold_pct / 2.0 {
        LossSeverity::Warn
    } else {
        LossSeverity::Ok
    }
}

/// Returns the design's loss-severity color for `severity`, theme-aware.
/// Dark-mode values are the design mock's exact hex triples (`#3fbf77`
/// green / `#d9a53f` amber / `#e05252` red) rather than the nearby-but-not-
/// identical `StatusKind::Success`/`Warn`/`Error` dark colors, since this is
/// specifically the VBT loss-column/chart palette the design specifies.
/// Light-mode values reuse `StatusKind::Success`/`Warn`/`Error`'s existing
/// light colors (already proven >=3:1 against the light panel background by
/// `status_colors_are_readable_against_their_own_panel_background` below)
/// rather than inventing new ones.
pub fn loss_severity_color(dark_mode: bool, severity: LossSeverity) -> Color32 {
    if dark_mode {
        match severity {
            LossSeverity::Ok => Color32::from_rgb(0x3f, 0xbf, 0x77),
            LossSeverity::Warn => Color32::from_rgb(0xd9, 0xa5, 0x3f),
            LossSeverity::Over => Color32::from_rgb(0xe0, 0x52, 0x52),
        }
    } else {
        match severity {
            LossSeverity::Ok => Color32::from_rgb(20, 120, 40),
            LossSeverity::Warn => Color32::from_rgb(150, 105, 0),
            LossSeverity::Over => Color32::from_rgb(180, 30, 30),
        }
    }
}

/// Relative luminance (WCAG-style, sRGB-approximated) used only by the
/// tests below to assert a text/background pair is actually readable,
/// rather than eyeballing hex triples.
#[cfg(test)]
fn relative_luminance(c: Color32) -> f64 {
    fn chan(v: u8) -> f64 {
        let v = v as f64 / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * chan(c.r()) + 0.7152 * chan(c.g()) + 0.0587 * chan(c.b())
}

/// WCAG contrast ratio between two colors (1.0 = no contrast, 21.0 = max).
#[cfg(test)]
fn contrast_ratio(a: Color32, b: Color32) -> f64 {
    let (l1, l2) = (relative_luminance(a), relative_luminance(b));
    let (lighter, darker) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (lighter + 0.05) / (darker + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two panel backgrounds these colors are actually drawn against
    /// (egui 0.29's default `Visuals::dark()`/`light()` panel fill).
    const DARK_PANEL_BG: Color32 = Color32::from_rgb(27, 27, 27);
    const LIGHT_PANEL_BG: Color32 = Color32::from_rgb(248, 248, 248);

    const ALL_KINDS: [StatusKind; 5] = [
        StatusKind::Error,
        StatusKind::Warn,
        StatusKind::Success,
        StatusKind::Neutral,
        StatusKind::Info,
    ];

    #[test]
    fn status_colors_differ_between_dark_and_light_mode() {
        for kind in ALL_KINDS {
            assert_ne!(
                status_color(true, kind),
                status_color(false, kind),
                "{kind:?} should use a different color per theme"
            );
        }
    }

    #[test]
    fn status_colors_are_readable_against_their_own_panel_background() {
        // WCAG AA "large text" minimum is 3:1; these are colored labels
        // (often bold/heading-adjacent), so 3:1 is the right bar rather
        // than the stricter 4.5:1 for body text.
        for kind in ALL_KINDS {
            let dark = status_color(true, kind);
            let light = status_color(false, kind);
            assert!(
                contrast_ratio(dark, DARK_PANEL_BG) >= 3.0,
                "{kind:?} dark-mode color {dark:?} too low contrast on dark panel"
            );
            assert!(
                contrast_ratio(light, LIGHT_PANEL_BG) >= 3.0,
                "{kind:?} light-mode color {light:?} too low contrast on light panel"
            );
        }
    }

    #[test]
    fn banner_colors_have_readable_text_on_their_own_background_in_both_themes() {
        for kind in [
            BannerKind::Working,
            BannerKind::Done,
            BannerKind::ActionNeeded,
            BannerKind::Neutral,
        ] {
            for dark_mode in [true, false] {
                let (bg, text) = banner_colors(dark_mode, kind);
                assert_ne!(bg, text);
                assert!(
                    contrast_ratio(bg, text) >= 4.5,
                    "{kind:?} dark_mode={dark_mode} bg={bg:?} text={text:?} contrast too low"
                );
            }
        }
    }

    #[test]
    fn loss_severity_bands_match_the_design_rule() {
        // green < threshold/2 <= amber < threshold <= red, threshold=20.
        assert_eq!(loss_severity(9.9, 20.0), LossSeverity::Ok);
        assert_eq!(loss_severity(10.0, 20.0), LossSeverity::Warn);
        assert_eq!(loss_severity(19.9, 20.0), LossSeverity::Warn);
        assert_eq!(loss_severity(20.0, 20.0), LossSeverity::Over);
        assert_eq!(loss_severity(40.0, 20.0), LossSeverity::Over);
        assert_eq!(loss_severity(-5.0, 20.0), LossSeverity::Ok);
    }

    #[test]
    fn loss_severity_dark_colors_match_the_design_hex_values() {
        assert_eq!(
            loss_severity_color(true, LossSeverity::Ok),
            Color32::from_rgb(0x3f, 0xbf, 0x77)
        );
        assert_eq!(
            loss_severity_color(true, LossSeverity::Warn),
            Color32::from_rgb(0xd9, 0xa5, 0x3f)
        );
        assert_eq!(
            loss_severity_color(true, LossSeverity::Over),
            Color32::from_rgb(0xe0, 0x52, 0x52)
        );
    }

    #[test]
    fn loss_severity_colors_differ_between_dark_and_light_mode() {
        for severity in [LossSeverity::Ok, LossSeverity::Warn, LossSeverity::Over] {
            assert_ne!(
                loss_severity_color(true, severity),
                loss_severity_color(false, severity)
            );
        }
    }

    #[test]
    fn loss_severity_colors_are_readable_against_their_own_panel_background() {
        for severity in [LossSeverity::Ok, LossSeverity::Warn, LossSeverity::Over] {
            let dark = loss_severity_color(true, severity);
            let light = loss_severity_color(false, severity);
            assert!(
                contrast_ratio(dark, DARK_PANEL_BG) >= 3.0,
                "{severity:?} dark color {dark:?} too low contrast on dark panel"
            );
            assert!(
                contrast_ratio(light, LIGHT_PANEL_BG) >= 3.0,
                "{severity:?} light color {light:?} too low contrast on light panel"
            );
        }
    }

    #[test]
    fn banner_backgrounds_differ_between_dark_and_light_mode() {
        for kind in [
            BannerKind::Working,
            BannerKind::Done,
            BannerKind::ActionNeeded,
            BannerKind::Neutral,
        ] {
            let (dark_bg, _) = banner_colors(true, kind);
            let (light_bg, _) = banner_colors(false, kind);
            assert_ne!(dark_bg, light_bg);
        }
    }
}
