//! Fixture-only public API spike. No production full-download path is exposed.
use super::*;
use anki::{progress::ProgressState, sync::collection::normal::SyncActionRequired};
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    response::IntoResponse,
    Router,
};
use std::{path::PathBuf, process::Command as ProcessCommand, time::Duration};
use tokio::{runtime::Runtime, sync::Notify};

struct Fixture {
    profile: PathBuf,
    col: Option<Collection>,
    progress: Arc<Mutex<ProgressState>>,
    joined_backups: usize,
}

impl Fixture {
    fn new(profile: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&profile)?;
        let profile = safety::validate_profile(&profile)?;
        safety::peek_and_backup(&profile)?;
        std::fs::create_dir_all(profile.join("collection.media"))?;
        std::fs::create_dir_all(profile.join("backups"))?;
        let mut fixture = Self {
            profile,
            col: None,
            progress: Arc::default(),
            joined_backups: 0,
        };
        fixture.reopen()?;
        fixture.backup()?;
        Ok(fixture)
    }

    fn col(&mut self) -> &mut Collection {
        self.col.as_mut().unwrap()
    }

    fn reopen(&mut self) -> Result<()> {
        self.col = Some(
            CollectionBuilder::new(self.profile.join("collection.anki2"))
                .set_media_paths(
                    self.profile.join("collection.media"),
                    self.profile.join("collection.media.db2"),
                )
                .set_shared_progress_state(self.progress.clone())
                .build()?,
        );
        Ok(())
    }

    /// Same rule as the actor: a written backup is followed by close and reopen,
    /// because Anki's backup drops the cross-process file lock.
    fn backup(&mut self) -> Result<()> {
        let profile = self.profile.clone();
        if backup(self.col(), &profile)? {
            self.joined_backups += 1;
            self.col.take().unwrap().close(None)?;
            self.reopen()?;
        }
        Ok(())
    }

    fn transfer(
        &mut self,
        rt: &Runtime,
        auth: anki::sync::login::SyncAuth,
        client: reqwest::Client,
        upload: bool,
    ) -> Result<()> {
        if !upload {
            self.backup()?;
        }
        let col = self.col.take().unwrap();
        let outcome = if upload {
            rt.block_on(col.full_upload(auth, client))
        } else {
            // Explicit user exception: generated temporary fixture, completed backup,
            // no other process. Upstream closes SQLite before replacing the file.
            rt.block_on(col.full_download(auth, client))
        };
        // Reopen even when a transfer failed or was interrupted.
        self.reopen()?;
        outcome.map_err(CoreError::from)
    }

    fn search(&mut self, query: &str) -> Result<Vec<i64>> {
        Ok(SearchService::search_cards(
            self.col(),
            pb::search::SearchRequest {
                search: query.into(),
                order: None,
            },
        )?
        .ids)
    }

    fn add(&mut self, front: &str) -> Result<()> {
        let mut queued = HashMap::new();
        execute(
            self.col(),
            Command::AddNote {
                notetype: "Basic".into(),
                fields: vec![front.into(), "answer".into()],
            },
            &mut queued,
        )?;
        Ok(())
    }

    fn typed_progress(&self) -> pb::collection::progress::Value {
        anki::progress::progress_to_proto(
            self.progress.lock().unwrap().last_progress,
            &anki_i18n::I18n::template_only(),
        )
        .value
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(mut col) = self.col.take() {
            if let Err(error) = backup(&mut col, &self.profile) {
                eprintln!("fixture exit backup: {error}");
            }
            if let Err(error) = col.close(None) {
                eprintln!("fixture exit close: {error}");
            }
        }
    }
}

#[test]
fn public_sync_round_trip() {
    // Isolate upstream's environment-based user configuration from parallel tests.
    if std::env::var_os("TANKI_SYNC_SPIKE_CHILD").is_none() {
        let status = ProcessCommand::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "actor::sync_spike::public_sync_round_trip",
                "--nocapture",
            ])
            .env("TANKI_SYNC_SPIKE_CHILD", "1")
            .env("SYNC_USER1", "fixture:fixture-password")
            .env_remove("SYNC_USER2")
            .env_remove("PASSWORDS_HASHED")
            .status()
            .unwrap();
        assert!(status.success(), "isolated sync spike failed: {status}");
        return;
    }
    std::thread::Builder::new()
        .name("tanki-sync-spike-actor".into())
        .spawn(round_trip_on_actor)
        .unwrap()
        .join()
        .unwrap()
        .unwrap();
}

fn round_trip_on_actor() -> Result<()> {
    let root = tempfile::tempdir()?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let (address, future) = rt
        .block_on(anki::sync::http_server::SimpleServer::make_server(
            anki::sync::http_server::SyncServerConfig {
                host: "127.0.0.1".parse().unwrap(),
                port: 0,
                base_folder: root.path().join("server"),
                ip_header: anki::sync::http_server::default_ip_header(),
            },
        ))
        .map_err(|e| CoreError::Other(e.to_string()))?;
    let server = rt.spawn(future);
    let endpoint = SyncEndpoint::parse(&format!("http://{address}/"))?;
    let client = crate::sync::client()?;
    assert!(rt
        .block_on(anki::sync::login::sync_login(
            "fixture",
            "wrong",
            Some(endpoint.as_str().into()),
            client.clone()
        ))
        .is_err());
    let login = rt.block_on(anki::sync::login::sync_login(
        "fixture",
        "fixture-password",
        Some(endpoint.as_str().into()),
        client.clone(),
    ))?;
    assert!(
        login.endpoint.is_none(),
        "upstream login unexpectedly preserves endpoint"
    );
    let auth = endpoint.auth(login.hkey);
    println!("login: explicit loopback endpoint; returned endpoint restored");

    let mut a = Fixture::new(root.path().join("client-a"))?;
    crate::fixture::populate(a.col())?;
    let original = a.search("")?;
    assert!(!original.is_empty());
    let output = rt.block_on(a.col().normal_sync(auth.clone(), client.clone()))?;
    assert!(matches!(
        output.required,
        SyncActionRequired::FullSyncRequired {
            upload_ok: true,
            ..
        }
    ));
    assert!(matches!(
        a.typed_progress(),
        pb::collection::progress::Value::NormalSync(_)
    ));
    a.transfer(&rt, auth.clone(), client.clone(), true)?;
    assert!(matches!(
        a.typed_progress(),
        pb::collection::progress::Value::FullSync(_)
    ));
    println!(
        "full upload: {} cards; typed FullSync progress",
        original.len()
    );

    let mut b = Fixture::new(root.path().join("client-b"))?;
    b.add("download-discarded-sentinel")?;
    let before_backup = b.joined_backups;
    b.transfer(&rt, auth.clone(), client.clone(), false)?;
    assert!(
        b.joined_backups > before_backup,
        "download did not finish a backup"
    );
    assert_eq!(b.search("")?, original);
    assert!(b.search("download-discarded-sentinel")?.is_empty());
    println!("full download: backup joined first; exact card IDs round-tripped");

    a.add("normal-from-a")?;
    rt.block_on(a.col().normal_sync(auth.clone(), client.clone()))?;
    rt.block_on(b.col().normal_sync(auth.clone(), client.clone()))?;
    assert_eq!(b.search("normal-from-a")?.len(), 1);
    b.add("normal-from-b")?;
    rt.block_on(b.col().normal_sync(auth.clone(), client.clone()))?;
    rt.block_on(a.col().normal_sync(auth.clone(), client.clone()))?;
    assert_eq!(a.search("normal-from-b")?.len(), 1);
    assert_eq!(a.search("")?, b.search("")?);
    println!("normal sync: notes propagated in both directions");

    rt.block_on(a.col().media()?.sync_media(
        a.col().new_progress_handler(),
        auth.clone(),
        client.clone(),
        None,
    ))?;
    rt.block_on(b.col().media()?.sync_media(
        b.col().new_progress_handler(),
        auth.clone(),
        client.clone(),
        None,
    ))?;
    for file in ["fixture.svg", "fixture.wav"] {
        assert_eq!(
            std::fs::read(a.profile.join("collection.media").join(file))?,
            std::fs::read(b.profile.join("collection.media").join(file))?
        );
    }
    assert!(matches!(
        b.typed_progress(),
        pb::collection::progress::Value::MediaSync(_)
    ));
    println!("media sync: SVG and WAV byte-for-byte; typed MediaSync progress");

    abort_with_want_abort(&rt, &mut a, &endpoint, &auth, &client)?;
    abort_by_drop_then_reopen(&rt, &mut a, &endpoint, &auth, &client)?;
    protocol_redirect_is_followed(&rt, &mut a, &endpoint, &auth, &client)?;
    // rslib/build.rs always sets BUILDHASH from vendor/anki/out/buildhash (empty
    // when the file is absent), so a BUILDHASH build variable set by tanki is
    // overridden and the client string is "anki,<version> (),<platform>".
    assert_eq!(anki::version::buildhash(), "");
    println!("client string build hash is empty; the server accepted it for every request above");
    a.backup()?;
    b.backup()?;
    println!(
        "backup workers joined: a={}, b={}",
        a.joined_backups, b.joined_backups
    );
    drop(b);
    drop(a);
    server.abort();
    Ok(())
}

/// A proxy in front of the real server that parks `/sync/start`, the first
/// request made after `normal_sync` has opened its local transaction. It
/// signals `arrived`, then waits for `release` before forwarding.
struct GatedProxy {
    address: std::net::SocketAddr,
    task: tokio::task::JoinHandle<()>,
    arrived: Arc<Notify>,
    release: Arc<Notify>,
}

fn gated_proxy(
    rt: &Runtime,
    endpoint: &SyncEndpoint,
    client: &reqwest::Client,
) -> Result<GatedProxy> {
    let arrived = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (notify, gate) = (arrived.clone(), release.clone());
    let upstream = endpoint.as_str().to_string();
    let proxy_client = client.clone();
    let router = Router::new().fallback(move |req: Request<Body>| {
        let (notify, gate) = (notify.clone(), gate.clone());
        let upstream = upstream.clone();
        let client = proxy_client.clone();
        async move {
            if req.uri().path() == "/sync/start" {
                notify.notify_one();
                gate.notified().await;
            }
            let (mut parts, body) = req.into_parts();
            let data = to_bytes(body, 10 * 1024 * 1024).await.unwrap();
            for header in ["transfer-encoding", "content-length", "connection", "host"] {
                parts.headers.remove(header);
            }
            let response = client
                .post(format!(
                    "{}{}",
                    upstream,
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
    let (address, task) = spawn_router(rt, router)?;
    Ok(GatedProxy {
        address,
        task,
        arrived,
        release,
    })
}

/// Cooperative abort: `want_abort` is set on the shared progress state while a
/// request is in flight. Upstream notices at its next progress check, rolls its
/// transaction back and tells the server to abort. The collection stays usable
/// with no reopen.
fn abort_with_want_abort(
    rt: &Runtime,
    a: &mut Fixture,
    endpoint: &SyncEndpoint,
    auth: &anki::sync::login::SyncAuth,
    client: &reqwest::Client,
) -> Result<()> {
    a.add("survives-cooperative-abort")?;
    let GatedProxy {
        address,
        task: proxy,
        arrived,
        release,
    } = gated_proxy(rt, endpoint, client)?;
    let proxy_auth = SyncEndpoint::parse(&format!("http://{address}/"))?.auth(auth.hkey.clone());
    let progress = a.progress.clone();
    let outcome = rt.block_on(async {
        let flag = async {
            arrived.notified().await;
            progress.lock().unwrap().want_abort = true;
            release.notify_one();
        };
        tokio::join!(a.col().normal_sync(proxy_auth, client.clone()), flag).0
    });
    assert!(
        matches!(outcome, Err(anki::error::AnkiError::Interrupted)),
        "expected Interrupted, got {outcome:?}"
    );
    // Same connection, no reopen: reads, writes and the next sync all work.
    assert_eq!(a.search("survives-cooperative-abort")?.len(), 1);
    a.add("added-after-cooperative-abort")?;
    let output = rt.block_on(a.col().normal_sync(auth.clone(), client.clone()))?;
    assert!(matches!(output.required, SyncActionRequired::NoChanges));
    proxy.abort();
    println!(
        "abort (want_abort): Interrupted returned; same connection kept working; next sync passed"
    );
    Ok(())
}

/// Hard abort: the sync future is dropped while its transaction is open (what
/// happens if tanki cancels the task instead of asking). Upstream gets no
/// chance to roll back, so the connection must be closed and reopened.
fn abort_by_drop_then_reopen(
    rt: &Runtime,
    a: &mut Fixture,
    endpoint: &SyncEndpoint,
    auth: &anki::sync::login::SyncAuth,
    client: &reqwest::Client,
) -> Result<()> {
    a.add("survives-dropped-sync")?;
    let GatedProxy {
        address,
        task: proxy,
        arrived,
        release: _release,
    } = gated_proxy(rt, endpoint, client)?;
    let proxy_auth = SyncEndpoint::parse(&format!("http://{address}/"))?.auth(auth.hkey.clone());
    let outcome = rt.block_on(async {
        tokio::select! {
            value = a.col().normal_sync(proxy_auth, client.clone()) =>
                Err(CoreError::Other(format!("sync finished before abort: {value:?}"))),
            _ = arrived.notified() => Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(10)) =>
                Err(CoreError::Other("timed out waiting for sync start".into())),
        }
    });
    assert!(matches!(
        a.typed_progress(),
        pb::collection::progress::Value::NormalSync(_)
    ));
    // Closing the connection rolls the abandoned transaction back.
    a.col.take().unwrap().close(None)?;
    a.reopen()?;
    outcome?;
    assert_eq!(a.search("survives-dropped-sync")?.len(), 1);
    // The server still has the session open; tell it, as upstream would have.
    let _ = rt.block_on(anki::sync::collection::progress::sync_abort(
        auth.clone(),
        client.clone(),
    ));
    let output = rt.block_on(a.col().normal_sync(auth.clone(), client.clone()))?;
    assert!(matches!(output.required, SyncActionRequired::NoChanges));
    proxy.abort();
    println!("abort (future dropped): close and reopen kept committed notes; next sync passed");
    Ok(())
}

fn spawn_router(
    rt: &Runtime,
    router: Router,
) -> Result<(std::net::SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = rt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))?;
    let address = listener.local_addr()?;
    let server = rt.spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Ok((address, server))
}

/// Anki follows a protocol-level 308 itself (it reads the `Location` header and
/// retries), so reqwest's redirect policy never sees it and validating the
/// configured URL is not enough. Both servers here are on loopback: no test may
/// provoke a request towards AnkiWeb, even one a guard is expected to stop.
fn protocol_redirect_is_followed(
    rt: &Runtime,
    a: &mut Fixture,
    endpoint: &SyncEndpoint,
    auth: &anki::sync::login::SyncAuth,
    client: &reqwest::Client,
) -> Result<()> {
    let target = endpoint.as_str().to_string();
    let location = target.clone();
    let router = Router::new().fallback(move || {
        let location = location.clone();
        async move { (StatusCode::PERMANENT_REDIRECT, [("location", location)], "") }
    });
    let (address, server) = spawn_router(rt, router)?;
    let redirecting = SyncEndpoint::parse(&format!("http://{address}/"))?.auth(auth.hkey.clone());
    a.add("after-redirect")?;
    let output = rt.block_on(a.col().normal_sync(redirecting, client.clone()))?;
    assert_eq!(output.new_endpoint.as_deref(), Some(target.as_str()));
    assert!(matches!(output.required, SyncActionRequired::NoChanges));
    server.abort();
    println!("protocol 308: upstream followed it to {target} and reported new_endpoint");
    Ok(())
}
