//! Project filesystem layout (A1 layout v2) and path helpers.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};

use crate::atomic::atomic_write;

/// The one project marker file (`OCA-build.md` §4.1).
pub const PROJECT_FILE: &str = "project.yaml";

/// Files ending in this suffix are AI-comment sidecars, not documents.
pub const COMMENT_SUFFIX: &str = ".comment.md";

/// Directories every project gets on `oca new` (relative to project root).
pub const DIRS: &[&str] = &[
    "manuscript",
    "characters",
    "world",
    "outline",
    "music",
    "notes",
    "audio",
    "comments",
    "assets/images",
];

/// The app's own dirs (cache/state/logs) — everything OCA owns inside a
/// project. Created on scaffold and re-ensured on open, so a project tree
/// assembled without `oca new` (adopted, imported, generated) still works.
pub const APP_DIRS: &[&str] = &[
    ".oca/cache",
    ".oca/state",
    ".oca/state/recovery",
    ".oca/logs",
];

/// Create the empty project tree under `root`.
pub fn scaffold(root: &Path) -> Result<()> {
    for dir in DIRS.iter().chain(APP_DIRS) {
        fs_create(&root.join(dir))?;
    }
    Ok(())
}

/// Create just the `.oca/…` dirs if they're missing (idempotent).
pub fn ensure_app_dirs(root: &Path) -> Result<()> {
    for dir in APP_DIRS {
        fs_create(&root.join(dir))?;
    }
    Ok(())
}

fn fs_create(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create directory {}", dir.display()))
}

/// `rel` must be a project-relative path made only of normal components:
/// no `..`, no absolute roots, no prefix.
pub fn validate_rel(rel: &Path) -> Result<()> {
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        anyhow::bail!("path must be project-relative: {}", rel.display());
    }
    Ok(())
}

/// URL-ish slug for filenames: lowercase alphanumerics joined by `-`.
pub fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for c in title.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            out.push(c);
            pending_dash = false;
        } else {
            pending_dash = true;
        }
    }
    if out.is_empty() {
        "untitled".to_string()
    } else {
        out
    }
}

/// Write a brand-new file, refusing to clobber an existing one.
pub fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.exists() {
        anyhow::bail!("file already exists: {}", path.display());
    }
    atomic_write(path, bytes)
}

/// The `.oca` user home (`HOME` on unix, `USERPROFILE` on Windows).
pub fn oca_home() -> Result<PathBuf> {
    for var in ["HOME", "USERPROFILE"] {
        if let Ok(v) = std::env::var(var)
            && !v.is_empty()
        {
            return Ok(PathBuf::from(v));
        }
    }
    anyhow::bail!("cannot locate user home directory (HOME/USERPROFILE unset)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn scaffold_creates_full_layout() {
        let tmp = TempDir::new().unwrap();
        scaffold(tmp.path()).unwrap();
        for d in DIRS.iter().chain(APP_DIRS) {
            assert!(tmp.path().join(d).is_dir(), "missing {d}");
        }
        // precious dirs are state/cache/log locations, never deleted by us
        assert!(tmp.path().join(".oca/state/recovery").is_dir());
    }

    #[test]
    fn ensure_app_dirs_alone_is_enough_to_lock() {
        let tmp = TempDir::new().unwrap();
        // A tree with content but no `.oca/` (adopted or generated): only
        // the app dirs appear, content dirs are not invented.
        std::fs::write(tmp.path().join("project.yaml"), "project_id: prj_x\n").unwrap();
        ensure_app_dirs(tmp.path()).unwrap();
        assert!(tmp.path().join(".oca/state").is_dir());
        assert!(tmp.path().join(".oca/cache").is_dir());
        assert!(!tmp.path().join("manuscript").exists());
        // Idempotent.
        ensure_app_dirs(tmp.path()).unwrap();
    }

    #[test]
    fn slugify_cases() {
        assert_eq!(slugify("The Burden of Pride"), "the-burden-of-pride");
        assert_eq!(slugify(" Rin  Valen!! "), "rin-valen");
        assert_eq!(slugify("..."), "untitled");
        assert_eq!(slugify("Élan Vital"), "élan-vital");
        assert!(!slugify("a/b").contains('/'));
    }

    #[test]
    fn rel_paths_must_be_inside_project() {
        assert!(validate_rel(Path::new("manuscript/001-x.md")).is_ok());
        assert!(validate_rel(Path::new("../escape.md")).is_err());
        assert!(validate_rel(Path::new("/abs/x.md")).is_err());
        assert!(validate_rel(Path::new("a/../b.md")).is_err());
    }

    #[test]
    fn write_new_refuses_overwrite() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("x.md");
        write_new(&p, b"a").unwrap();
        assert!(write_new(&p, b"b").is_err());
        assert_eq!(std::fs::read(&p).unwrap(), b"a");
    }
}
