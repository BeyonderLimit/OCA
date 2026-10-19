//! Phase 10 music operations graded deterministically against the mock
//! model (A8, AGENTS "AI evals"): the exit criteria's "rhyme/meter
//! scores computed for both" plus the roadmap graders PRESERVE_HOOK and
//! PRESERVE_RHYME_SCHEME. No LLM-as-judge, no real model.

use std::sync::Arc;

use oca_ai::{
    Ai, DEFAULT_BUDGET, GenerationRequest, MockModel, complete, create_proposal,
    parse_observations, parse_singability, prepare_alternatives, prepare_analyze_song,
    prepare_check_singability, prepare_improve_rhyme, prepare_preserve_hook,
};
use oca_core::Command;
use oca_eval::{
    Grader, NonEmpty, PreservesHook, PreservesRhymeScheme, SchemaValidity, song_hook, song_project,
};
use oca_music::score_lines;
use tempfile::TempDir;

const LYRICS_REL: &str = "music/songs/lantern/lyrics.md";
const VERSE: &str = "The ferry lamps go out at four\nthe harbor locks its own back door";
const CHORUS: &str = "Keep the light on\nthe kettle keeps its slow repose";

fn mock_ai() -> Ai {
    let ai = Ai::new(Arc::new(MockModel::music_defaults()), Vec::new());
    ai.load().unwrap();
    ai
}

/// The raw model generation — what the graders measure.
fn raw_generation(ai: &Ai, request: &GenerationRequest) -> String {
    let mut raw = String::new();
    ai.generate_with(request, &mut |token| raw.push_str(token))
        .unwrap();
    raw
}

fn lyrics_body(core: &oca_core::Core) -> String {
    core.read_document(&std::path::PathBuf::from(LYRICS_REL))
        .unwrap()
        .body()
        .to_string()
}

fn selection_range(body: &str, needle: &str) -> (usize, usize) {
    let start = body.find(needle).expect("selection in fixture");
    (start, start + needle.len())
}

#[test]
fn eval_improve_rhyme_preserves_the_rhyme_scheme() {
    let tmp = TempDir::new().unwrap();
    let mut core = song_project(tmp.path()).unwrap();
    let ai = mock_ai();

    // The scheme to preserve is measured over the fixture, not assumed.
    let verse_lines: Vec<String> = VERSE.lines().map(str::to_string).collect();
    let original = score_lines(&verse_lines, "en");
    assert_eq!(original.rhyme_scheme, ["a", "a"]);

    let body = lyrics_body(&core);
    let (start, end) = selection_range(&body, VERSE);
    let prepared = prepare_improve_rhyme(&core, "lantern", start, end, DEFAULT_BUDGET).unwrap();

    let grade = PreservesRhymeScheme {
        scheme: original.rhyme_scheme.clone(),
        language: "en".into(),
    }
    .grade(&raw_generation(&ai, &prepared.request));
    assert!(grade.passed, "{grade}");

    // The graded output still completes the proposal loop.
    let draft = complete(&ai, prepared, &mut |_| {}).unwrap();
    let id = create_proposal(&mut core, draft).unwrap();
    core.execute(Command::ProposalApply { id }).unwrap();
    assert!(lyrics_body(&core).contains("until the door"));
}

#[test]
fn eval_preserve_hook_keeps_the_hook_through_apply() {
    let tmp = TempDir::new().unwrap();
    let mut core = song_project(tmp.path()).unwrap();
    let ai = mock_ai();

    let hook = song_hook(&core, "lantern").unwrap();
    assert_eq!(hook, "Keep the light on");

    let body = lyrics_body(&core);
    let (start, end) = selection_range(&body, CHORUS);
    let prepared = prepare_preserve_hook(&core, "lantern", start, end, DEFAULT_BUDGET).unwrap();

    let grade = PreservesHook { hook: hook.clone() }.grade(&raw_generation(&ai, &prepared.request));
    assert!(grade.passed, "{grade}");

    let draft = complete(&ai, prepared, &mut |_| {}).unwrap();
    let id = create_proposal(&mut core, draft).unwrap();
    core.execute(Command::ProposalApply { id }).unwrap();
    // The applied chorus still opens with the same hook line.
    assert_eq!(song_hook(&core, "lantern").unwrap(), hook);
}

/// The graders must fail bad output too — a suite that cannot fail is
/// not measuring anything.
#[test]
fn music_graders_fail_bad_output() {
    let dropped = PreservesHook {
        hook: "Keep the light on".into(),
    }
    .grade("hold the lamp up\nthe kettle hums");
    assert!(!dropped.passed, "{dropped}");
    assert!(dropped.detail.contains("0 time"), "{dropped}");

    let no_hook = PreservesHook::default().grade("anything");
    assert!(!no_hook.passed, "{no_hook}");

    let broke = PreservesRhymeScheme {
        scheme: vec!["a".into(), "a".into()],
        language: "en".into(),
    }
    .grade("rows of light\nand quiet sand");
    assert!(!broke.passed, "{broke}");
    assert!(broke.detail.contains("want a a"), "{broke}");

    // An original with nothing rhyming: no vacuous pass.
    let vacuous = PreservesRhymeScheme {
        scheme: vec!["a".into(), "b".into()],
        language: "en".into(),
    }
    .grade("anything at all\nwith two lines");
    assert!(!vacuous.passed, "{vacuous}");
}

#[test]
fn eval_song_reports_are_schema_valid_and_alternatives_non_empty() {
    let tmp = TempDir::new().unwrap();
    let core = song_project(tmp.path()).unwrap();
    let ai = mock_ai();
    let body = lyrics_body(&core);
    let (start, end) = selection_range(&body, VERSE);

    let analyze = prepare_analyze_song(&core, "lantern", DEFAULT_BUDGET).unwrap();
    let raw = raw_generation(&ai, &analyze.request);
    let grade = SchemaValidity {
        required: &["observations"],
    }
    .grade(&raw);
    assert!(grade.passed, "{grade}");
    let observations = parse_observations(&raw).expect("graded-valid JSON parses");
    assert!(!observations.observations.is_empty());

    let sing = prepare_check_singability(&core, "lantern", DEFAULT_BUDGET).unwrap();
    let raw = raw_generation(&ai, &sing.request);
    let grade = SchemaValidity {
        required: &["verdict", "problems"],
    }
    .grade(&raw);
    assert!(grade.passed, "{grade}");
    let singability = parse_singability(&raw).expect("graded-valid JSON parses");
    assert!(singability.problems.is_empty(), "{singability:?}");

    let alternatives = prepare_alternatives(&core, "lantern", start, end, DEFAULT_BUDGET).unwrap();
    let raw = raw_generation(&ai, &alternatives.request);
    let grade = NonEmpty.grade(&raw);
    assert!(grade.passed, "{grade}");
    assert!(raw.contains("alternatives"), "{raw}");
}
