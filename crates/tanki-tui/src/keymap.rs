//! Key bindings: a table of (scope, key, action) rows. Defaults mirror
//! Anki's shortcuts (R4 §22) plus vim-style navigation. Actions and scopes
//! have stable names and keys parse from strings like `ctrl+shift+z`, so a
//! config file can remap them later ([`Keymap::from_rows`]).
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// Everywhere unless a screen binds the same key.
    Global,
    Decks,
    Overview,
    Review,
    Congrats,
    Zoom,
    /// Lists inside overlays (menus, choices).
    Menu,
}

impl Scope {
    pub const ALL: [Scope; 7] = [
        Scope::Global,
        Scope::Decks,
        Scope::Overview,
        Scope::Review,
        Scope::Congrats,
        Scope::Zoom,
        Scope::Menu,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Decks => "decks",
            Scope::Overview => "overview",
            Scope::Review => "review",
            Scope::Congrats => "congrats",
            Scope::Zoom => "zoom",
            Scope::Menu => "menu",
        }
    }
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }
    pub fn title(self) -> &'static str {
        match self {
            Scope::Global => "Everywhere",
            Scope::Decks => "Deck list",
            Scope::Overview => "Overview",
            Scope::Review => "Review",
            Scope::Congrats => "Finished",
            Scope::Zoom => "Image zoom",
            Scope::Menu => "Menus",
        }
    }
}

macro_rules! actions {
    ($($variant:ident = $name:literal, $desc:literal;)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Action {
            Flag(u8),
            $($variant,)*
        }
        impl Action {
            /// Every action, for the palette and name lookups.
            pub fn all() -> Vec<Action> {
                let mut all: Vec<Action> = vec![$(Action::$variant,)*];
                all.extend((1..=7).map(Action::Flag));
                all
            }
            pub fn name(self) -> String {
                match self {
                    Action::Flag(n) => format!("flag-{n}"),
                    $(Action::$variant => $name.to_owned(),)*
                }
            }
            pub fn description(self) -> String {
                match self {
                    Action::Flag(n) => format!("Toggle flag {n}"),
                    $(Action::$variant => $desc.to_owned(),)*
                }
            }
        }
    };
}

actions! {
    Quit = "quit", "Quit tanki";
    Help = "help", "Show keys for this screen";
    Palette = "palette", "Command palette";
    About = "about", "About tanki";
    Back = "back", "Back / close";
    DeckList = "deck-list", "Go to the deck list";
    Study = "study", "Study: overview / review toggle";
    Undo = "undo", "Undo";
    Redo = "redo", "Redo";
    CycleGraphics = "cycle-graphics", "Cycle the image protocol";
    Up = "up", "Move up / scroll up";
    Down = "down", "Move down / scroll down";
    PageUp = "page-up", "Page up";
    PageDown = "page-down", "Page down";
    Top = "top", "Go to the top";
    Bottom = "bottom", "Go to the bottom";
    Collapse = "collapse", "Collapse deck / go to parent";
    Expand = "expand", "Expand deck";
    ToggleCollapse = "toggle-collapse", "Collapse or expand deck";
    Select = "select", "Open / choose";
    StudyDeck = "study-deck", "Study the selected deck now";
    StudyNow = "study-now", "Study now";
    Unbury = "unbury", "Unbury cards";
    Refresh = "refresh", "Refresh";
    ShowAnswerOrGood = "show-answer", "Show answer / answer Good";
    Again = "again", "Answer Again";
    Hard = "hard", "Answer Hard";
    Good = "good", "Answer Good";
    Easy = "easy", "Answer Easy";
    More = "more", "More actions";
    Replay = "replay-audio", "Replay audio";
    PauseAudio = "pause-audio", "Pause / resume audio";
    SeekBack = "seek-back", "Audio -5s";
    SeekForward = "seek-forward", "Audio +5s";
    PlayNextButton = "play-next-button", "Play the next audio button";
    ToggleMark = "toggle-mark", "Mark / unmark note";
    BuryCard = "bury-card", "Bury card";
    BuryNote = "bury-note", "Bury note";
    SuspendCard = "suspend-card", "Suspend card";
    SuspendNote = "suspend-note", "Suspend note";
    Forget = "forget", "Reset card to new...";
    SetDueDate = "set-due-date", "Set due date...";
    DeleteNote = "delete-note", "Delete note...";
    ToggleAutoAdvance = "toggle-auto-advance", "Toggle auto advance";
    ZoomImage = "zoom-image", "Zoom images";
    OpenInBrowser = "open-in-browser", "Open card in the web browser";
    ToggleHints = "toggle-hints", "Show / hide hints";
    ToggleDetails = "toggle-details", "Show / hide details";
    FocusInput = "focus-input", "Focus the answer input";
    NextImage = "next-image", "Next image";
    PrevImage = "previous-image", "Previous image";
}

impl Action {
    pub fn from_name(name: &str) -> Option<Self> {
        Self::all().into_iter().find(|a| a.name() == name)
    }
}

/// A normalised key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Chord {
    pub fn plain(code: KeyCode) -> Self {
        Self {
            code,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }
    /// Normalises a crossterm event: letters are lower case with `shift`
    /// set when upper case or shifted; other characters drop `shift` (the
    /// character already says it: `*`, `@`, `!`). Releases are `None`.
    pub fn from_event(event: &KeyEvent) -> Option<Self> {
        if event.kind == KeyEventKind::Release {
            return None;
        }
        let m = event.modifiers;
        let mut chord = Chord {
            code: event.code,
            ctrl: m.contains(KeyModifiers::CONTROL),
            alt: m.contains(KeyModifiers::ALT),
            shift: m.contains(KeyModifiers::SHIFT),
        };
        match event.code {
            KeyCode::Char(c) if c.is_ascii_alphabetic() => {
                chord.shift |= c.is_ascii_uppercase();
                chord.code = KeyCode::Char(c.to_ascii_lowercase());
            }
            KeyCode::Char(_) => chord.shift = false,
            KeyCode::BackTab => {
                chord.code = KeyCode::Tab;
                chord.shift = true;
            }
            _ => {}
        }
        Some(chord)
    }
    /// Parses `ctrl+shift+z`, `space`, `f5`, `*`, `shift+a`, `ctrl+delete`.
    pub fn parse(spec: &str) -> Option<Self> {
        let mut chord = Chord::plain(KeyCode::Null);
        let parts: Vec<&str> = if spec == "+" {
            vec!["+"]
        } else if let Some(prefix) = spec.strip_suffix("++") {
            prefix.split('+').chain(["+"]).collect()
        } else {
            spec.split('+').collect()
        };
        let (key, mods) = parts.split_last()?;
        for m in mods {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "c" => chord.ctrl = true,
                "alt" | "a" | "m" => chord.alt = true,
                "shift" | "s" => chord.shift = true,
                _ => return None,
            }
        }
        let lower = key.to_ascii_lowercase();
        chord.code = match lower.as_str() {
            "space" => KeyCode::Char(' '),
            "enter" | "return" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backspace" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            f if f.len() > 1 && f.starts_with('f') && f[1..].parse::<u8>().is_ok() => {
                KeyCode::F(f[1..].parse().ok()?)
            }
            _ => {
                let mut chars = key.chars();
                let c = chars.next()?;
                if chars.next().is_some() {
                    return None;
                }
                if c.is_ascii_alphabetic() {
                    chord.shift |= c.is_ascii_uppercase();
                    KeyCode::Char(c.to_ascii_lowercase())
                } else {
                    chord.shift = false;
                    KeyCode::Char(c)
                }
            }
        };
        Some(chord)
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("Ctrl+")?;
        }
        if self.alt {
            f.write_str("Alt+")?;
        }
        if self.shift {
            f.write_str("Shift+")?;
        }
        match self.code {
            KeyCode::Char(' ') => f.write_str("Space"),
            KeyCode::Char(c)
                if (self.shift || self.ctrl || self.alt) && c.is_ascii_alphabetic() =>
            {
                write!(f, "{}", c.to_ascii_uppercase())
            }
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::Enter => f.write_str("Enter"),
            KeyCode::Esc => f.write_str("Esc"),
            KeyCode::Tab => f.write_str("Tab"),
            KeyCode::Backspace => f.write_str("Backspace"),
            KeyCode::Delete => f.write_str("Delete"),
            KeyCode::Up => f.write_str("Up"),
            KeyCode::Down => f.write_str("Down"),
            KeyCode::Left => f.write_str("Left"),
            KeyCode::Right => f.write_str("Right"),
            KeyCode::Home => f.write_str("Home"),
            KeyCode::End => f.write_str("End"),
            KeyCode::PageUp => f.write_str("PageUp"),
            KeyCode::PageDown => f.write_str("PageDown"),
            KeyCode::F(n) => write!(f, "F{n}"),
            other => write!(f, "{other:?}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub scope: Scope,
    pub chord: Chord,
    pub action: Action,
}

/// Default bindings as text rows: (scope, key, action).
pub const DEFAULTS: &[(&str, &str, &str)] = &[
    // Main window (R4 §22.1) plus tanki's own.
    ("global", "ctrl+q", "quit"),
    ("global", "ctrl+c", "quit"),
    ("global", "?", "help"),
    ("global", "f1", "help"),
    ("global", "f2", "about"),
    ("global", ":", "palette"),
    ("global", "ctrl+k", "palette"),
    ("global", "d", "deck-list"),
    ("global", "s", "study"),
    ("global", "ctrl+z", "undo"),
    ("global", "ctrl+shift+z", "redo"),
    // Ctrl+Shift+Z needs the kitty keyboard protocol; Ctrl+Y works everywhere.
    ("global", "ctrl+y", "redo"),
    ("global", "ctrl+g", "cycle-graphics"),
    ("global", "esc", "back"),
    // Deck list (R4 §4) with vim-style movement.
    ("decks", "up", "up"),
    ("decks", "k", "up"),
    ("decks", "down", "down"),
    ("decks", "j", "down"),
    ("decks", "pageup", "page-up"),
    ("decks", "pagedown", "page-down"),
    ("decks", "home", "top"),
    ("decks", "g", "top"),
    ("decks", "end", "bottom"),
    ("decks", "shift+g", "bottom"),
    ("decks", "left", "collapse"),
    ("decks", "h", "collapse"),
    ("decks", "right", "expand"),
    ("decks", "l", "expand"),
    ("decks", "space", "toggle-collapse"),
    ("decks", "tab", "toggle-collapse"),
    ("decks", "enter", "select"),
    ("decks", "s", "study-deck"),
    ("decks", "q", "quit"),
    ("decks", "r", "refresh"),
    // Overview (R4 §22.2).
    ("overview", "enter", "study-now"),
    ("overview", "space", "study-now"),
    ("overview", "s", "study-now"),
    ("overview", "u", "unbury"),
    ("overview", "q", "deck-list"),
    // Reviewer (R4 §22.3, reviewer.py `_shortcutKeys`).
    ("review", "space", "show-answer"),
    ("review", "enter", "show-answer"),
    ("review", "1", "again"),
    ("review", "2", "hard"),
    ("review", "3", "good"),
    ("review", "4", "easy"),
    ("review", "u", "undo"),
    ("review", "m", "more"),
    ("review", "r", "replay-audio"),
    ("review", "f5", "replay-audio"),
    ("review", "5", "pause-audio"),
    ("review", "6", "seek-back"),
    ("review", "7", "seek-forward"),
    ("review", "p", "play-next-button"),
    ("review", "ctrl+1", "flag-1"),
    ("review", "ctrl+2", "flag-2"),
    ("review", "ctrl+3", "flag-3"),
    ("review", "ctrl+4", "flag-4"),
    ("review", "ctrl+5", "flag-5"),
    ("review", "ctrl+6", "flag-6"),
    ("review", "ctrl+7", "flag-7"),
    // Ctrl+digit is not sent by most terminals without the kitty protocol.
    ("review", "alt+1", "flag-1"),
    ("review", "alt+2", "flag-2"),
    ("review", "alt+3", "flag-3"),
    ("review", "alt+4", "flag-4"),
    ("review", "alt+5", "flag-5"),
    ("review", "alt+6", "flag-6"),
    ("review", "alt+7", "flag-7"),
    ("review", "*", "toggle-mark"),
    ("review", "=", "bury-note"),
    ("review", "-", "bury-card"),
    ("review", "!", "suspend-note"),
    ("review", "@", "suspend-card"),
    ("review", "ctrl+alt+n", "forget"),
    ("review", "ctrl+shift+d", "set-due-date"),
    // What Ctrl+Shift+D arrives as without the kitty protocol.
    ("review", "ctrl+d", "set-due-date"),
    ("review", "ctrl+delete", "delete-note"),
    ("review", "ctrl+backspace", "delete-note"),
    ("review", "shift+a", "toggle-auto-advance"),
    ("review", "z", "zoom-image"),
    ("review", "w", "open-in-browser"),
    ("review", "x", "toggle-hints"),
    ("review", "shift+x", "toggle-details"),
    ("review", "tab", "focus-input"),
    ("review", "up", "up"),
    ("review", "k", "up"),
    ("review", "down", "down"),
    ("review", "j", "down"),
    ("review", "pageup", "page-up"),
    ("review", "pagedown", "page-down"),
    ("review", "home", "top"),
    ("review", "end", "bottom"),
    ("review", "q", "back"),
    // Congrats.
    ("congrats", "u", "unbury"),
    ("congrats", "r", "refresh"),
    ("congrats", "enter", "deck-list"),
    ("congrats", "q", "deck-list"),
    // Image zoom.
    ("zoom", "esc", "back"),
    ("zoom", "q", "back"),
    ("zoom", "z", "back"),
    ("zoom", "right", "next-image"),
    ("zoom", "l", "next-image"),
    ("zoom", "n", "next-image"),
    ("zoom", "left", "previous-image"),
    ("zoom", "h", "previous-image"),
    ("zoom", "shift+n", "previous-image"),
    // Menus and choosers.
    ("menu", "up", "up"),
    ("menu", "k", "up"),
    ("menu", "down", "down"),
    ("menu", "j", "down"),
    ("menu", "tab", "down"),
    ("menu", "shift+tab", "up"),
    ("menu", "home", "top"),
    ("menu", "end", "bottom"),
    ("menu", "enter", "select"),
    ("menu", "space", "select"),
    ("menu", "esc", "back"),
    ("menu", "q", "back"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeymapError(pub String);

impl fmt::Display for KeymapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone)]
pub struct Keymap {
    bindings: Vec<Binding>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::from_rows(DEFAULTS).expect("default keymap parses")
    }
}

impl Keymap {
    pub fn from_rows(rows: &[(&str, &str, &str)]) -> Result<Self, KeymapError> {
        let mut bindings = Vec::with_capacity(rows.len());
        for (scope, key, action) in rows {
            let scope = Scope::from_name(scope)
                .ok_or_else(|| KeymapError(format!("unknown scope {scope:?}")))?;
            let chord =
                Chord::parse(key).ok_or_else(|| KeymapError(format!("unknown key {key:?}")))?;
            let action = Action::from_name(action)
                .ok_or_else(|| KeymapError(format!("unknown action {action:?}")))?;
            bindings.push(Binding {
                scope,
                chord,
                action,
            });
        }
        Ok(Self { bindings })
    }
    /// Replaces every binding of `chord` in `scope` (a later config file).
    pub fn bind(&mut self, scope: Scope, chord: Chord, action: Action) {
        self.bindings
            .retain(|b| !(b.scope == scope && b.chord == chord));
        self.bindings.push(Binding {
            scope,
            chord,
            action,
        });
    }
    /// Removes a default or user binding (JSON `action: null`).
    pub fn unbind(&mut self, scope: Scope, chord: Chord) {
        self.bindings
            .retain(|binding| binding.scope != scope || binding.chord != chord);
    }
    /// The action for `chord`, trying `scopes` in order.
    pub fn lookup(&self, scopes: &[Scope], chord: &Chord) -> Option<Action> {
        scopes.iter().find_map(|scope| {
            self.bindings
                .iter()
                .find(|b| b.scope == *scope && b.chord == *chord)
                .map(|b| b.action)
        })
    }
    /// Keys bound to `action` in the first of `scopes` that has any.
    pub fn keys_for(&self, scopes: &[Scope], action: Action) -> Vec<Chord> {
        for scope in scopes {
            let keys: Vec<Chord> = self
                .bindings
                .iter()
                .filter(|b| b.scope == *scope && b.action == action)
                .map(|b| b.chord)
                .collect();
            if !keys.is_empty() {
                return keys;
            }
        }
        Vec::new()
    }
    /// Short key label, e.g. "Space" or "Ctrl+1".
    pub fn hint(&self, scopes: &[Scope], action: Action) -> String {
        self.keys_for(scopes, action)
            .first()
            .map(ToString::to_string)
            .unwrap_or_default()
    }
    /// (action, keys) for the help overlay: actions bound in `scope` whose
    /// keys are not shadowed by an earlier scope in `active`.
    pub fn help_rows(&self, scope: Scope, active: &[Scope]) -> Vec<(Action, Vec<Chord>)> {
        let earlier: Vec<Scope> = active
            .iter()
            .take_while(|s| **s != scope)
            .copied()
            .collect();
        let mut rows: Vec<(Action, Vec<Chord>)> = Vec::new();
        for b in self.bindings.iter().filter(|b| b.scope == scope) {
            if self.lookup(&earlier, &b.chord).is_some() {
                continue;
            }
            match rows.iter_mut().find(|(a, _)| *a == b.action) {
                Some((_, keys)) => keys.push(b.chord),
                None => rows.push((b.action, vec![b.chord])),
            }
        }
        rows
    }
    /// Actions reachable in the active scopes (for the palette).
    pub fn actions(&self, active: &[Scope]) -> Vec<Action> {
        let mut out = Vec::new();
        for scope in active {
            for (action, _) in self.help_rows(*scope, active) {
                if !out.contains(&action) {
                    out.push(action);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn normalises_shift_and_symbols() {
        let a = Chord::from_event(&ev(KeyCode::Char('A'), KeyModifiers::SHIFT)).unwrap();
        assert_eq!(a, Chord::parse("shift+a").unwrap());
        assert_eq!(
            Chord::from_event(&ev(KeyCode::Char('A'), KeyModifiers::NONE)).unwrap(),
            a
        );
        let star = Chord::from_event(&ev(KeyCode::Char('*'), KeyModifiers::SHIFT)).unwrap();
        assert_eq!(star, Chord::parse("*").unwrap());
        // Kitty reports Ctrl+Shift+Z as `Z` or `z` with both modifiers.
        let redo = Chord::parse("ctrl+shift+z").unwrap();
        for c in ['z', 'Z'] {
            let e = ev(
                KeyCode::Char(c),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            );
            assert_eq!(Chord::from_event(&e).unwrap(), redo);
        }
        let mut release = ev(KeyCode::Char('a'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert!(Chord::from_event(&release).is_none());
        assert_eq!(
            Chord::from_event(&ev(KeyCode::BackTab, KeyModifiers::SHIFT)).unwrap(),
            Chord::parse("shift+tab").unwrap()
        );
    }

    #[test]
    fn parses_and_displays() {
        for (spec, shown) in [
            ("ctrl+shift+z", "Ctrl+Shift+Z"),
            ("space", "Space"),
            ("f5", "F5"),
            ("ctrl+alt+n", "Ctrl+Alt+N"),
            ("ctrl+delete", "Ctrl+Delete"),
            ("+", "+"),
            ("ctrl++", "Ctrl++"),
            ("G", "Shift+G"),
        ] {
            assert_eq!(Chord::parse(spec).unwrap().to_string(), shown, "{spec}");
        }
        assert!(Chord::parse("hyper+x").is_none());
        assert!(Chord::parse("xy").is_none());
    }

    #[test]
    fn screen_scope_wins_over_global() {
        let km = Keymap::default();
        let s = Chord::parse("s").unwrap();
        assert_eq!(
            km.lookup(&[Scope::Decks, Scope::Global], &s),
            Some(Action::StudyDeck)
        );
        assert_eq!(
            km.lookup(&[Scope::Overview, Scope::Global], &s),
            Some(Action::StudyNow)
        );
        assert_eq!(
            km.lookup(&[Scope::Review, Scope::Global], &s),
            Some(Action::Study)
        );
        let u = Chord::parse("u").unwrap();
        assert_eq!(
            km.lookup(&[Scope::Review, Scope::Global], &u),
            Some(Action::Undo)
        );
        assert_eq!(
            km.lookup(&[Scope::Overview, Scope::Global], &u),
            Some(Action::Unbury)
        );
        assert_eq!(km.lookup(&[Scope::Decks, Scope::Global], &u), None);
        let one = Chord::parse("1").unwrap();
        assert_eq!(
            km.lookup(&[Scope::Review, Scope::Global], &one),
            Some(Action::Again)
        );
        assert_eq!(km.lookup(&[Scope::Decks, Scope::Global], &one), None);
        let flag = Chord::parse("ctrl+3").unwrap();
        assert_eq!(km.lookup(&[Scope::Review], &flag), Some(Action::Flag(3)));
        let esc = Chord::parse("esc").unwrap();
        assert_eq!(
            km.lookup(&[Scope::Zoom, Scope::Global], &esc),
            Some(Action::Back)
        );
    }

    #[test]
    fn reviewer_defaults_match_anki() {
        let km = Keymap::default();
        let review = [Scope::Review, Scope::Global];
        for (key, action) in [
            ("space", Action::ShowAnswerOrGood),
            ("enter", Action::ShowAnswerOrGood),
            ("4", Action::Easy),
            ("m", Action::More),
            ("r", Action::Replay),
            ("f5", Action::Replay),
            ("5", Action::PauseAudio),
            ("6", Action::SeekBack),
            ("7", Action::SeekForward),
            ("*", Action::ToggleMark),
            ("=", Action::BuryNote),
            ("-", Action::BuryCard),
            ("!", Action::SuspendNote),
            ("@", Action::SuspendCard),
            ("ctrl+alt+n", Action::Forget),
            ("ctrl+shift+d", Action::SetDueDate),
            ("ctrl+delete", Action::DeleteNote),
            ("shift+a", Action::ToggleAutoAdvance),
            ("ctrl+z", Action::Undo),
            ("ctrl+shift+z", Action::Redo),
            ("ctrl+7", Action::Flag(7)),
        ] {
            assert_eq!(
                km.lookup(&review, &Chord::parse(key).unwrap()),
                Some(action),
                "{key}"
            );
        }
    }

    #[test]
    fn help_rows_hide_shadowed_keys_and_names_round_trip() {
        let km = Keymap::default();
        let active = [Scope::Decks, Scope::Global];
        let global = km.help_rows(Scope::Global, &active);
        // `s` is shadowed by the deck list's study-deck.
        assert!(!global.iter().any(|(a, _)| *a == Action::Study));
        assert!(global.iter().any(|(a, _)| *a == Action::Help));
        let decks = km.help_rows(Scope::Decks, &active);
        let up = decks.iter().find(|(a, _)| *a == Action::Up).unwrap();
        assert_eq!(up.1.len(), 2);
        for action in Action::all() {
            assert_eq!(Action::from_name(&action.name()), Some(action));
        }
        assert_eq!(km.hint(&[Scope::Review], Action::Flag(2)), "Ctrl+2");
        assert!(km.actions(&active).contains(&Action::StudyDeck));
        assert!(!km.actions(&active).contains(&Action::Again));
    }

    #[test]
    fn remapping() {
        let mut km = Keymap::from_rows(&[("review", "space", "show-answer")]).unwrap();
        let x = Chord::parse("x").unwrap();
        km.bind(Scope::Review, x, Action::Good);
        assert_eq!(km.lookup(&[Scope::Review], &x), Some(Action::Good));
        km.bind(Scope::Review, x, Action::Easy);
        assert_eq!(km.lookup(&[Scope::Review], &x), Some(Action::Easy));
        km.unbind(Scope::Review, x);
        assert_eq!(km.lookup(&[Scope::Review], &x), None);
        assert!(Keymap::from_rows(&[("nowhere", "x", "good")]).is_err());
        assert!(Keymap::from_rows(&[("review", "x", "fly")]).is_err());
    }
}
