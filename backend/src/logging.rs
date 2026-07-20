//! File logging with a fixed retention window.
//!
//! Every run appends to a daily log under `logs/`, so there is a durable record of what
//! the driver did. That record is the point: a hang shows up as an operation that logged
//! its start and never logged completion, and a misbehaving scope shows up as the exact
//! sequence of transactions that preceded it.
//!
//! Logs older than [`RETENTION_DAYS`] are deleted on startup.
//!
//! This installs a **global** subscriber, so it belongs to the application, not the
//! library — call it once from `main`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter, Layer};

/// How long log files are kept. Older files are removed when logging initialises.
pub const RETENTION_DAYS: u64 = 3;

/// Default directory for log files, relative to the working directory.
pub const DEFAULT_LOG_DIR: &str = "logs";

/// Base filename; the rolling appender appends a `.YYYY-MM-DD` suffix.
const FILE_PREFIX: &str = "openmso5202d.log";

/// Environment variable that overrides the console log level (e.g. `MSO_LOG=trace`).
const ENV_FILTER: &str = "MSO_LOG";

/// Holds the background log-writing thread alive.
///
/// Logging stops when this is dropped, so keep it for the lifetime of the program —
/// dropping it early silently loses buffered lines.
#[must_use = "logging stops when the guard is dropped; keep it alive for the whole program"]
pub struct LogGuard {
    _worker: WorkerGuard,
    path: PathBuf,
}

impl LogGuard {
    /// The directory log files are being written to.
    pub fn directory(&self) -> &Path {
        &self.path
    }
}

/// Initialise logging into [`DEFAULT_LOG_DIR`].
pub fn init() -> std::io::Result<LogGuard> {
    init_in(DEFAULT_LOG_DIR)
}

/// Initialise logging into `dir`, creating it if needed and pruning expired files.
///
/// The file log captures `DEBUG` and above — every device operation and USB transaction —
/// so the durable record is complete regardless of what the console shows.
///
/// The console stays quiet at `WARN`, leaving the application's own output readable.
/// Override with the `MSO_LOG` environment variable to watch a run live or re-run a stuck
/// session verbosely: `MSO_LOG=info` for the step trace, `MSO_LOG=trace` for every USB
/// transaction.
pub fn init_in(dir: impl AsRef<Path>) -> std::io::Result<LogGuard> {
    let dir = dir.as_ref().to_path_buf();
    fs::create_dir_all(&dir)?;
    prune_expired(&dir);

    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(FILE_PREFIX)
        // Backstop in case the process runs for longer than the retention window without
        // restarting, since pruning only happens here at startup.
        .max_log_files(RETENTION_DAYS as usize + 1)
        .build(&dir)
        .map_err(std::io::Error::other)?;
    let (writer, worker) = tracing_appender::non_blocking(appender);

    let to_file = fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .with_target(true)
        .with_thread_ids(true)
        .with_filter(EnvFilter::new("debug"));

    let to_console = fmt::layer().with_writer(std::io::stderr).with_filter(
        EnvFilter::try_from_env(ENV_FILTER).unwrap_or_else(|_| EnvFilter::new("warn")),
    );

    // `try_init` rather than `init`: a second call (e.g. from a test) should not panic.
    let _ = tracing_subscriber::registry()
        .with(to_file)
        .with(to_console)
        .try_init();

    tracing::info!(
        directory = %dir.display(),
        retention_days = RETENTION_DAYS,
        "logging started"
    );
    Ok(LogGuard {
        _worker: worker,
        path: dir,
    })
}

/// Delete log files last modified more than [`RETENTION_DAYS`] ago.
///
/// Best-effort: a file we cannot inspect or remove is left alone rather than failing
/// startup over housekeeping. Only files matching our own prefix are ever considered.
fn prune_expired(dir: &Path) {
    let max_age = Duration::from_secs(RETENTION_DAYS * 24 * 60 * 60);
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(FILE_PREFIX) {
            continue;
        }
        let expired = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .and_then(|modified| SystemTime::now().duration_since(modified).map_err(std::io::Error::other))
            .map(|age| age > max_age)
            .unwrap_or(false);
        if expired {
            let _ = fs::remove_file(entry.path());
        }
    }
}
