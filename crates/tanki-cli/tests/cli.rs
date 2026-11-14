use std::process::Command;

#[test]
fn version_has_unofficial_disclaimer() {
    let output = tanki().arg("--version").output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("not affiliated with or endorsed by Ankitects"));
}
#[test]
fn fixture_dump_requires_explicit_safe_base() {
    let missing = tanki().arg("fixture-dump").output().unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8(missing.stderr)
        .unwrap()
        .contains("explicit --base"));
    let denied = tanki()
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
    let output = tanki()
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
    let mut command = tanki();
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

fn generated_fixture() -> tempfile::TempDir {
    let base = tempfile::tempdir().unwrap();
    let profile = base.path().join("tanki-fixture");
    std::fs::create_dir(&profile).unwrap();
    let actor = tanki_core::CollectionActor::open(&profile).unwrap();
    actor.execute(tanki_core::Command::GenerateFixture).unwrap();
    actor.close().unwrap();
    base
}

fn fixture_command(base: &std::path::Path) -> Command {
    let mut command = tanki();
    command
        .arg("--base")
        .arg(base)
        .env_remove("TANKI_ALLOW_REAL_PROFILE")
        .env_remove("ANKI_BASE")
        .env_remove("XDG_DATA_HOME");
    command
}

fn assert_closed(base: &std::path::Path) {
    let profile = base.join("tanki-fixture");
    let actor = tanki_core::CollectionActor::open(&profile)
        .expect("CLI must release its actor and backup worker");
    actor.close().unwrap();
    assert!(std::fs::read_dir(profile.join("backups"))
        .unwrap()
        .next()
        .is_some());
}

#[test]
fn decks_fixture_json_and_table_have_counts_and_close_actor() {
    let base = generated_fixture();
    let json = fixture_command(base.path())
        .args(["decks", "--json"])
        .output()
        .unwrap();
    assert!(
        json.status.success(),
        "{}",
        String::from_utf8_lossy(&json.stderr)
    );
    let tree: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let children = tree["children"].as_array().unwrap();
    assert!(children.iter().any(|deck| deck["name"] == "Default"));
    assert!(children.iter().all(|deck| deck["new"].is_number()
        && deck["learning"].is_number()
        && deck["review"].is_number()));
    assert_closed(base.path());
    let table = fixture_command(base.path()).arg("decks").output().unwrap();
    assert!(
        table.status.success(),
        "{}",
        String::from_utf8_lossy(&table.stderr)
    );
    let text = String::from_utf8(table.stdout).unwrap();
    assert!(text.lines().next().unwrap().contains("New   Learn     Due"));
    assert!(text.contains("Default"));
    assert_closed(base.path());
}

#[test]
fn review_non_tty_and_missing_deck_errors_still_close_and_backup() {
    let base = generated_fixture();
    let output = fixture_command(base.path()).arg("review").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires a terminal"));
    assert_closed(base.path());
    let output = fixture_command(base.path())
        .args(["review", "missing::deck"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("No deck named"));
    assert_closed(base.path());
    let output = fixture_command(base.path()).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires a terminal"));
    assert_closed(base.path());
}

#[test]
fn study_commands_require_safe_base_and_profile_names() {
    for command in ["review", "decks"] {
        let missing = tanki().arg(command).output().unwrap();
        assert!(!missing.status.success());
        assert!(String::from_utf8_lossy(&missing.stderr).contains("explicit --base"));
        let base = tempfile::tempdir().unwrap();
        let invalid = fixture_command(base.path())
            .args(["--profile", "../escape", command])
            .output()
            .unwrap();
        assert!(!invalid.status.success());
        assert!(String::from_utf8_lossy(&invalid.stderr).contains("single folder name"));
        assert!(std::fs::read_dir(base.path()).unwrap().next().is_none());
    }
}

#[test]
fn invalid_keymap_fails_before_collection_open() {
    let base = tempfile::tempdir().unwrap();
    let keymap = base.path().join("keys.json");
    std::fs::write(&keymap, r#"[{"scope":"review","key":"x","action":"fly"}]"#).unwrap();
    let output = fixture_command(base.path())
        .arg("--keymap")
        .arg(keymap)
        .arg("review")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown keymap action"));
    assert!(!base.path().join("tanki-fixture").exists());
}

#[test]
fn leftover_wal_exits_25_until_confirmed_closed() {
    let base = generated_fixture();
    let wal = base.path().join("tanki-fixture/collection.anki2-wal");
    std::fs::write(&wal, b"").unwrap();
    let refused = fixture_command(base.path()).arg("decks").output().unwrap();
    assert_eq!(refused.status.code(), Some(25));
    assert!(String::from_utf8(refused.stderr)
        .unwrap()
        .contains("--confirm-closed"));
    let confirmed = fixture_command(base.path())
        .args(["decks", "--confirm-closed"])
        .output()
        .unwrap();
    assert!(
        confirmed.status.success(),
        "{}",
        String::from_utf8_lossy(&confirmed.stderr)
    );
    assert!(!wal.exists(), "a clean close removes the WAL");
}

/// The binary under test, with its config folder kept out of the user's
/// home (it records every collection it opens).
fn tanki() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tanki"));
    command.env(
        "XDG_CONFIG_HOME",
        std::env::temp_dir().join(format!("tanki-cli-test-config-{}", std::process::id())),
    );
    command
}

/// PLAN 4.4: the first open of an unseen collection forces a backup; later
/// opens follow Anki's schedule (30-minute default interval), so a burst of
/// CLI commands does not write a backup each.
#[test]
fn only_the_first_open_of_a_collection_forces_a_backup() {
    let base = generated_fixture();
    let config = base.path().join("config.toml");
    let backups = || {
        std::fs::read_dir(base.path().join("tanki-fixture/backups"))
            .unwrap()
            .count()
    };
    let decks = || {
        let output = fixture_command(base.path())
            .arg("--config")
            .arg(&config)
            .arg("decks")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    let before = backups();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    decks();
    let first = backups();
    assert_eq!(first, before + 1, "unseen collection: forced backup");
    assert!(std::fs::read_to_string(&config)
        .unwrap()
        .contains("collection.anki2"));
    for _ in 0..3 {
        decks();
    }
    assert_eq!(backups(), first, "seen collection: Anki's interval applies");
}
