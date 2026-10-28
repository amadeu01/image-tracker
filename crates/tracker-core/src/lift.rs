//! Lift type classification from a bar track.
//!
//! All distances are measured in plate-diameter units taken from the track
//! itself (`TrackPoint::plate_diameter_px`), so no pixel scale is baked in.
//! Positions use the image convention (y grows downward), so a rising bar
//! has decreasing y.
//!
//! `Bench` is never returned from a bar track alone: a bench press bar path
//! is indistinguishable from a short squat path, so such tracks are
//! `Unknown` until bench clips exist to tune a rule on.

use crate::geometry::Point;

/// The lift being performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiftType {
    Squat,
    Deadlift,
    Bench,
    Unknown,
}

/// One tracked bar position with the plate size observed at that frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackPoint {
    pub frame_index: u64,
    pub position: Point,
    pub plate_diameter_px: f64,
}

/// Result of `classify_lift`; `reason` is always non-empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiftClassification {
    pub lift: LiftType,
    pub reason: String,
}

/// Thresholds for `classify_lift`, all in plate-diameter units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiftClassifierConfig {
    deadlift_min_rise_plates: f64,
    deadlift_start_low_tolerance_plates: f64,
    squat_min_depth_plates: f64,
    squat_return_tolerance_plates: f64,
}

impl LiftClassifierConfig {
    pub fn builder() -> LiftClassifierConfigBuilder {
        LiftClassifierConfigBuilder::default()
    }

    /// Initial values, to be tuned against labelled clips.
    pub fn default_config() -> Self {
        Self {
            deadlift_min_rise_plates: 1.0,
            deadlift_start_low_tolerance_plates: 0.25,
            squat_min_depth_plates: 0.75,
            squat_return_tolerance_plates: 0.35,
        }
    }

    /// Minimum rise from the start for a deadlift.
    pub fn deadlift_min_rise_plates(&self) -> f64 {
        self.deadlift_min_rise_plates
    }
    /// How far below the start the track may dip and still count as
    /// "starting at its lowest point".
    pub fn deadlift_start_low_tolerance_plates(&self) -> f64 {
        self.deadlift_start_low_tolerance_plates
    }
    /// Minimum descent below the start for a squat.
    pub fn squat_min_depth_plates(&self) -> f64 {
        self.squat_min_depth_plates
    }
    /// How close to the start height the track must end for a squat.
    pub fn squat_return_tolerance_plates(&self) -> f64 {
        self.squat_return_tolerance_plates
    }
}

impl Default for LiftClassifierConfig {
    fn default() -> Self {
        Self::default_config()
    }
}

/// Builder for `LiftClassifierConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LiftClassifierConfigBuilder {
    inner: LiftClassifierConfig,
}

impl LiftClassifierConfigBuilder {
    pub fn deadlift_min_rise_plates(mut self, v: f64) -> Self {
        self.inner.deadlift_min_rise_plates = v;
        self
    }
    pub fn deadlift_start_low_tolerance_plates(mut self, v: f64) -> Self {
        self.inner.deadlift_start_low_tolerance_plates = v;
        self
    }
    pub fn squat_min_depth_plates(mut self, v: f64) -> Self {
        self.inner.squat_min_depth_plates = v;
        self
    }
    pub fn squat_return_tolerance_plates(mut self, v: f64) -> Self {
        self.inner.squat_return_tolerance_plates = v;
        self
    }
    pub fn build(self) -> LiftClassifierConfig {
        self.inner
    }
}

fn unknown(reason: &str) -> LiftClassification {
    LiftClassification {
        lift: LiftType::Unknown,
        reason: reason.to_string(),
    }
}

/// Classifies the lift from a bar track (see module docs).
pub fn classify_lift(track: &[TrackPoint], config: &LiftClassifierConfig) -> LiftClassification {
    if track.len() < 2 {
        return unknown("track too short to classify");
    }
    let mut diameters: Vec<f64> = track
        .iter()
        .map(|p| p.plate_diameter_px)
        .filter(|d| d.is_finite() && *d > 0.0)
        .collect();
    if diameters.is_empty() {
        return unknown("no valid plate diameter in track");
    }
    diameters.sort_by(|a, b| a.total_cmp(b));
    let plate = diameters[diameters.len() / 2];

    let start_y = track[0].position.y;
    let end_y = track[track.len() - 1].position.y;
    let (min_y, max_y) = track
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
            (lo.min(p.position.y), hi.max(p.position.y))
        });
    let rise = (start_y - min_y) / plate;
    let dip = (max_y - start_y) / plate;
    let end_offset = (end_y - start_y).abs() / plate;

    if dip <= config.deadlift_start_low_tolerance_plates && rise >= config.deadlift_min_rise_plates
    {
        return LiftClassification {
            lift: LiftType::Deadlift,
            reason: format!("starts at its lowest point and rises {rise:.2} plate diameters"),
        };
    }
    if dip >= config.squat_min_depth_plates && end_offset <= config.squat_return_tolerance_plates {
        return LiftClassification {
            lift: LiftType::Squat,
            reason: format!("descends {dip:.2} plate diameters and returns near its start height"),
        };
    }
    if dip > 0.0 && end_offset <= config.squat_return_tolerance_plates {
        return unknown(
            "dip-and-return range is below the squat floor; bench is not classified from a bar track alone",
        );
    }
    unknown("track matches no known lift pattern")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(ys: &[f64]) -> Vec<TrackPoint> {
        ys.iter()
            .enumerate()
            .map(|(i, &y)| TrackPoint {
                frame_index: i as u64,
                position: Point::new(100.0, y),
                plate_diameter_px: 100.0,
            })
            .collect()
    }

    fn cfg() -> LiftClassifierConfig {
        LiftClassifierConfig::default_config()
    }

    #[test]
    fn deadlift_track_starting_low_and_rising_over_one_plate_diameter_is_deadlift() {
        let c = classify_lift(&track(&[500.0, 450.0, 400.0, 350.0, 340.0]), &cfg());
        assert_eq!(c.lift, LiftType::Deadlift);
        assert!(!c.reason.is_empty());
    }

    #[test]
    fn squat_track_dipping_and_returning_to_start_height_is_squat() {
        let c = classify_lift(&track(&[300.0, 350.0, 420.0, 350.0, 305.0]), &cfg());
        assert_eq!(c.lift, LiftType::Squat);
    }

    #[test]
    fn static_track_is_unknown() {
        let c = classify_lift(&track(&[300.0, 300.0, 300.0]), &cfg());
        assert_eq!(c.lift, LiftType::Unknown);
        assert!(!c.reason.is_empty());
    }

    #[test]
    fn empty_track_is_unknown() {
        let c = classify_lift(&[], &cfg());
        assert_eq!(c.lift, LiftType::Unknown);
        assert!(!c.reason.is_empty());
    }

    #[test]
    fn short_dip_and_return_below_bench_range_is_unknown_not_bench() {
        let c = classify_lift(&track(&[300.0, 320.0, 340.0, 320.0, 300.0]), &cfg());
        assert_eq!(c.lift, LiftType::Unknown);
        assert!(c.reason.contains("bench"));
    }

    #[test]
    fn config_builder_overrides_defaults() {
        let c = LiftClassifierConfig::builder()
            .squat_min_depth_plates(0.1)
            .build();
        let r = classify_lift(&track(&[300.0, 320.0, 340.0, 320.0, 300.0]), &c);
        assert_eq!(r.lift, LiftType::Squat);
    }
}
