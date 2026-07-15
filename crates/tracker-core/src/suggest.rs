//! Tracker auto-suggestion (4.3): given a Seed, guess whether the `Color`
//! or `Template` tracker (see CONTEXT.md, "Tracker") is the better fit,
//! without requiring the user to know the difference up front.
//!
//! Heuristic: a physical color Marker only helps if (a) it's actually
//! *saturated* enough to be a deliberately-placed marker rather than an
//! incidental gray/skin/metal tone, and (b) its color is *distinct* from
//! the surrounding scene, so the `ColorTracker`'s per-frame color scan
//! won't be swamped by background pixels that happen to match. Both
//! conditions are checked by learning a `ColorModel` from the seed patch
//! (same patch a `TemplateTracker` would use) and then sampling an annulus
//! (a ring) around it standing in for "the background near the object":
//! close enough to be representative of what a search window will contain,
//! far enough out to exclude the object itself.
//!
//! If either condition fails, `Template` is the fallback — it has no
//! reliance on color at all, so it degrades gracefully for markerless
//! footage (a bare plate/bar end) or cluttered same-hue scenes.

use crate::color::{ColorModel, ColorModelConfig};
use crate::geometry::{Frame, Point};

/// Which tracker (see CONTEXT.md, "Tracker") `suggest_tracker` recommends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackerKind {
    /// Follow the seed's `ColorModel` (a physical Marker was likely placed).
    Color,
    /// Follow the seed patch's appearance via correlation (no distinct
    /// marker color found; safe default).
    Template,
}

/// Configuration for `suggest_tracker`: patch/annulus geometry and the
/// thresholds that decide `Color` vs `Template`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackerSuggestionConfig {
    patch_radius: u32,
    annulus_inner_radius: u32,
    annulus_outer_radius: u32,
    background_match_threshold: f64,
    min_saturation: f64,
    color_config: ColorModelConfig,
}

impl TrackerSuggestionConfig {
    /// Starts a builder with sensible defaults; see `default_config` for
    /// the rationale behind each one.
    pub fn builder() -> TrackerSuggestionConfigBuilder {
        TrackerSuggestionConfigBuilder::default()
    }

    /// Sensible defaults:
    /// - `patch_radius` 5 (matches `TemplateTrackerConfig`'s default patch).
    /// - annulus `10..=15` (2x-3x the patch radius): close enough to be
    ///   representative of a search window's surroundings, far enough out
    ///   to exclude the seed patch itself.
    /// - `background_match_threshold` 0.15: if more than 15% of the
    ///   annulus already matches the seed's color model, that color isn't
    ///   distinct enough to reliably discriminate object from background.
    /// - `min_saturation` 0.25: below this the seed patch reads as
    ///   effectively gray (bare metal/plate/skin), too weak a color signal
    ///   for a `ColorModel` to be worth trusting regardless of how distinct
    ///   the background is.
    pub fn default_config() -> Self {
        Self {
            patch_radius: 5,
            annulus_inner_radius: 10,
            annulus_outer_radius: 15,
            background_match_threshold: 0.15,
            min_saturation: 0.25,
            color_config: ColorModelConfig::default_tolerance(),
        }
    }

    pub fn patch_radius(&self) -> u32 {
        self.patch_radius
    }

    pub fn annulus_inner_radius(&self) -> u32 {
        self.annulus_inner_radius
    }

    pub fn annulus_outer_radius(&self) -> u32 {
        self.annulus_outer_radius
    }

    pub fn background_match_threshold(&self) -> f64 {
        self.background_match_threshold
    }

    pub fn min_saturation(&self) -> f64 {
        self.min_saturation
    }

    pub fn color_config(&self) -> ColorModelConfig {
        self.color_config
    }
}

impl Default for TrackerSuggestionConfig {
    fn default() -> Self {
        Self::default_config()
    }
}

/// Builder for `TrackerSuggestionConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TrackerSuggestionConfigBuilder {
    inner: TrackerSuggestionConfig,
}

impl TrackerSuggestionConfigBuilder {
    pub fn patch_radius(mut self, radius: u32) -> Self {
        self.inner.patch_radius = radius;
        self
    }

    pub fn annulus_inner_radius(mut self, radius: u32) -> Self {
        self.inner.annulus_inner_radius = radius;
        self
    }

    pub fn annulus_outer_radius(mut self, radius: u32) -> Self {
        self.inner.annulus_outer_radius = radius;
        self
    }

    pub fn background_match_threshold(mut self, fraction: f64) -> Self {
        self.inner.background_match_threshold = fraction;
        self
    }

    pub fn min_saturation(mut self, saturation: f64) -> Self {
        self.inner.min_saturation = saturation;
        self
    }

    pub fn color_config(mut self, config: ColorModelConfig) -> Self {
        self.inner.color_config = config;
        self
    }

    pub fn build(self) -> TrackerSuggestionConfig {
        self.inner
    }
}

/// Suggests `Color` or `Template` for a Seed at `seed` in `frame`.
///
/// Learns a `ColorModel` from the seed patch, then samples the annulus
/// between `annulus_inner_radius` and `annulus_outer_radius` (clipped to
/// the frame — works even when the seed is near an edge, sampling
/// whatever of the ring remains on-frame). Recommends `Color` only if the
/// seed patch is saturated enough (`min_saturation`) *and* the fraction of
/// annulus pixels matching the model stays at or below
/// `background_match_threshold`; falls back to `Template` otherwise,
/// including when the seed patch itself is out of bounds or the annulus is
/// entirely off-frame (too little evidence to trust a `Color` call).
pub fn suggest_tracker(frame: &Frame, seed: Point, config: TrackerSuggestionConfig) -> TrackerKind {
    let Ok(model) = ColorModel::learn(frame, seed, config.patch_radius, config.color_config) else {
        return TrackerKind::Template;
    };

    if model.sat() < config.min_saturation {
        return TrackerKind::Template;
    }

    let cx = seed.x.round() as i64;
    let cy = seed.y.round() as i64;
    let inner = config.annulus_inner_radius as i64;
    let outer = config.annulus_outer_radius as i64;

    let mut sampled: u64 = 0;
    let mut matched: u64 = 0;
    for y in (cy - outer)..=(cy + outer) {
        if y < 0 || y >= frame.height() as i64 {
            continue;
        }
        let dy = y - cy;
        for x in (cx - outer)..=(cx + outer) {
            if x < 0 || x >= frame.width() as i64 {
                continue;
            }
            let dx = x - cx;
            if dx.abs() <= inner && dy.abs() <= inner {
                continue; // inside the inner square: too close to the seed patch
            }
            if let Some(rgb) = frame.pixel(x as u32, y as u32) {
                sampled += 1;
                if model.matches(rgb) {
                    matched += 1;
                }
            }
        }
    }

    if sampled == 0 {
        // No annulus pixels available at all (e.g. a tiny frame): not
        // enough evidence the background is distinct.
        return TrackerKind::Template;
    }

    let background_match_fraction = matched as f64 / sampled as f64;
    if background_match_fraction <= config.background_match_threshold {
        TrackerKind::Color
    } else {
        TrackerKind::Template
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a `width`x`height` frame of `bg`, with a `size`x`size` blob of
    /// `fg` centered at `(cx, cy)`.
    fn frame_with_centered_blob(
        width: u32,
        height: u32,
        bg: [u8; 3],
        cx: i64,
        cy: i64,
        size: i64,
        fg: [u8; 3],
    ) -> Frame {
        let half = size / 2;
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        for y in 0..height as i64 {
            for x in 0..width as i64 {
                let inside = (x - cx).abs() <= half && (y - cy).abs() <= half;
                let color = if inside { fg } else { bg };
                rgb.extend_from_slice(&color);
            }
        }
        Frame::new(width, height, rgb).unwrap()
    }

    #[test]
    fn saturated_blob_distinct_from_gray_background_suggests_color() {
        // Saturated green blob on a plain mid-gray background: distinct
        // color, low background match fraction (0) -> Color.
        let frame = frame_with_centered_blob(80, 80, [128, 128, 128], 40, 40, 8, [0, 200, 0]);
        let kind = suggest_tracker(
            &frame,
            Point::new(40.0, 40.0),
            TrackerSuggestionConfig::default(),
        );
        assert_eq!(kind, TrackerKind::Color);
    }

    #[test]
    fn dark_plate_on_dark_cluttered_gray_background_suggests_template() {
        // Dark plate (low-saturation dark gray) on a dark, cluttered
        // gray-on-gray background: the seed patch itself has near-zero
        // saturation, so the min_saturation floor alone routes this to
        // Template regardless of the background.
        let frame = frame_with_centered_blob(80, 80, [40, 40, 40], 40, 40, 8, [70, 70, 70]);
        let kind = suggest_tracker(
            &frame,
            Point::new(40.0, 40.0),
            TrackerSuggestionConfig::default(),
        );
        assert_eq!(kind, TrackerKind::Template);
    }

    #[test]
    fn unsaturated_seed_on_colorful_background_still_suggests_template() {
        // Saturation floor should reject a gray seed even when the
        // background happens to be colorful (and thus would otherwise pass
        // the background-match check trivially).
        let frame = frame_with_centered_blob(80, 80, [0, 200, 0], 40, 40, 8, [128, 128, 128]);
        let kind = suggest_tracker(
            &frame,
            Point::new(40.0, 40.0),
            TrackerSuggestionConfig::default(),
        );
        assert_eq!(kind, TrackerKind::Template);
    }

    #[test]
    fn background_matching_seed_color_suggests_template() {
        // Saturated seed color, but the annulus is filled with pixels that
        // also match the model (a scene cluttered with the same hue) ->
        // background isn't distinct -> Template.
        let mut rgb = vec![0u8; 80 * 80 * 3];
        for y in 0..80i64 {
            for x in 0..80i64 {
                let idx = (y as usize * 80 + x as usize) * 3;
                rgb[idx..idx + 3].copy_from_slice(&[0, 200, 0]);
            }
        }
        let frame = Frame::new(80, 80, rgb).unwrap();
        let kind = suggest_tracker(
            &frame,
            Point::new(40.0, 40.0),
            TrackerSuggestionConfig::default(),
        );
        assert_eq!(kind, TrackerKind::Template);
    }

    #[test]
    fn annulus_clipped_at_frame_edge_still_decides_color() {
        // Seed near the top-left corner: the seed patch itself (radius 5,
        // fully covered by a blob big enough to fill it) still fits, but
        // the annulus (radius up to 15) is clipped by the frame's edges
        // (only part of the ring is on-frame). The remaining on-frame
        // background (gray) still clearly doesn't match the saturated
        // green seed color, so this should still resolve to Color rather
        // than falling back just because of the clipping.
        let frame = frame_with_centered_blob(30, 30, [128, 128, 128], 10, 10, 14, [0, 200, 0]);
        let kind = suggest_tracker(
            &frame,
            Point::new(10.0, 10.0),
            TrackerSuggestionConfig::default(),
        );
        assert_eq!(kind, TrackerKind::Color);
    }

    #[test]
    fn out_of_bounds_seed_patch_suggests_template() {
        let frame = frame_with_centered_blob(80, 80, [128, 128, 128], 40, 40, 8, [0, 200, 0]);
        // Seed right at the corner: patch_radius(5) pushes it out of bounds.
        let kind = suggest_tracker(
            &frame,
            Point::new(0.0, 0.0),
            TrackerSuggestionConfig::default(),
        );
        assert_eq!(kind, TrackerKind::Template);
    }

    #[test]
    fn builder_overrides_thresholds() {
        let config = TrackerSuggestionConfig::builder()
            .patch_radius(3)
            .annulus_inner_radius(6)
            .annulus_outer_radius(9)
            .background_match_threshold(0.5)
            .min_saturation(0.1)
            .build();
        assert_eq!(config.patch_radius(), 3);
        assert_eq!(config.annulus_inner_radius(), 6);
        assert_eq!(config.annulus_outer_radius(), 9);
        assert_eq!(config.background_match_threshold(), 0.5);
        assert_eq!(config.min_saturation(), 0.1);
    }
}
