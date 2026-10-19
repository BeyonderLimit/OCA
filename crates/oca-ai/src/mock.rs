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
    /// Request-aware canned responses, tried in order: the first pattern
    /// found in the prompt (or system) wins. Empty = plain `tokens`.
    scripts: Vec<Script>,
    delay: Duration,
    loaded: AtomicBool,
    cancelled: AtomicBool,
    counters: Mutex<Counters>,
    fail_load: Option<String>,
    fail_generate: Option<String>,
}

/// One canned response: match when the prompt or system contains `find`,
/// then stream `then` as tokens.
#[derive(Clone, Debug)]
pub struct Script {
    pub find: String,
    pub then: Vec<String>,
}

/// Split canned text into streamable tokens at whitespace boundaries.
fn tokens(text: &str) -> Vec<String> {
    text.split_inclusive(char::is_whitespace)
        .map(str::to_string)
        .collect()
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
            scripts: Vec::new(),
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

    /// Request-aware canned responses, tried in order (Phase 9 ops).
    pub fn with_scripts(mut self, scripts: Vec<Script>) -> Self {
        self.scripts = scripts;
        self
    }

    /// The canned model every story-AI test runs against: recognizes each
    /// Phase 9 operation by the `Operation:` line its prompt carries and
    /// answers with output that satisfies that operation's contract
    /// (valid JSON for the schema ops).
    pub fn story_defaults() -> Self {
        Self::new().with_scripts(story_scripts())
    }

    /// The canned model every music-AI test runs against (Phase 10):
    /// recognizes each operation by its `Operation:` line and answers
    /// with output that satisfies that operation's contract — real end
    /// rhymes for the scored rewrites, valid JSON for the schema ops.
    pub fn music_defaults() -> Self {
        Self::new().with_scripts(music_scripts())
    }

    /// Every canned script, story and music — what `[ai] mock = true`
    /// boots (`oca_ai::bootstrap`), so the CLI/TUI run every operation
    /// offline with one scripted model.
    pub fn ai_defaults() -> Self {
        Self::new().with_scripts([story_scripts(), music_scripts()].concat())
    }
}

/// The Phase 9 story scripts — what [`MockModel::story_defaults`] serves.
fn story_scripts() -> Vec<Script> {
    vec![
        Script {
            find: "Operation: continue_scene".into(),
            then: tokens(" The storm broke over the harbor before anyone could reach the boats."),
        },
        Script {
            find: "Operation: rewrite_selection".into(),
            then: tokens("Elena read the letter twice; Marcus waited by the door."),
        },
        Script {
            find: "Operation: summarize_chapter".into(),
            then: tokens(
                "Elena receives a letter that names no sender. Marcus claims he \
                     found it in the archive; she does not believe him. They argue, \
                     and she hides the letter inside the hall.",
            ),
        },
        Script {
            find: "Operation: generate_outline".into(),
            then: tokens(
                "{\"sections\":[{\"title\":\"Act I — The Letter\",\"beats\":[\
                     \"Elena receives the letter\",\"Marcus lies about the archive\",\
                     \"The elders convene at the hall\"]}]}",
            ),
        },
        Script {
            find: "Operation: review_chapter".into(),
            then: tokens(
                "{\"issues\":[{\"type\":\"timeline\",\"severity\":\"warning\",\
                     \"location\":\"paragraph 2\",\"explanation\":\"The letter is \
                     said to arrive after market day, yet the market scene already \
                     references its contents.\"}]}",
            ),
        },
    ]
}

/// The Phase 10 music scripts — what [`MockModel::music_defaults`] serves.
fn music_scripts() -> Vec<Script> {
    vec![
        Script {
            find: "Operation: song_concept".into(),
            then: tokens(
                "The song stays in one room with the sea outside: a harbor-window \
                     lamp that outlasts the night. Titles: \"Kept Light\", \"Harbor \
                     Almanac\", \"Lamp Against the Tide\". Themes: waiting, devotion, \
                     return.",
            ),
        },
        Script {
            find: "Operation: song_structure".into(),
            then: tokens("intro\nverse\nchorus\nverse\nchorus\nbridge\nchorus\noutro"),
        },
        Script {
            find: "Operation: generate_section".into(),
            then: tokens("The gulls turn over the black water\nand the cold comes in like a bell"),
        },
        Script {
            find: "Operation: rewrite_lyrics".into(),
            then: tokens(
                "The ferry lamps go out in rows\nthe harbor keeps its slow repose\n\
                     I set the kettle by the door\nand wait for footsteps on the shore",
            ),
        },
        Script {
            find: "Operation: improve_rhyme".into(),
            then: tokens("The dark keeps score\nand I will keep the light until the door"),
        },
        Script {
            find: "Operation: improve_meter".into(),
            then: tokens(
                "The ferry lamps go out in rows\nthe harbor keeps its slow repose\n\
                     I set the kettle by the door\nand wait for footsteps on the shore",
            ),
        },
        Script {
            find: "Operation: preserve_hook".into(),
            then: tokens("Keep the light on\nthe winter keeps its own long song"),
        },
        Script {
            find: "Operation: preserve_rhyme_scheme".into(),
            then: tokens("The dark keeps score\nand I will keep the light until the door"),
        },
        Script {
            find: "Operation: alternatives".into(),
            then: tokens(
                "{\"alternatives\":[\"The ferry lamps go out in rows\",\
                     \"Low water hums a patient blues\",\"A window keeps its small \
                     repose\"]}",
            ),
        },
        Script {
            find: "Operation: analyze_song".into(),
            then: tokens(
                "{\"observations\":[{\"area\":\"structure\",\"note\":\"The chorus \
                     carries the song's only repeated image; the verses walk toward \
                     it.\"},{\"area\":\"hook\",\"note\":\"The hook opens the chorus, \
                     where a listener expects it.\"}]}",
            ),
        },
        Script {
            find: "Operation: check_singability".into(),
            then: tokens(
                "{\"verdict\":\"Singable — every line sits in a comfortable 6–9 \
                     syllable band and the hook is easy to hold.\",\"problems\":[]}",
            ),
        },
    ]
}

impl MockModel {
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
        request: &GenerationRequest,
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

        // Request-aware scripts first (Phase 9 operations), else canned.
        let scripted = self.scripts.iter().find(|s| {
            request.prompt.contains(&s.find)
                || request
                    .system
                    .as_deref()
                    .is_some_and(|system| system.contains(&s.find))
        });
        let source: &[String] = scripted.map(|s| s.then.as_slice()).unwrap_or(&self.tokens);

        let mut text = String::new();
        let mut tokens = 0u32;
        let mut stopped = false;
        for token in source {
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
