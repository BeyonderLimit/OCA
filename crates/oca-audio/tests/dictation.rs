//! Phase 7B exit criteria: record → transcribe → insert works end to end
//! with only a whisper stand-in (no LLM anywhere), the raw transcript is
//! stored with the take, and **the transcript never auto-inserts into the
//! manuscript** — chapter/note writes happen only through the explicit
//! author actions.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use oca_audio::{Audio, FnTranscriber, MockHost, RecState, Take, Transcriber, default_take_dir};
use oca_core::{Command, Core};

const SAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/blackwood");

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

/// A private copy of the sample project, opened through Core.
fn open_sample() -> (tempfile::TempDir, Core, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dst = tmp.path().join("blackwood");
    copy_tree(Path::new(SAMPLE), &dst);
    let mut core = Core::new();
    core.execute(Command::OpenProject { root: dst.clone() })
        .unwrap();
    (tmp, core, dst)
}

fn recorded() -> oca_audio::RecordedAudio {
    oca_audio::RecordedAudio {
        samples: vec![0.25; 800],
        spec: oca_audio::AudioSpec::mono(8_000),
    }
}

/// The stand-in every test uses: WAV bytes in, canned text out — no
/// runtime, no model, no LLM (rule 3 mirrors rule 6's mock discipline).
fn canned(text: &str) -> Arc<dyn Transcriber> {
    let text = text.to_string();
    Arc::new(FnTranscriber::new(
        "stand-in",
        move |_wav: &Path, _c: &AtomicBool| Ok(text.clone()),
    ))
}

#[test]
fn fn_transcriber_adapts_any_closure() {
    let t = FnTranscriber::new("stand-in", |wav: &Path, _c: &AtomicBool| {
        Ok(format!(
            "said: {}",
            wav.file_name().unwrap().to_string_lossy()
        ))
    });
    assert_eq!(t.label(), "stand-in");
    let tmp = tempfile::tempdir().unwrap();
    let wav = tmp.path().join("clip.wav");
    std::fs::write(&wav, b"x").unwrap();
    let cancel = AtomicBool::new(false);
    assert_eq!(t.transcribe(&wav, &cancel).unwrap(), "said: clip.wav");
}

/// The headline flow: record (mock host) → transcribe → insert, and the
/// "never auto-inserts" guarantee checked between each step.
#[test]
fn record_transcribe_insert_works_and_nothing_lands_on_its_own() {
    let (_tmp, mut core, root) = open_sample();

    // The manuscript's state before anything dictation-related happens.
    let rel = core.resolve("ch_001").unwrap();
    let before = core.read_document(&rel).unwrap().body().to_string();
    assert!(!root.join("notes").exists(), "transcribe creates no notes");

    let host: Arc<dyn oca_audio::AudioHost> = Arc::new(MockHost::new());
    let text = {
        let facade =
            Audio::with_transcriber(&core, Arc::clone(&host), Some(canned("the lamp was lit")))
                .unwrap();

        // record → save (mock capture, no device)
        let recorder = facade.record().unwrap();
        assert_eq!(recorder.state(), RecState::Recording);
        let done = recorder.stop();
        let take = facade
            .save_take(&done, &default_take_dir(), Some("Voice memo"))
            .unwrap();

        // transcribe → raw transcript stored with the take, and *nowhere
        // else*: nothing about the manuscript or notes changed yet.
        let text = facade.transcribe(&take).unwrap();
        assert_eq!(text, "the lamp was lit");
        let stored = facade.take(&take.meta.take_id).unwrap();
        let transcript = stored.meta.transcript.clone().expect("transcript stored");
        assert_eq!(transcript.text, "the lamp was lit");
        assert!(transcript.created.is_some());

        // A rename keeps it: the sidecar content travels with the file.
        let renamed = facade.rename(&stored, "memo-renamed").unwrap();
        assert_eq!(
            renamed
                .meta
                .transcript
                .expect("transcript survives rename")
                .text,
            "the lamp was lit"
        );
        text
    };

    // Nothing has moved: same body, still no notes.
    let after = core.read_document(&rel).unwrap().body().to_string();
    assert_eq!(after, before, "a transcript never auto-inserts");
    assert!(!root.join("notes").exists());

    // insert — explicitly, and only now does the manuscript change.
    let inserted = oca_audio::dictation::insert_into_chapter(&mut core, "1", &text).unwrap();
    assert_eq!(inserted, rel);
    let body = core.read_document(&rel).unwrap().body().to_string();
    assert!(body.contains("the lamp was lit"), "{body}");
}

#[test]
fn add_to_notes_creates_the_note_once_then_appends() {
    let (_tmp, mut core, root) = open_sample();
    let host = Arc::new(MockHost::new());
    let facade = Audio::new(&core, host).unwrap();
    let take = facade
        .save_take(&recorded(), &default_take_dir(), Some("Idea"))
        .unwrap();

    let note = oca_audio::dictation::add_to_notes(&mut core, &take, "the lamp was lit").unwrap();
    assert_eq!(note, PathBuf::from("notes/dictation.md"));
    assert!(root.join("notes/dictation.md").is_file());
    let body = core.read_document(&note).unwrap().body().to_string();
    assert!(body.contains("## Idea"), "{body}");
    assert!(body.contains("the lamp was lit"), "{body}");

    // Second time appends to the same note instead of failing.
    oca_audio::dictation::add_to_notes(&mut core, &take, "second visit").unwrap();
    let body = core.read_document(&note).unwrap().body().to_string();
    assert!(body.contains("second visit"), "{body}");
    assert!(body.contains("the lamp was lit"), "{body}");

    // Empty transcripts are refused before anything is written.
    let err = oca_audio::dictation::add_to_notes(&mut core, &take, "  \n ")
        .unwrap_err()
        .to_string();
    assert!(err.contains("no transcript"), "{err}");
}

#[test]
fn without_a_transcriber_dictation_explains_itself() {
    let (_tmp, core, _root) = open_sample();
    let host = Arc::new(MockHost::new());
    let facade = Audio::new(&core, host).unwrap();
    assert!(facade.transcriber_label().is_none());
    let take = facade
        .save_take(&recorded(), &default_take_dir(), Some("Idea"))
        .unwrap();
    let err = facade.transcribe(&take).unwrap_err().to_string();
    assert!(err.contains("no dictation runtime"), "{err}");
    assert!(err.contains("runtime install"), "actionable: {err}");
    assert!(
        facade.take("idea").unwrap().meta.transcript.is_none(),
        "nothing was stored"
    );
}

#[test]
fn an_empty_transcript_is_refused_without_touching_the_sidecar() {
    let (_tmp, core, root) = open_sample();
    let facade =
        Audio::with_transcriber(&core, Arc::new(MockHost::new()), Some(canned("   \n  "))).unwrap();
    let take = facade
        .save_take(&recorded(), &default_take_dir(), Some("Idea"))
        .unwrap();
    let sidecar = root.join(&take.sidecar_rel);
    let before = std::fs::read_to_string(&sidecar).unwrap();
    let err = facade.transcribe(&take).unwrap_err().to_string();
    assert!(err.contains("no speech detected"), "{err}");
    assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), before);
}

#[test]
fn read_only_projects_refuse_transcription() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("blackwood");
    copy_tree(Path::new(SAMPLE), &root);
    // A future format_version opens the project read-only (Phase 1 gate).
    let yaml = root.join("project.yaml");
    let raw = std::fs::read_to_string(&yaml).unwrap();
    std::fs::write(
        &yaml,
        raw.replace("format_version: '1'", "format_version: '99'"),
    )
    .unwrap();
    let mut core = Core::new();
    core.execute(Command::OpenProject { root: root.clone() })
        .unwrap();
    let store = oca_audio::TakeStore::new(&root);
    let take = store
        .save(&default_take_dir(), &recorded(), Some("seed"))
        .unwrap();
    let facade =
        Audio::with_transcriber(&core, Arc::new(MockHost::new()), Some(canned("text"))).unwrap();
    let err = facade.transcribe(&take).unwrap_err().to_string();
    assert!(err.contains("cannot transcribe"), "{err}");
    assert!(err.contains("newer"), "{err}");
}

#[test]
fn transcribe_to_store_is_the_shared_engine_path() {
    // The TUI's job thread uses this directly with owned pieces.
    let (_tmp, core, root) = open_sample();
    let facade = Audio::new(&core, Arc::new(MockHost::new())).unwrap();
    let take: Take = facade
        .save_take(&recorded(), &default_take_dir(), Some("Idea"))
        .unwrap();
    let cancel = AtomicBool::new(false);
    let text = oca_audio::dictation::transcribe_to_store(
        facade.store(),
        &take,
        canned("engine-level transcript").as_ref(),
        &cancel,
    )
    .unwrap();
    assert_eq!(text, "engine-level transcript");
    let stored = facade.take(&take.meta.take_id).unwrap();
    assert_eq!(
        stored.meta.transcript.unwrap().text,
        "engine-level transcript"
    );
    let _ = root;
}
