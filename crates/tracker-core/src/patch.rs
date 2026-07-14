//! Grayscale patch extraction with bounds handling.

use crate::geometry::Frame;

/// A square grayscale patch extracted from a `Frame`.
///
/// Values are luma (f32), row-major, size `(2*radius+1)^2`.
#[derive(Debug, Clone, PartialEq)]
pub struct Patch {
    radius: u32,
    values: Vec<f32>,
}

impl Patch {
    /// The radius the patch was extracted with.
    pub fn radius(&self) -> u32 {
        self.radius
    }

    /// The side length of the square patch: `2*radius + 1`.
    pub fn side(&self) -> u32 {
        2 * self.radius + 1
    }

    /// Row-major luma values, length `side()^2`.
    pub fn values(&self) -> &[f32] {
        &self.values
    }

    /// The luma value at local offset `(dx, dy)` within the patch,
    /// where `dx, dy` are in `0..side()`.
    pub fn get(&self, dx: u32, dy: u32) -> Option<f32> {
        let side = self.side();
        if dx >= side || dy >= side {
            return None;
        }
        self.values.get((dy * side + dx) as usize).copied()
    }
}

/// Converts an RGB triple to luma using standard broadcast weights.
fn luma(rgb: [u8; 3]) -> f32 {
    0.299 * rgb[0] as f32 + 0.587 * rgb[1] as f32 + 0.114 * rgb[2] as f32
}

/// Extracts a square grayscale patch of radius `radius` centered at integer
/// pixel `(cx, cy)` in `frame`.
///
/// Returns `None` if any part of the patch would fall outside the frame's
/// bounds.
pub fn extract_patch(frame: &Frame, cx: i64, cy: i64, radius: u32) -> Option<Patch> {
    let r = radius as i64;
    let min_x = cx - r;
    let max_x = cx + r;
    let min_y = cy - r;
    let max_y = cy + r;
    if min_x < 0 || min_y < 0 {
        return None;
    }
    if max_x >= frame.width() as i64 || max_y >= frame.height() as i64 {
        return None;
    }

    let side = (2 * radius + 1) as usize;
    let mut values = Vec::with_capacity(side * side);
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let rgb = frame.pixel(x as u32, y as u32)?;
            values.push(luma(rgb));
        }
    }
    Some(Patch { radius, values })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uniform_frame(width: u32, height: u32, color: [u8; 3]) -> Frame {
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        for _ in 0..(width * height) {
            rgb.extend_from_slice(&color);
        }
        Frame::new(width, height, rgb).unwrap()
    }

    #[test]
    fn extract_patch_from_uniform_frame_has_expected_size_and_values() {
        let frame = uniform_frame(10, 10, [100, 100, 100]);
        let patch = extract_patch(&frame, 5, 5, 1).unwrap();
        assert_eq!(patch.side(), 3);
        assert_eq!(patch.values().len(), 9);
        for &v in patch.values() {
            assert!((v - 100.0).abs() < 1e-6);
        }
    }

    #[test]
    fn extract_patch_computes_luma_from_standard_weights() {
        // Pure red: luma = 0.299 * 255
        let frame = uniform_frame(3, 3, [255, 0, 0]);
        let patch = extract_patch(&frame, 1, 1, 0).unwrap();
        assert_eq!(patch.values().len(), 1);
        assert!((patch.values()[0] - 0.299 * 255.0).abs() < 1e-4);
    }

    #[test]
    fn extract_patch_returns_none_when_exceeding_left_top_bounds() {
        let frame = uniform_frame(10, 10, [0, 0, 0]);
        assert_eq!(extract_patch(&frame, 0, 5, 1), None);
        assert_eq!(extract_patch(&frame, 5, 0, 1), None);
    }

    #[test]
    fn extract_patch_returns_none_when_exceeding_right_bottom_bounds() {
        let frame = uniform_frame(10, 10, [0, 0, 0]);
        assert_eq!(extract_patch(&frame, 9, 5, 1), None);
        assert_eq!(extract_patch(&frame, 5, 9, 1), None);
    }

    #[test]
    fn extract_patch_succeeds_at_exact_corner_boundary() {
        let frame = uniform_frame(10, 10, [50, 50, 50]);
        // radius 0 at the very corner is valid.
        assert!(extract_patch(&frame, 0, 0, 0).is_some());
        assert!(extract_patch(&frame, 9, 9, 0).is_some());
    }

    #[test]
    fn extract_patch_is_row_major() {
        // 3x3 frame with distinct luma per column, radius 1 centered at (1, 1).
        let mut rgb = Vec::new();
        for _ in 0..3 {
            for v in [0u8, 100, 200] {
                rgb.extend_from_slice(&[v, v, v]);
            }
        }
        let frame = Frame::new(3, 3, rgb).unwrap();
        let patch = extract_patch(&frame, 1, 1, 1).unwrap();
        assert_eq!(patch.side(), 3);
        // Only one row; values should be left-to-right: 0, 100, 200 (as luma).
        assert!(patch.values()[0] < patch.values()[1]);
        assert!(patch.values()[1] < patch.values()[2]);
    }

    #[test]
    fn patch_get_returns_value_at_local_offset() {
        let mut rgb = Vec::new();
        for _ in 0..3 {
            for v in [0u8, 100, 200] {
                rgb.extend_from_slice(&[v, v, v]);
            }
        }
        let frame = Frame::new(3, 3, rgb).unwrap();
        let patch = extract_patch(&frame, 1, 1, 1).unwrap();
        assert!(patch.get(0, 0).unwrap() < patch.get(2, 0).unwrap());
        assert_eq!(patch.get(3, 0), None);
    }
}
