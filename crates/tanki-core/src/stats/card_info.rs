//! Card info (`ts/routes/card-info/`): the stats rows of `lib.ts`, the
//! review log table of `Revlog.svelte` and the forgetting curve of
//! `forgetting-curve.ts`, over Anki's `card_stats` response.
use super::d3::js_round;
use super::format::{plain, span, time_span, Unit, DAY};
use anki_i18n::I18n;
use anki_proto as pb;
use chrono::TimeZone;
use serde::{Deserialize, Serialize};

/// `RevlogEntry.ReviewKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewKind {
    Learning,
    Review,
    Relearning,
    Filtered,
    Manual,
    Rescheduled,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MemoryState {
    /// Days.
    pub stability: f32,
    /// 1–10.
    pub difficulty: f32,
}

/// `CardStatsResponse.StatsRevlogEntry`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RevlogEntry {
    /// Unix seconds.
    pub time: i64,
    pub kind: ReviewKind,
    /// 0 (manual) or 1–4.
    pub button: u32,
    /// Seconds.
    pub interval: u32,
    /// Per mille; FSRS-era entries store difficulty here (≤ 1100).
    pub ease: u32,
    pub taken_secs: f32,
    pub memory_state: Option<MemoryState>,
    /// Seconds.
    pub last_interval: u32,
}

/// One row of the review log table (`RevlogRow` of `Revlog.svelte`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RevlogRow {
    /// YYYY-MM-DD, local.
    pub date: String,
    /// HH:MM, local.
    pub time: String,
    pub kind: ReviewKind,
    pub kind_label: String,
    pub rating: u32,
    pub interval: String,
    /// "250%" (SM-2) or "D:45%" (FSRS), empty for 0.
    pub ease: String,
    pub taken: String,
    /// Time since the previous counted review ("N/A" for manual,
    /// rescheduled and unrated filtered entries). Desktop computes but no
    /// longer shows it.
    pub elapsed: String,
    pub stability: String,
}

/// One label/value row of the stats table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InfoRow {
    pub label: String,
    pub value: String,
}

/// Forgetting-curve ranges (`TimeRange`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CurveRange {
    Week,
    Month,
    Year,
    AllTime,
}

impl CurveRange {
    fn max_days(self) -> f64 {
        match self {
            CurveRange::Week => 7.0,
            CurveRange::Month => 30.0,
            CurveRange::Year => 365.0,
            CurveRange::AllTime => f64::INFINITY,
        }
    }
}

/// One point of the forgetting curve.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint {
    /// Unix seconds.
    pub time: f64,
    pub days_since_first_learn: f64,
    pub elapsed_days: f64,
    /// Percent.
    pub retrievability: f64,
    pub stability: f64,
}

/// Everything the card info screen shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardInfo {
    pub card_id: i64,
    pub note_id: i64,
    pub rows: Vec<InfoRow>,
    /// Date, type, rating, interval, ease, time taken (desktop's columns),
    /// then elapsed and stability.
    pub revlog_headings: Vec<String>,
    /// Newest first.
    pub revlog: Vec<RevlogEntry>,
    pub revlog_rows: Vec<RevlogRow>,
    /// The card has an FSRS memory state: desktop shows the curve.
    pub fsrs: bool,
    pub desired_retention: f32,
    /// FSRS decay for the curve (`CardInfo.svelte`).
    pub decay: f32,
    pub curve_title: String,
    /// Week, month, year, all time.
    pub curve_range_labels: Vec<String>,
    pub desired_retention_label: String,
    /// When the stats were read (Unix seconds), the curve's "now".
    pub now: i64,
}

fn local(secs: i64) -> chrono::DateTime<chrono::Local> {
    chrono::Local
        .timestamp_opt(secs, 0)
        .single()
        .unwrap_or_default()
}

/// `Timestamp.dateString()`: YYYY-MM-DD in local time.
pub fn date_string(secs: i64) -> String {
    local(secs).format("%Y-%m-%d").to_string()
}

/// `Number.toFixed(digits)` (ties away from zero).
fn to_fixed(n: f64, digits: usize) -> String {
    let scale = 10f64.powi(digits as i32);
    format!("{:.*}", digits, js_round(n * scale) / scale)
}

fn kind_of(value: i32) -> ReviewKind {
    use pb::stats::revlog_entry::ReviewKind as K;
    match K::try_from(value).unwrap_or(K::Manual) {
        K::Learning => ReviewKind::Learning,
        K::Review => ReviewKind::Review,
        K::Relearning => ReviewKind::Relearning,
        K::Filtered => ReviewKind::Filtered,
        K::Manual => ReviewKind::Manual,
        K::Rescheduled => ReviewKind::Rescheduled,
    }
}

fn kind_label(tr: &I18n, kind: ReviewKind) -> String {
    plain(match kind {
        ReviewKind::Learning => tr.card_stats_review_log_type_learn(),
        ReviewKind::Review => tr.card_stats_review_log_type_review(),
        ReviewKind::Relearning => tr.card_stats_review_log_type_relearn(),
        ReviewKind::Filtered => tr.card_stats_review_log_type_filtered(),
        ReviewKind::Manual => tr.card_stats_review_log_type_manual(),
        ReviewKind::Rescheduled => tr.card_stats_review_log_type_rescheduled(),
    })
}

/// `formatEaseOrDifficulty`.
fn ease_or_difficulty(ease: u32) -> String {
    if ease == 0 {
        return String::new();
    }
    let pct = ease as f64 / 10.0;
    if pct <= 110.0 {
        format!("D:{}%", to_fixed(pct - 10.0, 0))
    } else {
        format!("{}%", to_fixed(pct, 0))
    }
}

/// `filterRevlogEntryByReviewKind`.
fn counted(entry: &RevlogEntry) -> bool {
    entry.kind != ReviewKind::Manual
        && entry.kind != ReviewKind::Rescheduled
        && (entry.kind != ReviewKind::Filtered || entry.ease != 0)
}

/// The review log table rows (`revlogRows` of `Revlog.svelte`).
pub(crate) fn revlog_rows(tr: &I18n, revlog: &[RevlogEntry]) -> Vec<RevlogRow> {
    revlog
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let previous = revlog[index + 1..].iter().find(|e| counted(e));
            let elapsed = if counted(entry) {
                match previous {
                    Some(p) => span(tr, (entry.time - p.time) as f64),
                    None => "0".to_owned(),
                }
            } else {
                "N/A".to_owned()
            };
            let time = local(entry.time);
            RevlogRow {
                date: time.format("%Y-%m-%d").to_string(),
                time: time.format("%H:%M").to_string(),
                kind: entry.kind,
                kind_label: kind_label(tr, entry.kind),
                rating: entry.button,
                interval: span(tr, entry.interval as f64),
                ease: ease_or_difficulty(entry.ease),
                taken: time_span(tr, entry.taken_secs as f64, true, true, Unit::Years),
                elapsed,
                stability: match entry.memory_state {
                    Some(m) if m.stability != 0.0 => span(tr, m.stability as f64 * DAY),
                    _ => String::new(),
                },
            }
        })
        .collect()
}

/// `rowsFromStats` of `lib.ts`.
fn rows_from_stats(tr: &I18n, s: &pb::stats::CardStatsResponse) -> Vec<InfoRow> {
    let mut rows = Vec::new();
    let mut push = |label: std::borrow::Cow<'_, str>, value: String| {
        rows.push(InfoRow {
            label: plain(label),
            value,
        })
    };
    push(tr.card_stats_added(), date_string(s.added));
    if let Some(t) = s.first_review {
        push(tr.card_stats_first_review(), date_string(t));
    }
    if let Some(t) = s.latest_review {
        push(tr.card_stats_latest_review(), date_string(t));
    }
    if let Some(t) = s.due_date {
        push(tr.statistics_due_date(), date_string(t));
    }
    if let Some(p) = s.due_position {
        push(tr.card_stats_new_card_position(), p.to_string());
    }
    if s.interval > 0 {
        push(tr.card_stats_interval(), span(tr, s.interval as f64 * DAY));
    }
    if let Some(m) = &s.memory_state {
        let secs = m.stability as f64 * DAY;
        let mut stability = time_span(tr, secs, false, false, Unit::Years);
        if m.stability > 31.0 {
            stability = format!(
                "{stability} ({})",
                time_span(tr, secs, false, false, Unit::Days)
            );
        }
        push(tr.card_stats_fsrs_stability(), stability);
        push(
            tr.card_stats_fsrs_difficulty(),
            format!(
                "{}%",
                to_fixed((m.difficulty as f64 - 1.0) / 9.0 * 100.0, 0)
            ),
        );
        if let Some(r) = s.fsrs_retrievability.filter(|r| *r != 0.0) {
            push(
                tr.card_stats_fsrs_retrievability(),
                format!("{}%", to_fixed(r as f64 * 100.0, 0)),
            );
        }
    } else if s.ease > 0 && s.desired_retention.is_none() {
        push(
            tr.card_stats_ease(),
            format!("{}%", super::format::js_num(s.ease as f64 / 10.0)),
        );
    }
    push(tr.card_stats_review_count(), s.reviews.to_string());
    push(tr.card_stats_lapse_count(), s.lapses.to_string());
    if s.total_secs != 0.0 {
        push(
            tr.card_stats_average_time(),
            span(tr, s.average_secs as f64),
        );
        push(tr.card_stats_total_time(), span(tr, s.total_secs as f64));
    }
    push(tr.card_stats_card_template(), s.card_type.clone());
    push(tr.card_stats_note_type(), s.notetype.clone());
    let deck = match &s.original_deck {
        Some(original) => format!("{} ({original})", s.deck),
        None => s.deck.clone(),
    };
    push(tr.card_stats_deck_name(), deck);
    push(tr.card_stats_preset(), s.preset.clone());
    push(tr.card_stats_card_id(), s.card_id.to_string());
    push(tr.card_stats_note_id(), s.note_id.to_string());
    if !s.custom_data.is_empty() {
        let value = match serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
            &s.custom_data,
        ) {
            Ok(map) => map
                .iter()
                .map(|(k, v)| match v {
                    serde_json::Value::String(text) => format!("{k}={text}"),
                    other => format!("{k}={other}"),
                })
                .collect::<Vec<_>>()
                .join(" "),
            Err(_) => s.custom_data.clone(),
        };
        push(tr.card_stats_custom_data(), value);
    }
    rows
}

/// The FSRS decay `CardInfo.svelte` uses for the curve.
fn decay(params: &[f32]) -> f32 {
    match params.len() {
        0 => 0.1542,
        1..=20 => 0.5,
        _ => params[20],
    }
}

pub(crate) fn from_proto(tr: &I18n, s: pb::stats::CardStatsResponse) -> CardInfo {
    let revlog: Vec<RevlogEntry> = s
        .revlog
        .iter()
        .map(|e| RevlogEntry {
            time: e.time,
            kind: kind_of(e.review_kind),
            button: e.button_chosen,
            interval: e.interval,
            ease: e.ease,
            taken_secs: e.taken_secs,
            memory_state: e.memory_state.map(|m| MemoryState {
                stability: m.stability,
                difficulty: m.difficulty,
            }),
            last_interval: e.last_interval,
        })
        .collect();
    CardInfo {
        card_id: s.card_id,
        note_id: s.note_id,
        rows: rows_from_stats(tr, &s),
        revlog_headings: vec![
            plain(tr.card_stats_review_log_date()),
            plain(tr.card_stats_review_log_type()),
            plain(tr.card_stats_review_log_rating()),
            plain(tr.card_stats_interval()),
            plain(tr.card_stats_ease()),
            plain(tr.card_stats_review_log_time_taken()),
            plain(tr.card_stats_review_log_elapsed_time()),
            plain(tr.card_stats_fsrs_stability()),
        ],
        revlog_rows: revlog_rows(tr, &revlog),
        revlog,
        fsrs: s.memory_state.is_some(),
        desired_retention: s.desired_retention.unwrap_or(0.9),
        decay: decay(&s.fsrs_params),
        curve_title: plain(tr.card_stats_fsrs_forgetting_curve_title()),
        curve_range_labels: vec![
            plain(tr.card_stats_fsrs_forgetting_curve_first_week()),
            plain(tr.card_stats_fsrs_forgetting_curve_first_month()),
            plain(tr.card_stats_fsrs_forgetting_curve_first_year()),
            plain(tr.card_stats_fsrs_forgetting_curve_all_time()),
        ],
        desired_retention_label: plain(tr.card_stats_fsrs_forgetting_curve_desired_retention()),
        now: chrono::Utc::now().timestamp(),
    }
}

// ---- forgetting-curve.ts ----

const MIN_POINTS: f64 = 1000.0;

/// `forgettingCurve(stability, daysElapsed, decay)`.
fn curve(stability: f64, days: f64, decay: f64) -> f64 {
    let factor = 0.9f64.powf(1.0 / -decay) - 1.0;
    ((days / stability) * factor + 1.0).powf(-decay)
}

/// `filterRevlog`: newest first, up to the last reset or the first entry
/// without a memory state, counted kinds only.
fn filter_revlog(revlog: &[RevlogEntry]) -> Vec<&RevlogEntry> {
    let mut result = Vec::new();
    for entry in revlog {
        if (entry.kind == ReviewKind::Manual && entry.ease == 0) || entry.memory_state.is_none() {
            break;
        }
        result.push(entry);
    }
    result.retain(|e| counted(e));
    result
}

/// `calculateMaxDays`.
fn max_days(filtered: &[&RevlogEntry], range: CurveRange, now: f64) -> f64 {
    let (Some(newest), Some(oldest)) = (filtered.first(), filtered.last()) else {
        return 0.0;
    };
    let since_first = (now - oldest.time as f64) / 86400.0;
    let since_last = (now - newest.time as f64) / 86400.0;
    let scheduled = newest.interval as f64 / 86400.0;
    let preview = (scheduled * 1.5 - since_last).max(scheduled * 0.5);
    (since_first + preview).min(range.max_days())
}

/// The ranges desktop offers for this review log (`ForgettingCurve.svelte`:
/// none when the history spans a week or less).
pub fn curve_ranges(revlog: &[RevlogEntry], now: i64) -> Vec<CurveRange> {
    let filtered = filter_revlog(revlog);
    let max = max_days(&filtered, CurveRange::AllTime, now as f64);
    let mut ranges = Vec::new();
    if max > 7.0 {
        ranges.extend([CurveRange::Week, CurveRange::Month]);
        if max > 30.0 {
            ranges.push(CurveRange::Year);
        }
        if max > 365.0 {
            ranges.push(CurveRange::AllTime);
        }
    }
    ranges
}

/// The range desktop selects first.
pub fn default_curve_range(revlog: &[RevlogEntry], now: i64) -> CurveRange {
    let max = max_days(&filter_revlog(revlog), CurveRange::AllTime, now as f64);
    if max > 365.0 {
        CurveRange::AllTime
    } else if max > 30.0 {
        CurveRange::Year
    } else if max > 7.0 {
        CurveRange::Month
    } else {
        CurveRange::Week
    }
}

/// The forgetting curve's points for a range (`prepareData`); empty when
/// the card has no FSRS history. `now` is Unix seconds.
pub fn forgetting_curve(
    revlog: &[RevlogEntry],
    range: CurveRange,
    decay: f32,
    now: i64,
) -> Vec<CurvePoint> {
    let filtered = filter_revlog(revlog);
    if filtered.is_empty() {
        return Vec::new();
    }
    let now = now as f64;
    let decay = decay as f64;
    let max_days = max_days(&filtered, range, now);
    let mut data: Vec<CurvePoint> = Vec::new();
    let mut last_time = 0.0;
    let mut last_stability = 0.0;
    let step = (max_days / MIN_POINTS).min(1.0);
    // d3 would loop forever on an empty span; nothing is drawn then anyway.
    let step = if step > 0.0 { step } else { 1.0 };
    let mut since_first = 0.0;
    let stability_of = |e: &RevlogEntry| e.memory_state.map(|m| m.stability as f64).unwrap_or(0.0);
    for (index, entry) in filtered.iter().rev().enumerate() {
        let review_time = entry.time as f64;
        if index == 0 {
            last_time = review_time;
            last_stability = stability_of(entry);
            data.push(CurvePoint {
                time: review_time,
                days_since_first_learn: 0.0,
                elapsed_days: 0.0,
                retrievability: 100.0,
                stability: last_stability,
            });
            continue;
        }
        let total = (review_time - last_time) / 86400.0;
        let mut elapsed = 0.0;
        while elapsed < total - step {
            elapsed += step;
            let previous = data.last().map(|p| p.days_since_first_learn).unwrap_or(0.0);
            data.push(CurvePoint {
                time: last_time + elapsed * 86400.0,
                days_since_first_learn: previous + step,
                elapsed_days: elapsed,
                retrievability: curve(last_stability, elapsed, decay) * 100.0,
                stability: last_stability,
            });
        }
        since_first += total;
        data.push(CurvePoint {
            time: last_time + total * 86400.0,
            days_since_first_learn: since_first,
            elapsed_days: 0.0,
            retrievability: 100.0,
            stability: last_stability,
        });
        last_time = review_time;
        last_stability = stability_of(entry);
    }
    let since_last = (now - last_time) / 86400.0;
    let mut elapsed = 0.0;
    while elapsed < since_last - step {
        elapsed += step;
        let previous = data.last().map(|p| p.days_since_first_learn).unwrap_or(0.0);
        data.push(CurvePoint {
            time: last_time + elapsed * 86400.0,
            days_since_first_learn: previous + step,
            elapsed_days: elapsed,
            retrievability: curve(last_stability, elapsed, decay) * 100.0,
            stability: last_stability,
        });
    }
    since_first += since_last;
    data.push(CurvePoint {
        time: now,
        days_since_first_learn: since_first,
        elapsed_days: since_last,
        retrievability: curve(last_stability, since_last, decay) * 100.0,
        stability: last_stability,
    });
    let preview_days = max_days - since_last;
    let mut preview = 0.0;
    while preview < preview_days {
        preview += step;
        let previous = data.last().map(|p| p.days_since_first_learn).unwrap_or(0.0);
        data.push(CurvePoint {
            time: now + preview * 86400.0,
            days_since_first_learn: previous + step,
            elapsed_days: since_last + preview,
            // As desktop: the preview continues from the last sampled
            // elapsed day, not from `since_last`.
            retrievability: curve(last_stability, elapsed + preview, decay) * 100.0,
            stability: last_stability,
        });
    }
    data.retain(|p| p.days_since_first_learn <= max_days);
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        time: i64,
        kind: ReviewKind,
        stability: Option<f32>,
        interval_days: u32,
    ) -> RevlogEntry {
        RevlogEntry {
            time,
            kind,
            button: 3,
            interval: interval_days * 86400,
            ease: 2500,
            taken_secs: 8.0,
            memory_state: stability.map(|s| MemoryState {
                stability: s,
                difficulty: 5.0,
            }),
            last_interval: 0,
        }
    }

    const T0: i64 = 1_700_000_000;
    const D: i64 = 86400;

    #[test]
    fn curve_formula_matches_fsrs() {
        // R(S) = 0.9 for any decay.
        assert!((curve(10.0, 10.0, 0.5) - 0.9).abs() < 1e-12);
        assert!((curve(10.0, 10.0, 0.1542) - 0.9).abs() < 1e-12);
        assert_eq!(curve(10.0, 0.0, 0.5), 1.0);
        assert_eq!(decay(&[]), 0.1542);
        assert_eq!(decay(&[1.0; 19]), 0.5);
        let mut params = vec![1.0; 21];
        params[20] = 0.2;
        assert_eq!(decay(&params), 0.2);
    }

    #[test]
    fn filter_stops_at_reset_and_skips_uncounted_kinds() {
        let mut reset = entry(T0, ReviewKind::Manual, Some(1.0), 0);
        reset.ease = 0;
        let revlog = vec![
            entry(T0 + 3 * D, ReviewKind::Review, Some(9.0), 9),
            entry(T0 + 2 * D, ReviewKind::Rescheduled, Some(4.0), 4),
            entry(T0 + D, ReviewKind::Learning, Some(3.0), 3),
            reset,
            entry(T0 - D, ReviewKind::Learning, Some(1.0), 1),
        ];
        let kept: Vec<i64> = filter_revlog(&revlog).iter().map(|e| e.time).collect();
        assert_eq!(kept, vec![T0 + 3 * D, T0 + D]);
        // No memory state ends the history too.
        let revlog = vec![entry(T0, ReviewKind::Review, None, 3)];
        assert!(filter_revlog(&revlog).is_empty());
    }

    #[test]
    fn curve_points_follow_prepare_data() {
        // Learnt at T0 (S = 2), reviewed at T0 + 2d (S = 10, interval 10d),
        // looked at 3 days later.
        let revlog = vec![
            entry(T0 + 2 * D, ReviewKind::Review, Some(10.0), 10),
            entry(T0, ReviewKind::Learning, Some(2.0), 2),
        ];
        let now = T0 + 5 * D;
        // maxDays = min(5 + max(15 - 3, 5), ∞) = 17 → step 0.017.
        assert_eq!(default_curve_range(&revlog, now), CurveRange::Month);
        assert_eq!(
            curve_ranges(&revlog, now),
            vec![CurveRange::Week, CurveRange::Month]
        );
        let points = forgetting_curve(&revlog, CurveRange::Month, 0.5, now);
        assert_eq!(points[0].retrievability, 100.0);
        assert_eq!(points[0].time, T0 as f64);
        // The review resets retrievability to 100% two days in.
        let reset = points
            .iter()
            .position(|p| p.retrievability == 100.0 && p.days_since_first_learn > 1.0)
            .unwrap();
        assert!((points[reset].days_since_first_learn - 2.0).abs() < 1e-9);
        // Just before it, R(2 days, S = 2) ≈ 90%.
        assert!((points[reset - 1].retrievability - 90.0).abs() < 0.2);
        // "Now" is a point: R(3 days, S = 10).
        let now_point = points.iter().find(|p| p.time == now as f64).unwrap();
        assert!((now_point.retrievability - curve(10.0, 3.0, 0.5) * 100.0).abs() < 1e-9);
        // Nothing beyond maxDays.
        assert!(points.iter().all(|p| p.days_since_first_learn <= 17.0));
        assert!(points.last().unwrap().days_since_first_learn > 16.9);
        // A week view is cut at seven days.
        let week = forgetting_curve(&revlog, CurveRange::Week, 0.5, now);
        assert!(week.iter().all(|p| p.days_since_first_learn <= 7.0));
        assert!(forgetting_curve(&[], CurveRange::Week, 0.5, now).is_empty());
    }

    #[test]
    fn revlog_rows_and_ease_text() {
        let tr = I18n::template_only();
        let mut manual = entry(T0 + D, ReviewKind::Manual, None, 0);
        manual.ease = 0;
        manual.button = 0;
        let revlog = vec![
            entry(T0 + 4 * D, ReviewKind::Review, Some(10.0), 10),
            manual,
            entry(T0, ReviewKind::Learning, None, 1),
        ];
        let rows = revlog_rows(&tr, &revlog);
        assert_eq!(rows[0].elapsed, "4 days");
        assert_eq!(rows[1].elapsed, "N/A");
        assert_eq!(rows[2].elapsed, "0");
        assert_eq!(rows[0].kind_label, "Review");
        assert_eq!(rows[0].ease, "250%");
        assert_eq!(rows[0].interval, "10 days");
        assert_eq!(rows[0].taken, "8s");
        assert_eq!(rows[0].stability, "10 days");
        assert_eq!(rows[1].ease, "");
        assert_eq!(ease_or_difficulty(560), "D:46%");
        assert_eq!(ease_or_difficulty(1100), "D:100%");
        assert_eq!(to_fixed(2.5, 0), "3");
    }
}
