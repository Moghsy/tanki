//! Sync and package operations in the app: one data operation at a time runs
//! on the collection actor while the loop keeps drawing and polling, and the
//! `SyncUi` / `PackageUi` widgets own the screens (PLAN 8.9, 8.11, 8.12).
use super::{App, ChoiceValue, Overlay, PromptKind, Screen};
use crate::backend::{Backend, DataOp};
use crate::input::TextInput;
use crate::package_ui::{Intent as PackageIntent, PackageUi};
use crate::sync_ui::{SyncIntent, SyncRequest};
use std::path::{Path, PathBuf};
use tanki_core::{
    config::{credentials_path, Config, Credentials},
    BackupInfo, CoreError, DataCommand, DataResponse, ExportOptions, ExportSelection, SyncCommand,
    SyncCredentials, SyncDirection, SyncEndpoint,
};

pub const NOT_CONFIGURED: &str = "sync is not configured: run `tanki sync login --url URL`";
const NOT_LOGGED_IN: &str = "not logged in to the sync server: run `tanki sync login --url URL`";
const BUSY: &str = "Another operation is still running.";

/// What the running operation is for, so its result goes to the right place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobKind {
    Sync,
    Package,
    Backup,
    ListBackups,
}

pub(crate) struct Job {
    pub kind: JobKind,
    op: Box<dyn DataOp>,
}

impl App<'_> {
    /// Where tanki's `config.toml` is (`None`: sync is unavailable).
    pub fn set_config(&mut self, path: Option<PathBuf>) {
        self.config_path = path;
    }

    /// True while a data operation runs or one of its screens is open.
    pub fn data_busy(&self) -> bool {
        self.job.is_some() || self.sync_ui.is_visible() || self.package_ui.is_some()
    }

    pub(super) fn profile_key(&self) -> String {
        self.profile.dir.to_string_lossy().into_owned()
    }

    fn can_start_data(&mut self) -> bool {
        if self.data_busy() {
            self.error(BUSY);
            return false;
        }
        true
    }

    fn begin(&mut self, kind: JobKind, command: DataCommand) -> Result<(), CoreError> {
        if self.job.is_some() {
            return Err(CoreError::Other(BUSY.into()));
        }
        let op = self.backend.start_data(command)?;
        self.job = Some(Job { kind, op });
        Ok(())
    }

    /// Polls the running operation; called on every tick.
    pub(crate) fn poll_job(&mut self) {
        let Some(job) = &self.job else { return };
        let kind = job.kind;
        match job.op.poll().transpose() {
            None => {
                let progress = self.backend.data_progress();
                match kind {
                    JobKind::Sync => self.sync_ui.on_progress(progress),
                    JobKind::Package => {
                        if let Some(ui) = &mut self.package_ui {
                            ui.on_progress(progress);
                        }
                    }
                    _ => {}
                }
            }
            Some(result) => {
                self.job = None;
                self.finish_job(kind, result);
            }
        }
    }

    fn finish_job(&mut self, kind: JobKind, result: Result<DataResponse, CoreError>) {
        match kind {
            JobKind::Sync => {
                let intent = self.sync_ui.on_response(result);
                self.run_sync_intent(intent);
            }
            JobKind::Package => {
                if let Some(ui) = &mut self.package_ui {
                    let intent = ui.on_response(result);
                    self.run_package_intent(intent);
                }
            }
            JobKind::Backup => match result {
                Ok(DataResponse::Backups(list)) if !list.is_empty() => {
                    self.notice(format!("Backup written: {}", file_name(&list[0].path)))
                }
                Ok(_) => self.notice("Backup created."),
                Err(e) => self.error(format!("Backup failed: {e}")),
            },
            JobKind::ListBackups => match result {
                Ok(DataResponse::Backups(list)) => self.offer_backups(list),
                Ok(_) => self.error("Unexpected response while listing backups."),
                Err(e) => self.error(format!("Could not list backups: {e}")),
            },
        }
    }

    /// Cancels a running operation when the app quits.
    pub(crate) fn cancel_job(&mut self) {
        if self.job.is_some() {
            self.backend.cancel_data();
        }
    }

    // ---- sync ----------------------------------------------------------------

    /// The stored login for the configured server, as `tanki sync` requires.
    fn sync_auth(&self) -> Result<SyncCredentials, String> {
        let Some(path) = &self.config_path else {
            return Err(NOT_CONFIGURED.into());
        };
        let key = self.profile_key();
        let config = Config::load(path).map_err(|e| e.to_string())?;
        let Some(endpoint) = config.sync_endpoint(&key).map_err(|e| e.to_string())? else {
            return Err(NOT_CONFIGURED.into());
        };
        let credentials = Credentials::load(&credentials_path(path)).map_err(|e| e.to_string())?;
        match credentials.get(&key).map_err(|e| e.to_string())? {
            Some(auth) if auth.endpoint == endpoint.as_str() => Ok(auth),
            Some(_) => Err(
                "the stored login belongs to a different server: run `tanki sync login --url URL`"
                    .into(),
            ),
            None => Err(NOT_LOGGED_IN.into()),
        }
    }

    pub(crate) fn sync_action(&mut self) {
        if !self.can_start_data() {
            return;
        }
        if let Err(message) = self.sync_auth() {
            self.error(message);
            return;
        }
        let intent = self.sync_ui.start_normal();
        self.run_sync_intent(intent);
    }

    pub(crate) fn run_sync_intent(&mut self, intent: SyncIntent) {
        match intent {
            SyncIntent::None | SyncIntent::Close => {}
            SyncIntent::Start(request) => self.start_sync(request),
            SyncIntent::Cancel => self.backend.cancel_data(),
            SyncIntent::SaveEndpoint(url) => self.save_endpoint(&url),
            SyncIntent::ForgetLogin => self.forget_login(),
            SyncIntent::Finished { new_endpoint } => {
                if let Some(url) = new_endpoint {
                    self.save_endpoint(&url);
                }
                self.refresh_after_data();
            }
        }
    }

    fn start_sync(&mut self, request: SyncRequest) {
        let started = self
            .sync_auth()
            .map_err(CoreError::Invalid)
            .and_then(|auth| {
                let command = match request {
                    SyncRequest::Normal { media } => SyncCommand::Run {
                        auth,
                        direction: SyncDirection::Normal,
                        media,
                    },
                    SyncRequest::Upload => SyncCommand::Run {
                        auth,
                        direction: SyncDirection::Upload,
                        media: true,
                    },
                    SyncRequest::Download => SyncCommand::Run {
                        auth,
                        direction: SyncDirection::Download,
                        media: true,
                    },
                    SyncRequest::Media => SyncCommand::Media { auth },
                };
                self.begin(JobKind::Sync, DataCommand::Sync(command))
            });
        if let Err(error) = started {
            let _ = self.sync_ui.on_response(Err(error));
        }
    }

    /// Persists a server redirect (already validated by tanki-core), to the
    /// credentials and the config, as `tanki sync` does.
    fn save_endpoint(&mut self, url: &str) {
        let (Some(path), key) = (self.config_path.clone(), self.profile_key()) else {
            return;
        };
        let saved = (|| -> tanki_core::Result<()> {
            let endpoint = SyncEndpoint::parse(url)?;
            let cred_path = credentials_path(&path);
            let mut credentials = Credentials::load(&cred_path)?;
            if let Some(mut auth) = credentials.get(&key)? {
                auth.endpoint = endpoint.as_str().into();
                credentials.set(&key, auth)?;
                credentials.save(&cred_path)?;
            }
            let mut config = Config::load(&path)?;
            config.profile_mut(&key).sync.url = Some(endpoint.as_str().into());
            config.save(&path)
        })();
        match saved {
            Ok(()) => self.notice(format!("The sync server moved to {url}; saved.")),
            Err(e) => self.error(format!("Could not save the new sync server: {e}")),
        }
    }

    /// A rejected login is forgotten, as desktop does.
    fn forget_login(&mut self) {
        let Some(path) = self.config_path.clone() else {
            return;
        };
        let cred_path = credentials_path(&path);
        if let Ok(mut credentials) = Credentials::load(&cred_path) {
            if credentials.remove(&self.profile_key()) {
                let _ = credentials.save(&cred_path);
            }
        }
    }

    fn refresh_after_data(&mut self) {
        self.refresh_screen();
        if let Ok(tree) = crate::backend::deck_tree(&self.backend) {
            self.totals = Some(crate::backend::total_counts(&tree));
        }
    }

    // ---- packages ------------------------------------------------------------

    pub(crate) fn import_action(&mut self) {
        if !self.can_start_data() {
            return;
        }
        self.overlay = Some(Overlay::Prompt {
            title: "Path of the .apkg deck package to import (a .colpkg asks to restore instead)"
                .into(),
            input: TextInput::default(),
            kind: PromptKind::ImportPath,
        });
    }

    pub(crate) fn export_action(&mut self) {
        if !self.can_start_data() {
            return;
        }
        let what = match (&self.screen, self.current_deck) {
            (Screen::Overview(_), Some(_)) => "the current deck",
            _ => "the whole collection",
        };
        self.overlay = Some(Overlay::Prompt {
            title: format!(
                "Output file: .apkg exports {what}; .colpkg always exports the whole collection"
            ),
            input: TextInput::default(),
            kind: PromptKind::ExportPath,
        });
    }

    pub(crate) fn backup_now_action(&mut self) {
        if !self.can_start_data() {
            return;
        }
        match self.begin(JobKind::Backup, DataCommand::Backup { force: true }) {
            Ok(()) => self.notice("Creating a backup…"),
            Err(e) => self.error(format!("Backup failed: {e}")),
        }
    }

    pub(crate) fn restore_backup_action(&mut self) {
        if !self.can_start_data() {
            return;
        }
        if let Err(e) = self.begin(JobKind::ListBackups, DataCommand::ListBackups) {
            self.error(format!("Could not list backups: {e}"));
        }
    }

    fn offer_backups(&mut self, list: Vec<BackupInfo>) {
        if list.is_empty() {
            self.notice("No backups.");
            return;
        }
        let mut options: Vec<(String, ChoiceValue)> = list
            .iter()
            .enumerate()
            .map(|(i, b)| {
                (
                    format!("{}  ({})", file_name(&b.path), human_bytes(b.bytes)),
                    ChoiceValue::Backup(i),
                )
            })
            .collect();
        options.push(("Cancel".into(), ChoiceValue::Cancel));
        self.backups = list;
        self.overlay = Some(Overlay::Choice {
            title: "Restore a backup".into(),
            message:
                "Newest first. Restoring replaces the collection; you will be asked to confirm."
                    .into(),
            options,
            selected: 0,
            cancel: ChoiceValue::Cancel,
        });
    }

    pub(crate) fn restore_chosen(&mut self, index: usize) {
        if let Some(info) = self.backups.get(index) {
            self.package_ui = Some(PackageUi::restore(info.path.clone()));
        }
    }

    /// The typed path of an import or export prompt.
    pub(crate) fn path_entered(&mut self, kind: PromptKind, text: &str) {
        if text.is_empty() {
            return;
        }
        let path = match expand_path(text) {
            Ok(path) => path,
            Err(e) => return self.error(format!("Bad path: {e}")),
        };
        match kind {
            PromptKind::ImportPath => {
                if !path.is_file() {
                    return self.error(format!("{} is not a file.", path.display()));
                }
                self.package_ui = Some(PackageUi::import(path));
            }
            PromptKind::ExportPath => self.start_export(path),
            PromptKind::SetDueDate => {}
        }
    }

    fn start_export(&mut self, path: PathBuf) {
        let colpkg = has_extension(&path, "colpkg");
        if !colpkg && !has_extension(&path, "apkg") {
            return self.error("The output file name must end in .apkg or .colpkg.");
        }
        if path.exists() {
            return self.error(format!(
                "{} exists; choose another file name.",
                path.display()
            ));
        }
        let (ui, intent) = if colpkg {
            PackageUi::export_collection(path, true, false)
        } else {
            let selection = match (&self.screen, self.current_deck) {
                (Screen::Overview(_), Some(id)) => ExportSelection::Deck(id),
                _ => ExportSelection::All,
            };
            let options = ExportOptions {
                with_media: true,
                ..Default::default()
            };
            PackageUi::export(path, options, selection)
        };
        self.package_ui = Some(ui);
        self.run_package_intent(intent);
    }

    pub(crate) fn run_package_intent(&mut self, intent: PackageIntent) {
        match intent {
            PackageIntent::None => {}
            PackageIntent::Start(command) => {
                if let Err(error) = self.begin(JobKind::Package, command) {
                    if let Some(ui) = &mut self.package_ui {
                        let _ = ui.on_response(Err(error));
                    }
                }
            }
            PackageIntent::Cancel => self.backend.cancel_data(),
            PackageIntent::Close { refresh } => {
                self.package_ui = None;
                if refresh {
                    self.refresh_after_data();
                }
            }
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

fn has_extension(path: &Path, wanted: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(wanted))
}

fn human_bytes(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MiB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1} KiB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

/// Strips surrounding quotes (drag and drop), expands `~/` and makes the
/// path absolute.
fn expand_path(text: &str) -> std::io::Result<PathBuf> {
    let text = text.trim().trim_matches(['"', '\'']);
    let path = match text.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(rest),
            None => PathBuf::from(text),
        },
        None => PathBuf::from(text),
    };
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}
