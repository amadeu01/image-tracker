//! tracker-app: adapters (ffmpeg IO, egui UI, overlay render, CSV/JSON export).

pub mod app;
pub mod cli;
pub mod compare;
pub mod export_job;
pub mod ffmpeg_sink;
pub mod ffmpeg_source;
pub mod ffprobe;
pub mod frame_cache;
pub mod overlay_export;
pub mod screen_map;
pub mod seek_source;
pub mod telemetry;
pub mod thumbnail_strip;
pub mod thumbnail_worker;
pub mod tracking;
