//! The note types screens against a real collection actor on a temp study
//! fixture.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{buffer::Buffer, layout::Rect};
use std::path::PathBuf;
use tanki_core::notetypes::{NotetypeCommand, NotetypeDetail, NotetypeResponse};
use tanki_core::{
    BrowseCommand, BrowseMode, CollectionActor, Command, EditResponse, ManageCommand,
    ManageResponse, NoteCommand, Response, SearchOrder,
};
use tanki_tui::notetypes_ui::{ChangeIntent, ChangeNotetypeUi, NotetypesIntent, NotetypesUi};
use tanki_tui::services::AnkiRenderer;

struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "tanki-notetypes-flow-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl_s() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)
}

fn typed(ui: &mut NotetypesUi, actor: &CollectionActor, s: &str) {
    for c in s.chars() {
        ui.on_key(actor, key(KeyCode::Char(c)));
    }
}

fn screen(ui: &NotetypesUi, w: u16, h: u16) -> String {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    ui.render(area, &mut buf, &AnkiRenderer);
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buf[(x, y)].symbol().to_owned())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn get(actor: &CollectionActor, id: i64) -> NotetypeDetail {
    match actor
        .execute(Command::Manage(ManageCommand::Notetypes(
            NotetypeCommand::Get { id },
        )))
        .unwrap()
    {
        Response::Manage(ManageResponse::Notetypes(NotetypeResponse::Detail(d))) => d,
        other => panic!("{other:?}"),
    }
}

fn note_ids(actor: &CollectionActor, query: &str) -> Vec<i64> {
    match actor
        .execute(Command::Browse(BrowseCommand::Search {
            query: query.into(),
            mode: BrowseMode::Notes,
            order: SearchOrder::Default,
        }))
        .unwrap()
    {
        Response::Edit(EditResponse::Ids(ids)) => ids,
        other => panic!("{other:?}"),
    }
}

fn note(actor: &CollectionActor, id: i64) -> tanki_core::NoteData {
    match actor
        .execute(Command::Notes(NoteCommand::GetNote { id }))
        .unwrap()
    {
        Response::Edit(EditResponse::Note(n)) => n,
        other => panic!("{other:?}"),
    }
}

fn open(actor: &CollectionActor) -> NotetypesUi {
    let mut ui = NotetypesUi::new(actor, actor.media_dir()).unwrap();
    while ui.selected().is_some_and(|s| s.name != "Basic") {
        ui.on_key(actor, key(KeyCode::Char('j')));
    }
    ui
}

#[test]
fn fields_editor_renames_and_reorders_with_the_full_sync_prompt() {
    let dir = Dir::new();
    let actor = CollectionActor::open(&dir.0).unwrap();
    actor.execute(Command::GenerateStudyFixture).unwrap();
    let mut ui = open(&actor);
    let list = screen(&ui, 90, 24);
    assert!(list.contains("Basic") && list.contains("Cloze"), "{list}");
    let basic = ui.selected().unwrap().clone();
    assert!(basic.use_count > 0);
    let ids = note_ids(&actor, "note:Basic");
    let before = note(&actor, ids[0]);

    // Rename Front: no schema change, saved without any prompt.
    ui.on_key(&actor, key(KeyCode::Char('f')));
    ui.on_key(&actor, key(KeyCode::Char('n')));
    for _ in 0..5 {
        ui.on_key(&actor, key(KeyCode::Backspace));
    }
    typed(&mut ui, &actor, "Question");
    ui.on_key(&actor, key(KeyCode::Enter));
    let intent = ui.on_key(&actor, ctrl_s());
    assert!(
        matches!(intent, NotetypesIntent::Changed(c) if c.notetype),
        "{}",
        screen(&ui, 90, 24)
    );
    let d = get(&actor, basic.id);
    assert_eq!(d.fields[0].name, "Question");
    assert_eq!(d.templates[0].question, "{{Question}}");
    assert_eq!(note(&actor, ids[0]).fields, before.fields);

    // Move it below Back: the prompt appears first, the notes follow after y.
    ui.on_key(&actor, key(KeyCode::Char('J')));
    let intent = ui.on_key(&actor, ctrl_s());
    assert_eq!(intent, NotetypesIntent::None);
    let text = screen(&ui, 90, 24);
    assert!(text.contains("Full sync required"), "{text}");
    assert_eq!(
        get(&actor, basic.id).fields[0].name,
        "Question",
        "unchanged until confirmed"
    );
    let intent = ui.on_key(&actor, key(KeyCode::Char('y')));
    assert!(matches!(intent, NotetypesIntent::Changed(_)));
    let d = get(&actor, basic.id);
    assert_eq!(d.fields[0].name, "Back");
    let after = note(&actor, ids[0]);
    assert_eq!(
        after.fields,
        [before.fields[1].clone(), before.fields[0].clone()]
    );

    // The required cloze field cannot be deleted.
    ui.on_key(&actor, key(KeyCode::Esc));
    while ui.selected().is_some_and(|s| s.name != "Cloze") {
        ui.on_key(&actor, key(KeyCode::Char('j')));
    }
    ui.on_key(&actor, key(KeyCode::Char('f')));
    ui.on_key(&actor, key(KeyCode::Char('d')));
    assert!(
        ui.status().unwrap().contains("required"),
        "{:?}",
        ui.status()
    );
}

#[test]
fn templates_editor_previews_a_real_note_and_shows_anki_errors() {
    let dir = Dir::new();
    let actor = CollectionActor::open(&dir.0).unwrap();
    actor.execute(Command::GenerateStudyFixture).unwrap();
    let mut ui = open(&actor);
    let basic = ui.selected().unwrap().id;
    ui.on_key(&actor, key(KeyCode::Char('c')));
    let text = screen(&ui, 120, 24);
    assert!(text.contains("Cards of Basic"), "{text}");
    assert!(text.contains("Preview (note 1/"), "{text}");
    assert!(text.contains("Welcome to tanki"), "{text}");
    // Live preview of an uncommitted change.
    typed(&mut ui, &actor, " UNSAVED");
    let text = screen(&ui, 120, 24);
    assert!(text.contains("UNSAVED"), "{text}");
    assert_eq!(get(&actor, basic).templates[0].question, "{{Front}}");

    // A template naming an unknown field: shown inline, not saved.
    typed(&mut ui, &actor, " {{Nope}}");
    let text = screen(&ui, 120, 24);
    assert!(text.contains("Nope"), "{text}");
    let intent = ui.on_key(&actor, ctrl_s());
    assert_eq!(intent, NotetypesIntent::None);
    let text = screen(&ui, 120, 24);
    assert!(text.contains("Card 1:") && text.contains("Nope"), "{text}");
    assert_eq!(get(&actor, basic).templates[0].question, "{{Front}}");

    // Remove the bad part and save: no schema change, no prompt.
    for _ in 0..9 {
        ui.on_key(&actor, key(KeyCode::Backspace));
    }
    let intent = ui.on_key(&actor, ctrl_s());
    assert!(
        matches!(intent, NotetypesIntent::Changed(_)),
        "{}",
        screen(&ui, 120, 24)
    );
    assert_eq!(
        get(&actor, basic).templates[0].question,
        "{{Front}} UNSAVED"
    );
}

#[test]
fn change_note_type_form_changes_the_selected_notes() {
    let dir = Dir::new();
    let actor = CollectionActor::open(&dir.0).unwrap();
    actor.execute(Command::GenerateStudyFixture).unwrap();
    let ids = note_ids(&actor, "note:Basic");
    let reversed = match actor
        .execute(Command::Manage(ManageCommand::Notetypes(
            NotetypeCommand::List,
        )))
        .unwrap()
    {
        Response::Manage(ManageResponse::Notetypes(NotetypeResponse::List(l))) => {
            l.into_iter()
                .find(|n| n.name.starts_with("Basic (and reversed"))
                .unwrap()
                .id
        }
        other => panic!("{other:?}"),
    };
    let old = note(&actor, ids[0]).notetype_id;
    let chosen = vec![ids[0]];
    let mut ui = ChangeNotetypeUi::new(&actor, chosen.clone()).unwrap();
    ui.on_key(&actor, key(KeyCode::Char('t')));
    for c in "reversed".chars() {
        ui.on_key(&actor, key(KeyCode::Char(c)));
    }
    ui.on_key(&actor, key(KeyCode::Enter));
    let info = ui.info().unwrap();
    assert_eq!(info.new_notetype_id, reversed);
    assert_eq!(ui.new_fields(), [Some(0), Some(1)]);
    // The fresh fixture collection has not synced: the core asks first.
    assert_eq!(ui.on_key(&actor, key(KeyCode::Enter)), ChangeIntent::None);
    assert_eq!(note(&actor, ids[0]).notetype_id, old);
    let intent = ui.on_key(&actor, key(KeyCode::Char('y')));
    assert!(
        matches!(intent, ChangeIntent::Changed { notes: 1, .. }),
        "{intent:?}"
    );
    assert_eq!(note(&actor, ids[0]).notetype_id, reversed);
}
