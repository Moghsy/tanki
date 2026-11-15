//! Note types screen: list, fields editor, templates editor with live preview, change note type mapping (PLAN 8.6).
//!
//! Owned by the Note types stream (docs/implementation/phase-5-interfaces.md).
//!
//! # Wiring
//! - Build with [`NotetypesUi::new`] (from the decks screen or the command
//!   palette). The change-note-type mapping form is a separate widget,
//!   [`ChangeNotetypeUi::new`], built from the notes the browser selected.
//! - Feed key presses to `on_key` and bracketed paste to `on_paste`; act on
//!   the returned intent; draw with `render` and show `hints()` in the footer.
//! - [`NotetypesIntent::ExternalEdit`] asks the app to run `$EDITOR` on the
//!   text and call [`NotetypesUi::apply_external`] with the result.
//! - After other code mutated the collection call `on_changes` with the
//!   broadcast `OpChanges` (skip the widget's own, via
//!   `backend::Tracked::take_own`).
//!
//! Every operation that forces a one-way full sync asks first: the core
//! answers `SchemaChangeRequired` and the widget shows the prompt, then
//! repeats the command confirmed. Required fields (cloze text, image
//! occlusion) are protected by the core's drafts.
mod change;
mod render;
#[cfg(test)]
mod tests;

use crate::backend::Backend;
use crate::input::TextInput;
use crate::textarea::TextArea;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{buffer::Buffer, layout::Rect};
use tanki_core::notetypes::{
    NotetypeCommand, NotetypeDetail, NotetypeEdit, NotetypeResponse, NotetypeSummary, RestorePlan,
    StockKind, StockKindInfo, TemplateDetail, TemplatePreview, TemplateProblem,
};
use tanki_core::{
    BrowseCommand, BrowseMode, Command, CoreError, EditResponse, ManageCommand, ManageResponse,
    NoteCommand, NotetypeKind, OpChanges, Response, SearchOrder,
};

pub use change::{ChangeIntent, ChangeNotetypeUi, CHANGE_KEYS};

/// What only the app can do, returned by the widget's key handlers.
#[derive(Debug, Clone, PartialEq)]
pub enum NotetypesIntent {
    None,
    /// Leave the screen.
    Close,
    /// Note types changed: refresh the decks screen, add form and browser.
    Changed(OpChanges),
    /// Show this in the app's message line.
    Message(String),
    /// Run `$EDITOR` on `text` (a temporary file ending in `suffix`) and
    /// call [`NotetypesUi::apply_external`] with what comes back.
    ExternalEdit {
        text: String,
        suffix: &'static str,
    },
}

pub const LIST_KEYS: &[(&str, &str)] = &[
    ("↑↓ / j k", "choose a note type"),
    ("Enter / c", "card types (templates editor)"),
    ("f", "fields"),
    ("a", "add from a stock type or clone"),
    ("r", "rename"),
    ("d", "delete (and its notes)"),
    ("o", "LaTeX header, footer and SVG"),
    ("R", "restore to the stock type (typed confirmation)"),
    ("Esc / q", "close"),
];

pub const FIELD_KEYS: &[(&str, &str)] = &[
    ("↑↓ / j k", "choose a field"),
    ("K / J or Alt+↑/↓", "move the field up / down"),
    ("a / n / d", "add / rename / delete"),
    ("s", "use as the sort field"),
    (
        "1 2 3 4",
        "right-to-left / HTML editor by default / collapse by default / exclude from search",
    ),
    ("e / f", "description / editor font and size"),
    ("Ctrl+S", "save"),
    ("Esc", "back (asks when unsaved)"),
];

pub const TEMPLATE_KEYS: &[(&str, &str)] = &[
    (
        "Tab / Shift+Tab",
        "next / previous part: front, back, styling, browser front, browser back",
    ),
    ("Ctrl+S", "save (Anki validates the templates)"),
    ("Alt+[ / Alt+]", "previous / next card type"),
    ("Alt+A / Alt+R / Alt+D", "add / rename / delete a card type"),
    ("Alt+, / Alt+.", "move the card type earlier / later"),
    ("Alt+O", "deck override"),
    ("Alt+N / Alt+F", "preview another note / fill empty fields"),
    ("Alt+X", "edit this part in $EDITOR"),
    ("Esc", "back (asks when unsaved)"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    Front,
    Back,
    Styling,
    BrowserFront,
    BrowserBack,
}

impl Tab {
    pub(crate) const ALL: [Tab; 5] = [
        Tab::Front,
        Tab::Back,
        Tab::Styling,
        Tab::BrowserFront,
        Tab::BrowserBack,
    ];
    pub(crate) fn title(self) -> &'static str {
        match self {
            Tab::Front => "Front",
            Tab::Back => "Back",
            Tab::Styling => "Styling",
            Tab::BrowserFront => "Browser front",
            Tab::BrowserBack => "Browser back",
        }
    }
    fn suffix(self) -> &'static str {
        if self == Tab::Styling {
            ".css"
        } else {
            ".html"
        }
    }
}

/// An action that may need the full-sync confirmation, kept while the prompt
/// is open.
#[derive(Debug, Clone)]
pub(crate) enum Action {
    Delete(i64),
    Save,
    Restore {
        id: i64,
        force_kind: Option<StockKind>,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum PromptKind {
    /// Name for a stock type (`Some(kind)`) or a clone of the note type.
    AddStock(StockKind),
    AddClone(i64),
    Rename(i64),
    FieldAdd,
    FieldRename(usize),
    FieldDescription(usize),
    FieldFont(usize),
    TemplateAdd,
    TemplateRename(usize),
    DeckOverride(usize),
}

pub(crate) struct Prompt {
    pub(crate) title: String,
    pub(crate) kind: PromptKind,
    pub(crate) input: TextInput,
}

pub(crate) enum AddChoice {
    Stock(StockKind),
    Clone(i64),
}

pub(crate) struct Picker {
    pub(crate) title: String,
    pub(crate) items: Vec<(AddChoice, String)>,
    pub(crate) query: TextInput,
    pub(crate) selected: usize,
}

impl Picker {
    pub(crate) fn matches(&self) -> Vec<usize> {
        crate::fuzzy::rank(
            self.query.text(),
            self.items.iter().map(|(_, name)| name.as_str()),
        )
    }
}

pub(crate) struct RestoreForm {
    pub(crate) plan: RestorePlan,
    pub(crate) id: i64,
    pub(crate) force_kind: Option<StockKind>,
    /// Choosing the stock kind first (the note type has no recorded origin).
    pub(crate) choosing: Option<usize>,
    pub(crate) typed: TextInput,
}

pub(crate) enum Overlay {
    None,
    Prompt(Prompt),
    Picker(Picker),
    ConfirmDelete(i64),
    ConfirmDiscard,
    ConfirmDeleteField(usize),
    ConfirmDeleteTemplate(usize),
    /// "This requires a full sync": repeat the action confirmed on `y`.
    FullSync(Action),
    Restore(RestoreForm),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Status {
    pub(crate) text: String,
    pub(crate) error: bool,
}

pub(crate) struct FieldsScreen {
    pub(crate) draft: NotetypeDetail,
    pub(crate) original: NotetypeDetail,
    pub(crate) cursor: usize,
}

pub(crate) struct TemplatesScreen {
    pub(crate) draft: NotetypeDetail,
    pub(crate) original: NotetypeDetail,
    pub(crate) index: usize,
    pub(crate) tab: Tab,
    pub(crate) area: TextArea,
    /// Notes of the type to preview with, and which one.
    pub(crate) samples: Vec<i64>,
    pub(crate) sample: usize,
    pub(crate) fill_empty: bool,
    pub(crate) preview: Option<Result<TemplatePreview, String>>,
    /// The last rejection by Anki, shown until the next edit.
    pub(crate) problem: Option<TemplateProblem>,
}

pub(crate) struct LatexScreen {
    pub(crate) draft: NotetypeDetail,
    pub(crate) original: NotetypeDetail,
    pub(crate) pre: TextArea,
    pub(crate) post: TextArea,
    /// 0 header, 1 footer, 2 SVG toggle.
    pub(crate) focus: usize,
}

pub(crate) enum Screen {
    List,
    Fields(Box<FieldsScreen>),
    Templates(Box<TemplatesScreen>),
    Latex(Box<LatexScreen>),
}

pub struct NotetypesUi {
    pub(crate) notetypes: Vec<NotetypeSummary>,
    pub(crate) stock: Vec<StockKindInfo>,
    pub(crate) selected: usize,
    pub(crate) screen: Screen,
    pub(crate) overlay: Overlay,
    pub(crate) status: Option<Status>,
    pub(crate) media_dir: std::path::PathBuf,
    pub(crate) night_mode: bool,
}

// ---- backend helpers ----------------------------------------------------------

pub(crate) fn run(
    b: &dyn Backend,
    command: NotetypeCommand,
) -> Result<NotetypeResponse, CoreError> {
    match b.execute(Command::Manage(ManageCommand::Notetypes(command)))? {
        Response::Manage(ManageResponse::Notetypes(r)) => Ok(r),
        other => Err(CoreError::Other(format!(
            "Unexpected response from the collection: {other:?}"
        ))),
    }
}

pub(crate) fn unexpected<T>(r: NotetypeResponse) -> Result<T, CoreError> {
    Err(CoreError::Other(format!(
        "Unexpected response from the collection: {r:?}"
    )))
}

fn fetch_list(b: &dyn Backend) -> Result<Vec<NotetypeSummary>, CoreError> {
    match run(b, NotetypeCommand::List)? {
        NotetypeResponse::List(l) => Ok(l),
        other => unexpected(other),
    }
}

fn fetch_detail(b: &dyn Backend, id: i64) -> Result<NotetypeDetail, CoreError> {
    match run(b, NotetypeCommand::Get { id })? {
        NotetypeResponse::Detail(d) => Ok(d),
        other => unexpected(other),
    }
}

fn is_plain(key: &KeyEvent) -> bool {
    !key.modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

impl NotetypesUi {
    pub fn new(b: &dyn Backend, media_dir: std::path::PathBuf) -> Result<Self, String> {
        let notetypes = fetch_list(b).map_err(|e| e.to_string())?;
        let stock = match run(b, NotetypeCommand::StockKinds).map_err(|e| e.to_string())? {
            NotetypeResponse::StockKinds(s) => s,
            other => return unexpected(other).map_err(|e: CoreError| e.to_string()),
        };
        Ok(Self {
            notetypes,
            stock,
            selected: 0,
            screen: Screen::List,
            overlay: Overlay::None,
            status: None,
            media_dir,
            night_mode: true,
        })
    }

    /// Night mode for the preview renderer (from `Display`).
    pub fn set_night_mode(&mut self, night: bool) {
        self.night_mode = night;
    }

    // ---- read access ---------------------------------------------------------

    pub fn notetypes(&self) -> &[NotetypeSummary] {
        &self.notetypes
    }

    pub fn selected(&self) -> Option<&NotetypeSummary> {
        self.notetypes.get(self.selected)
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_ref().map(|s| s.text.as_str())
    }

    /// True while an editor screen has unsaved edits.
    pub fn is_dirty(&self) -> bool {
        match &self.screen {
            Screen::List => false,
            Screen::Fields(s) => s.draft != s.original,
            Screen::Templates(s) => s.dirty(),
            Screen::Latex(s) => s.dirty(),
        }
    }

    /// True on the list (the app may then close the screen at once).
    pub fn on_list(&self) -> bool {
        matches!(self.screen, Screen::List)
    }

    pub fn hints(&self) -> String {
        match &self.overlay {
            Overlay::None => match &self.screen {
                Screen::List => "Enter cards · f fields · a add · r rename · d delete · o LaTeX · R restore · Esc close".into(),
                Screen::Fields(_) => "a add · n rename · d delete · K/J move · s sort field · 1-4 options · e description · Ctrl+S save · Esc back".into(),
                Screen::Templates(_) => "Tab part · Ctrl+S save · Alt+[ ] card type · Alt+A add · Alt+R rename · Alt+D delete · Alt+O deck · Alt+N note · Alt+X $EDITOR · Esc back".into(),
                Screen::Latex(_) => "Tab next · Space toggles SVG · Ctrl+S save · Esc back".into(),
            },
            Overlay::Prompt(_) => "type the value · Enter accept · Esc cancel".into(),
            Overlay::Picker(_) => "type to filter · ↑↓ choose · Enter select · Esc cancel".into(),
            Overlay::ConfirmDelete(_)
            | Overlay::ConfirmDeleteField(_)
            | Overlay::ConfirmDeleteTemplate(_) => "y delete · n keep".into(),
            Overlay::ConfirmDiscard => "y discard the changes · n keep editing".into(),
            Overlay::FullSync(_) => "y continue (full upload on the next sync) · n cancel".into(),
            Overlay::Restore(form) => {
                if form.choosing.is_some() {
                    "↑↓ choose the stock kind · Enter select · Esc cancel".into()
                } else {
                    "type the note type's name · Enter restore · Esc cancel".into()
                }
            }
        }
    }

    pub fn render(
        &self,
        area: Rect,
        buf: &mut Buffer,
        renderer: &dyn crate::services::CardRenderer,
    ) {
        render::draw(self, area, buf, renderer);
    }

    pub(crate) fn set_status(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            error: false,
        });
    }

    pub(crate) fn set_error(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            error: true,
        });
    }

    // ---- reloading -----------------------------------------------------------

    fn reload_list(&mut self, b: &dyn Backend, keep: Option<i64>) {
        let keep = keep.or_else(|| self.selected().map(|s| s.id));
        match fetch_list(b) {
            Ok(list) => {
                self.notetypes = list;
                self.selected = keep
                    .and_then(|id| self.notetypes.iter().position(|n| n.id == id))
                    .unwrap_or(0)
                    .min(self.notetypes.len().saturating_sub(1));
            }
            Err(e) => self.set_error(e.to_string()),
        }
    }

    /// Other code changed note types: refresh the list (an open editor keeps
    /// its draft; saving would still be validated against the stored type).
    pub fn on_changes(&mut self, b: &dyn Backend, changes: &OpChanges) {
        if changes.notetype {
            self.reload_list(b, None);
        }
    }

    // ---- keys ----------------------------------------------------------------

    pub fn on_paste(&mut self, b: &dyn Backend, text: &str) -> NotetypesIntent {
        match &mut self.overlay {
            Overlay::Prompt(p) => p.input.insert_str(text),
            Overlay::Picker(p) => {
                p.query.insert_str(text);
                p.selected = 0;
            }
            Overlay::Restore(f) if f.choosing.is_none() => f.typed.insert_str(text),
            Overlay::None => match &mut self.screen {
                Screen::Templates(s) => {
                    s.area.paste(text);
                    s.edited(b);
                }
                Screen::Latex(s) => {
                    s.paste(text);
                }
                _ => {}
            },
            _ => {}
        }
        NotetypesIntent::None
    }

    pub fn on_key(&mut self, b: &dyn Backend, key: KeyEvent) -> NotetypesIntent {
        if key.kind == KeyEventKind::Release {
            return NotetypesIntent::None;
        }
        let overlay = std::mem::replace(&mut self.overlay, Overlay::None);
        if !matches!(overlay, Overlay::None) {
            return self.overlay_key(b, overlay, key);
        }
        match &self.screen {
            Screen::List => self.list_key(b, key),
            Screen::Fields(_) => self.fields_key(b, key),
            Screen::Templates(_) => self.templates_key(b, key),
            Screen::Latex(_) => self.latex_key(b, key),
        }
    }

    // ---- list ----------------------------------------------------------------

    fn list_key(&mut self, b: &dyn Backend, key: KeyEvent) -> NotetypesIntent {
        let count = self.notetypes.len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') if is_plain(&key) => {
                self.selected = self.selected.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') if is_plain(&key) => {
                self.selected = (self.selected + 1).min(count.saturating_sub(1))
            }
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::PageDown => self.selected = (self.selected + 10).min(count.saturating_sub(1)),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = count.saturating_sub(1),
            KeyCode::Esc | KeyCode::Char('q') if is_plain(&key) => return NotetypesIntent::Close,
            KeyCode::Enter | KeyCode::Char('c') if is_plain(&key) => self.open_templates(b),
            KeyCode::Char('f') if is_plain(&key) => self.open_fields(b),
            KeyCode::Char('o') if is_plain(&key) => self.open_latex(b),
            KeyCode::Char('a') if is_plain(&key) => self.open_add(),
            KeyCode::Char('r') if is_plain(&key) => {
                if let Some(nt) = self.selected().cloned() {
                    self.overlay = Overlay::Prompt(Prompt {
                        title: "Rename note type".into(),
                        kind: PromptKind::Rename(nt.id),
                        input: TextInput::new(nt.name),
                    });
                }
            }
            KeyCode::Char('d') if is_plain(&key) => {
                if let Some(nt) = self.selected() {
                    if self.notetypes.len() < 2 {
                        self.set_error("Please add another note type first");
                    } else {
                        self.overlay = Overlay::ConfirmDelete(nt.id);
                    }
                }
            }
            KeyCode::Char('R') if is_plain(&key) || key.modifiers == KeyModifiers::SHIFT => {
                self.open_restore(b, None)
            }
            _ => {}
        }
        NotetypesIntent::None
    }

    fn open_fields(&mut self, b: &dyn Backend) {
        let Some(id) = self.selected().map(|s| s.id) else {
            return;
        };
        match fetch_detail(b, id) {
            Ok(d) => {
                self.screen = Screen::Fields(Box::new(FieldsScreen {
                    original: d.clone(),
                    draft: d,
                    cursor: 0,
                }));
                self.status = None;
            }
            Err(e) => self.set_error(e.to_string()),
        }
    }

    fn open_latex(&mut self, b: &dyn Backend) {
        let Some(id) = self.selected().map(|s| s.id) else {
            return;
        };
        match fetch_detail(b, id) {
            Ok(d) => {
                self.screen = Screen::Latex(Box::new(LatexScreen {
                    pre: TextArea::new(d.latex_pre.clone()),
                    post: TextArea::new(d.latex_post.clone()),
                    original: d.clone(),
                    draft: d,
                    focus: 0,
                }));
                self.status = None;
            }
            Err(e) => self.set_error(e.to_string()),
        }
    }

    fn open_templates(&mut self, b: &dyn Backend) {
        let Some(nt) = self.selected().cloned() else {
            return;
        };
        match fetch_detail(b, nt.id) {
            Ok(d) => {
                let samples = match b.execute(Command::Browse(BrowseCommand::Search {
                    query: format!("\"note:{}\"", nt.name.replace('"', "")),
                    mode: BrowseMode::Notes,
                    order: SearchOrder::Default,
                })) {
                    Ok(Response::Edit(EditResponse::Ids(ids))) => {
                        ids.into_iter().take(50).collect()
                    }
                    _ => Vec::new(),
                };
                let mut screen = TemplatesScreen {
                    area: TextArea::new(""),
                    original: d.clone(),
                    draft: d,
                    index: 0,
                    tab: Tab::Front,
                    samples,
                    sample: 0,
                    fill_empty: true,
                    preview: None,
                    problem: None,
                };
                screen.load_area();
                screen.refresh_preview(b);
                self.screen = Screen::Templates(Box::new(screen));
                self.status = None;
            }
            Err(e) => self.set_error(e.to_string()),
        }
    }

    fn open_add(&mut self) {
        let mut items: Vec<(AddChoice, String)> = self
            .stock
            .iter()
            .map(|s| (AddChoice::Stock(s.kind), format!("Add: {}", s.name)))
            .collect();
        items.extend(
            self.notetypes
                .iter()
                .map(|n| (AddChoice::Clone(n.id), format!("Clone: {}", n.name))),
        );
        self.overlay = Overlay::Picker(Picker {
            title: "Add note type".into(),
            items,
            query: TextInput::default(),
            selected: 0,
        });
    }

    fn open_restore(&mut self, b: &dyn Backend, force_kind: Option<StockKind>) {
        let Some(id) = self.selected().map(|s| s.id) else {
            return;
        };
        match run(b, NotetypeCommand::RestorePlan { id, force_kind }) {
            Ok(NotetypeResponse::RestorePlan(plan)) => {
                let choosing = plan.needs_kind.then_some(0);
                self.overlay = Overlay::Restore(RestoreForm {
                    plan,
                    id,
                    force_kind,
                    choosing,
                    typed: TextInput::default(),
                });
            }
            Ok(other) => {
                let _ = unexpected::<()>(other);
            }
            Err(e) => self.set_error(e.to_string()),
        }
    }

    // ---- overlays --------------------------------------------------------------

    fn overlay_key(&mut self, b: &dyn Backend, overlay: Overlay, key: KeyEvent) -> NotetypesIntent {
        let yes = matches!(key.code, KeyCode::Char('y' | 'Y')) && is_plain(&key);
        let no = matches!(key.code, KeyCode::Char('n' | 'N') | KeyCode::Esc);
        match overlay {
            Overlay::None => NotetypesIntent::None,
            Overlay::ConfirmDelete(id) => {
                if yes {
                    return self.perform(b, Action::Delete(id), false);
                }
                if !no {
                    self.overlay = Overlay::ConfirmDelete(id);
                }
                NotetypesIntent::None
            }
            Overlay::ConfirmDiscard => {
                if yes {
                    self.screen = Screen::List;
                    self.status = None;
                } else if !no {
                    self.overlay = Overlay::ConfirmDiscard;
                }
                NotetypesIntent::None
            }
            Overlay::ConfirmDeleteField(index) => {
                if yes {
                    if let Screen::Fields(s) = &mut self.screen {
                        match s.draft.apply(NotetypeEdit::DeleteField { index }) {
                            Ok(()) => s.cursor = s.cursor.min(s.draft.fields.len() - 1),
                            Err(e) => {
                                self.status = Some(Status {
                                    text: e.to_string(),
                                    error: true,
                                })
                            }
                        }
                    }
                } else if !no {
                    self.overlay = Overlay::ConfirmDeleteField(index);
                }
                NotetypesIntent::None
            }
            Overlay::ConfirmDeleteTemplate(index) => {
                if yes {
                    if let Screen::Templates(s) = &mut self.screen {
                        s.store_area();
                        match s.draft.apply(NotetypeEdit::DeleteTemplate { index }) {
                            Ok(()) => {
                                s.index = s.index.min(s.draft.templates.len() - 1);
                                s.load_area();
                                s.problem = None;
                                s.refresh_preview(b);
                            }
                            Err(e) => {
                                self.status = Some(Status {
                                    text: e.to_string(),
                                    error: true,
                                })
                            }
                        }
                    }
                } else if !no {
                    self.overlay = Overlay::ConfirmDeleteTemplate(index);
                }
                NotetypesIntent::None
            }
            Overlay::FullSync(action) => {
                if yes {
                    return self.perform(b, action, true);
                }
                if !no {
                    self.overlay = Overlay::FullSync(action);
                }
                NotetypesIntent::None
            }
            Overlay::Prompt(prompt) => self.prompt_key(b, prompt, key),
            Overlay::Picker(picker) => self.picker_key(picker, key),
            Overlay::Restore(form) => self.restore_key(b, form, key),
        }
    }

    fn picker_key(&mut self, mut p: Picker, key: KeyEvent) -> NotetypesIntent {
        let matches = p.matches();
        match key.code {
            KeyCode::Esc => return NotetypesIntent::None,
            KeyCode::Up => p.selected = p.selected.saturating_sub(1),
            KeyCode::Down => p.selected = (p.selected + 1).min(matches.len().saturating_sub(1)),
            KeyCode::Enter => {
                if let Some(&i) = matches.get(p.selected.min(matches.len().saturating_sub(1))) {
                    let (choice, label) = p.items.swap_remove(i);
                    let (kind, name) = match choice {
                        AddChoice::Stock(k) => (
                            PromptKind::AddStock(k),
                            label.trim_start_matches("Add: ").to_owned(),
                        ),
                        AddChoice::Clone(id) => (
                            PromptKind::AddClone(id),
                            format!("{} copy", label.trim_start_matches("Clone: ")),
                        ),
                    };
                    self.overlay = Overlay::Prompt(Prompt {
                        title: "Name of the new note type".into(),
                        kind,
                        input: TextInput::new(name),
                    });
                    return NotetypesIntent::None;
                }
            }
            _ => {
                if p.query.handle_key(&key) {
                    p.selected = 0;
                }
            }
        }
        self.overlay = Overlay::Picker(p);
        NotetypesIntent::None
    }

    fn prompt_key(&mut self, b: &dyn Backend, mut p: Prompt, key: KeyEvent) -> NotetypesIntent {
        match key.code {
            KeyCode::Esc => return NotetypesIntent::None,
            KeyCode::Enter => return self.prompt_done(b, p.kind, p.input.text().trim().to_owned()),
            _ => {
                p.input.handle_key(&key);
            }
        }
        self.overlay = Overlay::Prompt(p);
        NotetypesIntent::None
    }

    fn prompt_done(&mut self, b: &dyn Backend, kind: PromptKind, text: String) -> NotetypesIntent {
        match kind {
            PromptKind::AddStock(kind) => {
                let command = NotetypeCommand::AddStock {
                    kind,
                    name: Some(text),
                };
                self.added(b, command)
            }
            PromptKind::AddClone(id) => {
                let command = NotetypeCommand::Clone {
                    id,
                    name: Some(text),
                };
                self.added(b, command)
            }
            PromptKind::Rename(id) => match run(b, NotetypeCommand::Rename { id, name: text }) {
                Ok(NotetypeResponse::Saved { notetype, changes }) => {
                    self.reload_list(b, Some(id));
                    self.set_status(format!("Renamed to {}", notetype.name));
                    NotetypesIntent::Changed(changes)
                }
                Ok(other) => {
                    let _ = unexpected::<()>(other);
                    NotetypesIntent::None
                }
                Err(e) => {
                    self.set_error(e.to_string());
                    NotetypesIntent::None
                }
            },
            PromptKind::FieldAdd => self.edit_fields(NotetypeEdit::AddField { name: text }, true),
            PromptKind::FieldRename(index) => {
                self.edit_fields(NotetypeEdit::RenameField { index, name: text }, false)
            }
            PromptKind::FieldDescription(index) => {
                if let Screen::Fields(s) = &mut self.screen {
                    if let Some(mut field) = s.draft.fields.get(index).cloned() {
                        field.description = text;
                        return self
                            .edit_fields(NotetypeEdit::SetFieldOptions { index, field }, false);
                    }
                }
                NotetypesIntent::None
            }
            PromptKind::FieldFont(index) => {
                if let Screen::Fields(s) = &mut self.screen {
                    if let Some(mut field) = s.draft.fields.get(index).cloned() {
                        let (name, size) = match text.rsplit_once(' ') {
                            Some((n, s)) if s.parse::<u32>().is_ok() => {
                                (n.trim().to_owned(), s.parse().unwrap_or(field.font_size))
                            }
                            _ => (text.clone(), field.font_size),
                        };
                        field.font_name = name;
                        field.font_size = size;
                        return self
                            .edit_fields(NotetypeEdit::SetFieldOptions { index, field }, false);
                    }
                }
                NotetypesIntent::None
            }
            PromptKind::TemplateAdd => {
                self.edit_templates(b, NotetypeEdit::AddTemplate { name: text }, true)
            }
            PromptKind::TemplateRename(index) => {
                self.edit_templates(b, NotetypeEdit::RenameTemplate { index, name: text }, false)
            }
            PromptKind::DeckOverride(index) => self.set_deck_override(b, index, text),
        }
    }

    fn added(&mut self, b: &dyn Backend, command: NotetypeCommand) -> NotetypesIntent {
        match run(b, command) {
            Ok(NotetypeResponse::Added { id, changes }) => {
                self.reload_list(b, Some(id));
                self.set_status("Note type added");
                NotetypesIntent::Changed(changes)
            }
            Ok(other) => {
                let _ = unexpected::<()>(other);
                NotetypesIntent::None
            }
            Err(e) => {
                self.set_error(e.to_string());
                NotetypesIntent::None
            }
        }
    }

    fn restore_key(
        &mut self,
        b: &dyn Backend,
        mut form: RestoreForm,
        key: KeyEvent,
    ) -> NotetypesIntent {
        if key.code == KeyCode::Esc {
            return NotetypesIntent::None;
        }
        if let Some(choice) = form.choosing {
            let kinds = &self.stock;
            match key.code {
                KeyCode::Up => form.choosing = Some(choice.saturating_sub(1)),
                KeyCode::Down => {
                    form.choosing = Some((choice + 1).min(kinds.len().saturating_sub(1)))
                }
                KeyCode::Enter => {
                    if let Some(kind) = kinds.get(choice).map(|k| k.kind) {
                        // Re-plan with the chosen kind.
                        match run(
                            b,
                            NotetypeCommand::RestorePlan {
                                id: form.id,
                                force_kind: Some(kind),
                            },
                        ) {
                            Ok(NotetypeResponse::RestorePlan(plan)) => {
                                form.plan = plan;
                                form.force_kind = Some(kind);
                                form.choosing = None;
                            }
                            Ok(_) => {}
                            Err(e) => {
                                self.set_error(e.to_string());
                                return NotetypesIntent::None;
                            }
                        }
                    }
                }
                _ => {}
            }
            self.overlay = Overlay::Restore(form);
            return NotetypesIntent::None;
        }
        match key.code {
            KeyCode::Enter => {
                if form.typed.text().trim() == form.plan.notetype_name {
                    return self.perform(
                        b,
                        Action::Restore {
                            id: form.id,
                            force_kind: form.force_kind,
                        },
                        false,
                    );
                }
                self.set_error("Type the note type's name exactly to confirm");
            }
            _ => {
                form.typed.handle_key(&key);
            }
        }
        self.overlay = Overlay::Restore(form);
        NotetypesIntent::None
    }

    // ---- actions with the full-sync guard ------------------------------------------

    fn perform(&mut self, b: &dyn Backend, action: Action, confirmed: bool) -> NotetypesIntent {
        let result = match &action {
            Action::Delete(id) => run(b, NotetypeCommand::Delete { id: *id, confirmed }),
            Action::Save => match self.draft_to_save() {
                Some(notetype) => run(
                    b,
                    NotetypeCommand::Save {
                        notetype,
                        confirmed,
                    },
                ),
                None => return NotetypesIntent::None,
            },
            Action::Restore { id, force_kind } => run(
                b,
                NotetypeCommand::Restore {
                    id: *id,
                    force_kind: *force_kind,
                    typed_confirmation: true,
                    confirmed,
                },
            ),
        };
        match result {
            Err(CoreError::SchemaChangeRequired) => {
                self.overlay = Overlay::FullSync(action);
                NotetypesIntent::None
            }
            Err(e) => {
                self.set_error(e.to_string());
                NotetypesIntent::None
            }
            Ok(NotetypeResponse::Changed(changes)) => {
                self.reload_list(b, None);
                self.set_status("Note type deleted");
                NotetypesIntent::Changed(changes)
            }
            Ok(NotetypeResponse::Saved { notetype, changes }) => {
                self.saved(b, notetype);
                NotetypesIntent::Changed(changes)
            }
            Ok(NotetypeResponse::Rejected(problem)) => {
                self.rejected(b, problem);
                NotetypesIntent::None
            }
            Ok(other) => {
                let _ = unexpected::<()>(other);
                NotetypesIntent::None
            }
        }
    }

    /// The draft of the open editor, with a templates part still being typed
    /// stored in it.
    fn draft_to_save(&mut self) -> Option<NotetypeDetail> {
        match &mut self.screen {
            Screen::Fields(s) => Some(s.draft.clone()),
            Screen::Templates(s) => {
                s.store_area();
                Some(s.draft.clone())
            }
            Screen::Latex(s) => {
                s.store();
                Some(s.draft.clone())
            }
            Screen::List => None,
        }
    }

    fn saved(&mut self, b: &dyn Backend, notetype: NotetypeDetail) {
        let id = notetype.id;
        match &mut self.screen {
            Screen::Fields(s) => {
                s.cursor = s.cursor.min(notetype.fields.len().saturating_sub(1));
                s.original = notetype.clone();
                s.draft = notetype;
            }
            Screen::Templates(s) => {
                s.index = s.index.min(notetype.templates.len().saturating_sub(1));
                s.original = notetype.clone();
                s.draft = notetype;
                s.problem = None;
                s.load_area();
                s.refresh_preview(b);
            }
            Screen::Latex(s) => {
                s.original = notetype.clone();
                s.draft = notetype;
            }
            Screen::List => {
                self.reload_list(b, Some(id));
                self.set_status("Restored to the stock note type");
                return;
            }
        }
        self.reload_list(b, Some(id));
        self.set_status("Saved");
    }

    fn rejected(&mut self, b: &dyn Backend, problem: TemplateProblem) {
        self.set_error(problem.message.clone());
        if let Screen::Templates(s) = &mut self.screen {
            s.store_area();
            let index = (problem.template as usize).min(s.draft.templates.len().saturating_sub(1));
            if index != s.index {
                s.index = index;
                s.load_area();
            }
            s.problem = Some(problem);
            s.refresh_preview(b);
        }
    }

    fn save(&mut self, b: &dyn Backend) -> NotetypesIntent {
        if !self.is_dirty() {
            self.set_status("Nothing to save");
            return NotetypesIntent::None;
        }
        self.perform(b, Action::Save, false)
    }

    /// Esc in an editor: back to the list, asking when there is unsaved work.
    fn back(&mut self) {
        if self.is_dirty() {
            self.overlay = Overlay::ConfirmDiscard;
        } else {
            self.screen = Screen::List;
            self.status = None;
        }
    }

    // ---- fields editor ---------------------------------------------------------------

    fn edit_fields(&mut self, edit: NotetypeEdit, jump_to_end: bool) -> NotetypesIntent {
        if let Screen::Fields(s) = &mut self.screen {
            match s.draft.apply(edit) {
                Ok(()) => {
                    if jump_to_end {
                        s.cursor = s.draft.fields.len() - 1;
                    }
                    self.status = None;
                }
                Err(e) => self.set_error(e.to_string()),
            }
        }
        NotetypesIntent::None
    }

    fn fields_key(&mut self, b: &dyn Backend, key: KeyEvent) -> NotetypesIntent {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let Screen::Fields(s) = &mut self.screen else {
            return NotetypesIntent::None;
        };
        let count = s.draft.fields.len();
        let at = s.cursor;
        let field = s.draft.fields.get(at).cloned();
        match key.code {
            KeyCode::Char('s') if ctrl => return self.save(b),
            KeyCode::Esc => self.back(),
            KeyCode::Up if alt => {
                if at > 0 {
                    s.cursor -= 1;
                    return self.edit_fields(
                        NotetypeEdit::MoveField {
                            from: at,
                            to: at - 1,
                        },
                        false,
                    );
                }
            }
            KeyCode::Down if alt => {
                if at + 1 < count {
                    s.cursor += 1;
                    return self.edit_fields(
                        NotetypeEdit::MoveField {
                            from: at,
                            to: at + 1,
                        },
                        false,
                    );
                }
            }
            KeyCode::Up | KeyCode::Char('k') => s.cursor = at.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => s.cursor = (at + 1).min(count - 1),
            KeyCode::Char('K') if at > 0 => {
                s.cursor -= 1;
                return self.edit_fields(
                    NotetypeEdit::MoveField {
                        from: at,
                        to: at - 1,
                    },
                    false,
                );
            }
            KeyCode::Char('J') if at + 1 < count => {
                s.cursor += 1;
                return self.edit_fields(
                    NotetypeEdit::MoveField {
                        from: at,
                        to: at + 1,
                    },
                    false,
                );
            }
            KeyCode::Char('a') if is_plain(&key) => {
                self.overlay = Overlay::Prompt(Prompt {
                    title: "New field name".into(),
                    kind: PromptKind::FieldAdd,
                    input: TextInput::default(),
                });
            }
            KeyCode::Char('n') if is_plain(&key) => {
                if let Some(f) = field {
                    self.overlay = Overlay::Prompt(Prompt {
                        title: "Rename field".into(),
                        kind: PromptKind::FieldRename(at),
                        input: TextInput::new(f.name),
                    });
                }
            }
            KeyCode::Char('e') if is_plain(&key) => {
                if let Some(f) = field {
                    self.overlay = Overlay::Prompt(Prompt {
                        title: "Description (shown while the field is empty)".into(),
                        kind: PromptKind::FieldDescription(at),
                        input: TextInput::new(f.description),
                    });
                }
            }
            KeyCode::Char('f') if is_plain(&key) => {
                if let Some(f) = field {
                    self.overlay = Overlay::Prompt(Prompt {
                        title: "Editor font and size, e.g. Arial 20".into(),
                        kind: PromptKind::FieldFont(at),
                        input: TextInput::new(format!("{} {}", f.font_name, f.font_size)),
                    });
                }
            }
            KeyCode::Char('d') if is_plain(&key) => {
                if let Some(f) = field {
                    if count < 2 {
                        self.set_error("Notes require at least one field");
                    } else if f.required {
                        self.set_error(format!(
                            "The field {} is required by this note type and cannot be deleted",
                            f.name
                        ));
                    } else {
                        self.overlay = Overlay::ConfirmDeleteField(at);
                    }
                }
            }
            KeyCode::Char('s') if is_plain(&key) => {
                return self.edit_fields(NotetypeEdit::SetSortField { index: at }, false)
            }
            KeyCode::Char(c @ '1'..='4') if is_plain(&key) => {
                if let Some(mut f) = field {
                    match c {
                        '1' => f.rtl = !f.rtl,
                        '2' => f.plain_text = !f.plain_text,
                        '3' => f.collapsed = !f.collapsed,
                        _ => f.exclude_from_search = !f.exclude_from_search,
                    }
                    return self.edit_fields(
                        NotetypeEdit::SetFieldOptions {
                            index: at,
                            field: f,
                        },
                        false,
                    );
                }
            }
            _ => {}
        }
        NotetypesIntent::None
    }

    // ---- templates editor -------------------------------------------------------------

    fn edit_templates(
        &mut self,
        b: &dyn Backend,
        edit: NotetypeEdit,
        jump_to_end: bool,
    ) -> NotetypesIntent {
        if let Screen::Templates(s) = &mut self.screen {
            s.store_area();
            match s.draft.apply(edit) {
                Ok(()) => {
                    if jump_to_end {
                        s.index = s.draft.templates.len() - 1;
                    }
                    s.problem = None;
                    s.load_area();
                    s.refresh_preview(b);
                    self.status = None;
                }
                Err(e) => self.set_error(e.to_string()),
            }
        }
        NotetypesIntent::None
    }

    fn set_deck_override(
        &mut self,
        b: &dyn Backend,
        index: usize,
        name: String,
    ) -> NotetypesIntent {
        let target = if name.is_empty() {
            None
        } else {
            match b.execute(Command::Notes(NoteCommand::DeckNames {
                include_filtered: false,
            })) {
                Ok(Response::Edit(EditResponse::DeckNames(decks))) => {
                    match decks.iter().find(|d| d.name.eq_ignore_ascii_case(&name)) {
                        Some(d) => Some(d.id),
                        None => {
                            self.set_error(format!("There is no deck named {name}"));
                            return NotetypesIntent::None;
                        }
                    }
                }
                _ => {
                    self.set_error("Could not read the deck names");
                    return NotetypesIntent::None;
                }
            }
        };
        if let Screen::Templates(s) = &mut self.screen {
            s.store_area();
            if let Some(mut t) = s.draft.templates.get(index).cloned() {
                t.target_deck_id = target;
                return self.edit_templates(
                    b,
                    NotetypeEdit::SetTemplate { index, template: t },
                    false,
                );
            }
        }
        NotetypesIntent::None
    }

    fn deck_name(&self, b: &dyn Backend, id: i64) -> Option<String> {
        match b.execute(Command::Notes(NoteCommand::DeckNames {
            include_filtered: false,
        })) {
            Ok(Response::Edit(EditResponse::DeckNames(decks))) => {
                decks.into_iter().find(|d| d.id == id).map(|d| d.name)
            }
            _ => None,
        }
    }

    fn templates_key(&mut self, b: &dyn Backend, key: KeyEvent) -> NotetypesIntent {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let Screen::Templates(s) = &mut self.screen else {
            return NotetypesIntent::None;
        };
        let at = s.index;
        let count = s.draft.templates.len();
        match key.code {
            KeyCode::Char('s') | KeyCode::Char('S') if ctrl && !alt => return self.save(b),
            KeyCode::Esc => self.back(),
            KeyCode::Tab | KeyCode::BackTab => {
                let back = key.code == KeyCode::BackTab || shift;
                s.store_area();
                let pos = Tab::ALL.iter().position(|t| *t == s.tab).unwrap_or(0);
                let next = if back {
                    (pos + Tab::ALL.len() - 1) % Tab::ALL.len()
                } else {
                    (pos + 1) % Tab::ALL.len()
                };
                s.tab = Tab::ALL[next];
                s.load_area();
            }
            KeyCode::Char('[') if alt => s.select(b, at.saturating_sub(1)),
            KeyCode::Char(']') if alt => s.select(b, (at + 1).min(count - 1)),
            KeyCode::Char(',') if alt => {
                if at > 0 {
                    s.index -= 1;
                    return self.edit_templates(
                        b,
                        NotetypeEdit::MoveTemplate {
                            from: at,
                            to: at - 1,
                        },
                        false,
                    );
                }
            }
            KeyCode::Char('.') if alt => {
                if at + 1 < count {
                    s.index += 1;
                    return self.edit_templates(
                        b,
                        NotetypeEdit::MoveTemplate {
                            from: at,
                            to: at + 1,
                        },
                        false,
                    );
                }
            }
            KeyCode::Char('a') | KeyCode::Char('A') if alt => {
                self.overlay = Overlay::Prompt(Prompt {
                    title: "Name of the new card type (starts as a copy of the last)".into(),
                    kind: PromptKind::TemplateAdd,
                    input: TextInput::new(format!("Card {}", count + 1)),
                });
            }
            KeyCode::Char('r') | KeyCode::Char('R') if alt => {
                let name = s.draft.templates[at].name.clone();
                self.overlay = Overlay::Prompt(Prompt {
                    title: "Rename card type".into(),
                    kind: PromptKind::TemplateRename(at),
                    input: TextInput::new(name),
                });
            }
            KeyCode::Char('d') | KeyCode::Char('D') if alt => {
                if s.draft.kind == NotetypeKind::Cloze || count < 2 {
                    self.set_error("A note type needs at least one card type");
                } else {
                    self.overlay = Overlay::ConfirmDeleteTemplate(at);
                }
            }
            KeyCode::Char('o') | KeyCode::Char('O') if alt => {
                let current = s.draft.templates[at].target_deck_id;
                let name = current
                    .and_then(|id| self.deck_name(b, id))
                    .unwrap_or_default();
                self.overlay = Overlay::Prompt(Prompt {
                    title: "Deck override (empty for none)".into(),
                    kind: PromptKind::DeckOverride(at),
                    input: TextInput::new(name),
                });
            }
            KeyCode::Char('n') | KeyCode::Char('N') if alt => {
                if !s.samples.is_empty() {
                    s.sample = (s.sample + 1) % s.samples.len();
                    s.refresh_preview(b);
                }
            }
            KeyCode::Char('f') | KeyCode::Char('F') if alt => {
                s.fill_empty = !s.fill_empty;
                s.refresh_preview(b);
            }
            KeyCode::Char('x') | KeyCode::Char('X') if alt => {
                s.store_area();
                return NotetypesIntent::ExternalEdit {
                    text: s.area.value().to_owned(),
                    suffix: s.tab.suffix(),
                };
            }
            _ => {
                if s.area.handle_key(&key) {
                    s.edited(b);
                }
            }
        }
        NotetypesIntent::None
    }

    /// The result of `$EDITOR` for the part the editor was started on.
    pub fn apply_external(&mut self, b: &dyn Backend, text: String) {
        match &mut self.screen {
            Screen::Templates(s) => {
                if s.area.value() != text {
                    s.area.select_all();
                    s.area.paste(&text);
                    s.edited(b);
                }
            }
            Screen::Latex(s) => s.set_focused(text),
            _ => {}
        }
    }

    // ---- LaTeX ------------------------------------------------------------------------

    fn latex_key(&mut self, b: &dyn Backend, key: KeyEvent) -> NotetypesIntent {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Screen::Latex(s) = &mut self.screen else {
            return NotetypesIntent::None;
        };
        match key.code {
            KeyCode::Char('s') | KeyCode::Char('S') if ctrl => return self.save(b),
            KeyCode::Esc => self.back(),
            KeyCode::Tab => s.focus = (s.focus + 1) % 3,
            KeyCode::BackTab => s.focus = (s.focus + 2) % 3,
            KeyCode::Char(' ') | KeyCode::Enter if s.focus == 2 => {
                s.draft.latex_svg = !s.draft.latex_svg
            }
            _ => match s.focus {
                0 => {
                    s.pre.handle_key(&key);
                }
                1 => {
                    s.post.handle_key(&key);
                }
                _ => {}
            },
        }
        NotetypesIntent::None
    }
}

impl TemplatesScreen {
    pub(crate) fn tab_text(&self, tab: Tab) -> &str {
        let t = &self.draft.templates[self.index];
        match tab {
            Tab::Front => &t.question,
            Tab::Back => &t.answer,
            Tab::Styling => &self.draft.css,
            Tab::BrowserFront => &t.browser_question,
            Tab::BrowserBack => &t.browser_answer,
        }
    }

    /// The text area's content into the draft.
    pub(crate) fn store_area(&mut self) {
        let text = self.area.value().to_owned();
        match self.tab {
            Tab::Styling => self.draft.css = text,
            tab => {
                let t = &mut self.draft.templates[self.index];
                match tab {
                    Tab::Front => t.question = text,
                    Tab::Back => t.answer = text,
                    Tab::BrowserFront => t.browser_question = text,
                    _ => t.browser_answer = text,
                }
            }
        }
    }

    pub(crate) fn load_area(&mut self) {
        let text = self.tab_text(self.tab).to_owned();
        self.area = TextArea::new(text);
    }

    pub(crate) fn dirty(&self) -> bool {
        let mut now = self.draft.clone();
        let text = self.area.value().to_owned();
        match self.tab {
            Tab::Styling => now.css = text,
            tab => {
                let t = &mut now.templates[self.index];
                match tab {
                    Tab::Front => t.question = text,
                    Tab::Back => t.answer = text,
                    Tab::BrowserFront => t.browser_question = text,
                    _ => t.browser_answer = text,
                }
            }
        }
        now != self.original
    }

    pub(crate) fn select(&mut self, b: &dyn Backend, index: usize) {
        if index != self.index {
            self.store_area();
            self.index = index;
            self.load_area();
            self.problem = None;
            self.refresh_preview(b);
        }
    }

    /// After the text changed: keep the draft in step and re-render.
    pub(crate) fn edited(&mut self, b: &dyn Backend) {
        self.store_area();
        self.problem = None;
        self.refresh_preview(b);
    }

    pub(crate) fn template(&self) -> &TemplateDetail {
        &self.draft.templates[self.index]
    }

    pub(crate) fn refresh_preview(&mut self, b: &dyn Backend) {
        let note_id = self.samples.get(self.sample).copied();
        let result = run(
            b,
            NotetypeCommand::Preview {
                notetype_id: self.draft.id,
                note_id,
                template: self.template().clone(),
                css: Some(self.draft.css.clone()),
                card_ord: self.index as u32,
                fill_empty: note_id.is_none() || self.fill_empty,
            },
        );
        self.preview = Some(match result {
            Ok(NotetypeResponse::Preview(p)) => Ok(p),
            Ok(other) => Err(format!(
                "Unexpected response from the collection: {other:?}"
            )),
            Err(e) => Err(e.to_string()),
        });
    }
}

impl LatexScreen {
    pub(crate) fn store(&mut self) {
        self.draft.latex_pre = self.pre.value().to_owned();
        self.draft.latex_post = self.post.value().to_owned();
    }

    pub(crate) fn dirty(&self) -> bool {
        self.pre.value() != self.original.latex_pre
            || self.post.value() != self.original.latex_post
            || self.draft.latex_svg != self.original.latex_svg
    }

    fn paste(&mut self, text: &str) {
        match self.focus {
            0 => self.pre.paste(text),
            1 => self.post.paste(text),
            _ => {}
        }
    }

    fn set_focused(&mut self, text: String) {
        let area = match self.focus {
            0 => &mut self.pre,
            1 => &mut self.post,
            _ => return,
        };
        area.select_all();
        area.paste(&text);
    }
}
