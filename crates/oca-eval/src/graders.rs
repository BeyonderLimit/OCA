//! The deterministic graders. Each is a pure function of one output
//! string; nothing here knows what a model is.

use oca_ai::repair_json;

/// One grader's verdict on one output — small enough to assert on in CI
/// and readable in a report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grade {
    pub grader: &'static str,
    pub passed: bool,
    pub detail: String,
}

impl std::fmt::Display for Grade {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}: {}",
            if self.passed { "PASS" } else { "FAIL" },
            self.grader,
            self.detail
        )
    }
}

/// A named, deterministic quality check over an operation's output.
pub trait Grader {
    /// Stable name for reports and CI logs.
    fn name(&self) -> &'static str;
    fn grade(&self, output: &str) -> Grade;
}

fn verdict(grader: &'static str, passed: bool, detail: String) -> Grade {
    Grade {
        grader,
        passed,
        detail,
    }
}

/// The output has at least one word — the floor contract for free-text
/// operations (continue, rewrite) that carry no stronger schema.
#[derive(Clone, Copy, Debug, Default)]
pub struct NonEmpty;

impl Grader for NonEmpty {
    fn name(&self) -> &'static str {
        "non-empty"
    }
    fn grade(&self, output: &str) -> Grade {
        let words = output.split_whitespace().count();
        verdict(self.name(), words > 0, format!("{words} word(s)"))
    }
}

/// The output parses as a JSON object carrying every required top-level
/// key. Uses the same fence-repair policy as the operations pipeline, so
/// "what counts as valid" means one thing across the system.
#[derive(Clone, Debug)]
pub struct SchemaValidity {
    pub required: &'static [&'static str],
}

impl Grader for SchemaValidity {
    fn name(&self) -> &'static str {
        "schema-validity"
    }
    fn grade(&self, output: &str) -> Grade {
        let value: serde_json::Value = match serde_json::from_str(repair_json(output)) {
            Ok(value) => value,
            Err(e) => return verdict(self.name(), false, format!("not valid JSON: {e}")),
        };
        let Some(object) = value.as_object() else {
            return verdict(
                self.name(),
                false,
                "valid JSON but not an object".to_string(),
            );
        };
        let missing: Vec<&str> = self
            .required
            .iter()
            .copied()
            .filter(|key| !object.contains_key(*key))
            .collect();
        if missing.is_empty() {
            verdict(self.name(), true, format!("{:?} present", self.required))
        } else {
            verdict(
                self.name(),
                false,
                format!("missing required key(s): {missing:?}"),
            )
        }
    }
}

/// The summary's word count sits inside the band the operation promised.
#[derive(Clone, Copy, Debug)]
pub struct SummaryLength {
    pub min_words: usize,
    pub max_words: usize,
}

impl Grader for SummaryLength {
    fn name(&self) -> &'static str {
        "summary-length"
    }
    fn grade(&self, output: &str) -> Grade {
        let words = output.split_whitespace().count();
        verdict(
            self.name(),
            words >= self.min_words && words <= self.max_words,
            format!("{words} words (want {}–{})", self.min_words, self.max_words),
        )
    }
}

/// Every named entity that must survive a rewrite appears in the output.
/// The entity list is explicit (usually [`crate::character_names`]
/// intersected with the selection) — no guessing.
#[derive(Clone, Debug, Default)]
pub struct PreservesNamedEntities {
    pub entities: Vec<String>,
}

impl Grader for PreservesNamedEntities {
    fn name(&self) -> &'static str {
        "preserves-named-entities"
    }
    fn grade(&self, output: &str) -> Grade {
        let missing: Vec<&str> = self
            .entities
            .iter()
            .filter(|entity| !output.contains(entity.as_str()))
            .map(String::as_str)
            .collect();
        if missing.is_empty() {
            verdict(
                self.name(),
                true,
                format!("{} entit(y/ies) preserved", self.entities.len()),
            )
        } else {
            verdict(
                self.name(),
                false,
                format!("missing from the output: {missing:?}"),
            )
        }
    }
}

/// Normalize a lyric line the way the hook comparison does: chord
/// markers stripped, whitespace collapsed, case folded. Used by
/// [`PreservesHook`] — chords are not the hook.
fn normalize_lyric_line(line: &str) -> String {
    let (text, _) = oca_music::parse_chord_line(line);
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Does any rhyme class repeat? A scheme where every letter appears once
/// means nothing rhymes yet — there is no scheme to preserve.
fn scheme_repeats(labels: &[String]) -> bool {
    labels
        .iter()
        .enumerate()
        .any(|(i, l)| !l.is_empty() && labels[..i].contains(l))
}

/// The song's hook — [`oca_music::Lyrics::hook_line`], the first chorus
/// line — still appears in the output, unchanged. This is the Phase 10
/// preserve-hook contract; the hook's identity comes from `oca-music`,
/// never from the model's say-so.
#[derive(Clone, Debug, Default)]
pub struct PreservesHook {
    pub hook: String,
}

impl Grader for PreservesHook {
    fn name(&self) -> &'static str {
        "preserves-hook"
    }
    fn grade(&self, output: &str) -> Grade {
        if self.hook.trim().is_empty() {
            return verdict(
                self.name(),
                false,
                "no hook to preserve — the song has no chorus yet".to_string(),
            );
        }
        let hook = normalize_lyric_line(&self.hook);
        let hits = output
            .lines()
            .filter(|line| normalize_lyric_line(line) == hook)
            .count();
        verdict(
            self.name(),
            hits >= 1,
            format!("hook present {hits} time(s)"),
        )
    }
}

/// The output's end-rhyme scheme equals the original's — the Phase 10
/// preserve-rhyme-scheme contract, measured by `oca-music`'s
/// deterministic [`oca_music::rhyme_scheme`] (AGENTS: never reimplement
/// syllable/rhyme math here — call it). An original with no rhyming pair
/// fails honestly: an all-distinct scheme would pass vacuously.
#[derive(Clone, Debug, Default)]
pub struct PreservesRhymeScheme {
    /// The original selection's scheme, from [`oca_music::score_lines`].
    pub scheme: Vec<String>,
    /// The language the phonetics run under (song/project default).
    pub language: String,
}

impl Grader for PreservesRhymeScheme {
    fn name(&self) -> &'static str {
        "preserves-rhyme-scheme"
    }
    fn grade(&self, output: &str) -> Grade {
        if !scheme_repeats(&self.scheme) {
            return verdict(
                self.name(),
                false,
                "the original has no measurable rhymes — there is no scheme to preserve"
                    .to_string(),
            );
        }
        let lines: Vec<String> = output.lines().map(str::to_string).collect();
        let got = oca_music::score_lines(&lines, &self.language).rhyme_scheme;
        verdict(
            self.name(),
            got == self.scheme,
            format!("scheme {} (want {})", got.join(" "), self.scheme.join(" ")),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_empty_catches_silence() {
        assert!(NonEmpty.grade("The storm broke.").passed);
        assert!(!NonEmpty.grade("   \n").passed);
        assert!(!NonEmpty.grade("").passed);
    }

    #[test]
    fn schema_validity_checks_keys_after_fence_repair() {
        let grader = SchemaValidity {
            required: &["issues"],
        };
        assert!(grader.grade("{\"issues\": []}").passed);
        assert!(grader.grade("```json\n{\"issues\": []}\n```").passed);
        assert!(
            grader.grade("{\"issues\": [], \"extra\": 1}").passed,
            "extra keys are fine"
        );
        let fail = grader.grade("Looks fine to me!");
        assert!(!fail.passed);
        assert!(fail.detail.contains("not valid JSON"), "{fail}");
        let fail = grader.grade("{\"other\": 1}");
        assert!(!fail.passed);
        assert!(fail.detail.contains("issues"), "{fail}");
        assert!(!grader.grade("[1, 2]").passed, "not an object");
    }

    #[test]
    fn summary_length_enforces_the_band() {
        let grader = SummaryLength {
            min_words: 3,
            max_words: 5,
        };
        assert!(grader.grade("One two three.").passed);
        assert!(!grader.grade("One two.").passed, "too short");
        assert!(
            !grader.grade("One two three four five six.").passed,
            "too long"
        );
    }

    #[test]
    fn entity_preservation_reports_what_went_missing() {
        let grader = PreservesNamedEntities {
            entities: vec!["Elena".into(), "Marcus".into()],
        };
        assert!(
            grader
                .grade("Elena waited; Marcus watched the door.")
                .passed
        );
        let fail = grader.grade("She waited by the door.");
        assert!(!fail.passed);
        assert!(
            fail.detail.contains("Elena") && fail.detail.contains("Marcus"),
            "{fail}"
        );
    }

    #[test]
    fn hook_grader_needs_a_real_hook_and_a_verbatim_line() {
        let grader = PreservesHook {
            hook: "Keep the light on".into(),
        };
        assert!(grader.grade("Keep the light on\nthe kettle hums").passed);
        assert!(
            grader.grade("[G]Keep the light on\nthe kettle hums").passed,
            "chords are not the hook"
        );
        let fail = grader.grade("Hold the lamp up\nthe kettle hums");
        assert!(!fail.passed);
        assert!(fail.detail.contains("0 time"), "{fail}");
        let none = PreservesHook::default().grade("anything");
        assert!(!none.passed, "{none}");
    }

    #[test]
    fn rhyme_scheme_grader_compares_deterministic_schemes() {
        let grader = PreservesRhymeScheme {
            scheme: vec!["a".into(), "a".into()],
            language: "en".into(),
        };
        assert!(
            grader
                .grade("the dark keeps score\nand I will keep it till the door")
                .passed
        );
        let fail = grader.grade("rows of light\nand quiet sand");
        assert!(!fail.passed, "{fail}");
        assert!(fail.detail.contains("want a a"), "{fail}");
        // An original with no rhyming pair: nothing to preserve — an
        // all-distinct scheme must not pass vacuously.
        let vacuous = PreservesRhymeScheme {
            scheme: vec!["a".into(), "b".into()],
            language: "en".into(),
        }
        .grade("anything at all\nwith two lines");
        assert!(!vacuous.passed, "{vacuous}");
    }
}
