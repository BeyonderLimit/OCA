//! `lyrics.md` — the single source of truth for song order and content
//! (A11/D4). Headings are the section model:
//!
//! ```markdown
//! # Ashes
//!
//! ## Verse 1
//! ...
//! ## Chorus
//! ...
//! ## Chorus 2 {=chorus}     <- reference: repeats the original chorus
//! ```
//!
//! A `## Name` heading's canonical token is its lowercase name with spaces
//! and hyphens folded to `_` and trailing numbering stripped, so the plan
//! in `song.yaml` (`[verse, chorus, final_chorus]`) compares 1:1 against
//! the headings. Section kinds (Intro/Verse/Pre-Chorus/Chorus/Post-Chorus/
//! Bridge/Breakdown/Outro/Interlude, plus the "Final Chorus" alias) drive
//! structure validation; unknown names are reported, not rejected.

use anyhow::{Context, Result};
use oca_markdown::parse;
use oca_model::Block;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionKind {
    Intro,
    Verse,
    PreChorus,
    Chorus,
    PostChorus,
    Bridge,
    Breakdown,
    Outro,
    Interlude,
}

impl SectionKind {
    /// The nine supported section kinds; `Final Chorus` is accepted as an
    /// alias of `Chorus` (its canonical token stays `final_chorus` so the
    /// plan can call it out separately).
    pub fn from_canonical(canonical: &str) -> Option<Self> {
        Some(match canonical {
            "intro" => SectionKind::Intro,
            "verse" => SectionKind::Verse,
            "pre_chorus" => SectionKind::PreChorus,
            "chorus" | "final_chorus" => SectionKind::Chorus,
            "post_chorus" => SectionKind::PostChorus,
            "bridge" => SectionKind::Bridge,
            "breakdown" => SectionKind::Breakdown,
            "outro" => SectionKind::Outro,
            "interlude" => SectionKind::Interlude,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            SectionKind::Intro => "Intro",
            SectionKind::Verse => "Verse",
            SectionKind::PreChorus => "Pre-Chorus",
            SectionKind::Chorus => "Chorus",
            SectionKind::PostChorus => "Post-Chorus",
            SectionKind::Bridge => "Bridge",
            SectionKind::Breakdown => "Breakdown",
            SectionKind::Outro => "Outro",
            SectionKind::Interlude => "Interlude",
        }
    }
}

/// One lyric line (chords included verbatim; analysis strips them).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LyricLine {
    /// 1-based file line number.
    pub line_no: u32,
    pub raw: String,
}

/// One `## …` section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    /// Heading text exactly as authored (minus the `{=ref}` attribute).
    pub heading: String,
    /// Base name without instance number: `Verse`.
    pub base: String,
    pub number: Option<u32>,
    /// Parsed kind; `None` when the name is outside the supported set.
    pub kind: Option<SectionKind>,
    /// Plan/drift token: `verse`, `pre_chorus`, `final_chorus`, …
    pub canonical: String,
    /// `{=chorus}` reference target, if this section repeats an earlier one.
    pub reference_to: Option<String>,
    /// Heading line (1-based) — the TUI navigates sections by it.
    pub start_line: u32,
    pub lines: Vec<LyricLine>,
}

impl Section {
    /// The section this one repeats, resolved against earlier sections
    /// (first non-reference section with the same canonical token).
    pub fn referent<'s>(&'s self, sections: &'s [Section]) -> Option<&'s Section> {
        let target = self.reference_to.as_ref()?;
        sections
            .iter()
            .take_while(|s| !std::ptr::eq(*s, self))
            .filter(|s| s.reference_to.is_none() && s.canonical == *target)
            .last()
    }

    /// Lines to analyze: its own, or the referent's when it's a repeat.
    pub fn effective_lines<'s>(&'s self, sections: &'s [Section]) -> &'s [LyricLine] {
        match self.referent(sections) {
            Some(target) => &target.lines,
            None => &self.lines,
        }
    }

    /// Display label: `Verse 2`, `Final Chorus`.
    pub fn label(&self) -> String {
        match self.number {
            Some(n) => format!("{} {}", self.base, n),
            None => self.base.clone(),
        }
    }
}

/// Parsed `lyrics.md`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lyrics {
    /// The `# Title` heading, when present.
    pub title: Option<String>,
    pub sections: Vec<Section>,
}

impl Lyrics {
    /// Parse full file bytes. Parsing never rewrites (oca-markdown
    /// round-trip invariant); the TUI saves raw body edits.
    pub fn parse(raw: &str) -> Result<Self> {
        let doc = parse(raw);
        let raw_lines: Vec<&str> = raw.lines().collect();

        // (line_no, level, text) for h1/h2 in document order.
        let mut heads: Vec<(u32, u8, String)> = Vec::new();
        for block in &doc.blocks {
            if let Block::Heading { level, text, lines } = block
                && *level <= 2
            {
                heads.push((lines.start, *level, text.trim().to_string()));
            }
        }
        let title = heads
            .iter()
            .find(|(_, level, _)| *level == 1)
            .map(|(_, _, text)| text.clone());

        let mut sections = Vec::new();
        for (idx, &(line_no, level, ref text)) in heads.iter().enumerate() {
            if level != 2 {
                continue;
            }
            let body_start = line_no as usize; // 0-based line after heading
            let body_end = heads
                .get(idx + 1)
                .map(|(next, _, _)| (*next as usize).saturating_sub(1))
                .unwrap_or(raw_lines.len());
            let lines: Vec<LyricLine> = raw_lines
                .get(body_start..body_end.max(body_start))
                .unwrap_or(&[])
                .iter()
                .enumerate()
                .filter(|(_, l)| !l.trim().is_empty())
                .map(|(i, l)| LyricLine {
                    line_no: body_start as u32 + 1 + i as u32,
                    raw: (*l).to_string(),
                })
                .collect();
            let (heading, reference_to) = split_reference(text);
            let (base, number) = split_number(&heading);
            let canonical = canonical_of(&base);
            sections.push(Section {
                heading,
                base,
                number,
                kind: SectionKind::from_canonical(&canonical),
                canonical,
                reference_to,
                start_line: line_no,
                lines,
            });
        }
        Ok(Self { title, sections })
    }
}

/// `Chorus 2 {=chorus}` → (`Chorus 2`, Some("chorus")).
fn split_reference(text: &str) -> (String, Option<String>) {
    match text.rsplit_once("{=") {
        Some((head, tail)) if tail.ends_with('}') => (
            head.trim_end().to_string(),
            Some(tail.trim_end_matches('}').trim().to_lowercase()),
        ),
        _ => (text.to_string(), None),
    }
}

/// `Verse 12` → (`Verse`, Some(12)).
fn split_number(text: &str) -> (String, Option<u32>) {
    match text.rsplit_once(' ') {
        Some((head, tail)) if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => {
            (head.trim_end().to_string(), tail.parse().ok())
        }
        _ => (text.to_string(), None),
    }
}

/// `Pre-Chorus` → `pre_chorus`; `Final Chorus` → `final_chorus`.
pub fn canonical_of(base: &str) -> String {
    base.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

/// Language support gate (A9): lyric tools report "unsupported" rather
/// than wrong numbers.
pub fn language_supported(code: &str) -> bool {
    matches!(code, "en")
}

/// Parse with context for error messages.
pub fn parse_lyrics(raw: &str) -> Result<Lyrics> {
    Lyrics::parse(raw).context("parse lyrics.md")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# Ashes

## Verse 1
line one of verse
line two of verse

## Pre-Chorus
pre line

## Chorus
the chorus goes here

## Final Chorus
final chorus words

## Chorus 2 {=chorus}

## Bridge
bridge words

## Whatever
unknown section
";

    #[test]
    fn sections_and_kinds() {
        let l = Lyrics::parse(SAMPLE).unwrap();
        assert_eq!(l.title.as_deref(), Some("Ashes"));
        let c: Vec<&str> = l.sections.iter().map(|s| s.canonical.as_str()).collect();
        assert_eq!(
            c,
            vec![
                "verse",
                "pre_chorus",
                "chorus",
                "final_chorus",
                "chorus",
                "bridge",
                "whatever"
            ]
        );
        assert_eq!(l.sections[0].kind, Some(SectionKind::Verse));
        assert_eq!(l.sections[3].kind, Some(SectionKind::Chorus)); // alias
        assert_eq!(l.sections[6].kind, None); // unknown
        assert_eq!(l.sections[0].number, Some(1));
        assert_eq!(l.sections[0].label(), "Verse 1");
    }

    #[test]
    fn body_lines_carry_line_numbers() {
        let l = Lyrics::parse(SAMPLE).unwrap();
        let v = &l.sections[0];
        assert_eq!(v.lines.len(), 2);
        assert_eq!(v.lines[0].line_no, 4);
        assert_eq!(v.lines[0].raw, "line one of verse");
    }

    #[test]
    fn reference_sections_resolve_to_the_original() {
        let l = Lyrics::parse(SAMPLE).unwrap();
        let repeat = &l.sections[4];
        assert_eq!(repeat.reference_to.as_deref(), Some("chorus"));
        let target = repeat.referent(&l.sections).unwrap();
        assert_eq!(target.canonical, "chorus");
        assert_eq!(target.number, None);
        // effective lines are the original's (empty reference body)
        assert_eq!(repeat.effective_lines(&l.sections).len(), 1);
    }

    #[test]
    fn missing_referent_resolves_to_none() {
        let raw = "# X\n\n## Chorus 2 {=verse}\n\ntext\n";
        let l = Lyrics::parse(raw).unwrap();
        assert!(l.sections[0].referent(&l.sections).is_none());
    }

    #[test]
    fn canonical_folds_spaces_and_hyphens() {
        assert_eq!(canonical_of("Pre-Chorus"), "pre_chorus");
        assert_eq!(canonical_of("Final Chorus"), "final_chorus");
        assert_eq!(canonical_of("Post-Chorus"), "post_chorus");
    }

    #[test]
    fn no_headings_is_fine() {
        let l = Lyrics::parse("just some words\n").unwrap();
        assert!(l.title.is_none());
        assert!(l.sections.is_empty());
    }
}
