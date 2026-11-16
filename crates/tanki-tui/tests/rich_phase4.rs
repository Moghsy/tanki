//! Phase 4 exit: real fixture -> core rendering -> native compositor -> cells,
//! plus optional bitmap selection, saved preference and scrolling.
use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};
use std::time::{Duration, Instant};
use tanki_core::{CollectionActor, Command};
#[cfg(feature = "blitz")]
use tanki_tui::app::AppEvent;
use tanki_tui::{
    app::{App, Profile, Screen, Services, Start},
    backend,
    keymap::Action,
    reviewer::Now,
    services::{AnkiRenderer, Display, MediaAudio, TermImages},
};

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
                backend: tanki_media::BackendChoice::Off,
                ..Default::default()
            }),
        }),
        display: Display::from_capabilities(&caps),
        opener: Box::new(|_| panic!("no external browser in fixture tests")),
    };
    (
        App::new(
            actor,
            services,
            Profile {
                name: "fixture".into(),
                dir: actor.profile_dir().into(),
                media_dir: actor.media_dir(),
            },
            Start::Review,
            Now::current(),
        ),
        ready,
    )
}

fn draw(app: &mut App<'_>) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    terminal.backend().buffer().clone()
}

fn has_pixels(buffer: &Buffer) -> bool {
    buffer
        .content()
        .iter()
        .any(|c| matches!(c.symbol(), "▀" | "▄"))
}

fn wait_pixels(app: &mut App<'_>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let buffer = draw(app);
        if has_pixels(&buffer) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected image cells: {:?}",
            buffer
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn overlay(app: &App<'_>) -> String {
    let Screen::Review(view) = &app.screen else {
        panic!("review")
    };
    let card = &view.render.as_ref().unwrap().card;
    assert!(
        !card.notices.iter().any(|n| n.contains("unavailable")),
        "{:?}",
        card.notices
    );
    card.images
        .iter()
        .find_map(|b| match &b.source {
            tanki_render::ImageSource::Occlusion { overlay, .. } => Some(overlay.clone()),
            _ => None,
        })
        .expect("native occlusion bitmap")
}

#[test]
fn both_occlusion_modes_are_reviewable_through_rating_and_undo() {
    let tmp = tempfile::tempdir().unwrap();
    let actor = CollectionActor::open(tmp.path()).unwrap();
    actor.execute(Command::GenerateStudyFixture).unwrap();
    let id = backend::find_deck(&backend::deck_tree(&actor).unwrap(), "Study::Occlusion").unwrap();
    backend::select_deck(&actor, id).unwrap();
    let (mut app, ready) = app(&actor);
    let mut answered = 0;
    for _ in 0..8 {
        if matches!(app.screen, Screen::Congrats(_)) {
            break;
        }
        draw(&mut app);
        let question = overlay(&app);
        assert!(question.contains("#cc3355"));
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        wait_pixels(&mut app);
        app.dispatch(Action::ToggleMasks);
        draw(&mut app);
        assert_ne!(overlay(&app), question);
        app.dispatch(Action::ToggleMasks);
        app.dispatch(Action::ShowAnswerOrGood);
        draw(&mut app);
        let answer = overlay(&app);
        assert_ne!(question, answer);
        app.dispatch(Action::Easy);
        answered += 1;
        if answered == 1 {
            app.dispatch(Action::Undo);
            assert!(matches!(app.screen, Screen::Review(_)));
            draw(&mut app);
            assert_eq!(overlay(&app), question);
            app.dispatch(Action::ShowAnswerOrGood);
            app.dispatch(Action::Easy);
        }
    }
    assert_eq!(answered, 6);
    assert!(matches!(app.screen, Screen::Congrats(_)));
    drop(app);
    actor.close().unwrap();
}

#[cfg(feature = "blitz")]
#[test]
fn bitmap_choice_is_saved_and_a_tall_card_scrolls_as_pixels() {
    let tmp = tempfile::tempdir().unwrap();
    let actor = CollectionActor::open(tmp.path()).unwrap();
    actor.execute(Command::GenerateStudyFixture).unwrap();
    let id = backend::find_deck(&backend::deck_tree(&actor).unwrap(), "Study::Rich").unwrap();
    backend::select_deck(&actor, id).unwrap();
    // Keep only the flex card, and make it taller than the viewport so this
    // exercises slicing rather than merely re-scaling one small bitmap.
    let tanki_core::Response::CardIds(other) = actor
        .execute(Command::Search {
            query: "deck:Study::Rich -tag:phase4::bitmap-css".into(),
        })
        .unwrap()
    else {
        panic!()
    };
    actor
        .execute(Command::BuryOrSuspend {
            target: tanki_core::Target::Cards(other),
            mode: tanki_core::BuryMode::Suspend,
        })
        .unwrap();
    let top = backend::queue(&actor, 1).unwrap().cards.remove(0);
    let tanki_core::Response::Edit(tanki_core::EditResponse::Note(note)) = actor
        .execute(Command::Notes(tanki_core::NoteCommand::GetNote {
            id: top.note_id,
        }))
        .unwrap()
    else {
        panic!()
    };
    actor.execute(Command::Notes(tanki_core::NoteCommand::UpdateNotes { updates:vec![tanki_core::NoteUpdate {
        id:note.id, expected_mtime:note.mtime_secs, expected_fields:Some(note.fields.clone()),
        fields:vec![(0,format!("{}<div style='height:900px;background:#335577'>Tall scrolling bitmap</div>",note.fields[0]))], tags:None,
    }] })).unwrap();
    let (mut app, ready) = app(&actor);
    let config_path = tmp.path().join("tanki.toml");
    app.set_config(Some(config_path.clone()));
    draw(&mut app);
    app.dispatch(Action::CycleRenderTier);
    let Screen::Review(view) = &app.screen else {
        panic!()
    };
    let ntid = view.reviewer.card().unwrap().rendered.notetype_id;
    let config = tanki_core::config::Config::load(&config_path).unwrap();
    assert_eq!(
        config.profiles[&actor.profile_dir().to_string_lossy().to_string()].render_tiers
            [&ntid.to_string()],
        tanki_core::config::RenderTier::Bitmap
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        draw(&mut app);
        let Screen::Review(view) = &app.screen else {
            panic!()
        };
        if view
            .render
            .as_ref()
            .unwrap()
            .card
            .images
            .iter()
            .any(|i| matches!(i.source, tanki_render::ImageSource::Bitmap { .. }))
        {
            break;
        }
        assert!(Instant::now() < deadline, "bitmap worker did not complete");
        std::thread::sleep(Duration::from_millis(10));
    }
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(has_pixels(&draw(&mut app)));
    app.dispatch(Action::PageDown);
    app.handle(AppEvent::Tick, Now::current());
    let Screen::Review(view) = &app.screen else {
        panic!()
    };
    assert!(view.scroll > 0, "tall bitmap must scroll");
    let scrolled = draw(&mut app);
    // Uniform halves are encoded as a blank with an RGB background, which
    // is also an image cell (there need not be a half-block glyph).
    assert!(
        scrolled
            .content()
            .iter()
            .any(|c| c.bg == ratatui::style::Color::Rgb(0x33, 0x55, 0x77)),
        "{scrolled:?}"
    );
    app.dispatch(Action::ShowAnswerOrGood);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let buffer = draw(&mut app);
        let Screen::Review(view) = &app.screen else {
            panic!()
        };
        let answer_bitmap =
            view.reviewer.showing_answer()
                && view.render.as_ref().is_some_and(|render| {
                    render.card.images.iter().any(|image| {
                        matches!(image.source, tanki_render::ImageSource::Bitmap { .. })
                    })
                });
        if answer_bitmap && has_pixels(&buffer) {
            break;
        }
        assert!(Instant::now() < deadline, "answer bitmap did not display");
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(app);
    let (mut restored, _) = self::app(&actor);
    restored.set_config(Some(config_path));
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        draw(&mut restored);
        let Screen::Review(view) = &restored.screen else {
            panic!()
        };
        if view
            .render
            .as_ref()
            .unwrap()
            .card
            .images
            .iter()
            .any(|i| matches!(i.source, tanki_render::ImageSource::Bitmap { .. }))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "saved tier did not apply after reopening"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(restored);
    actor.close().unwrap();
}
