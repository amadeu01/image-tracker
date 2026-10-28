//! Seed proposal: pick the moving plate from per-frame plate detections.
//!
//! Candidates are linked sample to sample (a sample is one `FrameDetections`
//! entry; the pre-pass may stride over many frames) by global nearest-
//! neighbour assignment. The speed gate is expressed in plate diameters per
//! elapsed frame and every link must also keep the plate size consistent, so
//! a head-sized circle never joins a plate track. Short occlusions are
//! bridged by merging tracks whose end and start are consistent in speed and
//! size.
//!
//! Static tracks (floor plates, rack hardware, jitter) are rejected by a
//! robust vertical range (10th to 90th percentile of one consistent point per
//! track) and the track with the largest range wins. The proposal sits at the
//! first stable run of that track. No pixel scale is baked in.
//!
//! `PlateCandidate::score` saturates and is only coarsely comparable across
//! frames, so confidence is derived from track length, persistence, vertical
//! travel and ambiguity, never from score differences.

use crate::geometry::Point;
use crate::lift::{classify_lift, LiftClassification, LiftClassifierConfig, TrackPoint};
use crate::plate_finder::PlateCandidate;

/// Plate candidates found in one frame.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameDetections {
    pub frame_index: u64,
    pub candidates: Vec<PlateCandidate>,
}

/// A proposed seed for the tracker.
#[derive(Debug, Clone, PartialEq)]
pub struct SeedProposal {
    /// Frame of the chosen track's first stable detection.
    pub frame_index: u64,
    /// Bar Point there (sleeve end when the track has one, else plate centre).
    pub position: Point,
    /// Median plate diameter over the track, from the detections.
    pub plate_diameter_px: f64,
    pub lift: LiftClassification,
    /// In `[0, 1]`.
    pub confidence: f64,
}

/// Thresholds for `propose_seed`, in plate-diameter units, fractions or
/// sample counts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SeedProposalConfig {
    link_gate_plates_per_frame: f64,
    size_ratio_tolerance: f64,
    max_gap_samples: usize,
    merge_gap_samples: usize,
    min_track_points: usize,
    min_track_persistence: f64,
    min_stable_samples: usize,
    stable_window_samples: usize,
    min_relative_diameter: f64,
    min_vertical_travel_plates: f64,
    sleeve_end_min_fraction: f64,
    ambiguity_ratio: f64,
    full_confidence_points: usize,
    full_confidence_travel_plates: f64,
    lift_classifier: LiftClassifierConfig,
}

impl SeedProposalConfig {
    pub fn builder() -> SeedProposalConfigBuilder {
        SeedProposalConfigBuilder::default()
    }

    /// Initial values, to be tuned against labelled clips.
    pub fn default_config() -> Self {
        Self {
            link_gate_plates_per_frame: 0.12,
            size_ratio_tolerance: 0.3,
            max_gap_samples: 3,
            merge_gap_samples: 10,
            min_track_points: 5,
            min_track_persistence: 0.5,
            min_stable_samples: 4,
            stable_window_samples: 6,
            min_relative_diameter: 0.6,
            min_vertical_travel_plates: 0.5,
            sleeve_end_min_fraction: 0.8,
            ambiguity_ratio: 0.5,
            full_confidence_points: 30,
            full_confidence_travel_plates: 2.0,
            lift_classifier: LiftClassifierConfig::default_config(),
        }
    }

    /// Max link distance per elapsed frame, in plate diameters.
    pub fn link_gate_plates_per_frame(&self) -> f64 {
        self.link_gate_plates_per_frame
    }
    /// Allowed relative diameter deviation when linking (0.3 = +-30%).
    pub fn size_ratio_tolerance(&self) -> f64 {
        self.size_ratio_tolerance
    }
    /// Consecutive samples a track may miss and still be continued.
    pub fn max_gap_samples(&self) -> usize {
        self.max_gap_samples
    }
    /// Max samples between two tracks for them to be merged.
    pub fn merge_gap_samples(&self) -> usize {
        self.merge_gap_samples
    }
    /// Minimum linked detections for a track to count.
    pub fn min_track_points(&self) -> usize {
        self.min_track_points
    }
    /// Minimum fraction of the samples between a track's first and last
    /// point that must carry a detection, in `[0, 1]`. Sparse chains of
    /// unrelated false circles fall below it.
    pub fn min_track_persistence(&self) -> f64 {
        self.min_track_persistence
    }
    /// Samples that must be present inside a stability window.
    pub fn min_stable_samples(&self) -> usize {
        self.min_stable_samples
    }
    /// Length, in consecutive samples, of the stability window.
    pub fn stable_window_samples(&self) -> usize {
        self.stable_window_samples
    }
    /// Smallest median diameter, as a fraction of the largest moving
    /// track's, that may still be proposed.
    pub fn min_relative_diameter(&self) -> f64 {
        self.min_relative_diameter
    }
    /// Minimum robust vertical range, in plate diameters, for a track.
    pub fn min_vertical_travel_plates(&self) -> f64 {
        self.min_vertical_travel_plates
    }
    /// Fraction of a track's samples that must carry a sleeve end for the
    /// sleeve end to be used as the track's point.
    pub fn sleeve_end_min_fraction(&self) -> f64 {
        self.sleeve_end_min_fraction
    }
    /// Second-best travel / best travel at or above which confidence drops.
    pub fn ambiguity_ratio(&self) -> f64 {
        self.ambiguity_ratio
    }
    /// Track length at which the length term of confidence saturates.
    pub fn full_confidence_points(&self) -> usize {
        self.full_confidence_points
    }
    /// Vertical travel (plate diameters) at which the travel term saturates.
    pub fn full_confidence_travel_plates(&self) -> f64 {
        self.full_confidence_travel_plates
    }
    /// Classifier thresholds used on the linked track.
    pub fn lift_classifier(&self) -> LiftClassifierConfig {
        self.lift_classifier
    }
}

/// Builder for [`SeedProposalConfig`], starting from the defaults.
#[derive(Debug, Clone, Copy)]
pub struct SeedProposalConfigBuilder {
    cfg: SeedProposalConfig,
}

impl Default for SeedProposalConfigBuilder {
    fn default() -> Self {
        Self {
            cfg: SeedProposalConfig::default_config(),
        }
    }
}

impl SeedProposalConfigBuilder {
    pub fn link_gate_plates_per_frame(mut self, v: f64) -> Self {
        self.cfg.link_gate_plates_per_frame = v;
        self
    }
    pub fn size_ratio_tolerance(mut self, v: f64) -> Self {
        self.cfg.size_ratio_tolerance = v;
        self
    }
    pub fn max_gap_samples(mut self, v: usize) -> Self {
        self.cfg.max_gap_samples = v;
        self
    }
    pub fn merge_gap_samples(mut self, v: usize) -> Self {
        self.cfg.merge_gap_samples = v;
        self
    }
    pub fn min_track_points(mut self, v: usize) -> Self {
        self.cfg.min_track_points = v;
        self
    }
    pub fn min_track_persistence(mut self, v: f64) -> Self {
        self.cfg.min_track_persistence = v;
        self
    }
    pub fn min_stable_samples(mut self, v: usize) -> Self {
        self.cfg.min_stable_samples = v;
        self
    }
    pub fn stable_window_samples(mut self, v: usize) -> Self {
        self.cfg.stable_window_samples = v;
        self
    }
    pub fn min_relative_diameter(mut self, v: f64) -> Self {
        self.cfg.min_relative_diameter = v;
        self
    }
    pub fn min_vertical_travel_plates(mut self, v: f64) -> Self {
        self.cfg.min_vertical_travel_plates = v;
        self
    }
    pub fn sleeve_end_min_fraction(mut self, v: f64) -> Self {
        self.cfg.sleeve_end_min_fraction = v;
        self
    }
    pub fn ambiguity_ratio(mut self, v: f64) -> Self {
        self.cfg.ambiguity_ratio = v;
        self
    }
    pub fn full_confidence_points(mut self, v: usize) -> Self {
        self.cfg.full_confidence_points = v;
        self
    }
    pub fn full_confidence_travel_plates(mut self, v: f64) -> Self {
        self.cfg.full_confidence_travel_plates = v;
        self
    }
    pub fn lift_classifier(mut self, v: LiftClassifierConfig) -> Self {
        self.cfg.lift_classifier = v;
        self
    }
    /// Builds the config; the stability window is clamped so that
    /// `1 <= min_stable_samples <= stable_window_samples`.
    pub fn build(self) -> SeedProposalConfig {
        let mut cfg = self.cfg;
        cfg.min_track_persistence = if cfg.min_track_persistence.is_nan() {
            SeedProposalConfig::default_config().min_track_persistence
        } else {
            cfg.min_track_persistence.clamp(0.0, 1.0)
        };
        cfg.stable_window_samples = cfg.stable_window_samples.max(1);
        cfg.min_stable_samples = cfg.min_stable_samples.clamp(1, cfg.stable_window_samples);
        cfg
    }
}

/// One linked detection.
#[derive(Debug, Clone, Copy)]
struct Linked {
    sample: usize,
    frame_index: u64,
    center: Point,
    sleeve: Option<Point>,
    diameter_px: f64,
}

type Track = Vec<Linked>;

fn finite_point(p: Point) -> bool {
    p.x.is_finite() && p.y.is_finite()
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|a, b| a.total_cmp(b));
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

/// Median diameter of the last few points of a track.
fn recent_diameter(track: &[Linked]) -> f64 {
    let tail = &track[track.len().saturating_sub(5)..];
    let mut d: Vec<f64> = tail.iter().map(|p| p.diameter_px).collect();
    median(&mut d)
}

fn size_consistent(diameter: f64, reference: f64, tolerance: f64) -> bool {
    (diameter / reference - 1.0).abs() <= tolerance
}

fn dist_plates(a: Point, b: Point, diameter: f64) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt() / diameter
}

/// Valid samples, sorted by frame index with duplicate indices merged.
fn clean_samples(detections: &[FrameDetections]) -> Vec<(u64, Vec<&PlateCandidate>)> {
    let mut frames: Vec<&FrameDetections> = detections.iter().collect();
    frames.sort_by_key(|f| f.frame_index);
    let mut out: Vec<(u64, Vec<&PlateCandidate>)> = Vec::new();
    for f in frames {
        let valid = f
            .candidates
            .iter()
            .filter(|c| finite_point(c.center) && c.radius_px.is_finite() && c.radius_px > 0.0);
        match out.last_mut() {
            Some((idx, cs)) if *idx == f.frame_index => cs.extend(valid),
            _ => out.push((f.frame_index, valid.collect())),
        }
    }
    out
}

fn link_tracks(detections: &[FrameDetections], config: &SeedProposalConfig) -> Vec<Track> {
    let samples = clean_samples(detections);
    let mut tracks: Vec<Track> = Vec::new();
    for (s, (frame_index, cands)) in samples.iter().enumerate() {
        // All (track, candidate) pairs inside the gates, closest first.
        let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
        for (ti, track) in tracks.iter().enumerate() {
            let last = track[track.len() - 1];
            if s - last.sample > config.max_gap_samples + 1 {
                continue;
            }
            let elapsed = frame_index.saturating_sub(last.frame_index);
            if elapsed == 0 {
                continue;
            }
            let reference = recent_diameter(track);
            for (ci, c) in cands.iter().enumerate() {
                if !size_consistent(c.radius_px * 2.0, reference, config.size_ratio_tolerance) {
                    continue;
                }
                let per_frame =
                    dist_plates(c.center, last.center, last.diameter_px) / elapsed as f64;
                if per_frame <= config.link_gate_plates_per_frame {
                    pairs.push((per_frame, ti, ci));
                }
            }
        }
        pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut track_taken = vec![false; tracks.len()];
        let mut cand_taken = vec![false; cands.len()];
        let make = |c: &PlateCandidate| Linked {
            sample: s,
            frame_index: *frame_index,
            center: c.center,
            sleeve: c.sleeve_end.filter(|p| finite_point(*p)),
            diameter_px: c.radius_px * 2.0,
        };
        for (_, ti, ci) in pairs {
            if track_taken[ti] || cand_taken[ci] {
                continue;
            }
            track_taken[ti] = true;
            cand_taken[ci] = true;
            tracks[ti].push(make(cands[ci]));
        }
        for (ci, c) in cands.iter().enumerate() {
            if !cand_taken[ci] {
                tracks.push(vec![make(c)]);
            }
        }
    }
    merge_tracks(tracks, config)
}

/// Bridge occlusions: join a track's end to a later track's start when the
/// implied speed and the diameter are consistent.
fn merge_tracks(tracks: Vec<Track>, config: &SeedProposalConfig) -> Vec<Track> {
    let n = tracks.len();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| tracks[i][0].sample);
    let starts: Vec<usize> = order.iter().map(|&i| tracks[i][0].sample).collect();

    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for a in 0..n {
        let end = tracks[a][tracks[a].len() - 1];
        let lo = starts.partition_point(|&s| s <= end.sample);
        for &b in order[lo..].iter() {
            let first = tracks[b][0];
            if first.sample - end.sample > config.merge_gap_samples {
                break;
            }
            let elapsed = first.frame_index.saturating_sub(end.frame_index);
            if elapsed == 0
                || !size_consistent(
                    first.diameter_px,
                    recent_diameter(&tracks[a]),
                    config.size_ratio_tolerance,
                )
            {
                continue;
            }
            let per_frame = dist_plates(first.center, end.center, end.diameter_px) / elapsed as f64;
            if per_frame <= config.link_gate_plates_per_frame {
                pairs.push((per_frame, a, b));
            }
        }
    }
    pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut next: Vec<Option<usize>> = vec![None; n];
    let mut has_prev = vec![false; n];
    for (_, a, b) in pairs {
        if next[a].is_none() && !has_prev[b] {
            next[a] = Some(b);
            has_prev[b] = true;
        }
    }
    let mut slots: Vec<Option<Track>> = tracks.into_iter().map(Some).collect();
    let mut merged = Vec::new();
    for head in 0..n {
        if has_prev[head] {
            continue;
        }
        let mut chain = slots[head].take().unwrap_or_default();
        let mut cur = head;
        while let Some(nx) = next[cur] {
            chain.extend(slots[nx].take().unwrap_or_default());
            cur = nx;
        }
        merged.push(chain);
    }
    merged
}

/// One consistent bar point per track: the sleeve end only when most samples
/// carry one, otherwise always the plate centre.
fn use_sleeve(track: &[Linked], config: &SeedProposalConfig) -> bool {
    let with = track.iter().filter(|p| p.sleeve.is_some()).count();
    with as f64 >= config.sleeve_end_min_fraction * track.len() as f64
}

fn point_of(p: &Linked, sleeve: bool) -> Point {
    if sleeve {
        p.sleeve.unwrap_or(p.center)
    } else {
        p.center
    }
}

/// Index of the first sample starting a stable run, if any.
fn first_stable(track: &[Linked], config: &SeedProposalConfig) -> Option<usize> {
    for i in 0..track.len() {
        let end = track[i].sample + config.stable_window_samples;
        let window: Vec<&Linked> = track[i..].iter().take_while(|p| p.sample < end).collect();
        if window.len() < config.min_stable_samples {
            continue;
        }
        let mut d: Vec<f64> = window.iter().map(|p| p.diameter_px).collect();
        let m = median(&mut d);
        if window
            .iter()
            .all(|p| size_consistent(p.diameter_px, m, config.size_ratio_tolerance))
        {
            return Some(i);
        }
    }
    None
}

/// 10th to 90th percentile vertical range, in pixels.
fn robust_range_px(track: &[Linked], sleeve: bool) -> f64 {
    let mut ys: Vec<f64> = track.iter().map(|p| point_of(p, sleeve).y).collect();
    ys.sort_by(|a, b| a.total_cmp(b));
    let n = ys.len();
    let lo = ys[((n - 1) as f64 * 0.1).floor() as usize];
    let hi = ys[((n - 1) as f64 * 0.9).ceil() as usize];
    hi - lo
}

fn median_diameter(track: &[Linked]) -> f64 {
    let mut d: Vec<f64> = track.iter().map(|p| p.diameter_px).collect();
    median(&mut d)
}

struct Candidate<'a> {
    track: &'a [Linked],
    sleeve: bool,
    range_px: f64,
    diameter_px: f64,
    /// Range over the shared scale; set after all tracks are known.
    travel: f64,
}

/// Propose the seed for tracking: the first stable point of the moving plate.
///
/// Returns `None` when no track is long enough, stable and travels far
/// enough, so a static floor plate or rack hardware is never proposed.
pub fn propose_seed(
    detections: &[FrameDetections],
    config: &SeedProposalConfig,
) -> Option<SeedProposal> {
    let tracks = link_tracks(detections, config);
    // Persistence = points / samples spanned. Sporadic false circles chained
    // across long gaps are sparse; a real plate is present most of the time.
    // Filtered before the static test and the shared scale so a sparse chain
    // can neither win nor inflate the scale.
    let tracks: Vec<Track> = tracks
        .into_iter()
        .filter(|t| {
            let span = t[t.len() - 1].sample - t[0].sample + 1;
            t.len() as f64 / span as f64 >= config.min_track_persistence
        })
        .collect();
    let mut cands: Vec<Candidate> = tracks
        .iter()
        .filter_map(|t| {
            let start = first_stable(t, config)?;
            let track = &t[start..];
            if track.len() < config.min_track_points {
                return None;
            }
            let sleeve = use_sleeve(track, config);
            let range_px = robust_range_px(track, sleeve);
            let diameter_px = median_diameter(track);
            // Static test: the track's own range in its own plate diameters.
            (range_px / diameter_px >= config.min_vertical_travel_plates).then_some(Candidate {
                track,
                sleeve,
                range_px,
                diameter_px,
                travel: 0.0,
            })
        })
        .collect();
    // One frame-level scale: the largest moving diameter. Smaller moving
    // circles (head, hardware) never compete with a larger moving one.
    let scale = cands.iter().map(|c| c.diameter_px).fold(0.0, f64::max);
    cands.retain(|c| c.diameter_px >= config.min_relative_diameter * scale);
    for c in &mut cands {
        c.travel = c.range_px / scale;
    }
    cands.sort_by(|a, b| b.travel.total_cmp(&a.travel));
    let best = cands.first()?;
    let second_travel = cands.get(1).map(|c| c.travel);

    let track = best.track;
    let mut diameters: Vec<f64> = track.iter().map(|p| p.diameter_px).collect();
    let plate_diameter_px = median(&mut diameters);

    let points: Vec<TrackPoint> = track
        .iter()
        .map(|p| TrackPoint {
            frame_index: p.frame_index,
            position: point_of(p, best.sleeve),
            plate_diameter_px: p.diameter_px,
        })
        .collect();
    let lift = classify_lift(&points, &config.lift_classifier);

    let first = track[0];
    let last = track[track.len() - 1];
    let span = (last.sample - first.sample + 1) as f64;
    let length_term = (track.len() as f64 / config.full_confidence_points.max(1) as f64).min(1.0);
    let travel_term = (best.travel / config.full_confidence_travel_plates).min(1.0);
    let persistence_term = (track.len() as f64 / span).min(1.0);
    let mut confidence = 0.4 * length_term + 0.4 * travel_term + 0.2 * persistence_term;
    if let Some(second) = second_travel {
        let ratio = second / best.travel;
        if ratio >= config.ambiguity_ratio {
            confidence *= 1.0 - 0.5 * ratio.min(1.0);
        }
    }

    Some(SeedProposal {
        frame_index: first.frame_index,
        position: point_of(&first, best.sleeve),
        plate_diameter_px,
        lift,
        confidence: confidence.clamp(0.0, 1.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lift::LiftType;

    const R: f64 = 60.0;
    const D: f64 = 120.0;

    fn cand(x: f64, y: f64, radius: f64) -> PlateCandidate {
        PlateCandidate {
            center: Point::new(x, y),
            radius_px: radius,
            sleeve_end: None,
            score: 1.0,
        }
    }

    fn frame(i: u64, candidates: Vec<PlateCandidate>) -> FrameDetections {
        FrameDetections {
            frame_index: i,
            candidates,
        }
    }

    /// Bar y (image coords) at the start of a lift.
    const Y0: f64 = 900.0;

    /// Plate rising `rise` plate diameters over `n` samples, `stride` frames
    /// apart, starting at frame `start`.
    fn rising_strided(
        start: u64,
        n: u64,
        stride: u64,
        rise: f64,
        radius: f64,
    ) -> Vec<(u64, PlateCandidate)> {
        let d = radius * 2.0;
        (0..n)
            .map(|k| {
                let y = Y0 - rise * d * k as f64 / (n - 1) as f64;
                (start + k * stride, cand(300.0, y, radius))
            })
            .collect()
    }

    fn rising(start: u64, n: u64, rise: f64) -> Vec<(u64, PlateCandidate)> {
        rising_strided(start, n, 1, rise, R)
    }

    fn to_frames(v: Vec<(u64, PlateCandidate)>) -> Vec<FrameDetections> {
        v.into_iter().map(|(i, c)| frame(i, vec![c])).collect()
    }

    fn cfg() -> SeedProposalConfig {
        SeedProposalConfig::default_config()
    }

    #[test]
    fn static_plate_present_in_every_frame_is_never_proposed() {
        let dets: Vec<_> = (0..60)
            .map(|i| frame(i, vec![cand(500.0, 800.0, R)]))
            .collect();
        assert!(propose_seed(&dets, &cfg()).is_none());
    }

    #[test]
    fn proposal_frame_is_within_a_small_bound_of_the_first_stable_detection() {
        let mut dets: Vec<_> = (0..100).map(|i| frame(i, vec![])).collect();
        dets.extend(to_frames(rising(100, 40, 3.0)));
        let p = propose_seed(&dets, &cfg()).unwrap();
        let window = cfg().stable_window_samples() as u64;
        assert!(p.frame_index >= 100);
        assert!(p.frame_index <= 100 + 2 * window, "got {}", p.frame_index);
    }

    #[test]
    fn no_plates_anywhere_returns_none() {
        let dets: Vec<_> = (0..50).map(|i| frame(i, vec![])).collect();
        assert!(propose_seed(&dets, &cfg()).is_none());
    }

    #[test]
    fn empty_detection_list_returns_none() {
        assert!(propose_seed(&[], &cfg()).is_none());
    }

    #[test]
    fn proposal_carries_lift_classification_from_the_linked_track() {
        let p = propose_seed(&to_frames(rising(0, 40, 3.0)), &cfg()).unwrap();
        assert_eq!(p.lift.lift, LiftType::Deadlift);
        assert!((0.0..=1.0).contains(&p.confidence));
    }

    #[test]
    fn plate_diameter_comes_from_the_detections_not_a_constant() {
        for radius in [40.0, 90.0] {
            let dets = to_frames(rising_strided(0, 40, 1, 3.0, radius));
            let p = propose_seed(&dets, &cfg()).unwrap();
            assert!((p.plate_diameter_px - radius * 2.0).abs() < 1e-9);
        }
    }

    #[test]
    fn moving_bar_wins_over_static_floor_plate() {
        let dets: Vec<_> = rising(0, 40, 3.0)
            .into_iter()
            .map(|(i, c)| frame(i, vec![cand(900.0, 1000.0, 70.0), c]))
            .collect();
        let p = propose_seed(&dets, &cfg()).unwrap();
        assert!((p.position.x - 300.0).abs() < 1e-9);
    }

    #[test]
    fn detection_gap_in_the_middle_keeps_one_full_range_track() {
        // Rise of 1.6 plates: each half alone (0.8) is not a deadlift, the
        // whole track is.
        let dets: Vec<_> = rising(0, 40, 1.6)
            .into_iter()
            .filter(|(i, _)| !(18..22).contains(i))
            .map(|(i, c)| frame(i, vec![c]))
            .collect();
        let p = propose_seed(&dets, &cfg()).unwrap();
        assert_eq!(p.frame_index, 0);
        assert_eq!(p.lift.lift, LiftType::Deadlift);
    }

    #[test]
    fn sleeve_end_is_preferred_as_position() {
        let dets: Vec<_> = rising(0, 40, 3.0)
            .into_iter()
            .map(|(i, mut c)| {
                c.sleeve_end = Some(Point::new(c.center.x + 5.0, c.center.y));
                frame(i, vec![c])
            })
            .collect();
        let p = propose_seed(&dets, &cfg()).unwrap();
        assert!((p.position.x - 305.0).abs() < 1e-9);
    }

    #[test]
    fn candidate_steal_goes_to_the_closest_candidate() {
        // Samples 4 frames apart, bar moves 0.05 plate per sample. At sample 1
        // a wrong candidate X (0.3 plate away, listed first) competes with the
        // true continuation Y. Y must win, so the track starts at frame 0.
        let bar = rising_strided(0, 30, 4, 1.5, R);
        let dets: Vec<_> = bar
            .into_iter()
            .enumerate()
            .map(|(k, (i, c))| {
                let mut cs = vec![];
                if k == 1 {
                    cs.push(cand(c.center.x + 0.3 * D, c.center.y, R));
                }
                cs.push(c);
                frame(i, cs)
            })
            .collect();
        let p = propose_seed(&dets, &cfg()).unwrap();
        assert_eq!(p.frame_index, 0);
        assert!((p.position.x - 300.0).abs() < 1e-9);
    }

    #[test]
    fn head_sized_circle_does_not_link_to_a_plate_track() {
        // From frame 1 on, a head-sized circle sits 0.05 plate above the bar
        // and is listed first.
        let dets: Vec<_> = rising(0, 40, 3.0)
            .into_iter()
            .map(|(i, c)| {
                let mut cs = vec![];
                if i >= 1 {
                    let y = Y0 - 0.3 * (Y0 - c.center.y) - 0.05 * D;
                    cs.push(cand(c.center.x, y, R / 2.0));
                }
                cs.push(c);
                frame(i, cs)
            })
            .collect();
        let p = propose_seed(&dets, &cfg()).unwrap();
        assert!((p.plate_diameter_px - D).abs() < 1e-9);
        assert_eq!(p.frame_index, 0);
    }

    #[test]
    fn deadlift_plate_still_for_many_samples_then_rising_is_proposed_in_the_still_stretch() {
        // Documented behaviour: the proposal is the first stable detection of
        // the CHOSEN moving track. The plate is still for 40 samples and
        // then rises, so the proposal lies in the still stretch (or at the
        // lift start), never on the static floor plate.
        let mut bar: Vec<(u64, PlateCandidate)> =
            (0..40).map(|i| (i, cand(300.0, Y0, R))).collect();
        bar.extend(rising(40, 40, 3.0));
        let dets: Vec<_> = bar
            .into_iter()
            .map(|(i, c)| frame(i, vec![cand(900.0, 1000.0, 70.0), c]))
            .collect();
        let p = propose_seed(&dets, &cfg()).unwrap();
        assert!(p.frame_index <= 40, "got {}", p.frame_index);
        assert!((p.position.x - 300.0).abs() < 1e-9);
        assert_eq!(p.lift.lift, LiftType::Deadlift);
    }

    fn jitter_plate(i: u64, sleeve_every: Option<u64>) -> PlateCandidate {
        let s = if i.is_multiple_of(2) { 1.0 } else { -1.0 };
        let mut c = cand(500.0 + s * 2.0, 800.0 + s * 0.1 * D, R * (1.0 + s * 0.05));
        if let Some(n) = sleeve_every {
            if i.is_multiple_of(n) {
                c.sleeve_end = Some(Point::new(500.0, 800.0 + 0.6 * D));
            }
        }
        c
    }

    #[test]
    fn jittery_static_plate_is_never_proposed() {
        let dets: Vec<_> = (0..80)
            .map(|i| frame(i, vec![jitter_plate(i, None)]))
            .collect();
        assert!(propose_seed(&dets, &cfg()).is_none());
    }

    #[test]
    fn static_plate_with_intermittent_sleeve_end_is_never_proposed() {
        let dets: Vec<_> = (0..80)
            .map(|i| frame(i, vec![jitter_plate(i, Some(2))]))
            .collect();
        assert!(propose_seed(&dets, &cfg()).is_none());
    }

    #[test]
    fn squat_wobble_then_set_includes_the_walkout_in_the_track() {
        // Documented behaviour: unrack/walk-out wobble that links into the
        // same track is included, so the proposal is at the first stable
        // sample of the wobble, not at the start of the set. The lift is
        // still classified from the whole track.
        let mut pts: Vec<(u64, PlateCandidate)> = (0..12u64)
            .map(|i| {
                let w = if i.is_multiple_of(2) { 0.03 } else { -0.03 };
                (i, cand(300.0, Y0 + w * D, R))
            })
            .collect();
        for k in 0..60u64 {
            // Down 1.0 plate over 30 frames, then back up.
            let t = if k < 30 { k } else { 60 - k } as f64 / 30.0;
            pts.push((12 + k, cand(300.0, Y0 + t * D, R)));
        }
        let p = propose_seed(&to_frames(pts), &cfg()).unwrap();
        assert!(p.frame_index < 12, "got {}", p.frame_index);
        assert_eq!(p.lift.lift, LiftType::Squat);
    }

    /// 40-sample rise of 1.8 plates with `gap` empty samples starting at 17.
    fn rise_with_empty_gap(gap: u64) -> Vec<FrameDetections> {
        rising(0, 40, 1.8)
            .into_iter()
            .map(|(i, c)| {
                if (17..17 + gap).contains(&i) {
                    frame(i, vec![])
                } else {
                    frame(i, vec![c])
                }
            })
            .collect()
    }

    #[test]
    fn gap_longer_than_max_gap_is_merged_when_consistent() {
        let p = propose_seed(&rise_with_empty_gap(6), &cfg()).unwrap();
        assert_eq!(p.frame_index, 0);
        assert_eq!(p.lift.lift, LiftType::Deadlift);
    }

    #[test]
    fn gap_with_inconsistent_motion_is_not_merged() {
        let dets: Vec<_> = rising(0, 40, 1.8)
            .into_iter()
            .map(|(i, mut c)| {
                if (17..23).contains(&i) {
                    return frame(i, vec![]);
                }
                if i >= 23 {
                    c.center.x += 2.0 * D;
                }
                frame(i, vec![c])
            })
            .collect();
        let p = propose_seed(&dets, &cfg()).unwrap();
        assert_ne!(p.lift.lift, LiftType::Deadlift);
    }

    #[test]
    fn stride_of_eight_frames_still_links_a_real_bar() {
        // 0.05 plate per frame, sampled every 8 frames.
        let dets = to_frames(rising_strided(0, 9, 8, 3.2, R));
        let p = propose_seed(&dets, &cfg()).unwrap();
        assert_eq!(p.frame_index, 0);
        assert_eq!(p.lift.lift, LiftType::Deadlift);
    }

    #[test]
    fn confidence_is_lower_for_short_or_ambiguous_tracks_than_for_a_long_clean_one() {
        let clean = propose_seed(&to_frames(rising(0, 60, 3.0)), &cfg()).unwrap();
        let short = propose_seed(&to_frames(rising(0, 8, 0.8)), &cfg()).unwrap();
        let ambiguous_dets: Vec<_> = rising(0, 60, 3.0)
            .into_iter()
            .map(|(i, c)| {
                let other = cand(700.0, c.center.y + 50.0, R);
                frame(i, vec![c, other])
            })
            .collect();
        let ambiguous = propose_seed(&ambiguous_dets, &cfg()).unwrap();
        assert!(short.confidence < clean.confidence);
        assert!(ambiguous.confidence < clean.confidence);
        for p in [&clean, &short, &ambiguous] {
            assert!((0.0..=1.0).contains(&p.confidence));
        }
    }

    #[test]
    fn nan_zero_negative_radius_and_unsorted_or_duplicate_frame_index_do_not_panic() {
        let mut dets = to_frames(rising(0, 30, 3.0));
        dets.reverse();
        dets.push(frame(5, vec![cand(300.0, Y0, R)]));
        dets.push(frame(
            7,
            vec![
                cand(f64::NAN, 100.0, R),
                cand(100.0, f64::NAN, R),
                cand(100.0, 100.0, f64::NAN),
                cand(100.0, 100.0, 0.0),
                cand(100.0, 100.0, -5.0),
                cand(f64::INFINITY, 100.0, R),
            ],
        ));
        let _ = propose_seed(&dets, &cfg());
        let all_bad = vec![frame(0, vec![cand(f64::NAN, f64::NAN, f64::NAN)]); 10];
        assert!(propose_seed(&all_bad, &cfg()).is_none());
    }

    /// Plate (D=120) and a second circle of radius `r2` rising together in
    /// the same frames; the circle moves `px2` pixels, the plate `px1`.
    fn plate_and_circle(px1: f64, r2: f64, px2: f64) -> Vec<FrameDetections> {
        (0..40u64)
            .map(|i| {
                let t = i as f64 / 39.0;
                frame(
                    i,
                    vec![
                        cand(300.0, Y0 - t * px1, R),
                        cand(380.0, Y0 - 200.0 - t * px2, r2),
                    ],
                )
            })
            .collect()
    }

    #[test]
    fn head_circle_moving_with_the_bar_does_not_outrank_the_plate() {
        // Head D=50 moves the same 120 px = 2.4 head diameters.
        let p = propose_seed(&plate_and_circle(D, 25.0, D), &cfg()).unwrap();
        assert!((p.plate_diameter_px - D).abs() < 1e-9);
        assert!((p.position.x - 300.0).abs() < 1e-9);
    }

    #[test]
    fn smaller_moving_circle_never_wins_over_a_larger_moving_one() {
        // Circle D=60 (0.5 of the plate) moves further in pixels.
        let p = propose_seed(&plate_and_circle(100.0, 30.0, 150.0), &cfg()).unwrap();
        assert!((p.plate_diameter_px - D).abs() < 1e-9);
        assert!((p.position.x - 300.0).abs() < 1e-9);
    }

    #[test]
    fn ambiguity_uses_the_shared_scale() {
        // Documented: tracks are ranked by pixel range over one shared scale
        // (the largest moving diameter). A much smaller moving circle is not a
        // competitor, so it must not lower confidence; a comparable-size
        // circle with the same travel is, and does.
        let alone = propose_seed(&to_frames(rising(0, 40, 1.0)), &cfg()).unwrap();
        let with_head = propose_seed(&plate_and_circle(D, 25.0, D), &cfg()).unwrap();
        assert!((with_head.confidence - alone.confidence).abs() < 1e-9);
        let rival = propose_seed(&plate_and_circle(D, 50.0, 0.9 * D), &cfg()).unwrap();
        assert!(rival.confidence < alone.confidence);
        assert!((rival.plate_diameter_px - D).abs() < 1e-9);
    }

    #[test]
    fn zero_stable_window_is_clamped_or_rejected() {
        let c = SeedProposalConfig::builder()
            .stable_window_samples(0)
            .min_stable_samples(0)
            .build();
        assert!(c.stable_window_samples() >= 1);
        assert!(c.min_stable_samples() >= 1);
        assert!(c.min_stable_samples() <= c.stable_window_samples());
        let _ = propose_seed(&to_frames(rising(0, 40, 3.0)), &c);
        let c = SeedProposalConfig::builder()
            .stable_window_samples(2)
            .min_stable_samples(9)
            .build();
        assert!(c.min_stable_samples() <= c.stable_window_samples());
    }

    /// Persistent plate at x=100 plus a sporadic chain of same-size circles at
    /// x=330 hopping vertically (the v7 shape): stride 4 frames, 90 samples.
    fn persistent_plate_and_false_chain(
        plate_rise: f64,
        plate_every: impl Fn(u64) -> bool,
    ) -> Vec<FrameDetections> {
        // A short static burst (so a stability window exists), then sparse
        // detections hopping vertically; gaps alternate link / merge sized.
        let hops = [630.0, 460.0, 434.0, 795.0];
        let mut chain_samples: Vec<(u64, f64)> = (0..4).map(|s| (s, 795.0)).collect();
        let mut s = 8u64;
        let mut k = 0usize;
        while s < 90 && chain_samples.len() < 15 {
            chain_samples.push((s, hops[k % hops.len()]));
            s += if k.is_multiple_of(2) { 4 } else { 7 };
            k += 1;
        }
        (0..90u64)
            .map(|s| {
                let mut cands = Vec::new();
                if plate_every(s) {
                    let y = Y0 - plate_rise * D * s as f64 / 89.0;
                    cands.push(cand(100.0, y, R));
                }
                if let Some((_, y)) = chain_samples.iter().find(|(cs, _)| *cs == s) {
                    cands.push(cand(330.0, *y, R));
                }
                frame(s * 4, cands)
            })
            .collect()
    }

    #[test]
    fn sporadic_false_chain_with_large_vertical_range_never_beats_a_persistent_plate() {
        let dets = persistent_plate_and_false_chain(1.0, |_| true);
        let p = propose_seed(&dets, &cfg()).expect("persistent plate proposed");
        assert!(
            (p.position.x - 100.0).abs() < 1.0,
            "followed x={}",
            p.position.x
        );
        assert!((p.plate_diameter_px - D).abs() < 1e-9);
        let plate_only: Vec<_> = dets
            .iter()
            .map(|f| {
                frame(
                    f.frame_index,
                    f.candidates
                        .iter()
                        .filter(|c| c.center.x < 200.0)
                        .cloned()
                        .collect(),
                )
            })
            .collect();
        let alone = propose_seed(&plate_only, &cfg()).unwrap();
        assert_eq!(p.lift.lift, alone.lift.lift);
        assert_eq!(p.frame_index, alone.frame_index);
    }

    #[test]
    fn plate_occluded_thirty_percent_of_samples_is_still_proposed() {
        let dets = to_frames(
            rising_strided(0, 100, 4, 1.5, R)
                .into_iter()
                .enumerate()
                .filter(|(k, _)| ![2, 5, 8].contains(&(k % 10)))
                .map(|(_, v)| v)
                .collect(),
        );
        let p = propose_seed(&dets, &cfg()).expect("70% persistent plate survives");
        assert!((p.position.x - 300.0).abs() < 1.0);
    }

    #[test]
    fn min_track_persistence_clamps_and_rejects_nan() {
        assert_eq!(cfg().min_track_persistence(), 0.5);
        let hi = SeedProposalConfig::builder()
            .min_track_persistence(7.0)
            .build();
        assert_eq!(hi.min_track_persistence(), 1.0);
        let lo = SeedProposalConfig::builder()
            .min_track_persistence(-1.0)
            .build();
        assert_eq!(lo.min_track_persistence(), 0.0);
        let nan = SeedProposalConfig::builder()
            .min_track_persistence(f64::NAN)
            .build();
        assert_eq!(nan.min_track_persistence(), 0.5);
    }

    #[test]
    fn persistence_filter_runs_before_ranking_not_after() {
        // Persistent plate barely moves (static test fails); the chain has
        // huge range. The chain must not be proposed either way.
        let dets = persistent_plate_and_false_chain(0.2, |_| true);
        if let Some(p) = propose_seed(&dets, &cfg()) {
            assert!(
                (p.position.x - 100.0).abs() < 1.0,
                "chain proposed at x={}",
                p.position.x
            );
        }
    }
}
