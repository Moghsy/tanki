//! Filtered deck, custom study, unbury and deck description dialogs (PLAN 8.2).
//!
//! Owned by the Decks tools stream (docs/implementation/phase-5-interfaces.md).
//!
//! Four modal widgets over `tanki_core::deck_tools`, each drawn as a centred
//! box over whatever the app shows:
//!
//! - [`FilteredDeckUi`]: desktop's filtered deck dialog (new, edit, from a
//!   browser search).
//! - [`CustomStudyUi`]: the six custom study modes and the tag chooser.
//! - [`DeckDescriptionUi`]: the rendered description, and its editor with
//!   the Markdown flag.
//! - [`DeckPromptUi`]: add, rename, "Move to…" and delete for the deck list.
//!
//! All four take raw key events and a `&dyn Backend`, and answer with a
//! [`DeckToolsIntent`]. Unbury needs no widget: the app's existing choice
//! runs `Command::Unbury`. Rebuild and empty need none either:
//! [`rebuild_filtered`] and [`empty_filtered`].
mod custom_study;
mod decks;
mod description;
mod filtered;
#[cfg(test)]
mod tests;

pub use custom_study::CustomStudyUi;
pub use decks::{DeckPromptKind, DeckPromptUi};
pub use description::DeckDescriptionUi;
pub use filtered::FilteredDeckUi;

use crate::backend::Backend;
use crate::input::TextInput;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::Span,
    widgets::{Block, Borders, Clear, Widget},
};
use tanki_core::deck_tools::{DeckToolsCommand, DeckToolsResponse};
use tanki_core::{Command, CoreError, ManageCommand, ManageResponse, OpChanges, Response};
use unicode_width::UnicodeWidthChar;

/// What only the app can do after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeckToolsIntent {
    None,
    /// Cancelled or dismissed: remove the dialog.
    Close,
    /// The collection changed and the dialog stays open: refresh the
    /// screens underneath.
    Changed(OpChanges),
    /// Show this in the app's message line; the dialog stays open (Anki's
    /// errors arrive this way, so the user can correct the form).
    Message(String),
    /// Finished: remove the dialog, refresh, and show the message if any.
    Done {
        changes: OpChanges,
        message: Option<String>,
    },
    /// Finished: remove the dialog, refresh, and open this deck's overview.
    /// Anki has already made it the current deck.
    OpenDeck {
        deck_id: i64,
        changes: OpChanges,
    },
}

pub(crate) fn tools(
    backend: &dyn Backend,
    command: DeckToolsCommand,
) -> Result<DeckToolsResponse, CoreError> {
    match backend.execute(Command::Manage(ManageCommand::DeckTools(command)))? {
        Response::Manage(ManageResponse::DeckTools(response)) => Ok(response),
        _ => Err(CoreError::Other(
            "Unexpected response from the collection".into(),
        )),
    }
}

/// The overview's "Rebuild" for a filtered deck. Like desktop, there is no
/// message: the overview's counts show the result.
pub fn rebuild_filtered(backend: &dyn Backend, deck_id: i64) -> Result<OpChanges, CoreError> {
    match tools(backend, DeckToolsCommand::RebuildFilteredDeck { deck_id })? {
        DeckToolsResponse::Count { changes, .. } => Ok(changes),
        _ => Err(unexpected()),
    }
}

/// The overview's "Empty" for a filtered deck: its cards go home.
pub fn empty_filtered(backend: &dyn Backend, deck_id: i64) -> Result<OpChanges, CoreError> {
    match tools(backend, DeckToolsCommand::EmptyFilteredDeck { deck_id })? {
        DeckToolsResponse::Changes(changes) => Ok(changes),
        _ => Err(unexpected()),
    }
}

fn unexpected() -> CoreError {
    CoreError::Other("Unexpected response from the collection".into())
}

fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}
fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}
fn rev() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

fn check(on: bool) -> &'static str {
    if on {
        "[x]"
    } else {
        "[ ]"
    }
}

fn is_cancel(key: &KeyEvent) -> bool {
    key.code == KeyCode::Esc
        || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
}

fn put(buf: &mut Buffer, x: u16, y: u16, width: u16, text: &str, style: Style) {
    if width > 0 {
        buf.set_stringn(x, y, text, width as usize, style);
    }
}

/// Clears and borders a centred box of at most `width` × `height` cells
/// (borders included) and returns its inside.
fn frame(area: Rect, buf: &mut Buffer, title: &str, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let rect = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    Clear.render(rect, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(format!(" {title} "), bold()));
    let inner = block.inner(rect);
    block.render(rect, buf);
    inner
}

/// Draws a one-line input in `width` columns, scrolled so the cursor stays
/// visible; the caret is a reversed cell when `focused`.
fn draw_input(buf: &mut Buffer, x: u16, y: u16, width: u16, input: &TextInput, focused: bool) {
    if width == 0 {
        return;
    }
    let column = input.cursor_column();
    let skip = (column + 1).saturating_sub(width as usize);
    let mut seen = 0;
    let visible: String = input
        .text()
        .chars()
        .filter(|c| {
            let before = seen;
            seen += c.width().unwrap_or(0);
            before >= skip
        })
        .collect();
    put(buf, x, y, width, &visible, Style::default());
    if focused {
        let caret = x + (column - skip) as u16;
        if caret < x + width {
            buf[(caret, y)].set_style(rev());
        }
    }
}

/// A key for a number field: digits and editing keys only. With `signed`,
/// `-` flips the sign.
fn number_key(input: &mut TextInput, key: &KeyEvent, signed: bool) -> bool {
    let plain = !key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    match key.code {
        KeyCode::Char('-') if signed && plain => {
            let text = input.text();
            *input = TextInput::new(match text.strip_prefix('-') {
                Some(rest) => rest.to_owned(),
                None => format!("-{text}"),
            });
            true
        }
        KeyCode::Char(c) if plain && !c.is_ascii_digit() => false,
        KeyCode::Char(_) if plain && input.text().trim_start_matches('-').len() >= 6 => true,
        _ => input.handle_key(key),
    }
}

fn number(input: &TextInput) -> i64 {
    input.text().trim().parse().unwrap_or(0)
}
