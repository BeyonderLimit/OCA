//! Mention detection (A11): word-boundary match on a name or alias.
//! Documented limits: case-sensitive, and pronouns/anaphora ("she") are not
//! resolved.

/// True when `text` contains `term` bounded by non-alphanumeric characters
/// (or the edges of the text): `Elena` matches "Elena walked" and "Elena's"
/// but not "Elenara" or "elena".
pub fn mentions(text: &str, term: &str) -> bool {
    if term.is_empty() {
        return false;
    }
    text.match_indices(term).any(|(i, _)| {
        let before_ok = text[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        let after_ok = text[i + term.len()..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric());
        before_ok && after_ok
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_at_word_boundaries_and_edges() {
        assert!(mentions("Elena walked home.", "Elena"));
        assert!(mentions("She met Elena.", "Elena"));
        assert!(mentions("Elena's letter", "Elena"));
        assert!(mentions("Elena", "Elena"));
    }

    #[test]
    fn rejects_substrings_regardless_of_case() {
        assert!(!mentions("Elenara's shrine", "Elena"));
        assert!(!mentions("Elenas", "Elena"));
        assert!(!mentions("elena walked home", "Elena"), "case-sensitive");
    }

    #[test]
    fn multiword_alias_respects_outer_boundaries() {
        assert!(mentions("the girl from the village", "the girl"));
        assert!(!mentions("othegirl from the village", "the girl"));
    }

    #[test]
    fn empty_term_never_matches() {
        assert!(!mentions("anything", ""));
    }
}
