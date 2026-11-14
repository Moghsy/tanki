//! Installing a staged collection over the live one (restore, full download).
//!
//! Anki's own restore and full download rename a new file over
//! `collection.anki2` without checking for other processes, and a desktop
//! session that has made a backup no longer holds SQLite's lock (PLAN 4.4).
//! tanki installs only while it holds its own locks and no other process has
//! either file open:
//!
//! 1. The caller has closed the live collection after a completed backup, and
//!    the staged collection is closed too, so neither has a WAL file.
//! 2. Open both files and take Linux open-file-description (OFD) write locks
//!    over SQLite's lock bytes (pending, reserved and the shared range). OFD
//!    locks conflict with the POSIX locks SQLite uses in every process, so
//!    any connection that still holds or later requests a lock is refused.
//! 3. Re-check `/proc` for other holders of either file or their companions;
//!    an SQLite connection that lost its lock still has the file open.
//! 4. Check that the locked descriptors are still the files at the two paths,
//!    on one filesystem, and that no WAL appeared. Remove stale shm index
//!    files, which belong to the old file and which SQLite would rebuild.
//! 5. Move staged media in, then rename the staged collection over the live
//!    one and sync the folder. Failure before the rename leaves the live
//!    collection untouched.
//! 6. Drop the locks. The caller reopens the collection right away. The
//!    descriptors must be closed before SQLite opens the file: closing any
//!    descriptor of a file drops every POSIX lock this process holds on it.
//!
//! Other platforms refuse, until equivalent locking exists there.
use crate::{safety, CoreError, Result};
use std::path::Path;

/// SQLite's lock bytes (`os_unix.c`): PENDING_BYTE at 1 GiB, then
/// RESERVED_BYTE, then SHARED_SIZE (510) bytes of the shared range.
#[cfg(target_os = "linux")]
const LOCK_START: i64 = 0x4000_0000;
#[cfg(target_os = "linux")]
const LOCK_LEN: i64 = 2 + 510;

/// Name of the staging folder, inside the profile so the final rename stays
/// on one filesystem. Never a profile tanki opens as such.
pub(crate) fn new_stage(profile: &Path) -> Result<std::path::PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| CoreError::Other(e.to_string()))?
        .as_nanos();
    let stage = profile.join(format!(".tanki-stage-{stamp}"));
    std::fs::create_dir(&stage)?;
    Ok(stage)
}

/// Removes a staging folder made by `new_stage`.
pub(crate) fn remove_stage(stage: &Path) {
    let named = stage
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(".tanki-stage-"));
    if named {
        let _ = std::fs::remove_dir_all(stage);
    }
}

#[cfg(target_os = "linux")]
mod ofd {
    use super::*;
    use std::os::fd::AsRawFd;

    pub(super) struct Locked(#[allow(dead_code)] std::fs::File);

    pub(super) fn lock(path: &Path) -> Result<Locked> {
        // Write access is needed for a write lock; nothing is written.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        // SAFETY: plain fcntl on an owned descriptor with a zeroed flock.
        let mut lock: libc::flock = unsafe { std::mem::zeroed() };
        lock.l_type = libc::F_WRLCK as _;
        lock.l_whence = libc::SEEK_SET as _;
        lock.l_start = LOCK_START;
        lock.l_len = LOCK_LEN;
        lock.l_pid = 0;
        let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_OFD_SETLK, &lock) };
        if result == -1 {
            let error = std::io::Error::last_os_error();
            return Err(match error.raw_os_error() {
                Some(libc::EAGAIN | libc::EACCES) => CoreError::InUse(format!(
                    "{} is in use by another program; nothing was replaced.",
                    path.display()
                )),
                _ => error.into(),
            });
        }
        Ok(Locked(file))
    }

    pub(super) fn identity(file: &Locked) -> Result<(u64, u64)> {
        use std::os::unix::fs::MetadataExt;
        let meta = file.0.metadata()?;
        Ok((meta.dev(), meta.ino()))
    }
}

/// Installs `stage/collection.anki2` (and, if `media`, the files in
/// `stage/collection.media`) into `profile`. See the module comment.
#[cfg(target_os = "linux")]
pub(crate) fn install(profile: &Path, stage: &Path, media: bool) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let live = profile.join("collection.anki2");
    let staged = stage.join("collection.anki2");
    // A WAL holds data that belongs to the old file. The shm file is only an
    // index SQLite rebuilds on open; a clean close leaves it behind, and it is
    // removed below once nobody else can be using it.
    let companions =
        |dir: &Path| std::fs::symlink_metadata(dir.join("collection.anki2-wal")).is_ok();
    if companions(profile) || companions(stage) {
        return Err(CoreError::ProfileBusy(
            "A WAL file exists beside a collection; nothing was replaced.".into(),
        ));
    }
    for path in [&live, &staged] {
        let meta = std::fs::symlink_metadata(path)?;
        if !meta.is_file() {
            return Err(CoreError::UnsafePath(format!(
                "{} is not a regular file",
                path.display()
            )));
        }
    }
    let live_lock = ofd::lock(&live)?;
    let staged_lock = ofd::lock(&staged)?;
    let mut watched = safety::collection_files(profile);
    watched.extend(safety::collection_files(stage));
    let holders = safety::other_holders(&watched);
    if !holders.is_empty() {
        return Err(CoreError::InUse(format!(
            "Another program has the collection open ({}); nothing was replaced.",
            holders
                .iter()
                .map(|(pid, name)| format!("{name} pid {pid}"))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let path_identity = |p: &Path| -> Result<(u64, u64)> {
        let m = std::fs::symlink_metadata(p)?;
        Ok((m.dev(), m.ino()))
    };
    let (live_id, staged_id) = (ofd::identity(&live_lock)?, ofd::identity(&staged_lock)?);
    if path_identity(&live)? != live_id
        || path_identity(&staged)? != staged_id
        || live_id.0 != staged_id.0
        || companions(profile)
        || companions(stage)
    {
        return Err(CoreError::ProfileBusy(
            "The collection files changed while locking; nothing was replaced.".into(),
        ));
    }
    for dir in [profile, stage] {
        let shm = dir.join("collection.anki2-shm");
        if std::fs::symlink_metadata(&shm).is_ok() {
            std::fs::remove_file(&shm)?;
        }
    }
    if media {
        install_media(
            &stage.join("collection.media"),
            &profile.join("collection.media"),
        )?;
    }
    std::fs::rename(&staged, &live)?;
    std::fs::File::open(profile)?.sync_all()?;
    drop(staged_lock);
    drop(live_lock);
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn install(_profile: &Path, _stage: &Path, _media: bool) -> Result<()> {
    Err(CoreError::Invalid(
        "Replacing a collection is only supported on Linux so far.".into(),
    ))
}

/// Moves restored media files in, replacing same-named files as Anki's
/// restore does. Only regular files directly in the folder; Anki already
/// sanitized their names when it extracted them.
#[cfg(target_os = "linux")]
fn install_media(from: &Path, to: &Path) -> Result<()> {
    if !from.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(to)?;
    if std::fs::symlink_metadata(to)?.file_type().is_symlink() {
        return Err(CoreError::UnsafePath(
            "collection.media is a symlink".into(),
        ));
    }
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        std::fs::File::open(entry.path())?.sync_all()?;
        std::fs::rename(entry.path(), to.join(entry.file_name()))?;
    }
    std::fs::File::open(to)?.sync_all()?;
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use anki::collection::{Collection, CollectionBuilder};
    use anki::services::{NotesService, SearchService};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::process::{Child, Command, Stdio};

    const HOLD: &str = "TANKI_REPLACEMENT_HOLD";
    const AFTER_BACKUP: &str = "TANKI_REPLACEMENT_AFTER_BACKUP";

    fn col(path: &Path) -> Collection {
        CollectionBuilder::new(path)
            .set_media_paths(
                path.with_file_name("collection.media"),
                path.with_file_name("collection.media.db2"),
            )
            .build()
            .unwrap()
    }

    fn add_note(col: &mut Collection, front: &str) {
        let notetype = col.get_notetype_by_name("Basic").unwrap().unwrap();
        let mut note = notetype.new_note();
        note.set_field(0, front).unwrap();
        col.add_note(&mut note, anki::prelude::DeckId(1)).unwrap();
    }

    fn fronts(path: &Path) -> Vec<String> {
        let mut col = col(path);
        let ids = SearchService::search_notes(
            &mut col,
            anki_proto::search::SearchRequest {
                search: String::new(),
                order: None,
            },
        )
        .unwrap()
        .ids;
        let fronts = ids
            .into_iter()
            .map(|nid| {
                NotesService::get_note(&mut col, anki_proto::notes::NoteId { nid })
                    .unwrap()
                    .fields[0]
                    .clone()
            })
            .collect();
        col.close(None).unwrap();
        fronts
    }

    /// Two closed collections: live (note "old") and staged (note "new").
    fn setup() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().canonicalize().unwrap();
        let mut live = col(&profile.join("collection.anki2"));
        add_note(&mut live, "old");
        live.close(None).unwrap();
        let stage = new_stage(&profile).unwrap();
        let mut staged = col(&stage.join("collection.anki2"));
        add_note(&mut staged, "new");
        staged.close(None).unwrap();
        (dir, profile, stage)
    }

    #[test]
    fn holder_child() {
        let Some(file) = std::env::var_os(HOLD) else {
            return;
        };
        let mut col = col(Path::new(&file));
        // Touch the database so SQLite takes its exclusive lock.
        add_note(&mut col, "held");
        if std::env::var_os(AFTER_BACKUP).is_some() {
            let folder = Path::new(&file).with_file_name("child-backups");
            std::fs::create_dir_all(&folder).unwrap();
            col.maybe_backup(folder, true)
                .unwrap()
                .unwrap()
                .join()
                .unwrap()
                .unwrap();
        }
        println!("ready");
        std::io::stdout().flush().unwrap();
        let _ = std::io::stdin().read_to_string(&mut String::new());
        col.close(None).unwrap();
        std::process::exit(0);
    }

    fn hold(file: &Path, after_backup: bool) -> Child {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "replacement::tests::holder_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(HOLD, file)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        if after_backup {
            command.env(AFTER_BACKUP, "1");
        }
        let mut child = command.spawn().unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        assert!(lines.any(|l| l.unwrap().trim_end().ends_with("ready")));
        child
    }

    fn release(mut child: Child) {
        drop(child.stdin.take());
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn installs_collection_and_media_when_nobody_holds_them() {
        let (_dir, profile, stage) = setup();
        std::fs::create_dir_all(stage.join("collection.media")).unwrap();
        std::fs::write(stage.join("collection.media/a.png"), b"restored").unwrap();
        std::fs::create_dir_all(profile.join("collection.media")).unwrap();
        std::fs::write(profile.join("collection.media/a.png"), b"live").unwrap();
        std::fs::write(profile.join("collection.media/b.png"), b"kept").unwrap();
        install(&profile, &stage, true).unwrap();
        assert_eq!(fronts(&profile.join("collection.anki2")), ["new"]);
        assert!(!stage.join("collection.anki2").exists());
        assert_eq!(
            std::fs::read(profile.join("collection.media/a.png")).unwrap(),
            b"restored"
        );
        assert_eq!(
            std::fs::read(profile.join("collection.media/b.png")).unwrap(),
            b"kept"
        );
        remove_stage(&stage);
        assert!(!stage.exists());
    }

    /// Restore-while-locked (PLAN 11 Phase 2 exit): a holder with SQLite's
    /// lock is refused by the OFD lock; one that lost its lock after a
    /// backup is refused by the `/proc` check. The live file is untouched.
    #[test]
    fn refuses_while_another_process_holds_the_collection() {
        for after_backup in [false, true] {
            let (_dir, profile, stage) = setup();
            let live = profile.join("collection.anki2");
            let child = hold(&live, after_backup);
            let before = std::fs::read(&live).unwrap();
            // The holder's WAL would refuse first; move it aside to test the
            // locks themselves (the child keeps it open).
            let wal = profile.join("collection.anki2-wal");
            let hidden = profile.join("held-wal");
            let had_wal = wal.exists();
            if had_wal {
                std::fs::rename(&wal, &hidden).unwrap();
            }
            match install(&profile, &stage, false) {
                Err(CoreError::InUse(message)) => {
                    if after_backup {
                        // Lost its lock: only the /proc check catches it.
                        assert!(message.contains(&child.id().to_string()), "{message}");
                    } else {
                        // Still holds SQLite's lock: the OFD lock is refused.
                        assert!(message.contains("in use by another program"), "{message}");
                    }
                }
                other => panic!("after_backup={after_backup}: expected InUse, got {other:?}"),
            }
            assert_eq!(std::fs::read(&live).unwrap(), before);
            assert!(stage.join("collection.anki2").exists());
            if had_wal {
                std::fs::rename(&hidden, &wal).unwrap();
            }
            release(child);
            assert!(fronts(&live).contains(&"old".to_string()));
        }
    }

    #[test]
    fn refuses_beside_a_wal_file() {
        let (_dir, profile, stage) = setup();
        std::fs::write(profile.join("collection.anki2-wal"), b"").unwrap();
        assert!(matches!(
            install(&profile, &stage, false),
            Err(CoreError::ProfileBusy(_))
        ));
        assert_eq!(fronts(&stage.join("collection.anki2")), ["new"]);
    }
}
