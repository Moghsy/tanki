//! tanki-render: card HTML + CSS -> styled terminal lines, image blocks and
//! interactive widgets (PLAN 6.1 tiers T0/T1, 6.2 supported subset).
//!
//! This crate must not depend on `anki` or `tanki-core` (PLAN 4.2, section 14
//! rule 9). It takes strings and a media folder, and knows nothing about
//! collections.
//!
//! Interface fixed in the Phase 1 "agree first" step. The renderer stream may
//! add items; changing or removing the items below needs the lead.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;

mod css;
mod image_cache;
mod layout;
mod media;
mod paint;
mod para;
mod style;
mod text;

pub use media::{image_cells, resolve as resolve_media, MediaRef, SizeRequest};

/// One side of a card, ready to lay out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CardSide {
    /// The side's HTML after the reviewer filters (`RenderedCard::review_question`
    /// / `review_answer` in tanki-core): may contain `[anki:play:q:N]` /
    /// `[anki:play:a:N]` audio references, `<input type=text id=typeans>`, and
    /// the `<code id=typeans>` type-answer diff.
    pub html: String,
    /// The note type's CSS (without `<style>` tags).
    pub css: String,
    /// Body classes, e.g. `card card1 isLin nightMode night_mode`.
    pub body_classes: String,
    /// `<profile>/collection.media`; relative `src` attributes resolve here.
    pub media_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rgb(pub u8, pub u8, pub u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RubyMode {
    /// Ruby text on a reserved row above the base text.
    #[default]
    Above,
    /// `漢字(かんじ)`.
    Inline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderOptions {
    /// Columns available for the card.
    pub width: u16,
    /// Rows visible at once; images are clamped to about 95% of this
    /// (`max-height: 95vh`).
    pub viewport_rows: u16,
    /// Cell size in pixels `(width, height)`; `None` assumes `(10, 20)`.
    pub cell_px: Option<(u16, u16)>,
    /// Dark terminal: adds the `night-mode` html class, and the reviewer's
    /// `body.nightMode` rule replaces the card's own colours with `fg`/`bg`.
    pub night_mode: bool,
    /// The terminal's default colours (OSC 10/11), when known. Used for the
    /// night-mode override and to keep author colours readable.
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    /// Graphics available (tier T1): images become [`ImageBlock`]s with
    /// reserved blank cells. Otherwise (T0) they become labelled
    /// placeholders such as `[image: name.png]`.
    pub images: bool,
    /// Do not draw replay buttons for `[anki:play:…]` references.
    pub hide_audio_buttons: bool,
    pub ruby: RubyMode,
    /// Keys of hint and `<details>` widgets the user has opened.
    pub expanded: BTreeSet<String>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            width: 80,
            viewport_rows: 24,
            cell_px: None,
            night_mode: false,
            fg: None,
            bg: None,
            images: false,
            hide_audio_buttons: false,
            ruby: RubyMode::Above,
            expanded: BTreeSet::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
pub struct Style {
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub dim: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Span {
    pub text: String,
    pub style: Style,
    /// OSC 8 hyperlink target.
    pub link: Option<String>,
    /// Index into [`RenderedCard::widgets`] when this span belongs to one.
    pub widget: Option<usize>,
}

/// One terminal row. Alignment is already applied (leading spaces), and the
/// display width never exceeds [`RenderOptions::width`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Line {
    pub spans: Vec<Span>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ImageSource {
    /// A file inside the media folder (already resolved and checked).
    File(PathBuf),
    /// An inline `<svg>` element, serialised.
    Svg(String),
}

/// A rectangle of cells reserved for an image. The cells are blank in
/// [`RenderedCard::lines`]; the caller draws the image over them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageBlock {
    pub source: ImageSource,
    /// Top-left cell, relative to the first line of the card.
    pub row: usize,
    pub col: u16,
    /// Size in cells.
    pub width: u16,
    pub height: u16,
    pub alt: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioSide {
    Question,
    Answer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WidgetKind {
    /// `[anki:play:q:N]` / `[anki:play:a:N]`: plays that one tag.
    Replay { side: AudioSide, index: usize },
    /// `<input id=typeans>`: the caller draws the input over this widget's cells.
    TypeAnswer,
    /// Hint filter markup (`<a class=hint …>` + hidden `<div class=hint>`).
    Hint { label: String, expanded: bool },
    /// `<details><summary>…</summary>…</details>`.
    Details { summary: String, expanded: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Widget {
    /// Stable within one side for the same HTML (e.g. `hint:0`), so
    /// [`RenderOptions::expanded`] survives re-rendering.
    pub key: String,
    pub kind: WidgetKind,
    pub row: usize,
    pub col: u16,
    pub width: u16,
}

/// The renderer's output for one side (PLAN 11 Phase 1 "agree first").
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RenderedCard {
    pub lines: Vec<Line>,
    pub images: Vec<ImageBlock>,
    pub widgets: Vec<Widget>,
    /// Row of `<hr id=answer>`, to scroll the answer into view.
    pub answer_row: Option<usize>,
    /// Things the text tier could not show, for a status-line badge
    /// (e.g. "card uses JavaScript").
    pub notices: Vec<String>,
}

/// Lays out one side. Pure and synchronous; image decoding is separate
/// ([`ImageCache`]).
pub fn render(side: &CardSide, options: &RenderOptions) -> RenderedCard {
    layout::render(side, options)
}

/// Paints the text of `card` into `buf` inside `area`, starting at line
/// `scroll`. Image cells stay blank; the caller draws images. Widgets get no
/// special drawing beyond their spans.
pub fn paint(
    card: &RenderedCard,
    scroll: usize,
    area: ratatui::layout::Rect,
    buf: &mut ratatui::buffer::Buffer,
) {
    paint::paint(card, scroll, area, buf)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageError {
    NotFound(PathBuf),
    Unsupported(String),
    Decode(String),
}

/// Decodes and scales images off the UI thread, with a cache keyed by source
/// and target size. Supports jpeg, png, gif (first frame), webp and svg
/// (resvg).
pub struct ImageCache {
    inner: image_cache::Inner,
}

impl ImageCache {
    pub fn new() -> Self {
        ImageCache {
            inner: image_cache::Inner::new(),
        }
    }
    /// Starts decoding `source` scaled to fit `max_px` (width, height) unless
    /// it is cached or already queued. Never blocks.
    pub fn request(&self, source: &ImageSource, max_px: (u32, u32)) {
        self.inner.request(source, max_px)
    }
    /// The decoded image, once ready.
    pub fn get(
        &self,
        source: &ImageSource,
        max_px: (u32, u32),
    ) -> Option<Result<std::sync::Arc<image::DynamicImage>, ImageError>> {
        self.inner.get(source, max_px)
    }
    /// Receives a message whenever a requested image finishes, so the UI can redraw.
    pub fn subscribe(&self) -> std::sync::mpsc::Receiver<()> {
        self.inner.subscribe()
    }
}

impl Default for ImageCache {
    fn default() -> Self {
        Self::new()
    }
}
