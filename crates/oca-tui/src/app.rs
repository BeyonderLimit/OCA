//! TUI application state: project tree + rope editor + status bar.
//! Reads through Core queries, writes through Core commands — nothing else.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use oca_ai::Ai;
use oca_audio::{
    Audio, AudioHost, Player, RecState, Recorder, Take, Transcriber, default_take_dir,
};
use oca_core::{Command, Core, Event, JobState, Priority, RecoveryEntry};
use oca_model::DocumentKind;
use oca_music::{CursorMetrics, EnglishPhonetics, Music, cursor_metrics, section_marks};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::editor::{Editor, KeyOutcome};

/// Unsaved buffers are journaled this often (A3 recovery).
const JOURNAL_INTERVAL: Duration = Duration::from_secs(2);

const GROUP_STYLE: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
const SELECTED_STYLE: Style = Style::new()
    .fg(Color::Black)
    .bg(Color::Cyan)
    .add_modifier(Modifier::BOLD);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Tree,
    Editor,
    /// The "set state at this chapter" form is capturing input.
    Form,
    /// The audio panel (take list + record controls) is open.
    Audio,
    /// The Ctrl-F search panel is capturing the query.
    Search,
    /// The snapshot panel (restore points + inline diff) is open.
    Snapshots,
    /// The AI panel (runtime status, Load/Unload, streaming probe) is open.
    Ai,
}

enum TreeItem {
    Group(&'static str),
    Doc { rel: PathBuf, title: String },
}

/// Fields the "set state at this chapter" form collects (A11 authoring aid).
const STATE_FIELDS: [&str; 2] = ["location", "emotional"];

/// The form's state: which character, which chapter, and what's typed so far.
/// All validation and writing happens in `oca_story::set_state` (rule 8).
struct StateForm {
    char_rel: PathBuf,
    char_name: String,
    chapter_rel: PathBuf,
    chapter_title: String,
    buffers: [String; STATE_FIELDS.len()],
    idx: usize,
}

impl StateForm {
    fn next_field(&mut self) {
        self.idx = (self.idx + 1) % STATE_FIELDS.len();
    }

    fn prev_field(&mut self) {
        self.idx = (self.idx + STATE_FIELDS.len() - 1) % STATE_FIELDS.len();
    }
}

/// What a take recorded from the current context should link to, and where
/// its file lands. Derived from the open document; the engine resolves and
/// validates the actual target (rule 8).
enum AttachContext {
    /// Open chapter — key is its project-relative path.
    Chapter(String),
    /// Open song — key is its slug.
    Song(String),
    /// No relevant document open: project-level voice note.
    Project,
}

impl AttachContext {
    fn describe(&self) -> String {
        match self {
            AttachContext::Chapter(rel) => format!("chapter {rel}"),
            AttachContext::Song(slug) => format!("song {slug}"),
            AttachContext::Project => "project".into(),
        }
    }

    /// The spec `Audio::resolve_target` understands.
    fn target_spec(&self) -> String {
        match self {
            AttachContext::Chapter(rel) => format!("chapter:{rel}"),
            AttachContext::Song(slug) => format!("song:{slug}"),
            AttachContext::Project => "project".into(),
        }
    }
}

/// Open state of the audio panel: the take list, the selection, and any
/// live recorder/player. All behavior lives in `oca-audio` (rule 8).
struct AudioPanel {
    /// Directory new takes are saved into (context-dependent).
    dir_rel: PathBuf,
    ctx: AttachContext,
    takes: Vec<Take>,
    sel: usize,
    rec: Option<Recorder>,
    player: Option<Player>,
    /// The active transcript: freshly transcribed, or the stored one for
    /// the selected take. Never auto-inserted anywhere — `i`/`n` are the
    /// author's explicit say-so.
    transcript: Option<String>,
    /// Streaming preview of an LLM cleanup of the transcript. Shown for
    /// review only; applying it is the author pressing `i`/`n`.
    preview: Option<String>,
    /// The dictation job (transcription or cleanup preview) while running.
    dict_job: Option<(u64, DictJobKind)>,
    /// Results streamed back by the running dictation job.
    dict_rx: Option<std::sync::mpsc::Receiver<String>>,
}

/// What the running dictation job is doing — both stream into `dict_rx`,
/// but a transcription lands as one message and a cleanup as tokens.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DictJobKind {
    Transcribe,
    Cleanup(&'static str),
}

impl DictJobKind {
    fn label(self) -> &'static str {
        match self {
            DictJobKind::Transcribe => "transcription",
            DictJobKind::Cleanup(label) => label,
        }
    }
}

enum Banner {
    /// A journaled draft differs from the file on disk.
    Recovery { entry: RecoveryEntry },
}

/// The Ctrl-F search panel: the query typed so far, live hits, and the
/// selection. All querying goes through `Core::search` — the index and its
/// FTS syntax live in oca-storage (rule 8).
struct SearchPanel {
    query: String,
    hits: Vec<oca_core::Hit>,
    sel: usize,
}

/// The snapshot panel (Phase 6): the restore-point list, plus the selected
/// snapshot's inline diff against the working files. All logic lives in
/// `oca-versioning` behind Core (rule 8).
struct SnapshotPanel {
    snapshots: Vec<oca_core::SnapshotInfo>,
    sel: usize,
    mode: SnapMode,
}

enum SnapMode {
    List,
    /// Scrollable unified diff of the selected snapshot vs. the files now.
    Diff {
        lines: Vec<String>,
        scroll: u16,
        title: String,
    },
}

/// The AI panel (Phase 7): model info, Load/Unload, and a streaming probe
/// with `[ESC] Stop`. All runtime logic lives in `oca-ai` + Core's jobs
/// (rule 8) — this is display and key handling only.
struct AiPanel {
    state: AiState,
    /// Tokens streamed so far by the probe (also drives the progress bar).
    stream_tokens: usize,
    /// The assembled probe output.
    stream_text: String,
    /// The probe job, while one runs.
    job: Option<u64>,
    /// Token channel fed by the probe job thread.
    rx: Option<std::sync::mpsc::Receiver<String>>,
    /// Last load/probe failure, shown in red until the next action.
    error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AiState {
    /// Runtime present, model not resident.
    Idle,
    /// Load job running.
    Loading,
    /// Model resident — probes can start.
    Ready,
    /// Probe streaming right now.
    Streaming,
}

/// Present while a song's `lyrics.md` is open: the language its lyric tools
/// run under plus the loaded phonetics. All analysis lives in `oca-music`
/// (rule 8) — this only caches what the status bar and section jumps need.
struct SongCtx {
    language: String,
    phon: EnglishPhonetics,
}

pub struct App {
    editor: Editor,
    items: Vec<TreeItem>,
    sel: usize,
    focus: Focus,
    open_rel: Option<PathBuf>,
    doc_title: String,
    words: usize,
    dirty: bool,
    status: String,
    banner: Option<Banner>,
    form: Option<StateForm>,
    /// Set while the open doc is a song's lyrics file.
    song: Option<SongCtx>,
    /// Live status-bar metrics for the cursor position (lyrics only).
    song_metrics: Option<CursorMetrics>,
    /// Device access for the audio panel (tests inject a mock host).
    audio_host: Arc<dyn AudioHost>,
    /// Present while the audio panel is open.
    audio_panel: Option<AudioPanel>,
    /// Present while the search panel is open.
    search_panel: Option<SearchPanel>,
    /// Present while the snapshot panel is open.
    snap_panel: Option<SnapshotPanel>,
    /// The AI runtime facade (None = no runtime — AI is optional, rule 3).
    ai: Option<Ai>,
    /// Present while the AI panel is open.
    ai_panel: Option<AiPanel>,
    /// Dictation (Phase 7B): the injected whisper seam — `None` means no
    /// transcription runtime, which only ever disables `t`/`c`/`p` with an
    /// explanation (rule 3: never required).
    transcriber: Option<Arc<dyn Transcriber>>,
    /// Last pass that reconciled the cache index with external edits.
    last_reconcile: Instant,
    last_journal: Instant,
    quit: bool,
    /// Last drawn inner editor rect (mouse mapping); sane default pre-draw.
    editor_area: Rect,
    tree_area: Rect,
}

impl App {
    pub fn new(core: &Core, audio_host: Arc<dyn AudioHost>) -> Self {
        let items = build_items(core);
        let sel = next_selectable(&items, 0, 1).unwrap_or(0);
        let status = if core.is_read_only() {
            core.read_only_reason()
                .unwrap_or_else(|| "read-only".into())
        } else {
            "ready — Enter opens the selected document".into()
        };
        Self {
            editor: Editor::new(),
            items,
            sel,
            focus: Focus::Tree,
            open_rel: None,
            doc_title: String::new(),
            words: 0,
            dirty: false,
            status,
            banner: None,
            form: None,
            song: None,
            song_metrics: None,
            audio_host,
            audio_panel: None,
            search_panel: None,
            snap_panel: None,
            ai: None,
            ai_panel: None,
            transcriber: None,
            last_reconcile: Instant::now(),
            last_journal: Instant::now(),
            quit: false,
            editor_area: Rect::new(0, 0, 72, 24),
            tree_area: Rect::new(0, 0, 30, 24),
        }
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    fn editor_dims(&self) -> (usize, usize) {
        (
            self.editor_area.width.max(1) as usize,
            self.editor_area.height.max(1) as usize,
        )
    }

    fn rebuild_items(&mut self, core: &Core) {
        let selected_rel = self.open_rel.clone();
        self.items = build_items(core);
        // Keep the cursor near where it was (first doc if current vanished).
        self.sel = next_selectable(&self.items, 0, 1).unwrap_or(0);
        if let Some(rel) = selected_rel
            && let Some(i) = self
                .items
                .iter()
                .position(|it| matches!(it, TreeItem::Doc { rel: r, .. } if *r == rel))
        {
            self.sel = i;
        }
    }

    // ---- document lifecycle ---------------------------------------------

    fn open_doc(&mut self, core: &mut Core, rel: PathBuf) {
        // Re-selecting the open document must not reload over unsaved edits.
        if self.open_rel.as_ref() == Some(&rel) {
            self.focus = Focus::Editor;
            return;
        }
        // Never lose unsaved work when switching documents: journal it first.
        if self.dirty {
            self.journal_now(core, true);
        }
        match core.read_document(&rel) {
            Ok(doc) => {
                let body = doc.body().to_string();
                self.words = body.split_whitespace().count();
                self.doc_title = doc
                    .frontmatter
                    .get_str("title")
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| {
                        rel.file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default()
                    });
                self.editor.load(&body);
                self.open_rel = Some(rel.clone());
                self.dirty = false;
                self.focus = Focus::Editor;
                self.song = self.load_song_ctx(core, &rel);
                self.song_metrics = None;
                self.refresh_song_metrics();
                self.status = format!("opened {} — {} words", rel.display(), self.words);
                // Offer a journaled draft that differs from the file.
                self.banner = None;
                match core.recovery_for(&rel) {
                    Ok(Some(entry)) if entry.content != body => {
                        self.banner = Some(Banner::Recovery { entry });
                    }
                    _ => {}
                }
            }
            Err(e) => self.status = format!("cannot open {}: {e}", rel.display()),
        }
    }

    /// Load the lyric-tool context for a song's `lyrics.md` (`None` for
    /// other documents). All parsing lives in `oca-music` (rule 8).
    fn load_song_ctx(&mut self, core: &Core, rel: &Path) -> Option<SongCtx> {
        let music = Music::new(core);
        let language = music.lyrics_language(rel).ok()??;
        match music.phonetics() {
            Ok(phon) => Some(SongCtx { language, phon }),
            // A broken dictionary.yaml must not break the editor: say so
            // and fall back to the embedded dictionary.
            Err(e) => {
                self.status = format!("dictionary.yaml ignored: {e:#}");
                Some(SongCtx {
                    language,
                    phon: EnglishPhonetics::new(),
                })
            }
        }
    }

    /// Recompute the status-bar metrics for the cursor position (lyrics
    /// files only). The buffer is small (a song), so per-move is cheap.
    fn refresh_song_metrics(&mut self) {
        self.song_metrics = self.song.as_ref().map(|ctx| {
            let text = self.editor.text();
            cursor_metrics(&text, self.editor.cursor(), &ctx.phon, &ctx.language)
        });
    }

    /// `Alt-]` / `Alt-[`: jump to the next/previous section heading. Brackets
    /// alone stay typeable — chords are written as `[Dm]`.
    fn jump_section(&mut self, forward: bool) {
        let text = self.editor.text();
        let marks = section_marks(&text);
        let cur = self.editor.cursor();
        let target = if forward {
            marks.iter().find(|m| m.char_offset > cur)
        } else {
            marks.iter().rev().find(|m| m.char_offset < cur)
        };
        match target {
            Some(m) => {
                self.editor.goto_char(m.char_offset);
                self.status = format!("section: {}", m.label);
                self.refresh_song_metrics();
            }
            None => {
                self.status = if forward {
                    "already in the last section".into()
                } else {
                    "already in the first section".into()
                };
            }
        }
    }

    fn save(&mut self, core: &mut Core) {
        let Some(rel) = self.open_rel.clone() else {
            self.status = "nothing to save — open a document first".into();
            return;
        };
        let body = self.editor.text();
        match core.save_body(&rel, &body) {
            Ok(_) => {
                self.dirty = false;
                self.status = format!("saved {}", rel.display());
                self.last_journal = Instant::now();
            }
            Err(e) => self.status = format!("save failed: {e}"),
        }
    }

    fn new_chapter(&mut self, core: &mut Core) {
        let created = core.execute(Command::CreateDocument {
            kind: DocumentKind::Chapter,
            title: "New Chapter".into(),
        });
        match created {
            Ok(events) => {
                let rel = events.iter().find_map(|e| match e {
                    oca_core::Event::DocumentCreated { rel, .. } => Some(rel.clone()),
                    _ => None,
                });
                if let Some(rel) = rel {
                    self.rebuild_items(core);
                    let created_rel = rel.clone();
                    self.open_doc(core, rel);
                    self.status = format!("created {}", created_rel.display());
                }
            }
            Err(e) => self.status = format!("cannot create chapter: {e}"),
        }
    }

    /// Journal the current buffer now (used on quit + interval ticks).
    fn journal_now(&mut self, core: &mut Core, force: bool) {
        if !self.dirty || core.is_read_only() {
            return;
        }
        if !force && self.last_journal.elapsed() < JOURNAL_INTERVAL {
            return;
        }
        let Some(rel) = self.open_rel.clone() else {
            return;
        };
        let content = self.editor.text();
        if core
            .execute(Command::WriteRecovery { doc: rel, content })
            .is_ok()
        {
            self.last_journal = Instant::now();
        }
    }

    // ---- "set state at this chapter" form (A11 authoring aid) --------------

    /// Open the form on the selected character, targeting the currently open
    /// chapter: state is keyed by chapter, so both are required.
    fn open_state_form(&mut self) {
        let Some(TreeItem::Doc { rel, title }) = self.items.get(self.sel) else {
            self.status = "select a character first".into();
            return;
        };
        let (char_rel, char_name) = (rel.clone(), title.clone());
        if group_of(&char_rel) != "characters" {
            self.status = "select a character to set state for".into();
            return;
        }
        let Some(chapter_rel) = self.open_rel.clone() else {
            self.status = "open a chapter first — state is set at a chapter".into();
            return;
        };
        if group_of(&chapter_rel) != "manuscript" {
            self.status = "open a chapter first — state is set at a chapter".into();
            return;
        }
        self.form = Some(StateForm {
            char_rel,
            char_name,
            chapter_title: self.doc_title.clone(),
            chapter_rel,
            buffers: [String::new(), String::new()],
            idx: 0,
        });
        self.focus = Focus::Form;
        self.status = "set state — Tab field · Enter applies · Esc cancels".into();
    }

    fn form_key(&mut self, core: &mut Core, key: KeyEvent) {
        let Some(mut form) = self.form.take() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.focus = Focus::Tree;
                self.status = "state not changed".into();
                return;
            }
            KeyCode::Tab | KeyCode::Down => form.next_field(),
            KeyCode::Up => form.prev_field(),
            KeyCode::Backspace => {
                form.buffers[form.idx].pop();
            }
            KeyCode::Char(c) => form.buffers[form.idx].push(c),
            KeyCode::Enter => {
                self.submit_state(core, form);
                return;
            }
            _ => {}
        }
        self.form = Some(form);
    }

    /// Apply the form through the story engine — every check and byte of
    /// writing lives there; the TUI only collects and displays (rule 8).
    fn submit_state(&mut self, core: &mut Core, form: StateForm) {
        let fields: Vec<(&str, &str)> = STATE_FIELDS
            .iter()
            .zip(form.buffers.iter())
            .filter(|(_, v)| !v.trim().is_empty())
            .map(|(k, v)| (*k, v.as_str()))
            .collect();
        if fields.is_empty() {
            self.status = "nothing to set — fill at least one field".into();
            self.form = Some(form);
            return;
        }
        let (char_key, chapter_key) = (
            form.char_rel.to_string_lossy().to_string(),
            form.chapter_rel.to_string_lossy().to_string(),
        );
        match oca_story::set_state(core, &char_key, &chapter_key, &fields) {
            Ok(_) => {
                self.status = format!("state set for {} at {}", form.char_name, form.chapter_title);
                self.focus = Focus::Tree;
            }
            Err(e) => {
                self.status = format!("cannot set state: {e}");
                self.form = Some(form);
            }
        }
    }

    /// Attach the AI runtime built by the frontend (bootstrap lives in the
    /// CLI; `None` means no runtime or no model — the panel still opens and
    /// explains how to enable AI).
    pub fn attach_ai(&mut self, ai: Ai) {
        self.ai = Some(ai);
    }

    /// Attach the dictation runtime built by the frontend (Phase 7B).
    /// Without it, the audio panel still opens and `t` explains what's
    /// missing — dictation is optional like everything AI-adjacent (rule 3).
    pub fn attach_transcriber(&mut self, transcriber: Arc<dyn Transcriber>) {
        self.transcriber = Some(transcriber);
    }

    // ---- AI panel (Phase 7) ----------------------------------------------

    fn open_ai_panel(&mut self) {
        let state = if self.ai.as_ref().is_some_and(|ai| ai.is_loaded()) {
            AiState::Ready
        } else {
            AiState::Idle
        };
        self.ai_panel = Some(AiPanel {
            state,
            stream_tokens: 0,
            stream_text: String::new(),
            job: None,
            rx: None,
            error: None,
        });
        self.focus = Focus::Ai;
        self.status = if self.ai.is_some() {
            "AI panel — l load, u unload, t probe, Esc close".into()
        } else {
            "AI panel — no runtime installed (AI is optional)".into()
        };
    }

    fn ai_key(&mut self, core: &mut Core, key: KeyEvent) {
        let Some(mut panel) = self.ai_panel.take() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                let was_streaming = panel.state == AiState::Streaming;
                if was_streaming {
                    // Interrupt the stream (socket abort) and mark the job.
                    if let Some(ai) = &self.ai {
                        ai.cancel();
                    }
                    if let Some(job) = panel.job.take() {
                        core.cancel_job(job);
                    }
                    panel.state = AiState::Ready;
                }
                self.focus = Focus::Tree;
                self.status = if was_streaming {
                    "generation stopped — the model stays loaded".into()
                } else {
                    "AI panel closed".into()
                };
                return;
            }
            KeyCode::Char('l') | KeyCode::Char('L')
                if matches!(panel.state, AiState::Idle | AiState::Ready) =>
            {
                match &self.ai {
                    Some(ai) => {
                        let ai = ai.clone();
                        panel.state = AiState::Loading;
                        panel.error = None;
                        panel.job = Some(core.spawn_job(
                            "load model",
                            Priority::Interactive,
                            move |_ctx| ai.load(),
                        ));
                        self.status = "loading model…".into();
                    }
                    None => {
                        panel.error = Some("no AI runtime installed — see the hint below".into());
                    }
                }
            }
            KeyCode::Char('u') | KeyCode::Char('U')
                if matches!(panel.state, AiState::Idle | AiState::Ready) =>
            {
                if let Some(ai) = &self.ai {
                    match ai.unload() {
                        Ok(()) => {
                            panel.state = AiState::Idle;
                            panel.error = None;
                            self.status = "model unloaded — memory released".into();
                        }
                        Err(err) => panel.error = Some(format!("{err:#}")),
                    }
                }
            }
            KeyCode::Char('t') | KeyCode::Char('T') if panel.state == AiState::Ready => {
                if let Some(ai) = &self.ai {
                    let (tx, rx) = std::sync::mpsc::channel();
                    let ai = ai.clone();
                    let request = oca_ai::probe_request();
                    let job = core.spawn_job("generate (probe)", Priority::Interactive, {
                        move |_ctx| ai.stream(request, tx).map(|_| ())
                    });
                    panel.state = AiState::Streaming;
                    panel.stream_tokens = 0;
                    panel.stream_text.clear();
                    panel.rx = Some(rx);
                    panel.job = Some(job);
                    panel.error = None;
                    self.status = "generating — Esc stops".into();
                }
            }
            _ => {}
        }
        self.ai_panel = Some(panel);
    }

    /// Drain probe tokens and retire finished AI jobs (runs every tick).
    fn poll_ai(&mut self, core: &mut Core) {
        let Some(panel) = &mut self.ai_panel else {
            return;
        };
        if let Some(rx) = &panel.rx {
            for token in rx.try_iter() {
                panel.stream_text.push_str(&token);
                panel.stream_tokens += 1;
            }
        }
        let Some(job) = panel.job else {
            return;
        };
        let Some(snapshot) = core.job(job) else {
            panel.job = None;
            return;
        };
        if snapshot.state.is_running() {
            return;
        }
        panel.job = None;
        panel.rx = None;
        match snapshot.state {
            JobState::Failed(msg) => {
                panel.error = Some(msg);
                // A failed probe leaves the model loaded; a failed load
                // leaves it unloaded.
                panel.state = if matches!(panel.state, AiState::Loading) {
                    AiState::Idle
                } else {
                    AiState::Ready
                };
            }
            JobState::Cancelled | JobState::Done | JobState::Running => {
                panel.state = AiState::Ready;
            }
        }
    }

    pub fn tick(&mut self, core: &mut Core) -> Result<()> {
        self.journal_now(core, false);
        self.poll_ai(core);
        self.poll_dictation(core);
        // Reconcile the cache index with edits made outside this session
        // (the polling watcher — mtime walk, only changed files reparse).
        // Skip while read-only: the session holding the lock maintains it.
        const RECONCILE_EVERY: Duration = Duration::from_secs(5);
        if !core.is_read_only() && self.last_reconcile.elapsed() >= RECONCILE_EVERY {
            self.last_reconcile = Instant::now();
            let _ = core.reconcile();
        }
        // Retire finished playback so the panel stops saying "playing".
        if let Some(panel) = &mut self.audio_panel
            && let Some(player) = &panel.player
            && player.is_done()
        {
            panel.player = None;
        }
        Ok(())
    }

    // ---- audio panel (Phase 4) --------------------------------------------

    /// Where a new take lands and what it should attach to, given the open
    /// document: a song's lyrics file → that song's `demos/`; a chapter or
    /// nothing → the project `audio/` directory.
    fn audio_context(open_rel: Option<&Path>) -> (PathBuf, AttachContext) {
        match open_rel {
            Some(rel) if group_of(rel) == "manuscript" => (
                default_take_dir(),
                AttachContext::Chapter(rel.to_string_lossy().into_owned()),
            ),
            Some(rel) if group_of(rel) == "music" => {
                let comps: Vec<Component> = rel.components().collect();
                if comps.len() == 4 {
                    // music/songs/<slug>/lyrics.md
                    let slug = comps[2].as_os_str().to_string_lossy().into_owned();
                    let dir = PathBuf::from("music/songs").join(&slug).join("demos");
                    (dir, AttachContext::Song(slug))
                } else {
                    (default_take_dir(), AttachContext::Project)
                }
            }
            _ => (default_take_dir(), AttachContext::Project),
        }
    }

    fn open_audio_panel(&mut self, core: &Core) {
        let (dir_rel, ctx) = Self::audio_context(self.open_rel.as_deref());
        let mut panel = AudioPanel {
            dir_rel,
            ctx,
            takes: Vec::new(),
            sel: 0,
            rec: None,
            player: None,
            transcript: None,
            preview: None,
            dict_job: None,
            dict_rx: None,
        };
        let host = Arc::clone(&self.audio_host);
        if let Ok(audio) = Audio::new(core, host) {
            match audio.takes() {
                Ok(takes) => panel.takes = takes,
                Err(e) => {
                    self.status = format!("cannot list takes: {e}");
                    self.audio_panel = Some(panel);
                    self.focus = Focus::Audio;
                    return;
                }
            }
        }
        // A stored transcript shows up right away (files are canonical —
        // reopening the panel never loses one).
        panel.transcript = panel
            .takes
            .first()
            .and_then(|t| t.meta.transcript.as_ref())
            .map(|t| t.text.clone());
        self.status = if self.transcriber.is_some() {
            format!(
                "audio — new takes go to {} · t transcribes the selected take",
                panel.dir_rel.display()
            )
        } else {
            format!(
                "audio — new takes go to {} · dictation: no runtime installed",
                panel.dir_rel.display()
            )
        };
        self.audio_panel = Some(panel);
        self.focus = Focus::Audio;
    }

    /// Cancel a running dictation job (Esc, or before the selection moves
    /// under it) and drop whatever it hasn't delivered yet. A cleanup
    /// stream also aborts at the model, exactly like the AI panel's stop.
    fn stop_dict_job(&mut self, core: &mut Core, panel: &mut AudioPanel) -> bool {
        let Some((job, kind)) = panel.dict_job.take() else {
            return false;
        };
        core.cancel_job(job);
        if matches!(kind, DictJobKind::Cleanup(_))
            && let Some(ai) = &self.ai
        {
            ai.cancel();
        }
        panel.dict_rx = None;
        true
    }

    /// Drain dictation results and retire the finished job (every tick),
    /// mirroring [`poll_ai`](Self::poll_ai). A message can only be read
    /// after taking the receiver out of the panel it borrows from.
    fn poll_dictation(&mut self, core: &mut Core) {
        let mut status: Option<String> = None;
        let mut refresh = false;
        let outcome: Option<(JobState, DictJobKind)> = {
            let Some(panel) = &mut self.audio_panel else {
                return;
            };
            let Some((id, kind)) = panel.dict_job else {
                return;
            };
            let Some(snapshot) = core.job(id) else {
                panel.dict_job = None;
                panel.dict_rx = None;
                return;
            };
            let msgs: Vec<String> = panel
                .dict_rx
                .as_ref()
                .map(|rx| rx.try_iter().collect())
                .unwrap_or_default();
            for msg in msgs {
                match kind {
                    DictJobKind::Transcribe => panel.transcript = Some(msg),
                    DictJobKind::Cleanup(_) => {
                        panel.preview.get_or_insert_with(String::new).push_str(&msg)
                    }
                }
            }
            if snapshot.state.is_running() {
                return;
            }
            // Everything the job sent arrived before it flipped to Done
            // (same thread, send-then-return), so nothing is left behind.
            panel.dict_job = None;
            panel.dict_rx = None;
            Some((snapshot.state, kind))
        };
        match outcome {
            Some((JobState::Done, DictJobKind::Transcribe)) => refresh = true,
            Some((JobState::Done, DictJobKind::Cleanup(label))) => {
                status = Some(format!("{label} ready — i insert · n to notes · Esc close"));
            }
            Some((JobState::Failed(msg), kind)) => {
                status = Some(format!("{} failed: {msg}", kind.label()));
            }
            Some((JobState::Cancelled, _)) => status = Some("stopped".into()),
            _ => {}
        }
        if refresh {
            // Re-read the store so the take list and the shown transcript
            // match the sidecar that was just written.
            if let Some(panel) = &mut self.audio_panel
                && let Ok(audio) = Audio::new(core, Arc::clone(&self.audio_host))
                && let Ok(takes) = audio.takes()
            {
                panel.takes = takes;
                panel.sel = panel.sel.min(panel.takes.len().saturating_sub(1));
                if let Some(take) = panel.takes.get(panel.sel) {
                    panel.transcript = take.meta.transcript.as_ref().map(|t| t.text.clone());
                }
            }
            status = status.or(Some(
                "transcript ready — i insert · n to notes · c clean".into(),
            ));
        }
        if let Some(s) = status {
            self.status = s;
        }
    }

    /// The text `i` (insert) and `n` (notes) act on: a preview being
    /// reviewed first, else the raw transcript.
    fn active_dict_text(panel: &AudioPanel) -> Option<String> {
        panel
            .preview
            .as_ref()
            .or(panel.transcript.as_ref())
            .map(|t| t.to_string())
    }

    fn audio_key(&mut self, core: &mut Core, key: KeyEvent) {
        let Some(mut panel) = self.audio_panel.take() else {
            return;
        };
        let host = Arc::clone(&self.audio_host);
        let transcriber = self.transcriber.clone();
        let audio = match Audio::with_transcriber(core, host, transcriber) {
            Ok(audio) => audio,
            Err(e) => {
                self.status = format!("audio: {e}");
                self.audio_panel = Some(panel);
                return;
            }
        };
        match key.code {
            KeyCode::Esc => {
                if self.stop_dict_job(core, &mut panel) {
                    // First Esc stops the run; the panel stays open with
                    // whatever text was already reviewed.
                    self.audio_panel = Some(panel);
                    self.status = "stopped — Esc again closes".into();
                    return;
                }
                if panel.rec.is_some() {
                    // Never lose a recording to a closed panel: save it,
                    // then drop the panel (it was taken out above).
                    let _saved = self.stop_and_save(core, panel);
                    self.focus = Focus::Tree;
                    self.status.push_str(" — panel closed");
                    return;
                }
                // Panel was taken out above; not storing it back closes it.
                self.focus = Focus::Tree;
                self.status = "audio closed".into();
                return;
            }
            // Transcribe the selected take. The result is stored with the
            // take's sidecar and *shown* here — it never touches the open
            // document until the author presses `i`.
            KeyCode::Char('t') if panel.rec.is_none() && panel.dict_job.is_none() => {
                let take = panel.takes.get(panel.sel).cloned();
                let (take, transcriber) = match (take, self.transcriber.clone()) {
                    (Some(take), Some(transcriber)) => (take, transcriber),
                    (_, _) => {
                        self.audio_panel = Some(panel);
                        self.status = oca_audio::dictation::missing_runtime_hint();
                        return;
                    }
                };
                let label = transcriber.label();
                let Some(root) = core.project().map(|p| p.root().to_path_buf()) else {
                    self.audio_panel = Some(panel);
                    self.status = "no project is open".into();
                    return;
                };
                let store = oca_audio::TakeStore::new(root);
                let (tx, rx) = std::sync::mpsc::channel();
                let job = core.spawn_job(
                    format!("transcribe {}", take.stem()),
                    Priority::Interactive,
                    move |ctx| {
                        let token = ctx.cancel_token();
                        match oca_audio::dictation::transcribe_to_store(
                            &store,
                            &take,
                            transcriber.as_ref(),
                            &token,
                        ) {
                            Ok(text) => {
                                let _ = tx.send(text);
                                Ok(())
                            }
                            // Author cancelled: stop quietly, don't report
                            // a failure for an action they aborted.
                            Err(_) if ctx.is_cancelled() => Ok(()),
                            Err(err) => Err(err),
                        }
                    },
                );
                panel.transcript = None;
                panel.preview = None;
                panel.dict_job = Some((job, DictJobKind::Transcribe));
                panel.dict_rx = Some(rx);
                self.audio_panel = Some(panel);
                self.status = format!("transcribing with {label} — Esc stops");
                return;
            }
            // LLM cleanup of the active transcript — a *preview* streamed
            // into the panel; it is never written anywhere by itself.
            KeyCode::Char('c' | 'p')
                if panel.rec.is_none()
                    && panel.dict_job.is_none()
                    && panel.transcript.is_some() =>
            {
                let kind = if key.code == KeyCode::Char('c') {
                    DictJobKind::Cleanup("cleanup preview")
                } else {
                    DictJobKind::Cleanup("prose preview")
                };
                let mode = if key.code == KeyCode::Char('c') {
                    oca_ai::Cleanup::Clean
                } else {
                    oca_ai::Cleanup::Prose
                };
                let Some(ai) = self.ai.clone() else {
                    self.audio_panel = Some(panel);
                    self.status =
                        "cleanup needs a language model — optional; dictation works without one"
                            .into();
                    return;
                };
                let text = panel.transcript.clone().unwrap_or_default();
                let (tx, rx) = std::sync::mpsc::channel();
                let request = oca_ai::cleanup_request(mode, &text);
                let label = kind.label().to_string();
                let job = core.spawn_job(label.clone(), Priority::Interactive, move |_ctx| {
                    // Load on first use so the preview needs no trip
                    // through the AI panel (and stays loaded after).
                    if !ai.is_loaded() {
                        ai.load()?;
                    }
                    ai.stream(request, tx).map(|_| ())
                });
                panel.preview = Some(String::new());
                panel.dict_job = Some((job, kind));
                panel.dict_rx = Some(rx);
                self.audio_panel = Some(panel);
                self.status = format!("{label} running — Esc stops");
                return;
            }
            // Insert the reviewed text into the open document's *editor
            // buffer* — the author saves with Ctrl-S; nothing touches files
            // behind their eyes (transcripts never insert themselves).
            KeyCode::Char('i') if panel.rec.is_none() => {
                let text = Self::active_dict_text(&panel);
                let is_manuscript = self
                    .open_rel
                    .as_deref()
                    .is_some_and(|rel| group_of(rel) == "manuscript");
                match (text, is_manuscript) {
                    (Some(text), true) => {
                        // Paragraph-shaped insert: only the gaps that aren't
                        // there yet, on either side of the cursor.
                        let full = self.editor.text();
                        let at = self.editor.cursor().min(full.len());
                        let before: String = full.chars().take(at).collect();
                        let after: String = full.chars().skip(at).collect();
                        let prefix = if before.is_empty() || before.ends_with("\n\n") {
                            ""
                        } else if before.ends_with('\n') {
                            "\n"
                        } else {
                            "\n\n"
                        };
                        let suffix = if after.is_empty() || after.starts_with('\n') {
                            ""
                        } else {
                            "\n\n"
                        };
                        self.editor.insert(&format!("{prefix}{text}{suffix}"));
                        self.dirty = true;
                        self.audio_panel = Some(panel);
                        self.status =
                            "inserted into the editor — Ctrl-S to save (nothing was written yet)"
                                .into();
                        return;
                    }
                    (Some(_), false) => {
                        self.audio_panel = Some(panel);
                        self.status =
                            "open a chapter first (Enter in the tree), then press i".into();
                        return;
                    }
                    (None, _) => {
                        self.audio_panel = Some(panel);
                        self.status = "nothing to insert — press t to transcribe first".into();
                        return;
                    }
                }
            }
            // Send the reviewed text to the project dictation note — the
            // explicit alternative to `i` (still the author's keystroke).
            KeyCode::Char('n') if panel.rec.is_none() => {
                let text = Self::active_dict_text(&panel);
                let take = panel.takes.get(panel.sel).cloned();
                match (text, take) {
                    (Some(text), Some(take)) => {
                        match oca_audio::dictation::add_to_notes(core, &take, &text) {
                            Ok(path) => {
                                self.audio_panel = Some(panel);
                                self.status = format!("added to {}", path.display());
                                return;
                            }
                            Err(e) => {
                                self.audio_panel = Some(panel);
                                self.status = format!("cannot add to notes: {e}");
                                return;
                            }
                        }
                    }
                    _ => {
                        self.audio_panel = Some(panel);
                        self.status = "nothing to add — press t to transcribe first".into();
                        return;
                    }
                }
            }
            KeyCode::Char('r') if panel.rec.is_none() => match audio.record() {
                Ok(rec) => {
                    panel.rec = Some(rec);
                    self.status = "recording — p pause · s save · Esc saves and closes".into();
                }
                Err(e) => self.status = format!("cannot record: {e}"),
            },
            KeyCode::Char('p') => {
                if let Some(rec) = &mut panel.rec {
                    match rec.state() {
                        RecState::Recording => match rec.pause() {
                            Ok(()) => self.status = "recording paused".into(),
                            Err(e) => self.status = format!("cannot pause: {e}"),
                        },
                        RecState::Paused => match rec.resume() {
                            Ok(()) => self.status = "recording resumed".into(),
                            Err(e) => self.status = format!("cannot resume: {e}"),
                        },
                    }
                }
            }
            KeyCode::Char('s') if panel.rec.is_some() => {
                panel = self.stop_and_save(core, panel);
            }
            KeyCode::Enter => {
                if let Some(take) = panel.takes.get(panel.sel).cloned() {
                    match audio.play(&take) {
                        Ok(player) => {
                            panel.player = Some(player);
                            self.status = format!("playing {}", take.stem());
                        }
                        Err(e) => self.status = format!("cannot play: {e}"),
                    }
                }
            }
            KeyCode::Char('a') => {
                if let Some(take) = panel.takes.get(panel.sel).cloned() {
                    let spec = panel.ctx.target_spec();
                    match audio
                        .resolve_target(&spec)
                        .and_then(|target| audio.attach(&take, &target))
                    {
                        Ok(updated) => {
                            self.status = format!(
                                "{} → {}",
                                updated.stem(),
                                updated
                                    .meta
                                    .attached
                                    .as_ref()
                                    .map(|a| a.describe())
                                    .unwrap_or_default()
                            );
                            panel.takes[panel.sel] = updated;
                        }
                        Err(e) => self.status = format!("cannot attach: {e}"),
                    }
                }
            }
            KeyCode::Char('d') if panel.rec.is_none() => {
                if let Some(take) = panel.takes.get(panel.sel).cloned() {
                    match audio.delete(&take) {
                        Ok(()) => {
                            panel.takes.remove(panel.sel);
                            panel.sel = panel.sel.min(panel.takes.len().saturating_sub(1));
                            self.status = format!("deleted {}", take.stem());
                        }
                        Err(e) => self.status = format!("cannot delete: {e}"),
                    }
                }
            }
            KeyCode::Down => {
                if panel.sel + 1 < panel.takes.len() {
                    if self.stop_dict_job(core, &mut panel) {
                        panel.preview = None;
                    }
                    panel.sel += 1;
                    panel.transcript = panel.takes[panel.sel]
                        .meta
                        .transcript
                        .as_ref()
                        .map(|t| t.text.clone());
                }
            }
            KeyCode::Up if panel.sel > 0 => {
                if self.stop_dict_job(core, &mut panel) {
                    panel.preview = None;
                }
                panel.sel -= 1;
                panel.transcript = panel.takes[panel.sel]
                    .meta
                    .transcript
                    .as_ref()
                    .map(|t| t.text.clone());
            }
            _ => {}
        }
        self.audio_panel = Some(panel);
    }

    /// Stop the active recording and save it into the panel's directory,
    /// selecting the new take. Returns the panel for the caller to store.
    fn stop_and_save(&mut self, core: &Core, mut panel: AudioPanel) -> AudioPanel {
        let Some(rec) = panel.rec.take() else {
            return panel;
        };
        let done = rec.stop();
        let host = Arc::clone(&self.audio_host);
        let audio = match Audio::new(core, host) {
            Ok(audio) => audio,
            Err(e) => {
                self.status = format!("cannot save recording: {e}");
                return panel;
            }
        };
        match audio.save_take(&done, &panel.dir_rel, None) {
            Ok(take) => {
                panel.takes.push(take);
                panel.sel = panel.takes.len() - 1;
                let sel = panel.sel;
                self.status = format!(
                    "saved {} — Enter plays · a attaches to {}",
                    panel.takes[sel].stem(),
                    panel.ctx.describe()
                );
            }
            Err(e) => self.status = format!("cannot save recording: {e}"),
        }
        panel
    }

    // ---- snapshot panel (Phase 6) -------------------------------------------

    /// Open the panel with the project's restore points (oldest first).
    fn open_snap_panel(&mut self, core: &Core) {
        let mut panel = SnapshotPanel {
            snapshots: Vec::new(),
            sel: 0,
            mode: SnapMode::List,
        };
        match core.snapshots() {
            Ok(list) => {
                panel.snapshots = list;
                panel.sel = panel.snapshots.len().saturating_sub(1);
                self.status = format!(
                    "{} snapshot(s) — c saves now, Enter shows changes, r restores",
                    panel.snapshots.len()
                );
            }
            Err(e) => self.status = format!("snapshots: {e}"),
        }
        self.snap_panel = Some(panel);
        self.focus = Focus::Snapshots;
    }

    fn snap_key(&mut self, core: &mut Core, key: KeyEvent) {
        let Some(mut panel) = self.snap_panel.take() else {
            return;
        };
        match key.code {
            KeyCode::Esc => match panel.mode {
                SnapMode::Diff { .. } => panel.mode = SnapMode::List,
                SnapMode::List => {
                    self.focus = Focus::Tree;
                    self.status = "snapshots closed".into();
                    return;
                }
            },
            KeyCode::Up => match &mut panel.mode {
                SnapMode::List => panel.sel = panel.sel.saturating_sub(1),
                SnapMode::Diff { scroll, .. } => *scroll = scroll.saturating_sub(1),
            },
            KeyCode::Down => match &mut panel.mode {
                SnapMode::List => {
                    if panel.sel + 1 < panel.snapshots.len() {
                        panel.sel += 1;
                    }
                }
                SnapMode::Diff { scroll, .. } => *scroll += 1,
            },
            KeyCode::Enter => match panel.mode {
                // List: load the diff; Diff: back to the list.
                SnapMode::List => {
                    if let Some(info) = panel.snapshots.get(panel.sel) {
                        match core.snapshot_diff(info.n, None) {
                            Ok(report) => {
                                let mut lines = Vec::new();
                                for f in &report.files {
                                    if f.binary {
                                        lines.push(format!(
                                            "{}: {} (binary)",
                                            match f.kind {
                                                oca_core::ChangeKind::Added => "added",
                                                oca_core::ChangeKind::Removed => "removed",
                                                oca_core::ChangeKind::Modified => "changed",
                                            },
                                            f.path.display()
                                        ));
                                    } else {
                                        lines.extend(f.patch.lines().map(str::to_string));
                                    }
                                }
                                lines.push(String::new());
                                lines.push(format!(
                                    "{} — Snapshot #{} vs your files now",
                                    report.summary, info.n
                                ));
                                panel.mode = SnapMode::Diff {
                                    lines,
                                    scroll: 0,
                                    title: format!(" Snapshot #{} ", info.n),
                                };
                            }
                            Err(e) => self.status = format!("diff: {e}"),
                        }
                    }
                }
                SnapMode::Diff { .. } => panel.mode = SnapMode::List,
            },
            KeyCode::Char('c') if matches!(panel.mode, SnapMode::List) => {
                match core.execute(Command::SnapshotCreate { label: None }) {
                    Ok(events) => {
                        if let Some(Event::SnapshotCreated { n, label, files }) = events.first() {
                            self.status = format!("Snapshot #{n} — {label} · {files} file(s)");
                        }
                        if let Ok(list) = core.snapshots() {
                            panel.snapshots = list;
                            panel.sel = panel.snapshots.len().saturating_sub(1);
                        }
                    }
                    Err(e) => self.status = format!("snapshot not saved: {e}"),
                }
            }
            KeyCode::Char('r') if matches!(panel.mode, SnapMode::List) => {
                if let Some(info) = panel.snapshots.get(panel.sel).cloned() {
                    match core.execute(Command::SnapshotRestore { n: info.n }) {
                        Ok(events) => {
                            if let Some(Event::SnapshotRestored {
                                n,
                                restored,
                                removed,
                                unchanged,
                                audio_differing,
                            }) = events.first()
                            {
                                self.status = format!(
                                    "restored Snapshot #{n} — {restored} restored, {removed} removed, {unchanged} unchanged{}",
                                    if audio_differing.is_empty() {
                                        String::new()
                                    } else {
                                        format!(
                                            " · {} take(s) kept as recorded",
                                            audio_differing.len()
                                        )
                                    }
                                );
                            }
                            if let Ok(list) = core.snapshots() {
                                panel.snapshots = list;
                                panel.sel = panel.sel.min(panel.snapshots.len().saturating_sub(1));
                            }
                        }
                        Err(e) => self.status = format!("restore refused: {e}"),
                    }
                }
            }
            _ => {}
        }
        self.snap_panel = Some(panel);
    }

    // ---- search panel (Phase 5) --------------------------------------------

    /// Keys while the search panel is focused: live query editing, hit
    /// navigation, opening a hit. Esc closes by *not* storing the panel
    /// back (same close semantics as the audio panel).
    fn search_key(&mut self, core: &mut Core, key: KeyEvent) {
        let Some(mut panel) = self.search_panel.take() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.focus = Focus::Tree;
                self.status = "search closed".into();
                return;
            }
            KeyCode::Enter => {
                if let Some(hit) = panel.hits.get(panel.sel) {
                    match Self::hit_target(&self.items, &hit.rel) {
                        Some(rel) => {
                            // Opening moves focus to the editor; the panel
                            // is dropped by not storing it back.
                            self.open_doc(core, rel);
                            return;
                        }
                        None => {
                            self.status = format!(
                                "{} — data, not a tree document (open its file from the tree)",
                                hit.rel.display()
                            );
                        }
                    }
                }
            }
            KeyCode::Down => {
                if panel.sel + 1 < panel.hits.len() {
                    panel.sel += 1;
                }
            }
            KeyCode::Up => panel.sel = panel.sel.saturating_sub(1),
            KeyCode::Backspace => {
                panel.query.pop();
                self.run_search(core, &mut panel);
            }
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                panel.query.push(c);
                self.run_search(core, &mut panel);
            }
            _ => {}
        }
        self.search_panel = Some(panel);
    }

    /// Re-run the query (each keystroke — FTS over the cache is fast even
    /// on a 500k-word corpus, A10).
    fn run_search(&mut self, core: &Core, panel: &mut SearchPanel) {
        panel.sel = 0;
        if panel.query.trim().is_empty() {
            panel.hits.clear();
            self.status = "search — type to filter, Enter opens, Esc closes".into();
            return;
        }
        match core.search(&panel.query, 50) {
            Ok(hits) => {
                self.status = format!("{} match(es)", hits.len());
                panel.hits = hits;
            }
            Err(e) => {
                panel.hits.clear();
                self.status = format!("search: {e}");
            }
        }
    }

    /// The tree document a search hit opens, or `None` for data rows
    /// (takes, raw YAML). Song metadata maps to its lyrics file — that's
    /// the song's editable face in the tree.
    fn hit_target(items: &[TreeItem], hit_rel: &Path) -> Option<PathBuf> {
        let mut rel = hit_rel.to_path_buf();
        if rel.file_name().is_some_and(|n| n == "song.yaml") {
            rel = rel.parent()?.join("lyrics.md");
        }
        items
            .iter()
            .any(|item| matches!(item, TreeItem::Doc { rel: r, .. } if *r == rel))
            .then_some(rel)
    }

    // ---- input -----------------------------------------------------------

    pub fn on_key(&mut self, core: &mut Core, key: KeyEvent) -> Result<()> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Global keys.
        if ctrl {
            match key.code {
                KeyCode::Char('q') => {
                    self.journal_now(core, true);
                    self.quit = true;
                    return Ok(());
                }
                KeyCode::Char('s') => {
                    self.save(core);
                    return Ok(());
                }
                KeyCode::Char('n') => {
                    self.new_chapter(core);
                    return Ok(());
                }
                KeyCode::Char('f') if matches!(self.focus, Focus::Tree | Focus::Editor) => {
                    self.search_panel = Some(SearchPanel {
                        query: String::new(),
                        hits: Vec::new(),
                        sel: 0,
                    });
                    self.focus = Focus::Search;
                    self.status = "search — type to filter, Enter opens, Esc closes".into();
                    return Ok(());
                }
                _ => {}
            }
        }

        // Recovery banner intercepts plain keys until answered.
        if let Some(banner) = self.banner.take() {
            let Banner::Recovery { entry } = banner;
            match key.code {
                KeyCode::Char('r') | KeyCode::Char('R') => {
                    self.words = entry.content.split_whitespace().count();
                    self.editor.load(&entry.content);
                    self.dirty = true;
                    self.focus = Focus::Editor;
                    self.refresh_song_metrics();
                    self.status = "draft restored — Ctrl-S to save it".into();
                }
                KeyCode::Char('d') | KeyCode::Char('D') => {
                    if let Some(rel) = self.open_rel.clone() {
                        let _ = core.execute(Command::ClearRecovery { doc: rel });
                    }
                    self.status = "draft discarded".into();
                }
                KeyCode::Esc => {
                    self.banner = None;
                    self.status = "kept the file on disk (draft still journaled)".into();
                }
                _ => {
                    self.banner = Some(Banner::Recovery { entry });
                }
            }
            return Ok(());
        }

        match self.focus {
            Focus::Tree => self.tree_key(core, key),
            Focus::Editor => self.editor_key(key),
            Focus::Form => self.form_key(core, key),
            Focus::Audio => self.audio_key(core, key),
            Focus::Search => self.search_key(core, key),
            Focus::Snapshots => self.snap_key(core, key),
            Focus::Ai => self.ai_key(core, key),
        }
        Ok(())
    }

    fn tree_key(&mut self, core: &mut Core, key: KeyEvent) {
        match key.code {
            KeyCode::Down => {
                if let Some(i) = next_selectable(&self.items, self.sel, 1) {
                    self.sel = i;
                }
            }
            KeyCode::Up => {
                if let Some(i) = next_selectable(&self.items, self.sel, -1) {
                    self.sel = i;
                }
            }
            KeyCode::PageDown => {
                let mut i = self.sel;
                for _ in 0..10 {
                    if let Some(n) = next_selectable(&self.items, i, 1) {
                        i = n;
                    } else {
                        break;
                    }
                }
                self.sel = i;
            }
            KeyCode::PageUp => {
                let mut i = self.sel;
                for _ in 0..10 {
                    if let Some(n) = next_selectable(&self.items, i, -1) {
                        i = n;
                    } else {
                        break;
                    }
                }
                self.sel = i;
            }
            KeyCode::Home => {
                self.sel = next_selectable(&self.items, 0, 1).unwrap_or(self.sel);
            }
            KeyCode::End => {
                if let Some(i) = next_selectable(&self.items, self.items.len(), -1) {
                    self.sel = i;
                }
            }
            KeyCode::Enter => {
                if let Some(TreeItem::Doc { rel, .. }) = self.items.get(self.sel) {
                    let rel = rel.clone();
                    self.open_doc(core, rel);
                }
            }
            KeyCode::Char('s') | KeyCode::Char('S') => self.open_state_form(),
            KeyCode::Char('a') | KeyCode::Char('A') => self.open_audio_panel(core),
            KeyCode::Char('v') | KeyCode::Char('V') => self.open_snap_panel(core),
            KeyCode::Char('i') | KeyCode::Char('I') => self.open_ai_panel(),
            KeyCode::Tab if self.open_rel.is_some() => {
                self.focus = Focus::Editor;
            }
            _ => {}
        }
    }

    fn editor_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Tab | KeyCode::Esc if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.focus = Focus::Tree;
                return;
            }
            _ => {}
        }
        // Song files: Alt-] / Alt-[ jump between sections (plain brackets
        // must stay typeable — chords are `[Dm]`).
        if self.song.is_some()
            && key.modifiers.contains(KeyModifiers::ALT)
            && matches!(key.code, KeyCode::Char(']') | KeyCode::Char('['))
        {
            self.jump_section(key.code == KeyCode::Char(']'));
            return;
        }
        let (w, h) = self.editor_dims();
        match self.editor.handle_key(key, w, h) {
            KeyOutcome::Edited => {
                self.dirty = true;
                self.words = self.editor.text().split_whitespace().count();
                self.refresh_song_metrics();
            }
            KeyOutcome::Moved => self.refresh_song_metrics(),
            KeyOutcome::Unhandled => {}
        }
    }

    pub fn on_paste(&mut self, _core: &mut Core, text: &str) {
        if self.focus == Focus::Editor && self.open_rel.is_some() {
            self.editor.insert(text);
            self.dirty = true;
            self.words = self.editor.text().split_whitespace().count();
            self.refresh_song_metrics();
        }
    }

    pub fn on_mouse(&mut self, core: &mut Core, mouse: MouseEvent) {
        let pos = ratatui::layout::Position {
            x: mouse.column,
            y: mouse.row,
        };
        if self.tree_area.contains(pos) {
            if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
                // Mirror ratatui's fresh-state scroll (offset shows `sel`).
                let vis_h = self.tree_area.height.saturating_sub(2) as usize;
                let offset = self.sel.saturating_sub(vis_h.saturating_sub(1));
                let row = mouse.row.saturating_sub(self.tree_area.y + 1) as usize;
                if let Some(TreeItem::Doc { rel, .. }) = self.items.get(offset + row) {
                    let rel = rel.clone();
                    self.sel = offset + row;
                    self.open_doc(core, rel);
                }
            }
            return;
        }
        if self.editor_area.contains(pos) {
            let x = mouse.column.saturating_sub(self.editor_area.x);
            let y = mouse.row.saturating_sub(self.editor_area.y);
            if matches!(mouse.kind, MouseEventKind::Down(_)) {
                self.focus = Focus::Editor;
            }
            if self.open_rel.is_some() {
                let (w, h) = self.editor_dims();
                self.editor.handle_mouse(mouse.kind, x, y, w, h);
            }
        }
    }

    // ---- rendering -------------------------------------------------------

    pub fn draw(&mut self, core: &Core, frame: &mut Frame) {
        let root = frame.area();
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(2)])
            .split(root);
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(30), Constraint::Min(10)])
            .split(rows[0]);
        self.tree_area = cols[0];
        self.draw_tree(frame, cols[0]);
        self.draw_editor(frame, cols[1]);
        self.draw_status(core, frame, rows[1]);
        self.draw_form(frame);
        self.draw_audio(frame);
        self.draw_search(frame);
        self.draw_snapshots(frame);
        self.draw_ai(frame);
    }

    fn draw_tree(&self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .items
            .iter()
            .map(|item| match item {
                TreeItem::Group(name) => {
                    ListItem::new(Line::styled(format!(" {name}"), GROUP_STYLE))
                }
                TreeItem::Doc { title, .. } => ListItem::new(Line::from(format!(
                    "  {}",
                    truncate(title, area.width.saturating_sub(6) as usize)
                ))),
            })
            .collect();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" PROJECT "))
            .highlight_symbol("▶ ")
            .highlight_style(SELECTED_STYLE);
        let mut state = ListState::default();
        state.select(Some(self.sel));
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn draw_editor(&mut self, frame: &mut Frame, area: Rect) {
        let title = if self.open_rel.is_some() {
            let mut t = format!(
                " {}{} — {}w ",
                self.doc_title,
                if self.dirty { " *" } else { "" },
                self.words
            );
            // Lyric metrics live in the title while a song is open (the
            // analysis is oca-music's; this is only formatting).
            if let Some(m) = &self.song_metrics {
                t.push_str(&song_metrics_text(m));
            }
            t
        } else {
            " no document ".to_string()
        };
        let block = Block::default().borders(Borders::ALL).title(title);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        self.editor_area = inner;
        match &self.open_rel {
            Some(_) => {
                let (w, h) = self.editor_dims();
                self.editor.ensure_cursor_visible(w, h);
                self.editor
                    .render(inner, frame.buffer_mut(), self.focus == Focus::Editor);
            }
            None => {
                let help = Paragraph::new(vec![
                    Line::from("Select a document in the project tree."),
                    Line::from(""),
                    Line::from("Enter open · Tab switch focus"),
                    Line::from("Ctrl-N new chapter · Ctrl-S save · Ctrl-Q quit"),
                ])
                .style(Style::default().fg(Color::DarkGray));
                frame.render_widget(help, inner);
            }
        }
    }

    /// Centered popup for the "set state at this chapter" form.
    /// Centered popup: the restore-point list, or the selected snapshot's
    /// inline diff (Phase 6, `V` in the tree).
    fn draw_snapshots(&self, frame: &mut Frame) {
        let Some(panel) = &self.snap_panel else {
            return;
        };
        match &panel.mode {
            SnapMode::List => {
                let area = centered(frame.area(), 84, 16);
                let block = Block::default().borders(Borders::ALL).title(" Snapshots ");
                let inner = block.inner(area);
                frame.render_widget(block, area);
                let mut lines = Vec::new();
                if panel.snapshots.is_empty() {
                    lines.push(Line::styled(
                        "no snapshots yet — press c to save the current state",
                        Style::new().fg(Color::DarkGray),
                    ));
                }
                for (i, s) in panel.snapshots.iter().enumerate() {
                    let marker = if i == panel.sel { "▶ " } else { "  " };
                    let mut line = Line::from(format!(
                        "{marker}#{:<4} {:<17} {:>5} file(s)  {}",
                        s.n,
                        oca_core::format_time(s.when),
                        s.files,
                        s.display_label()
                    ));
                    if i == panel.sel {
                        line = line.style(SELECTED_STYLE);
                    }
                    lines.push(line);
                }
                lines.push(Line::from(""));
                lines.push(Line::styled(
                    "↑↓ select · c save now · Enter diff · r restore · Esc close",
                    Style::new().fg(Color::DarkGray),
                ));
                frame.render_widget(Paragraph::new(lines), inner);
            }
            SnapMode::Diff {
                lines,
                scroll,
                title,
            } => {
                let area = centered(frame.area(), 100, 24);
                let block = Block::default().borders(Borders::ALL).title(title.clone());
                let inner = block.inner(area);
                frame.render_widget(block, area);
                let text: Vec<Line> = lines
                    .iter()
                    .map(|l| {
                        if let Some(rest) = l.strip_prefix('+') {
                            Line::styled(format!("+{rest}"), Style::new().fg(Color::Green))
                        } else if let Some(rest) = l.strip_prefix('-') {
                            Line::styled(format!("-{rest}"), Style::new().fg(Color::Red))
                        } else if l.starts_with('@') {
                            Line::styled(l.clone(), Style::new().fg(Color::Cyan))
                        } else {
                            Line::from(l.clone())
                        }
                    })
                    .collect();
                let para = Paragraph::new(text)
                    .scroll((*scroll, 0))
                    .wrap(Wrap { trim: false });
                frame.render_widget(para, inner);
            }
        }
    }

    /// Centered popup: model info, Load/Unload, and the streaming probe
    /// (Phase 7, `I` in the tree). Follows the roadmap mock.
    fn draw_ai(&self, frame: &mut Frame) {
        let Some(panel) = &self.ai_panel else {
            return;
        };
        let area = centered(frame.area(), 74, 24);
        let block = Block::default().borders(Borders::ALL).title(" AI Model ");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let mut lines: Vec<Line> = Vec::new();
        match &self.ai {
            Some(ai) => {
                let info = ai.info();
                lines.push(Line::styled(
                    format!("● {} ({})", info.name, info.runtime),
                    Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
                ));
                lines.push(Line::from(format!("  Context: {}", info.context)));
                lines.push(Line::from(format!(
                    "  GPU: {}",
                    if info.gpu_layers > 0 {
                        format!("enabled ({} layers)", info.gpu_layers)
                    } else {
                        "disabled (CPU)".to_string()
                    }
                )));
                lines.push(Line::from(format!(
                    "  Threads: {}",
                    if info.threads == 0 {
                        "auto".to_string()
                    } else {
                        info.threads.to_string()
                    }
                )));
                lines.push(Line::from(format!("  {}", ai.policy())));
                if let Some(tier) = oca_ai::Hardware::detect().tier() {
                    lines.push(Line::styled(
                        format!("  hardware: {}", tier.describe()),
                        Style::new().fg(Color::DarkGray),
                    ));
                }
                lines.push(Line::from(""));
                match panel.state {
                    AiState::Idle => lines.push(Line::from("[Load]")),
                    AiState::Loading => lines.push(Line::styled(
                        "Loading model…",
                        Style::new().fg(Color::Yellow),
                    )),
                    AiState::Ready => lines.push(Line::from("[t probe]  [Unload]")),
                    AiState::Streaming => {}
                }
                if matches!(panel.state, AiState::Streaming) {
                    // Roadmap mock: bar + [ESC] Stop.
                    let budget = 64usize; // probe request's max_tokens
                    let filled = (panel.stream_tokens * 20 / budget).min(20);
                    lines.push(Line::from(""));
                    lines.push(Line::styled("Generating…", Style::new().fg(Color::Yellow)));
                    lines.push(Line::styled(
                        format!("{}{}", "█".repeat(filled), "░".repeat(20 - filled)),
                        Style::new().fg(Color::Cyan),
                    ));
                    lines.push(Line::from(format!("{} token(s)", panel.stream_tokens)));
                    lines.push(Line::styled("  [ESC] Stop", Style::new().fg(Color::Yellow)));
                } else if !panel.stream_text.is_empty() {
                    lines.push(Line::from(""));
                    lines.push(Line::styled(
                        "Last probe output:",
                        Style::new().fg(Color::DarkGray),
                    ));
                    let mut shown = panel.stream_text.chars().take(180).collect::<String>();
                    if panel.stream_text.chars().count() > 180 {
                        shown.push('…');
                    }
                    lines.push(Line::styled(shown, Style::new().fg(Color::DarkGray)));
                }
                if let Some(error) = &panel.error {
                    lines.push(Line::from(""));
                    lines.push(Line::styled(
                        format!("⚠ {error}"),
                        Style::new().fg(Color::Red),
                    ));
                }
                lines.push(Line::from(""));
                lines.push(Line::styled(
                    "Models in ~/.oca/models/llm:",
                    Style::new().fg(Color::DarkGray),
                ));
                if ai.models().is_empty() {
                    lines.push(Line::styled(
                        "  (none discovered)",
                        Style::new().fg(Color::DarkGray),
                    ));
                }
                for model in ai.models().iter().take(3) {
                    lines.push(Line::from(format!(
                        "  {} ({})",
                        model.name,
                        oca_ai::human_size(model.bytes)
                    )));
                }
            }
            None => {
                lines.push(Line::styled(
                    "● no AI runtime",
                    Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ));
                lines.push(Line::from("AI is optional — everything works without it."));
                lines.push(Line::from(""));
                lines.push(Line::from("To enable:"));
                lines.push(Line::from(
                    "  1. side-load a runtime pack: oca runtime install --from <pack.tar.gz>",
                ));
                lines.push(Line::from("  2. drop a .gguf model into ~/.oca/models/llm"));
                lines.push(Line::from("  3. reopen the project, then press l to load"));
            }
        }
        lines.push(Line::from(""));
        lines.push(Line::styled(
            "l load · u unload · t probe · Esc close",
            Style::new().fg(Color::DarkGray),
        ));
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }

    fn draw_form(&self, frame: &mut Frame) {
        let Some(form) = &self.form else {
            return;
        };
        let area = centered(frame.area(), 58, 8);
        let block = Block::default().borders(Borders::ALL).title(format!(
            " Set state — {} at {} ",
            form.char_name, form.chapter_title
        ));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let mut lines: Vec<Line> = STATE_FIELDS
            .iter()
            .zip(form.buffers.iter())
            .enumerate()
            .map(|(i, (label, buf))| {
                let active = i == form.idx;
                let line = Line::from(format!(
                    "{}{label:<10} [{}]",
                    if active { "> " } else { "  " },
                    truncate(buf, 32)
                ));
                if active {
                    line.style(Style::new().fg(Color::Cyan))
                } else {
                    line
                }
            })
            .collect();
        lines.push(Line::from(""));
        lines.push(Line::styled(
            "Enter apply · Esc cancel",
            Style::new().fg(Color::DarkGray),
        ));
        frame.render_widget(Paragraph::new(lines), inner);
    }

    /// Centered popup listing takes with record/play controls.
    fn draw_audio(&self, frame: &mut Frame) {
        let Some(panel) = &self.audio_panel else {
            return;
        };
        // Grow when there's dictation text to review — the transcript is
        // half the point of opening the panel.
        let has_text = panel.transcript.is_some() || panel.preview.is_some();
        let height = if has_text { 25 } else { 18 };
        let area = centered(frame.area(), 78, height);
        let rec_badge = match &panel.rec {
            Some(rec) => match rec.state() {
                RecState::Recording => "  ● RECORDING".to_string(),
                RecState::Paused => "  ● PAUSED".to_string(),
            },
            None => String::new(),
        };
        let playing = if panel.player.is_some() { "  ♪" } else { "" };
        let working = if panel.dict_job.is_some() {
            "  ⟳ working (Esc stops)"
        } else {
            ""
        };
        let block = Block::default().borders(Borders::ALL).title(format!(
            " Audio — {}{rec_badge}{playing}{working} ",
            panel.dir_rel.display()
        ));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let mut lines = vec![
            Line::styled(
                format!("new takes attach to: {}", panel.ctx.describe()),
                Style::new().fg(Color::DarkGray),
            ),
            Line::from(""),
        ];
        if panel.takes.is_empty() {
            lines.push(Line::styled(
                "no takes yet — press r to record",
                Style::new().fg(Color::DarkGray),
            ));
        }
        for (i, take) in panel.takes.iter().enumerate() {
            let marker = if i == panel.sel { "▶ " } else { "  " };
            let attached = take
                .meta
                .attached
                .as_ref()
                .map(|a| a.describe())
                .unwrap_or_else(|| "—".into());
            let note = if take.meta.note.is_some() { " ✎" } else { "" };
            let transcript = if take.meta.transcript.is_some() {
                " ≡"
            } else {
                ""
            };
            let line = Line::from(format!(
                "{marker}{:<22} {:>5.1}s  {}{note}{transcript}",
                truncate(&take.stem(), 22),
                take.duration_secs(),
                attached
            ));
            lines.push(if i == panel.sel {
                line.style(SELECTED_STYLE)
            } else {
                line
            });
        }
        lines.push(Line::from(""));
        // The active text: a cleanup preview under review, else the raw
        // transcript. Shown, never applied — `i`/`n` are the say-so.
        let active = match (&panel.preview, &panel.transcript) {
            (Some(preview), _) => Some((
                "cleanup preview — review before using (AI output is never written by itself)",
                preview.as_str(),
            )),
            (None, Some(transcript)) => {
                Some(("transcript — stored with the take", transcript.as_str()))
            }
            (None, None) => None,
        };
        if let Some((label, text)) = active {
            lines.push(Line::styled(label, Style::new().fg(Color::DarkGray)));
            let shown: Vec<&str> = text.lines().take(7).collect();
            for l in &shown {
                lines.push(Line::from(truncate(l.trim_end(), 74)));
            }
            if text.lines().count() > shown.len() || text.lines().count() == 0 {
                lines.push(Line::styled("…", Style::new().fg(Color::DarkGray)));
            }
        }
        lines.push(Line::from(""));
        if has_text {
            // Cleanup keys only exist for the author when a language model
            // is installed (Phase 7B spec: the LLM options stay invisible
            // otherwise — dictation itself never needs them).
            let mut keys = "t transcribe · i insert · n to notes".to_string();
            if self.ai.is_some() {
                keys.push_str(" · c clean · p prose");
            }
            lines.push(Line::styled(keys, Style::new().fg(Color::DarkGray)));
        }
        lines.push(Line::styled(
            "↑↓ select · Enter play · a attach · d delete · r record · p pause · s save · Esc close",
            Style::new().fg(Color::DarkGray),
        ));
        frame.render_widget(Paragraph::new(lines), inner);
    }

    /// Centered popup: query line + live hit list (Phase 5 search, Ctrl-F).
    fn draw_search(&self, frame: &mut Frame) {
        let Some(panel) = &self.search_panel else {
            return;
        };
        let area = centered(frame.area(), 84, 20);
        let block = Block::default().borders(Borders::ALL).title(" Search ");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let mut lines = vec![
            Line::from(format!(
                "{}▏",
                if panel.query.is_empty() {
                    Span::styled(
                        "type to search chapters, characters, songs, notes, takes…",
                        Style::new().fg(Color::DarkGray),
                    )
                } else {
                    Span::raw(panel.query.clone())
                }
            )),
            Line::from(""),
        ];
        if !panel.query.is_empty() && panel.hits.is_empty() {
            lines.push(Line::styled("no matches", Style::new().fg(Color::DarkGray)));
        }
        for (i, hit) in panel.hits.iter().enumerate() {
            let marker = if i == panel.sel { "▶ " } else { "  " };
            let snippet = hit.snippet.replace('\n', " ");
            let mut line = Line::from(format!(
                "{marker}{:<9} {} — {}",
                hit.kind,
                truncate(&hit.title, 26),
                truncate(&snippet, 34)
            ));
            if i == panel.sel {
                line = line.style(SELECTED_STYLE);
            }
            lines.push(line);
        }
        lines.push(Line::from(""));
        lines.push(Line::styled(
            "type filter · ↑↓ select · Enter open · Esc close",
            Style::new().fg(Color::DarkGray),
        ));
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn draw_status(&self, core: &Core, frame: &mut Frame, area: Rect) {
        let line1 = if let Some(Banner::Recovery { entry }) = &self.banner {
            Line::from(vec![
                Span::styled(
                    " unsaved draft found ",
                    Style::new().fg(Color::Black).bg(Color::Yellow),
                ),
                Span::raw(format!(
                    "R restore ({} chars) · D discard · Esc keep file",
                    entry.content.len()
                )),
            ])
        } else if core.is_read_only() {
            Line::from(Span::styled(
                format!(
                    " READ-ONLY — {}",
                    core.read_only_reason().unwrap_or_default()
                ),
                Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
            ))
        } else {
            Line::from(format!(" {}", self.status))
        };
        let hints = match self.focus {
            Focus::Editor if self.song.is_some() => {
                " Tab/ Esc tree · Ctrl-F search · Alt-]/Alt-[ section · Ctrl-S save · Ctrl-Q quit"
            }
            Focus::Editor => {
                " Tab/ Esc tree · Ctrl-F search · Ctrl-S save · Ctrl-N chapter · Ctrl-Q quit"
            }
            Focus::Tree => {
                " ↑↓ select · Enter open · S state · A audio · V snapshots · I AI · Ctrl-F search · Tab editor · Ctrl-N chapter · Ctrl-Q quit"
            }
            Focus::Form => " Tab field · Enter apply · Esc cancel",
            Focus::Audio => {
                " ↑↓ select · Enter play · a attach · d delete · r record · p pause · s save · Esc close"
            }
            Focus::Search => " type filter · ↑↓ select · Enter open · Esc close",
            Focus::Snapshots => " ↑↓ select · c save now · Enter diff · r restore · Esc close",
            Focus::Ai => " l load · u unload · t probe · Esc close",
        };
        let line2 = Line::from(Span::styled(hints, Style::new().fg(Color::DarkGray)));
        frame.render_widget(Paragraph::new(vec![line1, line2]), area);
    }
}

/// Editor-title fragment with the live lyric metrics. Formatting only —
/// the numbers come from `oca_music::cursor_metrics`. An unsupported
/// language says so instead of showing possibly-wrong numbers (A9).
fn song_metrics_text(m: &CursorMetrics) -> String {
    let section = m.section.as_deref().unwrap_or("(top)");
    if !m.language_supported {
        return format!("· {section} · syllables unsupported ");
    }
    match (m.line_syllables, m.section_syllables) {
        (Some(line), Some(total)) => format!("· {section} · {line}/{total} syllables "),
        (Some(line), None) => format!("· {section} · {line} syllables "),
        _ => format!("· {section} "),
    }
}

fn build_items(core: &Core) -> Vec<TreeItem> {
    let mut items = Vec::new();
    for (group, docs) in core.groups() {
        items.push(TreeItem::Group(group));
        for doc in docs {
            items.push(TreeItem::Doc {
                rel: doc.rel.clone(),
                title: doc.title.clone(),
            });
        }
    }
    items
}

/// Next selectable (document) index from `from` stepping `dir`, wrapping not
/// intended: returns None at the edges.
fn next_selectable(items: &[TreeItem], from: usize, dir: isize) -> Option<usize> {
    if items.is_empty() {
        return None;
    }
    let len = items.len() as isize;
    let mut i = from as isize;
    if i >= len {
        i = len - 1;
    }
    if i < 0 {
        i = 0;
    }
    loop {
        i += dir;
        if i < 0 || i >= len {
            return None;
        }
        if matches!(items[i as usize], TreeItem::Doc { .. }) {
            return Some(i as usize);
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if max == 0 || s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// First path component — which project directory a document lives in.
fn group_of(rel: &Path) -> &str {
    rel.components()
        .next()
        .and_then(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .unwrap_or("")
}

/// A `w`×`h` rect centered inside `area`, clamped to its size.
fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use oca_ai::LanguageModel;
    use oca_audio::MockHost;
    use oca_core::Event;
    use oca_model::ProjectKind;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tempfile::TempDir;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }
    fn alt(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
    }

    fn project_with_chapter(dir: &TempDir) -> (Core, PathBuf) {
        let mut core = Core::new();
        core.execute(Command::CreateProject {
            root: dir.path().to_path_buf(),
            title: "Tui Novel".into(),
            kind: ProjectKind::Novel,
        })
        .unwrap();
        let events = core
            .execute(Command::CreateDocument {
                kind: DocumentKind::Chapter,
                title: "First".into(),
            })
            .unwrap();
        let Event::DocumentCreated { rel, .. } = &events[0] else {
            panic!("expected DocumentCreated");
        };
        (core, rel.clone())
    }

    #[test]
    fn tree_lists_documents_and_enter_opens() {
        let tmp = TempDir::new().unwrap();
        let (mut core, rel) = project_with_chapter(&tmp);
        let mut app = App::new(&core, Arc::new(MockHost::new()));

        assert_eq!(app.items.len(), 2, "group + one document");
        assert!(matches!(app.items[app.sel], TreeItem::Doc { .. }));

        app.on_key(&mut core, key(KeyCode::Enter)).unwrap();
        assert_eq!(app.open_rel.as_ref(), Some(&rel));
        assert_eq!(app.focus, Focus::Editor);
        assert!(app.editor.text().starts_with("# First"));
    }

    #[test]
    fn typing_then_ctrl_s_writes_through_core() {
        let tmp = TempDir::new().unwrap();
        let (mut core, rel) = project_with_chapter(&tmp);
        let mut app = App::new(&core, Arc::new(MockHost::new()));
        app.on_key(&mut core, key(KeyCode::Enter)).unwrap();

        for c in "It begins.".chars() {
            app.on_key(&mut core, key(KeyCode::Char(c))).unwrap();
        }
        assert!(app.dirty);
        app.on_key(&mut core, ctrl('s')).unwrap();
        assert!(!app.dirty, "Ctrl-S clears the dirty flag");

        let doc = core.read_document(&rel).unwrap();
        assert!(doc.body().contains("It begins."), "body = {:?}", doc.body());
        // Frontmatter untouched by the body save.
        assert!(doc.frontmatter.get_str("id").unwrap().starts_with("ch_"));
    }

    /// Phase 1 exit, TUI half: save → close (drop the session) → reopen in a
    /// fresh Core → continue writing. Frontmatter bytes must be identical
    /// across sessions; the body grows.
    #[test]
    fn tui_close_reopen_continue_roundtrip_is_byte_identical() {
        let tmp = TempDir::new().unwrap();
        let (mut core, rel) = project_with_chapter(&tmp);

        // Session 1: open, type, save, quit, close.
        let mut app = App::new(&core, Arc::new(MockHost::new()));
        app.on_key(&mut core, key(KeyCode::Enter)).unwrap();
        for c in "Session one wrote this.".chars() {
            app.on_key(&mut core, key(KeyCode::Char(c))).unwrap();
        }
        app.on_key(&mut core, ctrl('s')).unwrap();
        assert!(!app.dirty, "saved");
        app.on_key(&mut core, ctrl('q')).unwrap();
        assert!(app.should_quit());
        drop(app);
        let bytes_v1 = std::fs::read(tmp.path().join(&rel)).unwrap();

        // Session 2: reopen in a fresh Core (lock released on close).
        drop(core);
        let mut core = Core::new();
        core.execute(Command::OpenProject {
            root: tmp.path().to_path_buf(),
        })
        .unwrap();
        let mut app = App::new(&core, Arc::new(MockHost::new()));
        app.on_key(&mut core, key(KeyCode::Enter)).unwrap();
        assert!(
            app.editor.text().contains("Session one wrote this."),
            "session 2 must reopen the saved text"
        );
        assert!(
            app.banner.is_none(),
            "no draft to recover after a clean save"
        );
        for c in " Session two continues.".chars() {
            app.on_key(&mut core, key(KeyCode::Char(c))).unwrap();
        }
        app.on_key(&mut core, ctrl('s')).unwrap();

        let bytes_v2 = std::fs::read(tmp.path().join(&rel)).unwrap();
        assert!(String::from_utf8_lossy(&bytes_v2).contains("Session two continues."));
        assert_eq!(
            frontmatter_section(&bytes_v1),
            frontmatter_section(&bytes_v2),
            "frontmatter must round-trip byte-identically across sessions"
        );
    }

    fn frontmatter_section(bytes: &[u8]) -> &[u8] {
        let end = bytes
            .windows(5)
            .position(|w| w == b"\n---\n")
            .map(|i| i + 5)
            .unwrap_or(0);
        &bytes[..end]
    }

    #[test]
    fn ctrl_q_quits_and_journals_dirty_draft() {
        let tmp = TempDir::new().unwrap();
        let (mut core, rel) = project_with_chapter(&tmp);
        let mut app = App::new(&core, Arc::new(MockHost::new()));
        app.on_key(&mut core, key(KeyCode::Enter)).unwrap();
        for c in "unsaved thoughts".chars() {
            app.on_key(&mut core, key(KeyCode::Char(c))).unwrap();
        }
        app.on_key(&mut core, ctrl('q')).unwrap();
        assert!(app.should_quit());
        let entry = core.recovery_for(&rel).unwrap().expect("journaled on quit");
        assert!(entry.content.contains("unsaved thoughts"));
    }

    #[test]
    fn recovery_banner_restore_and_discard() {
        let tmp = TempDir::new().unwrap();
        let (mut core, rel) = project_with_chapter(&tmp);
        core.execute(Command::WriteRecovery {
            doc: rel.clone(),
            content: "# First\n\nThe remembered draft.\n".into(),
        })
        .unwrap();

        let mut app = App::new(&core, Arc::new(MockHost::new()));
        app.on_key(&mut core, key(KeyCode::Enter)).unwrap();
        assert!(app.banner.is_some(), "differing draft must show a banner");
        assert!(app.status.contains("opened"));

        // r → restores the draft into the editor
        app.on_key(&mut core, key(KeyCode::Char('r'))).unwrap();
        assert!(app.editor.text().contains("remembered draft"));
        assert!(app.dirty);
        assert!(app.banner.is_none());

        // And a second run: discard path.
        app.on_key(&mut core, ctrl('q')).unwrap();
        drop(app);
        let mut app = App::new(&core, Arc::new(MockHost::new()));
        app.on_key(&mut core, key(KeyCode::Enter)).unwrap();
        assert!(app.banner.is_some());
        app.on_key(&mut core, key(KeyCode::Char('d'))).unwrap();
        assert!(app.banner.is_none());
        assert!(core.recovery_for(&rel).unwrap().is_none(), "discard clears");
    }

    #[test]
    fn read_only_project_rejects_save_with_visible_reason() {
        let tmp = TempDir::new().unwrap();
        let mut holder = Core::new();
        holder
            .execute(Command::CreateProject {
                root: tmp.path().to_path_buf(),
                title: "Locked".into(),
                kind: ProjectKind::Novel,
            })
            .unwrap();
        holder
            .execute(Command::CreateDocument {
                kind: DocumentKind::Chapter,
                title: "First".into(),
            })
            .unwrap();

        let mut core = Core::new();
        core.execute(Command::OpenProject {
            root: tmp.path().to_path_buf(),
        })
        .unwrap();
        let mut app = App::new(&core, Arc::new(MockHost::new()));
        assert!(
            app.status.contains("another session"),
            "status: {}",
            app.status
        );

        // Open a doc and try to save: must fail visibly, file unchanged.
        app.on_key(&mut core, key(KeyCode::Enter)).unwrap();
        for c in "edit".chars() {
            app.on_key(&mut core, key(KeyCode::Char(c))).unwrap();
        }
        app.on_key(&mut core, ctrl('s')).unwrap();
        assert!(app.dirty, "failed save keeps dirty");
        assert!(app.status.contains("read-only"), "status: {}", app.status);
    }

    #[test]
    fn ctrl_n_creates_chapter_and_opens_it() {
        let tmp = TempDir::new().unwrap();
        let (mut core, _) = project_with_chapter(&tmp);
        let mut app = App::new(&core, Arc::new(MockHost::new()));
        app.on_key(&mut core, ctrl('n')).unwrap();
        assert_eq!(app.items.len(), 3, "group now has two documents");
        assert!(
            app.open_rel
                .as_ref()
                .is_some_and(|p| p.to_string_lossy().contains("002-")),
            "new chapter opened: {:?}",
            app.open_rel
        );
    }

    #[test]
    fn draws_without_panic_via_test_backend() {
        let tmp = TempDir::new().unwrap();
        let (mut core, _) = project_with_chapter(&tmp);
        let mut app = App::new(&core, Arc::new(MockHost::new()));
        app.on_key(&mut core, key(KeyCode::Enter)).unwrap();
        for c in "some text".chars() {
            app.on_key(&mut core, key(KeyCode::Char(c))).unwrap();
        }

        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| app.draw(&core, frame))
            .expect("frame renders");
        assert!(app.editor_area.width > 0 && app.editor_area.height > 0);
    }

    #[test]
    fn paste_inserts_when_editor_focused() {
        let tmp = TempDir::new().unwrap();
        let (mut core, rel) = project_with_chapter(&tmp);
        let mut app = App::new(&core, Arc::new(MockHost::new()));
        app.on_key(&mut core, key(KeyCode::Enter)).unwrap();
        app.on_paste(&mut core, "pasted chunk");
        assert!(app.dirty);
        assert!(app.editor.text().contains("pasted chunk"));
        let _ = rel;
    }

    /// A project with one chapter and one character; returns both rels.
    fn project_with_chapter_and_character(dir: &TempDir) -> (Core, PathBuf, PathBuf) {
        let (mut core, chapter) = project_with_chapter(dir);
        let events = core
            .execute(Command::CreateDocument {
                kind: DocumentKind::Character,
                title: "Elena".into(),
            })
            .unwrap();
        let Event::DocumentCreated { rel, .. } = &events[0] else {
            panic!("expected DocumentCreated");
        };
        (core, chapter, rel.clone())
    }

    /// Phase 2 v2 amend: the "set state at this chapter" form applies through
    /// the story engine, and engine queries see the result immediately.
    #[test]
    fn set_state_form_applies_state_through_story_engine() {
        let tmp = TempDir::new().unwrap();
        let (mut core, chapter_rel, char_rel) = project_with_chapter_and_character(&tmp);
        let mut app = App::new(&core, Arc::new(MockHost::new()));

        app.on_key(&mut core, key(KeyCode::Enter)).unwrap(); // open the chapter
        app.on_key(&mut core, key(KeyCode::Tab)).unwrap(); // back to the tree
        app.on_key(&mut core, key(KeyCode::Down)).unwrap(); // select Elena
        app.on_key(&mut core, key(KeyCode::Char('s'))).unwrap();
        assert_eq!(app.focus, Focus::Form, "S on a character opens the form");
        assert_eq!(app.form.as_ref().unwrap().chapter_rel, chapter_rel);

        for c in "blackwood".chars() {
            app.on_key(&mut core, key(KeyCode::Char(c))).unwrap();
        }
        app.on_key(&mut core, key(KeyCode::Tab)).unwrap(); // next field
        for c in "frightened".chars() {
            app.on_key(&mut core, key(KeyCode::Char(c))).unwrap();
        }
        app.on_key(&mut core, key(KeyCode::Enter)).unwrap();
        assert!(app.form.is_none(), "applying closes the form");
        assert_eq!(app.focus, Focus::Tree);
        assert!(app.status.contains("state set"), "status: {}", app.status);

        // The entity file carries the entry; engine queries see it at once.
        let story = oca_story::Story::new(&core);
        let chapter_id = story.chapters().unwrap()[0].id.clone();
        let doc = core.read_document(&char_rel).unwrap();
        let states = doc.frontmatter.get_value("states").unwrap();
        assert_eq!(states.as_sequence().unwrap().len(), 1);
        assert_eq!(
            states.as_sequence().unwrap()[0].get("at").unwrap().as_str(),
            Some(chapter_id.as_str())
        );
        assert_eq!(
            story.where_is("Elena", None).unwrap().get("location"),
            Some("blackwood")
        );
    }

    #[test]
    fn set_state_form_guards_and_cancels_without_writing() {
        let tmp = TempDir::new().unwrap();
        let (mut core, _chapter, char_rel) = project_with_chapter_and_character(&tmp);
        let mut app = App::new(&core, Arc::new(MockHost::new()));

        // Character selected but no chapter open: refused with a reason.
        app.on_key(&mut core, key(KeyCode::Down)).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('s'))).unwrap();
        assert!(app.form.is_none());
        assert!(
            app.status.contains("open a chapter"),
            "status: {}",
            app.status
        );

        // An open chapter is not a character.
        app.on_key(&mut core, key(KeyCode::Up)).unwrap();
        app.on_key(&mut core, key(KeyCode::Enter)).unwrap();
        app.on_key(&mut core, key(KeyCode::Tab)).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('s'))).unwrap();
        assert!(app.form.is_none());
        assert!(
            app.status.contains("select a character"),
            "status: {}",
            app.status
        );

        // Esc cancels without writing; an empty submit is refused.
        app.on_key(&mut core, key(KeyCode::Down)).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('s'))).unwrap();
        assert!(app.form.is_some());
        app.on_key(&mut core, key(KeyCode::Enter)).unwrap();
        assert!(app.form.is_some(), "empty fields do not apply");
        assert!(
            app.status.contains("nothing to set"),
            "status: {}",
            app.status
        );
        for c in "village".chars() {
            app.on_key(&mut core, key(KeyCode::Char(c))).unwrap();
        }
        app.on_key(&mut core, key(KeyCode::Esc)).unwrap();
        assert!(app.form.is_none());
        assert_eq!(app.focus, Focus::Tree);
        let doc = core.read_document(&char_rel).unwrap();
        assert!(
            doc.frontmatter.get_value("states").is_none(),
            "cancel must not touch the file"
        );
    }

    #[test]
    fn form_renders_over_the_frame() {
        let tmp = TempDir::new().unwrap();
        let (mut core, _chapter, _char) = project_with_chapter_and_character(&tmp);
        let mut app = App::new(&core, Arc::new(MockHost::new()));
        app.on_key(&mut core, key(KeyCode::Enter)).unwrap();
        app.on_key(&mut core, key(KeyCode::Tab)).unwrap();
        app.on_key(&mut core, key(KeyCode::Down)).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('s'))).unwrap();
        assert!(app.form.is_some());

        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| app.draw(&core, frame))
            .expect("frame renders");
    }

    /// Phase 3: while a song's lyrics.md is open, Alt-]/Alt-[ jump between
    /// sections (plain brackets stay typeable — chords are `[Dm]`) and the
    /// editor title carries live syllable metrics from `oca-music`.
    #[test]
    fn song_editor_section_jumps_and_metrics() {
        let tmp = TempDir::new().unwrap();
        let (mut core, _chapter) = project_with_chapter(&tmp);
        let entry = oca_music::Music::new_song(&mut core, "test-song", "Test Song").unwrap();
        // Draft the song body (what an author would type).
        core.execute(Command::SaveDocument {
            doc: entry.lyrics_rel.clone(),
            raw: "\
# Test Song

## Verse 1
[Dm]rain on the wire

## Chorus
hold on to the light
"
            .into(),
        })
        .unwrap();

        let mut app = App::new(&core, Arc::new(MockHost::new()));
        app.open_doc(&mut core, entry.lyrics_rel.clone());
        assert!(app.song.is_some(), "lyrics.md wires up the song context");

        // Put the cursor on the first lyric line ("[Dm]rain on the wire").
        let rain = app.editor.text().find("[Dm]").unwrap();
        app.editor.goto_char(rain);
        app.refresh_song_metrics();
        let m = app.song_metrics.as_ref().expect("metrics on open");
        assert_eq!(m.section.as_deref(), Some("Verse 1"));
        assert!(m.language_supported);
        assert_eq!(
            m.line_syllables,
            Some(5),
            "rain-on-the-wire, chords ignored"
        );

        // Alt-] jumps forward; Alt-[ jumps back. Chords never trigger it.
        let marks = section_marks(&app.editor.text());
        let chorus = marks[1].char_offset;
        app.on_key(&mut core, alt(']')).unwrap();
        assert_eq!(app.editor.cursor(), chorus, "Alt-] reached the chorus");
        assert!(
            app.status.contains("Chorus"),
            "status names the section: {}",
            app.status
        );
        let m = app.song_metrics.as_ref().unwrap();
        assert_eq!(m.section.as_deref(), Some("Chorus"));
        assert_eq!(m.section_syllables, Some(5)); // hold-on-to-the-light
        // Step onto the lyric line itself.
        app.on_key(&mut core, key(KeyCode::Down)).unwrap();
        let m = app.song_metrics.as_ref().unwrap();
        assert_eq!(m.line_syllables, Some(5));
        // Alt-[ from the body lands on this section's heading, then the
        // previous section on the next press.
        app.on_key(&mut core, alt('[')).unwrap();
        assert_eq!(app.editor.cursor(), chorus, "Alt-[ first hits the heading");
        app.on_key(&mut core, alt('[')).unwrap();
        assert_eq!(
            app.editor.cursor(),
            section_marks(&app.editor.text())[0].char_offset,
            "the next Alt-[ returns to Verse 1"
        );

        // Plain '[' / ']' type — chords like `[Am]` must stay authorable.
        // Step onto the lyric line so the chord lands in the body.
        app.on_key(&mut core, key(KeyCode::Down)).unwrap();
        let before = app.editor.text().len();
        app.on_key(&mut core, key(KeyCode::Char('['))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('A'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('m'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char(']'))).unwrap();
        assert!(
            app.editor.text().len() > before,
            "brackets typed, not eaten"
        );
        // Metrics survive the edit and still count the chord-free line.
        let m = app.song_metrics.as_ref().unwrap();
        assert_eq!(m.line_syllables, Some(5), "chords are not syllables");
    }

    /// Phase 3: non-lyrics documents get no song context (no metrics, and
    /// Alt-] stays out of the way).
    #[test]
    fn chapters_have_no_song_context() {
        let tmp = TempDir::new().unwrap();
        let (mut core, _rel) = project_with_chapter(&tmp);
        let mut app = App::new(&core, Arc::new(MockHost::new()));
        app.on_key(&mut core, key(KeyCode::Enter)).unwrap();
        assert!(app.song.is_none());
        assert!(app.song_metrics.is_none());
        let before = app.editor.cursor();
        app.on_key(&mut core, alt(']')).unwrap();
        assert_eq!(app.editor.cursor(), before, "Alt-] is inert in chapters");
    }

    /// Phase 4: from a chapter, the audio panel records → pause → resume →
    /// saves into `audio/`, attaches to the chapter by id, plays back, and
    /// closes — every rule enforced by `oca-audio` (rule 8).
    #[test]
    fn audio_panel_records_saves_and_attaches_to_the_open_chapter() {
        let tmp = TempDir::new().unwrap();
        let (mut core, rel) = project_with_chapter(&tmp);
        let host = Arc::new(MockHost::with_chunks(
            oca_audio::AudioSpec::mono(8_000),
            vec![vec![0.1; 40], vec![0.2; 40]],
        ));
        let mut app = App::new(&core, host.clone());
        app.open_doc(&mut core, rel.clone());

        // Tree → A opens the panel in the chapter's context.
        app.focus = Focus::Tree;
        app.on_key(&mut core, key(KeyCode::Char('a'))).unwrap();
        assert_eq!(app.focus, Focus::Audio);
        {
            let panel = app.audio_panel.as_ref().unwrap();
            assert!(matches!(panel.ctx, AttachContext::Chapter(_)));
            assert_eq!(panel.dir_rel, PathBuf::from("audio"));
            assert!(panel.takes.is_empty());
        }

        // record → pause → resume → save
        app.on_key(&mut core, key(KeyCode::Char('r'))).unwrap();
        assert!(app.audio_panel.as_ref().unwrap().rec.is_some());
        app.on_key(&mut core, key(KeyCode::Char('p'))).unwrap();
        assert_eq!(
            app.audio_panel
                .as_ref()
                .unwrap()
                .rec
                .as_ref()
                .unwrap()
                .state(),
            RecState::Paused
        );
        app.on_key(&mut core, key(KeyCode::Char('p'))).unwrap();
        assert_eq!(
            app.audio_panel
                .as_ref()
                .unwrap()
                .rec
                .as_ref()
                .unwrap()
                .state(),
            RecState::Recording
        );
        app.on_key(&mut core, key(KeyCode::Char('s'))).unwrap();
        {
            let panel = app.audio_panel.as_ref().unwrap();
            assert!(panel.rec.is_none(), "s stop clears the recorder");
            assert_eq!(panel.takes.len(), 1);
            assert_eq!(panel.takes[0].wav_rel, PathBuf::from("audio/take-001.wav"));
            assert_eq!(panel.takes[0].meta.duration_ms, 10, "80 frames @ 8 kHz");
        }
        assert!(tmp.path().join("audio/take-001.wav").is_file());

        // Attach: the sidecar records the chapter's immutable id.
        app.on_key(&mut core, key(KeyCode::Char('a'))).unwrap();
        let doc = core.read_document(&rel).unwrap();
        let id = doc.frontmatter.get_str("id").unwrap();
        let sidecar = std::fs::read_to_string(tmp.path().join("audio/take-001.yaml")).unwrap();
        assert!(sidecar.contains("kind: chapter"), "{sidecar}");
        assert!(sidecar.contains(&format!("id: {id}")), "{sidecar}");

        // Enter plays the selected take through the host.
        app.on_key(&mut core, key(KeyCode::Enter)).unwrap();
        assert_eq!(host.played().len(), 1, "playback got the take's frames");

        // Esc returns to the tree.
        app.on_key(&mut core, key(KeyCode::Esc)).unwrap();
        assert_eq!(app.focus, Focus::Tree);
        assert!(app.audio_panel.is_none());
    }

    /// A recording is never lost to a closed panel: Esc while recording
    /// stops, saves, and closes.
    #[test]
    fn closing_the_panel_while_recording_still_saves_the_take() {
        let tmp = TempDir::new().unwrap();
        let (mut core, _rel) = project_with_chapter(&tmp);
        let mut app = App::new(&core, Arc::new(MockHost::new()));

        app.on_key(&mut core, key(KeyCode::Char('a'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('r'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Esc)).unwrap();

        assert_eq!(app.focus, Focus::Tree);
        assert!(app.audio_panel.is_none());
        assert!(
            tmp.path().join("audio/take-001.wav").is_file(),
            "the recording was saved before closing"
        );
        assert!(
            app.status.contains("saved"),
            "status reports the save: {}",
            app.status
        );
    }

    /// With a song's lyrics open, takes land in the song's `demos/` and
    /// attach to the song's id.
    #[test]
    fn audio_panel_saves_song_demos_into_the_song_and_attaches_by_id() {
        let tmp = TempDir::new().unwrap();
        let (mut core, _rel) = project_with_chapter(&tmp);
        let entry = oca_music::Music::new_song(&mut core, "test-song", "Test Song").unwrap();
        let host = Arc::new(MockHost::with_chunks(
            oca_audio::AudioSpec::mono(8_000),
            vec![vec![0.3; 80]],
        ));
        let mut app = App::new(&core, host.clone());
        app.open_doc(&mut core, entry.lyrics_rel.clone());

        app.focus = Focus::Tree;
        app.on_key(&mut core, key(KeyCode::Char('a'))).unwrap();
        {
            let panel = app.audio_panel.as_ref().unwrap();
            assert!(matches!(panel.ctx, AttachContext::Song(_)));
            assert_eq!(panel.dir_rel, PathBuf::from("music/songs/test-song/demos"));
        }

        app.on_key(&mut core, key(KeyCode::Char('r'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('s'))).unwrap();
        let wav = tmp.path().join("music/songs/test-song/demos/take-001.wav");
        assert!(wav.is_file(), "demo saved inside the song directory");

        app.on_key(&mut core, key(KeyCode::Char('a'))).unwrap();
        let song_yaml =
            std::fs::read_to_string(tmp.path().join("music/songs/test-song/song.yaml")).unwrap();
        let song_id: String = song_yaml
            .lines()
            .find_map(|l| l.strip_prefix("id:"))
            .unwrap()
            .trim()
            .to_string();
        let sidecar =
            std::fs::read_to_string(tmp.path().join("music/songs/test-song/demos/take-001.yaml"))
                .unwrap();
        assert!(sidecar.contains("kind: song"), "{sidecar}");
        assert!(sidecar.contains(&format!("id: {song_id}")), "{sidecar}");
        assert!(
            app.status.contains("song"),
            "status shows the target: {}",
            app.status
        );
    }

    /// The panel renders over the workspace with its take list.
    #[test]
    fn audio_panel_draws() {
        let tmp = TempDir::new().unwrap();
        let (mut core, _rel) = project_with_chapter(&tmp);
        let mut app = App::new(&core, Arc::new(MockHost::new()));
        app.on_key(&mut core, key(KeyCode::Char('a'))).unwrap();

        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| app.draw(&core, frame))
            .expect("frame renders");
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("Audio"), "panel title renders");
        assert!(text.contains("no takes yet"), "empty state renders");
    }

    // ---- dictation (Phase 7B) ---------------------------------------------

    /// Tick until the audio panel reaches a condition (or fail loudly).
    fn settle_audio(app: &mut App, core: &mut Core, ok: impl Fn(&AudioPanel) -> bool) {
        for _ in 0..1_000 {
            app.tick(core).unwrap();
            if let Some(panel) = &app.audio_panel
                && ok(panel)
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("audio panel never reached the expected state");
    }

    fn dict_app(core: &Core, text: &str) -> App {
        let mut app = App::new(
            core,
            Arc::new(MockHost::with_chunks(
                oca_audio::AudioSpec::mono(8_000),
                vec![vec![0.1; 40]],
            )),
        );
        let text = text.to_string();
        app.attach_transcriber(Arc::new(oca_audio::FnTranscriber::new(
            "stand-in",
            move |_wav: &std::path::Path, _c: &std::sync::atomic::AtomicBool| Ok(text.clone()),
        )));
        app
    }

    /// The Phase 7B headline: record → transcribe → insert works with a
    /// stand-in runtime (no LLM anywhere), the transcript is stored with
    /// the take, and **nothing enters the manuscript until `i` + Ctrl-S**.
    #[test]
    fn dictation_records_transcribes_and_inserts_only_when_asked() {
        let tmp = TempDir::new().unwrap();
        let (mut core, rel) = project_with_chapter(&tmp);
        let mut app = dict_app(&core, "the lamp was lit");
        app.open_doc(&mut core, rel.clone());
        let editor_before = app.editor.text();
        let body_before = core.read_document(&rel).unwrap().body().to_string();

        // record → save → transcribe
        app.focus = Focus::Tree;
        app.on_key(&mut core, key(KeyCode::Char('a'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('r'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('s'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('t'))).unwrap();
        assert!(
            app.status.contains("transcribing with stand-in"),
            "{}",
            app.status
        );
        settle_audio(&mut app, &mut core, |p| {
            p.dict_job.is_none() && p.transcript.is_some()
        });
        assert_eq!(
            app.audio_panel.as_ref().unwrap().transcript.as_deref(),
            Some("the lamp was lit")
        );

        // The raw transcript is stored with the take — and *nowhere else*:
        // manuscript untouched, editor untouched, no notes file created.
        assert_eq!(
            core.read_document(&rel).unwrap().body(),
            body_before,
            "a transcript never auto-inserts"
        );
        assert_eq!(app.editor.text(), editor_before);
        assert!(
            !tmp.path().join("notes/dictation.md").exists(),
            "no notes by itself"
        );
        let sidecar = std::fs::read_to_string(tmp.path().join("audio/take-001.yaml")).unwrap();
        assert!(sidecar.contains("transcript:"), "{sidecar}");
        assert!(sidecar.contains("the lamp was lit"), "{sidecar}");

        // The panel shows the transcript with its review-only framing.
        {
            let backend = TestBackend::new(100, 34);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| app.draw(&core, frame))
                .expect("frame renders");
            let buffer = terminal.backend().buffer();
            let drawn: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
            assert!(drawn.contains("transcript — stored"), "{drawn}");
            assert!(drawn.contains("i insert"), "{drawn}");
        }

        // `i` inserts into the editor buffer only; the file changes only
        // when the author saves.
        app.on_key(&mut core, key(KeyCode::Char('i'))).unwrap();
        assert!(app.dirty, "insert marks the editor dirty");
        let with_text = app.editor.text();
        assert!(with_text.contains("the lamp was lit"), "{with_text}");
        assert!(with_text.contains("# First"), "heading survives");
        assert_eq!(
            core.read_document(&rel).unwrap().body(),
            body_before,
            "still not written anywhere"
        );
        app.on_key(&mut core, ctrl('s')).unwrap();
        let saved = core.read_document(&rel).unwrap().body().to_string();
        assert!(saved.contains("the lamp was lit"), "{saved}");
        assert!(saved.contains("# First"), "{saved}");

        // Files are canonical: closing and reopening the panel still shows
        // the transcript (it was read from the sidecar, not memory).
        app.on_key(&mut core, key(KeyCode::Esc)).unwrap();
        assert_eq!(app.focus, Focus::Tree);
        app.on_key(&mut core, key(KeyCode::Char('a'))).unwrap();
        assert_eq!(
            app.audio_panel.as_ref().unwrap().transcript.as_deref(),
            Some("the lamp was lit")
        );
    }

    /// Rule 3: without a transcription runtime the panel still works and
    /// `t` explains exactly what's missing instead of failing.
    #[test]
    fn t_without_a_transcriber_explains_itself() {
        let tmp = TempDir::new().unwrap();
        let (mut core, _rel) = project_with_chapter(&tmp);
        let mut app = App::new(&core, Arc::new(MockHost::new()));

        app.on_key(&mut core, key(KeyCode::Char('a'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('r'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('s'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('t'))).unwrap();
        assert!(
            app.status.contains("no dictation runtime"),
            "actionable: {}",
            app.status
        );
        let panel = app.audio_panel.as_ref().unwrap();
        assert!(panel.transcript.is_none());
        assert!(panel.dict_job.is_none(), "nothing was started");
        assert!(
            !std::fs::read_to_string(tmp.path().join("audio/take-001.yaml"))
                .unwrap()
                .contains("transcript:"),
            "nothing stored"
        );
    }

    /// An LLM cleanup is a *preview*: it streams into the panel for review
    /// and touches nothing until the author inserts it (rule 4's stance
    /// until proposals arrive in Phase 8).
    #[test]
    fn cleanup_streams_a_preview_that_only_inserts_when_asked() {
        let tmp = TempDir::new().unwrap();
        let (mut core, rel) = project_with_chapter(&tmp);
        let mut app = dict_app(&core, "the lamp was lit");
        let (_model, ai) = mock_ai(0, &["the lamp ", "was glowing"]);
        app.attach_ai(ai);
        app.open_doc(&mut core, rel.clone());
        let body_before = core.read_document(&rel).unwrap().body().to_string();

        app.focus = Focus::Tree;
        app.on_key(&mut core, key(KeyCode::Char('a'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('r'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('s'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('t'))).unwrap();
        settle_audio(&mut app, &mut core, |p| {
            p.dict_job.is_none() && p.transcript.is_some()
        });

        // c starts the preview stream.
        app.on_key(&mut core, key(KeyCode::Char('c'))).unwrap();
        assert!(
            app.status.contains("cleanup preview running"),
            "{}",
            app.status
        );
        settle_audio(&mut app, &mut core, |p| {
            p.dict_job.is_none() && p.preview.as_deref() == Some("the lamp was glowing")
        });
        // Preview over raw transcript for insert purposes; nothing written.
        assert_eq!(
            core.read_document(&rel).unwrap().body(),
            body_before,
            "a preview never writes by itself"
        );
        assert_eq!(app.editor.text(), core.read_document(&rel).unwrap().body());

        // i inserts the *reviewed* text (the preview), not the raw one.
        app.on_key(&mut core, key(KeyCode::Char('i'))).unwrap();
        let edited = app.editor.text();
        assert!(edited.contains("the lamp was glowing"), "{edited}");
        assert!(
            !edited.contains("the lamp was lit"),
            "raw not used: {edited}"
        );
        assert_eq!(
            core.read_document(&rel).unwrap().body(),
            body_before,
            "file untouched until Ctrl-S"
        );
    }

    /// The LLM side is optional too: `c` without a model says so, and the
    /// transcript stays exactly as it was.
    #[test]
    fn c_without_a_language_model_says_so() {
        let tmp = TempDir::new().unwrap();
        let (mut core, _rel) = project_with_chapter(&tmp);
        let mut app = dict_app(&core, "the lamp was lit");

        app.on_key(&mut core, key(KeyCode::Char('a'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('r'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('s'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('t'))).unwrap();
        settle_audio(&mut app, &mut core, |p| {
            p.dict_job.is_none() && p.transcript.is_some()
        });
        app.on_key(&mut core, key(KeyCode::Char('c'))).unwrap();
        assert!(
            app.status.contains("language model"),
            "optional, not fatal: {}",
            app.status
        );
        let panel = app.audio_panel.as_ref().unwrap();
        assert_eq!(panel.transcript.as_deref(), Some("the lamp was lit"));
        assert!(panel.preview.is_none());
        assert!(panel.dict_job.is_none(), "no job started");
    }

    /// Esc stops a running transcription: nothing is stored, nothing is
    /// shown as if it had succeeded.
    #[test]
    fn first_esc_stops_a_running_transcription() {
        let tmp = TempDir::new().unwrap();
        let (mut core, _rel) = project_with_chapter(&tmp);
        let mut app = App::new(
            &core,
            Arc::new(MockHost::with_chunks(
                oca_audio::AudioSpec::mono(8_000),
                vec![vec![0.1; 40]],
            )),
        );
        app.attach_transcriber(Arc::new(oca_audio::FnTranscriber::new(
            "slow",
            |_wav: &std::path::Path, cancel: &std::sync::atomic::AtomicBool| {
                for _ in 0..500 {
                    if cancel.load(std::sync::atomic::Ordering::SeqCst) {
                        return Err(anyhow::anyhow!("stopped"));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Ok("late text".to_string())
            },
        )));

        app.on_key(&mut core, key(KeyCode::Char('a'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('r'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('s'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('t'))).unwrap();
        assert!(app.audio_panel.as_ref().unwrap().dict_job.is_some());

        app.on_key(&mut core, key(KeyCode::Esc)).unwrap();
        let panel = app.audio_panel.as_ref().unwrap();
        assert!(panel.dict_job.is_none(), "the job was cancelled");
        assert_eq!(app.focus, Focus::Audio, "first Esc keeps the panel open");
        assert!(app.status.contains("stopped"), "{}", app.status);

        // Let a few ticks pass: nothing arrives, nothing is stored.
        for _ in 0..10 {
            app.tick(&mut core).unwrap();
        }
        let panel = app.audio_panel.as_ref().unwrap();
        assert!(panel.transcript.is_none(), "no late transcript appears");
        let sidecar = std::fs::read_to_string(tmp.path().join("audio/take-001.yaml")).unwrap();
        assert!(!sidecar.contains("transcript:"), "{sidecar}");

        // Second Esc closes as usual.
        app.on_key(&mut core, key(KeyCode::Esc)).unwrap();
        assert_eq!(app.focus, Focus::Tree);
        assert!(app.audio_panel.is_none());
    }

    // ---- search panel (Phase 5) -------------------------------------------

    #[test]
    fn ctrl_f_opens_search_and_enter_opens_the_hit() {
        let tmp = TempDir::new().unwrap();
        let (mut core, rel) = project_with_chapter(&tmp);
        let mut app = App::new(&core, Arc::new(MockHost::new()));

        app.on_key(&mut core, ctrl('f')).unwrap();
        assert_eq!(app.focus, Focus::Search);
        let panel = app.search_panel.as_ref().expect("panel open");
        assert!(panel.query.is_empty() && panel.hits.is_empty());

        for c in "first".chars() {
            app.on_key(&mut core, key(KeyCode::Char(c))).unwrap();
        }
        {
            let panel = app.search_panel.as_ref().unwrap();
            assert_eq!(panel.query, "first");
            assert!(!panel.hits.is_empty(), "the chapter is found");
            assert_eq!(panel.hits[0].rel, rel);
        }

        app.on_key(&mut core, key(KeyCode::Enter)).unwrap();
        assert_eq!(app.open_rel.as_ref(), Some(&rel), "hit opens the doc");
        assert_eq!(app.focus, Focus::Editor);
        assert!(app.search_panel.is_none(), "panel closes on open");
    }

    #[test]
    fn search_esc_closes_back_to_the_tree() {
        let tmp = TempDir::new().unwrap();
        let (mut core, _rel) = project_with_chapter(&tmp);
        let mut app = App::new(&core, Arc::new(MockHost::new()));

        app.on_key(&mut core, ctrl('f')).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('z'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Backspace)).unwrap();
        assert!(
            app.search_panel.as_ref().unwrap().query.is_empty(),
            "backspace edits the query"
        );

        app.on_key(&mut core, key(KeyCode::Esc)).unwrap();
        assert_eq!(app.focus, Focus::Tree);
        assert!(app.search_panel.is_none(), "Esc drops the panel");
    }

    #[test]
    fn search_data_hits_say_so_and_keep_the_panel() {
        let tmp = TempDir::new().unwrap();
        let (mut core, _rel) = project_with_chapter(&tmp);
        // An audio take sidecar: indexed, but not a tree document.
        let audio = tmp.path().join("audio");
        std::fs::create_dir_all(&audio).unwrap();
        std::fs::write(
            audio.join("memo.yaml"),
            "take_id: take_1\ntitle: Voice memo\n",
        )
        .unwrap();
        core.reconcile().unwrap();

        let mut app = App::new(&core, Arc::new(MockHost::new()));
        app.on_key(&mut core, ctrl('f')).unwrap();
        for c in "memo".chars() {
            app.on_key(&mut core, key(KeyCode::Char(c))).unwrap();
        }
        assert!(
            !app.search_panel.as_ref().unwrap().hits.is_empty(),
            "the take is indexed"
        );

        app.on_key(&mut core, key(KeyCode::Enter)).unwrap();
        assert_eq!(app.focus, Focus::Search, "data hit does not open an editor");
        assert!(app.search_panel.is_some(), "panel stays open");
        assert!(app.status.contains("data"), "said so: {}", app.status);
    }

    // ---- snapshot panel (Phase 6) ------------------------------------------

    #[test]
    fn v_opens_snapshots_c_creates_and_enter_diffs() {
        let tmp = TempDir::new().unwrap();
        let (mut core, rel) = project_with_chapter(&tmp);
        let mut app = App::new(&core, Arc::new(MockHost::new()));

        app.on_key(&mut core, key(KeyCode::Char('v'))).unwrap();
        assert_eq!(app.focus, Focus::Snapshots);
        assert!(
            app.snap_panel.as_ref().unwrap().snapshots.is_empty(),
            "nothing saved yet"
        );

        // `c` saves a snapshot and it shows up in the list.
        app.on_key(&mut core, key(KeyCode::Char('c'))).unwrap();
        {
            let panel = app.snap_panel.as_ref().unwrap();
            assert_eq!(panel.snapshots.len(), 1);
            assert_eq!(panel.sel, 0);
        }
        assert!(app.status.contains("Snapshot #1"), "{}", app.status);

        // Break the chapter, then Enter loads the inline diff.
        core.save_body(&rel, "rewritten\n").unwrap();
        app.on_key(&mut core, key(KeyCode::Enter)).unwrap();
        {
            let panel = app.snap_panel.as_ref().unwrap();
            match &panel.mode {
                SnapMode::Diff { lines, title, .. } => {
                    assert!(title.contains("#1"), "{title}");
                    let text = lines.join("\n");
                    assert!(text.contains("-# First"), "{text}");
                    assert!(text.contains("+rewritten"), "{text}");
                }
                SnapMode::List => panic!("expected the diff view"),
            }
        }

        // Enter again (or Esc) returns to the list; Esc closes.
        app.on_key(&mut core, key(KeyCode::Enter)).unwrap();
        assert!(matches!(
            app.snap_panel.as_ref().unwrap().mode,
            SnapMode::List
        ));
        app.on_key(&mut core, key(KeyCode::Esc)).unwrap();
        assert_eq!(app.focus, Focus::Tree);
        assert!(app.snap_panel.is_none());
    }

    #[test]
    fn snapshot_restore_from_panel_round_trips() {
        let tmp = TempDir::new().unwrap();
        let (mut core, rel) = project_with_chapter(&tmp);
        core.save_body(&rel, "good text\n").unwrap();
        let mut app = App::new(&core, Arc::new(MockHost::new()));

        app.on_key(&mut core, key(KeyCode::Char('v'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('c'))).unwrap(); // snapshot #1
        core.save_body(&rel, "oops\n").unwrap();

        app.on_key(&mut core, key(KeyCode::Char('r'))).unwrap();
        assert!(
            app.status.contains("restored Snapshot #1"),
            "{}",
            app.status
        );
        assert_eq!(core.read_document(&rel).unwrap().body(), "good text\n");
        // The auto-snapshot before restore is now the newest entry.
        let panel = app.snap_panel.as_ref().unwrap();
        assert_eq!(panel.snapshots.len(), 2, "{:?}", panel.snapshots);
    }

    #[test]
    fn search_panel_draws() {
        let tmp = TempDir::new().unwrap();
        let (mut core, _rel) = project_with_chapter(&tmp);
        let mut app = App::new(&core, Arc::new(MockHost::new()));
        app.on_key(&mut core, ctrl('f')).unwrap();
        for c in "first".chars() {
            app.on_key(&mut core, key(KeyCode::Char(c))).unwrap();
        }

        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| app.draw(&core, frame))
            .expect("frame renders");
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("Search"), "panel title renders");
        assert!(text.contains("chapter"), "hit kind renders");
    }

    // ---- AI panel (Phase 7) ----------------------------------------------

    fn mock_ai(delay_ms: u64, tokens: &[&str]) -> (Arc<oca_ai::MockModel>, Ai) {
        let model = Arc::new(
            oca_ai::MockModel::new()
                .with_tokens(tokens.iter().map(|t| t.to_string()).collect())
                .with_delay(delay_ms),
        );
        let ai = Ai::new(model.clone(), Vec::new());
        (model, ai)
    }

    /// Tick until the panel reaches a condition (or fail loudly).
    fn settle(app: &mut App, core: &mut Core, ok: impl Fn(&AiPanel) -> bool) {
        for _ in 0..1_000 {
            app.tick(core).unwrap();
            if let Some(panel) = &app.ai_panel
                && ok(panel)
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("AI panel never reached the expected state");
    }

    #[test]
    fn i_opens_ai_panel_and_no_runtime_degrades_gracefully() {
        let tmp = TempDir::new().unwrap();
        let (mut core, _rel) = project_with_chapter(&tmp);
        let mut app = App::new(&core, Arc::new(MockHost::new()));

        app.on_key(&mut core, key(KeyCode::Char('i'))).unwrap();
        assert_eq!(app.focus, Focus::Ai);
        let panel = app.ai_panel.as_ref().expect("panel open");
        assert_eq!(panel.state, AiState::Idle);

        // No runtime attached: loading explains itself instead of crashing.
        app.on_key(&mut core, key(KeyCode::Char('l'))).unwrap();
        let panel = app.ai_panel.as_ref().unwrap();
        assert!(
            panel
                .error
                .as_deref()
                .is_some_and(|e| e.contains("no AI runtime")),
            "{:?}",
            panel.error
        );
        assert_eq!(panel.state, AiState::Idle);

        app.on_key(&mut core, key(KeyCode::Esc)).unwrap();
        assert_eq!(app.focus, Focus::Tree);
        assert!(app.ai_panel.is_none());
    }

    #[test]
    fn load_probe_stop_and_unload_with_the_mock() {
        let tmp = TempDir::new().unwrap();
        let (mut core, _rel) = project_with_chapter(&tmp);
        let (model, ai) = mock_ai(5, &["Once", " upon", " a", " time", "."]);
        let mut app = App::new(&core, Arc::new(MockHost::new()));
        app.attach_ai(ai);

        // Load runs as a job; tick retires it.
        app.on_key(&mut core, key(KeyCode::Char('i'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('l'))).unwrap();
        assert_eq!(app.ai_panel.as_ref().unwrap().state, AiState::Loading);
        settle(&mut app, &mut core, |p| p.state == AiState::Ready);
        assert!(model.is_loaded());

        // Probe streams tokens into the panel on ticks.
        app.on_key(&mut core, key(KeyCode::Char('t'))).unwrap();
        assert_eq!(app.ai_panel.as_ref().unwrap().state, AiState::Streaming);
        settle(&mut app, &mut core, |p| p.stream_tokens >= 2);
        {
            let panel = app.ai_panel.as_ref().unwrap();
            assert!(
                panel.stream_text.starts_with("Once upon"),
                "{}",
                panel.stream_text
            );
        }

        // [ESC] Stop: interrupt, stay loaded, say so.
        app.on_key(&mut core, key(KeyCode::Esc)).unwrap();
        assert_eq!(app.focus, Focus::Tree);
        assert!(app.status.contains("generation stopped"), "{}", app.status);
        assert!(model.is_loaded(), "stop must not unload the model");

        // Unload releases residency.
        app.on_key(&mut core, key(KeyCode::Char('i'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('u'))).unwrap();
        assert_eq!(app.ai_panel.as_ref().unwrap().state, AiState::Idle);
        assert!(!model.is_loaded());
    }

    #[test]
    fn probe_crash_is_reported_and_the_app_survives() {
        let tmp = TempDir::new().unwrap();
        let (mut core, _rel) = project_with_chapter(&tmp);
        let model =
            Arc::new(oca_ai::MockModel::new().fails_generate("model runtime crashed (simulated)"));
        let mut app = App::new(&core, Arc::new(MockHost::new()));
        app.attach_ai(Ai::new(model, Vec::new()));

        app.on_key(&mut core, key(KeyCode::Char('i'))).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('l'))).unwrap();
        settle(&mut app, &mut core, |p| p.state == AiState::Ready);

        app.on_key(&mut core, key(KeyCode::Char('t'))).unwrap();
        settle(&mut app, &mut core, |p| p.error.is_some());
        let panel = app.ai_panel.as_ref().unwrap();
        assert_eq!(
            panel.state,
            AiState::Ready,
            "model stays loaded after a failed probe"
        );
        assert!(
            panel
                .error
                .as_deref()
                .is_some_and(|e| e.contains("crashed")),
            "{:?}",
            panel.error
        );
        // The app is still fully functional afterwards.
        app.on_key(&mut core, key(KeyCode::Esc)).unwrap();
        app.on_key(&mut core, key(KeyCode::Char('v'))).unwrap();
        assert_eq!(app.focus, Focus::Snapshots);
    }

    #[test]
    fn ai_panel_draws_runtime_info_and_streaming_progress() {
        let tmp = TempDir::new().unwrap();
        let (mut core, _rel) = project_with_chapter(&tmp);
        let (_model, ai) = mock_ai(30, &["a", "b", "c"]);
        let mut app = App::new(&core, Arc::new(MockHost::new()));
        app.attach_ai(ai);
        app.on_key(&mut core, key(KeyCode::Char('i'))).unwrap();

        let render_text = |app: &mut App, core: &Core| -> String {
            let backend = TestBackend::new(100, 32);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| app.draw(core, frame))
                .expect("frame renders");
            let buffer = terminal.backend().buffer();
            buffer.content().iter().map(|cell| cell.symbol()).collect()
        };

        let text = render_text(&mut app, &core);
        assert!(text.contains("AI Model"), "panel title renders");
        assert!(text.contains("Context"), "model info renders");
        assert!(text.contains("[Load]"), "idle state shows Load");
        assert!(text.contains("one heavy model at a time"), "policy renders");

        // Load + probe, then the streaming block renders.
        app.on_key(&mut core, key(KeyCode::Char('l'))).unwrap();
        settle(&mut app, &mut core, |p| p.state == AiState::Ready);
        app.on_key(&mut core, key(KeyCode::Char('t'))).unwrap();
        let text = render_text(&mut app, &core);
        assert!(text.contains("Generating"), "streaming state renders");
        assert!(text.contains("[ESC] Stop"), "stop hint renders");
    }

    #[test]
    fn no_runtime_panel_explains_how_to_enable_ai() {
        let tmp = TempDir::new().unwrap();
        let (mut core, _rel) = project_with_chapter(&tmp);
        let mut app = App::new(&core, Arc::new(MockHost::new()));
        app.on_key(&mut core, key(KeyCode::Char('i'))).unwrap();

        let backend = TestBackend::new(100, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| app.draw(&core, frame))
            .expect("frame renders");
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("no AI runtime"), "empty state renders");
        assert!(text.contains("optional"), "rule 3 messaging renders");
        assert!(text.contains("oca runtime install"), "hint renders");
    }
}
