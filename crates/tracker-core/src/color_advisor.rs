//! Marker Color Advisor (milestone 6, CONTEXT.md "Marker Color Advisor"):
//! analyzes a video's overall hue palette and recommends physical marker
//! colors that would contrast well against it, so a user can pick a marker
//! before filming rather than discovering after the fact that their scene
//! swallows it.
//!
//! Deliberately dependency-free like the rest of tracker-core: no color
//! names here (see CONTEXT.md, "Color Model" — no fixed color names in the
//! domain). `HueRecommendation::name` is presentation-layer labeling for a
//! CLI/UI to display, using a small fixed label set purely for human
//! readability; it carries no domain meaning and a caller could ignore it
//! and just use `hue_degrees`.

use crate::color::rgb_to_hsv;
use crate::geometry::Frame;

/// Configuration for `hue_histogram`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HueHistogramConfig {
    /// Number of equal-width buckets spanning `[0, 360)`. Default 36 (10°
    /// each).
    pub bucket_count: usize,
    /// Minimum saturation for a pixel's hue to be counted. Below this, hue
    /// is numerically defined but visually meaningless (washed-out
    /// pixels). Default 0.2.
    pub min_saturation: f64,
    /// Minimum value (brightness) for a pixel's hue to be counted. Below
    /// this, hue is meaningless (near-black pixels). Default 0.15.
    pub min_value: f64,
    /// Only every `stride`-th pixel (in raster order) is sampled, so a
    /// full-resolution frame stays cheap to histogram. Default 1 (every
    /// pixel). Must be >= 1; a value of 0 is treated as 1.
    pub stride: usize,
}

impl HueHistogramConfig {
    pub fn default_config() -> Self {
        Self {
            bucket_count: 36,
            min_saturation: 0.2,
            min_value: 0.15,
            stride: 1,
        }
    }
}

impl Default for HueHistogramConfig {
    fn default() -> Self {
        Self::default_config()
    }
}

/// A normalized hue histogram over one or more sampled frames: `buckets[i]`
/// is the fraction (in `[0, 1]`) of counted pixels whose hue falls in
/// bucket `i`'s range. Buckets sum to 1.0 when `total_counted > 0`, and are
/// all-zero when nothing was counted (e.g. an entirely gray/dark scene).
#[derive(Debug, Clone, PartialEq)]
pub struct HueHistogram {
    buckets: Vec<f64>,
    bucket_width_deg: f64,
    total_counted: u64,
}

impl HueHistogram {
    /// Fractional mass (in `[0, 1]`) in the bucket covering `hue_degrees`.
    pub fn mass_at(&self, hue_degrees: f64) -> f64 {
        self.buckets[bucket_index(hue_degrees, self.buckets.len())]
    }

    pub fn buckets(&self) -> &[f64] {
        &self.buckets
    }

    pub fn bucket_width_deg(&self) -> f64 {
        self.bucket_width_deg
    }

    /// Total number of pixels (across all sampled frames) that passed the
    /// saturation/value floors and were counted into some bucket.
    pub fn total_counted(&self) -> u64 {
        self.total_counted
    }
}

/// Bucket index for a hue value, wrapping `[0, 360)` into `bucket_count`
/// equal-width buckets.
fn bucket_index(hue_degrees: f64, bucket_count: usize) -> usize {
    let wrapped = hue_degrees.rem_euclid(360.0);
    let width = 360.0 / bucket_count as f64;
    let idx = (wrapped / width) as usize;
    idx.min(bucket_count - 1)
}

/// Builds a normalized hue histogram over `frames`, counting only pixels
/// whose saturation and value clear `config`'s floors, sub-sampled by
/// `config.stride`.
pub fn hue_histogram(frames: &[&Frame], config: HueHistogramConfig) -> HueHistogram {
    let bucket_count = config.bucket_count.max(1);
    let stride = config.stride.max(1);
    let mut counts = vec![0u64; bucket_count];
    let mut total: u64 = 0;

    for frame in frames {
        let rgb = frame.rgb();
        let pixel_count = (frame.width() as usize) * (frame.height() as usize);
        let mut i = 0usize;
        while i < pixel_count {
            let offset = i * 3;
            if offset + 2 < rgb.len() {
                let pixel = [rgb[offset], rgb[offset + 1], rgb[offset + 2]];
                let (h, s, v) = rgb_to_hsv(pixel);
                if s >= config.min_saturation && v >= config.min_value {
                    let idx = bucket_index(h, bucket_count);
                    counts[idx] += 1;
                    total += 1;
                }
            }
            i += stride;
        }
    }

    let buckets = if total > 0 {
        counts.iter().map(|&c| c as f64 / total as f64).collect()
    } else {
        vec![0.0; bucket_count]
    };

    HueHistogram {
        buckets,
        bucket_width_deg: 360.0 / bucket_count as f64,
        total_counted: total,
    }
}

/// A recommended marker hue: how far it sits (in scene presence) from the
/// video's dominant hues, plus a display-only label.
#[derive(Debug, Clone, PartialEq)]
pub struct HueRecommendation {
    pub hue_degrees: f64,
    /// Presentation-only label (e.g. "red", "cyan") for the nearest of a
    /// small fixed set of human color names. Not a domain concept — see
    /// module docs.
    pub name: &'static str,
    /// Fraction (in `[0, 1]`) of scene pixels whose hue is close to this
    /// recommendation's bucket -- i.e. how much this hue is already
    /// "used up" by the scene. Recommendations are chosen to minimize
    /// this.
    pub scene_presence: f64,
}

/// Fixed label set used only for display (CONTEXT.md: no color names in
/// the domain). Order doesn't matter; `label_for` finds the nearest by
/// hue distance.
const LABELS: &[(&str, f64)] = &[
    ("red", 0.0),
    ("orange", 30.0),
    ("yellow", 60.0),
    ("green", 120.0),
    ("cyan", 180.0),
    ("blue", 240.0),
    ("purple", 275.0),
    ("magenta", 300.0),
    ("pink", 330.0),
];

fn label_for(hue_degrees: f64) -> &'static str {
    LABELS
        .iter()
        .min_by(|a, b| {
            hue_dist(hue_degrees, a.1)
                .partial_cmp(&hue_dist(hue_degrees, b.1))
                .expect("hues are never NaN")
        })
        .map(|(name, _)| *name)
        .unwrap_or("unknown")
}

fn hue_dist(a: f64, b: f64) -> f64 {
    let diff = (a - b).abs() % 360.0;
    if diff > 180.0 {
        360.0 - diff
    } else {
        diff
    }
}

/// Minimum separation (in degrees) enforced between returned
/// recommendations, so `top_n` doesn't just return `top_n` neighboring
/// buckets around the single best gap.
const MIN_SEPARATION_DEG: f64 = 40.0;

/// Number of evenly-spaced candidate hues considered per call. Finer than
/// the histogram's own bucket width so recommendations aren't limited to
/// bucket centers.
const CANDIDATE_COUNT: usize = 72; // every 5 degrees

/// Recommends up to `top_n` marker hues that are least represented in the
/// scene described by `hist`, i.e. would contrast best against it.
/// Candidates within `MIN_SEPARATION_DEG` of an already-chosen
/// recommendation are skipped, so results are spread around the hue
/// wheel rather than clustered in one gap.
pub fn recommend_marker_hues(hist: &HueHistogram, top_n: usize) -> Vec<HueRecommendation> {
    if top_n == 0 {
        return Vec::new();
    }

    // Candidate hue -> scene presence (mass of the histogram bucket it
    // falls in). Lower presence is better.
    let mut candidates: Vec<(f64, f64)> = (0..CANDIDATE_COUNT)
        .map(|i| {
            let hue = i as f64 * (360.0 / CANDIDATE_COUNT as f64);
            (hue, hist.mass_at(hue))
        })
        .collect();
    // Sort by ascending scene presence (best candidates first); stable
    // sort keeps ties in ascending-hue order for determinism.
    candidates.sort_by(|a, b| a.1.partial_cmp(&b.1).expect("presence is never NaN"));

    let mut chosen: Vec<HueRecommendation> = Vec::new();
    for (hue, presence) in candidates {
        if chosen.len() >= top_n {
            break;
        }
        if chosen
            .iter()
            .any(|r| hue_dist(hue, r.hue_degrees) < MIN_SEPARATION_DEG)
        {
            continue;
        }
        chosen.push(HueRecommendation {
            hue_degrees: hue,
            name: label_for(hue),
            scene_presence: presence,
        });
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Frame;

    fn uniform_frame(width: u32, height: u32, color: [u8; 3]) -> Frame {
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        for _ in 0..(width * height) {
            rgb.extend_from_slice(&color);
        }
        Frame::new(width, height, rgb).unwrap()
    }

    fn half_and_half_frame(width: u32, height: u32, top: [u8; 3], bottom: [u8; 3]) -> Frame {
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        for y in 0..height {
            let color = if y < height / 2 { top } else { bottom };
            for _ in 0..width {
                rgb.extend_from_slice(&color);
            }
        }
        Frame::new(width, height, rgb).unwrap()
    }

    // --- hue_histogram ---

    #[test]
    fn single_color_frame_yields_one_bucket_near_one() {
        let frame = uniform_frame(10, 10, [255, 0, 0]); // pure red, hue 0
        let hist = hue_histogram(&[&frame], HueHistogramConfig::default());
        assert_eq!(hist.total_counted(), 100);
        let idx = bucket_index(0.0, hist.buckets().len());
        assert!((hist.buckets()[idx] - 1.0).abs() < 1e-9);
        let sum: f64 = hist.buckets().iter().sum();
        assert!((sum - 1.0).abs() < 1e-9);
    }

    #[test]
    fn two_color_frame_yields_two_populated_buckets() {
        let frame = half_and_half_frame(10, 10, [255, 0, 0], [0, 255, 0]); // red / green
        let hist = hue_histogram(&[&frame], HueHistogramConfig::default());
        let red_idx = bucket_index(0.0, hist.buckets().len());
        let green_idx = bucket_index(120.0, hist.buckets().len());
        assert!((hist.buckets()[red_idx] - 0.5).abs() < 1e-9);
        assert!((hist.buckets()[green_idx] - 0.5).abs() < 1e-9);
        let populated = hist.buckets().iter().filter(|&&m| m > 0.0).count();
        assert_eq!(populated, 2);
    }

    #[test]
    fn gray_frame_counts_nothing() {
        let frame = uniform_frame(10, 10, [128, 128, 128]);
        let hist = hue_histogram(&[&frame], HueHistogramConfig::default());
        assert_eq!(hist.total_counted(), 0);
        assert!(hist.buckets().iter().all(|&m| m == 0.0));
    }

    #[test]
    fn dark_saturated_frame_counts_nothing_below_value_floor() {
        // Fully saturated red but very dim -- below the default value floor.
        let frame = uniform_frame(10, 10, [20, 0, 0]);
        let hist = hue_histogram(&[&frame], HueHistogramConfig::default());
        assert_eq!(hist.total_counted(), 0);
    }

    #[test]
    fn hue_wraparound_edges_fall_in_adjacent_or_same_bucket_consistently() {
        // 359.9 deg and 0.1 deg are 0.2 deg apart on the wheel; with 36
        // buckets of 10 deg each they should both land in bucket 0 (which
        // spans [350, 360) union... actually [0,10) and separately the last
        // bucket [350,360) is where 359.9 falls). Verify bucket_index gives
        // sane, in-range results at the wrap.
        let config = HueHistogramConfig::default_config();
        let low_idx = bucket_index(0.1, config.bucket_count);
        let high_idx = bucket_index(359.9, config.bucket_count);
        assert_eq!(low_idx, 0);
        assert_eq!(high_idx, config.bucket_count - 1);

        // And exercise it end-to-end via two synthetic near-0/360 frames.
        // Note: 359.9 rounds to pure [255, 0, 0] in u8 RGB (same as hue 0),
        // so use 355 to keep the two RGB fixtures distinguishable while
        // still exercising the wraparound edge.
        let near_zero = uniform_frame(4, 4, hsv_to_rgb_approx(0.1));
        let near_360 = uniform_frame(4, 4, hsv_to_rgb_approx(355.0));
        let hist = hue_histogram(&[&near_zero, &near_360], HueHistogramConfig::default());
        assert!(hist.buckets()[0] > 0.0);
        assert!(hist.buckets()[config.bucket_count - 1] > 0.0);
    }

    #[test]
    fn stride_subsamples_but_still_counts_something() {
        let frame = uniform_frame(20, 20, [255, 0, 0]);
        let config = HueHistogramConfig {
            stride: 4,
            ..HueHistogramConfig::default_config()
        };
        let hist = hue_histogram(&[&frame], config);
        assert!(hist.total_counted() > 0);
        assert!(hist.total_counted() < 400);
    }

    /// Builds a saturated, full-value RGB color at the given hue (test
    /// fixture helper; mirrors color.rs's own test helper).
    fn hsv_to_rgb_approx(h: f64) -> [u8; 3] {
        let c = 1.0;
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

    // --- recommend_marker_hues ---

    fn gray_gym_histogram() -> HueHistogram {
        // Simulate a gray gym: nothing counted at all.
        let frame = uniform_frame(10, 10, [130, 130, 130]);
        hue_histogram(&[&frame], HueHistogramConfig::default())
    }

    #[test]
    fn gray_scene_recommends_saturated_hues_with_zero_presence() {
        let hist = gray_gym_histogram();
        let recs = recommend_marker_hues(&hist, 3);
        assert_eq!(recs.len(), 3);
        for r in &recs {
            assert_eq!(r.scene_presence, 0.0);
        }
    }

    #[test]
    fn green_scene_does_not_recommend_green() {
        let frame = uniform_frame(20, 20, [0, 255, 0]); // pure green field
        let hist = hue_histogram(&[&frame], HueHistogramConfig::default());
        let recs = recommend_marker_hues(&hist, 3);
        for r in &recs {
            assert!(
                hue_dist(r.hue_degrees, 120.0) > 20.0,
                "should not recommend a hue close to the scene's dominant green, got {}",
                r.hue_degrees
            );
        }
    }

    #[test]
    fn recommendations_are_separated_by_min_distance() {
        let hist = gray_gym_histogram();
        let recs = recommend_marker_hues(&hist, 5);
        for i in 0..recs.len() {
            for j in (i + 1)..recs.len() {
                assert!(
                    hue_dist(recs[i].hue_degrees, recs[j].hue_degrees) >= MIN_SEPARATION_DEG - 1e-9,
                    "recommendations {} and {} are too close",
                    recs[i].hue_degrees,
                    recs[j].hue_degrees
                );
            }
        }
    }

    #[test]
    fn top_n_zero_returns_empty() {
        let hist = gray_gym_histogram();
        assert!(recommend_marker_hues(&hist, 0).is_empty());
    }

    #[test]
    fn labels_are_assigned_from_fixed_set() {
        let hist = gray_gym_histogram();
        let recs = recommend_marker_hues(&hist, 3);
        for r in &recs {
            assert!(LABELS.iter().any(|(name, _)| *name == r.name));
        }
    }
}
