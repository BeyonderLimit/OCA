//! Rope-based editor widget (A9 spike result — see `docs/adr/0021`).
//!
//! Why not `tui-textarea`: it has **no soft wrap** (verified against 0.7.0
//! sources) — a hard fail for prose. This editor implements the A9 checklist:
//! soft wrap with display width, grapheme-correct movement, selection,
//! clipboard (internal + OSC52-ready), undo/redo with burst coalescing,
//! mouse, and ropey-backed buffers that stay smooth at 50k words.

use std::ops::Range;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use ropey::Rope;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const UNDO_LIMIT: usize = 200;
const COALESCE: Duration = Duration::from_millis(800);

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Insert,
    Delete,
    Newline,
}

struct Snapshot {
    text: Rope,
    cursor: usize,
    anchor: Option<usize>,
}

/// One display line after wrapping: a char range of one logical line
/// (the line terminator is never part of a segment).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seg {
    pub start: usize,
    pub end: usize,
    pub line: usize,
}

/// Wrapped view of the whole buffer for a given width (memoized per version).
pub struct WrapMap {
    segs: Vec<Seg>,
}

impl WrapMap {
    pub fn build(rope: &Rope, width: usize) -> Self {
        let width = width.max(1);
        let mut segs = Vec::new();
        for li in 0..rope.len_lines() {
            segs_for_line(rope, li, width, &mut segs);
        }
        if segs.is_empty() {
            segs.push(Seg {
                start: 0,
                end: 0,
                line: 0,
            });
        }
        Self { segs }
    }

    /// Incrementally rewrap after one edit (see `EditSpan`): rebuild only the
    /// changed line region and shift everything after it arithmetically, so a
    /// keystroke in a 50k-word file never re-wraps the whole document.
    fn splice(&mut self, rope: &Rope, width: usize, span: &EditSpan) {
        let width = width.max(1);
        let from = self.segs.partition_point(|s| s.line < span.first_line);
        // The old line after the edit region merges into the new region, so
        // its segments are dropped too (they are re-derived from `new_last`).
        let last_old = if span.has_trail {
            span.old_last + 1
        } else {
            span.old_last
        };
        let to = self.segs.partition_point(|s| s.line <= last_old);
        let delta = span.ins as isize - span.removed as isize;
        let shift = if span.has_trail {
            span.new_last as isize - span.old_last as isize - 1
        } else {
            span.new_last as isize - span.old_last as isize
        };
        for s in &mut self.segs[to..] {
            s.start = (s.start as isize + delta) as usize;
            s.end = (s.end as isize + delta) as usize;
            s.line = (s.line as isize + shift) as usize;
        }
        let mut fresh = Vec::new();
        for li in span.first_line..=span.new_last {
            segs_for_line(rope, li, width, &mut fresh);
        }
        self.segs.splice(from..to, fresh);
    }

    pub fn len(&self) -> usize {
        self.segs.len()
    }

    pub fn seg(&self, idx: usize) -> Seg {
        self.segs[idx]
    }

    /// Segment index holding `cursor` (a cursor at a wrap boundary belongs to
    /// the next segment, so it renders at column 0).
    fn index_of(&self, cursor: usize) -> usize {
        match self.segs.binary_search_by(|s| {
            if cursor < s.start {
                std::cmp::Ordering::Greater
            } else if cursor >= s.end {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        }) {
            Ok(i) => i,
            Err(_) => self
                .segs
                .iter()
                .position(|s| s.end > cursor)
                .unwrap_or(self.segs.len() - 1),
        }
    }

    /// (segment index, display column) for a char index.
    pub fn locate(&self, rope: &Rope, cursor: usize) -> (usize, usize) {
        let si = self.index_of(cursor);
        let seg = self.segs[si];
        let from = cursor.clamp(seg.start, seg.end);
        (si, seg_width(rope, from, cursor.max(seg.start)))
    }

    /// Char index for (segment index, display column): snaps left to the
    /// nearest grapheme boundary.
    pub fn char_at(&self, rope: &Rope, si: usize, col: usize) -> usize {
        let seg = self.segs[si];
        let text: String = rope.slice(seg.start..seg.end).to_string();
        let mut pos = seg.start;
        let mut w = 0;
        for g in text.graphemes(true) {
            let gw = g.width();
            if w + gw > col {
                break;
            }
            pos += g.chars().count();
            w += gw;
            if w == col {
                break;
            }
        }
        pos
    }
}

/// Wrap one logical line of `rope` into `out` (shared by `build` and the
/// incremental `splice`).
fn segs_for_line(rope: &Rope, li: usize, width: usize, out: &mut Vec<Seg>) {
    let line = rope.line(li);
    let lstart = rope.line_to_char(li);
    let mut content_end = lstart + line.len_chars();
    // Strip the terminator (\n or \r\n) from wrapping purposes.
    if line
        .get_char(line.len_chars().saturating_sub(1))
        .is_some_and(|c| c == '\n')
    {
        content_end -= 1;
        if line
            .get_char(line.len_chars().saturating_sub(2))
            .is_some_and(|c| c == '\r')
        {
            content_end -= 1;
        }
    }
    if content_end <= lstart {
        out.push(Seg {
            start: lstart,
            end: lstart,
            line: li,
        });
        return;
    }
    let mut i = lstart;
    while i < content_end {
        let seg_end = wrap_one(rope, i, content_end, width);
        out.push(Seg {
            start: i,
            end: seg_end,
            line: li,
        });
        i = seg_end;
    }
    // A line whose last segment fills the width exactly gets an empty tail
    // segment so the end-of-line cursor has a place to sit.
    if let Some(last) = out.last()
        && last.end == content_end
        && seg_width(rope, last.start, last.end) == width
    {
        out.push(Seg {
            start: content_end,
            end: content_end,
            line: li,
        });
    }
}

/// Plan for one incremental rewrap: the pre-edit buffer had `range` replaced
/// by `ins` chars. Old lines `[first_line..=old_last]` plus the line after the
/// region (it merges into the new region, so its old segments are dropped) are
/// rebuilt from the post-edit rope; every later segment shifts by
/// `ins - removed` chars and by the change in line count.
struct EditSpan {
    first_line: usize,
    old_last: usize,
    /// Whether an old line exists after the region (`old_last + 1`).
    has_trail: bool,
    /// Char offset where that line started (pre-edit).
    old_trail: usize,
    removed: usize,
    ins: usize,
    /// Last line to rebuild — filled in by `complete` after the edit.
    new_last: usize,
}

impl EditSpan {
    /// Compute against the **pre-edit** buffer.
    fn before(rope: &Rope, range: &Range<usize>, ins: usize) -> Self {
        let at = range.start;
        let removed = range.len();
        let first_line = rope.char_to_line(at);
        let old_last = if removed > 0 {
            rope.char_to_line(at + removed - 1)
        } else {
            first_line
        };
        let has_trail = old_last + 1 < rope.len_lines();
        let old_trail = if has_trail {
            rope.line_to_char(old_last + 1)
        } else {
            0
        };
        Self {
            first_line,
            old_last,
            has_trail,
            old_trail,
            removed,
            ins,
            new_last: old_last,
        }
    }

    /// Finish against the **post-edit** buffer: locate where the first
    /// unchanged trailing char now sits, and with it the last affected line.
    fn complete(&mut self, rope: &Rope) {
        self.new_last = if self.has_trail {
            let delta = self.ins as isize - self.removed as isize;
            rope.char_to_line((self.old_trail as isize + delta) as usize)
        } else {
            rope.len_lines() - 1
        };
    }
}

/// Base materialization window for `wrap_one` (chars): generous enough that
/// ordinary prose wraps in a single window.
const WRAP_WINDOW: usize = 512;

/// Greedy word wrap of `rope[start..end]` → the char index where the segment
/// ends (== where the next one starts).
///
/// Rules: a whitespace that would overflow is kept on the current line
/// (trailing space, clipped by the renderer); otherwise break at the last
/// whitespace (word boundary), or hard at the overflow when the word has none.
///
/// The text is scanned in windows so wrapping a huge logical line (a pasted
/// wall of text) never materializes the whole remaining line per segment —
/// that was O(line²). The window's last grapheme is held back unless the
/// window reaches the segment end, so a grapheme is never split across
/// windows (combining marks, ZWJ sequences).
fn wrap_one(rope: &Rope, start: usize, end: usize, width: usize) -> usize {
    let width = width.max(1);
    let mut w = 0usize;
    let mut rel = 0usize; // chars consumed since `start`
    let mut last_break: Option<usize> = None; // relative: next segment starts here
    let mut from = start;
    let mut window = WRAP_WINDOW.max(width * 4 + 128);
    while from < end {
        let take = end.min(from + window);
        let text: String = rope.slice(from..take).to_string();
        let gs: Vec<&str> = text.graphemes(true).collect();
        let n = gs.len();
        let mut consumed = 0usize;
        let mut progressed = false;
        for (i, g) in gs.iter().enumerate() {
            if i + 1 == n && take < end {
                break; // hold back: it may continue into the next window
            }
            let gw = g.width();
            let glen = g.chars().count();
            if w + gw > width {
                if w == 0 {
                    // A single grapheme wider than the area: place it, then break.
                    return start + rel + glen;
                }
                if g.chars().all(char::is_whitespace) {
                    // Keep the overflowing whitespace on this line.
                    return start + rel + glen;
                }
                let br = last_break.filter(|&b| b > 0 && b <= rel).unwrap_or(rel);
                let br = br.max(1);
                return start + br;
            }
            if g.chars().all(char::is_whitespace) {
                last_break = Some(rel + glen);
            }
            w += gw;
            rel += glen;
            consumed += glen;
            progressed = true;
        }
        if !progressed && take < end {
            window *= 2; // the window is one grapheme: grow until it fits
        }
        from += consumed;
    }
    end
}

fn seg_width(rope: &Rope, from: usize, to: usize) -> usize {
    if to <= from {
        return 0;
    }
    rope.slice(from..to).to_string().width()
}

/// What a key press did to the editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyOutcome {
    Edited,
    Moved,
    Unhandled,
}

pub struct Editor {
    buffer: Rope,
    cursor: usize,
    anchor: Option<usize>,
    clipboard: String,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_edit: Instant,
    last_kind: Option<EditKind>,
    version: u64,
    /// First visible segment (viewport scroll).
    pub scroll: usize,
    cache: Option<(u64, usize, WrapMap)>,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    pub fn new() -> Self {
        Self {
            buffer: Rope::new(),
            cursor: 0,
            anchor: None,
            clipboard: String::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: Instant::now(),
            last_kind: None,
            version: 0,
            scroll: 0,
            cache: None,
        }
    }

    /// Replace the buffer (document switch): history and selection reset.
    pub fn load(&mut self, text: &str) {
        self.buffer = Rope::from_str(text);
        self.cursor = 0;
        self.anchor = None;
        self.undo.clear();
        self.redo.clear();
        self.last_kind = None;
        self.version += 1;
        self.scroll = 0;
        self.cache = None;
    }

    pub fn text(&self) -> String {
        self.buffer.to_string()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Move the cursor to a char offset without editing (section jumps).
    /// Clears any selection; the next draw scrolls it into view.
    pub fn goto_char(&mut self, offset: usize) {
        self.cursor = offset.min(self.buffer.len_chars());
        self.anchor = None;
    }

    /// Normalized selection, if any.
    pub fn selection(&self) -> Option<Range<usize>> {
        let a = self.anchor?;
        (a != self.cursor).then(|| a.min(self.cursor)..a.max(self.cursor))
    }

    fn refresh_cache(&mut self, width: usize) {
        let stale = self
            .cache
            .as_ref()
            .is_none_or(|(v, w, _)| *v != self.version || *w != width);
        if stale {
            let built = WrapMap::build(&self.buffer, width);
            self.cache = Some((self.version, width, built));
        }
    }

    /// Capture an incremental rewrap plan while the cache still matches the
    /// pre-edit buffer (returns `None` when there is nothing to splice into).
    fn span_before(&self, range: &Range<usize>, ins: usize) -> Option<EditSpan> {
        self.cache.as_ref().and_then(|(v, _, _)| {
            (*v == self.version).then(|| EditSpan::before(&self.buffer, range, ins))
        })
    }

    /// Apply the plan captured by `span_before` after the buffer changed and
    /// `version` was bumped; falls back to a full rebuild when out of sync.
    fn finish_span(&mut self, span: Option<EditSpan>) {
        let Some(mut span) = span else {
            self.cache = None;
            return;
        };
        if let Some((v, w, map)) = self.cache.as_mut()
            && *v + 1 == self.version
        {
            span.complete(&self.buffer);
            map.splice(&self.buffer, *w, &span);
            *v = self.version;
        } else {
            self.cache = None;
        }
    }

    // ---- editing ---------------------------------------------------------

    fn snapshot_before(&mut self, kind: EditKind) {
        let now = Instant::now();
        let coalesce = kind == EditKind::Insert
            && self.last_kind == Some(EditKind::Insert)
            && now.duration_since(self.last_edit) < COALESCE;
        if !coalesce {
            self.undo.push(Snapshot {
                text: self.buffer.clone(),
                cursor: self.cursor,
                anchor: self.anchor,
            });
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last_edit = now;
        self.last_kind = Some(kind);
    }

    fn replace_range(&mut self, range: Range<usize>, with: &str, kind: EditKind) {
        self.snapshot_before(kind);
        let span = self.span_before(&range, with.chars().count());
        if !range.is_empty() {
            self.buffer.remove(range.clone());
        }
        self.cursor = range.start;
        if !with.is_empty() {
            self.buffer.insert(self.cursor, with);
            self.cursor += with.chars().count();
        }
        self.anchor = None;
        self.version += 1;
        self.finish_span(span);
    }

    fn delete_selection(&mut self, kind: EditKind) -> bool {
        if let Some(sel) = self.selection() {
            self.replace_range(sel, "", kind);
            true
        } else {
            false
        }
    }

    pub fn insert(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let kind = if text.contains('\n') {
            EditKind::Newline
        } else {
            EditKind::Insert
        };
        if let Some(sel) = self.selection() {
            self.replace_range(sel, text, kind);
        } else {
            let c = self.cursor;
            self.replace_range(c..c, text, kind);
        }
    }

    pub fn backspace(&mut self) {
        if self.delete_selection(EditKind::Delete) {
            return;
        }
        if self.cursor == 0 {
            return;
        }
        let from = prev_boundary(&self.buffer, self.cursor);
        self.replace_range(from..self.cursor, "", EditKind::Delete);
    }

    pub fn delete_forward(&mut self) {
        if self.delete_selection(EditKind::Delete) {
            return;
        }
        if self.cursor >= self.buffer.len_chars() {
            return;
        }
        let to = next_boundary(&self.buffer, self.cursor);
        self.replace_range(self.cursor..to, "", EditKind::Delete);
    }

    /// Copy the selection to the internal clipboard; returns the copied text
    /// (so the app can also emit OSC52 to the system clipboard).
    pub fn copy(&mut self) -> Option<String> {
        let sel = self.selection()?;
        self.clipboard = self.buffer.slice(sel).to_string();
        Some(self.clipboard.clone())
    }

    pub fn cut(&mut self) -> Option<String> {
        let copied = self.copy()?;
        self.delete_selection(EditKind::Delete);
        Some(copied)
    }

    pub fn paste(&mut self) {
        let text = self.clipboard.clone();
        self.insert(&text);
    }

    pub fn clipboard(&self) -> &str {
        &self.clipboard
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.buffer.len_chars();
        self.last_kind = None;
    }

    fn restore(&mut self, snap: Snapshot) {
        let new_text = snap.text;
        let old_len = self.buffer.len_chars();
        let new_len = new_text.len_chars();
        // Diff old vs. restored text into one contiguous replacement
        // (common prefix + common suffix), then rewrap only that region.
        let min = old_len.min(new_len);
        let mut p = 0;
        {
            let (mut ai, mut bi) = (self.buffer.chars(), new_text.chars());
            while p < min && ai.next() == bi.next() {
                p += 1;
            }
        }
        let mut s = 0;
        let max_s = min - p;
        while s < max_s && self.buffer.char(old_len - 1 - s) == new_text.char(new_len - 1 - s) {
            s += 1;
        }
        let range = p..old_len - s;
        let span = self.span_before(&range, new_len - p - s);
        self.buffer = new_text;
        self.cursor = snap.cursor;
        self.anchor = snap.anchor;
        self.version += 1;
        self.finish_span(span);
        self.last_kind = None;
    }

    pub fn undo(&mut self) -> bool {
        let Some(snap) = self.undo.pop() else {
            return false;
        };
        let current = Snapshot {
            text: self.buffer.clone(),
            cursor: self.cursor,
            anchor: self.anchor,
        };
        self.redo.push(current);
        self.restore(snap);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(snap) = self.redo.pop() else {
            return false;
        };
        let current = Snapshot {
            text: self.buffer.clone(),
            cursor: self.cursor,
            anchor: self.anchor,
        };
        self.undo.push(current);
        self.restore(snap);
        true
    }

    // ---- movement --------------------------------------------------------

    fn begin_shift(&mut self) {
        if self.anchor.is_none() {
            self.anchor = Some(self.cursor);
        }
    }

    fn leave_movement(&mut self, shift: bool) {
        if !shift {
            self.anchor = None;
            self.last_kind = None;
        }
    }

    fn move_left(&mut self, shift: bool) {
        if shift {
            self.begin_shift();
        } else if let Some(sel) = self.selection() {
            self.leave_movement(false);
            self.cursor = sel.start;
            return;
        } else {
            self.leave_movement(false);
        }
        self.cursor = prev_boundary(&self.buffer, self.cursor);
    }

    fn move_right(&mut self, shift: bool) {
        if shift {
            self.begin_shift();
        } else if let Some(sel) = self.selection() {
            self.leave_movement(false);
            self.cursor = sel.end;
            return;
        } else {
            self.leave_movement(false);
        }
        self.cursor = next_boundary(&self.buffer, self.cursor);
    }

    /// Vertical move across wrapped segments by `delta` segments.
    fn move_vertical(&mut self, delta: isize, shift: bool, width: usize) {
        self.leave_movement(shift);
        if shift {
            self.begin_shift();
        }
        let width = width.max(1);
        let map = WrapMap::build(&self.buffer, width);
        let (si, col) = map.locate(&self.buffer, self.cursor);
        let target = si as isize + delta;
        let clamped = target.clamp(0, map.len() as isize - 1) as usize;
        self.cursor = map.char_at(&self.buffer, clamped, col);
    }

    /// Home/End operate on the *logical* line (the whole paragraph), so a
    /// cursor sitting at a wrap boundary can always get back to its start.
    fn move_line_edge(&mut self, edge: LineEdge, shift: bool) {
        self.leave_movement(shift);
        if shift {
            self.begin_shift();
        }
        let line = self.buffer.char_to_line(self.cursor);
        let lstart = self.buffer.line_to_char(line);
        let line_text = self.buffer.line(line);
        let mut content_end = lstart + line_text.len_chars();
        if line_text
            .get_char(line_text.len_chars().saturating_sub(1))
            .is_some_and(|c| c == '\n')
        {
            content_end -= 1;
            if line_text
                .get_char(line_text.len_chars().saturating_sub(2))
                .is_some_and(|c| c == '\r')
            {
                content_end -= 1;
            }
        }
        self.cursor = match edge {
            LineEdge::Start => lstart,
            LineEdge::End => content_end,
        };
    }

    pub fn move_page(&mut self, delta: isize, shift: bool, width: usize, height: usize) {
        let step = delta * height.max(1) as isize;
        self.move_vertical(step, shift, width);
    }

    // ---- key handling ----------------------------------------------------

    /// `width`/`height` are the editor's inner viewport in cells.
    pub fn handle_key(&mut self, key: KeyEvent, width: usize, height: usize) -> KeyOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Char('a') if ctrl => {
                self.select_all();
                KeyOutcome::Moved
            }
            KeyCode::Char('c') if ctrl => {
                self.copy();
                KeyOutcome::Moved
            }
            KeyCode::Char('x') if ctrl => {
                if self.selection().is_some() {
                    self.cut();
                    KeyOutcome::Edited
                } else {
                    KeyOutcome::Moved
                }
            }
            KeyCode::Char('v') if ctrl => {
                self.paste();
                KeyOutcome::Edited
            }
            KeyCode::Char('z') if ctrl => {
                if self.undo() {
                    KeyOutcome::Edited
                } else {
                    KeyOutcome::Moved
                }
            }
            KeyCode::Char('y') if ctrl => {
                if self.redo() {
                    KeyOutcome::Edited
                } else {
                    KeyOutcome::Moved
                }
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                let mut buf = [0u8; 4];
                self.insert(c.encode_utf8(&mut buf));
                KeyOutcome::Edited
            }
            KeyCode::Enter => {
                self.insert("\n");
                KeyOutcome::Edited
            }
            KeyCode::Tab => {
                self.insert("    ");
                KeyOutcome::Edited
            }
            KeyCode::Backspace => {
                self.backspace();
                KeyOutcome::Edited
            }
            KeyCode::Delete => {
                self.delete_forward();
                KeyOutcome::Edited
            }
            KeyCode::Left => {
                self.move_left(shift);
                KeyOutcome::Moved
            }
            KeyCode::Right => {
                self.move_right(shift);
                KeyOutcome::Moved
            }
            KeyCode::Up => {
                self.move_vertical(-1, shift, width);
                KeyOutcome::Moved
            }
            KeyCode::Down => {
                self.move_vertical(1, shift, width);
                KeyOutcome::Moved
            }
            KeyCode::Home => {
                self.move_line_edge(LineEdge::Start, shift);
                KeyOutcome::Moved
            }
            KeyCode::End => {
                self.move_line_edge(LineEdge::End, shift);
                KeyOutcome::Moved
            }
            KeyCode::PageUp => {
                self.move_page(-1, shift, width, height);
                KeyOutcome::Moved
            }
            KeyCode::PageDown => {
                self.move_page(1, shift, width, height);
                KeyOutcome::Moved
            }
            _ => KeyOutcome::Unhandled,
        }
    }

    /// Mouse within the editor area: `(x, y)` are area-relative cells.
    /// Returns true when the editor consumed the event.
    pub fn handle_mouse(
        &mut self,
        kind: MouseEventKind,
        x: u16,
        y: u16,
        width: usize,
        height: usize,
    ) -> bool {
        let width = width.max(1);
        match kind {
            MouseEventKind::Down(_) => {
                let map = WrapMap::build(&self.buffer, width);
                let si = (self.scroll + y as usize).min(map.len() - 1);
                let click = map.char_at(&self.buffer, si, x as usize);
                self.anchor = Some(click);
                self.cursor = click;
                self.last_kind = None;
                true
            }
            MouseEventKind::Drag(_) => {
                let map = WrapMap::build(&self.buffer, width);
                let si = (self.scroll + y as usize).min(map.len() - 1);
                let to = map.char_at(&self.buffer, si, x as usize);
                if self.anchor.is_none() {
                    self.anchor = Some(self.cursor);
                }
                self.cursor = to;
                true
            }
            MouseEventKind::ScrollUp => {
                self.scroll = self.scroll.saturating_sub(3);
                true
            }
            MouseEventKind::ScrollDown => {
                let len = WrapMap::build(&self.buffer, width).len();
                self.scroll = (self.scroll + 3).min(len.saturating_sub(height.max(1)));
                true
            }
            _ => false,
        }
    }

    pub fn clamp_scroll(&mut self, height: usize) {
        let len = self.cache.as_ref().map(|(_, _, m)| m.len()).unwrap_or(1);
        let max = len.saturating_sub(height.max(1));
        self.scroll = self.scroll.min(max);
    }

    /// Keep the cursor segment inside the viewport.
    pub fn ensure_cursor_visible(&mut self, width: usize, height: usize) {
        let height = height.max(1);
        self.refresh_cache(width);
        let si = self
            .cache
            .as_ref()
            .unwrap()
            .2
            .locate(&self.buffer, self.cursor)
            .0;
        if si < self.scroll {
            self.scroll = si;
        } else if si >= self.scroll + height {
            self.scroll = si + 1 - height;
        }
        self.clamp_scroll(height);
    }

    // ---- rendering -------------------------------------------------------

    /// Draw the wrapped buffer with selection + cursor styles.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer, focused: bool) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let width = area.width as usize;
        let height = area.height as usize;
        self.refresh_cache(width);
        let map = &self.cache.as_ref().unwrap().2;
        let sel = self.selection();
        let cursor = self.cursor;

        let mut lines = Vec::with_capacity(height);
        for si in self.scroll..(self.scroll + height).min(map.len()) {
            let seg = map.seg(si);
            let text: String = self.buffer.slice(seg.start..seg.end).to_string();
            let mut spans: Vec<Span> = Vec::new();
            let mut run = String::new();
            let mut run_style = Style::default();
            let mut char_off = 0usize;
            for g in text.graphemes(true) {
                let clen = g.chars().count();
                let g_start = seg.start + char_off;
                let g_end = g_start + clen;
                let mut style = Style::default();
                if let Some(sel) = &sel
                    && g_start >= sel.start
                    && g_end <= sel.end
                {
                    style = style.bg(Color::Rgb(40, 60, 90));
                }
                if focused && cursor >= g_start && cursor < g_end {
                    style = style.bg(Color::Rgb(220, 220, 220)).fg(Color::Black);
                }
                if style != run_style {
                    if !run.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut run), run_style));
                    }
                    run_style = style;
                }
                run.push_str(g);
                char_off += clen;
            }
            if !run.is_empty() {
                spans.push(Span::styled(run, run_style));
            }
            lines.push(Line::from(spans));
        }
        Paragraph::new(lines).render(area, buf);
    }
}

#[derive(Clone, Copy)]
enum LineEdge {
    Start,
    End,
}

/// Char index of the grapheme boundary before `cursor`.
fn prev_boundary(rope: &Rope, cursor: usize) -> usize {
    if cursor == 0 {
        return 0;
    }
    let line = rope.char_to_line(cursor);
    let lstart = rope.line_to_char(line);
    if cursor == lstart {
        return cursor - 1; // the line terminator above
    }
    let prefix: String = rope.slice(lstart..cursor).to_string();
    let last = prefix.graphemes(true).next_back().unwrap_or("");
    cursor - last.chars().count().max(1)
}

/// Char index of the grapheme boundary after `cursor`.
fn next_boundary(rope: &Rope, cursor: usize) -> usize {
    if cursor >= rope.len_chars() {
        return rope.len_chars();
    }
    let line = rope.char_to_line(cursor);
    let lend = if line + 1 < rope.len_lines() {
        rope.line_to_char(line + 1)
    } else {
        rope.len_chars()
    };
    if cursor >= lend {
        return (cursor + 1).min(rope.len_chars());
    }
    let suffix: String = rope.slice(cursor..lend).to_string();
    let first = suffix.graphemes(true).next().unwrap_or("");
    (cursor + first.chars().count().max(1)).min(rope.len_chars())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::MouseButton;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }
    fn shift(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    fn seg_texts(map: &WrapMap, rope: &Rope) -> Vec<String> {
        (0..map.len())
            .map(|i| {
                let s = map.seg(i);
                rope.slice(s.start..s.end).to_string()
            })
            .collect()
    }

    #[test]
    fn typing_and_backspace() {
        let mut e = Editor::new();
        for c in "héllo".chars() {
            e.handle_key(key(KeyCode::Char(c)), 40, 10);
        }
        assert_eq!(e.text(), "héllo");
        e.handle_key(key(KeyCode::Backspace), 40, 10);
        assert_eq!(e.text(), "héll");
        e.handle_key(key(KeyCode::Backspace), 40, 10);
        assert_eq!(e.text(), "hél");
    }

    #[test]
    fn backspace_removes_grapheme_clusters() {
        let mut e = Editor::new();
        e.load("a👍b");
        e.handle_key(key(KeyCode::End), 40, 10);
        e.handle_key(key(KeyCode::Backspace), 40, 10);
        assert_eq!(e.text(), "a👍");
        e.handle_key(key(KeyCode::Backspace), 40, 10);
        assert_eq!(e.text(), "a", "emoji must be deleted as one grapheme");
    }

    #[test]
    fn soft_wrap_keeps_full_lines_and_words() {
        let rope = Rope::from_str("aaa bbb ccc ddd");
        let map = WrapMap::build(&rope, 7);
        // "aaa bbb" fills 7 cols; the following space stays on line 1
        // (clipped), so line 2 starts cleanly at "ccc". The trailing empty
        // segment is the end-of-line cursor home after an exactly-full line.
        assert_eq!(seg_texts(&map, &rope), vec!["aaa bbb ", "ccc ddd", ""]);
    }

    #[test]
    fn hard_break_for_long_words() {
        let rope = Rope::from_str("abcdefghij");
        let map = WrapMap::build(&rope, 4);
        assert_eq!(seg_texts(&map, &rope), vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn word_break_prefers_last_space() {
        let rope = Rope::from_str("hello wonderful world");
        let map = WrapMap::build(&rope, 10);
        let texts = seg_texts(&map, &rope);
        for t in &texts {
            assert!(t.width() <= 10, "segment {t:?} exceeds width");
        }
        assert_eq!(texts, vec!["hello ", "wonderful ", "world"]);
    }

    #[test]
    fn wide_chars_wrap_by_display_width() {
        let rope = Rope::from_str("日本語text");
        let map = WrapMap::build(&rope, 4);
        let texts = seg_texts(&map, &rope);
        // 2 CJK chars = width 4 → "日本"; then "語te" (2+1+1=4); then "xt".
        assert_eq!(texts[0], "日本");
        assert_eq!(texts[1], "語te");
    }

    #[test]
    fn vertical_movement_follows_wrapped_segments() {
        let mut e = Editor::new();
        e.load("aaa bbb ccc ddd");
        e.handle_key(key(KeyCode::Down), 7, 10);
        assert_eq!(e.cursor(), 8, "down from col 0 lands at col 0 of next seg");
        e.handle_key(key(KeyCode::Up), 7, 10);
        assert_eq!(e.cursor(), 0);
    }

    #[test]
    fn selection_shift_and_copy_cut_paste() {
        let mut e = Editor::new();
        e.load("hello world");
        e.handle_key(key(KeyCode::End), 40, 10);
        for _ in 0..5 {
            e.handle_key(shift(KeyCode::Left), 40, 10);
        }
        assert_eq!(e.selection(), Some(6..11));
        assert_eq!(
            e.handle_key(ctrl(KeyCode::Char('c')), 40, 10),
            KeyOutcome::Moved
        );
        assert_eq!(e.clipboard(), "world");
        e.handle_key(ctrl(KeyCode::Char('x')), 40, 10);
        assert_eq!(e.text(), "hello ");
        e.handle_key(key(KeyCode::End), 40, 10);
        e.handle_key(ctrl(KeyCode::Char('v')), 40, 10);
        assert_eq!(e.text(), "hello world");
    }

    #[test]
    fn undo_redo_roundtrip_with_burst_coalescing() {
        let mut e = Editor::new();
        e.load("base");
        e.handle_key(key(KeyCode::End), 40, 10);
        for c in "XYZ".chars() {
            e.handle_key(key(KeyCode::Char(c)), 40, 10);
        }
        assert_eq!(e.text(), "baseXYZ");
        assert!(e.undo());
        assert_eq!(e.text(), "base", "burst must undo as one step");
        assert!(e.redo());
        assert_eq!(e.text(), "baseXYZ");
        // A newline starts a new undo step.
        e.handle_key(key(KeyCode::Enter), 40, 10);
        e.handle_key(key(KeyCode::Char('q')), 40, 10);
        assert!(e.undo());
        assert_eq!(e.text(), "baseXYZ\n");
        assert!(e.undo());
        assert_eq!(e.text(), "baseXYZ");
        // Undo down to the origin.
        e.select_all();
        e.handle_key(key(KeyCode::Delete), 40, 10);
        assert_eq!(e.text(), "");
        for _ in 0..6 {
            e.undo();
        }
        assert_eq!(e.text(), "base", "undo walks back to the load point");
        assert!(!e.undo(), "stack exhausted at the origin");
    }

    #[test]
    fn select_all_delete_then_recover() {
        let mut e = Editor::new();
        e.load("keep or not");
        e.handle_key(ctrl(KeyCode::Char('a')), 40, 10);
        e.handle_key(key(KeyCode::Delete), 40, 10);
        assert_eq!(e.text(), "");
        assert!(e.undo());
        assert_eq!(e.text(), "keep or not");
    }

    #[test]
    fn mouse_click_and_drag_select_wrapped_text() {
        let mut e = Editor::new();
        e.load("aaa bbb ccc");
        let map = WrapMap::build(&e.buffer, 7);
        assert_eq!(map.len(), 2);

        // Click on second wrapped line (y=1), x=1 → second seg starts at 8.
        e.handle_mouse(MouseEventKind::Down(MouseButton::Left), 1, 1, 7, 10);
        assert_eq!(e.cursor(), 9, "click lands at the clicked column");
        assert!(e.selection().is_none(), "plain click clears selection");

        // Drag back over the boundary → selection across the wrap.
        e.handle_mouse(MouseEventKind::Drag(MouseButton::Left), 2, 0, 7, 10);
        let sel = e.selection().expect("drag creates selection");
        assert!(sel.start < 9 && sel.end == 9, "selection = {sel:?}");
    }

    #[test]
    fn scroll_wheel_moves_viewport_and_clamps() {
        let mut e = Editor::new();
        e.load("one\ntwo\nthree\nfour");
        e.handle_mouse(MouseEventKind::ScrollDown, 0, 0, 10, 2);
        assert!(e.scroll > 0);
        for _ in 0..100 {
            e.handle_mouse(MouseEventKind::ScrollDown, 0, 0, 10, 2);
        }
        let len = WrapMap::build(&e.buffer, 10).len();
        assert!(
            e.scroll <= len.saturating_sub(2),
            "scroll must clamp to the last screen"
        );
        for _ in 0..100 {
            e.handle_mouse(MouseEventKind::ScrollUp, 0, 0, 10, 2);
        }
        assert_eq!(e.scroll, 0);
    }

    /// Every edit path must leave the cached wrap map byte-for-byte equal to
    /// a from-scratch rebuild — the incremental splice may never drift.
    #[test]
    fn incremental_rewrap_always_matches_a_full_rebuild() {
        let text = "The quick brown fox jumps over the lazy dog and keeps running.\n\
                    Second line with considerably more words so that it wraps several \
                    segments at this small width, yes it does.\n\
                    含有宽字符的行 mixed with ascii and rather long content to force segments.\n\
                    \n\
                    short\n\
                    trailing space line \n";
        let width = 10usize;
        let mut e = Editor::new();
        e.load(text);

        fn assert_fresh(e: &mut Editor, width: usize, step: &str) {
            e.ensure_cursor_visible(width, 5);
            let (_, _, map) = e.cache.as_ref().expect("cache built");
            let full = WrapMap::build(&e.buffer, width);
            assert_eq!(
                map.segs, full.segs,
                "cache drifted from full rebuild after: {step}"
            );
        }

        assert_fresh(&mut e, width, "load");

        // Insert mid-line (trailing segments must be renumbered).
        e.cursor = 20;
        e.insert("x");
        assert_fresh(&mut e, width, "insert mid-line");

        // Hard line break mid-line.
        e.insert("\n");
        assert_fresh(&mut e, width, "insert newline");

        // Backspace joining lines back together.
        e.handle_key(key(KeyCode::Backspace), width, 5);
        assert_fresh(&mut e, width, "backspace join");

        // Multi-line paste.
        e.insert("alpha\nbeta\ngamma long enough to wrap at width ten\n");
        assert_fresh(&mut e, width, "multiline paste");

        // Mouse-placed caret, then type.
        e.handle_mouse(MouseEventKind::Down(MouseButton::Left), 3, 1, width, 5);
        e.insert("z");
        assert_fresh(&mut e, width, "mouse caret + type");

        // CRLF document: reload and edit.
        e.load("line one\r\nline two is longer than the wrap width here\r\nlast\r\n");
        e.insert("Q");
        assert_fresh(&mut e, width, "crlf insert");

        // Select-all replacement (removes everything, then inserts).
        e.select_all();
        e.insert("a");
        assert_fresh(&mut e, width, "select-all replace");

        // Undo/redo round trip (diff-based restore must splice too).
        e.load(text);
        e.ensure_cursor_visible(width, 5);
        e.cursor = 40;
        e.insert("burst of typing");
        assert_fresh(&mut e, width, "typed burst");
        assert!(e.undo(), "burst coalesced into one undo step");
        assert_fresh(&mut e, width, "undo");
        assert!(e.redo());
        assert_fresh(&mut e, width, "redo");
        assert!(e.undo());
        assert_fresh(&mut e, width, "undo again");

        // Edits at the very start and very end of the document.
        e.cursor = 0;
        e.insert("start ");
        assert_fresh(&mut e, width, "insert at doc start");
        e.cursor = e.buffer.len_chars();
        e.insert(" end");
        assert_fresh(&mut e, width, "insert at doc end");

        // Deletion spanning several lines.
        e.anchor = Some(0);
        e.cursor = e.buffer.len_chars() / 2;
        e.handle_key(key(KeyCode::Delete), width, 5);
        assert_fresh(&mut e, width, "selection delete across lines");
    }

    /// ~50k words of realistic prose: a line break every ten words.
    fn prose_50k() -> String {
        const WORDS: [&str; 10] = [
            "lorem",
            "ipsum",
            "dolor",
            "sit",
            "amet",
            "consectetur",
            "adipiscing",
            "elit",
            "delectus",
            "blanditiis",
        ];
        let mut text = String::new();
        for i in 0..50_000 {
            text.push_str(WORDS[i % 10]);
            text.push(if i % 10 == 9 { '\n' } else { ' ' });
        }
        text
    }

    #[test]
    fn fifty_thousand_word_file_stays_responsive() {
        let text = prose_50k();
        let mut e = Editor::new();

        let t0 = Instant::now();
        e.load(&text);
        let load_ms = t0.elapsed().as_millis();

        let t0 = Instant::now();
        e.handle_key(key(KeyCode::End), 80, 40);
        e.ensure_cursor_visible(80, 40);
        let wrap_ms = t0.elapsed().as_millis();

        // Keystroke at the caret: incremental splice, not a full rewrap.
        let t0 = Instant::now();
        e.handle_key(key(KeyCode::Char('x')), 80, 40);
        e.ensure_cursor_visible(80, 40);
        let keystroke_ms = t0.elapsed().as_millis();

        // Keystroke mid-document: worst case for the splice (every later
        // segment must be renumbered).
        let t0 = Instant::now();
        e.cursor = e.buffer.len_chars() / 2;
        e.handle_key(key(KeyCode::Char('y')), 80, 40);
        e.ensure_cursor_visible(80, 40);
        let keystroke_mid_ms = t0.elapsed().as_millis();

        println!(
            "50k-word prose: load={load_ms}ms wrap={wrap_ms}ms keystroke={keystroke_ms}ms keystroke_mid={keystroke_mid_ms}ms"
        );
        // Generous CI bounds (debug builds); release numbers in ADR-0021.
        assert!(load_ms < 2_000, "load took {load_ms}ms");
        assert!(wrap_ms < 2_000, "first wrap took {wrap_ms}ms");
        assert!(keystroke_ms < 1_000, "keystroke took {keystroke_ms}ms");
        assert!(
            keystroke_mid_ms < 1_000,
            "mid-doc keystroke took {keystroke_mid_ms}ms"
        );
        // The whole point of the incremental splice: an edit must be far
        // cheaper than re-wrapping the document.
        assert!(
            keystroke_ms * 2 < wrap_ms,
            "keystroke {keystroke_ms}ms must beat full wrap {wrap_ms}ms"
        );
        assert!(
            keystroke_mid_ms * 2 < wrap_ms,
            "mid-doc keystroke {keystroke_mid_ms}ms must beat full wrap {wrap_ms}ms"
        );
    }

    /// Pathological input: 50k words in a *single* logical line (pasted wall
    /// of text). Per-line incremental rewrap cannot subdivide it, so the
    /// bound is on `wrap_one`'s windowed scan instead of O(line²).
    #[test]
    fn single_line_wall_of_text_stays_bounded() {
        let text = "lorem ipsum dolor sit amet consectetur adipiscing elit ".repeat(50_000 / 6);
        let mut e = Editor::new();

        let t0 = Instant::now();
        e.load(&text);
        let load_ms = t0.elapsed().as_millis();

        let t0 = Instant::now();
        e.handle_key(key(KeyCode::End), 80, 40);
        e.ensure_cursor_visible(80, 40);
        let wrap_ms = t0.elapsed().as_millis();

        // Sample a few keystrokes and keep the min: this test binary runs
        // its tests in parallel, so a single sample can catch a sibling
        // test's scheduling stall instead of the editor's actual cost.
        // Budgets are generous for debug builds on shared/CI-class
        // hardware; the relative guard below is the precise detector for
        // edit-path regressions.
        let mut keystroke_ms = u128::MAX;
        for _ in 0..3 {
            let t0 = Instant::now();
            e.handle_key(key(KeyCode::Char('x')), 80, 40);
            e.ensure_cursor_visible(80, 40);
            keystroke_ms = keystroke_ms.min(t0.elapsed().as_millis());
        }

        println!("50k-word wall: load={load_ms}ms wrap={wrap_ms}ms keystroke={keystroke_ms}ms");
        assert!(load_ms < 2_000, "load took {load_ms}ms");
        assert!(wrap_ms < 3_000, "first wrap took {wrap_ms}ms");
        assert!(keystroke_ms < 3_000, "keystroke took {keystroke_ms}ms");
        // One giant line means an edit rebuilds that line: allow it to cost
        // what the rebuild costs, but no more than a small multiple of it.
        assert!(
            keystroke_ms <= wrap_ms * 2 + 100,
            "keystroke {keystroke_ms}ms vs wrap {wrap_ms}ms"
        );
    }

    #[test]
    fn wrap_map_survives_crlf_and_empty_lines() {
        let rope = Rope::from_str("line one\r\n\r\nlast");
        let map = WrapMap::build(&rope, 20);
        assert_eq!(seg_texts(&map, &rope), vec!["line one", "", "last"]);
    }

    #[test]
    fn home_end_stay_on_logical_line() {
        let mut e = Editor::new();
        e.load("aaa bbb ccc ddd");
        e.handle_key(key(KeyCode::End), 7, 10);
        assert_eq!(e.cursor(), 15, "End goes to the end of the paragraph");
        e.handle_key(key(KeyCode::Home), 7, 10);
        assert_eq!(e.cursor(), 0, "Home returns to the paragraph start");
    }
}
