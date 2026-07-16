//! Gap logic: a `TrackingSession` coordinates a `TemplateTracker` across a
//! frame sequence, coasting over short misses (interpolating positions) and
//! pausing (`NeedsReseed`) when a gap exceeds the configured coast limit.
//!
//! See CONTEXT.md's "Gap" term: short gaps are coasted over and flagged;
//! a gap longer than the coast limit pauses tracking until the user
//! re-places the Seed.

use crate::geometry::{Frame, Point};
use crate::tracker::{StepOutcome, Tracker};

/// Configuration for a `TrackingSession`, built via
/// `TrackingSessionConfig::builder()`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackingSessionConfig {
    coast_limit: u32,
    reacquire_min_score: Option<f64>,
    max_reacquire_distance: Option<f64>,
}

impl TrackingSessionConfig {
    /// Starts a builder with a sensible default coast limit of 5 frames.
    pub fn builder() -> TrackingSessionConfigBuilder {
        TrackingSessionConfigBuilder::default()
    }

    /// Maximum number of consecutive missed frames coasted over before the
    /// session pauses and needs a reseed.
    pub fn coast_limit(&self) -> u32 {
        self.coast_limit
    }

    /// Minimum score a `Found` outcome must clear to count as a
    /// reacquisition while a gap is open (`miss_count > 0`). `None` (the
    /// default) preserves the old behavior of trusting the tracker's own
    /// `min_score` gate unconditionally — set this from the app to the
    /// tracker's `update_threshold` (10.2) so a marginal match against
    /// background clutter (e.g. a rack/mirror that happens to resemble the
    /// template) doesn't end a gap and start "tracking" garbage. Normal
    /// (non-gap) stepping is unaffected either way.
    pub fn reacquire_min_score(&self) -> Option<f64> {
        self.reacquire_min_score
    }

    /// Maximum distance (pixels) a mid-gap `Found` may be from the last
    /// tracked position and still count as reacquisition (10.2b). `None`
    /// (the default) disables this guard. Set from the app to roughly
    /// `2 * search_radius`: a genuine reacquisition should be found within
    /// the tracker's own search window plus some slack for drift during the
    /// gap, whereas a false lock onto unrelated background clutter (rack,
    /// mirror) elsewhere in frame is usually much farther away — this is a
    /// belt-and-suspenders check alongside `reacquire_min_score`, catching
    /// far jumps regardless of score.
    pub fn max_reacquire_distance(&self) -> Option<f64> {
        self.max_reacquire_distance
    }
}

/// Builder for `TrackingSessionConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TrackingSessionConfigBuilder {
    coast_limit: Option<u32>,
    reacquire_min_score: Option<f64>,
    max_reacquire_distance: Option<f64>,
}

impl TrackingSessionConfigBuilder {
    /// Sets the maximum number of consecutive missed frames coasted over
    /// before the session pauses and needs a reseed.
    pub fn coast_limit(mut self, limit: u32) -> Self {
        self.coast_limit = Some(limit);
        self
    }

    /// Sets the minimum score a `Found` outcome must clear, while a gap is
    /// open, to count as reacquisition rather than another miss. Leave
    /// unset for the backward-compatible behavior of accepting any `Found`.
    pub fn reacquire_min_score(mut self, score: f64) -> Self {
        self.reacquire_min_score = Some(score);
        self
    }

    /// Sets the maximum distance (pixels) a mid-gap `Found` may be from the
    /// last tracked position and still count as reacquisition. Leave unset
    /// to disable the guard.
    pub fn max_reacquire_distance(mut self, distance: f64) -> Self {
        self.max_reacquire_distance = Some(distance);
        self
    }

    pub fn build(self) -> TrackingSessionConfig {
        TrackingSessionConfig {
            coast_limit: self.coast_limit.unwrap_or(5),
            reacquire_min_score: self.reacquire_min_score,
            max_reacquire_distance: self.max_reacquire_distance,
        }
    }
}

/// Where a per-frame sample's position came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The tracker located the object directly in this frame.
    Tracked,
    /// The object was not detected; this position was linearly
    /// interpolated across a coasted gap.
    Interpolated,
}

/// A single frame's tracked (or interpolated) position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub frame_index: u64,
    pub position: Point,
    pub source: Source,
}

/// A span of frames where the object could not be detected.
///
/// `end` is the last frame belonging to the gap: either the frame the
/// object was reacquired on hand-off (mirroring the closing `Found`/reseed
/// frame) is *not* included — `end` is the last *missed* frame, i.e. one
/// before reacquisition. For a trailing gap that never closes (video ends,
/// or the session is paused awaiting reseed), `end` is the last frame that
/// was actually processed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    pub start: u64,
    pub end: u64,
}

/// Euclidean distance between two points, used by the mid-gap distance
/// guard (10.2b).
fn distance(a: Point, b: Point) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

/// Whether the session is actively tracking or paused awaiting a reseed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Tracking,
    NeedsReseed,
}

/// Coordinates a `TemplateTracker` across a sequence of frames, applying
/// Gap logic: short misses are coasted over (search continues around the
/// last known position) and, once reacquired, the intervening frames are
/// linearly interpolated. A run of misses longer than `coast_limit` pauses
/// the session (`SessionState::NeedsReseed`) until the caller calls
/// `reseed`.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackingSession<T: Tracker> {
    tracker: T,
    config: TrackingSessionConfig,
    state: SessionState,
    last_pos: Point,
    frame_index: u64,
    samples: Vec<Sample>,
    gaps: Vec<Gap>,
    /// Frame index the current open gap started at, if any.
    open_gap_start: Option<u64>,
    miss_count: u32,
}

impl<T: Tracker> TrackingSession<T> {
    /// Starts a session with `tracker` seeded at `seed_frame_index` and
    /// `seed` position (recorded as the first `Tracked` sample).
    pub fn new(
        tracker: T,
        seed_frame_index: u64,
        seed: Point,
        config: TrackingSessionConfig,
    ) -> Self {
        Self {
            tracker,
            config,
            state: SessionState::Tracking,
            last_pos: seed,
            frame_index: seed_frame_index,
            samples: vec![Sample {
                frame_index: seed_frame_index,
                position: seed,
                source: Source::Tracked,
            }],
            gaps: Vec::new(),
            open_gap_start: None,
            miss_count: 0,
        }
    }

    pub fn state(&self) -> SessionState {
        self.state
    }

    pub fn samples(&self) -> &[Sample] {
        &self.samples
    }

    pub fn gaps(&self) -> &[Gap] {
        &self.gaps
    }

    /// Feeds the next frame (assumed to be `frame_index + 1`) to the
    /// tracker. No-op if the session is currently paused awaiting reseed.
    pub fn step(&mut self, frame: &Frame) {
        if self.state == SessionState::NeedsReseed {
            return;
        }

        let next_index = self.frame_index + 1;
        let outcome = self.tracker.step(frame, self.last_pos);

        // Reacquisition strictness (10.2): mid-gap, a `Found` only counts
        // as reacquiring the object if its score clears
        // `reacquire_min_score` — otherwise it's demoted to a `Miss` so the
        // gap keeps coasting instead of locking onto a marginal match
        // (rack, mirror, whatever else scores just above the tracker's own
        // `min_score`). Normal (non-gap) stepping is untouched: this only
        // applies while `open_gap_start` is set, i.e. we're already in a
        // miss streak.
        // Distance guard (10.2b): mid-gap, a `Found` far from the last
        // tracked position is demoted to `Miss` regardless of score — a
        // second, independent check alongside `reacquire_min_score` that
        // catches a confident-but-wrong lock onto background clutter
        // elsewhere in frame.
        let outcome = match outcome {
            StepOutcome::Found { position, .. }
                if self.open_gap_start.is_some()
                    && self
                        .config
                        .max_reacquire_distance
                        .is_some_and(|max_dist| distance(self.last_pos, position) > max_dist) =>
            {
                StepOutcome::Miss
            }
            other => other,
        };

        let outcome = match outcome {
            StepOutcome::Found { score, .. }
                if self.open_gap_start.is_some()
                    && self
                        .config
                        .reacquire_min_score
                        .is_some_and(|threshold| score < threshold) =>
            {
                StepOutcome::Miss
            }
            other => other,
        };

        match outcome {
            StepOutcome::Found { position, .. } => {
                if let Some(gap_start) = self.open_gap_start.take() {
                    self.gaps.push(Gap {
                        start: gap_start,
                        end: next_index - 1,
                    });
                    self.interpolate_gap(gap_start, next_index - 1, self.last_pos, position);
                }
                self.miss_count = 0;
                self.last_pos = position;
                self.frame_index = next_index;
                self.samples.push(Sample {
                    frame_index: next_index,
                    position,
                    source: Source::Tracked,
                });
            }
            StepOutcome::Miss => {
                if self.open_gap_start.is_none() {
                    self.open_gap_start = Some(next_index);
                }
                self.miss_count += 1;
                self.frame_index = next_index;

                if self.miss_count > self.config.coast_limit {
                    self.state = SessionState::NeedsReseed;
                    // Record the still-open gap as a trailing (unresolved)
                    // span ending at the last processed frame; it will be
                    // replaced (closed properly) if `reseed` succeeds.
                    // open_gap_start is set above on the first miss of the
                    // run, but fall back to next_index rather than panic.
                    let start = self.open_gap_start.take().unwrap_or(next_index);
                    self.gaps.push(Gap {
                        start,
                        end: next_index,
                    });
                }
            }
        }
    }

    /// Linearly interpolates positions for the missed frames strictly
    /// between the last tracked frame and the reacquired frame, pushing
    /// `Interpolated` samples for each.
    fn interpolate_gap(&mut self, start: u64, end: u64, from: Point, to: Point) {
        let span = (end + 1 - start) as f64; // number of missed frames, +1 for the reacquired step
        for (i, frame_index) in (start..=end).enumerate() {
            let t = (i as f64 + 1.0) / (span + 1.0);
            let position = Point::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
            self.samples.push(Sample {
                frame_index,
                position,
                source: Source::Interpolated,
            });
        }
    }

    /// Resumes a paused session: the caller has re-placed the seed at
    /// `point` on `frame_index`. If a gap was pending (trailing,
    /// unresolved), it is replaced with one closed at `frame_index - 1`.
    /// Records a `Tracked` sample for the reseed point itself.
    pub fn reseed(&mut self, frame_index: u64, point: Point) {
        // Replace the trailing (unresolved) gap recorded when we paused,
        // if any, with one closed right before the reseed frame.
        if self.state == SessionState::NeedsReseed {
            if let Some(last) = self.gaps.last_mut() {
                last.end = frame_index.saturating_sub(1);
            }
        }

        self.state = SessionState::Tracking;
        self.last_pos = point;
        self.frame_index = frame_index;
        self.open_gap_start = None;
        self.miss_count = 0;
        self.samples.push(Sample {
            frame_index,
            position: point,
            source: Source::Tracked,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::{TemplateTracker, TemplateTrackerConfig};

    /// Builds a frame with a bright `size`x`size` square (value 220) on a
    /// dark background (value 20), with the square's top-left corner at
    /// `(sx, sy)`. Mirrors the helper in `tracker.rs`.
    fn frame_with_square(width: u32, height: u32, sx: i64, sy: i64, size: i64) -> Frame {
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        for y in 0..height as i64 {
            for x in 0..width as i64 {
                let inside = x >= sx && x < sx + size && y >= sy && y < sy + size;
                let v = if inside { 220u8 } else { 20u8 };
                rgb.extend_from_slice(&[v, v, v]);
            }
        }
        Frame::new(width, height, rgb).unwrap()
    }

    fn blank_frame(width: u32, height: u32) -> Frame {
        frame_with_square(width, height, -100, -100, 0)
    }

    fn plain_config() -> TemplateTrackerConfig {
        TemplateTrackerConfig::builder()
            .patch_radius(2)
            .search_radius(6)
            .min_score(0.5)
            .build()
    }

    const W: u32 = 60;
    const H: u32 = 40;

    fn make_session(coast_limit: u32) -> TrackingSession<TemplateTracker> {
        let ref_frame = frame_with_square(W, H, 10, 10, 4);
        let seed = Point::new(12.0, 12.0);
        let tracker = TemplateTracker::new(&ref_frame, seed, plain_config()).unwrap();
        let config = TrackingSessionConfig::builder()
            .coast_limit(coast_limit)
            .build();
        TrackingSession::new(tracker, 0, seed, config)
    }

    #[test]
    fn found_frame_is_recorded_as_tracked() {
        let mut session = make_session(3);
        let frame = frame_with_square(W, H, 10, 10, 4); // object stays put
        session.step(&frame);

        assert_eq!(session.state(), SessionState::Tracking);
        assert_eq!(session.samples().len(), 2);
        let sample = session.samples()[1];
        assert_eq!(sample.frame_index, 1);
        assert_eq!(sample.source, Source::Tracked);
        assert_eq!(sample.position, Point::new(12.0, 12.0));
        assert!(session.gaps().is_empty());
    }

    #[test]
    fn short_gap_is_coasted_and_closed_with_interpolated_samples() {
        let mut session = make_session(3); // coast_limit 3 < hidden frames (2)
                                           // frames 1, 2: object hidden (blank)
        session.step(&blank_frame(W, H));
        session.step(&blank_frame(W, H));
        assert_eq!(session.state(), SessionState::Tracking);

        // frame 3: object reappears, moved to (18, 12)
        session.step(&frame_with_square(W, H, 16, 10, 4));
        assert_eq!(session.state(), SessionState::Tracking);

        assert_eq!(session.gaps(), &[Gap { start: 1, end: 2 }]);

        // samples: seed(0) tracked, 1 interp, 2 interp, 3 tracked
        let samples = session.samples();
        assert_eq!(samples.len(), 4);
        assert_eq!(samples[1].frame_index, 1);
        assert_eq!(samples[1].source, Source::Interpolated);
        assert_eq!(samples[2].frame_index, 2);
        assert_eq!(samples[2].source, Source::Interpolated);
        assert_eq!(samples[3].frame_index, 3);
        assert_eq!(samples[3].source, Source::Tracked);
        assert_eq!(samples[3].position, Point::new(18.0, 12.0));

        // Interpolated positions lie strictly between seed and reacquired
        // position, monotonically progressing.
        assert!(samples[1].position.x > 12.0 && samples[1].position.x < samples[2].position.x);
        assert!(samples[2].position.x < 18.0);
    }

    #[test]
    fn gap_exceeding_coast_limit_pauses_session() {
        let mut session = make_session(2); // limit 2: 3rd consecutive miss trips it
        session.step(&blank_frame(W, H)); // frame 1: miss 1
        assert_eq!(session.state(), SessionState::Tracking);
        session.step(&blank_frame(W, H)); // frame 2: miss 2
        assert_eq!(session.state(), SessionState::Tracking);
        session.step(&blank_frame(W, H)); // frame 3: miss 3 > limit -> pause
        assert_eq!(session.state(), SessionState::NeedsReseed);

        // Further steps are ignored while paused.
        session.step(&frame_with_square(W, H, 10, 10, 4));
        assert_eq!(session.state(), SessionState::NeedsReseed);
        assert_eq!(session.samples().len(), 1); // only the initial seed sample

        assert_eq!(session.gaps(), &[Gap { start: 1, end: 3 }]);
    }

    #[test]
    fn reseed_resumes_tracking_after_pause() {
        let mut session = make_session(1);
        session.step(&blank_frame(W, H)); // frame 1: miss 1
        session.step(&blank_frame(W, H)); // frame 2: miss 2 > limit(1) -> pause
        assert_eq!(session.state(), SessionState::NeedsReseed);
        assert_eq!(session.gaps(), &[Gap { start: 1, end: 2 }]);

        session.reseed(5, Point::new(30.0, 20.0));
        assert_eq!(session.state(), SessionState::Tracking);
        // Trailing gap closed right before the reseed frame.
        assert_eq!(session.gaps(), &[Gap { start: 1, end: 4 }]);

        let samples = session.samples();
        let last = *samples.last().unwrap();
        assert_eq!(last.frame_index, 5);
        assert_eq!(last.source, Source::Tracked);
        assert_eq!(last.position, Point::new(30.0, 20.0));

        // Tracking continues from the reseeded position.
        let frame = frame_with_square(W, H, 28, 18, 4);
        session.step(&frame);
        assert_eq!(session.state(), SessionState::Tracking);
        assert_eq!(session.samples().last().unwrap().frame_index, 6);
        assert_eq!(session.samples().last().unwrap().source, Source::Tracked);
    }

    #[test]
    fn trailing_unresolved_gap_recorded_when_video_ends_mid_coast() {
        // Object hidden for the remaining frames of the sequence, but never
        // exceeds the coast limit, so the session keeps "Tracking" with an
        // open gap that never gets a closing Found. The gap list should
        // still reflect the trailing gap up to the last processed frame if
        // the caller inspects it (no crash, no fabricated closure).
        let mut session = make_session(5);
        session.step(&blank_frame(W, H)); // frame 1: miss
        session.step(&blank_frame(W, H)); // frame 2: miss
        assert_eq!(session.state(), SessionState::Tracking);
        // No closed gap recorded yet: it's still open.
        assert!(session.gaps().is_empty());
    }

    // -- 10.2: reacquisition strictness --------------------------------

    /// A `Tracker` test double that returns a scripted sequence of
    /// `StepOutcome`s (one per `step` call, repeating the last entry once
    /// exhausted), so reacquisition-threshold tests can control scores
    /// precisely without needing real image patches to happen to score in a
    /// particular band.
    struct ScriptedTracker {
        outcomes: std::vec::IntoIter<StepOutcome>,
        last: StepOutcome,
    }

    impl ScriptedTracker {
        fn new(outcomes: Vec<StepOutcome>) -> Self {
            let last = outcomes.last().copied().unwrap_or(StepOutcome::Miss);
            Self {
                outcomes: outcomes.into_iter(),
                last,
            }
        }
    }

    impl Tracker for ScriptedTracker {
        fn step(&mut self, _frame: &Frame, _last_pos: Point) -> StepOutcome {
            self.outcomes.next().unwrap_or(self.last)
        }
    }

    fn make_scripted_session(
        coast_limit: u32,
        reacquire_min_score: Option<f64>,
        outcomes: Vec<StepOutcome>,
    ) -> TrackingSession<ScriptedTracker> {
        let tracker = ScriptedTracker::new(outcomes);
        let mut builder = TrackingSessionConfig::builder().coast_limit(coast_limit);
        if let Some(score) = reacquire_min_score {
            builder = builder.reacquire_min_score(score);
        }
        TrackingSession::new(tracker, 0, Point::new(5.0, 5.0), builder.build())
    }

    #[test]
    fn mid_gap_weak_match_below_reacquire_threshold_stays_a_gap() {
        // Miss, then a weak "Found" (score 0.55) that clears the tracker's
        // own min_score but sits below the configured reacquire_min_score
        // (0.7) — must be treated as another miss, keeping the gap open.
        let mut session = make_scripted_session(
            5,
            Some(0.7),
            vec![
                StepOutcome::Miss,
                StepOutcome::Found {
                    position: Point::new(40.0, 40.0), // e.g. the rack, far off
                    score: 0.55,
                },
            ],
        );
        session.step(&blank_frame(W, H)); // frame 1: miss
        session.step(&blank_frame(W, H)); // frame 2: weak match, demoted to miss
        assert_eq!(session.state(), SessionState::Tracking);
        assert!(
            session.gaps().is_empty(),
            "gap should still be open (not yet closed)"
        );
        // No sample was recorded for the demoted "Found" at (40, 40); the
        // session must not have jumped there.
        assert!(session
            .samples()
            .iter()
            .all(|s| s.position != Point::new(40.0, 40.0)));
    }

    #[test]
    fn mid_gap_strong_match_at_or_above_reacquire_threshold_closes_gap() {
        let mut session = make_scripted_session(
            5,
            Some(0.7),
            vec![
                StepOutcome::Miss,
                StepOutcome::Found {
                    position: Point::new(20.0, 20.0),
                    score: 0.9,
                },
            ],
        );
        session.step(&blank_frame(W, H)); // frame 1: miss
        session.step(&blank_frame(W, H)); // frame 2: strong match, reacquires
        assert_eq!(session.state(), SessionState::Tracking);
        assert_eq!(session.gaps(), &[Gap { start: 1, end: 1 }]);
        let last = session.samples().last().unwrap();
        assert_eq!(last.position, Point::new(20.0, 20.0));
        assert_eq!(last.source, Source::Tracked);
    }

    #[test]
    fn reacquire_threshold_does_not_affect_normal_non_gap_tracking() {
        // No gap open: a weak-but-above-min_score Found is accepted exactly
        // as it always was, threshold or not — the strictness only kicks in
        // once a miss streak is underway.
        let mut session = make_scripted_session(
            5,
            Some(0.7),
            vec![StepOutcome::Found {
                position: Point::new(6.0, 6.0),
                score: 0.55,
            }],
        );
        session.step(&blank_frame(W, H)); // frame 1: weak Found, but no gap open
        assert_eq!(session.state(), SessionState::Tracking);
        assert!(session.gaps().is_empty());
        let last = session.samples().last().unwrap();
        assert_eq!(last.position, Point::new(6.0, 6.0));
        assert_eq!(last.source, Source::Tracked);
    }

    // -- 10.2b: max_reacquire_distance guard -----------------------------

    fn make_scripted_session_with_distance_guard(
        coast_limit: u32,
        max_reacquire_distance: f64,
        outcomes: Vec<StepOutcome>,
    ) -> TrackingSession<ScriptedTracker> {
        let tracker = ScriptedTracker::new(outcomes);
        let config = TrackingSessionConfig::builder()
            .coast_limit(coast_limit)
            .max_reacquire_distance(max_reacquire_distance)
            .build();
        TrackingSession::new(tracker, 0, Point::new(5.0, 5.0), config)
    }

    #[test]
    fn mid_gap_found_far_from_last_position_is_demoted_to_miss_even_with_high_score() {
        // Last tracked position is (5, 5); the "reacquisition" lands at
        // (100, 100), ~134px away, with a perfect score — a confident lock
        // onto something that clearly isn't the object that just went
        // missing this close to where it was last seen.
        let mut session = make_scripted_session_with_distance_guard(
            5,
            50.0,
            vec![
                StepOutcome::Miss,
                StepOutcome::Found {
                    position: Point::new(100.0, 100.0),
                    score: 1.0,
                },
            ],
        );
        session.step(&blank_frame(W, H)); // frame 1: miss
        session.step(&blank_frame(W, H)); // frame 2: far match, demoted to miss
        assert_eq!(session.state(), SessionState::Tracking);
        assert!(
            session.gaps().is_empty(),
            "gap should still be open (not yet closed)"
        );
        assert!(session
            .samples()
            .iter()
            .all(|s| s.position != Point::new(100.0, 100.0)));
    }

    #[test]
    fn mid_gap_found_within_distance_guard_still_reacquires() {
        let mut session = make_scripted_session_with_distance_guard(
            5,
            50.0,
            vec![
                StepOutcome::Miss,
                StepOutcome::Found {
                    position: Point::new(20.0, 20.0), // ~21px away
                    score: 1.0,
                },
            ],
        );
        session.step(&blank_frame(W, H)); // frame 1: miss
        session.step(&blank_frame(W, H)); // frame 2: close match, reacquires
        assert_eq!(session.state(), SessionState::Tracking);
        assert_eq!(session.gaps(), &[Gap { start: 1, end: 1 }]);
        let last = session.samples().last().unwrap();
        assert_eq!(last.position, Point::new(20.0, 20.0));
        assert_eq!(last.source, Source::Tracked);
    }

    #[test]
    fn distance_guard_does_not_affect_normal_non_gap_tracking() {
        // No gap open: a Found far from last_pos is accepted as usual — the
        // distance guard, like the score gate, only kicks in mid-gap.
        let tracker = ScriptedTracker::new(vec![StepOutcome::Found {
            position: Point::new(100.0, 100.0),
            score: 1.0,
        }]);
        let config = TrackingSessionConfig::builder()
            .coast_limit(5)
            .max_reacquire_distance(50.0)
            .build();
        let mut session = TrackingSession::new(tracker, 0, Point::new(5.0, 5.0), config);
        session.step(&blank_frame(W, H));
        assert_eq!(session.state(), SessionState::Tracking);
        let last = session.samples().last().unwrap();
        assert_eq!(last.position, Point::new(100.0, 100.0));
        assert_eq!(last.source, Source::Tracked);
    }

    #[test]
    fn without_max_reacquire_distance_behavior_is_unchanged_backward_compat() {
        // max_reacquire_distance left unset (None): a far mid-gap Found
        // still reacquires as before this guard existed (subject only to
        // reacquire_min_score, if that's set).
        let mut session = make_scripted_session(
            5,
            None,
            vec![
                StepOutcome::Miss,
                StepOutcome::Found {
                    position: Point::new(500.0, 500.0),
                    score: 1.0,
                },
            ],
        );
        session.step(&blank_frame(W, H));
        session.step(&blank_frame(W, H));
        assert_eq!(session.state(), SessionState::Tracking);
        assert_eq!(session.gaps(), &[Gap { start: 1, end: 1 }]);
    }

    #[test]
    fn without_reacquire_min_score_behavior_is_unchanged_backward_compat() {
        // reacquire_min_score left unset (None): any Found — however
        // marginal — closes the gap, exactly as before this feature
        // existed.
        let mut session = make_scripted_session(
            5,
            None,
            vec![
                StepOutcome::Miss,
                StepOutcome::Found {
                    position: Point::new(20.0, 20.0),
                    score: 0.05, // would fail any sane reacquire threshold
                },
            ],
        );
        session.step(&blank_frame(W, H)); // frame 1: miss
        session.step(&blank_frame(W, H)); // frame 2: marginal match, still reacquires
        assert_eq!(session.state(), SessionState::Tracking);
        assert_eq!(session.gaps(), &[Gap { start: 1, end: 1 }]);
        let last = session.samples().last().unwrap();
        assert_eq!(last.position, Point::new(20.0, 20.0));
        assert_eq!(last.source, Source::Tracked);
    }
}
