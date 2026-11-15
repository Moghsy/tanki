//! The import, export and find-duplicates widgets against a real collection
//! actor on a temp profile.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{buffer::Buffer, layout::Rect};
use std::path::Path;
use tanki_core::transfer::{CsvNotetype, TextExportSelection};
use tanki_core::{
    BrowseCommand, BrowseMode, CollectionActor, Command, EditResponse, Response, SearchOrder,
};
use tanki_tui::transfer_ui::{DuplicatesUi, ExportUi, ImportUi, TransferIntent};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn screen(render: impl Fn(Rect, &mut Buffer)) -> String {
    let area = Rect::new(0, 0, 100, 26);
    let mut buf = Buffer::empty(area);
    render(area, &mut buf);
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn notes(actor: &CollectionActor, query: &str) -> Vec<i64> {
    match actor
        .execute(Command::Browse(BrowseCommand::Search {
            query: query.into(),
            mode: BrowseMode::Notes,
            order: SearchOrder::None,
        }))
        .unwrap()
    {
        Response::Edit(EditResponse::Ids(ids)) => ids,
        other => panic!("unexpected {other:?}"),
    }
}

fn write(dir: &Path, name: &str, text: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}

const TSV: &str = "#separator:tab\n#html:false\n#notetype:Basic\n#deck:Words\n#columns:Word\tMeaning\nchat\tcat\nchien\tdog\nchat\tfeline\n";

#[test]
fn csv_import_through_the_widget_then_export_and_find_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    std::fs::create_dir_all(&profile).unwrap();
    let actor = CollectionActor::open(&profile).unwrap();
    let file = write(dir.path(), "words.tsv", TSV);

    let mut ui = ImportUi::open_csv(&actor, &file).unwrap();
    assert!(matches!(
        ui.settings().notetype,
        Some(CsvNotetype::Global { .. })
    ));
    let form = screen(|a, b| ui.render(a, b));
    assert!(
        form.contains("Delimiter") && form.contains("Basic"),
        "{form}"
    );
    assert!(
        form.contains("Front") && form.contains("column 1 (Word)"),
        "{form}"
    );
    // Keep both: the file has two rows with the same first field.
    for _ in 0..10 {
        ui.on_key(key(KeyCode::Down), &actor);
    }
    ui.on_key(key(KeyCode::Right), &actor);
    ui.on_key(key(KeyCode::Right), &actor);
    assert_eq!(ui.on_key(key(KeyCode::Enter), &actor), TransferIntent::None);
    let mut spins = 0;
    while ui.is_running() {
        ui.tick(&actor);
        spins += 1;
        assert!(spins < 2000, "the import did not finish");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let log = screen(|a, b| ui.render(a, b));
    assert!(log.contains("3 notes found"), "{log}");
    match ui.on_key(key(KeyCode::Esc), &actor) {
        TransferIntent::Done { changes, message } => {
            assert!(changes.note);
            assert!(message.unwrap().contains("notes found"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(notes(&actor, "deck:Words").len(), 3);

    // Export the deck as text.
    let out = dir.path().join("words-out.txt");
    let mut export = ExportUi::new(
        "Deck Words",
        TextExportSelection::Query("deck:Words".into()),
        out.to_str().unwrap(),
    );
    export.on_key(key(KeyCode::Enter), &actor);
    let mut spins = 0;
    let done = loop {
        match export.tick(&actor) {
            TransferIntent::None => {
                spins += 1;
                assert!(spins < 2000);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            other => break other,
        }
    };
    match done {
        TransferIntent::Done { message, .. } => {
            assert!(message.unwrap().starts_with("Exported 3 notes to "));
        }
        other => panic!("{other:?}"),
    }
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("chien") && text.contains("feline"), "{text}");

    // Find duplicates on Front: "chat" appears twice.
    let mut dupes = DuplicatesUi::open(&actor, "deck:Words").unwrap();
    assert_eq!(
        dupes.on_key(key(KeyCode::Enter), &actor),
        TransferIntent::None
    );
    assert_eq!(dupes.groups().len(), 1);
    assert_eq!(dupes.groups()[0].value, "chat");
    let shown = screen(|a, b| dupes.render(a, b));
    assert!(
        shown.contains("chat") && shown.contains("2 in 1 groups"),
        "{shown}"
    );
    let ids = dupes.groups()[0].note_ids.clone();
    let expected = format!("nid:{},{}", ids[0], ids[1]);
    assert_eq!(
        dupes.on_key(key(KeyCode::Enter), &actor),
        TransferIntent::BrowseSearch(expected.clone())
    );
    assert_eq!(notes(&actor, &expected).len(), 2);
    match dupes.on_key(key(KeyCode::Char('t')), &actor) {
        TransferIntent::Changed(changes) => assert!(changes.note || changes.tag),
        other => panic!("{other:?}"),
    }
    assert_eq!(notes(&actor, "tag:duplicate").len(), 2);
}

#[test]
fn json_import_through_the_widget() {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    std::fs::create_dir_all(&profile).unwrap();
    let actor = CollectionActor::open(&profile).unwrap();
    let file = write(
        dir.path(),
        "data.json",
        r#"{"notes":[{"fields":["one","uno"],"notetype":"Basic","deck":"Spanish"}]}"#,
    );
    let mut ui = ImportUi::open_json(&actor, &file).unwrap();
    let mut spins = 0;
    while ui.is_running() {
        ui.tick(&actor);
        spins += 1;
        assert!(spins < 2000);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(matches!(
        ui.on_key(key(KeyCode::Enter), &actor),
        TransferIntent::Done { .. }
    ));
    assert_eq!(notes(&actor, "deck:Spanish").len(), 1);

    // A bad file reports the error and closes on Esc.
    let bad = write(dir.path(), "bad.json", "{");
    let mut ui = ImportUi::open_json(&actor, &bad).unwrap();
    let mut intent = TransferIntent::None;
    for _ in 0..2000 {
        intent = ui.tick(&actor);
        if !ui.is_running() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(matches!(intent, TransferIntent::Message(_)), "{intent:?}");
}
