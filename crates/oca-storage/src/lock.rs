//! Single-writer lock (A3): `.oca/state/lock` held with an OS advisory file
//! lock. Advisory locks vanish with the holding process — a crashed session
//! can never wedge the project open, so there is no staleness heuristics.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::layout::PROJECT_FILE;

fn lock_path(root: &Path) -> PathBuf {
    root.join(".oca/state/lock")
}

/// Held for the lifetime of an open `Project`; released (and removed) on drop.
#[derive(Debug)]
pub struct Lock {
    file: File,
    path: PathBuf,
}

impl Lock {
    /// Returns `Ok(None)` when another session currently holds the lock.
    pub fn acquire(root: &Path) -> Result<Option<Lock>> {
        if !root.join(PROJECT_FILE).is_file() {
            anyhow::bail!("not an OCA project: {}", root.display());
        }
        let path = lock_path(root);
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false) // never touch a lock file another session may hold
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;

        match file.try_lock() {
            Ok(()) => {
                let pid = std::process::id();
                file.set_len(0)?;
                file.write_all(format!("pid {pid}\n").as_bytes())?;
                file.sync_all().ok();
                Ok(Some(Lock { file, path }))
            }
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => {
                Err(e).with_context(|| format!("lock {}", path.display()))
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::scaffold;
    use tempfile::TempDir;

    #[test]
    fn second_acquire_fails_while_held_and_succeeds_after_release() {
        let tmp = TempDir::new().unwrap();
        scaffold(tmp.path()).unwrap();
        std::fs::write(tmp.path().join(PROJECT_FILE), "project_id: prj_x\n").unwrap();

        let held = Lock::acquire(tmp.path()).unwrap();
        assert!(held.is_some(), "first acquire must win");
        assert!(
            Lock::acquire(tmp.path()).unwrap().is_none(),
            "second acquire must report the lock as held"
        );

        drop(held);
        let again = Lock::acquire(tmp.path()).unwrap();
        assert!(again.is_some(), "lock must be reacquirable after release");
        drop(again);
        assert!(!lock_path(tmp.path()).exists(), "lock file removed on drop");
    }

    #[test]
    fn acquire_requires_a_project() {
        let tmp = TempDir::new().unwrap();
        assert!(Lock::acquire(tmp.path()).is_err());
    }
}
