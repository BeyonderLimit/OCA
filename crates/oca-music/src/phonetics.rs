//! Deterministic English phonetics (A9: `PhoneticProvider`).
//!
//! Lookup order: user `dictionary.yaml` → embedded common-word dictionary →
//! rule-based grapheme-to-phoneme (`g2p`). Everything is offline and
//! deterministic; there is no network and no statistical model.
//!
//! Phonemes use a compact ARPAbet-like set. Vowels carry a stress digit
//! (`0` unstressed, `1` primary, `2` secondary); consonants never do:
//!
//! ```text
//! VOWELS      AA AE AH AO AW AY EH ER EY IH IY OW OY UH UW
//! CONSONANTS  B CH D DH F G HH JH K L M N NG P R S SH T TH V W Y Z ZH
//! ```

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};

/// Embedded common-word dictionary (~350 entries): irregulars the rules
/// cannot get right (`love`, `one`, `through`) plus high-frequency song
/// vocabulary where precision matters.
const BUILTIN_EN: &str = include_str!("data/en.dict");

/// All vowel phoneme cores (no stress digit).
pub const VOWELS: &[&str] = &[
    "AA", "AE", "AH", "AO", "AW", "AY", "EH", "ER", "EY", "IH", "IY", "OW", "OY", "UH", "UW",
];

/// All consonant phoneme cores.
pub const CONSONANTS: &[&str] = &[
    "B", "CH", "D", "DH", "F", "G", "HH", "JH", "K", "L", "M", "N", "NG", "P", "R", "S", "SH", "T",
    "TH", "V", "W", "Y", "Z", "ZH",
];

/// `true` when `p` is a vowel phoneme, with or without a stress digit.
pub fn is_vowel(p: &str) -> bool {
    let core = p.trim_end_matches(['0', '1', '2']);
    VOWELS.contains(&core)
}

/// Split a phoneme into `(core, stress)` — `stress` is `None` on
/// consonants and unstressed vowels.
pub fn split_stress(p: &str) -> (&str, Option<u8>) {
    if p.len() >= 3 {
        let (core, digits) = p.split_at(p.len() - 1);
        if let Ok(d) = digits.parse::<u8>() {
            return (core, Some(d));
        }
    }
    (p, None)
}

/// Validate one phoneme token against the set above.
pub fn validate_phoneme(p: &str) -> Result<()> {
    let (core, stress) = split_stress(p);
    if !VOWELS.contains(&core) && !CONSONANTS.contains(&core) {
        bail!(
            "unknown phoneme {p:?} — allowed: {} {}",
            VOWELS.join(" "),
            CONSONANTS.join(" ")
        );
    }
    if is_vowel(p) && stress.is_none() {
        bail!("vowel {p:?} needs a stress digit (0, 1, or 2)");
    }
    if !is_vowel(p) && stress.is_some() {
        bail!("consonant {p:?} must not carry a stress digit");
    }
    Ok(())
}

/// One pronunciation: an ordered phoneme sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pronunciation {
    pub phonemes: Vec<String>,
}

impl Pronunciation {
    /// Parse a whitespace-separated respelling (`"K AE1 T"`), validating
    /// every token.
    pub fn parse(s: &str) -> Result<Self> {
        let phonemes: Vec<String> = s.split_whitespace().map(str::to_string).collect();
        if phonemes.is_empty() {
            bail!("empty pronunciation");
        }
        for p in &phonemes {
            validate_phoneme(p)?;
        }
        Ok(Self { phonemes })
    }

    /// Syllables = vowel phonemes.
    pub fn syllables(&self) -> usize {
        self.phonemes.iter().filter(|p| is_vowel(p)).count()
    }

    /// Indices of every vowel phoneme.
    pub fn vowel_indices(&self) -> Vec<usize> {
        self.phonemes
            .iter()
            .enumerate()
            .filter(|(_, p)| is_vowel(p))
            .map(|(i, _)| i)
            .collect()
    }

    /// The rhyme key: from the last primary-stressed vowel (fallback: the
    /// last vowel, or the whole tail when there are none) to the word end.
    /// Rhymes compare these keys — `day`/`stay` both key to `EY1 D`.
    pub fn rhyme_key(&self) -> &[String] {
        let vowels = self.vowel_indices();
        let start = vowels
            .iter()
            .rev()
            .find(|&&i| split_stress(&self.phonemes[i]).1 == Some(1))
            .copied()
            .or_else(|| vowels.last().copied())
            .unwrap_or(0);
        &self.phonemes[start..]
    }

    /// The last vowel phoneme (with stress), if any.
    pub fn final_vowel(&self) -> Option<&str> {
        self.vowel_indices()
            .last()
            .map(|&i| self.phonemes[i].as_str())
    }

    /// Phonemes strictly after the last vowel.
    pub fn coda(&self) -> &[String] {
        match self.vowel_indices().last() {
            Some(&i) => &self.phonemes[i + 1..],
            None => &[],
        }
    }

    /// Vowels inside the rhyme key (drives perfect vs multisyllabic).
    pub fn key_vowel_count(&self) -> usize {
        self.rhyme_key().iter().filter(|p| is_vowel(p)).count()
    }
}

/// One plug-in point for pronunciations (A9). English is the only
/// implementation in v1; others slot in behind the same trait.
pub trait PhoneticProvider {
    /// Pronounce one already-normalized word (lowercase, letters only);
    /// `None` only when the word has no pronounceable content.
    fn pronounce(&self, word: &str) -> Option<Pronunciation>;
}

/// English: user `dictionary.yaml` → embedded dictionary → rule fallback.
#[derive(Debug)]
pub struct EnglishPhonetics {
    user: HashMap<String, Pronunciation>,
    builtin: &'static HashMap<&'static str, &'static str>,
}

fn builtin_dict() -> &'static HashMap<&'static str, &'static str> {
    use std::sync::OnceLock;
    static DICT: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    DICT.get_or_init(|| {
        BUILTIN_EN
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter_map(|l| l.split_once(' '))
            .collect()
    })
}

impl Default for EnglishPhonetics {
    fn default() -> Self {
        Self::new()
    }
}

impl EnglishPhonetics {
    pub fn new() -> Self {
        Self {
            user: HashMap::new(),
            builtin: builtin_dict(),
        }
    }

    /// Build from a word → respelling map (the parsed `dictionary.yaml`),
    /// validating every entry.
    pub fn with_user_dictionary(map: HashMap<String, String>) -> Result<Self> {
        let mut user = HashMap::with_capacity(map.len());
        for (word, respelling) in map {
            let word_key = normalize(&word).context("dictionary key is not a word")?;
            if word_key.is_empty() {
                bail!("dictionary key {word:?} has no letters");
            }
            let pron = Pronunciation::parse(&respelling)
                .with_context(|| format!("pronunciation for {word:?}"))?;
            user.insert(word_key, pron);
        }
        Ok(Self {
            user,
            builtin: builtin_dict(),
        })
    }

    /// Load `<root>/dictionary.yaml` if present (invented names, fantasy
    /// words — A9); absent file means "no overrides".
    pub fn for_project(root: &Path) -> Result<Self> {
        let path = root.join("dictionary.yaml");
        if !path.exists() {
            return Ok(Self::new());
        }
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        Self::with_user_dictionary(parse_dictionary_yaml(&raw)?)
            .with_context(|| format!("dictionary.yaml in {}", root.display()))
    }
}

impl PhoneticProvider for EnglishPhonetics {
    fn pronounce(&self, word: &str) -> Option<Pronunciation> {
        let norm = normalize(word)?;
        if let Some(p) = self.user.get(&norm) {
            return Some(p.clone());
        }
        if let Some(&respelling) = self.builtin.get(norm.as_str()) {
            // Embedded entries are trusted; a parse error is a crate bug.
            return Some(
                Pronunciation::parse(respelling)
                    .unwrap_or_else(|e| panic!("en.dict entry {norm:?} is invalid: {e}")),
            );
        }
        Some(Pronunciation {
            phonemes: crate::g2p::guess(&norm),
        })
    }
}

/// Normalize a word token for lookup: lowercase, apostrophes removed
/// (`don't` → `dont`), everything non-alphabetic dropped.
pub fn normalize(word: &str) -> Option<String> {
    let out: String = word
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphabetic())
        .collect();
    if out.is_empty() { None } else { Some(out) }
}

/// Parse `dictionary.yaml`: flat `Word: P H O N E M E S` mapping. Comments
/// and blank lines are ignored; `language:`/meta keys are rejected so a
/// stray setting never becomes a "word" (language lives in `project.yaml`).
pub fn parse_dictionary_yaml(raw: &str) -> Result<HashMap<String, String>> {
    const META_KEYS: &[&str] = &["language", "type", "format_version"];
    let value: serde_yaml::Value =
        serde_yaml::from_str(raw).context("dictionary.yaml is not valid YAML")?;
    let map = value
        .as_mapping()
        .context("dictionary.yaml must be a mapping of word: pronunciation")?;
    let mut out = HashMap::new();
    for (k, v) in map {
        let key = k
            .as_str()
            .context("dictionary.yaml keys must be words")?
            .to_string();
        if META_KEYS.contains(&key.as_str()) {
            bail!(
                "dictionary.yaml must map words to pronunciations — \
                 {key:?} belongs in project.yaml"
            );
        }
        let value = v
            .as_str()
            .with_context(|| format!("pronunciation for {key:?} must be a single string"))?;
        out.insert(key, value.to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vowel_and_stress_helpers() {
        assert!(is_vowel("AE"));
        assert!(is_vowel("AE1"));
        assert!(!is_vowel("K"));
        assert_eq!(split_stress("AE1"), ("AE", Some(1)));
        assert_eq!(split_stress("K"), ("K", None));
    }

    #[test]
    fn pronounce_prefers_user_then_builtin_then_rules() {
        let mut user = HashMap::new();
        user.insert("lyra".to_string(), "L AY1 ER0".to_string());
        let p = EnglishPhonetics::with_user_dictionary(user).unwrap();
        assert_eq!(p.pronounce("Lyra").unwrap().phonemes, ["L", "AY1", "ER0"]);
        // builtin irregular, not rules
        assert_eq!(p.pronounce("love").unwrap().phonemes, ["L", "AH1", "V"]);
        // rule fallback: a regular CVC word
        let cat = p.pronounce("cat").unwrap();
        assert_eq!(cat.phonemes, ["K", "AE1", "T"]);
    }

    #[test]
    fn pronunciation_syllables_and_rhyme_key() {
        let ashes = Pronunciation::parse("AE1 SH IH0 Z").unwrap();
        assert_eq!(ashes.syllables(), 2);
        assert_eq!(ashes.rhyme_key(), ["AE1", "SH", "IH0", "Z"]);
        assert_eq!(ashes.final_vowel(), Some("IH0"));
        assert_eq!(ashes.coda(), ["Z"]);

        let fire = Pronunciation::parse("F AY1 ER0").unwrap();
        assert_eq!(fire.rhyme_key(), ["AY1", "ER0"]);
        assert_eq!(fire.key_vowel_count(), 2);
    }

    #[test]
    fn rhyme_key_falls_back_to_last_vowel_when_unstressed() {
        // "the" has no primary stress: key starts at its only vowel.
        let the = Pronunciation::parse("DH AH0").unwrap();
        assert_eq!(the.rhyme_key(), ["AH0"]);
    }

    #[test]
    fn dictionary_yaml_parses_and_rejects_meta_keys() {
        let map =
            parse_dictionary_yaml("Lyra: L AY1 ER0\n# comment\nBlackwood: B L AE1 K W UH2 D\n")
                .unwrap();
        assert_eq!(map["Lyra"], "L AY1 ER0");
        assert_eq!(map.len(), 2);

        let err = parse_dictionary_yaml("language: en\n").unwrap_err();
        assert!(err.to_string().contains("project.yaml"), "got: {err}");
    }

    #[test]
    fn user_dictionary_entries_are_validated() {
        let mut user = HashMap::new();
        user.insert("bad".to_string(), "Q X9".to_string());
        let err = EnglishPhonetics::with_user_dictionary(user).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("unknown phoneme"), "got: {msg}");
    }

    #[test]
    fn embedded_dict_entries_are_valid() {
        // every en.dict entry must parse — a bad entry would panic at
        // lookup time, so fail here instead
        for (word, respelling) in builtin_dict() {
            Pronunciation::parse(respelling)
                .unwrap_or_else(|e| panic!("en.dict entry {word:?} is invalid: {e}"));
        }
    }
}
