//! Statistics and card info widget tests with a fake collection answering
//! `ManageCommand::Stats` (panels computed by tanki-core's English port).
use super::*;
use crate::testing::buffer_text;
use std::cell::RefCell;
use std::path::Path;
use tanki_core::stats::{
    compute_panels_english, ButtonCounts, CardInfo, CardStateCounts, GraphRange, HourCounts,
    InfoRow, IntervalRange, LocalDate, MemoryState, RetentionCounts, ReviewKind, ReviewKinds,
    RevlogEntry, RevlogRow, TodayCounts, Weekday,
};
use tanki_core::Result;

fn kinds(mature: u32, young: u32, relearn: u32, learn: u32, filtered: u32) -> ReviewKinds {
    ReviewKinds {
        learn,
        relearn,
        young,
        mature,
        filtered,
    }
}

pub(crate) fn sample_data() -> GraphsData {
    let date = LocalDate {
        year: 2026,
        month: 11,
        day: 15,
    };
    let mut hours = vec![HourCounts::default(); 24];
    hours[9] = HourCounts {
        total: 6,
        correct: 5,
    };
    hours[20] = HourCounts {
        total: 3,
        correct: 1,
    };
    GraphsData {
        today_date: date,
        day_zero_date: date,
        today: TodayCounts {
            answer_count: 12,
            answer_millis: 96_000,
            correct_count: 10,
            mature_correct: 3,
            mature_count: 4,
            learn_count: 5,
            review_count: 6,
            relearn_count: 1,
            early_review_count: 0,
        },
        future_due: [(0, 5), (1, 4), (2, 1), (10, 3), (20, 2)]
            .into_iter()
            .collect(),
        daily_load: 3,
        review_counts: [
            (-20, kinds(3, 1, 1, 0, 0)),
            (-5, kinds(0, 0, 0, 2, 1)),
            (-1, kinds(4, 0, 0, 0, 0)),
            (0, kinds(1, 2, 1, 5, 0)),
        ]
        .into_iter()
        .collect(),
        review_times: [(0, kinds(10_000, 20_000, 5_000, 40_000, 0))]
            .into_iter()
            .collect(),
        counts_including_inactive: CardStateCounts {
            new: 10,
            learn: 2,
            relearn: 1,
            young: 5,
            mature: 2,
            ..Default::default()
        },
        counts_excluding_inactive: CardStateCounts {
            new: 9,
            learn: 2,
            relearn: 1,
            young: 4,
            mature: 2,
            suspended: 1,
            buried: 1,
        },
        intervals: [(1, 3), (2, 2), (5, 1), (10, 4), (25, 2)]
            .into_iter()
            .collect(),
        eases: [(250, 10), (230, 2)].into_iter().collect(),
        ease_average: 250.0,
        true_retention: tanki_core::stats::TrueRetentionData {
            today: RetentionCounts {
                young_passed: 5,
                young_failed: 1,
                mature_passed: 3,
                mature_failed: 1,
            },
            ..Default::default()
        },
        hours: tanki_core::stats::HoursData {
            one_month: hours.clone(),
            three_months: hours.clone(),
            one_year: hours.clone(),
            all_time: hours,
        },
        buttons: tanki_core::stats::ButtonsData {
            one_year: ButtonCounts {
                learning: [2, 0, 5, 1],
                young: [1, 1, 4, 0],
                mature: [0, 0, 3, 1],
            },
            ..Default::default()
        },
        added: [(-3, 4), (0, 2)].into_iter().collect(),
        ..Default::default()
    }
}

pub(crate) struct Fake {
    pub data: GraphsData,
    pub log: RefCell<Vec<StatsCommand>>,
    pub info: Option<CardInfo>,
}

impl Fake {
    pub(crate) fn new() -> Self {
        Self {
            data: sample_data(),
            log: RefCell::new(Vec::new()),
            info: None,
        }
    }
    fn last(&self) -> StatsCommand {
        self.log.borrow().last().cloned().expect("a command")
    }
}

impl Backend for Fake {
    fn execute(&self, command: Command) -> Result<Response> {
        let Command::Manage(ManageCommand::Stats(command)) = command else {
            return Err(CoreError::Other("unexpected command".into()));
        };
        self.log.borrow_mut().push(command.clone());
        let response = match command {
            StatsCommand::Graphs {
                scope,
                history,
                options,
            } => {
                if scope == StatsScope::Search("bad(".into()) {
                    return Err(CoreError::Invalid("Invalid search".into()));
                }
                let mut data = self.data.clone();
                data.history = history;
                let options = options.unwrap_or_else(|| PanelOptions::initial(&data));
                StatsResponse::Report(Box::new(StatsReport {
                    panels: compute_panels_english(&data, &options),
                    data,
                    options,
                }))
            }
            StatsCommand::Panels { data, options } => {
                StatsResponse::Panels(Box::new(compute_panels_english(&data, &options)))
            }
            StatsCommand::SetPreferences(_) => StatsResponse::Done,
            StatsCommand::CardInfo { .. } => match &self.info {
                Some(info) => StatsResponse::CardInfo(Box::new(info.clone())),
                None => return Err(CoreError::Other("no card".into())),
            },
        };
        Ok(Response::Manage(ManageResponse::Stats(response)))
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn draw(ui: &StatsUi, width: u16, height: u16) -> Vec<String> {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    ui.render(area, &mut buf);
    buffer_text(&buf)
}

/// Text snapshots under `src/stats_ui/snapshots` (styles are covered by the
/// chart tests). `TANKI_UPDATE_SNAPSHOTS=1` rewrites them.
fn snapshot(name: &str, lines: &[String]) {
    let text = lines.join("\n") + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/stats_ui/snapshots")
        .join(format!("{name}.txt"));
    if std::env::var_os("TANKI_UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &text).unwrap();
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(text, expected, "snapshot {name}");
}

fn open(fake: &Fake) -> StatsUi {
    StatsUi::open(
        fake,
        StatsOptions {
            scope: StatsScope::CurrentDeck,
            deck_name: Some("Study".into()),
            history: History::Year,
        },
    )
    .unwrap()
}

#[test]
fn statistics_screen_snapshots() {
    let fake = Fake::new();
    let mut ui = open(&fake);
    snapshot("stats_top", &draw(&ui, 80, 40));
    // Every panel, page by page.
    let mut pages = Vec::new();
    for _ in 0..12 {
        ui.on_key(&fake, key(KeyCode::PageDown));
        pages.extend(draw(&ui, 80, 40));
    }
    snapshot("stats_pages", &pages);
}

#[test]
fn panels_follow_page_order_and_fsrs() {
    let fake = Fake::new();
    let ui = open(&fake);
    use PanelKind as P;
    assert_eq!(
        visible_panels(ui.panels()),
        vec![
            P::Today,
            P::FutureDue,
            P::Calendar,
            P::Reviews,
            P::CardCounts,
            P::Intervals,
            P::Ease,
            P::TrueRetention,
            P::Hours,
            P::Buttons,
            P::Added
        ]
    );
    let mut fake = Fake::new();
    fake.data.fsrs = true;
    fake.data.stability = fake.data.intervals.clone();
    fake.data.difficulty = [(40, 3), (60, 2)].into_iter().collect();
    let ui = open(&fake);
    let visible = visible_panels(ui.panels());
    assert!(!visible.contains(&P::Ease));
    assert!(visible.contains(&P::Stability) && visible.contains(&P::Retrievability));
    let text = draw(&ui, 80, 200).join("\n");
    assert!(text.contains("Card Difficulty"), "{text}");
}

#[test]
fn ranges_recompute_without_a_query() {
    let fake = Fake::new();
    let mut ui = open(&fake);
    draw(&ui, 80, 40);
    // Future due is the second panel.
    ui.on_key(&fake, key(KeyCode::Tab));
    assert_eq!(ui.focused(), PanelKind::FutureDue);
    ui.on_key(&fake, key(KeyCode::Char('r')));
    let StatsCommand::Panels { options, .. } = fake.last() else {
        panic!("panels")
    };
    assert_eq!(options.future_due_range, GraphRange::ThreeMonths);
    assert_eq!(ui.panels().future_due.range, GraphRange::ThreeMonths);
    // Intervals cycle through percentiles.
    for _ in 0..4 {
        ui.on_key(&fake, key(KeyCode::Tab));
    }
    assert_eq!(ui.focused(), PanelKind::Intervals);
    ui.on_key(&fake, key(KeyCode::Char('r')));
    assert_eq!(ui.options().intervals_range, IntervalRange::All);
    // Count/time toggle.
    ui.on_key(&fake, key(KeyCode::Char('t')));
    assert!(ui.panels().reviews.time);
    let graphs = fake
        .log
        .borrow()
        .iter()
        .filter(|c| matches!(c, StatsCommand::Graphs { .. }))
        .count();
    assert_eq!(graphs, 1);
}

#[test]
fn scope_history_and_search() {
    let fake = Fake::new();
    let mut ui = open(&fake);
    ui.on_key(&fake, key(KeyCode::Char('s')));
    assert_eq!(ui.scope(), &StatsScope::Collection);
    assert!(matches!(
        fake.last(),
        StatsCommand::Graphs {
            scope: StatsScope::Collection,
            ..
        }
    ));
    ui.on_key(&fake, key(KeyCode::Char('s')));
    assert_eq!(ui.scope(), &StatsScope::CurrentDeck);
    ui.on_key(&fake, key(KeyCode::Char('a')));
    assert_eq!(ui.history(), History::All);
    assert_eq!(ui.options().reviews_range, GraphRange::AllTime);
    assert!(matches!(
        fake.last(),
        StatsCommand::Graphs {
            history: History::All,
            ..
        }
    ));
    ui.on_key(&fake, key(KeyCode::Char('/')));
    for c in "is:new".chars() {
        ui.on_key(&fake, key(KeyCode::Char(c)));
    }
    let shown = draw(&ui, 80, 20);
    // Anki's own label ("Search"), as on the scope line.
    assert_eq!(shown.last().unwrap(), "Search: is:new");
    ui.on_key(&fake, key(KeyCode::Enter));
    assert_eq!(ui.scope(), &StatsScope::Search("is:new".into()));
    assert!(draw(&ui, 80, 20)[0].contains("Search: is:new"));
    // A bad search keeps the scope and shows Anki's message.
    ui.on_key(&fake, key(KeyCode::Char('/')));
    ui.on_key(&fake, key(KeyCode::Char('u')));
    let input = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
    ui.on_key(&fake, input);
    for c in "bad(".chars() {
        ui.on_key(&fake, key(KeyCode::Char(c)));
    }
    ui.on_key(&fake, key(KeyCode::Enter));
    assert_eq!(ui.scope(), &StatsScope::Search("is:new".into()));
    assert_eq!(ui.message(), Some("Invalid search"));
    assert_eq!(draw(&ui, 80, 20).last().unwrap(), "Invalid search");
}

#[test]
fn preferences_are_saved() {
    let fake = Fake::new();
    let mut ui = open(&fake);
    ui.on_key(&fake, key(KeyCode::Char('w')));
    let saved: Vec<_> = fake
        .log
        .borrow()
        .iter()
        .filter_map(|c| match c {
            StatsCommand::SetPreferences(p) => Some(*p),
            _ => None,
        })
        .collect();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].first_weekday, Weekday::Monday);
    assert_eq!(ui.panels().calendar.weekday_labels[0], "M");
    ui.on_key(&fake, key(KeyCode::Char('i')));
    assert!(ui.panels().card_counts.separate_inactive);
    assert_eq!(ui.panels().card_counts.rows.len(), 7);
    ui.on_key(&fake, key(KeyCode::Char('[')));
    assert_eq!(ui.panels().calendar.year, 2025);
    ui.on_key(&fake, key(KeyCode::Char('[')));
    assert_eq!(ui.panels().calendar.year, 2025);
}

#[test]
fn selecting_a_bar_and_browsing() {
    let fake = Fake::new();
    let mut ui = open(&fake);
    draw(&ui, 80, 40);
    ui.on_key(&fake, key(KeyCode::Tab));
    assert_eq!(ui.on_key(&fake, key(KeyCode::Enter)), StatsIntent::None);
    ui.on_key(&fake, key(KeyCode::Right));
    let lines = draw(&ui, 80, 40);
    assert!(
        lines.last().unwrap().starts_with("Today: · 5 cards due"),
        "{:?}",
        lines.last()
    );
    assert_eq!(
        ui.on_key(&fake, key(KeyCode::Enter)),
        StatsIntent::Browse("\"prop:due=0\"".into())
    );
    ui.on_key(&fake, key(KeyCode::Right));
    assert_eq!(
        ui.on_key(&fake, key(KeyCode::Enter)),
        StatsIntent::Browse("\"prop:due=1\"".into())
    );
    // Card counts rows are links too.
    for _ in 0..3 {
        ui.on_key(&fake, key(KeyCode::Tab));
    }
    assert_eq!(ui.focused(), PanelKind::CardCounts);
    ui.on_key(&fake, key(KeyCode::Right));
    assert_eq!(
        ui.on_key(&fake, key(KeyCode::Enter)),
        StatsIntent::Browse("\"is:new\"".into())
    );
    assert_eq!(ui.on_key(&fake, key(KeyCode::Esc)), StatsIntent::Close);
}

#[test]
fn narrow_screens_group_bars() {
    let fake = Fake::new();
    let mut ui = open(&fake);
    // 40 columns fit the 32 future-due bins one column each.
    draw(&ui, 40, 30);
    ui.on_key(&fake, key(KeyCode::Tab));
    assert_eq!(ui.items(PanelKind::FutureDue).len(), 32);
    // 20 columns (a 16-column chart): 16 columns of two days.
    draw(&ui, 20, 30);
    let items = ui.items(PanelKind::FutureDue);
    assert_eq!(items.len(), 16);
    assert_eq!(
        items[0].query.as_deref(),
        Some("(\"prop:due=0\") OR (\"prop:due=1\")")
    );
    assert!(draw(&ui, 20, 30).iter().all(|l| l.chars().count() <= 20));
    assert!(draw(&ui, 40, 30).iter().all(|l| l.chars().count() <= 40));
}

pub(crate) fn sample_info() -> CardInfo {
    let now = 1_700_000_000 + 5 * 86400;
    let entry = |time: i64, kind, button, stability: f32, days: u32| RevlogEntry {
        time,
        kind,
        button,
        interval: days * 86400,
        ease: 530,
        taken_secs: 6.5,
        memory_state: Some(MemoryState {
            stability,
            difficulty: 5.0,
        }),
        last_interval: 0,
    };
    let revlog = vec![
        entry(1_700_000_000 + 2 * 86400, ReviewKind::Review, 3, 10.0, 10),
        entry(1_700_000_000, ReviewKind::Learning, 1, 2.0, 2),
    ];
    let row = |date: &str, kind: ReviewKind, label: &str, rating, interval: &str| RevlogRow {
        date: date.into(),
        time: "10:00".into(),
        kind,
        kind_label: label.into(),
        rating,
        interval: interval.into(),
        ease: "D:43%".into(),
        taken: "6.5s".into(),
        elapsed: String::new(),
        stability: String::new(),
    };
    CardInfo {
        card_id: 42,
        note_id: 7,
        rows: vec![
            InfoRow {
                label: "Added".into(),
                value: "2023-11-14".into(),
            },
            InfoRow {
                label: "Stability".into(),
                value: "10 days".into(),
            },
            InfoRow {
                label: "Card ID".into(),
                value: "42".into(),
            },
        ],
        revlog_headings: [
            "Date",
            "Type",
            "Rating",
            "Interval",
            "Ease",
            "Time",
            "Elapsed Time",
            "Stability",
        ]
        .map(String::from)
        .to_vec(),
        revlog_rows: vec![
            row("2023-11-16", ReviewKind::Review, "Review", 3, "10 days"),
            row("2023-11-14", ReviewKind::Learning, "Learn", 1, "2 days"),
        ],
        revlog,
        fsrs: true,
        desired_retention: 0.9,
        decay: 0.5,
        curve_title: "Forgetting Curve".into(),
        curve_range_labels: ["First week", "First month", "First year", "All time"]
            .map(String::from)
            .to_vec(),
        desired_retention_label: "Desired retention".into(),
        now,
    }
}

#[test]
fn card_info_rows_revlog_and_curve() {
    let mut fake = Fake::new();
    fake.info = Some(sample_info());
    let mut ui = CardInfoUi::open(&fake, 42).unwrap();
    assert!(matches!(
        fake.last(),
        StatsCommand::CardInfo { card_id: 42 }
    ));
    let area = Rect::new(0, 0, 70, 26);
    let mut buf = Buffer::empty(area);
    ui.render(area, &mut buf);
    let lines = buffer_text(&buf);
    // Dates in the curve labels depend on the local time zone; compare
    // everything above the curve's x axis.
    snapshot("card_info", &lines[..lines.len() - 3]);
    let text = lines.join("\n");
    assert!(
        text.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)),
        "{text}"
    );
    assert_eq!(ui.curve_range(), CurveRange::Month);
    ui.on_key(key(KeyCode::Char('r')));
    assert_eq!(ui.curve_range(), CurveRange::Week);
    ui.on_key(key(KeyCode::Char('r')));
    assert_eq!(ui.curve_range(), CurveRange::Month);
    assert_eq!(ui.on_key(key(KeyCode::Esc)), CardInfoIntent::Close);
    assert!(ui.hints().contains(&("r", "curve range")));

    // Without a memory state there is no curve.
    let mut info = sample_info();
    info.fsrs = false;
    let ui = CardInfoUi::new(info).with_title("Previous Card Info");
    let mut buf = Buffer::empty(area);
    ui.render(area, &mut buf);
    let text = buffer_text(&buf).join("\n");
    assert!(text.contains("Previous Card Info") && !text.contains("Forgetting"));
}

use tanki_core::stats::CurveRange;
