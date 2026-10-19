//! Seeded fixtures + deterministic graders for OCA's AI operations
//! (Phase 9, A8, rule 6). Every grade is a pure function of the output
//! text and project state — no LLM-as-judge ever runs in CI, so the same
//! grades come out on any machine, with or without a real model installed.
//!
//! Typical use (see `tests/evals.rs`): prepare a Phase 9 operation, run it
//! against `MockModel` (or a real model manually), and grade the raw
//! generation with [`SchemaValidity`], [`SummaryLength`], or
//! [`PreservesNamedEntities`] — the three first graders named in the
//! roadmap. [`NonEmpty`] is the cheapest sanity check for free-text
//! operations with no stronger contract.

mod fixtures;
mod graders;

pub use fixtures::{CHAPTER_TWO_SCENES, story_project};
pub use graders::{Grade, Grader, NonEmpty, PreservesNamedEntities, SchemaValidity, SummaryLength};

use anyhow::Result;
use oca_core::Core;

/// The project's known character names — the entity list the
/// rewrite-preservation grader checks against (entity knowledge comes from
/// the story engine, not from capitalization guessing).
pub fn character_names(core: &Core) -> Result<Vec<String>> {
    Ok(oca_story::Story::new(core)
        .characters()?
        .into_iter()
        .map(|c| c.name)
        .collect())
}
