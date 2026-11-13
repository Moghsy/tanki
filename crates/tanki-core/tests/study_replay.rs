//! Phase 1 exit test (PLAN 11): on a fixture collection, a scripted session
//! answers every due card, and the resulting card states and review log
//! equal those produced by Anki's own Python package given the same answers
//! and timestamps.
//!
//! The study fixture (`Command::GenerateStudyFixture`) is built once, closed,
//! and copied into two temp profiles. tanki drives one copy through
//! `CollectionActor` the way the TUI does (`Queue{limit:1}`, `Render`,
//! `StudyConfig`, `CompareAnswer` for type-in cards, `Answer`);
//! `scripts/study-replay.py` drives the other with the pinned Python package
//! (`col.sched.get_queued_cards()`, `build_answer`, `answer_card`), in lock
//! step: at every step both sides must offer the same card with the same
//! counts, then both answer it with the same rating, `answered_at_millis`
//! and `milliseconds_taken`. Finally Python reads both copies with the public
//! card/note services and exports the dataset protobuf for review logs. No SQL.
//!
//! The dataset export preserves all review-log scheduling fields, but sets
//! `usn` to zero, so sync USNs are outside this test's scope. All card and note
//! service fields and exported review-log fields must match exactly, including
//! modification times, learning due times, custom data and FSRS state. Calls
//! start early in the wall-clock second because upstream takes learning due
//! times and modification times from its own clock, not answered_at_millis.
//!
//! Skips unless `TANKI_PINNED_PYTHON` names an interpreter with Anki's Python
//! dependencies and `TANKI_PINNED_PYTHONPATH` the folder holding the pinned
//! `anki` package:
//!
//! ```sh
//! TANKI_PINNED_PYTHON=~/.local/share/AnkiProgramFiles/.venv/bin/python \
//! TANKI_PINNED_PYTHONPATH="$PWD/.tools/pinned-python" \
//!     cargo test -p tanki-core --test study_replay -- --nocapture
//! ```
use serde_json::{json, Value};
use std::{
    ffi::OsString,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command as Process, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tanki_core::*;

/// Public service fields, in the order emitted by the Python helper.
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

/// Upper bound on answers; the fixture needs far fewer.
const MAX_STEPS: usize = 600;
/// Ratings cycle through this sequence (index = step mod length).
const RATINGS: [Rating; 7] = [
    Rating::Good,
    Rating::Again,
    Rating::Easy,
    Rating::Good,
    Rating::Hard,
    Rating::Good,
    Rating::Again,
];

fn pinned_version() -> &'static str {
    include_str!("../../../vendor/anki/.version").trim()
}

#[test]
fn replay_matches_python() {
    let var = |name| std::env::var_os(name).filter(|v| !v.is_empty());
    let (Some(python), Some(pythonpath)) =
        (var("TANKI_PINNED_PYTHON"), var("TANKI_PINNED_PYTHONPATH"))
    else {
        eprintln!(
            "skipping study_replay: set TANKI_PINNED_PYTHON (interpreter with Anki's Python \
             dependencies) and TANKI_PINNED_PYTHONPATH (folder with the pinned anki {} package) \
             to run the Phase 1 exit test",
            pinned_version()
        );
        return;
    };
    std::thread::Builder::new()
        .name("tanki-study-replay".into())
        .stack_size(64 << 20)
        .spawn(move || {
            for attempt in 0..2 {
                if run(python.clone(), pythonpath.clone()) {
                    return;
                }
                eprintln!("study_replay: clock crossed a second during an answer; retry {attempt}");
            }
            panic!("Both replay attempts crossed a clock boundary; rerun on an idle machine");
        })
        .unwrap()
        .join()
        .unwrap();
}

fn rating_index(rating: Rating) -> u8 {
    match rating {
        Rating::Again => 0,
        Rating::Hard => 1,
        Rating::Good => 2,
        Rating::Easy => 3,
    }
}

/// `QueuedCards.Queue` numbering.
fn queue_index(kind: QueueKind) -> i64 {
    match kind {
        QueueKind::New => 0,
        QueueKind::Learning => 1,
        QueueKind::Review => 2,
    }
}

fn now() -> Duration {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap()
}

/// Waits until the current wall-clock second has at least 400 ms left, so
/// the step that follows runs within one second on both sides.
fn early_in_second() {
    let ms = now().subsec_millis();
    if ms > 600 {
        std::thread::sleep(Duration::from_millis(u64::from(1000 - ms) + 5));
    }
}

struct Python {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Python {
    fn start(exe: &OsString, pythonpath: &OsString) -> Self {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/study-replay.py");
        let mut child = Process::new(exe)
            .arg("-B")
            .arg(script)
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env("PYTHONPATH", pythonpath)
            .env_remove("PYTHONHOME")
            .env_remove("PYTHONSTARTUP")
            .current_dir(std::env::temp_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
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
                "study-replay.py failed on {request}:\n{}",
                error.as_str().unwrap_or("")
            );
        }
        reply
    }

    fn quit(mut self) {
        let _ = writeln!(self.stdin.as_mut().unwrap(), "{}", json!({"cmd": "quit"}));
        self.stdin.take();
        let status = self.child.wait().unwrap();
        assert!(status.success(), "study-replay.py exited with {status}");
    }
}

impl Drop for Python {
    fn drop(&mut self) {
        // EOF makes Python close its collection and join pending backups,
        // including when a Rust assertion unwinds before the explicit quit.
        self.stdin.take();
        let _ = self.child.wait();
    }
}

/// Copies a closed profile's collection and media into a new temp folder.
fn copy_profile(src: &Path) -> tempfile::TempDir {
    let wal = src.join("collection.anki2-wal");
    assert!(
        !wal.exists() || std::fs::metadata(&wal).unwrap().len() == 0,
        "fixture must be closed cleanly before copying"
    );
    let dst = tempfile::tempdir().unwrap();
    std::fs::copy(
        src.join("collection.anki2"),
        dst.path().join("collection.anki2"),
    )
    .unwrap();
    let media = dst.path().join("collection.media");
    std::fs::create_dir(&media).unwrap();
    for entry in std::fs::read_dir(src.join("collection.media")).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), media.join(entry.file_name())).unwrap();
        }
    }
    dst
}

fn execute(actor: &CollectionActor, command: Command) -> Response {
    let text = format!("{command:?}");
    actor
        .execute(command)
        .unwrap_or_else(|e| panic!("{text}: {e}"))
}

#[derive(Debug, PartialEq)]
struct Step {
    card_id: i64,
    queue: i64,
    counts: [u32; 3],
}

fn run(python_exe: OsString, pythonpath: OsString) -> bool {
    let mut python = Python::start(&python_exe, &pythonpath);
    let version = python.call(json!({"cmd": "version"}));
    assert_eq!(version["version"], pinned_version());

    // 1. Build the fixture and close it.
    let fixture = tempfile::tempdir().unwrap();
    let actor = CollectionActor::open(fixture.path()).unwrap();
    let Response::CardIds(all_cards) = execute(&actor, Command::GenerateStudyFixture) else {
        panic!("GenerateStudyFixture")
    };
    actor.close().unwrap();
    let tanki_dir = copy_profile(fixture.path());
    let python_dir = copy_profile(fixture.path());
    let python_col = python_dir.path().join("collection.anki2");
    let tanki_col = tanki_dir.path().join("collection.anki2");
    let before = python.call(json!({"cmd": "dump", "path": tanki_col}));
    let revlog_before = before["revlog"].as_array().unwrap().len();

    // 2. Open both copies and select the top deck on both.
    let actor = CollectionActor::open(tanki_dir.path()).unwrap();
    python.call(json!({"cmd": "open", "path": python_col}));
    let Response::DeckTree(tree) = execute(&actor, Command::DeckTree) else {
        panic!("DeckTree")
    };
    let study = tree
        .children
        .iter()
        .find(|d| d.name == "Study")
        .expect("Study deck")
        .id;
    early_in_second();
    let Response::Overview(overview) = execute(&actor, Command::SelectDeck { deck_id: study })
    else {
        panic!("SelectDeck")
    };
    let selected = python.call(json!({"cmd": "select", "deck_id": study}));
    assert_eq!(
        selected["counts"],
        json!([overview.new, overview.learning, overview.review]),
        "overview counts"
    );

    // 3. Answer every card, in lock step.
    let base_millis = now().as_millis() as i64;
    let mut steps = Vec::new();
    let mut seen_queues = [0usize; 3];
    let mut seen_ratings = [0usize; 4];
    let mut leeches = 0;
    let mut type_answers = 0;
    for step in 0..MAX_STEPS {
        early_in_second();
        let second = now().as_secs();
        let Response::Queue(queue) = execute(&actor, Command::Queue { limit: 1 }) else {
            panic!("Queue")
        };
        let theirs = python.call(json!({"cmd": "queue"}));
        let Some(card) = queue.cards.first() else {
            assert_eq!(
                theirs["card_id"],
                Value::Null,
                "step {step}: tanki is done, Python is not"
            );
            assert_eq!(
                theirs["counts"],
                json!([queue.new, queue.learning, queue.review])
            );
            break;
        };
        let ours = Step {
            card_id: card.id,
            queue: queue_index(card.queue),
            counts: [queue.new, queue.learning, queue.review],
        };
        let python_step = Step {
            card_id: theirs["card_id"].as_i64().unwrap_or(-1),
            queue: theirs["queue"].as_i64().unwrap_or(-1),
            counts: serde_json::from_value(theirs["counts"].clone()).unwrap(),
        };
        assert_eq!(ours, python_step, "step {step}: tanki vs Python queue");

        // What the reviewer shows before the answer.
        let Response::Rendered(rendered) = execute(&actor, Command::Render { card_id: card.id })
        else {
            panic!("Render")
        };
        let Response::StudyConfig(_) = execute(&actor, Command::StudyConfig { card_id: card.id })
        else {
            panic!("StudyConfig")
        };
        let _question = rendered.review_question();
        if let Some(TypeAnswer::Input {
            expected,
            combining,
        }) = &rendered.type_answer
        {
            type_answers += 1;
            let Response::Html(comparison) = execute(
                &actor,
                Command::CompareAnswer {
                    expected: expected.clone(),
                    provided: "typed".into(),
                    combining: *combining,
                },
            ) else {
                panic!("CompareAnswer")
            };
            let _answer = rendered.review_answer(&comparison);
        }

        let rating = RATINGS[step % RATINGS.len()];
        let answered_at_millis = base_millis + step as i64 * 1500;
        let milliseconds_taken = ((step as u32) * 7919 + 1234) % 95_000;
        let Response::Answered(outcome) = execute(
            &actor,
            Command::Answer {
                card_id: card.id,
                rating,
                answered_at_millis,
                milliseconds_taken,
            },
        ) else {
            panic!("Answer")
        };
        let theirs = python.call(json!({
            "cmd": "answer",
            "card_id": card.id,
            "rating": rating_index(rating),
            "answered_at_millis": answered_at_millis,
            "milliseconds_taken": milliseconds_taken,
        }));
        if now().as_secs() != second {
            actor.close().unwrap();
            python.call(json!({"cmd": "close"}));
            python.quit();
            return false;
        }
        assert_eq!(
            (outcome.leech, outcome.suspended),
            (
                theirs["leech"].as_bool().unwrap(),
                theirs["suspended"].as_bool().unwrap()
            ),
            "step {step}: leech outcome"
        );
        leeches += usize::from(outcome.leech);
        seen_queues[ours.queue as usize] += 1;
        seen_ratings[rating_index(rating) as usize] += 1;
        steps.push(ours);
    }
    assert!(steps.len() < MAX_STEPS, "the session did not finish");
    actor.close().unwrap();
    python.call(json!({"cmd": "close"}));

    // 4. Compare the collections.
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

    // 5. The session covered what it should.
    let revlog_after = ours["revlog"].as_array().unwrap().len();
    assert_eq!(
        revlog_after - revlog_before,
        steps.len(),
        "one revlog row per answer"
    );
    assert_eq!(ours["cards"].as_array().unwrap().len(), all_cards.len());
    assert!(
        seen_queues.iter().all(|&n| n > 0),
        "queues seen: {seen_queues:?}"
    );
    assert!(
        seen_ratings.iter().all(|&n| n > 0),
        "ratings used: {seen_ratings:?}"
    );
    assert!(leeches > 0, "no leech in the session");
    assert!(type_answers > 0, "no type-in card in the session");
    println!(
        "study_replay: Anki {} | {} answers in lock step, identical queue order and counts at \
         every step | queues new/learning/review: {:?} | ratings again/hard/good/easy: {:?} | \
         leeches: {leeches} | type-in cards: {type_answers}",
        pinned_version(),
        steps.len(),
        seen_queues,
        seen_ratings,
    );
    println!(
        "study_replay: {} cards (all service fields), {} exported revlog rows (all scheduling \
         fields), {} notes: exactly equal; no clock tolerance or normalization",
        ours["cards"].as_array().unwrap().len(),
        revlog_after,
        ours["notes"].as_array().unwrap().len(),
    );
    true
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
