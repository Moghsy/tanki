//! Maintenance stream: check database/media, empty cards, preferences and
//! profiles, on temporary fixtures only.
use std::path::Path;
use tanki_core::maintenance::{profiles, *};
use tanki_core::*;

struct Fixture {
    dir: tempfile::TempDir,
    actor: CollectionActor,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let actor = CollectionActor::open(dir.path()).unwrap();
    actor.execute(Command::GenerateStudyFixture).unwrap();
    Fixture { dir, actor }
}

fn run(actor: &CollectionActor, command: MaintenanceCommand) -> Result<MaintenanceResponse> {
    match actor.execute(Command::Manage(ManageCommand::Maintenance(command)))? {
        Response::Manage(ManageResponse::Maintenance(r)) => Ok(r),
        other => panic!("unexpected {other:?}"),
    }
}

fn edit(actor: &CollectionActor, command: NoteCommand) -> EditResponse {
    match actor.execute(Command::Notes(command)).unwrap() {
        Response::Edit(r) => r,
        other => panic!("unexpected {other:?}"),
    }
}

fn backups(dir: &Path) -> usize {
    std::fs::read_dir(dir.join("backups"))
        .map(|d| {
            d.flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".colpkg"))
                .count()
        })
        .unwrap_or(0)
}

fn undo_label(actor: &CollectionActor) -> Option<String> {
    match actor.execute(Command::UndoStatus).unwrap() {
        Response::UndoStatus(s) => s.undo,
        other => panic!("unexpected {other:?}"),
    }
}

/// Adds notes of "Basic (optional reversed card)" with the reverse field set,
/// then clears it, leaving one empty card per note.
fn note_with_empty_card(actor: &CollectionActor, front: &str) -> i64 {
    let EditResponse::Notetypes(all) = edit(actor, NoteCommand::Notetypes) else {
        panic!()
    };
    let nt = all
        .iter()
        .find(|n| n.name == "Basic (optional reversed card)")
        .unwrap();
    let EditResponse::Note(mut note) = edit(actor, NoteCommand::NewNote { notetype_id: nt.id })
    else {
        panic!()
    };
    note.fields = vec![front.into(), "back".into(), "y".into()];
    let EditResponse::NoteAdded { id, .. } = edit(actor, NoteCommand::AddNote { note, deck_id: 1 })
    else {
        panic!()
    };
    let EditResponse::Note(note) = edit(actor, NoteCommand::GetNote { id }) else {
        panic!()
    };
    let EditResponse::Updated(done) = edit(
        actor,
        NoteCommand::UpdateNotes {
            updates: vec![NoteUpdate {
                id,
                expected_mtime: note.mtime_secs,
                expected_fields: None,
                fields: vec![(2, String::new())],
                tags: None,
            }],
        },
    ) else {
        panic!()
    };
    assert!(done.conflicts.is_empty(), "{done:?}");
    id
}

fn card_ids(actor: &CollectionActor) -> Vec<i64> {
    match actor
        .execute(Command::Search {
            query: "deck:*".into(),
        })
        .unwrap()
    {
        Response::CardIds(ids) => ids,
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn check_database_needs_confirmation_backs_up_and_clears_undo() {
    let f = fixture();
    let _ = note_with_empty_card(&f.actor, "undoable");
    assert!(undo_label(&f.actor).is_some());
    let before = backups(f.dir.path());
    // Refused before anything is touched: no backup, undo kept.
    assert_eq!(
        run(
            &f.actor,
            MaintenanceCommand::CheckDatabase { confirmed: false }
        )
        .unwrap_err(),
        CoreError::SchemaChangeRequired
    );
    assert_eq!(backups(f.dir.path()), before);
    assert!(undo_label(&f.actor).is_some());

    let MaintenanceResponse::DatabaseChecked(report) = run(
        &f.actor,
        MaintenanceCommand::CheckDatabase { confirmed: true },
    )
    .unwrap() else {
        panic!()
    };
    assert!(report.backup.unwrap().ends_with(".colpkg"));
    assert!(
        backups(f.dir.path()) > before,
        "a forced backup precedes the check"
    );
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(undo_label(&f.actor), None, "check database clears undo");
    // The collection still answers afterwards.
    assert!(!card_ids(&f.actor).is_empty());
}

#[test]
fn check_database_broadcasts_a_full_refresh() {
    let f = fixture();
    let changes = f.actor.subscribe();
    run(
        &f.actor,
        MaintenanceCommand::CheckDatabase { confirmed: true },
    )
    .unwrap();
    let c = changes.recv().unwrap();
    assert!(c.card && c.note && c.deck && c.notetype);
}

#[test]
fn check_media_lists_trashes_restores_and_empties() {
    let f = fixture();
    let media = f.dir.path().join("collection.media");
    std::fs::write(media.join("unused-file.txt"), b"x").unwrap();
    let EditResponse::Notetypes(all) = edit(&f.actor, NoteCommand::Notetypes) else {
        panic!()
    };
    let basic = all.iter().find(|n| n.name == "Basic").unwrap();
    let EditResponse::Note(mut note) = edit(
        &f.actor,
        NoteCommand::NewNote {
            notetype_id: basic.id,
        },
    ) else {
        panic!()
    };
    note.fields = vec![r#"<img src="gone-missing.png">"#.into(), "b".into()];
    edit(&f.actor, NoteCommand::AddNote { note, deck_id: 1 });

    let used_files = std::fs::read_dir(&media).unwrap().count() - 1;
    let before = backups(f.dir.path());
    let MaintenanceResponse::MediaChecked(report) =
        run(&f.actor, MaintenanceCommand::CheckMedia).unwrap()
    else {
        panic!()
    };
    assert!(backups(f.dir.path()) > before, "backup first");
    assert!(report.backup.is_some());
    assert!(
        report.unused.contains(&"unused-file.txt".to_owned()),
        "{report:?}"
    );
    assert!(report.missing.contains(&"gone-missing.png".to_owned()));
    assert!(
        !report.report.contains('\u{2068}'),
        "isolation marks stripped"
    );
    assert!(!report.have_trash);

    // Only the chosen files go to the trash, and they are not deleted.
    let MaintenanceResponse::MediaTrashed(n) = run(
        &f.actor,
        MaintenanceCommand::TrashMedia {
            files: vec!["unused-file.txt".into()],
        },
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(n, 1);
    let trash = f.dir.path().join("media.trash");
    assert!(!media.join("unused-file.txt").exists());
    assert!(trash.join("unused-file.txt").exists());
    assert!(
        std::fs::read_dir(&media).unwrap().count() >= used_files,
        "used files stay"
    );

    run(&f.actor, MaintenanceCommand::RestoreMediaTrash).unwrap();
    assert!(media.join("unused-file.txt").exists());

    run(
        &f.actor,
        MaintenanceCommand::TrashMedia {
            files: vec!["unused-file.txt".into()],
        },
    )
    .unwrap();
    run(&f.actor, MaintenanceCommand::EmptyMediaTrash).unwrap();
    assert!(!media.join("unused-file.txt").exists());
    assert!(
        !trash.join("unused-file.txt").exists(),
        "emptying the trash deletes for good"
    );
}

#[test]
fn empty_cards_report_and_delete() {
    let f = fixture();
    let id = note_with_empty_card(&f.actor, "one");
    let before = card_ids(&f.actor).len();
    let MaintenanceResponse::EmptyCards(report) =
        run(&f.actor, MaintenanceCommand::FindEmptyCards).unwrap()
    else {
        panic!()
    };
    assert_eq!(report.notes.len(), 1);
    assert_eq!(report.notes[0].note_id, id);
    assert_eq!(report.notes[0].card_ids.len(), 1);
    assert!(!report.notes[0].all_cards_empty);
    assert!(!report.report.is_empty());
    // Reading changes nothing.
    assert_eq!(card_ids(&f.actor).len(), before);

    let changes = f.actor.subscribe();
    let MaintenanceResponse::EmptyCardsDeleted { cards, changes: c } = run(
        &f.actor,
        MaintenanceCommand::DeleteEmptyCards { keep_notes: true },
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(cards, 1);
    assert!(c.card);
    assert!(changes.recv().unwrap().card);
    assert_eq!(card_ids(&f.actor).len(), before - 1);
    let MaintenanceResponse::EmptyCards(again) =
        run(&f.actor, MaintenanceCommand::FindEmptyCards).unwrap()
    else {
        panic!()
    };
    assert!(again.is_empty());
    // Nothing left: deleting is a no-op.
    let MaintenanceResponse::EmptyCardsDeleted { cards, .. } = run(
        &f.actor,
        MaintenanceCommand::DeleteEmptyCards { keep_notes: true },
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(cards, 0);
}

#[test]
fn keep_notes_rule_picks_cards_to_delete() {
    let report = EmptyCardsReport {
        report: String::new(),
        notes: vec![
            EmptyCardNote {
                note_id: 1,
                card_ids: vec![10, 11],
                all_cards_empty: true,
            },
            EmptyCardNote {
                note_id: 2,
                card_ids: vec![20],
                all_cards_empty: false,
            },
        ],
    };
    assert_eq!(report.cards_to_delete(true), vec![11, 20]);
    assert_eq!(report.cards_to_delete(false), vec![10, 11, 20]);
}

#[test]
fn preferences_round_trip_and_reject_nothing_silently() {
    let f = fixture();
    let MaintenanceResponse::Preferences(mut prefs) =
        run(&f.actor, MaintenanceCommand::GetPreferences).unwrap()
    else {
        panic!()
    };
    assert_eq!(prefs.backup_daily, 12);
    assert_eq!(prefs.backup_minimum_interval_mins, 30);
    prefs.rollover = 6;
    prefs.learn_ahead_secs = 600;
    prefs.new_review_mix = 2;
    prefs.show_intervals_on_buttons = !prefs.show_intervals_on_buttons;
    prefs.default_search_text = "deck:current".into();
    prefs.backup_daily = 3;
    let changes = f.actor.subscribe();
    let MaintenanceResponse::PreferencesSaved(c) =
        run(&f.actor, MaintenanceCommand::SetPreferences(prefs.clone())).unwrap()
    else {
        panic!()
    };
    assert!(c.config);
    assert!(changes.recv().unwrap().config);
    let MaintenanceResponse::Preferences(read) =
        run(&f.actor, MaintenanceCommand::GetPreferences).unwrap()
    else {
        panic!()
    };
    assert_eq!(read, prefs);
}

fn base_with(profiles_to_make: &[&str]) -> tempfile::TempDir {
    let base = tempfile::tempdir().unwrap();
    for name in profiles_to_make {
        profiles::create(base.path(), name).unwrap();
    }
    base
}

#[test]
fn profiles_create_list_and_name_rules() {
    let base = base_with(&["Zed", "alpha"]);
    // Desktop's own files and non-profile folders are ignored and untouched.
    std::fs::write(base.path().join("prefs21.db"), b"secret").unwrap();
    std::fs::create_dir(base.path().join("addons21")).unwrap();
    std::fs::create_dir(base.path().join("not-a-profile")).unwrap();
    let names: Vec<_> = profiles::list(base.path())
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(names, ["Zed", "alpha"]);
    assert!(base.path().join("alpha/collection.anki2").is_file());
    assert!(base.path().join("alpha/backups").is_dir());
    for bad in ["", " x", "a/b", "..", ".hidden", "addons21", "prefs21.db"] {
        assert!(
            matches!(
                profiles::create(base.path(), bad),
                Err(CoreError::Invalid(_))
            ),
            "{bad:?}"
        );
    }
    assert!(matches!(
        profiles::create(base.path(), "ALPHA"),
        Err(CoreError::Invalid(_))
    ));
    assert_eq!(
        std::fs::read(base.path().join("prefs21.db")).unwrap(),
        b"secret"
    );
    assert!(!base.path().join("addons21/collection.anki2").exists());
}

#[test]
fn profile_rename_moves_folder_and_metadata() {
    let base = base_with(&["old", "other"]);
    let cfg_dir = tempfile::tempdir().unwrap();
    let cfg = cfg_dir.path().join("config.toml");
    let old_key = base
        .path()
        .join("old")
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut config = config::Config {
        last_profile: Some("old".into()),
        ..Default::default()
    };
    config.profile_mut(&old_key).sync.auto_sync = true;
    config.mark_seen(&Path::new(&old_key).join("collection.anki2"));
    config.save(&cfg).unwrap();

    assert!(matches!(
        profiles::rename(base.path(), "old", "OTHER", Some(&cfg)),
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        profiles::rename(base.path(), "missing", "x", Some(&cfg)),
        Err(CoreError::Invalid(_))
    ));
    let change = profiles::rename(base.path(), "old", "new", Some(&cfg)).unwrap();
    assert!(change.config_updated);
    assert!(!base.path().join("old").exists());
    assert!(base.path().join("new/collection.anki2").is_file());
    let new_key = base
        .path()
        .join("new")
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let loaded = config::Config::load(&cfg).unwrap();
    assert_eq!(loaded.last_profile.as_deref(), Some("new"));
    assert!(loaded.profile(&new_key).unwrap().sync.auto_sync);
    assert!(loaded.profile(&old_key).is_none());
    assert!(loaded.has_seen(&Path::new(&new_key).join("collection.anki2")));
    // The renamed profile still opens.
    CollectionActor::open(base.path().join("new"))
        .unwrap()
        .close()
        .unwrap();
}

#[test]
fn profile_delete_needs_typed_name_and_refuses_the_open_one() {
    let base = base_with(&["keep", "drop"]);
    let b = base.path();
    assert!(matches!(
        profiles::delete(b, "keep", "keep", Some("keep"), None),
        Err(CoreError::Invalid(m)) if m.contains("open")
    ));
    assert!(matches!(
        profiles::delete(b, "drop", "wrong", Some("keep"), None),
        Err(CoreError::Invalid(m)) if m.contains("type its name")
    ));
    assert!(matches!(
        profiles::delete(b, "nope", "nope", None, None),
        Err(CoreError::Invalid(_))
    ));
    assert!(b.join("drop/collection.anki2").is_file());
    let trashed = profiles::delete(b, "drop", "drop", Some("keep"), None).unwrap();
    assert!(!b.join("drop").exists());
    assert!(trashed.starts_with(b.join(profiles::TRASH)));
    assert!(trashed.join("collection.anki2").is_file(), "recoverable");
    let names: Vec<_> = profiles::list(b)
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(names, ["keep"]);
}
