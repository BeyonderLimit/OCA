//! Phase 1 exit criterion (A3): **kill-during-save crash test** — a child
//! process writes large alternating payloads through `atomic_write` in a tight
//! loop; the parent SIGKILLs it at varying delays and the target file must
//! always be exactly one complete version, never torn or truncated.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use oca_storage::atomic::atomic_write;
use tempfile::TempDir;

/// Payloads: large enough that a non-atomic writer would visibly tear, with
/// version tags for exact comparison.
const OLD: &str = "VERSION-OLD\n";
const NEW: &str = "VERSION-NEW\n";

fn padded(prefix: &str) -> String {
    let mut s = String::with_capacity(256 * 1024);
    s.push_str(prefix);
    while s.len() < 256 * 1024 {
        s.push_str("0123456789abcdef0123456789abcdef");
    }
    s
}

/// Child mode: runs when `OCA_CRASH_CHILD` points at the target file.
/// Writes OLD/NEW alternately until killed. Without the env var this test is
/// a no-op so `cargo test` passes it trivially.
#[test]
fn crash_child_writer() {
    let Ok(path) = std::env::var("OCA_CRASH_CHILD") else {
        return;
    };
    let path = PathBuf::from(path);
    let old = padded(OLD);
    let new = padded(NEW);
    let mut n = 0u64;
    loop {
        let payload = if n.is_multiple_of(2) { &new } else { &old };
        if atomic_write(&path, payload.as_bytes()).is_err() {
            // Directory vanished (parent cleaned up) — exit quietly.
            std::process::exit(0);
        }
        n = n.wrapping_add(1);
        // Tiny sleep keeps the OS from starving the parent's kill.
        if n.is_multiple_of(8) {
            std::thread::sleep(Duration::from_micros(100));
        }
    }
}

fn spawn_child(target: &std::path::Path) -> std::process::Child {
    let exe = std::env::current_exe().expect("test binary path");
    Command::new(exe)
        .args([
            "crash_child_writer",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("OCA_CRASH_CHILD", target)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn child writer")
}

#[test]
fn kill_during_save_never_tears_the_file() {
    let tmp = TempDir::new().unwrap();
    let target = tmp.path().join("manuscript").join("001-victim.md");
    fs::create_dir_all(target.parent().unwrap()).unwrap();

    let old_bytes = padded(OLD).into_bytes();
    let new_bytes = padded(NEW).into_bytes();
    fs::write(&target, &old_bytes).unwrap();

    // Deterministic schedule (A10: no wall-clock flake in CI): covers kill
    // before first save, mid-save, and after many saves.
    let mut saw_new = false;
    for delay_ms in [0u64, 3, 7, 11, 17, 29, 43, 61] {
        let mut child = spawn_child(&target);
        std::thread::sleep(Duration::from_millis(delay_ms + 2));
        let _ = child.kill();
        let _ = child.wait();

        let bytes = fs::read(&target).expect("target must exist after kill");
        let is_old = bytes == old_bytes;
        let is_new = bytes == new_bytes;
        assert!(
            is_old || is_new,
            "torn file after {delay_ms}ms kill: {} bytes (old={}, new={})",
            bytes.len(),
            old_bytes.len(),
            new_bytes.len()
        );
        saw_new |= is_new;
    }
    assert!(
        saw_new,
        "the child never completed a write — the crash test would be vacuous"
    );

    // The file is still a valid document we can reopen afterward.
    let raw = fs::read_to_string(&target).unwrap();
    let doc = oca_markdown::parse(&raw);
    assert_eq!(doc.raw, raw);
}
