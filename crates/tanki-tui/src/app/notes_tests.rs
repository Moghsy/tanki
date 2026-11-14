//! The add / edit screen and the browser in the app, against fakes: the
//! study fake for Phase 1 commands, the add screen's fake for notes and the
//! browser's fake for browse commands.
use super::*;
use crate::add_ui::tests::Fake as NotesFake;
use crate::browser_ui::tests::Fake as BrowseFake;
use crate::browser_ui::BrowserIntent;
use crate::services::{NoImages, PlainTextRenderer};
use crate::testing::{buffer_text, card, t0, FakeAudio, FakeBackend};
use ratatui::{backend::TestBackend, Terminal};
use std::cell::RefCell;
use std::rc::Rc;
use tanki_core::{config::Config, BrowseCommand, Command, NoteCommand, NoteData, Response};

struct World {
    study: FakeBackend,
    notes: NotesFake,
    browse: BrowseFake,
}

impl World {
    fn new(cards: Vec<crate::testing::FakeCard>) -> Self {
        Self {
            study: FakeBackend::new(cards),
            notes: NotesFake::new(),
            browse: BrowseFake::new(6),
        }
    }
    fn deck_loads(&self) -> usize {
        self.study
            .names()
            .iter()
            .filter(|n| *n == "DeckTree")
            .count()
    }
    fn renders(&self) -> usize {
        self.study.names().iter().filter(|n| *n == "Render").count()
    }
    fn note(&self, id: i64) -> NoteData {
        self.notes.notes.borrow()[&id].clone()
    }
    fn put_note(&self, id: i64, fields: [&str; 2]) {
        self.notes.notes.borrow_mut().insert(
            id,
            NoteData {
                id,
                notetype_id: 1,
                mtime_secs: 1000,
                fields: fields.map(String::from).to_vec(),
                ..Default::default()
            },
        );
    }
}

impl Backend for World {
    fn execute(&self, command: Command) -> backend::Result<Response> {
        match &command {
            Command::Notes(
                NoteCommand::AddTags { .. }
                | NoteCommand::RemoveTags { .. }
                | NoteCommand::ClearUnusedTags,
            )
            | Command::Browse(_) => self.browse.execute(command),
            Command::Notes(_) => self.notes.execute(command),
            _ => self.study.execute(command),
        }
    }
}

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tanki-tui-notes-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join("flows")).unwrap();
        Self(path)
    }
    fn config(&self) -> PathBuf {
        self.0.join("config.toml")
    }
    fn profile(&self) -> PathBuf {
        self.0.join("profile")
    }
    fn key(&self) -> String {
        self.profile().to_string_lossy().into_owned()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn app<'a>(w: &'a World, temp: &Temp, start: Start) -> App<'a> {
    let mut app = App::new(
        w,
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
            dir: temp.profile(),
            media_dir: temp.profile().join("collection.media"),
        },
        start,
        t0(),
    );
    app.set_config(Some(temp.config()));
    app.flow_options.temp_dir = Some(temp.0.join("flows"));
    app
}

fn press_with(a: &mut App<'_>, code: KeyCode, modifiers: KeyModifiers) {
    let now = t0();
    a.handle(AppEvent::Key(KeyEvent::new(code, modifiers), now.at), now);
}
fn press(a: &mut App<'_>, code: KeyCode) {
    press_with(a, code, KeyModifiers::NONE);
}
fn typed(a: &mut App<'_>, text: &str) {
    for c in text.chars() {
        press(a, KeyCode::Char(c));
    }
}
fn message(a: &App<'_>) -> String {
    a.visible_message()
        .map(|m| m.text.clone())
        .unwrap_or_default()
}
fn screen_text(a: &mut App<'_>) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| a.draw(f)).unwrap();
    buffer_text(terminal.backend().buffer()).join("\n")
}
fn snapshot(name: &str, a: &mut App<'_>) {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| a.draw(f)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let rows = buffer_text(&buffer).join("\n");
    let styled = format!("{rows}\n\n{buffer:?}\n");
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("{name}.snap"));
    if std::env::var_os("TANKI_UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, &styled).unwrap();
    }
    assert_eq!(styled, std::fs::read_to_string(path).unwrap());
}
fn add_ui<'b>(a: &'b App<'_>) -> &'b AddUi {
    match a.layers.last() {
        Some(Layer::Add(ui)) => ui,
        _ => panic!("the add screen is not open"),
    }
}
fn browser<'b>(a: &'b App<'_>) -> &'b BrowserUi {
    match a.layers.last() {
        Some(Layer::Browser(ui)) => ui,
        _ => panic!("the browser is not open"),
    }
}

/// An editor that records the command and edits the file with `f`.
fn editor(
    seen: Rc<RefCell<Vec<Vec<String>>>>,
    f: impl Fn(&str) -> String + 'static,
) -> EditorRunner {
    Box::new(move |command, path| {
        seen.borrow_mut().push(command.to_vec());
        let text = std::fs::read_to_string(path)?;
        std::fs::write(path, f(&text))?;
        Ok(EditorExit::Success)
    })
}

#[test]
fn add_opens_from_the_deck_list_and_closes_with_the_discard_prompt() {
    let temp = Temp::new();
    let w = World::new(vec![card(1, "Q", "A")]);
    let mut a = app(&w, &temp, Start::Decks);
    press(&mut a, KeyCode::Char('a'));
    assert_eq!(a.scopes(), [Scope::Add]);
    assert!(!add_ui(&a).is_edit());
    // Global keys are typing now: `d` and `s` go into the field.
    typed(&mut a, "dos");
    assert_eq!(add_ui(&a).field_values()[0], "dos");
    snapshot("add_layer", &mut a);
    // Esc asks; `n` keeps editing, Esc and `y` discard.
    press(&mut a, KeyCode::Esc);
    assert!(add_ui(&a).has_overlay());
    press(&mut a, KeyCode::Char('n'));
    assert_eq!(add_ui(&a).field_values()[0], "dos");
    press(&mut a, KeyCode::Esc);
    press(&mut a, KeyCode::Char('y'));
    assert!(a.layers.is_empty());
    assert!(matches!(a.screen, Screen::Decks(_)));
    assert!(screen_text(&mut a).contains("Default"));
    // Nothing typed: Esc closes at once.
    press(&mut a, KeyCode::Char('a'));
    press(&mut a, KeyCode::Esc);
    assert!(a.layers.is_empty());
}

#[test]
fn adding_refreshes_the_deck_list_and_quit_asks_about_unsaved_input() {
    let temp = Temp::new();
    let w = World::new(vec![]);
    let mut a = app(&w, &temp, Start::Decks);
    press(&mut a, KeyCode::Char('a'));
    typed(&mut a, "hola");
    let loads = w.deck_loads();
    press_with(&mut a, KeyCode::Enter, KeyModifiers::CONTROL);
    assert_eq!(w.notes.count("AddNote"), 1);
    assert!(w.deck_loads() > loads, "the deck counts were not refreshed");
    assert!(
        !a.layers.is_empty(),
        "the add screen stays open after adding"
    );
    // Ctrl+Q with unsaved input asks first.
    typed(&mut a, "x");
    press_with(&mut a, KeyCode::Char('q'), KeyModifiers::CONTROL);
    assert!(!a.should_quit());
    assert!(matches!(
        a.overlay,
        Some(Overlay::Confirm {
            kind: ConfirmKind::QuitDiscard,
            ..
        })
    ));
    press(&mut a, KeyCode::Char('n'));
    assert!(!a.should_quit() && a.overlay.is_none());
    // F1 shows the screen's own keys.
    press(&mut a, KeyCode::F(1));
    let text = screen_text(&mut a);
    assert!(text.contains("Keys: Add / edit note"), "{text}");
    assert!(text.contains("Alt+X / Alt+Shift+X"), "{text}");
    press(&mut a, KeyCode::Esc);
    press_with(&mut a, KeyCode::Char('q'), KeyModifiers::CONTROL);
    press(&mut a, KeyCode::Char('y'));
    assert!(a.should_quit());
}

#[test]
fn paste_and_external_field_edit_go_to_the_add_screen() {
    let temp = Temp::new();
    let w = World::new(vec![]);
    let mut a = app(&w, &temp, Start::Decks);
    press(&mut a, KeyCode::Char('a'));
    a.handle(AppEvent::Paste("pasted <i>text</i>".into()), t0());
    assert_eq!(add_ui(&a).field_values()[0], "pasted <i>text</i>");
    std::fs::write(temp.config(), "[editor]\ncommand = \"myeditor --wait\"\n").unwrap();
    let seen = Rc::new(RefCell::new(Vec::new()));
    a.set_editor(editor(seen.clone(), |text| {
        format!("{}\n", text.replace("<i>", "<b>").replace("</i>", "</b>"))
    }));
    a.take_terminal_requests();
    press_with(&mut a, KeyCode::Char('x'), KeyModifiers::ALT);
    assert_eq!(seen.borrow()[0], ["myeditor", "--wait"]);
    // The editor's final newline is dropped; the screen is repainted.
    assert_eq!(add_ui(&a).field_values()[0], "pasted <b>text</b>");
    assert!(a.take_terminal_requests().1);
    // The whole note: field text and tags come back into the form.
    a.set_editor(editor(seen.clone(), |text| {
        text.replace("\ntags:\n", "\ntags: fresh\n")
            .replace("## Back\n", "## Back\nthe answer\n")
    }));
    press_with(
        &mut a,
        KeyCode::Char('X'),
        KeyModifiers::ALT | KeyModifiers::SHIFT,
    );
    assert_eq!(
        add_ui(&a).field_values(),
        ["pasted <b>text</b>", "the answer"]
    );
    assert_eq!(add_ui(&a).tags_text(), "fresh");
    // A failing editor changes nothing and says so.
    a.set_editor(Box::new(|_, _| Ok(EditorExit::Failure)));
    press_with(&mut a, KeyCode::Char('x'), KeyModifiers::ALT);
    assert_eq!(message(&a), "Edit cancelled.");
    assert_eq!(std::fs::read_dir(temp.0.join("flows")).unwrap().count(), 0);
}

#[test]
fn edit_current_from_the_reviewer_shows_the_saved_note() {
    let temp = Temp::new();
    let w = World::new(vec![card(1, "front", "back")]);
    // The fake card 1 belongs to note 10.
    w.put_note(10, ["front", "back"]);
    let mut a = app(&w, &temp, Start::Review);
    assert!(screen_text(&mut a).contains("front"));
    press(&mut a, KeyCode::Char('e'));
    assert_eq!(add_ui(&a).edited_note_id(), Some(10));
    press_with(&mut a, KeyCode::End, KeyModifiers::NONE);
    typed(&mut a, " edited");
    // What the collection renders after the save.
    w.study
        .with(|s| s.cards[0].rendered.question = "front edited".into());
    let renders = w.renders();
    press_with(&mut a, KeyCode::Enter, KeyModifiers::CONTROL);
    assert!(a.layers.is_empty());
    assert_eq!(w.note(10).fields[0], "front edited");
    assert!(w.renders() > renders, "the card was not rendered again");
    assert_eq!(message(&a), "Note saved.");
    assert!(screen_text(&mut a).contains("front edited"));
    assert!(matches!(a.screen, Screen::Review(_)));
}

#[test]
fn browser_opens_on_the_current_card_and_searches_are_remembered() {
    let temp = Temp::new();
    let w = World::new(vec![card(3, "Q", "A")]);
    let mut a = app(&w, &temp, Start::Review);
    press(&mut a, KeyCode::Char('b'));
    assert_eq!(a.scopes(), [Scope::Browser]);
    assert!(
        w.browse
            .logged(|c| matches!(
                c,
                Command::Browse(BrowseCommand::NormalizeSearch { query }) if query == "cid:3"
            ))
            .len()
            == 1
    );
    // A committed search lands in the profile's history in the config.
    press(&mut a, KeyCode::Char('/'));
    press(&mut a, KeyCode::End);
    press_with(&mut a, KeyCode::Char('u'), KeyModifiers::CONTROL);
    a.handle(AppEvent::Paste("tag:x".into()), t0());
    press(&mut a, KeyCode::Enter);
    assert_eq!(browser(&a).query(), "tag:x");
    let config = Config::load(&temp.config()).unwrap();
    assert_eq!(
        config.profile(&temp.key()).unwrap().search_history,
        ["tag:x"]
    );
    snapshot("browser_layer", &mut a);
    // Not available yet: said, not done.
    a.run_browser_intent(BrowserIntent::CardInfo(3));
    assert!(message(&a).contains("not available yet"));
    // `q` closes; the reviewer is still on its card.
    press(&mut a, KeyCode::Char('q'));
    assert!(a.layers.is_empty());
    assert!(matches!(a.screen, Screen::Review(_)));
    // From the deck list the browser opens on the default search.
    let mut a = app(&w, &temp, Start::Decks);
    press(&mut a, KeyCode::Char('b'));
    assert_eq!(browser(&a).query(), "deck:current");
}

#[test]
fn browser_edit_runs_the_injected_editor_and_reports_conflicts() {
    let temp = Temp::new();
    let w = World::new(vec![]);
    w.put_note(100, ["Paris", "France"]);
    let mut a = app(&w, &temp, Start::Decks);
    press(&mut a, KeyCode::Char('b'));
    let seen = Rc::new(RefCell::new(Vec::new()));
    a.set_editor(editor(seen.clone(), |text| text.replace("Paris", "Lyon")));
    a.run_browser_intent(BrowserIntent::EditNotes(vec![100]));
    assert_eq!(seen.borrow().len(), 1);
    assert_eq!(w.note(100).fields, ["Lyon", "France"]);
    assert_eq!(message(&a), "Updated 1 note.");
    assert!(matches!(a.layers.last(), Some(Layer::Browser(_))));
    // The note changed in the meantime: nothing is written, the file kept.
    *w.notes.conflict.borrow_mut() = true;
    a.set_editor(editor(seen.clone(), |text| text.replace("Lyon", "Nice")));
    a.run_browser_intent(BrowserIntent::EditNotes(vec![100]));
    assert_eq!(w.note(100).fields[0], "Lyon");
    let text = message(&a);
    assert!(
        text.starts_with("Nothing was saved, notes changed elsewhere: 100."),
        "{text}"
    );
    let kept = text.split("kept in ").nth(1).unwrap();
    assert!(std::fs::read_to_string(kept).unwrap().contains("Nice"));
    // Saving the file unchanged changes nothing.
    *w.notes.conflict.borrow_mut() = false;
    a.set_editor(editor(seen, |text| text.to_owned()));
    a.run_browser_intent(BrowserIntent::EditNotes(vec![100]));
    assert_eq!(message(&a), "No changes.");
}

#[test]
fn open_browser_from_the_add_history_stacks_layers() {
    let temp = Temp::new();
    let w = World::new(vec![]);
    let mut a = app(&w, &temp, Start::Decks);
    press(&mut a, KeyCode::Char('a'));
    typed(&mut a, "uno");
    press_with(&mut a, KeyCode::Enter, KeyModifiers::CONTROL);
    let id = add_ui(&a).history()[0].note_id;
    a.run_add_intent(crate::add_ui::AddIntent::OpenBrowser { note_id: id });
    assert_eq!(a.layers.len(), 2);
    assert_eq!(browser(&a).query(), format!("nid:{id}"));
    press(&mut a, KeyCode::Char('q'));
    assert_eq!(a.layers.len(), 1);
    assert!(matches!(a.layers.last(), Some(Layer::Add(_))));
}
