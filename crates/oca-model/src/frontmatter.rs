//! YAML frontmatter with byte-exact round-trip (ADR-0002).
//!
//! The raw block — both `---` fences included — is the source of truth. Reads
//! parse a view; writes patch targeted keys inside the inner YAML only.

use anyhow::{Result, anyhow};

use crate::yaml::LosslessYaml;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frontmatter {
    /// The full block including both `---` fences, or `None` when the document
    /// has no frontmatter.
    raw: Option<String>,
}

impl Frontmatter {
    pub fn none() -> Self {
        Self { raw: None }
    }

    /// Wrap an already-fenced block (`---\n...\n---\n`).
    pub fn from_fenced(raw: impl Into<String>) -> Self {
        Self {
            raw: Some(raw.into()),
        }
    }

    /// Build a new frontmatter block from top-level pairs (rendered via YAML,
    /// so values are quoted safely).
    pub fn create(pairs: &[(&str, &str)]) -> Result<Self> {
        let mut inner = LosslessYaml::default();
        for (k, v) in pairs {
            inner.set_str(k, v)?;
        }
        Ok(Self::from_fenced(fence(inner.raw())))
    }

    /// Full block bytes, if present.
    pub fn raw(&self) -> Option<&str> {
        self.raw.as_deref()
    }

    pub fn is_some(&self) -> bool {
        self.raw.is_some()
    }

    /// Inner YAML text between the fences.
    pub fn body(&self) -> Option<&str> {
        let raw = self.raw.as_deref()?;
        let rest = raw
            .strip_prefix("---\n")
            .or_else(|| raw.strip_prefix("---\r\n"))?;
        rest.strip_suffix("---\n")
            .or_else(|| rest.strip_suffix("---\r\n"))
            .or_else(|| rest.strip_suffix("---"))
    }

    pub fn get_str(&self, key: &str) -> Option<String> {
        LosslessYaml::new(self.body()?).get_str(key)
    }

    pub fn get_u64(&self, key: &str) -> Option<u64> {
        LosslessYaml::new(self.body()?).get_u64(key)
    }

    pub fn get_seq(&self, key: &str) -> Option<Vec<String>> {
        LosslessYaml::new(self.body()?).get_seq(key)
    }

    /// A top-level value as parsed YAML (sequences/mappings as well as
    /// scalars) — e.g. a character's `states:` entries.
    pub fn get_value(&self, key: &str) -> Option<serde_yaml::Value> {
        LosslessYaml::new(self.body()?).get_value(key)
    }

    /// Patch one top-level key with a structured (multi-line) YAML value —
    /// see [`LosslessYaml::set_block`]. Errors when the document has no
    /// frontmatter; scalars go through [`Self::set_str`].
    pub fn set_block(&mut self, key: &str, value: &str) -> Result<()> {
        let body = self
            .body()
            .ok_or_else(|| anyhow!("document has no frontmatter"))?;
        let mut yaml = LosslessYaml::new(body);
        yaml.set_block(key, value)?;
        self.raw = Some(fence(yaml.raw()));
        Ok(())
    }

    /// Patch one top-level key. Errors when the document has no frontmatter or
    /// the value is multi-line (see `LosslessYaml::set_str`).
    pub fn set_str(&mut self, key: &str, value: &str) -> Result<()> {
        let body = self
            .body()
            .ok_or_else(|| anyhow!("document has no frontmatter"))?;
        let mut yaml = LosslessYaml::new(body);
        yaml.set_str(key, value)?;
        self.raw = Some(fence(yaml.raw()));
        Ok(())
    }
}

/// Re-fence inner YAML, preserving the document's line-ending style.
fn fence(inner: &str) -> String {
    let crlf = inner.contains("\r\n");
    let eol = if crlf { "\r\n" } else { "\n" };
    let normalized = inner.replace("\r\n", "\n");
    let body = normalized.strip_suffix('\n').unwrap_or(&normalized);
    let body = if crlf {
        body.replace('\n', "\r\n")
    } else {
        body.to_string()
    };
    format!("---{eol}{body}{eol}---{eol}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_read() {
        let fm = Frontmatter::create(&[("id", "ch_1"), ("title", "A: B")]).unwrap();
        assert!(fm.is_some());
        assert_eq!(fm.get_str("id").as_deref(), Some("ch_1"));
        assert_eq!(fm.get_str("title").as_deref(), Some("A: B"));
        assert!(fm.raw().unwrap().starts_with("---\n"));
        assert!(fm.raw().unwrap().ends_with("---\n"));
    }

    #[test]
    fn set_preserves_surrounding_bytes() {
        let mut fm = Frontmatter::from_fenced(
            "---\n# why this exists\nid: ch_1\ntitle: Old\nmystery:\n  key: 1\n---\n",
        );
        fm.set_str("title", "New").unwrap();
        let out = fm.raw().unwrap();
        assert_eq!(
            out,
            "---\n# why this exists\nid: ch_1\ntitle: New\nmystery:\n  key: 1\n---\n"
        );
    }

    #[test]
    fn no_frontmatter_reads_none_and_set_fails() {
        let fm = Frontmatter::none();
        assert!(fm.raw().is_none());
        assert!(fm.get_str("id").is_none());
        let mut fm = fm;
        assert!(fm.set_str("id", "x").is_err());
    }

    #[test]
    fn set_block_writes_structured_values_losslessly() {
        let mut fm = Frontmatter::from_fenced("---\n# note\nid: char_1\nroles: [x]\n---\n");
        fm.set_block("states", "- at: ch_1\n  location: village")
            .unwrap();
        assert_eq!(
            fm.raw().unwrap(),
            "---\n# note\nid: char_1\nroles: [x]\nstates:\n  - at: ch_1\n    location: village\n---\n"
        );
        let states = fm.get_value("states").unwrap();
        assert_eq!(
            states.as_sequence().unwrap()[0]
                .get("location")
                .unwrap()
                .as_str(),
            Some("village")
        );
    }

    #[test]
    fn body_strips_fences() {
        let fm = Frontmatter::from_fenced("---\na: 1\n---\n");
        assert_eq!(fm.body(), Some("a: 1\n"));
    }
}
