//! Verify the stream boundaries together, without a terminal or audio device.
use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};
use std::{path::PathBuf, time::Duration};
use tanki_core::{CollectionActor, Command, Response};
use tanki_tui::{
    app::{App, AppEvent, Profile, Screen, Services, Start},
    backend,
    keymap::Action,
    reviewer::{Now, Phase},
    services::{AnkiRenderer, Display, MediaAudio, TermImages},
};

struct FixtureDir(PathBuf);
impl FixtureDir {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("tanki-tui-flow-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn app(actor: &CollectionActor) -> (App<'_>, std::sync::mpsc::Receiver<()>) {
    let caps = tanki_term::Capabilities {
        graphics: tanki_term::GraphicsMode::Blocks,
        detected: tanki_term::GraphicsMode::Blocks,
        cell_px: Some((10, 20)),
        ..Default::default()
    };
    let images = TermImages::new(
        tanki_term::Images::new(&caps),
        tanki_render::ImageCache::new(),
        caps.cell_px,
    );
    let ready = images.subscribe();
    let services = Services {
        renderer: Box::new(AnkiRenderer),
        images: Box::new(images),
        audio: Box::new(MediaAudio {
            audio: tanki_media::Audio::start(tanki_media::AudioConfig {
                media_dir: actor.media_dir(),
                backend: tanki_media::BackendChoice::Off,
                ..Default::default()
            }),
        }),
        display: Display::from_capabilities(&caps),
        opener: Box::new(|_| panic!("fixture flow must not open an external browser")),
    };
    let profile = Profile {
        name: "fixture".into(),
        dir: actor.profile_dir().to_owned(),
        media_dir: actor.media_dir(),
    };
    (
        App::new(actor, services, profile, Start::Review, Now::current()),
        ready,
    )
}

fn draw(app: &mut App<'_>) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    terminal.backend().buffer().clone()
}

#[test]
fn real_collection_renderer_and_images_finish_the_media_deck() {
    let dir = FixtureDir::new();
    let actor = CollectionActor::open(&dir.0).unwrap();
    actor.execute(Command::GenerateStudyFixture).unwrap();
    let tree = backend::deck_tree(&actor).unwrap();
    let id = backend::find_deck(&tree, "Study::Media").unwrap();
    backend::select_deck(&actor, id).unwrap();
    let (mut app, ready) = app(&actor);
    let mut answers = 0;
    let mut saw_pixels = false;
    for _ in 0..20 {
        if matches!(app.screen, Screen::Congrats(_)) {
            break;
        }
        draw(&mut app);
        let Screen::Review(view) = &app.screen else {
            panic!("expected review")
        };
        assert_eq!(view.reviewer.phase(), Phase::Question);
        assert!(view.reviewer.card().is_some());
        let has_image = !view.render.as_ref().unwrap().card.images.is_empty();
        if has_image && !saw_pixels {
            ready.recv_timeout(Duration::from_secs(5)).unwrap();
            let buffer = draw(&mut app);
            saw_pixels = buffer
                .content()
                .iter()
                .any(|c| matches!(c.symbol(), "▀" | "▄"));
            assert!(
                saw_pixels,
                "real decoded image should reach the cell buffer"
            );
        }
        // Rating a question is ignored; revealing and then rating advances it.
        app.dispatch(Action::Easy);
        let Screen::Review(view) = &app.screen else {
            panic!("rating question left review")
        };
        assert_eq!(view.reviewer.phase(), Phase::Question);
        app.dispatch(Action::ShowAnswerOrGood);
        draw(&mut app);
        let Screen::Review(view) = &app.screen else {
            panic!("expected answer")
        };
        assert_eq!(view.reviewer.phase(), Phase::Answer);
        app.dispatch(Action::Easy);
        answers += 1;
    }
    assert!(saw_pixels);
    assert!(answers >= 3);
    assert!(matches!(app.screen, Screen::Congrats(_)));
    assert!(backend::queue(&actor, 1).unwrap().cards.is_empty());
    app.shutdown();
    drop(app);
    actor.close().unwrap();
}

#[test]
fn real_typed_answer_diff_answer_and_undo() {
    let dir = FixtureDir::new();
    let actor = CollectionActor::open(&dir.0).unwrap();
    assert!(matches!(
        actor
            .execute(Command::AddNote {
                notetype: "Basic (type in the answer)".into(),
                fields: vec!["Capital of France?".into(), "Paris".into()],
            })
            .unwrap(),
        Response::NoteAdded { .. }
    ));
    let (mut app, _) = app(&actor);
    draw(&mut app);
    let Screen::Review(view) = &app.screen else {
        panic!("expected review")
    };
    assert!(view
        .render
        .as_ref()
        .unwrap()
        .card
        .widgets
        .iter()
        .any(|widget| matches!(widget.kind, tanki_render::WidgetKind::TypeAnswer)));
    app.handle(AppEvent::Paste("Paris".into()), Now::current());
    app.dispatch(Action::ShowAnswerOrGood);
    let buffer = draw(&mut app);
    assert!(buffer
        .content()
        .iter()
        .any(|c| c.bg == ratatui::style::Color::Rgb(0xaa, 0xff, 0xaa)));
    app.dispatch(Action::Easy);
    assert!(matches!(app.screen, Screen::Congrats(_)));
    app.dispatch(Action::Undo);
    assert!(!backend::queue(&actor, 1).unwrap().cards.is_empty());
    app.shutdown();
    drop(app);
    actor.close().unwrap();
}
