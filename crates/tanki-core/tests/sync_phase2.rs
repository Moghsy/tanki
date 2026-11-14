//! Phase 2 sync tests (PLAN 8.9, 10, 13) against Anki's own sync server on
//! loopback. Every collection is a generated fixture in a temp folder.
//!
//! The server runs in a child process of this test binary (`server_child`)
//! because Anki's `SimpleServer` reads its users from `SYNC_USER*` variables
//! when it starts; the child is the only process whose environment changes.
//! No test contacts AnkiWeb: the redirect test points at a loopback server
//! that answers with an AnkiWeb `Location`, and tanki's resolver refuses the
//! name before any lookup or connection.
use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command as Process, Stdio},
    sync::mpsc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tanki_core::{config::*, sync_ops, *};

const USER: &str = "fixture";
const PASSWORD: &str = "fixture-password";
const CHILD_ENV: &str = "TANKI_SYNC_TEST_SERVER_DATA";
const ADDRESS_PREFIX: &str = "TANKI_SYNC_SERVER_ADDR=";

/// Not a test of its own: the body of the server child process.
#[test]
fn server_child() {
    let Some(data) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    sync_ops::serve(
        SyncServerOptions {
            host: "127.0.0.1".into(),
            port: 0,
            data: PathBuf::from(data),
            users: vec![format!("{USER}:{PASSWORD}")],
        },
        |address| println!("{ADDRESS_PREFIX}{address}"),
    )
    .unwrap();
}

struct Server {
    child: Child,
    endpoint: String,
    _data: tempfile::TempDir,
}

impl Server {
    fn start() -> Self {
        let data = tempfile::tempdir().unwrap();
        let mut child = Process::new(std::env::current_exe().unwrap())
            .args(["--exact", "server_child", "--nocapture", "--test-threads=1"])
            .env(CHILD_ENV, data.path())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let address = loop {
            let line = lines
                .next()
                .expect("server child exited before listening")
                .unwrap();
            if let Some((_, address)) = line.split_once(ADDRESS_PREFIX) {
                break address.trim().to_owned();
            }
        };
        // Keep draining so the child never blocks on a full pipe.
        std::thread::spawn(move || lines.for_each(drop));
        Self {
            child,
            endpoint: format!("http://{address}/"),
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

/// Runs a data command the way the UI does: queue it, poll progress until
/// the reply arrives. Returns the reply and every progress stage seen.
fn run(actor: &CollectionActor, command: SyncCommand) -> (Result<DataResponse>, Vec<String>) {
    let pending = actor.start_data(DataCommand::Sync(command)).unwrap();
    let mut stages = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let stage = actor.progress().stage;
        if !stage.is_empty() && stages.last() != Some(&stage) {
            stages.push(stage);
        }
        match pending.try_recv() {
            Ok(Some(response)) => return (Ok(response), stages),
            Ok(None) => {
                assert!(Instant::now() < deadline, "sync did not finish");
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return (Err(error), stages),
        }
    }
}

fn synced(actor: &CollectionActor, command: SyncCommand) -> SyncReport {
    match run(actor, command).0 {
        Ok(DataResponse::Synced(report)) => report,
        other => panic!("expected Synced, got {other:?}"),
    }
}

fn normal(actor: &CollectionActor, auth: &SyncCredentials, media: bool) -> SyncReport {
    synced(
        actor,
        SyncCommand::Run {
            auth: auth.clone(),
            direction: SyncDirection::Normal,
            media,
        },
    )
}

fn login(actor: &CollectionActor, endpoint: &str, password: &str) -> Result<SyncCredentials> {
    match run(
        actor,
        SyncCommand::Login {
            endpoint: endpoint.into(),
            username: USER.into(),
            password: password.into(),
        },
    )
    .0?
    {
        DataResponse::LoggedIn(credentials) => Ok(credentials),
        other => panic!("expected LoggedIn, got {other:?}"),
    }
}

fn search(actor: &CollectionActor, query: &str) -> Vec<i64> {
    match actor
        .execute(Command::Search {
            query: query.into(),
        })
        .unwrap()
    {
        Response::CardIds(ids) => ids,
        other => panic!("{other:?}"),
    }
}

fn add_note(actor: &CollectionActor, front: &str) {
    actor
        .execute(Command::AddNote {
            notetype: "Basic".into(),
            fields: vec![front.into(), "back".into()],
        })
        .unwrap();
}

/// Answers the first queued card of the current deck; returns its id.
fn answer_one(actor: &CollectionActor) -> i64 {
    let Response::Queue(queue) = actor.execute(Command::Queue { limit: 1 }).unwrap() else {
        panic!("Queue")
    };
    let card_id = queue.cards.first().expect("a due card").id;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    actor
        .execute(Command::Answer {
            card_id,
            rating: Rating::Good,
            answered_at_millis: now.as_millis() as i64,
            milliseconds_taken: 4000,
        })
        .unwrap();
    card_id
}

/// Opens a closed profile's collection directly, as the lead's actor will
/// for full syncs (tests may own a `Collection`).
fn open_direct(profile: &Path) -> anki::collection::Collection {
    anki::collection::CollectionBuilder::new(profile.join("collection.anki2"))
        .set_media_paths(
            profile.join("collection.media"),
            profile.join("collection.media.db2"),
        )
        .build()
        .unwrap()
}

/// A temp profile whose collection the actor has created and backed up.
fn new_profile(study_fixture: bool) -> (tempfile::TempDir, CollectionActor) {
    let dir = tempfile::tempdir().unwrap();
    let actor = CollectionActor::open(dir.path()).unwrap();
    if study_fixture {
        actor.execute(Command::GenerateStudyFixture).unwrap();
    }
    (dir, actor)
}

/// Full upload with the actor closed; returns a reopened actor.
fn full_upload(dir: &Path, actor: CollectionActor, auth: &SyncCredentials) -> CollectionActor {
    actor.close().unwrap();
    sync_ops::upload(open_direct(dir), auth.clone()).unwrap();
    let mut col = open_direct(dir);
    assert!(sync_ops::has_synced_before(&mut col).unwrap());
    col.close(None).unwrap();
    CollectionActor::open(dir).unwrap()
}

/// Full download into a temp fixture only (never a real profile).
fn full_download(dir: &Path, actor: CollectionActor, auth: &SyncCredentials) -> CollectionActor {
    actor.close().unwrap();
    assert!(dir.starts_with(std::env::temp_dir().canonicalize().unwrap()));
    sync_ops::download(open_direct(dir), auth.clone()).unwrap();
    let mut col = open_direct(dir);
    assert!(sync_ops::has_synced_before(&mut col).unwrap());
    col.close(None).unwrap();
    CollectionActor::open(dir).unwrap()
}

fn media_files(actor: &CollectionActor) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<_> = std::fs::read_dir(actor.media_dir())
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().unwrap().is_file())
        .map(|entry| {
            (
                entry.file_name().to_string_lossy().into_owned(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect();
    files.sort();
    files
}

#[test]
fn login_refuses_wrong_password_and_keeps_configured_endpoint() {
    let server = Server::start();
    let (_dir, actor) = new_profile(false);
    let error = login(&actor, &server.endpoint, "wrong").unwrap_err();
    assert_eq!(
        error,
        CoreError::Invalid("The sync server rejected this username or password.".into())
    );
    let credentials = login(&actor, &server.endpoint, PASSWORD).unwrap();
    assert_eq!(credentials.endpoint, server.endpoint);
    assert!(!credentials.hkey.is_empty());
    let debug = format!("{credentials:?}");
    assert!(!debug.contains(&credentials.hkey), "{debug}");
    // A rejected host key maps to "auth required".
    let stale = SyncCredentials {
        hkey: "0000".into(),
        ..credentials
    };
    let (outcome, _) = run(
        &actor,
        SyncCommand::Status {
            auth: stale.clone(),
        },
    );
    assert!(sync_ops::is_auth_error(&outcome.unwrap_err()));
    actor.close().unwrap();
}

#[test]
fn ankiweb_urls_and_redirects_to_ankiweb_are_refused() {
    let server = Server::start();
    let (_dir, actor) = new_profile(false);
    for url in [
        "https://ankiweb.net/",
        "https://sync.ankiweb.net/",
        "https://SYNC2.AnkiWeb.net./",
    ] {
        let error = login(&actor, url, PASSWORD).unwrap_err();
        assert!(error.to_string().contains("AnkiWeb"), "{url}: {error}");
        let (outcome, _) = run(
            &actor,
            SyncCommand::Run {
                auth: SyncCredentials {
                    endpoint: url.into(),
                    hkey: "k".into(),
                    timeout_secs: 5,
                },
                direction: SyncDirection::Normal,
                media: true,
            },
        );
        assert!(outcome.unwrap_err().to_string().contains("AnkiWeb"));
    }
    let auth = login(&actor, &server.endpoint, PASSWORD).unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let redirector = |location: String| {
        let router = axum::Router::new().fallback(move || {
            let location = location.clone();
            async move {
                (
                    axum::http::StatusCode::PERMANENT_REDIRECT,
                    [("location", location)],
                    "",
                )
            }
        });
        let listener = rt
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let address = listener.local_addr().unwrap();
        rt.spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{address}/")
    };
    // To AnkiWeb: Anki follows the 308 itself; the resolver refuses the name.
    let to_ankiweb = SyncCredentials {
        endpoint: redirector("https://sync.ankiweb.net/".into()),
        ..auth.clone()
    };
    for command in [
        SyncCommand::Status {
            auth: to_ankiweb.clone(),
        },
        SyncCommand::Run {
            auth: to_ankiweb.clone(),
            direction: SyncDirection::Normal,
            media: true,
        },
    ] {
        let error = run(&actor, command).0.unwrap_err();
        assert_eq!(
            error,
            CoreError::Invalid(
                "The sync server redirected tanki to AnkiWeb, which was refused: AnkiWeb's \
                 terms exclude third-party clients. Check the server URL."
                    .into()
            )
        );
    }
    // To another self-hosted server: followed, validated and reported.
    let moved = SyncCredentials {
        endpoint: redirector(server.endpoint.clone()),
        ..auth.clone()
    };
    let report = synced(
        &actor,
        SyncCommand::Status {
            auth: moved.clone(),
        },
    );
    assert_eq!(
        report.new_endpoint.as_deref(),
        Some(server.endpoint.as_str())
    );
    let report = normal(&actor, &moved, false);
    assert_eq!(
        report.new_endpoint.as_deref(),
        Some(server.endpoint.as_str())
    );
    println!("redirects: AnkiWeb refused before lookup; loopback target reported as new_endpoint");
    actor.close().unwrap();
}

#[test]
fn full_upload_download_then_normal_sync_both_ways_with_media() {
    let server = Server::start();
    let (a_dir, a) = new_profile(true);
    let auth = login(&a, &server.endpoint, PASSWORD).unwrap();

    // First sync with an empty server: a one-way upload is required.
    let first = normal(&a, &auth, true);
    assert_eq!(
        first.required,
        SyncRequired::Full {
            upload: true,
            download: false
        }
    );
    // Status reports the same without changing anything.
    let status = synced(&a, SyncCommand::Status { auth: auth.clone() });
    assert!(matches!(status.required, SyncRequired::Full { .. }));
    a.close().unwrap();
    let mut col = open_direct(a_dir.path());
    assert!(!sync_ops::has_synced_before(&mut col).unwrap());
    col.close(None).unwrap();
    let a = full_upload(
        a_dir.path(),
        CollectionActor::open(a_dir.path()).unwrap(),
        &auth,
    );
    let report = normal(&a, &auth, true);
    assert_eq!(report.required, SyncRequired::NoChanges);
    let original = search(&a, "");

    let (b_dir, b) = new_profile(false);
    add_note(&b, "discarded-by-download");
    let b = full_download(b_dir.path(), b, &auth);
    assert_eq!(search(&b, ""), original);
    assert!(search(&b, "discarded-by-download").is_empty());
    // Media, polled through the actor's progress.
    let (outcome, stages) = run(&b, SyncCommand::Media { auth: auth.clone() });
    assert!(matches!(outcome, Ok(DataResponse::Done)));
    assert!(
        !b.progress().stage.is_empty(),
        "media progress was not kept"
    );
    assert_eq!(media_files(&a), media_files(&b));
    assert!(media_files(&b).len() >= 3);
    println!(
        "full upload/download: {} cards; media identical; stages seen: {stages:?}",
        original.len()
    );

    // Edits and reviews, A -> B.
    add_note(&a, "from-a");
    let answered_a = answer_one(&a);
    std::fs::write(a.media_dir().join("from-a.bin"), [1u8, 2, 3, 4]).unwrap();
    assert_eq!(normal(&a, &auth, true).required, SyncRequired::NoChanges);
    assert_eq!(normal(&b, &auth, true).required, SyncRequired::NoChanges);
    assert_eq!(search(&b, "from-a").len(), 1);
    assert!(search(&b, "rated:1").contains(&answered_a));

    // And B -> A.
    add_note(&b, "from-b");
    let answered_b = answer_one(&b);
    std::fs::write(b.media_dir().join("from-b.bin"), [9u8; 2048]).unwrap();
    assert_eq!(normal(&b, &auth, true).required, SyncRequired::NoChanges);
    assert_eq!(normal(&a, &auth, true).required, SyncRequired::NoChanges);
    assert_eq!(search(&a, "from-b").len(), 1);
    assert!(search(&a, "rated:1").contains(&answered_b));
    assert_eq!(search(&a, ""), search(&b, ""));
    assert_eq!(search(&a, "rated:1"), search(&b, "rated:1"));
    assert_eq!(media_files(&a), media_files(&b));
    // No full sync on either side. The side that synced last is clean; the
    // other reports a normal sync, because every completed sync stamps a new
    // server modification time (Anki's protocol, not a pending change).
    let last = synced(&a, SyncCommand::Status { auth: auth.clone() });
    assert_eq!(last.required, SyncRequired::NoChanges);
    let other = synced(&b, SyncCommand::Status { auth: auth.clone() });
    assert_eq!(other.required, SyncRequired::Normal);
    println!("normal sync: notes, reviews and media arrived both ways; no full sync prompt");
    a.close().unwrap();
    b.close().unwrap();
}

/// A proxy that parks `/sync/start` (sent after `normal_sync` opened its
/// transaction) until released.
struct GatedProxy {
    endpoint: String,
    arrived: mpsc::Receiver<()>,
    release: std::sync::Arc<tokio::sync::Notify>,
}

fn gated_proxy(rt: &tokio::runtime::Runtime, upstream: &str) -> GatedProxy {
    use axum::{body::Body, http::Request, response::IntoResponse};
    let (arrived_tx, arrived) = mpsc::channel();
    let arrived_tx = std::sync::Arc::new(std::sync::Mutex::new(arrived_tx));
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    let gate = release.clone();
    let upstream = upstream.to_owned();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let router = axum::Router::new().fallback(move |req: Request<Body>| {
        let (arrived_tx, gate, upstream, client) = (
            arrived_tx.clone(),
            gate.clone(),
            upstream.clone(),
            client.clone(),
        );
        async move {
            if req.uri().path() == "/sync/start" {
                let _ = arrived_tx.lock().unwrap().send(());
                gate.notified().await;
            }
            let (mut parts, body) = req.into_parts();
            let data = axum::body::to_bytes(body, 64 << 20).await.unwrap();
            for header in ["transfer-encoding", "content-length", "connection", "host"] {
                parts.headers.remove(header);
            }
            let response = client
                .post(format!(
                    "{upstream}{}",
                    parts.uri.path().trim_start_matches('/')
                ))
                .headers(parts.headers)
                .body(data)
                .send()
                .await
                .unwrap();
            let status = response.status();
            let mut headers = response.headers().clone();
            for header in ["transfer-encoding", "content-length", "connection"] {
                headers.remove(header);
            }
            (status, headers, response.bytes().await.unwrap()).into_response()
        }
    });
    let listener = rt
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let address = listener.local_addr().unwrap();
    rt.spawn(async move { axum::serve(listener, router).await.unwrap() });
    GatedProxy {
        endpoint: format!("http://{address}/"),
        arrived,
        release,
    }
}

#[test]
fn cancel_operation_interrupts_sync_and_the_next_sync_passes() {
    let server = Server::start();
    let (dir, a) = new_profile(true);
    let auth = login(&a, &server.endpoint, PASSWORD).unwrap();
    let a = full_upload(dir.path(), a, &auth);
    add_note(&a, "survives-cancel");

    let rt = tokio::runtime::Runtime::new().unwrap();
    let proxy = gated_proxy(&rt, &server.endpoint);
    let pending = a
        .start_data(DataCommand::Sync(SyncCommand::Run {
            auth: SyncCredentials {
                endpoint: proxy.endpoint.clone(),
                ..auth.clone()
            },
            direction: SyncDirection::Normal,
            media: true,
        }))
        .unwrap();
    proxy
        .arrived
        .recv_timeout(Duration::from_secs(30))
        .expect("sync reached the server");
    a.cancel_operation();
    // Let the throttled progress check fire after the release.
    std::thread::sleep(Duration::from_millis(150));
    proxy.release.notify_one();
    assert_eq!(pending.wait().unwrap_err(), CoreError::Interrupted);
    // Same collection, no reopen: the note survived and the next sync passes.
    assert_eq!(search(&a, "survives-cancel").len(), 1);
    assert_eq!(normal(&a, &auth, true).required, SyncRequired::NoChanges);
    let status = synced(&a, SyncCommand::Status { auth: auth.clone() });
    assert_eq!(status.required, SyncRequired::NoChanges);
    println!("cancel: Interrupted, rolled back on the same connection, next sync passed");
    a.close().unwrap();
}

#[test]
fn two_servers_on_one_collection_force_a_full_sync() {
    let first = Server::start();
    let second = Server::start();
    let (dir, a) = new_profile(true);
    let auth1 = login(&a, &first.endpoint, PASSWORD).unwrap();
    let auth2 = login(&a, &second.endpoint, PASSWORD).unwrap();
    let a = full_upload(dir.path(), a, &auth1);
    assert_eq!(normal(&a, &auth1, false).required, SyncRequired::NoChanges);

    // Another device already uses the second server.
    let (other_dir, other) = new_profile(false);
    add_note(&other, "other-device");
    let other = full_upload(other_dir.path(), other, &auth2);

    // The same collection, now pointed at the second server.
    add_note(&a, "after-first-server");
    let switched = normal(&a, &auth2, true);
    assert_eq!(
        switched.required,
        SyncRequired::Full {
            upload: true,
            download: true
        }
    );
    // Choosing upload replaces the second server's data ...
    let a = full_upload(dir.path(), a, &auth2);
    assert_eq!(normal(&a, &auth2, false).required, SyncRequired::NoChanges);
    // ... and going back to the first server forces a full sync again.
    let back = normal(&a, &auth1, false);
    assert!(
        matches!(back.required, SyncRequired::Full { .. }),
        "{back:?}"
    );
    println!(
        "two servers: switching to a second server -> {:?}; back to the first -> {:?}",
        switched.required, back.required
    );
    a.close().unwrap();
    other.close().unwrap();
}

#[test]
fn config_round_trip_atomic_save_and_seen_collections() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tanki").join(CONFIG_FILE);
    let mut config = Config::load(&path).unwrap();
    assert_eq!(config, Config::default());
    config.last_profile = Some("User 1".into());
    let profile = config.profile_mut("/tmp/profile");
    profile.sync.url = Some("http://127.0.0.1:8080/".into());
    profile.sync.username = Some("me".into());
    assert!(config.mark_seen(Path::new("/tmp/profile/collection.anki2")));
    assert!(!config.mark_seen(Path::new("/tmp/profile/collection.anki2")));
    config.save(&path).unwrap();
    let loaded = Config::load(&path).unwrap();
    assert_eq!(loaded, config);
    assert!(loaded.has_seen(Path::new("/tmp/profile/collection.anki2")));
    assert!(!loaded.has_seen(Path::new("/tmp/other/collection.anki2")));
    assert!(!loaded.profile("/tmp/profile").unwrap().sync.auto_sync);
    assert_eq!(
        loaded
            .sync_endpoint("/tmp/profile")
            .unwrap()
            .unwrap()
            .as_str(),
        "http://127.0.0.1:8080/"
    );
    // Saving again replaces the file whole and leaves no temporary file.
    config.profile_mut("/tmp/profile").sync.auto_sync = true;
    config.save(&path).unwrap();
    assert!(
        Config::load(&path)
            .unwrap()
            .profile("/tmp/profile")
            .unwrap()
            .sync
            .auto_sync
    );
    let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, [CONFIG_FILE]);
    // A broken file is reported, not silently replaced.
    std::fs::write(&path, "profiles = 3").unwrap();
    assert!(Config::load(&path).is_err());
}

#[cfg(unix)]
#[test]
fn credentials_file_is_0600_and_redacted() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("cfg").join(CONFIG_FILE);
    let path = credentials_path(&config_path);
    let secret = "0123456789abcdef-secret-host-key";
    let mut credentials = Credentials::load(&path).unwrap();
    credentials
        .set(
            "/tmp/profile",
            SyncCredentials {
                endpoint: "http://127.0.0.1:8080/".into(),
                hkey: secret.into(),
                timeout_secs: 30,
            },
        )
        .unwrap();
    assert!(credentials
        .set(
            "/tmp/x",
            SyncCredentials {
                endpoint: "https://sync.ankiweb.net/".into(),
                hkey: secret.into(),
                timeout_secs: 30,
            },
        )
        .is_err());
    credentials.save(&path).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let folder_mode = std::fs::metadata(path.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(folder_mode, 0o700);
    let loaded = Credentials::load(&path).unwrap();
    let stored = loaded.get("/tmp/profile").unwrap().unwrap();
    assert_eq!(stored.hkey, secret);
    for debug in [format!("{loaded:?}"), format!("{stored:?}")] {
        assert!(!debug.contains(secret), "{debug}");
        assert!(debug.contains("redacted"), "{debug}");
    }
    // Overwriting keeps 0600 even if someone loosened the old file.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let error = Credentials::load(&path).unwrap_err();
    assert!(!error.to_string().contains(secret));
    let mut again = loaded.clone();
    assert!(again.remove("/tmp/profile"));
    again.save(&path).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(Credentials::load(&path)
        .unwrap()
        .get("/tmp/profile")
        .unwrap()
        .is_none());
    // A hand-edited AnkiWeb endpoint is refused when read.
    std::fs::write(
        &path,
        format!("[profiles.p]\nendpoint = \"https://ankiweb.net/\"\nhkey = \"{secret}\"\ntimeout_secs = 1\n"),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let error = Credentials::load(&path).unwrap().get("p").unwrap_err();
    assert!(!error.to_string().contains(secret));
}

#[test]
fn sync_server_refuses_profile_and_anki_data_folders() {
    let dir = tempfile::tempdir().unwrap();
    let ok = dir.path().join("server-data");
    assert!(sync_ops::check_server_data_folder(&ok).is_ok());
    std::fs::create_dir(&ok).unwrap();
    assert!(sync_ops::check_server_data_folder(&ok).is_ok());

    let profile = dir.path().join("profile");
    std::fs::create_dir(&profile).unwrap();
    std::fs::write(profile.join("collection.anki2"), b"").unwrap();
    let base = dir.path().join("base");
    std::fs::create_dir(&base).unwrap();
    std::fs::write(base.join("prefs21.db"), b"").unwrap();
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    for bad in [
        profile.clone(),
        profile.join("server"),
        base.clone(),
        base.join("User 1").join("server"),
        home.join(".local/share/Anki2"),
        home.join(".local/share/Anki2/sync"),
        home.join(".var/app/net.ankiweb.Anki/data/Anki2/x"),
        dir.path().join("a/../profile"),
    ] {
        let error = sync_ops::check_server_data_folder(&bad).unwrap_err();
        assert!(
            matches!(error, CoreError::UnsafePath(_)),
            "{bad:?}: {error}"
        );
    }
    // The server never starts (and no environment changes) on a refusal.
    let before = std::env::var_os("SYNC_USER1");
    let error = sync_ops::run_server(SyncServerOptions {
        host: "127.0.0.1".into(),
        port: 0,
        data: profile,
        users: vec!["u:p".into()],
    })
    .unwrap_err();
    assert!(error.to_string().contains("collection.anki2"), "{error}");
    assert_eq!(std::env::var_os("SYNC_USER1"), before);
}
