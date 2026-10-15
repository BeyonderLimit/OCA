---
# Story time is in-world chronology (sortable strings); `chapter` is the
# narrative position — the chapter the event is shown in. Flashbacks and
# flash-forwards make the two clocks disagree on purpose (A11).
events:
  - id: ev_arrival
    title: Elena arrives in the valley
    story_time: '1888-02-28'
    chapter: ch_001
    characters: [elena, marcus]
  - id: ev_letter
    title: The letter is found
    story_time: '1888-03-01'
    chapter: ch_002
    characters: [elena]
  - id: ev_forest
    title: Elena enters Blackwood
    story_time: '1888-03-03'
    chapter: ch_004
    characters: [elena]
  - id: ev_market
    title: Marcus canvasses the market
    story_time: '1888-03-04'
    chapter: ch_005
    characters: [marcus]
  - id: ev_elder
    title: The elder tells what she saw
    story_time: '1888-03-08'
    chapter: ch_007
    characters: [marcus]
  - id: ev_trial
    title: The trial begins
    story_time: '1888-03-10'
    chapter: ch_008
    characters: [elena, marcus]
  - id: ev_departure
    title: Elena leaves for the coast
    story_time: '1888-03-12'
    chapter: ch_009
    characters: [elena]
  - id: ev_moot
    title: The factions hold a moot
    story_time: '1888-03-20'
    chapter: ch_006
    characters: [marcus]
    summary: A flash-forward — shown in chapter 6, it happens after the trial.
  - id: ev_flashback
    title: Elena's mother burns the first letter
    story_time: '1887-11-02'
    chapter: ch_010
    characters: [elena]
    summary: A flashback — shown in chapter 10, it happened before everything.
---

# Timeline

Events are stored in authored order; queries sort by the story clock or by
narrative position, whichever sense the question asks for.
