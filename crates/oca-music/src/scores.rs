//! Deterministic lyric scores for one block of lines (Phase 10): what the
//! AI proposal review shows for both the current draft and the revision
//! (`OCA-build.md` §7.3 — "rhyme/meter scores computed deterministically
//! for both"), and what the eval graders measure. Nothing here knows what
//! a model is — the same Phase 3 phonetics pipeline runs underneath.

use crate::lyrics::language_supported;
use crate::phonetics::{EnglishPhonetics, PhoneticProvider};
use crate::rhyme::{classify, words_of};
use crate::syllable;

/// Scores for one block of lyric lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LyricScores {
    /// Syllables per line (chord markers stripped); blank lines score 0.
    pub line_syllables: Vec<usize>,
    /// Total over the block; `None` when the language is unsupported (A9:
    /// "no numbers" beats wrong numbers).
    pub syllables: Option<usize>,
    /// The dominant per-line count — the meter the block is written to.
    pub meter: Option<usize>,
    /// Per-line end-word rhyme classes as letters (`a`, `b`, …) in order
    /// of first appearance; a line with no words gets `""`. This is the
    /// deterministic "scheme" the preserve-rhyme grader compares.
    pub rhyme_scheme: Vec<String>,
}

/// Score one block of lyric lines under the given language (the caller
/// resolves the song/project language — never guessed here).
pub fn score_lines(lines: &[String], language: &str) -> LyricScores {
    if !language_supported(language) {
        return LyricScores {
            line_syllables: Vec::new(),
            syllables: None,
            meter: None,
            rhyme_scheme: Vec::new(),
        };
    }
    let phon = EnglishPhonetics::new();
    let counts: Vec<usize> = lines
        .iter()
        .map(|l| syllable::line_syllables(l, &phon))
        .collect();
    LyricScores {
        syllables: Some(counts.iter().sum()),
        meter: syllable::meter(&counts),
        line_syllables: counts,
        rhyme_scheme: rhyme_scheme(lines, &phon),
    }
}

/// End-word rhyme classes as letters (`a`..`z`, then `aa`, `ab`, …): each
/// line joins the first earlier class whose head end-word rhymes with it,
/// else starts the next class. Deterministic — classes are created in
/// first-appearance order.
pub fn rhyme_scheme(lines: &[String], provider: &dyn PhoneticProvider) -> Vec<String> {
    let mut heads: Vec<String> = Vec::new();
    let mut labels = Vec::new();
    for line in lines {
        let Some(word) = words_of(line).pop() else {
            labels.push(String::new());
            continue;
        };
        let matched = heads.iter().position(|head| {
            match (provider.pronounce(&word), provider.pronounce(head)) {
                (Some(a), Some(b)) => classify(&a, &b).is_some(),
                _ => false,
            }
        });
        let idx = match matched {
            Some(i) => i,
            None => {
                heads.push(word);
                heads.len() - 1
            }
        };
        labels.push(scheme_label(idx));
    }
    labels
}

/// Rhyme-class letter for class `idx`: `a`..`z`, then `aa`, `ab`, …
fn scheme_label(idx: usize) -> String {
    let letter = (b'a' + (idx % 26) as u8) as char;
    let laps: String = "abcdefghijklmnopqrstuvwxyz".repeat(idx / 26);
    format!("{laps}{letter}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::Lyrics;

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn scheme_labels_cross_rhyme_as_abab() {
        let phon = EnglishPhonetics::new();
        let scheme = rhyme_scheme(
            &lines(&[
                "The fire burned low",
                "I held the door",
                "nobody knows",
                "the night before",
            ]),
            &phon,
        );
        assert_eq!(scheme, ["a", "b", "a", "b"]);
    }

    #[test]
    fn repeated_end_word_shares_its_class() {
        let phon = EnglishPhonetics::new();
        let scheme = rhyme_scheme(&lines(&["keep the light", "hold the light"]), &phon);
        assert_eq!(scheme, ["a", "a"]);
    }

    #[test]
    fn blank_lines_label_empty() {
        let phon = EnglishPhonetics::new();
        let scheme = rhyme_scheme(&lines(&["rain on tin", "", "rain on tin"]), &phon);
        assert_eq!(scheme[0], "a");
        assert_eq!(scheme[1], "");
        assert_eq!(scheme[2], "a");
    }

    #[test]
    fn score_lines_counts_syllables_past_chords() {
        let scores = score_lines(&lines(&["[G]The fire burned low", "nobody knows"]), "en");
        assert_eq!(scores.line_syllables, [5, 4]);
        assert_eq!(scores.syllables, Some(9));
        assert_eq!(scores.meter, Some(5));
        assert_eq!(scores.rhyme_scheme, ["a", "a"]);
    }

    #[test]
    fn unsupported_language_reports_no_numbers_not_wrong_ones() {
        let scores = score_lines(&lines(&["du feu sous la table"]), "fr");
        assert_eq!(scores.syllables, None);
        assert_eq!(scores.meter, None);
        assert!(scores.rhyme_scheme.is_empty());
    }

    #[test]
    fn hook_line_is_the_first_chorus_line_without_chords() {
        let lyrics = Lyrics::parse(
            "# Song\n\n## Verse\n\nA line nobody keeps\n\n## Chorus\n\n[G]Keep the light on\n",
        )
        .unwrap();
        assert_eq!(
            lyrics.hook_line().as_deref(),
            Some("Keep the light on"),
            "chords never sit inside the hook"
        );
        let plain = Lyrics::parse("## Verse\n\nOnly a verse\n").unwrap();
        assert_eq!(plain.hook_line(), None);
    }
}
