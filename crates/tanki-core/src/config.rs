//! tanki configuration and credentials (PLAN 4.5).
//!
//! The configuration is TOML at `$XDG_CONFIG_HOME/tanki/config.toml`
//! (fallback `~/.config/tanki/config.toml`). Sync host keys live in a separate
//! `credentials.toml` beside it, created with mode `0600`; the OS keyring is
//! deferred. Every function takes an explicit path so tests use temp folders.
//! Nothing here reads Anki desktop's `prefs21.db`.
//!
//! Profiles are keyed by a string chosen by the caller; tanki uses the
//! canonical profile folder path (`CollectionActor::profile_dir`).
use crate::{CoreError, Result, SyncCredentials, SyncEndpoint};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::{Path, PathBuf},
};

pub const CONFIG_FILE: &str = "config.toml";
pub const CREDENTIALS_FILE: &str = "credentials.toml";

/// `$XDG_CONFIG_HOME/tanki`, else `~/.config/tanki`. `None` without either.
pub fn default_dir() -> Option<PathBuf> {
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    var("XDG_CONFIG_HOME")
        .or_else(|| var("HOME").map(|home| home.join(".config")))
        .map(|dir| dir.join("tanki"))
}

/// `default_dir()/config.toml`.
pub fn default_path() -> Option<PathBuf> {
    default_dir().map(|dir| dir.join(CONFIG_FILE))
}

/// The credentials file that belongs to a config file (same folder).
pub fn credentials_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(CREDENTIALS_FILE)
}

/// Sync settings of one profile. Sync is off until `url` is set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncSettings {
    /// Validated self-hosted server URL (never AnkiWeb).
    pub url: Option<String>,
    pub username: Option<String>,
    /// Sync on open and close. Off by default.
    pub auto_sync: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileConfig {
    pub sync: SyncSettings,
    /// Explicit renderer choices per note type. These are tanki preferences,
    /// never collection schema changes.
    pub render_tiers: BTreeMap<String, RenderTier>,
    /// Browser search history, newest first (desktop keeps 30 in its
    /// profile, R6 §1.5). tanki state, never stored in the collection.
    pub search_history: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderTier {
    #[default]
    Text,
    Bitmap,
}

/// Desktop keeps this many searches (`browser.py`).
pub const SEARCH_HISTORY_LIMIT: usize = 30;

impl ProfileConfig {
    /// Moves `search` to the front, dropping duplicates and the oldest
    /// entries beyond the limit. Blank searches are not recorded.
    pub fn push_search(&mut self, search: &str) {
        let search = search.trim();
        if search.is_empty() {
            return;
        }
        self.search_history.retain(|s| s != search);
        self.search_history.insert(0, search.to_owned());
        self.search_history.truncate(SEARCH_HISTORY_LIMIT);
    }
}

/// Editor settings (tanki state, PLAN 4.5).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    /// Command for `$EDITOR` round trips; falls back to `$VISUAL`, then
    /// `$EDITOR`, then `vi`. Split on whitespace; the file path is appended.
    pub command: Option<String>,
}

impl EditorSettings {
    /// The editor command line to run, as program and arguments.
    pub fn resolve(&self) -> Vec<String> {
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        self.command
            .clone()
            .filter(|c| !c.trim().is_empty())
            .or_else(|| env("VISUAL"))
            .or_else(|| env("EDITOR"))
            .unwrap_or_else(|| "vi".to_owned())
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    }
}

/// The AnkiConnect-compatible API (PLAN 8.14, Phase 6), `[connect]`.
/// Defaults follow AnkiConnect: loopback port 8765, no key, origin
/// `http://localhost`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectSettings {
    /// Address to bind, `host:port` [default: 127.0.0.1:8765].
    pub bind: Option<String>,
    /// When set, every request must carry this `key` (AnkiConnect `apiKey`).
    pub api_key: Option<String>,
    /// Extra allowed CORS origins (AnkiConnect `webCorsOriginList`); `*`
    /// allows any origin.
    pub cors_origins: Vec<String>,
    /// Also serve the API while the TUI runs.
    pub in_tui: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub last_profile: Option<String>,
    pub profiles: BTreeMap<String, ProfileConfig>,
    /// Collections tanki has opened before (the first-open rule, PLAN 4.4).
    pub seen_collections: BTreeSet<String>,
    pub editor: EditorSettings,
    pub connect: ConnectSettings,
    pub code_tour: CodeTourSettings,
}

impl Config {
    /// Loads `path`; a missing file is an empty configuration. Unknown keys
    /// are ignored so older builds can read newer files.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| {
                CoreError::Invalid(format!("Invalid tanki config {}: {e}", path.display()))
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    /// Writes atomically: temporary file in the same folder, fsync, rename.
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string(self).map_err(|e| CoreError::Other(e.to_string()))?;
        write_atomic(path, text.as_bytes(), 0o644)
    }

    pub fn profile(&self, key: &str) -> Option<&ProfileConfig> {
        self.profiles.get(key)
    }

    pub fn profile_mut(&mut self, key: &str) -> &mut ProfileConfig {
        self.profiles.entry(key.to_owned()).or_default()
    }

    /// The configured server for a profile, re-validated on every read.
    pub fn sync_endpoint(&self, key: &str) -> Result<Option<SyncEndpoint>> {
        self.profile(key)
            .and_then(|p| p.sync.url.as_deref())
            .map(SyncEndpoint::parse)
            .transpose()
    }

    pub fn has_seen(&self, collection: &Path) -> bool {
        self.seen_collections.contains(&path_key(collection))
    }

    /// Returns true when the collection was not seen before.
    pub fn mark_seen(&mut self, collection: &Path) -> bool {
        self.seen_collections.insert(path_key(collection))
    }
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Host keys per profile. Debug output never shows a key.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Credentials {
    profiles: BTreeMap<String, SyncCredentials>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // SyncCredentials' own Debug redacts the host key.
        f.debug_struct("Credentials")
            .field("profiles", &self.profiles)
            .finish()
    }
}

impl Credentials {
    /// Loads `path`; a missing file means no credentials. Refuses a file
    /// that other users can read.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(error) => return Err(error.into()),
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path)?.permissions().mode();
            if mode & 0o077 != 0 {
                return Err(CoreError::UnsafePath(format!(
                    "{} is readable by other users (mode {:o}); run chmod 600 on it.",
                    path.display(),
                    mode & 0o777
                )));
            }
        }
        // The parse error could quote file content; keep it out of messages.
        toml::from_str(&text)
            .map_err(|_| CoreError::Invalid(format!("Invalid credentials file {}", path.display())))
    }

    /// Writes atomically with mode 0600 from creation.
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string(self).map_err(|e| CoreError::Other(e.to_string()))?;
        write_atomic(path, text.as_bytes(), 0o600)
    }

    /// The stored credentials, with the endpoint re-validated (a hand-edited
    /// file cannot point tanki at AnkiWeb).
    pub fn get(&self, profile: &str) -> Result<Option<SyncCredentials>> {
        let Some(stored) = self.profiles.get(profile) else {
            return Ok(None);
        };
        let endpoint = SyncEndpoint::parse(&stored.endpoint)?;
        Ok(Some(SyncCredentials {
            endpoint: endpoint.as_str().to_owned(),
            ..stored.clone()
        }))
    }

    pub fn set(&mut self, profile: &str, credentials: SyncCredentials) -> Result<()> {
        SyncEndpoint::parse(&credentials.endpoint)?;
        self.profiles.insert(profile.to_owned(), credentials);
        Ok(())
    }

    /// Returns true when credentials were removed.
    pub fn remove(&mut self, profile: &str) -> bool {
        self.profiles.remove(profile).is_some()
    }
}

/// Temporary file (created with `mode`) + fsync + rename + folder fsync. The
/// folder is created with mode 0700 when missing.
fn write_atomic(path: &Path, data: &[u8], mode: u32) -> Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !dir.exists() {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(dir)?;
    }
    let name = path
        .file_name()
        .ok_or_else(|| CoreError::Invalid(format!("Not a file path: {}", path.display())))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let temp = dir.join(format!(
        ".{}.{}.{stamp}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, mode);
    #[cfg(not(unix))]
    let _ = mode;
    let written = (|| -> Result<()> {
        let mut file = options.open(&temp)?;
        file.write_all(data)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
        return written;
    }
    #[cfg(unix)]
    if let Ok(folder) = std::fs::File::open(dir) {
        let _ = folder.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_history_is_newest_first_unique_and_capped() {
        let mut profile = ProfileConfig::default();
        for i in 0..40 {
            profile.push_search(&format!("deck:{i}"));
        }
        profile.push_search("deck:35");
        profile.push_search("   ");
        assert_eq!(profile.search_history.len(), SEARCH_HISTORY_LIMIT);
        assert_eq!(profile.search_history[0], "deck:35");
        assert_eq!(profile.search_history[1], "deck:39");
        assert_eq!(
            profile
                .search_history
                .iter()
                .filter(|s| *s == "deck:35")
                .count(),
            1
        );
        let mut config = Config::default();
        config.profile_mut("p").push_search("tag:x");
        let text = toml::to_string(&config).unwrap();
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), config);
    }

    #[test]
    fn credentials_path_is_beside_config() {
        assert_eq!(
            credentials_path(Path::new("/x/tanki/config.toml")),
            Path::new("/x/tanki/credentials.toml")
        );
    }

    #[test]
    fn missing_files_load_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            Config::load(&dir.path().join("none.toml")).unwrap(),
            Config::default()
        );
        assert!(Credentials::load(&dir.path().join("none.toml"))
            .unwrap()
            .get("p")
            .unwrap()
            .is_none());
    }

    #[test]
    fn unknown_keys_and_missing_sections_use_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);
        std::fs::write(
            &path,
            "future = 1\n[profiles.a.sync]\nurl = \"http://127.0.0.1:1/\"\n",
        )
        .unwrap();
        let config = Config::load(&path).unwrap();
        let sync = &config.profile("a").unwrap().sync;
        assert_eq!(sync.url.as_deref(), Some("http://127.0.0.1:1/"));
        assert!(!sync.auto_sync);
        assert!(config.seen_collections.is_empty());
    }

    #[test]
    fn ankiweb_in_config_is_refused_on_read() {
        let mut config = Config::default();
        config.profile_mut("a").sync.url = Some("https://sync.ankiweb.net/".into());
        assert!(config.sync_endpoint("a").is_err());
        assert!(config.sync_endpoint("missing").unwrap().is_none());
    }
}

/// Machine-local Neovim bridge settings. Card fields never select executables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CodeTourSettings {
    pub enabled: bool,
    /// Launch a reader tab in the caller's Herdr session when no editor exists.
    pub launch_in_herdr: bool,
    /// Start Claude only on an explicit ask/explain/refresh action.
    pub lazy_claude: bool,
    pub command: Vec<String>,
    pub nvim_appname: String,
    pub xdg_config_home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
    pub xdg_state_home: Option<PathBuf>,
    pub repositories: BTreeMap<String, PathBuf>,
}
impl Default for CodeTourSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            launch_in_herdr: true,
            lazy_claude: true,
            command: vec!["code-tour".into()],
            nvim_appname: "nvim".into(),
            xdg_config_home: None,
            xdg_data_home: None,
            xdg_state_home: None,
            repositories: BTreeMap::new(),
        }
    }
}
