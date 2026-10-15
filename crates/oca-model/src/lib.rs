//! OCA shared domain model: ids, lossless YAML patching, frontmatter, and the
//! document/project types shared by storage, core, and frontends.
//!
//! Invariant (ADR-0002): every YAML-bearing artifact keeps its raw text as the
//! source of truth. Parsing produces a *view*; edits are targeted patches that
//! never re-serialize what they didn't touch.

pub mod document;
pub mod frontmatter;
pub mod id;
pub mod project;
pub mod yaml;

pub use document::{Block, Document, LineRange, Section, WikiLink};
pub use frontmatter::Frontmatter;
pub use id::new_id;
pub use project::{DocumentKind, ProjectKind, ProjectMeta, SUPPORTED_FORMAT_VERSION};
pub use yaml::LosslessYaml;
