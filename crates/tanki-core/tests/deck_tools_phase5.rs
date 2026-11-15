//! Decks tools stream: every `DeckToolsCommand` through the actor on the
//! temp study fixture.
use tanki_core::deck_tools::*;
use tanki_core::*;

struct Fixture {
    _dir: tempfile::TempDir,
    actor: CollectionActor,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let actor = CollectionActor::open(dir.path()).unwrap();
    actor.execute(Command::GenerateStudyFixture).unwrap();
    Fixture { _dir: dir, actor }
}

fn tools(actor: &CollectionActor, command: DeckToolsCommand) -> Result<DeckToolsResponse> {
    match actor.execute(Command::Manage(ManageCommand::DeckTools(command)))? {
        Response::Manage(ManageResponse::DeckTools(r)) => Ok(r),
        other => panic!("unexpected {other:?}"),
    }
}

fn invalid(actor: &CollectionActor, command: DeckToolsCommand) -> String {
    match tools(actor, command) {
        Err(CoreError::Invalid(message)) => message,
        other => panic!("expected Invalid, got {other:?}"),
    }
}

fn deck_names(actor: &CollectionActor) -> Vec<DeckName> {
    match actor
        .execute(Command::Notes(NoteCommand::DeckNames {
            include_filtered: true,
        }))
        .unwrap()
    {
        Response::Edit(EditResponse::DeckNames(names)) => names,
        other => panic!("unexpected {other:?}"),
    }
}

fn deck_id(actor: &CollectionActor, name: &str) -> i64 {
    deck_names(actor)
        .into_iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("no deck {name}"))
        .id
}

fn cards(actor: &CollectionActor, query: &str) -> Vec<i64> {
    match actor
        .execute(Command::Browse(BrowseCommand::Search {
            query: query.into(),
            mode: BrowseMode::Cards,
            order: SearchOrder::None,
        }))
        .unwrap()
    {
        Response::Edit(EditResponse::Ids(ids)) => ids,
        other => panic!("unexpected {other:?}"),
    }
}

fn current_deck(actor: &CollectionActor) -> DeckOverview {
    match actor.execute(Command::Overview).unwrap() {
        Response::Overview(o) => o,
        other => panic!("unexpected {other:?}"),
    }
}

fn form(actor: &CollectionActor, deck_id: i64) -> FilteredDeckForm {
    load(actor, deck_id, None, None)
}

fn load(
    actor: &CollectionActor,
    deck_id: i64,
    search: Option<&str>,
    search_2: Option<&str>,
) -> FilteredDeckForm {
    match tools(
        actor,
        DeckToolsCommand::FilteredDeck {
            deck_id,
            search: search.map(Into::into),
            search_2: search_2.map(Into::into),
        },
    )
    .unwrap()
    {
        DeckToolsResponse::FilteredDeck(form) => *form,
        other => panic!("unexpected {other:?}"),
    }
}

fn save(actor: &CollectionActor, deck: FilteredDeck) -> i64 {
    match tools(actor, DeckToolsCommand::SaveFilteredDeck(deck)).unwrap() {
        DeckToolsResponse::Saved { changes, deck_id } => {
            assert!(changes.deck && changes.study_queues, "{changes:?}");
            deck_id
        }
        other => panic!("unexpected {other:?}"),
    }
}

fn defaults(actor: &CollectionActor, deck_id: i64) -> CustomStudyDefaults {
    match tools(actor, DeckToolsCommand::CustomStudyDefaults { deck_id }).unwrap() {
        DeckToolsResponse::CustomStudyDefaults(d) => *d,
        other => panic!("unexpected {other:?}"),
    }
}

fn study(actor: &CollectionActor, deck_id: i64, study: CustomStudy) -> i64 {
    match tools(actor, DeckToolsCommand::CustomStudy { deck_id, study }).unwrap() {
        DeckToolsResponse::Saved { deck_id, .. } => deck_id,
        other => panic!("unexpected {other:?}"),
    }
}

fn describe(actor: &CollectionActor, deck_id: i64) -> DeckDescription {
    match tools(actor, DeckToolsCommand::Description { deck_id }).unwrap() {
        DeckToolsResponse::Description(d) => d,
        other => panic!("unexpected {other:?}"),
    }
}

const ADDED: i32 = 5;
const DUE: i32 = 6;
const RANDOM: i32 = 1;

#[test]
fn filtered_deck_loads_new_existing_and_seeded() {
    let f = fixture();
    let actor = &f.actor;

    // New: Anki's name and searches for the current deck; the second filter
    // is prefilled but off, as in desktop's dialog.
    let new = form(actor, 0);
    assert_eq!(new.deck.id, 0);
    assert!(new.deck.name.starts_with("Filtered Deck "), "{new:?}");
    assert_eq!(new.deck.first.search, "deck:Study is:due");
    assert_eq!((new.deck.first.limit, new.deck.first.order), (100, RANDOM));
    assert_eq!(new.deck.second.search, "deck:Study is:new");
    assert_eq!((new.deck.second.limit, new.deck.second.order), (20, DUE));
    assert!(!new.deck.second_enabled);
    assert!(new.deck.reschedule);
    assert_eq!(
        (
            new.deck.preview_again_secs,
            new.deck.preview_hard_secs,
            new.deck.preview_good_secs
        ),
        (60, 600, 0)
    );
    assert_eq!(new.labels.build, "Build");
    assert_eq!(new.labels.delay_again, "Delay for Again");
    assert_eq!(new.labels.title, format!("Options for {}", new.deck.name));

    // Without FSRS the two retrievability orders are not offered.
    let labels: Vec<_> = new.orders.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Oldest seen first",
            "Random",
            "Increasing intervals",
            "Decreasing intervals",
            "Most lapses",
            "Order added",
            "Order due",
            "Latest added first",
            "Relative overdueness"
        ]
    );
    assert_eq!(new.orders.last().unwrap().value, 10);

    // Existing.
    let cram = deck_id(actor, "Study Cram");
    let existing = form(actor, cram);
    assert_eq!(existing.deck.id, cram);
    assert_eq!(existing.deck.name, "Study Cram");
    assert_eq!(existing.deck.first.search, "tag:study::cram");
    assert_eq!(
        (existing.deck.first.limit, existing.deck.first.order),
        (100, ADDED)
    );
    assert!(!existing.deck.second_enabled);
    assert_eq!(
        (existing.deck.second.limit, existing.deck.second.order),
        (20, RANDOM)
    );
    assert_eq!(existing.labels.build, "Rebuild");

    // Seeded from the browser's search.
    let seeded = load(actor, 0, Some("tag:study::later"), None);
    assert_eq!(seeded.deck.first.search, "tag:study::later");
    assert!(!seeded.deck.second_enabled);
    let seeded = load(actor, 0, Some("is:due"), Some("is:new"));
    assert_eq!(seeded.deck.second.search, "is:new");
    assert!(seeded.deck.second_enabled);

    // A normal deck or a missing deck cannot be loaded.
    let study = deck_id(actor, "Study");
    invalid(
        actor,
        DeckToolsCommand::FilteredDeck {
            deck_id: study,
            search: None,
            search_2: None,
        },
    );
    invalid(
        actor,
        DeckToolsCommand::FilteredDeck {
            deck_id: 42,
            search: None,
            search_2: None,
        },
    );
}

#[test]
fn filtered_deck_builds_rebuilds_and_empties() {
    let f = fixture();
    let actor = &f.actor;
    // `deck:` also matches a card's home deck; exclude cards that are out.
    let home = "deck:Study::Vocabulary::Japanese -deck:filtered";
    let japanese = cards(actor, home).len();
    assert!(japanese >= 4);

    // Build: pulls cards out of their home deck and becomes current.
    let mut deck = form(actor, 0).deck;
    deck.name = "Tools Test".into();
    deck.first = FilteredTerm {
        search: "deck:Study::Vocabulary::Japanese".into(),
        limit: 2,
        order: ADDED,
    };
    let id = save(actor, deck);
    assert_ne!(id, 0);
    assert_eq!(cards(actor, "deck:\"Tools Test\"").len(), 2);
    assert_eq!(cards(actor, home).len(), japanese - 2);
    let overview = current_deck(actor);
    assert_eq!((overview.deck_id, overview.filtered), (id, true));

    // Edit: second filter, preview mode with delays; all of it round-trips.
    let mut deck = form(actor, id).deck;
    assert_eq!(deck.name, "Tools Test");
    assert_eq!(deck.first.search, "deck:Study::Vocabulary::Japanese");
    assert_eq!((deck.first.limit, deck.first.order), (2, ADDED));
    deck.second_enabled = true;
    deck.second = FilteredTerm {
        search: "tag:study::later".into(),
        limit: 5,
        order: DUE,
    };
    deck.reschedule = false;
    deck.preview_again_secs = 11;
    deck.preview_hard_secs = 22;
    deck.preview_good_secs = 33;
    deck.name = "Tools Test 2".into();
    assert_eq!(save(actor, deck.clone()), id);
    let later = cards(actor, "tag:study::later").len();
    assert!(later >= 1);
    assert_eq!(cards(actor, "deck:\"Tools Test 2\"").len(), 2 + later);
    let reloaded = form(actor, id).deck;
    assert_eq!(reloaded, deck);

    // Rebuild reports how many cards the deck holds now.
    let DeckToolsResponse::Count { changes, count } =
        tools(actor, DeckToolsCommand::RebuildFilteredDeck { deck_id: id }).unwrap()
    else {
        panic!()
    };
    assert_eq!(count, 2 + later);
    assert!(changes.card && changes.study_queues);

    // Empty returns them.
    let DeckToolsResponse::Changes(changes) =
        tools(actor, DeckToolsCommand::EmptyFilteredDeck { deck_id: id }).unwrap()
    else {
        panic!()
    };
    assert!(changes.card);
    assert!(cards(actor, "deck:\"Tools Test 2\"").is_empty());
    assert_eq!(cards(actor, home).len(), japanese);

    // Rebuild and empty need a filtered deck.
    let study = deck_id(actor, "Study");
    let message = invalid(
        actor,
        DeckToolsCommand::EmptyFilteredDeck { deck_id: study },
    );
    assert_eq!(message, "This action can only be used on a filtered deck.");
    invalid(
        actor,
        DeckToolsCommand::RebuildFilteredDeck { deck_id: study },
    );
}

#[test]
fn filtered_deck_save_reports_anki_errors_and_changes_nothing() {
    let f = fixture();
    let actor = &f.actor;
    let before = deck_names(actor);

    let mut deck = form(actor, 0).deck;
    deck.name = "Nothing".into();
    deck.first.search = "tag:no-such-tag".into();
    let message = invalid(actor, DeckToolsCommand::SaveFilteredDeck(deck.clone()));
    assert!(
        message.starts_with("No cards matched the provided search."),
        "{message}"
    );
    assert_eq!(deck_names(actor), before);

    let mut bad = deck.clone();
    bad.first.search = "deck:(".into();
    let message = invalid(actor, DeckToolsCommand::SaveFilteredDeck(bad));
    assert!(!message.is_empty());
    assert_eq!(deck_names(actor), before);

    // The second term is ignored unless enabled, even when it is invalid.
    deck.second.search = "(".into();
    deck.allow_empty = true;
    let id = save(actor, deck);
    assert!(cards(actor, "deck:Nothing").is_empty());
    let saved = form(actor, id).deck;
    assert!(!saved.second_enabled);
    assert_eq!(saved.first.search, "tag:no-such-tag");
}

#[test]
fn custom_study_defaults_and_limit_modes() {
    let f = fixture();
    let actor = &f.actor;
    let study_deck = deck_id(actor, "Study");

    let d = defaults(actor, study_deck);
    assert_eq!((d.deck_id, d.deck_name.as_str()), (study_deck, "Study"));
    assert_eq!((d.extend_new, d.extend_review), (0, 0));
    assert!(d.available_new_in_children > 0);
    assert!(d.card_count > 20);
    assert!(d.tags.iter().any(|t| t.name == "study::later"));
    // Anki lists the tags of cards that are in the deck right now; the cram
    // cards are out in the fixture's filtered deck.
    assert!(d.tags.iter().all(|t| t.name != "study::cram"));
    assert!(d.tags.iter().all(|t| !t.include && !t.exclude));
    assert!(d.tags.windows(2).all(|w| w[0].name < w[1].name));
    assert_eq!(
        d.labels.available_new,
        format!(
            "Available new cards: {} ({} in subdecks)",
            d.available_new, d.available_new_in_children
        )
    );
    assert_eq!(
        d.labels.modes,
        [
            "Increase today's new card limit",
            "Increase today's review card limit",
            "Review forgotten cards",
            "Review ahead",
            "Preview new cards",
            "Study by card state or tag"
        ]
    );
    assert_eq!(d.labels.before[2], "Review cards forgotten in the last");
    assert_eq!(d.labels.days.get(1), "day");
    assert_eq!(d.labels.days.get(3), "days");
    assert_eq!(d.labels.cards_from_the_deck.get(100), "cards from the deck");
    assert_eq!(
        d.labels.cram_kinds[3],
        "All cards in random order (don't reschedule)"
    );
    assert_eq!(d.labels.session_name, "Custom Study Session");

    // A leaf deck: no "(n in subdecks)".
    let french = deck_id(actor, "Study::Vocabulary::French");
    let leaf = defaults(actor, french);
    assert_eq!(leaf.available_new_in_children, 0);
    assert_eq!(
        leaf.labels.available_new,
        format!("Available new cards: {}", leaf.available_new)
    );

    // Limit increases are remembered as the next default; decreases are not.
    let new_before = current_deck(actor).new;
    assert_eq!(study(actor, french, CustomStudy::NewLimitDelta(5)), 0);
    assert_eq!(study(actor, french, CustomStudy::ReviewLimitDelta(4)), 0);
    let d = defaults(actor, french);
    assert_eq!((d.extend_new, d.extend_review), (5, 4));
    assert_eq!(study(actor, french, CustomStudy::NewLimitDelta(-2)), 0);
    assert_eq!(defaults(actor, french).extend_new, 5);
    // No deck was built and the current deck is unchanged.
    let overview = current_deck(actor);
    assert_eq!(overview.deck_id, study_deck);
    assert!(overview.new >= new_before);

    // Filtered and missing decks have no custom study.
    let cram = deck_id(actor, "Study Cram");
    invalid(
        actor,
        DeckToolsCommand::CustomStudyDefaults { deck_id: cram },
    );
    invalid(actor, DeckToolsCommand::CustomStudyDefaults { deck_id: 42 });
}

#[test]
fn custom_study_builds_the_session_deck_in_each_mode() {
    let f = fixture();
    let actor = &f.actor;
    let study_deck = deck_id(actor, "Study");
    let in_session = |actor: &CollectionActor| cards(actor, "deck:\"Custom Study Session\"");

    // Forgotten: the fixture answered Again today.
    let forgotten = cards(
        actor,
        "rated:1:1 deck:Study -is:suspended -is:buried -deck:filtered",
    );
    assert!(!forgotten.is_empty());
    let session = study(actor, study_deck, CustomStudy::ForgotDays(1));
    assert_ne!(session, 0);
    assert_eq!(session, deck_id(actor, "Custom Study Session"));
    let mut got = in_session(actor);
    got.sort();
    let mut want = forgotten;
    want.sort();
    assert_eq!(got, want);
    let overview = current_deck(actor);
    assert_eq!((overview.deck_id, overview.filtered), (session, true));
    // Anki's fixed settings for this mode: no rescheduling, 60 s / 600 s / 0.
    let built = form(actor, session).deck;
    assert!(!built.reschedule);
    assert_eq!(
        (
            built.preview_again_secs,
            built.preview_hard_secs,
            built.preview_good_secs
        ),
        (60, 600, 0)
    );
    assert_eq!((built.first.order, built.first.limit), (RANDOM, 99_999));

    // Review ahead reuses the deck: the earlier cards go home first.
    let ahead = cards(actor, "tag:study::later");
    assert_eq!(
        study(actor, study_deck, CustomStudy::ReviewAheadDays(10)),
        session
    );
    let got = in_session(actor);
    assert!(ahead.iter().all(|id| got.contains(id)), "{got:?} {ahead:?}");
    let built = form(actor, session).deck;
    assert!(built.reschedule);
    assert_eq!(built.first.order, DUE);

    // Preview new cards added today.
    assert_eq!(
        study(actor, study_deck, CustomStudy::PreviewDays(1)),
        session
    );
    let got = in_session(actor);
    assert!(!got.is_empty());
    let new: Vec<i64> = cards(actor, "is:new");
    assert!(got.iter().all(|id| new.contains(id)));
    assert!(!form(actor, session).deck.reschedule);

    // Card state and tags: limited, and the tag choice is remembered.
    let cram = |kind, card_limit, include: &[&str], exclude: &[&str]| CustomStudy::Cram {
        kind,
        card_limit,
        tags_to_include: include.iter().map(|t| (*t).to_owned()).collect(),
        tags_to_exclude: exclude.iter().map(|t| (*t).to_owned()).collect(),
    };
    assert_eq!(
        study(actor, study_deck, cram(CramKind::New, 2, &[], &[])),
        session
    );
    assert_eq!(in_session(actor).len(), 2);
    assert_eq!(form(actor, session).deck.first.order, ADDED);

    assert_eq!(
        study(
            actor,
            study_deck,
            cram(CramKind::All, 100, &["study::later"], &["study::suspended"])
        ),
        session
    );
    let mut got = in_session(actor);
    got.sort();
    let mut want = ahead.clone();
    want.sort();
    assert_eq!(got, want);
    assert!(!form(actor, session).deck.reschedule);
    // The tag list covers cards that are in the deck now: send them home.
    assert!(defaults(actor, study_deck)
        .tags
        .iter()
        .all(|t| t.name != "study::later"));
    tools(
        actor,
        DeckToolsCommand::EmptyFilteredDeck { deck_id: session },
    )
    .unwrap();
    let d = defaults(actor, study_deck);
    let tag = |name: &str| d.tags.iter().find(|t| t.name == name).unwrap().clone();
    assert!(tag("study::later").include && !tag("study::later").exclude);
    assert!(tag("study::suspended").exclude && !tag("study::suspended").include);

    for kind in [CramKind::Due, CramKind::Review] {
        assert_eq!(study(actor, study_deck, cram(kind, 50, &[], &[])), session);
        assert!(!in_session(actor).is_empty());
        assert!(form(actor, session).deck.reschedule);
    }

    // Nothing matches: Anki's message, and the previous session is intact.
    let before = in_session(actor);
    let message = invalid(
        actor,
        DeckToolsCommand::CustomStudy {
            deck_id: study_deck,
            study: cram(CramKind::Due, 10, &["study::later"], &["study::later"]),
        },
    );
    assert_eq!(message, "No cards matched the criteria you provided.");
    assert_eq!(in_session(actor), before);

    // More than 100 tags are refused before Anki is called.
    let many: Vec<String> = (0..=MAX_TAGS).map(|i| format!("t{i}")).collect();
    let message = invalid(
        actor,
        DeckToolsCommand::CustomStudy {
            deck_id: study_deck,
            study: CustomStudy::Cram {
                kind: CramKind::All,
                card_limit: 10,
                tags_to_include: many,
                tags_to_exclude: vec![],
            },
        },
    );
    assert!(
        message.starts_with("A maximum of 100 tags can be selected."),
        "{message}"
    );
}

#[test]
fn custom_study_refuses_a_normal_deck_with_the_session_name() {
    let f = fixture();
    let actor = &f.actor;
    let study_deck = deck_id(actor, "Study");
    tools(
        actor,
        DeckToolsCommand::CreateDeck {
            name: "Custom Study Session".into(),
        },
    )
    .unwrap();
    let message = invalid(
        actor,
        DeckToolsCommand::CustomStudy {
            deck_id: study_deck,
            study: CustomStudy::ForgotDays(1),
        },
    );
    assert_eq!(
        message,
        "Please rename the existing Custom Study deck first."
    );
}

#[test]
fn deck_description_get_and_set_honour_the_markdown_flag() {
    let f = fixture();
    let actor = &f.actor;
    let study_deck = deck_id(actor, "Study");

    let d = describe(actor, study_deck);
    assert_eq!(d.name, "Study");
    assert!(d.markdown);
    assert!(d
        .text
        .starts_with("Welcome to the **tanki** study fixture."));
    assert!(d.html.contains("<strong>tanki</strong>"), "{}", d.html);
    assert_eq!(d.title, "Description");
    assert_eq!(d.markdown_label, "Interpret as Markdown");
    assert_eq!(d.html, current_deck(actor).description);

    // HTML: stored and shown as is.
    let DeckToolsResponse::Changes(changes) = tools(
        actor,
        DeckToolsCommand::SetDescription {
            deck_id: study_deck,
            text: "<b>Bold</b> and **stars**".into(),
            markdown: false,
        },
    )
    .unwrap() else {
        panic!()
    };
    assert!(changes.deck);
    let d = describe(actor, study_deck);
    assert!(!d.markdown);
    assert_eq!(d.text, "<b>Bold</b> and **stars**");
    assert_eq!(d.html, d.text);
    assert_eq!(current_deck(actor).description, d.text);

    // The same text as Markdown.
    tools(
        actor,
        DeckToolsCommand::SetDescription {
            deck_id: study_deck,
            text: d.text.clone(),
            markdown: true,
        },
    )
    .unwrap();
    let d = describe(actor, study_deck);
    assert!(d.markdown);
    assert!(d.html.contains("<strong>stars</strong>"), "{}", d.html);

    // The rest of the deck is untouched (same preset, still undoable).
    let science = deck_id(actor, "Study::Science");
    let before = describe(actor, science);
    assert!(!before.markdown);
    assert!(before.text.contains("Leeches"));
    let Response::UndoStatus(status) = actor.execute(Command::UndoStatus).unwrap() else {
        panic!()
    };
    assert_eq!(status.undo.as_deref(), Some("Update Deck"));

    // Filtered decks have no description to edit.
    let cram = deck_id(actor, "Study Cram");
    invalid(actor, DeckToolsCommand::Description { deck_id: cram });
    invalid(
        actor,
        DeckToolsCommand::SetDescription {
            deck_id: cram,
            text: "x".into(),
            markdown: false,
        },
    );
    invalid(actor, DeckToolsCommand::Description { deck_id: 42 });
}

#[test]
fn decks_are_created_renamed_moved_and_deleted() {
    let f = fixture();
    let actor = &f.actor;

    let DeckToolsResponse::Saved {
        changes,
        deck_id: leaf,
    } = tools(
        actor,
        DeckToolsCommand::CreateDeck {
            name: "Tools::Branch::Leaf".into(),
        },
    )
    .unwrap()
    else {
        panic!()
    };
    assert!(changes.deck);
    assert_eq!(deck_id(actor, "Tools::Branch::Leaf"), leaf);
    let branch = deck_id(actor, "Tools::Branch");

    tools(
        actor,
        DeckToolsCommand::RenameDeck {
            deck_id: branch,
            new_name: "Tools::Limb".into(),
        },
    )
    .unwrap();
    assert_eq!(deck_id(actor, "Tools::Limb::Leaf"), leaf);

    // Move to another parent, then to the top level.
    let later = deck_id(actor, "Study::Later");
    let count = |r| match r {
        DeckToolsResponse::Count { count, changes } => {
            assert!(changes.deck);
            count
        }
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(
        count(
            tools(
                actor,
                DeckToolsCommand::ReparentDecks {
                    deck_ids: vec![leaf],
                    new_parent: later,
                },
            )
            .unwrap()
        ),
        1
    );
    assert_eq!(deck_id(actor, "Study::Later::Leaf"), leaf);
    assert_eq!(
        count(
            tools(
                actor,
                DeckToolsCommand::ReparentDecks {
                    deck_ids: vec![later],
                    new_parent: 0,
                },
            )
            .unwrap()
        ),
        1
    );
    assert_eq!(deck_id(actor, "Later"), later);
    assert_eq!(deck_id(actor, "Later::Leaf"), leaf);

    // A filtered deck cannot be a parent.
    let cram = deck_id(actor, "Study Cram");
    let message = invalid(
        actor,
        DeckToolsCommand::ReparentDecks {
            deck_ids: vec![leaf],
            new_parent: cram,
        },
    );
    assert_eq!(message, "Filtered decks can not have child decks.");
    assert_eq!(deck_id(actor, "Later::Leaf"), leaf);

    // What the prompts show: counts, and where the deck may move.
    let DeckToolsResponse::DeckPrompt(info) =
        tools(actor, DeckToolsCommand::DeckPrompt { deck_id: later }).unwrap()
    else {
        panic!()
    };
    let held = cards(actor, "deck:Later").len();
    assert!(held >= 1);
    assert_eq!(info.name, "Later");
    assert!(!info.filtered);
    assert_eq!(info.card_count as usize, held);
    assert_eq!(info.subdeck_count, 1);
    let targets: Vec<&str> = info.targets.iter().map(|t| t.name.as_str()).collect();
    assert!(targets.contains(&"Study"), "{targets:?}");
    assert!(targets.contains(&"Tools::Limb"), "{targets:?}");
    // Not itself, its subdecks or filtered decks.
    assert!(
        !targets
            .iter()
            .any(|t| t.starts_with("Later") || *t == "Study Cram"),
        "{targets:?}"
    );
    let DeckToolsResponse::DeckPrompt(leaf_info) =
        tools(actor, DeckToolsCommand::DeckPrompt { deck_id: leaf }).unwrap()
    else {
        panic!()
    };
    assert_eq!(leaf_info.name, "Later::Leaf");
    // The present parent is not offered.
    assert!(leaf_info.targets.iter().all(|t| t.name != "Later"));
    assert_eq!(leaf_info.subdeck_count, 0);
    let DeckToolsResponse::DeckPrompt(blank) =
        tools(actor, DeckToolsCommand::DeckPrompt { deck_id: 0 }).unwrap()
    else {
        panic!()
    };
    assert_eq!(blank.name, "");
    assert!(blank.targets.is_empty());
    assert_eq!(blank.labels.create, "Create Deck");
    assert_eq!(blank.labels.new_deck_name, "New deck name:");
    assert_eq!(blank.labels.rename, "Rename Deck");
    assert_eq!(blank.labels.new_name, "New name:");
    assert_eq!(blank.labels.delete, "Delete Deck");
    assert!(tools(actor, DeckToolsCommand::DeckPrompt { deck_id: 424_242 }).is_err());

    // Delete takes subdecks and cards along and reports the cards.
    match tools(
        actor,
        DeckToolsCommand::DeleteDecks {
            deck_ids: vec![later],
        },
    )
    .unwrap()
    {
        DeckToolsResponse::Deleted {
            changes,
            count,
            message,
        } => {
            assert!(changes.deck);
            assert_eq!(count, held);
            assert!(
                message.starts_with(&format!("{held} card")) && message.ends_with("deleted."),
                "{message}"
            );
        }
        other => panic!("unexpected {other:?}"),
    }
    let names = deck_names(actor);
    assert!(names.iter().all(|d| !d.name.starts_with("Later")));
    assert!(cards(actor, "tag:study::later").is_empty());
}

#[test]
fn mutations_are_broadcast() {
    let f = fixture();
    let actor = &f.actor;
    let changes = actor.subscribe();
    let study_deck = deck_id(actor, "Study");
    tools(
        actor,
        DeckToolsCommand::SetDescription {
            deck_id: study_deck,
            text: "x".into(),
            markdown: false,
        },
    )
    .unwrap();
    assert!(changes.try_recv().unwrap().deck);
    describe(actor, study_deck);
    assert!(changes.try_recv().is_err());
}
