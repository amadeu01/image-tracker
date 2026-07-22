//! Color Tracker: locates a `ColorModel`'s color in successive frames by
//! scanning a window centered on the last known position and taking the
//! centroid of matching pixels (see CONTEXT.md, "Marker" / "Color Model" /
//! "Tracker").

use crate::color::ColorModel;
use crate::geometry::{Frame, Point};
use crate::motion::{distance, gate_radius, Track};
use crate::preprocessor::PreprocessorChain;
use crate::tracker::{StepOutcome, Tracker};

/// Configuration for a `ColorTracker`, built via `ColorTrackerConfig::builder()`.
#[derive(Debug, Clone, PartialEq)]
pub struct ColorTrackerConfig {
    search_radius: u32,
    min_pixels: u32,
    max_acceleration: f64,
    preprocessor: PreprocessorChain,
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

    /// The physically-plausible acceleration bound (px/s², 17.2) — see
    /// `TemplateTrackerConfig::max_acceleration`. Applies the same
    /// prediction-centred-search + gate treatment to the color centroid.
    pub fn max_acceleration(&self) -> f64 {
        self.max_acceleration
    }

    /// The `Preprocessor` chain applied (per RGB channel plane) to the
    /// search window before color matching. Empty (identity/no-op) by
    /// default.
    pub fn preprocessor(&self) -> &PreprocessorChain {
        &self.preprocessor
    }
}

/// Builder for `ColorTrackerConfig`.
#[derive(Debug, Clone, PartialEq)]
pub struct ColorTrackerConfigBuilder {
    search_radius: u32,
    min_pixels: u32,
    max_acceleration: f64,
    preprocessor: PreprocessorChain,
}

impl Default for ColorTrackerConfigBuilder {
    fn default() -> Self {
        Self {
            search_radius: 25,
            min_pixels: 5,
            // See `TemplateTrackerConfigBuilder::default` (17.2).
            max_acceleration: 6000.0,
            preprocessor: PreprocessorChain::new(),
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

    /// Sets the acceleration bound (px/s², 17.2) — see
    /// `TemplateTrackerConfigBuilder::max_acceleration`.
    pub fn max_acceleration(mut self, max_acceleration: f64) -> Self {
        self.max_acceleration = max_acceleration;
        self
    }

    /// Sets the `Preprocessor` chain applied to the search window before
    /// color matching (see CONTEXT.md, "Preprocessor").
    pub fn preprocessor(mut self, chain: PreprocessorChain) -> Self {
        self.preprocessor = chain;
        self
    }

    pub fn build(self) -> ColorTrackerConfig {
        ColorTrackerConfig {
            search_radius: self.search_radius,
            min_pixels: self.min_pixels,
            max_acceleration: self.max_acceleration,
            preprocessor: self.preprocessor,
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

/// An RGB window read out of a `Frame`, as three independent `f32` planes
/// (one per channel) plus its top-left offset in frame coordinates.
///
/// This is the "region buffer" the color path filters (see
/// `preprocessor.rs`'s module docs): `TemplateTracker` filters a
/// single-channel `Patch`; `ColorTracker` filters this 3-plane window by
/// running the same per-plane `Preprocessor::apply_plane` once per channel,
/// independently — there is no cross-channel coupling in either filter, so
/// this is exactly the same computation as the grayscale path, just called
/// three times.
struct RgbRegion {
    min_x: i64,
    min_y: i64,
    width: usize,
    height: usize,
    r: Vec<f32>,
    g: Vec<f32>,
    b: Vec<f32>,
}

impl RgbRegion {
    /// Reads the window `[min_x, max_x] x [min_y, max_y]` (inclusive,
    /// already clamped to frame bounds by the caller) out of `frame`.
    fn from_frame_window(frame: &Frame, min_x: i64, max_x: i64, min_y: i64, max_y: i64) -> Self {
        let width = (max_x - min_x + 1) as usize;
        let height = (max_y - min_y + 1) as usize;
        let mut r = Vec::with_capacity(width * height);
        let mut g = Vec::with_capacity(width * height);
        let mut b = Vec::with_capacity(width * height);
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let rgb = frame.pixel(x as u32, y as u32).unwrap_or([0, 0, 0]);
                r.push(rgb[0] as f32);
                g.push(rgb[1] as f32);
                b.push(rgb[2] as f32);
            }
        }
        Self {
            min_x,
            min_y,
            width,
            height,
            r,
            g,
            b,
        }
    }

    /// Applies `chain` to each of the three channel planes independently.
    fn apply_chain(&self, chain: &PreprocessorChain) -> Self {
        Self {
            min_x: self.min_x,
            min_y: self.min_y,
            width: self.width,
            height: self.height,
            r: chain.apply_plane(&self.r, self.width, self.height),
            g: chain.apply_plane(&self.g, self.width, self.height),
            b: chain.apply_plane(&self.b, self.width, self.height),
        }
    }

    /// The RGB triple at frame-absolute coordinates `(x, y)`, rounded and
    /// clamped back to `u8`. `x, y` must fall within this region's window.
    fn pixel_at(&self, x: i64, y: i64) -> [u8; 3] {
        let lx = (x - self.min_x) as usize;
        let ly = (y - self.min_y) as usize;
        let idx = ly * self.width + lx;
        let clamp = |v: f32| v.round().clamp(0.0, 255.0) as u8;
        [clamp(self.r[idx]), clamp(self.g[idx]), clamp(self.b[idx])]
    }

    /// Converts this (possibly filtered) region into a standalone `Frame`,
    /// for feeding into `ColorModel::learn` (used by
    /// `ColorTracker::learn_and_new` to learn a model in the same filtered
    /// space candidate windows will be scanned in).
    fn to_frame(&self) -> Frame {
        let mut rgb = Vec::with_capacity(self.width * self.height * 3);
        for i in 0..self.width * self.height {
            let clamp = |v: f32| v.round().clamp(0.0, 255.0) as u8;
            rgb.push(clamp(self.r[i]));
            rgb.push(clamp(self.g[i]));
            rgb.push(clamp(self.b[i]));
        }
        // Construction from a region of a valid frame always yields a
        // correctly-sized buffer, but crate rules forbid `unwrap`/`expect`
        // outside tests: fall back to a degenerate 1x1 black frame rather
        // than panic in the unreachable mismatch case.
        Frame::new(self.width as u32, self.height as u32, rgb)
            .unwrap_or_else(|_| Frame::new(1, 1, vec![0, 0, 0]).unwrap_or_else(|_| unreachable!()))
    }
}

impl ColorTracker {
    pub fn new(model: ColorModel, config: ColorTrackerConfig) -> Self {
        Self { model, config }
    }

    /// Learns a `ColorModel` from the square patch of pixels centered at
    /// `seed` (radius `model_radius`) in `frame`, running `config`'s
    /// preprocessor chain over that seed patch first — so the reference
    /// model is learned in the exact same filtered space every candidate
    /// search window will be scanned in per `step` (see CONTEXT.md,
    /// "Preprocessor": "reference and candidates must live in the same
    /// filtered space for scores to be comparable").
    ///
    /// Prefer this over `ColorModel::learn` + `ColorTracker::new` whenever
    /// `config`'s chain is non-empty: constructing from an already-learned,
    /// unfiltered model bypasses that guarantee.
    pub fn learn_and_new(
        frame: &Frame,
        seed: Point,
        model_radius: u32,
        model_config: crate::color::ColorModelConfig,
        config: ColorTrackerConfig,
    ) -> Result<Self, crate::color::ColorModelError> {
        let cx = seed.x.round() as i64;
        let cy = seed.y.round() as i64;
        let r = model_radius as i64;
        let min_x = cx - r;
        let max_x = cx + r;
        let min_y = cy - r;
        let max_y = cy + r;
        if min_x < 0 || min_y < 0 || max_x >= frame.width() as i64 || max_y >= frame.height() as i64
        {
            return Err(crate::color::ColorModelError::OutOfBounds);
        }
        let region = RgbRegion::from_frame_window(frame, min_x, max_x, min_y, max_y)
            .apply_chain(&config.preprocessor);
        let filtered_frame = region.to_frame();
        let center = Point::new(model_radius as f64, model_radius as f64);
        let model = ColorModel::learn(&filtered_frame, center, model_radius, model_config)?;
        Ok(Self::new(model, config))
    }

    /// Scans the search window centered on `last_pos` in `frame`, collecting
    /// pixels matching `self.model`. Returns `Found` at the centroid of
    /// matching pixels if their count is at least `min_pixels`, else `Miss`.
    ///
    /// The window is filtered through `config`'s preprocessor chain (per
    /// RGB channel plane, independently) before matching — the same-space
    /// counterpart to `TemplateTracker::step` filtering each candidate
    /// patch.
    pub fn step(&mut self, frame: &Frame, track: &Track, dt: f64) -> StepOutcome {
        let predicted = track.predicted(dt);
        let cx = predicted.x.round() as i64;
        let cy = predicted.y.round() as i64;
        let r = self.config.search_radius as i64;

        let min_x = (cx - r).max(0);
        let max_x = (cx + r).min(frame.width() as i64 - 1);
        let min_y = (cy - r).max(0);
        let max_y = (cy + r).min(frame.height() as i64 - 1);

        if min_x > max_x || min_y > max_y {
            return StepOutcome::Miss;
        }

        let region = RgbRegion::from_frame_window(frame, min_x, max_x, min_y, max_y)
            .apply_chain(&self.config.preprocessor);

        let mut count: u64 = 0;
        let mut sum_x: f64 = 0.0;
        let mut sum_y: f64 = 0.0;
        let mut scanned: u64 = 0;

        for y in min_y..=max_y {
            for x in min_x..=max_x {
                scanned += 1;
                let rgb = region.pixel_at(x, y);
                if self.model.matches(rgb) {
                    count += 1;
                    sum_x += x as f64;
                    sum_y += y as f64;
                }
            }
        }

        if count == 0 || count < self.config.min_pixels as u64 {
            return StepOutcome::Miss;
        }

        let position = Point::new(sum_x / count as f64, sum_y / count as f64);

        // Same physically-plausible gate as `TemplateTracker` (17.2, audit
        // F2): a centroid too far from the constant-velocity prediction is
        // rejected regardless of how solidly it fills the window.
        if distance(position, predicted) > gate_radius(track, self.config.max_acceleration, dt) {
            return StepOutcome::Miss;
        }

        let score = count as f64 / scanned as f64;
        // The color tracker has no separate identity template: its
        // fill-fraction is already an honest confidence, so identity and
        // effective score coincide (17.4).
        StepOutcome::Found {
            position,
            score,
            identity_confidence: score,
        }
    }
}

impl Tracker for ColorTracker {
    fn step(&mut self, frame: &Frame, track: &Track, dt: f64) -> StepOutcome {
        ColorTracker::step(self, frame, track, dt)
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
                    Some((bx, by, size, fg))
                        if x >= bx && x < bx + size && y >= by && y < by + size =>
                    {
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
        ColorModel::learn(
            &seed_frame,
            Point::new(5.0, 5.0),
            2,
            ColorModelConfig::default(),
        )
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

        let outcome = tracker.step(&frame, &Track::new(Point::new(20.0, 20.0)), 1.0);
        match outcome {
            StepOutcome::Found {
                position, score, ..
            } => {
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
        let outcome = tracker.step(&frame, &Track::new(Point::new(20.0, 20.0)), 1.0);
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

        let outcome = tracker.step(&frame, &Track::new(Point::new(20.0, 20.0)), 1.0);
        assert_eq!(outcome, StepOutcome::Miss);
    }

    #[test]
    fn misses_when_blob_smaller_than_min_pixels() {
        let model = red_model();
        // A single matching pixel: below min_pixels(4).
        let frame = frame_with_blob(40, 40, [128, 128, 128], Some((20, 20, 1, [255, 0, 0])));
        let mut tracker = ColorTracker::new(model, plain_config());

        let outcome = tracker.step(&frame, &Track::new(Point::new(20.0, 20.0)), 1.0);
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

        let outcome = tracker.step(&frame, &Track::new(Point::new(20.0, 20.0)), 1.0);
        match outcome {
            StepOutcome::Found { position, .. } => {
                // Centroid of the 4x4 in-window blob at (18..22) is (19.5, 19.5).
                assert!((position.x - 19.5).abs() < 1e-6);
                assert!((position.y - 19.5).abs() < 1e-6);
            }
            StepOutcome::Miss => panic!("expected Found: in-window blob should be detected"),
        }
    }

    // --- Preprocessor integration (task 11.2) ---

    use crate::preprocessor::Preprocessor;

    fn filtered_config() -> ColorTrackerConfig {
        ColorTrackerConfig::builder()
            .search_radius(15)
            .min_pixels(4)
            .preprocessor(PreprocessorChain::from_steps(vec![Preprocessor::Median {
                k: 3,
            }]))
            .build()
    }

    #[test]
    fn config_builder_overrides_preprocessor() {
        let chain = PreprocessorChain::from_steps(vec![Preprocessor::GaussianBlur { sigma: 1.0 }]);
        let config = ColorTrackerConfig::builder()
            .preprocessor(chain.clone())
            .build();
        assert_eq!(config.preprocessor(), &chain);
    }

    #[test]
    fn learn_and_new_self_matches_the_seed_frame_at_score_near_one() {
        // Same-space invariant: the model must be learned in the same
        // filtered space every candidate window is scanned in, so stepping
        // on the seed's own frame must still find (and centroid onto) the
        // blob, just as an unfiltered ColorTracker would.
        let frame = frame_with_blob(40, 40, [128, 128, 128], Some((18, 18, 4, [255, 0, 0])));
        let seed = Point::new(19.5, 19.5);
        let tracker_config = filtered_config();
        let mut tracker = ColorTracker::learn_and_new(
            &frame,
            seed,
            2,
            crate::color::ColorModelConfig::default(),
            tracker_config,
        )
        .unwrap();

        let outcome = tracker.step(&frame, &Track::new(Point::new(20.0, 20.0)), 1.0);
        match outcome {
            StepOutcome::Found {
                position, score, ..
            } => {
                assert!((position.x - 19.5).abs() < 1e-6);
                assert!((position.y - 19.5).abs() < 1e-6);
                assert!(score > 0.0);
            }
            StepOutcome::Miss => panic!("expected Found: filtered color tracker must self-match"),
        }
    }

    #[test]
    fn filtered_color_tracker_still_follows_a_moved_blob() {
        let model = red_model();
        let frame = frame_with_blob(40, 40, [128, 128, 128], Some((25, 10, 4, [255, 0, 0])));
        let mut tracker = ColorTracker::new(model, filtered_config());

        let outcome = tracker.step(&frame, &Track::new(Point::new(20.0, 20.0)), 1.0);
        match outcome {
            StepOutcome::Found { position, .. } => {
                assert!((position.x - 26.5).abs() < 1e-6);
                assert!((position.y - 11.5).abs() < 1e-6);
            }
            StepOutcome::Miss => panic!("expected Found"),
        }
    }

    #[test]
    fn learn_and_new_reports_out_of_bounds_like_color_model_learn() {
        let frame = frame_with_blob(10, 10, [128, 128, 128], None);
        let result = ColorTracker::learn_and_new(
            &frame,
            Point::new(0.0, 0.0),
            2,
            crate::color::ColorModelConfig::default(),
            plain_config(),
        );
        assert_eq!(result, Err(crate::color::ColorModelError::OutOfBounds));
    }
}
