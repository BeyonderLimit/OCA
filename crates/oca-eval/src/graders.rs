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
}
