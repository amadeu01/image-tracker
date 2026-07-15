//! tracker-app: adapters (ffmpeg IO, egui UI, overlay render, CSV/JSON export).

pub mod app;
pub mod cli;
pub mod ffmpeg_sink;
pub mod ffmpeg_source;
pub mod ffprobe;
pub mod frame_cache;
pub mod screen_map;
pub mod seek_source;
pub mod tracking;
