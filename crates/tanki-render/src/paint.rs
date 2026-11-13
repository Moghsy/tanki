//! Painting laid-out lines into a ratatui buffer.

use crate::{RenderedCard, Rgb, Style};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

fn color(c: Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

/// Converts a span style to a ratatui style. Unset colours stay unset so the
/// terminal's (or the caller's) colours show through.
pub fn to_ratatui(s: &Style) -> ratatui::style::Style {
    let mut out = ratatui::style::Style::default();
    if let Some(fg) = s.fg {
        out = out.fg(color(fg));
    }
    if let Some(bg) = s.bg {
        out = out.bg(color(bg));
    }
    let mut m = Modifier::empty();
    if s.bold {
        m |= Modifier::BOLD;
    }
    if s.italic {
        m |= Modifier::ITALIC;
    }
    if s.underline {
        m |= Modifier::UNDERLINED;
    }
    if s.strikethrough {
        m |= Modifier::CROSSED_OUT;
    }
    if s.dim {
        m |= Modifier::DIM;
    }
    out.add_modifier(m)
}

pub fn paint(card: &RenderedCard, scroll: usize, area: Rect, buf: &mut Buffer) {
    let area = area.intersection(buf.area);
    if area.is_empty() {
        return;
    }
    // Blank the area but keep any background the caller painted.
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(" ");
                cell.modifier = Modifier::empty();
            }
        }
    }
    for (i, line) in card.lines.iter().skip(scroll).take(area.height as usize).enumerate() {
        let y = area.y + i as u16;
        let mut x = area.x;
        for span in &line.spans {
            if x >= area.right() {
                break;
            }
            let max = (area.right() - x) as usize;
            let (nx, _) = buf.set_stringn(x, y, &span.text, max, to_ratatui(&span.style));
            x = nx;
        }
    }
}
