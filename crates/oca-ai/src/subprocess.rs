//! [`LlamaCppSubprocess`]: the first real [`LanguageModel`].
//!
//! Spawns `llama-server` bound to loopback on an ephemeral port with a
//! random per-session API key (ADR-0007), waits for `/health`, streams
//! tokens from its OpenAI-compatible API, and treats cancel as
//! socket-abort (interrupt the stream) or unload (kill the process).
//! A crashed runtime surfaces as a clean error — OCA never goes down
//! with it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

use crate::{
    GenerationOutcome, GenerationRequest, LanguageModel, ModelConfig, ModelInfo, default_oca_dir,
};

pub struct LlamaCppSubprocess {
    config: ModelConfig,
    child: Mutex<Option<Child>>,
    endpoint: Mutex<Option<Endpoint>>,
    cancel: AtomicBool,
    active_stream: Mutex<Option<Arc<TcpStream>>>,
}

#[derive(Clone)]
struct Endpoint {
    port: u16,
    api_key: String,
}

impl LlamaCppSubprocess {
    /// Build a model handle. Nothing is spawned until [`LanguageModel::load`].
    pub fn new(config: ModelConfig) -> Self {
        Self {
            config,
            child: Mutex::new(None),
            endpoint: Mutex::new(None),
            cancel: AtomicBool::new(false),
            active_stream: Mutex::new(None),
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

    fn chat_body(&self, request: &GenerationRequest) -> Value {
        let mut messages = Vec::new();
        if let Some(system) = &request.system {
            messages.push(json!({"role": "system", "content": system}));
        }
        messages.push(json!({"role": "user", "content": request.prompt}));
        let mut body = json!({
            "messages": messages,
            "stream": true,
            "temperature": self.config.temperature,
            "top_p": self.config.top_p,
            "max_tokens": request.max_tokens.unwrap_or(self.config.max_tokens),
        });
        if let Some(schema) = &request.schema {
            // Constrained-decoding hook (ADR-0008): pass the schema through
            // as the runtime's JSON-schema response format.
            body["response_format"] = json!({"type": "json_schema", "json_schema": schema});
        }
        body
    }
}

impl LanguageModel for LlamaCppSubprocess {
    fn info(&self) -> ModelInfo {
        ModelInfo {
            name: file_stem(&self.config.model_path),
            context: self.config.context,
            gpu_layers: self.config.gpu_layers,
            threads: self.config.threads,
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
            false
        } else {
            true
        }
    }

    fn load(&self) -> Result<()> {
        if self.is_loaded() {
            return Ok(());
        }
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
            format!("start model runtime ({})", self.config.server_bin.display())
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
            "model runtime ready"
        );
        Ok(())
    }

    fn generate(
        &self,
        request: &GenerationRequest,
        sink: &mut dyn FnMut(&str),
    ) -> Result<GenerationOutcome> {
        if !self.is_loaded() {
            return Err(anyhow!("model runtime is not loaded — load it first"));
        }
        let endpoint = self
            .endpoint
            .lock()
            .expect("endpoint lock")
            .clone()
            .ok_or_else(|| anyhow!("model runtime is not loaded — load it first"))?;
        self.cancel.store(false, Ordering::SeqCst);

        let body = self.chat_body(request);
        match stream_chat(
            endpoint.port,
            &endpoint.api_key,
            &body,
            &self.cancel,
            &self.active_stream,
            sink,
        ) {
            Ok((text, tokens, cancelled)) => Ok(GenerationOutcome {
                text,
                tokens,
                cancelled,
            }),
            Err(err) => {
                // Distinguish "the runtime died" from "the socket hiccuped".
                if !self.is_loaded() {
                    Err(anyhow!("model runtime exited unexpectedly ({err:#})"))
                } else {
                    Err(err)
                }
            }
        }
    }

    fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(stream) = self.active_stream.lock().expect("stream lock").as_ref() {
            // Interrupt the in-flight stream; the runtime stays up.
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    fn unload(&self) -> Result<()> {
        self.cancel();
        let child = self.child.lock().expect("child lock").take();
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait(); // reap — never leave a zombie
        }
        *self.endpoint.lock().expect("endpoint lock") = None;
        *self.active_stream.lock().expect("stream lock") = None;
        self.cancel.store(false, Ordering::SeqCst);
        Ok(())
    }
}

impl LlamaCppSubprocess {
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
                    "model runtime exited immediately ({status}) — see {}",
                    log_path().display()
                ));
            }
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "model runtime did not become ready within {}s — see {}",
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

/// One `/health` probe — connect, GET, look for a 200 status line.
fn health_ok(port: u16) -> bool {
    let Ok(stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return false;
    };
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(r) => r,
        Err(_) => return false,
    });
    let mut request = stream;
    if request
        .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut status = String::new();
    reader.read_line(&mut status).is_ok() && status.contains(" 200 ")
}

/// The streaming wire protocol, factored out so tests can drive it against
/// an in-process fake. Streams one chat completion; cancel aborts the
/// socket mid-flight. Returns `(text, tokens, cancelled)`.
pub(crate) fn stream_chat(
    port: u16,
    api_key: &str,
    body: &Value,
    cancel: &AtomicBool,
    active: &Mutex<Option<Arc<TcpStream>>>,
    sink: &mut dyn FnMut(&str),
) -> Result<(String, u32, bool)> {
    let stream = TcpStream::connect(("127.0.0.1", port)).context("connect to model runtime")?;
    stream.set_nodelay(true).ok();
    let stream = Arc::new(stream);
    *active.lock().expect("stream lock") = Some(Arc::clone(&stream));
    let mut reader = BufReader::new(
        stream
            .as_ref()
            .try_clone()
            .context("clone runtime socket")?,
    );

    let payload = body.to_string();
    let request = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
         Authorization: Bearer {api_key}\r\nContent-Type: application/json\r\n\
         Accept: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream
        .as_ref()
        .write_all(request.as_bytes())
        .context("send request to model runtime")?;

    let mut status = String::new();
    reader
        .read_line(&mut status)
        .context("read model runtime response")?;
    if !status.starts_with("HTTP/1.1 200") && !status.starts_with("HTTP/1.0 200") {
        let mut snippet = String::new();
        let _ = reader.take(2048).read_to_string(&mut snippet);
        *active.lock().expect("stream lock") = None;
        return Err(anyhow!(
            "model runtime refused generation ({}) {}",
            status.trim(),
            snippet.trim()
        ));
    }

    let mut text = String::new();
    let mut tokens = 0u32;
    let mut cancelled = false;
    let mut saw_done = false;
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // connection closed — checked against [DONE] below
            Ok(_) => {}
            Err(err) => {
                *active.lock().expect("stream lock") = None;
                if cancel.load(Ordering::SeqCst) {
                    cancelled = true;
                    break;
                }
                return Err(anyhow!("model runtime stream ended unexpectedly ({err})"));
            }
        }
        if cancel.load(Ordering::SeqCst) {
            let _ = stream.shutdown(Shutdown::Both);
            cancelled = true;
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            continue;
        }
        let Some(data) = trimmed.strip_prefix("data: ") else {
            continue; // other SSE fields carry nothing for us
        };
        if data == "[DONE]" {
            saw_done = true;
            break;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        if let Some(err) = value.get("error") {
            *active.lock().expect("stream lock") = None;
            return Err(anyhow!("model runtime error: {err}"));
        }
        if let Some(content) = value["choices"][0]["delta"]["content"].as_str()
            && !content.is_empty()
        {
            text.push_str(content);
            tokens += 1;
            sink(content);
        }
    }
    if cancel.load(Ordering::SeqCst) {
        cancelled = true;
    }
    *active.lock().expect("stream lock") = None;
    if !saw_done && !cancelled {
        // A real runtime always finishes with [DONE]; a truncated stream
        // means the runtime died mid-generation — never report that as a
        // successful (partial) completion.
        return Err(anyhow!(
            "model runtime stream ended before completion ({tokens} token(s) received)"
        ));
    }
    Ok((text, tokens, cancelled))
}

fn file_stem(path: &std::path::Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "model".into())
}

fn free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").context("allocate a loopback port")?;
    Ok(listener.local_addr()?.port())
}

/// 32 random bytes, hex — the per-session API key (A7). Time-fallback only
/// if the OS entropy source is unavailable.
fn random_key() -> String {
    let mut bytes = [0u8; 32];
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
            .unwrap_or(0x5eed);
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = ((nanos >> (i % 8 * 8)) & 0xff) as u8;
        }
    }
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn log_path() -> std::path::PathBuf {
    default_oca_dir().join("logs").join("llama-server.log")
}

/// Runtime logs go to `~/.oca/logs/llama-server.log` (or are dropped — the
/// TUI's stderr stays silent either way). llama-server does not echo
/// prompts into its logs, so the content gate (A7) holds by default.
fn runtime_log() -> Stdio {
    let path = log_path();
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

/// A tiny stand-in for `llama-server`, just enough of the OpenAI-compatible
/// API for tests and CI (which never have a real llama.cpp): `/health` and
/// streaming `/v1/chat/completions`. Documented-hidden on purpose — no
/// product code calls it.
#[doc(hidden)]
// Test-support code: unused by the product, but shipped so integration
// tests and CI can run a real subprocess without a llama.cpp install.
#[allow(dead_code)]
pub mod fake_server {
    use super::*;

    /// Accept connections forever; stream `tokens` chunks `delay` apart,
    /// then `[DONE]`.
    pub fn serve(listener: TcpListener, tokens: usize, delay: Duration) {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let _ = handle(&mut stream, tokens, delay);
        }
    }

    fn handle(stream: &mut TcpStream, tokens: usize, delay: Duration) -> std::io::Result<()> {
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut request_line = String::new();
        reader.read_line(&mut request_line)?;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header)? == 0 || header == "\r\n" || header == "\n" {
                break;
            }
        }
        if request_line.starts_with("GET /health") {
            stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 16\r\nConnection: close\r\n\r\n{\"status\":\"ok\"}",
            )?;
            return Ok(());
        }
        if request_line.starts_with("POST /v1/chat/completions") {
            stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )?;
            for i in 0..tokens {
                let chunk = format!(
                    "data: {{\"choices\":[{{\"delta\":{{\"content\":\"tok{i} \"}}}}]}}\n\n"
                );
                stream.write_all(chunk.as_bytes())?;
                stream.flush()?;
                if !delay.is_zero() {
                    std::thread::sleep(delay);
                }
            }
            stream.write_all(b"data: [DONE]\n\n")?;
            stream.flush()?;
            return Ok(());
        }
        stream.write_all(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serve_on(tokens: usize, delay: Duration) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || fake_server::serve(listener, tokens, delay));
        port
    }

    fn body() -> Value {
        json!({"messages": [], "stream": true})
    }

    #[test]
    fn streams_tokens_and_done() {
        let port = serve_on(3, Duration::ZERO);
        let cancel = AtomicBool::new(false);
        let active = Mutex::new(None);
        let mut seen = Vec::new();
        let (text, tokens, cancelled) =
            stream_chat(port, "key", &body(), &cancel, &active, &mut |t| {
                seen.push(t.to_string())
            })
            .unwrap();
        assert_eq!(tokens, 3);
        assert_eq!(text, "tok0 tok1 tok2 ");
        assert!(!cancelled);
        assert_eq!(seen, vec!["tok0 ", "tok1 ", "tok2 "]);
    }

    /// The critical requirement, 100/100: cancel interrupts a stream cleanly
    /// every single time — and always *after* real streaming began (the
    /// first token arrives through the sink before the cancel, so there is
    /// no timing flake under parallel test load).
    #[test]
    fn cancel_interrupts_100_of_100_streams() {
        let port = serve_on(50, Duration::from_millis(5));
        for round in 0..100 {
            let cancel = Arc::new(AtomicBool::new(false));
            let active: Mutex<Option<Arc<TcpStream>>> = Mutex::new(None);
            let worker_cancel = Arc::clone(&cancel);
            let (tx, rx) = std::sync::mpsc::channel();
            let handle = std::thread::spawn(move || {
                let mut sink = |token: &str| {
                    let _ = tx.send(token.to_string());
                };
                stream_chat(port, "key", &body(), &worker_cancel, &active, &mut sink)
            });
            // Wait until streaming genuinely began, then interrupt.
            rx.recv_timeout(Duration::from_secs(5))
                .unwrap_or_else(|_| panic!("round {round}: no first token"));
            cancel.store(true, Ordering::SeqCst);
            let result = handle.join().expect("worker thread");
            let (text, tokens, cancelled) =
                result.unwrap_or_else(|e| panic!("round {round}: {e:#}"));
            assert!(cancelled, "round {round} must report cancelled");
            assert!(tokens < 50, "round {round} stopped early ({tokens} tokens)");
            assert!(!text.is_empty(), "round {round} streamed something first");
        }
    }

    #[test]
    fn http_errors_carry_the_status() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let _ = stream.write_all(
                    b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 21\r\nConnection: close\r\n\r\n{\"error\":{\"msg\":\"no\"}}",
                );
            }
        });
        let cancel = AtomicBool::new(false);
        let active = Mutex::new(None);
        let mut sink = |_: &str| {};
        let err = stream_chat(port, "key", &body(), &cancel, &active, &mut sink)
            .unwrap_err()
            .to_string();
        assert!(err.contains("500"), "{err}");
        assert!(err.contains("refused"), "{err}");
    }

    #[test]
    fn dead_port_is_a_connect_error_not_a_hang() {
        let port = serve_on(1, Duration::ZERO);
        // Find a port that is definitely closed: bind, note, drop.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let dead = listener.local_addr().unwrap().port();
        drop(listener);
        let _ = port;
        let cancel = AtomicBool::new(false);
        let active = Mutex::new(None);
        let mut sink = |_: &str| {};
        let err = stream_chat(dead, "key", &body(), &cancel, &active, &mut sink)
            .unwrap_err()
            .to_string();
        assert!(err.contains("connect"), "{err}");
    }
}
