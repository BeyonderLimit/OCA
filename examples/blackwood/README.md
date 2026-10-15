# The Blackwood Letter — OCA sample novel

A ten-chapter fixture novel the story-engine integration tests run against
(`crates/oca-story/tests/queries.rs`). It is small but deliberately tricky:

- **Characters:** `characters/elena.md` (aliases `Ellie` and `the girl`,
  `states:` keyed by chapter, a YAML comment + an unknown key
  `favorite_color:` that must survive lossless edits) and
  `characters/marcus.md`.
- **Mention traps:** chapter 5 mentions Elena *only* as "Ellie"; chapter 9
  only as "the girl"; chapter 7 contains the shrine saint "Elenara", which is
  *not* a mention (word boundaries).
- **Two clocks:** `outline/timeline.md` events carry a `story_time` (in-world
  chronology) and the chapter they are shown in. Chapter 6 shows a
  flash-forward (`ev_moot`, story time after the trial) and chapter 10 shows a
  flashback (`ev_flashback`, story time before everything) — so "what happened
  before chapter 8?" answers differently in each time sense. Chapter 8 carries
  the `story_time:` anchor the story clock compares against.

The `.oca/` tree here is an empty skeleton (`.gitkeep` only) so the project
opens directly; the cache is rebuildable, and nothing in `.oca/` matters to
the tests, which work on a copy in a temp directory.
