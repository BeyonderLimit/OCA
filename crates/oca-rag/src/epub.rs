//! Minimal EPUB text extraction (Phase 11): enough to turn a reference
//! EPUB into normalized Markdown for the knowledge base — spine order,
//! chapter headings, paragraph text. Full-fidelity EPUB *manuscript*
//! import belongs to `oca-import` (Phase 15); this module never touches
//! the project model, it only reads prose.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};

/// A parsed EPUB: book title (when the metadata carries one) and one
/// `(heading, text)` pair per spine item that had prose.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct EpubBook {
    pub title: Option<String>,
    pub chapters: Vec<(String, String)>,
}

impl EpubBook {
    /// The normalized Markdown the library copy stores.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        for (heading, text) in &self.chapters {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("## ");
            out.push_str(heading);
            out.push_str("\n\n");
            out.push_str(text);
            out.push('\n');
        }
        out
    }
}

/// Read one EPUB into its normalized form. Anything unreadable (not a
/// zip, no container, no OPF, no spine) is a clean error naming the file.
pub fn read_epub(path: &Path) -> Result<EpubBook> {
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("{} is not a readable EPUB", path.display()))?;

    let container = read_entry(&mut zip, "META-INF/container.xml")
        .context("EPUB has no META-INF/container.xml")?;
    let opf_rel =
        rootfile_path(&container).ok_or_else(|| anyhow!("container.xml names no rootfile"))?;
    let opf_dir = opf_rel
        .parent()
        .map(|d| d.to_path_buf())
        .unwrap_or_default();
    let opf = read_entry(&mut zip, &opf_rel.to_string_lossy())
        .with_context(|| format!("EPUB rootfile {} is missing", opf_rel.display()))?;

    let (title, manifest, spine) = parse_opf(&opf)?;
    let mut book = EpubBook {
        title,
        chapters: Vec::new(),
    };
    for (n, idref) in spine.iter().enumerate() {
        let Some(href) = manifest.get(idref.as_str()) else {
            continue;
        };
        let entry = opf_dir.join(href);
        let entry_norm = normalize_rel(&entry);
        let Ok(xhtml) = read_entry(&mut zip, &entry_norm.to_string_lossy()) else {
            tracing::warn!(entry = %entry_norm.display(), "EPUB spine item missing; skipped");
            continue;
        };
        let (heading, text) = xhtml_to_text(&xhtml);
        if text.trim().is_empty() {
            continue;
        }
        let heading = heading.unwrap_or_else(|| format!("Section {}", n + 1));
        book.chapters.push((heading, text));
    }
    if book.chapters.is_empty() {
        anyhow::bail!("{} carries no readable text", path.display());
    }
    Ok(book)
}

fn read_entry(zip: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Result<String> {
    let mut entry = zip.by_name(name)?;
    let mut bytes = Vec::new();
    entry
        .read_to_end(&mut bytes)
        .with_context(|| format!("read EPUB entry {name}"))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// `container.xml` → the OPF path (`rootfile[@full-path]`).
fn rootfile_path(container: &str) -> Option<PathBuf> {
    let start = container.find("<rootfile ")?; // not `<rootfiles>`
    let tag_end = container[start..].find('>')? + start;
    let tag = &container[start..=tag_end];
    let attr = tag.find("full-path")?;
    let rest = &tag[attr + "full-path".len()..];
    let after_eq = rest.find('=')? + 1;
    let quote = rest[after_eq..].chars().next()?;
    let value_start = after_eq + 1;
    let value_end = rest[value_start..].find(quote)? + value_start;
    Some(PathBuf::from(&rest[value_start..value_end]))
}

/// OPF metadata: `(book title, id → href manifest, spine idrefs in order)`.
type OpfParts = (
    Option<String>,
    std::collections::BTreeMap<String, String>,
    Vec<String>,
);

fn parse_opf(opf: &str) -> Result<OpfParts> {
    let mut title: Option<String> = None;
    let mut manifest = std::collections::BTreeMap::new();
    let mut spine = Vec::new();
    let mut current: Option<(String, Vec<(String, String)>)> = None;
    let mut in_title = false;

    for event in xml_events(opf) {
        match event {
            XmlEvent::Start(name, attrs) => {
                if name == "title" {
                    in_title = true;
                }
                if name == "item"
                    && let (Some(id), Some(href)) = (attr(&attrs, "id"), attr(&attrs, "href"))
                {
                    manifest.insert(id, href);
                }
                if name == "itemref"
                    && let Some(idref) = attr(&attrs, "idref")
                {
                    spine.push(idref);
                }
                current = Some((name, attrs));
            }
            XmlEvent::Text(text) => {
                if in_title && title.is_none() && !text.trim().is_empty() {
                    title = Some(text.trim().to_string());
                }
            }
            XmlEvent::End(name) => {
                if name == "title" {
                    in_title = false;
                }
                if current.as_ref().is_some_and(|(cur, _)| *cur == name) {
                    current = None;
                }
            }
            XmlEvent::Other => {}
        }
    }
    if spine.is_empty() {
        anyhow::bail!("EPUB manifest declares no spine");
    }
    Ok((title, manifest, spine))
}

/// XHTML → `(first heading, prose)`: block elements become paragraph
/// breaks, markup disappears, entities decode, `head`/`script`/`style`
/// subtrees never contribute text. Text accumulates raw (entity events
/// arrive between text runs) and whitespace collapses at the end.
fn xhtml_to_text(xhtml: &str) -> (Option<String>, String) {
    const BLOCK: &[&str] = &[
        "p",
        "div",
        "br",
        "li",
        "tr",
        "blockquote",
        "section",
        "article",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
    ];
    const SKIP: &[&str] = &["script", "style", "head"];
    let mut text = String::new();
    let mut first_heading: Option<String> = None;
    let mut skip_depth = 0usize;
    let mut heading_buf: Option<String> = None;

    for event in xml_events(xhtml) {
        match event {
            XmlEvent::Start(name, _) => {
                if SKIP.contains(&name.as_str()) {
                    skip_depth += 1;
                    continue;
                }
                if skip_depth > 0 {
                    continue;
                }
                if name.starts_with('h') && name.len() == 2 && name[1..].parse::<u8>().is_ok() {
                    heading_buf = Some(String::new());
                }
                if BLOCK.contains(&name.as_str()) && !text.ends_with('\n') {
                    text.push('\n');
                }
            }
            XmlEvent::End(name) => {
                if SKIP.contains(&name.as_str()) {
                    skip_depth = skip_depth.saturating_sub(1);
                    continue;
                }
                if skip_depth > 0 {
                    continue;
                }
                if let Some(heading) = heading_buf.take()
                    && first_heading.is_none()
                    && !heading.trim().is_empty()
                {
                    first_heading = Some(heading.trim().to_string());
                }
                if BLOCK.contains(&name.as_str()) && !text.ends_with('\n') {
                    text.push('\n');
                }
            }
            XmlEvent::Text(raw) => {
                if skip_depth > 0 {
                    continue;
                }
                match heading_buf.as_mut() {
                    Some(heading) => heading.push_str(&raw),
                    None => text.push_str(&raw),
                }
            }
            XmlEvent::Other => {}
        }
    }
    // Whitespace runs (including the block markers) become clean
    // paragraph breaks.
    let paragraphs: Vec<&str> = text
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let mut collapsed = String::new();
    for line in &paragraphs {
        if !collapsed.is_empty() {
            collapsed.push_str("\n\n");
        }
        collapsed.push_str(&line.split_whitespace().collect::<Vec<_>>().join(" "));
    }
    (first_heading, collapsed)
}

/// A pull parser over the events this extractor needs — start/end names
/// with attributes, and text runs. Anything malformed ends the stream
/// (the prose collected so far still counts).
enum XmlEvent {
    Start(String, Vec<(String, String)>),
    End(String),
    Text(String),
    Other,
}

fn xml_events(input: &str) -> Vec<XmlEvent> {
    use quick_xml::Reader;
    use quick_xml::events::{BytesStart, Event};

    fn start_of(e: &BytesStart) -> XmlEvent {
        XmlEvent::Start(
            local_name(e.name().as_ref()),
            e.attributes()
                .flatten()
                .map(|a| (local_name(a.key.as_ref()), a.value.into_owned()))
                .collect(),
        )
    }

    let mut reader = Reader::from_str(input);
    reader.config_mut().trim_text(false);
    let mut out = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => out.push(start_of(&e)),
            Ok(Event::Empty(e)) => out.push(start_of(&e)),
            Ok(Event::End(e)) => out.push(XmlEvent::End(local_name(e.name().as_ref()))),
            Ok(Event::Text(t)) => out.push(XmlEvent::Text(t.as_ref().to_string())),
            Ok(Event::CData(t)) => out.push(XmlEvent::Text(t.as_ref().to_string())),
            // Entities arrive as their own events (`&amp;` → GeneralRef
            // "amp"); decode them back into the text stream.
            Ok(Event::GeneralRef(r)) => out.push(XmlEvent::Text(decode_entities(&format!(
                "&{};",
                r.as_ref()
            )))),
            Ok(Event::Eof) | Err(_) => break,
            _ => out.push(XmlEvent::Other),
        }
    }
    out
}

/// Strip an XML namespace: `dc:title` → `title`.
fn local_name(name: &str) -> String {
    match name.rsplit(':').next() {
        Some(local) => local.to_string(),
        None => name.to_string(),
    }
}

fn attr(attrs: &[(String, String)], key: &str) -> Option<String> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
}

/// Decode the XML entities prose actually contains.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(semi) = rest.find(';') else {
            out.push_str(rest);
            return out;
        };
        let entity = &rest[1..semi];
        match entity {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ => {
                if let Some(hex) = entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                    && let Ok(code) = u32::from_str_radix(hex, 16)
                    && let Some(c) = char::from_u32(code)
                {
                    out.push(c);
                } else if let Some(dec) = entity.strip_prefix('#')
                    && let Ok(code) = dec.parse::<u32>()
                    && let Some(c) = char::from_u32(code)
                {
                    out.push(c);
                } else {
                    out.push_str(&rest[..=semi]);
                }
            }
        }
        rest = &rest[semi + 1..];
    }
    out.push_str(rest);
    out
}

/// Resolve `a/b/../c` → `a/c` (EPUB hrefs are relative to the OPF).
fn normalize_rel(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rootfile_and_entities_and_rel_paths() {
        let container = r#"<container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#;
        assert_eq!(
            rootfile_path(container),
            Some(PathBuf::from("OEBPS/content.opf"))
        );
        assert_eq!(
            decode_entities("a &amp; b &#65; &#x42; &lt;x&gt;"),
            "a & b A B <x>"
        );
        assert_eq!(
            normalize_rel(Path::new("OEBPS/../TEXT/ch1.xhtml")),
            PathBuf::from("TEXT/ch1.xhtml")
        );
    }

    #[test]
    fn xhtml_becomes_prose_with_heading() {
        let xhtml = r#"<html><head><title>ignored</title></head><body>
            <h1>Chapter One</h1><p>Fog &amp; tide.</p><p>Second   paragraph.</p>
            <script>var x = 1;</script></body></html>"#;
        let (heading, text) = xhtml_to_text(xhtml);
        assert_eq!(heading.as_deref(), Some("Chapter One"));
        assert_eq!(text, "Fog & tide.\n\nSecond paragraph.");
        assert!(!text.contains("var x"), "script subtrees never contribute");
        assert!(!text.contains("ignored"), "head subtrees never contribute");
    }

    #[test]
    fn empty_markup_collapses_neatly() {
        let (heading, text) =
            xhtml_to_text("<html><body><p>   </p><p>Only line.</p></body></html>");
        assert_eq!(heading, None);
        assert_eq!(text, "Only line.");
    }
}
