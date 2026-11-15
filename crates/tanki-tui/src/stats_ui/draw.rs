//! Drawing the statistics panels. Every panel is a bordered block of a
//! fixed height for a given width, stacked into one column that scrolls.
use super::{visible_panels, Item, PanelKind, StatsUi};
use crate::charts::{self, palette, Heatmap, PieBar, Stack, StackedBars};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Widget},
};
use tanki_core::stats::{
    localized_number, CountKind, GraphRange, Histogram, HistogramPanel, History, IntervalRange,
    PercentRange, RetentionMode, StatsScope,
};
use unicode_width::UnicodeWidthStr;

const CHART_ROWS: u16 = 8;
const SMALL_CHART_ROWS: u16 = 6;
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn put(buf: &mut Buffer, x: u16, y: u16, text: &str, right: u16, style: Style) -> u16 {
    if x >= right || y >= buf.area.bottom() {
        return x;
    }
    buf.set_stringn(x, y, text, (right - x) as usize, style).0
}

fn put_line(buf: &mut Buffer, x: u16, y: u16, line: &Line<'_>, right: u16) {
    if x < right && y < buf.area.bottom() {
        buf.set_line(x, y, line, right - x);
    }
}

fn number(n: f64) -> String {
    localized_number(n, 2)
}

fn axis_value(x: f64, percent: bool) -> String {
    let text = localized_number(x, 2);
    if percent {
        format!("{text}%")
    } else {
        text
    }
}

/// Bars of a histogram fitted to a width: adjacent bins are grouped when
/// there are more bins than columns.
struct Columns {
    stacks: Vec<Stack>,
    items: Vec<Item>,
    bar_width: u16,
    first: f64,
    last: f64,
    max: f64,
}

fn group_query(queries: &[&Option<String>]) -> Option<String> {
    let parts: Vec<&String> = queries.iter().filter_map(|q| q.as_ref()).collect();
    match parts.len() {
        0 => None,
        1 => Some(parts[0].clone()),
        _ => Some(
            parts
                .iter()
                .map(|q| format!("({q})"))
                .collect::<Vec<_>>()
                .join(" OR "),
        ),
    }
}

fn fit(count: usize, width: u16) -> (usize, u16) {
    let width = width.max(1) as usize;
    let group = count.div_ceil(width).max(1);
    let columns = count.div_ceil(group).max(1);
    let bar = (width / columns).clamp(1, 3) as u16;
    (group, bar)
}

fn histogram_columns(h: &Histogram, width: u16, colour: Color) -> Columns {
    let (group, bar_width) = fit(h.bins.len(), width);
    let mut stacks = Vec::new();
    let mut items = Vec::new();
    for chunk in h.bins.chunks(group) {
        let value: u64 = chunk.iter().map(|b| b.value).sum();
        stacks.push(vec![(value as f64, colour)]);
        let label = if chunk.len() == 1 {
            chunk[0].label.clone()
        } else {
            format!(
                "{}–{}: {}",
                axis_value(chunk[0].x0, h.percent_axis),
                axis_value(chunk[chunk.len() - 1].x1, h.percent_axis),
                number(value as f64)
            )
        };
        let queries: Vec<&Option<String>> = chunk.iter().map(|b| &b.query).collect();
        items.push(Item {
            label,
            query: group_query(&queries),
        });
    }
    Columns {
        max: StackedBars::largest(&stacks),
        stacks,
        items,
        bar_width,
        first: h.bins.first().map(|b| b.x0).unwrap_or_default(),
        last: h.bins.last().map(|b| b.x1).unwrap_or_default(),
    }
}

const REVIEW_COLOURS: [Color; 5] = [
    palette::MATURE,
    palette::YOUNG,
    palette::RELEARN,
    palette::LEARN,
    palette::FILTERED,
];

fn reviews_columns(ui: &StatsUi, width: u16) -> Columns {
    let p = &ui.panels.reviews;
    let (group, bar_width) = fit(p.bins.len(), width);
    let mut stacks = Vec::new();
    let mut items = Vec::new();
    for chunk in p.bins.chunks(group) {
        let mut values = [0u64; 5];
        for bin in chunk {
            for (v, b) in values.iter_mut().zip(bin.values) {
                *v += b;
            }
        }
        stacks.push(
            values
                .iter()
                .zip(REVIEW_COLOURS)
                .map(|(v, c)| (*v as f64, c))
                .collect(),
        );
        let breakdown = p
            .kind_labels
            .iter()
            .zip(values)
            .filter(|(_, v)| *v > 0)
            .map(|(label, v)| format!("{label} {}", number(v as f64)))
            .collect::<Vec<_>>()
            .join(", ");
        let head = if chunk.len() == 1 {
            chunk[0].label.clone()
        } else {
            format!(
                "{}–{}",
                number(chunk[0].x0),
                number(chunk[chunk.len() - 1].x1)
            )
        };
        items.push(Item {
            label: if breakdown.is_empty() {
                head
            } else {
                format!("{head} ({breakdown})")
            },
            query: None,
        });
    }
    Columns {
        max: StackedBars::largest(&stacks),
        stacks,
        items,
        bar_width,
        first: p.bins.first().map(|b| b.x0).unwrap_or_default(),
        last: p.bins.last().map(|b| b.x1).unwrap_or_default(),
    }
}

fn histogram_of(ui: &StatsUi, kind: PanelKind) -> Option<&HistogramPanel> {
    let p = &ui.panels;
    Some(match kind {
        PanelKind::FutureDue => &p.future_due.panel,
        PanelKind::Intervals => &p.intervals.panel,
        PanelKind::Stability => &p.stability.as_ref()?.panel,
        PanelKind::Ease => p.ease.as_ref()?,
        PanelKind::Difficulty => &p.difficulty.as_ref()?.panel,
        PanelKind::Retrievability => &p.retrievability.as_ref()?.panel,
        PanelKind::Added => &p.added.panel,
        _ => return None,
    })
}

/// Width of the y axis labels for a maximum.
fn axis_width(max: f64) -> u16 {
    number(max).width() as u16 + 1
}

fn chart_width(inner: u16, max: f64) -> u16 {
    inner.saturating_sub(axis_width(max))
}

/// The selectable elements of a panel.
pub(super) fn items(ui: &StatsUi, kind: PanelKind, width: u16) -> Vec<Item> {
    let inner = width.saturating_sub(2);
    if let Some(panel) = histogram_of(ui, kind) {
        let Some(h) = &panel.histogram else {
            return Vec::new();
        };
        let max = h.bins.iter().map(|b| b.value).max().unwrap_or(0) as f64;
        return histogram_columns(h, chart_width(inner, max), palette::BAR).items;
    }
    let p = &ui.panels;
    match kind {
        PanelKind::Reviews => {
            let max = p
                .reviews
                .bins
                .iter()
                .map(|b| b.values.iter().sum::<u64>())
                .max()
                .unwrap_or(0) as f64;
            reviews_columns(ui, chart_width(inner, max)).items
        }
        PanelKind::Calendar => p
            .calendar
            .days
            .iter()
            .map(|d| Item {
                label: d.label.clone(),
                query: d.query.clone(),
            })
            .collect(),
        PanelKind::CardCounts => p
            .card_counts
            .rows
            .iter()
            .map(|r| Item {
                label: format!("{}: {} ({})", r.label, r.count, r.percent),
                query: Some(r.query.clone()),
            })
            .collect(),
        PanelKind::Hours if p.hours.has_data => p
            .hours
            .hours
            .iter()
            .map(|h| Item {
                label: h.label.clone(),
                query: None,
            })
            .collect(),
        PanelKind::Buttons if p.buttons.has_data => {
            let names = &p.buttons.button_labels;
            p.buttons
                .groups
                .iter()
                .flat_map(|g| {
                    (0..4).map(move |b| Item {
                        label: format!(
                            "{}: {} ({}) · {} · {}",
                            g.label,
                            b + 1,
                            names[b],
                            g.pressed[b],
                            g.correct_label
                        ),
                        query: None,
                    })
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

/// Lines a panel takes at this outer width (borders included).
pub(super) fn panel_height(ui: &StatsUi, kind: PanelKind, _width: u16) -> u16 {
    let p = &ui.panels;
    let border = 2;
    if let Some(panel) = histogram_of(ui, kind) {
        let controls = u16::from(kind != PanelKind::Ease);
        let body = if panel.histogram.is_some() {
            CHART_ROWS + 1
        } else {
            1
        };
        return border + 1 + controls + body + panel.table.len() as u16;
    }
    border
        + match kind {
            PanelKind::Today => p.today.lines.len() as u16,
            PanelKind::Calendar => {
                if p.calendar.days.is_empty() {
                    2
                } else {
                    2 + 7
                }
            }
            PanelKind::Reviews => {
                if p.reviews.bins.is_empty() {
                    2 + 1
                } else {
                    3 + CHART_ROWS + 1 + p.reviews.table.len() as u16
                }
            }
            PanelKind::CardCounts => 1 + 1 + p.card_counts.rows.len() as u16 + 1,
            PanelKind::TrueRetention => 1 + 1 + 1 + p.true_retention.rows.len() as u16,
            PanelKind::Hours => {
                if p.hours.has_data {
                    2 + SMALL_CHART_ROWS + 2
                } else {
                    3
                }
            }
            PanelKind::Buttons => {
                if p.buttons.has_data {
                    2 + SMALL_CHART_ROWS + 1 + 3
                } else {
                    3
                }
            }
            _ => 1,
        }
}

fn panel_title(ui: &StatsUi, kind: PanelKind) -> String {
    let p = &ui.panels;
    match kind {
        PanelKind::Today => p.today.title.clone(),
        PanelKind::Calendar => p.calendar.title.clone(),
        PanelKind::Reviews => p.reviews.title.clone(),
        PanelKind::CardCounts => p.card_counts.title.clone(),
        PanelKind::TrueRetention => p.true_retention.title.clone(),
        PanelKind::Hours => p.hours.title.clone(),
        PanelKind::Buttons => p.buttons.title.clone(),
        other => histogram_of(ui, other)
            .map(|h| h.title.clone())
            .unwrap_or_default(),
    }
}

fn subtitle(ui: &StatsUi, kind: PanelKind) -> Option<String> {
    let p = &ui.panels;
    match kind {
        PanelKind::Reviews => Some(p.reviews.subtitle.clone()),
        PanelKind::TrueRetention => Some(p.true_retention.subtitle.clone()),
        PanelKind::Hours => Some(p.hours.subtitle.clone()),
        PanelKind::Buttons => Some(p.buttons.subtitle.clone()),
        other => histogram_of(ui, other).map(|h| h.subtitle.clone()),
    }
}

/// "r: 1 month · 3 months · 1 year" with the current choice reversed.
fn choices_line<'a>(key: &'a str, choices: Vec<(String, bool)>) -> Vec<Span<'a>> {
    let mut spans = vec![Span::styled(format!("{key}: "), dim())];
    for (i, (label, current)) in choices.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", dim()));
        }
        spans.push(if current {
            Span::styled(label, Style::default().add_modifier(Modifier::REVERSED))
        } else {
            Span::raw(label)
        });
    }
    spans
}

fn graph_choices(ui: &StatsUi, current: GraphRange, all_time: bool) -> Vec<(String, bool)> {
    GraphRange::choices(all_time)
        .iter()
        .map(|r| (ui.panels.labels.graph_range(*r).to_owned(), *r == current))
        .collect()
}

fn toggle<'a>(key: &'a str, label: &str, on: bool) -> Vec<Span<'a>> {
    vec![
        Span::raw("   "),
        Span::styled(format!("{key}: "), dim()),
        Span::raw(format!("[{}] {label}", if on { "x" } else { " " })),
    ]
}

fn controls(ui: &StatsUi, kind: PanelKind) -> Option<Line<'static>> {
    let p = &ui.panels;
    let all = ui.history == History::All;
    let spans: Vec<Span<'static>> = match kind {
        PanelKind::FutureDue => {
            let mut spans = choices_line("r", graph_choices(ui, p.future_due.range, true));
            if p.future_due.have_backlog {
                spans.extend(toggle("b", &p.labels.backlog, p.future_due.backlog));
            }
            spans
        }
        PanelKind::Added => choices_line("r", graph_choices(ui, p.added.range, true)),
        PanelKind::Reviews => {
            let mut spans = choices_line("r", graph_choices(ui, p.reviews.range, all));
            spans.extend(toggle("t", &p.labels.review_time, p.reviews.time));
            spans
        }
        PanelKind::Hours => choices_line("r", graph_choices(ui, p.hours.range, all)),
        PanelKind::Buttons => choices_line("r", graph_choices(ui, p.buttons.range, all)),
        PanelKind::Intervals | PanelKind::Stability => {
            let current = if kind == PanelKind::Intervals {
                p.intervals.range
            } else {
                p.stability.as_ref().map(|s| s.range).unwrap_or_default()
            };
            choices_line(
                "r",
                IntervalRange::CHOICES
                    .iter()
                    .map(|r| (p.labels.interval_range(*r).to_owned(), *r == current))
                    .collect(),
            )
        }
        PanelKind::Difficulty | PanelKind::Retrievability => {
            let current = if kind == PanelKind::Difficulty {
                p.difficulty.as_ref().map(|d| d.range)
            } else {
                p.retrievability.as_ref().map(|d| d.range)
            }
            .unwrap_or_default();
            choices_line(
                "r",
                PercentRange::CHOICES
                    .iter()
                    .map(|r| (p.labels.percent_range(*r).to_owned(), *r == current))
                    .collect(),
            )
        }
        PanelKind::CardCounts => {
            let mut spans = toggle(
                "i",
                &p.labels.separate_inactive,
                p.card_counts.separate_inactive,
            );
            spans.remove(0);
            spans
        }
        PanelKind::TrueRetention => {
            let h = &p.true_retention.headings;
            let mode = p.true_retention.mode;
            choices_line(
                "m",
                vec![
                    (h[0].clone(), mode == RetentionMode::Young),
                    (h[1].clone(), mode == RetentionMode::Mature),
                    (h[2].clone(), mode == RetentionMode::Summary),
                ],
            )
        }
        PanelKind::Calendar => {
            let c = &p.calendar;
            vec![
                Span::styled("[ ", dim()),
                Span::raw(if c.year > c.min_year { "◄ " } else { "  " }),
                Span::styled(
                    c.year.to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(if c.year < c.max_year { " ►" } else { "  " }),
                Span::styled(" ]", dim()),
                Span::raw("   "),
                Span::styled("w: ", dim()),
                Span::raw(c.weekday_labels.first().cloned().unwrap_or_default()),
            ]
        }
        _ => return None,
    };
    Some(Line::from(spans))
}

/// The y axis (max at the top, 0 at the bottom) and the bars.
fn draw_bars(
    buf: &mut Buffer,
    area: Rect,
    columns: &Columns,
    selected: Option<usize>,
    percent_axis: bool,
    x_labels: bool,
) {
    let axis = axis_width(columns.max);
    let rows = area.height - u16::from(x_labels);
    put(
        buf,
        area.x,
        area.y,
        &number(columns.max),
        area.x + axis,
        dim(),
    );
    put(
        buf,
        area.x + axis - 2,
        area.y + rows - 1,
        "0",
        area.x + axis,
        dim(),
    );
    let chart = Rect::new(area.x + axis, area.y, area.width.saturating_sub(axis), rows);
    StackedBars::new(&columns.stacks)
        .max(columns.max)
        .bar_width(columns.bar_width)
        .selected(selected)
        .render(chart, buf);
    if x_labels {
        let y = area.y + rows;
        let first = axis_value(columns.first, percent_axis);
        let last = axis_value(columns.last, percent_axis);
        put(buf, chart.x, y, &first, chart.right(), dim());
        let drawn = columns.stacks.len() as u16 * columns.bar_width;
        let end = (chart.x + drawn).min(chart.right());
        let start = end
            .saturating_sub(last.width() as u16)
            .max(chart.x + first.width() as u16 + 1);
        put(buf, start, y, &last, chart.right(), dim());
    }
}

fn draw_table(buf: &mut Buffer, x: u16, mut y: u16, right: u16, rows: &[(String, String)]) -> u16 {
    let label_width = rows.iter().map(|(l, _)| l.width()).max().unwrap_or(0) as u16 + 2;
    for (label, value) in rows {
        put(buf, x, y, &format!("{label}:"), right, dim());
        put(buf, x + label_width, y, value, right, Style::default());
        y += 1;
    }
    y
}

fn draw_panel(ui: &StatsUi, kind: PanelKind, focused: bool, area: Rect, buf: &mut Buffer) {
    let border = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        dim()
    };
    let title_style = if focused {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(Span::styled(
            format!(" {} ", panel_title(ui, kind)),
            title_style,
        ));
    let inner = block.inner(area);
    block.render(area, buf);
    let right = inner.right();
    let mut y = inner.y;
    let x = inner.x;
    let selected = if focused { ui.cursor } else { None };
    let p = &ui.panels;
    let no_data = |buf: &mut Buffer, y: u16| {
        put(buf, x, y, &p.labels.no_data, right, dim());
    };
    if kind != PanelKind::Today && kind != PanelKind::CardCounts && kind != PanelKind::Calendar {
        if let Some(sub) = subtitle(ui, kind) {
            put(buf, x, y, &sub, right, dim());
            y += 1;
        }
    }
    if let Some(line) = controls(ui, kind) {
        put_line(buf, x, y, &line, right);
        y += 1;
    }
    if let Some(panel) = histogram_of(ui, kind) {
        match &panel.histogram {
            Some(h) => {
                let max = h.bins.iter().map(|b| b.value).max().unwrap_or(0) as f64;
                let columns = histogram_columns(h, chart_width(inner.width, max), palette::BAR);
                let chart = Rect::new(x, y, inner.width, CHART_ROWS + 1);
                draw_bars(buf, chart, &columns, selected, h.percent_axis, true);
                y += CHART_ROWS + 1;
            }
            None => {
                no_data(buf, y);
                y += 1;
            }
        }
        let rows: Vec<(String, String)> = panel
            .table
            .iter()
            .map(|r| (r.label.clone(), r.value.clone()))
            .collect();
        draw_table(buf, x, y, right, &rows);
        return;
    }
    match kind {
        PanelKind::Today => {
            for line in &p.today.lines {
                put(buf, x, y, line, right, Style::default());
                y += 1;
            }
        }
        PanelKind::Calendar => draw_calendar(
            ui,
            Rect::new(x, y, inner.width, inner.bottom() - y),
            selected,
            buf,
        ),
        PanelKind::Reviews => {
            if p.reviews.bins.is_empty() {
                no_data(buf, y);
                return;
            }
            let mut legend = Vec::new();
            for (label, colour) in p.reviews.kind_labels.iter().zip(REVIEW_COLOURS) {
                legend.push(Span::styled("■ ", Style::default().fg(colour)));
                legend.push(Span::raw(format!("{label}  ")));
            }
            put_line(buf, x, y, &Line::from(legend), right);
            y += 1;
            let max = p
                .reviews
                .bins
                .iter()
                .map(|b| b.values.iter().sum::<u64>())
                .max()
                .unwrap_or(0) as f64;
            let columns = reviews_columns(ui, chart_width(inner.width, max));
            draw_bars(
                buf,
                Rect::new(x, y, inner.width, CHART_ROWS + 1),
                &columns,
                selected,
                false,
                true,
            );
            y += CHART_ROWS + 1;
            let rows: Vec<(String, String)> = p
                .reviews
                .table
                .iter()
                .map(|r| (r.label.clone(), r.value.clone()))
                .collect();
            draw_table(buf, x, y, right, &rows);
        }
        PanelKind::CardCounts => {
            let segments: Vec<(f64, Color)> = p
                .card_counts
                .rows
                .iter()
                .map(|r| (r.count as f64, count_colour(r.kind)))
                .collect();
            PieBar {
                segments: &segments,
            }
            .render(Rect::new(x, y, inner.width, 1), buf);
            y += 1;
            let label_width = p
                .card_counts
                .rows
                .iter()
                .map(|r| r.label.width())
                .max()
                .unwrap_or(0) as u16;
            let label_width = label_width.max(p.card_counts.total_label.width() as u16) + 3;
            for (i, r) in p.card_counts.rows.iter().enumerate() {
                let style = if selected == Some(i) {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                put(
                    buf,
                    x,
                    y,
                    "■",
                    right,
                    Style::default().fg(count_colour(r.kind)),
                );
                put(buf, x + 2, y, &r.label, right, style);
                put(
                    buf,
                    x + label_width,
                    y,
                    &format!(
                        "{:>8}  {:>7}",
                        localized_number(r.count as f64, 0),
                        r.percent
                    ),
                    right,
                    Style::default(),
                );
                y += 1;
            }
            put(buf, x + 2, y, &p.card_counts.total_label, right, dim());
            put(
                buf,
                x + label_width,
                y,
                &format!("{:>8}", p.card_counts.total),
                right,
                Style::default(),
            );
        }
        PanelKind::TrueRetention => {
            let t = &p.true_retention;
            let title_width = t.rows.iter().map(|r| r.title.width()).max().unwrap_or(0) as u16 + 2;
            let headings: Vec<&String> = match t.mode {
                RetentionMode::Summary => t.headings[..4].iter().collect(),
                _ => t.headings[4..].iter().collect(),
            };
            let col = 11;
            for (i, h) in headings.iter().enumerate() {
                let text = format!("{h:>10}");
                put(
                    buf,
                    x + title_width + i as u16 * col,
                    y,
                    &text,
                    right,
                    dim(),
                );
            }
            y += 1;
            for r in &t.rows {
                put(buf, x, y, &r.title, right, dim());
                let cells: Vec<String> = match t.mode {
                    RetentionMode::Summary => vec![
                        r.young.retention.clone(),
                        r.mature.retention.clone(),
                        r.all.retention.clone(),
                        localized_number((r.all.passed + r.all.failed) as f64, 0),
                    ],
                    RetentionMode::Young => vec![
                        localized_number(r.young.passed as f64, 0),
                        localized_number(r.young.failed as f64, 0),
                        r.young.retention.clone(),
                    ],
                    RetentionMode::Mature => vec![
                        localized_number(r.mature.passed as f64, 0),
                        localized_number(r.mature.failed as f64, 0),
                        r.mature.retention.clone(),
                    ],
                };
                for (i, cell) in cells.iter().enumerate() {
                    put(
                        buf,
                        x + title_width + i as u16 * col,
                        y,
                        &format!("{cell:>10}"),
                        right,
                        Style::default(),
                    );
                }
                y += 1;
            }
        }
        PanelKind::Hours => {
            if !p.hours.has_data {
                no_data(buf, y);
                return;
            }
            let values: Vec<f64> = p.hours.hours.iter().map(|h| h.total as f64).collect();
            let max = values.iter().cloned().fold(0.0, f64::max);
            let axis = axis_width(max);
            let bar = ((inner.width.saturating_sub(axis)) / 24).clamp(1, 4);
            let stacks = charts::single_bars(&values, palette::BAR);
            let columns = Columns {
                stacks,
                items: Vec::new(),
                bar_width: bar,
                first: 0.0,
                last: 24.0,
                max,
            };
            draw_bars(
                buf,
                Rect::new(x, y, inner.width, SMALL_CHART_ROWS),
                &columns,
                selected,
                false,
                false,
            );
            y += SMALL_CHART_ROWS;
            // Correct share as a one-row bar per hour (0–100 %).
            put(buf, x, y, "%", x + axis, dim());
            for (i, h) in p.hours.hours.iter().enumerate() {
                let filled = if h.total > 0 {
                    (h.ratio * 8.0).round() as usize
                } else {
                    0
                };
                for dx in 0..bar {
                    let cx = x + axis + i as u16 * bar + dx;
                    put(
                        buf,
                        cx,
                        y,
                        charts::LOWER[filled.min(8)],
                        right,
                        Style::default().fg(palette::MATURE),
                    );
                }
            }
            y += 1;
            for hour in [0u16, 6, 12, 18] {
                put(
                    buf,
                    x + axis + hour * bar,
                    y,
                    &hour.to_string(),
                    right,
                    dim(),
                );
            }
        }
        PanelKind::Buttons => {
            if !p.buttons.has_data {
                no_data(buf, y);
                return;
            }
            let mut stacks = Vec::new();
            for g in &p.buttons.groups {
                for (b, count) in g.counts.iter().enumerate() {
                    stacks.push(vec![(*count as f64, palette::BUTTONS[b])]);
                }
            }
            let max = StackedBars::largest(&stacks);
            let axis = axis_width(max);
            let available = inner.width.saturating_sub(axis);
            let bar = (available / 15).clamp(1, 4);
            let group_width = bar * 5;
            put(buf, x, y, &number(max), x + axis, dim());
            put(
                buf,
                x + axis - 2,
                y + SMALL_CHART_ROWS - 1,
                "0",
                x + axis,
                dim(),
            );
            for (gi, chunk) in stacks.chunks(4).enumerate() {
                let gx = x + axis + gi as u16 * group_width;
                let local = selected.and_then(|s| (s / 4 == gi).then_some(s % 4));
                StackedBars::new(chunk)
                    .max(max)
                    .bar_width(bar)
                    .selected(local)
                    .render(
                        Rect::new(
                            gx,
                            y,
                            (bar * 4).min(right.saturating_sub(gx)),
                            SMALL_CHART_ROWS,
                        ),
                        buf,
                    );
                put(
                    buf,
                    gx,
                    y + SMALL_CHART_ROWS,
                    &p.buttons.groups[gi].label,
                    right,
                    dim(),
                );
            }
            y += SMALL_CHART_ROWS + 1;
            for g in &p.buttons.groups {
                put(
                    buf,
                    x,
                    y,
                    &format!("{}: {}", g.label, g.correct_label),
                    right,
                    Style::default(),
                );
                y += 1;
            }
        }
        _ => {}
    }
}

fn count_colour(kind: CountKind) -> Color {
    match kind {
        CountKind::New => palette::NEW,
        CountKind::Learning => palette::LEARN,
        CountKind::Relearning => palette::RELEARN,
        CountKind::Young => palette::YOUNG,
        CountKind::Mature => palette::MATURE,
        CountKind::Suspended => palette::SUSPENDED,
        CountKind::Buried => palette::BURIED,
    }
}

fn draw_calendar(ui: &StatsUi, area: Rect, selected: Option<usize>, buf: &mut Buffer) {
    let c = &ui.panels.calendar;
    if c.days.is_empty() {
        put(
            buf,
            area.x,
            area.y,
            &ui.panels.labels.no_data,
            area.right(),
            dim(),
        );
        return;
    }
    let weeks = c.days.iter().map(|d| d.week).max().unwrap_or(0) as usize + 1;
    let label = 2;
    let available = area.width.saturating_sub(label);
    let cell_width = if available as usize >= weeks * 2 {
        2
    } else {
        1
    };
    let visible = (available / cell_width) as usize;
    let first_week = weeks.saturating_sub(visible);
    let mut grid = vec![vec![None; weeks - first_week]; 7];
    let mut cursor = None;
    let mut month_seen = [false; 12];
    for (i, day) in c.days.iter().enumerate() {
        if (day.week as usize) < first_week {
            continue;
        }
        let column = day.week as usize - first_week;
        grid[day.weekday as usize][column] = Some(day.intensity);
        if selected == Some(i) {
            cursor = Some((day.weekday as usize, column));
        }
        let month = day.date.month as usize - 1;
        if !month_seen[month] && day.date.day <= 7 {
            month_seen[month] = true;
            let mx = area.x + label + column as u16 * cell_width;
            put(buf, mx, area.y, MONTHS[month], area.right(), dim());
        }
    }
    for (row, text) in c.weekday_labels.iter().enumerate() {
        put(
            buf,
            area.x,
            area.y + 1 + row as u16,
            text,
            area.x + label,
            dim(),
        );
    }
    Heatmap {
        cells: &grid,
        cell_width,
        cursor,
    }
    .render(
        Rect::new(
            area.x + label,
            area.y + 1,
            available,
            7.min(area.height.saturating_sub(1)),
        ),
        buf,
    );
}

fn scope_line(ui: &StatsUi) -> Line<'static> {
    let l = &ui.panels.labels;
    let scope = match &ui.scope {
        StatsScope::Collection => l.collection.clone(),
        StatsScope::Search(text) => format!("{}: {text}", l.search),
        StatsScope::CurrentDeck | StatsScope::Deck(_) | StatsScope::DeckId(_) => {
            match &ui.deck_name {
                Some(name) => format!("{}: {name}", l.deck),
                None => match &ui.scope {
                    StatsScope::Deck(name) => format!("{}: {name}", l.deck),
                    _ => l.deck.clone(),
                },
            }
        }
    };
    let history = match ui.history {
        History::Year => l.one_year_history.clone(),
        History::All => l.all_history.clone(),
    };
    Line::from(vec![
        Span::styled(
            l.page_title.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled("s/: ", dim()),
        Span::raw(scope),
        Span::raw("   "),
        Span::styled("a: ", dim()),
        Span::raw(history),
    ])
}

pub(super) fn render(ui: &StatsUi, area: Rect, buf: &mut Buffer) {
    if area.height < 3 || area.width < 10 {
        return;
    }
    put_line(buf, area.x, area.y, &scope_line(ui), area.right());
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 2);
    let visible = visible_panels(&ui.panels);
    let heights: Vec<u16> = visible
        .iter()
        .map(|kind| panel_height(ui, *kind, area.width))
        .collect();
    let total: u16 = heights.iter().sum();
    let max_scroll = total.saturating_sub(body.height);
    let scroll = ui.scroll.get().min(max_scroll);
    ui.scroll.set(scroll);
    // Draw everything off screen, then copy the visible window.
    let mut canvas = Buffer::empty(Rect::new(0, 0, area.width, total.max(1)));
    let mut top = 0;
    for (i, (kind, height)) in visible.iter().zip(&heights).enumerate() {
        draw_panel(
            ui,
            *kind,
            i == ui.focus,
            Rect::new(0, top, area.width, *height),
            &mut canvas,
        );
        top += height;
    }
    for dy in 0..body.height {
        let source = scroll + dy;
        if source >= total {
            break;
        }
        for dx in 0..area.width {
            buf[(body.x + dx, body.y + dy)] = canvas[(dx, source)].clone();
        }
    }
    let status_y = area.bottom() - 1;
    if let Some(input) = &ui.search {
        let line = Line::from(vec![
            Span::styled(format!("{}: ", ui.panels.labels.search), dim()),
            Span::raw(input.text().to_owned()),
        ]);
        put_line(buf, area.x, status_y, &line, area.right());
        let cx = area.x + ui.panels.labels.search.width() as u16 + 2 + input.cursor_column() as u16;
        if cx < area.right() {
            buf[(cx, status_y)].set_style(Style::default().add_modifier(Modifier::REVERSED));
        }
    } else if let Some(message) = &ui.message {
        put(
            buf,
            area.x,
            status_y,
            message,
            area.right(),
            Style::default().fg(Color::Red),
        );
    } else if let Some(text) = ui.selection_text() {
        put(buf, area.x, status_y, &text, area.right(), Style::default());
    } else if total > body.height {
        let percent = if max_scroll == 0 {
            100
        } else {
            scroll as u32 * 100 / max_scroll as u32
        };
        put(
            buf,
            area.x,
            status_y,
            &format!("{percent}%"),
            area.right(),
            dim(),
        );
    }
}
