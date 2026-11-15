//! Deck options screen: preset form and FSRS tools (PLAN 8.3).
//!
//! Owned by the Deck options stream (docs/implementation/phase-5-interfaces.md).
//!
//! A self-contained widget (the TUI widget pattern of `browser_ui` /
//! `sync_ui`). The app:
//! 1. builds it with [`DeckOptionsUi::open`] for a normal deck (one
//!    `DeckOptionsCommand::Load` through `Backend::execute`);
//! 2. draws it with [`DeckOptionsUi::render`] and shows [`DeckOptionsUi::hints`];
//! 3. sends keys to [`DeckOptionsUi::on_key`] and calls
//!    [`DeckOptionsUi::tick`] from its event loop while
//!    [`DeckOptionsUi::is_busy`] (saves and FSRS tools run through
//!    `Backend::start`; Esc cancels via `Backend::cancel_data`);
//! 4. carries out the returned [`DeckOptionsIntent`].
//!
//! The form is generic over core's form description (`deck_options::form`):
//! every group and setting of desktop's page with Anki's labels, help,
//! ranges and defaults; tabbed values ("Preset" / "This deck" / "Today
//! only"); warnings under the settings they concern. Editing state, preset
//! actions and the save request live in core's `DeckOptionsState`.
mod simulator;
#[cfg(test)]
mod tests;

use crate::backend::{Backend, CommandOp};
use crate::input::TextInput;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Widget, Wrap},
};
use simulator::SimulatorView;
use std::cell::Cell;
use tanki_core::deck_options::form::{FieldId, FieldKind, Scope};
use tanki_core::deck_options::model::DeckOptionsState;
use tanki_core::deck_options::{
    DeckOptionsCommand, DeckOptionsData, DeckOptionsResponse, SaveMode,
};
use tanki_core::{Command, CoreError, ManageCommand, ManageResponse, OperationProgress, Response};
use unicode_width::UnicodeWidthStr;

/// What only the app can do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeckOptionsIntent {
    None,
    /// Leave the screen; nothing was saved.
    Close,
    /// Saved; leave the screen and show this message. The actor's
    /// `OpChanges` broadcast refreshes decks and queues.
    Saved(String),
    /// Show a transient message; the screen stays.
    Message(String),
    /// Edit the custom scheduling script in `$EDITOR`, then hand the
    /// result to [`DeckOptionsUi::set_custom_scheduling`].
    EditScript(String),
}

/// Buttons in the form (desktop's FSRS section).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    OptimizeCurrent,
    Evaluate,
    OptimizeAll,
    Simulator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Field(FieldId),
    Action(Action),
}

#[derive(Debug, Clone, PartialEq)]
enum Row {
    Header(String),
    Field(FieldId),
    Warning(String),
    Action(Action),
    Blank,
}

impl Row {
    fn focus(&self) -> Option<Focus> {
        match self {
            Row::Field(id) => Some(Focus::Field(*id)),
            Row::Action(a) => Some(Focus::Action(*a)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NameKind {
    Add,
    Clone,
    Rename,
}

enum Dialog {
    Edit {
        id: FieldId,
        input: TextInput,
        error: Option<String>,
    },
    Presets {
        cursor: usize,
    },
    PresetName {
        kind: NameKind,
        input: TextInput,
    },
    ConfirmRemove,
    /// Unsaved changes on leaving.
    ConfirmClose,
    ConfirmOptimizeAll,
    /// The save needs a one-way sync (a preset was removed).
    ConfirmFullSync {
        mode: SaveMode,
    },
    Message(String),
    Simulator(Box<SimulatorView>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Job {
    Save(SaveMode),
    Optimize,
    Evaluate,
    MinimumRetention,
    Simulate,
}

struct Running {
    op: Box<dyn CommandOp>,
    job: Job,
    cancelling: bool,
    progress: OperationProgress,
}

pub struct DeckOptionsUi {
    state: DeckOptionsState,
    focus: Focus,
    scroll: Cell<usize>,
    dialog: Option<Dialog>,
    running: Option<Running>,
    message: Option<String>,
    /// The user accepted the one-way sync a preset removal causes.
    full_sync_confirmed: bool,
}

type Result<T> = tanki_core::Result<T>;

fn deck_options(command: DeckOptionsCommand) -> Command {
    Command::Manage(ManageCommand::DeckOptions(command))
}

fn unexpected(response: &Response) -> CoreError {
    let name = format!("{response:?}");
    let name = name.split(['(', ' ', '{']).next().unwrap_or("?").to_owned();
    CoreError::Other(format!("Unexpected response from the collection: {name}"))
}

fn ctrl(key: &KeyEvent, c: char) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char(c)
}

const LABEL_MAX: usize = 48;
const HELP_LINES: u16 = 4;

impl DeckOptionsUi {
    /// Loads the page for a normal deck.
    pub fn open(backend: &dyn Backend, deck_id: i64) -> Result<Self> {
        match backend.execute(deck_options(DeckOptionsCommand::Load { deck_id }))? {
            Response::Manage(ManageResponse::DeckOptions(DeckOptionsResponse::Loaded(data))) => {
                Ok(Self::from_data(*data))
            }
            other => Err(unexpected(&other)),
        }
    }

    pub fn from_data(data: DeckOptionsData) -> Self {
        let state = DeckOptionsState::new(data);
        let focus = Focus::Field(FieldId::NewPerDay);
        Self {
            state,
            focus,
            scroll: Cell::new(0),
            dialog: None,
            running: None,
            message: None,
            full_sync_confirmed: false,
        }
    }

    pub fn state(&self) -> &DeckOptionsState {
        &self.state
    }

    /// Unsaved changes.
    pub fn is_dirty(&self) -> bool {
        self.state.is_dirty()
    }

    /// A save or FSRS tool is with the actor: call [`Self::tick`].
    pub fn is_busy(&self) -> bool {
        self.running.is_some()
    }

    /// Moves the cursor to a setting (when it is shown).
    pub fn focus(&mut self, id: FieldId) {
        if self.rows(80).contains(&Row::Field(id)) {
            self.focus = Focus::Field(id);
        }
    }

    /// The script from `$EDITOR` ([`DeckOptionsIntent::EditScript`]).
    pub fn set_custom_scheduling(&mut self, script: String) {
        self.state.set_custom_scheduling(script);
    }

    fn strings(&self) -> &tanki_core::deck_options::form::Strings {
        &self.state.form().strings
    }

    // ----- rows -----

    fn rows(&self, width: usize) -> Vec<Row> {
        let mut rows = vec![];
        let fsrs = self.state.fsrs();
        for group in &self.state.form().groups {
            let fields: Vec<_> = group
                .fields
                .iter()
                .filter(|f| self.state.visible(f))
                .collect();
            if fields.is_empty() {
                continue;
            }
            if !rows.is_empty() {
                rows.push(Row::Blank);
            }
            rows.push(Row::Header(group.title.clone()));
            for field in fields {
                rows.push(Row::Field(field.id));
                for warning in self.state.warnings(field.id) {
                    for line in wrap(&warning, width.saturating_sub(6).max(20)) {
                        rows.push(Row::Warning(line));
                    }
                }
            }
            if group.fields.iter().any(|f| f.id == FieldId::Fsrs) && fsrs {
                rows.push(Row::Action(Action::OptimizeCurrent));
                rows.push(Row::Action(Action::Evaluate));
                rows.push(Row::Action(Action::OptimizeAll));
                rows.push(Row::Action(Action::Simulator));
            }
        }
        rows
    }

    fn selectable(rows: &[Row]) -> Vec<Focus> {
        rows.iter().filter_map(Row::focus).collect()
    }

    fn move_focus(&mut self, delta: isize) {
        let rows = self.rows(80);
        let focusable = Self::selectable(&rows);
        if focusable.is_empty() {
            return;
        }
        let index = focusable.iter().position(|f| *f == self.focus).unwrap_or(0) as isize;
        let next = (index + delta).clamp(0, focusable.len() as isize - 1) as usize;
        self.focus = focusable[next];
    }

    /// After FSRS is toggled the focused row may be hidden.
    fn fix_focus(&mut self) {
        let rows = self.rows(80);
        let focusable = Self::selectable(&rows);
        if !focusable.contains(&self.focus) {
            self.focus = focusable
                .first()
                .copied()
                .unwrap_or(Focus::Field(FieldId::NewPerDay));
        }
    }

    // ----- keys -----

    pub fn on_key(&mut self, key: KeyEvent, backend: &dyn Backend) -> DeckOptionsIntent {
        if let Some(running) = &mut self.running {
            let cancel = ctrl(&key, 'c') || matches!(key.code, KeyCode::Esc | KeyCode::Char('c'));
            if cancel && !running.cancelling {
                running.cancelling = true;
                backend.cancel_data();
            }
            return DeckOptionsIntent::None;
        }
        if self.dialog.is_some() {
            return self.dialog_key(key, backend);
        }
        self.message = None;
        let focus_field = match self.focus {
            Focus::Field(id) => Some(id),
            Focus::Action(_) => None,
        };
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.move_focus(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_focus(1),
            KeyCode::PageUp => self.move_focus(-10),
            KeyCode::PageDown => self.move_focus(10),
            KeyCode::Home | KeyCode::Char('g') => self.move_focus(-10_000),
            KeyCode::End | KeyCode::Char('G') => self.move_focus(10_000),
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('-') => {
                if let Some(id) = focus_field {
                    self.state.adjust(id, -1);
                    self.fix_focus();
                }
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('+') | KeyCode::Char(' ') => {
                if let Some(id) = focus_field {
                    self.state.adjust(id, 1);
                    self.fix_focus();
                }
            }
            KeyCode::Tab | KeyCode::Char('t') => {
                if let Some(id) = focus_field {
                    self.state.next_scope(id);
                }
            }
            KeyCode::Char('r') => {
                if let Some(id) = focus_field {
                    self.state.reset(id);
                    self.fix_focus();
                }
            }
            KeyCode::Enter => match self.focus {
                Focus::Field(id) => return self.activate(id),
                Focus::Action(action) => return self.action(action, backend),
            },
            KeyCode::Char('p') => {
                self.dialog = Some(Dialog::Presets {
                    cursor: self.state.selected_index(),
                })
            }
            KeyCode::Char('[') => self.switch_preset(-1),
            KeyCode::Char(']') => self.switch_preset(1),
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return self.save(SaveMode::Normal, backend)
            }
            KeyCode::Char('s') => return self.save(SaveMode::Normal, backend),
            KeyCode::Char('S') => return self.save(SaveMode::ApplyToChildren, backend),
            KeyCode::Char('o') => return self.action(Action::OptimizeCurrent, backend),
            KeyCode::Char('e') => return self.action(Action::Evaluate, backend),
            KeyCode::Char('O') => return self.action(Action::OptimizeAll, backend),
            KeyCode::Char('v') => return self.action(Action::Simulator, backend),
            KeyCode::Esc | KeyCode::Char('q') => {
                if self.state.is_dirty() {
                    self.dialog = Some(Dialog::ConfirmClose);
                } else {
                    return DeckOptionsIntent::Close;
                }
            }
            _ => {}
        }
        DeckOptionsIntent::None
    }

    fn switch_preset(&mut self, delta: isize) {
        let n = self.state.presets().len() as isize;
        if n == 0 {
            return;
        }
        let index = (self.state.selected_index() as isize + delta).rem_euclid(n);
        self.state.select(index as usize);
    }

    fn activate(&mut self, id: FieldId) -> DeckOptionsIntent {
        let Some(kind) = self.state.spec(id).map(|s| s.kind.clone()) else {
            return DeckOptionsIntent::None;
        };
        match kind {
            FieldKind::Bool | FieldKind::Choice { .. } | FieldKind::EasyDay => {
                self.state.adjust(id, 1);
                self.fix_focus();
            }
            FieldKind::Script => {
                return DeckOptionsIntent::EditScript(
                    self.state.globals().card_state_customizer.clone(),
                )
            }
            _ => {
                self.dialog = Some(Dialog::Edit {
                    id,
                    input: TextInput::new(self.state.edit_text(id)),
                    error: None,
                })
            }
        }
        DeckOptionsIntent::None
    }

    fn fsrs_guard(&mut self) -> bool {
        if !self.state.fsrs() {
            self.message = Some(self.strings().fsrs_must_be_enabled.clone());
            return false;
        }
        true
    }

    fn action(&mut self, action: Action, backend: &dyn Backend) -> DeckOptionsIntent {
        if !self.fsrs_guard() {
            return DeckOptionsIntent::None;
        }
        let preset_name = self.state.current().name.clone();
        let config = Box::new(self.state.current().config.clone());
        match action {
            Action::OptimizeCurrent | Action::Evaluate => {
                if self.state.assignments_changed() {
                    self.message = Some(self.strings().please_save_first.clone());
                    return DeckOptionsIntent::None;
                }
                let (job, command) = if action == Action::OptimizeCurrent {
                    (
                        Job::Optimize,
                        DeckOptionsCommand::Optimize {
                            preset_name,
                            config,
                            health_check: self.state.globals().fsrs_health_check,
                        },
                    )
                } else {
                    (
                        Job::Evaluate,
                        DeckOptionsCommand::Evaluate {
                            preset_name,
                            config,
                        },
                    )
                };
                self.start(job, command, backend);
            }
            Action::OptimizeAll => self.dialog = Some(Dialog::ConfirmOptimizeAll),
            Action::Simulator => {
                if !self.state.params_saved() {
                    self.message = Some(self.strings().simulate_save_preset.clone());
                    return DeckOptionsIntent::None;
                }
                let leech_suspend = self.state.current().config.leech_action == 0;
                self.dialog = Some(Dialog::Simulator(Box::new(SimulatorView::new(
                    leech_suspend,
                ))));
            }
        }
        DeckOptionsIntent::None
    }

    fn start(&mut self, job: Job, command: DeckOptionsCommand, backend: &dyn Backend) {
        match backend.start(deck_options(command)) {
            Ok(op) => {
                self.running = Some(Running {
                    op,
                    job,
                    cancelling: false,
                    progress: OperationProgress::default(),
                })
            }
            Err(error) => self.message = Some(error.to_string()),
        }
    }

    fn save(&mut self, mode: SaveMode, backend: &dyn Backend) -> DeckOptionsIntent {
        if mode == SaveMode::OptimizeAll && !self.fsrs_guard() {
            return DeckOptionsIntent::None;
        }
        let command = DeckOptionsCommand::Save {
            request: Box::new(self.state.save_request(mode)),
            confirmed: self.full_sync_confirmed,
        };
        self.start(Job::Save(mode), command, backend);
        DeckOptionsIntent::None
    }

    fn dialog_key(&mut self, key: KeyEvent, backend: &dyn Backend) -> DeckOptionsIntent {
        let Some(dialog) = self.dialog.take() else {
            return DeckOptionsIntent::None;
        };
        let yes = matches!(key.code, KeyCode::Char('y') | KeyCode::Enter);
        let no = matches!(key.code, KeyCode::Char('n') | KeyCode::Esc);
        match dialog {
            Dialog::Edit {
                id,
                mut input,
                error,
            } => match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => match self.state.set_text(id, input.text()) {
                    Ok(()) => self.fix_focus(),
                    Err(error) => {
                        self.dialog = Some(Dialog::Edit {
                            id,
                            input,
                            error: Some(error),
                        })
                    }
                },
                _ => {
                    input.handle_key(&key);
                    self.dialog = Some(Dialog::Edit { id, input, error });
                }
            },
            Dialog::Presets { mut cursor } => {
                let n = self.state.presets().len();
                let s = self.strings().clone();
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => cursor = cursor.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => cursor = (cursor + 1).min(n - 1),
                    KeyCode::Enter => {
                        self.state.select(cursor);
                        return DeckOptionsIntent::None;
                    }
                    KeyCode::Char('a') => {
                        self.dialog = Some(Dialog::PresetName {
                            kind: NameKind::Add,
                            input: TextInput::default(),
                        });
                        return DeckOptionsIntent::None;
                    }
                    KeyCode::Char('c') => {
                        self.state.select(cursor);
                        self.dialog = Some(Dialog::PresetName {
                            kind: NameKind::Clone,
                            input: TextInput::new(self.state.current().name.clone()),
                        });
                        return DeckOptionsIntent::None;
                    }
                    KeyCode::Char('r') => {
                        self.state.select(cursor);
                        self.dialog = Some(Dialog::PresetName {
                            kind: NameKind::Rename,
                            input: TextInput::new(self.state.current().name.clone()),
                        });
                        return DeckOptionsIntent::None;
                    }
                    KeyCode::Char('d') => {
                        self.state.select(cursor);
                        if self.state.can_remove() {
                            self.dialog = Some(Dialog::ConfirmRemove);
                        } else {
                            self.message = Some(s.default_cant_be_removed);
                            self.dialog = Some(Dialog::Presets { cursor });
                        }
                        return DeckOptionsIntent::None;
                    }
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('p') => {
                        return DeckOptionsIntent::None
                    }
                    _ => {}
                }
                self.dialog = Some(Dialog::Presets { cursor });
            }
            Dialog::PresetName { kind, mut input } => match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    let name = input.text().trim().to_owned();
                    if !name.is_empty() {
                        match kind {
                            NameKind::Add => self.state.add_preset(&name),
                            NameKind::Clone => self.state.clone_preset(&name),
                            NameKind::Rename => self.state.rename_preset(&name),
                        }
                    }
                }
                _ => {
                    input.handle_key(&key);
                    self.dialog = Some(Dialog::PresetName { kind, input });
                }
            },
            Dialog::ConfirmRemove => {
                if yes {
                    let needs_sync = self.state.removal_needs_full_sync();
                    match self.state.remove_preset() {
                        Ok(()) => self.full_sync_confirmed |= needs_sync,
                        Err(error) => self.message = Some(error),
                    }
                } else if !no {
                    self.dialog = Some(Dialog::ConfirmRemove);
                }
            }
            Dialog::ConfirmClose => match key.code {
                KeyCode::Char('s') | KeyCode::Char('y') | KeyCode::Enter => {
                    return self.save(SaveMode::Normal, backend)
                }
                KeyCode::Char('d') | KeyCode::Char('n') => return DeckOptionsIntent::Close,
                KeyCode::Esc | KeyCode::Char('c') => {}
                _ => self.dialog = Some(Dialog::ConfirmClose),
            },
            Dialog::ConfirmOptimizeAll => {
                if yes {
                    return self.save(SaveMode::OptimizeAll, backend);
                } else if !no {
                    self.dialog = Some(Dialog::ConfirmOptimizeAll);
                }
            }
            Dialog::ConfirmFullSync { mode } => {
                if yes {
                    self.full_sync_confirmed = true;
                    return self.save(mode, backend);
                } else if !no {
                    self.dialog = Some(Dialog::ConfirmFullSync { mode });
                }
            }
            Dialog::Message(text) => {
                if !(yes || no || key.code == KeyCode::Char('q')) {
                    self.dialog = Some(Dialog::Message(text));
                }
            }
            Dialog::Simulator(mut view) => {
                match view.on_key(key) {
                    simulator::Outcome::Close => return DeckOptionsIntent::None,
                    simulator::Outcome::Simulate => {
                        let request = view.request(&self.state);
                        self.start(
                            Job::Simulate,
                            DeckOptionsCommand::Simulate(Box::new(request)),
                            backend,
                        );
                    }
                    simulator::Outcome::MinimumRetention => {
                        let request = view.request(&self.state);
                        self.start(
                            Job::MinimumRetention,
                            DeckOptionsCommand::MinimumRetention(Box::new(request)),
                            backend,
                        );
                    }
                    simulator::Outcome::None => {}
                }
                self.dialog = Some(Dialog::Simulator(view));
            }
        }
        DeckOptionsIntent::None
    }

    // ----- long operations -----

    /// Polls the running command; call from the event loop while busy.
    pub fn tick(&mut self, backend: &dyn Backend) -> DeckOptionsIntent {
        let Some(running) = &mut self.running else {
            return DeckOptionsIntent::None;
        };
        running.progress = backend.data_progress();
        let result = match running.op.poll() {
            Ok(None) => return DeckOptionsIntent::None,
            Ok(Some(response)) => Ok(response),
            Err(error) => Err(error),
        };
        let job = running.job;
        self.running = None;
        self.finish(job, result)
    }

    fn finish(&mut self, job: Job, result: Result<Response>) -> DeckOptionsIntent {
        let response = match result {
            Ok(Response::Manage(ManageResponse::DeckOptions(r))) => Ok(r),
            Ok(other) => Err(unexpected(&other)),
            Err(error) => Err(error),
        };
        match (job, response) {
            (_, Err(CoreError::Interrupted)) => {
                self.message = Some("Cancelled.".into());
            }
            (Job::Save(mode), Err(CoreError::SchemaChangeRequired)) => {
                self.dialog = Some(Dialog::ConfirmFullSync { mode });
            }
            (_, Err(error)) => self.dialog = Some(Dialog::Message(error.to_string())),
            (Job::Save(mode), Ok(DeckOptionsResponse::Saved(_))) => {
                return DeckOptionsIntent::Saved(match mode {
                    SaveMode::OptimizeAll => "Deck options saved; all presets optimized.".into(),
                    _ => "Deck options saved.".into(),
                });
            }
            (Job::Optimize, Ok(DeckOptionsResponse::Optimized(o))) => {
                self.state.apply_optimized(&o);
                let mut text = o.message.clone();
                if !o.already_optimal {
                    let head = format!(
                        "{}: {} ({} reviews). Save to keep them.",
                        self.state
                            .spec(FieldId::FsrsParams)
                            .map_or("FSRS parameters", |s| s.label.as_str()),
                        tanki_core::deck_options::model::params_to_string(&o.params),
                        o.fsrs_items
                    );
                    text = if text.is_empty() {
                        head
                    } else {
                        format!("{head}\n\n{text}")
                    };
                }
                self.dialog = Some(Dialog::Message(text));
            }
            (Job::Evaluate, Ok(DeckOptionsResponse::Evaluated { message, .. })) => {
                self.dialog = Some(Dialog::Message(message));
            }
            (Job::Simulate, Ok(DeckOptionsResponse::Simulated(sim))) => {
                if let Some(Dialog::Simulator(view)) = &mut self.dialog {
                    view.add_run(sim);
                }
            }
            (Job::MinimumRetention, Ok(DeckOptionsResponse::MinimumRetention { message, .. })) => {
                if let Some(Dialog::Simulator(view)) = &mut self.dialog {
                    view.retention = Some(message);
                } else {
                    self.message = Some(message);
                }
            }
            (_, Ok(other)) => {
                self.message = Some(format!("Unexpected answer: {other:?}"));
            }
        }
        DeckOptionsIntent::None
    }

    // ----- drawing -----

    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.running.is_some() {
            return vec![("Esc", "cancel")];
        }
        match &self.dialog {
            Some(Dialog::Edit { .. }) | Some(Dialog::PresetName { .. }) => {
                vec![("Enter", "apply"), ("Esc", "cancel")]
            }
            Some(Dialog::Presets { .. }) => vec![
                ("Enter", "use"),
                ("a", "add"),
                ("c", "clone"),
                ("r", "rename"),
                ("d", "remove"),
                ("Esc", "back"),
            ],
            Some(Dialog::ConfirmClose) => {
                vec![("s", "save"), ("d", "discard"), ("Esc", "keep editing")]
            }
            Some(Dialog::ConfirmRemove)
            | Some(Dialog::ConfirmOptimizeAll)
            | Some(Dialog::ConfirmFullSync { .. }) => vec![("y", "yes"), ("n", "no")],
            Some(Dialog::Message(_)) => vec![("Enter", "close")],
            Some(Dialog::Simulator(view)) => view.hints(),
            None => {
                let mut hints = vec![
                    ("↑/↓", "move"),
                    ("←/→", "change"),
                    ("Enter", "edit"),
                    ("t", "tab"),
                    ("r", "default"),
                    ("p", "presets"),
                    ("s", "save"),
                    ("S", "save to subdecks"),
                ];
                if self.state.fsrs() {
                    hints.extend([("o", "optimize"), ("v", "simulator")]);
                }
                hints.push(("Esc", "close"));
                hints
            }
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height < 6 || area.width < 20 {
            return;
        }
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let warn = Style::default().fg(Color::Yellow);
        let width = area.width as usize;
        let mut y = area.y;

        let dirty = if self.state.is_dirty() {
            "  [modified]"
        } else {
            ""
        };
        let title = format!(
            "{} — {}{dirty}",
            self.strings().title,
            self.state.data().deck_name
        );
        buf.set_stringn(area.x, y, &title, width, bold);
        y += 1;
        let preset = Line::from(vec![
            Span::raw(format!("{}: ", self.strings().preset)),
            Span::styled(self.state.preset_label(self.state.selected_index()), bold),
            Span::styled("   p presets  [ ] switch", dim),
        ]);
        buf.set_line(area.x, y, &preset, area.width);
        y += 1;
        if let Some(warning) = &self.state.data().custom_scheduling_warning {
            for line in wrap(&format!("⚠ {warning}"), width).into_iter().take(2) {
                buf.set_stringn(area.x, y, &line, width, warn);
                y += 1;
            }
        }

        let footer = HELP_LINES + 2;
        let bottom = area.y + area.height;
        let body_height = bottom.saturating_sub(y + footer) as usize;
        let rows = self.rows(width);
        let cursor_row = rows
            .iter()
            .position(|r| r.focus() == Some(self.focus))
            .unwrap_or(0);
        let mut scroll = self.scroll.get();
        if cursor_row < scroll {
            scroll = cursor_row.saturating_sub(1);
        } else if body_height > 0 && cursor_row >= scroll + body_height {
            scroll = cursor_row + 1 - body_height;
        }
        scroll = scroll.min(rows.len().saturating_sub(body_height.max(1)));
        self.scroll.set(scroll);
        let label_width = LABEL_MAX.min(width * 11 / 20);
        for (i, row) in rows.iter().enumerate().skip(scroll).take(body_height) {
            let line_y = y + (i - scroll) as u16;
            let selected = row.focus() == Some(self.focus);
            self.render_row(row, selected, label_width, area.x, line_y, area.width, buf);
        }
        if body_height > 0 && rows.len() > scroll + body_height {
            buf.set_string(
                area.x + area.width - 1,
                y + body_height as u16 - 1,
                "↓",
                dim,
            );
        }
        if scroll > 0 {
            buf.set_string(area.x + area.width - 1, y, "↑", dim);
        }

        // Help for the focused setting.
        let help_y = bottom - footer;
        let help = match self.focus {
            Focus::Field(id) => self
                .state
                .spec(id)
                .map(|s| s.help.clone())
                .unwrap_or_default(),
            Focus::Action(action) => self.action_help(action),
        };
        let help_area = Rect::new(area.x, help_y, area.width, HELP_LINES);
        buf.set_stringn(area.x, help_y, "─".repeat(width), width, dim);
        Paragraph::new(strip_markup(&help))
            .style(dim)
            .wrap(Wrap { trim: true })
            .render(
                Rect::new(help_area.x, help_area.y + 1, help_area.width, HELP_LINES),
                buf,
            );

        // Status line: progress or message.
        let status_y = bottom - 1;
        let status = if let Some(running) = &self.running {
            progress_text(running)
        } else {
            self.message.clone().unwrap_or_default()
        };
        buf.set_stringn(area.x, status_y, &status, width, bold);

        if let Some(dialog) = &self.dialog {
            self.render_dialog(dialog, area, buf);
        }
    }

    fn action_help(&self, action: Action) -> String {
        let s = self.strings();
        match action {
            Action::OptimizeCurrent => self
                .state
                .spec(FieldId::FsrsParams)
                .map(|f| f.help.clone())
                .unwrap_or_default(),
            Action::Evaluate => format!("{} — Log loss / RMSE(bins)", s.evaluate),
            Action::OptimizeAll => s.optimize_all_confirm.clone(),
            Action::Simulator => format!("{} — {}", s.simulator, s.compute_minimum_retention),
        }
    }

    fn action_label(&self, action: Action) -> (String, &'static str) {
        let s = self.strings();
        match action {
            Action::OptimizeCurrent => (s.optimize_current.clone(), "o"),
            Action::Evaluate => (s.evaluate.clone(), "e"),
            Action::OptimizeAll => (s.optimize_all.clone(), "O"),
            Action::Simulator => (s.simulator.clone(), "v"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_row(
        &self,
        row: &Row,
        selected: bool,
        label_width: usize,
        x: u16,
        y: u16,
        width: u16,
        buf: &mut Buffer,
    ) {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let marker = if selected { "› " } else { "  " };
        let highlight = if selected {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        match row {
            Row::Blank => {}
            Row::Header(title) => {
                buf.set_stringn(
                    x,
                    y,
                    title,
                    width as usize,
                    bold.add_modifier(Modifier::UNDERLINED),
                );
            }
            Row::Warning(text) => {
                buf.set_stringn(
                    x + 4,
                    y,
                    format!("⚠ {text}"),
                    width.saturating_sub(4) as usize,
                    Style::default().fg(Color::Yellow),
                );
            }
            Row::Action(action) => {
                let (label, key) = self.action_label(*action);
                let line = Line::from(vec![
                    Span::raw(marker),
                    Span::styled(format!("▶ {label}"), highlight),
                    Span::styled(format!("  {key}"), dim),
                ]);
                buf.set_line(x, y, &line, width);
            }
            Row::Field(id) => {
                let Some(spec) = self.state.spec(*id) else {
                    return;
                };
                let mut label = spec.label.clone();
                if spec.global {
                    label.push_str(" (global)");
                }
                let label = fit(&label, label_width.saturating_sub(2));
                let pad = label_width.saturating_sub(2 + label.width());
                let value = match (&spec.kind, self.state.value(*id)) {
                    (FieldKind::Bool, tanki_core::deck_options::model::FieldValue::Bool(on)) => {
                        if on { "[x]" } else { "[ ]" }.to_owned()
                    }
                    (FieldKind::Choice { .. } | FieldKind::EasyDay, _) => {
                        format!("‹ {} ›", self.state.display(*id))
                    }
                    _ => self.state.display(*id),
                };
                let mut spans = vec![
                    Span::raw(marker),
                    Span::styled(label, highlight),
                    Span::raw(" ".repeat(pad + 1)),
                    Span::styled(value, bold),
                ];
                if !spec.scopes.is_empty() {
                    let active = self.state.scope(*id);
                    spans.push(Span::raw("  "));
                    for scope in &spec.scopes {
                        let name = match scope {
                            Scope::Preset => &self.strings().preset,
                            Scope::Deck => &self.strings().this_deck,
                            Scope::Today => &self.strings().today_only,
                        };
                        if *scope == active {
                            spans.push(Span::styled(format!("[{name}]"), bold));
                        } else {
                            spans.push(Span::styled(format!(" {name} "), dim));
                        }
                    }
                }
                if !self.state.is_default(*id) {
                    spans.push(Span::styled("  ↺", dim));
                }
                buf.set_line(x, y, &Line::from(spans), width);
            }
        }
    }

    fn render_dialog(&self, dialog: &Dialog, area: Rect, buf: &mut Buffer) {
        let s = self.strings();
        let dim = Style::default().add_modifier(Modifier::DIM);
        let (title, lines): (String, Vec<Line>) = match dialog {
            Dialog::Simulator(view) => {
                let rect = centered(
                    area,
                    area.width.saturating_sub(4).min(100),
                    area.height.saturating_sub(2).min(30),
                );
                view.render(
                    &self.state,
                    self.running.as_ref().map(progress_text),
                    rect,
                    buf,
                );
                return;
            }
            Dialog::Edit { id, input, error } => {
                let label = self
                    .state
                    .spec(*id)
                    .map(|s| s.label.clone())
                    .unwrap_or_default();
                let mut lines = vec![Line::from(vec![
                    Span::raw("> "),
                    Span::raw(input.text().to_owned()),
                    Span::styled("█", dim),
                ])];
                if let Some(error) = error {
                    lines.push(Line::styled(error.clone(), Style::default().fg(Color::Red)));
                }
                if matches!(
                    self.state.spec(*id).map(|s| &s.kind),
                    Some(FieldKind::Steps)
                ) {
                    lines.push(Line::styled("1m 10m 1h 2d (s/m/h/d)", dim));
                }
                (label, lines)
            }
            Dialog::Presets { cursor } => {
                let lines = (0..self.state.presets().len())
                    .map(|i| {
                        let mark = if i == self.state.selected_index() {
                            "●"
                        } else {
                            " "
                        };
                        let text = format!("{mark} {}", self.state.preset_label(i));
                        if i == *cursor {
                            Line::styled(
                                format!("› {text}"),
                                Style::default().add_modifier(Modifier::REVERSED),
                            )
                        } else {
                            Line::raw(format!("  {text}"))
                        }
                    })
                    .collect();
                (s.preset.clone(), lines)
            }
            Dialog::PresetName { kind, input } => {
                let title = match kind {
                    NameKind::Add => &s.add_preset,
                    NameKind::Clone => &s.clone_preset,
                    NameKind::Rename => &s.rename_preset,
                };
                (
                    title.clone(),
                    vec![
                        Line::raw(s.name_prompt.clone()),
                        Line::from(vec![
                            Span::raw("> "),
                            Span::raw(input.text().to_owned()),
                            Span::styled("█", dim),
                        ]),
                    ],
                )
            }
            Dialog::ConfirmRemove => {
                let mut lines = vec![Line::raw(s.confirm_remove(&self.state.current().name))];
                if self.state.removal_needs_full_sync() {
                    lines.push(Line::raw(""));
                    lines.push(Line::raw(s.will_require_full_sync.clone()));
                }
                lines.push(Line::raw(""));
                lines.push(Line::styled("y remove   n keep", dim));
                (s.remove_preset.clone(), lines)
            }
            Dialog::ConfirmClose => (
                s.title.clone(),
                vec![
                    Line::raw("Save your changes to the deck options?"),
                    Line::raw(""),
                    Line::styled("s save   d discard   Esc keep editing", dim),
                ],
            ),
            Dialog::ConfirmOptimizeAll => (
                s.optimize_all.clone(),
                vec![
                    Line::raw(s.optimize_all_confirm.clone()),
                    Line::raw(""),
                    Line::styled("y yes   n no", dim),
                ],
            ),
            Dialog::ConfirmFullSync { .. } => (
                s.remove_preset.clone(),
                vec![
                    Line::raw(s.will_require_full_sync.clone()),
                    Line::raw(""),
                    Line::styled("y save anyway   n back", dim),
                ],
            ),
            Dialog::Message(text) => (
                s.title.clone(),
                text.lines().map(|l| Line::raw(l.to_owned())).collect(),
            ),
        };
        let inner_width = area.width.saturating_sub(8).min(76);
        let wrapped: usize = lines
            .iter()
            .map(|l| l.width().max(1).div_ceil(inner_width.max(1) as usize))
            .sum();
        let height = (wrapped as u16 + 2).min(area.height);
        let rect = centered(area, inner_width + 2, height);
        Clear.render(rect, buf);
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(format!(" {title} ")))
            .render(rect, buf);
    }
}

fn progress_text(running: &Running) -> String {
    let what = match running.job {
        Job::Save(SaveMode::OptimizeAll) => "Optimizing all presets",
        Job::Save(_) => "Saving",
        Job::Optimize => "Optimizing",
        Job::Evaluate => "Evaluating",
        Job::MinimumRetention => "Computing minimum recommended retention",
        Job::Simulate => "Simulating",
    };
    let p = &running.progress;
    let mut text = if p.stage.is_empty() || p.stage == what {
        format!("{what}…")
    } else {
        format!("{what}: {}", p.stage)
    };
    match (p.current, p.total) {
        (Some(current), Some(total)) if total > 0 => {
            text.push_str(&format!(" {current}/{total} ({}%)", current * 100 / total))
        }
        (Some(current), None) => text.push_str(&format!(" {current}")),
        _ => {}
    }
    if running.cancelling {
        text.push_str(" — cancelling");
    } else {
        text.push_str("  (Esc cancels)");
    }
    text
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

/// Truncates to a display width with an ellipsis.
fn fit(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    for c in text.chars() {
        if out.width() + unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) + 1 > width {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

/// Greedy word wrap by display width.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = vec![];
    for paragraph in text.lines() {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if !line.is_empty() && line.width() + 1 + word.width() > width {
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
    }
    lines
}

/// Help texts carry a little Markdown/HTML from Fluent: drop the markup,
/// and join the hard-wrapped lines of each paragraph.
fn strip_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            '`' | '*' if !in_tag => {}
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split("\n\n")
        .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
