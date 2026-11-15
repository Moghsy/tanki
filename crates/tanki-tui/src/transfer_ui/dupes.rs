//! Find duplicates (R6): choose a field and an optional filter, list the
//! groups, open one in the browser, tag them all.
use super::*;
use tanki_core::transfer::DuplicateGroup;

enum Screen {
    Form,
    Results,
}

pub struct DuplicatesUi {
    screen: Screen,
    fields: Vec<String>,
    field: usize,
    search: TextInput,
    focus: usize,
    groups: Vec<DuplicateGroup>,
    selected: usize,
    scroll: std::cell::Cell<usize>,
    note: Option<String>,
}

impl DuplicatesUi {
    /// `search` pre-fills the filter (the browser passes its current search).
    pub fn open(backend: &dyn Backend, search: &str) -> Result<Self, CoreError> {
        let fields = match transfer(backend, TransferCommand::DuplicateFieldNames)? {
            TransferResponse::FieldNames(names) => names,
            _ => return Err(unexpected()),
        };
        if fields.is_empty() {
            return Err(CoreError::Invalid("There are no fields to compare".into()));
        }
        // Desktop starts on the first field of the first note type.
        let field = fields.iter().position(|f| f == "Front").unwrap_or(0);
        Ok(Self {
            screen: Screen::Form,
            fields,
            field,
            search: TextInput::new(search),
            focus: 0,
            groups: Vec::new(),
            selected: 0,
            scroll: std::cell::Cell::new(0),
            note: None,
        })
    }

    pub fn groups(&self) -> &[DuplicateGroup] {
        &self.groups
    }

    fn find(&mut self, backend: &dyn Backend) -> TransferIntent {
        let command = TransferCommand::FindDuplicates {
            field_name: self.fields[self.field].clone(),
            search: self.search.text().trim().to_owned(),
        };
        match transfer(backend, command) {
            Ok(TransferResponse::Duplicates(groups)) => {
                self.note = if groups.is_empty() {
                    Some("No duplicates found".into())
                } else {
                    None
                };
                if groups.is_empty() {
                    return TransferIntent::Message("No duplicates found".into());
                }
                self.groups = groups;
                self.selected = 0;
                self.scroll.set(0);
                self.screen = Screen::Results;
                TransferIntent::None
            }
            Ok(_) => self.fail(unexpected()),
            Err(error) => self.fail(error),
        }
    }

    fn fail(&mut self, error: CoreError) -> TransferIntent {
        let message = error.to_string();
        self.note = Some(message.clone());
        TransferIntent::Message(message)
    }

    fn tag_all(&mut self, backend: &dyn Backend) -> TransferIntent {
        let note_ids: Vec<i64> = self
            .groups
            .iter()
            .flat_map(|g| g.note_ids.clone())
            .collect();
        match transfer(backend, TransferCommand::TagDuplicates { note_ids }) {
            Ok(TransferResponse::Tagged { count, changes }) => {
                self.note = Some(format!("Tagged {count} notes \"duplicate\""));
                TransferIntent::Changed(changes)
            }
            Ok(_) => self.fail(unexpected()),
            Err(error) => self.fail(error),
        }
    }

    pub fn on_key(&mut self, key: KeyEvent, backend: &dyn Backend) -> TransferIntent {
        match self.screen {
            Screen::Form => {
                if is_cancel(&key) {
                    return TransferIntent::Close;
                }
                if key.code == KeyCode::Enter {
                    return self.find(backend);
                }
                if move_focus(&key, &mut self.focus, 2) {
                    return TransferIntent::None;
                }
                if self.focus == 0 {
                    let count = self.fields.len();
                    cycle(&key, &mut self.field, count);
                } else {
                    self.search.handle_key(&key);
                }
                TransferIntent::None
            }
            Screen::Results => match key.code {
                _ if is_cancel(&key) => {
                    self.screen = Screen::Form;
                    TransferIntent::None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.selected = (self.selected + 1).min(self.groups.len() - 1);
                    TransferIntent::None
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.selected = self.selected.saturating_sub(1);
                    TransferIntent::None
                }
                KeyCode::Enter => {
                    let ids = &self.groups[self.selected].note_ids;
                    let list: Vec<String> = ids.iter().map(i64::to_string).collect();
                    TransferIntent::BrowseSearch(format!("nid:{}", list.join(",")))
                }
                KeyCode::Char('t') => self.tag_all(backend),
                _ => TransferIntent::None,
            },
        }
    }

    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        match self.screen {
            Screen::Form => vec![
                ("Up/Down", "row"),
                ("Left/Right", "field"),
                ("Enter", "find"),
                ("Esc", "close"),
            ],
            Screen::Results => vec![
                ("Enter", "show in browser"),
                ("t", "tag all duplicate"),
                ("Esc", "back"),
            ],
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        match self.screen {
            Screen::Form => {
                let inner = frame(area, buf, "Find Duplicates", 70, 7);
                let value_x = inner.x + 16;
                let width = inner.width.saturating_sub(16);
                put(
                    buf,
                    inner.x,
                    inner.y,
                    16,
                    "Search in",
                    if self.focus == 0 {
                        bold()
                    } else {
                        Style::default()
                    },
                );
                let field = format!("< {} >", self.fields[self.field]);
                put(
                    buf,
                    value_x,
                    inner.y,
                    width,
                    &field,
                    if self.focus == 0 {
                        rev()
                    } else {
                        Style::default()
                    },
                );
                put(
                    buf,
                    inner.x,
                    inner.y + 1,
                    16,
                    "Optional filter",
                    if self.focus == 1 {
                        bold()
                    } else {
                        Style::default()
                    },
                );
                draw_input(
                    buf,
                    value_x,
                    inner.y + 1,
                    width,
                    &self.search,
                    self.focus == 1,
                );
                let bottom = inner.bottom().saturating_sub(1);
                match &self.note {
                    Some(note) => put(buf, inner.x, bottom, inner.width, note, bold()),
                    None => put(
                        buf,
                        inner.x,
                        bottom,
                        inner.width,
                        "Enter finds, Esc closes",
                        dim(),
                    ),
                }
            }
            Screen::Results => {
                let height = (self.groups.len() as u16 + 6).min(area.height).max(8);
                let inner = frame(area, buf, "Duplicates", 80, height);
                let notes: usize = self.groups.iter().map(|g| g.note_ids.len()).sum();
                let header = format!(
                    "{} in {} groups ({})",
                    notes,
                    self.groups.len(),
                    self.fields[self.field]
                );
                put(buf, inner.x, inner.y, inner.width, &header, bold());
                let visible = inner.height.saturating_sub(4) as usize;
                let mut scroll = self.scroll.get();
                if self.selected < scroll {
                    scroll = self.selected;
                } else if visible > 0 && self.selected >= scroll + visible {
                    scroll = self.selected + 1 - visible;
                }
                self.scroll.set(scroll);
                for (row, group) in self.groups.iter().enumerate().skip(scroll).take(visible) {
                    let y = inner.y + 2 + (row - scroll) as u16;
                    let line = format!("{:>3}  {}", group.note_ids.len(), group.value);
                    let style = if row == self.selected {
                        rev()
                    } else {
                        Style::default()
                    };
                    put(buf, inner.x, y, inner.width, &line, style);
                }
                let bottom = inner.bottom().saturating_sub(1);
                match &self.note {
                    Some(note) => put(buf, inner.x, bottom, inner.width, note, bold()),
                    None => put(
                        buf,
                        inner.x,
                        bottom,
                        inner.width,
                        "Enter shows a group, t tags all, Esc goes back",
                        dim(),
                    ),
                }
            }
        }
    }
}
