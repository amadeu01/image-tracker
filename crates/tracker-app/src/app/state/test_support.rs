//! Shared test fixtures for the `state` submodules (task 20.1 split): pulled
//! out of the former single `state.rs` `mod tests` so `jobs.rs`/`review.rs`/
//! `session.rs`/`mod.rs` can each build the same synthetic `AppState`/
//! `BarPath` shapes without duplicating them. `pub(super)` (rooted at
//! `state`) so every descendant test module can see these, per Rust's
//! module-tree visibility rules.
#![cfg(test)]

use std::path::PathBuf;

use super::{AppState, Job};
use crate::ffprobe::VideoMetadata;
use crate::tracking::{self, TrackingHandle, TrackingRunState};

pub(super) fn meta(frame_count: Option<u64>) -> VideoMetadata {
    VideoMetadata {
        width: 4,
        height: 4,
        fps_num: 30,
        fps_den: 1,
        frame_count,
        rotation: None,
    }
}

pub(super) fn sample(
    frame_index: u64,
    x: f64,
    y: f64,
    source: tracker_core::Source,
) -> tracker_core::Sample {
    tracker_core::Sample {
        frame_index,
        position: tracker_core::Point::new(x, y),
        source,
        confidence: None,
    }
}

/// A synthetic bar path with a clean single rep: descent (y 0->10) then
/// ascent (y 10->0) across 20 tracked, evenly-spaced frames at 30fps —
/// enough for `velocity_series`/`segment_reps` to detect exactly one
/// rep without tripping any of `rep.rs`'s noise-robustness dead-bands.
pub(super) fn one_rep_bar_path() -> tracker_core::BarPath {
    let tb = tracker_core::Timebase::new(30, 1).unwrap();
    let mut samples = Vec::new();
    for i in 0..=10u64 {
        samples.push(sample(
            i,
            0.0,
            i as f64 * 10.0,
            tracker_core::Source::Tracked,
        ));
    }
    for i in 11..=20u64 {
        samples.push(sample(
            i,
            0.0,
            (20 - i) as f64 * 10.0,
            tracker_core::Source::Tracked,
        ));
    }
    tracker_core::BarPath::new(&samples, &[], tb, 0)
}

/// Two clean reps (each a descent/ascent like `one_rep_bar_path`)
/// separated by a short flat/idle rest — `segment_reps` needs a real
/// rest between reps' concentric-end and the next rep's
/// eccentric-start (`rep.rs`'s "Idle is free-form" gate) to avoid
/// folding an immediate re-descent into one continuous phase — enough
/// for two distinct reps to be detected, for the 19.1 path-filtering
/// tests.
pub(super) fn two_rep_bar_path() -> tracker_core::BarPath {
    let tb = tracker_core::Timebase::new(30, 1).unwrap();
    let mut samples = Vec::new();
    let mut frame = 0u64;
    for _ in 0..2 {
        for i in 0..=10u64 {
            samples.push(sample(
                frame,
                0.0,
                i as f64 * 10.0,
                tracker_core::Source::Tracked,
            ));
            frame += 1;
        }
        for i in 11..=20u64 {
            samples.push(sample(
                frame,
                0.0,
                (20 - i) as f64 * 10.0,
                tracker_core::Source::Tracked,
            ));
            frame += 1;
        }
        // Rest at the top (y=0) for a few frames so segment_reps sees
        // a genuine Idle gap between reps rather than one continuous
        // descent/ascent alternation.
        for _ in 0..10u64 {
            samples.push(sample(frame, 0.0, 0.0, tracker_core::Source::Tracked));
            frame += 1;
        }
    }
    tracker_core::BarPath::new(&samples, &[], tb, 0)
}

pub(super) fn state_in_review() -> AppState {
    let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(30)));
    state.toggle_placing_seed();
    state.place_seed(tracker_core::Point::new(1.0, 1.0));
    state.calibration = tracker_core::Calibration::new(
        tracker_core::Point::new(0.0, 0.0),
        tracker_core::Point::new(100.0, 0.0),
        1.0,
    )
    .ok();
    let bar_path = one_rep_bar_path();
    state.bar_path = Some(bar_path.clone());
    state.results = Some(super::SessionResults::build(bar_path, state.calibration, 0));
    assert_eq!(state.current_step(), super::WorkflowStep::Review);
    state
}

/// A `TrackingHandle` for gating/reset tests: `TrackingHandle`'s fields
/// are private outside `tracking.rs`, so the only way to get one here is
/// `spawn_tracking` itself. The job points at a nonexistent path — the
/// worker thread fails fast (`FfmpegFrameSource::spawn` errors) and
/// sends a `TrackingMessage::Error`, which is fine: these tests only
/// exercise `AppState`'s synchronous reducer logic (gating predicates,
/// field resets) around `Some(handle)`/`None`, never the message
/// contents.
pub(super) fn dummy_tracking_handle() -> TrackingHandle {
    tracking::spawn_tracking(tracking::TrackingJob {
        video_path: PathBuf::from("/definitely/does/not/exist-10-4.mp4"),
        width: 4,
        height: 4,
        fps_num: 30,
        fps_den: 1,
        seed_frame_index: 0,
        seed_position: tracker_core::Point::new(0.0, 0.0),
        tracker_config: tracking::default_tracker_config(),
        session_config: tracking::default_session_config(),
        tracker_selection: tracking::TrackerSelection::Auto,
        color_tracker_config: tracking::default_color_tracker_config(),
    })
}

pub(super) fn state_with_active_run() -> AppState {
    let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
    state.toggle_placing_seed();
    state.set_frame(3);
    state.place_seed(tracker_core::Point::new(5.0, 5.0));
    state.job = Job::Tracking {
        handle: dummy_tracking_handle(),
        paused: false,
    };
    state.tracking_run = TrackingRunState::started();
    state
}
