//! The reviewer state machine: a port of desktop's `reviewer.py` (R2 §1.2,
//! §2, §9). It talks to the collection through [`Backend`] and returns
//! [`Effect`]s for everything the screen does (audio, notices, leaving the
//! reviewer), so it runs without a terminal and is tested with a fake backend.
use crate::backend::{self, Backend};
use crate::input::TextInput;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};
use tanki_core::{
    AnswerAction, AudioTag, Command, OpChanges, QuestionAction, QueuedCard, Rating, RenderedCard,
    StudyConfig, Target, TypeAnswer,
};
use tanki_media::rules::Side;
use tanki_media::MediaTag;

/// The time of an event: a monotonic instant for durations and the wall
/// clock for `answered_at_millis` and the day rollover.
#[derive(Debug, Clone, Copy)]
pub struct Now {
    pub at: Instant,
    pub wall_ms: i64,
}

impl Now {
    pub fn current() -> Self {
        let wall_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        Self {
            at: Instant::now(),
            wall_ms,
        }
    }
    /// This moment plus `millis` on both clocks.
    pub fn plus_millis(self, millis: u64) -> Self {
        Self {
            at: self.at + Duration::from_millis(millis),
            wall_ms: self.wall_ms + millis as i64,
        }
    }
}

/// `reviewer.py` `self.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Question,
    Answer,
    /// An answer is being recorded or the timebox prompt is open; keys are ignored.
    Transition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CueKind {
    ShowQuestion,
    ShowAnswer,
    Replay(Side),
}

/// What to play; the audio adapter applies `tanki_media::rules`.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioCue {
    pub kind: CueKind,
    pub question: Vec<MediaTag>,
    pub answer: Vec<MediaTag>,
    pub autoplay: bool,
    pub skip_question_when_replaying_answer: bool,
    pub interrupt: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Audio(AudioCue),
    /// A transient message for the status line.
    Notice(String),
    Error(String),
    /// A warning that stays visible for the session.
    Warning(String),
    /// No card left: show the congrats screen.
    Finished,
    /// `check_timebox`: ask Continue / Finish.
    Timebox {
        cards: u32,
        minutes: u32,
    },
    /// The collection is not on the v3 scheduler; offer `UpgradeScheduler`.
    SchedulerUpgradeRequired,
    /// A different card or side is shown: reset scrolling.
    CardChanged,
}

pub const SCHEDULER_UPGRADE_MESSAGE: &str = "Your collection needs to be upgraded to the v3 scheduler before you can study. Upgrading changes the collection schema, so the next sync will be a one-way full sync.";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub new: u32,
    pub learning: u32,
    pub review: u32,
}

#[derive(Debug, Clone)]
pub struct ReviewCard {
    pub queued: QueuedCard,
    pub rendered: RenderedCard,
    pub config: StudyConfig,
    pub question_audio: Vec<MediaTag>,
    pub answer_audio: Vec<MediaTag>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AutoTimer {
    due: Instant,
    /// The side the timer leads to: `Answer` from the question
    /// (`_on_show_answer_timeout`), `Question` from the answer.
    toward: Phase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerView {
    pub secs: u64,
    /// The answer-time cap was reached (desktop shows it in red).
    pub capped: bool,
}

/// `tanki_core::AudioTag` -> `tanki_media::MediaTag`.
pub fn media_tag(tag: &AudioTag) -> MediaTag {
    match tag {
        AudioTag::SoundOrVideo(name) => MediaTag::Sound(name.clone()),
        AudioTag::Tts {
            text,
            lang,
            voices,
            speed,
            ..
        } => MediaTag::Tts {
            text: text.clone(),
            lang: lang.clone(),
            voices: voices.clone(),
            speed: *speed,
        },
    }
}

pub struct Reviewer {
    phase: Phase,
    hold_at: Option<Instant>,
    held: Duration,
    timebox_held: Duration,
    held_invalid: bool,
    answer_active: Option<Duration>,
    card: Option<ReviewCard>,
    /// Rendering the next queued card never answers or removes it. The UI
    /// warms its optional bitmap cache while this card is on screen.
    next_rendered: Option<RenderedCard>,
    counts: Counts,
    shown_at: Instant,
    answer_at: Option<Instant>,
    typed: Option<TextInput>,
    comparison: Option<String>,
    timebox_start: Instant,
    timebox_reps: u32,
    timebox_pending: bool,
    auto_advance: bool,
    auto: Option<AutoTimer>,
    next_day_at: Option<i64>,
    /// Hint and `<details>` widgets the user opened (renderer keys).
    pub expanded: BTreeSet<String>,
}

impl Reviewer {
    pub fn new(now: Now) -> Self {
        Self {
            phase: Phase::Question,
            hold_at: None,
            held: Duration::ZERO,
            timebox_held: Duration::ZERO,
            held_invalid: false,
            answer_active: None,
            card: None,
            next_rendered: None,
            counts: Counts::default(),
            shown_at: now.at,
            answer_at: None,
            typed: None,
            comparison: None,
            timebox_start: now.at,
            timebox_reps: 0,
            timebox_pending: false,
            auto_advance: false,
            auto: None,
            next_day_at: None,
            expanded: BTreeSet::new(),
        }
    }

    // ---- read access for the screen -------------------------------------

    pub fn holding(&self) -> bool {
        self.hold_at.is_some()
    }
    fn active_duration(&self, end: Instant) -> Duration {
        let end = self.hold_at.map_or(end, |start| end.min(start));
        end.saturating_duration_since(self.shown_at)
            .saturating_sub(self.held)
    }
    pub fn begin_tour(&mut self, now: Now) -> bool {
        if self.holding() || self.phase != Phase::Answer || self.card.is_none() {
            return false;
        }
        self.hold_at = Some(now.at);
        self.held_invalid = false;
        self.auto = None;
        self.auto_advance = false;
        true
    }
    pub fn end_tour(&mut self, b: &dyn Backend, now: Now) -> Vec<Effect> {
        let Some(start) = self.hold_at.take() else {
            return Vec::new();
        };
        let duration = now.at.saturating_duration_since(start);
        self.held += duration;
        self.timebox_held += duration;
        let mut fx = Vec::new();
        let rollover = self.next_day_at.is_some_and(|t| now.wall_ms / 1000 >= t);
        if self.held_invalid || rollover {
            // If refreshing fails, Transition continues to block stale ratings.
            self.phase = Phase::Transition;
            if rollover {
                self.next_day_at = backend::sched_timing(b)
                    .ok()
                    .map(|t| t.next_day_at)
                    .filter(|t| now.wall_ms / 1000 < *t);
            }
            self.refresh(b, now, false, &mut fx);
            if !fx.iter().any(|effect| matches!(effect, Effect::Error(_))) {
                fx.push(Effect::Notice(
                    "Card or collection changed while the tour was open; review reloaded".into(),
                ));
            }
        }
        self.held_invalid = false;
        fx
    }
    pub fn phase(&self) -> Phase {
        self.phase
    }
    pub fn card(&self) -> Option<&ReviewCard> {
        self.card.as_ref()
    }
    pub fn next_rendered(&self) -> Option<&RenderedCard> {
        self.next_rendered.as_ref()
    }
    pub fn counts(&self) -> Counts {
        self.counts
    }
    pub fn auto_advance(&self) -> bool {
        self.auto_advance
    }
    pub fn timebox_pending(&self) -> bool {
        self.timebox_pending
    }
    /// The type-answer input while the question is shown.
    pub fn typed(&self) -> Option<&TextInput> {
        self.typed
            .as_ref()
            .filter(|_| self.phase == Phase::Question)
    }
    pub fn typed_mut(&mut self) -> Option<&mut TextInput> {
        if self.phase == Phase::Question {
            self.typed.as_mut()
        } else {
            None
        }
    }
    /// The HTML of the side being shown, after the reviewer filters.
    pub fn side_html(&self) -> Option<String> {
        let card = self.card.as_ref()?;
        Some(match self.phase {
            Phase::Question => card.rendered.review_question(),
            Phase::Answer | Phase::Transition => card
                .rendered
                .review_answer(self.comparison.as_deref().unwrap_or("")),
        })
    }
    pub fn showing_answer(&self) -> bool {
        self.phase != Phase::Question
    }
    /// The on-screen timer (`reviewer-bottom.ts`), when the preset shows it.
    pub fn timer(&self, now: Instant) -> Option<TimerView> {
        let card = self.card.as_ref()?;
        let config = &card.config;
        if !config.show_timer {
            return None;
        }
        let end = match self.answer_at {
            Some(at) if config.stop_timer_on_answer && self.phase != Phase::Question => at,
            _ => now,
        };
        let secs = if config.stop_timer_on_answer && self.phase != Phase::Question {
            self.answer_active.unwrap_or_default()
        } else {
            self.active_duration(end)
        }
        .as_secs();
        let cap = u64::from(config.cap_answer_time_secs);
        Some(if cap > 0 && secs >= cap {
            TimerView {
                secs: cap,
                capped: true,
            }
        } else {
            TimerView {
                secs,
                capped: false,
            }
        })
    }
    /// One audio tag of the current card (replay widgets).
    pub fn tag(&self, side: Side, index: usize) -> Option<MediaTag> {
        let card = self.card.as_ref()?;
        match side {
            Side::Question => card.question_audio.get(index).cloned(),
            Side::Answer => card.answer_audio.get(index).cloned(),
        }
    }

    // ---- fetching and showing cards --------------------------------------

    /// `Reviewer.show()` after Study Now: starts the timebox, checks the
    /// scheduler version and shows the first card.
    pub fn start(&mut self, b: &dyn Backend, now: Now) -> Vec<Effect> {
        self.timebox_start = now.at;
        self.timebox_held = Duration::ZERO;
        self.timebox_reps = 0;
        self.timebox_pending = false;
        self.next_day_at = backend::sched_timing(b).ok().map(|t| t.next_day_at);
        match backend::scheduler_status(b) {
            Ok(status) if !status.v3 => return vec![Effect::SchedulerUpgradeRequired],
            Err(error) => return vec![Effect::Error(error.to_string())],
            Ok(_) => {}
        }
        self.next_card(b, now)
    }

    /// After the user confirmed the upgrade offer.
    pub fn upgrade_scheduler(&mut self, b: &dyn Backend, now: Now) -> Vec<Effect> {
        match backend::changes(b, Command::UpgradeScheduler) {
            Ok(_) => {
                let mut fx = vec![Effect::Notice("Upgraded to the v3 scheduler.".into())];
                fx.extend(self.next_card(b, now));
                fx
            }
            Err(e) => vec![Effect::Error(e.to_string())],
        }
    }

    /// `nextCard()`: fetch the top of the queue and show its question.
    pub fn next_card(&mut self, b: &dyn Backend, now: Now) -> Vec<Effect> {
        let mut fx = Vec::new();
        self.refresh(b, now, false, &mut fx);
        fx
    }

    /// Fetches a fresh queue (required after any mutation before the next
    /// `Answer`). With `keep_same`, a card that is still on top keeps its
    /// phase and timer; only its queue data (counts, flag, intervals) update.
    fn refresh(&mut self, b: &dyn Backend, now: Now, keep_same: bool, fx: &mut Vec<Effect>) {
        self.next_rendered = None;
        let queue = match backend::queue(b, 2) {
            Ok(queue) => queue,
            Err(e) => {
                fx.push(Effect::Error(e.to_string()));
                return;
            }
        };
        if let Some(error) = queue.custom_scheduling_error.as_ref() {
            fx.push(Effect::Warning(error.clone()));
        }
        self.counts = Counts {
            new: queue.new,
            learning: queue.learning,
            review: queue.review,
        };
        let mut queued = queue.cards.into_iter();
        let Some(top) = queued.next() else {
            self.card = None;
            self.auto = None;
            self.typed = None;
            fx.push(Effect::Finished);
            return;
        };
        if let Some(next) = queued.next() {
            self.next_rendered = backend::render(b, next.id).ok();
        }
        if keep_same && self.phase != Phase::Transition {
            if let Some(card) = self.card.as_mut().filter(|c| c.queued.id == top.id) {
                card.queued = top;
                return;
            }
        }
        match self.load(b, top, fx) {
            Ok(card) => {
                self.card = Some(card);
                self.show_question(now, fx);
            }
            Err(e) => {
                self.card = None;
                self.auto = None;
                fx.push(Effect::Error(e.to_string()));
            }
        }
    }

    fn load(
        &mut self,
        b: &dyn Backend,
        queued: QueuedCard,
        _fx: &mut Vec<Effect>,
    ) -> backend::Result<ReviewCard> {
        let rendered = backend::render(b, queued.id)?;
        let config = backend::study_config(b, queued.id)?;
        Ok(ReviewCard {
            question_audio: rendered.question_audio.iter().map(media_tag).collect(),
            answer_audio: rendered.answer_audio.iter().map(media_tag).collect(),
            queued,
            rendered,
            config,
        })
    }

    fn cue(&self, kind: CueKind) -> Option<Effect> {
        let card = self.card.as_ref()?;
        Some(Effect::Audio(AudioCue {
            kind,
            question: card.question_audio.clone(),
            answer: card.answer_audio.clone(),
            autoplay: card.config.autoplay,
            skip_question_when_replaying_answer: card.config.skip_question_when_replaying_answer,
            interrupt: card.config.interrupt_audio,
        }))
    }

    /// `_showQuestion()`.
    fn show_question(&mut self, now: Now, fx: &mut Vec<Effect>) {
        self.phase = Phase::Question;
        self.shown_at = now.at;
        self.held = Duration::ZERO;
        self.answer_active = None;
        self.answer_at = None;
        self.comparison = None;
        self.expanded.clear();
        let Some(card) = self.card.as_ref() else {
            return;
        };
        self.typed = matches!(card.rendered.type_answer, Some(TypeAnswer::Input { .. }))
            .then(TextInput::default);

        fx.push(Effect::CardChanged);
        fx.extend(self.cue(CueKind::ShowQuestion));

        self.schedule_auto(now);
    }

    /// Space/Enter on the question side (`_getTypedAnswer` -> `_showAnswer`).
    pub fn show_answer(&mut self, b: &dyn Backend, now: Now) -> Vec<Effect> {
        let mut fx = Vec::new();
        if self.phase != Phase::Question {
            return fx;
        }
        let Some(card) = self.card.as_ref() else {
            return fx;
        };
        if let Some(TypeAnswer::Input {
            expected,
            combining,
        }) = &card.rendered.type_answer
        {
            let provided = self
                .typed
                .as_ref()
                .map(|t| t.text().to_owned())
                .unwrap_or_default();
            match backend::compare_answer(b, expected.clone(), provided.clone(), *combining) {
                Ok(html) => self.comparison = Some(html),
                Err(e) => {
                    fx.push(Effect::Error(format!("Could not compare the answer: {e}")));
                    self.comparison = Some(format!(
                        "<code id=typeans>{}</code>",
                        escape_html(&provided)
                    ));
                }
            }
        }
        self.phase = Phase::Answer;
        self.answer_at = Some(now.at);
        self.answer_active = Some(self.active_duration(now.at));
        fx.push(Effect::CardChanged);
        fx.extend(self.cue(CueKind::ShowAnswer));
        self.schedule_auto(now);
        fx
    }

    /// Space/Enter: show the answer, or answer Good on the answer side.
    pub fn enter(&mut self, b: &dyn Backend, now: Now) -> Vec<Effect> {
        match self.phase {
            Phase::Question => self.show_answer(b, now),
            Phase::Answer => self.answer(b, Rating::Good, now),
            Phase::Transition => Vec::new(),
        }
    }

    /// `_answerCard(ease)`.
    pub fn answer(&mut self, b: &dyn Backend, rating: Rating, now: Now) -> Vec<Effect> {
        let mut fx = Vec::new();
        if self.holding() || self.phase != Phase::Answer {
            return fx;
        }
        let Some(card) = self.card.as_ref() else {
            return fx;
        };
        let card_id = card.queued.id;
        let timebox_secs = card.config.timebox_secs;
        let taken = self.active_duration(now.at).as_millis();
        self.phase = Phase::Transition;
        self.auto = None;
        let outcome = match backend::answer(
            b,
            card_id,
            rating,
            now.wall_ms,
            u32::try_from(taken).unwrap_or(u32::MAX),
        ) {
            Ok(outcome) => outcome,
            Err(e) => {
                self.phase = Phase::Answer;
                fx.push(Effect::Error(e.to_string()));
                return fx;
            }
        };
        if outcome.leech {
            let mut message = String::from("Card was a leech.");
            if outcome.suspended {
                message.push_str(" It has been suspended.");
            }
            fx.push(Effect::Notice(message));
        }
        self.timebox_reps += 1;
        // `col.timeboxReached()`: reports the limit, not the elapsed time.
        let elapsed = now
            .at
            .saturating_duration_since(self.timebox_start)
            .saturating_sub(self.timebox_held);
        if timebox_secs > 0 && elapsed > Duration::from_secs(u64::from(timebox_secs)) {
            self.timebox_pending = true;
            fx.push(Effect::Timebox {
                cards: self.timebox_reps,
                minutes: (f64::from(timebox_secs) / 60.0).round() as u32,
            });
            return fx;
        }
        self.refresh(b, now, false, &mut fx);
        fx
    }

    /// Timebox prompt: Continue restarts the timebox and shows the next card.
    pub fn timebox_continue(&mut self, b: &dyn Backend, now: Now) -> Vec<Effect> {
        if !self.timebox_pending {
            return Vec::new();
        }
        self.timebox_pending = false;
        self.timebox_start = now.at;
        self.timebox_held = Duration::ZERO;
        self.timebox_reps = 0;
        self.next_card(b, now)
    }
    /// Timebox prompt: Finish (the screen goes to the deck list).
    pub fn timebox_finish(&mut self) {
        self.timebox_pending = false;
        self.auto = None;
    }

    /// Undo (`u`, Ctrl+Z) or redo, then a fresh queue.
    pub fn undo(&mut self, b: &dyn Backend, redo: bool, now: Now) -> Vec<Effect> {
        if self.holding() || self.phase == Phase::Transition {
            return Vec::new();
        }
        let mut fx = Vec::new();
        match backend::undo_redo(b, redo) {
            Ok(label) => {
                fx.push(Effect::Notice(undo_notice(&label, redo)));
                self.refresh(b, now, false, &mut fx);
            }
            Err(e) => fx.push(Effect::Error(e.to_string())),
        }
        fx
    }

    /// `OpChanges` from elsewhere (`op_executed` / `refresh_if_needed`).
    pub fn on_changes(&mut self, b: &dyn Backend, changes: &OpChanges, now: Now) -> Vec<Effect> {
        let mut fx = Vec::new();
        if self.holding() {
            self.held_invalid |= changes.card
                || changes.note
                || changes.note_text
                || changes.notetype
                || changes.config
                || changes.deck_config
                || changes.study_queues
                || changes.deck;
            return fx;
        }
        if self.timebox_pending || self.phase == Phase::Transition {
            return fx;
        }
        if changes.study_queues {
            self.refresh(b, now, false, &mut fx);
        } else if changes.note_text {
            if let Some(id) = self.card.as_ref().map(|c| c.queued.id) {
                match backend::render(b, id) {
                    Ok(rendered) => {
                        if let Some(card) = self.card.as_mut() {
                            card.question_audio =
                                rendered.question_audio.iter().map(media_tag).collect();
                            card.answer_audio =
                                rendered.answer_audio.iter().map(media_tag).collect();
                            card.rendered = rendered;
                        }
                        fx.push(Effect::CardChanged);
                    }
                    Err(e) => fx.push(Effect::Error(e.to_string())),
                }
            }
            self.refresh(b, now, true, &mut fx);
        } else if changes.card || changes.note {
            self.refresh(b, now, true, &mut fx);
        }
        fx
    }

    // ---- audio -------------------------------------------------------------

    /// `r` / F5 (`replayAudio`).
    pub fn replay(&self) -> Vec<Effect> {
        let side = match self.phase {
            Phase::Question => Side::Question,
            Phase::Answer => Side::Answer,
            Phase::Transition => return Vec::new(),
        };
        self.cue(CueKind::Replay(side)).into_iter().collect()
    }

    // ---- auto-advance, rollover ------------------------------------------

    fn schedule_auto(&mut self, now: Now) {
        self.auto = None;
        if !self.auto_advance {
            return;
        }
        let Some(card) = self.card.as_ref() else {
            return;
        };
        let (secs, toward) = match self.phase {
            Phase::Question => (card.config.seconds_to_show_question, Phase::Answer),
            Phase::Answer => (card.config.seconds_to_show_answer, Phase::Question),
            Phase::Transition => return,
        };
        if secs > 0.0 {
            self.auto = Some(AutoTimer {
                due: now.at + Duration::from_millis((secs * 1000.0) as u64),
                toward,
            });
        }
    }

    /// Shift+A.
    pub fn toggle_auto_advance(&mut self, now: Now) -> Vec<Effect> {
        self.auto_advance = !self.auto_advance;
        self.schedule_auto(now);
        vec![Effect::Notice(
            if self.auto_advance {
                "Auto Advance enabled"
            } else {
                "Auto Advance disabled"
            }
            .into(),
        )]
    }

    /// Called on every timer tick. Handles the day rollover and fires an
    /// auto-advance timer that has run out. With `wait_for_audio`, an
    /// expired timer waits while audio plays; [`Reviewer::audio_idle`]
    /// fires it later (`_on_av_player_did_end_playing`).
    pub fn tick(
        &mut self,
        b: &dyn Backend,
        now: Now,
        audio_playing: bool,
        focused: bool,
    ) -> Vec<Effect> {
        if self.holding() {
            return Vec::new();
        }
        if self.next_day_at.is_some_and(|t| now.wall_ms / 1000 >= t) {
            return self.rollover(b, now);
        }
        let Some(timer) = self.auto else {
            return Vec::new();
        };
        if now.at < timer.due {
            return Vec::new();
        }
        let wait = self.card.as_ref().is_some_and(|c| c.config.wait_for_audio);
        if wait && audio_playing {
            return Vec::new();
        }
        self.fire_auto(b, timer, now, focused)
    }

    /// The audio queue ran out (`AudioEvent::Idle`).
    pub fn audio_idle(&mut self, b: &dyn Backend, now: Now, focused: bool) -> Vec<Effect> {
        if self.holding() {
            return Vec::new();
        }
        match self.auto {
            Some(timer) if now.at >= timer.due => self.fire_auto(b, timer, now, focused),
            _ => Vec::new(),
        }
    }

    fn fire_auto(
        &mut self,
        b: &dyn Backend,
        timer: AutoTimer,
        now: Now,
        focused: bool,
    ) -> Vec<Effect> {
        self.auto = None;
        if !self.auto_advance {
            return Vec::new();
        }
        if !focused {
            // Desktop turns auto-advance off when its window lost focus.
            self.auto_advance = false;
            return vec![Effect::Notice("Auto Advance disabled".into())];
        }
        let Some(config) = self.card.as_ref().map(|c| c.config.clone()) else {
            return Vec::new();
        };
        match (timer.toward, self.phase) {
            (Phase::Answer, Phase::Question) => match config.question_action {
                QuestionAction::ShowAnswer => self.show_answer(b, now),
                QuestionAction::ShowReminder => {
                    vec![Effect::Notice("Question time elapsed".into())]
                }
            },
            (Phase::Question, Phase::Answer) => match config.answer_action {
                AnswerAction::AnswerAgain => self.answer(b, Rating::Again, now),
                AnswerAction::AnswerHard => self.answer(b, Rating::Hard, now),
                AnswerAction::AnswerGood => self.answer(b, Rating::Good, now),
                AnswerAction::ShowReminder => vec![Effect::Notice("Answer time elapsed".into())],
                AnswerAction::BuryCard => self.bury_card(b, now),
            },
            _ => Vec::new(),
        }
    }

    /// The scheduling day ended (`SchedTiming.next_day_at` passed): Anki
    /// rebuilds the queue lazily, so fetch it again.
    pub fn rollover(&mut self, b: &dyn Backend, now: Now) -> Vec<Effect> {
        self.next_day_at = backend::sched_timing(b)
            .ok()
            .map(|t| t.next_day_at)
            .filter(|t| now.wall_ms / 1000 < *t);
        let mut fx = Vec::new();
        if self.phase != Phase::Transition {
            self.refresh(b, now, true, &mut fx);
        }
        fx
    }

    // ---- card actions ------------------------------------------------------

    fn mutate(
        &mut self,
        b: &dyn Backend,
        command: Command,
        notice: Option<&str>,
        now: Now,
    ) -> Vec<Effect> {
        let mut fx = Vec::new();
        if self.holding() || self.phase == Phase::Transition || self.card.is_none() {
            return fx;
        }
        match backend::changes(b, command) {
            Ok(_) => {
                fx.extend(notice.map(|n| Effect::Notice(n.into())));
                self.refresh(b, now, true, &mut fx);
            }
            Err(e) => fx.push(Effect::Error(e.to_string())),
        }
        fx
    }

    fn ids(&self) -> Option<(i64, i64)> {
        self.card.as_ref().map(|c| (c.queued.id, c.queued.note_id))
    }

    /// Ctrl+1..7: the same flag again clears it.
    pub fn toggle_flag(&mut self, b: &dyn Backend, flag: u8, now: Now) -> Vec<Effect> {
        let Some(card) = self.card.as_ref() else {
            return Vec::new();
        };
        let flag = if card.queued.flag == flag { 0 } else { flag };
        let card_ids = vec![card.queued.id];
        self.mutate(b, Command::SetFlag { card_ids, flag }, None, now)
    }
    /// `*`.
    pub fn toggle_mark(&mut self, b: &dyn Backend, now: Now) -> Vec<Effect> {
        let Some(card) = self.card.as_ref() else {
            return Vec::new();
        };
        let command = Command::SetMarked {
            note_ids: vec![card.queued.note_id],
            marked: !card.queued.marked,
        };
        self.mutate(b, command, None, now)
    }
    fn bury_or_suspend(
        &mut self,
        b: &dyn Backend,
        note: bool,
        mode: tanki_core::BuryMode,
        notice: &str,
        now: Now,
    ) -> Vec<Effect> {
        let Some((card_id, note_id)) = self.ids() else {
            return Vec::new();
        };
        let target = if note {
            Target::Notes(vec![note_id])
        } else {
            Target::Cards(vec![card_id])
        };
        self.mutate(
            b,
            Command::BuryOrSuspend { target, mode },
            Some(notice),
            now,
        )
    }
    /// `-`.
    pub fn bury_card(&mut self, b: &dyn Backend, now: Now) -> Vec<Effect> {
        let mode = tanki_core::BuryMode::Bury;
        self.bury_or_suspend(b, false, mode, "Card buried.", now)
    }
    /// `=`.
    pub fn bury_note(&mut self, b: &dyn Backend, now: Now) -> Vec<Effect> {
        let mode = tanki_core::BuryMode::Bury;
        self.bury_or_suspend(b, true, mode, "Note buried.", now)
    }
    /// `@`.
    pub fn suspend_card(&mut self, b: &dyn Backend, now: Now) -> Vec<Effect> {
        let mode = tanki_core::BuryMode::Suspend;
        self.bury_or_suspend(b, false, mode, "Card suspended.", now)
    }
    /// `!`.
    pub fn suspend_note(&mut self, b: &dyn Backend, now: Now) -> Vec<Effect> {
        let mode = tanki_core::BuryMode::Suspend;
        self.bury_or_suspend(b, true, mode, "Note suspended.", now)
    }
    /// Ctrl+Alt+N, after the forget dialog.
    pub fn forget(
        &mut self,
        b: &dyn Backend,
        restore_position: bool,
        reset_counts: bool,
        now: Now,
    ) -> Vec<Effect> {
        let Some((card_id, _)) = self.ids() else {
            return Vec::new();
        };
        let command = Command::Forget {
            card_ids: vec![card_id],
            restore_position,
            reset_counts,
        };
        self.mutate(b, command, Some("Card reset to new."), now)
    }
    /// Ctrl+Shift+D, after the prompt.
    pub fn set_due_date(&mut self, b: &dyn Backend, days: String, now: Now) -> Vec<Effect> {
        let Some((card_id, _)) = self.ids() else {
            return Vec::new();
        };
        let command = Command::SetDueDate {
            card_ids: vec![card_id],
            days,
        };
        self.mutate(b, command, Some("Due date set."), now)
    }
    /// Ctrl+Delete, after confirmation.
    pub fn delete_note(&mut self, b: &dyn Backend, now: Now) -> Vec<Effect> {
        let Some((_, note_id)) = self.ids() else {
            return Vec::new();
        };
        let command = Command::DeleteNotes {
            note_ids: vec![note_id],
        };
        self.mutate(b, command, Some("Note deleted."), now)
    }
}

/// Desktop's "{action} undone" / "{action} redone".
pub fn undo_notice(label: &str, redo: bool) -> String {
    let label = if label.is_empty() { "Change" } else { label };
    format!("{label} {}", if redo { "redone" } else { "undone" })
}

/// `scheduling-next-learn-due` + `scheduling-learn-remaining`
/// (`ts/routes/congrats/lib.ts` `buildNextLearnMsg`).
pub fn next_learn_message(secs_until: u32, learn_remaining: u32) -> String {
    if secs_until >= 86_400 {
        return String::new();
    }
    let secs = f64::from(secs_until);
    let (amount, unit) = if secs < 60.0 {
        (secs.round(), "second")
    } else if secs < 3600.0 {
        ((secs / 60.0).round(), "minute")
    } else {
        ((secs / 3600.0).round(), "hour")
    };
    let amount = amount as u64;
    let plural = if amount == 1 { "" } else { "s" };
    let remaining = if learn_remaining == 1 {
        "There is one remaining learning card due later today.".to_owned()
    } else {
        format!("There are {learn_remaining} learning cards due later today.")
    };
    format!("The next learning card will be ready in {amount} {unit}{plural}. {remaining}")
}

/// `studying-card-studied-in-minute`.
pub fn timebox_message(cards: u32, minutes: u32) -> String {
    format!(
        "{cards} {} studied in {minutes} {}.",
        if cards == 1 { "card" } else { "cards" },
        if minutes == 1 { "minute" } else { "minutes" }
    )
}

pub fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests;
