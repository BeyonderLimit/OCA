//! Phase 7B exit criteria, end to end against a real spawned process:
//! transcription runs start, stream their result to the output file, and
//! cancel kills the runtime — with no LLM anywhere in the picture.
//!
//! CI has no whisper.cpp, so the "runtime" is this very test binary in a
//! disguise: `fake_whisper_cli` writes the transcript file the runtime
//! contract promises (`OCA_TRANSCRIBE_OUT`) when the child-only env marker
//! is present, and misbehaves on demand (fail, stall, produce nothing).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use oca_ai::{
    Dictation, HeavyModel, ModelManager, WhisperConfig, bootstrap_dictation, default_oca_dir,
};

/// The self-spawn entry: a normal `cargo test` run sees no env marker and
/// returns immediately; the child fakes one transcription run.
#[test]
fn fake_whisper_cli() {
    let Ok(behavior) = std::env::var("OCA_FAKE_WHISPER") else {
        return;
    };
    let out = std::env::var("OCA_TRANSCRIBE_OUT").expect("the runtime names the output file");
    // Loud noise on stdout, exactly what a chatty runtime would print —
    // the transcript contract is the output file, never the stream.
    println!("whisper.cpp fake: loading model, warming up, probably transcribing…");
    let (mode, text) = behavior.split_once(':').unwrap_or((&behavior, ""));
    match mode {
        "fail" => std::process::exit(1),
        "silent" => {}
        "sleep" => {
            let (ms, text) = text.split_once(':').unwrap_or((text, ""));
            std::thread::sleep(Duration::from_millis(ms.parse().unwrap_or(60_000)));
            std::fs::write(&out, text).expect("write transcript");
        }
        _ => std::fs::write(&out, text).expect("write transcript"),
    }
}

fn fake_config(behavior: &str) -> (WhisperConfig, tempfile::TempDir) {
    let tmp = tempfile::TempDir::new().unwrap();
    // Keep the child's log dir inside the temp home, not the real one.
    let mut config = WhisperConfig::new("fake.gguf", std::env::current_exe().unwrap());
    config.cli_args = Some(vec![
        "fake_whisper_cli".into(),
        "--exact".into(),
        "--quiet".into(),
    ]);
    config.cli_env = vec![
        ("OCA_FAKE_WHISPER".into(), behavior.into()),
        ("HOME".into(), tmp.path().to_string_lossy().into_owned()),
        (
            "USERPROFILE".into(),
            tmp.path().to_string_lossy().into_owned(),
        ),
    ];
    (config, tmp)
}

fn fake_wav(dir: &Path) -> PathBuf {
    let path = dir.join("take.wav");
    std::fs::write(&path, b"RIFF-not-really-audio-the-fake-never-reads-it").unwrap();
    path
}

fn dictation(behavior: &str) -> (Dictation, tempfile::TempDir) {
    let (config, tmp) = fake_config(behavior);
    (Dictation::new(config, ModelManager::new()), tmp)
}

fn not_cancelled() -> AtomicBool {
    AtomicBool::new(false)
}

/// The headline criterion: transcribe → transcript, 100 out of 100, no LLM
/// anywhere. Every round is a real spawned runtime.
#[test]
fn transcribe_100_of_100_with_a_real_spawned_runtime() {
    let (dict, tmp) = dictation("text:the lamp was still lit");
    let wav = fake_wav(tmp.path());
    for round in 0..100 {
        let cancel = not_cancelled();
        let text = dict
            .transcribe(&wav, &cancel)
            .unwrap_or_else(|e| panic!("round {round}: {e:#}"));
        assert_eq!(text, "the lamp was still lit", "round {round}");
    }
}

/// Cancel interrupts a long transcription: the runtime is killed, the
/// error says so, and nothing hangs.
#[test]
fn cancel_kills_the_runtime_and_reports_it() {
    let (dict, tmp) = dictation("sleep:60000:never");
    let wav = fake_wav(tmp.path());
    let cancel = Arc::new(AtomicBool::new(false));
    let worker = dict.clone();
    let wav2 = wav.clone();
    let cancel_flag = Arc::clone(&cancel);
    let handle = std::thread::spawn(move || worker.transcribe(&wav2, &cancel_flag));
    std::thread::sleep(Duration::from_millis(300));
    cancel.store(true, Ordering::SeqCst);
    let started = Instant::now();
    let err = handle
        .join()
        .expect("transcribe thread must not panic")
        .expect_err("a cancelled transcription must be an error")
        .to_string();
    assert!(err.contains("cancelled"), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "kill must be prompt"
    );
}

/// A runtime that exits non-zero is a clean error pointing at its log —
/// never a partial transcript, never a panic.
#[test]
fn a_failing_runtime_is_a_clean_error() {
    let (dict, tmp) = dictation("fail");
    let wav = fake_wav(tmp.path());
    let cancel = not_cancelled();
    let err = dict.transcribe(&wav, &cancel).unwrap_err().to_string();
    assert!(err.contains("exited"), "{err}");
    assert!(err.contains("whisper.log"), "points at the log: {err}");
}

/// The runtime finishing successfully but writing no transcript is an
/// error too — success must never look like missing output.
#[test]
fn missing_transcript_output_is_a_clean_error() {
    let (dict, tmp) = dictation("silent");
    let wav = fake_wav(tmp.path());
    let cancel = not_cancelled();
    let err = dict.transcribe(&wav, &cancel).unwrap_err().to_string();
    assert!(err.contains("produced no output"), "{err}");
}

/// No speech detected: an empty transcript is a valid outcome the caller
/// decides about — not a runtime failure.
#[test]
fn empty_transcript_is_a_result_not_an_error() {
    let (dict, tmp) = dictation("empty");
    let wav = fake_wav(tmp.path());
    let cancel = not_cancelled();
    assert_eq!(dict.transcribe(&wav, &cancel).unwrap(), "");
}

/// A missing audio file is caught before anything is spawned.
#[test]
fn missing_wav_is_a_clean_error() {
    let (dict, _tmp) = dictation("text:unused");
    let cancel = not_cancelled();
    let err = dict
        .transcribe(Path::new("/definitely/not/here.wav"), &cancel)
        .unwrap_err()
        .to_string();
    assert!(err.contains("not found"), "{err}");
}

/// The one-heavy-model governor (ADR-0006) covers dictation: while the
/// language model is resident, transcription is refused — explicitly.
#[test]
fn dictation_respects_the_residency_slot() {
    let (config, tmp) = fake_config("text:after you");
    let governor = ModelManager::new();
    governor.claim(HeavyModel::Llm).unwrap();
    let dict = Dictation::new(config, governor.clone());
    let wav = fake_wav(tmp.path());
    let cancel = not_cancelled();
    let err = dict.transcribe(&wav, &cancel).unwrap_err().to_string();
    assert!(err.contains("unload it first"), "{err}");

    governor.release(HeavyModel::Llm);
    assert_eq!(dict.transcribe(&wav, &cancel).unwrap(), "after you");
}

/// `bootstrap_dictation` needs both a runtime and a model, and works with
/// the LLM side of the house completely absent (rule 3).
#[test]
fn bootstrap_needs_runtime_and_model_but_no_llm() {
    let tmp = tempfile::TempDir::new().unwrap();
    let runtime_dir = tmp.path().join("runtime");
    let models_dir = tmp.path().join("models");
    let opts = oca_ai::BootstrapOptions {
        runtime_dir: Some(runtime_dir.clone()),
        whisper_models_dir: Some(models_dir.clone()),
        ..Default::default()
    };
    let governor = ModelManager::new();

    // Nothing at all: no dictation.
    assert!(
        bootstrap_dictation(&opts, governor.clone())
            .unwrap()
            .is_none()
    );

    // Runtime alone: still none.
    let pack_bin = runtime_dir.join("whisper-pack-1.0.0").join("bin");
    std::fs::create_dir_all(&pack_bin).unwrap();
    std::fs::write(pack_bin.join("whisper-cli"), b"#!/bin/sh\n").unwrap();
    assert!(
        bootstrap_dictation(&opts, governor.clone())
            .unwrap()
            .is_none()
    );

    // Model too: dictation appears, with the pack's runtime and the
    // largest model.
    std::fs::create_dir_all(&models_dir).unwrap();
    std::fs::write(models_dir.join("ggml-tiny.bin"), vec![0u8; 10]).unwrap();
    std::fs::write(models_dir.join("ggml-base.gguf"), vec![0u8; 20]).unwrap();
    let dict = bootstrap_dictation(&opts, governor.clone())
        .unwrap()
        .expect("dictation with runtime + model");
    assert_eq!(dict.info().model, "ggml-base");
    assert_eq!(dict.info().runtime, "whisper-cli");
    assert!(dict.info().label().contains("whisper-cli"));

    // An explicit-but-missing runtime path is reported, not papered over.
    let missing = oca_ai::BootstrapOptions {
        whisper_cli: Some(tmp.path().join("no-such-whisper")),
        ..opts.clone()
    };
    assert!(bootstrap_dictation(&missing, governor).unwrap().is_none());
}

/// A transcription never leaves its temp output behind (older leftovers
/// only — parallel rounds of the other tests may still be in flight).
#[test]
fn transcription_temp_files_are_cleaned_up() {
    let (dict, tmp) = dictation("text:tidy");
    let wav = fake_wav(tmp.path());
    let cancel = not_cancelled();
    dict.transcribe(&wav, &cancel).unwrap();
    let now = std::time::SystemTime::now();
    let leftovers: Vec<PathBuf> = std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("oca-transcribe-"))
        })
        .filter(|p| {
            p.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age > std::time::Duration::from_secs(30))
        })
        .collect();
    assert!(
        leftovers.is_empty(),
        "no temp transcripts may survive: {leftovers:?} ({} — default_oca_dir)",
        default_oca_dir().display()
    );
}
