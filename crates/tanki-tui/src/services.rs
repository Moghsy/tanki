//! Adapters between the TUI and the renderer, image and audio crates, plus
//! the type mappings the TUI owns (`tanki_core` -> `tanki_render` /
//! `tanki_media`). Screens use the traits, so tests swap in fakes.
use crate::reviewer::{AudioCue, CueKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use tanki_media::rules::{self, Side};
use tanki_media::MediaTag;
use tanki_render::{AudioSide, CardSide, ImageSource, RenderOptions, RenderedCard, Rgb, Style};
#[cfg(test)]
use tanki_render::{Line, Span, Widget, WidgetKind};
use tanki_term::{Capabilities, GraphicsMode};
#[cfg(test)]
use unicode_width::UnicodeWidthStr;

// ---- card rendering ---------------------------------------------------------

pub trait CardRenderer {
    fn render(&self, side: &CardSide, options: &RenderOptions) -> RenderedCard;
    fn paint(&self, card: &RenderedCard, scroll: usize, area: Rect, buf: &mut Buffer);
}

/// tanki-render's T0/T1 layout.
pub struct AnkiRenderer;

impl CardRenderer for AnkiRenderer {
    fn render(&self, side: &CardSide, options: &RenderOptions) -> RenderedCard {
        tanki_render::render(side, options)
    }
    fn paint(&self, card: &RenderedCard, scroll: usize, area: Rect, buf: &mut Buffer) {
        tanki_render::paint(card, scroll, area, buf)
    }
}

/// Deterministic test renderer for screen snapshots. Production always uses
/// `AnkiRenderer`; this fake isolates TUI tests from renderer layout changes.
#[cfg(test)]
pub struct PlainTextRenderer;

#[cfg(test)]
impl CardRenderer for PlainTextRenderer {
    fn render(&self, side: &CardSide, options: &RenderOptions) -> RenderedCard {
        plain::render(&side.html, options)
    }
    fn paint(&self, card: &RenderedCard, scroll: usize, area: Rect, buf: &mut Buffer) {
        paint_lines(&card.lines, scroll, area, buf);
    }
}

pub fn rgb(c: Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

pub fn to_ratatui_style(style: &Style) -> ratatui::style::Style {
    let mut out = ratatui::style::Style::default();
    if let Some(fg) = style.fg {
        out = out.fg(rgb(fg));
    }
    if let Some(bg) = style.bg {
        out = out.bg(rgb(bg));
    }
    let flags = [
        (style.bold, Modifier::BOLD),
        (style.italic, Modifier::ITALIC),
        (style.underline, Modifier::UNDERLINED),
        (style.strikethrough, Modifier::CROSSED_OUT),
        (style.dim, Modifier::DIM),
    ];
    for (on, modifier) in flags {
        if on {
            out = out.add_modifier(modifier);
        }
    }
    out
}

/// Paints renderer lines (used by the plain renderer).
#[cfg(test)]
pub fn paint_lines(lines: &[Line], scroll: usize, area: Rect, buf: &mut Buffer) {
    for (row, line) in lines
        .iter()
        .skip(scroll)
        .take(area.height as usize)
        .enumerate()
    {
        let y = area.y + row as u16;
        let mut x = area.x;
        for span in &line.spans {
            if x >= area.right() {
                break;
            }
            let max = (area.right() - x) as usize;
            let (nx, _) = buf.set_stringn(x, y, &span.text, max, to_ratatui_style(&span.style));
            x = nx;
        }
    }
}

#[cfg(test)]
mod plain {
    use super::*;

    #[derive(Default)]
    struct State {
        lines: Vec<Line>,
        current: Vec<Span>,
        widgets: Vec<Widget>,
        answer_row: Option<usize>,
        bold: u32,
        italic: u32,
        underline: u32,
        span_colors: Vec<Option<Rgb>>,
        pending_space: bool,
        replay_count: usize,
    }

    impl State {
        fn style(&self) -> Style {
            Style {
                bold: self.bold > 0,
                italic: self.italic > 0,
                underline: self.underline > 0,
                fg: self.span_colors.iter().rev().find_map(|c| *c),
                ..Default::default()
            }
        }
        fn break_line(&mut self) {
            let spans = std::mem::take(&mut self.current);
            self.lines.push(Line { spans });
            self.pending_space = false;
        }
        fn block(&mut self) {
            if !self.current.is_empty() {
                self.break_line();
            }
        }
        fn push_text(&mut self, text: &str, widget: Option<usize>) {
            if self.pending_space && !self.current.is_empty() {
                self.current.push(Span {
                    text: " ".into(),
                    style: self.style(),
                    ..Default::default()
                });
            }
            self.pending_space = false;
            self.current.push(Span {
                text: text.into(),
                style: self.style(),
                link: None,
                widget,
            });
        }
        fn widget(&mut self, kind: WidgetKind, key: String, text: &str) {
            let index = self.widgets.len();
            self.widgets.push(Widget {
                key,
                kind,
                row: 0,
                col: 0,
                width: text.width() as u16,
            });
            self.push_text(text, Some(index));
        }
        fn text(&mut self, raw: &str, hide_audio: bool) {
            let decoded = decode_entities(raw);
            let starts_space = decoded.starts_with(char::is_whitespace);
            let ends_space = decoded.ends_with(char::is_whitespace);
            if starts_space {
                self.pending_space = true;
            }
            let mut first = true;
            for word in decoded.split_whitespace() {
                if !first {
                    self.pending_space = true;
                }
                first = false;
                self.word(word, hide_audio);
            }
            if ends_space && !decoded.trim().is_empty() {
                self.pending_space = true;
            }
        }
        fn word(&mut self, word: &str, hide_audio: bool) {
            // `[anki:play:q:N]` audio references become replay buttons.
            let mut rest = word;
            while let Some(start) = rest.find("[anki:play:") {
                if start > 0 {
                    self.push_text(&rest[..start], None);
                }
                let tail = &rest[start + "[anki:play:".len()..];
                let Some(end) = tail.find(']') else {
                    break;
                };
                let mut parts = tail[..end].split(':');
                let side = match parts.next() {
                    Some("a") => AudioSide::Answer,
                    _ => AudioSide::Question,
                };
                let index = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
                if !hide_audio {
                    let key = format!("replay:{}", self.replay_count);
                    self.replay_count += 1;
                    self.widget(WidgetKind::Replay { side, index }, key, "▶");
                }
                rest = &tail[end + 1..];
            }
            if !rest.is_empty() {
                self.push_text(rest, None);
            }
        }
    }

    fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
        let lower = tag.to_ascii_lowercase();
        let pos = lower.find(&format!("{name}="))?;
        let value = &tag[pos + name.len() + 1..];
        let (quote, value) = match value.chars().next()? {
            q @ ('"' | '\'') => (Some(q), &value[1..]),
            _ => (None, value),
        };
        let end = match quote {
            Some(q) => value.find(q).unwrap_or(value.len()),
            None => value
                .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
                .unwrap_or(value.len()),
        };
        Some(&value[..end])
    }

    fn decode_entities(text: &str) -> String {
        text.replace("&nbsp;", "\u{a0}")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&darr;", "↓")
            .replace("&amp;", "&")
    }

    fn strip_elements(html: &str, name: &str) -> String {
        let mut out = String::new();
        let mut rest = html;
        let open = format!("<{name}");
        let close = format!("</{name}>");
        loop {
            let lower = rest.to_ascii_lowercase();
            let Some(start) = lower.find(&open) else {
                out.push_str(rest);
                return out;
            };
            out.push_str(&rest[..start]);
            match lower[start..].find(&close) {
                Some(end) => rest = &rest[start + end + close.len()..],
                None => return out,
            }
        }
    }

    pub fn render(html: &str, options: &RenderOptions) -> RenderedCard {
        let html = strip_elements(&strip_elements(html, "style"), "script");
        let mut s = State::default();
        let mut rest = html.as_str();
        while !rest.is_empty() {
            let Some(lt) = rest.find('<') else {
                s.text(rest, options.hide_audio_buttons);
                break;
            };
            if lt > 0 {
                s.text(&rest[..lt], options.hide_audio_buttons);
            }
            let Some(gt) = rest[lt..].find('>') else {
                s.text(&rest[lt..], options.hide_audio_buttons);
                break;
            };
            let tag = &rest[lt + 1..lt + gt];
            rest = &rest[lt + gt + 1..];
            let closing = tag.starts_with('/');
            let name: String = tag
                .trim_start_matches('/')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            match (name.as_str(), closing) {
                ("br", _) => s.break_line(),
                ("hr", false) => {
                    s.block();
                    if attr(tag, "id") == Some("answer") {
                        s.answer_row = Some(s.lines.len());
                    }
                    s.push_text(&"─".repeat(options.width.min(40) as usize), None);
                    s.break_line();
                }
                (
                    "div" | "p" | "li" | "tr" | "center" | "ul" | "ol" | "table" | "h1" | "h2"
                    | "h3" | "h4" | "blockquote" | "pre",
                    _,
                ) => s.block(),
                ("b" | "strong", false) => s.bold += 1,
                ("b" | "strong", true) => s.bold = s.bold.saturating_sub(1),
                ("i" | "em", false) => s.italic += 1,
                ("i" | "em", true) => s.italic = s.italic.saturating_sub(1),
                ("u", false) => s.underline += 1,
                ("u", true) => s.underline = s.underline.saturating_sub(1),
                ("span", false) => {
                    let color = match attr(tag, "class") {
                        Some("typeGood") => Some(Rgb(0, 160, 0)),
                        Some("typeBad") => Some(Rgb(200, 0, 0)),
                        Some("typeMissed") => Some(Rgb(128, 128, 128)),
                        _ => None,
                    };
                    s.span_colors.push(color);
                }
                ("span", true) => {
                    s.span_colors.pop();
                }
                ("input", false) if attr(tag, "id") == Some("typeans") => {
                    s.block();
                    let width = options.width.clamp(1, 30) as usize;
                    s.widget(WidgetKind::TypeAnswer, "typeans".into(), &" ".repeat(width));
                    s.break_line();
                }
                ("img", false) => {
                    let src = attr(tag, "src").unwrap_or("?");
                    s.push_text(&format!("[image: {src}]"), None);
                }
                _ => {}
            }
        }
        s.block();
        wrap(s, options.width.max(1) as usize)
    }

    fn trim_end(line: &mut Line) {
        while line
            .spans
            .last()
            .is_some_and(|s| s.widget.is_none() && s.text.trim().is_empty())
        {
            line.spans.pop();
        }
    }

    /// Wraps lines at `width` columns and fixes widget positions.
    fn wrap(s: State, width: usize) -> RenderedCard {
        let mut lines: Vec<Line> = Vec::new();
        let mut widgets = s.widgets;
        let mut answer_row = None;
        for (index, line) in s.lines.into_iter().enumerate() {
            if s.answer_row == Some(index) {
                answer_row = Some(lines.len());
            }
            let mut current = Line::default();
            let mut used = 0;
            for mut span in line.spans {
                loop {
                    let w = span.text.width();
                    if used + w <= width {
                        if let Some(i) = span.widget {
                            widgets[i].row = lines.len();
                            widgets[i].col = used as u16;
                        }
                        used += w;
                        current.spans.push(span);
                        break;
                    }
                    if used > 0 && w <= width {
                        trim_end(&mut current);
                        lines.push(std::mem::take(&mut current));
                        used = 0;
                        if span.text == " " {
                            break;
                        }
                        continue;
                    }
                    // A word longer than the line: split it.
                    let mut cut = 0;
                    let mut cols = 0;
                    for (i, c) in span.text.char_indices() {
                        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
                        if used + cols + cw > width {
                            break;
                        }
                        cols += cw;
                        cut = i + c.len_utf8();
                    }
                    if cut == 0 {
                        lines.push(std::mem::take(&mut current));
                        used = 0;
                        if width == 0 {
                            break;
                        }
                        cut = span.text.chars().next().map_or(0, char::len_utf8);
                    }
                    let head = Span {
                        text: span.text[..cut].to_owned(),
                        ..span.clone()
                    };
                    span.text = span.text[cut..].to_owned();
                    current.spans.push(head);
                    lines.push(std::mem::take(&mut current));
                    used = 0;
                    if span.text.is_empty() {
                        break;
                    }
                }
            }
            lines.push(current);
        }
        // Drop trailing blank lines.
        while lines.last().is_some_and(|l| l.spans.is_empty()) {
            lines.pop();
        }
        RenderedCard {
            lines,
            images: Vec::new(),
            widgets,
            answer_row,
            notices: vec!["plain-text renderer (development fallback)".into()],
        }
    }
}

// ---- mappings ----------------------------------------------------------------

/// `tanki_core::RenderedCard` + side HTML -> `tanki_render::CardSide`.
pub fn card_side(
    card: &tanki_core::RenderedCard,
    html: String,
    media_dir: &Path,
    night_mode: bool,
) -> CardSide {
    CardSide {
        html,
        css: card.css.clone(),
        body_classes: card.body_classes(night_mode),
        media_dir: media_dir.to_path_buf(),
    }
}

/// Terminal facts the renderer needs, from `Capabilities`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Display {
    pub cell_px: Option<(u16, u16)>,
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    /// From `Capabilities::dark()`; an unknown background counts as dark,
    /// the common terminal default.
    pub night_mode: bool,
}

impl Display {
    pub fn from_capabilities(caps: &Capabilities) -> Self {
        Self {
            cell_px: caps.cell_px,
            fg: caps.foreground.map(|(r, g, b)| Rgb(r, g, b)),
            bg: caps.background.map(|(r, g, b)| Rgb(r, g, b)),
            night_mode: caps.dark().unwrap_or(true),
        }
    }
    pub fn render_options(
        &self,
        graphics: GraphicsMode,
        area: Rect,
        hide_audio_buttons: bool,
        expanded: &BTreeSet<String>,
    ) -> RenderOptions {
        RenderOptions {
            width: area.width,
            viewport_rows: area.height,
            cell_px: self.cell_px,
            night_mode: self.night_mode,
            fg: self.fg,
            bg: self.bg,
            images: !matches!(graphics, GraphicsMode::None | GraphicsMode::Auto),
            hide_audio_buttons,
            expanded: expanded.clone(),
            ..RenderOptions::default()
        }
    }
}

// ---- images -------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageState {
    Drawn,
    Loading,
    Failed(String),
}

pub trait ImageDisplay {
    fn mode(&self) -> GraphicsMode;
    fn cycle(&mut self) -> GraphicsMode;
    /// Draws the image scaled into `area`.
    fn draw(&mut self, source: &ImageSource, area: Rect, buf: &mut Buffer) -> ImageState;
    /// Forget placements (screen change).
    fn clear(&mut self);
    /// Escape sequences to write to the terminal after `clear`/`cycle`.
    fn take_cleanup(&mut self) -> String;
}

/// tanki-render's decoder + tanki-term's protocols.
pub struct TermImages {
    images: tanki_term::Images,
    /// `None` while tanki-render's cache is not available.
    cache: Option<tanki_render::ImageCache>,
    cell_px: (u16, u16),
}

impl TermImages {
    pub fn new(
        images: tanki_term::Images,
        cache: Option<tanki_render::ImageCache>,
        cell_px: Option<(u16, u16)>,
    ) -> Self {
        Self {
            images,
            cache,
            cell_px: cell_px.unwrap_or((10, 20)),
        }
    }
    pub fn subscribe(&self) -> Option<std::sync::mpsc::Receiver<()>> {
        self.cache.as_ref().map(|c| c.subscribe())
    }
}

impl ImageDisplay for TermImages {
    fn mode(&self) -> GraphicsMode {
        self.images.mode()
    }
    fn cycle(&mut self) -> GraphicsMode {
        self.images.cycle()
    }
    fn draw(&mut self, source: &ImageSource, area: Rect, buf: &mut Buffer) -> ImageState {
        let Some(cache) = &self.cache else {
            return ImageState::Failed("image decoding is not available yet".into());
        };
        let max_px = (
            u32::from(area.width) * u32::from(self.cell_px.0),
            u32::from(area.height) * u32::from(self.cell_px.1),
        );
        cache.request(source, max_px);
        match cache.get(source, max_px) {
            None => ImageState::Loading,
            Some(Err(e)) => ImageState::Failed(format!("{e:?}")),
            Some(Ok(image)) => {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                source.hash(&mut hasher);
                self.images.render(hasher.finish(), &image, area, buf);
                ImageState::Drawn
            }
        }
    }
    fn clear(&mut self) {
        self.images.clear();
    }
    fn take_cleanup(&mut self) -> String {
        self.images.take_cleanup()
    }
}

/// No graphics: images stay as the renderer's text placeholders.
pub struct NoImages;

impl ImageDisplay for NoImages {
    fn mode(&self) -> GraphicsMode {
        GraphicsMode::None
    }
    fn cycle(&mut self) -> GraphicsMode {
        GraphicsMode::None
    }
    fn draw(&mut self, _: &ImageSource, _: Rect, _: &mut Buffer) -> ImageState {
        ImageState::Failed("graphics are off".into())
    }
    fn clear(&mut self) {}
    fn take_cleanup(&mut self) -> String {
        String::new()
    }
}

// ---- audio ----------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioStatus {
    Mpv,
    Builtin,
    NoDevice(String),
    Off,
}

pub trait AudioPlayer {
    /// Applies Anki's autoplay/replay rules to `cue` and plays the result.
    fn cue(&self, cue: &AudioCue);
    /// One tag (a replay button).
    fn play_one(&self, tag: MediaTag, interrupt: bool);
    fn stop(&self);
    fn toggle_pause(&self);
    fn seek(&self, seconds: f64);
    fn is_playing(&self) -> bool;
    fn status(&self) -> AudioStatus;
}

/// The tags Anki's rules choose for a cue.
pub fn cue_tags(cue: &AudioCue) -> Vec<MediaTag> {
    match cue.kind {
        CueKind::ShowQuestion => rules::on_show_question(&cue.question, cue.autoplay),
        CueKind::ShowAnswer => rules::on_show_answer(&cue.answer, cue.autoplay),
        CueKind::Replay(side) => rules::replay(
            side,
            &cue.question,
            &cue.answer,
            cue.skip_question_when_replaying_answer,
        ),
    }
}

pub struct MediaAudio {
    pub audio: tanki_media::Audio,
}

impl AudioPlayer for MediaAudio {
    fn cue(&self, cue: &AudioCue) {
        // Desktop's `play_tags` replaces the queue even when empty, so
        // showing a side without sound still interrupts per the preference.
        self.audio.play(cue_tags(cue), cue.interrupt);
    }
    fn play_one(&self, tag: MediaTag, interrupt: bool) {
        self.audio.play(vec![tag], interrupt);
    }
    fn stop(&self) {
        self.audio.stop();
    }
    fn toggle_pause(&self) {
        self.audio.toggle_pause();
    }
    fn seek(&self, seconds: f64) {
        self.audio.seek(seconds);
    }
    fn is_playing(&self) -> bool {
        self.audio.is_playing()
    }
    fn status(&self) -> AudioStatus {
        match self.audio.status() {
            tanki_media::BackendStatus::Mpv => AudioStatus::Mpv,
            tanki_media::BackendStatus::Builtin => AudioStatus::Builtin,
            tanki_media::BackendStatus::NoDevice(reason) => AudioStatus::NoDevice(reason),
            tanki_media::BackendStatus::Off => AudioStatus::Off,
        }
    }
}

/// Replay-button side -> rules side.
pub fn rules_side(side: AudioSide) -> Side {
    match side {
        AudioSide::Question => Side::Question,
        AudioSide::Answer => Side::Answer,
    }
}

// ---- open in browser --------------------------------------------------------------

fn file_url(dir: &Path) -> String {
    let mut url = String::from("file://");
    for byte in dir.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                url.push(byte as char)
            }
            other => url.push_str(&format!("%{other:02X}")),
        }
    }
    if !url.ends_with('/') {
        url.push('/');
    }
    url
}

/// A standalone page for one card side (the escape hatch, PLAN 6.1).
pub fn card_page(side: &CardSide) -> String {
    format!(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\">\n<base href=\"{}\">\n<style>\n{}\n</style></head>\n<body class=\"{}\">\n<div id=\"qa\">\n{}\n</div>\n</body></html>\n",
        file_url(&side.media_dir),
        side.css,
        side.body_classes,
        side.html
    )
}

/// Writes the page into the system temp folder (never the profile).
pub fn write_card_page(side: &CardSide, card_id: i64, profile: &Path) -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("tanki-card-{}-{card_id}.html", std::process::id()));
    if let (Ok(p), Ok(d)) = (profile.canonicalize(), dir.canonicalize()) {
        if d.starts_with(&p) {
            return Err(std::io::Error::other(
                "the temp folder is inside the profile; not writing there",
            ));
        }
    }
    std::fs::write(&path, card_page(side))?;
    Ok(path)
}

/// Opens a file with the desktop's handler, detached and silent.
pub fn system_open(path: &Path) -> std::io::Result<()> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(width: u16) -> RenderOptions {
        RenderOptions {
            width,
            ..Default::default()
        }
    }

    fn text(card: &RenderedCard) -> Vec<String> {
        card.lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.text.as_str()).collect())
            .collect()
    }

    #[test]
    fn plain_renderer_blocks_widgets_and_wrapping() {
        let html = "<style>.card{}</style><b>Front</b> side[anki:play:q:0]<hr id=answer>Back &amp; more words here<br><center><input type=text id=typeans></center>";
        let card = PlainTextRenderer.render(
            &CardSide {
                html: html.into(),
                ..Default::default()
            },
            &opts(12),
        );
        assert_eq!(
            text(&card),
            [
                "Front side▶",
                "────────────",
                "Back & more",
                "words here",
                "            "
            ]
        );
        assert_eq!(card.answer_row, Some(1));
        assert!(card.lines[0].spans[0].style.bold);
        assert_eq!(card.widgets.len(), 2);
        assert_eq!(
            card.widgets[0].kind,
            WidgetKind::Replay {
                side: AudioSide::Question,
                index: 0
            }
        );
        assert_eq!((card.widgets[0].row, card.widgets[0].col), (0, 10));
        assert_eq!(card.widgets[1].kind, WidgetKind::TypeAnswer);
        assert_eq!(card.widgets[1].row, 4);
        let hidden = PlainTextRenderer.render(
            &CardSide {
                html: "a [anki:play:a:2] b".into(),
                ..Default::default()
            },
            &RenderOptions {
                hide_audio_buttons: true,
                ..opts(20)
            },
        );
        assert_eq!(text(&hidden), ["a b"]);
        let long = PlainTextRenderer.render(
            &CardSide {
                html: "abcdefghij".into(),
                ..Default::default()
            },
            &opts(4),
        );
        assert_eq!(text(&long), ["abcd", "efgh", "ij"]);
    }

    #[test]
    fn type_answer_diff_colours() {
        let card = PlainTextRenderer.render(
            &CardSide {
                html: "<code id=typeans><span class=typeGood>Par</span><span class=typeBad>x</span></code>".into(),
                ..Default::default()
            },
            &opts(20),
        );
        assert_eq!(card.lines[0].spans[0].style.fg, Some(Rgb(0, 160, 0)));
        assert_eq!(card.lines[0].spans[1].style.fg, Some(Rgb(200, 0, 0)));
    }

    #[test]
    fn maps_core_card_and_capabilities() {
        let core = tanki_core::RenderedCard {
            css: ".card{}".into(),
            ord: 1,
            ..Default::default()
        };
        let side = card_side(&core, "<b>x</b>".into(), Path::new("/m"), true);
        assert_eq!(side.css, ".card{}");
        assert!(side.body_classes.starts_with("card card2"));
        assert!(side.body_classes.ends_with("nightMode night_mode"));
        let caps = Capabilities {
            cell_px: Some((9, 18)),
            background: Some((250, 250, 250)),
            foreground: Some((0, 0, 0)),
            ..Default::default()
        };
        let display = Display::from_capabilities(&caps);
        assert!(!display.night_mode);
        let o = display.render_options(
            GraphicsMode::Kitty,
            Rect::new(0, 0, 50, 20),
            true,
            &BTreeSet::new(),
        );
        assert_eq!(
            (o.width, o.viewport_rows, o.cell_px),
            (50, 20, Some((9, 18)))
        );
        assert!(o.images && o.hide_audio_buttons && !o.night_mode);
        assert_eq!(o.bg, Some(Rgb(250, 250, 250)));
        let o = display.render_options(
            GraphicsMode::None,
            Rect::new(0, 0, 5, 5),
            false,
            &BTreeSet::new(),
        );
        assert!(!o.images);
        assert!(Display::from_capabilities(&Capabilities::default()).night_mode);
    }

    #[test]
    fn card_page_has_base_css_and_classes() {
        let side = CardSide {
            html: "<img src=\"a b.png\">".into(),
            css: ".card { color: red }".into(),
            body_classes: "card card1 isLin".into(),
            media_dir: PathBuf::from("/tmp/p q/collection.media"),
        };
        let page = card_page(&side);
        assert!(page.contains("<base href=\"file:///tmp/p%20q/collection.media/\">"));
        assert!(page.contains(".card { color: red }"));
        assert!(page.contains("<body class=\"card card1 isLin\">"));
        assert!(page.contains("<img src=\"a b.png\">"));
        let profile = tempfile_dir();
        let path = write_card_page(&side, 42, &profile).unwrap();
        assert!(path.starts_with(std::env::temp_dir()));
        assert!(!path.starts_with(&profile));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), page);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(profile).unwrap();
    }

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tanki-tui-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
