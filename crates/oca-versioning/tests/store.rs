//! Phase 6 exit tests for the snapshot store: create → diff → restore
//! round-trips, a fuzz-style restore reproduces the exact prior filesystem
//! state, and audio is checksum-manifest only (never stored, never touched).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use oca_versioning::{GitStore, SnapshotStore, is_skipped};

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// Every non-skipped file as `rel → bytes` — the exactness oracle.
fn state(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(dir: &Path, rel: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let child = rel.join(e.file_name());
            if is_skipped(&child) {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                walk(&e.path(), &child, out);
            } else {
                out.insert(child, std::fs::read(e.path()).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, Path::new(""), &mut out);
    out
}

#[test]
fn create_lists_and_numbers_append_only() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let mut store = GitStore::new();

    assert!(store.list(root).unwrap().is_empty(), "no store yet");

    write(root, "manuscript/001.md", b"# One\n");
    let s1 = store.create(root, Some("Before the rewrite")).unwrap();
    assert_eq!(s1.n, 1);
    assert_eq!(s1.label, "Before the rewrite");
    assert_eq!(s1.files, 1);

    write(root, "manuscript/002.md", b"# Two\n");
    let s2 = store.create(root, None).unwrap();
    assert_eq!(s2.n, 2);
    assert_eq!(s2.display_label(), "Snapshot #2");

    let list = store.list(root).unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].n, 1);
    assert_eq!(list[1].files, 2, "the second snapshot sees both files");

    // Numbers are stable — history only grows.
    assert_eq!(store.create(root, None).unwrap().n, 3);
}

#[test]
fn diff_reports_every_change_kind() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let mut store = GitStore::new();

    write(root, "manuscript/keep.md", b"alpha\nbravo\ncharlie\n");
    write(root, "manuscript/edit.md", b"one\ntwo\nthree\n");
    write(root, "manuscript/gone.md", b"bye\n");
    let s = store.create(root, None).unwrap();

    write(root, "manuscript/edit.md", b"one\nTWO\nthree\nfour\n");
    write(root, "manuscript/new.md", b"hello\n");
    std::fs::remove_file(root.join("manuscript/gone.md")).unwrap();

    let diff = store.diff_working(root, s.n).unwrap();
    assert_eq!(diff.base, s.n);
    assert_eq!(diff.against, None, "compared with the working files");
    assert_eq!(diff.files.len(), 3, "keep.md is not listed: {diff:?}");
    let by: BTreeMap<_, _> = diff
        .files
        .iter()
        .map(|f| (f.path.to_string_lossy().into_owned(), f))
        .collect();
    assert_eq!(
        by["manuscript/edit.md"].kind,
        oca_versioning::ChangeKind::Modified
    );
    assert!(
        by["manuscript/edit.md"].patch.contains("-two"),
        "{}",
        by["manuscript/edit.md"].patch
    );
    assert!(by["manuscript/edit.md"].patch.contains("+TWO"));
    assert_eq!(
        by["manuscript/new.md"].kind,
        oca_versioning::ChangeKind::Added
    );
    assert_eq!(
        by["manuscript/gone.md"].kind,
        oca_versioning::ChangeKind::Removed
    );
    assert!(diff.summary.contains("3 file(s)"), "{}", diff.summary);

    // Snapshot-to-snapshot: snapshot 2 closes the loop.
    let s2 = store.create(root, None).unwrap();
    let back = store.diff_pair(root, s.n, s2.n).unwrap();
    assert_eq!(back.files.len(), 3, "{back:?}");
    let same = store.diff_pair(root, s2.n, s2.n).unwrap();
    assert_eq!(same.summary, "no changes");
}

#[test]
fn restore_round_trip_reproduces_exact_state() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let mut store = GitStore::new();

    write(root, "manuscript/001.md", b"# One\n\nThe Arrival.\n");
    write(root, "characters/elena.md", b"id: elena\n");
    write(root, "music/songs/ashes/lyrics.md", b"## Chorus\nfire\n");
    std::fs::create_dir_all(root.join("assets/images")).unwrap();
    write(
        root,
        "assets/images/cover.png",
        b"\x89PNG\x00\x01\x02binary",
    );
    let pristine = state(root);

    let s1 = store.create(root, Some("known good")).unwrap();

    // Mutate everything: edit, add, delete, binary edit.
    write(root, "manuscript/001.md", b"# One\n\nRewritten badly.\n");
    write(root, "manuscript/099.md", b"new chapter\n");
    std::fs::remove_file(root.join("characters/elena.md")).unwrap();
    write(root, "assets/images/cover.png", b"\x89PNG\x00\x03\x04other");
    let messy = state(root);
    assert_ne!(pristine, messy, "the mutation took effect");

    let report = store.restore(root, s1.n).unwrap();
    assert_eq!(report.n, s1.n);
    assert!(report.removed >= 1, "099.md and friends: {report:?}");
    assert_eq!(state(root), pristine, "restore reproduced the exact state");
}

/// Fuzz-style: a seeded LCG piles on random edits/additions/deletes, and
/// after each snapshot restore the filesystem matches that snapshot's
/// captured state byte for byte (audio excluded by design).
#[test]
fn fuzz_restore_reproduces_captured_states() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let mut store = GitStore::new();
    let mut rng = 0x5eedu64;
    let mut next = move || {
        rng = rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        rng >> 33
    };

    let names = ["alpha", "bravo", "charlie", "delta", "echo"];
    let mut snapshots: Vec<(usize, BTreeMap<PathBuf, Vec<u8>>)> = Vec::new();

    for round in 0..30 {
        // Random mutations.
        for _ in 0..(next() % 4 + 1) {
            let name = names[(next() % names.len() as u64) as usize];
            let rel = format!("world/{name}.md");
            match next() % 3 {
                0 => {
                    let body: String = format!("round {round} edit {}\n", next());
                    write(root, &rel, body.as_bytes());
                }
                1 => {
                    let _ = std::fs::remove_file(root.join(&rel));
                }
                _ => {
                    let mut raw = std::fs::read(root.join(&rel)).unwrap_or_default();
                    raw.extend_from_slice(format!("more {round}\n").as_bytes());
                    write(root, &rel, &raw);
                }
            }
        }
        // Sometimes snapshot, and always verify one restore per snapshot.
        if next() % 2 == 0 {
            let info = store.create(root, Some(&format!("round {round}"))).unwrap();
            let captured = state(root);
            snapshots.push((info.n, captured));
        }
        if let Some((n, expected)) = snapshots.last() {
            store.restore(root, *n).unwrap();
            assert_eq!(
                &state(root),
                expected,
                "restore of snapshot #{n} in round {round}"
            );
        }
    }

    // And the earliest snapshot still restores to its original capture.
    let (n, expected) = snapshots.first().unwrap();
    store.restore(root, *n).unwrap();
    assert_eq!(&state(root), expected, "the oldest snapshot survives churn");
}

#[test]
fn audio_is_manifest_only_and_never_touched() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let mut store = GitStore::new();

    write(root, "manuscript/001.md", b"# One\n");
    write(root, "audio/memo.wav", b"RIFFfake-wav-bytes-000");
    let s1 = store.create(root, None).unwrap();

    // The WAV is not in the snapshot as a blob: only a checksum in the
    // manifest. Proof — restore with the WAV deleted reports it missing
    // but cannot resurrect it, and the round-trip above still passes.
    std::fs::remove_file(root.join("audio/memo.wav")).unwrap();
    let report = store.restore(root, s1.n).unwrap();
    assert_eq!(
        report.audio_differing,
        vec![PathBuf::from("audio/memo.wav")],
        "missing take reported: {report:?}"
    );
    assert!(!root.join("audio/memo.wav").exists(), "never resurrected");

    // A changed take is reported and left exactly as recorded.
    write(root, "audio/memo.wav", b"RIFFdifferent-bytes-999");
    let report = store.restore(root, s1.n).unwrap();
    assert_eq!(report.audio_differing.len(), 1);
    assert_eq!(
        std::fs::read(root.join("audio/memo.wav")).unwrap(),
        b"RIFFdifferent-bytes-999",
        "restore never overwrites recorded bytes"
    );
    // Text around it was restored.
    assert_eq!(
        std::fs::read(root.join("manuscript/001.md")).unwrap(),
        b"# One\n"
    );

    // A take matching the manifest is not reported.
    write(root, "audio/memo.wav", b"RIFFfake-wav-bytes-000");
    let report = store.restore(root, s1.n).unwrap();
    assert!(report.audio_differing.is_empty(), "{report:?}");

    // Diff flags audio changes as binary (no patch, never a content diff).
    write(root, "audio/memo.wav", b"RIFFyet-other-bytes");
    let diff = store.diff_working(root, s1.n).unwrap();
    let audio = diff
        .files
        .iter()
        .find(|f| f.path == Path::new("audio/memo.wav"))
        .expect("audio difference is visible");
    assert!(audio.binary);
    assert!(audio.patch.is_empty());
}

#[test]
fn restore_of_missing_snapshot_is_a_clear_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let mut store = GitStore::new();
    write(root, "a.md", b"x\n");
    store.create(root, None).unwrap();
    let err = store.restore(root, 7).unwrap_err().to_string();
    assert!(err.contains("#7"), "{err}");
    assert!(err.contains("1"), "says how many exist: {err}");
}

#[test]
fn empty_project_and_unicode_paths() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let mut store = GitStore::new();

    let s = store.create(root, Some("nothing yet")).unwrap();
    assert_eq!(s.files, 0);
    assert_eq!(store.list(root).unwrap().len(), 1);

    write(root, "world/élan.md", "crème\n".as_bytes());
    write(root, "notes/日本語.md", "ノート\n".as_bytes());
    let s2 = store.create(root, None).unwrap();
    assert_eq!(s2.files, 2);

    std::fs::remove_file(root.join("world/élan.md")).unwrap();
    let report = store.restore(root, s2.n).unwrap();
    assert_eq!(
        (report.restored, report.removed, report.unchanged),
        (1, 0, 1)
    );
    assert_eq!(
        std::fs::read(root.join("world/élan.md")).unwrap(),
        "crème\n".as_bytes()
    );
    assert_eq!(
        std::fs::read(root.join("notes/日本語.md")).unwrap(),
        "ノート\n".as_bytes()
    );
}

#[test]
fn snapshots_ignore_internal_state() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let mut store = GitStore::new();

    write(root, "manuscript/001.md", b"# One\n");
    write(root, ".oca/cache/oca.db", b"sqlite-bytes");
    write(root, ".oca/state/recovery/x.journal", b"draft");
    write(root, ".git/config", b"[core]\n");
    write(root, "target/debug/oca", b"binary");
    let s = store.create(root, None).unwrap();
    assert_eq!(s.files, 1, "only the manuscript: {s:?}");

    // Mutating internals doesn't show up in diffs either.
    write(root, ".oca/cache/oca.db", b"other sqlite bytes");
    let diff = store.diff_working(root, s.n).unwrap();
    assert_eq!(diff.summary, "no changes");
}
