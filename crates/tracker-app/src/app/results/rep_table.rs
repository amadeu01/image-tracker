//! The Results rep table (task 13.3, split out of `side_panel.rs` in 20.1):
//! one clickable row per rep with #(1-based)/DEPTH/PEAK V/MEAN V/LOSS/TIME/▶
//! columns per the design mock. egui's `Grid` can't do row backgrounds or
//! clicks, so each row is an allocated `Sense::click` rect with a painted
//! 3px loss-colored left border and monospace text at fixed x-offsets (the
//! design notes' egui mapping). Ends with the "Export all rep clips" button.

use eframe::egui;

use super::education;
use crate::app::palette;
use crate::app::state::{format_clip_time, AppState};

/// Rep-table row height (design mock: ~28px rows; slightly tighter here).
const REP_ROW_HEIGHT: f32 = 22.0;
/// Left x-offset of each text column (#, DEPTH, PEAK V, MEAN V, LOSS, TIME)
/// inside a row, in px — the design mock's fixed grid columns, compressed to
/// fit the panel. The ▶ button is right-aligned separately.
const REP_COL_X: [f32; 6] = [8.0, 26.0, 64.0, 102.0, 140.0, 178.0];
/// ▶ button width, right-aligned in each row.
const REP_PLAY_WIDTH: f32 = 24.0;

/// What a click inside the rep table asked for, resolved after the render
/// loop (the rows borrow `state.results` immutably while drawing).
enum RepTableAction {
    /// Row click: select + jump (the mock's `onSelect`).
    Select(usize),
    /// ▶ click: toggle the rep's clip loop (the mock's `onPlay`, which
    /// `stopPropagation`s — here the button response is checked *before*
    /// the row response so ▶ never also triggers row-select-and-clear-clip).
    Play(usize),
}

pub fn rep_table(ui: &mut egui::Ui, state: &mut AppState) {
    struct Row {
        depth: String,
        peak: String,
        mean: String,
        loss: Option<f64>,
        range: String,
        clip_armed: bool,
        selected: bool,
    }

    let rows: Vec<Row> = {
        let Some(results) = &state.results else {
            return;
        };
        if results.metrics.is_empty() {
            ui.weak("(no reps detected)");
            return;
        }
        // Uncalibrated (px) values render as whole pixels, calibrated
        // (m, m/s) with 2 decimals — the mock's `fmtV`/`fmtD`.
        let calibrated = matches!(
            results.unit,
            Some(tracker_core::VelocityUnit::MetersPerSecond)
        );
        let fmt = |v: f64| {
            if calibrated {
                format!("{v:.2}")
            } else {
                format!("{}", v.round() as i64)
            }
        };
        let (num, den) = (state.metadata.fps_num, state.metadata.fps_den);
        results
            .metrics
            .iter()
            .enumerate()
            .map(|(i, m)| Row {
                depth: fmt(m.depth),
                peak: fmt(m.peak_concentric_speed),
                mean: fmt(m.mean_concentric_velocity),
                loss: results.loss_percent.get(i).copied().flatten(),
                range: results
                    .rep_frame_bounds(i)
                    .map(|(s, e)| {
                        format!(
                            "{}–{}",
                            format_clip_time(s, num, den),
                            format_clip_time(e, num, den)
                        )
                    })
                    .unwrap_or_else(|| "—".to_string()),
                clip_armed: state.rep_clip == Some(i),
                selected: state.selected_rep == Some(i),
            })
            .collect()
    };

    let dark_mode = ui.visuals().dark_mode;
    let chrome = palette::chrome_palette(dark_mode);
    let threshold = state.settings.stop_threshold_pct;
    let font = egui::FontId::monospace(10.0);
    let weak_color = ui.visuals().weak_text_color();
    let text_color = ui.visuals().text_color();

    // Header row: uppercase weak labels at the same fixed offsets, each
    // hoverable for its 19.4 education tooltip. Column width for hit-testing
    // is "to the next column's x" (last column runs to the row's right
    // edge) — the header has no separate widget per cell, so `ui.interact`
    // is used directly on a rect carved out of the header row.
    let headers: [(&str, &str); 6] = [
        ("#", "Rep number (1-based)."),
        ("DEPTH", education::TIP_DEPTH),
        ("PEAK V", education::TIP_PEAK_V),
        ("MEAN V", education::TIP_MEAN_V),
        ("LOSS", education::TIP_LOSS),
        ("TIME", education::TIP_TIME),
    ];
    let (header_rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 16.0), egui::Sense::hover());
    let header_painter = ui.painter_at(header_rect);
    for (i, ((label, tooltip), x)) in headers.iter().zip(REP_COL_X).enumerate() {
        header_painter.text(
            egui::pos2(header_rect.left() + x, header_rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::monospace(9.0),
            weak_color,
        );
        let next_x = REP_COL_X.get(i + 1).copied().unwrap_or(header_rect.width());
        let cell_rect = egui::Rect::from_min_max(
            egui::pos2(header_rect.left() + x, header_rect.top()),
            egui::pos2(header_rect.left() + next_x, header_rect.bottom()),
        );
        ui.interact(
            cell_rect,
            ui.id().with(("rep_table_header", i)),
            egui::Sense::hover(),
        )
        .on_hover_text(*tooltip);
    }

    let mut action: Option<RepTableAction> = None;
    for (i, row) in rows.iter().enumerate() {
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), REP_ROW_HEIGHT),
            egui::Sense::click(),
        );
        if !ui.is_rect_visible(rect) {
            continue;
        }
        let painter = ui.painter_at(rect);
        if row.selected {
            painter.rect_filled(rect, 0.0, chrome.accent.gamma_multiply(0.18));
        } else if response.hovered() {
            painter.rect_filled(rect, 0.0, weak_color.gamma_multiply(0.10));
        }
        // 3px left border in the loss-severity color (rep 1's missing loss
        // counts as 0% → green, matching the mock's `lossColor` for rep 1).
        let severity = palette::loss_severity(row.loss.unwrap_or(0.0), threshold);
        let loss_color = palette::loss_severity_color(dark_mode, severity);
        painter.rect_filled(
            egui::Rect::from_min_max(rect.min, egui::pos2(rect.left() + 3.0, rect.bottom())),
            0.0,
            loss_color,
        );
        let cy = rect.center().y;
        let col = |x: f32, text: &str, color: egui::Color32| {
            painter.text(
                egui::pos2(rect.left() + x, cy),
                egui::Align2::LEFT_CENTER,
                text,
                font.clone(),
                color,
            );
        };
        col(REP_COL_X[0], &(i + 1).to_string(), weak_color);
        col(REP_COL_X[1], &row.depth, text_color);
        col(REP_COL_X[2], &row.peak, text_color);
        col(REP_COL_X[3], &row.mean, text_color);
        match row.loss {
            Some(loss) => col(REP_COL_X[4], &format!("-{:.1}%", loss.max(0.0)), loss_color),
            None => col(REP_COL_X[4], "—", weak_color),
        }
        col(REP_COL_X[5], &row.range, weak_color);

        // ▶ (or ■ while looping) button, right-aligned; its response is
        // checked before the row's so a ▶ click never also row-selects.
        let button_rect = egui::Rect::from_min_size(
            egui::pos2(rect.right() - REP_PLAY_WIDTH - 4.0, rect.top() + 2.0),
            egui::vec2(REP_PLAY_WIDTH, REP_ROW_HEIGHT - 4.0),
        );
        let glyph = if row.clip_armed { "■" } else { "▶" };
        let play = ui
            .put(
                button_rect,
                egui::Button::new(egui::RichText::new(glyph).size(10.0)).small(),
            )
            .on_hover_text(if row.clip_armed {
                "stop the rep clip loop"
            } else {
                "play this rep as a loop"
            });
        if play.clicked() {
            action = Some(RepTableAction::Play(i));
        } else if response.clicked() {
            action = Some(RepTableAction::Select(i));
        } else if response.hovered() {
            response.on_hover_text(format!("Rep {}: click to jump", i + 1));
        }
    }
    match action {
        Some(RepTableAction::Play(i)) => state.toggle_rep_clip(i),
        Some(RepTableAction::Select(i)) => state.select_rep(i),
        None => {}
    }

    ui.add_space(6.0);
    // Task 19.3: remembered checkbox, default off — burning re-encodes
    // (slower than the plain stream copy), so it's opt-in rather than a
    // silent slowdown on the existing "just cut my clips" flow. Changing it
    // persists immediately (`theme::save_burn_overlay_in_rep_clips`), same
    // pattern as the "Full Path" toggle (15.2) and the stop threshold
    // (13.5), so the choice survives both later exports this session and a
    // restart.
    if ui
        .checkbox(
            &mut state.settings.burn_overlay_in_rep_clips,
            "Burn bar-path overlay into clips",
        )
        .on_hover_text(
            "when on, each exported clip has its own rep's bar-path drawn in \
             (slower — re-encodes instead of a plain stream copy)",
        )
        .changed()
    {
        crate::app::theme::save_burn_overlay_in_rep_clips(state.settings.burn_overlay_in_rep_clips);
    }
    if ui
        .add_enabled(
            state.can_export_rep_clips(),
            egui::Button::new("Export all rep clips"),
        )
        .on_hover_text(
            "write one <video>.repNN.mp4 per rep next to the video \
             (ffmpeg, in the background)",
        )
        .clicked()
    {
        state.start_rep_clip_export();
    }
}
