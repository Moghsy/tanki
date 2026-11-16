use tanki_core::{
    code_tour::{Attachment, CodeTourRef},
    *,
};
fn install(actor: &CollectionActor) -> i64 {
    let Response::Manage(ManageResponse::Notetypes(NotetypeResponse::Added { id, .. })) = actor
        .execute(Command::Manage(ManageCommand::Notetypes(
            NotetypeCommand::InstallCodeTour,
        )))
        .unwrap()
    else {
        panic!("install")
    };
    id
}
fn notes(actor: &CollectionActor, command: NoteCommand) -> EditResponse {
    let Response::Edit(r) = actor.execute(Command::Notes(command)).unwrap() else {
        panic!("notes")
    };
    r
}
#[test]
fn linked_note_queue_reorder_export_import_and_invalid_attachment() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("source")).unwrap();
    std::fs::create_dir(dir.path().join("target")).unwrap();
    let actor = CollectionActor::open(dir.path().join("source")).unwrap();
    let nt = install(&actor);
    assert_eq!(install(&actor), nt);
    actor.execute(Command::UpgradeScheduler).unwrap();
    let reference = CodeTourRef::from_id("0123456789abcdef0123456789abcdef").unwrap();
    let EditResponse::Note(mut note) = notes(&actor, NoteCommand::NewNote { notetype_id: nt })
    else {
        panic!()
    };
    note.fields = vec![
        "Describe scheduling".into(),
        "Answer".into(),
        reference.uri(),
    ];
    let EditResponse::NoteAdded { id, .. } =
        notes(&actor, NoteCommand::AddNote { note, deck_id: 1 })
    else {
        panic!()
    };
    let queue = |actor: &CollectionActor| {
        let Response::Queue(q) = actor.execute(Command::Queue { limit: 10 }).unwrap() else {
            panic!()
        };
        q
    };
    assert_eq!(
        queue(&actor).cards[0].code_tour,
        Some(Attachment::Saved(reference.clone()))
    );
    actor
        .execute(Command::Manage(ManageCommand::Notetypes(
            NotetypeCommand::Edit {
                id: nt,
                edits: vec![notetypes::NotetypeEdit::MoveField { from: 2, to: 1 }],
                confirmed: true,
            },
        )))
        .unwrap();
    assert_eq!(
        queue(&actor).cards[0].code_tour,
        Some(Attachment::Saved(reference.clone()))
    );
    let package = dir.path().join("linked.apkg");
    actor
        .execute(Command::Data(DataCommand::Package(
            PackageCommand::Export {
                path: package.clone(),
                options: ExportOptions::default(),
                selection: ExportSelection::All,
            },
        )))
        .unwrap();
    let target = CollectionActor::open(dir.path().join("target")).unwrap();
    target
        .execute(Command::Data(DataCommand::Package(
            PackageCommand::Import {
                path: package,
                options: None,
                schema_confirmed: true,
            },
        )))
        .unwrap();
    target.execute(Command::UpgradeScheduler).unwrap();
    assert_eq!(
        queue(&target).cards[0].code_tour,
        Some(Attachment::Saved(reference.clone()))
    );
    target.close().unwrap();
    let EditResponse::Note(stored) = notes(&actor, NoteCommand::GetNote { id }) else {
        panic!()
    };
    let EditResponse::Updated(report) = notes(
        &actor,
        NoteCommand::UpdateNotes {
            updates: vec![NoteUpdate {
                id,
                expected_mtime: stored.mtime_secs,
                expected_fields: Some(stored.fields.clone()),
                fields: vec![(1, "bad reference".into())],
                tags: None,
            }],
        },
    ) else {
        panic!()
    };
    assert_eq!(report.updated, [id]);
    assert!(matches!(
        queue(&actor).cards[0].code_tour,
        Some(Attachment::Invalid(_))
    ));
    actor.execute(Command::Undo).unwrap();
    assert_eq!(
        queue(&actor).cards[0].code_tour,
        Some(Attachment::Saved(reference))
    );
    actor.close().unwrap();
}
#[test]
fn install_refuses_incompatible_name_without_changing_it() {
    let dir = tempfile::tempdir().unwrap();
    let actor = CollectionActor::open(dir.path()).unwrap();
    let Response::Manage(ManageResponse::Notetypes(NotetypeResponse::Added { id, .. })) = actor
        .execute(Command::Manage(ManageCommand::Notetypes(
            NotetypeCommand::AddStock {
                kind: notetypes::StockKind::Basic,
                name: Some(code_tour::NOTETYPE.into()),
            },
        )))
        .unwrap()
    else {
        panic!()
    };
    assert!(actor
        .execute(Command::Manage(ManageCommand::Notetypes(
            NotetypeCommand::InstallCodeTour
        )))
        .is_err());
    let EditResponse::Notetype(nt) = notes(&actor, NoteCommand::Notetype { id }) else {
        panic!()
    };
    assert_eq!(nt.fields.len(), 2);
    actor.close().unwrap();
}
