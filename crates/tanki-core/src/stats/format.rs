//! Text helpers of Anki's TypeScript (`ts/lib/tslib/time.ts`,
//! `ts/lib/tslib/i18n/utils.ts`) that the graphs and card info use, over
//! the collection's translations.
use anki_i18n::I18n;
use std::borrow::Cow;

pub(crate) const MINUTE: f64 = 60.0;
pub(crate) const HOUR: f64 = 60.0 * MINUTE;
pub(crate) const DAY: f64 = 24.0 * HOUR;
pub(crate) const YEAR: f64 = 365.0 * DAY;
pub(crate) const MONTH: f64 = YEAR / 12.0;

/// Fluent output without Unicode isolation marks.
pub(crate) fn plain(text: Cow<'_, str>) -> String {
    anki_i18n::without_unicode_isolation(text.as_ref())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Unit {
    Seconds,
    Minutes,
    Hours,
    Days,
    Months,
    Years,
}

impl Unit {
    fn seconds(self) -> f64 {
        match self {
            Unit::Seconds => 1.0,
            Unit::Minutes => MINUTE,
            Unit::Hours => HOUR,
            Unit::Days => DAY,
            Unit::Months => MONTH,
            Unit::Years => YEAR,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Unit::Seconds => "seconds",
            Unit::Minutes => "minutes",
            Unit::Hours => "hours",
            Unit::Days => "days",
            Unit::Months => "months",
            Unit::Years => "years",
        }
    }
}

/// `naturalUnit`.
pub(crate) fn natural_unit(secs: f64) -> Unit {
    let secs = secs.abs();
    if secs < MINUTE {
        Unit::Seconds
    } else if secs < HOUR {
        Unit::Minutes
    } else if secs < DAY {
        Unit::Hours
    } else if secs < MONTH {
        Unit::Days
    } else if secs < YEAR {
        Unit::Months
    } else {
        Unit::Years
    }
}

/// `timeSpan(seconds, short, precise, maxUnit)`.
pub(crate) fn time_span(tr: &I18n, seconds: f64, short: bool, precise: bool, max: Unit) -> String {
    let unit = natural_unit(seconds).min(max);
    let mut amount = seconds / unit.seconds();
    if !precise && unit < Unit::Months {
        amount = super::d3::js_round(amount);
    }
    let amount = amount as f32;
    plain(match (short, unit) {
        (true, Unit::Seconds) => tr.statistics_elapsed_time_seconds(amount),
        (true, Unit::Minutes) => tr.statistics_elapsed_time_minutes(amount),
        (true, Unit::Hours) => tr.statistics_elapsed_time_hours(amount),
        (true, Unit::Days) => tr.statistics_elapsed_time_days(amount),
        (true, Unit::Months) => tr.statistics_elapsed_time_months(amount),
        (true, Unit::Years) => tr.statistics_elapsed_time_years(amount),
        (false, Unit::Seconds) => tr.scheduling_time_span_seconds(amount),
        (false, Unit::Minutes) => tr.scheduling_time_span_minutes(amount),
        (false, Unit::Hours) => tr.scheduling_time_span_hours(amount),
        (false, Unit::Days) => tr.scheduling_time_span_days(amount),
        (false, Unit::Months) => tr.scheduling_time_span_months(amount),
        (false, Unit::Years) => tr.scheduling_time_span_years(amount),
    })
}

/// `timeSpan(seconds)` with its defaults (long, precise, up to years).
pub(crate) fn span(tr: &I18n, seconds: f64) -> String {
    time_span(tr, seconds, false, true, Unit::Years)
}

/// `dayLabel(daysStart, daysEnd)`.
pub(crate) fn day_label(tr: &I18n, start: f64, end: f64) -> String {
    let larger = start.abs().max(end.abs());
    let smaller = start.abs().min(end.abs());
    plain(if larger - smaller <= 1.0 {
        if start >= 0.0 {
            tr.statistics_in_days_single(start as i64)
        } else {
            tr.statistics_days_ago_single(-start as i64)
        }
    } else if start >= 0.0 {
        tr.statistics_in_days_range(start as i64, (end - 1.0) as i64)
    } else {
        tr.statistics_days_ago_range((end - 1.0).abs() as i64, -start as i64)
    })
}

/// `studiedToday(cards, secs)` of `time.ts`.
pub(crate) fn studied_today(tr: &I18n, cards: u32, secs: f64) -> String {
    let unit = natural_unit(secs).min(Unit::Minutes);
    let amount = secs / unit.seconds();
    let per_card = if cards > 0 { secs / cards as f64 } else { 0.0 };
    // The message has line breaks that desktop's HTML shows as spaces.
    plain(tr.statistics_studied_today(unit.name(), per_card as f32, amount as f32, cards))
        .replace('\n', " ")
}

/// JavaScript's default number to string, as in template literals.
pub(crate) fn js_num(n: f64) -> String {
    if n == n.trunc() && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// `localizedNumber(n, precision)`: rounded to `precision` decimals,
/// grouped by thousands (English separators; the TUI has no locale data).
pub fn localized_number(n: f64, precision: u32) -> String {
    let round = 10f64.powi(precision as i32);
    let rounded = super::d3::js_round(n * round) / round;
    // toLocaleString shows at most three decimals.
    let mut text = format!("{:.*}", precision.min(3) as usize, rounded.abs());
    if text.contains('.') {
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
    let (int, frac) = match text.split_once('.') {
        Some((i, f)) => (i.to_owned(), Some(f.to_owned())),
        None => (text, None),
    };
    let mut grouped = String::new();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let sign = if rounded < 0.0 && grouped != "0" {
        "-"
    } else {
        ""
    };
    match frac {
        Some(f) => format!("{sign}{grouped}.{f}"),
        None => format!("{sign}{grouped}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_spans() {
        assert_eq!(localized_number(1234567.0, 0), "1,234,567");
        assert_eq!(localized_number(66.6666, 2), "66.67");
        assert_eq!(localized_number(50.0, 2), "50");
        assert_eq!(localized_number(-0.004, 2), "0");
        // Math.round rounds halves up, also below zero.
        assert_eq!(localized_number(-1500.5, 0), "-1,500");
        let tr = I18n::template_only();
        assert_eq!(span(&tr, 30.0 * DAY), "30 days");
        assert_eq!(span(&tr, 70.0), "1.17 minutes");
        assert_eq!(time_span(&tr, 70.0, true, true, Unit::Years), "1.17m");
        assert_eq!(
            time_span(&tr, 90.0 * DAY, false, false, Unit::Days),
            "90 days"
        );
        assert_eq!(day_label(&tr, 3.0, 4.0), "In 3 days");
        assert_eq!(day_label(&tr, 3.0, 10.0), "In 3-9 days");
        assert_eq!(day_label(&tr, -30.0, -20.0), "21-30 days ago");
        assert_eq!(
            studied_today(&tr, 4, 120.0),
            "Studied 4 cards in 2 minutes today (30s/card)"
        );
    }
}
