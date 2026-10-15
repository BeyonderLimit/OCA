//! Phase 1 command implementations — every mutation goes through
//! `oca_core::Core` (AGENTS rule 8; the CLI is the Core's first client).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use oca_core::{Command as CoreCommand, Core, Event};
use oca_model::{DocumentKind, ProjectKind};
use oca_music::CheckReport;
use oca_storage::registry::Registry;

use crate::cli::{AudioCommand, ChapterCommand, Command, SongCommand, StoryCommand, TagCommand};

pub fn dispatch(cmd: Command, home: Option<&Path>) -> Result<u8> {
    match cmd {
        Command::New { name, path, kind } => new_project(&name, path.as_deref(), &kind, home),
        Command::List => list_projects(home),
        Command::Open { project } => open_tui(project.as_deref()),
        Command::Chapter { cmd } => chapter(cmd),
        Command::Story { cmd } => story(cmd),
        Command::Song { cmd } => song(cmd),
        Command::Audio { cmd } => audio(cmd),
        Command::Reindex { full, project } => reindex(full, project.as_deref()),
        Command::Search {
            query,
            limit,
            project,
        } => search(&query.join(" "), limit, project.as_deref()),
        Command::Tag { cmd } => tag(cmd),
        Command::Tags { project } => tags(project.as_deref()),
        Command::Links { target, project } => links(&target, project.as_deref()),
        Command::Migrate { project } => migrate(project.as_deref()),
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

// ---- audio (Phase 4) -------------------------------------------------------

fn audio(cmd: AudioCommand) -> Result<u8> {
    match cmd {
        AudioCommand::List { project } => {
            let core = story_core(project.as_deref())?;
            let facade = oca_audio::Audio::new(&core, Arc::new(oca_audio::CpalHost))?;
            print_takes(&facade.takes()?);
            Ok(0)
        }
        AudioCommand::Record {
            title,
            attach,
            dir,
            project,
        } => {
            let root = find_project(project.as_deref())?;
            let core = open_core(&root)?;
            let facade = oca_audio::Audio::new(&core, Arc::new(oca_audio::CpalHost))?;
            // Resolve the attach target BEFORE recording, so a typo costs
            // nothing but a retype.
            let target = match attach {
                Some(spec) => Some(facade.resolve_target(&spec)?),
                None => None,
            };
            let dir_rel = dir.unwrap_or_else(oca_audio::default_take_dir);
            let recorder = facade.record()?;
            println!(
                "recording ({} Hz, {} channel(s)) — press Enter to stop",
                recorder.spec().sample_rate,
                recorder.spec().channels
            );
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .context("cannot read the stop signal")?;
            let done = recorder.stop();
            let take = facade.save_take(&done, &dir_rel, title.as_deref())?;
            let take = match target {
                Some(target) => facade.attach(&take, &target)?,
                None => take,
            };
            println!(
                "saved {} ({}, {:.1}s)",
                take.stem(),
                take.meta.take_id,
                take.duration_secs()
            );
            if let Some(attached) = &take.meta.attached {
                println!("attached to {}", attached.describe());
            }
            Ok(0)
        }
        AudioCommand::Play { take, project } => {
            let core = story_core(project.as_deref())?;
            let facade = oca_audio::Audio::new(&core, Arc::new(oca_audio::CpalHost))?;
            let take = facade.take(&take)?;
            println!("playing {} ({:.1}s)", take.stem(), take.duration_secs());
            let mut player = facade.play(&take)?;
            player.wait_done()?;
            Ok(0)
        }
        AudioCommand::Rename {
            take,
            stem,
            project,
        } => {
            let core = story_core(project.as_deref())?;
            let facade = oca_audio::Audio::new(&core, Arc::new(oca_audio::CpalHost))?;
            let take = facade.take(&take)?;
            let renamed = facade.rename(&take, &stem)?;
            println!(
                "renamed to {} ({})",
                renamed.wav_rel.display(),
                renamed.meta.take_id
            );
            Ok(0)
        }
        AudioCommand::Delete { take, project } => {
            let core = story_core(project.as_deref())?;
            let facade = oca_audio::Audio::new(&core, Arc::new(oca_audio::CpalHost))?;
            let take = facade.take(&take)?;
            let stem = take.stem();
            facade.delete(&take)?;
            println!("deleted {stem}");
            Ok(0)
        }
        AudioCommand::Attach {
            take,
            target,
            project,
        } => {
            let core = story_core(project.as_deref())?;
            let facade = oca_audio::Audio::new(&core, Arc::new(oca_audio::CpalHost))?;
            let take = facade.take(&take)?;
            let target = facade.resolve_target(&target)?;
            let take = facade.attach(&take, &target)?;
            println!(
                "{} → {}",
                take.stem(),
                take.meta.attached.as_ref().unwrap().describe()
            );
            Ok(0)
        }
        AudioCommand::Detach { take, project } => {
            let core = story_core(project.as_deref())?;
            let facade = oca_audio::Audio::new(&core, Arc::new(oca_audio::CpalHost))?;
            let take = facade.take(&take)?;
            let take = facade.detach(&take)?;
            println!("{} is unattached", take.stem());
            Ok(0)
        }
        AudioCommand::Note {
            take,
            text,
            lyrics,
            tempo,
            project,
        } => {
            let core = story_core(project.as_deref())?;
            let facade = oca_audio::Audio::new(&core, Arc::new(oca_audio::CpalHost))?;
            let take = facade.take(&take)?;
            if text.is_none() && lyrics.is_none() && tempo.is_none() {
                match &take.meta.note {
                    Some(note) => {
                        if let Some(t) = &note.text {
                            println!("text: {t}");
                        }
                        if let Some(l) = &note.lyrics {
                            println!("lyrics: {l}");
                        }
                        if let Some(bpm) = note.tempo_bpm {
                            println!("tempo: {bpm} BPM");
                        }
                    }
                    None => println!("no note on {}", take.stem()),
                }
                return Ok(0);
            }
            let take = facade.update_note(&take, text.as_deref(), lyrics.as_deref(), tempo)?;
            println!("updated the note on {}", take.stem());
            Ok(0)
        }
    }
}

fn print_takes(takes: &[oca_audio::Take]) {
    if takes.is_empty() {
        println!("no takes yet — record one with `oca audio record` or in the TUI");
        return;
    }
    for take in takes {
        let attached = take
            .meta
            .attached
            .as_ref()
            .map(|a| a.describe())
            .unwrap_or_else(|| "unattached".into());
        println!(
            "{:<24} {:<32} {:>6.1}s  {:<20} {}",
            take.stem(),
            take.meta.take_id,
            take.duration_secs(),
            attached,
            take.wav_rel.display()
        );
    }
}

// ---- cache index: reindex, search, tags, links, migrate (Phase 5) -----------

/// `oca reindex` — the DB is derived state; this rebuilds it from the files.
/// Default is the incremental reconcile (only changed files are reparsed);
/// `--full` forces a from-scratch rebuild.
fn reindex(full: bool, project: Option<&Path>) -> Result<u8> {
    let root = find_project(project)?;
    let mut core = open_core(&root)?;
    let started = std::time::Instant::now();
    let stats = if full {
        core.reindex()?
    } else {
        core.reconcile()?
    };
    let elapsed_ms = started.elapsed().as_millis();
    println!(
        "{}: {} file(s) indexed, {} removed — {} ms",
        if full { "full reindex" } else { "reindex" },
        stats.updated,
        stats.removed,
        elapsed_ms
    );
    if let Some(t) = core.index_totals() {
        println!(
            "{} documents · {} searchable units · {} tags · {} links · {} takes · {} songs · {} characters · {} events",
            t.documents, t.units, t.tags, t.links, t.takes, t.songs, t.characters, t.events
        );
    }
    Ok(0)
}

fn search(query: &str, limit: usize, project: Option<&Path>) -> Result<u8> {
    let query = query.trim();
    if query.is_empty() {
        bail!("nothing to search for — usage: oca search <words>");
    }
    let root = find_project(project)?;
    let core = open_core(&root)?;
    let hits = core.search(query, limit)?;
    if hits.is_empty() {
        println!("no matches for \"{query}\"");
        return Ok(0);
    }
    for hit in &hits {
        println!(
            "{:<9} {:<26} {}",
            hit.kind,
            ellipsize(&hit.title, 26),
            hit.rel.display()
        );
        if !hit.snippet.is_empty() {
            println!("          {}", hit.snippet.replace('\n', " "));
        }
    }
    println!("{} match(es)", hits.len());
    Ok(0)
}

fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

fn tags(project: Option<&Path>) -> Result<u8> {
    let core = open_core(&find_project(project)?)?;
    let list = core.tags()?;
    if list.is_empty() {
        println!("no tags yet — add one with `oca tag add <doc> <namespace:value>`");
        return Ok(0);
    }
    for t in list {
        println!("{:<32} {} document(s)", t.tag, t.uses);
    }
    Ok(0)
}

fn tag(cmd: TagCommand) -> Result<u8> {
    match cmd {
        TagCommand::List { doc, project } => {
            let mut core = story_core(project.as_deref())?;
            let rel = core.resolve(&doc)?;
            let document = core.read_document(&rel)?;
            let tags = document.frontmatter.get_seq("tags").unwrap_or_default();
            if tags.is_empty() {
                println!("no tags on {}", rel.display());
            } else {
                for t in tags {
                    println!("{t}");
                }
            }
            Ok(0)
        }
        TagCommand::Add { doc, tag, project } => set_tag(&doc, &tag, true, project.as_deref()),
        TagCommand::Remove { doc, tag, project } => set_tag(&doc, &tag, false, project.as_deref()),
    }
}

/// Deterministic tag editing: frontmatter patch through Core, then the save
/// path reindexes. Case-insensitive membership; the stored spelling is
/// preserved for pre-existing tags.
fn set_tag(doc: &str, tag: &str, add: bool, project: Option<&Path>) -> Result<u8> {
    validate_tag(tag)?;
    let mut core = story_core(project)?;
    let rel = core.resolve(doc)?;
    let mut document = core.read_document(&rel)?;
    let mut tags = document.frontmatter.get_seq("tags").unwrap_or_default();
    let lower = tag.to_ascii_lowercase();
    let present = tags.iter().any(|t| t.to_ascii_lowercase() == lower);
    if add && present {
        println!("{tag} is already on {}", rel.display());
        return Ok(0);
    }
    if !add && !present {
        if tags.is_empty() {
            bail!("{} has no tags", rel.display());
        }
        bail!(
            "{tag} is not on {} (tags: {})",
            rel.display(),
            tags.join(", ")
        );
    }
    if add {
        tags.push(tag.to_string());
    } else {
        tags.retain(|t| t.to_ascii_lowercase() != lower);
    }
    // Patch `tags:` as a sequence block directly (single-item lists included
    // — a scalar `set_str` path would quote them), or drop the whole block
    // when the last tag goes. Either way only `tags:` is rewritten, so
    // comments and unknown keys survive.
    let body = document.body().to_string();
    if tags.is_empty() {
        document.frontmatter.remove_key("tags")?;
    } else {
        let block = tags
            .iter()
            .map(|t| format!("- {t}"))
            .collect::<Vec<_>>()
            .join("\n");
        document.frontmatter.set_block("tags", &block)?;
    }
    let raw = oca_markdown::with_body(&document, &body);
    core.execute(CoreCommand::SaveDocument {
        doc: rel.clone(),
        raw,
    })?;
    if add {
        println!("tagged {} → {tag}", rel.display());
    } else if tags.is_empty() {
        println!("removed {tag} (no tags left)");
    } else {
        println!("removed {tag} from {}", rel.display());
    }
    Ok(0)
}

/// `namespace:value` or a bare word; letters/digits/`_`/`-` only, at most
/// one `:` — so a tag can never smuggle YAML syntax into a frontmatter
/// patch.
fn validate_tag(tag: &str) -> Result<()> {
    let parts: Vec<&str> = tag.split(':').collect();
    let ok = parts.len() <= 2
        && parts.iter().all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
        && !tag.starts_with('-');
    if !ok {
        bail!(
            "invalid tag {tag:?} — expected `namespace:value` or a bare word \
             (letters, digits, `_`, `-`)"
        );
    }
    Ok(())
}

/// `oca links <id-or-title>` — outgoing references and backlinks. Links are
/// by id (§4.1); titles are resolved through the index as a convenience.
fn links(target: &str, project: Option<&Path>) -> Result<u8> {
    let mut core = story_core(project)?;
    // `.md` documents resolve by id or title; songs/takes/events only by id.
    let (id, title, rel) = match core.resolve(target) {
        Ok(rel) => {
            let document = core.read_document(&rel)?;
            let id = document
                .frontmatter
                .get_str("id")
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "{} has no `id:` in its frontmatter — add one (links are by id, \
                         never path)",
                        rel.display()
                    )
                })?;
            let title = document
                .frontmatter
                .get_str("title")
                .unwrap_or_else(|| target.to_string());
            (id, title, rel)
        }
        Err(_) => {
            let (rel, title, kind) = core.unit_by_ref(target).ok().flatten().ok_or_else(|| {
                anyhow::anyhow!(
                    "nothing resolves to {target:?} — pass a document id/title, or an id \
                     of a song/take/event (see `oca search`)"
                )
            })?;
            (target.to_string(), format!("{title} ({kind})"), rel)
        }
    };
    let report = core.link_report(&id)?;
    println!("{title} — {id}  [{}]", rel.display());
    if report.outgoing.is_empty() {
        println!("  no outgoing references");
    } else {
        for (kind, to) in &report.outgoing {
            println!(
                "  → {kind}: {} ({})  [{}]",
                to.title,
                if to.id.is_empty() { "?" } else { &to.id },
                to.rel.display()
            );
        }
    }
    if report.incoming.is_empty() {
        println!("  no backlinks");
    } else {
        for (kind, from) in &report.incoming {
            println!(
                "  ← {kind}: {} ({})  [{}]",
                from.title,
                if from.id.is_empty() { "?" } else { &from.id },
                from.rel.display()
            );
        }
    }
    Ok(0)
}

/// Registered file-format migrations `(to_version, description)`, run in
/// order by `oca migrate`. Empty until a format newer than 1 exists —
/// format 1 was the first shipped, so nothing older needs converting. The
/// *database* schema migrates itself whenever the cache DB opens.
const FILE_MIGRATIONS: &[(u64, &str)] = &[];

fn migrate(project: Option<&Path>) -> Result<u8> {
    let root = find_project(project)?;
    let path = root.join(oca_storage::layout::PROJECT_FILE);
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let meta = oca_model::ProjectMeta::parse(&raw)?;
    let found = meta.format_version();
    let supported = oca_model::SUPPORTED_FORMAT_VERSION;
    if found > supported {
        bail!(
            "project format_version {found} is newer than this build supports ({supported}) \
             — upgrade OCA to migrate it"
        );
    }
    let core = open_core(&root)?; // holds the single-writer lock while we patch
    if found == supported {
        println!("project format {found} is current — nothing to migrate");
    } else {
        for (version, what) in FILE_MIGRATIONS {
            if *version > found {
                println!("applying format {version}: {what}");
            }
        }
        // The one step from a pre-versioned layout (version reads as 0
        // when `format_version:` is absent): stamp it. Lossless — only that
        // key is touched.
        let mut yaml = oca_model::LosslessYaml::new(&raw);
        yaml.set_str("format_version", &supported.to_string())?;
        oca_storage::atomic::atomic_write(&path, yaml.raw().as_bytes())?;
        println!("migrated format {found} → {supported}");
    }
    if let Some(t) = core.index_totals() {
        println!(
            "cache index: {} documents, {} units — schema current",
            t.documents, t.units
        );
    }
    Ok(0)
}
