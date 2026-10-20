//! Retrieval-augmented generation plumbing (Phase 11): reference-library
//! import, deterministic chunking, incremental embedding into the same
//! `oca.db` (D8 — one file, `sqlite-vec` linked in-process), and hybrid
//! search that fuses SQLite FTS with vector kNN.
//!
//! Everything here is cache-side and rebuildable (rule 2): the canonical
//! data is the project's Markdown — including `library/` copies, which
//! carry their provenance in frontmatter (A8: imported material is
//! untrusted context, quoted data, never instructions).
//!
//! The embedding runtime lives behind [`oca_ai::Embedder`] (rule 6: one
//! llama.cpp touchpoint, mocks in tests); this crate never spawns
//! anything itself. [`search::Rag`] implements [`oca_ai::HybridSearch`],
//! the seam the Phase 8 Retriever consumes — the direction of
//! dependencies keeps `oca-ai` free of retrieval details.

pub mod chunk;
pub mod epub;
pub mod import;
pub mod search;
pub mod sync;

pub use chunk::{Chunk, chunk_markdown};
pub use epub::{EpubBook, read_epub};
pub use import::{Imported, import_file};
pub use search::{Rag, RagHit, hybrid_search};
pub use sync::{SyncStats, sync};
