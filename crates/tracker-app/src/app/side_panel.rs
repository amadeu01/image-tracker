//! Side panel (task 7.2): the right-hand space next to the video, previously
//! empty. Three sections:
//!
//! - **Guide**: the numbered workflow, current step highlighted.
//! - **Status**: grouped video/seed/calibration/tracking key-value rows —
//!   the detail that used to be crammed into the single bottom status line.
//! - **Events**: the last few `AppEvent`s from `AppState`'s ring buffer,
//!   mirroring the `tracing` breadcrumbs already written to the log file.
//!
//! Rendering is thin by design: every value shown here is read straight off
//! `AppState`/`TrackingRunState`; the interesting logic (`current_step`,
//! the event ring buffer) lives in `state.rs` and is unit-tested there.

use eframe::egui;

use super::palette::{self, LossSeverity, StatusKind};
use super::state::{AppState, EventLevel};
use super::theme;
use crate::tracking::TrackerSelection;

const PANEL_WIDTH: f32 = 260.0;

const STEPS: [(u8, &str); 5] = [
    (1, "Scrub to bar visible"),
    (2, "Place seed [S]"),
    (3, "Calibrate [C] (optional, needed for m/s)"),
    (4, "Track"),
    (5, "Review / Export"),
];

/// 2-line how-to for each guide step (task 10.7's "expandable guide"),
/// shown inside a `CollapsingHeader` under each numbered step. Written to
/// answer the specific questions the 2026-07-15 user session raised (see
/// PLAN.md 10.7's row): why calibrate at all, what pausing/stopping does,
/// where exported files land.
const STEP_HOWTO: [(u8, &str); 5] = [
    (
        1,
        "Drag the scrub bar (or use ←/→, Shift+←/→ for ±10 frames) until \
         the barbell is clearly visible in the frame.",
    ),
    (
        2,
        "Click \"Place Seed\" [S], then click the barbell in the video — \
         ideally the plate hub/marker. The tracker follows from there.",
    ),
    (
        3,
        "Click \"Calibrate\" [C], then click one edge of a plate and the \
         opposite edge; without this, results are in pixels/s, not m/s.",
    ),
    (
        4,
        "Click \"Track\" to run to the end of the video. Pause/Resume/Stop \
         are available mid-run; Stop keeps whatever was tracked so far.",
    ),
    (
        5,
        "Reps, depth, and velocity appear below once tracking finishes. \
         Overlay video + CSV/JSON exports are written next to your video.",
    ),
];

pub fn show(ctx: &egui::Context, state: Option<&mut AppState>) {
    egui::SidePanel::right("side_panel")
        .default_width(PANEL_WIDTH)
        .resizable(true)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let Some(state) = state else {
                    empty_guide_section(ui);
                    return;
                };
                guide_section(ui, state);
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);
                status_section(ui, state);
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);
                tracking_settings_section(ui, state);
                if state.results.is_some() {
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(8.0);
                    results_section(ui, state);
                }
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);
                events_section(ui, state);
            });
        });
}

/// Guide shown before any video is loaded (10.5): step 0, distinct from the
/// numbered `STEPS` (which all assume a video is already open).
fn empty_guide_section(ui: &mut egui::Ui) {
    ui.heading("Guide");
    ui.colored_label(
        palette::status_color(ui.visuals().dark_mode, StatusKind::Info),
        "▶ 0. Open a video [Ctrl+O]",
    );
    for (id, label) in STEPS {
        guide_step_row(ui, id, label, false, false);
    }
}

fn guide_section(ui: &mut egui::Ui, state: &AppState) {
    ui.heading("Guide");
    let current = state.current_step().ordinal();
    for (id, label) in STEPS {
        let done = id < current;
        let is_current = id == current;
        guide_step_row(ui, id, label, done, is_current);
    }
}

/// One expandable guide step (task 10.7): the numbered/colored summary line
/// (unchanged from before 10.7) as a `CollapsingHeader`, with the
/// corresponding `STEP_HOWTO` entry shown when expanded. Collapsed by
/// default — the guide stays scannable at a glance; the how-to is there for
/// whoever wants it, not forced on everyone.
fn guide_step_row(ui: &mut egui::Ui, id: u8, label: &str, done: bool, is_current: bool) {
    let text = format!("{id}. {label}");
    let dark_mode = ui.visuals().dark_mode;
    let header_text = if is_current {
        egui::RichText::new(format!("▶ {text}"))
            .color(palette::status_color(dark_mode, StatusKind::Info))
    } else if done {
        egui::RichText::new(format!("✓ {text}"))
            .color(palette::status_color(dark_mode, StatusKind::Neutral))
    } else {
        egui::RichText::new(format!("   {text}"))
    };
    egui::CollapsingHeader::new(header_text)
        .id_salt(("guide_step", id))
        .default_open(false)
        .show(ui, |ui| {
            let howto = STEP_HOWTO
                .iter()
                .find(|(step_id, _)| *step_id == id)
                .map(|(_, text)| *text)
                .unwrap_or("");
            ui.weak(howto);
        });
}

fn status_section(ui: &mut egui::Ui, state: &AppState) {
    ui.heading("Status");

    ui.label(egui::RichText::new("Video").strong());
    let name = state
        .video_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| state.video_path.display().to_string());
    kv_row(ui, "file", &name);
    kv_row(
        ui,
        "frame",
        &format!(
            "{} / {}",
            state.current_frame,
            state.metadata.frame_count.unwrap_or(0).saturating_sub(1)
        ),
    );
    kv_row(
        ui,
        "fps",
        &format!("{}/{}", state.metadata.fps_num, state.metadata.fps_den),
    );

    ui.add_space(6.0);
    ui.label(egui::RichText::new("Seed").strong());
    match &state.seed {
        Some(seed) => {
            kv_row(
                ui,
                "position",
                &format!(
                    "({:.1}, {:.1}) @ frame {}",
                    seed.position.x, seed.position.y, seed.frame_index
                ),
            );
            let suggestion = match state.suggested_tracker {
                Some(tracker_core::TrackerKind::Color) => "Color",
                Some(tracker_core::TrackerKind::Template) => "Template",
                None => "—",
            };
            kv_row(ui, "suggested tracker", suggestion);
        }
        None => kv_row(ui, "position", "not placed"),
    }

    ui.add_space(6.0);
    ui.label(egui::RichText::new("Calibration").strong());
    match &state.calibration {
        Some(cal) => kv_row(ui, "scale", &format!("{:.1} px/m", cal.px_per_meter())),
        None => kv_row(ui, "scale", "not set"),
    }

    ui.add_space(6.0);
    ui.label(egui::RichText::new("Tracking").strong());
    let run = &state.tracking_run;
    let is_error = run.error.is_some();
    let is_paused = run.session_state == Some(tracker_core::SessionState::NeedsReseed);
    let is_searching = run.is_searching();
    let is_done = !run.running && run.bar_path.is_some();
    let dark_mode = ui.visuals().dark_mode;
    let (state_label, color) = if is_error {
        ("error", palette::status_color(dark_mode, StatusKind::Error))
    } else if is_paused {
        (
            "paused — reseed needed",
            palette::status_color(dark_mode, StatusKind::Warn),
        )
    } else if is_done {
        (
            "complete",
            palette::status_color(dark_mode, StatusKind::Success),
        )
    } else if is_searching {
        (
            "object lost — searching…",
            palette::status_color(dark_mode, StatusKind::Neutral),
        )
    } else if run.running {
        (
            "tracking",
            palette::status_color(dark_mode, StatusKind::Success),
        )
    } else {
        (
            "idle",
            palette::status_color(dark_mode, StatusKind::Neutral),
        )
    };
    kv_row_colored(ui, "state", state_label, color);
    if let Some(e) = &run.error {
        kv_row_colored(
            ui,
            "last error",
            e,
            palette::status_color(dark_mode, StatusKind::Error),
        );
    }
    kv_row(ui, "frames processed", &run.frames_processed.to_string());
    kv_row(ui, "gaps", &run.gap_count.to_string());
    if let Some(pos) = run.last_position {
        kv_row(
            ui,
            "last position",
            &format!("({:.1}, {:.1})", pos.x, pos.y),
        );
    }
    // Task 10.8: live rep counter, recomputed every ~30 processed frames
    // from the partial path (`AppState::poll_tracking`). Shown only while
    // a run is active and at least one recompute has succeeded — before
    // that there's nothing honest to say yet, so the row is omitted rather
    // than showing a misleading "0".
    if run.running {
        if let Some(count) = state.live_reps {
            kv_row(ui, "reps so far", &count.to_string());
        }
    }
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
fn tracking_settings_section(ui: &mut egui::Ui, state: &mut AppState) {
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
                    };
                    egui::ComboBox::from_id_salt("tracker_selection_combo")
                        .selected_text(selected_text)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut state.settings.tracker_selection,
                                TrackerSelection::Auto,
                                "Auto",
                            );
                            ui.selectable_value(
                                &mut state.settings.tracker_selection,
                                TrackerSelection::Template,
                                "Template",
                            );
                            ui.selectable_value(
                                &mut state.settings.tracker_selection,
                                TrackerSelection::Color,
                                "Color",
                            );
                        });
                });
                if state.settings.tracker_selection == TrackerSelection::Auto {
                    let suggestion = match state.suggested_tracker {
                        Some(tracker_core::TrackerKind::Color) => "Color",
                        Some(tracker_core::TrackerKind::Template) => "Template",
                        None => "—",
                    };
                    ui.weak(format!("(current suggestion: {suggestion})"));
                }

                ui.add_space(6.0);
                ui.label(egui::RichText::new("Filter chain").strong());
                ui.weak("applied gaussian-then-median when both are enabled (v1: fixed order)");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut state.settings.gaussian_enabled, "Gaussian blur");
                    ui.add_enabled(
                        state.settings.gaussian_enabled,
                        egui::DragValue::new(&mut state.settings.gaussian_sigma)
                            .speed(0.05)
                            .range(0.5..=5.0)
                            .prefix("σ="),
                    )
                    .on_hover_text("Gaussian blur standard deviation, in pixels (0.5-5.0)");
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut state.settings.median_enabled, "Median filter");
                    ui.add_enabled_ui(state.settings.median_enabled, |ui| {
                        egui::ComboBox::from_id_salt("median_k_combo")
                            .selected_text(format!("k={}", state.settings.median_k))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut state.settings.median_k, 3, "k=3");
                                ui.selectable_value(&mut state.settings.median_k, 5, "k=5");
                            });
                    })
                    .response
                    .on_hover_text("median filter neighborhood size (removes salt-and-pepper noise)");
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
                        .on_hover_text(
                            "velocity loss (vs rep 1) at which the Results header recommends \
                             stopping the set (5-40%, default 20%)",
                        );
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
                            4..=64,
                            "half-width of the template patch matched around the seed",
                        );
                        advanced_tuning_row(
                            ui,
                            "search radius (px)",
                            &mut state.settings.search_radius,
                            1.0,
                            5..=200,
                            "how far around the last position each frame searches for a match",
                        );
                        advanced_tuning_row_f64(
                            ui,
                            "min score",
                            &mut state.settings.min_score,
                            0.01,
                            0.0..=1.0,
                            "minimum correlation score counted as a match (Found vs Miss)",
                        );
                        advanced_tuning_row_f64(
                            ui,
                            "update threshold",
                            &mut state.settings.update_threshold,
                            0.01,
                            0.0..=1.0,
                            "score above which the template reference is refreshed each step",
                        );
                        advanced_tuning_row(
                            ui,
                            "coast limit (frames)",
                            &mut state.settings.coast_limit,
                            1.0,
                            0..=60,
                            "how many consecutive misses to coast through before pausing for a reseed",
                        );
                        advanced_tuning_row_f64(
                            ui,
                            "reacquire min score",
                            &mut state.settings.reacquire_min_score,
                            0.01,
                            0.0..=1.0,
                            "minimum score a mid-gap Found must clear to count as reacquisition",
                        );
                    });

                ui.add_space(6.0);
            });
            strategy_benchmark_section(ui, state);
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
            .on_hover_text(
                "runs a ~200-frame benchmark of every filter x tracker combination \
                 from the current seed, in the background",
            )
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

    if winner.is_some() && ui.button("Apply winner").clicked() {
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

/// Results section (task 10.3, headline cards + stop-set banner + loss
/// column added 13.5), shown only once a run has finished
/// (`state.results.is_some()`, the Review step). Three headline cards
/// (REPS / SET TIME / VEL. LOSS, per the design), a "Stop set recommended"
/// banner once any rep's loss crosses `state.settings.stop_threshold_pct`,
/// an uncalibrated-units chip, a per-rep depth/peak/mean/loss table, a
/// quality line (gaps/interpolated%/reseeds), and — when `velocity_series`
/// failed (10.9's GUI seam) — a warning in place of the table rather than a
/// silent empty one.
fn results_section(ui: &mut egui::Ui, state: &AppState) {
    let Some(results) = &state.results else {
        return;
    };
    ui.heading("Results");

    match &results.velocity {
        Err(e) => {
            ui.colored_label(
                palette::status_color(ui.visuals().dark_mode, StatusKind::Warn),
                format!("velocity unavailable: {e}"),
            );
        }
        Ok(_) => {
            let dark_mode = ui.visuals().dark_mode;
            let threshold = state.settings.stop_threshold_pct;
            let uncalibrated = !matches!(
                results.unit,
                Some(tracker_core::VelocityUnit::MetersPerSecond)
            );
            let stop = results.stop_set_evaluation(threshold);
            let max_loss = results
                .loss_percent
                .iter()
                .flatten()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max);

            // -- Headline cards: REPS / SET TIME / VEL. LOSS -------------
            ui.horizontal(|ui| {
                headline_card(ui, "REPS", results.reps.len().to_string(), None);
                let set_time = results
                    .set_duration_seconds()
                    .map(|s| format!("{s:.1}s"))
                    .unwrap_or_else(|| "—".to_string());
                headline_card(ui, "SET TIME", set_time, None);
                let (loss_text, loss_color) = if max_loss.is_finite() {
                    let severity = palette::loss_severity(max_loss, threshold);
                    (
                        format!("-{max_loss:.1}%"),
                        Some(palette::loss_severity_color(dark_mode, severity)),
                    )
                } else {
                    ("—".to_string(), None)
                };
                headline_card(ui, "VEL. LOSS", loss_text, loss_color);
            });

            // -- "Stop set recommended" banner ---------------------------
            if let Some(stop) = stop {
                ui.add_space(6.0);
                let over_color = palette::loss_severity_color(dark_mode, LossSeverity::Over);
                egui::Frame::none()
                    .fill(over_color.linear_multiply(0.15))
                    .stroke(egui::Stroke::new(1.0, over_color))
                    .rounding(6.0)
                    .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new("Stop set recommended")
                                    .strong()
                                    .color(over_color),
                            );
                            ui.label(
                                egui::RichText::new(format!(
                                    "Velocity loss reached {:.1}% — over your {:.0}% \
                                     threshold at rep {}.",
                                    stop.loss,
                                    threshold,
                                    stop.rep_index + 1
                                ))
                                .small(),
                            );
                        });
                    });
            }

            // -- Uncalibrated chip ----------------------------------------
            if uncalibrated {
                ui.add_space(6.0);
                let warn_color = palette::status_color(dark_mode, StatusKind::Warn);
                egui::Frame::none()
                    .fill(warn_color.linear_multiply(0.12))
                    .stroke(egui::Stroke::new(1.0, warn_color))
                    .rounding(6.0)
                    .inner_margin(egui::Margin::symmetric(10.0, 6.0))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(
                                "Calibration not set — values shown in px/s. \
                                 Calibrate for m/s.",
                            )
                            .small(),
                        );
                    });
            }

            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!("Reps: {}", results.reps.len()))
                    .heading()
                    .strong(),
            );

            let unit = match results.unit {
                Some(tracker_core::VelocityUnit::MetersPerSecond) => "m/s",
                Some(tracker_core::VelocityUnit::PixelsPerSecond) | None => "px/s",
            };
            let depth_unit = match results.unit {
                Some(tracker_core::VelocityUnit::MetersPerSecond) => "m",
                _ => "px",
            };

            if !results.metrics.is_empty() {
                egui::Grid::new("results_reps_grid")
                    .num_columns(5)
                    .striped(true)
                    .show(ui, |ui| {
                        ui.strong("#");
                        ui.strong(format!("depth ({depth_unit})"));
                        ui.strong(format!("peak ({unit})"));
                        ui.strong(format!("mean ({unit})"));
                        ui.strong("loss");
                        ui.end_row();
                        for (i, m) in results.metrics.iter().enumerate() {
                            ui.label(i.to_string());
                            ui.label(format!("{:.2}", m.depth));
                            ui.label(format!("{:.2}", m.peak_concentric_speed));
                            ui.label(format!("{:.2}", m.mean_concentric_velocity));
                            match results.loss_percent.get(i).copied().flatten() {
                                Some(loss) => {
                                    let severity = palette::loss_severity(loss, threshold);
                                    ui.colored_label(
                                        palette::loss_severity_color(dark_mode, severity),
                                        format!("-{loss:.1}%"),
                                    );
                                }
                                None => {
                                    ui.weak("—");
                                }
                            }
                            ui.end_row();
                        }
                    });
            } else {
                ui.weak("(no reps detected)");
            }
        }
    }

    ui.add_space(6.0);
    let q = &results.quality;
    ui.label(egui::RichText::new("Quality").strong());
    kv_row(ui, "gaps", &q.gap_count.to_string());
    kv_row(ui, "reseeds", &q.reseed_count.to_string());
    let interp_color = if q.interpolated_percent() > 20.0 {
        palette::status_color(ui.visuals().dark_mode, StatusKind::Warn)
    } else {
        ui.visuals().text_color()
    };
    ui.horizontal(|ui| {
        ui.weak("interpolated:");
        ui.colored_label(
            interp_color,
            format!(
                "{}/{} ({:.1}%)",
                q.interpolated_points,
                q.total_points,
                q.interpolated_percent()
            ),
        );
    });

    files_section(ui, state);
}

/// One of the Results header's three headline cards (REPS / SET TIME /
/// VEL. LOSS, task 13.5): an uppercase, letter-spaced label over a large
/// monospace value. `value_color` overrides the value's color (used for
/// VEL. LOSS's loss-severity coloring); `None` uses the default text color.
fn headline_card(
    ui: &mut egui::Ui,
    label: &str,
    value: String,
    value_color: Option<egui::Color32>,
) {
    egui::Frame::group(ui.style())
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
        });
}

/// "Files" list (task 12.6): every export written this session, kept
/// visible in the Results section rather than only flashing through the
/// events feed once — the user question this answers is literally "how do
/// I know from the UI the JSON/CSV was generated?" (PLAN.md 12.6). Each row
/// shows the filename (full path on hover), a copy-path button, and an
/// "open folder" button; a folder-open failure surfaces as an event rather
/// than panicking (`open_containing_folder`'s doc comment).
fn files_section(ui: &mut egui::Ui, state: &AppState) {
    if state.exported_files.is_empty() {
        return;
    }
    ui.add_space(6.0);
    ui.label(egui::RichText::new("Files").strong());
    for path in &state.exported_files {
        ui.horizontal(|ui| {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string());
            ui.label(&name).on_hover_text(path.display().to_string());
            if ui
                .small_button("Copy path")
                .on_hover_text("copy the full path to the clipboard")
                .clicked()
            {
                ui.output_mut(|o| o.copied_text = path.display().to_string());
            }
            if ui
                .small_button("Open folder")
                .on_hover_text("open the containing folder in the system file manager")
                .clicked()
            {
                open_containing_folder(path);
            }
        });
    }
}

/// Opens the OS file manager on `path`'s parent directory, best-effort:
/// spawn failures (no file manager registered, sandboxed/headless
/// environment, etc.) are logged rather than propagated — a discoverability
/// nicety must never be able to crash the app or block the UI thread (hence
/// `spawn`, not `status`/`output`, so this never waits on the child).
fn open_containing_folder(path: &std::path::Path) {
    let dir = path.parent().unwrap_or(path);

    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(dir).spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("explorer").arg(dir).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let result = std::process::Command::new("xdg-open").arg(dir).spawn();

    if let Err(e) = result {
        tracing::warn!(dir = %dir.display(), error = %e, "failed to open containing folder");
    }
}

fn events_section(ui: &mut egui::Ui, state: &AppState) {
    ui.heading("Events");
    if state.events.is_empty() {
        ui.weak("(none yet)");
        return;
    }
    let dark_mode = ui.visuals().dark_mode;
    for event in state.events.iter().rev() {
        let color = match event.level {
            EventLevel::Error => palette::status_color(dark_mode, StatusKind::Error),
            EventLevel::Warn => palette::status_color(dark_mode, StatusKind::Warn),
            EventLevel::Info => ui.visuals().text_color(),
        };
        ui.horizontal(|ui| {
            ui.weak(format!("+{:>6.1}s", event.elapsed_secs));
            ui.colored_label(color, &event.message);
        });
    }
}

fn kv_row(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.weak(format!("{key}:"));
        ui.label(value);
    });
}

fn kv_row_colored(ui: &mut egui::Ui, key: &str, value: &str, color: egui::Color32) {
    ui.horizontal(|ui| {
        ui.weak(format!("{key}:"));
        ui.colored_label(color, value);
    });
}
