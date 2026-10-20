//! OCA story engine (Phase 2): the deterministic story layer over an open
//! project (A11). Files are the source of truth — every query reads
//! Markdown/YAML through the Core API, and no AI is involved anywhere
//! (rule 3).
//!
//! Business logic lives *here*, not in the frontends (rule 8): the TUI and
//! CLI collect input, call [`Story`], and format the answer.
//!
//! Documented limits (A11): mention detection is a case-sensitive
//! word-boundary match on name + aliases — pronouns and other anaphora are
//! not resolved; `story_time` values are compared as strings, so keep one
//! sortable scheme (ISO dates or zero-padded labels) per project.

mod continuity;
mod entity;
mod mention;
mod state;
mod timeline;

pub use continuity::{
    Issue, IssueRef, Severity, check, ignored_ids, note_for, render, set_ignored,
};
pub use entity::{Character, Relationship};
pub use mention::mentions;
pub use state::{StateEntry, StateView, set_state};
pub use timeline::{Event, TimeSense};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use oca_core::Core;

/// Manuscript + entity directories and the timeline file (A1 layout).
const MANUSCRIPT_DIR: &str = "manuscript";
const CHARACTERS_DIR: &str = "characters";
pub(crate) const TIMELINE_REL: &str = "outline/timeline.md";

/// Story queries over an open project (read side; [`set_state`] is one
/// mutation, [`continuity`] the other surface).
pub struct Story<'c> {
    pub(crate) core: &'c Core,
}

/// One manuscript chapter in narrative order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chapter {
    pub rel: PathBuf,
    /// Frontmatter id (`ch_…`) — the handle states and events key by (A2).
    pub id: String,
    pub title: String,
    /// Filename sequence number (`008-…` → 8) — the narrative position.
    pub seq: u32,
}

/// Narrative position of a hand-authored chapter key (`at:` entries in
/// `states:`, `chapter:` fields in events): id first, then filename stem,
/// then title — the same tolerance `Story::chapter` offers, because these
/// keys live in frontmatter people write by hand and the docs show
/// filename-shaped examples.
pub(crate) fn position_of(chs: &[Chapter], key: &str) -> Option<usize> {
    chs.iter()
        .position(|c| c.id == key)
        .or_else(|| {
            chs.iter()
                .position(|c| c.rel.file_stem().and_then(|s| s.to_str()) == Some(key))
        })
        .or_else(|| chs.iter().position(|c| c.title.eq_ignore_ascii_case(key)))
}

/// "Who is Marcus?" — the authored identity plus resolved current state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub character: Character,
    /// Carry-forward state at the last chapter (empty for a stateless cast).
    pub state: BTreeMap<String, String>,
    /// The chapter the state was resolved at, when the project has chapters.
    pub at: Option<Chapter>,
}

/// Strings/numbers/bools as their rendered scalar; null, sequences, and
/// mappings have no flat rendering in story data.
pub(crate) fn scalar(v: &serde_yaml::Value) -> Option<String> {
    match v {
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

impl<'c> Story<'c> {
    pub fn new(core: &'c Core) -> Self {
        Self { core }
    }

    /// Chapters in narrative order. Files without an id (nothing resolves
    /// without one, A2) or a leading sequence number (no narrative
    /// position) are skipped.
    pub fn chapters(&self) -> Result<Vec<Chapter>> {
        let project = self.core.project().context("no project is open")?;
        let mut chs: Vec<Chapter> = project
            .documents()
            .iter()
            .filter(|d| d.group() == MANUSCRIPT_DIR)
            .filter_map(|d| {
                Some(Chapter {
                    rel: d.rel.clone(),
                    id: d.id.clone()?,
                    title: d.title.clone(),
                    seq: d.seq?,
                })
            })
            .collect();
        chs.sort_by(|a, b| (a.seq, &a.rel).cmp(&(b.seq, &b.rel)));
        Ok(chs)
    }

    /// Resolve a chapter by narrative number (`8`), id (`ch_…`), title
    /// (case-insensitive), filename stem (`008-the-trial`), or
    /// project-relative path — the key forms authors actually ask with.
    pub fn chapter(&self, key: &str) -> Result<Chapter> {
        let chs = self.chapters()?;
        let lower = key.to_lowercase();
        let path = Path::new(key);
        let stem = key.strip_suffix(".md").unwrap_or(key);
        chs.iter()
            .find(|c| {
                c.id == key
                    || c.rel == path
                    || c.rel.with_extension("") == Path::new(stem)
                    || c.rel.file_stem().and_then(|s| s.to_str()) == Some(stem)
                    || (!key.is_empty()
                        && key.chars().all(|c| c.is_ascii_digit())
                        && c.seq.to_string() == key)
                    || c.title.to_lowercase() == lower
            })
            .cloned()
            .with_context(|| {
                format!("no chapter {key:?} — try a number (8), an id (ch_…), a title, or a path")
            })
    }

    /// Every character entity (`characters/*.md`) in path order.
    pub fn characters(&self) -> Result<Vec<Character>> {
        let project = self.core.project().context("no project is open")?;
        let rels: Vec<PathBuf> = project
            .documents()
            .iter()
            .filter(|d| d.group() == CHARACTERS_DIR)
            .map(|d| d.rel.clone())
            .collect();
        let mut chars = Vec::with_capacity(rels.len());
        for rel in rels {
            let doc = self.core.read_document(&rel)?;
            chars.push(Character::parse(&rel, &doc));
        }
        Ok(chars)
    }

    /// Resolve a character by id, name, alias, title, filename stem, or path
    /// (first match; names and aliases compare case-insensitively).
    pub fn character(&self, key: &str) -> Result<Character> {
        let chars = self.characters()?;
        let lower = key.to_lowercase();
        let path = Path::new(key);
        let stem = key.strip_suffix(".md").unwrap_or(key);
        chars
            .iter()
            .find(|c| {
                c.id == key
                    || c.name.to_lowercase() == lower
                    || c.aliases.iter().any(|a| a.to_lowercase() == lower)
                    || c.rel == path
                    || c.rel.with_extension("") == Path::new(stem)
                    || c.rel.file_stem().and_then(|s| s.to_str()) == Some(stem)
                    || c.title.to_lowercase() == lower
            })
            .cloned()
            .with_context(|| {
                format!(
                    "no character {key:?} in characters/ — add characters/<slug>.md with \
                     id: / name: / aliases: lines (the TUI's New document writes the shape)"
                )
            })
    }

    /// "Where is Elena?" — carry-forward state at `at` (any chapter key;
    /// default: the last chapter, i.e. where the story leaves them).
    pub fn where_is(&self, character: &str, at: Option<&str>) -> Result<StateView> {
        let character = self.character(character)?;
        let chs = self.chapters()?;
        let chapter = match at {
            Some(key) => self.chapter(key)?,
            None => chs
                .last()
                .cloned()
                .context("the project has no chapters yet")?,
        };
        let fields = state::resolve(&character.states, &chs, &chapter);
        Ok(StateView {
            character,
            chapter,
            fields,
        })
    }

    /// "Who is Marcus?" — identity, relationships, and current state.
    pub fn who_is(&self, character: &str) -> Result<Profile> {
        let character = self.character(character)?;
        let chs = self.chapters()?;
        let at = chs.last().cloned();
        let state = match &at {
            Some(ch) => state::resolve(&character.states, &chs, ch),
            None => BTreeMap::new(),
        };
        Ok(Profile {
            character,
            state,
            at,
        })
    }

    /// "Which chapters contain Elena?" — bodies mentioning the name or an
    /// alias at word boundaries (see the crate docs for the limits).
    pub fn chapters_containing(&self, character: &str) -> Result<Vec<Chapter>> {
        let character = self.character(character)?;
        let mut hits = Vec::new();
        for ch in self.chapters()? {
            let doc = self.core.read_document(&ch.rel)?;
            let body = doc.body();
            if std::iter::once(&character.name)
                .chain(character.aliases.iter())
                .any(|term| mentions(body, term))
            {
                hits.push(ch);
            }
        }
        Ok(hits)
    }

    /// Timeline events from `outline/timeline.md` frontmatter (`events:`),
    /// in file order.
    pub fn events(&self) -> Result<Vec<Event>> {
        let project = self.core.project().context("no project is open")?;
        let rel = Path::new(TIMELINE_REL);
        if !project.documents().iter().any(|d| d.rel == rel) {
            bail!(
                "no {TIMELINE_REL} — record events there (frontmatter key 'events:') \
                 to ask what happened when"
            );
        }
        let doc = self.core.read_document(rel)?;
        Ok(match doc.frontmatter.get_value("events") {
            Some(v) => Event::parse_all(&v),
            None => Vec::new(),
        })
    }

    /// "What happened before <chapter>?" in one time sense:
    /// [`TimeSense::Narrative`] lists events *shown* in earlier chapters, in
    /// book order; [`TimeSense::StoryTime`] lists events whose `story_time`
    /// precedes the target chapter's `story_time:` frontmatter, in
    /// chronological order. A flashback shown late therefore still counts as
    /// "before" on the story clock — and a flash-forward shown early does
    /// not (A11).
    pub fn events_before(&self, chapter: &str, sense: TimeSense) -> Result<Vec<Event>> {
        let target = self.chapter(chapter)?;
        let chs = self.chapters()?;
        let position_of = |key: &str| position_of(&chs, key);
        let mut evs = self.events()?;
        match sense {
            TimeSense::Narrative => {
                let target_pos = position_of(&target.id).context("chapter lost from the index")?;
                evs.retain(|e| {
                    e.chapter
                        .as_deref()
                        .and_then(position_of)
                        .is_some_and(|p| p < target_pos)
                });
                evs.sort_by_key(|e| e.chapter.as_deref().and_then(position_of));
            }
            TimeSense::StoryTime => {
                let doc = self.core.read_document(&target.rel)?;
                let story_time = doc.frontmatter.get_str("story_time").with_context(|| {
                    format!(
                        "chapter {} has no story_time — add a 'story_time:' line to its \
                         frontmatter to order events by story time",
                        target.title
                    )
                })?;
                evs.retain(|e| e.story_time < story_time);
                evs.sort_by(|a, b| a.story_time.cmp(&b.story_time));
            }
        }
        Ok(evs)
    }
}
