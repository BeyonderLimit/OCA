//! Albums and `artist.yaml` (v2 amendments; album *analysis* is Phases 10
//! and 14 — this is the storage model only). Formats:
//!
//! ```text
//! music/artist.yaml          free-form identity (§5.2 categories)
//! music/albums/<slug>/album.yaml
//! ```
//!
//! `album.yaml`:
//!
//! ```yaml
//! id: alb_the_long_night
//! title: The Long Night
//! concept: A night-long vigil.
//! tracks: [song_ashes, song_blackwood]
//! ```

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

/// `music/artist.yaml` — lenient on purpose: the doc lists categories, not
/// a fixed schema, so unknown keys pass through and only the common fields
/// are typed.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Artist {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub influences: Vec<String>,
    #[serde(default)]
    pub vocal_style: Option<String>,
    #[serde(default)]
    pub themes: Vec<String>,
    #[serde(default)]
    pub instrumentation: Vec<String>,
    #[serde(default)]
    pub tempos: Option<String>,
    #[serde(default)]
    pub production: Option<String>,
    #[serde(default)]
    pub forbidden_cliches: Vec<String>,
    #[serde(default)]
    pub goals: Option<String>,
}

impl Artist {
    pub fn parse(raw: &str) -> Result<Self> {
        serde_yaml::from_str(raw).context("artist.yaml is not valid YAML")
    }

    /// Load `music/artist.yaml` if present.
    pub fn load(project_root: &Path) -> Result<Option<Self>> {
        let path = project_root.join("music").join("artist.yaml");
        if !path.exists() {
            return Ok(None);
        }
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        Self::parse(&raw)
            .map(Some)
            .with_context(|| format!("parse {}", path.display()))
    }
}

/// One `music/albums/<slug>/album.yaml`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Album {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub concept: Option<String>,
    #[serde(default)]
    pub tracks: Vec<String>,
}

impl Album {
    pub fn parse(raw: &str) -> Result<Self> {
        serde_yaml::from_str(raw).context("album.yaml is not valid YAML")
    }

    /// Every album under `music/albums/`, in directory order.
    pub fn load_all(project_root: &Path) -> Result<Vec<(String, Self)>> {
        let dir = project_root.join("music").join("albums");
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Ok(out);
        };
        for entry in entries.flatten() {
            let path = entry.path().join("album.yaml");
            if !path.exists() {
                continue;
            }
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            let album = Self::parse(&raw).with_context(|| format!("parse {}", path.display()))?;
            out.push((entry.file_name().to_string_lossy().to_string(), album));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn artist_parses_common_fields() {
        let raw = "\
name: The Blackwood Sessions
genres: [alternative, cinematic]
influences: [Nick Cave, Portishead]
vocal_style: close-mic, breathy
themes: [loss, rebirth]
instrumentation: [piano, upright bass, strings]
tempos: slow-to-mid
production: analog warmth, tape saturation
forbidden_cliches: [heart of gold, on the road again]
goals: one cohesive album, not singles
unknown_future_key: kept out of the struct
";
        let a = Artist::parse(raw).unwrap();
        assert_eq!(a.name.as_deref(), Some("The Blackwood Sessions"));
        assert_eq!(a.genres.len(), 2);
        assert_eq!(a.forbidden_cliches.len(), 2);
    }

    #[test]
    fn album_parses_tracks() {
        let raw = "\
id: alb_the_long_night
title: The Long Night
concept: A night-long vigil.
tracks: [song_ashes, song_blackwood]
";
        let a = Album::parse(raw).unwrap();
        assert_eq!(a.id.as_deref(), Some("alb_the_long_night"));
        assert_eq!(a.tracks, vec!["song_ashes", "song_blackwood"]);
    }

    #[test]
    fn load_all_reads_the_tree_and_ignores_missing_dir() {
        let tmp = TempDir::new().unwrap();
        assert!(Album::load_all(tmp.path()).unwrap().is_empty());

        let dir = tmp.path().join("music/albums/the-long-night");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("album.yaml"),
            "id: alb_1\ntitle: One\ntracks: []\n",
        )
        .unwrap();
        let albums = Album::load_all(tmp.path()).unwrap();
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].0, "the-long-night");
    }

    #[test]
    fn artist_absent_is_ok() {
        let tmp = TempDir::new().unwrap();
        assert!(Artist::load(tmp.path()).unwrap().is_none());
    }
}
