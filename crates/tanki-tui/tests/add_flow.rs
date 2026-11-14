//! The add form against a real collection actor on a temp collection.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::PathBuf;
use tanki_core::{CollectionActor, Command, EditResponse, NoteCommand, Response};
use tanki_tui::add_ui::{AddIntent, AddUi};

struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("tanki-add-flow-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn typed(ui: &mut AddUi, actor: &CollectionActor, s: &str) {
    for c in s.chars() {
        ui.on_key(actor, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
}

fn get(actor: &CollectionActor, id: i64) -> tanki_core::NoteData {
    match actor
        .execute(Command::Notes(NoteCommand::GetNote { id }))
        .unwrap()
    {
        Response::Edit(EditResponse::Note(n)) => n,
        other => panic!("{other:?}"),
    }
}

#[test]
fn add_then_edit_in_a_real_collection() {
    let dir = Dir::new();
    let actor = CollectionActor::open(&dir.0).unwrap();
    let mut ui = AddUi::new_add(&actor, 0, actor.media_dir()).unwrap();
    typed(&mut ui, &actor, "<b>front</b>");
    ui.on_key(&actor, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    typed(&mut ui, &actor, "back");
    ui.on_key(
        &actor,
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
    );
    typed(&mut ui, &actor, "one two");
    let intent = ui.on_key(&actor, KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
    assert!(
        matches!(intent, AddIntent::Changed(_)),
        "{intent:?} {:?}",
        ui.status()
    );
    let id = ui.history()[0].note_id;
    let note = get(&actor, id);
    assert_eq!(note.fields, ["<b>front</b>", "back"]);
    assert_eq!(note.tags, ["one", "two"]);
    assert_eq!(ui.history()[0].label, "front, back");
    assert_eq!(ui.field_values(), ["", ""]);
}
