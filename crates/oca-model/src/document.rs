//! `Document` — the in-memory model of one Markdown file. `raw` is always the
//! exact file bytes; parsing never rewrites it (round-trip identity by
//! construction).

use crate::frontmatter::Frontmatter;

/// 1-based inclusive line span in the full file (frontmatter included).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

impl LineRange {
    pub fn new(start: u32, end: u32) -> Self {
        Self {
            start,
            end: end.max(start),
        }
    }

    pub fn contains(&self, line: u32) -> bool {
        line >= self.start && line <= self.end
    }
}

/// Block-level structure of the body (design §4.4). Line ranges power the
/// TUI outline and future range-targeted edits.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        text: String,
        lines: LineRange,
    },
    Paragraph {
        preview: String,
        lines: LineRange,
    },
    Quote {
        lines: LineRange,
    },
    /// A paragraph that is exactly one image (`![alt](src)`).
    Image {
        alt: String,
        source: String,
        lines: LineRange,
    },
    Table {
        lines: LineRange,
    },
    Code {
        info: String,
        lines: LineRange,
    },
    List {
        ordered: bool,
        lines: LineRange,
    },
    /// Raw HTML block.
    Html {
        lines: LineRange,
    },
    /// An HTML block whose content is a comment (`<!-- ... -->`) — annotations
    /// the AI may propose to add; ordinary saves must leave them alone.
    Comment {
        preview: String,
        lines: LineRange,
    },
    ThematicBreak {
        lines: LineRange,
    },
}

impl Block {
    pub fn lines(&self) -> LineRange {
        match self {
            Block::Heading { lines, .. }
            | Block::Paragraph { lines, .. }
            | Block::Quote { lines }
            | Block::Image { lines, .. }
            | Block::Table { lines }
            | Block::Code { lines, .. }
            | Block::List { lines, .. }
            | Block::Html { lines }
            | Block::Comment { lines, .. }
            | Block::ThematicBreak { lines } => *lines,
        }
    }

    /// Headings only — what the outline tree renders.
    pub fn as_heading(&self) -> Option<(u8, &str, LineRange)> {
        match self {
            Block::Heading { level, text, lines } => Some((*level, text, *lines)),
            _ => None,
        }
    }
}

/// Heading + the block lines it spans, derived for tree rendering.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub level: u8,
    pub title: String,
    pub lines: LineRange,
}

/// A `[[target]]` / `[[target|alias]]` wiki link resolved by id, never path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WikiLink {
    pub target: String,
    pub alias: Option<String>,
    pub line: u32,
}

/// One project document: exact bytes + parsed views.
#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    /// Exact file bytes — what gets written back on save.
    pub raw: String,
    pub frontmatter: Frontmatter,
    pub blocks: Vec<Block>,
    pub links: Vec<WikiLink>,
    /// Line number where the body starts (first line after the frontmatter).
    pub body_start_line: u32,
    /// Byte offset where the body starts (after the frontmatter fences).
    pub body_start: usize,
}

impl Document {
    /// Body text (everything after the frontmatter), byte-exact.
    pub fn body(&self) -> &str {
        self.raw.get(self.body_start..).unwrap_or("")
    }

    pub fn word_count(&self) -> usize {
        self.body().split_whitespace().count()
    }

    /// Derived heading hierarchy for the outline/project tree.
    pub fn sections(&self) -> Vec<Section> {
        self.blocks
            .iter()
            .filter_map(|b| {
                b.as_heading().map(|(level, text, lines)| Section {
                    level,
                    title: text.to_string(),
                    lines,
                })
            })
            .collect()
    }
}
