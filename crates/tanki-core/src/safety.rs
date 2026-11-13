use crate::{CoreError, Result};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// Every place a real Anki base can live on this machine (R5 §1.1). These are
/// refused during development even when `TANKI_ALLOW_REAL_PROFILE=1` is set;
/// both checks go away at Phase 2 exit (PLAN section 14 rule 2).
fn real_bases() -> Vec<PathBuf> {
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let mut bases = Vec::new();
    if let Some(home) = var("HOME") {
        bases.push(home.join(".local/share/Anki2"));
        bases.push(home.join(".var/app/net.ankiweb.Anki/data/Anki2"));
    }
    if let Some(data) = var("XDG_DATA_HOME") {
        bases.push(data.join("Anki2"));
    }
    bases.extend(var("ANKI_BASE"));
    bases
}

fn forbidden(path: &Path) -> bool {
    real_bases().iter().any(|base| path.starts_with(base))
        || path
            .components()
            .any(|c| c.as_os_str() == "prefs21.db" || c.as_os_str() == "addons21")
}

pub fn validate_profile(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    // Reject known real profile paths before any filesystem access.
    if forbidden(&absolute) {
        return Err(CoreError::UnsafePath(
            "Development may not access the real Anki base.".into(),
        ));
    }
    if absolute
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(CoreError::UnsafePath(
            "Profile paths must not contain '..'.".into(),
        ));
    }
    // Require an existing directory so canonicalization cannot overlook symlink ancestors.
    let canonical = absolute.canonicalize()?;
    // Compare again with symlinks in the bases' parents resolved. Only the
    // parent is resolved, so nothing inside a real base is ever touched.
    let resolved_bases = real_bases()
        .into_iter()
        .filter_map(|base| Some(base.parent()?.canonicalize().ok()?.join(base.file_name()?)));
    if forbidden(&canonical) || resolved_bases.into_iter().any(|b| canonical.starts_with(b)) {
        return Err(CoreError::UnsafePath(
            "Profile resolves to a forbidden Anki path.".into(),
        ));
    }
    let temp = std::env::temp_dir().canonicalize()?;
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .canonicalize()
        .ok();
    let allowed =
        canonical.starts_with(&temp) || fixtures.is_some_and(|p| canonical.starts_with(p));
    if !allowed && std::env::var("TANKI_ALLOW_REAL_PROFILE").as_deref() != Ok("1") {
        return Err(CoreError::UnsafePath(
            "Until Phase 2, use a temporary or fixture profile (--base).".into(),
        ));
    }
    // Collection and media symlinks could escape an otherwise safe base.
    for name in [
        "collection.anki2",
        "collection.anki2-wal",
        "collection.anki2-shm",
        "collection.media",
        "collection.media.db2",
        "backups",
    ] {
        let child = canonical.join(name);
        if fs::symlink_metadata(&child).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(CoreError::UnsafePath(format!(
                "Profile entries must not be symlinks: {name}"
            )));
        }
    }
    Ok(canonical)
}

pub(crate) fn peek_and_backup(profile: &Path) -> Result<()> {
    let file = profile.join("collection.anki2");
    if !file.exists() {
        return Ok(());
    }
    let conn =
        rusqlite::Connection::open_with_flags(&file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(sql_error)?;
    conn.busy_timeout(std::time::Duration::ZERO)
        .map_err(sql_error)?;
    let mut statement = conn.prepare("select ver from col").map_err(sql_error)?;
    let mut rows = statement.query([]).map_err(sql_error)?;
    let version: i64 = rows
        .next()
        .map_err(sql_error)?
        .ok_or_else(|| CoreError::Invalid("Missing collection schema".into()))?
        .get(0)
        .map_err(sql_error)?;
    if version > 18 || version == 12 || version == 13 {
        return Err(CoreError::SchemaTooNew(version));
    }
    if version < 18 {
        // The active SELECT holds a read lock while the bytes are read. (Closing
        // the copy's descriptor then drops that lock, as POSIX locks do; nothing
        // reads after that point.) Refuse WAL data rather than silently omitting
        // uncheckpointed pages.
        if fs::metadata(profile.join("collection.anki2-wal")).is_ok_and(|m| m.len() > 0) {
            return Err(CoreError::Invalid(
                "Close the older client cleanly before upgrading this collection (nonempty WAL)."
                    .into(),
            ));
        }
        fs::create_dir_all(profile.join("backups"))?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| CoreError::Other(e.to_string()))?
            .as_nanos();
        let dest = profile.join(format!("backups/tanki-pre-upgrade-{stamp}.anki2"));
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)?;
        output.write_all(&fs::read(&file)?)?;
        output.sync_all()?;
        eprintln!("Saved pre-upgrade collection to {}", dest.display());
    }
    Ok(())
}
fn sql_error(e: rusqlite::Error) -> CoreError {
    if matches!(&e, rusqlite::Error::SqliteFailure(err, _) if matches!(err.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
    {
        CoreError::Locked
    } else {
        CoreError::Other(e.to_string())
    }
}
