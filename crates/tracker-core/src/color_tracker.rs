//! Color Tracker: locates a `ColorModel`'s color in successive frames by
//! scanning a window centered on the last known position and taking the
//! centroid of matching pixels (see CONTEXT.md, "Marker" / "Color Model" /
//! "Tracker").

use crate::color::ColorModel;
use crate::geometry::{Frame, Point};
use crate::tracker::{StepOutcome, Tracker};

/// Configuration for a `ColorTracker`, built via `ColorTrackerConfig::builder()`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorTrackerConfig {
    search_radius: u32,
    min_pixels: u32,
}

impl ColorTrackerConfig {
    /// Starts a builder with sensible defaults (search radius 25, min
    /// pixels 5).
    pub fn builder() -> ColorTrackerConfigBuilder {
        ColorTrackerConfigBuilder::default()
    }

    /// Half-width of the square search window around the last known position.
    pub fn search_radius(&self) -> u32 {
        self.search_radius
    }

    /// Minimum number of matching pixels within the search window required
    /// for a valid detection; below this, the step is a `Miss`.
    pub fn min_pixels(&self) -> u32 {
        self.min_pixels
    }
}

/// Builder for `ColorTrackerConfig`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorTrackerConfigBuilder {
    search_radius: u32,
    min_pixels: u32,
}

impl Default for ColorTrackerConfigBuilder {
    fn default() -> Self {
        Self {
            search_radius: 25,
            min_pixels: 5,
        }
    }
}

impl ColorTrackerConfigBuilder {
    /// Half-width of the square search window around the last known position.
    pub fn search_radius(mut self, radius: u32) -> Self {
        self.search_radius = radius;
        self
    }

    /// Minimum number of matching pixels required for a valid detection.
    pub fn min_pixels(mut self, count: u32) -> Self {
        self.min_pixels = count;
        self
    }

    pub fn build(self) -> ColorTrackerConfig {
        ColorTrackerConfig {
            search_radius: self.search_radius,
            min_pixels: self.min_pixels,
        }
    }
}

/// Tracks a `ColorModel` across successive frames: per step, scans the
/// square window centered on the last known position, collects every pixel
/// whose color matches the model, and reports the centroid (mean position)
/// of those pixels as `Found` — provided their count clears `min_pixels` —
/// or `Miss` otherwise.
///
/// Unlike `TemplateTracker`, there is no adaptive appearance model here: the
/// `ColorModel` is fixed at construction (learned once from the seed patch,
/// see `ColorModel::learn`), so a `ColorTracker` is stateless beyond its
/// config and model.
///
/// `Found`'s `score` is the fraction of scanned window pixels that matched
/// the color model (`matched / window_area`), a cheap proxy for how
/// solidly the marker fills the window — not a confidence bound like ZNCC.
#[derive(Debug, Clone, PartialEq)]
pub struct ColorTracker {
    model: ColorModel,
    config: ColorTrackerConfig,
}

impl ColorTracker {
    pub fn new(model: ColorModel, config: ColorTrackerConfig) -> Self {
        Self { model, config }
    }

    /// Scans the search window centered on `last_pos` in `frame`, collecting
    /// pixels matching `self.model`. Returns `Found` at the centroid of
    /// matching pixels if their count is at least `min_pixels`, else `Miss`.
    pub fn step(&mut self, frame: &Frame, last_pos: Point) -> StepOutcome {
        let cx = last_pos.x.round() as i64;
        let cy = last_pos.y.round() as i64;
        let r = self.config.search_radius as i64;

        let min_x = (cx - r).max(0);
        let max_x = (cx + r).min(frame.width() as i64 - 1);
        let min_y = (cy - r).max(0);
        let max_y = (cy + r).min(frame.height() as i64 - 1);

        if min_x > max_x || min_y > max_y {
            return StepOutcome::Miss;
        }

        let mut count: u64 = 0;
        let mut sum_x: f64 = 0.0;
        let mut sum_y: f64 = 0.0;
        let mut scanned: u64 = 0;

        for y in min_y..=max_y {
            for x in min_x..=max_x {
                scanned += 1;
                if let Some(rgb) = frame.pixel(x as u32, y as u32) {
                    if self.model.matches(rgb) {
                        count += 1;
                        sum_x += x as f64;
                        sum_y += y as f64;
                    }
                }
            }
        }

        if count == 0 || count < self.config.min_pixels as u64 {
            return StepOutcome::Miss;
        }

        let position = Point::new(sum_x / count as f64, sum_y / count as f64);
        let score = count as f64 / scanned as f64;
        StepOutcome::Found { position, score }
    }
}

impl Tracker for ColorTracker {
    fn step(&mut self, frame: &Frame, last_pos: Point) -> StepOutcome {
        ColorTracker::step(self, frame, last_pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::ColorModelConfig;

    /// Builds a `width`x`height` frame of `bg` with a `size`x`size` blob of
    /// `fg` whose top-left corner is at `(bx, by)`.
    fn frame_with_blob(
        width: u32,
        height: u32,
        bg: [u8; 3],
        blob: Option<(i64, i64, i64, [u8; 3])>,
    ) -> Frame {
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        for y in 0..height as i64 {
            for x in 0..width as i64 {
                let color = match blob {
                    Some((bx, by, size, fg)) if x >= bx && x < bx + size && y >= by && y < by + size => {
                        fg
                    }
                    _ => bg,
                };
                rgb.extend_from_slice(&color);
            }
        }
        Frame::new(width, height, rgb).unwrap()
    }

    fn red_model() -> ColorModel {
        let seed_frame = frame_with_blob(10, 10, [255, 0, 0], None);
        ColorModel::learn(&seed_frame, Point::new(5.0, 5.0), 2, ColorModelConfig::default())
            .unwrap()
    }

    fn plain_config() -> ColorTrackerConfig {
        ColorTrackerConfig::builder()
            .search_radius(15)
            .min_pixels(4)
            .build()
    }

    #[test]
    fn config_builder_has_defaults() {
        let config = ColorTrackerConfig::builder().build();
        assert_eq!(config.search_radius(), 25);
        assert_eq!(config.min_pixels(), 5);
    }

    #[test]
    fn config_builder_overrides_fields() {
        let config = plain_config();
        assert_eq!(config.search_radius(), 15);
        assert_eq!(config.min_pixels(), 4);
    }

    #[test]
    fn finds_exact_centroid_of_a_blob() {
        let model = red_model();
        // Gray background, red 4x4 blob top-left at (18, 18) -> centered at
        // (19.5, 19.5).
        let frame = frame_with_blob(40, 40, [128, 128, 128], Some((18, 18, 4, [255, 0, 0])));
        let mut tracker = ColorTracker::new(model, plain_config());

        let outcome = tracker.step(&frame, Point::new(20.0, 20.0));
        match outcome {
            StepOutcome::Found { position, score } => {
                assert!((position.x - 19.5).abs() < 1e-6);
                assert!((position.y - 19.5).abs() < 1e-6);
                assert!(score > 0.0 && score <= 1.0);
            }
            StepOutcome::Miss => panic!("expected Found"),
        }
    }

    #[test]
    fn follows_a_moved_blob() {
        let model = red_model();
        let frame = frame_with_blob(40, 40, [128, 128, 128], Some((25, 10, 4, [255, 0, 0])));
        let mut tracker = ColorTracker::new(model, plain_config());

        // Last known position is the blob's old spot; search radius (15) is
        // wide enough to still reach the moved blob at (25..29, 10..14).
        let outcome = tracker.step(&frame, Point::new(20.0, 20.0));
        match outcome {
            StepOutcome::Found { position, .. } => {
                assert!((position.x - 26.5).abs() < 1e-6);
                assert!((position.y - 11.5).abs() < 1e-6);
            }
            StepOutcome::Miss => panic!("expected Found"),
        }
    }

    #[test]
    fn misses_when_blob_absent() {
        let model = red_model();
        let frame = frame_with_blob(40, 40, [128, 128, 128], None);
        let mut tracker = ColorTracker::new(model, plain_config());

        let outcome = tracker.step(&frame, Point::new(20.0, 20.0));
        assert_eq!(outcome, StepOutcome::Miss);
    }

    #[test]
    fn misses_when_blob_smaller_than_min_pixels() {
        let model = red_model();
        // A single matching pixel: below min_pixels(4).
        let frame = frame_with_blob(40, 40, [128, 128, 128], Some((20, 20, 1, [255, 0, 0])));
        let mut tracker = ColorTracker::new(model, plain_config());

        let outcome = tracker.step(&frame, Point::new(20.0, 20.0));
        assert_eq!(outcome, StepOutcome::Miss);
    }

    #[test]
    fn ignores_a_second_blob_outside_the_search_window() {
        let model = red_model();
        let mut rgb = vec![128u8; 60 * 40 * 3];
        // In-window blob near (20, 20).
        for y in 18..22i64 {
            for x in 18..22i64 {
                let idx = (y as usize * 60 + x as usize) * 3;
                rgb[idx..idx + 3].copy_from_slice(&[255, 0, 0]);
            }
        }
        // Out-of-window blob far away at (55, 35), well beyond search_radius(15)
        // from (20, 20).
        for y in 33..37i64 {
            for x in 53..57i64 {
                let idx = (y as usize * 60 + x as usize) * 3;
                rgb[idx..idx + 3].copy_from_slice(&[255, 0, 0]);
            }
        }
        let frame = Frame::new(60, 40, rgb).unwrap();
        let mut tracker = ColorTracker::new(model, plain_config());

        let outcome = tracker.step(&frame, Point::new(20.0, 20.0));
        match outcome {
            StepOutcome::Found { position, .. } => {
                // Centroid of the 4x4 in-window blob at (18..22) is (19.5, 19.5).
                assert!((position.x - 19.5).abs() < 1e-6);
                assert!((position.y - 19.5).abs() < 1e-6);
            }
            StepOutcome::Miss => panic!("expected Found: in-window blob should be detected"),
        }
    }
}
