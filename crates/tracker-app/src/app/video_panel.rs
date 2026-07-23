//! Central video panel (task 7.2 split): the frame image, click handling
//! (seed placement / calibration points), and the crosshair/calibration
//! overlays drawn on top of it. Needs both the frame cache/texture (owned by
//! `TrackerApp`) and `AppState`, so it takes the whole `TrackerApp`.

use eframe::egui;

use super::palette;
use super::state::{effective_patch_radius, Mode, PATCH_RADIUS_RANGE};
use super::TrackerApp;
use crate::screen_map::screen_to_image_px;

pub fn show(app: &mut TrackerApp, ctx: &egui::Context) {
    egui::CentralPanel::default().show(ctx, |ui| {
        if app.state.is_none() {
            empty_state_prompt(ui, app.open_error.as_deref());
            return;
        };
        let Some(texture) = &app.texture else {
            ui.label("decoding first frame...");
            return;
        };
        let texture = texture.clone();
        // 18.1 (finding G2): the wanted frame (`state.current_frame`) may
        // not be the one `texture` currently shows — the decode worker just
        // hasn't replied yet (fast scrub, or tracking-follow outrunning
        // ffmpeg). Draw the stale texture rather than block, but say so.
        let decoding_current = app
            .state
            .as_ref()
            .is_some_and(|state| app.texture_frame != Some(state.current_frame));
        let Some(state) = &mut app.state else {
            return;
        };
        let available = ui.available_size();
        let tex_size = texture.size_vec2();
        let scale = (available.x / tex_size.x)
            .min(available.y / tex_size.y)
            .min(1.0);
        let response =
            ui.add(egui::Image::new((texture.id(), tex_size * scale)).sense(egui::Sense::click()));
        let image_rect = response.rect;

        // 18.1 point 3: a subtle non-blocking affordance while the wanted
        // frame hasn't arrived from the decode worker yet — the image above
        // is still the last frame that *did* arrive, never a block.
        if decoding_current {
            let galley = ui.painter().layout_no_wrap(
                "decoding…".to_string(),
                egui::FontId::proportional(12.0),
                egui::Color32::WHITE,
            );
            let anchor = image_rect.min + egui::vec2(8.0, 8.0);
            let text_rect = egui::Align2::LEFT_TOP.anchor_size(anchor, galley.size());
            ui.painter().rect_filled(
                text_rect.expand(4.0),
                3.0,
                egui::Color32::from_black_alpha(150),
            );
            ui.painter()
                .galley(text_rect.min, galley, egui::Color32::WHITE);
        }

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
                            // 18.1 (finding G2): this used to call
                            // `cache.get(seed.frame_index)` synchronously,
                            // spawning ffmpeg on the UI thread if the seed
                            // frame wasn't already cached. Now: use it
                            // immediately if the UI-side cache already has
                            // it (the common case — the seed is placed on
                            // the currently-displayed frame, which is
                            // already decoded), otherwise ask the decode
                            // worker and apply the suggestion later, once
                            // `poll_decode` sees the reply.
                            if let Some(frame) = app.frames.get(&seed.frame_index) {
                                let kind = tracker_core::suggest_tracker(
                                    frame,
                                    seed.position,
                                    tracker_core::TrackerSuggestionConfig::default(),
                                );
                                state.note_seed_suggestion(kind);
                            } else {
                                app.pending_seed_suggestion =
                                    Some((seed.frame_index, seed.position));
                                if let Some(decode) = &app.decode {
                                    decode.want(seed.frame_index);
                                }
                            }
                        }
                    } else if calibrating {
                        state.place_calibration_point(image_px);
                    }
                }
            }
        }

        // 15.3: scroll over the video while placing a seed resizes the
        // template patch region — writes the same `settings.patch_radius`
        // the Advanced DragValue edits (that knob stays the source of
        // truth; this is just another writer, clamped to the same range).
        if state.mode == Mode::PlacingSeed && response.hovered() {
            let scroll_y = ui.input(|i| i.raw_scroll_delta.y);
            if scroll_y.abs() > 0.0 {
                let step: i64 = if scroll_y > 0.0 { 1 } else { -1 };
                let next = (state.settings.patch_radius as i64 + step).clamp(
                    *PATCH_RADIUS_RANGE.start() as i64,
                    *PATCH_RADIUS_RANGE.end() as i64,
                );
                state.settings.patch_radius = next as u32;
            }
        }

        let patch_radius = effective_patch_radius(&state.settings);
        let accent = palette::chrome_palette(ui.visuals().dark_mode).accent;

        // 15.3: live preview of the patch region under the cursor while
        // placing, so the user sees the region they're about to seed.
        if state.mode == Mode::PlacingSeed {
            if let Some(hover_pos) = response.hover_pos() {
                if let Some(image_px) = screen_to_image_px(
                    hover_pos,
                    image_rect,
                    state.metadata.display_width(),
                    state.metadata.display_height(),
                ) {
                    draw_patch_region(
                        ui.painter(),
                        image_rect,
                        tex_size,
                        image_px,
                        patch_radius,
                        accent,
                    );
                    draw_patch_hint(ui.painter(), image_rect, tex_size, image_px, patch_radius);
                }
            }
        }

        if let Some(seed) = state.seed {
            if seed.frame_index == state.current_frame {
                // 15.3: the seed region IS the template patch — draw the
                // exact square the Template tracker will cut (side
                // 2*r + 1 source px), so radius changes are visible live.
                draw_patch_region(
                    ui.painter(),
                    image_rect,
                    tex_size,
                    seed.position,
                    patch_radius,
                    accent,
                );
                if state.mode == Mode::PlacingSeed {
                    draw_patch_hint(
                        ui.painter(),
                        image_rect,
                        tex_size,
                        seed.position,
                        patch_radius,
                    );
                }
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
        // 19.1: by default only the *selected rep*'s segment is drawn —
        // the whole-set polyline over several reps overlapped into an
        // unreadable scribble that hid the bar (user finding). 15.2's
        // `show_path` toggle is repurposed as the opt-in "whole set" view:
        // on, it draws the full polyline; off (default), it draws just
        // `selected_rep`'s frames via `SessionResults::path_points_to_draw`.
        // The *live* tracking crosshair above and the Seed marker are
        // deliberately not gated: lock-on feedback stays visible even with
        // the path hidden.
        if let Some(results) = &state.results {
            let points = results.path_points_to_draw(state.selected_rep, state.show_path);
            if !points.is_empty() {
                draw_bar_path(ui.painter(), image_rect, tex_size, points);
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

/// Draw the template patch region (task 15.3): the square of source pixels
/// the Template tracker cuts around the seed, side `2*radius + 1` image px,
/// mapped through the drawn image rect so it scales with fit/letterboxing.
/// Two-tone stroke — 1px dark outline outside a 1px accent stroke — so it
/// stays visible on both bright and dark video content. Painter overlay
/// only — never mutates frame pixels.
fn draw_patch_region(
    painter: &egui::Painter,
    image_rect: egui::Rect,
    image_native_size: egui::Vec2,
    center_px: tracker_core::Point,
    radius: u32,
    accent: egui::Color32,
) {
    let Some(center) = image_px_to_screen(image_rect, image_native_size, center_px) else {
        return;
    };
    if image_native_size.x <= 0.0 || image_native_size.y <= 0.0 {
        return;
    }
    let scale_x = image_rect.width() / image_native_size.x;
    let scale_y = image_rect.height() / image_native_size.y;
    // The patch spans center ± radius inclusive: 2r + 1 source pixels.
    let half = radius as f32 + 0.5;
    let rect = egui::Rect::from_center_size(
        center,
        egui::Vec2::new(2.0 * half * scale_x, 2.0 * half * scale_y),
    );
    let outline = egui::Color32::from_black_alpha(180);
    painter.rect_stroke(rect.expand(1.0), 0.0, egui::Stroke::new(1.0, outline));
    painter.rect_stroke(rect, 0.0, egui::Stroke::new(1.0, accent));
}

/// Small "patch NxN px — scroll to resize" label next to the patch region
/// while placing a seed (task 15.3), on a translucent backdrop so it reads
/// over any video content.
fn draw_patch_hint(
    painter: &egui::Painter,
    image_rect: egui::Rect,
    image_native_size: egui::Vec2,
    center_px: tracker_core::Point,
    radius: u32,
) {
    let Some(center) = image_px_to_screen(image_rect, image_native_size, center_px) else {
        return;
    };
    let scale_x = image_rect.width() / image_native_size.x.max(1.0);
    let side = 2 * radius + 1;
    let text = format!("patch {side}\u{d7}{side} px \u{2014} scroll to resize");
    let anchor = egui::pos2(center.x + (radius as f32 + 0.5) * scale_x + 8.0, center.y);
    let galley =
        painter.layout_no_wrap(text, egui::FontId::proportional(12.0), egui::Color32::WHITE);
    let text_rect = egui::Align2::LEFT_CENTER.anchor_size(anchor, galley.size());
    painter.rect_filled(
        text_rect.expand(3.0),
        3.0,
        egui::Color32::from_black_alpha(150),
    );
    painter.galley(text_rect.min, galley, egui::Color32::WHITE);
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
