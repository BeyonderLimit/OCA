//! Phase 11 embedding-seam tests, end to end against a real spawned
//! process: load → embed → unload through the OpenAI-compatible
//! `/v1/embeddings` surface, plus the residency rule (A6) and the clean
//! error paths. CI has no llama.cpp, so the "runtime" is this very test
//! binary in a disguise (`fake_server` serves embeddings when the
//! child-only env marker is present).

use std::path::PathBuf;
use std::sync::Arc;

use oca_ai::{
    Embedder, EmbedderConfig, HeavyModel, LlamaCppEmbedder, MOCK_EMBED_DIM, ModelManager,
    deterministic_embedding,
};

/// Child-only configuration, delivered via `server_env` (never on our own
/// environment — so this entry can never misfire during a normal run).
fn fake_config(model_file: &PathBuf) -> (EmbedderConfig, u16) {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let exe = std::env::current_exe().expect("test binary path");
    let mut config = EmbedderConfig::new(model_file, exe);
    config.server_args = Some(vec![
        "fake_embedding_server_main".into(),
        "--exact".into(),
        "--quiet".into(),
    ]);
    config.server_env = vec![("OCA_FAKE_PORT".into(), port.to_string())];
    config.port = Some(port);
    config.startup_timeout = std::time::Duration::from_secs(20);
    (config, port)
}

/// The self-spawn entry: a normal `cargo test` run sees the missing env
/// marker and returns immediately; the child serves forever.
#[test]
fn fake_embedding_server_main() {
    let Ok(port) = std::env::var("OCA_FAKE_PORT") else {
        return;
    };
    let port: u16 = port.parse().expect("fake port");
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).expect("bind fake port");
    oca_ai::fake_server::serve(listener, 0, std::time::Duration::ZERO);
}

fn temp_model() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("fake-embed.gguf");
    std::fs::write(&path, b"not a real model; the fake server never reads it").unwrap();
    (tmp, path)
}

#[test]
fn load_embed_unload_round_trip() {
    let (_tmp, model_file) = temp_model();
    let (config, _port) = fake_config(&model_file);
    let embedder = LlamaCppEmbedder::new(config, ModelManager::new());
    assert!(!embedder.is_loaded());
    embedder.load().expect("load");
    assert!(embedder.is_loaded());

    let texts = ["the harbor bell", "harbor law"];
    let vectors = embedder.embed(&texts).expect("embed");
    assert_eq!(vectors.len(), 2, "one vector per input, same order");
    for vector in &vectors {
        assert_eq!(vector.len(), MOCK_EMBED_DIM);
    }
    // The fake answers with the shared deterministic vectors — the seam
    // transports model output faithfully, dimension included.
    assert_eq!(
        vectors[0],
        deterministic_embedding("the harbor bell", MOCK_EMBED_DIM)
    );

    embedder.unload().expect("unload");
    assert!(!embedder.is_loaded());
    assert!(embedder.server_pid().is_none());
}

/// The one-heavy-model governor (A6): a resident language model blocks an
/// embedding load until it is unloaded — explicit swap, never a fight.
#[test]
fn embeddings_respect_the_residency_budget() {
    let (_tmp, model_file) = temp_model();
    let governor = ModelManager::new();
    governor.claim(HeavyModel::Llm).unwrap();
    let (config, _port) = fake_config(&model_file);
    let embedder = LlamaCppEmbedder::new(config, governor.clone());
    let err = embedder.load().unwrap_err().to_string();
    assert!(err.contains("already resident"), "{err}");

    governor.release(HeavyModel::Llm);
    embedder.load().expect("load after the swap");
    // Two embeddings sessions in a row reuse the slot without complaint.
    embedder.unload().unwrap();
    embedder.load().unwrap();
    embedder.unload().unwrap();
    assert_eq!(governor.resident(), None);
}

#[test]
fn embed_before_load_explains_itself() {
    let (_tmp, model_file) = temp_model();
    let (config, _port) = fake_config(&model_file);
    let embedder = LlamaCppEmbedder::new(config, ModelManager::new());
    let err = embedder.embed(&["x"]).unwrap_err().to_string();
    assert!(err.contains("not loaded"), "{err}");
}

/// A missing binary is a clean load error, never a spawn panic — and the
/// governor slot comes back.
#[test]
fn missing_binary_is_a_clean_error_and_frees_the_slot() {
    let (_tmp, model_file) = temp_model();
    let governor = ModelManager::new();
    let config = EmbedderConfig::new(&model_file, "/definitely/not/here/llama-server");
    let embedder = LlamaCppEmbedder::new(config, governor.clone());
    let err = embedder.load().unwrap_err().to_string();
    assert!(err.contains("start embedding runtime"), "{err}");
    assert!(!embedder.is_loaded());
    assert_eq!(
        governor.resident(),
        None,
        "a failed load must not leak the slot"
    );
}

/// A runtime that exits immediately (no port marker) surfaces at load.
#[test]
fn early_death_is_reported_at_load() {
    let (_tmp, model_file) = temp_model();
    let exe = std::env::current_exe().unwrap();
    let mut config = EmbedderConfig::new(&model_file, exe);
    config.server_args = Some(vec![
        "fake_embedding_server_main".into(),
        "--exact".into(),
        "--quiet".into(),
    ]);
    config.port = Some({
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    });
    config.startup_timeout = std::time::Duration::from_secs(5);
    let embedder = LlamaCppEmbedder::new(config, ModelManager::new());
    let err = embedder.load().unwrap_err().to_string();
    assert!(err.contains("exited immediately"), "{err}");
    assert!(!embedder.is_loaded());
}

/// The mock (rule 6) answers deterministic, normalized vectors and counts
/// its calls — the incremental-reindex tests assert on those counts.
#[test]
fn mock_embedder_is_deterministic_and_counts_calls() {
    use oca_ai::MockEmbedder;

    let embedder = MockEmbedder::new();
    let err = embedder.embed(&["x"]).unwrap_err().to_string();
    assert!(err.contains("not loaded"), "{err}");
    embedder.load().unwrap();
    let a = embedder.embed(&["the elysian gates"]).unwrap();
    let b = embedder.embed(&["the elysian gates"]).unwrap();
    assert_eq!(a, b);
    let counters = embedder.counters();
    assert_eq!(counters.embed, 2);
    assert_eq!(counters.texts, 2);
    embedder.unload().unwrap();
    assert_eq!(embedder.counters().unload, 1);
}

/// Mock and fake runtime produce identical vectors for the same text —
/// tests through either seam see the same retrieval behavior.
#[test]
fn mock_matches_the_fake_runtime_vectors() {
    let (_tmp, model_file) = temp_model();
    let (config, _port) = fake_config(&model_file);
    let runtime = LlamaCppEmbedder::new(config, ModelManager::new());
    runtime.load().unwrap();
    let via_runtime = runtime.embed(&["a passage about harbor law"]).unwrap();

    let mock = oca_ai::MockEmbedder::new();
    mock.load().unwrap();
    let via_mock = mock.embed(&["a passage about harbor law"]).unwrap();
    assert_eq!(via_runtime, via_mock);
    runtime.unload().unwrap();
}

/// The facade shares one governor with embeddings (A6) — same handle the
/// panel and dictation hold.
#[test]
fn facade_governor_is_shareable_with_embeddings() {
    let (_tmp, model_file) = temp_model();
    let (config, _port) = fake_config(&model_file);
    let embedder = Arc::new(LlamaCppEmbedder::new(config, ModelManager::new()));
    embedder.load().unwrap();
    assert!(embedder.is_loaded());
    embedder.unload().unwrap();
}
