//! tracker-core: pure domain logic for image-tracker.
//!
//! No UI/IO dependencies. Geometry, trackers, gaps, calibration,
//! kinematics, reps, and color advisor logic will live here.

pub mod geometry;
pub mod metric;
pub mod patch;
pub mod tracker;

pub use geometry::{Frame, FrameError, Point};
pub use metric::{CorrelationMetric, Zncc};
pub use patch::{extract_patch, Patch};
pub use tracker::{
    StepOutcome, TemplateTracker, TemplateTrackerConfig, TemplateTrackerConfigBuilder,
    TemplateTrackerError,
};
