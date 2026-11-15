//! Plain-text and SVG renderings of a [`StatsReport`] (`tanki stats
//! --format text|svg`, R6 §7.4). Desktop prints its web page to PDF; tanki
//! draws the same panels once more without a terminal: text with
//! eighth-block sparklines, and a standalone SVG document with bars,
//! stacked bars, the card-count strip and the calendar heatmap in desktop's
//! colours.
use super::{
    localized_number, CalendarPanel, HistogramPanel, Panels, RetentionMode, StatsReport, TableRow,
};
use std::fmt::Write;

/// Lower eighth blocks, index = eighths filled.
const LOWER: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// Calendar cells by intensity step (0 = no reviews).
const SHADES: [char; 5] = ['·', '░', '▒', '▓', '█'];

/// Desktop's graph colours (d3 schemes used by `ts/routes/graphs`).
mod colour {
    pub const NEW: &str = "#6baed6";
    pub const LEARN: &str = "#fd8d3c";
    pub const RELEARN: &str = "#fb6a4a";
    pub const YOUNG: &str = "#74c476";
    pub const MATURE: &str = "#31a354";
    pub const FILTERED: &str = "#9e9ac8";
    pub const SUSPENDED: &str = "#ffdc41";
    pub const BURIED: &str = "#808080";
    pub const BAR: &str = "#4292c6";
    pub const EMPTY: &str = "#e8e8e8";
    pub const HEAT: [&str; 4] = ["#bdd7e7", "#6baed6", "#3182bd", "#08519c"];
    pub const BUTTONS: [&str; 4] = ["#d73027", "#fee08b", "#a6d96a", "#1a9850"];
    /// Mature, young, relearn, learn, filtered (`REVIEW_KINDS`).
    pub const REVIEWS: [&str; 5] = [MATURE, YOUNG, RELEARN, LEARN, FILTERED];
}

/// The intensity step (0 = no reviews, 1–4) of a calendar day.
fn heat_level(intensity: f64) -> usize {
    if intensity <= 0.0 {
        0
    } else {
        ((intensity * 4.0).ceil() as usize).clamp(1, 4)
    }
}

/// One line of eighth blocks scaled to the largest value.
pub fn sparkline(values: &[u64]) -> String {
    let max = values.iter().copied().max().unwrap_or(0);
    values
        .iter()
        .map(|&v| {
            if max == 0 || v == 0 {
                LOWER[0]
            } else {
                LOWER[((v as f64 / max as f64 * 8.0).round() as usize).clamp(1, 8)]
            }
        })
        .collect()
}

/// The scope line: what the report covers.
fn scope_text(report: &StatsReport) -> String {
    let l = &report.panels.labels;
    let scope = if report.data.search.is_empty() {
        l.collection.clone()
    } else {
        format!("{}: {}", l.search, report.data.search)
    };
    let history = match report.data.history {
        super::History::Year => &l.one_year_history,
        super::History::All => &l.all_history,
    };
    format!("{scope} · {history}")
}

fn number(n: f64) -> String {
    localized_number(n, 2)
}

// ---------------------------------------------------------------- text

fn table_text(out: &mut String, rows: &[TableRow]) {
    let width = rows
        .iter()
        .map(|r| r.label.chars().count())
        .max()
        .unwrap_or(0);
    for r in rows {
        let pad = width - r.label.chars().count();
        let _ = writeln!(out, "  {}:{} {}", r.label, " ".repeat(pad), r.value);
    }
}

fn histogram_text(out: &mut String, panel: &HistogramPanel, range: Option<&str>, no_data: &str) {
    let _ = writeln!(out, "{}", panel.title);
    if !panel.subtitle.is_empty() {
        let _ = writeln!(out, "  {}", panel.subtitle);
    }
    if let Some(range) = range {
        let _ = writeln!(out, "  [{range}]");
    }
    match &panel.histogram {
        Some(h) if h.total > 0 => {
            let values: Vec<u64> = h.bins.iter().map(|b| b.value).collect();
            let _ = writeln!(out, "  {}", sparkline(&values));
            let first = h.bins.first().map(|b| b.x0).unwrap_or_default();
            let last = h.bins.last().map(|b| b.x1).unwrap_or_default();
            let suffix = if h.percent_axis { "%" } else { "" };
            let left = format!("{}{suffix}", number(first));
            let right = format!("{}{suffix}", number(last));
            let gap = values
                .len()
                .saturating_sub(left.chars().count() + right.chars().count());
            let _ = writeln!(out, "  {left}{}{right}", " ".repeat(gap.max(1)));
        }
        _ => {
            let _ = writeln!(out, "  {no_data}");
        }
    }
    table_text(out, &panel.table);
    out.push('\n');
}

fn calendar_text(out: &mut String, calendar: &CalendarPanel) {
    let _ = writeln!(out, "{} {}", calendar.title, calendar.year);
    let weeks = calendar
        .days
        .iter()
        .map(|d| d.week)
        .max()
        .map_or(0, |w| w + 1);
    let mut grid = vec![vec![' '; weeks as usize]; 7];
    for day in &calendar.days {
        grid[day.weekday as usize % 7][day.week as usize] = SHADES[heat_level(day.intensity)];
    }
    for (label, row) in calendar.weekday_labels.iter().zip(grid) {
        let cells: String = row.into_iter().collect();
        let _ = writeln!(out, "  {label} {}", cells.trim_end());
    }
    let days = calendar.days.iter().filter(|d| d.count > 0).count();
    let reviews: u64 = calendar.days.iter().map(|d| d.count).sum();
    let _ = writeln!(out, "  {days} days, {reviews} reviews\n");
}

/// The report as plain text: every visible panel's tables, with a
/// sparkline for each chart.
pub fn text(report: &StatsReport) -> String {
    let p = &report.panels;
    let l = &p.labels;
    let mut out = String::new();
    let _ = writeln!(out, "{} · {}\n", l.page_title, scope_text(report));

    let _ = writeln!(out, "{}", p.today.title);
    for line in &p.today.lines {
        let _ = writeln!(out, "  {line}");
    }
    out.push('\n');

    histogram_text(
        &mut out,
        &p.future_due.panel,
        Some(l.graph_range(p.future_due.range)),
        &l.no_data,
    );
    calendar_text(&mut out, &p.calendar);

    let r = &p.reviews;
    let _ = writeln!(
        out,
        "{}\n  {}\n  [{}]",
        r.title,
        r.subtitle,
        l.graph_range(r.range)
    );
    let totals: Vec<u64> = r.bins.iter().map(|b| b.values.iter().sum()).collect();
    if totals.iter().any(|t| *t > 0) {
        let _ = writeln!(out, "  {}", sparkline(&totals));
    } else {
        let _ = writeln!(out, "  {}", l.no_data);
    }
    table_text(&mut out, &r.table);
    out.push('\n');

    let c = &p.card_counts;
    let _ = writeln!(out, "{}", c.title);
    let width = c
        .rows
        .iter()
        .map(|r| r.label.chars().count())
        .max()
        .unwrap_or(0);
    for row in &c.rows {
        let pad = width - row.label.chars().count();
        let _ = writeln!(
            out,
            "  {}{} {:>8} {:>8}",
            row.label,
            " ".repeat(pad),
            row.count,
            row.percent
        );
    }
    let pad = width.saturating_sub(c.total_label.chars().count());
    let _ = writeln!(
        out,
        "  {}{} {:>8}\n",
        c.total_label,
        " ".repeat(pad),
        c.total
    );

    let interval = |range| Some(l.interval_range(range));
    histogram_text(
        &mut out,
        &p.intervals.panel,
        interval(p.intervals.range),
        &l.no_data,
    );
    if let Some(s) = &p.stability {
        histogram_text(&mut out, &s.panel, interval(s.range), &l.no_data);
    }
    if let Some(ease) = &p.ease {
        histogram_text(&mut out, ease, None, &l.no_data);
    }
    if let Some(d) = &p.difficulty {
        histogram_text(
            &mut out,
            &d.panel,
            Some(l.percent_range(d.range)),
            &l.no_data,
        );
    }
    if let Some(r) = &p.retrievability {
        histogram_text(
            &mut out,
            &r.panel,
            Some(l.percent_range(r.range)),
            &l.no_data,
        );
    }

    retention_text(&mut out, p);

    let h = &p.hours;
    let _ = writeln!(
        out,
        "{}\n  {}\n  [{}]",
        h.title,
        h.subtitle,
        l.graph_range(h.range)
    );
    if h.has_data {
        let totals: Vec<u64> = h.hours.iter().map(|h| h.total as u64).collect();
        let _ = writeln!(out, "  {}", sparkline(&totals));
        let _ = writeln!(out, "  0     6     12    18   23");
        for hour in h.hours.iter().filter(|h| h.total > 0) {
            let _ = writeln!(out, "  {}", hour.label.replace('\n', " · "));
        }
    } else {
        let _ = writeln!(out, "  {}", l.no_data);
    }
    out.push('\n');

    let b = &p.buttons;
    let _ = writeln!(
        out,
        "{}\n  {}\n  [{}]",
        b.title,
        b.subtitle,
        l.graph_range(b.range)
    );
    if b.has_data {
        let _ = writeln!(out, "  {:12} {}", "", b.button_labels.join(" / "));
        for g in &b.groups {
            let pressed = g.pressed.join(" / ");
            let _ = writeln!(out, "  {:12} {pressed}", g.label);
            let _ = writeln!(out, "  {:12} {}", "", g.correct_label);
        }
    } else {
        let _ = writeln!(out, "  {}", l.no_data);
    }
    out.push('\n');

    histogram_text(
        &mut out,
        &p.added.panel,
        Some(l.graph_range(p.added.range)),
        &l.no_data,
    );
    let mut text: String = out
        .lines()
        .map(|l| l.trim_end().to_owned() + "\n")
        .collect();
    while text.ends_with("\n\n") {
        text.pop();
    }
    text
}

fn retention_text(out: &mut String, p: &Panels) {
    let t = &p.true_retention;
    let _ = writeln!(out, "{}\n  {}", t.title, t.subtitle);
    let width = t
        .rows
        .iter()
        .map(|r| r.title.chars().count())
        .max()
        .unwrap_or(0);
    let h = &t.headings;
    let heading = |i: usize| h.get(i).map(String::as_str).unwrap_or("");
    match t.mode {
        RetentionMode::Summary => {
            let _ = writeln!(
                out,
                "  {:width$} {:>10} {:>10} {:>10} {:>10}",
                "",
                heading(0),
                heading(1),
                heading(2),
                heading(3)
            );
            for r in &t.rows {
                let _ = writeln!(
                    out,
                    "  {:width$} {:>10} {:>10} {:>10} {:>10}",
                    r.title,
                    r.young.retention,
                    r.mature.retention,
                    r.all.retention,
                    r.all.passed + r.all.failed
                );
            }
        }
        RetentionMode::Young | RetentionMode::Mature => {
            let _ = writeln!(
                out,
                "  {:width$} {:>10} {:>10} {:>10}",
                "",
                heading(4),
                heading(5),
                heading(6)
            );
            for r in &t.rows {
                let c = if t.mode == RetentionMode::Young {
                    &r.young
                } else {
                    &r.mature
                };
                let _ = writeln!(
                    out,
                    "  {:width$} {:>10} {:>10} {:>10}",
                    r.title, c.passed, c.failed, c.retention
                );
            }
        }
    }
    out.push('\n');
}

// ----------------------------------------------------------------- svg

const WIDTH: f64 = 800.0;
const MARGIN: f64 = 20.0;
const CHART_HEIGHT: f64 = 120.0;
const LINE: f64 = 16.0;

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && c != '\n' && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

/// An SVG document drawn top to bottom.
struct Svg {
    body: String,
    y: f64,
}

impl Svg {
    fn text(&mut self, x: f64, text: &str, class: &str) {
        let _ = writeln!(
            self.body,
            r#"<text x="{x:.1}" y="{:.1}" class="{class}">{}</text>"#,
            self.y,
            escape(text)
        );
    }
    fn line(&mut self, text: &str, class: &str) {
        self.y += LINE;
        self.text(MARGIN, text, class);
    }
    fn rect(&mut self, x: f64, y: f64, w: f64, h: f64, fill: &str, title: Option<&str>) {
        let _ = write!(
            self.body,
            r#"<rect x="{x:.1}" y="{y:.1}" width="{:.1}" height="{:.1}" fill="{fill}""#,
            w.max(0.0),
            h.max(0.0)
        );
        match title {
            Some(title) => {
                let _ = writeln!(self.body, "><title>{}</title></rect>", escape(title));
            }
            None => self.body.push_str("/>\n"),
        }
    }
    fn heading(&mut self, title: &str, subtitle: &str) {
        self.y += LINE * 2.0;
        self.text(MARGIN, title, "title");
        if !subtitle.is_empty() {
            self.line(subtitle, "subtitle");
        }
    }
    fn table(&mut self, rows: &[TableRow]) {
        for r in rows {
            self.y += LINE;
            self.text(MARGIN, &r.label, "label");
            self.text(MARGIN + 260.0, &r.value, "value");
        }
    }
    /// Vertical bars from stacks of `(value, colour)`, bottom first, with
    /// the largest value and the first/last x labels.
    fn bars(&mut self, stacks: &[Vec<(f64, &str)>], titles: &[String], left: &str, right: &str) {
        let max = stacks
            .iter()
            .map(|s| s.iter().map(|(v, _)| v).sum::<f64>())
            .fold(0.0, f64::max);
        let top = self.y + 8.0;
        let x0 = MARGIN + 40.0;
        let width = WIDTH - x0 - MARGIN;
        let _ = writeln!(
            self.body,
            r#"<text x="{:.1}" y="{:.1}" class="axis" text-anchor="end">{}</text>"#,
            x0 - 4.0,
            top + 10.0,
            escape(&number(max))
        );
        let _ = writeln!(
            self.body,
            r#"<text x="{:.1}" y="{:.1}" class="axis" text-anchor="end">0</text>"#,
            x0 - 4.0,
            top + CHART_HEIGHT
        );
        let count = stacks.len().max(1) as f64;
        let step = width / count;
        let bar = (step * 0.85).max(1.0);
        for (i, stack) in stacks.iter().enumerate() {
            let x = x0 + i as f64 * step;
            let mut base = top + CHART_HEIGHT;
            for (value, fill) in stack {
                if *value <= 0.0 || max <= 0.0 {
                    continue;
                }
                let h = value / max * CHART_HEIGHT;
                base -= h;
                self.rect(x, base, bar, h, fill, titles.get(i).map(String::as_str));
            }
        }
        let _ = writeln!(
            self.body,
            r##"<line x1="{x0:.1}" y1="{y:.1}" x2="{:.1}" y2="{y:.1}" stroke="#999"/>"##,
            x0 + width,
            y = top + CHART_HEIGHT
        );
        self.y = top + CHART_HEIGHT + LINE;
        self.text(x0, left, "axis");
        let _ = writeln!(
            self.body,
            r#"<text x="{:.1}" y="{:.1}" class="axis" text-anchor="end">{}</text>"#,
            x0 + width,
            self.y,
            escape(right)
        );
    }
    fn histogram(&mut self, panel: &HistogramPanel, range: Option<&str>, no_data: &str) {
        self.heading(&panel.title, &panel.subtitle);
        if let Some(range) = range {
            self.line(range, "range");
        }
        match &panel.histogram {
            Some(h) if h.total > 0 => {
                let stacks: Vec<Vec<(f64, &str)>> = h
                    .bins
                    .iter()
                    .map(|b| vec![(b.value as f64, colour::BAR)])
                    .collect();
                let titles: Vec<String> = h.bins.iter().map(|b| b.label.clone()).collect();
                let suffix = if h.percent_axis { "%" } else { "" };
                let first = h.bins.first().map(|b| b.x0).unwrap_or_default();
                let last = h.bins.last().map(|b| b.x1).unwrap_or_default();
                self.bars(
                    &stacks,
                    &titles,
                    &format!("{}{suffix}", number(first)),
                    &format!("{}{suffix}", number(last)),
                );
            }
            _ => self.line(no_data, "subtitle"),
        }
        self.table(&panel.table);
    }
    fn calendar(&mut self, calendar: &CalendarPanel) {
        self.heading(&format!("{} {}", calendar.title, calendar.year), "");
        let cell = 12.0;
        let top = self.y + 6.0;
        let x0 = MARGIN + 20.0;
        for (row, label) in calendar.weekday_labels.iter().enumerate() {
            let _ = writeln!(
                self.body,
                r#"<text x="{MARGIN:.1}" y="{:.1}" class="axis">{}</text>"#,
                top + row as f64 * cell + 10.0,
                escape(label)
            );
        }
        for day in &calendar.days {
            let level = heat_level(day.intensity);
            let fill = if level == 0 {
                colour::EMPTY
            } else {
                colour::HEAT[level - 1]
            };
            self.rect(
                x0 + day.week as f64 * cell,
                top + day.weekday as f64 * cell,
                cell - 2.0,
                cell - 2.0,
                fill,
                Some(&day.label.replace('\n', " ")),
            );
        }
        self.y = top + 7.0 * cell;
    }
}

const STYLE: &str = "text{font-family:sans-serif;font-size:12px;fill:#222}\
.page{font-size:20px;font-weight:bold}.title{font-size:16px;font-weight:bold}\
.subtitle,.range{fill:#666}.axis{font-size:10px;fill:#666}.value{fill:#000}";

/// The report as a standalone SVG document (every visible panel, stacked
/// in page order). Bars carry desktop's tooltip text as `<title>`.
pub fn svg(report: &StatsReport) -> String {
    let p = &report.panels;
    let l = &p.labels;
    let mut s = Svg {
        body: String::new(),
        y: MARGIN,
    };
    s.y += LINE;
    s.text(MARGIN, &l.page_title, "page");
    s.line(&scope_text(report), "subtitle");

    s.heading(&p.today.title, "");
    for line in &p.today.lines {
        s.line(line, "label");
    }

    s.histogram(
        &p.future_due.panel,
        Some(l.graph_range(p.future_due.range)),
        &l.no_data,
    );
    s.calendar(&p.calendar);

    let r = &p.reviews;
    s.heading(&r.title, &r.subtitle);
    s.line(l.graph_range(r.range), "range");
    if r.bins.iter().any(|b| b.values.iter().any(|v| *v > 0)) {
        let stacks: Vec<Vec<(f64, &str)>> = r
            .bins
            .iter()
            .map(|b| {
                b.values
                    .iter()
                    .zip(colour::REVIEWS)
                    .map(|(v, c)| (*v as f64, c))
                    .collect()
            })
            .collect();
        let titles: Vec<String> = r.bins.iter().map(|b| b.label.clone()).collect();
        let first = r.bins.first().map(|b| b.x0).unwrap_or_default();
        let last = r.bins.last().map(|b| b.x1).unwrap_or_default();
        s.bars(&stacks, &titles, &number(first), &number(last));
        // Legend.
        s.y += LINE;
        let y = s.y;
        let mut x = MARGIN;
        for (label, fill) in r.kind_labels.iter().zip(colour::REVIEWS) {
            s.rect(x, y - 9.0, 10.0, 10.0, fill, None);
            s.text(x + 14.0, label, "label");
            x += 24.0 + label.chars().count() as f64 * 7.0;
        }
    } else {
        s.line(&l.no_data, "subtitle");
    }
    s.table(&r.table);

    // Card counts: desktop's pie as one proportional strip with a legend.
    let c = &p.card_counts;
    s.heading(&c.title, "");
    let total: u64 = c.rows.iter().map(|r| r.count as u64).sum();
    let top = s.y + 8.0;
    let mut x = MARGIN;
    let width = WIDTH - 2.0 * MARGIN;
    for row in &c.rows {
        if total == 0 || row.count == 0 {
            continue;
        }
        let w = row.count as f64 / total as f64 * width;
        let fill = count_colour(row.kind);
        let title = format!("{}: {} ({})", row.label, row.count, row.percent);
        s.rect(x, top, w, 20.0, fill, Some(&title));
        x += w;
    }
    s.y = top + 20.0;
    for row in &c.rows {
        s.y += LINE;
        let y = s.y;
        s.rect(MARGIN, y - 9.0, 10.0, 10.0, count_colour(row.kind), None);
        s.text(MARGIN + 14.0, &row.label, "label");
        s.text(MARGIN + 160.0, &row.count.to_string(), "value");
        s.text(MARGIN + 230.0, &row.percent, "value");
    }
    s.y += LINE;
    s.text(MARGIN + 14.0, &c.total_label, "label");
    s.text(MARGIN + 160.0, &c.total, "value");

    s.histogram(
        &p.intervals.panel,
        Some(l.interval_range(p.intervals.range)),
        &l.no_data,
    );
    if let Some(st) = &p.stability {
        s.histogram(&st.panel, Some(l.interval_range(st.range)), &l.no_data);
    }
    if let Some(ease) = &p.ease {
        s.histogram(ease, None, &l.no_data);
    }
    if let Some(d) = &p.difficulty {
        s.histogram(&d.panel, Some(l.percent_range(d.range)), &l.no_data);
    }
    if let Some(r) = &p.retrievability {
        s.histogram(&r.panel, Some(l.percent_range(r.range)), &l.no_data);
    }

    let t = &p.true_retention;
    s.heading(&t.title, &t.subtitle);
    s.y += LINE;
    for (i, heading) in t.headings.iter().take(4).enumerate() {
        s.text(MARGIN + 140.0 + i as f64 * 110.0, heading, "label");
    }
    for row in &t.rows {
        s.y += LINE;
        s.text(MARGIN, &row.title, "label");
        let count = (row.all.passed + row.all.failed).to_string();
        for (i, value) in [
            row.young.retention.as_str(),
            row.mature.retention.as_str(),
            row.all.retention.as_str(),
            count.as_str(),
        ]
        .into_iter()
        .enumerate()
        {
            s.text(MARGIN + 140.0 + i as f64 * 110.0, value, "value");
        }
    }

    let h = &p.hours;
    s.heading(&h.title, &h.subtitle);
    s.line(l.graph_range(h.range), "range");
    if h.has_data {
        // Correct answers below, the rest above (desktop: bars + a line).
        let stacks: Vec<Vec<(f64, &str)>> = h
            .hours
            .iter()
            .map(|h| {
                vec![
                    (h.correct as f64, colour::BAR),
                    ((h.total - h.correct.min(h.total)) as f64, colour::HEAT[0]),
                ]
            })
            .collect();
        let titles: Vec<String> = h.hours.iter().map(|h| h.label.clone()).collect();
        s.bars(&stacks, &titles, "0", "23");
    } else {
        s.line(&l.no_data, "subtitle");
    }

    let b = &p.buttons;
    s.heading(&b.title, &b.subtitle);
    s.line(l.graph_range(b.range), "range");
    if b.has_data {
        let mut stacks = Vec::new();
        let mut titles = Vec::new();
        for g in &b.groups {
            for (i, count) in g.counts.iter().enumerate() {
                stacks.push(vec![(*count as f64, colour::BUTTONS[i])]);
                titles.push(format!(
                    "{} {}: {}",
                    g.label, b.button_labels[i], g.pressed[i]
                ));
            }
            stacks.push(Vec::new());
            titles.push(String::new());
        }
        stacks.pop();
        let first = b.groups.first().map(|g| g.label.as_str()).unwrap_or("");
        let last = b.groups.last().map(|g| g.label.as_str()).unwrap_or("");
        s.bars(&stacks, &titles, first, last);
        for g in &b.groups {
            s.line(&format!("{}: {}", g.label, g.correct_label), "label");
        }
    } else {
        s.line(&l.no_data, "subtitle");
    }

    s.histogram(
        &p.added.panel,
        Some(l.graph_range(p.added.range)),
        &l.no_data,
    );

    let height = (s.y + MARGIN * 2.0).ceil();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{WIDTH}\" height=\"{height}\" \
         viewBox=\"0 0 {WIDTH} {height}\">\n<style>{STYLE}</style>\n\
         <rect width=\"100%\" height=\"100%\" fill=\"#fff\"/>\n{}</svg>\n",
        s.body
    )
}

fn count_colour(kind: super::CountKind) -> &'static str {
    use super::CountKind as K;
    match kind {
        K::New => colour::NEW,
        K::Learning => colour::LEARN,
        K::Relearning => colour::RELEARN,
        K::Young => colour::YOUNG,
        K::Mature => colour::MATURE,
        K::Suspended => colour::SUSPENDED,
        K::Buried => colour::BURIED,
    }
}

#[cfg(test)]
mod tests;
