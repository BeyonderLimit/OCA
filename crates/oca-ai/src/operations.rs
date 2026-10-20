//! The five Phase 9 story operations, smallest first (AGENTS rule 4, 6, 8):
//! continue scene → rewrite selection → summarize chapter → generate
//! outline → review chapter.
//!
//! The pipeline is split so every step stays testable and thin frontends
//! stay clients of Core:
//!
//! 1. [`prepare_*`] (main thread, `&Core`, deterministic): resolves story
//!    state through the Phase 8 Retriever, builds the prompt, and pins the
//!    proposal target — no model involved.
//! 2. [`complete`] (job thread, no Core): streams the generation and turns
//!    the output into a [`Draft`] (schema ops parse + repair first).
//! 3. [`create_proposal`] (main thread, `&mut Core`): records the draft as
//!    a Core proposal. Nothing touches the manuscript here — the author
//!    accepts, edits, or rejects through the proposal UX (rule 4).
//!
//! `REVIEW_CHAPTER` is a report, not a revision (`OCA-build.md` §7.4): its
//! draft carries [`DraftTarget::Report`] and never becomes a proposal.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use oca_core::{Command, Core, ManifestEntry};
use oca_model::DocumentKind;
use oca_story::Story;

use crate::context::{
    ChapterContext, ChapterRef, ContextItem, HybridSearch, IntoContext, PRIORITY_BACKGROUND,
    PRIORITY_PRIMARY, PRIORITY_SUPPORTING, PromptBuilder, Retriever,
};
use crate::{Ai, GenerationRequest};

/// Default context budget for operations (estimated tokens). Frontends
/// may pass the model's configured context instead.
pub const DEFAULT_BUDGET: usize = 4096;

/// One operation's identity: the name recorded on proposals and the
/// prompt-template version, so results stay comparable across changes
/// (A8 prompt versioning).
#[derive(Debug)]
pub struct OperationSpec {
    pub name: &'static str,
    pub prompt_version: &'static str,
}

pub const CONTINUE_SCENE: OperationSpec = OperationSpec {
    name: "continue_scene",
    prompt_version: "continue-scene/1",
};
pub const REWRITE_SELECTION: OperationSpec = OperationSpec {
    name: "rewrite_selection",
    prompt_version: "rewrite-selection/1",
};
pub const SUMMARIZE_CHAPTER: OperationSpec = OperationSpec {
    name: "summarize_chapter",
    prompt_version: "summarize-chapter/1",
};
pub const GENERATE_OUTLINE: OperationSpec = OperationSpec {
    name: "generate_outline",
    prompt_version: "generate-outline/1",
};
pub const REVIEW_CHAPTER: OperationSpec = OperationSpec {
    name: "review_chapter",
    prompt_version: "review-chapter/1",
};

/// Where a completed draft would land. Pinned at prepare time against the
/// author's actual document, never guessed from model output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DraftTarget {
    /// Replace `start..end` of the document body (rewrite selection).
    Replace {
        document: PathBuf,
        start: usize,
        end: usize,
    },
    /// Append after the current body (summaries, outline). The container
    /// document is created empty if missing when the proposal is recorded
    /// — empty file only; AI text lands solely through the proposal.
    Append {
        document: PathBuf,
        kind: DocumentKind,
    },
    /// A report shown to the author; there is nothing to apply.
    Report,
}

/// How the raw completion becomes a payload.
#[derive(Clone, Debug)]
pub enum PayloadFormat {
    /// Trimmed free text.
    FreeText,
    /// Free text plus a `## <chapter title>` heading (summaries).
    SummaryHeading { title: String },
    /// Constrained-decoding JSON outline, rendered as markdown.
    JsonOutline,
    /// Constrained-decoding JSON issues, rendered as a report.
    JsonReview,
    /// Free text becomes a new `## <heading>` lyrics section (Phase 10
    /// generate-section). The heading is pinned at prepare time.
    SectionHeading { heading: String },
    /// Free-text lines become a `song.yaml` `plan:` block (Phase 10
    /// song-structure). `leading_newline` pads a block appended to a file
    /// that does not end with a newline — decided from the real body.
    PlanBlock { leading_newline: bool },
    /// Constrained-decoding JSON alternates, rendered as a numbered list.
    JsonAlternatives,
    /// Constrained-decoding JSON observations, rendered as a report.
    JsonObservations,
    /// Constrained-decoding JSON singability verdict, rendered as a report.
    JsonSingability,
}

/// An operation fully resolved against project state, ready to generate.
#[derive(Debug)]
pub struct Prepared {
    pub spec: &'static OperationSpec,
    pub request: GenerationRequest,
    pub target: DraftTarget,
    pub format: PayloadFormat,
    pub manifest: Vec<ManifestEntry>,
}

/// A completed generation: what to record, where it would go, and what
/// was in the prompt.
#[derive(Clone, Debug)]
pub struct Draft {
    pub operation: String,
    pub prompt_version: String,
    /// Model name that produced it.
    pub model: String,
    pub target: DraftTarget,
    pub payload: String,
    pub manifest: Vec<ManifestEntry>,
}

// ---- structured outputs (A8 constrained decoding + parse → repair) ------

/// `REVIEW_CHAPTER` output — matches the `CHECK_CONTINUITY` shape in
/// `OCA-build.md` §7.2.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Review {
    pub issues: Vec<ReviewIssue>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReviewIssue {
    #[serde(rename = "type")]
    pub kind: String,
    /// `info`, `warning`, or `error`.
    pub severity: String,
    pub location: String,
    pub explanation: String,
}

/// `GENERATE_OUTLINE` output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Outline {
    pub sections: Vec<OutlineSection>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutlineSection {
    pub title: String,
    pub beats: Vec<String>,
}

pub fn review_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "issues": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "type": { "type": "string" },
                        "severity": { "type": "string" },
                        "location": { "type": "string" },
                        "explanation": { "type": "string" }
                    },
                    "required": ["type", "severity", "location", "explanation"]
                }
            }
        },
        "required": ["issues"]
    })
}

pub fn outline_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "sections": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "title": { "type": "string" },
                        "beats": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["title", "beats"]
                }
            }
        },
        "required": ["sections"]
    })
}

/// Strip the markdown fences a model wraps JSON in — the one repair step
/// before a schema parse fails for real (A8 parse → repair → retry).
/// Shared with `oca-eval`'s schema-validity grader so "what parses" means
/// the same thing everywhere.
pub fn repair_json(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let rest = rest.strip_prefix("json").unwrap_or(rest);
    let rest = rest.strip_suffix("```").unwrap_or(rest);
    rest.trim()
}

pub fn parse_review(text: &str) -> Result<Review> {
    serde_json::from_str(repair_json(text)).context("model output is not a valid issue list")
}

pub fn parse_outline(text: &str) -> Result<Outline> {
    serde_json::from_str(repair_json(text)).context("model output is not a valid outline")
}

/// Render a parsed review as the report the author reads.
pub fn render_review(review: &Review) -> String {
    review
        .issues
        .iter()
        .map(|i| {
            format!(
                "{} [{}] {} — {}",
                i.severity, i.kind, i.location, i.explanation
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render a parsed outline as markdown for `outline/outline.md`.
pub fn render_outline(outline: &Outline) -> String {
    let mut out = String::new();
    for section in &outline.sections {
        out.push_str(&format!("## {}\n", section.title));
        for beat in &section.beats {
            out.push_str(&format!("- {beat}\n"));
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

// ---- prepare (deterministic, &Core) --------------------------------------

fn chapter_doc(core: &Core, key: &str) -> Result<(ChapterContext, PathBuf)> {
    let retriever = Retriever::new(core);
    let ctx = retriever.chapter(key)?;
    let rel = Story::new(core).chapter(key)?.rel;
    Ok((ctx, rel))
}

pub(crate) fn task_line(spec: &OperationSpec, task: &str) -> String {
    format!("Operation: {}\n{task}", spec.name)
}

pub(crate) fn validate_range(body: &str, start: usize, end: usize) -> Result<()> {
    if start > end
        || end > body.len()
        || !body.is_char_boundary(start)
        || !body.is_char_boundary(end)
    {
        anyhow::bail!("selection {start}..{end} is not a valid range in the chapter body");
    }
    Ok(())
}

/// `CONTINUE_SCENE`: text after the cursor (or selection end) in the
/// chapter, continuing the scene in progress. The prompt sees only what
/// precedes the cursor — never the author's future text.
pub fn prepare_continue_scene(
    core: &Core,
    chapter: &str,
    at: usize,
    budget: usize,
    search: Option<&dyn HybridSearch>,
) -> Result<Prepared> {
    let (ctx, document) = chapter_doc(core, chapter)?;
    let body = &ctx.body;
    if at > body.len() || !body.is_char_boundary(at) {
        anyhow::bail!("cursor {at} is not a valid position in the chapter body");
    }
    let before = &body[..at];
    // The scene in progress: text from the last `## ` heading to the cursor.
    let scene_start = before
        .rfind("\n## ")
        .map(|i| i + 1)
        .or_else(|| before.starts_with("## ").then_some(0))
        .unwrap_or(0);
    let mut items = vec![
        ContextItem::trusted(
            "chapter",
            ctx.chapter.id.clone(),
            ctx.chapter.heading(),
            PRIORITY_SUPPORTING,
        ),
        ContextItem::trusted(
            "scene_before_cursor",
            ctx.chapter.id.clone(),
            before[scene_start..].trim_end(),
            PRIORITY_PRIMARY,
        ),
    ];
    // Phase 11 feed: whatever the hybrid search ties to this chapter
    // (imported references included — always untrusted, A8).
    if let Some(hybrid) = search {
        items.extend(
            Retriever::with_search(core, Some(hybrid))
                .search(&ctx.chapter.title, 3)
                .unwrap_or_default(),
        );
    }
    let prompt = PromptBuilder::new(budget).build(
        &task_line(
            &CONTINUE_SCENE,
            "Continue the scene exactly where the excerpt stops: same point of view, \
             tense, and voice. Do not repeat the excerpt, do not add a heading or \
             quotation marks. Output only the continuation text.",
        ),
        &items,
    )?;
    Ok(Prepared {
        spec: &CONTINUE_SCENE,
        request: GenerationRequest {
            prompt: prompt.user,
            system: Some(prompt.system),
            schema: None,
            max_tokens: None,
        },
        target: DraftTarget::Replace {
            document,
            start: at,
            end: at,
        },
        format: PayloadFormat::FreeText,
        manifest: prompt.manifest,
    })
}

/// `REWRITE_SELECTION`: rewrite `start..end` of the chapter body, keeping
/// meaning; `guidance` steers style ("tighter", "from Marcus's POV", …).
pub fn prepare_rewrite_selection(
    core: &Core,
    chapter: &str,
    start: usize,
    end: usize,
    guidance: Option<&str>,
    budget: usize,
) -> Result<Prepared> {
    let (ctx, document) = chapter_doc(core, chapter)?;
    validate_range(&ctx.body, start, end)?;
    let selection = ctx.body[start..end].trim();
    if selection.is_empty() {
        anyhow::bail!("select some text to rewrite — an empty selection has nothing to rewrite");
    }
    const SURROUND: usize = 400;
    let before_start = start.saturating_sub(SURROUND);
    let after_end = (end + SURROUND).min(ctx.body.len());
    let mut around = String::new();
    if before_start < start {
        around.push_str("… ");
        around.push_str(ctx.body[before_start..start].trim());
        around.push(' ');
    }
    if end < after_end {
        around.push(' ');
        around.push_str(ctx.body[end..after_end].trim());
        around.push_str(" …");
    }
    let mut items = vec![ContextItem::trusted(
        "selection",
        ctx.chapter.id.clone(),
        selection,
        PRIORITY_PRIMARY,
    )];
    if !around.trim().is_empty() {
        items.push(ContextItem::trusted(
            "surrounding",
            ctx.chapter.id.clone(),
            around.trim(),
            PRIORITY_BACKGROUND,
        ));
    }
    items.push(ContextItem::trusted(
        "chapter",
        ctx.chapter.id.clone(),
        ctx.chapter.heading(),
        PRIORITY_SUPPORTING,
    ));
    let task = match guidance {
        Some(g) if !g.trim().is_empty() => format!(
            "Operation: {}\nRewrite the selected text, keeping its meaning and the \
             chapter's voice. Guidance: {}. Output only the rewritten selection.",
            REWRITE_SELECTION.name,
            g.trim()
        ),
        _ => task_line(
            &REWRITE_SELECTION,
            "Rewrite the selected text — same meaning, fresher expression — in the \
             chapter's voice. Output only the rewritten selection.",
        ),
    };
    let prompt = PromptBuilder::new(budget).build(&task, &items)?;
    Ok(Prepared {
        spec: &REWRITE_SELECTION,
        request: GenerationRequest {
            prompt: prompt.user,
            system: Some(prompt.system),
            schema: None,
            max_tokens: None,
        },
        target: DraftTarget::Replace {
            document,
            start,
            end,
        },
        format: PayloadFormat::FreeText,
        manifest: prompt.manifest,
    })
}

/// `SUMMARIZE_CHAPTER`: a 3–5 sentence summary, appended (as a proposal)
/// to `notes/summaries.md` under the chapter's heading.
pub fn prepare_summarize_chapter(core: &Core, chapter: &str, budget: usize) -> Result<Prepared> {
    let (ctx, _) = chapter_doc(core, chapter)?;
    let items = Retriever::new(core).chapter(chapter)?.blocks();
    let prompt = PromptBuilder::new(budget).build(
        &task_line(
            &SUMMARIZE_CHAPTER,
            "Summarize this chapter in 3–5 sentences: what happens, who acts, what \
             changes. Output only the summary.",
        ),
        &items,
    )?;
    Ok(Prepared {
        spec: &SUMMARIZE_CHAPTER,
        request: GenerationRequest {
            prompt: prompt.user,
            system: Some(prompt.system),
            schema: None,
            max_tokens: None,
        },
        target: DraftTarget::Append {
            document: PathBuf::from("notes/summaries.md"),
            kind: DocumentKind::Note,
        },
        format: PayloadFormat::SummaryHeading {
            title: ctx.chapter.title.clone(),
        },
        manifest: prompt.manifest,
    })
}

/// The shared story-state items the outline and review operations quote:
/// characters with current state, world facts, and the timeline.
fn story_state_items(core: &Core, chapter: Option<&ChapterRef>) -> Result<Vec<ContextItem>> {
    let story = Story::new(core);
    let mut items = Vec::new();
    for c in story.characters()? {
        let at = chapter.map(|r| r.id.as_str());
        let ctx = Retriever::new(core).character(&c.name, at)?;
        items.extend(ctx.blocks());
    }
    items.extend(Retriever::new(core).world()?.blocks());
    // A project may have no timeline (short stories, early drafts) — that
    // is missing context, not an operation error.
    if core.read_document(Path::new("outline/timeline.md")).is_ok() {
        let events = story.events()?;
        if !events.is_empty() {
            let mut text = String::from("Timeline:\n");
            for e in &events {
                text.push_str(&format!("- {} — {}", e.story_time, e.title));
                if let Some(summary) = &e.summary {
                    text.push_str(&format!(": {summary}"));
                }
                text.push('\n');
            }
            items.push(ContextItem::trusted(
                "timeline",
                "timeline",
                text.trim_end(),
                PRIORITY_BACKGROUND,
            ));
        }
    }
    Ok(items)
}

/// `GENERATE_OUTLINE`: a sectioned outline of beats from the manuscript,
/// characters, world, and timeline; appended to `outline/outline.md`.
/// Constrained decoding (A8) keeps the output parseable.
pub fn prepare_generate_outline(core: &Core, budget: usize) -> Result<Prepared> {
    let mut items = Retriever::new(core).manuscript()?.blocks();
    items.extend(story_state_items(core, None)?);
    let prompt = PromptBuilder::new(budget).build(
        &task_line(
            &GENERATE_OUTLINE,
            "Propose an outline for this story as JSON: sections with a title and a \
             list of beats. Base every beat on the provided material. Output only the \
             JSON.",
        ),
        &items,
    )?;
    Ok(Prepared {
        spec: &GENERATE_OUTLINE,
        request: GenerationRequest {
            prompt: prompt.user,
            system: Some(prompt.system),
            schema: Some(outline_schema()),
            max_tokens: None,
        },
        target: DraftTarget::Append {
            document: PathBuf::from("outline/outline.md"),
            kind: DocumentKind::PlotThread,
        },
        format: PayloadFormat::JsonOutline,
        manifest: prompt.manifest,
    })
}

/// `REVIEW_CHAPTER`: continuity/quality issues as a structured report —
/// shown to the author, never applied (§7.4: the review is a report).
pub fn prepare_review_chapter(core: &Core, chapter: &str, budget: usize) -> Result<Prepared> {
    let (ctx, _) = chapter_doc(core, chapter)?;
    let mut items = Retriever::new(core).chapter(chapter)?.blocks();
    items.extend(story_state_items(core, Some(&ctx.chapter))?);
    let prompt = PromptBuilder::new(budget).build(
        &task_line(
            &REVIEW_CHAPTER,
            "Review this chapter for continuity and craft problems (timeline clashes, \
             character/state contradictions, point-of-view slips). Report each issue \
             as JSON with type, severity, location, and explanation. If there are \
             none, output {\"issues\": []}. Output only the JSON.",
        ),
        &items,
    )?;
    Ok(Prepared {
        spec: &REVIEW_CHAPTER,
        request: GenerationRequest {
            prompt: prompt.user,
            system: Some(prompt.system),
            schema: Some(review_schema()),
            max_tokens: None,
        },
        target: DraftTarget::Report,
        format: PayloadFormat::JsonReview,
        manifest: prompt.manifest,
    })
}

// ---- complete (job thread, no Core) --------------------------------------

/// Stream the generation and turn it into a [`Draft`]. Call inside a job;
/// `sink` receives tokens as they arrive (progress display).
pub fn complete(ai: &Ai, prepared: Prepared, sink: &mut dyn FnMut(&str)) -> Result<Draft> {
    let outcome = ai.generate_with(&prepared.request, sink)?;
    if outcome.cancelled {
        anyhow::bail!("generation was stopped before it finished");
    }
    let output = outcome.text.trim_end().to_string();
    if output.is_empty() {
        anyhow::bail!("the model produced no text");
    }
    let payload = match &prepared.format {
        PayloadFormat::FreeText => output,
        PayloadFormat::SummaryHeading { title } => format!("## {title}\n{output}"),
        PayloadFormat::JsonOutline => render_outline(&parse_outline(&output)?),
        PayloadFormat::JsonReview => render_review(&parse_review(&output)?),
        PayloadFormat::SectionHeading { heading } => {
            format!("\n## {heading}\n\n{}\n", output.trim_end())
        }
        PayloadFormat::PlanBlock { leading_newline } => {
            let pad = if *leading_newline { "\n" } else { "" };
            // The trailing newline stays: replacing the middle of a
            // `song.yaml` must not glue the payload onto the next key.
            format!(
                "{pad}{}\n",
                crate::music_ops::render_plan(&output).trim_end()
            )
        }
        PayloadFormat::JsonAlternatives => {
            crate::music_ops::render_alternatives(&crate::music_ops::parse_alternatives(&output)?)
        }
        PayloadFormat::JsonObservations => {
            crate::music_ops::render_observations(&crate::music_ops::parse_observations(&output)?)
        }
        PayloadFormat::JsonSingability => {
            crate::music_ops::render_singability(&crate::music_ops::parse_singability(&output)?)
        }
    };
    Ok(Draft {
        operation: prepared.spec.name.to_string(),
        prompt_version: prepared.spec.prompt_version.to_string(),
        model: ai.info().name,
        target: prepared.target,
        payload,
        manifest: prepared.manifest,
    })
}

// ---- record (main thread, &mut Core) -------------------------------------

/// Record a draft as a Core proposal (rule 4). Appends create the empty
/// container document first when it's missing — an empty file, never AI
/// text; the payload lands only when the author accepts.
pub fn create_proposal(core: &mut Core, draft: Draft) -> Result<String> {
    let (document, start, end) = match &draft.target {
        DraftTarget::Report => {
            anyhow::bail!("review results are a report for you to read — there is nothing to apply")
        }
        DraftTarget::Replace {
            document,
            start,
            end,
        } => (document.clone(), *start, *end),
        DraftTarget::Append { document, kind } => {
            let rel = ensure_container(core, document, *kind)?;
            let len = core.read_document(&rel)?.body().len();
            (rel, len, len)
        }
    };
    let events = core.execute(Command::ProposalCreate {
        operation: draft.operation,
        prompt_version: draft.prompt_version,
        model: draft.model,
        document,
        start,
        end,
        payload: draft.payload,
        context_manifest: draft.manifest,
    })?;
    match &events[0] {
        oca_core::Event::ProposalCreated { id, .. } => Ok(id.clone()),
        other => anyhow::bail!("proposal was not recorded: {other:?}"),
    }
}

fn ensure_container(core: &mut Core, rel: &Path, kind: DocumentKind) -> Result<PathBuf> {
    if core.read_document(rel).is_ok() {
        return Ok(rel.to_path_buf());
    }
    // Title = the filename stem; `create_document` places it by kind.
    let title = rel
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .context("container path has no file stem")?;
    let events = core.execute(Command::CreateDocument { kind, title })?;
    let oca_core::Event::DocumentCreated { rel: created, .. } = &events[0] else {
        anyhow::bail!("container document was not created: {events:?}");
    };
    Ok(created.clone())
}
