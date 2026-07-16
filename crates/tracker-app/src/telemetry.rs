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

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// Qualifier/organization/application used to resolve the OS-specific state
/// directory via `directories::ProjectDirs`.
const QUALIFIER: &str = "";
const ORGANIZATION: &str = "amadeu01";
const APPLICATION: &str = "image-tracker";

/// How many breadcrumbs the ring buffer keeps; older ones are dropped as new
/// ones arrive. Chosen to comfortably cover "what led up to this" without the
/// crash report becoming unwieldy to paste into an LLM.
const MAX_BREADCRUMBS: usize = 50;

/// Global breadcrumb ring: one line per `info!`/`warn!`/`error!` `tracing`
/// event, captured by [`BreadcrumbLayer`] regardless of which thread emitted
/// it (worker threads included — tracking/export/thumbnail/benchmark all run
/// on their own threads and none of them go through `AppState::push_event`).
/// A `Mutex<VecDeque<_>>` rather than a lock-free structure: events are rare
/// enough (human/frame-scale, not per-pixel) that mutex contention is a
/// non-issue, and it keeps [`crash_report`] trivially able to snapshot it.
static BREADCRUMBS: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();

fn breadcrumb_ring() -> &'static Mutex<VecDeque<String>> {
    BREADCRUMBS.get_or_init(|| Mutex::new(VecDeque::with_capacity(MAX_BREADCRUMBS)))
}

/// Appends one line to the global breadcrumb ring, evicting the oldest entry
/// once at capacity. Exposed (not just used internally by the `Layer`) so
/// tests can drive it directly without going through a full subscriber.
fn record_breadcrumb(line: String) {
    let mut ring = breadcrumb_ring().lock().unwrap_or_else(|e| e.into_inner());
    if ring.len() >= MAX_BREADCRUMBS {
        ring.pop_front();
    }
    ring.push_back(line);
}

/// Snapshot of the current breadcrumb ring, oldest first.
pub fn breadcrumbs() -> Vec<String> {
    breadcrumb_ring()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .cloned()
        .collect()
}

/// Startup banner text (version/os/arch/ffmpeg), captured once at process
/// start via [`set_startup_banner`] so the crash report can include it
/// without re-deriving it (re-spawning `ffmpeg -version` from inside a panic
/// hook would be needlessly heavy, and the values don't change at runtime
/// anyway).
static STARTUP_BANNER: OnceLock<String> = OnceLock::new();

/// Records the startup banner text for later inclusion in crash reports.
/// Call once, early in `main`, after computing the banner. Calling it more
/// than once is a no-op (first call wins) — deliberately, since the value
/// never needs to change mid-process.
pub fn set_startup_banner(banner: String) {
    let _ = STARTUP_BANNER.set(banner);
}

fn startup_banner() -> String {
    STARTUP_BANNER
        .get()
        .cloned()
        .unwrap_or_else(|| "<startup banner unavailable>".to_string())
}

/// A `tracing_subscriber::Layer` that formats every `INFO`-and-above event
/// into a single line ("breadcrumb") and appends it to the global ring
/// buffer, alongside whatever the console/file layers do with it. This is
/// the preferred capture mechanism over a per-call-site `push_event`-style
/// API: it's zero extra work at each `tracing::info!` call site, and it sees
/// every event from every thread (registry layers run for the whole
/// process), so worker-thread breadcrumbs (tracking/export/thumbnail
/// decode/benchmark) land here automatically.
struct BreadcrumbLayer;

impl<S> Layer<S> for BreadcrumbLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        record_breadcrumb(format_breadcrumb(event));
    }
}

/// Renders one `tracing::Event` as a single self-contained line: timestamp,
/// level, target, message, and any structured fields — everything needed to
/// make sense of the breadcrumb without cross-referencing the full log file.
fn format_breadcrumb(event: &tracing::Event<'_>) -> String {
    struct FieldPrinter {
        message: Option<String>,
        fields: Vec<String>,
    }
    impl tracing::field::Visit for FieldPrinter {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.message = Some(format!("{value:?}"));
            } else {
                self.fields.push(format!("{}={:?}", field.name(), value));
            }
        }
    }
    let mut printer = FieldPrinter {
        message: None,
        fields: Vec::new(),
    };
    event.record(&mut printer);

    let metadata = event.metadata();
    let now = unix_timestamp_millis();
    let message = printer.message.unwrap_or_default();
    if printer.fields.is_empty() {
        format!("{now} {} {} {message}", metadata.level(), metadata.target())
    } else {
        format!(
            "{now} {} {} {message} {}",
            metadata.level(),
            metadata.target(),
            printer.fields.join(" ")
        )
    }
}

/// Milliseconds-resolution wall-clock timestamp, formatted as a plain
/// `<unix-seconds>.<millis>` string. Avoids pulling in a full date/time
/// dependency (`chrono`) just for breadcrumb lines — an LLM reading the
/// crash report doesn't need a calendar date, just relative ordering, and
/// the crash filename itself already carries a human-checkable timestamp.
fn unix_timestamp_millis() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}", now.as_secs(), now.subsec_millis())
}

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
        Some(dirs) => dirs
            .state_dir()
            .unwrap_or_else(|| dirs.data_dir())
            .join("logs"),
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
                .with(json_layer)
                .with(BreadcrumbLayer);

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
            let registry = tracing_subscriber::registry()
                .with(console_layer)
                .with(BreadcrumbLayer);
            let _ = registry.try_init();
            (TelemetryGuard { _file_guard: None }, None)
        }
    }
}

/// Default filter used by both layers: `RUST_LOG` if set, else `info`.
fn env_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
}

/// Installs a panic hook that logs the panic message, source location, and a
/// captured backtrace at `error` level through `tracing` before chaining to
/// the previous (default) hook so the usual stderr output still happens.
///
/// Call this after [`init`] so the log layers are already installed and the
/// panic gets a chance to reach the file layer.
///
/// # Why this still gets the log line out
///
/// This hook runs synchronously *before* the unwind (or abort, if the panic
/// happens while already unwinding, or the process is built with
/// `panic = "abort"`) — so the `tracing::error!` call below always executes.
/// What's not guaranteed is that the *file* layer's non-blocking writer has
/// flushed its background thread before the process actually exits: for the
/// main thread, a panic unwinds up through `main` and the `TelemetryGuard`
/// held there is dropped on the way out, which blocks until the writer
/// thread drains its queue. A panic on a non-main thread with the default
/// `panic = "unwind"` strategy only kills that thread, so the process (and
/// its guard) lives on and flushes normally on eventual exit; only an abort
/// (`panic = "abort"`, or a double panic) skips unwinding entirely, in which
/// case the OS may still deliver already-buffered writer output, but this is
/// not guaranteed — hence why the hook logs synchronously via `tracing`
/// *before* any abort/unwind proceeds, rather than relying on drop order.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let (message, location) = panic_report_fields(info);
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!(
            panic.message = %message,
            panic.location = %location,
            panic.backtrace = %backtrace,
            "tracker-app panicked"
        );

        // Best-effort crash bundle (task 12.2): a single self-contained file
        // with the startup banner, the last breadcrumbs leading up to the
        // panic, and the panic itself — meant to be pasted whole into an LLM
        // for debugging, without needing to go dig through the rotating
        // JSON log file. Any failure here (dir missing, disk full, ...) is
        // swallowed: writing the crash bundle must never mask the panic
        // itself or interfere with the previous hook still running below.
        let report = crash_report(
            &startup_banner(),
            &breadcrumbs(),
            &message,
            &location,
            &backtrace.to_string(),
        );
        match write_crash_report(&log_dir(), &report) {
            Ok(path) => eprintln!("crash report written to {}", path.display()),
            Err(_) => { /* best-effort; nothing more we can safely do here */ }
        }

        previous(info);
    }));
}

/// Builds the crash bundle's full text. Pure (no I/O) so it's directly
/// unit-testable without triggering a real panic.
fn crash_report(
    banner: &str,
    breadcrumbs: &[String],
    panic_message: &str,
    panic_location: &str,
    backtrace: &str,
) -> String {
    let mut out = String::new();
    out.push_str("=== tracker-app crash report ===\n\n");
    out.push_str("-- startup --\n");
    out.push_str(banner);
    out.push_str("\n\n");
    out.push_str(&format!("-- last {} breadcrumbs --\n", breadcrumbs.len()));
    if breadcrumbs.is_empty() {
        out.push_str("(none captured)\n");
    } else {
        for line in breadcrumbs {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str("\n-- panic --\n");
    out.push_str(&format!("message:  {panic_message}\n"));
    out.push_str(&format!("location: {panic_location}\n\n"));
    out.push_str("-- backtrace --\n");
    out.push_str(backtrace);
    out.push('\n');
    out
}

/// Writes `report` to `<dir>/crash-<unix-ts>.log`, creating `dir` if needed.
/// Returns the path written on success. Best-effort by design — see the call
/// site in [`install_panic_hook`] for why failures here must stay silent to
/// the panic itself.
fn write_crash_report(dir: &Path, report: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let path = dir.join(format!("crash-{ts}.log"));
    std::fs::write(&path, report)?;
    Ok(path)
}

/// Extracts a human-readable message and `file:line:col` location from a
/// `PanicHookInfo`. Factored out from [`install_panic_hook`] so the
/// formatting is unit-testable without triggering an actual panic.
fn panic_report_fields(info: &std::panic::PanicHookInfo<'_>) -> (String, String) {
    let message = if let Some(s) = info.payload().downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    };
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "<unknown location>".to_string());
    (message, location)
}

/// Returns the first line of `ffmpeg -version` output (e.g. `ffmpeg version
/// 6.1.1 Copyright (c) 2000-2023 the FFmpeg developers`), or `"unavailable"`
/// if `ffmpeg` isn't on `PATH`, fails to run, or produces no output. Used by
/// the startup banner; never fails startup itself.
pub fn ffmpeg_version_summary() -> String {
    match std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
    {
        Ok(output) => first_line_or_unavailable(&output.stdout),
        Err(_) => "unavailable".to_string(),
    }
}

/// Pure helper: extracts the first line of raw `ffmpeg -version` stdout
/// bytes, trimmed, falling back to `"unavailable"` if empty or not valid
/// UTF-8. Split out from [`ffmpeg_version_summary`] for unit testing without
/// spawning a process.
fn first_line_or_unavailable(stdout: &[u8]) -> String {
    match std::str::from_utf8(stdout) {
        Ok(text) => match text.lines().next() {
            Some(line) if !line.trim().is_empty() => line.trim().to_string(),
            _ => "unavailable".to_string(),
        },
        Err(_) => "unavailable".to_string(),
    }
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

    #[test]
    fn first_line_or_unavailable_extracts_first_line() {
        let stdout =
            b"ffmpeg version 6.1.1 Copyright (c) 2000-2023 the FFmpeg developers\nbuilt with gcc\n";
        assert_eq!(
            first_line_or_unavailable(stdout),
            "ffmpeg version 6.1.1 Copyright (c) 2000-2023 the FFmpeg developers"
        );
    }

    #[test]
    fn first_line_or_unavailable_reports_unavailable_on_empty_output() {
        assert_eq!(first_line_or_unavailable(b""), "unavailable");
        assert_eq!(first_line_or_unavailable(b"\n\n"), "unavailable");
    }

    #[test]
    fn first_line_or_unavailable_reports_unavailable_on_invalid_utf8() {
        assert_eq!(
            first_line_or_unavailable(&[0xff, 0xfe, 0x00]),
            "unavailable"
        );
    }

    #[test]
    fn ffmpeg_version_summary_never_panics() {
        // Whether or not ffmpeg is installed in the test environment, this
        // must return a plain string rather than failing.
        let summary = ffmpeg_version_summary();
        assert!(!summary.is_empty());
    }

    // -- 12.2: breadcrumb ring + crash report ----------------------------

    // The ring is process-global by design (that's the whole point: it must
    // see events from every thread, including other tests' `tracing::info!`
    // calls elsewhere in this crate, which run concurrently in the same test
    // binary). Serialize the tests that clear-and-assert-on it so they don't
    // race each other; other tests scattered across the crate that merely
    // *emit* an event are still free to interleave, since none of them
    // assert on ring contents.
    static RING_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn record_breadcrumb_caps_at_max_breadcrumbs() {
        let _guard = RING_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Uses the real global ring (it's process-global by design), so
        // clear it first and push one more than the cap to check eviction.
        {
            let mut ring = breadcrumb_ring().lock().unwrap();
            ring.clear();
        }
        for i in 0..(MAX_BREADCRUMBS + 10) {
            record_breadcrumb(format!("line {i}"));
        }
        let snapshot = breadcrumbs();
        assert_eq!(snapshot.len(), MAX_BREADCRUMBS);
        // Oldest entries evicted; the most recent one survives.
        assert_eq!(
            snapshot.last().unwrap(),
            &format!("line {}", MAX_BREADCRUMBS + 9)
        );
    }

    #[test]
    fn crash_report_includes_banner_breadcrumbs_and_panic_info() {
        let report = crash_report(
            "tracker-app 0.1.0 (linux/x86_64); ffmpeg: unavailable",
            &["info tracker_app video opened".to_string()],
            "index out of bounds",
            "src/foo.rs:12:5",
            "0: foo::bar\n1: main",
        );
        assert!(report.contains("tracker-app 0.1.0"));
        assert!(report.contains("info tracker_app video opened"));
        assert!(report.contains("index out of bounds"));
        assert!(report.contains("src/foo.rs:12:5"));
        assert!(report.contains("foo::bar"));
    }

    #[test]
    fn crash_report_handles_empty_breadcrumbs() {
        let report = crash_report("banner", &[], "boom", "loc", "bt");
        assert!(report.contains("(none captured)"));
    }

    #[test]
    fn write_crash_report_creates_dir_and_file_and_returns_its_path() {
        let dir =
            std::env::temp_dir().join(format!("image-tracker-crash-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = write_crash_report(&dir, "hello crash").unwrap();
        assert!(path.starts_with(&dir));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello crash");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn breadcrumb_layer_captures_tracing_info_events() {
        let _guard = RING_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Registering a *second* global subscriber isn't possible once one
        // is set (tests share a process), so drive the layer directly the
        // same way `on_event` does, rather than via `tracing::info!` through
        // a fresh registry.
        {
            let mut ring = breadcrumb_ring().lock().unwrap();
            ring.clear();
        }
        let subscriber = tracing_subscriber::registry().with(BreadcrumbLayer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(marker = "xyz123", "a breadcrumb-worthy event");
        });
        let snapshot = breadcrumbs();
        assert_eq!(snapshot.len(), 1);
        assert!(snapshot[0].contains("a breadcrumb-worthy event"));
        assert!(snapshot[0].contains("marker=\"xyz123\""));
    }

    #[test]
    fn set_startup_banner_first_call_wins() {
        // Global OnceLock shared across tests in this file; only assert the
        // no-op-on-repeat contract, not a specific value (another test may
        // have already set it first depending on run order).
        set_startup_banner("first".to_string());
        set_startup_banner("second".to_string());
        let banner = startup_banner();
        assert_ne!(banner, "<startup banner unavailable>");
    }
}
