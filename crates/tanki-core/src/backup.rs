//! Backups (PLAN 8.12): Anki's schedule and retention, written atomically.
//!
//! Anki's `maybe_backup` creates `backups/backup-<time>.colpkg` with a plain
//! `File::create` and fills it from a background thread, so a process killed
//! mid-write leaves a truncated package where desktop's "Open Backup" lists it.
//! tanki has Anki write into `backups/.tanki-incoming/` instead, waits for the
//! thread, syncs the file and renames it into `backups/`. Because Anki then
//! only thins the staging folder, the interval check and the retention filter
//! run here, ported from `rslib/src/collection/backup.rs` (26.09.4) and
//! checked against Anki's own thinning in a parity test.
use crate::{BackupInfo, CoreError, Result};
use anki::{collection::Collection, services::ConfigService};
use anki_proto::config::preferences::BackupLimits;
use chrono::{DateTime, Datelike, Local, NaiveDateTime, TimeZone};
use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

const FORMAT: &str = "backup-%Y-%m-%d-%H.%M.%S.colpkg";
pub(crate) const INCOMING: &str = ".tanki-incoming";

/// The collection's backup limits under Anki's `backups` key. Read directly
/// rather than through `get_preferences`, which refuses on v1/v2 schedulers.
fn limits(col: &mut Collection) -> Result<BackupLimits> {
    let stored = ConfigService::get_config_json(col, "backups".to_owned().into())
        .ok()
        .and_then(|json| serde_json::from_slice::<BackupLimits>(&json.json).ok());
    Ok(stored.unwrap_or(BackupLimits {
        daily: 12,
        weekly: 10,
        monthly: 9,
        minimum_interval_mins: 30,
    }))
}

/// Writes a backup unless `force` is false and one was made within the
/// collection's minimum interval, or nothing changed since this session's last
/// backup. Waits for Anki's backup thread. Returns whether Anki read the
/// collection file, which drops this process's SQLite lock (see
/// `actor::backup_and_relock`).
pub(crate) fn create(col: &mut Collection, profile: &Path, force: bool) -> Result<bool> {
    let folder = profile.join("backups");
    fs::create_dir_all(&folder)?;
    let limits = limits(col)?;
    if !force && has_recent_backup(&folder, limits.minimum_interval_mins)? {
        return Ok(false);
    }
    let incoming = folder.join(INCOMING);
    clear_incoming(&folder)?;
    fs::create_dir(&incoming)?;
    // Anki's interval check sees the empty staging folder, so this always
    // writes when the collection changed; the real check ran above.
    let result = match col.maybe_backup(incoming.clone(), true) {
        Ok(Some(handle)) => handle
            .join()
            .map_err(|_| CoreError::Other("Backup worker panicked".into()))?
            .map_err(CoreError::from)
            .and_then(|()| install(&incoming, &folder))
            .and_then(|()| thin(&folder, limits, Local::now()))
            .map(|()| true),
        Ok(None) => Ok(false),
        Err(error) => Err(error.into()),
    };
    let cleared = clear_incoming(&folder);
    let wrote = result?;
    cleared?;
    Ok(wrote)
}

/// Removes the staging folder and anything a killed process left in it.
pub(crate) fn clear_incoming(folder: &Path) -> Result<()> {
    let incoming = folder.join(INCOMING);
    match fs::symlink_metadata(&incoming) {
        Ok(meta) if meta.is_dir() => Ok(fs::remove_dir_all(&incoming)?),
        Ok(_) => Err(CoreError::UnsafePath(format!(
            "{} is not a folder",
            incoming.display()
        ))),
        Err(_) => Ok(()),
    }
}

/// Moves every finished backup from the staging folder into `folder`.
fn install(incoming: &Path, folder: &Path) -> Result<()> {
    for entry in fs::read_dir(incoming)? {
        let entry = entry?;
        let Some(mut time) = entry.file_name().to_str().and_then(datetime_from_file_name) else {
            continue;
        };
        fs::File::open(entry.path())?.sync_all()?;
        // Names have one-second resolution; in Anki a second backup within
        // the same second replaces the first. Keep both: take the next free
        // second instead.
        let mut target = folder.join(time.format(FORMAT).to_string());
        while fs::symlink_metadata(&target).is_ok() {
            time += chrono::Duration::seconds(1);
            target = folder.join(time.format(FORMAT).to_string());
        }
        fs::rename(entry.path(), target)?;
    }
    fs::File::open(folder)?.sync_all()?;
    Ok(())
}

/// Backup packages in `folder`, newest first.
pub(crate) fn list(profile: &Path) -> Result<Vec<BackupInfo>> {
    let folder = profile.join("backups");
    let mut found = Vec::new();
    if !folder.exists() {
        return Ok(Vec::new());
    }
    for entry in fs::read_dir(&folder)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        let Some(time) = entry.file_name().to_str().and_then(datetime_from_file_name) else {
            continue;
        };
        if meta.is_file() {
            found.push((
                time,
                BackupInfo {
                    path: entry.path(),
                    bytes: meta.len(),
                },
            ));
        }
    }
    found.sort_by_key(|(time, _)| std::cmp::Reverse(*time));
    Ok(found.into_iter().map(|(_, info)| info).collect())
}

/// Anki's `has_recent_backup`, ignoring tanki's staging folder.
fn has_recent_backup(folder: &Path, recent_mins: u32) -> Result<bool> {
    let recent_secs = u64::from(recent_mins) * 60;
    let now = SystemTime::now();
    Ok(fs::read_dir(folder)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name() != INCOMING)
        .filter_map(|entry| entry.metadata().ok())
        .filter_map(|meta| meta.created().ok())
        .filter_map(|time| now.duration_since(time).ok())
        .any(|age| age.as_secs() < recent_secs))
}

fn datetime_from_file_name(name: &str) -> Option<DateTime<Local>> {
    NaiveDateTime::parse_from_str(name, FORMAT)
        .ok()
        .and_then(|time| Local.from_local_datetime(&time).latest())
}

/// Deletes the backups Anki's retention scheme marks obsolete.
pub(crate) fn thin(folder: &Path, limits: BackupLimits, now: DateTime<Local>) -> Result<()> {
    let mut backups = Vec::new();
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        if let Some(time) = entry.file_name().to_str().and_then(datetime_from_file_name) {
            backups.push((entry.path(), time));
        }
    }
    for path in obsolete(backups, limits, now) {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// Port of Anki's `BackupFilter::obsolete_backups`.
fn obsolete(
    mut backups: Vec<(PathBuf, DateTime<Local>)>,
    mut limits: BackupLimits,
    now: DateTime<Local>,
) -> Vec<PathBuf> {
    let day = |t: &DateTime<Local>| t.num_days_from_ce();
    // Serial week number starting on Monday (day 1 was a Monday).
    let week = |t: &DateTime<Local>| (day(t) - 1) / 7;
    let month = |t: &DateTime<Local>| t.year() as u32 * 12 + t.month();
    let yesterday = day(&now) - 1;
    let (mut last_day, mut last_week, mut last_month) = (i32::MAX, i32::MAX, u32::MAX);
    let mut obsolete = Vec::new();
    backups.sort_unstable_by_key(|(_, t)| std::cmp::Reverse(t.timestamp()));
    for (path, t) in backups {
        let stage = if day(&t) >= yesterday {
            Some(None)
        } else if limits.daily > 0 {
            (day(&t) < last_day).then_some(Some(&mut limits.daily))
        } else if limits.weekly > 0 {
            (week(&t) < last_week).then_some(Some(&mut limits.weekly))
        } else if limits.monthly > 0 {
            (month(&t) < last_month).then_some(Some(&mut limits.monthly))
        } else {
            None
        };
        match stage {
            Some(counter) => {
                last_day = day(&t);
                last_week = week(&t);
                last_month = month(&t);
                if let Some(counter) = counter {
                    *counter -= 1;
                }
            }
            None => obsolete.push(path),
        }
    }
    obsolete
}

#[cfg(test)]
mod tests {
    use super::*;
    use anki::collection::CollectionBuilder;
    use std::collections::BTreeSet;

    fn names(folder: &Path) -> BTreeSet<String> {
        fs::read_dir(folder)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect()
    }

    /// Anki thins the folder it writes into; the port must keep and delete
    /// exactly the same files given the same history.
    #[test]
    fn retention_matches_anki() {
        let dir = tempfile::tempdir().unwrap();
        let (anki_dir, ours) = (dir.path().join("anki"), dir.path().join("ours"));
        fs::create_dir(&anki_dir).unwrap();
        fs::create_dir(&ours).unwrap();
        let now = Local::now();
        // Two years of history with several backups on some days.
        for hours in (6..24 * 730).step_by(7) {
            let time = now - chrono::Duration::hours(hours);
            let name = time.format(FORMAT).to_string();
            fs::write(anki_dir.join(&name), b"").unwrap();
            fs::write(ours.join(&name), b"").unwrap();
        }
        let mut col = CollectionBuilder::new(dir.path().join("c.anki2"))
            .build()
            .unwrap();
        let anki_dir2 = anki_dir.clone();
        col.maybe_backup(anki_dir2, true)
            .unwrap()
            .unwrap()
            .join()
            .unwrap()
            .unwrap();
        let limits = limits(&mut col).unwrap();
        col.close(None).unwrap();
        // Mirror the file Anki just wrote, then thin with the port.
        for name in names(&anki_dir).difference(&names(&ours)) {
            fs::write(ours.join(name), b"").unwrap();
        }
        thin(&ours, limits, Local::now()).unwrap();
        let kept = names(&ours);
        assert_eq!(kept, names(&anki_dir));
        assert!(kept.len() > 20 && kept.len() < 60, "kept {}", kept.len());
    }

    #[test]
    fn interval_and_atomic_install() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path();
        let mut col = CollectionBuilder::new(profile.join("collection.anki2"))
            .build()
            .unwrap();
        // A partial file left by a killed process is removed, never installed.
        fs::create_dir_all(profile.join("backups").join(INCOMING)).unwrap();
        fs::write(
            profile
                .join("backups")
                .join(INCOMING)
                .join("backup-2020-01-01-00.00.00.colpkg"),
            b"partial",
        )
        .unwrap();
        assert!(create(&mut col, profile, true).unwrap());
        let listed = list(profile).unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].bytes > 100);
        assert!(!profile.join("backups").join(INCOMING).exists());
        // Within the 30-minute default interval an unforced backup is skipped.
        col.set_modified().unwrap();
        assert!(!create(&mut col, profile, false).unwrap());
        col.close(None).unwrap();
    }
}
