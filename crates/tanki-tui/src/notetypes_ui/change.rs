//! The change-note-type mapping form (PLAN 8.6), used from the browser.
//!
//! # Wiring
//! The browser builds [`ChangeNotetypeUi::new`] from its selected note ids
//! and shows it as a layer. Keys go to `on_key`; act on [`ChangeIntent`];
//! draw with `render`; footer text from `hints()`.
use super::{run, unexpected, Status};
use crate::backend::Backend;
use crate::input::TextInput;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear, Widget},
};
use tanki_core::notetypes::{
    ChangeNotetypeInfo, NotetypeCommand, NotetypeResponse, NotetypeSummary,
};
use tanki_core::{Command, CoreError, EditResponse, NoteCommand, OpChanges, Response};

pub const CHANGE_KEYS: &[(&str, &str)] = &[
    ("↑↓", "choose a row of the new note type"),
    ("← →", "choose which old field / card type feeds it"),
    ("t", "choose another new note type"),
    ("d", "back to Anki's default mapping"),
    (
        "Enter / Ctrl+S",
        "change the note type (asks about the full sync)",
    ),
    ("Esc", "cancel"),
];

#[derive(Debug, Clone, PartialEq)]
pub enum ChangeIntent {
    None,
    Close,
    /// The notes changed type: refresh the browser; the form is done.
    Changed {
        changes: OpChanges,
        notes: usize,
    },
}

enum Overlay {
    None,
    /// Choosing the new note type.
    Picker {
        query: TextInput,
        selected: usize,
    },
    FullSync,
}

pub struct ChangeNotetypeUi {
    note_ids: Vec<i64>,
    notetypes: Vec<NotetypeSummary>,
    info: Option<ChangeNotetypeInfo>,
    new_fields: Vec<Option<u32>>,
    new_templates: Option<Vec<Option<u32>>>,
    row: usize,
    overlay: Overlay,
    status: Option<Status>,
}

impl ChangeNotetypeUi {
    /// For these notes (all of one note type). The target starts as the
    /// notes' own type, as in desktop; `t` chooses another.
    pub fn new(b: &dyn Backend, note_ids: Vec<i64>) -> Result<Self, String> {
        let notetypes = match run(b, NotetypeCommand::List).map_err(|e| e.to_string())? {
            NotetypeResponse::List(l) => l,
            other => return unexpected(other).map_err(|e: CoreError| e.to_string()),
        };
        if note_ids.is_empty() {
            return Err("No notes selected".into());
        }
        let current = match b.execute(Command::Notes(NoteCommand::GetNote { id: note_ids[0] })) {
            Ok(Response::Edit(EditResponse::Note(n))) => Some(n.notetype_id),
            _ => None,
        };
        let mut ui = Self {
            note_ids,
            notetypes,
            info: None,
            new_fields: Vec::new(),
            new_templates: None,
            row: 0,
            overlay: Overlay::None,
            status: None,
        };
        match current {
            Some(id) => ui.choose(b, id),
            None => {
                ui.overlay = Overlay::Picker {
                    query: TextInput::default(),
                    selected: 0,
                }
            }
        }
        Ok(ui)
    }

    pub fn info(&self) -> Option<&ChangeNotetypeInfo> {
        self.info.as_ref()
    }

    pub fn new_fields(&self) -> &[Option<u32>] {
        &self.new_fields
    }

    pub fn new_templates(&self) -> Option<&[Option<u32>]> {
        self.new_templates.as_deref()
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_ref().map(|s| s.text.as_str())
    }

    pub fn hints(&self) -> String {
        match self.overlay {
            Overlay::Picker { .. } => {
                "type to filter · ↑↓ choose · Enter select · Esc cancel".into()
            }
            Overlay::FullSync => "y continue (full upload on the next sync) · n cancel".into(),
            Overlay::None => {
                "↑↓ row · ←→ source · t new type · d default · Enter change · Esc cancel".into()
            }
        }
    }

    fn error(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            error: true,
        });
    }

    /// Loads Anki's default mapping for the new note type.
    fn choose(&mut self, b: &dyn Backend, new_notetype_id: i64) {
        match run(
            b,
            NotetypeCommand::ChangeInfo {
                note_ids: self.note_ids.clone(),
                new_notetype_id,
            },
        ) {
            Ok(NotetypeResponse::ChangeInfo(info)) => {
                self.new_fields = info.new_fields.clone();
                self.new_templates = info.new_templates.clone();
                self.info = Some(info);
                self.row = 0;
                self.status = None;
            }
            Ok(other) => {
                let _ = unexpected::<()>(other);
            }
            Err(e) => {
                self.error(e.to_string());
                if self.info.is_none() {
                    self.overlay = Overlay::Picker {
                        query: TextInput::default(),
                        selected: 0,
                    };
                }
            }
        }
    }

    fn rows(&self) -> usize {
        self.new_fields.len() + self.new_templates.as_ref().map_or(0, Vec::len)
    }

    /// Cycles the source of the current row through (Nothing) and the old
    /// items.
    fn cycle(&mut self, forward: bool) {
        let Some(info) = &self.info else { return };
        let fields = self.new_fields.len();
        let (slot, old_len) = if self.row < fields {
            (&mut self.new_fields[self.row], info.old_field_names.len())
        } else {
            let Some(templates) = self.new_templates.as_mut() else {
                return;
            };
            (
                &mut templates[self.row - fields],
                info.old_template_names.len(),
            )
        };
        // Positions: 0 = nothing, 1.. = old item + 1.
        let position = slot.map_or(0, |i| i as usize + 1);
        let total = old_len + 1;
        let next = if forward {
            (position + 1) % total
        } else {
            (position + total - 1) % total
        };
        *slot = next.checked_sub(1).map(|i| i as u32);
    }

    pub fn on_key(&mut self, b: &dyn Backend, key: KeyEvent) -> ChangeIntent {
        if key.kind == KeyEventKind::Release {
            return ChangeIntent::None;
        }
        match std::mem::replace(&mut self.overlay, Overlay::None) {
            Overlay::Picker {
                mut query,
                mut selected,
            } => {
                let matches = self.matches(query.text());
                match key.code {
                    KeyCode::Esc => {
                        return if self.info.is_none() {
                            ChangeIntent::Close
                        } else {
                            ChangeIntent::None
                        }
                    }
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => selected = (selected + 1).min(matches.len().saturating_sub(1)),
                    KeyCode::Enter => {
                        if let Some(&i) = matches.get(selected.min(matches.len().saturating_sub(1)))
                        {
                            let id = self.notetypes[i].id;
                            self.choose(b, id);
                            return ChangeIntent::None;
                        }
                    }
                    _ => {
                        if query.handle_key(&key) {
                            selected = 0;
                        }
                    }
                }
                self.overlay = Overlay::Picker { query, selected };
                ChangeIntent::None
            }
            Overlay::FullSync => {
                if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                    return self.apply(b, true);
                }
                if !matches!(key.code, KeyCode::Char('n' | 'N') | KeyCode::Esc) {
                    self.overlay = Overlay::FullSync;
                }
                ChangeIntent::None
            }
            Overlay::None => self.form_key(b, key),
        }
    }

    fn form_key(&mut self, b: &dyn Backend, key: KeyEvent) -> ChangeIntent {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let rows = self.rows();
        match key.code {
            KeyCode::Esc => return ChangeIntent::Close,
            KeyCode::Up | KeyCode::Char('k') => self.row = self.row.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.row = (self.row + 1).min(rows.saturating_sub(1))
            }
            KeyCode::Left | KeyCode::Char('h') => self.cycle(false),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => self.cycle(true),
            KeyCode::Char('t') if !ctrl => {
                self.overlay = Overlay::Picker {
                    query: TextInput::default(),
                    selected: 0,
                }
            }
            KeyCode::Char('d') if !ctrl => {
                if let Some(id) = self.info.as_ref().map(|i| i.new_notetype_id) {
                    self.choose(b, id);
                }
            }
            KeyCode::Enter => return self.apply(b, false),
            KeyCode::Char('s') | KeyCode::Char('S') if ctrl => return self.apply(b, false),
            _ => {}
        }
        ChangeIntent::None
    }

    fn matches(&self, query: &str) -> Vec<usize> {
        crate::fuzzy::rank(query, self.notetypes.iter().map(|n| n.name.as_str()))
    }

    fn apply(&mut self, b: &dyn Backend, confirmed: bool) -> ChangeIntent {
        let Some(info) = &self.info else {
            return ChangeIntent::None;
        };
        let result = run(
            b,
            NotetypeCommand::Change {
                note_ids: self.note_ids.clone(),
                new_notetype_id: info.new_notetype_id,
                new_fields: self.new_fields.clone(),
                new_templates: self.new_templates.clone(),
                confirmed,
            },
        );
        match result {
            Ok(NotetypeResponse::NotesChanged { count, changes }) => ChangeIntent::Changed {
                changes,
                notes: count,
            },
            Ok(other) => {
                let _ = unexpected::<()>(other);
                ChangeIntent::None
            }
            Err(CoreError::SchemaChangeRequired) => {
                self.overlay = Overlay::FullSync;
                ChangeIntent::None
            }
            Err(e) => {
                self.error(e.to_string());
                ChangeIntent::None
            }
        }
    }

    // ---- drawing ------------------------------------------------------------------

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width < 20 || area.height < 6 {
            return;
        }
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().fg(Color::DarkGray);
        let sel = Style::default().add_modifier(Modifier::REVERSED);
        let put = |buf: &mut Buffer, y: u16, text: &str, style: Style| {
            if y < area.bottom() {
                buf.set_stringn(area.x, y, text, area.width as usize, style);
            }
        };
        let Some(info) = &self.info else {
            put(buf, area.y, "Change note type", bold);
            self.draw_overlay(area, buf);
            return;
        };
        let title = format!(
            "Change note type of {} note{}: {} → {}",
            info.note_count,
            if info.note_count == 1 { "" } else { "s" },
            info.old_notetype_name,
            info.new_notetype_name
        );
        put(buf, area.y, &title, bold);
        let width = info
            .new_field_names
            .iter()
            .chain(&info.new_template_names)
            .map(|n| unicode_width::UnicodeWidthStr::width(n.as_str()))
            .max()
            .unwrap_or(8)
            .min(30);
        let mut y = area.y + 2;
        put(buf, y - 1, "Fields (new ← old):", dim);
        let source = |slot: Option<u32>, names: &[String]| match slot {
            Some(i) => names
                .get(i as usize)
                .cloned()
                .unwrap_or_else(|| "?".to_owned()),
            None => "(Nothing)".to_owned(),
        };
        for (i, name) in info.new_field_names.iter().enumerate() {
            let line = format!(
                "  {name:<width$}  ←  {}",
                source(
                    self.new_fields.get(i).copied().flatten(),
                    &info.old_field_names
                )
            );
            put(
                buf,
                y,
                &line,
                if self.row == i { sel } else { Style::default() },
            );
            y += 1;
        }
        let unused: Vec<&str> = info
            .old_field_names
            .iter()
            .enumerate()
            .filter(|(i, _)| !self.new_fields.contains(&Some(*i as u32)))
            .map(|(_, n)| n.as_str())
            .collect();
        if !unused.is_empty() {
            put(
                buf,
                y,
                &format!("  Not copied: {}", unused.join(", ")),
                Style::default().fg(Color::Yellow),
            );
            y += 1;
        }
        y += 1;
        match &self.new_templates {
            Some(templates) => {
                put(buf, y, "Card types (new ← old):", dim);
                y += 1;
                for (i, name) in info.new_template_names.iter().enumerate() {
                    let line = format!(
                        "  {name:<width$}  ←  {}",
                        source(
                            templates.get(i).copied().flatten(),
                            &info.old_template_names
                        )
                    );
                    let row = self.new_fields.len() + i;
                    put(
                        buf,
                        y,
                        &line,
                        if self.row == row {
                            sel
                        } else {
                            Style::default()
                        },
                    );
                    y += 1;
                }
                let dropped: Vec<&str> = info
                    .old_template_names
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !templates.contains(&Some(*i as u32)))
                    .map(|(_, n)| n.as_str())
                    .collect();
                if !dropped.is_empty() {
                    put(
                        buf,
                        y,
                        &format!(
                            "  Cards of these card types are deleted: {}",
                            dropped.join(", ")
                        ),
                        Style::default().fg(Color::Red),
                    );
                    y += 1;
                }
            }
            None => {
                put(
                    buf,
                    y,
                    "Card types are not mapped for cloze note types.",
                    dim,
                );
                y += 1;
            }
        }
        let _ = y;
        if let Some(status) = &self.status {
            let style = if status.error {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            put(buf, area.bottom() - 1, &status.text, style);
        }
        self.draw_overlay(area, buf);
    }

    fn draw_overlay(&self, area: Rect, buf: &mut Buffer) {
        let sel = Style::default().add_modifier(Modifier::REVERSED);
        match &self.overlay {
            Overlay::None => {}
            Overlay::FullSync => {
                let rect = centered(area, 60, 6);
                Clear.render(rect, buf);
                let block = Block::default()
                    .borders(Borders::ALL)
                    .title(" Full sync required ");
                let inner = block.inner(rect);
                block.render(rect, buf);
                buf.set_stringn(
                    inner.x,
                    inner.y,
                    "Changing the note type requires a one-way full sync:",
                    inner.width as usize,
                    Style::default(),
                );
                buf.set_stringn(
                    inner.x,
                    inner.y + 1,
                    "the next sync uploads the whole collection.",
                    inner.width as usize,
                    Style::default(),
                );
                buf.set_stringn(
                    inner.x,
                    inner.y + 3,
                    "Continue? (y/n)",
                    inner.width as usize,
                    Style::default(),
                );
            }
            Overlay::Picker { query, selected } => {
                let matches = self.matches(query.text());
                let height = (matches.len().min(10) as u16) + 3;
                let rect = centered(area, 50, height);
                Clear.render(rect, buf);
                let block = Block::default()
                    .borders(Borders::ALL)
                    .title(" New note type ");
                let inner = block.inner(rect);
                block.render(rect, buf);
                buf.set_stringn(
                    inner.x,
                    inner.y,
                    format!("> {}", query.text()),
                    inner.width as usize,
                    Style::default(),
                );
                let selected = (*selected).min(matches.len().saturating_sub(1));
                let skip = selected.saturating_sub(9);
                for (row, &i) in matches.iter().skip(skip).take(10).enumerate() {
                    let style = if row + skip == selected {
                        sel
                    } else {
                        Style::default()
                    };
                    buf.set_stringn(
                        inner.x,
                        inner.y + 1 + row as u16,
                        &self.notetypes[i].name,
                        inner.width as usize,
                        style,
                    );
                }
            }
        }
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}
