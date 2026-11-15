//! Text and SVG export against fixed aggregates.
use super::*;
use crate::stats::{
    compute_panels_english, ButtonCounts, ButtonsData, CardStateCounts, GraphsData, HourCounts,
    HoursData, LocalDate, PanelOptions, RetentionCounts, ReviewKinds, TodayCounts,
    TrueRetentionData,
};

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
    GraphsData {
        search: "deck:\"A<B\"".into(),
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
        future_due: [(0, 5), (1, 4), (10, 3)].into_iter().collect(),
        review_counts: [(-20, kinds(3, 1, 1, 0, 0)), (0, kinds(1, 2, 1, 5, 0))]
            .into_iter()
            .collect(),
        counts_including_inactive: CardStateCounts {
            new: 10,
            young: 5,
            mature: 5,
            ..Default::default()
        },
        counts_excluding_inactive: CardStateCounts {
            new: 9,
            young: 5,
            mature: 5,
            suspended: 1,
            ..Default::default()
        },
        intervals: [(1, 3), (10, 4), (25, 2)].into_iter().collect(),
        eases: [(250, 10), (230, 2)].into_iter().collect(),
        ease_average: 250.0,
        true_retention: TrueRetentionData {
            today: RetentionCounts {
                young_passed: 5,
                young_failed: 1,
                mature_passed: 3,
                mature_failed: 1,
            },
            ..Default::default()
        },
        hours: HoursData {
            one_month: hours.clone(),
            three_months: hours.clone(),
            one_year: hours.clone(),
            all_time: hours,
        },
        buttons: ButtonsData {
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

fn report(data: GraphsData) -> StatsReport {
    let options = PanelOptions::initial(&data);
    StatsReport {
        panels: compute_panels_english(&data, &options),
        data,
        options,
    }
}

fn fsrs_data() -> GraphsData {
    let mut d = data();
    d.fsrs = true;
    d.stability = d.intervals.clone();
    d.difficulty = [(40, 3), (60, 2)].into_iter().collect();
    d.difficulty_average = 48.0;
    d.retrievability = [(85, 4), (95, 6)].into_iter().collect();
    d.retrievability_average = 91.0;
    d
}

#[test]
fn sparkline_scales_to_eighths() {
    assert_eq!(sparkline(&[0, 1, 2, 4, 8]), " ▁▂▄█");
    assert_eq!(sparkline(&[0, 0]), "  ");
    // A small non-zero value still shows.
    assert_eq!(sparkline(&[1, 100]), "▁█");
    assert_eq!(sparkline(&[]), "");
}

#[test]
fn text_has_every_panel() {
    let r = report(data());
    let text = text(&r);
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
    assert!(
        text.starts_with("Statistics · Search: deck:\"A<B\" · "),
        "{text}"
    );
    assert!(text.contains("Studied 12 cards in 1.6 minutes today (8s/card)"));
    // Future due: days 0..=31 with values on days 0, 1 and 10.
    assert!(text.contains("\n  █▆        ▅\n"), "{text}");
    assert!(text.contains("Due tomorrow: 4 reviews"), "{text}");
    // Card counts with their percentages, and the total.
    assert!(text
        .lines()
        .any(|l| l.contains("New") && l.contains("10") && l.contains("50%")));
    assert!(text.contains("Median interval"));
    // Today's retention row: young 5/6, mature 3/4, total 8/10.
    let today = text
        .lines()
        .find(|l| l.trim_start().starts_with("Today ") && l.contains('%'))
        .unwrap();
    assert!(today.contains("83.3%") && today.contains("75.0%") && today.contains("80.0%"));
    assert!(
        text.contains("Learning: 6/8 correct (75%)") || text.contains("6/8"),
        "{text}"
    );
    assert!(!text.lines().any(|l| l.ends_with(' ')));
    // The calendar: seven rows, today's cell the darkest.
    let calendar: Vec<&str> = text
        .lines()
        .skip_while(|l| !l.starts_with("Calendar"))
        .skip(1)
        .take(7)
        .collect();
    assert_eq!(calendar.len(), 7);
    assert!(calendar.iter().any(|l| l.ends_with('█')), "{calendar:?}");
    assert!(!text.contains("Card Difficulty"));
    assert!(!text.ends_with("\n\n"));
}

#[test]
fn text_shows_fsrs_panels_and_no_data() {
    let text = text(&report(fsrs_data()));
    for title in ["Card Stability", "Card Difficulty", "Card Retrievability"] {
        assert!(text.contains(title), "{title} missing:\n{text}");
    }
    assert!(!text.contains("Card Ease"));
    let empty = report(GraphsData::default());
    let r_empty_no_data = || empty.panels.labels.no_data.clone();
    let text = super::text(&empty);
    // Anki's own scope label ("collection").
    let scope = format!("Statistics · {} · ", empty.panels.labels.collection);
    assert!(text.starts_with(&scope), "{text}");
    assert!(text.contains(&r_empty_no_data()), "{text}");
}

#[test]
fn svg_is_a_standalone_document() {
    let svg = svg(&report(data()));
    assert!(svg.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg xmlns="));
    assert!(svg.ends_with("</svg>\n"));
    // Balanced elements and escaped text.
    assert_eq!(svg.matches("<text").count(), svg.matches("</text>").count());
    assert_eq!(
        svg.matches("<title>").count(),
        svg.matches("</title>").count()
    );
    assert!(svg.contains("deck:&quot;A&lt;B&quot;"));
    assert!(!svg.contains("A<B"));
    for title in [
        "Today",
        "Future Due",
        "Reviews",
        "Card Counts",
        "Review Intervals",
        "Card Ease",
        "Retention",
        "Hourly Breakdown",
        "Answer Buttons",
        "Added",
    ] {
        assert!(svg.contains(&format!(">{title}")), "{title} missing");
    }
    // Desktop's colours: review kinds, card counts, buttons, heatmap.
    for fill in [
        colour::MATURE,
        colour::YOUNG,
        colour::LEARN,
        colour::NEW,
        colour::BUTTONS[0],
        colour::BAR,
        colour::EMPTY,
        colour::HEAT[3],
    ] {
        assert!(svg.contains(&format!("fill=\"{fill}\"")), "{fill} missing");
    }
    // One calendar cell per day of 2026 up to today (Nov 15 = day 319).
    assert_eq!(
        svg.matches(&format!("fill=\"{}\"", colour::EMPTY)).count() + 2,
        319
    );
    // Bars carry the tooltip text.
    assert!(
        svg.contains("5 cards due\nRunning total: 5</title>"),
        "{svg}"
    );
    // The height grows with the content.
    let height: f64 = svg
        .split("height=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .unwrap()
        .parse()
        .unwrap();
    assert!(height > 1500.0, "{height}");
}

#[test]
fn svg_draws_fsrs_panels() {
    let svg = svg(&report(fsrs_data()));
    assert!(svg.contains(">Card Stability"));
    assert!(svg.contains(">Card Difficulty"));
    assert!(svg.contains(">Card Retrievability"));
    assert!(!svg.contains(">Card Ease"));
}
