//! Character entity files (`characters/*.md`): identity, aliases,
//! declarative `states:`, and relationships (A11; the roadmap's single
//! per-chapter `state:` was superseded by the `states:` list).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use oca_model::Document;

use crate::scalar;
use crate::state::StateEntry;

/// One `characters/*.md` entity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Character {
    pub rel: PathBuf,
    pub id: String,
    /// Display name: `name:` → `title:` → filename stem.
    pub name: String,
    pub title: String,
    pub role: Option<String>,
    /// Extra names the text may use for them (mention detection, A11).
    pub aliases: Vec<String>,
    /// Declarative state in file order (carry-forward; see `state.rs`).
    pub states: Vec<StateEntry>,
    pub relationships: Vec<Relationship>,
}

/// One `relationships:` edge: `target: { type: distrust }` (a bare string
/// value is accepted too).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relationship {
    pub target: String,
    pub kind: String,
}

impl Character {
    /// Parse an entity file — tolerant by design: a character is usable from
    /// its id alone, and missing pieces read as empty rather than refusing
    /// to load a project someone hand-edited.
    pub fn parse(rel: &Path, doc: &Document) -> Self {
        let fm = &doc.frontmatter;
        let title = fm
            .get_str("title")
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| {
                rel.file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default()
            });
        let name = fm
            .get_str("name")
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| title.clone());
        Self {
            rel: rel.to_path_buf(),
            id: fm.get_str("id").unwrap_or_default(),
            name,
            role: fm.get_str("role").filter(|r| !r.is_empty()),
            title,
            aliases: fm.get_seq("aliases").unwrap_or_default(),
            states: parse_states(fm.get_value("states"), rel),
            relationships: parse_relationships(fm.get_value("relationships")),
        }
    }
}

fn parse_states(value: Option<serde_yaml::Value>, rel: &Path) -> Vec<StateEntry> {
    let Some(serde_yaml::Value::Sequence(items)) = value else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items {
        let parsed = item.as_mapping().and_then(|map| {
            let at = map.get("at").and_then(|v| v.as_str())?.to_string();
            let fields = map
                .iter()
                .filter(|(k, _)| k.as_str() != Some("at"))
                .filter_map(|(k, v)| Some((k.as_str()?.to_string(), scalar(v)?)))
                .collect::<BTreeMap<String, String>>();
            Some(StateEntry { at, fields })
        });
        match parsed {
            Some(entry) => out.push(entry),
            None => tracing::warn!(
                rel = %rel.display(),
                "skipping states entry without an 'at' chapter id"
            ),
        }
    }
    out
}

fn parse_relationships(value: Option<serde_yaml::Value>) -> Vec<Relationship> {
    let Some(serde_yaml::Value::Mapping(map)) = value else {
        return Vec::new();
    };
    map.iter()
        .filter_map(|(k, v)| {
            let target = k.as_str()?.to_string();
            let kind = match v {
                serde_yaml::Value::Mapping(inner) => inner.get("type").and_then(scalar)?,
                other => scalar(other)?,
            };
            Some(Relationship { target, kind })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use oca_model::Frontmatter;

    fn doc_with(fenced: &str) -> Document {
        Document {
            raw: format!("{fenced}body\n"),
            frontmatter: Frontmatter::from_fenced(fenced),
            blocks: Vec::new(),
            links: Vec::new(),
            body_start_line: 0,
            body_start: fenced.len(),
        }
    }

    #[test]
    fn parses_full_schema() {
        let doc = doc_with(
            "---\nid: elena\nname: Elena\nrole: protagonist\naliases: [Ellie, \"the girl\"]\n\
             relationships:\n  marcus:\n    type: distrust\nstates:\n  - at: ch_1\n    \
             location: village\n---\n",
        );
        let c = Character::parse(Path::new("characters/elena.md"), &doc);
        assert_eq!(c.id, "elena");
        assert_eq!(c.name, "Elena");
        assert_eq!(c.role.as_deref(), Some("protagonist"));
        assert_eq!(c.aliases, vec!["Ellie", "the girl"]);
        assert_eq!(c.states.len(), 1);
        assert_eq!(c.states[0].at, "ch_1");
        assert_eq!(c.states[0].fields["location"], "village");
        assert_eq!(
            c.relationships,
            vec![Relationship {
                target: "marcus".into(),
                kind: "distrust".into(),
            }]
        );
    }

    #[test]
    fn name_falls_back_to_title_then_stem() {
        let titled = doc_with("---\nid: x\ntitle: Marcus\n---\n");
        let c = Character::parse(Path::new("characters/marcus.md"), &titled);
        assert_eq!(c.name, "Marcus");

        let bare = doc_with("---\nid: x\n---\n");
        let c = Character::parse(Path::new("characters/gill.md"), &bare);
        assert_eq!(c.name, "gill", "filename stem is the last resort");
    }

    #[test]
    fn bare_string_relationship_and_skipped_bad_state_entry() {
        let doc = doc_with(
            "---\nid: m\nrelationships:\n  elena: distrust\nstates:\n  - location: lost\n  - \
             at: ch_2\n    location: harbor\n---\n",
        );
        let c = Character::parse(Path::new("characters/marcus.md"), &doc);
        assert_eq!(c.relationships[0].kind, "distrust");
        assert_eq!(
            c.states,
            vec![StateEntry {
                at: "ch_2".into(),
                fields: BTreeMap::from([("location".into(), "harbor".into())]),
            }],
            "an entry without 'at' is skipped, the rest still load"
        );
    }
}
