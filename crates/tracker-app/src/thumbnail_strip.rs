//! Pure math for the timeline thumbnail strip (task 10.6): which frame
//! indices to sample across a video, and how to downscale a decoded frame
//! to the strip's thumbnail size. No egui, no ffmpeg, no threads — this is
//! the TDD'd half; `thumbnail_worker.rs` is the thin decode-thread wiring
//! around it (mirrors `frame_cache.rs`/`seek_source.rs`'s split).

/// Target thumbnail height in pixels (10.6's "~80px height"). Width is
/// derived per-video from the source aspect ratio (`downscale_dimensions`).
pub const THUMBNAIL_HEIGHT: u32 = 80;

/// How many frames the strip samples across the video (10.6's "~20 evenly
/// spaced frames").
pub const THUMBNAIL_COUNT: usize = 20;

/// Picks `n` frame indices evenly spaced across `[0, total_frames - 1]`.
///
/// - `total_frames == 0` or `n == 0` -> empty (nothing to sample).
/// - `total_frames <= n` -> every frame, in order (no point sampling fewer
///   than one frame per slot).
/// - Otherwise -> `n` indices spread evenly from `0` to `total_frames - 1`
///   inclusive (first and last frame are always included), via integer
///   rounding so no float drift creeps into frame indices.
pub fn sample_frame_indices(total_frames: u64, n: usize) -> Vec<u64> {
    if total_frames == 0 || n == 0 {
        return Vec::new();
    }
    if total_frames <= n as u64 {
        return (0..total_frames).collect();
    }
    if n == 1 {
        return vec![0];
    }
    let last = total_frames - 1;
    (0..n)
        .map(|i| {
            // Round-to-nearest integer division: i * last / (n - 1).
            let num = i as u64 * last;
            let den = (n - 1) as u64;
            (num + den / 2) / den
        })
        .collect()
}

/// The output dimensions of downscaling a `src_w x src_h` frame to
/// `target_h` tall, preserving aspect ratio (nearest-neighbor downstream —
/// this just does the size math). Width is at least 1 even for degenerate
/// (zero-height) sources.
pub fn downscale_dimensions(src_w: u32, src_h: u32, target_h: u32) -> (u32, u32) {
    if src_h == 0 || src_w == 0 || target_h == 0 {
        return (1, target_h.max(1));
    }
    let w = ((src_w as u64 * target_h as u64) / src_h as u64).max(1) as u32;
    (w, target_h)
}

/// Nearest-neighbor downscale of a tightly-packed RGB24 buffer
/// (`src_w * src_h * 3` bytes) to `dst_w x dst_h` (also RGB24). Nearest
/// neighbor is fine for a ~80px-tall thumbnail (10.6's design note) —
/// there's no need for the quality of a proper filtered resize at this
/// size, and it's trivial to keep allocation-free per source pixel.
pub fn downscale_nearest_rgb(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Vec<u8> {
    let mut out = vec![0u8; dst_w as usize * dst_h as usize * 3];
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return out;
    }
    for dy in 0..dst_h {
        // Map dst row back to a src row, clamped into range.
        let sy = ((dy as u64 * src_h as u64) / dst_h as u64).min(src_h as u64 - 1) as u32;
        for dx in 0..dst_w {
            let sx = ((dx as u64 * src_w as u64) / dst_w as u64).min(src_w as u64 - 1) as u32;
            let src_idx = (sy as usize * src_w as usize + sx as usize) * 3;
            let dst_idx = (dy as usize * dst_w as usize + dx as usize) * 3;
            out[dst_idx..dst_idx + 3].copy_from_slice(&src[src_idx..src_idx + 3]);
        }
    }
    out
}

/// Which sampled slot (index into `indices`, as returned by
/// `sample_frame_indices`) best represents `current_frame` — the slot whose
/// sampled frame index is closest — so the strip can highlight a "you are
/// here" marker even though `current_frame` will almost never land exactly
/// on a sampled index. `None` for an empty strip.
pub fn nearest_slot(indices: &[u64], current_frame: u64) -> Option<usize> {
    indices
        .iter()
        .enumerate()
        .min_by_key(|(_, &idx)| idx.abs_diff(current_frame))
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_frame_indices_empty_video_is_empty() {
        assert_eq!(sample_frame_indices(0, 20), Vec::<u64>::new());
    }

    #[test]
    fn sample_frame_indices_zero_n_is_empty() {
        assert_eq!(sample_frame_indices(100, 0), Vec::<u64>::new());
    }

    #[test]
    fn sample_frame_indices_fewer_frames_than_n_returns_every_frame() {
        assert_eq!(sample_frame_indices(5, 20), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn sample_frame_indices_n_one_returns_first_frame() {
        assert_eq!(sample_frame_indices(1000, 1), vec![0]);
    }

    #[test]
    fn sample_frame_indices_spans_full_range_including_last_frame() {
        let idx = sample_frame_indices(2000, 20);
        assert_eq!(idx.len(), 20);
        assert_eq!(idx[0], 0);
        assert_eq!(*idx.last().unwrap(), 1999);
    }

    #[test]
    fn sample_frame_indices_is_monotonically_non_decreasing() {
        let idx = sample_frame_indices(2000, 20);
        for pair in idx.windows(2) {
            assert!(pair[1] >= pair[0]);
        }
    }

    #[test]
    fn sample_frame_indices_evenly_spaced_exact_division() {
        // 19 frames -> 20 samples but total < n so falls into the "every
        // frame" branch; use a case that divides evenly instead.
        let idx = sample_frame_indices(191, 20); // step of 10 exactly (190/19)
        let expected: Vec<u64> = (0..20).map(|i| i * 10).collect();
        assert_eq!(idx, expected);
    }

    #[test]
    fn downscale_dimensions_preserves_aspect_ratio() {
        assert_eq!(downscale_dimensions(1024, 576, 80), (142, 80));
    }

    #[test]
    fn downscale_dimensions_degenerate_source_falls_back_to_1_wide() {
        assert_eq!(downscale_dimensions(0, 0, 80), (1, 80));
    }

    #[test]
    fn downscale_nearest_rgb_output_has_expected_length() {
        let src = vec![0u8; 4 * 4 * 3];
        let out = downscale_nearest_rgb(&src, 4, 4, 2, 2);
        assert_eq!(out.len(), 2 * 2 * 3);
    }

    #[test]
    fn downscale_nearest_rgb_samples_correct_source_pixels() {
        // 2x2 source, each pixel a distinct color; downscale to 2x2 (identity
        // via nearest-neighbor) should reproduce it exactly.
        let src: Vec<u8> = vec![
            255, 0, 0, // (0,0) red
            0, 255, 0, // (1,0) green
            0, 0, 255, // (0,1) blue
            255, 255, 0, // (1,1) yellow
        ];
        let out = downscale_nearest_rgb(&src, 2, 2, 2, 2);
        assert_eq!(out, src);
    }

    #[test]
    fn downscale_nearest_rgb_downscales_4x4_to_1x1_picks_a_valid_pixel() {
        let mut src = vec![0u8; 4 * 4 * 3];
        // Make every pixel distinct-ish so we can confirm it's not just zeros.
        for (i, b) in src.iter_mut().enumerate() {
            *b = (i % 256) as u8;
        }
        let out = downscale_nearest_rgb(&src, 4, 4, 1, 1);
        assert_eq!(out.len(), 3);
        // Should be some real pixel from src, not left as the zero-init default
        // unless src[0..3] happens to be 0,1,2 (which it does) -- so just
        // assert it's exactly one of the 16 source pixels.
        let found = src.chunks(3).any(|px| px == out.as_slice());
        assert!(found);
    }

    #[test]
    fn nearest_slot_picks_closest_sampled_index() {
        let indices = vec![0, 10, 20, 30];
        assert_eq!(nearest_slot(&indices, 0), Some(0));
        assert_eq!(nearest_slot(&indices, 4), Some(0));
        assert_eq!(nearest_slot(&indices, 6), Some(1));
        assert_eq!(nearest_slot(&indices, 25), Some(2));
        assert_eq!(nearest_slot(&indices, 30), Some(3));
        assert_eq!(nearest_slot(&indices, 1000), Some(3));
    }

    #[test]
    fn nearest_slot_empty_is_none() {
        assert_eq!(nearest_slot(&[], 5), None);
    }
}
