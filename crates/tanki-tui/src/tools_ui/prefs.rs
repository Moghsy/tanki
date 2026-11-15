//! The collection-stored preferences form (PLAN 8.13). Desktop's Qt-only
//! settings (video driver, UI scale, window geometry) do not apply.
use super::{bold, dim, is_cancel, put, rev, run, Result, Step, ToolsIntent};
use crate::backend::Backend;
use crate::input::TextInput;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{buffer::Buffer, layout::Rect, style::Style};
use tanki_core::maintenance::{
    MaintenanceCommand as Cmd, MaintenanceResponse as Resp, Preferences,
};

enum Kind {
    Heading,
    Bool(fn(&Preferences) -> bool, fn(&mut Preferences, bool)),
    Number(fn(&Preferences) -> u32, fn(&mut Preferences, u32), u32),
    Text(fn(&Preferences) -> String, fn(&mut Preferences, String)),
    Choice(
        &'static [&'static str],
        fn(&Preferences) -> i32,
        fn(&mut Preferences, i32),
    ),
}

struct Row {
    label: &'static str,
    kind: Kind,
}

const fn row(label: &'static str, kind: Kind) -> Row {
    Row { label, kind }
}

const MIX: &[&str] = &[
    "Mix new cards and reviews",
    "Show after reviews",
    "Show before reviews",
];

fn rows() -> Vec<Row> {
    use Kind::*;
    vec![
        row("Scheduling", Heading),
        row(
            "Next day starts at (hour)",
            Number(|p| p.rollover, |p, v| p.rollover = v, 23),
        ),
        row(
            "Learn ahead limit (seconds)",
            Number(
                |p| p.learn_ahead_secs,
                |p, v| p.learn_ahead_secs = v,
                86_400,
            ),
        ),
        row(
            "New/review order",
            Choice(MIX, |p| p.new_review_mix, |p, v| p.new_review_mix = v),
        ),
        row(
            "Show day-learning cards first (v2)",
            Bool(|p| p.day_learn_first, |p, v| p.day_learn_first = v),
        ),
        row(
            "New timezone support (v2)",
            Bool(|p| p.new_timezone, |p, v| p.new_timezone = v),
        ),
        row("Reviewing", Heading),
        row(
            "Show remaining card count",
            Bool(
                |p| p.show_remaining_due_counts,
                |p, v| p.show_remaining_due_counts = v,
            ),
        ),
        row(
            "Show next review time above buttons",
            Bool(
                |p| p.show_intervals_on_buttons,
                |p, v| p.show_intervals_on_buttons = v,
            ),
        ),
        row(
            "Hide audio play buttons",
            Bool(
                |p| p.hide_audio_play_buttons,
                |p, v| p.hide_audio_play_buttons = v,
            ),
        ),
        row(
            "Interrupt current audio when answering",
            Bool(
                |p| p.interrupt_audio_when_answering,
                |p, v| p.interrupt_audio_when_answering = v,
            ),
        ),
        row(
            "Timebox time limit (seconds, 0 = off)",
            Number(|p| p.time_limit_secs, |p, v| p.time_limit_secs = v, 86_400),
        ),
        row(
            "Load balancer",
            Bool(
                |p| p.load_balancer_enabled,
                |p, v| p.load_balancer_enabled = v,
            ),
        ),
        row(
            "FSRS short-term scheduling with steps",
            Bool(
                |p| p.fsrs_short_term_with_steps_enabled,
                |p, v| p.fsrs_short_term_with_steps_enabled = v,
            ),
        ),
        row("Editing", Heading),
        row(
            "Add to current deck by default",
            Bool(
                |p| p.adding_defaults_to_current_deck,
                |p, v| p.adding_defaults_to_current_deck = v,
            ),
        ),
        row(
            "Paste clipboard images as PNG",
            Bool(|p| p.paste_images_as_png, |p, v| p.paste_images_as_png = v),
        ),
        row(
            "Paste without shift key strips formatting",
            Bool(
                |p| p.paste_strips_formatting,
                |p, v| p.paste_strips_formatting = v,
            ),
        ),
        row(
            "Default search text",
            Text(
                |p| p.default_search_text.clone(),
                |p, v| p.default_search_text = v,
            ),
        ),
        row(
            "Ignore accents in search",
            Bool(
                |p| p.ignore_accents_in_search,
                |p, v| p.ignore_accents_in_search = v,
            ),
        ),
        row(
            "Generate LaTeX images",
            Bool(|p| p.render_latex, |p, v| p.render_latex = v),
        ),
        row("Backups", Heading),
        row(
            "Daily backups to keep",
            Number(|p| p.backup_daily, |p, v| p.backup_daily = v, 9999),
        ),
        row(
            "Weekly backups to keep",
            Number(|p| p.backup_weekly, |p, v| p.backup_weekly = v, 9999),
        ),
        row(
            "Monthly backups to keep",
            Number(|p| p.backup_monthly, |p, v| p.backup_monthly = v, 9999),
        ),
        row(
            "Minimum minutes between backups",
            Number(
                |p| p.backup_minimum_interval_mins,
                |p, v| p.backup_minimum_interval_mins = v,
                9999,
            ),
        ),
    ]
}

pub(crate) struct PrefsUi {
    rows: Vec<Row>,
    original: Preferences,
    prefs: Preferences,
    selected: usize,
    editing: Option<TextInput>,
    discard_ask: bool,
    status: Option<String>,
}

impl PrefsUi {
    pub fn load(backend: &dyn Backend) -> Result<Self> {
        let Resp::Preferences(prefs) = run(backend, Cmd::GetPreferences)? else {
            return Err(super::unexpected());
        };
        let rows = rows();
        let selected = rows
            .iter()
            .position(|r| !matches!(r.kind, Kind::Heading))
            .unwrap_or(0);
        Ok(Self {
            rows,
            original: prefs.clone(),
            prefs,
            selected,
            editing: None,
            discard_ask: false,
            status: None,
        })
    }

    pub fn is_dirty(&self) -> bool {
        self.prefs != self.original
    }

    fn step(&mut self, down: bool) {
        let mut i = self.selected;
        loop {
            let next = if down { i + 1 } else { i.wrapping_sub(1) };
            if next >= self.rows.len() {
                return;
            }
            i = next;
            if !matches!(self.rows[i].kind, Kind::Heading) {
                self.selected = i;
                return;
            }
        }
    }

    fn value(&self, row: &Row) -> String {
        match &row.kind {
            Kind::Heading => String::new(),
            Kind::Bool(get, _) => if get(&self.prefs) { "[x]" } else { "[ ]" }.into(),
            Kind::Number(get, ..) => get(&self.prefs).to_string(),
            Kind::Text(get, _) => {
                let t = get(&self.prefs);
                if t.is_empty() {
                    "(none)".into()
                } else {
                    t
                }
            }
            Kind::Choice(names, get, _) => names
                .get(get(&self.prefs) as usize)
                .copied()
                .unwrap_or("?")
                .into(),
        }
    }

    pub fn paste(&mut self, text: &str) {
        if let Some(input) = &mut self.editing {
            input.insert_str(text);
        }
    }

    fn save(&mut self, backend: &dyn Backend) -> Step {
        match run(backend, Cmd::SetPreferences(self.prefs.clone())) {
            Ok(Resp::PreferencesSaved(changes)) => {
                self.original = self.prefs.clone();
                self.status = Some("Preferences saved.".into());
                Step::Intent(ToolsIntent::Changed(changes))
            }
            Ok(_) => {
                self.status = Some(super::unexpected().to_string());
                Step::Stay
            }
            // Anki's own message (for example an invalid rollover hour).
            Err(error) => {
                self.status = Some(error.to_string());
                Step::Stay
            }
        }
    }

    pub fn on_key(&mut self, key: KeyEvent, backend: &dyn Backend) -> Step {
        if self.discard_ask {
            self.discard_ask = false;
            return if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                Step::Back
            } else {
                Step::Stay
            };
        }
        if let Some(input) = &mut self.editing {
            if is_cancel(&key) {
                self.editing = None;
            } else if key.code == KeyCode::Enter {
                let text = input.text().to_owned();
                self.editing = None;
                self.commit(&text);
            } else {
                input.handle_key(&key);
            }
            return Step::Stay;
        }
        self.status = None;
        match key.code {
            // `s` and Ctrl-S both save.
            KeyCode::Char('s' | 'S') => return self.save(backend),
            KeyCode::Esc | KeyCode::Char('q') => {
                if self.is_dirty() {
                    self.discard_ask = true;
                    return Step::Stay;
                }
                return Step::Back;
            }
            KeyCode::Up | KeyCode::Char('k') => self.step(false),
            KeyCode::Down | KeyCode::Char('j') => self.step(true),
            KeyCode::Left | KeyCode::Char('h') => self.cycle(false),
            KeyCode::Right | KeyCode::Char('l') => self.cycle(true),
            KeyCode::Char(' ') | KeyCode::Enter | KeyCode::Char('x') => self.activate(),
            _ => {}
        }
        Step::Stay
    }

    fn cycle(&mut self, forward: bool) {
        if let Kind::Choice(names, get, set) = &self.rows[self.selected].kind {
            let n = names.len() as i32;
            let next = (get(&self.prefs) + if forward { 1 } else { n - 1 }) % n;
            set(&mut self.prefs, next);
        } else if let Kind::Bool(..) = &self.rows[self.selected].kind {
            self.activate();
        }
    }

    fn activate(&mut self) {
        match &self.rows[self.selected].kind {
            Kind::Heading => {}
            Kind::Bool(get, set) => {
                let v = !get(&self.prefs);
                set(&mut self.prefs, v);
            }
            Kind::Number(get, ..) => {
                self.editing = Some(TextInput::new(get(&self.prefs).to_string()))
            }
            Kind::Text(get, _) => self.editing = Some(TextInput::new(get(&self.prefs))),
            Kind::Choice(..) => self.cycle(true),
        }
    }

    fn commit(&mut self, text: &str) {
        match &self.rows[self.selected].kind {
            Kind::Number(_, set, max) => match text.trim().parse::<u32>() {
                Ok(v) if v <= *max => set(&mut self.prefs, v),
                _ => self.status = Some(format!("Enter a whole number from 0 to {max}.")),
            },
            Kind::Text(_, set) => set(&mut self.prefs, text.to_owned()),
            _ => {}
        }
    }

    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.discard_ask {
            return vec![("y", "discard"), ("any", "keep editing")];
        }
        if self.editing.is_some() {
            return vec![("Enter", "apply"), ("Esc", "cancel")];
        }
        vec![
            ("Space", "toggle/edit"),
            ("←/→", "change"),
            ("s", "save"),
            ("Esc", "back"),
        ]
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let title = format!(
            "Preferences{}",
            if self.is_dirty() { "  [modified]" } else { "" }
        );
        put(buf, area.x, area.y, area.width, &title, bold());
        let rows_visible = area.height.saturating_sub(4) as usize;
        let first = self
            .selected
            .saturating_sub(rows_visible.saturating_sub(1))
            .min(self.rows.len().saturating_sub(rows_visible));
        let label_width = (area.width as usize).saturating_sub(34).clamp(20, 44);
        for (i, row) in self.rows.iter().enumerate().skip(first).take(rows_visible) {
            let y = area.y + 2 + (i - first) as u16;
            if matches!(row.kind, Kind::Heading) {
                put(buf, area.x, y, area.width, row.label, bold());
                continue;
            }
            let style = if i == self.selected {
                rev()
            } else {
                Style::default()
            };
            let value = match (&self.editing, i == self.selected) {
                (Some(input), true) => format!("{}▏", input.text()),
                _ => self.value(row),
            };
            let line = format!("  {:<w$} {value}", row.label, w = label_width);
            put(buf, area.x, y, area.width, &line, style);
        }
        let bottom = area.bottom() - 1;
        if self.discard_ask {
            put(
                buf,
                area.x,
                bottom,
                area.width,
                "Discard unsaved changes? y/n",
                rev(),
            );
        } else if let Some(status) = &self.status {
            put(buf, area.x, bottom, area.width, status, dim());
        }
    }
}
