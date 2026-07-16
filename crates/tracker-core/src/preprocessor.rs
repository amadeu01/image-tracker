//! `Preprocessor`: chainable noise-reduction filters applied to image
//! regions before matching (see CONTEXT.md, "Preprocessor"; docs/theory.md
//! §5, "Filter theory" and "Region-level filtering + the same-space
//! invariant").
//!
//! # Design
//!
//! Both filters this port ships (`GaussianBlur`, `Median`) are separable
//! per-channel, per-plane operations: each acts on a single `width x height`
//! grid of `f32` samples independently of any other plane. That single
//! `apply_plane` operation is the actual "region buffer type" both trackers
//! use, just wrapped differently:
//!
//! - `TemplateTracker` extracts a single-channel grayscale [`Patch`] (already
//!   luma-converted by `extract_patch`), so its "region" *is* one plane —
//!   `PreprocessorChain::apply_patch` filters it directly.
//! - `ColorTracker` works on 3-channel RGB pixel data. Rather than invent a
//!   generic N-channel region type, it filters the R, G, B planes of its
//!   search window independently via three calls to `apply_plane` (see
//!   `color_tracker.rs`) — since each of Gaussian blur and median filter is
//!   already defined per-plane with no cross-channel coupling, this is
//!   exactly the same computation, just called once per channel.
//!
//! This is a deliberately closed enum rather than a `dyn Preprocessor` trait
//! object: the filter set is small and fixed (CONTEXT.md names Gaussian and
//! median specifically), and a concrete, `Clone + PartialEq + Debug` type is
//! what lets `TemplateTrackerConfig`/`ColorTrackerConfig` embed an optional
//! chain and still be compared/cloned in tests without `Box<dyn ..>`
//! plumbing. `PreprocessorChain` (an ordered `Vec<Preprocessor>`) is the
//! "chain" the CONTEXT.md term and task 11.3's `--filter` flags describe.
//!
//! # Same-space invariant (CONTEXT.md)
//!
//! Filtering must happen after `extract_patch` (or after a candidate RGB
//! window is read out), not once on a whole decoded frame — see
//! docs/theory.md §5. Both `TemplateTracker` and `ColorTracker` apply their
//! configured chain to the seed/reference region at construction *and* to
//! every candidate region per step, so reference and candidates are always
//! compared in the same filtered space.

use crate::patch::Patch;

/// A single region-filtering step. See the module docs for why this is a
/// closed enum rather than a `dyn` trait.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Preprocessor {
    /// No-op: returns the input unchanged.
    Identity,
    /// Separable Gaussian blur with standard deviation `sigma` (in pixels).
    /// Kernel radius is `ceil(3*sigma)` (at least 1), which captures more
    /// than 99% of the kernel's mass. Edge handling: clamp-to-edge
    /// (replicate the border sample) rather than zero-padding, so a flat
    /// region stays exactly flat and a real border pixel isn't pulled
    /// toward black.
    GaussianBlur { sigma: f64 },
    /// Median filter over a `k x k` neighborhood (`k` odd; an even `k` is
    /// treated as `k - 1`, i.e. only the integer neighborhood radius
    /// `k / 2` is ever used). Edge handling: clamp-to-edge, same
    /// rationale as `GaussianBlur`.
    Median { k: u32 },
}

impl Preprocessor {
    /// Applies this filter to a `width x height` row-major plane of `f32`
    /// samples, returning a new plane of the same size.
    pub fn apply_plane(&self, data: &[f32], width: usize, height: usize) -> Vec<f32> {
        match self {
            Preprocessor::Identity => data.to_vec(),
            Preprocessor::GaussianBlur { sigma } => gaussian_blur(data, width, height, *sigma),
            Preprocessor::Median { k } => median_filter(data, width, height, *k),
        }
    }
}

/// An ordered chain of `Preprocessor`s, applied in sequence (the output of
/// each step feeds the next). An empty chain is equivalent to `Identity`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PreprocessorChain {
    steps: Vec<Preprocessor>,
}

impl PreprocessorChain {
    /// An empty chain (identity: no filtering).
    pub fn new() -> Self {
        Self { steps: Vec::new() }
    }

    /// Builds a chain from an ordered list of steps.
    pub fn from_steps(steps: Vec<Preprocessor>) -> Self {
        Self { steps }
    }

    /// Appends a step to the end of the chain.
    pub fn push(&mut self, step: Preprocessor) {
        self.steps.push(step);
    }

    /// The chain's steps, in application order.
    pub fn steps(&self) -> &[Preprocessor] {
        &self.steps
    }

    /// `true` if the chain has no steps (identity).
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Applies every step in order to a `width x height` plane.
    pub fn apply_plane(&self, data: &[f32], width: usize, height: usize) -> Vec<f32> {
        let mut current = data.to_vec();
        for step in &self.steps {
            current = step.apply_plane(&current, width, height);
        }
        current
    }

    /// Applies every step in order to a `Patch`, returning a new `Patch` of
    /// the same radius. This is the integration point `TemplateTracker`
    /// uses on both the seed's reference patch and every candidate patch
    /// per step (see module docs, "same-space invariant").
    pub fn apply_patch(&self, patch: &Patch) -> Patch {
        let side = patch.side() as usize;
        let filtered = self.apply_plane(patch.values(), side, side);
        // `filtered` is always exactly `side*side` long: every filter
        // preserves plane size, so this reconstruction cannot fail.
        Patch::from_values(patch.radius(), filtered).unwrap_or_else(|| patch.clone())
    }
}

/// Clamp-to-edge index: maps a possibly out-of-range coordinate into
/// `0..len` by replicating the nearest border sample.
fn clamp_index(i: i64, len: usize) -> usize {
    i.clamp(0, len as i64 - 1) as usize
}

/// A normalized 1D Gaussian kernel with radius `ceil(3*sigma)` (minimum 1).
fn gaussian_kernel_1d(sigma: f64) -> Vec<f64> {
    let sigma = sigma.max(1e-6);
    let radius = (3.0 * sigma).ceil().max(1.0) as i64;
    let mut kernel: Vec<f64> = (-radius..=radius)
        .map(|i| {
            let x = i as f64;
            (-0.5 * (x / sigma).powi(2)).exp()
        })
        .collect();
    let sum: f64 = kernel.iter().sum();
    for v in &mut kernel {
        *v /= sum;
    }
    kernel
}

/// Separable Gaussian blur: horizontal pass then vertical pass, each with
/// clamp-to-edge boundary handling.
fn gaussian_blur(data: &[f32], width: usize, height: usize, sigma: f64) -> Vec<f32> {
    if width == 0 || height == 0 {
        return data.to_vec();
    }
    let kernel = gaussian_kernel_1d(sigma);
    let radius = (kernel.len() / 2) as i64;

    // Horizontal pass.
    let mut horiz = vec![0.0f32; width * height];
    for y in 0..height {
        for x in 0..width {
            let mut acc = 0.0f64;
            for (k, &w) in kernel.iter().enumerate() {
                let dx = k as i64 - radius;
                let sx = clamp_index(x as i64 + dx, width);
                acc += w * data[y * width + sx] as f64;
            }
            horiz[y * width + x] = acc as f32;
        }
    }

    // Vertical pass.
    let mut out = vec![0.0f32; width * height];
    for y in 0..height {
        for x in 0..width {
            let mut acc = 0.0f64;
            for (k, &w) in kernel.iter().enumerate() {
                let dy = k as i64 - radius;
                let sy = clamp_index(y as i64 + dy, height);
                acc += w * horiz[sy * width + x] as f64;
            }
            out[y * width + x] = acc as f32;
        }
    }
    out
}

/// Median filter over a `k x k` neighborhood (clamp-to-edge).
fn median_filter(data: &[f32], width: usize, height: usize, k: u32) -> Vec<f32> {
    if width == 0 || height == 0 {
        return data.to_vec();
    }
    let radius = (k / 2) as i64;
    let mut out = vec![0.0f32; width * height];
    let mut window = Vec::with_capacity(((2 * radius + 1) * (2 * radius + 1)) as usize);
    for y in 0..height {
        for x in 0..width {
            window.clear();
            for dy in -radius..=radius {
                for dx in -radius..=radius {
                    let sx = clamp_index(x as i64 + dx, width);
                    let sy = clamp_index(y as i64 + dy, height);
                    window.push(data[sy * width + sx]);
                }
            }
            window.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            out[y * width + x] = window[window.len() / 2];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variance(data: &[f32]) -> f64 {
        let n = data.len() as f64;
        let mean = data.iter().map(|&v| v as f64).sum::<f64>() / n;
        data.iter().map(|&v| (v as f64 - mean).powi(2)).sum::<f64>() / n
    }

    // --- Identity ---

    #[test]
    fn identity_is_byte_exact_no_op() {
        let data = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0];
        let out = Preprocessor::Identity.apply_plane(&data, 3, 3);
        assert_eq!(out, data);
    }

    #[test]
    fn empty_chain_is_identity() {
        let chain = PreprocessorChain::new();
        assert!(chain.is_empty());
        let data = vec![1.0, 2.0, 3.0, 4.0];
        assert_eq!(chain.apply_plane(&data, 2, 2), data);
    }

    // --- Gaussian ---

    #[test]
    fn gaussian_blur_preserves_flat_regions_within_eps() {
        let data = vec![42.0f32; 9 * 9];
        let out = Preprocessor::GaussianBlur { sigma: 1.5 }.apply_plane(&data, 9, 9);
        for &v in &out {
            assert!((v - 42.0).abs() < 1e-4, "expected ~42.0, got {v}");
        }
    }

    #[test]
    fn gaussian_blur_reduces_impulse_noise_variance() {
        // Deterministic pseudo-random impulse noise on a flat 20x20 field.
        let width = 20;
        let height = 20;
        let mut data = vec![100.0f32; width * height];
        // A simple LCG for reproducible "noise".
        let mut seed: u32 = 12345;
        for v in data.iter_mut() {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = ((seed >> 16) % 41) as f32 - 20.0; // +/- 20
            *v += noise;
        }
        let noisy_variance = variance(&data);
        let out = Preprocessor::GaussianBlur { sigma: 1.5 }.apply_plane(&data, width, height);
        let blurred_variance = variance(&out);
        assert!(
            blurred_variance < noisy_variance * 0.5,
            "expected blur to cut variance substantially: noisy={noisy_variance}, blurred={blurred_variance}"
        );
    }

    #[test]
    fn gaussian_blur_edge_handling_does_not_darken_border_of_uniform_image() {
        // Clamp-to-edge means a uniform image's border pixels must not be
        // pulled toward an implicit zero-padding value.
        let data = vec![200.0f32; 5 * 5];
        let out = Preprocessor::GaussianBlur { sigma: 2.0 }.apply_plane(&data, 5, 5);
        assert!((out[0] - 200.0).abs() < 1e-3, "corner pixel: {}", out[0]);
        assert!((out[4] - 200.0).abs() < 1e-3, "corner pixel: {}", out[4]);
    }

    // --- Median ---

    #[test]
    fn median_filter_removes_single_pixel_salt_and_pepper() {
        let width = 5;
        let height = 5;
        let mut data = vec![50.0f32; width * height];
        // Single salt pixel in the interior, surrounded by uniform background.
        data[2 * width + 2] = 255.0;
        let out = Preprocessor::Median { k: 3 }.apply_plane(&data, width, height);
        assert_eq!(
            out[2 * width + 2],
            50.0,
            "salt pixel should be fully removed"
        );
    }

    #[test]
    fn median_filter_preserves_step_edges() {
        // Left half 0, right half 255 — a hard vertical step edge.
        let width = 10;
        let height = 6;
        let mut data = vec![0.0f32; width * height];
        for y in 0..height {
            for x in 5..width {
                data[y * width + x] = 255.0;
            }
        }
        let out = Preprocessor::Median { k: 3 }.apply_plane(&data, width, height);
        for y in 0..height {
            for x in 0..width {
                let expected = if x < 5 { 0.0 } else { 255.0 };
                assert_eq!(
                    out[y * width + x],
                    expected,
                    "step edge should be preserved exactly at ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn median_filter_edge_handling_clamps_at_border() {
        // A 1x1 "image" — every neighbor clamps to the single pixel itself.
        let data = vec![77.0f32];
        let out = Preprocessor::Median { k: 3 }.apply_plane(&data, 1, 1);
        assert_eq!(out, vec![77.0]);
    }

    // --- Chain ---

    #[test]
    fn chain_applies_steps_in_order() {
        let width = 9;
        let height = 9;
        let mut data = vec![50.0f32; width * height];
        data[4 * width + 4] = 255.0; // single impulse at the center

        // Median first removes the impulse cleanly, then Gaussian on an
        // already-flat field stays flat.
        let median_then_gaussian = PreprocessorChain::from_steps(vec![
            Preprocessor::Median { k: 3 },
            Preprocessor::GaussianBlur { sigma: 1.0 },
        ]);
        let out_a = median_then_gaussian.apply_plane(&data, width, height);

        // Gaussian first spreads the impulse into neighboring pixels (no
        // longer a single-pixel outlier), so median afterward can no longer
        // fully remove its effect the same way.
        let gaussian_then_median = PreprocessorChain::from_steps(vec![
            Preprocessor::GaussianBlur { sigma: 1.0 },
            Preprocessor::Median { k: 3 },
        ]);
        let out_b = gaussian_then_median.apply_plane(&data, width, height);

        assert_ne!(
            out_a, out_b,
            "chain order should change the result: median+gaussian vs gaussian+median"
        );
        // median-first should get much closer back to the flat 50.0 field.
        let center_a = out_a[4 * width + 4];
        let center_b = out_b[4 * width + 4];
        assert!(
            (center_a - 50.0).abs() < (center_b - 50.0).abs(),
            "median-first should suppress the impulse more completely: a={center_a}, b={center_b}"
        );
    }

    #[test]
    fn chain_apply_patch_round_trips_radius_and_size() {
        let chain = PreprocessorChain::from_steps(vec![Preprocessor::GaussianBlur { sigma: 1.0 }]);
        let patch = Patch::from_values(2, vec![10.0; 25]).unwrap();
        let filtered = chain.apply_patch(&patch);
        assert_eq!(filtered.radius(), 2);
        assert_eq!(filtered.side(), 5);
        assert_eq!(filtered.values().len(), 25);
    }

    #[test]
    fn chain_apply_patch_is_identity_when_chain_is_empty() {
        let chain = PreprocessorChain::new();
        let patch =
            Patch::from_values(1, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]).unwrap();
        let filtered = chain.apply_patch(&patch);
        assert_eq!(filtered, patch);
    }
}
