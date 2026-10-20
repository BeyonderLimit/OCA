//! Phase 1 command implementations — every mutation goes through
//! `oca_core::Core` (AGENTS rule 8; the CLI is the Core's first client).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use oca_core::{Command as CoreCommand, Core, Event};
use oca_model::{DocumentKind, ProjectKind};
use oca_music::CheckReport;
use oca_storage::registry::Registry;

use crate::cli::{
    AiCommand, AudioCommand, ChapterCommand, Command, ProposalCommand, RuntimeCommand,
    SnapshotCommand, SongCommand, StoryCommand, TagCommand,
};
use crate::config::Config;

pub fn dispatch(cmd: Command, home: Option<&Path>, cfg: &Config) -> Result<u8> {
    match cmd {
        Command::New { name, path, kind } => new_project(&name, path.as_deref(), &kind, home),
        Command::List => list_projects(home),
        Command::Open { project } => open_tui(project.as_deref(), cfg),
        Command::Chapter { cmd } => chapter(cmd),
        Command::Story { cmd } => story(cmd),
        Command::Ai { cmd } => ai(cmd, cfg),
        Command::Proposal { cmd } => proposal(cmd),
        Command::Song { cmd } => song(cmd),
        Command::Audio { cmd } => audio(cmd),
        Command::Reindex { full, project } => reindex(full, project.as_deref(), cfg),
        Command::Search {
            query,
            limit,
            project,
        } => search(&query.join(" "), limit, project.as_deref(), cfg),
        Command::Import { paths, project } => import(&paths, project.as_deref(), cfg),
        Command::Tag { cmd } => tag(cmd),
        Command::Tags { project } => tags(project.as_deref()),
        Command::Links { target, project } => links(&target, project.as_deref()),
        Command::Migrate { project } => migrate(project.as_deref()),
        Command::Snapshot { cmd } => snapshot(cmd),
        Command::Runtime { cmd } => runtime(cmd, home),
        Command::Dictate {
            take,
            into,
            clean,
            prose,
            language,
            project,
        } => dictate(
            &take,
            into.as_deref(),
            clean,
            prose,
            language.as_deref(),
            project.as_deref(),
            cfg,
        ),
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

fn open_tui(project: Option<&str>, cfg: &Config) -> Result<u8> {
    let root = resolve_open_arg(project)?;
    let mut core = open_core(&root)?;
    // AI is optional end-to-end (rule 3): a failed discovery just means no
    // AI panel data, never a failed launch. Dictation (Phase 7B) is
    // discovered the same way — it never requires the LLM side.
    let opts = cfg.bootstrap_options();
    let ai = match oca_ai::bootstrap(&opts) {
        Ok(ai) => ai,
        Err(err) => {
            tracing::warn!("AI runtime discovery failed: {err:#}");
            None
        }
    };
    // One residency policy for the session (ADR-0006): the panel's model
    // and dictation share the governor through this handle.
    let governor = ai
        .as_ref()
        .map(|ai| ai.governor())
        .unwrap_or_else(oca_ai::ModelManager::new);
    let dictation = match oca_ai::bootstrap_dictation(&opts, governor.clone()) {
        Ok(dictation) => dictation,
        Err(err) => {
            tracing::warn!("dictation discovery failed: {err:#}");
            None
        }
    };
    let transcriber = dictation.map(|dictation| {
        let label = dictation.info().label();
        Arc::new(oca_audio::FnTranscriber::new(
            label,
            move |wav: &Path, cancel: &std::sync::atomic::AtomicBool| {
                dictation.transcribe(wav, cancel)
            },
        )) as Arc<dyn oca_audio::Transcriber>
    });
    // Phase 11: embeddings ride the same residency policy (A6); when they
    // exist, prompt context can draw on the hybrid index through the
    // Retriever seam, and the search panel fuses vectors with keywords.
    let embedder = match oca_ai::bootstrap_embeddings(&opts, governor) {
        Ok(embedder) => embedder,
        Err(err) => {
            tracing::warn!("embedding discovery failed: {err:#}");
            None
        }
    };
    if let Some(ai) = ai.as_ref()
        && let Some(embedder) = embedder.as_ref()
    {
        ai.set_search(Arc::new(oca_rag::Rag::new(Some(embedder.clone()))));
    }
    oca_tui::run(&mut core, ai, transcriber, embedder)?;
    Ok(0)
}

/// Dictation (Phase 7B): whisper transcription of a recorded take. The raw
/// transcript is stored with the take first; `--into` is an explicit
/// second step, and the LLM cleanups are previews — AI output is never
/// written into project files (proposals arrive with Phase 8).
fn dictate(
    take_key: &str,
    into: Option<&str>,
    clean: bool,
    prose: bool,
    language: Option<&str>,
    project: Option<&Path>,
    cfg: &Config,
) -> Result<u8> {
    if clean && prose {
        bail!("pick one of --clean or --prose, not both");
    }
    if (clean || prose) && into.is_some() {
        bail!(
            "--into cannot be combined with --clean/--prose — review the cleanup first, \
             then send the text you actually want (AI output is only ever shown for review)"
        );
    }

    let root = find_project(project)?;
    let mut core = open_core(&root)?;

    let mut opts = cfg.bootstrap_options();
    if let Some(lang) = language {
        opts.whisper_language = Some(lang.to_string());
    }
    // One residency policy for the whole run (ADR-0006): transcription
    // takes the whisper slot, a cleanup run takes the LLM slot — never
    // both heavy models resident at once.
    let governor = oca_ai::ModelManager::new();
    let dictation = oca_ai::bootstrap_dictation(&opts, governor)
        .context("dictation runtime discovery failed")?
        .ok_or_else(|| anyhow::anyhow!("{}", oca_audio::dictation::missing_runtime_hint()))?;

    let label = dictation.info().label();
    let transcriber = Arc::new(oca_audio::FnTranscriber::new(
        label.clone(),
        move |wav: &Path, cancel: &std::sync::atomic::AtomicBool| dictation.transcribe(wav, cancel),
    )) as Arc<dyn oca_audio::Transcriber>;
    let host: Arc<dyn oca_audio::AudioHost> = Arc::new(oca_audio::CpalHost);
    let facade = oca_audio::Audio::with_transcriber(&core, host, Some(transcriber))?;
    let take = facade.take(take_key)?;

    eprintln!("transcribing with {label}…");
    let text = facade.transcribe(&take)?;
    println!("{text}");

    if clean || prose {
        let mode = if clean {
            oca_ai::Cleanup::Clean
        } else {
            oca_ai::Cleanup::Prose
        };
        let ai = oca_ai::bootstrap(&opts)?.ok_or_else(|| {
            anyhow::anyhow!(
                "cleanup needs a language model — none is installed \
                 (dictation itself works without one)"
            )
        })?;
        ai.load()?;
        let (tx, rx) = std::sync::mpsc::channel();
        let outcome = ai.stream(oca_ai::cleanup_request(mode, &text), tx);
        let preview: String = rx.iter().collect();
        let _ = ai.unload();
        outcome?;
        eprintln!();
        println!("{preview}");
        eprintln!(
            "preview only — review it, then insert the text you want \
             (`oca dictate --into …` sends the raw transcript; AI output \
             never writes to files itself)"
        );
        return Ok(0);
    }

    match into {
        None => {
            eprintln!("stored with the take {}", take.meta.take_id);
        }
        Some("notes") => {
            let path = oca_audio::dictation::add_to_notes(&mut core, &take, &text)?;
            eprintln!(
                "stored with the take {} + added to {}",
                take.meta.take_id,
                path.display()
            );
        }
        Some(target) => {
            let Some(chapter) = target.strip_prefix("chapter:") else {
                bail!("unknown --into target {target:?} — use `notes` or `chapter:<chapter>`");
            };
            let rel = oca_audio::dictation::insert_into_chapter(&mut core, chapter, &text)?;
            eprintln!(
                "stored with the take {} + inserted into {}",
                take.meta.take_id,
                rel.display()
            );
        }
    }
    Ok(0)
}

/// Runtime packs: install from a local file, list + verify.
fn runtime(cmd: RuntimeCommand, home: Option<&Path>) -> Result<u8> {
    let runtime_dir = |dir: Option<PathBuf>| {
        dir.unwrap_or_else(|| match home {
            Some(h) => h.join(".oca").join("runtime"),
            None => oca_ai::default_runtime_dir(),
        })
    };
    match cmd {
        RuntimeCommand::Install { from, dir } => {
            let dir = runtime_dir(dir);
            let outcome = oca_ai::install_pack(&from, &dir)?;
            println!(
                "installed {} {} — {} file(s) verified → {}",
                outcome.name,
                outcome.version,
                outcome.files,
                outcome.dir.display()
            );
            Ok(0)
        }
        RuntimeCommand::List { dir } => {
            let dir = runtime_dir(dir);
            let packs = oca_ai::list_packs(&dir)?;
            if packs.is_empty() {
                println!(
                    "no runtime packs in {} (side-load with `oca runtime install --from <pack.tar.gz>`)",
                    dir.display()
                );
                return Ok(0);
            }
            let mut any_problems = false;
            for pack in &packs {
                if pack.problems.is_empty() {
                    println!("✓ {} {} — {}", pack.name, pack.version, pack.dir.display());
                } else {
                    any_problems = true;
                    println!(
                        "⚠ {} {} — {}",
                        pack.name,
                        pack.version,
                        pack.problems.join("; ")
                    );
                }
            }
            Ok(u8::from(any_problems))
        }
    }
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

// ---- ai (Phase 9) -----------------------------------------------------------

fn ai_project(cmd: &AiCommand) -> Option<PathBuf> {
    match cmd {
        AiCommand::Continue { project, .. }
        | AiCommand::Rewrite { project, .. }
        | AiCommand::Summarize { project, .. }
        | AiCommand::Outline { project }
        | AiCommand::Review { project, .. }
        | AiCommand::SongConcept { project, .. }
        | AiCommand::SongStructure { project, .. }
        | AiCommand::Verse { project, .. }
        | AiCommand::Chorus { project, .. }
        | AiCommand::Bridge { project, .. }
        | AiCommand::RewriteLyrics { project, .. }
        | AiCommand::ImproveRhyme { project, .. }
        | AiCommand::ImproveMeter { project, .. }
        | AiCommand::PreserveHook { project, .. }
        | AiCommand::PreserveRhymeScheme { project, .. }
        | AiCommand::Alternatives { project, .. }
        | AiCommand::Analyze { project, .. }
        | AiCommand::Singability { project, .. } => project.clone(),
    }
}

/// Phases 9/10 story and music operations. Deterministic preparation
/// resolves the target range and prompt; the model only ever fills in
/// text; the result lands as a Core proposal (rule 4) that the author
/// accepts or rejects — reports (review, concept, analyze, singability,
/// alternatives) are printed and never written.
fn ai(cmd: AiCommand, cfg: &Config) -> Result<u8> {
    let root = find_project(ai_project(&cmd).as_deref())?;
    let mut core = open_core(&root)?;

    let opts = cfg.bootstrap_options();
    let facade = oca_ai::bootstrap(&opts)?.ok_or_else(|| {
        anyhow::anyhow!(
            "no language model is available — install one with `oca runtime install`, \
             or set `mock = true` in the [ai] config section to try the built-in \
             scripted model"
        )
    })?;
    facade.load()?;
    let budget = opts
        .context
        .map(|c| c as usize)
        .unwrap_or(oca_ai::DEFAULT_BUDGET);

    let prepared = match cmd {
        AiCommand::Continue {
            chapter,
            at,
            project: _,
        } => {
            // Default: the end of the body — continue the last scene.
            let at = match at {
                Some(at) => at,
                None => {
                    let rel = oca_story::Story::new(&core).chapter(&chapter)?.rel;
                    core.read_document(&rel)?.body().len()
                }
            };
            // Phase 11: draw on the hybrid index when embeddings are
            // installed. Loading is best-effort — the language model
            // already holds the residency slot (A6), so this degrades to
            // keywords rather than fighting for RAM.
            if let Ok(Some(embedder)) = oca_ai::bootstrap_embeddings(&opts, facade.governor()) {
                let rag = oca_rag::Rag::new(Some(embedder));
                if rag.load().is_ok() {
                    facade.set_search(Arc::new(rag));
                }
            }
            let wired = facade.search();
            oca_ai::prepare_continue_scene(&core, &chapter, at, budget, wired.as_deref())?
        }
        AiCommand::Rewrite {
            chapter,
            start,
            end,
            guidance,
            project: _,
        } => oca_ai::prepare_rewrite_selection(
            &core,
            &chapter,
            start,
            end,
            guidance.as_deref(),
            budget,
        )?,
        AiCommand::Summarize {
            chapter,
            project: _,
        } => oca_ai::prepare_summarize_chapter(&core, &chapter, budget)?,
        AiCommand::Outline { project: _ } => oca_ai::prepare_generate_outline(&core, budget)?,
        AiCommand::Review {
            chapter,
            project: _,
        } => oca_ai::prepare_review_chapter(&core, &chapter, budget)?,
        // ---- music operations (Phase 10): same pipeline, music targets ----
        AiCommand::SongConcept { song, project: _ } => {
            oca_ai::prepare_song_concept(&core, &song, budget)?
        }
        AiCommand::SongStructure { song, project: _ } => {
            oca_ai::prepare_song_structure(&core, &song, budget)?
        }
        AiCommand::Verse { song, project: _ } => {
            oca_ai::prepare_generate_section(&core, &song, "Verse", budget)?
        }
        AiCommand::Chorus { song, project: _ } => {
            oca_ai::prepare_generate_section(&core, &song, "Chorus", budget)?
        }
        AiCommand::Bridge { song, project: _ } => {
            oca_ai::prepare_generate_section(&core, &song, "Bridge", budget)?
        }
        AiCommand::RewriteLyrics {
            song,
            start,
            end,
            guidance,
            project: _,
        } => oca_ai::prepare_rewrite_lyrics(&core, &song, start, end, guidance.as_deref(), budget)?,
        AiCommand::ImproveRhyme {
            song,
            start,
            end,
            project: _,
        } => oca_ai::prepare_improve_rhyme(&core, &song, start, end, budget)?,
        AiCommand::ImproveMeter {
            song,
            start,
            end,
            project: _,
        } => oca_ai::prepare_improve_meter(&core, &song, start, end, budget)?,
        AiCommand::PreserveHook {
            song,
            start,
            end,
            project: _,
        } => oca_ai::prepare_preserve_hook(&core, &song, start, end, budget)?,
        AiCommand::PreserveRhymeScheme {
            song,
            start,
            end,
            project: _,
        } => oca_ai::prepare_preserve_rhyme_scheme(&core, &song, start, end, budget)?,
        AiCommand::Alternatives {
            song,
            start,
            end,
            project: _,
        } => oca_ai::prepare_alternatives(&core, &song, start, end, budget)?,
        AiCommand::Analyze { song, project: _ } => {
            oca_ai::prepare_analyze_song(&core, &song, budget)?
        }
        AiCommand::Singability { song, project: _ } => {
            oca_ai::prepare_check_singability(&core, &song, budget)?
        }
    };

    let draft = oca_ai::complete(&facade, prepared, &mut |_| {})?;
    let _ = facade.unload();

    match draft.target {
        // The review is a report the author reads; it is never written.
        oca_ai::DraftTarget::Report => {
            println!("{}", draft.payload);
            Ok(0)
        }
        _ => {
            let id = oca_ai::create_proposal(&mut core, draft)?;
            println!("proposal {id} recorded — accept with `oca proposal apply {id}`");
            Ok(0)
        }
    }
}

/// Open the project for a proposal decision (mutates Core state).
fn proposal_core(project: Option<&Path>) -> Result<Core> {
    let root = find_project(project)?;
    open_core(&root)
}

fn proposal(cmd: ProposalCommand) -> Result<u8> {
    match cmd {
        ProposalCommand::List { project } => {
            let core = proposal_core(project.as_deref())?;
            for p in core.proposals()? {
                println!(
                    "{}  {:<9} {:<18} {}",
                    p.id,
                    p.status.as_str(),
                    p.operation,
                    p.target.document.display()
                );
            }
            Ok(0)
        }
        ProposalCommand::Show { id, project } => {
            let core = proposal_core(project.as_deref())?;
            let p = core
                .proposal(&id)?
                .with_context(|| format!("no proposal {id}"))?;
            println!("operation:  {}", p.operation);
            println!("prompt:     {}", p.prompt_version);
            println!("model:      {}", p.model);
            println!("created:    {}", p.created);
            println!("status:     {}", p.status.as_str());
            println!(
                "target:     {} (bytes {}..{})",
                p.target.document.display(),
                p.target.start,
                p.target.end
            );
            println!("provenance: {} context entries", p.context_manifest.len());
            println!("\n{}", p.payload);
            Ok(0)
        }
        ProposalCommand::Apply { id, project } => {
            let mut core = proposal_core(project.as_deref())?;
            let events = core.execute(CoreCommand::ProposalApply { id })?;
            match &events[0] {
                Event::ProposalApplied { rebased: false, .. } => {
                    println!(
                        "applied — a snapshot was saved first \
                         (undo with `oca snapshot restore`)"
                    );
                }
                Event::ProposalApplied { rebased: true, .. } => {
                    println!(
                        "applied — the passage had moved, so the text was rebased onto it; \
                         a snapshot was saved first (undo with `oca snapshot restore`)"
                    );
                }
                Event::ProposalStale { .. } => {
                    println!(
                        "stale — the passage changed while the model was thinking; \
                         your text was left untouched"
                    );
                }
                other => bail!("unexpected outcome: {other:?}"),
            }
            Ok(0)
        }
        ProposalCommand::Reject { id, project } => {
            let mut core = proposal_core(project.as_deref())?;
            core.execute(CoreCommand::ProposalReject { id })?;
            println!("rejected — kept for audit, never applied");
            Ok(0)
        }
    }
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
fn reindex(full: bool, project: Option<&Path>, cfg: &Config) -> Result<u8> {
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
    // Phase 11: the vector index rebuilds from the same files, in the same
    // cache DB — incremental unless the content or the model changed.
    embed_sync(&core, cfg);
    Ok(0)
}

/// Embed + vector-sync the cache when an embedding model is installed
/// (Phase 11). No model = one honest line, never an error (rule 3).
fn embed_sync(core: &Core, cfg: &Config) {
    let opts = cfg.bootstrap_options();
    match oca_ai::bootstrap_embeddings(&opts, oca_ai::ModelManager::new()) {
        Ok(Some(embedder)) => match oca_rag::sync(core, embedder.as_ref()) {
            Ok(stats) => println!(
                "embeddings: {} embedded, {} unchanged, {} removed — {} chunk(s) indexed",
                stats.embedded, stats.unchanged, stats.removed, stats.chunks
            ),
            Err(err) => println!("embeddings: sync failed: {err:#}"),
        },
        Ok(None) => println!("embeddings: no embedding model installed — keyword index only"),
        Err(err) => println!("embeddings: discovery failed: {err:#}"),
    }
}

/// `oca import <file>...`: copy Markdown / text / EPUB into `library/`
/// with provenance frontmatter (A8), then index + embed. Snapshot first,
/// like every other bulk write — the pre-import state stays restorable.
fn import(paths: &[PathBuf], project: Option<&Path>, cfg: &Config) -> Result<u8> {
    if paths.is_empty() {
        bail!("nothing to import — usage: oca import <file>...");
    }
    let root = find_project(project)?;
    let mut core = open_core(&root)?;
    if core.is_read_only() {
        bail!(
            "project is read-only: {}",
            core.read_only_reason().unwrap_or_default()
        );
    }
    let events = core.execute(CoreCommand::SnapshotCreate {
        label: Some("before import".into()),
    })?;
    if let Some(Event::SnapshotCreated { n, .. }) = events.first() {
        println!("snapshot {n} created before import");
    }
    for path in paths {
        let imported = oca_rag::import_file(&root, path)?;
        println!(
            "imported {} → {} ({})",
            path.display(),
            imported.rel.display(),
            imported.source_type
        );
    }
    core.reindex()?;
    embed_sync(&core, cfg);
    Ok(0)
}

fn search(query: &str, limit: usize, project: Option<&Path>, cfg: &Config) -> Result<u8> {
    let query = query.trim();
    if query.is_empty() {
        bail!("nothing to search for — usage: oca search <words>");
    }
    let root = find_project(project)?;
    let core = open_core(&root)?;
    // Hybrid when an embedding model is installed (Phase 11); keywords
    // alone when it isn't — searching never requires a model (rule 3).
    let opts = cfg.bootstrap_options();
    let embedder = match oca_ai::bootstrap_embeddings(&opts, oca_ai::ModelManager::new()) {
        Ok(embedder) => embedder,
        Err(err) => {
            tracing::warn!("embedding discovery failed: {err:#}");
            None
        }
    };
    if let Some(embedder) = &embedder
        && let Err(err) = embedder.load()
    {
        // A resident model elsewhere is an explicit swap (A6); a failed
        // load degrades to keywords with one honest line.
        println!("note: embeddings unavailable ({err:#}); showing keyword results");
    }
    let ready = embedder.as_deref().filter(|e| e.is_loaded());
    let hits = oca_rag::hybrid_search(&core, ready, query, limit)?;
    if hits.is_empty() {
        println!("no matches for \"{query}\"");
        return Ok(0);
    }
    for hit in &hits {
        println!(
            "{:<9} {:<26} {}{}",
            hit.kind,
            ellipsize(&hit.title, 26),
            hit.rel.display(),
            if hit.untrusted { "  [reference]" } else { "" }
        );
        if !hit.snippet.is_empty() {
            println!("          {}", hit.snippet.replace('\n', " "));
        }
    }
    println!(
        "{} match(es){}",
        hits.len(),
        if hits.iter().any(|h| h.semantic.is_some()) {
            " · hybrid"
        } else {
            ""
        }
    );
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

// ---- snapshots (Phase 6) ---------------------------------------------------

fn snapshot(cmd: SnapshotCommand) -> Result<u8> {
    match cmd {
        SnapshotCommand::Create { label, project } => {
            let mut core = open_core(&find_project(project.as_deref())?)?;
            let label = label.filter(|l| !l.trim().is_empty());
            let events = core.execute(CoreCommand::SnapshotCreate { label })?;
            let Some(Event::SnapshotCreated { n, label, files }) = events.first() else {
                bail!("snapshot did not report back");
            };
            println!("Snapshot #{n} — {label} · {files} file(s)");
            Ok(0)
        }
        SnapshotCommand::List { project } => {
            let core = open_core(&find_project(project.as_deref())?)?;
            let list = core.snapshots()?;
            if list.is_empty() {
                println!("no snapshots yet — save one with `oca snapshot create`");
                return Ok(0);
            }
            println!("#     WHEN (UTC)          FILES  LABEL");
            for s in &list {
                println!(
                    "{:<5} {:<17} {:>7}  {}",
                    s.n,
                    oca_core::format_time(s.when),
                    s.files,
                    s.display_label()
                );
            }
            Ok(0)
        }
        SnapshotCommand::Diff { n, m, project } => {
            let core = open_core(&find_project(project.as_deref())?)?;
            let report = core.snapshot_diff(n, m)?;
            if report.files.is_empty() {
                println!("{} — {}", report.summary, describe_against(&report));
                return Ok(0);
            }
            for f in &report.files {
                if f.binary {
                    // Images diff as a one-liner; audio says the full story.
                    println!("{}: {} (binary)", kind_word(f.kind), f.path.display());
                } else {
                    println!("{}", f.patch);
                }
            }
            println!("{} — {}", report.summary, describe_against(&report));
            Ok(0)
        }
        SnapshotCommand::Restore { n, project } => {
            let mut core = open_core(&find_project(project.as_deref())?)?;
            let events = core.execute(CoreCommand::SnapshotRestore { n })?;
            let Some(Event::SnapshotRestored {
                n,
                restored,
                removed,
                unchanged,
                audio_differing,
            }) = events.first()
            else {
                bail!("restore did not report back");
            };
            println!(
                "restored Snapshot #{n} — {restored} file(s) restored, {removed} removed, {unchanged} unchanged"
            );
            for rel in audio_differing {
                println!(
                    "  audio kept as recorded: {} (takes are never overwritten or removed)",
                    rel.display()
                );
            }
            Ok(0)
        }
    }
}

fn describe_against(report: &oca_core::DiffReport) -> String {
    match report.against {
        Some(m) => format!("Snapshot #{} vs Snapshot #{}", report.base, m),
        None => format!("Snapshot #{} vs your files now", report.base),
    }
}

fn kind_word(kind: oca_core::ChangeKind) -> &'static str {
    match kind {
        oca_core::ChangeKind::Added => "added",
        oca_core::ChangeKind::Removed => "removed",
        oca_core::ChangeKind::Modified => "changed",
    }
}
