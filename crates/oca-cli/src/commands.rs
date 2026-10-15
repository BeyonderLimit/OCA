//! Phase 1 command implementations — every mutation goes through
//! `oca_core::Core` (AGENTS rule 8; the CLI is the Core's first client).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use oca_core::{Command as CoreCommand, Core, Event};
use oca_model::{DocumentKind, ProjectKind};
use oca_music::CheckReport;
use oca_storage::registry::Registry;

use crate::cli::{ChapterCommand, Command, SongCommand, StoryCommand};

pub fn dispatch(cmd: Command, home: Option<&Path>) -> Result<u8> {
    match cmd {
        Command::New { name, path, kind } => new_project(&name, path.as_deref(), &kind, home),
        Command::List => list_projects(home),
        Command::Open { project } => open_tui(project.as_deref()),
        Command::Chapter { cmd } => chapter(cmd),
        Command::Story { cmd } => story(cmd),
        Command::Song { cmd } => song(cmd),
        Command::Doctor => unreachable!("doctor is dispatched from main"),
    }
}

// ---- project discovery -----------------------------------------------------

/// `--project` > cwd-is-a-project > error.
fn find_project(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        let p =
            std::fs::canonicalize(p).with_context(|| format!("cannot access {}", p.display()))?;
        if !p.join("project.yaml").is_file() {
            bail!("not an OCA project: {}", p.display());
        }
        return Ok(p);
    }
    let cwd = std::env::current_dir().context("current directory")?;
    if cwd.join("project.yaml").is_file() {
        return Ok(cwd);
    }
    bail!(
        "no project here — run inside a project directory or pass --project <path> \
         (oca list shows known projects)"
    )
}

/// `oca open` argument: a path first, then a registry name.
fn resolve_open_arg(arg: Option<&str>) -> Result<PathBuf> {
    if let Some(a) = arg {
        let as_path = Path::new(a);
        if as_path.join("project.yaml").is_file() {
            return Ok(std::fs::canonicalize(as_path)?);
        }
        if let Some(home) = crate::config::home_dir()
            && let Ok(reg) = Registry::load(&home)
            && let Some(e) = reg.entries().iter().find(|e| e.name == a)
        {
            let p = PathBuf::from(&e.path);
            if p.join("project.yaml").is_file() {
                return Ok(p);
            }
            bail!("registered project '{}' is missing on disk: {}", a, e.path);
        }
        bail!("no project named or at '{a}'");
    }
    find_project(None)
}

/// Open a project through Core; prints the read-only note when relevant.
fn open_core(root: &Path) -> Result<Core> {
    let mut core = Core::new();
    let events = core.execute(CoreCommand::OpenProject {
        root: root.to_path_buf(),
    })?;
    if let Event::ProjectOpened {
        read_only: true,
        reason,
        ..
    } = &events[0]
    {
        eprintln!(
            "note: project is read-only — {}",
            reason.as_deref().unwrap_or("another session is editing it")
        );
    }
    Ok(core)
}

// ---- commands --------------------------------------------------------------

fn new_project(name: &str, path: Option<&Path>, kind: &str, home: Option<&Path>) -> Result<u8> {
    let kind = ProjectKind::parse(kind)?;
    let base = match path {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir().context("current directory")?,
    };
    let root = base.join(name);

    let mut core = Core::new();
    let events = core.execute(CoreCommand::CreateProject {
        root: root.clone(),
        title: name.to_string(),
        kind,
    })?;
    let Event::ProjectCreated { root, project_id } = &events[0] else {
        bail!("unexpected result creating project");
    };

    // Registry is user-level state (D3): a simple file, best effort.
    if let Some(home) = home {
        let mut reg = Registry::load(home)?;
        let dir_name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| name.to_string());
        reg.upsert(&dir_name, root)?;
        reg.save(home)?;
    }

    println!("created project '{name}' ({project_id})");
    println!("  {}", root.display());
    println!(
        "next: oca open {}",
        root.file_name().unwrap_or_default().to_string_lossy()
    );
    Ok(0)
}

fn list_projects(home: Option<&Path>) -> Result<u8> {
    let Some(home) = home else {
        bail!("cannot locate home directory for the project registry");
    };
    let reg = Registry::load(home)?;
    if reg.entries().is_empty() {
        println!("no projects yet — create one with: oca new <name>");
        return Ok(0);
    }
    for e in reg.entries() {
        let state = if Path::new(&e.path).join("project.yaml").is_file() {
            ""
        } else {
            "  [missing]"
        };
        println!("{:<24} {}{}", e.name, e.path, state);
    }
    Ok(0)
}

fn open_tui(project: Option<&str>) -> Result<u8> {
    let root = resolve_open_arg(project)?;
    let mut core = open_core(&root)?;
    oca_tui::run(&mut core)?;
    Ok(0)
}

fn chapter(cmd: ChapterCommand) -> Result<u8> {
    match cmd {
        ChapterCommand::New { title, project } => {
            let root = find_project(project.as_deref())?;
            let mut core = open_core(&root)?;
            let events = core.execute(CoreCommand::CreateDocument {
                kind: DocumentKind::Chapter,
                title: title.clone(),
            })?;
            let Event::DocumentCreated { rel, .. } = &events[0] else {
                bail!("unexpected result creating chapter");
            };
            let id = core
                .project()
                .and_then(|p| p.peek(rel).ok().and_then(|(id, _)| id))
                .unwrap_or_default();
            println!("created {} ({id})", rel.display());
            Ok(0)
        }
        ChapterCommand::Append { doc, text, project } => {
            let root = find_project(project.as_deref())?;
            let mut core = open_core(&root)?;
            let rel = core.resolve(&doc)?;
            let text = text.join(" ");
            core.execute(CoreCommand::AppendParagraph {
                doc: rel.clone(),
                text,
            })?;
            println!("saved {}", rel.display());
            Ok(0)
        }
        ChapterCommand::Show { doc, project } => {
            let root = find_project(project.as_deref())?;
            let mut core = open_core(&root)?;
            let rel = core.resolve(&doc)?;
            let document = core.read_document(&rel)?;
            print!("{}", document.raw);
            Ok(0)
        }
    }
}

// ---- story queries (Phase 2) -----------------------------------------------

fn story(cmd: StoryCommand) -> Result<u8> {
    match cmd {
        StoryCommand::Who { character, project } => {
            let core = story_core(project.as_deref())?;
            let profile = oca_story::Story::new(&core).who_is(&character)?;
            let c = &profile.character;
            println!("{}  (id: {})", c.name, c.id);
            match &c.role {
                Some(role) => println!("  role: {role}"),
                None => println!("  role: —"),
            }
            let aliases = if c.aliases.is_empty() {
                "—".to_string()
            } else {
                c.aliases.join(", ")
            };
            println!("  aliases: {aliases}");
            if c.relationships.is_empty() {
                println!("  relationships: —");
            }
            for r in &c.relationships {
                println!("  relationship: {} — {}", r.target, r.kind);
            }
            match &profile.at {
                Some(ch) if !profile.state.is_empty() => {
                    println!("  state at {}:", ch.title);
                    for (k, v) in &profile.state {
                        println!("    {k}: {v}");
                    }
                }
                _ => println!("  state: —"),
            }
            Ok(0)
        }
        StoryCommand::Where {
            character,
            at,
            project,
        } => {
            let core = story_core(project.as_deref())?;
            let view = oca_story::Story::new(&core).where_is(&character, at.as_deref())?;
            println!("{} at {}:", view.character.name, view.chapter.title);
            if view.fields.is_empty() {
                println!("  (no state recorded yet)");
            }
            for (k, v) in &view.fields {
                println!("  {k}: {v}");
            }
            Ok(0)
        }
        StoryCommand::ChaptersWith { character, project } => {
            let core = story_core(project.as_deref())?;
            let story = oca_story::Story::new(&core);
            let c = story.character(&character)?;
            let hits = story.chapters_containing(&character)?;
            if hits.is_empty() {
                println!("no chapter mentions {} or an alias", c.name);
                return Ok(0);
            }
            println!("{} chapter(s) mention {} or an alias:", hits.len(), c.name);
            for ch in hits {
                println!("  {:03}  {}", ch.seq, ch.title);
            }
            Ok(0)
        }
        StoryCommand::Before {
            chapter,
            sense,
            project,
        } => {
            let core = story_core(project.as_deref())?;
            let sense = oca_story::TimeSense::parse(&sense)?;
            let story = oca_story::Story::new(&core);
            let target = story.chapter(&chapter)?;
            let chs = story.chapters()?;
            let evs = story.events_before(&chapter, sense)?;
            let clock = match sense {
                oca_story::TimeSense::Narrative => "narrative position",
                oca_story::TimeSense::StoryTime => "story time",
            };
            if evs.is_empty() {
                println!("no events before \"{}\" by {clock}", target.title);
                return Ok(0);
            }
            println!(
                "{} event(s) before \"{}\" by {clock}:",
                evs.len(),
                target.title
            );
            for e in &evs {
                let shown = e
                    .chapter
                    .as_deref()
                    .and_then(|id| chs.iter().find(|c| c.id == id))
                    .map(|c| c.title.as_str())
                    .unwrap_or("—");
                println!("  {}  {}  {}", e.story_time, shown, e.title);
            }
            Ok(0)
        }
    }
}

/// Open the project for a story query (queries never mutate, but opening
/// still takes the single-writer lock so the answer sees fresh files).
fn story_core(project: Option<&Path>) -> Result<Core> {
    let root = find_project(project)?;
    open_core(&root)
}

// ---- song (Phase 3) ---------------------------------------------------------

fn song(cmd: SongCommand) -> Result<u8> {
    match cmd {
        SongCommand::New { title, project } => {
            let root = find_project(project.as_deref())?;
            let mut core = open_core(&root)?;
            let slug = oca_storage::layout::slugify(&title);
            let entry = oca_music::Music::new_song(&mut core, &slug, &title)?;
            println!("created song \"{title}\" ({})", entry.slug());
            println!("  {}", entry.dir_rel.join("song.yaml").display());
            println!("  {}", entry.dir_rel.join("lyrics.md").display());
            Ok(0)
        }
        SongCommand::Check { song, project } => {
            let core = story_core(project.as_deref())?;
            let report = oca_music::Music::new(&core).check(&song)?;
            print_check(&report);
            Ok(if report.is_clean() { 0 } else { 1 })
        }
        SongCommand::SaveVersion {
            song,
            label,
            project,
        } => {
            let root = find_project(project.as_deref())?;
            let mut core = open_core(&root)?;
            let v = oca_music::Music::save_version(&mut core, &song, label.as_deref())?;
            match &v.label {
                Some(l) => println!("saved {} — {}", v.file, l),
                None => println!("saved {}", v.file),
            }
            Ok(0)
        }
    }
}

/// Render a check report for the terminal (pure display; the analysis lives
/// in `oca-music`).
fn print_check(r: &CheckReport) {
    println!("{}  (language: {})", r.title, r.language);
    if !r.language_supported {
        println!(
            "  syllable counting: unsupported language \"{}\" — \
             structure and drift checks still ran",
            r.language
        );
    }
    if let Some(d) = &r.drift {
        let at = d.first_difference;
        println!(
            "  plan drift at position {at}: plan [{}], lyrics [{}]",
            d.plan.join(", "),
            d.actual.join(", ")
        );
    } else if !r.sections.is_empty() {
        println!("  plan: matches the section list");
    }
    for w in &r.structure {
        match w.line {
            Some(n) => println!("  structure (line {n}): {}", w.message),
            None => println!("  structure: {}", w.message),
        }
    }
    println!("  sections:");
    for s in &r.sections {
        let tag = if s.is_reference { " (repeat)" } else { "" };
        match s.syllables {
            Some(syl) => println!("    {}{tag}: {} lines, {syl} syllables", s.label, s.lines),
            None => println!(
                "    {}{tag}: {} lines, syllables unsupported",
                s.label, s.lines
            ),
        }
    }
    if let Some(t) = r.total_syllables {
        let meter = r.meter.map(|m| format!(", meter ~{m}")).unwrap_or_default();
        println!("  total: {t} syllables{meter}");
    }
    // A9: no numbers at all beats wrong numbers — an unsupported language
    // simply shows no total.
    if !r.repetitions.is_empty() {
        println!("  repeated lines:");
        for rep in &r.repetitions {
            let where_ = rep
                .occurrences
                .iter()
                .map(|(_, n)| n.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            println!("    \"{}\" (lines {where_})", rep.text);
        }
    }
    if !r.rhymes.is_empty() {
        println!("  rhymes:");
        for h in &r.rhymes {
            println!(
                "    {} ({:?}, line {}, {})",
                h.word, h.kind, h.line, h.section
            );
        }
    }
}
