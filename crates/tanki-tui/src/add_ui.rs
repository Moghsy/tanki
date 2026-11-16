//! Notes stream (Phase 3): the add screen (PLAN 8.4) and the edit form for an
//! existing note (edit current): note type and deck pickers, fields with
//! field checks, sticky fields, tags with completion, history, discard
//! prompt. A self-contained widget the lead wires into `app.rs`.
//!
//! # Wiring
//! - Build with [`AddUi::new_add`] (Add Cards) or [`AddUi::new_edit`] (edit an
//!   existing note, e.g. from the reviewer or the browser).
//! - Feed key presses to [`AddUi::on_key`] and bracketed paste to
//!   [`AddUi::on_paste`]; act on the returned [`AddIntent`].
//! - Draw with [`AddUi::render`]; show [`AddUi::hints`] in the footer.
//! - After other code mutated the collection, call [`AddUi::on_changes`]
//!   with the broadcast `OpChanges` (skip the widget's own, via
//!   `backend::Tracked::take_own`).
//!
//! Fields are edited as raw stored HTML in [`TextArea`]s (the markup editor
//! is Phase 6). Keys are Alt-chords because terminals cannot send most
//! Ctrl+letter combinations; see [`AddUi::hints`] and `KEYS`.
mod editor_cmds;
mod media;
mod render;
#[cfg(test)]
pub(crate) mod tests;

use crate::backend::Backend;
use crate::completion::{self, Candidate, Completion, CompletionOutcome, Proposal};
use crate::input::TextInput;
use crate::textarea::TextArea;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{buffer::Buffer, layout::Rect};
use std::path::PathBuf;
use tanki_core::{
    Command, EditResponse, FieldInfo, FieldsCheck, NoteCommand, NoteData, NoteUpdate, NotetypeInfo,
    NotetypeKind, OpChanges, Response,
};

pub use editor_cmds::{EditorCommand, PALETTE};
pub use media::{clipboard_image, ClipboardFn};

/// How many added notes the session history keeps (desktop: 15).
pub const HISTORY_LIMIT: usize = 15;
const LABEL_LIMIT: usize = 30;

/// What only the app can do, returned by [`AddUi::on_key`] / `on_paste`.
#[derive(Debug, Clone, PartialEq)]
pub enum AddIntent {
    PickCodeTour,
    None,
    /// Leave the screen (nothing unsaved, or the user chose to discard).
    Close,
    /// A note was added (the form stays open) or saved from the history
    /// (back to the add form): refresh counts and lists.
    Changed(OpChanges),
    /// The edit-current form saved a note: close and refresh.
    Saved(OpChanges),
    /// Show this in the app's message line.
    Message(String),
    /// Open the browser on this note (`nid:`).
    OpenBrowser {
        note_id: i64,
    },
    /// Run `$EDITOR` and hand the result back with
    /// [`AddUi::apply_external_field`] / [`AddUi::apply_external_note`].
    ExternalEdit(ExternalEdit),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalEdit {
    /// One field's stored HTML.
    Field { ord: usize, html: String },
    /// The whole note, for `editor_flow`.
    Note {
        note_id: Option<i64>,
        notetype: String,
        deck: String,
        fields: Vec<(String, String)>,
        tags: Vec<String>,
    },
}

/// The key table, for the help overlay and tests: (keys, action).
pub const KEYS: &[(&str, &str)] = &[
    ("Ctrl+Enter / Ctrl+S / Alt+Enter", "add (save in edit mode)"),
    ("Esc", "close (asks when there is unsaved input)"),
    ("Tab / Shift+Tab", "next / previous field"),
    ("Ctrl+N / Ctrl+D", "note type / deck picker"),
    ("Ctrl+T", "tags"),
    ("Alt+S / Alt+Shift+S", "sticky: this field / all"),
    ("Alt+P, Alt+[ / Alt+]", "preview, previous / next card"),
    ("Alt+H", "history of added notes"),
    ("Alt+B / I / U", "bold / italic / underline"),
    ("Alt+T", "text colour"),
    ("Alt+L / Alt+O", "bulleted / numbered list"),
    ("Alt+C / Alt+Shift+C", "cloze: new number / same number"),
    (
        "Alt+M / Alt+Shift+M / Alt+E",
        "math inline / block / chemistry",
    ),
    ("Alt+F / Alt+V", "attach file or URL / clipboard image"),
    ("Alt+X / Alt+Shift+X", "edit field / note in $EDITOR"),
    ("Alt+G", "choose a saved code-tour"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Field(usize),
    Tags,
}

struct FieldEditor {
    info: FieldInfo,
    area: TextArea,
}

struct EditTarget {
    id: i64,
    mtime: u32,
    fields: Vec<String>,
    tags: Vec<String>,
}

/// Everything about the note being written. The widget keeps a second one
/// stashed while a history note is reopened for editing.
struct Form {
    notetype: NotetypeInfo,
    deck_id: i64,
    fields: Vec<FieldEditor>,
    tags: TextInput,
    focus: Focus,
    check: FieldsCheck,
    /// What the fields held when the form was (re)loaded: sticky values
    /// carried over by an add. Close asks only if something else is typed.
    baseline: Vec<String>,
    edit: Option<EditTarget>,
    preview_idx: usize,
}

impl Form {
    fn new(notetype: NotetypeInfo, deck_id: i64, values: Vec<String>) -> Self {
        let fields: Vec<FieldEditor> = notetype
            .fields
            .iter()
            .enumerate()
            .map(|(i, info)| FieldEditor {
                info: info.clone(),
                area: TextArea::new(values.get(i).cloned().unwrap_or_default()),
            })
            .collect();
        let baseline = fields.iter().map(|f| f.area.value().to_owned()).collect();
        Self {
            notetype,
            deck_id,
            fields,
            tags: TextInput::default(),
            focus: Focus::Field(0),
            check: FieldsCheck::Normal,
            baseline,
            edit: None,
            preview_idx: 0,
        }
    }

    fn values(&self) -> Vec<String> {
        self.fields
            .iter()
            .map(|f| f.area.value().to_owned())
            .collect()
    }

    fn tag_list(&self) -> Vec<String> {
        parse_tags(self.tags.text())
    }

    fn note_data(&self) -> NoteData {
        NoteData {
            id: self.edit.as_ref().map_or(0, |e| e.id),
            mtime_secs: self.edit.as_ref().map_or(0, |e| e.mtime),
            notetype_id: self.notetype.id,
            tags: self.tag_list(),
            fields: self.values(),
            ..NoteData::default()
        }
    }

    fn is_dirty(&self) -> bool {
        match &self.edit {
            Some(e) => self.values() != e.fields || tags_changed(&e.tags, &self.tag_list()),
            None => self.fields.iter().zip(&self.baseline).any(|(f, base)| {
                let v = f.area.value();
                !is_blank(v) && v != base
            }),
        }
    }
}

/// Desktop's "blank" test: nothing but `<br>` and whitespace.
pub fn is_blank(s: &str) -> bool {
    s.replace("<br>", "")
        .replace("<br/>", "")
        .replace("<br />", "")
        .trim()
        .is_empty()
}

pub fn parse_tags(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_owned).collect()
}

fn tags_changed(old: &[String], new: &[String]) -> bool {
    let mut a = old.to_vec();
    let mut b = new.to_vec();
    a.sort();
    b.sort();
    a != b
}

/// Changing note type with content (R7 §1.3, desktop's algorithm): copy
/// non-empty fields with the same name, then the remaining non-empty old
/// fields by position into the slots not filled by name.
pub fn carry_fields(
    old_names: &[String],
    old_values: &[String],
    new_names: &[String],
) -> Vec<String> {
    let mut out = vec![String::new(); new_names.len()];
    let mut copied: Vec<&str> = Vec::new();
    for (ni, name) in new_names.iter().enumerate() {
        if let Some(oi) = old_names.iter().position(|n| n == name) {
            if let Some(v) = old_values.get(oi).filter(|v| !v.is_empty()) {
                out[ni] = v.clone();
                copied.push(name);
            }
        }
    }
    let mut new_idx = 0;
    for (oi, old_name) in old_names.iter().enumerate() {
        while new_idx < new_names.len() && copied.contains(&new_names[new_idx].as_str()) {
            new_idx += 1;
        }
        if new_idx >= new_names.len() {
            break;
        }
        if !copied.contains(&old_name.as_str()) && old_values.get(oi).is_some_and(|v| !v.is_empty())
        {
            out[new_idx] = old_values[oi].clone();
            new_idx += 1;
        }
    }
    out
}

/// Label for the history list: `HtmlToText` of the joined fields, cut short.
fn truncate_label(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= LABEL_LIMIT {
        text.to_owned()
    } else {
        let cut: String = text.chars().take(LABEL_LIMIT - 1).collect();
        format!("{cut}…")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub note_id: i64,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickerKind {
    Notetype,
    Deck,
}

struct Picker {
    title: &'static str,
    items: Vec<(i64, String)>,
    query: TextInput,
    selected: usize,
}

impl Picker {
    fn matches(&self) -> Vec<usize> {
        crate::fuzzy::rank(
            self.query.text(),
            self.items.iter().map(|(_, n)| n.as_str()),
        )
    }
}

enum Overlay {
    None,
    Picker(PickerKind, Picker),
    History(usize),
    Colour(usize),
    MediaPath {
        input: TextInput,
        completions: Vec<String>,
        selected: usize,
    },
    PasteOffer {
        paths: Vec<PathBuf>,
        text: String,
    },
    ConfirmDiscard,
    ConfirmCloze,
    Conflict {
        conflicts: Vec<i64>,
        missing: Vec<i64>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Status {
    text: String,
    error: bool,
}

pub struct AddUi {
    form: Form,
    stash: Option<Form>,
    notetypes: Vec<NotetypeInfo>,
    decks: Vec<(i64, String)>,
    history: Vec<HistoryEntry>,
    overlay: Overlay,
    /// The shared completion popup over the tags input (opens while typing).
    tag_completion: Completion,
    preview_on: bool,
    preview: Option<Result<tanki_core::RenderedCard, String>>,
    status: Option<Status>,
    media_dir: PathBuf,
    night_mode: bool,
    clipboard: ClipboardFn,
}

fn edit_response(r: Response) -> Result<EditResponse, String> {
    match r {
        Response::Edit(e) => Ok(e),
        other => Err(format!(
            "Unexpected response from the collection: {other:?}"
        )),
    }
}

fn run(b: &dyn Backend, command: NoteCommand) -> Result<EditResponse, String> {
    b.execute(Command::Notes(command))
        .map_err(|e| e.to_string())
        .and_then(edit_response)
}

fn unexpected<T>(r: EditResponse) -> Result<T, String> {
    Err(format!("Unexpected response from the collection: {r:?}"))
}

fn fetch_notetype(b: &dyn Backend, id: i64) -> Result<NotetypeInfo, String> {
    match run(b, NoteCommand::Notetype { id })? {
        EditResponse::Notetype(n) => Ok(n),
        other => unexpected(other),
    }
}

fn fetch_notetypes(b: &dyn Backend) -> Result<Vec<NotetypeInfo>, String> {
    match run(b, NoteCommand::Notetypes)? {
        EditResponse::Notetypes(n) => Ok(n),
        other => unexpected(other),
    }
}

fn fetch_decks(b: &dyn Backend) -> Result<Vec<(i64, String)>, String> {
    match run(
        b,
        NoteCommand::DeckNames {
            include_filtered: false,
        },
    )? {
        EditResponse::DeckNames(d) => Ok(d.into_iter().map(|d| (d.id, d.name)).collect()),
        other => unexpected(other),
    }
}

fn fetch_note(b: &dyn Backend, id: i64) -> Result<NoteData, String> {
    match run(b, NoteCommand::GetNote { id })? {
        EditResponse::Note(n) => Ok(n),
        other => unexpected(other),
    }
}

/// Completes the space-separated tag at the cursor with `CompleteTag`.
fn tag_source(b: &dyn Backend) -> impl FnMut(&str, usize) -> Option<Proposal> + '_ {
    move |text: &str, cursor: usize| {
        let range = completion::word_token(text, cursor);
        let word = &text[range.clone()];
        if word.is_empty() {
            return None;
        }
        let candidates = match run(
            b,
            NoteCommand::CompleteTag {
                input: word.to_owned(),
                limit: 8,
            },
        ) {
            Ok(EditResponse::Strings(tags)) => tags
                .into_iter()
                .filter(|t| t != word)
                .map(|t| Candidate::new(t, ""))
                .collect(),
            _ => Vec::new(),
        };
        Some(Proposal { range, candidates })
    }
}

const IO_REFUSED: &str = "Image occlusion notes can't be created or edited in tanki yet";

impl AddUi {
    fn base(
        form: Form,
        notetypes: Vec<NotetypeInfo>,
        decks: Vec<(i64, String)>,
        media_dir: PathBuf,
    ) -> Self {
        Self {
            form,
            stash: None,
            notetypes,
            decks,
            history: Vec::new(),
            overlay: Overlay::None,
            tag_completion: Completion::new(true),
            preview_on: false,
            preview: None,
            status: None,
            media_dir,
            night_mode: true,
            clipboard: Box::new(clipboard_image),
        }
    }

    /// The Add Cards screen. `home_deck` is the reviewer card's home deck,
    /// or 0. The defaults come from `DefaultsForAdding`.
    pub fn new_add(b: &dyn Backend, home_deck: i64, media_dir: PathBuf) -> Result<Self, String> {
        let notetypes = fetch_notetypes(b)?;
        let decks = fetch_decks(b)?;
        let (deck_id, notetype_id) = match run(b, NoteCommand::DefaultsForAdding { home_deck })? {
            EditResponse::Defaults {
                deck_id,
                notetype_id,
            } => (deck_id, notetype_id),
            other => return unexpected(other),
        };
        let mut note_type = notetypes
            .iter()
            .find(|n| n.id == notetype_id && !n.image_occlusion)
            .cloned();
        let refused = note_type.is_none()
            && notetypes
                .iter()
                .any(|n| n.id == notetype_id && n.image_occlusion);
        if note_type.is_none() {
            note_type = notetypes.iter().find(|n| !n.image_occlusion).cloned();
        }
        let note_type = note_type.ok_or("There is no note type tanki can add notes with")?;
        let form = Form::new(note_type.clone(), deck_id, Vec::new());
        let mut ui = Self::base(form, notetypes, decks, media_dir);
        if refused {
            ui.set_error(format!("{IO_REFUSED}; using {}", note_type.name));
        }
        ui.after_edit(b);
        Ok(ui)
    }

    /// Edit current: loads the note with `GetNote`.
    pub fn new_edit(b: &dyn Backend, note_id: i64, media_dir: PathBuf) -> Result<Self, String> {
        let form = Self::load_edit_form(b, note_id)?;
        let notetypes = fetch_notetypes(b)?;
        let mut ui = Self::base(form, notetypes, Vec::new(), media_dir);
        ui.after_edit(b);
        Ok(ui)
    }

    fn load_edit_form(b: &dyn Backend, note_id: i64) -> Result<Form, String> {
        let note = fetch_note(b, note_id)?;
        let notetype = fetch_notetype(b, note.notetype_id)?;
        if notetype.image_occlusion {
            return Err(IO_REFUSED.into());
        }
        let mut form = Form::new(notetype, 0, note.fields.clone());
        form.tags = TextInput::new(note.tags.join(" "));
        form.edit = Some(EditTarget {
            id: note.id,
            mtime: note.mtime_secs,
            fields: note.fields,
            tags: note.tags,
        });
        Ok(form)
    }

    /// Replaces the clipboard reader (tests).
    pub fn set_clipboard(&mut self, f: ClipboardFn) {
        self.clipboard = f;
    }

    /// Night mode for the preview renderer (from `Display`).
    pub fn set_night_mode(&mut self, night: bool) {
        self.night_mode = night;
    }

    // ---- read access ---------------------------------------------------------

    pub fn is_edit(&self) -> bool {
        self.form.edit.is_some()
    }

    pub fn edited_note_id(&self) -> Option<i64> {
        self.form.edit.as_ref().map(|e| e.id)
    }

    pub fn notetype_name(&self) -> &str {
        &self.form.notetype.name
    }

    pub fn deck_id(&self) -> i64 {
        self.form.deck_id
    }

    pub fn code_tour_context(&self) -> Option<(i64, Option<i64>)> {
        self.form
            .notetype
            .fields
            .iter()
            .any(|f| f.name == tanki_core::code_tour::FIELD)
            .then(|| (self.form.notetype.id, self.edited_note_id()))
    }
    pub fn set_code_tour(
        &mut self,
        b: &dyn Backend,
        reference: tanki_core::code_tour::CodeTourRef,
    ) {
        if let Some(ord) = self
            .form
            .notetype
            .fields
            .iter()
            .position(|f| f.name == tanki_core::code_tour::FIELD)
        {
            self.apply_external_field(b, ord, reference.uri());
        }
    }
    pub fn field_values(&self) -> Vec<String> {
        self.form.values()
    }

    pub fn tags_text(&self) -> &str {
        self.form.tags.text()
    }

    pub fn check(&self) -> FieldsCheck {
        self.form.check
    }

    pub fn history(&self) -> &[HistoryEntry] {
        &self.history
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_ref().map(|s| s.text.as_str())
    }

    pub fn has_unsaved_input(&self) -> bool {
        self.form.is_dirty()
    }

    /// True while a modal prompt or picker is up.
    pub fn has_overlay(&self) -> bool {
        !matches!(self.overlay, Overlay::None)
    }

    /// Context keys for the app's footer.
    pub fn hints(&self) -> String {
        match &self.overlay {
            Overlay::None => {
                if self.form.focus == Focus::Tags && self.tag_completion.is_open() {
                    "Tab/↓ next · ↑ previous · Enter accept · Esc dismiss".into()
                } else if self.is_edit() {
                    "Ctrl+Enter save · Tab next field · Alt+B/I/U/C format · Alt+P preview · Alt+F attach · Alt+X $EDITOR · Esc close".into()
                } else {
                    "Ctrl+Enter add · Tab next field · Ctrl+N type · Ctrl+D deck · Alt+S sticky · Alt+H history · Alt+B/I/U/C format · Alt+P preview · Alt+F attach · Esc close".into()
                }
            }
            Overlay::Picker(..) => "type to filter · ↑↓ choose · Enter select · Esc cancel".into(),
            Overlay::History(_) => "↑↓ choose · Enter edit · b open in browser · Esc close".into(),
            Overlay::Colour(_) => "1-6 or ↑↓ choose · Enter apply · Esc cancel".into(),
            Overlay::MediaPath { .. } => {
                "path, file:// or http(s) URL · Tab complete · Enter attach · Esc cancel".into()
            }
            Overlay::PasteOffer { .. } => "Enter attach file · n paste as text · Esc cancel".into(),
            Overlay::ConfirmDiscard => "y discard · n keep editing".into(),
            Overlay::ConfirmCloze => "y add anyway · n keep editing".into(),
            Overlay::Conflict { .. } => "r reload the note · Esc keep editing".into(),
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

    // ---- status ----------------------------------------------------------------

    fn set_status(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            error: false,
        });
    }

    fn set_error(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            error: true,
        });
    }

    // ---- checks and preview ------------------------------------------------------

    fn run_check(&mut self, b: &dyn Backend) {
        match run(
            b,
            NoteCommand::CheckFields {
                note: self.form.note_data(),
            },
        ) {
            Ok(EditResponse::FieldsCheck(c)) => self.form.check = c,
            Ok(_) => {}
            Err(e) => self.set_error(e),
        }
    }

    fn cloze_numbers(&mut self, b: &dyn Backend) -> Vec<u32> {
        match run(
            b,
            NoteCommand::ClozeNumbers {
                note: self.form.note_data(),
            },
        ) {
            Ok(EditResponse::Numbers(n)) => n,
            Ok(_) => Vec::new(),
            Err(e) => {
                self.set_error(e);
                Vec::new()
            }
        }
    }

    /// Card ordinals the preview can show: the templates, or one per cloze
    /// number in use.
    fn preview_ords(&mut self, b: &dyn Backend) -> Vec<u32> {
        match self.form.notetype.kind {
            NotetypeKind::Normal => (0..self.form.notetype.templates.len().max(1) as u32).collect(),
            NotetypeKind::Cloze => {
                let mut n = self.cloze_numbers(b);
                n.sort_unstable();
                n.dedup();
                let ords: Vec<u32> = n.into_iter().filter(|n| *n > 0).map(|n| n - 1).collect();
                if ords.is_empty() {
                    vec![0]
                } else {
                    ords
                }
            }
        }
    }

    fn refresh_preview(&mut self, b: &dyn Backend) {
        if !self.preview_on {
            return;
        }
        let ords = self.preview_ords(b);
        self.form.preview_idx = self.form.preview_idx.min(ords.len() - 1);
        let ord = ords[self.form.preview_idx];
        self.preview = Some(
            match run(
                b,
                NoteCommand::RenderPreview {
                    note: self.form.note_data(),
                    card_ord: ord,
                },
            ) {
                Ok(EditResponse::Rendered(card)) => Ok(card),
                Ok(other) => unexpected(other),
                Err(e) => Err(e),
            },
        );
    }

    /// After any content change: field check and preview.
    fn after_edit(&mut self, b: &dyn Backend) {
        self.run_check(b);
        self.refresh_preview(b);
    }

    // ---- tags ----------------------------------------------------------------------

    /// The tag completion key: an Enter-accepted tag is followed by a space,
    /// as desktop's tag editor does, so the next tag can be typed at once.
    fn tag_completion_key(&mut self, b: &dyn Backend, key: &KeyEvent) -> CompletionOutcome {
        let before = self.form.tags.text().to_owned();
        let outcome = self
            .tag_completion
            .handle_key(key, &mut self.form.tags, &mut tag_source(b));
        if outcome == CompletionOutcome::Accepted {
            let rest = &self.form.tags.text()[completion::cursor_byte(&self.form.tags)..];
            if rest.starts_with(' ') {
                self.form
                    .tags
                    .handle_key(&KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
            } else {
                self.form.tags.insert_str(" ");
            }
        }
        if self.form.tags.text() != before {
            self.run_check(b);
        }
        outcome
    }

    // ---- input -----------------------------------------------------------------------

    /// Bracketed paste.
    pub fn on_paste(&mut self, b: &dyn Backend, text: &str) -> AddIntent {
        match &mut self.overlay {
            Overlay::None => {}
            Overlay::Picker(_, p) => {
                p.query.insert_str(text);
                p.selected = 0;
                return AddIntent::None;
            }
            Overlay::MediaPath {
                input,
                completions,
                selected,
            } => {
                input.insert_str(text);
                *completions = media::complete_path(input.text());
                *selected = 0;
                return AddIntent::None;
            }
            _ => return AddIntent::None,
        }
        match self.form.focus {
            Focus::Tags => {
                self.form.tags.insert_str(text);
                self.tag_completion
                    .refresh(&self.form.tags, &mut tag_source(b));
                self.run_check(b);
            }
            Focus::Field(i) => {
                let paths = media::detect_paths(text);
                if !paths.is_empty() {
                    self.overlay = Overlay::PasteOffer {
                        paths,
                        text: text.to_owned(),
                    };
                } else {
                    self.form.fields[i].area.paste(text);
                    self.after_edit(b);
                }
            }
        }
        AddIntent::None
    }

    pub fn on_key(&mut self, b: &dyn Backend, key: KeyEvent) -> AddIntent {
        if key.kind == KeyEventKind::Release {
            return AddIntent::None;
        }
        let overlay = std::mem::replace(&mut self.overlay, Overlay::None);
        if !matches!(overlay, Overlay::None) {
            return self.overlay_key(b, overlay, key);
        }
        self.form_key(b, key)
    }

    fn form_key(&mut self, b: &dyn Backend, key: KeyEvent) -> AddIntent {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let tags_focused = self.form.focus == Focus::Tags;
        // Add / save.
        let is_add = match key.code {
            KeyCode::Enter => ctrl || alt,
            KeyCode::Char('s') | KeyCode::Char('S') => ctrl && !alt,
            _ => false,
        };
        if is_add {
            return self.submit(b);
        }
        // The tags input and its popup take the key first; Tab with nothing
        // to complete, Esc with the popup closed and shortcuts fall through.
        if tags_focused && self.tag_completion_key(b, &key) != CompletionOutcome::Ignored {
            return AddIntent::None;
        }
        match key.code {
            KeyCode::Esc => return self.request_close(),
            KeyCode::Tab => {
                self.move_focus(true);
                return AddIntent::None;
            }
            KeyCode::BackTab => {
                self.move_focus(false);
                return AddIntent::None;
            }
            KeyCode::Char('n') if ctrl => return self.open_picker(PickerKind::Notetype),
            KeyCode::Char('d') if ctrl => return self.open_picker(PickerKind::Deck),
            KeyCode::Char('t') if ctrl => {
                self.form.focus = Focus::Tags;
                return AddIntent::None;
            }
            _ => {}
        }
        if alt {
            if let KeyCode::Char(c) = key.code {
                let upper = shift || c.is_uppercase();
                if let Some(intent) = self.alt_key(b, c.to_ascii_lowercase(), upper) {
                    return intent;
                }
            }
        }
        match self.form.focus {
            Focus::Tags => AddIntent::None,
            Focus::Field(i) => {
                let before = self.form.fields[i].area.value().to_owned();
                if self.form.fields[i].area.handle_key(&key)
                    && self.form.fields[i].area.value() != before
                {
                    self.after_edit(b);
                }
                AddIntent::None
            }
        }
    }

    fn move_focus(&mut self, forward: bool) {
        let n = self.form.fields.len();
        self.tag_completion.close();
        self.form.focus = match (self.form.focus, forward) {
            (Focus::Field(i), true) if i + 1 < n => Focus::Field(i + 1),
            (Focus::Field(_), true) => Focus::Tags,
            (Focus::Tags, true) => Focus::Field(0),
            (Focus::Field(0), false) => Focus::Tags,
            (Focus::Field(i), false) => Focus::Field(i - 1),
            (Focus::Tags, false) => Focus::Field(n.saturating_sub(1)),
        };
    }

    fn alt_key(&mut self, b: &dyn Backend, c: char, upper: bool) -> Option<AddIntent> {
        let field = match self.form.focus {
            Focus::Field(i) => Some(i),
            Focus::Tags => None,
        };
        let format = |ui: &mut Self, cmd: EditorCommand| {
            if let Some(i) = field {
                editor_cmds::apply(&mut ui.form.fields[i].area, cmd);
                ui.after_edit(b);
            }
            Some(AddIntent::None)
        };
        match (c, upper) {
            ('g', _) => Some(AddIntent::PickCodeTour),
            ('b', _) => format(self, EditorCommand::Bold),
            ('i', _) => format(self, EditorCommand::Italic),
            ('u', _) => format(self, EditorCommand::Underline),
            ('l', _) => format(self, EditorCommand::UnorderedList),
            ('o', _) => format(self, EditorCommand::OrderedList),
            ('e', _) => format(self, EditorCommand::Chemistry),
            ('m', false) => format(self, EditorCommand::MathInline),
            ('m', true) => format(self, EditorCommand::MathBlock),
            ('t', _) => {
                if field.is_some() {
                    self.overlay = Overlay::Colour(0);
                }
                Some(AddIntent::None)
            }
            ('c', upper) => {
                if field.is_some() {
                    let numbers = self.cloze_numbers(b);
                    let n = if upper {
                        editor_cmds::same_cloze_number(&numbers)
                    } else {
                        editor_cmds::next_cloze_number(&numbers)
                    };
                    return format(self, EditorCommand::Cloze(n));
                }
                Some(AddIntent::None)
            }
            ('s', false) => Some(self.toggle_sticky(b, field)),
            ('s', true) => Some(self.toggle_sticky_all(b)),
            ('p', _) => {
                self.preview_on = !self.preview_on;
                self.preview = None;
                self.refresh_preview(b);
                Some(AddIntent::None)
            }
            ('[', _) | (']', _) => {
                if self.preview_on {
                    let n = self.preview_ords(b).len();
                    let idx = self.form.preview_idx;
                    self.form.preview_idx = if c == ']' {
                        (idx + 1) % n
                    } else {
                        (idx + n - 1) % n
                    };
                    self.refresh_preview(b);
                }
                Some(AddIntent::None)
            }
            ('h', _) => {
                if self.is_edit() {
                    return Some(AddIntent::None);
                }
                if self.history.is_empty() {
                    self.set_status("Nothing added in this session yet");
                } else {
                    self.overlay = Overlay::History(0);
                }
                Some(AddIntent::None)
            }
            ('f', _) => {
                if field.is_some() {
                    self.overlay = Overlay::MediaPath {
                        input: TextInput::default(),
                        completions: Vec::new(),
                        selected: 0,
                    };
                }
                Some(AddIntent::None)
            }
            ('v', _) => {
                if let Some(i) = field {
                    self.paste_clipboard_image(b, i);
                }
                Some(AddIntent::None)
            }
            ('x', false) => field.map(|ord| {
                AddIntent::ExternalEdit(ExternalEdit::Field {
                    ord,
                    html: self.form.fields[ord].area.value().to_owned(),
                })
            }),
            ('x', true) => Some(AddIntent::ExternalEdit(self.external_note())),
            ('a', _) => {
                if let Some(i) = field {
                    self.form.fields[i].area.select_all();
                }
                Some(AddIntent::None)
            }
            _ => None,
        }
    }

    fn external_note(&self) -> ExternalEdit {
        let deck = self
            .decks
            .iter()
            .find(|(id, _)| *id == self.form.deck_id)
            .map(|(_, n)| n.clone())
            .unwrap_or_default();
        ExternalEdit::Note {
            note_id: self.edited_note_id(),
            notetype: self.form.notetype.name.clone(),
            deck,
            fields: self
                .form
                .fields
                .iter()
                .map(|f| (f.info.name.clone(), f.area.value().to_owned()))
                .collect(),
            tags: self.form.tag_list(),
        }
    }

    /// Result of `$EDITOR` on one field. The change is one undo step.
    pub fn apply_external_field(&mut self, b: &dyn Backend, ord: usize, html: String) {
        if let Some(f) = self.form.fields.get_mut(ord) {
            if f.area.value() != html {
                f.area.select_all();
                f.area.paste(&html);
                self.after_edit(b);
            }
        }
    }

    /// Result of `$EDITOR` on the whole note (values in field order).
    pub fn apply_external_note(&mut self, b: &dyn Backend, fields: Vec<String>, tags: Vec<String>) {
        for (f, v) in self.form.fields.iter_mut().zip(fields) {
            if f.area.value() != v {
                f.area.select_all();
                f.area.paste(&v);
            }
        }
        self.form.tags = TextInput::new(tags.join(" "));
        self.after_edit(b);
    }

    // ---- sticky ------------------------------------------------------------------------

    fn persist_sticky(&mut self, b: &dyn Backend, ord: usize, sticky: bool) -> bool {
        let id = self.form.notetype.id;
        match run(
            b,
            NoteCommand::SetSticky {
                notetype_id: id,
                field_ord: ord as u32,
                sticky,
            },
        ) {
            Ok(_) => {
                self.form.fields[ord].info.sticky = sticky;
                if let Some(nt) = self.notetypes.iter_mut().find(|n| n.id == id) {
                    if let Some(f) = nt.fields.get_mut(ord) {
                        f.sticky = sticky;
                    }
                }
                true
            }
            Err(e) => {
                self.set_error(e);
                false
            }
        }
    }

    fn toggle_sticky(&mut self, b: &dyn Backend, field: Option<usize>) -> AddIntent {
        if let Some(i) = field {
            let new = !self.form.fields[i].info.sticky;
            self.persist_sticky(b, i, new);
        }
        AddIntent::None
    }

    /// Desktop's toggle-all: if any field is sticky unstick those, else
    /// make every field sticky.
    fn toggle_sticky_all(&mut self, b: &dyn Backend) -> AddIntent {
        let any = self.form.fields.iter().any(|f| f.info.sticky);
        for i in 0..self.form.fields.len() {
            if self.form.fields[i].info.sticky == any && !self.persist_sticky(b, i, !any) {
                break;
            }
        }
        AddIntent::None
    }

    // ---- media ---------------------------------------------------------------------------

    fn insert_media(&mut self, b: &dyn Backend, command: NoteCommand) {
        let Focus::Field(i) = self.form.focus else {
            return;
        };
        match run(b, command) {
            Ok(EditResponse::Media(m)) => {
                self.form.fields[i].area.insert_at_cursor(&m.field_text);
                self.set_status(format!("Attached {}", m.filename));
                self.after_edit(b);
            }
            Ok(other) => self.set_error(format!("Unexpected response: {other:?}")),
            Err(e) => self.set_error(e),
        }
    }

    fn paste_clipboard_image(&mut self, b: &dyn Backend, _field: usize) {
        match (self.clipboard)() {
            Ok(data) => self.insert_media(
                b,
                NoteCommand::AddMedia {
                    desired_name: media::clipboard_name(),
                    data,
                },
            ),
            Err(e) => self.set_error(e),
        }
    }

    fn attach_text(&mut self, b: &dyn Backend, text: &str) {
        let text = text.trim();
        if text.starts_with("http://")
            || text.starts_with("https://")
            || text.starts_with("file://")
        {
            if let Some(path) = media::detect_paths(text).first() {
                self.insert_media(b, NoteCommand::AddMediaFromPath { path: path.clone() });
            } else {
                self.insert_media(
                    b,
                    NoteCommand::AddMediaFromUrl {
                        url: text.to_owned(),
                    },
                );
            }
        } else if !text.is_empty() {
            let path = media::detect_paths(text)
                .into_iter()
                .next()
                .unwrap_or_else(|| media::expand_home(text));
            self.insert_media(b, NoteCommand::AddMediaFromPath { path });
        }
    }

    // ---- overlays -----------------------------------------------------------------------

    fn open_picker(&mut self, kind: PickerKind) -> AddIntent {
        if self.is_edit() {
            self.set_status(match kind {
                PickerKind::Notetype => "The note type of an existing note can't be changed here",
                PickerKind::Deck => "Move cards to another deck in the browser",
            });
            return AddIntent::None;
        }
        let (title, items) = match kind {
            PickerKind::Notetype => (
                "Note type",
                self.notetypes
                    .iter()
                    .map(|n| (n.id, n.name.clone()))
                    .collect::<Vec<_>>(),
            ),
            PickerKind::Deck => ("Deck", self.decks.clone()),
        };
        let current = match kind {
            PickerKind::Notetype => self.form.notetype.id,
            PickerKind::Deck => self.form.deck_id,
        };
        let selected = items.iter().position(|(id, _)| *id == current).unwrap_or(0);
        let mut picker = Picker {
            title,
            items,
            query: TextInput::default(),
            selected: 0,
        };
        picker.selected = selected;
        self.overlay = Overlay::Picker(kind, picker);
        AddIntent::None
    }

    fn overlay_key(&mut self, b: &dyn Backend, overlay: Overlay, key: KeyEvent) -> AddIntent {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match overlay {
            Overlay::None => AddIntent::None,
            Overlay::Picker(kind, mut p) => {
                let n = p.matches().len();
                match key.code {
                    KeyCode::Esc => return AddIntent::None,
                    KeyCode::Up => p.selected = p.selected.saturating_sub(1),
                    KeyCode::Down => p.selected = (p.selected + 1).min(n.saturating_sub(1)),
                    KeyCode::Char('p') if ctrl => p.selected = p.selected.saturating_sub(1),
                    KeyCode::Char('n') if ctrl => {
                        p.selected = (p.selected + 1).min(n.saturating_sub(1))
                    }
                    KeyCode::Enter => {
                        let matches = p.matches();
                        if let Some(&idx) = matches.get(p.selected.min(n.saturating_sub(1))) {
                            let id = p.items[idx].0;
                            self.picked(b, kind, id);
                        }
                        return AddIntent::None;
                    }
                    _ => {
                        if p.query.handle_key(&key) {
                            p.selected = 0;
                        }
                    }
                }
                // Selection is an index into the ranked matches.
                self.overlay = Overlay::Picker(kind, p);
                AddIntent::None
            }
            Overlay::History(mut sel) => {
                match key.code {
                    KeyCode::Esc => return AddIntent::None,
                    KeyCode::Up => sel = sel.saturating_sub(1),
                    KeyCode::Down => sel = (sel + 1).min(self.history.len().saturating_sub(1)),
                    KeyCode::Char('b') => {
                        if let Some(h) = self.history.get(sel) {
                            return AddIntent::OpenBrowser { note_id: h.note_id };
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(id) = self.history.get(sel).map(|h| h.note_id) {
                            self.reopen_from_history(b, id);
                        }
                        return AddIntent::None;
                    }
                    _ => {}
                }
                self.overlay = Overlay::History(sel);
                AddIntent::None
            }
            Overlay::Colour(mut sel) => {
                let apply = |ui: &mut Self, i: usize| {
                    if let Focus::Field(f) = ui.form.focus {
                        editor_cmds::apply(&mut ui.form.fields[f].area, EditorCommand::Colour(i));
                        ui.after_edit(b);
                    }
                };
                match key.code {
                    KeyCode::Esc => return AddIntent::None,
                    KeyCode::Up => sel = sel.saturating_sub(1),
                    KeyCode::Down => sel = (sel + 1).min(PALETTE.len() - 1),
                    KeyCode::Enter => {
                        apply(self, sel);
                        return AddIntent::None;
                    }
                    KeyCode::Char(c @ '1'..='9') if (c as usize - '1' as usize) < PALETTE.len() => {
                        apply(self, c as usize - '1' as usize);
                        return AddIntent::None;
                    }
                    _ => {}
                }
                self.overlay = Overlay::Colour(sel);
                AddIntent::None
            }
            Overlay::MediaPath {
                mut input,
                mut completions,
                mut selected,
            } => {
                match key.code {
                    KeyCode::Esc => return AddIntent::None,
                    KeyCode::Enter => {
                        let text = input.text().to_owned();
                        self.attach_text(b, &text);
                        return AddIntent::None;
                    }
                    KeyCode::Tab => {
                        if let Some(c) = completions.get(selected).cloned() {
                            input = TextInput::new(c);
                            completions = media::complete_path(input.text());
                            selected = 0;
                        }
                    }
                    KeyCode::Down if !completions.is_empty() => {
                        selected = (selected + 1) % completions.len();
                    }
                    KeyCode::Up if !completions.is_empty() => {
                        selected = (selected + completions.len() - 1) % completions.len();
                    }
                    _ => {
                        if input.handle_key(&key) {
                            completions = media::complete_path(input.text());
                            selected = 0;
                        }
                    }
                }
                self.overlay = Overlay::MediaPath {
                    input,
                    completions,
                    selected,
                };
                AddIntent::None
            }
            Overlay::PasteOffer { paths, text } => {
                match key.code {
                    KeyCode::Esc => {}
                    KeyCode::Char('n') | KeyCode::Char('N') => {
                        if let Focus::Field(i) = self.form.focus {
                            self.form.fields[i].area.paste(&text);
                            self.after_edit(b);
                        }
                    }
                    KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                        for path in paths {
                            self.insert_media(b, NoteCommand::AddMediaFromPath { path });
                        }
                    }
                    _ => self.overlay = Overlay::PasteOffer { paths, text },
                }
                AddIntent::None
            }
            Overlay::ConfirmDiscard => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => self.leave(b),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => AddIntent::None,
                _ => {
                    self.overlay = Overlay::ConfirmDiscard;
                    AddIntent::None
                }
            },
            Overlay::ConfirmCloze => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => self.do_add(b),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => AddIntent::None,
                _ => {
                    self.overlay = Overlay::ConfirmCloze;
                    AddIntent::None
                }
            },
            Overlay::Conflict { conflicts, missing } => match key.code {
                KeyCode::Char('r') | KeyCode::Char('R') => self.reload_edit(b),
                KeyCode::Esc | KeyCode::Char('k') | KeyCode::Char('K') => AddIntent::None,
                _ => {
                    self.overlay = Overlay::Conflict { conflicts, missing };
                    AddIntent::None
                }
            },
        }
    }

    fn picked(&mut self, b: &dyn Backend, kind: PickerKind, id: i64) {
        match kind {
            PickerKind::Deck => {
                self.form.deck_id = id;
                self.set_status("");
                self.status = None;
            }
            PickerKind::Notetype => self.change_notetype(b, id),
        }
    }

    fn change_notetype(&mut self, b: &dyn Backend, id: i64) {
        if id == self.form.notetype.id {
            return;
        }
        let new = match fetch_notetype(b, id) {
            Ok(n) => n,
            Err(e) => return self.set_error(e),
        };
        if new.image_occlusion {
            return self.set_error(IO_REFUSED);
        }
        let old_names: Vec<String> = self
            .form
            .fields
            .iter()
            .map(|f| f.info.name.clone())
            .collect();
        let new_names: Vec<String> = new.fields.iter().map(|f| f.name.clone()).collect();
        let values = carry_fields(&old_names, &self.form.values(), &new_names);
        let tags = self.form.tags.text().to_owned();
        let focus = match self.form.focus {
            Focus::Field(i) => Focus::Field(i.min(new.fields.len().saturating_sub(1))),
            Focus::Tags => Focus::Tags,
        };
        let mut deck = self.form.deck_id;
        if let Ok(EditResponse::Deck(Some(d))) =
            run(b, NoteCommand::DefaultDeckForNotetype { notetype_id: id })
        {
            deck = d;
        }
        let mut form = Form::new(new, deck, values);
        form.baseline = vec![String::new(); form.fields.len()];
        form.tags = TextInput::new(tags);
        form.focus = focus;
        self.form = form;
        self.status = None;
        self.after_edit(b);
    }

    fn reopen_from_history(&mut self, b: &dyn Backend, note_id: i64) {
        match Self::load_edit_form(b, note_id) {
            Ok(form) => {
                let current = std::mem::replace(&mut self.form, form);
                if self.stash.is_none() {
                    self.stash = Some(current);
                }
                self.status = None;
                self.after_edit(b);
            }
            Err(_) => {
                self.set_error("That note no longer exists");
            }
        }
    }

    fn reload_edit(&mut self, b: &dyn Backend) -> AddIntent {
        let Some(id) = self.edited_note_id() else {
            return AddIntent::None;
        };
        match Self::load_edit_form(b, id) {
            Ok(form) => {
                self.form = form;
                self.set_status("Reloaded the note");
                self.after_edit(b);
                AddIntent::None
            }
            Err(_) => {
                self.set_error("The note was deleted");
                self.leave(b)
            }
        }
    }

    // ---- close, add, save ----------------------------------------------------------------

    /// Leaves the form: back to the stashed add form, or close.
    fn leave(&mut self, b: &dyn Backend) -> AddIntent {
        match self.stash.take() {
            Some(form) => {
                self.form = form;
                self.after_edit(b);
                AddIntent::None
            }
            None => AddIntent::Close,
        }
    }

    fn request_close(&mut self) -> AddIntent {
        if self.form.is_dirty() {
            self.overlay = Overlay::ConfirmDiscard;
            AddIntent::None
        } else {
            // `leave` needs a backend only to refresh; do it lazily.
            match self.stash.take() {
                Some(form) => {
                    self.form = form;
                    AddIntent::None
                }
                None => AddIntent::Close,
            }
        }
    }

    fn submit(&mut self, b: &dyn Backend) -> AddIntent {
        if self.is_edit() {
            return self.save_edit(b);
        }
        self.run_check(b);
        match self.form.check {
            FieldsCheck::Empty => self.set_error("The first field is empty."),
            FieldsCheck::NotetypeNotCloze => {
                self.set_error("Cloze deletions only work in a cloze note type.")
            }
            FieldsCheck::FieldNotCloze => {
                self.set_error("A cloze deletion is in a field the card does not use for clozes.")
            }
            FieldsCheck::MissingCloze => self.overlay = Overlay::ConfirmCloze,
            FieldsCheck::Normal | FieldsCheck::Duplicate => return self.do_add(b),
        }
        AddIntent::None
    }

    fn do_add(&mut self, b: &dyn Backend) -> AddIntent {
        let note = self.form.note_data();
        let deck_id = self.form.deck_id;
        let (id, changes) = match run(
            b,
            NoteCommand::AddNote {
                note: note.clone(),
                deck_id,
            },
        ) {
            Ok(EditResponse::NoteAdded { id, changes }) => (id, changes),
            Ok(other) => {
                self.set_error(format!("Unexpected response: {other:?}"));
                return AddIntent::None;
            }
            Err(e) => {
                self.set_error(e);
                return AddIntent::None;
            }
        };
        let joined = note
            .fields
            .iter()
            .filter(|f| !is_blank(f))
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let label = match run(
            b,
            NoteCommand::HtmlToText {
                html: joined.clone(),
                preserve_media_filenames: true,
            },
        ) {
            Ok(EditResponse::Text(t)) => t,
            _ => joined,
        };
        self.history.insert(
            0,
            HistoryEntry {
                note_id: id,
                label: truncate_label(&label),
            },
        );
        self.history.truncate(HISTORY_LIMIT);
        // New blank note: sticky fields and the tags stay.
        let values: Vec<String> = self
            .form
            .fields
            .iter()
            .map(|f| {
                if f.info.sticky {
                    f.area.value().to_owned()
                } else {
                    String::new()
                }
            })
            .collect();
        let tags = self.form.tags.text().to_owned();
        let mut form = Form::new(self.form.notetype.clone(), deck_id, values);
        form.tags = TextInput::new(tags);
        self.form = form;
        self.set_status("Added");
        self.after_edit(b);
        AddIntent::Changed(changes)
    }

    fn save_edit(&mut self, b: &dyn Backend) -> AddIntent {
        let Some(target) = &self.form.edit else {
            return AddIntent::None;
        };
        let values = self.form.values();
        let fields: Vec<(u32, String)> = values
            .iter()
            .enumerate()
            .filter(|(i, v)| target.fields.get(*i) != Some(*v))
            .map(|(i, v)| (i as u32, v.clone()))
            .collect();
        let tags = self.form.tag_list();
        let tags = tags_changed(&target.tags, &tags).then_some(tags);
        if fields.is_empty() && tags.is_none() {
            self.set_status("No changes");
            return self.leave(b);
        }
        let update = NoteUpdate {
            id: target.id,
            expected_mtime: target.mtime,
            expected_fields: Some(target.fields.clone()),
            fields,
            tags,
        };
        match run(
            b,
            NoteCommand::UpdateNotes {
                updates: vec![update],
            },
        ) {
            Ok(EditResponse::Updated(report)) => {
                if !report.conflicts.is_empty() || !report.missing.is_empty() {
                    self.overlay = Overlay::Conflict {
                        conflicts: report.conflicts,
                        missing: report.missing,
                    };
                    return AddIntent::None;
                }
                let changes = report.changes;
                self.set_status("Saved");
                match self.leave(b) {
                    AddIntent::Close => AddIntent::Saved(changes),
                    _ => AddIntent::Changed(changes),
                }
            }
            Ok(other) => {
                self.set_error(format!("Unexpected response: {other:?}"));
                AddIntent::None
            }
            Err(e) => {
                self.set_error(e);
                AddIntent::None
            }
        }
    }

    // ---- collection changes made elsewhere -----------------------------------------------

    /// Feed the broadcast `OpChanges` of mutations the widget did not make:
    /// refreshes note types and decks, and warns when the edited note
    /// changed (saving then reports the conflict).
    pub fn on_changes(&mut self, b: &dyn Backend, changes: &OpChanges) {
        if changes.notetype || changes.deck {
            if let Ok(n) = fetch_notetypes(b) {
                self.notetypes = n;
            }
            if !self.is_edit() {
                if let Ok(d) = fetch_decks(b) {
                    self.decks = d;
                }
            }
            if changes.notetype {
                if let Ok(nt) = fetch_notetype(b, self.form.notetype.id) {
                    let old: Vec<String> = self
                        .form
                        .fields
                        .iter()
                        .map(|f| f.info.name.clone())
                        .collect();
                    let new: Vec<String> = nt.fields.iter().map(|f| f.name.clone()).collect();
                    if old != new {
                        let values = carry_fields(&old, &self.form.values(), &new);
                        let focus = self.form.focus;
                        let tags = self.form.tags.text().to_owned();
                        let edit = self.form.edit.take();
                        let mut form = Form::new(nt, self.form.deck_id, values);
                        form.tags = TextInput::new(tags);
                        form.edit = edit;
                        form.baseline = vec![String::new(); form.fields.len()];
                        if let Focus::Field(i) = focus {
                            form.focus = Focus::Field(i.min(form.fields.len().saturating_sub(1)));
                        }
                        self.form = form;
                    } else {
                        for (f, info) in self.form.fields.iter_mut().zip(nt.fields.iter()) {
                            f.info = info.clone();
                        }
                        self.form.notetype = nt;
                    }
                    self.after_edit(b);
                }
            }
        }
        if changes.note || changes.note_text {
            if let Some(t) = &self.form.edit {
                let id = t.id;
                let mtime = t.mtime;
                match fetch_note(b, id) {
                    Ok(n) if n.mtime_secs != mtime => {
                        self.set_error(
                            "This note was changed elsewhere; saving will report a conflict",
                        );
                    }
                    Err(_) => self.set_error("This note was deleted elsewhere"),
                    _ => {}
                }
            }
        }
    }
}
