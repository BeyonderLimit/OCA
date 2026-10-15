//! Command-line surface for OCA.
//!
//! Thin by design (AGENTS rule 8): all behavior lives behind the Core API;
//! this crate only parses arguments and dispatches.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// OCA — Offline Creative Assistant: an offline-first writing & songwriting studio.
#[derive(Debug, Parser)]
#[command(name = "oca", version, about, long_about = None)]
pub struct Cli {
    /// Increase log verbosity: -v = info, -vv = debug, -vvv = trace (default: warn).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Allow project content (manuscript/lyrics text) in log files.
    /// Off by default: logs never contain project content unless passed (A7).
    #[arg(long, global = true)]
    pub log_content: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a new project (registers it for `oca list` / `oca open`).
    New {
        /// Directory name for the project (created under --path or the cwd).
        name: String,
        /// Parent directory to create the project in (default: cwd).
        #[arg(long)]
        path: Option<PathBuf>,
        /// Project kind.
        #[arg(long, default_value = "novel")]
        kind: String,
    },

    /// List registered projects.
    List,

    /// Open a project in the TUI (name from `oca list`, or a path).
    Open {
        /// Project name or directory (default: the project in the cwd).
        project: Option<String>,
    },

    /// Chapter documents.
    Chapter {
        #[command(subcommand)]
        cmd: ChapterCommand,
    },

    /// Deterministic story queries: state, mentions, timeline (Phase 2 —
    /// no AI involved).
    Story {
        #[command(subcommand)]
        cmd: StoryCommand,
    },

    /// Song tools: offline lyric analysis (Phase 3 — no AI involved).
    Song {
        #[command(subcommand)]
        cmd: SongCommand,
    },

    /// Audio takes: record, play, and link voice notes to chapters and
    /// songs (Phase 4 — no AI involved).
    Audio {
        #[command(subcommand)]
        cmd: AudioCommand,
    },

    /// Rebuild the cache index from the files (files are canonical; the
    /// index in `.oca/cache/oca.db` is derived — Phase 5).
    Reindex {
        /// Reparse every file, even ones that did not change.
        #[arg(long)]
        full: bool,
        /// Project directory (default: the project in the cwd).
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Full-text search across chapters, characters, world, outline, songs,
    /// lyrics, and audio notes; `tag:ns:value` searches tags instead.
    Search {
        /// Words to search for (the last word matches as a prefix).
        query: Vec<String>,
        /// Maximum number of hits to show.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Project directory (default: the project in the cwd).
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Read or edit a document's tags (`namespace:value`, e.g.
    /// `character:elena`).
    Tag {
        #[command(subcommand)]
        cmd: TagCommand,
    },

    /// List every tag in the project with usage counts.
    Tags {
        /// Project directory (default: the project in the cwd).
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Show a document's outgoing references and its backlinks (by id or
    /// title — links never use paths).
    Links {
        /// Document id or title.
        target: String,
        /// Project directory (default: the project in the cwd).
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Bring the project's file format up to date (a no-op when current).
    Migrate {
        /// Project directory (default: the project in the cwd).
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Report the health of each OCA subsystem (warnings are informational; exit 0).
    Doctor,
}

#[derive(Debug, Subcommand)]
pub enum TagCommand {
    /// Show the tags on a document.
    List {
        /// Document id or title.
        doc: String,
        /// Project directory (default: the project in the cwd).
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Add a tag to a document (frontmatter patch; comments and unknown
    /// keys survive).
    Add {
        /// Document id or title.
        doc: String,
        /// Tag to add — `namespace:value` or a bare word.
        tag: String,
        /// Project directory (default: the project in the cwd).
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Remove a tag from a document.
    Remove {
        /// Document id or title.
        doc: String,
        /// Tag to remove.
        tag: String,
        /// Project directory (default: the project in the cwd).
        #[arg(long)]
        project: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ChapterCommand {
    /// Create a chapter (adds frontmatter with a fresh id).
    New {
        title: String,
        /// Project directory (default: the project in the cwd).
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Append a paragraph to a chapter (by id, title, or path).
    Append {
        /// Chapter id (`ch_…`), title, or project-relative path.
        doc: String,
        /// Paragraph text.
        #[arg(num_args = 1.., required = true)]
        text: Vec<String>,
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Print a chapter's exact bytes to stdout.
    Show {
        /// Chapter id (`ch_…`), title, or project-relative path.
        doc: String,
        #[arg(long)]
        project: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub enum StoryCommand {
    /// "Who is Marcus?" — identity, relationships, and current state.
    Who {
        /// Character id, name, alias, or path.
        character: String,
        /// Project directory (default: the project in the cwd).
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// "Where is Elena?" — carry-forward state (at the last chapter by default).
    Where {
        /// Character id, name, alias, or path.
        character: String,
        /// Chapter to resolve the state at (number, id, title, or path).
        #[arg(long)]
        at: Option<String>,
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// "Which chapters contain Elena?" — name + alias mentions in chapter bodies.
    ChaptersWith {
        /// Character id, name, alias, or path.
        character: String,
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// "What happened before <chapter>?" — timeline events in one time sense.
    Before {
        /// Chapter to stop at (number, id, title, or path).
        chapter: String,
        /// Which clock: `narrative` (book order) or `story` (in-world time).
        #[arg(long, default_value = "narrative")]
        sense: String,
        #[arg(long)]
        project: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub enum SongCommand {
    /// Scaffold a new song (song.yaml + lyrics.md skeleton).
    New {
        /// Song title (the slug derives from it).
        title: String,
        /// Project directory (default: the project in the cwd).
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Offline lyric analysis: rhyme scheme, structure, syllable counts.
    Check {
        /// Song id, title, slug, or project-relative path.
        song: String,
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Snapshot the current lyrics as `versions/vNN.md`.
    SaveVersion {
        /// Song id, title, slug, or project-relative path.
        song: String,
        /// Optional human label recorded in `versions/index.yaml`.
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        project: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub enum AudioCommand {
    /// List every take (title, id, duration, attachment).
    List {
        /// Project directory (default: the project in the cwd).
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Record from the default input device until Enter is pressed.
    /// (Pause/resume are TUI controls.)
    Record {
        /// Take title; also becomes the filename.
        #[arg(long)]
        title: Option<String>,
        /// Attach after saving: `project`, `chapter:<chapter>`, or
        /// `song:<song>` (validated before recording starts).
        #[arg(long)]
        attach: Option<String>,
        /// Directory for the new take (default: `audio/`).
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Play a take through the default output device.
    Play {
        /// Take id, title, or filename stem.
        take: String,
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Rename a take's files (the id and every attachment stay).
    Rename {
        /// Take id, title, or filename stem.
        take: String,
        /// New filename stem (lowercase letters, digits, dashes).
        stem: String,
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Delete a take's audio and metadata.
    Delete {
        /// Take id, title, or filename stem.
        take: String,
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Link a take to the project, a chapter, or a song.
    Attach {
        /// Take id, title, or filename stem.
        take: String,
        /// `project`, `chapter:<chapter>`, or `song:<song>`.
        target: String,
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Remove a take's attachment.
    Detach {
        /// Take id, title, or filename stem.
        take: String,
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Edit a take's Creative Note (with no flags: print the current note).
    Note {
        /// Take id, title, or filename stem.
        take: String,
        /// Note text (replaces the current text).
        #[arg(long)]
        text: Option<String>,
        /// Idea lyric line (replaces the current one).
        #[arg(long)]
        lyrics: Option<String>,
        /// Tempo in BPM (replaces the current one).
        #[arg(long)]
        tempo: Option<u32>,
        #[arg(long)]
        project: Option<PathBuf>,
    },
}
