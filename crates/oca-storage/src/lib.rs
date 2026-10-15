//! OCA filesystem storage: project lifecycle, safe writes, single-writer
//! lock, recovery journal, and the id→path index.
//!
//! Layering: this crate owns *bytes on disk*; policy (read-only gates,
//! events) is enforced one level up in `oca-core`.

pub mod atomic;
pub mod index;
pub mod journal;
pub mod layout;
pub mod lock;
pub mod registry;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::atomic::atomic_write;
use crate::index::{DocEntry, Index, next_chapter_seq};
use crate::layout::{PROJECT_FILE, slugify, validate_rel, write_new};
use crate::lock::Lock;
use oca_markdown::{frontmatter_of, parse};
use oca_model::{DocumentKind, ProjectKind, ProjectMeta};

/// Why a project is open read-only, if it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadOnly {
    /// Another session holds the single-writer lock.
    Locked,
    /// `format_version` is newer than this build understands.
    FutureFormat { found: u64, supported: u64 },
}

impl ReadOnly {
    pub fn reason(&self) -> String {
        match self {
            ReadOnly::Locked => {
                "another session is editing this project (single-writer lock held)".into()
            }
            ReadOnly::FutureFormat { found, supported } => format!(
                "project format_version {found} is newer than this build supports ({supported})"
            ),
        }
    }
}

/// An open project: root + metadata + lock + document index.
pub struct Project {
    root: PathBuf,
    meta: ProjectMeta,
    _lock: Option<Lock>,
    read_only: Option<ReadOnly>,
    index: Index,
}

impl Project {
    /// Create a brand-new project directory tree and `project.yaml`.
    pub fn create(root: &Path, title: &str, kind: ProjectKind) -> Result<Project> {
        if root.join(PROJECT_FILE).exists() {
            anyhow::bail!("project already exists: {}", root.display());
        }
        layout::scaffold(root)?;
        let meta = ProjectMeta::new_template(title, kind)?;
        write_new(&root.join(PROJECT_FILE), meta.raw().as_bytes())?;
        Project::open(root)
    }

    /// Open an existing project. Never fails on a held lock or a future
    /// format — the project opens *read-only* with a reason (A3/A1).
    pub fn open(root: &Path) -> Result<Project> {
        let root = root.to_path_buf();
        let yaml_path = root.join(PROJECT_FILE);
        let raw = std::fs::read_to_string(&yaml_path)
            .with_context(|| format!("read {}", yaml_path.display()))?;
        let meta = ProjectMeta::parse(&raw)?;

        let lock = Lock::acquire(&root)?;
        let read_only = if !meta.is_writable_format() {
            Some(ReadOnly::FutureFormat {
                found: meta.format_version(),
                supported: oca_model::SUPPORTED_FORMAT_VERSION,
            })
        } else if lock.is_none() {
            Some(ReadOnly::Locked)
        } else {
            None
        };

        let entries = index::scan(&root)?;
        let mut index = Index::default();
        index.rebuild(entries);
        Ok(Project {
            root,
            meta,
            _lock: lock,
            read_only,
            index,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn meta(&self) -> &ProjectMeta {
        &self.meta
    }

    pub fn read_only(&self) -> bool {
        self.read_only.is_some()
    }

    pub fn read_only_reason(&self) -> Option<String> {
        self.read_only.map(|r| r.reason())
    }

    pub fn documents(&self) -> &[DocEntry] {
        &self.index.entries
    }

    pub fn groups(&self) -> Vec<(&'static str, Vec<&DocEntry>)> {
        self.index.groups()
    }

    fn ensure_writable(&self) -> Result<()> {
        match &self.read_only {
            None => Ok(()),
            Some(r) => anyhow::bail!("project is read-only: {}", r.reason()),
        }
    }

    /// Resolve `id` (preferred) or exact title to a project-relative path.
    /// A missing or vanished hit triggers one rescan — a file renamed on disk
    /// still resolves (A2 exit criterion: external moves never break links).
    pub fn resolve(&mut self, key: &str) -> Result<PathBuf> {
        if let Some(rel) = self.lookup(key) {
            return Ok(rel);
        }
        self.refresh_index()?;
        if let Some(rel) = self.lookup(key) {
            return Ok(rel);
        }
        // A literal project-relative path also resolves (typing paths in CLI).
        let direct = PathBuf::from(key);
        validate_rel(&direct)?;
        if direct.extension().and_then(|e| e.to_str()) == Some("md")
            && self.root.join(&direct).is_file()
        {
            return Ok(direct);
        }
        anyhow::bail!("no document with id or title: {key}")
    }

    fn lookup(&self, key: &str) -> Option<PathBuf> {
        let rel = self
            .index
            .path_of(key)
            .or_else(|| self.index.by_title(key).map(|e| e.rel.clone()))?;
        // Trust the index only while its file is still there.
        self.root.join(&rel).is_file().then_some(rel)
    }

    pub fn refresh_index(&mut self) -> Result<()> {
        let entries = index::scan(&self.root)?;
        self.index.rebuild(entries);
        Ok(())
    }

    /// Read + parse one document (project-relative path).
    pub fn read_document(&self, rel: &Path) -> Result<oca_model::Document> {
        validate_rel(rel)?;
        let path = self.root.join(rel);
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        Ok(parse(&raw))
    }

    /// Create a document of `kind` with title; returns its relative path.
    /// Chapters get `NNN-slug.md` names and an H1 body; others `slug.md`.
    pub fn create_document(&mut self, kind: DocumentKind, title: &str) -> Result<PathBuf> {
        self.ensure_writable()?;
        let dir = self.root.join(kind.dir());
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;

        let slug = slugify(title);
        let rel = loop {
            let candidate = if kind == DocumentKind::Chapter {
                let seq = next_chapter_seq(&self.root);
                PathBuf::from(kind.dir()).join(format!("{seq:03}-{slug}.md"))
            } else {
                PathBuf::from(kind.dir()).join(format!("{slug}.md"))
            };
            if !self.root.join(&candidate).exists() {
                break candidate;
            }
            if kind == DocumentKind::Chapter {
                continue; // next seq
            }
            anyhow::bail!("a document named {} already exists", candidate.display());
        };

        let fm = self.meta.document_frontmatter(kind, title)?;
        let mut raw = fm.raw().unwrap_or_default().to_string();
        if kind == DocumentKind::Chapter {
            raw.push_str(&format!("# {title}\n"));
        }
        write_new(&self.root.join(&rel), raw.as_bytes())?;
        self.refresh_index()?;
        Ok(rel)
    }

    /// Save full file bytes for `rel` (atomic; frontmatter untouched unless
    /// the caller changed it). Creating a *new* file refreshes the index so
    /// it appears in `documents()` immediately (existing files keep the
    /// cached index — the TUI's hot save path must not rescan the tree).
    pub fn save_document(&mut self, rel: &Path, raw: &str) -> Result<()> {
        self.ensure_writable()?;
        validate_rel(rel)?;
        let existed = self.root.join(rel).exists();
        atomic_write(&self.root.join(rel), raw.as_bytes())?;
        if !existed {
            self.refresh_index()?;
        }
        Ok(())
    }

    /// Append one paragraph to `rel`; returns the new raw bytes.
    pub fn append_paragraph(&mut self, rel: &Path, text: &str) -> Result<String> {
        let doc = self.read_document(rel)?;
        let new_raw = oca_markdown::append_paragraph(&doc.raw, text);
        self.save_document(rel, &new_raw)?;
        Ok(new_raw)
    }

    /// Patch a frontmatter key on `rel`, preserving every other byte
    /// (unknown keys + comments survive edits — exit criterion). A value
    /// containing newlines is a structured YAML value (sequence/mapping
    /// fragment, e.g. a character's `states:` block); single lines are
    /// scalars.
    pub fn set_frontmatter(&mut self, rel: &Path, key: &str, value: &str) -> Result<()> {
        let mut doc = self.read_document(rel)?;
        if value.contains('\n') {
            doc.frontmatter.set_block(key, value)?;
        } else {
            doc.frontmatter.set_str(key, value)?;
        }
        let new_raw = oca_markdown::with_body(&doc, doc.body());
        self.save_document(rel, &new_raw)?;
        Ok(())
    }

    /// True when `rel` has a journaled unsaved draft newer than the file.
    pub fn recovery_for(&self, rel: &Path) -> Result<Option<journal::RecoveryEntry>> {
        journal::recovery_for(&self.root, rel)
    }

    pub fn write_recovery(&self, rel: &Path, content: &str) -> Result<()> {
        journal::write_recovery(&self.root, rel, content)
    }

    pub fn clear_recovery(&self, rel: &Path) -> Result<()> {
        journal::clear_recovery(&self.root, rel)
    }

    pub fn list_recovery(&self) -> Result<Vec<journal::RecoveryEntry>> {
        journal::list_recovery(&self.root)
    }

    /// Id/title of `rel` as currently on disk (for status lines).
    pub fn peek(&self, rel: &Path) -> Result<(Option<String>, String)> {
        validate_rel(rel)?;
        let raw = std::fs::read_to_string(self.root.join(rel))?;
        let fm = frontmatter_of(&raw);
        let stem = rel
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let title = fm
            .get_str("title")
            .filter(|t| !t.is_empty())
            .unwrap_or(stem);
        Ok((fm.get_str("id"), title))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn created_project(dir: &TempDir) -> Project {
        Project::create(dir.path(), "My Novel", ProjectKind::Novel).unwrap()
    }

    #[test]
    fn create_open_close_reopen() {
        let tmp = TempDir::new().unwrap();
        {
            let p = created_project(&tmp);
            assert!(!p.read_only());
            assert_eq!(p.meta().title(), "My Novel");
            assert!(p.meta().project_id().is_valid());
        } // lock released on drop
        let p = Project::open(tmp.path()).unwrap();
        assert!(!p.read_only(), "lock must release when session closes");
    }

    #[test]
    fn create_twice_fails() {
        let tmp = TempDir::new().unwrap();
        created_project(&tmp);
        assert!(Project::create(tmp.path(), "again", ProjectKind::Novel).is_err());
    }

    /// `save_document` on a *new* file refreshes the index (so a song's
    /// `lyrics.md` shows up in `documents()` right away); saving an existing
    /// file keeps the cached index (the hot save path must not rescan).
    #[test]
    fn save_document_refreshes_index_only_for_new_files() {
        let tmp = TempDir::new().unwrap();
        let mut p = created_project(&tmp);
        let music_rel = PathBuf::from("music/songs/ashes/lyrics.md");

        p.save_document(&music_rel, "# Ashes\n\n## Verse 1\nrain\n")
            .unwrap();
        assert!(
            p.documents().iter().any(|d| d.rel == music_rel),
            "a new file must appear in documents() without a manual reindex"
        );

        // Existing file: the index is untouched (still lists it) and the
        // bytes land on disk.
        p.save_document(&music_rel, "# Ashes\n\n## Verse 1\nrain, rain\n")
            .unwrap();
        assert!(
            p.read_document(&music_rel)
                .unwrap()
                .body()
                .contains("rain, rain")
        );
    }

    #[test]
    fn chapter_create_read_append_persist() {
        let tmp = TempDir::new().unwrap();
        let mut p = created_project(&tmp);
        let rel = p
            .create_document(DocumentKind::Chapter, "The Burden")
            .unwrap();
        assert_eq!(rel, PathBuf::from("manuscript/001-the-burden.md"));

        let doc = p.read_document(&rel).unwrap();
        assert!(doc.frontmatter.get_str("id").unwrap().starts_with("ch_"));
        assert!(doc.body().starts_with("# The Burden"));

        p.append_paragraph(&rel, "It began.").unwrap();
        drop(p);

        let p = Project::open(tmp.path()).unwrap();
        let doc = p.read_document(&rel).unwrap();
        assert!(doc.body().ends_with("It began.\n"));
        assert!(doc.body().contains("# The Burden\n\nIt began."));
    }

    #[test]
    fn two_chapters_numbered_sequentially() {
        let tmp = TempDir::new().unwrap();
        let mut p = created_project(&tmp);
        let a = p.create_document(DocumentKind::Chapter, "One").unwrap();
        let b = p.create_document(DocumentKind::Chapter, "Two").unwrap();
        assert_eq!(a, PathBuf::from("manuscript/001-one.md"));
        assert_eq!(b, PathBuf::from("manuscript/002-two.md"));
    }

    #[test]
    fn save_same_bytes_is_byte_identical() {
        let tmp = TempDir::new().unwrap();
        let mut p = created_project(&tmp);
        let rel = p.create_document(DocumentKind::Chapter, "Exact").unwrap();
        let before = std::fs::read(tmp.path().join(&rel)).unwrap();
        let doc = p.read_document(&rel).unwrap();
        p.save_document(&rel, &doc.raw).unwrap();
        let after = std::fs::read(tmp.path().join(&rel)).unwrap();
        assert_eq!(
            before, after,
            "saving unchanged content must not alter bytes"
        );
    }

    #[test]
    fn frontmatter_unknown_keys_and_comments_survive_edit() {
        let tmp = TempDir::new().unwrap();
        let mut p = created_project(&tmp);
        let rel = p.create_document(DocumentKind::Chapter, "C").unwrap();
        // Hand-edit the file to add comment + unknown keys (as a user would).
        let raw = std::fs::read_to_string(tmp.path().join(&rel))
            .unwrap()
            .replace(
                "---\n",
                "---\n# why this chapter exists\nweird_key:\n  nested: [1, 2]\n",
            );
        std::fs::write(tmp.path().join(&rel), raw).unwrap();

        p.append_paragraph(&rel, "Body text.").unwrap();
        p.set_frontmatter(&rel, "title", "Renamed").unwrap();

        let out = std::fs::read_to_string(tmp.path().join(&rel)).unwrap();
        assert!(out.contains("# why this chapter exists"));
        assert!(out.contains("weird_key:\n  nested: [1, 2]"));
        assert!(out.contains("title: Renamed"));
        assert!(out.contains("Body text."));
        let doc = p.read_document(&rel).unwrap();
        assert_eq!(doc.frontmatter.get_str("title").as_deref(), Some("Renamed"));
    }

    #[test]
    fn set_frontmatter_multiline_value_patches_block_losslessly() {
        let tmp = TempDir::new().unwrap();
        let mut p = created_project(&tmp);
        let rel = p.create_document(DocumentKind::Character, "Elena").unwrap();
        let raw = std::fs::read_to_string(tmp.path().join(&rel))
            .unwrap()
            .replace(
                "---\n",
                "---\n# she is the protagonist\nrole: protagonist\n",
            );
        std::fs::write(tmp.path().join(&rel), raw).unwrap();

        // A multi-line value is a structured YAML fragment (states block).
        p.set_frontmatter(
            &rel,
            "states",
            "- at: ch_1\n  location: village\n- at: ch_4\n  location: blackwood",
        )
        .unwrap();

        let out = std::fs::read_to_string(tmp.path().join(&rel)).unwrap();
        assert!(out.contains("# she is the protagonist"));
        assert!(out.contains("role: protagonist"));
        assert!(out.contains(
            "states:\n  - at: ch_1\n    location: village\n  - at: ch_4\n    location: blackwood\n"
        ));
        let doc = p.read_document(&rel).unwrap();
        let states = doc.frontmatter.get_value("states").unwrap();
        assert_eq!(states.as_sequence().unwrap().len(), 2);
    }

    #[test]
    fn external_rename_still_resolves_by_id() {
        let tmp = TempDir::new().unwrap();
        let mut p = created_project(&tmp);
        let rel = p.create_document(DocumentKind::Chapter, "Move Me").unwrap();
        let id = p.peek(&rel).unwrap().0.unwrap();

        // Someone renames the file in their file manager while we run.
        std::fs::rename(
            tmp.path().join(&rel),
            tmp.path().join("manuscript/001-renamed.md"),
        )
        .unwrap();

        let resolved = p.resolve(&id).unwrap();
        assert_eq!(resolved, PathBuf::from("manuscript/001-renamed.md"));
        // The old path no longer resolves after the rescan.
        assert!(p.resolve("manuscript/001-move-me.md").is_err());
    }

    #[test]
    fn future_format_version_opens_read_only() {
        let tmp = TempDir::new().unwrap();
        created_project(&tmp);
        let yaml = tmp.path().join(PROJECT_FILE);
        let raw = std::fs::read_to_string(&yaml).unwrap();
        let raw = raw.replace("format_version: '1'", "format_version: '99'");
        std::fs::write(&yaml, raw).unwrap();

        let mut p = Project::open(tmp.path()).unwrap();
        assert!(p.read_only());
        let reason = p.read_only_reason().unwrap();
        assert!(
            reason.contains('9'),
            "reason should name the version: {reason}"
        );
        assert!(p.create_document(DocumentKind::Chapter, "Nope").is_err());
        assert!(p.save_document(Path::new("manuscript/x.md"), "x").is_err());
    }

    #[test]
    fn second_open_is_read_only_while_first_holds_lock() {
        let tmp = TempDir::new().unwrap();
        let _first = created_project(&tmp);
        let mut second = Project::open(tmp.path()).unwrap();
        assert!(second.read_only());
        assert!(
            second
                .read_only_reason()
                .unwrap()
                .contains("another session")
        );
        assert!(second.create_document(DocumentKind::Note, "x").is_err());
    }

    #[test]
    fn resolve_by_title_and_path() {
        let tmp = TempDir::new().unwrap();
        let mut p = created_project(&tmp);
        let rel = p
            .create_document(DocumentKind::Chapter, "Unique Title Here")
            .unwrap();
        assert_eq!(p.resolve("Unique Title Here").unwrap(), rel);
        assert_eq!(
            p.resolve("manuscript/001-unique-title-here.md").unwrap(),
            rel
        );
        assert!(p.resolve("ch_does_not_exist").is_err());
    }
}
