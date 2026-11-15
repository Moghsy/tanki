//! Phase 5 through the app against a real collection actor on a temp
//! fixture: each management screen opens from its key as a layer, draws, and
//! closes back to the screen below.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};
use std::path::PathBuf;
use tanki_core::{CollectionActor, Command};
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
            std::env::temp_dir().join(format!("tanki-phase5-app-{}-{stamp}", std::process::id()));
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

fn fixture() -> (FixtureDir, CollectionActor) {
    let dir = FixtureDir::new();
    let actor = CollectionActor::open(dir.0.join("profile")).unwrap();
    actor.execute(Command::GenerateStudyFixture).unwrap();
    (dir, actor)
}

#[test]
fn deck_options_open_from_the_deck_list_and_close() {
    let (_dir, actor) = fixture();
    let mut app = app(&actor);
    screen(&mut app);
    press(&mut app, KeyCode::Char('o'));
    assert!(
        matches!(app.layers.last(), Some(Layer::DeckOptions(_))),
        "{}",
        screen(&mut app).join("\n")
    );
    assert_eq!(app.scopes(), [Scope::Manage]);
    let text = screen(&mut app).join("\n");
    assert!(text.contains("Daily Limits"), "{text}");
    press(&mut app, KeyCode::Esc);
    assert!(app.layers.is_empty(), "{}", screen(&mut app).join("\n"));
}

#[test]
fn note_types_open_from_the_deck_list_and_close() {
    let (_dir, actor) = fixture();
    let mut app = app(&actor);
    press_with(&mut app, KeyCode::Char('N'), KeyModifiers::SHIFT);
    assert!(matches!(app.layers.last(), Some(Layer::Notetypes(_))));
    let text = screen(&mut app).join("\n");
    assert!(text.contains("Basic") && text.contains("Cloze"), "{text}");
    press(&mut app, KeyCode::Esc);
    assert!(app.layers.is_empty(), "{}", screen(&mut app).join("\n"));
}

#[test]
fn change_note_type_opens_from_the_browser() {
    let (_dir, actor) = fixture();
    let mut app = app(&actor);
    press(&mut app, KeyCode::Char('b'));
    screen(&mut app);
    press_with(&mut app, KeyCode::Char('N'), KeyModifiers::SHIFT);
    assert!(
        matches!(app.layers.last(), Some(Layer::ChangeNotetype(_))),
        "{}",
        screen(&mut app).join("\n")
    );
    screen(&mut app);
    press(&mut app, KeyCode::Esc);
    // Esc may first close the type picker; a second one leaves the form.
    if matches!(app.layers.last(), Some(Layer::ChangeNotetype(_))) {
        press(&mut app, KeyCode::Esc);
    }
    assert!(matches!(app.layers.last(), Some(Layer::Browser(_))));
}

fn top_is_dialog(app: &App<'_>) -> bool {
    matches!(app.layers.last(), Some(Layer::DeckTools(_)))
}

#[test]
fn create_a_deck_from_the_deck_list() {
    let (_dir, actor) = fixture();
    let mut app = app(&actor);
    screen(&mut app);
    press(&mut app, KeyCode::Char('n'));
    assert!(top_is_dialog(&app), "{}", screen(&mut app).join("\n"));
    // The deck list stays visible under the dialog.
    let text = screen(&mut app).join("\n");
    assert!(text.contains("Study"), "{text}");
    typed(&mut app, "Phase5Made");
    press(&mut app, KeyCode::Enter);
    assert!(app.layers.is_empty(), "{}", screen(&mut app).join("\n"));
    let tree = tanki_tui::backend::deck_tree(&actor).unwrap();
    assert!(tanki_tui::backend::find_deck(&tree, "Phase5Made").is_some());
}

#[test]
fn deck_tools_dialogs_open_from_their_keys() {
    let (_dir, actor) = fixture();
    let mut app = app(&actor);
    screen(&mut app);
    // Tools > Create Filtered Deck.
    press(&mut app, KeyCode::Char('f'));
    assert!(top_is_dialog(&app));
    screen(&mut app);
    press(&mut app, KeyCode::Esc);
    assert!(app.layers.is_empty());
    // Overview: custom study and the description.
    press(&mut app, KeyCode::Enter);
    for key in ['c', 'i'] {
        press(&mut app, KeyCode::Char(key));
        assert!(
            top_is_dialog(&app),
            "{key}: {}",
            screen(&mut app).join("\n")
        );
        screen(&mut app);
        press(&mut app, KeyCode::Esc);
        assert!(
            app.layers.is_empty(),
            "{key}: {}",
            screen(&mut app).join("\n")
        );
    }
    // The browser's search as a filtered deck.
    press(&mut app, KeyCode::Char('b'));
    screen(&mut app);
    press_with(&mut app, KeyCode::Char('F'), KeyModifiers::SHIFT);
    assert!(top_is_dialog(&app), "{}", screen(&mut app).join("\n"));
    assert_eq!(app.layers.len(), 2);
    screen(&mut app);
}

#[test]
fn statistics_and_card_info_open_and_close() {
    let (_dir, actor) = fixture();
    let mut app = app(&actor);
    screen(&mut app);
    press(&mut app, KeyCode::Char('t'));
    assert!(
        matches!(app.layers.last(), Some(Layer::Stats(_))),
        "{}",
        screen(&mut app).join("\n")
    );
    screen(&mut app);
    press(&mut app, KeyCode::Esc);
    assert!(app.layers.is_empty(), "{}", screen(&mut app).join("\n"));
    // Card info on the browser's current row.
    press(&mut app, KeyCode::Char('b'));
    screen(&mut app);
    press(&mut app, KeyCode::Char('i'));
    assert!(
        matches!(app.layers.last(), Some(Layer::CardInfo(_))),
        "{}",
        screen(&mut app).join("\n")
    );
    screen(&mut app);
    press(&mut app, KeyCode::Esc);
    assert!(matches!(app.layers.last(), Some(Layer::Browser(_))));
}

#[test]
fn tools_screen_opens_and_asks_to_switch_profiles() {
    let (dir, actor) = fixture();
    // A second profile beside the open one.
    std::fs::create_dir(dir.0.join("other")).unwrap();
    CollectionActor::open(dir.0.join("other"))
        .unwrap()
        .close()
        .unwrap();
    let mut app = app(&actor);
    app.set_base_dir(Some(dir.0.clone()));
    press_with(&mut app, KeyCode::Char('T'), KeyModifiers::SHIFT);
    assert!(
        matches!(app.layers.last(), Some(Layer::Tools(_))),
        "{}",
        screen(&mut app).join("\n")
    );
    screen(&mut app);
    press(&mut app, KeyCode::Char('5'));
    let text = screen(&mut app).join("\n");
    assert!(text.contains("other") && text.contains("profile"), "{text}");
    // "other" sorts first.
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Enter);
    assert!(app.should_quit(), "{}", screen(&mut app).join("\n"));
    assert_eq!(app.take_profile_switch().as_deref(), Some("other"));
}

fn top_is_transfer(app: &App<'_>) -> bool {
    matches!(app.layers.last(), Some(Layer::Transfer(_)))
}

#[test]
fn text_import_and_export_go_by_extension_and_the_browser_finds_duplicates() {
    let (dir, actor) = fixture();
    let mut app = app(&actor);
    screen(&mut app);
    // A CSV file from the import prompt opens the field-mapping form.
    let csv = dir.0.join("notes.csv");
    std::fs::write(&csv, "front one,back one\nfront two,back two\n").unwrap();
    press_with(&mut app, KeyCode::Char('I'), KeyModifiers::SHIFT);
    typed(&mut app, &csv.to_string_lossy());
    press(&mut app, KeyCode::Enter);
    assert!(top_is_transfer(&app), "{}", screen(&mut app).join("\n"));
    screen(&mut app);
    press(&mut app, KeyCode::Esc);
    assert!(app.layers.is_empty(), "{}", screen(&mut app).join("\n"));
    // A .txt name from the export prompt opens the text export form.
    press_with(&mut app, KeyCode::Char('E'), KeyModifiers::SHIFT);
    typed(&mut app, &dir.0.join("out.txt").to_string_lossy());
    press(&mut app, KeyCode::Enter);
    assert!(top_is_transfer(&app), "{}", screen(&mut app).join("\n"));
    screen(&mut app);
    press(&mut app, KeyCode::Esc);
    assert!(app.layers.is_empty(), "{}", screen(&mut app).join("\n"));
    // Browser: find duplicates and export the rows as text.
    press(&mut app, KeyCode::Char('b'));
    screen(&mut app);
    for key in ['X', 'E'] {
        press_with(&mut app, KeyCode::Char(key), KeyModifiers::SHIFT);
        assert!(
            top_is_transfer(&app),
            "{key}: {}",
            screen(&mut app).join("\n")
        );
        screen(&mut app);
        press(&mut app, KeyCode::Esc);
        assert!(
            matches!(app.layers.last(), Some(Layer::Browser(_))),
            "{key}: {}",
            screen(&mut app).join("\n")
        );
    }
}
