//! The Phase 10 music operations, smallest first (AGENTS rule 4, 6, 8):
//! song concept → song structure → generate section (verse/chorus/bridge)
//! → rewrite lyrics → improve rhyme → improve meter → preserve hook →
//! preserve rhyme scheme → alternatives → analyze song → check
//! singability.
//!
//! Same three-phase pipeline as the story operations (`operations`):
//! deterministic prepare on the main thread, streamed completion in a job,
//! and — for the writing operations — a Core proposal the author accepts
//! (rule 4). `OCA-build.md` §7.2 maps each operation to a spec name; the
//! metadata target (`song_structure`) is the only one that leaves
//! `lyrics.md`, and it lands in `song.yaml`'s `plan:` block through a
//! proposal like everything else.
//!
//! Deterministic tools grade what the model proposes (roadmap Phase 10):
//! rhyme/meter scores and the hook come from `oca-music` (`score_lines`,
//! `hook_line`), never from the model. Concept, alternatives, analyze,
//! and singability are reports — shown, never applied (`§7.4`).

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use oca_core::Core;
use oca_model::DocumentKind;
use oca_music::Music;

use crate::GenerationRequest;
use crate::context::{ContextItem, IntoContext, PRIORITY_PRIMARY, PromptBuilder, Retriever};
use crate::operations::{DraftTarget, OperationSpec, PayloadFormat, Prepared, task_line};

pub const SONG_CONCEPT: OperationSpec = OperationSpec {
    name: "song_concept",
    prompt_version: "song-concept/1",
};
pub const SONG_STRUCTURE: OperationSpec = OperationSpec {
    name: "song_structure",
    prompt_version: "song-structure/1",
};
pub const GENERATE_SECTION: OperationSpec = OperationSpec {
    name: "generate_section",
    prompt_version: "generate-section/1",
};
pub const REWRITE_LYRICS: OperationSpec = OperationSpec {
    name: "rewrite_lyrics",
    prompt_version: "rewrite-lyrics/1",
};
pub const IMPROVE_RHYME: OperationSpec = OperationSpec {
    name: "improve_rhyme",
    prompt_version: "improve-rhyme/1",
};
pub const IMPROVE_METER: OperationSpec = OperationSpec {
    name: "improve_meter",
    prompt_version: "improve-meter/1",
};
pub const PRESERVE_HOOK: OperationSpec = OperationSpec {
    name: "preserve_hook",
    prompt_version: "preserve-hook/1",
};
pub const PRESERVE_RHYME_SCHEME: OperationSpec = OperationSpec {
    name: "preserve_rhyme_scheme",
    prompt_version: "preserve-rhyme-scheme/1",
};
pub const ALTERNATIVES: OperationSpec = OperationSpec {
    name: "alternatives",
    prompt_version: "alternatives/1",
};
pub const ANALYZE_SONG: OperationSpec = OperationSpec {
    name: "analyze_song",
    prompt_version: "analyze-song/1",
};
pub const CHECK_SINGABILITY: OperationSpec = OperationSpec {
    name: "check_singability",
    prompt_version: "check-singability/1",
};

// ---- structured outputs (A8 constrained decoding + parse → repair) ------

/// `ALTERNATIVES` output: candidate wordings of a selection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Alternatives {
    pub alternatives: Vec<String>,
}

/// `ANALYZE_SONG` output: one observation per area (structure, rhyme,
/// meter, hook, repetition, theme).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Observations {
    pub observations: Vec<Observation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub area: String,
    pub note: String,
}

/// `CHECK_SINGABILITY` output: can a voice carry these lines
/// (`OCA-build.md` §7.2 — excellent poetry can still be terrible lyrics).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Singability {
    pub verdict: String,
    pub problems: Vec<SingProblem>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SingProblem {
    pub location: String,
    pub issue: String,
}

pub fn alternatives_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "alternatives": {
                "type": "array",
                "items": { "type": "string" }
            }
        },
        "required": ["alternatives"]
    })
}

pub fn observations_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "observations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "area": { "type": "string" },
                        "note": { "type": "string" }
                    },
                    "required": ["area", "note"]
                }
            }
        },
        "required": ["observations"]
    })
}

pub fn singability_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "verdict": { "type": "string" },
            "problems": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "location": { "type": "string" },
                        "issue": { "type": "string" }
                    },
                    "required": ["location", "issue"]
                }
            }
        },
        "required": ["verdict", "problems"]
    })
}

pub fn parse_alternatives(text: &str) -> Result<Alternatives> {
    serde_json::from_str(crate::operations::repair_json(text))
        .context("model output is not a valid list of alternatives")
}

pub fn parse_observations(text: &str) -> Result<Observations> {
    serde_json::from_str(crate::operations::repair_json(text))
        .context("model output is not a valid observation list")
}

pub fn parse_singability(text: &str) -> Result<Singability> {
    serde_json::from_str(crate::operations::repair_json(text))
        .context("model output is not a valid singability verdict")
}

/// Render alternatives as the numbered list the author weighs.
pub fn render_alternatives(alternatives: &Alternatives) -> String {
    alternatives
        .alternatives
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{}. {line}", i + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render observations as the analyze report.
pub fn render_observations(observations: &Observations) -> String {
    observations
        .observations
        .iter()
        .map(|o| format!("- {}: {}", o.area.trim(), o.note.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render the singability verdict plus its problems.
pub fn render_singability(singability: &Singability) -> String {
    let mut out = singability.verdict.trim().to_string();
    for problem in &singability.problems {
        out.push_str(&format!("\n- {} — {}", problem.location, problem.issue));
    }
    out
}

/// Free-text lines become a `song.yaml` `plan:` block: one item per
/// non-empty line; a stray `plan:` header is not an item. The trailing
/// newline is added by the caller so a middle-of-file replace never glues
/// the block onto the next key.
pub fn render_plan(output: &str) -> String {
    let mut out = String::from("plan:\n");
    for line in output.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if line.starts_with("plan:") {
            continue;
        }
        out.push_str(&format!("  - {line}\n"));
    }
    out.trim_end().to_string()
}

// ---- prepare (deterministic, &Core) --------------------------------------

/// Resolve a song plus its two documents: the lyrics body (the byte
/// ranges live in here) and the `song.yaml` body (the plan block).
fn song_docs(
    core: &Core,
    key: &str,
) -> Result<(oca_music::LoadedSong, PathBuf, PathBuf, String, String)> {
    let song = Music::new(core).song(key)?;
    let lyrics_rel = PathBuf::from(format!("music/songs/{}/lyrics.md", song.entry.slug()));
    let lyrics_body = core
        .read_document(&lyrics_rel)
        .with_context(|| format!("read {}", lyrics_rel.display()))?
        .body()
        .to_string();
    let yaml_rel = PathBuf::from(format!("music/songs/{}/song.yaml", song.entry.slug()));
    let yaml_body = core
        .read_document(&yaml_rel)
        .with_context(|| format!("read {}", yaml_rel.display()))?
        .body()
        .to_string();
    Ok((song, lyrics_rel, yaml_rel, lyrics_body, yaml_body))
}

fn validate_selection(body: &str, start: usize, end: usize) -> Result<()> {
    if start > end
        || end > body.len()
        || !body.is_char_boundary(start)
        || !body.is_char_boundary(end)
    {
        anyhow::bail!("selection {start}..{end} is not a valid range in the lyrics");
    }
    Ok(())
}

/// The song context plus the selected lyric lines — every selection-based
/// operation shares these.
fn selection_items(
    core: &Core,
    song: &str,
    slug: &str,
    body: &str,
    start: usize,
    end: usize,
) -> Result<(Vec<ContextItem>, String)> {
    validate_selection(body, start, end)?;
    let selection = body[start..end].trim();
    if selection.is_empty() {
        anyhow::bail!("select some lyric lines first — an empty selection has nothing to revise");
    }
    let mut items = vec![ContextItem::trusted(
        "selection",
        slug.to_string(),
        selection,
        PRIORITY_PRIMARY,
    )];
    items.extend(Retriever::new(core).song(song)?.blocks());
    Ok((items, selection.to_string()))
}

fn prepared(
    spec: &'static OperationSpec,
    task: String,
    items: Vec<ContextItem>,
    target: DraftTarget,
    format: PayloadFormat,
    schema: Option<serde_json::Value>,
    budget: usize,
) -> Result<Prepared> {
    let prompt = PromptBuilder::new(budget).build(&task, &items)?;
    Ok(Prepared {
        spec,
        request: GenerationRequest {
            prompt: prompt.user,
            system: Some(prompt.system),
            schema,
            max_tokens: None,
        },
        target,
        format,
        manifest: prompt.manifest,
    })
}

/// The context blocks for whole-song operations: metadata plus every
/// section, through the same Retriever the context engine tests pin.
fn whole_song_items(core: &Core, key: &str) -> Result<Vec<ContextItem>> {
    Ok(Retriever::new(core).song(key)?.blocks())
}

/// Byte range of the top-level `plan:` block in a `song.yaml` body: the
/// `plan:` line plus the following indented, comment, or blank lines.
/// When there is no plan yet, the append point is the end of the file.
pub(crate) fn plan_range(body: &str) -> (usize, usize, bool) {
    let mut offset = 0usize;
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let top = !line.starts_with(' ')
            && !line.starts_with('\t')
            && !trimmed.is_empty()
            && !trimmed.starts_with('#');
        if top && trimmed.starts_with("plan:") {
            let start = offset;
            let mut end = offset + line.len();
            let mut scan = offset + line.len();
            for next in body[scan..].split_inclusive('\n') {
                let t = next.trim_start();
                if next.starts_with(' ')
                    || next.starts_with('\t')
                    || t.starts_with('#')
                    || t.trim().is_empty()
                {
                    scan += next.len();
                    end = scan;
                } else {
                    break;
                }
            }
            return (start, end, true);
        }
        offset += line.len();
    }
    (body.len(), body.len(), false)
}

/// `SONG_CONCEPT`: what the song is about — one image, title ideas,
/// themes. A report: identity calls (title, mood) stay the author's
/// hands; they copy what they like into `song.yaml` themselves.
pub fn prepare_song_concept(core: &Core, song: &str, budget: usize) -> Result<Prepared> {
    let items = whole_song_items(core, song)?;
    prepared(
        &SONG_CONCEPT,
        task_line(
            &SONG_CONCEPT,
            "Describe this song's concept: the central image, what it feels like, \
             three title ideas, and three themes. Output only the description.",
        ),
        items,
        DraftTarget::Report,
        PayloadFormat::FreeText,
        None,
        budget,
    )
}

/// `SONG_STRUCTURE`: propose the song's `plan:` list. Lands in
/// `song.yaml` through a proposal — replacing the existing plan block,
/// or appended at the end when the song has no plan yet.
pub fn prepare_song_structure(core: &Core, song: &str, budget: usize) -> Result<Prepared> {
    let (_, _, yaml_rel, _, yaml_body) = song_docs(core, song)?;
    let items = whole_song_items(core, song)?;
    let (start, end, _) = plan_range(&yaml_body);
    let leading_newline = !yaml_body.is_empty() && !yaml_body[..start].ends_with('\n');
    prepared(
        &SONG_STRUCTURE,
        task_line(
            &SONG_STRUCTURE,
            "Propose the structural plan for this song as a list: one section per \
             line (intro, verse, pre_chorus, chorus, bridge, outro — lowercase, as \
             in the plan format). Base it on the existing sections. Output only the \
             section names, one per line.",
        ),
        items,
        DraftTarget::Replace {
            document: yaml_rel,
            start,
            end,
        },
        PayloadFormat::PlanBlock { leading_newline },
        None,
        budget,
    )
}

/// `GENERATE_VERSE` / `GENERATE_CHORUS` / `GENERATE_BRIDGE`: one new
/// section titled `heading` ("Verse 3", "Chorus", "Bridge"), appended
/// after the song's current sections through a proposal.
pub fn prepare_generate_section(
    core: &Core,
    song: &str,
    heading: &str,
    budget: usize,
) -> Result<Prepared> {
    let heading = heading.trim();
    if heading.is_empty() || heading.contains('\n') {
        anyhow::bail!("a section heading is one short line, like \"Bridge\" or \"Verse 3\"");
    }
    // The target must use the resolved slug, never the typed key (an
    // author may have passed a title or id).
    let slug = Music::new(core).song(song)?.entry.slug().to_string();
    let items = whole_song_items(core, song)?;
    prepared(
        &GENERATE_SECTION,
        task_line(
            &GENERATE_SECTION,
            &format!(
                "Write a new \"{heading}\" section for this song: lyric lines only — \
                 no heading, no chord markers, no quotation marks. Match the song's \
                 voice and meter. Output only the lines."
            ),
        ),
        items,
        DraftTarget::Append {
            document: PathBuf::from(format!("music/songs/{slug}/lyrics.md")),
            kind: DocumentKind::Song,
        },
        PayloadFormat::SectionHeading {
            heading: heading.to_string(),
        },
        None,
        budget,
    )
}

/// `REWRITE_LYRICS`: rewrite `start..end` of the lyrics, keeping meaning;
/// `guidance` steers style ("tighter", "more image, less telling", …).
pub fn prepare_rewrite_lyrics(
    core: &Core,
    song: &str,
    start: usize,
    end: usize,
    guidance: Option<&str>,
    budget: usize,
) -> Result<Prepared> {
    let (loaded, lyrics_rel, _, body, _) = song_docs(core, song)?;
    let (items, _) = selection_items(core, song, loaded.entry.slug(), &body, start, end)?;
    let task = match guidance {
        Some(g) if !g.trim().is_empty() => task_line(
            &REWRITE_LYRICS,
            &format!(
                "Rewrite the selected lyric lines, keeping their meaning and the \
                 song's voice. Guidance: {}. Output only the rewritten lines.",
                g.trim()
            ),
        ),
        _ => task_line(
            &REWRITE_LYRICS,
            "Rewrite the selected lyric lines — same meaning, fresher image — in the \
             song's voice. Output only the rewritten lines.",
        ),
    };
    prepared(
        &REWRITE_LYRICS,
        task,
        items,
        DraftTarget::Replace {
            document: lyrics_rel,
            start,
            end,
        },
        PayloadFormat::FreeText,
        None,
        budget,
    )
}

/// The selection's deterministic scores — where every scored operation
/// gets its numbers from (`oca-music`, never the model).
fn selection_scores(song: &oca_music::LoadedSong, selection: &str) -> oca_music::LyricScores {
    let lines: Vec<String> = selection.lines().map(str::to_string).collect();
    oca_music::score_lines(&lines, &song.language)
}

fn unsupported_for_scores(language: &str, what: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "language {language:?} cannot be scored for {what} — OCA reports unsupported \
         rather than inventing numbers (A9); revise the wording manually"
    )
}

/// Does any rhyme class repeat? A scheme where every letter appears once
/// means nothing rhymes yet — there is no scheme to preserve.
fn scheme_repeats(labels: &[String]) -> bool {
    labels
        .iter()
        .enumerate()
        .any(|(i, l)| !l.is_empty() && labels[..i].contains(l))
}

/// `IMPROVE_RHYME`: rewrite the selection so its end words rhyme harder,
/// keeping meaning and meter. The prompt carries the current deterministic
/// scheme; `oca-eval`'s `PreservesRhymeScheme`-style graders measure it
/// before and after.
pub fn prepare_improve_rhyme(
    core: &Core,
    song: &str,
    start: usize,
    end: usize,
    budget: usize,
) -> Result<Prepared> {
    let (loaded, lyrics_rel, _, body, _) = song_docs(core, song)?;
    let (items, selection) = selection_items(core, song, loaded.entry.slug(), &body, start, end)?;
    let scores = selection_scores(&loaded, &selection);
    if scores.syllables.is_none() {
        return Err(unsupported_for_scores(&loaded.language, "rhyme"));
    }
    let instruction = if scheme_repeats(&scores.rhyme_scheme) {
        format!(
            "Rewrite the selected lines to strengthen their end rhymes. Current \
             end-rhyme scheme: {}. Every line whose letter repeats must rhyme with \
             its partners; keep the meaning, the meter, and the scheme. Output only \
             the rewritten lines.",
            scores.rhyme_scheme.join(" ")
        )
    } else {
        "Rewrite the selected lines to introduce strong end rhymes — a scheme a \
         singer can lean on — while keeping the meaning and the meter. Output only \
         the rewritten lines."
            .to_string()
    };
    prepared(
        &IMPROVE_RHYME,
        task_line(&IMPROVE_RHYME, &instruction),
        items,
        DraftTarget::Replace {
            document: lyrics_rel,
            start,
            end,
        },
        PayloadFormat::FreeText,
        None,
        budget,
    )
}

/// `IMPROVE_METER`: rewrite the selection onto the song's dominant line
/// length. The prompt carries the deterministic per-line counts and the
/// meter — the model never measures anything itself.
pub fn prepare_improve_meter(
    core: &Core,
    song: &str,
    start: usize,
    end: usize,
    budget: usize,
) -> Result<Prepared> {
    let (loaded, lyrics_rel, _, body, _) = song_docs(core, song)?;
    let (items, selection) = selection_items(core, song, loaded.entry.slug(), &body, start, end)?;
    let scores = selection_scores(&loaded, &selection);
    let Some(meter) = scores.meter else {
        return Err(unsupported_for_scores(&loaded.language, "meter"));
    };
    prepared(
        &IMPROVE_METER,
        task_line(
            &IMPROVE_METER,
            &format!(
                "Rewrite the selected lines so each lands on {meter} syllables (±1). \
                 Current per-line syllables: {}. Keep the meaning and the rhymes. \
                 Output only the rewritten lines.",
                scores
                    .line_syllables
                    .iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
        items,
        DraftTarget::Replace {
            document: lyrics_rel,
            start,
            end,
        },
        PayloadFormat::FreeText,
        None,
        budget,
    )
}

/// `PRESERVE_HOOK`: rewrite the selection while the song's hook — the
/// first chorus line — stays verbatim. The selection must contain the
/// hook; the grader checks it survives.
pub fn prepare_preserve_hook(
    core: &Core,
    song: &str,
    start: usize,
    end: usize,
    budget: usize,
) -> Result<Prepared> {
    let (loaded, lyrics_rel, _, body, _) = song_docs(core, song)?;
    let (items, selection) = selection_items(core, song, loaded.entry.slug(), &body, start, end)?;
    let Some(hook) = loaded.lyrics.hook_line() else {
        anyhow::bail!(
            "{} has no chorus yet — the hook is a chorus's first line",
            loaded.entry.slug()
        );
    };
    let contains_hook = selection
        .lines()
        .any(|l| oca_music::parse_chord_line(l).0.trim() == hook.trim());
    if !contains_hook {
        anyhow::bail!(
            "the hook line {hook:?} is not in this selection — select the chorus that carries it"
        );
    }
    prepared(
        &PRESERVE_HOOK,
        task_line(
            &PRESERVE_HOOK,
            &format!(
                "Rewrite the selected lines, tightening the phrasing. Hard constraint: \
                 the hook line — {hook:?} — must appear in the output exactly once, \
                 unchanged. Output only the rewritten lines."
            ),
        ),
        items,
        DraftTarget::Replace {
            document: lyrics_rel,
            start,
            end,
        },
        PayloadFormat::FreeText,
        None,
        budget,
    )
}

/// `PRESERVE_RHYME_SCHEME`: reword the selection keeping the exact
/// end-rhyme scheme. The grader compares the deterministic scheme of both
/// sides; a selection with no measurable rhymes has nothing to preserve.
pub fn prepare_preserve_rhyme_scheme(
    core: &Core,
    song: &str,
    start: usize,
    end: usize,
    budget: usize,
) -> Result<Prepared> {
    let (loaded, lyrics_rel, _, body, _) = song_docs(core, song)?;
    let (items, selection) = selection_items(core, song, loaded.entry.slug(), &body, start, end)?;
    let scores = selection_scores(&loaded, &selection);
    if scores.syllables.is_none() {
        return Err(unsupported_for_scores(&loaded.language, "rhyme"));
    }
    if !scheme_repeats(&scores.rhyme_scheme) {
        anyhow::bail!(
            "this selection's lines do not rhyme with each other — run improve rhyme first"
        );
    }
    prepared(
        &PRESERVE_RHYME_SCHEME,
        task_line(
            &PRESERVE_RHYME_SCHEME,
            &format!(
                "Rewrite the selected lines with fresher wording. Hard constraint: keep \
                 the end-rhyme scheme exactly — {}. Each output line keeps its current \
                 letter's rhyme. Output only the rewritten lines.",
                scores.rhyme_scheme.join(" ")
            ),
        ),
        items,
        DraftTarget::Replace {
            document: lyrics_rel,
            start,
            end,
        },
        PayloadFormat::FreeText,
        None,
        budget,
    )
}

/// `ALTERNATIVES`: candidate wordings of the selection, each scored
/// deterministically — a report the author weighs, never a proposal.
pub fn prepare_alternatives(
    core: &Core,
    song: &str,
    start: usize,
    end: usize,
    budget: usize,
) -> Result<Prepared> {
    let (loaded, _, _, body, _) = song_docs(core, song)?;
    let (items, _) = selection_items(core, song, loaded.entry.slug(), &body, start, end)?;
    prepared(
        &ALTERNATIVES,
        task_line(
            &ALTERNATIVES,
            "Propose five alternative wordings of the selected lines, each keeping the \
             meaning and roughly the same syllable count. Output only JSON: \
             {\"alternatives\": [\"…\"]}.",
        ),
        items,
        DraftTarget::Report,
        PayloadFormat::JsonAlternatives,
        Some(alternatives_schema()),
        budget,
    )
}

/// `ANALYZE_THEME/STRUCTURE/RHYME/SYLLABLES/METER/HOOK/REPETITION` as
/// one structured report (§7.2 analyze family) — shown, never applied.
/// The deterministic `oca song check` remains the authority on numbers;
/// this adds the model's reading beside it.
pub fn prepare_analyze_song(core: &Core, song: &str, budget: usize) -> Result<Prepared> {
    let items = whole_song_items(core, song)?;
    prepared(
        &ANALYZE_SONG,
        task_line(
            &ANALYZE_SONG,
            "Analyze this song as JSON: observations with an area (theme, structure, \
             rhyme, syllables, meter, hook, repetition) and a note. Base every \
             observation on the provided material. Output only the JSON.",
        ),
        items,
        DraftTarget::Report,
        PayloadFormat::JsonObservations,
        Some(observations_schema()),
        budget,
    )
}

/// `CHECK_SINGABILITY`: can a voice carry these lines — a report, never
/// applied. Excellent poetry can still be terrible lyrics (§7.2).
pub fn prepare_check_singability(core: &Core, song: &str, budget: usize) -> Result<Prepared> {
    let items = whole_song_items(core, song)?;
    prepared(
        &CHECK_SINGABILITY,
        task_line(
            &CHECK_SINGABILITY,
            "Judge this song's singability as JSON: a verdict, plus problems (location \
             and issue) where a line is too long, too dense, or unsingable. Output \
             only the JSON.",
        ),
        items,
        DraftTarget::Report,
        PayloadFormat::JsonSingability,
        Some(singability_schema()),
        budget,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_range_finds_the_inline_block() {
        let body =
            "title: Ashes\nthemes: [loss]\n\nplan: [verse, chorus]\n\nlinks:\n  chapters: []\n";
        let (start, end, found) = plan_range(body);
        assert!(found);
        assert_eq!(&body[start..end], "plan: [verse, chorus]\n\n");
    }

    #[test]
    fn plan_range_covers_a_block_list_to_the_next_key() {
        let body = "title: Ashes\nplan:\n  - verse\n  - chorus\ntitle_style: x\n";
        let (start, end, found) = plan_range(body);
        assert!(found);
        assert_eq!(&body[start..end], "plan:\n  - verse\n  - chorus\n");
        assert_eq!(&body[end..], "title_style: x\n");
    }

    #[test]
    fn plan_range_appends_when_there_is_no_plan() {
        let body = "title: Ashes\nthemes: [loss]\n";
        let (start, end, found) = plan_range(body);
        assert!(!found);
        assert_eq!(start, body.len());
        assert_eq!(end, body.len());
    }

    #[test]
    fn render_plan_one_item_per_line_and_header_filtered() {
        assert_eq!(
            render_plan("plan:\nverse\nchorus\n\n"),
            "plan:\n  - verse\n  - chorus"
        );
    }

    #[test]
    fn schema_round_trips() {
        let alt = parse_alternatives("{\"alternatives\":[\"a\",\"b\"]}").unwrap();
        assert_eq!(render_alternatives(&alt), "1. a\n2. b");
        let fenced = parse_observations(
            "```json\n{\"observations\":[{\"area\":\"hook\",\"note\":\"kept\"}]}\n```",
        )
        .unwrap();
        assert_eq!(render_observations(&fenced), "- hook: kept");
        let sing = parse_singability("{\"verdict\":\"singable\",\"problems\":[]}").unwrap();
        assert_eq!(render_singability(&sing), "singable");
    }
}
