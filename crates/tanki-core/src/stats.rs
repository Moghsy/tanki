//! Statistics graphs and card info (PLAN 8.8, R6 §7).
//!
//! Owned by the Statistics stream (docs/implementation/phase-5-interfaces.md).
//! Reached through `Command::Manage(ManageCommand::Stats(…))`.
//!
//! Anki returns raw aggregates from one `StatsService::graphs` call; every
//! number desktop shows after that is computed by its TypeScript
//! (`ts/routes/graphs/*.ts`, `ts/routes/card-info/*.ts`). That maths is
//! ported here as pure functions over [`GraphsData`] (module `panels`) and
//! over a card's review log (module `card_info`), so the TUI, the CLI's
//! JSON and its SVG share one implementation.
//!
//! Per-panel range switches (1 month / 3 months / 1 year / all time,
//! percentiles, backlog, count/time) are client-side on desktop: the TUI
//! keeps the [`GraphsData`] of the last [`StatsCommand::Graphs`] and sends it
//! back with [`StatsCommand::Panels`], which recomputes and relabels without
//! touching the collection. Only scope and history changes query again.
mod card_info;
mod d3;
pub mod export;
mod format;
mod panels;

pub use card_info::{
    curve_ranges, date_string, default_curve_range, forgetting_curve, CardInfo, CurvePoint,
    CurveRange, InfoRow, MemoryState, ReviewKind, RevlogEntry, RevlogRow,
};
pub use format::localized_number;
pub use panels::*;

use crate::{CoreError, OpChanges, Result};
use anki::collection::Collection;
use anki::services::{DecksService, SearchService, StatsService};
use anki_proto as pb;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Each variant's doc comment names its `StatsResponse` variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StatsCommand {
    /// Runs the graphs query for a scope and history window and computes
    /// every panel -> `Report`. Reads only.
    Graphs {
        scope: StatsScope,
        history: History,
        /// `None`: desktop's initial choices with the stored preferences.
        options: Option<PanelOptions>,
    },
    /// Recomputes the panels of an earlier report with other options
    /// (client-side on desktop) -> `Panels`. Does not read the collection.
    Panels {
        data: Box<GraphsData>,
        options: PanelOptions,
    },
    /// Stores the graph preferences desktop keeps in the collection config
    /// (first weekday, backlog, separate inactive) -> `Done`.
    SetPreferences(GraphPreferences),
    /// Card info for one card -> `CardInfo`.
    ///
    /// Not a pure read: Anki's `card_stats` writes the card's
    /// `last_review_time` when it is unset (straight to storage: no undo
    /// entry, no mtime/usn change, so nothing to sync). It also computes,
    /// but does not store, an FSRS memory state for a card that has none.
    CardInfo { card_id: i64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StatsResponse {
    Report(Box<StatsReport>),
    Panels(Box<Panels>),
    CardInfo(Box<CardInfo>),
    Done,
}

impl StatsResponse {
    /// The changes a mutating command made; the actor broadcasts these.
    /// None of these commands is an undoable operation.
    pub fn changes(&self) -> Option<&OpChanges> {
        None
    }
}

/// Which cards the graphs cover (desktop's `RangeBox`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StatsScope {
    /// `deck:current` (desktop's default).
    CurrentDeck,
    /// The whole collection (empty search, Anki's fast path).
    Collection,
    /// A deck (and its children) by name.
    Deck(String),
    /// A deck by id.
    DeckId(i64),
    /// Any Anki search.
    Search(String),
}

/// The review-log window loaded by the query (`GraphsRequest.days`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum History {
    /// 365 days (desktop's default).
    #[default]
    Year,
    /// All history (`days = 0`).
    All,
}

impl History {
    fn days(self) -> u32 {
        match self {
            History::Year => 365,
            History::All => 0,
        }
    }
}

/// `GraphPreferences.Weekday`: the weekdays desktop offers as the
/// calendar's first column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Weekday {
    #[default]
    Sunday,
    Monday,
    Friday,
    Saturday,
}

impl Weekday {
    /// 0 = Sunday … 6 = Saturday.
    pub fn number(self) -> u32 {
        match self {
            Weekday::Sunday => 0,
            Weekday::Monday => 1,
            Weekday::Friday => 5,
            Weekday::Saturday => 6,
        }
    }
    fn from_number(n: i32) -> Self {
        match n {
            1 => Weekday::Monday,
            5 => Weekday::Friday,
            6 => Weekday::Saturday,
            _ => Weekday::Sunday,
        }
    }
    /// The next choice (Sunday → Monday → Friday → Saturday → Sunday).
    pub fn next(self) -> Self {
        match self {
            Weekday::Sunday => Weekday::Monday,
            Weekday::Monday => Weekday::Friday,
            Weekday::Friday => Weekday::Saturday,
            Weekday::Saturday => Weekday::Sunday,
        }
    }
}

/// Graph preferences stored in the collection config under desktop's keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GraphPreferences {
    pub first_weekday: Weekday,
    pub card_counts_separate_inactive: bool,
    pub future_due_show_backlog: bool,
}

/// A calendar date in local time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
pub struct LocalDate {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

impl LocalDate {
    pub(crate) fn naive(self) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(self.year, self.month, self.day).unwrap_or_default()
    }
    pub(crate) fn from_naive(date: chrono::NaiveDate) -> Self {
        use chrono::Datelike;
        Self {
            year: date.year(),
            month: date.month(),
            day: date.day(),
        }
    }
}

/// Review counts (or milliseconds) by kind for one day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReviewKinds {
    pub learn: u32,
    pub relearn: u32,
    pub young: u32,
    pub mature: u32,
    pub filtered: u32,
}

impl ReviewKinds {
    pub fn total(&self) -> u64 {
        self.learn as u64
            + self.relearn as u64
            + self.young as u64
            + self.mature as u64
            + self.filtered as u64
    }
}

/// `GraphsResponse.Today`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TodayCounts {
    pub answer_count: u32,
    pub answer_millis: u32,
    pub correct_count: u32,
    pub mature_correct: u32,
    pub mature_count: u32,
    pub learn_count: u32,
    pub review_count: u32,
    pub relearn_count: u32,
    pub early_review_count: u32,
}

/// `GraphsResponse.CardCounts.Counts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CardStateCounts {
    pub new: u32,
    pub learn: u32,
    pub relearn: u32,
    pub young: u32,
    pub mature: u32,
    pub suspended: u32,
    pub buried: u32,
}

/// `GraphsResponse.Hours.Hour`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HourCounts {
    pub total: u32,
    pub correct: u32,
}

/// `GraphsResponse.Hours`: 24 entries per range.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HoursData {
    pub one_month: Vec<HourCounts>,
    pub three_months: Vec<HourCounts>,
    pub one_year: Vec<HourCounts>,
    pub all_time: Vec<HourCounts>,
}

/// `GraphsResponse.Buttons.ButtonCounts`: presses of buttons 1–4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ButtonCounts {
    pub learning: [u32; 4],
    pub young: [u32; 4],
    pub mature: [u32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ButtonsData {
    pub one_month: ButtonCounts,
    pub three_months: ButtonCounts,
    pub one_year: ButtonCounts,
    pub all_time: ButtonCounts,
}

/// `TrueRetentionStats.TrueRetention`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RetentionCounts {
    pub young_passed: u32,
    pub young_failed: u32,
    pub mature_passed: u32,
    pub mature_failed: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TrueRetentionData {
    pub today: RetentionCounts,
    pub yesterday: RetentionCounts,
    pub week: RetentionCounts,
    pub month: RetentionCounts,
    pub year: RetentionCounts,
    pub all_time: RetentionCounts,
}

/// The raw aggregates of one graphs query (`GraphsResponse`) in tanki
/// types. Day keys are relative to today (negative = past).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GraphsData {
    /// The search Anki evaluated ("" = whole collection).
    pub search: String,
    pub history: History,
    pub fsrs: bool,
    pub rollover_hour: u32,
    /// The local date when the query ran.
    pub today_date: LocalDate,
    /// The local date of review day 0 (now minus the rollover hour), the
    /// calendar's anchor (`calendar.ts`).
    pub day_zero_date: LocalDate,
    pub preferences: GraphPreferences,
    pub today: TodayCounts,
    pub future_due: BTreeMap<i32, u32>,
    pub have_backlog: bool,
    pub daily_load: u32,
    pub review_counts: BTreeMap<i32, ReviewKinds>,
    /// Milliseconds.
    pub review_times: BTreeMap<i32, ReviewKinds>,
    pub counts_including_inactive: CardStateCounts,
    pub counts_excluding_inactive: CardStateCounts,
    /// Interval in days -> cards (review and relearning cards).
    pub intervals: BTreeMap<u32, u32>,
    /// FSRS stability in days -> cards.
    pub stability: BTreeMap<u32, u32>,
    /// Ease in percent -> cards (cards without a memory state).
    pub eases: BTreeMap<u32, u32>,
    pub ease_average: f32,
    /// Difficulty in percent (0–99) -> cards.
    pub difficulty: BTreeMap<u32, u32>,
    pub difficulty_average: f32,
    /// Retrievability in percent (0–99) -> cards.
    pub retrievability: BTreeMap<u32, u32>,
    pub retrievability_average: f32,
    pub retrievability_sum_by_card: f32,
    pub retrievability_sum_by_note: f32,
    pub true_retention: TrueRetentionData,
    pub hours: HoursData,
    pub buttons: ButtonsData,
    /// Day added (<= 0) -> cards.
    pub added: BTreeMap<i32, u32>,
}

/// One graphs query: the raw aggregates and the computed panels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsReport {
    pub data: GraphsData,
    pub options: PanelOptions,
    pub panels: Panels,
}

pub(crate) fn execute(col: &mut Collection, command: StatsCommand) -> Result<StatsResponse> {
    Ok(match command {
        StatsCommand::Graphs {
            scope,
            history,
            options,
        } => {
            let data = graphs(col, &scope, history)?;
            let options = options.unwrap_or_else(|| PanelOptions::initial(&data));
            let panels = panels::compute(col.tr(), &data, &options);
            StatsResponse::Report(Box::new(StatsReport {
                data,
                options,
                panels,
            }))
        }
        StatsCommand::Panels { data, options } => {
            StatsResponse::Panels(Box::new(panels::compute(col.tr(), &data, &options)))
        }
        StatsCommand::SetPreferences(prefs) => {
            StatsService::set_graph_preferences(
                col,
                pb::stats::GraphPreferences {
                    calendar_first_day_of_week: prefs.first_weekday.number() as i32,
                    card_counts_separate_inactive: prefs.card_counts_separate_inactive,
                    browser_links_supported: true,
                    future_due_show_backlog: prefs.future_due_show_backlog,
                },
            )?;
            StatsResponse::Done
        }
        StatsCommand::CardInfo { card_id } => {
            let stats = StatsService::card_stats(col, pb::cards::CardId { cid: card_id })?;
            StatsResponse::CardInfo(Box::new(card_info::from_proto(col.tr(), stats)))
        }
    })
}

/// The panels with Anki's English strings, without a collection (fake
/// backends in tests, tools). The actor uses the collection's language.
pub fn compute_panels_english(data: &GraphsData, options: &PanelOptions) -> Panels {
    panels::compute(&anki_i18n::I18n::template_only(), data, options)
}

/// The search text for a scope; a named deck must exist.
fn scope_search(col: &mut Collection, scope: &StatsScope) -> Result<String> {
    use pb::search::search_node::Filter;
    let node = |filter| pb::search::SearchNode {
        filter: Some(filter),
    };
    Ok(match scope {
        StatsScope::CurrentDeck => "deck:current".into(),
        StatsScope::Collection => String::new(),
        StatsScope::Search(text) => text.clone(),
        StatsScope::Deck(name) => {
            let found =
                DecksService::get_deck_id_by_name(col, pb::generic::String { val: name.clone() });
            if found.is_err() {
                return Err(CoreError::Invalid(format!("No deck named {name}")));
            }
            SearchService::build_search_string(col, node(Filter::Deck(name.clone())))?.val
        }
        StatsScope::DeckId(id) => {
            let deck = DecksService::get_deck(col, pb::decks::DeckId { did: *id })
                .map_err(|_| CoreError::Invalid(format!("No deck with id {id}")))?;
            SearchService::build_search_string(col, node(Filter::Deck(deck.name)))?.val
        }
    })
}

fn graphs(col: &mut Collection, scope: &StatsScope, history: History) -> Result<GraphsData> {
    let search = scope_search(col, scope)?;
    let response = StatsService::graphs(
        col,
        pb::stats::GraphsRequest {
            search: search.clone(),
            days: history.days(),
        },
    )?;
    let prefs = StatsService::get_graph_preferences(col)?;
    let now = chrono::Local::now();
    let day_zero = now - chrono::Duration::hours(response.rollover_hour as i64);
    let mut data = from_proto(response);
    data.search = search;
    data.history = history;
    data.today_date = LocalDate::from_naive(now.date_naive());
    data.day_zero_date = LocalDate::from_naive(day_zero.date_naive());
    data.preferences = GraphPreferences {
        first_weekday: Weekday::from_number(prefs.calendar_first_day_of_week),
        card_counts_separate_inactive: prefs.card_counts_separate_inactive,
        future_due_show_backlog: prefs.future_due_show_backlog,
    };
    Ok(data)
}

fn from_proto(r: pb::stats::GraphsResponse) -> GraphsData {
    use pb::stats::graphs_response as g;
    fn kinds(r: &g::review_counts_and_times::Reviews) -> ReviewKinds {
        ReviewKinds {
            learn: r.learn,
            relearn: r.relearn,
            young: r.young,
            mature: r.mature,
            filtered: r.filtered,
        }
    }
    fn counts(c: Option<g::card_counts::Counts>) -> CardStateCounts {
        let c = c.unwrap_or_default();
        CardStateCounts {
            new: c.new_cards,
            learn: c.learn,
            relearn: c.relearn,
            young: c.young,
            mature: c.mature,
            suspended: c.suspended,
            buried: c.buried,
        }
    }
    fn hours(h: &[g::hours::Hour]) -> Vec<HourCounts> {
        h.iter()
            .map(|h| HourCounts {
                total: h.total,
                correct: h.correct,
            })
            .collect()
    }
    fn four(v: &[u32]) -> [u32; 4] {
        let mut out = [0; 4];
        for (o, v) in out.iter_mut().zip(v) {
            *o = *v;
        }
        out
    }
    fn buttons(b: Option<g::buttons::ButtonCounts>) -> ButtonCounts {
        let b = b.unwrap_or_default();
        ButtonCounts {
            learning: four(&b.learning),
            young: four(&b.young),
            mature: four(&b.mature),
        }
    }
    fn retention(t: Option<g::true_retention_stats::TrueRetention>) -> RetentionCounts {
        let t = t.unwrap_or_default();
        RetentionCounts {
            young_passed: t.young_passed,
            young_failed: t.young_failed,
            mature_passed: t.mature_passed,
            mature_failed: t.mature_failed,
        }
    }
    let today = r.today.unwrap_or_default();
    let future = r.future_due.unwrap_or_default();
    let reviews = r.reviews.unwrap_or_default();
    let card_counts = r.card_counts.unwrap_or_default();
    let eases = r.eases.unwrap_or_default();
    let difficulty = r.difficulty.unwrap_or_default();
    let retrievability = r.retrievability.unwrap_or_default();
    let h = r.hours.unwrap_or_default();
    let b = r.buttons.unwrap_or_default();
    let t = r.true_retention.unwrap_or_default();
    GraphsData {
        fsrs: r.fsrs,
        rollover_hour: r.rollover_hour,
        today: TodayCounts {
            answer_count: today.answer_count,
            answer_millis: today.answer_millis,
            correct_count: today.correct_count,
            mature_correct: today.mature_correct,
            mature_count: today.mature_count,
            learn_count: today.learn_count,
            review_count: today.review_count,
            relearn_count: today.relearn_count,
            early_review_count: today.early_review_count,
        },
        future_due: future.future_due.into_iter().collect(),
        have_backlog: future.have_backlog,
        daily_load: future.daily_load,
        review_counts: reviews.count.iter().map(|(k, v)| (*k, kinds(v))).collect(),
        review_times: reviews.time.iter().map(|(k, v)| (*k, kinds(v))).collect(),
        counts_including_inactive: counts(card_counts.including_inactive),
        counts_excluding_inactive: counts(card_counts.excluding_inactive),
        intervals: r
            .intervals
            .unwrap_or_default()
            .intervals
            .into_iter()
            .collect(),
        stability: r
            .stability
            .unwrap_or_default()
            .intervals
            .into_iter()
            .collect(),
        eases: eases.eases.into_iter().collect(),
        ease_average: eases.average,
        difficulty: difficulty.eases.into_iter().collect(),
        difficulty_average: difficulty.average,
        retrievability: retrievability.retrievability.into_iter().collect(),
        retrievability_average: retrievability.average,
        retrievability_sum_by_card: retrievability.sum_by_card,
        retrievability_sum_by_note: retrievability.sum_by_note,
        true_retention: TrueRetentionData {
            today: retention(t.today),
            yesterday: retention(t.yesterday),
            week: retention(t.week),
            month: retention(t.month),
            year: retention(t.year),
            all_time: retention(t.all_time),
        },
        hours: HoursData {
            one_month: hours(&h.one_month),
            three_months: hours(&h.three_months),
            one_year: hours(&h.one_year),
            all_time: hours(&h.all_time),
        },
        buttons: ButtonsData {
            one_month: buttons(b.one_month),
            three_months: buttons(b.three_months),
            one_year: buttons(b.one_year),
            all_time: buttons(b.all_time),
        },
        added: r.added.unwrap_or_default().added.into_iter().collect(),
        ..Default::default()
    }
}
