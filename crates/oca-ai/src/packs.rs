//! Runtime packs (ADR-0007 / D8): optional per-platform downloads holding
//! `llama-server` (and later whisper), side-loaded **offline** via
//! `oca runtime install --from <pack.tar.gz>`. Every file is verified
//! against the pack's SHA-256 manifest *before* anything lands in
//! `~/.oca/runtime/` — a tampered or wrong-platform pack never installs.
//!
//! Pack format: a `.tar.gz` containing `manifest.json` plus the files it
//! lists (conventionally under `bin/`, which becomes executable on unix).

use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The pack's self-description, stored at the archive root.
pub const MANIFEST_FILE: &str = "manifest.json";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackManifest {
    pub name: String,
    pub version: String,
    /// `"<os>-<arch>"` (see [`PackManifest::current_platform`]) or `"any"`.
    pub platform: String,
    /// Archive-relative path → lowercase hex SHA-256.
    pub files: BTreeMap<String, String>,
}

impl PackManifest {
    pub fn current_platform() -> String {
        format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
    }
}

/// A pack found under the runtime root, plus everything wrong with it
/// (empty `problems` = verified clean).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledPack {
    pub name: String,
    pub version: String,
    pub dir: PathBuf,
    pub problems: Vec<String>,
}

/// What a successful install produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallOutcome {
    pub dir: PathBuf,
    pub name: String,
    pub version: String,
    /// Number of files verified against the manifest.
    pub files: usize,
}

/// Verify, extract, and place a pack under `runtime_root` (default
/// `~/.oca/runtime`). Every file is checksum-verified before it lands.
pub fn install_pack(from: &Path, runtime_root: &Path) -> Result<InstallOutcome> {
    // Pass 1: the manifest governs everything, so read it first.
    let manifest = read_manifest(from)
        .with_context(|| format!("read pack manifest from {}", from.display()))?;
    if manifest.platform != "any" && manifest.platform != PackManifest::current_platform() {
        return Err(anyhow!(
            "pack is for '{}' but this is '{}' — refusing to install",
            manifest.platform,
            PackManifest::current_platform()
        ));
    }
    if manifest.name.is_empty() || manifest.version.is_empty() {
        return Err(anyhow!("pack manifest needs a name and a version"));
    }

    let dest = runtime_root.join(format!("{}-{}", manifest.name, manifest.version));
    if dest.exists() {
        return Err(anyhow!(
            "{} {} is already installed at {} — remove it first",
            manifest.name,
            manifest.version,
            dest.display()
        ));
    }

    std::fs::create_dir_all(runtime_root)
        .with_context(|| format!("create runtime directory {}", runtime_root.display()))?;
    let staging = runtime_root.join(".installing");
    if staging.exists() {
        std::fs::remove_dir_all(&staging).ok();
    }

    let extract = extract_verified(from, &manifest, &staging);
    if let Err(err) = extract {
        std::fs::remove_dir_all(&staging).ok();
        return Err(err);
    }

    std::fs::rename(&staging, &dest)
        .with_context(|| format!("place {} at {}", manifest.name, dest.display()))?;
    Ok(InstallOutcome {
        dir: dest,
        name: manifest.name,
        version: manifest.version,
        files: manifest.files.len(),
    })
}

/// Pass 2: extract every file, hashing as we go; nothing is trusted until
/// the whole pack matches.
fn extract_verified(from: &Path, manifest: &PackManifest, staging: &Path) -> Result<()> {
    std::fs::create_dir_all(staging)
        .with_context(|| format!("create staging directory {}", staging.display()))?;
    let file =
        std::fs::File::open(from).with_context(|| format!("open pack {}", from.display()))?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let mut seen: HashSet<String> = HashSet::new();
    for entry in archive.entries().context("read pack entries")? {
        let mut entry = entry.context("read pack entry")?;
        let rel = safe_rel_path(&entry.path().context("pack entry path")?)?;
        if entry.header().entry_type().is_symlink() {
            return Err(anyhow!(
                "pack contains a symbolic link ({}): not allowed",
                rel.display()
            ));
        }
        if rel == Path::new(MANIFEST_FILE) {
            // Already parsed in pass 1 (it is the trust anchor, not listed
            // in its own files map) — copy it through as-is.
            let out_path = staging.join(MANIFEST_FILE);
            let mut out = std::fs::File::create(&out_path).context("extract manifest")?;
            std::io::copy(&mut entry, &mut out).context("extract manifest")?;
            continue;
        }
        let key = rel.to_string_lossy().into_owned();
        if !entry.header().entry_type().is_file() {
            // Directories are implied by their files; anything else is odd.
            continue;
        }
        let expected = manifest
            .files
            .get(&key)
            .ok_or_else(|| anyhow!("pack contains '{}' which is not in its manifest", key))?;
        if let Some(parent) = rel.parent() {
            std::fs::create_dir_all(staging.join(parent))
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let out_path = staging.join(&rel);
        let mut out =
            std::fs::File::create(&out_path).with_context(|| format!("extract {}", key))?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let n = entry
                .read(&mut buffer)
                .with_context(|| format!("extract {key}"))?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
            out.write_all(&buffer[..n])
                .with_context(|| format!("extract {key}"))?;
        }
        let actual = to_hex(&hasher.finalize());
        if &actual != expected {
            return Err(anyhow!(
                "pack file '{key}' failed its checksum — the pack is damaged or tampered with"
            ));
        }
        set_executable_bit(&rel, &out_path);
        seen.insert(key);
    }
    for key in manifest.files.keys() {
        if !seen.contains(key) {
            return Err(anyhow!(
                "pack is missing '{}' (listed in its manifest)",
                key
            ));
        }
    }
    Ok(())
}

/// Read and parse `manifest.json` from a `.tar.gz` pack.
fn read_manifest(from: &Path) -> Result<PackManifest> {
    let file = std::fs::File::open(from)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries().context("read pack entries")? {
        let mut entry = entry.context("read pack entry")?;
        let path = entry.path().context("pack entry path")?.to_path_buf();
        if path == Path::new(MANIFEST_FILE) {
            let mut raw = String::new();
            entry.read_to_string(&mut raw).context("read manifest")?;
            return serde_json::from_str(&raw).context("parse pack manifest");
        }
    }
    Err(anyhow!("pack has no {MANIFEST_FILE}"))
}

/// Every installed pack under `runtime_root`, each with its problem list
/// (empty = verified clean right now).
pub fn list_packs(runtime_root: &Path) -> Result<Vec<InstalledPack>> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(runtime_root) else {
        return Ok(out);
    };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    dirs.sort();
    for dir in dirs {
        if !dir.is_dir() {
            continue;
        }
        let manifest_path = dir.join(MANIFEST_FILE);
        let manifest: PackManifest = match std::fs::read_to_string(&manifest_path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
        {
            Some(m) => m,
            None => continue, // not a pack (e.g. a stray ".installing" dir)
        };
        let problems = verify_pack(&dir)?;
        out.push(InstalledPack {
            name: manifest.name,
            version: manifest.version,
            dir,
            problems,
        });
    }
    Ok(out)
}

/// Re-checksum an installed pack against its manifest. Returns the problems
/// (empty = intact).
pub fn verify_pack(dir: &Path) -> Result<Vec<String>> {
    let raw = std::fs::read_to_string(dir.join(MANIFEST_FILE))
        .with_context(|| format!("read manifest in {}", dir.display()))?;
    let manifest: PackManifest = serde_json::from_str(&raw).context("parse pack manifest")?;
    let mut problems = Vec::new();
    for (rel, expected) in &manifest.files {
        let path = dir.join(rel);
        match hash_file(&path) {
            Ok(actual) if &actual == expected => {}
            Ok(_) => problems.push(format!("{rel} changed since install")),
            Err(_) => problems.push(format!("{rel} is missing")),
        }
    }
    Ok(problems)
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(to_hex(&hasher.finalize()))
}

/// Pack entries must be plain relative paths — no absolute, no `..`, no
/// drive prefixes (a tar slip would otherwise escape the runtime dir).
fn safe_rel_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Err(anyhow!(
            "pack entry has an absolute path: {}",
            path.display()
        ));
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => {
                return Err(anyhow!("pack entry escapes the pack: {}", path.display()));
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err(anyhow!("pack entry has an empty path"));
    }
    Ok(out)
}

#[cfg(unix)]
fn set_executable_bit(rel: &Path, path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    // `bin/` contents are executables; everything else stays 0644.
    let mode = if rel.starts_with("bin") { 0o755 } else { 0o644 };
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn set_executable_bit(_rel: &Path, _path: &Path) {}

fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_rel_path_rejects_every_escape() {
        assert!(safe_rel_path(Path::new("../evil")).is_err());
        assert!(safe_rel_path(Path::new("a/../../b")).is_err());
        assert!(safe_rel_path(Path::new("/etc/passwd")).is_err());
        assert!(safe_rel_path(Path::new("..")).is_err());
        assert!(safe_rel_path(Path::new("")).is_err());
        assert_eq!(
            safe_rel_path(Path::new("./bin/llama-server")).unwrap(),
            PathBuf::from("bin/llama-server")
        );
        assert_eq!(
            safe_rel_path(Path::new("bin/x")).unwrap(),
            PathBuf::from("bin/x")
        );
    }
}

/// Test/release support: build a runtime pack (`.tar.gz` + manifest) — the
/// mirror of `install_pack`, shipped so integration tests and release
/// tooling can produce packs without pulling tar/flate2 in themselves.
/// Documented-hidden on purpose: product code installs, never builds.
#[doc(hidden)]
// Test-support code: unused by the product, but shipped so integration
// tests and release tooling can build packs without extra dependencies.
#[allow(dead_code)]
pub mod pack_writer {
    use std::collections::BTreeMap;
    use std::path::Path;

    use sha2::{Digest, Sha256};

    use crate::PackManifest;

    /// Write a pack containing `manifest.json` plus `files` to `at`.
    /// Returns the manifest that was embedded.
    pub fn write_pack_file(
        at: &Path,
        name: &str,
        version: &str,
        files: &[(&str, &[u8])],
    ) -> std::io::Result<PackManifest> {
        let manifest = PackManifest {
            name: name.into(),
            version: version.into(),
            platform: PackManifest::current_platform(),
            files: files
                .iter()
                .map(|(rel, bytes)| {
                    let mut hasher = Sha256::new();
                    hasher.update(bytes);
                    let mut hex = String::new();
                    for b in hasher.finalize() {
                        hex.push_str(&format!("{b:02x}"));
                    }
                    (rel.to_string(), hex)
                })
                .collect::<BTreeMap<_, _>>(),
        };
        let raw = serde_json::to_vec(&manifest)?;
        let file = std::fs::File::create(at)?;
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        append(&mut builder, super::MANIFEST_FILE, &raw)?;
        for (rel, bytes) in files {
            append(&mut builder, rel, bytes)?;
        }
        builder.into_inner()?.finish()?;
        Ok(manifest)
    }

    fn append(
        builder: &mut tar::Builder<flate2::write::GzEncoder<std::fs::File>>,
        path: &str,
        bytes: &[u8],
    ) -> std::io::Result<()> {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, path, bytes)?;
        Ok(())
    }
}
