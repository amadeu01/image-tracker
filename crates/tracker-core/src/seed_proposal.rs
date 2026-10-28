//! Seed proposal: pick the moving plate from per-frame plate detections.
//!
//! Candidates are linked frame to frame by nearest-neighbour continuity, with
//! the link gate expressed in plate-diameter units. Tracks with negligible
//! vertical travel (floor plates, rack hardware) are dropped, and the track
//! with the largest vertical travel wins. No pixel scale is baked in.
//!
//! `PlateCandidate::score` saturates and is only coarsely comparable across
//! frames, so confidence is derived from track length, persistence and
//! vertical travel, not from score differences.

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
    /// Frame of the track's first stable detection.
    pub frame_index: u64,
    /// Bar Point there (sleeve end when found, else plate centre).
    pub position: Point,
    /// Median plate diameter over the track, from the detections.
    pub plate_diameter_px: f64,
    pub lift: LiftClassification,
    /// In `[0, 1]`.
    pub confidence: f64,
}

/// Thresholds for `propose_seed`, in plate-diameter units or fractions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SeedProposalConfig {
    link_gate_plates_per_frame: f64,
    max_gap_frames: u64,
    min_track_points: usize,
    min_vertical_travel_plates: f64,
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
            link_gate_plates_per_frame: 0.5,
            max_gap_frames: 5,
            min_track_points: 5,
            min_vertical_travel_plates: 0.5,
            full_confidence_points: 30,
            full_confidence_travel_plates: 2.0,
            lift_classifier: LiftClassifierConfig::default_config(),
        }
    }

    /// Max link distance per elapsed frame, in plate diameters.
    pub fn link_gate_plates_per_frame(&self) -> f64 {
        self.link_gate_plates_per_frame
    }
    /// Frames a track may go undetected and still be continued.
    pub fn max_gap_frames(&self) -> u64 {
        self.max_gap_frames
    }
    /// Minimum linked detections for a track to count.
    pub fn min_track_points(&self) -> usize {
        self.min_track_points
    }
    /// Minimum vertical range, in plate diameters, for a track to count.
    pub fn min_vertical_travel_plates(&self) -> f64 {
        self.min_vertical_travel_plates
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
    pub fn max_gap_frames(mut self, v: u64) -> Self {
        self.cfg.max_gap_frames = v;
        self
    }
    pub fn min_track_points(mut self, v: usize) -> Self {
        self.cfg.min_track_points = v;
        self
    }
    pub fn min_vertical_travel_plates(mut self, v: f64) -> Self {
        self.cfg.min_vertical_travel_plates = v;
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
    pub fn build(self) -> SeedProposalConfig {
        self.cfg
    }
}

/// One linked detection.
#[derive(Debug, Clone, Copy)]
struct Linked {
    frame_index: u64,
    position: Point,
    diameter_px: f64,
}

fn bar_point(c: &PlateCandidate) -> Point {
    c.sleeve_end.unwrap_or(c.center)
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

fn link_tracks(detections: &[FrameDetections], config: &SeedProposalConfig) -> Vec<Vec<Linked>> {
    let mut frames: Vec<&FrameDetections> = detections.iter().collect();
    frames.sort_by_key(|f| f.frame_index);

    let mut tracks: Vec<Vec<Linked>> = Vec::new();
    for frame in frames {
        let existing = tracks.len();
        let mut claimed = vec![false; existing];
        for cand in &frame.candidates {
            let pos = bar_point(cand);
            let diameter = cand.radius_px * 2.0;
            if !(diameter.is_finite() && diameter > 0.0) {
                continue;
            }
            let mut best: Option<(usize, f64)> = None;
            for (i, track) in tracks.iter().enumerate().take(existing) {
                if claimed[i] {
                    continue;
                }
                let last = track[track.len() - 1];
                let elapsed = frame.frame_index.saturating_sub(last.frame_index);
                if elapsed == 0 || elapsed > config.max_gap_frames + 1 {
                    continue;
                }
                let dist = ((pos.x - last.position.x).powi(2) + (pos.y - last.position.y).powi(2))
                    .sqrt()
                    / last.diameter_px;
                if dist <= config.link_gate_plates_per_frame * elapsed as f64
                    && best.is_none_or(|(_, d)| dist < d)
                {
                    best = Some((i, dist));
                }
            }
            let linked = Linked {
                frame_index: frame.frame_index,
                position: pos,
                diameter_px: diameter,
            };
            match best {
                Some((i, _)) => {
                    claimed[i] = true;
                    tracks[i].push(linked);
                }
                None => tracks.push(vec![linked]),
            }
        }
    }
    tracks
}

fn vertical_travel_plates(track: &[Linked]) -> f64 {
    let mut diameters: Vec<f64> = track.iter().map(|p| p.diameter_px).collect();
    let d = median(&mut diameters);
    let (lo, hi) = track
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
            (lo.min(p.position.y), hi.max(p.position.y))
        });
    (hi - lo) / d
}

/// Propose the seed for tracking: the first stable point of the moving plate.
///
/// Returns `None` when no track is long enough and travels far enough, so a
/// static floor plate or rack hardware is never proposed.
pub fn propose_seed(
    detections: &[FrameDetections],
    config: &SeedProposalConfig,
) -> Option<SeedProposal> {
    let tracks = link_tracks(detections, config);
    let (track, travel) = tracks
        .iter()
        .filter(|t| t.len() >= config.min_track_points)
        .map(|t| (t, vertical_travel_plates(t)))
        .filter(|(_, travel)| *travel >= config.min_vertical_travel_plates)
        .max_by(|a, b| a.1.total_cmp(&b.1))?;

    let mut diameters: Vec<f64> = track.iter().map(|p| p.diameter_px).collect();
    let plate_diameter_px = median(&mut diameters);

    let points: Vec<TrackPoint> = track
        .iter()
        .map(|p| TrackPoint {
            frame_index: p.frame_index,
            position: p.position,
            plate_diameter_px: p.diameter_px,
        })
        .collect();
    let lift = classify_lift(&points, &config.lift_classifier);

    let first = track[0];
    let last = track[track.len() - 1];
    let span = (last.frame_index - first.frame_index + 1) as f64;
    let length_term = (track.len() as f64 / config.full_confidence_points.max(1) as f64).min(1.0);
    let travel_term = (travel / config.full_confidence_travel_plates).min(1.0);
    let persistence_term = (track.len() as f64 / span).min(1.0);
    let confidence =
        (0.4 * length_term + 0.4 * travel_term + 0.2 * persistence_term).clamp(0.0, 1.0);

    Some(SeedProposal {
        frame_index: first.frame_index,
        position: first.position,
        plate_diameter_px,
        lift,
        confidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lift::LiftType;

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

    /// Plate rising 3 diameters over `n` frames starting at `start`.
    fn rising(start: u64, n: u64, radius: f64) -> Vec<(u64, PlateCandidate)> {
        let d = radius * 2.0;
        (0..n)
            .map(|k| {
                let y = 900.0 - 3.0 * d * k as f64 / (n - 1) as f64;
                (start + k, cand(300.0, y, radius))
            })
            .collect()
    }

    fn cfg() -> SeedProposalConfig {
        SeedProposalConfig::default_config()
    }

    #[test]
    fn static_plate_present_in_every_frame_is_never_proposed() {
        let dets: Vec<_> = (0..60)
            .map(|i| frame(i, vec![cand(500.0, 800.0, 60.0)]))
            .collect();
        assert!(propose_seed(&dets, &cfg()).is_none());
    }

    #[test]
    fn proposal_starts_at_first_stable_detection_not_frame_zero() {
        let mut dets: Vec<_> = (0..100).map(|i| frame(i, vec![])).collect();
        for (i, c) in rising(100, 40, 60.0) {
            dets.push(frame(i, vec![c]));
        }
        let p = propose_seed(&dets, &cfg()).unwrap();
        assert!(p.frame_index >= 100);
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
        let dets: Vec<_> = rising(0, 40, 60.0)
            .into_iter()
            .map(|(i, c)| frame(i, vec![c]))
            .collect();
        let p = propose_seed(&dets, &cfg()).unwrap();
        assert_eq!(p.lift.lift, LiftType::Deadlift);
        assert!((0.0..=1.0).contains(&p.confidence));
    }

    #[test]
    fn plate_diameter_comes_from_the_detections_not_a_constant() {
        for radius in [40.0, 90.0] {
            let dets: Vec<_> = rising(0, 40, radius)
                .into_iter()
                .map(|(i, c)| frame(i, vec![c]))
                .collect();
            let p = propose_seed(&dets, &cfg()).unwrap();
            assert!((p.plate_diameter_px - radius * 2.0).abs() < 1e-9);
        }
    }

    #[test]
    fn moving_bar_wins_over_static_floor_plate() {
        let dets: Vec<_> = rising(0, 40, 60.0)
            .into_iter()
            .map(|(i, c)| frame(i, vec![cand(900.0, 1000.0, 70.0), c]))
            .collect();
        let p = propose_seed(&dets, &cfg()).unwrap();
        assert!((p.position.x - 300.0).abs() < 1e-9);
    }

    #[test]
    fn detection_gap_in_the_middle_keeps_one_track() {
        let dets: Vec<_> = rising(0, 40, 60.0)
            .into_iter()
            .filter(|(i, _)| !(18..22).contains(i))
            .map(|(i, c)| frame(i, vec![c]))
            .collect();
        let p = propose_seed(&dets, &cfg()).unwrap();
        assert_eq!(p.frame_index, 0);
        // Travel covers the whole rise, so the track was not split.
        assert_eq!(p.lift.lift, LiftType::Deadlift);
    }

    #[test]
    fn sleeve_end_is_preferred_as_position() {
        let dets: Vec<_> = rising(0, 40, 60.0)
            .into_iter()
            .map(|(i, mut c)| {
                c.sleeve_end = Some(Point::new(c.center.x + 5.0, c.center.y));
                frame(i, vec![c])
            })
            .collect();
        let p = propose_seed(&dets, &cfg()).unwrap();
        assert!((p.position.x - 305.0).abs() < 1e-9);
    }
}
