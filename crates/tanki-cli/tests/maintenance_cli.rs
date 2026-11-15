//! `tanki check ...` and `tanki profiles ...` through the built binary, on a
//! generated fixture with a fake HOME and a temp `--base` (PLAN 14 rule 1).
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
        .stdin(Stdio::null());
    command
}

fn fixture() -> tempfile::TempDir {
    let base = tempfile::tempdir().unwrap();
    let profile = base.path().join("tanki-fixture");
    std::fs::create_dir(&profile).unwrap();
    let actor = tanki_core::CollectionActor::open(&profile).unwrap();
    actor.execute(tanki_core::Command::GenerateFixture).unwrap();
    actor.close().unwrap();
    base
}

fn run(base: &Path, args: &[&str]) -> Output {
    let mut command = tanki(base);
    if args[0] == "check" {
        command.args(["--profile", "tanki-fixture"]);
    }
    command.args(args).output().unwrap()
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
fn fails(base: &Path, args: &[&str], code: i32) -> String {
    let output = run(base, args);
    assert_eq!(
        output.status.code(),
        Some(code),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8(output.stderr).unwrap()
}
fn backups(base: &Path) -> usize {
    std::fs::read_dir(base.join("tanki-fixture/backups"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".colpkg"))
        .count()
}

#[test]
fn check_db_needs_yes_then_backs_up_and_reports() {
    let base = fixture();
    let b = base.path();
    // The first open of an unseen collection writes its own backup.
    ok(b, &["check", "empty-cards"]);
    let before = backups(b);
    let err = fails(b, &["check", "db"], 24);
    assert!(err.contains("--yes"), "{err}");
    assert_eq!(backups(b), before, "a refused check makes no backup");
    let out = ok(b, &["check", "db", "--yes"]);
    assert!(out.contains("No problems found"), "{out}");
    assert!(out.contains("Backup written: backup-"), "{out}");
    let json: serde_json::Value =
        serde_json::from_str(&ok(b, &["--json", "check", "db", "--yes"])).unwrap();
    assert_eq!(json["problems"], serde_json::json!([]));
}

#[test]
fn check_media_refuses_without_yes_and_trashes_unused_on_request() {
    let base = fixture();
    let b = base.path();
    let media = b.join("tanki-fixture/collection.media");
    std::fs::write(media.join("stray.txt"), b"x").unwrap();
    let err = fails(b, &["check", "media"], 1);
    assert!(
        err.contains("rename media files") && err.contains("--yes"),
        "{err}"
    );
    assert!(media.join("stray.txt").exists());

    let out = ok(b, &["check", "media", "--yes"]);
    assert!(out.contains("Backup written"), "{out}");
    assert!(out.contains("--trash-unused"), "{out}");
    assert!(media.join("stray.txt").exists(), "listing never deletes");

    let json: serde_json::Value = serde_json::from_str(&ok(
        b,
        &["--json", "check", "media", "--yes", "--trash-unused"],
    ))
    .unwrap();
    assert!(json["unused"]
        .as_array()
        .unwrap()
        .contains(&"stray.txt".into()));
    assert_eq!(json["trashed"], 1);
    let trash = b.join("tanki-fixture/media.trash/stray.txt");
    assert!(!media.join("stray.txt").exists() && trash.exists());

    ok(b, &["check", "media", "--yes", "--restore-trash"]);
    assert!(media.join("stray.txt").exists());
    ok(
        b,
        &["check", "media", "--yes", "--trash-unused", "--empty-trash"],
    );
    assert!(!trash.exists() && !media.join("stray.txt").exists());
}

#[test]
fn check_empty_cards_reports_without_changing_anything() {
    let base = fixture();
    let b = base.path();
    assert!(ok(b, &["check", "empty-cards"]).contains("No empty cards"));
    let json: serde_json::Value =
        serde_json::from_str(&ok(b, &["--json", "check", "empty-cards"])).unwrap();
    assert_eq!(json["would_delete"], 0);
    assert_eq!(json["keep_notes"], true);
    let json: serde_json::Value = serde_json::from_str(&ok(
        b,
        &["--json", "check", "empty-cards", "--delete-notes", "--yes"],
    ))
    .unwrap();
    assert_eq!(json["keep_notes"], false);
    assert_eq!(json["deleted"], serde_json::Value::Null);
}

#[test]
fn profiles_list_create_rename_delete() {
    let base = fixture();
    let b = base.path();
    std::fs::write(b.join("prefs21.db"), b"keep me").unwrap();
    std::fs::create_dir(b.join("addons21")).unwrap();
    assert_eq!(ok(b, &["profiles", "list"]).trim(), "tanki-fixture");

    ok(b, &["profiles", "create", "second"]);
    assert!(b.join("second/collection.anki2").is_file());
    let err = fails(b, &["profiles", "create", "second"], 1);
    assert!(err.contains("already exists"), "{err}");
    let err = fails(b, &["profiles", "create", "../escape"], 1);
    assert!(err.contains("not a usable profile name"), "{err}");

    ok(b, &["profiles", "rename", "second", "third"]);
    assert!(!b.join("second").exists() && b.join("third").exists());
    let json: serde_json::Value =
        serde_json::from_str(&ok(b, &["--json", "profiles", "list"])).unwrap();
    let names: Vec<_> = json["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["tanki-fixture", "third"]);

    // Typed confirmation is required; nothing moves without it.
    let err = fails(b, &["profiles", "delete", "third"], 1);
    assert!(err.contains("--confirm third"), "{err}");
    let err = fails(b, &["profiles", "delete", "third", "--confirm", "thir"], 1);
    assert!(err.contains("type its name"), "{err}");
    assert!(b.join("third/collection.anki2").is_file());
    ok(b, &["profiles", "delete", "third", "--confirm", "third"]);
    assert!(!b.join("third").exists());
    assert_eq!(ok(b, &["profiles", "list"]).trim(), "tanki-fixture");
    assert!(std::fs::read_dir(b.join(".tanki-trash")).unwrap().count() == 1);

    // Desktop's files are never touched.
    assert_eq!(std::fs::read(b.join("prefs21.db")).unwrap(), b"keep me");
    assert!(std::fs::read_dir(b.join("addons21"))
        .unwrap()
        .next()
        .is_none());
}
