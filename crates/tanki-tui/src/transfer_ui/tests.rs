//! Cell-buffer snapshots and flows against a fake backend that answers the
//! transfer commands.
use super::*;
use crate::backend::Result as BackendResult;
use crate::testing::buffer_text;
use crossterm::event::KeyEvent;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use tanki_core::transfer::{
    CsvDeck, CsvDelimiter, CsvMetadataInfo, CsvNotetype, CsvSettings, DuplicateGroup,
    TextExportSelection,
};
use tanki_core::{DeckName, EditResponse, ImportLog, NoteCommand, NotetypeInfo};

type Job = BackendResult<TransferResponse>;

#[derive(Default)]
struct Fake {
    /// Commands answered at once through `execute`.
    executed: RefCell<Vec<TransferCommand>>,
    /// Commands queued through `start`.
    started: RefCell<Vec<TransferCommand>>,
    /// Results of started commands, oldest first.
    jobs: RefCell<VecDeque<Job>>,
    /// Polls that answer "still working" before a job finishes.
    busy_polls: Cell<u32>,
    cancelled: Cell<bool>,
    groups: RefCell<Vec<DuplicateGroup>>,
}

struct Op {
    result: RefCell<Option<Job>>,
    busy: Cell<u32>,
}

impl CommandOp for Op {
    fn poll(&self) -> BackendResult<Option<Response>> {
        if self.busy.get() > 0 {
            self.busy.set(self.busy.get() - 1);
            return Ok(None);
        }
        match self.result.borrow_mut().take() {
            Some(Ok(r)) => Ok(Some(Response::Manage(ManageResponse::Transfer(r)))),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }
}

fn info(delimiter: CsvDelimiter, notetype: i64) -> CsvMetadataInfo {
    CsvMetadataInfo {
        settings: CsvSettings {
            delimiter,
            column_labels: vec!["Word".into(), "Meaning".into(), "Tags".into()],
            deck: Some(CsvDeck::Name("French".into())),
            notetype: Some(CsvNotetype::Global {
                id: notetype,
                field_columns: vec![1, 2],
            }),
            ..Default::default()
        },
        preview: vec![
            vec!["bonjour".into(), "hello".into(), "polite".into()],
            vec!["merci".into(), "thanks".into(), String::new()],
        ],
        notetype_name: Some(if notetype == 1 { "Basic" } else { "Cloze" }.into()),
        field_names: if notetype == 1 {
            vec!["Front".into(), "Back".into()]
        } else {
            vec!["Text".into(), "Back Extra".into()]
        },
    }
}

impl Backend for Fake {
    fn execute(&self, command: Command) -> BackendResult<Response> {
        Ok(match command {
            Command::Notes(NoteCommand::Notetypes) => Response::Edit(EditResponse::Notetypes(
                (1..=2)
                    .map(|id| NotetypeInfo {
                        id,
                        name: if id == 1 { "Basic" } else { "Cloze" }.into(),
                        ..Default::default()
                    })
                    .collect(),
            )),
            Command::Notes(NoteCommand::DeckNames { .. }) => {
                Response::Edit(EditResponse::DeckNames(vec![DeckName {
                    id: 7,
                    name: "French".into(),
                    filtered: false,
                }]))
            }
            Command::Manage(ManageCommand::Transfer(command)) => {
                self.executed.borrow_mut().push(command.clone());
                let response = match command {
                    TransferCommand::CsvMetadata(request) => {
                        let delimiter = request.delimiter.unwrap_or(CsvDelimiter::Tab);
                        TransferResponse::CsvMetadata(Box::new(info(
                            delimiter,
                            request.notetype_id.unwrap_or(1),
                        )))
                    }
                    TransferCommand::DuplicateFieldNames => {
                        TransferResponse::FieldNames(vec!["Back".into(), "Front".into()])
                    }
                    TransferCommand::FindDuplicates { .. } => {
                        TransferResponse::Duplicates(self.groups.borrow().clone())
                    }
                    TransferCommand::TagDuplicates { note_ids } => TransferResponse::Tagged {
                        count: note_ids.len(),
                        changes: OpChanges {
                            note: true,
                            ..Default::default()
                        },
                    },
                    other => panic!("not executed synchronously: {other:?}"),
                };
                Response::Manage(ManageResponse::Transfer(response))
            }
            other => panic!("unexpected {other:?}"),
        })
    }
    fn start(&self, command: Command) -> BackendResult<Box<dyn CommandOp>> {
        let Command::Manage(ManageCommand::Transfer(command)) = command else {
            panic!("unexpected {command:?}")
        };
        self.started.borrow_mut().push(command);
        let result = self.jobs.borrow_mut().pop_front().expect("a queued result");
        Ok(Box::new(Op {
            result: RefCell::new(Some(result)),
            busy: Cell::new(self.busy_polls.get()),
        }))
    }
    fn data_progress(&self) -> OperationProgress {
        OperationProgress {
            stage: "Importing notes".into(),
            current: Some(1),
            total: Some(2),
        }
    }
    fn cancel_data(&self) {
        self.cancelled.set(true);
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn press(ui: &mut ImportUi, fake: &Fake, codes: &[KeyCode]) {
    for code in codes {
        ui.on_key(key(*code), fake);
    }
}

fn imported(new: usize) -> Job {
    let mut log = ImportLog {
        new,
        found_notes: new as u32 + 1,
        duplicate: 1,
        ..Default::default()
    };
    log.entries
        .insert("new".into(), vec![vec!["bonjour".into(), "hello".into()]]);
    log.entries.insert(
        "first_field_match".into(),
        vec![vec!["merci".into(), "thanks".into()]],
    );
    Ok(TransferResponse::Imported {
        log,
        changes: OpChanges {
            note: true,
            ..Default::default()
        },
    })
}

fn draw(render: impl Fn(Rect, &mut Buffer), width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    render(area, &mut buf);
    buffer_text(&buf).join("\n").trim_end().to_owned()
}

#[test]
fn mapping_form_snapshot() {
    let fake = Fake::default();
    let ui = ImportUi::open_csv(&fake, "/tmp/words.tsv").unwrap();
    let text = draw(|a, b| ui.render(a, b), 100, 24);
    assert_screen("csv_form", &text, SNAP_CSV_FORM);
}

#[test]
fn form_edits_reload_metadata_and_start_the_import_with_the_edited_mapping() {
    let fake = Fake::default();
    let mut ui = ImportUi::open_csv(&fake, "/tmp/words.tsv").unwrap();
    // Delimiter row: Right selects the next delimiter and re-reads the file.
    press(&mut ui, &fake, &[KeyCode::Right]);
    assert_eq!(ui.settings().delimiter, CsvDelimiter::Pipe);
    let TransferCommand::CsvMetadata(request) = fake.executed.borrow().last().unwrap().clone()
    else {
        panic!()
    };
    assert_eq!(request.delimiter, Some(CsvDelimiter::Pipe));
    // Note type row: Right picks the next type and maps its fields.
    press(
        &mut ui,
        &fake,
        &[KeyCode::Down, KeyCode::Down, KeyCode::Right],
    );
    assert!(matches!(
        ui.settings().notetype,
        Some(CsvNotetype::Global { id: 2, .. })
    ));
    // Deck row: replace the text.
    press(&mut ui, &fake, &[KeyCode::Down]);
    ui.on_key(
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        &fake,
    );
    for c in "Spanish".chars() {
        ui.on_key(key(KeyCode::Char(c)), &fake);
    }
    // First field row: Right moves it from column 1 to column 2.
    press(&mut ui, &fake, &[KeyCode::Down, KeyCode::Right]);
    // Duplicate handling: down to "If a note matches" (fields, tags column,
    // guid column, two tag boxes) and pick the next choice.
    press(&mut ui, &fake, &[KeyCode::Down; 6]);
    press(&mut ui, &fake, &[KeyCode::Right]);

    fake.jobs.borrow_mut().push_back(imported(1));
    fake.busy_polls.set(1);
    ui.on_key(key(KeyCode::Enter), &fake);
    assert!(ui.is_running());
    let TransferCommand::ImportCsv { path, settings } = fake.started.borrow()[0].clone() else {
        panic!()
    };
    assert_eq!(path.to_str(), Some("/tmp/words.tsv"));
    assert_eq!(settings.deck, Some(CsvDeck::Name("Spanish".into())));
    assert_eq!(
        settings.notetype,
        Some(CsvNotetype::Global {
            id: 2,
            field_columns: vec![2, 2]
        })
    );
    assert_eq!(
        settings.dupe_resolution,
        tanki_core::transfer::DupeResolution::Preserve
    );
    let running = draw(|a, b| ui.render(a, b), 100, 12);
    assert_screen("csv_running", &running, SNAP_CSV_RUNNING);
    assert_eq!(ui.tick(&fake), TransferIntent::None);
    assert!(ui.is_running());
    assert_eq!(ui.tick(&fake), TransferIntent::None);
    let log = draw(|a, b| ui.render(a, b), 100, 14);
    assert_screen("csv_log", &log, SNAP_CSV_LOG);
    // Right shows the next list; Esc finishes with the changes.
    press(&mut ui, &fake, &[KeyCode::Right]);
    match ui.on_key(key(KeyCode::Esc), &fake) {
        TransferIntent::Done { changes, message } => {
            assert!(changes.note);
            assert!(message.unwrap().contains("2 notes found: 1 new"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn esc_cancels_a_running_import_and_returns_to_the_form() {
    let fake = Fake::default();
    let mut ui = ImportUi::open_csv(&fake, "/tmp/words.tsv").unwrap();
    fake.jobs
        .borrow_mut()
        .push_back(Err(CoreError::Interrupted));
    fake.busy_polls.set(1);
    ui.on_key(key(KeyCode::Enter), &fake);
    assert_eq!(ui.tick(&fake), TransferIntent::None);
    assert_eq!(ui.on_key(key(KeyCode::Esc), &fake), TransferIntent::None);
    assert!(fake.cancelled.get());
    let intent = ui.tick(&fake);
    assert!(matches!(intent, TransferIntent::Message(m) if m.contains("cancelled")));
    assert!(!ui.is_running());
    // Back in the form: Esc closes.
    assert_eq!(ui.on_key(key(KeyCode::Esc), &fake), TransferIntent::Close);
}

#[test]
fn import_errors_stay_in_the_form() {
    let fake = Fake::default();
    let mut ui = ImportUi::open_csv(&fake, "/tmp/words.tsv").unwrap();
    fake.jobs
        .borrow_mut()
        .push_back(Err(CoreError::Invalid("No deck".into())));
    ui.on_key(key(KeyCode::Enter), &fake);
    assert!(matches!(ui.tick(&fake), TransferIntent::Message(m) if m == "No deck"));
    let text = draw(|a, b| ui.render(a, b), 100, 24);
    assert!(text.contains("No deck"), "{text}");
}

#[test]
fn json_import_confirms_a_schema_change() {
    let fake = Fake::default();
    fake.jobs
        .borrow_mut()
        .push_back(Err(CoreError::SchemaChangeRequired));
    fake.jobs.borrow_mut().push_back(imported(3));
    let mut ui = ImportUi::open_json(&fake, "/tmp/data.json").unwrap();
    assert!(ui.is_running());
    assert_eq!(ui.tick(&fake), TransferIntent::None);
    let text = draw(|a, b| ui.render(a, b), 90, 12);
    assert_screen("json_schema_confirm", &text, SNAP_JSON_SCHEMA_CONFIRM);
    ui.on_key(key(KeyCode::Char('y')), &fake);
    assert!(matches!(
        fake.started.borrow()[1],
        TransferCommand::ImportJson {
            schema_confirmed: true,
            ..
        }
    ));
    ui.tick(&fake);
    assert!(matches!(
        ui.on_key(key(KeyCode::Enter), &fake),
        TransferIntent::Done { .. }
    ));
}

#[test]
fn json_import_schema_refusal_can_be_declined() {
    let fake = Fake::default();
    fake.jobs
        .borrow_mut()
        .push_back(Err(CoreError::SchemaChangeRequired));
    let mut ui = ImportUi::open_json(&fake, "/tmp/data.json").unwrap();
    ui.tick(&fake);
    assert_eq!(
        ui.on_key(key(KeyCode::Char('n')), &fake),
        TransferIntent::Close
    );
    assert_eq!(fake.started.borrow().len(), 1);
}

#[test]
fn export_form_snapshot_and_run() {
    let fake = Fake::default();
    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("notes.txt");
    std::fs::write(&existing, "x").unwrap();
    let shown = ExportUi::new(
        "Deck French",
        TextExportSelection::Deck(7),
        "/tmp/notes.txt",
    );
    let text = draw(|a, b| shown.render(a, b), 100, 12);
    assert_screen("export_form", &text, SNAP_EXPORT_FORM);
    let mut ui = ExportUi::new(
        "Deck French",
        TextExportSelection::Deck(7),
        existing.to_str().unwrap(),
    );

    // The file exists: the first Enter only warns.
    ui.on_key(key(KeyCode::Enter), &fake);
    assert!(fake.started.borrow().is_empty());
    // Kind row, Space: cards.
    ui.on_key(key(KeyCode::Down), &fake);
    ui.on_key(key(KeyCode::Char(' ')), &fake);
    fake.jobs
        .borrow_mut()
        .push_back(Ok(TransferResponse::Exported { count: 4 }));
    ui.on_key(key(KeyCode::Enter), &fake);
    ui.on_key(key(KeyCode::Enter), &fake);
    assert!(ui.is_running());
    assert!(matches!(
        fake.started.borrow()[0],
        TransferCommand::ExportCards {
            selection: TextExportSelection::Deck(7),
            ..
        }
    ));
    match ui.tick(&fake) {
        TransferIntent::Done { message, .. } => {
            assert!(message.unwrap().starts_with("Exported 4 cards to "));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn export_of_note_ids_cannot_switch_to_cards() {
    let fake = Fake::default();
    let mut ui = ExportUi::new(
        "2 notes",
        TextExportSelection::NoteIds(vec![1, 2]),
        "/tmp/n.txt",
    );
    ui.on_key(key(KeyCode::Down), &fake);
    ui.on_key(key(KeyCode::Char(' ')), &fake);
    fake.jobs
        .borrow_mut()
        .push_back(Ok(TransferResponse::Exported { count: 2 }));
    ui.on_key(key(KeyCode::Enter), &fake);
    assert!(matches!(
        fake.started.borrow()[0],
        TransferCommand::ExportNotes { .. }
    ));
}

fn group(value: &str, ids: &[i64]) -> DuplicateGroup {
    DuplicateGroup {
        value: value.into(),
        note_ids: ids.to_vec(),
    }
}

#[test]
fn duplicates_flow_lists_groups_and_returns_a_browser_search() {
    let fake = Fake::default();
    *fake.groups.borrow_mut() = vec![group("cat", &[3, 4]), group("dog", &[1, 2, 9])];
    let mut ui = DuplicatesUi::open(&fake, "deck:French").unwrap();
    let form = draw(|a, b| ui.render(a, b), 90, 10);
    assert_screen("duplicates_form", &form, SNAP_DUPLICATES_FORM);
    assert_eq!(ui.on_key(key(KeyCode::Enter), &fake), TransferIntent::None);
    let TransferCommand::FindDuplicates { field_name, search } =
        fake.executed.borrow().last().unwrap().clone()
    else {
        panic!()
    };
    assert_eq!(
        (field_name.as_str(), search.as_str()),
        ("Front", "deck:French")
    );
    let results = draw(|a, b| ui.render(a, b), 90, 12);
    assert_screen("duplicates_results", &results, SNAP_DUPLICATES_RESULTS);
    ui.on_key(key(KeyCode::Down), &fake);
    assert_eq!(
        ui.on_key(key(KeyCode::Enter), &fake),
        TransferIntent::BrowseSearch("nid:1,2,9".into())
    );
    match ui.on_key(key(KeyCode::Char('t')), &fake) {
        TransferIntent::Changed(changes) => assert!(changes.note),
        other => panic!("{other:?}"),
    }
    let TransferCommand::TagDuplicates { note_ids } =
        fake.executed.borrow().last().unwrap().clone()
    else {
        panic!()
    };
    assert_eq!(note_ids, [3, 4, 1, 2, 9]);
    // Esc goes back to the form, then closes.
    assert_eq!(ui.on_key(key(KeyCode::Esc), &fake), TransferIntent::None);
    assert_eq!(ui.on_key(key(KeyCode::Esc), &fake), TransferIntent::Close);
}

#[test]
fn duplicates_none_found_stays_on_the_form() {
    let fake = Fake::default();
    let mut ui = DuplicatesUi::open(&fake, "").unwrap();
    assert!(matches!(
        ui.on_key(key(KeyCode::Enter), &fake),
        TransferIntent::Message(m) if m == "No duplicates found"
    ));
    assert!(ui.groups().is_empty());
    // Right on the field row changes the field.
    ui.on_key(key(KeyCode::Right), &fake);
    ui.on_key(key(KeyCode::Enter), &fake);
    let TransferCommand::FindDuplicates { field_name, .. } =
        fake.executed.borrow().last().unwrap().clone()
    else {
        panic!()
    };
    assert_eq!(field_name, "Back");
}

fn assert_screen(name: &str, actual: &str, expected: &str) {
    let expected = expected.strip_prefix('\n').unwrap_or(expected).trim_end();
    if actual != expected {
        println!("=== {name}\n{actual}\n=== end");
    }
    assert!(
        actual == expected,
        "screen {name} differs\n--- actual ---\n{actual}\n--- expected ---\n{expected}"
    );
}

const SNAP_CSV_FORM: &str = r#"


  ┌ Import words.tsv ────────────────────────────────────────────────────────────────────────────┐
  │Delimiter          < Tab >                                                                    │
  │Fields hold HTML   [ ]                                                                        │
  │Note type          Basic                                                                      │
  │Deck               French                                                                     │
  │  Front            column 1 (Word) e.g. bonjour                                               │
  │  Back             column 2 (Meaning) e.g. hello                                              │
  │Tags column        (none)                                                                     │
  │GUID column        (none)                                                                     │
  │Tags for all                                                                                  │
  │Tags if updated                                                                               │
  │If a note matches  Update current                                                             │
  │Match within       Note type                                                                  │
  │                                                                                              │
  │Preview                                                                                       │
  │1: bonjour | 2: hello | 3: polite                                                             │
  │1: merci | 2: thanks | 3:                                                                     │
  │Enter imports, Esc closes                                                                     │
  └──────────────────────────────────────────────────────────────────────────────────────────────┘
"#;

const SNAP_CSV_RUNNING: &str = r#"



                    ┌ Import words.tsv ────────────────────────────────────────┐
                    │Working…                                                  │
                    │                                                          │
                    │Esc cancels                                               │
                    └──────────────────────────────────────────────────────────┘
"#;

const SNAP_CSV_LOG: &str = r#"
     ┌ Import words.tsv: result ──────────────────────────────────────────────────────────────┐
     │2 notes found: 1 new, 0 updated, 1 duplicate, 1 first field match                       │
     │                                                                                        │
     │[new 1]  first field match 1                                                            │
     │                                                                                        │
     │bonjour | hello                                                                         │
     │                                                                                        │
     │                                                                                        │
     │                                                                                        │
     │                                                                                        │
     │                                                                                        │
     │                                                                                        │
     │Left/Right list, Up/Down scroll, Esc closes                                             │
     └────────────────────────────────────────────────────────────────────────────────────────┘
"#;

const SNAP_JSON_SCHEMA_CONFIRM: &str = r#"


          ┌ Import data.json ──────────────────────────────────────────────────┐
          │This file adds note types, which requires a one-way full sync       │
          │(upload) on your next sync; every other device must then download.  │
          │                                                                    │
          │                                                                    │
          │                                                                    │
          │y imports, any other key cancels                                    │
          └────────────────────────────────────────────────────────────────────┘
"#;

const SNAP_EXPORT_FORM: &str = r#"
          ┌ Export Deck French ──────────────────────────────────────────────────────────┐
          │File              /tmp/notes.txt                                              │
          │Export            notes                                                       │
          │Include HTML      [ ]                                                         │
          │Tags column       [x]                                                         │
          │Deck column       [ ]                                                         │
          │Note type column  [ ]                                                         │
          │GUID column       [ ]                                                         │
          │                                                                              │
          │Enter exports, Esc closes                                                     │
          └──────────────────────────────────────────────────────────────────────────────┘
"#;

const SNAP_DUPLICATES_FORM: &str = r#"

          ┌ Find Duplicates ───────────────────────────────────────────────────┐
          │Search in       < Front >                                           │
          │Optional filter deck:French                                         │
          │                                                                    │
          │                                                                    │
          │Enter finds, Esc closes                                             │
          └────────────────────────────────────────────────────────────────────┘
"#;

const SNAP_DUPLICATES_RESULTS: &str = r#"


     ┌ Duplicates ──────────────────────────────────────────────────────────────────┐
     │5 in 2 groups (Front)                                                         │
     │                                                                              │
     │  2  cat                                                                      │
     │  3  dog                                                                      │
     │                                                                              │
     │Enter shows a group, t tags all, Esc goes back                                │
     └──────────────────────────────────────────────────────────────────────────────┘
"#;
