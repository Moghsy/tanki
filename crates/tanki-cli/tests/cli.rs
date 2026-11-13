use std::process::Command;

#[test]
fn version_has_unofficial_disclaimer() {
    let output = Command::new(env!("CARGO_BIN_EXE_tanki"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("not affiliated with or endorsed by Ankitects"));
}
#[test]
fn fixture_dump_requires_explicit_safe_base() {
    let missing = Command::new(env!("CARGO_BIN_EXE_tanki"))
        .arg("fixture-dump")
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8(missing.stderr)
        .unwrap()
        .contains("explicit --base"));
    let denied = Command::new(env!("CARGO_BIN_EXE_tanki"))
        .args(["--base", env!("CARGO_MANIFEST_DIR"), "fixture-dump"])
        .env_remove("TANKI_ALLOW_REAL_PROFILE")
        .output()
        .unwrap();
    assert!(!denied.status.success());
    assert!(String::from_utf8(denied.stderr)
        .unwrap()
        .contains("temporary or fixture"));
}

#[test]
fn doctor_non_tty_has_no_escape_sequences() {
    let output = Command::new(env!("CARGO_BIN_EXE_tanki"))
        .args(["--graphics=kitty", "doctor"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!output.stdout.contains(&0x1b));
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("skipped"));
}

fn fixture_dump(base: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tanki"));
    command
        .arg("--base")
        .arg(base)
        .arg("fixture-dump")
        .env_remove("TANKI_ALLOW_REAL_PROFILE")
        .env_remove("ANKI_BASE")
        .env_remove("XDG_DATA_HOME");
    command
}

#[test]
fn guard_allows_non_temp_base_only_with_env() {
    // target/tmp is outside the system temp folder, so the guard applies.
    let base = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let denied = fixture_dump(base.path()).output().unwrap();
    assert!(!denied.status.success());
    assert!(String::from_utf8(denied.stderr)
        .unwrap()
        .contains("temporary or fixture"));
    assert!(std::fs::read_dir(base.path()).unwrap().next().is_none());
    std::fs::create_dir(base.path().join("tanki-fixture")).unwrap();
    let allowed = fixture_dump(base.path())
        .env("TANKI_ALLOW_REAL_PROFILE", "1")
        .output()
        .unwrap();
    assert!(
        allowed.status.success(),
        "{}",
        String::from_utf8_lossy(&allowed.stderr)
    );
    assert!(base.path().join("tanki-fixture/collection.anki2").exists());
}

#[test]
fn real_anki_bases_are_refused_even_with_env() {
    // A fake HOME inside a temp folder stands in for the real one; the real
    // profile is never named or touched.
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let cases = [
        (home.join(".local/share/Anki2"), None),
        (home.join(".var/app/net.ankiweb.Anki/data/Anki2"), None),
        (
            root.path().join("xdg/Anki2"),
            Some(("XDG_DATA_HOME", root.path().join("xdg"))),
        ),
        (
            root.path().join("custom"),
            Some(("ANKI_BASE", root.path().join("custom"))),
        ),
    ];
    for (base, env) in cases {
        std::fs::create_dir_all(&base).unwrap();
        let mut command = fixture_dump(&base);
        command
            .env("HOME", &home)
            .env("TANKI_ALLOW_REAL_PROFILE", "1");
        if let Some((name, value)) = env {
            command.env(name, value);
        }
        let output = command.output().unwrap();
        assert!(!output.status.success(), "{}", base.display());
        assert!(String::from_utf8(output.stderr)
            .unwrap()
            .contains("real Anki base"));
        assert!(std::fs::read_dir(&base).unwrap().next().is_none());
    }
}

#[test]
fn second_process_on_open_profile_exits_with_locked_code() {
    let base = tempfile::tempdir().unwrap();
    let profile = base.path().join("tanki-fixture");
    std::fs::create_dir(&profile).unwrap();
    let actor = tanki_core::CollectionActor::open(&profile).unwrap();
    let output = fixture_dump(base.path()).output().unwrap();
    assert_eq!(output.status.code(), Some(20));
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("This profile is open in Anki (or another tanki)"));
    actor.close().unwrap();
    assert!(fixture_dump(base.path()).output().unwrap().status.success());
}
