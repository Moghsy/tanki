//! Statistics stream: `StatsCommand` through the actor on the generated
//! study fixture (temp folders only).
use tanki_core::stats::*;
use tanki_core::*;

fn stats(actor: &CollectionActor, command: StatsCommand) -> Result<StatsResponse> {
    match actor.execute(Command::Manage(ManageCommand::Stats(command)))? {
        Response::Manage(ManageResponse::Stats(r)) => Ok(r),
        other => panic!("unexpected {other:?}"),
    }
}

fn report(actor: &CollectionActor, scope: StatsScope, history: History) -> StatsReport {
    match stats(
        actor,
        StatsCommand::Graphs {
            scope,
            history,
            options: None,
        },
    )
    .unwrap()
    {
        StatsResponse::Report(r) => *r,
        other => panic!("unexpected {other:?}"),
    }
}

fn card_ids(actor: &CollectionActor, query: &str) -> Vec<i64> {
    match actor
        .execute(Command::Search {
            query: query.into(),
        })
        .unwrap()
    {
        Response::CardIds(ids) => ids,
        other => panic!("unexpected {other:?}"),
    }
}

fn fixture() -> (tempfile::TempDir, CollectionActor) {
    let dir = tempfile::tempdir().unwrap();
    let actor = CollectionActor::open(dir.path()).unwrap();
    actor.execute(Command::GenerateStudyFixture).unwrap();
    (dir, actor)
}

#[test]
fn graphs_for_each_scope_and_history() {
    let (_dir, actor) = fixture();
    let all = report(&actor, StatsScope::Collection, History::Year);
    assert_eq!(all.data.search, "");
    let total = card_ids(&actor, "").len() as u64;
    let counts = &all.data.counts_including_inactive;
    let counted = counts.new + counts.learn + counts.relearn + counts.young + counts.mature;
    assert_eq!(counted as u64, total);
    // The fixture graded cards today and set due dates: reviews today,
    // review cards with intervals, cards due in the future.
    assert!(all.data.today.answer_count > 0);
    assert!(!all.data.intervals.is_empty());
    assert!(!all.data.future_due.is_empty());
    assert!(!all.panels.today.lines.is_empty());
    assert!(all.panels.reviews.bins.iter().any(|b| b.cumulative > 0));
    assert!(all.panels.calendar.max_count > 0);
    assert!(all.panels.intervals.panel.histogram.is_some());
    assert!(all.panels.card_counts.rows.iter().any(|r| r.count > 0));
    assert_eq!(all.panels.true_retention.rows.len(), 5);
    assert_eq!(all.options, PanelOptions::initial(&all.data));
    // FSRS is off in the fixture: ease instead of the FSRS panels.
    assert!(!all.panels.fsrs);
    assert!(all.panels.ease.is_some() && all.panels.difficulty.is_none());

    let deck = report(
        &actor,
        StatsScope::Deck("Study::Science".into()),
        History::All,
    );
    assert_eq!(deck.data.search, "deck:Study::Science");
    assert_eq!(deck.data.history, History::All);
    assert_eq!(deck.options.reviews_range, GraphRange::AllTime);
    assert_eq!(deck.panels.true_retention.rows.len(), 6);
    let deck_total = card_ids(&actor, "deck:Study::Science").len() as u32;
    let c = deck.data.counts_including_inactive;
    assert_eq!(c.new + c.learn + c.relearn + c.young + c.mature, deck_total);
    assert!(deck_total < total as u32);

    let current = report(&actor, StatsScope::CurrentDeck, History::Year);
    assert_eq!(current.data.search, "deck:current");
    let search = report(&actor, StatsScope::Search("is:new".into()), History::Year);
    let c = search.data.counts_including_inactive;
    assert_eq!(c.new as usize, card_ids(&actor, "is:new").len());
    assert_eq!(c.young + c.mature, 0);

    let missing = stats(
        &actor,
        StatsCommand::Graphs {
            scope: StatsScope::Deck("No Such Deck".into()),
            history: History::Year,
            options: None,
        },
    )
    .unwrap_err();
    assert!(missing.to_string().contains("No Such Deck"));
    let bad = stats(
        &actor,
        StatsCommand::Graphs {
            scope: StatsScope::Search("prop:".into()),
            history: History::Year,
            options: None,
        },
    );
    assert!(bad.is_err());
}

#[test]
fn panels_recompute_without_a_query() {
    let (_dir, actor) = fixture();
    let r = report(&actor, StatsScope::Collection, History::Year);
    let mut options = r.options.clone();
    options.future_due_range = GraphRange::Year;
    options.intervals_range = IntervalRange::All;
    options.reviews_time = true;
    let StatsResponse::Panels(panels) = stats(
        &actor,
        StatsCommand::Panels {
            data: Box::new(r.data.clone()),
            options,
        },
    )
    .unwrap() else {
        panic!("panels")
    };
    assert_eq!(panels.future_due.range, GraphRange::Year);
    assert_eq!(panels.intervals.range, IntervalRange::All);
    assert!(panels.reviews.time);
    assert_ne!(panels.reviews.subtitle, r.panels.reviews.subtitle);
    let year = panels.future_due.panel.histogram.as_ref().unwrap();
    let month = r.panels.future_due.panel.histogram.as_ref().unwrap();
    assert!(year.total >= month.total);
}

#[test]
fn preferences_are_stored_in_the_collection() {
    let (_dir, actor) = fixture();
    let r = report(&actor, StatsScope::Collection, History::Year);
    // Anki's defaults for a new collection.
    let defaults = GraphPreferences {
        first_weekday: Weekday::Sunday,
        card_counts_separate_inactive: true,
        future_due_show_backlog: true,
    };
    assert_eq!(r.data.preferences, defaults);
    let prefs = GraphPreferences {
        first_weekday: Weekday::Monday,
        card_counts_separate_inactive: false,
        future_due_show_backlog: false,
    };
    let done = stats(&actor, StatsCommand::SetPreferences(prefs)).unwrap();
    assert!(matches!(done, StatsResponse::Done));
    assert!(done.changes().is_none());
    let r = report(&actor, StatsScope::Collection, History::Year);
    assert_eq!(r.data.preferences, prefs);
    assert_eq!(r.options.first_weekday, Weekday::Monday);
    assert!(!r.options.future_due_backlog);
    assert!(!r.panels.card_counts.separate_inactive);
    assert_eq!(r.panels.card_counts.rows.len(), 5);
    assert_eq!(r.panels.calendar.weekday_labels[0], "M");
}

#[test]
fn card_info_rows_revlog_and_curve() {
    let (_dir, actor) = fixture();
    let card = card_ids(&actor, "tag:study::again card:1")[0];
    let StatsResponse::CardInfo(info) =
        stats(&actor, StatsCommand::CardInfo { card_id: card }).unwrap()
    else {
        panic!("card info")
    };
    assert_eq!(info.card_id, card);
    let labels: Vec<&str> = info.rows.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(labels[0], "Added");
    for expected in [
        "Reviews",
        "Lapses",
        "Card Type",
        "Note Type",
        "Deck",
        "Card ID",
    ] {
        assert!(
            labels.contains(&expected),
            "{expected} missing from {labels:?}"
        );
    }
    let id_row = info.rows.iter().find(|r| r.label == "Card ID").unwrap();
    assert_eq!(id_row.value, card.to_string());
    assert!(!info.revlog.is_empty());
    assert_eq!(info.revlog.len(), info.revlog_rows.len());
    assert_eq!(info.revlog_headings.len(), 8);
    assert_eq!(info.revlog_rows[0].date.len(), 10);
    // Newest first.
    assert!(info.revlog.windows(2).all(|w| w[0].time >= w[1].time));
    // FSRS is off: the card has no memory state, so desktop hides the
    // curve, although Anki replays memory states into the review log.
    assert!(!info.fsrs);
    let curve = forgetting_curve(&info.revlog, CurveRange::Week, info.decay, info.now);
    if let Some(first) = curve.first() {
        assert_eq!(first.retrievability, 100.0);
        assert!(curve.iter().all(|p| p.retrievability <= 100.0));
    }
    // Asking twice gives the same rows (the write-back is idempotent).
    let StatsResponse::CardInfo(again) =
        stats(&actor, StatsCommand::CardInfo { card_id: card }).unwrap()
    else {
        panic!("card info")
    };
    assert_eq!(again.rows, info.rows);

    let new_card = card_ids(&actor, "is:new")[0];
    let StatsResponse::CardInfo(fresh) =
        stats(&actor, StatsCommand::CardInfo { card_id: new_card }).unwrap()
    else {
        panic!("card info")
    };
    assert!(fresh.rows.iter().any(|r| r.label == "Position"));
    assert!(stats(&actor, StatsCommand::CardInfo { card_id: 1 }).is_err());
}
