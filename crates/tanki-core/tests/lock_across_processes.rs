//! Cross-process view of the collection lock (PLAN 4.4, R5 §1.5).
//!
//! The probe runs in a child process (this test binary re-executed), because
//! SQLite tracks locks per process: a second connection in the same process is
//! refused from SQLite's own bookkeeping even when the kernel lock is gone.

use anki::collection::CollectionBuilder;
use std::path::Path;

const PROBE_ENV: &str = "TANKI_LOCK_PROBE_FILE";
const LOCKED: i32 = 20;

/// Child side: the read-only schema peek tanki does before opening.
fn probe(file: &Path) -> i32 {
    let conn =
        rusqlite::Connection::open_with_flags(file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    conn.busy_timeout(std::time::Duration::ZERO).unwrap();
    match conn.query_row("select ver from col", [], |row| row.get::<_, i64>(0)) {
        Ok(_) => 0,
        Err(rusqlite::Error::SqliteFailure(error, _))
            if matches!(
                error.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) =>
        {
            LOCKED
        }
        Err(other) => panic!("unexpected probe error: {other}"),
    }
}

fn locked_for_another_process(file: &Path) -> bool {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "probe_child", "--nocapture"])
        .env(PROBE_ENV, file)
        .status()
        .unwrap();
    match status.code() {
        Some(0) => false,
        Some(LOCKED) => true,
        other => panic!("probe child failed: {other:?}"),
    }
}

#[test]
fn probe_child() {
    if let Some(file) = std::env::var_os(PROBE_ENV) {
        std::process::exit(probe(Path::new(&file)));
    }
}

/// Upstream behaviour tanki has to work around. If this starts failing after a
/// pin bump, Anki fixed its backup and `backup_and_relock` can go.
#[test]
fn upstream_backup_drops_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("collection.anki2");
    let backups = dir.path().join("backups");
    std::fs::create_dir(&backups).unwrap();
    let mut col = CollectionBuilder::new(&file).build().unwrap();
    assert!(locked_for_another_process(&file));
    let handle = col.maybe_backup(backups, true).unwrap().expect("backup");
    handle.join().unwrap().unwrap();
    assert!(
        !locked_for_another_process(&file),
        "upstream backup no longer drops the lock; see backup_and_relock"
    );
    col.close(None).unwrap();
}

fn backups(profile: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut files: Vec<_> = std::fs::read_dir(profile.join("backups"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .map(|path| (path.clone(), std::fs::read(path).unwrap()))
        .collect();
    files.sort();
    files
}

#[test]
fn actor_holds_the_lock_after_every_backup() -> tanki_core::Result<()> {
    use tanki_core::{CollectionActor, Command};
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("collection.anki2");
    // Opening a new collection forces the first backup.
    let actor = CollectionActor::open(dir.path())?;
    let first = backups(dir.path());
    assert_eq!(first.len(), 1);
    assert!(locked_for_another_process(&file), "after the open backup");
    actor.execute(Command::AddNote {
        notetype: "Basic".into(),
        fields: vec!["question".into(), "answer".into()],
    })?;
    actor.execute(Command::Backup)?;
    // Backup names have one-second resolution, so compare contents too.
    assert_ne!(backups(dir.path()), first, "no session backup was written");
    assert!(locked_for_another_process(&file), "after a session backup");
    // Nothing changed since: no backup is written and the session continues.
    actor.execute(Command::Backup)?;
    actor.execute(Command::DeckTree)?;
    assert!(locked_for_another_process(&file));
    actor.close()?;
    assert!(!locked_for_another_process(&file), "released on close");
    Ok(())
}

#[test]
fn actor_holds_the_lock_after_check_database_and_media() -> tanki_core::Result<()> {
    use tanki_core::maintenance::MaintenanceCommand;
    use tanki_core::{CollectionActor, Command, ManageCommand};
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("collection.anki2");
    let actor = CollectionActor::open(dir.path())?;
    actor.execute(Command::GenerateStudyFixture)?;
    for command in [
        MaintenanceCommand::CheckDatabase { confirmed: true },
        MaintenanceCommand::CheckMedia,
    ] {
        let name = format!("{command:?}");
        actor.execute(Command::Manage(ManageCommand::Maintenance(command)))?;
        assert!(locked_for_another_process(&file), "after {name}");
        actor.execute(Command::DeckTree)?;
    }
    actor.close()?;
    Ok(())
}
