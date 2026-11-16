//! Study, deck and reviewer commands (PLAN 8.1, 8.2).
//!
//! Thin wrappers over Anki's service traits that reproduce what desktop's
//! `reviewer.py`, `overview.py`, `deckbrowser.py` and `flags.py` do around
//! them. Everything here runs on the actor thread.
use crate::actor::changes;
use crate::*;
use anki::{
    collection::Collection,
    services::{
        CardRenderingService, CardsService, CollectionService, ConfigService, DeckConfigService,
        DecksService, NotesService, NotetypesService, SchedulerService, TagsService,
    },
};
use anki_proto as pb;
use pb::config::config_key::{Bool as BoolKey, String as StringKey};
use std::collections::HashMap;

/// Scheduling states of the cards the last `Queue` returned, by card id.
pub(crate) type StateCache = HashMap<i64, pb::scheduler::SchedulingStates>;

/// `anki.tags.MARKED_TAG`; matched case-insensitively (`TagManager.in_list`).
pub(crate) const MARKED_TAG: &str = "marked";

/// Translated text without the U+2068/U+2069 marks Fluent puts around
/// arguments (PLAN 4.3).
pub(crate) fn plain(text: impl AsRef<str>) -> String {
    anki_i18n::without_unicode_isolation(text.as_ref())
}

fn op_changes(c: Option<pb::collection::OpChanges>) -> OpChanges {
    changes(c.unwrap_or_default())
}

fn has_tag(tags: &[String], tag: &str) -> bool {
    // Python's `str.lower()` on both sides.
    let tag = tag.to_lowercase();
    tags.iter().any(|t| t.to_lowercase() == tag)
}

fn card(col: &mut Collection, cid: i64) -> Result<pb::cards::Card> {
    Ok(CardsService::get_card(col, pb::cards::CardId { cid })?)
}

// ---------------------------------------------------------------------------
// Scheduler version

fn config_json(col: &mut Collection, key: &str) -> Option<serde_json::Value> {
    // A missing key is a NotFound error; treat it as absent.
    ConfigService::get_config_json(col, key.to_owned().into())
        .ok()
        .and_then(|json| serde_json::from_slice(&json.json).ok())
}

fn config_bool(col: &mut Collection, key: BoolKey) -> Result<bool> {
    Ok(
        ConfigService::get_config_bool(col, pb::config::GetConfigBoolRequest { key: key as i32 })?
            .val,
    )
}

fn config_string(col: &mut Collection, key: StringKey) -> Result<String> {
    Ok(ConfigService::get_config_string(
        col,
        pb::config::GetConfigStringRequest { key: key as i32 },
    )?
    .val)
}

/// `Collection.sched_ver()` and `v3_scheduler()`: `schedVer` defaults to 1
/// when missing (rslib `scheduler_version`), and v3 is version 2 plus the
/// `sched2021` flag.
pub(crate) fn scheduler_status(col: &mut Collection) -> Result<SchedulerStatus> {
    let version = config_json(col, "schedVer")
        .and_then(|v| v.as_u64())
        .unwrap_or(1) as u32;
    let v3 = version == 2 && config_bool(col, BoolKey::Sched2021)?;
    Ok(SchedulerStatus { v3, version })
}

/// The reviewer refuses to start without v3 (`Reviewer.show`): Anki's queue
/// code does not check the flag and would study a v1 collection with v3
/// rules.
fn require_v3(col: &mut Collection) -> Result<()> {
    if scheduler_status(col)?.v3 {
        Ok(())
    } else {
        Err(CoreError::Invalid(
            plain(col.tr().scheduling_update_required()).replace("V2", "v3"),
        ))
    }
}

/// What desktop's "Update" button does (`DeckBrowser._confirm_upgrade`): a
/// v1 collection is converted by `UpgradeScheduler` (cards, revlog, new
/// timezone; marks the schema modified, so the next sync is a full sync),
/// then `set_v3_scheduler(True)` sets `sched2021`, and loading the v3
/// scheduler enables the new timezone when `creationOffset` is missing
/// (`Collection._load_scheduler`). Already on v3: nothing changes.
pub(crate) fn upgrade_scheduler(col: &mut Collection) -> Result<OpChanges> {
    let status = scheduler_status(col)?;
    let mut out = OpChanges::default();
    if status.v3 {
        return Ok(out);
    }
    if status.version == 1 {
        SchedulerService::upgrade_scheduler(col)?;
        // The RPC returns no OpChanges; it rewrites cards, revlog and config.
        out.card = true;
        out.config = true;
        out.study_queues = true;
        out.browser_table = true;
    } else if status.version != 2 {
        return Err(CoreError::Invalid(format!(
            "Unsupported scheduler version {}",
            status.version
        )));
    }
    let set = ConfigService::set_config_bool(
        col,
        pb::config::SetConfigBoolRequest {
            key: BoolKey::Sched2021 as i32,
            value: true,
            undoable: false,
        },
    )?;
    merge(&mut out, changes(set));
    if config_json(col, "creationOffset").is_none_or(|v| v.is_null()) {
        let mut prefs = ConfigService::get_preferences(col)?;
        prefs
            .scheduling
            .get_or_insert_with(Default::default)
            .new_timezone = true;
        merge(
            &mut out,
            changes(ConfigService::set_preferences(col, prefs)?),
        );
    }
    Ok(out)
}

fn merge(into: &mut OpChanges, from: OpChanges) {
    let OpChanges {
        card,
        note,
        deck,
        tag,
        notetype,
        config,
        deck_config,
        mtime,
        browser_table,
        browser_sidebar,
        note_text,
        study_queues,
    } = from;
    into.card |= card;
    into.note |= note;
    into.deck |= deck;
    into.tag |= tag;
    into.notetype |= notetype;
    into.config |= config;
    into.deck_config |= deck_config;
    into.mtime |= mtime;
    into.browser_table |= browser_table;
    into.browser_sidebar |= browser_sidebar;
    into.note_text |= note_text;
    into.study_queues |= study_queues;
}

// ---------------------------------------------------------------------------
// Queue and answering

/// `GetQueuedCards`, then per card what `reviewer.py` does with it:
/// `V3CardInfo.from_queue` copies `card.custom_data` into the current state
/// (the backend leaves it empty, and custom scheduling and `AnswerCard` rely
/// on it), `DescribeNextStates` gives the button labels, and the note's
/// `marked` tag decides the mark icon.
pub(crate) fn queue(col: &mut Collection, limit: u32, cache: &mut StateCache) -> Result<Queue> {
    require_v3(col)?;
    let q = SchedulerService::get_queued_cards(
        col,
        pb::scheduler::GetQueuedCardsRequest {
            fetch_limit: limit,
            intraday_learning_only: false,
        },
    )?;
    cache.clear();
    let mut cards = Vec::new();
    let script = config_string(col, StringKey::CardStateCustomizer)?;
    let mut custom_scheduling_error = None;
    let mut field_names: HashMap<i64, Vec<String>> = HashMap::new();
    for c in q.cards {
        let card = c
            .card
            .ok_or_else(|| CoreError::Other("Missing queued card".into()))?;
        let mut states = c
            .states
            .ok_or_else(|| CoreError::Other("Missing scheduling states".into()))?;
        if let Some(current) = states.current.as_mut() {
            current.custom_data = Some(card.custom_data.clone());
        }
        if !script.trim().is_empty() {
            match crate::custom_scheduling::apply(&states, &c.context.unwrap_or_default(), &script)
            {
                Ok(updated) => states = updated,
                Err(error) => {
                    custom_scheduling_error.get_or_insert(error);
                }
            }
        }
        let intervals = SchedulerService::describe_next_states(col, states.clone())?
            .vals
            .into_iter()
            .map(plain)
            .collect();
        let note = NotesService::get_note(col, pb::notes::NoteId { nid: card.note_id })?;
        cache.insert(card.id, states);
        let queue = match pb::scheduler::queued_cards::Queue::try_from(c.queue) {
            Ok(pb::scheduler::queued_cards::Queue::Learning) => QueueKind::Learning,
            Ok(pb::scheduler::queued_cards::Queue::Review) => QueueKind::Review,
            _ => QueueKind::New,
        };
        if let std::collections::hash_map::Entry::Vacant(slot) = field_names.entry(note.notetype_id)
        {
            let nt = NotetypesService::get_notetype(
                col,
                pb::notetypes::NotetypeId {
                    ntid: note.notetype_id,
                },
            )?;
            slot.insert(nt.fields.iter().map(|f| f.name.clone()).collect());
        }
        let fields: std::collections::BTreeMap<String, String> = field_names[&note.notetype_id]
            .iter()
            .cloned()
            .zip(note.fields.iter().cloned())
            .collect();
        cards.push(QueuedCard {
            fields,
            id: card.id,
            intervals,
            note_id: card.note_id,
            deck_id: card.deck_id,
            queue,
            flag: (card.flags & 0b111) as u8,
            marked: has_tag(&note.tags, MARKED_TAG),
        });
    }
    Ok(Queue {
        custom_scheduling_error,
        new: q.new_count,
        learning: q.learning_count,
        review: q.review_count,
        cards,
    })
}

/// `Scheduler.build_answer` + `answer_card`, then the leech check of
/// `Reviewer._answerCard`'s `after_answer`: `StateIsLeech(new_state)`, and
/// the card's queue after answering tells whether the leech was suspended.
pub(crate) fn answer(
    col: &mut Collection,
    cache: &StateCache,
    card_id: i64,
    rating: Rating,
    answered_at_millis: i64,
    milliseconds_taken: u32,
) -> Result<AnswerOutcome> {
    let states = cache
        .get(&card_id)
        .cloned()
        .ok_or_else(|| CoreError::Invalid("Fetch a fresh queue before answering.".into()))?;
    use pb::scheduler::card_answer::Rating as R;
    let (new_state, rating) = match rating {
        Rating::Again => (states.again, R::Again),
        Rating::Hard => (states.hard, R::Hard),
        Rating::Good => (states.good, R::Good),
        Rating::Easy => (states.easy, R::Easy),
    };
    let changes = changes(
        SchedulerService::answer_card(
            col,
            pb::scheduler::CardAnswer {
                card_id,
                current_state: states.current,
                new_state: new_state.clone(),
                rating: rating as i32,
                answered_at_millis,
                milliseconds_taken,
            },
        )
        .map_err(|error| match &error {
            // AnkiError's Display hides InvalidInput's explanation. Preserve the
            // validation message from custom data when presenting answer failures.
            anki::error::AnkiError::InvalidInput { .. } => {
                CoreError::Invalid(plain(error.message(col.tr())))
            }
            _ => CoreError::from(error),
        })?,
    );
    let leech = match new_state {
        Some(state) => SchedulerService::state_is_leech(col, state)?.val,
        None => false,
    };
    // `card.queue < 0` in `after_answer`; -1 is QUEUE_TYPE_SUSPENDED.
    let suspended = card(col, card_id)?.queue == -1;
    Ok(AnswerOutcome {
        changes,
        leech,
        suspended,
    })
}

pub(crate) fn undone(undone: pb::collection::OpChangesAfterUndo) -> Response {
    Response::Undone {
        changes: op_changes(undone.changes),
        label: undone.operation,
    }
}

pub(crate) fn undo_status(col: &mut Collection) -> Result<UndoStatus> {
    let status = CollectionService::get_undo_status(col)?;
    let label = |s: String| (!s.is_empty()).then_some(s);
    Ok(UndoStatus {
        undo: label(status.undo),
        redo: label(status.redo),
    })
}

// ---------------------------------------------------------------------------
// Type in the answer

/// Resolves the first `[[type:…]]` of a rendered question exactly as
/// `reviewer.py` `typeAnsQuestionFilter` does, including its quirks: the
/// `cloze:` and `nc:` prefixes keep only the next `:`-separated part (so
/// `cloze:nc:Field` looks up a field named `nc`), fields are matched by name
/// in the note type, and a cloze with nothing to type counts as missing.
pub(crate) fn type_answer(
    col: &mut Collection,
    card: &pb::cards::Card,
    question: &str,
) -> Result<Option<TypeAnswer>> {
    let Some(inner) = first_type_placeholder(question) else {
        return Ok(None);
    };
    let mut field = inner.as_str();
    let mut cloze_ord = None;
    if field.starts_with("cloze:") {
        cloze_ord = Some(card.template_idx + 1);
        field = field.split(':').nth(1).unwrap_or_default();
    }
    let mut combining = true;
    if field.starts_with("nc:") {
        combining = false;
        field = field.split(':').nth(1).unwrap_or_default();
    }
    let note = NotesService::get_note(col, pb::notes::NoteId { nid: card.note_id })?;
    let notetype = NotetypesService::get_notetype(
        col,
        pb::notetypes::NotetypeId {
            ntid: note.notetype_id,
        },
    )?;
    let value = notetype
        .fields
        .iter()
        .position(|f| f.name == field)
        .map(|i| note.fields.get(i).cloned().unwrap_or_default());
    let expected = match (value, cloze_ord) {
        (Some(text), Some(ordinal)) => {
            let text = CardRenderingService::extract_cloze_for_typing(
                col,
                pb::card_rendering::ExtractClozeForTypingRequest { text, ordinal },
            )?
            .val;
            // `_contentForCloze`: `... or None`.
            (!text.is_empty()).then_some(text)
        }
        (value, _) => value,
    };
    Ok(Some(match expected {
        None if cloze_ord.is_some() => {
            TypeAnswer::Message(plain(col.tr().studying_please_run_toolsempty_cards()))
        }
        None => TypeAnswer::Message(plain(col.tr().studying_type_answer_unknown_field(field))),
        Some(text) if text.is_empty() => TypeAnswer::Empty,
        Some(expected) => TypeAnswer::Input {
            expected,
            combining,
        },
    }))
}

pub(crate) fn compare_answer(
    col: &mut Collection,
    expected: String,
    provided: String,
    combining: bool,
) -> Result<String> {
    Ok(CardRenderingService::compare_answer(
        col,
        pb::card_rendering::CompareAnswerRequest {
            expected,
            provided,
            combining,
        },
    )?
    .val)
}

// ---------------------------------------------------------------------------
// Card actions

pub(crate) fn set_flag(col: &mut Collection, card_ids: Vec<i64>, flag: u8) -> Result<OpChanges> {
    if flag > 7 {
        return Err(CoreError::Invalid(format!("No flag {flag}; flags are 0-7")));
    }
    Ok(op_changes(
        CardsService::set_flag(
            col,
            pb::cards::SetFlagRequest {
                card_ids,
                flag: flag.into(),
            },
        )?
        .changes,
    ))
}

/// `Reviewer.toggle_mark_on_current_note`: add or remove the `marked` tag.
pub(crate) fn set_marked(
    col: &mut Collection,
    note_ids: Vec<i64>,
    marked: bool,
) -> Result<OpChanges> {
    let request = pb::tags::NoteIdsAndTagsRequest {
        note_ids,
        tags: MARKED_TAG.into(),
    };
    let out = if marked {
        TagsService::add_note_tags(col, request)?
    } else {
        TagsService::remove_note_tags(col, request)?
    };
    Ok(op_changes(out.changes))
}

pub(crate) fn bury_or_suspend(
    col: &mut Collection,
    target: Target,
    mode: BuryMode,
) -> Result<OpChanges> {
    use pb::scheduler::bury_or_suspend_cards_request::Mode;
    let (card_ids, note_ids) = match target {
        Target::Cards(ids) => (ids, vec![]),
        Target::Notes(ids) => (vec![], ids),
    };
    let mode = match mode {
        BuryMode::Bury => Mode::BuryUser,
        BuryMode::Suspend => Mode::Suspend,
    };
    Ok(op_changes(
        SchedulerService::bury_or_suspend_cards(
            col,
            pb::scheduler::BuryOrSuspendCardsRequest {
                card_ids,
                note_ids,
                mode: mode as i32,
            },
        )?
        .changes,
    ))
}

pub(crate) fn unbury(col: &mut Collection, deck_id: i64, mode: UnburyMode) -> Result<OpChanges> {
    use pb::scheduler::unbury_deck_request::Mode;
    let mode = match mode {
        UnburyMode::UserOnly => Mode::UserOnly,
        UnburyMode::SchedOnly => Mode::SchedOnly,
        UnburyMode::All => Mode::All,
    };
    Ok(changes(SchedulerService::unbury_deck(
        col,
        pb::scheduler::UnburyDeckRequest {
            deck_id,
            mode: mode as i32,
        },
    )?))
}

const REVIEWER: pb::scheduler::schedule_cards_as_new_request::Context =
    pb::scheduler::schedule_cards_as_new_request::Context::Reviewer;

/// The forget dialog's checkboxes as last used from the reviewer.
pub(crate) fn forget_defaults(col: &mut Collection) -> Result<ForgetDefaults> {
    let d = SchedulerService::schedule_cards_as_new_defaults(
        col,
        pb::scheduler::ScheduleCardsAsNewDefaultsRequest {
            context: REVIEWER as i32,
        },
    )?;
    Ok(ForgetDefaults {
        restore_position: d.restore_position,
        reset_counts: d.reset_counts,
    })
}

/// `forget_cards(context=REVIEWER)` → `Scheduler.schedule_cards_as_new`
/// (`log=True`); the context makes Anki remember the checkboxes.
pub(crate) fn forget(
    col: &mut Collection,
    card_ids: Vec<i64>,
    restore_position: bool,
    reset_counts: bool,
) -> Result<OpChanges> {
    Ok(changes(SchedulerService::schedule_cards_as_new(
        col,
        pb::scheduler::ScheduleCardsAsNewRequest {
            card_ids,
            log: true,
            restore_position,
            reset_counts,
            context: Some(REVIEWER as i32),
        },
    )?))
}

/// `Reviewer.on_set_due`: `SET_DUE_REVIEWER` makes Anki remember `days`.
pub(crate) fn set_due_date(
    col: &mut Collection,
    card_ids: Vec<i64>,
    days: String,
) -> Result<OpChanges> {
    Ok(changes(SchedulerService::set_due_date(
        col,
        pb::scheduler::SetDueDateRequest {
            card_ids,
            days,
            config_key: Some(pb::config::OptionalStringConfigKey {
                key: StringKey::SetDueReviewer as i32,
            }),
        },
    )?))
}

pub(crate) fn delete_notes(col: &mut Collection, note_ids: Vec<i64>) -> Result<OpChanges> {
    Ok(op_changes(
        NotesService::remove_notes(
            col,
            pb::notes::RemoveNotesRequest {
                note_ids,
                card_ids: vec![],
            },
        )?
        .changes,
    ))
}

// ---------------------------------------------------------------------------
// Decks, overview, congrats

pub(crate) fn deck_tree(col: &mut Collection) -> Result<pb::decks::DeckTreeNode> {
    Ok(DecksService::deck_tree(
        col,
        pb::decks::DeckTreeRequest {
            now: anki::prelude::TimestampSecs::now().0,
        },
    )?)
}

fn find_node(node: &pb::decks::DeckTreeNode, id: i64) -> Option<&pb::decks::DeckTreeNode> {
    if node.deck_id == id {
        return Some(node);
    }
    node.children.iter().find_map(|child| find_node(child, id))
}

/// Makes `deck_id` the current deck (`DeckManager.select`) and returns its
/// overview. The cached scheduling states belong to the old deck's queue,
/// so they are dropped.
pub(crate) fn select_deck(
    col: &mut Collection,
    deck_id: i64,
    cache: &mut StateCache,
) -> Result<DeckOverview> {
    // `set_current_deck` accepts ids that do not exist; desktop never sends one.
    let _ = DecksService::get_deck(col, pb::decks::DeckId { did: deck_id })?;
    let _ = DecksService::set_current_deck(col, pb::decks::DeckId { did: deck_id })?;
    cache.clear();
    overview(col)
}

/// The overview screen (`Overview._renderPage`, `_desc`, `_table`).
pub(crate) fn overview(col: &mut Collection) -> Result<DeckOverview> {
    let deck = DecksService::get_current_deck(col)?;
    let q = SchedulerService::get_queued_cards(
        col,
        pb::scheduler::GetQueuedCardsRequest {
            fetch_limit: 0,
            intraday_learning_only: false,
        },
    )?;
    let tree = deck_tree(col)?;
    let delta = |tree: u32, queue: u32| tree as i32 - queue as i32;
    let (new_delta, learning_delta, review_delta) = find_node(&tree, deck.id)
        .map(|n| {
            (
                delta(n.new_count, q.new_count),
                delta(n.learn_count, q.learning_count),
                delta(n.review_count, q.review_count),
            )
        })
        .unwrap_or_default();
    let filtered = matches!(deck.kind, Some(pb::decks::deck::Kind::Filtered(_)));
    let description = match &deck.kind {
        Some(pb::decks::deck::Kind::Normal(normal)) if normal.markdown_description => {
            CardRenderingService::render_markdown(
                col,
                pb::card_rendering::RenderMarkdownRequest {
                    markdown: normal.description.clone(),
                    sanitize: true,
                },
            )?
            .val
        }
        Some(pb::decks::deck::Kind::Normal(normal)) => normal.description.clone(),
        _ => {
            let tr = col.tr();
            plain(format!(
                "{} {} {}",
                tr.studying_this_is_a_special_deck_for(),
                tr.studying_cards_will_be_automatically_returned_to(),
                tr.studying_deleting_this_deck_from_the_deck()
            ))
        }
    };
    let info = SchedulerService::congrats_info(col)?;
    Ok(DeckOverview {
        deck_id: deck.id,
        name: deck.name,
        filtered,
        new: q.new_count,
        learning: q.learning_count,
        review: q.review_count,
        new_delta,
        learning_delta,
        review_delta,
        description,
        have_user_buried: info.have_user_buried,
        have_sched_buried: info.have_sched_buried,
    })
}

pub(crate) fn set_deck_collapsed(
    col: &mut Collection,
    deck_id: i64,
    collapsed: bool,
) -> Result<OpChanges> {
    Ok(changes(DecksService::set_deck_collapsed(
        col,
        pb::decks::SetDeckCollapsedRequest {
            deck_id,
            collapsed,
            scope: pb::decks::set_deck_collapsed_request::Scope::Reviewer as i32,
        },
    )?))
}

pub(crate) fn congrats(col: &mut Collection) -> Result<Congrats> {
    let c = SchedulerService::congrats_info(col)?;
    Ok(Congrats {
        learn_remaining: c.learn_remaining,
        secs_until_next_learn: c.secs_until_next_learn,
        review_remaining: c.review_remaining,
        new_remaining: c.new_remaining,
        have_sched_buried: c.have_sched_buried,
        have_user_buried: c.have_user_buried,
        is_filtered_deck: c.is_filtered_deck,
        deck_description: c.deck_description,
    })
}

pub(crate) fn studied_today(col: &mut Collection) -> Result<String> {
    Ok(plain(SchedulerService::studied_today(col)?.val))
}

pub(crate) fn sched_timing(col: &mut Collection) -> Result<SchedTiming> {
    let t = SchedulerService::sched_timing_today(col)?;
    Ok(SchedTiming {
        days_elapsed: t.days_elapsed,
        next_day_at: t.next_day_at,
    })
}

// ---------------------------------------------------------------------------
// Reviewer settings

/// The card's preset as pylib reads it (`Card.current_deck_id()` = original
/// deck if the card is in a filtered deck, then
/// `DeckManager.config_dict_for_deck_id`, which falls back to the default
/// preset when the deck's preset is missing), plus the collection-wide
/// reviewer preferences.
pub(crate) fn study_config(col: &mut Collection, card_id: i64) -> Result<StudyConfig> {
    use pb::deck_config::deck_config::config::{AnswerAction as A, QuestionAction as Q};
    let card = card(col, card_id)?;
    let home = if card.original_deck_id != 0 {
        card.original_deck_id
    } else {
        card.deck_id
    };
    let deck = DecksService::get_deck(col, pb::decks::DeckId { did: home })?;
    let dcid = match deck.kind {
        Some(pb::decks::deck::Kind::Normal(normal)) => normal.config_id,
        // A filtered home deck cannot happen in a valid collection.
        _ => 1,
    };
    // `get_deck_config` falls back to the default preset itself.
    let preset = DeckConfigService::get_deck_config(col, pb::deck_config::DeckConfigId { dcid })?
        .config
        .unwrap_or_default();
    let reviewing = ConfigService::get_preferences(col)?
        .reviewing
        .unwrap_or_default();
    let custom_scheduling_set = !config_string(col, StringKey::CardStateCustomizer)?.is_empty();
    let set_due_default = config_string(col, StringKey::SetDueReviewer)?;
    Ok(StudyConfig {
        autoplay: !preset.disable_autoplay,
        skip_question_when_replaying_answer: preset.skip_question_when_replaying_answer,
        show_timer: preset.show_timer,
        stop_timer_on_answer: preset.stop_timer_on_answer,
        cap_answer_time_secs: preset.cap_answer_time_to_secs,
        seconds_to_show_question: preset.seconds_to_show_question,
        seconds_to_show_answer: preset.seconds_to_show_answer,
        question_action: match Q::try_from(preset.question_action) {
            Ok(Q::ShowReminder) => QuestionAction::ShowReminder,
            _ => QuestionAction::ShowAnswer,
        },
        answer_action: match A::try_from(preset.answer_action) {
            Ok(A::AnswerAgain) => AnswerAction::AnswerAgain,
            Ok(A::AnswerGood) => AnswerAction::AnswerGood,
            Ok(A::AnswerHard) => AnswerAction::AnswerHard,
            Ok(A::ShowReminder) => AnswerAction::ShowReminder,
            _ => AnswerAction::BuryCard,
        },
        wait_for_audio: preset.wait_for_audio,
        show_intervals: reviewing.show_intervals_on_buttons,
        show_remaining_count: reviewing.show_remaining_due_counts,
        timebox_secs: reviewing.time_limit_secs,
        interrupt_audio: reviewing.interrupt_audio_when_answering,
        hide_audio_play_buttons: reviewing.hide_audio_play_buttons,
        custom_scheduling_set,
        set_due_default,
        flag_names: flag_names(col),
    })
}

/// `FlagManager._load_flags`: a custom label from the `flagLabels` config
/// object (keys "1"-"7") wins, else Anki's localised default.
pub(crate) fn flag_names(col: &mut Collection) -> Vec<String> {
    let labels = config_json(col, "flagLabels").unwrap_or_default();
    let tr = col.tr();
    let defaults = [
        tr.actions_flag_red(),
        tr.actions_flag_orange(),
        tr.actions_flag_green(),
        tr.actions_flag_blue(),
        tr.actions_flag_pink(),
        tr.actions_flag_turquoise(),
        tr.actions_flag_purple(),
    ];
    defaults
        .into_iter()
        .enumerate()
        .map(
            |(i, default)| match labels.get((i + 1).to_string()).and_then(|v| v.as_str()) {
                Some(custom) => custom.to_owned(),
                None => plain(default),
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `V3CardInfo.from_queue`: the queued card's `custom_data` is copied
    /// into the cached current state, and survives answering.
    #[test]
    fn queue_copies_custom_data_into_current_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut col = anki::collection::CollectionBuilder::new(dir.path().join("c.anki2"))
            .build()
            .unwrap();
        let nt = col.get_notetype_by_name("Basic").unwrap().unwrap();
        let mut note =
            NotesService::new_note(&mut col, pb::notetypes::NotetypeId { ntid: nt.id.0 }).unwrap();
        note.fields = vec!["q".into(), "a".into()];
        let _ = NotesService::add_note(
            &mut col,
            pb::notes::AddNoteRequest {
                note: Some(note),
                deck_id: 1,
            },
        )
        .unwrap();
        let mut cache = StateCache::new();
        let id = queue(&mut col, 1, &mut cache).unwrap().cards[0].id;
        assert_eq!(
            cache[&id].current.as_ref().unwrap().custom_data.as_deref(),
            Some("")
        );
        let mut stored = card(&mut col, id).unwrap();
        stored.custom_data = r#"{"x":1}"#.into();
        let _ = CardsService::update_cards(
            &mut col,
            pb::cards::UpdateCardsRequest {
                cards: vec![stored],
                skip_undo_entry: true,
            },
        )
        .unwrap();
        let q = queue(&mut col, 1, &mut cache).unwrap();
        assert_eq!(q.cards[0].id, id);
        assert_eq!(
            cache[&id].current.as_ref().unwrap().custom_data.as_deref(),
            Some(r#"{"x":1}"#)
        );
        let outcome = answer(&mut col, &cache, id, Rating::Good, 1_700_000_000_000, 1000).unwrap();
        assert!(outcome.changes.card && !outcome.leech && !outcome.suspended);
        assert_eq!(card(&mut col, id).unwrap().custom_data, r#"{"x":1}"#);
        col.close(None).unwrap();
    }

    #[test]
    fn tags_match_case_insensitively() {
        assert!(has_tag(&["Marked".into()], MARKED_TAG));
        assert!(has_tag(&["x".into(), "MARKED".into()], MARKED_TAG));
        assert!(!has_tag(&["marked::sub".into()], MARKED_TAG));
        assert!(!has_tag(&[], MARKED_TAG));
    }

    #[test]
    fn merge_ors_every_flag() {
        let mut a = OpChanges {
            card: true,
            ..Default::default()
        };
        merge(
            &mut a,
            OpChanges {
                config: true,
                study_queues: true,
                ..Default::default()
            },
        );
        assert!(a.card && a.config && a.study_queues && !a.note);
    }

    #[test]
    fn find_node_searches_depth_first() {
        let leaf = pb::decks::DeckTreeNode {
            deck_id: 3,
            ..Default::default()
        };
        let root = pb::decks::DeckTreeNode {
            deck_id: 0,
            children: vec![
                pb::decks::DeckTreeNode {
                    deck_id: 1,
                    ..Default::default()
                },
                pb::decks::DeckTreeNode {
                    deck_id: 2,
                    children: vec![leaf],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(find_node(&root, 3).map(|n| n.deck_id), Some(3));
        assert!(find_node(&root, 4).is_none());
    }
}
