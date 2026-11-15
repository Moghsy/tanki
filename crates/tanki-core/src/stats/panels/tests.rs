//! Panel maths against fixed aggregates. The expected bins and totals were
//! printed by the pinned TypeScript logic (`future-due.ts`, `reviews.ts`,
//! `intervals.ts`, `ease.ts`, `difficulty.ts`, `added.ts` with d3-array
//! 3.2.4 and d3-scale 4.0.2) under node, for exactly these inputs.
use super::*;
use crate::stats::TodayCounts;

fn tr() -> I18n {
    I18n::template_only()
}

fn kinds(mature: u32, young: u32, relearn: u32, learn: u32, filtered: u32) -> ReviewKinds {
    ReviewKinds {
        learn,
        relearn,
        young,
        mature,
        filtered,
    }
}

fn data() -> GraphsData {
    GraphsData {
        today_date: LocalDate {
            year: 2026,
            month: 11,
            day: 15,
        },
        day_zero_date: LocalDate {
            year: 2026,
            month: 11,
            day: 15,
        },
        future_due: [
            (-3, 2),
            (-1, 1),
            (0, 5),
            (1, 4),
            (2, 1),
            (10, 3),
            (40, 2),
            (400, 1),
        ]
        .into_iter()
        .collect(),
        have_backlog: true,
        daily_load: 7,
        review_counts: [
            (-400, kinds(0, 0, 0, 1, 0)),
            (-40, kinds(1, 2, 0, 0, 0)),
            (-20, kinds(3, 1, 1, 0, 0)),
            (-5, kinds(0, 0, 0, 2, 1)),
            (-1, kinds(4, 0, 0, 0, 0)),
            (0, kinds(1, 1, 1, 1, 1)),
        ]
        .into_iter()
        .collect(),
        intervals: [(1, 3), (2, 2), (5, 1), (10, 4), (25, 2), (60, 1), (200, 1)]
            .into_iter()
            .collect(),
        eases: [(130, 1), (200, 3), (250, 10), (255, 2), (310, 1)]
            .into_iter()
            .collect(),
        ease_average: 250.0,
        difficulty: [(10, 2), (35, 5), (50, 10), (72, 3), (99, 1)]
            .into_iter()
            .collect(),
        difficulty_average: 48.6,
        added: [(-200, 3), (-30, 5), (-10, 2), (-1, 1), (0, 4)]
            .into_iter()
            .collect(),
        ..Default::default()
    }
}

/// `(x0, x1, value)` of the non-empty bins.
fn filled(h: &Histogram) -> Vec<(f64, f64, u64)> {
    h.bins
        .iter()
        .filter(|b| b.value > 0)
        .map(|b| (b.x0, b.x1, b.value))
        .collect()
}

fn table(panel: &HistogramPanel) -> Vec<(String, String)> {
    panel
        .table
        .iter()
        .map(|r| (r.label.clone(), r.value.clone()))
        .collect()
}

#[test]
fn future_due_bins_ranges_and_backlog() {
    let tr = tr();
    let d = data();
    let p = future_due_panel(&tr, &d, GraphRange::Month, false);
    let h = p.panel.histogram.as_ref().unwrap();
    assert_eq!(h.bins.len(), 32);
    assert_eq!(h.total, 13);
    assert_eq!(
        filled(h),
        vec![(0.0, 1.0, 5), (1.0, 2.0, 4), (2.0, 3.0, 1), (10.0, 11.0, 3)]
    );
    assert_eq!(
        table(&p.panel),
        vec![
            ("Total".into(), "13 reviews".into()),
            ("Average".into(), "0 reviews/day".into()),
            ("Due tomorrow".into(), "4 reviews".into()),
            ("Daily load".into(), "7 reviews/day".into()),
        ]
    );
    // Clicks: single days and the last ([31, 31]) bin.
    assert_eq!(h.bins[0].query.as_deref(), Some("\"prop:due=0\""));
    assert_eq!(h.bins[31].query.as_deref(), Some("\"prop:due=31\""));
    assert_eq!(h.bins[1].cumulative, 9);

    let p = future_due_panel(&tr, &d, GraphRange::Month, true);
    let h = p.panel.histogram.as_ref().unwrap();
    assert_eq!(h.bins.len(), 35);
    assert_eq!(h.total, 16);
    assert_eq!(filled(h)[..2], [(-3.0, -2.0, 2), (-1.0, 0.0, 1)]);

    let p = future_due_panel(&tr, &d, GraphRange::AllTime, false);
    let h = p.panel.histogram.as_ref().unwrap();
    assert_eq!(h.bins.len(), 81);
    assert_eq!(
        filled(h),
        vec![
            (0.0, 5.0, 10),
            (10.0, 15.0, 3),
            (40.0, 45.0, 2),
            (400.0, 400.0, 1)
        ]
    );
    assert_eq!(
        h.bins[0].query.as_deref(),
        Some("\"prop:due>=0\" AND \"prop:due<=4\"")
    );

    let p = future_due_panel(&tr, &d, GraphRange::Year, false);
    let h = p.panel.histogram.as_ref().unwrap();
    assert_eq!((h.bins.len(), h.total), (74, 15));

    let empty = GraphsData::default();
    let p = future_due_panel(&tr, &empty, GraphRange::Month, false);
    assert!(p.panel.histogram.is_none() && p.panel.table.is_empty());
}

#[test]
fn reviews_bins_and_table() {
    let tr = tr();
    let d = data();
    let p = reviews_panel(&tr, &d, GraphRange::Month, false);
    assert_eq!(p.bins.len(), 32);
    assert_eq!((p.bins[0].x0, p.bins[0].x1), (-30.0, -29.0));
    let last = p.bins.last().unwrap();
    assert_eq!((last.x0, last.x1), (1.0, 1.0));
    let filled: Vec<_> = p
        .bins
        .iter()
        .filter(|b| b.values.iter().sum::<u64>() > 0)
        .map(|b| (b.x0, b.x1, b.values.iter().sum::<u64>()))
        .collect();
    assert_eq!(
        filled,
        vec![
            (-20.0, -19.0, 5),
            (-5.0, -4.0, 3),
            (-1.0, 0.0, 4),
            (0.0, 1.0, 5)
        ]
    );
    // Mature, young, relearn, learn, filtered.
    let day = p.bins.iter().find(|b| b.x0 == -20.0).unwrap();
    assert_eq!(day.values, [3, 1, 1, 0, 0]);
    let rows: Vec<_> = p.table.iter().map(|r| r.value.clone()).collect();
    assert_eq!(
        rows,
        vec![
            "4 of 31 (13%)",
            "17 reviews",
            "1 review/day",
            "4 reviews/day"
        ]
    );

    let p = reviews_panel(&tr, &d, GraphRange::Year, false);
    assert_eq!(p.bins.len(), 74);
    assert_eq!((p.bins[0].x0, p.bins[0].x1), (-364.0, -359.0));
    let sums: Vec<_> = p
        .bins
        .iter()
        .filter(|b| b.cumulative > 0 && b.values.iter().sum::<u64>() > 0)
        .map(|b| (b.x0, b.values.iter().sum::<u64>()))
        .collect();
    assert_eq!(sums, vec![(-44.0, 3), (-24.0, 5), (-9.0, 3), (-4.0, 9)]);
    assert_eq!(p.table[0].value, "5 of 365 (1%)");

    let p = reviews_panel(&tr, &d, GraphRange::AllTime, false);
    assert_eq!(p.bins.len(), 82);
    assert_eq!((p.bins[0].x0, p.bins[0].x1), (-400.0, -399.0));
    assert_eq!(p.table[1].value, "21 reviews");
    assert_eq!(p.table[0].value, "6 of 401 (1%)");

    // Time mode adds the answer time row.
    let mut timed = d.clone();
    timed.review_times = [
        (-1, kinds(60_000, 0, 0, 0, 0)),
        (0, kinds(0, 30_000, 0, 0, 0)),
    ]
    .into_iter()
    .collect();
    let p = reviews_panel(&tr, &timed, GraphRange::Month, true);
    assert_eq!(p.table.len(), 5);
    assert_eq!(p.table[1].value, "1.5 minutes");
    // 90 s over the 9 reviews of days -1 and 0 … and the other month days.
    assert!(
        p.table[4].value.starts_with("5.29s"),
        "{}",
        p.table[4].value
    );
}

#[test]
fn interval_ranges_and_percentiles() {
    let tr = tr();
    let d = data();
    let cases = [
        (
            IntervalRange::Month,
            32,
            12,
            vec![
                (1.0, 2.0, 3),
                (2.0, 3.0, 2),
                (5.0, 6.0, 1),
                (10.0, 11.0, 4),
                (25.0, 26.0, 2),
            ],
        ),
        (
            IntervalRange::Percentile50,
            12,
            10,
            vec![(1.0, 2.0, 3), (2.0, 3.0, 2), (5.0, 6.0, 1), (10.0, 11.0, 4)],
        ),
        (
            IntervalRange::Percentile95,
            56,
            13,
            vec![
                (1.0, 3.0, 5),
                (5.0, 7.0, 1),
                (9.0, 11.0, 4),
                (25.0, 27.0, 2),
                (59.0, 61.0, 1),
            ],
        ),
        (
            IntervalRange::All,
            111,
            14,
            vec![
                (1.0, 3.0, 5),
                (5.0, 7.0, 1),
                (9.0, 11.0, 4),
                (25.0, 27.0, 2),
                (59.0, 61.0, 1),
                (199.0, 201.0, 1),
            ],
        ),
    ];
    for (range, bins, total, expected) in cases {
        let p = interval_panel(&tr, &d.intervals, range, false);
        let h = p.histogram.as_ref().unwrap();
        assert_eq!((h.bins.len(), h.total), (bins, total), "{range:?}");
        assert_eq!(filled(h), expected, "{range:?}");
        assert_eq!(p.table[0].value, "10 days");
    }
    let p = interval_panel(&tr, &d.intervals, IntervalRange::Percentile95, true);
    let h = p.histogram.as_ref().unwrap();
    assert_eq!((h.bins.len(), h.total), (56, 13));
    assert_eq!(h.bins[0].x0, 0.0);
    // FSRS queries search stability with half-day margins.
    assert_eq!(
        h.bins[0].query.as_deref(),
        Some("\"prop:s>=0\" \"prop:s<0.5\"")
    );
    let p = interval_panel(&tr, &d.intervals, IntervalRange::Month, false);
    let h = p.histogram.as_ref().unwrap();
    assert_eq!(h.bins[0].query.as_deref(), Some("\"prop:ivl=1\""));
    assert!(
        interval_panel(&tr, &BTreeMap::new(), IntervalRange::All, false)
            .histogram
            .is_none()
    );
}

#[test]
fn ease_and_difficulty_bins() {
    let tr = tr();
    let d = data();
    let p = ease_panel(&tr, &d);
    let h = p.histogram.as_ref().unwrap();
    assert_eq!(h.bins.len(), 21);
    assert_eq!(
        filled(h),
        vec![
            (130.0, 140.0, 1),
            (200.0, 210.0, 3),
            (250.0, 260.0, 12),
            (310.0, 320.0, 1)
        ]
    );
    assert_eq!(
        h.bins[0].query.as_deref(),
        Some("\"prop:ease>=1.3\" AND \"prop:ease<1.4\"")
    );
    assert_eq!(p.table[0].value, "250%");

    let cases = [
        (PercentRange::All, 21, (0.0, 5.0)),
        (PercentRange::Percentile50, 17, (35.0, 36.0)),
        (PercentRange::Percentile95, 19, (10.0, 15.0)),
        (PercentRange::Percentile100, 19, (10.0, 15.0)),
    ];
    for (range, bins, first) in cases {
        let p = difficulty_panel(&tr, &d, range);
        let h = p.histogram.as_ref().unwrap();
        assert_eq!(h.bins.len(), bins, "{range:?}");
        assert_eq!((h.bins[0].x0, h.bins[0].x1), first, "{range:?}");
    }
    let p = difficulty_panel(&tr, &d, PercentRange::Percentile50);
    assert_eq!(
        filled(p.histogram.as_ref().unwrap()),
        vec![(35.0, 36.0, 5), (50.0, 51.0, 10)]
    );
    let p = difficulty_panel(&tr, &d, PercentRange::All);
    let h = p.histogram.as_ref().unwrap();
    assert_eq!(
        filled(h),
        vec![
            (10.0, 15.0, 2),
            (35.0, 40.0, 5),
            (50.0, 55.0, 10),
            (70.0, 75.0, 3),
            (95.0, 100.0, 1)
        ]
    );
    // The last bin reaches 99 inclusive.
    let top = h.bins.iter().find(|b| b.x0 == 95.0).unwrap();
    assert_eq!(
        top.query.as_deref(),
        Some("\"prop:d>=0.95\" AND \"prop:d<=1\"")
    );
    assert_eq!(p.table[0].value, "49%");
}

#[test]
fn added_bins_and_queries() {
    let tr = tr();
    let d = data();
    let p = added_panel(&tr, &d, GraphRange::Month);
    let h = p.histogram.as_ref().unwrap();
    assert_eq!((h.bins.len(), h.total), (33, 12));
    assert_eq!(
        filled(h),
        vec![
            (-30.0, -29.0, 5),
            (-10.0, -9.0, 2),
            (-1.0, 0.0, 1),
            (0.0, 1.0, 4)
        ]
    );
    let today = h.bins.iter().find(|b| b.x0 == 0.0).unwrap();
    assert_eq!(today.query.as_deref(), Some("\"added:1\""));
    let yesterday = h.bins.iter().find(|b| b.x0 == -1.0).unwrap();
    assert_eq!(
        yesterday.query.as_deref(),
        Some("\"added:2\" AND -\"added:1\"")
    );
    assert_eq!(table(&p)[0].1, "12 cards");
    let p = added_panel(&tr, &d, GraphRange::AllTime);
    let h = p.histogram.as_ref().unwrap();
    assert_eq!((h.bins.len(), h.total), (101, 15));
    assert_eq!(filled(h)[0], (-200.0, -198.0, 3));
}

#[test]
fn card_counts_today_retention_hours_buttons() {
    let tr = tr();
    let mut d = data();
    d.counts_including_inactive = CardStateCounts {
        new: 10,
        learn: 2,
        relearn: 1,
        young: 5,
        mature: 2,
        ..Default::default()
    };
    d.counts_excluding_inactive = CardStateCounts {
        new: 8,
        learn: 2,
        relearn: 1,
        young: 4,
        mature: 2,
        suspended: 2,
        buried: 1,
    };
    let p = card_counts_panel(&tr, &d, false);
    assert_eq!(p.rows.len(), 5);
    assert_eq!(p.total, "20");
    assert_eq!(p.rows[0].percent, "50%");
    assert_eq!(p.rows[0].query, "\"is:new\"");
    let p = card_counts_panel(&tr, &d, true);
    assert_eq!(p.rows.len(), 7);
    assert_eq!(p.rows[5].kind, CountKind::Suspended);
    assert_eq!(p.rows[5].percent, "10%");
    assert_eq!(
        p.rows[3].query,
        "(\"is:review\" AND -\"is:learn\") AND \"prop:ivl<21\"AND -(\"is:buried\" OR \"is:suspended\")"
    );

    assert_eq!(today_panel(&tr, &d).lines.len(), 1);
    d.today = TodayCounts {
        answer_count: 3,
        answer_millis: 30_000,
        correct_count: 2,
        mature_correct: 1,
        mature_count: 2,
        learn_count: 1,
        review_count: 2,
        ..Default::default()
    };
    let lines = today_panel(&tr, &d).lines;
    assert_eq!(lines.len(), 4);
    assert!(lines[1].ends_with(" 1 (33.33%)"), "{}", lines[1]);
    assert!(lines[3].contains("1/2 (50%)"), "{}", lines[3]);

    d.true_retention.today = RetentionCounts {
        young_passed: 9,
        young_failed: 1,
        mature_passed: 0,
        mature_failed: 0,
    };
    let p = true_retention_panel(&tr, &d, RetentionMode::Summary);
    assert_eq!(p.rows.len(), 5);
    assert_eq!(p.rows[0].young.retention, "90.0%");
    assert_eq!(p.rows[0].all.passed, 9);
    assert_eq!(p.rows[0].mature.retention, "N/A");
    d.history = History::All;
    assert_eq!(
        true_retention_panel(&tr, &d, RetentionMode::Young)
            .rows
            .len(),
        6
    );

    let mut hours = vec![HourCounts::default(); 24];
    hours[9] = HourCounts {
        total: 4,
        correct: 3,
    };
    d.hours.one_year = hours;
    let p = hours_panel(&tr, &d, GraphRange::Year);
    assert!(p.has_data);
    assert_eq!(p.hours[9].ratio, 0.75);
    assert!(!hours_panel(&tr, &d, GraphRange::Month).has_data);

    d.buttons.one_year.young = [1, 0, 3, 0];
    let p = buttons_panel(&tr, &d, GraphRange::Year);
    assert!(p.has_data);
    assert_eq!(p.groups[1].total, 4);
    assert_eq!(p.groups[1].correct, 3);
    assert_eq!(p.groups[1].percent, "75");
    assert_eq!(p.groups[1].pressed[2], "3 (75%)");
    assert_eq!(p.groups[0].percent, "0");
}

#[test]
fn calendar_layout_weeks_and_intensity() {
    let tr = tr();
    let d = data();
    // 2026-11-15 is a Sunday; 1 January 2026 a Thursday.
    let p = calendar_panel(&tr, &d, None, Weekday::Sunday);
    assert_eq!((p.year, p.min_year, p.max_year), (2026, 2025, 2026));
    assert_eq!(p.max_count, 5);
    assert_eq!(p.weekday_labels.join(""), "SMTWTFS");
    let jan1 = &p.days[0];
    assert_eq!(
        (jan1.date, jan1.week, jan1.weekday),
        (
            LocalDate {
                year: 2026,
                month: 1,
                day: 1
            },
            0,
            4
        )
    );
    let today = p.days.last().unwrap();
    assert_eq!(today.date.day, 15);
    assert_eq!(today.count, 5);
    assert_eq!(today.intensity, 1.0);
    assert_eq!(today.query.as_deref(), Some("\"prop:rated=0\""));
    // 1 Jan (Thu) to 15 Nov (Sun): weeks 0 to 46.
    assert_eq!(today.week, 46);
    assert_eq!(today.weekday, 0);
    // One entry per day up to today (no later days).
    assert_eq!(p.days.len(), 319);
    let five_ago = p
        .days
        .iter()
        .find(|x| x.date.month == 11 && x.date.day == 10)
        .unwrap();
    assert_eq!(five_ago.count, 3);
    assert_eq!(five_ago.date.day, 10);
    assert!((five_ago.intensity - (3.0f64 / 5.0).sqrt()).abs() < 1e-12);

    // Monday first moves Sunday to the last row.
    let p = calendar_panel(&tr, &d, None, Weekday::Monday);
    assert_eq!(p.weekday_labels.join(""), "MTWTFSS");
    let today = p.days.last().unwrap();
    assert_eq!((today.week, today.weekday), (45, 6));

    // Last year's calendar starts one year back (history = 1 year).
    let p = calendar_panel(&tr, &d, Some(2025), Weekday::Sunday);
    assert_eq!(p.year, 2025);
    // Days with reviews always show (day -400 is 11 October 2025); empty
    // days only within the last year.
    assert_eq!(
        (p.days[0].date.month, p.days[0].date.day, p.days[0].count),
        (10, 11, 1)
    );
    assert_eq!((p.days[1].date.month, p.days[1].date.day), (11, 16));
    assert!(p.days[1..].iter().all(|x| x.count == 0));
    // Clamped to the allowed years.
    assert_eq!(
        calendar_panel(&tr, &d, Some(1990), Weekday::Sunday).year,
        2025
    );
}

#[test]
fn options_follow_history_and_fsrs_panels() {
    let mut d = data();
    let options = PanelOptions::initial(&d);
    assert_eq!(options.reviews_range, GraphRange::Month);
    assert_eq!(options.hours_range, GraphRange::Year);
    d.history = History::All;
    let mut options = PanelOptions::initial(&d);
    assert_eq!(options.reviews_range, GraphRange::AllTime);
    assert_eq!(options.buttons_range, GraphRange::AllTime);
    options.follow_history(History::Year);
    assert_eq!(options.hours_range, GraphRange::Year);
    assert_eq!(GraphRange::Year.next(false), GraphRange::Month);
    assert_eq!(GraphRange::Year.next(true), GraphRange::AllTime);

    let tr = tr();
    let p = compute(&tr, &d, &options);
    assert!(p.ease.is_some() && p.stability.is_none() && p.difficulty.is_none());
    d.fsrs = true;
    let p = compute(&tr, &d, &options);
    assert!(p.ease.is_none() && p.stability.is_some() && p.retrievability.is_some());
    assert_eq!(p.labels.month, "1 month");
    assert_eq!(p.labels.all_time, "all");
}
