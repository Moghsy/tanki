//! The deck options widget against a real `CollectionActor` on the study
//! fixture: load, edit with keys, save through `Backend::start`, reload.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{buffer::Buffer, layout::Rect};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tanki_core::deck_options::form::{FieldId, Scope};
use tanki_core::{CollectionActor, Command, Response};
use tanki_tui::deck_options_ui::{DeckOptionsIntent, DeckOptionsUi};

struct FixtureDir(PathBuf);
impl FixtureDir {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "tanki-tui-deck-options-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn deck_id(actor: &CollectionActor, path: &[&str]) -> i64 {
    let Response::DeckTree(root) = actor.execute(Command::DeckTree).unwrap() else {
        panic!("deck tree")
    };
    let mut node = &root;
    for part in path {
        node = node.children.iter().find(|d| d.name == *part).unwrap();
    }
    node.id
}

/// Ticks until the started command finishes.
fn finish(ui: &mut DeckOptionsUi, actor: &CollectionActor) -> DeckOptionsIntent {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let intent = ui.tick(actor);
        if !ui.is_busy() {
            return intent;
        }
        assert!(Instant::now() < deadline, "operation did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn text(ui: &DeckOptionsUi) -> Vec<String> {
    let area = Rect::new(0, 0, 100, 30);
    let mut buf = Buffer::empty(area);
    ui.render(area, &mut buf);
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol().to_owned())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

#[test]
fn edit_and_save_against_the_actor() {
    let dir = FixtureDir::new();
    let actor = CollectionActor::open(&dir.0).unwrap();
    actor.execute(Command::GenerateStudyFixture).unwrap();
    let french = deck_id(&actor, &["Study", "Vocabulary", "French"]);

    let mut ui = DeckOptionsUi::open(&actor, french).unwrap();
    let screen = text(&ui);
    assert_eq!(screen[0], "Deck Options — Study::Vocabulary::French");
    assert!(screen[1].starts_with("Preset: Study: French (used by 1 deck)"));
    assert!(screen
        .iter()
        .any(|l| l.contains("› New cards/day") && l.contains(" 6  [Preset]")));

    // New cards/day 6 -> 8 in the preset; a "this deck" review limit.
    for k in [
        KeyCode::Right,
        KeyCode::Right,
        KeyCode::Down,
        KeyCode::Char('t'),
    ] {
        ui.on_key(key(k), &actor);
    }
    ui.on_key(key(KeyCode::Left), &actor);
    assert_eq!(ui.state().scope(FieldId::ReviewsPerDay), Scope::Deck);
    // Turn on "Bury interday learning siblings".
    ui.focus(FieldId::BuryInterdayLearning);
    ui.on_key(key(KeyCode::Enter), &actor);
    assert!(ui.is_dirty());
    assert_eq!(
        ui.on_key(key(KeyCode::Char('s')), &actor),
        DeckOptionsIntent::None
    );
    assert_eq!(
        finish(&mut ui, &actor),
        DeckOptionsIntent::Saved("Deck options saved.".into())
    );

    let ui = DeckOptionsUi::open(&actor, french).unwrap();
    let state = ui.state();
    assert!(!state.is_dirty());
    assert_eq!(state.current().config.new_per_day, 8);
    assert!(state.current().config.bury_interday_learning);
    assert_eq!(state.data().limits.review, Some(199));
    assert_eq!(state.scope(FieldId::ReviewsPerDay), Scope::Deck);
}

#[test]
fn fsrs_tools_run_as_long_operations() {
    let dir = FixtureDir::new();
    let actor = CollectionActor::open(&dir.0).unwrap();
    actor.execute(Command::GenerateStudyFixture).unwrap();
    let study = deck_id(&actor, &["Study"]);
    let mut ui = DeckOptionsUi::open(&actor, study).unwrap();
    // Turn FSRS on and save.
    ui.focus(FieldId::Fsrs);
    ui.on_key(key(KeyCode::Enter), &actor);
    ui.on_key(key(KeyCode::Char('s')), &actor);
    assert!(matches!(
        finish(&mut ui, &actor),
        DeckOptionsIntent::Saved(_)
    ));

    let mut ui = DeckOptionsUi::open(&actor, study).unwrap();
    assert!(ui.state().fsrs());
    // Optimise: the fixture's reviews all happened today, so Anki finds no
    // history to train on and says so.
    ui.on_key(key(KeyCode::Char('o')), &actor);
    assert!(ui.is_busy());
    assert_eq!(finish(&mut ui, &actor), DeckOptionsIntent::None);
    let screen = text(&ui);
    assert!(
        screen.iter().any(|l| l.contains("No reviews found.")),
        "{screen:#?}"
    );
    ui.on_key(key(KeyCode::Enter), &actor);

    // The simulator runs and draws a braille chart.
    ui.on_key(key(KeyCode::Char('v')), &actor);
    for _ in 0..12 {
        // Fewer days (365 -> 92) keep the test quick.
        ui.on_key(key(KeyCode::Left), &actor);
    }
    ui.on_key(key(KeyCode::Enter), &actor);
    finish(&mut ui, &actor);
    let screen = text(&ui);
    let braille = screen
        .iter()
        .flat_map(|l| l.chars())
        .filter(|c| ('\u{2801}'..='\u{28ff}').contains(c))
        .count();
    assert!(braille > 0, "{screen:#?}");
    ui.on_key(key(KeyCode::Esc), &actor);
    assert_eq!(
        ui.on_key(key(KeyCode::Esc), &actor),
        DeckOptionsIntent::Close
    );
}
