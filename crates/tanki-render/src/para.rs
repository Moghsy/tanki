//! Inline formatting: one paragraph of styled text and atoms, broken into
//! lines (UAX #14), reordered for bidi, aligned, and composed into rows.

use crate::style::{Align, Transform, WhiteSpace};
use crate::text::{grapheme_width, str_width};
use crate::Style;
use std::rc::Rc;
use unicode_bidi::{BidiClass, BidiInfo, Level};
use unicode_segmentation::UnicodeSegmentation;

/// Per-cell information carried through layout. Images and widgets are
/// located from where their cells end up.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Meta {
    pub style: Style,
    pub link: Option<Rc<str>>,
    pub widget: Option<usize>,
    pub image: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct ISpan {
    pub text: String,
    pub meta: Meta,
}

pub type IRow = Vec<ISpan>;

pub fn spaces(n: usize) -> String {
    " ".repeat(n)
}

pub fn push_span(row: &mut IRow, text: &str, meta: &Meta) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = row.last_mut() {
        if &last.meta == meta {
            last.text.push_str(text);
            return;
        }
    }
    row.push(ISpan {
        text: text.to_string(),
        meta: meta.clone(),
    });
}

pub fn row_width(row: &IRow) -> usize {
    row.iter().map(|s| str_width(&s.text)).sum()
}

/// Removes trailing plain spaces (no style, widget or image).
pub fn trim_row_end(row: &mut IRow) {
    while let Some(last) = row.last_mut() {
        if last.meta != Meta::default() {
            break;
        }
        let t = last.text.trim_end_matches(' ').len();
        last.text.truncate(t);
        if last.text.is_empty() {
            row.pop();
        } else {
            break;
        }
    }
}

/// An unbreakable multi-row inline box (an image, or ruby text over its base).
/// Rows are bottom-aligned with the text row of a line.
#[derive(Debug, Clone)]
pub struct Atom {
    pub width: usize,
    pub rows: Vec<IRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TStyle {
    pub meta: Meta,
    pub ws: WhiteSpace,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PK {
    Text(usize),
    Atom(usize),
    Break,
}

#[derive(Debug, Clone, Copy)]
struct Piece {
    end: usize,
    kind: PK,
}

pub struct Para {
    text: String,
    pieces: Vec<Piece>,
    styles: Vec<TStyle>,
    atoms: Vec<Atom>,
    /// The last thing pushed was a collapsible space (or nothing yet).
    last_space: bool,
    has_content: bool,
    pub align: Align,
    pub rtl: bool,
    pub dir_auto: bool,
    /// For `text-transform: capitalize` across pushes.
    prev_alnum: bool,
}

const OBJ: char = '\u{FFFC}';

impl Para {
    pub fn new(align: Align, rtl: bool, dir_auto: bool) -> Self {
        Para {
            text: String::new(),
            pieces: Vec::new(),
            styles: Vec::new(),
            atoms: Vec::new(),
            last_space: true,
            has_content: false,
            align,
            rtl,
            dir_auto,
            prev_alnum: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.has_content
    }

    fn style_index(&mut self, st: &TStyle) -> usize {
        if let Some(i) = self.styles.iter().rposition(|s| s == st) {
            return i;
        }
        self.styles.push(st.clone());
        self.styles.len() - 1
    }

    fn push_raw(&mut self, s: &str, kind: PK) {
        if s.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(s);
        let end = self.text.len();
        if let (Some(last), PK::Text(_)) = (self.pieces.last_mut(), kind) {
            if last.kind == kind && last.end == start {
                last.end = end;
                return;
            }
        }
        self.pieces.push(Piece { end, kind });
    }

    /// Appends text, applying `white-space` collapsing and `text-transform`.
    pub fn push_text(&mut self, s: &str, st: &TStyle, transform: Transform) {
        let idx = self.style_index(st);
        let ws = st.ws;
        let mut buf = String::with_capacity(s.len());
        let mut col_since_nl = 0usize;
        for c in s.chars() {
            let c = match transform {
                Transform::None => c,
                Transform::Upper => c.to_uppercase().next().unwrap_or(c),
                Transform::Lower => c.to_lowercase().next().unwrap_or(c),
                Transform::Capitalize => {
                    let out = if !self.prev_alnum {
                        c.to_uppercase().next().unwrap_or(c)
                    } else {
                        c
                    };
                    self.prev_alnum = c.is_alphanumeric();
                    out
                }
            };
            let is_ws = matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c');
            if is_ws {
                if c == '\n' && ws.keeps_newlines() {
                    self.push_raw(&std::mem::take(&mut buf), PK::Text(idx));
                    self.push_raw("\n", PK::Break);
                    self.last_space = true;
                    self.has_content = true;
                    col_since_nl = 0;
                    continue;
                }
                if c == '\r' {
                    continue;
                }
                if ws.collapses_spaces() {
                    if !self.last_space {
                        buf.push(' ');
                        self.last_space = true;
                    }
                } else if c == '\t' {
                    let n = 8 - (col_since_nl % 8);
                    buf.push_str(&spaces(n));
                    col_since_nl += n;
                    self.last_space = false;
                    self.has_content = true;
                } else {
                    buf.push(' ');
                    col_since_nl += 1;
                    self.last_space = false;
                    self.has_content = true;
                }
                continue;
            }
            if c.is_control() || c == OBJ {
                continue;
            }
            buf.push(c);
            col_since_nl += 1;
            self.last_space = false;
            self.has_content = true;
        }
        self.push_raw(&buf, PK::Text(idx));
    }

    /// Appends text that is laid out but not visible (`visibility: hidden`).
    pub fn push_hidden(&mut self, s: &str, ws: WhiteSpace) {
        let st = TStyle {
            meta: Meta::default(),
            ws,
        };
        let mut collapsed = Para::new(Align::Left, false, false);
        collapsed.last_space = self.last_space;
        collapsed.push_text(s, &st, Transform::None);
        let mut out = String::new();
        for g in collapsed.text.graphemes(true) {
            match g {
                " " => out.push(' '),
                "\n" => out.push('\n'),
                _ => out.push_str(&"\u{a0}".repeat(grapheme_width(g))),
            }
        }
        // Re-push through the normal path so breaks and spacing stay right.
        let idx = self.style_index(&st);
        for (i, part) in out.split('\n').enumerate() {
            if i > 0 {
                self.push_raw("\n", PK::Break);
                self.last_space = true;
            }
            if !part.is_empty() {
                self.push_raw(part, PK::Text(idx));
                self.last_space = part.ends_with(' ');
                self.has_content = true;
            }
        }
    }

    pub fn push_break(&mut self) {
        self.push_raw("\n", PK::Break);
        self.last_space = true;
        self.has_content = true;
    }

    /// A break opportunity without a visible character (`<wbr>`).
    pub fn push_wbr(&mut self, st: &TStyle) {
        let idx = self.style_index(st);
        self.push_raw("\u{200B}", PK::Text(idx));
    }

    pub fn push_atom(&mut self, atom: Atom) {
        self.atoms.push(atom);
        let i = self.atoms.len() - 1;
        let mut b = [0u8; 4];
        self.push_raw(OBJ.encode_utf8(&mut b), PK::Atom(i));
        self.last_space = false;
        self.has_content = true;
    }

    /// Lays the paragraph out in `width` columns. Returns rows with their
    /// left padding (alignment) already applied.
    pub fn layout(&self, width: usize) -> Vec<IRow> {
        if !self.has_content || width == 0 {
            return Vec::new();
        }
        let lines = self.break_lines(width);
        let mut rtl = self.rtl;
        if self.dir_auto {
            rtl = first_strong_rtl(&self.text).unwrap_or(rtl);
        }
        let mut out = Vec::new();
        for mut line in lines {
            if rtl
                || line.iter().any(|u| {
                    u.kind == UK::Text && self.text[u.start..u.end].chars().any(is_rtl_char)
                })
            {
                line = self.reorder(line, rtl);
            }
            let rows = self.compose(&line);
            let w = rows.iter().map(row_width).max().unwrap_or(0);
            let free = width.saturating_sub(w);
            let pad = match self.align {
                Align::Left => 0,
                Align::Right => free,
                Align::Center | Align::WebkitCenter => free / 2,
                Align::Start | Align::Justify => {
                    if rtl {
                        free
                    } else {
                        0
                    }
                }
                Align::End => {
                    if rtl {
                        0
                    } else {
                        free
                    }
                }
            };
            for r in rows {
                let mut row = IRow::new();
                if pad > 0 && !r.is_empty() {
                    push_span(&mut row, &spaces(pad), &Meta::default());
                }
                for s in r {
                    push_span(&mut row, &s.text, &s.meta);
                }
                out.push(row);
            }
        }
        out
    }

    fn units(&self, a: usize, b: usize, cursor: &mut usize, out: &mut Vec<Unit>) {
        for (off, g) in self.text[a..b].grapheme_indices(true) {
            let pos = a + off;
            while *cursor + 1 < self.pieces.len() && self.pieces[*cursor].end <= pos {
                *cursor += 1;
            }
            let Some(piece) = self.pieces.get(*cursor) else {
                continue;
            };
            let (kind, width) = match piece.kind {
                PK::Break => continue,
                PK::Atom(i) => (UK::Atom(i), self.atoms[i].width),
                PK::Text(_) => {
                    if g == " " {
                        (UK::Space, 1)
                    } else {
                        let w = grapheme_width(g);
                        if w == 0
                            && g.chars()
                                .all(|c| c == '\u{200B}' || c == '\u{AD}' || c.is_control())
                        {
                            continue;
                        }
                        (UK::Text, w)
                    }
                }
            };
            out.push(Unit {
                start: pos,
                end: pos + g.len(),
                piece: *cursor,
                width,
                kind,
            });
        }
    }

    fn piece_wraps(&self, piece: usize) -> bool {
        match self.pieces.get(piece).map(|p| p.kind) {
            Some(PK::Text(s)) => self.styles[s].ws.wraps(),
            _ => true,
        }
    }

    fn piece_collapses(&self, piece: usize) -> bool {
        match self.pieces.get(piece).map(|p| p.kind) {
            Some(PK::Text(s)) => self.styles[s].ws.collapses_spaces(),
            _ => true,
        }
    }

    fn piece_index_at(&self, byte: usize) -> usize {
        self.pieces
            .partition_point(|p| p.end <= byte)
            .min(self.pieces.len().saturating_sub(1))
    }

    fn break_lines(&self, width: usize) -> Vec<Vec<Unit>> {
        let mut lines: Vec<Vec<Unit>> = Vec::new();
        let mut cur: Vec<Unit> = Vec::new();
        let mut cur_w = 0usize;
        let mut prev = 0usize;
        let mut cursor = 0usize;
        let mut seg: Vec<Unit> = Vec::new();
        for (pos, op) in unicode_linebreak::linebreaks(&self.text) {
            let mandatory = op == unicode_linebreak::BreakOpportunity::Mandatory;
            if !mandatory && pos < self.text.len() {
                // No soft wrap inside `nowrap`/`pre` text.
                let before = self.text[..pos]
                    .chars()
                    .next_back()
                    .map(|c| pos - c.len_utf8())
                    .unwrap_or(0);
                if !self.piece_wraps(self.piece_index_at(before)) {
                    continue;
                }
            }
            seg.clear();
            self.units(prev, pos, &mut cursor, &mut seg);
            prev = pos;
            let core_len = seg
                .iter()
                .rposition(|u| u.kind != UK::Space)
                .map(|i| i + 1)
                .unwrap_or(0);
            let w_core: usize = seg[..core_len].iter().map(|u| u.width).sum();
            let w_full: usize = seg.iter().map(|u| u.width).sum();
            if cur_w + w_core <= width {
                cur_w += w_full;
                cur.append(&mut seg);
            } else {
                if !cur.is_empty() {
                    lines.push(self.finish_line(std::mem::take(&mut cur), width));
                    cur_w = 0;
                }
                if w_core <= width {
                    cur_w = w_full;
                    cur.append(&mut seg);
                } else {
                    // overflow-wrap: break-word (reviewer `body` rule).
                    for mut u in seg.drain(..) {
                        if u.kind == UK::Space && cur.is_empty() && !lines.is_empty() {
                            continue;
                        }
                        if cur_w + u.width > width && !cur.is_empty() {
                            lines.push(self.finish_line(std::mem::take(&mut cur), width));
                            cur_w = 0;
                        }
                        if u.width > width {
                            u.kind = UK::Ellipsis;
                            u.width = 1;
                        }
                        cur_w += u.width;
                        cur.push(u);
                    }
                }
            }
            if mandatory {
                lines.push(self.finish_line(std::mem::take(&mut cur), width));
                cur_w = 0;
            }
        }
        if !cur.is_empty() {
            lines.push(self.finish_line(cur, width));
        }
        lines
    }

    fn finish_line(&self, mut line: Vec<Unit>, width: usize) -> Vec<Unit> {
        let mut w: usize = line.iter().map(|u| u.width).sum();
        while let Some(last) = line.last() {
            if last.kind == UK::Space && (self.piece_collapses(last.piece) || w > width) {
                w -= last.width;
                line.pop();
            } else {
                break;
            }
        }
        line
    }

    fn reorder(&self, line: Vec<Unit>, rtl: bool) -> Vec<Unit> {
        let mut s = String::new();
        let mut offsets = Vec::with_capacity(line.len());
        for u in &line {
            offsets.push(s.len());
            match u.kind {
                UK::Atom(_) => s.push(OBJ),
                UK::Ellipsis => s.push('…'),
                _ => s.push_str(&self.text[u.start..u.end]),
            }
        }
        if s.is_empty() {
            return line;
        }
        let base = if rtl { Level::rtl() } else { Level::ltr() };
        let info = BidiInfo::new(&s, Some(base));
        let levels: Vec<Level> = offsets.iter().map(|&o| info.levels[o]).collect();
        let order = BidiInfo::reorder_visual(&levels);
        let mut slots: Vec<Option<Unit>> = line.into_iter().map(Some).collect();
        order.into_iter().filter_map(|i| slots[i].take()).collect()
    }

    fn compose(&self, line: &[Unit]) -> Vec<IRow> {
        let h = line
            .iter()
            .map(|u| match u.kind {
                UK::Atom(i) => self.atoms[i].rows.len().max(1),
                _ => 1,
            })
            .max()
            .unwrap_or(1);
        let mut rows: Vec<IRow> = vec![IRow::new(); h];
        let blank = Meta::default();
        for u in line {
            match u.kind {
                UK::Atom(i) => {
                    let atom = &self.atoms[i];
                    let ah = atom.rows.len().max(1);
                    for (r, row) in rows.iter_mut().enumerate() {
                        if r + ah >= h && r + ah - h < atom.rows.len() {
                            let src = &atom.rows[r + ah - h];
                            for s in src {
                                push_span(row, &s.text, &s.meta);
                            }
                            let pad = atom.width.saturating_sub(row_width(src));
                            push_span(row, &spaces(pad), &blank);
                        } else {
                            push_span(row, &spaces(atom.width), &blank);
                        }
                    }
                }
                _ => {
                    let meta = match self.pieces[u.piece].kind {
                        PK::Text(s) => &self.styles[s].meta,
                        _ => &blank,
                    };
                    let text = match u.kind {
                        UK::Ellipsis => "…",
                        _ => &self.text[u.start..u.end],
                    };
                    push_span(&mut rows[h - 1], text, meta);
                    for row in rows.iter_mut().take(h - 1) {
                        push_span(row, &spaces(u.width), &blank);
                    }
                }
            }
        }
        for row in rows.iter_mut() {
            trim_row_end(row);
        }
        rows
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum UK {
    Text,
    Space,
    Atom(usize),
    /// A grapheme too wide for the line, shown as `…`.
    Ellipsis,
}

#[derive(Debug, Clone, Copy)]
struct Unit {
    start: usize,
    end: usize,
    piece: usize,
    width: usize,
    kind: UK,
}

pub fn is_rtl_char(c: char) -> bool {
    matches!(unicode_bidi::bidi_class(c), BidiClass::R | BidiClass::AL)
}

fn first_strong_rtl(s: &str) -> Option<bool> {
    for c in s.chars() {
        match unicode_bidi::bidi_class(c) {
            BidiClass::R | BidiClass::AL => return Some(true),
            BidiClass::L => return Some(false),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> TStyle {
        TStyle {
            meta: Meta::default(),
            ws: WhiteSpace::Normal,
        }
    }

    fn text_rows(rows: &[IRow]) -> Vec<String> {
        rows.iter()
            .map(|r| r.iter().map(|s| s.text.as_str()).collect())
            .collect()
    }

    #[test]
    fn collapses_and_wraps() {
        let mut p = Para::new(Align::Left, false, false);
        p.push_text(
            "  hello   world\n  and more text ",
            &plain(),
            Transform::None,
        );
        assert_eq!(
            text_rows(&p.layout(11)),
            vec!["hello world", "and more", "text"]
        );
    }

    #[test]
    fn breaks_and_empty_lines() {
        let mut p = Para::new(Align::Left, false, false);
        p.push_text("a", &plain(), Transform::None);
        p.push_break();
        p.push_break();
        p.push_text("b", &plain(), Transform::None);
        p.push_break();
        assert_eq!(text_rows(&p.layout(10)), vec!["a", "", "b"]);
        let mut only = Para::new(Align::Left, false, false);
        only.push_break();
        assert_eq!(text_rows(&only.layout(10)), vec![""]);
    }

    #[test]
    fn centred_and_cjk() {
        let mut p = Para::new(Align::Center, false, false);
        p.push_text("日本語を勉強する", &plain(), Transform::None);
        let rows = p.layout(9);
        assert_eq!(text_rows(&rows), vec!["日本語を", "勉強する"]);
        let mut p = Para::new(Align::Center, false, false);
        p.push_text("ab", &plain(), Transform::None);
        assert_eq!(text_rows(&p.layout(6)), vec!["  ab"]);
    }

    #[test]
    fn long_words_break_and_wide_chars_fit() {
        let mut p = Para::new(Align::Left, false, false);
        p.push_text("abcdefghij", &plain(), Transform::None);
        assert_eq!(text_rows(&p.layout(4)), vec!["abcd", "efgh", "ij"]);
        let mut p = Para::new(Align::Left, false, false);
        p.push_text("日本", &plain(), Transform::None);
        assert_eq!(text_rows(&p.layout(1)), vec!["…", "…"]);
    }

    #[test]
    fn pre_keeps_spaces() {
        let st = TStyle {
            meta: Meta::default(),
            ws: WhiteSpace::Pre,
        };
        let mut p = Para::new(Align::Left, false, false);
        p.push_text("a  b\n\tc", &st, Transform::None);
        assert_eq!(text_rows(&p.layout(20)), vec!["a  b", "        c"]);
    }

    #[test]
    fn rtl_reordered() {
        let mut p = Para::new(Align::Start, true, false);
        p.push_text("שלום", &plain(), Transform::None);
        let rows = p.layout(6);
        assert_eq!(text_rows(&rows), vec!["  םולש"]);
    }

    #[test]
    fn atoms_bottom_aligned() {
        let mut p = Para::new(Align::Left, false, false);
        p.push_text("x ", &plain(), Transform::None);
        let meta = Meta {
            image: Some(0),
            ..Meta::default()
        };
        p.push_atom(Atom {
            width: 3,
            rows: vec![
                vec![ISpan {
                    text: "   ".into(),
                    meta: meta.clone(),
                }],
                vec![ISpan {
                    text: "   ".into(),
                    meta,
                }],
            ],
        });
        p.push_text(" y", &plain(), Transform::None);
        let rows = p.layout(20);
        assert_eq!(text_rows(&rows), vec!["     ", "x     y"]);
    }
}
