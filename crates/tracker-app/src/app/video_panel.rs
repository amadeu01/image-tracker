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

        let calibrating = matches!(app.state.mode, Mode::Calibrating { .. });

        if response.clicked() {
            if let Some(click_pos) = response.interact_pointer_pos() {
                if let Some(image_px) = screen_to_image_px(
                    click_pos,
                    image_rect,
                    app.state.metadata.display_width(),
                    app.state.metadata.display_height(),
                ) {
                    if app.state.mode == Mode::PlacingSeed {
                        app.state.place_seed(image_px);
                        if let Some(seed) = app.state.seed {
                            if let Ok(frame) = app.cache.get(seed.frame_index) {
                                let kind = tracker_core::suggest_tracker(
                                    &frame,
                                    seed.position,
                                    tracker_core::TrackerSuggestionConfig::default(),
                                );
                                app.state.note_seed_suggestion(kind);
                            }
                        }
                    } else if calibrating {
                        app.state.place_calibration_point(image_px);
                    }
                }
            }
        }

        if let Some(seed) = app.state.seed {
            if seed.frame_index == app.state.current_frame {
                draw_crosshair(
                    ui.painter(),
                    image_rect,
                    tex_size,
                    seed.position,
                    egui::Color32::from_rgb(255, 60, 60),
                );
            }
        }

        // Live tracking crosshair: the latest tracked/interpolated
        // position, shown only while the display frame has caught up to it
        // (the display is driven to follow progress in `poll_tracking`, so
        // in practice this is almost always true once a run is active).
        if let (Some(idx), Some(pos)) = (
            app.state.tracking_run.last_frame_index,
            app.state.tracking_run.last_position,
        ) {
            if idx == app.state.current_frame {
                draw_crosshair(
                    ui.painter(),
                    image_rect,
                    tex_size,
                    pos,
                    egui::Color32::from_rgb(60, 255, 120),
                );
            }
        }

        if let Mode::Calibrating {
            first_point: Some(first),
            ..
        } = app.state.mode
        {
            draw_calibration_pending_point(ui.painter(), image_rect, tex_size, first);
        }

        if let Some((a, b)) = app.state.last_calibration_segment {
            draw_calibration_segment(ui.painter(), image_rect, tex_size, a, b);
        }
    });
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
    let stroke = egui::Stroke::new(2.0, color);
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
    let stroke = egui::Stroke::new(2.0, color);
    painter.line_segment([a, b], stroke);
    painter.circle_filled(a, 4.0, color);
    painter.circle_filled(b, 4.0, color);
}
