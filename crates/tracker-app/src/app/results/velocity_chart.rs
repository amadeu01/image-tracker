//! The Results velocity chart (task 13.4, split out of `side_panel.rs` in
//! 20.1): mean concentric velocity by rep, hand-painted per the design
//! notes' egui mapping (no egui_plot dep) — data polyline + loss-colored
//! clickable dots, dashed −10/−20/−30% threshold lines vs rep 1, a dashed
//! least-squares trend line (`tracker_core::linear_trend`), rep-number x
//! labels and unit-aware y ticks. Also holds `headline_card`, the small
//! REPS/SET TIME/VEL. LOSS tile the Results header renders above the chart.

use eframe::egui;

use super::education;
use crate::app::palette::{self, LossSeverity};
use crate::app::state::AppState;

/// Plot-area insets inside the chart's allocated rect, straight from the
/// design mock's SVG geometry (viewBox 420×190, plot x 34..410, y 10..160):
/// 34px left for the y-axis tick labels, 30px bottom for the rep-number
/// labels, 10px top/right breathing room.
const CHART_INSET_LEFT: f32 = 34.0;
const CHART_INSET_BOTTOM: f32 = 30.0;
const CHART_INSET_TOP: f32 = 10.0;
const CHART_INSET_RIGHT: f32 = 10.0;
/// Horizontal padding inside the plot area before the first / after the
/// last dot (the mock's `xOf` `34 + 24 + …` spread).
const CHART_X_PAD: f32 = 24.0;
/// Dot radii: selected rep is drawn bigger, per the mock (`dotR: sel ? 6 : 4.5`).
const CHART_DOT_R: f32 = 4.5;
const CHART_DOT_R_SELECTED: f32 = 6.0;
/// Extra slack around a dot for click/hover hit-testing (design notes:
/// "hit-test by distance < r+2").
const CHART_HIT_SLACK: f32 = 2.0;
/// The mock's aspect ratio (viewBox 420×190), used to scale chart height to
/// however wide the (resizable) panel currently is.
const CHART_ASPECT: f32 = 190.0 / 420.0;

/// x of rep `i`'s dot (of `n` reps) inside `rect`: the plot span minus the
/// 24px end padding, spread evenly — the mock's `xOf`. A single rep sits
/// centered.
fn chart_x(rect: egui::Rect, i: usize, n: usize) -> f32 {
    let x0 = rect.left() + CHART_INSET_LEFT + CHART_X_PAD;
    let x1 = rect.right() - CHART_INSET_RIGHT - CHART_X_PAD;
    if n <= 1 {
        (x0 + x1) / 2.0
    } else {
        x0 + (i as f32 / (n - 1) as f32) * (x1 - x0)
    }
}

/// y of velocity `v` inside `rect` for the value range `vmin..vmax` — the
/// mock's `yOf` (vmin at the plot bottom, vmax at the plot top). A
/// degenerate range centers vertically rather than dividing by zero.
fn chart_y(rect: egui::Rect, v: f64, vmin: f64, vmax: f64) -> f32 {
    let y_bottom = rect.bottom() - CHART_INSET_BOTTOM;
    let y_top = rect.top() + CHART_INSET_TOP;
    if vmax <= vmin {
        return (y_bottom + y_top) / 2.0;
    }
    let t = ((v - vmin) / (vmax - vmin)) as f32;
    y_bottom - t * (y_bottom - y_top)
}

/// Value range for the chart's y-axis: the min/max over the rep means *and*
/// the threshold lines (so a dashed −30% line never falls off the bottom of
/// the plot), padded 5% of the span each side (the mock hardcodes 0.52..0.90
/// around its fixed data; we derive the equivalent). A flat/single-value
/// data set gets an artificial ±5%-of-value (or ±0.5 near zero) span so the
/// line lands mid-plot instead of degenerating.
fn chart_value_range(means: &[f64], thresholds: &[f64]) -> (f64, f64) {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for &v in means.iter().chain(thresholds) {
        if v.is_finite() {
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    if !lo.is_finite() || !hi.is_finite() {
        return (0.0, 1.0);
    }
    let span = hi - lo;
    let pad = if span > 0.0 {
        span * 0.05
    } else {
        (lo.abs() * 0.05).max(0.5)
    };
    (lo - pad, hi + pad)
}

/// Clicking a dot selects that rep via `AppState::select_rep`, the same
/// shared selection the rep table rows and scrub segments use.
pub fn velocity_chart(ui: &mut egui::Ui, state: &mut AppState) {
    let (means, losses, calibrated, unit_label) = {
        let Some(results) = &state.results else {
            return;
        };
        if results.metrics.is_empty() {
            return;
        }
        let calibrated = matches!(
            results.unit,
            Some(tracker_core::VelocityUnit::MetersPerSecond)
        );
        (
            results
                .metrics
                .iter()
                .map(|m| m.mean_concentric_velocity)
                .collect::<Vec<f64>>(),
            results.loss_percent.clone(),
            calibrated,
            if calibrated { "m/s" } else { "px/s" },
        )
    };
    let n = means.len();
    let threshold = state.settings.stop_threshold_pct;
    let selected = state.selected_rep;
    let dark_mode = ui.visuals().dark_mode;
    let chrome = palette::chrome_palette(dark_mode);
    let weak_color = ui.visuals().weak_text_color();
    let text_color = ui.visuals().text_color();
    // Unit-aware value formatting, same rule as the rep table (the mock's
    // `fmtV`): m/s with 2 decimals, px/s as whole numbers.
    let fmt_v = |v: f64| {
        if calibrated {
            format!("{v:.2}")
        } else {
            format!("{}", v.round() as i64)
        }
    };

    // Rep 1's mean anchors the loss-threshold lines; only a finite positive
    // baseline yields honest threshold positions.
    let v1 = means.first().copied().filter(|v| v.is_finite() && *v > 0.0);
    // The three dashed lines are fixed at −10/−20/−30% with fixed
    // green/amber/red severity colors (the mock's #3fbf77/#d9a53f/#e05252),
    // independent of the user's configurable stop threshold.
    let threshold_lines: Vec<(f64, f64, LossSeverity)> = match v1 {
        Some(v1) => [
            (0.10, LossSeverity::Ok),
            (0.20, LossSeverity::Warn),
            (0.30, LossSeverity::Over),
        ]
        .iter()
        .map(|&(loss, sev)| (loss * 100.0, v1 * (1.0 - loss), sev))
        .collect(),
        None => Vec::new(),
    };
    let threshold_values: Vec<f64> = threshold_lines.iter().map(|&(_, v, _)| v).collect();
    let (vmin, vmax) = chart_value_range(&means, &threshold_values);

    let mut clicked_rep: Option<usize> = None;
    egui::Frame::none()
        .fill(chrome.panel_bg)
        .stroke(egui::Stroke::new(1.0f32, chrome.border))
        .rounding(6.0)
        .inner_margin(egui::Margin::same(10.0))
        .show(ui, |ui| {
            // Header row: section label left, threshold hint right.
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("MEAN CONCENTRIC VELOCITY BY REP")
                        .size(10.0)
                        .color(weak_color)
                        .strong(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new("dashed: 10 / 20 / 30% loss")
                            .size(10.0)
                            .color(weak_color),
                    )
                    .on_hover_text(education::EXPLAIN_VELOCITY_LOSS);
                });
            });
            ui.add_space(4.0);

            let width = ui.available_width();
            let height = (width * CHART_ASPECT).clamp(140.0, 200.0);
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
            if !ui.is_rect_visible(rect) {
                return;
            }
            let painter = ui.painter_at(rect);
            let axis_stroke = egui::Stroke::new(1.0f32, chrome.border);
            let plot_left = rect.left() + CHART_INSET_LEFT;
            let plot_right = rect.right() - CHART_INSET_RIGHT;
            let plot_top = rect.top() + CHART_INSET_TOP;
            let plot_bottom = rect.bottom() - CHART_INSET_BOTTOM;

            // Axes.
            painter.line_segment(
                [
                    egui::pos2(plot_left, plot_top),
                    egui::pos2(plot_left, plot_bottom),
                ],
                axis_stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(plot_left, plot_bottom),
                    egui::pos2(plot_right, plot_bottom),
                ],
                axis_stroke,
            );

            // Dashed loss-threshold lines vs rep 1 (only when rep 1's mean
            // is a valid baseline), colored by the severity each represents.
            for &(loss_pct, v, severity) in &threshold_lines {
                let color = palette::loss_severity_color(dark_mode, severity).gamma_multiply(0.65);
                let y = chart_y(rect, v, vmin, vmax);
                painter.add(egui::Shape::dashed_line(
                    &[egui::pos2(plot_left, y), egui::pos2(plot_right, y)],
                    egui::Stroke::new(1.0f32, color),
                    4.0,
                    4.0,
                ));
                painter.text(
                    egui::pos2(plot_right, y - 2.0),
                    egui::Align2::RIGHT_BOTTOM,
                    format!("−{loss_pct:.0}%"),
                    egui::FontId::monospace(9.0),
                    color,
                );
            }

            // Dashed least-squares trend line (tracker-core fit).
            if let Some((slope, intercept)) = tracker_core::linear_trend(&means) {
                let p = |i: usize| {
                    egui::pos2(
                        chart_x(rect, i, n),
                        chart_y(rect, intercept + slope * i as f64, vmin, vmax),
                    )
                };
                painter.add(egui::Shape::dashed_line(
                    &[p(0), p(n - 1)],
                    egui::Stroke::new(1.0f32, chrome.accent.gamma_multiply(0.55)),
                    2.0,
                    3.0,
                ));
            }

            // Data polyline under the dots.
            let dots: Vec<egui::Pos2> = means
                .iter()
                .enumerate()
                .map(|(i, &v)| egui::pos2(chart_x(rect, i, n), chart_y(rect, v, vmin, vmax)))
                .collect();
            for pair in dots.windows(2) {
                painter.line_segment(
                    [pair[0], pair[1]],
                    egui::Stroke::new(1.5f32, text_color.gamma_multiply(0.8)),
                );
            }

            // Dots, loss-colored (rep 1's missing loss counts as 0% →
            // green, same rule as the table's left border), selected bigger.
            for (i, &center) in dots.iter().enumerate() {
                let loss = losses.get(i).copied().flatten();
                let severity = palette::loss_severity(loss.unwrap_or(0.0), threshold);
                let color = palette::loss_severity_color(dark_mode, severity);
                let r = if selected == Some(i) {
                    CHART_DOT_R_SELECTED
                } else {
                    CHART_DOT_R
                };
                painter.circle(center, r, color, egui::Stroke::new(2.0f32, chrome.panel_bg));
                // Rep-number x label (1-based, per the mock).
                painter.text(
                    egui::pos2(center.x, plot_bottom + 14.0),
                    egui::Align2::CENTER_CENTER,
                    (i + 1).to_string(),
                    egui::FontId::monospace(10.0),
                    weak_color,
                );
            }

            // Unit-aware y ticks: 3 evenly spaced values over the range.
            for k in 0..3 {
                let v = vmin + (vmax - vmin) * (k as f64 + 0.5) / 3.0;
                painter.text(
                    egui::pos2(plot_left - 6.0, chart_y(rect, v, vmin, vmax)),
                    egui::Align2::RIGHT_CENTER,
                    fmt_v(v),
                    egui::FontId::monospace(9.0),
                    weak_color,
                );
            }

            // Hit-test hover/click against the dots (distance < r + slack).
            let hit_at = |pos: egui::Pos2| {
                dots.iter().enumerate().find_map(|(i, &c)| {
                    let r = if selected == Some(i) {
                        CHART_DOT_R_SELECTED
                    } else {
                        CHART_DOT_R
                    };
                    (c.distance(pos) < r + CHART_HIT_SLACK).then_some(i)
                })
            };
            if let Some(i) = response.hover_pos().and_then(hit_at) {
                let tip = match losses.get(i).copied().flatten() {
                    Some(loss) => format!(
                        "Rep {} — {} {} (−{:.0}%)",
                        i + 1,
                        fmt_v(means[i]),
                        unit_label,
                        loss.max(0.0)
                    ),
                    None => format!("Rep {} — {} {}", i + 1, fmt_v(means[i]), unit_label),
                };
                response.clone().on_hover_text(tip);
            }
            if response.clicked() {
                clicked_rep = response.interact_pointer_pos().and_then(hit_at);
            }
        });
    if let Some(i) = clicked_rep {
        state.select_rep(i);
    }
}

/// One of the Results header's three headline cards (REPS / SET TIME /
/// VEL. LOSS, task 13.5): an uppercase, letter-spaced label over a large
/// monospace value. `value_color` overrides the value's color (used for
/// VEL. LOSS's loss-severity coloring); `None` uses the default text color.
pub fn headline_card(
    ui: &mut egui::Ui,
    label: &str,
    value: String,
    value_color: Option<egui::Color32>,
    tooltip: &str,
) {
    // Same chrome as `side_panel::section_card` (13.7's harmonisation),
    // tighter margin — these sit nested inside the Results section card.
    let chrome = palette::chrome_palette(ui.visuals().dark_mode);
    egui::Frame::none()
        .fill(chrome.panel_bg)
        .stroke(egui::Stroke::new(1.0f32, chrome.border))
        .rounding(6.0)
        .inner_margin(egui::Margin::symmetric(10.0, 8.0))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new(label)
                        .small()
                        .weak()
                        .text_style(egui::TextStyle::Small),
                );
                let mut text = egui::RichText::new(value)
                    .text_style(egui::TextStyle::Monospace)
                    .strong()
                    .size(18.0);
                if let Some(color) = value_color {
                    text = text.color(color);
                }
                ui.label(text);
            });
        })
        .response
        .on_hover_text(tooltip);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> egui::Rect {
        // The mock's viewBox: 420×190 at the origin.
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(420.0, 190.0))
    }

    #[test]
    fn chart_x_matches_mock_end_padding() {
        // Mock's xOf: first dot at 34+24 = 58, last at 410−24 = 386.
        let r = rect();
        assert!((chart_x(r, 0, 8) - 58.0).abs() < 1e-4);
        assert!((chart_x(r, 7, 8) - 386.0).abs() < 1e-4);
    }

    #[test]
    fn chart_x_spreads_evenly_and_monotonically() {
        let r = rect();
        let xs: Vec<f32> = (0..5).map(|i| chart_x(r, i, 5)).collect();
        let step = xs[1] - xs[0];
        for pair in xs.windows(2) {
            assert!((pair[1] - pair[0] - step).abs() < 1e-4);
            assert!(pair[1] > pair[0]);
        }
    }

    #[test]
    fn chart_x_single_rep_is_centered() {
        let r = rect();
        // Plot x-span after insets+padding: 58..386 → center 222.
        assert!((chart_x(r, 0, 1) - 222.0).abs() < 1e-4);
    }

    #[test]
    fn chart_y_maps_range_endpoints_to_plot_edges() {
        // Mock's yOf: vmin at y=160 (bottom−30), vmax at y=10 (top+10).
        let r = rect();
        assert!((chart_y(r, 0.52, 0.52, 0.90) - 160.0).abs() < 1e-4);
        assert!((chart_y(r, 0.90, 0.52, 0.90) - 10.0).abs() < 1e-4);
        // Higher velocity → smaller y (up the screen).
        assert!(chart_y(r, 0.80, 0.52, 0.90) < chart_y(r, 0.60, 0.52, 0.90));
    }

    #[test]
    fn chart_y_degenerate_range_centers() {
        let r = rect();
        // Plot y-span 10..160 → center 85.
        assert!((chart_y(r, 0.8, 0.8, 0.8) - 85.0).abs() < 1e-4);
    }

    #[test]
    fn chart_value_range_covers_data_and_thresholds_with_padding() {
        let means = [0.82, 0.70];
        let thresholds = [0.738, 0.656, 0.574]; // v1 × 0.9/0.8/0.7
        let (lo, hi) = chart_value_range(&means, &thresholds);
        assert!(lo < 0.574, "lowest threshold line must stay in view");
        assert!(hi > 0.82, "fastest rep must stay in view");
        let span = 0.82 - 0.574;
        assert!((lo - (0.574 - span * 0.05)).abs() < 1e-9);
        assert!((hi - (0.82 + span * 0.05)).abs() < 1e-9);
    }

    #[test]
    fn chart_value_range_flat_data_gets_artificial_span() {
        let (lo, hi) = chart_value_range(&[0.8, 0.8], &[]);
        assert!(lo < 0.8 && hi > 0.8);
        let (lo, hi) = chart_value_range(&[0.0], &[]);
        assert!(lo < 0.0 && hi > 0.0);
    }

    #[test]
    fn chart_value_range_ignores_non_finite_and_survives_empty() {
        let (lo, hi) = chart_value_range(&[f64::NAN, 0.8, 0.6], &[]);
        assert!(lo < 0.6 && hi > 0.6 && hi < 1.0);
        assert_eq!(chart_value_range(&[], &[]), (0.0, 1.0));
    }
}
