//! The statistics and card info widgets against a real `CollectionActor`
//! on the study fixture: scope, history and search switches query Anki,
//! range switches recompute, the browser links are valid Anki searches, and
//! a review elsewhere refreshes the page.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{buffer::Buffer, layout::Rect};
use std::path::PathBuf;
use tanki_core::stats::{CountKind, GraphRange, History, StatsScope};
use tanki_core::{CollectionActor, Command, OpChanges, Rating, Response};
use tanki_tui::stats_ui::{
    visible_panels, CardInfoIntent, CardInfoUi, PanelKind, StatsIntent, StatsOptions, StatsUi,
};

struct FixtureDir(PathBuf);
impl FixtureDir {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("tanki-tui-stats-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture() -> (FixtureDir, CollectionActor) {
    let dir = FixtureDir::new();
    let actor = CollectionActor::open(&dir.0).unwrap();
    actor.execute(Command::GenerateStudyFixture).unwrap();
    (dir, actor)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn lines(render: impl Fn(Rect, &mut Buffer), width: u16, height: u16) -> Vec<String> {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    render(area, &mut buf);
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol().to_owned())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

fn draw(ui: &StatsUi, width: u16, height: u16) -> Vec<String> {
    lines(|area, buf| ui.render(area, buf), width, height)
}

fn search(actor: &CollectionActor, query: &str) -> Vec<i64> {
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

fn open(actor: &CollectionActor) -> StatsUi {
    StatsUi::open(
        actor,
        StatsOptions {
            scope: StatsScope::CurrentDeck,
            deck_name: Some("Study".into()),
            history: History::Year,
        },
    )
    .unwrap()
}

#[test]
fn statistics_against_the_actor() {
    let (_dir, actor) = fixture();
    let mut ui = open(&actor);
    let screen = draw(&ui, 100, 60).join("\n");
    assert!(screen.starts_with("Statistics"), "{screen}");
    assert!(screen.contains("deck: Study"), "{screen}");
    assert!(screen.contains("Studied "), "{screen}");
    assert!(screen.contains("Future Due"), "{screen}");
    // FSRS is off in the fixture: SM-2's ease instead of the FSRS panels.
    let visible = visible_panels(ui.panels());
    assert_eq!(visible.len(), 11);
    assert!(visible.contains(&PanelKind::Ease));

    // Every card-count row links to an Anki search that finds its cards.
    for row in &ui.panels().card_counts.rows {
        let query = format!("deck:current ({})", row.query);
        assert_eq!(
            search(&actor, &query).len() as u32,
            row.count,
            "{:?}: {query}",
            row.kind
        );
    }

    // A future-due bar opens the browser on a search Anki accepts.
    ui.on_key(&actor, key(KeyCode::Tab));
    assert_eq!(ui.focused(), PanelKind::FutureDue);
    let mut browsed = None;
    for _ in 0..40 {
        ui.on_key(&actor, key(KeyCode::Right));
        if let StatsIntent::Browse(query) = ui.on_key(&actor, key(KeyCode::Enter)) {
            if !search(&actor, &format!("deck:current ({query})")).is_empty() {
                browsed = Some(query);
                break;
            }
        }
    }
    assert!(browsed.is_some(), "no future-due bar found cards");

    // Range switches recompute; scope and history query again.
    ui.on_key(&actor, key(KeyCode::Char('r')));
    assert_eq!(ui.panels().future_due.range, GraphRange::ThreeMonths);
    let deck_new = ui
        .panels()
        .card_counts
        .rows
        .iter()
        .map(|r| r.count)
        .sum::<u32>();
    ui.on_key(&actor, key(KeyCode::Char('s')));
    assert_eq!(ui.scope(), &StatsScope::Collection);
    let all = ui
        .panels()
        .card_counts
        .rows
        .iter()
        .map(|r| r.count)
        .sum::<u32>();
    assert_eq!(all as usize, search(&actor, "").len());
    assert!(all >= deck_new);
    ui.on_key(&actor, key(KeyCode::Char('a')));
    assert_eq!(ui.history(), History::All);
    assert_eq!(ui.options().reviews_range, GraphRange::AllTime);
    assert!(draw(&ui, 100, 20)[0].contains(&ui.panels().labels.all_history));

    // A search scope: only new cards.
    ui.on_key(&actor, key(KeyCode::Char('/')));
    for c in "is:new".chars() {
        ui.on_key(&actor, key(KeyCode::Char(c)));
    }
    ui.on_key(&actor, key(KeyCode::Enter));
    assert_eq!(ui.scope(), &StatsScope::Search("is:new".into()));
    let new_cards = search(&actor, "is:new").len() as u32;
    let counts = &ui.panels().card_counts.rows;
    assert_eq!(counts.iter().map(|r| r.count).sum::<u32>(), new_cards);
    assert!(counts
        .iter()
        .filter(|r| matches!(r.kind, CountKind::Young | CountKind::Mature))
        .all(|r| r.count == 0));
    // Anki rejects a bad search: the scope stays and its message shows.
    ui.on_key(&actor, key(KeyCode::Char('/')));
    ui.on_key(
        &actor,
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
    );
    for c in "prop:".chars() {
        ui.on_key(&actor, key(KeyCode::Char(c)));
    }
    ui.on_key(&actor, key(KeyCode::Enter));
    assert_eq!(ui.scope(), &StatsScope::Search("is:new".into()));
    assert!(ui.message().is_some());
    assert_eq!(ui.on_key(&actor, key(KeyCode::Esc)), StatsIntent::Close);
}

#[test]
fn a_review_elsewhere_refreshes_the_page() {
    let (_dir, actor) = fixture();
    let mut ui = open(&actor);
    let before = ui.panels().today.lines.clone();
    let Response::Queue(queue) = actor.execute(Command::Queue { limit: 1 }).unwrap() else {
        panic!("queue")
    };
    let card = queue.cards.first().expect("a due card").id;
    actor
        .execute(Command::Answer {
            card_id: card,
            rating: Rating::Good,
            answered_at_millis: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64,
            milliseconds_taken: 4000,
        })
        .unwrap();
    // Changes that do not touch cards, decks, config or notes are ignored.
    ui.on_changes(
        &actor,
        &OpChanges {
            tag: true,
            ..Default::default()
        },
    );
    assert_eq!(ui.panels().today.lines, before);
    ui.on_changes(
        &actor,
        &OpChanges {
            card: true,
            ..Default::default()
        },
    );
    assert_ne!(ui.panels().today.lines, before);
}

#[test]
fn card_info_against_the_actor() {
    let (_dir, actor) = fixture();
    let card = search(&actor, "tag:study::again card:1")[0];
    let mut ui = CardInfoUi::open(&actor, card).unwrap();
    assert_eq!(ui.info().card_id, card);
    let screen = lines(|area, buf| ui.render(area, buf), 100, 40).join("\n");
    assert!(screen.contains("Card Info"), "{screen}");
    assert!(screen.contains("Added"), "{screen}");
    assert!(screen.contains(&card.to_string()), "{screen}");
    // The review log table, newest first, with Anki's headings.
    assert!(
        screen.contains("Date") && screen.contains("Rating"),
        "{screen}"
    );
    assert!(!ui.info().revlog_rows.is_empty());
    // FSRS is off: no forgetting curve.
    assert!(!screen.contains("Forgetting"), "{screen}");
    // Refreshing after a change keeps the same card.
    ui.refresh(&actor).unwrap();
    assert_eq!(ui.info().card_id, card);
    assert_eq!(ui.on_key(key(KeyCode::Esc)), CardInfoIntent::Close);
    assert!(CardInfoUi::open(&actor, 1).is_err());
}
