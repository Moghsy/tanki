//! OSC 8 hyperlinks travel beside the cell buffer. Escape sequences never
//! enter width-counted text, and a target change redraws even unchanged text.
use crate::services::CardRenderer;
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::{Buffer, Cell, CellDiffOption, CellWidth};
use ratatui::layout::{Position, Rect, Size};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::rc::Rc;
use tanki_render::{CardSide, RenderOptions, RenderedCard};

const CLOSE: &[u8] = b"\x1b]8;;\x1b\\";
// Row first, for deterministic terminal drawing order.
type Point = (u16, u16);

/// Only web/mail links become terminal actions. In particular, OSC terminators,
/// C0/C1 controls, whitespace, javascript:, data: and command: cannot pass.
pub fn safe_uri(uri: &str) -> bool {
    if uri.len() > 8192 || uri.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let Some((scheme, rest)) = uri.split_once(':') else {
        return false;
    };
    if scheme.eq_ignore_ascii_case("mailto") {
        return !rest.is_empty() && !rest.starts_with('/');
    }
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    rest.strip_prefix("//").is_some_and(|rest| {
        !rest
            .split(['/', '?', '#'])
            .next()
            .unwrap_or_default()
            .is_empty()
    })
}

#[derive(Default)]
struct FrameLinks {
    candidates: BTreeMap<Point, (String, Cell)>,
    size_candidates: BTreeMap<Point, (u8, Cell)>,
    targets: BTreeMap<Point, String>,
    sizes: BTreeMap<Point, u8>,
    cells: BTreeMap<Point, Cell>,
}

/// Shared by the renderer decorator and terminal backend on the UI thread.
#[derive(Clone, Default)]
pub struct Links(Rc<RefCell<FrameLinks>>, bool);

impl Links {
    pub fn with_text_sizing(supported: bool) -> Self {
        Self(Rc::default(), supported)
    }
}

/// Retains the actual leading cells painted by a renderer. A one-row scratch
/// buffer uses Ratatui's own clipping and grapheme rules, including CJK/emoji.
pub struct LinkedRenderer<R> {
    inner: R,
    links: Links,
}

impl<R> LinkedRenderer<R> {
    pub fn new(inner: R, links: Links) -> Self {
        Self { inner, links }
    }
}

impl<R: CardRenderer> CardRenderer for LinkedRenderer<R> {
    fn begin_frame(&self) {
        *self.links.0.borrow_mut() = FrameLinks::default();
        self.inner.begin_frame();
    }
    fn finish_frame(&self, buffer: &Buffer, links_visible: bool) {
        self.inner.finish_frame(buffer, links_visible);
        let mut frame = self.links.0.borrow_mut();
        if !links_visible {
            frame.candidates.clear();
            frame.size_candidates.clear();
        }
        // This is also the source of truth for href-only redraws. Do not replay
        // trailing cells of wide graphemes or cells owned by graphics protocols.
        for y in buffer.area.top()..buffer.area.bottom() {
            let mut x = buffer.area.left();
            while x < buffer.area.right() {
                let cell = &buffer[(x, y)];
                if cell.diff_option == CellDiffOption::None
                    && !cell.symbol().chars().any(char::is_control)
                {
                    frame.cells.insert((y, x), cell.clone());
                }
                x = x.saturating_add(cell.cell_width().max(1));
            }
        }
        frame.targets = frame
            .candidates
            .iter()
            .filter(|(point, (_, expected))| frame.cells.get(point) == Some(expected))
            .map(|(point, (target, _))| (*point, target.clone()))
            .collect();
        frame.sizes = frame
            .size_candidates
            .iter()
            .filter(|(point, (_, expected))| frame.cells.get(point) == Some(expected))
            .map(|(point, (scale, _))| (*point, *scale))
            .collect();
    }
    fn obscure_links(&self, area: Rect) {
        self.links
            .0
            .borrow_mut()
            .candidates
            .retain(|&(y, x), _| !area.contains((x, y).into()));
        self.links
            .0
            .borrow_mut()
            .size_candidates
            .retain(|&(y, x), _| !area.contains((x, y).into()));
        self.inner.obscure_links(area);
    }
    fn render(&self, side: &CardSide, options: &RenderOptions) -> RenderedCard {
        self.inner.render(side, options)
    }
    fn paint(&self, card: &RenderedCard, scroll: usize, area: Rect, buffer: &mut Buffer) {
        self.inner.paint(card, scroll, area, buffer);
        let area = area.intersection(buffer.area);
        if area.is_empty() {
            return;
        }
        let mut frame = self.links.0.borrow_mut();
        // A later painting of the same area replaces all earlier link metadata.
        frame
            .candidates
            .retain(|&(y, x), _| !area.contains((x, y).into()));
        frame
            .size_candidates
            .retain(|&(y, x), _| !area.contains((x, y).into()));
        let mut scratch = Buffer::empty(Rect::new(area.x, 0, area.width, 1));
        for (i, line) in card
            .lines
            .iter()
            .skip(scroll)
            .take(area.height as usize)
            .enumerate()
        {
            let y = area.y + i as u16;
            let mut x = area.x;
            for span in &line.spans {
                if x >= area.right() {
                    break;
                }
                let (next, _) = scratch.set_stringn(
                    x,
                    0,
                    &span.text,
                    (area.right() - x) as usize,
                    crate::services::to_ratatui_style(&span.style),
                );
                let target = span.link.as_deref().filter(|target| safe_uri(target));
                while x < next {
                    if let Some(target) = target {
                        frame
                            .candidates
                            .insert((y, x), (target.to_owned(), buffer[(x, y)].clone()));
                    }
                    if self.links.1 && (1..100).contains(&span.style.text_scale) {
                        frame
                            .size_candidates
                            .insert((y, x), (span.style.text_scale, buffer[(x, y)].clone()));
                    }
                    x = x.saturating_add(scratch[(x, 0)].cell_width().max(1));
                }
                x = next;
            }
        }
    }
}

/// Decorates Crossterm's drawing rather than its buffer symbols. Each group
/// starts with an explicit cursor/style setup supplied by CrosstermBackend.
pub struct HyperlinkBackend<W: Write> {
    inner: CrosstermBackend<W>,
    links: Links,
    previous: BTreeMap<Point, String>,
    previous_sizes: BTreeMap<Point, u8>,
}

impl<W: Write> HyperlinkBackend<W> {
    pub fn new(writer: W, links: Links) -> Self {
        Self {
            inner: CrosstermBackend::new(writer),
            links,
            previous: BTreeMap::new(),
            previous_sizes: BTreeMap::new(),
        }
    }
}

impl<W: Write> Write for HyperlinkBackend<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.inner.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        Write::flush(&mut self.inner)
    }
}

impl<W: Write> Backend for HyperlinkBackend<W> {
    type Error = io::Error;
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let frame = self.links.0.borrow();
        let mut dirty: BTreeMap<Point, Cell> =
            content.map(|(x, y, cell)| ((y, x), cell.clone())).collect();
        let changed: BTreeSet<Point> = self
            .previous
            .keys()
            .chain(frame.targets.keys())
            .copied()
            .filter(|point| self.previous.get(point) != frame.targets.get(point))
            .collect();
        for point in changed {
            if let Some(cell) = frame.cells.get(&point) {
                dirty.entry(point).or_insert_with(|| cell.clone());
            }
        }
        for point in self.previous_sizes.keys().chain(frame.sizes.keys()) {
            if self.previous_sizes.get(point) != frame.sizes.get(point) {
                if let Some(cell) = frame.cells.get(point) {
                    dirty.entry(*point).or_insert_with(|| cell.clone());
                }
            }
        }
        self.inner.write_all(CLOSE)?;
        let cells: Vec<_> = dirty.iter().collect();
        let drawn = (|| {
            let mut start = 0;
            while start < cells.len() {
                let target = frame.targets.get(cells[start].0);
                let mut end = start + 1;
                while end < cells.len() && frame.targets.get(cells[end].0) == target {
                    end += 1;
                }
                if let Some(target) = target {
                    write!(self.inner, "\x1b]8;;{target}\x1b\\")?;
                }
                self.inner.draw(
                    cells[start..end]
                        .iter()
                        .map(|((y, x), cell)| (*x, *y, *cell)),
                )?;
                for ((y, x), cell) in &cells[start..end] {
                    let Some(scale) = frame.sizes.get(&(*y, *x)) else {
                        continue;
                    };
                    let Some(sequence) =
                        tanki_term::sized_text(cell.symbol(), *scale, cell.cell_width())
                    else {
                        continue;
                    };
                    // Set the cell's style through Crossterm, then replace its
                    // glyph at the same cursor position. OSC 66 stays outside
                    // Ratatui's symbols, so cell widths and links remain valid.
                    self.inner.draw(std::iter::once((*x, *y, *cell)))?;
                    self.inner.set_cursor_position(Position::new(*x, *y))?;
                    self.inner.write_all(sequence.as_bytes())?;
                }
                // Always close, including before drawing unrelated/image cells.
                self.inner.write_all(CLOSE)?;
                start = end;
            }
            Ok(())
        })();
        let closed = self.inner.write_all(CLOSE);
        drawn.and(closed)?;
        self.previous = frame.targets.clone();
        self.previous_sizes = frame.sizes.clone();
        Ok(())
    }
    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }
    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.inner.get_cursor_position()
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position)
    }
    fn clear(&mut self) -> io::Result<()> {
        self.previous.clear();
        self.previous_sizes.clear();
        self.inner.write_all(CLOSE)?;
        self.inner.clear()
    }
    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.previous.clear();
        self.previous_sizes.clear();
        self.inner.write_all(CLOSE)?;
        self.inner.clear_region(clear_type)
    }
    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.inner.append_lines(n)
    }
    fn size(&self) -> io::Result<Size> {
        self.inner.size()
    }
    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size()
    }
    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.inner)
    }
}

#[cfg(test)]
mod tests;
