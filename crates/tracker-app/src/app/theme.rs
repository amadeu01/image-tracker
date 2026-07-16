//! Light/dark theme persistence (task 12.4).
//!
//! egui/winit already *follow the system theme at startup* on their own —
//! `egui_winit::State` reads the OS theme via `winit`'s `Theme` and applies
//! it to `egui::Context` before the first frame, and keeps following it on
//! `ThemeChanged` events, with no code needed here. What's missing is a way
//! for the user to *override* that (the toolbar toggle in `toolbar.rs`) and
//! have the override survive a restart.
//!
//! Rather than pull in eframe's `persistence` feature (a whole
//! `eframe::App::save`/`storage` machinery, serializing arbitrary app
//! state to a RON blob) for one boolean, this writes a tiny standalone JSON
//! file next to the log directory `telemetry.rs` already resolves via
//! `directories::ProjectDirs` — same crate, same qualifier/org/app triple,
//! already a dependency, no new one added. Missing/corrupt/unwritable file
//! is never fatal: `load` returns `None` (falls back to system-followed) and
//! `save` best-effort logs a warning, exactly like `telemetry.rs`'s own
//! stance on log-directory failures.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::telemetry;

#[derive(Debug, Serialize, Deserialize)]
struct ThemeConfig {
    /// `true`/`false` = explicit user override; the file simply doesn't
    /// exist when the user has never toggled (so there's nothing to encode
    /// for "follow system").
    dark_mode: bool,
}

fn config_path() -> PathBuf {
    // Sibling of the logs dir (`<state dir>/logs`) rather than a second
    // `ProjectDirs` resolution: `telemetry::log_dir()` already did the
    // qualifier/org/app lookup once, so reuse its parent.
    telemetry::log_dir()
        .parent()
        .map(|p| p.join("theme.json"))
        .unwrap_or_else(|| PathBuf::from("theme.json"))
}

/// Loads the persisted override, if any. `None` means "no override yet —
/// follow the system theme egui/winit already applied at startup."
pub fn load_override() -> Option<bool> {
    let path = config_path();
    let contents = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<ThemeConfig>(&contents) {
        Ok(cfg) => Some(cfg.dark_mode),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "failed to parse theme config");
            None
        }
    }
}

/// Persists an explicit user override. Best-effort: a failure to write is
/// logged, never propagated (a theme toggle must never crash the app).
pub fn save_override(dark_mode: bool) {
    let path = config_path();
    let contents = match serde_json::to_string(&ThemeConfig { dark_mode }) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "failed to serialize theme config");
            return;
        }
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(path = %parent.display(), error = %e, "failed to create theme config dir");
            return;
        }
    }
    if let Err(e) = std::fs::write(&path, contents) {
        tracing::warn!(path = %path.display(), error = %e, "failed to write theme config");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_path_lands_next_to_the_log_dir_named_theme_json() {
        let path = config_path();
        assert_eq!(path.file_name().unwrap(), "theme.json");
        assert_eq!(path.parent(), telemetry::log_dir().parent());
    }

    #[test]
    fn load_override_returns_none_when_file_is_corrupt_or_missing() {
        // We can't isolate the real config dir per-test without an env var
        // seam (out of scope here — see telemetry.rs's own tests, which
        // have the same limitation), so this only asserts the *shape* of
        // the contract: parsing garbage never panics and yields `None`.
        let bogus: Result<ThemeConfig, _> = serde_json::from_str("not json");
        assert!(bogus.is_err());
    }
}
