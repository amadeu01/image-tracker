//! Central video panel (task 7.2 split): the frame image, click handling
//! (seed placement / calibration points), and the crosshair/calibration
//! overlays drawn on top of it. Needs both the frame cache/texture (owned by
//! `TrackerApp`) and `AppState`, so it takes the whole `TrackerApp`.

use eframe::egui;

use super::state::Mode;
use super::TrackerApp;
use crate::screen_map::screen_to_image_px;

pub fn show(app: &mut TrackerApp, ctx: &egui::Context) {
    egui::CentralPanel::default().show(ctx, |ui| {
        let (Some(state), Some(cache)) = (&mut app.state, &mut app.cache) else {
            empty_state_prompt(ui, app.open_error.as_deref());
            return;
        };
        let Some(texture) = &app.texture else {
            ui.label("decoding first frame...");
            return;
        };
        let texture = texture.clone();
        let available = ui.available_size();
        let tex_size = texture.size_vec2();
        let scale = (available.x / tex_size.x)
            .min(available.y / tex_size.y)
            .min(1.0);
        let response =
            ui.add(egui::Image::new((texture.id(), tex_size * scale)).sense(egui::Sense::click()));
        let image_rect = response.rect;

        let calibrating = matches!(state.mode, Mode::Calibrating { .. });

        if response.clicked() {
            if let Some(click_pos) = response.interact_pointer_pos() {
                if let Some(image_px) = screen_to_image_px(
                    click_pos,
                    image_rect,
                    state.metadata.display_width(),
                    state.metadata.display_height(),
                ) {
                    if state.mode == Mode::PlacingSeed {
                        state.place_seed(image_px);
                        if let Some(seed) = state.seed {
                            if let Ok(frame) = cache.get(seed.frame_index) {
                                let kind = tracker_core::suggest_tracker(
                                    &frame,
                                    seed.position,
                                    tracker_core::TrackerSuggestionConfig::default(),
                                );
                                state.note_seed_suggestion(kind);
                            }
                        }
                    } else if calibrating {
                        state.place_calibration_point(image_px);
                    }
                }
            }
        }

        if let Some(seed) = state.seed {
            if seed.frame_index == state.current_frame {
                draw_crosshair(
                    ui.painter(),
                    image_rect,
                    tex_size,
                    seed.position,
                    egui::Color32::from_rgb(255, 60, 60),
                );
            }
        }

        // Live tracking crosshair: shown only while the display frame has
        // caught up to the run's progress (the display is driven to follow
        // progress in `poll_tracking`, so in practice this is almost always
        // true once a run is active).
        //
        // 10.2: while the session is coasting through a gap or paused
        // awaiting reseed, drawing at `last_position` would show the
        // crosshair wandering along the interpolated path toward wherever
        // it eventually (mis)reacquires — the "jumped to the rack" bug.
        // Instead freeze at `last_tracked_position` (the last real match)
        // and render gray, honestly showing "lost" rather than a confident
        // green lock.
        if let (Some(idx), Some(pos)) = (
            state.tracking_run.last_frame_index,
            state.tracking_run.last_tracked_position,
        ) {
            if idx == state.current_frame {
                let color = if state.tracking_run.is_searching() {
                    egui::Color32::from_rgb(150, 150, 150)
                } else {
                    egui::Color32::from_rgb(60, 255, 120)
                };
                draw_crosshair(ui.painter(), image_rect, tex_size, pos, color);
            }
        }

        if let Mode::Calibrating {
            first_point: Some(first),
            ..
        } = state.mode
        {
            draw_calibration_pending_point(ui.painter(), image_rect, tex_size, first);
        }

        if let Some((a, b)) = state.last_calibration_segment {
            draw_calibration_segment(ui.painter(), image_rect, tex_size, a, b);
        }

        // Bar path polyline (task 10.3): drawn once a run's finished
        // (Review step). Tracked segments green, interpolated (coasted
        // over a gap) orange, so the honest/fabricated split from
        // CONTEXT.md's "Gap" term is visible on the path itself, not just
        // in the Results section's quality line.
        //
        // 15.2: the whole overlay (polyline + its white current-position
        // crosshair) is gated on `show_path` — the transport-row toggle.
        // The *live* tracking crosshair above and the Seed marker are
        // deliberately not gated: lock-on feedback stays visible even with
        // the path hidden.
        if state.show_path {
            if let Some(results) = &state.results {
                draw_bar_path(
                    ui.painter(),
                    image_rect,
                    tex_size,
                    results.bar_path.points(),
                );
                if let Some(point) = results.bar_path.position_at(state.current_frame) {
                    draw_crosshair(
                        ui.painter(),
                        image_rect,
                        tex_size,
                        point.position,
                        egui::Color32::WHITE,
                    );
                }
            }
        }
    });
}

/// Draws the Bar Path as a polyline: consecutive points are joined with a
/// green segment if both are `Source::Tracked`, orange if either endpoint
/// is `Source::Interpolated` (a coasted-over gap). Painter overlay only —
/// never mutates frame pixels.
fn draw_bar_path(
    painter: &egui::Painter,
    image_rect: egui::Rect,
    image_native_size: egui::Vec2,
    points: &[tracker_core::PathPoint],
) {
    let tracked = egui::Color32::from_rgb(60, 200, 90);
    let interpolated = egui::Color32::from_rgb(255, 165, 0);
    for pair in points.windows(2) {
        let (Some(a), Some(b)) = (
            image_px_to_screen(image_rect, image_native_size, pair[0].position),
            image_px_to_screen(image_rect, image_native_size, pair[1].position),
        ) else {
            continue;
        };
        let color = if pair[0].source == tracker_core::Source::Interpolated
            || pair[1].source == tracker_core::Source::Interpolated
        {
            interpolated
        } else {
            tracked
        };
        painter.line_segment([a, b], egui::Stroke::new(2.0_f32, color));
    }
}

/// Draw a crosshair marker at an image-pixel position (the Seed, red; the
/// live tracking position, green — see call sites), converting back to
/// screen coordinates for the currently drawn (scaled, letterboxed) image
/// rect. Painter overlay only — never mutates frame pixels.
fn draw_crosshair(
    painter: &egui::Painter,
    image_rect: egui::Rect,
    image_native_size: egui::Vec2,
    px: tracker_core::Point,
    color: egui::Color32,
) {
    if image_native_size.x <= 0.0 || image_native_size.y <= 0.0 {
        return;
    }
    let scale_x = image_rect.width() / image_native_size.x;
    let scale_y = image_rect.height() / image_native_size.y;
    let screen = image_rect.min + egui::Vec2::new(px.x as f32 * scale_x, px.y as f32 * scale_y);

    let radius = 8.0;
    let stroke = egui::Stroke::new(2.0_f32, color);
    painter.line_segment(
        [
            egui::pos2(screen.x - radius, screen.y),
            egui::pos2(screen.x + radius, screen.y),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(screen.x, screen.y - radius),
            egui::pos2(screen.x, screen.y + radius),
        ],
        stroke,
    );
    painter.circle_stroke(screen, radius * 0.6, stroke);
}

/// Central-panel content when no video is loaded (10.5): a friendly prompt
/// instead of a blank window, plus an inline "Open video…" button so the
/// toolbar button isn't the only way in. Shows the last "Open video" error
/// (if any), so a failed probe doesn't silently strand the user.
fn empty_state_prompt(ui: &mut egui::Ui, open_error: Option<&str>) {
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() / 3.0);
        ui.heading("Open a video to begin");
        ui.label("Ctrl+O, or the \"Open video…\" button above");
        if let Some(err) = open_error {
            ui.add_space(8.0);
            ui.colored_label(egui::Color32::RED, err);
        }
    });
}

/// Convert an image-pixel point to a screen point within the currently
/// drawn (scaled, letterboxed) image rect. Shared by the seed crosshair and
/// calibration overlays.
fn image_px_to_screen(
    image_rect: egui::Rect,
    image_native_size: egui::Vec2,
    px: tracker_core::Point,
) -> Option<egui::Pos2> {
    if image_native_size.x <= 0.0 || image_native_size.y <= 0.0 {
        return None;
    }
    let scale_x = image_rect.width() / image_native_size.x;
    let scale_y = image_rect.height() / image_native_size.y;
    Some(image_rect.min + egui::Vec2::new(px.x as f32 * scale_x, px.y as f32 * scale_y))
}

/// Draw a marker at the first-clicked calibration point, while awaiting the
/// second click.
fn draw_calibration_pending_point(
    painter: &egui::Painter,
    image_rect: egui::Rect,
    image_native_size: egui::Vec2,
    point_px: tracker_core::Point,
) {
    if let Some(screen) = image_px_to_screen(image_rect, image_native_size, point_px) {
        let color = egui::Color32::from_rgb(60, 160, 255);
        painter.circle_filled(screen, 4.0, color);
    }
}

/// Draw a line between the two most recently clicked calibration points,
/// with endpoint markers. Painter overlay only — never mutates frame pixels.
fn draw_calibration_segment(
    painter: &egui::Painter,
    image_rect: egui::Rect,
    image_native_size: egui::Vec2,
    a_px: tracker_core::Point,
    b_px: tracker_core::Point,
) {
    let (Some(a), Some(b)) = (
        image_px_to_screen(image_rect, image_native_size, a_px),
        image_px_to_screen(image_rect, image_native_size, b_px),
    ) else {
        return;
    };
    let color = egui::Color32::from_rgb(60, 160, 255);
    let stroke = egui::Stroke::new(2.0_f32, color);
    painter.line_segment([a, b], stroke);
    painter.circle_filled(a, 4.0, color);
    painter.circle_filled(b, 4.0, color);
}
