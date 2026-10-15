//! OCA document parsing: frontmatter split + comrak block structure with
//! source line ranges (A9).
//!
//! Round-trip invariant: `parse(raw).raw == raw` — the parser produces views
//! (frontmatter, blocks, links) but never regenerates bytes. Save paths write
//! `raw` (possibly body-spliced by an explicit edit), so Markdown + frontmatter
//! survive byte-identically.

use comrak::Arena;
use comrak::nodes::{ListType, Node, NodeValue};
use comrak::{Options, parse_document};

use oca_model::{Block, Document, Frontmatter, LineRange, WikiLink};

/// Comrak options: GFM tables + task lists (Phase 1 needs nothing else).
fn comrak_options<'a>() -> Options<'a> {
    let mut opts = Options::default();
    opts.extension.table = true;
    opts.extension.tasklist = true;
    opts
}

/// Frontmatter-only view of a file — index scans skip body parsing.
pub fn frontmatter_of(raw: &str) -> Frontmatter {
    split_frontmatter(raw).0
}

/// Parse full file bytes into the document model.
pub fn parse(raw: &str) -> Document {
    let (frontmatter, body, fm_lines, body_start) = split_frontmatter(raw);
    let offset = fm_lines; // body line L lives on file line L + offset

    let arena = Arena::new();
    let root = parse_document(&arena, body, &comrak_options());

    let mut blocks = Vec::new();
    for node in root.children() {
        let data = node.data();
        let sp = data.sourcepos;
        let lines = LineRange::new(sp.start.line as u32 + offset, sp.end.line as u32 + offset);
        let block = match &data.value {
            NodeValue::Heading(h) => Block::Heading {
                level: h.level,
                text: literal_text(node),
                lines,
            },
            NodeValue::Paragraph => match single_image(node) {
                Some((alt, source)) => Block::Image { alt, source, lines },
                None => Block::Paragraph {
                    preview: clamp(&literal_text(node)),
                    lines,
                },
            },
            NodeValue::BlockQuote => Block::Quote { lines },
            NodeValue::CodeBlock(c) => Block::Code {
                info: c.info.clone(),
                lines,
            },
            NodeValue::List(l) => Block::List {
                ordered: matches!(l.list_type, ListType::Ordered),
                lines,
            },
            NodeValue::Table(_) => Block::Table { lines },
            NodeValue::HtmlBlock(h) => {
                if h.literal.trim_start().starts_with("<!--") {
                    Block::Comment {
                        preview: clamp(h.literal.trim()),
                        lines,
                    }
                } else {
                    Block::Html { lines }
                }
            }
            NodeValue::ThematicBreak => Block::ThematicBreak { lines },
            _ => continue, // footnotes/metadata/etc: not part of the v1 model
        };
        blocks.push(block);
    }

    let links = scan_links(body, fm_lines + 1);

    Document {
        raw: raw.to_string(),
        frontmatter,
        blocks,
        links,
        body_start_line: fm_lines + 1,
        body_start,
    }
}

/// Strip a leading `---` frontmatter block: `(frontmatter, body, fence-lines,
/// body-byte-offset)`. An unterminated opener is *not* frontmatter (it stays
/// body content, e.g. a thematic break).
fn split_frontmatter(raw: &str) -> (Frontmatter, &str, u32, usize) {
    let no_front = (Frontmatter::none(), raw, 0, 0);
    let Some(first_nl) = raw.find('\n') else {
        return no_front;
    };
    if raw[..first_nl].trim_end_matches('\r') != "---" {
        return no_front;
    }
    let mut pos = first_nl + 1;
    while pos <= raw.len() {
        let line_end = raw[pos..].find('\n').map(|i| pos + i).unwrap_or(raw.len());
        if raw[pos..line_end].trim_end_matches('\r') == "---" {
            let fm_end = if line_end < raw.len() {
                line_end + 1
            } else {
                raw.len()
            };
            let fm = &raw[..fm_end];
            let fence_lines = fm.matches('\n').count() as u32;
            return (
                Frontmatter::from_fenced(fm),
                &raw[fm_end..],
                fence_lines,
                fm_end,
            );
        }
        if line_end >= raw.len() {
            break;
        }
        pos = line_end + 1;
    }
    no_front
}

/// All `[[target]]` / `[[target|alias]]` links in `body`, with file line
/// numbers starting at `first_line`.
fn scan_links(body: &str, first_line: u32) -> Vec<WikiLink> {
    let mut links = Vec::new();
    for (i, line) in body.lines().enumerate() {
        let lineno = first_line + i as u32;
        let mut idx = 0;
        while let Some(rel) = line[idx..].find("[[") {
            let open = idx + rel + 2;
            let Some(close_rel) = line[open..].find("]]") else {
                break;
            };
            let content = line[open..open + close_rel].trim();
            if !content.is_empty() && !content.contains('[') {
                let (target, alias) = match content.split_once('|') {
                    Some((t, a)) => (t.trim(), Some(a.trim().to_string())),
                    None => (content, None),
                };
                if !target.is_empty() {
                    links.push(WikiLink {
                        target: target.to_string(),
                        alias,
                        line: lineno,
                    });
                }
            }
            idx = open + close_rel + 2;
        }
    }
    links
}

/// Concatenate the plain text under `node` (headings/paragraph previews).
fn collect_text(node: Node<'_>, out: &mut String) {
    match &node.data().value {
        NodeValue::Text(t) => out.push_str(t),
        NodeValue::Code(c) => out.push_str(&c.literal),
        NodeValue::SoftBreak | NodeValue::LineBreak => out.push(' '),
        _ => {
            for child in node.children() {
                collect_text(child, out);
            }
        }
    }
}

fn literal_text(node: Node<'_>) -> String {
    let mut out = String::new();
    collect_text(node, &mut out);
    out
}

/// If the paragraph contains exactly one image, return `(alt, source)`.
fn single_image(node: Node<'_>) -> Option<(String, String)> {
    let mut kids = node.children();
    let first = kids.next()?;
    if kids.next().is_some() {
        return None;
    }
    match &first.data().value {
        NodeValue::Image(img) => {
            let mut alt = String::new();
            collect_text(first, &mut alt);
            Some((alt, img.url.clone()))
        }
        _ => None,
    }
}

fn clamp(s: &str) -> String {
    const MAX: usize = 80;
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX).collect();
    out.push('…');
    out
}

/// Splice the document's (possibly patched) frontmatter onto `body`.
pub fn with_body(doc: &Document, body: &str) -> String {
    match doc.frontmatter.raw() {
        Some(fm) => format!("{fm}{body}"),
        None => body.to_string(),
    }
}

/// Append `text` as a new paragraph, keeping normal Markdown paragraph
/// separation and a trailing newline.
pub fn append_paragraph(raw: &str, text: &str) -> String {
    let text = text.trim();
    let mut out = raw.to_string();
    if !out.is_empty() && !text.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        if !out.ends_with("\n\n") {
            out.push('\n');
        }
    }
    out.push_str(text);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "---\n\
        id: ch_01ABC\n\
        title: \"A: B\"\n\
        # a comment the editor must not eat\n\
        unknown_key: [1, 2]\n\
        ---\n\
        # Chapter One\n\
        \n\
        Opening paragraph with **bold** and `code`.\n\
        \n\
        > A quote.\n\
        \n\
        ## Scene: Dawn\n\
        \n\
        Text with a [[char_01XYZ|Rin]] link and [[ch_01DEF]].\n\
        \n\
        ```rust\n\
        fn main() {}\n\
        ```\n\
        \n\
        - one\n\
        - two\n\
        \n\
        1. first\n\
        2. second\n\
        \n\
        | a | b |\n\
        |---|---|\n\
        | 1 | 2 |\n\
        \n\
        ![cover](assets/images/cover.png)\n\
        \n\
        <!-- an inline annotation -->\n\
        \n\
        <div>raw html</div>\n\
        \n\
        ---\n";

    #[test]
    fn round_trip_bytes_identical() {
        for raw in [
            FIXTURE,
            "no frontmatter\njust text\n",
            "---\na: 1\n---\nbody\n",
            "---\na: 1\n---",
            "",
            "---\r\nid: x\r\n---\r\nbody\r\n",
            "---\nunterminated\nstill body\n",
            "# Only a heading\n",
        ] {
            assert_eq!(parse(raw).raw, raw, "raw must round-trip: {raw:?}");
        }
    }

    #[test]
    fn frontmatter_split() {
        let doc = parse(FIXTURE);
        assert_eq!(doc.frontmatter.get_str("id").as_deref(), Some("ch_01ABC"));
        assert_eq!(doc.frontmatter.get_str("title").as_deref(), Some("A: B"));
        assert!(doc.frontmatter.raw().unwrap().contains("# a comment"));
        assert_eq!(doc.body_start_line, 7, "6 fence/YAML lines, body on line 7");
        assert!(doc.body().starts_with("# Chapter One"));
    }

    #[test]
    fn no_frontmatter_body_is_whole_file() {
        let raw = "# H\n\ntext\n";
        let doc = parse(raw);
        assert!(doc.frontmatter.raw().is_none());
        assert_eq!(doc.body_start, 0);
        assert_eq!(doc.body(), raw);
    }

    #[test]
    fn block_structure_with_file_line_ranges() {
        let doc = parse(FIXTURE);
        let headings: Vec<(u8, &str)> = doc
            .blocks
            .iter()
            .filter_map(|b| b.as_heading().map(|(l, t, _)| (l, t)))
            .collect();
        assert_eq!(headings, vec![(1, "Chapter One"), (2, "Scene: Dawn")]);

        // File lines (offset by the 6-line frontmatter): "# Chapter One" is 7.
        assert_eq!(doc.blocks[0].lines(), LineRange::new(7, 7));

        let kinds = |b: &Block| match b {
            Block::Heading { .. } => "heading",
            Block::Paragraph { .. } => "para",
            Block::Quote { .. } => "quote",
            Block::Image { .. } => "image",
            Block::Table { .. } => "table",
            Block::Code { .. } => "code",
            Block::List { ordered, .. } => {
                if *ordered {
                    "ol"
                } else {
                    "ul"
                }
            }
            Block::Html { .. } => "html",
            Block::Comment { .. } => "comment",
            Block::ThematicBreak { .. } => "hr",
        };
        let seq: Vec<&str> = doc.blocks.iter().map(kinds).collect();
        assert_eq!(
            seq,
            vec![
                "heading", "para", "quote", "heading", "para", "code", "ul", "ol", "table",
                "image", "comment", "html", "hr"
            ]
        );
    }

    #[test]
    fn previews_are_plain_text() {
        let doc = parse(FIXTURE);
        let para = doc
            .blocks
            .iter()
            .find_map(|b| match b {
                Block::Paragraph { preview, .. } => Some(preview.as_str()),
                _ => None,
            })
            .unwrap();
        assert_eq!(para, "Opening paragraph with bold and code.");
    }

    #[test]
    fn image_block_carries_alt_and_source() {
        let doc = parse(FIXTURE);
        let img = doc
            .blocks
            .iter()
            .find_map(|b| match b {
                Block::Image { alt, source, .. } => Some((alt.clone(), source.clone())),
                _ => None,
            })
            .unwrap();
        assert_eq!(img.0, "cover");
        assert_eq!(img.1, "assets/images/cover.png");
    }

    #[test]
    fn html_comment_is_annotation_block() {
        let doc = parse(FIXTURE);
        let comment = doc
            .blocks
            .iter()
            .find_map(|b| match b {
                Block::Comment { preview, .. } => Some(preview.clone()),
                _ => None,
            })
            .unwrap();
        assert!(comment.contains("an inline annotation"));
    }

    #[test]
    fn wiki_links_extracted_with_line_numbers() {
        let doc = parse(FIXTURE);
        assert_eq!(
            doc.links,
            vec![
                WikiLink {
                    target: "char_01XYZ".into(),
                    alias: Some("Rin".into()),
                    line: 15,
                },
                WikiLink {
                    target: "ch_01DEF".into(),
                    alias: None,
                    line: 15,
                },
            ]
        );
    }

    #[test]
    fn no_bracket_false_positives() {
        let doc = parse("single [bracket] and [a](b) only\n");
        assert!(doc.links.is_empty());
        let doc = parse("text [[unclosed\nmore\n");
        assert!(doc.links.is_empty());
    }

    #[test]
    fn multibyte_text_keeps_line_numbers() {
        let raw = "---\nid: x\n---\nÉmojis 🎵🎨 on line one\n\nafter\n";
        let doc = parse(raw);
        let para_lines = doc.blocks[1].lines();
        assert_eq!(
            (para_lines.start, para_lines.end),
            (6, 6),
            "second body block must be file line 6"
        );
    }

    #[test]
    fn append_paragraph_separators() {
        assert_eq!(append_paragraph("", "a"), "a\n");
        assert_eq!(append_paragraph("a\n", "b"), "a\n\nb\n");
        assert_eq!(append_paragraph("a\n\n", "b"), "a\n\nb\n");
        assert_eq!(append_paragraph("a", "b"), "a\n\nb\n");
        assert_eq!(append_paragraph("a\n\n", ""), "a\n\n");
    }

    #[test]
    fn with_body_preserves_frontmatter_bytes() {
        let doc = parse(FIXTURE);
        let out = with_body(&doc, "New body.\n");
        assert!(out.starts_with(doc.frontmatter.raw().unwrap()));
        assert!(out.ends_with("New body.\n"));
        // reparsing gives back our body unchanged
        assert_eq!(parse(&out).body(), "New body.\n");
    }
}
