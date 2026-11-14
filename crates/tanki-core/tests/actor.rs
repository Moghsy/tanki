use tanki_core::*;

#[test]
fn review_undo_backups_and_locking() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actor = CollectionActor::open(dir.path())?;
    assert!(std::fs::read_dir(dir.path().join("backups"))?
        .next()
        .is_some());
    let changes = actor.subscribe();
    actor.execute(Command::AddNote {
        notetype: "Basic".into(),
        fields: vec![
            "question [sound:q.wav]".into(),
            "answer [sound:a.wav]".into(),
        ],
    })?;
    assert!(changes.recv().unwrap().note);
    let Response::Queue(q) = actor.execute(Command::Queue { limit: 1 })? else {
        panic!("queue")
    };
    assert_eq!(q.new, 1);
    let card_id = q.cards[0].id;
    assert_eq!(q.cards[0].intervals.len(), 4);
    let Response::Rendered(card) = actor.execute(Command::Render { card_id })? else {
        panic!("render")
    };
    assert_eq!(
        card.question_audio,
        vec![AudioTag::SoundOrVideo("q.wav".into())]
    );
    assert_eq!(
        card.answer_audio,
        vec![AudioTag::SoundOrVideo("a.wav".into())]
    );
    actor.execute(Command::Answer {
        card_id,
        rating: Rating::Good,
        answered_at_millis: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64,
        milliseconds_taken: 3000,
    })?;
    assert!(changes.recv().unwrap().card);
    // The live WAL file refuses before SQLite's lock is even tried; with the
    // override the lock still refuses.
    assert!(matches!(
        CollectionActor::open(dir.path()),
        Err(CoreError::ProfileBusy(_))
    ));
    assert!(matches!(
        CollectionActor::open_with_options(
            dir.path(),
            OpenOptions {
                confirmed_closed: true,
                ..Default::default()
            }
        ),
        Err(CoreError::Locked)
    ));
    actor.execute(Command::Undo)?;
    let Response::Queue(q) = actor.execute(Command::Queue { limit: 1 })? else {
        panic!("queue")
    };
    assert_eq!(q.new, 1);
    actor.close()?;
    CollectionActor::open(dir.path())?.close()?;
    assert!(dir.path().join("collection.media").is_dir());
    assert!(!dir.path().join("collection.mdb").exists());
    Ok(())
}

#[test]
fn fixture_contains_all_stock_types_and_survives_drop() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actor = CollectionActor::open(dir.path())?;
    let Response::CardIds(ids) = actor.execute(Command::GenerateFixture)? else {
        panic!("ids")
    };
    assert!(ids.len() >= 8, "{ids:?}");
    for card_id in ids {
        actor.execute(Command::Render { card_id })?;
    }
    drop(actor);
    CollectionActor::open(dir.path())?.close()?;
    Ok(())
}

#[test]
fn symlink_escape_is_rejected() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let other = tempfile::tempdir()?;
    std::os::unix::fs::symlink(other.path(), dir.path().join("collection.media"))?;
    assert!(matches!(
        CollectionActor::open(dir.path()),
        Err(CoreError::UnsafePath(_))
    ));
    assert!(!dir.path().join("collection.anki2").exists());
    Ok(())
}

#[test]
fn data_operations_poll_without_blocking_and_report_idle_progress() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actor = CollectionActor::open(dir.path())?;
    assert_eq!(actor.progress().stage, "");
    actor.cancel_operation();
    // An import of a missing file fails, stub or not.
    let pending = actor.start_data(DataCommand::Package(PackageCommand::Import {
        path: dir.path().join("missing.apkg"),
        options: None,
        schema_confirmed: false,
    }))?;
    // The cancel flag is reset when the operation starts.
    let result = loop {
        match pending.try_recv() {
            Ok(Some(response)) => break Ok(response),
            Ok(None) => std::thread::yield_now(),
            Err(error) => break Err(error),
        }
    };
    assert!(result.is_err());
    match actor.start_data(DataCommand::ListBackups)?.wait()? {
        DataResponse::Backups(list) => assert_eq!(list.len(), 1),
        other => panic!("{other:?}"),
    }
    // The actor keeps serving after a failed data operation.
    assert!(matches!(
        actor.execute(Command::DeckTree)?,
        Response::DeckTree(_)
    ));
    actor.close()
}
