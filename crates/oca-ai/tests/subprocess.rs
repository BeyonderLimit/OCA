//! Phase 7 exit criteria, end to end against a real spawned process:
//! streaming generation starts/stops cleanly 100/100 times, and killing
//! the model process is a clean error — OCA never goes down with it.
//!
//! CI has no llama.cpp, so the "server" is this very test binary in a
//! disguise: `fake_llama_server_main` serves the OpenAI-compatible surface
//! (see `oca_ai::fake_server`) when the child-only env marker is present.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use oca_ai::{GenerationRequest, LanguageModel, LlamaCppSubprocess, ModelConfig};

/// Child-only configuration, delivered via `server_env` (never on our own
/// environment — so this entry can never misfire during a normal run).
fn fake_config(model_file: &PathBuf, tokens: usize, delay_ms: u64) -> (ModelConfig, u16) {
    // Pick a port, then let the child have it.
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let exe = std::env::current_exe().expect("test binary path");
    let mut config = ModelConfig::new(model_file, exe);
    config.server_args = Some(vec![
        "fake_llama_server_main".into(),
        "--exact".into(),
        "--quiet".into(),
    ]);
    config.server_env = vec![
        ("OCA_FAKE_PORT".into(), port.to_string()),
        ("OCA_FAKE_TOKENS".into(), tokens.to_string()),
        ("OCA_FAKE_DELAY_MS".into(), delay_ms.to_string()),
    ];
    config.port = Some(port);
    config.startup_timeout = Duration::from_secs(20);
    (config, port)
}

/// The self-spawn entry: a normal `cargo test` run sees the missing env
/// marker and returns immediately; the child serves forever.
#[test]
fn fake_llama_server_main() {
    let Ok(port) = std::env::var("OCA_FAKE_PORT") else {
        return;
    };
    let port: u16 = port.parse().expect("fake port");
    let tokens: usize = std::env::var("OCA_FAKE_TOKENS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let delay_ms: u64 = std::env::var("OCA_FAKE_DELAY_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).expect("bind fake port");
    oca_ai::fake_server::serve(listener, tokens, Duration::from_millis(delay_ms));
}

fn temp_model() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("fake.gguf");
    std::fs::write(&path, b"not a real model; the fake server never reads it").unwrap();
    (tmp, path)
}

/// The headline criterion: load → stream → unload, 100 out of 100.
#[test]
fn load_stream_unload_100_of_100() {
    let (_tmp, model_file) = temp_model();
    for round in 0..100 {
        let (config, _port) = fake_config(&model_file, 3, 0);
        let model = LlamaCppSubprocess::new(config);
        model
            .load()
            .unwrap_or_else(|e| panic!("round {round}: load failed: {e:#}"));
        let mut text = String::new();
        let out = model
            .generate(&GenerationRequest::default(), &mut |token| {
                text.push_str(token)
            })
            .unwrap_or_else(|e| panic!("round {round}: generate failed: {e:#}"));
        assert_eq!(out.tokens, 3, "round {round}");
        assert_eq!(text, "tok0 tok1 tok2 ", "round {round}");
        assert!(!out.cancelled, "round {round}");
        model
            .unload()
            .unwrap_or_else(|e| panic!("round {round}: {e:#}"));
        assert!(!model.is_loaded(), "round {round}");
        assert!(model.server_pid().is_none(), "round {round}");
    }
}

/// The other headline criterion: an externally killed runtime is a clean
/// error mid-stream, not a dead OCA — and a fresh load recovers.
#[test]
fn killed_runtime_is_a_clean_error_and_reload_recovers() {
    let (_tmp, model_file) = temp_model();
    // 200 tokens × 5ms ≈ 1s of streaming — plenty of time to kill it.
    let (config, _port) = fake_config(&model_file, 200, 5);
    let model = Arc::new(LlamaCppSubprocess::new(config));
    model.load().expect("load");

    let worker = Arc::clone(&model);
    let handle = std::thread::spawn(move || {
        let mut sink = |_: &str| {};
        worker.generate(&GenerationRequest::default(), &mut sink)
    });

    // Kill the runtime out from under the stream.
    std::thread::sleep(Duration::from_millis(200));
    let pid = model.server_pid().expect("server pid");
    kill_hard(pid);
    std::thread::sleep(Duration::from_millis(50));

    let result = handle.join().expect("worker thread must not panic");
    let err = result
        .expect_err("a killed runtime must surface as an error")
        .to_string();
    assert!(
        err.contains("exited unexpectedly") || err.contains("stream ended"),
        "clean crash message: {err}"
    );
    assert!(!model.is_loaded(), "the dead runtime is reaped");
    model.unload().unwrap(); // idempotent, no panic

    // Recovery: a fresh load starts a new runtime and streams fine.
    model.load().expect("reload");
    let out = model
        .generate(&GenerationRequest::default(), &mut |_| {})
        .expect("generate after recovery");
    assert_eq!(out.tokens, 200);
    model.unload().unwrap();
}

/// SIGKILL (unix) / taskkill (windows) — an *external* death, not unload.
#[cfg(unix)]
fn kill_hard(pid: u32) {
    // SAFETY: plain signal to our own spawned child; ESRCH is fine.
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
}

#[cfg(windows)]
fn kill_hard(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .output();
}

/// A runtime that dies on startup (here: the fake exits immediately
/// because it got no port) is reported before any generation is attempted.
#[test]
fn early_death_is_reported_at_load() {
    let (_tmp, model_file) = temp_model();
    let exe = std::env::current_exe().unwrap();
    let mut config = ModelConfig::new(&model_file, exe);
    // The entry finds no OCA_FAKE_PORT and returns instantly.
    config.server_args = Some(vec![
        "fake_llama_server_main".into(),
        "--exact".into(),
        "--quiet".into(),
    ]);
    config.port = Some({
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    });
    config.startup_timeout = Duration::from_secs(5);
    let model = LlamaCppSubprocess::new(config);
    let err = model.load().unwrap_err().to_string();
    assert!(err.contains("exited immediately"), "{err}");
    assert!(!model.is_loaded());
}

/// A missing binary is a clean load error, never a spawn panic.
#[test]
fn missing_binary_is_a_clean_error() {
    let (_tmp, model_file) = temp_model();
    let config = ModelConfig::new(&model_file, "/definitely/not/here/llama-server");
    let model = LlamaCppSubprocess::new(config);
    let err = model.load().unwrap_err().to_string();
    assert!(err.contains("start model runtime"), "{err}");
    assert!(!model.is_loaded());
    model.unload().unwrap();
}

/// Generate before load → a clear instruction, not a socket error.
#[test]
fn generate_before_load_explains_itself() {
    let (_tmp, model_file) = temp_model();
    let config = ModelConfig::new(&model_file, "/definitely/not/here/llama-server");
    let model = LlamaCppSubprocess::new(config);
    let mut sink = |_: &str| {};
    let err = model
        .generate(&GenerationRequest::default(), &mut sink)
        .unwrap_err()
        .to_string();
    assert!(err.contains("load it first"), "{err}");
}
