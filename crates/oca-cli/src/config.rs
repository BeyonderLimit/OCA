//! Layered configuration (Phase 0 deliverable).
//!
//! Precedence: defaults → `~/.oca/config.toml` → `<project>/.oca/config.toml`
//! → CLI flags. Example:
//!
//! ```toml
//! log_level = "info"   # error|warn|info|debug|trace
//! log_content = false  # never log project text unless explicitly enabled (A7)
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::Deserialize;

/// Valid `log_level` values.
pub const LEVELS: [&str; 5] = ["error", "warn", "info", "debug", "trace"];

/// Shape of a config file layer.
#[derive(Debug, Default, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    pub log_level: Option<String>,
    pub log_content: Option<bool>,
}

/// Fully merged configuration plus which layer files were read (for `doctor`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Config {
    pub log_level: Option<String>,
    pub log_content: bool,
    pub sources: Vec<PathBuf>,
}

impl Config {
    /// Load and merge layers in order; later layers win. Malformed or unknown
    /// keys are hard errors — a typo'd config should not be silently ignored.
    pub fn load(global: Option<&Path>, project: Option<&Path>) -> anyhow::Result<Self> {
        let mut cfg = Config::default();
        for path in [global, project].into_iter().flatten() {
            if !path.is_file() {
                continue;
            }
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading config {}", path.display()))?;
            let layer: FileConfig = toml::from_str(&text)
                .with_context(|| format!("parsing config {}", path.display()))?;
            if let Some(level) = layer.log_level {
                if !LEVELS.contains(&level.as_str()) {
                    bail!(
                        "{}: invalid log_level {level:?} (expected one of {})",
                        path.display(),
                        LEVELS.join(", ")
                    );
                }
                cfg.log_level = Some(level);
            }
            if let Some(flag) = layer.log_content {
                cfg.log_content = flag;
            }
            cfg.sources.push(path.to_path_buf());
        }
        Ok(cfg)
    }

    /// Human-readable source summary for `doctor`.
    pub fn sources_detail(&self) -> String {
        if self.sources.is_empty() {
            "defaults (no config files found)".to_string()
        } else {
            self.sources
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        }
    }
}

/// Phase 0 project detection: a directory containing `project.yaml` is a project.
/// Phase 1 replaces this with `oca open` session state.
pub fn detect_project_dir(cwd: Option<&Path>) -> Option<PathBuf> {
    let cwd = cwd?;
    if cwd.join("project.yaml").is_file() {
        Some(cwd.to_path_buf())
    } else {
        None
    }
}

/// Home directory via environment (test-friendly; no deprecation pitfalls).
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

pub fn global_config_path(home: &Path) -> PathBuf {
    home.join(".oca").join("config.toml")
}

pub fn project_config_path(project: &Path) -> PathBuf {
    project.join(".oca").join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn defaults_when_no_files_exist() {
        let cfg = Config::load(None, None).unwrap();
        assert_eq!(cfg.log_level, None);
        assert!(!cfg.log_content);
        assert!(cfg.sources.is_empty());
    }

    #[test]
    fn global_layer_applied() {
        let dir = TempDir::new().unwrap();
        let global = dir.path().join("global.toml");
        write(&global, "log_level = \"debug\"\n");
        let cfg = Config::load(Some(&global), None).unwrap();
        assert_eq!(cfg.log_level.as_deref(), Some("debug"));
        assert_eq!(cfg.sources.len(), 1);
    }

    #[test]
    fn project_layer_overrides_global() {
        let dir = TempDir::new().unwrap();
        let global = dir.path().join("global.toml");
        let project = dir.path().join("project.toml");
        write(&global, "log_level = \"debug\"\nlog_content = false\n");
        write(&project, "log_level = \"trace\"\nlog_content = true\n");
        let cfg = Config::load(Some(&global), Some(&project)).unwrap();
        assert_eq!(cfg.log_level.as_deref(), Some("trace"));
        assert!(cfg.log_content);
        assert_eq!(cfg.sources.len(), 2);
    }

    #[test]
    fn partial_project_layer_keeps_global_values() {
        let dir = TempDir::new().unwrap();
        let global = dir.path().join("global.toml");
        let project = dir.path().join("project.toml");
        write(&global, "log_level = \"info\"\n");
        write(&project, "log_content = true\n");
        let cfg = Config::load(Some(&global), Some(&project)).unwrap();
        assert_eq!(cfg.log_level.as_deref(), Some("info"));
        assert!(cfg.log_content);
    }

    #[test]
    fn invalid_log_level_is_an_error() {
        let dir = TempDir::new().unwrap();
        let global = dir.path().join("global.toml");
        write(&global, "log_level = \"loud\"\n");
        let err = Config::load(Some(&global), None).unwrap_err();
        assert!(err.to_string().contains("invalid log_level"));
    }

    #[test]
    fn unknown_key_is_an_error() {
        let dir = TempDir::new().unwrap();
        let global = dir.path().join("global.toml");
        write(&global, "logg_level = \"info\"\n");
        let err = Config::load(Some(&global), None).unwrap_err();
        assert!(format!("{err:#}").contains("logg_level"));
    }

    #[test]
    fn project_detection_requires_project_yaml() {
        let dir = TempDir::new().unwrap();
        assert_eq!(detect_project_dir(Some(dir.path())), None);
        std::fs::write(dir.path().join("project.yaml"), "project_id: p1\n").unwrap();
        assert_eq!(
            detect_project_dir(Some(dir.path())),
            Some(dir.path().to_path_buf())
        );
        assert_eq!(detect_project_dir(None), None);
    }
}
