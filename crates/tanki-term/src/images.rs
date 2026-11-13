//! Image display through ratatui-image (PLAN 5.1, 5.2).
//!
//! Kitty images use Unicode placeholders: the image is transmitted once as a
//! virtual placement and the buffer holds U+10EEEE text cells, so images clip,
//! scroll and survive multiplexers like any other text. Sixel and iTerm2
//! write their escape sequence into the first cell of the area, as
//! ratatui-image does.
//!
//! # What the TUI must do
//! - Draw through `render` every frame; the encoded form is cached by
//!   (key, area size) so only the first frame at a size pays for encoding.
//! - On a screen change (leaving the card view) call `clear`, then write the
//!   string from `take_cleanup` to the terminal (it frees kitty image data;
//!   it is empty for other modes). Placeholders that are no longer drawn show
//!   nothing, so skipping this only leaks terminal memory.
//! - On a mode switch (`set_mode`/`cycle`) write `take_cleanup` too and force
//!   a full redraw (`terminal.clear()`), so cells holding the old protocol's
//!   output are repainted.
//! - After a terminal resize, `render` re-encodes for the new area by itself.
//!
//! # Cost
//! Encoding is synchronous inside `render`, on a cache miss only. It is
//! dominated by resizing to the area and, for kitty, base64 of raw RGBA
//! (the area is at most a few hundred pixels per side after scaling, so the
//! payload stays around 1 MB). Measured in release on a 2000x1500 source:
//! 10-16 ms for 40x15 cells, 27-50 ms for 100x40 cells, any mode; debug builds
//! are several times slower. That is one slow frame per image, not async. See the ignored test `measure_encode_cost`.
use std::collections::HashMap;

use image::imageops::FilterType;
use image::DynamicImage;
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::{FontSize, Image, Resize};

use crate::{passthrough, Capabilities, GraphicsMode};

const MAX_ENTRIES: usize = 32;

struct Entry {
    /// `None`: encoding failed; remembered so a bad image is not retried per frame.
    proto: Option<Protocol>,
    kitty_id: Option<u32>,
    used: u64,
}

pub struct Images {
    picker: Picker,
    mode: GraphicsMode,
    detected: GraphicsMode,
    tmux: bool,
    cache: HashMap<(u64, u16, u16), Entry>,
    tick: u64,
    encodes: usize,
    stale_kitty: Vec<u32>,
}

fn resolve(mode: GraphicsMode, detected: GraphicsMode) -> GraphicsMode {
    match (mode, detected) {
        (GraphicsMode::Auto, GraphicsMode::Auto) => GraphicsMode::Blocks,
        (GraphicsMode::Auto, d) => d,
        (m, _) => m,
    }
}

impl Images {
    pub fn new(capabilities: &Capabilities) -> Self {
        let (w, h) = capabilities.cell_px.unwrap_or((10, 20));
        #[allow(deprecated)] // the only constructor taking a font size without querying stdio
        let picker = Picker::from_fontsize(FontSize::new(w.max(1), h.max(1)));
        let mut images = Self {
            picker,
            mode: GraphicsMode::None,
            detected: capabilities.detected,
            tmux: capabilities.tmux,
            cache: HashMap::new(),
            tick: 0,
            encodes: 0,
            stale_kitty: Vec::new(),
        };
        images.set_mode(capabilities.graphics);
        images
    }

    pub fn mode(&self) -> GraphicsMode {
        self.mode
    }

    /// Switches protocol live; drops cached encodings. `Auto` means the
    /// detected mode.
    pub fn set_mode(&mut self, mode: GraphicsMode) {
        let mode = resolve(mode, self.detected);
        self.forget();
        self.mode = mode;
        let protocol = match mode {
            GraphicsMode::Kitty => ProtocolType::Kitty,
            GraphicsMode::Iterm => ProtocolType::Iterm2,
            GraphicsMode::Sixel => ProtocolType::Sixel,
            _ => ProtocolType::Halfblocks,
        };
        self.picker.set_protocol_type(protocol);
    }

    /// Next mode in the order Kitty, Iterm, Sixel, Blocks, None; returns it.
    pub fn cycle(&mut self) -> GraphicsMode {
        let next = match self.mode {
            GraphicsMode::Kitty => GraphicsMode::Iterm,
            GraphicsMode::Iterm => GraphicsMode::Sixel,
            GraphicsMode::Sixel => GraphicsMode::Blocks,
            GraphicsMode::Blocks => GraphicsMode::None,
            _ => GraphicsMode::Kitty,
        };
        self.set_mode(next);
        next
    }

    /// Draws `image` scaled to fit `area` (aspect kept, centred). `key`
    /// identifies the image so the encoded form is reused across frames while
    /// the key, area size and mode stay the same. `None` draws nothing.
    pub fn render(&mut self, key: u64, image: &DynamicImage, area: Rect, buf: &mut Buffer) {
        let area = area.intersection(buf.area);
        if self.mode == GraphicsMode::None || area.width == 0 || area.height == 0 {
            return;
        }
        self.tick += 1;
        let slot = (key, area.width, area.height);
        if !self.cache.contains_key(&slot) {
            self.evict();
            self.encodes += 1;
            let proto = self
                .picker
                .new_protocol(
                    image.clone(),
                    Size::new(area.width, area.height),
                    Resize::Scale(Some(FilterType::Triangle)),
                )
                .ok();
            self.cache.insert(
                slot,
                Entry {
                    proto,
                    kitty_id: None,
                    used: 0,
                },
            );
        }
        let entry = self.cache.get_mut(&slot).expect("just inserted");
        entry.used = self.tick;
        let Some(proto) = &entry.proto else { return };
        let size = proto.size();
        let target = Rect {
            x: area.x + area.width.saturating_sub(size.width) / 2,
            y: area.y,
            width: size.width.min(area.width),
            height: size.height.min(area.height),
        };
        Image::new(proto)
            .allow_clipping(true)
            .render_into(target, buf);
        if self.mode == GraphicsMode::Kitty && entry.kitty_id.is_none() {
            // The transmit sequence rides in the first cell the first time.
            entry.kitty_id = buf
                .cell((target.x, target.y))
                .and_then(|c| kitty_id(c.symbol()));
        }
    }

    /// Forgets all encoded images and placements (call on screen change).
    /// Kitty image data is freed by writing `take_cleanup` to the terminal.
    pub fn clear(&mut self) {
        self.forget();
    }

    /// Escape sequences deleting kitty images transmitted earlier and now
    /// forgotten (empty when there are none). Write them to the terminal
    /// after `clear`, `set_mode` or `cycle`.
    pub fn take_cleanup(&mut self) -> String {
        let seq: String = self
            .stale_kitty
            .drain(..)
            .map(|id| passthrough(&format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\"), self.tmux))
            .collect();
        seq
    }

    /// How many images were encoded so far (cache misses).
    pub fn encode_count(&self) -> usize {
        self.encodes
    }

    fn forget(&mut self) {
        let ids = self.cache.drain().filter_map(|(_, e)| e.kitty_id);
        self.stale_kitty.extend(ids);
    }

    fn evict(&mut self) {
        if self.cache.len() < MAX_ENTRIES {
            return;
        }
        if let Some(oldest) = self
            .cache
            .iter()
            .min_by_key(|(_, e)| e.used)
            .map(|(k, _)| *k)
        {
            if let Some(e) = self.cache.remove(&oldest) {
                self.stale_kitty.extend(e.kitty_id);
            }
        }
    }
}

/// Extracts `i=<id>` from the kitty transmit sequence in a cell symbol.
fn kitty_id(symbol: &str) -> Option<u32> {
    let rest = symbol.split("q=2,i=").nth(1)?;
    let end = rest.find(|c: char| !c.is_ascii_digit())?;
    rest[..end].parse().ok()
}

trait RenderInto {
    fn render_into(self, area: Rect, buf: &mut Buffer);
}
impl RenderInto for Image<'_> {
    fn render_into(self, area: Rect, buf: &mut Buffer) {
        ratatui::widgets::Widget::render(self, area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};
    use std::time::Instant;

    fn caps(mode: GraphicsMode) -> Capabilities {
        Capabilities {
            graphics: mode,
            detected: GraphicsMode::Blocks,
            cell_px: Some((10, 20)),
            ..Capabilities::default()
        }
    }
    fn sample(w: u32, h: u32) -> DynamicImage {
        DynamicImage::ImageRgba8(RgbaImage::from_fn(w, h, |x, y| {
            Rgba([(x * 255 / w) as u8, (y * 255 / h) as u8, 90, 255])
        }))
    }
    fn render(img: &mut Images, key: u64, area: Rect) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 20));
        img.render(key, &sample(200, 100), area, &mut buf);
        buf
    }
    fn symbols(buf: &Buffer) -> Vec<String> {
        buf.content()
            .iter()
            .map(|c| c.symbol().to_owned())
            .collect()
    }

    #[test]
    fn blocks_draw_coloured_half_blocks() {
        let mut img = Images::new(&caps(GraphicsMode::Blocks));
        let buf = render(&mut img, 1, Rect::new(2, 1, 20, 10));
        let cells: Vec<_> = buf
            .content()
            .iter()
            .filter(|c| matches!(c.symbol(), "▀" | "▄"))
            .collect();
        assert!(!cells.is_empty());
        assert!(cells
            .iter()
            .all(|c| matches!(c.fg, ratatui::style::Color::Rgb(..))
                && matches!(c.bg, ratatui::style::Color::Rgb(..))));
    }

    #[test]
    fn kitty_draws_placeholders_with_id() {
        let mut img = Images::new(&caps(GraphicsMode::Kitty));
        let buf = render(&mut img, 1, Rect::new(0, 0, 20, 10));
        let syms = symbols(&buf);
        assert!(syms.iter().any(|s| s.contains('\u{10EEEE}')));
        let id = syms
            .iter()
            .find_map(|s| kitty_id(s))
            .expect("transmit sequence with id");
        // the placeholder colour encodes the low 24 bits of the id
        let [_, r, g, b] = id.to_be_bytes();
        assert!(buf
            .content()
            .iter()
            .any(|c| c.symbol().contains('\u{10EEEE}')
                && c.fg == ratatui::style::Color::Rgb(r, g, b)));
        // forgetting yields a delete for exactly that id
        img.clear();
        assert!(img.take_cleanup().contains(&format!("i={id},")));
        assert!(img.take_cleanup().is_empty());
    }

    #[test]
    fn sixel_and_iterm_write_their_sequences() {
        let mut img = Images::new(&caps(GraphicsMode::Sixel));
        let buf = render(&mut img, 1, Rect::new(0, 0, 20, 10));
        assert!(symbols(&buf).iter().any(|s| s.contains("\x1bP")));
        let mut img = Images::new(&caps(GraphicsMode::Iterm));
        let buf = render(&mut img, 1, Rect::new(0, 0, 20, 10));
        assert!(symbols(&buf).iter().any(|s| s.contains("\x1b]1337;File=")));
    }

    #[test]
    fn none_draws_nothing() {
        let mut img = Images::new(&caps(GraphicsMode::None));
        let buf = render(&mut img, 1, Rect::new(0, 0, 20, 10));
        assert_eq!(buf, Buffer::empty(Rect::new(0, 0, 40, 20)));
        assert_eq!(img.encode_count(), 0);
    }

    #[test]
    fn cache_reuse_and_invalidation() {
        let mut img = Images::new(&caps(GraphicsMode::Blocks));
        let area = Rect::new(0, 0, 20, 10);
        render(&mut img, 1, area);
        render(&mut img, 1, area);
        assert_eq!(img.encode_count(), 1);
        render(&mut img, 2, area);
        assert_eq!(img.encode_count(), 2);
        render(&mut img, 1, Rect::new(0, 0, 24, 10));
        assert_eq!(img.encode_count(), 3);
        img.set_mode(GraphicsMode::Sixel);
        render(&mut img, 1, area);
        assert_eq!(img.encode_count(), 4);
        img.clear();
        render(&mut img, 1, area);
        assert_eq!(img.encode_count(), 5);
    }

    #[test]
    fn fits_keeps_aspect_and_centres() {
        let mut img = Images::new(&caps(GraphicsMode::Blocks));
        // 2:1 image, 10x20 cells: 40 wide area is 2x taller-limited
        let buf = render(&mut img, 1, Rect::new(0, 0, 40, 5));
        let xs: Vec<u16> = (0..40)
            .filter(|&x| matches!(buf[(x, 0)].symbol(), "▀" | "▄"))
            .collect();
        assert!(
            xs.first().unwrap() > &0 && xs.last().unwrap() < &39,
            "centred: {xs:?}"
        );
        assert!(
            (0..40).all(|x| buf[(x, 5)].symbol() == " "),
            "stays inside area"
        );
    }

    #[test]
    fn cycle_order_and_auto() {
        let mut img = Images::new(&caps(GraphicsMode::Kitty));
        let order: Vec<_> = (0..6).map(|_| img.cycle()).collect();
        use GraphicsMode::*;
        assert_eq!(order, [Iterm, Sixel, Blocks, None, Kitty, Iterm]);
        img.set_mode(Auto);
        assert_eq!(img.mode(), Blocks);
        assert_eq!(Images::new(&caps(Auto)).mode(), Blocks);
    }

    #[test]
    #[ignore = "timing report: cargo test -p tanki-term --release measure_encode_cost -- --ignored --nocapture"]
    fn measure_encode_cost() {
        let photo = sample(2000, 1500);
        for mode in [
            GraphicsMode::Kitty,
            GraphicsMode::Sixel,
            GraphicsMode::Iterm,
            GraphicsMode::Blocks,
        ] {
            for (w, h) in [(40, 15), (100, 40)] {
                let mut img = Images::new(&caps(mode));
                let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
                let t = Instant::now();
                img.render(1, &photo, Rect::new(0, 0, w, h), &mut buf);
                println!("{mode:?} {w}x{h} cells from 2000x1500: {:?}", t.elapsed());
            }
        }
    }
}
