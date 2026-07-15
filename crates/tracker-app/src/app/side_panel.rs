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

pub fn show(ctx: &egui::Context, state: &AppState) {
    egui::SidePanel::right("side_panel")
        .default_width(PANEL_WIDTH)
        .resizable(true)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                guide_section(ui, state);
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);
                status_section(ui, state);
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);
                events_section(ui, state);
            });
        });
}

fn guide_section(ui: &mut egui::Ui, state: &AppState) {
    ui.heading("Guide");
    let current = state.current_step().ordinal();
    for (id, label) in STEPS {
        let done = id < current;
        let is_current = id == current;
        let text = format!("{id}. {label}");
        if is_current {
            ui.colored_label(egui::Color32::from_rgb(90, 170, 255), format!("▶ {text}"));
        } else if done {
            ui.colored_label(egui::Color32::GRAY, format!("✓ {text}"));
        } else {
            ui.label(format!("   {text}"));
        }
    }
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
