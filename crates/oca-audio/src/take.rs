//! Take metadata: the sidecar YAML that lives next to every `.wav`.
//!
//! Files are canonical (rule 2): a take is `<stem>.wav` + `<stem>.yaml`, and
//! everything OCA knows about the recording sits in the sidecar. The
//! `take_id` is an immutable ULID — renames change the filename only, so
//! attachments keyed by id never break.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where a take is linked from. Links carry ids, never paths (A2): the
/// target may be renamed at any time without touching the audio.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Attachment {
    /// Project-level voice note (no specific target).
    Project,
    /// Attached to a manuscript chapter by its frontmatter id.
    Chapter { id: String },
    /// Attached to a song by its `song.yaml` id.
    Song { id: String },
}

impl Attachment {
    pub fn short(&self) -> &'static str {
        match self {
            Attachment::Project => "project",
            Attachment::Chapter { .. } => "chapter",
            Attachment::Song { .. } => "song",
        }
    }

    /// One-line description for status bars and `oca audio list`.
    pub fn describe(&self) -> String {
        match self {
            Attachment::Project => "project".into(),
            Attachment::Chapter { id } => format!("chapter {id}"),
            Attachment::Song { id } => format!("song {id}"),
        }
    }
}

/// The Creative Note that can ride along with a take (`OCA-build.md` §10).
/// Every field is optional; v1 writes `text`/`lyrics`/`tempo_bpm` only.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Note {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lyrics: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tempo_bpm: Option<u32>,
    /// Reserved for melody/MIDI data (Phase 17 — v1 does no melody
    /// extraction and never writes this field). It round-trips through
    /// every sidecar patch so data from a future version survives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub melody: Option<serde_yaml::Value>,
}

impl Note {
    pub fn is_empty(&self) -> bool {
        self.text.is_none() && self.lyrics.is_none() && self.tempo_bpm.is_none()
    }
}

/// The raw transcript of a dictation take (Phase 7B): what the speaker
/// said, kept verbatim with the audio. It never rewrites itself into the
/// manuscript — where it goes is always an author's decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transcript {
    /// Verbatim recognized speech (already trimmed of surrounding space).
    pub text: String,
    /// RFC 3339 UTC timestamp of the transcription run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
}

/// The parsed sidecar document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TakeMeta {
    /// Immutable `take_…` id — attachments and external references use this.
    pub take_id: String,
    pub title: String,
    /// RFC 3339 UTC timestamp written when the take was saved.
    pub created: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub duration_ms: u64,
    /// Content digest of the WAV bytes (`sha256:…`); snapshots store audio
    /// by reference + checksum, never as blobs (A1).
    pub checksum: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attached: Option<Attachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<Note>,
    /// Set when the take has been transcribed (dictation, Phase 7B).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<Transcript>,
}

/// A take on disk: parsed metadata plus the two files' project-relative
/// paths.
#[derive(Clone, Debug, PartialEq)]
pub struct Take {
    pub meta: TakeMeta,
    pub wav_rel: PathBuf,
    pub sidecar_rel: PathBuf,
}

impl Take {
    /// The filename stem (`chorus-002` in `chorus-002.wav`).
    pub fn stem(&self) -> String {
        self.wav_rel
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Seconds, for humans (`12.3s`).
    pub fn duration_secs(&self) -> f64 {
        self.meta.duration_ms as f64 / 1_000.0
    }

    pub fn wav_abs(&self, root: &Path) -> PathBuf {
        root.join(&self.wav_rel)
    }

    pub fn sidecar_abs(&self, root: &Path) -> PathBuf {
        root.join(&self.sidecar_rel)
    }
}

/// Lowercase, ASCII-safe filename stem from a human title. Empty results
/// (titles with no usable characters) are an error, not a silent fallback.
pub fn slugify(title: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut last_dash = false;
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        return Err(format!(
            "title {title:?} has no usable characters for a filename"
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> TakeMeta {
        TakeMeta {
            take_id: "take_01ABCDEF".into(),
            title: "chorus idea".into(),
            created: "2026-10-15T12:00:00Z".into(),
            sample_rate: 44_100,
            channels: 1,
            duration_ms: 1_500,
            checksum: "sha256:00".into(),
            attached: Some(Attachment::Chapter { id: "ch_01".into() }),
            note: None,
            transcript: None,
        }
    }

    #[test]
    fn sidecar_yaml_round_trips() {
        let yaml = serde_yaml::to_string(&meta()).unwrap();
        let back: TakeMeta = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back, meta());
        assert!(yaml.contains("kind: chapter"));
    }

    #[test]
    fn a_dictation_transcript_round_trips() {
        let mut m = meta();
        m.transcript = Some(Transcript {
            text: "the lamp was lit\nand the door stood open".into(),
            created: Some("2026-10-16T09:30:00Z".into()),
        });
        let yaml = serde_yaml::to_string(&m).unwrap();
        assert!(yaml.contains("transcript:"), "{yaml}");
        let back: TakeMeta = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(
            back.transcript.as_ref().unwrap().text,
            "the lamp was lit\nand the door stood open"
        );

        // Sidecars without the key (every take saved before Phase 7B)
        // parse with no transcript at all.
        let mut plain = meta();
        plain.transcript = None;
        let legacy = serde_yaml::to_string(&plain).unwrap();
        let back: TakeMeta = serde_yaml::from_str(&legacy).unwrap();
        assert!(back.transcript.is_none());
    }

    #[test]
    fn absent_attachment_and_note_stay_absent() {
        let mut m = meta();
        m.attached = None;
        let yaml = serde_yaml::to_string(&m).unwrap();
        assert!(!yaml.contains("attached"));
        let back: TakeMeta = serde_yaml::from_str(&yaml).unwrap();
        assert!(back.attached.is_none());
    }

    #[test]
    fn reserved_melody_field_round_trips() {
        // A sidecar written by a future version keeps its melody data when
        // parsed by this one.
        let yaml = "take_id: take_x\ntitle: t\ncreated: 2026-01-01T00:00:00Z\nsample_rate: 8000\n\
                    channels: 1\nduration_ms: 1\nchecksum: sha256:00\n\
                    note:\n  text: hi\n  melody:\n    notes: [C4, E4]\n";
        let m: TakeMeta = serde_yaml::from_str(yaml).unwrap();
        let melody = m.note.as_ref().unwrap().melody.as_ref().unwrap();
        assert!(melody.get("notes").is_some());
    }

    #[test]
    fn null_attachment_reads_as_unattached() {
        let yaml = serde_yaml::to_string(&meta()).unwrap();
        let patched = yaml.replace("kind: chapter\n  id: ch_01", "");
        let m: TakeMeta = serde_yaml::from_str(&patched).unwrap();
        assert!(m.attached.is_none());
    }

    #[test]
    fn slugify_titles() {
        assert_eq!(slugify("Chorus Idea #7").unwrap(), "chorus-idea-7");
        assert_eq!(slugify("  spaced  out  ").unwrap(), "spaced-out");
        assert_eq!(slugify("UPPER").unwrap(), "upper");
        assert!(slugify("!!!").is_err());
        assert_eq!(slugify("a").unwrap(), "a");
    }

    #[test]
    fn attachment_descriptions() {
        assert_eq!(Attachment::Project.describe(), "project");
        assert_eq!(
            Attachment::Song {
                id: "song_ashes".into()
            }
            .describe(),
            "song song_ashes"
        );
    }
}
