//! The filtered deck form (desktop's `filtered_deck.py`).
use super::{
    bold, check, dim, draw_input, frame, is_cancel, number, number_key, put, tools, unexpected,
    DeckToolsIntent,
};
use crate::backend::Backend;
use crate::input::TextInput;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{buffer::Buffer, layout::Rect, style::Style};
use tanki_core::deck_tools::{
    DeckToolsCommand, DeckToolsResponse, FilteredDeck, FilteredDeckForm, FilteredTerm, MAX_LIMIT,
};
use tanki_core::CoreError;

/// Width of the label column.
const LABELS: u16 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Name,
    Search(usize),
    Limit(usize),
    Order(usize),
    Second,
    Reschedule,
    /// Again, Hard, Good.
    Delay(usize),
    AllowEmpty,
}

enum Item {
    Header(String),
    Hint(String),
    Row(Row),
}

/// Create or edit a filtered deck. Enter (or Ctrl+S) saves and builds it;
/// the answer is [`DeckToolsIntent::OpenDeck`].
#[derive(Debug, Clone)]
pub struct FilteredDeckUi {
    form: FilteredDeckForm,
    name: TextInput,
    search: [TextInput; 2],
    limit: [TextInput; 2],
    /// Indexes into `form.orders`.
    order: [usize; 2],
    second: bool,
    reschedule: bool,
    delays: [TextInput; 3],
    allow_empty: bool,
    cursor: usize,
}

impl FilteredDeckUi {
    /// A new deck with Anki's searches for the current deck (desktop's
    /// Tools → Create Filtered Deck).
    pub fn new(backend: &dyn Backend) -> Result<Self, CoreError> {
        Self::load(backend, 0, None)
    }

    /// An existing filtered deck (the overview's options key).
    pub fn edit(backend: &dyn Backend, deck_id: i64) -> Result<Self, CoreError> {
        Self::load(backend, deck_id, None)
    }

    /// A new deck seeded with the browser's search.
    pub fn from_search(backend: &dyn Backend, search: &str) -> Result<Self, CoreError> {
        Self::load(backend, 0, Some(search.to_owned()))
    }

    fn load(
        backend: &dyn Backend,
        deck_id: i64,
        search: Option<String>,
    ) -> Result<Self, CoreError> {
        match tools(
            backend,
            DeckToolsCommand::FilteredDeck {
                deck_id,
                search,
                search_2: None,
            },
        )? {
            DeckToolsResponse::FilteredDeck(form) => Ok(Self::from_form(*form)),
            _ => Err(unexpected()),
        }
    }

    pub fn from_form(form: FilteredDeckForm) -> Self {
        let deck = &form.deck;
        let order = |term: &FilteredTerm| {
            form.orders
                .iter()
                .position(|o| o.value == term.order)
                .unwrap_or(0)
        };
        let mut ui = Self {
            name: TextInput::new(deck.name.clone()),
            search: [
                TextInput::new(deck.first.search.clone()),
                TextInput::new(deck.second.search.clone()),
            ],
            limit: [
                TextInput::new(deck.first.limit.to_string()),
                TextInput::new(deck.second.limit.to_string()),
            ],
            order: [order(&deck.first), order(&deck.second)],
            second: deck.second_enabled,
            reschedule: deck.reschedule,
            delays: [
                TextInput::new(deck.preview_again_secs.to_string()),
                TextInput::new(deck.preview_hard_secs.to_string()),
                TextInput::new(deck.preview_good_secs.to_string()),
            ],
            allow_empty: deck.allow_empty,
            cursor: 0,
            form,
        };
        // Desktop focuses the search field.
        ui.cursor = 1;
        ui
    }

    /// 0 while the deck is not saved yet.
    pub fn deck_id(&self) -> i64 {
        self.form.deck.id
    }

    fn rows(&self) -> Vec<Row> {
        self.items()
            .into_iter()
            .filter_map(|item| match item {
                Item::Row(row) => Some(row),
                _ => None,
            })
            .collect()
    }

    fn items(&self) -> Vec<Item> {
        let labels = &self.form.labels;
        let mut items = vec![
            Item::Row(Row::Name),
            Item::Header(labels.filter.clone()),
            Item::Row(Row::Search(0)),
            Item::Row(Row::Limit(0)),
            Item::Row(Row::Order(0)),
            Item::Row(Row::Second),
        ];
        if self.second {
            items.extend([
                Item::Header(labels.filter_2.clone()),
                Item::Row(Row::Search(1)),
                Item::Row(Row::Limit(1)),
                Item::Row(Row::Order(1)),
            ]);
        }
        items.push(Item::Row(Row::Reschedule));
        if !self.reschedule {
            items.extend([
                Item::Row(Row::Delay(0)),
                Item::Row(Row::Delay(1)),
                Item::Row(Row::Delay(2)),
                Item::Hint(labels.zero_hint.clone()),
            ]);
        }
        items.push(Item::Row(Row::AllowEmpty));
        items
    }

    fn row(&self) -> Row {
        let rows = self.rows();
        rows[self.cursor.min(rows.len() - 1)]
    }

    fn focus(&mut self, row: Row) {
        self.cursor = self.rows().iter().position(|r| *r == row).unwrap_or(0);
    }

    /// The deck as the form stands; numbers are clamped like desktop's
    /// spin boxes.
    pub fn deck(&self) -> FilteredDeck {
        let term = |i: usize| FilteredTerm {
            search: self.search[i].text().to_owned(),
            limit: number(&self.limit[i]).clamp(1, MAX_LIMIT as i64) as u32,
            order: self.form.orders.get(self.order[i]).map_or(0, |o| o.value),
        };
        let delay = |i: usize| number(&self.delays[i]).clamp(0, MAX_LIMIT as i64) as u32;
        FilteredDeck {
            id: self.form.deck.id,
            name: self.name.text().to_owned(),
            reschedule: self.reschedule,
            first: term(0),
            second: term(1),
            second_enabled: self.second,
            preview_again_secs: delay(0),
            preview_hard_secs: delay(1),
            preview_good_secs: delay(2),
            allow_empty: self.allow_empty,
        }
    }

    fn build(&mut self, backend: &dyn Backend) -> DeckToolsIntent {
        match tools(backend, DeckToolsCommand::SaveFilteredDeck(self.deck())) {
            Ok(DeckToolsResponse::Saved { changes, deck_id }) => {
                DeckToolsIntent::OpenDeck { deck_id, changes }
            }
            Ok(_) => DeckToolsIntent::Message(unexpected().to_string()),
            Err(e) => DeckToolsIntent::Message(e.to_string()),
        }
    }

    pub fn on_paste(&mut self, text: &str) {
        match self.row() {
            Row::Name => self.name.insert_str(text),
            Row::Search(i) => self.search[i].insert_str(text),
            _ => {}
        }
    }

    pub fn on_key(&mut self, backend: &dyn Backend, key: KeyEvent) -> DeckToolsIntent {
        if is_cancel(&key) {
            return DeckToolsIntent::Close;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let count = self.rows().len();
        match key.code {
            KeyCode::Enter => return self.build(backend),
            KeyCode::Char('s') if ctrl => return self.build(backend),
            KeyCode::Tab | KeyCode::Down => {
                self.cursor = (self.cursor + 1) % count;
                return DeckToolsIntent::None;
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.cursor = (self.cursor + count - 1) % count;
                return DeckToolsIntent::None;
            }
            _ => {}
        }
        let row = self.row();
        let space = key.code == KeyCode::Char(' ');
        match row {
            Row::Name => {
                self.name.handle_key(&key);
            }
            Row::Search(i) => {
                self.search[i].handle_key(&key);
            }
            Row::Limit(i) => {
                number_key(&mut self.limit[i], &key, false);
            }
            Row::Delay(i) => {
                number_key(&mut self.delays[i], &key, false);
            }
            Row::Order(i) => {
                let n = self.form.orders.len().max(1);
                match key.code {
                    KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('k') => {
                        self.order[i] = (self.order[i] + n - 1) % n
                    }
                    KeyCode::Right
                    | KeyCode::Char('l')
                    | KeyCode::Char('j')
                    | KeyCode::Char(' ') => self.order[i] = (self.order[i] + 1) % n,
                    _ => {}
                }
            }
            Row::Second if space => {
                self.second = !self.second;
                self.focus(Row::Second);
            }
            Row::Reschedule if space => {
                self.reschedule = !self.reschedule;
                self.focus(Row::Reschedule);
            }
            Row::AllowEmpty if space => {
                self.allow_empty = !self.allow_empty;
                self.focus(Row::AllowEmpty);
            }
            _ => {}
        }
        DeckToolsIntent::None
    }

    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        let field = match self.row() {
            Row::Order(_) => ("←/→", "order"),
            Row::Second | Row::Reschedule | Row::AllowEmpty => ("Space", "toggle"),
            _ => ("type", "edit"),
        };
        vec![
            ("Tab/↓", "next"),
            field,
            (
                "Enter",
                if self.form.deck.id == 0 {
                    "build"
                } else {
                    "rebuild"
                },
            ),
            ("Esc", "cancel"),
        ]
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let labels = &self.form.labels;
        let items = self.items();
        let selected = self.row();
        let inner = frame(area, buf, &labels.title, 76, items.len() as u16 + 4);
        if inner.height == 0 || inner.width < 8 {
            return;
        }
        // Keep the selected row in view when the box is cut short.
        let body = inner.height.saturating_sub(2).max(1) as usize;
        let at = items
            .iter()
            .position(|item| matches!(item, Item::Row(row) if *row == selected))
            .unwrap_or(0);
        let skip = (at + 1).saturating_sub(body);
        let field_x = inner.x + 2 + LABELS;
        let field_width = inner.right().saturating_sub(field_x);
        for (line, item) in items.iter().skip(skip).take(body).enumerate() {
            let y = inner.y + line as u16;
            match item {
                Item::Header(text) => put(buf, inner.x + 2, y, inner.width - 2, text, bold()),
                Item::Hint(text) => put(buf, field_x, y, field_width, text, dim()),
                Item::Row(row) => {
                    let focused = *row == selected;
                    let style = if focused { bold() } else { Style::default() };
                    if focused {
                        put(buf, inner.x, y, 2, ">", bold());
                    }
                    let label = |buf: &mut Buffer, text: &str| {
                        put(buf, inner.x + 2, y, LABELS - 1, text, style)
                    };
                    let line = |buf: &mut Buffer, text: &str| {
                        put(buf, inner.x + 2, y, inner.width - 2, text, style)
                    };
                    match *row {
                        Row::Name => {
                            label(buf, &labels.name);
                            draw_input(buf, field_x, y, field_width, &self.name, focused);
                        }
                        Row::Search(i) => {
                            label(buf, &format!("  {}", labels.search));
                            draw_input(buf, field_x, y, field_width, &self.search[i], focused);
                        }
                        Row::Limit(i) => {
                            label(buf, &format!("  {}", labels.limit_to));
                            draw_input(buf, field_x, y, field_width, &self.limit[i], focused);
                        }
                        Row::Order(i) => {
                            label(buf, &format!("  {}", labels.cards_selected_by));
                            let order = self
                                .form
                                .orders
                                .get(self.order[i])
                                .map_or("", |o| o.label.as_str());
                            put(buf, field_x, y, field_width, &format!("‹ {order} ›"), style);
                        }
                        Row::Delay(i) => {
                            let text =
                                [&labels.delay_again, &labels.delay_hard, &labels.delay_good][i];
                            label(buf, &format!("  {text}"));
                            let digits = (self.delays[i].text().chars().count() as u16 + 1).max(6);
                            let digits = digits.min(field_width);
                            draw_input(buf, field_x, y, digits, &self.delays[i], focused);
                            let unit_x = field_x + digits + 1;
                            put(
                                buf,
                                unit_x,
                                y,
                                inner.right().saturating_sub(unit_x),
                                &labels.seconds,
                                dim(),
                            );
                        }
                        Row::Second => line(
                            buf,
                            &format!("{} {}", check(self.second), labels.enable_second_filter),
                        ),
                        Row::Reschedule => line(
                            buf,
                            &format!("{} {}", check(self.reschedule), labels.reschedule),
                        ),
                        Row::AllowEmpty => line(
                            buf,
                            &format!("{} {}", check(self.allow_empty), labels.allow_empty),
                        ),
                    }
                }
            }
        }
        if inner.height >= 3 {
            let footer = format!("Enter {} · Esc cancel", labels.build);
            put(
                buf,
                inner.x + 2,
                inner.bottom() - 1,
                inner.width - 2,
                &footer,
                dim(),
            );
        }
    }
}
