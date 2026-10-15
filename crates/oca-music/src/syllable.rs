//! Syllable counting and line metrics (rule 5). Counts come from the
//! phonetics pipeline: chord markers are stripped first, words split, each
//! word's syllables = its vowel phonemes. Deterministic, offline, and
//! stable enough for the TUI status bar and `oca song check`.

use crate::chord;
use crate::phonetics::PhoneticProvider;
use crate::rhyme::words_of;

/// Syllable counts for one lyric line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineMetrics {
    /// The line with chord markers removed.
    pub text: String,
    /// (word, syllables) in order.
    pub words: Vec<(String, usize)>,
    /// Inline chord names on this line, in order.
    pub chords: Vec<String>,
}

impl LineMetrics {
    pub fn syllables(&self) -> usize {
        self.words.iter().map(|(_, n)| n).sum()
    }
}

/// Analyze one lyric line: strip chords, split words, count syllables.
pub fn line_metrics(line: &str, provider: &dyn PhoneticProvider) -> LineMetrics {
    let (text, events) = chord::parse_line(line);
    let words: Vec<(String, usize)> = words_of(&text)
        .into_iter()
        .map(|w| {
            let n = provider.pronounce(&w).map(|p| p.syllables()).unwrap_or(0);
            (w, n)
        })
        .collect();
    LineMetrics {
        text,
        words,
        chords: events.into_iter().map(|e| e.chord).collect(),
    }
}

/// Syllables on a chord-stripped line (the hot path for the TUI status
/// bar — one line, no allocation beyond the strip).
pub fn line_syllables(line: &str, provider: &dyn PhoneticProvider) -> usize {
    let (text, _) = chord::parse_line(line);
    words_of(&text)
        .into_iter()
        .map(|w| provider.pronounce(&w).map(|p| p.syllables()).unwrap_or(0))
        .sum()
}

/// The mode (most common nonzero count) of a set of line counts — the
/// "target" meter a section aims at. Returns `None` for empty input.
pub fn meter(counts: &[usize]) -> Option<usize> {
    use std::collections::HashMap;
    let mut freq: HashMap<usize, usize> = HashMap::new();
    for &c in counts.iter().filter(|&&c| c > 0) {
        *freq.entry(c).or_default() += 1;
    }
    freq.into_iter()
        .max_by(|(ka, va), (kb, vb)| va.cmp(vb).then(ka.cmp(kb)))
        .map(|(k, _)| k)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phonetics::EnglishPhonetics;

    #[test]
    fn counts_syllables_per_word() {
        let p = EnglishPhonetics::new();
        let m = line_metrics("ashes on the water", &p);
        assert_eq!(m.syllables(), 2 + 1 + 1 + 2);
        assert_eq!(m.words[0], ("ashes".to_string(), 2));
    }

    #[test]
    fn chord_markers_do_not_count() {
        let p = EnglishPhonetics::new();
        // exit criterion: syllable count ignores chord markers
        let with_chords = line_metrics("[Dm]ashes on [F]the wa-ter", &p);
        let without = line_metrics("ashes on the wa-ter", &p);
        assert_eq!(with_chords.syllables(), without.syllables());
        assert_eq!(with_chords.chords, vec!["Dm", "F"]);
    }

    #[test]
    fn known_answer_fixture() {
        let p = EnglishPhonetics::new();
        // dictionary-backed words
        assert_eq!(line_syllables("love", &p), 1);
        assert_eq!(line_syllables("beautiful", &p), 3);
        assert_eq!(line_syllables("syllables", &p), 3);
        // rule-derived words
        assert_eq!(line_syllables("standing", &p), 2);
        assert_eq!(line_syllables("heartache", &p), 2);
        assert_eq!(line_syllables("tonight", &p), 2);
    }

    #[test]
    fn meter_picks_most_common() {
        assert_eq!(meter(&[8, 8, 7, 8]), Some(8));
        assert_eq!(meter(&[7, 7, 8, 8]), Some(8)); // tie → larger
        assert_eq!(meter(&[]), None);
        assert_eq!(meter(&[0, 0]), None);
    }
}
