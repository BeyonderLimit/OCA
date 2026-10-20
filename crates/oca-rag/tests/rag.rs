//! Phase 11 end-to-end: a real project through Core, the real cache DB
//! with `sqlite-vec` linked in-process, and `MockEmbedder` vectors (rule
//! 6 — no test touches a real model). The exit criteria live here:
//! import → hybrid query returns ranked, deduplicated results; re-embed
//! after an edit is incremental; retrieval works with no generative LLM.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use oca_ai::{Embedder, MockEmbedder};
use oca_core::{Command, Core, Event};
use oca_model::{DocumentKind, ProjectKind};
use oca_rag::{Rag, hybrid_search, import_file, sync};
use tempfile::TempDir;

const BODY: &str = "\
## The Hall

Elena read the letter again by lamplight; the ink ran where the paper had been folded.

## After

Nobody spoke of it at supper.
";

fn fixture() -> (TempDir, Core) {
    let tmp = TempDir::new().unwrap();
    let mut core = Core::new();
    core.execute(Command::CreateProject {
        root: tmp.path().to_path_buf(),
        title: "Rag Story".into(),
        kind: ProjectKind::Novel,
    })
    .unwrap();
    let events = core
        .execute(Command::CreateDocument {
            kind: DocumentKind::Chapter,
            title: "The Letter".into(),
        })
        .unwrap();
    let Event::DocumentCreated { rel, .. } = &events[0] else {
        panic!("expected DocumentCreated: {events:?}");
    };
    core.save_body(rel, BODY).unwrap();
    core.reindex().unwrap();
    (tmp, core)
}

fn write_project_file(root: &Path, rel: &str, raw: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, raw).unwrap();
}

fn embedder() -> Arc<dyn Embedder> {
    Arc::new(MockEmbedder::new())
}

#[test]
fn import_sync_hybrid_query_ranks_and_dedupes() {
    let (tmp, mut core) = fixture();
    let root = tmp.path();

    // A reference outside the project, then import it into library/.
    let source = root.join("../harbor-law.md");
    let source = std::path::absolute(&source).unwrap();
    std::fs::write(
        &source,
        "# Harbor Law\n\nNo vessel leaves the harbor before the bell rings twice.\n",
    )
    .unwrap();
    let imported = import_file(root, &source).unwrap();
    assert_eq!(imported.rel, PathBuf::from("library/harbor-law.md"));
    core.reindex().unwrap();

    let embedder = embedder();
    let stats = sync(&core, embedder.as_ref()).unwrap();
    assert!(stats.embedded > 0, "{stats:?}");
    assert_eq!(stats.removed, 0);

    // The distinctive phrase ranks the reference first, once, untrusted.
    let hits = hybrid_search(&core, Some(embedder.as_ref()), "bell rings twice", 10).unwrap();
    assert!(!hits.is_empty(), "hybrid must return hits");
    let first = &hits[0];
    assert_eq!(first.rel, PathBuf::from("library/harbor-law.md"));
    assert_eq!(first.kind, "reference");
    assert!(first.untrusted, "imported material is untrusted (A8)");
    assert!(first.semantic.is_some(), "the vector half voted: {first:?}");
    let occurrences = hits
        .iter()
        .filter(|h| h.rel == *"library/harbor-law.md")
        .count();
    assert_eq!(occurrences, 1, "deduplicated by document: {hits:?}");
}

/// `tag:` queries stay metadata lookups even when vectors can vote —
/// lookalike prose must not pollute a tag result (regression guard: the
/// hybrid path once let the vector half answer `tag:` queries).
#[test]
fn tag_queries_never_let_vectors_vote() {
    let (_tmp, mut core) = fixture();
    let embedder = embedder();
    sync(&core, embedder.as_ref()).unwrap();
    let chapter = PathBuf::from("manuscript/001-the-letter.md");
    core.execute(Command::SetFrontmatter {
        doc: chapter.clone(),
        key: "tags".into(),
        // A newline routes the write through the block (sequence) path.
        value: "- character:elena\n".into(),
    })
    .unwrap();
    core.reindex().unwrap();

    let hits = hybrid_search(&core, Some(embedder.as_ref()), "tag:character:elena", 10).unwrap();
    assert_eq!(hits.len(), 1, "only the tagged document: {hits:?}");
    assert_eq!(hits[0].rel, chapter);
    assert!(hits[0].semantic.is_none(), "no vector vote on metadata");
}

#[test]
fn reembed_after_an_edit_is_incremental() {
    let (_tmp, mut core) = fixture();
    let embedder = Arc::new(MockEmbedder::new());

    let first = sync(&core, embedder.as_ref()).unwrap();
    assert!(first.embedded > 0);
    let total = first.chunks;
    assert_eq!(first.embedded + first.unchanged, total);

    // Nothing changed: nothing re-embeds (and the embedder proves it).
    let before = embedder.counters();
    let second = sync(&core, embedder.as_ref()).unwrap();
    assert_eq!(second.embedded, 0, "{second:?}");
    assert_eq!(second.unchanged, total, "{second:?}");
    assert_eq!(embedder.counters().texts, before.texts, "no embed calls");

    // Edit one chapter: only its chunks re-embed.
    let chapters: Vec<PathBuf> = core
        .documents()
        .iter()
        .filter(|d| d.rel.starts_with("manuscript"))
        .map(|d| d.rel.clone())
        .collect();
    let rel = &chapters[0];
    core.save_body(
        rel,
        &format!("{BODY}\n\nA new paragraph about the zephyrous wind.\n"),
    )
    .unwrap();
    core.reindex().unwrap();
    let third = sync(&core, embedder.as_ref()).unwrap();
    assert!(
        third.embedded >= 1,
        "the edited chapter re-embeds: {third:?}"
    );
    assert!(
        third.embedded < total,
        "only the changed chunks re-embed, not the whole file: {third:?}"
    );
    assert_eq!(third.unchanged, total - third.embedded, "{third:?}");
    assert_eq!(embedder.counters().texts - before.texts, third.embedded);

    // The fresh text is findable through the vectors now.
    let hits = hybrid_search(&core, Some(embedder.as_ref()), "zephyrous wind", 5).unwrap();
    assert!(
        hits.iter().any(|h| h.rel == *rel && h.semantic.is_some()),
        "edited chapter votes with its new vector: {hits:?}"
    );
}

#[test]
fn deleting_a_document_empties_its_index_rows() {
    let (tmp, mut core) = fixture();
    let root = tmp.path();
    write_project_file(
        root,
        "notes/scrap.md",
        "---\ntitle: Scrap\n---\nNotes about the lighthouse keeper.\n",
    );
    core.reindex().unwrap();
    let embedder = embedder();
    let stats = sync(&core, embedder.as_ref()).unwrap();
    assert!(stats.embedded >= 2);

    std::fs::remove_file(root.join("notes/scrap.md")).unwrap();
    core.reindex().unwrap();
    let stats = sync(&core, embedder.as_ref()).unwrap();
    assert!(stats.removed >= 1, "{stats:?}");

    let hits = hybrid_search(&core, Some(embedder.as_ref()), "lighthouse keeper", 10).unwrap();
    assert!(
        !hits.iter().any(|h| h.rel == *"notes/scrap.md"),
        "deleted documents stop voting: {hits:?}"
    );
}

#[test]
fn dimension_change_rebuilds_the_whole_index() {
    let (_tmp, core) = fixture();
    let narrow = Arc::new(MockEmbedder::new().with_dim(8));
    let wide_stats = sync(&core, narrow.as_ref()).unwrap();
    assert!(wide_stats.embedded > 0);

    let wide = Arc::new(MockEmbedder::new().with_dim(32));
    let stats = sync(&core, wide.as_ref()).unwrap();
    assert_eq!(
        stats.unchanged, 0,
        "a dimension change invalidates every vector: {stats:?}"
    );
    assert_eq!(stats.embedded, stats.chunks, "{stats:?}");

    let hits = hybrid_search(&core, Some(wide.as_ref()), "Elena read the letter", 5).unwrap();
    assert!(
        hits.iter().any(|h| h.semantic.is_some()),
        "search works on the rebuilt index: {hits:?}"
    );
}

#[test]
fn a_different_model_falls_back_to_keywords() {
    let (_tmp, core) = fixture();
    let indexed = embedder();
    sync(&core, indexed.as_ref()).unwrap();

    // Same vectors, different runtime identity — vectors don't compare.
    struct OtherModel(MockEmbedder);
    impl Embedder for OtherModel {
        fn info(&self) -> oca_ai::EmbedderInfo {
            oca_ai::EmbedderInfo {
                name: "other-model".into(),
                runtime: "mock".into(),
            }
        }
        fn is_loaded(&self) -> bool {
            self.0.is_loaded()
        }
        fn load(&self) -> anyhow::Result<()> {
            self.0.load()
        }
        fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            self.0.embed(texts)
        }
        fn unload(&self) -> anyhow::Result<()> {
            self.0.unload()
        }
    }
    let other = OtherModel(MockEmbedder::new());
    other.load().unwrap();

    let hits = hybrid_search(&core, Some(&other), "Elena read the letter", 5).unwrap();
    assert!(!hits.is_empty(), "keyword results still flow");
    assert!(
        hits.iter().all(|h| h.semantic.is_none()),
        "foreign vectors never vote: {hits:?}"
    );
}

#[test]
fn no_embedder_means_keyword_only() {
    let (_tmp, core) = fixture();
    let hits = hybrid_search(&core, None, "Elena read the letter", 5).unwrap();
    assert!(!hits.is_empty(), "rule 3: keywords alone still answer");
    assert!(hits.iter().all(|h| h.semantic.is_none()));
    assert!(hits.iter().all(|h| h.origin() == "lexical"));
}

#[test]
fn rag_feeds_the_retriever_seam() {
    use oca_ai::HybridSearch;

    let (_tmp, core) = fixture();
    let rag = Rag::new(Some(embedder()));
    rag.load().unwrap();
    sync(&core, rag.embedder().expect("embedder").as_ref()).unwrap();
    // Through the trait, exactly as the Phase 8 Retriever calls it.
    let docs = HybridSearch::search(&rag, &core, "Elena read the letter", 5).unwrap();
    assert!(!docs.is_empty());
    assert!(docs[0].score > 0.0);
    assert!(docs[0].rel.starts_with("manuscript/"));
}

/// A minimal but real EPUB — zip container + OPF + one XHTML chapter —
/// imported, indexed, and found by hybrid query.
#[test]
fn epub_import_round_trips_into_the_vector_index() {
    let (tmp, mut core) = fixture();
    let root = tmp.path();
    let epub_path = root.join("../tideways.epub");
    let epub_path = std::path::absolute(&epub_path).unwrap();
    write_minimal_epub(&epub_path).unwrap();

    let imported = import_file(root, &epub_path).unwrap();
    assert_eq!(imported.source_type, "epub");
    let copied = std::fs::read_to_string(root.join(&imported.rel)).unwrap();
    assert!(copied.contains("title: Tideways"), "{copied}"); // from the OPF metadata
    assert!(copied.contains("## Chapter One"), "{copied}"); // spine heading
    assert!(
        copied.contains("The tideways run deeper than any map admits."),
        "{copied}"
    );
    assert!(
        copied.contains("Currents remember every sailor who never returned."),
        "{copied}"
    );
    assert!(copied.contains("source_type: epub"), "{copied}");

    core.reindex().unwrap();
    let embedder = embedder();
    sync(&core, embedder.as_ref()).unwrap();
    let hits = hybrid_search(
        &core,
        Some(embedder.as_ref()),
        "currents remember every sailor",
        5,
    )
    .unwrap();
    assert_eq!(
        hits.first().map(|h| h.rel.clone()),
        Some(imported.rel),
        "the imported EPUB chapter ranks first: {hits:?}"
    );
    assert!(hits[0].untrusted);
}

/// Build a minimal EPUB on disk (container.xml, OPF, one XHTML chapter).
fn write_minimal_epub(path: &Path) -> anyhow::Result<()> {
    use std::io::Write as _;
    use zip::write::SimpleFileOptions;

    let file = std::fs::File::create(path)?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default();
    zip.start_file("mimetype", opts)?;
    zip.write_all(b"application/epub+zip")?;
    zip.start_file("META-INF/container.xml", opts)?;
    zip.write_all(
        br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>
"#,
    )?;
    zip.start_file("OEBPS/content.opf", opts)?;
    zip.write_all(
        br#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="uid">rag-test</dc:identifier>
    <dc:title>Tideways</dc:title>
  </metadata>
  <manifest>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="ch1"/>
  </spine>
</package>
"#,
    )?;
    zip.start_file("OEBPS/ch1.xhtml", opts)?;
    zip.write_all(
        br#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml">
<head><title>Chapter One</title></head>
<body>
<h1>Chapter One</h1>
<p>The <em>tideways</em> run deeper than any map admits.</p>
<p>Currents remember every sailor who never returned.</p>
</body>
</html>
"#,
    )?;
    zip.finish()?;
    Ok(())
}
