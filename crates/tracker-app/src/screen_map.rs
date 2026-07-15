//! Pure screen↔image coordinate mapping for task 2.4 (seed placement).
//!
//! The frame texture is drawn inside a widget rect, scaled to fit (never
//! upscaled beyond 1.0, matching `app.rs`'s `ensure_texture`/paint logic),
//! and centered — leaving letterbox bars on the shorter axis. This module
//! is the single source of truth for that mapping so it can be unit-tested
//! without an egui `Context`.

use egui::{Pos2, Rect, Vec2};

/// Compute the scale factor and the sub-rect (within `panel_rect`) that the
/// image is actually drawn into, given `image_size` (in pixels) fitted to
/// `panel_rect` with letterboxing and capped at 1.0 (no upscaling).
pub fn fitted_image_rect(panel_rect: Rect, image_size: Vec2) -> Rect {
    if image_size.x <= 0.0
        || image_size.y <= 0.0
        || panel_rect.width() <= 0.0
        || panel_rect.height() <= 0.0
    {
        return Rect::from_min_size(panel_rect.min, Vec2::ZERO);
    }
    let scale = (panel_rect.width() / image_size.x)
        .min(panel_rect.height() / image_size.y)
        .min(1.0);
    let drawn_size = image_size * scale;
    let offset = (panel_rect.size() - drawn_size) / 2.0;
    Rect::from_min_size(panel_rect.min + offset, drawn_size)
}

/// Map a click position (in the same coordinate space as `panel_rect`, i.e.
/// egui screen coords) to image-pixel coordinates, given the panel rect the
/// image is displayed in and the image's native pixel dimensions.
///
/// Returns `None` if the click falls outside the drawn image area (in the
/// letterbox bars) or if the image/panel has zero/negative size.
pub fn screen_to_image_px(
    click: Pos2,
    panel_rect: Rect,
    image_width: u32,
    image_height: u32,
) -> Option<tracker_core::Point> {
    if image_width == 0 || image_height == 0 {
        return None;
    }
    let image_size = Vec2::new(image_width as f32, image_height as f32);
    let drawn = fitted_image_rect(panel_rect, image_size);
    if drawn.width() <= 0.0 || drawn.height() <= 0.0 || !drawn.contains(click) {
        return None;
    }
    let local = click - drawn.min;
    let scale_x = image_size.x / drawn.width();
    let scale_y = image_size.y / drawn.height();
    let x = (local.x * scale_x) as f64;
    let y = (local.y * scale_y) as f64;
    Some(tracker_core::Point::new(x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: f32, y0: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(Pos2::new(x0, y0), Vec2::new(w, h))
    }

    #[test]
    fn click_at_top_left_of_drawn_image_maps_to_origin() {
        // panel exactly matches a 100x50 image at scale 1.0, no letterbox
        let panel = rect(0.0, 0.0, 100.0, 50.0);
        let p = screen_to_image_px(Pos2::new(0.0, 0.0), panel, 100, 50).unwrap();
        assert!((p.x - 0.0).abs() < 1e-3);
        assert!((p.y - 0.0).abs() < 1e-3);
    }

    #[test]
    fn click_at_center_maps_to_image_center() {
        let panel = rect(0.0, 0.0, 100.0, 50.0);
        let p = screen_to_image_px(Pos2::new(50.0, 25.0), panel, 100, 50).unwrap();
        assert!((p.x - 50.0).abs() < 1e-3);
        assert!((p.y - 25.0).abs() < 1e-3);
    }

    #[test]
    fn scale_down_when_panel_smaller_than_image() {
        // image 200x100 fitted into a 100x100 panel -> scale 0.5, letterboxed vertically
        let panel = rect(0.0, 0.0, 100.0, 100.0);
        // drawn image: 100x50, centered vertically -> offset y = 25
        let p = screen_to_image_px(Pos2::new(0.0, 25.0), panel, 200, 100).unwrap();
        assert!((p.x - 0.0).abs() < 1e-3);
        assert!((p.y - 0.0).abs() < 1e-3);

        let p2 = screen_to_image_px(Pos2::new(100.0, 75.0), panel, 200, 100).unwrap();
        assert!((p2.x - 200.0).abs() < 1e-2);
        assert!((p2.y - 100.0).abs() < 1e-2);
    }

    #[test]
    fn click_in_letterbox_bar_is_none() {
        let panel = rect(0.0, 0.0, 100.0, 100.0);
        // drawn image is 100x50 vertically centered (y in [25, 75])
        let p = screen_to_image_px(Pos2::new(50.0, 10.0), panel, 200, 100);
        assert_eq!(p, None);
    }

    #[test]
    fn click_outside_panel_entirely_is_none() {
        let panel = rect(0.0, 0.0, 100.0, 100.0);
        let p = screen_to_image_px(Pos2::new(500.0, 500.0), panel, 200, 100);
        assert_eq!(p, None);
    }

    #[test]
    fn never_upscales_beyond_one() {
        // panel much bigger than a tiny image: scale capped at 1.0
        let panel = rect(0.0, 0.0, 1000.0, 1000.0);
        // drawn image is 10x10 centered -> offset (495, 495)
        let p = screen_to_image_px(Pos2::new(495.0, 495.0), panel, 10, 10).unwrap();
        assert!((p.x - 0.0).abs() < 1e-3);
        assert!((p.y - 0.0).abs() < 1e-3);
    }

    #[test]
    fn zero_sized_image_dims_returns_none() {
        let panel = rect(0.0, 0.0, 100.0, 100.0);
        assert_eq!(screen_to_image_px(Pos2::new(1.0, 1.0), panel, 0, 100), None);
        assert_eq!(screen_to_image_px(Pos2::new(1.0, 1.0), panel, 100, 0), None);
    }
}
