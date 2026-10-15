//! ChordPro-style inline chords (G6/A11): `I [Dm]thought I [F]knew`.
//!
//! The parser extracts chord events with character offsets; syllable
//! counting strips markers before it looks at a word. Phase 3 stores,
//! displays, and transposes; Phase 15 exports lead sheets from the same
//! events.

use anyhow::{Result, bail};

/// One inline chord: the literal name (`Dm`) and its character offset in
/// the chord-stripped line (where the chord lands among the sung words;
/// Unicode scalar values, not bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChordEvent {
    pub chord: String,
    pub offset: usize,
}

const SHARPS: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// Parse one line into `(text without chord markers, chord events)`.
/// Malformed markers (`[unclosed`, `[ ]`) stay literal text.
pub fn parse_line(line: &str) -> (String, Vec<ChordEvent>) {
    let mut out = String::with_capacity(line.len());
    let mut events = Vec::new();
    let mut offset = 0usize;
    let mut buffer = String::new();
    let mut in_marker = false;
    for c in line.chars() {
        match (in_marker, c) {
            (false, '[') => {
                in_marker = true;
                buffer.clear();
            }
            (true, ']') => {
                let name = buffer.trim();
                if is_chord_name(name) {
                    events.push(ChordEvent {
                        chord: name.to_string(),
                        offset,
                    });
                    in_marker = false;
                } else {
                    // Not a chord (e.g. a footnote `[1]`): keep literal.
                    out.push('[');
                    out.push_str(&buffer);
                    out.push(']');
                    offset += buffer.chars().count() + 2;
                    in_marker = false;
                }
            }
            (true, c) => buffer.push(c),
            (false, c) => {
                out.push(c);
                offset += 1;
            }
        }
    }
    if in_marker {
        // Unterminated marker: literal.
        out.push('[');
        out.push_str(&buffer);
    }
    (out, events)
}

/// Chord names we accept inside `[...]`: root + optional accidentals +
/// a quality drawn from the standard suffix alphabet (no prose vowels like
/// the `o` in `[hold]` or the `r` in `[Bridge]`), optionally a `/` bass.
fn is_chord_name(name: &str) -> bool {
    if name.is_empty() || name.chars().count() > 12 {
        return false;
    }
    let main = match name.split_once('/') {
        Some((m, _)) => m,
        None => name,
    };
    let mut cs = main.chars();
    let Some(root) = cs.next() else { return false };
    if !root.is_ascii_uppercase() || root_index(root).is_none() {
        return false;
    }
    let rest: String = cs.as_str().trim_start_matches(['b', '#']).to_string();
    rest.chars().all(|c| {
        matches!(
            c,
            'm' | 'M' | 'a' | 'd' | 'i' | 's' | 'u' | 'g' | 'j' | '0'..='9' | 'b' | '#'
        )
    })
}

fn root_index(root: char) -> Option<usize> {
    Some(match root {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    })
}

/// Transpose one chord name by `semitones` (positive = up; rendering is
/// always sharps — `Bb`-down renders `G#`, documented and deterministic).
/// Unparsable chords (`N.C.`, `hold`) come back unchanged.
pub fn transpose(chord: &str, semitones: i32) -> String {
    let (main, bass) = match chord.split_once('/') {
        Some((m, b)) => (m, Some(b)),
        None => (chord, None),
    };
    let Some((root, accidental, quality)) = split_root(main) else {
        return chord.to_string();
    };
    let Some(idx) = root_index(root) else {
        return chord.to_string();
    };
    let up = ((idx as i32 + accidental + semitones).rem_euclid(12)) as usize;
    let mut out = format!("{}{}", SHARPS[up], quality);
    if let Some(b) = bass
        && let Some((broot, bacc, bq)) = split_root(b)
        && let Some(bidx) = root_index(broot)
    {
        let bup = ((bidx as i32 + bacc + semitones).rem_euclid(12)) as usize;
        out.push_str(&format!("/{}{}", SHARPS[bup], bq));
    }
    out
}

/// Split `Dm`, `F#sus4`, `Bbmaj7` into `(root, accidental offset, quality)`.
fn split_root(s: &str) -> Option<(char, i32, &str)> {
    let mut cs = s.chars();
    let root = cs.next()?;
    root_index(root)?;
    let rest = cs.as_str();
    let accidental: i32 = rest
        .chars()
        .take_while(|c| matches!(c, 'b' | '#'))
        .map(|c| if c == '#' { 1 } else { -1 })
        .sum();
    let acc_len = rest.chars().take_while(|c| matches!(c, 'b' | '#')).count();
    Some((root, accidental, &rest[acc_len..]))
}

/// Transpose every chord marker in a line, preserving layout byte-for-byte
/// apart from the chord names.
pub fn transpose_line(line: &str, semitones: i32) -> Result<String> {
    if semitones == 0 {
        return Ok(line.to_string());
    }
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find('[') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find(']') {
            Some(end) => {
                let name = after[..end].trim();
                if is_chord_name(name) {
                    out.push_str(&format!("[{}]", transpose(name, semitones)));
                } else {
                    out.push('[');
                    out.push_str(&after[..end]);
                    out.push(']');
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push('[');
                out.push_str(after);
                return Ok(out);
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// Validate an accidental/quality render after transposition (used by the
/// CLI to reject nonsense input early).
pub fn validate_semitones(n: i32) -> Result<()> {
    if !(-11..=11).contains(&n) {
        bail!("transpose by -11..=11 semitones, got {n}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events_and_stripped_text() {
        let (text, events) = parse_line("I [Dm]thought I [F]knew");
        assert_eq!(text, "I thought I knew");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].chord, "Dm");
        // offsets index the stripped text: [Dm] lands before "thought" (2),
        // [F] before "knew" (11).
        assert_eq!(events[0].offset, 2);
        assert_eq!(events[1].chord, "F");
        // "thought" is 7 chars: [F] lands at 2 + 7 + 3 = 12.
        assert_eq!(events[1].offset, 12);
    }

    #[test]
    fn prose_brackets_stay_literal() {
        let (text, events) = parse_line("see [1] and [hold me]");
        assert_eq!(text, "see [1] and [hold me]");
        assert!(events.is_empty());
    }

    #[test]
    fn unterminated_marker_stays_literal() {
        let (text, events) = parse_line("oops [Dm");
        assert_eq!(text, "oops [Dm");
        assert!(events.is_empty());
    }

    #[test]
    fn transposes_roots_and_keeps_quality() {
        assert_eq!(transpose("Dm", 2), "Em");
        assert_eq!(transpose("C", 7), "G");
        assert_eq!(transpose("F#sus4", 1), "Gsus4");
        // sharp rendering is deterministic: down from Bb lands on G#
        assert_eq!(transpose("Bbmaj7", -2), "G#maj7");
        assert_eq!(transpose("N.C.", 3), "N.C.");
        assert_eq!(transpose("Ab", 1), "A");
        assert_eq!(transpose("C", -1), "B");
    }

    #[test]
    fn transposes_bass_notes() {
        assert_eq!(transpose("C/G", 2), "D/A");
    }

    #[test]
    fn transposes_a_line_in_place() {
        let line = "[Dm]hold [F]on";
        assert_eq!(transpose_line(line, 2).unwrap(), "[Em]hold [G]on");
        assert_eq!(transpose_line(line, 0).unwrap(), line);
    }

    #[test]
    fn offsets_are_char_not_byte() {
        let (text, events) = parse_line("é [G]low");
        assert_eq!(text, "é low");
        // 'é' is one char (two bytes): [G] lands at char 2, not byte 3.
        assert_eq!(events[0].offset, 2);
    }
}
