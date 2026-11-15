//! The deck list's prompts: add, rename, "Move to…" and delete (desktop's
//! deck browser menu; "Move to…" replaces its drag and drop).
use super::{
    bold, dim, draw_input, frame, is_cancel, put, rev, tools, unexpected, DeckToolsIntent,
};
use crate::backend::Backend;
use crate::input::TextInput;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{buffer::Buffer, layout::Rect, style::Style};
use tanki_core::deck_tools::{DeckPromptInfo, DeckToolsCommand, DeckToolsResponse};
use tanki_core::CoreError;

/// Rows of the "Move to…" list shown at once.
const LIST_ROWS: usize = 10;
const TOP_LEVEL: &str = "(top level)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeckPromptKind {
    Add,
    Rename,
    Move,
    Delete,
}

/// One prompt over the deck list. A finished prompt answers
/// [`DeckToolsIntent::Done`]; Anki's refusals (a name clash, a filtered
/// parent) arrive as [`DeckToolsIntent::Message`] and keep it open.
#[derive(Debug, Clone)]
pub struct DeckPromptUi {
    kind: DeckPromptKind,
    info: DeckPromptInfo,
    /// The name (add, rename) or the filter (move).
    input: TextInput,
    /// Index into [`Self::matches`].
    cursor: usize,
}

impl DeckPromptUi {
    /// A new deck. `prefix` seeds the name: `"Parent::"` adds a subdeck.
    pub fn add(backend: &dyn Backend, prefix: &str) -> Result<Self, CoreError> {
        let mut ui = Self::open(backend, DeckPromptKind::Add, 0)?;
        ui.input = TextInput::new(prefix);
        Ok(ui)
    }

    /// Rename with the full name, so `::` can also move the deck.
    pub fn rename(backend: &dyn Backend, deck_id: i64) -> Result<Self, CoreError> {
        Self::open(backend, DeckPromptKind::Rename, deck_id)
    }

    /// Choose a new parent from the normal decks, or the top level.
    pub fn move_to(backend: &dyn Backend, deck_id: i64) -> Result<Self, CoreError> {
        Self::open(backend, DeckPromptKind::Move, deck_id)
    }

    /// Confirm deleting the deck, its subdecks and their cards.
    pub fn delete(backend: &dyn Backend, deck_id: i64) -> Result<Self, CoreError> {
        Self::open(backend, DeckPromptKind::Delete, deck_id)
    }

    /// `deck_id` is ignored for [`DeckPromptKind::Add`].
    pub fn open(
        backend: &dyn Backend,
        kind: DeckPromptKind,
        deck_id: i64,
    ) -> Result<Self, CoreError> {
        let deck_id = if kind == DeckPromptKind::Add {
            0
        } else {
            deck_id
        };
        match tools(backend, DeckToolsCommand::DeckPrompt { deck_id })? {
            DeckToolsResponse::DeckPrompt(info) => Ok(Self::from_info(kind, *info)),
            _ => Err(unexpected()),
        }
    }

    pub fn from_info(kind: DeckPromptKind, info: DeckPromptInfo) -> Self {
        let input = match kind {
            DeckPromptKind::Rename => TextInput::new(info.name.clone()),
            _ => TextInput::default(),
        };
        Self {
            kind,
            info,
            input,
            cursor: 0,
        }
    }

    pub fn kind(&self) -> DeckPromptKind {
        self.kind
    }

    /// 0 for the add prompt.
    pub fn deck_id(&self) -> i64 {
        self.info.deck_id
    }

    /// The parents the filter leaves, as `(deck id, name)`; id 0 is the top
    /// level, offered to a subdeck only.
    fn matches(&self) -> Vec<(i64, &str)> {
        let needle = self.input.text().trim().to_lowercase();
        let top = self.info.name.contains("::").then_some((0, TOP_LEVEL));
        top.into_iter()
            .chain(self.info.targets.iter().map(|t| (t.id, t.name.as_str())))
            .filter(|(_, name)| needle.is_empty() || name.to_lowercase().contains(&needle))
            .collect()
    }

    fn finish(
        &self,
        result: Result<DeckToolsResponse, CoreError>,
        message: Option<String>,
    ) -> DeckToolsIntent {
        match result {
            Ok(DeckToolsResponse::Deleted {
                changes, message, ..
            }) => DeckToolsIntent::Done {
                changes,
                message: Some(message),
            },
            Ok(
                DeckToolsResponse::Changes(changes)
                | DeckToolsResponse::Count { changes, .. }
                | DeckToolsResponse::Saved { changes, .. },
            ) => DeckToolsIntent::Done { changes, message },
            Ok(_) => DeckToolsIntent::Message(unexpected().to_string()),
            Err(e) => DeckToolsIntent::Message(e.to_string()),
        }
    }

    fn accept(&mut self, backend: &dyn Backend) -> DeckToolsIntent {
        let deck_id = self.info.deck_id;
        match self.kind {
            DeckPromptKind::Add | DeckPromptKind::Rename => {
                let name = self.input.text().trim().to_owned();
                if name.is_empty() {
                    return DeckToolsIntent::Message("Type a deck name.".into());
                }
                if self.kind == DeckPromptKind::Add {
                    return self
                        .finish(tools(backend, DeckToolsCommand::CreateDeck { name }), None);
                }
                if name == self.info.name {
                    return DeckToolsIntent::Close;
                }
                let command = DeckToolsCommand::RenameDeck {
                    deck_id,
                    new_name: name,
                };
                self.finish(tools(backend, command), None)
            }
            DeckPromptKind::Move => {
                let matches = self.matches();
                let Some(&(new_parent, target)) = matches.get(self.cursor) else {
                    return DeckToolsIntent::Message("No deck matches.".into());
                };
                let leaf = self.info.name.rsplit("::").next().unwrap_or_default();
                let message = if new_parent == 0 {
                    format!("Moved {leaf} to the top level.")
                } else {
                    format!("Moved {leaf} to {target}.")
                };
                let command = DeckToolsCommand::ReparentDecks {
                    deck_ids: vec![deck_id],
                    new_parent,
                };
                self.finish(tools(backend, command), Some(message))
            }
            DeckPromptKind::Delete => {
                let command = DeckToolsCommand::DeleteDecks {
                    deck_ids: vec![deck_id],
                };
                self.finish(tools(backend, command), None)
            }
        }
    }

    pub fn on_paste(&mut self, text: &str) {
        if self.kind != DeckPromptKind::Delete {
            self.input.insert_str(text);
            self.cursor = 0;
        }
    }

    pub fn on_key(&mut self, backend: &dyn Backend, key: KeyEvent) -> DeckToolsIntent {
        if is_cancel(&key) {
            return DeckToolsIntent::Close;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match self.kind {
            DeckPromptKind::Delete => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => self.accept(backend),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => {
                    DeckToolsIntent::Close
                }
                _ => DeckToolsIntent::None,
            },
            DeckPromptKind::Add | DeckPromptKind::Rename => {
                if key.code == KeyCode::Enter {
                    return self.accept(backend);
                }
                self.input.handle_key(&key);
                DeckToolsIntent::None
            }
            DeckPromptKind::Move => {
                let count = self.matches().len();
                let last = count.saturating_sub(1);
                match key.code {
                    KeyCode::Enter => return self.accept(backend),
                    KeyCode::Down | KeyCode::Tab => self.cursor = (self.cursor + 1).min(last),
                    KeyCode::Char('n') if ctrl => self.cursor = (self.cursor + 1).min(last),
                    KeyCode::Up | KeyCode::BackTab => self.cursor = self.cursor.saturating_sub(1),
                    KeyCode::Char('p') if ctrl => self.cursor = self.cursor.saturating_sub(1),
                    KeyCode::PageDown => self.cursor = (self.cursor + LIST_ROWS).min(last),
                    KeyCode::PageUp => self.cursor = self.cursor.saturating_sub(LIST_ROWS),
                    _ => {
                        let before = self.input.text().to_owned();
                        self.input.handle_key(&key);
                        if self.input.text() != before {
                            self.cursor = 0;
                        }
                    }
                }
                DeckToolsIntent::None
            }
        }
    }

    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        match self.kind {
            DeckPromptKind::Add => vec![("Enter", "create"), ("Esc", "cancel")],
            DeckPromptKind::Rename => vec![("Enter", "rename"), ("Esc", "cancel")],
            DeckPromptKind::Move => vec![
                ("type", "filter"),
                ("↑/↓", "choose"),
                ("Enter", "move"),
                ("Esc", "cancel"),
            ],
            DeckPromptKind::Delete => vec![("y", "delete"), ("Esc", "cancel")],
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        match self.kind {
            DeckPromptKind::Add | DeckPromptKind::Rename => self.render_name(area, buf),
            DeckPromptKind::Move => self.render_move(area, buf),
            DeckPromptKind::Delete => self.render_delete(area, buf),
        }
    }

    fn footer(&self, inner: Rect, buf: &mut Buffer) {
        if inner.height < 2 {
            return;
        }
        let text = self
            .hints()
            .iter()
            .map(|(key, what)| format!("{key} {what}"))
            .collect::<Vec<_>>()
            .join(" · ");
        put(
            buf,
            inner.x + 2,
            inner.bottom() - 1,
            inner.width.saturating_sub(2),
            &text,
            dim(),
        );
    }

    fn render_name(&self, area: Rect, buf: &mut Buffer) {
        let labels = &self.info.labels;
        let (title, label) = if self.kind == DeckPromptKind::Add {
            (labels.create.clone(), &labels.new_deck_name)
        } else {
            (
                format!("{}: {}", labels.rename, self.info.name),
                &labels.new_name,
            )
        };
        let inner = frame(area, buf, &title, 64, 6);
        if inner.width < 8 || inner.height == 0 {
            return;
        }
        let x = inner.x + 2;
        let width = inner.width - 2;
        put(buf, x, inner.y, width, label, Style::default());
        if inner.height >= 2 {
            draw_input(buf, x, inner.y + 1, width, &self.input, true);
        }
        if inner.height >= 4 {
            self.footer(inner, buf);
        }
    }

    fn render_move(&self, area: Rect, buf: &mut Buffer) {
        let matches = self.matches();
        let rows = matches.len().clamp(1, LIST_ROWS);
        let title = format!("Move {} to…", self.info.name);
        let inner = frame(area, buf, &title, 64, rows as u16 + 6);
        if inner.width < 12 || inner.height < 3 {
            return;
        }
        let x = inner.x + 2;
        let width = inner.width - 2;
        let label = "Filter: ";
        put(buf, x, inner.y, width, label, dim());
        let field_x = x + label.len() as u16;
        draw_input(
            buf,
            field_x,
            inner.y,
            inner.right().saturating_sub(field_x),
            &self.input,
            true,
        );
        let rows = (inner.height.saturating_sub(4) as usize).min(rows);
        if matches.is_empty() {
            put(buf, x, inner.y + 2, width, "No deck matches.", dim());
        }
        let skip = (self.cursor + 1).saturating_sub(rows);
        for (line, (i, (_, name))) in matches.iter().enumerate().skip(skip).take(rows).enumerate() {
            let y = inner.y + 2 + line as u16;
            if i == self.cursor {
                put(buf, inner.x, y, 2, ">", bold());
                put(buf, x, y, width, name, rev());
            } else {
                put(buf, x, y, width, name, Style::default());
            }
        }
        self.footer(inner, buf);
    }

    fn render_delete(&self, area: Rect, buf: &mut Buffer) {
        let info = &self.info;
        let inner = frame(area, buf, &info.labels.delete, 64, 7);
        if inner.width < 8 || inner.height == 0 {
            return;
        }
        let x = inner.x + 2;
        let width = inner.width - 2;
        put(buf, x, inner.y, width, &info.name, bold());
        let plural = |n: u32, one: &str, many: &str| {
            if n == 1 {
                format!("1 {one}")
            } else {
                format!("{n} {many}")
            }
        };
        let cards = plural(info.card_count, "card", "cards");
        let what = if info.filtered {
            // Anki sends a filtered deck's cards home instead.
            format!("{cards} will return to their home decks.")
        } else if info.subdeck_count > 0 {
            format!(
                "{cards} and {} will be deleted.",
                plural(info.subdeck_count, "subdeck", "subdecks")
            )
        } else {
            format!("{cards} will be deleted.")
        };
        if inner.height >= 3 {
            put(buf, x, inner.y + 2, width, &what, Style::default());
        }
        if inner.height >= 5 {
            self.footer(inner, buf);
        }
    }
}
