//! Rebuildable cache database (`.oca/cache/oca.db`): SQLite + FTS5.
//!
//! Files stay canonical (AGENTS rule 2): every row here is derived from
//! Markdown/YAML on disk and can be recreated byte-for-byte by
//! `oca reindex` — the "delete DB → reindex → identical results" test is
//! the contract. The DB is the *only* place full-text search, tags,
//! links/backlinks, and the audio-asset registry are answered from, so
//! editing stays fast on a 500k-word corpus (A10 budgets).
//!
//! Design notes (ADR-0023):
//! - One FTS5 table (`search_fts`, external content over `search_text`)
//!   covers every searchable *unit*: document bodies, song metadata, lyric
//!   text, timeline events, audio-take notes, comment sidecars.
//!   `search_units` maps FTS rowids back to `(kind, ref, rel, title)`.
//! - Schema evolves through the `MIGRATIONS` list below; `open` applies
//!   pending ones. Tables for Phase 6/7 artifacts (snapshots, proposals,
//!   AI operations) arrive with those phases' migrations.
//! - Reconcile is mtime+size based: external edits show up in search
//!   without a full rebuild (`oca reindex` only forces what changed).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde_yaml::Value;

use crate::layout::COMMENT_SUFFIX;
use oca_model::Frontmatter;

/// DB file under the (deletable) cache dir.
pub const DB_REL: &str = ".oca/cache/oca.db";

/// Pending schema migrations, applied in order at open. Each runs inside
/// its own transaction and is recorded in `schema_migrations`. Tables for
/// Phase 6/7 artifacts (snapshots, proposals, AI operations, embeddings)
/// arrive with those phases.
const MIGRATIONS: &[(i64, &str, &str)] = &[(1, "core index", "
CREATE TABLE documents(
  rel TEXT PRIMARY KEY,
  id TEXT,
  kind TEXT NOT NULL,
  title TEXT NOT NULL,
  seq INTEGER,
  body TEXT NOT NULL,
  word_count INTEGER NOT NULL,
  mtime_ns INTEGER NOT NULL,
  size INTEGER NOT NULL
);
CREATE INDEX documents_id ON documents(id);

CREATE TABLE search_units(
  unit_rowid INTEGER PRIMARY KEY,
  kind TEXT NOT NULL,
  ref TEXT NOT NULL DEFAULT '',
  rel TEXT NOT NULL,
  title TEXT NOT NULL
);
CREATE INDEX search_units_rel ON search_units(rel);
CREATE INDEX search_units_ref ON search_units(ref);

-- Backing text for the FTS table (external content): snippet()/highlight()
-- need the originals, and DELETE FROM search_fts re-reads them from here.
CREATE TABLE search_text(
  unit_rowid INTEGER PRIMARY KEY,
  title TEXT,
  body TEXT
);
CREATE VIRTUAL TABLE search_fts USING fts5(title, body, content='search_text', content_rowid='unit_rowid');

CREATE TABLE document_tags(
  rel TEXT NOT NULL,
  tag TEXT NOT NULL,
  PRIMARY KEY(rel, tag)
);
CREATE INDEX document_tags_tag ON document_tags(tag);

CREATE TABLE document_links(
  from_id TEXT NOT NULL,
  to_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  from_rel TEXT NOT NULL,
  PRIMARY KEY(from_id, to_id, kind)
);
CREATE INDEX document_links_to ON document_links(to_id);

CREATE TABLE characters(
  rel TEXT PRIMARY KEY,
  id TEXT,
  name TEXT,
  aliases TEXT,
  role TEXT
);
CREATE TABLE character_states(
  rel TEXT NOT NULL,
  at TEXT NOT NULL,
  state TEXT NOT NULL,
  PRIMARY KEY(rel, at)
);
CREATE TABLE relationships(
  from_id TEXT NOT NULL,
  to_id TEXT NOT NULL,
  type TEXT,
  source_rel TEXT NOT NULL,
  PRIMARY KEY(from_id, to_id)
);
CREATE TABLE locations(
  rel TEXT PRIMARY KEY,
  id TEXT,
  name TEXT
);
CREATE TABLE timeline_events(
  id TEXT PRIMARY KEY,
  title TEXT,
  story_time TEXT,
  chapter_id TEXT,
  characters TEXT,
  summary TEXT,
  source_rel TEXT NOT NULL
);
CREATE TABLE songs(
  rel TEXT PRIMARY KEY,
  id TEXT,
  title TEXT,
  artist TEXT,
  language TEXT,
  tempo INTEGER,
  key_name TEXT,
  themes TEXT
);
CREATE TABLE song_sections(
  song_rel TEXT NOT NULL,
  idx INTEGER NOT NULL,
  heading TEXT,
  body TEXT,
  PRIMARY KEY(song_rel, idx)
);
CREATE TABLE audio_assets(
  take_id TEXT PRIMARY KEY,
  rel TEXT NOT NULL UNIQUE,
  title TEXT,
  duration_ms INTEGER,
  sample_rate INTEGER,
  channels INTEGER,
  checksum TEXT,
  attach_kind TEXT,
  attach_id TEXT,
  note TEXT,
  mtime_ns INTEGER NOT NULL,
  size INTEGER NOT NULL
);
CREATE TABLE annotations(
  rel TEXT PRIMARY KEY,
  id TEXT,
  body TEXT
);
")];

/// One search hit (FTS or tag query).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// Unit kind: chapter, character, world, outline, lyric, song, audio,
    /// event, comment, note.
    pub kind: String,
    /// Immutable id when the source has one, else empty.
    pub ref_id: String,
    pub rel: PathBuf,
    pub title: String,
    /// FTS snippet with ‹…› around matches (empty for tag queries).
    pub snippet: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagCount {
    pub tag: String,
    pub uses: i64,
}

/// One end of a reference: the document a link points from/to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkEnd {
    pub kind: String,
    pub id: String,
    pub rel: PathBuf,
    pub title: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkReport {
    /// References this document makes (`0` = link kind, `1` = the target).
    pub outgoing: Vec<(String, LinkEnd)>,
    /// References pointing *at* this document (`0` = link kind, `1` = the
    /// source).
    pub incoming: Vec<(String, LinkEnd)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReindexStats {
    /// Files (re)indexed this pass.
    pub updated: usize,
    /// Rows for files no longer on disk.
    pub removed: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Totals {
    pub documents: i64,
    pub units: i64,
    pub tags: i64,
    pub links: i64,
    pub takes: i64,
    pub songs: i64,
    pub characters: i64,
    pub events: i64,
}

/// The cache DB for one project. Never canonical: delete the file and the
/// next open rebuilds it.
pub struct Db {
    conn: Connection,
}

impl Db {
    /// Open (creating + migrating) `.oca/cache/oca.db` under `root`. A
    /// corrupt or unreadable DB is deleted and recreated — the manuscript
    /// never depends on this file.
    pub fn open(root: &Path) -> Result<Db> {
        let path = root.join(DB_REL);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        match Self::connect(&path) {
            Ok(db) => Ok(db),
            Err(first) => {
                // Self-heal: the cache is disposable, the files are not.
                for suffix in ["", "-wal", "-shm"] {
                    let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
                }
                Self::connect(&path).with_context(|| {
                    format!(
                        "recreate cache index at {} (first attempt: {first})",
                        path.display()
                    )
                })
            }
        }
    }

    fn connect(path: &Path) -> Result<Db> {
        let conn = Connection::open(path)
            .with_context(|| format!("open cache index {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_millis(2_000))?;
        // WAL keeps search responsive during saves; best-effort (some
        // filesystems refuse it and plain rollback journals are fine).
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        migrate(&conn)?;
        Ok(Db { conn })
    }

    /// Full rebuild from the filesystem (what `oca reindex --full` and the
    /// first open of a project without a DB run). Deterministic: same
    /// files in → same rows out.
    pub fn reindex_all(&mut self, root: &Path) -> Result<ReindexStats> {
        let tx = self.conn.unchecked_transaction()?;
        for table in [
            "search_fts",
            "search_text",
            "search_units",
            "document_tags",
            "document_links",
            "documents",
            "characters",
            "character_states",
            "relationships",
            "locations",
            "timeline_events",
            "songs",
            "song_sections",
            "audio_assets",
            "annotations",
        ] {
            tx.execute_batch(&format!("DELETE FROM {table}"))?;
        }
        let stats = index_tree(&tx, root, true)?;
        tx.commit()?;
        Ok(stats)
    }

    /// Incremental pass: reindex files whose (mtime, size) changed or that
    /// are new; drop rows for files that vanished. Cheap when nothing
    /// changed (a stat walk, no parsing) — this is the "watcher": external
    /// edits reconcile on the next open, save, or `oca reindex`.
    pub fn reconcile(&mut self, root: &Path) -> Result<ReindexStats> {
        let tx = self.conn.unchecked_transaction()?;
        let stats = index_tree(&tx, root, false)?;
        tx.commit()?;
        Ok(stats)
    }

    /// Index exactly one file (the save path). Song dirs reindex as a unit
    /// (song.yaml + lyrics.md change together).
    pub fn index_file(&mut self, root: &Path, rel: &Path) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        if let Some(dir) = song_dir_of(rel) {
            index_song_dir(&tx, root, &dir)?;
        } else {
            index_one(&tx, root, rel)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Full-text search (FTS5) or, for `tag:ns:value` queries, a tag
    /// lookup. Returns at most `limit` hits, best match first.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Hit>> {
        let query = query.trim();
        if let Some(tag) = query.strip_prefix("tag:") {
            return self.search_tag(tag, limit);
        }
        let matched = fts_query(query);
        if matched.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT u.kind, u.ref, u.rel, u.title, \
                    snippet(search_fts, 1, '‹', '›', '…', 14) \
             FROM search_fts JOIN search_units u ON u.unit_rowid = search_fts.rowid \
             WHERE search_fts MATCH ?1 \
             ORDER BY rank LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![matched, limit as i64], |row| {
            Ok(Hit {
                kind: row.get(0)?,
                ref_id: row.get(1)?,
                rel: PathBuf::from(row.get::<_, String>(2)?),
                title: row.get(3)?,
                snippet: row.get::<_, String>(4).unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    fn search_tag(&self, tag: &str, limit: usize) -> Result<Vec<Hit>> {
        let mut stmt = self.conn.prepare(
            "SELECT u.kind, u.ref, u.rel, u.title \
             FROM document_tags t JOIN search_units u ON u.rel = t.rel \
             WHERE LOWER(t.tag) = LOWER(?1) \
             ORDER BY u.rel LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![tag, limit as i64], |row| {
            Ok(Hit {
                kind: row.get(0)?,
                ref_id: row.get(1)?,
                rel: PathBuf::from(row.get::<_, String>(2)?),
                title: row.get(3)?,
                snippet: String::new(),
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Every tag in use, with document counts (`oca tags`).
    pub fn tags(&self) -> Result<Vec<TagCount>> {
        let mut stmt = self
            .conn
            .prepare("SELECT tag, COUNT(*) FROM document_tags GROUP BY tag ORDER BY tag")?;
        let rows = stmt.query_map([], |row| {
            Ok(TagCount {
                tag: row.get(0)?,
                uses: row.get(1)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// References this document makes, by id (outgoing) and references
    /// pointing at it (incoming) — one report, two directions.
    pub fn links(&self, id: &str) -> Result<LinkReport> {
        let mut stmt = self.conn.prepare(
            "SELECT document_links.kind, to_id, \
                    COALESCE(u.kind, 'unresolved'), COALESCE(u.ref, ''), \
                    COALESCE(u.rel, ''), COALESCE(u.title, '') \
             FROM document_links LEFT JOIN search_units u ON u.ref = to_id \
             WHERE from_id = ?1 \
             GROUP BY document_links.kind, to_id \
             ORDER BY to_id",
        )?;
        let rows = stmt.query_map(params![id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                end_of(row.get(1)?, row.get(2)?, row.get(4)?, row.get(5)?),
            ))
        })?;
        let mut report = LinkReport {
            outgoing: rows.collect::<std::result::Result<Vec<_>, _>>()?,
            incoming: Vec::new(),
        };
        report.incoming = self.backlinks(id)?;
        Ok(report)
    }

    /// References pointing at this document (backlinks), by id. Sources
    /// are joined by `rel` across *both* registries — `.md` documents and
    /// `song.yaml` — so a song's links still backlink to a chapter.
    pub fn backlinks(&self, id: &str) -> Result<Vec<(String, LinkEnd)>> {
        let mut stmt = self.conn.prepare(
            "SELECT document_links.kind, from_id, \
                    COALESCE(d.kind, u.kind, 'link'), from_rel, \
                    COALESCE(d.title, u.title, '') \
             FROM document_links \
             LEFT JOIN documents d ON d.rel = document_links.from_rel \
             LEFT JOIN search_units u ON u.rel = document_links.from_rel \
             WHERE to_id = ?1 \
             GROUP BY document_links.kind, from_id \
             ORDER BY from_rel",
        )?;
        let rows = stmt.query_map(params![id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                end_of(row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?),
            ))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// The first search unit carrying this immutable id. Songs, takes, and
    /// events aren't `.md` documents, so the in-memory file index can't
    /// resolve their ids — `oca links` falls back to here.
    pub fn unit_by_ref(&self, id: &str) -> Result<Option<(PathBuf, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT rel, title, kind FROM search_units \
             WHERE ref = ?1 ORDER BY unit_rowid LIMIT 1",
        )?;
        let mut rows = stmt.query(params![id])?;
        Ok(if let Some(row) = rows.next()? {
            Some((
                PathBuf::from(row.get::<_, String>(0)?),
                row.get(1)?,
                row.get(2)?,
            ))
        } else {
            None
        })
    }

    /// Row counts for `oca reindex` / `doctor` output.
    pub fn totals(&self) -> Result<Totals> {
        let count = |table: &str| -> Result<i64> {
            Ok(self
                .conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?)
        };
        Ok(Totals {
            documents: count("documents")?,
            units: count("search_units")?,
            tags: count("document_tags")?,
            links: count("document_links")?,
            takes: count("audio_assets")?,
            songs: count("songs")?,
            characters: count("characters")?,
            events: count("timeline_events")?,
        })
    }
}

fn end_of(id: String, kind: String, rel: String, title: String) -> LinkEnd {
    LinkEnd {
        kind,
        id,
        rel: PathBuf::from(rel),
        title,
    }
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations(\
           version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)",
    )?;
    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |r| r.get(0),
    )?;
    for (version, name, sql) in MIGRATIONS {
        if *version > current {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            tx.execute(
                "INSERT INTO schema_migrations(version, applied_at) \
                 VALUES (?1, datetime('now'))",
                params![version],
            )?;
            tx.commit()?;
            tracing::debug!(
                version = *version,
                name = *name,
                "cache index migration applied"
            );
        }
    }
    Ok(())
}

/// Turn a user query into a safe FTS5 MATCH expression: each whitespace
/// term becomes a quoted phrase (so `AND`, `*`, `-`, `"` in user text can
/// never be syntax), all terms must match, and the last term matches as a
/// prefix (incremental-search UX). Returns "" for queries with no terms.
fn fts_query(query: &str) -> String {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    match terms.len() {
        0 => String::new(),
        1 => format!("{}*", terms[0]),
        _ => {
            let (last, rest) = terms.split_last().unwrap();
            format!("{} {}*", rest.join(" "), last)
        }
    }
}

/// Every indexable file under `root` (hidden dirs skipped): Markdown
/// documents, comment sidecars, `song.yaml`, and audio-take sidecars under
/// `audio/` or any `demos/` dir.
fn index_tree(conn: &Connection, root: &Path, force: bool) -> Result<ReindexStats> {
    let mut files = Vec::new();
    walk_files(root, root, &mut files)?;
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut song_dirs: HashSet<PathBuf> = HashSet::new();
    let mut stats = ReindexStats::default();

    for rel in &files {
        if !candidate(rel) {
            continue;
        }
        seen.insert(rel.clone());
        if let Some(dir) = song_dir_of(rel) {
            // Song dirs reindex as a unit when either file changed.
            if !song_dirs.insert(dir.clone()) {
                continue;
            }
        } else if !force && !changed(conn, root, rel)? {
            continue;
        }
        let path = root.join(rel);
        match index_path(conn, root, rel, &path) {
            Ok(()) => stats.updated += 1,
            // A malformed sidecar or unreadable file must not fail the
            // whole pass; it just doesn't contribute rows.
            Err(err) => {
                tracing::warn!(rel = %rel.display(), error = %err, "skip unindexable file")
            }
        }
    }

    // Rows whose files are gone: drop them.
    let mut gone: HashSet<PathBuf> = HashSet::new();
    {
        let mut stmt =
            conn.prepare("SELECT rel FROM search_units UNION SELECT rel FROM audio_assets")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        for row in rows {
            let rel = PathBuf::from(row?);
            if !seen.contains(&rel) {
                gone.insert(rel);
            }
        }
    }
    for rel in gone {
        clear_rel(conn, &rel)?;
        stats.removed += 1;
    }
    Ok(stats)
}

fn candidate(rel: &Path) -> bool {
    let name = rel.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.ends_with(".md") {
        return true;
    }
    if name == "song.yaml" && rel.starts_with("music/songs") {
        return true;
    }
    name.ends_with(".yaml")
        && (rel.starts_with("audio")
            || rel
                .components()
                .any(|c| c.as_os_str().to_str() == Some("demos")))
}

fn walk_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if !name.starts_with('.') {
                walk_files(root, &path, out)?;
            }
            continue;
        }
        if let Ok(rel) = path.strip_prefix(root) {
            out.push(rel.to_path_buf());
        }
    }
    Ok(())
}

fn mtime_ns_and_size(path: &Path) -> Result<(i64, i64)> {
    let meta = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    Ok((mtime, meta.len() as i64))
}

fn changed(conn: &Connection, root: &Path, rel: &Path) -> Result<bool> {
    let (mtime, size) = mtime_ns_and_size(&root.join(rel))?;
    let rel = rel.to_string_lossy().to_string();
    let stored: Option<(i64, i64)> = conn
        .query_row(
            "SELECT mtime_ns, size FROM documents WHERE rel = ?1",
            params![rel.clone()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()
        .or_else(|| {
            conn.query_row(
                "SELECT mtime_ns, size FROM audio_assets WHERE rel = ?1",
                params![rel],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()
        });
    Ok(stored != Some((mtime, size)))
}

/// `music/songs/<slug>/song.yaml` and `…/lyrics.md` belong to one song
/// directory (indexed together).
fn song_dir_of(rel: &Path) -> Option<PathBuf> {
    let name = rel.file_name()?.to_str()?;
    if name != "song.yaml" && name != "lyrics.md" {
        return None;
    }
    let dir = rel.parent()?;
    dir.starts_with("music/songs").then(|| dir.to_path_buf())
}

fn index_path(conn: &Connection, root: &Path, rel: &Path, _path: &Path) -> Result<()> {
    if let Some(dir) = song_dir_of(rel) {
        return index_song_dir(conn, root, &dir);
    }
    index_one(conn, root, rel)
}

/// Remove every row derived from `rel` (its file is going away or being
/// reindexed from scratch).
fn clear_rel(conn: &Connection, rel: &Path) -> Result<()> {
    let rel = rel.to_string_lossy().to_string();
    // Unit rows must go with the FTS rows that share their rowids.
    let rowids: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT unit_rowid FROM search_units WHERE rel = ?1")?;
        let rows = stmt.query_map(params![rel.clone()], |r| r.get(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for rowid in rowids {
        conn.execute("DELETE FROM search_fts WHERE rowid = ?1", params![rowid])?;
    }
    conn.execute(
        "DELETE FROM search_text WHERE unit_rowid IN \
         (SELECT unit_rowid FROM search_units WHERE rel = ?1)",
        params![rel.clone()],
    )?;
    conn.execute(
        "DELETE FROM search_units WHERE rel = ?1",
        params![rel.clone()],
    )?;
    for table in [
        "documents",
        "document_tags",
        "characters",
        "character_states",
        "locations",
        "songs",
        "audio_assets",
        "annotations",
    ] {
        conn.execute(
            &format!("DELETE FROM {table} WHERE rel = ?1"),
            params![rel.clone()],
        )?;
    }
    conn.execute(
        "DELETE FROM timeline_events WHERE source_rel = ?1",
        params![rel.clone()],
    )?;
    conn.execute(
        "DELETE FROM document_links WHERE from_rel = ?1",
        params![rel.clone()],
    )?;
    conn.execute(
        "DELETE FROM relationships WHERE source_rel = ?1",
        params![rel],
    )?;
    Ok(())
}

fn insert_unit(
    conn: &Connection,
    kind: &str,
    ref_id: &str,
    rel: &Path,
    title: &str,
    body: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO search_units(kind, ref, rel, title) VALUES (?1, ?2, ?3, ?4)",
        params![kind, ref_id, rel.to_string_lossy().to_string(), title],
    )?;
    let rowid = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO search_text(unit_rowid, title, body) VALUES (?1, ?2, ?3)",
        params![rowid, title, body],
    )?;
    conn.execute(
        "INSERT INTO search_fts(rowid, title, body) VALUES (?1, ?2, ?3)",
        params![rowid, title, body],
    )?;
    Ok(())
}

/// Index one file by type: Markdown document (with per-dir extras), audio
/// take sidecar, or ignore.
fn index_one(conn: &Connection, root: &Path, rel: &Path) -> Result<()> {
    let name = rel.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let path = root.join(rel);
    if name.ends_with(".yaml") {
        return index_take(conn, rel, &path);
    }
    if !name.ends_with(".md") {
        return Ok(());
    }
    index_doc(conn, rel, &path)
}

fn index_doc(conn: &Connection, rel: &Path, path: &Path) -> Result<()> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let doc = oca_markdown::parse(&raw);
    let fm = &doc.frontmatter;
    let stem = rel
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let is_comment = rel
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(COMMENT_SUFFIX));
    let id = fm.get_str("id").unwrap_or_default();
    let title = fm
        .get_str("title")
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| stem.clone());
    let body = doc.body().to_string();
    let (mtime, size) = mtime_ns_and_size(path)?;
    let rel_s = rel.to_string_lossy().to_string();
    clear_rel(conn, rel)?;

    let kind = if is_comment { "comment" } else { kind_for(rel) };
    let seq = crate::index::leading_number(&stem);
    conn.execute(
        "INSERT INTO documents(rel, id, kind, title, seq, body, word_count, mtime_ns, size) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            rel_s,
            id,
            kind,
            title,
            seq.map(|s| s as i64),
            body,
            body.split_whitespace().count() as i64,
            mtime,
            size
        ],
    )?;
    insert_unit(conn, kind, &id, rel, &title, &body)?;

    if is_comment {
        conn.execute(
            "INSERT OR REPLACE INTO annotations(rel, id, body) VALUES (?1, ?2, ?3)",
            params![rel_s, id, body],
        )?;
        return Ok(());
    }

    // Tags (`namespace:value`, §4.5) — authored in frontmatter, never
    // invented here.
    for tag in fm.get_seq("tags").unwrap_or_default() {
        conn.execute(
            "INSERT OR IGNORE INTO document_tags(rel, tag) VALUES (?1, ?2)",
            params![rel_s, tag],
        )?;
    }

    // References: frontmatter `links:` mapping (ids, §4.2) + body
    // [[wiki-links]]. Files are canonical; this is an index copy.
    index_frontmatter_links(conn, rel, &id, fm)?;
    for wiki in &doc.links {
        if wiki.target.is_empty() || id.is_empty() {
            continue;
        }
        conn.execute(
            "INSERT OR IGNORE INTO document_links(from_id, to_id, kind, from_rel) \
             VALUES (?1, ?2, 'wiki', ?3)",
            params![id, wiki.target, rel_s],
        )?;
    }

    match rel.components().next().and_then(|c| c.as_os_str().to_str()) {
        Some("characters") => index_character(conn, rel, &id, fm)?,
        Some("outline") if rel.file_name().and_then(|n| n.to_str()) == Some("timeline.md") => {
            index_timeline(conn, rel, fm)?
        }
        Some("world") => {
            conn.execute(
                "INSERT OR REPLACE INTO locations(rel, id, name) VALUES (?1, ?2, ?3)",
                params![rel_s, id, title],
            )?;
        }
        _ => {}
    }
    Ok(())
}

/// User-facing unit kind from the top-level directory.
fn kind_for(rel: &Path) -> &'static str {
    match rel.components().next().and_then(|c| c.as_os_str().to_str()) {
        Some("manuscript") => "chapter",
        Some("characters") => "character",
        Some("world") => "world",
        Some("outline") => "outline",
        Some("music") => "lyric",
        Some("notes") => "note",
        _ => "note",
    }
}

fn index_frontmatter_links(
    conn: &Connection,
    rel: &Path,
    id: &str,
    fm: &Frontmatter,
) -> Result<()> {
    if id.is_empty() {
        return Ok(());
    }
    let Some(Value::Mapping(map)) = fm.get_value("links") else {
        return Ok(());
    };
    for (kind, targets) in map {
        let Some(kind) = kind.as_str() else { continue };
        let Some(targets) = targets.as_sequence() else {
            continue;
        };
        for target in targets.iter().filter_map(Value::as_str) {
            conn.execute(
                "INSERT OR IGNORE INTO document_links(from_id, to_id, kind, from_rel) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![id, target, kind, rel.to_string_lossy().to_string()],
            )?;
        }
    }
    Ok(())
}

fn index_character(conn: &Connection, rel: &Path, id: &str, fm: &Frontmatter) -> Result<()> {
    let name = fm
        .get_str("name")
        .or_else(|| fm.get_str("title"))
        .unwrap_or_default();
    let aliases = fm.get_seq("aliases").unwrap_or_default().join("\n");
    conn.execute(
        "INSERT OR REPLACE INTO characters(rel, id, name, aliases, role) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            rel.to_string_lossy().to_string(),
            id,
            name,
            aliases,
            fm.get_str("role")
        ],
    )?;
    if let Some(Value::Sequence(states)) = fm.get_value("states") {
        for state in states {
            let at = state.get("at").and_then(Value::as_str).unwrap_or_default();
            if at.is_empty() {
                continue;
            }
            // Whole state mapping (minus `at`) as JSON — queryable later
            // without owning story semantics here.
            let mut map = state.clone();
            if let Value::Mapping(m) = &mut map {
                m.remove(Value::String("at".into()));
            }
            let json = serde_json::to_string(&map).unwrap_or_default();
            conn.execute(
                "INSERT OR REPLACE INTO character_states(rel, at, state) VALUES (?1, ?2, ?3)",
                params![rel.to_string_lossy().to_string(), at, json],
            )?;
        }
    }
    if !id.is_empty()
        && let Some(Value::Mapping(rels)) = fm.get_value("relationships")
    {
        for (other, spec) in rels {
            let Some(other) = other.as_str() else {
                continue;
            };
            let ty = spec.get("type").and_then(Value::as_str);
            conn.execute(
                "INSERT OR REPLACE INTO relationships(from_id, to_id, type, source_rel) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![id, other, ty, rel.to_string_lossy().to_string()],
            )?;
        }
    }
    Ok(())
}

fn index_timeline(conn: &Connection, rel: &Path, fm: &Frontmatter) -> Result<()> {
    let Some(Value::Sequence(events)) = fm.get_value("events") else {
        return Ok(());
    };
    for event in events {
        let id = event.get("id").and_then(Value::as_str).unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        let title = event
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let story_time = event.get("story_time").and_then(Value::as_str);
        let chapter_id = event.get("chapter").and_then(Value::as_str);
        let summary = event
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let characters = event
            .get("characters")
            .and_then(Value::as_sequence)
            .map(|seq| {
                seq.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        conn.execute(
            "INSERT OR REPLACE INTO timeline_events(\
               id, title, story_time, chapter_id, characters, summary, source_rel) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                title,
                story_time,
                chapter_id,
                characters,
                summary,
                rel.to_string_lossy().to_string()
            ],
        )?;
        // Events live in frontmatter, not the body — give each one its own
        // searchable unit so cross-entity search finds them.
        insert_unit(
            conn,
            "event",
            id,
            rel,
            title,
            &format!("{title}\n{summary}"),
        )?;
    }
    Ok(())
}

/// Index a song directory as a unit: `song.yaml` (metadata row + unit +
/// links) and `lyrics.md` (document + lyric unit + section rows).
fn index_song_dir(conn: &Connection, root: &Path, dir: &Path) -> Result<()> {
    let yaml_rel = dir.join("song.yaml");
    let lyrics_rel = dir.join("lyrics.md");
    let mut song_id = String::new();
    let mut song_title = String::new();

    let yaml_path = root.join(&yaml_rel);
    if yaml_path.is_file() {
        let raw = std::fs::read_to_string(&yaml_path)?;
        let value: Value =
            serde_yaml::from_str(&raw).with_context(|| format!("parse {}", yaml_rel.display()))?;
        song_id = value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        song_title = value
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let artist = value
            .get("artist")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let language = value.get("language").and_then(Value::as_str);
        let tempo = value.get("tempo").and_then(Value::as_i64);
        let key_name = value.get("key").and_then(Value::as_str);
        let themes = seq_text(&value, "themes");
        let body_bits = [
            song_title.clone(),
            artist.to_string(),
            seq_text(&value, "genre"),
            seq_text(&value, "mood"),
            themes.clone(),
        ]
        .join("\n");

        clear_rel(conn, &yaml_rel)?;
        conn.execute(
            "INSERT INTO songs(rel, id, title, artist, language, tempo, key_name, themes) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                yaml_rel.to_string_lossy().to_string(),
                song_id,
                song_title,
                artist,
                language,
                tempo,
                key_name,
                themes
            ],
        )?;
        insert_unit(conn, "song", &song_id, &yaml_rel, &song_title, &body_bits)?;
        if !song_id.is_empty()
            && let Some(Value::Mapping(map)) = value.get("links")
        {
            for (kind, targets) in map {
                let Some(kind) = kind.as_str() else { continue };
                let Some(targets) = targets.as_sequence() else {
                    continue;
                };
                for target in targets.iter().filter_map(Value::as_str) {
                    conn.execute(
                        "INSERT OR IGNORE INTO document_links(from_id, to_id, kind, from_rel) \
                         VALUES (?1, ?2, ?3, ?4)",
                        params![
                            song_id,
                            target,
                            kind,
                            yaml_rel.to_string_lossy().to_string()
                        ],
                    )?;
                }
            }
        }
    }

    let lyrics_path = root.join(&lyrics_rel);
    if lyrics_path.is_file() {
        let raw = std::fs::read_to_string(&lyrics_path)?;
        let doc = oca_markdown::parse(&raw);
        let title = if song_title.is_empty() {
            lyrics_rel
                .parent()
                .and_then(|d| d.file_name())
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "lyrics".into())
        } else {
            song_title.clone()
        };
        clear_rel(conn, &lyrics_rel)?;
        let (mtime, size) = mtime_ns_and_size(&lyrics_path)?;
        let body = doc.body().to_string();
        conn.execute(
            "INSERT INTO documents(rel, id, kind, title, seq, body, word_count, mtime_ns, size) \
             VALUES (?1, ?2, 'lyric', ?3, NULL, ?4, ?5, ?6, ?7)",
            params![
                lyrics_rel.to_string_lossy().to_string(),
                song_id,
                title,
                body,
                body.split_whitespace().count() as i64,
                mtime,
                size
            ],
        )?;
        insert_unit(conn, "lyric", &song_id, &lyrics_rel, &title, &body)?;

        // `## Heading` sections (§4.3) — the structured copy lyric tooling
        // can grow on.
        let song_key = if yaml_path.is_file() {
            yaml_rel.to_string_lossy().to_string()
        } else {
            lyrics_rel.to_string_lossy().to_string()
        };
        let mut idx = 0i64;
        let mut heading = String::new();
        let mut text = String::new();
        for line in raw.lines() {
            if let Some(h) = line.strip_prefix("## ") {
                flush_section(conn, &song_key, idx, &heading, &text)?;
                idx += 1;
                heading = h.split(" {=").next().unwrap_or(h).trim().to_string();
                text.clear();
            } else if !heading.is_empty() {
                text.push_str(line);
                text.push('\n');
            }
        }
        flush_section(conn, &song_key, idx, &heading, &text)?;
    }
    Ok(())
}

fn flush_section(
    conn: &Connection,
    song_key: &str,
    idx: i64,
    heading: &str,
    text: &str,
) -> Result<()> {
    if heading.is_empty() {
        return Ok(());
    }
    conn.execute(
        "INSERT OR REPLACE INTO song_sections(song_rel, idx, heading, body) \
         VALUES (?1, ?2, ?3, ?4)",
        params![song_key, idx, heading, text],
    )?;
    Ok(())
}

fn seq_text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_sequence)
        .map(|seq| {
            seq.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// An audio take sidecar (`<stem>.yaml` next to `<stem>.wav`). The sidecar
/// stays canonical; this row is the index copy (Phase 5's "audio metadata
/// in the DB").
fn index_take(conn: &Connection, rel: &Path, path: &Path) -> Result<()> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let value: Value =
        serde_yaml::from_str(&raw).with_context(|| format!("parse {}", rel.display()))?;
    let Some(take_id) = value.get("take_id").and_then(Value::as_str) else {
        return Ok(()); // not a take sidecar
    };
    clear_rel(conn, rel)?;
    let (mtime, size) = mtime_ns_and_size(path)?;
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let duration_ms = value.get("duration_ms").and_then(Value::as_i64);
    let sample_rate = value.get("sample_rate").and_then(Value::as_i64);
    let channels = value.get("channels").and_then(Value::as_i64);
    let checksum = value.get("checksum").and_then(Value::as_str);
    let attached = value.get("attached");
    let attach_kind = attached.and_then(|a| a.get("kind")).and_then(Value::as_str);
    let attach_id = attached.and_then(|a| a.get("id")).and_then(Value::as_str);
    let note = value.get("note");
    let note_text = [
        note.and_then(|n| n.get("text"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        note.and_then(|n| n.get("lyrics"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        note.and_then(|n| n.get("tempo"))
            .and_then(Value::as_i64)
            .map(|n| n.to_string())
            .unwrap_or_default(),
    ]
    .join("\n");
    conn.execute(
        "INSERT OR REPLACE INTO audio_assets(\
           take_id, rel, title, duration_ms, sample_rate, channels, checksum, \
           attach_kind, attach_id, note, mtime_ns, size) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            take_id,
            rel.to_string_lossy().to_string(),
            title,
            duration_ms,
            sample_rate,
            channels,
            checksum,
            attach_kind,
            attach_id,
            note_text,
            mtime,
            size
        ],
    )?;
    let body = format!("{title}\n{note_text}");
    insert_unit(conn, "audio", take_id, rel, title, &body)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{PROJECT_FILE, scaffold};
    use tempfile::TempDir;

    fn project() -> TempDir {
        let tmp = TempDir::new().unwrap();
        scaffold(tmp.path()).unwrap();
        std::fs::write(
            tmp.path().join(PROJECT_FILE),
            "---\nid: proj_1\ntitle: T\n---\n",
        )
        .unwrap();
        tmp
    }

    fn write(root: &Path, rel: &str, raw: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, raw).unwrap();
    }

    #[test]
    fn migrations_apply_once_and_open_is_idempotent() {
        let tmp = project();
        let root = tmp.path();
        {
            let db = Db::open(root).unwrap();
            let v: i64 = db
                .conn
                .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(v, 1);
        }
        let _again = Db::open(root).unwrap(); // no error, no duplicate rows
        let db = Db::open(root).unwrap();
        let n: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn full_reindex_and_search_across_entities() {
        let tmp = project();
        let root = tmp.path();
        write(
            root,
            "manuscript/001-start.md",
            "---\nid: ch_1\ntitle: The Start\n---\nThe letter arrived at dawn.\n",
        );
        write(
            root,
            "characters/elena.md",
            "---\nid: elena\ntitle: Elena\nname: Elena\naliases: [Ellie]\n---\nElena is the protagonist.\n",
        );
        write(
            root,
            "music/songs/ashes/song.yaml",
            "id: song_ashes\ntitle: Ashes\nartist: X\nthemes: [loss]\nlinks:\n  chapters: [ch_1]\n",
        );
        write(
            root,
            "music/songs/ashes/lyrics.md",
            "# Ashes\n\n## Verse 1\nI thought the fire would end us.\n",
        );
        write(
            root,
            "outline/timeline.md",
            "---\nevents:\n  - id: ev_letter\n    title: The letter is found\n    story_time: '1888-03-01'\n    chapter: ch_1\n---\n",
        );
        write(
            root,
            "audio/memo.yaml",
            "take_id: take_1\ntitle: Voice memo\nduration_ms: 12\nnote:\n  text: about the letter\n",
        );
        write(
            root,
            "notes/todo.md",
            "---\ntags: [theme:loss]\n---\nRemember the letter.\n",
        );

        let mut db = Db::open(root).unwrap();
        let stats = db.reindex_all(root).unwrap();
        // 4 plain docs + 1 audio sidecar + 1 song dir (song.yaml +
        // lyrics.md index together).
        assert_eq!(stats.updated, 6);

        let hits = db.search("letter", 20).unwrap();
        let kinds: Vec<&str> = hits.iter().map(|h| h.kind.as_str()).collect();
        assert!(kinds.contains(&"chapter"), "{kinds:?}");
        assert!(kinds.contains(&"audio"), "{kinds:?}"); // note text
        assert!(kinds.contains(&"event"), "{kinds:?}"); // frontmatter event
        assert!(kinds.contains(&"note"), "{kinds:?}");
        assert!(
            hits.iter().any(|h| h.snippet.contains('‹')),
            "snippets mark matches"
        );

        // Song metadata + lyric text are both searchable.
        assert!(!db.search("ashes", 20).unwrap().is_empty());
        assert!(!db.search("fire", 20).unwrap().is_empty());

        // Tag query.
        let hits = db.search("tag:theme:loss", 20).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].rel, PathBuf::from("notes/todo.md"));

        // Links: frontmatter mapping from the song → chapter; backlinks.
        let report = db.links("song_ashes").unwrap();
        assert_eq!(report.outgoing.len(), 1);
        assert_eq!(report.outgoing[0].1.id, "ch_1");
        assert_eq!(report.outgoing[0].0, "chapters");
        assert_eq!(report.incoming.len(), 0);
        let report = db.links("ch_1").unwrap();
        assert_eq!(report.incoming.len(), 1);
        assert_eq!(report.incoming[0].1.id, "song_ashes");

        let totals = db.totals().unwrap();
        assert_eq!(totals.characters, 1);
        assert_eq!(totals.songs, 1);
        assert_eq!(totals.takes, 1);
        assert_eq!(totals.events, 1);
    }

    /// The rebuildability contract: deleting the DB and reindexing yields
    /// identical rows (AGENTS rule 2 — files are canonical).
    #[test]
    fn delete_db_reindex_identical_results() {
        let tmp = project();
        let root = tmp.path();
        write(
            root,
            "manuscript/001-start.md",
            "---\nid: ch_1\ntitle: Start\ntags: [draft]\n---\nBody words here.\n",
        );
        write(
            root,
            "characters/elena.md",
            "---\nid: elena\ntitle: Elena\naliases: [Ellie]\nstates:\n  - at: ch_1\n    location: village\nrelationships:\n  marcus:\n    type: distrust\n---\nElena.\n",
        );
        write(
            root,
            "outline/timeline.md",
            "---\nevents:\n  - id: ev_a\n    title: Event A\n    story_time: '1'\n    chapter: ch_1\n    characters: [elena]\n    summary: It begins.\n---\n",
        );
        write(
            root,
            "music/songs/ashes/song.yaml",
            "id: song_ashes\ntitle: Ashes\n",
        );
        write(root, "music/songs/ashes/lyrics.md", "## Verse\nrain\n");
        write(root, "audio/memo.yaml", "take_id: take_1\ntitle: Memo\n");

        fn dump(root: &Path) -> Vec<String> {
            let mut db = Db::open(root).unwrap();
            db.reindex_all(root).unwrap();
            let mut out = Vec::new();
            for table in [
                "documents ORDER BY rel",
                "search_units ORDER BY rel, kind, ref",
                "search_text ORDER BY unit_rowid",
                "document_tags ORDER BY rel, tag",
                "document_links ORDER BY from_id, to_id",
                "characters ORDER BY rel",
                "character_states ORDER BY rel, at",
                "relationships ORDER BY from_id, to_id",
                "timeline_events ORDER BY id",
                "songs ORDER BY rel",
                "song_sections ORDER BY song_rel, idx",
                "audio_assets ORDER BY take_id",
            ] {
                let mut stmt = db.conn.prepare(&format!("SELECT * FROM {table}")).unwrap();
                let cols = stmt.column_count();
                let rows = stmt.query_map([], |r| {
                    let mut vals = Vec::new();
                    for i in 0..cols {
                        let v: rusqlite::types::Value = r.get(i).unwrap();
                        vals.push(format!("{v:?}"));
                    }
                    Ok(vals.join("|"))
                });
                out.push(format!("== {table}"));
                for row in rows.unwrap() {
                    out.push(row.unwrap());
                }
            }
            // FTS text survives a rebuild too.
            for term in ["elena", "rain", "memo", "begins"] {
                out.push(format!(
                    "== search {term}: {:?}",
                    db.search(term, 50).unwrap()
                ));
            }
            out
        }

        let first = dump(root);
        // The user deletes the cache dir like any temp file.
        std::fs::remove_file(root.join(DB_REL)).unwrap();
        let _ = std::fs::remove_file(format!("{}-wal", root.join(DB_REL).display()));
        let _ = std::fs::remove_file(format!("{}-shm", root.join(DB_REL).display()));
        let second = dump(root);
        assert_eq!(first, second, "rebuild from files must reproduce the index");
    }

    /// External edits reconcile without a full rebuild; deletions drop rows.
    #[test]
    fn reconcile_picks_up_edits_and_removals() {
        let tmp = project();
        let root = tmp.path();
        write(
            root,
            "manuscript/001-start.md",
            "---\nid: ch_1\ntitle: Start\n---\nOriginal text.\n",
        );
        write(
            root,
            "notes/x.md",
            "---\nid: note_x\ntitle: X\n---\nSoon gone.\n",
        );
        let mut db = Db::open(root).unwrap();
        db.reindex_all(root).unwrap();
        assert_eq!(db.search("gone", 5).unwrap().len(), 1);

        // External edit (as if in another editor) + removal.
        std::thread::sleep(std::time::Duration::from_millis(10));
        write(
            root,
            "manuscript/001-start.md",
            "---\nid: ch_1\ntitle: Start\n---\nEdited text.\n",
        );
        std::fs::remove_file(root.join("notes/x.md")).unwrap();

        let stats = db.reconcile(root).unwrap();
        assert_eq!(stats.updated, 1, "only the changed file is reparsed");
        assert_eq!(stats.removed, 1);
        assert_eq!(db.search("gone", 5).unwrap().len(), 0);
        assert_eq!(db.search("edited", 5).unwrap().len(), 1);
        assert_eq!(db.totals().unwrap().documents, 1);
    }

    /// A corrupt DB file is recreated from the files on open.
    #[test]
    fn corrupt_db_is_recreated_on_open() {
        let tmp = project();
        let root = tmp.path();
        write(
            root,
            "manuscript/001-start.md",
            "---\nid: ch_1\ntitle: Start\n---\nWords.\n",
        );
        {
            let mut db = Db::open(root).unwrap();
            db.reindex_all(root).unwrap();
        }
        std::fs::write(root.join(DB_REL), "this is not a sqlite file at all").unwrap();
        let mut db = Db::open(root).unwrap(); // recreated, not fatal
        db.reindex_all(root).unwrap();
        assert_eq!(db.totals().unwrap().documents, 1, "rebuilt from files");
    }

    /// fts_query never lets user text become syntax.
    #[test]
    fn fts_queries_are_escaped_and_prefix_on_the_last_term() {
        assert_eq!(fts_query(""), "");
        assert_eq!(fts_query("   "), "");
        assert_eq!(fts_query("let"), "\"let\"*");
        assert_eq!(fts_query("the letter"), "\"the\" \"letter\"*");
        assert_eq!(
            fts_query("a \"b\" AND c-d"),
            "\"a\" \"\"\"b\"\"\" \"AND\" \"c-d\"*"
        );
    }

    /// Saving one file updates only that file's rows (A10 single-file
    /// budget path).
    #[test]
    fn index_file_touches_one_document() {
        let tmp = project();
        let root = tmp.path();
        write(
            root,
            "manuscript/001-start.md",
            "---\nid: ch_1\ntitle: Start\n---\nOne.\n",
        );
        write(
            root,
            "manuscript/002-next.md",
            "---\nid: ch_2\ntitle: Next\n---\nTwo.\n",
        );
        let mut db = Db::open(root).unwrap();
        db.reindex_all(root).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        write(
            root,
            "manuscript/001-start.md",
            "---\nid: ch_1\ntitle: Start\n---\nUpdated opening.\n",
        );
        db.index_file(root, Path::new("manuscript/001-start.md"))
            .unwrap();
        assert_eq!(db.search("updated", 5).unwrap().len(), 1);
        assert_eq!(
            db.search("two", 5).unwrap().len(),
            1,
            "other rows untouched"
        );
    }
}
