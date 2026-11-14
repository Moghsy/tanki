//! Test doubles: an in-memory collection behind [`Backend`] and fake
//! renderer/image/audio adapters. Nothing here calls the other crates' stubs.
use crate::backend::{self, Backend, DataOp, Result};
use crate::reviewer::{AudioCue, Now};
use crate::services::{AudioPlayer, AudioStatus, ImageDisplay, ImageState};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::Instant;
use tanki_core::{
    AnswerOutcome, Command, Congrats, CoreError, DataCommand, DataResponse, Deck, DeckOverview,
    ForgetDefaults, OpChanges, OperationProgress, Queue, QueueKind, QueuedCard, Rating,
    RenderedCard, Response, SchedTiming, SchedulerStatus, StudyConfig, Target,
};
use tanki_media::MediaTag;
use tanki_render::ImageSource;
use tanki_term::GraphicsMode;

pub fn command_name(command: &Command) -> String {
    let debug = format!("{command:?}");
    debug
        .split([' ', '{', '('])
        .next()
        .unwrap_or_default()
        .to_owned()
}

#[derive(Debug, Clone)]
pub struct FakeCard {
    pub queued: QueuedCard,
    pub rendered: RenderedCard,
}

pub fn card(id: i64, question: &str, answer: &str) -> FakeCard {
    FakeCard {
        queued: QueuedCard {
            id,
            note_id: id * 10,
            deck_id: 1,
            intervals: vec!["<1m".into(), "<6m".into(), "<10m".into(), "4d".into()],
            queue: QueueKind::New,
            ..Default::default()
        },
        rendered: RenderedCard {
            question: question.into(),
            answer: answer.into(),
            ..Default::default()
        },
    }
}

#[derive(Debug, Clone)]
pub struct Answered {
    pub card_id: i64,
    pub rating: Rating,
    pub answered_at_millis: i64,
    pub milliseconds_taken: u32,
}

pub struct FakeState {
    pub cards: Vec<FakeCard>,
    pub config: StudyConfig,
    pub log: Vec<Command>,
    pub answered: Vec<Answered>,
    pub undo: Vec<FakeCard>,
    pub redo: Vec<FakeCard>,
    /// The outcome of the next `Answer` (leech flags).
    pub next_outcome: AnswerOutcome,
    /// Commands (by name) that fail with this error.
    pub failing: HashMap<String, CoreError>,
    pub tree: Deck,
    pub overview: DeckOverview,
    pub congrats: Congrats,
    pub timing: SchedTiming,
    pub status: SchedulerStatus,
    pub studied: String,
    /// Counts reported besides the cards (others not fetched).
    pub extra_counts: (u32, u32, u32),
}

/// The fake's data operations (sync, packages, backups).
pub struct FakeData {
    /// Every data command started, in order.
    pub started: Vec<DataCommand>,
    /// Results handed to the next operations in order; `Done` when empty.
    pub results: VecDeque<Result<DataResponse>>,
    /// While true, polling reports the operation as still pending.
    pub hold: Rc<Cell<bool>>,
    pub progress: OperationProgress,
    pub cancels: u32,
}

struct FakeOp {
    hold: Rc<Cell<bool>>,
    result: RefCell<Option<Result<DataResponse>>>,
}

impl DataOp for FakeOp {
    fn poll(&self) -> Result<Option<DataResponse>> {
        if self.hold.get() {
            return Ok(None);
        }
        match self.result.borrow_mut().take() {
            Some(result) => result.map(Some),
            None => Ok(None),
        }
    }
}

pub struct FakeBackend {
    pub state: RefCell<FakeState>,
    pub data: RefCell<FakeData>,
}

impl FakeBackend {
    pub fn new(cards: Vec<FakeCard>) -> Self {
        let mut config = backend::fallback_study_config();
        config.autoplay = true;
        Self {
            data: RefCell::new(FakeData {
                started: Vec::new(),
                results: VecDeque::new(),
                hold: Rc::new(Cell::new(false)),
                progress: OperationProgress::default(),
                cancels: 0,
            }),
            state: RefCell::new(FakeState {
                cards,
                config,
                log: Vec::new(),
                answered: Vec::new(),
                undo: Vec::new(),
                redo: Vec::new(),
                next_outcome: AnswerOutcome::default(),
                failing: HashMap::new(),
                tree: sample_tree(),
                overview: DeckOverview {
                    deck_id: 1,
                    name: "Default".into(),
                    new: 2,
                    learning: 1,
                    review: 3,
                    new_delta: 1,
                    description: "<p>Fixture deck</p>".into(),
                    ..Default::default()
                },
                congrats: Congrats {
                    learn_remaining: 2,
                    secs_until_next_learn: 300,
                    have_user_buried: true,
                    deck_description: String::new(),
                    ..Default::default()
                },
                timing: SchedTiming {
                    days_elapsed: 10,
                    next_day_at: i64::MAX / 2,
                },
                status: SchedulerStatus {
                    v3: true,
                    version: 2,
                },
                studied: "Studied 3 cards in 1 minutes today (20s/card).".into(),
                extra_counts: (0, 0, 0),
            }),
        }
    }
    pub fn with<R>(&self, f: impl FnOnce(&mut FakeState) -> R) -> R {
        f(&mut self.state.borrow_mut())
    }
    pub fn names(&self) -> Vec<String> {
        self.state.borrow().log.iter().map(command_name).collect()
    }
    pub fn clear_log(&self) {
        self.state.borrow_mut().log.clear();
    }
    pub fn fail(&self, name: &str, error: CoreError) {
        self.state
            .borrow_mut()
            .failing
            .insert(name.to_owned(), error);
    }
}

fn changed() -> OpChanges {
    OpChanges {
        card: true,
        study_queues: true,
        ..Default::default()
    }
}

impl Backend for FakeBackend {
    fn start_data(&self, command: DataCommand) -> Result<Box<dyn DataOp>> {
        let mut data = self.data.borrow_mut();
        data.started.push(command);
        let result = data.results.pop_front().unwrap_or(Ok(DataResponse::Done));
        Ok(Box::new(FakeOp {
            hold: data.hold.clone(),
            result: RefCell::new(Some(result)),
        }))
    }
    fn data_progress(&self) -> OperationProgress {
        self.data.borrow().progress.clone()
    }
    fn cancel_data(&self) {
        self.data.borrow_mut().cancels += 1;
    }
    fn execute(&self, command: Command) -> Result<Response> {
        let mut s = self.state.borrow_mut();
        s.log.push(command.clone());
        if let Some(error) = s.failing.get(&command_name(&command)) {
            return Err(error.clone());
        }
        Ok(match command {
            Command::DeckTree => Response::DeckTree(s.tree.clone()),
            Command::Queue { limit } => {
                let mut counts = s.extra_counts;
                for c in &s.cards {
                    match c.queued.queue {
                        QueueKind::New => counts.0 += 1,
                        QueueKind::Learning => counts.1 += 1,
                        QueueKind::Review => counts.2 += 1,
                    }
                }
                Response::Queue(Queue {
                    new: counts.0,
                    learning: counts.1,
                    review: counts.2,
                    cards: s
                        .cards
                        .iter()
                        .take(limit as usize)
                        .map(|c| c.queued.clone())
                        .collect(),
                })
            }
            Command::Render { card_id } => {
                let card = s.cards.iter().find(|c| c.queued.id == card_id);
                match card {
                    Some(c) => Response::Rendered(c.rendered.clone()),
                    None => return Err(CoreError::Invalid(format!("no card {card_id}"))),
                }
            }
            Command::StudyConfig { .. } => Response::StudyConfig(s.config.clone()),
            Command::Answer {
                card_id,
                rating,
                answered_at_millis,
                milliseconds_taken,
            } => {
                if s.cards.first().map(|c| c.queued.id) != Some(card_id) {
                    return Err(CoreError::Invalid("not the queue top".into()));
                }
                let card = s.cards.remove(0);
                s.undo.push(card);
                s.redo.clear();
                s.answered.push(Answered {
                    card_id,
                    rating,
                    answered_at_millis,
                    milliseconds_taken,
                });
                let mut outcome = std::mem::take(&mut s.next_outcome);
                outcome.changes = changed();
                Response::Answered(outcome)
            }
            Command::Undo => match s.undo.pop() {
                Some(card) => {
                    s.cards.insert(0, card.clone());
                    s.redo.push(card);
                    s.answered.pop();
                    Response::Undone {
                        changes: changed(),
                        label: "Answer Card".into(),
                    }
                }
                None => return Err(CoreError::Invalid("Nothing to undo".into())),
            },
            Command::Redo => match s.redo.pop() {
                Some(card) => {
                    s.cards.retain(|c| c.queued.id != card.queued.id);
                    s.undo.push(card);
                    Response::Undone {
                        changes: changed(),
                        label: "Answer Card".into(),
                    }
                }
                None => return Err(CoreError::Invalid("Nothing to redo".into())),
            },
            Command::SelectDeck { deck_id } => {
                s.overview.deck_id = deck_id;
                Response::Overview(s.overview.clone())
            }
            Command::Overview => Response::Overview(s.overview.clone()),
            Command::Congrats => Response::Congrats(s.congrats.clone()),
            Command::StudiedToday => Response::Text(s.studied.clone()),
            Command::SchedTiming => Response::SchedTiming(s.timing),
            Command::SchedulerStatus => Response::SchedulerStatus(s.status),
            Command::UpgradeScheduler => {
                s.status.v3 = true;
                Response::Changes(changed())
            }
            Command::CompareAnswer {
                expected, provided, ..
            } => Response::Html(format!("<code id=typeans>{provided}|{expected}</code>")),
            Command::SetFlag { card_ids, flag } => {
                for c in s
                    .cards
                    .iter_mut()
                    .filter(|c| card_ids.contains(&c.queued.id))
                {
                    c.queued.flag = flag;
                }
                Response::Changes(OpChanges {
                    card: true,
                    ..Default::default()
                })
            }
            Command::SetMarked { note_ids, marked } => {
                for c in s
                    .cards
                    .iter_mut()
                    .filter(|c| note_ids.contains(&c.queued.note_id))
                {
                    c.queued.marked = marked;
                }
                Response::Changes(OpChanges {
                    note: true,
                    ..Default::default()
                })
            }
            Command::BuryOrSuspend { target, .. } => {
                match target {
                    Target::Cards(ids) => s.cards.retain(|c| !ids.contains(&c.queued.id)),
                    Target::Notes(ids) => s.cards.retain(|c| !ids.contains(&c.queued.note_id)),
                }
                Response::Changes(changed())
            }
            Command::Forget { card_ids, .. } | Command::SetDueDate { card_ids, .. } => {
                s.cards.retain(|c| !card_ids.contains(&c.queued.id));
                Response::Changes(changed())
            }
            Command::DeleteNotes { note_ids } => {
                s.cards.retain(|c| !note_ids.contains(&c.queued.note_id));
                Response::Changes(changed())
            }
            Command::SetDeckCollapsed { deck_id, collapsed } => {
                fn set(deck: &mut Deck, id: i64, collapsed: bool) {
                    if deck.id == id {
                        deck.collapsed = collapsed;
                    }
                    for child in &mut deck.children {
                        set(child, id, collapsed);
                    }
                }
                set(&mut s.tree, deck_id, collapsed);
                Response::Changes(OpChanges {
                    deck: true,
                    ..Default::default()
                })
            }
            Command::Unbury { .. } => Response::Changes(changed()),
            Command::ForgetDefaults => Response::ForgetDefaults(ForgetDefaults {
                restore_position: true,
                reset_counts: false,
            }),
            Command::UndoStatus => Response::UndoStatus(Default::default()),
            other => {
                return Err(CoreError::Invalid(format!(
                    "Not implemented yet: {}",
                    command_name(&other)
                )))
            }
        })
    }
}

pub fn sample_tree() -> Deck {
    let deck = |id, name: &str, level, counts: (u32, u32, u32), children| Deck {
        id,
        name: name.into(),
        new: counts.0,
        learning: counts.1,
        review: counts.2,
        level,
        children,
        ..Default::default()
    };
    let mut filtered = deck(4, "Cram", 1, (0, 0, 2), vec![]);
    filtered.filtered = true;
    deck(
        0,
        "",
        0,
        (0, 0, 0),
        vec![
            deck(1, "Default", 1, (2, 1, 3), vec![]),
            deck(
                2,
                "Languages",
                1,
                (5, 0, 10),
                vec![deck(3, "Japanese", 2, (5, 0, 0), vec![])],
            ),
            filtered,
        ],
    )
}

/// A fixed clock origin for tests.
pub fn t0() -> Now {
    Now {
        at: Instant::now(),
        wall_ms: 1_700_000_000_000,
    }
}

#[derive(Default)]
pub struct FakeAudio {
    pub cues: RefCell<Vec<AudioCue>>,
    pub played: RefCell<Vec<MediaTag>>,
    pub playing: Cell<bool>,
    pub calls: RefCell<Vec<String>>,
}

impl AudioPlayer for FakeAudio {
    fn cue(&self, cue: &AudioCue) {
        self.cues.borrow_mut().push(cue.clone());
    }
    fn play_one(&self, tag: MediaTag, _interrupt: bool) {
        self.played.borrow_mut().push(tag);
    }
    fn stop(&self) {
        self.calls.borrow_mut().push("stop".into());
    }
    fn toggle_pause(&self) {
        self.calls.borrow_mut().push("pause".into());
    }
    fn seek(&self, seconds: f64) {
        self.calls.borrow_mut().push(format!("seek {seconds}"));
    }
    fn is_playing(&self) -> bool {
        self.playing.get()
    }
    fn status(&self) -> AudioStatus {
        AudioStatus::Builtin
    }
}

/// Draws a block of `#` where an image goes.
pub struct FakeImages {
    pub mode: GraphicsMode,
    pub drawn: Vec<(ImageSource, Rect)>,
    pub clears: usize,
}

impl Default for FakeImages {
    fn default() -> Self {
        Self {
            mode: GraphicsMode::Kitty,
            drawn: Vec::new(),
            clears: 0,
        }
    }
}

impl ImageDisplay for FakeImages {
    fn mode(&self) -> GraphicsMode {
        self.mode
    }
    fn cycle(&mut self) -> GraphicsMode {
        self.mode = match self.mode {
            GraphicsMode::Kitty => GraphicsMode::Iterm,
            GraphicsMode::Iterm => GraphicsMode::Sixel,
            GraphicsMode::Sixel => GraphicsMode::Blocks,
            GraphicsMode::Blocks => GraphicsMode::None,
            _ => GraphicsMode::Kitty,
        };
        self.mode
    }
    fn draw(&mut self, source: &ImageSource, area: Rect, buf: &mut Buffer) -> ImageState {
        self.drawn.push((source.clone(), area));
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_symbol("#");
                }
            }
        }
        ImageState::Drawn
    }
    fn clear(&mut self) {
        self.clears += 1;
    }
    fn take_cleanup(&mut self) -> String {
        String::new()
    }
}

/// The buffer as plain text, one string per row, trailing spaces removed.
pub fn buffer_text(buf: &Buffer) -> Vec<String> {
    let area = buf.area;
    (area.top()..area.bottom())
        .map(|y| {
            let mut line = String::new();
            let mut x = area.left();
            while x < area.right() {
                let cell = &buf[(x, y)];
                line.push_str(cell.symbol());
                x += 1;
            }
            line.trim_end().to_owned()
        })
        .collect()
}
