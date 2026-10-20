//! [`Embedder`]: the embedding-model seam (Phase 11), the same rule-6
//! posture as [`crate::LanguageModel`] — only this crate talks to the
//! runtime, tests only ever see [`crate::MockEmbedder`].
//!
//! The real implementation spawns `llama-server` with a separate embedding
//! model (`~/.oca/models/embeddings/*.gguf`) and posts batches to its
//! OpenAI-compatible `/v1/embeddings`. It claims the shared residency
//! budget as [`crate::HeavyModel::Embeddings`] (A6), so an embedding
//! session and a language model never fight for RAM — swapping is
//! explicit, exactly like everywhere else.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use serde_json::{Value, json};

use crate::subprocess::{Endpoint, free_port, health_ok, log_path, random_key, runtime_log};
use crate::{HeavyModel, ModelManager};

/// Static description for `doctor`/the AI panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbedderInfo {
    /// File stem of the model — what the UI shows.
    pub name: String,
    /// The runtime process behind it.
    pub runtime: String,
}

/// Everything needed to spawn the embedding runtime (all optional except
/// the model and the server binary).
#[derive(Clone, Debug)]
pub struct EmbedderConfig {
    pub model_path: PathBuf,
    /// Token pool the runtime gets (`-c`). Embedding models are typically
    /// short-context; 4096 is a safe default.
    pub context: u32,
    pub gpu_layers: u32,
    pub threads: u32,
    pub server_bin: PathBuf,
    pub port: Option<u16>,
    /// Replace the standard server arguments entirely — test doubles and
    /// nonstandard runtimes, mirroring [`crate::ModelConfig::server_args`].
    pub server_args: Option<Vec<String>>,
    /// Extra child-process environment (`(key, value)` pairs).
    pub server_env: Vec<(String, String)>,
    pub startup_timeout: Duration,
}

impl EmbedderConfig {
    pub fn new(model_path: impl Into<PathBuf>, server_bin: impl Into<PathBuf>) -> Self {
        Self {
            model_path: model_path.into(),
            context: 4096,
            gpu_layers: 0,
            threads: 0,
            server_bin: server_bin.into(),
            port: None,
            server_args: None,
            server_env: Vec::new(),
            startup_timeout: Duration::from_secs(30),
        }
    }
}

/// The embedding touchpoint (rule 6, one runtime gateway). Everything is
/// `&self` with interior state so the governor can swap residency from
/// another thread between calls.
pub trait Embedder: Send + Sync {
    fn info(&self) -> EmbedderInfo;
    fn is_loaded(&self) -> bool;
    /// Start (or attach to) the runtime. Idempotent; claims the residency
    /// budget as [`HeavyModel::Embeddings`].
    fn load(&self) -> Result<()>;
    /// Embed a batch of texts. One vector per input, same order, same
    /// dimension — callers normalize for indexing (that's index policy,
    /// not model output).
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
    /// The vector dimension, when the model knows it without being asked
    /// (mocks do; a llama-server runtime reveals it on first embed). Used
    /// to invalidate stale vectors when a different model takes over.
    fn dim(&self) -> Option<usize> {
        None
    }
    /// Kill the runtime and free the residency slot.
    fn unload(&self) -> Result<()>;
}

/// `llama-server` with an embedding GGUF, loaded until unloaded.
pub struct LlamaCppEmbedder {
    config: EmbedderConfig,
    child: Mutex<Option<Child>>,
    endpoint: Mutex<Option<Endpoint>>,
    governor: ModelManager,
}

impl LlamaCppEmbedder {
    /// Build a handle. Nothing is spawned until [`Embedder::load`].
    pub fn new(config: EmbedderConfig, governor: ModelManager) -> Self {
        Self {
            config,
            child: Mutex::new(None),
            endpoint: Mutex::new(None),
            governor,
        }
    }

    /// The runtime process, when it is alive (for diagnostics/tests).
    pub fn server_pid(&self) -> Option<u32> {
        self.child
            .lock()
            .expect("child lock")
            .as_ref()
            .map(|c| c.id())
    }
}

impl Embedder for LlamaCppEmbedder {
    fn info(&self) -> EmbedderInfo {
        EmbedderInfo {
            name: file_stem(&self.config.model_path),
            runtime: self
                .config
                .server_bin
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "llama-server".into()),
        }
    }

    fn is_loaded(&self) -> bool {
        let mut guard = self.child.lock().expect("child lock");
        let Some(child) = guard.as_mut() else {
            return false;
        };
        let gone = !matches!(child.try_wait(), Ok(None));
        if gone {
            // Reap the exit quietly; the next load() starts fresh.
            *guard = None;
            *self.endpoint.lock().expect("endpoint lock") = None;
            self.governor.release(HeavyModel::Embeddings);
            false
        } else {
            true
        }
    }

    fn load(&self) -> Result<()> {
        if self.is_loaded() {
            return Ok(());
        }
        self.governor.claim(HeavyModel::Embeddings)?;
        let result = self.spawn();
        if result.is_err() {
            self.governor.release(HeavyModel::Embeddings);
        }
        result
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if !self.is_loaded() {
            return Err(anyhow!(
                "embedding runtime is not loaded — load it first (or run oca reindex)"
            ));
        }
        let endpoint = self
            .endpoint
            .lock()
            .expect("endpoint lock")
            .clone()
            .ok_or_else(|| anyhow!("embedding runtime is not loaded — load it first"))?;
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let body = json!({
            "input": texts,
            "model": file_stem(&self.config.model_path),
        });
        match post_embeddings(endpoint.port, &endpoint.api_key, &body) {
            Ok(vectors) => Ok(vectors),
            Err(err) => {
                // Distinguish "the runtime died" from "the request hiccuped".
                if !self.is_loaded() {
                    Err(anyhow!("embedding runtime exited unexpectedly ({err:#})"))
                } else {
                    Err(err)
                }
            }
        }
    }

    fn unload(&self) -> Result<()> {
        let child = self.child.lock().expect("child lock").take();
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait(); // reap — never leave a zombie
        }
        *self.endpoint.lock().expect("endpoint lock") = None;
        self.governor.release(HeavyModel::Embeddings);
        Ok(())
    }
}

impl LlamaCppEmbedder {
    fn spawn(&self) -> Result<()> {
        let port = match self.config.port {
            Some(port) => port,
            None => free_port()?,
        };
        let api_key = random_key();
        let args: Vec<String> = match &self.config.server_args {
            Some(args) => args.clone(),
            None => {
                let mut args = vec![
                    "-m".into(),
                    self.config.model_path.to_string_lossy().into_owned(),
                    "--host".into(),
                    "127.0.0.1".into(),
                    "--port".into(),
                    port.to_string(),
                    "-c".into(),
                    self.config.context.to_string(),
                    "-ngl".into(),
                    self.config.gpu_layers.to_string(),
                ];
                if self.config.threads > 0 {
                    args.push("-t".into());
                    args.push(self.config.threads.to_string());
                }
                args.push("--api-key".into());
                args.push(api_key.clone());
                args
            }
        };
        let mut cmd = Command::new(&self.config.server_bin);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(runtime_log())
            .stderr(runtime_log());
        for (key, value) in &self.config.server_env {
            cmd.env(key, value);
        }
        let child = cmd.spawn().with_context(|| {
            format!(
                "start embedding runtime ({})",
                self.config.server_bin.display()
            )
        })?;
        *self.child.lock().expect("child lock") = Some(child);
        *self.endpoint.lock().expect("endpoint lock") = Some(Endpoint { port, api_key });
        if let Err(err) = self.wait_ready() {
            let _ = self.unload();
            return Err(err);
        }
        tracing::info!(
            model = %file_stem(&self.config.model_path),
            port,
            "embedding runtime ready"
        );
        Ok(())
    }

    /// Poll `/health` until ready, the process dies, or the budget runs out.
    fn wait_ready(&self) -> Result<()> {
        let endpoint = self
            .endpoint
            .lock()
            .expect("endpoint lock")
            .clone()
            .ok_or_else(|| anyhow!("no endpoint"))?;
        let deadline = Instant::now() + self.config.startup_timeout;
        loop {
            if let Some(child) = self.child.lock().expect("child lock").as_mut()
                && let Ok(Some(status)) = child.try_wait()
            {
                return Err(anyhow!(
                    "embedding runtime exited immediately ({status}) — see {}",
                    log_path().display()
                ));
            }
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "embedding runtime did not become ready within {}s — see {}",
                    self.config.startup_timeout.as_secs(),
                    log_path().display()
                ));
            }
            if health_ok(endpoint.port) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    }
}

/// One non-streaming `POST /v1/embeddings` (the OpenAI-compatible batch
/// endpoint). Returns one vector per input, ordered by the response index.
pub(crate) fn post_embeddings(port: u16, api_key: &str, body: &Value) -> Result<Vec<Vec<f32>>> {
    use std::io::{BufReader, Read as _, Write as _};

    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))
        .context("connect to embedding runtime")?;
    stream.set_nodelay(true).ok();
    let mut reader = BufReader::new(
        stream
            .try_clone()
            .context("clone embedding runtime socket")?,
    );
    let payload = body.to_string();
    let request = format!(
        "POST /v1/embeddings HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
         Authorization: Bearer {api_key}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream
        .write_all(request.as_bytes())
        .context("send request to embedding runtime")?;

    let mut response = String::new();
    reader
        .read_to_string(&mut response)
        .context("read embedding runtime response")?;
    let (head, body) = response
        .split_once("\r\n\r\n")
        .context("embedding runtime sent a malformed response")?;
    let status = head.lines().next().unwrap_or_default();
    if !status.starts_with("HTTP/1.1 200") && !status.starts_with("HTTP/1.0 200") {
        return Err(anyhow!(
            "embedding runtime refused the request ({status}) {}",
            body.trim().chars().take(200).collect::<String>()
        ));
    }
    let value: Value = serde_json::from_str(body).context("embedding runtime sent invalid JSON")?;
    if let Some(err) = value.get("error") {
        return Err(anyhow!("embedding runtime error: {err}"));
    }
    let Some(data) = value["data"].as_array() else {
        return Err(anyhow!("embedding runtime response carries no data"));
    };
    let mut ordered: Vec<(u64, Vec<f32>)> = Vec::with_capacity(data.len());
    for entry in data {
        let index = entry["index"].as_u64().unwrap_or(0);
        let vector: Vec<f32> = entry["embedding"]
            .as_array()
            .context("embedding entry carries no vector")?
            .iter()
            .map(|v| {
                v.as_f64()
                    .map(|f| f as f32)
                    .context("embedding vector holds a non-number")
            })
            .collect::<Result<_>>()?;
        ordered.push((index, vector));
    }
    ordered.sort_by_key(|(index, _)| *index);
    Ok(ordered.into_iter().map(|(_, v)| v).collect())
}

/// Dimension the deterministic mock vectors use ([`crate::MockEmbedder`]
/// and the test runtime double answer with this many numbers).
pub const MOCK_EMBED_DIM: usize = 32;

/// Deterministic bag-of-tokens embedding shared by [`crate::MockEmbedder`]
/// and the test runtime double: each token flips one dimension of a
/// signed count vector, L2-normalized. Texts that share vocabulary land
/// close together — enough for honest ranking tests without a model.
pub fn deterministic_embedding(text: &str, dim: usize) -> Vec<f32> {
    let dim = dim.max(2);
    let mut vector = vec![0f32; dim];
    for token in text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
    {
        let mut hash = 0xcbf2_9ce4_8422_2325u64; // FNV-1a
        for byte in token.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        // Sign by a second hash so antonyms don't accidentally align.
        let sign = if hash & 1 == 0 { 1.0 } else { -1.0 };
        vector[(hash as usize) % dim] += sign;
    }
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm == 0.0 {
        vector[0] = 1.0;
        return vector;
    }
    for v in &mut vector {
        *v /= norm;
    }
    vector
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "model".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_embedding_is_stable_normalized_and_orderless() {
        let a = deterministic_embedding("the elysian gates opened", 32);
        let b = deterministic_embedding("the elysian gates opened", 32);
        assert_eq!(a, b, "same text, same vector — every time");
        let norm = a.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "unit length, got {norm}");
        // Shared vocabulary scores closer than none at all.
        let related = deterministic_embedding("elysan gates of the void", 32);
        let alien = deterministic_embedding("completely unrelated zephyr", 32);
        let dot = |x: &[f32], y: &[f32]| x.iter().zip(y).map(|(a, b)| a * b).sum::<f32>();
        assert!(dot(&a, &related) > dot(&a, &alien));
    }

    #[test]
    fn deterministic_embedding_survives_empty_text() {
        let v = deterministic_embedding("", 8);
        assert_eq!(v.len(), 8);
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }
}
