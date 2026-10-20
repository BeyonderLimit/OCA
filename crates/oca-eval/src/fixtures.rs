//! The seeded fixture projects: fixed bytes, no RNG — "seeded" here means
//! deterministic, so an eval on CI reproduces an eval run anywhere.

use std::path::Path;

use anyhow::{Result, bail};
use oca_core::{Command, Core, Event};
use oca_model::{DocumentKind, ProjectKind};

/// The fixture chapter: two scenes, named characters in the prose, a
/// letter, and a door to watch.
pub const CHAPTER_TWO_SCENES: &str = "\
## The Hall

Elena read the letter again by lamplight; the ink ran where the paper had been folded.

## After

Nobody spoke of it at supper — Marcus watched the door instead.
";

const ELENA: &str = "\
---
id: char_elena
name: Elena
role: protagonist
aliases: [Ellie]
relationships:
  marcus:
    type: distrust
states:
  - at: ch_001
    location: the hall
---
";

const MARCUS: &str = "\
---
id: char_marcus
name: Marcus
role: deuteragonist
states:
  - at: ch_001
    location: the doorway
---
";

/// Create the seeded story project under `root` and return an open Core
/// over it: one two-scene chapter plus two characters with state.
pub fn story_project(root: &Path) -> Result<Core> {
    std::fs::create_dir_all(root)?;
    let mut core = Core::new();
    core.execute(Command::CreateProject {
        root: root.to_path_buf(),
        title: "The Letter".into(),
        kind: ProjectKind::Novel,
    })?;
    let events = core.execute(Command::CreateDocument {
        kind: DocumentKind::Chapter,
        title: "The Hall".into(),
    })?;
    let Event::DocumentCreated { rel, .. } = &events[0] else {
        bail!("expected DocumentCreated, got {events:?}");
    };
    core.save_body(rel, CHAPTER_TWO_SCENES)?;
    std::fs::create_dir_all(root.join("characters"))?;
    std::fs::write(root.join("characters/elena.md"), ELENA)?;
    std::fs::write(root.join("characters/marcus.md"), MARCUS)?;
    core.refresh_index()?;
    Ok(core)
}

// ---- Phase 12: the seeded-contradiction novel ------------------------------
//
// One planted issue per continuity check, plus clean surroundings (a
// defined world location, a resolving wiki link, sortable story times) so
// that any *extra* HIGH issue the detector reports is a false positive the
// grader fails on. Chapter ids are pinned so `at:` / `chapter:` keys read
// like a real project, not ULIDs.

const CONTINUITY_CHAPTER_ONE: &str = "\
---
id: ch_alpha
title: The Arrival
---

# The Arrival

Elena read the letter again. A margin note pointed at [[ghost-doc]].
";

const CONTINUITY_CHAPTER_TWO: &str = "\
---
id: ch_beta
title: The Trial
story_time: '1888-02-02'
---

# The Trial

Marcus kept the door. Barnaby counted the votes.
";

const CONTINUITY_ELENA: &str = "\
---
id: elena
title: Elena
name: Elena
aliases: [Ellie, \"the girl\"]
relationships:
  barnaby:
    type: trust
states:
  - at: ch_alpha
    location: blackwood
    emotional: waiting
  - at: ch_gone
    location: nowhere
  - at: ch_alpha
    emotional: restless
  - at: ch_beta
    location: Blackwood
---

# Elena

Protagonist; her states carry forward by chapter (A11).
";

const CONTINUITY_BARNABY: &str = "\
---
id: barnaby
title: Barnaby
name: Barnaby
aliases: [\"the girl\"]
relationships:
  elena:
    type: distrust
  ghost:
    type: fears
---

# Barnaby

He counts everything, including grudges.
";

const CONTINUITY_WORLD: &str = "\
---
id: world_blackwood
title: Blackwood
---

# Blackwood

A forest with rules of its own.
";

const CONTINUITY_TIMELINE: &str = "\
---
events:
  - id: ev_ok
    title: The arrival
    story_time: '1888-01-01'
    chapter: ch_alpha
  - id: ev_lost
    title: The vanished scene
    story_time: '1888-01-05'
    chapter: ch_99
  - id: ev_dup
    title: First telling
    story_time: '1888-01-06'
  - id: ev_dup
    title: Second telling
    story_time: '1888-01-07'
  - id: ev_march
    title: The marching
    story_time: March 1888
    chapter: ch_alpha
---

# Timeline
";

/// The seeded-contradiction project (Phase 12 exit criteria): every issue
/// planted on purpose, listed in `GRADED_SEEDS` alongside what must stay
/// quiet.
pub fn continuity_project(root: &Path) -> Result<Core> {
    std::fs::create_dir_all(root)?;
    let mut core = Core::new();
    core.execute(Command::CreateProject {
        root: root.to_path_buf(),
        title: "The Long Season".into(),
        kind: ProjectKind::Novel,
    })?;
    for (rel, body) in [
        ("manuscript/001-the-arrival.md", CONTINUITY_CHAPTER_ONE),
        ("manuscript/002-the-trial.md", CONTINUITY_CHAPTER_TWO),
        ("characters/elena.md", CONTINUITY_ELENA),
        ("characters/barnaby.md", CONTINUITY_BARNABY),
        ("world/blackwood.md", CONTINUITY_WORLD),
        ("outline/timeline.md", CONTINUITY_TIMELINE),
    ] {
        core.execute(Command::SaveDocument {
            doc: rel.into(),
            raw: body.into(),
        })?;
    }
    core.refresh_index()?;
    Ok(core)
}

/// What the detector must say about [`continuity_project`]: `(issue id,
/// severity label)`. Every seed is a contradiction planted on purpose;
/// the grader additionally forbids any `HIGH` *outside* this list, so a
/// detector that cries wolf on the clean surroundings fails.
pub const CONTINUITY_SEEDS: &[(&str, &str)] = &[
    // HIGH — broken references / definite contradictions.
    ("character/unknown-state-chapter|elena|ch_gone", "HIGH"),
    ("timeline/event-unknown-chapter|ev_lost", "HIGH"),
    ("timeline/duplicate-event-id|ev_dup", "HIGH"),
    ("relationship/target-missing|barnaby|ghost", "HIGH"),
    (
        "object/broken-wiki-link|manuscript/001-the-arrival.md|ghost-doc",
        "HIGH",
    ),
    // WARN — ambiguity and drift the author should look at.
    (
        "character/duplicate-state-field|elena|ch_alpha|emotional",
        "WARN",
    ),
    ("character/alias-collision|girl", "WARN"),
    ("location/spelling-drift|blackwood", "WARN"),
    ("timeline/unsortable-story-time|ev_march", "WARN"),
    // INFO — honest nudges only.
    ("relationship/reciprocal-mismatch|barnaby|elena", "INFO"),
    ("timeline/chapter-missing-story-time|coverage", "INFO"),
    ("world/location-undefined|nowhere", "INFO"),
];

/// The fixture song: a rhyming verse (four/door — scheme `a a`) and a
/// chorus whose first line is the hook ("Keep the light on").
pub const SONG_TWO_SECTIONS: &str = "\
# Harbor Lantern

## Verse 1
The ferry lamps go out at four
the harbor locks its own back door

## Chorus
Keep the light on
the kettle keeps its slow repose
";

/// Create the seeded song project under `root` and return an open Core
/// over it: `music/songs/lantern/` with authored lyrics.
pub fn song_project(root: &Path) -> Result<Core> {
    std::fs::create_dir_all(root)?;
    let mut core = Core::new();
    core.execute(Command::CreateProject {
        root: root.to_path_buf(),
        title: "Test Songs".into(),
        kind: ProjectKind::Novel,
    })?;
    let entry = oca_music::Music::new_song(&mut core, "lantern", "Harbor Lantern")?;
    core.save_body(&entry.lyrics_rel, SONG_TWO_SECTIONS)?;
    core.refresh_index()?;
    Ok(core)
}
