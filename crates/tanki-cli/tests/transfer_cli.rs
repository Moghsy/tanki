//! `tanki import` / `tanki export` for text and JSON files, through the
//! built binary with a fake HOME and a temp `--base`.
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn tanki(base: &Path) -> Command {
    let scratch = base.join("scratch");
    let mut command = Command::new(env!("CARGO_BIN_EXE_tanki"));
    command
        .env("HOME", scratch.join("home"))
        .env("XDG_CONFIG_HOME", scratch.join("config"))
        .env_remove("ANKI_BASE")
        .env_remove("XDG_DATA_HOME")
        .arg("--base")
        .arg(base)
        .args(["--profile", "tanki-fixture"])
        .stdin(Stdio::null());
    command
}

/// A profile holding an empty collection (stock note types only).
fn fixture() -> tempfile::TempDir {
    let base = tempfile::tempdir().unwrap();
    let profile = base.path().join("tanki-fixture");
    std::fs::create_dir(&profile).unwrap();
    tanki_core::CollectionActor::open(&profile)
        .unwrap()
        .close()
        .unwrap();
    base
}

fn run(base: &Path, args: &[&str]) -> Output {
    tanki(base).args(args).output().unwrap()
}

fn ok(base: &Path, args: &[&str]) -> String {
    let output = run(base, args);
    assert!(
        output.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn fails(base: &Path, args: &[&str]) -> String {
    let output = run(base, args);
    assert!(!output.status.success(), "{args:?} should fail");
    String::from_utf8(output.stderr).unwrap()
}

fn json(base: &Path, args: &[&str]) -> serde_json::Value {
    let mut full = vec!["--json"];
    full.extend_from_slice(args);
    serde_json::from_str(&ok(base, &full)).unwrap()
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

const TSV: &str = "#separator:tab\n#html:false\nbonjour\thello\tgreeting polite\nmerci\tthanks\t\n";

#[test]
fn text_preview_shows_settings_and_imports_nothing() {
    let base = fixture();
    let file = base.path().join("words.tsv");
    std::fs::write(&file, TSV).unwrap();
    let out = ok(
        base.path(),
        &["import", s(&file), "--notetype", "Basic", "--preview"],
    );
    assert!(out.contains("Delimiter: Tab"), "{out}");
    assert!(out.contains("Note type: Basic"), "{out}");
    assert!(out.contains("Front: column 1"), "{out}");
    assert!(out.contains("bonjour | hello | greeting polite"), "{out}");
    let listing = ok(base.path(), &["export", s(&base.path().join("n.txt"))]);
    assert!(listing.contains("Exported 0 notes"), "{listing}");
}

#[test]
fn text_import_maps_columns_and_handles_duplicates() {
    let base = fixture();
    let file = base.path().join("words.tsv");
    std::fs::write(&file, TSV).unwrap();
    let report = json(
        base.path(),
        &[
            "import",
            s(&file),
            "--notetype",
            "Basic",
            "--deck",
            "French",
            "--map",
            "Front=2,Back=1",
            "--tags-column",
            "3",
            "--global-tags",
            "imported",
        ],
    );
    assert_eq!(report["log"]["new"], 2);
    assert_eq!(report["log"]["found_notes"], 2);

    // The deck, mapping and tags are what was asked for.
    let out = base.path().join("out.txt");
    let text = ok(
        base.path(),
        &[
            "export",
            s(&out),
            "--deck",
            "French",
            "--with-tags",
            "--with-guid",
        ],
    );
    assert!(text.contains("Exported 2 notes"), "{text}");
    let written = std::fs::read_to_string(&out).unwrap();
    assert!(written.contains("hello\tbonjour"), "{written}");
    assert!(
        written.contains("imported") && written.contains("greeting"),
        "{written}"
    );

    // Keep current: nothing new; keep both adds notes.
    let kept = json(
        base.path(),
        &[
            "import",
            s(&file),
            "--notetype",
            "Basic",
            "--map",
            "Front=2,Back=1",
            "--dupes",
            "keep",
        ],
    );
    assert_eq!(kept["log"]["new"], 0);
    let both = json(
        base.path(),
        &[
            "import",
            s(&file),
            "--notetype",
            "Basic",
            "--map",
            "Front=2,Back=1",
            "--dupes",
            "add",
        ],
    );
    assert_eq!(both["log"]["found_notes"], 2);
    let all = ok(base.path(), &["export", s(&out), "--force"]);
    assert!(all.contains("Exported 4 notes"), "{all}");
}

#[test]
fn text_import_reports_bad_options() {
    let base = fixture();
    let file = base.path().join("words.tsv");
    std::fs::write(&file, TSV).unwrap();
    let f = s(&file);
    let err = fails(base.path(), &["import", f, "--notetype", "Nope"]);
    assert!(err.contains("No note type named"), "{err}");
    let err = fails(
        base.path(),
        &["import", f, "--notetype", "Basic", "--map", "Missing=1"],
    );
    assert!(err.contains("no field"), "{err}");
    let err = fails(
        base.path(),
        &["import", f, "--notetype", "Basic", "--map", "Front=9"],
    );
    assert!(err.contains("does not exist"), "{err}");
    let err = fails(base.path(), &["import", s(&base.path().join("gone.csv"))]);
    assert!(err.contains("No file"), "{err}");
}

#[test]
fn json_import_and_text_exports() {
    let base = fixture();
    let file = base.path().join("data.json");
    std::fs::write(
        &file,
        r#"{"notes":[{"fields":["Q one","A one"],"notetype":"Basic","deck":"Json"}]}"#,
    )
    .unwrap();
    let report = json(base.path(), &["import", s(&file)]);
    assert_eq!(report["log"]["new"], 1);

    let notes = base.path().join("notes.tsv");
    let out = ok(
        base.path(),
        &[
            "export",
            s(&notes),
            "--query",
            "deck:Json",
            "--with-deck-column",
            "--with-notetype",
        ],
    );
    assert!(out.contains("Exported 1 notes"), "{out}");
    let text = std::fs::read_to_string(&notes).unwrap();
    assert!(
        text.contains("#deck column:") && text.contains("#notetype column:"),
        "{text}"
    );

    let cards = base.path().join("cards.txt");
    let out = ok(
        base.path(),
        &["export", s(&cards), "--cards", "--deck", "Json"],
    );
    assert!(out.contains("Exported 1 cards"), "{out}");
    assert!(std::fs::read_to_string(&cards).unwrap().contains("Q one"));

    let err = fails(base.path(), &["export", s(&cards), "--cards"]);
    assert!(err.contains("--force"), "{err}");
    let err = fails(
        base.path(),
        &["export", s(&cards), "--force", "--cards", "--with-guid"],
    );
    assert!(err.contains("Card text"), "{err}");
    let err = fails(
        base.path(),
        &["export", s(&cards), "--force", "--with-scheduling"],
    );
    assert!(err.contains("text export"), "{err}");
    let err = fails(
        base.path(),
        &["export", s(&cards), "--force", "--query", "nothingmatches"],
    );
    assert!(err.contains("Nothing matches"), "{err}");
}
