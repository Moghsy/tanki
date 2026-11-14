//! Notes-TUI stream (Phase 3): multi-line text area for the raw HTML field
//! editor (PLAN 8.5), with its own undo stack. Edits stored HTML verbatim.
//!
//! The text is kept exactly as given: `value()` of an untouched area returns
//! the bytes `set_value` received (CRLF, trailing whitespace and all). Enter
//! inserts `\n` only. Soft wrapping is display only; the stored text never
//! gains a line break because of it.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};
use std::cell::Cell;
use unicode_width::UnicodeWidthChar;

const UNDO_LIMIT: usize = 200;
const TAB_WIDTH: usize = 4;

/// One display row: a byte range of the text without its `\n`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualLine {
    pub start: usize,
    pub end: usize,
    /// The last row of its logical line (a cursor may sit at `end`).
    pub last_of_line: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditKind {
    None,
    Insert,
    DeleteBack,
    DeleteForward,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    text: String,
    cursor: usize,
}

#[derive(Debug, Clone)]
pub struct TextArea {
    text: String,
    /// Byte offset, always on a char boundary.
    cursor: usize,
    anchor: Option<usize>,
    /// Display column kept while moving up and down.
    want_col: Option<usize>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_edit: EditKind,
    scroll: Cell<usize>,
    width: Cell<usize>,
    page: Cell<usize>,
}

impl Default for TextArea {
    fn default() -> Self {
        Self::new("")
    }
}

/// Display width of a char; control characters take no room (a tab four).
fn char_width(c: char) -> usize {
    match c {
        '\t' => TAB_WIDTH,
        c if c.is_control() => 0,
        c => c.width().unwrap_or(0),
    }
}

fn str_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Class {
    Space,
    Word,
    Other,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Other
    }
}

impl TextArea {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.len();
        Self {
            text,
            cursor,
            anchor: None,
            want_col: None,
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: EditKind::None,
            scroll: Cell::new(0),
            width: Cell::new(80),
            page: Cell::new(10),
        }
    }

    /// The exact text, byte for byte.
    pub fn value(&self) -> &str {
        &self.text
    }

    /// Replaces the text without any normalisation. The cursor goes to the
    /// end and the undo history is cleared.
    pub fn set_value(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
        self.anchor = None;
        self.want_col = None;
        self.undo.clear();
        self.redo.clear();
        self.last_edit = EditKind::None;
        self.scroll.set(0);
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn set_cursor(&mut self, byte: usize) {
        let mut byte = byte.min(self.text.len());
        while !self.text.is_char_boundary(byte) {
            byte -= 1;
        }
        self.cursor = byte;
        self.anchor = None;
        self.want_col = None;
        self.last_edit = EditKind::None;
    }

    /// Selected byte range, ordered.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then(|| (anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    pub fn selected_text(&self) -> Option<&str> {
        self.selection().map(|(s, e)| &self.text[s..e])
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.text.len();
        self.last_edit = EditKind::None;
    }

    // ---- undo ------------------------------------------------------------

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            cursor: self.cursor,
        }
    }

    /// Call before changing the text. Consecutive typing (or deleting the
    /// same way) shares one undo step.
    fn begin_edit(&mut self, kind: EditKind) {
        let coalesce = matches!(
            kind,
            EditKind::Insert | EditKind::DeleteBack | EditKind::DeleteForward
        ) && self.last_edit == kind
            && !self.undo.is_empty();
        if !coalesce {
            self.undo.push(self.snapshot());
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last_edit = kind;
        self.want_col = None;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.snapshot());
        self.text = prev.text;
        self.cursor = prev.cursor;
        self.anchor = None;
        self.last_edit = EditKind::None;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(self.snapshot());
        self.text = next.text;
        self.cursor = next.cursor;
        self.anchor = None;
        self.last_edit = EditKind::None;
        true
    }

    // ---- editing -----------------------------------------------------------

    fn delete_selection_raw(&mut self) -> bool {
        match self.selection() {
            Some((s, e)) => {
                self.text.drain(s..e);
                self.cursor = s;
                self.anchor = None;
                true
            }
            None => {
                self.anchor = None;
                false
            }
        }
    }

    fn insert_with(&mut self, s: &str, kind: EditKind) {
        if s.is_empty() && self.selection().is_none() {
            return;
        }
        self.begin_edit(kind);
        self.delete_selection_raw();
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    /// Inserts at the cursor, replacing a selection.
    pub fn insert_str(&mut self, s: &str) {
        self.insert_with(s, EditKind::Other);
    }

    /// A paste: one undo step, text inserted verbatim.
    pub fn paste(&mut self, s: &str) {
        self.insert_with(s, EditKind::Other);
    }

    /// Inserts at the cursor (the end of a selection), keeping the selected
    /// text. Used for media links.
    pub fn insert_at_cursor(&mut self, s: &str) {
        if let Some((_, e)) = self.selection() {
            self.cursor = e;
        }
        self.anchor = None;
        self.insert_with(s, EditKind::Other);
    }

    /// Replaces the selection (or inserts at the cursor) with `replacement`
    /// and puts the cursor `cursor_offset` bytes into it. One undo step.
    pub fn replace_selection(&mut self, replacement: &str, cursor_offset: usize) {
        self.begin_edit(EditKind::Other);
        self.delete_selection_raw();
        self.text.insert_str(self.cursor, replacement);
        self.cursor += cursor_offset.min(replacement.len());
    }

    /// Wraps the selection in `before` / `after`, or inserts both with the
    /// cursor between them when nothing is selected. With `trim`, leading
    /// and trailing whitespace (and `&nbsp;`) of the selection stays outside
    /// the wrapper, as desktop's cloze button does. One undo step.
    pub fn wrap_selection(&mut self, before: &str, after: &str, trim: bool) {
        let selected = self.selected_text().unwrap_or("").to_owned();
        let (lead, core, trail) = if trim {
            split_whitespace_edges(&selected)
        } else {
            ("", selected.as_str(), "")
        };
        let mut out = String::new();
        out.push_str(lead);
        out.push_str(before);
        let inner = out.len();
        out.push_str(core);
        let cursor_offset = if core.is_empty() {
            inner
        } else {
            inner + core.len() + after.len()
        };
        out.push_str(after);
        out.push_str(trail);
        self.replace_selection(&out, cursor_offset);
    }

    fn prev_boundary(&self, from: usize) -> usize {
        self.text[..from]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next_boundary(&self, from: usize) -> usize {
        self.text[from..]
            .chars()
            .next()
            .map_or(from, |c| from + c.len_utf8())
    }

    fn word_left(&self, from: usize) -> usize {
        let mut pos = from;
        let mut chars = self.text[..from].chars().rev().peekable();
        while let Some(&c) = chars.peek() {
            if class(c) != Class::Space {
                break;
            }
            pos -= c.len_utf8();
            chars.next();
        }
        if let Some(&first) = chars.peek() {
            let k = class(first);
            while let Some(&c) = chars.peek() {
                if class(c) != k {
                    break;
                }
                pos -= c.len_utf8();
                chars.next();
            }
        }
        pos
    }

    fn word_right(&self, from: usize) -> usize {
        let mut pos = from;
        let mut chars = self.text[from..].chars().peekable();
        while let Some(&c) = chars.peek() {
            if class(c) != Class::Space {
                break;
            }
            pos += c.len_utf8();
            chars.next();
        }
        if let Some(&first) = chars.peek() {
            let k = class(first);
            while let Some(&c) = chars.peek() {
                if class(c) != k {
                    break;
                }
                pos += c.len_utf8();
                chars.next();
            }
        }
        pos
    }

    fn line_start(&self, from: usize) -> usize {
        self.text[..from].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self, from: usize) -> usize {
        self.text[from..]
            .find('\n')
            .map_or(self.text.len(), |i| from + i)
    }

    fn backspace(&mut self, by_word: bool) {
        if self.selection().is_some() {
            self.begin_edit(EditKind::Other);
            self.delete_selection_raw();
            return;
        }
        if self.cursor == 0 {
            return;
        }
        let start = if by_word {
            self.word_left(self.cursor)
        } else {
            self.prev_boundary(self.cursor)
        };
        self.begin_edit(if by_word {
            EditKind::Other
        } else {
            EditKind::DeleteBack
        });
        self.text.drain(start..self.cursor);
        self.cursor = start;
    }

    fn delete_forward(&mut self, by_word: bool) {
        if self.selection().is_some() {
            self.begin_edit(EditKind::Other);
            self.delete_selection_raw();
            return;
        }
        if self.cursor >= self.text.len() {
            return;
        }
        let end = if by_word {
            self.word_right(self.cursor)
        } else {
            self.next_boundary(self.cursor)
        };
        self.begin_edit(if by_word {
            EditKind::Other
        } else {
            EditKind::DeleteForward
        });
        self.text.drain(self.cursor..end);
    }

    fn delete_range(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        self.begin_edit(EditKind::Other);
        self.anchor = None;
        self.text.drain(start..end);
        self.cursor = start;
    }

    // ---- wrapping and positions -----------------------------------------------

    /// The display rows for a given width in columns.
    pub fn visual_lines(&self, width: usize) -> Vec<VisualLine> {
        let width = width.max(1);
        let mut out = Vec::new();
        let mut line_start = 0;
        loop {
            let line_end = self.text[line_start..]
                .find('\n')
                .map_or(self.text.len(), |i| line_start + i);
            wrap_line(&self.text, line_start, line_end, width, &mut out);
            if line_end >= self.text.len() {
                break;
            }
            line_start = line_end + 1;
        }
        out
    }

    fn locate(lines: &[VisualLine], text: &str, cursor: usize) -> (usize, usize) {
        for (row, v) in lines.iter().enumerate() {
            let inside =
                cursor >= v.start && (cursor < v.end || (v.last_of_line && cursor <= v.end));
            if inside {
                return (row, str_width(&text[v.start..cursor]));
            }
        }
        let last = lines.len().saturating_sub(1);
        (
            last,
            lines.last().map_or(0, |v| str_width(&text[v.start..v.end])),
        )
    }

    /// `(row, column)` of the cursor at the given width.
    pub fn cursor_position(&self, width: usize) -> (usize, usize) {
        Self::locate(&self.visual_lines(width), &self.text, self.cursor)
    }

    fn byte_at_column(&self, v: &VisualLine, col: usize) -> usize {
        let mut acc = 0;
        for (i, c) in self.text[v.start..v.end].char_indices() {
            let w = char_width(c);
            if acc + w > col {
                return v.start + i;
            }
            acc += w;
        }
        v.end
    }

    fn move_rows(&mut self, delta: isize) {
        let lines = self.visual_lines(self.width.get());
        let (row, col) = Self::locate(&lines, &self.text, self.cursor);
        let want = *self.want_col.get_or_insert(col);
        let target = row as isize + delta;
        if target < 0 {
            self.cursor = 0;
        } else if target as usize >= lines.len() {
            self.cursor = self.text.len();
        } else {
            let v = lines[target as usize];
            self.cursor = self.byte_at_column(&v, want);
        }
    }

    // ---- keys ------------------------------------------------------------------

    fn move_to(&mut self, shift: bool, f: impl FnOnce(&mut Self)) {
        if shift {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.last_edit = EditKind::None;
        f(self);
    }

    /// Applies an editing or movement key. Returns false when the key is not
    /// the text area's (Tab, Esc, Ctrl+Enter, function keys...).
    pub fn handle_key(&mut self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let page = self.page.get().max(2) as isize - 1;
        match key.code {
            KeyCode::Char(c) if ctrl && !alt => match c.to_ascii_lowercase() {
                'z' if shift => {
                    self.redo();
                }
                'z' => {
                    self.undo();
                }
                'y' => {
                    self.redo();
                }
                'a' => self.move_to(false, |s| s.cursor = s.line_start(s.cursor)),
                'e' => self.move_to(false, |s| s.cursor = s.line_end(s.cursor)),
                'w' => self.backspace(true),
                'u' => {
                    let start = self.line_start(self.cursor);
                    self.delete_range(start, self.cursor);
                }
                'k' => {
                    let end = self.line_end(self.cursor);
                    let end = if end == self.cursor {
                        self.next_boundary(end)
                    } else {
                        end
                    };
                    self.delete_range(self.cursor, end);
                }
                _ => return false,
            },
            KeyCode::Char(_) if alt => return false,
            KeyCode::Char(c) => {
                let mut buf = [0; 4];
                self.insert_with(c.encode_utf8(&mut buf), EditKind::Insert);
            }
            KeyCode::Enter if !ctrl && !alt => self.insert_with("\n", EditKind::Other),
            KeyCode::Backspace if alt => self.backspace(true),
            KeyCode::Backspace => self.backspace(ctrl),
            KeyCode::Delete => self.delete_forward(ctrl),
            KeyCode::Left => {
                if let (false, Some((s, _))) = (shift, self.selection()) {
                    self.move_to(false, |t| t.cursor = s);
                } else if ctrl {
                    self.move_to(shift, |s| s.cursor = s.word_left(s.cursor));
                } else {
                    self.move_to(shift, |s| s.cursor = s.prev_boundary(s.cursor));
                }
            }
            KeyCode::Right => {
                if let (false, Some((_, e))) = (shift, self.selection()) {
                    self.move_to(false, |t| t.cursor = e);
                } else if ctrl {
                    self.move_to(shift, |s| s.cursor = s.word_right(s.cursor));
                } else {
                    self.move_to(shift, |s| s.cursor = s.next_boundary(s.cursor));
                }
            }
            KeyCode::Up => self.move_to(shift, |s| s.move_rows(-1)),
            KeyCode::Down => self.move_to(shift, |s| s.move_rows(1)),
            KeyCode::PageUp => self.move_to(shift, |s| s.move_rows(-page)),
            KeyCode::PageDown => self.move_to(shift, |s| s.move_rows(page)),
            KeyCode::Home if ctrl => self.move_to(shift, |s| s.cursor = 0),
            KeyCode::End if ctrl => self.move_to(shift, |s| s.cursor = s.text.len()),
            KeyCode::Home => self.move_to(shift, |s| s.cursor = s.line_start(s.cursor)),
            KeyCode::End => self.move_to(shift, |s| s.cursor = s.line_end(s.cursor)),
            _ => return false,
        }
        true
    }

    // ---- drawing ---------------------------------------------------------------

    /// Draws the visible rows into `area` and keeps the cursor in view.
    /// The caret is drawn as a reversed cell when `focused`.
    pub fn render(&self, area: Rect, buf: &mut Buffer, focused: bool) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let width = area.width as usize;
        let height = area.height as usize;
        self.width.set(width);
        self.page.set(height);
        let lines = self.visual_lines(width);
        let (crow, ccol) = Self::locate(&lines, &self.text, self.cursor);
        let mut scroll = self.scroll.get().min(lines.len().saturating_sub(1));
        if crow < scroll {
            scroll = crow;
        } else if crow >= scroll + height {
            scroll = crow + 1 - height;
        }
        self.scroll.set(scroll);
        let selection = self.selection();
        let selected = Style::default().add_modifier(Modifier::REVERSED);
        for (i, v) in lines.iter().enumerate().skip(scroll).take(height) {
            let y = area.y + (i - scroll) as u16;
            let mut x = 0usize;
            for (off, c) in self.text[v.start..v.end].char_indices() {
                let w = char_width(c);
                if w == 0 {
                    continue;
                }
                let byte = v.start + off;
                let style = match selection {
                    Some((s, e)) if byte >= s && byte < e => selected,
                    _ => Style::default(),
                };
                if x + w > width {
                    break;
                }
                if c == '\t' {
                    for k in 0..w {
                        buf.set_string(area.x + (x + k) as u16, y, " ", style);
                    }
                } else {
                    let mut tmp = [0; 4];
                    buf.set_string(area.x + x as u16, y, &*c.encode_utf8(&mut tmp), style);
                }
                x += w;
            }
        }
        if focused && crow >= scroll && crow < scroll + height {
            let x = area.x + ccol.min(width - 1) as u16;
            let y = area.y + (crow - scroll) as u16;
            let cell = &mut buf[(x, y)];
            let style = cell.style().add_modifier(Modifier::REVERSED);
            cell.set_style(style);
        }
    }

    /// Current first visible display row (for tests).
    pub fn scroll_row(&self) -> usize {
        self.scroll.get()
    }
}

/// `(leading, core, trailing)` whitespace split; `&nbsp;` counts as space.
fn split_whitespace_edges(s: &str) -> (&str, &str, &str) {
    let mut start = 0;
    loop {
        let rest = &s[start..];
        if let Some(r) = rest.strip_prefix("&nbsp;") {
            start = s.len() - r.len();
        } else if let Some(c) = rest.chars().next().filter(|c| c.is_whitespace()) {
            start += c.len_utf8();
        } else {
            break;
        }
    }
    let mut end = s.len();
    while end > start {
        let head = &s[start..end];
        if head.ends_with("&nbsp;") {
            end -= "&nbsp;".len();
        } else if let Some(c) = head.chars().next_back().filter(|c| c.is_whitespace()) {
            end -= c.len_utf8();
        } else {
            break;
        }
    }
    (&s[..start], &s[start..end], &s[end..])
}

fn wrap_line(
    text: &str,
    line_start: usize,
    line_end: usize,
    width: usize,
    out: &mut Vec<VisualLine>,
) {
    let mut start = line_start;
    let mut col = 0usize;
    let mut last_break: Option<usize> = None;
    for (off, c) in text[line_start..line_end].char_indices() {
        let i = line_start + off;
        let w = char_width(c);
        if w > 0 && col > 0 && col + w > width {
            let brk = match last_break {
                Some(b) if b > start => b,
                _ => i,
            };
            out.push(VisualLine {
                start,
                end: brk,
                last_of_line: false,
            });
            start = brk;
            col = str_width(&text[brk..i]);
            last_break = None;
            if col > 0 && col + w > width {
                out.push(VisualLine {
                    start,
                    end: i,
                    last_of_line: false,
                });
                start = i;
                col = 0;
            }
        }
        col += w;
        if c == ' ' || c == '\t' {
            last_break = Some(i + c.len_utf8());
        }
    }
    out.push(VisualLine {
        start,
        end: line_end,
        last_of_line: true,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn with(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }
    fn type_str(t: &mut TextArea, s: &str) {
        for c in s.chars() {
            let code = if c == '\n' {
                KeyCode::Enter
            } else {
                KeyCode::Char(c)
            };
            assert!(t.handle_key(&key(code)));
        }
    }
    fn rows(t: &TextArea, width: usize) -> Vec<String> {
        t.visual_lines(width)
            .iter()
            .map(|v| t.value()[v.start..v.end].to_owned())
            .collect()
    }

    #[test]
    fn untouched_value_is_byte_identical() {
        for s in [
            "",
            "plain",
            "a\r\nb\r\n",
            "trailing  \t \n",
            "<div>x</div>\n\n",
            "caf\u{e9} e\u{301} &nbsp; \u{a0}",
            "{{c1::a}} \\(x\\) [sound:a.mp3]<br />",
        ] {
            let mut t = TextArea::default();
            t.set_value(s);
            assert_eq!(t.value(), s);
            // Moving, selecting and drawing do not touch the text.
            t.handle_key(&key(KeyCode::Home));
            t.handle_key(&with(KeyCode::Right, KeyModifiers::SHIFT));
            let mut buf = Buffer::empty(Rect::new(0, 0, 20, 4));
            t.render(Rect::new(0, 0, 20, 4), &mut buf, true);
            assert_eq!(t.value().as_bytes(), s.as_bytes());
        }
    }

    #[test]
    fn typing_enter_and_deleting() {
        let mut t = TextArea::default();
        type_str(&mut t, "ab\ncd");
        assert_eq!(t.value(), "ab\ncd");
        t.handle_key(&key(KeyCode::Backspace));
        t.handle_key(&key(KeyCode::Left));
        t.handle_key(&key(KeyCode::Delete));
        assert_eq!(t.value(), "ab\n");
        t.handle_key(&key(KeyCode::Home));
        t.handle_key(&key(KeyCode::Backspace));
        assert_eq!(t.value(), "ab");
        assert!(!t.handle_key(&key(KeyCode::Tab)));
        assert!(!t.handle_key(&with(KeyCode::Enter, KeyModifiers::CONTROL)));
        assert!(!t.handle_key(&key(KeyCode::Esc)));
    }

    #[test]
    fn movement_by_words_lines_and_pages() {
        let mut t = TextArea::new("one two  three\nfour five\nsix");
        t.set_cursor(0);
        t.handle_key(&with(KeyCode::Right, KeyModifiers::CONTROL));
        assert_eq!(t.cursor(), 3);
        t.handle_key(&with(KeyCode::Right, KeyModifiers::CONTROL));
        assert_eq!(t.cursor(), 7);
        t.handle_key(&with(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(t.cursor(), 4);
        t.handle_key(&key(KeyCode::End));
        assert_eq!(t.cursor(), 14);
        t.handle_key(&key(KeyCode::Down));
        assert_eq!(t.cursor(), 24); // column 14 clamps to the line end
        t.handle_key(&with(KeyCode::Home, KeyModifiers::CONTROL));
        assert_eq!(t.cursor(), 0);
        t.handle_key(&with(KeyCode::End, KeyModifiers::CONTROL));
        assert_eq!(t.cursor(), t.value().len());
        t.handle_key(&key(KeyCode::PageUp));
        assert_eq!(t.cursor(), 0);
        t.handle_key(&key(KeyCode::PageDown));
        assert_eq!(t.cursor(), t.value().len());
    }

    #[test]
    fn up_down_keep_the_goal_column() {
        let mut t = TextArea::new("abcdef\nab\nabcdef");
        t.set_cursor(5);
        t.handle_key(&key(KeyCode::Down));
        assert_eq!(t.cursor(), 9);
        t.handle_key(&key(KeyCode::Down));
        assert_eq!(t.cursor(), 10 + 5);
    }

    #[test]
    fn selection_replace_and_collapse() {
        let mut t = TextArea::new("hello world");
        t.set_cursor(0);
        for _ in 0..5 {
            t.handle_key(&with(KeyCode::Right, KeyModifiers::SHIFT));
        }
        assert_eq!(t.selected_text(), Some("hello"));
        type_str(&mut t, "J");
        assert_eq!(t.value(), "J world");
        t.select_all();
        assert_eq!(t.selected_text(), Some("J world"));
        t.handle_key(&key(KeyCode::Left));
        assert_eq!((t.cursor(), t.selection()), (0, None));
        t.select_all();
        t.handle_key(&key(KeyCode::Backspace));
        assert_eq!(t.value(), "");
    }

    #[test]
    fn undo_redo_coalesce_typing() {
        let mut t = TextArea::default();
        type_str(&mut t, "abc");
        t.insert_str("XY");
        type_str(&mut t, "de");
        assert_eq!(t.value(), "abcXYde");
        assert!(t.undo());
        assert_eq!(t.value(), "abcXY");
        assert!(t.undo());
        assert_eq!(t.value(), "abc");
        assert!(t.undo());
        assert_eq!(t.value(), "");
        assert!(!t.undo());
        assert!(t.redo());
        assert_eq!(t.value(), "abc");
        type_str(&mut t, "!");
        assert!(!t.can_redo());
        t.handle_key(&with(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(t.value(), "abc");
        t.handle_key(&with(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert_eq!(t.value(), "abc!");
    }

    #[test]
    fn paste_is_one_undo_step_and_verbatim() {
        let mut t = TextArea::new("ab");
        t.set_cursor(1);
        t.paste("x\r\ny\n z  ");
        assert_eq!(t.value(), "ax\r\ny\n z  b");
        assert!(t.undo());
        assert_eq!(t.value(), "ab");
        assert_eq!(t.cursor(), 1);
    }

    #[test]
    fn wrap_selection_cases() {
        let mut t = TextArea::new("a  b ");
        t.set_cursor(1);
        t.handle_key(&with(KeyCode::Right, KeyModifiers::SHIFT));
        t.handle_key(&with(KeyCode::Right, KeyModifiers::SHIFT));
        t.handle_key(&with(KeyCode::Right, KeyModifiers::SHIFT));
        t.handle_key(&with(KeyCode::Right, KeyModifiers::SHIFT));
        assert_eq!(t.selected_text(), Some("  b "));
        t.wrap_selection("{{c1::", "}}", true);
        assert_eq!(t.value(), "a  {{c1::b}} ");
        assert_eq!(t.cursor(), "a  {{c1::b}}".len());
        t.undo();
        assert_eq!(t.value(), "a  b ");
        let mut e = TextArea::new("x");
        e.wrap_selection("<b>", "</b>", false);
        assert_eq!(e.value(), "x<b></b>");
        assert_eq!(e.cursor(), "x<b>".len());
        let mut n = TextArea::new("&nbsp;hi&nbsp;");
        n.select_all();
        n.wrap_selection("<i>", "</i>", true);
        assert_eq!(n.value(), "&nbsp;<i>hi</i>&nbsp;");
    }

    #[test]
    fn soft_wrapping_with_wide_chars() {
        let t = TextArea::new("hello world foo");
        assert_eq!(rows(&t, 8), ["hello ", "world ", "foo"]);
        let t = TextArea::new("abcdefghij");
        assert_eq!(rows(&t, 4), ["abcd", "efgh", "ij"]);
        let t = TextArea::new("日本語です");
        assert_eq!(rows(&t, 5), ["日本", "語で", "す"]);
        let t = TextArea::new("a\n\nb");
        assert_eq!(rows(&t, 5), ["a", "", "b"]);
        let t = TextArea::new("e\u{301}x");
        assert_eq!(rows(&t, 1), ["e\u{301}", "x"]);
    }

    #[test]
    fn cursor_position_on_wrapped_rows() {
        let mut t = TextArea::new("hello world");
        t.set_cursor(6);
        assert_eq!(t.cursor_position(8), (1, 0));
        t.set_cursor(11);
        assert_eq!(t.cursor_position(8), (1, 5));
        let mut w = TextArea::new("日本a");
        w.set_cursor(6);
        assert_eq!(w.cursor_position(10), (0, 4));
    }

    #[test]
    fn scrolls_to_keep_the_cursor_visible() {
        let text: String = (0..10).map(|i| format!("line{i}\n")).collect();
        let t = TextArea::new(text);
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::empty(area);
        t.render(area, &mut buf, true);
        // cursor on the empty last row 10
        assert_eq!(t.scroll_row(), 8);
        let mut t = t;
        t.handle_key(&with(KeyCode::Home, KeyModifiers::CONTROL));
        let mut buf = Buffer::empty(area);
        t.render(area, &mut buf, true);
        assert_eq!(t.scroll_row(), 0);
        let shown: Vec<String> = crate::testing::buffer_text(&buf);
        assert_eq!(shown, ["line0", "line1", "line2"]);
    }

    #[test]
    fn render_marks_selection_and_caret() {
        let mut t = TextArea::new("abc");
        t.set_cursor(0);
        t.handle_key(&with(KeyCode::Right, KeyModifiers::SHIFT));
        let area = Rect::new(0, 0, 6, 1);
        let mut buf = Buffer::empty(area);
        t.render(area, &mut buf, true);
        assert!(buf[(0, 0)].modifier.contains(Modifier::REVERSED));
        assert!(buf[(1, 0)].modifier.contains(Modifier::REVERSED)); // caret
        assert!(!buf[(2, 0)].modifier.contains(Modifier::REVERSED));
    }
}
