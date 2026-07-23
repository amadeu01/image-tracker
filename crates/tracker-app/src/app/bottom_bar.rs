//! Bottom bars (task 7.2 split): a one-line status summary and the scrub
//! bar. Detailed status (seed/calibration/tracking breakdown, events) moved
//! to the side panel (`side_panel.rs`) — this stays intentionally terse.

use eframe::egui;

use super::palette;
use super::state::AppState;
use super::theme;

/// Design's status bar (task 13.1) is "a monospace one-liner: file · frame ·
/// mode · seed · calibration" — `state.status_line()` already assembles the
/// mode/seed/calibration clause (see `state.rs`), so this restyle is font +
/// chrome only: every label goes through `egui::TextStyle::Monospace`
/// (numbers/paths line up instead of proportional-font jitter) and the
/// panel picks up the design's hairline top border via `chrome_palette`.
pub fn show_status_bar(ctx: &egui::Context, state: Option<&AppState>) {
    let dark_mode = ctx.style().visuals.dark_mode;
    let border = palette::chrome_palette(dark_mode).border;
    egui::TopBottomPanel::bottom("status_bar")
        .frame(egui::Frame::side_top_panel(&ctx.style()).stroke(egui::Stroke::new(1.0, border)))
        .show(ctx, |ui| {
            let Some(state) = state else {
                ui.label(
                    egui::RichText::new("no video open — Ctrl+O or \"Open video…\"").monospace(),
                );
                return;
            };
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{}  ·  frame {}/{}  ·  {}",
                        state.video_path.display(),
                        state.current_frame,
                        state.metadata.frame_count.unwrap_or(0).saturating_sub(1),
                        state.status_line(),
                    ))
                    .monospace(),
                );
                let tracking_active = state.tracking.is_some()
                    || state.tracking_run.error.is_some()
                    || state.bar_path.is_some();
                if tracking_active {
                    ui.separator();
                    let is_error = state.tracking_run.error.is_some();
                    let is_paused = state.tracking_run.session_state
                        == Some(tracker_core::SessionState::NeedsReseed);
                    let color = if is_error {
                        egui::Color32::RED
                    } else if is_paused {
                        egui::Color32::YELLOW
                    } else {
                        egui::Color32::LIGHT_GREEN
                    };
                    ui.label(
                        egui::RichText::new(state.tracking_run.status_line())
                            .monospace()
                            .color(color),
                    );
                }
                if !state.status.is_empty() {
                    ui.separator();
                    ui.label(
                        egui::RichText::new(&state.status)
                            .monospace()
                            .color(egui::Color32::RED),
                    );
                }
            });
        });
}

/// Height of the rep-segment strip, matching the design mock's 34px track.
const SEGMENT_BAR_HEIGHT: f32 = 34.0;
/// Vertical inset of segment blocks inside the strip (mock: top/bottom 6px).
const SEGMENT_INSET: f32 = 6.0;

/// `(left, width)` of a rep segment as fractions of the bar width — the
/// mock's `leftPct`/`widthPct` math (`start/total`, `(end-start)/total`),
/// clamped into `[0, 1]` so a rep whose bounds overshoot a mis-reported
/// `frame_count` can't paint outside the track. Zero/degenerate `total`
/// collapses to a zero-width segment at 0 rather than dividing by zero.
fn segment_fraction(start_frame: u64, end_frame: u64, total_frames: u64) -> (f32, f32) {
    if total_frames == 0 {
        return (0.0, 0.0);
    }
    let total = total_frames as f64;
    let left = (start_frame as f64 / total).clamp(0.0, 1.0);
    let right = (end_frame as f64 / total).clamp(0.0, 1.0);
    (left as f32, (right - left).max(0.0) as f32)
}

/// Which rep segment (index into `bounds`) contains a click at `frac`
/// (0..=1 across the bar) — the widget's hit test, kept as a pure function.
/// Ends are inclusive (a click exactly on a rep's last frame still selects
/// it); when adjacent reps share a boundary frame the earlier rep wins.
fn segment_at(bounds: &[(u64, u64)], total_frames: u64, frac: f32) -> Option<usize> {
    bounds.iter().position(|&(start, end)| {
        let (left, width) = segment_fraction(start, end, total_frames);
        frac >= left && frac <= left + width
    })
}

pub fn show_scrub_bar(ctx: &egui::Context, state: Option<&mut AppState>) {
    egui::TopBottomPanel::bottom("scrub_bar").show(ctx, |ui| {
        let Some(state) = state else {
            return;
        };
        show_rep_segments(ui, state);
        ui.horizontal(|ui| {
            // Bigger prev/next buttons (10.6) -- easier click targets than
            // the old text-sized buttons, since these get used far more
            // often than a one-off toolbar action.
            let button_size = egui::vec2(56.0, 28.0);
            if ui
                .add_sized(button_size, egui::Button::new("<< prev"))
                .on_hover_text("previous frame (←)")
                .clicked()
            {
                state.prev_frame();
            }
            let max = state.metadata.frame_count.unwrap_or(1).saturating_sub(1);
            let mut frame_val = state.current_frame;
            let slider = ui
                .add(egui::Slider::new(&mut frame_val, 0..=max))
                .on_hover_text("scrub to a frame (←/→ = ±1, Shift+←/→ = ±10)");
            if slider.changed() {
                state.set_frame(frame_val as i64);
            }
            if ui
                .add_sized(button_size, egui::Button::new("next >>"))
                .on_hover_text("next frame (→)")
                .clicked()
            {
                state.next_frame();
            }
            // Bar-path overlay scope toggle (task 15.2, repurposed by
            // 19.1). Off (default): only the selected rep's path segment
            // is drawn over the video. On: the whole-set polyline is drawn
            // — opt-in, since overlapping every rep's line reads as an
            // unreadable scribble that hides the bar. Lives in the
            // transport row so it's reachable both live and in review; the
            // click persists immediately (same save-on-change stance as the
            // stop-threshold DragValue).
            let mut show = state.show_path;
            if ui
                .toggle_value(&mut show, "👁 Full Path")
                .on_hover_text(
                    "off: show only the selected rep's path; on: show the whole set's path",
                )
                .changed()
            {
                state.show_path = show;
                theme::save_show_path(show);
            }
        });
    });
}

/// The rep-segment strip above the frame slider (task 13.2): one clickable
/// block per rep positioned by start/end frame, selected rep highlighted,
/// 2px playhead, and in/out markers while a rep clip is active (13.3 arms
/// that). Only rendered once results with reps exist — before that the
/// slider row alone is the whole scrub bar, unchanged. Clicking a segment
/// selects that rep and jumps to its start; clicking the empty track is a
/// coarse seek (the design notes' deviation: the slider below stays for
/// fine scrub).
fn show_rep_segments(ui: &mut egui::Ui, state: &mut AppState) {
    let bounds: Vec<(u64, u64)> = match &state.results {
        Some(results) if !results.reps.is_empty() => (0..results.reps.len())
            .filter_map(|i| results.rep_frame_bounds(i))
            .collect(),
        _ => return,
    };
    if bounds.is_empty() {
        return;
    }
    let total = state.metadata.frame_count.unwrap_or(1).saturating_sub(1);
    if total == 0 {
        return;
    }
    let dark_mode = ui.visuals().dark_mode;
    let chrome = palette::chrome_palette(dark_mode);

    let desired = egui::vec2(ui.available_width(), SEGMENT_BAR_HEIGHT);
    let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        4.0,
        chrome.app_bg,
        egui::Stroke::new(1.0, chrome.border),
    );

    // Segment blocks: accent fill at 0.16 alpha, selected at 0.45 + solid
    // accent border (the mock's rgba pairs, via gamma_multiply per the
    // design notes).
    let frac_x = |frac: f32| rect.left() + frac * rect.width();
    for (i, &(start, end)) in bounds.iter().enumerate() {
        let (left, width) = segment_fraction(start, end, total);
        let seg_rect = egui::Rect::from_min_max(
            egui::pos2(frac_x(left), rect.top() + SEGMENT_INSET),
            egui::pos2(frac_x(left + width), rect.bottom() - SEGMENT_INSET),
        );
        let selected = state.selected_rep == Some(i);
        let fill = chrome
            .accent
            .gamma_multiply(if selected { 0.45 } else { 0.16 });
        let border = if selected {
            chrome.accent
        } else {
            chrome.accent.gamma_multiply(0.35)
        };
        painter.rect(seg_rect, 3.0, fill, egui::Stroke::new(1.0, border));
    }

    // Playhead: 2px vline at the current frame.
    let playhead_x = frac_x((state.current_frame as f64 / total as f64).clamp(0.0, 1.0) as f32);
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(playhead_x - 1.0, rect.top()),
            egui::pos2(playhead_x + 1.0, rect.bottom()),
        ),
        0.0,
        ui.visuals().strong_text_color(),
    );

    // In/out markers: 2px accent vlines while a rep clip is active (13.3
    // sets `rep_clip`; nothing arms it yet, but the drawing seam is 13.2's).
    if let Some(clip_bounds) = state
        .rep_clip
        .and_then(|i| state.results.as_ref()?.rep_frame_bounds(i))
    {
        let (left, width) = segment_fraction(clip_bounds.0, clip_bounds.1, total);
        for frac in [left, left + width] {
            let x = frac_x(frac);
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(x - 1.0, rect.top()),
                    egui::pos2(x + 1.0, rect.bottom()),
                ),
                0.0,
                chrome.accent,
            );
        }
    }

    // Tooltip ("Rep N") + click handling, hit-tested through the pure
    // helpers so the logic stays unit-testable.
    let pointer_frac = |pos: egui::Pos2| ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
    if let Some(hover) = response.hover_pos() {
        if let Some(i) = segment_at(&bounds, total, pointer_frac(hover)) {
            response.clone().on_hover_text(format!("Rep {}", i + 1));
        }
    }
    if response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let frac = pointer_frac(pos);
            match segment_at(&bounds, total, frac) {
                Some(i) => state.select_rep(i),
                // Empty track: coarse seek to the clicked frame.
                None => state.set_frame((frac as f64 * total as f64).round() as i64),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_fraction_matches_the_mock_left_and_width_math() {
        // start/total and (end-start)/total, as in the design's JS.
        let (left, width) = segment_fraction(50, 100, 200);
        assert!((left - 0.25).abs() < 1e-6);
        assert!((width - 0.25).abs() < 1e-6);
    }

    #[test]
    fn segment_fraction_clamps_bounds_beyond_total_and_survives_zero_total() {
        let (left, width) = segment_fraction(150, 300, 200);
        assert!((left - 0.75).abs() < 1e-6);
        assert!((width - 0.25).abs() < 1e-6, "right edge clamps to 1.0");
        assert_eq!(segment_fraction(10, 20, 0), (0.0, 0.0));
    }

    #[test]
    fn segment_at_finds_the_containing_rep_inclusive_of_ends() {
        let bounds = [(10, 40), (60, 90)];
        assert_eq!(segment_at(&bounds, 100, 0.25), Some(0));
        assert_eq!(segment_at(&bounds, 100, 0.10), Some(0), "start inclusive");
        assert_eq!(segment_at(&bounds, 100, 0.40), Some(0), "end inclusive");
        assert_eq!(segment_at(&bounds, 100, 0.75), Some(1));
        assert_eq!(segment_at(&bounds, 100, 0.50), None, "gap between reps");
        assert_eq!(segment_at(&bounds, 100, 0.95), None, "after last rep");
    }

    #[test]
    fn segment_at_prefers_the_earlier_rep_on_a_shared_boundary() {
        let bounds = [(0, 50), (50, 100)];
        assert_eq!(segment_at(&bounds, 100, 0.5), Some(0));
    }
}
