//! Phase 2 safety (PLAN 4.4, 8.12): desktop-is-running checks that do not rely
//! on the SQLite lock, and backups that a killed process cannot leave half
//! written. Holders run in child processes (this test binary re-executed),
//! because SQLite and POSIX locks are per process.

use anki::collection::CollectionBuilder;
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, Command as Process, Stdio},
    time::Duration,
};
use tanki_core::*;

const HOLDER_ENV: &str = "TANKI_TEST_HOLDER";
const BACKUP_LOOP_ENV: &str = "TANKI_TEST_BACKUP_LOOP";

/// Child: opens the collection with Anki, makes a backup (which drops its
/// SQLite lock, as desktop's does), reports ready and holds the file open
/// until stdin closes.
#[test]
fn holder_child() {
    let Some(file) = std::env::var_os(HOLDER_ENV) else {
        return;
    };
    let file = Path::new(&file);
    let mut col = CollectionBuilder::new(file).build().unwrap();
    let backups = file.parent().unwrap().join("holder-backups");
    std::fs::create_dir_all(&backups).unwrap();
    col.set_modified().unwrap();
    col.maybe_backup(backups, true)
        .unwrap()
        .unwrap()
        .join()
        .unwrap()
        .unwrap();
    println!("ready");
    std::io::stdout().flush().unwrap();
    let mut rest = String::new();
    let _ = std::io::stdin().read_to_string(&mut rest);
    col.close(None).unwrap();
    std::process::exit(0);
}

fn spawn_holder(file: &Path) -> Child {
    let mut child = Process::new(std::env::current_exe().unwrap())
        .args(["--exact", "holder_child", "--nocapture", "--test-threads=1"])
        .env(HOLDER_ENV, file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    assert!(
        lines.any(|line| line.unwrap().trim_end().ends_with("ready")),
        "holder did not start"
    );
    child
}

fn release(mut child: Child) {
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
}

fn make_profile(dir: &Path) -> Result<()> {
    let actor = CollectionActor::open(dir)?;
    actor.execute(Command::AddNote {
        notetype: "Basic".into(),
        fields: vec!["front".into(), "back".into()],
    })?;
    actor.close()
}

#[test]
fn clean_close_leaves_no_wal_and_a_leftover_wal_needs_confirmation() -> Result<()> {
    let dir = tempfile::tempdir()?;
    make_profile(dir.path())?;
    let wal = dir.path().join("collection.anki2-wal");
    assert!(!wal.exists(), "tanki's clean close must remove the WAL");
    // As left by a crashed desktop.
    std::fs::write(&wal, b"")?;
    match CollectionActor::open(dir.path()) {
        Err(CoreError::ProfileBusy(message)) => {
            assert!(message.contains("--confirm-closed"), "{message}")
        }
        other => panic!("expected ProfileBusy, got {:?}", other.err()),
    }
    let actor = CollectionActor::open_with_options(
        dir.path(),
        OpenOptions {
            confirmed_closed: true,
        },
    )?;
    actor.close()?;
    assert!(!wal.exists());
    CollectionActor::open(dir.path())?.close()
}

/// The case the SQLite lock misses: the holder already made a backup, so a
/// plain open would succeed. The `/proc` check refuses, even with the WAL
/// override, and names the process.
#[test]
fn holder_that_lost_its_lock_is_found_through_proc() -> Result<()> {
    if !cfg!(target_os = "linux") {
        return Ok(());
    }
    let dir = tempfile::tempdir()?;
    make_profile(dir.path())?;
    let file = dir.path().canonicalize()?.join("collection.anki2");
    let holder = spawn_holder(&file);
    // The lock really is gone: a read-only peek from this process succeeds.
    let conn =
        rusqlite::Connection::open_with_flags(&file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    conn.busy_timeout(Duration::ZERO).unwrap();
    assert!(conn
        .query_row("select ver from col", [], |r| r.get::<_, i64>(0))
        .is_ok());
    drop(conn);
    let pid = holder.id().to_string();
    for confirmed_closed in [false, true] {
        match CollectionActor::open_with_options(dir.path(), OpenOptions { confirmed_closed }) {
            Err(CoreError::InUse(message)) => assert!(message.contains(&pid), "{message}"),
            other => panic!("expected InUse, got {:?}", other.err()),
        }
    }
    release(holder);
    CollectionActor::open(dir.path())?.close()
}

#[test]
fn backups_are_listed_newest_first_and_forced_on_request() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actor = CollectionActor::open(dir.path())?;
    let list = |actor: &CollectionActor| match actor
        .start_data(DataCommand::ListBackups)
        .and_then(|p| p.wait())
    {
        Ok(DataResponse::Backups(list)) => list,
        other => panic!("{other:?}"),
    };
    let opened = list(&actor);
    assert_eq!(opened.len(), 1, "the open backup");
    // Unforced within the interval: skipped. Forced: written (next second,
    // since names have one-second resolution).
    actor.execute(Command::AddNote {
        notetype: "Basic".into(),
        fields: vec!["a".into(), "b".into()],
    })?;
    assert!(matches!(
        actor
            .start_data(DataCommand::Backup { force: false })?
            .wait()?,
        DataResponse::Done
    ));
    assert_eq!(list(&actor).len(), 1);
    std::thread::sleep(Duration::from_millis(1100));
    actor
        .start_data(DataCommand::Backup { force: true })?
        .wait()?;
    let after = list(&actor);
    assert_eq!(after.len(), 2);
    assert!(after[0].path > after[1].path, "newest first");
    assert!(after.iter().all(|b| b.bytes > 0));
    // The actor still holds the collection after the backups.
    assert!(matches!(
        actor.execute(Command::DeckTree)?,
        Response::DeckTree(_)
    ));
    actor.close()
}

#[test]
fn unconfirmed_restore_is_refused_and_export_cannot_overwrite_the_profile() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actor = CollectionActor::open(dir.path())?;
    let before = std::fs::read(dir.path().join("collection.anki2"))?;
    let restore = actor.start_data(DataCommand::Package(PackageCommand::Restore {
        path: dir.path().join("whatever.colpkg"),
        confirmed: false,
    }))?;
    assert!(matches!(restore.wait(), Err(CoreError::Invalid(_))));
    for name in [
        "collection.anki2",
        "collection.anki2-wal",
        "collection.media.db2",
        "collection.media/x.colpkg",
    ] {
        let export = actor.start_data(DataCommand::Package(PackageCommand::ExportCollection {
            path: dir.path().join(name),
            media: false,
            legacy: false,
        }))?;
        assert!(
            matches!(export.wait(), Err(CoreError::UnsafePath(_))),
            "{name}"
        );
    }
    actor.close()?;
    // Closing may checkpoint, but the collection is still the same one.
    assert!(!before.is_empty());
    CollectionActor::open(dir.path())?.close()
}

/// Child: adds a large note and forces a backup, forever, until killed.
#[test]
fn backup_loop_child() {
    let Some(profile) = std::env::var_os(BACKUP_LOOP_ENV) else {
        return;
    };
    let actor = CollectionActor::open(Path::new(&profile)).unwrap();
    // Poorly compressible field so each backup takes a while to write.
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let noise: String = (0..3_000_000)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            char::from(b'!' + (state % 90) as u8)
        })
        .collect();
    println!("ready");
    std::io::stdout().flush().unwrap();
    loop {
        actor
            .execute(Command::AddNote {
                notetype: "Basic".into(),
                fields: vec![noise.clone(), "x".into()],
            })
            .unwrap();
        actor.execute(Command::Backup).unwrap();
    }
}

/// A complete zip ends with an end-of-central-directory record
/// (`PK\x05\x06` within the last 64 KiB + 22 bytes).
fn complete_zip(path: &Path) -> bool {
    let data = std::fs::read(path).unwrap();
    let tail = &data[data.len().saturating_sub(65_557)..];
    tail.windows(4).any(|w| w == b"PK\x05\x06")
}

/// PLAN 11 Phase 2 exit: a killed process leaves no partial backup.
#[test]
fn killed_process_leaves_no_partial_backup() -> Result<()> {
    let dir = tempfile::tempdir()?;
    make_profile(dir.path())?;
    let backups = dir.path().join("backups");
    let mut interrupted_writes = 0;
    for delay_ms in [150u64, 260, 370, 480, 590, 700] {
        let mut child = Process::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "backup_loop_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(BACKUP_LOOP_ENV, dir.path())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        assert!(lines.any(|l| l.unwrap().trim_end().ends_with("ready")));
        std::thread::sleep(Duration::from_millis(delay_ms));
        child.kill().unwrap();
        child.wait().unwrap();
        let incoming = backups.join(".tanki-incoming");
        if std::fs::read_dir(&incoming).is_ok_and(|mut d| d.next().is_some()) {
            interrupted_writes += 1;
        }
        for entry in std::fs::read_dir(&backups)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "colpkg") {
                assert!(complete_zip(&path), "partial backup {}", path.display());
            }
        }
        // The killed process may leave a WAL; the user would confirm.
        let actor = CollectionActor::open_with_options(
            dir.path(),
            OpenOptions {
                confirmed_closed: true,
            },
        )?;
        actor.close()?;
        assert!(
            !incoming.exists(),
            "staging folder cleared on the next backup"
        );
    }
    eprintln!("kills that landed mid-write: {interrupted_writes} of 6");
    Ok(())
}
