//! Spike D: desktop version skew.
//!
//! Builds a collection with the pinned (26.08.1) core, lets Anki 25.09.4's
//! Python package open and change copies of it, then reopens each copy with
//! the pinned core and diffs the whole persisted state. The result is written
//! up in `docs/implementation/spike-d-version-skew.md`.
//!
//! Skips unless `TANKI_SKEW_PYTHON` names an interpreter whose `anki` package
//! is 25.09.4. Optionally, `TANKI_SKEW_PINNED_PYTHONPATH` (the folder holding
//! the pinned Python package) runs every scenario a second time with the
//! pinned package as a control, so a difference that both versions cause is
//! not mistaken for skew.
//!
//! Only one process ever has a collection open: the actor and the direct
//! `Collection` are closed before Python starts, and Python closes before the
//! next snapshot.

use anki::{
    collection::{Collection, CollectionBuilder},
    services::{
        CardRenderingService, CardsService, ConfigService, DeckConfigService, DecksService,
        NotesService, NotetypesService, SchedulerService, SearchService, StatsService,
    },
};
use anki_proto as pb;
use pb::deck_config::deck_config::config::ReviewCardOrder;
use pb::decks::deck::filtered::search_term::Order as FilteredOrder;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process,
};
use tanki_core::{CollectionActor, Command, Rating, Response};

const DESKTOP_VERSION: &str = "25.09.4";
const PINNED_VERSION: &str = "26.08.1";
const SKEW_DECK: &str = "Skew deck";
const SKEW_DECK_RENAMED: &str = "Skew deck renamed in desktop";
const FILTERED_DECK: &str = "Skew filtered";
const SPECIAL_NOTETYPE: &str = "Skew special fields";
const SPARE_PRESET: &str = "Skew spare preset";
/// FSRS-6 defaults with the first and last weight changed, so they are
/// non-default but valid.
const FSRS6_PARAMS: [f32; 21] = [
    0.3, 1.2931, 2.3065, 8.2956, 6.4133, 0.8334, 3.0194, 0.001, 1.8722, 0.1666, 0.796, 1.4835,
    0.0614, 0.2629, 1.6483, 0.6014, 1.8729, 0.5425, 0.0912, 0.0658, 0.2,
];

type Snapshot = BTreeMap<String, String>;

#[test]
fn desktop_25_09_4_round_trip() {
    let Some(python) = std::env::var_os("TANKI_SKEW_PYTHON").filter(|v| !v.is_empty()) else {
        eprintln!(
            "skipping version_skew: set TANKI_SKEW_PYTHON to an interpreter whose `anki` \
             package is {DESKTOP_VERSION} to run Spike D"
        );
        return;
    };
    let pinned = std::env::var_os("TANKI_SKEW_PINNED_PYTHONPATH").filter(|v| !v.is_empty());
    // Anki's debug build recurses deeply in places; give it room.
    std::thread::Builder::new()
        .name("tanki-version-skew".into())
        .stack_size(64 << 20)
        .spawn(move || run(python, pinned))
        .unwrap()
        .join()
        .unwrap();
}

// ---------------------------------------------------------------------------
// The spike

struct Ids {
    skew_deck: i64,
    filtered_deck: i64,
    special_notetype: i64,
    special_note: i64,
    special_cards: Vec<i64>,
    spare_preset: i64,
    moved_card: i64,
    custom_data_card: i64,
}

struct Python {
    exe: OsString,
    pythonpath: Option<OsString>,
    version: &'static str,
}

fn run(exe: OsString, pinned: Option<OsString>) {
    let desktop = Python {
        exe: exe.clone(),
        pythonpath: None,
        version: DESKTOP_VERSION,
    };
    let reported = desktop.version_check();
    assert_eq!(
        reported, DESKTOP_VERSION,
        "TANKI_SKEW_PYTHON must have Anki {DESKTOP_VERSION}"
    );
    let control = pinned.map(|path| Python {
        exe,
        pythonpath: Some(path),
        version: PINNED_VERSION,
    });
    if let Some(control) = &control {
        assert_eq!(control.version_check(), PINNED_VERSION);
    }

    let fixture = tempfile::tempdir().unwrap();
    let ids = build_fixture(fixture.path());
    let base = snapshot(fixture.path());
    assert_eq!(
        base,
        snapshot(fixture.path()),
        "snapshots must be deterministic and side-effect free"
    );
    dump("base", &base);
    check_fixture(&base, &ids);
    println!(
        "\n=== Spike D: Anki {DESKTOP_VERSION} on a collection written by the pinned core ==="
    );
    println!(
        "fixture: {} snapshot entries, {} cards",
        base.len(),
        base.keys()
            .filter(|k| k.ends_with(".id") && k.starts_with("card/"))
            .count()
    );

    let mut results = BTreeMap::new();
    for scenario in SCENARIOS {
        let outcome = run_scenario(&desktop, scenario, fixture.path(), &base, &ids);
        if let Some(control) = &control {
            let control_outcome = run_scenario(control, scenario, fixture.path(), &base, &ids);
            compare_with_control(scenario, &outcome, &control_outcome);
        }
        results.insert(scenario.name, outcome);
    }
    assert_findings(&base, &ids, &results, fixture.path());
    println!("\n=== Spike D: all findings asserted ===");
}

struct Scenario {
    name: &'static str,
    what: &'static str,
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "a_open_close",
        what: "open and close only",
    },
    Scenario {
        name: "b_deck_options",
        what: "deck options screen round trip (only the current preset sent), new/day +1",
    },
    Scenario {
        name: "c_legacy_deck_config",
        what: "legacy decks.get_config/update_config and decks.save on two presets",
    },
    Scenario {
        name: "d_daily_use",
        what: "answer (v3), edit note, set collapseTime, rename deck, save note type unchanged",
    },
    Scenario {
        name: "e1_check_database",
        what: "Check Database (col.fix_integrity)",
    },
    Scenario {
        name: "e2_empty_cards",
        what: "Empty Cards report, then delete what it lists (keep notes)",
    },
    Scenario {
        name: "e3_filtered_round_trip",
        what: "filtered deck proto round trip, legacy deck saves, rebuild",
    },
    Scenario {
        name: "e4_filtered_dialog_save",
        what: "filtered deck saved as the 25.09.4 dialog would (order combo shows -1)",
    },
    Scenario {
        name: "e5_fsrs_recompute",
        what: "deck options: desired retention 0.87 -> 0.88, FSRS w0 0.3 -> 0.35 (recomputes memory states)",
    },
];

struct Outcome {
    report: Value,
    after: Snapshot,
    changes: Vec<Change>,
}

fn run_scenario(
    python: &Python,
    scenario: &Scenario,
    fixture: &Path,
    base: &Snapshot,
    ids: &Ids,
) -> Outcome {
    let copy = copy_profile(fixture);
    let (status, report) = python.run(scenario.name, &copy.path().join("collection.anki2"));
    assert_eq!(
        report["opened"],
        Value::Bool(true),
        "{} could not open the fixture: {report}",
        python.version
    );
    assert!(
        status == 0 && report.get("error").is_none(),
        "{} failed {}: {report}",
        python.version,
        scenario.name
    );
    // The pinned core must reopen what the older version wrote.
    let after = snapshot(copy.path());
    dump(&format!("{}-{}", scenario.name, python.version), &after);
    let changes = diff(base, &after);
    let actions = &report["actions"];
    println!(
        "\n--- [{}] {}: {} ---",
        python.version, scenario.name, scenario.what
    );
    println!(
        "opened: yes; col before: {}; col after: {}",
        report["col_before"], report["col_after"]
    );
    println!("actions: {actions}");
    if let Some(extra) = report
        .get("extra")
        .filter(|e| e.as_object().is_some_and(|o| !o.is_empty()))
    {
        println!("extra: {extra}");
    }
    let cards: BTreeSet<&str> = base
        .keys()
        .filter(|k| k.starts_with("card/"))
        .map(|k| object(k))
        .collect();
    let touched: BTreeSet<&str> = changes
        .iter()
        .map(|c| object(&c.path))
        .filter(|o| o.starts_with("card/"))
        .collect();
    println!(
        "pinned core reopened it: yes; schema {}; sync_status_offline {}; cards untouched \
         (due, interval, FSRS state, custom data identical): {} of {}",
        after["collection/schema_version"],
        after["collection/sync_status_offline"],
        cards.len() - touched.len(),
        cards.len()
    );
    let mut counts = BTreeMap::new();
    for change in &changes {
        let class = classify(scenario.name, change, ids, actions);
        *counts.entry(class).or_insert(0) += 1;
        if class != Class::Bookkeeping {
            println!("  {class:<12} {change}");
        }
    }
    println!(
        "summary: {}",
        counts
            .iter()
            .map(|(class, n)| format!("{class}={n}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    Outcome {
        report,
        after,
        changes,
    }
}

fn compare_with_control(scenario: &Scenario, desktop: &Outcome, control: &Outcome) {
    // Differences between what 25.09.4 left and what the pinned package left
    // for the same actions, ignoring bookkeeping and per-run timestamps.
    let skew: Vec<_> = diff(&control.after, &desktop.after)
        .into_iter()
        .filter(|c| !is_bookkeeping(&c.path) && !is_volatile(&c.path))
        .collect();
    println!(
        "control [{}] vs [{}] for {}: {} non-bookkeeping differences",
        PINNED_VERSION,
        DESKTOP_VERSION,
        scenario.name,
        skew.len()
    );
    for change in &skew {
        println!("  skew         {change}");
    }
}

// ---------------------------------------------------------------------------
// Classification

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    /// Modification time or USN of an object that was saved.
    Bookkeeping,
    /// The object the scenario deliberately changed, and its direct effects.
    Deliberate,
    /// Anything else: lost, reset, or changed in meaning. Each one is asserted.
    Unexpected,
}

impl std::fmt::Display for Class {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(match self {
            Class::Bookkeeping => "bookkeeping",
            Class::Deliberate => "deliberate",
            Class::Unexpected => "UNEXPECTED",
        })
    }
}

fn last_segment(path: &str) -> &str {
    path.rsplit(['.', '/']).next().unwrap_or(path)
}

fn is_bookkeeping(path: &str) -> bool {
    matches!(last_segment(path), "mtime_secs" | "usn" | "mtime")
}

/// Values that legitimately differ between two runs of the same scenario.
fn is_volatile(path: &str) -> bool {
    path.starts_with("revlog/") || last_segment(path) == "last_review_time_secs"
}

fn object(path: &str) -> &str {
    path.split('.').next().unwrap_or(path)
}

/// Whether a snapshot object is the card or one of its review log entries.
fn is_card_or_revlog(obj: &str, card: i64) -> bool {
    obj == format!("card/{card}") || obj.starts_with(&format!("revlog/{card}@"))
}

fn id_in(actions: &Value, key: &str) -> Vec<i64> {
    actions
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a.get(key))
        .flat_map(|v| match v {
            Value::Array(items) => items.iter().filter_map(Value::as_i64).collect(),
            other => other.as_i64().into_iter().collect::<Vec<_>>(),
        })
        .collect()
}

/// Study counters on a deck that answering a card updates.
fn is_deck_study_stat(path: &str) -> bool {
    matches!(
        last_segment(path),
        "last_day_studied" | "new_studied" | "review_studied" | "milliseconds_studied"
    )
}

fn classify(scenario: &str, change: &Change, ids: &Ids, actions: &Value) -> Class {
    let path = change.path.as_str();
    if is_bookkeeping(path) {
        return Class::Bookkeeping;
    }
    let obj = object(path);
    let deliberate = match scenario {
        "b_deck_options" => path == "deck_config/1.config.new_per_day",
        "c_legacy_deck_config" => {
            path == "deck_config/1.config.new_per_day"
                || path == format!("deck_config/{}.config.reviews_per_day", ids.spare_preset)
        }
        "d_daily_use" => {
            let answered = id_in(actions, "answered_card");
            let card_hit = answered.iter().any(|id| is_card_or_revlog(obj, *id));
            card_hit
                || obj == format!("note/{}", ids.special_note)
                || path == "config/collapseTime"
                // The same setting, as the preferences message reports it.
                || path == "preferences.scheduling.learn_ahead_secs"
                || path == format!("deck/{}.name", ids.skew_deck)
                || (obj == "deck/1" && is_deck_study_stat(path))
                // Answering a card stores the current deck and queue state.
                || path == "config/curDeck"
                || path == "config/activeDecks"
                // The undo/last-answered bookkeeping keys.
                || path.starts_with("config/_deck_")
                || path == "deck_options.current_deck.limits.new_today"
                || path == "deck_options.current_deck.limits.review_today"
        }
        "e2_empty_cards" => {
            let deleted = id_in(actions, "deleted_cards");
            deleted.iter().any(|id| is_card_or_revlog(obj, *id))
        }
        "e3_filtered_round_trip" => {
            // Rebuilding moves cards in and out of the filtered deck.
            (obj.starts_with("card/")
                && matches!(
                    last_segment(path),
                    "deck_id" | "original_deck_id" | "due" | "original_due" | "queue"
                ))
                || path == "config/curDeck"
                || path == "config/activeDecks"
        }
        "e4_filtered_dialog_save" => {
            path.starts_with(&format!(
                "deck/{}.kind.search_terms[0].order",
                ids.filtered_deck
            )) || (obj.starts_with("card/")
                && matches!(
                    last_segment(path),
                    "deck_id" | "original_deck_id" | "due" | "original_due" | "queue"
                ))
                || path == "config/curDeck"
                || path == "config/activeDecks"
        }
        "e5_fsrs_recompute" => {
            path == "deck_config/1.config.desired_retention"
                || path == "deck_config/1.config.fsrs_params_6[0]"
                || (obj.starts_with("card/")
                    && (path.contains(".memory_state.") || path.ends_with(".desired_retention")))
                || (obj.starts_with("revlog/") && path.contains(".memory_state."))
        }
        _ => false,
    };
    if deliberate {
        Class::Deliberate
    } else {
        Class::Unexpected
    }
}

fn unexpected<'a>(outcome: &'a Outcome, scenario: &str, ids: &Ids) -> Vec<&'a Change> {
    outcome
        .changes
        .iter()
        .filter(|c| classify(scenario, c, ids, &outcome.report["actions"]) == Class::Unexpected)
        .collect()
}

// ---------------------------------------------------------------------------
// Assertions: what survives and what does not

fn assert_findings(base: &Snapshot, ids: &Ids, results: &BTreeMap<&str, Outcome>, fixture: &Path) {
    let preset_order =
        |s: &Snapshot, id: i64| s[&format!("deck_config/{id}.config.review_order")].clone();
    let filtered_order =
        |s: &Snapshot| s[&format!("deck/{}.kind.search_terms[0].order", ids.filtered_deck)].clone();
    for (name, outcome) in results {
        let report = &outcome.report;
        // 25.09.4 opens the file, schema 18 stays, and nothing it does here
        // touches the schema-modified stamp (no forced full sync).
        assert_eq!(report["col_before"]["ver"], 18, "{name}");
        assert_eq!(report["col_after"]["ver"], 18, "{name}");
        assert_eq!(
            report["col_before"]["scm"], report["col_after"]["scm"],
            "{name}: schema-modified stamp changed"
        );
        assert_eq!(
            report["col_before"]["ls"], report["col_after"]["ls"],
            "{name}"
        );
        assert_eq!(outcome.after["collection/schema_version"], "18", "{name}");
        // 25.09.4 reads the new enum values back as raw numbers.
        let observed = &report["observed"];
        assert_eq!(observed["presets"]["Default"]["review_order"], 12, "{name}");
        assert_eq!(
            observed["presets"][SPARE_PRESET]["review_order"], 12,
            "{name}"
        );
        assert_eq!(observed["legacy_default_preset_reviewOrder"], 12, "{name}");
        assert_eq!(
            observed["filtered_orders_proto"],
            serde_json::json!([10]),
            "{name}"
        );
        assert_eq!(observed["filtered_terms_legacy"][0][2], 10, "{name}");
        assert_eq!(
            observed["config_experimentalFeatures"],
            serde_json::json!({"1": true})
        );
        // The new review order survives every scenario, on both presets.
        assert_eq!(
            preset_order(&outcome.after, 1),
            "RelativeOverdueness",
            "{name}"
        );
        assert_eq!(
            preset_order(&outcome.after, ids.spare_preset),
            "RelativeOverdueness",
            "{name}"
        );
        // New config keys survive.
        assert_eq!(
            outcome.after["config/experimentalFeatures"], r#"{"1":true}"#,
            "{name}"
        );
        // Custom data survives.
        assert_eq!(
            outcome.after[&format!("card/{}.custom_data", ids.custom_data_card)],
            base[&format!("card/{}.custom_data", ids.custom_data_card)],
            "{name}"
        );
        if *name != "e4_filtered_dialog_save" {
            assert_eq!(
                filtered_order(&outcome.after),
                "RelativeOverdueness",
                "{name}"
            );
        }
    }

    // (a)-(c): nothing at all beyond bookkeeping and the deliberate edit.
    for name in ["a_open_close", "b_deck_options", "c_legacy_deck_config"] {
        let left = unexpected(&results[name], name, ids);
        assert!(left.is_empty(), "{name}: {left:#?}");
    }
    assert!(
        results["a_open_close"].changes.is_empty(),
        "open/close alone must change nothing: {:#?}",
        results["a_open_close"].changes
    );

    // (d): daily use changes only what it touches.
    let d = &results["d_daily_use"];
    let left = unexpected(d, "d_daily_use", ids);
    assert!(left.is_empty(), "d_daily_use: {left:#?}");
    assert_eq!(
        d.after[&format!("deck/{}.name", ids.skew_deck)],
        format!("{SKEW_DECK_RENAMED:?}")
    );
    // The deck-level overrides survive the rename.
    for field in ["review_limit", "new_limit", "desired_retention"] {
        let key = format!("deck/{}.kind.{field}", ids.skew_deck);
        assert_eq!(d.after[&key], base[&key], "{key}");
    }
    // Both cards of the special-field note survive an edit in 25.09.4.
    for card in &ids.special_cards {
        assert!(d.after.contains_key(&format!("card/{card}.id")));
    }
    // The moved card's FSRS state (computed by the pinned core on set_deck)
    // is untouched.
    for field in [
        "memory_state.stability",
        "memory_state.difficulty",
        "desired_retention",
        "due",
    ] {
        let key = format!("card/{}.{field}", ids.moved_card);
        assert_eq!(d.after[&key], base[&key], "{key}");
    }

    // (e1): Check Database leaves the newer core's data alone.
    let e1 = &results["e1_check_database"];
    let left = unexpected(e1, "e1_check_database", ids);
    assert!(left.is_empty(), "e1_check_database: {left:#?}");

    // (e2): the newer core generates a card for {{#Tags}} that 25.09.4 calls
    // empty; its Empty Cards tool deletes it.
    let e2 = &results["e2_empty_cards"];
    let deleted = id_in(&e2.report["actions"], "deleted_cards");
    assert_eq!(
        deleted,
        vec![ids.special_cards[1]],
        "25.09.4 empty-cards deletion"
    );
    assert!(!e2
        .after
        .contains_key(&format!("card/{}.id", ids.special_cards[1])));
    assert_eq!(base["derived/empty_cards_newer_core"], "[]");
    let left = unexpected(e2, "e2_empty_cards", ids);
    assert!(left.is_empty(), "e2_empty_cards: {left:#?}");

    // (e3): proto round trip, legacy saves and a rebuild keep order 10, but
    // 25.09.4 rebuilds with its default order instead (prost's getter maps
    // the unknown value to OLDEST_REVIEWED_FIRST). Which cards that selects
    // depends on hash tie-breaks, so it is printed, not asserted.
    let e3 = &results["e3_filtered_round_trip"];
    let left = unexpected(e3, "e3_filtered_round_trip", ids);
    assert!(left.is_empty(), "e3_filtered_round_trip: {left:#?}");
    let members = |s: &Snapshot| -> Vec<String> {
        s.iter()
            .filter(|(k, v)| k.ends_with(".deck_id") && **v == ids.filtered_deck.to_string())
            .map(|(k, _)| object(k).to_string())
            .collect()
    };
    println!(
        "\ne3: filtered deck members: built by pinned core {:?}; after 25.09.4 rebuild {:?}",
        members(base),
        members(&e3.after)
    );

    // (e4): the 25.09.4 dialog would store -1, which both cores read as the
    // default order (oldest reviewed first).
    let e4 = &results["e4_filtered_dialog_save"];
    assert_eq!(e4.report["extra"]["filtered_order_label_count"], 10);
    assert_eq!(filtered_order(&e4.after), "-1");

    // (e5): 25.09.4 recomputes memory states with its own FSRS crate.
    let e5 = &results["e5_fsrs_recompute"];
    let left = unexpected(e5, "e5_fsrs_recompute", ids);
    assert!(left.is_empty(), "e5_fsrs_recompute: {left:#?}");
    let native = native_fsrs_recompute(fixture);
    let mut compared = 0;
    let mut max_rel = 0f64;
    for (key, value) in &e5.after {
        if key.starts_with("card/") && key.contains(".memory_state.") {
            let ours: f64 = native[key].parse().unwrap();
            let theirs: f64 = value.parse().unwrap();
            max_rel = max_rel.max(((ours - theirs) / ours).abs());
            compared += 1;
        }
    }
    println!(
        "\ne5: memory states recomputed by {DESKTOP_VERSION} vs the pinned core after the same \
         edit: {compared} values, max relative difference {max_rel:.2e}"
    );
    assert!(compared > 0);
    // For these short histories the two FSRS crates (5.1.0 in 25.09.4, 6.6.1
    // in the pinned core) agree exactly.
    assert!(max_rel < 1e-6, "FSRS recompute differs: {max_rel}");
    let dr_key = format!("card/{}.desired_retention", ids.moved_card);
    println!(
        "e5: moved card (deck override 0.93) desired_retention: before {}, {DESKTOP_VERSION} {}, pinned {}",
        base[&dr_key], e5.after[&dr_key], native[&dr_key]
    );
}

/// The pinned core performs scenario e5's edit itself, for comparison.
fn native_fsrs_recompute(fixture: &Path) -> Snapshot {
    let copy = copy_profile(fixture);
    let mut col = open(copy.path());
    let data =
        DeckConfigService::get_deck_configs_for_update(&mut col, pb::decks::DeckId { did: 1 })
            .unwrap();
    let mut current = data
        .all_config
        .iter()
        .filter_map(|c| c.config.clone())
        .find(|c| c.id == data.current_deck.as_ref().unwrap().config_id)
        .unwrap();
    let inner = current.config.as_mut().unwrap();
    inner.desired_retention = 0.88;
    inner.fsrs_params_6[0] = 0.35;
    let _ = DeckConfigService::update_deck_configs(
        &mut col,
        pb::deck_config::UpdateDeckConfigsRequest {
            target_deck_id: 1,
            configs: vec![current],
            removed_config_ids: vec![],
            mode: pb::deck_config::UpdateDeckConfigsMode::Normal as i32,
            card_state_customizer: data.card_state_customizer.clone(),
            limits: data.current_deck.as_ref().and_then(|d| d.limits),
            new_cards_ignore_review_limit: data.new_cards_ignore_review_limit,
            apply_all_parent_limits: data.apply_all_parent_limits,
            fsrs: data.fsrs,
            fsrs_reschedule: false,
            fsrs_health_check: data.fsrs_health_check,
        },
    )
    .unwrap();
    col.close(None).unwrap();
    snapshot(copy.path())
}

// ---------------------------------------------------------------------------
// Fixture

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn open(profile: &Path) -> Collection {
    CollectionBuilder::new(profile.join("collection.anki2"))
        .set_media_paths(
            profile.join("collection.media"),
            profile.join("collection.media.db2"),
        )
        .build()
        .unwrap()
}

fn build_fixture(profile: &Path) -> Ids {
    // 1. Through the actor: stock fixture, then answers with SM-2.
    let actor = CollectionActor::open(profile).unwrap();
    let Response::CardIds(cards) = actor.execute(Command::GenerateFixture).unwrap() else {
        panic!("fixture")
    };
    assert!(cards.len() >= 8);
    for rating in [
        Rating::Easy,
        Rating::Easy,
        Rating::Good,
        Rating::Again,
        Rating::Hard,
        Rating::Easy,
    ] {
        let Response::Queue(queue) = actor.execute(Command::Queue { limit: 1 }).unwrap() else {
            panic!("queue")
        };
        actor
            .execute(Command::Answer {
                card_id: queue.cards[0].id,
                rating,
                answered_at_millis: now_millis(),
                milliseconds_taken: 4000,
            })
            .unwrap();
    }
    // Keep some new cards outside the filtered deck built below. (If every
    // new card sits in a filtered deck, Check Database in both versions resets
    // `nextPos` to 0 because filtered new cards have negative due numbers.)
    for i in 0..3 {
        actor
            .execute(Command::AddNote {
                notetype: "Basic".into(),
                fields: vec![format!("Extra front {i}"), format!("Extra back {i}")],
            })
            .unwrap();
    }
    actor.close().unwrap();

    // 2. Directly: everything the pinned core can store, set to non-defaults.
    let mut col = open(profile);
    let data =
        DeckConfigService::get_deck_configs_for_update(&mut col, pb::decks::DeckId { did: 1 })
            .unwrap();
    let mut default = data
        .all_config
        .iter()
        .filter_map(|c| c.config.clone())
        .find(|c| c.id == 1)
        .unwrap();
    {
        let c = default.config.as_mut().unwrap();
        // New in 26.08 (value 12); the only new persisted value in a preset.
        c.review_order = ReviewCardOrder::RelativeOverdueness as i32;
        // Older FSRS fields, as controls.
        c.desired_retention = 0.87;
        c.historical_retention = 0.88;
        c.fsrs_params_6 = FSRS6_PARAMS.to_vec();
        c.ignore_revlogs_before_date = "2001-01-01".into();
        c.easy_days_percentages = vec![1.0, 1.0, 1.0, 1.0, 1.0, 0.5, 0.0];
        c.new_per_day = 33;
    }
    let mut spare = default.clone();
    spare.id = 0;
    spare.name = SPARE_PRESET.into();
    spare.config.as_mut().unwrap().new_per_day = 44;
    let _ = DeckConfigService::update_deck_configs(
        &mut col,
        pb::deck_config::UpdateDeckConfigsRequest {
            target_deck_id: 1,
            // The last entry is assigned to the target deck.
            configs: vec![spare, default],
            removed_config_ids: vec![],
            mode: pb::deck_config::UpdateDeckConfigsMode::Normal as i32,
            card_state_customizer: data.card_state_customizer.clone(),
            limits: data.current_deck.as_ref().and_then(|d| d.limits),
            new_cards_ignore_review_limit: data.new_cards_ignore_review_limit,
            apply_all_parent_limits: data.apply_all_parent_limits,
            // Enabling FSRS computes memory states for the cards answered above.
            fsrs: true,
            fsrs_reschedule: false,
            fsrs_health_check: false,
        },
    )
    .unwrap();
    let spare_preset =
        DeckConfigService::get_deck_configs_for_update(&mut col, pb::decks::DeckId { did: 1 })
            .unwrap()
            .all_config
            .into_iter()
            .filter_map(|c| c.config)
            .find(|c| c.name == SPARE_PRESET)
            .unwrap()
            .id;

    // Config keys written by 26.08 desktop (Python side); the core never
    // writes them, but a profile used with 26.08 desktop can carry them.
    for (key, json) in [
        ("experimentalFeatures", r#"{"1":true}"#),
        ("customColorPickerPalette", r##"["#ff0000"]"##),
    ] {
        let _ = ConfigService::set_config_json(
            &mut col,
            pb::config::SetConfigJsonRequest {
                key: key.into(),
                value_json: json.as_bytes().to_vec(),
                undoable: false,
            },
        )
        .unwrap();
    }

    // A note type whose second template is conditional on a special field.
    // The pinned core generates that card when the note has tags; 25.09.4
    // does not (fixed upstream in #4533 / #4807).
    let stock = NotetypesService::get_stock_notetype_legacy(
        &mut col,
        pb::notetypes::StockNotetype {
            kind: pb::notetypes::stock_notetype::Kind::BasicAndReversed as i32,
        },
    )
    .unwrap();
    let mut legacy: Value = serde_json::from_slice(&stock.json).unwrap();
    legacy["name"] = SPECIAL_NOTETYPE.into();
    legacy["tmpls"][1]["qfmt"] = "{{#Tags}}{{Back}}{{/Tags}}".into();
    let special_notetype = NotetypesService::add_notetype_legacy(
        &mut col,
        pb::generic::Json {
            json: serde_json::to_vec(&legacy).unwrap(),
        },
    )
    .unwrap()
    .id;
    let mut note = NotesService::new_note(
        &mut col,
        pb::notetypes::NotetypeId {
            ntid: special_notetype,
        },
    )
    .unwrap();
    note.fields = vec!["Special front".into(), "Special back".into()];
    note.tags = vec!["skew".into()];
    let special_note = NotesService::add_note(
        &mut col,
        pb::notes::AddNoteRequest {
            note: Some(note),
            deck_id: 1,
        },
    )
    .unwrap()
    .note_id;
    let mut special_cards = search_cards(&mut col, &format!("nid:{special_note}"));
    special_cards.sort_by_key(|id| {
        CardsService::get_card(&mut col, pb::cards::CardId { cid: *id })
            .unwrap()
            .template_idx
    });
    assert_eq!(
        special_cards.len(),
        2,
        "pinned core makes the {{#Tags}} card"
    );

    // A normal deck with deck-level overrides.
    let mut deck = DecksService::new_deck(&mut col).unwrap();
    deck.name = SKEW_DECK.into();
    match deck.kind.as_mut() {
        Some(pb::decks::deck::Kind::Normal(normal)) => {
            normal.review_limit = Some(77);
            normal.new_limit = Some(7);
            normal.desired_retention = Some(0.93);
            normal.description = "Deck-level overrides".into();
        }
        _ => panic!("new deck is not normal"),
    }
    let skew_deck = DecksService::add_deck(&mut col, deck).unwrap().id;

    // Answer with FSRS on; one answer carries custom data.
    let custom_data_card = answer_direct(&mut col, 3, Some(r#"{"skew":"yes"}"#.into()));
    answer_direct(&mut col, 2, None);
    answer_direct(&mut col, 0, None);

    // Move a review card with a memory state into the override deck; the
    // pinned core recomputes its memory state there (#5339).
    let moved_card = search_cards(&mut col, "deck:Default is:review prop:s>0")
        .into_iter()
        .find(|id| *id != custom_data_card)
        .expect("a review card with FSRS state");
    let _ = CardsService::set_deck(
        &mut col,
        pb::cards::SetDeckRequest {
            card_ids: vec![moved_card],
            deck_id: skew_deck,
        },
    )
    .unwrap();

    // A filtered deck using the new order (value 10).
    let mut filtered =
        DecksService::get_or_create_filtered_deck(&mut col, pb::decks::DeckId { did: 0 }).unwrap();
    filtered.name = FILTERED_DECK.into();
    filtered.allow_empty = true;
    filtered.config.as_mut().unwrap().search_terms = vec![pb::decks::deck::filtered::SearchTerm {
        search: "deck:Default is:new".into(),
        limit: 2,
        order: FilteredOrder::RelativeOverdueness as i32,
    }];
    let filtered_deck = DecksService::add_or_update_filtered_deck(&mut col, filtered)
        .unwrap()
        .id;
    let _ = DecksService::set_current_deck(&mut col, pb::decks::DeckId { did: 1 }).unwrap();
    col.close(None).unwrap();

    Ids {
        skew_deck,
        filtered_deck,
        special_notetype,
        special_note,
        special_cards,
        spare_preset,
        moved_card,
        custom_data_card,
    }
}

fn search_cards(col: &mut Collection, search: &str) -> Vec<i64> {
    SearchService::search_cards(
        col,
        pb::search::SearchRequest {
            search: search.into(),
            order: None,
        },
    )
    .unwrap()
    .ids
}

fn answer_direct(col: &mut Collection, rating: i32, custom_data: Option<String>) -> i64 {
    let queued = SchedulerService::get_queued_cards(
        col,
        pb::scheduler::GetQueuedCardsRequest {
            fetch_limit: 1,
            intraday_learning_only: false,
        },
    )
    .unwrap();
    let top = queued.cards.into_iter().next().expect("a card to answer");
    let card = top.card.unwrap();
    let states = top.states.unwrap();
    let mut new_state = match rating {
        0 => states.again,
        1 => states.hard,
        2 => states.good,
        _ => states.easy,
    }
    .unwrap();
    new_state.custom_data = custom_data;
    let _ = SchedulerService::answer_card(
        col,
        pb::scheduler::CardAnswer {
            card_id: card.id,
            current_state: states.current,
            new_state: Some(new_state),
            rating,
            answered_at_millis: now_millis(),
            milliseconds_taken: 3000,
        },
    )
    .unwrap();
    card.id
}

fn check_fixture(base: &Snapshot, ids: &Ids) {
    assert_eq!(
        base["deck_config/1.config.review_order"],
        "RelativeOverdueness"
    );
    assert_eq!(
        base[&format!("deck/{}.kind.search_terms[0].order", ids.filtered_deck)],
        "RelativeOverdueness"
    );
    assert_eq!(base["deck_options.fsrs"], "true");
    assert_eq!(
        base[&format!("card/{}.custom_data", ids.custom_data_card)],
        r#""{\"skew\":\"yes\"}""#
    );
    assert!(base.contains_key(&format!("card/{}.memory_state.stability", ids.moved_card)));
    assert_eq!(
        base[&format!("card/{}.desired_retention", ids.moved_card)],
        "0.93"
    );
    let with_state = base
        .keys()
        .filter(|k| k.starts_with("card/") && k.ends_with(".memory_state.stability"))
        .count();
    assert!(with_state >= 3, "FSRS memory states: {with_state}");
    assert!(base.contains_key(&format!("notetype/{}.name", ids.special_notetype)));
}

// ---------------------------------------------------------------------------
// Snapshot through the pinned core

/// The sanctioned read-only schema-version peek (PLAN 4.4); nothing else in
/// Rust reads the file without Anki.
fn schema_version(profile: &Path) -> i64 {
    let conn = rusqlite::Connection::open_with_flags(
        profile.join("collection.anki2"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    conn.query_row("select ver from col", [], |r| r.get(0))
        .unwrap()
}

fn snapshot(profile: &Path) -> Snapshot {
    let mut s = Snapshot::new();
    s.insert(
        "collection/schema_version".into(),
        schema_version(profile).to_string(),
    );
    let mut col = open(profile);
    s.insert(
        "collection/sync_status_offline".into(),
        format!("{:?}", col.sync_status_offline().unwrap()),
    );

    // Raw `other` bytes are shown as text; they replace the byte lists that
    // `flatten` writes for the same paths.
    let mut others: Vec<(String, String)> = Vec::new();
    let mut options =
        DeckConfigService::get_deck_configs_for_update(&mut col, pb::decks::DeckId { did: 1 })
            .unwrap();
    for entry in std::mem::take(&mut options.all_config) {
        let mut config = entry.config.unwrap();
        let prefix = format!("deck_config/{}", config.id);
        s.insert(format!("{prefix}.use_count"), entry.use_count.to_string());
        if let Some(inner) = config.config.as_mut() {
            others.push((
                format!("{prefix}.config.other"),
                take_text(&mut inner.other),
            ));
        }
        flatten(&prefix, &format!("{config:#?}"), &mut s);
    }
    options.defaults = None;
    flatten("deck_options", &format!("{options:#?}"), &mut s);

    let decks = DecksService::get_deck_names(
        &mut col,
        pb::decks::GetDeckNamesRequest {
            skip_empty_default: false,
            include_filtered: true,
        },
    )
    .unwrap();
    for entry in decks.entries {
        let mut deck =
            DecksService::get_deck(&mut col, pb::decks::DeckId { did: entry.id }).unwrap();
        let prefix = format!("deck/{}", deck.id);
        if let Some(common) = deck.common.as_mut() {
            others.push((
                format!("{prefix}.common.other"),
                take_text(&mut common.other),
            ));
        }
        flatten(&prefix, &format!("{deck:#?}"), &mut s);
    }

    for entry in NotetypesService::get_notetype_names(&mut col)
        .unwrap()
        .entries
    {
        let mut nt =
            NotetypesService::get_notetype(&mut col, pb::notetypes::NotetypeId { ntid: entry.id })
                .unwrap();
        let prefix = format!("notetype/{}", nt.id);
        if let Some(config) = nt.config.as_mut() {
            others.push((
                format!("{prefix}.config.other"),
                take_text(&mut config.other),
            ));
        }
        for (i, field) in nt.fields.iter_mut().enumerate() {
            if let Some(config) = field.config.as_mut() {
                others.push((
                    format!("{prefix}.fields[{i}].config.other"),
                    take_text(&mut config.other),
                ));
            }
        }
        for (i, template) in nt.templates.iter_mut().enumerate() {
            if let Some(config) = template.config.as_mut() {
                others.push((
                    format!("{prefix}.templates[{i}].config.other"),
                    take_text(&mut config.other),
                ));
            }
        }
        flatten(&prefix, &format!("{nt:#?}"), &mut s);
    }

    let config: Value =
        serde_json::from_slice(&ConfigService::get_all_config(&mut col).unwrap().json).unwrap();
    for (key, value) in config.as_object().unwrap() {
        s.insert(format!("config/{key}"), canonical_json(value));
    }
    let prefs = ConfigService::get_preferences(&mut col).unwrap();
    flatten("preferences", &format!("{prefs:#?}"), &mut s);

    for cid in search_cards(&mut col, "") {
        let card = CardsService::get_card(&mut col, pb::cards::CardId { cid }).unwrap();
        flatten(&format!("card/{cid}"), &format!("{card:#?}"), &mut s);
        let logs = StatsService::get_review_logs(&mut col, pb::cards::CardId { cid }).unwrap();
        // Keyed by time, so a new entry does not shift the others.
        let mut seen = BTreeMap::new();
        for entry in &logs.entries {
            let n = seen.entry(entry.time).or_insert(0);
            *n += 1;
            let key = format!("revlog/{cid}@{}#{n}", entry.time);
            flatten(&key, &format!("{entry:#?}"), &mut s);
        }
    }
    let notes = SearchService::search_notes(
        &mut col,
        pb::search::SearchRequest {
            search: "".into(),
            order: None,
        },
    )
    .unwrap()
    .ids;
    for nid in notes {
        let note = NotesService::get_note(&mut col, pb::notes::NoteId { nid }).unwrap();
        flatten(&format!("note/{nid}"), &format!("{note:#?}"), &mut s);
    }

    let mut empty: Vec<i64> = CardRenderingService::get_empty_cards(&mut col)
        .unwrap()
        .notes
        .into_iter()
        .flat_map(|n| n.card_ids)
        .collect();
    empty.sort();
    s.insert(
        "derived/empty_cards_newer_core".into(),
        format!("{empty:?}"),
    );
    s.extend(others);
    col.close(None).unwrap();
    s
}

fn take_text(bytes: &mut Vec<u8>) -> String {
    let taken = std::mem::take(bytes);
    match serde_json::from_slice::<Value>(&taken) {
        Ok(value) => canonical_json(&value),
        Err(_) => format!("{:?}", String::from_utf8_lossy(&taken)),
    }
}

/// JSON with object keys sorted, whatever map type serde_json was built with.
fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<_, _> = map.iter().collect();
            let inner: Vec<String> = sorted
                .into_iter()
                .map(|(k, v)| format!("{}:{}", Value::String(k.clone()), canonical_json(v)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::Array(items) => {
            format!(
                "[{}]",
                items
                    .iter()
                    .map(canonical_json)
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        other => other.to_string(),
    }
}

/// Turns pretty `Debug` output of a protobuf message into `path = value`
/// lines, so a diff names the field that changed.
fn flatten(prefix: &str, text: &str, out: &mut Snapshot) {
    struct Frame {
        path: String,
        array: bool,
        next: usize,
    }
    let mut stack = vec![Frame {
        path: prefix.to_string(),
        array: false,
        next: 0,
    }];
    for raw in text.lines() {
        let line = raw.trim();
        let line = line.strip_suffix(',').unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        if matches!(line, "}" | ")" | "]") {
            stack.pop();
            continue;
        }
        let top = stack.last_mut().unwrap();
        let (path, rest) = if top.array {
            top.next += 1;
            (format!("{}[{}]", top.path, top.next - 1), line)
        } else if let Some((key, value)) = line.split_once(": ").filter(|(k, _)| {
            !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }) {
            (format!("{}.{key}", top.path), value)
        } else {
            // A type name such as `Some(` or `Config {`: transparent.
            (top.path.clone(), line)
        };
        if rest.ends_with(['{', '(', '[']) {
            stack.push(Frame {
                path,
                array: rest.ends_with('['),
                next: 0,
            });
        } else {
            let previous = out.insert(path.clone(), rest.to_string());
            assert!(previous.is_none(), "duplicate snapshot path {path}");
        }
    }
}

/// With `TANKI_SKEW_DUMP_DIR` set, writes each snapshot there as text.
fn dump(name: &str, snapshot: &Snapshot) {
    if let Some(dir) = std::env::var_os("TANKI_SKEW_DUMP_DIR").filter(|v| !v.is_empty()) {
        let text: String = snapshot
            .iter()
            .map(|(k, v)| format!("{k} = {v}\n"))
            .collect();
        std::fs::write(Path::new(&dir).join(format!("{name}.txt")), text).unwrap();
    }
}

// ---------------------------------------------------------------------------
// Diff

#[derive(Debug, Clone, PartialEq)]
struct Change {
    path: String,
    before: Option<String>,
    after: Option<String>,
}

impl std::fmt::Display for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let show = |v: &Option<String>| v.clone().unwrap_or_else(|| "<absent>".into());
        write!(
            f,
            "{}: {} -> {}",
            self.path,
            show(&self.before),
            show(&self.after)
        )
    }
}

fn diff(before: &Snapshot, after: &Snapshot) -> Vec<Change> {
    let mut keys: Vec<&String> = before.keys().chain(after.keys()).collect();
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .filter_map(|k| {
            let (b, a) = (before.get(k), after.get(k));
            (b != a).then(|| Change {
                path: k.clone(),
                before: b.cloned(),
                after: a.cloned(),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Files and processes

fn copy_profile(src: &Path) -> tempfile::TempDir {
    let dst = tempfile::tempdir().unwrap();
    for name in [
        "collection.anki2",
        "collection.anki2-wal",
        "collection.media.db2",
    ] {
        let from = src.join(name);
        if from.exists() {
            std::fs::copy(&from, dst.path().join(name)).unwrap();
        }
    }
    assert!(
        !src.join("collection.anki2-wal").exists()
            || std::fs::metadata(src.join("collection.anki2-wal"))
                .unwrap()
                .len()
                == 0,
        "fixture must be closed cleanly before copying"
    );
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

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/version-skew.py")
}

impl Python {
    fn command(&self, args: &[&OsStr]) -> (i32, Value) {
        let mut cmd = process::Command::new(&self.exe);
        cmd.arg("-B")
            .arg(script())
            .arg("--expect-version")
            .arg(self.version)
            .args(args)
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env_remove("PYTHONPATH")
            .env_remove("PYTHONHOME")
            .env_remove("PYTHONSTARTUP")
            .current_dir(std::env::temp_dir());
        if let Some(path) = &self.pythonpath {
            cmd.env("PYTHONPATH", path);
        }
        let output = cmd
            .output()
            .unwrap_or_else(|e| panic!("running {:?}: {e}", self.exe));
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let value = stdout
            .lines()
            .rev()
            .find_map(|l| serde_json::from_str::<Value>(l).ok())
            .unwrap_or_else(|| panic!("no JSON from driver; stdout:\n{stdout}\nstderr:\n{stderr}"));
        (output.status.code().unwrap_or(-1), value)
    }

    fn version_check(&self) -> String {
        let (status, value) = self.command(&[OsStr::new("version")]);
        assert_eq!(
            status, 0,
            "the interpreter's Anki is not {}: {value}",
            self.version
        );
        value["version"].as_str().unwrap().to_string()
    }

    fn run(&self, scenario: &str, collection: &Path) -> (i32, Value) {
        self.command(&[
            OsStr::new("run"),
            OsStr::new(scenario),
            collection.as_os_str(),
        ])
    }
}
