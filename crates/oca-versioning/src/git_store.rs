//! The git2-backed snapshot store (vendored libgit2 — no system Git).
//!
//! Internals stay internal: history is an append-only chain under
//! `refs/heads/snapshots` in a bare store at [`.oca/state/snapshots.git`].
//! Nothing here is shown to users except through the types in `lib.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use git2::{IndexEntry, IndexTime, ObjectType, Repository, Signature, Tree};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ChangeKind, DiffReport, FileDiff, MANIFEST_NAME, RestoreReport, STORE_DIR, SnapshotInfo,
    SnapshotStore, is_audio, looks_binary, to_hex, walk_files,
};

// The manifest lives at this in-tree path inside every snapshot.
const MANIFEST: &str = MANIFEST_NAME;

/// Internal ref holding the snapshot chain (append-only).
const REF: &str = "refs/heads/snapshots";

/// Per-snapshot manifest, stored inside the snapshot's own tree under
/// `.oca/` — never materialized into the working tree.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    /// How many files the snapshot holds (excluding the manifest itself).
    files: usize,
    /// Working-tree-relative WAV path → lowercase hex SHA-256.
    #[serde(default)]
    audio: BTreeMap<String, String>,
}

/// `rel path → file bytes` — one snapshot's (or the working tree's) text
/// content, audio excluded.
type FileMap = BTreeMap<PathBuf, Vec<u8>>;
/// `audio path → hex checksum` — the manifest's view of recorded bytes.
type ChecksumMap = BTreeMap<String, String>;

/// The default store. Cheap to construct; everything derives from `root`.
#[derive(Default)]
pub struct GitStore;

impl GitStore {
    pub fn new() -> Self {
        Self
    }

    /// Open (or create) the bare store for `root`. Creating is lazy: a
    /// project earns its store on the first snapshot.
    fn open(root: &Path) -> Result<Repository> {
        let dir = root.join(STORE_DIR);
        if dir.exists() {
            Repository::open(&dir).context("open snapshot store")
        } else {
            let mut opts = git2::RepositoryInitOptions::new();
            opts.bare(true).no_reinit(true);
            Repository::init_opts(&dir, &opts).context("create snapshot store")
        }
    }

    /// The snapshot chain, oldest first.
    fn chain(repo: &Repository) -> Result<Vec<git2::Oid>> {
        let mut out = Vec::new();
        if let Ok(r) = repo.find_reference(REF) {
            let mut c = r.peel_to_commit().context("read snapshot tip")?;
            loop {
                out.push(c.id());
                match c.parent(0) {
                    Ok(p) => c = p,
                    Err(_) => break,
                }
            }
            out.reverse();
        }
        Ok(out)
    }

    fn info(repo: &Repository, oid: git2::Oid, n: usize) -> Result<SnapshotInfo> {
        let commit = repo.find_commit(oid).context("read snapshot")?;
        let tree = commit.tree().context("read snapshot files")?;
        let manifest = read_manifest(repo, &tree).unwrap_or_default();
        Ok(SnapshotInfo {
            n,
            label: commit.message().unwrap_or("").trim().to_string(),
            when: commit.time().seconds(),
            files: manifest.files,
        })
    }

    /// All files of a snapshot as `rel → bytes` (the in-tree manifest
    /// excluded), plus its audio checksum map.
    fn tree_files(repo: &Repository, tree: &Tree) -> Result<(FileMap, ChecksumMap)> {
        let mut files = BTreeMap::new();
        collect(repo, tree, Path::new(""), &mut files)?;
        let manifest: Manifest = files
            .remove(Path::new(MANIFEST))
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or_default();
        Ok((files, manifest.audio))
    }

    /// The working tree as `rel → bytes`, splitting out audio paths.
    fn working(root: &Path) -> Result<(FileMap, Vec<PathBuf>)> {
        let mut files = BTreeMap::new();
        let mut audio = Vec::new();
        for rel in walk_files(root)? {
            if is_audio(&rel) {
                audio.push(rel);
            } else {
                files.insert(rel.clone(), std::fs::read(root.join(&rel))?);
            }
        }
        Ok((files, audio))
    }

    fn commit_of(repo: &Repository, n: usize) -> Result<git2::Oid> {
        let chain = Self::chain(repo)?;
        chain
            .get(n.wrapping_sub(1))
            .copied()
            .with_context(|| format!("no snapshot #{n} (history holds {})", chain.len()))
    }
}

impl SnapshotStore for GitStore {
    fn create(&mut self, root: &Path, label: Option<&str>) -> Result<SnapshotInfo> {
        let repo = Self::open(root)?;
        let mut manifest = Manifest {
            version: 1,
            ..Manifest::default()
        };
        let mut blobs: Vec<(PathBuf, git2::Oid, usize)> = Vec::new();
        for rel in walk_files(root)? {
            let bytes = std::fs::read(root.join(&rel))
                .with_context(|| format!("read {}", rel.display()))?;
            if is_audio(&rel) {
                manifest.audio.insert(
                    rel.to_string_lossy().into_owned(),
                    to_hex(&Sha256::digest(&bytes)),
                );
            } else {
                let oid = repo.blob(&bytes).context("store snapshot data")?;
                blobs.push((rel, oid, bytes.len()));
            }
        }
        manifest.files = blobs.len();

        // Content tree via the store's index (no working checkout needed).
        let mut index = repo.index().context("snapshot index")?;
        index.clear().context("snapshot index")?;
        for (rel, oid, len) in &blobs {
            let entry = IndexEntry {
                ctime: IndexTime::new(0, 0),
                mtime: IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode: 0o100644,
                uid: 0,
                gid: 0,
                file_size: (*len).min(u32::MAX as usize) as u32,
                id: *oid,
                flags: 0,
                flags_extended: 0,
                path: rel.as_os_str().as_encoded_bytes().to_vec(),
            };
            index.add(&entry).context("snapshot index entry")?;
        }
        let content_oid = index.write_tree().context("snapshot tree")?;

        // Nest the manifest under `.oca/` so restore never writes it to the
        // working tree (`.oca/` is skipped wholesale). The outer tree is
        // seeded from the content tree so both live side by side.
        let manifest_raw = serde_json::to_vec(&manifest).context("snapshot manifest")?;
        let manifest_oid = repo
            .blob(&manifest_raw)
            .context("store snapshot manifest")?;
        let mut inner = repo.treebuilder(None).context("snapshot manifest dir")?;
        inner
            .insert("snapshot-manifest.json", manifest_oid, 0o100644)
            .context("snapshot manifest dir")?;
        let inner_oid = inner.write().context("snapshot manifest dir")?;
        let content_tree = repo.find_tree(content_oid).context("snapshot tree")?;
        let mut outer = repo
            .treebuilder(Some(&content_tree))
            .context("snapshot tree")?;
        outer
            .insert(".oca", inner_oid, 0o040000)
            .context("snapshot tree")?;
        let tree_oid = outer.write().context("snapshot tree")?;
        let tree = repo.find_tree(tree_oid).context("snapshot tree")?;

        let chain = Self::chain(&repo)?;
        let parent_commits: Vec<git2::Commit> = match chain.last() {
            Some(oid) => vec![repo.find_commit(*oid).context("snapshot tip")?],
            None => Vec::new(),
        };
        let parent: Vec<&git2::Commit> = parent_commits.iter().collect();
        let sig = Signature::now("OCA", "oca@localhost").context("snapshot signature")?;
        let message = label.unwrap_or("").trim();
        let oid = repo
            .commit(Some(REF), &sig, &sig, message, &tree, &parent)
            .context("save snapshot")?;
        Self::info(&repo, oid, chain.len() + 1)
    }

    fn list(&self, root: &Path) -> Result<Vec<SnapshotInfo>> {
        let dir = root.join(STORE_DIR);
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let repo = Repository::open(&dir).context("open snapshot store")?;
        let chain = Self::chain(&repo)?;
        chain
            .iter()
            .enumerate()
            .map(|(i, oid)| Self::info(&repo, *oid, i + 1))
            .collect()
    }

    fn restore(&mut self, root: &Path, n: usize) -> Result<RestoreReport> {
        let repo = Self::open(root)?;
        let oid = Self::commit_of(&repo, n)?;
        let tree = repo.find_commit(oid)?.tree().context("snapshot files")?;
        let (want, want_audio) = Self::tree_files(&repo, &tree)?;
        let (have, have_audio) = Self::working(root)?;

        let mut report = RestoreReport {
            n,
            ..RestoreReport::default()
        };
        for (rel, bytes) in &want {
            match have.get(rel) {
                Some(cur) if cur == bytes => report.unchanged += 1,
                _ => {
                    write_file(&root.join(rel), bytes)
                        .with_context(|| format!("restore {}", rel.display()))?;
                    report.restored += 1;
                }
            }
        }
        for rel in have.keys() {
            if !want.contains_key(rel) {
                std::fs::remove_file(root.join(rel))
                    .with_context(|| format!("remove {}", rel.display()))?;
                report.removed += 1;
            }
        }
        // Audio: never written, never removed — reported only. Takes that
        // vanished from the working tree show up from the manifest side.
        let have_keys: Vec<String> = have_audio
            .iter()
            .map(|rel| rel.to_string_lossy().into_owned())
            .collect();
        for rel in have_audio {
            let key = rel.to_string_lossy().into_owned();
            match want_audio.get(&key) {
                None => report.audio_differing.push(rel), // take newer than the snapshot
                Some(expected) => {
                    let raw = std::fs::read(root.join(&rel))?;
                    if to_hex(&Sha256::digest(&raw)) != *expected {
                        report.audio_differing.push(rel);
                    }
                }
            }
        }
        for key in want_audio.keys() {
            if !have_keys.contains(key) {
                report.audio_differing.push(PathBuf::from(key));
            }
        }
        Ok(report)
    }

    fn diff_working(&self, root: &Path, n: usize) -> Result<DiffReport> {
        let repo = Self::open(root)?;
        let oid = Self::commit_of(&repo, n)?;
        let tree = repo.find_commit(oid)?.tree().context("snapshot files")?;
        let (base_files, base_audio) = Self::tree_files(&repo, &tree)?;
        let (cur_files, cur_paths) = Self::working(root)?;
        let cur_audio = audio_map(root, &cur_paths);
        Ok(build_diff(
            &base_files,
            &cur_files,
            &base_audio,
            &cur_audio,
            n,
            None,
        ))
    }

    fn diff_pair(&self, root: &Path, a: usize, b: usize) -> Result<DiffReport> {
        let repo = Self::open(root)?;
        let ta = repo
            .find_commit(Self::commit_of(&repo, a)?)?
            .tree()
            .context("snapshot files")?;
        let tb = repo
            .find_commit(Self::commit_of(&repo, b)?)?
            .tree()
            .context("snapshot files")?;
        let (fa, aa) = Self::tree_files(&repo, &ta)?;
        let (fb, ab) = Self::tree_files(&repo, &tb)?;
        Ok(build_diff(&fa, &fb, &aa, &ab, a, Some(b)))
    }
}

fn collect(
    repo: &Repository,
    tree: &Tree,
    prefix: &Path,
    out: &mut BTreeMap<PathBuf, Vec<u8>>,
) -> Result<()> {
    for entry in tree.iter() {
        let name = String::from_utf8_lossy(entry.name_bytes()).into_owned();
        let rel = prefix.join(name);
        match entry.kind() {
            Some(ObjectType::Tree) => {
                let sub = entry
                    .to_object(repo)?
                    .into_tree()
                    .map_err(|_| anyhow::anyhow!("not a directory: {}", rel.display()))?;
                collect(repo, &sub, &rel, out)?;
            }
            Some(ObjectType::Blob) => {
                let blob = entry
                    .to_object(repo)?
                    .into_blob()
                    .map_err(|_| anyhow::anyhow!("not a file: {}", rel.display()))?;
                out.insert(rel, blob.content().to_vec());
            }
            _ => {} // anything else is not authoring content
        }
    }
    Ok(())
}

fn read_manifest(repo: &Repository, tree: &Tree) -> Option<Manifest> {
    let entry = tree.get_name(".oca")?;
    let sub = entry.to_object(repo).ok()?.into_tree().ok()?;
    let entry = sub.get_name("snapshot-manifest.json")?;
    let blob = entry.to_object(repo).ok()?.into_blob().ok()?;
    serde_json::from_slice(blob.content()).ok()
}

/// Compare two file sets plus their audio checksum maps into a unified
/// report. Audio never yields a patch — recorded bytes are not stored.
fn build_diff(
    base: &FileMap,
    cur: &FileMap,
    base_audio: &ChecksumMap,
    cur_audio: &ChecksumMap,
    base_n: usize,
    cur_n: Option<usize>,
) -> DiffReport {
    let base_label = format!("snapshot #{base_n}");
    let cur_label = match cur_n {
        Some(m) => format!("snapshot #{m}"),
        None => "now".to_string(),
    };
    let (base_label, cur_label) = (base_label.as_str(), cur_label.as_str());
    let mut files = Vec::new();
    let mut plus = 0usize;
    let mut minus = 0usize;

    let mut paths: Vec<&PathBuf> = base.keys().chain(cur.keys()).collect();
    paths.sort();
    paths.dedup();
    for path in paths {
        let old = base.get(path);
        let new = cur.get(path);
        if old == new {
            continue;
        }
        let kind = match (old, new) {
            (None, Some(_)) => ChangeKind::Added,
            (Some(_), None) => ChangeKind::Removed,
            _ => ChangeKind::Modified,
        };
        let old_bytes: &[u8] = old.map(Vec::as_slice).unwrap_or_default();
        let new_bytes: &[u8] = new.map(Vec::as_slice).unwrap_or_default();
        if looks_binary(old_bytes) || looks_binary(new_bytes) {
            files.push(FileDiff {
                path: path.clone(),
                kind,
                patch: String::new(),
                binary: true,
            });
            continue;
        }
        let old_text = String::from_utf8_lossy(old_bytes);
        let new_text = String::from_utf8_lossy(new_bytes);
        let diff = similar::TextDiff::from_lines(&old_text, &new_text);
        for change in diff.iter_all_changes() {
            match change.tag() {
                similar::ChangeTag::Insert => plus += 1,
                similar::ChangeTag::Delete => minus += 1,
                _ => {}
            }
        }
        let patch = diff
            .unified_diff()
            .context_radius(3)
            .header(
                &format!("{} ({base_label})", path.display()),
                &format!("{} ({cur_label})", path.display()),
            )
            .to_string();
        files.push(FileDiff {
            path: path.clone(),
            kind,
            patch,
            binary: false,
        });
    }

    // Audio differences (manifest vs manifest, or manifest vs working).
    let mut audio_paths: Vec<&String> = base_audio.keys().chain(cur_audio.keys()).collect();
    audio_paths.sort();
    audio_paths.dedup();
    for key in audio_paths {
        let (old, new) = (base_audio.get(key), cur_audio.get(key));
        if old == new {
            continue;
        }
        let kind = match (old, new) {
            (None, Some(_)) => ChangeKind::Added,
            (Some(_), None) => ChangeKind::Removed,
            _ => ChangeKind::Modified,
        };
        files.push(FileDiff {
            path: PathBuf::from(key),
            kind,
            patch: String::new(),
            binary: true,
        });
    }

    let changed = files.len();
    let summary = if changed == 0 {
        "no changes".to_string()
    } else {
        format!("{changed} file(s) changed, +{plus} / −{minus}")
    };
    DiffReport {
        files,
        summary,
        against: cur_n,
        base: base_n,
    }
}

/// SHA-256 checksum map for the working tree's audio files.
fn audio_map(root: &Path, rels: &[PathBuf]) -> ChecksumMap {
    rels.iter()
        .filter_map(|rel| {
            let raw = std::fs::read(root.join(rel)).ok()?;
            Some((
                rel.to_string_lossy().into_owned(),
                to_hex(&Sha256::digest(&raw)),
            ))
        })
        .collect()
}

/// Write bytes to `path` atomically: a sibling temp file, then rename.
fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("create directory {}", dir.display()))?;
    }
    let tmp = path.with_extension("snaptmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}
