//! `tanki add / edit / search / delete` through the built binary, on a
//! generated fixture collection with a fake HOME (PLAN 9, Phase 3 exit test).
use std::io::Write;
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
        .env_remove("VISUAL")
        .env_remove("EDITOR")
        .arg("--base")
        .arg(base)
        .args(["--profile", "tanki-fixture"])
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
fn with_stdin(base: &Path, args: &[&str], input: &str) -> Output {
    let mut child = tanki(base)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn add_basic(base: &Path, front: &str, back: &str) -> i64 {
    ok(
        base,
        &[
            "add",
            "--type",
            "Basic",
            "--field",
            &format!("Front={front}"),
            "--field",
            &format!("Back={back}"),
            "--tag",
            "t1",
        ],
    )
    .trim()
    .parse()
    .unwrap()
}

fn stored(base: &Path, id: i64) -> tanki_core::NoteData {
    let actor = tanki_core::CollectionActor::open(base.join("tanki-fixture")).unwrap();
    let response = actor
        .execute(tanki_core::Command::Notes(
            tanki_core::NoteCommand::GetNote { id },
        ))
        .unwrap();
    actor.close().unwrap();
    let tanki_core::Response::Edit(tanki_core::EditResponse::Note(note)) = response else {
        panic!("not a note")
    };
    note
}

fn note_file(id: i64, mtime: u32, front: &str, back: &str) -> String {
    format!("# Note\nid: {id}\nmtime: {mtime}\nnotetype: Basic\ntags: t1\n\n## Front\n{front}\n\n## Back\n{back}\n")
}

#[test]
fn add_with_flags_stores_html_verbatim_and_prints_the_id() {
    let base = fixture();
    let id = add_basic(
        base.path(),
        "Paris is <b>the</b> capital",
        "France &amp; more",
    );
    let note = stored(base.path(), id);
    assert_eq!(
        note.fields,
        ["Paris is <b>the</b> capital", "France &amp; more"]
    );
    assert_eq!(note.tags, ["t1"]);
    let json = ok(
        base.path(),
        &["--json", "add", "--field", "Front=a", "--field", "Back=b"],
    );
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(value["id"].as_i64().unwrap() > 0);
}

#[test]
fn add_refusals_exit_non_zero_with_a_message() {
    let base = fixture();
    let b = base.path();
    let err = fails(
        b,
        &[
            "add", "--type", "Basic", "--field", "Front=", "--field", "Back=x",
        ],
        1,
    );
    assert!(err.contains("first field is empty"), "{err}");
    let err = fails(b, &["add", "--type", "Basic", "--field", "Nope=x"], 1);
    assert!(err.contains("no field"), "{err}");
    let err = fails(
        b,
        &["add", "--type", "Nonexistent", "--field", "Front=x"],
        1,
    );
    assert!(err.contains("No note type"), "{err}");
    let err = fails(
        b,
        &["add", "--deck", "No such deck", "--field", "Front=x"],
        1,
    );
    assert!(err.contains("No deck"), "{err}");
    let err = fails(
        b,
        &[
            "add",
            "--type",
            "Basic",
            "--field",
            "Front=a {{c1::b}}",
            "--field",
            "Back=x",
        ],
        1,
    );
    assert!(err.contains("Cloze"), "{err}");
    let err = fails(
        b,
        &["add", "--type", "Cloze", "--field", "Text=no cloze here"],
        1,
    );
    assert!(err.contains("--force"), "{err}");
    // --force adds it anyway.
    ok(
        b,
        &[
            "add",
            "--type",
            "Cloze",
            "--field",
            "Text=no cloze here",
            "--force",
        ],
    );
    // A duplicate first field is only a warning.
    add_basic(b, "dup", "1");
    let output = run(
        b,
        &[
            "add",
            "--type",
            "Basic",
            "--field",
            "Front=dup",
            "--field",
            "Back=2",
        ],
    );
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("warning"));
}

#[test]
fn add_from_file_and_stdin_allows_several_notes() {
    let base = fixture();
    let b = base.path();
    let text = "# Note\nnotetype: Basic\ntags: x y\n\n## Front\nQ1\n\n## Back\nA1\n\n# Note\nnotetype: Basic\n\n## Front\nQ2\n\n## Back\nA2\n";
    let path = b.join("in.md");
    std::fs::write(&path, text).unwrap();
    let out = ok(b, &["add", "--file", path.to_str().unwrap()]);
    let ids: Vec<i64> = out.lines().map(|l| l.parse().unwrap()).collect();
    assert_eq!(ids.len(), 2);
    assert_eq!(stored(b, ids[0]).tags, ["x", "y"]);
    // Stdin, JSON array.
    let output = with_stdin(b, &["--json", "add", "--file", "-"], text);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value.as_array().unwrap().len(), 2);
    // A problem refuses everything.
    let bad = "# Note\nnotetype: Basic\n\n## Front\nok\n\n## Nope\nx\n";
    let output = with_stdin(b, &["add", "--file", "-"], bad);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("line"));
}

#[test]
fn add_edit_uses_the_editor_on_a_template() {
    let base = fixture();
    let b = base.path();
    let script = b.join("fill.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nsed -i 's/^## Front$/&\\nfrom editor/' \"$1\"\n",
    )
    .unwrap();
    make_executable(&script);
    let output = tanki(b)
        .env("VISUAL", &script)
        .args(["add", "--edit", "--type", "Basic"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let id: i64 = String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(stored(b, id).fields[0], "from editor");
}

fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn edit_file_changes_only_the_changed_field() {
    let base = fixture();
    let b = base.path();
    let id = add_basic(b, "front", "back");
    let note = stored(b, id);
    let path = b.join("e.md");
    std::fs::write(&path, note_file(id, note.mtime_secs, "front", "back2")).unwrap();
    // mtime has one-second resolution: make the edit land in a later second.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let out = ok(b, &["--json", "edit", "--file", path.to_str().unwrap()]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["updated"], serde_json::json!([id]));
    assert_eq!(stored(b, id).fields, ["front", "back2"]);
    // A file read before that change is stale: conflict, nothing written.
    std::fs::write(&path, note_file(id, note.mtime_secs, "front", "stale")).unwrap();
    let err = fails(
        b,
        &["edit", &id.to_string(), "--file", path.to_str().unwrap()],
        26,
    );
    assert!(err.contains(&id.to_string()), "{err}");
    assert_eq!(stored(b, id).fields, ["front", "back2"]);
    // Unchanged content prints no changes.
    let current = stored(b, id);
    std::fs::write(&path, note_file(id, current.mtime_secs, "front", "back2")).unwrap();
    assert!(ok(b, &["edit", "--file", path.to_str().unwrap()]).contains("No changes"));
    // Stdin.
    let output = with_stdin(
        b,
        &["edit", "--file", "-"],
        &note_file(id, current.mtime_secs, "front", "back3"),
    );
    assert!(output.status.success());
    assert_eq!(stored(b, id).fields[1], "back3");
}

#[test]
fn edit_with_a_scripted_editor_updates_the_note() {
    let base = fixture();
    let b = base.path();
    let id = add_basic(b, "front", "back");
    let script = b.join("ed.sh");
    std::fs::write(&script, "#!/bin/sh\nsed -i 's/^back$/edited/' \"$1\"\n").unwrap();
    make_executable(&script);
    let output = tanki(b)
        .env("VISUAL", &script)
        .args(["edit", &id.to_string()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stored(b, id).fields, ["front", "edited"]);
    // Failing editor aborts.
    let fail = b.join("fail.sh");
    std::fs::write(&fail, "#!/bin/sh\nexit 1\n").unwrap();
    make_executable(&fail);
    let output = tanki(b)
        .env("VISUAL", &fail)
        .args(["edit", &id.to_string()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stored(b, id).fields, ["front", "edited"]);
}

#[test]
fn edit_refuses_missing_ids() {
    let base = fixture();
    let err = fails(base.path(), &["edit", "999"], 1);
    assert!(!err.is_empty());
}

#[test]
fn delete_by_id_needs_yes_when_not_interactive() {
    let base = fixture();
    let b = base.path();
    let id = add_basic(b, "gone", "x");
    let err = fails(b, &["delete", &id.to_string()], 1);
    assert!(err.contains("--yes"), "{err}");
    let out = ok(b, &["--json", "delete", "--yes", &id.to_string()]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["deleted"], 1);
    fails(b, &["delete", "--yes", &id.to_string()], 1);
}

// ---- tests that need search (Browser stream) ---------------------------------

#[test]
fn search_formats_sort_limit_and_errors() {
    let base = fixture();
    let b = base.path();
    let a = add_basic(b, "alpha", "1");
    let z = add_basic(b, "zulu", "2");
    let ids = ok(
        b,
        &[
            "search", "tag:t1", "--notes", "--format", "ids", "--sort", "noteFld",
        ],
    );
    assert_eq!(
        ids.lines().collect::<Vec<_>>(),
        [a.to_string(), z.to_string()]
    );
    let ids = ok(
        b,
        &[
            "search",
            "tag:t1",
            "--notes",
            "--format",
            "ids",
            "--sort",
            "noteFld",
            "--reverse",
            "--limit",
            "1",
        ],
    );
    assert_eq!(ids.trim(), z.to_string());
    let json = ok(
        b,
        &[
            "search",
            "tag:t1",
            "--notes",
            "--columns",
            "noteFld,tags",
            "--format",
            "json",
        ],
    );
    let rows: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 2);
    assert!(rows[0]["noteFld"].is_string() && rows[0]["id"].is_i64());
    let via_global = ok(b, &["--json", "search", "tag:t1", "--notes"]);
    assert!(serde_json::from_str::<serde_json::Value>(&via_global)
        .unwrap()
        .is_array());
    let table = ok(b, &["search", "tag:t1", "--notes", "--columns", "noteFld"]);
    assert!(table.lines().next().unwrap().starts_with("id"));
    assert_eq!(table.lines().count(), 3);
    let err = fails(b, &["search", "\"unterminated"], 1);
    assert!(!err.is_empty());
    fails(b, &["search", "x", "--sort", "nonsense"], 1);
}

#[test]
fn edit_and_delete_by_query() {
    let base = fixture();
    let b = base.path();
    let a = add_basic(b, "qa", "1");
    let z = add_basic(b, "qb", "2");
    let script = b.join("ed.sh");
    std::fs::write(&script, "#!/bin/sh\nsed -i 's/^1$/one/' \"$1\"\n").unwrap();
    make_executable(&script);
    let output = tanki(b)
        .env("VISUAL", &script)
        .args(["edit", "tag:t1"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stored(b, a).fields[1], "one");
    assert_eq!(stored(b, z).fields[1], "2");
    fails(b, &["edit", "tag:nomatch"], 1);
    // More than one note needs --yes; a query matching nothing is refused.
    fails(b, &["delete", "tag:t1"], 1);
    fails(b, &["delete", "--yes", "tag:nomatch"], 1);
    let out = ok(b, &["delete", "--yes", "tag:t1"]);
    assert!(out.contains("Deleted 2"));
}

/// Phase 3 exit test, second half: add -> search -> edit -> delete through
/// the CLI only.
#[test]
fn exit_add_search_edit_delete() {
    let base = fixture();
    let b = base.path();
    let id = add_basic(b, "Paris <i>is</i>  a city", "  France\n&nbsp;");
    let found = ok(b, &["search", "tag:t1", "--notes", "--format", "ids"]);
    assert_eq!(found.trim(), id.to_string());
    let before: serde_json::Value = serde_json::from_str(&ok(
        b,
        &[
            "search",
            &format!("nid:{id}"),
            "--notes",
            "--columns",
            "noteFld",
            "--format",
            "json",
        ],
    ))
    .unwrap();
    let note = stored(b, id);
    let path = b.join("e.md");
    let back = "changed <b>back</b>";
    // Keep Front byte-identical by quoting it.
    let front = serde_json::to_string(&note.fields[0]).unwrap();
    std::fs::write(
        &path,
        format!(
            "# Note\nid: {id}\nmtime: {}\nnotetype: Basic\ntags: t1\n\n## Front {{quoted}}\n{front}\n\n## Back\n{back}\n",
            note.mtime_secs
        ),
    )
    .unwrap();
    ok(b, &["edit", "--file", path.to_str().unwrap()]);
    let after = stored(b, id);
    assert_eq!(after.fields[0], note.fields[0]);
    assert_eq!(after.fields[1], back);
    let json: serde_json::Value = serde_json::from_str(&ok(
        b,
        &[
            "search",
            &format!("nid:{id}"),
            "--notes",
            "--format",
            "json",
        ],
    ))
    .unwrap();
    assert_eq!(json.as_array().unwrap().len(), 1);
    assert_eq!(json[0]["id"], id);
    let after_cols: serde_json::Value = serde_json::from_str(&ok(
        b,
        &[
            "search",
            &format!("nid:{id}"),
            "--notes",
            "--columns",
            "noteFld",
            "--format",
            "json",
        ],
    ))
    .unwrap();
    assert_eq!(before[0]["noteFld"], after_cols[0]["noteFld"]);
    ok(b, &["delete", "--yes", &id.to_string()]);
    assert_eq!(
        ok(b, &["search", "tag:t1", "--notes", "--format", "ids"]).trim(),
        ""
    );
}
