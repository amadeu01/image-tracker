//! `Track`: constant-velocity motion state threaded through `Tracker::step`
//! (17.2, audit F1/F2).
//!
//! A tracker used to be handed a single `Point` — the last known position —
//! and nothing else. It could not know how much time had passed, how fast
//! the object was moving, or how uncertain the last estimate was, so it had
//! no way to judge a candidate physically implausible. `velocity.rs`
//! already derives exactly this state, but strictly *downstream* of
//! tracking; `Track` feeds it back in.
//!
//! This is deliberately a constant-velocity predictor with a gating radius,
//! not a full Kalman filter (PLAN.md 17.2) — `uncertainty` is a single
//! scalar (a growing radius while coasting through misses), not a
//! covariance matrix.

use crate::geometry::Point;

/// Motion state for one tracked object: last known position, its estimated
/// velocity, and a scalar uncertainty radius.
///
/// `velocity` is in pixels per second (not per frame), so it composes
/// directly with `dt` regardless of frame rate. `uncertainty` starts at
/// `0.0` and grows while coasting through misses (`coasted`), resetting to
/// `0.0` on the next real observation (`observed`) — a rough stand-in for a
/// covariance matrix, sized to widen the gate and the search window the
/// longer the object has gone unseen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Track {
    pub position: Point,
    pub velocity: Point,
    pub uncertainty: f64,
}

impl Track {
    /// A fresh track at `position` with zero velocity and zero uncertainty
    /// — the state a seed or reseed starts from, since there is no prior
    /// observation to derive a velocity from yet.
    pub fn new(position: Point) -> Self {
        Self {
            position,
            velocity: Point::new(0.0, 0.0),
            uncertainty: 0.0,
        }
    }

    /// The constant-velocity prediction `dt` seconds ahead of `position`.
    /// This is what `Tracker::step` centres its search window on, instead
    /// of the raw last position (audit F1).
    pub fn predicted(&self, dt: f64) -> Point {
        Point::new(
            self.position.x + self.velocity.x * dt,
            self.position.y + self.velocity.y * dt,
        )
    }

    /// The next track state after a real (`Found`) observation `dt` seconds
    /// later: velocity is re-derived from the displacement, and uncertainty
    /// resets to `0.0` since we're no longer coasting blind.
    pub fn observed(&self, position: Point, dt: f64) -> Track {
        let velocity = if dt > 0.0 {
            Point::new(
                (position.x - self.position.x) / dt,
                (position.y - self.position.y) / dt,
            )
        } else {
            // A non-positive dt (shouldn't happen for a well-formed frame
            // sequence) can't support a velocity estimate; keep the prior
            // one rather than divide by zero.
            self.velocity
        };
        Track {
            position,
            velocity,
            uncertainty: 0.0,
        }
    }

    /// The next track state after a `Miss`: coast forward along the
    /// constant-velocity prediction (audit F1's "predict through occlusion
    /// rather than freezing") rather than sitting at the last observed
    /// position, and grow `uncertainty` by `growth_per_second * dt` — the
    /// longer the coast, the wider the net a reacquisition is allowed to be
    /// cast in.
    pub fn coasted(&self, dt: f64, growth_per_second: f64) -> Track {
        Track {
            position: self.predicted(dt),
            velocity: self.velocity,
            uncertainty: self.uncertainty + growth_per_second * dt,
        }
    }
}

/// Euclidean distance between two points.
pub(crate) fn distance(a: Point, b: Point) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

/// The physically-plausible gating radius around a constant-velocity
/// prediction (audit F1/F2): how far an observation may land from
/// `track.predicted(dt)` and still be accepted as the same object, given a
/// bound on acceleration (`max_acceleration`, px/s²) a loaded barbell can
/// plausibly undergo, plus the track's own accumulated uncertainty from
/// coasting.
///
/// `0.5 * max_acceleration * dt²` is the maximum extra displacement
/// `max_acceleration` sustained over `dt` seconds could add on top of the
/// constant-velocity prediction (`s = 0.5*a*t²`). This is deliberately not
/// a covariance-based gate (no Kalman filter, PLAN.md 17.2) — just a bound
/// derived from the physics of the object being tracked.
///
/// Unlike the mid-gap-only guards in `session.rs` (`max_reacquire_distance`,
/// `reacquire_min_score`), this gate is evaluated by the tracker on *every*
/// step, gap open or not — it is what makes silent, gap-free drift
/// rejectable (audit F2).
pub(crate) fn gate_radius(track: &Track, max_acceleration: f64, dt: f64) -> f64 {
    0.5 * max_acceleration * dt * dt + track.uncertainty
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicted_advances_by_velocity_times_dt() {
        let track = Track {
            position: Point::new(10.0, 20.0),
            velocity: Point::new(4.0, -2.0),
            uncertainty: 0.0,
        };
        assert_eq!(track.predicted(0.5), Point::new(12.0, 19.0));
    }

    #[test]
    fn new_track_predicts_no_movement() {
        let track = Track::new(Point::new(5.0, 5.0));
        assert_eq!(track.predicted(1.0), Point::new(5.0, 5.0));
        assert_eq!(track.uncertainty, 0.0);
    }

    #[test]
    fn observed_derives_velocity_and_resets_uncertainty() {
        let track = Track {
            position: Point::new(0.0, 0.0),
            velocity: Point::new(0.0, 0.0),
            uncertainty: 12.0,
        };
        let next = track.observed(Point::new(10.0, 0.0), 0.5);
        assert_eq!(next.position, Point::new(10.0, 0.0));
        assert_eq!(next.velocity, Point::new(20.0, 0.0));
        assert_eq!(next.uncertainty, 0.0);
    }

    #[test]
    fn coasted_predicts_forward_and_grows_uncertainty() {
        let track = Track {
            position: Point::new(0.0, 0.0),
            velocity: Point::new(10.0, 0.0),
            uncertainty: 0.0,
        };
        let next = track.coasted(0.5, 4.0);
        assert_eq!(next.position, Point::new(5.0, 0.0));
        assert_eq!(next.velocity, Point::new(10.0, 0.0));
        assert_eq!(next.uncertainty, 2.0);
    }

    #[test]
    fn gate_radius_grows_with_dt_squared_and_uncertainty() {
        let track = Track {
            position: Point::new(0.0, 0.0),
            velocity: Point::new(0.0, 0.0),
            uncertainty: 5.0,
        };
        // 0.5 * 100 * (1.0)^2 + 5.0 = 55.0
        assert_eq!(gate_radius(&track, 100.0, 1.0), 55.0);
    }
}
