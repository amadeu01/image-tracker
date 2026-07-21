//! Template Tracker: locates a seeded patch in successive frames by
//! searching a window centered on the last known position and picking the
//! best ZNCC match.

use crate::geometry::{Frame, Point};
use crate::metric::{CorrelationMetric, Zncc};
use crate::patch::{extract_patch, Patch};
use crate::preprocessor::PreprocessorChain;

/// Configuration for a `TemplateTracker`, built via `TemplateTrackerConfig::builder()`.
#[derive(Debug, Clone, PartialEq)]
pub struct TemplateTrackerConfig {
    patch_radius: u32,
    search_radius: u32,
    min_score: f64,
    update_threshold: f64,
    anchor_floor: f64,
    preprocessor: PreprocessorChain,
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

    /// Minimum score against the *anchor* template (captured once at the
    /// seed, never changed) for a candidate to be eligible at all — the
    /// anchor's veto (17.3). Prevents the drift ratchet where the adaptive
    /// template, refreshed from a slightly-wrong match, walks the tracker
    /// off the object one pixel per frame while still scoring high against
    /// itself (audit F3). Set below `min_score` so the anchor only rejects
    /// candidates it no longer recognizes as the seeded object, not merely
    /// weak ones; the adaptive still handles legitimate appearance change
    /// (rotation, lighting) above this floor.
    pub fn anchor_floor(&self) -> f64 {
        self.anchor_floor
    }

    /// The `Preprocessor` chain applied to the reference patch (at
    /// construction) and to every candidate patch (per step). Empty
    /// (identity/no-op) by default.
    pub fn preprocessor(&self) -> &PreprocessorChain {
        &self.preprocessor
    }
}

/// Builder for `TemplateTrackerConfig`.
#[derive(Debug, Clone, PartialEq)]
pub struct TemplateTrackerConfigBuilder {
    patch_radius: u32,
    search_radius: u32,
    min_score: f64,
    update_threshold: f64,
    anchor_floor: f64,
    preprocessor: PreprocessorChain,
}

impl Default for TemplateTrackerConfigBuilder {
    fn default() -> Self {
        Self {
            patch_radius: 5,
            search_radius: 15,
            min_score: 0.5,
            update_threshold: 0.7,
            anchor_floor: 0.3,
            preprocessor: PreprocessorChain::new(),
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

    /// Minimum anchor score for a candidate to be eligible (17.3 veto).
    pub fn anchor_floor(mut self, floor: f64) -> Self {
        self.anchor_floor = floor;
        self
    }

    /// Sets the `Preprocessor` chain applied to the reference patch and
    /// every candidate patch (see CONTEXT.md, "Preprocessor").
    pub fn preprocessor(mut self, chain: PreprocessorChain) -> Self {
        self.preprocessor = chain;
        self
    }

    pub fn build(self) -> TemplateTrackerConfig {
        TemplateTrackerConfig {
            patch_radius: self.patch_radius,
            search_radius: self.search_radius,
            min_score: self.min_score,
            update_threshold: self.update_threshold,
            anchor_floor: self.anchor_floor,
            preprocessor: self.preprocessor,
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
        // Same-space invariant (CONTEXT.md "Preprocessor"): the reference
        // patch must go through the identical chain every candidate patch
        // will go through in `step`.
        let template = config.preprocessor.apply_patch(&template);
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

        // Each eligible candidate carries its winning (effective) score and
        // its anchor score separately: the anchor score gates the adaptive
        // refresh (17.3), so it must survive to the accept step, not be
        // collapsed into the effective score here.
        let mut best: Option<(Point, f64, f64, Patch)> = None;

        for dy in -r..=r {
            for dx in -r..=r {
                let x = cx + dx;
                let y = cy + dy;
                let Some(candidate) = extract_patch(frame, x, y, self.config.patch_radius) else {
                    continue;
                };
                let candidate = self.config.preprocessor.apply_patch(&candidate);
                let anchor_score = metric.score(&self.anchor, &candidate);
                let adaptive_score = metric.score(&self.adaptive, &candidate);

                // The anchor's veto (17.3, audit F3): a candidate is only
                // eligible if the never-changing anchor still recognizes it
                // above `anchor_floor`. Without this, `max(anchor, adaptive)`
                // lets a candidate the anchor rejects win on the adaptive's
                // score alone, and the refresh writes that error back into
                // the adaptive — a drift ratchet with no restoring force.
                let Some(anchor_score) = anchor_score.filter(|a| *a >= self.config.anchor_floor)
                else {
                    continue;
                };

                // Among eligible candidates the effective (winning) score is
                // still max(anchor, adaptive): the adaptive may *refine* the
                // match within the anchor's approval, it just can't override
                // the anchor's identity check above.
                let score = match adaptive_score {
                    Some(b) => anchor_score.max(b),
                    None => anchor_score,
                };
                if best
                    .as_ref()
                    .is_none_or(|(_, best_score, _, _)| score > *best_score)
                {
                    best = Some((
                        Point::new(x as f64, y as f64),
                        score,
                        anchor_score,
                        candidate,
                    ));
                }
            }
        }

        match best {
            Some((position, score, _anchor_score, candidate))
                if score >= self.config.min_score =>
            {
                // Refresh the adaptive when the effective match is strong
                // (>= update_threshold). The anchor veto above already
                // guaranteed the winning candidate cleared `anchor_floor`,
                // so a refreshed adaptive can only ever be a patch the
                // anchor still recognizes as the seeded object — this keeps
                // the adaptive useful for real appearance change (a rotated
                // plate can score high on the adaptive while the anchor
                // sits at the floor) without reopening the drift ratchet
                // (audit F3), because a candidate the anchor has lost
                // entirely never becomes eligible to refresh from.
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
        assert_eq!(config.anchor_floor(), 0.3);
    }

    /// Builds a frame whose left half is one gray value and right half
    /// another, so a patch's appearance depends on where it sits — used to
    /// exercise anchor vs adaptive disagreement.
    fn split_frame(width: u32, height: u32, boundary: i64, left: u8, right: u8) -> Frame {
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        for _y in 0..height as i64 {
            for x in 0..width as i64 {
                let v = if x < boundary { left } else { right };
                rgb.extend_from_slice(&[v, v, v]);
            }
        }
        Frame::new(width, height, rgb).unwrap()
    }

    /// 17.3: a candidate the anchor rejects (below `anchor_floor`) must not
    /// be selected, even if it is the best available match — the anchor's
    /// veto. Regression against the `max(anchor, adaptive)` ratchet.
    #[test]
    fn anchor_floor_vetoes_candidates_the_anchor_rejects() {
        // Seed on a bright square: anchor learns "bright patch on dark".
        let seed_frame = frame_with_square(40, 40, 18, 18, 5);
        let config = TemplateTrackerConfig::builder()
            .patch_radius(3)
            .search_radius(6)
            .min_score(0.3)
            .anchor_floor(0.5)
            .build();
        let mut tracker = TemplateTracker::new(&seed_frame, Point::new(20.0, 20.0), config).unwrap();

        // Next frame: the square is gone entirely (uniform field). Nothing
        // resembles the anchor, so every candidate is vetoed → Miss, rather
        // than locking onto whatever scored least-badly.
        let empty = split_frame(40, 40, 40, 20, 20);
        assert_eq!(tracker.step(&empty, Point::new(20.0, 20.0)), StepOutcome::Miss);
    }

    /// 17.3: with a permissive (zero) floor the veto is disabled and the
    /// tracker behaves as before — guards against the floor silently
    /// breaking legitimate tracking.
    #[test]
    fn zero_anchor_floor_preserves_normal_tracking() {
        let seed_frame = frame_with_square(40, 40, 18, 18, 5);
        let config = TemplateTrackerConfig::builder()
            .patch_radius(3)
            .search_radius(6)
            .min_score(0.5)
            .anchor_floor(0.0)
            .build();
        let mut tracker = TemplateTracker::new(&seed_frame, Point::new(20.0, 20.0), config).unwrap();
        // Same square shifted right by 2px: should be found near (22,20).
        let moved = frame_with_square(40, 40, 20, 18, 5);
        match tracker.step(&moved, Point::new(20.0, 20.0)) {
            StepOutcome::Found { position, .. } => {
                assert!((position.x - 22.0).abs() <= 1.0, "x was {}", position.x);
            }
            StepOutcome::Miss => panic!("should have found the shifted square"),
        }
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
        let config = TemplateTrackerConfig::builder()
            .update_threshold(0.8)
            .build();
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

    /// The adaptive template still absorbs *partial* appearance change —
    /// change the anchor no longer scores at `min_score` but still
    /// recognizes above `anchor_floor`. This is the legitimate half of the
    /// 3.6 dual-template design that 17.3 preserves. (In this synthetic
    /// walk, anchor score decays 1.0 → 0.63 by t=0.5; the real target, the
    /// rotation-invariant sleeve end, decays far less.)
    #[test]
    fn adaptive_absorbs_partial_appearance_change_above_the_anchor_floor() {
        let (width, height, cx, cy, radius) = (30u32, 30u32, 15i64, 15i64, 3i64);
        let pos = Point::new(15.0, 15.0);
        let seed_frame = frame_with_blended_pattern(width, height, cx, cy, radius, 0.0);
        // Floor low enough to admit the partial walk (anchor >= 0.6 through t=0.5).
        let config = TemplateTrackerConfig::builder()
            .patch_radius(3)
            .search_radius(2)
            .min_score(0.5)
            .update_threshold(0.7)
            .anchor_floor(0.3)
            .build();
        let mut tracker = TemplateTracker::new(&seed_frame, pos, config).unwrap();

        for step in 1..=5 {
            let t = step as f64 / 10.0; // up to t=0.5, anchor still ~0.63
            let frame = frame_with_blended_pattern(width, height, cx, cy, radius, t);
            match tracker.step(&frame, pos) {
                StepOutcome::Found { position, .. } => assert_eq!(position, pos),
                StepOutcome::Miss => panic!("dual-template lost the object at t={t}"),
            }
        }
    }

    /// 17.3, the ratchet-stopper: once appearance walks far enough that the
    /// anchor no longer recognizes the patch at all (score below
    /// `anchor_floor`), the tracker Misses rather than following the
    /// adaptive onto what is, by the anchor's judgement, a different object.
    /// Under the old `max(anchor, adaptive)` combinator this walked all the
    /// way to an anti-correlated pattern (anchor score -0.19) while still
    /// reporting Found — the drift that audit F3 traced onto rack hardware.
    #[test]
    fn veto_stops_tracking_once_appearance_leaves_the_anchor_behind() {
        let (width, height, cx, cy, radius) = (30u32, 30u32, 15i64, 15i64, 3i64);
        let pos = Point::new(15.0, 15.0);
        let seed_frame = frame_with_blended_pattern(width, height, cx, cy, radius, 0.0);
        let config = TemplateTrackerConfig::builder()
            .patch_radius(3)
            .search_radius(2)
            .min_score(0.5)
            .update_threshold(0.7)
            .anchor_floor(0.3)
            .build();
        let mut tracker = TemplateTracker::new(&seed_frame, pos, config).unwrap();

        // Walk the whole way to the anti-correlated pattern_b. Somewhere
        // past the floor (~t=0.65) the veto must engage and stay engaged.
        let mut missed = false;
        for step in 1..=10 {
            let t = step as f64 / 10.0;
            let frame = frame_with_blended_pattern(width, height, cx, cy, radius, t);
            if tracker.step(&frame, pos) == StepOutcome::Miss {
                missed = true;
            }
        }
        assert!(missed, "veto never engaged despite appearance fully leaving the anchor");
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
        let mut tracker = TemplateTracker::new(&seed_frame, pos, config.clone()).unwrap();

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

    // --- Preprocessor integration (task 11.2) ---

    use crate::preprocessor::{Preprocessor, PreprocessorChain};

    #[test]
    fn filtered_tracker_self_matches_the_seed_frame_at_score_near_one() {
        // Same-space invariant (CONTEXT.md "Preprocessor"): if the chain is
        // applied identically to the reference and every candidate, a
        // tracker stepping on its own seed frame must still self-match
        // near-perfectly, exactly as an unfiltered tracker would.
        let width = 30;
        let height = 30;
        let seed_frame = frame_with_square(width, height, 12, 12, 6);
        let seed = Point::new(15.0, 15.0);
        let chain = PreprocessorChain::from_steps(vec![Preprocessor::GaussianBlur { sigma: 1.2 }]);
        let config = TemplateTrackerConfig::builder()
            .patch_radius(4)
            .search_radius(4)
            .min_score(0.5)
            .preprocessor(chain)
            .build();
        let mut tracker = TemplateTracker::new(&seed_frame, seed, config).unwrap();

        let outcome = tracker.step(&seed_frame, seed);
        match outcome {
            StepOutcome::Found { position, score } => {
                assert_eq!(position, seed);
                assert!(
                    score > 0.99,
                    "expected near-perfect self-match, got {score}"
                );
            }
            StepOutcome::Miss => panic!("expected Found: filtered tracker must self-match"),
        }
    }

    /// A simple xorshift-style LCG for reproducible per-pixel noise.
    fn lcg_next(state: &mut u32) -> u32 {
        *state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        *state
    }

    /// Builds a frame with a bright moving square plus independent per-pixel
    /// noise (salt/pepper style: occasional large jumps), seeded
    /// deterministically from `frame_index` so consecutive frames are
    /// independent noise draws over the same moving object.
    fn noisy_frame_with_square(
        width: u32,
        height: u32,
        sx: i64,
        sy: i64,
        size: i64,
        frame_index: u32,
    ) -> Frame {
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        let mut state = 0x9e3779b9u32.wrapping_add(frame_index.wrapping_mul(2654435761));
        for y in 0..height as i64 {
            for x in 0..width as i64 {
                let inside = x >= sx && x < sx + size && y >= sy && y < sy + size;
                let base = if inside { 220i32 } else { 20i32 };
                let r = lcg_next(&mut state);
                // Heavy-tailed impulse noise: most pixels are clean, roughly
                // 1 in 6 gets knocked to a near-random extreme value —
                // enough to routinely flip the ZNCC winner among nearby
                // candidates without a denoising preprocessor.
                let v = if r.is_multiple_of(6) {
                    (r >> 8) % 256
                } else {
                    base.clamp(0, 255) as u32
                };
                let v = v as u8;
                rgb.extend_from_slice(&[v, v, v]);
            }
        }
        Frame::new(width, height, rgb).unwrap()
    }

    #[test]
    fn gaussian_chain_tracks_a_moving_noisy_square_at_least_as_well_as_unfiltered() {
        let width = 50;
        let height = 50;
        let size = 6;
        let seed = Point::new(13.0, 13.0);
        // Moves by (1, 1) every frame, plus per-frame impulse noise.
        let positions: Vec<(i64, i64)> = (0..10).map(|i| (10 + i, 10 + i)).collect();

        let run = |chain: PreprocessorChain| -> (u32, f64) {
            let seed_frame = noisy_frame_with_square(width, height, 10, 10, size, 0);
            let config = TemplateTrackerConfig::builder()
                .patch_radius(4)
                .search_radius(4)
                .min_score(0.0) // count every step so score sums are comparable
                .preprocessor(chain)
                .build();
            let mut tracker = TemplateTracker::new(&seed_frame, seed, config).unwrap();
            let mut last_pos = seed;
            let mut found_count = 0u32;
            let mut score_sum = 0.0;
            for (i, &(sx, sy)) in positions.iter().enumerate() {
                let frame = noisy_frame_with_square(width, height, sx, sy, size, i as u32 + 1);
                match tracker.step(&frame, last_pos) {
                    StepOutcome::Found { position, score } => {
                        found_count += 1;
                        score_sum += score;
                        last_pos = position;
                    }
                    StepOutcome::Miss => {}
                }
            }
            (found_count, score_sum)
        };

        let (_, unfiltered_score_sum) = run(PreprocessorChain::new());
        let (_, filtered_score_sum) = run(PreprocessorChain::from_steps(vec![
            Preprocessor::GaussianBlur { sigma: 1.2 },
        ]));

        assert!(
            filtered_score_sum >= unfiltered_score_sum,
            "expected the gaussian-filtered chain to score at least as well as unfiltered on \
             this noisy moving-square sequence: filtered={filtered_score_sum}, \
             unfiltered={unfiltered_score_sum}"
        );
    }
}
