//! The take library on disk: scan `audio/` + `music/songs/<song>/demos/`,
//! save new takes atomically, and patch sidecars losslessly so unknown keys
//! (including future melody data) survive every edit.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use oca_model::new_id;
use oca_model::yaml::LosslessYaml;
use oca_storage::atomic::atomic_write;
use sha2::{Digest, Sha256};

use crate::record::RecordedAudio;
use crate::take::{Attachment, Note, Take, TakeMeta, slugify};
use crate::wav::{DecodedAudio, decode_wav, encode_wav};

/// Where takes live. The location is a convenience; the sidecar's
/// `attached:` block is the authoritative link (renames and moves never
/// break attachments because they reference ids).
pub fn validate_dir(dir_rel: &Path) -> Result<()> {
    let normal = dir_rel
        .components()
        .all(|c| matches!(c, Component::Normal(_)));
    if !normal {
        bail!("audio directory must be a plain project-relative path");
    }
    let comps: Vec<String> = dir_rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let is_audio = comps.first().is_some_and(|c| c == "audio");
    let is_demos =
        comps.len() == 4 && comps[0] == "music" && comps[1] == "songs" && comps[3] == "demos";
    if !is_audio && !is_demos {
        bail!("audio files live in audio/ or music/songs/<song>/demos/");
    }
    Ok(())
}

pub struct TakeStore {
    root: PathBuf,
}

impl TakeStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every take in the project, in directory then filename order. A `.wav`
    /// without a sidecar is not a take (OCA did not record it and has no id
    /// for it) and is skipped with a warning.
    pub fn list(&self) -> Result<Vec<Take>> {
        let mut dirs = vec![PathBuf::from("audio")];
        let songs_dir = self.root.join("music").join("songs");
        if let Ok(entries) = std::fs::read_dir(&songs_dir) {
            for entry in entries.flatten() {
                if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                if entry.path().join("demos").is_dir() {
                    let mut rel = PathBuf::from("music");
                    rel.push("songs");
                    rel.push(entry.file_name());
                    rel.push("demos");
                    dirs.push(rel);
                }
            }
        }
        dirs.sort();

        let mut takes = Vec::new();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(self.root.join(&dir)) else {
                continue;
            };
            let mut wavs: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "wav"))
                .collect();
            wavs.sort();
            for wav_abs in wavs {
                let stem = wav_abs
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let sidecar_rel = dir.join(format!("{stem}.yaml"));
                let raw = match std::fs::read_to_string(self.root.join(&sidecar_rel)) {
                    Ok(raw) => raw,
                    Err(_) => {
                        tracing::warn!("skipping {} — no sidecar (not an OCA take)", stem);
                        continue;
                    }
                };
                let meta: TakeMeta = match serde_yaml::from_str(&raw) {
                    Ok(meta) => meta,
                    Err(e) => {
                        tracing::warn!("skipping {} — unreadable sidecar: {e}", stem);
                        continue;
                    }
                };
                takes.push(Take {
                    meta,
                    wav_rel: dir.join(format!("{stem}.wav")),
                    sidecar_rel,
                });
            }
        }
        Ok(takes)
    }

    /// Resolve a take by id, id prefix, title, or filename stem — the forms
    /// an author actually types.
    pub fn resolve(&self, key: &str) -> Result<Take> {
        let takes = self.list()?;
        let lower = key.to_lowercase();
        let mut matches: Vec<&Take> = takes
            .iter()
            .filter(|t| {
                t.meta.take_id == key
                    || t.meta.take_id.to_lowercase().starts_with(&lower)
                    || t.meta.title.to_lowercase() == lower
                    || t.stem().to_lowercase() == lower
            })
            .collect();
        if matches.is_empty() {
            bail!("no take matches {key:?} (try `oca audio list`)");
        }
        if matches.len() > 1 {
            let ids: Vec<&str> = matches.iter().map(|t| t.meta.take_id.as_str()).collect();
            bail!("{key:?} is ambiguous — matches {}", ids.join(", "));
        }
        Ok(matches.remove(0).clone())
    }

    /// Save recorded samples as a new take inside `dir_rel`.
    pub fn save(&self, dir_rel: &Path, audio: &RecordedAudio, title: Option<&str>) -> Result<Take> {
        validate_dir(dir_rel)?;
        if audio.samples.is_empty() {
            bail!("recording captured no audio");
        }
        if audio.spec.channels == 0 || audio.spec.sample_rate == 0 {
            bail!("recording has an invalid format");
        }
        let dir_abs = self.root.join(dir_rel);
        std::fs::create_dir_all(&dir_abs)
            .with_context(|| format!("cannot create {}", dir_abs.display()))?;

        let stem = match title {
            Some(t) => slugify(t).map_err(|e| anyhow::anyhow!(e))?,
            None => self.next_take_stem(&dir_abs)?,
        };
        let (stem, wav_rel, sidecar_rel) = self.unique_stem(dir_rel, &stem);

        let wav_bytes = encode_wav(&audio.samples, audio.spec)?;
        let mut hasher = Sha256::new();
        hasher.update(&wav_bytes);
        let checksum = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();

        let meta = TakeMeta {
            take_id: new_id("take").to_string(),
            title: title.unwrap_or(&stem).to_string(),
            created: now_rfc3339(),
            sample_rate: audio.spec.sample_rate,
            channels: audio.spec.channels,
            duration_ms: audio.duration_ms(),
            checksum: format!("sha256:{checksum}"),
            attached: None,
            note: None,
        };
        let sidecar_bytes = serde_yaml::to_string(&meta).context("cannot render take metadata")?;

        atomic_write(&self.root.join(&wav_rel), &wav_bytes)
            .with_context(|| format!("cannot write {}", wav_rel.display()))?;
        atomic_write(&self.root.join(&sidecar_rel), sidecar_bytes.as_bytes())
            .with_context(|| format!("cannot write {}", sidecar_rel.display()))?;
        tracing::info!("saved take {} to {}", meta.take_id, wav_rel.display());
        Ok(Take {
            meta,
            wav_rel,
            sidecar_rel,
        })
    }

    /// Rename the files; the `take_id` (and every attachment) is untouched.
    pub fn rename(&self, take: &Take, new_stem: &str) -> Result<Take> {
        let stem = slugify(new_stem).map_err(|e| anyhow::anyhow!(e))?;
        if take.stem() == stem {
            return Ok(take.clone());
        }
        let dir_rel = take
            .wav_rel
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let new_wav_rel = dir_rel.join(format!("{stem}.wav"));
        let new_sidecar_rel = dir_rel.join(format!("{stem}.yaml"));
        let new_wav_abs = self.root.join(&new_wav_rel);
        if new_wav_abs.exists() {
            bail!("{} already exists", new_wav_rel.display());
        }
        std::fs::rename(take.wav_abs(&self.root), &new_wav_abs)
            .with_context(|| format!("cannot rename to {}", new_wav_rel.display()))?;
        let sidecar_abs = take.sidecar_abs(&self.root);
        if sidecar_abs.exists() {
            std::fs::rename(&sidecar_abs, self.root.join(&new_sidecar_rel))
                .with_context(|| format!("cannot rename to {}", new_sidecar_rel.display()))?;
        }
        Ok(Take {
            meta: take.meta.clone(),
            wav_rel: new_wav_rel,
            sidecar_rel: new_sidecar_rel,
        })
    }

    pub fn delete(&self, take: &Take) -> Result<()> {
        let wav_abs = take.wav_abs(&self.root);
        std::fs::remove_file(&wav_abs)
            .with_context(|| format!("cannot delete {}", take.wav_rel.display()))?;
        let sidecar_abs = take.sidecar_abs(&self.root);
        if let Err(e) = std::fs::remove_file(&sidecar_abs)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return Err(e).with_context(|| format!("cannot delete {}", take.sidecar_rel.display()));
        }
        Ok(())
    }

    /// Set (`Some`) or clear (`None`) the attachment, patching only that
    /// block — unknown keys and formatting elsewhere survive byte-for-byte.
    pub fn attach(&self, take: &Take, target: Option<&Attachment>) -> Result<Take> {
        match target {
            Some(attachment) => {
                let block = serde_yaml::to_string(attachment).context("cannot render target")?;
                self.patch(take, "attached", Some(&block))
            }
            None => self.patch(take, "attached", None),
        }
    }

    /// Replace the Creative Note block (or clear it when every field is
    /// empty). Reserved `melody:` data already in the sidecar is carried
    /// over automatically: v1 has no way to set it and no way to lose it.
    pub fn set_note(&self, take: &Take, note: Option<&Note>) -> Result<Take> {
        match note {
            Some(note) if !note.is_empty() => {
                let mut note = note.clone();
                if note.melody.is_none() {
                    note.melody = take.meta.note.as_ref().and_then(|n| n.melody.clone());
                }
                let block = serde_yaml::to_string(&note).context("cannot render note")?;
                self.patch(take, "note", Some(&block))
            }
            _ => self.patch(take, "note", None),
        }
    }

    pub fn load(&self, take: &Take) -> Result<DecodedAudio> {
        let bytes = std::fs::read(take.wav_abs(&self.root)).context("cannot read take audio")?;
        decode_wav(&bytes)
    }

    // ---- internals -------------------------------------------------------

    fn patch(&self, take: &Take, key: &str, block: Option<&str>) -> Result<Take> {
        let sidecar_abs = take.sidecar_abs(&self.root);
        let raw = std::fs::read_to_string(&sidecar_abs)
            .with_context(|| format!("cannot read {}", take.sidecar_rel.display()))?;
        let mut doc = LosslessYaml::new(raw);
        match block {
            Some(value) => doc.set_block(key, value)?,
            None => doc.remove_key(key)?,
        }
        atomic_write(&sidecar_abs, doc.raw().as_bytes())
            .with_context(|| format!("cannot write {}", take.sidecar_rel.display()))?;
        let meta: TakeMeta = serde_yaml::from_str(doc.raw()).with_context(|| {
            format!(
                "{} is inconsistent after patching",
                take.sidecar_rel.display()
            )
        })?;
        Ok(Take {
            meta,
            wav_rel: take.wav_rel.clone(),
            sidecar_rel: take.sidecar_rel.clone(),
        })
    }

    fn next_take_stem(&self, dir_abs: &Path) -> Result<String> {
        for n in 1u32.. {
            let stem = format!("take-{n:03}");
            if !dir_abs.join(format!("{stem}.wav")).exists() {
                return Ok(stem);
            }
        }
        unreachable!("take numbering exhausted")
    }

    /// If `stem` is taken, append `-2`, `-3`, … until it is free.
    fn unique_stem(&self, dir_rel: &Path, stem: &str) -> (String, PathBuf, PathBuf) {
        let mut candidate = stem.to_string();
        for n in 2u32.. {
            let wav_rel = dir_rel.join(format!("{candidate}.wav"));
            if !self.root.join(&wav_rel).exists() {
                let sidecar_rel = dir_rel.join(format!("{candidate}.yaml"));
                return (candidate, wav_rel, sidecar_rel);
            }
            candidate = format!("{stem}-{n}");
        }
        unreachable!("stem numbering exhausted")
    }
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wav::AudioSpec;

    fn recorded(seconds: f32) -> RecordedAudio {
        let spec = AudioSpec::mono(8_000);
        let frames = (spec.sample_rate as f32 * seconds) as usize;
        RecordedAudio {
            samples: (0..frames).map(|i| (i % 100) as f32 / 100.0).collect(),
            spec,
        }
    }

    fn store() -> (tempfile::TempDir, TakeStore) {
        let tmp = tempfile::tempdir().unwrap();
        let store = TakeStore::new(tmp.path());
        (tmp, store)
    }

    #[test]
    fn dir_validation() {
        assert!(validate_dir(Path::new("audio")).is_ok());
        assert!(validate_dir(Path::new("audio/voice")).is_ok());
        assert!(
            validate_dir(Path::new("music/songs/ashes/demos")).is_ok(),
            "song demos are a take directory"
        );
        assert!(validate_dir(Path::new("manuscript")).is_err());
        assert!(validate_dir(Path::new("music/songs/ashes")).is_err());
        assert!(validate_dir(Path::new("../escape")).is_err());
        assert!(validate_dir(Path::new("/absolute")).is_err());
    }

    #[test]
    fn save_list_resolve_delete() {
        let (_tmp, store) = store();
        let take = store
            .save(Path::new("audio"), &recorded(0.1), None)
            .unwrap();
        assert_eq!(take.wav_rel, PathBuf::from("audio/take-001.wav"));
        assert!(take.meta.take_id.starts_with("take_"));
        assert!(take.meta.checksum.starts_with("sha256:"));
        assert_eq!(take.meta.duration_ms, 100);

        let takes = store.list().unwrap();
        assert_eq!(takes.len(), 1);
        assert_eq!(
            store.resolve("take-001").unwrap().meta.take_id,
            take.meta.take_id
        );
        assert_eq!(
            store.resolve(&take.meta.take_id).unwrap().meta.take_id,
            take.meta.take_id
        );
        assert!(store.resolve("nope").is_err());

        let second = store
            .save(Path::new("audio"), &recorded(0.1), None)
            .unwrap();
        assert_eq!(second.wav_rel, PathBuf::from("audio/take-002.wav"));

        store.delete(&take).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn titled_saves_slugify_and_dedupe() {
        let (_tmp, store) = store();
        let a = store
            .save(Path::new("audio"), &recorded(0.05), Some("Chorus Idea #7"))
            .unwrap();
        assert_eq!(a.wav_rel, PathBuf::from("audio/chorus-idea-7.wav"));
        assert_eq!(a.meta.title, "Chorus Idea #7");
        let b = store
            .save(Path::new("audio"), &recorded(0.05), Some("Chorus Idea #7"))
            .unwrap();
        assert_eq!(b.wav_rel, PathBuf::from("audio/chorus-idea-7-2.wav"));
    }

    #[test]
    fn save_rejects_non_audio_dirs_and_empty_audio() {
        let (_tmp, store) = store();
        assert!(
            store
                .save(Path::new("manuscript"), &recorded(0.1), None)
                .is_err()
        );
        assert!(
            store
                .save(
                    Path::new("audio"),
                    &RecordedAudio {
                        samples: vec![],
                        spec: AudioSpec::mono(8_000)
                    },
                    None
                )
                .is_err()
        );
    }

    #[test]
    fn bare_wavs_without_sidecars_are_not_takes() {
        let (_tmp, store) = store();
        std::fs::create_dir_all(store.root().join("audio")).unwrap();
        std::fs::write(store.root().join("audio/dropped-in.wav"), b"RIFF").unwrap();
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn rename_keeps_id_and_moves_both_files() {
        let (_tmp, store) = store();
        let take = store
            .save(Path::new("audio"), &recorded(0.1), Some("mic idea"))
            .unwrap();
        assert_eq!(take.wav_rel, PathBuf::from("audio/mic-idea.wav"));
        let renamed = store.rename(&take, "guitar-take").unwrap();
        assert_eq!(renamed.wav_rel, PathBuf::from("audio/guitar-take.wav"));
        assert_eq!(renamed.meta.take_id, take.meta.take_id);
        assert_eq!(renamed.meta.title, "mic idea", "rename touches files only");
        assert!(!take.wav_abs(store.root()).exists());
        assert!(renamed.wav_abs(store.root()).exists());
        // Old stem no longer resolves; new one and the title do.
        assert!(store.resolve("mic-idea").is_err());
        assert_eq!(
            store.resolve("guitar-take").unwrap().meta.take_id,
            take.meta.take_id
        );
        assert_eq!(
            store.resolve("mic idea").unwrap().meta.take_id,
            take.meta.take_id
        );
    }

    #[test]
    fn attach_patches_only_the_attached_block() {
        let (_tmp, store) = store();
        let take = store
            .save(Path::new("audio"), &recorded(0.1), None)
            .unwrap();
        let sidecar = take.sidecar_abs(store.root());
        // A future-format sidecar: extra key + comment must survive.
        let raw = std::fs::read_to_string(&sidecar).unwrap();
        std::fs::write(
            &sidecar,
            format!("# author comment\n{raw}future_field: 42\n"),
        )
        .unwrap();

        let updated = store
            .attach(&take, Some(&Attachment::Chapter { id: "ch_01".into() }))
            .unwrap();
        assert_eq!(
            updated.meta.attached,
            Some(Attachment::Chapter { id: "ch_01".into() })
        );
        let raw = std::fs::read_to_string(&sidecar).unwrap();
        assert!(raw.contains("# author comment"));
        assert!(raw.contains("future_field: 42"));

        let cleared = store.attach(&updated, None).unwrap();
        assert!(cleared.meta.attached.is_none());
        let raw = std::fs::read_to_string(&sidecar).unwrap();
        assert!(!raw.contains("attached"));
        assert!(raw.contains("future_field: 42"));
    }

    #[test]
    fn note_round_trip_preserves_reserved_melody() {
        let (_tmp, store) = store();
        let take = store
            .save(Path::new("audio"), &recorded(0.1), None)
            .unwrap();
        let sidecar = take.sidecar_abs(store.root());
        // Hand-add a note with future melody data, as a later version would.
        let raw = std::fs::read_to_string(&sidecar).unwrap();
        let raw = format!("{raw}note:\n  text: old\n  melody:\n    notes: [C4]\n");
        std::fs::write(&sidecar, raw).unwrap();
        let take = store.resolve("take-001").unwrap();

        // v1 edits the note's text; the reserved melody data must survive.
        let updated = store
            .set_note(
                &take,
                Some(&Note {
                    text: Some("needs reverb".into()),
                    ..Default::default()
                }),
            )
            .unwrap();
        let note = updated.meta.note.clone().unwrap();
        assert_eq!(note.text.as_deref(), Some("needs reverb"));
        assert!(note.melody.is_some(), "reserved melody data must survive");

        let cleared = store.set_note(&updated, None).unwrap();
        assert!(cleared.meta.note.is_none());
    }

    #[test]
    fn takes_in_song_demos_are_listed() {
        let (_tmp, store) = store();
        std::fs::create_dir_all(store.root().join("music/songs/ashes/demos")).unwrap();
        let take = store
            .save(Path::new("music/songs/ashes/demos"), &recorded(0.1), None)
            .unwrap();
        assert!(take.wav_rel.starts_with("music/songs/ashes/demos"));
        assert_eq!(store.list().unwrap().len(), 1);
    }
}
