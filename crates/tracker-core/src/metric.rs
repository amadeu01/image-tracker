//! Correlation metrics for comparing two patches (e.g. template vs. search window).

use crate::patch::Patch;

/// A pluggable similarity metric between two equally-shaped `Patch`es.
///
/// Implementations return `None` when the patches have mismatched sizes
/// (different `side()`), since a per-pixel comparison is undefined in that
/// case. Otherwise they return `Some(score)`.
pub trait CorrelationMetric {
    /// Score `a` against `b`. Higher is more similar.
    fn score(&self, a: &Patch, b: &Patch) -> Option<f64>;
}

/// Zero-mean Normalized Cross-Correlation.
///
/// Invariant to linear brightness/contrast changes (a positive affine
/// transform of pixel values does not change the score). Range is
/// `[-1.0, 1.0]` for non-constant patches; a zero-variance (constant) patch
/// yields `0.0` rather than dividing by zero / producing `NaN`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Zncc;

impl CorrelationMetric for Zncc {
    fn score(&self, a: &Patch, b: &Patch) -> Option<f64> {
        if a.side() != b.side() {
            return None;
        }

        let av = a.values();
        let bv = b.values();

        let n = av.len() as f64;
        if n == 0.0 {
            return Some(0.0);
        }

        let mean_a: f64 = av.iter().map(|&v| v as f64).sum::<f64>() / n;
        let mean_b: f64 = bv.iter().map(|&v| v as f64).sum::<f64>() / n;

        let mut num = 0.0;
        let mut var_a = 0.0;
        let mut var_b = 0.0;
        for i in 0..av.len() {
            let da = av[i] as f64 - mean_a;
            let db = bv[i] as f64 - mean_b;
            num += da * db;
            var_a += da * da;
            var_b += db * db;
        }

        let denom = (var_a * var_b).sqrt();
        if denom == 0.0 {
            return Some(0.0);
        }

        Some(num / denom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Frame;
    use crate::patch::extract_patch;

    fn checker_frame() -> Frame {
        // 3x3 frame with a non-constant pattern.
        let vals = [10u8, 200, 30, 90, 5, 250, 60, 120, 180];
        let mut rgb = Vec::new();
        for v in vals {
            rgb.extend_from_slice(&[v, v, v]);
        }
        Frame::new(3, 3, rgb).unwrap()
    }

    fn uniform_frame(width: u32, height: u32, color: [u8; 3]) -> Frame {
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        for _ in 0..(width * height) {
            rgb.extend_from_slice(&color);
        }
        Frame::new(width, height, rgb).unwrap()
    }

    #[test]
    fn zncc_of_patch_with_itself_is_one() {
        let frame = checker_frame();
        let patch = extract_patch(&frame, 1, 1, 1).unwrap();
        let score = Zncc.score(&patch, &patch).unwrap();
        assert!((score - 1.0).abs() < 1e-9);
    }

    #[test]
    fn zncc_is_invariant_to_brightness_and_contrast_change() {
        let frame = checker_frame();
        let patch = extract_patch(&frame, 1, 1, 1).unwrap();

        // b = 0.5*a + 10: positive affine transform of every value (kept
        // within the 0..=255 byte range so the round-trip through `Frame`
        // doesn't clamp and break linearity).
        let side = patch.side();
        let transformed: Vec<f32> = patch.values().iter().map(|&v| 0.5 * v + 10.0).collect();
        let mut rgb = Vec::new();
        for &v in &transformed {
            let byte = v.round().clamp(0.0, 255.0) as u8;
            rgb.extend_from_slice(&[byte, byte, byte]);
        }
        let transformed_frame = Frame::new(side, side, rgb).unwrap();
        let transformed_patch = extract_patch(
            &transformed_frame,
            (side / 2) as i64,
            (side / 2) as i64,
            side / 2,
        )
        .unwrap();

        let score = Zncc.score(&patch, &transformed_patch).unwrap();
        assert!((score - 1.0).abs() < 1e-4);
    }

    #[test]
    fn zncc_of_constant_patch_is_zero_not_nan() {
        let frame = uniform_frame(3, 3, [100, 100, 100]);
        let patch = extract_patch(&frame, 1, 1, 1).unwrap();
        let score = Zncc.score(&patch, &patch).unwrap();
        assert_eq!(score, 0.0);
        assert!(!score.is_nan());
    }

    #[test]
    fn zncc_returns_none_for_mismatched_patch_sizes() {
        let frame = checker_frame();
        let big_frame = uniform_frame(10, 10, [50, 50, 50]);
        let small = extract_patch(&frame, 1, 1, 0).unwrap();
        let big = extract_patch(&big_frame, 5, 5, 1).unwrap();
        assert_eq!(Zncc.score(&small, &big), None);
    }
}
