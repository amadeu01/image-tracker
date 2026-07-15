//! egui app shell (task 2.3): open a video, show a frame, scrub through it.
//!
//! `AppState` separates pure(ish) state (current frame index, mode, status
//! message) from the egui `App` impl, which is intentionally thin — egui
//! rendering itself isn't unit-tested, but the state transitions it drives
//! (frame index clamping) are, via `frame_cache::clamp_frame_index`.
//!
//! `Mode` grows a variant per interactive task: `PlacingSeed` (2.4) and
//! `Calibrating` (2.5), matched on `state.mode`.

use std::path::PathBuf;

use eframe::egui;

use crate::ffprobe::VideoMetadata;
use crate::frame_cache::{clamp_frame_index, FrameCache};
use crate::screen_map::screen_to_image_px;
use crate::seek_source::SeekingFrameDecoder;
use crate::tracking::{self, TrackingHandle, TrackingRunState};

/// What clicking on the frame view currently does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    /// Just look around: scrub the slider, no click handling yet.
    ViewOnly,
    /// Clicking the frame places the Seed (task 2.4).
    PlacingSeed,
    /// Clicking the frame places calibration points (task 2.5). Holds the
    /// first click (if any) and the known real-world length in meters used
    /// to derive px/m once both points are placed.
    Calibrating {
        first_point: Option<tracker_core::Point>,
        known_length_meters: f64,
    },
}

/// Default known length for the calibration reference object: a standard
/// 450mm bumper plate diameter.
pub const DEFAULT_CALIBRATION_LENGTH_METERS: f64 = 0.450;

/// A user-placed Seed: image-pixel position plus the frame it was placed on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seed {
    pub position: tracker_core::Point,
    pub frame_index: u64,
}

/// UI/session state, independent of egui so the index-clamping logic can be
/// unit-tested without a `Context`.
pub struct AppState {
    pub video_path: PathBuf,
    pub metadata: VideoMetadata,
    pub mode: Mode,
    pub current_frame: u64,
    /// The Seed, once placed (task 2.4). `None` until the user clicks in
    /// `Mode::PlacingSeed`.
    pub seed: Option<Seed>,
    /// The resolved Calibration (task 2.5), once both points have been
    /// clicked and the segment accepted. `None` until then; overwritten each
    /// time a new pair is completed.
    pub calibration: Option<tracker_core::Calibration>,
    /// The two points of the most recently completed calibration pair
    /// (success or failure), so the UI can draw a line between them even
    /// after the pair has reset for a potential third click.
    pub last_calibration_segment: Option<(tracker_core::Point, tracker_core::Point)>,
    /// Bottom status bar text; errors surface here rather than panicking
    /// (project rule — see PLAN.md 2.6).
    pub status: String,
    /// The active/paused tracking worker's channel handle, once "Track" has
    /// been clicked (task 2.6). `None` before a run starts and again once
    /// it finishes/errors.
    pub tracking: Option<TrackingHandle>,
    /// Pure reducer over that worker's progress messages; drives the live
    /// crosshair and status bar while `tracking` is active, and still holds
    /// the last-known state (including any error) after it finishes.
    pub tracking_run: TrackingRunState,
    /// The completed `BarPath`, once a tracking run reaches clean
    /// end-of-video. Consumed by milestone 3 (overlay render / export).
    pub bar_path: Option<tracker_core::BarPath>,
}

impl AppState {
    pub fn new(video_path: PathBuf, metadata: VideoMetadata) -> Self {
        Self {
            video_path,
            metadata,
            mode: Mode::ViewOnly,
            current_frame: 0,
            seed: None,
            calibration: None,
            last_calibration_segment: None,
            status: String::new(),
            tracking: None,
            tracking_run: TrackingRunState::default(),
            bar_path: None,
        }
    }

    /// Toggle between `ViewOnly` and `PlacingSeed`.
    pub fn toggle_placing_seed(&mut self) {
        self.mode = match self.mode {
            Mode::PlacingSeed => Mode::ViewOnly,
            _ => Mode::PlacingSeed,
        };
    }

    /// Toggle between `ViewOnly` and `Calibrating`. Entering `Calibrating`
    /// starts a fresh pair (no first point yet) and defaults the known
    /// length to `DEFAULT_CALIBRATION_LENGTH_METERS` (450mm plate).
    pub fn toggle_calibrating(&mut self) {
        self.mode = match self.mode {
            Mode::Calibrating { .. } => Mode::ViewOnly,
            _ => Mode::Calibrating {
                first_point: None,
                known_length_meters: DEFAULT_CALIBRATION_LENGTH_METERS,
            },
        };
    }

    /// Update the known real-world length (in meters) used for the current
    /// calibration pair. No-op outside `Mode::Calibrating`.
    pub fn set_calibration_length(&mut self, meters: f64) {
        if let Mode::Calibrating {
            known_length_meters,
            ..
        } = &mut self.mode
        {
            *known_length_meters = meters;
        }
    }

    /// Record a Seed at the given image-pixel position on the current frame.
    /// Only takes effect in `Mode::PlacingSeed`.
    pub fn place_seed(&mut self, position: tracker_core::Point) {
        if self.mode != Mode::PlacingSeed {
            return;
        }
        self.seed = Some(Seed {
            position,
            frame_index: self.current_frame,
        });
    }

    /// Record a calibration click at the given image-pixel position. Only
    /// takes effect in `Mode::Calibrating`.
    ///
    /// The first click of a pair is just remembered. The second click
    /// attempts to build a `Calibration` from the two points and the
    /// currently configured known length; success or failure, the pair then
    /// resets so a third click starts a brand-new pair.
    pub fn place_calibration_point(&mut self, position: tracker_core::Point) {
        let Mode::Calibrating {
            first_point,
            known_length_meters,
        } = self.mode
        else {
            return;
        };

        match first_point {
            None => {
                self.mode = Mode::Calibrating {
                    first_point: Some(position),
                    known_length_meters,
                };
            }
            Some(first) => {
                self.last_calibration_segment = Some((first, position));
                match tracker_core::Calibration::new(first, position, known_length_meters) {
                    Ok(cal) => {
                        self.calibration = Some(cal);
                        self.status.clear();
                    }
                    Err(e) => {
                        self.status = format!("calibration failed: {e}");
                    }
                }
                // Third click restarts the pair, regardless of outcome.
                self.mode = Mode::Calibrating {
                    first_point: None,
                    known_length_meters,
                };
            }
        }
    }

    /// Status-bar text reflecting mode and, once placed, the Seed and
    /// Calibration state.
    pub fn status_line(&self) -> String {
        let mode = match self.mode {
            Mode::ViewOnly => "view".to_string(),
            Mode::PlacingSeed => "placing seed (click frame)".to_string(),
            Mode::Calibrating { first_point, .. } => match first_point {
                Some(_) => "calibrating (click 2nd point)".to_string(),
                None => "calibrating (click 1st point)".to_string(),
            },
        };
        let seed_part = match &self.seed {
            Some(seed) => format!(
                "seed: ({:.1}, {:.1}) @ frame {}",
                seed.position.x, seed.position.y, seed.frame_index
            ),
            None => "seed: none".to_string(),
        };
        let calibration_part = match &self.calibration {
            Some(cal) => format!("calibration: {:.1} px/m", cal.px_per_meter()),
            None => "calibration: none".to_string(),
        };
        format!("mode: {mode}  |  {seed_part}  |  {calibration_part}")
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

    /// Whether the "Track" action should currently be available: a Seed
    /// must be placed, and no run already active.
    pub fn can_start_tracking(&self) -> bool {
        self.seed.is_some() && self.tracking.is_none()
    }

    /// Spawns a background tracking run from the current Seed, using this
    /// module's default `TemplateTracker`/`TrackingSession` tuning. No-op if
    /// `can_start_tracking` is false.
    pub fn start_tracking(&mut self) {
        if !self.can_start_tracking() {
            return;
        }
        let Some(seed) = self.seed else { return };
        let handle = tracking::spawn_tracking(
            self.video_path.clone(),
            self.metadata.display_width(),
            self.metadata.display_height(),
            self.metadata.fps_num,
            self.metadata.fps_den,
            seed.frame_index,
            seed.position,
            tracking::default_tracker_config(),
            tracking::default_session_config(),
        );
        self.tracking = Some(handle);
        self.tracking_run = TrackingRunState::started();
        self.bar_path = None;
    }

    /// Drains any pending messages from the active tracking worker,
    /// applying each to `tracking_run` and advancing the display frame to
    /// follow the latest tracked/interpolated position. Returns `true` if
    /// at least one message was processed (the caller should request a
    /// repaint). Once the run finishes (or errors), stores the completed
    /// `BarPath` (if any) and drops the worker handle.
    pub fn poll_tracking(&mut self) -> bool {
        let Some(handle) = &self.tracking else {
            return false;
        };
        let mut any = false;
        let mut finished = false;
        let mut messages = Vec::new();
        while let Ok(msg) = handle.messages.try_recv() {
            messages.push(msg);
        }
        for msg in messages {
            any = true;
            if let Some(frame_index) = msg.video_frame_index() {
                self.set_frame(frame_index as i64);
            }
            if self.tracking_run.apply(msg) {
                finished = true;
            }
        }
        if finished {
            self.bar_path = self.tracking_run.bar_path.clone();
            self.tracking = None;
        }
        any
    }

    /// Sends a reseed command to a paused tracking worker, using the
    /// current Seed (which must already be placed on the frame the run
    /// paused at — the UI only enables the Resume action once that's
    /// true). No-op if there's no active worker or no Seed.
    pub fn resume_tracking(&mut self) {
        let (Some(handle), Some(seed)) = (&self.tracking, self.seed) else {
            return;
        };
        handle.resume(seed.frame_index, seed.position);
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
        if self.state.poll_tracking() {
            ctx.request_repaint();
        }
        // While a run is active, keep repainting so progress keeps flowing
        // even if nothing else prompts a redraw.
        if self.state.tracking.is_some() {
            ctx.request_repaint();
        }
        self.ensure_texture(ctx);

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let label = match self.state.mode {
                    Mode::ViewOnly => "Place Seed",
                    Mode::PlacingSeed => "Placing Seed... (click frame)",
                    Mode::Calibrating { .. } => "Place Seed",
                };
                if ui.selectable_label(self.state.mode == Mode::PlacingSeed, label).clicked() {
                    self.state.toggle_placing_seed();
                }
                // Key toggle, e.g. 's' for seed placement.
                if ui.ctx().input(|i| i.key_pressed(egui::Key::S)) {
                    self.state.toggle_placing_seed();
                }

                ui.separator();

                let calibrating = matches!(self.state.mode, Mode::Calibrating { .. });
                let cal_label = if calibrating {
                    "Calibrating... (click 2 points)"
                } else {
                    "Calibrate"
                };
                if ui.selectable_label(calibrating, cal_label).clicked() {
                    self.state.toggle_calibrating();
                }
                // Key toggle, 'c' for calibration.
                if ui.ctx().input(|i| i.key_pressed(egui::Key::C)) {
                    self.state.toggle_calibrating();
                }

                if let Mode::Calibrating {
                    known_length_meters, ..
                } = self.state.mode
                {
                    ui.label("known length (m):");
                    let mut meters = known_length_meters;
                    if ui
                        .add(egui::DragValue::new(&mut meters).speed(0.001).range(0.001..=10.0))
                        .changed()
                    {
                        self.state.set_calibration_length(meters);
                    }
                }

                ui.separator();

                let paused = self.state.tracking_run.session_state
                    == Some(tracker_core::SessionState::NeedsReseed);
                if paused {
                    // Nudge the user straight into placing a new seed on the
                    // paused frame.
                    if self.state.mode != Mode::PlacingSeed {
                        self.state.mode = Mode::PlacingSeed;
                    }
                    let ready = self
                        .state
                        .seed
                        .map(|s| Some(s.frame_index) == self.state.tracking_run.last_frame_index)
                        .unwrap_or(false);
                    ui.colored_label(egui::Color32::YELLOW, "tracking paused: click a new seed");
                    if ui
                        .add_enabled(ready, egui::Button::new("Resume"))
                        .clicked()
                    {
                        self.state.resume_tracking();
                    }
                } else if ui
                    .add_enabled(
                        self.state.can_start_tracking(),
                        egui::Button::new("Track"),
                    )
                    .clicked()
                {
                    self.state.start_tracking();
                }
            });
        });

        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "{}  |  frame {}/{}  |  {}",
                    self.state.video_path.display(),
                    self.state.current_frame,
                    self.state.metadata.frame_count.unwrap_or(0).saturating_sub(1),
                    self.state.status_line(),
                ));
                let tracking_active = self.state.tracking.is_some()
                    || self.state.tracking_run.error.is_some()
                    || self.state.bar_path.is_some();
                if tracking_active {
                    ui.separator();
                    let is_error = self.state.tracking_run.error.is_some();
                    let is_paused = self.state.tracking_run.session_state
                        == Some(tracker_core::SessionState::NeedsReseed);
                    let color = if is_error {
                        egui::Color32::RED
                    } else if is_paused {
                        egui::Color32::YELLOW
                    } else {
                        egui::Color32::LIGHT_GREEN
                    };
                    ui.colored_label(color, self.state.tracking_run.status_line());
                }
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
                let response = ui.add(
                    egui::Image::new((texture.id(), tex_size * scale))
                        .sense(egui::Sense::click()),
                );
                let image_rect = response.rect;

                let calibrating = matches!(self.state.mode, Mode::Calibrating { .. });

                if response.clicked() {
                    if let Some(click_pos) = response.interact_pointer_pos() {
                        if let Some(image_px) = screen_to_image_px(
                            click_pos,
                            image_rect,
                            self.state.metadata.display_width(),
                            self.state.metadata.display_height(),
                        ) {
                            if self.state.mode == Mode::PlacingSeed {
                                self.state.place_seed(image_px);
                            } else if calibrating {
                                self.state.place_calibration_point(image_px);
                            }
                        }
                    }
                }

                if let Some(seed) = self.state.seed {
                    if seed.frame_index == self.state.current_frame {
                        draw_crosshair(
                            ui.painter(),
                            image_rect,
                            tex_size,
                            seed.position,
                            egui::Color32::from_rgb(255, 60, 60),
                        );
                    }
                }

                // Live tracking crosshair: the latest tracked/interpolated
                // position, shown only while the display frame has caught
                // up to it (the display is driven to follow progress in
                // `poll_tracking`, so in practice this is almost always
                // true once a run is active).
                if let (Some(idx), Some(pos)) = (
                    self.state.tracking_run.last_frame_index,
                    self.state.tracking_run.last_position,
                ) {
                    if idx == self.state.current_frame {
                        draw_crosshair(
                            ui.painter(),
                            image_rect,
                            tex_size,
                            pos,
                            egui::Color32::from_rgb(60, 255, 120),
                        );
                    }
                }

                if let Mode::Calibrating {
                    first_point: Some(first),
                    ..
                } = self.state.mode
                {
                    draw_calibration_pending_point(ui.painter(), image_rect, tex_size, first);
                }

                if let Some((a, b)) = self.state.last_calibration_segment {
                    draw_calibration_segment(ui.painter(), image_rect, tex_size, a, b);
                }
            } else {
                ui.label("decoding first frame...");
            }
        });
    }
}

/// Draw a crosshair marker at an image-pixel position (the Seed, red; the
/// live tracking position, green — see call sites), converting back to
/// screen coordinates for the currently drawn (scaled, letterboxed) image
/// rect. Painter overlay only — never mutates frame pixels.
fn draw_crosshair(
    painter: &egui::Painter,
    image_rect: egui::Rect,
    image_native_size: egui::Vec2,
    px: tracker_core::Point,
    color: egui::Color32,
) {
    if image_native_size.x <= 0.0 || image_native_size.y <= 0.0 {
        return;
    }
    let scale_x = image_rect.width() / image_native_size.x;
    let scale_y = image_rect.height() / image_native_size.y;
    let screen =
        image_rect.min + egui::Vec2::new(px.x as f32 * scale_x, px.y as f32 * scale_y);

    let radius = 8.0;
    let stroke = egui::Stroke::new(2.0, color);
    painter.line_segment(
        [
            egui::pos2(screen.x - radius, screen.y),
            egui::pos2(screen.x + radius, screen.y),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(screen.x, screen.y - radius),
            egui::pos2(screen.x, screen.y + radius),
        ],
        stroke,
    );
    painter.circle_stroke(screen, radius * 0.6, stroke);
}

/// Convert an image-pixel point to a screen point within the currently
/// drawn (scaled, letterboxed) image rect. Shared by the seed crosshair and
/// calibration overlays.
fn image_px_to_screen(
    image_rect: egui::Rect,
    image_native_size: egui::Vec2,
    px: tracker_core::Point,
) -> Option<egui::Pos2> {
    if image_native_size.x <= 0.0 || image_native_size.y <= 0.0 {
        return None;
    }
    let scale_x = image_rect.width() / image_native_size.x;
    let scale_y = image_rect.height() / image_native_size.y;
    Some(
        image_rect.min
            + egui::Vec2::new(px.x as f32 * scale_x, px.y as f32 * scale_y),
    )
}

/// Draw a marker at the first-clicked calibration point, while awaiting the
/// second click.
fn draw_calibration_pending_point(
    painter: &egui::Painter,
    image_rect: egui::Rect,
    image_native_size: egui::Vec2,
    point_px: tracker_core::Point,
) {
    if let Some(screen) = image_px_to_screen(image_rect, image_native_size, point_px) {
        let color = egui::Color32::from_rgb(60, 160, 255);
        painter.circle_filled(screen, 4.0, color);
    }
}

/// Draw a line between the two most recently clicked calibration points,
/// with endpoint markers. Painter overlay only — never mutates frame pixels.
fn draw_calibration_segment(
    painter: &egui::Painter,
    image_rect: egui::Rect,
    image_native_size: egui::Vec2,
    a_px: tracker_core::Point,
    b_px: tracker_core::Point,
) {
    let (Some(a), Some(b)) = (
        image_px_to_screen(image_rect, image_native_size, a_px),
        image_px_to_screen(image_rect, image_native_size, b_px),
    ) else {
        return;
    };
    let color = egui::Color32::from_rgb(60, 160, 255);
    let stroke = egui::Stroke::new(2.0, color);
    painter.line_segment([a, b], stroke);
    painter.circle_filled(a, 4.0, color);
    painter.circle_filled(b, 4.0, color);
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
            rotation: None,
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

    #[test]
    fn toggle_placing_seed_switches_modes() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        assert_eq!(state.mode, Mode::ViewOnly);
        state.toggle_placing_seed();
        assert_eq!(state.mode, Mode::PlacingSeed);
        state.toggle_placing_seed();
        assert_eq!(state.mode, Mode::ViewOnly);
    }

    #[test]
    fn place_seed_is_ignored_outside_placing_mode() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.place_seed(tracker_core::Point::new(1.0, 2.0));
        assert!(state.seed.is_none());
    }

    #[test]
    fn place_seed_records_position_and_current_frame() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.toggle_placing_seed();
        state.set_frame(3);
        state.place_seed(tracker_core::Point::new(12.5, 7.0));
        let seed = state.seed.expect("seed should be set");
        assert_eq!(seed.frame_index, 3);
        assert_eq!(seed.position, tracker_core::Point::new(12.5, 7.0));
    }

    #[test]
    fn toggle_calibrating_switches_modes_and_seeds_default_length() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.toggle_calibrating();
        match state.mode {
            Mode::Calibrating {
                first_point,
                known_length_meters,
            } => {
                assert_eq!(first_point, None);
                assert_eq!(known_length_meters, DEFAULT_CALIBRATION_LENGTH_METERS);
            }
            _ => panic!("expected Calibrating mode"),
        }
        state.toggle_calibrating();
        assert_eq!(state.mode, Mode::ViewOnly);
    }

    #[test]
    fn two_clicks_resolve_calibration_with_default_length() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.toggle_calibrating();
        state.place_calibration_point(tracker_core::Point::new(0.0, 0.0));
        assert!(state.calibration.is_none());
        state.place_calibration_point(tracker_core::Point::new(200.0, 0.0));
        let cal = state.calibration.expect("calibration should resolve");
        assert!((cal.px_per_meter() - (200.0 / DEFAULT_CALIBRATION_LENGTH_METERS)).abs() < 1e-6);
    }

    #[test]
    fn third_click_restarts_the_pair() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.toggle_calibrating();
        state.place_calibration_point(tracker_core::Point::new(0.0, 0.0));
        state.place_calibration_point(tracker_core::Point::new(200.0, 0.0));
        // Third click: starts a fresh pair rather than being treated as a
        // second point of the old one.
        state.place_calibration_point(tracker_core::Point::new(50.0, 50.0));
        match state.mode {
            Mode::Calibrating { first_point, .. } => {
                assert_eq!(first_point, Some(tracker_core::Point::new(50.0, 50.0)));
            }
            _ => panic!("expected Calibrating mode"),
        }
    }

    #[test]
    fn coincident_calibration_points_surface_error_and_restart_pair() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.toggle_calibrating();
        state.place_calibration_point(tracker_core::Point::new(10.0, 10.0));
        state.place_calibration_point(tracker_core::Point::new(10.0, 10.0));
        assert!(state.calibration.is_none());
        assert!(state.status.contains("calibration failed"));
        match state.mode {
            Mode::Calibrating { first_point, .. } => assert_eq!(first_point, None),
            _ => panic!("expected Calibrating mode"),
        }
    }

    #[test]
    fn calibration_clicks_are_ignored_outside_calibrating_mode() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.place_calibration_point(tracker_core::Point::new(0.0, 0.0));
        assert_eq!(state.mode, Mode::ViewOnly);
        assert!(state.calibration.is_none());
    }

    #[test]
    fn set_calibration_length_updates_pending_pair() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.toggle_calibrating();
        state.set_calibration_length(1.0);
        state.place_calibration_point(tracker_core::Point::new(0.0, 0.0));
        state.place_calibration_point(tracker_core::Point::new(100.0, 0.0));
        let cal = state.calibration.expect("calibration should resolve");
        assert!((cal.px_per_meter() - 100.0).abs() < 1e-6);
    }

    #[test]
    fn status_line_reports_calibration_px_per_meter() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        state.toggle_calibrating();
        state.place_calibration_point(tracker_core::Point::new(0.0, 0.0));
        state.place_calibration_point(tracker_core::Point::new(200.0, 0.0));
        let line = state.status_line();
        assert!(line.contains("px/m"));
    }

    #[test]
    fn status_line_reflects_mode_and_seed() {
        let mut state = AppState::new(PathBuf::from("x.mp4"), meta(Some(10)));
        assert!(state.status_line().contains("view"));
        assert!(state.status_line().contains("seed: none"));

        state.toggle_placing_seed();
        state.place_seed(tracker_core::Point::new(4.0, 5.0));
        let line = state.status_line();
        assert!(line.contains("placing seed"));
        assert!(line.contains("4.0"));
        assert!(line.contains("5.0"));
    }
}
