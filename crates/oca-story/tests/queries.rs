//! Phase 2 exit criteria: all four example queries answered correctly in
//! integration tests over the `examples/` sample novel, no AI involved —
//! plus the v2 additions: flashback fixture ("before chapter 8?" in both
//! time senses), alias fixture ("Ellie" mentions), the "set state at chapter"
//! aid, and the short-story form (A12).
//!
//! Everything runs on a copy of the sample in a temp dir; the committed
//! example is never written to.

use std::path::{Path, PathBuf};

use oca_core::{Command, Core, Event};
use oca_model::{DocumentKind, ProjectKind};
use oca_story::{Story, TimeSense};

const SAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/blackwood");

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

/// A private copy of the sample novel, opened through Core.
fn open_sample() -> (tempfile::TempDir, Core) {
    let tmp = tempfile::tempdir().unwrap();
    let dst = tmp.path().join("blackwood");
    copy_tree(Path::new(SAMPLE), &dst);
    let mut core = Core::new();
    core.execute(Command::OpenProject { root: dst }).unwrap();
    (tmp, core)
}

fn ids(chs: &[oca_story::Chapter]) -> Vec<&str> {
    chs.iter().map(|c| c.id.as_str()).collect()
}

fn event_ids(evs: &[oca_story::Event]) -> Vec<&str> {
    evs.iter().map(|e| e.id.as_str()).collect()
}

// ---- "Where is Elena?" ------------------------------------------------------

#[test]
fn where_is_carries_state_forward_to_the_last_chapter() {
    let (_tmp, core) = open_sample();
    let story = Story::new(&core);
    let view = story.where_is("Elena", None).unwrap();
    assert_eq!(
        view.chapter.id, "ch_010",
        "default = where the story leaves her"
    );
    assert_eq!(
        view.get("location"),
        Some("blackwood"),
        "ch_004 entry carries"
    );
    assert_eq!(view.get("emotional"), Some("frightened"));
}

#[test]
fn where_is_at_an_earlier_chapter_uses_that_eras_state() {
    let (_tmp, core) = open_sample();
    let story = Story::new(&core);
    // Before the ch_004 entry exists on the timeline: only ch_001 applies.
    let view = story.where_is("Elena", Some("3")).unwrap();
    assert_eq!(view.get("location"), Some("village"));
    assert_eq!(view.get("emotional"), Some("isolated"));
    // At the transition chapter itself the new entry already applies.
    let view = story.where_is("Elena", Some("ch_004")).unwrap();
    assert_eq!(view.get("location"), Some("blackwood"));
}

#[test]
fn characters_resolve_by_id_name_and_alias() {
    let (_tmp, core) = open_sample();
    let story = Story::new(&core);
    assert_eq!(story.character("elena").unwrap().name, "Elena");
    assert_eq!(story.character("Elena").unwrap().id, "elena");
    assert_eq!(
        story.character("Ellie").unwrap().id,
        "elena",
        "aliases resolve too"
    );
}

// ---- "Who is Marcus?" -------------------------------------------------------

#[test]
fn who_is_reports_identity_relationships_and_state() {
    let (_tmp, core) = open_sample();
    let story = Story::new(&core);
    let profile = story.who_is("Marcus").unwrap();
    assert_eq!(profile.character.name, "Marcus");
    assert_eq!(profile.character.role.as_deref(), Some("investigator"));
    assert_eq!(profile.character.aliases, vec!["Marc"]);
    assert!(
        profile
            .character
            .relationships
            .iter()
            .any(|r| r.target == "elena" && r.kind == "distrust")
    );
    assert_eq!(profile.at.as_ref().unwrap().id, "ch_010");
    assert_eq!(
        profile.state.get("location").map(String::as_str),
        Some("courthouse"),
        "ch_008 state"
    );
    assert_eq!(
        profile.state.get("emotional").map(String::as_str),
        Some("resolute")
    );
}

// ---- "Which chapters contain Elena?" (alias fixture) ------------------------

#[test]
fn chapters_containing_finds_name_and_aliases_but_not_substrings() {
    let (_tmp, core) = open_sample();
    let story = Story::new(&core);
    let hits = story.chapters_containing("Elena").unwrap();
    // Chapter 5 says only "Ellie"; chapter 9 only "the girl"; chapter 7 says
    // "Elenara" (word-boundary trap) and must not count.
    assert_eq!(
        ids(&hits),
        vec![
            "ch_001", "ch_002", "ch_003", "ch_004", "ch_005", "ch_006", "ch_008", "ch_009",
            "ch_010",
        ]
    );
    assert!(
        !ids(&hits).contains(&"ch_007"),
        "Elenara is not Elena — word boundaries matter"
    );
}

// ---- "What happened before chapter 8?" (flashback fixture, both senses) -----

#[test]
fn before_chapter_8_in_both_time_senses() {
    let (_tmp, core) = open_sample();
    let story = Story::new(&core);

    let narrative = story.events_before("ch_008", TimeSense::Narrative).unwrap();
    assert_eq!(
        event_ids(&narrative),
        vec![
            "ev_arrival",
            "ev_letter",
            "ev_forest",
            "ev_market",
            "ev_moot",
            "ev_elder"
        ],
        "narrative sense: events shown in chapters 1-7, in book order"
    );
    assert!(
        !event_ids(&narrative).contains(&"ev_flashback"),
        "the chapter-10 flashback is shown after chapter 8"
    );

    let chronology = story.events_before("ch_008", TimeSense::StoryTime).unwrap();
    assert_eq!(
        event_ids(&chronology),
        vec![
            "ev_flashback",
            "ev_arrival",
            "ev_letter",
            "ev_forest",
            "ev_market",
            "ev_elder"
        ],
        "story sense: in-world chronology before the ch_008 anchor"
    );
    assert!(
        !event_ids(&chronology).contains(&"ev_moot"),
        "the chapter-6 flash-forward happens after the trial"
    );

    // The two clocks disagree exactly where flashback/flash-forward demand it.
    assert!(event_ids(&narrative).contains(&"ev_moot"));
    assert!(event_ids(&chronology).contains(&"ev_flashback"));
}

#[test]
fn story_clock_needs_the_targets_story_time_anchor() {
    let (_tmp, core) = open_sample();
    let story = Story::new(&core);
    let err = story
        .events_before("ch_001", TimeSense::StoryTime)
        .unwrap_err();
    assert!(
        err.to_string().contains("story_time"),
        "actionable error, got: {err}"
    );
}

// ---- chapter key forms (what authors actually type) -------------------------

#[test]
fn chapters_resolve_by_number_id_title_and_path() {
    let (_tmp, core) = open_sample();
    let story = Story::new(&core);
    for key in [
        "8",
        "ch_008",
        "The Trial",
        "008-the-trial",
        "manuscript/008-the-trial.md",
    ] {
        assert_eq!(story.chapter(key).unwrap().id, "ch_008", "key {key:?}");
    }
    assert!(story.chapter("ch_999").is_err());
}

// ---- "set state at this chapter" (the authoring aid) ------------------------

#[test]
fn set_state_upserts_merges_and_stays_lossless() {
    let (tmp, mut core) = open_sample();
    let rel = oca_story::set_state(
        &mut core,
        "Elena",
        "ch_006",
        &[("location", "harbor"), ("emotional", "wary")],
    )
    .unwrap();
    assert_eq!(rel, PathBuf::from("characters/elena.md"));

    {
        let story = Story::new(&core);
        assert_eq!(
            story.where_is("Elena", Some("6")).unwrap().get("location"),
            Some("harbor")
        );
        assert_eq!(
            story.where_is("Elena", None).unwrap().get("location"),
            Some("harbor"),
            "the new entry carries forward to the last chapter"
        );
        assert_eq!(
            story.where_is("Elena", Some("5")).unwrap().get("location"),
            Some("blackwood"),
            "earlier chapters keep their era's state"
        );
    }

    // A second set at the same chapter merges instead of duplicating.
    oca_story::set_state(&mut core, "Elena", "ch_006", &[("goal", "find_mother")]).unwrap();
    let story = Story::new(&core);
    let at_6 = story.where_is("Elena", Some("6")).unwrap();
    assert_eq!(at_6.get("location"), Some("harbor"));
    assert_eq!(at_6.get("goal"), Some("find_mother"));

    // The patch touched only the states block: comments and unknown keys
    // survive, and there is exactly one entry per chapter.
    let raw = std::fs::read_to_string(tmp.path().join("blackwood/characters/elena.md")).unwrap();
    assert!(raw.contains("# Elena is the protagonist"));
    assert!(raw.contains("favorite_color: green"));
    assert_eq!(raw.matches("at: ch_006").count(), 1);
    assert_eq!(
        raw.matches("at: ch_001").count(),
        1,
        "old entries keep their bytes"
    );
}

// ---- short story as a supported form (A12) ----------------------------------

#[test]
fn short_story_form_supports_story_queries() {
    let tmp = tempfile::tempdir().unwrap();
    let mut core = Core::new();
    core.execute(Command::CreateProject {
        root: tmp.path().join("flash"),
        title: "Flash".to_string(),
        kind: ProjectKind::ShortStory,
    })
    .unwrap();
    let events = core
        .execute(Command::CreateDocument {
            kind: DocumentKind::Chapter,
            title: "One Moment".to_string(),
        })
        .unwrap();
    let Event::DocumentCreated { rel: chapter, .. } = &events[0] else {
        panic!("expected DocumentCreated");
    };
    let chapter = chapter.clone();
    core.execute(Command::AppendParagraph {
        doc: chapter.clone(),
        text: "Nora waited by the door.".to_string(),
    })
    .unwrap();
    core.execute(Command::CreateDocument {
        kind: DocumentKind::Character,
        title: "Nora".to_string(),
    })
    .unwrap();

    {
        let chs = Story::new(&core).chapters().unwrap();
        assert_eq!(chs.len(), 1, "a short story is a one-chapter project");
        oca_story::set_state(&mut core, "Nora", &chs[0].id, &[("location", "door")]).unwrap();
    }

    let story = Story::new(&core);
    assert_eq!(story.chapters_containing("Nora").unwrap().len(), 1);
    let view = story.where_is("Nora", None).unwrap();
    assert_eq!(view.get("location"), Some("door"));
}
