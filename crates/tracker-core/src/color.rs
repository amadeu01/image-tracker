//! `ColorModel`: a color signature learned by sampling pixels around a
//! seed patch (HSV median ± tolerance). No fixed color names — the model
//! represents whatever the user marked (see CONTEXT.md, "Color Model").

use crate::geometry::{Frame, Point};

/// Converts an RGB triple to HSV.
///
/// `h` is in degrees `[0, 360)`, `s` and `v` are in `[0, 1]`.
pub fn rgb_to_hsv(rgb: [u8; 3]) -> (f64, f64, f64) {
    let r = rgb[0] as f64 / 255.0;
    let g = rgb[1] as f64 / 255.0;
    let b = rgb[2] as f64 / 255.0;

    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;

    let h = if delta == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta).rem_euclid(6.0))
    } else if max == g {
        60.0 * (((b - r) / delta) + 2.0)
    } else {
        60.0 * (((r - g) / delta) + 4.0)
    };

    let s = if max == 0.0 { 0.0 } else { delta / max };
    let v = max;

    (h, s, v)
}

/// Errors that can occur when learning a `ColorModel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorModelError {
    /// The seed patch (center ± radius) falls outside the frame's bounds.
    OutOfBounds,
}

/// Configuration for `ColorModel::learn`: tolerance bands applied around
/// the learned median hue/saturation/value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorModelConfig {
    hue_tolerance: f64,
    sat_tolerance: f64,
    val_tolerance: f64,
}

impl ColorModelConfig {
    /// Sensible defaults: ±20° hue, ±0.3 saturation, ±0.35 value.
    pub fn default_tolerance() -> Self {
        Self {
            hue_tolerance: 20.0,
            sat_tolerance: 0.3,
            val_tolerance: 0.35,
        }
    }

    pub fn new(hue_tolerance: f64, sat_tolerance: f64, val_tolerance: f64) -> Self {
        Self {
            hue_tolerance,
            sat_tolerance,
            val_tolerance,
        }
    }
}

impl Default for ColorModelConfig {
    fn default() -> Self {
        Self::default_tolerance()
    }
}

/// A learned color signature: median hue/saturation/value ± tolerance,
/// sampled from a seed patch. `matches` is a cheap per-pixel check
/// intended for use in a per-frame search-window scan (milestone 4.2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorModel {
    hue: f64,
    sat: f64,
    val: f64,
    hue_tolerance: f64,
    sat_tolerance: f64,
    val_tolerance: f64,
}

impl ColorModel {
    /// Learns a `ColorModel` from the square patch of pixels centered at
    /// `center` with the given `radius`, in `frame`.
    ///
    /// Returns `ColorModelError::OutOfBounds` if the patch would fall
    /// outside the frame.
    pub fn learn(
        frame: &Frame,
        center: Point,
        radius: u32,
        config: ColorModelConfig,
    ) -> Result<Self, ColorModelError> {
        let cx = center.x.round() as i64;
        let cy = center.y.round() as i64;
        let r = radius as i64;

        let min_x = cx - r;
        let max_x = cx + r;
        let min_y = cy - r;
        let max_y = cy + r;
        if min_x < 0
            || min_y < 0
            || max_x >= frame.width() as i64
            || max_y >= frame.height() as i64
        {
            return Err(ColorModelError::OutOfBounds);
        }

        let mut hues = Vec::new();
        let mut sats = Vec::new();
        let mut vals = Vec::new();
        // Cartesian components of hue, to average around the circle
        // correctly (median-of-angle handled via a representative sample
        // below; see median_angle_deg for the wraparound-safe approach).
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                // Bounds already checked above; frame.pixel cannot fail here.
                if let Some(rgb) = frame.pixel(x as u32, y as u32) {
                    let (h, s, v) = rgb_to_hsv(rgb);
                    hues.push(h);
                    sats.push(s);
                    vals.push(v);
                }
            }
        }

        let hue = median_angle_deg(&hues);
        let sat = median(&mut sats);
        let val = median(&mut vals);

        Ok(Self {
            hue,
            sat,
            val,
            hue_tolerance: config.hue_tolerance,
            sat_tolerance: config.sat_tolerance,
            val_tolerance: config.val_tolerance,
        })
    }

    /// Returns `true` if `rgb` falls within this model's HSV tolerance
    /// band. Cheap: one HSV conversion plus three range checks.
    pub fn matches(&self, rgb: [u8; 3]) -> bool {
        let (h, s, v) = rgb_to_hsv(rgb);

        if hue_distance_deg(h, self.hue) > self.hue_tolerance {
            return false;
        }
        if (s - self.sat).abs() > self.sat_tolerance {
            return false;
        }
        if (v - self.val).abs() > self.val_tolerance {
            return false;
        }
        true
    }

    pub fn hue(&self) -> f64 {
        self.hue
    }

    pub fn sat(&self) -> f64 {
        self.sat
    }

    pub fn val(&self) -> f64 {
        self.val
    }
}

/// Median of a slice of values in `[0, 1]` (sorts a copy of `values`).
fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).expect("values must not be NaN"));
    let n = values.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

/// The shortest angular distance between two hues in degrees, accounting
/// for wraparound at 360°/0°. Always in `[0, 180]`.
fn hue_distance_deg(a: f64, b: f64) -> f64 {
    let diff = (a - b).abs() % 360.0;
    if diff > 180.0 {
        360.0 - diff
    } else {
        diff
    }
}

/// A wraparound-safe "median" of angles in degrees `[0, 360)`: converts each
/// angle to a unit vector, sums them, and takes the angle of the mean
/// vector. This is the circular mean, which behaves sensibly for near-0/360
/// clusters (e.g. reds) where a naive numeric median would be pulled toward
/// 180.
fn median_angle_deg(hues: &[f64]) -> f64 {
    if hues.is_empty() {
        return 0.0;
    }
    let (sum_sin, sum_cos) = hues.iter().fold((0.0, 0.0), |(s, c), h| {
        let rad = h.to_radians();
        (s + rad.sin(), c + rad.cos())
    });
    let mean = sum_sin.atan2(sum_cos).to_degrees();
    mean.rem_euclid(360.0)
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

    // --- rgb_to_hsv ---

    #[test]
    fn rgb_to_hsv_pure_red() {
        let (h, s, v) = rgb_to_hsv([255, 0, 0]);
        assert!((h - 0.0).abs() < 1e-6);
        assert!((s - 1.0).abs() < 1e-6);
        assert!((v - 1.0).abs() < 1e-6);
    }

    #[test]
    fn rgb_to_hsv_pure_green() {
        let (h, s, v) = rgb_to_hsv([0, 255, 0]);
        assert!((h - 120.0).abs() < 1e-6);
        assert!((s - 1.0).abs() < 1e-6);
        assert!((v - 1.0).abs() < 1e-6);
    }

    #[test]
    fn rgb_to_hsv_pure_blue() {
        let (h, s, v) = rgb_to_hsv([0, 0, 255]);
        assert!((h - 240.0).abs() < 1e-6);
        assert!((s - 1.0).abs() < 1e-6);
        assert!((v - 1.0).abs() < 1e-6);
    }

    #[test]
    fn rgb_to_hsv_gray_has_zero_saturation() {
        let (_h, s, v) = rgb_to_hsv([128, 128, 128]);
        assert!((s - 0.0).abs() < 1e-6);
        assert!((v - 128.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn rgb_to_hsv_black_has_zero_value() {
        let (_h, s, v) = rgb_to_hsv([0, 0, 0]);
        assert!((s - 0.0).abs() < 1e-6);
        assert!((v - 0.0).abs() < 1e-6);
    }

    // --- ColorModel::learn ---

    #[test]
    fn learn_from_uniform_red_patch_yields_red_model() {
        let frame = uniform_frame(10, 10, [255, 0, 0]);
        let model =
            ColorModel::learn(&frame, Point::new(5.0, 5.0), 2, ColorModelConfig::default())
                .unwrap();
        assert!((model.hue() - 0.0).abs() < 1e-6);
        assert!((model.sat() - 1.0).abs() < 1e-6);
        assert!((model.val() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn learn_out_of_bounds_returns_error() {
        let frame = uniform_frame(10, 10, [0, 0, 0]);
        let result =
            ColorModel::learn(&frame, Point::new(0.0, 0.0), 2, ColorModelConfig::default());
        assert_eq!(result, Err(ColorModelError::OutOfBounds));
    }

    #[test]
    fn learn_handles_hue_wraparound_near_red() {
        // Patch straddling 0°/360°: half at hue ~350°, half at hue ~10°.
        // The circular mean should land near 0°, not near 180° (which a
        // naive numeric median of [350, 10] would give as their average
        // wouldn't naively be 180, but a sorted-median over many samples
        // spanning the wrap would be pulled toward the middle of the
        // numeric range unless wraparound is handled).
        let mut rgb = Vec::new();
        let hue_350 = hsv_to_rgb_approx(350.0);
        let hue_10 = hsv_to_rgb_approx(10.0);
        for y in 0..5u32 {
            for _x in 0..5u32 {
                if y % 2 == 0 {
                    rgb.extend_from_slice(&hue_350);
                } else {
                    rgb.extend_from_slice(&hue_10);
                }
            }
        }
        let frame = Frame::new(5, 5, rgb).unwrap();
        let model =
            ColorModel::learn(&frame, Point::new(2.0, 2.0), 2, ColorModelConfig::default())
                .unwrap();
        // Should be near 0/360, i.e. within 15 degrees of 0 (wraparound-aware).
        let dist = hue_distance_deg(model.hue(), 0.0);
        assert!(dist < 15.0, "expected hue near 0, got {}", model.hue());
    }

    /// Approximate helper: builds a saturated, full-value RGB color at the
    /// given hue (for constructing wraparound test fixtures).
    fn hsv_to_rgb_approx(h: f64) -> [u8; 3] {
        let c = 1.0; // s * v, with s = v = 1
        let x = c * (1.0 - (((h / 60.0) % 2.0) - 1.0).abs());
        let (r1, g1, b1) = if (0.0..60.0).contains(&h) {
            (c, x, 0.0)
        } else if (60.0..120.0).contains(&h) {
            (x, c, 0.0)
        } else if (120.0..180.0).contains(&h) {
            (0.0, c, x)
        } else if (180.0..240.0).contains(&h) {
            (0.0, x, c)
        } else if (240.0..300.0).contains(&h) {
            (x, 0.0, c)
        } else {
            (c, 0.0, x)
        };
        [
            (r1 * 255.0).round() as u8,
            (g1 * 255.0).round() as u8,
            (b1 * 255.0).round() as u8,
        ]
    }

    // --- ColorModel::matches ---

    #[test]
    fn matches_pixel_within_tolerance_band() {
        let frame = uniform_frame(10, 10, [255, 0, 0]);
        let model =
            ColorModel::learn(&frame, Point::new(5.0, 5.0), 2, ColorModelConfig::default())
                .unwrap();
        assert!(model.matches([255, 0, 0]));
        // A slightly darker/less saturated red should still match within
        // the default tolerance.
        assert!(model.matches([200, 30, 30]));
    }

    #[test]
    fn matches_rejects_pixel_outside_tolerance_band() {
        let frame = uniform_frame(10, 10, [255, 0, 0]);
        let model =
            ColorModel::learn(&frame, Point::new(5.0, 5.0), 2, ColorModelConfig::default())
                .unwrap();
        // Pure blue is far away in hue.
        assert!(!model.matches([0, 0, 255]));
    }

    #[test]
    fn matches_rejects_low_saturation_gray_against_saturated_model() {
        let frame = uniform_frame(10, 10, [255, 0, 0]);
        let model =
            ColorModel::learn(&frame, Point::new(5.0, 5.0), 2, ColorModelConfig::default())
                .unwrap();
        // Mid-gray has near-zero saturation, far outside the model's
        // saturation tolerance even though its hue is undefined/arbitrary.
        assert!(!model.matches([128, 128, 128]));
    }

    #[test]
    fn matches_a_gray_model_rejects_saturated_pixel() {
        let frame = uniform_frame(10, 10, [128, 128, 128]);
        let model =
            ColorModel::learn(&frame, Point::new(5.0, 5.0), 2, ColorModelConfig::default())
                .unwrap();
        assert!(model.matches([128, 128, 128]));
        assert!(!model.matches([255, 0, 0]));
    }
}
