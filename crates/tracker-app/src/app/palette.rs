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

/// Chrome colors for the shell restyle (task 13.1): app background, panel
/// fill, hairline borders, and the single accent blue used for the
/// Live/Results toggle's pulsing dot and section-label emphasis. Distinct
/// from `StatusKind`/`LossSeverity` (those color *values*/severity, this
/// colors structural chrome), but same "translate, don't copy" rule from
/// the design notes: dark-mode uses the mock's exact hex
/// (`#141416`/`#1f1f24`/`#2c2c31`/`#6ea3ec`); light-mode is a fresh pick
/// tuned for contrast against a near-white panel rather than a lightened
/// version of the dark hex (the same reasoning `loss_severity_color`
/// documents above).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChromePalette {
    pub app_bg: Color32,
    pub panel_bg: Color32,
    pub border: Color32,
    pub accent: Color32,
    /// Right side-panel fill (design `#18181b`): between `app_bg` and
    /// `panel_bg` so the cards on top of it still read as raised.
    pub side_bg: Color32,
    /// Top toolbar strip fill (design `#1d1d21`).
    pub toolbar_bg: Color32,
    /// Button/interactive-widget fill (design `#26262b`).
    pub button_bg: Color32,
    /// Button border (design `#3a3a40`), also used as the hovered stroke.
    pub button_border: Color32,
    /// Quiet hint-bar strip background (design `#202024`).
    pub hint_bg: Color32,
    /// Quiet hint-bar text (design `#9a9aa2`).
    pub hint_text: Color32,
}

/// Returns the shell chrome palette for `dark_mode`. See `ChromePalette`.
pub fn chrome_palette(dark_mode: bool) -> ChromePalette {
    if dark_mode {
        ChromePalette {
            app_bg: Color32::from_rgb(0x14, 0x14, 0x16),
            panel_bg: Color32::from_rgb(0x1f, 0x1f, 0x24),
            border: Color32::from_rgb(0x2c, 0x2c, 0x31),
            accent: Color32::from_rgb(0x6e, 0xa3, 0xec),
            side_bg: Color32::from_rgb(0x18, 0x18, 0x1b),
            toolbar_bg: Color32::from_rgb(0x1d, 0x1d, 0x21),
            button_bg: Color32::from_rgb(0x26, 0x26, 0x2b),
            button_border: Color32::from_rgb(0x3a, 0x3a, 0x40),
            hint_bg: Color32::from_rgb(0x20, 0x20, 0x24),
            hint_text: Color32::from_rgb(0x9a, 0x9a, 0xa2),
        }
    } else {
        ChromePalette {
            app_bg: Color32::from_rgb(0xf5, 0xf5, 0xf7),
            panel_bg: Color32::from_rgb(0xff, 0xff, 0xff),
            border: Color32::from_rgb(0xd8, 0xd8, 0xdc),
            // Darker than the design's dark-mode accent so text/dots drawn
            // in it stay >=3:1 against a near-white panel (the dark hex
            // 0x6ea3ec fails that against white).
            accent: Color32::from_rgb(0x2f, 0x6f, 0xd1),
            // Slightly-off-white so the white cards still read as raised —
            // the same panel_bg/side_bg relationship the dark theme has.
            side_bg: Color32::from_rgb(0xec, 0xec, 0xef),
            toolbar_bg: Color32::from_rgb(0xea, 0xea, 0xee),
            button_bg: Color32::from_rgb(0xff, 0xff, 0xff),
            button_border: Color32::from_rgb(0xc4, 0xc4, 0xcb),
            hint_bg: Color32::from_rgb(0xe7, 0xe7, 0xea),
            hint_text: Color32::from_rgb(0x55, 0x55, 0x5c),
        }
    }
}

/// Builds the app's global `egui::Visuals` from `chrome_palette` (task
/// 13.7). This is the fix for the 13.4 user finding "GUI not even close to
/// the design": 13.1 defined `ChromePalette` but every panel/widget still
/// rendered egui's stock visuals because nothing ever pushed the palette
/// into the global `Style`. Pure (no `Context`), so tests can assert the
/// mapping; `apply_chrome` is the one-liner that installs it.
pub fn chrome_visuals(dark_mode: bool) -> eframe::egui::Visuals {
    use eframe::egui::{Rounding, Stroke, Visuals};
    let p = chrome_palette(dark_mode);
    let mut v = if dark_mode {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    v.panel_fill = p.app_bg;
    v.window_fill = p.panel_bg;
    v.extreme_bg_color = p.app_bg;
    v.faint_bg_color = p.button_bg;
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = p.accent.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, p.accent);

    let rounding = Rounding::same(4.0);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.app_bg;
    w.noninteractive.weak_bg_fill = p.panel_bg;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.rounding = rounding;
    w.inactive.bg_fill = p.button_bg;
    w.inactive.weak_bg_fill = p.button_bg;
    w.inactive.bg_stroke = Stroke::new(1.0, p.button_border);
    w.inactive.rounding = rounding;
    // Hover/active: the mock's hover is `#2e2e34` — one step lighter than
    // the resting button; derive both from `button_bg` so light mode gets
    // the equivalent relationship (slightly darker there) for free.
    let (hovered_bg, active_bg) = if dark_mode {
        (
            Color32::from_rgb(0x2e, 0x2e, 0x34),
            Color32::from_rgb(0x35, 0x35, 0x3c),
        )
    } else {
        (
            Color32::from_rgb(0xf0, 0xf0, 0xf3),
            Color32::from_rgb(0xe4, 0xe4, 0xe9),
        )
    };
    w.hovered.bg_fill = hovered_bg;
    w.hovered.weak_bg_fill = hovered_bg;
    w.hovered.bg_stroke = Stroke::new(1.0, p.button_border);
    w.hovered.rounding = rounding;
    w.active.bg_fill = active_bg;
    w.active.weak_bg_fill = active_bg;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.rounding = rounding;
    w.open.bg_fill = p.button_bg;
    w.open.weak_bg_fill = p.button_bg;
    w.open.bg_stroke = Stroke::new(1.0, p.button_border);
    w.open.rounding = rounding;
    v
}

/// Installs `chrome_visuals` on the context. Called at startup, on the
/// theme toggle, and whenever `TrackerApp::update` detects the effective
/// visuals have drifted back to stock (e.g. a system `ThemeChanged`
/// re-application) — never unconditionally per-frame.
pub fn apply_chrome(ctx: &eframe::egui::Context, dark_mode: bool) {
    ctx.set_visuals(chrome_visuals(dark_mode));
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
    fn chrome_palette_differs_between_dark_and_light_mode() {
        let dark = chrome_palette(true);
        let light = chrome_palette(false);
        assert_ne!(dark.app_bg, light.app_bg);
        assert_ne!(dark.panel_bg, light.panel_bg);
        assert_ne!(dark.border, light.border);
        assert_ne!(dark.accent, light.accent);
    }

    #[test]
    fn chrome_palette_dark_hex_matches_the_design_mock() {
        let dark = chrome_palette(true);
        assert_eq!(dark.app_bg, Color32::from_rgb(0x14, 0x14, 0x16));
        assert_eq!(dark.panel_bg, Color32::from_rgb(0x1f, 0x1f, 0x24));
        assert_eq!(dark.border, Color32::from_rgb(0x2c, 0x2c, 0x31));
        assert_eq!(dark.accent, Color32::from_rgb(0x6e, 0xa3, 0xec));
    }

    #[test]
    fn chrome_palette_accent_is_readable_against_its_own_panel_background() {
        // 3:1 (WCAG "large text"/UI-component bar) rather than 4.5:1: the
        // accent is used for bold small labels and a status dot, not body
        // copy.
        for dark_mode in [true, false] {
            let p = chrome_palette(dark_mode);
            assert!(
                contrast_ratio(p.accent, p.panel_bg) >= 3.0,
                "dark_mode={dark_mode} accent {:?} too low contrast on panel {:?}",
                p.accent,
                p.panel_bg
            );
        }
    }

    #[test]
    fn chrome_palette_panel_and_border_are_distinguishable() {
        for dark_mode in [true, false] {
            let p = chrome_palette(dark_mode);
            assert_ne!(p.panel_bg, p.border);
        }
    }

    #[test]
    fn chrome_palette_dark_shell_hex_matches_the_design_mock_13_7() {
        let p = chrome_palette(true);
        assert_eq!(p.side_bg, Color32::from_rgb(0x18, 0x18, 0x1b));
        assert_eq!(p.toolbar_bg, Color32::from_rgb(0x1d, 0x1d, 0x21));
        assert_eq!(p.button_bg, Color32::from_rgb(0x26, 0x26, 0x2b));
        assert_eq!(p.button_border, Color32::from_rgb(0x3a, 0x3a, 0x40));
        assert_eq!(p.hint_bg, Color32::from_rgb(0x20, 0x20, 0x24));
        assert_eq!(p.hint_text, Color32::from_rgb(0x9a, 0x9a, 0xa2));
    }

    #[test]
    fn chrome_palette_shell_colors_differ_between_dark_and_light_mode() {
        let dark = chrome_palette(true);
        let light = chrome_palette(false);
        assert_ne!(dark.side_bg, light.side_bg);
        assert_ne!(dark.toolbar_bg, light.toolbar_bg);
        assert_ne!(dark.button_bg, light.button_bg);
        assert_ne!(dark.button_border, light.button_border);
        assert_ne!(dark.hint_bg, light.hint_bg);
        assert_ne!(dark.hint_text, light.hint_text);
    }

    #[test]
    fn hint_text_is_readable_against_hint_bg_in_both_themes() {
        // The quiet hint strip's own text/background pair (design's
        // #9a9aa2-on-#202024). 3:1 minimum, per the task spec; both themes
        // comfortably exceed it.
        for dark_mode in [true, false] {
            let p = chrome_palette(dark_mode);
            assert!(
                contrast_ratio(p.hint_text, p.hint_bg) >= 3.0,
                "dark_mode={dark_mode} hint_text {:?} too low contrast on hint_bg {:?}",
                p.hint_text,
                p.hint_bg
            );
        }
    }

    #[test]
    fn default_text_is_readable_against_every_new_shell_background() {
        // The default egui text color must stay readable on each surface
        // the shell now paints (buttons/toolbar/side panel/cards).
        for dark_mode in [true, false] {
            let p = chrome_palette(dark_mode);
            let text = chrome_visuals(dark_mode).widgets.inactive.fg_stroke.color;
            for (name, bg) in [
                ("side_bg", p.side_bg),
                ("toolbar_bg", p.toolbar_bg),
                ("button_bg", p.button_bg),
                ("panel_bg", p.panel_bg),
                ("app_bg", p.app_bg),
            ] {
                assert!(
                    contrast_ratio(text, bg) >= 4.5,
                    "dark_mode={dark_mode} default text {text:?} vs {name} {bg:?} too low"
                );
            }
        }
    }

    #[test]
    fn chrome_visuals_maps_the_palette_onto_egui_visuals() {
        for dark_mode in [true, false] {
            let p = chrome_palette(dark_mode);
            let v = chrome_visuals(dark_mode);
            assert_eq!(v.dark_mode, dark_mode);
            assert_eq!(v.panel_fill, p.app_bg);
            assert_eq!(v.window_fill, p.panel_bg);
            assert_eq!(v.extreme_bg_color, p.app_bg);
            assert_eq!(v.hyperlink_color, p.accent);
            assert_eq!(v.selection.stroke.color, p.accent);
            assert_eq!(v.widgets.inactive.bg_fill, p.button_bg);
            assert_eq!(v.widgets.inactive.bg_stroke.color, p.button_border);
            assert_eq!(v.widgets.noninteractive.bg_stroke.color, p.border);
            assert_eq!(
                v.widgets.inactive.rounding,
                eframe::egui::Rounding::same(4.0)
            );
        }
    }

    #[test]
    fn button_border_is_distinguishable_from_button_fill() {
        for dark_mode in [true, false] {
            let p = chrome_palette(dark_mode);
            assert_ne!(p.button_bg, p.button_border);
            assert_ne!(p.side_bg, p.panel_bg, "cards must read raised on side_bg");
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
