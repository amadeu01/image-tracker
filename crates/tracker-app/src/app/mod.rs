//! egui app shell (task 2.3): open a video, show a frame, scrub through it.
//!
//! Split (task 7.2, audit finding: this file was 927 lines) into:
//! - `state.rs` — `AppState`, `Mode`, `Seed`, the workflow-step/event-ring
//!   logic behind the side panel, and (nearly) all the unit tests. No egui
//!   `Context` dependency.
//! - `toolbar.rs` — the top toolbar (seed/calibrate toggles, Track/Resume).
//! - `bottom_bar.rs` — the one-line status summary + scrub bar.
//! - `side_panel.rs` — the guide/status/events panel (new in 7.2).
//! - `video_panel.rs` — the central frame view, click handling, overlays.
//!
//! This file just owns `TrackerApp` (state + texture/cache) and wires the
//! pieces together in `eframe::App::update`. Public surface is unchanged:
//! callers still just use `app::run`, `app::AppState`, etc. (re-exported
//! below), so `main.rs` didn't need to change.

mod bottom_bar;
mod side_panel;
mod state;
mod toolbar;
mod video_panel;

pub use state::{AppState, Mode, Seed, DEFAULT_CALIBRATION_LENGTH_METERS};

use std::path::PathBuf;

use eframe::egui;

use crate::ffprobe::VideoMetadata;
use crate::frame_cache::FrameCache;
use crate::seek_source::SeekingFrameDecoder;

/// The eframe `App`. Thin by design: state transitions live on `AppState`
/// (tested in `state.rs`); this struct wires that state to egui widgets and
/// to the seek-based frame cache (see `frame_cache.rs` for why: a scrub bar
/// needs random access but full-video caching is too much memory).
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
            metadata.display_width(),
            metadata.display_height(),
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
                let handle = ctx.load_texture("current-frame", image, egui::TextureOptions::LINEAR);
                self.texture = Some(handle);
                self.texture_frame = Some(self.state.current_frame);
                self.state.status.clear();
            }
            Err(e) => {
                tracing::error!(frame = self.state.current_frame, error = %e, "failed to decode frame");
                self.state.status =
                    format!("failed to decode frame {}: {e}", self.state.current_frame);
            }
        }
    }
}

impl eframe::App for TrackerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.state.poll_tracking() {
            ctx.request_repaint();
        }
        if self.state.poll_export() {
            ctx.request_repaint();
        }
        // While a run/export is active, keep repainting so progress keeps
        // flowing even if nothing else prompts a redraw.
        if self.state.tracking.is_some() || self.state.export.is_some() {
            ctx.request_repaint();
        }
        self.ensure_texture(ctx);

        toolbar::show(ctx, &mut self.state);
        bottom_bar::show_status_bar(ctx, &self.state);
        bottom_bar::show_scrub_bar(ctx, &mut self.state);
        // Side panel before the central panel so it claims its space first
        // (egui lays out panels in call order); the video then fills
        // whatever's left instead of an empty area to its right.
        side_panel::show(ctx, &self.state);
        video_panel::show(self, ctx);
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
