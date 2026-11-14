use std::io::IsTerminal as _;

use anyhow::{Context as _, Result};
use purge_config::Paths;
use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{Builder, Rotation},
};
use tracing_subscriber::{
    EnvFilter, Layer as _, layer::SubscriberExt as _, util::SubscriberInitExt as _,
};

/// Daily log files kept before the oldest is deleted.
const KEPT_LOG_FILES: usize = 14;

/// Logs to a daily-rotated file in the data directory, and to stderr when
/// `to_stderr`. Keep the guard alive for as long as logging is needed.
pub fn init_logging(paths: &Paths, to_stderr: bool) -> Result<WorkerGuard> {
    let dir = paths.log_dir();
    // The appender lists this directory to prune old logs; it must exist.
    std::fs::create_dir_all(&dir).context("cannot create log directory")?;
    let appender = Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix("purge")
        .filename_suffix("log")
        .max_log_files(KEPT_LOG_FILES)
        .build(&dir)
        .context("cannot open log file")?;
    let (file, guard) = tracing_appender::non_blocking(appender);
    let filter = || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(file)
                .with_ansi(false)
                .with_filter(filter()),
        )
        .with(to_stderr.then(|| {
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(std::io::stderr().is_terminal())
                .with_filter(filter())
        }))
        .init();
    Ok(guard)
}
