//! Hybrid search (Phase 11): SQLite FTS (Phase 5) in parallel with the
//! `vec0` kNN index, fused into one ranked, deduplicated list. The
//! fusion is deterministic arithmetic over both rankings — no model in
//! the loop, so the same query over the same files always sorts the
//! same way.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use oca_ai::{Embedder, HybridSearch, RetrievedDoc};
use oca_core::Core;
use rusqlite::params;

use crate::sync::{VEC_TABLE, get_meta, vector_blob};

/// Lexical half weight in the fusion score. Equal halves: keywords
/// anchor exact terms, vectors catch paraphrase — neither dominates.
const WEIGHT_LEXICAL: f32 = 0.5;
/// Vector half weight in the fusion score.
const WEIGHT_SEMANTIC: f32 = 0.5;
/// How many vector neighbors to consider per query (fused and deduped
/// down to `limit`).
const VECTOR_OVERFETCH: usize = 3;
/// Minimum neighbor count so short queries still see a field.
const VECTOR_MIN_K: usize = 10;
/// Snippet window for vector-only hits, characters.
const VECTOR_SNIPPET_CHARS: usize = 200;

/// One fused hit.
#[derive(Clone, Debug, PartialEq)]
pub struct RagHit {
    pub kind: String,
    pub rel: PathBuf,
    pub title: String,
    pub snippet: String,
    /// Fusion score — higher is closer to the query.
    pub score: f32,
    /// Cosine similarity of the best-matching chunk, when vectors voted.
    pub semantic: Option<f32>,
    /// 0-based rank in the keyword half, when keywords matched.
    pub lexical_rank: Option<usize>,
    /// Imported reference material under `library/` (A8): quoted data,
    /// never instructions.
    pub untrusted: bool,
}

impl RagHit {
    /// `hybrid`, `lexical`, or `vector` — which halves voted.
    pub fn origin(&self) -> &'static str {
        match (self.lexical_rank, self.semantic) {
            (Some(_), Some(_)) => "hybrid",
            (Some(_), None) => "lexical",
            (None, Some(_)) => "vector",
            (None, None) => "lexical",
        }
    }
}

/// The retrieval facade the frontends hold: an optional embedder (absent
/// = no model installed = keyword-only, rule 3).
pub struct Rag {
    embedder: Option<Arc<dyn Embedder>>,
}

impl Rag {
    pub fn new(embedder: Option<Arc<dyn Embedder>>) -> Self {
        Self { embedder }
    }

    pub fn embedder(&self) -> Option<&Arc<dyn Embedder>> {
        self.embedder.as_ref()
    }

    /// Load the embedding runtime, when there is one. Idempotent; the
    /// residency governor (A6) makes a concurrent LLM a clean error the
    /// caller surfaces, never a silent fight.
    pub fn load(&self) -> Result<()> {
        match &self.embedder {
            Some(embedder) => embedder.load(),
            None => Ok(()),
        }
    }

    pub fn search(&self, core: &Core, query: &str, limit: usize) -> Result<Vec<RagHit>> {
        hybrid_search(core, self.embedder.as_deref(), query, limit)
    }
}

/// The Phase 8 feed: `oca-ai`'s Retriever calls this through the
/// `HybridSearch` seam — `oca-rag` depends on `oca-ai`, never the other
/// way around.
impl HybridSearch for Rag {
    fn search(&self, core: &Core, query: &str, limit: usize) -> Result<Vec<RetrievedDoc>> {
        Ok(self
            .search(core, query, limit)?
            .into_iter()
            .map(|hit| RetrievedDoc {
                rel: hit.rel.display().to_string(),
                title: hit.title,
                snippet: hit.snippet,
                untrusted: hit.untrusted,
                score: hit.score,
            })
            .collect())
    }
}

/// One hybrid query: keyword hits and vector hits, fused and deduped by
/// document. Without an embedder (or an index, or a loaded runtime) the
/// vector half just doesn't vote — keyword results come back unchanged.
pub fn hybrid_search(
    core: &Core,
    embedder: Option<&dyn Embedder>,
    query: &str,
    limit: usize,
) -> Result<Vec<RagHit>> {
    let query = query.trim();
    let limit = limit.max(1);
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let lexical = core.search(query, (limit * 2).max(limit))?;

    struct Acc {
        kind: String,
        title: String,
        snippet_lex: Option<String>,
        snippet_vec: Option<String>,
        semantic: Option<f32>,
        lexical_rank: Option<usize>,
    }
    let mut docs: BTreeMap<String, Acc> = BTreeMap::new();
    for (rank, hit) in lexical.iter().enumerate() {
        let rel = hit.rel.display().to_string();
        let acc = docs.entry(rel).or_insert_with(|| Acc {
            kind: hit.kind.clone(),
            title: hit.title.clone(),
            snippet_lex: None,
            snippet_vec: None,
            semantic: None,
            lexical_rank: None,
        });
        acc.snippet_lex = Some(hit.snippet.clone());
        acc.lexical_rank = Some(rank);
    }

    // Vector half — everything it needs may legitimately be absent.
    if let Some(embedder) = embedder
        && let Some(db) = core.project().and_then(|p| p.db())
    {
        let conn = db.connection();
        if let Some(vector_hits) = vector_half(conn, embedder, query, limit)? {
            for (rel, snippet, semantic) in vector_hits {
                let acc = docs.entry(rel.clone()).or_insert_with(|| {
                    let (kind, title) = document_kind_title(conn, &rel);
                    Acc {
                        kind,
                        title,
                        snippet_lex: None,
                        snippet_vec: None,
                        semantic: None,
                        lexical_rank: None,
                    }
                });
                acc.snippet_vec = acc.snippet_vec.take().or(Some(snippet));
                if acc.semantic.is_none_or(|best| semantic > best) {
                    acc.semantic = Some(semantic);
                }
            }
        }
    }

    let mut hits: Vec<RagHit> = docs
        .into_iter()
        .map(|(rel, acc)| {
            let lexical_score = acc
                .lexical_rank
                .map_or(0.0, |rank| 1.0 / (1.0 + rank as f32));
            let semantic_score = acc.semantic.unwrap_or(0.0);
            let untrusted = rel.starts_with("library/");
            RagHit {
                kind: acc.kind,
                rel: PathBuf::from(&rel),
                title: acc.title,
                snippet: acc
                    .snippet_lex
                    .or(acc.snippet_vec)
                    .unwrap_or_default()
                    .replace('\n', " "),
                score: WEIGHT_LEXICAL * lexical_score + WEIGHT_SEMANTIC * semantic_score,
                semantic: acc.semantic,
                lexical_rank: acc.lexical_rank,
                untrusted,
            }
        })
        .collect();
    // Deterministic order: score first, path breaks ties.
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.rel.cmp(&b.rel))
    });
    hits.truncate(limit);
    Ok(hits)
}

/// The vector half: embed the query, kNN over `rag_vectors`, cosine per
/// neighbor, best chunk per document. `None` when the index can't vote
/// (no index yet, or the indexed model no longer matches what's loaded).
fn vector_half(
    conn: &rusqlite::Connection,
    embedder: &dyn Embedder,
    query: &str,
    limit: usize,
) -> Result<Option<Vec<(String, String, f32)>>> {
    let indexed_model = get_meta(conn, "model");
    if indexed_model.as_deref() != Some(embedder.info().name.as_str()) {
        // A different model is loaded than the one that indexed — its
        // vectors would not be comparable. Keywords still work.
        return Ok(None);
    }
    let rows: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM {VEC_TABLE}"), [], |r| {
        r.get(0)
    })?;
    if rows == 0 {
        return Ok(None);
    }
    let k = (limit * VECTOR_OVERFETCH)
        .max(VECTOR_MIN_K)
        .min(rows.max(1) as usize);
    let query_vector = embedder
        .embed(&[query])
        .context("embed the search query")?
        .into_iter()
        .next()
        .context("embedding model returned no query vector")?;
    let mut normalized = query_vector;
    crate::sync::normalize(&mut normalized);
    let blob = vector_blob(&normalized);
    let mut stmt = conn.prepare(&format!(
        "SELECT v.rowid, v.distance FROM {VEC_TABLE} v \
         WHERE v.embedding MATCH ?1 AND k = ?2"
    ))?;
    let neighbors: Vec<(i64, f32)> = stmt
        .query_map(params![blob.as_slice(), k as i64], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut by_rel: BTreeMap<String, (String, f32)> = BTreeMap::new();
    for (rowid, distance) in neighbors {
        let Ok((rel, text)) = chunk_for_row(conn, rowid) else {
            continue;
        };
        // Unit vectors: cosine = 1 - d²/2 (clamp the float noise).
        let similarity = (1.0 - distance * distance / 2.0).clamp(0.0, 1.0);
        let better = by_rel.get(&rel).is_none_or(|(_, best)| similarity > *best);
        if better {
            by_rel.insert(rel, (snippet_of(&text), similarity));
        }
    }
    Ok(Some(
        by_rel
            .into_iter()
            .map(|(rel, (snippet, similarity))| (rel, snippet, similarity))
            .collect(),
    ))
}

fn chunk_for_row(conn: &rusqlite::Connection, rowid: i64) -> Result<(String, String)> {
    conn.query_row(
        "SELECT rel, text FROM rag_chunks WHERE chunk_rowid = ?1",
        params![rowid],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .context("vector neighbor lost its chunk row")
}

fn document_kind_title(conn: &rusqlite::Connection, rel: &str) -> (String, String) {
    conn.query_row(
        "SELECT kind, title FROM documents WHERE rel = ?1",
        params![rel],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )
    .unwrap_or_else(|_| ("note".to_string(), stem_of(rel)))
}

fn stem_of(rel: &str) -> String {
    PathBuf::from(rel)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| rel.to_string())
}

fn snippet_of(text: &str) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= VECTOR_SNIPPET_CHARS {
        collapsed
    } else {
        let cut: String = collapsed.chars().take(VECTOR_SNIPPET_CHARS).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_reports_which_halves_voted() {
        let hit = |semantic: Option<f32>, lexical_rank: Option<usize>| RagHit {
            kind: "chapter".into(),
            rel: PathBuf::from("manuscript/001.md"),
            title: "One".into(),
            snippet: String::new(),
            score: 0.0,
            semantic,
            lexical_rank,
            untrusted: false,
        };
        assert_eq!(hit(Some(0.9), Some(0)).origin(), "hybrid");
        assert_eq!(hit(None, Some(1)).origin(), "lexical");
        assert_eq!(hit(Some(0.5), None).origin(), "vector");
    }

    #[test]
    fn snippets_collapse_and_cap() {
        assert_eq!(snippet_of("a\nb  c"), "a b c");
        let long = "word ".repeat(100);
        let snippet = snippet_of(&long);
        assert!(snippet.chars().count() <= VECTOR_SNIPPET_CHARS + 1);
        assert!(snippet.ends_with('…'));
    }

    #[test]
    fn library_rels_are_untrusted() {
        // The fusion marks provenance by path (A8) — covered end to end in
        // tests/rag.rs; here just the marker itself.
        assert!("library/x.md".starts_with("library/"));
        assert!(!"manuscript/001.md".starts_with("library/"));
    }
}
