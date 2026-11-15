//! Custom study (desktop's `customstudy.py` and `taglimit.py`).
use super::{
    bold, dim, draw_input, frame, is_cancel, number, number_key, put, tools, unexpected,
    DeckToolsIntent,
};
use crate::backend::Backend;
use crate::input::TextInput;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{buffer::Buffer, layout::Rect, style::Style};
use tanki_core::deck_tools::{
    CramKind, CustomStudy, CustomStudyDefaults, DeckToolsCommand, DeckToolsResponse,
    MAX_FORGOT_DAYS, MAX_LIMIT,
};
use tanki_core::CoreError;

const NEW: usize = 0;
const REVIEW: usize = 1;
const FORGOT: usize = 2;
const AHEAD: usize = 3;
const PREVIEW: usize = 4;
const CRAM: usize = 5;

const KINDS: [CramKind; 4] = [
    CramKind::New,
    CramKind::Due,
    CramKind::Review,
    CramKind::All,
];

/// The tag chooser of "Study by card state or tag".
#[derive(Debug, Clone)]
struct TagPicker {
    cursor: usize,
    include: Vec<bool>,
    exclude: Vec<bool>,
}

/// The six custom study modes. Enter runs the selected one: the two limit
/// modes answer [`DeckToolsIntent::Done`], the four deck-building modes
/// [`DeckToolsIntent::OpenDeck`] with the "Custom Study Session" deck.
#[derive(Debug, Clone)]
pub struct CustomStudyUi {
    defaults: CustomStudyDefaults,
    mode: usize,
    value: TextInput,
    kind: usize,
    tags: Option<TagPicker>,
}

impl CustomStudyUi {
    /// For a normal deck; a filtered deck is an error with Anki's message.
    pub fn open(backend: &dyn Backend, deck_id: i64) -> Result<Self, CoreError> {
        match tools(backend, DeckToolsCommand::CustomStudyDefaults { deck_id })? {
            DeckToolsResponse::CustomStudyDefaults(defaults) => Ok(Self::from_defaults(*defaults)),
            _ => Err(unexpected()),
        }
    }

    pub fn from_defaults(defaults: CustomStudyDefaults) -> Self {
        let mut ui = Self {
            defaults,
            mode: NEW,
            value: TextInput::default(),
            kind: 0,
            tags: None,
        };
        ui.set_mode(NEW);
        ui
    }

    pub fn deck_id(&self) -> i64 {
        self.defaults.deck_id
    }

    /// `onRadioChange`: every mode starts from its own default.
    fn set_mode(&mut self, mode: usize) {
        self.mode = mode;
        let value = match mode {
            NEW => self.defaults.extend_new,
            REVIEW => self.defaults.extend_review,
            CRAM => 100,
            _ => 1,
        };
        self.value = TextInput::new(value.to_string());
    }

    /// The spin box range of the mode.
    fn range(&self) -> (i64, i64) {
        let max = MAX_LIMIT as i64;
        match self.mode {
            NEW | REVIEW => (-max, max),
            FORGOT => (1, MAX_FORGOT_DAYS as i64),
            _ => (1, max),
        }
    }

    fn amount(&self) -> i64 {
        let (min, max) = self.range();
        number(&self.value).clamp(min, max)
    }

    /// The request the form describes; tags come from the chooser.
    fn request(&self) -> CustomStudy {
        let n = self.amount();
        match self.mode {
            NEW => CustomStudy::NewLimitDelta(n as i32),
            REVIEW => CustomStudy::ReviewLimitDelta(n as i32),
            FORGOT => CustomStudy::ForgotDays(n as u32),
            AHEAD => CustomStudy::ReviewAheadDays(n as u32),
            PREVIEW => CustomStudy::PreviewDays(n as u32),
            _ => {
                let chosen = |flags: &[bool]| {
                    self.defaults
                        .tags
                        .iter()
                        .zip(flags)
                        .filter(|(_, on)| **on)
                        .map(|(tag, _)| tag.name.clone())
                        .collect::<Vec<_>>()
                };
                let (include, exclude) = match &self.tags {
                    Some(picker) => (chosen(&picker.include), chosen(&picker.exclude)),
                    None => (vec![], vec![]),
                };
                CustomStudy::Cram {
                    kind: KINDS[self.kind],
                    card_limit: n as u32,
                    tags_to_include: include,
                    tags_to_exclude: exclude,
                }
            }
        }
    }

    fn run(&mut self, backend: &dyn Backend) -> DeckToolsIntent {
        let command = DeckToolsCommand::CustomStudy {
            deck_id: self.defaults.deck_id,
            study: self.request(),
        };
        // Desktop keeps the dialog open on failure: the cause is most
        // likely an empty result the user can remedy.
        match tools(backend, command) {
            Ok(DeckToolsResponse::Saved { changes, deck_id }) if deck_id != 0 => {
                DeckToolsIntent::OpenDeck { deck_id, changes }
            }
            Ok(DeckToolsResponse::Saved { changes, .. }) => DeckToolsIntent::Done {
                changes,
                message: None,
            },
            Ok(_) => DeckToolsIntent::Message(unexpected().to_string()),
            Err(e) => DeckToolsIntent::Message(e.to_string()),
        }
    }

    fn accept(&mut self, backend: &dyn Backend) -> DeckToolsIntent {
        // Desktop disables OK for the deck-building modes on an empty deck.
        if self.mode > REVIEW && self.defaults.card_count == 0 {
            return DeckToolsIntent::Message("This deck has no cards.".into());
        }
        if self.mode == CRAM && !self.defaults.tags.is_empty() {
            let tags = &self.defaults.tags;
            self.tags = Some(TagPicker {
                cursor: 0,
                include: tags.iter().map(|t| t.include).collect(),
                exclude: tags.iter().map(|t| t.exclude).collect(),
            });
            return DeckToolsIntent::None;
        }
        self.run(backend)
    }

    pub fn on_key(&mut self, backend: &dyn Backend, key: KeyEvent) -> DeckToolsIntent {
        if let Some(picker) = self.tags.as_mut() {
            if is_cancel(&key) {
                self.tags = None;
                return DeckToolsIntent::None;
            }
            let count = picker.include.len();
            let at = picker.cursor;
            match key.code {
                KeyCode::Enter => return self.run(backend),
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                    picker.cursor = (at + 1) % count
                }
                KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                    picker.cursor = (at + count - 1) % count
                }
                KeyCode::Home | KeyCode::Char('g') => picker.cursor = 0,
                KeyCode::End | KeyCode::Char('G') => picker.cursor = count - 1,
                KeyCode::PageDown => picker.cursor = (at + 10).min(count - 1),
                KeyCode::PageUp => picker.cursor = at.saturating_sub(10),
                KeyCode::Char('i') | KeyCode::Char('+') => {
                    picker.include[at] = !picker.include[at];
                    picker.exclude[at] = false;
                }
                KeyCode::Char('x') | KeyCode::Char('-') => {
                    picker.exclude[at] = !picker.exclude[at];
                    picker.include[at] = false;
                }
                // none -> require -> exclude -> none
                KeyCode::Char(' ') => {
                    let (include, exclude) = match (picker.include[at], picker.exclude[at]) {
                        (false, false) => (true, false),
                        (true, false) => (false, true),
                        _ => (false, false),
                    };
                    picker.include[at] = include;
                    picker.exclude[at] = exclude;
                }
                KeyCode::Char('c') => {
                    picker.include.fill(false);
                    picker.exclude.fill(false);
                }
                _ => {}
            }
            return DeckToolsIntent::None;
        }
        if is_cancel(&key) {
            return DeckToolsIntent::Close;
        }
        match key.code {
            KeyCode::Enter => return self.accept(backend),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => self.set_mode((self.mode + 1) % 6),
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                self.set_mode((self.mode + 5) % 6)
            }
            KeyCode::Left | KeyCode::Char('h') if self.mode == CRAM => {
                self.kind = (self.kind + 3) % 4
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') if self.mode == CRAM => {
                self.kind = (self.kind + 1) % 4
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End => {}
            _ => {
                number_key(&mut self.value, &key, self.mode <= REVIEW);
            }
        }
        DeckToolsIntent::None
    }

    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.tags.is_some() {
            return vec![
                ("j/k", "move"),
                ("i", "require"),
                ("x", "exclude"),
                ("c", "clear"),
                ("Enter", "study"),
                ("Esc", "back"),
            ];
        }
        let mut hints = vec![("j/k", "mode"), ("0-9", "amount")];
        match self.mode {
            NEW | REVIEW => hints.push(("-", "decrease")),
            CRAM => hints.push(("←/→", "cards")),
            _ => {}
        }
        hints.push((
            "Enter",
            if self.mode == CRAM && !self.defaults.tags.is_empty() {
                "choose tags"
            } else {
                "ok"
            },
        ));
        hints.push(("Esc", "cancel"));
        hints
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        match &self.tags {
            Some(picker) => self.render_tags(picker, area, buf),
            None => self.render_modes(area, buf),
        }
    }

    fn render_modes(&self, area: Rect, buf: &mut Buffer) {
        let labels = &self.defaults.labels;
        let title = format!("Custom Study: {}", self.defaults.deck_name);
        let inner = frame(area, buf, &title, 64, 14);
        if inner.width < 8 {
            return;
        }
        let width = inner.width - 2;
        let x = inner.x + 2;
        let mut y = inner.y;
        let bottom = inner.bottom();
        for (i, label) in labels.modes.iter().enumerate() {
            if y >= bottom {
                return;
            }
            let on = i == self.mode;
            if on {
                put(buf, inner.x, y, 2, ">", bold());
            }
            let mark = if on { "(•)" } else { "( )" };
            let style = if on { bold() } else { Style::default() };
            put(buf, x, y, width, &format!("{mark} {label}"), style);
            y += 1;
        }
        y += 1;
        // The line above the number: what is available, for the limit modes.
        let available = match self.mode {
            NEW => labels.available_new.as_str(),
            REVIEW => labels.available_review.as_str(),
            _ => "",
        };
        if y < bottom {
            put(buf, x, y, width, available, dim());
        }
        y += 1;
        if y < bottom {
            let before = &labels.before[self.mode];
            let n = self.amount();
            let after = match self.mode {
                NEW | REVIEW => labels.cards.get(n),
                CRAM => labels.cards_from_the_deck.get(n),
                _ => labels.days.get(n),
            };
            put(buf, x, y, width, before, Style::default());
            let field_x = x + before.chars().count() as u16 + 1;
            let field = (self.value.text().chars().count() as u16 + 1).max(4);
            if field_x + field < inner.right() {
                draw_input(buf, field_x, y, field, &self.value, true);
                let after_x = field_x + field + 1;
                put(
                    buf,
                    after_x,
                    y,
                    inner.right().saturating_sub(after_x),
                    after,
                    Style::default(),
                );
            }
        }
        y += 1;
        if self.mode == CRAM && y < bottom {
            put(
                buf,
                x,
                y,
                width,
                &format!("‹ {} ›", labels.cram_kinds[self.kind]),
                Style::default(),
            );
        }
        y += 2;
        if y < bottom {
            let ok = if self.mode == CRAM && !self.defaults.tags.is_empty() {
                &labels.choose_tags
            } else {
                &labels.ok
            };
            put(buf, x, y, width, &format!("Enter {ok} · Esc cancel"), dim());
        }
    }

    fn render_tags(&self, picker: &TagPicker, area: Rect, buf: &mut Buffer) {
        let labels = &self.defaults.labels;
        let count = picker.include.len();
        let inner = frame(area, buf, &labels.tags_title, 64, count as u16 + 6);
        if inner.width < 8 || inner.height < 4 {
            return;
        }
        let x = inner.x + 2;
        let width = inner.width - 2;
        put(
            buf,
            x,
            inner.y,
            width,
            &format!("[+] {}", labels.require_tags),
            dim(),
        );
        put(
            buf,
            x,
            inner.y + 1,
            width,
            &format!("[-] {}", labels.exclude_tags),
            dim(),
        );
        let rows = inner.height.saturating_sub(4) as usize;
        let skip = (picker.cursor + 1).saturating_sub(rows);
        for (line, i) in (skip..count).take(rows).enumerate() {
            let y = inner.y + 2 + line as u16;
            let on = i == picker.cursor;
            if on {
                put(buf, inner.x, y, 2, ">", bold());
            }
            let mark = match (picker.include[i], picker.exclude[i]) {
                (true, _) => "[+]",
                (_, true) => "[-]",
                _ => "[ ]",
            };
            let style = if on { bold() } else { Style::default() };
            let name = &self.defaults.tags[i].name;
            put(buf, x, y, width, &format!("{mark} {name}"), style);
        }
        let chosen = picker.include.iter().filter(|on| **on).count();
        let excluded = picker.exclude.iter().filter(|on| **on).count();
        put(
            buf,
            x,
            inner.bottom() - 1,
            width,
            &format!(
                "{chosen} required · {excluded} excluded · Enter {}",
                labels.ok
            ),
            dim(),
        );
    }
}
