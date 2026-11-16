use super::*;
use crate::services::{NoImages, PlainTextRenderer};
use crate::testing::{buffer_text, card, t0, FakeAudio, FakeBackend};
use ratatui::{backend::TestBackend, Terminal};
use tanki_core::{Command, CoreError, TypeAnswer};

fn app(b: &FakeBackend, start: Start, now: Now) -> App<'_> {
    App::new(
        b,
        Services {
            renderer: Box::new(PlainTextRenderer),
            images: Box::new(NoImages),
            audio: Box::new(FakeAudio::default()),
            display: Display {
                cell_px: Some((10, 20)),
                fg: None,
                bg: None,
                night_mode: true,
            },
            opener: Box::new(|_| Ok(())),
        },
        Profile {
            name: "fixture".into(),
            dir: "/tmp/tanki-test-profile".into(),
            media_dir: "/tmp/tanki-test-profile/collection.media".into(),
        },
        start,
        now,
    )
}
fn key(a: &mut App<'_>, code: KeyCode, now: Now) {
    a.handle(
        AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE), now.at),
        now,
    );
}
fn rendered(a: &mut App<'_>, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| a.draw(f)).unwrap();
    terminal.backend().buffer().clone()
}
fn snapshot(name: &str, a: &mut App<'_>) {
    let buffer = rendered(a, 80, 24);
    let rows = buffer_text(&buffer).join("\n");
    // Include every cell's style, including blank cells: regressions in zero
    // counts, queue underlines, diff colours and selected rows are observable.
    let styled = format!("{rows}\n\n{buffer:?}\n");
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("{name}.snap"));
    if std::env::var_os("TANKI_UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &styled).unwrap();
    }
    assert_eq!(styled, std::fs::read_to_string(path).unwrap());
}

#[test]
fn screens_and_overlays_cell_snapshots() {
    let b = FakeBackend::new(vec![card(
        1,
        "<b>Question</b>",
        "Question<hr id=answer><i>Answer</i>",
    )]);
    let now = t0();
    let mut a = app(&b, Start::Decks, now);
    snapshot("decks", &mut a);
    a.open_overview(Some(1));
    snapshot("overview", &mut a);
    a.start_review();
    snapshot("question", &mut a);
    a.dispatch(Action::ShowAnswerOrGood);
    snapshot("answer", &mut a);
    a.dispatch(Action::Help);
    snapshot("help", &mut a);
    key(&mut a, KeyCode::Esc, now);
    a.dispatch(Action::Palette);
    a.handle(AppEvent::Paste("bury".into()), now);
    snapshot("palette", &mut a);
    a.overlay = None;
    a.show_congrats();
    snapshot("congrats", &mut a);
    a.dispatch(Action::About);
    snapshot("about", &mut a);
}

#[test]
fn slash_fuzzy_filters_decks_including_collapsed_children() {
    let b = FakeBackend::new(vec![card(1, "Q", "A")]);
    let now = t0();
    let mut a = app(&b, Start::Decks, now);
    key(&mut a, KeyCode::Char('/'), now);
    for c in "lja".chars() {
        key(&mut a, KeyCode::Char(c), now);
    }
    let Screen::Decks(v) = &a.screen else {
        panic!()
    };
    assert_eq!(v.rows.len(), 1);
    assert_eq!(v.rows[0].full_name, "Languages::Japanese");
    key(&mut a, KeyCode::Enter, now);
    let Screen::Decks(v) = &a.screen else {
        panic!()
    };
    assert!(v.filter.as_ref().is_some_and(|f| !f.editing));
    key(&mut a, KeyCode::Esc, now);
    let Screen::Decks(v) = &a.screen else {
        panic!()
    };
    assert!(v.filter.is_none());
    assert_eq!(v.rows.len(), 4);
}

#[test]
fn deck_movement_collapse_open_and_scope_specific_study() {
    let b = FakeBackend::new(vec![card(1, "Q", "A")]);
    let now = t0();
    let mut a = app(&b, Start::Decks, now);
    key(&mut a, KeyCode::Char('j'), now);
    key(&mut a, KeyCode::Char(' '), now);
    let Screen::Decks(v) = &a.screen else {
        panic!()
    };
    assert_eq!(v.rows.len(), 3);
    assert!(v.rows[1].collapsed);
    assert!(b.state.borrow().log.iter().any(|c| matches!(
        c,
        Command::SetDeckCollapsed {
            deck_id: 2,
            collapsed: true
        }
    )));
    key(&mut a, KeyCode::Enter, now);
    assert!(matches!(a.screen, Screen::Overview(_)));
    key(&mut a, KeyCode::Char('s'), now);
    assert!(matches!(a.screen, Screen::Review(_)));
    key(&mut a, KeyCode::Esc, now);
    assert!(matches!(a.screen, Screen::Overview(_)));
}

#[test]
fn typed_answers_capture_utf8_and_paste_before_shortcuts() {
    let mut c = card(1, "Type [[type:Back]]", "<hr id=answer>[[type:Back]]");
    c.rendered.type_answer = Some(TypeAnswer::Input {
        expected: "日本 123".into(),
        combining: true,
    });
    let b = FakeBackend::new(vec![c]);
    let now = t0();
    let mut a = app(&b, Start::Review, now);
    for c in "日本".chars() {
        key(&mut a, KeyCode::Char(c), now);
    }
    a.handle(AppEvent::Paste("\n123\x1b".into()), now);
    key(&mut a, KeyCode::Char('r'), now);
    let Screen::Review(v) = &a.screen else {
        panic!()
    };
    assert_eq!(v.reviewer.typed().unwrap().text(), "日本 123r");
    assert!(b.state.borrow().answered.is_empty());
    snapshot("typing", &mut a);
    key(&mut a, KeyCode::Enter, now);
    assert!(b
        .state
        .borrow()
        .log
        .iter()
        .any(|c| matches!(c, Command::CompareAnswer { provided, .. } if provided == "日本 123r")));
    let Screen::Review(v) = &a.screen else {
        panic!()
    };
    assert_eq!(v.reviewer.phase(), Phase::Answer);
}

#[test]
fn old_transition_keys_releases_and_rating_repeats_are_dropped() {
    let b = FakeBackend::new(vec![card(1, "Q1", "A1"), card(2, "Q2", "A2")]);
    let now = t0();
    let mut a = app(&b, Start::Review, now);
    key(&mut a, KeyCode::Char('1'), now);
    assert!(b.state.borrow().answered.is_empty());
    key(&mut a, KeyCode::Enter, now);
    let mut repeat = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    repeat.kind = crossterm::event::KeyEventKind::Repeat;
    a.handle(AppEvent::Key(repeat, now.at), now);
    assert!(b.state.borrow().answered.is_empty());
    key(&mut a, KeyCode::Char('3'), now.plus_millis(100));
    assert_eq!(b.state.borrow().answered.len(), 1);
    // Received while the first answer was in progress.
    a.handle(
        AppEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), now.at),
        now.plus_millis(101),
    );
    let Screen::Review(v) = &a.screen else {
        panic!()
    };
    assert_eq!(v.reviewer.phase(), Phase::Question);
    assert_eq!(v.reviewer.card().unwrap().queued.id, 2);
}

#[test]
fn palette_remapping_help_and_quit_work_across_overlays() {
    let b = FakeBackend::new(vec![card(1, "Q", "A")]);
    let now = t0();
    let mut a = app(&b, Start::Review, now);
    let mut map = Keymap::default();
    map.bind(
        Scope::Review,
        Chord::parse("f8").unwrap(),
        Action::ShowAnswerOrGood,
    );
    a.set_keymap(map);
    assert!(a
        .keymap
        .help_rows(Scope::Review, &a.scopes())
        .iter()
        .any(|(action, keys)| *action == Action::ShowAnswerOrGood
            && keys.contains(&Chord::parse("f8").unwrap())));
    key(&mut a, KeyCode::F(8), now);
    let Screen::Review(v) = &a.screen else {
        panic!()
    };
    assert_eq!(v.reviewer.phase(), Phase::Answer);
    a.dispatch(Action::Palette);
    a.handle(AppEvent::Paste("bury card".into()), now);
    assert_eq!(a.palette_matches("bury card")[0], Action::BuryCard);
    key(&mut a, KeyCode::Enter, now);
    assert!(matches!(a.screen, Screen::Congrats(_)));
    a.dispatch(Action::Help);
    a.handle(
        AppEvent::Key(
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
            now.plus_millis(100).at,
        ),
        now.plus_millis(100),
    );
    assert!(a.should_quit());
}

#[test]
fn timebox_overlay_blocks_auto_audio_idle_and_finishes_to_decks() {
    let b = FakeBackend::new(vec![card(1, "Q1", "A1"), card(2, "Q2", "A2")]);
    b.with(|s| {
        s.config.timebox_secs = 1;
        s.config.seconds_to_show_question = 0.01;
    });
    let now = t0();
    let mut a = app(&b, Start::Review, now);
    a.dispatch(Action::ToggleAutoAdvance);
    key(&mut a, KeyCode::Enter, now);
    a.handle(
        AppEvent::Key(
            KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE),
            now.plus_millis(2000).at,
        ),
        now.plus_millis(2000),
    );
    assert!(matches!(a.overlay, Some(Overlay::Choice { .. })));
    a.handle(AppEvent::Audio(AudioEvent::Idle), now.plus_millis(3000));
    assert!(matches!(a.overlay, Some(Overlay::Choice { .. })));
    snapshot("timebox", &mut a);
    key(&mut a, KeyCode::Down, now.plus_millis(3001));
    key(&mut a, KeyCode::Enter, now.plus_millis(3002));
    assert!(matches!(a.screen, Screen::Decks(_)));
}

#[test]
fn day_rollover_and_congrats_poll_refresh_decks_and_overview() {
    let now = t0();
    let b = FakeBackend::new(vec![]);
    b.with(|s| s.timing.next_day_at = now.wall_ms / 1000 + 1);
    let mut a = app(&b, Start::Decks, now);
    b.clear_log();
    a.handle(AppEvent::Tick, now.plus_millis(1000));
    assert_eq!(b.names(), ["SchedTiming", "DeckTree", "StudiedToday"]);
    b.with(|s| {
        s.overview.new = 0;
        s.overview.learning = 0;
        s.overview.review = 0;
    });
    a.show_congrats();
    b.with(|s| s.overview.learning = 1);
    a.handle(AppEvent::Tick, now.plus_millis(61_000));
    assert!(matches!(a.screen, Screen::Overview(_)));
}

#[test]
fn scheduler_upgrade_and_delete_require_confirmation_errors_visible() {
    let now = t0();
    let b = FakeBackend::new(vec![card(1, "Q", "A")]);
    b.with(|s| s.status.v3 = false);
    let mut a = app(&b, Start::Review, now);
    assert!(matches!(
        a.overlay,
        Some(Overlay::Confirm {
            kind: ConfirmKind::UpgradeScheduler,
            yes: false,
            ..
        })
    ));
    key(&mut a, KeyCode::Char('n'), now);
    assert!(matches!(a.screen, Screen::Decks(_)));
    assert!(!b.names().contains(&"UpgradeScheduler".to_owned()));
    a.start_review();
    key(&mut a, KeyCode::Char('y'), now);
    a.dispatch(Action::DeleteNote);
    key(&mut a, KeyCode::Enter, now);
    assert!(!b.names().contains(&"DeleteNotes".to_owned()));
    b.fail("DeleteNotes", CoreError::Other("fixture failure".into()));
    a.dispatch(Action::DeleteNote);
    key(&mut a, KeyCode::Char('y'), now);
    assert!(a
        .visible_message()
        .unwrap()
        .text
        .contains("fixture failure"));
    a.handle(AppEvent::Tick, now.plus_millis(5001));
    assert!(a.visible_message().is_none());
}

#[test]
fn resize_and_every_screen_are_safe_at_small_terminal_sizes() {
    let b = FakeBackend::new(vec![card(1, "Q", "A")]);
    let now = t0();
    let mut a = app(&b, Start::Decks, now);
    for size in [(1, 1), (10, 2), (49, 9), (50, 10), (80, 24)] {
        rendered(&mut a, size.0, size.1);
    }
    a.open_overview(Some(1));
    rendered(&mut a, 50, 10);
    a.start_review();
    rendered(&mut a, 50, 10);
    a.handle(AppEvent::Resize, now);
    a.show_congrats();
    rendered(&mut a, 50, 10);
}

struct WidgetRenderer;
impl CardRenderer for WidgetRenderer {
    fn render(
        &self,
        _: &tanki_render::CardSide,
        _: &tanki_render::RenderOptions,
    ) -> tanki_render::RenderedCard {
        use tanki_render::*;
        RenderedCard {
            lines: vec![Line::default(); 40],
            images: vec![
                ImageBlock {
                    source: ImageSource::File("/tmp/test-image.png".into()),
                    row: 1,
                    col: 0,
                    width: 4,
                    height: 2,
                    alt: "image".into()
                };
                2
            ],
            widgets: vec![Widget {
                key: "hint:0".into(),
                kind: WidgetKind::Hint {
                    label: "Hint".into(),
                    expanded: false,
                },
                row: 0,
                col: 0,
                width: 4,
            }],
            ..Default::default()
        }
    }
    fn paint(
        &self,
        _: &tanki_render::RenderedCard,
        _: usize,
        _: Rect,
        _: &mut ratatui::buffer::Buffer,
    ) {
    }
}

struct CleanupImages {
    pending: bool,
}
impl ImageDisplay for CleanupImages {
    fn mode(&self) -> tanki_term::GraphicsMode {
        tanki_term::GraphicsMode::Kitty
    }
    fn cycle(&mut self) -> tanki_term::GraphicsMode {
        self.clear();
        tanki_term::GraphicsMode::None
    }
    fn draw(
        &mut self,
        _: &tanki_render::ImageSource,
        _: Rect,
        _: &mut ratatui::buffer::Buffer,
    ) -> crate::services::ImageState {
        crate::services::ImageState::Drawn
    }
    fn clear(&mut self) {
        self.pending = true;
    }
    fn take_cleanup(&mut self) -> String {
        if std::mem::take(&mut self.pending) {
            "delete-images".into()
        } else {
            String::new()
        }
    }
}

#[test]
fn images_zoom_scroll_screen_changes_and_protocol_cycle_cleanup_placements() {
    let b = FakeBackend::new(vec![card(1, "Q", "A")]);
    let now = t0();
    let mut a = app(&b, Start::Review, now);
    a.services.renderer = Box::new(WidgetRenderer);
    a.services.images = Box::new(CleanupImages { pending: false });
    rendered(&mut a, 80, 24);
    a.dispatch(Action::ZoomImage);
    assert!(matches!(a.overlay, Some(Overlay::Zoom { index: 0 })));
    a.services.images = Box::new(crate::testing::FakeImages::default());
    snapshot("zoom", &mut a);
    a.services.images = Box::new(CleanupImages { pending: false });
    key(&mut a, KeyCode::Char('n'), now);
    assert!(matches!(a.overlay, Some(Overlay::Zoom { index: 1 })));
    key(&mut a, KeyCode::Esc, now);
    assert!(a.overlay.is_none());
    let (cleanup, clear) = a.take_terminal_requests();
    assert!(cleanup.contains("delete-images") && clear);
    a.dispatch(Action::Down);
    assert!(a.take_terminal_requests().0.contains("delete-images"));
    a.dispatch(Action::ToggleHints);
    let Screen::Review(v) = &a.screen else {
        panic!()
    };
    assert!(v.reviewer.expanded.contains("hint:0"));
    a.dispatch(Action::CycleGraphics);
    let (cleanup, clear) = a.take_terminal_requests();
    assert!(cleanup.contains("delete-images") && clear);
    a.go_decks();
    assert!(a.take_terminal_requests().0.contains("delete-images"));
}

#[test]
fn browser_escape_hatch_opens_the_active_side_and_reports_opener_errors() {
    let b = FakeBackend::new(vec![card(97, "Browser Q", "Browser A")]);
    let now = t0();
    let mut a = app(&b, Start::Review, now);
    let opened = std::rc::Rc::new(std::cell::RefCell::new(None));
    let record = opened.clone();
    a.services.opener = Box::new(move |path| {
        *record.borrow_mut() = Some(path.to_owned());
        Ok(())
    });
    a.dispatch(Action::OpenInBrowser);
    let path = opened.borrow().clone().unwrap();
    let html = std::fs::read_to_string(&path).unwrap();
    assert!(html.contains("Browser Q") && !html.contains("Browser A"));
    assert!(html.contains("<base href=\"file:///tmp/tanki-test-profile/collection.media/\">"));
    a.dispatch(Action::ShowAnswerOrGood);
    a.dispatch(Action::OpenInBrowser);
    std::fs::remove_file(path).unwrap();
    let path = opened.borrow().clone().unwrap();
    let html = std::fs::read_to_string(&path).unwrap();
    assert!(html.contains("Browser A"));
    std::fs::remove_file(path).unwrap();
    let record = opened.clone();
    a.services.opener = Box::new(move |path| {
        *record.borrow_mut() = Some(path.to_owned());
        Err(std::io::Error::other("no browser"))
    });
    a.dispatch(Action::OpenInBrowser);
    assert!(a.visible_message().unwrap().text.contains("no browser"));
    std::fs::remove_file(opened.borrow().as_ref().unwrap()).unwrap();
}

// ---- plugin hooks and generic hold ----------------------------------------

mod plugin_port {
    use crate::plugins::PluginPort;
    use std::{cell::RefCell, rc::Rc, time::Instant};
    use tanki_plugin::{Event, EventKind, Hook, HookContext};

    #[derive(Default)]
    pub struct Shared {
        pub seen: Vec<HookContext>,
        pub on_hook: Vec<Event>,
        pub pending: Vec<Event>,
        pub actions_run: Vec<(String, String, i64)>,
    }
    pub struct Fake(pub Rc<RefCell<Shared>>);
    impl PluginPort for Fake {
        fn dispatch(&mut self, _: Hook, ctx: &HookContext, _: Instant) -> Vec<Event> {
            let mut s = self.0.borrow_mut();
            s.seen.push(ctx.clone());
            std::mem::take(&mut s.on_hook)
        }
        fn action(&mut self, plugin: &str, id: &str, card_id: i64, _: Instant) -> Vec<Event> {
            self.0
                .borrow_mut()
                .actions_run
                .push((plugin.into(), id.into(), card_id));
            vec![]
        }
        fn poll(&mut self, _: Instant) -> Vec<Event> {
            std::mem::take(&mut self.0.borrow_mut().pending)
        }
        fn actions(&self) -> Vec<(String, String, String)> {
            vec![("fake".into(), "open".into(), "Open it".into())]
        }
        fn shutdown(&mut self) {}
    }
    pub fn event(kind: EventKind) -> Event {
        Event {
            plugin: "fake".into(),
            kind,
        }
    }
}

fn plugin_app<'a>(
    b: &'a FakeBackend,
    now: Now,
) -> (
    App<'a>,
    std::rc::Rc<std::cell::RefCell<plugin_port::Shared>>,
) {
    let mut a = app(b, Start::Review, now);
    let shared = std::rc::Rc::new(std::cell::RefCell::new(plugin_port::Shared::default()));
    a.set_plugins(Some(Box::new(plugin_port::Fake(shared.clone()))));
    (a, shared)
}

#[test]
fn plugin_hook_fires_once_after_reveal_with_note_fields() {
    let mut c = card(1, "Q", "A");
    c.queued.fields = [("CodeTour".to_owned(), "x".to_owned())].into();
    let b = FakeBackend::new(vec![c]);
    let now = t0();
    let (mut a, shared) = plugin_app(&b, now);
    a.handle(AppEvent::Tick, now);
    assert!(shared.borrow().seen.is_empty(), "nothing before the reveal");
    a.dispatch(Action::ShowAnswerOrGood);
    a.handle(AppEvent::Tick, now);
    a.handle(AppEvent::Tick, now);
    let seen = &shared.borrow().seen;
    assert_eq!(seen.len(), 1);
    assert_eq!((seen[0].card_id, seen[0].note_id), (1, 10));
    assert_eq!(seen[0].fields["CodeTour"], "x");
}

#[test]
fn plugin_hold_blocks_rating_and_ctrl_o_force_releases() {
    use tanki_plugin::EventKind::*;
    let b = FakeBackend::new(vec![card(1, "Q", "A")]);
    let now = t0();
    let (mut a, shared) = plugin_app(&b, now);
    a.dispatch(Action::ShowAnswerOrGood);
    shared.borrow_mut().on_hook = vec![plugin_port::event(HoldReview("opening".into()))];
    a.handle(AppEvent::Tick, now);
    assert!(a.holding());
    assert_eq!(a.hold_status(), Some("opening"));
    shared.borrow_mut().pending = vec![plugin_port::event(Status("reading".into()))];
    a.handle(AppEvent::Tick, now);
    assert_eq!(a.hold_status(), Some("reading"));
    a.dispatch(Action::Good);
    assert!(b.state.borrow().answered.is_empty(), "rating is blocked");
    a.dispatch(Action::ReleaseHold);
    assert!(!a.holding() && a.hold_status().is_none());
    // The hook does not fire again for the same card after returning.
    a.handle(AppEvent::Tick, now.plus_millis(1000));
    assert_eq!(shared.borrow().seen.len(), 1);
    a.dispatch(Action::Good);
    assert_eq!(b.state.borrow().answered.len(), 1);
}

#[test]
fn plugin_release_event_and_dropped_plugin_end_the_hold() {
    use tanki_plugin::EventKind::*;
    for ending in [
        vec![ReleaseReview],
        vec![
            ReleaseReview,
            Dropped {
                reason: "exited".into(),
                permanent: false,
            },
        ],
    ] {
        let b = FakeBackend::new(vec![card(1, "Q", "A")]);
        let now = t0();
        let (mut a, shared) = plugin_app(&b, now);
        a.dispatch(Action::ShowAnswerOrGood);
        shared.borrow_mut().on_hook = vec![plugin_port::event(HoldReview("x".into()))];
        a.handle(AppEvent::Tick, now);
        assert!(a.holding());
        shared.borrow_mut().pending = ending.into_iter().map(plugin_port::event).collect();
        a.handle(AppEvent::Tick, now);
        assert!(!a.holding());
    }
}

#[test]
fn plugin_cannot_hold_the_question_side_or_a_second_time() {
    use tanki_plugin::EventKind::*;
    let b = FakeBackend::new(vec![card(1, "Q", "A")]);
    let now = t0();
    let (mut a, shared) = plugin_app(&b, now);
    shared.borrow_mut().pending = vec![plugin_port::event(HoldReview("early".into()))];
    a.handle(AppEvent::Tick, now);
    assert!(!a.holding(), "no hold before the answer is revealed");
    a.dispatch(Action::ShowAnswerOrGood);
    shared.borrow_mut().on_hook = vec![plugin_port::event(HoldReview("first".into()))];
    a.handle(AppEvent::Tick, now);
    shared.borrow_mut().pending = vec![plugin_port::event(HoldReview("second".into()))];
    a.handle(AppEvent::Tick, now);
    assert_eq!(a.hold_status(), Some("first"));
}

#[test]
fn plugin_actions_are_chosen_and_forwarded() {
    let b = FakeBackend::new(vec![card(1, "Q", "A")]);
    let now = t0();
    let (mut a, shared) = plugin_app(&b, now);
    a.dispatch(Action::PluginActions);
    assert!(a.overlay.is_some());
    a.handle(
        AppEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), now.at),
        now,
    );
    assert_eq!(
        shared.borrow().actions_run,
        [("fake".to_owned(), "open".to_owned(), 1)]
    );
}

#[test]
fn plugin_hint_shows_with_the_answer_for_its_own_card_only() {
    use tanki_plugin::EventKind::*;
    let b = FakeBackend::new(vec![card(1, "Q", "A")]);
    let now = t0();
    let (mut a, shared) = plugin_app(&b, now);
    let shown = |a: &mut App<'_>| buffer_text(&rendered(a, 80, 24)).join("\n");
    let hint = |card_id, text: Option<&str>| {
        plugin_port::event(Hint {
            card_id,
            text: text.map(str::to_owned),
        })
    };
    // Nothing on the question side, even when a hint is already known.
    shared.borrow_mut().pending = vec![hint(1, Some("Open code-tour in Neovim"))];
    a.handle(AppEvent::Tick, now);
    assert!(!shown(&mut a).contains("Open code-tour"));
    a.dispatch(Action::ShowAnswerOrGood);
    let text = shown(&mut a);
    assert!(text.contains("Shift+O Open code-tour in Neovim"), "{text}");
    // A late hint for another card is not shown on this one.
    shared.borrow_mut().pending = vec![hint(2, Some("Open other"))];
    a.handle(AppEvent::Tick, now);
    assert!(!shown(&mut a).contains("Open code-tour"));
    // Clearing, and a dropped plugin, remove it.
    shared.borrow_mut().pending = vec![hint(1, Some("Open code-tour in Neovim"))];
    a.handle(AppEvent::Tick, now);
    assert!(shown(&mut a).contains("Open code-tour"));
    shared.borrow_mut().pending = vec![hint(1, Some("  "))];
    a.handle(AppEvent::Tick, now);
    assert!(!shown(&mut a).contains("Open code-tour"));
    shared.borrow_mut().pending = vec![
        hint(1, Some("Open code-tour in Neovim")),
        plugin_port::event(Dropped {
            reason: "exited".into(),
            permanent: false,
        }),
    ];
    a.handle(AppEvent::Tick, now);
    assert!(!shown(&mut a).contains("Open code-tour"));
}
