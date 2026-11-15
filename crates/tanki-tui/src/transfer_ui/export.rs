//! Notes or cards as a text file (R5 §6): the browser's selection or a deck.
use super::*;
use std::path::PathBuf;
use tanki_core::transfer::TextExportSelection;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Path,
    Kind,
    Html,
    Tags,
    Deck,
    Notetype,
    Guid,
}

enum Screen {
    Form,
    Running(Run),
}

pub struct ExportUi {
    label: String,
    selection: TextExportSelection,
    path: TextInput,
    cards: bool,
    html: bool,
    tags: bool,
    deck: bool,
    notetype: bool,
    guid: bool,
    focus: usize,
    screen: Screen,
    note: Option<String>,
    /// The file exists and Enter was pressed once.
    overwrite_warned: bool,
}

impl ExportUi {
    /// `label` names the selection for the title ("Deck French", "12 selected
    /// notes"). Note ids force a notes export and card ids a cards export;
    /// the other selections offer both. `path` is the suggested file.
    pub fn new(label: impl Into<String>, selection: TextExportSelection, path: &str) -> Self {
        let cards = matches!(selection, TextExportSelection::CardIds(_));
        Self {
            label: label.into(),
            selection,
            path: TextInput::new(path),
            cards,
            html: false,
            tags: true,
            deck: false,
            notetype: false,
            guid: false,
            focus: 0,
            screen: Screen::Form,
            note: None,
            overwrite_warned: false,
        }
    }

    fn kind_fixed(&self) -> bool {
        matches!(
            self.selection,
            TextExportSelection::NoteIds(_) | TextExportSelection::CardIds(_)
        )
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::Path, Row::Kind, Row::Html];
        if !self.cards {
            rows.extend([Row::Tags, Row::Deck, Row::Notetype, Row::Guid]);
        }
        rows
    }

    fn command(&self) -> TransferCommand {
        let path = PathBuf::from(self.path.text().trim());
        if self.cards {
            TransferCommand::ExportCards {
                path,
                selection: self.selection.clone(),
                with_html: self.html,
            }
        } else {
            TransferCommand::ExportNotes {
                path,
                selection: self.selection.clone(),
                with_html: self.html,
                with_tags: self.tags,
                with_deck: self.deck,
                with_notetype: self.notetype,
                with_guid: self.guid,
            }
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self.screen, Screen::Running(_))
    }

    /// Polls the running export. Call from the event loop's tick.
    pub fn tick(&mut self, backend: &dyn Backend) -> TransferIntent {
        let Screen::Running(run) = &mut self.screen else {
            return TransferIntent::None;
        };
        match run.poll(backend) {
            Ok(None) => TransferIntent::None,
            Ok(Some(TransferResponse::Exported { count })) => {
                let what = if self.cards { "card" } else { "note" };
                TransferIntent::Done {
                    changes: OpChanges::default(),
                    message: Some(format!(
                        "Exported {count} {what}{} to {}",
                        if count == 1 { "" } else { "s" },
                        self.path.text().trim()
                    )),
                }
            }
            Ok(Some(_)) => self.fail(unexpected().to_string()),
            Err(CoreError::Interrupted) => self.fail("Export cancelled".into()),
            Err(error) => self.fail(error.to_string()),
        }
    }

    fn fail(&mut self, message: String) -> TransferIntent {
        self.note = Some(message.clone());
        self.screen = Screen::Form;
        TransferIntent::Message(message)
    }

    pub fn on_key(&mut self, key: KeyEvent, backend: &dyn Backend) -> TransferIntent {
        if let Screen::Running(run) = &mut self.screen {
            if is_cancel(&key) && !run.cancelling {
                run.cancel(backend);
            }
            return TransferIntent::None;
        }
        if is_cancel(&key) {
            return TransferIntent::Close;
        }
        if key.code == KeyCode::Enter {
            return self.submit(backend);
        }
        let rows = self.rows();
        if move_focus(&key, &mut self.focus, rows.len()) {
            return TransferIntent::None;
        }
        self.focus = self.focus.min(rows.len() - 1);
        let toggle = matches!(
            key.code,
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
        );
        match rows[self.focus] {
            Row::Path => {
                if self.path.handle_key(&key) {
                    self.overwrite_warned = false;
                }
            }
            Row::Kind if toggle && !self.kind_fixed() => self.cards = !self.cards,
            Row::Html if toggle => self.html = !self.html,
            Row::Tags if toggle => self.tags = !self.tags,
            Row::Deck if toggle => self.deck = !self.deck,
            Row::Notetype if toggle => self.notetype = !self.notetype,
            Row::Guid if toggle => self.guid = !self.guid,
            _ => {}
        }
        TransferIntent::None
    }

    fn submit(&mut self, backend: &dyn Backend) -> TransferIntent {
        let text = self.path.text().trim().to_owned();
        if text.is_empty() {
            self.note = Some("Enter a file name".into());
            return TransferIntent::None;
        }
        if std::path::Path::new(&text).exists() && !self.overwrite_warned {
            self.overwrite_warned = true;
            self.note = Some(format!("{text} exists: Enter again to overwrite it"));
            return TransferIntent::None;
        }
        match Run::start(backend, self.command()) {
            Ok(run) => {
                self.note = None;
                self.screen = Screen::Running(run);
            }
            Err(error) => self.note = Some(error.to_string()),
        }
        TransferIntent::None
    }

    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        match self.screen {
            Screen::Form => vec![
                ("Up/Down", "row"),
                ("Space", "toggle"),
                ("Enter", "export"),
                ("Esc", "close"),
            ],
            Screen::Running(_) => vec![("Esc", "cancel")],
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let title = format!("Export {}", self.label);
        if let Screen::Running(run) = &self.screen {
            let inner = frame(area, buf, &title, 60, 5);
            put(
                buf,
                inner.x,
                inner.y,
                inner.width,
                &run.line(),
                Style::default(),
            );
            put(buf, inner.x, inner.y + 2, inner.width, "Esc cancels", dim());
            return;
        }
        let rows = self.rows();
        let inner = frame(area, buf, &title, 80, rows.len() as u16 + 4);
        let value_x = inner.x + 18;
        let value_width = inner.width.saturating_sub(18);
        for (i, row) in rows.iter().enumerate() {
            let y = inner.y + i as u16;
            let focused = i == self.focus;
            let (label, value) = match row {
                Row::Path => ("File", String::new()),
                Row::Kind => (
                    "Export",
                    if self.cards {
                        "cards (question, answer)"
                    } else {
                        "notes"
                    }
                    .to_owned(),
                ),
                Row::Html => ("Include HTML", check(self.html).to_owned()),
                Row::Tags => ("Tags column", check(self.tags).to_owned()),
                Row::Deck => ("Deck column", check(self.deck).to_owned()),
                Row::Notetype => ("Note type column", check(self.notetype).to_owned()),
                Row::Guid => ("GUID column", check(self.guid).to_owned()),
            };
            put(
                buf,
                inner.x,
                y,
                16,
                label,
                if focused { bold() } else { Style::default() },
            );
            if *row == Row::Path {
                draw_input(buf, value_x, y, value_width, &self.path, focused);
            } else {
                put(
                    buf,
                    value_x,
                    y,
                    value_width,
                    &value,
                    if focused { rev() } else { Style::default() },
                );
            }
        }
        let bottom = inner.bottom().saturating_sub(1);
        match &self.note {
            Some(note) => put(buf, inner.x, bottom, inner.width, note, bold()),
            None => put(
                buf,
                inner.x,
                bottom,
                inner.width,
                "Enter exports, Esc closes",
                dim(),
            ),
        }
    }
}
