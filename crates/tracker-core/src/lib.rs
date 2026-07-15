//! tracker-core: pure domain logic for image-tracker.
//!
//! No UI/IO dependencies. Geometry, trackers, gaps, calibration,
//! kinematics, reps, and color advisor logic will live here.

pub mod bar_path;
pub mod calibration;
pub mod color;
pub mod export;
pub mod frame_source;
pub mod geometry;
pub mod metric;
pub mod overlay;
pub mod patch;
pub mod session;
pub mod tracker;
pub mod video_sink;

pub use bar_path::{BarPath, PathPoint, Timebase, TimebaseError};
pub use calibration::{Calibration, CalibrationError};
pub use color::{rgb_to_hsv, ColorModel, ColorModelConfig, ColorModelError};
pub use export::{export_csv, export_json};
pub use frame_source::FrameSource;
pub use video_sink::VideoSink;
pub use geometry::{Frame, FrameError, Point};
pub use metric::{CorrelationMetric, Zncc};
pub use overlay::{render_overlay, Color, OverlayStyle, OverlayStyleBuilder};
pub use patch::{extract_patch, Patch};
pub use session::{
    Gap, Sample, SessionState, Source, TrackingSession, TrackingSessionConfig,
    TrackingSessionConfigBuilder,
};
pub use tracker::{
    StepOutcome, TemplateTracker, TemplateTrackerConfig, TemplateTrackerConfigBuilder,
    TemplateTrackerError,
};
