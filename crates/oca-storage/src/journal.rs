//! Recovery journal (A3): unsaved editor buffers live under
//! `.oca/state/recovery/` so a crash loses at most the last journal
//! interval — never the writing session. `.oca/state/` is precious
//! (AGENTS Rule 2): we only ever add/update files here.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::atomic::atomic_write;
use crate::layout::validate_rel;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryEntry {
    /// Project-relative document path the draft belongs to.
    pub rel_path: String,
    pub content: String,
    /// Unix seconds when the draft was journaled.
    pub saved_unix: u64,
}

fn journal_path(root: &Path, rel: &Path) -> Result<PathBuf> {
    validate_rel(rel)?;
    let key = rel
        .to_string_lossy()
        .trim_end_matches(".md")
        .replace(['/', '\\', ':'], "_");
    let key = if key.is_empty() {
        "doc".to_string()
    } else {
        key
    };
    Ok(root.join(".oca/state/recovery").join(format!("{key}.json")))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Journal (or refresh) the unsaved draft for `rel`.
pub fn write_recovery(root: &Path, rel: &Path, content: &str) -> Result<()> {
    let entry = RecoveryEntry {
        rel_path: rel.to_string_lossy().replace('\\', "/"),
        content: content.to_string(),
        saved_unix: now_unix(),
    };
    let path = journal_path(root, rel)?;
    let bytes = serde_json::to_vec_pretty(&entry).context("serialize recovery entry")?;
    atomic_write(&path, &bytes)
}

/// The journaled draft for `rel`, if one exists.
pub fn recovery_for(root: &Path, rel: &Path) -> Result<Option<RecoveryEntry>> {
    let path = journal_path(root, rel)?;
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let entry: RecoveryEntry = serde_json::from_slice(&bytes).context("parse recovery entry")?;
    Ok(Some(entry))
}

/// Drop the journal for `rel` (after a successful save or an explicit
/// discard).
pub fn clear_recovery(root: &Path, rel: &Path) -> Result<()> {
    let path = journal_path(root, rel)?;
    if path.is_file() {
        fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
    }
    Ok(())
}

/// Every journaled draft in the project (for the restore prompt on open).
pub fn list_recovery(root: &Path) -> Result<Vec<RecoveryEntry>> {
    let dir = root.join(".oca/state/recovery");
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    for entry in fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        if entry.path().extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let bytes = fs::read(entry.path())?;
        match serde_json::from_slice::<RecoveryEntry>(&bytes) {
            Ok(e) => out.push(e),
            Err(_) => continue, // a torn journal from a hard kill: skip it
        }
    }
    out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::scaffold;
    use tempfile::TempDir;

    #[test]
    fn roundtrip_write_read_clear() {
        let tmp = TempDir::new().unwrap();
        scaffold(tmp.path()).unwrap();
        let rel = Path::new("manuscript/001-x.md");

        write_recovery(tmp.path(), rel, "draft body").unwrap();
        let got = recovery_for(tmp.path(), rel).unwrap().expect("entry");
        assert_eq!(got.content, "draft body");
        assert_eq!(got.rel_path, "manuscript/001-x.md");
        assert!(got.saved_unix > 0);

        let all = list_recovery(tmp.path()).unwrap();
        assert_eq!(all.len(), 1);

        clear_recovery(tmp.path(), rel).unwrap();
        assert!(recovery_for(tmp.path(), rel).unwrap().is_none());
        assert!(list_recovery(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn journals_from_different_dirs_do_not_collide() {
        let tmp = TempDir::new().unwrap();
        scaffold(tmp.path()).unwrap();
        write_recovery(tmp.path(), Path::new("notes/a.md"), "A").unwrap();
        write_recovery(tmp.path(), Path::new("manuscript/a.md"), "M").unwrap();
        assert_eq!(
            recovery_for(tmp.path(), Path::new("notes/a.md"))
                .unwrap()
                .unwrap()
                .content,
            "A"
        );
        assert_eq!(
            recovery_for(tmp.path(), Path::new("manuscript/a.md"))
                .unwrap()
                .unwrap()
                .content,
            "M"
        );
        assert_eq!(list_recovery(tmp.path()).unwrap().len(), 2);
    }

    #[test]
    fn rejects_paths_outside_project() {
        let tmp = TempDir::new().unwrap();
        scaffold(tmp.path()).unwrap();
        assert!(write_recovery(tmp.path(), Path::new("../x.md"), "no").is_err());
    }
}
