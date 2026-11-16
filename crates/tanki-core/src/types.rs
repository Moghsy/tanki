use serde::{Deserialize, Serialize};

pub type Result<T> = std::result::Result<T, CoreError>;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoreError {
    Locked,
    UnsafePath(String),
    SchemaTooNew(i64),
    Invalid(String),
    Other(String),
    ActorStopped,
    /// The operation would force a one-way full sync; repeat it confirmed.
    SchemaChangeRequired,
    /// Another process has the collection open although SQLite's lock did
    /// not say so (found under `/proc`, or by tanki's replacement locks).
    /// Same meaning as `Locked`; the text names the processes.
    InUse(String),
    /// A leftover `collection.anki2-wal` (desktop running or crashed), or the
    /// files changed during a replacement. The text says how to proceed.
    ProfileBusy(String),
    /// Cancelled through `CollectionActor::cancel_operation`.
    Interrupted,
}
impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Locked => f.write_str(
                "This profile is open in Anki (or another tanki). Close it and try again.",
            ),
            Self::SchemaTooNew(v) => write!(
                f,
                "Collection schema {v} is unsupported; tanki needs updating."
            ),
            Self::UnsafePath(s) | Self::Invalid(s) | Self::Other(s) => f.write_str(s),
            Self::ActorStopped => f.write_str("Collection actor stopped"),
            Self::SchemaChangeRequired => f.write_str(
                "This change will require a one-way full sync (upload) on the next sync. Confirm to continue.",
            ),
            Self::ProfileBusy(s) | Self::InUse(s) => f.write_str(s),
            Self::Interrupted => f.write_str("Operation cancelled"),
        }
    }
}
impl std::error::Error for CoreError {}
impl From<std::io::Error> for CoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Other(e.to_string())
    }
}
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpChanges {
    pub card: bool,
    pub note: bool,
    pub deck: bool,
    pub tag: bool,
    pub notetype: bool,
    pub config: bool,
    pub deck_config: bool,
    pub mtime: bool,
    pub browser_table: bool,
    pub browser_sidebar: bool,
    pub note_text: bool,
    pub study_queues: bool,
}
impl OpChanges {
    /// After the collection was replaced or reopened: refresh everything.
    pub fn everything() -> Self {
        Self {
            card: true,
            note: true,
            deck: true,
            tag: true,
            notetype: true,
            config: true,
            deck_config: true,
            mtime: true,
            browser_table: true,
            browser_sidebar: true,
            note_text: true,
            study_queues: true,
        }
    }
}
/// One node of the deck tree with study counts (`DeckTree{now}`). The root is
/// a dummy node; display its `children`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Deck {
    pub id: i64,
    /// Last component of the deck name (Anki's tree node name).
    pub name: String,
    /// Counts include children and are capped by the study limits.
    pub new: u32,
    pub learning: u32,
    pub review: u32,
    pub children: Vec<Deck>,
    /// Depth in the tree; the root is 0, top-level decks are 1.
    pub level: u32,
    /// Study-screen collapse state (`DeckCollapseScope::Reviewer`).
    pub collapsed: bool,
    pub filtered: bool,
}
/// Which count the reviewer underlines for a card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum QueueKind {
    #[default]
    New,
    /// Intraday and interday learning both count as learning.
    Learning,
    Review,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueuedCard {
    /// Optional saved code-tour in the note's CodeTour field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_tour: Option<crate::code_tour::Attachment>,
    /// Raw note fields by name, for plugins. Never serialized; a plugin only
    /// receives the fields its manifest declares.
    #[serde(default, skip_serializing)]
    pub fields: std::collections::BTreeMap<String, String>,
    pub id: i64,
    /// Again, Hard, Good, Easy labels from `DescribeNextStates`, localised,
    /// isolation marks stripped.
    pub intervals: Vec<String>,
    pub note_id: i64,
    /// The deck the card is in now (a filtered deck when it is in one).
    pub deck_id: i64,
    pub queue: QueueKind,
    /// User flag 0 (none) to 7.
    pub flag: u8,
    /// The note has the `marked` tag.
    pub marked: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Queue {
    #[serde(default)]
    pub custom_scheduling_error: Option<String>,
    pub new: u32,
    pub learning: u32,
    pub review: u32,
    pub cards: Vec<QueuedCard>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Rating {
    Again,
    Hard,
    Good,
    Easy,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AudioTag {
    SoundOrVideo(String),
    Tts {
        text: String,
        lang: String,
        voices: Vec<String>,
        speed: f32,
        other_args: Vec<String>,
    },
}
/// A card rendered as pylib's `Card.render_output()` does (PLAN 6.0 steps
/// 1-2). `question` and `answer` match Anki's Python output byte for byte
/// (golden test), so they still contain `[[type:…]]` placeholders and
/// `[anki:play:q|a:N]` audio references. The reviewer-side steps that
/// desktop's `reviewer.py` performs are the `review_question` and
/// `review_answer` methods.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RenderedCard {
    /// The note type used to choose a persisted rendering tier.
    #[serde(default)]
    pub notetype_id: i64,
    /// All front template sources, for classification once per note type.
    #[serde(default)]
    pub template_question: String,
    /// All back template sources, for classification once per note type.
    #[serde(default)]
    pub template_answer: String,
    pub question: String,
    pub answer: String,
    pub css: String,
    pub question_audio: Vec<AudioTag>,
    pub answer_audio: Vec<AudioTag>,
    /// Template ordinal, 0-based (body class `card{ord+1}`).
    #[serde(default)]
    pub ord: u32,
    /// The first `[[type:…]]` placeholder on the question side, resolved
    /// against the note as `reviewer.py` `typeAnsQuestionFilter` does. `None`
    /// when the question has no placeholder.
    #[serde(default)]
    pub type_answer: Option<TypeAnswer>,
}
/// What the reviewer does with a `[[type:…]]` placeholder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TypeAnswer {
    /// Show an input; on the answer side compare what was typed with
    /// `expected` (`Command::CompareAnswer`).
    Input {
        /// Raw field HTML, or the cloze text to type for `type:cloze:`.
        expected: String,
        /// False for `type:nc:` (ignore combining marks).
        combining: bool,
    },
    /// The placeholder cannot be used; the question shows this text instead
    /// (unknown field, or a cloze with nothing to type).
    Message(String),
    /// The field is empty; the placeholder is removed.
    Empty,
}
/// HTML that stands for the type-answer input on the question side. The
/// renderer turns exactly this element into its input widget.
pub const TYPE_ANSWER_INPUT: &str = "<center><input type=text id=typeans></center>";

/// Reviewer settings for one card: its home deck's preset plus the
/// collection-wide preferences the reviewer reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StudyConfig {
    // From the deck preset of the card's home deck (original deck if filtered).
    /// Inverse of the preset's `disable_autoplay`.
    pub autoplay: bool,
    pub skip_question_when_replaying_answer: bool,
    pub show_timer: bool,
    pub stop_timer_on_answer: bool,
    /// The answer-time cap in seconds (`cap_answer_time_to_secs`).
    pub cap_answer_time_secs: u32,
    /// Auto-advance delays; 0 disables that side.
    pub seconds_to_show_question: f32,
    pub seconds_to_show_answer: f32,
    pub question_action: QuestionAction,
    pub answer_action: AnswerAction,
    pub wait_for_audio: bool,
    // Collection-wide.
    /// `estTimes`: show next intervals above the answer buttons.
    pub show_intervals: bool,
    /// `dueCounts`: show remaining counts.
    pub show_remaining_count: bool,
    /// `timeLim` in seconds; 0 = no timebox.
    pub timebox_secs: u32,
    /// `interruptAudioWhenAnswering`.
    pub interrupt_audio: bool,
    pub hide_audio_play_buttons: bool,
    /// The custom scheduling script is set; tanki does not run it (PLAN 8.1).
    pub custom_scheduling_set: bool,
    /// Last value used in the reviewer's set-due-date prompt (`setDueReviewer`).
    pub set_due_default: String,
    /// Names of flags 1-7: custom labels or Anki's localised defaults.
    pub flag_names: Vec<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum QuestionAction {
    #[default]
    ShowAnswer,
    ShowReminder,
}
/// Order matches the proto enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AnswerAction {
    #[default]
    BuryCard,
    AnswerAgain,
    AnswerGood,
    AnswerHard,
    ShowReminder,
}
/// Outcome of `Command::Answer`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerOutcome {
    pub changes: OpChanges,
    /// The new state is a leech (`StateIsLeech`).
    pub leech: bool,
    /// The card is suspended after answering (leech action "suspend").
    pub suspended: bool,
}
/// The overview screen of the current deck.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeckOverview {
    pub deck_id: i64,
    /// Full deck name with `::` separators.
    pub name: String,
    pub filtered: bool,
    /// Counts from the study queue (what Study Now will show).
    pub new: u32,
    pub learning: u32,
    pub review: u32,
    /// Deck-tree count minus queue count per column (desktop's `+N` hint for
    /// buried cards); 0 when they agree.
    pub new_delta: i32,
    pub learning_delta: i32,
    pub review_delta: i32,
    /// Description as HTML (Markdown already converted when the deck's flag
    /// says so); for a filtered deck, Anki's fixed explanatory text.
    pub description: String,
    pub have_user_buried: bool,
    pub have_sched_buried: bool,
}
/// `CongratsInfo`: shown when the current deck has nothing left today.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Congrats {
    pub learn_remaining: u32,
    /// 86400 means no more learning cards today.
    pub secs_until_next_learn: u32,
    pub review_remaining: bool,
    pub new_remaining: bool,
    pub have_sched_buried: bool,
    pub have_user_buried: bool,
    pub is_filtered_deck: bool,
    /// Rendered HTML.
    pub deck_description: String,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchedTiming {
    pub days_elapsed: u32,
    /// Unix seconds of the next day rollover; re-query the deck list then.
    pub next_day_at: i64,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchedulerStatus {
    /// The v3 scheduler is enabled; studying needs it.
    pub v3: bool,
    /// Stored scheduler version (1 or 2).
    pub version: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    Cards(Vec<i64>),
    Notes(Vec<i64>),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuryMode {
    /// Manual bury (`BURY_USER`).
    Bury,
    Suspend,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnburyMode {
    UserOnly,
    SchedOnly,
    All,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgetDefaults {
    pub restore_position: bool,
    pub reset_counts: bool,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoStatus {
    /// Label of the step `Undo` would revert, e.g. "Answer Card".
    pub undo: Option<String>,
    pub redo: Option<String>,
}
/// Requests to the collection actor. Each returns exactly one `Response`
/// variant, named in its doc comment. Commands that change the collection
/// return `OpChanges` (inside `Changes`, `NoteAdded`, `Answered` or `Undone`),
/// which the actor also broadcasts to subscribers, and they invalidate the
/// cached scheduling states: fetch a fresh `Queue` before the next `Answer`.
#[derive(Debug, Clone)]
pub enum Command {
    /// Opt in/out of external legacy LaTeX image generation -> `Changes`.
    SetLatexGeneration {
        enabled: bool,
    },
    Data(crate::DataCommand),
    /// Notes, note types, tags, media (Phase 3) -> `Edit`.
    Notes(crate::NoteCommand),
    /// Search and browser (Phase 3) -> `Edit`.
    Browse(crate::BrowseCommand),
    /// Management screens (Phase 5) -> `Manage`.
    Manage(crate::ManageCommand),
    /// -> `DeckTree` (with counts; rolls the day over if needed).
    DeckTree,
    /// Cards of the current deck -> `Queue`. Idempotent; caches each card's
    /// scheduling states (with `custom_data` copied in) for `Answer`.
    Queue {
        limit: u32,
    },
    /// -> `Rendered`.
    Render {
        card_id: i64,
    },
    /// Answers a card from the last `Queue` -> `Answered`.
    Answer {
        card_id: i64,
        rating: Rating,
        answered_at_millis: i64,
        /// Uncapped; Anki caps it with the preset's answer-time limit.
        milliseconds_taken: u32,
    },
    /// -> `Undone`.
    Undo,
    /// -> `Undone`.
    Redo,
    /// -> `UndoStatus`.
    UndoStatus,
    /// Adds a note to deck 1 -> `NoteAdded`.
    AddNote {
        notetype: String,
        fields: Vec<String>,
    },
    /// -> `CardIds`.
    Search {
        query: String,
    },
    /// Forced backup; closes and reopens the collection -> `Backup`.
    Backup,
    /// -> `CardIds` (all cards).
    GenerateFixture,
    /// Adds the study fixture (`fixture::populate_study`): a deck tree under
    /// "Study" with new, learning and due review cards, media, presets and a
    /// filtered deck, and makes "Study" the current deck -> `CardIds` (all
    /// cards of the collection).
    GenerateStudyFixture,

    // Decks, overview, congrats.
    /// Makes `deck_id` the current deck -> `Overview`.
    SelectDeck {
        deck_id: i64,
    },
    /// The current deck -> `Overview`.
    Overview,
    /// -> `Changes`.
    SetDeckCollapsed {
        deck_id: i64,
        collapsed: bool,
    },
    /// The current deck -> `Congrats`.
    Congrats,
    /// Localised "Studied N cards in M minutes today." -> `Text`.
    StudiedToday,
    /// -> `SchedTiming`.
    SchedTiming,
    /// -> `SchedulerStatus`.
    SchedulerStatus,
    /// Upgrades to the v3 scheduler. Changes the schema (full sync); the
    /// caller confirms first -> `Changes`.
    UpgradeScheduler,

    // Reviewer.
    /// -> `StudyConfig`.
    StudyConfig {
        card_id: i64,
    },
    /// `CompareAnswer` -> `Html` (the `<code id=typeans>` diff).
    CompareAnswer {
        expected: String,
        provided: String,
        combining: bool,
    },
    /// Flag 0 clears -> `Changes`.
    SetFlag {
        card_ids: Vec<i64>,
        flag: u8,
    },
    /// Adds or removes the `marked` tag -> `Changes`.
    SetMarked {
        note_ids: Vec<i64>,
        marked: bool,
    },
    /// -> `Changes`.
    BuryOrSuspend {
        target: Target,
        mode: BuryMode,
    },
    /// -> `Changes`.
    Unbury {
        deck_id: i64,
        mode: UnburyMode,
    },
    /// Reviewer defaults for the forget dialog -> `ForgetDefaults`.
    ForgetDefaults,
    /// Reset to new -> `Changes`.
    Forget {
        card_ids: Vec<i64>,
        restore_position: bool,
        reset_counts: bool,
    },
    /// `days` uses Anki's syntax (`N`, `N-M`, `N!`); remembered as the
    /// reviewer default -> `Changes`.
    SetDueDate {
        card_ids: Vec<i64>,
        days: String,
    },
    /// -> `Changes`.
    DeleteNotes {
        note_ids: Vec<i64>,
    },
}
#[derive(Debug, Clone)]
pub enum Response {
    Data(crate::DataResponse),
    Edit(crate::EditResponse),
    Manage(crate::ManageResponse),
    DeckTree(Deck),
    Queue(Queue),
    Rendered(RenderedCard),
    Changes(OpChanges),
    NoteAdded { id: i64, changes: OpChanges },
    CardIds(Vec<i64>),
    Backup,
    Answered(AnswerOutcome),
    Undone { changes: OpChanges, label: String },
    UndoStatus(UndoStatus),
    Overview(DeckOverview),
    Congrats(Congrats),
    Text(String),
    Html(String),
    SchedTiming(SchedTiming),
    SchedulerStatus(SchedulerStatus),
    StudyConfig(StudyConfig),
    ForgetDefaults(ForgetDefaults),
}
impl Response {
    /// The changes a mutating command made; the actor broadcasts these.
    pub fn changes(&self) -> Option<&OpChanges> {
        match self {
            Self::Changes(changes)
            | Self::NoteAdded { changes, .. }
            | Self::Undone { changes, .. }
            | Self::Answered(AnswerOutcome { changes, .. })
            | Self::Data(crate::DataResponse::Imported { changes, .. }) => Some(changes),
            Self::Edit(edit) => edit.changes(),
            Self::Manage(manage) => manage.changes(),
            _ => None,
        }
    }
}

/// Calls `replace` with the inner text of every `[[type:…]]` placeholder
/// (regex `\[\[type:(.+?)\]\]`, as in `reviewer.py`) and substitutes the
/// result. Returns `None` when there is no placeholder.
fn replace_type_placeholders(buf: &str, mut replace: impl FnMut(&str) -> String) -> Option<String> {
    const OPEN: &str = "[[type:";
    let mut out = String::with_capacity(buf.len());
    let mut rest = buf;
    let mut found = false;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        // `.+?\]\]`: at least one character, no newline, shortest match.
        let line = &after[..after.find('\n').unwrap_or(after.len())];
        let close = line
            .chars()
            .next()
            .map(char::len_utf8)
            .and_then(|first| Some(line[first..].find("]]")? + first));
        match close {
            Some(end) => {
                found = true;
                out.push_str(&rest[..start]);
                out.push_str(&replace(&after[..end]));
                rest = &after[end + 2..];
            }
            None => {
                out.push_str(&rest[..start + 1]);
                rest = &rest[start + 1..];
            }
        }
    }
    out.push_str(rest);
    found.then_some(out)
}

/// Inner text of the first `[[type:…]]` placeholder (`re.search` in
/// `reviewer.py` `typeAnsQuestionFilter`).
pub(crate) fn first_type_placeholder(buf: &str) -> Option<String> {
    let mut first = None;
    replace_type_placeholders(buf, |inner| {
        first.get_or_insert_with(|| inner.to_owned());
        String::new()
    });
    first
}

impl RenderedCard {
    /// Body classes as desktop sets them (`theme.py` `body_classes_for_card_ord`).
    pub fn body_classes(&self, night_mode: bool) -> String {
        let platform = if cfg!(target_os = "macos") {
            "isMac"
        } else if cfg!(windows) {
            "isWin"
        } else {
            "isLin"
        };
        let mut classes = format!("card card{} {platform}", self.ord + 1);
        if night_mode {
            classes.push_str(" nightMode night_mode");
        }
        classes
    }
    /// The question as the reviewer shows it (`typeAnsQuestionFilter`): every
    /// `[[type:…]]` becomes [`TYPE_ANSWER_INPUT`], the message, or nothing.
    pub fn review_question(&self) -> String {
        let replacement = match &self.type_answer {
            None => return self.question.clone(),
            Some(TypeAnswer::Input { .. }) => TYPE_ANSWER_INPUT.to_owned(),
            Some(TypeAnswer::Message(message)) => message.clone(),
            Some(TypeAnswer::Empty) => String::new(),
        };
        replace_type_placeholders(&self.question, |_| replacement.clone())
            .unwrap_or_else(|| self.question.clone())
    }
    /// The answer as the reviewer shows it (`typeAnsAnswerFilter`).
    /// `comparison` is the `Command::CompareAnswer` output for what was typed;
    /// it is only used when `type_answer` is `Input`. Otherwise placeholders
    /// are removed. When the answer has `<hr id=answer>`, the divider moves to
    /// just before the comparison.
    pub fn review_answer(&self, comparison: &str) -> String {
        let answer = &self.answer;
        if !matches!(self.type_answer, Some(TypeAnswer::Input { .. })) {
            return replace_type_placeholders(answer, |_| String::new())
                .unwrap_or_else(|| answer.clone());
        }
        let without_hr = answer.replace("<hr id=answer>", "");
        let had_hr = without_hr.len() != answer.len();
        let block = format!("\n<div>{comparison}</div>");
        let block = if had_hr {
            format!("<hr id=answer>{block}")
        } else {
            block
        };
        match replace_type_placeholders(&without_hr, |_| block.clone()) {
            Some(replaced) => replaced,
            // `hadHR and not re.search(...)`: keep the original. Without the
            // divider and without a placeholder nothing changes either.
            None => answer.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(question: &str, answer: &str, type_answer: Option<TypeAnswer>) -> RenderedCard {
        RenderedCard {
            question: question.into(),
            answer: answer.into(),
            type_answer,
            ..Default::default()
        }
    }
    const INPUT: Option<TypeAnswer> = Some(TypeAnswer::Input {
        expected: String::new(),
        combining: true,
    });

    #[test]
    fn placeholders_follow_the_regex() {
        let ids = |s: &str| {
            let mut seen = vec![];
            replace_type_placeholders(s, |inner| {
                seen.push(inner.to_owned());
                String::new()
            });
            seen
        };
        assert_eq!(ids("a [[type:Back]] b [[type:nc:F]]"), ["Back", "nc:F"]);
        assert!(ids("[[type:]]").is_empty());
        assert_eq!(ids("[[type:]]]"), ["]"]);
        assert!(ids("[[type:a\nb]]").is_empty());
        assert_eq!(ids("[[type:x]]]"), ["x"]);
    }

    #[test]
    fn question_side() {
        let q = "{Q}\n\n[[type:Back]]";
        assert_eq!(card(q, "", None).review_question(), q);
        assert_eq!(
            card(q, "", INPUT).review_question(),
            format!("{{Q}}\n\n{TYPE_ANSWER_INPUT}")
        );
        assert_eq!(
            card(q, "", Some(TypeAnswer::Message("unknown".into()))).review_question(),
            "{Q}\n\nunknown"
        );
        assert_eq!(
            card(q, "", Some(TypeAnswer::Empty)).review_question(),
            "{Q}\n\n"
        );
    }

    #[test]
    fn answer_side_moves_divider() {
        // Stock "Basic (type in the answer)" back template.
        let a = "Q\n\n<hr id=answer>\n\n[[type:Back]]";
        assert_eq!(
            card("", a, INPUT).review_answer("<code id=typeans>x</code>"),
            "Q\n\n\n\n<hr id=answer>\n<div><code id=typeans>x</code></div>"
        );
        // No input: placeholders removed, divider kept.
        assert_eq!(
            card("", a, None).review_answer("x"),
            "Q\n\n<hr id=answer>\n\n"
        );
        // Divider but no placeholder: unchanged.
        let plain = "Q<hr id=answer>A";
        assert_eq!(card("", plain, INPUT).review_answer("x"), plain);
        // No divider.
        assert_eq!(
            card("", "A [[type:B]]", INPUT).review_answer("x"),
            "A \n<div>x</div>"
        );
    }

    #[test]
    fn body_classes() {
        let mut c = RenderedCard {
            ord: 1,
            ..Default::default()
        };
        assert!(c.body_classes(false).starts_with("card card2 is"));
        c.ord = 0;
        assert!(c.body_classes(true).ends_with(" nightMode night_mode"));
    }
}
