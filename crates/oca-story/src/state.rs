//! Character state (A11): entries live in the entity file, keyed by chapter
//! id, with carry-forward semantics — the state at chapter N is every entry
//! at or before N applied in order, later fields overriding earlier ones.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Result, bail};
use oca_core::{Command, Core};

use crate::{Chapter, Character, Story};

/// One authored `states:` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateEntry {
    /// Chapter id the entry is keyed at.
    pub at: String,
    /// Flat fields (`location`, `emotional`, `goal`, …); `at` excluded.
    pub fields: BTreeMap<String, String>,
}

/// Resolved state at one chapter — the answer to "Where is Elena?".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateView {
    pub character: Character,
    pub chapter: Chapter,
    pub fields: BTreeMap<String, String>,
}

impl StateView {
    /// One field of the resolved state (`Some("blackwood")`).
    pub fn get(&self, field: &str) -> Option<&str> {
        self.fields.get(field).map(String::as_str)
    }
}

/// Carry-forward resolution: apply every entry whose `at` chapter sits at or
/// before `target` in narrative order; later fields override earlier ones.
/// Entries whose `at` id doesn't resolve to a chapter are ignored.
pub(crate) fn resolve(
    states: &[StateEntry],
    chapters: &[Chapter],
    target: &Chapter,
) -> BTreeMap<String, String> {
    let position_of = |key: &str| crate::position_of(chapters, key);
    let mut out = BTreeMap::new();
    let Some(target_pos) = position_of(&target.id) else {
        return out;
    };
    for entry in states {
        if let Some(pos) = position_of(&entry.at)
            && pos <= target_pos
        {
            out.extend(entry.fields.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
    }
    out
}

/// The "set state at this chapter" authoring aid (A11): upsert a `states:`
/// entry for `character` keyed by the target chapter's frontmatter id,
/// patching the entity file losslessly through the Core (unknown keys and
/// comments survive; only the `states:` block is rewritten). Fields already
/// present at that chapter are kept — the new values win. Returns the
/// patched file's project-relative path.
pub fn set_state(
    core: &mut Core,
    character: &str,
    chapter: &str,
    fields: &[(&str, &str)],
) -> Result<PathBuf> {
    if fields.is_empty() {
        bail!("nothing to set — pass at least one field (e.g. location: blackwood)");
    }
    if fields.iter().any(|(k, _)| k.trim().is_empty()) {
        bail!("state field names must not be empty");
    }
    let (char_rel, chapter_id) = {
        // Read side first (immutable borrow), then mutate through Core.
        let story = Story::new(core);
        let c = story.character(character)?;
        let ch = story.chapter(chapter)?;
        (c.rel, ch.id)
    };

    let doc = core.read_document(&char_rel)?;
    let mut entries: Vec<serde_yaml::Value> = match doc.frontmatter.get_value("states") {
        Some(serde_yaml::Value::Sequence(s)) => s,
        Some(_) => bail!(
            "{}: 'states:' must be a list of entries",
            char_rel.display()
        ),
        None => Vec::new(),
    };

    let overrides: Vec<(serde_yaml::Value, serde_yaml::Value)> = fields
        .iter()
        .map(|(k, v)| {
            (
                serde_yaml::Value::String((*k).to_string()),
                serde_yaml::Value::String((*v).to_string()),
            )
        })
        .collect();

    let mut merged = false;
    for entry in entries.iter_mut() {
        if entry.get("at").and_then(|v| v.as_str()) == Some(chapter_id.as_str())
            && let Some(map) = entry.as_mapping_mut()
        {
            for (k, v) in overrides.iter().cloned() {
                map.insert(k, v);
            }
            merged = true;
            break;
        }
    }
    if !merged {
        let mut map = serde_yaml::Mapping::new();
        map.insert(
            serde_yaml::Value::String("at".to_string()),
            serde_yaml::Value::String(chapter_id.clone()),
        );
        for (k, v) in overrides {
            map.insert(k, v);
        }
        entries.push(serde_yaml::Value::Mapping(map));
    }

    let block = serde_yaml::to_string(&serde_yaml::Value::Sequence(entries))
        .map_err(|e| anyhow::anyhow!("cannot render states: {e}"))?;
    core.execute(Command::SetFrontmatter {
        doc: char_rel.clone(),
        key: "states".to_string(),
        value: block,
    })?;
    Ok(char_rel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ch(id: &str, seq: u32) -> Chapter {
        Chapter {
            rel: PathBuf::from(format!("manuscript/{seq:03}.md")),
            id: id.into(),
            title: format!("Chapter {seq}"),
            seq,
        }
    }

    fn entry(at: &str, fields: &[(&str, &str)]) -> StateEntry {
        StateEntry {
            at: at.into(),
            fields: fields
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn carry_forward_merges_in_order_later_fields_win() {
        let chs = [ch("ch_1", 1), ch("ch_2", 2), ch("ch_3", 3)];
        let states = [
            entry(
                "ch_1",
                &[("location", "village"), ("emotional", "isolated")],
            ),
            entry("ch_3", &[("location", "blackwood")]),
        ];
        let at_2 = resolve(&states, &chs, &chs[1]);
        assert_eq!(at_2["location"], "village", "only ch_1 applies yet");
        let at_3 = resolve(&states, &chs, &chs[2]);
        assert_eq!(at_3["location"], "blackwood");
        assert_eq!(
            at_3["emotional"], "isolated",
            "unmentioned fields carry forward"
        );
    }

    #[test]
    fn unknown_at_ids_are_ignored() {
        let chs = [ch("ch_1", 1)];
        let states = [entry("ch_deleted", &[("location", "nowhere")])];
        assert!(resolve(&states, &chs, &chs[0]).is_empty());
    }

    #[test]
    fn at_keys_accept_stems_and_titles_not_just_ids() {
        let chs = [ch("ch_1", 1), ch("ch_2", 2)];
        // Hand-authored keys in the style people actually write: a
        // filename stem and a title — neither is the frontmatter id.
        let states = [
            entry("001", &[("location", "harbor-house")]),
            entry("Chapter 2", &[("emotional", "resolved")]),
        ];
        let at_2 = resolve(&states, &chs, &chs[1]);
        assert_eq!(at_2["location"], "harbor-house", "stem key resolves");
        assert_eq!(at_2["emotional"], "resolved", "title key resolves");
    }
}
