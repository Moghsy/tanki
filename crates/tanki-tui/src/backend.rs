//! The collection as the TUI sees it: a synchronous command channel
//! (`CollectionActor` in production, a fake in tests) plus typed helpers.
use std::cell::Cell;
#[cfg(test)]
use tanki_core::{AnswerAction, QuestionAction};
use tanki_core::{
    AnswerOutcome, CollectionActor, Command, Congrats, CoreError, DataCommand, DataResponse, Deck,
    DeckOverview, OpChanges, OperationProgress, Queue, Rating, RenderedCard, Response, SchedTiming,
    SchedulerStatus, StudyConfig,
};

pub type Result<T> = tanki_core::Result<T>;

/// Anything that executes collection commands. Only `CollectionActor`
/// touches a collection (PLAN 14 rule 13).
pub trait Backend {
    fn execute(&self, command: Command) -> Result<Response>;

    /// Queues a long data operation (sync, packages, backups) and returns at
    /// once; poll the handle from the event loop. The UI never blocks on one.
    fn start_data(&self, _command: DataCommand) -> Result<Box<dyn DataOp>> {
        Err(CoreError::Other(
            "This collection does not support data operations".into(),
        ))
    }
    /// Queues any long command (Phase 5: FSRS, check database/media, CSV
    /// import) and returns at once; poll the handle from the event loop.
    /// Progress and cancel use `data_progress` / `cancel_data`.
    fn start(&self, _command: Command) -> Result<Box<dyn CommandOp>> {
        Err(CoreError::Other(
            "This collection does not support background commands".into(),
        ))
    }
    /// Progress of the running data operation.
    fn data_progress(&self) -> OperationProgress {
        OperationProgress::default()
    }
    /// Asks the running data operation to stop.
    fn cancel_data(&self) {}
}

/// A queued data operation.
pub trait DataOp {
    /// `Ok(None)` while the actor is still working.
    fn poll(&self) -> Result<Option<DataResponse>>;
}

/// A queued command of any kind (`Backend::start`).
pub trait CommandOp {
    /// `Ok(None)` while the actor is still working.
    fn poll(&self) -> Result<Option<Response>>;
}

impl CommandOp for tanki_core::PendingCommand {
    fn poll(&self) -> Result<Option<Response>> {
        self.try_recv()
    }
}

impl DataOp for tanki_core::PendingOperation {
    fn poll(&self) -> Result<Option<DataResponse>> {
        self.try_recv()
    }
}

impl Backend for CollectionActor {
    fn execute(&self, command: Command) -> Result<Response> {
        CollectionActor::execute(self, command)
    }
    fn start_data(&self, command: DataCommand) -> Result<Box<dyn DataOp>> {
        Ok(Box::new(CollectionActor::start_data(self, command)?))
    }
    fn start(&self, command: Command) -> Result<Box<dyn CommandOp>> {
        Ok(Box::new(CollectionActor::start(self, command)?))
    }
    fn data_progress(&self) -> OperationProgress {
        self.progress()
    }
    fn cancel_data(&self) {
        self.cancel_operation();
    }
}

/// Counts the mutations this process made, so the matching `OpChanges`
/// broadcasts can be told apart from changes made elsewhere (desktop's
/// `op_executed` skips ops whose handler is the screen itself).
pub struct Tracked<'a> {
    inner: &'a dyn Backend,
    own: Cell<u32>,
}

impl<'a> Tracked<'a> {
    pub fn new(inner: &'a dyn Backend) -> Self {
        Self {
            inner,
            own: Cell::new(0),
        }
    }
    /// True (and consumes one) when a broadcast belongs to our own mutation.
    pub fn take_own(&self) -> bool {
        let own = self.own.get();
        if own > 0 {
            self.own.set(own - 1);
            true
        } else {
            false
        }
    }
}

impl Backend for Tracked<'_> {
    fn execute(&self, command: Command) -> Result<Response> {
        let result = self.inner.execute(command);
        if let Ok(response) = &result {
            if response.changes().is_some() {
                self.own.set(self.own.get() + 1);
            }
        }
        result
    }
    fn start_data(&self, command: DataCommand) -> Result<Box<dyn DataOp>> {
        self.inner.start_data(command)
    }
    /// Results of started commands are not counted as own mutations; their
    /// broadcast refreshes the screen like an outside change.
    fn start(&self, command: Command) -> Result<Box<dyn CommandOp>> {
        self.inner.start(command)
    }
    fn data_progress(&self) -> OperationProgress {
        self.inner.data_progress()
    }
    fn cancel_data(&self) {
        self.inner.cancel_data();
    }
}

fn unexpected(response: &Response) -> CoreError {
    let name = format!("{response:?}");
    let name = name.split(['(', ' ', '{']).next().unwrap_or("?").to_owned();
    CoreError::Other(format!("Unexpected response from the collection: {name}"))
}

macro_rules! expect {
    ($backend:expr, $command:expr, $pattern:pat => $value:expr) => {
        match $backend.execute($command)? {
            $pattern => Ok($value),
            other => Err(unexpected(&other)),
        }
    };
}

pub fn deck_tree(b: &dyn Backend) -> Result<Deck> {
    expect!(b, Command::DeckTree, Response::DeckTree(d) => d)
}
pub fn queue(b: &dyn Backend, limit: u32) -> Result<Queue> {
    expect!(b, Command::Queue { limit }, Response::Queue(q) => q)
}
pub fn render(b: &dyn Backend, card_id: i64) -> Result<RenderedCard> {
    expect!(b, Command::Render { card_id }, Response::Rendered(r) => r)
}
pub fn study_config(b: &dyn Backend, card_id: i64) -> Result<StudyConfig> {
    expect!(b, Command::StudyConfig { card_id }, Response::StudyConfig(c) => c)
}
pub fn answer(
    b: &dyn Backend,
    card_id: i64,
    rating: Rating,
    answered_at_millis: i64,
    milliseconds_taken: u32,
) -> Result<AnswerOutcome> {
    expect!(b, Command::Answer { card_id, rating, answered_at_millis, milliseconds_taken },
        Response::Answered(o) => o)
}
/// `Undo` or `Redo` -> the label of the step.
pub fn undo_redo(b: &dyn Backend, redo: bool) -> Result<String> {
    let command = if redo { Command::Redo } else { Command::Undo };
    expect!(b, command, Response::Undone { label, .. } => label)
}
pub fn changes(b: &dyn Backend, command: Command) -> Result<OpChanges> {
    let response = b.execute(command)?;
    response
        .changes()
        .cloned()
        .ok_or_else(|| unexpected(&response))
}
pub fn select_deck(b: &dyn Backend, deck_id: i64) -> Result<DeckOverview> {
    expect!(b, Command::SelectDeck { deck_id }, Response::Overview(o) => o)
}
pub fn overview(b: &dyn Backend) -> Result<DeckOverview> {
    expect!(b, Command::Overview, Response::Overview(o) => o)
}
pub fn congrats(b: &dyn Backend) -> Result<Congrats> {
    expect!(b, Command::Congrats, Response::Congrats(c) => c)
}
pub fn studied_today(b: &dyn Backend) -> Result<String> {
    expect!(b, Command::StudiedToday, Response::Text(t) => t)
}
pub fn sched_timing(b: &dyn Backend) -> Result<SchedTiming> {
    expect!(b, Command::SchedTiming, Response::SchedTiming(t) => t)
}
pub fn scheduler_status(b: &dyn Backend) -> Result<SchedulerStatus> {
    expect!(b, Command::SchedulerStatus, Response::SchedulerStatus(s) => s)
}
pub fn compare_answer(
    b: &dyn Backend,
    expected: String,
    provided: String,
    combining: bool,
) -> Result<String> {
    expect!(b, Command::CompareAnswer { expected, provided, combining }, Response::Html(h) => h)
}
pub fn forget_defaults(b: &dyn Backend) -> Result<tanki_core::ForgetDefaults> {
    expect!(b, Command::ForgetDefaults, Response::ForgetDefaults(d) => d)
}

/// Anki's defaults, for the synthetic backend. Study uses actual settings.
#[cfg(test)]
pub fn fallback_study_config() -> StudyConfig {
    StudyConfig {
        autoplay: true,
        skip_question_when_replaying_answer: false,
        show_timer: false,
        stop_timer_on_answer: false,
        cap_answer_time_secs: 60,
        seconds_to_show_question: 0.0,
        seconds_to_show_answer: 0.0,
        question_action: QuestionAction::ShowAnswer,
        answer_action: AnswerAction::BuryCard,
        wait_for_audio: true,
        show_intervals: true,
        show_remaining_count: true,
        timebox_secs: 0,
        interrupt_audio: true,
        hide_audio_play_buttons: false,
        custom_scheduling_set: false,
        set_due_default: "1".into(),
        flag_names: [
            "Red",
            "Orange",
            "Green",
            "Blue",
            "Pink",
            "Turquoise",
            "Purple",
        ]
        .map(String::from)
        .to_vec(),
    }
}

/// One visible row of the deck tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeckRow {
    pub id: i64,
    pub name: String,
    /// Full name with `::` separators.
    pub full_name: String,
    /// 0 for top-level decks.
    pub depth: usize,
    pub new: u32,
    pub learning: u32,
    pub review: u32,
    pub collapsed: bool,
    pub filtered: bool,
    pub has_children: bool,
}

/// The rows of `root`'s children in display order, skipping the children of
/// collapsed decks.
pub fn visible_rows(root: &Deck) -> Vec<DeckRow> {
    fn walk(deck: &Deck, prefix: &str, depth: usize, out: &mut Vec<DeckRow>) {
        let full_name = if prefix.is_empty() {
            deck.name.clone()
        } else {
            format!("{prefix}::{}", deck.name)
        };
        out.push(DeckRow {
            id: deck.id,
            name: deck.name.clone(),
            full_name: full_name.clone(),
            depth,
            new: deck.new,
            learning: deck.learning,
            review: deck.review,
            collapsed: deck.collapsed,
            filtered: deck.filtered,
            has_children: !deck.children.is_empty(),
        });
        if !deck.collapsed {
            for child in &deck.children {
                walk(child, &full_name, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    for child in &root.children {
        walk(child, "", 0, &mut out);
    }
    out
}

/// Finds a deck by its full name (`Parent::Child`), ignoring case as Anki
/// does for deck names.
pub fn find_deck(root: &Deck, full_name: &str) -> Option<i64> {
    fn walk(deck: &Deck, prefix: &str, wanted: &str) -> Option<i64> {
        let name = if prefix.is_empty() {
            deck.name.clone()
        } else {
            format!("{prefix}::{}", deck.name)
        };
        if name.to_lowercase() == wanted {
            return Some(deck.id);
        }
        deck.children.iter().find_map(|c| walk(c, &name, wanted))
    }
    let wanted = full_name.trim().to_lowercase();
    root.children.iter().find_map(|c| walk(c, "", &wanted))
}

/// Total counts of the top-level decks.
pub fn total_counts(root: &Deck) -> (u32, u32, u32) {
    root.children.iter().fold((0, 0, 0), |(n, l, r), d| {
        (n + d.new, l + d.learning, r + d.review)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deck(id: i64, name: &str, children: Vec<Deck>) -> Deck {
        Deck {
            id,
            name: name.into(),
            children,
            ..Default::default()
        }
    }

    #[test]
    fn rows_skip_collapsed_children_and_find_by_full_name() {
        let mut parent = deck(2, "Lang", vec![deck(3, "Japanese", vec![])]);
        let root = deck(0, "", vec![deck(1, "Default", vec![]), parent.clone()]);
        let rows = visible_rows(&root);
        assert_eq!(
            rows.iter()
                .map(|r| r.full_name.as_str())
                .collect::<Vec<_>>(),
            ["Default", "Lang", "Lang::Japanese"]
        );
        assert_eq!(rows[2].depth, 1);
        assert!(rows[1].has_children);
        assert_eq!(find_deck(&root, "lang::JAPANESE"), Some(3));
        assert_eq!(find_deck(&root, "Missing"), None);
        parent.collapsed = true;
        let root = deck(0, "", vec![parent]);
        assert_eq!(visible_rows(&root).len(), 1);
        assert_eq!(find_deck(&root, "Lang::Japanese"), Some(3));
    }

    struct Echo;
    impl Backend for Echo {
        fn execute(&self, command: Command) -> Result<Response> {
            Ok(match command {
                Command::SetFlag { .. } => Response::Changes(OpChanges::default()),
                _ => Response::Text("x".into()),
            })
        }
    }

    #[test]
    fn tracked_counts_own_mutations_and_typed_helpers_reject_mismatches() {
        let echo = Echo;
        let tracked = Tracked::new(&echo);
        assert!(!tracked.take_own());
        changes(
            &tracked,
            Command::SetFlag {
                card_ids: vec![1],
                flag: 1,
            },
        )
        .unwrap();
        assert!(tracked.take_own());
        assert!(!tracked.take_own());
        assert_eq!(studied_today(&tracked).unwrap(), "x");
        let error = deck_tree(&tracked).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Unexpected response from the collection: Text"
        );
    }
}

/// Every deck as a flat row labelled with its full name, ignoring collapse
/// state; used by the deck-list fuzzy filter.
pub fn all_rows(root: &Deck) -> Vec<DeckRow> {
    fn walk(deck: &Deck, prefix: &str, out: &mut Vec<DeckRow>) {
        let full_name = if prefix.is_empty() {
            deck.name.clone()
        } else {
            format!("{prefix}::{}", deck.name)
        };
        out.push(DeckRow {
            id: deck.id,
            name: full_name.clone(),
            full_name: full_name.clone(),
            depth: 0,
            new: deck.new,
            learning: deck.learning,
            review: deck.review,
            collapsed: false,
            filtered: deck.filtered,
            has_children: false,
        });
        for child in &deck.children {
            walk(child, &full_name, out);
        }
    }
    let mut out = Vec::new();
    for child in &root.children {
        walk(child, "", &mut out);
    }
    out
}
