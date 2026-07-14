//! tracker-core: pure domain logic for image-tracker.
//!
//! No UI/IO dependencies. Geometry, trackers, gaps, calibration,
//! kinematics, reps, and color advisor logic will live here.

pub mod geometry;
pub mod patch;

pub use geometry::{Frame, FrameError, Point};
pub use patch::{extract_patch, Patch};
