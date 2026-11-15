//! The import widget: the field-mapping form for a text file, the running
//! import and the result log (R5 §5.4); and a `ForeignData` JSON import.
use super::*;
use std::path::PathBuf;
use tanki_core::transfer::{
    CsvDeck, CsvDelimiter, CsvMetadataInfo, CsvMetadataRequest, CsvNotetype, CsvSettings,
    DupeResolution, MatchScope,
};
use tanki_core::{DeckName, EditResponse, ImportLog, NoteCommand, NotetypeInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Delimiter,
    Html,
    Notetype,
    Deck,
    Field(usize),
    TagsColumn,
    GuidColumn,
    GlobalTags,
    UpdatedTags,
    Dupe,
    Scope,
}

enum Screen {
    Form,
    Running(Run),
    SchemaConfirm,
    Log,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Csv,
    Json,
}

pub struct ImportUi {
    path: PathBuf,
    kind: Kind,
    screen: Screen,
    info: Option<CsvMetadataInfo>,
    settings: CsvSettings,
    notetypes: Vec<NotetypeInfo>,
    decks: Vec<DeckName>,
    deck: TextInput,
    /// The deck is named in a column and the box was not touched.
    deck_from_column: bool,
    global_tags: TextInput,
    updated_tags: TextInput,
    focus: usize,
    /// Error from the last attempt, shown in the form.
    note: Option<String>,
    log: Option<(ImportLog, OpChanges)>,
    category: usize,
    scroll: usize,
}

impl ImportUi {
    /// Reads the file's metadata (header directives, delimiter, preview) and
    /// opens the mapping form.
    pub fn open_csv(backend: &dyn Backend, path: impl Into<PathBuf>) -> Result<Self, CoreError> {
        let path = path.into();
        let info = metadata(
            backend,
            CsvMetadataRequest {
                path: path.clone(),
                ..Default::default()
            },
        )?;
        let notetypes = match backend.execute(Command::Notes(NoteCommand::Notetypes))? {
            Response::Edit(EditResponse::Notetypes(all)) => all,
            _ => return Err(unexpected()),
        };
        let decks = match backend.execute(Command::Notes(NoteCommand::DeckNames {
            include_filtered: false,
        }))? {
            Response::Edit(EditResponse::DeckNames(all)) => all,
            _ => return Err(unexpected()),
        };
        let settings = info.settings.clone();
        let mut ui = Self {
            path,
            kind: Kind::Csv,
            screen: Screen::Form,
            info: Some(info),
            deck: TextInput::default(),
            deck_from_column: false,
            global_tags: TextInput::new(settings.global_tags.join(" ")),
            updated_tags: TextInput::new(settings.updated_tags.join(" ")),
            settings,
            notetypes,
            decks,
            focus: 0,
            note: None,
            log: None,
            category: 0,
            scroll: 0,
        };
        ui.sync_deck_input();
        Ok(ui)
    }

    /// Starts a `ForeignData` JSON import at once; `Esc` cancels it.
    pub fn open_json(backend: &dyn Backend, path: impl Into<PathBuf>) -> Result<Self, CoreError> {
        let path = path.into();
        let run = Run::start(
            backend,
            TransferCommand::ImportJson {
                path: path.clone(),
                schema_confirmed: false,
            },
        )?;
        Ok(Self {
            path,
            kind: Kind::Json,
            screen: Screen::Running(run),
            info: None,
            settings: CsvSettings::default(),
            notetypes: Vec::new(),
            decks: Vec::new(),
            deck: TextInput::default(),
            deck_from_column: false,
            global_tags: TextInput::default(),
            updated_tags: TextInput::default(),
            focus: 0,
            note: None,
            log: None,
            category: 0,
            scroll: 0,
        })
    }

    /// True while an import runs (the app keeps ticking and shows progress).
    pub fn is_running(&self) -> bool {
        matches!(self.screen, Screen::Running(_))
    }

    /// The settings the next import would use.
    pub fn settings(&self) -> CsvSettings {
        self.current_settings()
    }

    fn sync_deck_input(&mut self) {
        match &self.settings.deck {
            Some(CsvDeck::Name(name)) => self.deck = TextInput::new(name.clone()),
            Some(CsvDeck::Id(id)) => {
                let name = self
                    .decks
                    .iter()
                    .find(|d| d.id == *id)
                    .map(|d| d.name.clone());
                self.deck = TextInput::new(name.unwrap_or_default());
            }
            Some(CsvDeck::Column(_)) => {
                self.deck = TextInput::default();
                self.deck_from_column = true;
            }
            None => self.deck = TextInput::default(),
        }
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::Delimiter, Row::Html, Row::Notetype, Row::Deck];
        if matches!(self.settings.notetype, Some(CsvNotetype::Global { .. })) {
            let fields = self.info.as_ref().map_or(0, |i| i.field_names.len());
            rows.extend((0..fields).map(Row::Field));
        }
        rows.extend([
            Row::TagsColumn,
            Row::GuidColumn,
            Row::GlobalTags,
            Row::UpdatedTags,
            Row::Dupe,
            Row::Scope,
        ]);
        rows
    }

    fn columns(&self) -> usize {
        self.settings.columns()
    }

    fn current_settings(&self) -> CsvSettings {
        let mut settings = self.settings.clone();
        let name = self.deck.text().trim();
        if !(self.deck_from_column && name.is_empty()) && !name.is_empty() {
            settings.deck = Some(CsvDeck::Name(name.to_owned()));
        }
        let tags = |input: &TextInput| input.text().split_whitespace().map(str::to_owned).collect();
        settings.global_tags = tags(&self.global_tags);
        settings.updated_tags = tags(&self.updated_tags);
        settings
    }

    fn reload(
        &mut self,
        backend: &dyn Backend,
        delimiter: Option<CsvDelimiter>,
        notetype_id: Option<i64>,
        is_html: Option<bool>,
        whole: bool,
    ) {
        let request = CsvMetadataRequest {
            path: self.path.clone(),
            delimiter,
            notetype_id,
            deck_id: None,
            is_html,
        };
        match metadata(backend, request) {
            Ok(info) => {
                if whole {
                    let keep = (self.settings.dupe_resolution, self.settings.match_scope);
                    self.settings = info.settings.clone();
                    self.settings.dupe_resolution = keep.0;
                    self.settings.match_scope = keep.1;
                } else {
                    self.settings.notetype = info.settings.notetype.clone();
                }
                self.info = Some(info);
                self.note = None;
            }
            Err(error) => self.note = Some(error.to_string()),
        }
    }

    fn global_id(&self) -> Option<i64> {
        match self.settings.notetype {
            Some(CsvNotetype::Global { id, .. }) => Some(id),
            _ => None,
        }
    }

    fn start(&mut self, backend: &dyn Backend, confirmed: bool) -> TransferIntent {
        let command = match self.kind {
            Kind::Csv => {
                if self.settings.notetype.is_none() {
                    self.note = Some("Choose a note type".into());
                    return TransferIntent::None;
                }
                TransferCommand::ImportCsv {
                    path: self.path.clone(),
                    settings: self.current_settings(),
                }
            }
            Kind::Json => TransferCommand::ImportJson {
                path: self.path.clone(),
                schema_confirmed: confirmed,
            },
        };
        match Run::start(backend, command) {
            Ok(run) => {
                self.note = None;
                self.screen = Screen::Running(run);
                TransferIntent::None
            }
            Err(error) => {
                self.note = Some(error.to_string());
                TransferIntent::None
            }
        }
    }

    /// Polls the running import. Call from the event loop's tick.
    pub fn tick(&mut self, backend: &dyn Backend) -> TransferIntent {
        let Screen::Running(run) = &mut self.screen else {
            return TransferIntent::None;
        };
        match run.poll(backend) {
            Ok(None) => TransferIntent::None,
            Ok(Some(TransferResponse::Imported { log, changes })) => {
                self.log = Some((log, changes));
                self.category = 0;
                self.scroll = 0;
                self.screen = Screen::Log;
                TransferIntent::None
            }
            Ok(Some(_)) => self.fail(unexpected().to_string()),
            Err(CoreError::SchemaChangeRequired) if self.kind == Kind::Json => {
                self.screen = Screen::SchemaConfirm;
                TransferIntent::None
            }
            Err(CoreError::Interrupted) => {
                self.fail("Import cancelled; nothing was imported".into())
            }
            Err(error) => self.fail(error.to_string()),
        }
    }

    /// Back to the form (or closed, for JSON) with the error shown.
    fn fail(&mut self, message: String) -> TransferIntent {
        match self.kind {
            Kind::Csv => {
                self.note = Some(message.clone());
                self.screen = Screen::Form;
                TransferIntent::Message(message)
            }
            Kind::Json => {
                self.note = Some(message.clone());
                self.screen = Screen::Log;
                self.log = None;
                TransferIntent::Message(message)
            }
        }
    }

    pub fn on_key(&mut self, key: KeyEvent, backend: &dyn Backend) -> TransferIntent {
        match &mut self.screen {
            Screen::Running(run) => {
                if is_cancel(&key) && !run.cancelling {
                    run.cancel(backend);
                }
                TransferIntent::None
            }
            Screen::SchemaConfirm => {
                if key.code == KeyCode::Char('y') {
                    self.start(backend, true)
                } else {
                    TransferIntent::Close
                }
            }
            Screen::Log => self.log_key(key),
            Screen::Form => self.form_key(key, backend),
        }
    }

    fn log_key(&mut self, key: KeyEvent) -> TransferIntent {
        let categories = self.log.as_ref().map_or(0, |(log, _)| log.entries.len());
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                return match self.log.take() {
                    Some((log, changes)) => TransferIntent::Done {
                        message: Some(summary(&log)),
                        changes,
                    },
                    None => TransferIntent::Close,
                }
            }
            KeyCode::Right | KeyCode::Tab | KeyCode::Left | KeyCode::BackTab => {
                let forward = matches!(key.code, KeyCode::Right | KeyCode::Tab);
                if categories > 0 {
                    let step = if forward { 1 } else { categories - 1 };
                    self.category = (self.category + step) % categories;
                    self.scroll = 0;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => self.scroll += 1,
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageDown => self.scroll += 10,
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            _ => {}
        }
        TransferIntent::None
    }

    fn form_key(&mut self, key: KeyEvent, backend: &dyn Backend) -> TransferIntent {
        if is_cancel(&key) {
            return TransferIntent::Close;
        }
        if key.code == KeyCode::Enter {
            return self.start(backend, false);
        }
        let rows = self.rows();
        if move_focus(&key, &mut self.focus, rows.len()) {
            return TransferIntent::None;
        }
        self.focus = self.focus.min(rows.len() - 1);
        let columns = self.columns();
        match rows[self.focus] {
            Row::Delimiter => {
                let mut at = CsvDelimiter::ALL
                    .iter()
                    .position(|d| *d == self.settings.delimiter)
                    .unwrap_or(0);
                if cycle(&key, &mut at, CsvDelimiter::ALL.len()) {
                    let delimiter = CsvDelimiter::ALL[at];
                    self.reload(backend, Some(delimiter), self.global_id(), None, true);
                }
            }
            Row::Html => {
                if matches!(
                    key.code,
                    KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                ) {
                    self.settings.is_html = !self.settings.is_html;
                }
            }
            Row::Notetype => {
                let count = self.notetypes.len();
                let current = self
                    .global_id()
                    .and_then(|id| self.notetypes.iter().position(|n| n.id == id));
                let mut at = current.unwrap_or(count.saturating_sub(1));
                if cycle(&key, &mut at, count) {
                    let id = self.notetypes[at].id;
                    // Anki re-guesses the column mapping for the new type.
                    self.reload(backend, None, Some(id), None, false);
                }
            }
            Row::Deck => {
                if self.deck.handle_key(&key) {
                    self.deck_from_column = false;
                }
            }
            Row::Field(index) => {
                if let Some(CsvNotetype::Global { field_columns, .. }) = &mut self.settings.notetype
                {
                    if let Some(slot) = field_columns.get_mut(index) {
                        let mut at = *slot as usize;
                        if cycle(&key, &mut at, columns + 1) {
                            *slot = at as u32;
                        }
                    }
                }
            }
            Row::TagsColumn => {
                let mut at = self.settings.tags_column as usize;
                if cycle(&key, &mut at, columns + 1) {
                    self.settings.tags_column = at as u32;
                }
            }
            Row::GuidColumn => {
                let mut at = self.settings.guid_column as usize;
                if cycle(&key, &mut at, columns + 1) {
                    self.settings.guid_column = at as u32;
                }
            }
            Row::GlobalTags => {
                self.global_tags.handle_key(&key);
            }
            Row::UpdatedTags => {
                self.updated_tags.handle_key(&key);
            }
            Row::Dupe => {
                let mut at = DupeResolution::ALL
                    .iter()
                    .position(|d| *d == self.settings.dupe_resolution)
                    .unwrap_or(0);
                if cycle(&key, &mut at, DupeResolution::ALL.len()) {
                    self.settings.dupe_resolution = DupeResolution::ALL[at];
                }
            }
            Row::Scope => {
                let mut at = MatchScope::ALL
                    .iter()
                    .position(|d| *d == self.settings.match_scope)
                    .unwrap_or(0);
                if cycle(&key, &mut at, MatchScope::ALL.len()) {
                    self.settings.match_scope = MatchScope::ALL[at];
                }
            }
        }
        TransferIntent::None
    }

    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        match &self.screen {
            Screen::Form => vec![
                ("Up/Down", "row"),
                ("Left/Right", "change"),
                ("Enter", "import"),
                ("Esc", "close"),
            ],
            Screen::Running(_) => vec![("Esc", "cancel")],
            Screen::SchemaConfirm => vec![("y", "import"), ("any", "cancel")],
            Screen::Log => vec![
                ("Left/Right", "list"),
                ("Up/Down", "scroll"),
                ("Esc", "close"),
            ],
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let title = format!("Import {}", file_name(&self.path));
        match &self.screen {
            Screen::Form => self.render_form(area, buf, &title),
            Screen::Running(run) => {
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
            }
            Screen::SchemaConfirm => {
                let inner = frame(area, buf, &title, 70, 8);
                let text = "This file adds note types, which requires a one-way full sync (upload) on your next sync; every other device must then download.";
                for (i, line) in wrap(text, inner.width as usize).iter().enumerate() {
                    put(
                        buf,
                        inner.x,
                        inner.y + i as u16,
                        inner.width,
                        line,
                        Style::default(),
                    );
                }
                put(
                    buf,
                    inner.x,
                    inner.bottom() - 1,
                    inner.width,
                    "y imports, any other key cancels",
                    dim(),
                );
            }
            Screen::Log => self.render_log(area, buf, &title),
        }
    }

    fn render_form(&self, area: Rect, buf: &mut Buffer, title: &str) {
        let info = self.info.as_ref();
        let preview = info.map_or(0, |i| i.preview.len().min(5));
        let rows = self.rows();
        let height = (rows.len() + preview + 5) as u16;
        let inner = frame(area, buf, title, 96, height);
        let label_width = 18u16;
        let value_x = inner.x + label_width + 1;
        let value_width = inner.width.saturating_sub(label_width + 1);
        for (i, row) in rows.iter().enumerate() {
            let y = inner.y + i as u16;
            if y >= inner.bottom() {
                break;
            }
            let focused = i == self.focus;
            let (label, value) = self.row_text(*row);
            let style = if focused { bold() } else { Style::default() };
            put(buf, inner.x, y, label_width, &label, style);
            match row {
                Row::Deck => {
                    if self.deck_from_column && self.deck.text().is_empty() {
                        let column = match self.settings.deck {
                            Some(CsvDeck::Column(c)) => c,
                            _ => 0,
                        };
                        put(
                            buf,
                            value_x,
                            y,
                            value_width,
                            &format!("(named in column {column})"),
                            dim(),
                        );
                    } else {
                        draw_input(buf, value_x, y, value_width, &self.deck, focused);
                    }
                }
                Row::GlobalTags => {
                    draw_input(buf, value_x, y, value_width, &self.global_tags, focused)
                }
                Row::UpdatedTags => {
                    draw_input(buf, value_x, y, value_width, &self.updated_tags, focused)
                }
                _ => {
                    let text = if focused {
                        format!("< {value} >")
                    } else {
                        value
                    };
                    put(
                        buf,
                        value_x,
                        y,
                        value_width,
                        &text,
                        if focused { rev() } else { Style::default() },
                    );
                }
            }
        }
        let mut y = inner.y + rows.len() as u16 + 1;
        if let Some(info) = info {
            put(buf, inner.x, y, inner.width, "Preview", dim());
            for row in info.preview.iter().take(5) {
                y += 1;
                if y >= inner.bottom() {
                    break;
                }
                let cells: Vec<String> = row
                    .iter()
                    .enumerate()
                    .map(|(i, c)| format!("{}: {c}", i + 1))
                    .collect();
                put(
                    buf,
                    inner.x,
                    y,
                    inner.width,
                    &cells.join(" | "),
                    Style::default(),
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
                "Enter imports, Esc closes",
                dim(),
            ),
        }
    }

    fn column_text(&self, column: u32) -> String {
        if column == 0 {
            return "(none)".into();
        }
        let label = self
            .settings
            .column_labels
            .get(column as usize - 1)
            .filter(|l| !l.is_empty());
        let sample = self
            .info
            .as_ref()
            .and_then(|i| i.preview.first())
            .and_then(|r| r.get(column as usize - 1))
            .map(|s| format!(" e.g. {s}"))
            .unwrap_or_default();
        match label {
            Some(label) => format!("column {column} ({label}){sample}"),
            None => format!("column {column}{sample}"),
        }
    }

    fn row_text(&self, row: Row) -> (String, String) {
        let s = &self.settings;
        match row {
            Row::Delimiter => ("Delimiter".into(), s.delimiter.name().into()),
            Row::Html => ("Fields hold HTML".into(), check(s.is_html).into()),
            Row::Notetype => (
                "Note type".into(),
                match (
                    &s.notetype,
                    self.info.as_ref().and_then(|i| i.notetype_name.clone()),
                ) {
                    (Some(CsvNotetype::Global { .. }), Some(name)) => name,
                    (Some(CsvNotetype::Column(c)), _) => format!("named in column {c}"),
                    _ => "(choose)".into(),
                },
            ),
            Row::Deck => ("Deck".into(), String::new()),
            Row::Field(i) => {
                let name = self
                    .info
                    .as_ref()
                    .and_then(|info| info.field_names.get(i).cloned())
                    .unwrap_or_default();
                let column = match &s.notetype {
                    Some(CsvNotetype::Global { field_columns, .. }) => {
                        field_columns.get(i).copied().unwrap_or(0)
                    }
                    _ => 0,
                };
                (format!("  {name}"), self.column_text(column))
            }
            Row::TagsColumn => ("Tags column".into(), self.column_text(s.tags_column)),
            Row::GuidColumn => ("GUID column".into(), self.column_text(s.guid_column)),
            Row::GlobalTags => ("Tags for all".into(), String::new()),
            Row::UpdatedTags => ("Tags if updated".into(), String::new()),
            Row::Dupe => ("If a note matches".into(), s.dupe_resolution.name().into()),
            Row::Scope => ("Match within".into(), s.match_scope.name().into()),
        }
    }

    fn render_log(&self, area: Rect, buf: &mut Buffer, title: &str) {
        let inner = frame(
            area,
            buf,
            &format!("{title}: result"),
            90,
            area.height.min(24),
        );
        let Some((log, _)) = &self.log else {
            let message = self.note.clone().unwrap_or_else(|| "No result".into());
            put(
                buf,
                inner.x,
                inner.y,
                inner.width,
                &message,
                Style::default(),
            );
            put(
                buf,
                inner.x,
                inner.bottom() - 1,
                inner.width,
                "Esc closes",
                dim(),
            );
            return;
        };
        put(buf, inner.x, inner.y, inner.width, &summary(log), bold());
        let names = ordered(log);
        if names.is_empty() {
            put(
                buf,
                inner.x,
                inner.y + 2,
                inner.width,
                "Nothing was listed.",
                dim(),
            );
        } else {
            let tabs: Vec<String> = names
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    let label = format!("{} {}", name.replace('_', " "), log.entries[*name].len());
                    if i == self.category.min(names.len() - 1) {
                        format!("[{label}]")
                    } else {
                        label
                    }
                })
                .collect();
            put(
                buf,
                inner.x,
                inner.y + 2,
                inner.width,
                &tabs.join("  "),
                Style::default(),
            );
            let notes = &log.entries[names[self.category.min(names.len() - 1)]];
            let visible = inner.height.saturating_sub(5) as usize;
            let skip = self.scroll.min(notes.len().saturating_sub(visible));
            for (row, fields) in notes.iter().skip(skip).take(visible).enumerate() {
                put(
                    buf,
                    inner.x,
                    inner.y + 4 + row as u16,
                    inner.width,
                    &fields.join(" | "),
                    Style::default(),
                );
            }
        }
        put(
            buf,
            inner.x,
            inner.bottom() - 1,
            inner.width,
            "Left/Right list, Up/Down scroll, Esc closes",
            dim(),
        );
    }
}

fn metadata(
    backend: &dyn Backend,
    request: CsvMetadataRequest,
) -> Result<CsvMetadataInfo, CoreError> {
    match transfer(backend, TransferCommand::CsvMetadata(request))? {
        TransferResponse::CsvMetadata(info) => Ok(*info),
        _ => Err(unexpected()),
    }
}

/// One line for the message bar and the log's header.
pub(crate) fn summary(log: &ImportLog) -> String {
    let mut text = format!(
        "{} notes found: {} new, {} updated, {} duplicate",
        log.found_notes, log.new, log.updated, log.duplicate
    );
    if log.conflicting > 0 {
        text.push_str(&format!(", {} conflicting", log.conflicting));
    }
    for name in [
        "first_field_match",
        "missing_notetype",
        "missing_deck",
        "empty_first_field",
    ] {
        if let Some(notes) = log.entries.get(name) {
            text.push_str(&format!(", {} {}", notes.len(), name.replace('_', " ")));
        }
    }
    text
}

/// The log's lists in Anki's order, not alphabetical.
fn ordered(log: &ImportLog) -> Vec<&String> {
    const ORDER: [&str; 8] = [
        "new",
        "updated",
        "duplicate",
        "conflicting",
        "first_field_match",
        "missing_notetype",
        "missing_deck",
        "empty_first_field",
    ];
    let mut names: Vec<&String> = log.entries.keys().collect();
    names.sort_by_key(|n| ORDER.iter().position(|o| o == n).unwrap_or(ORDER.len()));
    names
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.len() + 1 + word.len() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}
