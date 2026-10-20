//! The Retriever (Phase 8): pulls deterministic project state — story and
//! music engine views (Phases 2/3) plus full-text search (Phase 5) — into
//! serializable context objects. Read-only by construction; AI output
//! flows into proposals (rule 4), never back through here.

use anyhow::{Context as _, Result};

use oca_core::{Core, DocEntry};
use oca_music::Music;
use oca_story::{Chapter, Story};

use super::{
    AlbumContext, ArtistContext, ChapterContext, ChapterRef, CharacterContext, ContextItem,
    ManuscriptContext, PRIORITY_BACKGROUND, SceneContext, SongContext, SongSection, WorldContext,
    WorldEntity,
};

/// Read-side access to everything the context engine quotes.
pub struct Retriever<'c> {
    core: &'c Core,
    hybrid: Option<&'c dyn HybridSearch>,
}

/// One hybrid retrieval hit (Phase 11): a document the fused FTS ∥ vector
/// search ranked for the query, with the provenance the context builder
/// needs to keep imported material untrusted (A8).
#[derive(Clone, Debug, PartialEq)]
pub struct RetrievedDoc {
    /// Project-relative path (`library/…` for imported references).
    pub rel: String,
    pub title: String,
    pub snippet: String,
    /// Imported reference material — quoted data, never instructions (A8).
    pub untrusted: bool,
    /// Fusion score (higher = closer match).
    pub score: f32,
}

/// The Phase 11 feed into the Retriever: frontends inject `oca-rag`'s
/// hybrid search (FTS ∥ vector → rerank → dedup) through this seam. `oca-ai`
/// never depends on `oca-rag` — the composition root wires it (rule 8:
/// the TUI/CLI hold zero retrieval logic).
pub trait HybridSearch: Send + Sync {
    fn search(&self, core: &Core, query: &str, limit: usize) -> Result<Vec<RetrievedDoc>>;
}

fn to_ref(c: &Chapter) -> ChapterRef {
    ChapterRef {
        id: c.id.clone(),
        title: c.title.clone(),
        seq: c.seq,
    }
}

impl<'c> Retriever<'c> {
    pub fn new(core: &'c Core) -> Self {
        Self { core, hybrid: None }
    }

    /// A retriever whose [`Retriever::search`] draws on the injected hybrid
    /// search (Phase 11); `None` keeps the plain Phase 5 keyword path.
    pub fn with_search(core: &'c Core, hybrid: Option<&'c dyn HybridSearch>) -> Self {
        Self { core, hybrid }
    }

    /// Whole-project view: title, language, and the chapter list.
    pub fn manuscript(&self) -> Result<ManuscriptContext> {
        let meta = self.core.meta().context("no project is open")?;
        let story = Story::new(self.core);
        Ok(ManuscriptContext {
            title: meta.title(),
            language: meta.language(),
            chapters: story.chapters()?.iter().map(to_ref).collect(),
        })
    }

    /// One chapter by any key the story engine resolves (number, id,
    /// title, stem, path): identity, neighbors, full body.
    pub fn chapter(&self, key: &str) -> Result<ChapterContext> {
        let story = Story::new(self.core);
        let chapters: Vec<ChapterRef> = story.chapters()?.iter().map(to_ref).collect();
        let chapter = story.chapter(key)?;
        let idx = chapters
            .iter()
            .position(|c| c.id == chapter.id)
            .context("chapter left the manuscript mid-read")?;
        let body = self.core.read_document(&chapter.rel)?.body().to_string();
        Ok(ChapterContext {
            chapter: chapters[idx].clone(),
            body,
            previous: idx.checked_sub(1).and_then(|i| chapters.get(i).cloned()),
            next: chapters.get(idx + 1).cloned(),
        })
    }

    /// One scene: the section under the `## ` heading `heading` (matched
    /// case-insensitively) inside the chapter.
    pub fn scene(&self, key: &str, heading: &str) -> Result<SceneContext> {
        let chapter = self.chapter(key)?;
        let want = heading.trim().to_lowercase();
        let lines: Vec<&str> = chapter.body.lines().collect();
        let mut start = None;
        for (i, line) in lines.iter().enumerate() {
            if let Some(h) = line.strip_prefix("## ")
                && h.trim().to_lowercase() == want
            {
                start = Some(i);
                break;
            }
        }
        let start = start.with_context(|| {
            format!(
                "no scene {heading:?} in chapter {} — scenes are `## ` sections",
                chapter.chapter.seq
            )
        })?;
        let end = lines[start + 1..]
            .iter()
            .position(|l| l.starts_with("## "))
            .map(|p| start + 1 + p)
            .unwrap_or(lines.len());
        Ok(SceneContext {
            chapter: chapter.chapter,
            heading: heading.trim().to_string(),
            body: lines[start + 1..end].join("\n"),
        })
    }

    /// One character by any name/story key: identity, aliases,
    /// relationships, and carry-forward state at `at` (default: the last
    /// chapter — what the story engine calls their current state).
    pub fn character(&self, name: &str, at: Option<&str>) -> Result<CharacterContext> {
        let story = Story::new(self.core);
        match at {
            Some(ch) => {
                let view = story.where_is(name, Some(ch))?;
                Ok(Self::character_context(
                    view.character,
                    fields_of(&view.fields),
                    Some(to_ref(&view.chapter)),
                ))
            }
            None => {
                let profile = story.who_is(name)?;
                Ok(Self::character_context(
                    profile.character,
                    fields_of(&profile.state),
                    profile.at.as_ref().map(to_ref),
                ))
            }
        }
    }

    /// Every `world/*.md` document, in path order.
    pub fn world(&self) -> Result<WorldContext> {
        let project = self.core.project().context("no project is open")?;
        let mut docs: Vec<&DocEntry> = project
            .documents()
            .iter()
            .filter(|d| d.group() == oca_model::DocumentKind::Location.dir())
            .collect();
        docs.sort_by(|a, b| a.rel.cmp(&b.rel));
        let mut entities = Vec::with_capacity(docs.len());
        for d in docs {
            let body = self.core.read_document(&d.rel)?.body().to_string();
            entities.push(WorldEntity {
                id: d.id.clone().unwrap_or_default(),
                title: d.title.clone(),
                body,
            });
        }
        Ok(WorldContext { entities })
    }

    /// One song by slug/id/title: metadata plus every section.
    pub fn song(&self, key: &str) -> Result<SongContext> {
        let song = Music::new(self.core).song(key)?;
        Ok(SongContext {
            slug: song.entry.slug().to_string(),
            title: song.yaml.title.clone(),
            artist: song.yaml.artist.clone(),
            themes: song.yaml.themes.clone(),
            plan: song.yaml.plan.clone().unwrap_or_default(),
            sections: song
                .lyrics
                .sections
                .iter()
                .map(|s| SongSection {
                    heading: s.heading.clone(),
                    lines: match &s.reference_to {
                        Some(target) => vec![format!("(repeats {target})")],
                        None => s.lines.iter().map(|l| l.raw.clone()).collect(),
                    },
                })
                .collect(),
        })
    }

    /// One album by slug or title.
    pub fn album(&self, key: &str) -> Result<AlbumContext> {
        let albums = Music::new(self.core).albums()?;
        let lower = key.to_lowercase();
        albums
            .into_iter()
            .find(|(slug, a)| {
                slug == key || a.title.as_deref().map(str::to_lowercase).as_deref() == Some(&lower)
            })
            .map(|(slug, a)| AlbumContext {
                dir: format!("music/albums/{slug}"),
                title: a.title,
                concept: a.concept,
                tracks: a.tracks,
            })
            .with_context(|| format!("no album {key:?}"))
    }

    /// The Artist Bible (`music/artist.yaml`), when the project has one.
    pub fn artist(&self) -> Result<ArtistContext> {
        let a = Music::new(self.core)
            .artist()?
            .context("this project has no music/artist.yaml")?;
        Ok(ArtistContext {
            name: a.name,
            genres: a.genres,
            influences: a.influences,
            vocal_style: a.vocal_style,
            themes: a.themes,
            instrumentation: a.instrumentation,
            tempos: a.tempos,
            production: a.production,
            forbidden_cliches: a.forbidden_cliches,
            goals: a.goals,
        })
    }

    /// Search results as untrusted blocks — searched material is quoted
    /// data, never instructions (A8). With a hybrid search wired (Phase
    /// 11), the fused FTS ∥ vector ranking feeds this instead of plain
    /// keyword search; imported references stay untrusted either way.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<ContextItem>> {
        if let Some(hybrid) = self.hybrid {
            return Ok(hybrid
                .search(self.core, query, limit)?
                .into_iter()
                .map(|doc| {
                    let mut item = ContextItem::trusted(
                        "search",
                        doc.rel,
                        format!("{} — {}", doc.title, doc.snippet),
                        PRIORITY_BACKGROUND,
                    );
                    item.untrusted = doc.untrusted;
                    item
                })
                .collect());
        }
        Ok(self
            .core
            .search(query, limit)?
            .into_iter()
            .map(|h| {
                let mut item = ContextItem::trusted(
                    "search",
                    format!("{}:{}", h.kind, h.rel.display()),
                    format!("{} — {}", h.title, h.snippet),
                    PRIORITY_BACKGROUND,
                );
                item.untrusted = true;
                item
            })
            .collect())
    }

    fn character_context(
        character: oca_story::Character,
        state: Vec<(String, String)>,
        state_at: Option<ChapterRef>,
    ) -> CharacterContext {
        CharacterContext {
            id: character.id,
            name: character.name,
            role: character.role,
            aliases: character.aliases,
            relationships: character
                .relationships
                .into_iter()
                .map(|r| (r.target, r.kind))
                .collect(),
            state,
            state_at,
        }
    }
}

fn fields_of(fields: &std::collections::BTreeMap<String, String>) -> Vec<(String, String)> {
    fields.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}
