//! [`Dictation`]: whisper.cpp as a one-shot transcription subprocess
//! (Phase 7B). Dictation works with **no LLM installed** — it is discovered
//! and bootstrapped independently of the language model, but it claims the
//! same one-heavy-model residency slot (ADR-0006) while it runs.
//!
//! One transcription = one spawned `whisper-cli` run over a WAV file:
//! `--output-txt` writes the transcript to a file we name, `--no-prints`
//! keeps the runtime quiet, and cancel kills the child. Nothing about the
//! capture lives here — `oca-audio` owns takes, the mic, and where a
//! transcript is allowed to go; this module only turns WAV bytes into text.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow};

use crate::{DiscoveredModel, ModelManager, default_oca_dir};

/// Runtime + model configuration for dictation. Fixed per instance.
#[derive(Clone, Debug)]
pub struct WhisperConfig {
    /// The GGUF (or legacy `.bin`) whisper model to load.
    pub model_path: PathBuf,
    /// The `whisper-cli` executable (runtime pack, then `PATH`).
    pub cli_bin: PathBuf,
    /// Spoken language code (`"fr"`), or `None` for the runtime's
    /// auto-detection.
    pub language: Option<String>,
    /// Worker threads (0 = runtime's choice).
    pub threads: u32,
    /// Replace the standard runtime arguments entirely — for test doubles
    /// and nonstandard runtimes. `None` = the standard `whisper-cli` flags
    /// (`-m`, `-f`, `-nt`, `-np`, `-otxt`, `-of`, plus `-l`/`-t` when set).
    pub cli_args: Option<Vec<String>>,
    /// Extra environment for the runtime process (test doubles, GPU
    /// selection). Applied only to the spawned child.
    pub cli_env: Vec<(String, String)>,
}

impl WhisperConfig {
    pub fn new(model_path: impl Into<PathBuf>, cli_bin: impl Into<PathBuf>) -> Self {
        Self {
            model_path: model_path.into(),
            cli_bin: cli_bin.into(),
            language: None,
            threads: 0,
            cli_args: None,
            cli_env: Vec::new(),
        }
    }
}

/// How the dictation runtime describes itself (`doctor`, status lines).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DictationInfo {
    /// The executable's file name — e.g. "whisper-cli".
    pub runtime: String,
    /// The model file's stem — e.g. "ggml-base".
    pub model: String,
}

impl DictationInfo {
    /// One-line description for status bars and CLI output.
    pub fn label(&self) -> String {
        format!("{} · {}", self.runtime, self.model)
    }
}

struct DictationInner {
    config: WhisperConfig,
    governor: ModelManager,
}

/// Transcription behind one handle (rule 6's whisper half): cloned handles
/// share the same configuration and residency slot.
#[derive(Clone)]
pub struct Dictation {
    inner: Arc<DictationInner>,
}

/// Unique output-file basenames even under concurrent transcriptions.
static TRANSCRIBE_SEQ: AtomicU64 = AtomicU64::new(0);

impl Dictation {
    pub fn new(config: WhisperConfig, governor: ModelManager) -> Self {
        Self {
            inner: Arc::new(DictationInner { config, governor }),
        }
    }

    pub fn info(&self) -> DictationInfo {
        DictationInfo {
            runtime: file_stem(&self.inner.config.cli_bin),
            model: file_stem(&self.inner.config.model_path),
        }
    }

    /// The residency policy this dictation runs under (shared with the
    /// language model when both were bootstrapped together).
    pub fn governor(&self) -> ModelManager {
        self.inner.governor.clone()
    }

    /// Transcribe one WAV file to raw text. Blocking; `cancel` kills the
    /// runtime mid-transcription. An empty result means the runtime heard
    /// no speech — callers decide what to do with that.
    pub fn transcribe(&self, wav: &Path, cancel: &AtomicBool) -> Result<String> {
        if !wav.is_file() {
            return Err(anyhow!("audio file not found: {}", wav.display()));
        }
        self.inner.governor.claim(crate::HeavyModel::Whisper)?;
        let result = self.inner.run(wav, cancel);
        self.inner.governor.release(crate::HeavyModel::Whisper);
        result
    }
}

impl DictationInner {
    fn run(&self, wav: &Path, cancel: &AtomicBool) -> Result<String> {
        let out_txt = std::env::temp_dir().join(format!(
            "oca-transcribe-{}-{}.txt",
            std::process::id(),
            TRANSCRIBE_SEQ.fetch_add(1, Ordering::Relaxed)
        ));

        let args: Vec<String> = match &self.config.cli_args {
            Some(args) => args.clone(),
            None => {
                let mut args = vec![
                    "-m".into(),
                    self.config.model_path.to_string_lossy().into_owned(),
                    "-f".into(),
                    wav.to_string_lossy().into_owned(),
                    // Plain text, no timestamps, no chatter: stdout/logs
                    // stay out of the transcript, which comes from the
                    // output file below.
                    "-nt".into(),
                    "-np".into(),
                    "-otxt".into(),
                    "-of".into(),
                    out_txt.with_extension("").to_string_lossy().into_owned(),
                ];
                if let Some(language) = &self.config.language {
                    args.push("-l".into());
                    args.push(language.clone());
                }
                if self.config.threads > 0 {
                    args.push("-t".into());
                    args.push(self.config.threads.to_string());
                }
                args
            }
        };

        let mut cmd = Command::new(&self.config.cli_bin);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(transcribe_log())
            .stderr(transcribe_log());
        for (key, value) in &self.config.cli_env {
            cmd.env(key, value);
        }
        // The transcript contract, honored by the real runtime via `-of`
        // above and by test doubles via this variable: the transcript is
        // written to exactly this file.
        cmd.env("OCA_TRANSCRIBE_OUT", &out_txt);

        let mut child = cmd.spawn().with_context(|| {
            format!(
                "start transcription runtime ({})",
                self.config.cli_bin.display()
            )
        })?;

        let status = match wait_child(&mut child, cancel) {
            Ok(status) => status,
            Err(err) => {
                // Cancelled (or the wait itself failed): kill the runtime,
                // reap it, and never leave the temp output behind.
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&out_txt);
                return Err(err);
            }
        };
        if !status.success() {
            let _ = std::fs::remove_file(&out_txt);
            return Err(anyhow!(
                "transcription runtime exited with {status} — see {}",
                transcribe_log_path().display()
            ));
        }

        let text = std::fs::read_to_string(&out_txt).with_context(|| {
            format!(
                "transcription runtime produced no output — see {}",
                transcribe_log_path().display()
            )
        });
        let _ = std::fs::remove_file(&out_txt);
        Ok(text?.trim().to_string())
    }
}

/// Poll the child until it exits or the token is set (kill → cancelled).
fn wait_child(child: &mut Child, cancel: &AtomicBool) -> Result<std::process::ExitStatus> {
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(anyhow!("transcription cancelled"));
        }
        match child.try_wait().context("wait for transcription runtime")? {
            Some(status) => return Ok(status),
            None => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "runtime".into())
}

fn transcribe_log_path() -> PathBuf {
    default_oca_dir().join("logs").join("whisper.log")
}

/// The runtime's stdout/stderr go to `~/.oca/logs/whisper.log` — the TUI
/// and CLI stderr stay silent either way (the doctor/TUI quiet contract),
/// and the content gate (A7) holds: the transcript itself never lands in
/// a log.
fn transcribe_log() -> Stdio {
    let path = transcribe_log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(file) => Stdio::from(file),
        Err(_) => Stdio::null(),
    }
}

/// Find `whisper-cli`: installed runtime packs first, then `PATH`.
pub fn find_whisper_cli(runtime_dir: &Path) -> Option<PathBuf> {
    crate::find_executable_in(
        runtime_dir,
        std::env::var_os("PATH").as_deref(),
        "whisper-cli",
    )
}

/// The search with an explicit `PATH` (`None` = no `PATH` at all) — `doctor`
/// and tests pass their own so nothing touches process-global state.
pub fn find_whisper_cli_in(
    runtime_dir: &Path,
    path_var: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    crate::find_executable_in(runtime_dir, path_var, "whisper-cli")
}

/// Models live outside the project: `~/.oca/models/whisper`. `.gguf` and
/// legacy `.bin` whisper models both count; largest first.
pub fn discover_whisper_models(dir: &Path) -> Result<Vec<DiscoveredModel>> {
    crate::discover_model_files(dir, &["gguf", "bin"])
}

/// Whisper models live outside the project: `~/.oca/models/whisper`.
pub fn default_whisper_models_dir() -> PathBuf {
    default_oca_dir().join("models").join("whisper")
}
