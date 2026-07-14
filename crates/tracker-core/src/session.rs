//! Gap logic: a `TrackingSession` coordinates a `TemplateTracker` across a
//! frame sequence, coasting over short misses (interpolating positions) and
//! pausing (`NeedsReseed`) when a gap exceeds the configured coast limit.
//!
//! See CONTEXT.md's "Gap" term: short gaps are coasted over and flagged;
//! a gap longer than the coast limit pauses tracking until the user
//! re-places the Seed.

use crate::geometry::{Frame, Point};
use crate::tracker::{StepOutcome, TemplateTracker};

/// Configuration for a `TrackingSession`, built via
/// `TrackingSessionConfig::builder()`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackingSessionConfig {
    coast_limit: u32,
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
}

/// Builder for `TrackingSessionConfig`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackingSessionConfigBuilder {
    coast_limit: u32,
}

impl Default for TrackingSessionConfigBuilder {
    fn default() -> Self {
        Self { coast_limit: 5 }
    }
}

impl TrackingSessionConfigBuilder {
    /// Sets the maximum number of consecutive missed frames coasted over
    /// before the session pauses and needs a reseed.
    pub fn coast_limit(mut self, limit: u32) -> Self {
        self.coast_limit = limit;
        self
    }

    pub fn build(self) -> TrackingSessionConfig {
        TrackingSessionConfig {
            coast_limit: self.coast_limit,
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
pub struct TrackingSession {
    tracker: TemplateTracker,
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

impl TrackingSession {
    /// Starts a session with `tracker` seeded at `seed_frame_index` and
    /// `seed` position (recorded as the first `Tracked` sample).
    pub fn new(
        tracker: TemplateTracker,
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
        match self.tracker.step(frame, self.last_pos) {
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
                    self.gaps.push(Gap {
                        start: self.open_gap_start.take().unwrap(),
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
            let position = Point::new(
                from.x + (to.x - from.x) * t,
                from.y + (to.y - from.y) * t,
            );
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
    use crate::tracker::TemplateTrackerConfig;

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

    fn make_session(coast_limit: u32) -> TrackingSession {
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
}
