//! `oca song check` — deterministic song report: plan drift (D4/A11),
//! section-structure warnings, per-section syllable metrics, and repeated
//! lines. No AI, no network; the same report drives CLI output and tests.

use crate::lyrics::{Lyrics, language_supported};
use crate::phonetics::{EnglishPhonetics, PhoneticProvider, Pronunciation};
use crate::rhyme::{RhymeKind, classify, words_of};
use crate::song::SongYaml;
use crate::syllable::{line_metrics, meter};

/// Plan-vs-actual drift: the first index where the plan's canonical tokens
/// diverge from the headings'. `lyrics.md` is authoritative — the report
/// exists so the author notices, not so the tool "fixes" anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drift {
    pub plan: Vec<String>,
    pub actual: Vec<String>,
    /// Index of the first differing token (== len of common prefix).
    pub first_difference: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructureWarning {
    /// Heading line, when the warning is about one section.
    pub line: Option<u32>,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionMetrics {
    pub label: String,
    pub canonical: String,
    pub is_reference: bool,
    /// Lines (a reference counts the original's lines).
    pub lines: usize,
    /// Syllables; `None` when the language is unsupported (A9: "wrong
    /// numbers" is worse than "no numbers").
    pub syllables: Option<usize>,
}

/// A line repeated verbatim (normalized: lowercased, chords stripped).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepeatedLine {
    pub text: String,
    /// (section label, line number) occurrences, in song order.
    pub occurrences: Vec<(String, u32)>,
}

/// End-of-line rhyme groups: sections' last words that rhyme with each
/// other. Kinds beyond `Perfect` are listed but labelled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RhymeHit {
    pub word: String,
    pub kind: RhymeKind,
    pub line: u32,
    pub section: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckReport {
    pub song_id: String,
    pub title: String,
    pub language: String,
    /// `false` → syllable tools report unsupported, not wrong numbers (A9).
    pub language_supported: bool,
    pub drift: Option<Drift>,
    pub structure: Vec<StructureWarning>,
    pub sections: Vec<SectionMetrics>,
    /// Total syllables over real (non-reference) sections; `None` if
    /// unsupported language.
    pub total_syllables: Option<usize>,
    /// Most common nonzero per-line count in the song — the meter the
    /// author seems to be writing to.
    pub meter: Option<usize>,
    pub repetitions: Vec<RepeatedLine>,
    /// Lines that deviate from the section meter by >1 syllable.
    pub meter_outliers: Vec<Outlier>,
    pub rhymes: Vec<RhymeHit>,
}

impl CheckReport {
    /// Clean = no plan drift and no structure warnings. Warnings are
    /// informational (like `doctor`): repetitions and rhymes are observations.
    pub fn is_clean(&self) -> bool {
        self.drift.is_none() && self.structure.is_empty()
    }
}

/// Build the report for one song.
pub fn check(yaml: &SongYaml, lyrics: &Lyrics, project_language: &str) -> CheckReport {
    let language = yaml.effective_language(project_language);
    let supported = language_supported(&language);
    let phon: Box<dyn PhoneticProvider> = if supported {
        Box::new(EnglishPhonetics::new())
    } else {
        Box::new(NoPhonetics)
    };

    let song_id = yaml.id.clone().unwrap_or_default();
    let title = lyrics
        .title
        .clone()
        .or_else(|| yaml.title.clone())
        .unwrap_or_default();

    let drift = check_drift(yaml, lyrics);
    let structure = check_structure(lyrics);
    let sections = section_metrics(lyrics, phon.as_ref(), supported);
    let (total, meter_value, outliers) = song_meter(lyrics, phon.as_ref(), supported);
    let repetitions = repeated_lines(lyrics, phon.as_ref());
    let rhymes = end_rhymes(lyrics, phon.as_ref(), supported);

    CheckReport {
        song_id,
        title,
        language,
        language_supported: supported,
        drift,
        structure,
        sections,
        total_syllables: if supported { Some(total) } else { None },
        meter: meter_value,
        repetitions,
        meter_outliers: outliers,
        rhymes,
    }
}

/// Placeholder provider for unsupported languages: everything is zero
/// syllables so no wrong numbers ever surface.
struct NoPhonetics;
impl PhoneticProvider for NoPhonetics {
    fn pronounce(&self, _word: &str) -> Option<Pronunciation> {
        Some(Pronunciation { phonemes: vec![] })
    }
}

fn check_drift(yaml: &SongYaml, lyrics: &Lyrics) -> Option<Drift> {
    let plan = yaml.plan.as_ref()?;
    let actual: Vec<String> = lyrics
        .sections
        .iter()
        .map(|s| s.canonical.clone())
        .collect();
    let first = plan
        .iter()
        .zip(actual.iter())
        .position(|(p, a)| p != a)
        .unwrap_or_else(|| plan.len().min(actual.len()));
    if plan == &actual {
        None
    } else {
        Some(Drift {
            plan: plan.clone(),
            actual,
            first_difference: first,
        })
    }
}

fn check_structure(lyrics: &Lyrics) -> Vec<StructureWarning> {
    let mut out = Vec::new();
    if lyrics.sections.is_empty() {
        out.push(StructureWarning {
            line: None,
            message: "no sections — add `## Verse 1` style headings".into(),
        });
    }
    for (i, s) in lyrics.sections.iter().enumerate() {
        if s.kind.is_none() {
            out.push(StructureWarning {
                line: Some(s.start_line),
                message: format!(
                    "unknown section {:?} — supported: Intro, Verse, Pre-Chorus, Chorus, \
                     Final Chorus, Post-Chorus, Bridge, Breakdown, Outro, Interlude",
                    s.heading
                ),
            });
        }
        if s.reference_to.is_some() && s.referent(&lyrics.sections).is_none() {
            out.push(StructureWarning {
                line: Some(s.start_line),
                message: format!(
                    "section {:?} references {{={}}} but no earlier section matches",
                    s.heading,
                    s.reference_to.as_deref().unwrap_or("")
                ),
            });
        }
        let empty = if s.reference_to.is_some() {
            s.referent(&lyrics.sections)
                .is_none_or(|t| t.lines.is_empty())
        } else {
            s.lines.is_empty()
        };
        if empty && s.kind.is_some() {
            out.push(StructureWarning {
                line: Some(s.start_line),
                message: format!("section {:?} has no lines", s.heading),
            });
        }
        let _ = i;
    }
    out
}

fn section_metrics(
    lyrics: &Lyrics,
    phon: &dyn PhoneticProvider,
    supported: bool,
) -> Vec<SectionMetrics> {
    lyrics
        .sections
        .iter()
        .map(|s| {
            let lines = s.effective_lines(&lyrics.sections);
            let syllables = if supported {
                Some(
                    lines
                        .iter()
                        .map(|l| line_metrics(&l.raw, phon).syllables())
                        .sum(),
                )
            } else {
                None
            };
            SectionMetrics {
                label: s.label(),
                canonical: s.canonical.clone(),
                is_reference: s.reference_to.is_some(),
                lines: lines.len(),
                syllables,
            }
        })
        .collect()
}

/// (section label, 1-based line within it, syllables, section meter) — a
/// line that deviates from its section's meter by more than one syllable.
pub type Outlier = (String, u32, usize, Option<usize>);

fn song_meter(
    lyrics: &Lyrics,
    phon: &dyn PhoneticProvider,
    supported: bool,
) -> (usize, Option<usize>, Vec<Outlier>) {
    if !supported {
        return (0, None, Vec::new());
    }
    let mut all: Vec<usize> = Vec::new();
    let mut per_section: Vec<(String, Vec<usize>)> = Vec::new();
    for s in &lyrics.sections {
        let lines = s.effective_lines(&lyrics.sections);
        let counts: Vec<usize> = lines
            .iter()
            .map(|l| line_metrics(&l.raw, phon).syllables())
            .collect();
        all.extend(counts.iter().copied());
        per_section.push((s.label(), counts));
    }
    let total = all.iter().sum();
    let song_meter = meter(&all);
    let mut outliers = Vec::new();
    for (label, counts) in &per_section {
        let target = meter(counts);
        for (i, &c) in counts.iter().enumerate() {
            let deviates = match target {
                Some(t) => c.abs_diff(t) > 1,
                None => false,
            };
            if deviates {
                outliers.push((label.clone(), i as u32 + 1, c, target));
            }
        }
    }
    (total, song_meter, outliers)
}

fn repeated_lines(lyrics: &Lyrics, phon: &dyn PhoneticProvider) -> Vec<RepeatedLine> {
    use std::collections::HashMap;
    let mut seen: HashMap<String, Vec<(String, u32)>> = HashMap::new();
    for s in &lyrics.sections {
        for l in &s.lines {
            let norm: String = line_metrics(&l.raw, phon)
                .text
                .to_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if norm.split(' ').count() < 2 {
                continue; // single words ("oh", "yeah") repeat by nature
            }
            seen.entry(norm).or_default().push((s.label(), l.line_no));
        }
    }
    let mut out: Vec<RepeatedLine> = seen
        .into_iter()
        .filter(|(_, occ)| occ.len() >= 2)
        .map(|(text, occurrences)| RepeatedLine { text, occurrences })
        .collect();
    out.sort_by(|a, b| a.text.cmp(&b.text));
    out
}

/// Last word of each real (non-reference) section line, checked against
/// every other section's last word — classic end-rhyme discovery, offline.
fn end_rhymes(lyrics: &Lyrics, phon: &dyn PhoneticProvider, supported: bool) -> Vec<RhymeHit> {
    if !supported {
        return Vec::new();
    }
    let mut ends: Vec<(String, u32, String, Pronunciation)> = Vec::new();
    for s in &lyrics.sections {
        if s.reference_to.is_some() {
            continue;
        }
        for l in &s.lines {
            let (text, _) = crate::chord::parse_line(&l.raw);
            let words = words_of(&text);
            if let Some(last) = words.last()
                && let Some(p) = phon.pronounce(last)
            {
                ends.push((last.clone(), l.line_no, s.label(), p));
            }
        }
    }
    let mut out = Vec::new();
    for (i, (wa, la, sa, pa)) in ends.iter().enumerate() {
        for (wb, lb, sb, pb) in &ends[i + 1..] {
            if sa == sb {
                continue; // cross-section pairs only
            }
            if let Some(kind) = classify(pa, pb) {
                out.push(RhymeHit {
                    word: wb.clone(),
                    kind,
                    line: *lb,
                    section: sb.clone(),
                });
                out.push(RhymeHit {
                    word: wa.clone(),
                    kind,
                    line: *la,
                    section: sa.clone(),
                });
            }
        }
    }
    out.sort_by(|a, b| (a.line, &a.word).cmp(&(b.line, &b.word)));
    out.dedup_by(|a, b| a.line == b.line && a.word == b.word);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::Lyrics;

    fn song(plan: Option<&str>, lyrics_text: &str) -> CheckReport {
        let yaml = SongYaml::parse(&format!(
            "id: song_t\ntitle: T\n{}",
            plan.map(|p| format!("plan: [{p}]\n")).unwrap_or_default()
        ))
        .unwrap();
        let lyrics = Lyrics::parse(lyrics_text).unwrap();
        check(&yaml, &lyrics, "en")
    }

    const CLEAN: &str = "\
# T

## Verse 1
ashes on the water
falling like the rain

## Chorus
hold on to the flame

## Verse 2
 ashes and the embers
 smoke across the sky

## Chorus 2 {=chorus}
";

    #[test]
    fn clean_song_has_no_drift_or_structure_warnings() {
        let r = song(Some("verse, chorus, verse, chorus"), CLEAN);
        assert!(r.drift.is_none());
        assert!(r.structure.is_empty(), "{:?}", r.structure);
        assert!(r.language_supported);
    }

    #[test]
    fn drift_reports_first_difference() {
        let r = song(Some("verse, chorus, bridge"), CLEAN);
        let d = r.drift.unwrap();
        assert_eq!(d.first_difference, 2);
        assert_eq!(d.actual[2], "verse");
        assert_eq!(d.plan[2], "bridge");
    }

    #[test]
    fn missing_plan_means_no_drift_section() {
        let r = song(None, CLEAN);
        assert!(r.drift.is_none());
    }

    #[test]
    fn metrics_count_sections_and_total() {
        let r = song(None, CLEAN);
        assert_eq!(r.sections.len(), 4);
        assert!(r.sections[3].is_reference);
        // reference reuses the chorus's lines/syllables
        assert_eq!(r.sections[1].syllables, r.sections[3].syllables);
        assert!(r.total_syllables.unwrap() > 0);
        // syllable count ignores chord markers even with chords present
        let chorded = song(None, "# T\n\n## Verse 1\n[Dm]ashes on [F]the wa-ter\n");
        // ashes(2) + on(1) + the(1) + wa-ter(2)
        assert_eq!(chorded.sections[0].syllables, Some(6));
    }

    #[test]
    fn unknown_section_and_missing_referent_warn() {
        let r = song(
            None,
            "# T\n\n## Bridge 2\n\nwords\n\n## Chorus 9 {=verse}\n\ntext\n",
        );
        let msgs: Vec<&str> = r.structure.iter().map(|w| w.message.as_str()).collect();
        // "Bridge 2" is known (bridge); the referent for {=verse} is missing
        // because there is no verse section.
        assert!(
            msgs.iter()
                .any(|m| m.contains("no earlier section matches")),
            "got: {msgs:?}"
        );
    }

    #[test]
    fn empty_section_warns() {
        let r = song(None, "# T\n\n## Verse 1\n\n## Chorus\nwords here\n");
        assert!(
            r.structure
                .iter()
                .any(|w| w.message.contains("has no lines")),
            "{:?}",
            r.structure
        );
    }

    #[test]
    fn repetition_is_detected() {
        let r = song(
            None,
            "# T\n\n## Verse 1\nhold on to the light\nhold on to the light\n",
        );
        assert_eq!(r.repetitions.len(), 1);
        assert_eq!(r.repetitions[0].occurrences.len(), 2);
        assert_eq!(r.repetitions[0].occurrences[0].0, "Verse 1");
    }

    #[test]
    fn unsupported_language_reports_unsupported_not_numbers() {
        let r = song(None, CLEAN);
        assert!(r.language_supported); // sanity: en works
        let yaml = SongYaml::parse("id: s\nlanguage: fr\n").unwrap();
        let lyrics = Lyrics::parse(CLEAN).unwrap();
        let fr = check(&yaml, &lyrics, "en");
        assert!(!fr.language_supported);
        assert!(fr.total_syllables.is_none());
        assert!(fr.sections.iter().all(|s| s.syllables.is_none()));
        // structure/drift don't need phonetics
        assert!(fr.structure.is_empty());
    }

    #[test]
    fn end_rhymes_span_sections() {
        let r = song(
            None,
            "# T\n\n## Verse 1\ncome back tonight\n\n## Chorus\nhold the light\n",
        );
        assert!(
            r.rhymes.iter().any(|h| h.kind == RhymeKind::Perfect),
            "{:?}",
            r.rhymes
        );
    }
}
