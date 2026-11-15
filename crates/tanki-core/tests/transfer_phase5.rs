//! Import/export extras: CSV metadata and import, JSON import, text exports
//! and find duplicates, through the actor on temp collections.
use std::path::{Path, PathBuf};
use tanki_core::transfer::*;
use tanki_core::*;

struct Fixture {
    dir: tempfile::TempDir,
    actor: CollectionActor,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    std::fs::create_dir_all(&profile).unwrap();
    let actor = CollectionActor::open(&profile);
    Fixture {
        actor: actor.unwrap(),
        dir,
    }
}

impl Fixture {
    fn file(&self, name: &str, text: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::write(&path, text).unwrap();
        path
    }
    fn out(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

fn transfer(actor: &CollectionActor, command: TransferCommand) -> Result<TransferResponse> {
    match actor.execute(Command::Manage(ManageCommand::Transfer(command)))? {
        Response::Manage(ManageResponse::Transfer(r)) => Ok(r),
        other => panic!("unexpected {other:?}"),
    }
}

fn metadata(actor: &CollectionActor, path: &Path) -> Result<CsvMetadataInfo> {
    let request = CsvMetadataRequest {
        path: path.into(),
        ..Default::default()
    };
    match transfer(actor, TransferCommand::CsvMetadata(request))? {
        TransferResponse::CsvMetadata(info) => Ok(*info),
        other => panic!("unexpected {other:?}"),
    }
}

fn import_csv(actor: &CollectionActor, path: &Path, settings: CsvSettings) -> ImportLog {
    match transfer(
        actor,
        TransferCommand::ImportCsv {
            path: path.into(),
            settings,
        },
    )
    .unwrap()
    {
        TransferResponse::Imported { log, .. } => log,
        other => panic!("unexpected {other:?}"),
    }
}

fn search(actor: &CollectionActor, query: &str) -> Vec<i64> {
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

fn notetype(actor: &CollectionActor, name: &str) -> NotetypeInfo {
    let Response::Edit(EditResponse::Notetypes(all)) = actor
        .execute(Command::Notes(NoteCommand::Notetypes))
        .unwrap()
    else {
        panic!("notetypes")
    };
    all.into_iter().find(|nt| nt.name == name).unwrap()
}

fn add_note(actor: &CollectionActor, notetype: &str, fields: &[&str]) -> i64 {
    let nt = notetype_id(actor, notetype);
    let Response::Edit(EditResponse::Note(mut note)) = actor
        .execute(Command::Notes(NoteCommand::NewNote { notetype_id: nt }))
        .unwrap()
    else {
        panic!("note")
    };
    for (slot, text) in note.fields.iter_mut().zip(fields) {
        *slot = text.to_string();
    }
    match actor
        .execute(Command::Notes(NoteCommand::AddNote { note, deck_id: 1 }))
        .unwrap()
    {
        Response::Edit(EditResponse::NoteAdded { id, .. }) => id,
        other => panic!("unexpected {other:?}"),
    }
}

fn notetype_id(actor: &CollectionActor, name: &str) -> i64 {
    notetype(actor, name).id
}

fn note_fields(actor: &CollectionActor, id: i64) -> Vec<String> {
    match actor
        .execute(Command::Notes(NoteCommand::GetNote { id }))
        .unwrap()
    {
        Response::Edit(EditResponse::Note(n)) => n.fields,
        other => panic!("unexpected {other:?}"),
    }
}

const DIRECTIVES: &str = "#separator:semicolon\n#html:false\n#notetype:Basic\n#deck:Imported\n#tags:batch\n#columns:Front;Back;Tags\n#tags column:3\nhello;world;a b\nsecond;row;\n";

#[test]
fn metadata_reads_header_directives_and_previews() {
    let f = fixture();
    let path = f.file("notes.txt", DIRECTIVES);
    let info = metadata(&f.actor, &path).unwrap();
    let s = &info.settings;
    assert_eq!(s.delimiter, CsvDelimiter::Semicolon);
    assert!(!s.is_html);
    assert_eq!(s.global_tags, ["batch"]);
    assert_eq!(s.tags_column, 3);
    assert_eq!(s.column_labels, ["Front", "Back", "Tags"]);
    assert_eq!(s.deck, Some(CsvDeck::Name("Imported".into())));
    assert_eq!(info.notetype_name.as_deref(), Some("Basic"));
    assert_eq!(info.field_names, ["Front", "Back"]);
    assert_eq!(
        s.notetype,
        Some(CsvNotetype::Global {
            id: notetype_id(&f.actor, "Basic"),
            field_columns: vec![1, 2]
        })
    );
    assert_eq!(info.preview.len(), 2);
    assert_eq!(info.preview[0], ["hello", "world", "a b"]);
}

#[test]
fn delimiter_is_detected_and_can_be_overridden() {
    let f = fixture();
    let comma = f.file("c.csv", "one,two\nthree,four\n");
    assert_eq!(
        metadata(&f.actor, &comma).unwrap().settings.delimiter,
        CsvDelimiter::Comma
    );
    let tab = f.file("t.tsv", "one\ttwo,x\nthree\tfour,y\n");
    assert_eq!(
        metadata(&f.actor, &tab).unwrap().settings.delimiter,
        CsvDelimiter::Tab
    );
    let forced = TransferCommand::CsvMetadata(CsvMetadataRequest {
        path: tab,
        delimiter: Some(CsvDelimiter::Comma),
        ..Default::default()
    });
    let TransferResponse::CsvMetadata(info) = transfer(&f.actor, forced).unwrap() else {
        panic!()
    };
    assert_eq!(info.settings.delimiter, CsvDelimiter::Comma);
    assert_eq!(info.preview[0].len(), 2);
}

#[test]
fn bad_inputs_are_refused() {
    let f = fixture();
    assert!(matches!(
        metadata(&f.actor, &f.out("missing.txt")),
        Err(CoreError::Invalid(_))
    ));
    let empty = f.file("empty.txt", "");
    assert!(metadata(&f.actor, &empty).is_err());
}

#[test]
fn csv_import_maps_columns_and_handles_duplicates() {
    let f = fixture();
    let path = f.file("notes.txt", DIRECTIVES);
    let info = metadata(&f.actor, &path).unwrap();
    let log = import_csv(&f.actor, &path, info.settings.clone());
    assert_eq!((log.new, log.found_notes), (2, 2));
    let ids = search(&f.actor, "deck:Imported");
    assert_eq!(ids.len(), 2);
    assert_eq!(search(&f.actor, "tag:batch tag:a tag:b").len(), 1);
    assert_eq!(search(&f.actor, "tag:batch").len(), 2);

    // Same file again, keeping the current notes.
    let mut settings = info.settings.clone();
    settings.dupe_resolution = DupeResolution::Preserve;
    let log = import_csv(&f.actor, &path, settings.clone());
    assert_eq!(log.new, 0);
    assert_eq!(log.entries["first_field_match"].len(), 2);

    // Changed second field, updating the current note.
    let changed = f.file("changed.txt", &DIRECTIVES.replace("world", "planet"));
    settings.dupe_resolution = DupeResolution::Update;
    let log = import_csv(&f.actor, &changed, settings.clone());
    assert_eq!(log.new, 0);
    assert_eq!(log.entries["first_field_match"].len(), 2);
    assert_eq!(search(&f.actor, "deck:Imported").len(), 2);
    assert_eq!(search(&f.actor, "planet").len(), 1);

    // Keep both.
    settings.dupe_resolution = DupeResolution::Duplicate;
    let log = import_csv(&f.actor, &path, settings);
    assert_eq!(log.found_notes, 2, "{log:?}");
    assert_eq!(search(&f.actor, "deck:Imported").len(), 4);
}

#[test]
fn edited_mapping_swaps_fields() {
    let f = fixture();
    let path = f.file("swap.txt", "#separator:tab\n#html:false\nq1\ta1\nq2\ta2\n");
    let mut settings = metadata(&f.actor, &path).unwrap().settings;
    settings.notetype = Some(CsvNotetype::Global {
        id: notetype_id(&f.actor, "Basic"),
        field_columns: vec![2, 1],
    });
    settings.deck = Some(CsvDeck::Name("Swapped".into()));
    let log = import_csv(&f.actor, &path, settings);
    assert_eq!(log.new, 2);
    let ids = search(&f.actor, "deck:Swapped");
    let mut fronts: Vec<String> = ids
        .iter()
        .map(|id| note_fields(&f.actor, *id)[0].clone())
        .collect();
    fronts.sort();
    assert_eq!(fronts, ["a1", "a2"]);
}

#[test]
fn csv_import_runs_as_a_long_operation_and_is_undoable() {
    let f = fixture();
    let path = f.file("notes.txt", DIRECTIVES);
    let info = metadata(&f.actor, &path).unwrap();
    let pending = f
        .actor
        .start(Command::Manage(ManageCommand::Transfer(
            TransferCommand::ImportCsv {
                path,
                settings: info.settings,
            },
        )))
        .unwrap();
    let Response::Manage(ManageResponse::Transfer(TransferResponse::Imported { log, changes })) =
        pending.wait().unwrap()
    else {
        panic!("imported")
    };
    assert_eq!(log.new, 2);
    assert!(changes.note);
    assert_eq!(search(&f.actor, "deck:Imported").len(), 2);
    f.actor.execute(Command::Undo).unwrap();
    assert!(search(&f.actor, "deck:Imported").is_empty());
}

#[test]
fn json_import_adds_notes() {
    let f = fixture();
    let json = r#"{"notes":[
        {"fields":["json front","json back"],"notetype":"Basic","deck":"FromJson","tags":["j"]},
        {"fields":["second",null],"notetype":"Basic","deck":"FromJson"}]}"#;
    let path = f.file("data.json", json);
    let TransferResponse::Imported { log, changes } = transfer(
        &f.actor,
        TransferCommand::ImportJson {
            path,
            schema_confirmed: false,
        },
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(log.new, 2);
    assert!(changes.note);
    assert_eq!(search(&f.actor, "deck:FromJson").len(), 2);
    assert_eq!(search(&f.actor, "tag:j").len(), 1);
    let bad = f.file("bad.json", "not json");
    assert!(transfer(
        &f.actor,
        TransferCommand::ImportJson {
            path: bad,
            schema_confirmed: false
        }
    )
    .is_err());
}

fn export_notes(f: &Fixture, name: &str, selection: TextExportSelection) -> Result<(u32, String)> {
    let path = f.out(name);
    let response = transfer(
        &f.actor,
        TransferCommand::ExportNotes {
            path: path.clone(),
            selection,
            with_html: false,
            with_tags: true,
            with_deck: true,
            with_notetype: true,
            with_guid: true,
        },
    )?;
    let TransferResponse::Exported { count } = response else {
        panic!()
    };
    Ok((count, std::fs::read_to_string(path).unwrap()))
}

#[test]
fn notes_export_round_trips_through_the_importer() {
    let f = fixture();
    let path = f.file("notes.txt", DIRECTIVES);
    let settings = metadata(&f.actor, &path).unwrap().settings;
    import_csv(&f.actor, &path, settings);

    let (count, text) = export_notes(&f, "out.txt", TextExportSelection::All).unwrap();
    assert_eq!(count, 2);
    assert!(text.contains("#separator:tab"));
    assert!(text.contains("#guid column:1"));
    assert!(text.contains("hello"));

    let (one, _) = export_notes(&f, "one.txt", TextExportSelection::Query("hello".into())).unwrap();
    assert_eq!(one, 1);
    let (deck, _) = export_notes(&f, "deck.tsv", TextExportSelection::Deck(1)).unwrap();
    assert_eq!(deck, 0);

    // The exported file is importable and matches the existing notes.
    let info = metadata(&f.actor, &f.out("out.txt")).unwrap();
    assert_eq!(info.settings.guid_column, 1);
    let mut settings = info.settings;
    settings.dupe_resolution = DupeResolution::Preserve;
    let log = import_csv(&f.actor, &f.out("out.txt"), settings);
    assert_eq!(log.new, 0);
    assert_eq!(log.entries["first_field_match"].len(), 2);
}

#[test]
fn text_exports_refuse_bad_targets_and_empty_selections() {
    let f = fixture();
    add_note(&f.actor, "Basic", &["x", "y"]);
    for name in ["out.apkg", "collection.anki2", "out"] {
        assert!(
            export_notes(&f, name, TextExportSelection::All).is_err(),
            "{name}"
        );
    }
    assert!(export_notes(
        &f,
        "a.txt",
        TextExportSelection::Query("nomatchatall".into())
    )
    .is_err());
    assert!(export_notes(&f, "b.txt", TextExportSelection::NoteIds(vec![])).is_err());
    let nid = search(&f.actor, "x")[0];
    let (count, _) = export_notes(&f, "c.txt", TextExportSelection::NoteIds(vec![nid])).unwrap();
    assert_eq!(count, 1);
}

#[test]
fn cards_export_writes_question_and_answer() {
    let f = fixture();
    add_note(&f.actor, "Basic", &["capital of France", "Paris"]);
    let path = f.out("cards.txt");
    let TransferResponse::Exported { count } = transfer(
        &f.actor,
        TransferCommand::ExportCards {
            path: path.clone(),
            selection: TextExportSelection::Query("Paris".into()),
            with_html: false,
        },
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(count, 1);
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.contains("#separator:tab"));
    assert!(text.contains("capital of France"));
    assert!(text.contains("Paris"));
}

fn dupes(actor: &CollectionActor, field: &str, search: &str) -> Vec<DuplicateGroup> {
    match transfer(
        actor,
        TransferCommand::FindDuplicates {
            field_name: field.into(),
            search: search.into(),
        },
    )
    .unwrap()
    {
        TransferResponse::Duplicates(groups) => groups,
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn find_duplicates_groups_by_stripped_field_text() {
    let f = fixture();
    let a = add_note(&f.actor, "Basic", &["dog", "1"]);
    let b = add_note(&f.actor, "Basic (and reversed card)", &["<b>dog</b>", "2"]);
    let c = add_note(&f.actor, "Basic", &["cat", "3"]);
    let d = add_note(
        &f.actor,
        "Basic (optional reversed card)",
        &["cat&nbsp;", "4"],
    );
    let e = add_note(&f.actor, "Basic", &["bird", "dog"]);
    let _ = e;

    let mut groups = dupes(&f.actor, "Front", "");
    groups.sort_by(|x, y| x.value.cmp(&y.value));
    assert_eq!(groups.len(), 2, "{groups:?}");
    assert_eq!(groups[0].value, "cat");
    assert_eq!(groups[0].note_ids, [c, d]);
    assert_eq!(groups[1].value, "dog");
    assert_eq!(groups[1].note_ids, [a, b]);

    // A search filter narrows the candidates; field names are case-blind.
    let groups = dupes(&f.actor, "front", "dog");
    assert_eq!(groups.len(), 1);
    assert!(dupes(&f.actor, "Front", "tag:nothing").is_empty());
    // The second field holds "dog" once.
    assert!(dupes(&f.actor, "Back", "").is_empty());
    assert!(dupes(&f.actor, "No such field", "").is_empty());
    assert!(matches!(
        transfer(
            &f.actor,
            TransferCommand::FindDuplicates {
                field_name: " ".into(),
                search: String::new()
            }
        ),
        Err(CoreError::Invalid(_))
    ));
    assert!(transfer(
        &f.actor,
        TransferCommand::FindDuplicates {
            field_name: "Front".into(),
            search: "(".into()
        }
    )
    .is_err());
}

#[test]
fn field_names_and_tag_duplicates() {
    let f = fixture();
    let a = add_note(&f.actor, "Basic", &["same", "1"]);
    let b = add_note(&f.actor, "Basic", &["same", "2"]);
    let TransferResponse::FieldNames(names) =
        transfer(&f.actor, TransferCommand::DuplicateFieldNames).unwrap()
    else {
        panic!()
    };
    assert!(names.contains(&"Front".to_owned()) && names.contains(&"Text".to_owned()));
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);

    let changes = f.actor.subscribe();
    let TransferResponse::Tagged { count, changes: c } = transfer(
        &f.actor,
        TransferCommand::TagDuplicates {
            note_ids: vec![a, b],
        },
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(count, 2);
    assert!(c.note || c.tag);
    assert!(changes.recv().is_ok());
    assert_eq!(search(&f.actor, "tag:duplicate").len(), 2);
    assert!(transfer(
        &f.actor,
        TransferCommand::TagDuplicates { note_ids: vec![] }
    )
    .is_err());
}
