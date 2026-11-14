//! Browser stream: every `BrowseCommand` through the actor on temp collections.
use anki::services::ConfigService;
use tanki_core::*;

fn browse(actor: &CollectionActor, command: BrowseCommand) -> Result<EditResponse> {
    match actor.execute(Command::Browse(command))? {
        Response::Edit(r) => Ok(r),
        other => panic!("unexpected {other:?}"),
    }
}

fn notes(actor: &CollectionActor, command: NoteCommand) -> EditResponse {
    match actor.execute(Command::Notes(command)).unwrap() {
        Response::Edit(r) => r,
        other => panic!("unexpected {other:?}"),
    }
}

fn ids(actor: &CollectionActor, query: &str, mode: BrowseMode, order: SearchOrder) -> Vec<i64> {
    let EditResponse::Ids(ids) = browse(
        actor,
        BrowseCommand::Search {
            query: query.into(),
            mode,
            order,
        },
    )
    .unwrap() else {
        panic!("ids")
    };
    ids
}

fn cards(actor: &CollectionActor, query: &str) -> Vec<i64> {
    ids(actor, query, BrowseMode::Cards, SearchOrder::None)
}

fn text(actor: &CollectionActor, command: BrowseCommand) -> Result<String> {
    match browse(actor, command)? {
        EditResponse::Text(t) => Ok(t),
        other => panic!("unexpected {other:?}"),
    }
}

fn counted(actor: &CollectionActor, command: BrowseCommand) -> usize {
    match browse(actor, command).unwrap() {
        EditResponse::Count { count, .. } => count,
        other => panic!("unexpected {other:?}"),
    }
}

fn changed(actor: &CollectionActor, command: BrowseCommand) -> OpChanges {
    match browse(actor, command).unwrap() {
        EditResponse::Changes(c) => c,
        other => panic!("unexpected {other:?}"),
    }
}

fn notetype_id(actor: &CollectionActor, name: &str) -> i64 {
    let EditResponse::Notetypes(all) = notes(actor, NoteCommand::Notetypes) else {
        panic!()
    };
    all.into_iter().find(|n| n.name == name).unwrap().id
}

/// Adds a note of `notetype` to deck 1 with these fields and tags.
fn add(actor: &CollectionActor, notetype: &str, fields: &[&str], tags: &[&str]) -> i64 {
    let notetype_id = notetype_id(actor, notetype);
    let EditResponse::Note(mut note) = notes(actor, NoteCommand::NewNote { notetype_id }) else {
        panic!()
    };
    note.fields = fields.iter().map(|f| f.to_string()).collect();
    note.tags = tags.iter().map(|t| t.to_string()).collect();
    let EditResponse::NoteAdded { id, .. } =
        notes(actor, NoteCommand::AddNote { note, deck_id: 1 })
    else {
        panic!()
    };
    id
}

fn note(actor: &CollectionActor, id: i64) -> NoteData {
    let EditResponse::Note(n) = notes(actor, NoteCommand::GetNote { id }) else {
        panic!()
    };
    n
}

fn rows(
    actor: &CollectionActor,
    mode: BrowseMode,
    columns: &[&str],
    ids: &[i64],
) -> Vec<BrowserRow> {
    let EditResponse::Rows(rows) = browse(
        actor,
        BrowseCommand::Rows {
            mode,
            columns: columns.iter().map(|c| c.to_string()).collect(),
            ids: ids.to_vec(),
        },
    )
    .unwrap() else {
        panic!()
    };
    rows
}

fn settings(actor: &CollectionActor) -> BrowserSettings {
    let EditResponse::Settings(s) = browse(actor, BrowseCommand::Settings).unwrap() else {
        panic!()
    };
    s
}

fn sidebar(actor: &CollectionActor) -> SidebarData {
    let EditResponse::Sidebar(s) = browse(actor, BrowseCommand::Sidebar).unwrap() else {
        panic!()
    };
    s
}

fn invalid(result: Result<impl std::fmt::Debug>) -> String {
    match result {
        Err(CoreError::Invalid(message)) => message,
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[test]
fn search_in_both_modes_with_every_order() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actor = CollectionActor::open(dir.path())?;
    let b = add(&actor, "Basic", &["b", "1"], &[]);
    let a = add(&actor, "Basic (and reversed card)", &["a", "2"], &[]);
    let c = add(&actor, "Basic", &["c", "3"], &[]);
    let field = |key: &str, reverse| SearchOrder::Column {
        key: key.into(),
        reverse,
    };
    // Notes mode: by sort field, reversed, creation order (unset
    // `noteSortType` falls back to `noteCrt`, as desktop), no order.
    let n = |order| ids(&actor, "", BrowseMode::Notes, order);
    assert_eq!(n(field("noteFld", false)), [a, b, c]);
    assert_eq!(n(field("noteFld", true)), [c, b, a]);
    assert_eq!(n(SearchOrder::Default), [b, a, c]);
    let mut unordered = n(SearchOrder::None);
    unordered.sort();
    assert_eq!(unordered, [b, a, c]);
    // Cards mode: a new collection stores `sortType: noteFld`.
    let all = ids(&actor, "deck:*", BrowseMode::Cards, field("noteFld", false));
    assert_eq!(all.len(), 4);
    assert_eq!(
        ids(&actor, "deck:*", BrowseMode::Cards, SearchOrder::Default),
        all
    );
    let EditResponse::Ids(of_a) =
        browse(&actor, BrowseCommand::CardsOfNotes { note_ids: vec![a] })?
    else {
        panic!()
    };
    assert_eq!(of_a.len(), 2);
    assert!(all[..2].iter().all(|id| of_a.contains(id)));
    assert_eq!(cards(&actor, "a").len(), 2);
    // Only built-in, sortable columns are accepted.
    assert!(invalid(browse(
        &actor,
        BrowseCommand::Search {
            query: String::new(),
            mode: BrowseMode::Cards,
            order: field("question", false),
        },
    ))
    .contains("question"));
    invalid(browse(
        &actor,
        BrowseCommand::Search {
            query: String::new(),
            mode: BrowseMode::Notes,
            order: field("stability", false),
        },
    ));
    invalid(browse(
        &actor,
        BrowseCommand::Search {
            query: String::new(),
            mode: BrowseMode::Cards,
            order: field("c.id desc", false),
        },
    ));
    // Parse errors carry Anki's message.
    let message = invalid(browse(
        &actor,
        BrowseCommand::Search {
            query: "(a".into(),
            mode: BrowseMode::Cards,
            order: SearchOrder::None,
        },
    ));
    assert!(message.contains(')'), "{message}");
    assert!(!message.contains('\u{2068}'));
    Ok(())
}

#[test]
fn normalize_join_and_replace() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actor = CollectionActor::open(dir.path())?;
    let normalize = |query: &str| {
        text(
            &actor,
            BrowseCommand::NormalizeSearch {
                query: query.into(),
            },
        )
    };
    assert_eq!(normalize("deck:a    tag:b")?, "deck:a tag:b");
    assert_eq!(normalize("a or  b")?, "a OR b");
    assert!(!invalid(normalize("a and")).is_empty());
    let join = |joiner, negate| {
        text(
            &actor,
            BrowseCommand::JoinSearch {
                existing: "deck:a".into(),
                additional: "tag:b".into(),
                joiner,
                negate,
            },
        )
        .unwrap()
    };
    assert_eq!(join(SearchJoiner::And, false), "deck:a tag:b");
    assert_eq!(join(SearchJoiner::Or, false), "deck:a OR tag:b");
    assert_eq!(join(SearchJoiner::And, true), "deck:a -tag:b");
    assert_eq!(join(SearchJoiner::Or, true), "deck:a OR -tag:b");
    assert_eq!(
        text(
            &actor,
            BrowseCommand::ReplaceSearch {
                existing: "deck:a tag:b".into(),
                replacement: "deck:c".into(),
            },
        )?,
        "deck:c tag:b"
    );
    invalid(text(
        &actor,
        BrowseCommand::JoinSearch {
            existing: "(".into(),
            additional: "x".into(),
            joiner: SearchJoiner::And,
            negate: false,
        },
    ));
    Ok(())
}

#[test]
fn columns_rows_colours_and_deleted_rows() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actor = CollectionActor::open(dir.path())?;
    let EditResponse::Columns(columns) = browse(&actor, BrowseCommand::Columns)? else {
        panic!()
    };
    let column = |key: &str| columns.iter().find(|c| c.key == key).unwrap().clone();
    assert_eq!(column("noteFld").sort_cards, Some(SortDirection::Ascending));
    assert_eq!(
        column("cardDue").sort_cards,
        Some(SortDirection::Descending)
    );
    assert_eq!(column("question").sort_cards, None);
    assert_eq!(column("stability").sort_notes, None);
    assert_eq!(column("template").notes_label, "Cards");
    assert!(!columns.iter().any(|c| c.key.is_empty()));

    let flagged = add(&actor, "Basic", &["flagged", ""], &["marked"]);
    let marked = add(&actor, "Basic", &["marked", ""], &["Marked"]);
    let suspended = add(&actor, "Basic", &["suspended", ""], &[]);
    let buried = add(&actor, "Basic", &["buried", ""], &[]);
    let plain = add(&actor, "Basic", &["plain", ""], &[]);
    let card = |nid: i64| cards(&actor, &format!("nid:{nid}"))[0];
    let all: Vec<i64> = [flagged, marked, suspended, buried, plain]
        .into_iter()
        .map(card)
        .collect();
    let suspend = |ids: Vec<i64>, mode| {
        actor
            .execute(Command::BuryOrSuspend {
                target: Target::Cards(ids),
                mode,
            })
            .unwrap()
    };
    suspend(all[..3].to_vec(), BuryMode::Suspend);
    suspend(vec![all[3]], BuryMode::Bury);
    actor.execute(Command::SetFlag {
        card_ids: vec![all[0]],
        flag: 1,
    })?;
    let got = rows(&actor, BrowseMode::Cards, &["noteFld", "deck"], &all);
    assert_eq!(
        got.iter().map(|r| r.color).collect::<Vec<_>>(),
        [
            RowColor::Flag(1),
            RowColor::Marked,
            RowColor::Suspended,
            RowColor::Buried,
            RowColor::Default
        ]
    );
    assert_eq!(got[4].cells.len(), 2);
    assert_eq!(got[4].cells[0].text, "plain");
    assert_eq!(got[4].cells[1].text, "Default");
    assert_eq!(got[4].id, all[4]);
    // Notes mode: marked or default only; the table flag follows the mode.
    let note_rows = rows(&actor, BrowseMode::Notes, &["noteFld"], &[flagged, plain]);
    assert_eq!(note_rows[0].color, RowColor::Marked);
    assert_eq!(note_rows[1].color, RowColor::Default);
    assert_eq!(settings(&actor).mode, BrowseMode::Notes);
    // A deleted id is an error row, not a failure.
    actor.execute(Command::DeleteNotes {
        note_ids: vec![plain],
    })?;
    let got = rows(&actor, BrowseMode::Cards, &["noteFld"], &all[3..]);
    assert!(got[0].error.is_none());
    assert!(got[1].cells.is_empty());
    assert_eq!(got[1].id, all[4]);
    assert!(!got[1].error.as_deref().unwrap().is_empty());
    let got = rows(&actor, BrowseMode::Notes, &["noteFld"], &[plain]);
    assert!(got[0].error.is_some());
    Ok(())
}

fn raw_config(profile: &std::path::Path, key: &str) -> Option<serde_json::Value> {
    let mut col = anki::collection::CollectionBuilder::new(profile.join("collection.anki2"))
        .build()
        .unwrap();
    let value = ConfigService::get_config_json(&mut col, key.to_owned().into())
        .ok()
        .map(|j| serde_json::from_slice(&j.json).unwrap());
    col.close(None).unwrap();
    value
}

fn write_raw_config(profile: &std::path::Path, key: &str, value: serde_json::Value) {
    let mut col = anki::collection::CollectionBuilder::new(profile.join("collection.anki2"))
        .build()
        .unwrap();
    let _ = ConfigService::set_config_json(
        &mut col,
        anki_proto::config::SetConfigJsonRequest {
            key: key.into(),
            value_json: serde_json::to_vec(&value).unwrap(),
            undoable: false,
        },
    )
    .unwrap();
    col.close(None).unwrap();
}

#[test]
fn settings_use_desktop_keys_and_defaults() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actor = CollectionActor::open(dir.path())?;
    let fresh = settings(&actor);
    assert_eq!(
        fresh,
        BrowserSettings {
            mode: BrowseMode::Cards,
            card_columns: ["noteFld", "template", "cardDue", "deck"]
                .map(String::from)
                .to_vec(),
            note_columns: ["noteFld", "note", "template", "noteTags"]
                .map(String::from)
                .to_vec(),
            card_sort: "noteFld".into(),
            card_sort_reverse: false,
            note_sort: "noteCrt".into(),
            note_sort_reverse: false,
        }
    );
    let new = BrowserSettings {
        mode: BrowseMode::Notes,
        card_columns: vec!["question".into(), "cardIvl".into()],
        note_columns: vec!["noteFld".into()],
        card_sort: "cardIvl".into(),
        card_sort_reverse: true,
        note_sort: "noteCrt".into(),
        note_sort_reverse: true,
    };
    let changes = changed(&actor, BrowseCommand::SetSettings(new.clone()));
    assert!(changes.config);
    assert_eq!(settings(&actor), new);
    // Writing the same settings again changes nothing.
    assert_eq!(
        changed(&actor, BrowseCommand::SetSettings(new.clone())),
        OpChanges::default()
    );
    invalid(browse(
        &actor,
        BrowseCommand::SetSettings(BrowserSettings {
            card_columns: vec![],
            ..new.clone()
        }),
    ));
    // Default order now follows the stored notes sort.
    let first = add(&actor, "Basic", &["x", ""], &[]);
    let second = add(&actor, "Basic", &["y", ""], &[]);
    assert_eq!(
        ids(&actor, "", BrowseMode::Notes, SearchOrder::Default),
        [second, first]
    );
    actor.close()?;
    let raw = |key| raw_config(dir.path(), key);
    assert_eq!(
        raw("activeCols"),
        Some(serde_json::json!(["question", "cardIvl"]))
    );
    assert_eq!(raw("activeNoteCols"), Some(serde_json::json!(["noteFld"])));
    assert_eq!(raw("sortType"), Some(serde_json::json!("cardIvl")));
    assert_eq!(raw("sortBackwards"), Some(serde_json::json!(true)));
    assert_eq!(raw("noteSortType"), Some(serde_json::json!("noteCrt")));
    assert_eq!(
        raw("browserNoteSortBackwards"),
        Some(serde_json::json!(true))
    );
    assert_eq!(
        raw("browserTableShowNotesMode"),
        Some(serde_json::json!(true))
    );
    // What desktop wrote is read back; unknown sort keys fall back to noteCrt.
    write_raw_config(dir.path(), "noteSortType", serde_json::json!("addonColumn"));
    write_raw_config(dir.path(), "activeCols", serde_json::json!("not a list"));
    let actor = CollectionActor::open_with_options(
        dir.path(),
        OpenOptions {
            seen_before: true,
            ..Default::default()
        },
    )?;
    let s = settings(&actor);
    assert_eq!(s.note_sort, "noteCrt");
    assert_eq!(s.card_columns, fresh.card_columns);
    assert_eq!(s.card_sort, "cardIvl");
    Ok(())
}

#[test]
fn sidebar_contents_collapse_state_and_saved_searches() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actor = CollectionActor::open(dir.path())?;
    let nid = add(&actor, "Basic", &["q", "a"], &["lang::french", "zoo"]);
    let card = cards(&actor, &format!("nid:{nid}"));
    counted(
        &actor,
        BrowseCommand::SetDeckByName {
            card_ids: card.clone(),
            name: "Lang::French".into(),
        },
    );
    let data = sidebar(&actor);
    assert_eq!(data.flag_names.len(), 7);
    assert_eq!(data.flag_names[0], "Red");
    assert!(data.notetypes.iter().any(|n| n.name == "Basic"));
    let lang = data.decks.iter().find(|d| d.name == "Lang").unwrap();
    assert_eq!(lang.full_name, "Lang");
    assert_eq!(lang.children[0].full_name, "Lang::French");
    // New decks start collapsed in the browser (rslib `Deck::new_normal`).
    assert!(lang.collapsed);
    let tag_names: Vec<_> = data
        .tags
        .children
        .iter()
        .map(|t| t.full_name.clone())
        .collect();
    assert_eq!(tag_names, ["lang", "zoo"]);
    assert_eq!(data.tags.children[0].children[0].full_name, "lang::french");
    assert!(data.saved_searches.is_empty());

    // Browser collapse state is separate from the deck list's.
    changed(
        &actor,
        BrowseCommand::SetDeckCollapsed {
            deck_id: lang.id,
            collapsed: false,
        },
    );
    changed(
        &actor,
        BrowseCommand::SetTagCollapsed {
            name: "lang".into(),
            collapsed: true,
        },
    );
    let data = sidebar(&actor);
    assert!(
        !data
            .decks
            .iter()
            .find(|d| d.name == "Lang")
            .unwrap()
            .collapsed
    );
    assert!(data.tags.children[0].collapsed);
    let Response::DeckTree(tree) = actor.execute(Command::DeckTree)? else {
        panic!()
    };
    // The deck list's state did not follow, and does not lead either.
    assert!(
        tree.children
            .iter()
            .find(|d| d.name == "Lang")
            .unwrap()
            .collapsed
    );
    changed(
        &actor,
        BrowseCommand::SetDeckCollapsed {
            deck_id: lang.id,
            collapsed: true,
        },
    );
    actor.execute(Command::SetDeckCollapsed {
        deck_id: lang.id,
        collapsed: false,
    })?;
    assert!(
        sidebar(&actor)
            .decks
            .iter()
            .find(|d| d.name == "Lang")
            .unwrap()
            .collapsed
    );

    let changes = changed(
        &actor,
        BrowseCommand::SetSavedSearches {
            searches: vec![
                ("zeta".into(), "tag:zoo".into()),
                ("Alpha".into(), "deck:Lang".into()),
            ],
        },
    );
    assert!(changes.config);
    assert_eq!(
        sidebar(&actor).saved_searches,
        [
            ("Alpha".to_string(), "deck:Lang".to_string()),
            ("zeta".to_string(), "tag:zoo".to_string())
        ]
    );
    actor.close()?;
    assert_eq!(
        raw_config(dir.path(), "savedFilters"),
        Some(serde_json::json!({"Alpha": "deck:Lang", "zeta": "tag:zoo"}))
    );
    // Custom flag labels (`flagLabels`) win over the defaults.
    write_raw_config(dir.path(), "flagLabels", serde_json::json!({"2": "Later"}));
    let actor = CollectionActor::open_with_options(
        dir.path(),
        OpenOptions {
            seen_before: true,
            ..Default::default()
        },
    )?;
    let data = sidebar(&actor);
    assert_eq!(data.flag_names[..3], ["Red", "Later", "Green"]);
    Ok(())
}

#[test]
fn bulk_card_actions() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actor = CollectionActor::open(dir.path())?;
    let n1 = add(&actor, "Basic (and reversed card)", &["one", "1"], &[]);
    let n2 = add(&actor, "Basic", &["two", "2"], &[]);
    let all = cards(&actor, "deck:*");
    assert_eq!(all.len(), 3);
    let EditResponse::Ids(of_notes) = browse(
        &actor,
        BrowseCommand::CardsOfNotes {
            note_ids: vec![n2, n1],
        },
    )?
    else {
        panic!()
    };
    assert_eq!(of_notes.len(), 3);
    assert_eq!(of_notes[0], cards(&actor, &format!("nid:{n2}"))[0]);
    let EditResponse::Ids(of_cards) = browse(
        &actor,
        BrowseCommand::NotesOfCards {
            card_ids: all.iter().rev().copied().chain([123]).collect(),
        },
    )?
    else {
        panic!()
    };
    assert_eq!(of_cards, [n2, n1]);

    // Decks: by name (created when new) and by id.
    assert_eq!(
        counted(
            &actor,
            BrowseCommand::SetDeckByName {
                card_ids: all.clone(),
                name: "Moved::Here".into(),
            },
        ),
        3
    );
    assert_eq!(cards(&actor, "\"deck:Moved::Here\"").len(), 3);
    counted(
        &actor,
        BrowseCommand::SetDeckByName {
            card_ids: vec![all[0]],
            name: "moved::here".into(),
        },
    );
    let EditResponse::DeckNames(decks) = notes(
        &actor,
        NoteCommand::DeckNames {
            include_filtered: false,
        },
    ) else {
        panic!()
    };
    assert_eq!(
        decks
            .iter()
            .filter(|d| d.name.to_lowercase() == "moved::here")
            .count(),
        1
    );
    assert_eq!(
        counted(
            &actor,
            BrowseCommand::SetDeck {
                card_ids: vec![all[0]],
                deck_id: 1
            }
        ),
        1
    );
    assert_eq!(cards(&actor, "deck:Default"), [all[0]]);
    invalid(browse(
        &actor,
        BrowseCommand::SetDeckByName {
            card_ids: vec![all[0]],
            name: "  ".into(),
        },
    ));

    // Restore unsuspends and unburies.
    actor.execute(Command::BuryOrSuspend {
        target: Target::Cards(vec![all[0]]),
        mode: BuryMode::Suspend,
    })?;
    actor.execute(Command::BuryOrSuspend {
        target: Target::Cards(vec![all[1]]),
        mode: BuryMode::Bury,
    })?;
    assert!(
        changed(
            &actor,
            BrowseCommand::Restore {
                card_ids: all[..2].to_vec()
            }
        )
        .card
    );
    assert!(cards(&actor, "is:suspended OR is:buried").is_empty());

    // Reposition (new cards only) and its remembered defaults.
    let EditResponse::RepositionDefaults(d) = browse(&actor, BrowseCommand::RepositionDefaults)?
    else {
        panic!()
    };
    assert_eq!(d, RepositionDefaults::default());
    assert_eq!(
        counted(
            &actor,
            BrowseCommand::Reposition {
                card_ids: vec![all[2]],
                start: 100,
                step: 1,
                randomize: false,
                shift: true,
            },
        ),
        1
    );
    assert_eq!(cards(&actor, "prop:pos=100"), [all[2]]);
    let EditResponse::RepositionDefaults(d) = browse(&actor, BrowseCommand::RepositionDefaults)?
    else {
        panic!()
    };
    assert!(d.shift && !d.random);
    invalid(browse(
        &actor,
        BrowseCommand::Reposition {
            card_ids: vec![all[2]],
            start: -1,
            step: 1,
            randomize: false,
            shift: false,
        },
    ));

    // Set due date with the browser's remembered default.
    assert_eq!(text(&actor, BrowseCommand::SetDueDefault)?, "0");
    assert!(
        changed(
            &actor,
            BrowseCommand::SetDueDate {
                card_ids: vec![all[0]],
                days: "5".into(),
            },
        )
        .card
    );
    assert_eq!(cards(&actor, "prop:due=5"), [all[0]]);
    assert_eq!(text(&actor, BrowseCommand::SetDueDefault)?, "5");
    invalid(browse(
        &actor,
        BrowseCommand::SetDueDate {
            card_ids: vec![all[0]],
            days: "soon".into(),
        },
    ));

    // Grade now, then forget with the browser context's defaults.
    changed(
        &actor,
        BrowseCommand::GradeNow {
            card_ids: vec![all[1]],
            rating: Rating::Easy,
        },
    );
    assert_eq!(cards(&actor, "is:review"), [all[0], all[1]]);
    let EditResponse::ForgetDefaults(f) = browse(&actor, BrowseCommand::ForgetDefaults)? else {
        panic!()
    };
    assert_eq!(
        f,
        ForgetDefaults {
            restore_position: true,
            reset_counts: false
        }
    );
    changed(
        &actor,
        BrowseCommand::Forget {
            card_ids: vec![all[0], all[1]],
            restore_position: false,
            reset_counts: true,
        },
    );
    assert_eq!(cards(&actor, "is:new").len(), 3);
    let EditResponse::ForgetDefaults(f) = browse(&actor, BrowseCommand::ForgetDefaults)? else {
        panic!()
    };
    assert!(!f.restore_position && f.reset_counts);
    let Response::ForgetDefaults(reviewer) = actor.execute(Command::ForgetDefaults)? else {
        panic!()
    };
    assert!(reviewer.restore_position && !reviewer.reset_counts);

    let EditResponse::Strings(fields) = browse(
        &actor,
        BrowseCommand::FieldNamesForNotes {
            note_ids: vec![n1, n2],
        },
    )?
    else {
        panic!()
    };
    assert!(fields.contains(&"Front".to_string()) && fields.contains(&"Back".to_string()));
    Ok(())
}

#[test]
fn find_and_replace_refuses_empty_ids_and_honours_scope() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actor = CollectionActor::open(dir.path())?;
    let a = add(&actor, "Basic", &["Cat cat", "cat"], &["Animal"]);
    let b = add(&actor, "Basic", &["cat", "dog"], &["animal::pet"]);
    let fr = |note_ids: Vec<i64>, search: &str, replacement: &str, scope| FindReplace {
        note_ids,
        search: search.into(),
        replacement: replacement.into(),
        regex: false,
        match_case: false,
        scope,
    };
    // Empty ids would mean every note in Anki: refused, nothing changed.
    let message = invalid(browse(
        &actor,
        BrowseCommand::FindReplace(fr(vec![], "cat", "x", FindReplaceScope::AllFields)),
    ));
    assert!(message.contains("select"));
    invalid(browse(
        &actor,
        BrowseCommand::FindReplace(fr(vec![], "animal", "x", FindReplaceScope::Tags)),
    ));
    assert_eq!(note(&actor, a).fields, ["Cat cat", "cat"]);
    // The child tag adopted the existing parent's case.
    assert_eq!(note(&actor, b).tags, ["Animal::pet"]);

    // One field by name, case-insensitive by default, selected notes only.
    assert_eq!(
        counted(
            &actor,
            BrowseCommand::FindReplace(fr(
                vec![a],
                "cat",
                "$1x",
                FindReplaceScope::Field("Front".into())
            )),
        ),
        1
    );
    assert_eq!(note(&actor, a).fields, ["$1x $1x", "cat"]);
    assert_eq!(note(&actor, b).fields, ["cat", "dog"]);
    // Match case over all fields.
    let mut cased = fr(vec![a, b], "cat", "Kat", FindReplaceScope::AllFields);
    cased.match_case = true;
    assert_eq!(counted(&actor, BrowseCommand::FindReplace(cased)), 2);
    assert_eq!(note(&actor, a).fields, ["$1x $1x", "Kat"]);
    assert_eq!(note(&actor, b).fields, ["Kat", "dog"]);
    // Regex with capture groups.
    let mut regex = fr(vec![b], r"^(\w)og$", "${1}ig", FindReplaceScope::AllFields);
    regex.regex = true;
    assert_eq!(
        counted(&actor, BrowseCommand::FindReplace(regex.clone())),
        1
    );
    assert_eq!(note(&actor, b).fields, ["Kat", "dig"]);
    regex.search = "(".into();
    invalid(browse(&actor, BrowseCommand::FindReplace(regex)));
    // Tags scope.
    assert_eq!(
        counted(
            &actor,
            BrowseCommand::FindReplace(fr(vec![b], "animal", "beast", FindReplaceScope::Tags)),
        ),
        1
    );
    assert_eq!(note(&actor, b).tags, ["beast::pet"]);
    assert_eq!(note(&actor, a).tags, ["Animal"]);
    Ok(())
}
