//! Calibration: mapping from image pixels to real-world meters (task 2.5),
//! derived from two user-marked points and a known real-world length (e.g.
//! a standard 450mm plate diameter). See CONTEXT.md's "Calibration" term.

use crate::geometry::Point;

/// Errors constructing a `Calibration`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CalibrationError {
    /// The known length must be strictly positive.
    NonPositiveLength { meters: f64 },
    /// The two points must be distinct (zero pixel distance can't be scaled).
    CoincidentPoints,
}

impl std::fmt::Display for CalibrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CalibrationError::NonPositiveLength { meters } => {
                write!(f, "known length must be positive, got {meters}")
            }
            CalibrationError::CoincidentPoints => {
                write!(f, "calibration points must not coincide")
            }
        }
    }
}

/// A pixel-to-meter mapping derived from two clicked points and the known
/// real-world distance between them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Calibration {
    px_per_meter: f64,
}

impl Calibration {
    /// Build a `Calibration` from two image-pixel points and the known
    /// real-world length (in meters) between them.
    ///
    /// Rejects a non-positive `known_length_meters` and coincident points
    /// (zero pixel distance), both of which would make `px_per_meter`
    /// undefined or infinite.
    pub fn new(a: Point, b: Point, known_length_meters: f64) -> Result<Self, CalibrationError> {
        if !(known_length_meters > 0.0) {
            return Err(CalibrationError::NonPositiveLength {
                meters: known_length_meters,
            });
        }
        let dx = b.x - a.x;
        let dy = b.y - a.y;
        let px_distance = (dx * dx + dy * dy).sqrt();
        if px_distance <= 0.0 {
            return Err(CalibrationError::CoincidentPoints);
        }
        Ok(Self {
            px_per_meter: px_distance / known_length_meters,
        })
    }

    /// Pixels per meter, as derived from the two calibration points.
    pub fn px_per_meter(&self) -> f64 {
        self.px_per_meter
    }

    /// Convert a distance in pixels to meters using this calibration.
    pub fn px_to_meters(&self, px: f64) -> f64 {
        px / self.px_per_meter
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_segment_gives_expected_px_per_meter() {
        // 200px apart, known length 0.45m -> ~444.44 px/m
        let cal = Calibration::new(Point::new(0.0, 0.0), Point::new(200.0, 0.0), 0.45).unwrap();
        assert!((cal.px_per_meter() - (200.0 / 0.45)).abs() < 1e-6);
    }

    #[test]
    fn diagonal_segment_uses_euclidean_distance() {
        // 3-4-5 triangle scaled: distance 5.0px, known length 1.0m -> 5.0 px/m
        let cal = Calibration::new(Point::new(0.0, 0.0), Point::new(3.0, 4.0), 1.0).unwrap();
        assert!((cal.px_per_meter() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn px_to_meters_converts_using_calibration() {
        let cal = Calibration::new(Point::new(0.0, 0.0), Point::new(200.0, 0.0), 0.45).unwrap();
        let meters = cal.px_to_meters(200.0);
        assert!((meters - 0.45).abs() < 1e-9);
    }

    #[test]
    fn zero_length_is_rejected() {
        let err = Calibration::new(Point::new(0.0, 0.0), Point::new(200.0, 0.0), 0.0).unwrap_err();
        assert_eq!(err, CalibrationError::NonPositiveLength { meters: 0.0 });
    }

    #[test]
    fn negative_length_is_rejected() {
        let err = Calibration::new(Point::new(0.0, 0.0), Point::new(200.0, 0.0), -1.0).unwrap_err();
        assert_eq!(err, CalibrationError::NonPositiveLength { meters: -1.0 });
    }

    #[test]
    fn coincident_points_are_rejected() {
        let err = Calibration::new(Point::new(5.0, 5.0), Point::new(5.0, 5.0), 0.45).unwrap_err();
        assert_eq!(err, CalibrationError::CoincidentPoints);
    }

    #[test]
    fn error_display_messages() {
        assert_eq!(
            CalibrationError::NonPositiveLength { meters: -1.0 }.to_string(),
            "known length must be positive, got -1"
        );
        assert_eq!(
            CalibrationError::CoincidentPoints.to_string(),
            "calibration points must not coincide"
        );
    }
}
