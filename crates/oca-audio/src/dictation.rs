//! Dictation (Phase 7B): microphone takes → raw transcript, with no LLM
//! installed and nothing ever inserting itself into the manuscript.
//!
//! The split keeps the layering intact (rule 3 + rule 8): turning WAV
//! bytes into text is the [`Transcriber`] seam, implemented by the
//! whisper runtime in `oca-ai` and injected by the frontends; deciding
//! where a transcript *goes* is this crate's business, through explicit
//! author actions only — [`add_to_notes`] and [`insert_into_chapter`]
//! exist, and [`crate::Audio::transcribe`] writes nothing but the take's
//! own sidecar.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use anyhow::{Result, bail};
use oca_core::{Command, Core, Event};
use oca_model::DocumentKind;
use oca_story::Story;

use crate::store::TakeStore;
use crate::take::Take;

/// Speech-to-text behind one seam. The real implementation is
/// `oca_ai::Dictation` (whisper.cpp, another subprocess runtime after
/// Phase 7); every test here uses a stand-in, and with no transcriber
/// attached the dictation features explain themselves and stop.
pub trait Transcriber: Send + Sync {
    /// Runtime description for status lines ("whisper-cli · ggml-base").
    fn label(&self) -> String;
    /// Blocking transcription of one WAV file. The cancel token kills the
    /// runtime mid-run; an empty result means no speech was detected.
    fn transcribe(&self, wav: &Path, cancel: &AtomicBool) -> Result<String>;
}

/// A [`Transcriber`] built from any blocking function — the shape frontends
/// use to adapt `oca_ai::Dictation` (and tests use for stand-ins) without
/// this crate ever depending on the AI runtime.
pub struct FnTranscriber<F> {
    label: String,
    f: F,
}

impl<F> FnTranscriber<F> {
    pub fn new(label: impl Into<String>, f: F) -> Self {
        Self {
            label: label.into(),
            f,
        }
    }
}

impl<F> Transcriber for FnTranscriber<F>
where
    F: Fn(&Path, &AtomicBool) -> Result<String> + Send + Sync,
{
    fn label(&self) -> String {
        self.label.clone()
    }

    fn transcribe(&self, wav: &Path, cancel: &AtomicBool) -> Result<String> {
        (self.f)(wav, cancel)
    }
}

/// The guidance a frontend shows when no transcriber is available (rule 3:
/// every feature works without the runtime; this is all that's missing).
pub fn missing_runtime_hint() -> String {
    "no dictation runtime — side-load a pack with `oca runtime install` and put a \
     whisper model in ~/.oca/models/whisper (`oca doctor` shows both)"
        .to_string()
}

/// Run one transcription and store it with the take. The core of
/// [`crate::Audio::transcribe`], taking owned pieces so a job thread can
/// run it (the TUI's path) — empty transcripts are refused without
/// touching the sidecar.
pub fn transcribe_to_store(
    store: &TakeStore,
    take: &Take,
    transcriber: &dyn Transcriber,
    cancel: &AtomicBool,
) -> Result<String> {
    let text = transcriber.transcribe(&take.wav_abs(store.root()), cancel)?;
    if text.trim().is_empty() {
        bail!("no speech detected in {} — nothing was stored", take.stem());
    }
    let text = text.trim().to_string();
    store.set_transcript(take, &text)?;
    Ok(text)
}

/// Title of the note dictation appends to (`notes/dictation.md`).
pub const DICTATION_NOTE_TITLE: &str = "Dictation";

/// Append a transcript to the project's dictation note, creating the note
/// if it doesn't exist yet. An explicit author action — calling this *is*
/// the decision; nothing appends on its own when a transcription ends.
pub fn add_to_notes(core: &mut Core, take: &Take, transcript: &str) -> Result<PathBuf> {
    let text = transcript.trim();
    if text.is_empty() {
        bail!("no transcript to add — transcribe the take first");
    }
    let rel = match core.resolve(DICTATION_NOTE_TITLE) {
        Ok(rel) => rel,
        Err(_) => {
            let events = core.execute(Command::CreateDocument {
                kind: DocumentKind::Note,
                title: DICTATION_NOTE_TITLE.into(),
            })?;
            match events.into_iter().next() {
                Some(Event::DocumentCreated { rel, .. }) => rel,
                _ => bail!("cannot create the dictation note"),
            }
        }
    };
    let when = take
        .meta
        .transcript
        .as_ref()
        .and_then(|t| t.created.clone())
        .unwrap_or_else(|| take.meta.created.clone());
    let section = format!("## {} — {}\n\n{}", take.meta.title, when, text);
    core.execute(Command::AppendParagraph {
        doc: rel.clone(),
        text: section,
    })?;
    Ok(rel)
}

/// Append a transcript to a chapter's body (by sequence, title, or id —
/// resolved through the story engine, exactly like `attach`). Like
/// [`add_to_notes`], this runs only because the author asked: a
/// dictation never inserts itself into the manuscript.
pub fn insert_into_chapter(core: &mut Core, chapter: &str, transcript: &str) -> Result<PathBuf> {
    let text = transcript.trim();
    if text.is_empty() {
        bail!("no transcript to insert — transcribe the take first");
    }
    let target = Story::new(core).chapter(chapter)?;
    core.execute(Command::AppendParagraph {
        doc: target.rel.clone(),
        text: text.to_string(),
    })?;
    Ok(target.rel)
}
