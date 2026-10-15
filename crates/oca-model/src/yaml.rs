//! Lossless YAML: raw text is the source of truth; reads go through a parsed
//! view; writes are targeted line patches that leave everything they don't
//! touch byte-identical (unknown keys, comments, ordering, indentation).
//!
//! Scope note: patches handle top-level scalars (strings, numbers, booleans).
//! Values containing newlines are rejected — Phase 1 never needs them, and a
//! wrong block-scalar rewrite would be lossier than an error.

use anyhow::{Result, bail};

/// A YAML document (frontmatter inner text or a whole file like `project.yaml`)
/// whose bytes are preserved verbatim unless explicitly patched.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LosslessYaml {
    raw: String,
    crlf: bool,
}

impl LosslessYaml {
    pub fn new(raw: impl Into<String>) -> Self {
        let raw = raw.into();
        let crlf = raw.contains("\r\n");
        Self { raw, crlf }
    }

    pub fn raw(&self) -> &str {
        &self.raw
    }

    pub fn into_raw(self) -> String {
        self.raw
    }

    /// Parsed view of the document. `None` when the text is not a YAML mapping
    /// (invalid YAML never invalidates the raw bytes).
    pub fn parse(&self) -> Option<serde_yaml::Mapping> {
        serde_yaml::from_str::<serde_yaml::Mapping>(&self.raw).ok()
    }

    /// A top-level scalar as a string (numbers/bools stringified).
    pub fn get_str(&self, key: &str) -> Option<String> {
        let map = self.parse()?;
        match map.get(serde_yaml::Value::String(key.to_string()))? {
            serde_yaml::Value::String(s) => Some(s.clone()),
            serde_yaml::Value::Number(n) => Some(n.to_string()),
            serde_yaml::Value::Bool(b) => Some(b.to_string()),
            serde_yaml::Value::Null => Some(String::new()),
            other => serde_yaml::to_string(other)
                .ok()
                .map(|s| s.trim_end().to_string()),
        }
    }

    pub fn get_u64(&self, key: &str) -> Option<u64> {
        self.get_str(key)?.parse().ok()
    }

    /// A top-level sequence of strings (block or inline style).
    pub fn get_seq(&self, key: &str) -> Option<Vec<String>> {
        let map = self.parse()?;
        match map.get(serde_yaml::Value::String(key.to_string()))? {
            serde_yaml::Value::Sequence(items) => Some(
                items
                    .iter()
                    .map(|v| match v {
                        serde_yaml::Value::String(s) => s.clone(),
                        other => other.as_str().unwrap_or_default().to_string(),
                    })
                    .collect(),
            ),
            _ => None,
        }
    }

    /// A top-level value as parsed YAML: sequences and mappings alike (e.g.
    /// `states:` entries or timeline `events:`), scalars in parsed form.
    pub fn get_value(&self, key: &str) -> Option<serde_yaml::Value> {
        let map = self.parse()?;
        map.get(serde_yaml::Value::String(key.to_string())).cloned()
    }

    /// Set a top-level key to a *structured* (multi-line) YAML value: a
    /// sequence or mapping fragment given without the key line, e.g.
    /// `- at: ch_1\n  location: x` becomes `key:\n  - at: ch_1\n    location: x`.
    /// The fragment is validated before any byte changes — a malformed value
    /// is rejected and the document stays untouched — and everything outside
    /// the replaced block keeps the same byte-identity guarantee as
    /// [`Self::set_str`] (unknown keys, comments, ordering survive). Scalars
    /// go through `set_str`; on write, the replaced block's *formatting* is
    /// normalized to standard YAML (the values are what's preserved).
    pub fn set_block(&mut self, key: &str, value: &str) -> Result<()> {
        if key.is_empty() || key.contains(':') || key.contains('\n') {
            bail!("invalid YAML key: {key:?}");
        }
        let value = value.replace("\r\n", "\n");
        let value = value.trim_matches('\n');
        if value.is_empty() {
            bail!("empty YAML value for {key} (scalars go through set_str)");
        }
        let block: Vec<String> = std::iter::once(format!("{key}:"))
            .chain(value.split('\n').map(|l| {
                if l.trim().is_empty() {
                    String::new()
                } else {
                    format!("  {l}")
                }
            }))
            .collect();
        // Validate the fragment in isolation first: the block must parse, and
        // only structured values belong here — `key:\n  plain scalar` parses,
        // so a scalar fragment is rejected explicitly (use `set_str`).
        let isolated = block.join("\n");
        let parsed: serde_yaml::Mapping = serde_yaml::from_str(&isolated)
            .map_err(|e| anyhow::anyhow!("invalid YAML value for {key}: {e}"))?;
        match parsed.get(serde_yaml::Value::String(key.to_string())) {
            Some(serde_yaml::Value::Sequence(_) | serde_yaml::Value::Mapping(_)) => {}
            _ => bail!("{key} needs a sequence or mapping value (scalars go through set_str)"),
        }

        let eol_cr = if self.crlf { "\r" } else { "" };
        // Each replacement line carries the document's line ending, matching
        // `set_str`'s splice: existing lines already embed their own `\r`.
        let repl: String = block
            .iter()
            .map(|l| format!("{l}{eol_cr}"))
            .collect::<Vec<_>>()
            .join("\n");

        let lines: Vec<&str> = self.raw.split('\n').collect();
        let out = match find_key_line(&lines, key) {
            Some(key_line) => {
                let end = value_block_end(&lines, key_line);
                let mut out = String::new();
                if key_line > 0 {
                    out.push_str(&lines[..key_line].join("\n"));
                    out.push('\n');
                }
                out.push_str(&repl);
                out.push('\n');
                out.push_str(&lines[end..].join("\n"));
                if !self.raw.ends_with('\n') && out.ends_with('\n') {
                    out.pop();
                }
                out
            }
            None => {
                let mut out = self.raw.clone();
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push_str(eol_cr);
                    out.push('\n');
                }
                out.push_str(&repl);
                out.push('\n'); // the last replacement line carries `eol_cr`
                out
            }
        };
        // A well-formed fragment must leave a well-formed document (checked
        // only when the original parsed — pre-existing invalid bytes are the
        // author's business, not this patch's).
        if self.parse().is_some() {
            serde_yaml::from_str::<serde_yaml::Mapping>(&out)
                .map_err(|e| anyhow::anyhow!("patching {key} would leave invalid YAML: {e}"))?;
        }
        self.raw = out;
        Ok(())
    }

    /// Set a top-level scalar, patching only that key's lines. Existing value
    /// block is replaced in place; a missing key is appended at the end.
    pub fn set_str(&mut self, key: &str, value: &str) -> Result<()> {
        if value.contains('\n') {
            bail!("multi-line YAML values are not supported yet");
        }
        if key.is_empty() || key.contains(':') || key.contains('\n') {
            bail!("invalid YAML key: {key:?}");
        }
        let line = self.render_pair(key, value)?;
        let eol_cr = if self.crlf { "\r" } else { "" };

        let lines: Vec<&str> = self.raw.split('\n').collect();
        match find_key_line(&lines, key) {
            Some(key_line) => {
                let end = value_block_end(&lines, key_line);
                // Reconstruct as if joined with '\n': prefix + new pair +
                // suffix. The split's trailing "" (from a newline-terminated
                // document) collapses back correctly through join semantics.
                let mut out = String::new();
                if key_line > 0 {
                    out.push_str(&lines[..key_line].join("\n"));
                    out.push('\n');
                }
                out.push_str(&line);
                out.push_str(eol_cr);
                out.push('\n');
                out.push_str(&lines[end..].join("\n"));
                if !self.raw.ends_with('\n') && out.ends_with('\n') {
                    out.pop();
                }
                self.raw = out;
            }
            None => {
                // Append at end, keeping the document newline-terminated.
                if !self.raw.is_empty() && !self.raw.ends_with('\n') {
                    self.raw.push_str(eol_cr);
                    self.raw.push('\n');
                }
                self.raw.push_str(&line);
                self.raw.push_str(eol_cr);
                self.raw.push('\n');
            }
        }
        Ok(())
    }

    /// `key: <repr>` where repr is a YAML-safe rendering of the value.
    fn render_pair(&self, key: &str, value: &str) -> Result<String> {
        let rep = serde_yaml::to_string(&serde_yaml::Value::String(value.to_string()))
            .map_err(|e| anyhow::anyhow!("cannot render {key}: {e}"))?;
        let rep = rep.trim_end_matches('\n');
        if rep.contains('\n') {
            // serde_yaml chose a block style for this value; we don't (yet).
            bail!("cannot render multi-line YAML value for {key}");
        }
        Ok(format!("{key}: {rep}"))
    }
}

/// Index of the top-level line introducing `key:` (indentation-sensitive so a
/// nested `id:` under another key is never mistaken for the top-level one).
fn find_key_line(lines: &[&str], key: &str) -> Option<usize> {
    let prefix = format!("{key}:");
    lines.iter().position(|l| {
        let indent = l.len() - l.trim_start().len();
        indent == 0 && l.starts_with(&prefix)
    })
}

/// First index *after* the value block that starts at `key_line`: consumes
/// indented continuation lines (block scalars, nested maps) plus blank lines
/// that sit inside them, stopping before any top-level line — so comments and
/// blank lines *between* keys survive outside the replaced region.
fn value_block_end(lines: &[&str], key_line: usize) -> usize {
    let mut end = key_line + 1;
    let mut i = end;
    while i < lines.len() {
        let l = lines[i];
        let indented =
            |s: &str| (s.starts_with(' ') || s.starts_with('\t')) && !s.trim().is_empty();
        if indented(l) {
            i += 1;
            end = i;
            continue;
        }
        if l.trim().is_empty() {
            // A blank line belongs to the block only if the next non-blank
            // line is indented (interior of a block scalar).
            match lines[i + 1..].iter().find(|n| !n.trim().is_empty()) {
                Some(n) if indented(n) => {
                    i += 1;
                    end = i;
                    continue;
                }
                _ => break,
            }
        }
        break; // top-level key, comment, fence, ...
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_scalars_and_seqs() {
        let y =
            LosslessYaml::new("id: ch_1\ncount: 3\nok: true\ntags: [a, b]\nnested:\n  deep: 1\n");
        assert_eq!(y.get_str("id").as_deref(), Some("ch_1"));
        assert_eq!(y.get_u64("count"), Some(3));
        assert_eq!(y.get_str("ok").as_deref(), Some("true"));
        assert_eq!(y.get_seq("tags"), Some(vec!["a".into(), "b".into()]));
        assert_eq!(y.get_str("nested").as_deref(), Some("deep: 1"));
        assert_eq!(y.get_str("missing"), None);
    }

    #[test]
    fn set_preserves_comments_unknown_keys_and_order() {
        let raw = "# header comment\nid: old\ntitle: T\n# between\nstatus: draft\nweird: [1, 2]\n";
        let mut y = LosslessYaml::new(raw);
        y.set_str("status", "revised").unwrap();
        let out = y.raw();
        assert!(out.contains("# header comment"));
        assert!(out.contains("# between"));
        assert!(out.contains("weird: [1, 2]"));
        assert!(out.contains("status: revised"));
        assert!(!out.contains("draft"));
        // everything else byte-identical
        assert_eq!(
            out,
            "# header comment\nid: old\ntitle: T\n# between\nstatus: revised\nweird: [1, 2]\n"
        );
    }

    #[test]
    fn set_appends_missing_key_at_end() {
        let mut y = LosslessYaml::new("a: 1\n");
        y.set_str("b", "two").unwrap();
        assert_eq!(y.raw(), "a: 1\nb: two\n");
    }

    #[test]
    fn set_replaces_block_scalar_value_only() {
        let raw = "notes: |\n  line one\n  line two\nnext: 1\n";
        let mut y = LosslessYaml::new(raw);
        y.set_str("notes", "flat").unwrap();
        assert_eq!(y.raw(), "notes: flat\nnext: 1\n");
    }

    #[test]
    fn set_nested_key_does_not_touch_deeper_same_name() {
        let raw = "child:\n  id: nested\nid: top\n";
        let mut y = LosslessYaml::new(raw);
        y.set_str("id", "patched").unwrap();
        assert_eq!(y.raw(), "child:\n  id: nested\nid: patched\n");
    }

    #[test]
    fn crlf_style_preserved_by_patch() {
        let mut y = LosslessYaml::new("a: 1\r\nb: 2\r\n");
        assert!(y.crlf);
        y.set_str("a", "9").unwrap();
        assert_eq!(y.raw(), "a: '9'\r\nb: 2\r\n");
        y.set_str("c", "3").unwrap();
        assert_eq!(y.raw(), "a: '9'\r\nb: 2\r\nc: '3'\r\n");
    }

    #[test]
    fn multiline_values_rejected() {
        let mut y = LosslessYaml::new("a: 1\n");
        assert!(y.set_str("b", "x\ny").is_err());
        assert_eq!(y.raw(), "a: 1\n", "rejected patch must not modify bytes");
    }

    #[test]
    fn invalid_yaml_still_reads_raw_and_patches_simple_keys() {
        let mut y = LosslessYaml::new("a: [unclosed\n");
        assert!(y.parse().is_none());
        y.set_str("a", "ok").unwrap();
        assert_eq!(y.raw(), "a: ok\n");
    }

    #[test]
    fn get_value_reads_nested_structures() {
        let y = LosslessYaml::new("states:\n  - at: ch_1\n    location: x\n");
        let v = y.get_value("states").unwrap();
        let seq = v.as_sequence().unwrap();
        assert_eq!(seq[0].get("at").unwrap().as_str(), Some("ch_1"));
        assert!(y.get_value("missing").is_none());
    }

    #[test]
    fn set_block_patches_sequence_value_in_place() {
        let raw = "# keep\nid: ch_1\nstates:\n  - at: old\n    location: x\nnext: 1\n";
        let mut y = LosslessYaml::new(raw);
        y.set_block("states", "- at: new\n  location: y").unwrap();
        assert_eq!(
            y.raw(),
            "# keep\nid: ch_1\nstates:\n  - at: new\n    location: y\nnext: 1\n"
        );
        let v = y.get_value("states").unwrap();
        assert_eq!(
            v.as_sequence().unwrap()[0].get("at").unwrap().as_str(),
            Some("new")
        );
    }

    #[test]
    fn set_block_appends_missing_key_and_crlf_survives() {
        let mut y = LosslessYaml::new("a: 1\r\n");
        y.set_block("states", "- at: ch_1\r\n  location: x\r\n")
            .unwrap();
        assert_eq!(
            y.raw(),
            "a: 1\r\nstates:\r\n  - at: ch_1\r\n    location: x\r\n"
        );
    }

    #[test]
    fn set_block_rejects_malformed_fragment_without_touching_bytes() {
        let mut y = LosslessYaml::new("a: 1\n");
        let err = y.set_block("states", "- at: [unclosed").unwrap_err();
        assert!(err.to_string().contains("invalid YAML value"), "{err}");
        assert_eq!(y.raw(), "a: 1\n");
    }

    #[test]
    fn set_block_rejects_scalar_fragments() {
        let mut y = LosslessYaml::new("a: 1\n");
        let err = y.set_block("states", "just a string").unwrap_err();
        assert!(err.to_string().contains("sequence or mapping"), "{err}");
        assert_eq!(y.raw(), "a: 1\n");
    }

    #[test]
    fn set_block_rejects_fragment_that_would_break_document() {
        // The fragment trips YAML validation (ambiguous marker/sequence mix);
        // either way the bytes must stay untouched.
        let mut y = LosslessYaml::new("a: 1\n");
        assert!(y.set_block("events", "- id: x\n---\nother: y").is_err());
        assert_eq!(y.raw(), "a: 1\n");
    }

    #[test]
    fn quote_safe_rendering() {
        let mut y = LosslessYaml::new("t: x\n");
        y.set_str("t", "with: colon").unwrap();
        assert_eq!(y.raw(), "t: 'with: colon'\n");
        assert_eq!(y.get_str("t").as_deref(), Some("with: colon"));
    }
}
