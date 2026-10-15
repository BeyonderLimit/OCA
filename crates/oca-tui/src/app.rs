//! TUI application state: project tree + rope editor + status bar.
//! Reads through Core queries, writes through Core commands — nothing else.

use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
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
    last_journal: Instant,
    quit: bool,
    /// Last drawn inner editor rect (mouse mapping); sane default pre-draw.
    editor_area: Rect,
    tree_area: Rect,
}

impl App {
    pub fn new(core: &Core) -> Self {
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
        Ok(())
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
                " ↑↓ select · Enter open · S state · Tab editor · Ctrl-N chapter · Ctrl-Q quit"
            }
            Focus::Form => " Tab field · Enter apply · Esc cancel",
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
        let mut app = App::new(&core);

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
        let mut app = App::new(&core);
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
        let mut app = App::new(&core);
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
        let mut app = App::new(&core);
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
        let mut app = App::new(&core);
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

        let mut app = App::new(&core);
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
        let mut app = App::new(&core);
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
        let mut app = App::new(&core);
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
        let mut app = App::new(&core);
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
        let mut app = App::new(&core);
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
        let mut app = App::new(&core);
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
        let mut app = App::new(&core);

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
        let mut app = App::new(&core);

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
        let mut app = App::new(&core);
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

        let mut app = App::new(&core);
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
        let mut app = App::new(&core);
        app.on_key(&mut core, key(KeyCode::Enter)).unwrap();
        assert!(app.song.is_none());
        assert!(app.song_metrics.is_none());
        let before = app.editor.cursor();
        app.on_key(&mut core, alt(']')).unwrap();
        assert_eq!(app.editor.cursor(), before, "Alt-] is inert in chapters");
    }
}
