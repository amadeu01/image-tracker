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

use super::state::{AppState, EventLevel};

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

pub fn show(ctx: &egui::Context, state: Option<&AppState>) {
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
        egui::Color32::from_rgb(90, 170, 255),
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
    let header_text = if is_current {
        egui::RichText::new(format!("▶ {text}")).color(egui::Color32::from_rgb(90, 170, 255))
    } else if done {
        egui::RichText::new(format!("✓ {text}")).color(egui::Color32::GRAY)
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
    let (state_label, color) = if is_error {
        ("error", egui::Color32::from_rgb(230, 70, 70))
    } else if is_paused {
        (
            "paused — reseed needed",
            egui::Color32::from_rgb(230, 200, 60),
        )
    } else if is_done {
        ("complete", egui::Color32::from_rgb(90, 200, 110))
    } else if is_searching {
        ("object lost — searching…", egui::Color32::GRAY)
    } else if run.running {
        ("tracking", egui::Color32::from_rgb(90, 200, 110))
    } else {
        ("idle", egui::Color32::GRAY)
    };
    kv_row_colored(ui, "state", state_label, color);
    if let Some(e) = &run.error {
        kv_row_colored(ui, "last error", e, egui::Color32::from_rgb(230, 70, 70));
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
}

/// Results section (task 10.3), shown only once a run has finished
/// (`state.results.is_some()`, the Review step). Headline rep count, a
/// per-rep depth/peak/mean table, a quality line (gaps/interpolated%
/// /reseeds), and — when `velocity_series` failed (10.9's GUI seam) — a
/// warning in place of the table rather than a silent empty one.
fn results_section(ui: &mut egui::Ui, state: &AppState) {
    let Some(results) = &state.results else {
        return;
    };
    ui.heading("Results");

    match &results.velocity {
        Err(e) => {
            ui.colored_label(
                egui::Color32::from_rgb(230, 200, 60),
                format!("velocity unavailable: {e}"),
            );
        }
        Ok(_) => {
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
                    .num_columns(4)
                    .striped(true)
                    .show(ui, |ui| {
                        ui.strong("#");
                        ui.strong(format!("depth ({depth_unit})"));
                        ui.strong(format!("peak ({unit})"));
                        ui.strong(format!("mean ({unit})"));
                        ui.end_row();
                        for (i, m) in results.metrics.iter().enumerate() {
                            ui.label(i.to_string());
                            ui.label(format!("{:.2}", m.depth));
                            ui.label(format!("{:.2}", m.peak_concentric_speed));
                            ui.label(format!("{:.2}", m.mean_concentric_velocity));
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
        egui::Color32::from_rgb(230, 200, 60)
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
}

fn events_section(ui: &mut egui::Ui, state: &AppState) {
    ui.heading("Events");
    if state.events.is_empty() {
        ui.weak("(none yet)");
        return;
    }
    for event in state.events.iter().rev() {
        let color = match event.level {
            EventLevel::Error => egui::Color32::from_rgb(230, 70, 70),
            EventLevel::Warn => egui::Color32::from_rgb(230, 200, 60),
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
