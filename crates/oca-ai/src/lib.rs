//! The AI runtime behind one abstraction (AGENTS rule 6): only
//! [`LanguageModel`] implementations talk to llama.cpp, tests run against
//! [`MockModel`], and deleting every model file leaves the app fully
//! functional (rule 3 — AI optional).
//!
//! - [`LlamaCppSubprocess`] spawns `llama-server` on loopback with a random
//!   per-session API key (ADR-0007), streams tokens over its OpenAI-compatible
//!   HTTP API, and treats cancel as socket-abort + process kill. A crashed
//!   runtime is a clean error, never a crash of OCA.
//! - [`ModelManager`] enforces the one-heavy-model governor (ADR-0006):
//!   LLM / embeddings / whisper share the machine's appetite; swaps are
//!   explicit.
//! - Runtime packs (optional downloads with SHA-256 manifests, side-loaded
//!   offline) install via [`install_pack`]; [`find_llama_server`] looks in the
//!   runtime dir first, then `PATH`.
//! - [`Ai`] is the thin facade frontends hold (rule 8 — TUI/CLI are clients;
//!   all logic stays here).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};

mod hardware;
mod mock;
mod packs;
mod subprocess;

pub use hardware::{Hardware, Tier};
pub use mock::MockModel;
#[doc(hidden)]
pub use packs::pack_writer;
pub use packs::{
    InstallOutcome, InstalledPack, PackManifest, install_pack, list_packs, verify_pack,
};
pub use subprocess::LlamaCppSubprocess;
#[doc(hidden)]
pub use subprocess::fake_server;

/// How the runtime describes itself to the UI (`doctor`, the AI panel).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    /// Display name — the model file's stem, or "mock".
    pub name: String,
    /// Context window, in tokens.
    pub context: u32,
    /// Layers offloaded to the GPU (0 = CPU).
    pub gpu_layers: u32,
    /// Worker threads (0 = runtime's choice).
    pub threads: u32,
    /// Backend description, e.g. "llama-server".
    pub runtime: String,
}

/// Sampling + runtime configuration for one model. Fixed per instance —
/// build a new model to change it.
#[derive(Clone, Debug)]
pub struct ModelConfig {
    /// The GGUF file to serve.
    pub model_path: PathBuf,
    /// Context window passed to the runtime.
    pub context: u32,
    pub temperature: f32,
    pub top_p: f32,
    /// Default generation budget when a request doesn't override it.
    pub max_tokens: u32,
    /// Worker threads (0 = runtime's choice).
    pub threads: u32,
    /// Layers offloaded to the GPU (0 = CPU).
    pub gpu_layers: u32,
    /// The `llama-server` executable (runtime pack, then `PATH`).
    pub server_bin: PathBuf,
    /// Fixed server port (`None` = ephemeral, the default). Mostly for
    /// tests and locked-down networks.
    pub port: Option<u16>,
    /// Replace the standard runtime arguments entirely — for test doubles
    /// and nonstandard runtimes. `None` = the standard `llama-server`
    /// flags (`-m`, `--host`, `--port`, `-c`, `-ngl`, `-t`, `--api-key`).
    pub server_args: Option<Vec<String>>,
    /// Extra environment for the runtime process (GPU selection,
    /// test doubles). Applied only to the spawned child.
    pub server_env: Vec<(String, String)>,
    /// How long `load` waits for the runtime to become healthy.
    pub startup_timeout: std::time::Duration,
}

impl ModelConfig {
    pub fn new(model_path: impl Into<PathBuf>, server_bin: impl Into<PathBuf>) -> Self {
        Self {
            model_path: model_path.into(),
            context: 8192,
            temperature: 0.7,
            top_p: 0.9,
            max_tokens: 512,
            threads: 0,
            gpu_layers: 0,
            server_bin: server_bin.into(),
            port: None,
            server_args: None,
            server_env: Vec::new(),
            startup_timeout: std::time::Duration::from_secs(30),
        }
    }
}

/// One generation request. `schema` is the constrained-decoding hook
/// (ADR-0008): when present it is passed through as the runtime's JSON-schema
/// response format; free-text requests skip it.
#[derive(Clone, Debug, Default)]
pub struct GenerationRequest {
    pub prompt: String,
    pub system: Option<String>,
    pub schema: Option<serde_json::Value>,
    /// Overrides [`ModelConfig::max_tokens`] for this request.
    pub max_tokens: Option<u32>,
}

/// What a generation produced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GenerationOutcome {
    pub text: String,
    /// Tokens observed (streamed chunks that carried content).
    pub tokens: u32,
    /// True when `cancel` stopped it — not an error, not a completion.
    pub cancelled: bool,
}

/// The single llama.cpp touchpoint (AGENTS rule 6). Everything is `&self` —
/// implementations carry interior state so cancel can arrive from another
/// thread while a generation streams.
pub trait LanguageModel: Send + Sync {
    /// Static description for `doctor`/the AI panel.
    fn info(&self) -> ModelInfo;
    fn is_loaded(&self) -> bool;
    /// Start (or attach to) the runtime. Idempotent.
    fn load(&self) -> Result<()>;
    /// Stream tokens into `sink` until completion, error, or [`Self::cancel`].
    fn generate(
        &self,
        request: &GenerationRequest,
        sink: &mut dyn FnMut(&str),
    ) -> Result<GenerationOutcome>;
    /// Interrupt the in-flight generation from any thread. No-op when idle.
    fn cancel(&self);
    /// Stop the runtime and release its memory. Idempotent.
    fn unload(&self) -> Result<()>;
}

/// The heavy-model kinds sharing one residency budget (ADR-0006). Phase 7
/// uses `Llm`; embeddings (Phase 11) and whisper (Phase 7B) claim the same
/// slot when they arrive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeavyModel {
    Llm,
    Embeddings,
    Whisper,
}

impl HeavyModel {
    fn label(self) -> &'static str {
        match self {
            HeavyModel::Llm => "language",
            HeavyModel::Embeddings => "embedding",
            HeavyModel::Whisper => "transcription",
        }
    }
}

/// The one-heavy-model governor (ADR-0006): by default only one heavy model
/// is resident; swapping kinds is explicit — the user unloads one and loads
/// another, never silently.
#[derive(Debug)]
pub struct ModelManager {
    resident: Mutex<Option<HeavyModel>>,
    capacity: usize,
}

impl Default for ModelManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelManager {
    /// Governor default: one heavy model resident at a time.
    pub fn new() -> Self {
        Self {
            resident: Mutex::new(None),
            capacity: 1,
        }
    }

    /// Spare-RAM machines may raise this via config (still explicit).
    pub fn with_capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity.max(1);
        self
    }

    /// Reserve the residency slot for `kind`. Errors when another kind is
    /// resident and the budget is full — the caller unloads first.
    pub fn claim(&self, kind: HeavyModel) -> Result<()> {
        let mut resident = self.resident.lock().expect("manager lock");
        match *resident {
            None => {
                *resident = Some(kind);
                Ok(())
            }
            Some(current) if current == kind => Ok(()),
            Some(current) if self.capacity > 1 => {
                // Spare capacity: allow the second kind alongside.
                *resident = Some(kind);
                tracing::info!(kind = ?kind, resident = ?current, "additional heavy model resident");
                Ok(())
            }
            Some(current) => Err(anyhow!(
                "a {} model is already resident — one heavy model at a time; unload it first",
                current.label()
            )),
        }
    }

    /// Free the slot (only if it is ours).
    pub fn release(&self, kind: HeavyModel) {
        let mut resident = self.resident.lock().expect("manager lock");
        if *resident == Some(kind) {
            *resident = None;
        }
    }

    pub fn resident(&self) -> Option<HeavyModel> {
        *self.resident.lock().expect("manager lock")
    }

    /// Human-readable policy for `doctor` and config dumps.
    pub fn policy(&self) -> String {
        if self.capacity > 1 {
            format!("up to {} heavy models resident", self.capacity)
        } else {
            "one heavy model at a time".to_string()
        }
    }
}

/// A GGUF file found on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredModel {
    pub path: PathBuf,
    /// File stem — what the UI shows.
    pub name: String,
    pub bytes: u64,
}

/// Scan `dir` for `*.gguf`, largest first (the usual "best model here"
/// heuristic). A missing directory is not an error — it just means "none".
pub fn discover_models(dir: &Path) -> Result<Vec<DiscoveredModel>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("read models directory {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let is_gguf = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("gguf"));
        if !is_gguf || !path.is_file() {
            continue;
        }
        let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        out.push(DiscoveredModel { path, name, bytes });
    }
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

/// The user's OCA home: `~/.oca` (models, runtime packs, logs live here —
/// never inside a project).
pub fn default_oca_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|h| !h.is_empty()));
    match home {
        Some(h) => PathBuf::from(h).join(".oca"),
        None => PathBuf::from(".oca"),
    }
}

/// Models live outside the project: `~/.oca/models/llm`.
pub fn default_models_dir() -> PathBuf {
    default_oca_dir().join("models").join("llm")
}

/// Side-loaded runtime packs live here: `~/.oca/runtime`.
pub fn default_runtime_dir() -> PathBuf {
    default_oca_dir().join("runtime")
}

/// Find `llama-server`: installed runtime packs first, then `PATH`. Returns
/// the first candidate that exists — never spawns anything.
pub fn find_llama_server(runtime_dir: &Path) -> Option<PathBuf> {
    find_server_in(runtime_dir, std::env::var_os("PATH").as_deref())
}

/// The search with an explicit `PATH` (`None` = search no `PATH` at all) —
/// tests pass their own so nothing touches process-global state.
pub fn find_server_in(runtime_dir: &Path, path_var: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    };
    if let Ok(packs) = std::fs::read_dir(runtime_dir) {
        let mut dirs: Vec<PathBuf> = packs.flatten().map(|e| e.path()).collect();
        dirs.sort();
        for dir in dirs {
            let candidate = dir.join("bin").join(exe);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    for dir in std::env::split_paths(path_var?) {
        let candidate = dir.join(exe);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Overrides the config may supply for the runtime (all optional).
#[derive(Clone, Debug, Default)]
pub struct BootstrapOptions {
    pub models_dir: Option<PathBuf>,
    pub runtime_dir: Option<PathBuf>,
    /// Explicit server path — beats discovery.
    pub llama_server: Option<PathBuf>,
    pub context: Option<u32>,
    pub gpu_layers: Option<u32>,
    pub threads: Option<u32>,
}

/// Build the AI facade if a runtime *and* a model are present; `None` means
/// "no AI available" — every feature keeps working without it (rule 3).
pub fn bootstrap(opts: &BootstrapOptions) -> Result<Option<Ai>> {
    let runtime_dir = opts.runtime_dir.clone().unwrap_or_else(default_runtime_dir);
    let server = match &opts.llama_server {
        Some(explicit) if explicit.is_file() => Some(explicit.clone()),
        Some(explicit) => {
            tracing::warn!(path = %explicit.display(), "configured llama-server not found");
            None
        }
        None => find_llama_server(&runtime_dir),
    };
    let Some(server_bin) = server else {
        return Ok(None);
    };
    let models_dir = opts.models_dir.clone().unwrap_or_else(default_models_dir);
    let models = discover_models(&models_dir)?;
    let Some(chosen) = models.first().cloned() else {
        return Ok(None);
    };
    let mut config = ModelConfig::new(chosen.path.clone(), server_bin);
    if let Some(context) = opts.context {
        config.context = context;
    }
    if let Some(gpu) = opts.gpu_layers {
        config.gpu_layers = gpu;
    }
    if let Some(threads) = opts.threads {
        config.threads = threads;
    }
    let model = LlamaCppSubprocess::new(config);
    Ok(Some(Ai::new(Arc::new(model), models)))
}

/// A fixed prompt the AI panel uses to prove the pipeline works end-to-end
/// (Phase 7 has no AI features yet — the context engine arrives in Phase 8).
pub fn probe_request() -> GenerationRequest {
    GenerationRequest {
        prompt: "Write exactly one short sentence about a door.".into(),
        system: Some("You are a concise writing companion.".into()),
        schema: None,
        max_tokens: Some(64),
    }
}

/// What frontends hold: the model plus the governor, behind `Arc` so the
/// streaming job thread and the panel share one instance (rule 8 — the TUI
/// keeps zero runtime logic).
#[derive(Clone)]
pub struct Ai {
    inner: Arc<AiInner>,
}

struct AiInner {
    model: Arc<dyn LanguageModel>,
    manager: ModelManager,
    models: Vec<DiscoveredModel>,
}

impl Ai {
    pub fn new(model: Arc<dyn LanguageModel>, models: Vec<DiscoveredModel>) -> Self {
        Self {
            inner: Arc::new(AiInner {
                model,
                manager: ModelManager::new(),
                models,
            }),
        }
    }

    /// GGUF files on disk, largest first (for the picker).
    pub fn models(&self) -> &[DiscoveredModel] {
        &self.inner.models
    }

    pub fn info(&self) -> ModelInfo {
        self.inner.model.info()
    }

    pub fn is_loaded(&self) -> bool {
        self.inner.model.is_loaded()
    }

    /// Load through the governor: another heavy model resident → explicit
    /// error telling the user to unload it first.
    pub fn load(&self) -> Result<()> {
        self.inner.manager.claim(HeavyModel::Llm)?;
        if let Err(err) = self.inner.model.load() {
            self.inner.manager.release(HeavyModel::Llm);
            return Err(err);
        }
        Ok(())
    }

    pub fn unload(&self) -> Result<()> {
        self.inner.model.unload()?;
        self.inner.manager.release(HeavyModel::Llm);
        Ok(())
    }

    /// Interrupt whatever is streaming. Any thread, any time.
    pub fn cancel(&self) {
        self.inner.model.cancel();
    }

    /// Blocking streaming helper: each token is sent on `tx` as it arrives.
    /// Callers wrap this in a job thread (Core's job system) and drain the
    /// receiver on their tick — the streaming itself lives in the model.
    pub fn stream(
        &self,
        request: GenerationRequest,
        tx: std::sync::mpsc::Sender<String>,
    ) -> Result<GenerationOutcome> {
        self.inner.model.generate(&request, &mut |token| {
            let _ = tx.send(token.to_string());
        })
    }

    /// The residency policy, for `doctor` and the panel.
    pub fn policy(&self) -> String {
        self.inner.manager.policy()
    }
}

/// One-line size formatting for `doctor`/the panel ("1.4 GB", "812 MB").
pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= KB && unit + 1 < units.len() {
        value /= KB;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", units[unit])
    } else {
        format!("{value:.1} {}", units[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn governor_enforces_one_heavy_model() {
        let m = ModelManager::new();
        assert_eq!(m.policy(), "one heavy model at a time");
        m.claim(HeavyModel::Llm).unwrap();
        assert_eq!(m.resident(), Some(HeavyModel::Llm));
        // Same kind again: fine (idempotent).
        m.claim(HeavyModel::Llm).unwrap();
        // Different kind: explicit error, not a silent swap.
        let err = m.claim(HeavyModel::Embeddings).unwrap_err().to_string();
        assert!(err.contains("unload it first"), "{err}");
        m.release(HeavyModel::Llm);
        assert_eq!(m.resident(), None);
        m.claim(HeavyModel::Embeddings).unwrap();
        // Releasing the wrong kind clears nothing.
        m.release(HeavyModel::Whisper);
        assert_eq!(m.resident(), Some(HeavyModel::Embeddings));
    }

    #[test]
    fn governor_allows_more_with_capacity() {
        let m = ModelManager::new().with_capacity(2);
        m.claim(HeavyModel::Llm).unwrap();
        m.claim(HeavyModel::Embeddings).unwrap();
        assert!(m.policy().contains('2'));
    }

    #[test]
    fn discovery_orders_by_size_and_tolerates_missing_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(
            discover_models(&tmp.path().join("nope"))
                .unwrap()
                .is_empty()
        );

        std::fs::write(tmp.path().join("small.gguf"), vec![0u8; 10]).unwrap();
        std::fs::write(tmp.path().join("big.gguf"), vec![0u8; 99]).unwrap();
        std::fs::write(tmp.path().join("notes.md"), b"not a model").unwrap();
        let found = discover_models(tmp.path()).unwrap();
        assert_eq!(
            found.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            vec!["big", "small"]
        );
        assert_eq!(found[0].bytes, 99);
    }

    #[test]
    fn find_server_prefers_packs_then_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let runtime = tmp.path().join("runtime");
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = if cfg!(windows) {
            "llama-server.exe"
        } else {
            "llama-server"
        };
        std::fs::write(bin.join(exe), b"#!/bin/sh\n").unwrap();

        // A PATH containing ours finds it.
        let path_var = std::ffi::OsString::from(bin.as_os_str());
        assert_eq!(
            find_server_in(&runtime, Some(&path_var)),
            Some(bin.join(exe))
        );

        // Pack hit wins over PATH.
        let pack = runtime.join("llama-pack-1").join("bin");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(pack.join(exe), b"packed\n").unwrap();
        assert_eq!(
            find_server_in(&runtime, Some(&path_var)),
            Some(pack.join(exe))
        );

        // No PATH: only packs count.
        assert_eq!(find_server_in(&runtime, None), Some(pack.join(exe)));
    }

    #[test]
    fn human_sizes_read_well() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn mock_streams_and_counts_calls() {
        let model = Arc::new(
            MockModel::new()
                .with_tokens(vec!["Hel".into(), "lo".into()])
                .with_delay(0),
        );
        let ai = Ai::new(model.clone(), Vec::new());
        assert!(!ai.is_loaded());
        ai.load().unwrap();
        assert!(ai.is_loaded());
        ai.load().unwrap(); // idempotent

        let (tx, rx) = std::sync::mpsc::channel();
        let out = ai
            .stream(
                GenerationRequest {
                    prompt: "hi".into(),
                    ..GenerationRequest::default()
                },
                tx,
            )
            .unwrap();
        assert_eq!(out.text, "Hello");
        assert_eq!(out.tokens, 2);
        assert!(!out.cancelled);
        assert_eq!(rx.iter().collect::<Vec<_>>().join(""), "Hello");

        ai.unload().unwrap();
        assert!(!ai.is_loaded());
        let counters = model.counters();
        assert_eq!(
            (counters.load, counters.unload, counters.generate),
            (2, 1, 1)
        );
    }

    #[test]
    fn mock_cancel_interrupts_and_mock_failure_is_a_clean_error() {
        let model = std::sync::Arc::new(MockModel::new().with_delay(50));
        model.load().unwrap();
        let worker = std::sync::Arc::clone(&model);
        let h = std::thread::spawn(move || {
            let mut sink = |_: &str| {};
            worker.generate(&GenerationRequest::default(), &mut sink)
        });
        std::thread::sleep(std::time::Duration::from_millis(20));
        model.cancel();
        let out = h.join().unwrap().unwrap();
        assert!(out.cancelled, "{out:?}");

        let failing = MockModel::new().fails_generate("model runtime crashed (simulated)");
        failing.load().unwrap();
        let mut sink = |_: &str| {};
        let err = failing
            .generate(&GenerationRequest::default(), &mut sink)
            .unwrap_err()
            .to_string();
        assert!(err.contains("crashed"), "{err}");
    }
}
