//! Background decode thread for the timeline thumbnail strip (task 10.6):
//! decodes `thumbnail_strip::sample_frame_indices`' ~20 sampled frames once
//! per loaded video, downscales each to `THUMBNAIL_HEIGHT` px tall, and
//! streams them back to the UI thread over a channel — same
//! spawn-thread-plus-mpsc shape as `tracking.rs`'s worker, but with no
//! control channel (nothing to pause/resume/stop; it's a short, bounded
//! job) and its own `SeekingFrameDecoder` instance so it never contends with
//! `TrackerApp`'s main scrub-bar `FrameCache`.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use crate::frame_cache::FrameDecoder;
use crate::seek_source::SeekingFrameDecoder;
use crate::thumbnail_strip::{self, THUMBNAIL_HEIGHT};

/// One decoded-and-downscaled thumbnail, or the end-of-stream marker.
pub enum ThumbnailMessage {
    /// `slot` is the index into the sampled-indices list (and thus into the
    /// UI's `Vec<Option<TextureHandle>>`), not the video frame index —
    /// `frame_index` carries that separately (needed for click-to-jump).
    Thumb {
        slot: usize,
        frame_index: u64,
        width: u32,
        height: u32,
        rgb: Vec<u8>,
    },
    Done,
}

/// The UI-thread handle: just the receiving end of the channel plus the
/// frame indices the worker is decoding (known upfront, so the UI can lay
/// out placeholder boxes before any thumbnail has arrived).
pub struct ThumbnailHandle {
    pub messages: Receiver<ThumbnailMessage>,
    pub frame_indices: Vec<u64>,
}

/// Spawns the background thumbnail-decode thread for `video_path` and
/// returns a handle to poll. `frame_count` is the video's known frame
/// count (`VideoMetadata::frame_count`, defaulting the same way
/// `AppState::frame_count` does — 1 for an unknown/absent count, so at
/// worst a single-frame strip is sampled rather than panicking on an
/// unknown length).
pub fn spawn_thumbnails(
    video_path: PathBuf,
    width: u32,
    height: u32,
    fps_num: u64,
    fps_den: u64,
    frame_count: u64,
) -> ThumbnailHandle {
    let frame_indices =
        thumbnail_strip::sample_frame_indices(frame_count, thumbnail_strip::THUMBNAIL_COUNT);
    let (tx, rx) = mpsc::channel::<ThumbnailMessage>();

    let indices_for_thread = frame_indices.clone();
    thread::spawn(move || {
        let mut decoder = SeekingFrameDecoder::new(video_path, width, height, fps_num, fps_den);
        for (slot, &frame_index) in indices_for_thread.iter().enumerate() {
            match decoder.decode_frame(frame_index) {
                Ok(frame) => {
                    let (dst_w, dst_h) = thumbnail_strip::downscale_dimensions(
                        frame.width(),
                        frame.height(),
                        THUMBNAIL_HEIGHT,
                    );
                    let rgb = thumbnail_strip::downscale_nearest_rgb(
                        frame.rgb(),
                        frame.width(),
                        frame.height(),
                        dst_w,
                        dst_h,
                    );
                    if tx
                        .send(ThumbnailMessage::Thumb {
                            slot,
                            frame_index,
                            width: dst_w,
                            height: dst_h,
                            rgb,
                        })
                        .is_err()
                    {
                        // UI dropped the handle (e.g. a new video was opened
                        // mid-decode) -- nothing left to send to, stop early.
                        return;
                    }
                }
                Err(e) => {
                    tracing::warn!(frame = frame_index, error = %e, "thumbnail decode failed, skipping");
                }
            }
        }
        let _ = tx.send(ThumbnailMessage::Done);
    });

    ThumbnailHandle {
        messages: rx,
        frame_indices,
    }
}
