//! Runtime pack side-loading (ADR-0007 / D8): verify-then-install, platform
//! matching, tar-slip defense, and post-install verification. CI never has
//! a real llama.cpp — packs are built in the test, exactly like a release
//! pipeline would.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use oca_ai::{PackManifest, install_pack, list_packs, verify_pack};
use sha2::{Digest, Sha256};

fn sha_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let mut out = String::new();
    for b in hasher.finalize() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Build a `.tar.gz` pack from `(path, bytes)` pairs — the release
/// pipeline's job, done by hand here.
fn build_pack(at: &Path, files: &[(&str, &[u8])]) -> (PackManifest, PathBuf) {
    let manifest = PackManifest {
        name: "llama-pack".into(),
        version: "1.0.0".into(),
        platform: PackManifest::current_platform(),
        files: files
            .iter()
            .map(|(rel, bytes)| (rel.to_string(), sha_hex(bytes)))
            .collect::<BTreeMap<_, _>>(),
    };
    let mut raw = serde_json::to_vec(&manifest).unwrap();
    raw.push(b'\n');

    let pack = at.join("llama-pack-1.0.0.tar.gz");
    let file = std::fs::File::create(&pack).unwrap();
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    append(&mut builder, "manifest.json", &raw);
    for (rel, bytes) in files {
        append(&mut builder, rel, bytes);
    }
    builder.into_inner().unwrap().finish().unwrap();
    (manifest, pack)
}

fn append(
    builder: &mut tar::Builder<flate2::write::GzEncoder<std::fs::File>>,
    path: &str,
    bytes: &[u8],
) {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, path, bytes).unwrap();
}

const FAKE_SERVER: &[u8] = b"#!/bin/sh\necho fake llama-server\n";
const README: &[u8] = b"Runtime pack contents\n";

#[test]
fn install_verifies_and_places_files() {
    let tmp = tempfile::TempDir::new().unwrap();
    let runtime = tmp.path().join("runtime");
    let (_manifest, pack) = build_pack(
        tmp.path(),
        &[("bin/llama-server", FAKE_SERVER), ("README.md", README)],
    );

    let outcome = install_pack(&pack, &runtime).unwrap();
    let dir = outcome.dir.clone();
    assert_eq!(dir, runtime.join("llama-pack-1.0.0"));
    assert_eq!(outcome.name, "llama-pack");
    assert_eq!(outcome.version, "1.0.0");
    assert_eq!(outcome.files, 2);
    assert_eq!(
        std::fs::read(dir.join("bin/llama-server")).unwrap(),
        FAKE_SERVER
    );
    assert_eq!(std::fs::read(dir.join("README.md")).unwrap(), README);
    assert!(dir.join("manifest.json").exists());

    // Installed cleanly: no problems, listed by name/version.
    assert!(verify_pack(&dir).unwrap().is_empty());
    let packs = list_packs(&runtime).unwrap();
    assert_eq!(packs.len(), 1);
    assert_eq!(
        (packs[0].name.as_str(), packs[0].version.as_str()),
        ("llama-pack", "1.0.0")
    );
    assert!(packs[0].problems.is_empty());

    // The finder sees the binary inside the pack.
    let found = oca_ai::find_server_in(&runtime, None).unwrap();
    assert_eq!(found, dir.join("bin/llama-server"));
}

#[test]
fn tampered_pack_never_lands() {
    let tmp = tempfile::TempDir::new().unwrap();
    let runtime = tmp.path().join("runtime");
    let (_manifest, pack) = build_pack(tmp.path(), &[("bin/llama-server", FAKE_SERVER)]);

    // A pack whose payload no longer matches its manifest is refused.
    let tampered = tmp.path().join("tampered.tar.gz");
    tamper_file_in_gz(&pack, &tampered, "bin/llama-server", b"evil payload");
    let err = install_pack(&tampered, &runtime).unwrap_err();
    let err = format!("{err:#}");
    assert!(err.contains("checksum"), "{err:#}");
    assert!(!runtime.join("llama-pack-1.0.0").exists(), "nothing lands");
    assert!(
        !runtime.join(".installing").exists(),
        "staging cleaned up after the failure"
    );
}

/// Rebuild a pack with one file's bytes swapped (manifest untouched).
fn tamper_file_in_gz(src: &Path, dst: &Path, target: &str, new_bytes: &[u8]) {
    let file = std::fs::File::open(src).unwrap();
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().to_string_lossy().into_owned();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut bytes).unwrap();
        let replaced = if path == target {
            new_bytes.to_vec()
        } else {
            bytes
        };
        entries.push((path, replaced));
    }
    let file = std::fs::File::create(dst).unwrap();
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for (path, bytes) in entries {
        append(&mut builder, &path, &bytes);
    }
    builder.into_inner().unwrap().finish().unwrap();
}

#[test]
fn wrong_platform_is_refused() {
    let tmp = tempfile::TempDir::new().unwrap();
    let runtime = tmp.path().join("runtime");
    let file = std::fs::File::create(tmp.path().join("pack.tar.gz")).unwrap();
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    let manifest = PackManifest {
        name: "llama-pack".into(),
        version: "2.0.0".into(),
        platform: "martian-riscv".into(),
        files: BTreeMap::new(),
    };
    let raw = serde_json::to_vec(&manifest).unwrap();
    append(&mut builder, "manifest.json", &raw);
    let pack = builder
        .into_inner()
        .unwrap()
        .finish()
        .map(|_| tmp.path().join("pack.tar.gz"))
        .unwrap();

    let err = install_pack(&pack, &runtime).unwrap_err();
    let err = format!("{err:#}");
    assert!(err.contains("martian-riscv"), "{err:#}");
    assert!(err.contains(&PackManifest::current_platform()), "{err:#}");
}

#[test]
fn symlink_entries_are_refused() {
    let tmp = tempfile::TempDir::new().unwrap();
    let runtime = tmp.path().join("runtime");

    // A symlink could point anywhere; packs are plain files only. (Path
    // traversal (`..` entries) is covered by `safe_rel_path`'s unit test —
    // the tar crate refuses to even write those.)
    let pack = tmp.path().join("linky.tar.gz");
    let file = std::fs::File::create(&pack).unwrap();
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    let manifest = PackManifest {
        name: "evil".into(),
        version: "1.0.0".into(),
        platform: PackManifest::current_platform(),
        files: BTreeMap::new(),
    };
    let raw = serde_json::to_vec(&manifest).unwrap();
    append(&mut builder, "manifest.json", &raw);
    let mut header = tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_cksum();
    builder
        .append_link(&mut header, "bin/llama-server", "/etc/passwd")
        .unwrap();
    let pack = builder
        .into_inner()
        .unwrap()
        .finish()
        .map(|_| tmp.path().join("linky.tar.gz"))
        .unwrap();

    let err = install_pack(&pack, &runtime).unwrap_err();
    let err = format!("{err:#}");
    assert!(err.contains("symbolic link"), "{err:#}");
    assert!(!runtime.join("evil-1.0.0").exists());
}

#[test]
fn uninstallable_cases_fail_cleanly() {
    let tmp = tempfile::TempDir::new().unwrap();
    let runtime = tmp.path().join("runtime");

    // No manifest at all.
    let bare = tmp.path().join("bare.tar.gz");
    let file = std::fs::File::create(&bare).unwrap();
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    append(&mut builder, "bin/llama-server", FAKE_SERVER);
    let bare = builder
        .into_inner()
        .unwrap()
        .finish()
        .map(|_| tmp.path().join("bare.tar.gz"))
        .unwrap();
    let err = install_pack(&bare, &runtime).unwrap_err();
    let err = format!("{err:#}");
    assert!(err.contains("manifest.json"), "{err:#}");

    // A file in the pack that the manifest doesn't list.
    let (_m, pack) = build_pack(tmp.path(), &[("bin/llama-server", FAKE_SERVER)]);
    let extra = tmp.path().join("extra.tar.gz");
    tamper_file_in_gz(&pack, &extra, "bin/llama-server", FAKE_SERVER); // same bytes…
    // …but append an unlabeled file:
    append_extra(&extra, "sneaky.txt", b"not in the manifest");
    let err = install_pack(&extra, &runtime).unwrap_err();
    let err = format!("{err:#}");
    assert!(err.contains("not in its manifest"), "{err:#}");

    // Missing file (listed, not shipped).
    let missing = tmp.path().join("missing.tar.gz");
    strip_file(&pack, &missing, "bin/llama-server");
    let err = install_pack(&missing, &runtime).unwrap_err();
    let err = format!("{err:#}");
    assert!(err.contains("missing"), "{err:#}");

    // Installing twice is refused with a clear path to follow.
    let (_m, pack) = build_pack(tmp.path(), &[("bin/llama-server", FAKE_SERVER)]);
    install_pack(&pack, &runtime).unwrap();
    let err = install_pack(&pack, &runtime).unwrap_err();
    let err = format!("{err:#}");
    assert!(err.contains("already installed"), "{err:#}");
}

fn append_extra(gz: &Path, path: &str, bytes: &[u8]) {
    let file = std::fs::OpenOptions::new().append(true).open(gz).unwrap();
    let _ = file; // gz streams can't be appended — rebuild instead
    let rebuilt = gz.with_extension("rebuilt.tar.gz");
    let src = std::fs::File::open(gz).unwrap();
    let decoder = flate2::read::GzDecoder::new(src);
    let mut archive = tar::Archive::new(decoder);
    let out = std::fs::File::create(&rebuilt).unwrap();
    let encoder = flate2::write::GzEncoder::new(out, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let p = entry.path().unwrap().to_string_lossy().into_owned();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut bytes).unwrap();
        append(&mut builder, &p, &bytes);
    }
    append(&mut builder, path, bytes);
    builder.into_inner().unwrap().finish().unwrap();
    std::fs::rename(&rebuilt, gz).unwrap();
}

fn strip_file(gz: &Path, dst: &Path, drop_path: &str) {
    let src = std::fs::File::open(gz).unwrap();
    let decoder = flate2::read::GzDecoder::new(src);
    let mut archive = tar::Archive::new(decoder);
    let out = std::fs::File::create(dst).unwrap();
    let encoder = flate2::write::GzEncoder::new(out, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let p = entry.path().unwrap().to_string_lossy().into_owned();
        if p == drop_path {
            continue;
        }
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut bytes).unwrap();
        append(&mut builder, &p, &bytes);
    }
    builder.into_inner().unwrap().finish().unwrap();
}

#[test]
fn verification_flags_post_install_tampering() {
    let tmp = tempfile::TempDir::new().unwrap();
    let runtime = tmp.path().join("runtime");
    let (_m, pack) = build_pack(tmp.path(), &[("bin/llama-server", FAKE_SERVER)]);
    let dir = install_pack(&pack, &runtime).unwrap().dir;

    // Someone edits the installed binary — verification notices.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(dir.join("bin/llama-server"))
        .unwrap();
    file.write_all(b"# tampered").unwrap();
    drop(file);

    let problems = verify_pack(&dir).unwrap();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("changed"), "{problems:?}");

    // And a deleted file is "missing", not an error.
    std::fs::remove_file(dir.join("README.md")).ok();
    std::fs::remove_file(dir.join("bin/llama-server")).unwrap();
    let problems = verify_pack(&dir).unwrap();
    assert!(
        problems.iter().any(|p| p.contains("missing")),
        "{problems:?}"
    );
}
