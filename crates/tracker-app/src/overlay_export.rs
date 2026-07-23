//! Shared overlay-video render loop (10.3): re-decodes the source video and
//! burns in the bar path overlay (`render_overlay`, 3.1) frame by frame,
//! encoding the result via `FfmpegVideoSink` (3.2). Extracted out of
//! `cli.rs` (previously CLI-only, task 3.4) so the GUI's auto-export
//! (10.3) can call the exact same render loop from a background thread
//! instead of reimplementing it.

use std::path::Path;

use tracker_core::{
    render_overlay, render_rep_bottoms, BarPath, OverlayStyle, PathPoint, Rep, VideoSink,
};

use crate::ffmpeg_sink::FfmpegVideoSink;
use crate::ffmpeg_source::FfmpegFrameSource;

/// Renders `bar_path`'s overlay (plus rep-bottom markers, if any) onto
/// `video_path`, writing the result to `out_path`. Decodes only up to the
/// last frame the path covers, not the whole video, since tracking may
/// have stopped early.
#[allow(clippy::too_many_arguments)]
pub fn render_overlay_video(
    video_path: &Path,
    out_path: &Path,
    width: u32,
    height: u32,
    fps_num: u64,
    fps_den: u64,
    bar_path: &tracker_core::BarPath,
    reps: &[tracker_core::Rep],
) -> Result<(), String> {
    let last_frame = bar_path
        .points()
        .last()
        .map(|p| p.frame_index)
        .unwrap_or(bar_path.start_frame());

    // `Rep`s index into whatever slice `segment_reps` was given, which the
    // caller built from `velocity_series(bar_path.points(), ...)` -- one
    // sample per point, same order. So `bar_path.points()`'s frame indices
    // line up with `Rep::bottom` the same way the velocity slice's would.
    let frame_indices: Vec<u64> = bar_path.points().iter().map(|p| p.frame_index).collect();

    let mut source = FfmpegFrameSource::spawn(video_path, width, height)
        .map_err(|e| format!("failed to spawn ffmpeg decoder: {e}"))?;
    let mut sink = FfmpegVideoSink::spawn(out_path, width, height, fps_num, fps_den)
        .map_err(|e| format!("failed to spawn ffmpeg encoder: {e}"))?;

    let style = OverlayStyle::builder().build();

    let mut frame_index: u64 = 0;
    while let Some(mut frame) = source
        .next_frame_checked()
        .map_err(|e| format!("decode error at frame {frame_index}: {e}"))?
    {
        if frame_index > last_frame {
            break;
        }
        render_overlay(&mut frame, bar_path, frame_index, &style);
        render_rep_bottoms(
            &mut frame,
            bar_path,
            reps,
            &frame_indices,
            frame_index,
            &style,
        );
        sink.write_frame(&frame)
            .map_err(|e| format!("encode error at frame {frame_index}: {e}"))?;
        frame_index += 1;
    }

    sink.finish()
        .map_err(|e| format!("failed to finalize overlay video: {e}"))
}

// -- Task 19.3: per-rep bar-path overlay burned into a rep clip -----------

/// The subset of `points` whose `frame_index` falls in `[start, end]`
/// (inclusive), used to scope a burned-in path to one rep's own frames
/// rather than the whole set's trailing polyline. Pure and unit-testable —
/// `render_rep_clip_overlay` is the only (untestable-headlessly) caller.
/// `points` must already be sorted by ascending `frame_index` (true of
/// `BarPath::points()`, the only real caller), so this is a contiguous
/// `partition_point` slice, no allocation, mirroring
/// `SessionResults::path_points_to_draw` (19.1) exactly.
pub fn scope_points_to_rep_frames(points: &[PathPoint], start: u64, end: u64) -> &[PathPoint] {
    let lo = points.partition_point(|p| p.frame_index < start);
    let hi = points.partition_point(|p| p.frame_index <= end);
    &points[lo..hi]
}

/// Renders rep `rep`'s own bar-path segment (task 19.1's per-rep scoping,
/// reused here rather than the whole-set trail) burned into a clip covering
/// video-absolute frames `[bounds.0, bounds.1]`, writing the result to
/// `out_path`. Reuses the exact same `render_overlay`/`render_rep_bottoms`
/// renderer and `FfmpegVideoSink` encoder as `render_overlay_video` — only
/// the path fed to `render_overlay` and the frame range written differ.
///
/// `frame_indices` must be the *full* path's velocity-aligned frame index
/// table (as `render_overlay_video` builds it) since `rep.bottom` indexes
/// into it; only `rep` itself is passed to `render_rep_bottoms` so no other
/// rep's bottom tick leaks into this clip.
///
/// No seek support in `FfmpegFrameSource` (it always decodes from frame 0),
/// so this re-decodes the video from the start and skips frames before
/// `bounds.0`, writing only `[bounds.0, bounds.1]` to the sink — slower than
/// a stream-copy clip on a long video with a late rep, but correct, and rep
/// clips are exported as a background job (never on the UI thread) so the
/// extra decode time doesn't block anything.
#[allow(clippy::too_many_arguments)]
pub fn render_rep_clip_overlay(
    video_path: &Path,
    out_path: &Path,
    width: u32,
    height: u32,
    fps_num: u64,
    fps_den: u64,
    bar_path: &BarPath,
    rep: &Rep,
    frame_indices: &[u64],
    bounds: (u64, u64),
) -> Result<(), String> {
    let (start_frame, end_frame) = bounds;
    let timebase = bar_path.timebase();
    let scoped_points: Vec<PathPoint> =
        scope_points_to_rep_frames(bar_path.points(), start_frame, end_frame).to_vec();
    let scoped_path = BarPath::from_points(scoped_points, timebase, bar_path.start_frame());
    let reps = std::slice::from_ref(rep);

    let mut source = FfmpegFrameSource::spawn(video_path, width, height)
        .map_err(|e| format!("failed to spawn ffmpeg decoder: {e}"))?;
    let mut sink = FfmpegVideoSink::spawn(out_path, width, height, fps_num, fps_den)
        .map_err(|e| format!("failed to spawn ffmpeg encoder: {e}"))?;

    let style = OverlayStyle::builder().build();

    let mut frame_index: u64 = 0;
    while let Some(mut frame) = source
        .next_frame_checked()
        .map_err(|e| format!("decode error at frame {frame_index}: {e}"))?
    {
        if frame_index > end_frame {
            break;
        }
        if frame_index >= start_frame {
            render_overlay(&mut frame, &scoped_path, frame_index, &style);
            render_rep_bottoms(
                &mut frame,
                &scoped_path,
                reps,
                frame_indices,
                frame_index,
                &style,
            );
            sink.write_frame(&frame)
                .map_err(|e| format!("encode error at frame {frame_index}: {e}"))?;
        }
        frame_index += 1;
    }

    sink.finish()
        .map_err(|e| format!("failed to finalize rep clip overlay video: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracker_core::Source;

    fn point(frame_index: u64) -> PathPoint {
        PathPoint {
            frame_index,
            t_seconds: frame_index as f64 / 30.0,
            position: tracker_core::Point::new(0.0, 0.0),
            source: Source::Tracked,
            confidence: Some(0.9),
        }
    }

    #[test]
    fn scope_points_to_rep_frames_keeps_only_the_inclusive_bounds() {
        let points: Vec<PathPoint> = (0..10).map(point).collect();
        let scoped = scope_points_to_rep_frames(&points, 3, 6);
        let frames: Vec<u64> = scoped.iter().map(|p| p.frame_index).collect();
        assert_eq!(frames, vec![3, 4, 5, 6]);
    }

    #[test]
    fn scope_points_to_rep_frames_excludes_a_neighboring_reps_frames() {
        // Two adjacent reps' points in one path: rep 0 is [0,4], rep 1 is
        // [5,9]. Scoping to rep 1 must not leak rep 0's tail frame 4 or any
        // frame past rep 1's own end.
        let points: Vec<PathPoint> = (0..10).map(point).collect();
        let rep1 = scope_points_to_rep_frames(&points, 5, 9);
        assert!(rep1
            .iter()
            .all(|p| p.frame_index >= 5 && p.frame_index <= 9));
        assert_eq!(rep1.len(), 5);
    }

    #[test]
    fn scope_points_to_rep_frames_is_empty_when_bounds_dont_overlap_any_point() {
        let points: Vec<PathPoint> = (0..5).map(point).collect();
        assert!(scope_points_to_rep_frames(&points, 100, 200).is_empty());
    }
}
