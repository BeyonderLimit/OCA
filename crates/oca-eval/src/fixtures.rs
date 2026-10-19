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
