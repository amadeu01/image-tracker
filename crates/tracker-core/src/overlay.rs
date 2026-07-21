//! Overlay renderer: draws the Bar Path (see CONTEXT.md's "Overlay Video"
//! term) onto a `Frame`'s pixel buffer directly.
//!
//! This is pure pixel math on domain types (`Frame`, `BarPath`), so it
//! lives in tracker-core rather than tracker-app: the same overlay must be
//! burned into frames for the MP4 export (3.2), where there is no egui
//! painter available, so pixel-level drawing is the common denominator
//! for both the live UI preview and the exported video. tracker-app can
//! still choose to draw a *live preview* overlay with egui's painter
//! (as 2.4/2.6 already do for the seed crosshair / live tracking dot);
//! this module is specifically for what gets baked into exported frames.
//!
//! Text rendering is intentionally skipped: legend entries are drawn as
//! plain color swatches only (no bitmap font), documented in
//! `OverlayStyle`'s doc comment. Callers that want text labels can
//! composite them on top with a real text-rendering adapter later.

use crate::bar_path::BarPath;
use crate::geometry::Frame;
use crate::rep::Rep;
use crate::session::Source;

/// RGB color, 8-bit per channel.
pub type Color = [u8; 3];

/// Style/config for `render_overlay`, built via `OverlayStyle::builder()`.
///
/// Legend rendering is swatches-only (no text): each toggled-on legend
/// entry draws as a small filled rectangle in its associated color. A
/// future task can composite text labels on top (e.g. via a bitmap font
/// or a separate text-rendering adapter) once one exists; wiring that in
/// here would be premature given no such adapter exists yet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlayStyle {
    path_color: Color,
    gap_color: Color,
    marker_color: Color,
    line_thickness: u32,
    marker_radius: u32,
    show_legend: bool,
    legend_swatch_size: u32,
    legend_margin: u32,
    legend_padding: u32,
}

impl OverlayStyle {
    /// Starts a builder with sensible defaults: green path, orange gap
    /// segments, red marker, 2px line thickness, legend on.
    pub fn builder() -> OverlayStyleBuilder {
        OverlayStyleBuilder::default()
    }

    pub fn path_color(&self) -> Color {
        self.path_color
    }

    pub fn gap_color(&self) -> Color {
        self.gap_color
    }

    pub fn marker_color(&self) -> Color {
        self.marker_color
    }

    pub fn line_thickness(&self) -> u32 {
        self.line_thickness
    }

    pub fn marker_radius(&self) -> u32 {
        self.marker_radius
    }

    pub fn show_legend(&self) -> bool {
        self.show_legend
    }
}

/// Builder for `OverlayStyle`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlayStyleBuilder {
    path_color: Color,
    gap_color: Color,
    marker_color: Color,
    line_thickness: u32,
    marker_radius: u32,
    show_legend: bool,
    legend_swatch_size: u32,
    legend_margin: u32,
    legend_padding: u32,
}

impl Default for OverlayStyleBuilder {
    fn default() -> Self {
        Self {
            path_color: [0, 200, 0],
            gap_color: [255, 140, 0],
            marker_color: [255, 0, 0],
            line_thickness: 2,
            marker_radius: 5,
            show_legend: true,
            legend_swatch_size: 10,
            legend_margin: 8,
            legend_padding: 4,
        }
    }
}

impl OverlayStyleBuilder {
    /// Color of polyline segments between two consecutive tracked points.
    pub fn path_color(mut self, color: Color) -> Self {
        self.path_color = color;
        self
    }

    /// Color of polyline segments that cross an interpolated/gap point.
    pub fn gap_color(mut self, color: Color) -> Self {
        self.gap_color = color;
        self
    }

    /// Color of the current-position marker.
    pub fn marker_color(mut self, color: Color) -> Self {
        self.marker_color = color;
        self
    }

    /// Line thickness in pixels (minimum enforced at 1).
    pub fn line_thickness(mut self, thickness: u32) -> Self {
        self.line_thickness = thickness.max(1);
        self
    }

    /// Marker radius in pixels.
    pub fn marker_radius(mut self, radius: u32) -> Self {
        self.marker_radius = radius;
        self
    }

    /// Toggles the legend box on/off.
    pub fn show_legend(mut self, show: bool) -> Self {
        self.show_legend = show;
        self
    }

    pub fn build(self) -> OverlayStyle {
        OverlayStyle {
            path_color: self.path_color,
            gap_color: self.gap_color,
            marker_color: self.marker_color,
            line_thickness: self.line_thickness,
            marker_radius: self.marker_radius,
            show_legend: self.show_legend,
            legend_swatch_size: self.legend_swatch_size,
            legend_margin: self.legend_margin,
            legend_padding: self.legend_padding,
        }
    }
}

/// Draws a line segment from `(x0, y0)` to `(x1, y1)` with the given
/// `thickness` (in pixels) and `color`, clipped to `frame`'s bounds.
/// Uses Bresenham's algorithm for the centerline and stamps a filled
/// square of side `thickness` at each step to fake width — simple, robust
/// to any slope, and good enough at the thicknesses this overlay uses.
fn draw_line(frame: &mut Frame, x0: i64, y0: i64, x1: i64, y1: i64, thickness: u32, color: Color) {
    let half = (thickness as i64 - 1) / 2;
    let extra = thickness as i64 - 1 - half;

    let mut plot = |cx: i64, cy: i64| {
        for dy in -half..=extra {
            for dx in -half..=extra {
                frame.set_pixel(cx + dx, cy + dy, color);
            }
        }
    };

    let (mut x, mut y) = (x0, y0);
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;

    loop {
        plot(x, y);
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

/// Draws a filled-square crosshair-ish marker centered at `(cx, cy)` with
/// half-width `radius`, clipped to `frame`'s bounds.
fn draw_marker(frame: &mut Frame, cx: i64, cy: i64, radius: u32, color: Color) {
    let r = radius as i64;
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy <= r * r {
                frame.set_pixel(cx + dx, cy + dy, color);
            }
        }
    }
}

fn draw_filled_rect(frame: &mut Frame, x: i64, y: i64, w: i64, h: i64, color: Color) {
    for dy in 0..h {
        for dx in 0..w {
            frame.set_pixel(x + dx, y + dy, color);
        }
    }
}

/// Renders the Bar Path onto `frame` up to and including
/// `current_frame_index`: a polyline through path points (green for
/// tracked-to-tracked segments, orange wherever a segment touches an
/// interpolated/gap point), a marker at the point recorded at
/// `current_frame_index` (if any), and — if `style.show_legend()` — a
/// small legend box of color swatches in the top-left corner.
///
/// Never panics: points computed off-frame (e.g. the bar tracked outside
/// the visible image, or extrapolated there) are simply clipped by
/// `Frame::set_pixel`'s bounds check.
pub fn render_overlay(
    frame: &mut Frame,
    path: &BarPath,
    current_frame_index: u64,
    style: &OverlayStyle,
) {
    let points: Vec<_> = path
        .points()
        .iter()
        .filter(|p| p.frame_index <= current_frame_index)
        .collect();

    for pair in points.windows(2) {
        let a = pair[0];
        let b = pair[1];
        let color = if a.source == Source::Interpolated || b.source == Source::Interpolated {
            style.gap_color
        } else {
            style.path_color
        };
        draw_line(
            frame,
            a.position.x.round() as i64,
            a.position.y.round() as i64,
            b.position.x.round() as i64,
            b.position.y.round() as i64,
            style.line_thickness,
            color,
        );
    }

    if let Some(current) = points.last() {
        draw_marker(
            frame,
            current.position.x.round() as i64,
            current.position.y.round() as i64,
            style.marker_radius,
            style.marker_color,
        );
    }

    if style.show_legend {
        draw_legend(frame, style);
    }
}

/// Draws a small horizontal tick mark at each `Rep`'s `bottom` position
/// (task 5.4), for reps whose bottom has already been reached by
/// `current_frame_index`. A separate function rather than folding into
/// `render_overlay` since text labels (e.g. "rep 1 depth: ...") aren't
/// available (no font rendering, see this module's doc comment) — a tick is
/// the S-sized visual marker that's feasible without one, and callers who
/// don't track reps can simply not call this.
///
/// Uses `style`'s marker color and a fixed small rectangle (wider than
/// tall, to read as a horizontal tick rather than a dot) so it's visually
/// distinct from the current-position marker circle.
pub fn render_rep_bottoms(
    frame: &mut Frame,
    path: &BarPath,
    reps: &[Rep],
    velocity_frame_indices: &[u64],
    current_frame_index: u64,
    style: &OverlayStyle,
) {
    let tick_half_width: i64 = 8;
    let tick_height: i64 = 3;
    for rep in reps {
        let Some(&frame_index) = velocity_frame_indices.get(rep.bottom) else {
            continue;
        };
        if frame_index > current_frame_index {
            continue;
        }
        let Some(point) = path.position_at(frame_index) else {
            continue;
        };
        let cx = point.position.x.round() as i64;
        let cy = point.position.y.round() as i64;
        draw_filled_rect(
            frame,
            cx - tick_half_width,
            cy - tick_height / 2,
            tick_half_width * 2,
            tick_height,
            style.marker_color,
        );
    }
}

fn draw_legend(frame: &mut Frame, style: &OverlayStyle) {
    let swatch = style.legend_swatch_size as i64;
    let margin = style.legend_margin as i64;
    let padding = style.legend_padding as i64;
    let entries = [style.path_color, style.gap_color, style.marker_color];

    let box_w = swatch + padding * 2;
    let box_h = entries.len() as i64 * swatch + (entries.len() as i64 + 1) * padding;
    draw_filled_rect(frame, margin, margin, box_w, box_h, [30, 30, 30]);

    for (i, color) in entries.iter().enumerate() {
        let y = margin + padding + i as i64 * (swatch + padding);
        draw_filled_rect(frame, margin + padding, y, swatch, swatch, *color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bar_path::Timebase;
    use crate::geometry::Point;
    use crate::session::Sample;

    fn blank_frame(w: u32, h: u32) -> Frame {
        Frame::new(w, h, vec![0u8; w as usize * h as usize * 3]).unwrap()
    }

    fn sample(frame_index: u64, x: f64, y: f64, source: Source) -> Sample {
        Sample {
            frame_index,
            position: Point::new(x, y),
            source,
            confidence: None,
        }
    }

    fn tb() -> Timebase {
        Timebase::new(30, 1).unwrap()
    }

    #[test]
    fn draw_line_colors_the_midpoint_of_a_horizontal_segment() {
        let mut frame = blank_frame(20, 5);
        draw_line(&mut frame, 0, 2, 10, 2, 1, [0, 255, 0]);
        assert_eq!(frame.pixel(5, 2), Some([0, 255, 0]));
        assert_eq!(frame.pixel(0, 2), Some([0, 255, 0]));
        assert_eq!(frame.pixel(10, 2), Some([0, 255, 0]));
    }

    #[test]
    fn draw_line_with_thickness_colors_neighboring_rows() {
        let mut frame = blank_frame(20, 10);
        draw_line(&mut frame, 5, 5, 15, 5, 3, [0, 255, 0]);
        // thickness 3 => rows 4,5,6 colored at x=5
        assert_eq!(frame.pixel(5, 4), Some([0, 255, 0]));
        assert_eq!(frame.pixel(5, 5), Some([0, 255, 0]));
        assert_eq!(frame.pixel(5, 6), Some([0, 255, 0]));
    }

    #[test]
    fn draw_line_off_frame_does_not_panic() {
        let mut frame = blank_frame(5, 5);
        draw_line(&mut frame, -10, -10, 100, 100, 2, [1, 2, 3]);
        // frame untouched at valid interior far from the line's actual path
        assert_eq!(frame.pixel(0, 4), Some([0, 0, 0]));
    }

    // All positions below stay clear of the legend box, which occupies the
    // top-left corner (roughly x,y in 8..=25 with the default style).

    #[test]
    fn render_overlay_draws_path_between_tracked_points() {
        let mut frame = blank_frame(60, 60);
        let samples = vec![
            sample(0, 30.0, 40.0, Source::Tracked),
            sample(1, 50.0, 40.0, Source::Tracked),
        ];
        let path = BarPath::new(&samples, &[], tb(), 0);
        let style = OverlayStyle::builder()
            .line_thickness(1)
            .show_legend(false)
            .build();
        render_overlay(&mut frame, &path, 1, &style);
        // midpoint of the horizontal segment should be path-colored
        assert_eq!(frame.pixel(40, 40), Some(style.path_color()));
    }

    #[test]
    fn render_overlay_colors_gap_segments_differently() {
        let mut frame = blank_frame(60, 60);
        let samples = vec![
            sample(0, 30.0, 40.0, Source::Tracked),
            sample(1, 50.0, 40.0, Source::Interpolated),
        ];
        let path = BarPath::new(&samples, &[], tb(), 0);
        let style = OverlayStyle::builder()
            .line_thickness(1)
            .show_legend(false)
            .build();
        render_overlay(&mut frame, &path, 1, &style);
        assert_eq!(frame.pixel(40, 40), Some(style.gap_color()));
    }

    #[test]
    fn render_overlay_draws_marker_at_current_frame_position() {
        let mut frame = blank_frame(60, 60);
        let samples = vec![
            sample(0, 30.0, 30.0, Source::Tracked),
            sample(1, 50.0, 50.0, Source::Tracked),
        ];
        let path = BarPath::new(&samples, &[], tb(), 0);
        let style = OverlayStyle::builder()
            .marker_radius(2)
            .show_legend(false)
            .build();
        render_overlay(&mut frame, &path, 1, &style);
        assert_eq!(frame.pixel(50, 50), Some(style.marker_color()));
    }

    #[test]
    fn render_overlay_only_draws_up_to_current_frame_index() {
        let mut frame = blank_frame(60, 60);
        let samples = vec![
            sample(0, 30.0, 30.0, Source::Tracked),
            sample(1, 40.0, 40.0, Source::Tracked),
            sample(2, 55.0, 55.0, Source::Tracked),
        ];
        let path = BarPath::new(&samples, &[], tb(), 0);
        let style = OverlayStyle::builder()
            .marker_radius(1)
            .show_legend(false)
            .build();
        render_overlay(&mut frame, &path, 1, &style);
        // marker should be at frame 1's position, not frame 2's
        assert_eq!(frame.pixel(55, 55), Some([0, 0, 0]));
        assert_eq!(frame.pixel(40, 40), Some(style.marker_color()));
    }

    #[test]
    fn render_overlay_off_frame_positions_do_not_panic() {
        let mut frame = blank_frame(10, 10);
        let samples = vec![
            sample(0, -100.0, -100.0, Source::Tracked),
            sample(1, 500.0, 500.0, Source::Tracked),
        ];
        let path = BarPath::new(&samples, &[], tb(), 0);
        let style = OverlayStyle::builder().build();
        render_overlay(&mut frame, &path, 1, &style);
    }

    #[test]
    fn render_rep_bottoms_draws_tick_at_bottom_position() {
        let mut frame = blank_frame(60, 60);
        let samples = vec![
            sample(0, 30.0, 30.0, Source::Tracked),
            sample(1, 30.0, 40.0, Source::Tracked),
            sample(2, 30.0, 30.0, Source::Tracked),
        ];
        let path = BarPath::new(&samples, &[], tb(), 0);
        let velocity_frame_indices = vec![0u64, 1, 2];
        let reps = vec![Rep {
            eccentric_start: 0,
            bottom: 1,
            concentric_end: 2,
        }];
        let style = OverlayStyle::builder().show_legend(false).build();
        render_rep_bottoms(&mut frame, &path, &reps, &velocity_frame_indices, 2, &style);
        assert_eq!(frame.pixel(30, 40), Some(style.marker_color()));
    }

    #[test]
    fn render_rep_bottoms_skips_reps_not_yet_reached() {
        let mut frame = blank_frame(60, 60);
        let samples = vec![
            sample(0, 30.0, 30.0, Source::Tracked),
            sample(1, 30.0, 40.0, Source::Tracked),
            sample(2, 30.0, 30.0, Source::Tracked),
        ];
        let path = BarPath::new(&samples, &[], tb(), 0);
        let velocity_frame_indices = vec![0u64, 1, 2];
        let reps = vec![Rep {
            eccentric_start: 0,
            bottom: 1,
            concentric_end: 2,
        }];
        let style = OverlayStyle::builder().show_legend(false).build();
        // current_frame_index 0 is before the rep's bottom (frame 1).
        render_rep_bottoms(&mut frame, &path, &reps, &velocity_frame_indices, 0, &style);
        assert_eq!(frame.pixel(30, 40), Some([0, 0, 0]));
    }

    #[test]
    fn render_overlay_can_hide_legend() {
        let mut frame = blank_frame(20, 20);
        let samples = vec![sample(0, 5.0, 5.0, Source::Tracked)];
        let path = BarPath::new(&samples, &[], tb(), 0);
        let style = OverlayStyle::builder().show_legend(false).build();
        render_overlay(&mut frame, &path, 0, &style);
        assert_eq!(frame.pixel(0, 0), Some([0, 0, 0]));
    }
}
