//! The scripted [`LanguageModel`] every test runs against (AGENTS rule 6:
//! no test ever touches a real model). Streams canned tokens with a
//! configurable delay, honors `cancel` per token, and can simulate failures
//! so crash-isolation paths stay honest.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Result, anyhow};

use crate::{GenerationOutcome, GenerationRequest, LanguageModel, ModelInfo};

/// What the mock recorded — tests assert on call counts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub load: usize,
    pub unload: usize,
    pub generate: usize,
    pub cancel: usize,
}

/// A deterministic fake: fixed tokens, optional delay, optional injected
/// failures. Send + Sync — share it across the job thread and the test.
pub struct MockModel {
    info: ModelInfo,
    tokens: Vec<String>,
    delay: Duration,
    loaded: AtomicBool,
    cancelled: AtomicBool,
    counters: Mutex<Counters>,
    fail_load: Option<String>,
    fail_generate: Option<String>,
}

impl Default for MockModel {
    fn default() -> Self {
        Self::new()
    }
}

impl MockModel {
    pub fn new() -> Self {
        Self {
            info: ModelInfo {
                name: "mock".into(),
                context: 4096,
                gpu_layers: 0,
                threads: 1,
                runtime: "mock".into(),
            },
            tokens: vec![
                "Hello".into(),
                " from".into(),
                " the".into(),
                " mock".into(),
                ".".into(),
            ],
            delay: Duration::from_millis(0),
            loaded: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            counters: Mutex::new(Counters::default()),
            fail_load: None,
            fail_generate: None,
        }
    }

    pub fn with_tokens(mut self, tokens: Vec<String>) -> Self {
        self.tokens = tokens;
        self
    }

    pub fn with_delay(mut self, ms: u64) -> Self {
        self.delay = Duration::from_millis(ms);
        self
    }

    /// Make `load` fail — for "runtime missing" paths.
    pub fn fails_load(mut self, message: impl Into<String>) -> Self {
        self.fail_load = Some(message.into());
        self
    }

    /// Make `generate` fail — the crash-isolation simulation.
    pub fn fails_generate(mut self, message: impl Into<String>) -> Self {
        self.fail_generate = Some(message.into());
        self
    }

    pub fn counters(&self) -> Counters {
        self.counters.lock().expect("mock counters").clone()
    }
}

impl LanguageModel for MockModel {
    fn info(&self) -> ModelInfo {
        self.info.clone()
    }

    fn is_loaded(&self) -> bool {
        self.loaded.load(Ordering::SeqCst)
    }

    fn load(&self) -> Result<()> {
        if let Some(message) = &self.fail_load {
            return Err(anyhow!(message.clone()));
        }
        self.counters.lock().expect("mock counters").load += 1;
        self.loaded.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn generate(
        &self,
        _request: &GenerationRequest,
        sink: &mut dyn FnMut(&str),
    ) -> Result<GenerationOutcome> {
        if !self.is_loaded() {
            return Err(anyhow!("model is not loaded"));
        }
        if let Some(message) = &self.fail_generate {
            return Err(anyhow!(message.clone()));
        }
        self.counters.lock().expect("mock counters").generate += 1;
        self.cancelled.store(false, Ordering::SeqCst);

        let mut text = String::new();
        let mut tokens = 0u32;
        let mut stopped = false;
        for token in &self.tokens {
            if self.cancelled.load(Ordering::SeqCst) {
                stopped = true;
                break;
            }
            if !self.delay.is_zero() {
                std::thread::sleep(self.delay);
            }
            if self.cancelled.load(Ordering::SeqCst) {
                stopped = true;
                break;
            }
            text.push_str(token);
            tokens += 1;
            sink(token);
        }
        if stopped {
            self.counters.lock().expect("mock counters").cancel += 1;
        }
        Ok(GenerationOutcome {
            text,
            tokens,
            cancelled: stopped,
        })
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    fn unload(&self) -> Result<()> {
        self.counters.lock().expect("mock counters").unload += 1;
        self.loaded.store(false, Ordering::SeqCst);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_every_token_when_never_cancelled() {
        let model = MockModel::new().with_tokens(vec!["a".into(), "b".into(), "c".into()]);
        model.load().unwrap();
        let mut seen = Vec::new();
        let out = model
            .generate(&GenerationRequest::default(), &mut |t| {
                seen.push(t.to_string())
            })
            .unwrap();
        assert_eq!(seen, vec!["a", "b", "c"]);
        assert_eq!(out.text, "abc");
        assert!(!out.cancelled);
    }

    #[test]
    fn generate_without_load_is_a_clean_error() {
        let model = MockModel::new();
        let mut sink = |_: &str| {};
        let err = model
            .generate(&GenerationRequest::default(), &mut sink)
            .unwrap_err()
            .to_string();
        assert!(err.contains("not loaded"), "{err}");
    }
}
