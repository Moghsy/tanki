//! `tanki completions` and `tanki man-pages` (PLAN 11, Phase 6 packaging).
//! Neither opens a collection; they run with a fake HOME anyway (rule 1).
use std::process::{Command, Output, Stdio};

fn tanki(args: &[&str]) -> Output {
    let scratch = tempfile::tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_tanki"))
        .env("HOME", scratch.path().join("home"))
        .env("XDG_CONFIG_HOME", scratch.path().join("config"))
        .env_remove("ANKI_BASE")
        .env_remove("XDG_DATA_HOME")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn completions_for_every_shell_name_the_subcommands() {
    for shell in ["bash", "zsh", "fish", "elvish", "powershell"] {
        let output = tanki(&["completions", shell]);
        assert!(output.status.success(), "{shell}: {output:?}");
        let script = String::from_utf8(output.stdout).unwrap();
        for sub in ["review", "sync", "serve", "completions"] {
            assert!(script.contains(sub), "{shell} script lacks {sub}");
        }
    }
}

#[test]
fn unknown_shell_is_refused() {
    assert!(!tanki(&["completions", "tcsh"]).status.success());
}

#[test]
fn man_pages_cover_visible_subcommands() {
    let out = tempfile::tempdir().unwrap();
    let dir = out.path().join("man1");
    let output = tanki(&["man-pages", dir.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    for page in [
        "tanki.1",
        "tanki-review.1",
        "tanki-serve.1",
        "tanki-sync-login.1",
        "tanki-check-db.1",
    ] {
        let text = std::fs::read_to_string(dir.join(page)).unwrap();
        assert!(text.starts_with(".ie"), "{page} is not roff");
    }
    let main = std::fs::read_to_string(dir.join("tanki.1")).unwrap();
    assert!(main.contains("Ankitects"), "the disclaimer is in tanki(1)");
    assert!(!dir.join("tanki-fixture-dump.1").exists());
    assert!(!dir.join("tanki-man-pages.1").exists());
    assert!(!dir.join("tanki-help.1").exists());
}
