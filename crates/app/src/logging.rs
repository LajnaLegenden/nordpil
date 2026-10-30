//! Logs to stderr and to a daily-rotated file in the platform state directory.

use std::path::PathBuf;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

const DEFAULT_FILTER: &str = "warn,nordpil=info,nordpil_backend=info,nordpil_query=info";
const KEEP_LOG_FILES: usize = 7;

/// Starts logging. `RUST_LOG` overrides the default filter. Keep the returned
/// guard alive until exit, or buffered log lines are lost.
pub fn init() -> Option<WorkerGuard> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let dir = log_dir();
    let appender = dir.as_ref().and_then(|dir| {
        RollingFileAppender::builder()
            .rotation(Rotation::DAILY)
            .filename_prefix("nordpil")
            .filename_suffix("log")
            .max_log_files(KEEP_LOG_FILES)
            .build(dir)
            .ok()
    });
    let (file_layer, guard) = match appender {
        Some(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let layer = fmt::layer().with_ansi(false).with_writer(writer);
            (Some(layer), Some(guard))
        }
        None => (None, None),
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(std::io::stderr))
        .with(file_layer)
        .init();

    if guard.is_none() {
        tracing::warn!(?dir, "could not open log directory, logging to stderr only");
    }
    guard
}

fn log_dir() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "nordpil")?;
    let base = dirs.state_dir().unwrap_or(dirs.data_local_dir());
    Some(base.join("logs"))
}
