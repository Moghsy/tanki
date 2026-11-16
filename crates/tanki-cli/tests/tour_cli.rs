use serde_json::Value;
use std::{
    path::Path,
    process::{Command, Output},
};
fn run(base: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tanki"))
        .env("HOME", base.join("home"))
        .env("XDG_CONFIG_HOME", base.join("config"))
        .env_remove("ANKI_BASE")
        .env_remove("XDG_DATA_HOME")
        .args([
            "--base",
            base.to_str().unwrap(),
            "--profile",
            "synthetic",
            "--config",
            base.join("settings.toml").to_str().unwrap(),
            "--json",
        ])
        .args(args)
        .output()
        .unwrap()
}
fn ok(base: &Path, args: &[&str]) -> Value {
    let output = run(base, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn setup(base: &Path) {
    let program = base.join("bridge.py");
    std::fs::write(&program, r#"import json, sys
args=sys.argv[1:]
if args == ['capabilities']: print(json.dumps(dict(protocol=1, standalone=True, inspect=True, no_focus=True,lazy_agent=True,new_editor=True)))
elif args == ['--list']: print('[]')
elif args[:2] == ['saved','list']: print('{"tours":[],"issues":[]}')
elif args[:2] == ['saved','inspect']: print(json.dumps(dict(id=args[2],title='Test',revision='a'*64)))
else: raise Exception(args)
"#).unwrap();
    std::fs::write(
        base.join("settings.toml"),
        format!(
            "[code_tour]\nenabled = true\ncommand = [\"python3\", {:?}]\n",
            program.to_str().unwrap()
        ),
    )
    .unwrap();
}
#[test]
fn local_commands_do_not_open_collection_and_authoring_preserves_fields() {
    let base = tempfile::tempdir().unwrap();
    let base = base.path();
    setup(base);
    assert_eq!(ok(base, &["tour", "list"])["tours"], serde_json::json!([]));
    assert_eq!(ok(base, &["tour", "doctor"])["capabilities"]["protocol"], 1);
    assert!(!base.join("synthetic").exists());
    std::fs::create_dir(base.join("synthetic")).unwrap();
    let actor = tanki_core::CollectionActor::open(base.join("synthetic")).unwrap();
    actor.close().unwrap();
    let nt = ok(base, &["tour", "install-note-type"])["notetype_id"]
        .as_i64()
        .unwrap();
    assert_eq!(ok(base, &["tour", "install-note-type"])["notetype_id"], nt);
    let id = "0123456789abcdef0123456789abcdef";
    let note = ok(
        base,
        &[
            "tour",
            "add",
            "--tour",
            id,
            "--prompt",
            "Explain <rating>",
            "--answer",
            "Use & schedule",
        ],
    )["note_id"]
        .as_i64()
        .unwrap();
    let duplicate = run(
        base,
        &[
            "tour",
            "add",
            "--tour",
            id,
            "--prompt",
            "Explain <rating>",
            "--answer",
            "Other",
        ],
    );
    assert!(!duplicate.status.success());
    let id2 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    ok(
        base,
        &["tour", "attach", "--note", &note.to_string(), "--tour", id2],
    );
    let check = ok(base, &["tour", "check", "--query", &format!("nid:{note}")]);
    assert_eq!(check[0]["result"]["status"], "available");
    let actor = tanki_core::CollectionActor::open(base.join("synthetic")).unwrap();
    let tanki_core::Response::Edit(tanki_core::EditResponse::Note(stored)) = actor
        .execute(tanki_core::Command::Notes(
            tanki_core::NoteCommand::GetNote { id: note },
        ))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        stored.fields,
        [
            "Explain &lt;rating&gt;",
            "Use &amp; schedule",
            &format!("code-tour://saved/{id2}")
        ]
    );
    actor.close().unwrap();
}
