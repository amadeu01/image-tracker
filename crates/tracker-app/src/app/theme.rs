//! Light/dark theme persistence (task 12.4) + stop-set threshold
//! persistence (task 13.5), sharing one small JSON file.
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
//! state to a RON blob) for a couple of scalars, this writes a tiny
//! standalone JSON file next to the log directory `telemetry.rs` already
//! resolves via `directories::ProjectDirs` — same crate, same
//! qualifier/org/app triple, already a dependency, no new one added.
//! Missing/corrupt/unwritable file is never fatal: `load` falls back to
//! `AppConfig::default()` (best-effort logging a warning on a parse
//! failure) and `save` best-effort logs a warning, exactly like
//! `telemetry.rs`'s own stance on log-directory failures.
//!
//! ## `theme.json` -> `settings.json` (13.5)
//! 12.4 originally wrote just `{"dark_mode": bool}` to `theme.json`. 13.5
//! adds a second persisted scalar (the stop-set velocity-loss threshold,
//! task 13.5's config), so the file is renamed `settings.json` and its
//! schema grows an optional `stop_threshold_pct`. A user upgrading from a
//! 12.4-era build has an existing `theme.json` but no `settings.json` yet:
//! `load` transparently falls back to reading the legacy file (dark_mode
//! only, no stop threshold) when `settings.json` doesn't exist, so the
//! theme override survives the rename silently — no explicit migration
//! step, no data loss, and the *next* save simply starts writing
//! `settings.json` (the old file is left alone, never deleted).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::telemetry;

#[derive(Debug, Default, Serialize, Deserialize)]
struct AppConfig {
    /// `Some` = explicit user override; `None` when the user has never
    /// toggled (so there's nothing to encode for "follow system").
    #[serde(default)]
    dark_mode: Option<bool>,
    /// Stop-set velocity-loss threshold (%), task 13.5. `None` when the
    /// user has never changed it from `TrackingSettings::default`'s 20%.
    #[serde(default)]
    stop_threshold_pct: Option<f64>,
    /// Bar-path overlay visibility (task 15.2). `None` when the user has
    /// never toggled it — treated as "show" (`AppState::new`'s default) —
    /// so files written by older builds keep working unchanged.
    #[serde(default)]
    show_path: Option<bool>,
    /// Whether "Export all rep clips" burns the per-rep overlay into each
    /// clip (task 19.3). `None` when the user has never toggled it —
    /// treated as "off" (`TrackingSettings::default`), so older
    /// `settings.json` files keep parsing unchanged.
    #[serde(default)]
    burn_overlay_in_rep_clips: Option<bool>,
}

fn config_path() -> PathBuf {
    // Sibling of the logs dir (`<state dir>/logs`) rather than a second
    // `ProjectDirs` resolution: `telemetry::log_dir()` already did the
    // qualifier/org/app lookup once, so reuse its parent.
    telemetry::log_dir()
        .parent()
        .map(|p| p.join("settings.json"))
        .unwrap_or_else(|| PathBuf::from("settings.json"))
}

/// Path of the pre-13.5 `theme.json`, read only as a silent migration
/// fallback (see module docs) — never written by this module anymore.
fn legacy_theme_config_path() -> PathBuf {
    telemetry::log_dir()
        .parent()
        .map(|p| p.join("theme.json"))
        .unwrap_or_else(|| PathBuf::from("theme.json"))
}

/// `theme.json`'s pre-13.5 schema, used only to parse the legacy migration
/// fallback.
#[derive(Debug, Deserialize)]
struct LegacyThemeConfig {
    dark_mode: bool,
}

/// Loads the persisted config, falling back to `AppConfig::default()` (no
/// override, no custom threshold) on any missing/corrupt file — reading
/// config must never be fatal. Falls back to the legacy `theme.json` (see
/// module docs) only when `settings.json` itself doesn't exist yet.
fn load() -> AppConfig {
    let path = config_path();
    if let Ok(contents) = std::fs::read_to_string(&path) {
        return match serde_json::from_str::<AppConfig>(&contents) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "failed to parse settings config");
                AppConfig::default()
            }
        };
    }
    // settings.json doesn't exist (yet) -- silently try the legacy file.
    let legacy_path = legacy_theme_config_path();
    let Ok(contents) = std::fs::read_to_string(&legacy_path) else {
        return AppConfig::default();
    };
    match serde_json::from_str::<LegacyThemeConfig>(&contents) {
        Ok(legacy) => AppConfig {
            dark_mode: Some(legacy.dark_mode),
            stop_threshold_pct: None,
            show_path: None,
            burn_overlay_in_rep_clips: None,
        },
        Err(_) => AppConfig::default(),
    }
}

fn write(cfg: &AppConfig) {
    let path = config_path();
    let contents = match serde_json::to_string(cfg) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "failed to serialize settings config");
            return;
        }
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(path = %parent.display(), error = %e, "failed to create settings config dir");
            return;
        }
    }
    if let Err(e) = std::fs::write(&path, contents) {
        tracing::warn!(path = %path.display(), error = %e, "failed to write settings config");
    }
}

/// Loads the persisted theme override, if any. `None` means "no override
/// yet — follow the system theme egui/winit already applied at startup."
pub fn load_override() -> Option<bool> {
    load().dark_mode
}

/// Persists an explicit theme override. Best-effort: a failure to write is
/// logged, never propagated (a theme toggle must never crash the app).
/// Preserves whatever `stop_threshold_pct` was already on disk.
pub fn save_override(dark_mode: bool) {
    let mut cfg = load();
    cfg.dark_mode = Some(dark_mode);
    write(&cfg);
}

/// Loads the persisted stop-set threshold (%), if the user has ever changed
/// it. `None` means "use `TrackingSettings::default`'s 20%."
pub fn load_stop_threshold() -> Option<f64> {
    load().stop_threshold_pct
}

/// Persists the stop-set threshold (%). Best-effort, same stance as
/// `save_override`. Preserves whatever `dark_mode` was already on disk.
pub fn save_stop_threshold(pct: f64) {
    let mut cfg = load();
    cfg.stop_threshold_pct = Some(pct);
    write(&cfg);
}

/// Loads the persisted bar-path overlay visibility (task 15.2), if the user
/// has ever toggled it. `None` means "use `AppState::new`'s default: shown."
pub fn load_show_path() -> Option<bool> {
    load().show_path
}

/// Persists the bar-path overlay visibility. Best-effort, same stance as
/// `save_override`. Preserves the other persisted fields already on disk.
pub fn save_show_path(show: bool) {
    let mut cfg = load();
    cfg.show_path = Some(show);
    write(&cfg);
}

/// Loads the persisted "burn overlay into rep clips" choice (task 19.3), if
/// the user has ever toggled it. `None` means "use
/// `TrackingSettings::default`'s off."
pub fn load_burn_overlay_in_rep_clips() -> Option<bool> {
    load().burn_overlay_in_rep_clips
}

/// Persists the "burn overlay into rep clips" choice. Best-effort, same
/// stance as `save_override`. Preserves the other persisted fields already
/// on disk.
pub fn save_burn_overlay_in_rep_clips(burn: bool) {
    let mut cfg = load();
    cfg.burn_overlay_in_rep_clips = Some(burn);
    write(&cfg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_path_lands_next_to_the_log_dir_named_settings_json() {
        let path = config_path();
        assert_eq!(path.file_name().unwrap(), "settings.json");
        assert_eq!(path.parent(), telemetry::log_dir().parent());
    }

    #[test]
    fn legacy_theme_config_path_is_the_old_theme_json_name() {
        let path = legacy_theme_config_path();
        assert_eq!(path.file_name().unwrap(), "theme.json");
    }

    #[test]
    fn load_falls_back_to_defaults_when_both_files_are_corrupt_or_missing() {
        // We can't isolate the real config dir per-test without an env var
        // seam (out of scope here — see telemetry.rs's own tests, which
        // have the same limitation), so this only asserts the *shape* of
        // the contract: parsing garbage never panics and yields defaults.
        let bogus: Result<AppConfig, _> = serde_json::from_str("not json");
        assert!(bogus.is_err());
        let legacy_bogus: Result<LegacyThemeConfig, _> = serde_json::from_str("not json");
        assert!(legacy_bogus.is_err());
    }

    #[test]
    fn app_config_round_trips_all_fields_through_json() {
        let cfg = AppConfig {
            dark_mode: Some(true),
            stop_threshold_pct: Some(15.0),
            show_path: Some(false),
            burn_overlay_in_rep_clips: Some(true),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.dark_mode, Some(true));
        assert_eq!(back.stop_threshold_pct, Some(15.0));
        assert_eq!(back.show_path, Some(false));
        assert_eq!(back.burn_overlay_in_rep_clips, Some(true));
    }

    #[test]
    fn missing_burn_overlay_in_rep_clips_field_parses_as_none_default_off() {
        // A settings.json written before 19.3 has no
        // `burn_overlay_in_rep_clips` key; it must parse (defaulting to
        // None = "off", matching `TrackingSettings::default`), not error.
        let back: AppConfig =
            serde_json::from_str(r#"{"dark_mode":true,"stop_threshold_pct":15.0}"#).unwrap();
        assert_eq!(back.burn_overlay_in_rep_clips, None);
    }

    #[test]
    fn missing_show_path_field_parses_as_none_default_shown() {
        // A settings.json written before 15.2 has no `show_path` key; it
        // must parse (defaulting to None = "show"), not error.
        let back: AppConfig =
            serde_json::from_str(r#"{"dark_mode":true,"stop_threshold_pct":15.0}"#).unwrap();
        assert_eq!(back.show_path, None);
    }

    #[test]
    fn legacy_schema_parses_into_dark_mode_only() {
        let legacy: LegacyThemeConfig = serde_json::from_str(r#"{"dark_mode":false}"#).unwrap();
        assert!(!legacy.dark_mode);
    }
}
