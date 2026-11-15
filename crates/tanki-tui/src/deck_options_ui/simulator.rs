//! The FSRS simulator panel: desktop's simulator inputs, one braille line
//! per simulation (ratatui `Chart`), and the minimum recommended retention.
//! The request comes from the current preset (`simulate_request`).
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Axis, Block, Chart, Clear, Dataset, GraphType, LegendPosition, Paragraph, Widget, Wrap,
    },
};
use tanki_core::deck_options::model::DeckOptionsState;
use tanki_core::deck_options::{SimulateRequest, Simulation};

/// What the panel wants run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    None,
    Close,
    Simulate,
    MinimumRetention,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Metric {
    Count,
    Time,
    Memorized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Input {
    Days,
    DeckSize,
    SuspendLeeches,
    Smooth,
}

const INPUTS: [Input; 4] = [
    Input::Days,
    Input::DeckSize,
    Input::SuspendLeeches,
    Input::Smooth,
];
const MAX_RUNS: usize = 6;
const COLORS: [Color; MAX_RUNS] = [
    Color::Cyan,
    Color::Yellow,
    Color::Magenta,
    Color::Green,
    Color::Red,
    Color::Blue,
];

pub(super) struct SimulatorView {
    cursor: usize,
    days: u32,
    deck_size: u32,
    suspend_leeches: bool,
    smooth: bool,
    pub(super) metric: Metric,
    runs: Vec<Simulation>,
    pub(super) retention: Option<String>,
}

impl SimulatorView {
    /// Desktop's defaults: 365 days, no extra cards, smoothing on, leeches
    /// suspended when the preset suspends them.
    pub(super) fn new(suspend_leeches: bool) -> Self {
        Self {
            cursor: 0,
            days: 365,
            deck_size: 0,
            suspend_leeches,
            smooth: true,
            metric: Metric::Count,
            runs: vec![],
            retention: None,
        }
    }

    pub(super) fn request(&self, state: &DeckOptionsState) -> SimulateRequest {
        state.simulate_request(self.days, self.deck_size, self.suspend_leeches)
    }

    pub(super) fn add_run(&mut self, run: Simulation) {
        if self.runs.len() == MAX_RUNS {
            self.runs.remove(0);
        }
        self.runs.push(run);
    }

    fn adjust(&mut self, delta: i64) {
        match INPUTS[self.cursor] {
            // Desktop's ranges: 1–3650 days, 0–100000 cards.
            Input::Days => {
                let step = if self.days >= 100 { 30 } else { 1 };
                self.days = (i64::from(self.days) + delta * step).clamp(1, 3650) as u32
            }
            Input::DeckSize => {
                let step = if self.deck_size >= 100 { 100 } else { 10 };
                self.deck_size = (i64::from(self.deck_size) + delta * step).clamp(0, 100_000) as u32
            }
            Input::SuspendLeeches => self.suspend_leeches = !self.suspend_leeches,
            Input::Smooth => self.smooth = !self.smooth,
        }
    }

    pub(super) fn on_key(&mut self, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Outcome::Close,
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor = (self.cursor + 1).min(INPUTS.len() - 1)
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('-') => self.adjust(-1),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('+') | KeyCode::Char(' ') => {
                self.adjust(1)
            }
            KeyCode::Tab => {
                self.metric = match self.metric {
                    Metric::Count => Metric::Time,
                    Metric::Time => Metric::Memorized,
                    Metric::Memorized => Metric::Count,
                }
            }
            KeyCode::Enter | KeyCode::Char('s') => return Outcome::Simulate,
            KeyCode::Char('m') => return Outcome::MinimumRetention,
            KeyCode::Char('c') => {
                self.runs.pop();
            }
            _ => {}
        }
        Outcome::None
    }

    pub(super) fn hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("Enter", "simulate"),
            ("m", "min. retention"),
            ("c", "clear last"),
            ("Tab", "chart"),
            ("←/→", "change"),
            ("Esc", "back"),
        ]
    }

    /// The plotted series of one run: (day, value).
    fn series(&self, run: &Simulation) -> Vec<(f64, f64)> {
        let values: Vec<f64> = match self.metric {
            Metric::Count => run
                .daily_review_count
                .iter()
                .zip(&run.daily_new_count)
                .map(|(r, n)| f64::from(r + n))
                .collect(),
            // Minutes per day.
            Metric::Time => run
                .daily_time_cost
                .iter()
                .map(|s| f64::from(*s) / 60.0)
                .collect(),
            Metric::Memorized => run
                .accumulated_knowledge_acquisition
                .iter()
                .map(|v| f64::from(*v))
                .collect(),
        };
        let values = if self.smooth && self.metric != Metric::Memorized {
            moving_average(&values, self.days.div_ceil(365).max(1) as usize)
        } else {
            values
        };
        values
            .into_iter()
            .enumerate()
            .map(|(day, v)| (day as f64, v))
            .collect()
    }

    pub(super) fn render(
        &self,
        state: &DeckOptionsState,
        progress: Option<String>,
        area: Rect,
        buf: &mut Buffer,
    ) {
        let s = &state.form().strings;
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        Clear.render(area, buf);
        let block = Block::bordered().title(format!(" {} ", s.simulator));
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height < 8 || inner.width < 30 {
            return;
        }
        let toggle = |on: bool| if on { "[x]" } else { "[ ]" }.to_owned();
        let rows = [
            (s.days_to_simulate.clone(), format!("‹ {} ›", self.days)),
            (
                s.additional_new_cards.clone(),
                format!("‹ {} ›", self.deck_size),
            ),
            (s.suspend_leeches.clone(), toggle(self.suspend_leeches)),
            (s.smooth_graph.clone(), toggle(self.smooth)),
        ];
        let label_width = rows
            .iter()
            .map(|(l, _)| l.chars().count())
            .max()
            .unwrap_or(0)
            + 2;
        let mut y = inner.y;
        for (i, (label, value)) in rows.iter().enumerate() {
            let selected = i == self.cursor;
            let style = if selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            let line = Line::from(vec![
                Span::raw(if selected { "› " } else { "  " }),
                Span::styled(format!("{label:label_width$}"), style),
                Span::styled(value.clone(), bold),
            ]);
            buf.set_line(inner.x, y, &line, inner.width);
            y += 1;
        }
        let c = &state.current().config;
        let retention = format!(
            "{} {:.0}%  ·  {} {}/{}  ·  {} {}",
            state
                .spec(tanki_core::deck_options::form::FieldId::DesiredRetention)
                .map_or("", |f| f.label.as_str()),
            c.desired_retention * 100.0,
            state.current().name,
            c.new_per_day,
            c.reviews_per_day,
            state
                .spec(tanki_core::deck_options::form::FieldId::MaximumReviewInterval)
                .map_or("", |f| f.label.as_str()),
            c.maximum_review_interval,
        );
        buf.set_stringn(inner.x + 2, y, retention, inner.width as usize - 2, dim);
        y += 1;
        let metrics = [
            (Metric::Count, &s.sim_reviews),
            (Metric::Time, &s.sim_time),
            (Metric::Memorized, &s.sim_memorized),
        ];
        let mut tabs = vec![Span::raw("  ")];
        for (metric, name) in metrics {
            if metric == self.metric {
                tabs.push(Span::styled(format!("[{name}]"), bold));
            } else {
                tabs.push(Span::styled(format!(" {name} "), dim));
            }
        }
        buf.set_line(inner.x, y, &Line::from(tabs), inner.width);
        y += 1;

        let footer = 2u16;
        let chart_height = (inner.y + inner.height).saturating_sub(y + footer);
        let chart_area = Rect::new(inner.x, y, inner.width, chart_height);
        if self.runs.is_empty() {
            Paragraph::new(format!("Enter: {}", s.simulate))
                .style(dim)
                .render(
                    Rect::new(
                        chart_area.x + 2,
                        chart_area.y + 1,
                        chart_area.width.saturating_sub(2),
                        1,
                    ),
                    buf,
                );
        } else if chart_height >= 4 {
            let series: Vec<Vec<(f64, f64)>> = self.runs.iter().map(|r| self.series(r)).collect();
            let max_x = series
                .iter()
                .map(|s| s.len())
                .max()
                .unwrap_or(1)
                .saturating_sub(1)
                .max(1) as f64;
            let max_y = series
                .iter()
                .flat_map(|s| s.iter().map(|p| p.1))
                .fold(0.0f64, f64::max)
                .max(1.0);
            let datasets = series
                .iter()
                .enumerate()
                .map(|(i, data)| {
                    Dataset::default()
                        .name(format!("#{}", i + 1))
                        .marker(Marker::Braille)
                        .graph_type(GraphType::Line)
                        .style(Style::default().fg(COLORS[i % COLORS.len()]))
                        .data(data)
                })
                .collect();
            let y_label = |v: f64| {
                if max_y < 10.0 {
                    format!("{v:.1}")
                } else {
                    format!("{v:.0}")
                }
            };
            let legend = (self.runs.len() > 1).then_some(LegendPosition::TopLeft);
            Chart::new(datasets)
                .legend_position(legend)
                .x_axis(Axis::default().bounds([0.0, max_x]).labels(vec![
                    "0".to_owned(),
                    format!("{:.0}", max_x / 2.0),
                    format!("{max_x:.0}d"),
                ]))
                .y_axis(Axis::default().bounds([0.0, max_y * 1.05]).labels(vec![
                    "0".to_owned(),
                    y_label(max_y / 2.0),
                    y_label(max_y),
                ]))
                .render(chart_area, buf);
        }
        let status = progress
            .or_else(|| self.retention.clone())
            .unwrap_or_default();
        Paragraph::new(status)
            .style(bold)
            .wrap(Wrap { trim: true })
            .render(
                Rect::new(
                    inner.x + 1,
                    inner.y + inner.height - footer,
                    inner.width - 1,
                    footer,
                ),
                buf,
            );
    }
}

/// Desktop's smoothing (`SimulatorModal.svelte`).
fn moving_average(values: &[f64], window: usize) -> Vec<f64> {
    (0..values.len())
        .map(|i| {
            let start = (i + 1).saturating_sub(window);
            let slice = &values[start..=i];
            slice.iter().sum::<f64>() / slice.len() as f64
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_average_matches_desktop() {
        assert_eq!(moving_average(&[1.0, 3.0, 5.0], 1), vec![1.0, 3.0, 5.0]);
        assert_eq!(moving_average(&[1.0, 3.0, 5.0], 2), vec![1.0, 2.0, 4.0]);
    }
}
