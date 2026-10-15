//! Snapshots — the project's safety net (G12/A11).
//!
//! A snapshot is the whole project state at one moment: every file that
//! counts as authoring content, restored byte-exact with one call. Storage
//! is internal (`.oca/state/snapshots.git`, git objects via `git2`) and the
//! UX vocabulary is **Snapshots** only — nothing user-facing may use Git
//! terms (rule 7, enforced by a source scan in `oca-cli/tests/vocabulary.rs`).
//!
//! What a snapshot covers:
//! - every file under the project root, except `.oca/` internals
//!   (cache/logs/state are not authoring content — rule 2), `.git/`,
//!   `target/`, and other people's tooling directories;
//! - **audio (`*.wav`) is tracked by checksum manifest, never stored as
//!   blobs** — recorded bytes are irreplaceable and huge; a restore never
//!   touches them, it only reports how they differ.
//!
//! Snapshots are append-only: restoring first saves the current state, so
//! history only grows. The [`SnapshotStore`] trait keeps the engine
//! swappable (implementation lock: git2 with vendored libgit2 — no system
//! Git required).

pub mod git_store;

pub use git_store::GitStore;

use std::path::{Path, PathBuf};

/// Where the internal snapshot store lives inside a project.
pub const STORE_DIR: &str = ".oca/state/snapshots.git";

/// In-tree (store-only) path of the per-snapshot manifest: file count plus
/// the audio checksum map. Never written to the working tree on restore.
pub const MANIFEST_NAME: &str = ".oca/snapshot-manifest.json";

/// Directory names skipped entirely when walking a project.
pub const SKIP_DIRS: &[&str] = &[".oca", ".git", "target"];

/// A snapshot as the user sees it: a number, a label, a time, a size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotInfo {
    /// 1-based position in history (oldest = 1). History only grows.
    pub n: usize,
    /// Author-supplied label, or empty for an untitled snapshot.
    pub label: String,
    /// Save time, seconds since the Unix epoch (format with [`format_time`]).
    pub when: i64,
    /// How many files the snapshot holds (audio counted by entry).
    pub files: usize,
}

impl SnapshotInfo {
    /// The label, or a generated placeholder when untitled.
    pub fn display_label(&self) -> String {
        if self.label.is_empty() {
            format!("Snapshot #{}", self.n)
        } else {
            self.label.clone()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Removed,
    Modified,
}

/// One file's change between two states (old → new), as a unified patch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    pub path: PathBuf,
    pub kind: ChangeKind,
    /// Unified diff text; empty when the file is treated as binary (images)
    /// or is audio — recorded bytes are never diffed or stored.
    pub patch: String,
    pub binary: bool,
}

/// The diff of a snapshot (or between two snapshots): one entry per changed
/// file, plus a one-line summary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffReport {
    pub files: Vec<FileDiff>,
    /// e.g. `3 file(s) changed, +12 / −7`.
    pub summary: String,
    /// Snapshots compared (oldest first); `None` means "the working files".
    pub against: Option<usize>,
    pub base: usize,
}

/// What a restore did, for the CLI/TUI report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RestoreReport {
    pub n: usize,
    /// Files written (changed or new relative to the snapshot).
    pub restored: usize,
    /// Files removed because the snapshot predates them.
    pub removed: usize,
    /// Files already identical to the snapshot.
    pub unchanged: usize,
    /// Audio takes whose recorded bytes differ from the snapshot's
    /// manifest — kept as they are, never overwritten or deleted.
    pub audio_differing: Vec<PathBuf>,
}

/// The snapshot engine, swappable behind this trait (implementation lock:
/// vendored libgit2). All paths are relative to a project root.
pub trait SnapshotStore {
    /// Save the current working state as a new snapshot (append-only).
    fn create(&mut self, root: &Path, label: Option<&str>) -> anyhow::Result<SnapshotInfo>;
    /// Every snapshot, oldest first.
    fn list(&self, root: &Path) -> anyhow::Result<Vec<SnapshotInfo>>;
    /// Return the project to snapshot `n`. Callers snapshot the current
    /// state first (auto-snapshot policy lives in Core); audio is never
    /// touched — only reported via [`RestoreReport::audio_differing`].
    fn restore(&mut self, root: &Path, n: usize) -> anyhow::Result<RestoreReport>;
    /// Snapshot `n` compared with the working files as they are now.
    fn diff_working(&self, root: &Path, n: usize) -> anyhow::Result<DiffReport>;
    /// Snapshot `a` compared with snapshot `b`.
    fn diff_pair(&self, root: &Path, a: usize, b: usize) -> anyhow::Result<DiffReport>;
}

/// Is this path audio we track by checksum only (never store as a blob)?
pub fn is_audio(rel: &Path) -> bool {
    rel.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("wav"))
}

/// Should this file be skipped entirely (not authoring content)?
pub fn is_skipped(rel: &Path) -> bool {
    rel.components().any(
        |c| matches!(c, std::path::Component::Normal(name) if SKIP_DIRS.iter().any(|d| name == *d)),
    )
}

/// `"2026-10-15 14:23"` (UTC) for a Unix time — enough for a list; the
/// store keeps the exact instant internally.
pub fn format_time(unix: i64) -> String {
    let (days, secs) = (unix.div_euclid(86_400), unix.rem_euclid(86_400));
    // Civil-from-days (Howard Hinnant's algorithm): era-based, no calendar
    // tables needed.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60
    )
}

/// Refuse snapshot number 0 — snapshots are numbered from 1, oldest
/// first, and history only grows.
pub fn validate_n(n: usize) -> anyhow::Result<()> {
    if n == 0 {
        anyhow::bail!("snapshots are numbered from 1");
    }
    Ok(())
}

/// Lowercase hex of raw bytes (audio checksums in the manifest).
pub fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Does this look like binary content (NUL in the first 8 KiB)?
pub(crate) fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|b| *b == 0)
}

/// List every file under `root` (relative paths), skipping [`SKIP_DIRS`].
/// Symlinks are not followed — a snapshot stores regular files only.
pub(crate) fn walk_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk(root, Path::new(""), &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(root: &Path, rel: &Path, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    let dir = root.join(rel);
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let child = rel.join(&name);
        let ty = entry.file_type()?;
        if ty.is_dir() {
            if !SKIP_DIRS.iter().any(|d| name == *d) {
                walk(root, &child, out)?;
            }
        } else if ty.is_file() && !is_skipped(&child) {
            out.push(child);
        }
        // Symlinks and other oddities are deliberately ignored.
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_and_audio_rules() {
        assert!(is_skipped(Path::new(".oca/cache/oca.db")));
        assert!(is_skipped(Path::new(".git/objects/ab/cd")));
        assert!(is_skipped(Path::new("target/debug/oca")));
        assert!(!is_skipped(Path::new("manuscript/001.md")));
        assert!(!is_skipped(Path::new("audio/.hidden-was-fine.md")));
        assert!(is_audio(Path::new("audio/memo.wav")));
        assert!(is_audio(Path::new("audio/MEMO.WAV")));
        assert!(!is_audio(Path::new("audio/memo.yaml")));
    }

    #[test]
    fn format_time_is_utc_readable() {
        assert_eq!(format_time(0), "1970-01-01 00:00");
        // 2026-10-15 12:00:00 UTC
        assert_eq!(format_time(1_792_065_600), "2026-10-15 12:00");
        // Pre-epoch stays sane.
        assert_eq!(format_time(-86_400), "1969-12-31 00:00");
    }

    #[test]
    fn hex_encoding() {
        assert_eq!(to_hex(&[0x00, 0x0f, 0xff]), "000fff");
    }

    #[test]
    fn binary_detection() {
        assert!(looks_binary(b"RIFF\0\0\0\0WAVE"));
        assert!(!looks_binary(b"plain prose\n"));
    }
}
