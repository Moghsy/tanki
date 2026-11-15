//! Card info (`ts/routes/card-info/`): the stats rows, the review log table
//! and, for cards with an FSRS memory state, the forgetting curve.
//!
//! The app builds it with [`CardInfoUi::open`] for a card id (reviewer:
//! current or previous card; browser: the current row), draws it with
//! [`CardInfoUi::render`] and sends keys to [`CardInfoUi::on_key`].
//! Opening it runs Anki's `card_stats`, which may write the card's
//! `last_review_time` (no undo entry, nothing to sync).
use super::stats;
use crate::backend::Backend;
use crate::charts::LineChart;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Widget},
};
use std::cell::Cell;
use tanki_core::stats::{
    curve_ranges, date_string, default_curve_range, forgetting_curve, CardInfo, CurveRange,
    ReviewKind, StatsCommand, StatsResponse,
};
use tanki_core::CoreError;
use unicode_width::UnicodeWidthStr;

/// What only the app can do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardInfoIntent {
    None,
    Close,
}

const CURVE_ROWS: u16 = 12;

pub struct CardInfoUi {
    info: CardInfo,
    title: String,
    curve_range: CurveRange,
    ranges: Vec<CurveRange>,
    scroll: Cell<u16>,
    height: Cell<u16>,
}

impl CardInfoUi {
    pub fn open(backend: &dyn Backend, card_id: i64) -> Result<Self, CoreError> {
        match stats(backend, StatsCommand::CardInfo { card_id })? {
            StatsResponse::CardInfo(info) => Ok(Self::new(*info)),
            _ => Err(CoreError::Other("Unexpected card info response".into())),
        }
    }

    pub fn new(info: CardInfo) -> Self {
        let ranges = curve_ranges(&info.revlog, info.now);
        let curve_range = default_curve_range(&info.revlog, info.now);
        Self {
            title: "Card Info".into(),
            info,
            curve_range,
            ranges,
            scroll: Cell::new(0),
            height: Cell::new(24),
        }
    }

    /// A heading other than "Card Info" (e.g. for the previous card).
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    pub fn info(&self) -> &CardInfo {
        &self.info
    }
    pub fn curve_range(&self) -> CurveRange {
        self.curve_range
    }

    /// Reload after the card changed (answered, rescheduled).
    pub fn refresh(&mut self, backend: &dyn Backend) -> Result<(), CoreError> {
        let fresh = Self::open(backend, self.info.card_id)?;
        self.info = fresh.info;
        self.ranges = fresh.ranges;
        if !self.ranges.contains(&self.curve_range) {
            self.curve_range = fresh.curve_range;
        }
        Ok(())
    }

    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        let mut hints = vec![("j/k", "scroll")];
        if self.info.fsrs && !self.ranges.is_empty() {
            hints.push(("r", "curve range"));
        }
        hints.push(("Esc", "back"));
        hints
    }

    pub fn on_key(&mut self, key: KeyEvent) -> CardInfoIntent {
        let page = self.height.get().saturating_sub(2).max(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return CardInfoIntent::Close,
            KeyCode::Down | KeyCode::Char('j') => self.scroll_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_by(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll_by(page as i32),
            KeyCode::PageUp => self.scroll_by(-(page as i32)),
            KeyCode::Home | KeyCode::Char('g') => self.scroll.set(0),
            KeyCode::Char('r') if !self.ranges.is_empty() => {
                let at = self
                    .ranges
                    .iter()
                    .position(|r| *r == self.curve_range)
                    .map_or(0, |i| (i + 1) % self.ranges.len());
                self.curve_range = self.ranges[at];
            }
            _ => {}
        }
        CardInfoIntent::None
    }

    fn scroll_by(&self, delta: i32) {
        let max = self.content_height().saturating_sub(self.height.get());
        let next = (self.scroll.get() as i32 + delta).clamp(0, max as i32);
        self.scroll.set(next as u16);
    }

    fn content_height(&self) -> u16 {
        let rows = self.info.rows.len() as u16;
        let revlog = if self.info.revlog.is_empty() {
            0
        } else {
            1 + 1 + self.info.revlog.len() as u16
        };
        let curve = if self.info.fsrs {
            1 + 2 + CURVE_ROWS
        } else {
            0
        };
        rows + revlog + curve
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(Span::styled(
                format!(" {} ", self.title),
                Style::default().add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.is_empty() {
            return;
        }
        self.height.set(inner.height);
        let total = self.content_height().max(1);
        let max = total.saturating_sub(inner.height);
        self.scroll.set(self.scroll.get().min(max));
        let mut canvas = Buffer::empty(Rect::new(0, 0, inner.width, total));
        self.draw(&mut canvas, inner.width);
        for dy in 0..inner.height {
            let source = self.scroll.get() + dy;
            if source >= total {
                break;
            }
            for dx in 0..inner.width {
                buf[(inner.x + dx, inner.y + dy)] = canvas[(dx, source)].clone();
            }
        }
    }

    fn draw(&self, buf: &mut Buffer, width: u16) {
        let dim = Style::default().fg(Color::DarkGray);
        let info = &self.info;
        let label_width = info.rows.iter().map(|r| r.label.width()).max().unwrap_or(0) as u16 + 2;
        let mut y = 0;
        for row in &info.rows {
            buf.set_stringn(0, y, &row.label, width as usize, dim);
            if label_width < width {
                buf.set_stringn(
                    label_width,
                    y,
                    &row.value,
                    (width - label_width) as usize,
                    Style::default(),
                );
            }
            y += 1;
        }
        if !info.revlog.is_empty() {
            y += 1;
            // Desktop's columns: date (with time), type, rating, interval,
            // ease, time taken.
            let h = &info.revlog_headings;
            let headings = [
                h[0].clone(),
                h[1].clone(),
                h[2].clone(),
                h[3].clone(),
                h[4].clone(),
                h[5].clone(),
            ];
            let cells: Vec<[String; 6]> = info
                .revlog_rows
                .iter()
                .map(|r| {
                    [
                        format!("{} @ {}", r.date, r.time),
                        r.kind_label.clone(),
                        r.rating.to_string(),
                        r.interval.clone(),
                        r.ease.clone(),
                        r.taken.clone(),
                    ]
                })
                .collect();
            let mut widths = headings.clone().map(|h| h.width());
            for row in &cells {
                for (w, cell) in widths.iter_mut().zip(row) {
                    *w = (*w).max(cell.width());
                }
            }
            let mut x = 0u16;
            let mut columns = Vec::new();
            for w in widths {
                columns.push(x);
                x = x.saturating_add(w as u16 + 2);
            }
            let write = |buf: &mut Buffer, column: usize, y: u16, text: &str, style: Style| {
                let x = columns[column];
                if x < width {
                    buf.set_stringn(x, y, text, (width - x) as usize, style);
                }
            };
            for (i, heading) in headings.iter().enumerate() {
                write(buf, i, y, heading, dim.add_modifier(Modifier::BOLD));
            }
            y += 1;
            for (row, entry) in cells.iter().zip(&info.revlog_rows) {
                let kind_style = match entry.kind {
                    ReviewKind::Learning => Style::default().fg(Color::Rgb(0xfd, 0x8d, 0x3c)),
                    ReviewKind::Review => Style::default().fg(Color::Rgb(0x31, 0xa3, 0x54)),
                    ReviewKind::Relearning => Style::default().fg(Color::Rgb(0xfb, 0x6a, 0x4a)),
                    _ => Style::default(),
                };
                for (i, cell) in row.iter().enumerate() {
                    let style = match i {
                        1 => kind_style,
                        2 if entry.rating == 1 => Style::default().fg(Color::Red),
                        _ => Style::default(),
                    };
                    write(buf, i, y, cell, style);
                }
                y += 1;
            }
        }
        if info.fsrs {
            y += 1;
            buf.set_stringn(
                0,
                y,
                &info.curve_title,
                width as usize,
                Style::default().add_modifier(Modifier::BOLD),
            );
            y += 1;
            let mut spans = vec![Span::styled("r: ", dim)];
            for (i, range) in self.ranges.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled(" · ", dim));
                }
                let label = info.curve_range_labels[*range as usize].clone();
                spans.push(if *range == self.curve_range {
                    Span::styled(label, Style::default().add_modifier(Modifier::REVERSED))
                } else {
                    Span::raw(label)
                });
            }
            buf.set_line(0, y, &Line::from(spans), width);
            y += 1;
            self.draw_curve(buf, Rect::new(0, y, width, CURVE_ROWS));
        }
    }

    fn draw_curve(&self, buf: &mut Buffer, area: Rect) {
        let info = &self.info;
        let points = forgetting_curve(&info.revlog, self.curve_range, info.decay, info.now);
        let (Some(first), Some(last)) = (points.first(), points.last()) else {
            return;
        };
        let now = info.now as f64;
        let past: Vec<(f64, f64)> = points
            .iter()
            .filter(|p| p.time <= now)
            .map(|p| (p.days_since_first_learn, p.retrievability))
            .collect();
        let future: Vec<(f64, f64)> = points
            .iter()
            .filter(|p| p.time >= now)
            .map(|p| (p.days_since_first_learn, p.retrievability))
            .collect();
        let lowest = points
            .iter()
            .map(|p| p.retrievability)
            .fold(100.0, f64::min);
        let y_min = (100.0 - 1.2 * (100.0 - lowest)).max(0.0);
        let x_max = last
            .days_since_first_learn
            .max(first.days_since_first_learn + 1e-9);
        let desired = info.desired_retention as f64 * 100.0;
        let target = [(0.0, desired), (x_max, desired)];
        let mut series = vec![
            (past.as_slice(), Color::Rgb(0x46, 0x82, 0xb4)),
            (future.as_slice(), Color::DarkGray),
        ];
        if desired > y_min {
            series.push((&target[..], Color::Rgb(0xff, 0x63, 0x47)));
        }
        LineChart {
            series,
            x_bounds: [0.0, x_max],
            y_bounds: [y_min, 100.0],
            x_labels: vec![
                date_string(first.time as i64),
                date_string(last.time as i64),
            ],
            y_labels: vec![format!("{y_min:.0}%"), "100%".into()],
        }
        .render(area, buf);
    }
}
