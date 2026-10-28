//! Run tracking from the UI (task 2.6): a background thread drives a
//! `TemplateTracker`/`TrackingSession` (tracker-core) across the video from
//! the Seed's frame to the end, streaming progress back to the UI thread
//! over a channel so egui stays responsive.
//!
//! Split in two halves, the pure half is TDD'd directly:
//! - `TrackingRunState` — a pure reducer over `TrackingMessage`s (no egui,
//!   no threads, no IO). This is what's unit-tested below.
//! - `spawn_tracking`/`run_tracking_worker` — thin thread/channel wiring
//!   that `app.rs` calls into; not unit-tested (would just be testing
//!   `std::thread`/`mpsc`), verified instead by the manual smoke run.

mod config;
mod run_loop;
mod state;
mod worker;

pub use config::*;
pub use state::*;
pub use worker::*;
