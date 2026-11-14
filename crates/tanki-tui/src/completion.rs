//! A completion popup over [`TextInput`], shared by the browser's search bar
//! and tag inputs (and the add screen's tag field).
//!
//! The caller owns the input and supplies candidates through a
//! [`CompletionSource`] (any `FnMut(&str, usize) -> Option<Proposal>` works):
//! given the text and the cursor's byte offset, it returns the byte range the
//! completion replaces and the candidates. [`Completion::handle_key`] routes a
//! key to the popup or the input: Tab / Down / Ctrl-N next, Shift-Tab / Up /
//! Ctrl-P previous, Enter accepts, Esc dismisses; any other key edits the
//! input and refilters. Keys it does not use come back as
//! [`CompletionOutcome::Ignored`] (Enter with the popup closed, Up/Down for
//! history, Esc to leave the field). [`Completion::render`] draws the popup
//! under (or above) the input's row.
//!
//! [`SearchVocabulary`] is the search bar's source: Anki's search keys (R6
//! §1.3), deck / tag / note type / card names after their key, quoted when
//! needed, and matching entries of the search history.
use crate::input::TextInput;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};
use std::ops::Range;
use unicode_width::UnicodeWidthStr;

/// One proposal: `text` replaces the range; `detail` is a dim label
/// ("deck", "history", …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub text: String,
    pub detail: String,
}

impl Candidate {
    pub fn new(text: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            detail: detail.into(),
        }
    }
}

/// Candidates for the byte `range` of the input's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub range: Range<usize>,
    pub candidates: Vec<Candidate>,
}

/// Supplies candidates for `text` with the cursor at byte `cursor`.
pub trait CompletionSource {
    fn propose(&mut self, text: &str, cursor: usize) -> Option<Proposal>;
}

impl<F: FnMut(&str, usize) -> Option<Proposal>> CompletionSource for F {
    fn propose(&mut self, text: &str, cursor: usize) -> Option<Proposal> {
        self(text, cursor)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionOutcome {
    /// Not used: the caller handles the key.
    Ignored,
    /// The popup moved, opened or closed.
    Handled,
    /// The input changed (the popup was refiltered).
    Edited,
    /// A candidate was inserted.
    Accepted,
}

/// Rows of candidates shown at once.
const VISIBLE: usize = 8;

#[derive(Debug, Clone, Default)]
pub struct Completion {
    proposal: Option<Proposal>,
    selected: usize,
    /// Open by itself while typing, whenever the source has candidates
    /// for a non-empty token.
    auto_open: bool,
}

/// Byte offset of the input's cursor (the input exposes its display column).
pub fn cursor_byte(input: &TextInput) -> usize {
    let column = input.cursor_column();
    let mut width = 0;
    for (i, c) in input.text().char_indices() {
        if width >= column {
            return i;
        }
        width += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
    }
    input.text().len()
}

/// Replaces `range` of the input's text with `with`; the cursor ends after it.
pub fn replace_range(input: &mut TextInput, range: Range<usize>, with: &str) {
    let text = input.text();
    let start = range.start.min(text.len());
    let end = range.end.clamp(start, text.len());
    let after = text[end..].to_owned();
    let mut new = TextInput::new(format!("{}{with}", &text[..start]));
    new.insert_str(&after);
    for _ in after.chars() {
        new.handle_key(&KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    }
    *input = new;
}

impl Completion {
    /// `auto_open`: show the popup while typing (the search bar); otherwise
    /// only Tab opens it.
    pub fn new(auto_open: bool) -> Self {
        Self {
            auto_open,
            ..Default::default()
        }
    }
    pub fn is_open(&self) -> bool {
        self.proposal.is_some()
    }
    pub fn close(&mut self) {
        self.proposal = None;
        self.selected = 0;
    }
    pub fn candidates(&self) -> &[Candidate] {
        self.proposal.as_ref().map_or(&[], |p| &p.candidates)
    }
    pub fn selected(&self) -> Option<&Candidate> {
        self.candidates().get(self.selected)
    }

    /// Asks the source; opens when it has candidates. Returns whether open.
    pub fn open(&mut self, input: &TextInput, source: &mut dyn CompletionSource) -> bool {
        let previous = self.selected().map(|c| c.text.clone());
        self.proposal = source
            .propose(input.text(), cursor_byte(input))
            .filter(|p| !p.candidates.is_empty());
        // Keep the highlighted candidate when it is still offered.
        self.selected = previous
            .and_then(|text| self.candidates().iter().position(|c| c.text == text))
            .unwrap_or(0);
        self.is_open()
    }

    /// Refilters after an edit: an open popup follows the text (closing when
    /// nothing matches); a closed one opens only with `auto_open` and a
    /// non-empty token.
    pub fn refresh(&mut self, input: &TextInput, source: &mut dyn CompletionSource) {
        if self.is_open() {
            self.open(input, source);
        } else if self.auto_open {
            let cursor = cursor_byte(input);
            if let Some(p) = source.propose(input.text(), cursor) {
                if !p.candidates.is_empty() && p.range.start < p.range.end {
                    self.proposal = Some(p);
                    self.selected = 0;
                }
            }
        }
    }

    fn step(&mut self, forward: bool) {
        let n = self.candidates().len();
        if n > 0 {
            self.selected = if forward {
                (self.selected + 1) % n
            } else {
                (self.selected + n - 1) % n
            };
        }
    }

    /// Inserts the highlighted candidate. A candidate ending in `:` (a
    /// search key) leaves the popup open for the value.
    pub fn accept(&mut self, input: &mut TextInput, source: &mut dyn CompletionSource) -> bool {
        let Some(proposal) = self.proposal.take() else {
            return false;
        };
        let Some(candidate) = proposal.candidates.get(self.selected) else {
            return false;
        };
        replace_range(input, proposal.range.clone(), &candidate.text);
        self.selected = 0;
        if candidate.text.ends_with(':') {
            self.open(input, source);
        }
        true
    }

    pub fn handle_key(
        &mut self,
        key: &KeyEvent,
        input: &mut TextInput,
        source: &mut dyn CompletionSource,
    ) -> CompletionOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.is_open() {
            match key.code {
                KeyCode::Tab | KeyCode::Down => self.step(true),
                KeyCode::Char('n') if ctrl => self.step(true),
                KeyCode::BackTab | KeyCode::Up => self.step(false),
                KeyCode::Char('p') if ctrl => self.step(false),
                KeyCode::Enter => {
                    self.accept(input, source);
                    return CompletionOutcome::Accepted;
                }
                KeyCode::Esc => self.close(),
                _ => {
                    if !input.handle_key(key) {
                        return CompletionOutcome::Ignored;
                    }
                    self.refresh(input, source);
                    return CompletionOutcome::Edited;
                }
            }
            return CompletionOutcome::Handled;
        }
        if key.code == KeyCode::Tab {
            return if self.open(input, source) {
                CompletionOutcome::Handled
            } else {
                CompletionOutcome::Ignored
            };
        }
        if input.handle_key(key) {
            self.refresh(input, source);
            CompletionOutcome::Edited
        } else {
            CompletionOutcome::Ignored
        }
    }

    /// Draws the popup for an input drawn at `anchor` (its row; the text
    /// starts at `anchor.x`), inside `bounds`.
    pub fn render(&self, input: &TextInput, anchor: Rect, bounds: Rect, buf: &mut Buffer) {
        let Some(proposal) = &self.proposal else {
            return;
        };
        let items = &proposal.candidates;
        let rows = items.len().min(VISIBLE) as u16;
        let text_width = items
            .iter()
            .map(|c| {
                c.text.width()
                    + if c.detail.is_empty() {
                        0
                    } else {
                        c.detail.width() + 2
                    }
            })
            .max()
            .unwrap_or(0) as u16;
        let width = (text_width + 2).max(12).min(bounds.width);
        let height = (rows + 2).min(bounds.height);
        if width < 4 || height < 3 {
            return;
        }
        let start = proposal.range.start.min(input.text().len());
        let column = input.text()[..start].width() as u16;
        let x = (anchor.x + column)
            .min(bounds.right().saturating_sub(width))
            .max(bounds.x);
        let below = anchor.y + 1;
        let y = if below + height <= bounds.bottom() {
            below
        } else {
            anchor.y.saturating_sub(height).max(bounds.y)
        };
        let area = Rect {
            x,
            y,
            width,
            height,
        };
        let inner_rows = (height - 2) as usize;
        let first = self.selected.saturating_sub(inner_rows.saturating_sub(1));
        let dim = Style::default().add_modifier(Modifier::DIM);
        let lines: Vec<Line> = items
            .iter()
            .enumerate()
            .skip(first)
            .take(inner_rows)
            .map(|(i, c)| {
                let mut spans = vec![Span::raw(c.text.clone())];
                if !c.detail.is_empty() {
                    let pad = (width as usize - 2)
                        .saturating_sub(c.text.width() + c.detail.width())
                        .max(1);
                    spans.push(Span::raw(" ".repeat(pad)));
                    spans.push(Span::styled(c.detail.clone(), dim));
                }
                let line = Line::from(spans);
                if i == self.selected {
                    line.style(Style::default().add_modifier(Modifier::REVERSED))
                } else {
                    line
                }
            })
            .collect();
        Clear.render(area, buf);
        let title = if items.len() > inner_rows {
            format!(" {}/{} ", self.selected + 1, items.len())
        } else {
            String::new()
        };
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(title))
            .render(area, buf);
    }
}

// ---- search terms -------------------------------------------------------------

/// Start of the search term that ends at `cursor`: after the last space or
/// parenthesis outside quotes, and after a leading `-`.
pub fn search_token(text: &str, cursor: usize) -> Range<usize> {
    let cursor = cursor.min(text.len());
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (i, c) in text[..cursor].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '"' => quoted = !quoted,
            c if !quoted && (c.is_whitespace() || c == '(' || c == ')') => start = i + c.len_utf8(),
            _ => {}
        }
    }
    if text[start..cursor].starts_with('-') {
        start += 1;
    }
    start..cursor
}

/// The space-separated word ending at `cursor` (tag inputs).
pub fn word_token(text: &str, cursor: usize) -> Range<usize> {
    let cursor = cursor.min(text.len());
    let start = text[..cursor]
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(i, c)| i + c.len_utf8());
    start..cursor
}

/// `key:value` as a search term matching `value` literally: `\`, `"`, `*`
/// and `_` escaped, the whole term quoted when the value has a space or
/// parenthesis (the form Anki's normaliser writes).
pub fn search_term(key: &str, value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '\\' | '"' | '*' | '_') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    if value.contains(|c: char| c.is_whitespace() || c == '(' || c == ')') {
        format!("\"{key}:{escaped}\"")
    } else {
        format!("{key}:{escaped}")
    }
}

/// Search keys and complete terms (R6 §1.3), as offered before a colon.
const KEYS: &[(&str, &str)] = &[
    ("deck:", "deck and subdecks"),
    ("tag:", "tag and children"),
    ("note:", "note type"),
    ("card:", "card type or number"),
    ("is:due", "due today"),
    ("is:new", "new cards"),
    ("is:learn", "learning"),
    ("is:review", "review cards"),
    ("is:suspended", "suspended"),
    ("is:buried", "buried"),
    ("flag:", "0 none, 1-7"),
    ("prop:", "card property"),
    ("added:", "added in N days"),
    ("edited:", "edited in N days"),
    ("rated:", "answered in N days"),
    ("introduced:", "first review in N days"),
    ("resched:", "rescheduled in N days"),
    ("preset:", "deck options preset"),
    ("dupe:", "duplicates"),
    ("re:", "regex"),
    ("nc:", "ignore accents"),
    ("sc:", "strip cloze"),
    ("w:", "whole word"),
    ("nid:", "note ids"),
    ("cid:", "card ids"),
    ("mid:", "note type id"),
    ("did:", "deck ids"),
    ("has-cd:", "custom data key"),
];

const IS_VALUES: &[&str] = &[
    "due",
    "new",
    "learn",
    "review",
    "suspended",
    "buried",
    "buried-manually",
    "buried-sibling",
];
const PROP_VALUES: &[(&str, &str)] = &[
    ("ivl", "interval days"),
    ("due", "days from today"),
    ("reps", "reviews"),
    ("lapses", "lapses"),
    ("ease", "ease, e.g. 2.5"),
    ("pos", "new position"),
    ("rated", "days ago, <= 0"),
    ("resched", "days ago, <= 0"),
    ("s", "FSRS stability"),
    ("d", "FSRS difficulty"),
    ("r", "FSRS retrievability"),
];
const FLAG_VALUES: &[(&str, &str)] = &[
    ("0", "no flag"),
    ("1", "red"),
    ("2", "orange"),
    ("3", "green"),
    ("4", "blue"),
    ("5", "pink"),
    ("6", "turquoise"),
    ("7", "purple"),
];
const LIMIT: usize = 50;

/// Names the search bar completes; the browser fills it from its sidebar
/// data and the profile's search history.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchVocabulary {
    /// Full deck names (`A::B`).
    pub decks: Vec<String>,
    /// Full tag names.
    pub tags: Vec<String>,
    pub notetypes: Vec<String>,
    /// Card (template) names across note types.
    pub templates: Vec<String>,
    /// Newest first (`ProfileConfig::search_history`).
    pub history: Vec<String>,
}

/// Case-insensitive matches of `prefix`: prefix matches first, then
/// substring matches, each in the given order.
fn matching<'a>(names: impl Iterator<Item = &'a str>, prefix: &str) -> Vec<&'a str> {
    let wanted = prefix.to_lowercase();
    let mut starts = Vec::new();
    let mut contains = Vec::new();
    for name in names {
        let lower = name.to_lowercase();
        if lower.starts_with(&wanted) {
            starts.push(name);
        } else if lower.contains(&wanted) {
            contains.push(name);
        }
    }
    starts.extend(contains);
    starts
}

/// Undoes the escapes `search_term` adds, for matching typed values.
fn unescape(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else if c != '"' {
            out.push(c);
        }
    }
    out
}

impl SearchVocabulary {
    pub fn propose(&self, text: &str, cursor: usize) -> Option<Proposal> {
        let range = search_token(text, cursor);
        let token = &text[range.clone()];
        let mut candidates: Vec<Candidate> = Vec::new();
        let bare = token.strip_prefix('"').unwrap_or(token);
        if let Some((key, value)) = bare.split_once(':') {
            let value = unescape(value);
            let key_lower = key.to_ascii_lowercase();
            let names = |list: &[String], detail: &str| {
                matching(list.iter().map(String::as_str), &value)
                    .into_iter()
                    .map(|name| Candidate::new(search_term(&key_lower, name), detail))
                    .collect::<Vec<_>>()
            };
            let fixed = |list: &[(&str, &str)]| {
                matching(list.iter().map(|(v, _)| *v), &value)
                    .into_iter()
                    .map(|v| {
                        let detail = list.iter().find(|(x, _)| *x == v).map_or("", |(_, d)| d);
                        Candidate::new(format!("{key_lower}:{v}"), detail)
                    })
                    .collect::<Vec<_>>()
            };
            candidates = match key_lower.as_str() {
                "deck" => names(&self.decks, "deck"),
                "tag" => names(&self.tags, "tag"),
                "note" => names(&self.notetypes, "note type"),
                "card" => names(&self.templates, "card"),
                "is" => matching(IS_VALUES.iter().copied(), &value)
                    .into_iter()
                    .map(|v| Candidate::new(format!("is:{v}"), "state"))
                    .collect(),
                "flag" => fixed(FLAG_VALUES),
                "prop" => fixed(PROP_VALUES),
                _ => Vec::new(),
            };
        } else if !token.is_empty() {
            candidates = matching(KEYS.iter().map(|(k, _)| *k), token)
                .into_iter()
                .filter(|k| k.to_lowercase().starts_with(&token.to_lowercase()))
                .map(|k| {
                    let detail = KEYS.iter().find(|(x, _)| *x == k).map_or("", |(_, d)| d);
                    Candidate::new(k, detail)
                })
                .collect();
        }
        candidates.retain(|c| c.text != token);
        candidates.truncate(LIMIT);
        // The whole query from history, when the cursor is at the end and
        // the query has several terms (or no term matches).
        let whole = text.trim();
        if cursor >= text.len() && !whole.is_empty() {
            let lower = whole.to_lowercase();
            let history: Vec<Candidate> = self
                .history
                .iter()
                .filter(|h| h.to_lowercase().starts_with(&lower) && h.as_str() != whole)
                .take(5)
                .map(|h| Candidate::new(h.clone(), "history"))
                .collect();
            // A query of several terms completes from history first; a
            // single term completes the term.
            if !history.is_empty() && (candidates.is_empty() || range.start > 0) {
                return Some(Proposal {
                    range: 0..text.len(),
                    candidates: history,
                });
            }
        }
        Some(Proposal { range, candidates })
    }
}

impl CompletionSource for SearchVocabulary {
    fn propose(&mut self, text: &str, cursor: usize) -> Option<Proposal> {
        SearchVocabulary::propose(self, text, cursor)
    }
}

impl CompletionSource for &SearchVocabulary {
    fn propose(&mut self, text: &str, cursor: usize) -> Option<Proposal> {
        SearchVocabulary::propose(self, text, cursor)
    }
}

/// A source completing the word at the cursor from a fixed list (tag
/// inputs when the caller already has the tags; deck names).
pub struct ListSource<'a> {
    pub items: &'a [String],
    pub detail: &'a str,
    /// Complete the whole input (a deck name with spaces) instead of the
    /// space-separated word.
    pub whole_input: bool,
}

impl CompletionSource for ListSource<'_> {
    fn propose(&mut self, text: &str, cursor: usize) -> Option<Proposal> {
        let range = if self.whole_input {
            0..text.len()
        } else {
            word_token(text, cursor)
        };
        let token = &text[range.clone()];
        let candidates = matching(self.items.iter().map(String::as_str), token)
            .into_iter()
            .filter(|c| *c != token)
            .take(LIMIT)
            .map(|c| Candidate::new(c, self.detail))
            .collect();
        Some(Proposal { range, candidates })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::buffer_text;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn vocab() -> SearchVocabulary {
        SearchVocabulary {
            decks: vec!["Default".into(), "Lang::French words".into(), "Lang".into()],
            tags: vec!["leech".into(), "lang::fr".into()],
            notetypes: vec!["Basic".into(), "Cloze".into()],
            templates: vec!["Card 1".into()],
            history: vec!["deck:Lang is:due".into(), "tag:leech".into()],
        }
    }

    fn typed(text: &str) -> TextInput {
        TextInput::new(text)
    }

    #[test]
    fn tokens_respect_quotes_parens_and_negation() {
        assert_eq!(search_token("a deck:x", 8), 2..8);
        assert_eq!(search_token("(-tag:a", 7), 2..7);
        assert_eq!(search_token("\"deck:a b", 9), 0..9);
        assert_eq!(search_token("x \\\"y z", 7), 6..7);
        assert_eq!(word_token("one tw", 6), 4..6);
        assert_eq!(
            search_term("deck", "Lang::French words"),
            "\"deck:Lang::French words\""
        );
        assert_eq!(search_term("tag", "a_b*"), "tag:a\\_b\\*");
        assert_eq!(unescape("a\\_b"), "a_b");
    }

    #[test]
    fn search_vocabulary_offers_keys_values_and_history() {
        let v = vocab();
        let texts = |text: &str| {
            v.propose(text, text.len())
                .unwrap()
                .candidates
                .into_iter()
                .map(|c| c.text)
                .collect::<Vec<_>>()
        };
        assert_eq!(texts("de"), ["deck:"]);
        // Prefix matches first, then substring matches.
        assert_eq!(texts("is:d")[..2], ["is:due", "is:suspended"]);
        assert_eq!(
            texts("deck:la"),
            ["\"deck:Lang::French words\"", "deck:Lang"]
        );
        assert_eq!(texts("-tag:fr"), ["tag:lang::fr"]);
        assert_eq!(texts("note:cl"), ["note:Cloze"]);
        assert_eq!(texts("flag:").len(), 8);
        // A complete term offers nothing.
        assert!(texts("flag:1").is_empty());
        assert!(texts("prop:").contains(&"prop:ivl".to_string()));
        // History replaces the whole input when no term matches.
        let p = v.propose("deck:Lang i", 11).unwrap();
        assert_eq!(p.range, 0..11);
        assert_eq!(p.candidates[0].text, "deck:Lang is:due");
        assert!(texts("").is_empty());
    }

    #[test]
    fn keys_cycle_accept_and_dismiss() {
        let mut v = vocab();
        let mut input = typed("tag:");
        let mut c = Completion::new(false);
        // Typing does not open a manual popup; Tab does.
        assert_eq!(
            c.handle_key(&key(KeyCode::Char('l')), &mut input, &mut v),
            CompletionOutcome::Edited
        );
        assert!(!c.is_open());
        assert_eq!(
            c.handle_key(&key(KeyCode::Tab), &mut input, &mut v),
            CompletionOutcome::Handled
        );
        assert_eq!(c.selected().unwrap().text, "tag:leech");
        c.handle_key(&key(KeyCode::Tab), &mut input, &mut v);
        assert_eq!(c.selected().unwrap().text, "tag:lang::fr");
        c.handle_key(&key(KeyCode::BackTab), &mut input, &mut v);
        c.handle_key(&key(KeyCode::Up), &mut input, &mut v);
        assert_eq!(c.selected().unwrap().text, "tag:lang::fr");
        assert_eq!(
            c.handle_key(&key(KeyCode::Enter), &mut input, &mut v),
            CompletionOutcome::Accepted
        );
        assert_eq!(input.text(), "tag:lang::fr");
        assert!(!c.is_open());
        // Esc closes the popup but leaves the text; Enter then is the caller's.
        c.handle_key(&key(KeyCode::Tab), &mut input, &mut v);
        assert!(!c.is_open(), "nothing else matches");
        input = typed("is:");
        c.handle_key(&key(KeyCode::Tab), &mut input, &mut v);
        assert!(c.is_open());
        assert_eq!(
            c.handle_key(&key(KeyCode::Esc), &mut input, &mut v),
            CompletionOutcome::Handled
        );
        assert_eq!(
            c.handle_key(&key(KeyCode::Enter), &mut input, &mut v),
            CompletionOutcome::Ignored
        );
        assert_eq!(input.text(), "is:");
    }

    #[test]
    fn auto_open_follows_typing_and_keys_reopen_for_values() {
        let mut v = vocab();
        let mut input = typed("a ");
        let mut c = Completion::new(true);
        for ch in "de".chars() {
            c.handle_key(&key(KeyCode::Char(ch)), &mut input, &mut v);
        }
        assert_eq!(c.selected().unwrap().text, "deck:");
        c.handle_key(&key(KeyCode::Enter), &mut input, &mut v);
        assert_eq!(input.text(), "a deck:");
        // Accepting a key reopens for its values.
        assert!(c.is_open());
        assert_eq!(c.candidates().len(), 3);
        c.handle_key(&key(KeyCode::Char('z')), &mut input, &mut v);
        assert!(!c.is_open());
        // Replacing in the middle keeps the text after the cursor.
        let mut input = typed("tag:le is:due");
        for _ in 0..7 {
            input.handle_key(&key(KeyCode::Left));
        }
        assert_eq!(cursor_byte(&input), 6);
        c.handle_key(&key(KeyCode::Tab), &mut input, &mut v);
        c.handle_key(&key(KeyCode::Enter), &mut input, &mut v);
        assert_eq!(input.text(), "tag:leech is:due");
        assert_eq!(cursor_byte(&input), 9);
    }

    #[test]
    fn list_source_and_closure_sources() {
        let tags = vec![
            "alpha".to_string(),
            "beta".to_string(),
            "alphabet".to_string(),
        ];
        let mut source = ListSource {
            items: &tags,
            detail: "tag",
            whole_input: false,
        };
        let mut input = typed("beta al");
        let mut c = Completion::new(false);
        c.handle_key(&key(KeyCode::Tab), &mut input, &mut source);
        assert_eq!(c.candidates().len(), 2);
        c.handle_key(&key(KeyCode::Down), &mut input, &mut source);
        c.handle_key(&key(KeyCode::Enter), &mut input, &mut source);
        assert_eq!(input.text(), "beta alphabet");
        let mut closure = |text: &str, cursor: usize| {
            Some(Proposal {
                range: 0..cursor,
                candidates: vec![Candidate::new(text.to_uppercase(), "")],
            })
        };
        c.handle_key(&key(KeyCode::Tab), &mut input, &mut closure);
        c.handle_key(&key(KeyCode::Enter), &mut input, &mut closure);
        assert_eq!(input.text(), "BETA ALPHABET");
    }

    #[test]
    fn popup_snapshot_below_and_above() {
        let mut v = vocab();
        let mut input = typed("x deck:");
        let mut c = Completion::new(false);
        c.handle_key(&key(KeyCode::Tab), &mut input, &mut v);
        c.handle_key(&key(KeyCode::Down), &mut input, &mut v);
        let area = Rect::new(0, 0, 40, 7);
        let mut buf = Buffer::empty(area);
        c.render(&input, Rect::new(0, 0, 40, 1), area, &mut buf);
        assert_eq!(
            buffer_text(&buf),
            [
                "",
                "  ┌───────────────────────────────┐",
                "  │deck:Default               deck│",
                "  │\"deck:Lang::French words\"  deck│",
                "  │deck:Lang                  deck│",
                "  └───────────────────────────────┘",
                "",
            ]
        );
        assert!(buf[(3, 3)].modifier.contains(Modifier::REVERSED));
        // No room below: drawn above the input row.
        let mut buf = Buffer::empty(area);
        c.render(&input, Rect::new(0, 6, 40, 1), area, &mut buf);
        assert_eq!(buffer_text(&buf)[1], "  ┌───────────────────────────────┐");
        assert_eq!(buffer_text(&buf)[6], "");
    }
}
