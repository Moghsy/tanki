//! Profiles are the base folder's subfolders holding a `collection.anki2`
//! (PLAN 4.4, 8.13). Only tanki's own metadata is touched: never
//! `prefs21.db`, never `addons21/` (PLAN 14 rule 3). Nothing here opens a
//! collection except [`create`], which makes a new empty one.
use crate::config::{self, Config, Credentials};
use crate::{CoreError, Result};
use std::path::{Path, PathBuf};

/// Where deleted profiles go: recoverable by moving the folder back.
pub const TRASH: &str = ".tanki-trash";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProfileInfo {
    pub name: String,
    /// Size of `collection.anki2` in bytes.
    pub collection_bytes: u64,
}

/// A profile name is one plain folder name. Desktop allows more, but names
/// that Anki itself would reject (path separators, leading dots) are refused
/// here so no profile can be hidden or escape the base.
pub fn valid_name(name: &str) -> bool {
    let trimmed = name.trim();
    !(trimmed.is_empty()
        || trimmed != name
        || name.starts_with('.')
        || name == "addons21"
        || name.eq_ignore_ascii_case("prefs21.db")
        || name.len() > 200
        || name.contains(['/', '\\', '\0'])
        || name.chars().any(char::is_control))
}

fn check_name(name: &str) -> Result<()> {
    if valid_name(name) {
        Ok(())
    } else {
        Err(CoreError::Invalid(format!(
            "{name:?} is not a usable profile name: use a plain folder name without slashes or a leading dot."
        )))
    }
}

fn is_profile(base: &Path, name: &str) -> bool {
    let path = base.join(name);
    path.is_dir()
        && !path
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        && path.join("collection.anki2").is_file()
}

/// The base's profiles, sorted by name.
pub fn list(base: &Path) -> Result<Vec<ProfileInfo>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(base)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "addons21" || name.starts_with('.') || !entry.file_type()?.is_dir() {
            continue;
        }
        let collection = entry.path().join("collection.anki2");
        if let Ok(meta) = std::fs::metadata(&collection) {
            if meta.is_file() {
                found.push(ProfileInfo {
                    name,
                    collection_bytes: meta.len(),
                });
            }
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

/// Another process (desktop Anki, another tanki) has the profile open.
fn check_not_in_use(profile: &Path) -> Result<()> {
    let holders = crate::safety::other_holders(&crate::safety::collection_files(profile));
    if let Some((pid, _)) = holders.first() {
        return Err(CoreError::InUse(format!(
            "The profile is open in another program (pid {pid}); close it first."
        )));
    }
    if profile.join("collection.anki2-wal").exists() {
        return Err(CoreError::ProfileBusy(
            "The profile has a leftover collection.anki2-wal (Anki is running, or crashed). Open it once in Anki or tanki first.".into(),
        ));
    }
    Ok(())
}

/// Creates `base/name` with a new empty collection and its first backup
/// (the open path makes both), then closes it.
pub fn create(base: &Path, name: &str) -> Result<PathBuf> {
    check_name(name)?;
    let path = base.join(name);
    if path.symlink_metadata().is_ok() {
        return Err(CoreError::Invalid(format!(
            "Profile {name:?} already exists."
        )));
    }
    // Names are unique ignoring case, as in desktop.
    if list(base)?
        .iter()
        .any(|p| p.name.eq_ignore_ascii_case(name))
    {
        return Err(CoreError::Invalid(format!(
            "A profile named like {name:?} already exists."
        )));
    }
    std::fs::create_dir(&path)?;
    let opened = crate::CollectionActor::open(&path).and_then(|actor| actor.close());
    if let Err(error) = opened {
        // Leave nothing half-made behind: only remove what this call made.
        let _ = std::fs::remove_dir_all(&path);
        return Err(error);
    }
    Ok(path)
}

/// Where `config` keys profile `path` (the canonical profile folder).
fn key(path: &Path) -> Result<String> {
    Ok(path.canonicalize()?.to_string_lossy().into_owned())
}

/// What a rename or delete did to tanki's config.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MetadataChange {
    pub config_updated: bool,
    pub credentials_updated: bool,
}

/// Renames the folder `base/old` to `base/new` and moves tanki's metadata
/// (`last_profile`, per-profile settings, seen-collection record and stored
/// sync login in `config_file`) to the new name. The profile must not be open
/// anywhere. Names are unique ignoring case.
pub fn rename(
    base: &Path,
    old: &str,
    new: &str,
    config_file: Option<&Path>,
) -> Result<MetadataChange> {
    check_name(old)?;
    check_name(new)?;
    if !is_profile(base, old) {
        return Err(CoreError::Invalid(format!("No profile named {old:?}.")));
    }
    if old != new && list(base)?.iter().any(|p| p.name.eq_ignore_ascii_case(new)) {
        return Err(CoreError::Invalid(format!(
            "A profile named like {new:?} already exists."
        )));
    }
    if old == new {
        return Ok(MetadataChange::default());
    }
    let from = base.join(old);
    let to = base.join(new);
    if to.symlink_metadata().is_ok() {
        return Err(CoreError::Invalid(format!(
            "{new:?} already exists in the base folder."
        )));
    }
    check_not_in_use(&from)?;
    let old_key = key(&from)?;
    std::fs::rename(&from, &to)?;
    let new_key = key(&to)?;
    let mut change = MetadataChange::default();
    if let Some(file) = config_file {
        let mut cfg = Config::load(file)?;
        let mut touched = false;
        if cfg.last_profile.as_deref() == Some(old) {
            cfg.last_profile = Some(new.to_owned());
            touched = true;
        }
        if let Some(settings) = cfg.profiles.remove(&old_key) {
            cfg.profiles.insert(new_key.clone(), settings);
            touched = true;
        }
        let old_collection = format!("{old_key}/collection.anki2");
        if cfg.seen_collections.remove(&old_collection) {
            cfg.seen_collections
                .insert(format!("{new_key}/collection.anki2"));
            touched = true;
        }
        if touched {
            cfg.save(file)?;
        }
        change.config_updated = touched;
        let creds_file = config::credentials_path(file);
        if creds_file.exists() {
            let mut creds = Credentials::load(&creds_file)?;
            if let Some(stored) = creds.get(&old_key)? {
                creds.set(&new_key, stored)?;
                creds.remove(&old_key);
                creds.save(&creds_file)?;
                change.credentials_updated = true;
            }
        }
    }
    Ok(change)
}

/// Removes profile `name`: the typed confirmation must equal the name, the
/// profile must not be `open` (the one this process has open) nor open in
/// another program. The folder moves to `base/.tanki-trash/<name>-<time>`
/// instead of being erased, and tanki's metadata for it is dropped. Returns
/// the trash location.
pub fn delete(
    base: &Path,
    name: &str,
    typed_confirmation: &str,
    open: Option<&str>,
    config_file: Option<&Path>,
) -> Result<PathBuf> {
    check_name(name)?;
    if !is_profile(base, name) {
        return Err(CoreError::Invalid(format!("No profile named {name:?}.")));
    }
    if open == Some(name) {
        return Err(CoreError::Invalid(format!(
            "Profile {name:?} is open; switch to another profile first."
        )));
    }
    if typed_confirmation != name {
        return Err(CoreError::Invalid(format!(
            "To delete the profile, type its name ({name}) exactly."
        )));
    }
    let profile = base.join(name);
    check_not_in_use(&profile)?;
    let profile_key = key(&profile)?;
    let trash = base.join(TRASH);
    std::fs::create_dir_all(&trash)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let mut target = trash.join(format!("{name}-{stamp}"));
    let mut n = 1;
    while target.symlink_metadata().is_ok() {
        n += 1;
        target = trash.join(format!("{name}-{stamp}-{n}"));
    }
    std::fs::rename(&profile, &target)?;
    if let Some(file) = config_file {
        let mut cfg = Config::load(file)?;
        let mut touched = false;
        if cfg.last_profile.as_deref() == Some(name) {
            cfg.last_profile = None;
            touched = true;
        }
        touched |= cfg.profiles.remove(&profile_key).is_some();
        touched |= cfg
            .seen_collections
            .remove(&format!("{profile_key}/collection.anki2"));
        if touched {
            cfg.save(file)?;
        }
        let creds_file = config::credentials_path(file);
        if creds_file.exists() {
            let mut creds = Credentials::load(&creds_file)?;
            if creds.remove(&profile_key) {
                creds.save(&creds_file)?;
            }
        }
    }
    Ok(target)
}
