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

mod banner;
mod bottom_bar;
mod palette;
mod settings_section;
mod side_panel;
mod state;
mod theme;
mod thumbnail_panel;
mod toolbar;
mod video_panel;

pub use state::{AppState, DisplayMode, Mode, Phase, Seed, DEFAULT_CALIBRATION_LENGTH_METERS};

use std::path::PathBuf;

use eframe::egui;

use crate::ffprobe::VideoMetadata;
use crate::frame_cache::FrameCache;
use crate::seek_source::SeekingFrameDecoder;
use crate::thumbnail_worker::{self, ThumbnailHandle, ThumbnailMessage};

/// Video extensions offered in the "Open video…" file dialog (10.5). Not
/// exhaustive (ffmpeg reads far more), just the common ones a user is likely
/// to have on disk.
const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "mkv", "avi", "webm"];

/// The eframe `App`. Thin by design: state transitions live on `AppState`
/// (tested in `state.rs`); this struct wires that state to egui widgets and
/// to the seek-based frame cache (see `frame_cache.rs` for why: a scrub bar
/// needs random access but full-video caching is too much memory).
///
/// `state`/`cache` are `None` in the empty state (10.5): no video loaded yet,
/// either because the app was started with no CLI arg or because an "Open
/// video" attempt hasn't succeeded yet. Every other field lives inside
/// `AppState` once a video *is* loaded, so there's a single source of truth
/// for "is a video open" — `state.is_some()`.
pub struct TrackerApp {
    pub state: Option<AppState>,
    cache: Option<FrameCache<SeekingFrameDecoder>>,
    texture: Option<egui::TextureHandle>,
    /// Which frame `texture` currently shows, so we don't re-upload every
    /// frame when nothing changed.
    texture_frame: Option<u64>,
    /// Message from the most recent failed "Open video" attempt, shown in
    /// the empty-state central panel (which has no `AppState.status` to
    /// write to). Cleared as soon as an open succeeds.
    pub open_error: Option<String>,
    /// The background thumbnail-decode worker for the current video (10.6),
    /// once spawned by `load_video`. `None` before the first video loads;
    /// dropping a previous handle here (on a second "Open video") also
    /// stops that worker sending any more messages into the void — see
    /// `thumbnail_worker::spawn_thumbnails`'s early-return-on-send-error.
    thumbnails: Option<ThumbnailHandle>,
    /// Decoded thumbnail textures, indexed the same way as
    /// `thumbnails.frame_indices` (slot `i` here <-> `frame_indices[i]`
    /// there). `None` entries are still-loading placeholders; sized to
    /// `frame_indices.len()` as soon as the handle is spawned so the strip
    /// can lay out every placeholder box immediately (10.6's "placeholder
    /// boxes fill in as thumbs arrive").
    thumbnail_textures: Vec<Option<egui::TextureHandle>>,
    /// User's explicit theme override (task 12.4), persisted via
    /// `theme::save_override`/loaded via `theme::load_override`. `None`
    /// means "no override yet" — egui/winit already applied the system
    /// theme before the first frame, and `update` leaves it alone rather
    /// than fighting further `ThemeChanged` events.
    theme_override: Option<bool>,
    /// User's explicit filmstrip open/closed override (task 13.7). `None`
    /// means "use the default": open pre-tracking, collapsed once results
    /// exist (the design replaced the strip with the segment scrub as the
    /// primary navigation). See `thumbnail_panel::show`.
    pub filmstrip_override: Option<bool>,
}

impl TrackerApp {
    /// Starts with a video already loaded (CLI-arg path, unchanged from
    /// before 10.5).
    pub fn new(video_path: PathBuf, metadata: VideoMetadata) -> Self {
        let mut app = Self::empty();
        app.load_video(video_path, metadata);
        app
    }

    /// Starts with no video loaded (10.5's empty state): `tracker-app` with
    /// no CLI arg opens straight to the "Open a video to begin" prompt
    /// instead of refusing to start.
    pub fn empty() -> Self {
        Self {
            state: None,
            cache: None,
            texture: None,
            texture_frame: None,
            open_error: None,
            thumbnails: None,
            thumbnail_textures: Vec::new(),
            theme_override: theme::load_override(),
            filmstrip_override: None,
        }
    }

    /// Flips the effective theme and persists the new choice (task 12.4).
    /// Called from the toolbar's sun/moon button; applies immediately via
    /// `ctx.set_visuals` rather than waiting for the next frame's poll so
    /// the click feels instant.
    pub fn toggle_theme(&mut self, ctx: &egui::Context) {
        let new_dark = !ctx.style().visuals.dark_mode;
        palette::apply_chrome(ctx, new_dark);
        self.theme_override = Some(new_dark);
        theme::save_override(new_dark);
    }

    /// Rebuilds every video-dependent piece of state (`AppState`, the seek
    /// decoder/frame cache, the current texture) for a newly opened video.
    /// Used by both constructors above and by `open_video` (10.5) — opening
    /// a *second* video mid-session goes through exactly the same reset a
    /// fresh launch would, so there's no stale seed/calibration/tracking
    /// state left over from the previous video.
    fn load_video(&mut self, video_path: PathBuf, metadata: VideoMetadata) {
        let decoder = SeekingFrameDecoder::new(
            video_path.clone(),
            metadata.display_width(),
            metadata.display_height(),
            metadata.fps_num,
            metadata.fps_den,
        );
        let thumb_handle = thumbnail_worker::spawn_thumbnails(
            video_path.clone(),
            metadata.display_width(),
            metadata.display_height(),
            metadata.fps_num,
            metadata.fps_den,
            metadata.frame_count.unwrap_or(1),
        );
        self.thumbnail_textures = vec![None; thumb_handle.frame_indices.len()];
        self.thumbnails = Some(thumb_handle);
        let mut state = AppState::new(video_path, metadata);
        // Restore the persisted stop-set threshold (task 13.5), if the user
        // has ever changed it from `TrackingSettings::default`'s 20% —
        // mirrors `theme_override`'s load-once-at-startup pattern above,
        // kept out of `AppState::new` itself so state.rs stays free of
        // filesystem IO (its tests construct `AppState` directly, often
        // many times per test).
        if let Some(pct) = theme::load_stop_threshold() {
            state.settings.stop_threshold_pct = pct;
        }
        self.state = Some(state);
        self.cache = Some(FrameCache::new(decoder, 16));
        self.texture = None;
        self.texture_frame = None;
    }

    /// Drains any pending messages from the thumbnail-decode worker (10.6),
    /// uploading each arriving thumbnail as its own small texture as soon as
    /// it's ready — the strip shows placeholders for slots that haven't
    /// arrived yet rather than blocking on the whole batch. Returns `true`
    /// if at least one message was processed (caller should request a
    /// repaint), mirroring `AppState::poll_tracking`/`poll_export`'s shape.
    fn poll_thumbnails(&mut self, ctx: &egui::Context) -> bool {
        let Some(handle) = &self.thumbnails else {
            return false;
        };
        let mut any = false;
        while let Ok(msg) = handle.messages.try_recv() {
            any = true;
            match msg {
                ThumbnailMessage::Thumb {
                    slot,
                    width,
                    height,
                    rgb,
                    ..
                } => {
                    let image = egui::ColorImage::from_rgb([width as usize, height as usize], &rgb);
                    let name = format!("thumb-{slot}");
                    let tex = ctx.load_texture(name, image, egui::TextureOptions::NEAREST);
                    if let Some(slot_ref) = self.thumbnail_textures.get_mut(slot) {
                        *slot_ref = Some(tex);
                    }
                }
                ThumbnailMessage::Done => {}
            }
        }
        any
    }

    /// Opens the "Open video…" native file dialog (`rfd`), filtered to
    /// common video extensions, and loads whatever the user picks. No-op if
    /// the dialog is cancelled. Errors (e.g. `ffprobe` failing on the
    /// chosen file) are surfaced via `open_error`/an `AppState` event
    /// rather than left to crash the app — it stays exactly as usable as it
    /// was before the attempt.
    pub fn prompt_open_video(&mut self) {
        tracing::info!("file dialog opened");
        let Some(path) = rfd::FileDialog::new()
            .set_title("Open video")
            .add_filter("Video", VIDEO_EXTENSIONS)
            .pick_file()
        else {
            tracing::info!("file dialog cancelled");
            return; // dialog cancelled
        };
        tracing::info!(path = %path.display(), "file dialog picked path");
        self.open_video(path);
    }

    /// Probes `video_path` and, on success, loads it (replacing any
    /// previously loaded video and its session state). On failure, leaves
    /// the current state untouched and records the error for display.
    fn open_video(&mut self, video_path: PathBuf) {
        match crate::ffprobe::probe(&video_path) {
            Ok(metadata) => {
                tracing::info!(video = %video_path.display(), "opening video");
                let metadata_for_event = metadata;
                self.load_video(video_path.clone(), metadata);
                self.open_error = None;
                if let Some(state) = &mut self.state {
                    state.note_video_opened(&video_path, &metadata_for_event);
                }
            }
            Err(e) => {
                let message = format!("failed to open {}: {e}", video_path.display());
                tracing::error!(video = %video_path.display(), error = %e, "failed to probe video");
                if let Some(state) = &mut self.state {
                    state.status = message.clone();
                    state.note_error(message.clone());
                }
                self.open_error = Some(message);
            }
        }
    }

    fn ensure_texture(&mut self, ctx: &egui::Context) {
        let Some(state) = &mut self.state else {
            return;
        };
        let Some(cache) = &mut self.cache else {
            return;
        };
        if self.texture_frame == Some(state.current_frame) {
            return; // already showing the right frame
        }
        match cache.get(state.current_frame) {
            Ok(frame) => {
                let size = [frame.width() as usize, frame.height() as usize];
                let image = egui::ColorImage::from_rgb(size, frame.rgb());
                let handle = ctx.load_texture("current-frame", image, egui::TextureOptions::LINEAR);
                self.texture = Some(handle);
                self.texture_frame = Some(state.current_frame);
                state.status.clear();
            }
            Err(e) => {
                tracing::error!(frame = state.current_frame, error = %e, "failed to decode frame");
                state.status = format!("failed to decode frame {}: {e}", state.current_frame);
            }
        }
    }
}

impl eframe::App for TrackerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Keep the chrome visuals installed (task 13.7, superseding 12.4's
        // stock `Visuals::dark()/light()` re-apply). The effective theme is
        // the persisted user override when there is one, else whatever
        // egui/winit's system-theme-follow currently says
        // (`visuals.dark_mode`) — so with no override, a system
        // `ThemeChanged` flips `dark_mode`, `panel_fill` no longer matches
        // our palette, and this reinstalls the chrome for the new theme
        // instead of fighting the follow logic. The `panel_fill` check also
        // covers the very first frame (stock visuals) and any future stock
        // re-application; when nothing drifted this is a cheap no-op
        // comparison, never a per-frame `set_visuals`.
        {
            let visuals = ctx.style().visuals.clone();
            let dark = self.theme_override.unwrap_or(visuals.dark_mode);
            if visuals.dark_mode != dark
                || visuals.panel_fill != palette::chrome_palette(dark).app_bg
            {
                palette::apply_chrome(ctx, dark);
            }
        }
        if let Some(state) = &mut self.state {
            if state.poll_tracking() {
                ctx.request_repaint();
            }
            if state.poll_export() {
                ctx.request_repaint();
            }
            if state.poll_benchmark() {
                ctx.request_repaint();
            }
            // While a run/export/benchmark is active, keep repainting so
            // progress keeps flowing even if nothing else prompts a redraw.
            if state.tracking.is_some() || state.export.is_some() || state.benchmark.is_some() {
                ctx.request_repaint();
            }
            // Rep clip loop (task 13.3): while a ▶'d clip is armed, step
            // the playhead one frame per UI frame and schedule the next
            // repaint one video-frame-duration out, so the loop cycles at
            // roughly video fps through the existing seek decoder
            // (`ensure_texture` below picks up the new `current_frame`).
            if state.advance_rep_clip() {
                let (num, den) = (state.metadata.fps_num.max(1), state.metadata.fps_den.max(1));
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                    den as f64 / num as f64,
                ));
            }
        }
        if self.poll_thumbnails(ctx) {
            ctx.request_repaint();
        }
        self.ensure_texture(ctx);
        handle_frame_step_shortcuts(ctx, self.state.as_mut());

        toolbar::show(ctx, self);
        banner::show(ctx, self.state.as_ref());
        bottom_bar::show_status_bar(ctx, self.state.as_ref());
        bottom_bar::show_scrub_bar(ctx, self.state.as_mut());
        // Thumbnail strip after the scrub bar so it stacks above it
        // (`TopBottomPanel::bottom` panels stack upward from the bottom in
        // call order); hidden entirely in the empty state (10.5) since
        // there's no video/thumbnails to show.
        thumbnail_panel::show(ctx, self);
        // Side panel before the central panel so it claims its space first
        // (egui lays out panels in call order); the video then fills
        // whatever's left instead of an empty area to its right.
        side_panel::show(ctx, self.state.as_mut());
        video_panel::show(self, ctx);
    }
}

/// ←/→ = ±1 frame, Shift+←/→ = ±10 (task 10.6). Guarded by
/// `wants_keyboard_input` so this doesn't steal arrow-key input from a
/// focused text field — e.g. the calibration "known length" `DragValue`,
/// which egui lets left/right arrows nudge while it's focused. No-op with
/// no video loaded.
fn handle_frame_step_shortcuts(ctx: &egui::Context, state: Option<&mut AppState>) {
    let Some(state) = state else {
        return;
    };
    if ctx.wants_keyboard_input() {
        return;
    }
    let (left, right, shift) = ctx.input(|i| {
        (
            i.key_pressed(egui::Key::ArrowLeft),
            i.key_pressed(egui::Key::ArrowRight),
            i.modifiers.shift,
        )
    });
    let step: i64 = if shift { 10 } else { 1 };
    if left {
        state.set_frame(state.current_frame as i64 - step);
    }
    if right {
        state.set_frame(state.current_frame as i64 + step);
    }
}

/// Runs the app: creates the native window and hands control to eframe's
/// event loop. Not called from unit tests (no display in CI); `main.rs`
/// invokes this after CLI parsing (and, when a video path was given,
/// `ffprobe`) succeed. `video` is `None` for the no-arg empty-state launch
/// (10.5).
pub fn run(video: Option<(PathBuf, VideoMetadata)>) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id("image-tracker")
            .with_icon(std::sync::Arc::new(app_icon())),
        ..Default::default()
    };
    eframe::run_native(
        "Image Tracker",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(match video {
                Some((path, metadata)) => TrackerApp::new(path, metadata),
                None => TrackerApp::empty(),
            }))
        }),
    )
}

/// Decodes the embedded app icon PNG (10.5) for the window/taskbar. Decode
/// failure (shouldn't happen — the PNG is generated at build/asset time and
/// checked in) falls back to a blank 1x1 icon rather than panicking.
fn app_icon() -> egui::IconData {
    const ICON_BYTES: &[u8] = include_bytes!("../../../../assets/icons/image-tracker.png");
    match image::load_from_memory(ICON_BYTES) {
        Ok(img) => {
            let img = img.into_rgba8();
            let (width, height) = img.dimensions();
            egui::IconData {
                rgba: img.into_raw(),
                width,
                height,
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to decode embedded app icon");
            egui::IconData {
                rgba: vec![0, 0, 0, 0],
                width: 1,
                height: 1,
            }
        }
    }
}
