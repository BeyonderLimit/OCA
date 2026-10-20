//! Every Phase 9 operation run end-to-end against the mock model, then
//! graded deterministically. No LLM-as-judge, no real model: the same
//! grades come out on any machine (A8, AGENTS "AI evals").

use std::path::Path;
use std::sync::Arc;

use oca_ai::{
    Ai, DEFAULT_BUDGET, GenerationRequest, MockModel, complete, create_proposal, parse_outline,
    parse_review, prepare_continue_scene, prepare_generate_outline, prepare_review_chapter,
    prepare_rewrite_selection, prepare_summarize_chapter,
};
use oca_core::Command;
use oca_eval::{
    CHAPTER_TWO_SCENES, CONTINUITY_SEEDS, ContinuityDetection, Grader, NonEmpty,
    PreservesNamedEntities, SchemaValidity, SummaryLength, character_names, continuity_project,
    story_project,
};
use oca_story::{check, render};
use tempfile::TempDir;

fn mock_ai() -> Ai {
    let ai = Ai::new(Arc::new(MockModel::story_defaults()), Vec::new());
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

fn scene_end() -> usize {
    CHAPTER_TWO_SCENES
        .find("\n\n## After")
        .expect("fixture has two scenes")
}

#[test]
fn eval_continue_scene() {
    let tmp = TempDir::new().unwrap();
    let mut core = story_project(tmp.path()).unwrap();
    let ai = mock_ai();

    let prepared = prepare_continue_scene(&core, "1", scene_end(), DEFAULT_BUDGET, None).unwrap();
    let grade = NonEmpty.grade(&raw_generation(&ai, &prepared.request));
    assert!(grade.passed, "{grade}");

    // …and the graded output still completes the full proposal loop.
    let draft = complete(&ai, prepared, &mut |_| {}).unwrap();
    let id = create_proposal(&mut core, draft).unwrap();
    core.execute(Command::ProposalApply { id }).unwrap();
    let body = core
        .read_document(&std::path::PathBuf::from("manuscript/001-the-hall.md"))
        .unwrap()
        .body()
        .to_string();
    assert!(body.contains("had been folded. The storm broke"), "{body}");
}

#[test]
fn eval_rewrite_selection_preserves_named_entities() {
    let tmp = TempDir::new().unwrap();
    let core = story_project(tmp.path()).unwrap();
    let ai = mock_ai();

    let selection = "Elena read the letter again by lamplight";
    let start = CHAPTER_TWO_SCENES.find(selection).unwrap();
    let end = start + selection.len();

    // The entities to preserve come from the story engine, not guessing.
    let names = character_names(&core).unwrap();
    let entities: Vec<String> = names
        .into_iter()
        .filter(|n| selection.contains(n.as_str()))
        .collect();
    assert_eq!(entities, ["Elena"]);

    let prepared =
        prepare_rewrite_selection(&core, "1", start, end, Some("tighter"), DEFAULT_BUDGET).unwrap();
    let grade = PreservesNamedEntities {
        entities: entities.clone(),
    }
    .grade(&raw_generation(&ai, &prepared.request));
    assert!(grade.passed, "{grade}");

    // The graded rewrite still replaces the selection when accepted.
    let mut core = core;
    let draft = complete(&ai, prepared, &mut |_| {}).unwrap();
    let id = create_proposal(&mut core, draft).unwrap();
    core.execute(Command::ProposalApply { id }).unwrap();
}

#[test]
fn eval_summarize_chapter_hits_the_length_band() {
    let tmp = TempDir::new().unwrap();
    let core = story_project(tmp.path()).unwrap();
    let ai = mock_ai();

    let prepared = prepare_summarize_chapter(&core, "1", DEFAULT_BUDGET).unwrap();
    let grade = SummaryLength {
        min_words: 10,
        max_words: 60,
    }
    .grade(&raw_generation(&ai, &prepared.request));
    assert!(grade.passed, "{grade}");

    let draft = complete(&ai, prepared, &mut |_| {}).unwrap();
    assert!(
        draft.payload.starts_with("## The Hall\n"),
        "{}",
        draft.payload
    );
}

#[test]
fn eval_generate_outline_is_schema_valid() {
    let tmp = TempDir::new().unwrap();
    let core = story_project(tmp.path()).unwrap();
    let ai = mock_ai();

    let prepared = prepare_generate_outline(&core, DEFAULT_BUDGET).unwrap();
    let raw = raw_generation(&ai, &prepared.request);
    let grade = SchemaValidity {
        required: &["sections"],
    }
    .grade(&raw);
    assert!(grade.passed, "{grade}");
    let outline = parse_outline(&raw).expect("graded-valid JSON also parses as an outline");
    assert!(!outline.sections.is_empty());
}

#[test]
fn eval_review_chapter_is_schema_valid() {
    let tmp = TempDir::new().unwrap();
    let core = story_project(tmp.path()).unwrap();
    let ai = mock_ai();

    let prepared = prepare_review_chapter(&core, "1", DEFAULT_BUDGET).unwrap();
    let raw = raw_generation(&ai, &prepared.request);
    let grade = SchemaValidity {
        required: &["issues"],
    }
    .grade(&raw);
    assert!(grade.passed, "{grade}");
    let review = parse_review(&raw).expect("graded-valid JSON also parses as a review");
    assert!(!review.issues.is_empty());
    for issue in &review.issues {
        assert!(!issue.kind.is_empty() && !issue.explanation.is_empty());
    }
}

/// The graders must fail bad output too — a suite that cannot fail is not
/// measuring anything.
#[test]
fn evals_fail_a_model_that_answers_prose_to_a_schema() {
    let tmp = TempDir::new().unwrap();
    let core = story_project(tmp.path()).unwrap();
    let ai = Ai::new(
        Arc::new(MockModel::new().with_tokens(vec!["Looks".into(), " fine!".into()])),
        Vec::new(),
    );
    ai.load().unwrap();

    let prepared = prepare_review_chapter(&core, "1", DEFAULT_BUDGET).unwrap();
    let grade = SchemaValidity {
        required: &["issues"],
    }
    .grade(&raw_generation(&ai, &prepared.request));
    assert!(!grade.passed, "{grade}");
}

/// Every file under `root`, sorted — for proving a detector never wrote.
fn snapshot_bytes(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                out.push((
                    path.strip_prefix(root).unwrap().display().to_string(),
                    std::fs::read(&path).unwrap(),
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// Phase 12 exit criteria: the seeded contradictions are detected with
/// their planted severities and references, no false HIGH on the clean
/// surroundings, byte-identical output across runs — and not one byte of
/// the project changes (reports only, zero silent fixes).
#[test]
fn eval_continuity_detects_seeded_contradictions() {
    let tmp = TempDir::new().unwrap();
    let core = continuity_project(tmp.path()).unwrap();

    let before = snapshot_bytes(tmp.path());
    let report = render(&check(&core).unwrap());
    assert_eq!(
        before,
        snapshot_bytes(tmp.path()),
        "the detector must never write — reports only"
    );

    let grade = ContinuityDetection {
        seeds: CONTINUITY_SEEDS,
    }
    .grade(&report);
    assert!(grade.passed, "{grade}\n--- report ---\n{report}");

    // Deterministic: the same project, the same report.
    assert_eq!(report, render(&check(&core).unwrap()));
}
