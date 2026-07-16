//! Per-rep metrics (task 5.4): depth, peak/mean concentric velocity for
//! each `Rep` (see CONTEXT.md's "Rep" term and `rep.rs`).
//!
//! ## Alignment
//! `segment_reps` (5.3) returns `Rep`s as indices into the `velocity` slice
//! it was given. `velocity_series` (5.2) returns exactly one
//! `VelocitySample` per input `PathPoint`, in the same order, so a `Rep`'s
//! indices also index the `points` slice that produced that `velocity` —
//! callers must pass the *same* `points` (or at least a slice of the same
//! length/order) that `velocity` was derived from.
//!
//! ## Units
//! `depth` is computed from `points` (always raw pixels, per
//! `bar_path.rs`/`export.rs`) and scaled to meters via `cal` if given, to
//! match whatever unit `velocity` itself carries (px/s or m/s). Callers are
//! responsible for passing the same `cal` (or lack of one) used to build
//! `velocity` via `velocity_series`, otherwise `depth` and the velocity
//! figures would disagree on unit system.
//!
//! ## Concentric phase
//! The concentric (ascent) phase of a `Rep` spans `bottom..=concentric_end`
//! (see `rep.rs`). Per the documented axis convention (image y grows
//! downward), ascent has negative `vy`, so "upward speed" is `vy.abs()`.
//!
//! ## Peak vs. mean concentric velocity
//! - `peak_concentric_speed`: the maximum instantaneous upward speed
//!   (`vy.abs()`) of any *non-interpolated* sample in the concentric phase.
//! - `mean_concentric_velocity`: **displacement over time**
//!   (`concentric displacement / concentric duration`), the VBT
//!   (velocity-based training) industry-standard definition of "mean
//!   concentric velocity" — not the arithmetic mean of instantaneous
//!   samples, which would be biased by however densely frames were
//!   sampled. Computed directly from the `bottom` and `concentric_end`
//!   `PathPoint` positions/timestamps, independent of sampling density.
//!
//! ## Honest numbers (per CONTEXT.md's "Gap" term)
//! Samples with `from_interpolated: true` (a velocity sample derived across
//! a coasted-over Gap) are excluded from `peak_concentric_speed`; how many
//! were excluded is reported in `excluded_interpolated_samples` so callers
//! can judge how much of the concentric phase was fabricated motion rather
//! than tracked. `mean_concentric_velocity` is unaffected by this exclusion
//! (it only ever looks at the two endpoint positions), but if either
//! endpoint itself is `Source::Interpolated` the resulting mean velocity is
//! honest.

use crate::bar_path::PathPoint;
use crate::calibration::Calibration;
use crate::rep::Rep;
use crate::velocity::{VelocitySample, VelocityUnit};

/// Per-rep metrics: depth, peak/mean concentric velocity (task 5.4).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RepMetrics {
    /// Timestamp (seconds) of the rep's `eccentric_start`.
    pub start_t: f64,
    /// Timestamp (seconds) of the rep's `bottom`.
    pub bottom_t: f64,
    /// Timestamp (seconds) of the rep's `concentric_end`.
    pub end_t: f64,
    /// Vertical displacement (max y - min y) over the whole rep span
    /// (`eccentric_start..=concentric_end`), in px or meters (see `unit`).
    pub depth: f64,
    /// Maximum instantaneous upward speed during the concentric phase,
    /// excluding samples that touch a coasted-over Gap.
    pub peak_concentric_speed: f64,
    /// Concentric displacement / concentric duration (VBT-standard mean
    /// concentric velocity), in px/s or m/s (see `unit`).
    pub mean_concentric_velocity: f64,
    pub unit: VelocityUnit,
    /// Count of concentric-phase `VelocitySample`s with
    /// `from_interpolated: true`, excluded from `peak_concentric_speed`.
    pub excluded_interpolated_samples: usize,
}

/// Computes `RepMetrics` for a single `rep`. `velocity` and `points` must be
/// the parallel, same-order slices `rep`'s indices refer to (see module
/// docs). `cal`, if given, must be the same `Calibration` used to build
/// `velocity` (via `velocity_series`), so `depth`'s unit matches the
/// velocity figures'.
pub fn rep_metrics(
    rep: &Rep,
    velocity: &[VelocitySample],
    points: &[PathPoint],
    cal: Option<&Calibration>,
) -> RepMetrics {
    let span = &points[rep.eccentric_start..=rep.concentric_end];
    let min_y = span
        .iter()
        .map(|p| p.position.y)
        .fold(f64::INFINITY, f64::min);
    let max_y = span
        .iter()
        .map(|p| p.position.y)
        .fold(f64::NEG_INFINITY, f64::max);
    let depth_px = max_y - min_y;
    let depth = match cal {
        Some(c) => c.px_to_meters(depth_px),
        None => depth_px,
    };

    let concentric = &velocity[rep.bottom..=rep.concentric_end];
    let excluded_interpolated_samples = concentric.iter().filter(|s| s.from_interpolated).count();
    let peak_concentric_speed = concentric
        .iter()
        .filter(|s| !s.from_interpolated)
        .map(|s| s.vy.abs())
        .fold(0.0_f64, f64::max);

    let bottom_point = &points[rep.bottom];
    let end_point = &points[rep.concentric_end];
    let concentric_displacement_px = (bottom_point.position.y - end_point.position.y).abs();
    let concentric_displacement = match cal {
        Some(c) => c.px_to_meters(concentric_displacement_px),
        None => concentric_displacement_px,
    };
    let concentric_duration = end_point.t_seconds - bottom_point.t_seconds;
    let mean_concentric_velocity = if concentric_duration > 0.0 {
        concentric_displacement / concentric_duration
    } else {
        0.0
    };

    let unit = if cal.is_some() {
        VelocityUnit::MetersPerSecond
    } else {
        VelocityUnit::PixelsPerSecond
    };

    RepMetrics {
        start_t: points[rep.eccentric_start].t_seconds,
        bottom_t: bottom_point.t_seconds,
        end_t: end_point.t_seconds,
        depth,
        peak_concentric_speed,
        mean_concentric_velocity,
        unit,
        excluded_interpolated_samples,
    }
}

/// Computes `RepMetrics` for every rep in `reps`, in order. See
/// `rep_metrics` for the single-rep contract.
pub fn all_rep_metrics(
    reps: &[Rep],
    velocity: &[VelocitySample],
    points: &[PathPoint],
    cal: Option<&Calibration>,
) -> Vec<RepMetrics> {
    reps.iter()
        .map(|rep| rep_metrics(rep, velocity, points, cal))
        .collect()
}

/// Velocity loss (%) of each rep's `mean_concentric_velocity` vs rep 1's,
/// per the VBT-tracker design spec's formula: `loss_i = (1 - mean_i /
/// mean_1) * 100`. Rep 1 (index 0) always has no loss to report against
/// itself, so it's `None` (displayed as "—" per the design). Every other
/// rep is `Some(loss)` — `loss` is negative if a rep is *faster* than rep 1
/// (e.g. a warm-up rep), which callers should display as-is rather than
/// clamp, since a negative "loss" is meaningful signal, not an error.
///
/// Guards the degenerate case of rep 1 having a zero (or non-finite) mean
/// velocity — a `NaN`/`inf` ratio would otherwise poison every subsequent
/// rep's loss — by returning `None` for every rep rather than fabricating a
/// number from an undefined baseline. An empty `metrics` returns an empty
/// `Vec`.
pub fn velocity_loss_percent(metrics: &[RepMetrics]) -> Vec<Option<f64>> {
    let Some(first) = metrics.first() else {
        return Vec::new();
    };
    let mean1 = first.mean_concentric_velocity;
    let baseline_valid = mean1.is_finite() && mean1 != 0.0;
    metrics
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if i == 0 || !baseline_valid {
                None
            } else {
                Some((1.0 - m.mean_concentric_velocity / mean1) * 100.0)
            }
        })
        .collect()
}

/// The first rep whose velocity loss (vs rep 1) reaches `threshold_pct`,
/// per the VBT-tracker design's "stop set recommended" banner. `losses` is
/// expected to be `velocity_loss_percent`'s output (rep-1's `None` is
/// simply skipped, never a match). Returns `None` if no rep crosses the
/// threshold (including when `losses` is empty or every entry is `None`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StopSet {
    /// Index into `losses` (and thus into the `metrics`/`reps` it was
    /// derived from) of the first rep that crossed the threshold.
    pub rep_index: usize,
    /// That rep's velocity loss (%), always `>= threshold_pct`.
    pub loss: f64,
}

pub fn stop_set_evaluation(losses: &[Option<f64>], threshold_pct: f64) -> Option<StopSet> {
    losses.iter().enumerate().find_map(|(rep_index, loss)| {
        loss.filter(|&l| l >= threshold_pct)
            .map(|loss| StopSet { rep_index, loss })
    })
}

/// Wall-clock duration (seconds) spanned by the set: rep 1's
/// `eccentric_start` timestamp to the last rep's `concentric_end`
/// timestamp (the "SET TIME" headline card). `None` for an empty `metrics`
/// (nothing to span).
pub fn set_duration_seconds(metrics: &[RepMetrics]) -> Option<f64> {
    let first = metrics.first()?;
    let last = metrics.last()?;
    Some(last.end_t - first.start_t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Point;
    use crate::session::Source;

    /// Builds parallel `points`/`velocity` slices for a synthetic rep: a
    /// descent (y increasing) then ascent (y decreasing) at 30fps, 1px/frame
    /// motion, so depth/peak/mean are easy to hand-check.
    fn synthetic_rep() -> (Vec<PathPoint>, Vec<VelocitySample>, Rep) {
        // y: 0 -> 10 (descent, 10 frames) -> 0 (ascent, 10 frames)
        let mut ys = Vec::new();
        for i in 0..=10 {
            ys.push(i as f64);
        }
        for i in (0..10).rev() {
            ys.push(i as f64);
        }
        let points: Vec<PathPoint> = ys
            .iter()
            .enumerate()
            .map(|(i, &y)| PathPoint {
                frame_index: i as u64,
                t_seconds: i as f64 / 30.0,
                position: Point::new(0.0, y),
                source: Source::Tracked,
            })
            .collect();
        let velocity = crate::velocity::velocity_series(&points, 1, None).unwrap();
        let rep = Rep {
            eccentric_start: 0,
            bottom: 10,
            concentric_end: points.len() - 1,
        };
        (points, velocity, rep)
    }

    #[test]
    fn depth_is_max_minus_min_y_over_rep_span() {
        let (points, velocity, rep) = synthetic_rep();
        let metrics = rep_metrics(&rep, &velocity, &points, None);
        assert!((metrics.depth - 10.0).abs() < 1e-9);
        assert_eq!(metrics.unit, VelocityUnit::PixelsPerSecond);
    }

    #[test]
    fn mean_concentric_velocity_is_displacement_over_time() {
        let (points, velocity, rep) = synthetic_rep();
        let metrics = rep_metrics(&rep, &velocity, &points, None);
        // Ascent: y goes 10 -> 0 over 10 frames (bottom idx 10 to end idx
        // 20) at 30fps: displacement 10px over 10/30s = 30 px/s.
        let expected = 10.0 / (10.0 / 30.0);
        assert!((metrics.mean_concentric_velocity - expected).abs() < 1e-6);
    }

    #[test]
    fn peak_concentric_speed_is_max_upward_speed() {
        let (points, velocity, rep) = synthetic_rep();
        let metrics = rep_metrics(&rep, &velocity, &points, None);
        // Constant-speed ascent (1px/frame @ 30fps = 30 px/s); central
        // differences make interior samples exactly 30, endpoints slightly
        // different one-sided values -- peak should be >= 30.
        assert!(metrics.peak_concentric_speed >= 30.0 - 1e-6);
    }

    #[test]
    fn excludes_interpolated_samples_from_peak() {
        let (mut points, _velocity, rep) = synthetic_rep();
        // Mark one concentric-phase point as interpolated with an outlier
        // position, so if it leaked into peak it would dominate.
        points[15].source = Source::Interpolated;
        points[15].position = Point::new(0.0, -1000.0);
        let velocity = crate::velocity::velocity_series(&points, 1, None).unwrap();
        let metrics = rep_metrics(&rep, &velocity, &points, None);
        assert!(metrics.excluded_interpolated_samples > 0);
        // Peak should stay near the honest 30 px/s, not the huge outlier
        // velocity produced by the -1000 spike.
        assert!(metrics.peak_concentric_speed < 100.0);
    }

    #[test]
    fn calibration_scales_depth_and_mean_velocity_to_meters() {
        let (points, _velocity, rep) = synthetic_rep();
        let cal = Calibration::new(Point::new(0.0, 0.0), Point::new(200.0, 0.0), 0.45).unwrap();
        let velocity = crate::velocity::velocity_series(&points, 1, Some(&cal)).unwrap();
        let metrics = rep_metrics(&rep, &velocity, &points, Some(&cal));
        assert_eq!(metrics.unit, VelocityUnit::MetersPerSecond);
        assert!((metrics.depth - cal.px_to_meters(10.0)).abs() < 1e-9);
        let expected_px = 10.0 / (10.0 / 30.0);
        assert!((metrics.mean_concentric_velocity - cal.px_to_meters(expected_px)).abs() < 1e-6);
    }

    #[test]
    fn all_rep_metrics_maps_over_every_rep() {
        let (points, velocity, rep) = synthetic_rep();
        let reps = vec![rep, rep];
        let metrics = all_rep_metrics(&reps, &velocity, &points, None);
        assert_eq!(metrics.len(), 2);
        assert_eq!(metrics[0], metrics[1]);
    }

    /// Builds a synthetic `Vec<RepMetrics>` with hand-picked
    /// `mean_concentric_velocity`s (everything else zeroed/arbitrary, since
    /// `velocity_loss_percent`/`stop_set_evaluation`/`set_duration_seconds`
    /// only look at `mean_concentric_velocity` and the `start_t`/`end_t`
    /// timestamps).
    fn metrics_with_means(means: &[f64]) -> Vec<RepMetrics> {
        means
            .iter()
            .enumerate()
            .map(|(i, &mean_concentric_velocity)| RepMetrics {
                start_t: i as f64,
                bottom_t: i as f64 + 0.5,
                end_t: i as f64 + 1.0,
                depth: 0.0,
                peak_concentric_speed: 0.0,
                mean_concentric_velocity,
                unit: VelocityUnit::PixelsPerSecond,
                excluded_interpolated_samples: 0,
            })
            .collect()
    }

    #[test]
    fn velocity_loss_percent_rep_one_is_none_rest_vs_rep_one_mean() {
        // Rep 1 mean 100, rep 2 mean 80 (20% loss), rep 3 mean 120 (20%
        // *faster*, i.e. -20% "loss").
        let metrics = metrics_with_means(&[100.0, 80.0, 120.0]);
        let losses = velocity_loss_percent(&metrics);
        assert_eq!(losses.len(), 3);
        assert_eq!(losses[0], None);
        assert!((losses[1].unwrap() - 20.0).abs() < 1e-9);
        assert!((losses[2].unwrap() - (-20.0)).abs() < 1e-9);
    }

    #[test]
    fn velocity_loss_percent_is_empty_for_empty_metrics() {
        assert_eq!(velocity_loss_percent(&[]), Vec::<Option<f64>>::new());
    }

    #[test]
    fn velocity_loss_percent_guards_zero_rep_one_mean() {
        let metrics = metrics_with_means(&[0.0, 50.0]);
        let losses = velocity_loss_percent(&metrics);
        assert_eq!(losses, vec![None, None]);
    }

    #[test]
    fn stop_set_evaluation_finds_first_rep_at_or_above_threshold() {
        let metrics = metrics_with_means(&[100.0, 90.0, 75.0, 60.0]);
        let losses = velocity_loss_percent(&metrics);
        // losses: [None, 10%, 25%, 40%]
        let stop = stop_set_evaluation(&losses, 20.0).unwrap();
        assert_eq!(stop.rep_index, 2);
        assert!((stop.loss - 25.0).abs() < 1e-9);
    }

    #[test]
    fn stop_set_evaluation_none_when_never_crossed() {
        let metrics = metrics_with_means(&[100.0, 95.0, 90.0]);
        let losses = velocity_loss_percent(&metrics);
        assert_eq!(stop_set_evaluation(&losses, 20.0), None);
    }

    #[test]
    fn stop_set_evaluation_none_for_empty_losses() {
        assert_eq!(stop_set_evaluation(&[], 20.0), None);
    }

    #[test]
    fn set_duration_seconds_spans_first_start_to_last_end() {
        let metrics = metrics_with_means(&[100.0, 90.0, 80.0]);
        // start_t/end_t built as i, i+1 above -> spans 0.0..=3.0.
        assert!((set_duration_seconds(&metrics).unwrap() - 3.0).abs() < 1e-9);
    }

    #[test]
    fn set_duration_seconds_none_for_empty_metrics() {
        assert_eq!(set_duration_seconds(&[]), None);
    }
}
