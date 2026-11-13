//! Shared Phase 2 contracts. Collection ownership stays with the actor.
use crate::OpChanges;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    /// User confirms desktop is closed after an unclean shutdown. Never bypasses /proc or locks.
    pub confirmed_closed: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SyncCredentials {
    pub endpoint: String,
    pub hkey: String,
    pub timeout_secs: u32,
}
impl std::fmt::Debug for SyncCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncCredentials")
            .field("endpoint", &self.endpoint)
            .field("hkey", &"[redacted]")
            .field("timeout_secs", &self.timeout_secs)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncDirection {
    #[default]
    Normal,
    Upload,
    Download,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncRequired {
    NoChanges,
    Normal,
    Full { upload: bool, download: bool },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncReport {
    pub required: SyncRequired,
    pub server_message: String,
    pub new_endpoint: Option<String>,
}

#[derive(Clone)]
pub enum SyncCommand {
    Login {
        endpoint: String,
        username: String,
        password: String,
    },
    Status {
        auth: SyncCredentials,
    },
    Run {
        auth: SyncCredentials,
        direction: SyncDirection,
        media: bool,
    },
    Media {
        auth: SyncCredentials,
    },
}
impl std::fmt::Debug for SyncCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Login {
                endpoint, username, ..
            } => f
                .debug_struct("Login")
                .field("endpoint", endpoint)
                .field("username", username)
                .finish_non_exhaustive(),
            Self::Status { auth } => f.debug_tuple("Status").field(auth).finish(),
            Self::Run {
                auth,
                direction,
                media,
            } => f
                .debug_struct("Run")
                .field("auth", auth)
                .field("direction", direction)
                .field("media", media)
                .finish(),
            Self::Media { auth } => f.debug_tuple("Media").field(auth).finish(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub enum UpdateCondition {
    #[default]
    IfNewer,
    Always,
    Never,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportOptions {
    pub merge_notetypes: bool,
    pub update_notes: UpdateCondition,
    pub update_notetypes: UpdateCondition,
    pub with_scheduling: bool,
    pub with_deck_configs: bool,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExportOptions {
    pub with_scheduling: bool,
    pub with_deck_configs: bool,
    pub with_media: bool,
    pub legacy: bool,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum ExportSelection {
    #[default]
    All,
    Deck(i64),
    Query(String),
}
#[derive(Debug, Clone)]
pub enum PackageCommand {
    Import {
        path: PathBuf,
        options: Option<ImportOptions>,
        schema_confirmed: bool,
    },
    Export {
        path: PathBuf,
        options: ExportOptions,
        selection: ExportSelection,
    },
    ExportCollection {
        path: PathBuf,
        media: bool,
        legacy: bool,
    },
    Restore {
        path: PathBuf,
        confirmed: bool,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportLog {
    pub new: usize,
    pub updated: usize,
    pub duplicate: usize,
    pub conflicting: usize,
    pub found_notes: u32,
    /// Category -> field snippets, using the library's sanitized result log.
    pub entries: std::collections::BTreeMap<String, Vec<Vec<String>>>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupInfo {
    pub path: PathBuf,
    pub bytes: u64,
}
#[derive(Debug, Clone)]
pub enum DataCommand {
    Sync(SyncCommand),
    Package(PackageCommand),
    Backup { force: bool },
    ListBackups,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DataResponse {
    LoggedIn(SyncCredentials),
    Synced(SyncReport),
    Imported { log: ImportLog, changes: OpChanges },
    Exported { notes: u32 },
    Backups(Vec<BackupInfo>),
    Done,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OperationProgress {
    pub stage: String,
    pub current: Option<u64>,
    pub total: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct SyncServerOptions {
    pub host: String,
    pub port: u16,
    pub data: PathBuf,
    /// username:password strings; never print these.
    pub users: Vec<String>,
}
