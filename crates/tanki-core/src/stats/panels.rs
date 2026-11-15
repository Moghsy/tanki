//! The 14 graph panels of desktop's statistics page, in its order
//! (`ts/routes/graphs/+page.svelte`): today, future due, calendar, reviews,
//! card counts, intervals, stability, ease, difficulty, retrievability,
//! true retention, hours, answer buttons, added. Each `*_panel` function is
//! a port of the matching `ts/routes/graphs/<name>.ts`, cited per function.
//! Ease shows only without FSRS; stability, difficulty and retrievability
//! only with FSRS (the `{#if}` guards of the Svelte components).
use super::d3::{self, js_round, Bin};
use super::format::{
    self, day_label, js_num, localized_number, plain, time_span, Unit, DAY, MONTH, YEAR,
};
use super::{
    ButtonCounts, CardStateCounts, GraphsData, History, HourCounts, LocalDate, RetentionCounts,
    ReviewKinds, Weekday,
};
use anki_i18n::I18n;
use chrono::{Datelike, Duration, NaiveDate};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// `GraphRange`: the per-panel range radio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GraphRange {
    #[default]
    Month,
    ThreeMonths,
    Year,
    AllTime,
}

impl GraphRange {
    /// The choices a panel offers; "all time" only with all history for
    /// the review-log panels (`GraphRangeRadios`).
    pub fn choices(all_time: bool) -> &'static [GraphRange] {
        if all_time {
            &[
                GraphRange::Month,
                GraphRange::ThreeMonths,
                GraphRange::Year,
                GraphRange::AllTime,
            ]
        } else {
            &[GraphRange::Month, GraphRange::ThreeMonths, GraphRange::Year]
        }
    }
    /// The next choice, wrapping.
    pub fn next(self, all_time: bool) -> Self {
        let choices = Self::choices(all_time);
        let at = choices.iter().position(|c| *c == self).unwrap_or(0);
        choices[(at + 1) % choices.len()]
    }
    /// `GraphRangeRadios` with `followRevlog`: all history selects all
    /// time; one year leaves all time.
    fn follow(self, history: History) -> Self {
        match (history, self) {
            (History::All, _) => GraphRange::AllTime,
            (History::Year, GraphRange::AllTime) => GraphRange::Year,
            (_, range) => range,
        }
    }
}

/// `IntervalRange` (intervals and stability).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum IntervalRange {
    Month,
    Percentile50,
    #[default]
    Percentile95,
    All,
}

impl IntervalRange {
    pub const CHOICES: [IntervalRange; 4] = [
        IntervalRange::Month,
        IntervalRange::Percentile50,
        IntervalRange::Percentile95,
        IntervalRange::All,
    ];
    pub fn next(self) -> Self {
        let at = Self::CHOICES.iter().position(|c| *c == self).unwrap_or(0);
        Self::CHOICES[(at + 1) % 4]
    }
}

/// `PercentageRangeEnum` (difficulty and retrievability).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PercentRange {
    Percentile50,
    Percentile95,
    Percentile100,
    #[default]
    All,
}

impl PercentRange {
    pub const CHOICES: [PercentRange; 4] = [
        PercentRange::Percentile50,
        PercentRange::Percentile95,
        PercentRange::Percentile100,
        PercentRange::All,
    ];
    pub fn next(self) -> Self {
        let at = Self::CHOICES.iter().position(|c| *c == self).unwrap_or(0);
        Self::CHOICES[(at + 1) % 4]
    }
    /// `PercentageRangeToQuantile`.
    fn quantile(self) -> Option<f64> {
        match self {
            PercentRange::Percentile100 => Some(1.0),
            PercentRange::Percentile95 => Some(0.975),
            PercentRange::Percentile50 => Some(0.75),
            PercentRange::All => None,
        }
    }
}

/// True retention table modes (`TrueRetention.svelte`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RetentionMode {
    Young,
    Mature,
    #[default]
    Summary,
}

/// Every client-side choice of the page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PanelOptions {
    pub future_due_range: GraphRange,
    pub future_due_backlog: bool,
    /// `None`: the current year.
    pub calendar_year: Option<i32>,
    pub first_weekday: Weekday,
    pub reviews_range: GraphRange,
    pub reviews_time: bool,
    pub card_counts_separate_inactive: bool,
    pub intervals_range: IntervalRange,
    pub stability_range: IntervalRange,
    pub difficulty_range: PercentRange,
    pub retrievability_range: PercentRange,
    pub retention_mode: RetentionMode,
    pub hours_range: GraphRange,
    pub buttons_range: GraphRange,
    pub added_range: GraphRange,
}

impl PanelOptions {
    /// Desktop's initial choices for this data (component defaults, the
    /// stored preferences, and `followRevlog`).
    pub fn initial(data: &GraphsData) -> Self {
        let mut options = Self {
            future_due_range: GraphRange::Month,
            future_due_backlog: data.preferences.future_due_show_backlog,
            calendar_year: None,
            first_weekday: data.preferences.first_weekday,
            reviews_range: GraphRange::Month,
            reviews_time: false,
            card_counts_separate_inactive: data.preferences.card_counts_separate_inactive,
            intervals_range: IntervalRange::Percentile95,
            stability_range: IntervalRange::Percentile95,
            difficulty_range: PercentRange::All,
            retrievability_range: PercentRange::All,
            retention_mode: RetentionMode::Summary,
            hours_range: GraphRange::Year,
            buttons_range: GraphRange::Year,
            added_range: GraphRange::Month,
        };
        options.follow_history(data.history);
        options
    }
    /// Applies `followRevlog` after the history window changed.
    pub fn follow_history(&mut self, history: History) {
        self.reviews_range = self.reviews_range.follow(history);
        self.hours_range = self.hours_range.follow(history);
        self.buttons_range = self.buttons_range.follow(history);
    }
}

/// One label/value row of a panel's table (`TableDatum`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableRow {
    pub label: String,
    pub value: String,
}

fn row(label: impl Into<String>, value: impl Into<String>) -> TableRow {
    TableRow {
        label: label.into(),
        value: value.into(),
    }
}

/// One histogram bar: `[x0, x1)` (the last also holds `x1`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistogramBin {
    pub x0: f64,
    pub x1: f64,
    pub value: u64,
    /// Running total up to and including this bin.
    pub cumulative: u64,
    /// Desktop's tooltip text, in plain lines.
    pub label: String,
    /// The browser search desktop runs on click.
    pub query: Option<String>,
}

/// A histogram panel's data (`HistogramData`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Histogram {
    pub bins: Vec<HistogramBin>,
    pub total: u64,
    /// Desktop draws the running total as an area.
    pub cumulative: bool,
    /// X tick labels are percentages.
    pub percent_axis: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TodayPanel {
    pub title: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistogramPanel {
    pub title: String,
    pub subtitle: String,
    /// `None`: no data.
    pub histogram: Option<Histogram>,
    pub table: Vec<TableRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FutureDuePanel {
    pub range: GraphRange,
    pub backlog: bool,
    /// Desktop offers the backlog checkbox only when there is a backlog.
    pub have_backlog: bool,
    pub panel: HistogramPanel,
}

/// One calendar day.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalendarDay {
    /// Week column (0–53) counted from the year's first week.
    pub week: u32,
    /// Row 0–6 counted from the first weekday.
    pub weekday: u32,
    pub date: LocalDate,
    pub count: u64,
    /// `sqrt(count / max)`, the colour scale's input; 0 for no reviews.
    pub intensity: f64,
    /// Date and review count, as desktop's tooltip.
    pub label: String,
    /// `"prop:rated=N"` for days with reviews.
    pub query: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalendarPanel {
    pub title: String,
    pub year: i32,
    pub min_year: i32,
    pub max_year: i32,
    pub first_weekday: Weekday,
    /// One narrow label per row, from the first weekday.
    pub weekday_labels: Vec<String>,
    pub days: Vec<CalendarDay>,
    /// Largest daily count in the loaded history (all years).
    pub max_count: u64,
}

/// The five review kinds, bottom of the stack first (`BinIndex`).
pub const REVIEW_KINDS: [&str; 5] = ["mature", "young", "relearn", "learn", "filtered"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StackedBin {
    pub x0: f64,
    pub x1: f64,
    /// Mature, young, relearn, learn, filtered (`REVIEW_KINDS`).
    pub values: [u64; 5],
    pub cumulative: u64,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewsPanel {
    pub title: String,
    pub subtitle: String,
    pub range: GraphRange,
    pub time: bool,
    /// Labels of `REVIEW_KINDS`.
    pub kind_labels: Vec<String>,
    pub bins: Vec<StackedBin>,
    pub table: Vec<TableRow>,
}

/// The card-count categories in desktop's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CountKind {
    New,
    Learning,
    Relearning,
    Young,
    Mature,
    Suspended,
    Buried,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CountRow {
    pub kind: CountKind,
    pub label: String,
    pub count: u32,
    pub percent: String,
    pub query: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardCountsPanel {
    pub title: String,
    pub separate_inactive: bool,
    /// The shown categories (suspended/buried only when separate).
    pub rows: Vec<CountRow>,
    pub total_label: String,
    pub total: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetentionCell {
    pub passed: u32,
    pub failed: u32,
    /// "92.3%" or "N/A".
    pub retention: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetentionRow {
    pub title: String,
    pub young: RetentionCell,
    pub mature: RetentionCell,
    pub all: RetentionCell,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrueRetentionPanel {
    pub title: String,
    pub subtitle: String,
    pub mode: RetentionMode,
    /// Column headings: young, mature, total, count, pass, fail, retention.
    pub headings: Vec<String>,
    pub rows: Vec<RetentionRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HourRow {
    pub hour: u32,
    pub total: u32,
    pub correct: u32,
    /// Correct share (0–1); 0 without reviews.
    pub ratio: f64,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HoursPanel {
    pub title: String,
    pub subtitle: String,
    pub range: GraphRange,
    pub has_data: bool,
    pub hours: Vec<HourRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ButtonGroup {
    /// "learning", "young" or "mature".
    pub kind: String,
    pub label: String,
    pub counts: [u32; 4],
    pub total: u32,
    pub correct: u32,
    /// Correct percentage text (`localizedNumber`).
    pub percent: String,
    /// "Correct: …" line of the tooltip.
    pub correct_label: String,
    /// "N (P%)" per button.
    pub pressed: [String; 4],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ButtonsPanel {
    pub title: String,
    pub subtitle: String,
    pub range: GraphRange,
    pub has_data: bool,
    /// Again, Hard, Good, Easy.
    pub button_labels: [String; 4],
    pub groups: Vec<ButtonGroup>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RangePanel {
    pub range: GraphRange,
    pub panel: HistogramPanel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntervalPanel {
    pub range: IntervalRange,
    pub panel: HistogramPanel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PercentPanel {
    pub range: PercentRange,
    pub panel: HistogramPanel,
}

/// Localised control labels of the page.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StatsLabels {
    pub page_title: String,
    pub deck: String,
    pub collection: String,
    pub search: String,
    pub one_year_history: String,
    pub all_history: String,
    pub month: String,
    pub three_months: String,
    pub year: String,
    pub all_time: String,
    pub backlog: String,
    pub review_time: String,
    pub separate_inactive: String,
    pub no_data: String,
    pub running_total: String,
}

impl StatsLabels {
    pub fn graph_range(&self, range: GraphRange) -> &str {
        match range {
            GraphRange::Month => &self.month,
            GraphRange::ThreeMonths => &self.three_months,
            GraphRange::Year => &self.year,
            GraphRange::AllTime => &self.all_time,
        }
    }
    pub fn interval_range(&self, range: IntervalRange) -> &str {
        match range {
            IntervalRange::Month => &self.month,
            IntervalRange::Percentile50 => "50%",
            IntervalRange::Percentile95 => "95%",
            IntervalRange::All => &self.all_time,
        }
    }
    pub fn percent_range(&self, range: PercentRange) -> &str {
        match range {
            PercentRange::Percentile50 => "50%",
            PercentRange::Percentile95 => "95%",
            PercentRange::Percentile100 => "100%",
            PercentRange::All => &self.all_time,
        }
    }
}

/// All panels of one report, in page order. FSRS-only and SM-2-only
/// panels are `None` when hidden.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Panels {
    pub fsrs: bool,
    pub history: History,
    pub labels: StatsLabels,
    pub today: TodayPanel,
    pub future_due: FutureDuePanel,
    pub calendar: CalendarPanel,
    pub reviews: ReviewsPanel,
    pub card_counts: CardCountsPanel,
    pub intervals: IntervalPanel,
    pub stability: Option<IntervalPanel>,
    pub ease: Option<HistogramPanel>,
    pub difficulty: Option<PercentPanel>,
    pub retrievability: Option<PercentPanel>,
    pub true_retention: TrueRetentionPanel,
    pub hours: HoursPanel,
    pub buttons: ButtonsPanel,
    pub added: RangePanel,
}

pub(crate) fn compute(tr: &I18n, data: &GraphsData, options: &PanelOptions) -> Panels {
    let fsrs = data.fsrs;
    Panels {
        fsrs,
        history: data.history,
        labels: labels(tr),
        today: today_panel(tr, data),
        future_due: future_due_panel(
            tr,
            data,
            options.future_due_range,
            options.future_due_backlog,
        ),
        calendar: calendar_panel(tr, data, options.calendar_year, options.first_weekday),
        reviews: reviews_panel(tr, data, options.reviews_range, options.reviews_time),
        card_counts: card_counts_panel(tr, data, options.card_counts_separate_inactive),
        intervals: IntervalPanel {
            range: options.intervals_range,
            panel: interval_panel(tr, &data.intervals, options.intervals_range, false),
        },
        stability: fsrs.then(|| IntervalPanel {
            range: options.stability_range,
            panel: interval_panel(tr, &data.stability, options.stability_range, true),
        }),
        ease: (!fsrs).then(|| ease_panel(tr, data)),
        difficulty: fsrs.then(|| PercentPanel {
            range: options.difficulty_range,
            panel: difficulty_panel(tr, data, options.difficulty_range),
        }),
        retrievability: fsrs.then(|| PercentPanel {
            range: options.retrievability_range,
            panel: retrievability_panel(tr, data, options.retrievability_range),
        }),
        true_retention: true_retention_panel(tr, data, options.retention_mode),
        hours: hours_panel(tr, data, options.hours_range),
        buttons: buttons_panel(tr, data, options.buttons_range),
        added: RangePanel {
            range: options.added_range,
            panel: added_panel(tr, data, options.added_range),
        },
    }
}

fn labels(tr: &I18n) -> StatsLabels {
    StatsLabels {
        page_title: plain(tr.statistics_title()),
        deck: plain(tr.statistics_range_deck()),
        collection: plain(tr.statistics_range_collection()),
        search: plain(tr.statistics_range_search()),
        one_year_history: plain(tr.statistics_range_1_year_history()),
        all_history: plain(tr.statistics_range_all_history()),
        month: format::span(tr, MONTH),
        three_months: format::span(tr, 3.0 * MONTH),
        year: format::span(tr, YEAR),
        all_time: plain(tr.statistics_range_all_time()),
        backlog: plain(tr.statistics_backlog_checkbox()),
        review_time: plain(tr.statistics_reviews_time_checkbox()),
        separate_inactive: plain(tr.statistics_counts_separate_suspended_buried_cards()),
        no_data: plain(tr.statistics_no_data()),
        running_total: plain(tr.statistics_running_total()),
    }
}

/// The sum of a numeric-map bin (`getNumericMapBinValue`).
fn add_u32(sum: &mut u64, value: &u32) {
    *sum += *value as u64;
}

/// Bins to histogram bars with running totals.
fn bars(
    bins: &[Bin<u64>],
    mut label: impl FnMut(&Bin<u64>, u64) -> String,
    mut query: impl FnMut(&Bin<u64>) -> Option<String>,
) -> Vec<HistogramBin> {
    let mut running = 0;
    bins.iter()
        .map(|bin| {
            running += bin.sum;
            HistogramBin {
                x0: bin.x0,
                x1: bin.x1,
                value: bin.sum,
                cumulative: running,
                label: label(bin, running),
                query: query(bin),
            }
        })
        .collect()
}

/// `today.ts` `gatherData`.
pub(crate) fn today_panel(tr: &I18n, data: &GraphsData) -> TodayPanel {
    let today = &data.today;
    let lines = if today.answer_count > 0 {
        let studied =
            format::studied_today(tr, today.answer_count, today.answer_millis as f64 / 1000.0);
        let again = today.answer_count.saturating_sub(today.correct_count);
        let again_text = format!(
            "{} {} ({}%)",
            plain(tr.statistics_today_again_count()),
            again,
            localized_number(again as f64 / today.answer_count as f64 * 100.0, 2)
        );
        let types = plain(tr.statistics_today_type_counts(
            today.learn_count,
            today.review_count,
            today.relearn_count,
            today.early_review_count,
        ));
        let mature = if today.mature_count > 0 {
            plain(tr.statistics_today_correct_mature(
                today.mature_correct,
                today.mature_count,
                localized_number(
                    today.mature_correct as f64 / today.mature_count as f64 * 100.0,
                    2,
                ),
            ))
        } else {
            plain(tr.statistics_today_no_mature_cards())
        };
        vec![studied, again_text, types, mature]
    } else {
        vec![plain(tr.statistics_today_no_cards())]
    };
    TodayPanel {
        title: plain(tr.statistics_today_title()),
        lines,
    }
}

/// `future-due.ts` `buildHistogram`.
pub(crate) fn future_due_panel(
    tr: &I18n,
    data: &GraphsData,
    range: GraphRange,
    backlog: bool,
) -> FutureDuePanel {
    let mut panel = HistogramPanel {
        title: plain(tr.statistics_future_due_title()),
        subtitle: plain(tr.statistics_future_due_subtitle()),
        histogram: None,
        table: Vec::new(),
    };
    let counts: BTreeMap<i32, u32> = data
        .future_due
        .iter()
        .filter(|(day, _)| backlog || **day >= 0)
        .map(|(k, v)| (*k, *v))
        .collect();
    let result = FutureDuePanel {
        range,
        backlog,
        have_backlog: data.have_backlog,
        panel: panel.clone(),
    };
    let key_min = counts.keys().next().map(|k| *k as f64);
    let key_max = counts.keys().next_back().map(|k| *k as f64);
    let x_min = if backlog { key_min } else { Some(0.0) };
    let x_max = match range {
        GraphRange::Month => Some(31.0),
        GraphRange::ThreeMonths => Some(90.0),
        GraphRange::Year => Some(365.0),
        GraphRange::AllTime => key_max,
    };
    let (Some(x_min), Some(x_max)) = (x_min, x_max) else {
        return result;
    };
    let desired = (x_max - x_min).min(70.0);
    let thresholds = d3::ticks(x_min, x_max, desired);
    let bins = d3::bin(
        (x_min, x_max),
        &thresholds,
        counts.iter().map(|(k, v)| (*k as f64, *v)),
        add_u32,
    );
    if bins.iter().all(|b| b.len == 0) {
        return result;
    }
    let total: u64 = bins.iter().map(|b| b.sum).sum();
    let bars = bars(
        &bins,
        |bin, running| {
            let end = if bin.x1 == x_max {
                bin.x1 + 1.0
            } else {
                bin.x1
            };
            format!(
                "{}:\n{}\n{}: {}",
                day_label(tr, bin.x0, end),
                plain(tr.statistics_cards_due(bin.sum as u32)),
                plain(tr.statistics_running_total()),
                localized_number(running as f64, 2)
            )
        },
        |bin| {
            let start = bin.x0;
            let end = if bin.x1 == x_max {
                bin.x1
            } else {
                bin.x1 - 1.0
            };
            Some(if start == end {
                format!("\"prop:due={}\"", js_num(start))
            } else {
                format!(
                    "\"prop:due>={}\" AND \"prop:due<={}\"",
                    js_num(start),
                    js_num(end)
                )
            })
        },
    );
    let period_days = x_max - x_min;
    let per_day = if period_days > 0.0 {
        js_round(total as f64 / period_days)
    } else {
        total as f64
    };
    panel.table = vec![
        row(
            plain(tr.statistics_total()),
            plain(tr.statistics_reviews(total)),
        ),
        row(
            plain(tr.statistics_average()),
            plain(tr.statistics_reviews_per_day(per_day as i64)),
        ),
        row(
            plain(tr.statistics_due_tomorrow()),
            plain(tr.statistics_reviews(data.future_due.get(&1).copied().unwrap_or_default())),
        ),
        row(
            plain(tr.statistics_daily_load()),
            plain(tr.statistics_reviews_per_day(data.daily_load)),
        ),
    ];
    panel.histogram = Some(Histogram {
        bins: bars,
        total,
        cumulative: true,
        percent_axis: false,
    });
    FutureDuePanel { panel, ..result }
}

/// `calendar.ts` `gatherData` + `renderCalendar` (layout only).
pub(crate) fn calendar_panel(
    tr: &I18n,
    data: &GraphsData,
    year: Option<i32>,
    first_weekday: Weekday,
) -> CalendarPanel {
    let today = data.today_date.naive();
    let day_zero = data.day_zero_date.naive();
    let max_year = today.year();
    // CalendarGraph.svelte: one year of history reaches into last year;
    // all history goes back to 2000. tanki stops at the earliest year with
    // reviews instead of offering empty years back to 2000.
    let min_year = match data.history {
        History::Year => max_year - 1,
        History::All => data
            .review_counts
            .keys()
            .next()
            .map(|day| (day_zero + Duration::days(*day as i64)).year())
            .unwrap_or(max_year)
            .clamp(2000, max_year),
    };
    let target = year.unwrap_or(max_year).clamp(min_year, max_year);
    let first = first_weekday.number();
    let week_of = |date: NaiveDate| -> (u32, u32) {
        let jan1 = NaiveDate::from_ymd_opt(date.year(), 1, 1).unwrap_or(date);
        let floor = |d: NaiveDate| {
            let offset = (d.weekday().num_days_from_sunday() + 7 - first) % 7;
            d - Duration::days(offset as i64)
        };
        let week = ((floor(date) - floor(jan1)).num_days() / 7) as u32;
        let weekday = (date.weekday().num_days_from_sunday() + 7 - first) % 7;
        (week, weekday)
    };
    let mut max_count = 0;
    let mut by_year_day: BTreeMap<u32, CalendarDay> = BTreeMap::new();
    for (day, kinds) in &data.review_counts {
        let count = kinds.total();
        max_count = max_count.max(count);
        let date = day_zero + Duration::days(*day as i64);
        if date.year() != target {
            continue;
        }
        let (week, weekday) = week_of(date);
        by_year_day.insert(
            date.ordinal0(),
            CalendarDay {
                week,
                weekday,
                date: LocalDate::from_naive(date),
                count,
                intensity: 0.0,
                label: String::new(),
                query: (count > 0).then(|| format!("\"prop:rated={day}\"")),
            },
        );
    }
    let start = NaiveDate::from_ymd_opt(target, 1, 1).unwrap_or(today);
    let year_ago = today
        .with_year(today.year() - 1)
        .unwrap_or(today - Duration::days(365));
    // As desktop: 365 days from 1 January (31 December of a leap year shows
    // only with reviews).
    for i in 0..365 {
        let date = start + Duration::days(i);
        if date > today {
            continue;
        }
        if data.history == History::Year && date <= year_ago {
            continue;
        }
        by_year_day.entry(date.ordinal0()).or_insert_with(|| {
            let (week, weekday) = week_of(date);
            CalendarDay {
                week,
                weekday,
                date: LocalDate::from_naive(date),
                count: 0,
                intensity: 0.0,
                label: String::new(),
                query: None,
            }
        });
    }
    let days = by_year_day
        .into_values()
        .map(|mut day| {
            if max_count > 0 && day.count > 0 {
                day.intensity = (day.count as f64 / max_count as f64).sqrt();
            }
            day.label = format!(
                "{}: {}",
                day.date.naive().format("%A, %B %-d, %Y"),
                plain(tr.statistics_reviews(day.count))
            );
            day
        })
        .collect::<Vec<_>>();
    const NARROW: [&str; 7] = ["S", "M", "T", "W", "T", "F", "S"];
    CalendarPanel {
        title: plain(tr.statistics_calendar_title()),
        year: target,
        min_year,
        max_year,
        first_weekday,
        weekday_labels: (0..7)
            .map(|i| NARROW[((first + i) % 7) as usize].to_owned())
            .collect(),
        days: if max_count == 0 { Vec::new() } else { days },
        max_count,
    }
}

fn kinds_array(k: &ReviewKinds) -> [u64; 5] {
    [
        k.mature as u64,
        k.young as u64,
        k.relearn as u64,
        k.learn as u64,
        k.filtered as u64,
    ]
}

/// `reviews.ts` `renderReviews` (bins and table).
pub(crate) fn reviews_panel(
    tr: &I18n,
    data: &GraphsData,
    range: GraphRange,
    time: bool,
) -> ReviewsPanel {
    let kind_labels = vec![
        plain(tr.statistics_counts_mature_cards()),
        plain(tr.statistics_counts_young_cards()),
        plain(tr.statistics_counts_relearning_cards()),
        plain(tr.statistics_counts_learning_cards()),
        plain(tr.statistics_counts_filtered_cards()),
    ];
    let mut panel = ReviewsPanel {
        title: plain(tr.statistics_reviews_title()),
        subtitle: plain(if time {
            tr.statistics_reviews_time_subtitle()
        } else {
            tr.statistics_reviews_count_subtitle()
        }),
        range,
        time,
        kind_labels,
        bins: Vec::new(),
        table: Vec::new(),
    };
    let x_max = 1.0;
    let x_min = match range {
        GraphRange::Month => Some(-30.0),
        GraphRange::ThreeMonths => Some(-89.0),
        GraphRange::Year => Some(-364.0),
        GraphRange::AllTime => data.review_counts.keys().next().map(|k| *k as f64),
    };
    let Some(mut x_min) = x_min else {
        return panel;
    };
    let desired = x_min.abs().min(70.0);
    let unbound = range == GraphRange::AllTime;
    let original_x_min = x_min;
    let mut thresholds = d3::ticks(x_min, x_max, desired);
    if unbound && thresholds.len() >= 2 {
        let spacing = thresholds[1] - thresholds[0];
        let partial = thresholds[0] - x_min;
        if spacing > 0.0 && partial > 0.0 && partial < spacing {
            x_min = thresholds[0] - spacing;
            thresholds = d3::ticks(x_min, x_max, desired);
        }
    }
    if matches!(range, GraphRange::Year | GraphRange::AllTime) {
        let mut shifted: Vec<f64> = thresholds.iter().map(|t| (t + 1.0).min(1.0)).collect();
        shifted.sort_by(|a, b| a.total_cmp(b));
        shifted.dedup();
        thresholds = shifted;
    }
    let source = if time {
        &data.review_times
    } else {
        &data.review_counts
    };
    let bins = d3::bin(
        (x_min, x_max),
        &thresholds,
        source.iter().map(|(k, v)| (*k as f64, kinds_array(v))),
        |sum: &mut [u64; 5], v: &[u64; 5]| {
            for (s, v) in sum.iter_mut().zip(v) {
                *s += v;
            }
        },
    );
    let studied_days: usize = bins.iter().map(|b| b.len).sum();
    if studied_days == 0 {
        return panel;
    }
    let value_label = |n: u64| -> String {
        if time {
            time_span(tr, n as f64 / 1000.0, false, true, Unit::Hours)
        } else {
            plain(tr.statistics_reviews(n))
        }
    };
    let first_x0 = bins[0].x0;
    let mut running = 0;
    panel.bins = bins
        .iter()
        .map(|bin| {
            let total: u64 = bin.sum.iter().sum();
            running += total;
            let start = if bin.x0 == first_x0 {
                original_x_min
            } else {
                bin.x0.floor()
            };
            let end = if bin.x1 == 0.0 { 1.0 } else { bin.x1 };
            StackedBin {
                x0: bin.x0,
                x1: bin.x1,
                values: bin.sum,
                cumulative: running,
                label: format!("{}: {}", day_label(tr, start, end), value_label(total)),
            }
        })
        .collect();
    let total = running;
    let period_days = -original_x_min + 1.0;
    let studied_percent = studied_days as f64 / period_days * 100.0;
    let period_avg = total as f64 / period_days;
    let studied_avg = total as f64 / studied_days as f64;
    let percent = if studied_percent < 99.5 {
        localized_number(studied_percent, 0)
    } else if studied_percent < 99.95 {
        localized_number(studied_percent, 1)
    } else if studied_percent < 100.0 {
        localized_number(studied_percent, 2)
    } else {
        "100".to_owned()
    };
    let (total_text, for_studied, for_period) = if time {
        (
            time_span(tr, total as f64 / 1000.0, false, true, Unit::Hours),
            plain(tr.statistics_minutes_per_day(js_round(studied_avg / 1000.0 / 60.0) as i64)),
            plain(tr.statistics_minutes_per_day(js_round(period_avg / 1000.0 / 60.0) as i64)),
        )
    } else {
        (
            plain(tr.statistics_reviews(total)),
            plain(tr.statistics_reviews_per_day(js_round(studied_avg) as i64)),
            plain(tr.statistics_reviews_per_day(js_round(period_avg) as i64)),
        )
    };
    panel.table = vec![
        row(
            plain(tr.statistics_days_studied()),
            plain(tr.statistics_amount_of_total_with_percentage(
                studied_days as u64,
                period_days as i64,
                percent,
            )),
        ),
        row(plain(tr.statistics_total()), total_text),
        row(plain(tr.statistics_average_over_period()), for_period),
    ];
    if studied_percent < 100.0 {
        panel.table.push(row(
            plain(tr.statistics_average_for_days_studied()),
            for_studied,
        ));
    }
    if time {
        // The review count over the same domain (`countBins`).
        let reviews: u64 = data
            .review_counts
            .iter()
            .filter(|(k, _)| x_min <= **k as f64 && **k as f64 <= x_max)
            .map(|(_, v)| v.total())
            .sum();
        let secs = total as f64 / 1000.0;
        let average = secs / reviews as f64;
        let per_minute = reviews as f64 * 60.0 / secs;
        panel.table.push(row(
            plain(tr.statistics_average_answer_time_label()),
            plain(tr.statistics_average_answer_time(average as f32, per_minute as f32)),
        ));
    }
    panel
}

/// `card-counts.ts` `gatherData` + `renderCards` (table).
pub(crate) fn card_counts_panel(
    tr: &I18n,
    data: &GraphsData,
    separate_inactive: bool,
) -> CardCountsPanel {
    let c: &CardStateCounts = if separate_inactive {
        &data.counts_excluding_inactive
    } else {
        &data.counts_including_inactive
    };
    let extra = if separate_inactive {
        "AND -(\"is:buried\" OR \"is:suspended\")"
    } else {
        ""
    };
    let all = [
        (
            CountKind::New,
            plain(tr.statistics_counts_new_cards()),
            c.new,
            true,
            format!("\"is:new\"{extra}"),
        ),
        (
            CountKind::Learning,
            plain(tr.statistics_counts_learning_cards()),
            c.learn,
            true,
            format!("(-\"is:review\" AND \"is:learn\"){extra}"),
        ),
        (
            CountKind::Relearning,
            plain(tr.statistics_counts_relearning_cards()),
            c.relearn,
            true,
            format!("(\"is:review\" AND \"is:learn\"){extra}"),
        ),
        (
            CountKind::Young,
            plain(tr.statistics_counts_young_cards()),
            c.young,
            true,
            format!("(\"is:review\" AND -\"is:learn\") AND \"prop:ivl<21\"{extra}"),
        ),
        (
            CountKind::Mature,
            plain(tr.statistics_counts_mature_cards()),
            c.mature,
            true,
            format!("(\"is:review\" -\"is:learn\") AND \"prop:ivl>=21\"{extra}"),
        ),
        (
            CountKind::Suspended,
            plain(tr.statistics_counts_suspended_cards()),
            c.suspended,
            separate_inactive,
            "\"is:suspended\"".to_owned(),
        ),
        (
            CountKind::Buried,
            plain(tr.statistics_counts_buried_cards()),
            c.buried,
            separate_inactive,
            "\"is:buried\"".to_owned(),
        ),
    ];
    let sum: u64 = all.iter().map(|e| e.2 as u64).sum();
    let x_max = sum.max(1) as f64;
    CardCountsPanel {
        title: plain(tr.statistics_counts_title()),
        separate_inactive,
        rows: all
            .into_iter()
            .filter(|e| e.3)
            .map(|(kind, label, count, _, query)| CountRow {
                kind,
                label,
                count,
                percent: format!("{}%", localized_number(count as f64 / x_max * 100.0, 2)),
                query,
            })
            .collect(),
        total_label: plain(tr.statistics_counts_total_cards()),
        total: localized_number(sum as f64, 2),
    }
}

/// `(value, count)` pairs in ascending order.
fn sorted_counts(map: &BTreeMap<u32, u32>) -> Vec<(f64, u64)> {
    map.iter()
        .filter(|(_, c)| **c > 0)
        .map(|(k, c)| (*k as f64, *c as u64))
        .collect()
}

/// `intervals.ts` `prepareIntervalData` (`fsrs` = the stability graph).
pub(crate) fn interval_panel(
    tr: &I18n,
    map: &BTreeMap<u32, u32>,
    range: IntervalRange,
    fsrs: bool,
) -> HistogramPanel {
    let mut panel = HistogramPanel {
        title: plain(if fsrs {
            tr.statistics_card_stability_title()
        } else {
            tr.statistics_intervals_title()
        }),
        subtitle: plain(if fsrs {
            tr.statistics_card_stability_subtitle()
        } else {
            tr.statistics_intervals_subtitle()
        }),
        histogram: None,
        table: Vec::new(),
    };
    let sorted = sorted_counts(map);
    let Some(&(largest, _)) = sorted.last() else {
        return panel;
    };
    let x_min = 0.0;
    let mut nice = false;
    let mut x_max = match range {
        IntervalRange::Month => largest.min(30.0),
        IntervalRange::Percentile50 => {
            nice = true;
            d3::quantile_counts(&sorted, 0.5).unwrap_or_default()
        }
        IntervalRange::Percentile95 => {
            nice = true;
            d3::quantile_counts(&sorted, 0.95).unwrap_or_default()
        }
        IntervalRange::All => {
            nice = true;
            largest
        }
    };
    x_max += 1.0;
    let increment = |x: f64| if fsrs { x } else { x + 1.0 };
    let desired = (x_max - x_min).min(70.0);
    let (d0, d1) = if nice {
        d3::nice(x_min, x_max)
    } else {
        (x_min, x_max)
    };
    let domain = (increment(d0), increment(d1));
    let ticks = d3::ticks(domain.0, domain.1, desired);
    let mut thresholds = Vec::new();
    if let Some(first) = ticks.first().copied() {
        for (i, x) in ticks.iter().enumerate() {
            thresholds.push(x - (first - 1.0));
            if i == ticks.len() - 1 {
                thresholds.push(x + 1.0);
            }
        }
    }
    let bins = d3::bin(
        domain,
        &thresholds,
        sorted.iter().map(|(v, c)| (*v, *c)),
        |sum: &mut u64, count: &u64| *sum += count,
    );
    // Each entry stands for `count` cards: d3's `bin.length` over the
    // expanded array is the summed count.
    let total: u64 = bins.iter().map(|b| b.sum).sum();
    if total == 0 {
        return panel;
    }
    let bars = bars(
        &bins,
        |bin, running| {
            let cards = bin.sum;
            let interval = if bin.x1 - bin.x0 <= 1.0 {
                if fsrs {
                    tr.statistics_stability_day_single(cards, bin.x0)
                } else {
                    tr.statistics_intervals_day_single(cards, bin.x0)
                }
            } else if fsrs {
                tr.statistics_stability_day_range(cards, bin.x0 as i64, (bin.x1 - 1.0) as i64)
            } else {
                tr.statistics_intervals_day_range(cards, bin.x0 as i64, (bin.x1 - 1.0) as i64)
            };
            format!(
                "{}\n{}: {}%",
                plain(interval),
                plain(tr.statistics_running_total()),
                localized_number(running as f64 / total as f64 * 100.0, 1)
            )
        },
        |bin| {
            let mut start = bin.x0;
            let end = bin.x1 - 1.0;
            Some(if fsrs {
                if start == 0.0 {
                    start = 0.5;
                }
                format!(
                    "\"prop:s>={}\" \"prop:s<{}\"",
                    js_num(start - 0.5),
                    js_num(end + 0.5)
                )
            } else if start == end {
                format!("\"prop:ivl={}\"", js_num(start))
            } else {
                format!(
                    "\"prop:ivl>={}\" \"prop:ivl<={}\"",
                    js_num(start),
                    js_num(end)
                )
            })
        },
    );
    let median = js_round(d3::quantile_counts(&sorted, 0.5).unwrap_or_default());
    panel.table = vec![row(
        plain(if fsrs {
            tr.statistics_median_stability()
        } else {
            tr.statistics_median_interval()
        }),
        time_span(tr, median * DAY, false, false, Unit::Years),
    )];
    panel.histogram = Some(Histogram {
        bins: bars,
        total,
        cumulative: true,
        percent_axis: false,
    });
    panel
}

/// `getAdjustedScaleAndTicks`; `min_bin` is the `percentageRange.ts`
/// variant (bins at least one percent wide), off for `ease.ts`'s copy.
fn adjusted_scale_and_ticks(
    min: f64,
    max: f64,
    desired: f64,
    min_bin: bool,
) -> ((f64, f64), Vec<f64>) {
    let predomain = d3::nice(min, max);
    let mut ticks = d3::ticks(predomain.0, predomain.1, desired);
    let min_offset = min - predomain.0;
    let mut tick_size = if ticks.len() >= 2 {
        ticks[1] - ticks[0]
    } else {
        f64::NAN
    };
    if min_bin && tick_size < 1.0 {
        ticks = d3::range(min, max, 1.0);
        tick_size = 1.0;
    }
    if min_offset == 0.0 || (min_offset % tick_size != 0.0 && tick_size % min_offset != 0.0) {
        return (predomain, ticks);
    }
    (
        (predomain.0 + min_offset, predomain.1 + min_offset),
        ticks.into_iter().map(|t| t + min_offset).collect(),
    )
}

/// `percentageRange.ts` `easeQuantile`.
fn ease_quantile(map: &BTreeMap<u32, u32>, quantile: f64) -> Option<f64> {
    let mut count = map.values().map(|v| *v as f64).sum::<f64>() * quantile;
    for (key, value) in map {
        count -= *value as f64;
        if count <= 0.0 {
            return Some(*key as f64);
        }
    }
    None
}

fn percent_histogram(
    map: &BTreeMap<u32, u32>,
    domain: (f64, f64),
    ticks: &[f64],
    label: impl Fn(&Bin<u64>) -> String,
    query: impl Fn(f64, f64) -> String,
) -> Histogram {
    let bins = d3::bin(
        domain,
        ticks,
        map.iter().map(|(k, v)| (*k as f64, *v)),
        add_u32,
    );
    let total = bins.iter().map(|b| b.sum).sum();
    Histogram {
        bins: bars(
            &bins,
            |bin, _| label(bin),
            |bin| Some(query(bin.x0, bin.x1 - 1.0)),
        ),
        total,
        cumulative: false,
        percent_axis: true,
    }
}

fn percent(n: f64) -> String {
    format!("{}%", localized_number(n, 0))
}

/// `ease.ts` `prepareData`.
pub(crate) fn ease_panel(tr: &I18n, data: &GraphsData) -> HistogramPanel {
    let mut panel = HistogramPanel {
        title: plain(tr.statistics_card_ease_title()),
        subtitle: plain(tr.statistics_card_ease_subtitle()),
        histogram: None,
        table: Vec::new(),
    };
    let Some(&largest) = data.eases.keys().next_back() else {
        return panel;
    };
    let (domain, ticks) = adjusted_scale_and_ticks(130.0, largest as f64 + 1.0, 20.0, false);
    panel.histogram = Some(percent_histogram(
        &data.eases,
        domain,
        &ticks,
        |bin| {
            let range = if bin.x1.floor() - bin.x0.floor() <= 10.0 {
                format!("{}%", js_num(bin.x0))
            } else {
                format!("{}%-{}%", js_num(bin.x0), js_num(bin.x1))
            };
            plain(tr.statistics_card_ease_tooltip(bin.sum as u32, range))
        },
        |start, end| {
            if start == end {
                format!("\"prop:ease={}\"", js_num(start / 100.0))
            } else {
                format!(
                    "\"prop:ease>={}\" AND \"prop:ease<{}\"",
                    js_num(start / 100.0),
                    js_num((end + 1.0) / 100.0)
                )
            }
        },
    ));
    panel.table = vec![row(
        plain(tr.statistics_median_ease()),
        percent(data.ease_average as f64),
    )];
    panel
}

/// `difficulty.ts`/`retrievability.ts` share this shape.
fn percent_range_panel(
    map: &BTreeMap<u32, u32>,
    range: PercentRange,
    label: impl Fn(&Bin<u64>) -> String,
    prop: &str,
) -> Option<Histogram> {
    if map.is_empty() {
        return None;
    }
    let (min, max) = match range.quantile() {
        Some(q) => (
            ease_quantile(map, 1.0 - q).unwrap_or(0.0),
            ease_quantile(map, q).unwrap_or(0.0),
        ),
        None => (0.0, 100.0),
    };
    let (domain, ticks) = adjusted_scale_and_ticks(min, max, 20.0, true);
    Some(percent_histogram(
        map,
        domain,
        &ticks,
        label,
        |start, end| {
            let till = if end == 99.0 { "<=" } else { "<" };
            format!(
                "\"prop:{prop}>={}\" AND \"prop:{prop}{till}{}\"",
                js_num(start / 100.0),
                js_num((end + 1.0) / 100.0)
            )
        },
    ))
}

/// `difficulty.ts` `prepareData`.
pub(crate) fn difficulty_panel(
    tr: &I18n,
    data: &GraphsData,
    range: PercentRange,
) -> HistogramPanel {
    let histogram = percent_range_panel(
        &data.difficulty,
        range,
        |bin| {
            plain(tr.statistics_card_difficulty_tooltip(
                bin.sum as u32,
                format!("{}%-{}%", js_num(bin.x0), js_num(bin.x1)),
            ))
        },
        "d",
    );
    HistogramPanel {
        title: plain(tr.statistics_card_difficulty_title()),
        subtitle: plain(tr.statistics_card_difficulty_subtitle2()),
        table: if histogram.is_some() {
            vec![row(
                plain(tr.statistics_median_difficulty()),
                percent(data.difficulty_average as f64),
            )]
        } else {
            Vec::new()
        },
        histogram,
    }
}

/// `retrievability.ts` `prepareData`.
pub(crate) fn retrievability_panel(
    tr: &I18n,
    data: &GraphsData,
    range: PercentRange,
) -> HistogramPanel {
    let histogram = percent_range_panel(
        &data.retrievability,
        range,
        |bin| {
            plain(tr.statistics_retrievability_tooltip(
                bin.sum as u32,
                format!("{}%-{}%", js_num(bin.x0), js_num(bin.x1)),
            ))
        },
        "r",
    );
    HistogramPanel {
        title: plain(tr.statistics_card_retrievability_title()),
        subtitle: plain(tr.statistics_retrievability_subtitle()),
        table: if histogram.is_some() {
            vec![
                row(
                    plain(tr.statistics_average_retrievability()),
                    percent(data.retrievability_average as f64),
                ),
                row(
                    plain(tr.statistics_estimated_total_knowledge()),
                    format!(
                        "{} / {}",
                        plain(tr.statistics_cards(
                            js_round(data.retrievability_sum_by_card as f64) as i64
                        )),
                        plain(tr.statistics_notes(
                            js_round(data.retrievability_sum_by_note as f64) as i64
                        ))
                    ),
                ),
            ]
        } else {
            Vec::new()
        },
        histogram,
    }
}

/// `true-retention.ts` `calculateRetentionPercentageString`.
fn retention_text(tr: &I18n, passed: u32, failed: u32) -> String {
    let total = passed as u64 + failed as u64;
    if total == 0 {
        return plain(tr.statistics_true_retention_not_applicable());
    }
    format!("{:.1}%", passed as f64 / total as f64 * 100.0)
}

/// `true-retention.ts` `getRowData` and the three table views.
pub(crate) fn true_retention_panel(
    tr: &I18n,
    data: &GraphsData,
    mode: RetentionMode,
) -> TrueRetentionPanel {
    let t = &data.true_retention;
    let mut periods = vec![
        (plain(tr.statistics_true_retention_today()), t.today),
        (plain(tr.statistics_true_retention_yesterday()), t.yesterday),
        (plain(tr.statistics_true_retention_week()), t.week),
        (plain(tr.statistics_true_retention_month()), t.month),
        (plain(tr.statistics_true_retention_year()), t.year),
    ];
    if data.history == History::All {
        periods.push((plain(tr.statistics_true_retention_all_time()), t.all_time));
    }
    let cell = |passed: u32, failed: u32| RetentionCell {
        passed,
        failed,
        retention: retention_text(tr, passed, failed),
    };
    TrueRetentionPanel {
        title: plain(tr.statistics_true_retention_title()),
        subtitle: plain(tr.statistics_true_retention_subtitle()),
        mode,
        headings: vec![
            plain(tr.statistics_true_retention_young()),
            plain(tr.statistics_true_retention_mature()),
            plain(tr.statistics_true_retention_total()),
            plain(tr.statistics_true_retention_count()),
            plain(tr.statistics_true_retention_pass()),
            plain(tr.statistics_true_retention_fail()),
            plain(tr.statistics_true_retention_retention()),
        ],
        rows: periods
            .into_iter()
            .map(|(title, r): (String, RetentionCounts)| RetentionRow {
                title,
                young: cell(r.young_passed, r.young_failed),
                mature: cell(r.mature_passed, r.mature_failed),
                all: cell(
                    r.young_passed + r.mature_passed,
                    r.young_failed + r.mature_failed,
                ),
            })
            .collect(),
    }
}

/// `hours.ts` `renderHours` (data and tooltips).
pub(crate) fn hours_panel(tr: &I18n, data: &GraphsData, range: GraphRange) -> HoursPanel {
    let source: &[HourCounts] = match range {
        GraphRange::Month => &data.hours.one_month,
        GraphRange::ThreeMonths => &data.hours.three_months,
        GraphRange::Year => &data.hours.one_year,
        GraphRange::AllTime => &data.hours.all_time,
    };
    let hours: Vec<HourRow> = source
        .iter()
        .enumerate()
        .map(|(hour, h)| {
            let hour = hour as u32;
            let ratio = if h.total > 0 {
                h.correct as f64 / h.total as f64
            } else {
                0.0
            };
            HourRow {
                hour,
                total: h.total,
                correct: h.correct,
                ratio,
                label: format!(
                    "{}\n{}\n{}",
                    plain(tr.statistics_hours_range(hour, hour + 1)),
                    plain(tr.statistics_hours_reviews(h.total)),
                    plain(tr.statistics_hours_correct_reviews(
                        localized_number(ratio * 100.0, 2),
                        h.correct
                    ))
                ),
            }
        })
        .collect();
    HoursPanel {
        title: plain(tr.statistics_hours_title()),
        subtitle: plain(tr.statistics_hours_subtitle()),
        range,
        has_data: hours.iter().any(|h| h.total > 0),
        hours,
    }
}

/// `buttons.ts` `renderButtons` (totals and tooltips).
pub(crate) fn buttons_panel(tr: &I18n, data: &GraphsData, range: GraphRange) -> ButtonsPanel {
    let source: &ButtonCounts = match range {
        GraphRange::Month => &data.buttons.one_month,
        GraphRange::ThreeMonths => &data.buttons.three_months,
        GraphRange::Year => &data.buttons.one_year,
        GraphRange::AllTime => &data.buttons.all_time,
    };
    let group = |kind: &str, label: String, counts: [u32; 4]| {
        let total: u32 = counts.iter().sum();
        let correct: u32 = counts[1..].iter().sum();
        let share = |n: u32| {
            if total > 0 {
                localized_number(n as f64 / total as f64 * 100.0, 2)
            } else {
                "0".to_owned()
            }
        };
        let percent = share(correct);
        ButtonGroup {
            kind: kind.to_owned(),
            label,
            counts,
            total,
            correct,
            correct_label: plain(tr.statistics_hours_correct(correct, total, percent.clone())),
            percent,
            pressed: counts.map(|n| format!("{} ({}%)", localized_number(n as f64, 2), share(n))),
        }
    };
    let groups = vec![
        group(
            "learning",
            plain(tr.statistics_counts_learning_cards()),
            source.learning,
        ),
        group(
            "young",
            plain(tr.statistics_counts_young_cards()),
            source.young,
        ),
        group(
            "mature",
            plain(tr.statistics_counts_mature_cards()),
            source.mature,
        ),
    ];
    ButtonsPanel {
        title: plain(tr.statistics_answer_buttons_title()),
        subtitle: plain(tr.statistics_answer_buttons_subtitle()),
        range,
        has_data: groups.iter().any(|g| g.counts.iter().any(|c| *c > 0)),
        button_labels: [
            plain(tr.studying_again()),
            plain(tr.studying_hard()),
            plain(tr.studying_good()),
            plain(tr.studying_easy()),
        ],
        groups,
    }
}

/// `added.ts` `buildHistogram`.
pub(crate) fn added_panel(tr: &I18n, data: &GraphsData, range: GraphRange) -> HistogramPanel {
    let mut panel = HistogramPanel {
        title: plain(tr.statistics_added_title()),
        subtitle: plain(tr.statistics_added_subtitle()),
        histogram: None,
        table: Vec::new(),
    };
    let Some(&earliest) = data.added.keys().next() else {
        return panel;
    };
    let x_min = match range {
        GraphRange::Month => -31.0,
        GraphRange::ThreeMonths => -90.0,
        GraphRange::Year => -365.0,
        GraphRange::AllTime => earliest as f64,
    };
    let x_max = 1.0;
    let desired = x_min.abs().min(70.0);
    let thresholds = d3::ticks(x_min, x_max, desired);
    let bins = d3::bin(
        (x_min, x_max),
        &thresholds,
        data.added.iter().map(|(k, v)| (*k as f64, *v)),
        add_u32,
    );
    let total: u64 = bins.iter().map(|b| b.sum).sum();
    if total == 0 {
        return panel;
    }
    let period_days = x_min.abs().max(1.0);
    let per_day = js_round(total as f64 / period_days);
    panel.table = vec![
        row(
            plain(tr.statistics_total()),
            plain(tr.statistics_cards(total)),
        ),
        row(
            plain(tr.statistics_average()),
            plain(tr.statistics_cards_per_day(per_day as i64)),
        ),
    ];
    panel.histogram = Some(Histogram {
        bins: bars(
            &bins,
            |bin, running| {
                format!(
                    "{}:\n{}\n{}: {}",
                    day_label(tr, bin.x0, bin.x1),
                    plain(tr.statistics_cards(bin.sum)),
                    plain(tr.statistics_running_total()),
                    plain(tr.statistics_cards(running))
                )
            },
            |bin| {
                let start = bin.x0.abs() + 1.0;
                let end = bin.x1.abs() + 1.0;
                let include = format!("\"added:{}\"", js_num(start));
                Some(if start == 1.0 {
                    include
                } else {
                    format!("{include} AND -\"added:{}\"", js_num(end))
                })
            },
        ),
        total,
        cumulative: true,
        percent_axis: false,
    });
    panel
}

#[cfg(test)]
mod tests;
