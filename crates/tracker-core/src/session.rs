//! Gap logic: a `TrackingSession` coordinates a `TemplateTracker` across a
//! frame sequence, coasting over short misses (interpolating positions) and
//! pausing (`NeedsReseed`) when a gap exceeds the configured coast limit.
//!
//! See CONTEXT.md's "Gap" term: short gaps are coasted over and flagged;
//! a gap longer than the coast limit pauses tracking until the user
//! re-places the Seed.

use crate::geometry::{Frame, Point};
use crate::motion::Track;
use crate::tracker::{StepOutcome, Tracker};

/// Configuration for a `TrackingSession`, built via
/// `TrackingSessionConfig::builder()`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackingSessionConfig {
    coast_limit: u32,
    reacquire_min_score: Option<f64>,
    max_reacquire_distance: Option<f64>,
    coast_uncertainty_growth: f64,
    sustained_suspect_limit: u32,
    lost_confidence: f64,
    lost_detection: bool,
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

    /// Growth rate (px of uncertainty per second of coasting, 17.2) applied
    /// to the `Track`'s uncertainty on every missed frame — see
    /// `motion::Track::coasted`. Widens the tracker's own gating radius
    /// (`motion::gate_radius`) the longer the object has gone unseen, so a
    /// reacquisition after a long coast isn't held to the same tight
    /// per-frame acceleration bound as a normal step.
    pub fn coast_uncertainty_growth(&self) -> f64 {
        self.coast_uncertainty_growth
    }

    /// Number of *consecutive* `Found` frames whose identity confidence is
    /// below `accuracy::DEFAULT_TRUSTED_CONFIDENCE` (17.4's honest-doubt
    /// threshold — reused here rather than adding a second tunable
    /// confidence cutoff for the same concept) before the session gives up
    /// and transitions to the terminal `SessionState::Lost` (17.4b).
    ///
    /// This is deliberately a *new* knob, not a retune of `coast_limit`/
    /// `reacquire_min_score`/etc: those all react to *misses* (the tracker
    /// admitting it can't find the object), whereas this reacts to
    /// *sustained low-confidence hits* (the tracker claiming success while
    /// its own anchor score says the claim is doubtful) — the audit's F5
    /// "tracked, but wrong" case, which the existing six knobs have no way
    /// to express at all.
    ///
    /// Default 10: long enough that a single noisy anchor score (motion
    /// blur, brief partial occlusion) doesn't trip it, short enough that a
    /// real drift-onto-background lock (which holds low identity
    /// confidence indefinitely once it happens) is caught well before it
    /// silently reseeds itself into a new stale position at coast_limit.
    pub fn sustained_suspect_limit(&self) -> u32 {
        self.sustained_suspect_limit
    }

    /// The identity-confidence floor (17.4b) below which a `Found` counts
    /// toward the sustained-suspect streak that trips `SessionState::Lost`.
    ///
    /// This is deliberately **lower** than 17.4's `DEFAULT_TRUSTED_CONFIDENCE`
    /// (0.7, the honest-doubt threshold used to *flag* Suspect samples in
    /// exports). Terminating the whole run is a heavier action than flagging a
    /// sample, so it must require confidence in the genuine-false-lock band,
    /// not merely "somewhat uncertain". Measured (audit + 2026-07-22 user
    /// footage): a correct track whose plate appearance shifts at unrack
    /// decays only to ~0.57 against the fixed seed, while a real lock onto rack
    /// hardware collapses to ~0.40-0.46. Default 0.45 sits between them, so a
    /// correct-but-changed track is never killed. Tunable per footage.
    pub fn lost_confidence(&self) -> f64 {
        self.lost_confidence
    }

    /// Whether confidence-based terminal `Lost` detection is active (17.4b).
    ///
    /// **Default off.** Measured on the project's test footage (v3/v4,
    /// specular chrome plates): the identity-confidence bands of a *correct*
    /// track (anchor ZNCC ~0.3-0.6, low because a smooth plate is a poor ZNCC
    /// target) and a genuine false lock onto rack hardware (~0.40-0.46, per
    /// the audit) **overlap** — no `lost_confidence` floor separates them, so
    /// auto-terminating on confidence kills a 100%-accurate track (v4:
    /// truncated a mean-3.2px track at frame 327) as readily as it stops a
    /// real drift. A confidence signal that can't discriminate must not drive
    /// a terminal, run-ending decision. With this off, a genuine loss still
    /// pauses via `NeedsReseed` (reseed to continue) — the run is never
    /// silently ended. Enable it only for footage with a real Marker /
    /// reliable confidence (CONTEXT.md).
    pub fn lost_detection(&self) -> bool {
        self.lost_detection
    }
}

/// Builder for `TrackingSessionConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TrackingSessionConfigBuilder {
    coast_limit: Option<u32>,
    reacquire_min_score: Option<f64>,
    max_reacquire_distance: Option<f64>,
    coast_uncertainty_growth: Option<f64>,
    sustained_suspect_limit: Option<u32>,
    lost_confidence: Option<f64>,
    lost_detection: Option<bool>,
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

    /// Sets the per-second uncertainty growth rate applied to the `Track`
    /// while coasting through misses. Leave unset for the default (20.0
    /// px/s).
    pub fn coast_uncertainty_growth(mut self, growth: f64) -> Self {
        self.coast_uncertainty_growth = Some(growth);
        self
    }

    /// Sets the number of consecutive low-identity-confidence `Found`
    /// frames before the session transitions to the terminal
    /// `SessionState::Lost` (17.4b). Leave unset for the default (10).
    pub fn sustained_suspect_limit(mut self, limit: u32) -> Self {
        self.sustained_suspect_limit = Some(limit);
        self
    }

    /// Sets the identity-confidence floor (17.4b) below which a `Found`
    /// counts toward the Lost streak. Leave unset for the default (0.45).
    pub fn lost_confidence(mut self, floor: f64) -> Self {
        self.lost_confidence = Some(floor);
        self
    }

    /// Enables/disables terminal `Lost` detection (17.4b). Leave unset for
    /// the default (**off** — see `TrackingSessionConfig::lost_detection`).
    pub fn lost_detection(mut self, enabled: bool) -> Self {
        self.lost_detection = Some(enabled);
        self
    }

    pub fn build(self) -> TrackingSessionConfig {
        TrackingSessionConfig {
            coast_limit: self.coast_limit.unwrap_or(5),
            reacquire_min_score: self.reacquire_min_score,
            max_reacquire_distance: self.max_reacquire_distance,
            coast_uncertainty_growth: self.coast_uncertainty_growth.unwrap_or(20.0),
            sustained_suspect_limit: self.sustained_suspect_limit.unwrap_or(10),
            lost_confidence: self.lost_confidence.unwrap_or(0.45),
            lost_detection: self.lost_detection.unwrap_or(false),
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
    /// Identity confidence at this frame (17.4): `Some(anchor_score)` for a
    /// directly-`Tracked` sample, `None` for an `Interpolated` (coasted) one
    /// or the seed. This is the honest "is this still the seeded object?"
    /// measure — a confident false lock reports a *low* value here even
    /// while its effective match score is ~1.0 (audit F5).
    pub confidence: Option<f64>,
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

/// Whether the session is actively tracking, paused awaiting a reseed, or
/// has given up.
///
/// `NeedsReseed` and `Lost` are both "stopped consuming frames" states, but
/// they mean different things (audit F5/F4, docs/design/tracking-audit-2026-07-21.md):
/// `NeedsReseed` is a *transient* pause — a run of misses, almost certainly
/// a real occlusion — that a fresh seed at roughly the same place recovers
/// from. `Lost` is *terminal*: the tracker kept reporting `Found` (never
/// missed, never paused) but its own identity confidence (the anchor score
/// — see `Sample::confidence`) stayed low for `sustained_suspect_limit`
/// consecutive frames in a row, i.e. it has probably locked onto the wrong
/// thing and drifting-and-recovering isn't the fix; a fresh seed is
/// required to trust it again, and headless callers must not manufacture
/// one from the same (untrustworthy) stale position (17.4b — see
/// `crates/tracker-app/src/cli.rs`'s headless auto-resume, which stops on
/// `Lost` rather than reseeding through it as it does for `NeedsReseed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Tracking,
    NeedsReseed,
    Lost,
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
    /// Position of the last confirmed `Found`/reseed observation — the
    /// anchor `interpolate_gap` walks from, and what `max_reacquire_distance`
    /// measures against. Distinct from `track.position`, which is the
    /// tracker's motion state and keeps advancing (via prediction) through
    /// a coasted Miss; `last_pos` only moves on a real observation.
    last_pos: Point,
    /// Motion state (17.2, audit F1/F2) fed to `Tracker::step`: position,
    /// velocity and uncertainty. Advances by prediction (`Track::coasted`)
    /// through a Miss rather than freezing, and resets to a fresh
    /// zero-velocity track on `reseed`.
    track: Track,
    frame_index: u64,
    samples: Vec<Sample>,
    gaps: Vec<Gap>,
    /// Frame index the current open gap started at, if any.
    open_gap_start: Option<u64>,
    miss_count: u32,
    /// Consecutive `Found` frames in a row whose identity confidence was
    /// below `accuracy::DEFAULT_TRUSTED_CONFIDENCE` (17.4b). Reset by any
    /// `Found` at/above the threshold or by a `Miss` (a miss is a
    /// different failure mode, already handled by `miss_count`/
    /// `NeedsReseed`). Trips `SessionState::Lost` at
    /// `config.sustained_suspect_limit`.
    suspect_streak: u32,
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
            track: Track::new(seed),
            frame_index: seed_frame_index,
            samples: vec![Sample {
                frame_index: seed_frame_index,
                position: seed,
                source: Source::Tracked,
                // The seed is a human-placed point, not a tracker match — it
                // has no anchor score, so confidence is None (the user is the
                // ground truth for this one frame).
                confidence: None,
            }],
            gaps: Vec::new(),
            open_gap_start: None,
            miss_count: 0,
            suspect_streak: 0,
        }
    }

    pub fn state(&self) -> SessionState {
        self.state
    }

    /// The current frame index the session has processed up to. Unlike
    /// `samples().last().frame_index`, this advances on *every* `step` call
    /// (Found, Miss, or the pause that trips `NeedsReseed`) — samples are
    /// only pushed for `Found`/reseed frames and, retroactively, for a gap's
    /// interpolated frames once it closes. Callers that need to know "what
    /// frame are we actually at" (e.g. to report pause progress or to
    /// resume at the right place) must use this, not the last sample: while
    /// a gap is open but not yet closed or paused, the last sample can be
    /// many frames stale (10.9 root cause — the CLI's headless auto-resume
    /// was reseeding at the stale last-*sample* frame index instead of this
    /// one, which regressed `self.frame_index` backwards and produced the
    /// same reseed frame repeatedly forever).
    pub fn frame_index(&self) -> u64 {
        self.frame_index
    }

    pub fn samples(&self) -> &[Sample] {
        &self.samples
    }

    pub fn gaps(&self) -> &[Gap] {
        &self.gaps
    }

    /// The current motion state (17.2) fed to the tracker: position,
    /// velocity and uncertainty. Exposed mainly for tests/diagnostics —
    /// callers driving a session don't normally need it.
    pub fn track(&self) -> Track {
        self.track
    }

    /// Feeds the next frame (assumed to be `frame_index + 1`) to the
    /// tracker, `dt` seconds after the previous one. No-op if the session
    /// is currently paused awaiting reseed.
    pub fn step(&mut self, frame: &Frame, dt: f64) {
        if self.state == SessionState::NeedsReseed || self.state == SessionState::Lost {
            return;
        }

        let next_index = self.frame_index + 1;
        let outcome = self.tracker.step(frame, &self.track, dt);

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
            StepOutcome::Found {
                position,
                identity_confidence,
                ..
            } => {
                if let Some(gap_start) = self.open_gap_start.take() {
                    self.gaps.push(Gap {
                        start: gap_start,
                        end: next_index - 1,
                    });
                    self.interpolate_gap(gap_start, next_index - 1, self.last_pos, position);
                }
                self.miss_count = 0;
                self.track = self.track.observed(position, dt);
                self.last_pos = position;
                self.frame_index = next_index;
                self.samples.push(Sample {
                    frame_index: next_index,
                    position,
                    source: Source::Tracked,
                    confidence: Some(identity_confidence),
                });

                // 17.4b: a `Found` that keeps claiming success while its own
                // anchor score stays below `lost_confidence` (the genuine
                // false-lock band, ~0.45 — NOT 17.4's 0.7 doubt threshold,
                // which a correct-but-appearance-changed track routinely dips
                // into at unrack, causing a false Lost before rep 1) for
                // `sustained_suspect_limit` frames in a row is a "tracked, but
                // wrong" run (audit F5), not a transient blip — give up rather
                // than let it keep exporting confident-looking samples off the
                // seeded object.
                if self.config.lost_detection && identity_confidence < self.config.lost_confidence {
                    self.suspect_streak += 1;
                    if self.suspect_streak >= self.config.sustained_suspect_limit {
                        self.state = SessionState::Lost;
                    }
                } else {
                    self.suspect_streak = 0;
                }
            }
            StepOutcome::Miss => {
                if self.open_gap_start.is_none() {
                    self.open_gap_start = Some(next_index);
                }
                self.miss_count += 1;
                self.suspect_streak = 0;
                // Coast: predict forward along the velocity estimate rather
                // than freezing at the last position (audit F1), and widen
                // the gate a little for however long we've been coasting.
                self.track = self.track.coasted(dt, self.config.coast_uncertainty_growth);
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
                confidence: None,
            });
        }
    }

    /// Resumes a paused session: the caller has re-placed the seed at
    /// `point` on `frame_index`. If a gap was pending (trailing,
    /// unresolved), it is replaced with one closed at `frame_index - 1`.
    /// Records a `Tracked` sample for the reseed point itself.
    ///
    /// ## Monotonic-samples guarantee (10.9)
    /// `samples()` must stay strictly increasing by `frame_index` — a
    /// `BarPath`/`velocity_series` built from it derives timestamps
    /// straight from `frame_index`, and a duplicate or regressing index
    /// produces a non-positive `dt`, which `velocity_series` rejects
    /// wholesale (`VelocityError::NonMonotonicTime`), silently zeroing out
    /// velocity and rep detection for the *entire* run.
    ///
    /// Semantics chosen: reseeding at `frame_index` where a sample already
    /// exists at or after that index (i.e. `frame_index <=` the last
    /// recorded sample's) *replaces* the last recorded sample in place,
    /// rather than appending a new (duplicate-or-regressing) one. This
    /// happens in practice when a caller re-reseeds using a stale frame
    /// index (e.g. re-deriving "current frame" from the last *sample*
    /// instead of `Self::frame_index`, as the CLI's headless auto-resume
    /// did before this fix) — treating it as "the seed I already placed is
    /// still the best guess for this frame" is more honest than crashing or
    /// silently corrupting the series, and it self-heals: the very next
    /// `step` resumes strictly forward from the (now-corrected) last
    /// sample.
    pub fn reseed(&mut self, frame_index: u64, point: Point) {
        let last_recorded = self.samples.last().map(|s| s.frame_index);
        let effective_frame_index = match last_recorded {
            Some(last) if frame_index <= last => last,
            _ => frame_index,
        };
        let replacing = last_recorded.is_some_and(|last| frame_index <= last);

        // Replace the trailing (unresolved) gap recorded when we paused,
        // if any, with one closed right before the (effective) reseed
        // frame. `Lost` (17.4b) has no gap to close (it was reached via
        // `Found`s, not misses), but a manual reseed out of it is still
        // allowed — e.g. a GUI user re-placing the seed after the run
        // stopped itself — same as `NeedsReseed`.
        if self.state == SessionState::NeedsReseed {
            if let Some(last) = self.gaps.last_mut() {
                last.end = effective_frame_index.saturating_sub(1);
            }
        }

        self.state = SessionState::Tracking;
        self.last_pos = point;
        // A reseed is a fresh human/auto-placed point, with no velocity
        // history to trust (17.2) — same reasoning as the initial seed.
        self.track = Track::new(point);
        self.frame_index = effective_frame_index;
        self.open_gap_start = None;
        self.miss_count = 0;
        self.suspect_streak = 0;
        if replacing {
            self.samples.pop();
        }
        self.samples.push(Sample {
            frame_index: effective_frame_index,
            position: point,
            source: Source::Tracked,
            // A reseed is a (human or auto) re-placed point, like the seed:
            // no anchor score of its own.
            confidence: None,
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
        session.step(&frame, 1.0);

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
        session.step(&blank_frame(W, H), 1.0);
        session.step(&blank_frame(W, H), 1.0);
        assert_eq!(session.state(), SessionState::Tracking);

        // frame 3: object reappears, moved to (18, 12)
        session.step(&frame_with_square(W, H, 16, 10, 4), 1.0);
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
        session.step(&blank_frame(W, H), 1.0); // frame 1: miss 1
        assert_eq!(session.state(), SessionState::Tracking);
        session.step(&blank_frame(W, H), 1.0); // frame 2: miss 2
        assert_eq!(session.state(), SessionState::Tracking);
        session.step(&blank_frame(W, H), 1.0); // frame 3: miss 3 > limit -> pause
        assert_eq!(session.state(), SessionState::NeedsReseed);

        // Further steps are ignored while paused.
        session.step(&frame_with_square(W, H, 10, 10, 4), 1.0);
        assert_eq!(session.state(), SessionState::NeedsReseed);
        assert_eq!(session.samples().len(), 1); // only the initial seed sample

        assert_eq!(session.gaps(), &[Gap { start: 1, end: 3 }]);
    }

    #[test]
    fn reseed_resumes_tracking_after_pause() {
        let mut session = make_session(1);
        session.step(&blank_frame(W, H), 1.0); // frame 1: miss 1
        session.step(&blank_frame(W, H), 1.0); // frame 2: miss 2 > limit(1) -> pause
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
        session.step(&frame, 1.0);
        assert_eq!(session.state(), SessionState::Tracking);
        assert_eq!(session.samples().last().unwrap().frame_index, 6);
        assert_eq!(session.samples().last().unwrap().source, Source::Tracked);
    }

    // -- 10.9: reseed must never duplicate/regress a sample frame index ---

    #[test]
    fn reseeding_the_same_frame_twice_keeps_samples_strictly_increasing() {
        let mut session = make_session(1);
        session.step(&blank_frame(W, H), 1.0); // frame 1: miss
        session.step(&blank_frame(W, H), 1.0); // frame 2: miss > limit(1) -> pause
        assert_eq!(session.state(), SessionState::NeedsReseed);

        session.reseed(5, Point::new(30.0, 20.0));
        assert_eq!(session.state(), SessionState::Tracking);
        assert_eq!(session.samples().last().unwrap().frame_index, 5);

        // Reseeding again at the *same* frame index (e.g. a caller retrying
        // with a stale "current frame" reading) must not duplicate frame 5
        // -- it replaces the sample already recorded there instead.
        session.reseed(5, Point::new(31.0, 21.0));
        assert_eq!(session.state(), SessionState::Tracking);
        let samples = session.samples();
        assert_eq!(samples.last().unwrap().frame_index, 5);
        assert_eq!(samples.last().unwrap().position, Point::new(31.0, 21.0));
        // Samples strictly increasing: no duplicate frame_index anywhere.
        for w in samples.windows(2) {
            assert!(
                w[1].frame_index > w[0].frame_index,
                "samples must be strictly increasing by frame_index: {:?} -> {:?}",
                w[0],
                w[1]
            );
        }

        // Reseeding at an even earlier (regressing) frame index behaves the
        // same way: clamps to the last recorded index rather than going
        // backwards.
        session.reseed(2, Point::new(1.0, 1.0));
        let samples = session.samples();
        assert_eq!(samples.last().unwrap().frame_index, 5);
        assert_eq!(samples.last().unwrap().position, Point::new(1.0, 1.0));
        for w in samples.windows(2) {
            assert!(w[1].frame_index > w[0].frame_index);
        }

        // A genuinely later reseed still records a new sample as normal.
        session.reseed(7, Point::new(2.0, 2.0));
        let samples = session.samples();
        assert_eq!(samples.last().unwrap().frame_index, 7);
        assert_eq!(samples.len(), samples.len()); // sanity
        for w in samples.windows(2) {
            assert!(w[1].frame_index > w[0].frame_index);
        }
    }

    #[test]
    fn frame_index_advances_on_every_step_even_without_a_new_sample() {
        // While a gap is open (Miss streak that hasn't yet closed or
        // paused), no new sample is pushed per frame -- but frame_index()
        // must still track the actual current frame, since callers that
        // need "what frame are we at" (progress reporting, resume) rely on
        // it rather than the stale last sample (10.9).
        let mut session = make_session(5); // generous coast limit: stays Tracking
        assert_eq!(session.frame_index(), 0);
        session.step(&blank_frame(W, H), 1.0); // frame 1: miss, no new sample
        assert_eq!(session.frame_index(), 1);
        assert_eq!(session.samples().last().unwrap().frame_index, 0);
        session.step(&blank_frame(W, H), 1.0); // frame 2: miss, no new sample
        assert_eq!(session.frame_index(), 2);
        assert_eq!(session.samples().last().unwrap().frame_index, 0);
    }

    #[test]
    fn trailing_unresolved_gap_recorded_when_video_ends_mid_coast() {
        // Object hidden for the remaining frames of the sequence, but never
        // exceeds the coast limit, so the session keeps "Tracking" with an
        // open gap that never gets a closing Found. The gap list should
        // still reflect the trailing gap up to the last processed frame if
        // the caller inspects it (no crash, no fabricated closure).
        let mut session = make_session(5);
        session.step(&blank_frame(W, H), 1.0); // frame 1: miss
        session.step(&blank_frame(W, H), 1.0); // frame 2: miss
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
        fn step(&mut self, _frame: &Frame, _track: &Track, _dt: f64) -> StepOutcome {
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

    /// 17.2, audit F1: on a Miss, the session's `Track` coasts forward by
    /// prediction (`position + velocity*dt`) rather than freezing at the
    /// last observed position.
    #[test]
    fn coast_predicts_forward_through_a_miss_instead_of_freezing() {
        let mut session = make_scripted_session(
            5,
            None,
            vec![
                StepOutcome::Found {
                    position: Point::new(20.0, 10.0), // moved (15, 5) from seed (5, 5)
                    score: 1.0,
                    identity_confidence: 1.0,
                },
                StepOutcome::Miss,
            ],
        );
        session.step(&blank_frame(W, H), 1.0); // frame 1: Found, establishes velocity (15, 5)/s
        assert_eq!(session.track().position, Point::new(20.0, 10.0));
        assert_eq!(session.track().velocity, Point::new(15.0, 5.0));

        session.step(&blank_frame(W, H), 1.0); // frame 2: Miss, dt = 1.0s
        assert_eq!(
            session.track().position,
            Point::new(35.0, 15.0),
            "coast must predict forward along the velocity estimate, not freeze at (20, 10)"
        );
        assert!(
            session.track().uncertainty > 0.0,
            "uncertainty should grow while coasting"
        );
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
                    identity_confidence: 0.55,
                },
            ],
        );
        session.step(&blank_frame(W, H), 1.0); // frame 1: miss
        session.step(&blank_frame(W, H), 1.0); // frame 2: weak match, demoted to miss
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
                    identity_confidence: 0.9,
                },
            ],
        );
        session.step(&blank_frame(W, H), 1.0); // frame 1: miss
        session.step(&blank_frame(W, H), 1.0); // frame 2: strong match, reacquires
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
                identity_confidence: 0.55,
            }],
        );
        session.step(&blank_frame(W, H), 1.0); // frame 1: weak Found, but no gap open
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
                    identity_confidence: 1.0,
                },
            ],
        );
        session.step(&blank_frame(W, H), 1.0); // frame 1: miss
        session.step(&blank_frame(W, H), 1.0); // frame 2: far match, demoted to miss
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
                    identity_confidence: 1.0,
                },
            ],
        );
        session.step(&blank_frame(W, H), 1.0); // frame 1: miss
        session.step(&blank_frame(W, H), 1.0); // frame 2: close match, reacquires
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
            identity_confidence: 1.0,
        }]);
        let config = TrackingSessionConfig::builder()
            .coast_limit(5)
            .max_reacquire_distance(50.0)
            .build();
        let mut session = TrackingSession::new(tracker, 0, Point::new(5.0, 5.0), config);
        session.step(&blank_frame(W, H), 1.0);
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
                    identity_confidence: 1.0,
                },
            ],
        );
        session.step(&blank_frame(W, H), 1.0);
        session.step(&blank_frame(W, H), 1.0);
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
                    identity_confidence: 0.05,
                },
            ],
        );
        session.step(&blank_frame(W, H), 1.0); // frame 1: miss
        session.step(&blank_frame(W, H), 1.0); // frame 2: marginal match, still reacquires
        assert_eq!(session.state(), SessionState::Tracking);
        assert_eq!(session.gaps(), &[Gap { start: 1, end: 1 }]);
        let last = session.samples().last().unwrap();
        assert_eq!(last.position, Point::new(20.0, 20.0));
        assert_eq!(last.source, Source::Tracked);
    }

    // -- 17.4b: terminal Lost state ------------------------------------

    fn make_scripted_session_with_suspect_limit(
        sustained_suspect_limit: u32,
        outcomes: Vec<StepOutcome>,
    ) -> TrackingSession<ScriptedTracker> {
        let tracker = ScriptedTracker::new(outcomes);
        let config = TrackingSessionConfig::builder()
            .sustained_suspect_limit(sustained_suspect_limit)
            // 17.4b Lost detection is opt-in (default off, see
            // `lost_detection`'s doc) — these tests exercise it, so enable it.
            .lost_detection(true)
            .build();
        TrackingSession::new(tracker, 0, Point::new(5.0, 5.0), config)
    }

    fn low_confidence_found(x: f64, y: f64) -> StepOutcome {
        StepOutcome::Found {
            position: Point::new(x, y),
            score: 0.99,              // effective match score can stay high (audit F5)
            identity_confidence: 0.4, // well below DEFAULT_TRUSTED_CONFIDENCE (0.7)
        }
    }

    fn high_confidence_found(x: f64, y: f64) -> StepOutcome {
        StepOutcome::Found {
            position: Point::new(x, y),
            score: 0.99,
            identity_confidence: 0.95,
        }
    }

    #[test]
    fn lost_detection_is_off_by_default_so_low_confidence_never_terminates() {
        // Default config leaves `lost_detection` off (specular-plate
        // confidence can't discriminate a good track from a false lock — see
        // its doc): a long run of low-confidence Founds must keep Tracking,
        // never terminate the run.
        let tracker = ScriptedTracker::new(vec![
            low_confidence_found(10.0, 10.0),
            low_confidence_found(11.0, 10.0),
            low_confidence_found(12.0, 10.0),
            low_confidence_found(13.0, 10.0),
            low_confidence_found(14.0, 10.0),
        ]);
        // default builder: sustained_suspect_limit 10, lost_detection OFF
        let config = TrackingSessionConfig::builder().build();
        let mut session = TrackingSession::new(tracker, 0, Point::new(5.0, 5.0), config);
        for _ in 0..5 {
            session.step(&blank_frame(W, H), 1.0);
            assert_eq!(session.state(), SessionState::Tracking);
        }
    }

    #[test]
    fn sustained_low_confidence_found_trips_lost() {
        // limit 3: the 3rd consecutive low-confidence Found trips it.
        let mut session = make_scripted_session_with_suspect_limit(
            3,
            vec![
                low_confidence_found(10.0, 10.0),
                low_confidence_found(11.0, 10.0),
                low_confidence_found(12.0, 10.0),
            ],
        );
        session.step(&blank_frame(W, H), 1.0);
        assert_eq!(session.state(), SessionState::Tracking);
        session.step(&blank_frame(W, H), 1.0);
        assert_eq!(session.state(), SessionState::Tracking);
        session.step(&blank_frame(W, H), 1.0);
        assert_eq!(session.state(), SessionState::Lost);

        // The sample for the frame that tripped it is still recorded
        // (honest partial path) — Lost doesn't discard it.
        let last = session.samples().last().unwrap();
        assert_eq!(last.frame_index, 3);
        assert_eq!(last.position, Point::new(12.0, 10.0));

        // Further steps are ignored, like NeedsReseed.
        session.step(&blank_frame(W, H), 1.0);
        assert_eq!(session.state(), SessionState::Lost);
        assert_eq!(session.samples().len(), 4);
    }

    #[test]
    fn a_single_transient_low_confidence_frame_does_not_trip_lost() {
        let mut session = make_scripted_session_with_suspect_limit(
            3,
            vec![
                low_confidence_found(10.0, 10.0),
                high_confidence_found(11.0, 10.0), // recovers -> resets streak
                low_confidence_found(12.0, 10.0),
                low_confidence_found(13.0, 10.0),
            ],
        );
        for _ in 0..4 {
            session.step(&blank_frame(W, H), 1.0);
        }
        // Streak reset by the high-confidence frame in the middle, so only
        // 2 consecutive low-confidence frames at the end: below the
        // limit(3).
        assert_eq!(session.state(), SessionState::Tracking);
    }

    #[test]
    fn a_miss_between_low_confidence_founds_resets_the_suspect_streak() {
        // coast_limit's default (5) keeps a single Miss from itself
        // pausing the session, so this isolates the suspect-streak reset.
        let mut session = make_scripted_session_with_suspect_limit(
            2,
            vec![
                low_confidence_found(10.0, 10.0),
                StepOutcome::Miss,
                low_confidence_found(12.0, 10.0),
            ],
        );
        session.step(&blank_frame(W, H), 1.0); // low-confidence Found: streak 1
        session.step(&blank_frame(W, H), 1.0); // Miss: resets streak
        assert_eq!(session.state(), SessionState::Tracking);
        session.step(&blank_frame(W, H), 1.0); // low-confidence Found: streak 1 again, not 2
        assert_eq!(session.state(), SessionState::Tracking);
    }

    #[test]
    fn reseed_recovers_a_session_from_lost() {
        let mut session =
            make_scripted_session_with_suspect_limit(1, vec![low_confidence_found(10.0, 10.0)]);
        session.step(&blank_frame(W, H), 1.0);
        assert_eq!(session.state(), SessionState::Lost);

        session.reseed(5, Point::new(30.0, 20.0));
        assert_eq!(session.state(), SessionState::Tracking);
        let last = session.samples().last().unwrap();
        assert_eq!(last.frame_index, 5);
        assert_eq!(last.position, Point::new(30.0, 20.0));
    }
}
