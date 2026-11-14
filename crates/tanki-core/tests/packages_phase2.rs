//! Packages stream (Phase 2): `.apkg` import/export, import options and log,
//! schema guard, `.colpkg` export and staged restore, cancellation. All on
//! generated fixtures in temporary folders.
use anki::collection::{Collection, CollectionBuilder};
use anki::services::{DecksService, ImportExportService, NotesService, SearchService};
use anki_proto as pb;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{atomic::AtomicBool, atomic::Ordering, Arc, Mutex};
use std::time::Duration;
use tanki_core::*;

type Progress = Arc<Mutex<anki::progress::ProgressState>>;

/// A closed profile with both generated fixtures: every stock note type,
/// custom note types, nested decks, scheduling and media.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let actor = CollectionActor::open(dir.path()).unwrap();
    actor.execute(Command::GenerateFixture).unwrap();
    actor.execute(Command::GenerateStudyFixture).unwrap();
    actor.close().unwrap();
    dir
}

fn open(profile: &Path) -> Collection {
    open_with(profile, Progress::default())
}

fn open_with(profile: &Path, progress: Progress) -> Collection {
    std::fs::create_dir_all(profile.join("collection.media")).unwrap();
    CollectionBuilder::new(profile.join("collection.anki2"))
        .set_media_paths(
            profile.join("collection.media"),
            profile.join("collection.media.db2"),
        )
        .set_shared_progress_state(progress)
        .build()
        .unwrap()
}

fn export(
    col: &mut Collection,
    path: &Path,
    options: ExportOptions,
    selection: ExportSelection,
) -> u32 {
    match packages::execute(
        col,
        PackageCommand::Export {
            path: path.into(),
            options,
            selection,
        },
    )
    .unwrap()
    {
        DataResponse::Exported { notes } => notes,
        other => panic!("{other:?}"),
    }
}

fn import(
    col: &mut Collection,
    path: &Path,
    options: Option<ImportOptions>,
    schema_confirmed: bool,
) -> Result<ImportLog> {
    match packages::execute(
        col,
        PackageCommand::Import {
            path: path.into(),
            options,
            schema_confirmed,
        },
    )? {
        DataResponse::Imported { log, changes } => {
            assert!(changes.note || log.new == 0, "{changes:?}");
            Ok(log)
        }
        other => panic!("{other:?}"),
    }
}

fn with_media() -> ExportOptions {
    ExportOptions {
        with_media: true,
        ..Default::default()
    }
}

#[derive(Debug, PartialEq, Clone)]
struct NoteRow {
    field_names: Vec<String>,
    fields: Vec<String>,
    tags: Vec<String>,
}

fn note_ids(col: &mut Collection, search: &str) -> Vec<i64> {
    SearchService::search_notes(
        col,
        pb::search::SearchRequest {
            search: search.into(),
            order: None,
        },
    )
    .unwrap()
    .ids
}

fn card_count(col: &mut Collection, search: &str) -> usize {
    SearchService::search_cards(
        col,
        pb::search::SearchRequest {
            search: search.into(),
            order: None,
        },
    )
    .unwrap()
    .ids
    .len()
}

/// Notes by GUID (note type names differ after import: Anki renames
/// clashing ones, so field names stand in for the type).
fn notes(col: &mut Collection, search: &str) -> BTreeMap<String, NoteRow> {
    use anki::services::NotetypesService;
    note_ids(col, search)
        .into_iter()
        .map(|nid| {
            let note = NotesService::get_note(col, pb::notes::NoteId { nid }).unwrap();
            let nt = NotetypesService::get_notetype(
                col,
                pb::notetypes::NotetypeId {
                    ntid: note.notetype_id,
                },
            )
            .unwrap();
            (
                note.guid,
                NoteRow {
                    field_names: nt.fields.into_iter().map(|f| f.name).collect(),
                    fields: note.fields,
                    tags: note.tags,
                },
            )
        })
        .collect()
}

fn media_files(folder: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().into_string().unwrap(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

/// Every file under `dir` with its bytes and modification time.
fn tree(dir: &Path) -> BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)> {
    let mut out = BTreeMap::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(tree(&path));
        } else {
            let meta = std::fs::metadata(&path).unwrap();
            out.insert(
                path.clone(),
                (std::fs::read(&path).unwrap(), meta.modified().unwrap()),
            );
        }
    }
    out
}

/// Everything an import could change, for "collection unchanged" checks.
#[derive(Debug, PartialEq)]
struct State {
    notes: Vec<(i64, u32, Vec<String>)>,
    notetypes: Vec<(i64, i64, u32, Vec<String>)>,
    presets: pb::import_export::ImportAnkiPackageOptions,
    schema: i64,
    modified: i64,
}

fn state(col: &mut Collection) -> State {
    let notes = note_ids(col, "")
        .into_iter()
        .map(|nid| {
            let note = NotesService::get_note(col, pb::notes::NoteId { nid }).unwrap();
            (note.id, note.mtime_secs, note.fields)
        })
        .collect();
    let notetypes = col
        .get_all_notetypes()
        .unwrap()
        .into_iter()
        .map(|nt| {
            (
                nt.id.0,
                nt.mtime_secs.0,
                nt.config.sort_field_idx,
                nt.fields.iter().map(|f| f.name.clone()).collect(),
            )
        })
        .collect();
    let presets = ImportExportService::get_import_anki_package_presets(col).unwrap();
    let meta = col.sync_meta().unwrap();
    State {
        notes,
        notetypes,
        presets,
        schema: meta.schema.0,
        modified: meta.modified.0,
    }
}

fn set_first_field(col: &mut Collection, search: &str, text: &str) {
    let nid = note_ids(col, search)[0];
    let mut note = NotesService::get_note(col, pb::notes::NoteId { nid }).unwrap();
    note.fields[0] = text.into();
    let _ = NotesService::update_notes(
        col,
        pb::notes::UpdateNotesRequest {
            notes: vec![note],
            skip_undo_entry: false,
        },
    )
    .unwrap();
}

fn first_field(col: &mut Collection, search: &str) -> String {
    let nid = note_ids(col, search)[0];
    NotesService::get_note(col, pb::notes::NoteId { nid })
        .unwrap()
        .fields
        .remove(0)
}

/// Note mtimes have one-second resolution; update conditions compare them.
fn next_second() {
    std::thread::sleep(Duration::from_millis(1100));
}

fn deck_id(col: &mut Collection, name: &str) -> i64 {
    DecksService::get_deck_id_by_name(col, pb::generic::String { val: name.into() })
        .unwrap()
        .did
}

#[test]
fn apkg_round_trips_whole_collection_deck_and_query() {
    let source = fixture();
    let mut a = open(source.path());
    let out = tempfile::tempdir().unwrap();
    let a_media = media_files(&source.path().join("collection.media"));

    let cases: Vec<(&str, ExportSelection, String)> = vec![
        ("all", ExportSelection::All, String::new()),
        (
            "deck",
            // The parent deck: children are included. As on desktop, cards
            // currently moved into a filtered deck are not.
            ExportSelection::Deck(deck_id(&mut a, "Study")),
            "deck:Study -deck:filtered".into(),
        ),
        (
            "query",
            ExportSelection::Query("tag:golden::*".into()),
            "tag:golden::*".into(),
        ),
    ];
    for (name, selection, search) in cases {
        let path = out.path().join(format!("{name}.apkg"));
        let exported = export(&mut a, &path, with_media(), selection);
        let mut expected = notes(&mut a, &search);
        assert_eq!(exported as usize, expected.len(), "{name}");
        // Without scheduling, Anki's export drops the "marked" and "leech" tags.
        for note in expected.values_mut() {
            note.tags
                .retain(|t| !t.eq_ignore_ascii_case("marked") && !t.eq_ignore_ascii_case("leech"));
        }
        assert!(!expected.is_empty());

        let target = tempfile::tempdir().unwrap();
        let mut b = open(target.path());
        let log = import(&mut b, &path, Some(ImportOptions::default()), false).unwrap();
        assert_eq!(log.new, expected.len(), "{name}");
        assert_eq!(log.found_notes as usize, expected.len());
        assert_eq!(notes(&mut b, ""), expected, "{name}");
        assert_eq!(
            card_count(&mut b, ""),
            card_count(&mut a, &search),
            "{name}"
        );
        // Scheduling was not requested: every imported card is new.
        assert_eq!(card_count(&mut b, "-is:new"), 0, "{name}");

        let imported = media_files(&target.path().join("collection.media"));
        assert!(!imported.is_empty(), "{name}: no media imported");
        for (file, bytes) in &imported {
            assert_eq!(Some(bytes), a_media.get(file), "{name}: {file}");
        }
        b.close(None).unwrap();
    }
    // The whole-collection package carries all fixture media in use.
    let target = tempfile::tempdir().unwrap();
    let mut b = open(target.path());
    import(&mut b, &out.path().join("all.apkg"), None, false).unwrap();
    let imported = media_files(&target.path().join("collection.media"));
    for file in [
        "fixture.svg",
        "fixture.wav",
        "study-gradient.png",
        "study-tone.wav",
    ] {
        assert_eq!(imported.get(file), a_media.get(file), "{file}");
    }
    b.close(None).unwrap();
    a.close(None).unwrap();
}

#[test]
fn scheduling_is_carried_only_when_requested() {
    let source = fixture();
    let mut a = open(source.path());
    let reviewed = card_count(&mut a, "-is:new");
    assert!(reviewed > 0);
    let out = tempfile::tempdir().unwrap();
    let scheduled = out.path().join("scheduled.apkg");
    export(
        &mut a,
        &scheduled,
        ExportOptions {
            with_scheduling: true,
            with_deck_configs: true,
            ..Default::default()
        },
        ExportSelection::All,
    );
    let plain = out.path().join("plain.apkg");
    export(
        &mut a,
        &plain,
        ExportOptions::default(),
        ExportSelection::All,
    );

    let with = |path: &Path, with_scheduling: bool| {
        let target = tempfile::tempdir().unwrap();
        let mut b = open(target.path());
        import(
            &mut b,
            path,
            Some(ImportOptions {
                with_scheduling,
                with_deck_configs: with_scheduling,
                ..Default::default()
            }),
            false,
        )
        .unwrap();
        let count = card_count(&mut b, "-is:new");
        b.close(None).unwrap();
        count
    };
    assert!(with(&scheduled, true) > 0);
    assert_eq!(with(&scheduled, false), 0);
    assert_eq!(with(&plain, true), 0);
    a.close(None).unwrap();
}

#[test]
fn update_conditions_on_reimport_of_an_edited_package() {
    let source = fixture();
    let mut a = open(source.path());
    let out = tempfile::tempdir().unwrap();
    let first = out.path().join("first.apkg");
    export(&mut a, &first, with_media(), ExportSelection::All);
    let target = tempfile::tempdir().unwrap();
    let mut b = open(target.path());
    import(&mut b, &first, Some(ImportOptions::default()), false).unwrap();

    const NOTE: &str = "tag:golden::latex-missing";
    let original = first_field(&mut b, NOTE);
    next_second();
    set_first_field(&mut a, NOTE, "edited in A");
    let edited = out.path().join("edited.apkg");
    export(&mut a, &edited, with_media(), ExportSelection::All);

    let reimport = |b: &mut Collection, update_notes| {
        import(
            b,
            &edited,
            Some(ImportOptions {
                update_notes,
                ..Default::default()
            }),
            false,
        )
        .unwrap()
    };
    let log = reimport(&mut b, UpdateCondition::Never);
    assert_eq!((log.new, log.updated), (0, 0));
    assert_eq!(log.duplicate, log.found_notes as usize);
    assert_eq!(first_field(&mut b, NOTE), original);

    let log = reimport(&mut b, UpdateCondition::IfNewer);
    assert_eq!(log.updated, 1);
    assert_eq!(log.entries["updated"][0][0], "edited in A");
    assert_eq!(first_field(&mut b, NOTE), "edited in A");

    // Now the local note is newer than the package's.
    next_second();
    set_first_field(&mut b, NOTE, "edited in B");
    let log = reimport(&mut b, UpdateCondition::IfNewer);
    assert_eq!(log.updated, 0);
    assert_eq!(first_field(&mut b, NOTE), "edited in B");

    let log = reimport(&mut b, UpdateCondition::Always);
    assert_eq!(log.updated, 1);
    assert_eq!(first_field(&mut b, NOTE), "edited in A");
    b.close(None).unwrap();
    a.close(None).unwrap();
}

/// Crafted re-import: one new note, one edited note, one note whose type
/// gained a field (conflicting without merge), everything else unchanged.
/// Also the schema guard for merging and the merge itself.
#[test]
fn import_log_categories_merge_and_schema_guard() {
    const CHANGED_TYPE: &str = "Basic (optional reversed card)";
    let source = fixture();
    let mut a = open(source.path());
    let out = tempfile::tempdir().unwrap();
    let first = out.path().join("first.apkg");
    export(&mut a, &first, with_media(), ExportSelection::All);
    let targets: Vec<_> = (0..2).map(|_| tempfile::tempdir().unwrap()).collect();
    let mut plain = open(targets[0].path());
    let mut merged = open(targets[1].path());
    import(&mut plain, &first, Some(ImportOptions::default()), false).unwrap();
    import(&mut merged, &first, Some(ImportOptions::default()), false).unwrap();

    next_second();
    set_first_field(&mut a, "tag:golden::latex-missing", "edited");
    let mut nt = (*a.get_notetype_by_name(CHANGED_TYPE).unwrap().unwrap()).clone();
    nt.fields.push(anki::notetype::NoteField::new("Added"));
    a.update_notetype(&mut nt, false).unwrap();
    let changed_notes = note_ids(&mut a, &format!("\"note:{CHANGED_TYPE}\"")).len();
    assert!(changed_notes > 0);
    let ntid = a.get_notetype_by_name("Basic").unwrap().unwrap().id.0;
    let mut basic = NotesService::new_note(&mut a, pb::notetypes::NotetypeId { ntid }).unwrap();
    basic.fields = vec!["brand new".into(), "note".into()];
    let _ = NotesService::add_note(
        &mut a,
        pb::notes::AddNoteRequest {
            note: Some(basic),
            deck_id: 1,
        },
    )
    .unwrap();
    let second = out.path().join("second.apkg");
    let total = export(&mut a, &second, with_media(), ExportSelection::All) as usize;

    let log = import(&mut plain, &second, Some(ImportOptions::default()), false).unwrap();
    assert_eq!(log.found_notes as usize, total);
    assert_eq!(log.new, 1);
    assert_eq!(log.updated, 1);
    assert_eq!(log.conflicting, changed_notes);
    assert_eq!(log.duplicate, total - 2 - changed_notes);
    assert_eq!(
        log.entries["new"],
        [vec!["brand new".to_owned(), "note".into()]]
    );
    assert_eq!(log.entries["updated"][0][0], "edited");
    assert_eq!(log.entries["conflicting"].len(), changed_notes);
    assert_eq!(log.entries["duplicate"].len(), log.duplicate);
    plain.close(None).unwrap();

    // Merging note types needs confirmation and changes nothing without it.
    let merge = Some(ImportOptions {
        merge_notetypes: true,
        ..Default::default()
    });
    let before = state(&mut merged);
    let media_before = media_files(&targets[1].path().join("collection.media"));
    assert_eq!(
        import(&mut merged, &second, merge.clone(), false).unwrap_err(),
        CoreError::SchemaChangeRequired
    );
    assert_eq!(state(&mut merged), before);
    assert_eq!(
        media_files(&targets[1].path().join("collection.media")),
        media_before
    );

    let log = import(&mut merged, &second, merge, true).unwrap();
    assert_eq!(log.conflicting, 0);
    assert_eq!(log.new, 1);
    // Adding a field did not touch those notes' mtimes, so with their note
    // type merged they are plain duplicates now.
    assert_eq!(log.updated, 1);
    assert_eq!(log.duplicate, total - 2);
    assert!(state(&mut merged).schema > before.schema);
    // The existing note type gained the field; no copy was added.
    let merged_type = merged.get_notetype(nt.id).unwrap().unwrap();
    assert!(merged_type.fields.iter().any(|f| f.name == "Added"));
    assert_eq!(
        merged.get_all_notetypes().unwrap().len(),
        before.notetypes.len()
    );
    // Presets were persisted by the confirmed import only.
    assert!(
        ImportExportService::get_import_anki_package_presets(&mut merged)
            .unwrap()
            .merge_notetypes
    );
    merged.close(None).unwrap();
    a.close(None).unwrap();
}

/// Without merging, Anki still marks the schema modified when an incoming
/// note type with matching fields has another sort field. Detected and undone.
#[test]
fn sort_field_schema_change_is_undone_unless_confirmed() {
    let source = fixture();
    let mut a = open(source.path());
    let out = tempfile::tempdir().unwrap();
    let first = out.path().join("first.apkg");
    export(
        &mut a,
        &first,
        ExportOptions::default(),
        ExportSelection::All,
    );
    let target = tempfile::tempdir().unwrap();
    let mut b = open(target.path());
    import(&mut b, &first, Some(ImportOptions::default()), false).unwrap();

    next_second();
    let mut nt = (*a.get_notetype_by_name("Basic").unwrap().unwrap()).clone();
    nt.config.sort_field_idx = 1;
    a.update_notetype(&mut nt, false).unwrap();
    let second = out.path().join("second.apkg");
    export(
        &mut a,
        &second,
        ExportOptions::default(),
        ExportSelection::All,
    );

    let before = state(&mut b);
    assert_eq!(
        import(&mut b, &second, Some(ImportOptions::default()), false).unwrap_err(),
        CoreError::SchemaChangeRequired
    );
    assert_eq!(state(&mut b), before);

    import(&mut b, &second, Some(ImportOptions::default()), true).unwrap();
    let sort = b
        .get_notetype(nt.id)
        .unwrap()
        .unwrap()
        .config
        .sort_field_idx;
    assert_eq!(sort, 1);
    assert!(state(&mut b).schema > before.schema);
    b.close(None).unwrap();
    a.close(None).unwrap();
}

#[test]
fn presets_are_used_when_options_are_none_and_persisted_after_import() {
    let source = fixture();
    let mut a = open(source.path());
    let out = tempfile::tempdir().unwrap();
    let golden = out.path().join("golden.apkg");
    export(
        &mut a,
        &golden,
        ExportOptions::default(),
        ExportSelection::Query("tag:golden::*".into()),
    );
    let study = out.path().join("study.apkg");
    let study_deck = deck_id(&mut a, "Study");
    export(
        &mut a,
        &study,
        ExportOptions {
            with_scheduling: true,
            ..Default::default()
        },
        ExportSelection::Deck(study_deck),
    );

    let target = tempfile::tempdir().unwrap();
    let mut b = open(target.path());
    let defaults = ImportExportService::get_import_anki_package_presets(&mut b).unwrap();
    assert!(!defaults.with_scheduling && !defaults.merge_notetypes);

    import(
        &mut b,
        &golden,
        Some(ImportOptions {
            with_scheduling: true,
            update_notes: UpdateCondition::Never,
            update_notetypes: UpdateCondition::Always,
            ..Default::default()
        }),
        false,
    )
    .unwrap();
    let presets = ImportExportService::get_import_anki_package_presets(&mut b).unwrap();
    use pb::import_export::ImportAnkiPackageUpdateCondition as C;
    assert!(presets.with_scheduling);
    assert!(!presets.with_deck_configs && !presets.merge_notetypes);
    assert_eq!(presets.update_notes(), C::Never);
    assert_eq!(presets.update_notetypes(), C::Always);
    // Survives closing and reopening (stored in the collection config).
    b.close(None).unwrap();
    let mut b = open(target.path());
    assert_eq!(
        ImportExportService::get_import_anki_package_presets(&mut b).unwrap(),
        presets
    );

    // `None` imports with the saved presets: scheduling comes along.
    import(&mut b, &study, None, false).unwrap();
    assert!(card_count(&mut b, "-is:new") > 0);
    b.close(None).unwrap();
    a.close(None).unwrap();
}

#[test]
fn colpkg_export_and_restore_into_a_separate_stage() {
    let source = fixture();
    let mut a = open(source.path());
    let expected = notes(&mut a, "");
    let cards = card_count(&mut a, "");
    let out = tempfile::tempdir().unwrap();
    let colpkg = out.path().join("all.colpkg");
    // Wrong extension: refused, and the collection is closed cleanly.
    let wrong = out.path().join("collection.anki2");
    assert!(matches!(
        packages::export_collection(a, &wrong, true, false),
        Err(CoreError::Invalid(_))
    ));
    assert!(!wrong.exists());
    let a = open(source.path());
    packages::export_collection(a, &colpkg, true, false).unwrap();
    assert!(colpkg.is_file());

    let live = tree(source.path());
    let stage = out.path().join("stage");
    let progress = Progress::default();
    packages::restore_to_stage(&colpkg, &stage, progress.clone()).unwrap();
    assert!(progress.lock().unwrap().last_progress.is_some());
    assert!(stage.join("collection.anki2").is_file());
    assert_eq!(tree(source.path()), live, "live profile changed");

    let mut staged = open(&stage);
    assert_eq!(notes(&mut staged, ""), expected);
    assert_eq!(card_count(&mut staged, ""), cards);
    let staged_media = media_files(&stage.join("collection.media"));
    assert_eq!(
        staged_media,
        media_files(&source.path().join("collection.media"))
    );
    staged.close(None).unwrap();

    // Stages that are not empty are refused and left as they were.
    let busy = out.path().join("busy");
    std::fs::create_dir(&busy).unwrap();
    std::fs::write(busy.join("note.txt"), b"keep").unwrap();
    assert!(matches!(
        packages::restore_to_stage(&colpkg, &busy, Progress::default()),
        Err(CoreError::Invalid(m)) if m.contains("not empty")
    ));
    assert_eq!(std::fs::read_dir(&busy).unwrap().count(), 1);
    assert!(matches!(
        packages::restore_to_stage(&colpkg, &stage, Progress::default()),
        Err(CoreError::Invalid(m)) if m.contains("already holds a collection")
    ));
    // A profile is never a valid stage.
    assert!(packages::restore_to_stage(&colpkg, source.path(), Progress::default()).is_err());
    #[cfg(unix)]
    {
        let empty = out.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        let link = out.path().join("link");
        std::os::unix::fs::symlink(&empty, &link).unwrap();
        assert!(matches!(
            packages::restore_to_stage(&colpkg, &link, Progress::default()),
            Err(CoreError::UnsafePath(_))
        ));
    }
    assert!(matches!(
        packages::restore_to_stage(
            &out.path().join("missing.colpkg"),
            &out.path().join("s2"),
            Progress::default()
        ),
        Err(CoreError::Invalid(_))
    ));
    assert_eq!(tree(source.path()), live, "live profile changed");
}

#[test]
fn colpkg_is_never_imported_as_a_package() {
    let source = fixture();
    let a = open(source.path());
    let out = tempfile::tempdir().unwrap();
    let colpkg = out.path().join("all.colpkg");
    packages::export_collection(a, &colpkg, false, false).unwrap();
    let target = tempfile::tempdir().unwrap();
    let mut b = open(target.path());
    let before = state(&mut b);
    let error = import(&mut b, &colpkg, None, true).unwrap_err();
    assert!(
        error.to_string().contains("tanki backup restore"),
        "{error}"
    );
    assert_eq!(state(&mut b), before);
    // A wrong export extension is refused before anything is written.
    assert!(packages::execute(
        &mut b,
        PackageCommand::Export {
            path: out.path().join("collection.anki2"),
            options: ExportOptions::default(),
            selection: ExportSelection::All,
        }
    )
    .is_err());
    assert!(!out.path().join("collection.anki2").exists());
    b.close(None).unwrap();
}

#[test]
fn unknown_deck_and_empty_query_are_refused() {
    let source = fixture();
    let mut a = open(source.path());
    let out = tempfile::tempdir().unwrap();
    let path = out.path().join("x.apkg");
    let run = |a: &mut Collection, selection| {
        packages::execute(
            a,
            PackageCommand::Export {
                path: path.clone(),
                options: ExportOptions::default(),
                selection,
            },
        )
    };
    assert!(matches!(
        run(&mut a, ExportSelection::Deck(424242)),
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        run(&mut a, ExportSelection::Query("tag:no-such-tag".into())),
        Err(CoreError::Invalid(_))
    ));
    // A search syntax error comes from Anki's parser.
    assert!(run(&mut a, ExportSelection::Query("(unbalanced".into())).is_err());
    assert!(!path.exists());
    a.close(None).unwrap();
}

/// Keeps asking for an abort until `done`; Anki's progress handler clears the
/// flag when it starts, so a single early request would be lost.
fn keep_cancelling(progress: Progress, done: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while !done.load(Ordering::Relaxed) {
            progress.lock().unwrap().want_abort = true;
            std::thread::yield_now();
        }
    })
}

#[test]
fn cancelled_import_is_interrupted_and_rolled_back() {
    let source = fixture();
    let mut a = open(source.path());
    let out = tempfile::tempdir().unwrap();
    let path = out.path().join("all.apkg");
    export(&mut a, &path, with_media(), ExportSelection::All);
    a.close(None).unwrap();

    let target = tempfile::tempdir().unwrap();
    let progress = Progress::default();
    let mut b = open_with(target.path(), progress.clone());
    let before = state(&mut b);
    let done = Arc::new(AtomicBool::new(false));
    let canceller = keep_cancelling(progress.clone(), done.clone());
    let result = import(&mut b, &path, None, false);
    done.store(true, Ordering::Relaxed);
    canceller.join().unwrap();
    assert_eq!(result.unwrap_err(), CoreError::Interrupted);
    assert_eq!(state(&mut b), before);
    // The collection keeps working.
    progress.lock().unwrap().reset();
    assert!(import(&mut b, &path, None, false).unwrap().new > 0);
    b.close(None).unwrap();
}

#[test]
fn cancelled_restore_leaves_an_empty_stage() {
    let source = fixture();
    let a = open(source.path());
    let out = tempfile::tempdir().unwrap();
    let colpkg = out.path().join("all.colpkg");
    packages::export_collection(a, &colpkg, true, false).unwrap();

    // Requested before the start.
    let progress = Progress::default();
    progress.lock().unwrap().want_abort = true;
    let stage = out.path().join("early");
    assert_eq!(
        packages::restore_to_stage(&colpkg, &stage, progress),
        Err(CoreError::Interrupted)
    );
    assert_eq!(std::fs::read_dir(&stage).unwrap().count(), 0);

    // Requested while it runs.
    let progress = Progress::default();
    let done = Arc::new(AtomicBool::new(false));
    let canceller = keep_cancelling(progress.clone(), done.clone());
    let stage = out.path().join("running");
    let result = packages::restore_to_stage(&colpkg, &stage, progress);
    done.store(true, Ordering::Relaxed);
    canceller.join().unwrap();
    assert_eq!(result, Err(CoreError::Interrupted));
    assert_eq!(std::fs::read_dir(&stage).unwrap().count(), 0);
}

fn wait(actor: &CollectionActor, op: PendingOperation) -> Result<DataResponse> {
    loop {
        if let Some(response) = op.try_recv()? {
            return Ok(response);
        }
        let _ = actor.progress();
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn packages_through_the_actor_with_polling_progress_and_cancel() {
    let source = fixture();
    let out = tempfile::tempdir().unwrap();
    let path = out.path().join("all.apkg");
    let actor = CollectionActor::open(source.path()).unwrap();
    let Response::CardIds(cards) = actor
        .execute(Command::Search {
            query: String::new(),
        })
        .unwrap()
    else {
        panic!("search")
    };
    let op = actor
        .start_data(DataCommand::Package(PackageCommand::Export {
            path: path.clone(),
            options: with_media(),
            selection: ExportSelection::All,
        }))
        .unwrap();
    let DataResponse::Exported { notes } = wait(&actor, op).unwrap() else {
        panic!("export")
    };
    assert!(notes > 0);
    assert!(!actor.progress().stage.is_empty());
    // `.colpkg` through import is refused by the actor's package path too.
    let refused = actor
        .start_data(DataCommand::Package(PackageCommand::Import {
            path: out.path().join("x.colpkg"),
            options: None,
            schema_confirmed: true,
        }))
        .unwrap()
        .wait();
    assert!(matches!(refused, Err(CoreError::Invalid(_))));
    actor.close().unwrap();

    let target = tempfile::tempdir().unwrap();
    let actor = CollectionActor::open(target.path()).unwrap();
    let changes = actor.subscribe();
    // Cancelled: keep asking until the actor answers.
    let op = actor
        .start_data(DataCommand::Package(PackageCommand::Import {
            path: path.clone(),
            options: None,
            schema_confirmed: false,
        }))
        .unwrap();
    let cancelled = loop {
        actor.cancel_operation();
        match op.try_recv() {
            Ok(None) => std::thread::yield_now(),
            other => break other,
        }
    };
    assert_eq!(cancelled.unwrap_err(), CoreError::Interrupted);

    let op = actor
        .start_data(DataCommand::Package(PackageCommand::Import {
            path,
            options: None,
            schema_confirmed: false,
        }))
        .unwrap();
    let DataResponse::Imported {
        log,
        changes: op_changes,
    } = wait(&actor, op).unwrap()
    else {
        panic!("import")
    };
    assert_eq!(log.new as u32, notes);
    assert!(op_changes.note && op_changes.card);
    assert!(changes.recv_timeout(Duration::from_secs(5)).unwrap().note);
    assert!(!actor.progress().stage.is_empty(), "{:?}", actor.progress());
    let Response::CardIds(imported) = actor
        .execute(Command::Search {
            query: String::new(),
        })
        .unwrap()
    else {
        panic!("search")
    };
    assert_eq!(imported.len(), cards.len());
    actor.close().unwrap();
}
