//! Application state: screens, overlays, the status line and the event
//! dispatcher. Drawing lives in `ui.rs`; the terminal loop in `terminal.rs`.
use crate::add_ui::AddUi;
use crate::backend::{self, Backend, DeckRow, Tracked};
use crate::browser_ui::BrowserUi;
use crate::deck_options_ui::DeckOptionsUi;
use crate::deck_tools_ui::DeckPromptKind;
use crate::deck_tools_ui::{CustomStudyUi, DeckDescriptionUi, DeckPromptUi, FilteredDeckUi};
use crate::editor_flow::{EditorExit, FlowOptions};
use crate::input::TextInput;
use crate::keymap::{Action, Chord, Keymap, Scope};
use crate::notetypes_ui::{ChangeNotetypeUi, NotetypesUi};
use crate::package_ui::PackageUi;
use crate::reviewer::{self, Effect, Now, Phase, Reviewer, SCHEDULER_UPGRADE_MESSAGE};
use crate::services::{self, AudioPlayer, CardRenderer, Display, ImageDisplay};
use crate::stats_ui::{CardInfoUi, StatsUi};
use crate::sync_ui::SyncUi;
use crate::tools_ui::ToolsUi;
use crate::transfer_ui::{DuplicatesUi, ExportUi, ImportUi};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tanki_core::{BackupInfo, Congrats, Deck, DeckOverview, OpChanges, Rating, UnburyMode};
use tanki_media::AudioEvent;
use tanki_render::WidgetKind;

/// How long a status message stays.
pub const MESSAGE_SECS: u64 = 5;
/// The congrats screen re-polls this often (`CongratsPage.svelte`).
pub const CONGRATS_POLL_SECS: u64 = 60;

mod plugins;

pub type Opener = Box<dyn Fn(&Path) -> std::io::Result<()>>;
/// Runs the editor command (program and arguments) on a file and waits. The
/// terminal loop's runner suspends the screen around it; tests inject one.
pub type EditorRunner = Box<dyn FnMut(&[String], &Path) -> std::io::Result<EditorExit>>;

pub struct Services {
    pub renderer: Box<dyn CardRenderer>,
    pub images: Box<dyn ImageDisplay>,
    pub audio: Box<dyn AudioPlayer>,
    pub display: Display,
    /// Opens a file in the desktop's browser (the escape hatch).
    pub opener: Opener,
}

#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub dir: PathBuf,
    pub media_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    Decks,
    /// Study the current deck (already selected by the caller).
    Review,
}

pub enum AppEvent {
    /// A key and when the reader received it.
    Key(KeyEvent, Instant),
    Paste(String),
    Mouse(MouseEvent),
    Resize,
    FocusGained,
    FocusLost,
    Changes(OpChanges),
    Audio(AudioEvent),
    ImageReady,
    Tick,
}

#[derive(Debug, Default)]
pub struct DecksView {
    pub tree: Option<Deck>,
    pub rows: Vec<DeckRow>,
    pub selected: usize,
    pub offset: usize,
    pub studied: Option<String>,
    pub error: Option<String>,
    pub list_area: Rect,
    /// The `/` fuzzy filter: the query and whether it is still being typed.
    pub filter: Option<DeckFilter>,
}

#[derive(Debug, Default)]
pub struct DeckFilter {
    pub input: TextInput,
    pub editing: bool,
}

impl DecksView {
    /// Recomputes `rows` from the tree: the collapsed tree, or the fuzzy
    /// matches (best first) while a filter is set.
    pub fn rebuild_rows(&mut self) {
        let Some(tree) = &self.tree else {
            return;
        };
        self.rows = match &self.filter {
            Some(f) if !f.input.text().trim().is_empty() => {
                let all = backend::all_rows(tree);
                let order =
                    crate::fuzzy::rank(f.input.text(), all.iter().map(|r| r.full_name.as_str()));
                order.into_iter().map(|i| all[i].clone()).collect()
            }
            Some(_) => backend::all_rows(tree),
            None => backend::visible_rows(tree),
        };
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }
}

#[derive(Debug, Default)]
pub struct OverviewView {
    pub data: Option<DeckOverview>,
    pub error: Option<String>,
}

pub struct CongratsView {
    pub data: Option<Congrats>,
    pub fetched_at: Instant,
    pub error: Option<String>,
}

pub struct RenderCache {
    pub key: u64,
    pub card: tanki_render::RenderedCard,
}

pub struct ReviewView {
    pub reviewer: Reviewer,
    pub scroll: usize,
    pub warning: Option<String>,
    pub input_focused: bool,
    pub render: Option<RenderCache>,
    pub replay_next: usize,
    /// Where the card was drawn last; also the size used to lay it out.
    pub card_area: Rect,
}

pub enum Screen {
    Decks(DecksView),
    Overview(OverviewView),
    Review(Box<ReviewView>),
    Congrats(CongratsView),
}

impl Screen {
    pub fn scope(&self) -> Scope {
        match self {
            Screen::Decks(_) => Scope::Decks,
            Screen::Overview(_) => Scope::Overview,
            Screen::Review(_) => Scope::Review,
            Screen::Congrats(_) => Scope::Congrats,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MenuItem {
    pub label: String,
    pub key: String,
    pub action: Action,
    pub checked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    SetDueDate,
    ImportPath,
    ExportPath,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKind {
    DeleteNote,
    UpgradeScheduler,
    /// Quit although the add / edit screen has unsaved input.
    QuitDiscard,
}

/// A full-screen widget over the current screen (desktop opens these as
/// windows): the screen below keeps its state, e.g. the reviewer's card.
/// The top layer gets the keys and is drawn instead of the screen.
pub enum Layer {
    Add(Box<AddUi>),
    Browser(Box<BrowserUi>),
    DeckOptions(Box<DeckOptionsUi>),
    Notetypes(Box<NotetypesUi>),
    ChangeNotetype(Box<ChangeNotetypeUi>),
    /// A deck tools dialog, drawn over the layer or screen below.
    DeckTools(Box<DeckToolsLayer>),
    Stats(Box<StatsUi>),
    CardInfo(Box<CardInfoUi>),
    Tools(Box<ToolsUi>),
    Transfer(Box<TransferLayer>),
}

/// CSV / JSON import, text export and find duplicates (PLAN 8.11).
pub enum TransferLayer {
    Import(Box<ImportUi>),
    Export(ExportUi),
    Duplicates(DuplicatesUi),
}

/// The deck tools dialogs (PLAN 8.2).
pub enum DeckToolsLayer {
    Filtered(FilteredDeckUi),
    CustomStudy(CustomStudyUi),
    Prompt(DeckPromptUi),
    Description(DeckDescriptionUi),
}

impl Layer {
    pub fn scope(&self) -> Scope {
        match self {
            Layer::Add(_) => Scope::Add,
            Layer::Browser(_) => Scope::Browser,
            _ => Scope::Manage,
        }
    }

    /// Drawn as a box over what is below instead of full screen.
    pub fn is_dialog(&self) -> bool {
        matches!(self, Layer::DeckTools(_))
    }

    /// Asks before quitting when this is true.
    pub fn has_unsaved_input(&self) -> bool {
        match self {
            Layer::Add(ui) => ui.has_unsaved_input(),
            Layer::DeckOptions(ui) => ui.is_dirty(),
            Layer::Notetypes(ui) => ui.is_dirty(),
            _ => false,
        }
    }

    /// Another part of the app changed the collection.
    pub fn on_changes(&mut self, backend: &dyn Backend, changes: &OpChanges) {
        match self {
            Layer::Add(ui) => ui.on_changes(backend, changes),
            Layer::Browser(ui) => ui.on_changes(backend, changes),
            Layer::Notetypes(ui) => ui.on_changes(backend, changes),
            Layer::Stats(ui) => ui.on_changes(backend, changes),
            Layer::CardInfo(ui) => {
                let _ = ui.refresh(backend);
            }
            Layer::DeckOptions(_)
            | Layer::ChangeNotetype(_)
            | Layer::DeckTools(_)
            | Layer::Tools(_)
            | Layer::Transfer(_) => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceValue {
    Unbury(UnburyMode),
    TimeboxContinue,
    TimeboxFinish,
    /// A backup from the list offered by `restore-backup` (index).
    Backup(usize),
    /// An entry of the plugin action list (index).
    PluginAction(usize),
    Cancel,
}

pub enum Overlay {
    Help {
        scroll: usize,
    },
    About,
    Palette {
        input: TextInput,
        selected: usize,
    },
    Menu {
        title: String,
        items: Vec<MenuItem>,
        selected: usize,
    },
    Prompt {
        title: String,
        input: TextInput,
        kind: PromptKind,
    },
    Confirm {
        message: String,
        kind: ConfirmKind,
        yes: bool,
    },
    Choice {
        title: String,
        message: String,
        options: Vec<(String, ChoiceValue)>,
        selected: usize,
        /// Esc picks this one.
        cancel: ChoiceValue,
    },
    Forget {
        restore: bool,
        reset: bool,
        row: usize,
    },
    Zoom {
        index: usize,
    },
}

#[derive(Debug, Clone)]
pub struct Message {
    pub text: String,
    pub error: bool,
    pub at: Instant,
}

pub struct App<'a> {
    pub(crate) backend: Tracked<'a>,
    pub(crate) services: Services,
    pub(crate) profile: Profile,
    pub(crate) keymap: Keymap,
    pub screen: Screen,
    pub overlay: Option<Overlay>,
    pub(crate) message: Option<Message>,
    pub(crate) totals: Option<(u32, u32, u32)>,
    pub(crate) current_deck: Option<i64>,
    /// The reviewer's card shown now and the one before (Previous Card Info).
    pub(crate) shown_card: Option<i64>,
    pub(crate) previous_card: Option<i64>,
    /// The Anki base folder (the Tools screen's profiles).
    pub(crate) base_dir: Option<PathBuf>,
    /// Set by the Tools screen: quit and reopen on this profile.
    pub(crate) switch_to: Option<String>,
    pub(crate) focused: bool,
    pub(crate) quit: bool,
    pub(crate) now: Now,
    next_day_at: Option<i64>,
    ignore_keys_before: Option<Instant>,
    cleanup: String,
    full_redraw: bool,
    /// Path of tanki's config.toml; `None` means sync is unavailable.
    config_path: Option<PathBuf>,
    render_tiers: std::collections::BTreeMap<String, tanki_render::tier::RenderTier>,
    classified: std::collections::BTreeMap<i64, tanki_render::tier::Classification>,
    pub(crate) sync_ui: SyncUi,
    pub(crate) package_ui: Option<PackageUi>,
    job: Option<data::Job>,
    /// The list behind the open restore chooser.
    backups: Vec<BackupInfo>,
    /// Add / edit and browser screens over `screen`, topmost last.
    pub layers: Vec<Layer>,
    pub(crate) editor: EditorRunner,
    plugins: Option<Box<dyn crate::plugins::PluginPort>>,
    plugin_hold: Option<plugins::PluginHold>,
    plugin_hint: Option<plugins::PluginHint>,
    plugin_action_list: Vec<(String, String, String)>,
    /// Last card whose `answer_revealed` hook was sent.
    reveal_fired: Option<i64>,
    /// Where the editor flows put their private temporary folders.
    pub(crate) flow_options: FlowOptions,
}

impl<'a> App<'a> {
    pub fn new(
        backend: &'a dyn Backend,
        services: Services,
        profile: Profile,
        start: Start,
        now: Now,
    ) -> Self {
        let mut app = Self {
            backend: Tracked::new(backend),
            services,
            profile,
            keymap: Keymap::default(),
            screen: Screen::Decks(DecksView::default()),
            overlay: None,
            message: None,
            totals: None,
            current_deck: None,
            shown_card: None,
            previous_card: None,
            base_dir: None,
            switch_to: None,
            focused: true,
            quit: false,
            now,
            next_day_at: None,
            ignore_keys_before: None,
            cleanup: String::new(),
            full_redraw: false,
            config_path: None,
            render_tiers: Default::default(),
            classified: Default::default(),
            sync_ui: SyncUi::default(),
            package_ui: None,
            job: None,
            backups: Vec::new(),
            layers: Vec::new(),
            editor: Box::new(|_, _| Err(std::io::Error::other("No terminal to run the editor in"))),
            flow_options: FlowOptions::default(),
            plugins: None,
            plugin_hold: None,
            plugin_hint: None,
            plugin_action_list: Vec::new(),
            reveal_fired: None,
        };
        app.next_day_at = backend::sched_timing(&app.backend)
            .ok()
            .map(|t| t.next_day_at);
        match start {
            Start::Decks => app.go_decks(),
            Start::Review => {
                app.load_totals();
                app.start_review();
            }
        }
        app
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }
    pub fn set_keymap(&mut self, keymap: Keymap) {
        self.keymap = keymap;
    }
    pub fn shutdown(&mut self) {
        if let Some(plugins) = self.plugins.as_mut() {
            plugins.shutdown();
        }
        self.cancel_job();
        self.services.audio.shutdown();
        self.clear_images();
    }
    /// Escape sequences the terminal must receive before the next frame
    /// (kitty image deletion), and whether to clear it fully first.
    pub fn take_terminal_requests(&mut self) -> (String, bool) {
        (
            std::mem::take(&mut self.cleanup),
            std::mem::replace(&mut self.full_redraw, false),
        )
    }
    pub fn scopes(&self) -> Vec<Scope> {
        match self.layers.last() {
            Some(layer) => vec![layer.scope()],
            None => vec![self.screen.scope(), Scope::Global],
        }
    }

    // ---- messages ----------------------------------------------------------

    pub fn notice(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            error: false,
            at: self.now.at,
        });
    }
    pub fn error(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            error: true,
            at: self.now.at,
        });
    }
    pub fn visible_message(&self) -> Option<&Message> {
        self.message.as_ref().filter(|m| {
            self.now.at.saturating_duration_since(m.at) < Duration::from_secs(MESSAGE_SECS)
        })
    }

    fn clear_images(&mut self) {
        self.services.images.clear();
        self.cleanup.push_str(&self.services.images.take_cleanup());
        // Erase Sixel/iTerm placements too; repaint every cell next frame.
        self.full_redraw = true;
    }

    // ---- screens -----------------------------------------------------------

    fn load_totals(&mut self) {
        if let Ok(tree) = backend::deck_tree(&self.backend) {
            self.totals = Some(backend::total_counts(&tree));
        }
    }

    fn leave_review(&mut self) {
        if matches!(self.screen, Screen::Review(_)) {
            // `Overview.show()` stops audio.
            self.services.audio.stop();
            self.clear_images();
        }
    }

    pub fn go_decks(&mut self) {
        self.leave_review();
        let previous = match &self.screen {
            Screen::Decks(view) => view.rows.get(view.selected).map(|r| r.id),
            _ => None,
        }
        .or(self.current_deck);
        let mut view = DecksView::default();
        match backend::deck_tree(&self.backend) {
            Ok(tree) => {
                self.totals = Some(backend::total_counts(&tree));
                view.tree = Some(tree);
                view.rebuild_rows();
            }
            Err(e) => view.error = Some(e.to_string()),
        }
        view.studied = backend::studied_today(&self.backend).ok();
        if let Some(id) = previous {
            view.selected = view.rows.iter().position(|r| r.id == id).unwrap_or(0);
        }
        self.screen = Screen::Decks(view);
    }

    fn reload_decks(&mut self) {
        let Screen::Decks(view) = &mut self.screen else {
            return;
        };
        let selected = view.rows.get(view.selected).map(|r| r.id);
        match backend::deck_tree(&self.backend) {
            Ok(tree) => {
                self.totals = Some(backend::total_counts(&tree));
                view.tree = Some(tree);
                view.rebuild_rows();
                view.error = None;
            }
            Err(e) => view.error = Some(e.to_string()),
        }
        view.studied = backend::studied_today(&self.backend).ok();
        view.selected = selected
            .and_then(|id| view.rows.iter().position(|r| r.id == id))
            .unwrap_or(view.selected.min(view.rows.len().saturating_sub(1)));
    }

    /// The overview of `deck_id` (selecting it), or of the current deck.
    /// A finished deck shows the congrats screen, as desktop does.
    pub fn open_overview(&mut self, deck_id: Option<i64>) {
        self.leave_review();
        let result = match deck_id {
            Some(id) => backend::select_deck(&self.backend, id),
            None => backend::overview(&self.backend),
        };
        if let Some(id) = deck_id {
            self.current_deck = Some(id);
        }
        match result {
            Ok(data) => {
                self.current_deck = Some(data.deck_id);
                if data.new + data.learning + data.review == 0 {
                    self.show_congrats();
                    return;
                }
                self.screen = Screen::Overview(OverviewView {
                    data: Some(data),
                    error: None,
                });
            }
            Err(e) => {
                self.screen = Screen::Overview(OverviewView {
                    data: None,
                    error: Some(e.to_string()),
                })
            }
        }
    }

    pub fn show_congrats(&mut self) {
        self.leave_review();
        let (data, error) = match backend::congrats(&self.backend) {
            Ok(data) => (Some(data), None),
            Err(e) => (None, Some(e.to_string())),
        };
        self.screen = Screen::Congrats(CongratsView {
            data,
            fetched_at: self.now.at,
            error,
        });
    }

    /// Study Now: `col.startTimebox()` and the reviewer.
    pub fn start_review(&mut self) {
        let now = self.now;
        self.screen = Screen::Review(Box::new(ReviewView {
            reviewer: Reviewer::new(now),
            scroll: 0,
            warning: None,
            input_focused: true,
            render: None,
            replay_next: 0,
            card_area: Rect::new(0, 0, 80, 20),
        }));
        let fx = match &mut self.screen {
            Screen::Review(view) => view.reviewer.start(&self.backend, now),
            _ => unreachable!(),
        };
        self.apply(fx);
    }

    fn review(&mut self) -> Option<&mut ReviewView> {
        match &mut self.screen {
            Screen::Review(view) => Some(view),
            _ => None,
        }
    }

    /// Runs a reviewer method and applies its effects.
    fn with_reviewer(&mut self, f: impl FnOnce(&mut Reviewer, &dyn Backend, Now) -> Vec<Effect>) {
        let now = self.now;
        let started = Instant::now();
        let was_answer =
            matches!(&self.screen, Screen::Review(view) if view.reviewer.phase() == Phase::Answer);
        let fx = match &mut self.screen {
            Screen::Review(view) => f(&mut view.reviewer, &self.backend, now),
            _ => return,
        };
        self.apply(fx);
        if was_answer
            && !matches!(&self.screen, Screen::Review(view) if view.reviewer.phase() == Phase::Answer)
        {
            self.ignore_keys_before = Some(now.at + started.elapsed());
        }
    }

    fn apply(&mut self, fx: Vec<Effect>) {
        for effect in fx {
            match effect {
                Effect::Audio(cue) => self.services.audio.cue(&cue),
                Effect::Notice(text) => self.notice(text),
                Effect::Error(text) => self.error(text),
                Effect::Warning(text) => {
                    if let Some(view) = self.review() {
                        view.warning = Some(text);
                    }
                }
                Effect::Finished => {
                    self.load_totals();
                    self.show_congrats();
                }
                Effect::Timebox { cards, minutes } => {
                    self.overlay = Some(Overlay::Choice {
                        title: "Timebox".into(),
                        message: reviewer::timebox_message(cards, minutes),
                        options: vec![
                            ("Continue".into(), ChoiceValue::TimeboxContinue),
                            ("Finish".into(), ChoiceValue::TimeboxFinish),
                        ],
                        selected: 0,
                        cancel: ChoiceValue::TimeboxContinue,
                    })
                }
                Effect::SchedulerUpgradeRequired => {
                    self.overlay = Some(Overlay::Confirm {
                        message: format!("{SCHEDULER_UPGRADE_MESSAGE} Upgrade now?"),
                        kind: ConfirmKind::UpgradeScheduler,
                        yes: false,
                    })
                }
                Effect::CardChanged => {
                    let card = self
                        .review()
                        .and_then(|v| v.reviewer.card().map(|c| c.queued.id));
                    if card != self.shown_card {
                        self.previous_card = self.shown_card.take();
                        self.shown_card = card;
                    }
                    if let Some(view) = self.review() {
                        view.scroll = 0;
                        view.render = None;
                        view.replay_next = 0;
                        view.input_focused = true;
                    }
                    self.clear_images();
                }
            }
        }
    }

    // ---- events ------------------------------------------------------------

    pub fn handle(&mut self, event: AppEvent, now: Now) {
        self.now = now;
        match event {
            AppEvent::Key(key, received) => {
                if self.ignore_keys_before.is_some_and(|t| received < t) {
                    return;
                }
                self.on_key(key);
            }
            AppEvent::Paste(text) => self.on_paste(&text),
            AppEvent::Mouse(mouse) => self.on_mouse(mouse),
            AppEvent::Resize => {
                if let Some(view) = self.review() {
                    view.render = None;
                }
                self.clear_images();
            }
            AppEvent::FocusGained => self.focused = true,
            AppEvent::FocusLost => self.focused = false,
            AppEvent::Changes(changes) => self.on_changes(&changes),
            AppEvent::Audio(AudioEvent::Idle) => {
                if self.overlay.is_some() || !self.layers.is_empty() {
                    return;
                }
                let focused = self.focused;
                self.with_reviewer(|r, b, now| r.audio_idle(b, now, focused));
            }
            AppEvent::Audio(AudioEvent::Error(message)) => {
                let short: String = message.chars().take(80).collect();
                self.error(short);
            }
            AppEvent::Audio(_) | AppEvent::ImageReady => {}
            AppEvent::Tick => self.on_tick(),
        }
    }

    fn on_tick(&mut self) {
        let now = self.now;
        self.poll_plugins();
        self.poll_job();
        self.tick_layers();
        if let Screen::Review(_) = self.screen {
            // The reviewer waits (no auto advance) under the add / browser.
            if self.overlay.is_none() && self.layers.is_empty() {
                let playing = self.services.audio.is_playing();
                let focused = self.focused;
                self.with_reviewer(|r, b, now| r.tick(b, now, playing, focused));
            }
            return;
        }
        if self.next_day_at.is_some_and(|t| now.wall_ms / 1000 >= t) {
            self.next_day_at = backend::sched_timing(&self.backend)
                .ok()
                .map(|t| t.next_day_at)
                .filter(|t| now.wall_ms / 1000 < *t);
            self.refresh_screen();
            return;
        }
        if let Screen::Congrats(view) = &self.screen {
            if now.at.saturating_duration_since(view.fetched_at)
                >= Duration::from_secs(CONGRATS_POLL_SECS)
            {
                self.poll_congrats();
            }
        }
    }

    /// Re-polls the congrats data; leaves for the overview when cards are due again.
    fn poll_congrats(&mut self) {
        let due = match backend::overview(&self.backend) {
            Ok(o) => o.new + o.learning + o.review > 0,
            Err(_) => backend::queue(&self.backend, 1).is_ok_and(|q| !q.cards.is_empty()),
        };
        if due {
            self.open_overview(None);
        } else {
            self.show_congrats();
        }
    }

    pub(crate) fn refresh_screen(&mut self) {
        match &self.screen {
            Screen::Decks(_) => self.reload_decks(),
            Screen::Overview(_) => self.open_overview(None),
            Screen::Congrats(_) => self.poll_congrats(),
            Screen::Review(_) => {}
        }
    }

    fn on_changes(&mut self, changes: &OpChanges) {
        if self.backend.take_own() {
            return;
        }
        for layer in &mut self.layers {
            layer.on_changes(&self.backend, changes);
        }
        self.changes_below(changes);
    }

    /// Refreshes the screen (under any layers) after `changes`.
    pub(crate) fn changes_below(&mut self, changes: &OpChanges) {
        if changes.notetype {
            self.classified.clear();
        }
        match &self.screen {
            Screen::Review(_) => {
                let changes = changes.clone();
                self.with_reviewer(|r, b, now| r.on_changes(b, &changes, now));
            }
            Screen::Decks(_) => self.reload_decks(),
            Screen::Overview(_) | Screen::Congrats(_) => {
                if changes.study_queues || changes.deck {
                    self.refresh_screen();
                }
            }
        }
    }

    fn on_paste(&mut self, text: &str) {
        if self.holding() {
            return;
        }
        if self.sync_ui.is_visible() || self.package_ui.is_some() {
            return;
        }
        match &mut self.overlay {
            Some(Overlay::Palette { input, selected }) => {
                input.insert_str(text);
                *selected = 0;
            }
            Some(Overlay::Prompt { input, .. }) => input.insert_str(text),
            Some(_) => {}
            None if !self.layers.is_empty() => self.layer_paste(text),
            None => {
                if let Some(view) = self.review() {
                    if let Some(input) = view.reviewer.typed_mut() {
                        input.insert_str(text);
                        view.input_focused = true;
                    }
                }
            }
        }
    }

    fn on_mouse(&mut self, mouse: MouseEvent) {
        if self.holding() {
            return;
        }
        if self.overlay.is_some()
            || self.sync_ui.is_visible()
            || self.package_ui.is_some()
            || !self.layers.is_empty()
        {
            return;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                for _ in 0..3 {
                    self.dispatch(Action::Up);
                }
            }
            MouseEventKind::ScrollDown => {
                for _ in 0..3 {
                    self.dispatch(Action::Down);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let (x, y) = (mouse.column, mouse.row);
                match &mut self.screen {
                    Screen::Decks(view) => {
                        let area = view.list_area;
                        if y >= area.y && y < area.bottom() {
                            let index = view.offset + (y - area.y) as usize;
                            if index < view.rows.len() {
                                if view.selected == index {
                                    self.dispatch(Action::Select);
                                } else {
                                    view.selected = index;
                                }
                            }
                        }
                    }
                    Screen::Review(view) => {
                        let area = view.card_area;
                        let Some(render) = &view.render else { return };
                        if x < area.x || y < area.y {
                            return;
                        }
                        let row = (y - area.y) as usize + view.scroll;
                        let col = x - area.x;
                        let hit =
                            render.card.widgets.iter().find(|w| {
                                w.row == row && col >= w.col && col < w.col + w.width.max(1)
                            });
                        if let Some(widget) = hit.cloned() {
                            self.activate_widget(&widget.kind, &widget.key);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn activate_widget(&mut self, kind: &WidgetKind, key: &str) {
        match kind {
            WidgetKind::Replay { side, index } => {
                let side = services::rules_side(*side);
                let Some(view) = self.review() else { return };
                let tag = view.reviewer.tag(side, *index);
                let interrupt = view
                    .reviewer
                    .card()
                    .is_some_and(|c| c.config.interrupt_audio);
                if let Some(tag) = tag {
                    self.services.audio.play_one(tag, interrupt);
                }
            }
            WidgetKind::Hint { .. } | WidgetKind::Details { .. } => {
                if let Some(view) = self.review() {
                    if !view.reviewer.expanded.remove(key) {
                        view.reviewer.expanded.insert(key.to_owned());
                    }
                    view.render = None;
                }
                self.clear_images();
            }
            WidgetKind::TypeAnswer => {
                if let Some(view) = self.review() {
                    view.input_focused = true;
                }
            }
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        let Some(chord) = Chord::from_event(&key) else {
            return;
        };
        if self.holding() && self.overlay.is_none() {
            if let Some(action) = self.keymap.lookup(&[Scope::Review, Scope::Global], &chord) {
                self.dispatch(action);
            }
            return;
        }
        // The add and browser screens take every key but their scope's own.
        if self.overlay.is_none()
            && !self.layers.is_empty()
            && !self.sync_ui.is_visible()
            && self.package_ui.is_none()
        {
            self.layer_key(key, chord);
            return;
        }
        // Ctrl+C cancels a running sync or package operation instead of quitting.
        let cancels = chord.ctrl
            && chord.code == KeyCode::Char('c')
            && (self.sync_ui.is_running()
                || self.package_ui.as_ref().is_some_and(PackageUi::is_running));
        if self.keymap.lookup(&[Scope::Global], &chord) == Some(Action::Quit) && !cancels {
            self.quit = true;
            return;
        }
        if self.sync_ui.is_visible() {
            let intent = self.sync_ui.on_key(key);
            self.run_sync_intent(intent);
            return;
        }
        if let Some(ui) = &mut self.package_ui {
            let intent = ui.on_key(&key);
            self.run_package_intent(intent);
            return;
        }
        if self.overlay.is_some() {
            self.overlay_key(key);
            return;
        }
        // The type-answer input takes typing while it has focus.
        if let Screen::Review(view) = &mut self.screen {
            if view.input_focused {
                if let Some(input) = view.reviewer.typed_mut() {
                    if input.handle_key(&key) {
                        return;
                    }
                    match key.code {
                        KeyCode::Enter => {
                            self.dispatch(Action::ShowAnswerOrGood);
                            return;
                        }
                        KeyCode::Esc => {
                            view.input_focused = false;
                            return;
                        }
                        _ => {}
                    }
                }
            }
        }
        let Some(chord) = Chord::from_event(&key) else {
            return;
        };
        if self.deck_filter_key(&key) {
            return;
        }
        if let Some(action) = self.keymap.lookup(&self.scopes(), &chord) {
            if key.kind == crossterm::event::KeyEventKind::Repeat
                && matches!(
                    action,
                    Action::ShowAnswerOrGood
                        | Action::Again
                        | Action::Hard
                        | Action::Good
                        | Action::Easy
                )
            {
                return;
            }
            self.dispatch(action);
        }
    }

    /// The deck list's `/` filter. Returns true when it consumed `key`.
    fn deck_filter_key(&mut self, key: &KeyEvent) -> bool {
        let Screen::Decks(view) = &mut self.screen else {
            return false;
        };
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match &mut view.filter {
            None => {
                if plain && key.code == KeyCode::Char('/') {
                    view.filter = Some(DeckFilter {
                        input: TextInput::default(),
                        editing: true,
                    });
                    view.rebuild_rows();
                    view.selected = 0;
                    return true;
                }
                false
            }
            Some(f) if f.editing => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Esc => {
                        view.filter = None;
                        view.rebuild_rows();
                    }
                    KeyCode::Enter => f.editing = false,
                    KeyCode::Down => {
                        view.selected = (view.selected + 1).min(view.rows.len().saturating_sub(1))
                    }
                    KeyCode::Up => view.selected = view.selected.saturating_sub(1),
                    KeyCode::Char('n') if ctrl => {
                        view.selected = (view.selected + 1).min(view.rows.len().saturating_sub(1))
                    }
                    KeyCode::Char('p') if ctrl => view.selected = view.selected.saturating_sub(1),
                    _ => {
                        if f.input.handle_key(key) {
                            view.selected = 0;
                            view.rebuild_rows();
                        }
                    }
                }
                true
            }
            Some(_) => {
                if key.code == KeyCode::Esc {
                    // Keep the selected deck in view when the tree returns.
                    let id = view.rows.get(view.selected).map(|r| r.id);
                    view.filter = None;
                    view.rebuild_rows();
                    if let Some(i) = id.and_then(|id| view.rows.iter().position(|r| r.id == id)) {
                        view.selected = i;
                    }
                    return true;
                }
                if plain && key.code == KeyCode::Char('/') {
                    if let Some(f) = &mut view.filter {
                        f.editing = true;
                    }
                    return true;
                }
                false
            }
        }
    }

    // ---- actions -----------------------------------------------------------

    /// Runs `action` on the current screen.
    pub fn dispatch(&mut self, action: Action) {
        if self.holding()
            && !matches!(
                action,
                Action::ReleaseHold | Action::Quit | Action::Help | Action::Palette | Action::About
            )
        {
            return;
        }
        let now = self.now;
        if !self.layers.is_empty() {
            // Over a layer only these make sense; screen actions would act
            // on the hidden screen.
            match action {
                Action::Quit => self.request_quit(),
                Action::Help => self.overlay = Some(Overlay::Help { scroll: 0 }),
                Action::About => self.overlay = Some(Overlay::About),
                Action::AddNote => self.open_add(),
                Action::Browse => self.open_browser(None),
                Action::NoteTypes => self.open_notetypes(),
                _ => {}
            }
            return;
        }
        match action {
            Action::Quit => self.quit = true,
            Action::ReleaseHold => self.release_hold(),
            Action::PluginActions => self.plugin_actions(),
            Action::AddNote => self.open_add(),
            Action::Browse => {
                let search = match &self.screen {
                    Screen::Review(view) => {
                        view.reviewer.card().map(|c| format!("cid:{}", c.queued.id))
                    }
                    _ => None,
                };
                self.open_browser(search);
            }
            Action::Help => self.overlay = Some(Overlay::Help { scroll: 0 }),
            Action::About => self.overlay = Some(Overlay::About),
            Action::DeckOptions => self.open_deck_options(),
            Action::NoteTypes => self.open_notetypes(),
            Action::CreateDeck => self.open_deck_prompt(DeckPromptKind::Add),
            Action::RenameDeck => self.open_deck_prompt(DeckPromptKind::Rename),
            Action::MoveDeck => self.open_deck_prompt(DeckPromptKind::Move),
            Action::DeleteDeck => self.open_deck_prompt(DeckPromptKind::Delete),
            Action::FilteredDeck => self.open_filtered_deck_new(None),
            Action::CustomStudy => self.open_custom_study(),
            Action::RebuildDeck => self.rebuild_or_empty(true),
            Action::EmptyDeck => self.rebuild_or_empty(false),
            Action::DeckDescription => self.open_deck_description(),
            Action::Statistics => self.open_stats(),
            Action::Tools => self.open_tools(),
            Action::SharedDecks => self.open_shared_decks(),
            Action::CardInfo => {
                let card = self
                    .review()
                    .and_then(|v| v.reviewer.card().map(|c| c.queued.id));
                self.open_card_info(card, false);
            }
            Action::PreviousCardInfo => self.open_card_info(self.previous_card, true),
            Action::Palette => {
                self.overlay = Some(Overlay::Palette {
                    input: TextInput::default(),
                    selected: 0,
                })
            }
            Action::DeckList => self.go_decks(),
            Action::CycleGraphics => {
                let mode = self.services.images.cycle();
                self.cleanup.push_str(&self.services.images.take_cleanup());
                self.full_redraw = true;
                if let Some(view) = self.review() {
                    view.render = None;
                }
                self.notice(format!("Graphics: {}", graphics_name(mode)));
            }
            Action::Undo | Action::Redo => self.undo(action == Action::Redo),
            Action::Sync
            | Action::Import
            | Action::Export
            | Action::BackupNow
            | Action::RestoreBackup
                if matches!(self.screen, Screen::Decks(_) | Screen::Overview(_)) =>
            {
                match action {
                    Action::Sync => self.sync_action(),
                    Action::Import => self.import_action(),
                    Action::Export => self.export_action(),
                    Action::BackupNow => self.backup_now_action(),
                    _ => self.restore_backup_action(),
                }
            }
            _ => match self.screen.scope() {
                Scope::Decks => self.decks_action(action),
                Scope::Overview => self.overview_action(action),
                Scope::Congrats => self.congrats_action(action),
                Scope::Review => self.review_action(action, now),
                _ => {}
            },
        }
        if self.overlay.is_some() {
            self.clear_images();
        }
    }

    fn undo(&mut self, redo: bool) {
        if let Screen::Review(_) = self.screen {
            self.with_reviewer(|r, b, now| r.undo(b, redo, now));
            return;
        }
        match backend::undo_redo(&self.backend, redo) {
            Ok(label) => {
                self.notice(reviewer::undo_notice(&label, redo));
                self.refresh_screen();
            }
            Err(e) => self.error(e.to_string()),
        }
    }

    fn decks_action(&mut self, action: Action) {
        let Screen::Decks(view) = &mut self.screen else {
            return;
        };
        let len = view.rows.len();
        let page = view.list_area.height.max(1) as usize;
        let last = len.saturating_sub(1);
        match action {
            Action::Up => view.selected = view.selected.saturating_sub(1),
            Action::Down => view.selected = (view.selected + 1).min(last),
            Action::PageUp => view.selected = view.selected.saturating_sub(page),
            Action::PageDown => view.selected = (view.selected + page).min(last),
            Action::Top => view.selected = 0,
            Action::Bottom => view.selected = last,
            Action::Collapse | Action::Expand | Action::ToggleCollapse => {
                let Some(row) = view.rows.get(view.selected).cloned() else {
                    return;
                };
                let target = match action {
                    Action::Collapse if row.has_children && !row.collapsed => Some(true),
                    Action::Collapse => {
                        // Go to the parent row.
                        if let Some(parent) = view.rows[..view.selected]
                            .iter()
                            .rposition(|r| r.depth + 1 == row.depth)
                        {
                            view.selected = parent;
                        }
                        None
                    }
                    Action::Expand if row.has_children && row.collapsed => Some(false),
                    Action::ToggleCollapse if row.has_children => Some(!row.collapsed),
                    _ => None,
                };
                if let Some(collapsed) = target {
                    self.set_collapsed(row.id, collapsed);
                }
            }
            Action::Select => {
                if let Some(id) = view.rows.get(view.selected).map(|r| r.id) {
                    self.open_overview(Some(id));
                }
            }
            Action::StudyDeck => {
                if let Some(id) = view.rows.get(view.selected).map(|r| r.id) {
                    self.current_deck = Some(id);
                    if let Err(e) = backend::select_deck(&self.backend, id) {
                        self.error(e.to_string());
                        return;
                    }
                    self.start_review();
                }
            }
            Action::Refresh => self.reload_decks(),
            Action::Study => {
                let id = view.rows.get(view.selected).map(|r| r.id);
                self.open_overview(id);
            }
            _ => {}
        }
    }

    /// Collapses or expands a deck: shown at once, saved with `SetDeckCollapsed`.
    fn set_collapsed(&mut self, deck_id: i64, collapsed: bool) {
        fn set(deck: &mut Deck, id: i64, collapsed: bool) {
            if deck.id == id {
                deck.collapsed = collapsed;
            }
            for child in &mut deck.children {
                set(child, id, collapsed);
            }
        }
        if let Screen::Decks(view) = &mut self.screen {
            if let Some(tree) = view.tree.as_mut() {
                set(tree, deck_id, collapsed);
                view.rows = backend::visible_rows(tree);
                view.selected = view.rows.iter().position(|r| r.id == deck_id).unwrap_or(0);
            }
        }
        let command = tanki_core::Command::SetDeckCollapsed { deck_id, collapsed };
        if let Err(e) = backend::changes(&self.backend, command) {
            self.error(format!("Could not save the collapse state: {e}"));
        }
    }

    fn overview_action(&mut self, action: Action) {
        match action {
            Action::StudyNow | Action::Study => self.start_review(),
            Action::Unbury => self.unbury(),
            Action::Back => self.go_decks(),
            _ => {}
        }
    }

    fn congrats_action(&mut self, action: Action) {
        match action {
            Action::Unbury => self.unbury(),
            Action::Refresh => self.poll_congrats(),
            Action::Back => self.go_decks(),
            Action::Study => self.open_overview(None),
            _ => {}
        }
    }

    /// `on_unbury`: asks which kind when both kinds are buried.
    fn unbury(&mut self) {
        let info = match backend::congrats(&self.backend) {
            Ok(info) => info,
            Err(e) => {
                self.error(e.to_string());
                return;
            }
        };
        if !info.have_sched_buried && !info.have_user_buried {
            self.notice("No buried cards.");
            return;
        }
        if info.have_sched_buried && info.have_user_buried {
            self.overlay = Some(Overlay::Choice {
                title: "Unbury".into(),
                message: "What would you like to unbury?".into(),
                options: vec![
                    (
                        "Manually Buried Cards".into(),
                        ChoiceValue::Unbury(UnburyMode::UserOnly),
                    ),
                    (
                        "Buried Siblings".into(),
                        ChoiceValue::Unbury(UnburyMode::SchedOnly),
                    ),
                    (
                        "All Buried Cards".into(),
                        ChoiceValue::Unbury(UnburyMode::All),
                    ),
                    ("Cancel".into(), ChoiceValue::Cancel),
                ],
                selected: 0,
                cancel: ChoiceValue::Cancel,
            });
        } else {
            self.do_unbury(UnburyMode::All);
        }
    }

    fn do_unbury(&mut self, mode: UnburyMode) {
        let deck_id = match self.current_deck {
            Some(id) => id,
            None => match backend::overview(&self.backend) {
                Ok(o) => o.deck_id,
                Err(e) => {
                    self.error(e.to_string());
                    return;
                }
            },
        };
        match backend::changes(&self.backend, tanki_core::Command::Unbury { deck_id, mode }) {
            Ok(_) => {
                self.notice("Cards unburied.");
                self.open_overview(None);
            }
            Err(e) => self.error(e.to_string()),
        }
    }

    /// Lays out the current side if the cache is stale; returns it.
    fn cycle_render_tier(&mut self) {
        use tanki_render::tier::RenderTier;
        let Screen::Review(view) = &mut self.screen else {
            return;
        };
        let Some(card) = view.reviewer.card() else {
            return;
        };
        let id = card.rendered.notetype_id.to_string();
        let tier = match self.render_tiers.get(&id).copied().unwrap_or_default() {
            RenderTier::Text => RenderTier::Bitmap,
            RenderTier::Bitmap => RenderTier::Text,
        };
        if tier == RenderTier::Bitmap && !cfg!(feature = "blitz") {
            self.notice("Bitmap tier unavailable: build with --features blitz.");
            return;
        }
        self.render_tiers.insert(id.clone(), tier);
        view.render = None;
        self.clear_images();
        if let Some(path) = &self.config_path {
            let saved = tanki_core::config::Config::load(path).and_then(|mut config| {
                config.profile_mut(&self.profile_key()).render_tiers.insert(
                    id,
                    match tier {
                        RenderTier::Text => tanki_core::config::RenderTier::Text,
                        RenderTier::Bitmap => tanki_core::config::RenderTier::Bitmap,
                    },
                );
                config.save(path)
            });
            if let Err(error) = saved {
                self.error(format!(
                    "Tier changed for this session; could not save: {error}"
                ));
                return;
            }
        }
        self.notice(format!("This note type uses {tier:?} rendering."));
    }

    /// Lays out the current side if the cache is stale; returns it.
    pub(crate) fn ensure_rendered(&mut self) -> Option<&tanki_render::RenderedCard> {
        let graphics = self.services.images.mode();
        let Screen::Review(view) = &mut self.screen else {
            return None;
        };
        let html = view.reviewer.side_html()?;
        let card = view.reviewer.card()?;
        let area = view.card_area;
        let hide = card.config.hide_audio_play_buttons;
        let tier = self
            .render_tiers
            .get(&card.rendered.notetype_id.to_string())
            .copied()
            .unwrap_or_default();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (
            &html,
            &card.rendered.css,
            area.width,
            area.height,
            hide,
            &view.reviewer.expanded,
            format!("{graphics:?}"),
            tier,
            tanki_render::tier::revision(),
        )
            .hash(&mut hasher);
        let key = hasher.finish();
        if view.render.as_ref().map(|r| r.key) != Some(key) {
            let display = &self.services.display;
            let side = services::card_side(
                &card.rendered,
                html,
                &self.profile.media_dir,
                display.night_mode,
            );
            let mut options = display.render_options(graphics, area, hide, &view.reviewer.expanded);
            options.tier = tier;
            let mut rendered = self.services.renderer.render(&side, &options);
            let classification = self
                .classified
                .entry(card.rendered.notetype_id)
                .or_insert_with(|| {
                    tanki_render::tier::classify(
                        if card.rendered.template_question.is_empty() {
                            &card.rendered.question
                        } else {
                            &card.rendered.template_question
                        },
                        if card.rendered.template_answer.is_empty() {
                            &card.rendered.answer
                        } else {
                            &card.rendered.template_answer
                        },
                        &card.rendered.css,
                    )
                });
            if tier == tanki_render::tier::RenderTier::Text {
                rendered
                    .notices
                    .extend(classification.reasons.iter().cloned());
            }
            if let Some(next) = view.reviewer.next_rendered() {
                let next_side = services::card_side(
                    next,
                    next.review_question(),
                    &self.profile.media_dir,
                    display.night_mode,
                );
                let mut next_options = options.clone();
                next_options.expanded.clear();
                next_options.tier = self
                    .render_tiers
                    .get(&next.notetype_id.to_string())
                    .copied()
                    .unwrap_or_default();
                tanki_render::tier::pre_render(&next_side, &next_options);
                // The answer of the current card is also a predictable next side.
            }
            if !view.reviewer.showing_answer() && card.rendered.type_answer.is_none() {
                let answer = services::card_side(
                    &card.rendered,
                    card.rendered.review_answer(""),
                    &self.profile.media_dir,
                    display.night_mode,
                );
                tanki_render::tier::pre_render(&answer, &options);
            }
            // A newly shown answer scrolls to the divider (`<hr id=answer>`).
            if view.reviewer.showing_answer() && view.scroll == 0 {
                if let Some(row) = rendered.answer_row {
                    if rendered.lines.len() > area.height as usize {
                        view.scroll =
                            row.min(rendered.lines.len().saturating_sub(area.height as usize));
                    }
                }
            }
            view.render = Some(RenderCache {
                key,
                card: rendered,
            });
        }
        view.render.as_ref().map(|r| &r.card)
    }

    fn scroll(&mut self, delta: isize) {
        let Some(len) = self.ensure_rendered().map(|c| c.lines.len()) else {
            return;
        };
        let Some(view) = self.review() else { return };
        let max = len.saturating_sub(view.card_area.height as usize);
        view.scroll = view.scroll.saturating_add_signed(delta).min(max);
        self.clear_images();
    }

    fn review_action(&mut self, action: Action, now: Now) {
        let Some(view) = self.review() else { return };
        let phase = view.reviewer.phase();
        let page = view.card_area.height.max(2) as isize - 1;
        let answering = matches!(
            action,
            Action::Again | Action::Hard | Action::Good | Action::Easy | Action::ShowAnswerOrGood
        ) && phase == Phase::Answer;
        let started = Instant::now();
        match action {
            Action::ShowAnswerOrGood => self.with_reviewer(|r, b, now| r.enter(b, now)),
            Action::Again | Action::Hard | Action::Good | Action::Easy => {
                let rating = match action {
                    Action::Again => Rating::Again,
                    Action::Hard => Rating::Hard,
                    Action::Good => Rating::Good,
                    _ => Rating::Easy,
                };
                self.with_reviewer(|r, b, now| r.answer(b, rating, now));
            }
            Action::Back | Action::Study => self.open_overview(None),
            Action::More => self.open_more_menu(),
            Action::Replay => {
                let fx = view.reviewer.replay();
                self.apply(fx);
            }
            Action::PauseAudio => self.services.audio.toggle_pause(),
            Action::RecordVoice => match self.services.audio.toggle_recording() {
                Ok(true) => self.notice("Recording own voice; press v to stop."),
                Ok(false) => self.notice("Finishing recording; Shift+V replays when ready."),
                Err(error) => self.error(error),
            },
            Action::ReplayVoice => {
                if let Err(error) = self.services.audio.replay_recording() {
                    self.error(error);
                }
            }
            Action::ToggleMasks => {
                let key = "occlusion:masks-hidden".to_string();
                if !view.reviewer.expanded.remove(&key) {
                    view.reviewer.expanded.insert(key);
                }
                view.render = None;
                self.clear_images();
            }
            Action::CycleRenderTier => self.cycle_render_tier(),
            Action::SeekBack => self.services.audio.seek(-5.0),
            Action::SeekForward => self.services.audio.seek(5.0),
            Action::PlayNextButton => self.play_next_button(),
            Action::Flag(n) => self.with_reviewer(|r, b, now| r.toggle_flag(b, n, now)),
            Action::ToggleMark => self.with_reviewer(|r, b, now| r.toggle_mark(b, now)),
            Action::BuryCard => self.with_reviewer(|r, b, now| r.bury_card(b, now)),
            Action::BuryNote => self.with_reviewer(|r, b, now| r.bury_note(b, now)),
            Action::SuspendCard => self.with_reviewer(|r, b, now| r.suspend_card(b, now)),
            Action::SuspendNote => self.with_reviewer(|r, b, now| r.suspend_note(b, now)),
            Action::Forget => {
                if view.reviewer.card().is_none() || phase == Phase::Transition {
                    return;
                }
                let defaults =
                    backend::forget_defaults(&self.backend).unwrap_or(tanki_core::ForgetDefaults {
                        restore_position: true,
                        reset_counts: false,
                    });
                self.overlay = Some(Overlay::Forget {
                    restore: defaults.restore_position,
                    reset: defaults.reset_counts,
                    row: 0,
                });
            }
            Action::SetDueDate => {
                let Some(card) = view.reviewer.card() else {
                    return;
                };
                let default = card.config.set_due_default.clone();
                self.overlay = Some(Overlay::Prompt {
                    title: "Set due date (0 = today, 1! = tomorrow + change interval, 3-7 = random choice of 3-7 days)".into(),
                    input: TextInput::new(default),
                    kind: PromptKind::SetDueDate,
                });
            }
            Action::DeleteNote => {
                if view.reviewer.card().is_none() {
                    return;
                }
                self.overlay = Some(Overlay::Confirm {
                    message: "Delete this note and all its cards?".into(),
                    kind: ConfirmKind::DeleteNote,
                    yes: false,
                });
            }
            Action::ToggleAutoAdvance => self.with_reviewer(|r, _, now| r.toggle_auto_advance(now)),
            Action::ZoomImage => {
                let count = self.ensure_rendered().map_or(0, |c| c.images.len());
                if count == 0 {
                    self.notice("This side has no images.");
                } else {
                    self.clear_images();
                    self.overlay = Some(Overlay::Zoom { index: 0 });
                }
            }
            Action::OpenInBrowser => self.open_in_browser(),
            Action::EditNote => self.open_edit_current(),
            Action::ToggleHints | Action::ToggleDetails => {
                let hints = action == Action::ToggleHints;
                let keys: Vec<(String, bool)> = self
                    .ensure_rendered()
                    .map(|c| {
                        c.widgets
                            .iter()
                            .filter_map(|w| match (&w.kind, hints) {
                                (WidgetKind::Hint { expanded, .. }, true)
                                | (WidgetKind::Details { expanded, .. }, false) => {
                                    Some((w.key.clone(), *expanded))
                                }
                                _ => None,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let Some(view) = self.review() else { return };
                if keys.is_empty() {
                    let what = if hints { "hints" } else { "details" };
                    self.notice(format!("This side has no {what}."));
                    return;
                }
                // Open all when any is closed, otherwise close all.
                let open = keys.iter().any(|(_, expanded)| !expanded);
                for (key, _) in keys {
                    if open {
                        view.reviewer.expanded.insert(key);
                    } else {
                        view.reviewer.expanded.remove(&key);
                    }
                }
                view.render = None;
                self.clear_images();
            }
            Action::FocusInput => {
                if view.reviewer.typed().is_some() {
                    view.input_focused = true;
                }
            }
            Action::Up => self.scroll(-1),
            Action::Down => self.scroll(1),
            Action::PageUp => self.scroll(-page),
            Action::PageDown => self.scroll(page),
            Action::Top => self.scroll(isize::MIN / 2),
            Action::Bottom => self.scroll(isize::MAX / 2),
            _ => {}
        }
        if answering {
            // Keys pressed while the answer was being recorded are dropped
            // (desktop's "transition" state).
            self.ignore_keys_before = Some(now.at + started.elapsed());
        }
    }

    fn play_next_button(&mut self) {
        let buttons: Vec<(WidgetKind, String)> = self
            .ensure_rendered()
            .map(|c| {
                c.widgets
                    .iter()
                    .filter(|w| matches!(w.kind, WidgetKind::Replay { .. }))
                    .map(|w| (w.kind.clone(), w.key.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let Some(view) = self.review() else { return };
        if buttons.is_empty() {
            self.notice("This side has no audio buttons.");
            return;
        }
        let index = view.replay_next % buttons.len();
        view.replay_next = index + 1;
        let (kind, key) = &buttons[index];
        self.activate_widget(kind, key);
        self.notice(format!("Playing audio {} of {}", index + 1, buttons.len()));
    }

    fn open_in_browser(&mut self) {
        let Screen::Review(view) = &self.screen else {
            return;
        };
        let Some(html) = view.reviewer.side_html() else {
            return;
        };
        let Some(card) = view.reviewer.card() else {
            return;
        };
        let side = services::card_side(
            &card.rendered,
            html,
            &self.profile.media_dir,
            self.services.display.night_mode,
        );
        let card_id = card.queued.id;
        let result = services::write_card_page(&side, card_id, &self.profile.dir)
            .and_then(|path| (self.services.opener)(&path).map(|()| path));
        match result {
            Ok(path) => self.notice(format!("Opened {}", path.display())),
            Err(e) => self.error(format!("Could not open the card: {e}")),
        }
    }

    /// `_contextMenu()`: every reviewer action in one list.
    fn open_more_menu(&mut self) {
        let Screen::Review(view) = &self.screen else {
            return;
        };
        let Some(card) = view.reviewer.card() else {
            return;
        };
        let flag = card.queued.flag;
        let names = card.config.flag_names.clone();
        let auto = view.reviewer.auto_advance();
        let scopes = [Scope::Review, Scope::Global];
        let km = &self.keymap;
        let item = |action: Action, label: &str, checked: bool| MenuItem {
            label: label.to_owned(),
            key: km.hint(&scopes, action),
            action,
            checked,
        };
        let mut items: Vec<MenuItem> = (1..=7u8)
            .map(|n| {
                let name = names
                    .get(n as usize - 1)
                    .cloned()
                    .unwrap_or_else(|| format!("Flag {n}"));
                item(Action::Flag(n), &format!("Flag: {name}"), flag == n)
            })
            .collect();
        items.extend([
            item(Action::BuryCard, "Bury Card", false),
            item(Action::Forget, "Reset Card to New...", false),
            item(Action::SetDueDate, "Set Due Date...", false),
            item(Action::SuspendCard, "Suspend Card", false),
            item(Action::DeckOptions, "Options", false),
            item(Action::CardInfo, "Card Info", false),
            item(Action::PreviousCardInfo, "Previous Card Info", false),
            item(Action::ToggleMark, "Mark Note", card.queued.marked),
            item(Action::BuryNote, "Bury Note", false),
            item(Action::SuspendNote, "Suspend Note", false),
            item(Action::DeleteNote, "Delete Note...", false),
            item(Action::EditNote, "Edit Note...", false),
            item(Action::Replay, "Replay Audio", false),
            item(Action::RecordVoice, "Record Own Voice", false),
            item(Action::ReplayVoice, "Replay Own Voice", false),
            item(Action::ToggleMasks, "Show / Hide Occlusion Masks", false),
            item(
                Action::CycleRenderTier,
                "Switch Text / Bitmap for Note Type",
                false,
            ),
            item(Action::PlayNextButton, "Play Next Audio Button", false),
            item(Action::PauseAudio, "Pause Audio", false),
            item(Action::SeekBack, "Audio -5s", false),
            item(Action::SeekForward, "Audio +5s", false),
            item(Action::ToggleAutoAdvance, "Auto Advance", auto),
            item(Action::ZoomImage, "Zoom Images", false),
            item(Action::OpenInBrowser, "Open in Web Browser", false),
            item(Action::ToggleHints, "Show / Hide Hints", false),
            item(Action::ToggleDetails, "Show / Hide Details", false),
            item(Action::Undo, "Undo", false),
            item(Action::Redo, "Redo", false),
            item(Action::CycleGraphics, "Cycle Image Protocol", false),
        ]);
        self.overlay = Some(Overlay::Menu {
            title: "More".into(),
            items,
            selected: 0,
        });
    }

    // ---- overlays ------------------------------------------------------------

    /// Palette entries for the active scopes, filtered by `query`.
    pub fn palette_matches(&self, query: &str) -> Vec<Action> {
        let actions = self.keymap.actions(&self.scopes());
        let labels: Vec<String> = actions
            .iter()
            .map(|a| format!("{} {}", a.description(), a.name()))
            .collect();
        crate::fuzzy::rank(query, labels.iter().map(String::as_str))
            .into_iter()
            .map(|i| actions[i])
            .collect()
    }

    fn overlay_key(&mut self, key: KeyEvent) {
        let Some(overlay) = self.overlay.take() else {
            return;
        };
        let chord = Chord::from_event(&key);
        let menu_action = chord.and_then(|c| self.keymap.lookup(&[Scope::Menu], &c));
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match overlay {
            Overlay::Help { scroll } => {
                let next = match (menu_action, key.code) {
                    (Some(Action::Down), _) => Some(scroll + 1),
                    (Some(Action::Up), _) => Some(scroll.saturating_sub(1)),
                    (_, KeyCode::PageDown) => Some(scroll + 10),
                    (_, KeyCode::PageUp) => Some(scroll.saturating_sub(10)),
                    _ => None,
                };
                if let Some(scroll) = next {
                    self.overlay = Some(Overlay::Help { scroll });
                }
            }
            Overlay::About => {}
            Overlay::Palette {
                mut input,
                mut selected,
            } => {
                let matches = self.palette_matches(input.text());
                match key.code {
                    KeyCode::Esc => return,
                    KeyCode::Enter => {
                        if let Some(action) = matches.get(selected).copied() {
                            self.dispatch(action);
                        }
                        return;
                    }
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Char('p') if ctrl => selected = selected.saturating_sub(1),
                    KeyCode::Down | KeyCode::Tab => {
                        selected = (selected + 1).min(matches.len().saturating_sub(1))
                    }
                    KeyCode::Char('n') if ctrl => {
                        selected = (selected + 1).min(matches.len().saturating_sub(1))
                    }
                    _ => {
                        if input.handle_key(&key) {
                            selected = 0;
                        }
                    }
                }
                self.overlay = Some(Overlay::Palette { input, selected });
            }
            Overlay::Menu {
                title,
                items,
                mut selected,
            } => {
                // A reviewer key runs its item directly, as in desktop's menu.
                let direct = chord
                    .and_then(|c| self.keymap.lookup(&[Scope::Review], &c))
                    .filter(|a| items.iter().any(|i| i.action == *a))
                    .filter(|_| {
                        !matches!(
                            menu_action,
                            Some(Action::Down | Action::Up | Action::Select)
                        )
                    });
                if let Some(action) = direct {
                    self.dispatch(action);
                    return;
                }
                match menu_action {
                    Some(Action::Back) => return,
                    Some(Action::Select) => {
                        if let Some(item) = items.get(selected) {
                            let action = item.action;
                            self.dispatch(action);
                        }
                        return;
                    }
                    Some(Action::Up) => selected = selected.saturating_sub(1),
                    Some(Action::Down) => {
                        selected = (selected + 1).min(items.len().saturating_sub(1))
                    }
                    Some(Action::Top) => selected = 0,
                    Some(Action::Bottom) => selected = items.len().saturating_sub(1),
                    _ => {}
                }
                self.overlay = Some(Overlay::Menu {
                    title,
                    items,
                    selected,
                });
            }
            Overlay::Prompt {
                title,
                mut input,
                kind,
            } => match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    let text = input.text().trim().to_owned();
                    match kind {
                        PromptKind::SetDueDate => {
                            if !text.is_empty() {
                                self.with_reviewer(|r, b, now| r.set_due_date(b, text, now));
                            }
                        }
                        PromptKind::ImportPath | PromptKind::ExportPath => {
                            self.path_entered(kind, &text)
                        }
                    }
                }
                _ => {
                    input.handle_key(&key);
                    self.overlay = Some(Overlay::Prompt { title, input, kind });
                }
            },
            Overlay::Confirm { message, kind, yes } => {
                let decided = match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => Some(true),
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Some(false),
                    KeyCode::Enter => Some(yes),
                    _ => None,
                };
                match decided {
                    Some(true) => self.confirmed(kind),
                    Some(false) => {
                        if kind == ConfirmKind::UpgradeScheduler {
                            self.go_decks();
                        }
                    }
                    None => {
                        let yes = match key.code {
                            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                                !yes
                            }
                            KeyCode::Char('h') => true,
                            KeyCode::Char('l') => false,
                            _ => yes,
                        };
                        self.overlay = Some(Overlay::Confirm { message, kind, yes });
                    }
                }
            }
            Overlay::Choice {
                title,
                message,
                options,
                mut selected,
                cancel,
            } => {
                match menu_action {
                    Some(Action::Back) => {
                        self.chosen(cancel);
                        return;
                    }
                    Some(Action::Select) => {
                        let value = options.get(selected).map_or(cancel, |o| o.1);
                        self.chosen(value);
                        return;
                    }
                    Some(Action::Up) => selected = selected.saturating_sub(1),
                    Some(Action::Down) => {
                        selected = (selected + 1).min(options.len().saturating_sub(1))
                    }
                    _ => {}
                }
                self.overlay = Some(Overlay::Choice {
                    title,
                    message,
                    options,
                    selected,
                    cancel,
                });
            }
            Overlay::Forget {
                mut restore,
                mut reset,
                mut row,
            } => {
                match (key.code, menu_action) {
                    (KeyCode::Esc, _) => return,
                    (KeyCode::Enter, _) => {
                        self.with_reviewer(|r, b, now| r.forget(b, restore, reset, now));
                        return;
                    }
                    (KeyCode::Char(' '), _) => match row {
                        0 => restore = !restore,
                        1 => reset = !reset,
                        _ => {
                            self.with_reviewer(|r, b, now| r.forget(b, restore, reset, now));
                            return;
                        }
                    },
                    (_, Some(Action::Up)) => row = row.saturating_sub(1),
                    (_, Some(Action::Down)) => row = (row + 1).min(2),
                    _ => {}
                }
                self.overlay = Some(Overlay::Forget {
                    restore,
                    reset,
                    row,
                });
            }
            Overlay::Zoom { mut index } => {
                let count = match &self.screen {
                    Screen::Review(view) => view.render.as_ref().map_or(0, |r| r.card.images.len()),
                    _ => 0,
                };
                let action = chord.and_then(|c| self.keymap.lookup(&[Scope::Zoom], &c));
                match action {
                    Some(Action::Back) => {
                        self.clear_images();
                        self.full_redraw = true;
                        return;
                    }
                    Some(Action::NextImage) if count > 0 => index = (index + 1) % count,
                    Some(Action::PrevImage) if count > 0 => index = (index + count - 1) % count,
                    _ => {}
                }
                self.clear_images();
                self.overlay = Some(Overlay::Zoom { index });
            }
        }
    }

    fn confirmed(&mut self, kind: ConfirmKind) {
        match kind {
            ConfirmKind::DeleteNote => self.with_reviewer(|r, b, now| r.delete_note(b, now)),
            ConfirmKind::UpgradeScheduler => {
                self.with_reviewer(|r, b, now| r.upgrade_scheduler(b, now))
            }
            ConfirmKind::QuitDiscard => self.quit = true,
        }
    }

    fn chosen(&mut self, value: ChoiceValue) {
        match value {
            ChoiceValue::Unbury(mode) => self.do_unbury(mode),
            ChoiceValue::TimeboxContinue => {
                self.with_reviewer(|r, b, now| r.timebox_continue(b, now))
            }
            ChoiceValue::TimeboxFinish => {
                if let Some(view) = self.review() {
                    view.reviewer.timebox_finish();
                }
                self.go_decks();
            }
            ChoiceValue::Backup(index) => self.restore_chosen(index),
            ChoiceValue::PluginAction(index) => self.run_plugin_action(index),
            ChoiceValue::Cancel => {}
        }
    }
}

pub fn graphics_name(mode: tanki_term::GraphicsMode) -> &'static str {
    match mode {
        tanki_term::GraphicsMode::Auto => "auto",
        tanki_term::GraphicsMode::Kitty => "kitty",
        tanki_term::GraphicsMode::Sixel => "sixel",
        tanki_term::GraphicsMode::Iterm => "iterm",
        tanki_term::GraphicsMode::Blocks => "blocks",
        tanki_term::GraphicsMode::None => "text",
    }
}

mod data;
mod manage;
mod notes;

#[cfg(test)]
mod data_tests;
#[cfg(test)]
mod notes_tests;
#[cfg(test)]
mod tests;
