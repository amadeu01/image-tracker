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
use super::results::{self, headline_card, rep_table, velocity_chart};
use super::state::{AppState, EventLevel};

/// Widened 320 → 460 in task 13.7 to match the design mock's fixed 460px
/// right column (13.3 had already widened 260 → 320 for the rep table);
/// the panel stays user-resizable.
const PANEL_WIDTH: f32 = 460.0;

/// Metrics education copy, split out to `app/results/education.rs` in task
/// 20.1 alongside the rep table/velocity chart it annotates. Re-exported
/// under this name so `side_panel::education` keeps working as a path (the
/// module's own tests, and anything future that reaches for it here, don't
/// need to know it physically lives in `results::education` now).
pub use results::education;

/// Card chrome for one side-panel section (task 13.7, the design's
/// `#1f1f24` / `#2c2c31` / radius-6 / 14px-padding cards): every section
/// (guide, status, settings, results, events) renders inside one of these
/// on the darker `side_bg` panel, instead of 12.x's bare
/// separator-delimited stack.
fn section_card<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let chrome = palette::chrome_palette(ui.visuals().dark_mode);
    egui::Frame::none()
        .fill(chrome.panel_bg)
        .stroke(egui::Stroke::new(1.0f32, chrome.border))
        .rounding(6.0)
        .inner_margin(egui::Margin::same(14.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
        .inner
}

/// Section-header typography (task 13.1): the design specifies uppercase,
/// small (~11px), letter-spaced labels rather than egui's default
/// `heading()` (large, proportionally-weighted). egui has no letter-spacing
/// primitive, so the ~11px tracking is approximated by joining the
/// uppercased characters with a thin space (U+2009) — close enough to the
/// design's "spaced out capitals" look without needing custom font shaping
/// (the design notes explicitly say default fonts are fine for now). Color
/// is the existing muted `StatusKind::Neutral` (not the new chrome accent)
/// so labels read as quiet section dividers, not clickable/emphasized text.
fn section_label(ui: &mut egui::Ui, text: &str) {
    let spaced = text
        .to_uppercase()
        .chars()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join("\u{2009}");
    let color = palette::status_color(ui.visuals().dark_mode, StatusKind::Neutral);
    ui.label(egui::RichText::new(spaced).size(11.0).color(color).strong());
    ui.add_space(2.0);
}

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
    let chrome = palette::chrome_palette(ctx.style().visuals.dark_mode);
    egui::SidePanel::right("side_panel")
        .default_width(PANEL_WIDTH)
        .resizable(true)
        .frame(
            egui::Frame::default()
                .fill(chrome.side_bg)
                .inner_margin(egui::Margin::same(14.0)),
        )
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let Some(state) = state else {
                    section_card(ui, empty_guide_section);
                    return;
                };
                section_card(ui, |ui| guide_section(ui, state));
                ui.add_space(12.0);
                section_card(ui, |ui| status_section(ui, state));
                ui.add_space(12.0);
                section_card(ui, |ui| {
                    super::settings_section::tracking_settings_section(ui, state)
                });
                if state.results.is_some() {
                    ui.add_space(12.0);
                    section_card(ui, |ui| results_section(ui, state));
                }
                ui.add_space(12.0);
                section_card(ui, |ui| events_section(ui, state));
            });
        });
}

/// Guide shown before any video is loaded (10.5): step 0, distinct from the
/// numbered `STEPS` (which all assume a video is already open).
fn empty_guide_section(ui: &mut egui::Ui) {
    section_label(ui, "Guide");
    ui.colored_label(
        palette::status_color(ui.visuals().dark_mode, StatusKind::Info),
        "▶ 0. Open a video [Ctrl+O]",
    );
    for (id, label) in STEPS {
        guide_step_row(ui, id, label, false, false);
    }
}

fn guide_section(ui: &mut egui::Ui, state: &AppState) {
    section_label(ui, "Guide");
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
    section_label(ui, "Status");

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

/// Results section (task 10.3, headline cards + stop-set banner + loss
/// column added 13.5), shown only once a run has finished
/// (`state.results.is_some()`, the Review step). Three headline cards
/// (REPS / SET TIME / VEL. LOSS, per the design), a "Stop set recommended"
/// banner once any rep's loss crosses `state.settings.stop_threshold_pct`,
/// an uncalibrated-units chip, a per-rep depth/peak/mean/loss table, a
/// quality line (gaps/interpolated%/reseeds), and — when `velocity_series`
/// failed (10.9's GUI seam) — a warning in place of the table rather than a
/// silent empty one.
fn results_section(ui: &mut egui::Ui, state: &mut AppState) {
    let Some(results) = &state.results else {
        return;
    };
    let velocity_ok = results.velocity.is_ok();
    section_label(ui, "Results");

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
                headline_card(
                    ui,
                    "REPS",
                    results.reps.len().to_string(),
                    None,
                    education::TIP_REPS,
                );
                let set_time = results
                    .set_duration_seconds()
                    .map(|s| format!("{s:.1}s"))
                    .unwrap_or_else(|| "—".to_string());
                headline_card(ui, "SET TIME", set_time, None, education::TIP_SET_TIME);
                let (loss_text, loss_color) = if max_loss.is_finite() {
                    let severity = palette::loss_severity(max_loss, threshold);
                    (
                        format!("-{max_loss:.1}%"),
                        Some(palette::loss_severity_color(dark_mode, severity)),
                    )
                } else {
                    ("—".to_string(), None)
                };
                headline_card(
                    ui,
                    "VEL. LOSS",
                    loss_text,
                    loss_color,
                    education::TIP_VEL_LOSS,
                );
            });

            // -- "Stop set recommended" banner ---------------------------
            if let Some(stop) = stop {
                ui.add_space(6.0);
                let over_color = palette::loss_severity_color(dark_mode, LossSeverity::Over);
                egui::Frame::none()
                    .fill(over_color.linear_multiply(0.15))
                    .stroke(egui::Stroke::new(1.0f32, over_color))
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
                    .stroke(egui::Stroke::new(1.0f32, warn_color))
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
        }
    }

    if velocity_ok {
        ui.add_space(6.0);
        rep_table(ui, state);
        ui.add_space(6.0);
        velocity_chart(ui, state);
    }

    ui.add_space(6.0);
    // Re-borrowed (`ResultsQuality` is `Copy`) rather than reusing the
    // `results` binding from above: `rep_table` needed `&mut state`.
    let q = state
        .results
        .as_ref()
        .map(|r| r.quality)
        .unwrap_or_default();
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
    section_label(ui, "Events");
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
