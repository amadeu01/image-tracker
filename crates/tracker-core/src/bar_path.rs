//! `BarPath`: the read-only aggregate consumed by kinematics (5.x) and
//! export (3.3). Combines a `TrackingSession`'s `Sample`/`Gap` output with a
//! `Timebase` (per-video fps) so callers can ask "what time is frame N?"
//! and iterate `(t_seconds, Point, Source)` triples.
//!
//! See CONTEXT.md's "Bar Path" term.

use crate::geometry::Point;
use crate::session::{Gap, Sample, Source};

/// Converts frame indices to timestamps given a rational frames-per-second
/// rate (e.g. `600/19` or `60000/1001`), as reported by ffprobe.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timebase {
    fps_num: u64,
    fps_den: u64,
}

/// Error constructing a `Timebase` from a degenerate rational fps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimebaseError {
    /// Numerator or denominator was zero (undefined or infinite fps).
    ZeroRate,
}

impl Timebase {
    /// Builds a `Timebase` from a rational fps (`num`/`den`). Rejects a
    /// zero numerator or denominator, both of which would make frame
    /// duration undefined or infinite.
    pub fn new(fps_num: u64, fps_den: u64) -> Result<Self, TimebaseError> {
        if fps_num == 0 || fps_den == 0 {
            return Err(TimebaseError::ZeroRate);
        }
        Ok(Self { fps_num, fps_den })
    }

    /// Frames per second as an `f64`.
    pub fn fps(&self) -> f64 {
        self.fps_num as f64 / self.fps_den as f64
    }

    /// Timestamp in seconds for `frame_index` (0-based, relative to the
    /// start of the video).
    pub fn timestamp(&self, frame_index: u64) -> f64 {
        frame_index as f64 * self.fps_den as f64 / self.fps_num as f64
    }
}

/// A single point on the Bar Path, with its timestamp and provenance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathPoint {
    /// Video-absolute frame index (session-relative index + start_frame).
    pub frame_index: u64,
    pub t_seconds: f64,
    pub position: Point,
    pub source: Source,
    /// Identity confidence (17.4), carried through from the source `Sample`:
    /// `Some(anchor_score)` for a tracked point, `None` for interpolated,
    /// seed, or reseed points.
    pub confidence: Option<f64>,
}

/// The Bar Path aggregate: a `TrackingSession`'s positions and gaps, with a
/// `Timebase` attached so every sample has a real-world timestamp. Built
/// once from a session's output; read-only thereafter.
#[derive(Debug, Clone, PartialEq)]
pub struct BarPath {
    timebase: Timebase,
    start_frame: u64,
    points: Vec<PathPoint>,
    gaps: Vec<Gap>,
}

impl BarPath {
    /// Builds a `BarPath` from a session's `samples` and `gaps`, a
    /// `timebase`, and `start_frame` — the frame index (in the source
    /// video) that the session's own frame 0 corresponds to. Sample frame
    /// indices are relative to the session; `start_frame` shifts them into
    /// video-absolute frame indices before timestamping.
    pub fn new(samples: &[Sample], gaps: &[Gap], timebase: Timebase, start_frame: u64) -> Self {
        let points = samples
            .iter()
            .map(|s| {
                let frame_index = start_frame + s.frame_index;
                PathPoint {
                    frame_index,
                    t_seconds: timebase.timestamp(frame_index),
                    position: s.position,
                    source: s.source,
                    confidence: s.confidence,
                }
            })
            .collect();
        Self {
            timebase,
            start_frame,
            points,
            gaps: gaps.to_vec(),
        }
    }

    /// Builds a `BarPath` directly from an already-computed slice of
    /// `PathPoint`s (e.g. a rep's own frame-bounded segment sliced out of a
    /// larger path's `points()`), with no gaps recorded. Used by callers
    /// that need a `BarPath` to hand to `render_overlay`/`render_rep_bottoms`
    /// scoped to a subset of points — those functions read `points()`/
    /// `position_at`, not `gaps()`, so an empty gap list is a safe default
    /// (task 19.3: burning a per-rep overlay into a rep clip, where the
    /// trailing path must only ever show that rep's own frames).
    pub fn from_points(points: Vec<PathPoint>, timebase: Timebase, start_frame: u64) -> Self {
        Self {
            timebase,
            start_frame,
            points,
            gaps: Vec::new(),
        }
    }

    /// The timebase this path was built with.
    pub fn timebase(&self) -> Timebase {
        self.timebase
    }

    /// The video-absolute frame index that this path's session started at.
    pub fn start_frame(&self) -> u64 {
        self.start_frame
    }

    /// Iterates the path's points in frame order.
    pub fn points(&self) -> &[PathPoint] {
        &self.points
    }

    /// The gaps recorded by the tracking session, unchanged.
    pub fn gaps(&self) -> &[Gap] {
        &self.gaps
    }

    /// Total duration spanned by the path, in seconds: the timestamp of
    /// the last point minus the timestamp of the first. `0.0` for an empty
    /// or single-point path.
    pub fn duration_seconds(&self) -> f64 {
        match (self.points.first(), self.points.last()) {
            (Some(first), Some(last)) => last.t_seconds - first.t_seconds,
            _ => 0.0,
        }
    }

    /// Looks up the point recorded at video-absolute `frame_index`, if
    /// any. Session-relative frame indices are shifted by `start_frame`
    /// before lookup, so callers use the same frame numbering as the
    /// source video throughout.
    pub fn position_at(&self, frame_index: u64) -> Option<PathPoint> {
        self.points
            .iter()
            .find(|p| p.frame_index == frame_index)
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timebase_rejects_zero_numerator_or_denominator() {
        assert_eq!(Timebase::new(0, 1), Err(TimebaseError::ZeroRate));
        assert_eq!(Timebase::new(30, 0), Err(TimebaseError::ZeroRate));
    }

    #[test]
    fn timebase_converts_frame_index_to_seconds_for_odd_rational_fps() {
        // 600/19 fps: frame duration = 19/600 s.
        let tb = Timebase::new(600, 19).unwrap();
        assert!((tb.fps() - 31.578_947_368_421_05).abs() < 1e-9);
        assert_eq!(tb.timestamp(0), 0.0);
        let expected = 19.0 / 600.0;
        assert!((tb.timestamp(1) - expected).abs() < 1e-12);
        assert!((tb.timestamp(19) - 19.0 * expected).abs() < 1e-9);
    }

    #[test]
    fn timebase_handles_ntsc_style_rate() {
        // 60000/1001 fps ("59.94"): frame duration = 1001/60000 s.
        let tb = Timebase::new(60_000, 1001).unwrap();
        let expected = 1001.0 / 60_000.0;
        assert!((tb.timestamp(1) - expected).abs() < 1e-12);
    }

    #[test]
    fn from_points_builds_a_bar_path_with_no_gaps_from_a_raw_point_slice() {
        let tb = Timebase::new(30, 1).unwrap();
        let points = vec![
            PathPoint {
                frame_index: 10,
                t_seconds: tb.timestamp(10),
                position: Point::new(1.0, 2.0),
                source: Source::Tracked,
                confidence: Some(0.9),
            },
            PathPoint {
                frame_index: 11,
                t_seconds: tb.timestamp(11),
                position: Point::new(1.5, 2.5),
                source: Source::Tracked,
                confidence: Some(0.9),
            },
        ];
        let path = BarPath::from_points(points.clone(), tb, 10);
        assert_eq!(path.points(), points.as_slice());
        assert_eq!(path.start_frame(), 10);
        assert_eq!(path.timebase(), tb);
        assert!(path.gaps().is_empty());
    }

    fn sample(frame_index: u64, x: f64, y: f64, source: Source) -> Sample {
        Sample {
            frame_index,
            position: Point::new(x, y),
            source,
            confidence: None,
        }
    }

    #[test]
    fn bar_path_builds_points_with_timestamps_from_samples() {
        let tb = Timebase::new(600, 19).unwrap();
        let samples = vec![
            sample(0, 10.0, 20.0, Source::Tracked),
            sample(1, 11.0, 20.5, Source::Tracked),
            sample(2, 12.0, 21.0, Source::Interpolated),
        ];
        let path = BarPath::new(&samples, &[], tb, 0);

        let points = path.points();
        assert_eq!(points.len(), 3);
        assert_eq!(points[0].t_seconds, 0.0);
        assert_eq!(points[0].position, Point::new(10.0, 20.0));
        assert_eq!(points[0].source, Source::Tracked);
        assert!((points[1].t_seconds - 19.0 / 600.0).abs() < 1e-12);
        assert_eq!(points[2].source, Source::Interpolated);
    }

    #[test]
    fn bar_path_shifts_timestamps_by_start_frame_offset() {
        let tb = Timebase::new(30, 1).unwrap();
        let samples = vec![sample(0, 5.0, 5.0, Source::Tracked)];
        // Session started at video-absolute frame 100.
        let path = BarPath::new(&samples, &[], tb, 100);
        assert_eq!(path.start_frame(), 100);
        assert!((path.points()[0].t_seconds - 100.0 / 30.0).abs() < 1e-12);
    }

    #[test]
    fn bar_path_exposes_gaps_unchanged() {
        let tb = Timebase::new(30, 1).unwrap();
        let samples = vec![sample(0, 0.0, 0.0, Source::Tracked)];
        let gaps = vec![Gap { start: 1, end: 3 }];
        let path = BarPath::new(&samples, &gaps, tb, 0);
        assert_eq!(path.gaps(), &[Gap { start: 1, end: 3 }]);
    }

    #[test]
    fn bar_path_duration_is_last_minus_first_timestamp() {
        let tb = Timebase::new(30, 1).unwrap();
        let samples = vec![
            sample(0, 0.0, 0.0, Source::Tracked),
            sample(1, 1.0, 1.0, Source::Tracked),
            sample(2, 2.0, 2.0, Source::Tracked),
        ];
        let path = BarPath::new(&samples, &[], tb, 0);
        assert!((path.duration_seconds() - 2.0 / 30.0).abs() < 1e-12);
    }

    #[test]
    fn bar_path_duration_is_zero_for_empty_or_single_point_path() {
        let tb = Timebase::new(30, 1).unwrap();
        assert_eq!(BarPath::new(&[], &[], tb, 0).duration_seconds(), 0.0);

        let samples = vec![sample(0, 0.0, 0.0, Source::Tracked)];
        assert_eq!(BarPath::new(&samples, &[], tb, 0).duration_seconds(), 0.0);
    }

    #[test]
    fn bar_path_looks_up_position_by_video_absolute_frame_index() {
        let tb = Timebase::new(30, 1).unwrap();
        let samples = vec![
            sample(0, 1.0, 1.0, Source::Tracked),
            sample(1, 2.0, 2.0, Source::Tracked),
        ];
        // Session-relative frame 1, offset by start_frame 50 -> video frame 51.
        let path = BarPath::new(&samples, &[], tb, 50);

        let found = path.position_at(51).expect("frame 51 present");
        assert_eq!(found.position, Point::new(2.0, 2.0));

        assert!(path.position_at(999).is_none());
        // Below start_frame: no such video frame in this path.
        assert!(path.position_at(10).is_none());
    }
}
