//! Rhyme classification (§6.2). All comparisons run on `Pronunciation`
//! rhyme keys — from the last primary-stressed vowel to the word end — so
//! `day`/`stay` match on `EY1 D` regardless of onset.
//!
//! The categories are deterministic and disjoint by construction, checked
//! in this order:
//!
//! 1. **Multisyllabic** — keys identical, ≥2 vowels in the key
//!    (`fire`/`higher`).
//! 2. **Perfect** — keys identical, one vowel (`day`/`stay`).
//! 3. **Assonance** — last vowel identical, codas differ (`day`/`great`).
//! 4. **Consonance** — codas identical (non-empty), last vowels differ
//!    (`luck`/`lock`, `cold`/`called`).
//! 5. **Slant** — keys same length, differ in exactly one phoneme, but
//!    neither vowel nor coda is shared outright (`fallow`/`mellow`).
//!
//! "Internal" rhyme is a placement, not a sound relation: it reports a
//! rhyming pair where the second word sits inside the line (not at its
//! end). Word lists are supplied by the caller, so the same engine serves
//! CLI queries, tests, and (Phase 9+) AI candidate generation.

use crate::phonetics::{PhoneticProvider, Pronunciation, is_vowel, split_stress};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RhymeKind {
    Multisyllabic,
    Perfect,
    Assonance,
    Consonance,
    Slant,
}

impl RhymeKind {
    pub fn label(self) -> &'static str {
        match self {
            RhymeKind::Multisyllabic => "multisyllabic",
            RhymeKind::Perfect => "perfect",
            RhymeKind::Assonance => "assonance",
            RhymeKind::Consonance => "consonance",
            RhymeKind::Slant => "slant",
        }
    }
}

/// Classify the relation between two pronunciations; `None` = not a rhyme
/// under any category.
pub fn classify(a: &Pronunciation, b: &Pronunciation) -> Option<RhymeKind> {
    let ka = a.rhyme_key();
    let kb = b.rhyme_key();
    if ka.is_empty() || kb.is_empty() {
        return None;
    }
    if ka == kb {
        return if a.key_vowel_count() >= 2 {
            Some(RhymeKind::Multisyllabic)
        } else {
            Some(RhymeKind::Perfect)
        };
    }
    let va = a.final_vowel();
    let vb = b.final_vowel();
    let ca = a.coda();
    let cb = b.coda();
    if va == vb && ca != cb {
        return Some(RhymeKind::Assonance);
    }
    if !ca.is_empty() && ca == cb && va != vb {
        return Some(RhymeKind::Consonance);
    }
    if ka.len() == kb.len() && ka.iter().zip(kb.iter()).filter(|(x, y)| x != y).count() == 1 {
        return Some(RhymeKind::Slant);
    }
    None
}

/// One rhyming pair with its kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RhymePair {
    pub word: String,
    pub kind: RhymeKind,
}

/// Find every candidate that rhymes with `word`, best kinds first.
pub fn rhymes_with<P: PhoneticProvider>(
    word: &str,
    candidates: &[String],
    provider: &P,
) -> Vec<RhymePair> {
    let Some(target) = provider.pronounce(word) else {
        return Vec::new();
    };
    let mut out: Vec<RhymePair> = candidates
        .iter()
        .filter(|c| !word.eq_ignore_ascii_case(c))
        .filter_map(|c| {
            provider.pronounce(c).and_then(|p| {
                classify(&target, &p).map(|kind| RhymePair {
                    word: c.clone(),
                    kind,
                })
            })
        })
        .collect();
    out.sort_by(|a, b| (a.kind, &a.word).cmp(&(b.kind, &b.word)));
    out
}

/// A rhyme between two words inside one line (line placement, not sound).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InternalRhyme {
    pub first: String,
    pub second: String,
    pub kind: RhymeKind,
}

/// All rhyming pairs of distinct words inside one line (already
/// chord-stripped, whitespace-separated).
pub fn internal_rhymes<P: PhoneticProvider>(words: &[String], provider: &P) -> Vec<InternalRhyme> {
    let prons: Vec<(usize, Pronunciation)> = words
        .iter()
        .enumerate()
        .filter_map(|(i, w)| provider.pronounce(w).map(|p| (i, p)))
        .collect();
    let mut out = Vec::new();
    for (ai, a) in &prons {
        for (bi, b) in &prons {
            if bi <= ai {
                continue;
            }
            if let Some(kind) = classify(a, b) {
                out.push(InternalRhyme {
                    first: words[*ai].clone(),
                    second: words[*bi].clone(),
                    kind,
                });
            }
        }
    }
    out
}

/// Split a chord-stripped lyric line into pronounceable words: any
/// non-alphabetic character separates (`stop—believing` → two words,
/// `broken-hearted` → two words), apostrophes are removed
/// (`don't` → `dont`).
pub fn words_of(line: &str) -> Vec<String> {
    line.split_whitespace()
        .flat_map(|tok| {
            tok.split(|c: char| !c.is_alphabetic() && c != '\'')
                .map(|w| w.replace('\'', ""))
                .collect::<Vec<_>>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Pronounce via the provider, applying the vowel-stress view used by the
/// rhyme key (helper for callers that only need syllable counts).
pub fn syllables_of<P: PhoneticProvider>(word: &str, provider: &P) -> usize {
    provider.pronounce(word).map(|p| p.syllables()).unwrap_or(0)
}

/// `true` when the phoneme is a vowel carrying any stress (test helper).
pub fn stressed_vowel(p: &str) -> bool {
    is_vowel(p) && split_stress(p).1.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phonetics::EnglishPhonetics;

    fn p(word: &str) -> Pronunciation {
        EnglishPhonetics::new().pronounce(word).unwrap()
    }

    #[test]
    fn perfect_and_multisyllabic() {
        assert_eq!(classify(&p("day"), &p("stay")), Some(RhymeKind::Perfect));
        assert_eq!(classify(&p("light"), &p("night")), Some(RhymeKind::Perfect));
        assert_eq!(
            classify(&p("fire"), &p("higher")),
            Some(RhymeKind::Multisyllabic)
        );
        // onsets never matter
        assert_eq!(classify(&p("stay"), &p("day")), Some(RhymeKind::Perfect));
    }

    #[test]
    fn assonance_and_consonance() {
        assert_eq!(classify(&p("day"), &p("great")), Some(RhymeKind::Assonance));
        assert_eq!(
            classify(&p("luck"), &p("lock")),
            Some(RhymeKind::Consonance)
        );
        assert_eq!(
            classify(&p("cold"), &p("called")),
            Some(RhymeKind::Consonance)
        );
    }

    #[test]
    fn slant_fires_when_vowel_and_coda_both_differ_once() {
        assert_eq!(classify(&p("fallow"), &p("mellow")), Some(RhymeKind::Slant));
    }

    #[test]
    fn non_rhymes_are_none() {
        assert_eq!(classify(&p("cat"), &p("dog")), None);
        assert_eq!(classify(&p("day"), &p("luck")), None);
        assert_eq!(classify(&p("love"), &p("song")), None);
    }

    #[test]
    fn internal_rhyme_inside_a_line() {
        // "dream" rhymes with "stream" inside one line
        let words = words_of("I could dream in the midnight stream");
        let got = internal_rhymes(&words, &EnglishPhonetics::new());
        assert!(
            got.iter().any(|r| {
                (r.first == "dream" && r.second == "stream")
                    || (r.first == "stream" && r.second == "dream")
            }),
            "got: {got:?}"
        );
    }

    #[test]
    fn rhymes_with_ranks_best_first() {
        let cands: Vec<String> = ["stay", "away", "lock", "fire"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let got = rhymes_with("day", &cands, &EnglishPhonetics::new());
        // "away" and "stay" both key to EY1 — perfect rhymes; ties order
        // alphabetically. "lock" is a consonance of "day"? no: K vs no coda.
        assert_eq!(got[0].word, "away");
        assert_eq!(got[0].kind, RhymeKind::Perfect);
        assert_eq!(got[1].word, "stay");
        assert_eq!(got[1].kind, RhymeKind::Perfect);
        assert!(got.iter().all(|r| r.word != "lock"));
    }

    #[test]
    fn words_of_splits_and_trims() {
        assert_eq!(
            words_of("Don't stop—believing!"),
            vec!["Dont", "stop", "believing"]
        );
        assert_eq!(words_of("broken-hearted"), vec!["broken", "hearted"]);
    }
}
