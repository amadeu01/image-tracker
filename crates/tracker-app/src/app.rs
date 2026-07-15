//! egui app shell (task 2.3): open a video, show a frame, scrub through it.
//!
//! `AppState` separates pure(ish) state (current frame index, mode, status
//! message) from the egui `App` impl, which is intentionally thin — egui
//! rendering itself isn't unit-tested, but the state transitions it drives
//! (frame index clamping) are, via `frame_cache::clamp_frame_index`.
//!
//! `Mode` exists now (with a single variant) so tasks 2.4 (seed placement)
//! and 2.5 (calibration) can add variants and match on `state.mode` without
//! restructuring this file.

use std::path::PathBuf;

use eframe::egui;

use crate::ffprobe::VideoMetadata;
use crate::frame_cache::{clamp_frame_index, FrameCache};
use crate::seek_source::SeekingFrameDecoder;

/// What clicking on the frame view currently does. 2.4 will add
/// `PlacingSeed`, 2.5 will add `Calibrating { .. }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Just look around: scrub the slider, no click handling yet.
    ViewOnly,
}

/// UI/session state, independent of egui so the index-clamping logic can be
/// unit-tested without a `Context`.
pub struct AppState {
    pub video_path: PathBuf,
    pub metadata: VideoMetadata,
    pub mode: Mode,
    pub current_frame: u64,
    /// Bottom status bar text; errors surface here rather than panicking
    /// (project rule — see PLAN.md 2.6).
    pub status: String,
}

impl AppState {
    pub fn new(video_path: PathBuf, metadata: VideoMetadata) -> Self {
        Self {
            video_path,
            metadata,
            mode: Mode::ViewOnly,
            current_frame: 0,
            status: String::new(),
        }
    }

    fn frame_count(&self) -> u64 {
        self.metadata.frame_count.unwrap_or(1)
    }

    /// Set the current frame, clamped to the valid range for this video.
    pub fn set_frame(&mut self, requested: i64) {
        self.current_frame = clamp_frame_index(requested, self.frame_count());
    }

    pub fn next_frame(&mut self) {
        self.set_frame(self.current_frame as i64 + 1);
    }

    pub fn prev_frame(&mut self) {
        self.set_frame(self.current_frame as i64 - 1);
    }
}

/// The eframe `App`. Thin by design: state transitions live on `AppState`
/// (tested above); this struct wires that state to egui widgets and to the
/// seek-based frame cache (see `frame_cache.rs` for why: a scrub bar needs
/// random access but full-video caching is too much memory).
pub struct TrackerApp {
    pub state: AppState,
    cache: FrameCache<SeekingFrameDecoder>,
    texture: Option<egui::TextureHandle>,
    /// Which frame `texture` currently shows, so we don't re-upload every
    /// frame when nothing changed.
    texture_frame: Option<u64>,
}

impl TrackerApp {
    pub fn new(video_path: PathBuf, metadata: VideoMetadata) -> Self {
        let decoder = SeekingFrameDecoder::new(
            video_path.clone(),
            metadata.width,
            metadata.height,
            metadata.fps_num,
            metadata.fps_den,
        );
        Self {
            state: AppState::new(video_path, metadata),
            cache: FrameCache::new(decoder, 16),
            texture: None,
            texture_frame: None,
        }
    }

    fn ensure_texture(&mut self, ctx: &egui::Context) {
        if self.texture_frame == Some(self.state.current_frame) {
            return; // already showing the right frame
        }
        match self.cache.get(self.state.current_frame) {
            Ok(frame) => {
                let size = [frame.width() as usize, frame.height() as usize];
                let image = egui::ColorImage::from_rgb(size, frame.rgb());
                let handle = ctx.load_texture(
                    "current-frame",
                    image,
                    egui::TextureOptions::LINEAR,
                );
                self.texture = Some(handle);
                self.texture_frame = Some(self.state.current_frame);
                self.state.status.clear();
            }
            Err(e) => {
                self.state.status = format!("failed to decode frame {}: {e}", self.state.current_frame);
            }
        }
    }
}

impl eframe::App for TrackerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ensure_texture(ctx);

        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "{}  |  frame {}/{}",
                    self.state.video_path.display(),
                    self.state.current_frame,
                    self.state.metadata.frame_count.unwrap_or(0).saturating_sub(1),
                ));
                if !self.state.status.is_empty() {
                    ui.separator();
                    ui.colored_label(egui::Color32::RED, &self.state.status);
                }
            });
        });

        egui::TopBottomPanel::bottom("scrub_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("<< prev").clicked() {
                    self.state.prev_frame();
                }
                let max = self.state.metadata.frame_count.unwrap_or(1).saturating_sub(1);
                let mut frame_val = self.state.current_frame;
                let slider = ui.add(egui::Slider::new(&mut frame_val, 0..=max));
                if slider.changed() {
                    self.state.set_frame(frame_val as i64);
                }
                if ui.button("next >>").clicked() {
                    self.state.next_frame();
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(texture) = &self.texture {
                let available = ui.available_size();
                let tex_size = texture.size_vec2();
                let scale = (available.x / tex_size.x).min(available.y / tex_size.y).min(1.0);
                ui.image((texture.id(), tex_size * scale));
            } else {
                ui.label("decoding first frame...");
            }
        });
    }
}

/// Runs the app: creates the native window and hands control to eframe's
/// event loop. Not called from unit tests (no display in CI); `main.rs`
/// invokes this after CLI parsing + ffprobe succeed.
pub fn run(video_path: PathBuf, metadata: VideoMetadata) -> eframe::Result<()> {
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "image-tracker",
        options,
        Box::new(move |_cc| Ok(Box::new(TrackerApp::new(video_path, metadata)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(frame_count: Option<u64>) -> VideoMetadata {
        VideoMetadata {
            width: 4,
            height: 4,
            fps_num: 30,
            fps_den: 1,
            frame_count,
        }
    }

    #[test]
    fn starts_at_frame_zero() {
        let state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        assert_eq!(state.current_frame, 0);
    }

    #[test]
    fn next_frame_advances_by_one() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.next_frame();
        assert_eq!(state.current_frame, 1);
    }

    #[test]
    fn prev_frame_at_zero_stays_at_zero() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.prev_frame();
        assert_eq!(state.current_frame, 0);
    }

    #[test]
    fn next_frame_at_last_index_stays_put() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.set_frame(9);
        state.next_frame();
        assert_eq!(state.current_frame, 9);
    }

    #[test]
    fn set_frame_clamps_out_of_range_requests() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.set_frame(1000);
        assert_eq!(state.current_frame, 9);
        state.set_frame(-5);
        assert_eq!(state.current_frame, 0);
    }

    #[test]
    fn missing_frame_count_treated_as_single_frame_video() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(None));
        state.next_frame();
        assert_eq!(state.current_frame, 0);
    }
}
