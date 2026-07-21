//! Accuracy against hand-labelled ground truth (PLAN 17.1).
//!
//! Every metric this project had before this module — `tracked_pct`, gaps,
//! reseeds, mean match score, mean jitter — measures the tracker's
//! *self-assessment*. A tracker locked onto the wrong object maximises all
//! of them at once: it never misses, never reseeds, and a stationary false
//! lock on rack steel has *lower* jitter and *higher* correlation than
//! correct tracking of a moving barbell. See
//! `docs/design/tracking-audit-2026-07-21.md` §F7.
//!
//! This module measures something none of those can: how far the reported
//! position is from where a human says the bar actually was.
//!
//! Two numbers matter, and they are deliberately kept apart:
//!
//! - **Position error** on frames where the bar is visible — how well it
//!   tracks when tracking is possible at all.
//! - **False confidence** on frames where the bar is *not* visible — how
//!   often it reports a position it cannot possibly know. This is the one
//!   that would have caught the drift-onto-the-rack failure, where the
//!   position error metric is undefined but the tracker was reporting
//!   `Tracked` at ZNCC 0.996.
//!
//! Errors are reported in pixels *and* in plate-diameters. Pixels are not
//! comparable across videos of different resolution or camera distance; a
//! competition plate is 0.450 m, so plate-diameters is a physical unit that
//! is.

use crate::geometry::Point;
use crate::session::{Sample, Source};

/// What a human labeller saw at a given frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LabelStatus {
    /// The bar is visible and its position was marked.
    Visible(Point),
    /// The bar is present but too blurred/ambiguous to localise precisely.
    /// Excluded from the headline error (the label's own uncertainty would
    /// pollute it) but still counted as "a position was legitimately
    /// reportable here".
    Blurred(Point),
    /// The bar cannot be seen — occluded by the lifter, or gone from the
    /// frame entirely. Any confident position report here is wrong by
    /// definition.
    Occluded,
    /// The bar has left the frame (or sits on its boundary). Same
    /// treatment as `Occluded` for scoring; kept distinct because it is a
    /// different physical situation and worth reporting separately.
    OutOfFrame,
}

impl LabelStatus {
    /// The marked position, if the labeller could give one.
    pub fn position(&self) -> Option<Point> {
        match self {
            LabelStatus::Visible(p) | LabelStatus::Blurred(p) => Some(*p),
            LabelStatus::Occluded | LabelStatus::OutOfFrame => None,
        }
    }

    /// Whether a tracker could legitimately report *any* position here.
    pub fn is_reportable(&self) -> bool {
        self.position().is_some()
    }
}

/// One hand-labelled frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GroundTruthLabel {
    pub frame_index: u64,
    pub status: LabelStatus,
}

/// Accuracy of a tracked run against ground-truth labels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AccuracyReport {
    /// Labels that had a matching sample and a markable position.
    pub scored_frames: usize,
    /// Labels with no matching sample at all (e.g. before the seed frame,
    /// or after the session gave up). Not errors — just unmeasured.
    pub unmatched_frames: usize,
    /// Mean Euclidean error over `Visible` labels, in pixels.
    pub mean_error_px: Option<f64>,
    /// 95th-percentile error (nearest-rank), in pixels.
    pub p95_error_px: Option<f64>,
    /// Worst single error, in pixels.
    pub max_error_px: Option<f64>,
    /// Fraction of `Visible` labels tracked within `tolerance_px`.
    pub within_tolerance: Option<f64>,
    /// Frames where the bar was NOT visible but the tracker still reported
    /// a directly-tracked position. The headline honesty failure.
    pub false_confidence: usize,
    /// Frames where the bar was not visible and the tracker reported an
    /// interpolated (coasted) position. Less severe — the pipeline already
    /// flags these as uncertain — but still counted.
    pub coasted_while_absent: usize,
    /// Frames where the bar was not visible and the tracker correctly
    /// reported nothing.
    pub correctly_absent: usize,
}

impl AccuracyReport {
    /// Mean error expressed in plate-diameters, given the plate's apparent
    /// diameter in pixels. `None` when there is no error to report or the
    /// diameter is not a usable positive number.
    pub fn mean_error_plate_diameters(&self, plate_diameter_px: f64) -> Option<f64> {
        if !(plate_diameter_px > 0.0) {
            return None;
        }
        self.mean_error_px.map(|e| e / plate_diameter_px)
    }
}

/// Scores `samples` against `labels`.
///
/// Samples are matched to labels by exact `frame_index`. A label with no
/// matching sample counts as `unmatched_frames` rather than as an error:
/// not reporting a position is a different thing from reporting a wrong
/// one, and only the latter is a tracking failure.
///
/// `tolerance_px` sets the "close enough to be on the bar" bar for
/// `within_tolerance`; callers should derive it from the plate diameter
/// (e.g. 0.1 plate-diameters) rather than hardcoding pixels, so the
/// threshold means the same thing across videos.
pub fn grade(
    samples: &[Sample],
    labels: &[GroundTruthLabel],
    tolerance_px: f64,
) -> AccuracyReport {
    let mut errors: Vec<f64> = Vec::new();
    let mut unmatched = 0usize;
    let mut false_confidence = 0usize;
    let mut coasted_while_absent = 0usize;
    let mut correctly_absent = 0usize;

    for label in labels {
        let sample = samples.iter().find(|s| s.frame_index == label.frame_index);

        match (label.status, sample) {
            // Bar visible, tracker reported something: score the distance.
            (LabelStatus::Visible(truth), Some(s)) => {
                errors.push(distance(truth, s.position));
            }
            // Blurred labels are reportable but not scored (see LabelStatus).
            (LabelStatus::Blurred(_), Some(_)) => {}
            (LabelStatus::Visible(_) | LabelStatus::Blurred(_), None) => unmatched += 1,

            // Bar absent: any *tracked* report is false confidence.
            (LabelStatus::Occluded | LabelStatus::OutOfFrame, Some(s)) => match s.source {
                Source::Tracked => false_confidence += 1,
                Source::Interpolated => coasted_while_absent += 1,
            },
            (LabelStatus::Occluded | LabelStatus::OutOfFrame, None) => correctly_absent += 1,
        }
    }

    let scored_frames = errors.len();
    let (mean, p95, max, within) = if errors.is_empty() {
        (None, None, None, None)
    } else {
        let mut sorted = errors.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).expect("errors are finite"));
        let mean = errors.iter().sum::<f64>() / errors.len() as f64;
        // Nearest-rank p95: smallest value at or above the 95th percentile.
        let rank = ((0.95 * sorted.len() as f64).ceil() as usize).max(1) - 1;
        let p95 = sorted[rank];
        let max = *sorted.last().expect("non-empty");
        let within =
            errors.iter().filter(|e| **e <= tolerance_px).count() as f64 / errors.len() as f64;
        (Some(mean), Some(p95), Some(max), Some(within))
    };

    AccuracyReport {
        scored_frames,
        unmatched_frames: unmatched,
        mean_error_px: mean,
        p95_error_px: p95,
        max_error_px: max,
        within_tolerance: within,
        false_confidence,
        coasted_while_absent,
        correctly_absent,
    }
}

fn distance(a: Point, b: Point) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracked(frame_index: u64, x: f64, y: f64) -> Sample {
        Sample {
            frame_index,
            position: Point::new(x, y),
            source: Source::Tracked,
        }
    }

    fn interpolated(frame_index: u64, x: f64, y: f64) -> Sample {
        Sample {
            frame_index,
            position: Point::new(x, y),
            source: Source::Interpolated,
        }
    }

    fn visible(frame_index: u64, x: f64, y: f64) -> GroundTruthLabel {
        GroundTruthLabel {
            frame_index,
            status: LabelStatus::Visible(Point::new(x, y)),
        }
    }

    fn occluded(frame_index: u64) -> GroundTruthLabel {
        GroundTruthLabel {
            frame_index,
            status: LabelStatus::Occluded,
        }
    }

    #[test]
    fn perfect_tracking_scores_zero_error() {
        let samples = vec![tracked(10, 100.0, 200.0), tracked(20, 110.0, 210.0)];
        let labels = vec![visible(10, 100.0, 200.0), visible(20, 110.0, 210.0)];
        let r = grade(&samples, &labels, 5.0);
        assert_eq!(r.scored_frames, 2);
        assert_eq!(r.mean_error_px, Some(0.0));
        assert_eq!(r.within_tolerance, Some(1.0));
        assert_eq!(r.false_confidence, 0);
    }

    #[test]
    fn error_is_euclidean_distance() {
        let samples = vec![tracked(1, 3.0, 4.0)];
        let labels = vec![visible(1, 0.0, 0.0)];
        let r = grade(&samples, &labels, 1.0);
        assert_eq!(r.mean_error_px, Some(5.0));
        assert_eq!(r.max_error_px, Some(5.0));
        assert_eq!(r.within_tolerance, Some(0.0));
    }

    /// The regression this whole module exists for: a tracker that never
    /// misses and reports a confident position on every frame scores
    /// perfectly on `tracked_pct`, yet is nowhere near the bar.
    #[test]
    fn confident_false_lock_scores_badly_despite_never_missing() {
        // Tracker parked on a rack upright for the whole run.
        let samples: Vec<Sample> = (0..5).map(|i| tracked(i, 210.0, 120.0)).collect();
        // Bar was actually moving through a rep.
        let labels: Vec<GroundTruthLabel> = (0..5)
            .map(|i| visible(i, 285.0, 100.0 + 20.0 * i as f64))
            .collect();
        let r = grade(&samples, &labels, 13.0);
        assert_eq!(r.scored_frames, 5);
        assert!(r.mean_error_px.expect("errors") > 70.0);
        assert_eq!(r.within_tolerance, Some(0.0));
    }

    #[test]
    fn reporting_a_position_while_the_bar_is_absent_is_false_confidence() {
        let samples = vec![tracked(7, 400.0, 100.0)];
        let labels = vec![occluded(7)];
        let r = grade(&samples, &labels, 5.0);
        assert_eq!(r.false_confidence, 1);
        assert_eq!(r.correctly_absent, 0);
        assert_eq!(r.scored_frames, 0);
    }

    #[test]
    fn coasting_while_absent_is_counted_separately_from_false_confidence() {
        let samples = vec![interpolated(7, 400.0, 100.0)];
        let labels = vec![occluded(7)];
        let r = grade(&samples, &labels, 5.0);
        assert_eq!(r.false_confidence, 0);
        assert_eq!(r.coasted_while_absent, 1);
    }

    #[test]
    fn reporting_nothing_while_the_bar_is_absent_is_correct() {
        let samples = vec![tracked(1, 10.0, 10.0)];
        let labels = vec![occluded(7)];
        let r = grade(&samples, &labels, 5.0);
        assert_eq!(r.correctly_absent, 1);
        assert_eq!(r.false_confidence, 0);
    }

    #[test]
    fn missing_samples_are_unmatched_not_errors() {
        let samples = vec![tracked(1, 10.0, 10.0)];
        let labels = vec![visible(999, 10.0, 10.0)];
        let r = grade(&samples, &labels, 5.0);
        assert_eq!(r.unmatched_frames, 1);
        assert_eq!(r.scored_frames, 0);
        assert_eq!(r.mean_error_px, None);
    }

    #[test]
    fn blurred_labels_are_reportable_but_not_scored() {
        let samples = vec![tracked(1, 50.0, 50.0)];
        let labels = vec![GroundTruthLabel {
            frame_index: 1,
            status: LabelStatus::Blurred(Point::new(0.0, 0.0)),
        }];
        let r = grade(&samples, &labels, 5.0);
        assert_eq!(r.scored_frames, 0);
        assert_eq!(r.false_confidence, 0);
        assert_eq!(r.unmatched_frames, 0);
    }

    #[test]
    fn p95_uses_nearest_rank_and_max_is_the_worst() {
        let samples: Vec<Sample> = (0..20).map(|i| tracked(i, i as f64, 0.0)).collect();
        let labels: Vec<GroundTruthLabel> = (0..20).map(|i| visible(i, 0.0, 0.0)).collect();
        let r = grade(&samples, &labels, 100.0);
        assert_eq!(r.max_error_px, Some(19.0));
        assert_eq!(r.p95_error_px, Some(18.0));
    }

    #[test]
    fn plate_diameters_convert_from_pixels() {
        let samples = vec![tracked(1, 13.4, 0.0)];
        let labels = vec![visible(1, 0.0, 0.0)];
        let r = grade(&samples, &labels, 5.0);
        let d = r.mean_error_plate_diameters(134.0).expect("converted");
        assert!((d - 0.1).abs() < 1e-9);
        assert_eq!(r.mean_error_plate_diameters(0.0), None);
    }

    #[test]
    fn out_of_frame_is_treated_like_occluded_for_confidence() {
        let samples = vec![tracked(3, 463.0, 102.0)];
        let labels = vec![GroundTruthLabel {
            frame_index: 3,
            status: LabelStatus::OutOfFrame,
        }];
        let r = grade(&samples, &labels, 5.0);
        assert_eq!(r.false_confidence, 1);
    }
}
