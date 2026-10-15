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

    /// Report the health of each OCA subsystem (warnings are informational; exit 0).
    Doctor,
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
