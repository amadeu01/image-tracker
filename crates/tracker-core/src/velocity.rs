//! Velocity series (task 5.2): central finite differences over smoothed Bar
//! Path positions, optionally scaled to m/s via a `Calibration`.
//!
//! Pipeline: smooth positions first (5.1's `smooth_positions`), then
//! differentiate. Differentiating raw (unsmoothed) tracker jitter would
//! amplify per-frame noise into wild velocity spikes, so smoothing always
//! runs first; the *raw* positions are untouched and still exported as-is
//! (see `export.rs`) — only the velocity series is derived from smoothed
//! data.
//!
//! ## Axis convention
//! `vx`/`vy` are in image-pixel convention: x increases rightward, **y
//! increases downward** (origin top-left, same as `Point`/export). This
//! means **a bar moving up has negative `vy`** and a bar moving down has
//! positive `vy`. Consumers (rep segmentation, 5.3) rely on this documented
//! sign: eccentric (descent) = positive `vy`, concentric (ascent) =
//! negative `vy`.
//!
//! ## Units
//! Without a `Calibration`, `vx`/`vy`/`speed` are in px/s and `unit` is
//! `VelocityUnit::PixelsPerSecond`. With a `Calibration`, they're scaled by
//! `px_to_meters` and `unit` is `VelocityUnit::MetersPerSecond`. Only one
//! set of fields is populated (no separate raw+scaled fields) since the
//! `PathPoint`/export already carries the raw pixel positions untouched —
//! duplicating both unit systems on every velocity sample would be
//! redundant.
//!
//! ## Differencing
//! Interior samples use a central difference: `(p[i+1] - p[i-1]) / (t[i+1]
//! - t[i-1])`.
//!
//! The first and last samples use a one-sided (forward/backward)
//! difference since they lack a neighbor on one side.
//!
//! ## Honest numbers (per CONTEXT.md's "Gap" term)
//! A velocity sample is derived from the two `PathPoint`s bracketing it (for
//! central differences) or from itself and one neighbor (for one-sided
//! differences at the ends). If *either* of those source points has
//! `Source::Interpolated`, the resulting `VelocitySample` is flagged
//! `from_interpolated: true` so metrics (5.4) can exclude coasted-over gaps
//! from honest peak/mean velocity figures rather than silently averaging in
//! fabricated motion.

use crate::bar_path::PathPoint;
use crate::calibration::Calibration;
use crate::session::Source;

/// Unit system a `VelocitySample`'s `vx`/`vy`/`speed` are expressed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VelocityUnit {
    /// No `Calibration` was available: pixels per second.
    PixelsPerSecond,
    /// Scaled via `Calibration::px_to_meters`: meters per second.
    MetersPerSecond,
}

/// A single velocity estimate at one Bar Path point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VelocitySample {
    /// Video-absolute frame index, copied from the source `PathPoint`.
    pub frame_index: u64,
    pub t_seconds: f64,
    /// Horizontal velocity. Positive = rightward.
    pub vx: f64,
    /// Vertical velocity in image convention: positive = downward, negative
    /// = upward ("bar moving up" => `vy < 0.0`).
    pub vy: f64,
    /// `sqrt(vx^2 + vy^2)`.
    pub speed: f64,
    pub unit: VelocityUnit,
    /// `true` if either point used to compute this sample was
    /// `Source::Interpolated` (coasted over a Gap) — metrics should
    /// generally exclude these from honest peak/mean figures.
    pub from_interpolated: bool,
}

/// Errors computing a velocity series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VelocityError {
    /// Fewer than 2 points: no interval to difference over.
    TooFewPoints,
    /// Timestamps were not strictly increasing (shouldn't happen for a
    /// well-formed `BarPath`, but differencing over a zero/negative `dt`
    /// is undefined).
    NonMonotonicTime,
    /// Propagated from `smooth_positions`.
    Smoothing(crate::smoothing::SmoothingError),
}

impl std::fmt::Display for VelocityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VelocityError::TooFewPoints => {
                write!(f, "need at least 2 points to compute velocity")
            }
            VelocityError::NonMonotonicTime => {
                write!(f, "timestamps must be strictly increasing")
            }
            VelocityError::Smoothing(e) => write!(f, "smoothing failed: {e:?}"),
        }
    }
}

/// Computes a velocity series from `points`: smooths positions over
/// `smoothing_window` (see `smooth_positions`), then differentiates.
///
/// Returns `Err(VelocityError::TooFewPoints)` for fewer than 2 points, and
/// `Err(VelocityError::NonMonotonicTime)` if any consecutive pair of
/// timestamps is not strictly increasing. `cal`, if given, scales the
/// output to m/s (see module docs for axis/unit conventions).
pub fn velocity_series(
    points: &[PathPoint],
    smoothing_window: usize,
    cal: Option<&Calibration>,
) -> Result<Vec<VelocitySample>, VelocityError> {
    if points.len() < 2 {
        return Err(VelocityError::TooFewPoints);
    }
    for w in points.windows(2) {
        if w[1].t_seconds <= w[0].t_seconds {
            return Err(VelocityError::NonMonotonicTime);
        }
    }

    let smoothed = crate::smoothing::smooth_positions(points, smoothing_window)
        .map_err(VelocityError::Smoothing)?;

    let n = smoothed.len();
    let unit = if cal.is_some() {
        VelocityUnit::MetersPerSecond
    } else {
        VelocityUnit::PixelsPerSecond
    };
    let scale = |px: f64| -> f64 {
        match cal {
            Some(c) => c.px_to_meters(px),
            None => px,
        }
    };

    let samples = (0..n)
        .map(|i| {
            let (lo, hi) = if i == 0 {
                (0, 1)
            } else if i == n - 1 {
                (n - 2, n - 1)
            } else {
                (i - 1, i + 1)
            };
            let dt = smoothed[hi].t_seconds - smoothed[lo].t_seconds;
            let dx = smoothed[hi].position.x - smoothed[lo].position.x;
            let dy = smoothed[hi].position.y - smoothed[lo].position.y;
            let vx = scale(dx) / dt;
            let vy = scale(dy) / dt;
            let speed = (vx * vx + vy * vy).sqrt();
            let from_interpolated = smoothed[lo].source == Source::Interpolated
                || smoothed[hi].source == Source::Interpolated;
            VelocitySample {
                frame_index: smoothed[i].frame_index,
                t_seconds: smoothed[i].t_seconds,
                vx,
                vy,
                speed,
                unit,
                from_interpolated,
            }
        })
        .collect();
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Point;

    fn point(frame_index: u64, t: f64, x: f64, y: f64, source: Source) -> PathPoint {
        PathPoint {
            frame_index,
            t_seconds: t,
            position: Point::new(x, y),
            source,
        }
    }

    #[test]
    fn rejects_fewer_than_two_points() {
        let pts = vec![point(0, 0.0, 0.0, 0.0, Source::Tracked)];
        assert_eq!(
            velocity_series(&pts, 1, None),
            Err(VelocityError::TooFewPoints)
        );
        assert_eq!(
            velocity_series(&[], 1, None),
            Err(VelocityError::TooFewPoints)
        );
    }

    #[test]
    fn rejects_non_monotonic_time() {
        let pts = vec![
            point(0, 1.0, 0.0, 0.0, Source::Tracked),
            point(1, 1.0, 1.0, 1.0, Source::Tracked),
        ];
        assert_eq!(
            velocity_series(&pts, 1, None),
            Err(VelocityError::NonMonotonicTime)
        );

        let pts_dec = vec![
            point(0, 1.0, 0.0, 0.0, Source::Tracked),
            point(1, 0.5, 1.0, 1.0, Source::Tracked),
        ];
        assert_eq!(
            velocity_series(&pts_dec, 1, None),
            Err(VelocityError::NonMonotonicTime)
        );
    }

    #[test]
    fn constant_velocity_series_gives_constant_vx() {
        // x = t, y = 0: constant vx = 1.0 px/s, vy = 0.
        let pts: Vec<PathPoint> = (0..5)
            .map(|i| point(i, i as f64, i as f64, 0.0, Source::Tracked))
            .collect();
        let series = velocity_series(&pts, 1, None).unwrap();
        assert_eq!(series.len(), 5);
        for s in &series {
            assert!((s.vx - 1.0).abs() < 1e-9);
            assert!((s.vy - 0.0).abs() < 1e-9);
            assert_eq!(s.unit, VelocityUnit::PixelsPerSecond);
            assert!(!s.from_interpolated);
        }
    }

    #[test]
    fn endpoints_use_one_sided_difference() {
        // x = t^2 (non-linear), so central vs one-sided differ measurably.
        let pts: Vec<PathPoint> = (0..5)
            .map(|i| {
                let t = i as f64;
                point(i, t, t * t, 0.0, Source::Tracked)
            })
            .collect();
        let series = velocity_series(&pts, 1, None).unwrap();
        // First point: one-sided forward diff over [0,1]: (1-0)/(1-0) = 1.0
        assert!((series[0].vx - 1.0).abs() < 1e-9);
        // Last point: one-sided backward diff over [3,4]: (16-9)/(4-3) = 7.0
        assert!((series[4].vx - 7.0).abs() < 1e-9);
        // Interior point at i=2 (t=2): central diff over [1,3]: (9-1)/2 = 4.0
        assert!((series[2].vx - 4.0).abs() < 1e-9);
    }

    #[test]
    fn bar_moving_up_gives_negative_vy() {
        // y decreases over time (moving up the image, i.e. up in real life).
        let pts: Vec<PathPoint> = (0..3)
            .map(|i| point(i, i as f64, 0.0, 10.0 - i as f64, Source::Tracked))
            .collect();
        let series = velocity_series(&pts, 1, None).unwrap();
        for s in &series {
            assert!(s.vy < 0.0);
        }
    }

    #[test]
    fn calibration_scales_to_meters_per_second() {
        let cal = Calibration::new(Point::new(0.0, 0.0), Point::new(200.0, 0.0), 0.45).unwrap();
        let pts: Vec<PathPoint> = (0..3)
            .map(|i| point(i, i as f64, i as f64, 0.0, Source::Tracked))
            .collect();
        let series = velocity_series(&pts, 1, Some(&cal)).unwrap();
        for s in &series {
            assert_eq!(s.unit, VelocityUnit::MetersPerSecond);
            // 1 px/s scaled: px_to_meters(1.0)
            assert!((s.vx - cal.px_to_meters(1.0)).abs() < 1e-9);
        }
    }

    #[test]
    fn flags_samples_touching_an_interpolated_point() {
        let pts = vec![
            point(0, 0.0, 0.0, 0.0, Source::Tracked),
            point(1, 1.0, 1.0, 0.0, Source::Interpolated),
            point(2, 2.0, 2.0, 0.0, Source::Tracked),
        ];
        let series = velocity_series(&pts, 1, None).unwrap();
        // Sample 0: uses points [0,1] -> touches interpolated point 1.
        assert!(series[0].from_interpolated);
        // Sample 1 (central, uses [0,2]): neither endpoint interpolated,
        // even though point 1 itself is interpolated -- only the two
        // differencing endpoints are checked.
        assert!(!series[1].from_interpolated);
        // Sample 2: uses points [1,2] -> touches interpolated point 1.
        assert!(series[2].from_interpolated);
    }

    #[test]
    fn smooths_before_differentiating() {
        // Noisy zigzag on top of a linear trend in x; without smoothing the
        // instantaneous velocity would swing wildly, with smoothing it
        // should track closer to the underlying trend's slope of 1.0 px/s.
        let raw_x = [0.0, 3.0, 0.0, 3.0, 0.0, 3.0, 0.0];
        let pts: Vec<PathPoint> = raw_x
            .iter()
            .enumerate()
            .map(|(i, &x)| point(i as u64, i as f64, x, 0.0, Source::Tracked))
            .collect();
        let unsmoothed = velocity_series(&pts, 1, None).unwrap();
        let smoothed = velocity_series(&pts, 5, None).unwrap();
        let variance = |vals: &[f64]| {
            let mean = vals.iter().sum::<f64>() / vals.len() as f64;
            vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / vals.len() as f64
        };
        let unsmoothed_vx: Vec<f64> = unsmoothed.iter().map(|s| s.vx).collect();
        let smoothed_vx: Vec<f64> = smoothed.iter().map(|s| s.vx).collect();
        assert!(variance(&smoothed_vx) < variance(&unsmoothed_vx));
    }
}
