//! Tools screen: check database, check media, empty cards, preferences, profiles (PLAN 8.10, 8.12, 8.13).
//!
//! Owned by the Maintenance stream (docs/implementation/phase-5-interfaces.md).
//!
//! [`ToolsUi`] is one full-screen widget the app shows as a layer. It takes
//! raw key events and a `&dyn Backend`, polls its long operation from
//! [`ToolsUi::tick`], and answers with a [`ToolsIntent`]:
//!
//! - Check Database and Check Media explain what they will change and wait for
//!   `y` before starting. Each backs up first (inside the core command), shows
//!   Anki's progress, and Esc cancels.
//! - Empty Cards reports first; deleting asks again and honours the
//!   keep-notes rule.
//! - Preferences edits the collection-stored form.
//! - Profiles lists, creates, renames and deletes the base folder's profile
//!   folders without opening them. Switching is an intent: only the app can
//!   close this collection and open another.
mod check;
mod prefs;
mod profiles;
#[cfg(test)]
mod tests;

use crate::backend::{Backend, CommandOp};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};
use std::path::PathBuf;
use tanki_core::maintenance::{MaintenanceCommand, MaintenanceResponse};
use tanki_core::{Command, CoreError, ManageCommand, ManageResponse, OpChanges, Response};

pub use check::CheckKind;

/// What only the app can do after a key or a tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolsIntent {
    None,
    /// Leave the Tools screen.
    Close,
    /// The collection changed: refresh the screens underneath.
    Changed(OpChanges),
    /// Show this in the app's message line.
    Message(String),
    /// Close this collection and open the named profile of the same base
    /// folder (the app must save nothing of this screen; it holds no edits).
    SwitchProfile(String),
}

pub(crate) type Result<T> = std::result::Result<T, CoreError>;

pub(crate) fn unexpected() -> CoreError {
    CoreError::Other("Unexpected response from the collection".into())
}

pub(crate) fn command(c: MaintenanceCommand) -> Command {
    Command::Manage(ManageCommand::Maintenance(c))
}

pub(crate) fn into_response(r: Response) -> Result<MaintenanceResponse> {
    match r {
        Response::Manage(ManageResponse::Maintenance(r)) => Ok(r),
        _ => Err(unexpected()),
    }
}

pub(crate) fn run(backend: &dyn Backend, c: MaintenanceCommand) -> Result<MaintenanceResponse> {
    into_response(backend.execute(command(c))?)
}

pub(crate) fn start(backend: &dyn Backend, c: MaintenanceCommand) -> Result<Box<dyn CommandOp>> {
    backend.start(command(c))
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

pub(crate) fn put(buf: &mut Buffer, x: u16, y: u16, width: u16, text: &str, style: Style) {
    if width > 0 {
        buf.set_stringn(x, y, text, width as usize, style);
    }
}

/// Word-wraps `text` to `width` columns. Blank lines and each paragraph's
/// leading indentation are kept.
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut out = Vec::new();
    for para in text.split('\n') {
        let indent = &para[..para.len() - para.trim_start().len()];
        let mut line = String::new();
        for word in para.split_whitespace() {
            let sep = usize::from(!line.is_empty());
            if !line.is_empty()
                && indent.len() + line.chars().count() + sep + word.chars().count() > width
            {
                out.push(format!("{indent}{}", std::mem::take(&mut line)));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        out.push(if line.is_empty() {
            String::new()
        } else {
            format!("{indent}{line}")
        });
    }
    out
}

pub(crate) fn is_cancel(key: &KeyEvent) -> bool {
    key.code == KeyCode::Esc
        || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
}

/// A sub-screen's answer to a key.
pub(crate) enum Step {
    Stay,
    /// Back to the menu.
    Back,
    Intent(ToolsIntent),
}

const MENU: [(&str, &str); 5] = [
    (
        "Check Database",
        "Repair and optimize; backs up first, clears undo",
    ),
    (
        "Check Media",
        "Unused and missing files, media trash; backs up first",
    ),
    (
        "Empty Cards",
        "Find cards whose front renders empty, then delete them",
    ),
    ("Preferences", "Scheduling, reviewing, editing and backups"),
    ("Profiles", "List, create, rename and delete profiles"),
];

enum Screen {
    Menu,
    Check(check::CheckUi),
    Prefs(prefs::PrefsUi),
    Profiles(profiles::ProfilesUi),
}

pub struct ToolsUi {
    screen: Screen,
    menu: usize,
    base: Option<PathBuf>,
    open_profile: Option<String>,
    config: Option<PathBuf>,
}

impl ToolsUi {
    /// `base` is the Anki base folder holding the profiles (profiles are
    /// unavailable without it); `open_profile` is the folder name of the
    /// profile this collection belongs to; `config` is tanki's config file,
    /// whose per-profile metadata follows a rename.
    pub fn new(
        base: Option<PathBuf>,
        open_profile: Option<String>,
        config: Option<PathBuf>,
    ) -> Self {
        Self {
            screen: Screen::Menu,
            menu: 0,
            base,
            open_profile,
            config,
        }
    }

    /// True while a check is running (the app should keep ticking).
    pub fn is_busy(&self) -> bool {
        matches!(&self.screen, Screen::Check(c) if c.is_running())
    }

    pub fn on_key(&mut self, key: KeyEvent, backend: &dyn Backend) -> ToolsIntent {
        let step = match &mut self.screen {
            Screen::Menu => return self.menu_key(key, backend),
            Screen::Check(c) => c.on_key(key, backend),
            Screen::Prefs(p) => p.on_key(key, backend),
            Screen::Profiles(p) => p.on_key(key),
        };
        match step {
            Step::Stay => ToolsIntent::None,
            Step::Back => {
                self.screen = Screen::Menu;
                ToolsIntent::None
            }
            Step::Intent(intent) => intent,
        }
    }

    fn menu_key(&mut self, key: KeyEvent, backend: &dyn Backend) -> ToolsIntent {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => ToolsIntent::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.menu = self.menu.saturating_sub(1);
                ToolsIntent::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.menu = (self.menu + 1).min(MENU.len() - 1);
                ToolsIntent::None
            }
            KeyCode::Enter => self.open(self.menu, backend),
            KeyCode::Char(c @ '1'..='5') => self.open(c as usize - '1' as usize, backend),
            _ => ToolsIntent::None,
        }
    }

    fn open(&mut self, index: usize, backend: &dyn Backend) -> ToolsIntent {
        self.menu = index;
        let opened: Result<Screen> = match index {
            0 => Ok(Screen::Check(check::CheckUi::new(CheckKind::Database))),
            1 => Ok(Screen::Check(check::CheckUi::new(CheckKind::Media))),
            2 => check::CheckUi::start_empty_cards(backend).map(Screen::Check),
            3 => prefs::PrefsUi::load(backend).map(Screen::Prefs),
            _ => Ok(Screen::Profiles(profiles::ProfilesUi::new(
                self.base.clone(),
                self.open_profile.clone(),
                self.config.clone(),
            ))),
        };
        match opened {
            Ok(screen) => {
                self.screen = screen;
                ToolsIntent::None
            }
            Err(error) => ToolsIntent::Message(format!("{}: {error}", MENU[index].0)),
        }
    }

    /// Polls the running check; call it on every event-loop tick.
    pub fn tick(&mut self, backend: &dyn Backend) -> ToolsIntent {
        match &mut self.screen {
            Screen::Check(c) => c.tick(backend),
            _ => ToolsIntent::None,
        }
    }

    pub fn paste(&mut self, text: &str) {
        match &mut self.screen {
            Screen::Prefs(p) => p.paste(text),
            Screen::Profiles(p) => p.paste(text),
            _ => {}
        }
    }

    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        match &self.screen {
            Screen::Menu => vec![("Enter", "open"), ("j/k", "move"), ("Esc", "close")],
            Screen::Check(c) => c.hints(),
            Screen::Prefs(p) => p.hints(),
            Screen::Profiles(p) => p.hints(),
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height < 5 || area.width < 20 {
            return;
        }
        match &self.screen {
            Screen::Menu => {
                put(buf, area.x, area.y, area.width, "Tools", bold());
                for (i, (name, about)) in MENU.iter().enumerate() {
                    let y = area.y + 2 + i as u16;
                    if y >= area.bottom() {
                        break;
                    }
                    let style = if i == self.menu {
                        rev()
                    } else {
                        Style::default()
                    };
                    let line = format!(" {} {name:<16}", i + 1);
                    put(buf, area.x, y, area.width, &line, style);
                    let x = area.x + line.chars().count() as u16;
                    if x < area.right() {
                        put(buf, x + 1, y, area.right() - x - 1, about, dim());
                    }
                }
            }
            Screen::Check(c) => c.render(area, buf),
            Screen::Prefs(p) => p.render(area, buf),
            Screen::Profiles(p) => p.render(area, buf),
        }
    }
}
