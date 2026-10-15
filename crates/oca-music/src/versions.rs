//! "Save as version" (A11 G12): an author-named draft of one item, stored
//! as visible files under the song's `versions/` directory. Snapshots
//! (automatic, whole project) are a different mechanism — compare and
//! promote arrive in Phase 6; Phase 3 saves and lists only.
//!
//! Layout: `versions/v01.md`, `v02.md`, … byte-identical copies of
//! `lyrics.md`; optional labels live in `versions/index.yaml` so the
//! copies stay pristine for later diffs.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One saved version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionRef {
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct IndexFile {
    #[serde(default)]
    versions: Vec<VersionRef>,
}

/// `v01.md` for 1, `v12.md` for 12.
pub fn version_filename(n: u32) -> String {
    format!("v{n:02}.md")
}

/// Next version number for a song directory (max existing `vNN.md` + 1).
pub fn next_version_number(song_dir: &Path) -> Result<u32> {
    let dir = song_dir.join("versions");
    let mut max = 0u32;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(stem) = name.strip_prefix('v').and_then(|s| s.strip_suffix(".md"))
                && let Ok(n) = stem.parse::<u32>()
            {
                max = max.max(n);
            }
        }
    }
    Ok(max + 1)
}

/// Parse `versions/index.yaml` (missing file = empty index).
pub fn read_index(song_dir: &Path) -> Result<Vec<VersionRef>> {
    let path = song_dir.join("versions").join("index.yaml");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let parsed: IndexFile =
        serde_yaml::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    Ok(parsed.versions)
}

/// Every saved version file on disk, in number order, with labels from
/// `index.yaml` overlaid. Unlabeled versions come back with `label: None`.
pub fn list(song_dir: &Path) -> Result<Vec<VersionRef>> {
    let labels = read_index(song_dir)?;
    let dir = song_dir.join("versions");
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(stem) = name.strip_prefix('v').and_then(|s| s.strip_suffix(".md")) else {
                continue;
            };
            if stem.parse::<u32>().is_err() {
                continue;
            }
            let label = labels
                .iter()
                .find(|v| v.file == name)
                .and_then(|v| v.label.clone());
            out.push(VersionRef { file: name, label });
        }
    }
    out.sort_by(|a, b| a.file.cmp(&b.file));
    Ok(out)
}

/// Render the index file body from its entries.
pub fn render_index(versions: &[VersionRef]) -> Result<String> {
    serde_yaml::to_string(&IndexFile {
        versions: versions.to_vec(),
    })
    .context("render versions/index.yaml")
}

/// Insert (or replace) the label for a saved file, returning the new index.
pub fn upsert_label(index: &[VersionRef], file: &str, label: &str) -> Vec<VersionRef> {
    let mut out = index.to_vec();
    match out.iter_mut().find(|v| v.file == file) {
        Some(v) => v.label = Some(label.to_string()),
        None => out.push(VersionRef {
            file: file.to_string(),
            label: Some(label.to_string()),
        }),
    }
    out
}

/// Rel paths (project-relative) the facade writes for a save.
pub fn save_paths(song_dir_rel: &Path, n: u32) -> (PathBuf, PathBuf) {
    (
        song_dir_rel.join("versions").join(version_filename(n)),
        song_dir_rel.join("versions").join("index.yaml"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn numbering_skips_gaps_and_restarts_after_max() {
        let tmp = TempDir::new().unwrap();
        let song = tmp.path().join("music/songs/ashes");
        std::fs::create_dir_all(song.join("versions")).unwrap();
        assert_eq!(next_version_number(&song).unwrap(), 1);
        std::fs::write(song.join("versions/v01.md"), "a").unwrap();
        std::fs::write(song.join("versions/v07.md"), "b").unwrap();
        assert_eq!(next_version_number(&song).unwrap(), 8);
    }

    #[test]
    fn index_round_trips_labels() {
        let tmp = TempDir::new().unwrap();
        let song = tmp.path().join("s");
        let idx = upsert_label(&[], "v01.md", "before the rewrite");
        let idx = upsert_label(&idx, "v02.md", "chorus pass");
        let raw = render_index(&idx).unwrap();
        std::fs::create_dir_all(song.join("versions")).unwrap();
        std::fs::write(song.join("versions/index.yaml"), &raw).unwrap();
        let back = read_index(&song).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].label.as_deref(), Some("before the rewrite"));
        assert_eq!(back[1].file, "v02.md");
    }

    #[test]
    fn missing_index_is_empty() {
        let tmp = TempDir::new().unwrap();
        assert!(read_index(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn version_filenames_are_two_digit() {
        assert_eq!(version_filename(1), "v01.md");
        assert_eq!(version_filename(12), "v12.md");
        assert_eq!(
            save_paths(Path::new("music/songs/ashes"), 3),
            (
                PathBuf::from("music/songs/ashes/versions/v03.md"),
                PathBuf::from("music/songs/ashes/versions/index.yaml")
            )
        );
    }
}
