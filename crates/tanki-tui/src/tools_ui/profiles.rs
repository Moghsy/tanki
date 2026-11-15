//! Profiles: the base folder's subfolders holding a collection (PLAN 8.13).
//! Folder operations only; nothing here opens a collection, reads
//! `prefs21.db` or touches `addons21/`.
use super::{bold, dim, is_cancel, put, rev, Step, ToolsIntent};
use crate::input::TextInput;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{buffer::Buffer, layout::Rect, style::Style};
use std::path::PathBuf;
use tanki_core::maintenance::profiles::{self, ProfileInfo};

enum Prompt {
    New(TextInput),
    Rename {
        old: String,
        input: TextInput,
    },
    /// Typed confirmation: the name must be entered in full.
    Delete {
        name: String,
        input: TextInput,
    },
}

pub(crate) struct ProfilesUi {
    base: Option<PathBuf>,
    open: Option<String>,
    config: Option<PathBuf>,
    list: Vec<ProfileInfo>,
    selected: usize,
    prompt: Option<Prompt>,
    status: Option<String>,
}

impl ProfilesUi {
    pub fn new(base: Option<PathBuf>, open: Option<String>, config: Option<PathBuf>) -> Self {
        let mut ui = Self {
            base,
            open,
            config,
            list: Vec::new(),
            selected: 0,
            prompt: None,
            status: None,
        };
        ui.reload();
        ui
    }

    fn reload(&mut self) {
        let Some(base) = &self.base else {
            self.status = Some("No Anki base folder is known.".into());
            return;
        };
        match profiles::list(base) {
            Ok(list) => {
                self.list = list;
                self.selected = self.selected.min(self.list.len().saturating_sub(1));
            }
            Err(e) => self.status = Some(e.to_string()),
        }
    }

    fn current(&self) -> Option<&ProfileInfo> {
        self.list.get(self.selected)
    }

    pub fn paste(&mut self, text: &str) {
        match &mut self.prompt {
            Some(Prompt::New(i)) => i.insert_str(text),
            Some(Prompt::Rename { input, .. } | Prompt::Delete { input, .. }) => {
                input.insert_str(text)
            }
            None => {}
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Step {
        if let Some(prompt) = &mut self.prompt {
            if is_cancel(&key) {
                self.prompt = None;
            } else if key.code == KeyCode::Enter {
                let prompt = self.prompt.take().unwrap();
                self.apply(prompt);
            } else {
                match prompt {
                    Prompt::New(i)
                    | Prompt::Rename { input: i, .. }
                    | Prompt::Delete { input: i, .. } => {
                        i.handle_key(&key);
                    }
                }
            }
            return Step::Stay;
        }
        self.status = None;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Step::Back,
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(self.list.len().saturating_sub(1))
            }
            KeyCode::Char('n') if self.base.is_some() => {
                self.prompt = Some(Prompt::New(TextInput::new("")))
            }
            KeyCode::Char('r') => {
                if let Some(p) = self.current().cloned() {
                    if self.open.as_deref() == Some(p.name.as_str()) {
                        self.status = Some(
                            "This profile is open; switch to another one to rename it.".into(),
                        );
                    } else {
                        self.prompt = Some(Prompt::Rename {
                            input: TextInput::new(p.name.clone()),
                            old: p.name,
                        });
                    }
                }
            }
            KeyCode::Char('d') => {
                if let Some(p) = self.current().cloned() {
                    if self.open.as_deref() == Some(p.name.as_str()) {
                        self.status = Some("This profile is open and cannot be deleted.".into());
                    } else {
                        self.prompt = Some(Prompt::Delete {
                            name: p.name,
                            input: TextInput::new(""),
                        });
                    }
                }
            }
            KeyCode::Enter => {
                if let Some(p) = self.current() {
                    if self.open.as_deref() == Some(p.name.as_str()) {
                        self.status = Some("Already open.".into());
                    } else {
                        return Step::Intent(ToolsIntent::SwitchProfile(p.name.clone()));
                    }
                }
            }
            _ => {}
        }
        Step::Stay
    }

    fn apply(&mut self, prompt: Prompt) {
        let Some(base) = self.base.clone() else {
            return;
        };
        let config = self.config.as_deref();
        let result = match prompt {
            Prompt::New(input) => {
                let name = input.text().to_owned();
                profiles::create(&base, &name).map(|_| format!("Created profile {name}."))
            }
            Prompt::Rename { old, input } => {
                let new = input.text().to_owned();
                profiles::rename(&base, &old, &new, config)
                    .map(|_| format!("Renamed {old} to {new}."))
            }
            Prompt::Delete { name, input } => {
                profiles::delete(&base, &name, input.text(), self.open.as_deref(), config)
                    .map(|to| format!("Deleted {name}; its folder is now {}", to.display()))
            }
        };
        self.status = Some(match result {
            Ok(message) => message,
            Err(error) => error.to_string(),
        });
        self.reload();
    }

    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.prompt.is_some() {
            return vec![("Enter", "apply"), ("Esc", "cancel")];
        }
        vec![
            ("Enter", "switch"),
            ("n", "new"),
            ("r", "rename"),
            ("d", "delete"),
            ("Esc", "back"),
        ]
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        put(buf, area.x, area.y, area.width, "Profiles", bold());
        if let Some(base) = &self.base {
            put(
                buf,
                area.x,
                area.y + 1,
                area.width,
                &base.display().to_string(),
                dim(),
            );
        }
        for (i, p) in self.list.iter().enumerate() {
            let y = area.y + 3 + i as u16;
            if y >= area.bottom().saturating_sub(3) {
                break;
            }
            let open = self.open.as_deref() == Some(p.name.as_str());
            let style = if i == self.selected {
                rev()
            } else {
                Style::default()
            };
            let line = format!(
                " {} {:<30} {:>9}",
                if open { "*" } else { " " },
                p.name,
                human(p.collection_bytes)
            );
            put(buf, area.x, y, area.width, &line, style);
        }
        let y = area.bottom().saturating_sub(2);
        match &self.prompt {
            Some(Prompt::New(i)) => put(
                buf,
                area.x,
                y,
                area.width,
                &format!("New profile name: {}▏", i.text()),
                Style::default(),
            ),
            Some(Prompt::Rename { old, input }) => put(
                buf,
                area.x,
                y,
                area.width,
                &format!("Rename {old} to: {}▏", input.text()),
                Style::default(),
            ),
            Some(Prompt::Delete { name, input }) => put(
                buf,
                area.x,
                y,
                area.width,
                &format!(
                    "Delete {name}? Its folder moves to .tanki-trash. Type the name to confirm: {}▏",
                    input.text()
                ),
                Style::default(),
            ),
            None => {}
        }
        if let Some(status) = &self.status {
            put(buf, area.x, area.bottom() - 1, area.width, status, dim());
        }
    }
}

fn human(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} GiB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MiB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1} KiB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}
