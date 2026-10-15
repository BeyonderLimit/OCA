//! Project document index: an in-memory id→path map scanned from frontmatter
//! (A2: links resolve by id, never path — so a file renamed on disk still
//! resolves after a rescan).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::layout::{COMMENT_SUFFIX, validate_rel};
use oca_markdown::frontmatter_of;
use oca_model::DocumentKind;

/// One document discovered on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocEntry {
    /// Project-relative path (always `/`-separated via `PathBuf` on top dir).
    pub rel: PathBuf,
    /// Frontmatter `id`, when present.
    pub id: Option<String>,
    /// Frontmatter `title`, falling back to the filename stem.
    pub title: String,
    /// Leading number of chapter-style filenames (`001-…` → 1).
    pub seq: Option<u32>,
}

impl DocEntry {
    /// First path component (`manuscript`, `characters`, …) — tree group key.
    pub fn group(&self) -> &str {
        self.rel
            .components()
            .next()
            .and_then(|c| c.as_os_str().to_str())
            .unwrap_or("other")
    }
}

/// Every Markdown document under `root`, sorted for stable tree display:
/// group → numeric filename order → name. Skips `.oca`, hidden directories,
/// and AI comment sidecars.
pub fn scan(root: &Path) -> Result<Vec<DocEntry>> {
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    out.sort_by(|a, b| {
        let seq_key = |e: &DocEntry| (e.group().to_string(), e.seq, e.rel.clone());
        seq_key(a).cmp(&seq_key(b))
    });
    Ok(out)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<DocEntry>) -> Result<()> {
    let entries = std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name.starts_with('.') {
                continue; // .oca and any hidden dir
            }
            walk(root, &path, out)?;
            continue;
        }
        if !name.ends_with(".md") || name.ends_with(COMMENT_SUFFIX) {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .context("scan produced a path outside the project")?;
        if validate_rel(rel).is_err() {
            continue;
        }
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let fm = frontmatter_of(&raw);
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        out.push(DocEntry {
            rel: rel.to_path_buf(),
            id: fm.get_str("id"),
            title: fm
                .get_str("title")
                .filter(|t| !t.is_empty())
                .unwrap_or(stem.clone()),
            seq: leading_number(&stem),
        });
    }
    Ok(())
}

/// `001-the-burden` → 1.
pub fn leading_number(stem: &str) -> Option<u32> {
    let digits: String = stem.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

/// Index maps for `Project`.
#[derive(Default)]
pub struct Index {
    pub entries: Vec<DocEntry>,
    by_id: HashMap<String, usize>,
    by_title: HashMap<String, usize>,
}

impl Index {
    pub fn rebuild(&mut self, entries: Vec<DocEntry>) {
        self.by_id.clear();
        self.by_title.clear();
        for (i, e) in entries.iter().enumerate() {
            if let Some(id) = &e.id {
                self.by_id.insert(id.clone(), i);
            }
            self.by_title.entry(e.title.to_lowercase()).or_insert(i);
        }
        self.entries = entries;
    }

    pub fn by_id(&self, id: &str) -> Option<&DocEntry> {
        self.by_id.get(id).map(|&i| &self.entries[i])
    }

    pub fn by_title(&self, title: &str) -> Option<&DocEntry> {
        self.by_title
            .get(&title.to_lowercase())
            .map(|&i| &self.entries[i])
    }

    pub fn path_of(&self, id: &str) -> Option<PathBuf> {
        self.by_id(id).map(|e| e.rel.clone())
    }

    /// Document kind directory → entries (for the TUI project tree).
    pub fn groups(&self) -> Vec<(&'static str, Vec<&DocEntry>)> {
        let order: [&str; 7] = [
            "manuscript",
            "characters",
            "world",
            "outline",
            "music",
            "notes",
            "audio",
        ];
        let mut groups: Vec<(&'static str, Vec<&DocEntry>)> = Vec::new();
        for dir in order {
            let members: Vec<&DocEntry> =
                self.entries.iter().filter(|e| e.group() == dir).collect();
            if !members.is_empty() {
                groups.push((dir, members));
            }
        }
        groups
    }
}

/// Next free chapter number in `manuscript/` (1-based, zero-padded to 3).
pub fn next_chapter_seq(root: &Path) -> u32 {
    let dir = root.join(DocumentKind::Chapter.dir());
    let mut max = 0u32;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let stem = e
                .path()
                .file_stem()
                .map(|s| s.to_string_lossy().to_string());
            if let Some(stem) = stem
                && let Some(n) = leading_number(&stem)
            {
                max = max.max(n);
            }
        }
    }
    max + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::scaffold;
    use tempfile::TempDir;

    #[test]
    fn scan_indexes_ids_titles_and_skips_comments_and_hidden() {
        let tmp = TempDir::new().unwrap();
        scaffold(tmp.path()).unwrap();
        std::fs::write(
            tmp.path().join("manuscript/001-start.md"),
            "---\nid: ch_1\ntitle: Start\n---\nbody\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("manuscript/002-next.md"),
            "---\nid: ch_2\n---\nno title\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("characters/rin.md"),
            "---\nid: char_9\ntitle: Rin\n---\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("comments/001-start.comment.md"),
            "---\nid: should_not_appear\n---\n",
        )
        .unwrap();
        std::fs::write(tmp.path().join(".oca/cache/ghost.md"), "id: nope").unwrap();

        let idx = scan(tmp.path()).unwrap();
        let ids: Vec<_> = idx.iter().filter_map(|e| e.id.as_deref()).collect();
        assert_eq!(ids, vec!["char_9", "ch_1", "ch_2"]);
        // title fallback to filename stem
        assert_eq!(
            idx.iter()
                .find(|e| e.id.as_deref() == Some("ch_2"))
                .unwrap()
                .title,
            "002-next"
        );

        let mut index = Index::default();
        index.rebuild(idx);
        assert_eq!(
            index.path_of("ch_1").unwrap(),
            PathBuf::from("manuscript/001-start.md")
        );
        assert!(index.path_of("should_not_appear").is_none());
        assert!(index.by_title("rin").is_some());
        assert_eq!(index.groups()[0].0, "manuscript");
    }

    #[test]
    fn chapter_sequence_skips_gaps() {
        let tmp = TempDir::new().unwrap();
        scaffold(tmp.path()).unwrap();
        std::fs::write(tmp.path().join("manuscript/003-x.md"), "x").unwrap();
        std::fs::write(tmp.path().join("manuscript/010-y.md"), "y").unwrap();
        assert_eq!(next_chapter_seq(tmp.path()), 11);
        assert_eq!(leading_number("007-title"), Some(7));
        assert_eq!(leading_number("title"), None);
    }
}
