//! Timeline thumbnail strip (task 10.6): ~20 sampled-frame thumbnails
//! rendered above the scrub bar, decoded once per video by
//! `thumbnail_worker` and progressively uploaded as textures by
//! `TrackerApp::poll_thumbnails`. Clicking a thumbnail jumps to its frame; a
//! highlighted border marks whichever thumbnail is closest to the current
//! frame. Hidden entirely with no video loaded (10.5's empty state).

use eframe::egui;

use super::TrackerApp;
use crate::thumbnail_strip::{self, THUMBNAIL_HEIGHT};

pub fn show(ctx: &egui::Context, app: &mut TrackerApp) {
    if app.state.is_none() {
        return; // empty state (10.5): no video, nothing to show
    }
    // Cloned up front so the rest of this function can borrow `app` mutably
    // (for texture uploads and jump-to-frame clicks) without also holding a
    // live borrow of `app.thumbnails`.
    let Some(frame_indices) = app.thumbnails.as_ref().map(|h| h.frame_indices.clone()) else {
        return;
    };
    if frame_indices.is_empty() {
        return;
    }
    let current_frame = app.state.as_ref().map(|s| s.current_frame).unwrap_or(0);
    let highlighted = thumbnail_strip::nearest_slot(&frame_indices, current_frame);

    egui::TopBottomPanel::bottom("thumbnail_strip").show(ctx, |ui| {
        ui.horizontal(|ui| {
            for (slot, &frame_index) in frame_indices.iter().enumerate() {
                let is_current = highlighted == Some(slot);
                match &app.thumbnail_textures[slot] {
                    Some(tex) => {
                        let size = tex.size_vec2();
                        let response = ui
                            .add(egui::ImageButton::new((tex.id(), size)).selected(is_current))
                            .on_hover_text(format!("jump to frame {frame_index}"));
                        if response.clicked() {
                            if let Some(state) = app.state.as_mut() {
                                state.set_frame(frame_index as i64);
                            }
                        }
                    }
                    None => {
                        // Placeholder box for a thumbnail that hasn't
                        // arrived from the decode thread yet (10.6:
                        // "placeholder boxes fill in as thumbs arrive").
                        let width = THUMBNAIL_HEIGHT as f32 * 16.0 / 9.0; // rough guess pre-decode
                        let (rect, _response) = ui.allocate_exact_size(
                            egui::vec2(width, THUMBNAIL_HEIGHT as f32),
                            egui::Sense::hover(),
                        );
                        ui.painter()
                            .rect_filled(rect, 2.0, egui::Color32::from_gray(60));
                    }
                }
            }
        });
    });
}
