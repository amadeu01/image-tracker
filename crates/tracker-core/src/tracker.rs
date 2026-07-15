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
    update_threshold: f64,
}

impl TemplateTrackerConfig {
    /// Starts a builder with sensible defaults (patch radius 5, search
    /// radius 15, min score 0.5, update threshold 0.7).
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

    /// Minimum effective (winning) score required to refresh the adaptive
    /// template with the newly matched patch. Marginal matches (below this
    /// but still above `min_score`) are accepted as `Found` without letting
    /// the adaptive template drift toward them.
    pub fn update_threshold(&self) -> f64 {
        self.update_threshold
    }
}

/// Builder for `TemplateTrackerConfig`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TemplateTrackerConfigBuilder {
    patch_radius: u32,
    search_radius: u32,
    min_score: f64,
    update_threshold: f64,
}

impl Default for TemplateTrackerConfigBuilder {
    fn default() -> Self {
        Self {
            patch_radius: 5,
            search_radius: 15,
            min_score: 0.5,
            update_threshold: 0.7,
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

    /// Minimum winning score required to refresh the adaptive template.
    pub fn update_threshold(mut self, threshold: f64) -> Self {
        self.update_threshold = threshold;
        self
    }

    pub fn build(self) -> TemplateTrackerConfig {
        TemplateTrackerConfig {
            patch_radius: self.patch_radius,
            search_radius: self.search_radius,
            min_score: self.min_score,
            update_threshold: self.update_threshold,
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

/// Anything that, given a frame and the last known position, produces a
/// `StepOutcome` for the current frame (see CONTEXT.md, "Tracker"). Lets
/// `TrackingSession`'s gap/coast logic (1.6) drive either a `TemplateTracker`
/// or a `ColorTracker` (4.2) interchangeably.
pub trait Tracker {
    fn step(&mut self, frame: &Frame, last_pos: Point) -> StepOutcome;
}

impl Tracker for TemplateTracker {
    fn step(&mut self, frame: &Frame, last_pos: Point) -> StepOutcome {
        TemplateTracker::step(self, frame, last_pos)
    }
}

/// Tracks a template patch (captured from a seed point on a reference
/// frame) across successive frames by searching a window centered on the
/// last known position and returning the best ZNCC match.
///
/// Dual-template matching (3.6): the *anchor* template is captured once at
/// the seed and never changes, preventing total drift away from the
/// originally-marked object. The *adaptive* template starts out identical
/// to the anchor but is periodically refreshed with the freshly-matched
/// patch, tracking gradual appearance change (rotation, lighting) that
/// would otherwise erode the anchor's match score over a long clip. Per
/// step, each candidate's effective score is `max(anchor_score,
/// adaptive_score)`; the best-scoring candidate wins. The adaptive
/// template is only refreshed when the winning effective score clears
/// `update_threshold` — comfortably above `min_score` — so marginal
/// matches (occlusion edges, near-misses) are accepted as `Found` without
/// letting the adaptive template creep toward the wrong thing.
#[derive(Debug, Clone, PartialEq)]
pub struct TemplateTracker {
    config: TemplateTrackerConfig,
    anchor: Patch,
    adaptive: Patch,
}

impl TemplateTracker {
    /// Captures the reference patch around `seed` in `frame`. Used as both
    /// the anchor and the initial adaptive template.
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
        Ok(Self {
            config,
            anchor: template.clone(),
            adaptive: template,
        })
    }

    /// Searches a window centered on `last_pos` in `frame` for the best
    /// match against `max(anchor_score, adaptive_score)`. Refreshes the
    /// adaptive template from the winning patch when its effective score
    /// clears `update_threshold`.
    pub fn step(&mut self, frame: &Frame, last_pos: Point) -> StepOutcome {
        let metric = Zncc;
        let cx = last_pos.x.round() as i64;
        let cy = last_pos.y.round() as i64;
        let r = self.config.search_radius as i64;

        let mut best: Option<(Point, f64, Patch)> = None;

        for dy in -r..=r {
            for dx in -r..=r {
                let x = cx + dx;
                let y = cy + dy;
                let Some(candidate) = extract_patch(frame, x, y, self.config.patch_radius) else {
                    continue;
                };
                let anchor_score = metric.score(&self.anchor, &candidate);
                let adaptive_score = metric.score(&self.adaptive, &candidate);
                let score = match (anchor_score, adaptive_score) {
                    (Some(a), Some(b)) => a.max(b),
                    (Some(a), None) => a,
                    (None, Some(b)) => b,
                    (None, None) => continue,
                };
                if best
                    .as_ref()
                    .is_none_or(|(_, best_score, _)| score > *best_score)
                {
                    best = Some((Point::new(x as f64, y as f64), score, candidate));
                }
            }
        }

        match best {
            Some((position, score, candidate)) if score >= self.config.min_score => {
                if score >= self.config.update_threshold {
                    self.adaptive = candidate;
                }
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
        assert_eq!(config.update_threshold(), 0.7);
    }

    #[test]
    fn config_builder_overrides_fields() {
        let config = plain_config();
        assert_eq!(config.patch_radius(), 3);
        assert_eq!(config.search_radius(), 6);
        assert_eq!(config.min_score(), 0.5);
    }

    #[test]
    fn config_builder_overrides_update_threshold() {
        let config = TemplateTrackerConfig::builder().update_threshold(0.8).build();
        assert_eq!(config.update_threshold(), 0.8);
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
        let mut tracker = TemplateTracker::new(&ref_frame, seed, plain_config()).unwrap();

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
        let mut tracker = TemplateTracker::new(&ref_frame, seed, plain_config()).unwrap();

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
        let mut tracker = TemplateTracker::new(&ref_frame, seed, config).unwrap();

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

    /// Two hand-picked pseudo-random patterns (not affine transforms of one
    /// another) used to synthesize gradual, per-pixel, non-affine appearance
    /// change — ZNCC is invariant to a single global affine transform of a
    /// patch's values, so a uniform brightness ramp would not actually
    /// exercise the adaptive template at all. Blending two independent
    /// patterns pixel-by-pixel does.
    fn pattern_a(dx: i64, dy: i64) -> u8 {
        (dx * 37 + dy * 17).rem_euclid(256) as u8
    }
    fn pattern_b(dx: i64, dy: i64) -> u8 {
        (dx * 83 + dy * 61 + 129).rem_euclid(256) as u8
    }

    /// Builds a frame with a `2*radius+1` square patch centered at `(cx,
    /// cy)` whose pixels are `t`-blended between `pattern_a` and
    /// `pattern_b`, on a flat mid-gray background.
    fn frame_with_blended_pattern(
        width: u32,
        height: u32,
        cx: i64,
        cy: i64,
        radius: i64,
        t: f64,
    ) -> Frame {
        let mut rgb = vec![128u8; (width * height * 3) as usize];
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                let x = cx + dx;
                let y = cy + dy;
                if x < 0 || y < 0 || x >= width as i64 || y >= height as i64 {
                    continue;
                }
                let a = pattern_a(dx, dy) as f64;
                let b = pattern_b(dx, dy) as f64;
                let v = ((1.0 - t) * a + t * b).round().clamp(0.0, 255.0) as u8;
                let idx = (y as usize * width as usize + x as usize) * 3;
                rgb[idx] = v;
                rgb[idx + 1] = v;
                rgb[idx + 2] = v;
            }
        }
        Frame::new(width, height, rgb).unwrap()
    }

    fn dual_template_config() -> TemplateTrackerConfig {
        TemplateTrackerConfig::builder()
            .patch_radius(3)
            .search_radius(2)
            .min_score(0.5)
            .update_threshold(0.7)
            .build()
    }

    #[test]
    fn dual_template_stays_found_through_gradual_appearance_change_that_would_lose_anchor_alone()
    {
        let width = 30;
        let height = 30;
        let pos = Point::new(15.0, 15.0);
        let cx = 15i64;
        let cy = 15i64;
        let radius = 3i64;

        let seed_frame = frame_with_blended_pattern(width, height, cx, cy, radius, 0.0);
        let config = dual_template_config();
        let mut tracker = TemplateTracker::new(&seed_frame, pos, config).unwrap();

        // Walk the appearance gradually from pattern_a (t=0.0) to pattern_b
        // (t=1.0) in small per-frame steps small enough that each
        // consecutive step scores above `update_threshold`, so the adaptive
        // template keeps re-locking on. The object never moves.
        let mut last_frame = seed_frame.clone();
        for step in 1..=10 {
            let t = step as f64 / 10.0;
            let frame = frame_with_blended_pattern(width, height, cx, cy, radius, t);
            match tracker.step(&frame, pos) {
                StepOutcome::Found { position, .. } => assert_eq!(position, pos),
                StepOutcome::Miss => panic!("dual-template lost the object at t={t}"),
            }
            last_frame = frame;
        }

        // Confirm the premise: an anchor-only tracker (no adaptive update)
        // really would have lost this by the end — the anchor patch (t=0.0)
        // scored directly against the final appearance (t=1.0) falls below
        // min_score.
        let anchor_patch = extract_patch(&seed_frame, cx, cy, config.patch_radius()).unwrap();
        let final_patch = extract_patch(&last_frame, cx, cy, config.patch_radius()).unwrap();
        let anchor_only_score = Zncc.score(&anchor_patch, &final_patch).unwrap();
        assert!(
            anchor_only_score < config.min_score(),
            "expected anchor-only score to have dropped below min_score, got {anchor_only_score}"
        );
    }

    #[test]
    fn occlusion_misses_without_corrupting_the_adaptive_template() {
        let width = 30;
        let height = 30;
        let pos = Point::new(15.0, 15.0);
        let cx = 15i64;
        let cy = 15i64;
        let radius = 3i64;

        let seed_frame = frame_with_blended_pattern(width, height, cx, cy, radius, 0.0);
        let config = dual_template_config();
        let mut tracker = TemplateTracker::new(&seed_frame, pos, config).unwrap();

        // Occlusion: the object is replaced by something wholly unrelated
        // (pattern_b, t=1.0) — far below min_score against both anchor and
        // adaptive (which still equals the anchor at this point).
        let occluder = frame_with_blended_pattern(width, height, cx, cy, radius, 1.0);
        assert_eq!(tracker.step(&occluder, pos), StepOutcome::Miss);

        // The object reappears exactly as it was at the seed. If the miss
        // had corrupted the adaptive template (e.g. adopted the occluder),
        // this would no longer score a near-perfect match.
        let outcome = tracker.step(&seed_frame, pos);
        match outcome {
            StepOutcome::Found { position, score } => {
                assert_eq!(position, pos);
                assert!(
                    score > 0.99,
                    "expected near-perfect self-match after occlusion, got {score}"
                );
            }
            StepOutcome::Miss => panic!("expected Found: object reappeared unchanged"),
        }
    }

    #[test]
    fn marginal_match_below_update_threshold_does_not_refresh_adaptive_template() {
        let width = 30;
        let height = 30;
        let pos = Point::new(15.0, 15.0);
        let cx = 15i64;
        let cy = 15i64;
        let radius = 3i64;

        let seed_frame = frame_with_blended_pattern(width, height, cx, cy, radius, 0.0);
        let config = dual_template_config();
        let mut tracker = TemplateTracker::new(&seed_frame, pos, config).unwrap();

        // t=0.55: scores ~0.53 against the anchor — above min_score (0.5)
        // but below update_threshold (0.7), a marginal match that should be
        // accepted as Found without refreshing the adaptive template.
        let marginal = frame_with_blended_pattern(width, height, cx, cy, radius, 0.55);
        let first = tracker.step(&marginal, pos);
        let first_score = match first {
            StepOutcome::Found { score, .. } => {
                assert!(score >= config.min_score() && score < config.update_threshold());
                score
            }
            StepOutcome::Miss => panic!("expected a marginal Found, got Miss"),
        };

        // Stepping against the exact same marginal frame again: if the
        // adaptive template had been replaced by that marginal patch, this
        // would now self-match at ~1.0. Since neither anchor nor adaptive
        // changed, the score should reproduce the same marginal value.
        let second = tracker.step(&marginal, pos);
        match second {
            StepOutcome::Found { score, .. } => {
                assert!(
                    (score - first_score).abs() < 1e-9,
                    "expected unchanged marginal score, got {score} vs {first_score} \
                     (adaptive template was likely refreshed on a marginal match)"
                );
            }
            StepOutcome::Miss => panic!("expected Found on repeat of the same marginal frame"),
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
        let mut tracker = TemplateTracker::new(&frame, seed, config).unwrap();

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
