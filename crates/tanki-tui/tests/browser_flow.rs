//! The browser widget against a real collection actor on the study fixture.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{buffer::Buffer, layout::Rect};
use std::path::PathBuf;
use tanki_core::{
    BrowseCommand, BrowseMode, CollectionActor, Command, EditResponse, Response, SearchOrder,
};
use tanki_tui::browser_ui::{BrowserIntent, BrowserOptions, BrowserUi};
use tanki_tui::services::AnkiRenderer;

struct FixtureDir(PathBuf);
impl FixtureDir {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("tanki-browser-flow-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 140,
    height: 30,
};

fn draw(ui: &mut BrowserUi, actor: &CollectionActor) -> Vec<String> {
    ui.prepare(actor, AREA);
    let mut buf = Buffer::empty(AREA);
    ui.render(AREA, &mut buf, &AnkiRenderer);
    (0..AREA.height)
        .map(|y| {
            (0..AREA.width)
                .map(|x| buf[(x, y)].symbol().to_owned())
                .collect::<String>()
        })
        .collect()
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn press(ui: &mut BrowserUi, actor: &CollectionActor, keys: &str) -> BrowserIntent {
    let mut last = BrowserIntent::None;
    for c in keys.chars() {
        last = ui.on_key(actor, key(KeyCode::Char(c)));
        ui.prepare(actor, AREA);
    }
    last
}

fn count(actor: &CollectionActor, query: &str, mode: BrowseMode) -> usize {
    let Response::Edit(EditResponse::Ids(ids)) = actor
        .execute(Command::Browse(BrowseCommand::Search {
            query: query.into(),
            mode,
            order: SearchOrder::None,
        }))
        .unwrap()
    else {
        panic!()
    };
    ids.len()
}

#[test]
fn browse_search_act_and_switch_modes_on_a_real_collection() {
    let dir = FixtureDir::new();
    let actor = CollectionActor::open(&dir.0).unwrap();
    actor.execute(Command::GenerateStudyFixture).unwrap();
    let mut ui = BrowserUi::open(
        &actor,
        BrowserOptions {
            media_dir: actor.media_dir(),
            search: Some("deck:Study".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let total = count(&actor, "deck:Study", BrowseMode::Cards);
    assert_eq!(ui.ids().len(), total);
    let text = draw(&mut ui, &actor);
    assert!(text[0].starts_with("Search: deck:Study"), "{text:?}");
    assert!(text[1].contains("Sort Field") && text[1].contains("Due"));
    assert!(
        text.iter().any(|l| l.contains("Welcome to tanki")),
        "{text:?}"
    );
    // Deck names elide in the middle, as Anki asks.
    assert!(
        text.iter().any(|l| l.contains("Study::V…:French")),
        "{text:#?}"
    );
    assert!(text.iter().any(|l| l.starts_with("▾ Decks")), "sidebar");

    // Search through the bar, with Anki normalising the text.
    ui.on_key(&actor, key(KeyCode::Char('/')));
    ui.on_key(
        &actor,
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
    );
    // The last term is complete, so no completion popup takes the Enter.
    for c in "deck:Study   tag:study::again".chars() {
        ui.on_key(&actor, key(KeyCode::Char(c)));
    }
    let intent = ui.on_key(&actor, key(KeyCode::Enter));
    let BrowserIntent::SearchCommitted(query) = intent else {
        panic!("{intent:?} {:?}", ui.message())
    };
    assert_eq!(query, "deck:Study tag:study::again");
    let again = count(&actor, &query, BrowseMode::Cards);
    assert!(again >= 2);
    assert_eq!(ui.ids().len(), again);

    // Select all and flag them orange through the real actor.
    let orange = format!("{query} flag:2");
    let before = count(&actor, &orange, BrowseMode::Cards);
    ui.on_key(
        &actor,
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
    );
    press(&mut ui, &actor, "2");
    assert_eq!(count(&actor, &orange, BrowseMode::Cards), again);
    let text = draw(&mut ui, &actor);
    assert!(text.iter().any(|l| l.contains('⚑')), "{text:?}");
    // Undo through the widget.
    press(&mut ui, &actor, "u");
    assert_eq!(count(&actor, &orange, BrowseMode::Cards), before);
    assert!(
        ui.message().unwrap().starts_with("Undone"),
        "{:?}",
        ui.message()
    );

    // Notes mode is stored under desktop's key and the table follows.
    press(&mut ui, &actor, "M");
    assert_eq!(ui.mode(), BrowseMode::Notes);
    assert_eq!(ui.ids().len(), count(&actor, &query, BrowseMode::Notes));
    let Response::Edit(EditResponse::Settings(settings)) = actor
        .execute(Command::Browse(BrowseCommand::Settings))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(settings.mode, BrowseMode::Notes);
    let text = draw(&mut ui, &actor);
    assert!(text[0].trim_end().ends_with("[Notes]"));
    assert!(text[1].contains("Tags"), "{text:?}");
    // The preview renders the note's first card with the real renderer.
    press(&mut ui, &actor, "p");
    let text = draw(&mut ui, &actor);
    assert!(text.iter().any(|l| l.contains("Preview: question")));
    // Edit hands the note ids to the app.
    let intent = ui.on_key(&actor, key(KeyCode::Enter));
    assert!(matches!(intent, BrowserIntent::EditNotes(ref ids) if ids.len() == 1));
    press(&mut ui, &actor, "M");
    assert_eq!(ui.mode(), BrowseMode::Cards);
    drop(ui);
    actor.close().unwrap();
}
