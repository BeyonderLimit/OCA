//! Timeline events (`outline/timeline.md`): every event carries an in-world
//! **story time** and, optionally, the chapter it is *shown* in — its
//! **narrative position**. The two clocks stay separate so a flashback in a
//! late chapter still counts as "before" on the story clock (A11; the file
//! format is documented in `OCA-build.md` §4.6).

use anyhow::{Result, bail};

/// Which clock a "before" question is answered from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeSense {
    /// Position in the book: the chapter the event appears in.
    Narrative,
    /// In-world chronology: the event's `story_time`.
    StoryTime,
}

impl TimeSense {
    /// Frontend spelling: `narrative` | `story`.
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "narrative" => Ok(Self::Narrative),
            "story" => Ok(Self::StoryTime),
            other => bail!(
                "unknown time sense {other:?} — use 'narrative' (book order) or \
                 'story' (in-world time)"
            ),
        }
    }
}

/// One authored timeline event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub id: String,
    pub title: String,
    /// Sortable in-world time: an ISO date or a zero-padded label (compared
    /// as a string, so keep one consistent scheme per project).
    pub story_time: String,
    /// Chapter id where the event is shown; `None` = story-time-only.
    pub chapter: Option<String>,
    /// Character ids involved.
    pub characters: Vec<String>,
    pub summary: Option<String>,
}

impl Event {
    /// Parse the `events:` sequence. Entries without an id or `story_time`
    /// can't be placed on either clock and are skipped (logged).
    pub(crate) fn parse_all(value: &serde_yaml::Value) -> Vec<Event> {
        let Some(items) = value.as_sequence() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for item in items {
            match Self::parse_one(item) {
                Some(event) => out.push(event),
                None => tracing::warn!("skipping timeline event without id and story_time"),
            }
        }
        out
    }

    fn parse_one(item: &serde_yaml::Value) -> Option<Event> {
        let m = item.as_mapping()?;
        let id = m.get("id")?.as_str()?.to_string();
        let title = m
            .get("title")
            .and_then(crate::scalar)
            .unwrap_or_else(|| id.clone());
        let story_time = crate::scalar(m.get("story_time")?)?;
        let chapter = m.get("chapter").and_then(crate::scalar);
        let characters = m
            .get("characters")
            .and_then(|v| v.as_sequence())
            .map(|seq| seq.iter().filter_map(crate::scalar).collect())
            .unwrap_or_default();
        let summary = m.get("summary").and_then(crate::scalar);
        Some(Event {
            id,
            title,
            story_time,
            chapter,
            characters,
            summary,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_events_sequence() {
        let v: serde_yaml::Value = serde_yaml::from_str(
            "- id: ev_a\n  title: The letter\n  story_time: 1888-03-01\n  chapter: ch_2\n  \
             characters: [elena, marcus]\n- id: ev_b\n  story_time: 12\n",
        )
        .unwrap();
        let evs = Event::parse_all(&v);
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].title, "The letter");
        assert_eq!(evs[0].story_time, "1888-03-01");
        assert_eq!(evs[0].chapter.as_deref(), Some("ch_2"));
        assert_eq!(evs[0].characters, vec!["elena", "marcus"]);
        assert_eq!(evs[1].title, "ev_b", "title falls back to the id");
        assert_eq!(evs[1].story_time, "12", "numbers render as scalars");
        assert_eq!(evs[1].chapter, None);
    }

    #[test]
    fn entries_without_id_or_story_time_are_skipped() {
        let v: serde_yaml::Value = serde_yaml::from_str(
            "- title: no id\n  story_time: '1'\n- id: ev_no_time\n- id: ev_ok\n  story_time: '2'\n",
        )
        .unwrap();
        let evs = Event::parse_all(&v);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].id, "ev_ok");
    }

    #[test]
    fn sense_parses_from_frontend_spelling() {
        assert_eq!(TimeSense::parse("narrative").unwrap(), TimeSense::Narrative);
        assert_eq!(TimeSense::parse("Story").unwrap(), TimeSense::StoryTime);
        assert!(TimeSense::parse("wallclock").is_err());
    }
}
