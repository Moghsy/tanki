//! CSV/JSON import, text export and find duplicates (PLAN 8.11).
//!
//! Owned by the Import/export extras stream (docs/implementation/phase-5-interfaces.md).
//!
//! Three self-contained widgets over `tanki_core::transfer`, each drawn as a
//! centred box over whatever the app shows. All take raw key events and a
//! `&dyn Backend` and answer with a [`TransferIntent`]:
//!
//! - [`ImportUi`]: the field-mapping form for a text file (delimiter, HTML,
//!   note type, deck, column mapping, tags, duplicate handling), the running
//!   import with progress, and the result log. `ImportUi::open_json` runs a
//!   `ForeignData` file straight away. Call [`ImportUi::tick`] from the event
//!   loop while it is open.
//! - [`ExportUi`]: notes or cards as a text file for a selection.
//! - [`DuplicatesUi`]: find duplicates; a group answers
//!   [`TransferIntent::BrowseSearch`].
mod dupes;
mod export;
mod import;
#[cfg(test)]
mod tests;

pub use dupes::DuplicatesUi;
pub use export::ExportUi;
pub use import::ImportUi;

use crate::backend::{Backend, CommandOp};
use crate::input::TextInput;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::Span,
    widgets::{Block, Borders, Clear, Widget},
};
use tanki_core::transfer::{TransferCommand, TransferResponse};
use tanki_core::{
    Command, CoreError, ManageCommand, ManageResponse, OpChanges, OperationProgress, Response,
};
use unicode_width::UnicodeWidthChar;

/// What only the app can do after a key or a tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferIntent {
    None,
    /// Cancelled or dismissed: remove the widget.
    Close,
    /// Finished: remove the widget, refresh the screens underneath, and show
    /// the message if any.
    Done {
        changes: OpChanges,
        message: Option<String>,
    },
    /// The collection changed and the widget stays open: refresh.
    Changed(OpChanges),
    /// Show this in the app's message line; the widget stays open.
    Message(String),
    /// Search the browser for this (a `nid:` list for one duplicate group).
    /// The widget stays open; the app decides whether to keep it beneath.
    BrowseSearch(String),
}

pub(crate) fn transfer(
    backend: &dyn Backend,
    command: TransferCommand,
) -> Result<TransferResponse, CoreError> {
    match backend.execute(Command::Manage(ManageCommand::Transfer(command)))? {
        Response::Manage(ManageResponse::Transfer(response)) => Ok(response),
        _ => Err(unexpected()),
    }
}

pub(crate) fn unexpected() -> CoreError {
    CoreError::Other("Unexpected response from the collection".into())
}

/// A queued transfer command being polled from `tick`.
pub(crate) struct Run {
    op: Box<dyn CommandOp>,
    pub progress: OperationProgress,
    pub cancelling: bool,
}

impl Run {
    pub fn start(backend: &dyn Backend, command: TransferCommand) -> Result<Self, CoreError> {
        let op = backend.start(Command::Manage(ManageCommand::Transfer(command)))?;
        Ok(Self {
            op,
            progress: OperationProgress::default(),
            cancelling: false,
        })
    }

    /// `Ok(None)` while the actor is still working.
    pub fn poll(&mut self, backend: &dyn Backend) -> Result<Option<TransferResponse>, CoreError> {
        match self.op.poll()? {
            Some(Response::Manage(ManageResponse::Transfer(response))) => Ok(Some(response)),
            Some(_) => Err(unexpected()),
            None => {
                self.progress = backend.data_progress();
                Ok(None)
            }
        }
    }

    pub fn cancel(&mut self, backend: &dyn Backend) {
        self.cancelling = true;
        backend.cancel_data();
    }

    pub fn line(&self) -> String {
        let p = &self.progress;
        let stage = if self.cancelling {
            "Cancelling…"
        } else if p.stage.is_empty() {
            "Working…"
        } else {
            &p.stage
        };
        match (p.current, p.total) {
            (Some(c), Some(t)) => format!("{stage} {c}/{t}"),
            (Some(c), None) => format!("{stage} {c}"),
            _ => stage.to_owned(),
        }
    }
}

pub(crate) fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}
pub(crate) fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}
pub(crate) fn rev() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

pub(crate) fn check(on: bool) -> &'static str {
    if on {
        "[x]"
    } else {
        "[ ]"
    }
}

pub(crate) fn is_cancel(key: &KeyEvent) -> bool {
    key.code == KeyCode::Esc
        || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
}

pub(crate) fn put(buf: &mut Buffer, x: u16, y: u16, width: u16, text: &str, style: Style) {
    if width > 0 {
        buf.set_stringn(x, y, text, width as usize, style);
    }
}

/// Clears and borders a centred box of at most `width` × `height` cells
/// (borders included) and returns its inside.
pub(crate) fn frame(area: Rect, buf: &mut Buffer, title: &str, width: u16, height: u16) -> Rect {
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

/// A one-line input in `width` columns, scrolled so the cursor stays visible.
pub(crate) fn draw_input(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    input: &TextInput,
    focused: bool,
) {
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

/// The last path component, for titles.
pub(crate) fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Moves a focus index with Up/Down/Tab/BackTab; true when the key was one.
pub(crate) fn move_focus(key: &KeyEvent, focus: &mut usize, rows: usize) -> bool {
    if rows == 0 {
        return false;
    }
    match key.code {
        KeyCode::Down | KeyCode::Tab => *focus = (*focus + 1) % rows,
        KeyCode::Up | KeyCode::BackTab => *focus = (*focus + rows - 1) % rows,
        _ => return false,
    }
    true
}

/// Cycles a choice index with Left/Right (and Space, forward); true when
/// the key was one.
pub(crate) fn cycle(key: &KeyEvent, index: &mut usize, count: usize) -> bool {
    if count == 0 {
        return false;
    }
    match key.code {
        KeyCode::Right | KeyCode::Char(' ') => *index = (*index + 1) % count,
        KeyCode::Left => *index = (*index + count - 1) % count,
        _ => return false,
    }
    true
}
