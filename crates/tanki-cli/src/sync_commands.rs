//! `tanki sync` and `tanki sync-server` (Sync stream, PLAN 8.9).
//!
//! Self-hosted servers only: there is no default URL and AnkiWeb hosts are
//! refused. Progress goes to stderr; `--json` output goes to stdout. Host keys
//! and passwords never appear in any output.
// The lead wires `run`/`run_server` into `main.rs`; until then the module is
// only reached from its tests.
#![allow(dead_code)]

use clap::{Args, Subcommand, ValueEnum};
use serde_json::json;
use std::{
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tanki_core::{
    config::{credentials_path, Config, Credentials},
    sync_ops, CollectionActor, CoreError, DataCommand, DataResponse, SyncCommand, SyncCredentials,
    SyncDirection, SyncEndpoint, SyncReport, SyncRequired, SyncServerOptions,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// `tanki sync [--upload|--download] [--no-media]`, or one of the actions.
#[derive(Debug, Clone, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct SyncArgs {
    #[command(subcommand)]
    pub action: Option<SyncAction>,
    /// One-way: replace the server's collection with this one.
    #[arg(long, conflicts_with = "download")]
    pub upload: bool,
    /// One-way: replace this collection with the server's (a backup is taken first).
    #[arg(long)]
    pub download: bool,
    /// Sync the collection only, not media.
    #[arg(long)]
    pub no_media: bool,
}

#[derive(Debug, Clone, Subcommand)]
pub enum SyncAction {
    /// Configure a self-hosted sync server for this profile and log in.
    Login {
        /// Server URL, e.g. https://sync.example.org/ (AnkiWeb is refused).
        #[arg(long)]
        url: String,
        /// Prompted for when omitted.
        #[arg(long)]
        username: Option<String>,
        /// Read the password from the first line of stdin instead of the terminal.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Show whether a sync is needed. Changes nothing.
    Status,
    /// Forget this profile's stored login (the server URL stays configured).
    Logout,
    /// Sync automatically when tanki opens and closes this profile.
    Auto {
        #[arg(value_enum)]
        state: OnOff,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OnOff {
    On,
    Off,
}

/// `tanki sync-server`: Anki's own sync server, plain HTTP.
#[derive(Debug, Clone, Args)]
pub struct SyncServerArgs {
    /// Address to bind. Use 0.0.0.0 only behind a firewall, VPN or TLS proxy.
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,
    #[arg(long, default_value_t = 8080)]
    pub port: u16,
    /// The server's own data folder. Never an Anki profile or data folder.
    #[arg(long)]
    pub data: PathBuf,
    /// File with one `username:password` per line, mode 0600. Without it the
    /// server reads SYNC_USER1, SYNC_USER2, ... from the environment.
    #[arg(long)]
    pub users_file: Option<PathBuf>,
}

/// Sync outcomes with their own exit codes (`main.rs` downcasts to this).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncCliError {
    /// The collection and the server diverged; a one-way sync must be chosen.
    FullSyncChoiceRequired { upload: bool, download: bool },
    /// No stored login, or the server rejected it.
    AuthRequired(String),
    /// No server configured for this profile.
    NotConfigured,
}

impl SyncCliError {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::FullSyncChoiceRequired { .. } => 21,
            Self::AuthRequired(_) => 22,
            Self::NotConfigured => 23,
        }
    }
}

impl std::fmt::Display for SyncCliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FullSyncChoiceRequired { upload, download } => {
                f.write_str(
                    "This collection and the server's copy cannot be merged; a one-way sync \
                     must replace one of them.",
                )?;
                if *upload {
                    f.write_str(
                        " `tanki sync --upload` replaces the server's copy (and every other \
                         device's on its next sync) with this collection.",
                    )?;
                }
                if *download {
                    f.write_str(
                        " `tanki sync --download` replaces this collection with the server's \
                         copy; changes made here since the last sync are lost (a backup is \
                         taken first).",
                    )?;
                }
                Ok(())
            }
            Self::AuthRequired(why) => write!(f, "{why} Run `tanki sync login --url URL`."),
            Self::NotConfigured => f.write_str(
                "Sync is not configured for this profile. tanki syncs only with a self-hosted \
                 server: run `tanki sync login --url URL` (see `tanki sync-server`).",
            ),
        }
    }
}

impl std::error::Error for SyncCliError {}

/// Shown when a profile is pointed at a server for the first time, or at a
/// different server. The library has no actor command yet to ask whether the
/// collection synced before, so it is shown on every first configuration.
pub const FIRST_TIME_WARNING: &str = "Note: if this collection has synced with another server \
     before (for example AnkiWeb), the first sync with this server is a one-way full upload or \
     download that replaces one side, and every other device (desktop, phone) must then be \
     pointed at this server too. Syncing one collection with two servers forces a full sync \
     each time it switches.";

/// Input and output of one command, replaceable in tests.
pub struct Io<'a> {
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
    /// Reads a visible line after printing the prompt (username).
    pub read_line: &'a mut dyn FnMut(&str) -> std::io::Result<String>,
    /// Reads a secret (password) without echo.
    pub read_secret: &'a mut dyn FnMut(&str, bool) -> std::io::Result<String>,
}

pub fn run(
    actor: &CollectionActor,
    args: &SyncArgs,
    json: bool,
    config: &Path,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    let mut read_line = |prompt: &str| -> std::io::Result<String> {
        eprint!("{prompt}");
        std::io::stderr().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        Ok(line.trim_end_matches(['\r', '\n']).to_owned())
    };
    let mut read_secret = |prompt: &str, from_stdin: bool| -> std::io::Result<String> {
        if from_stdin {
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            Ok(line.trim_end_matches(['\r', '\n']).to_owned())
        } else {
            rpassword::prompt_password(prompt)
        }
    };
    run_with(
        actor,
        args,
        json,
        config,
        &mut Io {
            out: &mut stdout,
            err: &mut stderr,
            read_line: &mut read_line,
            read_secret: &mut read_secret,
        },
    )
}

fn profile_key(actor: &CollectionActor) -> String {
    actor.profile_dir().to_string_lossy().into_owned()
}

pub fn run_with(
    actor: &CollectionActor,
    args: &SyncArgs,
    json: bool,
    config_path: &Path,
    io: &mut Io,
) -> Result<()> {
    let profile = profile_key(actor);
    match &args.action {
        Some(SyncAction::Login {
            url,
            username,
            password_stdin,
        }) => login(
            actor,
            &profile,
            config_path,
            url,
            username.clone(),
            *password_stdin,
            json,
            io,
        ),
        Some(SyncAction::Logout) => {
            let path = credentials_path(config_path);
            let mut credentials = Credentials::load(&path)?;
            let removed = credentials.remove(&profile);
            credentials.save(&path)?;
            if json {
                writeln!(io.out, "{}", json!({ "logged_out": removed }))?;
            } else {
                writeln!(io.out, "Logged out.")?;
            }
            Ok(())
        }
        Some(SyncAction::Auto { state }) => {
            let mut config = Config::load(config_path)?;
            if config.sync_endpoint(&profile)?.is_none() {
                return Err(SyncCliError::NotConfigured.into());
            }
            config.profile_mut(&profile).sync.auto_sync = *state == OnOff::On;
            config.save(config_path)?;
            if json {
                writeln!(io.out, "{}", json!({ "auto_sync": *state == OnOff::On }))?;
            } else {
                writeln!(
                    io.out,
                    "Auto sync {}.",
                    if *state == OnOff::On { "on" } else { "off" }
                )?;
            }
            Ok(())
        }
        Some(SyncAction::Status) => {
            let auth = stored_auth(&profile, config_path)?;
            let report = match operation(actor, SyncCommand::Status { auth }, io.err) {
                Ok(DataResponse::Synced(report)) => report,
                Ok(other) => return Err(unexpected(other)),
                Err(error) => return Err(map_error(error, &profile, config_path)),
            };
            save_new_endpoint(&report, &profile, config_path)?;
            if json {
                writeln!(io.out, "{}", report_json(&report, None, false))?;
            } else {
                writeln!(io.out, "{}", status_text(report.required))?;
                server_message(&report, io.out)?;
            }
            Ok(())
        }
        None => sync(actor, args, json, &profile, config_path, io),
    }
}

#[allow(clippy::too_many_arguments)]
fn login(
    actor: &CollectionActor,
    profile: &str,
    config_path: &Path,
    url: &str,
    username: Option<String>,
    password_stdin: bool,
    json: bool,
    io: &mut Io,
) -> Result<()> {
    let endpoint = SyncEndpoint::parse(url)?;
    let mut config = Config::load(config_path)?;
    let previous = config.sync_endpoint(profile)?;
    let username = match username {
        Some(name) => name,
        None => (io.read_line)("Username: ")?,
    };
    if username.is_empty() {
        return Err("A username is required.".into());
    }
    let password = (io.read_secret)("Password: ", password_stdin)?;
    let changed = previous.as_ref() != Some(&endpoint);
    if changed {
        writeln!(io.err, "{FIRST_TIME_WARNING}")?;
    }
    let credentials = match operation(
        actor,
        SyncCommand::Login {
            endpoint: endpoint.as_str().into(),
            username: username.clone(),
            password,
        },
        io.err,
    ) {
        Ok(DataResponse::LoggedIn(credentials)) => credentials,
        Ok(other) => return Err(unexpected(other)),
        Err(error) => return Err(error.into()),
    };
    // Credentials first: a saved URL without a key is "auth required", a key
    // without a URL is unused.
    let path = credentials_path(config_path);
    let mut stored = Credentials::load(&path)?;
    stored.set(profile, credentials.clone())?;
    stored.save(&path)?;
    let sync = &mut config.profile_mut(profile).sync;
    sync.url = Some(credentials.endpoint.clone());
    sync.username = Some(username.clone());
    config.save(config_path)?;
    if json {
        writeln!(
            io.out,
            "{}",
            json!({
                "logged_in": true,
                "endpoint": credentials.endpoint,
                "username": username,
                "server_changed": changed,
            })
        )?;
    } else {
        writeln!(
            io.out,
            "Logged in to {} as {username}. Run `tanki sync` to sync.",
            credentials.endpoint
        )?;
    }
    Ok(())
}

fn sync(
    actor: &CollectionActor,
    args: &SyncArgs,
    json: bool,
    profile: &str,
    config_path: &Path,
    io: &mut Io,
) -> Result<()> {
    let auth = stored_auth(profile, config_path)?;
    let media = !args.no_media;
    let direction = match (args.upload, args.download) {
        (true, _) => SyncDirection::Upload,
        (_, true) => SyncDirection::Download,
        _ => SyncDirection::Normal,
    };
    let run = SyncCommand::Run {
        auth: auth.clone(),
        direction,
        media,
    };
    let report = match operation(actor, run, io.err) {
        Ok(DataResponse::Synced(report)) => Some(report),
        Ok(DataResponse::Done) => None,
        Ok(other) => return Err(unexpected(other)),
        Err(error) => return Err(map_error(error, profile, config_path)),
    };
    if let Some(report) = &report {
        save_new_endpoint(report, profile, config_path)?;
        if let SyncRequired::Full { upload, download } = report.required {
            return Err(SyncCliError::FullSyncChoiceRequired { upload, download }.into());
        }
    }
    // After a one-way sync, media follows (a normal run already did it).
    if direction != SyncDirection::Normal && media {
        let auth = stored_auth(profile, config_path)?;
        match operation(actor, SyncCommand::Media { auth }, io.err) {
            Ok(_) => {}
            Err(error) => return Err(map_error(error, profile, config_path)),
        }
    }
    let report = report.unwrap_or(SyncReport {
        required: SyncRequired::NoChanges,
        server_message: String::new(),
        new_endpoint: None,
    });
    if json {
        writeln!(io.out, "{}", report_json(&report, Some(direction), media))?;
    } else {
        writeln!(
            io.out,
            "{}",
            match direction {
                SyncDirection::Normal => "Sync complete.",
                SyncDirection::Upload => "Full upload complete.",
                SyncDirection::Download => "Full download complete.",
            }
        )?;
        server_message(&report, io.out)?;
    }
    Ok(())
}

/// The profile's credentials, refusing an unconfigured profile (23), a
/// missing login (22), and a login for a different server than configured.
fn stored_auth(profile: &str, config_path: &Path) -> Result<SyncCredentials> {
    let config = Config::load(config_path)?;
    let Some(endpoint) = config.sync_endpoint(profile)? else {
        return Err(SyncCliError::NotConfigured.into());
    };
    let credentials = Credentials::load(&credentials_path(config_path))?;
    match credentials.get(profile)? {
        Some(auth) if auth.endpoint == endpoint.as_str() => Ok(auth),
        Some(_) => Err(SyncCliError::AuthRequired(
            "The stored login belongs to a different server.".into(),
        )
        .into()),
        None => Err(SyncCliError::AuthRequired("Not logged in.".into()).into()),
    }
}

/// A rejected login is forgotten, as desktop does.
fn map_error(error: CoreError, profile: &str, config_path: &Path) -> Box<dyn std::error::Error> {
    if sync_ops::is_auth_error(&error) {
        let path = credentials_path(config_path);
        if let Ok(mut credentials) = Credentials::load(&path) {
            if credentials.remove(profile) {
                let _ = credentials.save(&path);
            }
        }
        return SyncCliError::AuthRequired("The sync server rejected the saved login.".into())
            .into();
    }
    error.into()
}

/// Persists a server redirect (already validated by tanki-core).
fn save_new_endpoint(report: &SyncReport, profile: &str, config_path: &Path) -> Result<()> {
    let Some(new) = &report.new_endpoint else {
        return Ok(());
    };
    let endpoint = SyncEndpoint::parse(new)?;
    let path = credentials_path(config_path);
    let mut credentials = Credentials::load(&path)?;
    if let Some(mut auth) = credentials.get(profile)? {
        auth.endpoint = endpoint.as_str().into();
        credentials.set(profile, auth)?;
        credentials.save(&path)?;
    }
    let mut config = Config::load(config_path)?;
    config.profile_mut(profile).sync.url = Some(endpoint.as_str().into());
    config.save(config_path)?;
    Ok(())
}

fn unexpected(response: DataResponse) -> Box<dyn std::error::Error> {
    format!("Unexpected reply to a sync command: {response:?}").into()
}

/// Queues a sync command and reports progress on `err` until it finishes.
fn operation(
    actor: &CollectionActor,
    command: SyncCommand,
    err: &mut dyn Write,
) -> tanki_core::Result<DataResponse> {
    let pending = actor.start_data(DataCommand::Sync(command))?;
    let mut last = String::new();
    let mut printed_at = Instant::now() - Duration::from_secs(1);
    loop {
        if let Some(response) = pending.try_recv()? {
            return Ok(response);
        }
        let line = progress_line(&actor.progress());
        if !line.is_empty() && line != last && printed_at.elapsed() >= Duration::from_millis(250) {
            let _ = writeln!(err, "{line}");
            last = line;
            printed_at = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn progress_line(progress: &tanki_core::OperationProgress) -> String {
    match (progress.current, progress.total) {
        (Some(current), Some(total)) => {
            format!("sync: {} {current}/{total}", progress.stage)
        }
        (Some(current), None) => format!("sync: {} {current}", progress.stage),
        _ if progress.stage.is_empty() => String::new(),
        _ => format!("sync: {}", progress.stage),
    }
}

fn status_text(required: SyncRequired) -> &'static str {
    match required {
        SyncRequired::NoChanges => "In sync.",
        SyncRequired::Normal => "Changes to sync.",
        SyncRequired::Full { .. } => "A one-way full sync is required.",
    }
}

fn server_message(report: &SyncReport, out: &mut dyn Write) -> std::io::Result<()> {
    if !report.server_message.is_empty() {
        writeln!(out, "Server: {}", report.server_message)?;
    }
    Ok(())
}

fn report_json(
    report: &SyncReport,
    direction: Option<SyncDirection>,
    media: bool,
) -> serde_json::Value {
    let (required, upload, download) = match report.required {
        SyncRequired::NoChanges => ("no_changes", false, false),
        SyncRequired::Normal => ("normal", false, false),
        SyncRequired::Full { upload, download } => ("full", upload, download),
    };
    let mut value = json!({
        "required": required,
        "upload_ok": upload,
        "download_ok": download,
        "server_message": report.server_message,
        "new_endpoint": report.new_endpoint,
    });
    if let Some(direction) = direction {
        value["direction"] = json!(match direction {
            SyncDirection::Normal => "normal",
            SyncDirection::Upload => "upload",
            SyncDirection::Download => "download",
        });
        value["media"] = json!(media);
    }
    value
}

/// Reads `username:password` lines; refuses a file other users can read.
pub fn read_users_file(path: &Path) -> Result<Vec<String>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)?.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(format!(
                "{} is readable by other users (mode {:o}); run chmod 600 on it.",
                path.display(),
                mode & 0o777
            )
            .into());
        }
    }
    let text = std::fs::read_to_string(path)?;
    let mut users = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !line.contains(':') {
            // Never echo the line: it may be a password.
            return Err(format!(
                "{} line {}: expected username:password",
                path.display(),
                index + 1
            )
            .into());
        }
        users.push(line.to_owned());
    }
    if users.is_empty() {
        return Err(format!("{} lists no users", path.display()).into());
    }
    Ok(users)
}

pub fn server_options(args: &SyncServerArgs) -> Result<SyncServerOptions> {
    let users = match &args.users_file {
        Some(path) => read_users_file(path)?,
        None => Vec::new(),
    };
    Ok(SyncServerOptions {
        host: args.host.clone(),
        port: args.port,
        data: args.data.clone(),
        users,
    })
}

/// Blocks until the server stops (Ctrl-C). Call before any other thread starts.
pub fn run_server(args: &SyncServerArgs) -> std::result::Result<(), Box<dyn std::error::Error>> {
    sync_ops::run_server(server_options(args)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::io::BufRead;

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        command: Top,
    }
    #[derive(clap::Subcommand)]
    enum Top {
        Sync(SyncArgs),
        SyncServer(SyncServerArgs),
    }

    fn sync_args(args: &[&str]) -> std::result::Result<SyncArgs, clap::Error> {
        let cli = Cli::try_parse_from(["tanki", "sync"].iter().chain(args))?;
        match cli.command {
            Top::Sync(args) => Ok(args),
            Top::SyncServer(_) => unreachable!(),
        }
    }

    #[test]
    fn parses_sync_arguments() {
        let args = sync_args(&[]).unwrap();
        assert!(args.action.is_none() && !args.upload && !args.download && !args.no_media);
        let args = sync_args(&["--upload", "--no-media"]).unwrap();
        assert!(args.upload && args.no_media);
        assert!(sync_args(&["--download"]).unwrap().download);
        assert!(sync_args(&["--upload", "--download"]).is_err());
        let args = sync_args(&["login", "--url", "http://h:1/", "--username", "u"]).unwrap();
        assert!(matches!(
            args.action,
            Some(SyncAction::Login { ref url, username: Some(ref u), password_stdin: false })
                if url == "http://h:1/" && u == "u"
        ));
        assert!(sync_args(&["login"]).is_err(), "--url is required");
        assert!(sync_args(&["login", "--url", "x", "--password", "p"]).is_err());
        assert!(matches!(
            sync_args(&["status"]).unwrap().action,
            Some(SyncAction::Status)
        ));
        assert!(matches!(
            sync_args(&["auto", "on"]).unwrap().action,
            Some(SyncAction::Auto { state: OnOff::On })
        ));
        assert!(sync_args(&["status", "--upload"]).is_err());
    }

    #[test]
    fn parses_sync_server_arguments() {
        let cli = Cli::try_parse_from(["tanki", "sync-server", "--data", "/tmp/d"]).unwrap();
        let Top::SyncServer(args) = cli.command else {
            panic!()
        };
        assert_eq!(
            (args.host.as_str(), args.port, args.data.as_path()),
            ("127.0.0.1", 8080, Path::new("/tmp/d"))
        );
        assert!(args.users_file.is_none());
        assert!(Cli::try_parse_from(["tanki", "sync-server"]).is_err());
        assert!(
            Cli::try_parse_from(["tanki", "sync-server", "--data", "d", "--user", "u:p"]).is_err()
        );
    }

    #[test]
    fn exit_codes_and_messages() {
        let full = SyncCliError::FullSyncChoiceRequired {
            upload: true,
            download: true,
        };
        assert_eq!(full.exit_code(), 21);
        let text = full.to_string();
        assert!(text.contains("--upload") && text.contains("--download"));
        assert!(text.contains("replaces"));
        let only_upload = SyncCliError::FullSyncChoiceRequired {
            upload: true,
            download: false,
        };
        assert!(!only_upload.to_string().contains("--download"));
        assert_eq!(SyncCliError::AuthRequired("x".into()).exit_code(), 22);
        assert_eq!(SyncCliError::NotConfigured.exit_code(), 23);
        // main.rs downcasts the boxed error.
        let boxed: Box<dyn std::error::Error> = SyncCliError::NotConfigured.into();
        assert_eq!(
            boxed
                .downcast_ref::<SyncCliError>()
                .map(SyncCliError::exit_code),
            Some(23)
        );
    }

    #[test]
    fn progress_lines() {
        let p = |stage: &str, current, total| tanki_core::OperationProgress {
            stage: stage.into(),
            current,
            total,
        };
        assert_eq!(progress_line(&p("", None, None)), "");
        assert_eq!(progress_line(&p("x", None, None)), "sync: x");
        assert_eq!(
            progress_line(&p("Transferring", Some(3), Some(9))),
            "sync: Transferring 3/9"
        );
    }

    #[cfg(unix)]
    #[test]
    fn users_file_must_be_private_and_is_never_echoed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("users");
        std::fs::write(&path, "# comment\nalice:pw1\n\n bob:pw2 \n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_users_file(&path)
            .unwrap_err()
            .to_string()
            .contains("chmod 600"));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_users_file(&path).unwrap(), ["alice:pw1", "bob:pw2"]);
        std::fs::write(&path, "secretwithoutcolon\n").unwrap();
        let error = read_users_file(&path).unwrap_err().to_string();
        assert!(!error.contains("secretwithoutcolon"), "{error}");
        let args = SyncServerArgs {
            host: "127.0.0.1".into(),
            port: 0,
            data: dir.path().join("data"),
            users_file: Some(path.clone()),
        };
        std::fs::write(&path, "u:p\n").unwrap();
        assert_eq!(server_options(&args).unwrap().users, ["u:p"]);
        // A profile folder is refused before the server starts.
        std::fs::create_dir(dir.path().join("profile")).unwrap();
        std::fs::write(dir.path().join("profile/collection.anki2"), b"").unwrap();
        let refused = SyncServerArgs {
            data: dir.path().join("profile"),
            ..args
        };
        assert!(run_server(&refused).is_err());
    }

    // End to end against Anki's server, in a child process of this test
    // binary (the server reads its users from the environment).

    const CHILD_ENV: &str = "TANKI_CLI_SYNC_TEST_SERVER_DATA";
    const PREFIX: &str = "TANKI_SYNC_SERVER_ADDR=";
    const USER: &str = "cli";
    const PASSWORD: &str = "cli-password";

    #[test]
    fn server_child() {
        let Some(data) = std::env::var_os(CHILD_ENV) else {
            return;
        };
        sync_ops::serve(
            SyncServerOptions {
                host: "127.0.0.1".into(),
                port: 0,
                data: data.into(),
                users: vec![format!("{USER}:{PASSWORD}")],
            },
            |address| println!("{PREFIX}{address}"),
        )
        .unwrap();
    }

    struct Server {
        child: std::process::Child,
        url: String,
        _data: tempfile::TempDir,
    }
    impl Server {
        fn start() -> Self {
            let data = tempfile::tempdir().unwrap();
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sync_commands::tests::server_child",
                    "--nocapture",
                ])
                .env(CHILD_ENV, data.path())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let mut lines = std::io::BufReader::new(child.stdout.take().unwrap()).lines();
            let address = loop {
                let line = lines.next().expect("server child exited").unwrap();
                if let Some((_, address)) = line.split_once(PREFIX) {
                    break address.trim().to_owned();
                }
            };
            std::thread::spawn(move || lines.for_each(drop));
            Self {
                child,
                url: format!("http://{address}/"),
                _data: data,
            }
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    struct Outcome {
        result: Result<()>,
        out: String,
        err: String,
    }

    fn call(actor: &CollectionActor, args: &[&str], config: &Path, password: &str) -> Outcome {
        let args = sync_args(args).unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut read_line = |_: &str| Ok("unused".to_owned());
        let mut read_secret = |_: &str, _: bool| Ok(password.to_owned());
        let result = run_with(
            actor,
            &args,
            true,
            config,
            &mut Io {
                out: &mut out,
                err: &mut err,
                read_line: &mut read_line,
                read_secret: &mut read_secret,
            },
        );
        Outcome {
            result,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    }

    fn exit_code(outcome: &Outcome) -> Option<u8> {
        outcome
            .result
            .as_ref()
            .err()
            .and_then(|e| e.downcast_ref::<SyncCliError>())
            .map(SyncCliError::exit_code)
    }

    fn new_actor() -> (tempfile::TempDir, CollectionActor) {
        let dir = tempfile::tempdir().unwrap();
        let actor = CollectionActor::open(dir.path()).unwrap();
        actor
            .execute(tanki_core::Command::GenerateStudyFixture)
            .unwrap();
        (dir, actor)
    }

    #[test]
    fn login_status_conflict_and_auth_exit_codes() {
        let server = Server::start();
        let (_profile, actor) = new_actor();
        let config_dir = tempfile::tempdir().unwrap();
        let config = config_dir.path().join("tanki/config.toml");

        let not_configured = call(&actor, &[], &config, "");
        assert_eq!(exit_code(&not_configured), Some(23));
        assert_eq!(exit_code(&call(&actor, &["status"], &config, "")), Some(23));

        let refused = call(
            &actor,
            &[
                "login",
                "--url",
                "https://sync.ankiweb.net/",
                "--username",
                USER,
            ],
            &config,
            PASSWORD,
        );
        assert!(refused.result.unwrap_err().to_string().contains("AnkiWeb"));
        let wrong = call(
            &actor,
            &["login", "--url", &server.url, "--username", USER],
            &config,
            "wrong",
        );
        assert!(wrong.result.is_err());
        assert!(!config.exists(), "a failed login saved nothing");

        let login = call(
            &actor,
            &["login", "--url", &server.url, "--username", USER],
            &config,
            PASSWORD,
        );
        login.result.unwrap();
        let reply: serde_json::Value = serde_json::from_str(&login.out).unwrap();
        assert_eq!(reply["endpoint"], server.url.as_str());
        assert_eq!(reply["username"], USER);
        assert_eq!(reply["server_changed"], true);
        assert!(login.err.contains("one-way full upload or download"));
        let hkey = Credentials::load(&credentials_path(&config))
            .unwrap()
            .get(&profile_key(&actor))
            .unwrap()
            .unwrap()
            .hkey;
        for text in [
            &login.out,
            &login.err,
            &std::fs::read_to_string(&config).unwrap(),
        ] {
            assert!(!text.contains(&hkey) && !text.contains(PASSWORD), "{text}");
        }
        let saved = Config::load(&config).unwrap();
        let sync = &saved.profile(&profile_key(&actor)).unwrap().sync;
        assert_eq!(sync.url.as_deref(), Some(server.url.as_str()));
        assert!(!sync.auto_sync);
        // Same server again: no warning.
        let again = call(
            &actor,
            &["login", "--url", &server.url, "--username", USER],
            &config,
            PASSWORD,
        );
        again.result.unwrap();
        assert!(!again.err.contains("one-way"));

        let status = call(&actor, &["status"], &config, "");
        status.result.unwrap();
        let status: serde_json::Value = serde_json::from_str(&status.out).unwrap();
        assert_eq!(status["required"], "full");

        // First sync with an empty server: the user must choose.
        let conflict = call(&actor, &[], &config, "");
        assert_eq!(exit_code(&conflict), Some(21));
        assert!(conflict.out.is_empty());

        call(&actor, &["auto", "on"], &config, "").result.unwrap();
        let saved = Config::load(&config).unwrap();
        assert!(saved.profile(&profile_key(&actor)).unwrap().sync.auto_sync);

        // A rejected key is forgotten and reported as auth required.
        let path = credentials_path(&config);
        let mut credentials = Credentials::load(&path).unwrap();
        let mut stale = credentials.get(&profile_key(&actor)).unwrap().unwrap();
        stale.hkey = "not-a-valid-key".into();
        credentials.set(&profile_key(&actor), stale).unwrap();
        credentials.save(&path).unwrap();
        assert_eq!(
            exit_code(&call(&actor, &["--no-media"], &config, "")),
            Some(22)
        );
        assert!(Credentials::load(&path)
            .unwrap()
            .get(&profile_key(&actor))
            .unwrap()
            .is_none());
        assert_eq!(exit_code(&call(&actor, &[], &config, "")), Some(22));
        call(&actor, &["logout"], &config, "").result.unwrap();
        actor.close().unwrap();
    }

    #[test]
    #[ignore = "needs lead actor interception"]
    fn upload_then_normal_sync() {
        let server = Server::start();
        let (_profile, actor) = new_actor();
        let config_dir = tempfile::tempdir().unwrap();
        let config = config_dir.path().join("config.toml");
        call(
            &actor,
            &["login", "--url", &server.url, "--username", USER],
            &config,
            PASSWORD,
        )
        .result
        .unwrap();
        assert_eq!(exit_code(&call(&actor, &[], &config, "")), Some(21));
        let upload = call(&actor, &["--upload"], &config, "");
        upload.result.unwrap();
        let reply: serde_json::Value = serde_json::from_str(&upload.out).unwrap();
        assert_eq!(reply["direction"], "upload");
        let normal = call(&actor, &[], &config, "");
        normal.result.unwrap();
        let reply: serde_json::Value = serde_json::from_str(&normal.out).unwrap();
        assert_eq!(reply["required"], "no_changes");
        actor.close().unwrap();
    }

    #[test]
    #[ignore = "needs lead actor interception"]
    fn download_into_fresh_profile() {
        let server = Server::start();
        let (_a_dir, a) = new_actor();
        let config_dir = tempfile::tempdir().unwrap();
        let config = config_dir.path().join("config.toml");
        let login = ["login", "--url", server.url.as_str(), "--username", USER];
        call(&a, &login, &config, PASSWORD).result.unwrap();
        call(&a, &["--upload"], &config, "").result.unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let b = CollectionActor::open(b_dir.path()).unwrap();
        call(&b, &login, &config, PASSWORD).result.unwrap();
        let download = call(&b, &["--download"], &config, "");
        download.result.unwrap();
        let ids = |actor: &CollectionActor| {
            actor
                .execute(tanki_core::Command::Search { query: "".into() })
                .unwrap()
        };
        assert_eq!(format!("{:?}", ids(&a)), format!("{:?}", ids(&b)));
        a.close().unwrap();
        b.close().unwrap();
    }
}
