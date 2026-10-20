//! Deterministic chunking: markdown body → heading-aware paragraph
//! groups. Same file in, same chunks out — the incremental re-embed
//! compares chunk checksums, so chunking must never drift.

use sha2::{Digest, Sha256};

/// Soft target for one chunk, in characters. Embedding models are
/// short-context (A8: conservative estimates); 1200 chars of prose fits
/// comfortably in 512 tokens for every mainstream embedding GGUF.
const TARGET_CHARS: usize = 1200;
/// Hard ceiling: a single paragraph longer than this splits (at a
/// whitespace boundary when one exists) instead of becoming a monster.
const MAX_CHARS: usize = 1600;

/// One embeddable chunk: a run of paragraphs under a heading path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    /// 0-based position within the document — the stable cache key
    /// alongside the rel path.
    pub idx: usize,
    /// Heading path (`Act I > The Hall`), empty before the first heading.
    pub heading: String,
    pub text: String,
}

impl Chunk {
    /// What actually gets embedded: heading context prefixed to the text
    /// (a vector of "The Hall" alone would match every hall).
    pub fn embed_text(&self) -> String {
        if self.heading.is_empty() {
            self.text.clone()
        } else {
            format!("{}\n{}", self.heading, self.text)
        }
    }

    /// SHA-256 of the chunk text, hex — the incremental sync's change
    /// detector (never a user-facing word; cache internals only).
    pub fn checksum(&self) -> String {
        hex_digest(self.text.as_bytes())
    }
}

pub(crate) fn hex_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Split a markdown body (frontmatter already stripped) into chunks:
/// heading changes flush, paragraphs pack up to [`TARGET_CHARS`], single
/// oversized paragraphs hard-split at [`MAX_CHARS`]. ATX headings only
/// (`#`–`######`) — the project format's heading syntax.
pub fn chunk_markdown(body: &str) -> Vec<Chunk> {
    let mut headings: Vec<String> = Vec::new();
    let mut buffer = String::new();
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut in_fence = false;

    let flush = |buffer: &mut String, headings: &Vec<String>, chunks: &mut Vec<Chunk>| {
        let text = buffer.trim();
        if !text.is_empty() {
            chunks.push(Chunk {
                idx: chunks.len(),
                heading: headings
                    .iter()
                    .filter(|h| !h.is_empty())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" > "),
                text: text.to_string(),
            });
        }
        buffer.clear();
    };

    for line in body.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim_start().starts_with("```") || trimmed.trim_start().starts_with("~~~") {
            // Fences are content, but their `#` lines must not become
            // headings.
            in_fence = !in_fence;
            push_line(&mut buffer, trimmed);
            continue;
        }
        if !in_fence && let Some((level, title)) = atx_heading(trimmed) {
            flush(&mut buffer, &headings, &mut chunks);
            headings.truncate(level - 1);
            // Skipped levels (`#` straight to `###`) pad the path.
            while headings.len() < level - 1 {
                headings.push(String::new());
            }
            headings.push(title.to_string());
            continue;
        }
        if trimmed.is_empty() {
            // Paragraph boundary: a full buffer flushes; an empty line
            // inside a run just separates paragraphs.
            if !buffer.trim().is_empty() && buffer_len(&buffer) >= TARGET_CHARS {
                flush(&mut buffer, &headings, &mut chunks);
            } else {
                push_line(&mut buffer, "");
            }
            continue;
        }
        // Would this paragraph overflow the current chunk? Flush first.
        let para_len = trimmed.chars().count();
        if !buffer.trim().is_empty() && buffer_len(&buffer) + 1 + para_len > TARGET_CHARS {
            flush(&mut buffer, &headings, &mut chunks);
        }
        // A single paragraph past the hard ceiling splits as it enters.
        if para_len > MAX_CHARS {
            for piece in hard_split(trimmed, MAX_CHARS) {
                if !buffer.trim().is_empty() && buffer_len(&buffer) >= TARGET_CHARS {
                    flush(&mut buffer, &headings, &mut chunks);
                }
                push_line(&mut buffer, &piece);
                if buffer_len(&buffer) >= TARGET_CHARS {
                    flush(&mut buffer, &headings, &mut chunks);
                }
            }
            continue;
        }
        push_line(&mut buffer, trimmed);
    }
    flush(&mut buffer, &headings, &mut chunks);
    chunks
}

fn push_line(buffer: &mut String, line: &str) {
    if line.is_empty() {
        if !buffer.is_empty() && !buffer.ends_with("\n\n") {
            buffer.push('\n');
        }
        return;
    }
    if !buffer.is_empty() && !buffer.ends_with('\n') {
        buffer.push('\n');
    } else if buffer.ends_with("\n\n") {
        // already separated
    }
    buffer.push_str(line);
}

fn buffer_len(buffer: &str) -> usize {
    buffer.trim().chars().count()
}

/// `("## The Hall") → Some((2, "The Hall"))`; None for anything that is
/// not an ATX heading (`###x` is a paragraph, setext underlines are
/// underlines).
fn atx_heading(line: &str) -> Option<(usize, &str)> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &line[hashes..];
    if !(rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t')) {
        return None;
    }
    let title = rest.trim();
    if title.is_empty() {
        return None;
    }
    Some((hashes, title))
}

/// Split oversized text at whitespace boundaries near `max`, always on a
/// char boundary.
fn hard_split(text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0;
    let len = text.chars().count();
    while start < len {
        let take = max.min(len - start);
        let mut end = start + take;
        if end < len {
            // Pull back to the last whitespace within the window.
            let window: String = text.chars().skip(start).take(take).collect();
            if let Some(cut) = window.rfind(char::is_whitespace) {
                end = start + cut;
            }
        }
        let piece: String = text.chars().skip(start).take(end - start).collect();
        let piece = piece.trim();
        if !piece.is_empty() {
            out.push(piece.to_string());
        }
        start = (end + 1).min(len);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_paths_and_paragraph_packing() {
        let body = "# Novel\n\nOpening line.\n\n## The Hall\n\nElena read the letter.\n\nThe ink ran.\n\n## After\n\nNobody spoke.\n";
        let chunks = chunk_markdown(body);
        assert_eq!(chunks.len(), 3, "{chunks:#?}");
        assert_eq!(chunks[0].heading, "Novel");
        assert_eq!(chunks[0].text, "Opening line.");
        assert_eq!(chunks[1].heading, "Novel > The Hall");
        assert_eq!(chunks[1].text, "Elena read the letter.\nThe ink ran.");
        assert_eq!(chunks[2].heading, "Novel > After");
        assert_eq!(chunks[2].idx, 2);
    }

    #[test]
    fn long_documents_split_at_the_target_size() {
        let para = "Words worth counting. ".repeat(100); // ~2100 chars
        let body = format!("## Scene\n\n{para}\n");
        let chunks = chunk_markdown(&body);
        assert!(chunks.len() >= 2, "one paragraph must split: {chunks:#?}");
        for chunk in &chunks {
            assert!(
                chunk.text.chars().count() <= MAX_CHARS + 40,
                "hard ceiling respected: {} chars",
                chunk.text.chars().count()
            );
            assert!(!chunk.text.is_empty());
        }
        // Splitting never loses the words (modulo boundary whitespace).
        let joined: String = chunks
            .iter()
            .map(|c| c.text.replace(' ', ""))
            .collect::<String>();
        let expected: String = para.chars().filter(|c| !c.is_whitespace()).collect();
        assert_eq!(joined, expected);
    }

    #[test]
    fn heading_lines_inside_code_fences_are_content() {
        let body = "## Real\n\n```\n# not a heading\n```\n\nAfter the fence.\n";
        let chunks = chunk_markdown(body);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].text.contains("# not a heading"));
        assert_eq!(chunks[0].heading, "Real");
    }

    #[test]
    fn skipped_heading_levels_pad_the_path_without_panicking() {
        let body = "# Act\n\n### Scene\n\nText.\n\n## Beat\n\nMore.\n";
        let chunks = chunk_markdown(body);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].heading, "Act > Scene");
        assert_eq!(chunks[1].heading, "Act > Beat");
    }

    #[test]
    fn headingless_text_chunks_without_a_path() {
        let chunks = chunk_markdown("Just prose.\n\nAnd more.\n");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].heading, "");
        assert_eq!(chunks[0].embed_text(), "Just prose.\nAnd more.");
    }

    #[test]
    fn empty_bodies_chunk_to_nothing() {
        assert!(chunk_markdown("").is_empty());
        assert!(chunk_markdown("   \n\n  \n").is_empty());
    }

    #[test]
    fn embed_text_carries_heading_context_and_checksum_is_stable() {
        let chunks = chunk_markdown("## The Hall\n\nElena waited.\n");
        assert_eq!(chunks[0].embed_text(), "The Hall\nElena waited.");
        assert_eq!(chunks[0].checksum(), chunks[0].checksum());
        assert_eq!(chunks[0].checksum().len(), 64);
    }

    #[test]
    fn chunking_is_deterministic() {
        let body = "## A\n\none two three\n\n## B\n\nfour five six\n";
        assert_eq!(chunk_markdown(body), chunk_markdown(body));
    }
}
