//! Logging foundation (task 8.1): `tracing` + `tracing-subscriber` wired up
//! for `tracker-app` only — `tracker-core` stays dependency-free and never
//! instruments itself; only adapters (this crate) do.
//!
//! [`init`] builds a `tracing_subscriber::registry()` with two [`Layer`]s:
//!
//! 1. A pretty console layer, filtered by `RUST_LOG` (default `info`),
//!    writing to stdout for humans watching the terminal.
//! 2. A JSON-lines rolling file layer (daily rotation, via
//!    `tracing-appender`) writing to the OS state directory resolved by
//!    [`log_dir`], for machine-readable logs that survive the process.
//!
//! # Adding a third layer (Sentry, Datadog, ...)
//!
//! The `Layer` trait is the plug-in point: `tracing_subscriber::Registry`
//! is a `Vec`-like stack of layers, each of which sees every span/event and
//! decides independently what to do with it. A future error-reporting layer
//! (e.g. `sentry-tracing`'s `sentry_tracing::layer()`, or a Datadog OTLP
//! layer) slots in with one more `.with(...)` call in [`init`] below —
//! no changes to instrumented code elsewhere in the crate are needed, since
//! call sites just use `tracing::info!`/`error!`/`#[instrument]` and don't
//! know which layers are subscribed.
//!
//! # Failure handling
//!
//! Telemetry must never take the app down. If the log directory can't be
//! resolved or created, or the file layer can't be installed, [`init`]
//! falls back to a console-only subscriber (or, if even that fails, leaves
//! the default no-op subscriber in place) and returns `Ok` with a `None`
//! log file path rather than propagating an error that would abort `main`.

use std::path::{Path, PathBuf};

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// Qualifier/organization/application used to resolve the OS-specific state
/// directory via `directories::ProjectDirs`.
const QUALIFIER: &str = "";
const ORGANIZATION: &str = "amadeu01";
const APPLICATION: &str = "image-tracker";

/// Holds resources that must stay alive for the lifetime of the process so
/// buffered log lines get flushed on exit (e.g. the `tracing-appender`
/// worker thread's guard). Drop this only when the process is shutting
/// down — typically by holding it in a local binding in `main`.
pub struct TelemetryGuard {
    _file_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
}

/// Resolves the directory logs should be written to: `<state
/// dir>/image-tracker/logs` per `directories::ProjectDirs`, falling back to
/// `$TMPDIR/image-tracker-logs` if the OS state/data directory can't be
/// determined (e.g. no home directory in the environment).
///
/// Pure and unit-testable: doesn't create the directory or touch the
/// filesystem, just computes the path.
pub fn log_dir() -> PathBuf {
    match directories::ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION) {
        Some(dirs) => dirs.state_dir().unwrap_or_else(|| dirs.data_dir()).join("logs"),
        None => std::env::temp_dir().join("image-tracker-logs"),
    }
}

/// Sets up global `tracing` subscription: console + JSON file layers.
///
/// Never fails in a way that aborts the caller: on any error setting up the
/// file layer (directory creation, etc.), falls back to a console-only
/// subscriber. Returns the resolved log file path (if file logging is
/// active) alongside the guard that must be kept alive for logs to flush.
pub fn init() -> (TelemetryGuard, Option<PathBuf>) {
    let console_layer = tracing_subscriber::fmt::layer()
        .pretty()
        .with_filter(env_filter());

    let dir = log_dir();
    match std::fs::create_dir_all(&dir) {
        Ok(()) => {
            let file_appender = tracing_appender::rolling::daily(&dir, "image-tracker.log");
            let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
            let json_layer = tracing_subscriber::fmt::layer()
                .json()
                .with_writer(non_blocking)
                .with_filter(env_filter());

            let registry = tracing_subscriber::registry()
                .with(console_layer)
                .with(json_layer);

            if registry.try_init().is_err() {
                // A global subscriber is already set (e.g. under test).
                // Not fatal: telemetry just no-ops for this process.
            }

            (
                TelemetryGuard {
                    _file_guard: Some(guard),
                },
                Some(resolved_log_path(&dir)),
            )
        }
        Err(_) => {
            // Couldn't create the log directory: fall back to console-only
            // logging rather than failing startup.
            let registry = tracing_subscriber::registry().with(console_layer);
            let _ = registry.try_init();
            (TelemetryGuard { _file_guard: None }, None)
        }
    }
}

/// Default filter used by both layers: `RUST_LOG` if set, else `info`.
fn env_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
}

/// `tracing_appender::rolling` doesn't expose the exact filename it'll
/// write today (it's date-suffixed internally), so this reports the
/// directory it writes into, which is what users need to find their logs.
fn resolved_log_path(dir: &Path) -> PathBuf {
    dir.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_dir_resolves_to_a_non_empty_path_ending_in_logs() {
        let dir = log_dir();
        assert_eq!(dir.file_name().unwrap(), "logs");
    }

    #[test]
    fn log_dir_is_stable_across_calls() {
        assert_eq!(log_dir(), log_dir());
    }
}
