//! Phase 3 through the app against a real collection actor on a temp
//! collection: add a Basic note on the add screen, then find it in the
//! browser, with the search remembered in tanki's config.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};
use std::path::PathBuf;
use tanki_core::{
    config::Config, BrowseCommand, BrowseMode, CollectionActor, Command, EditResponse, Response,
    SearchOrder,
};
use tanki_tui::{
    app::{App, AppEvent, Layer, Profile, Services, Start},
    keymap::Scope,
    reviewer::Now,
    services::{AnkiRenderer, Display, MediaAudio, NoImages},
};

struct FixtureDir(PathBuf);
impl FixtureDir {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("tanki-phase3-app-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(path.join("profile")).unwrap();
        Self(path)
    }
}
impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn app(actor: &CollectionActor) -> App<'_> {
    let services = Services {
        renderer: Box::new(AnkiRenderer),
        images: Box::new(NoImages),
        audio: Box::new(MediaAudio {
            audio: tanki_media::Audio::start(tanki_media::AudioConfig {
                media_dir: actor.media_dir(),
                backend: tanki_media::BackendChoice::Off,
                ..Default::default()
            }),
        }),
        display: Display::from_capabilities(&tanki_term::Capabilities::default()),
        opener: Box::new(|_| panic!("the flow must not open an external browser")),
    };
    let profile = Profile {
        name: "fixture".into(),
        dir: actor.profile_dir().to_owned(),
        media_dir: actor.media_dir(),
    };
    App::new(actor, services, profile, Start::Decks, Now::current())
}

fn press_with(app: &mut App<'_>, code: KeyCode, modifiers: KeyModifiers) {
    let now = Now::current();
    app.handle(AppEvent::Key(KeyEvent::new(code, modifiers), now.at), now);
}
fn press(app: &mut App<'_>, code: KeyCode) {
    press_with(app, code, KeyModifiers::NONE);
}
fn typed(app: &mut App<'_>, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}
fn screen(app: &mut App<'_>) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let buf = terminal.backend().buffer().clone();
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_owned())
                .collect::<String>()
        })
        .collect()
}

#[test]
fn add_a_basic_note_then_find_it_in_the_browser() {
    let dir = FixtureDir::new();
    let actor = CollectionActor::open(dir.0.join("profile")).unwrap();
    let config = dir.0.join("config.toml");
    let mut app = app(&actor);
    app.set_config(Some(config.clone()));

    // Add: the defaults are Basic in Default.
    press(&mut app, KeyCode::Char('a'));
    assert_eq!(app.scopes(), [Scope::Add]);
    let text = screen(&mut app).join("\n");
    assert!(
        text.contains("Type: Basic") && text.contains("Deck: Default"),
        "{text}"
    );
    typed(&mut app, "Paris");
    press(&mut app, KeyCode::Tab);
    typed(&mut app, "capital of <b>France</b>");
    press_with(&mut app, KeyCode::Enter, KeyModifiers::CONTROL);
    let Response::Edit(EditResponse::Ids(notes)) = actor
        .execute(Command::Browse(BrowseCommand::Search {
            query: "Paris".into(),
            mode: BrowseMode::Notes,
            order: SearchOrder::None,
        }))
        .unwrap()
    else {
        panic!("search")
    };
    assert_eq!(notes.len(), 1, "the note was not added");
    // The form was cleared by the add, so Esc closes at once; the deck list
    // counts the new card.
    press(&mut app, KeyCode::Esc);
    assert!(app.layers.is_empty());
    let text = screen(&mut app).join("\n");
    assert!(text.contains("Default"), "{text}");

    // Browse: search for it and see the row.
    press(&mut app, KeyCode::Char('b'));
    assert!(matches!(app.layers.last(), Some(Layer::Browser(_))));
    press(&mut app, KeyCode::Char('/'));
    press(&mut app, KeyCode::End);
    press_with(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    typed(&mut app, "Paris");
    press(&mut app, KeyCode::Enter);
    let rows = screen(&mut app);
    let text = rows.join("\n");
    assert!(text.contains("Search: Paris"), "{text}");
    assert!(
        rows.iter()
            .any(|r| r.contains("Paris") && r.contains("Default")),
        "{text}"
    );
    assert!(
        text.contains("1 card shown") || text.contains("1 cards shown"),
        "{text}"
    );
    let history = Config::load(&config)
        .unwrap()
        .profile(&actor.profile_dir().to_string_lossy())
        .map(|p| p.search_history.clone())
        .unwrap_or_default();
    assert_eq!(history, ["Paris"]);
    press(&mut app, KeyCode::Char('q'));
    assert!(app.layers.is_empty());
    drop(app);
    actor.close().unwrap();
}
