//! Chart primitives beyond ratatui's `Chart`/`BarChart`/`Sparkline`: stacked
//! bars, pie-as-bar, calendar heatmap (PLAN 8.8). Usable by any screen once
//! it lands.
//!
//! Owned by the Statistics stream (docs/implementation/phase-5-interfaces.md).
//!
//! All widgets are pure: they draw what they are given into a `Buffer`.
//! Bars resolve to eighths of a cell: a cell where two segments meet shows
//! the lower part as a block glyph in the lower colour over the upper
//! colour as background, so five-colour stacks stay exact to an eighth.
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{Axis, Chart, Dataset, GraphType, Widget},
};

/// Lower eighth blocks, index = eighths filled (0 = empty).
pub const LOWER: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
/// Left eighth blocks, index = eighths filled (0 = empty).
pub const LEFT: [&str; 9] = [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉", "█"];

/// Desktop's graph colours (d3 schemes used by `ts/routes/graphs`).
pub mod palette {
    use ratatui::style::Color;
    /// `schemeBlues[5][2]`: new cards.
    pub const NEW: Color = Color::Rgb(0x6b, 0xae, 0xd6);
    /// `schemeOranges[5][2]`: learning.
    pub const LEARN: Color = Color::Rgb(0xfd, 0x8d, 0x3c);
    /// `schemeReds[5][2]`: relearning.
    pub const RELEARN: Color = Color::Rgb(0xfb, 0x6a, 0x4a);
    /// `schemeGreens[5][2]`: young.
    pub const YOUNG: Color = Color::Rgb(0x74, 0xc4, 0x76);
    /// `schemeGreens[5][3]`: mature.
    pub const MATURE: Color = Color::Rgb(0x31, 0xa3, 0x54);
    /// `schemePurples[5][2]`: filtered.
    pub const FILTERED: Color = Color::Rgb(0x9e, 0x9a, 0xc8);
    /// Card counts: suspended.
    pub const SUSPENDED: Color = Color::Rgb(0xff, 0xdc, 0x41);
    /// Card counts: buried.
    pub const BURIED: Color = Color::Rgb(0x80, 0x80, 0x80);
    /// Single-series bars.
    pub const BAR: Color = Color::Rgb(0x42, 0x92, 0xc6);
    /// Calendar: days without reviews.
    pub const EMPTY: Color = Color::Rgb(0x44, 0x44, 0x44);
    /// Calendar intensity steps (`schemeBlues[5][1..]`), light to dark.
    pub const HEAT: [Color; 4] = [
        Color::Rgb(0xbd, 0xd7, 0xe7),
        Color::Rgb(0x6b, 0xae, 0xd6),
        Color::Rgb(0x31, 0x82, 0xbd),
        Color::Rgb(0x08, 0x51, 0x9c),
    ];
    /// Answer buttons 1–4 (`interpolateRdYlGn` at 1, 2, 3, 4).
    pub const BUTTONS: [Color; 4] = [
        Color::Rgb(0xd7, 0x30, 0x27),
        Color::Rgb(0xfe, 0xe0, 0x8b),
        Color::Rgb(0xa6, 0xd9, 0x6a),
        Color::Rgb(0x1a, 0x98, 0x50),
    ];
}

/// One bar: segments from the bottom (or left) up, `(value, colour)`.
pub type Stack = Vec<(f64, Color)>;

/// Where segment boundaries fall, in eighths, for a stack scaled so that
/// `max` fills `eighths`. A non-zero stack shows at least one eighth.
fn boundaries(stack: &[(f64, Color)], max: f64, eighths: u32) -> Vec<(u32, Color)> {
    let mut out = Vec::new();
    if max <= 0.0 {
        return out;
    }
    let mut running = 0.0;
    let mut previous = 0;
    for (value, colour) in stack {
        if *value <= 0.0 {
            continue;
        }
        running += value;
        let mut end = ((running / max) * eighths as f64)
            .round()
            .min(eighths as f64) as u32;
        if end <= previous && out.is_empty() {
            end = 1.min(eighths);
        }
        if end > previous {
            out.push((end, *colour));
            previous = end;
        }
    }
    out
}

/// The glyph index and colours of the cell spanning `[start, start + 8)`
/// eighths: `(filled eighths, colour of the filled part, colour after)`.
fn cell(bounds: &[(u32, Color)], start: u32) -> Option<(usize, Color, Option<Color>)> {
    let index = bounds.iter().position(|(end, _)| *end > start)?;
    let (end, colour) = bounds[index];
    if end >= start + 8 {
        return Some((8, colour, None));
    }
    let next = bounds.get(index + 1).map(|(_, c)| *c);
    Some(((end - start) as usize, colour, next))
}

/// Vertical bars with stacked, coloured segments.
#[derive(Debug, Clone)]
pub struct StackedBars<'a> {
    pub bars: &'a [Stack],
    /// The value at the top of the area; `None`: the largest stack.
    pub max: Option<f64>,
    pub bar_width: u16,
    pub gap: u16,
    /// A selected bar, drawn with a marker row at the bottom.
    pub selected: Option<usize>,
}

impl<'a> StackedBars<'a> {
    pub fn new(bars: &'a [Stack]) -> Self {
        Self {
            bars,
            max: None,
            bar_width: 1,
            gap: 0,
            selected: None,
        }
    }
    pub fn max(mut self, max: f64) -> Self {
        self.max = Some(max);
        self
    }
    pub fn bar_width(mut self, width: u16) -> Self {
        self.bar_width = width.max(1);
        self
    }
    pub fn gap(mut self, gap: u16) -> Self {
        self.gap = gap;
        self
    }
    pub fn selected(mut self, selected: Option<usize>) -> Self {
        self.selected = selected;
        self
    }
    /// The largest stack total.
    pub fn largest(bars: &[Stack]) -> f64 {
        bars.iter()
            .map(|s| s.iter().map(|(v, _)| v.max(0.0)).sum::<f64>())
            .fold(0.0, f64::max)
    }
}

impl Widget for StackedBars<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let max = self.max.unwrap_or_else(|| Self::largest(self.bars));
        let eighths = area.height as u32 * 8;
        let step = self.bar_width + self.gap;
        for (i, stack) in self.bars.iter().enumerate() {
            let x0 = area.x + i as u16 * step;
            if x0 >= area.right() {
                break;
            }
            let bounds = boundaries(stack, max, eighths);
            for row in 0..area.height {
                let y = area.bottom() - 1 - row;
                let Some((filled, colour, next)) = cell(&bounds, row as u32 * 8) else {
                    break;
                };
                let mut style = Style::default().fg(colour);
                if let Some(next) = next {
                    style = style.bg(next);
                }
                for dx in 0..self.bar_width {
                    let x = x0 + dx;
                    if x >= area.right() {
                        break;
                    }
                    buf[(x, y)].set_symbol(LOWER[filled]).set_style(style);
                }
            }
            if self.selected == Some(i) {
                for dx in 0..self.bar_width {
                    let x = x0 + dx;
                    if x < area.right() {
                        let cell = &mut buf[(x, area.bottom() - 1)];
                        let style = cell.style().add_modifier(Modifier::REVERSED);
                        if cell.symbol() == " " {
                            cell.set_symbol("▔");
                        }
                        cell.set_style(style);
                    }
                }
            }
        }
    }
}

/// Plain vertical bars of one colour each (`StackedBars` with one segment).
pub fn single_bars(values: &[f64], colour: Color) -> Vec<Stack> {
    values.iter().map(|v| vec![(*v, colour)]).collect()
}

/// A pie as one horizontal 100 % stacked bar (PLAN 8.8).
#[derive(Debug, Clone)]
pub struct PieBar<'a> {
    pub segments: &'a [(f64, Color)],
}

impl Widget for PieBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let total: f64 = self.segments.iter().map(|(v, _)| v.max(0.0)).sum();
        let bounds = boundaries(self.segments, total, area.width as u32 * 8);
        for column in 0..area.width {
            let Some((filled, colour, next)) = cell(&bounds, column as u32 * 8) else {
                break;
            };
            let mut style = Style::default().fg(colour);
            if let Some(next) = next {
                style = style.bg(next);
            }
            for y in area.top()..area.bottom() {
                buf[(area.x + column, y)]
                    .set_symbol(LEFT[filled])
                    .set_style(style);
            }
        }
    }
}

/// The intensity step (0 = no reviews, 1–4) of a calendar cell.
pub fn heat_level(intensity: f64) -> usize {
    if intensity <= 0.0 {
        0
    } else {
        ((intensity * 4.0).ceil() as usize).clamp(1, 4)
    }
}

/// A calendar heatmap: `cells[row][column]`, `None` = no cell, `Some(x)`
/// an intensity in 0–1 (0 = a day without reviews).
#[derive(Debug, Clone)]
pub struct Heatmap<'a> {
    pub cells: &'a [Vec<Option<f64>>],
    /// Columns per cell (2 leaves a gap between weeks).
    pub cell_width: u16,
    /// `(row, column)` of a highlighted cell.
    pub cursor: Option<(usize, usize)>,
}

impl Widget for Heatmap<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let width = self.cell_width.max(1);
        for (r, row) in self.cells.iter().enumerate() {
            let y = area.y + r as u16;
            if y >= area.bottom() {
                break;
            }
            for (c, value) in row.iter().enumerate() {
                let x = area.x + c as u16 * width;
                if x >= area.right() {
                    break;
                }
                let Some(value) = value else { continue };
                let colour = match heat_level(*value) {
                    0 => palette::EMPTY,
                    level => palette::HEAT[level - 1],
                };
                let mut style = Style::default().fg(colour);
                let mut symbol = "■";
                if self.cursor == Some((r, c)) {
                    style = style.add_modifier(Modifier::REVERSED);
                    symbol = "▣";
                }
                buf[(x, y)].set_symbol(symbol).set_style(style);
            }
        }
    }
}

/// A braille line chart (ratatui's `Chart`) with labelled axes.
#[derive(Debug, Clone)]
pub struct LineChart<'a> {
    /// Each series: points and colour; drawn in order.
    pub series: Vec<(&'a [(f64, f64)], Color)>,
    pub x_bounds: [f64; 2],
    pub y_bounds: [f64; 2],
    pub x_labels: Vec<String>,
    pub y_labels: Vec<String>,
}

impl Widget for LineChart<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let datasets = self
            .series
            .iter()
            .map(|(points, colour)| {
                Dataset::default()
                    .marker(Marker::Braille)
                    .graph_type(GraphType::Line)
                    .style(Style::default().fg(*colour))
                    .data(points)
            })
            .collect::<Vec<_>>();
        let labels = |labels: Vec<String>| {
            labels
                .into_iter()
                .map(|l| Line::from(Span::raw(l)))
                .collect::<Vec<_>>()
        };
        Chart::new(datasets)
            .x_axis(
                Axis::default()
                    .bounds(self.x_bounds)
                    .labels(labels(self.x_labels)),
            )
            .y_axis(
                Axis::default()
                    .bounds(self.y_bounds)
                    .labels(labels(self.y_labels)),
            )
            .render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::buffer_text;

    fn draw(width: u16, height: u16, widget: impl Widget) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);
        buf
    }

    #[test]
    fn single_bars_use_eighth_blocks() {
        let bars = single_bars(&[8.0, 4.0, 1.0, 0.0, 16.0, 0.1], Color::Blue);
        let buf = draw(6, 2, StackedBars::new(&bars));
        // Max 16 over two rows (16 eighths): 1 value = 1 eighth; a tiny
        // non-zero bar still shows one eighth.
        assert_eq!(buffer_text(&buf), vec!["    █", "█▄▁ █▁"]);
        assert_eq!(buf[(0, 1)].fg, Color::Blue);
    }

    #[test]
    fn stacked_bars_split_cells_between_colours() {
        let red = Color::Red;
        let green = Color::Green;
        // 3 + 6 of 16 eighths: row 0 holds 3 red under 5 green, row 1 one
        // green eighth.
        let bars = vec![vec![(3.0, red), (6.0, green)], vec![(16.0, green)]];
        let buf = draw(4, 2, StackedBars::new(&bars).max(16.0).bar_width(1).gap(1));
        assert_eq!(buffer_text(&buf), vec!["▁ █", "▃ █"]);
        assert_eq!((buf[(0, 1)].fg, buf[(0, 1)].bg), (red, green));
        assert_eq!((buf[(0, 0)].fg, buf[(0, 0)].bg), (green, Color::Reset));
        assert_eq!(buf[(2, 1)].fg, green);
    }

    #[test]
    fn selected_bar_is_marked() {
        let bars = single_bars(&[0.0, 2.0], Color::Blue);
        let buf = draw(4, 1, StackedBars::new(&bars).bar_width(2).selected(Some(0)));
        assert_eq!(buffer_text(&buf), vec!["▔▔██"]);
        assert!(buf[(0, 0)].modifier.contains(Modifier::REVERSED));
        assert!(!buf[(2, 0)].modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn pie_bar_is_one_proportional_strip() {
        let segments = [(1.0, Color::Blue), (1.0, Color::Red), (2.0, Color::Green)];
        // 4 cells = 32 eighths: blue 8, red 8, green 16.
        let buf = draw(
            4,
            1,
            PieBar {
                segments: &segments,
            },
        );
        assert_eq!(buffer_text(&buf), vec!["████"]);
        let colours: Vec<_> = (0..4).map(|x| buf[(x, 0)].fg).collect();
        assert_eq!(
            colours,
            vec![Color::Blue, Color::Red, Color::Green, Color::Green]
        );
        // Uneven split: 1 of 3 over 2 cells = 5.33 → 5 eighths blue.
        let segments = [(1.0, Color::Blue), (2.0, Color::Red)];
        let buf = draw(
            2,
            1,
            PieBar {
                segments: &segments,
            },
        );
        assert_eq!(buffer_text(&buf), vec!["▋█"]);
        assert_eq!((buf[(0, 0)].fg, buf[(0, 0)].bg), (Color::Blue, Color::Red));
    }

    #[test]
    fn heatmap_levels_and_cursor() {
        assert_eq!(
            [0.0, 0.1, 0.25, 0.26, 0.6, 1.0].map(heat_level),
            [0, 1, 1, 2, 3, 4]
        );
        let cells = vec![
            vec![None, Some(0.0), Some(1.0)],
            vec![Some(0.3), None, Some(0.6)],
        ];
        let buf = draw(
            6,
            2,
            Heatmap {
                cells: &cells,
                cell_width: 2,
                cursor: Some((1, 2)),
            },
        );
        assert_eq!(buffer_text(&buf), vec!["  ■ ■", "■   ▣"]);
        assert_eq!(buf[(2, 0)].fg, palette::EMPTY);
        assert_eq!(buf[(4, 0)].fg, palette::HEAT[3]);
        assert_eq!(buf[(0, 1)].fg, palette::HEAT[1]);
        assert!(buf[(4, 1)].modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn line_chart_draws_braille() {
        let points: Vec<(f64, f64)> = (0..=20)
            .map(|i| (i as f64, 100.0 - i as f64 * 2.0))
            .collect();
        let buf = draw(
            24,
            6,
            LineChart {
                series: vec![(&points, Color::Cyan)],
                x_bounds: [0.0, 20.0],
                y_bounds: [50.0, 100.0],
                x_labels: vec!["0".into(), "20d".into()],
                y_labels: vec!["50%".into(), "100%".into()],
            },
        );
        let text = buffer_text(&buf).join("\n");
        assert!(text.contains("100%") && text.contains("20d"), "{text}");
        assert!(
            text.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)),
            "{text}"
        );
    }
}
