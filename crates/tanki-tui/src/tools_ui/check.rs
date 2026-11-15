//! Check Database, Check Media and Empty Cards.
use super::{
    bold, dim, is_cancel, put, rev, run, start, unexpected, wrap, Result, Step, ToolsIntent,
};
use crate::backend::{Backend, CommandOp};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{buffer::Buffer, layout::Rect, style::Style};
use tanki_core::maintenance::{
    DatabaseReport, EmptyCardsReport, MaintenanceCommand as Cmd, MaintenanceResponse as Resp,
    MediaReport,
};
use tanki_core::{CoreError, OperationProgress};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckKind {
    Database,
    Media,
    EmptyCards,
}

/// A question waiting for `y`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ask {
    TrashUnused,
    EmptyTrash,
    DeleteEmpty,
}

enum Phase {
    /// Says what will change; `y` starts.
    Confirm,
    Running(Box<dyn CommandOp>),
    Database(DatabaseReport),
    Media(MediaReport),
    Empty(EmptyCardsReport),
    Failed(String),
}

pub(crate) struct CheckUi {
    kind: CheckKind,
    phase: Phase,
    progress: OperationProgress,
    scroll: usize,
    ask: Option<Ask>,
    keep_notes: bool,
    /// The last action's outcome, shown under the report.
    note: Option<String>,
}

impl CheckUi {
    pub fn new(kind: CheckKind) -> Self {
        Self {
            kind,
            phase: Phase::Confirm,
            progress: OperationProgress::default(),
            scroll: 0,
            ask: None,
            keep_notes: true,
            note: None,
        }
    }

    /// Empty cards need no confirmation: finding them is read-only.
    pub fn start_empty_cards(backend: &dyn Backend) -> Result<Self> {
        let mut ui = Self::new(CheckKind::EmptyCards);
        ui.phase = Phase::Running(start(backend, Cmd::FindEmptyCards)?);
        Ok(ui)
    }

    pub fn is_running(&self) -> bool {
        matches!(self.phase, Phase::Running(_))
    }

    fn begin(&mut self, backend: &dyn Backend) {
        let command = match self.kind {
            CheckKind::Database => Cmd::CheckDatabase { confirmed: true },
            CheckKind::Media => Cmd::CheckMedia,
            CheckKind::EmptyCards => Cmd::FindEmptyCards,
        };
        self.progress = OperationProgress::default();
        self.phase = match start(backend, command) {
            Ok(op) => Phase::Running(op),
            Err(error) => Phase::Failed(error.to_string()),
        };
    }

    pub fn tick(&mut self, backend: &dyn Backend) -> ToolsIntent {
        let Phase::Running(op) = &self.phase else {
            return ToolsIntent::None;
        };
        let polled = op.poll();
        match polled {
            Ok(None) => {
                self.progress = backend.data_progress();
                ToolsIntent::None
            }
            Ok(Some(response)) => {
                match super::into_response(response) {
                    Ok(Resp::DatabaseChecked(report)) => {
                        self.phase = Phase::Database(report);
                        return ToolsIntent::Changed(tanki_core::OpChanges::everything());
                    }
                    Ok(Resp::MediaChecked(report)) => {
                        self.phase = Phase::Media(report);
                        return ToolsIntent::Changed(tanki_core::OpChanges::everything());
                    }
                    Ok(Resp::EmptyCards(report)) => self.phase = Phase::Empty(report),
                    Ok(_) => self.phase = Phase::Failed(unexpected().to_string()),
                    Err(error) => self.phase = Phase::Failed(error.to_string()),
                }
                self.scroll = 0;
                ToolsIntent::None
            }
            Err(CoreError::Interrupted) => {
                self.phase = Phase::Failed("Cancelled".into());
                ToolsIntent::None
            }
            Err(error) => {
                self.phase = Phase::Failed(error.to_string());
                ToolsIntent::None
            }
        }
    }

    pub fn on_key(&mut self, key: KeyEvent, backend: &dyn Backend) -> Step {
        if matches!(self.phase, Phase::Running(_)) {
            if is_cancel(&key) {
                backend.cancel_data();
            }
            return Step::Stay;
        }
        if let Some(ask) = self.ask.take() {
            return match key.code {
                KeyCode::Char('y' | 'Y') => self.confirmed(ask, backend),
                _ => Step::Stay,
            };
        }
        match (&self.phase, key.code) {
            (Phase::Confirm, KeyCode::Char('y' | 'Y')) => {
                self.begin(backend);
                return Step::Stay;
            }
            (_, KeyCode::Esc | KeyCode::Char('q')) => return Step::Back,
            (Phase::Media(r), KeyCode::Char('t')) if !r.unused.is_empty() => {
                self.ask = Some(Ask::TrashUnused)
            }
            (Phase::Media(_), KeyCode::Char('e')) => self.ask = Some(Ask::EmptyTrash),
            (Phase::Media(_), KeyCode::Char('r')) => return self.restore_trash(backend),
            (Phase::Empty(r), KeyCode::Char('d'))
                if !r.cards_to_delete(self.keep_notes).is_empty() =>
            {
                self.ask = Some(Ask::DeleteEmpty)
            }
            (Phase::Empty(_), KeyCode::Char('k')) => self.keep_notes = !self.keep_notes,
            (_, KeyCode::Up) => self.scroll = self.scroll.saturating_sub(1),
            (_, KeyCode::Down) => self.scroll += 1,
            (_, KeyCode::PageUp) => self.scroll = self.scroll.saturating_sub(10),
            (_, KeyCode::PageDown) => self.scroll += 10,
            _ => {}
        }
        Step::Stay
    }

    fn confirmed(&mut self, ask: Ask, backend: &dyn Backend) -> Step {
        match (ask, &mut self.phase) {
            (Ask::TrashUnused, Phase::Media(report)) => {
                let files = report.unused.clone();
                match run(backend, Cmd::TrashMedia { files }) {
                    Ok(Resp::MediaTrashed(n)) => {
                        report.unused.clear();
                        report.have_trash = report.have_trash || n > 0;
                        self.note = Some(format!("Moved {n} unused file(s) to the media trash."));
                    }
                    Ok(_) => self.note = Some(unexpected().to_string()),
                    Err(e) => self.note = Some(e.to_string()),
                }
                Step::Stay
            }
            (Ask::EmptyTrash, Phase::Media(report)) => {
                match run(backend, Cmd::EmptyMediaTrash) {
                    Ok(_) => {
                        report.have_trash = false;
                        self.note = Some("Media trash emptied.".into());
                    }
                    Err(e) => self.note = Some(e.to_string()),
                }
                Step::Stay
            }
            (Ask::DeleteEmpty, Phase::Empty(_)) => {
                match run(
                    backend,
                    Cmd::DeleteEmptyCards {
                        keep_notes: self.keep_notes,
                    },
                ) {
                    Ok(Resp::EmptyCardsDeleted { cards, changes }) => {
                        self.phase = Phase::Empty(EmptyCardsReport {
                            report: String::new(),
                            notes: Vec::new(),
                        });
                        self.note = Some(format!("Deleted {cards} empty card(s)."));
                        return Step::Intent(ToolsIntent::Changed(changes));
                    }
                    Ok(_) => self.note = Some(unexpected().to_string()),
                    Err(e) => self.note = Some(e.to_string()),
                }
                Step::Stay
            }
            _ => Step::Stay,
        }
    }

    fn restore_trash(&mut self, backend: &dyn Backend) -> Step {
        match run(backend, Cmd::RestoreMediaTrash) {
            Ok(_) => {
                if let Phase::Media(report) = &mut self.phase {
                    report.have_trash = false;
                }
                self.note =
                    Some("Trash restored. Run Check Media again to refresh the lists.".into());
            }
            Err(e) => self.note = Some(e.to_string()),
        }
        Step::Stay
    }

    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.ask.is_some() {
            return vec![("y", "yes"), ("any", "no")];
        }
        match &self.phase {
            Phase::Confirm => vec![("y", "run"), ("Esc", "back")],
            Phase::Running(_) => vec![("Esc", "cancel")],
            Phase::Media(_) => vec![
                ("t", "trash unused"),
                ("e", "empty trash"),
                ("r", "restore trash"),
                ("Esc", "back"),
            ],
            Phase::Empty(_) => vec![("d", "delete"), ("k", "keep notes"), ("Esc", "back")],
            Phase::Database(_) | Phase::Failed(_) => vec![("Esc", "back")],
        }
    }

    fn title(&self) -> &'static str {
        match self.kind {
            CheckKind::Database => "Check Database",
            CheckKind::Media => "Check Media",
            CheckKind::EmptyCards => "Empty Cards",
        }
    }

    fn explanation(&self) -> &'static str {
        match self.kind {
            CheckKind::Database => {
                "Check Database will first write a backup of the collection. It then repairs \
problems it finds, rebuilds and optimizes the file, and clears the undo history.\n\n\
Some repairs force a one-way full sync (upload or download) on the next sync.\n\n\
Press y to run it, Esc to go back."
            }
            CheckKind::Media => {
                "Check Media will first write a backup of the collection. It then compares the \
media folder with your notes. It may rename media files whose names are invalid and rewrite the \
note fields that refer to them; there is no dry run.\n\n\
Afterwards you can move unused files to the media trash, restore the trash, or empty it for good.\n\n\
Press y to run it, Esc to go back."
            }
            CheckKind::EmptyCards => "",
        }
    }

    /// The report body, one line per row, before scrolling.
    fn body(&self, width: usize) -> Vec<(String, Style)> {
        let plain = Style::default();
        let mut lines: Vec<(String, Style)> = Vec::new();
        let mut text = |s: &str| {
            for l in wrap(s, width) {
                lines.push((l, plain));
            }
        };
        match &self.phase {
            Phase::Confirm => text(self.explanation()),
            Phase::Failed(message) => text(&format!("Failed: {message}")),
            Phase::Running(_) => {}
            Phase::Database(report) => {
                if let Some(b) = &report.backup {
                    text(&format!("Backup written: {b}"));
                }
                if report.problems.is_empty() {
                    text("Database rebuilt and optimized. No problems found.");
                } else {
                    text("Problems fixed:");
                    for p in &report.problems {
                        text(&format!("  {p}"));
                    }
                }
            }
            Phase::Media(report) => {
                if let Some(b) = &report.backup {
                    text(&format!("Backup written: {b}"));
                }
                if !report.report.trim().is_empty() {
                    text(report.report.trim_end());
                }
                text(&format!(
                    "Trash: {}",
                    if report.have_trash {
                        "has files"
                    } else {
                        "empty"
                    }
                ));
                lines.push((format!("Unused ({}):", report.unused.len()), bold()));
                lines.extend(report.unused.iter().map(|f| (format!("  {f}"), plain)));
                lines.push((format!("Missing ({}):", report.missing.len()), bold()));
                lines.extend(report.missing.iter().map(|f| (format!("  {f}"), plain)));
            }
            Phase::Empty(report) => {
                if report.is_empty() {
                    text("No empty cards.");
                } else {
                    text(report.report.trim_end());
                    let n = report.cards_to_delete(self.keep_notes).len();
                    text(&format!(
                        "\n{n} card(s) would be deleted. Keep notes with no valid cards: {} (k)",
                        if self.keep_notes { "yes" } else { "no" }
                    ));
                }
            }
        }
        lines
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        put(buf, area.x, area.y, area.width, self.title(), bold());
        let width = area.width as usize;
        let top = area.y + 2;
        let bottom = area.bottom().saturating_sub(2);
        if let Phase::Running(_) = &self.phase {
            let p = &self.progress;
            let line = match (p.current, p.total) {
                (Some(c), Some(t)) => format!("{} {c}/{t}", p.stage),
                _ => p.stage.clone(),
            };
            let shown = if line.is_empty() {
                "Working…".to_owned()
            } else {
                line
            };
            put(buf, area.x, top, area.width, &shown, Style::default());
            return;
        }
        let body = self.body(width);
        let rows = (bottom.saturating_sub(top)) as usize;
        let skip = self.scroll.min(body.len().saturating_sub(rows.max(1)));
        for (y, (line, style)) in (top..).zip(body.iter().skip(skip).take(rows)) {
            put(buf, area.x, y, area.width, line, *style);
        }
        if let Some(ask) = self.ask {
            let question = match ask {
                Ask::TrashUnused => {
                    let n = match &self.phase {
                        Phase::Media(r) => r.unused.len(),
                        _ => 0,
                    };
                    format!("Move {n} unused file(s) to the media trash (restorable)? y/n")
                }
                Ask::EmptyTrash => {
                    "Permanently delete every file in the media trash? This cannot be undone. y/n"
                        .to_owned()
                }
                Ask::DeleteEmpty => {
                    let n = match &self.phase {
                        Phase::Empty(r) => r.cards_to_delete(self.keep_notes).len(),
                        _ => 0,
                    };
                    format!("Delete {n} empty card(s)? y/n")
                }
            };
            put(buf, area.x, area.bottom() - 1, area.width, &question, rev());
        } else if let Some(note) = &self.note {
            put(buf, area.x, area.bottom() - 1, area.width, note, dim());
        }
    }
}
