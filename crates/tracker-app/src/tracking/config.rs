//! Tracker selection and the `AnyTracker` dispatch enum. The pure tuning ->
//! config mapping lives in `tracker_core::tuning` and is re-exported here so
//! `crate::tracking::*` paths stay stable.

use tracker_core::{
    CircleTracker, ColorTracker, Frame, StepOutcome, TemplateTracker, Track, Tracker,
};

pub use tracker_core::tuning::{
    color_tracker_config, default_circle_tracker_config, default_color_tracker_config,
    default_session_config, default_tracker_config, rep_segmentation_config, session_config,
    tracker_config, TrackerTuning, DEFAULT_ANCHOR_FLOOR, DEFAULT_COAST_LIMIT, DEFAULT_MIN_SCORE,
    DEFAULT_PATCH_RADIUS, DEFAULT_REACQUIRE_MIN_SCORE, DEFAULT_SEARCH_RADIUS,
    DEFAULT_UPDATE_THRESHOLD,
};

/// Which tracker `run_tracking_worker` should use once it has decoded the
/// seed frame (task 4.3): `Auto` runs `tracker_core::suggest_tracker` on the
/// seed patch and logs the chosen kind; `Template`/`Color` force a specific
/// tracker regardless of the seed's appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrackerSelection {
    #[default]
    Auto,
    Template,
    Color,
    /// The plate-circle tracker (17.5, audit F6): geometric circle fit
    /// rather than appearance matching. Not reachable via `Auto` yet
    /// (`suggest_tracker`'s heuristic doesn't know how to recognize "a
    /// circular edge is present" — deferred, see PLAN 17.5) — must be
    /// selected explicitly with `--tracker circle`.
    Circle,
}

impl std::str::FromStr for TrackerSelection {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "auto" => Ok(TrackerSelection::Auto),
            "template" => Ok(TrackerSelection::Template),
            "color" => Ok(TrackerSelection::Color),
            "circle" => Ok(TrackerSelection::Circle),
            other => Err(format!(
                "bad --tracker (expected auto|template|color|circle): {other}"
            )),
        }
    }
}

/// Either tracker (4.2/4.3/17.5), so a `TrackingSession` can drive whichever
/// one was resolved for a run without the session itself needing to know
/// which.
#[derive(Debug, Clone, PartialEq)]
pub enum AnyTracker {
    Template(TemplateTracker),
    Color(ColorTracker),
    Circle(CircleTracker),
}

impl Tracker for AnyTracker {
    fn step(&mut self, frame: &Frame, track: &Track, dt: f64) -> StepOutcome {
        match self {
            AnyTracker::Template(t) => t.step(frame, track, dt),
            AnyTracker::Color(t) => t.step(frame, track, dt),
            AnyTracker::Circle(t) => t.step(frame, track, dt),
        }
    }
}
