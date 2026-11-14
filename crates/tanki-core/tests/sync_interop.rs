//! Phase 2 exit test (PLAN 11): two clients share one sync server; one is
//! tanki, the other Anki's own pinned Python package. Edits and reviews made
//! on each arrive on the other with no full-sync prompt after the initial
//! one, and both media folders end up byte-identical.
//!
//! The server is Anki's `SimpleServer` on loopback, run in a child process of
//! this test binary (`server_child`) because it reads its users from the
//! environment. tanki drives its collection through `CollectionActor`
//! (`start_data` + `progress`), including the full upload; Python
//! (`scripts/sync-interop.py`) creates an empty collection, full-downloads,
//! then normal-syncs. Each side adds notes (with media), edits a note made by
//! the other side, flags/marks and answers cards. At the end Python reads
//! both collections with the public card/note services and the dataset
//! export (as `study_replay.rs` does; no SQL), and the test compares them
//! exactly, including modification times and USNs.
//!
//! Skips unless `TANKI_PINNED_PYTHON` and `TANKI_PINNED_PYTHONPATH` are set:
//!
//! ```sh
//! TANKI_PINNED_PYTHON=~/.local/share/AnkiProgramFiles/.venv/bin/python \
//! TANKI_PINNED_PYTHONPATH="$PWD/.tools/pinned-python" \
//!     cargo test -p tanki-core --test sync_interop -- --nocapture
//! ```
use serde_json::{json, Value};
use std::{
    ffi::OsString,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command as Process, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tanki_core::*;

const USER: &str = "interop";
const PASSWORD: &str = "interop-password";
const CHILD_ENV: &str = "TANKI_SYNC_INTEROP_SERVER_DATA";
const ADDRESS_PREFIX: &str = "TANKI_SYNC_SERVER_ADDR=";

const CARD_COLUMNS: [&str; 23] = [
    "id",
    "nid",
    "did",
    "ord",
    "mod",
    "usn",
    "type",
    "queue",
    "due",
    "ivl",
    "factor",
    "reps",
    "lapses",
    "left",
    "odue",
    "odid",
    "flags",
    "original_position",
    "memory_state",
    "desired_retention",
    "decay",
    "last_review_time",
    "custom_data",
];
const REVLOG_COLUMNS: [&str; 8] = [
    "id", "cid", "ease", "ivl", "lastIvl", "factor", "time", "type",
];
const NOTE_COLUMNS: [&str; 7] = ["id", "mod", "usn", "tags", "flds", "guid", "mid"];

fn pinned_version() -> &'static str {
    include_str!("../../../vendor/anki/.version").trim()
}

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

struct Python {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Python {
    fn start(exe: &OsString, pythonpath: &OsString) -> Self {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/sync-interop.py");
        let mut command = Process::new(exe);
        command
            .arg("-B")
            .arg("-s")
            .arg(script)
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env("PYTHONPATH", pythonpath)
            .env_remove("PYTHONHOME")
            .env_remove("PYTHONSTARTUP")
            .current_dir(std::env::temp_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        // Anki's Python client honours proxy variables; loopback only here.
        for var in [
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
        ] {
            command.env_remove(var);
        }
        let mut child = command
            .spawn()
            .unwrap_or_else(|e| panic!("starting {exe:?}: {e}"));
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin: Some(stdin),
            stdout,
        }
    }

    fn call(&mut self, request: Value) -> Value {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{request}").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("bad reply to {request}: {e}: {line:?}"));
        if let Some(error) = reply.get("error") {
            panic!(
                "sync-interop.py failed on {request}:\n{}",
                error.as_str().unwrap_or("")
            );
        }
        reply
    }

    fn quit(mut self) {
        let _ = writeln!(self.stdin.as_mut().unwrap(), "{}", json!({"cmd": "quit"}));
        self.stdin.take();
        let status = self.child.wait().unwrap();
        assert!(status.success(), "sync-interop.py exited with {status}");
    }
}

impl Drop for Python {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.wait();
    }
}

/// `start_data` + `progress` polling, as the UI does.
fn data(actor: &CollectionActor, command: SyncCommand) -> DataResponse {
    let text = format!("{command:?}");
    let pending = actor.start_data(DataCommand::Sync(command)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let _ = actor.progress();
        match pending.try_recv() {
            Ok(Some(response)) => return response,
            Ok(None) => {
                assert!(Instant::now() < deadline, "{text} did not finish");
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("{text}: {error}"),
        }
    }
}

fn tanki_sync(actor: &CollectionActor, auth: &SyncCredentials) -> SyncRequired {
    match data(
        actor,
        SyncCommand::Run {
            auth: auth.clone(),
            direction: SyncDirection::Normal,
            media: true,
        },
    ) {
        DataResponse::Synced(report) => {
            assert!(report.new_endpoint.is_none());
            report.required
        }
        other => panic!("{other:?}"),
    }
}

fn execute(actor: &CollectionActor, command: Command) -> Response {
    let text = format!("{command:?}");
    actor
        .execute(command)
        .unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn search(actor: &CollectionActor, query: &str) -> Vec<i64> {
    match execute(
        actor,
        Command::Search {
            query: query.into(),
        },
    ) {
        Response::CardIds(ids) => ids,
        other => panic!("{other:?}"),
    }
}

fn tanki_answer(actor: &CollectionActor, count: usize) -> Vec<i64> {
    let mut answered = Vec::new();
    for _ in 0..count {
        let Response::Queue(queue) = execute(actor, Command::Queue { limit: 1 }) else {
            panic!("Queue")
        };
        let Some(card) = queue.cards.first() else {
            break;
        };
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        execute(
            actor,
            Command::Answer {
                card_id: card.id,
                rating: Rating::Good,
                answered_at_millis: now.as_millis() as i64,
                milliseconds_taken: 3000,
            },
        );
        answered.push(card.id);
    }
    answered
}

fn media_files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
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

/// Python's reply to `sync` must not ask for a full sync.
fn assert_normal(reply: &Value, step: &str) {
    let required = reply["required"].as_str().unwrap();
    assert!(
        matches!(required, "NO_CHANGES" | "NORMAL_SYNC"),
        "{step}: Python was asked for {required}"
    );
    assert_eq!(reply["full_download"], false);
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn python_and_tanki_clients_share_one_server() {
    let var = |name| std::env::var_os(name).filter(|v| !v.is_empty());
    let (Some(python_exe), Some(pythonpath)) =
        (var("TANKI_PINNED_PYTHON"), var("TANKI_PINNED_PYTHONPATH"))
    else {
        eprintln!(
            "skipping sync_interop: set TANKI_PINNED_PYTHON (interpreter with Anki's Python \
             dependencies) and TANKI_PINNED_PYTHONPATH (folder with the pinned anki {} package) \
             to run the Phase 2 exit test",
            pinned_version()
        );
        return;
    };
    let server = Server::start();
    let mut python = Python::start(&python_exe, &pythonpath);
    assert_eq!(
        python.call(json!({"cmd": "version"}))["version"],
        pinned_version()
    );
    let mut transcript = Vec::new();

    // 1. tanki: study fixture, login, first sync (full upload expected).
    let tanki_dir = tempfile::tempdir().unwrap();
    let tanki = CollectionActor::open(tanki_dir.path()).unwrap();
    execute(&tanki, Command::GenerateStudyFixture);
    let DataResponse::LoggedIn(auth) = data(
        &tanki,
        SyncCommand::Login {
            endpoint: server.endpoint.clone(),
            username: USER.into(),
            password: PASSWORD.into(),
        },
    ) else {
        panic!("login")
    };
    assert_eq!(auth.endpoint, server.endpoint);
    let first = tanki_sync(&tanki, &auth);
    assert_eq!(
        first,
        SyncRequired::Full {
            upload: true,
            download: false
        }
    );
    let uploaded = data(
        &tanki,
        SyncCommand::Run {
            auth: auth.clone(),
            direction: SyncDirection::Upload,
            media: true,
        },
    );
    assert!(matches!(uploaded, DataResponse::Done), "{uploaded:?}");
    assert!(matches!(
        data(&tanki, SyncCommand::Media { auth: auth.clone() }),
        DataResponse::Done
    ));
    transcript.push(format!("tanki first sync: {first:?}; full upload; media"));

    // 2. Python: empty collection, login, full download (its initial prompt).
    let python_dir = tempfile::tempdir().unwrap();
    let python_col = python_dir.path().join("collection.anki2");
    python.call(json!({"cmd": "open", "path": python_col}));
    python.call(json!({
        "cmd": "login", "endpoint": server.endpoint, "username": USER, "password": PASSWORD,
    }));
    let initial = python.call(json!({"cmd": "sync", "allow_full_download": true}));
    assert_eq!(initial["required"], "FULL_DOWNLOAD");
    assert_eq!(initial["full_download"], true);
    python.call(json!({"cmd": "media"}));
    transcript.push("python first sync: FULL_DOWNLOAD; downloaded; media".into());

    // 3. tanki edits: a note with media, a flag, a marked note, reviews.
    let png_a: Vec<u8> = (0u8..=255).cycle().take(3000).collect();
    std::fs::write(tanki.media_dir().join("tanki-one.png"), &png_a).unwrap();
    let Response::NoteAdded { id: tanki_note, .. } = execute(
        &tanki,
        Command::AddNote {
            notetype: "Basic".into(),
            fields: vec![
                r#"tanki-one <img src="tanki-one.png">"#.into(),
                "from tanki".into(),
            ],
        },
    ) else {
        panic!("AddNote")
    };
    execute(
        &tanki,
        Command::SetMarked {
            note_ids: vec![tanki_note],
            marked: true,
        },
    );
    let reviewed_by_tanki = tanki_answer(&tanki, 3);
    assert_eq!(reviewed_by_tanki.len(), 3);
    execute(
        &tanki,
        Command::SetFlag {
            card_ids: vec![reviewed_by_tanki[0]],
            flag: 3,
        },
    );
    let required = tanki_sync(&tanki, &auth);
    assert_eq!(required, SyncRequired::NoChanges);
    transcript.push(format!("tanki round 1 sync: {required:?}"));
    let reply = python.call(json!({"cmd": "sync"}));
    assert_normal(&reply, "python round 1");
    python.call(json!({"cmd": "media"}));
    transcript.push(format!("python round 1 sync: {}", reply["required"]));
    let found = python.call(json!({"cmd": "find_notes", "query": "tanki-one tag:marked"}));
    assert_eq!(found["note_ids"], json!([tanki_note]));

    // 4. Python edits: a note with media, an edit to tanki's note, reviews.
    let png_b: Vec<u8> = (0u8..200).rev().cycle().take(5000).collect();
    let added = python.call(json!({
        "cmd": "add_note", "front": "python-one", "back": "from python",
        "media_name": "python-one.png", "media_hex": hex(&png_b),
    }));
    python.call(json!({
        "cmd": "edit_back", "query": "tanki-one", "back": "edited by python",
    }));
    let reviewed_by_python = python.call(json!({"cmd": "answer", "count": 3}));
    assert_eq!(reviewed_by_python["card_ids"].as_array().unwrap().len(), 3);
    let reply = python.call(json!({"cmd": "sync"}));
    assert_normal(&reply, "python round 2");
    python.call(json!({"cmd": "media"}));
    transcript.push(format!("python round 2 sync: {}", reply["required"]));
    let required = tanki_sync(&tanki, &auth);
    assert_eq!(required, SyncRequired::NoChanges);
    transcript.push(format!("tanki round 2 sync: {required:?}"));
    assert_eq!(search(&tanki, "python-one").len(), 1);
    assert_eq!(search(&tanki, r#""back:edited by python""#).len(), 1);
    for id in reviewed_by_python["card_ids"].as_array().unwrap() {
        assert!(search(&tanki, "rated:1").contains(&id.as_i64().unwrap()));
    }
    assert_eq!(
        std::fs::read(
            tanki
                .media_dir()
                .join(added["media_name"].as_str().unwrap())
        )
        .unwrap(),
        png_b
    );

    // 5. One more review round from tanki, then Python catches up.
    let more = tanki_answer(&tanki, 2);
    assert!(!more.is_empty());
    let required = tanki_sync(&tanki, &auth);
    assert_eq!(required, SyncRequired::NoChanges);
    transcript.push(format!("tanki round 3 sync: {required:?}"));
    let reply = python.call(json!({"cmd": "sync"}));
    assert_normal(&reply, "python round 3");
    python.call(json!({"cmd": "media"}));
    transcript.push(format!("python round 3 sync: {}", reply["required"]));

    // 6. Compare everything.
    let tanki_media = tanki.media_dir();
    tanki.close().unwrap();
    python.call(json!({"cmd": "close"}));
    let tanki_col = tanki_dir.path().join("collection.anki2");
    let ours = python.call(json!({"cmd": "dump", "path": tanki_col}));
    let theirs = python.call(json!({"cmd": "dump", "path": python_col}));
    python.quit();
    compare_rows("cards", &CARD_COLUMNS, &ours["cards"], &theirs["cards"]);
    compare_rows(
        "revlog",
        &REVLOG_COLUMNS,
        &ours["revlog"],
        &theirs["revlog"],
    );
    compare_rows("notes", &NOTE_COLUMNS, &ours["notes"], &theirs["notes"]);
    let ours_media = media_files(&tanki_media);
    let theirs_media = media_files(&python_dir.path().join("collection.media"));
    let names = |files: &[(String, Vec<u8>)]| files.iter().map(|f| f.0.clone()).collect::<Vec<_>>();
    assert_eq!(names(&ours_media), names(&theirs_media), "media file names");
    assert_eq!(ours_media, theirs_media, "media bytes");
    assert!(names(&ours_media).contains(&"tanki-one.png".to_owned()));

    for line in &transcript {
        println!("sync_interop: {line}");
    }
    println!(
        "sync_interop: Anki {} | {} cards, {} review-log rows, {} notes exactly equal (all \
         service fields incl. mod and usn) | {} media files byte-identical | no full-sync prompt \
         after the initial upload/download",
        pinned_version(),
        ours["cards"].as_array().unwrap().len(),
        ours["revlog"].as_array().unwrap().len(),
        ours["notes"].as_array().unwrap().len(),
        ours_media.len(),
    );
}

/// Exact comparison with a field-specific diagnostic.
fn compare_rows(table: &str, columns: &[&str], ours: &Value, theirs: &Value) {
    let (ours, theirs) = (ours.as_array().unwrap(), theirs.as_array().unwrap());
    let ids = |rows: &[Value]| rows.iter().map(|r| r[0].clone()).collect::<Vec<_>>();
    assert_eq!(ids(ours), ids(theirs), "{table}: different row ids");
    for (a, b) in ours.iter().zip(theirs) {
        let (a, b) = (a.as_array().unwrap(), b.as_array().unwrap());
        assert_eq!(a.len(), columns.len(), "{table}: unexpected columns");
        assert_eq!(a.len(), b.len());
        for (column, (x, y)) in a.iter().zip(b).enumerate() {
            assert_eq!(x, y, "{table} id {} {}", a[0], columns[column]);
        }
    }
}
