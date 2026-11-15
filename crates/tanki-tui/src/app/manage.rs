//! Phase 5 in the app: the management screens as layers over the current
//! screen (PLAN 7, 8.2–8.13), their intents, ticks for long operations and
//! `$EDITOR` round trips for scripts and templates.
use super::{App, DeckToolsLayer, Layer, Screen, TransferLayer};
use crate::deck_options_ui::{DeckOptionsIntent, DeckOptionsUi};
use crate::deck_tools_ui::{
    CustomStudyUi, DeckDescriptionUi, DeckPromptKind, DeckPromptUi, DeckToolsIntent, FilteredDeckUi,
};
use crate::editor_flow::{self, TextOutcome};
use crate::notetypes_ui::{ChangeIntent, ChangeNotetypeUi, NotetypesIntent, NotetypesUi};
use crate::stats_ui::{CardInfoUi, StatsIntent, StatsOptions, StatsUi};
use crate::tools_ui::{ToolsIntent, ToolsUi};
use crate::transfer_ui::{DuplicatesUi, ExportUi, ImportUi, TransferIntent};
use std::path::Path;
use tanki_core::{stats::StatsScope, transfer::TextExportSelection, OpChanges};

impl App<'_> {
    /// The deck the user is looking at: the deck list's selection, the
    /// overview's deck, or the reviewer card's deck.
    pub(crate) fn focused_deck(&self) -> Option<(i64, bool)> {
        match &self.screen {
            Screen::Decks(view) => view.rows.get(view.selected).map(|r| (r.id, r.filtered)),
            Screen::Overview(view) => view.data.as_ref().map(|d| (d.deck_id, d.filtered)),
            Screen::Review(view) => view.reviewer.card().map(|c| (c.queued.deck_id, false)),
            Screen::Congrats(_) => self.current_deck.map(|id| (id, false)),
        }
    }

    // ---- opening -------------------------------------------------------------

    /// Deck options (desktop `o`) for the focused deck.
    pub(crate) fn open_deck_options(&mut self) {
        let Some((deck_id, filtered)) = self.focused_deck() else {
            self.notice("Select a deck first.");
            return;
        };
        if filtered {
            self.open_filtered_deck_edit(deck_id);
            return;
        }
        match DeckOptionsUi::open(&self.backend, deck_id) {
            Ok(ui) => self.push_layer(Layer::DeckOptions(Box::new(ui))),
            Err(e) => self.error(format!("Could not open the deck options: {e}")),
        }
    }

    /// A filtered deck's options are its filtered-deck dialog (desktop).
    pub(crate) fn open_filtered_deck_edit(&mut self, deck_id: i64) {
        let result = FilteredDeckUi::edit(&self.backend, deck_id);
        self.push_dialog(result.map(DeckToolsLayer::Filtered));
    }

    /// Tools > Create Filtered Deck, or the browser's search as filter 1.
    pub(crate) fn open_filtered_deck_new(&mut self, search: Option<String>) {
        let result = match search {
            Some(search) => FilteredDeckUi::from_search(&self.backend, &search),
            None => FilteredDeckUi::new(&self.backend),
        };
        self.push_dialog(result.map(DeckToolsLayer::Filtered));
    }

    /// Custom study (overview `c`), for normal decks only.
    pub(crate) fn open_custom_study(&mut self) {
        match self.focused_deck() {
            Some((_, true)) => self.notice("Custom study is not available for filtered decks."),
            Some((deck_id, false)) => {
                let result = CustomStudyUi::open(&self.backend, deck_id);
                self.push_dialog(result.map(DeckToolsLayer::CustomStudy));
            }
            None => self.notice("Select a deck first."),
        }
    }

    /// The deck description (overview `i`).
    pub(crate) fn open_deck_description(&mut self) {
        let Some((deck_id, _)) = self.focused_deck() else {
            self.notice("Select a deck first.");
            return;
        };
        let result = DeckDescriptionUi::open(&self.backend, deck_id);
        self.push_dialog(result.map(DeckToolsLayer::Description));
    }

    /// Create (under the selected deck's parent path), rename, move or
    /// delete from the deck list.
    pub(crate) fn open_deck_prompt(&mut self, kind: DeckPromptKind) {
        let backend = &self.backend;
        let result = match (kind, self.focused_deck()) {
            (DeckPromptKind::Add, _) => DeckPromptUi::add(backend, ""),
            (_, None) => {
                self.notice("Select a deck first.");
                return;
            }
            (DeckPromptKind::Rename, Some((id, _))) => DeckPromptUi::rename(backend, id),
            (DeckPromptKind::Move, Some((id, _))) => DeckPromptUi::move_to(backend, id),
            (DeckPromptKind::Delete, Some((id, _))) => DeckPromptUi::delete(backend, id),
        };
        self.push_dialog(result.map(DeckToolsLayer::Prompt));
    }

    /// Overview `r` / `e` on a filtered deck.
    pub(crate) fn rebuild_or_empty(&mut self, rebuild: bool) {
        let Some((deck_id, true)) = self.focused_deck() else {
            self.notice("Only filtered decks can be rebuilt or emptied.");
            return;
        };
        let result = if rebuild {
            crate::deck_tools_ui::rebuild_filtered(&self.backend, deck_id)
        } else {
            crate::deck_tools_ui::empty_filtered(&self.backend, deck_id)
        };
        match result {
            Ok(changes) => {
                self.changes_below(&changes);
                self.load_totals();
                self.notice(if rebuild {
                    "Deck rebuilt."
                } else {
                    "Deck emptied."
                });
            }
            Err(e) => self.error(e.to_string()),
        }
    }

    /// Statistics (desktop `t`) for the focused deck, or the collection.
    pub(crate) fn open_stats(&mut self) {
        let options = match self.focused_deck() {
            Some((deck_id, _)) => StatsOptions {
                scope: StatsScope::DeckId(deck_id),
                deck_name: self.deck_name(deck_id),
                ..StatsOptions::default()
            },
            None => StatsOptions {
                scope: StatsScope::Collection,
                ..StatsOptions::default()
            },
        };
        match StatsUi::open(&self.backend, options) {
            Ok(ui) => self.push_layer(Layer::Stats(Box::new(ui))),
            Err(e) => self.error(format!("Could not load the statistics: {e}")),
        }
    }

    /// Card info (reviewer `i`, browser `i`) or Previous Card Info.
    pub(crate) fn open_card_info(&mut self, card_id: Option<i64>, previous: bool) {
        let Some(card_id) = card_id else {
            self.notice(if previous {
                "No previous card."
            } else {
                "No card."
            });
            return;
        };
        match CardInfoUi::open(&self.backend, card_id) {
            Ok(ui) => {
                let ui = if previous {
                    ui.with_title("Previous Card Info")
                } else {
                    ui
                };
                self.push_layer(Layer::CardInfo(Box::new(ui)));
            }
            Err(e) => self.error(format!("Could not load card info: {e}")),
        }
    }

    fn deck_name(&self, deck_id: i64) -> Option<String> {
        match &self.screen {
            Screen::Decks(view) => view
                .rows
                .iter()
                .find(|r| r.id == deck_id)
                .map(|r| r.full_name.clone()),
            Screen::Overview(view) => view.data.as_ref().map(|d| d.name.clone()),
            _ => None,
        }
    }

    pub(crate) fn run_stats_intent(&mut self, intent: StatsIntent) {
        match intent {
            StatsIntent::None => {}
            StatsIntent::Close => self.pop_layer(),
            StatsIntent::Browse(search) => self.open_browser(Some(search)),
        }
    }

    /// Tools: check database / media, empty cards, preferences, profiles.
    pub(crate) fn open_tools(&mut self) {
        let ui = ToolsUi::new(
            self.base_dir.clone(),
            Some(self.profile.name.clone()),
            self.config_path.clone(),
        );
        self.push_layer(Layer::Tools(Box::new(ui)));
    }

    pub(crate) fn run_tools_intent(&mut self, intent: ToolsIntent) {
        match intent {
            ToolsIntent::None => {}
            ToolsIntent::Close => self.pop_layer(),
            ToolsIntent::Changed(changes) => {
                self.changes_to_lower_layers(&changes);
                self.load_totals();
            }
            ToolsIntent::Message(text) => self.notice(text),
            ToolsIntent::SwitchProfile(name) => {
                // The caller closes this collection (joining its backup)
                // and opens the other profile.
                self.switch_to = Some(name);
                self.quit = true;
            }
        }
    }

    /// The profile the Tools screen asked to switch to, if any.
    pub fn take_profile_switch(&mut self) -> Option<String> {
        self.switch_to.take()
    }

    /// The Anki base folder holding the open profile.
    pub fn set_base_dir(&mut self, base: Option<std::path::PathBuf>) {
        self.base_dir = base;
    }

    /// The import prompt's CSV / TSV / text or JSON file (`.apkg` and
    /// `.colpkg` stay on the package flow). False when not one of these.
    pub(crate) fn open_text_import(&mut self, path: &Path) -> bool {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let result = match ext.as_str() {
            "csv" | "tsv" | "txt" => ImportUi::open_csv(&self.backend, path),
            "json" => ImportUi::open_json(&self.backend, path),
            _ => return false,
        };
        self.push_transfer(result.map(|ui| TransferLayer::Import(Box::new(ui))));
        true
    }

    /// The export prompt's `.txt` / `.tsv` / `.csv` file: notes or cards as
    /// text, of the overview's deck or the whole collection.
    pub(crate) fn open_text_export(&mut self, path: &Path) -> bool {
        let text = ["txt", "tsv", "csv"]
            .iter()
            .any(|e| path.extension().is_some_and(|x| x.eq_ignore_ascii_case(e)));
        if !text {
            return false;
        }
        let (label, selection) = match (&self.screen, self.current_deck) {
            (Screen::Overview(view), Some(id)) => (
                view.data
                    .as_ref()
                    .map_or("Current deck".to_owned(), |d| format!("Deck {}", d.name)),
                TextExportSelection::Deck(id),
            ),
            _ => ("Whole collection".to_owned(), TextExportSelection::All),
        };
        let ui = ExportUi::new(label, selection, &path.to_string_lossy());
        self.push_layer(Layer::Transfer(Box::new(TransferLayer::Export(ui))));
        true
    }

    /// The browser's rows as text.
    pub(crate) fn open_text_export_of(&mut self, notes: bool, ids: Vec<i64>) {
        let (label, selection) = if notes {
            (
                format!("{} selected note(s)", ids.len()),
                TextExportSelection::NoteIds(ids),
            )
        } else {
            (
                format!("{} selected card(s)", ids.len()),
                TextExportSelection::CardIds(ids),
            )
        };
        let ui = ExportUi::new(label, selection, "");
        self.push_layer(Layer::Transfer(Box::new(TransferLayer::Export(ui))));
    }

    /// Find duplicates within the browser's search.
    pub(crate) fn open_duplicates(&mut self, search: &str) {
        let result = DuplicatesUi::open(&self.backend, search);
        self.push_transfer(result.map(TransferLayer::Duplicates));
    }

    fn push_transfer(&mut self, result: Result<TransferLayer, tanki_core::CoreError>) {
        match result {
            Ok(layer) => self.push_layer(Layer::Transfer(Box::new(layer))),
            Err(e) => self.error(e.to_string()),
        }
    }

    pub(crate) fn run_transfer_intent(&mut self, intent: TransferIntent) {
        match intent {
            TransferIntent::None => {}
            TransferIntent::Close => self.pop_layer(),
            TransferIntent::Changed(changes) => {
                self.changes_to_lower_layers(&changes);
                self.load_totals();
            }
            TransferIntent::Message(text) => self.notice(text),
            TransferIntent::Done { changes, message } => {
                self.pop_layer();
                self.changes_to_lower_layers(&changes);
                self.load_totals();
                if let Some(text) = message {
                    self.notice(text);
                }
            }
            TransferIntent::BrowseSearch(search) => {
                // The widget stays open above the browser it came from;
                // search that browser, or open one.
                let below = self.layers.len().saturating_sub(2);
                let in_browser = matches!(self.layers.get(below), Some(Layer::Browser(_)))
                    && self.layers.len() >= 2;
                if in_browser {
                    let top = self.layers.pop();
                    if let Some(Layer::Browser(ui)) = self.layers.last_mut() {
                        ui.search(&self.backend, &search);
                    }
                    if let Some(top) = top {
                        self.layers.push(top);
                    }
                    self.notice(format!(
                        "The browser now shows {search}; Esc returns to it."
                    ));
                } else {
                    self.open_browser(Some(search));
                }
            }
        }
    }

    /// Desktop's "Get Shared": AnkiWeb's shared deck list in the web browser
    /// (a web page, not a sync endpoint); the downloaded file is imported
    /// with the import prompt.
    pub(crate) fn open_shared_decks(&mut self) {
        match (self.services.opener)(Path::new(SHARED_DECKS_URL)) {
            Ok(()) => self.notice(
                "Opened AnkiWeb's shared decks in the web browser; import a downloaded .apkg with Shift+I.",
            ),
            Err(e) => self.error(format!("Could not open {SHARED_DECKS_URL}: {e}")),
        }
    }

    fn push_dialog(&mut self, result: Result<DeckToolsLayer, tanki_core::CoreError>) {
        match result {
            Ok(dialog) => self.push_layer(Layer::DeckTools(Box::new(dialog))),
            Err(e) => self.error(e.to_string()),
        }
    }

    /// Manage note types (desktop Ctrl+Shift+N).
    pub(crate) fn open_notetypes(&mut self) {
        match NotetypesUi::new(&self.backend, self.profile.media_dir.clone()) {
            Ok(mut ui) => {
                ui.set_night_mode(self.services.display.night_mode);
                self.push_layer(Layer::Notetypes(Box::new(ui)));
            }
            Err(e) => self.error(format!("Could not open note types: {e}")),
        }
    }

    /// Change note type for the browser's selected notes.
    pub(crate) fn open_change_notetype(&mut self, note_ids: Vec<i64>) {
        if note_ids.is_empty() {
            return;
        }
        match ChangeNotetypeUi::new(&self.backend, note_ids) {
            Ok(ui) => self.push_layer(Layer::ChangeNotetype(Box::new(ui))),
            Err(e) => self.error(e),
        }
    }

    // ---- ticks -------------------------------------------------------------------

    /// Polls long operations that management layers started.
    pub(crate) fn tick_layers(&mut self) {
        let backend = &self.backend;
        match self.layers.last_mut() {
            Some(Layer::DeckOptions(ui)) if ui.is_busy() => {
                let intent = ui.tick(backend);
                self.run_deck_options_intent(intent);
            }
            Some(Layer::Tools(ui)) if ui.is_busy() => {
                let intent = ui.tick(backend);
                self.run_tools_intent(intent);
            }
            Some(Layer::Transfer(layer)) => {
                let intent = match &mut **layer {
                    TransferLayer::Import(ui) if ui.is_running() => ui.tick(backend),
                    TransferLayer::Export(ui) if ui.is_running() => ui.tick(backend),
                    _ => return,
                };
                self.run_transfer_intent(intent);
            }
            _ => {}
        }
    }

    // ---- intents -----------------------------------------------------------------

    pub(crate) fn run_deck_options_intent(&mut self, intent: DeckOptionsIntent) {
        match intent {
            DeckOptionsIntent::None => {}
            DeckOptionsIntent::Close => self.pop_layer(),
            DeckOptionsIntent::Saved(message) => {
                self.pop_layer();
                self.changes_below(&MANAGE_CHANGES);
                self.load_totals();
                self.notice(message);
            }
            DeckOptionsIntent::Message(text) => self.notice(text),
            DeckOptionsIntent::EditScript(script) => {
                if let Some(text) = self.edit_in_editor(&script, "custom-scheduling.js") {
                    if let Some(Layer::DeckOptions(ui)) = self.layers.last_mut() {
                        ui.set_custom_scheduling(text);
                    }
                }
            }
        }
    }

    pub(crate) fn run_notetypes_intent(&mut self, intent: NotetypesIntent) {
        match intent {
            NotetypesIntent::None => {}
            NotetypesIntent::Close => {
                self.pop_layer();
                self.changes_below(&NOTETYPE_CHANGES);
            }
            NotetypesIntent::Changed(changes) => self.changes_to_lower_layers(&changes),
            NotetypesIntent::Message(text) => self.notice(text),
            NotetypesIntent::ExternalEdit { text, suffix } => {
                if let Some(edited) = self.edit_in_editor(&text, &format!("template{suffix}")) {
                    let backend = &self.backend;
                    if let Some(Layer::Notetypes(ui)) = self.layers.last_mut() {
                        ui.apply_external(backend, edited);
                    }
                }
            }
        }
    }

    pub(crate) fn run_deck_tools_intent(&mut self, intent: DeckToolsIntent) {
        match intent {
            DeckToolsIntent::None => {}
            DeckToolsIntent::Close => self.pop_layer(),
            DeckToolsIntent::Changed(changes) => {
                self.changes_to_lower_layers(&changes);
                self.load_totals();
            }
            DeckToolsIntent::Message(text) => self.notice(text),
            DeckToolsIntent::Done { changes, message } => {
                self.pop_layer();
                self.changes_to_lower_layers(&changes);
                self.load_totals();
                if let Some(text) = message {
                    self.notice(text);
                }
            }
            DeckToolsIntent::OpenDeck { deck_id, changes } => {
                self.pop_layer();
                self.changes_to_lower_layers(&changes);
                self.load_totals();
                // Leave any browser below for the new deck's overview.
                while self.layers.last().is_some_and(|l| !l.has_unsaved_input()) {
                    self.pop_layer();
                }
                if self.layers.is_empty() {
                    self.open_overview(Some(deck_id));
                }
            }
        }
    }

    pub(crate) fn run_change_intent(&mut self, intent: ChangeIntent) {
        match intent {
            ChangeIntent::None => {}
            ChangeIntent::Close => self.pop_layer(),
            ChangeIntent::Changed { changes, notes } => {
                self.pop_layer();
                self.changes_to_lower_layers(&changes);
                self.notice(format!(
                    "Changed the note type of {notes} note{}.",
                    if notes == 1 { "" } else { "s" }
                ));
            }
        }
    }

    /// A layer's own command changed the collection (the actor does not
    /// broadcast it back to the app): refresh every layer below the top one
    /// and the screen.
    pub(crate) fn changes_to_lower_layers(&mut self, changes: &OpChanges) {
        let backend = &self.backend;
        let below = self.layers.len().saturating_sub(1);
        for layer in &mut self.layers[..below] {
            layer.on_changes(backend, changes);
        }
        self.changes_below(changes);
    }

    // ---- $EDITOR -------------------------------------------------------------------

    /// Runs `$EDITOR` on `text` in a temporary `file_name`; the edited text,
    /// or `None` when unchanged or cancelled (with a message).
    pub(crate) fn edit_in_editor(&mut self, text: &str, file_name: &str) -> Option<String> {
        self.services.audio.shutdown();
        let command = self.editor_command();
        let options = self.flow_options.clone();
        let editor = &mut self.editor;
        let result = editor_flow::edit_text(
            text,
            file_name,
            &options,
            &mut |_| Vec::new(),
            &mut |path: &Path| editor(&command, path),
        );
        self.after_editor();
        match result {
            Ok(TextOutcome::Edited(edited)) => {
                Some(super::notes::without_final_newline(text, edited))
            }
            Ok(TextOutcome::Unchanged) => None,
            Ok(TextOutcome::Aborted { kept }) => {
                self.notice(super::notes::aborted(kept));
                None
            }
            Err(e) => {
                self.error(e.to_string());
                None
            }
        }
    }
}

/// Desktop's Get Shared page (`aqt/deckbrowser.py`, `aqt.appShared + "decks/"`).
pub(crate) const SHARED_DECKS_URL: &str = "https://ankiweb.net/shared/decks/";

/// What a deck options save may have changed.
const MANAGE_CHANGES: OpChanges = OpChanges {
    card: true,
    note: false,
    deck: true,
    tag: false,
    notetype: false,
    config: true,
    deck_config: true,
    mtime: false,
    browser_table: true,
    browser_sidebar: false,
    note_text: false,
    study_queues: true,
};

/// What leaving the note types screen may have changed.
const NOTETYPE_CHANGES: OpChanges = OpChanges {
    card: true,
    note: true,
    deck: false,
    tag: false,
    notetype: true,
    config: false,
    deck_config: false,
    mtime: false,
    browser_table: true,
    browser_sidebar: true,
    note_text: true,
    study_queues: true,
};
