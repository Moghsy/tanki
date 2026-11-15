//! The deck description: the rendered text as the overview shows it, and
//! desktop's `deckdescription.py` editor with its Markdown flag.
use super::{bold, check, dim, frame, is_cancel, put, tools, unexpected, DeckToolsIntent};
use crate::backend::Backend;
use crate::services::{CardRenderer, Display};
use crate::textarea::TextArea;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{buffer::Buffer, layout::Rect, style::Style};
use std::cell::Cell;
use std::path::Path;
use tanki_core::deck_tools::{DeckDescription, DeckToolsCommand, DeckToolsResponse};
use tanki_core::CoreError;

#[derive(Debug, Clone)]
struct Editor {
    area: TextArea,
    markdown: bool,
    /// Esc was pressed once with unsaved changes.
    warned: bool,
}

/// A normal deck's description. It opens on the rendered text (`e` edits)
/// or straight in the editor. Saving from the view answers
/// [`DeckToolsIntent::Changed`] and shows the new text; an editor opened
/// directly answers [`DeckToolsIntent::Done`].
#[derive(Debug, Clone)]
pub struct DeckDescriptionUi {
    info: DeckDescription,
    editor: Option<Editor>,
    /// Opened with [`Self::edit`]: leaving the editor closes the dialog.
    edit_only: bool,
    scroll: usize,
    /// Furthest useful scroll, known after a draw.
    max_scroll: Cell<usize>,
}

impl DeckDescriptionUi {
    /// The rendered description. A filtered deck is an error.
    pub fn open(backend: &dyn Backend, deck_id: i64) -> Result<Self, CoreError> {
        Ok(Self::from_description(load(backend, deck_id)?))
    }

    /// The editor at once (desktop's Description button).
    pub fn edit(backend: &dyn Backend, deck_id: i64) -> Result<Self, CoreError> {
        let mut ui = Self::open(backend, deck_id)?;
        ui.edit_only = true;
        ui.start_edit();
        Ok(ui)
    }

    pub fn from_description(info: DeckDescription) -> Self {
        Self {
            info,
            editor: None,
            edit_only: false,
            scroll: 0,
            max_scroll: Cell::new(0),
        }
    }

    pub fn deck_id(&self) -> i64 {
        self.info.deck_id
    }

    pub fn is_editing(&self) -> bool {
        self.editor.is_some()
    }

    fn start_edit(&mut self) {
        let mut area = TextArea::new(self.info.text.clone());
        area.set_cursor(0);
        self.editor = Some(Editor {
            area,
            markdown: self.info.markdown,
            warned: false,
        });
    }

    fn save(&mut self, backend: &dyn Backend) -> DeckToolsIntent {
        let Some(editor) = &self.editor else {
            return DeckToolsIntent::None;
        };
        let deck_id = self.info.deck_id;
        let command = DeckToolsCommand::SetDescription {
            deck_id,
            text: editor.area.value().to_owned(),
            markdown: editor.markdown,
        };
        let changes = match tools(backend, command) {
            Ok(DeckToolsResponse::Changes(changes)) => changes,
            Ok(_) => return DeckToolsIntent::Message(unexpected().to_string()),
            Err(e) => return DeckToolsIntent::Message(e.to_string()),
        };
        if self.edit_only {
            return DeckToolsIntent::Done {
                changes,
                message: None,
            };
        }
        match load(backend, deck_id) {
            Ok(info) => self.info = info,
            Err(_) => {
                // Saved, but the rendered text could not be fetched again.
                return DeckToolsIntent::Done {
                    changes,
                    message: None,
                };
            }
        }
        self.editor = None;
        self.scroll = 0;
        self.max_scroll.set(0);
        DeckToolsIntent::Changed(changes)
    }

    pub fn on_paste(&mut self, text: &str) {
        if let Some(editor) = &mut self.editor {
            editor.area.paste(text);
            editor.warned = false;
        }
    }

    pub fn on_key(&mut self, backend: &dyn Backend, key: KeyEvent) -> DeckToolsIntent {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(editor) = &mut self.editor {
            if is_cancel(&key) {
                let dirty =
                    editor.area.value() != self.info.text || editor.markdown != self.info.markdown;
                if dirty && !editor.warned {
                    editor.warned = true;
                    return DeckToolsIntent::Message(
                        "Unsaved changes: Ctrl+S saves, Esc again discards.".into(),
                    );
                }
                self.editor = None;
                return if self.edit_only {
                    DeckToolsIntent::Close
                } else {
                    DeckToolsIntent::None
                };
            }
            editor.warned = false;
            match key.code {
                KeyCode::Char('s') if ctrl => return self.save(backend),
                KeyCode::Char('t') if ctrl => editor.markdown = !editor.markdown,
                _ => {
                    editor.area.handle_key(&key);
                }
            }
            return DeckToolsIntent::None;
        }
        if is_cancel(&key) || key.code == KeyCode::Char('q') {
            return DeckToolsIntent::Close;
        }
        let max = self.max_scroll.get();
        let page = 10;
        self.scroll = self.scroll.min(max);
        match key.code {
            KeyCode::Char('e') | KeyCode::Enter => self.start_edit(),
            KeyCode::Down | KeyCode::Char('j') => self.scroll = (self.scroll + 1).min(max),
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll = (self.scroll + page).min(max),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(page),
            KeyCode::Home | KeyCode::Char('g') => self.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => self.scroll = max,
            _ => {}
        }
        DeckToolsIntent::None
    }

    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.editor.is_some() {
            vec![
                ("Ctrl+S", "save"),
                ("Ctrl+T", "Markdown"),
                ("Esc", "cancel"),
            ]
        } else {
            vec![("e", "edit"), ("j/k", "scroll"), ("Esc", "close")]
        }
    }

    /// `media_dir` resolves images in the description.
    pub fn render(
        &self,
        area: Rect,
        buf: &mut Buffer,
        renderer: &dyn CardRenderer,
        display: &Display,
        media_dir: &Path,
    ) {
        let title = format!("{}: {}", self.info.title, self.info.name);
        let height = area.height.saturating_sub(2).clamp(8, 22);
        let inner = frame(area, buf, &title, 78, height);
        if inner.width < 8 || inner.height < 3 {
            return;
        }
        let x = inner.x + 1;
        let width = inner.width - 2;
        let footer = self
            .hints()
            .iter()
            .map(|(key, what)| format!("{key} {what}"))
            .collect::<Vec<_>>()
            .join(" · ");
        put(buf, x, inner.bottom() - 1, width, &footer, dim());
        if let Some(editor) = &self.editor {
            let flag = format!("{} {}", check(editor.markdown), self.info.markdown_label);
            put(buf, x, inner.y, width, &flag, bold());
            let body = Rect::new(x, inner.y + 2, width, inner.height.saturating_sub(4));
            editor.area.render(body, buf, true);
            return;
        }
        let body = Rect::new(x, inner.y, width, inner.height - 2);
        if self.info.html.trim().is_empty() {
            put(
                buf,
                x,
                inner.y,
                width,
                "This deck has no description.",
                dim(),
            );
            self.max_scroll.set(0);
            return;
        }
        let side = tanki_render::CardSide {
            html: self.info.html.clone(),
            css: String::new(),
            body_classes: if display.night_mode {
                "nightMode night_mode".into()
            } else {
                String::new()
            },
            media_dir: media_dir.to_path_buf(),
        };
        let options = display.render_options(
            tanki_term::GraphicsMode::None,
            body,
            true,
            &Default::default(),
        );
        let rendered = renderer.render(&side, &options);
        let max = rendered.lines.len().saturating_sub(body.height as usize);
        self.max_scroll.set(max);
        let scroll = self.scroll.min(max);
        renderer.paint(&rendered, scroll, body, buf);
        if max > 0 {
            let position = format!(
                " {}/{} ",
                scroll + body.height as usize,
                rendered.lines.len()
            );
            let px = inner.right().saturating_sub(position.len() as u16 + 1);
            put(
                buf,
                px,
                inner.bottom() - 1,
                position.len() as u16,
                &position,
                Style::default(),
            );
        }
    }
}

fn load(backend: &dyn Backend, deck_id: i64) -> Result<DeckDescription, CoreError> {
    match tools(backend, DeckToolsCommand::Description { deck_id })? {
        DeckToolsResponse::Description(info) => Ok(info),
        _ => Err(unexpected()),
    }
}
