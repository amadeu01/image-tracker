//! tracker-core: pure domain logic for image-tracker.
//!
//! No UI/IO dependencies. Geometry, trackers, gaps, calibration,
//! kinematics, reps, and color advisor logic will live here.

pub mod bar_path;
pub mod calibration;
pub mod color;
pub mod color_tracker;
pub mod export;
pub mod frame_source;
pub mod geometry;
pub mod metric;
pub mod overlay;
pub mod patch;
pub mod rep;
pub mod session;
pub mod smoothing;
pub mod suggest;
pub mod tracker;
pub mod velocity;
pub mod video_sink;

pub use bar_path::{BarPath, PathPoint, Timebase, TimebaseError};
pub use calibration::{Calibration, CalibrationError};
pub use color::{rgb_to_hsv, ColorModel, ColorModelConfig, ColorModelError};
pub use color_tracker::{ColorTracker, ColorTrackerConfig, ColorTrackerConfigBuilder};
pub use export::{export_csv, export_json};
pub use frame_source::FrameSource;
pub use geometry::{Frame, FrameError, Point};
pub use metric::{CorrelationMetric, Zncc};
pub use overlay::{render_overlay, Color, OverlayStyle, OverlayStyleBuilder};
pub use patch::{extract_patch, Patch};
pub use rep::{segment_reps, Rep, RepSegmentationConfig, RepSegmentationConfigBuilder};
pub use session::{
    Gap, Sample, SessionState, Source, TrackingSession, TrackingSessionConfig,
    TrackingSessionConfigBuilder,
};
pub use smoothing::{smooth_positions, SmoothingError};
pub use suggest::{
    suggest_tracker, TrackerKind, TrackerSuggestionConfig, TrackerSuggestionConfigBuilder,
};
pub use tracker::{
    StepOutcome, TemplateTracker, TemplateTrackerConfig, TemplateTrackerConfigBuilder,
    TemplateTrackerError, Tracker,
};
pub use velocity::{velocity_series, VelocityError, VelocitySample, VelocityUnit};
pub use video_sink::VideoSink;
