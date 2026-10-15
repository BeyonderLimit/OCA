//! `~/.oca/projects.toml` — the simple list of known projects (D3: simple
//! files, no database for user state).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::atomic::atomic_write;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct RegistryFile {
    #[serde(default)]
    projects: Vec<Entry>,
}

#[derive(Clone, Debug, Default)]
pub struct Registry {
    file: RegistryFile,
}

impl Registry {
    /// Load the registry from `<home>/.oca/projects.toml` (missing file is an
    /// empty registry; corrupt file is an error, never silently dropped).
    pub fn load(home: &Path) -> Result<Self> {
        let path = registry_path(home);
        if !path.is_file() {
            return Ok(Self::default());
        }
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let file: RegistryFile =
            toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        Ok(Self { file })
    }

    pub fn entries(&self) -> &[Entry] {
        &self.file.projects
    }

    /// Insert or update by name (path normalized to absolute).
    pub fn upsert(&mut self, name: &str, path: &Path) -> Result<()> {
        let abs = std::fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .replace('\\', "/");
        match self.file.projects.iter_mut().find(|e| e.name == name) {
            Some(existing) => existing.path = abs,
            None => self.file.projects.push(Entry {
                name: name.to_string(),
                path: abs,
            }),
        }
        Ok(())
    }

    pub fn remove(&mut self, name: &str) {
        self.file.projects.retain(|e| e.name != name);
    }

    pub fn save(&self, home: &Path) -> Result<()> {
        let path = registry_path(home);
        let text = toml::to_string_pretty(&self.file).context("serialize registry")?;
        atomic_write(&path, text.as_bytes())
    }
}

pub fn registry_path(home: &Path) -> PathBuf {
    home.join(".oca/projects.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn upsert_save_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let mut reg = Registry::load(tmp.path()).unwrap();
        assert!(reg.entries().is_empty());

        reg.upsert("novel", tmp.path()).unwrap();
        reg.upsert("short", tmp.path()).unwrap();
        reg.upsert("novel", tmp.path()).unwrap(); // dedupe by name
        assert_eq!(reg.entries().len(), 2);
        reg.save(tmp.path()).unwrap();

        let re = Registry::load(tmp.path()).unwrap();
        assert_eq!(re.entries().len(), 2);
        assert_eq!(re.entries()[0].name, "novel");
    }

    #[test]
    fn remove_entry() {
        let tmp = TempDir::new().unwrap();
        let mut reg = Registry::load(tmp.path()).unwrap();
        reg.upsert("a", tmp.path()).unwrap();
        reg.remove("a");
        assert!(reg.entries().is_empty());
    }
}
