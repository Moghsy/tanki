//! The parts of d3 that Anki's graph code relies on, ported exactly
//! (d3-array 3.2.4 `ticks`/`tickIncrement`/`bin`/`quantile`/`range`,
//! d3-scale 4.0.2 `linear().nice()`), the versions pinned by
//! `vendor/anki/yarn.lock`. Binning results must match desktop bin for bin,
//! so the floating point steps follow the JavaScript source.

const E10: f64 = 7.0710678118654755; // sqrt(50)
const E5: f64 = 3.1622776601683795; // sqrt(10)
const E2: f64 = std::f64::consts::SQRT_2;

/// JavaScript's `Math.round` (halves round up, also for negatives).
pub(crate) fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

fn tick_spec(start: f64, stop: f64, count: f64) -> (f64, f64, f64) {
    let step = (stop - start) / count.max(0.0);
    let power = step.log10().floor();
    let error = step / 10f64.powf(power);
    let factor = if error >= E10 {
        10.0
    } else if error >= E5 {
        5.0
    } else if error >= E2 {
        2.0
    } else {
        1.0
    };
    let (mut i1, mut i2, inc);
    if power < 0.0 {
        let positive = 10f64.powf(-power) / factor;
        i1 = js_round(start * positive);
        i2 = js_round(stop * positive);
        if i1 / positive < start {
            i1 += 1.0;
        }
        if i2 / positive > stop {
            i2 -= 1.0;
        }
        inc = -positive;
    } else {
        let positive = 10f64.powf(power) * factor;
        i1 = js_round(start / positive);
        i2 = js_round(stop / positive);
        if i1 * positive < start {
            i1 += 1.0;
        }
        if i2 * positive > stop {
            i2 -= 1.0;
        }
        inc = positive;
    }
    if i2 < i1 && (0.5..2.0).contains(&count) {
        return tick_spec(start, stop, count * 2.0);
    }
    (i1, i2, inc)
}

/// `d3.ticks(start, stop, count)`.
pub(crate) fn ticks(start: f64, stop: f64, count: f64) -> Vec<f64> {
    if count.is_nan() || count <= 0.0 {
        return Vec::new();
    }
    if start == stop {
        return vec![start];
    }
    let reverse = stop < start;
    let (i1, i2, inc) = if reverse {
        tick_spec(stop, start, count)
    } else {
        tick_spec(start, stop, count)
    };
    if i2.is_nan() || i1.is_nan() || i2 < i1 {
        return Vec::new();
    }
    let n = (i2 - i1 + 1.0) as usize;
    (0..n)
        .map(|i| {
            let i = i as f64;
            match (reverse, inc < 0.0) {
                (true, true) => (i2 - i) / -inc,
                (true, false) => (i2 - i) * inc,
                (false, true) => (i1 + i) / -inc,
                (false, false) => (i1 + i) * inc,
            }
        })
        .collect()
}

/// `d3.tickIncrement(start, stop, count)`.
pub(crate) fn tick_increment(start: f64, stop: f64, count: f64) -> f64 {
    tick_spec(start, stop, count).2
}

/// `d3.scaleLinear().domain([start, stop]).nice()` (count 10): the niced
/// domain. A degenerate domain stays as it is, as in d3.
pub(crate) fn nice(start: f64, stop: f64) -> (f64, f64) {
    let count = 10.0;
    let (mut a, mut b, swapped) = if stop < start {
        (stop, start, true)
    } else {
        (start, stop, false)
    };
    let mut prestep: Option<f64> = None;
    for _ in 0..10 {
        let step = tick_increment(a, b, count);
        if Some(step) == prestep {
            return if swapped { (b, a) } else { (a, b) };
        } else if step > 0.0 {
            a = (a / step).floor() * step;
            b = (b / step).ceil() * step;
        } else if step < 0.0 {
            a = (a * step).ceil() / step;
            b = (b * step).floor() / step;
        } else {
            break;
        }
        prestep = Some(step);
    }
    (start, stop)
}

/// One histogram bin: `[x0, x1)` (the last bin also holds `x1`) and the
/// values that fell into it, summed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Bin<T> {
    pub x0: f64,
    pub x1: f64,
    /// Number of data points (`bin.length` in d3).
    pub len: usize,
    pub sum: T,
}

/// `d3.bin().domain([x0, x1]).thresholds(thresholds)(data)` with a value
/// accessor, summing each point's payload with `add`.
pub(crate) fn bin<T: Default + Clone, P>(
    domain: (f64, f64),
    thresholds: &[f64],
    data: impl IntoIterator<Item = (f64, P)>,
    add: impl Fn(&mut T, &P),
) -> Vec<Bin<T>> {
    let (x0, x1) = domain;
    let m = thresholds.len();
    let mut a = 0usize;
    let mut b = m;
    while a < m && thresholds[a] <= x0 {
        a += 1;
    }
    while b > 0 && thresholds[b - 1] > x1 {
        b -= 1;
    }
    let tz: &[f64] = if a < b { &thresholds[a..b] } else { &[] };
    let m = tz.len();
    let mut bins: Vec<Bin<T>> = (0..=m)
        .map(|i| Bin {
            x0: if i > 0 { tz[i - 1] } else { x0 },
            x1: if i < m { tz[i] } else { x1 },
            len: 0,
            sum: T::default(),
        })
        .collect();
    for (x, payload) in data {
        if x0 <= x && x <= x1 {
            // bisectRight
            let index = tz.partition_point(|t| *t <= x);
            let bin = &mut bins[index];
            bin.len += 1;
            add(&mut bin.sum, &payload);
        }
    }
    bins
}

/// `d3.quantile` over a sorted multiset given as `(value, count)` pairs in
/// ascending order of value (the expanded array is never built).
pub(crate) fn quantile_counts(sorted: &[(f64, u64)], p: f64) -> Option<f64> {
    let n: u64 = sorted.iter().map(|(_, c)| *c).sum();
    if n == 0 || p.is_nan() {
        return None;
    }
    let at = |index: u64| -> f64 {
        let mut seen = 0;
        for (value, count) in sorted {
            seen += count;
            if index < seen {
                return *value;
            }
        }
        sorted.last().map(|(v, _)| *v).unwrap_or_default()
    };
    if p <= 0.0 || n < 2 {
        return Some(at(0));
    }
    if p >= 1.0 {
        return Some(at(n - 1));
    }
    let i = (n - 1) as f64 * p;
    let i0 = i.floor();
    let value0 = at(i0 as u64);
    let value1 = at(i0 as u64 + 1);
    Some(value0 + (value1 - value0) * (i - i0))
}

/// `d3.range(start, stop, step)`.
pub(crate) fn range(start: f64, stop: f64, step: f64) -> Vec<f64> {
    let n = ((stop - start) / step).ceil().max(0.0) as usize;
    (0..n).map(|i| start + i as f64 * step).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_match_d3() {
        // Values printed by d3-array 3.2.4 under node.
        assert_eq!(
            ticks(0.0, 31.0, 31.0),
            (0..=31).map(f64::from).collect::<Vec<_>>()
        );
        assert_eq!(
            ticks(0.0, 365.0, 70.0),
            (0..=73).map(|i| f64::from(i) * 5.0).collect::<Vec<_>>()
        );
        assert_eq!(
            ticks(-364.0, 1.0, 70.0),
            (0..=72)
                .map(|i| -360.0 + f64::from(i) * 5.0)
                .collect::<Vec<_>>()
        );
        assert_eq!(ticks(130.0, 131.0, 20.0)[..3], [130.0, 130.05, 130.1]);
        assert_eq!(ticks(5.0, 5.0, 3.0), vec![5.0]);
        assert!(ticks(0.0, 1.0, 0.0).is_empty());
    }

    #[test]
    fn nice_matches_d3() {
        assert_eq!(nice(0.0, 251.0), (0.0, 260.0));
        assert_eq!(nice(130.0, 251.0), (130.0, 260.0));
        assert_eq!(nice(0.0, 7.0), (0.0, 7.0));
        assert_eq!(nice(13.0, 87.0), (10.0, 90.0));
        assert_eq!(nice(50.0, 50.0), (50.0, 50.0));
    }

    #[test]
    fn bin_follows_d3_threshold_rules() {
        // Domain [0, 31] with ticks 0..=31: 0 is dropped as a threshold, 31
        // stays, so the last bin is [31, 31].
        let thresholds = ticks(0.0, 31.0, 31.0);
        let data = [(0.0, 2u32), (1.0, 3), (30.5, 1), (31.0, 4), (40.0, 9)];
        let bins = bin::<u32, u32>((0.0, 31.0), &thresholds, data, |s, v| *s += v);
        assert_eq!(bins.len(), 32);
        assert_eq!((bins[0].x0, bins[0].x1, bins[0].sum), (0.0, 1.0, 2));
        assert_eq!(bins[1].sum, 3);
        assert_eq!(bins[30].sum, 1);
        assert_eq!((bins[31].x0, bins[31].x1, bins[31].sum), (31.0, 31.0, 4));
        assert_eq!(bins.iter().map(|b| b.len).sum::<usize>(), 4);
    }

    #[test]
    fn quantiles_over_counts() {
        let sorted = [(1.0, 2), (4.0, 1), (10.0, 1)];
        // Expanded: [1, 1, 4, 10].
        assert_eq!(quantile_counts(&sorted, 0.5), Some(2.5));
        assert_eq!(
            quantile_counts(&sorted, 0.95),
            Some(4.0 + 6.0 * (3.0 * 0.95 - 2.0))
        );
        assert_eq!(quantile_counts(&sorted, 0.0), Some(1.0));
        assert_eq!(quantile_counts(&sorted, 1.0), Some(10.0));
        assert_eq!(quantile_counts(&[], 0.5), None);
        assert_eq!(range(3.0, 6.0, 1.0), vec![3.0, 4.0, 5.0]);
    }
}
