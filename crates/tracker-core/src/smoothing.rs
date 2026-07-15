//! Centered moving-average smoothing over Bar Path positions (5.1).
//!
//! Attenuates per-frame tracking jitter while preserving each point's
//! `frame_index`, `t_seconds`, and `source` — only `position` changes.
//! Interpolated samples are smoothed like any other point but keep their
//! `Source::Interpolated` flag, so export/metrics can still tell which
//! samples were coasted over a Gap.
//!
//! Edges shrink the window rather than padding with phantom points: the
//! first and last points of the series are averaged over fewer neighbors
//! than the interior, never over data that doesn't exist.

use crate::bar_path::PathPoint;
use crate::geometry::Point;

/// Errors constructing a smoothing window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmoothingError {
    /// Window size was zero.
    ZeroWindow,
    /// Window size was even; only odd windows are centered symmetrically.
    EvenWindow,
}

/// Smooths `points` with a centered moving average over `window` samples
/// (must be odd and non-zero; `window = 1` is the identity). Near the
/// edges, the window shrinks to the largest symmetric reach available on
/// both sides (e.g. the very first/last point averages just itself) — no
/// padding, no reuse of a neighbor from only one side to compensate for a
/// missing one on the other. This keeps the average centered on the point,
/// which is what makes a linear ramp reproduce exactly everywhere.
///
/// Returns a new `Vec<PathPoint>` of the same length, with `frame_index`,
/// `t_seconds`, and `source` copied unchanged from the input and only
/// `position` replaced by the averaged value.
pub fn smooth_positions(
    points: &[PathPoint],
    window: usize,
) -> Result<Vec<PathPoint>, SmoothingError> {
    if window == 0 {
        return Err(SmoothingError::ZeroWindow);
    }
    if window.is_multiple_of(2) {
        return Err(SmoothingError::EvenWindow);
    }

    let half = window / 2;
    let n = points.len();
    let smoothed = (0..n)
        .map(|i| {
            let reach = half.min(i).min(n - 1 - i);
            let lo = i - reach;
            let hi = i + reach;
            let count = hi - lo + 1;
            let (sum_x, sum_y) = points[lo..=hi]
                .iter()
                .fold((0.0, 0.0), |(sx, sy), p| (sx + p.position.x, sy + p.position.y));
            let position = Point::new(sum_x / count as f64, sum_y / count as f64);
            PathPoint {
                position,
                ..points[i]
            }
        })
        .collect();
    Ok(smoothed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Source;

    fn point(frame_index: u64, x: f64, y: f64, source: Source) -> PathPoint {
        PathPoint {
            frame_index,
            t_seconds: frame_index as f64 / 30.0,
            position: Point::new(x, y),
            source,
        }
    }

    #[test]
    fn rejects_zero_window() {
        let pts = vec![point(0, 0.0, 0.0, Source::Tracked)];
        assert_eq!(
            smooth_positions(&pts, 0),
            Err(SmoothingError::ZeroWindow)
        );
    }

    #[test]
    fn rejects_even_window() {
        let pts = vec![point(0, 0.0, 0.0, Source::Tracked)];
        assert_eq!(
            smooth_positions(&pts, 4),
            Err(SmoothingError::EvenWindow)
        );
    }

    #[test]
    fn window_one_is_identity() {
        let pts = vec![
            point(0, 1.0, 2.0, Source::Tracked),
            point(1, 3.0, -1.0, Source::Interpolated),
            point(2, 5.0, 7.0, Source::Tracked),
        ];
        let smoothed = smooth_positions(&pts, 1).unwrap();
        assert_eq!(smoothed, pts);
    }

    #[test]
    fn constant_series_is_unchanged() {
        let pts: Vec<PathPoint> = (0..7).map(|i| point(i, 4.0, -2.0, Source::Tracked)).collect();
        let smoothed = smooth_positions(&pts, 5).unwrap();
        for p in &smoothed {
            assert_eq!(p.position, Point::new(4.0, -2.0));
        }
    }

    #[test]
    fn linear_ramp_is_preserved_exactly_by_centered_average() {
        // A centered moving average of a linear function equals the
        // function itself wherever the full window fits.
        let pts: Vec<PathPoint> = (0..9)
            .map(|i| point(i, i as f64 * 2.0, i as f64 * -3.0 + 1.0, Source::Tracked))
            .collect();
        let smoothed = smooth_positions(&pts, 5).unwrap();
        for (orig, sm) in pts.iter().zip(smoothed.iter()) {
            assert!((orig.position.x - sm.position.x).abs() < 1e-9);
            assert!((orig.position.y - sm.position.y).abs() < 1e-9);
        }
    }

    #[test]
    fn step_noise_variance_is_attenuated() {
        // Alternating +1/-1 noise on top of a constant baseline: smoothing
        // should reduce the spread of values toward the baseline.
        let raw: Vec<f64> = (0..11)
            .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let pts: Vec<PathPoint> = raw
            .iter()
            .enumerate()
            .map(|(i, &v)| point(i as u64, v, 0.0, Source::Tracked))
            .collect();
        let smoothed = smooth_positions(&pts, 5).unwrap();

        let variance = |vals: &[f64]| {
            let mean = vals.iter().sum::<f64>() / vals.len() as f64;
            vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / vals.len() as f64
        };
        let raw_var = variance(&raw);
        let smoothed_x: Vec<f64> = smoothed.iter().map(|p| p.position.x).collect();
        let smoothed_var = variance(&smoothed_x);
        assert!(smoothed_var < raw_var);
    }

    #[test]
    fn edges_shrink_the_window_symmetrically_instead_of_padding() {
        // window = 5 (half = 2), but near the edges there aren't enough
        // neighbors on both sides, so the window shrinks to the largest
        // *symmetric* reach available — never reusing a neighbor on only
        // one side to make up for a missing one on the other.
        let pts: Vec<PathPoint> = (0..5)
            .map(|i| point(i, i as f64, 0.0, Source::Tracked))
            .collect();
        let smoothed = smooth_positions(&pts, 5).unwrap();

        // First point: no neighbors available on the left, so reach = 0
        // -> averages just itself.
        assert!((smoothed[0].position.x - 0.0).abs() < 1e-9);
        // Second point: one neighbor on each side -> average of 0,1,2 = 1.0
        assert!((smoothed[1].position.x - 1.0).abs() < 1e-9);
        // Middle point has the full window: average of 0..=4 -> 2.0
        assert!((smoothed[2].position.x - 2.0).abs() < 1e-9);
        // Second-to-last point: symmetric reach of 1 -> average of 2,3,4 = 3.0
        assert!((smoothed[3].position.x - 3.0).abs() < 1e-9);
        // Last point: reach = 0 -> averages just itself.
        assert!((smoothed[4].position.x - 4.0).abs() < 1e-9);
    }

    #[test]
    fn preserves_frame_index_timestamp_and_source() {
        let pts = vec![
            point(10, 1.0, 1.0, Source::Tracked),
            point(11, 2.0, 2.0, Source::Interpolated),
            point(12, 3.0, 3.0, Source::Tracked),
        ];
        let smoothed = smooth_positions(&pts, 3).unwrap();
        for (orig, sm) in pts.iter().zip(smoothed.iter()) {
            assert_eq!(orig.frame_index, sm.frame_index);
            assert_eq!(orig.t_seconds, sm.t_seconds);
            assert_eq!(orig.source, sm.source);
        }
    }

    #[test]
    fn empty_input_yields_empty_output() {
        let smoothed = smooth_positions(&[], 5).unwrap();
        assert!(smoothed.is_empty());
    }
}
