//! Incremental vector sync: chunk the indexed Markdown, embed only what
//! changed, keep `rag_chunks` + the `vec0` table consistent with the
//! files. Everything here lives in the cache DB (rule 2) — delete
//! `.oca/cache/oca.db` and the next `oca reindex` rebuilds it all.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, Result, anyhow};
use oca_ai::Embedder;
use oca_core::Core;
use rusqlite::{Connection, params};

use crate::chunk::{Chunk, chunk_markdown};

/// The `vec0` virtual table (created with the embedding model's
/// dimension — no static migration can know it).
pub const VEC_TABLE: &str = "rag_vectors";

/// Texts per embedding request — small enough for short-context models,
/// large enough that a manuscript is a handful of calls.
const EMBED_BATCH: usize = 32;

/// What one sync pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncStats {
    /// Chunks in the index after this pass.
    pub chunks: usize,
    /// Chunks embedded (and re-embedded) this pass.
    pub embedded: usize,
    /// Chunks whose vectors were already current — never re-embedded.
    pub unchanged: usize,
    /// Rows dropped: documents deleted, shrunk, or emptied.
    pub removed: usize,
}

struct Work {
    rel: String,
    idx: usize,
    chunk: Chunk,
    mtime_ns: i64,
}

/// Sync the vector index with the project's Markdown documents.
/// `embedder.load()` happens here (idempotent) — with no model installed
/// the caller never reaches this function (rule 3).
pub fn sync(core: &Core, embedder: &dyn Embedder) -> Result<SyncStats> {
    let db = core
        .project()
        .and_then(|p| p.db())
        .context("no cache index yet — run `oca reindex` first")?;
    let conn = db.connection();
    let docs = db.markdown_docs()?;
    sync_conn(conn, embedder, &docs)
}

pub(crate) fn sync_conn(
    conn: &Connection,
    embedder: &dyn Embedder,
    docs: &[(std::path::PathBuf, String, i64)],
) -> Result<SyncStats> {
    let model = embedder.info().name;
    embedder.load().context("load the embedding model")?;

    let mut stats = SyncStats::default();
    let mut live_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut work = Vec::new();
    // A model that knows its dimension can invalidate stale vectors
    // before the work list is computed (same name, new shape).
    if let Some(dim) = embedder.dim() {
        ensure_vec_table(conn, dim)?;
    }
    for (rel, body, mtime_ns) in docs {
        let rel_s = rel.to_string_lossy().into_owned();
        let chunks = chunk_markdown(body);
        live_counts.insert(rel_s.clone(), chunks.len());
        for chunk in chunks {
            let checksum = chunk.checksum();
            let current = conn
                .query_row(
                    "SELECT content_checksum, embed_model FROM rag_chunks \
                     WHERE rel = ?1 AND idx = ?2",
                    params![rel_s, chunk.idx],
                    |row| {
                        Ok(row.get::<_, String>(0)? == checksum
                            && row.get::<_, String>(1)? == model)
                    },
                )
                .unwrap_or(false);
            if current {
                stats.unchanged += 1;
            } else {
                work.push(Work {
                    rel: rel_s.clone(),
                    idx: chunk.idx,
                    chunk,
                    mtime_ns: *mtime_ns,
                });
            }
        }
    }
    stats.chunks = live_counts.values().sum();
    stats.removed = drop_orphans(conn, &live_counts)?;

    let mut resets = 0usize;
    let mut done: BTreeSet<(String, usize)> = BTreeSet::new();
    let mut i = 0usize;
    while i < work.len() {
        let end = (i + EMBED_BATCH).min(work.len());
        let inputs: Vec<String> = work[i..end].iter().map(|w| w.chunk.embed_text()).collect();
        let refs: Vec<&str> = inputs.iter().map(String::as_str).collect();
        let vectors = embedder
            .embed(&refs)
            .with_context(|| format!("embed {} chunk(s)", inputs.len()))?;
        if vectors.len() != inputs.len() {
            return Err(anyhow!(
                "embedding model returned {} vector(s) for {} input(s)",
                vectors.len(),
                inputs.len()
            ));
        }
        let dim = vectors[0].len();
        if ensure_vec_table(conn, dim)? {
            // Dimension changed (a different model): the old vectors are
            // gone with the dropped table — recompute what's left, and
            // stop counting chunks that must now re-embed as unchanged.
            resets += 1;
            if resets > 1 {
                return Err(anyhow!(
                    "embedding dimension kept changing during one sync — refusing to loop"
                ));
            }
            stats.unchanged = 0;
            work = recompute_work(conn, docs, &model, &done)?;
            i = 0;
            continue;
        }
        for (work_item, mut vector) in work[i..end].iter().zip(vectors) {
            normalize(&mut vector);
            upsert(conn, work_item, &model, &vector)?;
            done.insert((work_item.rel.clone(), work_item.idx));
        }
        stats.embedded += end - i;
        i = end;
    }

    set_meta(conn, "model", &model)?;
    Ok(stats)
}

/// Recompute the work list after a mid-sync reset: every chunk that is
/// not already written this pass (its row vanished with the dropped
/// table).
fn recompute_work(
    conn: &Connection,
    docs: &[(std::path::PathBuf, String, i64)],
    model: &str,
    done: &BTreeSet<(String, usize)>,
) -> Result<Vec<Work>> {
    let mut work = Vec::new();
    for (rel, body, mtime_ns) in docs {
        let rel_s = rel.to_string_lossy().into_owned();
        for chunk in chunk_markdown(body) {
            if done.contains(&(rel_s.clone(), chunk.idx)) {
                continue;
            }
            let checksum = chunk.checksum();
            let current = conn
                .query_row(
                    "SELECT content_checksum, embed_model FROM rag_chunks \
                     WHERE rel = ?1 AND idx = ?2",
                    params![rel_s, chunk.idx],
                    |row| {
                        Ok(row.get::<_, String>(0)? == checksum
                            && row.get::<_, String>(1)? == model)
                    },
                )
                .unwrap_or(false);
            if !current {
                work.push(Work {
                    rel: rel_s.clone(),
                    idx: chunk.idx,
                    chunk,
                    mtime_ns: *mtime_ns,
                });
            }
        }
    }
    Ok(work)
}

/// Create the vector table when missing; return true when it had to be
/// dropped because the dimension changed (a new embedding model).
fn ensure_vec_table(conn: &Connection, dim: usize) -> Result<bool> {
    if dim == 0 {
        return Err(anyhow!("embedding model produced a zero-dimension vector"));
    }
    let exists = table_exists(conn, VEC_TABLE)?;
    let stored = meta_dim(conn);
    if exists && stored == Some(dim) {
        return Ok(false);
    }
    if exists {
        conn.execute_batch(&format!("DROP TABLE {VEC_TABLE}"))?;
        conn.execute("DELETE FROM rag_chunks", [])?;
        conn.execute("DELETE FROM rag_meta", [])?;
    }
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE {VEC_TABLE} USING vec0(embedding float[{dim}])"
    ))?;
    // Persist the dimension the table was built with — the next sync (and
    // the search-time readiness check) compares against it.
    set_meta(conn, "dim", &dim.to_string())?;
    Ok(exists)
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let found: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![name],
        |r| r.get(0),
    )?;
    Ok(found > 0)
}

fn meta_dim(conn: &Connection) -> Option<usize> {
    get_meta(conn, "dim")?.parse().ok()
}

pub(crate) fn get_meta(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM rag_meta WHERE key = ?1",
        params![key],
        |r| r.get(0),
    )
    .ok()
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO rag_meta(key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// Drop rows for documents that left the project or shrank — and their
/// vectors, which live in the same file.
fn drop_orphans(conn: &Connection, live_counts: &BTreeMap<String, usize>) -> Result<usize> {
    let mut stmt = conn.prepare("SELECT chunk_rowid, rel, idx FROM rag_chunks")?;
    let rows: Vec<(i64, String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut removed = 0usize;
    for (rowid, rel, idx) in rows {
        let alive = live_counts
            .get(&rel)
            .is_some_and(|count| (idx as usize) < *count);
        if alive {
            continue;
        }
        conn.execute("DELETE FROM rag_vectors WHERE rowid = ?1", params![rowid])?;
        conn.execute(
            "DELETE FROM rag_chunks WHERE chunk_rowid = ?1",
            params![rowid],
        )?;
        removed += 1;
    }
    Ok(removed)
}

fn upsert(conn: &Connection, work: &Work, model: &str, vector: &[f32]) -> Result<()> {
    conn.execute(
        "INSERT INTO rag_chunks(rel, idx, heading, text, content_checksum, embed_model, mtime_ns) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT(rel, idx) DO UPDATE SET \
           heading = excluded.heading, text = excluded.text, \
           content_checksum = excluded.content_checksum, \
           embed_model = excluded.embed_model, mtime_ns = excluded.mtime_ns",
        params![
            work.rel,
            work.idx as i64,
            work.chunk.heading,
            work.chunk.text,
            work.chunk.checksum(),
            model,
            work.mtime_ns,
        ],
    )?;
    let rowid: i64 = conn.query_row(
        "SELECT chunk_rowid FROM rag_chunks WHERE rel = ?1 AND idx = ?2",
        params![work.rel, work.idx as i64],
        |r| r.get(0),
    )?;
    // vec0 rows update by delete + insert.
    conn.execute("DELETE FROM rag_vectors WHERE rowid = ?1", params![rowid])?;
    let bytes: Vec<u8> = vector.iter().flat_map(|f| f.to_le_bytes()).collect();
    conn.execute(
        "INSERT INTO rag_vectors(rowid, embedding) VALUES (?1, ?2)",
        params![rowid, bytes.as_slice()],
    )?;
    Ok(())
}

/// Unit length keeps the vec0 L2 distance monotone with cosine similarity
/// — search converts `1 - d²/2` back to a cosine score.
pub(crate) fn normalize(vector: &mut [f32]) {
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in vector.iter_mut() {
            *v /= norm;
        }
    }
}

pub(crate) fn vector_blob(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|f| f.to_le_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_makes_units() {
        let mut v = vec![3.0, 4.0];
        normalize(&mut v);
        assert!((v[0] * v[0] + v[1] * v[1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn normalize_survives_the_zero_vector() {
        let mut v = vec![0.0, 0.0];
        normalize(&mut v);
        assert_eq!(v, vec![0.0, 0.0]);
    }

    #[test]
    fn blobs_are_little_endian_f32s() {
        assert_eq!(vector_blob(&[1.0]), 1.0f32.to_le_bytes().to_vec());
    }
}
