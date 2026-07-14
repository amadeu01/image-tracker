//! Template Tracker: locates a seeded patch in successive frames by
//! searching a window centered on the last known position and picking the
//! best ZNCC match.

use crate::geometry::{Frame, Point};
use crate::metric::{CorrelationMetric, Zncc};
use crate::patch::{extract_patch, Patch};

/// Configuration for a `TemplateTracker`, built via `TemplateTrackerConfig::builder()`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TemplateTrackerConfig {
    patch_radius: u32,
    search_radius: u32,
    min_score: f64,
}

impl TemplateTrackerConfig {
    /// Starts a builder with sensible defaults (patch radius 5, search
    /// radius 15, min score 0.5).
    pub fn builder() -> TemplateTrackerConfigBuilder {
        TemplateTrackerConfigBuilder::default()
    }

    pub fn patch_radius(&self) -> u32 {
        self.patch_radius
    }

    pub fn search_radius(&self) -> u32 {
        self.search_radius
    }

    pub fn min_score(&self) -> f64 {
        self.min_score
    }
}

/// Builder for `TemplateTrackerConfig`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TemplateTrackerConfigBuilder {
    patch_radius: u32,
    search_radius: u32,
    min_score: f64,
}

impl Default for TemplateTrackerConfigBuilder {
    fn default() -> Self {
        Self {
            patch_radius: 5,
            search_radius: 15,
            min_score: 0.5,
        }
    }
}

impl TemplateTrackerConfigBuilder {
    /// Half-width of the square template patch extracted around the seed.
    pub fn patch_radius(mut self, radius: u32) -> Self {
        self.patch_radius = radius;
        self
    }

    /// Half-width of the square search window around the last known position.
    pub fn search_radius(mut self, radius: u32) -> Self {
        self.search_radius = radius;
        self
    }

    /// Minimum ZNCC score for a candidate to count as found rather than a miss.
    pub fn min_score(mut self, score: f64) -> Self {
        self.min_score = score;
        self
    }

    pub fn build(self) -> TemplateTrackerConfig {
        TemplateTrackerConfig {
            patch_radius: self.patch_radius,
            search_radius: self.search_radius,
            min_score: self.min_score,
        }
    }
}

/// Errors constructing a `TemplateTracker`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateTrackerError {
    /// The seed patch (seed position ± patch_radius) falls outside the
    /// reference frame's bounds.
    SeedPatchOutOfBounds,
}

/// Result of a single tracking `step`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StepOutcome {
    /// The template was located above the configured `min_score` threshold.
    Found { position: Point, score: f64 },
    /// No candidate in the search window scored above the threshold (or no
    /// candidate patch was available, e.g. window fully off-frame). Feeds
    /// the Gap logic (1.6): a miss signal.
    Miss,
}

/// Tracks a template patch (captured from a seed point on a reference
/// frame) across successive frames by searching a window centered on the
/// last known position and returning the best ZNCC match.
#[derive(Debug, Clone, PartialEq)]
pub struct TemplateTracker {
    config: TemplateTrackerConfig,
    template: Patch,
}

impl TemplateTracker {
    /// Captures the reference patch around `seed` in `frame`.
    ///
    /// Fails with `SeedPatchOutOfBounds` if the seed patch would extend
    /// past the frame's edges.
    pub fn new(
        frame: &Frame,
        seed: Point,
        config: TemplateTrackerConfig,
    ) -> Result<Self, TemplateTrackerError> {
        let template = extract_patch(
            frame,
            seed.x.round() as i64,
            seed.y.round() as i64,
            config.patch_radius,
        )
        .ok_or(TemplateTrackerError::SeedPatchOutOfBounds)?;
        Ok(Self { config, template })
    }

    /// Searches a window centered on `last_pos` in `frame` for the best
    /// ZNCC match against the reference template.
    pub fn step(&self, frame: &Frame, last_pos: Point) -> StepOutcome {
        let metric = Zncc;
        let cx = last_pos.x.round() as i64;
        let cy = last_pos.y.round() as i64;
        let r = self.config.search_radius as i64;

        let mut best: Option<(Point, f64)> = None;

        for dy in -r..=r {
            for dx in -r..=r {
                let x = cx + dx;
                let y = cy + dy;
                let Some(candidate) = extract_patch(frame, x, y, self.config.patch_radius) else {
                    continue;
                };
                let Some(score) = metric.score(&self.template, &candidate) else {
                    continue;
                };
                if best.is_none_or(|(_, best_score)| score > best_score) {
                    best = Some((Point::new(x as f64, y as f64), score));
                }
            }
        }

        match best {
            Some((position, score)) if score >= self.config.min_score => {
                StepOutcome::Found { position, score }
            }
            _ => StepOutcome::Miss,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a frame with a bright `size`x`size` square (value 220) on a
    /// dark background (value 20), with the square's top-left corner at
    /// `(sx, sy)`.
    fn frame_with_square(width: u32, height: u32, sx: i64, sy: i64, size: i64) -> Frame {
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        for y in 0..height as i64 {
            for x in 0..width as i64 {
                let inside = x >= sx && x < sx + size && y >= sy && y < sy + size;
                let v = if inside { 220u8 } else { 20u8 };
                rgb.extend_from_slice(&[v, v, v]);
            }
        }
        Frame::new(width, height, rgb).unwrap()
    }

    fn plain_config() -> TemplateTrackerConfig {
        TemplateTrackerConfig::builder()
            .patch_radius(3)
            .search_radius(6)
            .min_score(0.5)
            .build()
    }

    #[test]
    fn config_builder_has_defaults() {
        let config = TemplateTrackerConfig::builder().build();
        assert_eq!(config.patch_radius(), 5);
        assert_eq!(config.search_radius(), 15);
        assert_eq!(config.min_score(), 0.5);
    }

    #[test]
    fn config_builder_overrides_fields() {
        let config = plain_config();
        assert_eq!(config.patch_radius(), 3);
        assert_eq!(config.search_radius(), 6);
        assert_eq!(config.min_score(), 0.5);
    }

    #[test]
    fn new_fails_when_seed_patch_out_of_bounds() {
        let frame = frame_with_square(20, 20, 8, 8, 4);
        let seed = Point::new(0.0, 0.0); // radius 3 patch would go negative
        let result = TemplateTracker::new(&frame, seed, plain_config());
        assert_eq!(result, Err(TemplateTrackerError::SeedPatchOutOfBounds));
    }

    #[test]
    fn new_succeeds_when_seed_patch_in_bounds() {
        let frame = frame_with_square(20, 20, 8, 8, 4);
        let seed = Point::new(10.0, 10.0);
        assert!(TemplateTracker::new(&frame, seed, plain_config()).is_ok());
    }

    #[test]
    fn step_finds_object_moved_by_known_offset() {
        let width = 40;
        let height = 40;
        let size = 6;
        // Reference frame: square centered near (12, 12).
        let ref_frame = frame_with_square(width, height, 10, 10, size);
        let seed = Point::new(12.0, 12.0);
        let tracker = TemplateTracker::new(&ref_frame, seed, plain_config()).unwrap();

        // Next frame: same square moved by (dx, dy) = (5, -3).
        let moved_frame = frame_with_square(width, height, 15, 7, size);
        let last_pos = seed; // tracker starts searching around the last known position
        let outcome = tracker.step(&moved_frame, last_pos);

        match outcome {
            StepOutcome::Found { position, score } => {
                assert_eq!(position, Point::new(17.0, 9.0));
                assert!(score > 0.9, "expected high score, got {score}");
            }
            StepOutcome::Miss => panic!("expected Found, got Miss"),
        }
    }

    #[test]
    fn step_misses_when_object_absent() {
        let width = 40;
        let height = 40;
        let ref_frame = frame_with_square(width, height, 10, 10, 6);
        let seed = Point::new(12.0, 12.0);
        let tracker = TemplateTracker::new(&ref_frame, seed, plain_config()).unwrap();

        // Blank frame: no bright square anywhere.
        let blank = frame_with_square(width, height, -100, -100, 0);
        let outcome = tracker.step(&blank, seed);
        assert_eq!(outcome, StepOutcome::Miss);
    }

    #[test]
    fn step_respects_frame_edges_without_panicking() {
        let width = 20;
        let height = 20;
        let size = 4;
        // Square right at the top-left corner.
        let ref_frame = frame_with_square(width, height, 0, 0, size);
        let seed = Point::new(2.0, 2.0);
        let config = TemplateTrackerConfig::builder()
            .patch_radius(2)
            .search_radius(5)
            .min_score(0.5)
            .build();
        let tracker = TemplateTracker::new(&ref_frame, seed, config).unwrap();

        // Same frame, same position: search window extends past the
        // frame's negative edge but must not panic, and should still find
        // the object at its known location.
        let outcome = tracker.step(&ref_frame, seed);
        match outcome {
            StepOutcome::Found { position, score } => {
                assert_eq!(position, seed);
                assert!(score > 0.9);
            }
            StepOutcome::Miss => panic!("expected Found near border, got Miss"),
        }
    }

    #[test]
    fn step_search_window_stays_local_to_last_pos() {
        // Two identical squares far apart; tracker seeded near the first
        // must not jump to the second, distant one when it's just as good a
        // match but outside the search window.
        let width = 60;
        let height = 20;
        let size = 4;
        let mut rgb = vec![20u8; (width * height * 3) as usize];
        let mut set_square = |sx: i64, sy: i64| {
            for y in sy..sy + size {
                for x in sx..sx + size {
                    let idx = (y as usize * width as usize + x as usize) * 3;
                    rgb[idx] = 220;
                    rgb[idx + 1] = 220;
                    rgb[idx + 2] = 220;
                }
            }
        };
        set_square(5, 8);
        set_square(50, 8);
        let frame = Frame::new(width, height, rgb).unwrap();

        let seed = Point::new(7.0, 10.0);
        let config = TemplateTrackerConfig::builder()
            .patch_radius(2)
            .search_radius(5)
            .min_score(0.5)
            .build();
        let tracker = TemplateTracker::new(&frame, seed, config).unwrap();

        let outcome = tracker.step(&frame, seed);
        match outcome {
            StepOutcome::Found { position, .. } => {
                // Must find the nearby square, not jump to the far one.
                assert!(position.x < 20.0);
            }
            StepOutcome::Miss => panic!("expected Found near seed"),
        }
    }
}
