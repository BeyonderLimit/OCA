//! Project metadata (`project.yaml`) and document-kind taxonomy.

use anyhow::Result;

use crate::frontmatter::Frontmatter;
use crate::id::{Id, new_id};
use crate::yaml::LosslessYaml;

/// Highest `format_version` this build can write. Newer projects open
/// read-only (A1 / guardrail: never clobber a future layout).
pub const SUPPORTED_FORMAT_VERSION: u64 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectKind {
    Novel,
    ShortStory,
}

impl ProjectKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProjectKind::Novel => "novel",
            ProjectKind::ShortStory => "short-story",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "novel" => Ok(ProjectKind::Novel),
            "short-story" => Ok(ProjectKind::ShortStory),
            other => anyhow::bail!("unknown project kind: {other}"),
        }
    }
}

/// A creatable document type: where it lives and which id prefix it uses
/// (A2: ids, never paths).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentKind {
    Chapter,
    Character,
    Location,
    Organization,
    Item,
    Faction,
    PlotThread,
    Song,
    Album,
    Audio,
    Note,
}

impl DocumentKind {
    /// Project-relative directory, no trailing slash.
    pub fn dir(&self) -> &'static str {
        match self {
            DocumentKind::Chapter => "manuscript",
            DocumentKind::Character => "characters",
            DocumentKind::Location
            | DocumentKind::Organization
            | DocumentKind::Item
            | DocumentKind::Faction => "world",
            DocumentKind::PlotThread => "outline",
            DocumentKind::Song | DocumentKind::Album | DocumentKind::Audio => "music",
            DocumentKind::Note => "notes",
        }
    }

    /// Id prefix (A2 `*_id` naming; links look like `[[char_01J...]]`).
    pub fn prefix(&self) -> &'static str {
        match self {
            DocumentKind::Chapter => "ch",
            DocumentKind::Character => "char",
            DocumentKind::Location => "loc",
            DocumentKind::Organization => "org",
            DocumentKind::Item => "item",
            DocumentKind::Faction => "fac",
            DocumentKind::PlotThread => "plot",
            DocumentKind::Song => "song",
            DocumentKind::Album => "alb",
            DocumentKind::Audio => "aud",
            DocumentKind::Note => "note",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            DocumentKind::Chapter => "Chapter",
            DocumentKind::Character => "Character",
            DocumentKind::Location => "Location",
            DocumentKind::Organization => "Organization",
            DocumentKind::Item => "Item",
            DocumentKind::Faction => "Faction",
            DocumentKind::PlotThread => "Plot Thread",
            DocumentKind::Song => "Song",
            DocumentKind::Album => "Album",
            DocumentKind::Audio => "Audio",
            DocumentKind::Note => "Note",
        }
    }
}

/// `project.yaml`: lossless raw + accessor view (A2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMeta {
    yaml: LosslessYaml,
}

impl ProjectMeta {
    /// A freshly generated metadata file for `title`.
    pub fn new_template(title: &str, kind: ProjectKind) -> Result<Self> {
        let mut yaml = LosslessYaml::default();
        yaml.set_str("project_id", new_id("prj").as_str())?;
        yaml.set_str("title", title)?;
        yaml.set_str("kind", kind.as_str())?;
        yaml.set_str("language", "en")?;
        yaml.set_str("format_version", &SUPPORTED_FORMAT_VERSION.to_string())?;
        Ok(Self { yaml })
    }

    pub fn parse(raw: &str) -> Result<Self> {
        let yaml = LosslessYaml::new(raw);
        if yaml.parse().is_none() {
            anyhow::bail!("project.yaml is not valid YAML mapping");
        }
        Ok(Self { yaml })
    }

    pub fn raw(&self) -> &str {
        self.yaml.raw()
    }

    pub fn project_id(&self) -> Id {
        Id::from(
            self.yaml
                .get_str("project_id")
                .unwrap_or_else(|| "prj_UNKNOWN".to_string()),
        )
    }

    /// Absent version reads as 0 (pre-versioned adopt/import projects) —
    /// writable, and `adopt` will stamp it up.
    pub fn format_version(&self) -> u64 {
        self.yaml.get_u64("format_version").unwrap_or(0)
    }

    /// Newer-than-supported projects open read-only (guardrail: migrations
    /// must never run against a file this build doesn't understand).
    pub fn is_writable_format(&self) -> bool {
        self.format_version() <= SUPPORTED_FORMAT_VERSION
    }

    pub fn title(&self) -> String {
        self.yaml.get_str("title").unwrap_or_default()
    }

    pub fn kind(&self) -> ProjectKind {
        self.yaml
            .get_str("kind")
            .and_then(|k| ProjectKind::parse(&k).ok())
            .unwrap_or(ProjectKind::Novel)
    }

    pub fn language(&self) -> String {
        self.yaml.get_str("language").unwrap_or_else(|| "en".into())
    }

    /// Frontmatter template for a new document of `kind`.
    pub fn document_frontmatter(&self, kind: DocumentKind, title: &str) -> Result<Frontmatter> {
        Frontmatter::create(&[
            ("id", new_id(kind.prefix()).as_str()),
            ("title", title),
            ("project_id", self.project_id().as_str()),
            ("created", ""),
            ("tags", "[]"),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_round_trips_losslessly() {
        let meta = ProjectMeta::new_template("My Novel", ProjectKind::Novel).unwrap();
        let reparsed = ProjectMeta::parse(meta.raw()).unwrap();
        assert_eq!(reparsed, meta);
        assert!(reparsed.project_id().is_valid());
        assert_eq!(reparsed.format_version(), SUPPORTED_FORMAT_VERSION);
        assert!(reparsed.is_writable_format());
        assert_eq!(reparsed.kind(), ProjectKind::Novel);
        assert_eq!(reparsed.title(), "My Novel");
    }

    #[test]
    fn newer_format_version_is_not_writable() {
        let raw = "project_id: prj_x\ntitle: Future\nformat_version: 99\n";
        let meta = ProjectMeta::parse(raw).unwrap();
        assert!(!meta.is_writable_format());
        // raw preserved byte-for-byte by the read path
        assert_eq!(meta.raw(), raw);
    }

    #[test]
    fn doc_kind_directories_and_prefixes() {
        assert_eq!(DocumentKind::Chapter.dir(), "manuscript");
        assert_eq!(DocumentKind::Location.dir(), "world");
        assert_eq!(DocumentKind::Song.dir(), "music");
        assert_eq!(DocumentKind::Character.prefix(), "char");
        assert!(DocumentKind::Chapter.prefix().len() < 6);
    }

    #[test]
    fn document_frontmatter_carries_ids() {
        let meta = ProjectMeta::new_template("T", ProjectKind::Novel).unwrap();
        let fm = meta
            .document_frontmatter(DocumentKind::Chapter, "One")
            .unwrap();
        assert!(fm.get_str("id").unwrap().starts_with("ch_"));
        assert_eq!(fm.get_str("title").as_deref(), Some("One"));
        assert_eq!(
            fm.get_str("project_id").as_deref(),
            Some(meta.project_id().as_str())
        );
    }
}
