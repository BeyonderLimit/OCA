//! `song.yaml` — song metadata (§4.2). Files are canonical (rule 2): this
//! is a read view; nothing in Phase 3 rewrites `song.yaml` except the
//! initial skeleton from `oca song new`. The optional `plan:` is drift
//! reference only — `lyrics.md` headings are authoritative (A11/D4).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct SongLinks {
    #[serde(default)]
    pub chapters: Vec<String>,
    #[serde(default)]
    pub characters: Vec<String>,
}

/// Parsed `song.yaml`. Unknown keys are ignored (forward-compatible);
/// missing keys default so a hand-minimal file still loads.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct SongYaml {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub genre: Vec<String>,
    #[serde(default)]
    pub mood: Vec<String>,
    #[serde(default)]
    pub tempo: Option<i64>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub time_signature: Option<String>,
    #[serde(default)]
    pub themes: Vec<String>,
    /// Optional structure plan; `lyrics.md` wins on drift.
    #[serde(default)]
    pub plan: Option<Vec<String>>,
    #[serde(default)]
    pub links: Option<SongLinks>,
}

impl SongYaml {
    pub fn parse(raw: &str) -> Result<Self> {
        serde_yaml::from_str(raw).context("song.yaml is not valid YAML")
    }

    /// The language the lyric tools should use: the song override, else
    /// the caller-provided project default.
    pub fn effective_language(&self, project_language: &str) -> String {
        match &self.language {
            Some(l) if !l.is_empty() => l.clone(),
            _ => project_language.to_string(),
        }
    }
}

/// One discovered song: its directory plus both files' relative paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SongEntry {
    /// Project-relative directory, e.g. `music/songs/ashes`.
    pub dir_rel: PathBuf,
    /// `song.yaml` path, relative to the project.
    pub yaml_rel: PathBuf,
    /// `lyrics.md` path, relative to the project.
    pub lyrics_rel: PathBuf,
}

impl SongEntry {
    /// Directory slug: `music/songs/ashes` → `ashes`.
    pub fn slug(&self) -> &str {
        self.dir_rel
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
    }
}

/// Skeleton for a new song (`oca song new`): minimal, valid, no plan.
pub fn skeleton_yaml(id: &str, title: &str, language: &str) -> String {
    format!(
        "type: song\n\
         id: {id}\n\
         language: {language}\n\
         title: {title}\n\
         artist: \"\"\n\
         status: draft\n\
         genre: []\n\
         mood: []\n\
         tempo: 120\n\
         key: C\n\
         time_signature: 4/4\n\
         themes: []\n"
    )
}

/// Skeleton `lyrics.md`: title heading plus the two sections every song
/// starts from.
pub fn skeleton_lyrics(title: &str) -> String {
    format!("# {title}\n\n## Verse 1\n\n## Chorus\n\n")
}

/// Read `song.yaml` next to a song directory.
pub fn load_yaml(project_root: &Path, entry: &SongEntry) -> Result<SongYaml> {
    let path = project_root.join(&entry.yaml_rel);
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    SongYaml::parse(&raw).with_context(|| format!("parse {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_shape() {
        let raw = "\
type: song
id: song_ashes
language: en
title: Ashes
artist: \"\"
status: draft
genre: [alternative, cinematic]
mood: [melancholic, defiant]
tempo: 92
key: D minor
time_signature: 4/4
themes: [loss, rebirth, revenge]
plan: [intro, verse, pre_chorus, chorus, verse, chorus, bridge, final_chorus, outro]
links:
  chapters: [ch_012, ch_018]
  characters: [elena]
";
        let s = SongYaml::parse(raw).unwrap();
        assert_eq!(s.id.as_deref(), Some("song_ashes"));
        assert_eq!(s.tempo, Some(92));
        assert_eq!(s.key.as_deref(), Some("D minor"));
        assert_eq!(s.plan.as_ref().unwrap().len(), 9);
        assert_eq!(s.links.unwrap().characters, vec!["elena"]);
    }

    #[test]
    fn minimal_file_loads_with_defaults() {
        let s = SongYaml::parse("id: song_x\ntitle: X\n").unwrap();
        assert!(s.plan.is_none());
        assert!(s.genre.is_empty());
        assert_eq!(s.effective_language("en"), "en");
    }

    #[test]
    fn song_language_overrides_project() {
        let s = SongYaml::parse("id: s\nlanguage: fr\n").unwrap();
        assert_eq!(s.effective_language("en"), "fr");
    }

    #[test]
    fn skeletons_are_valid() {
        let y = SongYaml::parse(&skeleton_yaml("song_ashes", "Ashes", "en")).unwrap();
        assert_eq!(y.id.as_deref(), Some("song_ashes"));
        assert_eq!(y.title.as_deref(), Some("Ashes"));
        assert!(y.plan.is_none());
        let l = crate::lyrics::Lyrics::parse(&skeleton_lyrics("Ashes")).unwrap();
        assert_eq!(l.title.as_deref(), Some("Ashes"));
        assert_eq!(l.sections.len(), 2);
    }
}
