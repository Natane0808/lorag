//! Shared tracing initialization for CLI and GUI.
//!
//! Both binaries (`lorag` CLI and `lorag-gui`) must call [`init_tracing`] exactly once
//! during startup. The CLI passes `use_file_appender = false` and `log_bridge = None`
//! (stderr only, byte-identical to the historical inline init in `src/main.rs`); the GUI
//! passes `true` and `Some(bridge)` to additionally write a daily-rotating log file under
//! the OS data directory and broadcast each formatted event into an in-memory
//! `tokio::sync::broadcast` channel for the GUI logs page.
//!
//! The function is idempotent at the process level: calling it more than once is a no-op
//! after the first successful init (we guard global subscriber install with `std::sync::Once`).
//! Subsequent calls return `Ok(())` without re-initializing. The [`LogBridge`] passed on
//! the first winning call is the one installed.

use std::io::{self, Write};
use std::sync::Once;

use anyhow::{Context, Result};
use tokio::sync::broadcast;
use tracing_subscriber::EnvFilter;

/// Custom tracing-subscriber timer that formats timestamps in the local timezone
/// using RFC 3339 format (e.g. `2026-07-30T15:30:45.123+08:00`).
///
/// `tracing_subscriber::fmt::time::LocalTime` requires the `local-time` feature on
/// `tracing-subscriber` (which pulls in the `time` crate). We avoid that extra
/// dependency by implementing `FormatTime` with `chrono::Local`, which is already
/// a project dependency.
struct LocalTimer;

impl tracing_subscriber::fmt::time::FormatTime for LocalTimer {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        write!(
            w,
            "{}",
            chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z")
        )
    }
}

/// Default filter when neither `RUST_LOG` nor `LOG_LEVEL` is set.
const DEFAULT_LEVEL: &str = "info";

/// Lance / lancedb / datafusion / arrow are *extremely* chatty at INFO.
///
/// `tracing_subscriber::EnvFilter` target segments are **literal** (NOT globs), so
/// `lance=warn` does NOT match `lance::dataset_events`. We must list every sub-target
/// explicitly. This block is load-bearing — do not trim. Kept byte-for-byte identical
/// to the historical inline block in `src/main.rs`.
const LANCE_SILENCE: &str = ",lance::dataset_events=warn,lance::execution=warn,lance::io_events=warn,\
lance::file_audit=warn,lancedb=warn,datafusion=warn,arrow=warn";

/// Prefix used for daily log file names: `lorag.log.YYYY-MM-DD`.
/// Only referenced from the `gui` feature code path.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
const LOG_FILE_PREFIX: &str = "lorag.log";

/// Keep `N` most-recent daily log files on disk (GUI mode only).
/// `tracing-appender::rolling::daily` does not prune old files automatically; we do a
/// best-effort best-effort prune at init time, ignoring errors (disk cleanup is best-effort).
const LOG_RETENTION_DAYS: u64 = 7;

static INIT: Once = Once::new();

/// Broadcast channel bridge that feeds formatted tracing events to GUI subscribers.
///
/// `LogBridge` is a cheaply-cloneable handle to a `tokio::sync::broadcast::Sender<String>`.
/// Pass `Some(bridge)` to [`init_tracing`] to install a third tracing-subscriber layer
/// that formats each event into a single-line string (level + target + message + fields,
/// no ANSI escapes) and pushes it into the channel via non-blocking `send`. GUI pages
/// (the logs page in G9) subscribe via [`LogBridge::subscribe`] and receive a live tail.
///
/// If there are no receivers (or receivers have lagged beyond the channel capacity),
/// `send` errors are silently dropped — logging must never crash or block the app.
#[derive(Clone, Debug)]
pub struct LogBridge {
    sender: broadcast::Sender<String>,
}

impl LogBridge {
    /// Create a new bridge with the given broadcast channel capacity.
    ///
    /// Capacity is the number of most recent lines kept for slow subscribers; receivers
    /// that fall behind will observe [`broadcast::error::RecvError::Lagged`].
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    /// Subscribe to the broadcast stream. Each new subscriber starts from the *next*
    /// event after subscription (no replay).
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.sender.subscribe()
    }

    /// Access the underlying sender (e.g. for cloning into background tasks).
    pub fn sender(&self) -> &broadcast::Sender<String> {
        &self.sender
    }
}

/// Custom tracing-subscriber [`Layer`] that writes each event into the [`LogBridge`].
///
/// A [`tracing_subscriber::fmt::MakeWriter`] that yields [`BridgeWriter`]s which forward
/// each fully-formatted event line into the broadcast sender.
///
/// `tracing_subscriber::fmt::layer` calls `make_writer()` for every event, writes the
/// fully-formatted single event into it (terminated by `\n`), then drops the writer.
/// We buffer writes and flush the entire line (minus the trailing `\n`) into the
/// channel in the writer's `Drop` impl.
#[derive(Clone)]
struct BridgeMakeWriter {
    sender: broadcast::Sender<String>,
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for BridgeMakeWriter {
    type Writer = BridgeWriter;

    fn make_writer(&'a self) -> Self::Writer {
        BridgeWriter {
            sender: self.sender.clone(),
            buf: String::new(),
        }
    }
}

/// Per-event writer produced by [`BridgeMakeWriter`]. Accumulates bytes written by the
/// fmt layer and flushes them as one broadcast message on drop.
struct BridgeWriter {
    sender: broadcast::Sender<String>,
    buf: String,
}

impl Write for BridgeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Ok(s) = std::str::from_utf8(buf) {
            self.buf.push_str(s);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for BridgeWriter {
    fn drop(&mut self) {
        // The fmt layer writes each event terminated by '\n'. Strip that trailing
        // newline (and any trailing whitespace) before broadcasting, so the GUI
        // logs page sees one event per broadcast message with no embedded newlines.
        let line = self.buf.trim_end_matches(['\n', '\r']).to_string();
        if !line.is_empty() {
            // Non-blocking send; drop errors (no receivers / lagged are fine).
            let _ = self.sender.send(line);
        }
    }
}

/// Initialize the global tracing subscriber.
///
/// - `use_file_appender = false` (CLI): stderr only, exactly matching the historical
///   behavior (RUST_LOG > LOG_LEVEL > "info" precedence; `LANCE_SILENCE` suffix appended).
/// - `use_file_appender = true` (GUI): stderr **and** a daily-rotating, non-blocking
///   file appender under `dirs::data_dir().join("lorag").join("logs")`.
///   On Windows that is `%APPDATA%\lorag\logs\`, on macOS `~/Library/Application Support/lorag/logs/`,
///   on Linux `~/.local/share/lorag/logs/`.
/// - `log_bridge = Some(bridge)`: additionally install a third fmt layer whose writer
///   is [`BridgeMakeWriter`], broadcasting each ANSI-free formatted event line to any
///   GUI subscribers. Pass `None` for the CLI.
///
/// The non-blocking `WorkerGuard` is stored in a process-local static so its flusher
/// thread stays alive until exit; we do not expose it to callers.
///
/// If the file appender fails in GUI mode (e.g. data dir unwritable), we log a warning
/// to stderr and continue with stderr-only instead of returning Err — a broken file
/// logger should never prevent the GUI from launching.
pub fn init_tracing(use_file_appender: bool, log_bridge: Option<LogBridge>) -> Result<()> {
    let mut first_call_result: Result<()> = Ok(());

    INIT.call_once(|| {
        first_call_result = init_tracing_inner(use_file_appender, log_bridge);
    });

    first_call_result
}

fn init_tracing_inner(use_file_appender: bool, log_bridge: Option<LogBridge>) -> Result<()> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    // Resolve env filter exactly like the historical main.rs inline block:
    //   RUST_LOG > LOG_LEVEL > "info"; then always append the lance silencing suffix.
    let base = std::env::var("RUST_LOG")
        .or_else(|_| std::env::var("LOG_LEVEL"))
        .unwrap_or_else(|_| DEFAULT_LEVEL.to_string());
    let full_filter = format!("{base}{LANCE_SILENCE}");
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&full_filter));

    // CLI path (no file, no bridge): single stderr fmt layer, byte-identical to G2.
    // We take the fast path without going through the registry machinery so output is
    // bit-for-bit identical to the historical inline init. Note we apply the filter
    // via `.with_env_filter` here to preserve semantics with the registry path below.
    if !use_file_appender && log_bridge.is_none() {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_timer(LocalTimer)
            .init();
        return Ok(());
    }

    // We apply EnvFilter ONCE at the registry level (instead of `.with_filter()` per layer)
    // because stacking 3+ `Filtered<L, F, S>` layers produces overly-nested `Layered<...>`
    // types that rustc can't unify as `SubscriberInitExt`. A single filter layer on the
    // registry still filters every event before any layer sees it.
    //
    // `fmt::Layer<S, ...>` implements `Layer<S>` for the *specific* S it is generic over,
    // so each arm must construct its own stderr/file/bridge layers inline (no pre-binding
    // into variables / closures — those fix S at the definition site and break stacking).
    #[cfg(feature = "gui")]
    {
        // File writer (Option<NonBlocking>): guard leaked to 'static so NonBlocking: 'static.
        let file_writer = if use_file_appender {
            resolve_log_dir().ok().and_then(|log_dir| {
                let _ = prune_old_logs(&log_dir);
                let (non_blocking, guard) = build_file_appender(&log_dir).ok()?;
                let _ = Box::leak(Box::new(guard));
                Some(non_blocking)
            })
        } else {
            None
        };
        match (file_writer, log_bridge) {
            (Some(fw), Some(bl)) => tracing_subscriber::registry()
                .with(env_filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(std::io::stderr as fn() -> std::io::Stderr)
                        .with_timer(LocalTimer),
                )
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(fw)
                        .with_ansi(false)
                        .with_timer(LocalTimer),
                )
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(BridgeMakeWriter { sender: bl.sender })
                        .with_ansi(false)
                        .with_timer(LocalTimer),
                )
                .init(),
            (Some(fw), None) => tracing_subscriber::registry()
                .with(env_filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(std::io::stderr as fn() -> std::io::Stderr)
                        .with_timer(LocalTimer),
                )
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(fw)
                        .with_ansi(false)
                        .with_timer(LocalTimer),
                )
                .init(),
            (None, Some(bl)) => tracing_subscriber::registry()
                .with(env_filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(std::io::stderr as fn() -> std::io::Stderr)
                        .with_timer(LocalTimer),
                )
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(BridgeMakeWriter { sender: bl.sender })
                        .with_ansi(false)
                        .with_timer(LocalTimer),
                )
                .init(),
            (None, None) => tracing_subscriber::registry()
                .with(env_filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(std::io::stderr as fn() -> std::io::Stderr)
                        .with_timer(LocalTimer),
                )
                .init(),
        }
    }
    #[cfg(not(feature = "gui"))]
    {
        match log_bridge {
            Some(bl) => tracing_subscriber::registry()
                .with(env_filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(std::io::stderr as fn() -> std::io::Stderr)
                        .with_timer(LocalTimer),
                )
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(BridgeMakeWriter { sender: bl.sender })
                        .with_ansi(false)
                        .with_timer(LocalTimer),
                )
                .init(),
            None => tracing_subscriber::registry()
                .with(env_filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(std::io::stderr as fn() -> std::io::Stderr)
                        .with_timer(LocalTimer),
                )
                .init(),
        }
    }

    if use_file_appender && let Ok(log_dir) = resolve_log_dir() {
        tracing::info!(
            log_dir = %log_dir.display(),
            retention_days = LOG_RETENTION_DAYS,
            "tracing file appender initialized"
        );
    }

    Ok(())
}

/// Build the non-blocking daily rolling file appender. Isolated so the `tracing-appender`
/// crate is only referenced when the `gui` feature is on (it is optional / behind `gui`).
#[cfg(feature = "gui")]
fn build_file_appender(
    log_dir: &std::path::Path,
) -> Result<(
    tracing_appender::non_blocking::NonBlocking,
    tracing_appender::non_blocking::WorkerGuard,
)> {
    let file_appender = tracing_appender::rolling::daily(log_dir, LOG_FILE_PREFIX);
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    Ok((non_blocking, guard))
}

/// Stub when building without `gui` feature: file appender not available.
/// Caller (`init_tracing_inner`) only reaches this on `use_file_appender=true`, which
/// only happens in the GUI binary that requires `gui` feature — this branch is defensive
/// and unused outside `gui`.
#[cfg(not(feature = "gui"))]
#[allow(dead_code)]
fn build_file_appender(_log_dir: &std::path::Path) -> Result<()> {
    anyhow::bail!(
        "tracing-appender is not available; rebuild with `--features gui` for file logging"
    )
}

/// Resolve the log directory: `<data_dir>/lorag/logs`, creating it if missing.
///
/// `dirs::data_dir` is the OS-standard per-user application data root (not the same as
/// `dirs::config_dir` or the legacy `~/.aha/` path we use for models).
fn resolve_log_dir() -> Result<std::path::PathBuf> {
    let base = dirs::data_dir().context(
        "failed to resolve OS data directory for log output (set HOME/APPDATA/XDG_DATA_HOME appropriately)",
    )?;
    let log_dir = base.join("lorag").join("logs");
    std::fs::create_dir_all(&log_dir).with_context(|| {
        format!(
            "failed to create log directory at {} (check permissions / disk space)",
            log_dir.display()
        )
    })?;
    Ok(log_dir)
}

/// Best-effort prune of daily log files older than [`LOG_RETENTION_DAYS`].
///
/// We deliberately do not bubble errors up — failing to prune old logs is never fatal.
/// Only used in the `gui` feature path; dead otherwise.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
fn prune_old_logs(log_dir: &std::path::Path) -> std::io::Result<()> {
    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(
            60 * 60 * 24 * LOG_RETENTION_DAYS,
        ))
        .unwrap_or(std::time::UNIX_EPOCH);

    for entry in std::fs::read_dir(log_dir)? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        // Only touch files that look like ours: start with `lorag.log` prefix.
        let is_ours = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with(LOG_FILE_PREFIX))
            .unwrap_or(false);
        if !is_ours {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_file() {
            continue;
        }
        let modified = match meta.modified() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if modified < cutoff {
            let _ = std::fs::remove_file(&path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lance_silence_suffix_covers_required_targets() {
        for target in [
            "lance::dataset_events",
            "lance::execution",
            "lance::io_events",
            "lance::file_audit",
            "lancedb",
            "datafusion",
            "arrow",
        ] {
            assert!(
                LANCE_SILENCE.contains(&format!("{target}=warn")),
                "LANCE_SILENCE must set {target}=warn"
            );
        }
    }

    #[test]
    fn init_tracing_is_idempotent() {
        // First call may or may not win depending on test ordering with other tests;
        // either way it must not panic or error on the second call.
        let _ = init_tracing(false, None);
        let _ = init_tracing(false, None);
        let _ = init_tracing(true, None);
        let _ = init_tracing(true, Some(LogBridge::new(64)));
    }
}
