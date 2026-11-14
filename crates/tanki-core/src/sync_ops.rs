//! Phase 2 sync (PLAN 8.9): login, status, normal/full/media sync with a
//! self-hosted server only, and Anki's own sync server.
//!
//! Every call builds its client with `sync::safe_client` (HTTP/1 only, no
//! proxy, no HTTP redirects, AnkiWeb names refused before lookup) and every
//! auth carries the explicit, validated endpoint: `sync_login` returns an
//! auth without one, which Anki would treat as AnkiWeb. Anki follows the
//! protocol's 308 redirect itself and reports `new_endpoint`; that is
//! validated before it is returned. Progress and cancel go through the
//! collection's shared progress state (`want_abort`).
use crate::{
    sync::{is_ankiweb_host, safe_client, SafeClient},
    CoreError, DataResponse, Result, SyncCommand, SyncCredentials, SyncDirection, SyncEndpoint,
    SyncReport, SyncRequired, SyncServerOptions,
};
use anki::{
    collection::Collection,
    error::{AnkiError, SyncErrorKind},
    sync::collection::normal::SyncActionRequired,
};
use std::path::{Component, Path, PathBuf};

/// Default per-request I/O timeout (Anki's Rust default).
pub const DEFAULT_TIMEOUT_SECS: u32 = 30;

/// Text of the error returned when the server rejects the credentials. The
/// CLI maps it to "auth required" (`is_auth_error`).
pub const AUTH_REQUIRED: &str =
    "The sync server rejected the saved login. Log in again with `tanki sync login`.";

const REDIRECTED_TO_ANKIWEB: &str =
    "The sync server redirected tanki to AnkiWeb, which was refused: \
     AnkiWeb's terms exclude third-party clients. Check the server URL.";

/// True when `error` means the stored login is missing or rejected.
pub fn is_auth_error(error: &CoreError) -> bool {
    matches!(error, CoreError::Invalid(text) if text == AUTH_REQUIRED)
}

/// Login, status, normal sync and media sync. `Run` with a non-normal
/// direction never reaches this function: the actor intercepts it.
pub fn execute(col: &mut Collection, command: SyncCommand) -> Result<DataResponse> {
    match command {
        SyncCommand::Login {
            endpoint,
            username,
            password,
        } => login(col, &endpoint, username, password).map(DataResponse::LoggedIn),
        SyncCommand::Status { auth } => status(col, &auth).map(DataResponse::Synced),
        SyncCommand::Run {
            auth,
            direction: SyncDirection::Normal,
            media,
        } => normal(col, &auth, media).map(DataResponse::Synced),
        SyncCommand::Run { .. } => Err(CoreError::Invalid(
            "Full upload and download are handled by the collection actor".into(),
        )),
        SyncCommand::Media { auth } => {
            media_sync(col, &auth)?;
            Ok(DataResponse::Done)
        }
    }
}

/// One-way full upload. Consumes the collection, as Anki's call does.
pub fn upload(col: Collection, auth: SyncCredentials) -> Result<()> {
    let (_, sync_auth) = checked_auth(&auth)?;
    let client = safe_client()?;
    let tr = col.tr().clone();
    let outcome = runtime()?.block_on(col.full_upload(sync_auth, client.client.clone()));
    outcome.map_err(|e| sync_error(e, &client, &tr))
}

/// One-way full download. Called ONLY with a staging collection by the actor.
pub fn download(col: Collection, auth: SyncCredentials) -> Result<()> {
    let (_, sync_auth) = checked_auth(&auth)?;
    let client = safe_client()?;
    let tr = col.tr().clone();
    let outcome = runtime()?.block_on(col.full_download(sync_auth, client.client.clone()));
    outcome.map_err(|e| sync_error(e, &client, &tr))
}

/// Whether this collection has completed a sync with some server (full
/// upload, full download or normal sync), using public calls only: the
/// collection USN starts at 0 and every completed sync raises it, and a
/// non-zero last-sync time makes the offline status report pending work.
pub fn has_synced_before(col: &mut Collection) -> Result<bool> {
    use anki_proto::sync::sync_status_response::Required;
    Ok(col.sync_meta()?.usn.0 > 0 || col.sync_status_offline()? != Required::NoChanges)
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| CoreError::Other(format!("Starting the sync runtime: {e}")))
}

fn checked_auth(auth: &SyncCredentials) -> Result<(SyncEndpoint, anki::sync::login::SyncAuth)> {
    let endpoint = SyncEndpoint::parse(&auth.endpoint)?;
    if auth.hkey.is_empty() {
        return Err(CoreError::Invalid(AUTH_REQUIRED.into()));
    }
    let sync_auth = endpoint.auth_with_timeout(auth.hkey.clone(), auth.timeout_secs);
    Ok((endpoint, sync_auth))
}

/// Maps Anki's sync errors to messages without secrets. The host key never
/// appears in Anki's messages (it travels in a header, and reqwest errors
/// have their URL stripped upstream).
fn sync_error(error: AnkiError, client: &SafeClient, tr: &anki_i18n::I18n) -> CoreError {
    if client.refused_ankiweb() {
        return CoreError::Invalid(REDIRECTED_TO_ANKIWEB.into());
    }
    match &error {
        AnkiError::Interrupted => CoreError::Interrupted,
        AnkiError::SyncError { source } if source.kind == SyncErrorKind::AuthFailed => {
            CoreError::Invalid(AUTH_REQUIRED.into())
        }
        AnkiError::SyncError { .. } | AnkiError::NetworkError { .. } => {
            CoreError::Other(anki_i18n::without_unicode_isolation(&error.message(tr)))
        }
        _ => error.into(),
    }
}

/// A redirect target must pass the same checks as a configured URL.
fn checked_new_endpoint(new_endpoint: Option<String>) -> Result<Option<String>> {
    let Some(raw) = new_endpoint else {
        return Ok(None);
    };
    if reqwest::Url::parse(&raw)
        .ok()
        .and_then(|url| url.host_str().map(is_ankiweb_host))
        .unwrap_or(false)
    {
        return Err(CoreError::Invalid(REDIRECTED_TO_ANKIWEB.into()));
    }
    let endpoint = SyncEndpoint::parse(&raw).map_err(|e| {
        CoreError::Invalid(format!(
            "The sync server redirected to an unusable URL; it was not saved: {e}"
        ))
    })?;
    Ok(Some(endpoint.as_str().to_owned()))
}

fn required(value: SyncActionRequired) -> SyncRequired {
    match value {
        SyncActionRequired::NoChanges => SyncRequired::NoChanges,
        SyncActionRequired::NormalSyncRequired => SyncRequired::Normal,
        SyncActionRequired::FullSyncRequired {
            upload_ok,
            download_ok,
        } => SyncRequired::Full {
            upload: upload_ok,
            download: download_ok,
        },
    }
}

fn login(
    col: &Collection,
    endpoint: &str,
    username: String,
    password: String,
) -> Result<SyncCredentials> {
    let endpoint = SyncEndpoint::parse(endpoint)?;
    let client = safe_client()?;
    let outcome = runtime()?.block_on(anki::sync::login::sync_login(
        username,
        password,
        Some(endpoint.as_str().to_owned()),
        client.client.clone(),
    ));
    let auth = outcome.map_err(|e| match sync_error(e, &client, col.tr()) {
        error if is_auth_error(&error) => {
            CoreError::Invalid("The sync server rejected this username or password.".into())
        }
        error => error,
    })?;
    // The returned auth has no endpoint (= AnkiWeb); keep the configured one.
    Ok(SyncCredentials {
        endpoint: endpoint.as_str().to_owned(),
        hkey: auth.hkey,
        timeout_secs: DEFAULT_TIMEOUT_SECS,
    })
}

/// What desktop's sync button shows: local state first, the server only
/// when the collection itself is clean. Changes nothing.
fn status(col: &mut Collection, auth: &SyncCredentials) -> Result<SyncReport> {
    use anki_proto::sync::sync_status_response::Required;
    let (_, sync_auth) = checked_auth(auth)?;
    match col.sync_status_offline()? {
        Required::FullSync => {
            return Ok(SyncReport {
                required: SyncRequired::Full {
                    upload: true,
                    download: true,
                },
                server_message: String::new(),
                new_endpoint: None,
            })
        }
        Required::NormalSync => {
            return Ok(SyncReport {
                required: SyncRequired::Normal,
                server_message: String::new(),
                new_endpoint: None,
            })
        }
        Required::NoChanges => {}
    }
    let client = safe_client()?;
    let local = col.sync_meta()?;
    let mut server = anki::sync::http_client::HttpSyncClient::new(sync_auth, client.client.clone());
    let outcome = runtime()?.block_on(anki::sync::collection::status::online_sync_status_check(
        local,
        &mut server,
    ));
    let state = outcome.map_err(|e| sync_error(e, &client, col.tr()))?;
    Ok(SyncReport {
        required: required(state.required),
        server_message: state.server_message,
        new_endpoint: checked_new_endpoint(state.new_endpoint)?,
    })
}

fn normal(col: &mut Collection, auth: &SyncCredentials, media: bool) -> Result<SyncReport> {
    let (_, sync_auth) = checked_auth(auth)?;
    let client = safe_client()?;
    let rt = runtime()?;
    let outcome = rt.block_on(col.normal_sync(sync_auth, client.client.clone()));
    let output = outcome.map_err(|e| sync_error(e, &client, col.tr()))?;
    let report = SyncReport {
        required: required(output.required),
        server_message: output.server_message,
        new_endpoint: checked_new_endpoint(output.new_endpoint)?,
    };
    // A full sync must be chosen first; media follows it (desktop does the same).
    if media && !matches!(report.required, SyncRequired::Full { .. }) {
        let auth = SyncCredentials {
            endpoint: report
                .new_endpoint
                .clone()
                .unwrap_or_else(|| auth.endpoint.clone()),
            ..auth.clone()
        };
        media_sync(col, &auth)?;
    }
    Ok(report)
}

/// Runs media sync to completion on the calling (actor) thread, so every
/// error is returned here; callers poll `CollectionActor::progress` meanwhile.
fn media_sync(col: &mut Collection, auth: &SyncCredentials) -> Result<()> {
    let (_, sync_auth) = checked_auth(auth)?;
    let client = safe_client()?;
    let tr = col.tr().clone();
    let manager = col.media()?;
    let progress = col.new_progress_handler();
    let outcome =
        runtime()?.block_on(manager.sync_media(progress, sync_auth, client.client.clone(), None));
    outcome.map_err(|e| sync_error(e, &client, &tr))
}

// ---------------------------------------------------------------------------
// Server

/// Every place a real Anki base can live (R5 §1.1), as in `safety.rs`.
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

/// Entries that mark an Anki profile or base folder.
const PROFILE_MARKERS: [&str; 5] = [
    "collection.anki2",
    "collection.anki2-wal",
    "collection.media",
    "collection.media.db2",
    "prefs21.db",
];

/// Checks a server data folder by path only; nothing inside a real Anki
/// base is opened or listed. Refuses a profile folder, an Anki base, any
/// folder inside one, and any folder under the real bases. Returns the
/// absolute path.
pub fn check_server_data_folder(path: &Path) -> Result<PathBuf> {
    let refuse = |why: &str| {
        Err(CoreError::UnsafePath(format!(
            "Refusing sync server data folder {}: {why}. Use a new, empty folder of its own.",
            path.display()
        )))
    };
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    if absolute
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return refuse("it must not contain '..'");
    }
    let bases = real_bases();
    // Lexical check first, before any filesystem access.
    if bases.iter().any(|base| absolute.starts_with(base)) {
        return refuse("it is inside a real Anki data folder");
    }
    // Resolve symlinks in the deepest existing ancestor, and in the bases'
    // parents only (never inside a base).
    let mut existing = absolute.as_path();
    let mut rest = Vec::new();
    while !existing.exists() {
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                existing = parent;
            }
            _ => break,
        }
    }
    let mut resolved = existing.canonicalize()?;
    for name in rest.iter().rev() {
        resolved.push(name);
    }
    let resolved_bases = bases
        .iter()
        .filter_map(|base| Some(base.parent()?.canonicalize().ok()?.join(base.file_name()?)));
    if resolved_bases
        .chain(bases.iter().cloned())
        .any(|base| resolved.starts_with(base))
    {
        return refuse("it resolves to a real Anki data folder");
    }
    for folder in resolved.ancestors() {
        if let Some(marker) = PROFILE_MARKERS
            .iter()
            .find(|marker| folder.join(marker).exists())
        {
            return if folder == resolved {
                refuse(&format!("it holds {marker} (an Anki profile or base)"))
            } else {
                refuse(&format!(
                    "it is inside {}, which holds {marker} (an Anki profile or base)",
                    folder.display()
                ))
            };
        }
    }
    Ok(resolved)
}

/// A user name becomes a folder name under the data folder.
fn check_user(entry: &str) -> Result<()> {
    let name = entry.split_once(':').map(|(name, _)| name).ok_or_else(|| {
        CoreError::Invalid("Sync server users must be written as username:password.".into())
    })?;
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', '\0'])
        || name.trim() != name
    {
        return Err(CoreError::Invalid(format!(
            "Unusable sync server user name {name:?}: it becomes a folder name."
        )));
    }
    Ok(())
}

fn user_vars() -> Vec<String> {
    std::env::vars_os()
        .filter_map(|(key, _)| key.into_string().ok())
        .filter(|key| key.starts_with("SYNC_USER"))
        .collect()
}

/// Runs Anki's sync server until it fails or receives Ctrl-C. Blocking.
pub fn run_server(options: SyncServerOptions) -> Result<()> {
    serve(options, |address| {
        eprintln!(
            "tanki sync-server listening on http://{address}/ (plain HTTP; put it behind TLS or a private network)"
        )
    })
}

/// `run_server`, calling `ready` with the bound address (useful with port 0).
///
/// Anki's server reads its users only from `SYNC_USER1`, `SYNC_USER2`, ...
/// (and `PASSWORDS_HASHED`) when it starts; there is no config field for
/// them. When `options.users` is empty those variables are used as they are
/// (passthrough). Otherwise this function replaces them in the process
/// environment before the server starts. That is safe because it is called
/// once per process, before any runtime or worker thread exists (the CLI
/// calls it first thing; tests call it in a dedicated child process), so no
/// other thread can read the environment concurrently.
pub fn serve(options: SyncServerOptions, ready: impl FnOnce(std::net::SocketAddr)) -> Result<()> {
    let host: std::net::IpAddr = options.host.parse().map_err(|_| {
        CoreError::Invalid(format!(
            "Bind address must be an IP address, not {:?}",
            options.host
        ))
    })?;
    let data = check_server_data_folder(&options.data)?;
    if options.users.is_empty() {
        let names = user_vars();
        if names.is_empty() {
            return Err(CoreError::Invalid(
                "No sync server users: pass a users file or set SYNC_USER1=username:password."
                    .into(),
            ));
        }
        for name in names {
            if let Ok(value) = std::env::var(&name) {
                check_user(&value)?;
            }
        }
    } else {
        for user in &options.users {
            check_user(user)?;
        }
        for name in user_vars() {
            std::env::remove_var(name);
        }
        std::env::remove_var("PASSWORDS_HASHED");
        for (index, user) in options.users.iter().enumerate() {
            std::env::set_var(format!("SYNC_USER{}", index + 1), user);
        }
    }
    std::fs::create_dir_all(&data)?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| CoreError::Other(format!("Starting the server runtime: {e}")))?;
    rt.block_on(async {
        let (address, future) = anki::sync::http_server::SimpleServer::make_server(
            anki::sync::http_server::SyncServerConfig {
                host,
                port: options.port,
                base_folder: data,
                ip_header: anki::sync::http_server::default_ip_header(),
            },
        )
        .await
        .map_err(|e| CoreError::Other(format!("Sync server: {e}")))?;
        ready(address);
        future
            .await
            .map_err(|e| CoreError::Other(format!("Sync server: {e}")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_targets_are_validated() {
        assert_eq!(checked_new_endpoint(None).unwrap(), None);
        // Normalized as Anki normalizes endpoints (`join("./")`).
        assert_eq!(
            checked_new_endpoint(Some("http://127.0.0.1:9/a/".into())).unwrap(),
            Some("http://127.0.0.1:9/a/".into())
        );
        assert_eq!(
            checked_new_endpoint(Some("http://127.0.0.1:9/a".into())).unwrap(),
            Some("http://127.0.0.1:9/".into())
        );
        for bad in [
            "https://sync.ankiweb.net/",
            "https://ANKIWEB.net./",
            "http://u:p@127.0.0.1/",
            "nonsense",
        ] {
            assert!(checked_new_endpoint(Some(bad.into())).is_err(), "{bad}");
        }
        assert_eq!(
            checked_new_endpoint(Some("https://sync13.ankiweb.net/".into())).unwrap_err(),
            CoreError::Invalid(REDIRECTED_TO_ANKIWEB.into())
        );
    }

    #[test]
    fn user_names_cannot_escape_the_data_folder() {
        assert!(check_user("alice:secret:with:colons").is_ok());
        for bad in ["nopassword", ":pw", "../x:pw", "a/b:pw", "..:pw", " a:pw"] {
            assert!(check_user(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn credentials_without_key_need_login() {
        let auth = SyncCredentials {
            endpoint: "http://127.0.0.1:1/".into(),
            hkey: String::new(),
            timeout_secs: 0,
        };
        assert!(is_auth_error(&checked_auth(&auth).err().unwrap()));
        let ankiweb = SyncCredentials {
            endpoint: "https://sync.ankiweb.net/".into(),
            hkey: "k".into(),
            timeout_secs: 0,
        };
        assert!(checked_auth(&ankiweb).is_err());
    }
}
