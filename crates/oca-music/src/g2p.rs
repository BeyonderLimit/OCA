//! Rule-based grapheme-to-phoneme fallback (A9: dictionary + rules; this
//! is the rules half). Deterministic longest-match scan with a small number
//! of post-passes — magic `e`, final `-le`, `-ed`/`-ing`, and stress.
//!
//! Accuracy strategy: the embedded dictionary carries every irregular and
//! common word the tests care about; the rules cover regular morphology
//! (suffixes, CVC-e, digraphs) well enough to rhyme invented-but-regular
//! words ("starling" needs no dictionary). Known approximations are
//! documented at each rule.

use crate::phonetics::split_stress;

/// Guess phonemes for a normalized (lowercase alphabetic) word.
pub fn guess(word: &str) -> Vec<String> {
    let chars: Vec<char> = word.chars().collect();
    let (stem, magic) = strip_silent_e(&chars);
    let (mut out, cle_long) = scan(stem);
    if magic {
        longen_last_vowel(&mut out);
    }
    if cle_long {
        // the vowel *before* the -le coda longens ("table" EY, not the AH)
        longen_vowel_before_last(&mut out);
    }
    apply_stress(&mut out, word);
    out
}

/// Words ending in silent `e` lose it and longen the preceding vowel
/// ("make" M EY K). Words where the `e` is part of a final team (`-ee`,
/// `-oe`, `-ie`, `-ye`, `-ue`), the `-le` coda, the `-dge` stop, or the
/// r-colored `-ire`/`-are`/`-ere`/`-ore`/`-ure` family keep it — those are
/// handled by scan rules so the consonant and vowel quality stay right
/// ("bridge" keeps its JH, "fire" its AY ER).
fn strip_silent_e(w: &[char]) -> (&[char], bool) {
    let n = w.len();
    if n < 3 || w[n - 1] != 'e' {
        return (w, false);
    }
    let before = w[n - 2];
    if before == 'e' || before == 'o' || before == 'i' || before == 'y' || before == 'u' {
        return (w, false);
    }
    if before == 'l' && n >= 3 && !is_vowel_char(w[n - 3]) {
        return (w, false); // C-le coda handled by the scan
    }
    if before == 'g' && n >= 3 && w[n - 3] == 'd' {
        return (w, false); // -dge stop ("bridge", "judge")
    }
    if before == 'r' && n >= 3 && is_vowel_char(w[n - 3]) {
        return (w, false); // V-r-e family, see scan rules below
    }
    (&w[..n - 1], true)
}

fn is_vowel_char(c: char) -> bool {
    matches!(c, 'a' | 'e' | 'i' | 'o' | 'u' | 'y')
}

/// Long vowel quality for the magic-`e` / C-le post-pass.
fn long_vowel(core: &str) -> Vec<String> {
    match core {
        "AE" => vec!["EY".into()],
        "EH" => vec!["IY".into()],
        "IH" => vec!["AY".into()],
        "AA" => vec!["OW".into()],
        "AH" => vec!["Y".into(), "UW".into()], // "cute" KY UW T; "rule" approximates
        other => vec![other.to_string()],
    }
}

fn longen_last_vowel(out: &mut Vec<String>) {
    if let Some(&i) = vowel_indices(out).last() {
        longen_at(out, i);
    }
}

/// For the final `-le` case: longen the second-to-last vowel.
fn longen_vowel_before_last(out: &mut Vec<String>) {
    let vowels = vowel_indices(out);
    if let Some(&i) = vowels.get(vowels.len().saturating_sub(2)) {
        longen_at(out, i);
    }
}

fn vowel_indices(out: &[String]) -> Vec<usize> {
    out.iter()
        .enumerate()
        .filter(|(_, p)| crate::phonetics::is_vowel(p))
        .map(|(i, _)| i)
        .collect()
}

fn longen_at(out: &mut Vec<String>, i: usize) {
    let (core, stress) = split_stress(&out[i]);
    let mut long = long_vowel(core);
    if let Some(s) = stress {
        let last = long.len() - 1;
        long[last] = format!("{}{}", long[last], s);
    }
    out.splice(i..i + 1, long);
}

/// The grapheme scan: ordered longest-match patterns, suffix rules before
/// clusters before teams before single letters. Returns `(phonemes,
/// cle_long)` where `cle_long` asks the caller to longen the vowel before
/// a final `-le` coda after exactly one consonant ("table" EY vs "little" IH).
fn scan(w: &[char]) -> (Vec<String>, bool) {
    let mut out: Vec<String> = Vec::new();
    let n = w.len();
    let mut i = 0;
    // Consonants seen since the last vowel (drives the `-le` long/short call).
    let mut cons_since_vowel = 0usize;
    let mut cle_long = false;

    macro_rules! emit {
        ($($p:expr),* $(,)?) => { out.extend([$($p.to_string()),*]) };
    }
    macro_rules! consume {
        ($k:expr) => {{
            i += $k;
        }};
    }

    while i < n {
        let rest: String = w[i..].iter().collect();
        let s = rest.as_str();
        let final_len = n - i;

        // ---- suffixes with fixed pronunciations --------------------------
        if s.starts_with("tion") {
            emit!["SH", "AH", "N"];
            consume!(4);
            cons_since_vowel = 0;
            continue;
        }
        if s.starts_with("sion") || s.starts_with("cian") {
            emit!["ZH", "AH", "N"];
            consume!(4);
            cons_since_vowel = 0;
            continue;
        }
        if s.starts_with("cial") || s.starts_with("tial") {
            emit!["SH", "AH", "L"];
            consume!(4);
            cons_since_vowel = 0;
            continue;
        }
        if final_len == 4 && s.starts_with("ture") {
            emit!["CH", "ER"];
            consume!(4);
            cons_since_vowel = 0;
            continue;
        }
        if final_len == 3 && s.starts_with("ous") {
            emit!["AH", "S"];
            consume!(3);
            cons_since_vowel = 0;
            continue;
        }
        if final_len == 4 && (s.starts_with("ance") || s.starts_with("ence")) {
            emit!["AH", "N", "S"];
            consume!(4);
            cons_since_vowel = 0;
            continue;
        }
        if final_len == 3 && s.starts_with("ing") {
            emit!["IH", "NG"];
            consume!(3);
            cons_since_vowel = 0;
            continue;
        }
        if final_len == 2
            && s.starts_with("ed")
            && out.iter().any(|p| crate::phonetics::is_vowel(p))
        {
            let stem_last = i.checked_sub(1).and_then(|j| w.get(j).copied());
            match stem_last {
                Some('t') | Some('d') => emit!["IH", "D"],
                Some('p') | Some('k') | Some('f') | Some('s') | Some('h') | Some('c')
                | Some('x') => {
                    emit!["T"]
                }
                _ => emit!["D"],
            }
            consume!(2);
            continue;
        }

        // ---- clusters & digraphs -----------------------------------------
        if s.starts_with("igh") {
            emit!["AY"];
            consume!(3);
            cons_since_vowel = 0;
            continue;
        }
        if s.starts_with("tch") {
            emit!["CH"];
            consume!(3);
            cons_since_vowel = 1;
            continue;
        }
        if s.starts_with("dge") {
            emit!["JH"];
            consume!(3);
            cons_since_vowel = 1;
            continue;
        }
        if s.starts_with("sch") {
            emit!["S", "K"];
            consume!(3);
            cons_since_vowel = 2;
            continue;
        }
        if s.starts_with("air") {
            emit!["EH", "R"];
            consume!(3);
            cons_since_vowel = 0;
            continue;
        }
        if s.starts_with("eer") || s.starts_with("ear") {
            emit!["IY", "R"];
            consume!(3);
            cons_since_vowel = 0;
            continue;
        }
        // r-colored final `-e` family: the trailing `e` is silent but marks
        // the r-syllable ("fire" AY ER, "care" EH R, "more" AO R).
        if final_len == 3 && s.starts_with("ire") {
            emit!["AY", "ER"];
            consume!(3);
            cons_since_vowel = 0;
            continue;
        }
        if final_len == 3 && s.starts_with("are") {
            emit!["EH", "R"];
            consume!(3);
            cons_since_vowel = 0;
            continue;
        }
        if final_len == 3 && s.starts_with("ere") {
            emit!["IY", "R"];
            consume!(3);
            cons_since_vowel = 0;
            continue;
        }
        if final_len == 3 && s.starts_with("ore") {
            emit!["AO", "R"];
            consume!(3);
            cons_since_vowel = 0;
            continue;
        }
        if final_len == 3 && s.starts_with("ure") {
            emit!["Y", "UH", "R"];
            consume!(3);
            cons_since_vowel = 0;
            continue;
        }
        if s.starts_with("ch") {
            emit!["CH"];
            consume!(2);
            cons_since_vowel = 1;
            continue;
        }
        if s.starts_with("sh") {
            emit!["SH"];
            consume!(2);
            cons_since_vowel = 1;
            continue;
        }
        if s.starts_with("th") {
            emit!["TH"];
            consume!(2);
            cons_since_vowel = 1;
            continue;
        }
        if s.starts_with("ph") {
            emit!["F"];
            consume!(2);
            cons_since_vowel = 1;
            continue;
        }
        if s.starts_with("wh") {
            emit!["W"];
            consume!(2);
            cons_since_vowel = 1;
            continue;
        }
        if s.starts_with("ng") {
            emit!["NG"];
            consume!(2);
            cons_since_vowel = 1;
            continue;
        }
        if s.starts_with("nk") {
            // n before k is velar: "think" TH IH NG K
            emit!["NG", "K"];
            consume!(2);
            cons_since_vowel = 2;
            continue;
        }
        if s.starts_with("ck") {
            emit!["K"];
            consume!(2);
            cons_since_vowel = 1;
            continue;
        }
        if s.starts_with("qu") {
            emit!["K", "W"];
            consume!(2);
            cons_since_vowel = 2;
            continue;
        }
        if i == 0 && s.starts_with("kn") {
            emit!["N"];
            consume!(2);
            cons_since_vowel = 1;
            continue;
        }
        if i == 0 && s.starts_with("wr") {
            emit!["R"];
            consume!(2);
            cons_since_vowel = 1;
            continue;
        }
        if i == 0 && s.starts_with("gn") {
            emit!["N"];
            consume!(2);
            cons_since_vowel = 1;
            continue;
        }
        if final_len == 2 && s.starts_with("mb") {
            emit!["M"];
            consume!(2);
            cons_since_vowel = 1;
            continue;
        }
        if s.starts_with("gh") {
            if i == 0 {
                emit!["G"];
            } // word-initial ("ghost"); silent elsewhere ("night" via igh)
            consume!(2);
            cons_since_vowel += 1;
            continue;
        }

        // ---- vowel teams --------------------------------------------------
        let team = |out: &mut Vec<String>, phon: &[&str]| {
            out.extend(phon.iter().map(|p| p.to_string()));
        };
        let c = w[i];
        let c1 = w.get(i + 1).copied();
        let team_matched = match (c, c1) {
            ('a', Some('i')) | ('a', Some('y')) => {
                team(&mut out, &["EY"]);
                2
            }
            ('o', Some('a')) => {
                team(&mut out, &["OW"]);
                2
            }
            ('o', Some('o')) => {
                team(&mut out, &["UW"]);
                2
            }
            ('o', Some('u')) | ('o', Some('w')) => {
                team(&mut out, &["AW"]);
                2
            }
            ('o', Some('i')) | ('o', Some('y')) => {
                team(&mut out, &["OY"]);
                2
            }
            ('a', Some('u')) | ('a', Some('w')) => {
                team(&mut out, &["AO"]);
                2
            }
            ('e', Some('e')) | ('e', Some('a')) | ('i', Some('e')) | ('e', Some('y')) => {
                team(&mut out, &["IY"]);
                2
            }
            ('e', Some('w')) => {
                team(&mut out, &["UW"]);
                2
            }
            ('e', Some('i')) => {
                team(&mut out, &["EY"]);
                2
            }
            ('u', Some('e')) | ('u', Some('i')) | ('u', Some('w')) => {
                team(&mut out, &["UW"]);
                2
            }
            _ => 0,
        };
        if team_matched > 0 {
            i += team_matched;
            cons_since_vowel = 0;
            continue;
        }

        // ---- r-controlled --------------------------------------------------
        let rc = match (c, c1) {
            ('a', Some('r')) => Some(&["AA", "R"][..]),
            ('o', Some('r')) => Some(&["AO", "R"][..]),
            ('e', Some('r')) | ('i', Some('r')) | ('u', Some('r')) => Some(&["ER"][..]),
            _ => None,
        };
        if let Some(phon) = rc {
            // Only when r closes the syllable (end or before a consonant);
            // "springing" keeps r inside the first syllable via teams above.
            let after = w.get(i + 2).copied();
            if after.is_none_or(|a| !is_vowel_char(a)) {
                out.extend(phon.iter().map(|p| p.to_string()));
                i += 2;
                cons_since_vowel = phon.len() - 1;
                continue;
            }
        }

        // ---- final -le after a consonant -----------------------------------
        if c == 'l' && c1 == Some('e') && final_len == 2 && cons_since_vowel >= 1 {
            if cons_since_vowel == 1 {
                cle_long = true;
            }
            emit!["AH", "L"];
            consume!(2);
            cons_since_vowel = 2;
            continue;
        }

        // ---- y --------------------------------------------------------------
        if c == 'y' {
            let has_other_vowel = w[..i].iter().any(|&x| is_vowel_char(x));
            if i == 0 {
                emit!["Y"];
                cons_since_vowel = 1;
            } else if final_len == 1 {
                if has_other_vowel {
                    emit!["IY"];
                } else {
                    emit!["AY"];
                }
                cons_since_vowel = 0;
            } else {
                emit!["IH"];
                cons_since_vowel = 0;
            }
            consume!(1);
            continue;
        }

        // ---- geminates: doubled consonants are one sound -------------------
        // ("better" B EH T ER, "happy" HH AE P IY). The count still grows so
        // the -le long/short call sees real consonant depth ("little" short).
        if !is_vowel_char(c) && !matches!(c, 'h' | 'w' | 'y') && i > 0 && w[i - 1] == c {
            cons_since_vowel += 1;
            consume!(1);
            continue;
        }

        // ---- single letters ---------------------------------------------------
        match c {
            'a' => {
                // vowel before a latinate suffix or -ll keeps its broad
                // quality: "motion"/"nation" OW/EY, "song" AO, "call" AO.
                let after = &s[1..];
                if after.starts_with("tion")
                    || after.starts_with("sion")
                    || after.starts_with("cian")
                {
                    emit!["EY"];
                } else if after.starts_with("ll") {
                    emit!["AO"];
                } else {
                    emit!["AE"];
                }
                cons_since_vowel = 0;
            }
            'e' => {
                emit!["EH"];
                cons_since_vowel = 0;
            }
            'i' => {
                emit!["IH"];
                cons_since_vowel = 0;
            }
            'o' => {
                let after = &s[1..];
                if after.starts_with("tion") || after.starts_with("sion") {
                    emit!["OW"];
                } else if after.starts_with("ng") {
                    emit!["AO"];
                } else {
                    emit!["AA"];
                }
                cons_since_vowel = 0;
            }
            'u' => {
                emit!["AH"];
                cons_since_vowel = 0;
            }
            'b' => {
                emit!["B"];
                cons_since_vowel += 1;
            }
            'c' => {
                if matches!(c1, Some('e') | Some('i') | Some('y')) {
                    emit!["S"];
                } else {
                    emit!["K"];
                }
                cons_since_vowel += 1;
            }
            'd' => {
                emit!["D"];
                cons_since_vowel += 1;
            }
            'f' => {
                emit!["F"];
                cons_since_vowel += 1;
            }
            'g' => {
                if matches!(c1, Some('e') | Some('i') | Some('y')) {
                    emit!["JH"]; // soft g; exceptions ("girl", "get") live in en.dict
                } else {
                    emit!["G"];
                }
                cons_since_vowel += 1;
            }
            'h' => {
                emit!["HH"];
                cons_since_vowel += 1;
            }
            'j' => {
                emit!["JH"];
                cons_since_vowel += 1;
            }
            'k' => {
                emit!["K"];
                cons_since_vowel += 1;
            }
            'l' => {
                emit!["L"];
                cons_since_vowel += 1;
            }
            'm' => {
                emit!["M"];
                cons_since_vowel += 1;
            }
            'n' => {
                emit!["N"];
                cons_since_vowel += 1;
            }
            'p' => {
                emit!["P"];
                cons_since_vowel += 1;
            }
            'r' => {
                emit!["R"];
                cons_since_vowel += 1;
            }
            's' => {
                emit!["S"];
                cons_since_vowel += 1;
            }
            't' => {
                emit!["T"];
                cons_since_vowel += 1;
            }
            'v' => {
                emit!["V"];
                cons_since_vowel += 1;
            }
            'w' => {
                emit!["W"];
                cons_since_vowel += 1;
            }
            'x' => {
                emit!["K", "S"];
                cons_since_vowel += 2;
            }
            'z' => {
                emit!["Z"];
                cons_since_vowel += 1;
            }
            other => {
                // Non-ASCII letters (loanwords): best-effort vowel/consonant.
                emit![if is_vowel_char(other) { "AH" } else { "N" }];
                cons_since_vowel += 1;
            }
        }
        i += 1;
    }
    (out, cle_long)
}

/// Primary stress: first syllable by default; suffix rules move it
/// ("-tion" penult, "-ity" antepenult, "-ic"/"-ee" last). Function words
/// are all-unstressed — they live in `en.dict` with explicit `0` stress, so
/// the fallback path only ever sees content words.
fn apply_stress(out: &mut [String], word: &str) {
    let vowels: Vec<usize> = (0..out.len())
        .filter(|&i| crate::phonetics::is_vowel(&out[i]))
        .collect();
    if vowels.is_empty() {
        return;
    }
    let count = vowels.len();
    let pick = if word.ends_with("tion")
        || word.ends_with("sion")
        || word.ends_with("cian")
        || word.ends_with("cial")
        || word.ends_with("tial")
    {
        count.saturating_sub(2)
    } else if word.ends_with("ity") {
        count.saturating_sub(3)
    } else if word.ends_with("ic") || word.ends_with("ee") {
        count - 1
    } else {
        0
    };
    for (syl, &idx) in vowels.iter().enumerate() {
        let stress = if syl == pick { "1" } else { "0" };
        out[idx] = format!("{}{}", out[idx], stress);
    }
    // Schwa: an unstressed plain "o" reduces ("symbol" S IH M B AH L).
    for p in out.iter_mut() {
        if p == "AA0" {
            *p = "AH0".to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::guess;
    use crate::phonetics::Pronunciation;

    fn phon(word: &str) -> Vec<String> {
        Pronunciation {
            phonemes: guess(word),
        }
        .phonemes
    }

    #[test]
    fn cvc_words_and_digraphs() {
        assert_eq!(phon("cat"), ["K", "AE1", "T"]);
        assert_eq!(phon("ship"), ["SH", "IH1", "P"]);
        assert_eq!(phon("think"), ["TH", "IH1", "NG", "K"]);
        assert_eq!(phon("match"), ["M", "AE1", "CH"]);
        assert_eq!(phon("bridge"), ["B", "R", "IH1", "JH"]);
    }

    #[test]
    fn magic_e_longens() {
        assert_eq!(phon("make"), ["M", "EY1", "K"]);
        assert_eq!(phon("time"), ["T", "AY1", "M"]);
        assert_eq!(phon("hope"), ["HH", "OW1", "P"]);
        assert_eq!(phon("cute"), ["K", "Y", "UW1", "T"]);
    }

    #[test]
    fn silent_final_e_is_not_a_syllable() {
        assert_eq!(
            Pronunciation {
                phonemes: guess("make")
            }
            .syllables(),
            1
        );
        assert_eq!(
            Pronunciation {
                phonemes: guess("table")
            }
            .syllables(),
            2
        );
    }

    #[test]
    fn final_le_long_vs_short() {
        // exactly one consonant before -le → long vowel
        assert_eq!(phon("maple"), ["M", "EY1", "P", "AH0", "L"]);
        // two+ → short
        assert_eq!(phon("little"), ["L", "IH1", "T", "AH0", "L"]);
        assert_eq!(phon("apple"), ["AE1", "P", "AH0", "L"]);
    }

    #[test]
    fn ed_endings() {
        assert_eq!(phon("jumped"), ["JH", "AH1", "M", "P", "T"]);
        assert_eq!(phon("planted"), ["P", "L", "AE1", "N", "T", "IH0", "D"]);
        assert_eq!(phon("played"), ["P", "L", "EY1", "D"]);
        // stem ending in t/d keeps the full -ed syllable
        assert_eq!(phon("wanted"), ["W", "AE1", "N", "T", "IH0", "D"]);
    }

    #[test]
    fn ing_and_tion_suffixes() {
        assert_eq!(phon("standing"), ["S", "T", "AE1", "N", "D", "IH0", "NG"]);
        // -tion stresses the penult
        assert_eq!(phon("motion"), ["M", "OW1", "SH", "AH0", "N"]);
    }

    #[test]
    fn y_rules() {
        assert_eq!(phon("sky"), ["S", "K", "AY1"]);
        assert_eq!(phon("happy"), ["HH", "AE1", "P", "IY0"]);
        assert_eq!(phon("symbol"), ["S", "IH1", "M", "B", "AH0", "L"]);
    }

    #[test]
    fn r_controlled() {
        assert_eq!(phon("star"), ["S", "T", "AA1", "R"]);
        assert_eq!(phon("better"), ["B", "EH1", "T", "ER0"]);
        // r stays with the syllable when a vowel follows ("springing")
        let springing = phon("springing");
        assert_eq!(springing, ["S", "P", "R", "IH1", "NG", "IH0", "NG"]);
    }

    #[test]
    fn word_initial_kn_wr() {
        assert_eq!(phon("knew"), ["N", "UW1"]);
        assert_eq!(phon("wrong"), ["R", "AO1", "NG"]);
    }

    #[test]
    fn soft_c_and_g_with_dict_exceptions() {
        assert_eq!(phon("city"), ["S", "IH1", "T", "IY0"]);
        assert_eq!(phon("gentle"), ["JH", "EH1", "N", "T", "AH0", "L"]);
        // "girl" is an irregular soft g — the dictionary (not these rules)
        // overrides the JH; this is the provider-level behavior.
        let p = crate::phonetics::EnglishPhonetics::new();
        use crate::phonetics::PhoneticProvider;
        assert_eq!(p.pronounce("girl").unwrap().phonemes, ["G", "ER1", "L"]);
    }
}
