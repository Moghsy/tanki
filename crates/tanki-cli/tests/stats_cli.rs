//! `tanki stats` through the built binary, on the generated study fixture
//! (as `cargo xtask study-fixtures` writes it) with a fake HOME.
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

/// The study fixture: decks under "Study" with reviews today, review cards
/// with intervals and cards due in the future.
fn fixture() -> tempfile::TempDir {
    let base = tempfile::tempdir().unwrap();
    let profile = base.path().join("tanki-fixture");
    std::fs::create_dir(&profile).unwrap();
    let actor = tanki_core::CollectionActor::open(&profile).unwrap();
    actor
        .execute(tanki_core::Command::GenerateStudyFixture)
        .unwrap();
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

fn json(base: &Path, args: &[&str]) -> serde_json::Value {
    serde_json::from_str(&ok(base, args)).unwrap()
}

fn card_total(counts: &serde_json::Value) -> u64 {
    ["new", "learn", "relearn", "young", "mature"]
        .iter()
        .map(|k| counts[k].as_u64().unwrap())
        .sum()
}

#[test]
fn text_lists_every_panel() {
    let base = fixture();
    let text = ok(base.path(), &["stats"]);
    for title in [
        "Today",
        "Future Due",
        "Calendar",
        "Reviews",
        "Card Counts",
        "Review Intervals",
        "Card Ease",
        "Retention",
        "Hourly Breakdown",
        "Answer Buttons",
        "Added",
    ] {
        assert!(text.contains(title), "{title} missing:\n{text}");
    }
    // The whole collection, the last 12 months.
    assert!(text.lines().next().unwrap().starts_with("Statistics · "));
    assert!(text.contains("Studied "), "{text}");
    assert!(!text.contains('\x1b'), "no escape sequences when piped");
    let deck = ok(base.path(), &["stats", "--deck", "Study::Science"]);
    assert!(
        deck.lines().next().unwrap().contains("deck:Study::Science"),
        "{deck}"
    );
    assert_ne!(deck, text);
}

#[test]
fn json_holds_aggregates_and_panels() {
    let base = fixture();
    let all = json(base.path(), &["stats", "--format", "json"]);
    assert_eq!(all["data"]["search"], "");
    assert_eq!(all["data"]["history"], "Year");
    assert!(all["data"]["today"]["answer_count"].as_u64().unwrap() > 0);
    let total = card_total(&all["data"]["counts_including_inactive"]);
    assert!(total > 0);
    assert_eq!(all["options"]["intervals_range"], "Percentile95");
    let panels = &all["panels"];
    for key in [
        "today",
        "future_due",
        "calendar",
        "reviews",
        "card_counts",
        "intervals",
        "ease",
        "true_retention",
        "hours",
        "buttons",
        "added",
    ] {
        assert!(panels.get(key).is_some_and(|p| !p.is_null()), "{key}");
    }
    // FSRS is off in the fixture: no FSRS panels.
    assert!(panels["difficulty"].is_null());

    // The global --json flag means the same; a deck narrows the cards and
    // all history widens the review-log window.
    let deck = json(
        base.path(),
        &[
            "--json",
            "stats",
            "--deck",
            "Study::Science",
            "--all-history",
        ],
    );
    assert_eq!(deck["data"]["search"], "deck:Study::Science");
    assert_eq!(deck["data"]["history"], "All");
    assert_eq!(deck["options"]["reviews_range"], "AllTime");
    let deck_total = card_total(&deck["data"]["counts_including_inactive"]);
    assert!(deck_total > 0 && deck_total < total);

    let new = json(
        base.path(),
        &["stats", "--format=json", "--search", "is:new"],
    );
    assert_eq!(new["data"]["search"], "is:new");
    let counts = &new["data"]["counts_including_inactive"];
    assert_eq!(counts["young"], 0);
    assert_eq!(counts["mature"], 0);
    assert!(counts["new"].as_u64().unwrap() > 0);
}

#[test]
fn svg_is_a_document() {
    let base = fixture();
    let svg = ok(base.path(), &["stats", "--format", "svg"]);
    assert!(svg.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg "));
    assert!(svg.trim_end().ends_with("</svg>"));
    assert_eq!(svg.matches("<text").count(), svg.matches("</text>").count());
    assert!(svg.contains(">Future Due<") && svg.contains(">Answer Buttons<"));
    assert!(
        svg.matches("<rect").count() > 300,
        "calendar cells and bars"
    );
}

#[test]
fn refusals_exit_non_zero() {
    let base = fixture();
    let output = run(base.path(), &["stats", "--deck", "No Such Deck"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("No Such Deck"), "{stderr}");
    assert!(output.stdout.is_empty());

    let output = run(base.path(), &["stats", "--search", "prop:"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());

    let output = run(base.path(), &["stats", "--deck", "Study", "--search", "x"]);
    assert_eq!(output.status.code(), Some(2), "clap refuses both scopes");
    let output = run(base.path(), &["stats", "--format", "pdf"]);
    assert_eq!(output.status.code(), Some(2));
}
