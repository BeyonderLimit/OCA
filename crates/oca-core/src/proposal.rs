//! Proposals (A5, Phase 8): AI output never touches project files directly
//! (AGENTS rule 4). Generation produces a [`Proposal`] — replacement text
//! plus the exact anchor it was written against — stored under
//! `.oca/state/ai/` (precious state, rule 2). Applying goes through
//! [`crate::Core`], which takes an auto-snapshot first (the Phase 6 hook)
//! and refuses to clobber the author's edits: the recorded range still
//! holds the anchor → in place; the anchor survives elsewhere → rebase;
//! otherwise → `stale`, never a silent overwrite.
//!
//! The on-disk checksum field is `base_checksum` — rule 7 keeps Git
//! vocabulary out of ours, whatever earlier sketches called it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use oca_storage::atomic::atomic_write;

/// Directory under the project's precious state dir where proposals live.
pub const PROPOSAL_DIR: &str = ".oca/state/ai";

/// One line of a proposal's context manifest: what was in the prompt.
/// Frontends flow these through Core, so they live here (rule 8).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// Block kind (`chapter`, `character`, `search`, …).
    pub label: String,
    /// Provenance string — id, project-relative path, or query.
    pub source: String,
}

/// What the payload replaces: a document body byte range plus the exact
/// text that occupied it when the proposal was made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalTarget {
    /// Project-relative document path.
    pub document: PathBuf,
    /// Byte range in the document body (`start..end`, char boundaries).
    pub start: usize,
    pub end: usize,
    /// The body text that was at `start..end` — the apply anchor.
    pub anchor: String,
}

/// Lifecycle of a proposal. `Edited` is reserved for the Phase 9
/// accept-with-edits flow; nothing constructs it yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Pending,
    Accepted,
    Rejected,
    Stale,
    Edited,
}

impl ProposalStatus {
    /// The status as its lowercase name, for messages.
    pub fn as_str(self) -> &'static str {
        match self {
            ProposalStatus::Pending => "pending",
            ProposalStatus::Accepted => "accepted",
            ProposalStatus::Rejected => "rejected",
            ProposalStatus::Stale => "stale",
            ProposalStatus::Edited => "edited",
        }
    }
}

/// A first-class AI output (A5): replacement text with everything needed
/// to apply it safely and to audit how it was produced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    /// Fresh ULID-based id (`prop_…`).
    pub id: String,
    /// The operation that produced it (`rewrite_selection`, …).
    pub operation: String,
    /// Prompt template version — results stay comparable across changes.
    pub prompt_version: String,
    /// How the text was produced ("mock", a model name).
    pub model: String,
    /// Creation time, RFC 3339 UTC.
    pub created: String,
    pub target: ProposalTarget,
    /// `sha256:…` of the anchor text at generation time.
    pub base_checksum: String,
    /// What was in the prompt (ids/ranges), for provenance and debugging.
    pub context_manifest: Vec<ManifestEntry>,
    /// Replacement text for the target range.
    pub payload: String,
    pub status: ProposalStatus,
}

/// `sha256:…` over UTF-8 bytes — the same convention audio takes use.
pub fn checksum(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(text.as_bytes());
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256:");
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn dir(root: &Path) -> PathBuf {
    root.join(PROPOSAL_DIR)
}

fn file_of(root: &Path, id: &str) -> PathBuf {
    dir(root).join(format!("{id}.yaml"))
}

/// Persist a proposal (create or status change), atomically.
pub fn save(root: &Path, proposal: &Proposal) -> Result<()> {
    let yaml = serde_yaml::to_string(proposal).context("serialize proposal")?;
    let path = file_of(root, &proposal.id);
    atomic_write(&path, yaml.as_bytes())
        .with_context(|| format!("write proposal {}", proposal.id))?;
    Ok(())
}

/// Load one proposal by id.
pub fn load(root: &Path, id: &str) -> Result<Proposal> {
    let path = file_of(root, id);
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("no proposal {id} — it may have been applied and cleaned up"))?;
    serde_yaml::from_str(&raw).with_context(|| format!("parse proposal {id}"))
}

/// Load one proposal by id; `None` when it simply isn't there.
pub fn load_opt(root: &Path, id: &str) -> Result<Option<Proposal>> {
    if !file_of(root, id).exists() {
        return Ok(None);
    }
    load(root, id).map(Some)
}

/// Every proposal on disk, in id (creation) order.
pub fn list(root: &Path) -> Result<Vec<Proposal>> {
    let dir = dir(root);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut ids: Vec<String> = std::fs::read_dir(&dir)
        .with_context(|| format!("list {}", dir.display()))?
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".yaml").map(str::to_string)
        })
        .collect();
    ids.sort();
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.push(load(root, &id)?);
    }
    Ok(out)
}

/// Validate a `start..end` range against a body and return the anchored
/// text (`""` for an insertion point at the very end).
pub fn anchor_at(body: &str, start: usize, end: usize) -> Result<String> {
    if start > end {
        return Err(anyhow!("proposal range is inverted ({start}..{end})"));
    }
    if end > body.len() {
        return Err(anyhow!(
            "proposal range {start}..{end} reaches past the document body ({} bytes)",
            body.len()
        ));
    }
    match body.get(start..end) {
        Some(text) => Ok(text.to_string()),
        None => Err(anyhow!(
            "proposal range {start}..{end} does not sit on character boundaries"
        )),
    }
}

/// The apply rule (A5): the recorded range still holds the anchor → in
/// place; the anchor survives exactly once elsewhere → rebase; otherwise
/// the caller must mark the proposal `stale`.
pub fn locate(body: &str, target: &ProposalTarget) -> Locate {
    // In place: byte range still valid and still exactly the anchor.
    if let Ok(text) = anchor_at(body, target.start, target.end)
        && text == target.anchor
    {
        return Locate::Range {
            start: target.start,
            end: target.end,
            rebased: false,
        };
    }
    // An empty anchor (an insertion proposal) has nothing to search for.
    if target.anchor.is_empty() {
        return if body.len() >= target.end {
            // The insertion point still exists (possibly mid-document).
            Locate::Range {
                start: target.start,
                end: target.end.min(body.len()),
                rebased: false,
            }
        } else {
            Locate::Stale
        };
    }
    // Rebase: the anchor moved but survives exactly once.
    let first = body.find(&target.anchor);
    match first {
        Some(start)
            if body[start + target.anchor.len()..]
                .find(&target.anchor)
                .is_none() =>
        {
            Locate::Range {
                start,
                end: start + target.anchor.len(),
                rebased: true,
            }
        }
        _ => Locate::Stale,
    }
}

/// Where the anchor was found (or that it is gone).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Locate {
    Range {
        start: usize,
        end: usize,
        rebased: bool,
    },
    Stale,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(anchor: &str) -> ProposalTarget {
        ProposalTarget {
            document: PathBuf::from("manuscript/001-x.md"),
            start: 4,
            end: 4 + anchor.len(),
            anchor: anchor.into(),
        }
    }

    #[test]
    fn locate_in_place_rebase_and_stale() {
        let t = target("world");
        assert_eq!(
            locate("say world now", &t),
            Locate::Range {
                start: 4,
                end: 9,
                rebased: false
            }
        );
        // Inserted text earlier shifts the range; the anchor survives once.
        assert_eq!(
            locate("longer: say world now", &t),
            Locate::Range {
                start: 12,
                end: 17,
                rebased: true
            }
        );
        // The anchor itself was edited — nothing to apply onto.
        assert_eq!(locate("say worls now", &t), Locate::Stale);
        // Two surviving copies are ambiguous — never guess.
        assert_eq!(locate("world, world", &target("world")), Locate::Stale);
    }

    #[test]
    fn locate_empty_anchor_appends_only_where_it_was() {
        let t = ProposalTarget {
            document: PathBuf::from("manuscript/001-x.md"),
            start: 9,
            end: 9,
            anchor: String::new(),
        };
        // Body long enough: the insertion point exists.
        assert_eq!(
            locate("say word ", &t),
            Locate::Range {
                start: 9,
                end: 9,
                rebased: false
            }
        );
        // Body shrank past it — stale rather than a surprise mid-text insert.
        assert_eq!(locate("short", &t), Locate::Stale);
    }

    #[test]
    fn anchor_at_validates_range_and_boundaries() {
        assert_eq!(anchor_at("héllo", 0, 3).unwrap(), "hé");
        assert!(anchor_at("héllo", 2, 3).is_err()); // not a char boundary
        assert!(anchor_at("hi", 0, 9).is_err()); // past the body
        assert!(anchor_at("hi", 2, 1).is_err()); // inverted
    }

    #[test]
    fn checksum_is_sha256_prefixed() {
        assert_eq!(
            checksum("echo"),
            "sha256:092c79e8f80e559e404bcf660c48f3522b67aba9ff1484b0367e1a4ddef7431d"
        );
    }
}
