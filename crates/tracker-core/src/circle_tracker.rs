//! Circle Tracker (17.5, audit F6): fits the plate rim as a circle within a
//! search window, using a gradient-direction Hough-style accumulator instead
//! of appearance matching.
//!
//! Motivation (audit F6): a competition plate is a smooth, specular,
//! rotation-symmetric disc. Every appearance-based seed position we tried
//! forces a tradeoff — seeded off-axis, the tracker chases a 45mm plate-spin
//! artifact; seeded on-axis (rotation invariant), the chrome hub is too
//! smooth to out-score adjacent rack hardware on ZNCC or colour. No seed
//! position is both rotation-invariant *and* discriminative under either
//! metric, because neither metric uses the one fact about the object that
//! *is* both: it is a circle of a knowable diameter. This tracker fits that
//! circle directly instead of matching a patch or a colour.
//!
//! # Algorithm
//!
//! A classic gradient-direction Hough circle transform, done from scratch
//! (no image-processing crate): for every pixel in the search window whose
//! Sobel gradient magnitude clears `edge_threshold`, its gradient direction
//! is the local surface normal of whatever edge it sits on. If that edge is
//! part of a circle of radius `r` centred at `c`, then `c` lies exactly `r`
//! pixels from the edge pixel along that gradient direction — in *either*
//! sign, since we don't know a priori whether the rim is a light-on-dark or
//! dark-on-light edge. Every edge pixel casts two votes (one each direction)
//! into a `(cx, cy, r)` accumulator for every candcandidate radius in
//! `min_radius..=max_radius`; the accumulator's peak is the fitted circle.
//! This makes the circle's own curvature the signal, not any particular
//! surface pattern — a straight edge (a rack uprights, a barbell shaft, a
//! rotated texture line on the plate face) never accumulates at a single
//! `(cx, cy)` the way a genuine rim does, so this degrades gracefully in
//! front of exactly the clutter that defeats ZNCC/colour (audit F6).
//!
//! # Radius as the honest lost-signal
//!
//! A near-constant fitted radius across frames is the healthy signal for
//! "still the same plate"; a sudden radius jump is treated as a loss (a
//! `Miss`) even if the accumulator peak is otherwise well-supported, because
//! it means the fit likely locked onto a different circular (or
//! circular-ish) feature nearby — see `max_radius_jump_fraction`.
//! `StepOutcome` has no separate radius field (it's shared with
//! `TemplateTracker`/`ColorTracker`), so this tracker folds radius
//! stability into `identity_confidence` instead: the fraction of edge votes
//! agreeing with the winning `(cx, cy, r)` bin, gated additionally by radius
//! continuity from the last accepted fit.

use crate::geometry::{Frame, Point};
use crate::motion::{distance, gate_radius, Track};
use crate::tracker::{StepOutcome, Tracker};
use std::collections::HashMap;

/// Configuration for a `CircleTracker`, built via `CircleTrackerConfig::builder()`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CircleTrackerConfig {
    search_radius: u32,
    min_radius: u32,
    max_radius: u32,
    radius_step: u32,
    edge_threshold: f64,
    min_edge_support: f64,
    max_radius_jump_fraction: f64,
    max_velocity: f64,
}

impl CircleTrackerConfig {
    pub fn builder() -> CircleTrackerConfigBuilder {
        CircleTrackerConfigBuilder::default()
    }

    /// Half-width of the square search window around the constant-velocity
    /// prediction.
    pub fn search_radius(&self) -> u32 {
        self.search_radius
    }

    /// Smallest circle radius (px) the accumulator searches.
    pub fn min_radius(&self) -> u32 {
        self.min_radius
    }

    /// Largest circle radius (px) the accumulator searches.
    pub fn max_radius(&self) -> u32 {
        self.max_radius
    }

    /// Step (px) between candidate radii searched.
    pub fn radius_step(&self) -> u32 {
        self.radius_step.max(1)
    }

    /// Minimum Sobel gradient magnitude for a pixel to be treated as an edge
    /// and cast a vote.
    pub fn edge_threshold(&self) -> f64 {
        self.edge_threshold
    }

    /// Minimum fraction of edge votes the winning `(cx, cy, r)` bin must
    /// carry (out of two votes per edge pixel) for a fit to count as
    /// `Found` rather than `Miss` — the "no strong circular edge in the
    /// window" case.
    pub fn min_edge_support(&self) -> f64 {
        self.min_edge_support
    }

    /// Largest fractional change in fitted radius from the last *accepted*
    /// fit that is still treated as the same object. A near-constant radius
    /// is the healthy signal (module docs); a jump past this fraction is
    /// the real lost signal and is rejected as a `Miss` even if the
    /// accumulator peak clears `min_edge_support`.
    pub fn max_radius_jump_fraction(&self) -> f64 {
        self.max_radius_jump_fraction
    }

    /// The velocity reachability bound (px/s, 17.2) — see
    /// `TemplateTrackerConfig::max_velocity`.
    pub fn max_velocity(&self) -> f64 {
        self.max_velocity
    }
}

/// Builder for `CircleTrackerConfig`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CircleTrackerConfigBuilder {
    search_radius: u32,
    min_radius: u32,
    max_radius: u32,
    radius_step: u32,
    edge_threshold: f64,
    min_edge_support: f64,
    max_radius_jump_fraction: f64,
    max_velocity: f64,
}

impl Default for CircleTrackerConfigBuilder {
    fn default() -> Self {
        Self {
            search_radius: 50,
            min_radius: 40,
            max_radius: 90,
            radius_step: 2,
            edge_threshold: 35.0,
            min_edge_support: 0.0008,
            max_radius_jump_fraction: 0.6,
            // See `TemplateTrackerConfigBuilder::default` (17.2).
            max_velocity: 3000.0,
        }
    }
}

impl CircleTrackerConfigBuilder {
    pub fn search_radius(mut self, radius: u32) -> Self {
        self.search_radius = radius;
        self
    }

    pub fn min_radius(mut self, radius: u32) -> Self {
        self.min_radius = radius;
        self
    }

    pub fn max_radius(mut self, radius: u32) -> Self {
        self.max_radius = radius;
        self
    }

    pub fn radius_step(mut self, step: u32) -> Self {
        self.radius_step = step;
        self
    }

    pub fn edge_threshold(mut self, threshold: f64) -> Self {
        self.edge_threshold = threshold;
        self
    }

    pub fn min_edge_support(mut self, fraction: f64) -> Self {
        self.min_edge_support = fraction;
        self
    }

    pub fn max_radius_jump_fraction(mut self, fraction: f64) -> Self {
        self.max_radius_jump_fraction = fraction;
        self
    }

    pub fn max_velocity(mut self, max_velocity: f64) -> Self {
        self.max_velocity = max_velocity;
        self
    }

    pub fn build(self) -> CircleTrackerConfig {
        CircleTrackerConfig {
            search_radius: self.search_radius,
            min_radius: self.min_radius,
            max_radius: self.max_radius,
            radius_step: self.radius_step,
            edge_threshold: self.edge_threshold,
            min_edge_support: self.min_edge_support,
            max_radius_jump_fraction: self.max_radius_jump_fraction,
            max_velocity: self.max_velocity,
        }
    }
}

/// Errors constructing a `CircleTracker`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CircleTrackerError {
    /// No circle with edge support at or above `min_edge_support` was found
    /// in the seed window.
    NoCircleAtSeed,
}

/// The result of one gradient-Hough circle fit: the fitted centre, radius,
/// and the fraction of edge votes agreeing with that `(centre, radius)` bin
/// (0.0-1.0, this fit's honest confidence).
#[derive(Debug, Clone, Copy, PartialEq)]
struct CircleFit {
    center: Point,
    radius: f64,
    support: f64,
}

/// Luma (ITU-R BT.601) of an RGB triple.
fn luma(rgb: [u8; 3]) -> f64 {
    0.299 * rgb[0] as f64 + 0.587 * rgb[1] as f64 + 0.114 * rgb[2] as f64
}

/// Runs the gradient-direction Hough circle fit over the square window
/// centred at `(cx, cy)` (radius `config.search_radius`, clamped to the
/// frame with a 1px margin for the Sobel kernel) in `frame`. Returns `None`
/// if the window is degenerate (too close to the frame edge to have any
/// interior pixels) or no edge pixel clears `edge_threshold`.
fn fit_circle(frame: &Frame, cx: i64, cy: i64, config: &CircleTrackerConfig) -> Option<CircleFit> {
    let r = config.search_radius as i64;
    let w = frame.width() as i64;
    let h = frame.height() as i64;
    let min_x = (cx - r).max(1);
    let max_x = (cx + r).min(w - 2);
    let min_y = (cy - r).max(1);
    let max_y = (cy + r).min(h - 2);
    if min_x > max_x || min_y > max_y {
        return None;
    }

    let get_luma =
        |x: i64, y: i64| -> f64 { frame.pixel(x as u32, y as u32).map(luma).unwrap_or(0.0) };

    let min_r = config.min_radius as i64;
    let max_r = config.max_radius as i64;
    let step = config.radius_step() as i64;
    if min_r > max_r {
        return None;
    }
    let radii: Vec<i64> = (min_r..=max_r).step_by(step as usize).collect();

    let mut accumulator: HashMap<(i64, i64, i64), u32> = HashMap::new();
    let mut edge_count: u64 = 0;

    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let gx = (get_luma(x + 1, y - 1) + 2.0 * get_luma(x + 1, y) + get_luma(x + 1, y + 1))
                - (get_luma(x - 1, y - 1) + 2.0 * get_luma(x - 1, y) + get_luma(x - 1, y + 1));
            let gy = (get_luma(x - 1, y + 1) + 2.0 * get_luma(x, y + 1) + get_luma(x + 1, y + 1))
                - (get_luma(x - 1, y - 1) + 2.0 * get_luma(x, y - 1) + get_luma(x + 1, y - 1));
            let magnitude = (gx * gx + gy * gy).sqrt();
            if magnitude < config.edge_threshold {
                continue;
            }
            edge_count += 1;
            let ux = gx / magnitude;
            let uy = gy / magnitude;
            for &rad in &radii {
                let rad_f = rad as f64;
                let c1 = (
                    (x as f64 - rad_f * ux).round() as i64,
                    (y as f64 - rad_f * uy).round() as i64,
                );
                let c2 = (
                    (x as f64 + rad_f * ux).round() as i64,
                    (y as f64 + rad_f * uy).round() as i64,
                );
                *accumulator.entry((c1.0, c1.1, rad)).or_insert(0) += 1;
                *accumulator.entry((c2.0, c2.1, rad)).or_insert(0) += 1;
            }
        }
    }

    if edge_count == 0 {
        return None;
    }

    let best = accumulator.into_iter().max_by_key(|&(_, votes)| votes)?;
    let ((bx, by, br), votes) = best;
    let support = votes as f64 / (2.0 * edge_count as f64);

    Some(CircleFit {
        center: Point::new(bx as f64, by as f64),
        radius: br as f64,
        support: support.min(1.0),
    })
}

/// Tracks a plate/circle of unknown-but-stable radius across successive
/// frames by fitting a gradient-Hough circle within a window centred on the
/// constant-velocity prediction (17.2). See module docs.
#[derive(Debug, Clone, PartialEq)]
pub struct CircleTracker {
    config: CircleTrackerConfig,
    /// The radius of the last *accepted* fit — the reference `max_radius_jump_fraction`
    /// compares against (module docs, "radius as the honest lost-signal").
    /// `None` before the first accepted fit, so the very first observation
    /// (at the seed) is never rejected on radius-continuity grounds.
    last_radius: Option<f64>,
}

impl CircleTracker {
    /// Fits a circle at `seed` in `frame` to confirm a plate-shaped edge is
    /// actually there, and seeds `last_radius` from it. Fails with
    /// `NoCircleAtSeed` if the fit's edge support doesn't clear
    /// `min_edge_support` — mirrors `TemplateTracker::new`'s
    /// `SeedPatchOutOfBounds`/`ColorTracker`'s out-of-bounds check: a
    /// tracker that can't establish anything at the seed shouldn't silently
    /// start `Miss`-ing forever.
    pub fn new(
        frame: &Frame,
        seed: Point,
        config: CircleTrackerConfig,
    ) -> Result<Self, CircleTrackerError> {
        let cx = seed.x.round() as i64;
        let cy = seed.y.round() as i64;
        let fit =
            fit_circle(frame, cx, cy, &config).filter(|f| f.support >= config.min_edge_support);
        let Some(fit) = fit else {
            return Err(CircleTrackerError::NoCircleAtSeed);
        };
        Ok(Self {
            config,
            last_radius: Some(fit.radius),
        })
    }

    /// Fits a circle within the window centred on `track.predicted(dt)`.
    /// Rejects (as `Miss`) a fit with insufficient edge support, a centre
    /// outside `motion::gate_radius` of `track.position`, or a radius that
    /// jumped more than `max_radius_jump_fraction` from the last accepted
    /// fit — the radius-stability lost-signal (module docs).
    pub fn step(&mut self, frame: &Frame, track: &Track, dt: f64) -> StepOutcome {
        let predicted = track.predicted(dt);
        let cx = predicted.x.round() as i64;
        let cy = predicted.y.round() as i64;

        let Some(fit) = fit_circle(frame, cx, cy, &self.config) else {
            return StepOutcome::Miss;
        };

        if fit.support < self.config.min_edge_support {
            return StepOutcome::Miss;
        }

        if distance(fit.center, track.position) > gate_radius(track, self.config.max_velocity, dt) {
            return StepOutcome::Miss;
        }

        if let Some(last_radius) = self.last_radius {
            if last_radius > 0.0 {
                let jump = (fit.radius - last_radius).abs() / last_radius;
                if jump > self.config.max_radius_jump_fraction {
                    return StepOutcome::Miss;
                }
            }
        }

        self.last_radius = Some(fit.radius);
        StepOutcome::Found {
            position: fit.center,
            score: fit.support,
            identity_confidence: fit.support,
        }
    }
}

impl Tracker for CircleTracker {
    fn step(&mut self, frame: &Frame, track: &Track, dt: f64) -> StepOutcome {
        CircleTracker::step(self, frame, track, dt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Draws a `width`x`height` dark frame with a bright ring (rim) of
    /// `thickness` centred at `(cx, cy)` with outer radius `radius`.
    fn frame_with_ring(
        width: u32,
        height: u32,
        cx: i64,
        cy: i64,
        radius: i64,
        thickness: i64,
        occlude_from_deg: Option<(f64, f64)>,
    ) -> Frame {
        let mut rgb = vec![15u8; (width * height * 3) as usize];
        for y in 0..height as i64 {
            for x in 0..width as i64 {
                let dx = x - cx;
                let dy = y - cy;
                let d = ((dx * dx + dy * dy) as f64).sqrt();
                if (d - radius as f64).abs() <= thickness as f64 / 2.0 {
                    if let Some((from, to)) = occlude_from_deg {
                        let mut deg = (dy as f64).atan2(dx as f64).to_degrees();
                        if deg < 0.0 {
                            deg += 360.0;
                        }
                        if deg >= from && deg < to {
                            continue;
                        }
                    }
                    let idx = (y as usize * width as usize + x as usize) * 3;
                    rgb[idx] = 230;
                    rgb[idx + 1] = 230;
                    rgb[idx + 2] = 230;
                }
            }
        }
        Frame::new(width, height, rgb).unwrap()
    }

    fn test_config() -> CircleTrackerConfig {
        CircleTrackerConfig::builder()
            .search_radius(35)
            .min_radius(15)
            .max_radius(30)
            .radius_step(1)
            .edge_threshold(20.0)
            .min_edge_support(0.008)
            .build()
    }

    #[test]
    fn config_builder_has_defaults() {
        let config = CircleTrackerConfig::builder().build();
        assert_eq!(config.search_radius(), 50);
        assert_eq!(config.min_radius(), 40);
        assert_eq!(config.max_radius(), 90);
        assert_eq!(config.max_velocity(), 3000.0);
    }

    #[test]
    fn fit_recovers_center_and_radius_of_a_clean_ring() {
        let (w, h, cx, cy, radius) = (100u32, 100u32, 50i64, 50i64, 22i64);
        let frame = frame_with_ring(w, h, cx, cy, radius, 3, None);
        let fit = fit_circle(&frame, cx, cy, &test_config()).expect("expected a fit");
        assert!(
            (fit.center.x - cx as f64).abs() <= 1.0,
            "x off by {}",
            (fit.center.x - cx as f64).abs()
        );
        assert!(
            (fit.center.y - cy as f64).abs() <= 1.0,
            "y off by {}",
            (fit.center.y - cy as f64).abs()
        );
        assert!(
            (fit.radius - radius as f64).abs() <= 2.0,
            "radius off: got {}, want {}",
            fit.radius,
            radius
        );
    }

    #[test]
    fn new_and_step_find_a_stationary_ring() {
        let (w, h, cx, cy, radius) = (100u32, 100u32, 50i64, 50i64, 22i64);
        let frame = frame_with_ring(w, h, cx, cy, radius, 3, None);
        let seed = Point::new(cx as f64, cy as f64);
        let mut tracker = CircleTracker::new(&frame, seed, test_config()).unwrap();
        match tracker.step(&frame, &Track::new(seed), 1.0) {
            StepOutcome::Found {
                position,
                identity_confidence,
                ..
            } => {
                assert!((position.x - cx as f64).abs() <= 1.0);
                assert!((position.y - cy as f64).abs() <= 1.0);
                assert!(identity_confidence > 0.0);
            }
            StepOutcome::Miss => panic!("expected Found on a clean stationary ring"),
        }
    }

    #[test]
    fn partially_occluded_ring_still_fits_with_lower_confidence() {
        let (w, h, cx, cy, radius) = (100u32, 100u32, 50i64, 50i64, 22i64);
        let full = frame_with_ring(w, h, cx, cy, radius, 3, None);
        // Erase a 120-degree arc of the ring.
        let occluded = frame_with_ring(w, h, cx, cy, radius, 3, Some((0.0, 120.0)));

        let full_fit = fit_circle(&full, cx, cy, &test_config()).expect("full ring fits");
        let occ_fit =
            fit_circle(&occluded, cx, cy, &test_config()).expect("partial ring still fits");

        assert!(
            (occ_fit.center.x - cx as f64).abs() <= 2.0,
            "occluded fit center drifted: {}",
            occ_fit.center.x
        );
        assert!(
            occ_fit.support < full_fit.support,
            "occluded support ({}) should be lower than full ({})",
            occ_fit.support,
            full_fit.support
        );
    }

    #[test]
    fn no_circle_in_frame_is_a_miss() {
        let (w, h) = (100u32, 100u32);
        let blank = Frame::new(w, h, vec![15u8; (w * h * 3) as usize]).unwrap();
        // No seed circle available either -- construction itself must fail.
        let seed = Point::new(50.0, 50.0);
        assert_eq!(
            CircleTracker::new(&blank, seed, test_config()),
            Err(CircleTrackerError::NoCircleAtSeed)
        );
    }

    #[test]
    fn step_misses_when_the_ring_disappears() {
        let (w, h, cx, cy, radius) = (100u32, 100u32, 50i64, 50i64, 22i64);
        let frame = frame_with_ring(w, h, cx, cy, radius, 3, None);
        let seed = Point::new(cx as f64, cy as f64);
        let mut tracker = CircleTracker::new(&frame, seed, test_config()).unwrap();

        let blank = Frame::new(w, h, vec![15u8; (w * h * 3) as usize]).unwrap();
        assert_eq!(
            tracker.step(&blank, &Track::new(seed), 1.0),
            StepOutcome::Miss
        );
    }

    /// Rotation invariance (the central claim, module docs): a straight
    /// internal marker line rotating inside the disc must not move the
    /// fitted centre, since a straight edge's gradient votes don't
    /// accumulate at a single `(cx, cy, r)` the way the rim's curvature
    /// does.
    #[test]
    fn rotating_internal_marker_does_not_move_the_fitted_center() {
        let (w, h, cx, cy, radius) = (100u32, 100u32, 50i64, 50i64, 22i64);

        let with_marker_line = |angle_deg: f64| -> Frame {
            let mut frame = frame_with_ring(w, h, cx, cy, radius, 3, None);
            let angle = angle_deg.to_radians();
            for t in 0..radius {
                let x = cx + (t as f64 * angle.cos()).round() as i64;
                let y = cy + (t as f64 * angle.sin()).round() as i64;
                if x >= 0 && y >= 0 && (x as u32) < w && (y as u32) < h {
                    frame.set_pixel(x, y, [230, 230, 230]);
                }
            }
            frame
        };

        let a = with_marker_line(15.0);
        let b = with_marker_line(160.0);

        let fit_a = fit_circle(&a, cx, cy, &test_config()).expect("fit a");
        let fit_b = fit_circle(&b, cx, cy, &test_config()).expect("fit b");

        assert!(
            distance(fit_a.center, fit_b.center) <= 1.5,
            "rotation moved the fitted center: {:?} vs {:?}",
            fit_a.center,
            fit_b.center
        );
    }

    #[test]
    fn gate_rejects_an_implausible_jump() {
        let (w, h, cx, cy, radius) = (200u32, 200u32, 50i64, 50i64, 20i64);
        let seed_frame = frame_with_ring(w, h, cx, cy, radius, 3, None);
        let seed = Point::new(cx as f64, cy as f64);
        let config = CircleTrackerConfig::builder()
            .search_radius(120)
            .min_radius(15)
            .max_radius(25)
            .radius_step(1)
            .edge_threshold(20.0)
            .min_edge_support(0.008)
            .max_velocity(1.0) // gate radius ~0 at dt=1.0
            .build();
        let mut tracker = CircleTracker::new(&seed_frame, seed, config).unwrap();

        // A second, identical ring far away -- within the (wide) search
        // window but well outside the tight velocity gate.
        let mut rgb = seed_frame.rgb().to_vec();
        let far_frame = {
            let far = frame_with_ring(w, h, cx + 100, cy, radius, 3, None);
            rgb.copy_from_slice(far.rgb());
            Frame::new(w, h, rgb).unwrap()
        };

        assert_eq!(
            tracker.step(&far_frame, &Track::new(seed), 1.0),
            StepOutcome::Miss,
            "an implausible jump must be gated out"
        );
    }

    #[test]
    fn sudden_radius_change_is_treated_as_lost_even_with_good_edge_support() {
        let (w, h, cx, cy) = (150u32, 150u32, 75i64, 75i64);
        let seed_frame = frame_with_ring(w, h, cx, cy, 20, 3, None);
        let seed = Point::new(cx as f64, cy as f64);
        let config = CircleTrackerConfig::builder()
            .search_radius(60)
            .min_radius(10)
            .max_radius(60)
            .radius_step(1)
            .edge_threshold(20.0)
            .min_edge_support(0.008)
            .max_velocity(10_000.0)
            .max_radius_jump_fraction(0.2)
            .build();
        let mut tracker = CircleTracker::new(&seed_frame, seed, config).unwrap();

        // Same center, but a much bigger ring (radius 45 vs 20): a strong
        // circular edge, but not plausibly the same plate one frame later.
        let bigger_ring = frame_with_ring(w, h, cx, cy, 45, 3, None);
        assert_eq!(
            tracker.step(&bigger_ring, &Track::new(seed), 1.0),
            StepOutcome::Miss,
            "a sudden radius jump must be rejected even with strong edge support"
        );
    }
}
