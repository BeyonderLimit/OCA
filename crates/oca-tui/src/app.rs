//! TUI application state: project tree + rope editor + status bar.
//! Reads through Core queries, writes through Core commands — nothing else.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use oca_audio::{Audio, AudioHost, Player, RecState, Recorder, Take, default_take_dir};
use oca_core::{Command, Core, RecoveryEntry};
use oca_model::DocumentKind;
use oca_music::{CursorMetrics, EnglishPhonetics, Music, cursor_metrics, section_marks};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

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
}

enum Banner {
    /// A journaled draft differs from the file on disk.
    Recovery { entry: RecoveryEntry },
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

    pub fn tick(&mut self, core: &mut Core) -> Result<()> {
        self.journal_now(core, false);
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
        self.status = format!(
            "audio — new takes go to {} · attach to {}",
            panel.dir_rel.display(),
            panel.ctx.describe()
        );
        self.audio_panel = Some(panel);
        self.focus = Focus::Audio;
    }

    fn audio_key(&mut self, core: &Core, key: KeyEvent) {
        let Some(mut panel) = self.audio_panel.take() else {
            return;
        };
        let host = Arc::clone(&self.audio_host);
        let audio = match Audio::new(core, host) {
            Ok(audio) => audio,
            Err(e) => {
                self.status = format!("audio: {e}");
                self.audio_panel = Some(panel);
                return;
            }
        };
        match key.code {
            KeyCode::Esc => {
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
                    panel.sel += 1;
                }
            }
            KeyCode::Up => {
                panel.sel = panel.sel.saturating_sub(1);
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
        let area = centered(frame.area(), 78, 18);
        let rec_badge = match &panel.rec {
            Some(rec) => match rec.state() {
                RecState::Recording => "  ● RECORDING".to_string(),
                RecState::Paused => "  ● PAUSED".to_string(),
            },
            None => String::new(),
        };
        let playing = if panel.player.is_some() { "  ♪" } else { "" };
        let block = Block::default().borders(Borders::ALL).title(format!(
            " Audio — {}{rec_badge}{playing} ",
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
            let line = Line::from(format!(
                "{marker}{:<22} {:>5.1}s  {}{note}",
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
        lines.push(Line::styled(
            "↑↓ select · Enter play · a attach · d delete · r record · p pause · s save · Esc close",
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
                " Tab/ Esc tree · Alt-]/Alt-[ section · Ctrl-S save · Ctrl-Q quit"
            }
            Focus::Editor => " Tab/ Esc tree · Ctrl-S save · Ctrl-N chapter · Ctrl-Q quit",
            Focus::Tree => {
                " ↑↓ select · Enter open · S state · A audio · Tab editor · Ctrl-N chapter · Ctrl-Q quit"
            }
            Focus::Form => " Tab field · Enter apply · Esc cancel",
            Focus::Audio => {
                " ↑↓ select · Enter play · a attach · d delete · r record · p pause · s save · Esc close"
            }
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
}
