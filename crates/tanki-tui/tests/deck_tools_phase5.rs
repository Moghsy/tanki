//! The deck tools dialogs against a real collection actor on a temp study
//! fixture.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{buffer::Buffer, layout::Rect};
use std::path::Path;
use tanki_core::deck_tools::{
    CustomStudyDefaults, DeckDescription, DeckToolsCommand, DeckToolsResponse, FilteredDeckForm,
};
use tanki_core::{
    BrowseCommand, BrowseMode, CollectionActor, Command, DeckName, EditResponse, ManageCommand,
    ManageResponse, NoteCommand, Response, SearchOrder,
};
use tanki_tui::deck_tools_ui::{
    empty_filtered, rebuild_filtered, CustomStudyUi, DeckDescriptionUi, DeckPromptKind,
    DeckPromptUi, DeckToolsIntent, FilteredDeckUi,
};
use tanki_tui::services::{AnkiRenderer, Display};

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

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn ch(c: char) -> KeyEvent {
    key(KeyCode::Char(c))
}
fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}
const ENTER: KeyCode = KeyCode::Enter;
const DOWN: KeyCode = KeyCode::Down;

fn screen(draw: impl FnOnce(Rect, &mut Buffer)) -> String {
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    draw(area, &mut buf);
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol().to_owned())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn tools(actor: &CollectionActor, command: DeckToolsCommand) -> DeckToolsResponse {
    match actor
        .execute(Command::Manage(ManageCommand::DeckTools(command)))
        .unwrap()
    {
        Response::Manage(ManageResponse::DeckTools(r)) => r,
        other => panic!("unexpected {other:?}"),
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

fn find_deck(actor: &CollectionActor, name: &str) -> Option<i64> {
    deck_names(actor)
        .into_iter()
        .find(|d| d.name == name)
        .map(|d| d.id)
}

fn deck_id(actor: &CollectionActor, name: &str) -> i64 {
    find_deck(actor, name).unwrap_or_else(|| panic!("no deck {name}"))
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

fn current_deck(actor: &CollectionActor) -> i64 {
    match actor.execute(Command::Overview).unwrap() {
        Response::Overview(o) => o.deck_id,
        other => panic!("unexpected {other:?}"),
    }
}

fn form(actor: &CollectionActor, deck_id: i64) -> FilteredDeckForm {
    match tools(
        actor,
        DeckToolsCommand::FilteredDeck {
            deck_id,
            search: None,
            search_2: None,
        },
    ) {
        DeckToolsResponse::FilteredDeck(form) => *form,
        other => panic!("unexpected {other:?}"),
    }
}

fn defaults(actor: &CollectionActor, deck_id: i64) -> CustomStudyDefaults {
    match tools(actor, DeckToolsCommand::CustomStudyDefaults { deck_id }) {
        DeckToolsResponse::CustomStudyDefaults(d) => *d,
        other => panic!("unexpected {other:?}"),
    }
}

fn describe(actor: &CollectionActor, deck_id: i64) -> DeckDescription {
    match tools(actor, DeckToolsCommand::Description { deck_id }) {
        DeckToolsResponse::Description(d) => d,
        other => panic!("unexpected {other:?}"),
    }
}

fn opened(intent: DeckToolsIntent) -> i64 {
    match intent {
        DeckToolsIntent::OpenDeck { deck_id, changes } => {
            assert!(changes.deck && changes.study_queues, "{changes:?}");
            deck_id
        }
        other => panic!("expected OpenDeck, got {other:?}"),
    }
}

fn done(intent: DeckToolsIntent) -> Option<String> {
    match intent {
        DeckToolsIntent::Done { message, .. } => message,
        other => panic!("expected Done, got {other:?}"),
    }
}

#[test]
fn filtered_deck_from_search_builds_edits_rebuilds_and_empties() {
    let f = fixture();
    let actor = &f.actor;
    let later = cards(actor, "tag:study::later");
    assert!(!later.is_empty());

    // The browser's "filtered deck from search".
    let mut ui = FilteredDeckUi::from_search(actor, "tag:study::later").unwrap();
    let text = screen(|area, buf| ui.render(area, buf));
    for expected in [
        "Options for Filtered Deck",
        "tag:study::later",
        "cards selected by",
        "Enable second filter",
        "[x] Reschedule cards based on my answers in this deck",
        "Enter Build",
    ] {
        assert!(text.contains(expected), "{expected}\n{text}");
    }
    assert!(!text.contains("Delay for Again"), "{text}");
    let deck = opened(ui.on_key(actor, key(ENTER)));
    assert_eq!(current_deck(actor), deck);
    let held = cards(actor, &format!("did:{deck}"));
    assert_eq!(held.len(), later.len());
    assert!(held.iter().all(|id| later.contains(id)));

    // Edit it: reschedule off shows the preview delays; change one.
    let mut ui = FilteredDeckUi::edit(actor, deck).unwrap();
    assert_eq!(ui.deck_id(), deck);
    for code in [DOWN, DOWN, DOWN, DOWN] {
        assert_eq!(ui.on_key(actor, key(code)), DeckToolsIntent::None);
    }
    ui.on_key(actor, ch(' '));
    ui.on_key(actor, key(DOWN));
    ui.on_key(actor, ctrl('u'));
    ui.on_key(actor, ch('5'));
    let text = screen(|area, buf| ui.render(area, buf));
    for expected in [
        "[ ] Reschedule cards based on my answers in this deck",
        "Delay for Again",
        "Delay for Good",
        "seconds",
        "(0 = return card to original deck)",
        "Enter Rebuild",
    ] {
        assert!(text.contains(expected), "{expected}\n{text}");
    }
    assert_eq!(opened(ui.on_key(actor, ctrl('s'))), deck);
    let saved = form(actor, deck).deck;
    assert!(!saved.reschedule);
    assert_eq!(
        (saved.preview_again_secs, saved.preview_hard_secs),
        (5, 600)
    );
    assert_eq!(saved.first.search, "tag:study::later");

    // The overview's empty and rebuild.
    assert!(empty_filtered(actor, deck).unwrap().study_queues);
    assert!(cards(actor, &format!("did:{deck}")).is_empty());
    assert!(rebuild_filtered(actor, deck).unwrap().study_queues);
    assert_eq!(cards(actor, &format!("did:{deck}")).len(), later.len());

    // Anki's errors keep the form open and change nothing.
    let decks = deck_names(actor).len();
    let mut ui = FilteredDeckUi::new(actor).unwrap();
    ui.on_key(actor, ctrl('u'));
    for c in "tag:no-such-tag".chars() {
        ui.on_key(actor, ch(c));
    }
    match ui.on_key(actor, key(ENTER)) {
        DeckToolsIntent::Message(message) => assert!(!message.is_empty()),
        other => panic!("expected Message, got {other:?}"),
    }
    assert_eq!(deck_names(actor).len(), decks);
    assert_eq!(ui.on_key(actor, key(KeyCode::Esc)), DeckToolsIntent::Close);
}

#[test]
fn custom_study_extends_the_limit_and_builds_a_tagged_session() {
    let f = fixture();
    let actor = &f.actor;
    let study = deck_id(actor, "Study");

    let mut ui = CustomStudyUi::open(actor, study).unwrap();
    let text = screen(|area, buf| ui.render(area, buf));
    for expected in [
        "Custom Study: Study",
        "(•) Increase today's new card limit",
        "( ) Increase today's review card limit",
        "( ) Review forgotten cards",
        "( ) Review ahead",
        "( ) Preview new cards",
        "( ) Study by card state or tag",
        "Available new cards:",
    ] {
        assert!(text.contains(expected), "{expected}\n{text}");
    }
    // Seven more new cards today; Anki remembers the amount.
    ui.on_key(actor, key(KeyCode::Backspace));
    ui.on_key(actor, ch('7'));
    assert_eq!(done(ui.on_key(actor, key(ENTER))), None);
    assert_eq!(defaults(actor, study).extend_new, 7);

    // Study by card state or tag: all cards, requiring one tag.
    let d = defaults(actor, study);
    let tag = d
        .tags
        .iter()
        .position(|t| t.name == "study::later")
        .expect("the fixture tag");
    let mut ui = CustomStudyUi::open(actor, study).unwrap();
    ui.on_key(actor, ch('k'));
    for _ in 0..3 {
        ui.on_key(actor, key(KeyCode::Right));
    }
    let text = screen(|area, buf| ui.render(area, buf));
    assert!(text.contains("Select 100"), "{text}");
    assert!(text.contains("All cards in random order"), "{text}");
    assert_eq!(ui.on_key(actor, key(ENTER)), DeckToolsIntent::None);
    let text = screen(|area, buf| ui.render(area, buf));
    assert!(text.contains("Selective Study"), "{text}");
    for _ in 0..tag {
        ui.on_key(actor, ch('j'));
    }
    ui.on_key(actor, ch('i'));
    let text = screen(|area, buf| ui.render(area, buf));
    assert!(text.contains("> [+] study::later"), "{text}");
    let session = opened(ui.on_key(actor, key(ENTER)));
    assert_eq!(find_deck(actor, "Custom Study Session"), Some(session));
    assert_eq!(current_deck(actor), session);
    let held = cards(actor, &format!("did:{session}"));
    let later = cards(actor, "tag:study::later");
    assert!(!held.is_empty());
    assert!(held.iter().all(|id| later.contains(id)), "{held:?}");
    // The choice is remembered for the next time.
    let remembered = defaults(actor, study);
    assert!(remembered
        .tags
        .iter()
        .all(|t| t.include == (t.name == "study::later") && !t.exclude));

    // A filtered deck has no custom study.
    assert!(CustomStudyUi::open(actor, session).is_err());
}

#[test]
fn deck_description_is_rendered_and_edited_with_the_markdown_flag() {
    let f = fixture();
    let actor = &f.actor;
    let study = deck_id(actor, "Study");
    let display = Display {
        cell_px: None,
        fg: None,
        bg: None,
        night_mode: true,
    };
    let draw = |ui: &DeckDescriptionUi| {
        screen(|area, buf| ui.render(area, buf, &AnkiRenderer, &display, Path::new("/tmp")))
    };

    let mut ui = DeckDescriptionUi::open(actor, study).unwrap();
    let text = draw(&ui);
    for expected in [
        "Description: Study",
        "Welcome to the tanki study fixture.",
        "new, learning and review cards",
        "e edit",
    ] {
        assert!(text.contains(expected), "{expected}\n{text}");
    }
    assert!(!text.contains("**"), "{text}");

    // The editor shows the stored Markdown and the flag.
    assert_eq!(ui.on_key(actor, ch('e')), DeckToolsIntent::None);
    let text = draw(&ui);
    assert!(text.contains("[x] Interpret as Markdown"), "{text}");
    assert!(
        text.contains("Welcome to the **tanki** study fixture."),
        "{text}"
    );
    ui.on_key(actor, ctrl('t'));
    for c in "<u>New</u> ".chars() {
        ui.on_key(actor, ch(c));
    }
    assert!(draw(&ui).contains("[ ] Interpret as Markdown"));
    match ui.on_key(actor, ctrl('s')) {
        DeckToolsIntent::Changed(changes) => assert!(changes.deck, "{changes:?}"),
        other => panic!("expected Changed, got {other:?}"),
    }
    let saved = describe(actor, study);
    assert!(!saved.markdown);
    assert!(saved
        .text
        .starts_with("<u>New</u> Welcome to the **tanki** study fixture."));
    // Back on the view: HTML now, so the stars show as typed.
    assert!(!ui.is_editing());
    let text = draw(&ui);
    assert!(
        text.contains("New Welcome to the **tanki** study fixture."),
        "{text}"
    );

    // Straight into the editor: Markdown on again.
    let mut ui = DeckDescriptionUi::edit(actor, study).unwrap();
    assert!(ui.is_editing());
    ui.on_key(actor, ctrl('t'));
    assert_eq!(done(ui.on_key(actor, ctrl('s'))), None);
    let saved = describe(actor, study);
    assert!(saved.markdown);
    assert!(
        saved.html.contains("<strong>tanki</strong>"),
        "{}",
        saved.html
    );

    // A filtered deck has no description.
    let cram = deck_id(actor, "Study Cram");
    assert!(DeckDescriptionUi::open(actor, cram).is_err());
}

#[test]
fn deck_prompts_add_rename_move_and_delete() {
    let f = fixture();
    let actor = &f.actor;
    let draw = |ui: &DeckPromptUi| screen(|area, buf| ui.render(area, buf));
    let type_text = |ui: &mut DeckPromptUi, text: &str| {
        for c in text.chars() {
            assert_eq!(ui.on_key(actor, ch(c)), DeckToolsIntent::None);
        }
    };

    // Add, with parents made on the way.
    let mut ui = DeckPromptUi::add(actor, "").unwrap();
    let text = draw(&ui);
    assert!(
        text.contains("Create Deck") && text.contains("New deck name:"),
        "{text}"
    );
    type_text(&mut ui, "Tools::Leaf");
    assert_eq!(done(ui.on_key(actor, key(ENTER))), None);
    let leaf = deck_id(actor, "Tools::Leaf");

    // Rename with the full name.
    let mut ui = DeckPromptUi::rename(actor, leaf).unwrap();
    let text = draw(&ui);
    assert!(
        text.contains("Rename Deck: Tools::Leaf") && text.contains("New name:"),
        "{text}"
    );
    for _ in "Leaf".chars() {
        ui.on_key(actor, key(KeyCode::Backspace));
    }
    type_text(&mut ui, "Twig");
    assert_eq!(done(ui.on_key(actor, key(ENTER))), None);
    assert_eq!(deck_id(actor, "Tools::Twig"), leaf);
    // The unchanged name closes without a command.
    let mut ui = DeckPromptUi::rename(actor, leaf).unwrap();
    assert_eq!(ui.on_key(actor, key(ENTER)), DeckToolsIntent::Close);
    assert_eq!(deck_id(actor, "Tools::Twig"), leaf);

    // Move to…: normal decks only, without the deck's own parent.
    let mut ui = DeckPromptUi::move_to(actor, leaf).unwrap();
    assert_eq!(ui.kind(), DeckPromptKind::Move);
    let text = draw(&ui);
    assert!(text.contains("Move Tools::Twig to…"), "{text}");
    assert!(
        text.contains("(top level)") && text.contains("Default"),
        "{text}"
    );
    type_text(&mut ui, "cram");
    assert!(draw(&ui).contains("No deck matches."));
    ui.on_key(actor, ctrl('u'));
    type_text(&mut ui, "study::lat");
    let text = draw(&ui);
    assert!(text.contains("> Study::Later"), "{text}");
    assert_eq!(
        done(ui.on_key(actor, key(ENTER))).as_deref(),
        Some("Moved Twig to Study::Later.")
    );
    assert_eq!(deck_id(actor, "Study::Later::Twig"), leaf);
    // And back to the top level, the first row for a subdeck.
    let mut ui = DeckPromptUi::move_to(actor, leaf).unwrap();
    assert_eq!(
        done(ui.on_key(actor, key(ENTER))).as_deref(),
        Some("Moved Twig to the top level.")
    );
    assert_eq!(deck_id(actor, "Twig"), leaf);

    // Delete asks first and reports Anki's count.
    let later = deck_id(actor, "Study::Later");
    let held = cards(actor, "deck:Study::Later").len();
    assert!(held >= 1);
    let mut ui = DeckPromptUi::delete(actor, later).unwrap();
    let text = draw(&ui);
    assert!(
        text.contains("Delete Deck") && text.contains("Study::Later"),
        "{text}"
    );
    assert!(text.contains("will be deleted."), "{text}");
    assert_eq!(ui.on_key(actor, key(ENTER)), DeckToolsIntent::None);
    assert!(find_deck(actor, "Study::Later").is_some());
    let message = done(ui.on_key(actor, ch('y'))).expect("Anki's message");
    assert!(message.starts_with(&format!("{held} card")), "{message}");
    assert!(find_deck(actor, "Study::Later").is_none());
    assert!(cards(actor, "tag:study::later").is_empty());

    // A filtered deck's cards go home.
    let cram = deck_id(actor, "Study Cram");
    let ui = DeckPromptUi::delete(actor, cram).unwrap();
    assert!(draw(&ui).contains("will return to their home decks."));
}
