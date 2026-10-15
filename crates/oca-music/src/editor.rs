//! Live metrics for the TUI song editor: section navigation and the
//! status-bar readouts. Every number is computed here so `oca-tui` stays
//! thin (rule 8) and what the status bar shows matches `oca song check`.

use crate::lyrics::{Lyrics, Section, language_supported};
use crate::phonetics::PhoneticProvider;
use crate::syllable;

/// One `## ` heading as a navigation target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionMark {
    /// Heading text as authored (`Verse 2`, `Chorus 2 {=chorus}`).
    pub label: String,
    /// 1-based heading line.
    pub line: u32,
    /// Char offset of the heading line's start — where the cursor jumps.
    pub char_offset: usize,
    pub is_reference: bool,
}

/// Every section heading in file order (what `Alt-]` / `Alt-[` steps
/// through). Returns an empty list for text that does not parse.
pub fn section_marks(raw: &str) -> Vec<SectionMark> {
    let Ok(lyrics) = Lyrics::parse(raw) else {
        return Vec::new();
    };
    let line_offsets = line_byte_offsets(raw);
    lyrics
        .sections
        .iter()
        .map(|s| SectionMark {
            label: s.label(),
            line: s.start_line,
            char_offset: char_at_byte(
                raw,
                line_offsets[(s.start_line as usize - 1).min(line_offsets.len() - 1)],
            ),
            is_reference: s.reference_to.is_some(),
        })
        .collect()
}

/// The status-bar numbers for one cursor position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorMetrics {
    /// Section containing the cursor (`None` before the first heading).
    pub section: Option<String>,
    /// Syllables on the cursor's line; `None` for a heading/blank line or
    /// an unsupported language (A9: no numbers beats wrong numbers).
    pub line_syllables: Option<usize>,
    /// Syllables in the containing section — a `{=chorus}` repeat counts
    /// its referent's lines. `None` for an unsupported language.
    pub section_syllables: Option<usize>,
    /// `false` → the language has no syllable support; the numeric fields
    /// above are `None` rather than wrong.
    pub language_supported: bool,
}

/// Compute the status-bar metrics for `char_off` (the editor's cursor unit)
/// inside the song's lyrics text. Deterministic and offline — the same
/// phonetics pipeline as `oca song check`, applied to the live buffer.
pub fn cursor_metrics(
    raw: &str,
    char_off: usize,
    phon: &dyn PhoneticProvider,
    language: &str,
) -> CursorMetrics {
    let supported = language_supported(language);
    let empty = CursorMetrics {
        section: None,
        line_syllables: None,
        section_syllables: None,
        language_supported: supported,
    };
    let Ok(lyrics) = Lyrics::parse(raw) else {
        return empty;
    };
    let line_no = raw[..byte_at_char(raw, char_off).min(raw.len())]
        .matches('\n')
        .count() as u32
        + 1;
    let Some(section) = containing_section(&lyrics.sections, line_no) else {
        return empty;
    };
    let (line_syllables, section_syllables) = if supported {
        (
            section
                .lines
                .iter()
                .find(|l| l.line_no == line_no)
                .map(|l| syllable::line_syllables(&l.raw, phon)),
            Some(
                section
                    .effective_lines(&lyrics.sections)
                    .iter()
                    .map(|l| syllable::line_syllables(&l.raw, phon))
                    .sum(),
            ),
        )
    } else {
        (None, None)
    };
    CursorMetrics {
        section: Some(section.label()),
        line_syllables,
        section_syllables,
        language_supported: supported,
    }
}

/// Last heading at or before `line_no`.
fn containing_section(sections: &[Section], line_no: u32) -> Option<&Section> {
    sections
        .iter()
        .rposition(|s| s.start_line <= line_no)
        .map(|i| &sections[i])
}

/// Byte offset of the start of each line (index 0 = line 1).
fn line_byte_offsets(raw: &str) -> Vec<usize> {
    let mut offs = vec![0usize];
    for (i, ch) in raw.char_indices() {
        if ch == '\n' {
            offs.push(i + ch.len_utf8());
        }
    }
    offs
}

fn byte_at_char(raw: &str, char_off: usize) -> usize {
    raw.char_indices()
        .nth(char_off)
        .map(|(i, _)| i)
        .unwrap_or(raw.len())
}

fn char_at_byte(raw: &str, byte_off: usize) -> usize {
    raw[..byte_off.min(raw.len())].chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phonetics::EnglishPhonetics;

    const RAW: &str = "\
# Song

## Verse 1
rain on the wire

## Chorus
hold on to the light

## Chorus 2 {=chorus}
";

    #[test]
    fn marks_list_every_heading_with_jump_offsets() {
        let marks = section_marks(RAW);
        assert_eq!(marks.len(), 3);
        assert_eq!(marks[0].label, "Verse 1");
        assert_eq!(marks[0].line, 3);
        assert!(!marks[0].is_reference);
        assert_eq!(marks[2].label, "Chorus 2");
        assert!(marks[2].is_reference);
        for m in &marks {
            let at: String = RAW.chars().skip(m.char_offset).take(3).collect();
            assert_eq!(at, "## ", "mark {} does not point at its heading", m.label);
        }
    }

    #[test]
    fn metrics_count_the_line_and_its_section() {
        let phon = EnglishPhonetics::new();
        // Cursor on "hold on to the light" (line 6).
        let off = RAW.find("hold").unwrap();
        let m = cursor_metrics(RAW, off, &phon, "en");
        assert_eq!(m.section.as_deref(), Some("Chorus"));
        assert_eq!(m.line_syllables, Some(5)); // hold-on-to-the-light
        assert_eq!(m.section_syllables, Some(5));
        assert!(m.language_supported);
    }

    #[test]
    fn metrics_on_a_heading_have_no_line_count() {
        let phon = EnglishPhonetics::new();
        let off = RAW.find("## Chorus").unwrap();
        let m = cursor_metrics(RAW, off, &phon, "en");
        assert_eq!(m.section.as_deref(), Some("Chorus"));
        assert_eq!(m.line_syllables, None);
    }

    #[test]
    fn unsupported_language_reports_no_numbers() {
        let phon = EnglishPhonetics::new();
        let off = RAW.find("rain").unwrap();
        let m = cursor_metrics(RAW, off, &phon, "fr");
        assert!(!m.language_supported);
        assert_eq!(m.line_syllables, None);
        assert_eq!(m.section_syllables, None);
        assert_eq!(m.section.as_deref(), Some("Verse 1"));
    }

    #[test]
    fn empty_marks_for_unparseable_text() {
        assert!(section_marks("").is_empty());
    }
}
