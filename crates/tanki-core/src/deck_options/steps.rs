//! Learning-step text ("1m 10m 1h 2d"), a port of desktop's
//! `ts/routes/deck-options/steps.ts`. Steps are stored in minutes.

const MINUTE: f64 = 60.0;
const HOUR: f64 = 60.0 * MINUTE;
const DAY: f64 = 24.0 * HOUR;
const YEAR: f64 = 365.0 * DAY;
const MONTH: f64 = YEAR / 12.0;

#[derive(Clone, Copy, PartialEq, PartialOrd)]
enum Unit {
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
    fn smaller(self) -> Unit {
        match self {
            Unit::Years => Unit::Months,
            Unit::Months => Unit::Days,
            Unit::Days => Unit::Hours,
            Unit::Hours => Unit::Minutes,
            Unit::Minutes | Unit::Seconds => Unit::Seconds,
        }
    }
    fn suffix(self) -> &'static str {
        match self {
            Unit::Seconds => "s",
            Unit::Minutes => "m",
            Unit::Hours => "h",
            _ => "d",
        }
    }
}

fn natural_unit(secs: f64) -> Unit {
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

/// Largest unit the seconds divide into without a remainder.
fn natural_whole_unit(secs: f64) -> Unit {
    let mut unit = natural_unit(secs);
    while unit != Unit::Seconds {
        let amount = (secs / unit.seconds()).round();
        if (secs - amount * unit.seconds()).abs() < f64::EPSILON {
            return unit;
        }
        unit = unit.smaller();
    }
    unit
}

fn minutes_to_string(step: f32) -> String {
    let secs = f64::from(step) * 60.0;
    let mut unit = natural_whole_unit(secs);
    if unit >= Unit::Months {
        unit = Unit::Days;
    }
    let amount = (secs / unit.seconds()).round();
    format!("{amount}{}", unit.suffix())
}

/// `None` for text without digits (desktop's 0, which is then dropped).
fn string_to_minutes(text: &str) -> Option<f32> {
    let start = text.find(|c: char| c.is_ascii_digit())?;
    let digits_end = text[start..]
        .find(|c: char| !c.is_ascii_digit())
        .map_or(text.len(), |i| start + i);
    let number: f64 = text[start..digits_end].parse().ok()?;
    let unit = match &text[digits_end..] {
        "s" => Unit::Seconds,
        "h" => Unit::Hours,
        "d" => Unit::Days,
        _ => Unit::Minutes,
    };
    // Must stay representable as negative i32 seconds in a revlog.
    let seconds = (unit.seconds() * number).min(2f64.powi(31));
    Some((seconds / 60.0) as f32)
}

/// Steps in minutes -> "1m 10m 1h 2d".
pub fn steps_to_string(steps: &[f32]) -> String {
    steps
        .iter()
        .map(|s| minutes_to_string(*s))
        .collect::<Vec<_>>()
        .join(" ")
}

/// "1m 10m 1h" -> minutes; tokens without a number and zero steps are
/// dropped, as desktop does.
pub fn string_to_steps(text: &str) -> Vec<f32> {
    text.split(' ')
        .filter_map(string_to_minutes)
        .filter(|m| *m != 0.0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_desktop() {
        assert_eq!(steps_to_string(&[1.0, 10.0]), "1m 10m");
        assert_eq!(
            steps_to_string(&[0.5, 60.0, 1440.0, 2880.0]),
            "30s 1h 1d 2d"
        );
        assert_eq!(steps_to_string(&[90.0]), "90m");
        // Months and years are shown in days.
        assert_eq!(steps_to_string(&[1440.0 * 60.0]), "60d");
        assert_eq!(steps_to_string(&[]), "");
    }

    #[test]
    fn parses_like_desktop() {
        assert_eq!(string_to_steps("1m 10m"), vec![1.0, 10.0]);
        assert_eq!(string_to_steps("30s 1h 2d"), vec![0.5, 60.0, 2880.0]);
        // No suffix means minutes; garbage and zero steps are dropped.
        assert_eq!(string_to_steps("5  x 0 7q"), vec![5.0, 7.0]);
        // The number is the first run of digits.
        assert_eq!(string_to_steps("1.5m"), vec![1.0]);
        assert_eq!(string_to_steps(""), Vec::<f32>::new());
    }

    #[test]
    fn round_trips() {
        for text in ["1m 10m", "10m", "1m 1h 1d 3d", "45s"] {
            assert_eq!(steps_to_string(&string_to_steps(text)), text);
        }
    }
}
