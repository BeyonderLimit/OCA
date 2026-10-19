//! Phase 10 end-to-end music operation tests: prepare → complete (mock
//! model) → proposal → accept/stale → snapshot restore, plus the report
//! operations that never become proposals. Rule 6: every generation here
//! runs against `MockModel`, never a real model.

use std::sync::Arc;

use oca_ai::{
    Ai, DraftTarget, MockModel, PayloadFormat, complete, create_proposal, parse_alternatives,
    parse_observations, parse_singability, prepare_alternatives, prepare_analyze_song,
    prepare_check_singability, prepare_generate_section, prepare_improve_meter,
    prepare_improve_rhyme, prepare_preserve_hook, prepare_preserve_rhyme_scheme,
    prepare_rewrite_lyrics, prepare_song_concept, prepare_song_structure, render_alternatives,
    render_observations, render_plan, render_singability,
};
use oca_core::{Command, Core, Event};
use oca_model::ProjectKind;
use oca_music::{Lyrics, Music, SongYaml, score_lines};
use tempfile::TempDir;

const LYRICS: &str = "\
# Harbor Lantern

## Verse 1
The ferry lamps go out at four
the harbor locks its own back door

## Chorus
Keep the light on
the kettle keeps its slow repose
";

const VERSE: &str = "The ferry lamps go out at four\nthe harbor locks its own back door";
const CHORUS: &str = "Keep the light on\nthe kettle keeps its slow repose";

/// A one-song project: `music/songs/lantern/` with authored lyrics.
fn fixture() -> (TempDir, Core, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let mut core = Core::new();
    core.execute(Command::CreateProject {
        root: tmp.path().to_path_buf(),
        title: "Test Songs".into(),
        kind: ProjectKind::Novel,
    })
    .unwrap();
    let entry = Music::new_song(&mut core, "lantern", "Harbor Lantern").unwrap();
    core.save_body(&entry.lyrics_rel, LYRICS).unwrap();
    core.refresh_index().unwrap();
    (tmp, core, entry.lyrics_rel)
}

fn selection_range(body: &str, needle: &str) -> (usize, usize) {
    let start = body.find(needle).expect("selection in fixture");
    (start, start + needle.len())
}

fn mock_ai() -> Ai {
    let ai = Ai::new(Arc::new(MockModel::music_defaults()), Vec::new());
    ai.load().unwrap();
    ai
}

#[test]
fn improve_rhyme_replaces_the_selection_and_is_undoable() {
    // The Phase 10 exit flow: select the chorus/verse → improve rhyme.
    let (_tmp, mut core, rel) = fixture();
    let body = core.read_document(&rel).unwrap().body().to_string();
    let (start, end) = selection_range(&body, VERSE);

    let prepared = prepare_improve_rhyme(&core, "lantern", start, end, 4096).unwrap();
    assert_eq!(prepared.spec.name, "improve_rhyme");
    assert!(matches!(prepared.target, DraftTarget::Replace { .. }));
    let draft = complete(&mock_ai(), prepared, &mut |_| {}).unwrap();
    assert!(
        draft.payload.contains("until the door"),
        "{}",
        draft.payload
    );

    let id = create_proposal(&mut core, draft).unwrap();
    core.execute(Command::ProposalApply { id }).unwrap();
    let after = core.read_document(&rel).unwrap().body().to_string();
    assert!(!after.contains(VERSE), "old verse replaced: {after}");
    assert!(after.contains("until the door"), "{after}");

    // Accepting is undoable: the auto-snapshot restores the exact original.
    core.execute(Command::SnapshotRestore { n: 1 }).unwrap();
    assert_eq!(core.read_document(&rel).unwrap().body(), LYRICS);
}

#[test]
fn scored_prompts_carry_deterministic_numbers_not_model_guesses() {
    let (_tmp, core, rel) = fixture();
    let body = core.read_document(&rel).unwrap().body().to_string();
    let (start, end) = selection_range(&body, VERSE);

    let rhyme = prepare_improve_rhyme(&core, "lantern", start, end, 4096).unwrap();
    let prompt = format!(
        "{}{}",
        rhyme.request.system.as_deref().unwrap_or(""),
        rhyme.request.prompt
    );
    assert!(prompt.contains("end-rhyme scheme: a a"), "{prompt}");

    let meter = prepare_improve_meter(&core, "lantern", start, end, 4096).unwrap();
    let prompt = format!(
        "{}{}",
        meter.request.system.as_deref().unwrap_or(""),
        meter.request.prompt
    );
    assert!(prompt.contains("Current per-line syllables"), "{prompt}");
    assert!(prompt.contains('8'), "{prompt}");
}

#[test]
fn improve_meter_apply_keeps_the_song_parseable() {
    let (_tmp, mut core, rel) = fixture();
    let body = core.read_document(&rel).unwrap().body().to_string();
    let (start, end) = selection_range(&body, VERSE);

    let prepared = prepare_improve_meter(&core, "lantern", start, end, 4096).unwrap();
    let draft = complete(&mock_ai(), prepared, &mut |_| {}).unwrap();
    let id = create_proposal(&mut core, draft).unwrap();
    core.execute(Command::ProposalApply { id }).unwrap();

    let after = core.read_document(&rel).unwrap().body().to_string();
    let lyrics = Lyrics::parse(&after).unwrap();
    assert_eq!(lyrics.sections.len(), 2, "{after}");
    assert_eq!(lyrics.sections[0].base, "Verse");
    // The canned meter rewrite is 8 syllables per line — measured, not claimed.
    let verse_lines: Vec<String> = lyrics.sections[0]
        .lines
        .iter()
        .map(|l| l.raw.clone())
        .collect();
    let scores = score_lines(&verse_lines, "en");
    assert!(scores.line_syllables.iter().all(|n| *n == 8), "{scores:?}");
}

#[test]
fn preserve_hook_keeps_the_hook_line_verbatim() {
    let (_tmp, mut core, rel) = fixture();
    let body = core.read_document(&rel).unwrap().body().to_string();
    let (start, end) = selection_range(&body, CHORUS);

    let prepared = prepare_preserve_hook(&core, "lantern", start, end, 4096).unwrap();
    let prompt = format!(
        "{}{}",
        prepared.request.system.as_deref().unwrap_or(""),
        prepared.request.prompt
    );
    assert!(prompt.contains("Keep the light on"), "{prompt}");

    let draft = complete(&mock_ai(), prepared, &mut |_| {}).unwrap();
    assert!(
        draft.payload.contains("Keep the light on"),
        "{}",
        draft.payload
    );
    let id = create_proposal(&mut core, draft).unwrap();
    core.execute(Command::ProposalApply { id }).unwrap();
    let after = core.read_document(&rel).unwrap().body().to_string();
    assert_eq!(
        Lyrics::parse(&after).unwrap().hook_line().as_deref(),
        Some("Keep the light on")
    );
}

#[test]
fn preserve_hook_refuses_a_selection_without_the_hook() {
    let (_tmp, core, rel) = fixture();
    let body = core.read_document(&rel).unwrap().body().to_string();
    let (start, end) = selection_range(&body, VERSE);
    let err = prepare_preserve_hook(&core, "lantern", start, end, 4096)
        .unwrap_err()
        .to_string();
    assert!(err.contains("not in this selection"), "{err}");
}

#[test]
fn preserve_rhyme_scheme_carries_the_scheme_and_checks_it() {
    let (_tmp, core, rel) = fixture();
    let body = core.read_document(&rel).unwrap().body().to_string();
    // The verse rhymes four/door — scheme a a; a scheme worth preserving.
    let (start, end) = selection_range(&body, VERSE);

    let prepared = prepare_preserve_rhyme_scheme(&core, "lantern", start, end, 4096).unwrap();
    let prompt = format!(
        "{}{}",
        prepared.request.system.as_deref().unwrap_or(""),
        prepared.request.prompt
    );
    assert!(prompt.contains("scheme exactly — a a"), "{prompt}");
    let draft = complete(&mock_ai(), prepared, &mut |_| {}).unwrap();
    // The canned output rhymes score/door — the same two-class scheme.
    let lines: Vec<String> = draft.payload.lines().map(str::to_string).collect();
    assert_eq!(score_lines(&lines, "en").rhyme_scheme, ["a", "a"]);
}

#[test]
fn preserve_rhyme_scheme_refuses_a_selection_without_measurable_rhymes() {
    let (_tmp, core, rel) = fixture();
    // The chorus ends on on/repose — nothing rhymes, nothing to preserve.
    let body = core.read_document(&rel).unwrap().body().to_string();
    let (start, end) = selection_range(&body, CHORUS);
    let err = prepare_preserve_rhyme_scheme(&core, "lantern", start, end, 4096)
        .unwrap_err()
        .to_string();
    assert!(err.contains("do not rhyme with each other"), "{err}");
}

#[test]
fn song_structure_appends_a_plan_when_there_is_none() {
    let (_tmp, mut core, _) = fixture();
    let prepared = prepare_song_structure(&core, "lantern", 4096).unwrap();
    assert!(matches!(
        &prepared.target,
        DraftTarget::Replace { document, .. }
            if document == &std::path::PathBuf::from("music/songs/lantern/song.yaml")
    ));
    assert!(matches!(prepared.format, PayloadFormat::PlanBlock { .. }));

    let draft = complete(&mock_ai(), prepared, &mut |_| {}).unwrap();
    assert!(draft.payload.contains("plan:"), "{}", draft.payload);
    let id = create_proposal(&mut core, draft).unwrap();
    core.execute(Command::ProposalApply { id }).unwrap();

    let yaml_rel = std::path::PathBuf::from("music/songs/lantern/song.yaml");
    let after = core.read_document(&yaml_rel).unwrap().body().to_string();
    let parsed = SongYaml::parse(&after).unwrap();
    assert_eq!(
        parsed.plan.as_deref(),
        Some(
            &[
                "intro".to_string(),
                "verse".to_string(),
                "chorus".to_string(),
                "verse".to_string(),
                "chorus".to_string(),
                "bridge".to_string(),
                "chorus".to_string(),
                "outro".to_string(),
            ][..]
        ),
        "{after}"
    );
    // The other keys survive the replace untouched.
    assert!(after.contains("title: Harbor Lantern"), "{after}");
    assert!(after.contains("type: song"), "{after}");
}

#[test]
fn song_structure_replaces_an_existing_plan_block() {
    let (_tmp, mut core, _) = fixture();
    let yaml_rel = std::path::PathBuf::from("music/songs/lantern/song.yaml");
    let with_plan = "type: song\nid: song_lantern\nplan:\n  - verse\n  - chorus\n\
                     themes: [waiting]\n";
    core.save_body(&yaml_rel, with_plan).unwrap();

    let prepared = prepare_song_structure(&core, "lantern", 4096).unwrap();
    let draft = complete(&mock_ai(), prepared, &mut |_| {}).unwrap();
    let id = create_proposal(&mut core, draft).unwrap();
    core.execute(Command::ProposalApply { id }).unwrap();

    let after = core.read_document(&yaml_rel).unwrap().body().to_string();
    let parsed = SongYaml::parse(&after).unwrap();
    assert_eq!(parsed.plan.as_ref().unwrap().len(), 8, "{after}");
    // No glue between the block and the key that followed it.
    assert!(after.contains("\nthemes: [waiting]"), "{after}");
}

#[test]
fn generate_section_appends_a_heading_section_by_resolved_slug() {
    let (_tmp, mut core, rel) = fixture();
    // The author types the title, not the slug — the target must resolve.
    let prepared = prepare_generate_section(&core, "Harbor Lantern", "Bridge", 4096).unwrap();
    assert!(matches!(
        &prepared.target,
        DraftTarget::Append { document, .. }
            if document == &std::path::PathBuf::from("music/songs/lantern/lyrics.md")
    ));
    let draft = complete(&mock_ai(), prepared, &mut |_| {}).unwrap();
    assert!(
        draft.payload.contains("\n## Bridge\n\n"),
        "{}",
        draft.payload
    );
    assert!(!draft.payload.contains("## Verse"), "{}", draft.payload);

    let id = create_proposal(&mut core, draft).unwrap();
    core.execute(Command::ProposalApply { id }).unwrap();
    let after = core.read_document(&rel).unwrap().body().to_string();
    let lyrics = Lyrics::parse(&after).unwrap();
    assert_eq!(lyrics.sections.len(), 3, "{after}");
    assert_eq!(lyrics.sections[2].base, "Bridge");
}

#[test]
fn rewrite_lyrics_applies_with_and_without_guidance() {
    let (_tmp, mut core, rel) = fixture();
    let body = core.read_document(&rel).unwrap().body().to_string();
    let (start, end) = selection_range(&body, CHORUS);

    let prepared =
        prepare_rewrite_lyrics(&core, "lantern", start, end, Some("warmer"), 4096).unwrap();
    let draft = complete(&mock_ai(), prepared, &mut |_| {}).unwrap();
    assert_eq!(draft.operation, "rewrite_lyrics");
    let id = create_proposal(&mut core, draft).unwrap();
    core.execute(Command::ProposalApply { id }).unwrap();
    let after = core.read_document(&rel).unwrap().body().to_string();
    assert!(after.contains("ferry lamps go out in rows"), "{after}");
}

#[test]
fn rewrite_goes_stale_when_the_selection_is_edited_during_generation() {
    let (_tmp, mut core, rel) = fixture();
    let body = core.read_document(&rel).unwrap().body().to_string();
    let (start, end) = selection_range(&body, VERSE);
    let prepared = prepare_rewrite_lyrics(&core, "lantern", start, end, None, 4096).unwrap();
    let draft = complete(&mock_ai(), prepared, &mut |_| {}).unwrap();
    let id = create_proposal(&mut core, draft).unwrap();

    // The author rewrites the verse while the model was thinking.
    let edited = body.replace(VERSE, "Two gulls argue over nothing much.");
    core.save_body(&rel, &edited).unwrap();
    let events = core
        .execute(Command::ProposalApply { id: id.clone() })
        .unwrap();
    assert!(
        matches!(events[0], Event::ProposalStale { .. }),
        "{events:?}"
    );
    assert_eq!(core.read_document(&rel).unwrap().body(), edited);
}

#[test]
fn concept_alternatives_analyze_and_singability_are_reports() {
    let (_tmp, mut core, rel) = fixture();
    let body = core.read_document(&rel).unwrap().body().to_string();
    let (start, end) = selection_range(&body, VERSE);

    let concept = prepare_song_concept(&core, "lantern", 4096).unwrap();
    assert!(matches!(concept.target, DraftTarget::Report));
    let draft = complete(&mock_ai(), concept, &mut |_| {}).unwrap();
    let err = create_proposal(&mut core, draft).unwrap_err().to_string();
    assert!(err.contains("report"), "{err}");

    let alternatives = prepare_alternatives(&core, "lantern", start, end, 4096).unwrap();
    assert!(
        alternatives.request.schema.is_some(),
        "constrained decoding"
    );
    assert!(matches!(alternatives.target, DraftTarget::Report));
    let draft = complete(&mock_ai(), alternatives, &mut |_| {}).unwrap();
    assert!(draft.payload.starts_with("1. "), "{}", draft.payload);

    let analyze = prepare_analyze_song(&core, "lantern", 4096).unwrap();
    assert!(analyze.request.schema.is_some());
    let draft = complete(&mock_ai(), analyze, &mut |_| {}).unwrap();
    assert!(draft.payload.contains("- structure:"), "{}", draft.payload);

    let singability = prepare_check_singability(&core, "lantern", 4096).unwrap();
    assert!(singability.request.schema.is_some());
    let draft = complete(&mock_ai(), singability, &mut |_| {}).unwrap();
    assert!(draft.payload.starts_with("Singable"), "{}", draft.payload);
}

#[test]
fn schema_reports_survive_fences_and_render() {
    let alt = parse_alternatives(
        "```json\n{\"alternatives\":[\"rows of light\",\"a patient blues\"]}\n```",
    )
    .unwrap();
    assert_eq!(
        render_alternatives(&alt),
        "1. rows of light\n2. a patient blues"
    );
    let obs =
        parse_observations("{\"observations\":[{\"area\":\"meter\",\"note\":\"even\"}]}").unwrap();
    assert_eq!(render_observations(&obs), "- meter: even");
    let sing = parse_singability(
        "{\"verdict\":\"singable\",\"problems\":[{\"location\":\"line 3\",\"issue\":\"12 syllables\"}]}",
    )
    .unwrap();
    assert_eq!(
        render_singability(&sing),
        "singable\n- line 3 — 12 syllables"
    );
}

#[test]
fn render_plan_filters_stray_headers() {
    assert_eq!(
        render_plan("plan:\nverse\nchorus\n"),
        "plan:\n  - verse\n  - chorus"
    );
}
