//! Small modal dialogs of the browser: one-line prompts (with completion),
//! choice lists, the column chooser, the forget / reposition / find and
//! replace forms and confirmations. Each dialog edits its own state; the
//! browser reads the values when [`Outcome::Submit`] comes back.
use crate::completion::{Completion, CompletionOutcome, ListSource};
use crate::input::TextInput;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Open,
    Cancel,
    Submit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    ChangeDeck,
    AddTags,
    RemoveTags,
    SetDue,
    SaveSearch,
}

#[derive(Debug, Clone)]
pub struct Prompt {
    pub kind: PromptKind,
    pub title: String,
    pub help: String,
    pub input: TextInput,
    pub completion: Completion,
    /// Completion candidates (deck or tag names); empty for none.
    pub items: Vec<String>,
    /// Complete the whole input (deck names) rather than the last word.
    pub whole_input: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceKind {
    Sort,
    GradeNow,
}

#[derive(Debug, Clone)]
pub struct Choice {
    pub kind: ChoiceKind,
    pub title: String,
    pub options: Vec<String>,
    pub cursor: usize,
}

/// Column chooser: Space toggles the column under the cursor (the browser
/// applies it at once and keeps the dialog open).
#[derive(Debug, Clone)]
pub struct Columns {
    /// (key, label, active)
    pub items: Vec<(String, String, bool)>,
    pub cursor: usize,
}

#[derive(Debug, Clone)]
pub struct Forget {
    pub restore_position: bool,
    pub reset_counts: bool,
    pub cursor: usize,
    pub count: usize,
}

#[derive(Debug, Clone)]
pub struct Reposition {
    pub start: TextInput,
    pub step: TextInput,
    pub randomize: bool,
    pub shift: bool,
    pub cursor: usize,
    pub count: usize,
}

#[derive(Debug, Clone)]
pub struct FindReplaceForm {
    pub find: TextInput,
    pub replace: TextInput,
    /// "All fields", "Tags", then field names.
    pub scopes: Vec<String>,
    pub scope: usize,
    pub regex: bool,
    pub match_case: bool,
    pub cursor: usize,
    pub note_ids: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmAction {
    DeleteNotes(Vec<i64>),
    DeleteSaved(String),
}

#[derive(Debug, Clone)]
pub struct Confirm {
    pub message: String,
    pub action: ConfirmAction,
}

#[derive(Debug, Clone)]
pub enum Dialog {
    Prompt(Prompt),
    Choice(Choice),
    Columns(Columns),
    Forget(Forget),
    Reposition(Reposition),
    FindReplace(FindReplaceForm),
    Confirm(Confirm),
}

fn toggle_row(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(' '))
}

/// Up/Down/Tab movement over `rows` form rows.
fn move_cursor(key: &KeyEvent, cursor: &mut usize, rows: usize) -> bool {
    match key.code {
        KeyCode::Down | KeyCode::Tab => *cursor = (*cursor + 1) % rows,
        KeyCode::Up | KeyCode::BackTab => *cursor = (*cursor + rows - 1) % rows,
        _ => return false,
    }
    true
}

impl Prompt {
    pub fn new(kind: PromptKind, title: &str, help: &str, initial: &str) -> Self {
        Self {
            kind,
            title: title.into(),
            help: help.into(),
            input: TextInput::new(initial),
            completion: Completion::new(true),
            items: Vec::new(),
            whole_input: false,
        }
    }
    pub fn with_items(mut self, items: Vec<String>, whole_input: bool) -> Self {
        self.items = items;
        self.whole_input = whole_input;
        self
    }
}

impl Dialog {
    pub fn on_key(&mut self, key: &KeyEvent) -> Outcome {
        if key.code == KeyCode::Esc && !matches!(self, Dialog::Prompt(p) if p.completion.is_open())
        {
            return Outcome::Cancel;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Outcome::Cancel;
        }
        match self {
            Dialog::Prompt(p) => {
                let mut source = ListSource {
                    items: &p.items,
                    detail: "",
                    whole_input: p.whole_input,
                };
                match p.completion.handle_key(key, &mut p.input, &mut source) {
                    CompletionOutcome::Ignored if key.code == KeyCode::Enter => Outcome::Submit,
                    _ => Outcome::Open,
                }
            }
            Dialog::Choice(c) => match key.code {
                KeyCode::Enter => Outcome::Submit,
                KeyCode::Char('j') => {
                    c.cursor = (c.cursor + 1) % c.options.len();
                    Outcome::Open
                }
                KeyCode::Char('k') => {
                    c.cursor = (c.cursor + c.options.len() - 1) % c.options.len();
                    Outcome::Open
                }
                KeyCode::Char(d) if d.is_ascii_digit() => {
                    let n = d.to_digit(10).unwrap_or(0) as usize;
                    if (1..=c.options.len()).contains(&n) {
                        c.cursor = n - 1;
                        Outcome::Submit
                    } else {
                        Outcome::Open
                    }
                }
                _ => {
                    move_cursor(key, &mut c.cursor, c.options.len());
                    Outcome::Open
                }
            },
            Dialog::Columns(c) => match key.code {
                KeyCode::Enter | KeyCode::Char('q') => Outcome::Cancel,
                KeyCode::Char(' ') => Outcome::Submit,
                KeyCode::Char('j') => {
                    c.cursor = (c.cursor + 1) % c.items.len();
                    Outcome::Open
                }
                KeyCode::Char('k') => {
                    c.cursor = (c.cursor + c.items.len() - 1) % c.items.len();
                    Outcome::Open
                }
                _ => {
                    move_cursor(key, &mut c.cursor, c.items.len());
                    Outcome::Open
                }
            },
            Dialog::Forget(f) => {
                if key.code == KeyCode::Enter {
                    return Outcome::Submit;
                }
                if toggle_row(key) {
                    if f.cursor == 0 {
                        f.restore_position = !f.restore_position;
                    } else {
                        f.reset_counts = !f.reset_counts;
                    }
                } else {
                    move_cursor(key, &mut f.cursor, 2);
                }
                Outcome::Open
            }
            Dialog::Reposition(r) => {
                if key.code == KeyCode::Enter {
                    return Outcome::Submit;
                }
                if move_cursor(key, &mut r.cursor, 4) {
                    return Outcome::Open;
                }
                match r.cursor {
                    0 => {
                        r.start.handle_key(key);
                    }
                    1 => {
                        r.step.handle_key(key);
                    }
                    2 if toggle_row(key) => r.randomize = !r.randomize,
                    3 if toggle_row(key) => r.shift = !r.shift,
                    _ => {}
                }
                Outcome::Open
            }
            Dialog::FindReplace(f) => {
                if key.code == KeyCode::Enter {
                    return Outcome::Submit;
                }
                if move_cursor(key, &mut f.cursor, 5) {
                    return Outcome::Open;
                }
                match f.cursor {
                    0 => {
                        f.find.handle_key(key);
                    }
                    1 => {
                        f.replace.handle_key(key);
                    }
                    2 => match key.code {
                        KeyCode::Right | KeyCode::Char(' ') | KeyCode::Char('l') => {
                            f.scope = (f.scope + 1) % f.scopes.len()
                        }
                        KeyCode::Left | KeyCode::Char('h') => {
                            f.scope = (f.scope + f.scopes.len() - 1) % f.scopes.len()
                        }
                        _ => {}
                    },
                    3 if toggle_row(key) => f.regex = !f.regex,
                    4 if toggle_row(key) => f.match_case = !f.match_case,
                    _ => {}
                }
                Outcome::Open
            }
            Dialog::Confirm(_) => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => Outcome::Submit,
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => Outcome::Cancel,
                _ => Outcome::Open,
            },
        }
    }

    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        match self {
            Dialog::Prompt(_) => vec![("Enter", "ok"), ("Tab", "complete"), ("Esc", "cancel")],
            Dialog::Choice(_) => vec![("j/k", "move"), ("Enter", "choose"), ("Esc", "cancel")],
            Dialog::Columns(_) => vec![("j/k", "move"), ("Space", "show/hide"), ("Esc", "done")],
            Dialog::Forget(_) | Dialog::Reposition(_) | Dialog::FindReplace(_) => vec![
                ("Tab/↓", "next"),
                ("Space", "toggle"),
                ("Enter", "ok"),
                ("Esc", "cancel"),
            ],
            Dialog::Confirm(_) => vec![("y", "yes"), ("n", "no")],
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let rev = Style::default().add_modifier(Modifier::REVERSED);
        let check = |on: bool| if on { "[x]" } else { "[ ]" };
        let row = |selected: bool, text: String| {
            if selected {
                Line::styled(format!("> {text}"), bold)
            } else {
                Line::raw(format!("  {text}"))
            }
        };
        // The input to draw a cursor in: (line, input, columns before it).
        let mut input_at: Option<(usize, &TextInput, usize)> = None;
        let (title, lines, width): (String, Vec<Line>, u16) = match self {
            Dialog::Prompt(p) => {
                let mut lines = vec![];
                if !p.help.is_empty() {
                    lines.push(Line::styled(p.help.clone(), dim));
                }
                input_at = Some((lines.len(), &p.input, 0));
                lines.push(Line::raw(p.input.text().to_owned()));
                (p.title.clone(), lines, 60)
            }
            Dialog::Choice(c) => (
                c.title.clone(),
                c.options
                    .iter()
                    .enumerate()
                    .map(|(i, o)| row(i == c.cursor, format!("{} {o}", i + 1)))
                    .collect(),
                44,
            ),
            Dialog::Columns(c) => (
                "Columns".into(),
                c.items
                    .iter()
                    .enumerate()
                    .map(|(i, (_, label, on))| {
                        row(i == c.cursor, format!("{} {label}", check(*on)))
                    })
                    .collect(),
                40,
            ),
            Dialog::Forget(f) => (
                format!("Reset {} card(s) to new", f.count),
                vec![
                    row(
                        f.cursor == 0,
                        format!(
                            "{} Restore original position where possible",
                            check(f.restore_position)
                        ),
                    ),
                    row(
                        f.cursor == 1,
                        format!(
                            "{} Reset repetition and lapse counts",
                            check(f.reset_counts)
                        ),
                    ),
                ],
                56,
            ),
            Dialog::Reposition(r) => {
                input_at = Some(match r.cursor {
                    1 => (1, &r.step, 17),
                    _ => (0, &r.start, 17),
                });
                (
                    format!("Reposition {} new card(s)", r.count),
                    vec![
                        row(r.cursor == 0, format!("Start position {}", r.start.text())),
                        row(r.cursor == 1, format!("Step           {}", r.step.text())),
                        row(
                            r.cursor == 2,
                            format!("{} Randomize order", check(r.randomize)),
                        ),
                        row(
                            r.cursor == 3,
                            format!("{} Shift position of existing cards", check(r.shift)),
                        ),
                    ],
                    52,
                )
            }
            Dialog::FindReplace(f) => {
                input_at = match f.cursor {
                    0 => Some((1, &f.find, 11)),
                    1 => Some((2, &f.replace, 11)),
                    _ => None,
                };
                (
                    "Find and Replace".into(),
                    vec![
                        Line::styled(format!("In {} selected note(s)", f.note_ids.len()), dim),
                        row(f.cursor == 0, format!("Find:    {}", f.find.text())),
                        row(f.cursor == 1, format!("Replace: {}", f.replace.text())),
                        row(f.cursor == 2, format!("In:      ‹ {} ›", f.scopes[f.scope])),
                        row(
                            f.cursor == 3,
                            format!("{} Treat input as regular expression", check(f.regex)),
                        ),
                        row(f.cursor == 4, format!("{} Match case", check(f.match_case))),
                    ],
                    60,
                )
            }
            Dialog::Confirm(c) => (
                "Confirm".into(),
                vec![
                    Line::raw(c.message.clone()),
                    Line::raw(""),
                    Line::from(vec![
                        Span::styled("y", bold),
                        Span::raw(" yes   "),
                        Span::styled("n", bold),
                        Span::raw(" no"),
                    ]),
                ],
                50,
            ),
        };
        let width = width.min(area.width);
        let inner_width = width.saturating_sub(2) as usize;
        let wrapped: usize = lines
            .iter()
            .map(|l| l.width().max(1).div_ceil(inner_width.max(1)))
            .sum();
        let height = (wrapped as u16 + 2).min(area.height);
        let rect = Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y + (area.height - height) / 2,
            width,
            height,
        };
        Clear.render(rect, buf);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(format!(" {title} "), bold));
        let inner = block.inner(rect);
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .render(rect, buf);
        // The text cursor, as a reversed cell.
        if let Some((line, input, offset)) = input_at {
            let x = inner.x + (offset + input.cursor_column()) as u16;
            let y = inner.y + line as u16;
            if x < inner.right() && y < inner.bottom() {
                buf[(x, y)].set_style(rev);
            }
            if let Dialog::Prompt(p) = self {
                let anchor = Rect::new(inner.x, y, inner.width, 1);
                p.completion.render(input, anchor, area, buf);
            }
        }
    }
}
