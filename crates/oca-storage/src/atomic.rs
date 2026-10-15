//! Atomic file writes (A3): temp file in the same directory → fsync →
//! rename over target → fsync the directory (POSIX). A crash at any point
//! leaves the target either fully-old or fully-new — never torn.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` to `path` atomically.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| anyhow::anyhow!("path has no parent directory: {}", path.display()))?;
    fs::create_dir_all(dir).with_context(|| format!("create directory {}", dir.display()))?;

    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}.tmp-{}-{seq}", std::process::id()));

    let write = (|| -> Result<()> {
        let mut f = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(bytes).context("write temp file")?;
        f.sync_all().context("fsync temp file")?;
        drop(f);
        fs::rename(&tmp, path).with_context(|| format!("rename onto {}", path.display()))?;
        Ok(())
    })();
    if write.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write?;
    sync_dir(dir)?;
    Ok(())
}

/// fsync the directory so the rename itself is durable (POSIX only; Windows
/// has no directory handles to sync and relies on the replace-file semantics).
fn sync_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        File::open(dir)
            .with_context(|| format!("open directory {}", dir.display()))?
            .sync_all()
            .with_context(|| format!("fsync directory {}", dir.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn writes_and_replaces_leaving_no_temp_files() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("a/b.txt");
        atomic_write(&path, b"one").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"one");
        atomic_write(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    #[test]
    fn overwrites_larger_previous_content_completely() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("f.md");
        atomic_write(&path, &[b'x'; 50_000]).unwrap();
        atomic_write(&path, b"short").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"short");
    }
}
