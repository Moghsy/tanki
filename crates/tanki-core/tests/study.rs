//! Fixture-based tests for the Phase 1 study, deck and reviewer commands.
//!
//! Most tests run on the study fixture (`Command::GenerateStudyFixture`).
//! Tests that need a note type, config or scheduler version the actor cannot
//! create prepare a fresh collection directly through Anki first, close it,
//! and only then open it with the actor, so only one opener exists at a time.
use anki::{
    collection::{Collection, CollectionBuilder},
    notetype::{CardTemplate, Notetype},
    services::{CardsService, ConfigService, NotesService, StatsService},
};
use anki_proto as pb;
use std::path::Path;
use tanki_core::*;

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn study() -> (tempfile::TempDir, CollectionActor) {
    let dir = tempfile::tempdir().unwrap();
    let actor = CollectionActor::open(dir.path()).unwrap();
    let Response::CardIds(ids) = actor.execute(Command::GenerateStudyFixture).unwrap() else {
        panic!("GenerateStudyFixture did not return card ids")
    };
    assert!(ids.len() >= 30, "{}", ids.len());
    (dir, actor)
}

fn search(actor: &CollectionActor, query: &str) -> Vec<i64> {
    match actor.execute(Command::Search {
        query: query.into(),
    }) {
        Ok(Response::CardIds(ids)) => ids,
        other => panic!("search {query}: {other:?}"),
    }
}

fn one(actor: &CollectionActor, query: &str) -> i64 {
    let ids = search(actor, query);
    assert_eq!(ids.len(), 1, "{query} matches {ids:?}");
    ids[0]
}

fn changes(response: Result<Response>) -> OpChanges {
    match response {
        Ok(Response::Changes(changes)) => changes,
        other => panic!("expected Changes, got {other:?}"),
    }
}

fn queue(actor: &CollectionActor, limit: u32) -> Queue {
    match actor.execute(Command::Queue { limit }) {
        Ok(Response::Queue(q)) => q,
        other => panic!("expected Queue, got {other:?}"),
    }
}

fn overview(response: Result<Response>) -> DeckOverview {
    match response {
        Ok(Response::Overview(o)) => o,
        other => panic!("expected Overview, got {other:?}"),
    }
}

fn tree(actor: &CollectionActor) -> Deck {
    match actor.execute(Command::DeckTree) {
        Ok(Response::DeckTree(tree)) => tree,
        other => panic!("expected DeckTree, got {other:?}"),
    }
}

/// The tree node for a full deck name.
fn node<'a>(root: &'a Deck, name: &str) -> &'a Deck {
    let mut current = root;
    for part in name.split("::") {
        current = current
            .children
            .iter()
            .find(|d| d.name == part)
            .unwrap_or_else(|| panic!("no deck {name}"));
    }
    current
}

fn deck_id(actor: &CollectionActor, name: &str) -> i64 {
    node(&tree(actor), name).id
}

fn select(actor: &CollectionActor, name: &str) -> DeckOverview {
    let deck_id = deck_id(actor, name);
    overview(actor.execute(Command::SelectDeck { deck_id }))
}

fn study_config(actor: &CollectionActor, card_id: i64) -> StudyConfig {
    match actor.execute(Command::StudyConfig { card_id }) {
        Ok(Response::StudyConfig(c)) => c,
        other => panic!("expected StudyConfig, got {other:?}"),
    }
}

fn render(actor: &CollectionActor, card_id: i64) -> RenderedCard {
    match actor.execute(Command::Render { card_id }) {
        Ok(Response::Rendered(c)) => c,
        other => panic!("expected Rendered, got {other:?}"),
    }
}

fn compare(actor: &CollectionActor, expected: &str, provided: &str, combining: bool) -> String {
    match actor.execute(Command::CompareAnswer {
        expected: expected.into(),
        provided: provided.into(),
        combining,
    }) {
        Ok(Response::Html(html)) => html,
        other => panic!("expected Html, got {other:?}"),
    }
}

fn undo_status(actor: &CollectionActor) -> UndoStatus {
    match actor.execute(Command::UndoStatus) {
        Ok(Response::UndoStatus(s)) => s,
        other => panic!("expected UndoStatus, got {other:?}"),
    }
}

fn answer(actor: &CollectionActor, card_id: i64, rating: Rating) -> AnswerOutcome {
    match actor.execute(Command::Answer {
        card_id,
        rating,
        answered_at_millis: now_millis(),
        milliseconds_taken: 4000,
    }) {
        Ok(Response::Answered(outcome)) => outcome,
        other => panic!("expected Answered, got {other:?}"),
    }
}

/// Prepares a fresh collection in `dir` directly through Anki, then closes it.
fn prepare(dir: &Path, f: impl FnOnce(&mut Collection)) {
    let mut col = CollectionBuilder::new(dir.join("collection.anki2"))
        .set_media_paths(
            dir.join("collection.media"),
            dir.join("collection.media.db2"),
        )
        .build()
        .unwrap();
    f(&mut col);
    col.close(None).unwrap();
}

fn set_config(col: &mut Collection, key: &str, value: serde_json::Value) {
    let _ = ConfigService::set_config_json(
        col,
        pb::config::SetConfigJsonRequest {
            key: key.into(),
            value_json: serde_json::to_vec(&value).unwrap(),
            undoable: false,
        },
    )
    .unwrap();
}

// ---------------------------------------------------------------------------

#[test]
fn study_fixture_shape() {
    let (_dir, actor) = study();
    let root = tree(&actor);
    let study = node(&root, "Study");
    assert_eq!(study.level, 1);
    assert!(!study.filtered);
    assert_eq!(node(&root, "Study::Vocabulary::French").level, 3);
    assert!(node(&root, "Study Cram").filtered);
    assert!(
        study.new > 0 && study.learning > 0 && study.review > 0,
        "{study:?}"
    );
    // Current deck is "Study".
    let o = overview(actor.execute(Command::Overview));
    assert_eq!(o.name, "Study");
    assert_eq!(o.deck_id, study.id);
    assert!(o.new > 0 && o.learning > 0 && o.review > 0, "{o:?}");
    // Media came back with Anki's names and exists.
    let media = actor.media_dir();
    for name in ["study-gradient.png", "study-square.svg", "study-tone.wav"] {
        assert!(media.join(name).is_file(), "{name}");
    }
    // States.
    assert_eq!(search(&actor, "is:suspended").len(), 1);
    assert_eq!(
        search(&actor, "is:buried").len(),
        3,
        "manual + two siblings"
    );
    assert_eq!(search(&actor, "deck:\"Study Cram\"").len(), 2);
    assert!(!search(&actor, "deck:Study is:due is:review").is_empty());
    assert_eq!(search(&actor, "is:learn").len(), 5);
    assert_eq!(search(&actor, "flag:2").len(), 1);
}

#[test]
fn queue_reports_flags_marks_and_queue_kind() {
    let (_dir, actor) = study();
    select(&actor, "Study::Vocabulary::French");
    let q = queue(&actor, 100);
    assert_eq!(q.cards.len() as u32, q.new + q.learning + q.review);
    let apple = one(&actor, "\"la pomme\" card:1");
    let book = one(&actor, "\"le livre\" card:1");
    let by_id = |id| q.cards.iter().find(|c| c.id == id);
    // "French" has 6 new cards a day; both are early enough to be queued.
    let apple = by_id(apple).expect("la pomme queued");
    assert!(apple.marked, "tag `Marked` counts as marked");
    assert_eq!(apple.flag, 0);
    assert_eq!(apple.queue, QueueKind::New);
    let book = by_id(book).expect("le livre queued");
    assert_eq!(book.flag, 2);
    assert!(!book.marked);
    for card in &q.cards {
        assert_eq!(card.intervals.len(), 4);
        assert!(card
            .intervals
            .iter()
            .all(|i| !i.contains(['\u{2068}', '\u{2069}'])));
    }
    let relearning = one(&actor, "\"la maison\" card:1");
    assert_eq!(by_id(relearning).unwrap().queue, QueueKind::Learning);
    let review = one(&actor, "\"le chat\" card:1");
    let review = by_id(review)
        .or_else(|| by_id(one(&actor, "\"le chat\" card:2")))
        .unwrap();
    assert_eq!(review.queue, QueueKind::Review);
}

#[test]
fn answer_reports_leech_and_suspension_and_undo_redo() {
    let (_dir, actor) = study();
    select(&actor, "Study::Science");
    assert_eq!(undo_status(&actor).redo, None);
    // Reviews come before new cards; find the first review.
    let q = queue(&actor, 100);
    let review = q
        .cards
        .iter()
        .find(|c| c.queue == QueueKind::Review)
        .expect("a review in Science")
        .id;
    let outcome = answer(&actor, review, Rating::Again);
    assert!(outcome.leech, "threshold 1: first lapse is a leech");
    assert!(outcome.suspended, "leech action is suspend");
    assert!(outcome.changes.card);
    assert!(!search(&actor, "tag:leech").is_empty());
    assert_eq!(undo_status(&actor).undo.as_deref(), Some("Answer Card"));
    // Answers need a fresh queue.
    assert!(matches!(
        actor.execute(Command::Answer {
            card_id: review,
            rating: Rating::Good,
            answered_at_millis: now_millis(),
            milliseconds_taken: 1,
        }),
        Err(CoreError::Invalid(_))
    ));
    let Response::Undone { label, changes } = actor.execute(Command::Undo).unwrap() else {
        panic!("undo")
    };
    assert_eq!(label, "Answer Card");
    assert!(changes.card);
    assert!(search(&actor, "tag:leech").is_empty());
    assert_eq!(
        queue(&actor, 1).cards[0].id,
        review,
        "undone card is back on top"
    );
    let status = undo_status(&actor);
    assert_eq!(status.redo.as_deref(), Some("Answer Card"));
    let Response::Undone { label, .. } = actor.execute(Command::Redo).unwrap() else {
        panic!("redo")
    };
    assert_eq!(label, "Answer Card");
    assert!(!search(&actor, "tag:leech").is_empty());

    // A non-leech answer.
    let q = queue(&actor, 1);
    let outcome = answer(&actor, q.cards[0].id, Rating::Good);
    assert!(!outcome.leech && !outcome.suspended);
}

#[test]
fn answer_preserves_custom_data_caps_time_and_redoes_the_same_timestamp() {
    let (dir, actor) = study();
    select(&actor, "Study::Vocabulary::French");
    let book = one(&actor, "\"le livre\" card:1");
    assert!(queue(&actor, 100).cards.iter().any(|c| c.id == book));
    let answered_at = now_millis();
    let Response::Answered(_) = actor
        .execute(Command::Answer {
            card_id: book,
            rating: Rating::Good,
            answered_at_millis: answered_at,
            milliseconds_taken: 90_001,
        })
        .unwrap()
    else {
        panic!("Answer")
    };
    actor.execute(Command::Undo).unwrap();
    assert!(search(&actor, "\"le livre\" card:1 is:new").contains(&book));
    actor.execute(Command::Redo).unwrap();
    actor.close().unwrap();

    // Inspect only after the actor has joined and closed its collection.
    prepare(dir.path(), |col| {
        let card = CardsService::get_card(col, pb::cards::CardId { cid: book }).unwrap();
        assert_eq!(card.custom_data, r#"{"fixture":"study","counter":7}"#);
        assert_eq!(card.last_review_time_secs, Some(answered_at / 1000));
        assert_eq!(card.reps, 1);
        let log = StatsService::get_review_logs(col, pb::cards::CardId { cid: book }).unwrap();
        assert_eq!(log.entries.len(), 1, "undo/redo keeps one review log");
        let entry = &log.entries[0];
        assert_eq!(entry.time, answered_at / 1000);
        assert_eq!(entry.taken_secs, 60.0, "backend caps uncapped input");
        assert_eq!(entry.button_chosen, 3);
    });
}

#[test]
fn bury_and_suspend_card_and_note_are_undoable() {
    let (_dir, actor) = study();
    select(&actor, "Study::Vocabulary::French");
    let book = one(&actor, "\"le livre\" card:1");
    let note = queue(&actor, 100)
        .cards
        .iter()
        .find(|c| c.id == book)
        .unwrap()
        .note_id;
    for (mode, state) in [
        (BuryMode::Bury, "is:buried"),
        (BuryMode::Suspend, "is:suspended"),
    ] {
        for (target, count) in [
            (Target::Cards(vec![book]), 1),
            (Target::Notes(vec![note]), 2),
        ] {
            let query = format!("\"le livre\" {state}");
            changes(actor.execute(Command::BuryOrSuspend { target, mode }));
            assert_eq!(search(&actor, &query).len(), count);
            actor.execute(Command::Undo).unwrap();
            assert!(search(&actor, &query).is_empty());
            actor.execute(Command::Redo).unwrap();
            assert_eq!(search(&actor, &query).len(), count);
            actor.execute(Command::Undo).unwrap();
        }
    }
}

#[test]
fn card_actions() {
    let (_dir, actor) = study();
    select(&actor, "Study::Vocabulary::French");
    let book = one(&actor, "\"le livre\" card:1");
    let book_note = queue(&actor, 100)
        .cards
        .iter()
        .find(|c| c.id == book)
        .unwrap()
        .note_id;
    let find = |id| queue(&actor, 100).cards.into_iter().find(|c| c.id == id);

    // Flags.
    assert!(
        changes(actor.execute(Command::SetFlag {
            card_ids: vec![book],
            flag: 5
        }))
        .card
    );
    assert_eq!(find(book).unwrap().flag, 5);
    changes(actor.execute(Command::SetFlag {
        card_ids: vec![book],
        flag: 0,
    }));
    assert_eq!(find(book).unwrap().flag, 0);
    assert!(matches!(
        actor.execute(Command::SetFlag {
            card_ids: vec![book],
            flag: 8
        }),
        Err(CoreError::Invalid(_))
    ));

    // Mark.
    assert!(
        changes(actor.execute(Command::SetMarked {
            note_ids: vec![book_note],
            marked: true
        }))
        .note
    );
    assert!(find(book).unwrap().marked);
    changes(actor.execute(Command::SetMarked {
        note_ids: vec![book_note],
        marked: false,
    }));
    assert!(!find(book).unwrap().marked);
    let apple_note = queue(&actor, 100)
        .cards
        .into_iter()
        .find(|c| c.marked)
        .unwrap()
        .note_id;
    changes(actor.execute(Command::SetMarked {
        note_ids: vec![apple_note],
        marked: false,
    }));
    assert!(
        search(&actor, "tag:marked").is_empty(),
        "removing `marked` also removes `Marked`"
    );

    // Bury a card, suspend a note, unbury.
    let before = select(&actor, "Study::Vocabulary::French");
    assert!(before.have_sched_buried);
    changes(actor.execute(Command::BuryOrSuspend {
        target: Target::Cards(vec![book]),
        mode: BuryMode::Bury,
    }));
    assert!(find(book).is_none());
    let o = overview(actor.execute(Command::Overview));
    assert!(o.have_user_buried);
    assert_eq!(
        o.new, before.new,
        "next new card takes the buried one's place"
    );
    let french = deck_id(&actor, "Study::Vocabulary::French");
    changes(actor.execute(Command::Unbury {
        deck_id: french,
        mode: UnburyMode::UserOnly,
    }));
    assert!(find(book).is_some());
    let o = overview(actor.execute(Command::Overview));
    assert!(!o.have_user_buried && o.have_sched_buried);
    changes(actor.execute(Command::Unbury {
        deck_id: french,
        mode: UnburyMode::SchedOnly,
    }));
    assert!(!overview(actor.execute(Command::Overview)).have_sched_buried);
    changes(actor.execute(Command::BuryOrSuspend {
        target: Target::Notes(vec![book_note]),
        mode: BuryMode::Suspend,
    }));
    assert_eq!(search(&actor, "\"le livre\" is:suspended").len(), 2);
    let the_dog = one(&actor, "\"le chien\" card:2");
    changes(actor.execute(Command::BuryOrSuspend {
        target: Target::Cards(vec![the_dog]),
        mode: BuryMode::Bury,
    }));
    assert_eq!(
        search(&actor, "deck:Study::Vocabulary::French is:buried-manually").len(),
        1
    );
    changes(actor.execute(Command::Unbury {
        deck_id: french,
        mode: UnburyMode::All,
    }));
    assert!(search(&actor, "deck:Study::Vocabulary::French is:buried").is_empty());

    // Forget, remembering the reviewer defaults.
    assert_eq!(
        match actor.execute(Command::ForgetDefaults).unwrap() {
            Response::ForgetDefaults(d) => d,
            other => panic!("{other:?}"),
        },
        // Anki's defaults: restore the original position, keep the counts.
        ForgetDefaults {
            restore_position: true,
            reset_counts: false
        }
    );
    let cat = one(&actor, "\"le chat\" card:1");
    changes(actor.execute(Command::Forget {
        card_ids: vec![cat],
        restore_position: false,
        reset_counts: true,
    }));
    assert_eq!(search(&actor, "\"le chat\" card:1 is:new").len(), 1);
    assert_eq!(
        match actor.execute(Command::ForgetDefaults).unwrap() {
            Response::ForgetDefaults(d) => d,
            other => panic!("{other:?}"),
        },
        ForgetDefaults {
            restore_position: false,
            reset_counts: true
        }
    );

    // Set due date, remembered as the reviewer default.
    assert_eq!(study_config(&actor, cat).set_due_default, "1");
    changes(actor.execute(Command::SetDueDate {
        card_ids: vec![cat],
        days: "3-5".into(),
    }));
    assert_eq!(
        search(&actor, "\"le chat\" card:1 is:review -is:due").len(),
        1
    );
    assert_eq!(study_config(&actor, cat).set_due_default, "3-5");
    assert!(matches!(
        actor.execute(Command::SetDueDate {
            card_ids: vec![cat],
            days: "soon".into(),
        }),
        Err(CoreError::Other(_) | CoreError::Invalid(_))
    ));

    // Delete a note.
    assert!(
        changes(actor.execute(Command::DeleteNotes {
            note_ids: vec![book_note]
        }))
        .note
    );
    assert!(search(&actor, "\"le livre\"").is_empty());
    let Response::Undone { label, .. } = actor.execute(Command::Undo).unwrap() else {
        panic!("undo")
    };
    assert_eq!(label, "Delete Note");
    assert_eq!(search(&actor, "\"le livre\"").len(), 2);
}

#[test]
fn decks_overview_and_congrats() {
    let (_dir, actor) = study();
    // Markdown description, rendered and sanitised.
    let o = overview(actor.execute(Command::Overview));
    assert!(
        o.description.contains("<strong>tanki</strong>"),
        "{}",
        o.description
    );
    assert!(o.description.contains("<li>"), "{}", o.description);
    assert!(!o.filtered);
    // Deck-tree minus queue: the tree does not apply burying of siblings
    // at gather time, so the deltas are never negative here.
    assert!(o.new_delta >= 0 && o.learning_delta >= 0 && o.review_delta >= 0);
    assert!(o.have_user_buried && o.have_sched_buried);

    // HTML description is passed through.
    let science = select(&actor, "Study::Science");
    assert_eq!(
        science.description,
        "<i>Leeches</i> are suspended after one lapse."
    );
    // Filtered deck: Anki's fixed text, without isolation marks.
    let cram = select(&actor, "Study Cram");
    assert!(cram.filtered);
    assert_eq!(cram.new, 2);
    assert!(
        cram.description
            .starts_with("This is a special deck for studying outside"),
        "{}",
        cram.description
    );
    assert!(!cram.description.contains(['\u{2068}', '\u{2069}']));
    let Response::Congrats(c) = actor.execute(Command::Congrats).unwrap() else {
        panic!("congrats")
    };
    assert!(c.is_filtered_deck);

    // A deck with nothing due today.
    let later = select(&actor, "Study::Later");
    assert_eq!((later.new, later.learning, later.review), (0, 0, 0));
    assert!(queue(&actor, 1).cards.is_empty());
    let Response::Congrats(c) = actor.execute(Command::Congrats).unwrap() else {
        panic!("congrats")
    };
    assert_eq!(c.secs_until_next_learn, 86_400);
    assert_eq!(c.learn_remaining, 0);
    assert!(!c.is_filtered_deck && !c.review_remaining && !c.new_remaining);

    // Congrats data on a deck that still has cards: buried flags and the
    // rendered Markdown description.
    select(&actor, "Study");
    let Response::Congrats(c) = actor.execute(Command::Congrats).unwrap() else {
        panic!("congrats")
    };
    assert!(c.have_user_buried && c.have_sched_buried);
    assert!(c.deck_description.contains("<strong>tanki</strong>"));

    // Collapse.
    let study = deck_id(&actor, "Study");
    changes(actor.execute(Command::SetDeckCollapsed {
        deck_id: study,
        collapsed: true,
    }));
    assert!(node(&tree(&actor), "Study").collapsed);
    changes(actor.execute(Command::SetDeckCollapsed {
        deck_id: study,
        collapsed: false,
    }));
    assert!(!node(&tree(&actor), "Study").collapsed);

    // Unknown deck.
    assert!(actor
        .execute(Command::SelectDeck { deck_id: 424242 })
        .is_err());
}

#[test]
fn studied_today_and_timing() {
    let (_dir, actor) = study();
    let Response::Text(text) = actor.execute(Command::StudiedToday).unwrap() else {
        panic!("studied today")
    };
    // The fixture graded five cards today.
    assert!(text.starts_with("Studied 5 cards\nin "), "{text}");
    assert!(!text.contains(['\u{2068}', '\u{2069}']));
    let Response::SchedTiming(t) = actor.execute(Command::SchedTiming).unwrap() else {
        panic!("timing")
    };
    let now = now_millis() / 1000;
    assert!(
        t.next_day_at > now && t.next_day_at <= now + 86_400,
        "{t:?}"
    );
}

#[test]
fn study_config_uses_home_deck_preset() {
    let (_dir, actor) = study();
    let media = one(&actor, "front:Listen*");
    let c = study_config(&actor, media);
    assert_eq!(
        c,
        StudyConfig {
            autoplay: true,
            skip_question_when_replaying_answer: true,
            show_timer: true,
            stop_timer_on_answer: true,
            cap_answer_time_secs: 30,
            seconds_to_show_question: 20.0,
            seconds_to_show_answer: 10.0,
            question_action: QuestionAction::ShowReminder,
            answer_action: AnswerAction::AnswerHard,
            wait_for_audio: false,
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
                "Purple"
            ]
            .map(String::from)
            .to_vec(),
        }
    );
    // Default preset.
    let plain = study_config(&actor, one(&actor, "\"Welcome to tanki\""));
    assert!(plain.autoplay && !plain.show_timer && plain.cap_answer_time_secs == 60);
    assert_eq!(plain.answer_action, AnswerAction::BuryCard);
    // A card in the filtered deck uses its home deck's preset ("Science"
    // turns autoplay off; the filtered deck has no preset of its own).
    let cram = search(&actor, "deck:\"Study Cram\"")[0];
    assert!(!study_config(&actor, cram).autoplay);
    let science = one(&actor, "\"Speed of light?\"");
    assert!(!study_config(&actor, science).autoplay);
}

#[test]
fn type_answer_and_compare() {
    let dir = tempfile::tempdir().unwrap();
    prepare(dir.path(), |col| {
        // Custom note types for the cases the stock ones cannot reach.
        let basic = col.get_notetype_by_name("Basic").unwrap().unwrap();
        let mut typing = Notetype {
            name: "Typing cases".into(),
            fields: basic.fields.clone(),
            templates: vec![
                CardTemplate::new(
                    "nc",
                    "{{Front}}\n\n{{type:nc:Back}}",
                    "{{FrontSide}}\n\n<hr id=answer>\n\n{{type:nc:Back}}",
                ),
                CardTemplate::new("unknown", "{{Front}} [[type:Missing]]", "{{FrontSide}}"),
                CardTemplate::new(
                    "cloze quirk",
                    "{{Front}} [[type:cloze:nc:Back]]",
                    "{{FrontSide}}",
                ),
            ],
            config: basic.config.clone(),
            ..Default::default()
        };
        typing.config.original_stock_kind = 0;
        typing.config.original_id = None;
        col.add_notetype(&mut typing, false).unwrap();
        let cloze = col.get_notetype_by_name("Cloze").unwrap().unwrap();
        let mut cloze_typing = (*cloze).clone();
        cloze_typing.id = anki::notetype::NotetypeId(0);
        cloze_typing.name = "Cloze typing".into();
        cloze_typing.config.original_stock_kind = 0;
        cloze_typing.config.original_id = None;
        cloze_typing.templates = vec![CardTemplate::new(
            "Cloze",
            "{{cloze:Text}}\n{{type:cloze:Text}}",
            "{{cloze:Text}}<br>\n{{type:cloze:Text}}\n[[type:cloze:Back Extra]]",
        )];
        col.add_notetype(&mut cloze_typing, false).unwrap();
        for (nt, fields) in [
            ("Typing cases", vec!["front", "ば"]),
            (
                "Cloze typing",
                vec!["{{c1::Paris}} is in {{c2::France}}", ""],
            ),
            ("Basic (type in the answer)", vec!["Q", ""]),
            ("Basic (type in the answer)", vec!["猫", "ねこ"]),
        ] {
            let nt = col.get_notetype_by_name(nt).unwrap().unwrap();
            let mut note =
                NotesService::new_note(col, pb::notetypes::NotetypeId { ntid: nt.id.0 }).unwrap();
            note.fields = fields.into_iter().map(String::from).collect();
            let _ = NotesService::add_note(
                col,
                pb::notes::AddNoteRequest {
                    note: Some(note),
                    deck_id: 1,
                },
            )
            .unwrap();
        }
        set_config(
            col,
            "flagLabels",
            serde_json::json!({"2": "Later", "7": ""}),
        );
        set_config(col, "cardStateCustomizer", "states.good.normal = 1;".into());
    });
    let actor = CollectionActor::open(dir.path()).unwrap();

    // Stock "Basic (type in the answer)", end to end.
    let neko = one(&actor, "ねこ");
    let card = render(&actor, neko);
    assert_eq!(card.ord, 0);
    assert_eq!(
        card.type_answer,
        Some(TypeAnswer::Input {
            expected: "ねこ".into(),
            combining: true
        })
    );
    assert_eq!(card.review_question(), format!("猫\n\n{TYPE_ANSWER_INPUT}"));
    let good = compare(&actor, "ねこ", "ねこ", true);
    assert_eq!(
        good,
        "<code id=typeans><span class=typeGood>ねこ</span></code>"
    );
    assert_eq!(
        card.review_answer(&good),
        format!("猫\n\n\n\n<hr id=answer>\n<div>{good}</div>")
    );
    let empty = compare(&actor, "ねこ", "", true);
    assert_eq!(empty, "<code id=typeans>ねこ</code>");
    // rslib's own example (`typeanswer.rs` tests, R3 §3.4).
    assert_eq!(
        compare(&actor, "123", "1123", true),
        "<code id=typeans><span class=typeBad>1</span><span class=typeGood>123</span><br><span id=typearrow>&darr;</span><br><span class=typeGood>123</span></code>"
    );

    // Empty field: placeholder removed.
    let empty_back = search(&actor, "\"note:Basic (type in the answer)\" Q")[0];
    let card = render(&actor, empty_back);
    assert_eq!(card.type_answer, Some(TypeAnswer::Empty));
    assert_eq!(card.review_question(), "Q\n\n");

    // nc: no combining.
    let nc = one(&actor, "\"note:Typing cases\" card:1");
    let card = render(&actor, nc);
    assert_eq!(
        card.type_answer,
        Some(TypeAnswer::Input {
            expected: "ば".into(),
            combining: false
        })
    );
    assert_eq!(
        compare(&actor, "ば", "は", false),
        "<code id=typeans><span class=typeGood>ば</span></code>"
    );
    // Unknown field (literal placeholder in the template).
    let unknown = render(&actor, one(&actor, "\"note:Typing cases\" card:2"));
    assert_eq!(
        unknown.type_answer,
        Some(TypeAnswer::Message(
            "Type answer: unknown field Missing".into()
        ))
    );
    assert_eq!(unknown.ord, 1);
    assert_eq!(
        unknown.review_question(),
        "front Type answer: unknown field Missing"
    );
    // `cloze:nc:Back` looks up a field called "nc" (reviewer.py quirk).
    let quirk = render(&actor, one(&actor, "\"note:Typing cases\" card:3"));
    assert_eq!(
        quirk.type_answer,
        Some(TypeAnswer::Message("Please run Tools>Empty Cards".into()))
    );

    // Cloze typing: the text of the card's own cloze number.
    let c1 = render(&actor, one(&actor, "\"note:Cloze typing\" card:1"));
    assert_eq!(
        c1.type_answer,
        Some(TypeAnswer::Input {
            expected: "Paris".into(),
            combining: true
        })
    );
    let c2 = render(&actor, one(&actor, "\"note:Cloze typing\" card:2"));
    assert_eq!(c2.ord, 1);
    assert_eq!(
        c2.type_answer,
        Some(TypeAnswer::Input {
            expected: "France".into(),
            combining: true
        })
    );
    assert!(c2.review_question().ends_with(TYPE_ANSWER_INPUT));
    let comparison = compare(&actor, "France", "Frence", true);
    let answer = c2.review_answer(&comparison);
    assert!(
        answer.contains(&format!("<div>{comparison}</div>")),
        "{answer}"
    );
    assert!(!answer.contains("[[type:"), "{answer}");

    // Collection prefs in StudyConfig.
    let config = study_config(&actor, neko);
    assert!(config.custom_scheduling_set);
    assert_eq!(config.flag_names[1], "Later");
    assert_eq!(
        config.flag_names[6], "",
        "a present label wins even when empty"
    );
    assert_eq!(config.flag_names[0], "Red");
    actor.close().unwrap();
}

#[test]
fn render_sets_template_ordinal() {
    let (_dir, actor) = study();
    let reverse = one(&actor, "\"le chat\" card:2");
    let card = render(&actor, reverse);
    assert_eq!(card.ord, 1);
    assert!(card.body_classes(false).starts_with("card card2 "));
    assert_eq!(card.type_answer, None);
    assert_eq!(card.review_question(), card.question);
    // Audio and image on a media card.
    let media = render(&actor, one(&actor, "\"Which colours*\""));
    assert!(media.question.contains("study-gradient.png"));
    assert_eq!(
        media.answer_audio,
        vec![AudioTag::SoundOrVideo("study-tone.wav".into())]
    );
}

#[test]
fn scheduler_status_and_upgrade() {
    let (_dir, actor) = study();
    assert_eq!(
        match actor.execute(Command::SchedulerStatus).unwrap() {
            Response::SchedulerStatus(s) => s,
            other => panic!("{other:?}"),
        },
        SchedulerStatus {
            v3: true,
            version: 2
        }
    );
    // Already v3: nothing to do.
    assert_eq!(
        changes(actor.execute(Command::UpgradeScheduler)),
        OpChanges::default()
    );
    drop(actor);

    for version in [1, 2] {
        let dir = tempfile::tempdir().unwrap();
        prepare(dir.path(), |col| {
            set_config(col, "schedVer", version.into());
            set_config(col, "sched2021", false.into());
            if version == 1 {
                // A v1 collection may lack the creation offset.
                let _ =
                    ConfigService::remove_config(col, "creationOffset".to_owned().into()).unwrap();
            }
        });
        let actor = CollectionActor::open(dir.path()).unwrap();
        actor
            .execute(Command::AddNote {
                notetype: "Basic".into(),
                fields: vec!["q".into(), "a".into()],
            })
            .unwrap();
        let status =
            |actor: &CollectionActor| match actor.execute(Command::SchedulerStatus).unwrap() {
                Response::SchedulerStatus(s) => s,
                other => panic!("{other:?}"),
            };
        assert_eq!(status(&actor), SchedulerStatus { v3: false, version });
        // The reviewer refuses to study without v3.
        match actor.execute(Command::Queue { limit: 1 }) {
            Err(CoreError::Invalid(message)) => {
                assert!(message.contains("v3"), "{message}");
                assert!(!message.contains(['\u{2068}', '\u{2069}']));
            }
            other => panic!("expected refusal, got {other:?}"),
        }
        let c = changes(actor.execute(Command::UpgradeScheduler));
        assert!(c.config);
        assert_eq!(c.card, version == 1);
        assert_eq!(
            status(&actor),
            SchedulerStatus {
                v3: true,
                version: 2
            }
        );
        assert_eq!(queue(&actor, 1).new, 1);
        actor.close().unwrap();
        // The creation offset (new timezone) is set after the upgrade.
        prepare(dir.path(), |col| {
            let offset = ConfigService::get_config_json(col, "creationOffset".to_owned().into());
            assert!(offset.is_ok(), "creationOffset missing after upgrade");
        });
    }
}
