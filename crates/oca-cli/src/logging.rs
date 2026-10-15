//! Tracing setup (Phase 0 deliverable).
//!
//! Two sinks: stderr (level driven by `-v` / config, default `warn`) and a
//! daily-rolled file under `<project>/.oca/logs/` — or `~/.oca/logs/` when no
//! project is open — with an `info` floor so the file is useful while the
//! console stays quiet.
//!
//! Contract (A7): logs never contain project content unless `--log-content`
//! is passed; no telemetry, ever. Nothing in Phase 0 logs project content.

use std::path::{Path, PathBuf};

use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::prelude::*;

use crate::config::Config;

pub const DEFAULT_STDERR_LEVEL: &str = "warn";
pub const FILE_LEVEL_FLOOR: &str = "info";
pub const LOG_FILE_NAME: &str = "oca.log";

/// Result of setting up logging; kept so `doctor` can report the log file path.
pub struct Logging {
    pub stderr_level: String,
    /// Daily-rolled log file: base path `…/oca.log` (actual files are `oca.log.YYYY-MM-DD`).
    pub file: Result<PathBuf, String>,
    pub(crate) _guard: Option<tracing_appender::non_blocking::WorkerGuard>,
}

/// Map a `-v` count to a level; `0` means "unset" (fall back to config/default).
pub fn level_for_verbose(verbose: u8) -> Option<&'static str> {
    match verbose {
        0 => None,
        1 => Some("info"),
        2 => Some("debug"),
        _ => Some("trace"),
    }
}

fn rank(level: &str) -> u8 {
    match level {
        "error" => 0,
        "warn" => 1,
        "info" => 2,
        "debug" => 3,
        "trace" => 4,
        _ => 1,
    }
}

/// File level: at least `info` so the log file captures normal operation even
/// when the console is at `warn`.
pub fn file_level_for(stderr_level: &str) -> &str {
    if rank(stderr_level) < rank(FILE_LEVEL_FLOOR) {
        FILE_LEVEL_FLOOR
    } else {
        stderr_level
    }
}

/// Initialize global tracing. Never panics: if the file sink cannot be set up
/// the error is recorded and reported by `doctor`.
pub fn init(
    verbose: u8,
    log_content: bool,
    cfg: &Config,
    project_dir: Option<&Path>,
    home: Option<&Path>,
) -> Logging {
    let _ = log_content; // gate for future content logging (A7); no content-logging code exists yet.

    let stderr_level = level_for_verbose(verbose)
        .map(str::to_owned)
        .or_else(|| cfg.log_level.clone())
        .unwrap_or_else(|| DEFAULT_STDERR_LEVEL.to_owned());
    let file_level = file_level_for(&stderr_level);

    let mut logging = Logging {
        stderr_level: stderr_level.clone(),
        file: Err("no log directory available (no project, no home)".into()),
        _guard: None,
    };

    let stderr_filter = match std::env::var("RUST_LOG") {
        Ok(v) if !v.is_empty() => v,
        _ => stderr_level.clone(),
    };
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(EnvFilter::builder().parse_lossy(stderr_filter));

    let log_dir: Option<PathBuf> = project_dir
        .map(|p| p.join(".oca").join("logs"))
        .or_else(|| home.map(|h| h.join(".oca").join("logs")));

    let file_layer = log_dir
        .as_ref()
        .and_then(|dir| match std::fs::create_dir_all(dir) {
            Ok(()) => {
                let appender = tracing_appender::rolling::daily(dir, LOG_FILE_NAME);
                let (non_blocking, guard) = tracing_appender::non_blocking(appender);
                let layer = tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(non_blocking)
                    .with_filter(EnvFilter::new(file_level));
                logging._guard = Some(guard);
                Some(layer)
            }
            Err(err) => {
                logging.file = Err(format!("{}: {err}", dir.display()));
                None
            }
        });

    match (log_dir, file_layer) {
        (Some(dir), Some(layer)) => {
            tracing_subscriber::registry()
                .with(stderr_layer)
                .with(layer)
                .init();
            logging.file = Ok(dir.join(LOG_FILE_NAME));
        }
        _ => tracing_subscriber::registry().with(stderr_layer).init(),
    }

    tracing::debug!(
        stderr_level = %logging.stderr_level,
        file_level,
        "logging initialized"
    );
    logging
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbose_maps_to_levels() {
        assert_eq!(level_for_verbose(0), None);
        assert_eq!(level_for_verbose(1), Some("info"));
        assert_eq!(level_for_verbose(2), Some("debug"));
        assert_eq!(level_for_verbose(3), Some("trace"));
        assert_eq!(level_for_verbose(9), Some("trace"));
    }

    #[test]
    fn file_level_has_info_floor() {
        assert_eq!(file_level_for("error"), "info");
        assert_eq!(file_level_for("warn"), "info");
        assert_eq!(file_level_for("info"), "info");
        assert_eq!(file_level_for("debug"), "debug");
        assert_eq!(file_level_for("trace"), "trace");
    }
}
