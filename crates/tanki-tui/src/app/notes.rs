//! Phase 3 in the app: the add / edit screen and the browser as layers over
//! the current screen (PLAN 8.4-8.7), their intents, and `$EDITOR` round
//! trips with the screen suspended by the injected [`super::EditorRunner`].
use super::{App, ConfirmKind, Layer, Overlay, Screen};
use crate::add_ui::{AddIntent, AddUi, ExternalEdit};
use crate::browser_ui::{BrowserIntent, BrowserOptions, BrowserUi, Focus as BrowserFocus};
use crate::editor_flow::{self, Outcome, Problem, TextOutcome};
use crate::keymap::{Action, Chord};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};
use tanki_core::{
    config::Config,
    notefile::{self, FileNote},
    OpChanges,
};

/// What the browser may have changed (its own commands are not broadcast
/// back to the app): enough for every screen to refresh what it shows.
const BROWSER_CHANGES: OpChanges = OpChanges {
    card: true,
    note: true,
    deck: true,
    tag: false,
    notetype: false,
    config: false,
    deck_config: false,
    mtime: false,
    browser_table: false,
    browser_sidebar: false,
    note_text: true,
    study_queues: false,
};

impl App<'_> {
    /// Replaces how the editor runs (the terminal loop suspends the screen
    /// around it; tests write the file instead).
    pub fn set_editor(&mut self, runner: super::EditorRunner) {
        self.editor = runner;
    }

    fn load_config(&self) -> Option<Config> {
        self.config_path
            .as_ref()
            .and_then(|path| Config::load(path).ok())
    }

    fn editor_command(&self) -> Vec<String> {
        self.load_config()
            .map(|c| c.editor)
            .unwrap_or_default()
            .resolve()
    }

    // ---- opening and closing -------------------------------------------------

    fn push_layer(&mut self, layer: Layer) {
        if self.layers.is_empty() {
            // `Overview.show()`-like: the reviewer's audio stops.
            self.services.audio.stop();
        }
        self.layers.push(layer);
        self.clear_images();
    }

    fn pop_layer(&mut self) {
        self.layers.pop();
        if self.layers.is_empty() {
            if let Screen::Review(view) = &mut self.screen {
                view.render = None;
            }
        }
        self.clear_images();
    }

    /// Add (desktop `a`): the defaults follow the reviewer's card.
    pub(crate) fn open_add(&mut self) {
        let home_deck = match &self.screen {
            Screen::Review(view) if self.layers.is_empty() => {
                view.reviewer.card().map_or(0, |c| c.queued.deck_id)
            }
            _ => 0,
        };
        match AddUi::new_add(&self.backend, home_deck, self.profile.media_dir.clone()) {
            Ok(mut ui) => {
                ui.set_night_mode(self.services.display.night_mode);
                self.push_layer(Layer::Add(Box::new(ui)));
            }
            Err(e) => self.error(e),
        }
    }

    /// Edit the reviewer's current note (desktop `e`).
    pub(crate) fn open_edit_current(&mut self) {
        let Screen::Review(view) = &self.screen else {
            return;
        };
        let Some(note_id) = view.reviewer.card().map(|c| c.queued.note_id) else {
            return;
        };
        match AddUi::new_edit(&self.backend, note_id, self.profile.media_dir.clone()) {
            Ok(mut ui) => {
                ui.set_night_mode(self.services.display.night_mode);
                self.push_layer(Layer::Add(Box::new(ui)));
            }
            Err(e) => self.error(e),
        }
    }

    /// The browser (desktop `b`) on `search`, or the default search.
    pub(crate) fn open_browser(&mut self, search: Option<String>) {
        let key = self.profile_key();
        let history = self
            .load_config()
            .and_then(|c| c.profile(&key).map(|p| p.search_history.clone()))
            .unwrap_or_default();
        let options = BrowserOptions {
            media_dir: self.profile.media_dir.clone(),
            night_mode: self.services.display.night_mode,
            history,
            search,
        };
        match BrowserUi::open(&self.backend, options) {
            Ok(ui) => self.push_layer(Layer::Browser(Box::new(ui))),
            Err(e) => self.error(format!("Could not open the browser: {e}")),
        }
    }

    /// Quits, asking first when an add / edit form holds unsaved input.
    pub(crate) fn request_quit(&mut self) {
        let unsaved = self.layers.iter().any(|layer| match layer {
            Layer::Add(ui) => ui.has_unsaved_input(),
            Layer::Browser(_) => false,
        });
        if unsaved {
            self.overlay = Some(Overlay::Confirm {
                message: "Discard the unsaved note and quit?".into(),
                kind: ConfirmKind::QuitDiscard,
                yes: false,
            });
        } else {
            self.quit = true;
        }
    }

    // ---- input -----------------------------------------------------------------

    /// A key while a layer is on top: its scope's bindings (quit, help) when
    /// the widget has no prompt open, everything else to the widget.
    pub(crate) fn layer_key(&mut self, key: KeyEvent, chord: Chord) {
        let Some(top) = self.layers.last() else {
            return;
        };
        let modal = match top {
            Layer::Add(ui) => ui.has_overlay(),
            Layer::Browser(ui) => ui.has_dialog(),
        };
        if let Some(action) = self.keymap.lookup(&[top.scope()], &chord) {
            if action == Action::Quit || !modal {
                self.dispatch(action);
                return;
            }
        }
        let backend = &self.backend;
        match self.layers.last_mut() {
            Some(Layer::Add(ui)) => {
                let intent = ui.on_key(backend, key);
                self.run_add_intent(intent);
            }
            Some(Layer::Browser(ui)) => {
                let intent = ui.on_key(backend, key);
                self.run_browser_intent(intent);
            }
            None => {}
        }
    }

    /// Bracketed paste over a layer: the add screen takes it as is; the
    /// browser's search bar and prompts as typed text (one line).
    pub(crate) fn layer_paste(&mut self, text: &str) {
        let backend = &self.backend;
        match self.layers.last_mut() {
            Some(Layer::Add(ui)) => {
                let intent = ui.on_paste(backend, text);
                self.run_add_intent(intent);
            }
            Some(Layer::Browser(ui)) => {
                if ui.focus() != BrowserFocus::Search && !ui.has_dialog() {
                    return;
                }
                for c in text.chars().filter(|c| !c.is_control()) {
                    let intent =
                        ui.on_key(backend, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
                    if intent != BrowserIntent::None {
                        self.run_browser_intent(intent);
                        return;
                    }
                }
            }
            None => {}
        }
    }

    // ---- intents -----------------------------------------------------------------

    pub(crate) fn run_add_intent(&mut self, intent: AddIntent) {
        match intent {
            AddIntent::None => {}
            AddIntent::Close => self.pop_layer(),
            AddIntent::Changed(changes) => {
                self.changes_below(&changes);
                self.load_totals();
            }
            AddIntent::Saved(changes) => {
                self.pop_layer();
                // The edited note may be the reviewer's card: show it again.
                let changes = OpChanges {
                    note_text: true,
                    ..changes
                };
                self.changes_below(&changes);
                self.load_totals();
                self.notice("Note saved.");
            }
            AddIntent::Message(text) => self.notice(text),
            AddIntent::OpenBrowser { note_id } => self.open_browser(Some(format!("nid:{note_id}"))),
            AddIntent::ExternalEdit(edit) => self.external_edit(edit),
        }
    }

    pub(crate) fn run_browser_intent(&mut self, intent: BrowserIntent) {
        match intent {
            BrowserIntent::None => {}
            BrowserIntent::Close => {
                self.pop_layer();
                self.changes_below(&BROWSER_CHANGES);
            }
            BrowserIntent::EditNotes(ids) => self.edit_notes(&ids),
            BrowserIntent::SearchCommitted(query) => self.remember_search(&query),
            BrowserIntent::CardInfo(_) => self.notice("Card info is not available yet (Phase 5)."),
            BrowserIntent::FilteredDeck(_) => {
                self.notice("Creating filtered decks is not available yet (Phase 5).")
            }
            BrowserIntent::Preview(_) => {
                self.notice("A full-screen preview is not available yet; p shows the preview pane.")
            }
        }
    }

    /// `ProfileConfig::push_search` and save, as desktop keeps its history.
    fn remember_search(&mut self, query: &str) {
        let Some(path) = self.config_path.clone() else {
            return;
        };
        let key = self.profile_key();
        let saved = Config::load(&path).and_then(|mut config| {
            config.profile_mut(&key).push_search(query);
            config.save(&path)
        });
        if let Err(e) = saved {
            self.error(format!("Could not save the search history: {e}"));
        }
    }

    // ---- $EDITOR -------------------------------------------------------------------

    /// The browser's edit: the notes in a note file, written back with
    /// `UpdateNotes` (and new blocks added).
    fn edit_notes(&mut self, ids: &[i64]) {
        if ids.is_empty() {
            return;
        }
        let command = self.editor_command();
        let options = self.flow_options.clone();
        let editor = &mut self.editor;
        let result = editor_flow::edit_notes(&self.backend, ids, &options, &mut |path: &Path| {
            editor(&command, path)
        });
        self.after_editor();
        match result {
            Ok(Outcome::Applied {
                updated,
                added,
                changes,
                ..
            }) => {
                let mut text = format!("Updated {}.", plural(updated.len(), "note"));
                if !added.is_empty() {
                    text = format!("{} Added {}.", text, plural(added.len(), "note"));
                }
                self.notice(text);
                if let Some(Layer::Browser(ui)) = self.layers.last_mut() {
                    ui.on_changes(&self.backend, &changes);
                }
                self.changes_below(&changes);
            }
            Ok(Outcome::Unchanged) => self.notice("No changes."),
            Ok(Outcome::Aborted { kept }) => self.notice(aborted(kept)),
            Ok(Outcome::Conflicts {
                conflicts,
                missing,
                kept,
            }) => {
                let mut parts = Vec::new();
                if !conflicts.is_empty() {
                    parts.push(format!("changed elsewhere: {}", id_list(&conflicts)));
                }
                if !missing.is_empty() {
                    parts.push(format!("deleted: {}", id_list(&missing)));
                }
                self.error(format!(
                    "Nothing was saved, notes {}. Your edits are kept in {}",
                    parts.join("; "),
                    kept.display()
                ));
            }
            Err(e) => self.error(e.to_string()),
        }
    }

    /// The add screen's field or whole note in `$EDITOR`; the result goes
    /// back into the form (nothing is written to the collection here).
    fn external_edit(&mut self, edit: ExternalEdit) {
        let command = self.editor_command();
        let options = self.flow_options.clone();
        let editor = &mut self.editor;
        let mut run = |path: &Path| editor(&command, path);
        match edit {
            ExternalEdit::Field { ord, html } => {
                let result = editor_flow::edit_text(
                    &html,
                    "field.html",
                    &options,
                    &mut |_| Vec::new(),
                    &mut run,
                );
                self.after_editor();
                match result {
                    Ok(TextOutcome::Edited(text)) => {
                        let text = without_final_newline(&html, text);
                        if let Some(Layer::Add(ui)) = self.layers.last_mut() {
                            ui.apply_external_field(&self.backend, ord, text);
                        }
                    }
                    Ok(TextOutcome::Unchanged) => {}
                    Ok(TextOutcome::Aborted { kept }) => self.notice(aborted(kept)),
                    Err(e) => self.error(e.to_string()),
                }
            }
            ExternalEdit::Note {
                note_id,
                notetype,
                deck,
                fields,
                tags,
            } => {
                let original = FileNote {
                    id: note_id,
                    mtime: None,
                    notetype,
                    deck: (note_id.is_none() && !deck.is_empty()).then_some(deck),
                    tags,
                    fields,
                };
                let text = notefile::write(std::slice::from_ref(&original));
                let result = editor_flow::edit_text(
                    &text,
                    "note.md",
                    &options,
                    &mut |t| note_problems(t, &original),
                    &mut run,
                );
                self.after_editor();
                match result {
                    Ok(TextOutcome::Edited(text)) => {
                        let Ok(mut notes) = notefile::parse(&text) else {
                            return;
                        };
                        let Some(edited) = notes.pop() else { return };
                        let values = original
                            .fields
                            .iter()
                            .map(|(name, _)| {
                                edited
                                    .fields
                                    .iter()
                                    .find(|(n, _)| n == name)
                                    .map(|(_, v)| v.clone())
                                    .unwrap_or_default()
                            })
                            .collect();
                        if let Some(Layer::Add(ui)) = self.layers.last_mut() {
                            ui.apply_external_note(&self.backend, values, edited.tags);
                        }
                    }
                    Ok(TextOutcome::Unchanged) => {}
                    Ok(TextOutcome::Aborted { kept }) => self.notice(aborted(kept)),
                    Err(e) => self.error(e.to_string()),
                }
            }
        }
    }

    /// The screen was handed to the editor: repaint every cell.
    fn after_editor(&mut self) {
        self.clear_images();
        self.full_redraw = true;
    }

    /// The help overlay's rows for the top layer: the widget's key table.
    pub(crate) fn layer_keys(&self) -> Option<Vec<(String, String)>> {
        Some(match self.layers.last()? {
            Layer::Add(_) => crate::add_ui::KEYS
                .iter()
                .map(|(k, d)| ((*k).to_owned(), (*d).to_owned()))
                .collect(),
            Layer::Browser(ui) => ui
                .hints()
                .into_iter()
                .map(|(k, d)| (k.to_owned(), d.to_owned()))
                .collect(),
        })
    }
}

/// What may change in the add form's note file: field text and tags.
fn note_problems(text: &str, original: &FileNote) -> Vec<Problem> {
    let problem = |line: usize, message: &str| {
        vec![Problem {
            line,
            message: message.to_owned(),
        }]
    };
    let mut notes = match notefile::parse_located(text) {
        Ok(notes) => notes,
        Err(e) => return problem(e.line, &e.message),
    };
    if notes.len() != 1 {
        return problem(1, "keep exactly one note in the file");
    }
    let (line, note) = notes.remove(0);
    if note.notetype != original.notetype {
        return problem(
            line,
            "the note type cannot be changed here; use Ctrl+N in the form",
        );
    }
    if note.id != original.id {
        return problem(line, "keep the id: line as it was");
    }
    if note.deck.is_some() && note.deck != original.deck {
        return problem(line, "change the deck with Ctrl+D in the form");
    }
    let names: Vec<&String> = original.fields.iter().map(|(n, _)| n).collect();
    let edited: Vec<&String> = note.fields.iter().map(|(n, _)| n).collect();
    let mut sorted_names = names.clone();
    let mut sorted_edited = edited.clone();
    sorted_names.sort();
    sorted_edited.sort();
    if sorted_names != sorted_edited {
        let list: Vec<&str> = names.iter().map(|n| n.as_str()).collect();
        return problem(
            line,
            &format!("keep the fields of the note type: {}", list.join(", ")),
        );
    }
    Vec::new()
}

/// Editors end files with a newline; a field that had none gets none.
fn without_final_newline(original: &str, mut text: String) -> String {
    if !original.ends_with('\n') {
        if text.ends_with("\r\n") {
            text.truncate(text.len() - 2);
        } else if text.ends_with('\n') {
            text.pop();
        }
    }
    text
}

fn aborted(kept: Option<PathBuf>) -> String {
    match kept {
        Some(path) => format!("Edit cancelled; your text is kept in {}", path.display()),
        None => "Edit cancelled.".into(),
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

fn id_list(ids: &[i64]) -> String {
    ids.iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}
