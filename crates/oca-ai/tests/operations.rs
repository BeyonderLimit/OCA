//! Phase 9 end-to-end operation tests: prepare → complete (mock model) →
//! proposal → accept/edit/stale → snapshot restore. Rule 6: every
//! generation here runs against `MockModel`, never a real model.

use std::sync::Arc;

use oca_ai::{
    Ai, MockModel, PayloadFormat, complete, create_proposal, parse_outline, parse_review,
    prepare_continue_scene, prepare_generate_outline, prepare_review_chapter,
    prepare_rewrite_selection, prepare_summarize_chapter, render_outline, render_review,
};
use oca_core::{Command, Core, Event, ProposalStatus};
use oca_model::{DocumentKind, ProjectKind};
use tempfile::TempDir;

const BODY: &str = "\
## The Hall

Elena read the letter again by lamplight; the ink ran where the paper had been folded.

## After

Nobody spoke of it at supper.
";

fn chapter_body_end_of_first_scene(body: &str) -> usize {
    body.find("\n\n## After").expect("fixture has two scenes")
}

/// A one-chapter project (two scenes) plus one character with state.
fn fixture() -> (TempDir, Core, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let mut core = Core::new();
    core.execute(Command::CreateProject {
        root: tmp.path().to_path_buf(),
        title: "Test Story".into(),
        kind: ProjectKind::Novel,
    })
    .unwrap();
    let events = core
        .execute(Command::CreateDocument {
            kind: DocumentKind::Chapter,
            title: "The Letter".into(),
        })
        .unwrap();
    let Event::DocumentCreated { rel, .. } = &events[0] else {
        panic!("expected DocumentCreated: {events:?}");
    };
    let rel = rel.clone();
    core.save_body(&rel, BODY).unwrap();

    std::fs::create_dir_all(tmp.path().join("characters")).unwrap();
    std::fs::write(
        tmp.path().join("characters/elena.md"),
        "---\nid: char_elena\nname: Elena\nrole: protagonist\naliases: [Ellie]\n\
         relationships:\n  marcus:\n    type: distrust\nstates:\n  - at: ch_001\n    \
         location: the hall\n---\n",
    )
    .unwrap();
    core.refresh_index().unwrap();
    (tmp, core, rel)
}

fn mock_ai() -> Ai {
    let ai = Ai::new(Arc::new(MockModel::story_defaults()), Vec::new());
    ai.load().unwrap();
    ai
}

#[test]
fn continue_scene_inserts_at_the_cursor_and_restores() {
    let (_tmp, mut core, rel) = fixture();
    let at = chapter_body_end_of_first_scene(BODY);
    let prepared = prepare_continue_scene(&core, "1", at, 4096).unwrap();
    assert!(matches!(
        prepared.target,
        oca_ai::DraftTarget::Replace { .. }
    ));
    assert!(matches!(prepared.format, PayloadFormat::FreeText));

    let draft = complete(&mock_ai(), prepared, &mut |_| {}).unwrap();
    assert_eq!(draft.operation, "continue_scene");
    assert_eq!(draft.prompt_version, "continue-scene/1");
    assert!(draft.payload.contains("storm broke"), "{}", draft.payload);

    let id = create_proposal(&mut core, draft).unwrap();
    let events = core.execute(Command::ProposalApply { id }).unwrap();
    assert!(
        matches!(events[0], Event::ProposalApplied { rebased: false, .. }),
        "{events:?}"
    );
    let after = core.read_document(&rel).unwrap().body().to_string();
    assert!(
        after.contains("had been folded. The storm broke"),
        "{after}"
    );

    // Accepting is undoable: the auto-snapshot restores the exact original.
    core.execute(Command::SnapshotRestore { n: 1 }).unwrap();
    assert_eq!(core.read_document(&rel).unwrap().body(), BODY);
}

#[test]
fn rewrite_selection_applies_and_rebases() {
    let (_tmp, mut core, rel) = fixture();
    let selection = "Elena read the letter again by lamplight";
    let start = BODY.find(selection).unwrap();
    let end = start + selection.len();

    let prepared =
        prepare_rewrite_selection(&core, "1", start, end, Some("tighter"), 4096).unwrap();
    assert_eq!(prepared.spec.name, "rewrite_selection");
    let draft = complete(&mock_ai(), prepared, &mut |_| {}).unwrap();
    assert!(draft.payload.contains("Elena"), "{}", draft.payload);

    // The proposal records the anchor against the author's original text…
    let id = create_proposal(&mut core, draft).unwrap();

    // …then the author keeps typing elsewhere before accepting: the recorded
    // range no longer holds the anchor, but the anchor survives once → rebase.
    let shifted = format!("That evening, {BODY}");
    core.save_body(&rel, &shifted).unwrap();

    let events = core.execute(Command::ProposalApply { id }).unwrap();
    assert!(
        matches!(events[0], Event::ProposalApplied { rebased: true, .. }),
        "{events:?}"
    );
    let after = core.read_document(&rel).unwrap().body().to_string();
    assert!(
        !after.contains(selection),
        "old selection was replaced: {after}"
    );
    assert!(after.contains("Elena read the letter twice"), "{after}");
    assert!(after.starts_with("That evening,"), "{after}");
}

#[test]
fn rewrite_goes_stale_when_the_selection_itself_is_edited() {
    let (_tmp, mut core, rel) = fixture();
    let selection = "Elena read the letter again by lamplight";
    let start = BODY.find(selection).unwrap();
    let end = start + selection.len();
    let prepared = prepare_rewrite_selection(&core, "1", start, end, None, 4096).unwrap();
    let draft = complete(&mock_ai(), prepared, &mut |_| {}).unwrap();
    let id = create_proposal(&mut core, draft).unwrap();

    // The author rewrites the selection sentence itself while the model
    // was thinking — apply must refuse, never overwrite the new text.
    let edited = BODY.replace(selection, "She put the kettle on instead.");
    core.save_body(&rel, &edited).unwrap();

    let events = core
        .execute(Command::ProposalApply { id: id.clone() })
        .unwrap();
    assert!(
        matches!(events[0], Event::ProposalStale { .. }),
        "{events:?}"
    );
    assert_eq!(
        core.proposal(&id).unwrap().unwrap().status,
        ProposalStatus::Stale
    );
    assert_eq!(core.read_document(&rel).unwrap().body(), edited);
}

#[test]
fn summarize_appends_to_the_notes_file_on_accept() {
    let (_tmp, mut core, _) = fixture();
    let prepared = prepare_summarize_chapter(&core, "1", 4096).unwrap();
    assert!(
        matches!(
            &prepared.target,
            oca_ai::DraftTarget::Append { document, .. } if document == &std::path::PathBuf::from("notes/summaries.md")
        ),
        "{:?}",
        prepared.target
    );
    let draft = complete(&mock_ai(), prepared, &mut |_| {}).unwrap();
    assert!(draft.payload.starts_with("## "), "{}", draft.payload);

    // Before accept: the notes file doesn't exist yet (empty container only).
    let id = create_proposal(&mut core, draft).unwrap();
    let proposal = core.proposal(&id).unwrap().unwrap();
    assert_eq!(
        proposal.target.document,
        std::path::PathBuf::from("notes/summaries.md")
    );

    core.execute(Command::ProposalApply { id }).unwrap();
    let notes = core
        .read_document(&std::path::PathBuf::from("notes/summaries.md"))
        .unwrap()
        .body()
        .to_string();
    assert!(notes.contains("## The Letter"), "{notes}");
    assert!(notes.contains("Elena receives a letter"), "{notes}");
}

#[test]
fn outline_parses_schema_json_and_renders_markdown() {
    // The fence-repair step: models wrap JSON in ```json fences.
    let fenced = "```json\n{\"sections\":[{\"title\":\"Act I\",\"beats\":[\"a\"]}]}\n```";
    let outline = parse_outline(fenced).unwrap();
    assert_eq!(render_outline(&outline), "## Act I\n- a");

    let (_tmp, mut core, _) = fixture();
    let prepared = prepare_generate_outline(&core, 4096).unwrap();
    assert!(
        prepared.request.schema.is_some(),
        "constrained decoding (A8)"
    );
    let draft = complete(&mock_ai(), prepared, &mut |_| {}).unwrap();
    assert!(
        draft.payload.contains("## Act I — The Letter"),
        "{}",
        draft.payload
    );
    assert!(
        draft.payload.contains("- Elena receives the letter"),
        "{}",
        draft.payload
    );

    let id = create_proposal(&mut core, draft).unwrap();
    core.execute(Command::ProposalApply { id }).unwrap();
    let outline_doc = core
        .read_document(&std::path::PathBuf::from("outline/outline.md"))
        .unwrap()
        .body()
        .to_string();
    assert!(
        outline_doc.contains("The elders convene at the hall"),
        "{outline_doc}"
    );
}

#[test]
fn review_is_a_report_never_a_proposal() {
    let (_tmp, core, _) = fixture();
    let prepared = prepare_review_chapter(&core, "1", 4096).unwrap();
    assert!(prepared.request.schema.is_some());
    assert!(matches!(prepared.target, oca_ai::DraftTarget::Report));

    let mut core = core;
    let draft = complete(&mock_ai(), prepared, &mut |_| {}).unwrap();
    let err = create_proposal(&mut core, draft).unwrap_err().to_string();
    assert!(err.contains("report"), "{err}");

    // The structured output parses and renders for the author.
    let review = parse_review(
        "{\"issues\":[{\"type\":\"timeline\",\"severity\":\"warning\",\
         \"location\":\"paragraph 2\",\"explanation\":\"clash\"}]}",
    )
    .unwrap();
    assert_eq!(
        render_review(&review),
        "warning [timeline] paragraph 2 — clash"
    );
}

#[test]
fn invalid_schema_output_is_a_clean_error_not_a_mangled_draft() {
    let (_tmp, core, _) = fixture();
    let prepared = prepare_review_chapter(&core, "1", 4096).unwrap();
    // A model that answers prose to a schema request: refuse, don't guess.
    let ai = Ai::new(
        Arc::new(MockModel::new().with_tokens(vec!["Looks".into(), " fine!".into()])),
        Vec::new(),
    );
    ai.load().unwrap();
    let err = complete(&ai, prepared, &mut |_| {})
        .unwrap_err()
        .to_string();
    assert!(err.contains("issue list"), "{err}");
}

#[test]
fn operation_prompts_carry_provenance_and_budget() {
    let (_tmp, core, _) = fixture();
    let prepared = prepare_summarize_chapter(&core, "1", 4096).unwrap();
    assert!(!prepared.manifest.is_empty());
    assert!(
        prepared.manifest.iter().any(|m| m.label == "chapter"),
        "{:?}",
        prepared.manifest
    );
    let prompt = format!(
        "{}{}",
        prepared.request.system.as_deref().unwrap_or(""),
        prepared.request.prompt
    );
    assert!(prompt.contains("Operation: summarize_chapter"), "{prompt}");
    assert!(prompt.contains("<context"), "{prompt}");
}
