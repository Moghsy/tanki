//! Package operations overlay (Phase 2, Packages stream): import options,
//! progress with cancel, schema-change confirmation, the import result log,
//! export progress, and the destructive restore confirmation.
//!
//! Pure state: [`PackageUi::on_key`], [`PackageUi::on_progress`] and
//! [`PackageUi::on_response`] change the state and return an [`Intent`] that
//! the app executes (start a data operation, cancel it, close the overlay).
//! Drawing goes through the [`Widget`] impl on `&PackageUi`. Only
//! `tanki_core` types are used.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, Paragraph, Widget, Wrap};
use std::path::PathBuf;
use tanki_core::{
    CoreError, DataCommand, DataResponse, ExportOptions, ExportSelection, ImportLog, ImportOptions,
    OperationProgress, PackageCommand, UpdateCondition,
};

/// What the app must do after a transition.
#[derive(Debug, Clone)]
pub enum Intent {
    None,
    /// Start this data operation (`CollectionActor::start_data`) and feed its
    /// progress and result back through `on_progress` / `on_response`.
    Start(DataCommand),
    /// Call `CollectionActor::cancel_operation`.
    Cancel,
    /// Remove the overlay. `refresh` is true when the collection changed.
    Close {
        refresh: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    Import,
    Export,
    Restore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Screen {
    Form,
    Running,
    SchemaConfirm,
    Log,
    RestoreConfirm,
    Message,
}

/// Rows of the import form, in order.
const ROWS: usize = 7;
const RESTORE_WORD: &str = "restore";

#[derive(Debug, Clone)]
pub struct PackageUi {
    path: PathBuf,
    operation: Operation,
    screen: Screen,
    /// Import form.
    use_presets: bool,
    options: ImportOptions,
    focus: usize,
    /// Export request, kept for the title.
    export: Option<(ExportOptions, ExportSelection)>,
    /// Running.
    progress: OperationProgress,
    cancelling: bool,
    /// Result log.
    log: Option<ImportLog>,
    scroll: usize,
    /// Restore confirmation input.
    typed: String,
    /// Final message, and whether the collection changed.
    message: String,
    changed: bool,
}

impl PackageUi {
    fn new(path: PathBuf, operation: Operation, screen: Screen) -> Self {
        Self {
            path,
            operation,
            screen,
            use_presets: true,
            options: ImportOptions::default(),
            focus: 0,
            export: None,
            progress: OperationProgress::default(),
            cancelling: false,
            log: None,
            scroll: 0,
            typed: String::new(),
            message: String::new(),
            changed: false,
        }
    }

    /// Import options form for an `.apkg`. A `.colpkg` goes to the restore
    /// confirmation instead: importing never replaces the collection silently.
    pub fn import(path: PathBuf) -> Self {
        let colpkg = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("colpkg"));
        if colpkg {
            Self::restore(path)
        } else {
            Self::new(path, Operation::Import, Screen::Form)
        }
    }

    /// Typed confirmation before replacing the collection with a `.colpkg`.
    pub fn restore(path: PathBuf) -> Self {
        Self::new(path, Operation::Restore, Screen::RestoreConfirm)
    }

    /// Starts an `.apkg` export right away; execute the returned intent.
    pub fn export(
        path: PathBuf,
        options: ExportOptions,
        selection: ExportSelection,
    ) -> (Self, Intent) {
        let mut ui = Self::new(path.clone(), Operation::Export, Screen::Running);
        ui.export = Some((options.clone(), selection.clone()));
        let intent = Intent::Start(DataCommand::Package(PackageCommand::Export {
            path,
            options,
            selection,
        }));
        (ui, intent)
    }

    /// Starts a whole-collection `.colpkg` export right away.
    pub fn export_collection(path: PathBuf, media: bool, legacy: bool) -> (Self, Intent) {
        let ui = Self::new(path.clone(), Operation::Export, Screen::Running);
        let intent = Intent::Start(DataCommand::Package(PackageCommand::ExportCollection {
            path,
            media,
            legacy,
        }));
        (ui, intent)
    }

    /// True while an operation runs: the app should poll progress and the
    /// pending result, and route keys here.
    pub fn is_running(&self) -> bool {
        self.screen == Screen::Running
    }

    fn import_command(&self, schema_confirmed: bool) -> Intent {
        Intent::Start(DataCommand::Package(PackageCommand::Import {
            path: self.path.clone(),
            options: (!self.use_presets).then(|| self.options.clone()),
            schema_confirmed,
        }))
    }

    fn run(&mut self, intent: Intent) -> Intent {
        self.screen = Screen::Running;
        self.progress = OperationProgress::default();
        self.cancelling = false;
        intent
    }

    pub fn on_key(&mut self, key: &KeyEvent) -> Intent {
        let ctrl_c =
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
        match self.screen {
            Screen::Form => self.form_key(key.code),
            Screen::Running => {
                let cancel = ctrl_c || matches!(key.code, KeyCode::Esc | KeyCode::Char('c'));
                if cancel && !self.cancelling {
                    self.cancelling = true;
                    Intent::Cancel
                } else {
                    Intent::None
                }
            }
            Screen::SchemaConfirm => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let intent = self.import_command(true);
                    self.run(intent)
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.screen = Screen::Form;
                    Intent::None
                }
                _ => Intent::None,
            },
            Screen::Log => self.log_key(key.code),
            Screen::RestoreConfirm => match key.code {
                KeyCode::Esc => Intent::Close { refresh: false },
                KeyCode::Enter if self.typed == RESTORE_WORD => {
                    let intent = Intent::Start(DataCommand::Package(PackageCommand::Restore {
                        path: self.path.clone(),
                        confirmed: true,
                    }));
                    self.run(intent)
                }
                KeyCode::Backspace => {
                    self.typed.pop();
                    Intent::None
                }
                KeyCode::Char(c) if !ctrl_c && self.typed.len() < 32 => {
                    self.typed.push(c);
                    Intent::None
                }
                _ => Intent::None,
            },
            Screen::Message => match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => Intent::Close {
                    refresh: self.changed,
                },
                _ => Intent::None,
            },
        }
    }

    fn form_key(&mut self, code: KeyCode) -> Intent {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => Intent::Close { refresh: false },
            KeyCode::Up | KeyCode::Char('k') => {
                self.focus = (self.focus + ROWS - 1) % ROWS;
                Intent::None
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.focus = (self.focus + 1) % ROWS;
                Intent::None
            }
            KeyCode::Enter if self.focus == ROWS - 1 => {
                let intent = self.import_command(false);
                self.run(intent)
            }
            KeyCode::Char('i') => {
                let intent = self.import_command(false);
                self.run(intent)
            }
            KeyCode::Char(' ') | KeyCode::Enter | KeyCode::Left | KeyCode::Right => {
                self.toggle();
                Intent::None
            }
            _ => Intent::None,
        }
    }

    fn toggle(&mut self) {
        fn next(c: UpdateCondition) -> UpdateCondition {
            match c {
                UpdateCondition::IfNewer => UpdateCondition::Always,
                UpdateCondition::Always => UpdateCondition::Never,
                UpdateCondition::Never => UpdateCondition::IfNewer,
            }
        }
        let o = &mut self.options;
        match self.focus {
            0 => {
                self.use_presets = !self.use_presets;
                return;
            }
            1 => o.merge_notetypes = !o.merge_notetypes,
            2 => o.update_notes = next(o.update_notes),
            3 => o.update_notetypes = next(o.update_notetypes),
            4 => o.with_scheduling = !o.with_scheduling,
            5 => o.with_deck_configs = !o.with_deck_configs,
            _ => return,
        }
        // Changing an option means using the form's values.
        self.use_presets = false;
    }

    fn log_key(&mut self, code: KeyCode) -> Intent {
        let max = self.log_lines().len().saturating_sub(1);
        match code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                return Intent::Close { refresh: true }
            }
            KeyCode::Down | KeyCode::Char('j') => self.scroll += 1,
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll += 10,
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::Home | KeyCode::Char('g') => self.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => self.scroll = max,
            _ => {}
        }
        self.scroll = self.scroll.min(max);
        Intent::None
    }

    /// Latest `CollectionActor::progress()` while running.
    pub fn on_progress(&mut self, progress: OperationProgress) {
        if self.screen == Screen::Running {
            self.progress = progress;
        }
    }

    /// The finished operation's result.
    pub fn on_response(&mut self, result: Result<DataResponse, CoreError>) -> Intent {
        self.cancelling = false;
        match result {
            Ok(DataResponse::Imported { log, .. }) => {
                self.changed = true;
                self.log = Some(log);
                self.scroll = 0;
                self.screen = Screen::Log;
            }
            Ok(DataResponse::Exported { notes }) => {
                self.finish(format!(
                    "Exported {notes} notes to {}.",
                    self.path.display()
                ));
            }
            Ok(DataResponse::Done) if self.operation == Operation::Export => {
                self.finish(format!(
                    "Exported the whole collection to {}.",
                    self.path.display()
                ));
            }
            Ok(_) if self.operation == Operation::Restore => {
                self.changed = true;
                self.finish(format!(
                    "Restored the collection from {}. The previous collection was backed up first. The next sync will be a one-way full sync.",
                    self.path.display()
                ));
            }
            Ok(_) => self.finish("Done.".into()),
            Err(CoreError::SchemaChangeRequired) if self.operation == Operation::Import => {
                self.screen = Screen::SchemaConfirm;
            }
            Err(CoreError::Interrupted) => {
                self.finish("Cancelled. Nothing was changed.".into());
            }
            Err(error) => self.finish(format!("Failed: {error}")),
        }
        Intent::None
    }

    fn finish(&mut self, message: String) {
        self.message = message;
        self.screen = Screen::Message;
    }

    fn title(&self) -> String {
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match self.operation {
            Operation::Import => format!(" Import {name} "),
            Operation::Export => format!(" Export {name} "),
            Operation::Restore => format!(" Restore {name} "),
        }
    }

    fn form_lines(&self) -> Vec<Line<'static>> {
        fn cond(c: UpdateCondition) -> &'static str {
            match c {
                UpdateCondition::IfNewer => "if newer",
                UpdateCondition::Always => "always",
                UpdateCondition::Never => "never",
            }
        }
        fn check(on: bool) -> &'static str {
            if on {
                "[x]"
            } else {
                "[ ]"
            }
        }
        let o = &self.options;
        let saved = self.use_presets;
        let value = |s: String| if saved { "(saved)".to_owned() } else { s };
        let rows = [
            format!("{} Use the options saved by the last import", check(saved)),
            format!(
                "Merge note types:     {}",
                value(check(o.merge_notetypes).into())
            ),
            format!(
                "Update notes:         {}",
                value(cond(o.update_notes).into())
            ),
            format!(
                "Update note types:    {}",
                value(cond(o.update_notetypes).into())
            ),
            format!(
                "Import scheduling:    {}",
                value(check(o.with_scheduling).into())
            ),
            format!(
                "Import deck presets:  {}",
                value(check(o.with_deck_configs).into())
            ),
            "< Import >".to_owned(),
        ];
        let mut lines: Vec<Line> = rows
            .into_iter()
            .enumerate()
            .map(|(i, text)| {
                let marker = if i == self.focus { "> " } else { "  " };
                let style = if i == self.focus {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                Line::from(vec![Span::raw(marker), Span::styled(text, style)])
            })
            .collect();
        lines.push(Line::raw(""));
        if !saved && o.merge_notetypes {
            lines.push(Line::raw(
                "Merging note types may require a one-way full sync.",
            ));
        }
        lines.push(Line::raw("j/k move  space change  i import  esc close"));
        lines
    }

    fn log_lines(&self) -> Vec<Line<'static>> {
        let Some(log) = &self.log else {
            return vec![];
        };
        let mut lines = vec![
            Line::raw(format!("Notes in package: {}", log.found_notes)),
            Line::raw(format!(
                "New {}   Updated {}   Duplicate {}   Conflicting {}",
                log.new, log.updated, log.duplicate, log.conflicting
            )),
        ];
        for (category, notes) in &log.entries {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                format!("{} ({})", category.replace('_', " "), notes.len()),
                Style::default().add_modifier(Modifier::BOLD),
            ));
            for fields in notes {
                lines.push(Line::raw(format!("  {}", fields.join(" | "))));
            }
        }
        lines
    }
}

/// A centred rectangle of at most `width` x `height` inside `area`.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

impl Widget for &PackageUi {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let height = match self.screen {
            Screen::Log => area.height.saturating_sub(2).max(8),
            Screen::Form => 13,
            _ => 9,
        };
        let rect = centered(area, 64, height);
        Clear.render(rect, buf);
        let block = Block::default().borders(Borders::ALL).title(self.title());
        let inner = block.inner(rect);
        block.render(rect, buf);
        let wrap = Wrap { trim: false };
        match self.screen {
            Screen::Form => Paragraph::new(self.form_lines()).render(inner, buf),
            Screen::Running => {
                let [stage, gauge, _, hint] = Layout::vertical([
                    Constraint::Length(2),
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Length(1),
                ])
                .areas(inner);
                let text = if self.progress.stage.is_empty() {
                    "Working…".to_owned()
                } else {
                    self.progress.stage.clone()
                };
                Paragraph::new(text).wrap(wrap).render(stage, buf);
                if let (Some(current), Some(total)) = (self.progress.current, self.progress.total)
                {
                    let ratio = (current as f64 / total.max(1) as f64).clamp(0.0, 1.0);
                    Gauge::default()
                        .ratio(ratio)
                        .label(format!("{current}/{total}"))
                        .render(gauge, buf);
                }
                let hint_text = if self.cancelling {
                    "Cancelling…"
                } else {
                    "esc cancel"
                };
                Paragraph::new(hint_text).render(hint, buf);
            }
            Screen::SchemaConfirm => Paragraph::new(vec![
                Line::raw("This import changes note types. Your next sync will be a one-way full sync (upload), and other devices must then download."),
                Line::raw(""),
                Line::raw("Nothing has been imported yet. Continue?  y yes  n no"),
            ])
            .wrap(wrap)
            .render(inner, buf),
            Screen::Log => {
                let [body, hint] =
                    Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
                let lines = self.log_lines();
                let scroll = self.scroll.min(lines.len().saturating_sub(1));
                Paragraph::new(lines)
                    .scroll((scroll.min(u16::MAX as usize) as u16, 0))
                    .render(body, buf);
                Paragraph::new("j/k scroll  esc close").render(hint, buf);
            }
            Screen::RestoreConfirm => Paragraph::new(vec![
                Line::raw("This replaces your collection with this file. A backup is taken first. Changes since that backup that are not in the file will be lost, and the next sync will be a one-way full sync."),
                Line::raw(""),
                Line::raw(format!("Type \"{RESTORE_WORD}\" and press enter: {}", self.typed)),
                Line::raw("esc cancel"),
            ])
            .wrap(wrap)
            .render(inner, buf),
            Screen::Message => Paragraph::new(vec![
                Line::raw(self.message.clone()),
                Line::raw(""),
                Line::raw("enter close"),
            ])
            .wrap(wrap)
            .render(inner, buf),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::buffer_text;
    use ratatui::{backend::TestBackend, Terminal};

    fn key(ui: &mut PackageUi, code: KeyCode) -> Intent {
        ui.on_key(&KeyEvent::new(code, KeyModifiers::NONE))
    }
    fn keys(ui: &mut PackageUi, text: &str) {
        for c in text.chars() {
            key(ui, KeyCode::Char(c));
        }
    }
    fn draw(ui: &PackageUi, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| f.render_widget(ui, f.area())).unwrap();
        buffer_text(terminal.backend().buffer())
    }
    fn import_of(intent: &Intent) -> (Option<ImportOptions>, bool) {
        match intent {
            Intent::Start(DataCommand::Package(PackageCommand::Import {
                options,
                schema_confirmed,
                ..
            })) => (options.clone(), *schema_confirmed),
            other => panic!("{other:?}"),
        }
    }
    fn sample_log() -> ImportLog {
        let mut log = ImportLog {
            new: 2,
            updated: 1,
            duplicate: 3,
            conflicting: 0,
            found_notes: 6,
            ..Default::default()
        };
        log.entries.insert(
            "new".into(),
            vec![
                vec!["le chat".into(), "the cat".into()],
                vec!["le chien".into(), "the dog".into()],
            ],
        );
        log.entries.insert(
            "updated".into(),
            vec![vec!["la pomme".into(), "the apple".into()]],
        );
        log
    }

    #[test]
    fn form_uses_presets_until_an_option_changes() {
        let mut ui = PackageUi::import("/tmp/deck.apkg".into());
        let (options, confirmed) = import_of(&key(&mut ui, KeyCode::Char('i')));
        assert!(options.is_none() && !confirmed);
        assert!(ui.is_running());

        let mut ui = PackageUi::import("/tmp/deck.apkg".into());
        key(&mut ui, KeyCode::Down);
        key(&mut ui, KeyCode::Char(' ')); // merge
        key(&mut ui, KeyCode::Down);
        key(&mut ui, KeyCode::Char(' ')); // update notes: always
        key(&mut ui, KeyCode::Char(' ')); // never
        key(&mut ui, KeyCode::Down);
        key(&mut ui, KeyCode::Down);
        key(&mut ui, KeyCode::Enter); // scheduling (Enter toggles off the button)
        for _ in 0..2 {
            key(&mut ui, KeyCode::Down);
        }
        let (options, _) = import_of(&key(&mut ui, KeyCode::Enter));
        let options = options.unwrap();
        assert!(options.merge_notetypes && options.with_scheduling);
        assert!(matches!(options.update_notes, UpdateCondition::Never));
        assert!(!options.with_deck_configs);
    }

    #[test]
    fn schema_change_asks_and_retries_confirmed() {
        let mut ui = PackageUi::import("/tmp/deck.apkg".into());
        key(&mut ui, KeyCode::Char('i'));
        ui.on_response(Err(CoreError::SchemaChangeRequired));
        assert_eq!(
            draw(&ui, 70, 11),
            [
                "",
                "   ┌ Import deck.apkg ────────────────────────────────────────────┐",
                "   │This import changes note types. Your next sync will be a      │",
                "   │one-way full sync (upload), and other devices must then       │",
                "   │download.                                                     │",
                "   │                                                              │",
                "   │Nothing has been imported yet. Continue?  y yes  n no         │",
                "   │                                                              │",
                "   │                                                              │",
                "   └──────────────────────────────────────────────────────────────┘",
                "",
            ]
        );
        let (_, confirmed) = import_of(&key(&mut ui, KeyCode::Char('y')));
        assert!(confirmed && ui.is_running());
        // "No" goes back to the form without starting anything.
        ui.on_response(Err(CoreError::SchemaChangeRequired));
        assert!(matches!(key(&mut ui, KeyCode::Char('n')), Intent::None));
        assert_eq!(ui.screen, Screen::Form);
    }

    #[test]
    fn running_shows_progress_and_cancels_once() {
        let (mut ui, intent) = PackageUi::export(
            "/tmp/out.apkg".into(),
            ExportOptions::default(),
            ExportSelection::All,
        );
        assert!(matches!(
            intent,
            Intent::Start(DataCommand::Package(PackageCommand::Export { .. }))
        ));
        ui.on_progress(OperationProgress {
            stage: "Processed 17 notes...".into(),
            current: Some(17),
            total: Some(34),
        });
        let rows = draw(&ui, 70, 11);
        assert_eq!(
            rows[1],
            "   ┌ Export out.apkg ─────────────────────────────────────────────┐"
        );
        assert_eq!(
            rows[2],
            "   │Processed 17 notes...                                         │"
        );
        assert!(rows[4].contains("17/34"), "{rows:#?}");
        assert!(rows[6].contains("esc cancel"), "{rows:#?}");
        assert!(matches!(key(&mut ui, KeyCode::Esc), Intent::Cancel));
        assert!(matches!(key(&mut ui, KeyCode::Esc), Intent::None));
        assert!(draw(&ui, 70, 11)[6].contains("Cancelling…"));
        ui.on_response(Err(CoreError::Interrupted));
        assert!(draw(&ui, 70, 11)[2].contains("Cancelled. Nothing was changed."));
        assert!(matches!(
            key(&mut ui, KeyCode::Enter),
            Intent::Close { refresh: false }
        ));
    }

    #[test]
    fn import_log_is_scrollable() {
        let mut ui = PackageUi::import("/tmp/deck.apkg".into());
        key(&mut ui, KeyCode::Char('i'));
        ui.on_response(Ok(DataResponse::Imported {
            log: sample_log(),
            changes: Default::default(),
        }));
        assert_eq!(
            draw(&ui, 70, 10),
            [
                "",
                "   ┌ Import deck.apkg ────────────────────────────────────────────┐",
                "   │Notes in package: 6                                           │",
                "   │New 2   Updated 1   Duplicate 3   Conflicting 0               │",
                "   │                                                              │",
                "   │new (2)                                                       │",
                "   │  le chat | the cat                                           │",
                "   │j/k scroll  esc close                                         │",
                "   └──────────────────────────────────────────────────────────────┘",
                "",
            ]
        );
        key(&mut ui, KeyCode::Char('G'));
        let rows = draw(&ui, 70, 10);
        assert_eq!(
            rows[2],
            "   │  la pomme | the apple                                        │"
        );
        key(&mut ui, KeyCode::Up);
        assert!(draw(&ui, 70, 10)[2].contains("updated (1)"));
        key(&mut ui, KeyCode::Char('g'));
        assert!(draw(&ui, 70, 10)[2].contains("Notes in package: 6"));
        assert!(matches!(
            key(&mut ui, KeyCode::Esc),
            Intent::Close { refresh: true }
        ));
    }

    #[test]
    fn restore_needs_the_typed_word_and_colpkg_import_goes_there() {
        let mut ui = PackageUi::import("/tmp/backups/backup-1.colpkg".into());
        assert_eq!(ui.screen, Screen::RestoreConfirm);
        assert!(matches!(key(&mut ui, KeyCode::Enter), Intent::None));
        keys(&mut ui, "restorx");
        assert!(matches!(key(&mut ui, KeyCode::Enter), Intent::None));
        key(&mut ui, KeyCode::Backspace);
        keys(&mut ui, "e");
        let rows = draw(&ui, 70, 11);
        assert_eq!(
            rows,
            [
                "",
                "   ┌ Restore backup-1.colpkg ─────────────────────────────────────┐",
                "   │This replaces your collection with this file. A backup is     │",
                "   │taken first. Changes since that backup that are not in the    │",
                "   │file will be lost, and the next sync will be a one-way full   │",
                "   │sync.                                                         │",
                "   │                                                              │",
                "   │Type \"restore\" and press enter: restore                       │",
                "   │esc cancel                                                    │",
                "   └──────────────────────────────────────────────────────────────┘",
                "",
            ]
        );
        assert!(matches!(
            key(&mut ui, KeyCode::Enter),
            Intent::Start(DataCommand::Package(PackageCommand::Restore {
                confirmed: true,
                ..
            }))
        ));
        ui.on_response(Ok(DataResponse::Done));
        assert!(draw(&ui, 70, 11)[2].contains("Restored the collection"));
        assert!(matches!(
            key(&mut ui, KeyCode::Enter),
            Intent::Close { refresh: true }
        ));
        let mut ui = PackageUi::restore("/tmp/x.colpkg".into());
        assert!(matches!(
            key(&mut ui, KeyCode::Esc),
            Intent::Close { refresh: false }
        ));
    }

    #[test]
    fn form_snapshot() {
        let mut ui = PackageUi::import("/tmp/deck.apkg".into());
        key(&mut ui, KeyCode::Down);
        key(&mut ui, KeyCode::Char(' '));
        assert_eq!(
            draw(&ui, 70, 13),
            [
                "   ┌ Import deck.apkg ────────────────────────────────────────────┐",
                "   │  [ ] Use the options saved by the last import                │",
                "   │> Merge note types:     [x]                                   │",
                "   │  Update notes:         if newer                              │",
                "   │  Update note types:    if newer                              │",
                "   │  Import scheduling:    [ ]                                   │",
                "   │  Import deck presets:  [ ]                                   │",
                "   │  < Import >                                                  │",
                "   │                                                              │",
                "   │Merging note types may require a one-way full sync.           │",
                "   │j/k move  space change  i import  esc close                   │",
                "   │                                                              │",
                "   └──────────────────────────────────────────────────────────────┘",
            ]
        );
        key(&mut ui, KeyCode::Up);
        key(&mut ui, KeyCode::Char(' '));
        assert!(draw(&ui, 70, 13)[2].contains("(saved)"));
        assert!(matches!(
            key(&mut ui, KeyCode::Esc),
            Intent::Close { refresh: false }
        ));
    }
}
