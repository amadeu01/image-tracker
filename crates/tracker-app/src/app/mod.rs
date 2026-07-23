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
mod results;
mod settings_section;
mod side_panel;
mod state;
mod theme;
mod thumbnail_panel;
mod toolbar;
mod video_panel;

pub use state::{AppState, DisplayMode, Mode, Phase, Seed, DEFAULT_CALIBRATION_LENGTH_METERS};

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

use eframe::egui;

use crate::decode_worker::{self, DecodeHandle, DecodeMessage};
use crate::ffprobe::VideoMetadata;
use crate::seek_source::SeekingFrameDecoder;
use crate::thumbnail_worker::{self, ThumbnailHandle, ThumbnailMessage};

/// How many decoded frames `TrackerApp::frames` keeps on the UI side (task
/// 18.1). This is a *second*, much smaller cache than the decode worker's
/// own `FrameCache` — the worker's cache exists so it doesn't re-spawn
/// ffmpeg for a frame it already decoded; this one exists so the UI thread
/// can read a `tracker_core::Frame` it already has (to draw the texture, or
/// to run `suggest_tracker` for a seed placed on the currently-visible
/// frame — see `video_panel.rs`'s seed-placement handler) without ever
/// blocking on the worker. Small: unlike the worker's cache this is never
/// used to avoid a re-decode, just to answer "do I already have this frame
/// in hand".
const UI_FRAME_CACHE_CAPACITY: usize = 8;

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
    /// The async frame-decode worker (task 18.1): the UI sends "want frame
    /// N" and drains replies with `try_recv` in `update`/`poll_decode`.
    /// Never call a decoder synchronously from here again — see
    /// `docs/gui-threading.md` finding G1, which this replaces.
    decode: Option<DecodeHandle<tracker_core::Frame>>,
    /// Small UI-side cache of recently-decoded frames, populated only from
    /// `DecodeMessage::Decoded` replies (see `UI_FRAME_CACHE_CAPACITY`).
    /// `frame_order` tracks insertion order for eviction (a plain FIFO is
    /// enough here — this is a "do I already have it" convenience cache,
    /// not a performance-critical LRU like the worker's own).
    frames: HashMap<u64, tracker_core::Frame>,
    frame_order: VecDeque<u64>,
    /// Set when a seed was placed on a frame the UI didn't already have in
    /// `frames` (task 18.1's fix for finding G2): `(frame_index, position)`
    /// the seed was placed at. The frame is requested from the worker and
    /// `note_seed_suggestion` is applied once it arrives in `poll_decode`,
    /// instead of blocking the click handler on a synchronous decode.
    pending_seed_suggestion: Option<(u64, tracker_core::Point)>,
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
            decode: None,
            frames: HashMap::new(),
            frame_order: VecDeque::new(),
            pending_seed_suggestion: None,
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
        // Restore the persisted bar-path overlay visibility (task 15.2),
        // same pattern as the stop threshold above.
        if let Some(show) = theme::load_show_path() {
            state.show_path = show;
        }
        // Restore the persisted "burn overlay into rep clips" choice (task
        // 19.3), same pattern as the two loads above.
        if let Some(burn) = theme::load_burn_overlay_in_rep_clips() {
            state.settings.burn_overlay_in_rep_clips = burn;
        }
        self.state = Some(state);
        self.decode = Some(decode_worker::spawn_decode_worker(decoder, 16));
        self.frames.clear();
        self.frame_order.clear();
        self.pending_seed_suggestion = None;
        self.texture = None;
        self.texture_frame = None;
    }

    /// Records a newly-decoded frame in the small UI-side cache
    /// (`frames`/`frame_order`), evicting the oldest entry once
    /// `UI_FRAME_CACHE_CAPACITY` is exceeded.
    fn remember_frame(&mut self, frame_index: u64, frame: tracker_core::Frame) {
        if !self.frames.contains_key(&frame_index) {
            self.frame_order.push_back(frame_index);
            while self.frame_order.len() > UI_FRAME_CACHE_CAPACITY {
                if let Some(oldest) = self.frame_order.pop_front() {
                    self.frames.remove(&oldest);
                }
            }
        }
        self.frames.insert(frame_index, frame);
    }

    /// Drains the decode worker's reply channel (task 18.1), uploading a
    /// texture when the arriving frame is the one currently wanted
    /// (`state.current_frame`) and resolving a pending seed suggestion
    /// (the G2 fix — see `pending_seed_suggestion`'s doc comment) when its
    /// frame arrives. Returns `true` if anything was processed, so `update`
    /// knows to request a repaint, mirroring `poll_thumbnails`.
    fn poll_decode(&mut self, ctx: &egui::Context) -> bool {
        let Some(handle) = &self.decode else {
            return false;
        };
        // Drain into a `Vec` first: `remember_frame` etc below need `&mut
        // self`, which can't coexist with `handle.results` borrowing
        // `self.decode` immutably.
        let messages: Vec<_> = handle.results.try_iter().collect();
        let mut any = false;
        for msg in messages {
            any = true;
            match msg {
                DecodeMessage::Decoded { frame_index, frame } => {
                    self.remember_frame(frame_index, frame.clone());
                    if let Some(state) = &mut self.state {
                        if frame_index == state.current_frame {
                            let size = [frame.width() as usize, frame.height() as usize];
                            let image = egui::ColorImage::from_rgb(size, frame.rgb());
                            let handle = ctx.load_texture(
                                "current-frame",
                                image,
                                egui::TextureOptions::LINEAR,
                            );
                            self.texture = Some(handle);
                            self.texture_frame = Some(frame_index);
                            state.status.clear();
                        }
                        if let Some((pending_frame, position)) = self.pending_seed_suggestion {
                            if pending_frame == frame_index {
                                self.pending_seed_suggestion = None;
                                let kind = tracker_core::suggest_tracker(
                                    &frame,
                                    position,
                                    tracker_core::TrackerSuggestionConfig::default(),
                                );
                                state.note_seed_suggestion(kind);
                            }
                        }
                    }
                }
                DecodeMessage::Error {
                    frame_index,
                    message,
                } => {
                    tracing::error!(frame = frame_index, error = %message, "failed to decode frame");
                    if let Some(state) = &mut self.state {
                        if frame_index == state.current_frame {
                            state.status =
                                format!("failed to decode frame {frame_index}: {message}");
                        }
                    }
                    if self.pending_seed_suggestion.map(|(f, _)| f) == Some(frame_index) {
                        self.pending_seed_suggestion = None;
                    }
                }
            }
        }
        any
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

    /// Requests the currently-wanted frame from the decode worker if the
    /// displayed texture doesn't already match `state.current_frame` (task
    /// 18.1, replacing the synchronous `FrameCache::get` this used to call
    /// directly — see `docs/gui-threading.md` finding G1). Never decodes
    /// anything itself: if the UI-side `frames` cache already has the
    /// wanted frame (e.g. it was decoded for a previous purpose, like a
    /// seed suggestion), upload it immediately; otherwise ask the worker
    /// and let `poll_decode` pick up the reply on a later frame. Safe to
    /// call every `update` — a repeat `want()` for the same index the
    /// worker is already decoding is coalesced away for free.
    fn request_current_frame(&mut self, ctx: &egui::Context) {
        let Some(state) = &mut self.state else {
            return;
        };
        if self.texture_frame == Some(state.current_frame) {
            return; // already showing the right frame
        }
        if let Some(frame) = self.frames.get(&state.current_frame).cloned() {
            let size = [frame.width() as usize, frame.height() as usize];
            let image = egui::ColorImage::from_rgb(size, frame.rgb());
            let handle = ctx.load_texture("current-frame", image, egui::TextureOptions::LINEAR);
            self.texture = Some(handle);
            self.texture_frame = Some(state.current_frame);
            state.status.clear();
            return;
        }
        if let Some(decode) = &self.decode {
            decode.want(state.current_frame);
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
            if state.is_tracking() || state.is_exporting() || state.is_benchmarking() {
                ctx.request_repaint();
            }
            // Rep clip loop (task 13.3): while a ▶'d clip is armed, step
            // the playhead one frame per UI frame and schedule the next
            // repaint one video-frame-duration out, so the loop cycles at
            // roughly video fps through the async decode worker
            // (`request_current_frame`/`poll_decode` below pick up the new
            // `current_frame`). Task 19.2: `advance_rep_clip` only steps
            // once `self.texture_frame` already matches `current_frame`,
            // i.e. the frame it's about to leave has actually rendered —
            // otherwise the loop outruns the async decode and every
            // texture reply arrives for an already-stale `current_frame`,
            // which `poll_decode` then silently drops (see that method's
            // `frame_index == state.current_frame` check), leaving the
            // video frame frozen under a still-advancing path overlay.
            // While waiting on a still-in-flight decode, keep repainting
            // so the wait doesn't stall on some unrelated input event.
            if state.rep_clip.is_some() {
                if state.advance_rep_clip(self.texture_frame) {
                    let (num, den) = (state.metadata.fps_num.max(1), state.metadata.fps_den.max(1));
                    ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                        den as f64 / num as f64,
                    ));
                } else {
                    ctx.request_repaint();
                }
            }
        }
        if self.poll_thumbnails(ctx) {
            ctx.request_repaint();
        }
        if self.poll_decode(ctx) {
            ctx.request_repaint();
        }
        self.request_current_frame(ctx);
        // Keep repainting while the wanted frame hasn't arrived yet, so the
        // "decoding…" state (video_panel.rs) and the eventual texture both
        // show up promptly instead of waiting for some unrelated input to
        // trigger the next repaint.
        if let Some(state) = &self.state {
            if self.texture_frame != Some(state.current_frame) {
                ctx.request_repaint();
            }
        }
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
