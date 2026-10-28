//! Pure tuning -> config mapping for trackers, sessions and rep segmentation.
//!
//! Depends only on tracker-core types; the CLI, GUI and compare harness all
//! build their configs through here so defaults never drift.

use crate::{
    CircleTrackerConfig, ColorTrackerConfig, PreprocessorChain, RepSegmentationConfig,
    TemplateTrackerConfig, TrackingSessionConfig,
};

/// Sensible default `TemplateTracker` tuning for the test_videos/ footage.
/// Exposed as consts so 3.4 (end-to-end run on each video) can revisit them
/// without hunting through the tracking wiring.
pub const DEFAULT_PATCH_RADIUS: u32 = 12;
pub const DEFAULT_SEARCH_RADIUS: u32 = 30;
pub const DEFAULT_MIN_SCORE: f64 = 0.4;
pub const DEFAULT_UPDATE_THRESHOLD: f64 = 0.7;
/// Anchor veto floor (17.3): a candidate is eligible only if the anchor
/// template still scores it at or above this. Below `min_score` so it
/// rejects only candidates the anchor no longer recognizes as the object.
pub const DEFAULT_ANCHOR_FLOOR: f64 = 0.3;
pub const DEFAULT_COAST_LIMIT: u32 = 5;
/// Minimum score a mid-gap `Found` must clear to count as reacquisition
/// (10.2/10.2b). Originally wired straight to `update_threshold` (0.7),
/// which fixed the rack/mirror false-lock bug but, on the v1 e2e clip,
/// pushed gaps/reseeds from 2/0 to 8/7 — 0.7 also demoted plenty of
/// genuine-but-marginal reacquisitions (e.g. the plate sliding back into a
/// partially occluded rack corner) to misses. Tuned independently (10.2b):
/// swept 0.5/0.55/0.6 on v1 and picked 0.5, the smallest gaps+reseeds (8/6,
/// vs 11/9 at 0.55 and 8/7 at 0.6) with no jump-spike regression (tracked
/// max frame-to-frame displacement identical — 42.4px — across the whole
/// swept range including all the way down to `min_score` 0.4); v3 stays
/// 0 gaps/0 reseeds at every value and v2 stays in the same ballpark
/// (11-15 gaps/8-10 reseeds) — no blow-up. See docs/e2e-results.md's 10.2b
/// section for the full sweep table.
pub const DEFAULT_REACQUIRE_MIN_SCORE: f64 = 0.5;

/// Tunable overrides for `default_tracker_config`/`default_session_config`,
/// one field per CLI flag (3.6): `--patch-radius`, `--search-radius`,
/// `--min-score`, `--update-threshold`, `--coast-limit`. `None` falls back
/// to the module's `DEFAULT_*` const.
///
/// `preprocessor` (11.3, `--filter`) is the one field here that isn't a
/// `None`-falls-back-to-a-default override: an empty `PreprocessorChain` (its
/// `Default`) *is* the default (no filtering), so it needs no `Option`
/// wrapper. It's not `Copy` (it owns a `Vec<Preprocessor>`), so this struct
/// dropped its `Copy` derive — the one call site that read a `TrackerTuning`
/// twice (`cli::run_track`, building both `tracker_config` and
/// `color_tracker_config`) now clones it explicitly.
#[derive(Debug, Clone, Default)]
pub struct TrackerTuning {
    pub patch_radius: Option<u32>,
    pub search_radius: Option<u32>,
    pub min_score: Option<f64>,
    pub update_threshold: Option<f64>,
    /// `--anchor-floor` (17.3): overrides `DEFAULT_ANCHOR_FLOOR`. Set to 0
    /// to disable the anchor veto (reproduces the pre-17.3 drift-ratchet
    /// behaviour for A/B comparison).
    pub anchor_floor: Option<f64>,
    pub coast_limit: Option<u32>,
    /// `--reacquire-min-score` (10.2b): overrides
    /// `DEFAULT_REACQUIRE_MIN_SCORE`, decoupled from `update_threshold` so
    /// each can be tuned independently.
    pub reacquire_min_score: Option<f64>,
    /// Preprocessor chain (11.3, `--filter gaussian:<sigma>` / `--filter
    /// median:<k>`, repeatable, chain order = flag order): applied to both
    /// the `TemplateTracker`'s patch and the `ColorTracker`'s search window
    /// (see `tracker_config`/`color_tracker_config` below).
    pub preprocessor: PreprocessorChain,
}

/// Builds a `TemplateTrackerConfig` from the module's default consts.
pub fn default_tracker_config() -> TemplateTrackerConfig {
    tracker_config(TrackerTuning::default())
}

/// Builds a `TrackingSessionConfig` from the module's default consts.
pub fn default_session_config() -> TrackingSessionConfig {
    session_config(TrackerTuning::default())
}

/// Builds a `ColorTrackerConfig` using its own module defaults (search
/// radius 25, min pixels 5) and an empty (identity) filter chain — the
/// color path doesn't currently expose CLI tuning flags of its own beyond
/// `--filter` (4.3 is about *choosing* the tracker, not re-tuning it).
pub fn default_color_tracker_config() -> ColorTrackerConfig {
    color_tracker_config(TrackerTuning::default())
}

/// Builds a `ColorTrackerConfig`, applying `tuning.preprocessor` (11.3,
/// `--filter`) on top of the color path's own module defaults (search
/// radius 25, min pixels 5) — the color path has no other tunable knobs of
/// its own yet.
pub fn color_tracker_config(tuning: TrackerTuning) -> ColorTrackerConfig {
    ColorTrackerConfig::builder()
        .preprocessor(tuning.preprocessor)
        .build()
}

/// Builds a `CircleTrackerConfig` (17.5) using its own module defaults — the
/// circle path has no CLI tuning flags of its own yet, since it doesn't
/// share a config shape with the appearance-based trackers.
pub fn default_circle_tracker_config() -> CircleTrackerConfig {
    CircleTrackerConfig::builder().build()
}

/// Builds a `TemplateTrackerConfig`, using `tuning`'s overrides where set and
/// the module defaults otherwise.
pub fn tracker_config(tuning: TrackerTuning) -> TemplateTrackerConfig {
    TemplateTrackerConfig::builder()
        .patch_radius(tuning.patch_radius.unwrap_or(DEFAULT_PATCH_RADIUS))
        .search_radius(tuning.search_radius.unwrap_or(DEFAULT_SEARCH_RADIUS))
        .min_score(tuning.min_score.unwrap_or(DEFAULT_MIN_SCORE))
        .update_threshold(tuning.update_threshold.unwrap_or(DEFAULT_UPDATE_THRESHOLD))
        .anchor_floor(tuning.anchor_floor.unwrap_or(DEFAULT_ANCHOR_FLOOR))
        .preprocessor(tuning.preprocessor)
        .build()
}

/// Builds a `TrackingSessionConfig`, using `tuning`'s overrides where set
/// and the module defaults otherwise.
///
/// `reacquire_min_score` (10.2, decoupled in 10.2b) gates mid-gap
/// reacquisition: while a gap is open, a `Found` must clear this score —
/// not just the tracker's looser `min_score` — to count as reacquisition,
/// which is what stops the crosshair from locking onto background clutter
/// (a rack, a mirror) that scores just above `min_score` after the real
/// object has left the frame. It used to inherit `update_threshold`
/// directly (0.7), but that turned out too strict on genuine marginal
/// reacquisitions (v1 e2e: 2 gaps/0 reseeds -> 8 gaps/7 reseeds); it now has
/// its own default (`DEFAULT_REACQUIRE_MIN_SCORE`, tuned to 0.5) and its
/// own `--reacquire-min-score` CLI override, independent of
/// `--update-threshold`.
/// `max_reacquire_distance` (10.2b) is always set to `2 * search_radius`: a
/// genuine reacquisition should land within the tracker's own search window
/// plus some slack for drift during the gap, whereas a false lock onto
/// unrelated clutter elsewhere in frame is usually much farther away. This
/// is a second, independent guard alongside `reacquire_min_score` — it has
/// no separate CLI flag since it derives from `--search-radius`.
pub fn session_config(tuning: TrackerTuning) -> TrackingSessionConfig {
    let search_radius = tuning.search_radius.unwrap_or(DEFAULT_SEARCH_RADIUS);
    TrackingSessionConfig::builder()
        .coast_limit(tuning.coast_limit.unwrap_or(DEFAULT_COAST_LIMIT))
        .reacquire_min_score(
            tuning
                .reacquire_min_score
                .unwrap_or(DEFAULT_REACQUIRE_MIN_SCORE),
        )
        .max_reacquire_distance(2.0 * search_radius as f64)
        .build()
}

/// Builds a `RepSegmentationConfig` for a run's velocity units: uncalibrated
/// (px/s) data keeps `RepSegmentationConfig::default_config`'s dead-band
/// (tuned for pixel-scale motion); calibrated (m/s) data needs a much
/// smaller `min_velocity` (bar speeds are typically well under 1-2 m/s), or
/// every sample stays `Idle` and zero reps are ever detected. Likewise
/// `min_displacement` (task 15.1, phantom walkout reps): the px-scale 40.0
/// default becomes 0.15 m calibrated — well under a real squat's ~0.5 m ROM,
/// well above setup shuffling. Shared by the CLI (`cli.rs`), the GUI's
/// post-tracking `SessionResults` (10.3), and the live rep counter (10.8)
/// so the three never drift on this tuning.
pub fn rep_segmentation_config(calibrated: bool) -> RepSegmentationConfig {
    if calibrated {
        RepSegmentationConfig::builder()
            .min_velocity(0.03)
            .min_displacement(0.15)
            .build()
    } else {
        RepSegmentationConfig::default_config()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_config_falls_back_to_defaults_when_tuning_is_empty() {
        let config = tracker_config(TrackerTuning::default());
        assert_eq!(config.patch_radius(), DEFAULT_PATCH_RADIUS);
        assert_eq!(config.search_radius(), DEFAULT_SEARCH_RADIUS);
        assert_eq!(config.min_score(), DEFAULT_MIN_SCORE);
        assert_eq!(config.update_threshold(), DEFAULT_UPDATE_THRESHOLD);
    }

    #[test]
    fn tracker_config_applies_overrides() {
        let tuning = TrackerTuning {
            patch_radius: Some(20),
            search_radius: Some(40),
            min_score: Some(0.55),
            update_threshold: Some(0.8),
            anchor_floor: None,
            coast_limit: None,
            reacquire_min_score: None,
            preprocessor: PreprocessorChain::default(),
        };
        let config = tracker_config(tuning);
        assert_eq!(config.patch_radius(), 20);
        assert_eq!(config.search_radius(), 40);
        assert_eq!(config.min_score(), 0.55);
        assert_eq!(config.update_threshold(), 0.8);
    }

    #[test]
    fn tracker_config_applies_preprocessor_chain() {
        let chain = PreprocessorChain::from_steps(vec![crate::Preprocessor::Median { k: 3 }]);
        let tuning = TrackerTuning {
            preprocessor: chain.clone(),
            ..Default::default()
        };
        assert_eq!(tracker_config(tuning).preprocessor(), &chain);
    }

    #[test]
    fn color_tracker_config_applies_preprocessor_chain() {
        let chain =
            PreprocessorChain::from_steps(vec![crate::Preprocessor::GaussianBlur { sigma: 1.5 }]);
        let tuning = TrackerTuning {
            preprocessor: chain.clone(),
            ..Default::default()
        };
        assert_eq!(color_tracker_config(tuning).preprocessor(), &chain);
        // Default (no tuning) stays an empty/identity chain.
        assert!(default_color_tracker_config().preprocessor().is_empty());
    }

    #[test]
    fn session_config_applies_coast_limit_override() {
        let tuning = TrackerTuning {
            coast_limit: Some(9),
            ..Default::default()
        };
        assert_eq!(session_config(tuning).coast_limit(), 9);
        assert_eq!(
            session_config(TrackerTuning::default()).coast_limit(),
            DEFAULT_COAST_LIMIT
        );
    }

    #[test]
    fn session_config_reacquire_min_score_defaults_and_is_decoupled_from_update_threshold() {
        // Default: DEFAULT_REACQUIRE_MIN_SCORE, not update_threshold (10.2b).
        assert_eq!(
            session_config(TrackerTuning::default()).reacquire_min_score(),
            Some(DEFAULT_REACQUIRE_MIN_SCORE)
        );
        // Overriding --update-threshold no longer moves the reacquire floor.
        let tuning = TrackerTuning {
            update_threshold: Some(0.85),
            ..Default::default()
        };
        assert_eq!(
            session_config(tuning).reacquire_min_score(),
            Some(DEFAULT_REACQUIRE_MIN_SCORE)
        );
        // Its own override is independent.
        let tuning = TrackerTuning {
            reacquire_min_score: Some(0.6),
            ..Default::default()
        };
        assert_eq!(session_config(tuning).reacquire_min_score(), Some(0.6));
    }

    #[test]
    fn session_config_derives_max_reacquire_distance_from_search_radius() {
        assert_eq!(
            session_config(TrackerTuning::default()).max_reacquire_distance(),
            Some(2.0 * DEFAULT_SEARCH_RADIUS as f64)
        );
        let tuning = TrackerTuning {
            search_radius: Some(50),
            ..Default::default()
        };
        assert_eq!(session_config(tuning).max_reacquire_distance(), Some(100.0));
    }

    #[test]
    fn rep_segmentation_config_lowers_dead_band_when_calibrated() {
        let uncal = rep_segmentation_config(false);
        let cal = rep_segmentation_config(true);
        assert_eq!(uncal.min_velocity(), 5.0);
        assert_eq!(cal.min_velocity(), 0.03);
    }

    /// The min-displacement gate (task 15.1) must scale with units too:
    /// px-scale default when uncalibrated, meter-scale when calibrated.
    #[test]
    fn rep_segmentation_config_scales_min_displacement_with_units() {
        assert_eq!(rep_segmentation_config(false).min_displacement(), 40.0);
        assert_eq!(rep_segmentation_config(true).min_displacement(), 0.15);
    }
}
