//! tracker-core: pure domain logic for image-tracker.
//!
//! No UI/IO dependencies. Geometry, trackers, gaps, calibration,
//! kinematics, reps, and color advisor logic will live here.

pub mod geometry;

pub use geometry::{Frame, FrameError, Point};
