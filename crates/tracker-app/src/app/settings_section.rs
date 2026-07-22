//! "Tracking settings" section of the side panel (task 11.3, extracted from
//! `side_panel.rs` in task 14.2 when strategy-education copy pushed that file
//! past its size budget): tracker kind, filter chain, stop-set threshold,
//! advanced tuning knobs, and the "Test strategies" benchmark.
//!
//! Task 14.2 adds strategy education: hover tooltips on every control, a
//! "What do these do?" collapsible with plain-language explanations (ELI5
//! copy adapted from docs/theory.md §7, which has the full deep-dive), and a
//! "Docs" hyperlink to that section on GitHub. All user-facing copy lives in
//! the [`education`] module as named consts so tests can assert coverage.

use eframe::egui;

use super::palette::{self, StatusKind};
use super::state::AppState;
use super::theme;
use crate::tracking::TrackerSelection;

/// User-facing education copy (task 14.2). Tooltips are one or two clauses;
/// the `EXPLAIN_*` paragraphs (1–3 sentences) back the "What do these do?"
/// collapsible and are condensed from docs/theory.md §7's ELI5 sections —
/// the [`DOCS_URL`] link goes to the full deep-dive.
pub mod education {
    /// GitHub deep-dive opened by the "Docs" hyperlink.
    pub const DOCS_URL: &str =
        "https://github.com/amadeu01/image-tracker/blob/main/docs/theory.md#7-strategy-deep-dive";

    // -- Tooltips: tracker choice --------------------------------------
    pub const TIP_TRACKER_COMBO: &str = "Which algorithm follows the seed. Auto picks Color when \
         the seed patch is strongly colored and stands out from its \
         surroundings, and Template otherwise.";
    pub const TIP_TRACKER_AUTO: &str =
        "Decide per seed: the app samples the seed patch and the ring \
         around it — if the patch is saturated and its color is rare in the \
         ring, Color is used; otherwise Template.";
    pub const TIP_TRACKER_TEMPLATE: &str =
        "Match a small photo of what you clicked (ZNCC patch matching). \
         Robust to lighting changes; the safe default.";
    pub const TIP_TRACKER_COLOR: &str = "Learn the seed's color and follow the centroid of nearby \
         same-colored pixels. Best for a distinctly colored marker.";
    pub const TIP_SUGGESTION: &str = "What Auto would use for the current seed, from the color-\
         distinctness check of the seed patch vs. its surroundings.";

    // -- Tooltips: filter chain ----------------------------------------
    pub const TIP_GAUSSIAN: &str = "Slightly blur each frame before matching, averaging away \
         sensor grain. Helps template matching on noisy footage.";
    pub const TIP_GAUSSIAN_SIGMA: &str =
        "Blur strength (standard deviation, px). Larger σ smooths more \
         but also softens the features being matched (0.5–5.0; 1.5 is the \
         benchmarked sweet spot).";
    pub const TIP_MEDIAN: &str = "Replace each pixel with the median of its k×k neighborhood, \
         removing salt-and-pepper speckles without blurring edges.";
    pub const TIP_MEDIAN_K: &str =
        "Neighborhood size: k=3 kills single-pixel speckles; k=5 removes \
         larger blemishes but erodes fine detail.";

    // -- Tooltips: stop-set threshold ----------------------------------
    pub const TIP_STOP_THRESHOLD: &str =
        "Velocity loss (vs rep 1) at which the Results header recommends \
         stopping the set (5–40%, default 20%).";

    // -- Tooltips: benchmark -------------------------------------------
    pub const TIP_TEST_STRATEGIES: &str = "Benchmark all 6 filter × tracker combinations ({none, \
         gaussian:1.5, median:3} × {Template, Color}) over ~200 frames from \
         the current seed, in the background. Reports tracked % (frames the \
         object was found), misses, and jitter (frame-to-frame position \
         noise); the recommended winner is highlighted.";
    pub const TIP_APPLY_WINNER: &str =
        "Copy the winning combination's tracker kind and filter chain \
         into the settings above. The next Track run uses them.";

    // -- Tooltips: advanced knobs (meanings from tracking.rs's
    //    TrackerTuning / session-config docs) --------------------------
    pub const TIP_PATCH_RADIUS: &str = "Half-width of the template patch copied at the seed — the \
         (2r+1)² px 'photo' matched each frame. Bigger is more distinctive \
         but slower and less tolerant of rotation.";
    pub const TIP_SEARCH_RADIUS: &str =
        "How far (px) around the last position each frame is searched. \
         Must exceed the object's fastest per-frame movement; larger is \
         slower and riskier for look-alike locks.";
    pub const TIP_MIN_SCORE: &str = "Minimum match score for a frame to count as Found — below it \
         the frame is a Miss (object lost) and a gap opens.";
    pub const TIP_UPDATE_THRESHOLD: &str =
        "Score above which the adaptive template is refreshed from the \
         current frame, letting the tracker follow gradual appearance \
         change without learning from doubtful matches.";
    pub const TIP_COAST_LIMIT: &str =
        "How many consecutive missed frames to coast through (positions \
         interpolated afterwards) before the run pauses and asks for a \
         reseed.";
    pub const TIP_REACQUIRE_MIN_SCORE: &str =
        "Stricter score a match must clear mid-gap to count as \
         reacquisition — stops the tracker locking onto background clutter \
         (a rack, a mirror) that barely beats min score.";

    // -- "What do these do?" paragraphs (ELI5, from theory.md §7) ------
    pub const EXPLAIN_TEMPLATE: &str =
        "Template (ZNCC): finds the spot that looks most like the sticker \
         you pointed at — every frame it slides a little photo of what you \
         clicked over the nearby area and keeps the best-matching position, \
         even if the lighting got brighter or dimmer. See §7.1 in the docs.";
    pub const EXPLAIN_COLOR: &str =
        "Color: remembers the color of the dot you pointed at, then every \
         frame finds all the nearby specks with that same color and stands \
         in the middle of them. Great for a distinctly colored marker, weak \
         when the background shares the color. See §7.2.";
    pub const EXPLAIN_FILTERS: &str =
        "Filters clean each frame before matching. Gaussian blur gently \
         smudges the picture so tiny random speckles average away — like \
         squinting a little. Median lines up each pixel's neighborhood from \
         darkest to brightest and keeps the middle one, so a single crazy \
         speck gets outvoted. See §7.3–7.4.";
    pub const EXPLAIN_TEST: &str =
        "Test strategies runs every filter × tracker combination over the \
         same ~200 frames from your seed and scores each on how often it \
         kept the object and how steady the path was. Apply winner copies \
         the best combination into these settings. See §7.5.";
}

/// "Tracking settings" (task 11.3): tracker kind, filter chain, and advanced
/// tuning knobs, all read fresh by `AppState::start_tracking` on the next
/// Track/Re-track click. Always visible (before the first Track, and again
/// in Review ahead of Re-track) rather than hidden mid-run, so the user can
/// see what a run *will* use even while one is active — but every widget is
/// wrapped in `add_enabled_ui(!running, ..)` so nothing can be changed while
/// a run is actually in flight (that run already captured its own
/// `TrackerTuning`/chain at spawn time; editing these fields mid-run
/// wouldn't affect it, so disabling avoids the false impression that it
/// would).
pub fn tracking_settings_section(ui: &mut egui::Ui, state: &mut AppState) {
    let running = state.tracking.is_some();
    let header = if running {
        "Tracking settings (locked while running)"
    } else {
        "Tracking settings"
    };
    egui::CollapsingHeader::new(header)
        .id_salt("tracking_settings")
        .default_open(false)
        .show(ui, |ui| {
            ui.add_enabled_ui(!running, |ui| {
                ui.horizontal(|ui| {
                    ui.label("tracker:");
                    let selected_text = match state.settings.tracker_selection {
                        TrackerSelection::Auto => "Auto",
                        TrackerSelection::Template => "Template",
                        TrackerSelection::Color => "Color",
                        TrackerSelection::Circle => "Circle",
                    };
                    egui::ComboBox::from_id_salt("tracker_selection_combo")
                        .selected_text(selected_text)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut state.settings.tracker_selection,
                                TrackerSelection::Auto,
                                "Auto",
                            )
                            .on_hover_text(education::TIP_TRACKER_AUTO);
                            ui.selectable_value(
                                &mut state.settings.tracker_selection,
                                TrackerSelection::Template,
                                "Template",
                            )
                            .on_hover_text(education::TIP_TRACKER_TEMPLATE);
                            ui.selectable_value(
                                &mut state.settings.tracker_selection,
                                TrackerSelection::Color,
                                "Color",
                            )
                            .on_hover_text(education::TIP_TRACKER_COLOR);
                            ui.selectable_value(
                                &mut state.settings.tracker_selection,
                                TrackerSelection::Circle,
                                "Circle",
                            )
                            .on_hover_text(
                                "Fits the plate's rim as a circle (17.5): a geometric fit \
                                 rather than appearance matching, for a smooth/specular plate \
                                 that ZNCC/colour can't discriminate reliably. Not chosen by Auto.",
                            );
                        })
                        .response
                        .on_hover_text(education::TIP_TRACKER_COMBO);
                });
                if state.settings.tracker_selection == TrackerSelection::Auto {
                    let suggestion = match state.suggested_tracker {
                        Some(tracker_core::TrackerKind::Color) => "Color",
                        Some(tracker_core::TrackerKind::Template) => "Template",
                        None => "—",
                    };
                    ui.weak(format!("(current suggestion: {suggestion})"))
                        .on_hover_text(education::TIP_SUGGESTION);
                }

                ui.add_space(6.0);
                ui.label(egui::RichText::new("Filter chain").strong());
                ui.weak("applied gaussian-then-median when both are enabled (v1: fixed order)");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut state.settings.gaussian_enabled, "Gaussian blur")
                        .on_hover_text(education::TIP_GAUSSIAN);
                    ui.add_enabled(
                        state.settings.gaussian_enabled,
                        egui::DragValue::new(&mut state.settings.gaussian_sigma)
                            .speed(0.05)
                            .range(0.5..=5.0)
                            .prefix("σ="),
                    )
                    .on_hover_text(education::TIP_GAUSSIAN_SIGMA);
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut state.settings.median_enabled, "Median filter")
                        .on_hover_text(education::TIP_MEDIAN);
                    ui.add_enabled_ui(state.settings.median_enabled, |ui| {
                        egui::ComboBox::from_id_salt("median_k_combo")
                            .selected_text(format!("k={}", state.settings.median_k))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut state.settings.median_k, 3, "k=3");
                                ui.selectable_value(&mut state.settings.median_k, 5, "k=5");
                            });
                    })
                    .response
                    .on_hover_text(education::TIP_MEDIAN_K);
                });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.weak("stop-set threshold:");
                    let response = ui
                        .add(
                            egui::DragValue::new(&mut state.settings.stop_threshold_pct)
                                .speed(0.5)
                                .range(5.0..=40.0)
                                .suffix("%"),
                        )
                        .on_hover_text(education::TIP_STOP_THRESHOLD);
                    if response.changed() {
                        theme::save_stop_threshold(state.settings.stop_threshold_pct);
                    }
                });

                ui.add_space(6.0);
                egui::CollapsingHeader::new("Advanced")
                    .id_salt("tracking_settings_advanced")
                    .default_open(false)
                    .show(ui, |ui| {
                        advanced_tuning_row(
                            ui,
                            "patch radius (px)",
                            &mut state.settings.patch_radius,
                            1.0,
                            super::state::PATCH_RADIUS_RANGE,
                            education::TIP_PATCH_RADIUS,
                        );
                        advanced_tuning_row(
                            ui,
                            "search radius (px)",
                            &mut state.settings.search_radius,
                            1.0,
                            5..=200,
                            education::TIP_SEARCH_RADIUS,
                        );
                        advanced_tuning_row_f64(
                            ui,
                            "min score",
                            &mut state.settings.min_score,
                            0.01,
                            0.0..=1.0,
                            education::TIP_MIN_SCORE,
                        );
                        advanced_tuning_row_f64(
                            ui,
                            "update threshold",
                            &mut state.settings.update_threshold,
                            0.01,
                            0.0..=1.0,
                            education::TIP_UPDATE_THRESHOLD,
                        );
                        advanced_tuning_row(
                            ui,
                            "coast limit (frames)",
                            &mut state.settings.coast_limit,
                            1.0,
                            0..=60,
                            education::TIP_COAST_LIMIT,
                        );
                        advanced_tuning_row_f64(
                            ui,
                            "reacquire min score",
                            &mut state.settings.reacquire_min_score,
                            0.01,
                            0.0..=1.0,
                            education::TIP_REACQUIRE_MIN_SCORE,
                        );
                    });

                ui.add_space(6.0);
            });
            strategy_benchmark_section(ui, state);
            ui.add_space(6.0);
            explainer_section(ui);
        });
}

/// "What do these do?" (task 14.2): plain-language, 1–3-sentence
/// explanations of each strategy and of the benchmark, condensed from
/// docs/theory.md §7's ELI5 paragraphs, plus a hyperlink to the full
/// deep-dive on GitHub. `ui.hyperlink_to` is used (rather than a manual
/// `xdg-open`/`open` spawn like `open_containing_folder`) because eframe
/// enables egui-winit's `links` feature: egui opens the URL natively via
/// the `webbrowser` crate, non-blocking, with its own failure logging.
fn explainer_section(ui: &mut egui::Ui) {
    egui::CollapsingHeader::new("What do these do?")
        .id_salt("tracking_settings_explainer")
        .default_open(false)
        .show(ui, |ui| {
            ui.weak(education::EXPLAIN_TEMPLATE);
            ui.add_space(4.0);
            ui.weak(education::EXPLAIN_COLOR);
            ui.add_space(4.0);
            ui.weak(education::EXPLAIN_FILTERS);
            ui.add_space(4.0);
            ui.weak(education::EXPLAIN_TEST);
            ui.add_space(4.0);
            ui.hyperlink_to("Docs: strategy deep-dive", education::DOCS_URL)
                .on_hover_text(education::DOCS_URL);
        });
}

/// "Test strategies" button + progress/results (task 11.4): runs the
/// fixed 6-strategy matrix in the background over a ~200-frame segment
/// starting at the current Seed, then shows a compact table (strategy /
/// tracked% / jitter) with the recommended winner highlighted and an
/// "Apply winner" button that copies its filter chain + tracker kind into
/// `state.settings`. Not nested inside the `add_enabled_ui(!running, ..)`
/// block above since its own enabled-ness has an extra condition
/// (`can_test_strategies`, which also checks no benchmark is already
/// running) rather than just "not tracking".
fn strategy_benchmark_section(ui: &mut egui::Ui, state: &mut AppState) {
    ui.horizontal(|ui| {
        let enabled = state.can_test_strategies();
        if ui
            .add_enabled(enabled, egui::Button::new("Test strategies"))
            .on_hover_text(education::TIP_TEST_STRATEGIES)
            .clicked()
        {
            state.start_strategy_benchmark();
        }
        if let Some((done, total)) = state.benchmark_progress {
            ui.weak(format!("running… {done}/{total}"));
        }
    });

    let Some(rows) = state.benchmark_rows.clone() else {
        return;
    };
    let metrics: Vec<crate::compare::StrategyMetrics> = rows.iter().map(|r| r.metrics).collect();
    let winner = crate::compare::recommend(&metrics);

    egui::Grid::new("strategy_benchmark_results")
        .num_columns(3)
        .striped(true)
        .show(ui, |ui| {
            ui.label(egui::RichText::new("strategy").strong());
            ui.label(egui::RichText::new("tracked%").strong());
            ui.label(egui::RichText::new("jitter(px)").strong());
            ui.end_row();
            for (i, row) in rows.iter().enumerate() {
                let is_winner = winner == Some(i);
                let label =
                    if is_winner {
                        egui::RichText::new(row.strategy.label()).strong().color(
                            palette::status_color(ui.visuals().dark_mode, StatusKind::Success),
                        )
                    } else {
                        egui::RichText::new(row.strategy.label())
                    };
                ui.label(label);
                ui.label(format!("{:.1}%", row.metrics.tracked_pct));
                match row.metrics.mean_jitter {
                    Some(j) => ui.label(format!("{j:.2}")),
                    None => ui.label("-"),
                };
                ui.end_row();
            }
        });

    if winner.is_some()
        && ui
            .button("Apply winner")
            .on_hover_text(education::TIP_APPLY_WINNER)
            .clicked()
    {
        state.apply_benchmark_winner();
    }
}

fn advanced_tuning_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut u32,
    speed: f64,
    range: std::ops::RangeInclusive<u32>,
    hover: &str,
) {
    ui.horizontal(|ui| {
        ui.weak(format!("{label}:"));
        ui.add(egui::DragValue::new(value).speed(speed).range(range))
            .on_hover_text(hover);
    });
}

fn advanced_tuning_row_f64(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f64,
    speed: f64,
    range: std::ops::RangeInclusive<f64>,
    hover: &str,
) {
    ui.horizontal(|ui| {
        ui.weak(format!("{label}:"));
        ui.add(egui::DragValue::new(value).speed(speed).range(range))
            .on_hover_text(hover);
    });
}

#[cfg(test)]
mod tests {
    use super::education::*;

    /// Every education const, so coverage checks can't silently rot when
    /// copy is added or renamed.
    const ALL_COPY: &[(&str, &str)] = &[
        ("TIP_TRACKER_COMBO", TIP_TRACKER_COMBO),
        ("TIP_TRACKER_AUTO", TIP_TRACKER_AUTO),
        ("TIP_TRACKER_TEMPLATE", TIP_TRACKER_TEMPLATE),
        ("TIP_TRACKER_COLOR", TIP_TRACKER_COLOR),
        ("TIP_SUGGESTION", TIP_SUGGESTION),
        ("TIP_GAUSSIAN", TIP_GAUSSIAN),
        ("TIP_GAUSSIAN_SIGMA", TIP_GAUSSIAN_SIGMA),
        ("TIP_MEDIAN", TIP_MEDIAN),
        ("TIP_MEDIAN_K", TIP_MEDIAN_K),
        ("TIP_STOP_THRESHOLD", TIP_STOP_THRESHOLD),
        ("TIP_TEST_STRATEGIES", TIP_TEST_STRATEGIES),
        ("TIP_APPLY_WINNER", TIP_APPLY_WINNER),
        ("TIP_PATCH_RADIUS", TIP_PATCH_RADIUS),
        ("TIP_SEARCH_RADIUS", TIP_SEARCH_RADIUS),
        ("TIP_MIN_SCORE", TIP_MIN_SCORE),
        ("TIP_UPDATE_THRESHOLD", TIP_UPDATE_THRESHOLD),
        ("TIP_COAST_LIMIT", TIP_COAST_LIMIT),
        ("TIP_REACQUIRE_MIN_SCORE", TIP_REACQUIRE_MIN_SCORE),
        ("EXPLAIN_TEMPLATE", EXPLAIN_TEMPLATE),
        ("EXPLAIN_COLOR", EXPLAIN_COLOR),
        ("EXPLAIN_FILTERS", EXPLAIN_FILTERS),
        ("EXPLAIN_TEST", EXPLAIN_TEST),
    ];

    #[test]
    fn every_copy_const_is_substantial_prose() {
        for (name, text) in ALL_COPY {
            assert!(
                text.trim().len() >= 40,
                "{name} should be a real explanation, got: {text:?}"
            );
            assert!(
                !text.contains("  "),
                "{name} has a doubled space (string-continuation slip): {text:?}"
            );
        }
    }

    #[test]
    fn docs_url_points_at_the_strategy_deep_dive() {
        assert!(DOCS_URL.starts_with("https://github.com/amadeu01/image-tracker/"));
        assert!(DOCS_URL.ends_with("#7-strategy-deep-dive"));
        assert!(!DOCS_URL.contains(' '));
    }

    /// The explainer paragraphs must reference the theory-doc sections they
    /// were condensed from, so a reader knows where the depth lives.
    #[test]
    fn explainers_cite_their_theory_sections() {
        for (text, section) in [
            (EXPLAIN_TEMPLATE, "§7.1"),
            (EXPLAIN_COLOR, "§7.2"),
            (EXPLAIN_FILTERS, "§7.3"),
            (EXPLAIN_TEST, "§7.5"),
        ] {
            assert!(text.contains(section), "expected {section} in {text:?}");
        }
    }

    /// Advanced-knob tooltips must carry the load-bearing semantics from
    /// tracking.rs's TrackerTuning docs, not vague filler.
    #[test]
    fn advanced_tooltips_state_the_key_semantics() {
        assert!(TIP_MIN_SCORE.contains("Miss") || TIP_MIN_SCORE.contains("lost"));
        assert!(TIP_COAST_LIMIT.contains("reseed"));
        assert!(TIP_COAST_LIMIT.contains("interpolated"));
        assert!(TIP_REACQUIRE_MIN_SCORE.contains("mid-gap"));
        assert!(TIP_UPDATE_THRESHOLD.contains("refresh"));
        assert!(TIP_TEST_STRATEGIES.contains("200 frames"));
        assert!(TIP_TEST_STRATEGIES.contains('6'));
        assert!(TIP_APPLY_WINNER.contains("filter chain"));
    }
}
