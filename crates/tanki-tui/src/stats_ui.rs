//! Statistics screen (14 panels) and card info (PLAN 8.8).
//!
//! Owned by the Statistics stream (docs/implementation/phase-5-interfaces.md).
//!
//! [`StatsUi`] is desktop's statistics page: the panels in its order in one
//! scrollable column, a scope (deck / collection / search) and history
//! (1 year / all) switch at the top, and per-panel range switches. A
//! self-contained widget (the TUI widget pattern of `browser_ui`). The app:
//! 1. builds it with [`StatsUi::open`] (runs the graphs query);
//! 2. draws with [`StatsUi::render`];
//! 3. sends keys to [`StatsUi::on_key`] and carries out the returned
//!    [`StatsIntent`];
//! 4. calls [`StatsUi::on_changes`] after reviews or edits elsewhere.
//!
//! Range switches recompute through `StatsCommand::Panels` without a new
//! query; scope and history switches query again. [`CardInfoUi`] is the
//! card info screen.
mod card_info;
mod draw;
#[cfg(test)]
mod tests;

pub use card_info::{CardInfoIntent, CardInfoUi};

use crate::backend::Backend;
use crate::input::TextInput;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{buffer::Buffer, layout::Rect};
use std::cell::Cell;
use tanki_core::stats::{
    GraphsData, History, PanelOptions, Panels, StatsCommand, StatsReport, StatsResponse, StatsScope,
};
use tanki_core::{Command, CoreError, ManageCommand, ManageResponse, OpChanges, Response};

/// What only the app can do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatsIntent {
    None,
    /// Leave the screen.
    Close,
    /// Open the browser with this search (desktop's click on a bar).
    Browse(String),
}

/// How the screen starts.
#[derive(Debug, Clone)]
pub struct StatsOptions {
    pub scope: StatsScope,
    /// The deck's name for the scope line, when the scope is a deck.
    pub deck_name: Option<String>,
    pub history: History,
}

impl Default for StatsOptions {
    fn default() -> Self {
        Self {
            scope: StatsScope::CurrentDeck,
            deck_name: None,
            history: History::Year,
        }
    }
}

/// The panels in page order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelKind {
    Today,
    FutureDue,
    Calendar,
    Reviews,
    CardCounts,
    Intervals,
    Stability,
    Ease,
    Difficulty,
    Retrievability,
    TrueRetention,
    Hours,
    Buttons,
    Added,
}

impl PanelKind {
    const ALL: [PanelKind; 14] = [
        PanelKind::Today,
        PanelKind::FutureDue,
        PanelKind::Calendar,
        PanelKind::Reviews,
        PanelKind::CardCounts,
        PanelKind::Intervals,
        PanelKind::Stability,
        PanelKind::Ease,
        PanelKind::Difficulty,
        PanelKind::Retrievability,
        PanelKind::TrueRetention,
        PanelKind::Hours,
        PanelKind::Buttons,
        PanelKind::Added,
    ];
}

/// The panels shown for this report (SM-2 or FSRS ones).
pub fn visible_panels(panels: &Panels) -> Vec<PanelKind> {
    PanelKind::ALL
        .into_iter()
        .filter(|kind| match kind {
            PanelKind::Stability => panels.stability.is_some(),
            PanelKind::Ease => panels.ease.is_some(),
            PanelKind::Difficulty => panels.difficulty.is_some(),
            PanelKind::Retrievability => panels.retrievability.is_some(),
            _ => true,
        })
        .collect()
}

/// One selectable element of a panel (a bar, a day, a table row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Item {
    pub label: String,
    pub query: Option<String>,
}

pub(crate) fn stats(
    backend: &dyn Backend,
    command: StatsCommand,
) -> Result<StatsResponse, CoreError> {
    match backend.execute(Command::Manage(ManageCommand::Stats(command)))? {
        Response::Manage(ManageResponse::Stats(r)) => Ok(r),
        _ => Err(CoreError::Other(
            "Unexpected response from the collection".into(),
        )),
    }
}

pub struct StatsUi {
    scope: StatsScope,
    /// The deck scope to return to from collection/search.
    deck_scope: Option<StatsScope>,
    deck_name: Option<String>,
    history: History,
    last_search: String,
    data: GraphsData,
    options: PanelOptions,
    panels: Panels,
    focus: usize,
    /// Selected element within the focused panel.
    cursor: Option<usize>,
    scroll: Cell<u16>,
    /// Width and height of the last render (for paging and bar grouping).
    viewport: Cell<(u16, u16)>,
    search: Option<TextInput>,
    message: Option<String>,
}

impl StatsUi {
    pub fn open(backend: &dyn Backend, options: StatsOptions) -> Result<Self, CoreError> {
        let report = Self::query(backend, &options.scope, options.history, None)?;
        let deck_scope = match &options.scope {
            StatsScope::Collection | StatsScope::Search(_) => None,
            scope => Some(scope.clone()),
        };
        let last_search = match &options.scope {
            StatsScope::Search(text) => text.clone(),
            _ => String::new(),
        };
        Ok(Self {
            scope: options.scope,
            deck_scope,
            deck_name: options.deck_name,
            history: options.history,
            last_search,
            data: report.data,
            options: report.options,
            panels: report.panels,
            focus: 0,
            cursor: None,
            scroll: Cell::new(0),
            viewport: Cell::new((80, 24)),
            search: None,
            message: None,
        })
    }

    fn query(
        backend: &dyn Backend,
        scope: &StatsScope,
        history: History,
        options: Option<PanelOptions>,
    ) -> Result<StatsReport, CoreError> {
        match stats(
            backend,
            StatsCommand::Graphs {
                scope: scope.clone(),
                history,
                options,
            },
        )? {
            StatsResponse::Report(report) => Ok(*report),
            _ => Err(CoreError::Other("Unexpected statistics response".into())),
        }
    }

    pub fn panels(&self) -> &Panels {
        &self.panels
    }
    pub fn options(&self) -> &PanelOptions {
        &self.options
    }
    pub fn scope(&self) -> &StatsScope {
        &self.scope
    }
    pub fn history(&self) -> History {
        self.history
    }
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }
    /// The focused panel.
    pub fn focused(&self) -> PanelKind {
        let visible = visible_panels(&self.panels);
        visible[self.focus.min(visible.len() - 1)]
    }

    /// Reviews or edits happened elsewhere: query again, keeping choices.
    pub fn on_changes(&mut self, backend: &dyn Backend, changes: &OpChanges) {
        if changes.card || changes.deck || changes.config || changes.note {
            self.reload(backend);
        }
    }

    fn reload(&mut self, backend: &dyn Backend) {
        let mut options = self.options.clone();
        options.follow_history(self.history);
        match Self::query(backend, &self.scope, self.history, Some(options)) {
            Ok(report) => {
                self.data = report.data;
                self.options = report.options;
                self.panels = report.panels;
                let visible = visible_panels(&self.panels).len();
                self.focus = self.focus.min(visible - 1);
                self.cursor = None;
            }
            Err(error) => self.message = Some(error.to_string()),
        }
    }

    /// Recomputes the panels for changed options (no query).
    fn recompute(&mut self, backend: &dyn Backend) {
        match stats(
            backend,
            StatsCommand::Panels {
                data: Box::new(self.data.clone()),
                options: self.options.clone(),
            },
        ) {
            Ok(StatsResponse::Panels(panels)) => {
                self.panels = *panels;
                self.cursor = None;
            }
            Ok(_) => {}
            Err(error) => self.message = Some(error.to_string()),
        }
    }

    fn save_preferences(&mut self, backend: &dyn Backend) {
        let mut prefs = self.data.preferences;
        prefs.first_weekday = self.options.first_weekday;
        prefs.future_due_show_backlog = self.options.future_due_backlog;
        prefs.card_counts_separate_inactive = self.options.card_counts_separate_inactive;
        self.data.preferences = prefs;
        if let Err(error) = stats(backend, StatsCommand::SetPreferences(prefs)) {
            self.message = Some(error.to_string());
        }
    }

    fn set_scope(&mut self, backend: &dyn Backend, scope: StatsScope) {
        let previous = std::mem::replace(&mut self.scope, scope);
        let before = self.message.take();
        self.reload(backend);
        if self.message.is_some() && before.is_none() {
            // A bad search keeps the previous scope.
            self.scope = previous;
        }
        self.scroll.set(0);
        self.focus = 0;
    }

    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.search.is_some() {
            return vec![("Enter", "show"), ("Esc", "cancel")];
        }
        vec![
            ("Tab/S-Tab", "panel"),
            ("j/k", "scroll"),
            ("←/→", "select"),
            ("Enter", "browse"),
            ("r", "range"),
            ("s", "deck/collection"),
            ("/", "search"),
            ("a", "history"),
            ("t", "count/time"),
            ("b", "backlog"),
            ("i", "inactive"),
            ("[ ]", "year"),
            ("w", "first weekday"),
            ("m", "retention view"),
            ("Esc", "back"),
        ]
    }

    pub fn on_key(&mut self, backend: &dyn Backend, key: KeyEvent) -> StatsIntent {
        if let Some(input) = &mut self.search {
            match key.code {
                KeyCode::Esc => self.search = None,
                KeyCode::Enter => {
                    let text = input.text().trim().to_owned();
                    self.search = None;
                    self.last_search = text.clone();
                    let scope = if text.is_empty() {
                        StatsScope::Collection
                    } else {
                        StatsScope::Search(text)
                    };
                    self.set_scope(backend, scope);
                }
                _ => {
                    input.handle_key(&key);
                }
            }
            return StatsIntent::None;
        }
        self.message = None;
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let (_, height) = self.viewport.get();
        let page = height.saturating_sub(4).max(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return StatsIntent::Close,
            KeyCode::Down | KeyCode::Char('j') => self.scroll_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_by(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll_by(page as i32),
            KeyCode::PageUp => self.scroll_by(-(page as i32)),
            KeyCode::Home | KeyCode::Char('g') => {
                self.scroll.set(0);
                self.focus_panel(0);
            }
            KeyCode::End | KeyCode::Char('G') => {
                let last = visible_panels(&self.panels).len() - 1;
                self.focus_panel(last);
            }
            KeyCode::Tab | KeyCode::Char('J') | KeyCode::Char('n') => {
                self.focus_panel(self.focus + 1)
            }
            KeyCode::BackTab | KeyCode::Char('K') | KeyCode::Char('p') => {
                self.focus_panel(self.focus.saturating_sub(1))
            }
            KeyCode::Left | KeyCode::Char('h') => self.move_cursor(-1),
            KeyCode::Right | KeyCode::Char('l') => self.move_cursor(1),
            KeyCode::Enter => {
                let items = self.items(self.focused());
                if let Some(query) = self
                    .cursor
                    .and_then(|i| items.get(i))
                    .and_then(|item| item.query.clone())
                {
                    return StatsIntent::Browse(query);
                }
            }
            KeyCode::Char('r') if !shift => self.cycle_range(backend),
            KeyCode::Char('s') => {
                let scope = match (&self.scope, &self.deck_scope) {
                    (StatsScope::Collection, Some(deck)) => deck.clone(),
                    (StatsScope::Collection, None) => StatsScope::CurrentDeck,
                    _ => StatsScope::Collection,
                };
                self.set_scope(backend, scope);
            }
            KeyCode::Char('/') => self.search = Some(TextInput::new(self.last_search.clone())),
            KeyCode::Char('a') => {
                self.history = match self.history {
                    History::Year => History::All,
                    History::All => History::Year,
                };
                self.options.follow_history(self.history);
                self.reload(backend);
            }
            KeyCode::Char('t') => {
                self.options.reviews_time = !self.options.reviews_time;
                self.recompute(backend);
            }
            KeyCode::Char('b') => {
                self.options.future_due_backlog = !self.options.future_due_backlog;
                self.save_preferences(backend);
                self.recompute(backend);
            }
            KeyCode::Char('i') => {
                self.options.card_counts_separate_inactive =
                    !self.options.card_counts_separate_inactive;
                self.save_preferences(backend);
                self.recompute(backend);
            }
            KeyCode::Char('w') => {
                self.options.first_weekday = self.options.first_weekday.next();
                self.save_preferences(backend);
                self.recompute(backend);
            }
            KeyCode::Char('[') | KeyCode::Char(']') => {
                let calendar = &self.panels.calendar;
                let delta = if key.code == KeyCode::Char('[') {
                    -1
                } else {
                    1
                };
                let year = (calendar.year + delta).clamp(calendar.min_year, calendar.max_year);
                if year != calendar.year {
                    self.options.calendar_year = Some(year);
                    self.recompute(backend);
                }
            }
            KeyCode::Char('m') => {
                use tanki_core::stats::RetentionMode as M;
                self.options.retention_mode = match self.options.retention_mode {
                    M::Summary => M::Young,
                    M::Young => M::Mature,
                    M::Mature => M::Summary,
                };
                self.recompute(backend);
            }
            _ => {}
        }
        StatsIntent::None
    }

    fn cycle_range(&mut self, backend: &dyn Backend) {
        let all = self.history == History::All;
        let focused = self.focused();
        let calendar = (
            self.panels.calendar.year,
            self.panels.calendar.min_year,
            self.panels.calendar.max_year,
        );
        let o = &mut self.options;
        match focused {
            PanelKind::FutureDue => o.future_due_range = o.future_due_range.next(true),
            PanelKind::Reviews => o.reviews_range = o.reviews_range.next(all),
            PanelKind::Intervals => o.intervals_range = o.intervals_range.next(),
            PanelKind::Stability => o.stability_range = o.stability_range.next(),
            PanelKind::Difficulty => o.difficulty_range = o.difficulty_range.next(),
            PanelKind::Retrievability => o.retrievability_range = o.retrievability_range.next(),
            PanelKind::Hours => o.hours_range = o.hours_range.next(all),
            PanelKind::Buttons => o.buttons_range = o.buttons_range.next(all),
            PanelKind::Added => o.added_range = o.added_range.next(true),
            PanelKind::Calendar => {
                let (year, min, max) = calendar;
                o.calendar_year = Some(if year > min { year - 1 } else { max });
            }
            PanelKind::TrueRetention => {
                use tanki_core::stats::RetentionMode as M;
                o.retention_mode = match o.retention_mode {
                    M::Summary => M::Young,
                    M::Young => M::Mature,
                    M::Mature => M::Summary,
                };
            }
            PanelKind::Today | PanelKind::CardCounts | PanelKind::Ease => return,
        }
        self.recompute(backend);
    }

    fn scroll_by(&mut self, delta: i32) {
        let max = self.content_height().saturating_sub(self.body_height());
        let next = (self.scroll.get() as i32 + delta).clamp(0, max as i32);
        self.scroll.set(next as u16);
    }

    fn focus_panel(&mut self, index: usize) {
        let visible = visible_panels(&self.panels);
        self.focus = index.min(visible.len() - 1);
        self.cursor = None;
        // Bring the focused panel into view.
        let (top, height) = self.panel_span(self.focus);
        let body = self.body_height();
        let scroll = self.scroll.get();
        if top < scroll || top + height.min(body) > scroll + body {
            let max = self.content_height().saturating_sub(body);
            self.scroll.set(top.min(max));
        }
    }

    fn move_cursor(&mut self, delta: i32) {
        let count = self.items(self.focused()).len();
        if count == 0 {
            self.cursor = None;
            return;
        }
        let next = match self.cursor {
            None if delta > 0 => 0,
            None => count - 1,
            Some(i) => (i as i32 + delta).clamp(0, count as i32 - 1) as usize,
        };
        self.cursor = Some(next);
    }

    fn body_height(&self) -> u16 {
        self.viewport.get().1.saturating_sub(2)
    }

    fn inner_width(&self) -> u16 {
        self.viewport.get().0.saturating_sub(2)
    }

    /// Top line and height of a visible panel within the content.
    fn panel_span(&self, index: usize) -> (u16, u16) {
        let width = self.inner_width();
        let visible = visible_panels(&self.panels);
        let mut top = 0;
        for (i, kind) in visible.iter().enumerate() {
            let height = draw::panel_height(self, *kind, width);
            if i == index {
                return (top, height);
            }
            top += height;
        }
        (top, 0)
    }

    fn content_height(&self) -> u16 {
        let width = self.inner_width();
        visible_panels(&self.panels)
            .iter()
            .map(|kind| draw::panel_height(self, *kind, width))
            .sum()
    }

    pub(crate) fn items(&self, kind: PanelKind) -> Vec<Item> {
        draw::items(self, kind, self.viewport.get().0)
    }

    /// Selected element's description, for the status line.
    fn selection_text(&self) -> Option<String> {
        let items = self.items(self.focused());
        let item = items.get(self.cursor?)?;
        Some(item.label.replace('\n', " · "))
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        self.viewport.set((area.width, area.height));
        draw::render(self, area, buf);
    }
}
