//! Shared overlay-video render loop (10.3): re-decodes the source video and
//! burns in the bar path overlay (`render_overlay`, 3.1) frame by frame,
//! encoding the result via `FfmpegVideoSink` (3.2). Extracted out of
//! `cli.rs` (previously CLI-only, task 3.4) so the GUI's auto-export
//! (10.3) can call the exact same render loop from a background thread
//! instead of reimplementing it.

use std::path::Path;

use tracker_core::{render_overlay, render_rep_bottoms, OverlayStyle, VideoSink};

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
