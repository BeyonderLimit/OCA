//! The context engine (Phase 8) — the layer every AI feature talks to
//! instead of llama.cpp (AGENTS rule 6, `OCA-implementation.md` §3):
//!
//! ```text
//! AI feature → Retriever → story/music state + search → PromptBuilder → LanguageModel
//! ```
//!
//! - Explicit, serializable context types ([`ManuscriptContext`] …
//!   [`ArtistContext`]) so prompts are snapshot-testable (golden files).
//! - [`Retriever`] pulls deterministic engine state (Phases 2/3) and search
//!   results (Phase 5) into those types — read-only, never a writer.
//! - [`PromptBuilder`] assembles messages with provenance on every block, a
//!   conservative character-based token budget with a truncation policy
//!   (A8), and untrusted-content delimiting for imported/searched text.
//!
//! What generation produces flows into an `oca_core::Proposal` (A5, rule 4)
//! with the context manifest attached — nothing here touches project files.

mod prompt;
mod retriever;

use serde::{Deserialize, Serialize};

pub use prompt::{CHARS_PER_TOKEN, Prompt, PromptBuilder, SYSTEM_PREAMBLE, estimate_tokens};
pub use retriever::Retriever;

/// Where a context block came from — recorded in every proposal's manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// Block kind (`chapter`, `character`, `search`, …).
    pub kind: String,
    /// Provenance id: frontmatter id, project-relative path, or query.
    pub id: String,
}

impl Source {
    pub fn new(kind: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
        }
    }
}

/// One piece of prompt context: quoted project material with provenance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextItem {
    pub source: Source,
    pub text: String,
    /// Higher priority is kept first when the token budget runs out.
    pub priority: i32,
    /// Imported/searched material is quoted data, never instructions (A8).
    pub untrusted: bool,
}

impl ContextItem {
    /// Trusted project material at the given priority.
    pub fn trusted(
        kind: impl Into<String>,
        id: impl Into<String>,
        text: impl Into<String>,
        priority: i32,
    ) -> Self {
        Self {
            source: Source::new(kind, id),
            text: text.into(),
            priority,
            untrusted: false,
        }
    }
}

/// Priorities the Retriever assigns: the material the task is *about*
/// survives the budget; metadata around it is dropped first.
pub const PRIORITY_PRIMARY: i32 = 100;
pub const PRIORITY_SUPPORTING: i32 = 50;
pub const PRIORITY_BACKGROUND: i32 = 20;

/// Every context type renders to [`ContextItem`] blocks — the prompt
/// builder's only input. Renderings are deterministic: authored order,
/// empty fields omitted.
pub trait IntoContext {
    fn blocks(&self) -> Vec<ContextItem>;
}

/// Chapter identity shared by the chapter-level contexts (`Chapter` from
/// the story engine isn't serializable; this is the context-layer view).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChapterRef {
    /// Frontmatter id (`ch_…`).
    pub id: String,
    pub title: String,
    /// Narrative position (filename sequence number).
    pub seq: u32,
}

impl ChapterRef {
    fn heading(&self) -> String {
        format!("Chapter {} — {} ({})", self.seq, self.title, self.id)
    }
}

/// Whole-project view: what the manuscript is and what's in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManuscriptContext {
    pub title: String,
    pub language: String,
    pub chapters: Vec<ChapterRef>,
}

/// One chapter: identity, neighbors, and the full body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChapterContext {
    pub chapter: ChapterRef,
    pub body: String,
    pub previous: Option<ChapterRef>,
    pub next: Option<ChapterRef>,
}

/// One scene — an `## `-delimited section inside a chapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneContext {
    pub chapter: ChapterRef,
    /// The `## ` heading text, as authored.
    pub heading: String,
    pub body: String,
}

/// One character: identity, aliases, relationships, carry-forward state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterContext {
    pub id: String,
    pub name: String,
    pub role: Option<String>,
    pub aliases: Vec<String>,
    /// `(target, kind)` edges as authored.
    pub relationships: Vec<(String, String)>,
    /// Resolved state fields (`field: value`), sorted by field name.
    pub state: Vec<(String, String)>,
    /// The chapter the state was resolved at, when the project has chapters.
    pub state_at: Option<ChapterRef>,
}

/// One worldbuilding document (`world/*.md`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldEntity {
    pub id: String,
    pub title: String,
    pub body: String,
}

/// Worldbuilding facts: every `world/` document in path order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldContext {
    pub entities: Vec<WorldEntity>,
}

/// One `## ` section of a song's lyrics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SongSection {
    pub heading: String,
    /// The section's lines; a `{=ref}` section renders its repeat marker.
    pub lines: Vec<String>,
}

/// One song: metadata plus every section, for the music operations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SongContext {
    pub slug: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub themes: Vec<String>,
    pub plan: Vec<String>,
    pub sections: Vec<SongSection>,
}

/// One album (`music/albums/<slug>/album.yaml`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumContext {
    /// Project-relative directory.
    pub dir: String,
    pub title: Option<String>,
    pub concept: Option<String>,
    pub tracks: Vec<String>,
}

/// The Artist Bible (`music/artist.yaml`) — the voice everything sings in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtistContext {
    pub name: Option<String>,
    pub genres: Vec<String>,
    pub influences: Vec<String>,
    pub vocal_style: Option<String>,
    pub themes: Vec<String>,
    pub instrumentation: Vec<String>,
    pub tempos: Option<String>,
    pub production: Option<String>,
    pub forbidden_cliches: Vec<String>,
    pub goals: Option<String>,
}

fn nonempty(s: &str) -> bool {
    !s.trim().is_empty()
}

/// `key: value` lines for the given `(label, value)` pairs, skipping
/// empty/absent values — the stable rendering shared by the metadata blocks.
fn field_lines<'a>(fields: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    let mut out = String::new();
    for (label, value) in fields {
        if nonempty(value) {
            out.push_str(label);
            out.push_str(": ");
            out.push_str(value.trim());
            out.push('\n');
        }
    }
    out
}

impl IntoContext for ManuscriptContext {
    fn blocks(&self) -> Vec<ContextItem> {
        let mut text = format!(
            "Project: {} (language {})\nChapters:\n",
            self.title, self.language
        );
        for c in &self.chapters {
            text.push_str(&format!("{} — {} ({})\n", c.seq, c.title, c.id));
        }
        vec![ContextItem::trusted(
            "manuscript",
            self.title.clone(),
            text.trim_end(),
            PRIORITY_SUPPORTING,
        )]
    }
}

impl IntoContext for ChapterContext {
    fn blocks(&self) -> Vec<ContextItem> {
        let mut position = String::new();
        if let Some(p) = &self.previous {
            position.push_str(&format!("Previous: {}\n", p.heading()));
        }
        if let Some(n) = &self.next {
            position.push_str(&format!("Next: {}", n.heading()));
        }
        let mut items = vec![ContextItem::trusted(
            "chapter",
            self.chapter.id.clone(),
            format!("{}\n\n{}", self.chapter.heading(), self.body.trim_end()),
            PRIORITY_PRIMARY,
        )];
        if !position.is_empty() {
            items.push(ContextItem::trusted(
                "chapter_position",
                self.chapter.id.clone(),
                position,
                PRIORITY_BACKGROUND,
            ));
        }
        items
    }
}

impl IntoContext for SceneContext {
    fn blocks(&self) -> Vec<ContextItem> {
        vec![ContextItem::trusted(
            "scene",
            self.chapter.id.clone(),
            format!(
                "Scene “{}” in {}\n\n{}",
                self.heading,
                self.chapter.heading(),
                self.body.trim()
            ),
            PRIORITY_PRIMARY,
        )]
    }
}

impl IntoContext for CharacterContext {
    fn blocks(&self) -> Vec<ContextItem> {
        let mut identity = format!("Character: {} ({})\n", self.name, self.id);
        identity.push_str(&field_lines([
            ("role", self.role.as_deref().unwrap_or("")),
            ("aliases", &self.aliases.join("; ")),
            (
                "relationships",
                &self
                    .relationships
                    .iter()
                    .map(|(t, k)| format!("{t} — {k}"))
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
        ]));
        let mut items = vec![ContextItem::trusted(
            "character",
            self.id.clone(),
            identity.trim_end(),
            PRIORITY_SUPPORTING,
        )];
        if !self.state.is_empty() {
            let at = self
                .state_at
                .as_ref()
                .map(ChapterRef::heading)
                .unwrap_or_else(|| "as authored".to_string());
            let mut text = format!("State at {at}:\n");
            for (field, value) in &self.state {
                text.push_str(&format!("{field}: {value}\n"));
            }
            items.push(ContextItem::trusted(
                "character_state",
                self.id.clone(),
                text.trim_end(),
                PRIORITY_PRIMARY,
            ));
        }
        items
    }
}

impl IntoContext for WorldContext {
    fn blocks(&self) -> Vec<ContextItem> {
        self.entities
            .iter()
            .map(|e| {
                ContextItem::trusted(
                    "world",
                    if e.id.is_empty() {
                        e.title.clone()
                    } else {
                        e.id.clone()
                    },
                    format!("World: {}\n\n{}", e.title, e.body.trim()),
                    PRIORITY_BACKGROUND,
                )
            })
            .collect()
    }
}

impl IntoContext for SongContext {
    fn blocks(&self) -> Vec<ContextItem> {
        let mut meta = format!(
            "Song: {} ({})\n",
            self.title.clone().unwrap_or_else(|| self.slug.clone()),
            self.slug
        );
        meta.push_str(&field_lines([
            ("artist", self.artist.as_deref().unwrap_or("")),
            ("themes", &self.themes.join("; ")),
            ("plan", &self.plan.join(" → ")),
        ]));
        let mut items = vec![ContextItem::trusted(
            "song",
            self.slug.clone(),
            meta.trim_end(),
            PRIORITY_SUPPORTING,
        )];
        for s in &self.sections {
            let mut text = format!("## {}\n", s.heading);
            for line in &s.lines {
                text.push_str(line);
                text.push('\n');
            }
            items.push(ContextItem::trusted(
                "song_section",
                self.slug.clone(),
                text.trim_end(),
                PRIORITY_PRIMARY,
            ));
        }
        items
    }
}

impl IntoContext for AlbumContext {
    fn blocks(&self) -> Vec<ContextItem> {
        let mut text = format!(
            "Album: {}\n",
            self.title.clone().unwrap_or_else(|| self.dir.clone())
        );
        text.push_str(&self.dir);
        text.push('\n');
        text.push_str(&field_lines([(
            "concept",
            self.concept.as_deref().unwrap_or(""),
        )]));
        if !self.tracks.is_empty() {
            text.push_str("tracks:\n");
            for (i, t) in self.tracks.iter().enumerate() {
                text.push_str(&format!("{} — {}\n", i + 1, t));
            }
        }
        vec![ContextItem::trusted(
            "album",
            self.dir.clone(),
            text.trim_end(),
            PRIORITY_BACKGROUND,
        )]
    }
}

impl IntoContext for ArtistContext {
    fn blocks(&self) -> Vec<ContextItem> {
        let text = format!(
            "Artist Bible\n{}",
            field_lines([
                ("name", self.name.as_deref().unwrap_or("")),
                ("genres", &self.genres.join("; ")),
                ("influences", &self.influences.join("; ")),
                ("vocal style", self.vocal_style.as_deref().unwrap_or("")),
                ("themes", &self.themes.join("; ")),
                ("instrumentation", &self.instrumentation.join("; ")),
                ("tempos", self.tempos.as_deref().unwrap_or("")),
                ("production", self.production.as_deref().unwrap_or("")),
                ("forbidden cliches", &self.forbidden_cliches.join("; ")),
                ("goals", self.goals.as_deref().unwrap_or("")),
            ])
            .trim_end()
        );
        vec![ContextItem::trusted(
            "artist",
            self.name.clone().unwrap_or_else(|| "artist.yaml".into()),
            text,
            PRIORITY_SUPPORTING,
        )]
    }
}
