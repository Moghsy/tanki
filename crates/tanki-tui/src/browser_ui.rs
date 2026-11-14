//! The card browser (Phase 3, PLAN 8.7): search bar with completion and
//! history, a virtualised table with lazily fetched rows, cards / notes
//! modes, sorting, the column chooser, row colours, multi-select, the
//! sidebar tree with its search modifiers, a preview pane and the bulk
//! actions with their prompts.
//!
//! A self-contained widget (the TUI widget pattern of `sync_ui` /
//! `package_ui`). The app:
//! 1. builds it with [`BrowserUi::open`] (runs the initial search);
//! 2. before every draw calls [`BrowserUi::prepare`] with the area it will
//!    draw into: this fetches the rows that are visible (and a little
//!    overscan) through `BrowseCommand::Rows`, and the preview card;
//! 3. draws with [`BrowserUi::render`];
//! 4. sends keys to [`BrowserUi::on_key`] and carries out the returned
//!    [`BrowserIntent`];
//! 5. forwards `OpChanges` broadcast by the actor for changes made
//!    elsewhere (the editor, undo from another screen) to
//!    [`BrowserUi::on_changes`]. The widget already refreshes itself after
//!    its own commands; feeding those back too is harmless (one refetch).
//!
//! The search returns every id at once; rows are fetched only for the
//! visible window and cached, so 50k results stay responsive.
mod dialog;
mod sidebar;
#[cfg(test)]
pub(crate) mod tests;

use crate::backend::Backend;
use crate::completion::{Completion, CompletionOutcome, SearchVocabulary};
use crate::input::TextInput;
use crate::services::{self, CardRenderer};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use dialog::{
    Choice, ChoiceKind, Columns, Confirm, ConfirmAction, Dialog, FindReplaceForm, Forget, Outcome,
    Prompt, PromptKind, Reposition,
};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};
use sidebar::{ItemKind, LocalState};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use tanki_core::{
    BrowseCommand, BrowseMode, BrowserRow, BrowserSettings, BuryMode, ColumnInfo, Command,
    CoreError, EditResponse, Elide, FindReplace, FindReplaceScope, NoteCommand, OpChanges, Rating,
    RenderedCard, Response, RowColor, SearchJoiner, SearchOrder, SidebarData, SortDirection,
    Target,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// What only the app can do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserIntent {
    None,
    /// Leave the browser.
    Close,
    /// Edit these notes (the `$EDITOR` flow), then call `on_changes`.
    EditNotes(Vec<i64>),
    /// Show card info for this card.
    CardInfo(i64),
    /// Full-screen preview of this card.
    Preview(i64),
    /// Create a filtered deck from this search.
    FilteredDeck(String),
    /// A search ran: `ProfileConfig::push_search` it and save the config.
    SearchCommitted(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Table,
    Search,
    Sidebar,
}

/// How the browser starts.
#[derive(Debug, Clone, Default)]
pub struct BrowserOptions {
    /// `CollectionActor::media_dir`, for preview images.
    pub media_dir: PathBuf,
    pub night_mode: bool,
    /// `ProfileConfig::search_history`, newest first.
    pub history: Vec<String>,
    /// The initial search; when `None`, the collection's default search
    /// text (`BrowserSettings::default_search`), else `deck:current`.
    pub search: Option<String>,
}

/// Rows fetched beyond the visible window, both ways.
const OVERSCAN: usize = 20;
/// The row cache is trimmed to the window when it grows beyond this.
const CACHE_LIMIT: usize = 2000;
const SIDEBAR_WIDTH: u16 = 28;
const HISTORY_LIMIT: usize = 30;

struct PreviewCard {
    /// Table item (card or note id) it belongs to.
    item: i64,
    card: std::result::Result<(i64, RenderedCard), String>,
}

struct Areas {
    search: Rect,
    sidebar: Option<Rect>,
    table: Rect,
    preview: Option<Rect>,
    status: Rect,
    hints: Rect,
}

pub struct BrowserUi {
    media_dir: PathBuf,
    night_mode: bool,
    focus: Focus,
    search: TextInput,
    completion: Completion,
    vocab: SearchVocabulary,
    history_pos: Option<usize>,
    /// The search the table shows (normalised).
    query: String,
    settings: BrowserSettings,
    columns: Vec<ColumnInfo>,
    ids: Vec<i64>,
    rows: HashMap<i64, BrowserRow>,
    cursor: usize,
    scroll: usize,
    viewport: usize,
    selected: HashSet<i64>,
    /// Range selection: anchor index and the selection before it began.
    visual: Option<(usize, HashSet<i64>)>,
    sidebar_data: SidebarData,
    sidebar_local: LocalState,
    sidebar_rows: Vec<sidebar::Row>,
    sidebar_cursor: usize,
    sidebar_scroll: usize,
    sidebar_visible: bool,
    /// The next sidebar search is negated.
    negate: bool,
    preview_visible: bool,
    preview_answer: bool,
    preview_scroll: usize,
    preview: Option<PreviewCard>,
    preview_cache: RefCell<Option<(u64, tanki_render::RenderedCard)>>,
    dialog: Option<Dialog>,
    message: Option<String>,
}

type Result<T> = tanki_core::Result<T>;

fn browse(b: &dyn Backend, command: BrowseCommand) -> Result<EditResponse> {
    edit(b, Command::Browse(command))
}

fn edit(b: &dyn Backend, command: Command) -> Result<EditResponse> {
    match b.execute(command)? {
        Response::Edit(r) => Ok(r),
        other => Err(unexpected(&other)),
    }
}

fn unexpected(response: &Response) -> CoreError {
    let name = format!("{response:?}");
    let name = name.split(['(', ' ', '{']).next().unwrap_or("?").to_owned();
    CoreError::Other(format!("Unexpected response from the collection: {name}"))
}

fn browse_ids(b: &dyn Backend, command: BrowseCommand) -> Result<Vec<i64>> {
    match browse(b, command)? {
        EditResponse::Ids(ids) => Ok(ids),
        other => Err(CoreError::Other(format!("Unexpected answer: {other:?}"))),
    }
}

fn browse_text(b: &dyn Backend, command: BrowseCommand) -> Result<String> {
    match browse(b, command)? {
        EditResponse::Text(t) => Ok(t),
        other => Err(CoreError::Other(format!("Unexpected answer: {other:?}"))),
    }
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

/// Removes `[[type:…]]` placeholders, as desktop's preview does
/// (`type_ans_preview_filter`).
fn strip_type_answer(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find("[[type:") {
        let Some(end) = rest[start..].find("]]") else {
            break;
        };
        out.push_str(&rest[..start]);
        rest = &rest[start + end + 2..];
    }
    out.push_str(rest);
    out
}

/// Text fitted to `width` display columns with `elide`, padded.
fn fit(text: &str, width: usize, elide: Elide, centered: bool) -> String {
    let text: String = text.chars().filter(|c| !c.is_control()).collect();
    let w = text.width();
    let fitted = if w <= width {
        text
    } else if width == 0 {
        String::new()
    } else {
        let take = |iter: &mut dyn Iterator<Item = char>, budget: usize| {
            let mut out = String::new();
            let mut used = 0;
            for c in iter {
                let cw = c.width().unwrap_or(0);
                if used + cw > budget {
                    break;
                }
                used += cw;
                out.push(c);
            }
            out
        };
        match elide {
            Elide::None => take(&mut text.chars(), width),
            Elide::Right => format!("{}…", take(&mut text.chars(), width - 1)),
            Elide::Left => {
                let tail: String = take(&mut text.chars().rev(), width - 1)
                    .chars()
                    .rev()
                    .collect();
                format!("…{tail}")
            }
            Elide::Middle => {
                let head = (width - 1).div_ceil(2);
                let tail_len = width - 1 - head;
                let tail: String = take(&mut text.chars().rev(), tail_len)
                    .chars()
                    .rev()
                    .collect();
                format!("{}…{tail}", take(&mut text.chars(), head))
            }
        }
    };
    let pad = width.saturating_sub(fitted.width());
    if centered {
        format!(
            "{}{fitted}{}",
            " ".repeat(pad / 2),
            " ".repeat(pad - pad / 2)
        )
    } else {
        format!("{fitted}{}", " ".repeat(pad))
    }
}

/// Preferred width of a column; `None` = flexible (text columns).
fn column_width(key: &str) -> Option<usize> {
    Some(match key {
        "noteFld" | "question" | "answer" => return None,
        "cardDue" | "cardMod" | "noteMod" | "noteCrt" => 10,
        "cardIvl" => 10,
        "cardEase" | "cardLapses" | "cardReps" | "stability" | "difficulty" | "retrievability" => 7,
        "originalPosition" => 8,
        "template" => 12,
        "note" => 14,
        "deck" | "noteTags" => 16,
        _ => 12,
    })
}

/// Widths for the active columns in `total` columns (1 column gap).
fn column_widths(keys: &[String], total: usize) -> Vec<usize> {
    let gaps = keys.len().saturating_sub(1);
    let fixed: usize = keys.iter().filter_map(|k| column_width(k)).sum();
    let flex = keys.iter().filter(|k| column_width(k).is_none()).count();
    let free = total.saturating_sub(fixed + gaps);
    let each = free.checked_div(flex).unwrap_or(0);
    let mut extra = free.checked_rem(flex).unwrap_or(0);
    keys.iter()
        .map(|k| match column_width(k) {
            Some(w) => w,
            None => {
                let w = each + usize::from(extra > 0);
                extra = extra.saturating_sub(1);
                w.max(8)
            }
        })
        .collect()
}

fn row_style(color: RowColor) -> Style {
    let fg = match color {
        RowColor::Default => return Style::default(),
        RowColor::Flag(1) => Color::Red,
        RowColor::Flag(2) => Color::Rgb(255, 150, 0),
        RowColor::Flag(3) => Color::Green,
        RowColor::Flag(4) => Color::Blue,
        RowColor::Flag(5) => Color::Magenta,
        RowColor::Flag(6) => Color::Cyan,
        RowColor::Flag(_) => Color::Rgb(160, 90, 240),
        RowColor::Marked => Color::Rgb(130, 120, 255),
        RowColor::Suspended => Color::Yellow,
        RowColor::Buried => Color::Rgb(190, 120, 30),
    };
    Style::default().fg(fg)
}

/// A glyph per row state, so colour is not the only cue.
fn row_glyph(color: RowColor) -> char {
    match color {
        RowColor::Default => ' ',
        RowColor::Flag(_) => '⚑',
        RowColor::Marked => '★',
        RowColor::Suspended => '!',
        RowColor::Buried => '~',
    }
}

impl BrowserUi {
    /// Loads settings, columns and the sidebar, and runs the initial search.
    /// A failing initial search is shown as a message, not an error.
    pub fn open(backend: &dyn Backend, options: BrowserOptions) -> Result<Self> {
        let EditResponse::Settings(settings) = browse(backend, BrowseCommand::Settings)? else {
            return Err(CoreError::Other("Unexpected answer to Settings".into()));
        };
        let EditResponse::Columns(columns) = browse(backend, BrowseCommand::Columns)? else {
            return Err(CoreError::Other("Unexpected answer to Columns".into()));
        };
        let mut ui = Self {
            media_dir: options.media_dir,
            night_mode: options.night_mode,
            focus: Focus::Table,
            search: TextInput::default(),
            completion: Completion::new(true),
            vocab: SearchVocabulary {
                history: options.history,
                ..Default::default()
            },
            history_pos: None,
            query: String::new(),
            settings,
            columns,
            ids: Vec::new(),
            rows: HashMap::new(),
            cursor: 0,
            scroll: 0,
            viewport: 20,
            selected: HashSet::new(),
            visual: None,
            sidebar_data: SidebarData::default(),
            sidebar_local: LocalState::default(),
            sidebar_rows: Vec::new(),
            sidebar_cursor: 0,
            sidebar_scroll: 0,
            sidebar_visible: true,
            negate: false,
            preview_visible: false,
            preview_answer: false,
            preview_scroll: 0,
            preview: None,
            preview_cache: RefCell::new(None),
            dialog: None,
            message: None,
        };
        ui.reload_sidebar(backend)?;
        // Desktop's `_default_search`: the configured default search text,
        // else the current deck.
        let initial = options.search.unwrap_or_else(|| {
            let default = ui.settings.default_search.trim();
            if default.is_empty() {
                "deck:current".into()
            } else {
                default.to_owned()
            }
        });
        ui.search = TextInput::new(initial.clone());
        match browse_text(backend, BrowseCommand::NormalizeSearch { query: initial }) {
            Ok(query) => {
                ui.search = TextInput::new(query.clone());
                if let Err(error) = ui.run_search(backend, query, None) {
                    ui.message = Some(error.to_string());
                }
            }
            Err(error) => {
                ui.message = Some(error.to_string());
                ui.focus = Focus::Search;
            }
        }
        Ok(ui)
    }

    // ---- accessors -----------------------------------------------------------

    pub fn focus(&self) -> Focus {
        self.focus
    }
    pub fn mode(&self) -> BrowseMode {
        self.settings.mode
    }
    /// The search the table shows.
    pub fn query(&self) -> &str {
        &self.query
    }
    /// Ids of the table, in order.
    pub fn ids(&self) -> &[i64] {
        &self.ids
    }
    pub fn current(&self) -> Option<i64> {
        self.ids.get(self.cursor).copied()
    }
    /// The selected ids in table order, or the current row.
    pub fn targets(&self) -> Vec<i64> {
        if self.selected.is_empty() {
            self.current().into_iter().collect()
        } else {
            self.ids
                .iter()
                .filter(|id| self.selected.contains(id))
                .copied()
                .collect()
        }
    }
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }
    pub fn has_dialog(&self) -> bool {
        self.dialog.is_some()
    }
    /// Rows fetched so far (for tests and diagnostics).
    pub fn cached_rows(&self) -> usize {
        self.rows.len()
    }

    fn active_columns(&self) -> &[String] {
        match self.settings.mode {
            BrowseMode::Cards => &self.settings.card_columns,
            BrowseMode::Notes => &self.settings.note_columns,
        }
    }
    fn sort(&self) -> (&str, bool) {
        match self.settings.mode {
            BrowseMode::Cards => (&self.settings.card_sort, self.settings.card_sort_reverse),
            BrowseMode::Notes => (&self.settings.note_sort, self.settings.note_sort_reverse),
        }
    }
    fn column(&self, key: &str) -> Option<&ColumnInfo> {
        self.columns.iter().find(|c| c.key == key)
    }
    fn label<'a>(&self, column: &'a ColumnInfo) -> &'a str {
        match self.settings.mode {
            BrowseMode::Cards => &column.cards_label,
            BrowseMode::Notes => &column.notes_label,
        }
    }
    fn sortable(&self, column: &ColumnInfo) -> Option<SortDirection> {
        match self.settings.mode {
            BrowseMode::Cards => column.sort_cards,
            BrowseMode::Notes => column.sort_notes,
        }
    }
    /// Built-in order from the settings; only sortable columns.
    fn order(&self) -> SearchOrder {
        let (key, reverse) = self.sort();
        match self.column(key) {
            Some(c) if self.sortable(c).is_some() => SearchOrder::Column {
                key: key.to_owned(),
                reverse,
            },
            _ => SearchOrder::None,
        }
    }
    fn item_word(&self) -> &'static str {
        match self.settings.mode {
            BrowseMode::Cards => "card",
            BrowseMode::Notes => "note",
        }
    }

    // ---- data ----------------------------------------------------------------

    fn reload_sidebar(&mut self, backend: &dyn Backend) -> Result<()> {
        let EditResponse::Sidebar(data) = browse(backend, BrowseCommand::Sidebar)? else {
            return Err(CoreError::Other("Unexpected answer to Sidebar".into()));
        };
        self.sidebar_local.collapsed_sections = sidebar::Section::ALL
            .into_iter()
            .filter(|s| data.section_collapsed[s.index() as usize])
            .collect();
        self.sidebar_data = data;
        let mut decks = Vec::new();
        sidebar::deck_names(&self.sidebar_data.decks, &mut decks);
        let mut tags = Vec::new();
        sidebar::tag_names(&self.sidebar_data.tags, &mut tags);
        self.vocab.decks = decks;
        self.vocab.tags = tags;
        self.vocab.notetypes = self
            .sidebar_data
            .notetypes
            .iter()
            .map(|n| n.name.clone())
            .collect();
        let mut templates: Vec<String> = self
            .sidebar_data
            .notetypes
            .iter()
            .flat_map(|n| n.templates.iter().map(|t| t.name.clone()))
            .collect();
        templates.sort();
        templates.dedup();
        self.vocab.templates = templates;
        self.rebuild_sidebar_rows();
        Ok(())
    }

    fn rebuild_sidebar_rows(&mut self) {
        self.sidebar_rows = sidebar::rows(&self.sidebar_data, &self.sidebar_local);
        self.sidebar_cursor = self
            .sidebar_cursor
            .min(self.sidebar_rows.len().saturating_sub(1));
    }

    /// Runs `query` (already normalised); the cursor goes to `keep` when it
    /// is in the result, else to the first row.
    fn run_search(
        &mut self,
        backend: &dyn Backend,
        query: String,
        keep: Option<i64>,
    ) -> Result<()> {
        let ids = browse_ids(
            backend,
            BrowseCommand::Search {
                query: query.clone(),
                mode: self.settings.mode,
                order: self.order(),
            },
        )?;
        self.query = query;
        self.set_ids(ids, keep);
        Ok(())
    }

    fn set_ids(&mut self, ids: Vec<i64>, keep: Option<i64>) {
        self.ids = ids;
        self.rows.clear();
        self.selected.clear();
        self.visual = None;
        self.preview = None;
        self.cursor = keep
            .and_then(|id| self.ids.iter().position(|&i| i == id))
            .unwrap_or(0);
        self.scroll = self.cursor.saturating_sub(self.viewport / 2);
        self.clamp_scroll();
    }

    /// Re-runs the current search after rows were removed, keeping the
    /// cursor near where it was (desktop's `to_row_of_unselected_note`).
    fn research(&mut self, backend: &dyn Backend) -> Result<()> {
        let index = self.cursor;
        let keep = self.current();
        let query = self.query.clone();
        self.run_search(backend, query, keep)?;
        if keep.is_some() && self.current() != keep {
            self.cursor = index.min(self.ids.len().saturating_sub(1));
            self.clamp_scroll();
        }
        Ok(())
    }

    fn clamp_scroll(&mut self) {
        let view = self.viewport.max(1);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + view {
            self.scroll = self.cursor + 1 - view;
        }
        self.scroll = self.scroll.min(self.ids.len().saturating_sub(view));
    }

    /// Fetches the visible rows (plus overscan) not yet cached.
    fn fetch_rows(&mut self, backend: &dyn Backend) {
        if self.ids.is_empty() {
            return;
        }
        let start = self.scroll.saturating_sub(OVERSCAN);
        let end = (self.scroll + self.viewport + OVERSCAN).min(self.ids.len());
        if self.rows.len() > CACHE_LIMIT {
            let window: HashSet<i64> = self.ids[start..end].iter().copied().collect();
            self.rows.retain(|id, _| window.contains(id));
        }
        let missing: Vec<i64> = self.ids[start..end]
            .iter()
            .filter(|id| !self.rows.contains_key(id))
            .copied()
            .collect();
        if missing.is_empty() {
            return;
        }
        match browse(
            backend,
            BrowseCommand::Rows {
                mode: self.settings.mode,
                columns: self.active_columns().to_vec(),
                ids: missing,
            },
        ) {
            Ok(EditResponse::Rows(rows)) => {
                for row in rows {
                    self.rows.insert(row.id, row);
                }
            }
            Ok(_) => {}
            Err(error) => self.message = Some(error.to_string()),
        }
    }

    fn fetch_preview(&mut self, backend: &dyn Backend) {
        let Some(item) = self.current() else {
            self.preview = None;
            return;
        };
        if !self.preview_visible || self.preview.as_ref().is_some_and(|p| p.item == item) {
            return;
        }
        self.preview_scroll = 0;
        let card = (|| -> Result<(i64, RenderedCard)> {
            let card_id = match self.settings.mode {
                BrowseMode::Cards => item,
                BrowseMode::Notes => *browse_ids(
                    backend,
                    BrowseCommand::CardsOfNotes {
                        note_ids: vec![item],
                    },
                )?
                .first()
                .ok_or_else(|| CoreError::Invalid("This note has no cards".into()))?,
            };
            Ok((card_id, crate::backend::render(backend, card_id)?))
        })()
        .map_err(|e| e.to_string());
        self.preview = Some(PreviewCard { item, card });
    }

    /// Call before every draw with the area `render` will get: fetches the
    /// visible rows and the preview card.
    pub fn prepare(&mut self, backend: &dyn Backend, area: Rect) {
        let areas = self.layout(area);
        self.viewport = areas.table.height.saturating_sub(1).max(1) as usize;
        self.clamp_scroll();
        self.fetch_rows(backend);
        self.fetch_preview(backend);
    }

    /// Changes made elsewhere (or by this widget): drops stale rows and the
    /// preview, reloads the sidebar when it may have changed.
    pub fn on_changes(&mut self, backend: &dyn Backend, changes: &OpChanges) {
        if changes.browser_table || changes.note_text || changes.card || changes.note {
            self.rows.clear();
            self.preview = None;
            self.preview_cache.replace(None);
        }
        if changes.browser_sidebar || changes.deck || changes.tag || changes.notetype {
            if let Err(error) = self.reload_sidebar(backend) {
                self.message = Some(error.to_string());
            }
        }
    }

    /// Applies the result of a mutation run by this widget.
    fn done(&mut self, backend: &dyn Backend, result: Result<EditOrChanges>, message: String) {
        match result {
            Ok(changes) => {
                self.on_changes(backend, &changes.0);
                self.message = Some(message);
            }
            Err(error) => self.message = Some(error.to_string()),
        }
    }

    // ---- keys ----------------------------------------------------------------

    pub fn on_key(&mut self, backend: &dyn Backend, key: KeyEvent) -> BrowserIntent {
        if self.dialog.is_some() {
            return self.dialog_key(backend, &key);
        }
        if !matches!(self.focus, Focus::Search) {
            self.message = None;
        }
        match self.focus {
            Focus::Search => self.search_key(backend, &key),
            Focus::Sidebar => self.sidebar_key(backend, &key),
            Focus::Table => self.table_key(backend, &key),
        }
    }

    fn focus_search(&mut self) {
        self.focus = Focus::Search;
        self.history_pos = None;
        self.completion.close();
    }

    fn search_key(&mut self, backend: &dyn Backend, key: &KeyEvent) -> BrowserIntent {
        let outcome = self
            .completion
            .handle_key(key, &mut self.search, &mut &self.vocab);
        if outcome != CompletionOutcome::Ignored {
            if outcome == CompletionOutcome::Edited {
                self.message = None;
            }
            return BrowserIntent::None;
        }
        match key.code {
            KeyCode::Enter => self.commit_search(backend),
            KeyCode::Esc => {
                self.search = TextInput::new(self.query.clone());
                self.message = None;
                self.focus = Focus::Table;
                BrowserIntent::None
            }
            KeyCode::Up | KeyCode::Down => {
                let history = &self.vocab.history;
                if history.is_empty() {
                    return BrowserIntent::None;
                }
                let up = key.code == KeyCode::Up;
                let next = match (self.history_pos, up) {
                    (None, true) => Some(0),
                    (Some(i), true) => Some((i + 1).min(history.len() - 1)),
                    (None | Some(0), false) => None,
                    (Some(i), false) => Some(i - 1),
                };
                self.history_pos = next;
                self.search = TextInput::new(match next {
                    Some(i) => history[i].clone(),
                    None => self.query.clone(),
                });
                BrowserIntent::None
            }
            _ => BrowserIntent::None,
        }
    }

    fn remember(&mut self, query: &str) {
        let history = &mut self.vocab.history;
        history.retain(|h| h != query);
        history.insert(0, query.to_owned());
        history.truncate(HISTORY_LIMIT);
    }

    /// Normalises and runs the search bar's text (desktop's `_on_search`).
    fn commit_search(&mut self, backend: &dyn Backend) -> BrowserIntent {
        let text = self.search.text().to_owned();
        match browse_text(backend, BrowseCommand::NormalizeSearch { query: text }) {
            Ok(query) => self.apply_search(backend, query, Focus::Table),
            Err(error) => {
                self.message = Some(error.to_string());
                BrowserIntent::None
            }
        }
    }

    fn apply_search(
        &mut self,
        backend: &dyn Backend,
        query: String,
        focus: Focus,
    ) -> BrowserIntent {
        self.search = TextInput::new(query.clone());
        self.completion.close();
        match self.run_search(backend, query.clone(), None) {
            Ok(()) => {
                self.focus = focus;
                self.remember(&query);
                BrowserIntent::SearchCommitted(query)
            }
            Err(error) => {
                self.message = Some(error.to_string());
                BrowserIntent::None
            }
        }
    }

    fn move_to(&mut self, index: usize) {
        if self.ids.is_empty() {
            return;
        }
        self.cursor = index.min(self.ids.len() - 1);
        self.clamp_scroll();
        if let Some((anchor, base)) = &self.visual {
            let (a, b) = (*anchor.min(&self.cursor), *anchor.max(&self.cursor));
            let mut selected = base.clone();
            selected.extend(self.ids[a..=b].iter().copied());
            self.selected = selected;
        }
    }

    fn table_key(&mut self, backend: &dyn Backend, key: &KeyEvent) -> BrowserIntent {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.viewport.max(1);
        match key.code {
            KeyCode::Char('q') if !ctrl => return BrowserIntent::Close,
            KeyCode::Esc => {
                if self.visual.take().is_some() {
                } else if !self.selected.is_empty() {
                    self.selected.clear();
                } else {
                    return BrowserIntent::Close;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_to(self.cursor + 1),
            KeyCode::Up | KeyCode::Char('k') => self.move_to(self.cursor.saturating_sub(1)),
            KeyCode::PageDown => self.move_to(self.cursor + page),
            KeyCode::Char('d') if ctrl => self.move_to(self.cursor + page / 2),
            KeyCode::PageUp => self.move_to(self.cursor.saturating_sub(page)),
            KeyCode::Char('u') if ctrl => self.move_to(self.cursor.saturating_sub(page / 2)),
            KeyCode::Home | KeyCode::Char('g') => self.move_to(0),
            KeyCode::End | KeyCode::Char('G') => self.move_to(usize::MAX),
            KeyCode::Char(' ') => {
                if let Some(id) = self.current() {
                    self.visual = None;
                    if !self.selected.remove(&id) {
                        self.selected.insert(id);
                    }
                    self.move_to(self.cursor + 1);
                }
            }
            KeyCode::Char('v') => {
                if self.visual.take().is_none() && !self.ids.is_empty() {
                    self.visual = Some((self.cursor, self.selected.clone()));
                    self.move_to(self.cursor);
                }
            }
            KeyCode::Char('a') if ctrl => {
                self.visual = None;
                self.selected = self.ids.iter().copied().collect();
            }
            KeyCode::Char('*') => {
                self.visual = None;
                self.selected = self
                    .ids
                    .iter()
                    .filter(|id| !self.selected.contains(id))
                    .copied()
                    .collect();
            }
            KeyCode::Char('/') => self.focus_search(),
            KeyCode::Tab => {
                self.sidebar_visible = true;
                self.focus = Focus::Sidebar;
            }
            KeyCode::Char('b') if ctrl => self.sidebar_visible = !self.sidebar_visible,
            KeyCode::Char('r') if ctrl => self.undo(backend, true),
            KeyCode::Char('u') => self.undo(backend, false),
            KeyCode::Char('M') => self.toggle_mode(backend),
            KeyCode::Char('o') => self.open_columns(),
            KeyCode::Char('S') => self.open_sort(),
            KeyCode::Char('p') => {
                self.preview_visible = !self.preview_visible;
                self.preview_cache.replace(None);
            }
            KeyCode::Char('P') => {
                self.preview_answer = !self.preview_answer;
                self.preview_visible = true;
                self.preview_scroll = 0;
            }
            KeyCode::Char('}') => self.preview_scroll += 1,
            KeyCode::Char('{') => self.preview_scroll = self.preview_scroll.saturating_sub(1),
            KeyCode::Enter | KeyCode::Char('e') => {
                return match self.note_ids(backend) {
                    Ok(ids) if !ids.is_empty() => BrowserIntent::EditNotes(ids),
                    Ok(_) => BrowserIntent::None,
                    Err(error) => {
                        self.message = Some(error.to_string());
                        BrowserIntent::None
                    }
                }
            }
            KeyCode::Char('i') => {
                return match self.current_card(backend) {
                    Ok(Some(id)) => BrowserIntent::CardInfo(id),
                    Ok(None) => BrowserIntent::None,
                    Err(error) => {
                        self.message = Some(error.to_string());
                        BrowserIntent::None
                    }
                }
            }
            KeyCode::Char('V') => {
                return match self.current_card(backend) {
                    Ok(Some(id)) => BrowserIntent::Preview(id),
                    _ => BrowserIntent::None,
                }
            }
            KeyCode::Char('F') => return BrowserIntent::FilteredDeck(self.query.clone()),
            KeyCode::Char('w') => self.open_save_search(),
            KeyCode::Char(c @ '0'..='7') if !ctrl => self.flag(backend, c as u8 - b'0'),
            KeyCode::Char('m') => self.toggle_mark(backend),
            KeyCode::Char('s') => self.toggle_state(backend, BuryMode::Suspend),
            KeyCode::Char('b') => self.toggle_state(backend, BuryMode::Bury),
            KeyCode::Char('c') => self.open_change_deck(),
            KeyCode::Char('t') => self.open_tags(PromptKind::AddTags),
            KeyCode::Char('T') => self.open_tags(PromptKind::RemoveTags),
            KeyCode::Char('U') => {
                let result = edit(backend, Command::Notes(NoteCommand::ClearUnusedTags));
                let message = match &result {
                    Ok(EditResponse::Count { count, .. }) => {
                        format!("Removed {}", plural(*count, "unused tag"))
                    }
                    _ => String::new(),
                };
                self.done(backend, result.map(EditOrChanges::from), message);
            }
            KeyCode::Char('f') => self.open_forget(backend),
            KeyCode::Char('d') => self.open_set_due(backend),
            KeyCode::Char('r') => self.open_reposition(backend),
            KeyCode::Char('n') => self.open_grade_now(),
            KeyCode::Char('R') => self.open_find_replace(backend),
            KeyCode::Char('D') | KeyCode::Delete => self.open_delete(backend),
            _ => {}
        }
        BrowserIntent::None
    }

    // ---- target conversions --------------------------------------------------

    fn card_ids(&self, backend: &dyn Backend) -> Result<Vec<i64>> {
        let targets = self.targets();
        match self.settings.mode {
            BrowseMode::Cards => Ok(targets),
            BrowseMode::Notes if targets.is_empty() => Ok(targets),
            BrowseMode::Notes => {
                browse_ids(backend, BrowseCommand::CardsOfNotes { note_ids: targets })
            }
        }
    }
    fn note_ids(&self, backend: &dyn Backend) -> Result<Vec<i64>> {
        let targets = self.targets();
        match self.settings.mode {
            BrowseMode::Notes => Ok(targets),
            BrowseMode::Cards if targets.is_empty() => Ok(targets),
            BrowseMode::Cards => {
                browse_ids(backend, BrowseCommand::NotesOfCards { card_ids: targets })
            }
        }
    }
    fn current_card(&self, backend: &dyn Backend) -> Result<Option<i64>> {
        let Some(item) = self.current() else {
            return Ok(None);
        };
        Ok(match self.settings.mode {
            BrowseMode::Cards => Some(item),
            BrowseMode::Notes => browse_ids(
                backend,
                BrowseCommand::CardsOfNotes {
                    note_ids: vec![item],
                },
            )?
            .first()
            .copied(),
        })
    }
    /// `cid:N` / `nid:N` for the current row.
    fn current_filter(&self) -> Option<String> {
        let item = self.current()?;
        Some(match self.settings.mode {
            BrowseMode::Cards => format!("cid:{item}"),
            BrowseMode::Notes => format!("nid:{item}"),
        })
    }

    /// Runs `f` with the card ids, or says nothing is selected.
    fn with_cards(&mut self, backend: &dyn Backend) -> Option<Vec<i64>> {
        match self.card_ids(backend) {
            Ok(ids) if !ids.is_empty() => Some(ids),
            Ok(_) => {
                self.message = Some("Nothing selected".into());
                None
            }
            Err(error) => {
                self.message = Some(error.to_string());
                None
            }
        }
    }
    fn with_notes(&mut self, backend: &dyn Backend) -> Option<Vec<i64>> {
        match self.note_ids(backend) {
            Ok(ids) if !ids.is_empty() => Some(ids),
            Ok(_) => {
                self.message = Some("Nothing selected".into());
                None
            }
            Err(error) => {
                self.message = Some(error.to_string());
                None
            }
        }
    }

    // ---- actions ---------------------------------------------------------------

    fn changes(backend: &dyn Backend, command: Command) -> Result<EditOrChanges> {
        let response = backend.execute(command)?;
        response
            .changes()
            .cloned()
            .map(EditOrChanges)
            .ok_or_else(|| unexpected(&response))
    }

    fn undo(&mut self, backend: &dyn Backend, redo: bool) {
        let command = if redo { Command::Redo } else { Command::Undo };
        match backend.execute(command) {
            Ok(Response::Undone { changes, label }) => {
                self.on_changes(backend, &changes);
                if let Err(error) = self.research(backend) {
                    self.message = Some(error.to_string());
                    return;
                }
                let verb = if redo { "Redone" } else { "Undone" };
                self.message = Some(format!("{verb}: {label}"));
            }
            Ok(other) => self.message = Some(unexpected(&other).to_string()),
            Err(error) => self.message = Some(error.to_string()),
        }
    }

    /// Desktop's `set_flag_of_selected_cards`: pressing the current card's
    /// own flag clears it.
    fn flag(&mut self, backend: &dyn Backend, flag: u8) {
        let Some(card_ids) = self.with_cards(backend) else {
            return;
        };
        let current = self
            .current()
            .and_then(|id| self.rows.get(&id))
            .map(|r| r.color);
        let flag =
            if self.settings.mode == BrowseMode::Cards && current == Some(RowColor::Flag(flag)) {
                0
            } else {
                flag
            };
        let n = card_ids.len();
        let result = Self::changes(backend, Command::SetFlag { card_ids, flag });
        let message = if flag == 0 {
            format!("Flag cleared on {}", plural(n, "card"))
        } else {
            format!("Flag {flag} set on {}", plural(n, "card"))
        };
        self.done(backend, result, message);
    }

    /// True when the search `filter` plus `extra` matches any card.
    fn matches(&self, backend: &dyn Backend, extra: &str) -> Result<bool> {
        let Some(filter) = self.current_filter() else {
            return Ok(false);
        };
        Ok(!browse_ids(
            backend,
            BrowseCommand::Search {
                query: format!("{filter} {extra}"),
                mode: BrowseMode::Cards,
                order: SearchOrder::None,
            },
        )?
        .is_empty())
    }

    /// Desktop's toggle mark: the current note's state decides.
    fn toggle_mark(&mut self, backend: &dyn Backend) {
        let Some(note_ids) = self.with_notes(backend) else {
            return;
        };
        let marked = match self.matches(backend, "tag:marked") {
            Ok(m) => !m,
            Err(error) => {
                self.message = Some(error.to_string());
                return;
            }
        };
        let n = note_ids.len();
        let result = Self::changes(backend, Command::SetMarked { note_ids, marked });
        let verb = if marked { "Marked" } else { "Unmarked" };
        self.done(backend, result, format!("{verb} {}", plural(n, "note")));
    }

    /// Suspend / bury toggle: the current card's state decides (desktop's
    /// `suspend_selected_cards` / `bury_selected_cards`).
    fn toggle_state(&mut self, backend: &dyn Backend, mode: BuryMode) {
        let Some(card_ids) = self.with_cards(backend) else {
            return;
        };
        let (term, done, undone) = match mode {
            BuryMode::Suspend => ("is:suspended", "Suspended", "Unsuspended"),
            BuryMode::Bury => ("is:buried", "Buried", "Unburied"),
        };
        let n = card_ids.len();
        let (result, verb) = match self.matches(backend, term) {
            Ok(true) => (
                browse(backend, BrowseCommand::Restore { card_ids }).map(EditOrChanges::from),
                undone,
            ),
            Ok(false) => (
                Self::changes(
                    backend,
                    Command::BuryOrSuspend {
                        target: Target::Cards(card_ids),
                        mode,
                    },
                ),
                done,
            ),
            Err(error) => (Err(error), ""),
        };
        self.done(backend, result, format!("{verb} {}", plural(n, "card")));
    }

    fn toggle_mode(&mut self, backend: &dyn Backend) {
        let to_notes = self.settings.mode == BrowseMode::Cards;
        let convert = |ids: Vec<i64>| -> Result<Vec<i64>> {
            if ids.is_empty() {
                return Ok(ids);
            }
            browse_ids(
                backend,
                if to_notes {
                    BrowseCommand::NotesOfCards { card_ids: ids }
                } else {
                    BrowseCommand::CardsOfNotes { note_ids: ids }
                },
            )
        };
        let result = (|| -> Result<()> {
            let keep = convert(self.current().into_iter().collect())?
                .first()
                .copied();
            let selected = convert(self.targets_if_selected())?;
            let mut settings = self.settings.clone();
            settings.mode = if to_notes {
                BrowseMode::Notes
            } else {
                BrowseMode::Cards
            };
            browse(backend, BrowseCommand::SetSettings(settings.clone()))?;
            self.settings = settings;
            let query = self.query.clone();
            self.run_search(backend, query, keep)?;
            self.selected = selected.into_iter().collect();
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.message = Some(format!("Showing {}s", self.item_word()));
            }
            Err(error) => self.message = Some(error.to_string()),
        }
    }

    fn targets_if_selected(&self) -> Vec<i64> {
        if self.selected.is_empty() {
            Vec::new()
        } else {
            self.targets()
        }
    }

    fn save_settings(&mut self, backend: &dyn Backend, settings: BrowserSettings) -> Result<()> {
        browse(backend, BrowseCommand::SetSettings(settings.clone()))?;
        self.settings = settings;
        Ok(())
    }

    // ---- dialogs -------------------------------------------------------------

    fn open_columns(&mut self) {
        let active = self.active_columns();
        let mut items: Vec<(String, String, bool)> = self
            .columns
            .iter()
            .map(|c| {
                (
                    c.key.clone(),
                    self.label(c).to_owned(),
                    active.contains(&c.key),
                )
            })
            .collect();
        items.sort_by(|a, b| a.1.cmp(&b.1));
        self.dialog = Some(Dialog::Columns(Columns { items, cursor: 0 }));
    }

    fn open_sort(&mut self) {
        let (current, reverse) = self.sort();
        let mut cursor = 0;
        let options = self
            .active_columns()
            .iter()
            .enumerate()
            .map(|(i, key)| {
                let column = self.column(key);
                let label = column.map_or(key.as_str(), |c| self.label(c)).to_owned();
                if key == current {
                    cursor = i;
                    format!("{label} {}", if reverse { "▼" } else { "▲" })
                } else if column.and_then(|c| self.sortable(c)).is_none() {
                    format!("{label} (cannot sort)")
                } else {
                    label
                }
            })
            .collect();
        self.dialog = Some(Dialog::Choice(Choice {
            kind: ChoiceKind::Sort,
            title: "Sort by".into(),
            options,
            cursor,
        }));
    }

    fn open_grade_now(&mut self) {
        self.dialog = Some(Dialog::Choice(Choice {
            kind: ChoiceKind::GradeNow,
            title: "Grade now".into(),
            options: ["Again", "Hard", "Good", "Easy"].map(String::from).to_vec(),
            cursor: 2,
        }));
    }

    fn open_save_search(&mut self) {
        self.dialog = Some(Dialog::Prompt(Prompt::new(
            PromptKind::SaveSearch,
            "Save current search",
            &self.query,
            "",
        )));
    }

    fn open_change_deck(&mut self) {
        self.dialog = Some(Dialog::Prompt(
            Prompt::new(
                PromptKind::ChangeDeck,
                "Change deck",
                "A new name creates the deck",
                "",
            )
            .with_items(self.vocab.decks.clone(), true),
        ));
    }

    fn open_tags(&mut self, kind: PromptKind) {
        let title = if kind == PromptKind::AddTags {
            "Add tags"
        } else {
            "Remove tags"
        };
        self.dialog = Some(Dialog::Prompt(
            Prompt::new(kind, title, "Separate tags with spaces", "")
                .with_items(self.vocab.tags.clone(), false),
        ));
    }

    fn open_forget(&mut self, backend: &dyn Backend) {
        let Some(cards) = self.with_cards(backend) else {
            return;
        };
        match browse(backend, BrowseCommand::ForgetDefaults) {
            Ok(EditResponse::ForgetDefaults(d)) => {
                self.dialog = Some(Dialog::Forget(Forget {
                    restore_position: d.restore_position,
                    reset_counts: d.reset_counts,
                    cursor: 0,
                    count: cards.len(),
                }))
            }
            Ok(_) => {}
            Err(error) => self.message = Some(error.to_string()),
        }
    }

    fn open_set_due(&mut self, backend: &dyn Backend) {
        if self.with_cards(backend).is_none() {
            return;
        }
        match browse_text(backend, BrowseCommand::SetDueDefault) {
            Ok(default) => {
                self.dialog = Some(Dialog::Prompt(Prompt::new(
                    PromptKind::SetDue,
                    "Set due date",
                    "0 = today, 1! = tomorrow and reset interval, 3-7 = random range",
                    &default,
                )))
            }
            Err(error) => self.message = Some(error.to_string()),
        }
    }

    fn open_reposition(&mut self, backend: &dyn Backend) {
        let Some(cards) = self.with_cards(backend) else {
            return;
        };
        match browse(backend, BrowseCommand::RepositionDefaults) {
            Ok(EditResponse::RepositionDefaults(d)) => {
                self.dialog = Some(Dialog::Reposition(Reposition {
                    start: TextInput::new("0"),
                    step: TextInput::new("1"),
                    randomize: d.random,
                    shift: d.shift,
                    cursor: 0,
                    count: cards.len(),
                }))
            }
            Ok(_) => {}
            Err(error) => self.message = Some(error.to_string()),
        }
    }

    fn open_find_replace(&mut self, backend: &dyn Backend) {
        let Some(note_ids) = self.with_notes(backend) else {
            return;
        };
        match browse(
            backend,
            BrowseCommand::FieldNamesForNotes {
                note_ids: note_ids.clone(),
            },
        ) {
            Ok(EditResponse::Strings(fields)) => {
                let mut scopes = vec!["All fields".to_string(), "Tags".to_string()];
                scopes.extend(fields);
                self.dialog = Some(Dialog::FindReplace(FindReplaceForm {
                    find: TextInput::default(),
                    replace: TextInput::default(),
                    scopes,
                    scope: 0,
                    regex: false,
                    match_case: false,
                    cursor: 0,
                    note_ids,
                }));
            }
            Ok(_) => {}
            Err(error) => self.message = Some(error.to_string()),
        }
    }

    fn open_delete(&mut self, backend: &dyn Backend) {
        let Some(note_ids) = self.with_notes(backend) else {
            return;
        };
        self.dialog = Some(Dialog::Confirm(Confirm {
            message: format!(
                "Delete {} and all their cards?",
                plural(note_ids.len(), "note")
            ),
            action: ConfirmAction::DeleteNotes(note_ids),
        }));
    }

    fn dialog_key(&mut self, backend: &dyn Backend, key: &KeyEvent) -> BrowserIntent {
        let Some(dialog) = self.dialog.as_mut() else {
            return BrowserIntent::None;
        };
        match dialog.on_key(key) {
            Outcome::Open => BrowserIntent::None,
            Outcome::Cancel => {
                self.dialog = None;
                BrowserIntent::None
            }
            Outcome::Submit => {
                if let Some(Dialog::Columns(c)) = &mut self.dialog {
                    let key = c.items[c.cursor].0.clone();
                    let (shown, message) = self.toggle_column(backend, &key);
                    if let Some(Dialog::Columns(c)) = &mut self.dialog {
                        c.items[c.cursor].2 = shown;
                    }
                    self.message = message;
                    return BrowserIntent::None;
                }
                let dialog = self.dialog.take().expect("dialog");
                self.submit(backend, dialog)
            }
        }
    }

    /// Shows or hides a column (at least one stays). Returns whether it is
    /// shown now, and a message.
    fn toggle_column(&mut self, backend: &dyn Backend, key: &str) -> (bool, Option<String>) {
        let mut settings = self.settings.clone();
        let list = match settings.mode {
            BrowseMode::Cards => &mut settings.card_columns,
            BrowseMode::Notes => &mut settings.note_columns,
        };
        if let Some(i) = list.iter().position(|k| k == key) {
            if list.len() == 1 {
                return (true, Some("At least one column must be shown".into()));
            }
            list.remove(i);
        } else {
            list.push(key.to_owned());
        }
        let shown = list.iter().any(|k| k == key);
        match self.save_settings(backend, settings) {
            Ok(()) => {
                self.rows.clear();
                (shown, None)
            }
            Err(error) => (!shown, Some(error.to_string())),
        }
    }

    fn sort_by(&mut self, backend: &dyn Backend, index: usize) {
        let Some(key) = self.active_columns().get(index).cloned() else {
            return;
        };
        let Some(direction) = self.column(&key).and_then(|c| self.sortable(c)) else {
            self.message = Some("Sorting on this column is not supported".into());
            return;
        };
        let (current, reverse) = self.sort();
        let same = current == key;
        let reverse = if same {
            !reverse
        } else {
            direction == SortDirection::Descending
        };
        let mut settings = self.settings.clone();
        match settings.mode {
            BrowseMode::Cards => {
                settings.card_sort = key;
                settings.card_sort_reverse = reverse;
            }
            BrowseMode::Notes => {
                settings.note_sort = key;
                settings.note_sort_reverse = reverse;
            }
        }
        if let Err(error) = self.save_settings(backend, settings) {
            self.message = Some(error.to_string());
            return;
        }
        if same {
            // Same column: reverse in memory, as desktop's `_reverse`.
            let keep = self.current();
            let selected = std::mem::take(&mut self.selected);
            let mut ids = std::mem::take(&mut self.ids);
            ids.reverse();
            let rows = std::mem::take(&mut self.rows);
            self.set_ids(ids, keep);
            self.rows = rows;
            self.selected = selected;
        } else {
            let keep = self.current();
            let query = self.query.clone();
            if let Err(error) = self.run_search(backend, query, keep) {
                self.message = Some(error.to_string());
            }
        }
    }

    fn submit(&mut self, backend: &dyn Backend, dialog: Dialog) -> BrowserIntent {
        match dialog {
            Dialog::Prompt(p) => {
                let text = p.input.text().trim().to_owned();
                match p.kind {
                    PromptKind::SaveSearch => self.save_search(backend, text),
                    PromptKind::ChangeDeck => {
                        if let Some(card_ids) = self.with_cards(backend) {
                            let result = browse(
                                backend,
                                BrowseCommand::SetDeckByName {
                                    card_ids,
                                    name: text.clone(),
                                },
                            );
                            let message = match &result {
                                Ok(EditResponse::Count { count, .. }) => {
                                    format!("Moved {} to {text}", plural(*count, "card"))
                                }
                                _ => String::new(),
                            };
                            self.done(backend, result.map(EditOrChanges::from), message);
                        }
                    }
                    PromptKind::AddTags | PromptKind::RemoveTags => {
                        if text.is_empty() {
                            return BrowserIntent::None;
                        }
                        if let Some(note_ids) = self.with_notes(backend) {
                            let add = p.kind == PromptKind::AddTags;
                            let command = if add {
                                NoteCommand::AddTags {
                                    note_ids,
                                    tags: text,
                                }
                            } else {
                                NoteCommand::RemoveTags {
                                    note_ids,
                                    tags: text,
                                }
                            };
                            let result = edit(backend, Command::Notes(command));
                            let message = match &result {
                                Ok(EditResponse::Count { count, .. }) => format!(
                                    "{} tags {} {}",
                                    if add { "Added" } else { "Removed" },
                                    if add { "to" } else { "from" },
                                    plural(*count, "note")
                                ),
                                _ => String::new(),
                            };
                            self.done(backend, result.map(EditOrChanges::from), message);
                        }
                    }
                    PromptKind::SetDue => {
                        if let Some(card_ids) = self.with_cards(backend) {
                            let n = card_ids.len();
                            let result = browse(
                                backend,
                                BrowseCommand::SetDueDate {
                                    card_ids,
                                    days: text,
                                },
                            )
                            .map(EditOrChanges::from);
                            self.done(
                                backend,
                                result,
                                format!("Rescheduled {}", plural(n, "card")),
                            );
                        }
                    }
                }
            }
            Dialog::Choice(c) => match c.kind {
                ChoiceKind::Sort => self.sort_by(backend, c.cursor),
                ChoiceKind::GradeNow => {
                    if let Some(card_ids) = self.with_cards(backend) {
                        let rating = [Rating::Again, Rating::Hard, Rating::Good, Rating::Easy]
                            [c.cursor.min(3)];
                        let n = card_ids.len();
                        let result = browse(backend, BrowseCommand::GradeNow { card_ids, rating })
                            .map(EditOrChanges::from);
                        self.done(backend, result, format!("Graded {}", plural(n, "card")));
                    }
                }
            },
            Dialog::Columns(_) => {}
            Dialog::Forget(f) => {
                if let Some(card_ids) = self.with_cards(backend) {
                    let n = card_ids.len();
                    let result = browse(
                        backend,
                        BrowseCommand::Forget {
                            card_ids,
                            restore_position: f.restore_position,
                            reset_counts: f.reset_counts,
                        },
                    )
                    .map(EditOrChanges::from);
                    self.done(
                        backend,
                        result,
                        format!("Reset {} to new", plural(n, "card")),
                    );
                }
            }
            Dialog::Reposition(r) => {
                let (Ok(start), Ok(step)) = (
                    r.start.text().trim().parse::<i32>(),
                    r.step.text().trim().parse::<i32>(),
                ) else {
                    self.message = Some("Start and step must be whole numbers".into());
                    self.dialog = Some(Dialog::Reposition(r));
                    return BrowserIntent::None;
                };
                if let Some(card_ids) = self.with_cards(backend) {
                    let result = browse(
                        backend,
                        BrowseCommand::Reposition {
                            card_ids,
                            start,
                            step,
                            randomize: r.randomize,
                            shift: r.shift,
                        },
                    );
                    let message = match &result {
                        Ok(EditResponse::Count { count, .. }) => {
                            format!("Repositioned {}", plural(*count, "new card"))
                        }
                        _ => String::new(),
                    };
                    self.done(backend, result.map(EditOrChanges::from), message);
                }
            }
            Dialog::FindReplace(f) => {
                if f.find.text().is_empty() {
                    self.message = Some("Enter the text to find".into());
                    self.dialog = Some(Dialog::FindReplace(f));
                    return BrowserIntent::None;
                }
                let scope = match f.scope {
                    0 => FindReplaceScope::AllFields,
                    1 => FindReplaceScope::Tags,
                    i => FindReplaceScope::Field(f.scopes[i].clone()),
                };
                // Always the explicit notes chosen when the dialog opened.
                let result = browse(
                    backend,
                    BrowseCommand::FindReplace(FindReplace {
                        note_ids: f.note_ids,
                        search: f.find.text().to_owned(),
                        replacement: f.replace.text().to_owned(),
                        regex: f.regex,
                        match_case: f.match_case,
                        scope,
                    }),
                );
                let message = match &result {
                    Ok(EditResponse::Count { count, .. }) => {
                        format!("{} updated", plural(*count, "note"))
                    }
                    _ => String::new(),
                };
                self.done(backend, result.map(EditOrChanges::from), message);
            }
            Dialog::Confirm(c) => match c.action {
                ConfirmAction::DeleteNotes(note_ids) => {
                    let n = note_ids.len();
                    let result = Self::changes(backend, Command::DeleteNotes { note_ids });
                    self.done(backend, result, format!("Deleted {}", plural(n, "note")));
                    if let Err(error) = self.research(backend) {
                        self.message = Some(error.to_string());
                    }
                }
                ConfirmAction::DeleteSaved(name) => {
                    let searches = self
                        .sidebar_data
                        .saved_searches
                        .iter()
                        .filter(|(n, _)| *n != name)
                        .cloned()
                        .collect();
                    let result = browse(backend, BrowseCommand::SetSavedSearches { searches })
                        .map(EditOrChanges::from);
                    self.done(backend, result, format!("Deleted saved search {name}"));
                    self.reload_sidebar_or_report(backend);
                }
            },
        }
        BrowserIntent::None
    }

    fn reload_sidebar_or_report(&mut self, backend: &dyn Backend) {
        if let Err(error) = self.reload_sidebar(backend) {
            self.message = Some(error.to_string());
        }
    }

    /// Adds or replaces a saved search (`savedFilters`).
    fn save_search(&mut self, backend: &dyn Backend, name: String) {
        if name.is_empty() {
            return;
        }
        if name.contains('"') {
            self.message = Some("Names cannot contain \"".into());
            return;
        }
        let mut searches: Vec<(String, String)> = self
            .sidebar_data
            .saved_searches
            .iter()
            .filter(|(n, _)| *n != name)
            .cloned()
            .collect();
        searches.push((name.clone(), self.query.clone()));
        let result =
            browse(backend, BrowseCommand::SetSavedSearches { searches }).map(EditOrChanges::from);
        self.done(backend, result, format!("Saved search {name}"));
        self.reload_sidebar_or_report(backend);
    }

    // ---- sidebar ---------------------------------------------------------------

    fn sidebar_key(&mut self, backend: &dyn Backend, key: &KeyEvent) -> BrowserIntent {
        let n = self.sidebar_rows.len();
        let row = self.sidebar_rows.get(self.sidebar_cursor).cloned();
        match key.code {
            KeyCode::Tab | KeyCode::Esc => self.focus = Focus::Table,
            KeyCode::Char('q') => return BrowserIntent::Close,
            KeyCode::Char('/') => self.focus_search(),
            KeyCode::Down | KeyCode::Char('j') if n > 0 => {
                self.sidebar_cursor = (self.sidebar_cursor + 1).min(n - 1)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.sidebar_cursor = self.sidebar_cursor.saturating_sub(1)
            }
            KeyCode::Home | KeyCode::Char('g') => self.sidebar_cursor = 0,
            KeyCode::End | KeyCode::Char('G') => self.sidebar_cursor = n.saturating_sub(1),
            KeyCode::Char(' ') => {
                if let Some(row) = row.filter(|r| r.expandable) {
                    self.set_collapsed(backend, &row, !row.collapsed);
                }
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if let Some(row) = row {
                    if row.expandable && !row.collapsed {
                        self.set_collapsed(backend, &row, true);
                    } else if row.depth > 0 {
                        // To the parent.
                        if let Some(parent) = self.sidebar_rows[..self.sidebar_cursor]
                            .iter()
                            .rposition(|r| r.depth < row.depth)
                        {
                            self.sidebar_cursor = parent;
                        }
                    }
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if let Some(row) = row.filter(|r| r.expandable && r.collapsed) {
                    self.set_collapsed(backend, &row, false);
                }
            }
            KeyCode::Char('n') => {
                self.negate = !self.negate;
                self.message = Some(if self.negate {
                    "The next sidebar search is negated".into()
                } else {
                    "Negation off".into()
                });
                return BrowserIntent::None;
            }
            KeyCode::Enter => return self.sidebar_search(backend, row, None),
            KeyCode::Char('a') => {
                return self.sidebar_search(backend, row, Some(SearchJoiner::And))
            }
            KeyCode::Char('o') => return self.sidebar_search(backend, row, Some(SearchJoiner::Or)),
            KeyCode::Char('r') => return self.sidebar_replace(backend, row),
            KeyCode::Char('w') => self.open_save_search(),
            KeyCode::Char('x') | KeyCode::Delete => {
                if let Some(ItemKind::Saved(name)) = row.map(|r| r.kind) {
                    self.dialog = Some(Dialog::Confirm(Confirm {
                        message: format!("Delete the saved search {name}?"),
                        action: ConfirmAction::DeleteSaved(name),
                    }));
                }
            }
            _ => {}
        }
        BrowserIntent::None
    }

    fn set_collapsed(&mut self, backend: &dyn Backend, row: &sidebar::Row, collapsed: bool) {
        match &row.kind {
            ItemKind::Section(section) => {
                // Shown at once; stored under desktop's `COLLAPSE_*` key.
                if collapsed {
                    self.sidebar_local.collapsed_sections.insert(*section);
                } else {
                    self.sidebar_local.collapsed_sections.remove(section);
                }
                let index = section.index();
                self.sidebar_data.section_collapsed[index as usize] = collapsed;
                if let Err(error) = browse(
                    backend,
                    BrowseCommand::SetSectionCollapsed {
                        section: index,
                        collapsed,
                    },
                ) {
                    self.message = Some(error.to_string());
                }
            }
            ItemKind::Notetype(id) => {
                if collapsed {
                    self.sidebar_local.expanded_notetypes.remove(id);
                } else {
                    self.sidebar_local.expanded_notetypes.insert(*id);
                }
            }
            ItemKind::Deck(id) => {
                let id = *id;
                match browse(
                    backend,
                    BrowseCommand::SetDeckCollapsed {
                        deck_id: id,
                        collapsed,
                    },
                ) {
                    Ok(_) => {
                        sidebar::set_deck_collapsed(&mut self.sidebar_data.decks, id, collapsed);
                    }
                    Err(error) => self.message = Some(error.to_string()),
                }
            }
            ItemKind::Tag(name) => {
                match browse(
                    backend,
                    BrowseCommand::SetTagCollapsed {
                        name: name.clone(),
                        collapsed,
                    },
                ) {
                    Ok(_) => {
                        sidebar::set_tag_collapsed(&mut self.sidebar_data.tags, name, collapsed);
                    }
                    Err(error) => self.message = Some(error.to_string()),
                }
            }
            ItemKind::Saved(_) | ItemKind::Other => return,
        }
        self.rebuild_sidebar_rows();
    }

    fn item_search(&mut self, row: Option<sidebar::Row>) -> Option<String> {
        let search = row.and_then(|r| r.search);
        if search.is_none() {
            self.message = Some("This item has no search".into());
        }
        search
    }

    /// Enter = search the item; `a` / `o` = AND / OR it with the current
    /// search (desktop's Ctrl / Shift click). `n` first negates it (Alt).
    fn sidebar_search(
        &mut self,
        backend: &dyn Backend,
        row: Option<sidebar::Row>,
        joiner: Option<SearchJoiner>,
    ) -> BrowserIntent {
        let Some(search) = self.item_search(row) else {
            return BrowserIntent::None;
        };
        let negate = std::mem::take(&mut self.negate);
        let result = match joiner {
            None => browse_text(
                backend,
                BrowseCommand::NormalizeSearch {
                    query: if negate {
                        format!("-({search})")
                    } else {
                        search
                    },
                },
            ),
            Some(joiner) => browse_text(
                backend,
                BrowseCommand::JoinSearch {
                    existing: self.query.clone(),
                    additional: search,
                    joiner,
                    negate,
                },
            ),
        };
        match result {
            Ok(query) => self.apply_search(backend, query, Focus::Sidebar),
            Err(error) => {
                self.message = Some(error.to_string());
                BrowserIntent::None
            }
        }
    }

    /// `r`: replace terms of the same kind (desktop's Ctrl+Shift click).
    fn sidebar_replace(
        &mut self,
        backend: &dyn Backend,
        row: Option<sidebar::Row>,
    ) -> BrowserIntent {
        let Some(search) = self.item_search(row) else {
            return BrowserIntent::None;
        };
        let negate = std::mem::take(&mut self.negate);
        match browse_text(
            backend,
            BrowseCommand::ReplaceSearch {
                existing: self.query.clone(),
                replacement: if negate {
                    format!("-({search})")
                } else {
                    search
                },
            },
        ) {
            Ok(query) => self.apply_search(backend, query, Focus::Sidebar),
            Err(error) => {
                self.message = Some(error.to_string());
                BrowserIntent::None
            }
        }
    }

    // ---- drawing -------------------------------------------------------------

    /// Key hints for the current focus (also drawn on the bottom line).
    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if let Some(dialog) = &self.dialog {
            return dialog.hints();
        }
        match self.focus {
            Focus::Search => vec![
                ("Enter", "search"),
                ("Tab", "complete"),
                ("↑/↓", "history"),
                ("Esc", "back"),
            ],
            Focus::Sidebar => vec![
                ("Enter", "search"),
                ("a", "and"),
                ("o", "or"),
                ("r", "replace"),
                ("n", "negate"),
                ("Space", "fold"),
                ("w", "save search"),
                ("x", "delete saved"),
                ("Tab", "table"),
            ],
            Focus::Table => vec![
                ("/", "search"),
                ("Tab", "sidebar"),
                ("Space", "select"),
                ("v", "range"),
                ("C-a", "all"),
                ("*", "invert"),
                ("Enter", "edit"),
                ("p", "preview"),
                ("P", "side"),
                ("M", "cards/notes"),
                ("o", "columns"),
                ("S", "sort"),
                ("c", "deck"),
                ("t/T", "tags +/-"),
                ("U", "clear unused tags"),
                ("m", "mark"),
                ("1-7/0", "flag"),
                ("s", "suspend"),
                ("b", "bury"),
                ("f", "forget"),
                ("d", "due"),
                ("r", "reposition"),
                ("n", "grade"),
                ("R", "find/replace"),
                ("D", "delete"),
                ("i", "info"),
                ("V", "full preview"),
                ("F", "filtered deck"),
                ("w", "save search"),
                ("u/C-r", "undo/redo"),
                ("q", "close"),
            ],
        }
    }

    fn layout(&self, area: Rect) -> Areas {
        let line = |y: u16| Rect::new(area.x, y, area.width, 1.min(area.height));
        let search = line(area.y);
        let status = line(area.bottom().saturating_sub(2).max(area.y));
        let hints = line(area.bottom().saturating_sub(1));
        let middle_y = area.y + 1;
        let middle_h = area.height.saturating_sub(3);
        let mut middle = Rect::new(area.x, middle_y, area.width, middle_h);
        let sidebar = (self.sidebar_visible && area.width >= 60).then(|| {
            let width = SIDEBAR_WIDTH.min(area.width / 3);
            let s = Rect::new(middle.x, middle.y, width, middle.height);
            middle.x += width;
            middle.width -= width;
            s
        });
        let preview = self.preview_visible.then(|| {
            if middle.width >= 100 {
                let width = middle.width * 45 / 100;
                middle.width -= width;
                Rect::new(middle.right(), middle.y, width, middle.height)
            } else {
                let height = middle.height * 45 / 100;
                middle.height -= height;
                Rect::new(middle.x, middle.bottom(), middle.width, height)
            }
        });
        Areas {
            search,
            sidebar,
            table: middle,
            preview,
            status,
            hints,
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, renderer: &dyn CardRenderer) {
        if area.height < 4 || area.width < 20 {
            return;
        }
        let areas = self.layout(area);
        self.render_search(areas.search, buf);
        if let Some(rect) = areas.sidebar {
            self.render_sidebar(rect, buf);
        }
        self.render_table(areas.table, buf);
        if let Some(rect) = areas.preview {
            self.render_preview(rect, buf, renderer);
        }
        self.render_status(areas.status, buf);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let hints: Vec<Span> = self
            .hints()
            .into_iter()
            .flat_map(|(k, d)| {
                [
                    Span::styled(k, Style::default().add_modifier(Modifier::BOLD)),
                    Span::styled(format!(" {d}  "), dim),
                ]
            })
            .collect();
        Paragraph::new(Line::from(hints)).render(areas.hints, buf);
        if let Some(dialog) = &self.dialog {
            dialog.render(area, buf);
        }
        if self.focus == Focus::Search && self.dialog.is_none() {
            let label = "Search: ".width() as u16;
            let anchor = Rect::new(
                areas.search.x + label,
                areas.search.y,
                areas.search.width,
                1,
            );
            self.completion.render(&self.search, anchor, area, buf);
        }
    }

    fn render_search(&self, area: Rect, buf: &mut Buffer) {
        let focused = self.focus == Focus::Search;
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let mode = match self.settings.mode {
            BrowseMode::Cards => " [Cards] ",
            BrowseMode::Notes => " [Notes] ",
        };
        let label = Span::styled("Search: ", if focused { bold } else { Style::default() });
        let text = if focused {
            self.search.text().to_owned()
        } else {
            self.query.clone()
        };
        Paragraph::new(Line::from(vec![label, Span::raw(text)])).render(area, buf);
        let mode_x = area.right().saturating_sub(mode.width() as u16);
        buf.set_string(mode_x, area.y, mode, bold);
        if focused {
            let x = area.x + "Search: ".width() as u16 + self.search.cursor_column() as u16;
            if x < mode_x {
                buf[(x, area.y)].set_style(Style::default().add_modifier(Modifier::REVERSED));
            }
        }
    }

    fn render_sidebar(&self, area: Rect, buf: &mut Buffer) {
        let focused = self.focus == Focus::Sidebar;
        let block = Block::default().borders(Borders::RIGHT);
        let inner = block.inner(area);
        block.render(area, buf);
        let height = inner.height as usize;
        let mut scroll = self.sidebar_scroll;
        if self.sidebar_cursor < scroll {
            scroll = self.sidebar_cursor;
        } else if self.sidebar_cursor >= scroll + height {
            scroll = self.sidebar_cursor + 1 - height;
        }
        for (i, row) in self
            .sidebar_rows
            .iter()
            .enumerate()
            .skip(scroll)
            .take(height)
        {
            let marker = if !row.expandable {
                "  "
            } else if row.collapsed {
                "▸ "
            } else {
                "▾ "
            };
            let text = format!("{}{marker}{}", "  ".repeat(row.depth), row.label);
            let mut style = if row.depth == 0 {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            if i == self.sidebar_cursor && focused {
                style = style.add_modifier(Modifier::REVERSED);
            }
            let y = inner.y + (i - scroll) as u16;
            buf.set_string(
                inner.x,
                y,
                fit(&text, inner.width as usize, Elide::Right, false),
                style,
            );
        }
    }

    fn render_table(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        let keys = self.active_columns();
        let gutter = 2usize;
        let widths = column_widths(keys, (area.width as usize).saturating_sub(gutter));
        let (sort_key, reverse) = self.sort();
        let bold = Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        // Header.
        let mut x = area.x + gutter as u16;
        for (key, &w) in keys.iter().zip(&widths) {
            let column = self.column(key);
            let mut label = column.map_or(key.as_str(), |c| self.label(c)).to_owned();
            if key == sort_key {
                label.push_str(if reverse { " ▼" } else { " ▲" });
            }
            let centered = column.is_some_and(|c| c.centered);
            if x < area.right() {
                buf.set_stringn(
                    x,
                    area.y,
                    fit(&label, w, Elide::Right, centered),
                    (area.right() - x) as usize,
                    bold,
                );
            }
            x = x.saturating_add(w as u16 + 1);
        }
        if self.ids.is_empty() {
            buf.set_string(
                area.x + gutter as u16,
                area.y + 1.min(area.height - 1),
                "No matches",
                Style::default().add_modifier(Modifier::DIM),
            );
            return;
        }
        let body = (area.height - 1) as usize;
        for (offset, &id) in self.ids.iter().enumerate().skip(self.scroll).take(body) {
            let y = area.y + 1 + (offset - self.scroll) as u16;
            let row = self.rows.get(&id);
            let color = row.map_or(RowColor::Default, |r| r.color);
            let mut style = row_style(color);
            let selected = self.selected.contains(&id);
            if offset == self.cursor {
                style = style.add_modifier(if self.focus == Focus::Table {
                    Modifier::REVERSED
                } else {
                    Modifier::UNDERLINED
                });
            }
            let gutter_text = format!("{}{}", if selected { '•' } else { ' ' }, row_glyph(color));
            buf.set_stringn(area.x, y, gutter_text, area.width as usize, style);
            let mut x = area.x + gutter as u16;
            match row {
                None => {
                    buf.set_string(x, y, "…", style.add_modifier(Modifier::DIM));
                }
                Some(BrowserRow { error: Some(e), .. }) => {
                    let w = (area.width as usize).saturating_sub(gutter);
                    buf.set_string(
                        x,
                        y,
                        fit(e, w, Elide::Right, false),
                        style.add_modifier(Modifier::DIM),
                    );
                }
                Some(row) => {
                    for (i, &w) in widths.iter().enumerate() {
                        if x >= area.right() {
                            break;
                        }
                        let cell = row.cells.get(i);
                        let centered = keys
                            .get(i)
                            .and_then(|k| self.column(k))
                            .is_some_and(|c| c.centered);
                        let text =
                            cell.map_or(String::new(), |c| fit(&c.text, w, c.elide, centered));
                        let text = if w < (area.right() - x) as usize {
                            format!("{text} ")
                        } else {
                            text
                        };
                        buf.set_stringn(x, y, text, (area.right() - x) as usize, style);
                        x = x.saturating_add(w as u16 + 1);
                    }
                }
            }
            // Fill the rest of the cursor row so the highlight spans it.
            if offset == self.cursor && x < area.right() {
                let rest = (area.right() - x) as usize;
                buf.set_string(x, y, " ".repeat(rest), style);
            }
        }
    }

    fn render_preview(&self, area: Rect, buf: &mut Buffer, renderer: &dyn CardRenderer) {
        let side = if self.preview_answer {
            "answer"
        } else {
            "question"
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" Preview: {side} "));
        let inner = block.inner(area);
        block.render(area, buf);
        let Some(preview) = &self.preview else {
            return;
        };
        match &preview.card {
            Err(message) => {
                buf.set_string(
                    inner.x,
                    inner.y,
                    fit(message, inner.width as usize, Elide::Right, false),
                    Style::default().add_modifier(Modifier::DIM),
                );
            }
            Ok((card_id, card)) => {
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                (card_id, self.preview_answer, inner.width, inner.height).hash(&mut hasher);
                let key = hasher.finish();
                let mut cache = self.preview_cache.borrow_mut();
                if cache.as_ref().map(|(k, _)| *k) != Some(key) {
                    let html = if self.preview_answer {
                        &card.answer
                    } else {
                        &card.question
                    };
                    let side = services::card_side(
                        card,
                        strip_type_answer(html),
                        &self.media_dir,
                        self.night_mode,
                    );
                    let options = tanki_render::RenderOptions {
                        width: inner.width,
                        viewport_rows: inner.height,
                        night_mode: self.night_mode,
                        ..Default::default()
                    };
                    *cache = Some((key, renderer.render(&side, &options)));
                }
                if let Some((_, rendered)) = cache.as_ref() {
                    let max = rendered.lines.len().saturating_sub(inner.height as usize);
                    renderer.paint(rendered, self.preview_scroll.min(max), inner, buf);
                }
            }
        }
    }

    fn render_status(&self, area: Rect, buf: &mut Buffer) {
        let word = self.item_word();
        let mut text = format!("{} shown", plural(self.ids.len(), word));
        if !self.selected.is_empty() {
            text.push_str(&format!(", {} selected", self.selected.len()));
        }
        if self.visual.is_some() {
            text.push_str(" (range)");
        }
        if self.negate {
            text.push_str(" · negate");
        }
        let mut spans = vec![Span::raw(text)];
        if let Some(message) = &self.message {
            spans.push(Span::raw(" │ "));
            spans.push(Span::styled(
                message.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ));
        }
        Paragraph::new(Line::from(spans))
            .style(Style::default().add_modifier(Modifier::REVERSED))
            .render(area, buf);
    }
}

/// The changes of a finished mutation (any of the answer shapes).
struct EditOrChanges(OpChanges);

impl From<EditResponse> for EditOrChanges {
    fn from(response: EditResponse) -> Self {
        Self(response.changes().cloned().unwrap_or_default())
    }
}
