//! tracker-core: pure domain logic for image-tracker.
//!
//! No UI/IO dependencies. Geometry, trackers, gaps, calibration,
//! kinematics, reps, and color advisor logic will live here.

pub mod accuracy;
pub mod bar_path;
pub mod calibration;
pub mod circle_tracker;
pub mod color;
pub mod color_advisor;
pub mod color_tracker;
pub mod export;
pub mod frame_source;
pub mod geometry;
pub mod metric;
pub mod motion;
pub mod overlay;
pub mod patch;
pub mod preprocessor;
pub mod rep;
pub mod rep_metrics;
pub mod session;
pub mod smoothing;
pub mod suggest;
pub mod tracker;
pub mod velocity;
pub mod video_sink;

pub use accuracy::{grade, AccuracyReport, GroundTruthLabel, LabelStatus};
pub use bar_path::{BarPath, PathPoint, Timebase, TimebaseError};
pub use calibration::{Calibration, CalibrationError};
pub use circle_tracker::{
    CircleTracker, CircleTrackerConfig, CircleTrackerConfigBuilder, CircleTrackerError,
};
pub use color::{rgb_to_hsv, ColorModel, ColorModelConfig, ColorModelError};
pub use color_advisor::{
    hue_histogram, recommend_marker_hues, HueHistogram, HueHistogramConfig, HueRecommendation,
};
pub use color_tracker::{ColorTracker, ColorTrackerConfig, ColorTrackerConfigBuilder};
pub use export::{export_csv, export_json, export_reps_csv, export_reps_json};
pub use frame_source::FrameSource;
pub use geometry::{Frame, FrameError, Point};
pub use metric::{CorrelationMetric, Zncc};
pub use motion::Track;
pub use overlay::{render_overlay, render_rep_bottoms, Color, OverlayStyle, OverlayStyleBuilder};
pub use patch::{extract_patch, Patch};
pub use preprocessor::{Preprocessor, PreprocessorChain};
pub use rep::{segment_reps, Rep, RepSegmentationConfig, RepSegmentationConfigBuilder};
pub use rep_metrics::{
    all_rep_metrics, linear_trend, rep_metrics, set_duration_seconds, stop_set_evaluation,
    velocity_loss_percent, RepMetrics, StopSet,
};
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
