//! Rep segmentation (task 5.3): splits a `VelocitySample` series into
//! `Rep`s (see CONTEXT.md, "Rep") by the sign of `vy`.
//!
//! Per `velocity.rs`'s documented axis convention (image y grows downward),
//! a descending bar (eccentric phase) has `vy > 0` and an ascending bar
//! (concentric phase) has `vy < 0`. A rep is one eccentric phase followed
//! by one concentric phase.
//!
//! ## Robustness
//! Real velocity series are noisy, so a naive "first sign change starts a
//! rep" approach mis-fires constantly. This module instead:
//!
//! 1. **Dead-band / hysteresis** — a sample only counts as `Descending` or
//!    `Ascending` if `|vy|` exceeds `min_velocity` (config). Samples below
//!    that stay `Idle`, which covers both genuine rest between reps and
//!    the near-zero crossing at the bottom of a rep (or top, between
//!    reps).
//! 2. **Minimum phase duration** — a run of consecutive `Descending` (or
//!    `Ascending`) samples shorter than `min_phase_duration_seconds` is
//!    jitter, not a real phase, and gets folded back into `Idle`.
//! 3. **Idle is free-form** — `Idle` runs of any length/position are
//!    allowed between and inside reps (rest between reps, or a pause at
//!    the bottom of a squat); they never need to meet a minimum duration.
//! 4. **Minimum displacement** — each phase of a candidate rep must cover
//!    at least `min_displacement` of vertical travel (integrated from the
//!    velocity series); slow sustained drift (walkout/unrack shuffling)
//!    passes the two gates above but only travels a few pixels, and is
//!    discarded here rather than counted.
//! 5. **Incomplete trailing reps are dropped** — an eccentric phase with no
//!    following concentric phase (e.g. the clip ends mid-descent) is not
//!    emitted, since depth/velocity metrics (5.4) for it would be bogus.
//!
//! ## Indices
//! `Rep` fields are indices into the `velocity` slice passed to
//! `segment_reps` (not raw video frame numbers) — callers who need a frame
//! number can look up `velocity[idx].frame_index`.

use crate::velocity::VelocitySample;

/// Configuration for `segment_reps`, built via `RepSegmentationConfig::builder()`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RepSegmentationConfig {
    min_velocity: f64,
    min_phase_duration_seconds: f64,
    min_displacement: f64,
}

impl RepSegmentationConfig {
    /// Starts a builder with sensible defaults; see `default_config`.
    pub fn builder() -> RepSegmentationConfigBuilder {
        RepSegmentationConfigBuilder::default()
    }

    /// Sensible defaults, assuming ~30-60fps footage and a 2-6s squat rep:
    /// - `min_velocity` 5.0: a dead-band on `|vy|` in whatever unit the
    ///   `VelocitySample`s carry (px/s or m/s per `Calibration`). 5.0 is
    ///   tuned for the common uncalibrated px/s case; callers working in
    ///   m/s (bar speeds are typically well under 1-2 m/s) should override
    ///   this to something like `0.02`-`0.05` m/s.
    /// - `min_phase_duration_seconds` 0.15: comfortably shorter than a real
    ///   eccentric/concentric phase (roughly 1-3s each within a 2-6s rep)
    ///   but long enough to reject single/double-frame jitter even at
    ///   60fps (0.15s is ~9 frames at 60fps, ~4-5 at 30fps).
    /// - `min_displacement` 40.0: minimum vertical travel each of a rep's
    ///   eccentric and concentric phases must cover, in whatever unit the
    ///   positions feeding the `VelocitySample`s carry (px uncalibrated, m
    ///   per `Calibration`). Guards against the walkout/unrack phantom-rep
    ///   bug (task 15.1): slow sustained setup drift passes both the
    ///   velocity dead-band and the phase-duration gate but only travels a
    ///   handful of pixels. In our test footage a real squat covers ~180 px
    ///   of ROM while walkout wobble stays well under 50 px, so 40 px sits
    ///   comfortably between them for typical framing; callers working in
    ///   meters should override to something like `0.15` m.
    pub fn default_config() -> Self {
        Self {
            min_velocity: 5.0,
            min_phase_duration_seconds: 0.15,
            min_displacement: 40.0,
        }
    }

    /// Dead-band threshold on `|vy|` below which a sample is `Idle`.
    pub fn min_velocity(&self) -> f64 {
        self.min_velocity
    }

    /// Minimum duration (seconds) a `Descending`/`Ascending` run must span
    /// to count as a real phase rather than jitter.
    pub fn min_phase_duration_seconds(&self) -> f64 {
        self.min_phase_duration_seconds
    }

    /// Minimum vertical travel (position units: px or m) each phase of a
    /// candidate rep must cover; candidates below it are discarded.
    pub fn min_displacement(&self) -> f64 {
        self.min_displacement
    }
}

impl Default for RepSegmentationConfig {
    fn default() -> Self {
        Self::default_config()
    }
}

/// Builder for `RepSegmentationConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RepSegmentationConfigBuilder {
    inner: RepSegmentationConfig,
}

impl RepSegmentationConfigBuilder {
    pub fn min_velocity(mut self, threshold: f64) -> Self {
        self.inner.min_velocity = threshold;
        self
    }

    pub fn min_phase_duration_seconds(mut self, seconds: f64) -> Self {
        self.inner.min_phase_duration_seconds = seconds;
        self
    }

    pub fn min_displacement(mut self, displacement: f64) -> Self {
        self.inner.min_displacement = displacement;
        self
    }

    pub fn build(self) -> RepSegmentationConfig {
        self.inner
    }
}

/// One repetition of the lift (see CONTEXT.md, "Rep"), as indices into the
/// `velocity` slice passed to `segment_reps`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rep {
    /// Index of the first sample of the eccentric (descent) phase.
    pub eccentric_start: usize,
    /// Index of the turnaround: the last descending sample if the descent
    /// runs straight into the ascent, or the midpoint of an `Idle` pause
    /// between them if there's a plateau at the bottom.
    pub bottom: usize,
    /// Index of the last sample of the concentric (ascent) phase.
    pub concentric_end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Descending,
    Ascending,
}

#[derive(Debug, Clone, Copy)]
struct Run {
    phase: Phase,
    start: usize,
    end: usize, // inclusive
}

/// Segments `velocity` into `Rep`s by the sign of `vy` (see module docs for
/// the noise-robustness rules). Returns reps in chronological order;
/// trailing incomplete reps (descent with no following ascent) are
/// dropped.
pub fn segment_reps(velocity: &[VelocitySample], config: RepSegmentationConfig) -> Vec<Rep> {
    if velocity.len() < 2 {
        return Vec::new();
    }

    let raw_labels: Vec<Phase> = velocity
        .iter()
        .map(|s| {
            if s.vy > config.min_velocity {
                Phase::Descending
            } else if s.vy < -config.min_velocity {
                Phase::Ascending
            } else {
                Phase::Idle
            }
        })
        .collect();

    let runs = group_runs(&raw_labels);

    // Fold Descending/Ascending runs shorter than min_phase_duration back
    // into Idle (jitter, not a real phase).
    let mut filtered_labels = raw_labels;
    for run in &runs {
        if run.phase == Phase::Idle {
            continue;
        }
        let duration = velocity[run.end].t_seconds - velocity[run.start].t_seconds;
        if duration < config.min_phase_duration_seconds {
            for label in filtered_labels.iter_mut().take(run.end + 1).skip(run.start) {
                *label = Phase::Idle;
            }
        }
    }

    // Regroup now that short runs have been folded into Idle, so adjacent
    // Idle runs merge into one.
    let runs = group_runs(&filtered_labels);

    let mut reps = Vec::new();
    let mut i = 0;
    while i < runs.len() {
        if runs[i].phase != Phase::Descending {
            i += 1;
            continue;
        }

        let eccentric_start = runs[i].start;
        let mut bottom = runs[i].end;
        let mut j = i + 1;

        if j < runs.len() && runs[j].phase == Phase::Idle {
            bottom = (runs[j].start + runs[j].end) / 2;
            j += 1;
        }

        if j < runs.len() && runs[j].phase == Phase::Ascending {
            // Displacement gate (task 15.1): each phase of a real rep must
            // cover a minimum vertical travel. Per-phase (rather than total
            // ROM) so that an asymmetric candidate — e.g. a long slow drift
            // down with only a tiny bounce back up — is rejected too; for a
            // genuine rep the two phases cover roughly the same distance,
            // and any idle pause at the bottom contributes ~nothing, so
            // per-phase and total-ROM agree on real reps. Undersized
            // candidates are discarded outright (both phases consumed),
            // never merged into a neighbor.
            let eccentric_travel = run_displacement(velocity, &runs[i]);
            let concentric_travel = run_displacement(velocity, &runs[j]);
            if eccentric_travel >= config.min_displacement
                && concentric_travel >= config.min_displacement
            {
                reps.push(Rep {
                    eccentric_start,
                    bottom,
                    concentric_end: runs[j].end,
                });
            }
            i = j + 1;
        } else {
            // Descent with no following ascent: incomplete trailing rep,
            // not emitted. Skip past whatever we scanned.
            i = j;
        }
    }

    reps
}

/// Absolute vertical travel over a run, by trapezoidal integration of `vy`
/// over the run's sample times. Units follow the samples (px or m).
fn run_displacement(velocity: &[VelocitySample], run: &Run) -> f64 {
    let mut displacement = 0.0;
    for k in run.start..run.end {
        let dt = velocity[k + 1].t_seconds - velocity[k].t_seconds;
        displacement += 0.5 * (velocity[k].vy + velocity[k + 1].vy) * dt;
    }
    displacement.abs()
}

/// Groups a label sequence into contiguous runs.
fn group_runs(labels: &[Phase]) -> Vec<Run> {
    let mut runs = Vec::new();
    let mut idx = 0;
    while idx < labels.len() {
        let phase = labels[idx];
        let start = idx;
        while idx < labels.len() && labels[idx] == phase {
            idx += 1;
        }
        runs.push(Run {
            phase,
            start,
            end: idx - 1,
        });
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::velocity::VelocityUnit;

    /// Builds a `VelocitySample` series from a list of `vy` values, one
    /// sample per 1/30s frame (30fps), with `vx`/`speed` unused (0.0).
    fn series(vys: &[f64]) -> Vec<VelocitySample> {
        vys.iter()
            .enumerate()
            .map(|(i, &vy)| VelocitySample {
                frame_index: i as u64,
                t_seconds: i as f64 / 30.0,
                vx: 0.0,
                vy,
                speed: vy.abs(),
                unit: VelocityUnit::PixelsPerSecond,
                from_interpolated: false,
            })
            .collect()
    }

    fn default_config() -> RepSegmentationConfig {
        RepSegmentationConfig::builder()
            .min_velocity(5.0)
            .min_phase_duration_seconds(0.15)
            .build()
    }

    /// A single clean rep: ~1s idle, ~1s descent ramping vy up to 400,
    /// ~1s ascent ramping vy down to -400, ~1s idle. At 30fps that's 120
    /// samples total, comfortably above the min phase duration, and each
    /// phase integrates to ~190 px of travel — realistic (a real squat in
    /// our test footage covers ~180 px of ROM) and comfortably above the
    /// 40 px `min_displacement` gate. (Pre-15.1 this ramped to only 50,
    /// i.e. ~25 px of travel, which no real rep has.)
    fn clean_rep(idle_before: usize, idle_after: usize) -> Vec<f64> {
        let mut vys = vec![0.0; idle_before];
        for i in 0..30 {
            vys.push(400.0 * (i as f64 / 29.0)); // descent: ramp 0 -> 400
        }
        for i in 0..30 {
            vys.push(-400.0 * (i as f64 / 29.0)); // ascent: ramp 0 -> -400
        }
        vys.extend(vec![0.0; idle_after]);
        vys
    }

    #[test]
    fn single_clean_rep_is_detected() {
        let vys = clean_rep(30, 30);
        let velocity = series(&vys);
        let reps = segment_reps(&velocity, default_config());
        assert_eq!(reps.len(), 1);
        let rep = reps[0];
        // Descent starts once the ramp crosses the 5.0 dead-band, a few
        // samples after the leading idle block ends at idx 30.
        assert!(rep.eccentric_start >= 30 && rep.eccentric_start <= 35);
        // Ascent ends a few samples before the ramp re-enters the
        // dead-band, just before the trailing idle block starts at idx 90.
        assert!(rep.concentric_end >= 85 && rep.concentric_end < 90);
        // Bottom sits at the descent/ascent boundary (idx ~59/60).
        assert!(rep.bottom >= 55 && rep.bottom <= 64);
    }

    #[test]
    fn three_reps_are_all_detected() {
        let mut vys = vec![0.0; 20];
        for _ in 0..3 {
            vys.extend(clean_rep(0, 20));
        }
        let velocity = series(&vys);
        let reps = segment_reps(&velocity, default_config());
        assert_eq!(reps.len(), 3);
        // Reps are in chronological order and non-overlapping.
        for pair in reps.windows(2) {
            assert!(pair[0].concentric_end < pair[1].eccentric_start);
        }
    }

    #[test]
    fn noise_only_jitter_yields_zero_reps() {
        // Tiny jitter around zero, well under the 5.0 min_velocity.
        let vys: Vec<f64> = (0..100)
            .map(|i| if i % 2 == 0 { 0.5 } else { -0.5 })
            .collect();
        let velocity = series(&vys);
        let reps = segment_reps(&velocity, default_config());
        assert_eq!(reps.len(), 0);
    }

    #[test]
    fn descent_only_with_no_ascent_yields_zero_reps() {
        let mut vys = vec![0.0; 20];
        for i in 0..60 {
            vys.push(50.0 * (i as f64 / 59.0));
        }
        let velocity = series(&vys);
        let reps = segment_reps(&velocity, default_config());
        assert_eq!(reps.len(), 0);
    }

    #[test]
    fn pause_at_bottom_still_yields_one_rep() {
        // Realistic amplitude (~190 px travel per phase; was 50-peak ≈
        // 25 px pre-15.1, below the new min_displacement gate).
        let mut vys = vec![0.0; 20];
        for i in 0..30 {
            vys.push(400.0 * (i as f64 / 29.0));
        }
        // Plateau (pause at the bottom of the squat) for 20 samples.
        vys.extend(vec![0.0; 20]);
        for i in 0..30 {
            vys.push(-400.0 * (i as f64 / 29.0));
        }
        vys.extend(vec![0.0; 20]);
        let velocity = series(&vys);
        let reps = segment_reps(&velocity, default_config());
        assert_eq!(reps.len(), 1);
        let rep = reps[0];
        // Descent starts once the ramp crosses the dead-band, a few
        // samples after the leading idle block ends at idx 20.
        assert!(rep.eccentric_start >= 20 && rep.eccentric_start <= 25);
        // Ascent ends a few samples before the ramp re-enters the
        // dead-band, before the trailing idle block starts at idx 100.
        assert!(rep.concentric_end >= 95 && rep.concentric_end < 100);
        // Bottom falls somewhere within the plateau.
        assert!(rep.bottom >= 45 && rep.bottom < 75);
    }

    #[test]
    fn short_jitter_spike_does_not_start_a_phase() {
        // A single-frame spike above threshold, surrounded by idle: too
        // short to meet min_phase_duration, should be folded into Idle.
        let mut vys = vec![0.0; 20];
        vys.push(50.0);
        vys.extend(vec![0.0; 20]);
        let velocity = series(&vys);
        let reps = segment_reps(&velocity, default_config());
        assert_eq!(reps.len(), 0);
    }

    #[test]
    fn too_few_samples_yields_zero_reps() {
        let velocity = series(&[0.0]);
        assert_eq!(segment_reps(&velocity, default_config()), Vec::new());
        let velocity = series(&[]);
        assert_eq!(segment_reps(&velocity, default_config()), Vec::new());
    }

    #[test]
    fn default_config_has_documented_values() {
        let cfg = RepSegmentationConfig::default_config();
        assert_eq!(cfg.min_velocity(), 5.0);
        assert_eq!(cfg.min_phase_duration_seconds(), 0.15);
        assert_eq!(cfg.min_displacement(), 40.0);
        assert_eq!(cfg, RepSegmentationConfig::default());
    }

    /// Regression for the phantom-rep bug (task 15.1): during a walkout /
    /// unrack the bar drifts slowly up and down by a few dozen pixels for
    /// several seconds. Each oscillation clears both the velocity dead-band
    /// (10 px/s > 5) and the minimum phase duration (0.5s > 0.15s), but the
    /// travel per phase is only 10 px/s x 0.5s = 5 px — nowhere near a real
    /// squat's ~180 px range of motion. Without a displacement gate this
    /// counted as 3 "reps" before the lifter even started.
    #[test]
    fn walkout_like_small_slow_oscillation_yields_zero_reps() {
        let mut vys = vec![0.0; 15];
        for _ in 0..3 {
            vys.extend(vec![10.0; 15]); // 0.5s slow drift down: 5 px travel
            vys.extend(vec![-10.0; 15]); // 0.5s slow drift up: 5 px travel
        }
        vys.extend(vec![0.0; 15]);
        let velocity = series(&vys);
        let reps = segment_reps(&velocity, default_config());
        assert_eq!(reps.len(), 0);
    }

    /// A real-amplitude rep (~190 px of travel per phase, matching the
    /// ~180 px squat ROM in our test footage) must be unaffected by the
    /// displacement gate.
    #[test]
    fn real_amplitude_rep_survives_displacement_gate() {
        let vys = clean_rep(30, 30);
        let velocity = series(&vys);
        let reps = segment_reps(&velocity, default_config());
        assert_eq!(reps.len(), 1);
    }

    /// Boundary behavior: a phase whose displacement is exactly
    /// `min_displacement` is kept; strictly below is discarded. Constant
    /// vy = 30 px/s across a run spanning exactly 1s integrates to exactly
    /// 30.0 px per phase (trapezoid over a constant is exact).
    #[test]
    fn phase_displacement_exactly_at_threshold_is_kept() {
        let mut vys = vec![0.0; 10];
        vys.extend(vec![30.0; 31]); // spans 30 frame gaps = 1.0s -> 30.0 px
        vys.extend(vec![-30.0; 31]);
        vys.extend(vec![0.0; 10]);
        let velocity = series(&vys);

        let at = RepSegmentationConfig::builder()
            .min_velocity(5.0)
            .min_phase_duration_seconds(0.15)
            .min_displacement(30.0)
            .build();
        assert_eq!(segment_reps(&velocity, at).len(), 1);

        let above = RepSegmentationConfig::builder()
            .min_velocity(5.0)
            .min_phase_duration_seconds(0.15)
            .min_displacement(30.0 + 1e-9)
            .build();
        assert_eq!(segment_reps(&velocity, above).len(), 0);
    }

    /// Calibrated (meter-unit) variant: a 0.5 m rep passes a 0.15 m
    /// displacement gate; a 0.05 m walkout-scale drift does not.
    #[test]
    fn calibrated_meter_units_gate_works() {
        let config = RepSegmentationConfig::builder()
            .min_velocity(0.02)
            .min_phase_duration_seconds(0.15)
            .min_displacement(0.15)
            .build();

        // 0.5 m/s for 1s each way -> 0.5 m per phase: a real rep.
        let mut real = vec![0.0; 10];
        real.extend(vec![0.5; 31]);
        real.extend(vec![-0.5; 31]);
        real.extend(vec![0.0; 10]);
        assert_eq!(segment_reps(&series(&real), config).len(), 1);

        // 0.05 m/s for 1s each way -> 0.05 m per phase: setup drift.
        let mut drift = vec![0.0; 10];
        drift.extend(vec![0.05; 31]);
        drift.extend(vec![-0.05; 31]);
        drift.extend(vec![0.0; 10]);
        assert_eq!(segment_reps(&series(&drift), config).len(), 0);
    }

    /// A discarded undersized candidate must not merge with neighbors: a
    /// tiny oscillation between two real reps still yields exactly 2 reps,
    /// each with its own boundaries.
    #[test]
    fn undersized_candidate_between_real_reps_is_discarded_not_merged() {
        let mut vys = clean_rep(15, 15);
        vys.extend(vec![10.0; 15]);
        vys.extend(vec![-10.0; 15]);
        vys.extend(clean_rep(15, 15));
        let velocity = series(&vys);
        let reps = segment_reps(&velocity, default_config());
        assert_eq!(reps.len(), 2);
        assert!(reps[0].concentric_end < reps[1].eccentric_start);
    }
}
