//! The DOM walker: block flow, lists, tables, widgets, images and the final
//! assembly into [`RenderedCard`].

use crate::css::{MediaEnv, Origin, StyleSheet};
use crate::media::{self, MediaRef, SizeRequest};
use crate::para::{push_span, row_width, spaces, Atom, IRow, ISpan, Meta, Para, TStyle};
use crate::style::{
    Align, Computed, Display, Fg, Len, ListStyle, Styler, VAlign, WhiteSpace, UA_CSS,
};
use crate::text::{self, ensure_contrast, map_script, str_width};
use crate::{
    AudioSide, CardSide, ImageBlock, ImageSource, Line, RenderOptions, RenderedCard, Rgb, RubyMode,
    Span, Style, Widget, WidgetKind,
};
use ego_tree::NodeId;
use scraper::node::Node;
use scraper::{ElementRef, Html};
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::rc::Rc;
use unicode_segmentation::UnicodeSegmentation;

const DARK_CANVAS: Rgb = Rgb(0x1e, 0x1e, 0x1e);
const LIGHT_CANVAS: Rgb = Rgb(0xff, 0xff, 0xff);
const MAX_DEPTH: usize = 200;
const MAX_TABLE_DEPTH: usize = 3;

#[derive(Clone, Copy, Debug)]
struct Frame {
    col: usize,
    width: usize,
}

/// Rows being emitted for one flow (the card, or one table cell).
#[derive(Default)]
struct Out {
    rows: Vec<IRow>,
    pending_margin: usize,
    answer_row: Option<usize>,
    /// A list marker waiting for its item's first row: spans and the column
    /// where the marker must end.
    marker: Option<(IRow, usize)>,
}

impl Out {
    fn margin(&mut self, n: usize) {
        self.pending_margin = self.pending_margin.max(n);
    }

    fn emit(&mut self, content: IRow, col: usize) {
        if !self.rows.is_empty() {
            for _ in 0..self.pending_margin {
                self.rows.push(IRow::new());
            }
        }
        self.pending_margin = 0;
        let mut row = IRow::new();
        let blank = Meta::default();
        let mut cur = 0;
        if let Some((marker, end)) = self.marker.take() {
            let mw = row_width(&marker);
            let start = end.saturating_sub(mw);
            push_span(&mut row, &spaces(start), &blank);
            for s in marker {
                push_span(&mut row, &s.text, &s.meta);
            }
            cur = start + mw;
        }
        if col > cur && !content.is_empty() {
            push_span(&mut row, &spaces(col - cur), &blank);
        }
        for s in content {
            push_span(&mut row, &s.text, &s.meta);
        }
        self.rows.push(row);
    }
}

struct HintAnchor {
    key: String,
    label: String,
    expanded: bool,
}

struct WidgetRec {
    key: String,
    kind: WidgetKind,
}

struct ImageRec {
    source: ImageSource,
    width: usize,
    height: usize,
    alt: String,
}

struct Walker<'a> {
    opts: &'a RenderOptions,
    styler: Styler,
    media_dir: &'a Path,
    cell_px: (u16, u16),
    default_fg: Rgb,
    widgets: Vec<WidgetRec>,
    images: Vec<ImageRec>,
    notices: Vec<String>,
    hint_anchors: HashMap<NodeId, HintAnchor>,
    /// `id` of a hint's hidden `<div>` -> expanded.
    hint_targets: HashMap<String, bool>,
    details_keys: HashMap<NodeId, String>,
    cur_widget: Option<usize>,
    cur_link: Option<Rc<str>>,
    depth: usize,
    table_depth: usize,
    image_occlusion: bool,
    occlusion: Option<crate::occlusion::Occlusion>,
}

thread_local! {
    static SHEETS: RefCell<Vec<(u64, Rc<StyleSheet>)>> = const { RefCell::new(Vec::new()) };
}

fn load_sheet(author_css: &[String], env: &MediaEnv) -> Rc<StyleSheet> {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    author_css.hash(&mut h);
    (env.width_px as u32, env.height_px as u32, env.dark).hash(&mut h);
    let key = h.finish();
    if let Some(s) = SHEETS.with(|c| {
        c.borrow()
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, s)| s.clone())
    }) {
        return s;
    }
    let mut sheet = StyleSheet::default();
    let mut order = 0;
    sheet.parse_into(UA_CSS, Origin::UserAgent, env, &mut order);
    sheet.parse_into(crate::style::REVIEWER_CSS, Origin::Author, env, &mut order);
    for css in author_css {
        sheet.parse_into(css, Origin::Author, env, &mut order);
    }
    let sheet = Rc::new(sheet);
    SHEETS.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= 8 {
            c.remove(0);
        }
        c.push((key, sheet.clone()));
    });
    sheet
}

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

/// Reads a local stylesheet named by `<link>` or `@import` (media folder only).
fn read_local_css(media_dir: &Path, href: &str) -> Option<String> {
    match media::resolve(media_dir, href) {
        MediaRef::Local(p) if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("css")) => {
            let meta = std::fs::metadata(&p).ok()?;
            if meta.len() > 1 << 20 {
                return None;
            }
            std::fs::read_to_string(p).ok()
        }
        _ => None,
    }
}

fn attr_has_word(el: &ElementRef, attr: &str, word: &str) -> bool {
    el.value().attr(attr).is_some_and(|v| {
        v.split_ascii_whitespace()
            .any(|w| w.eq_ignore_ascii_case(word))
    })
}

pub fn render(side: &CardSide, opts: &RenderOptions) -> RenderedCard {
    let width = opts.width as usize;
    if width == 0 {
        return RenderedCard::default();
    }
    let cell_px = opts
        .cell_px
        .filter(|(w, h)| *w > 0 && *h > 0)
        .unwrap_or((10, 20));
    let mut body_classes = side.body_classes.trim().to_string();
    if opts.night_mode {
        for c in ["nightMode", "night_mode"] {
            if !body_classes.split_whitespace().any(|w| w == c) {
                body_classes.push(' ');
                body_classes.push_str(c);
            }
        }
    }
    let html_class = if opts.night_mode {
        "linux chrome night-mode"
    } else {
        "linux chrome"
    };
    let doc = format!(
        "<!doctype html><html class=\"{}\"><body class=\"{}\"><div id=\"qa\">{}</div></body></html>",
        html_class,
        escape_attr(body_classes.trim()),
        side.html
    );
    let dom = Html::parse_document(&doc);

    // Style sheets: the note type's CSS, then <style> elements and local
    // <link rel=stylesheet>/@import files in document order.
    let mut author_css = vec![side.css.clone()];
    for node in dom.tree.root().descendants() {
        let Some(el) = ElementRef::wrap(node) else {
            continue;
        };
        match el.value().name() {
            "style" => author_css.push(el.text().collect()),
            "link" if attr_has_word(&el, "rel", "stylesheet") => {
                if let Some(css) = el
                    .value()
                    .attr("href")
                    .and_then(|h| read_local_css(&side.media_dir, h))
                {
                    author_css.push(css);
                }
            }
            _ => {}
        }
    }
    let env = MediaEnv {
        width_px: width as f32 * cell_px.0 as f32,
        height_px: opts.viewport_rows.max(1) as f32 * cell_px.1 as f32,
        dark: opts.night_mode,
    };
    let mut sheet = load_sheet(&author_css, &env);
    if !sheet.imports.is_empty() {
        let mut extra: Vec<String> = Vec::new();
        for imp in sheet.imports.iter().take(8) {
            if let Some(css) = read_local_css(&side.media_dir, imp) {
                extra.push(css);
            }
        }
        if !extra.is_empty() {
            let mut all = extra;
            all.extend(author_css.iter().cloned());
            sheet = load_sheet(&all, &env);
        }
    }

    let canvas = opts.bg.unwrap_or(if opts.night_mode {
        DARK_CANVAS
    } else {
        LIGHT_CANVAS
    });
    let default_fg = opts.fg.unwrap_or(if opts.night_mode {
        Rgb(0xdd, 0xdd, 0xdd)
    } else {
        Rgb(0x20, 0x20, 0x20)
    });
    let styler = Styler {
        sheet,
        cell_px: (cell_px.0 as f32, cell_px.1 as f32),
        canvas,
        base_font_px: 16.0,
    };
    let mut w = Walker {
        opts,
        styler,
        media_dir: &side.media_dir,
        cell_px,
        default_fg,
        widgets: Vec::new(),
        images: Vec::new(),
        notices: Vec::new(),
        hint_anchors: HashMap::new(),
        hint_targets: HashMap::new(),
        details_keys: HashMap::new(),
        cur_widget: None,
        cur_link: None,
        depth: 0,
        table_depth: 0,
        image_occlusion: false,
        occlusion: None,
    };
    w.prepass(&dom);

    let html_el = dom.root_element();
    let avail_px = env.width_px;
    let root = Computed::root(16.0);
    let mut html_st = w.styler.compute(html_el, &root, avail_px);
    if html_st.display == Display::None {
        return RenderedCard::default();
    }
    w.styler.base_font_px = html_st.font_px;
    let Some(body) = html_el
        .children()
        .filter_map(ElementRef::wrap)
        .find(|e| e.value().name() == "body")
    else {
        return RenderedCard::default();
    };
    html_st.bg = None;
    let mut body_st = w.styler.compute(body, &html_st, avail_px);
    if body_st.display == Display::None {
        return RenderedCard::default();
    }
    w.styler.base_font_px = body_st.font_px;
    // The page canvas is the terminal's own background (R3 §8.1 step 8).
    body_st.bg = None;
    body_st.margin_top = 0;
    body_st.margin_bottom = 0;
    body_st.indent = 0;
    body_st.display = Display::Block;

    let mut out = Out::default();
    let frame = Frame { col: 0, width };
    w.block_content(body, &body_st, &mut out, frame);

    w.finish(out, width)
}

impl Walker<'_> {
    fn notice(&mut self, s: &str) {
        if !self.notices.iter().any(|n| n == s) {
            self.notices.push(s.to_string());
        }
    }

    /// Finds hint markup, `<details>` and things the text tier cannot show.
    fn prepass(&mut self, dom: &Html) {
        let mut hint_n = 0;
        let mut details_n = 0;
        let mut scripts = Vec::new();
        for node in dom.tree.root().descendants() {
            let Some(el) = ElementRef::wrap(node) else {
                continue;
            };
            let e = el.value();
            match e.name() {
                "a" if attr_has_word(&el, "class", "hint") => {
                    let target = e.attr("onclick").and_then(|oc| {
                        let i = oc.find("getElementById(")? + "getElementById(".len();
                        let rest = &oc[i..];
                        let q = rest.chars().next()?;
                        if q != '\'' && q != '"' {
                            return None;
                        }
                        let end = rest[1..].find(q)?;
                        Some(rest[1..1 + end].to_string())
                    });
                    let key = format!("hint:{hint_n}");
                    hint_n += 1;
                    let expanded = self.opts.expanded.contains(&key);
                    let label = el
                        .text()
                        .collect::<String>()
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ");
                    if let Some(t) = target {
                        self.hint_targets.insert(t, expanded);
                    }
                    self.hint_anchors.insert(
                        node.id(),
                        HintAnchor {
                            key,
                            label,
                            expanded,
                        },
                    );
                }
                "details" => {
                    self.details_keys
                        .insert(node.id(), format!("details:{details_n}"));
                    details_n += 1;
                }
                "script" => {
                    let t: String = el.text().collect();
                    if !t.trim().is_empty() || e.attr("src").is_some() {
                        scripts.push(t);
                    }
                }
                "iframe" | "object" | "embed" => {
                    self.notice("card embeds external content (iframe/object) that is not shown")
                }
                "video" | "audio" => self.notice("card has HTML audio/video that is not played"),
                "button" | "select" | "textarea" => {
                    self.notice("card has buttons or inputs that need JavaScript")
                }
                "input" if e.id() != Some("typeans") => {
                    self.notice("card has buttons or inputs that need JavaScript")
                }
                _ => {}
            }
            if e.id() == Some("image-occlusion-container") {
                self.image_occlusion = true;
            }
        }
        if self.image_occlusion {
            self.notices.retain(|n| !n.starts_with("card has buttons"));
            self.occlusion = crate::occlusion::Occlusion::parse(dom, &self.styler.sheet);
            if self.occlusion.as_ref().is_some_and(|io| io.toggle) {
                self.notice("image occlusion: M toggles masks");
            }
            if !self.opts.images {
                self.notice("image occlusion requires graphics (try --graphics=blocks)");
            }
            scripts.retain(|s| !crate::tier::native_io_script(s));
        } else if dom
            .tree
            .root()
            .descendants()
            .any(|n| ElementRef::wrap(n).is_some_and(|e| e.value().name() == "canvas"))
        {
            self.notice("card draws on a canvas, which is not shown");
        }
        if !scripts.is_empty() {
            self.notice("card uses JavaScript, which is not run");
        }
    }

    fn compute(&self, el: ElementRef, parent: &Computed, frame: Frame) -> Computed {
        let mut st = self
            .styler
            .compute(el, parent, frame.width as f32 * self.cell_px.0 as f32);
        if let Some(id) = el.value().id() {
            if let Some(&expanded) = self.hint_targets.get(id) {
                st.display = if expanded {
                    Display::Block
                } else {
                    Display::None
                };
            }
        }
        st
    }

    fn meta(&self, st: &Computed) -> Meta {
        let fg = match st.color {
            Fg::Rgb(c) => Some(ensure_contrast(c, st.bg.unwrap_or(self.styler.canvas))),
            Fg::Default | Fg::Transparent => match st.bg {
                // Default text on an author background must stay readable.
                Some(bg) if text::contrast(self.default_fg, bg) < text::MIN_CONTRAST => {
                    Some(ensure_contrast(self.default_fg, bg))
                }
                _ => None,
            },
        };
        Meta {
            style: Style {
                text_scale: if st.font_px < self.styler.base_font_px * 0.95 {
                    ((st.font_px / self.styler.base_font_px * 100.0).round() as u8).clamp(10, 90)
                } else {
                    0
                },
                fg,
                bg: st.bg,
                bold: st.bold,
                italic: st.italic,
                underline: st.underline,
                strikethrough: st.strike,
                dim: st.dim,
            },
            link: self.cur_link.clone(),
            widget: self.cur_widget,
            image: None,
        }
    }

    fn tstyle(&self, st: &Computed) -> TStyle {
        TStyle {
            meta: self.meta(st),
            ws: st.white_space,
        }
    }

    fn add_widget(&mut self, key: String, kind: WidgetKind) -> usize {
        self.widgets.push(WidgetRec { key, kind });
        self.widgets.len() - 1
    }

    fn new_para(st: &Computed) -> Para {
        Para::new(st.align, st.rtl, st.dir_auto)
    }

    fn flush(&mut self, para: &mut Para, out: &mut Out, frame: Frame) {
        if para.is_empty() {
            *para = Para::new(para.align, para.rtl, para.dir_auto);
            return;
        }
        for row in para.layout(frame.width) {
            out.emit(row, frame.col);
        }
        *para = Para::new(para.align, para.rtl, para.dir_auto);
    }

    fn inner_frame(frame: Frame, indent: usize) -> Frame {
        let keep = frame.width.min(8);
        let indent = indent.min(frame.width.saturating_sub(keep));
        Frame {
            col: frame.col + indent,
            width: frame.width - indent,
        }
    }

    /// A block-level element: margins, indent, then its special layout or flow.
    fn block(
        &mut self,
        el: ElementRef,
        st: &Computed,
        out: &mut Out,
        frame: Frame,
        parent_align: Align,
    ) {
        if st.fancy_layout {
            self.notice("card uses CSS layout tanki cannot show (flex, grid or positioning)");
        }
        let tag = el.value().name();
        out.margin(st.margin_top);
        let mut indent = st.indent;
        if matches!(tag, "ul" | "ol" | "menu" | "dir") {
            indent += list_indent(el, st);
        }
        let inner = Self::inner_frame(frame, indent);
        match (tag, st.display) {
            ("hr", _) => {
                let meta = Meta {
                    style: Style {
                        dim: true,
                        ..Style::default()
                    },
                    ..Meta::default()
                };
                out.emit(
                    vec![ISpan {
                        text: "─".repeat(inner.width),
                        meta,
                    }],
                    inner.col,
                );
                if el.value().id() == Some("answer") && out.answer_row.is_none() {
                    out.answer_row = Some(out.rows.len() - 1);
                }
            }
            ("table", Display::Table) => {
                if self.table_depth >= MAX_TABLE_DEPTH {
                    self.table_linear(el, st, out, inner);
                } else {
                    self.table_depth += 1;
                    self.table(el, st, out, inner, parent_align);
                    self.table_depth -= 1;
                }
            }
            ("details", _) => self.details(el, st, out, inner),
            (_, Display::ListItem) => {
                let marker = list_marker(el, st);
                let mut row = IRow::new();
                if !marker.is_empty() {
                    let mut m = self.meta(st);
                    m.style.underline = false;
                    m.style.strikethrough = false;
                    push_span(&mut row, &marker, &m);
                }
                out.marker = Some((row, inner.col));
                self.block_content(el, st, out, inner);
                if out.marker.is_some() {
                    out.emit(IRow::new(), inner.col);
                }
            }
            _ => self.block_content(el, st, out, inner),
        }
        out.margin(st.margin_bottom);
    }

    /// Lays out the children of a block in `frame`.
    fn block_content(&mut self, el: ElementRef, st: &Computed, out: &mut Out, frame: Frame) {
        let mut para = Self::new_para(st);
        self.flow(el, st, out, frame, &mut para);
        self.flush(&mut para, out, frame);
    }

    fn flow(
        &mut self,
        parent: ElementRef,
        pst: &Computed,
        out: &mut Out,
        frame: Frame,
        para: &mut Para,
    ) {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            let t: String = parent.text().collect();
            self.text(&t, pst, para, frame);
            self.depth -= 1;
            return;
        }
        for child in parent.children() {
            match child.value() {
                Node::Text(t) => self.text(t, pst, para, frame),
                Node::Element(_) => {
                    let Some(el) = ElementRef::wrap(child) else {
                        continue;
                    };
                    self.element(el, pst, out, frame, para);
                }
                _ => {}
            }
        }
        self.depth -= 1;
    }

    fn element(
        &mut self,
        el: ElementRef,
        pst: &Computed,
        out: &mut Out,
        frame: Frame,
        para: &mut Para,
    ) {
        let st = self.compute(el, pst, frame);
        if st.display == Display::None || st.display == Display::TableColumn {
            return;
        }
        let tag = el.value().name();
        match tag {
            "script" | "style" | "canvas" | "iframe" | "object" | "embed" | "video" | "audio"
            | "button" | "select" | "textarea" | "noscript" | "template" | "head" => return,
            "input" => {
                if el.value().id() == Some("typeans") {
                    self.flush(para, out, frame);
                    let idx = self.add_widget("typeans".into(), WidgetKind::TypeAnswer);
                    let meta = Meta {
                        widget: Some(idx),
                        ..Meta::default()
                    };
                    out.emit(
                        vec![ISpan {
                            text: spaces(frame.width),
                            meta,
                        }],
                        frame.col,
                    );
                }
                return;
            }
            "br" => {
                para.push_break();
                return;
            }
            "wbr" => {
                para.push_wbr(&self.tstyle(&st));
                return;
            }
            _ => {}
        }
        match st.display {
            Display::Block
            | Display::ListItem
            | Display::Table
            | Display::TableRow
            | Display::TableCell
            | Display::TableRowGroup
            | Display::TableCaption => {
                self.flush(para, out, frame);
                // Replaced elements still draw their image when author CSS
                // promotes them to blocks; walking their empty children would
                // otherwise silently discard them.
                if matches!(tag, "img" | "svg") {
                    out.margin(st.margin_top);
                    let mut image_para = Self::new_para(&st);
                    if tag == "img" {
                        self.image(el, &st, &mut image_para, frame);
                    } else {
                        self.inline_svg(el, &st, &mut image_para, frame);
                    }
                    self.flush(&mut image_para, out, frame);
                    out.margin(st.margin_bottom);
                    return;
                }
                self.block(el, &st, out, frame, pst.align);
                return;
            }
            _ => {}
        }
        if st.fancy_layout {
            self.notice("card uses CSS layout tanki cannot show (flex, grid or positioning)");
        }
        // Inline-level.
        match tag {
            "img" => self.image(el, &st, para, frame),
            "svg" => self.inline_svg(el, &st, para, frame),
            "ruby" => self.ruby(el, &st, para, frame),
            "math" => {
                // MathML: show its text, dimmed.
                let mut s = st.clone();
                s.dim = true;
                let t: String = el.text().collect();
                self.text(&t, &s, para, frame);
            }
            "a" if self.hint_anchors.contains_key(&el.id()) => self.hint(el, &st, para),
            _ => {
                if st.valign != VAlign::Baseline && matches!(st.display, Display::Inline) {
                    self.script(el, &st, out, frame, para);
                    return;
                }
                let saved_link = self.cur_link.clone();
                if tag == "a" {
                    if let Some(href) = el.value().attr("href") {
                        let h = href.trim();
                        if !h.is_empty()
                            && !h.starts_with('#')
                            && !h.to_ascii_lowercase().starts_with("javascript:")
                        {
                            self.cur_link = Some(Rc::from(h));
                        }
                    }
                }
                self.flow(el, &st, out, frame, para);
                self.cur_link = saved_link;
            }
        }
    }

    /// Text node: audio references become replay widgets; MathJax is dimmed.
    fn text(&mut self, t: &str, st: &Computed, para: &mut Para, frame: Frame) {
        if st.hidden || st.color == Fg::Transparent {
            para.push_hidden(t, st.white_space);
            return;
        }
        let mut rest = t;
        while let Some(pos) = rest.find("[anki:play:") {
            self.text_math(&rest[..pos], st, para, frame);
            let after = &rest[pos + "[anki:play:".len()..];
            let parsed = (|| {
                let mut chars = after.char_indices();
                let (_, side) = chars.next()?;
                let side = match side {
                    'q' => AudioSide::Question,
                    'a' => AudioSide::Answer,
                    _ => return None,
                };
                if after.get(1..2)? != ":" {
                    return None;
                }
                let digits_end = after[2..].find(|c: char| !c.is_ascii_digit())? + 2;
                if digits_end == 2 || after.get(digits_end..digits_end + 1)? != "]" {
                    return None;
                }
                let index: usize = after[2..digits_end].parse().ok()?;
                Some((side, index, digits_end + 1))
            })();
            match parsed {
                Some((side, index, len)) => {
                    if !self.opts.hide_audio_buttons {
                        let s = if side == AudioSide::Question {
                            'q'
                        } else {
                            'a'
                        };
                        let idx = self.add_widget(
                            format!("play:{s}:{index}"),
                            WidgetKind::Replay { side, index },
                        );
                        let mut ts = self.tstyle(st);
                        ts.meta.widget = Some(idx);
                        ts.meta.style.underline = false;
                        ts.meta.style.strikethrough = false;
                        ts.meta.link = None;
                        para.push_text("▶", &ts, st.transform);
                    }
                    rest = &after[len..];
                }
                None => {
                    self.text_math("[anki:play:", st, para, frame);
                    rest = after;
                }
            }
        }
        self.text_math(rest, st, para, frame);
    }

    fn text_math(&mut self, t: &str, st: &Computed, para: &mut Para, frame: Frame) {
        let normal = self.tstyle(st);
        for segment in crate::math::segments(t) {
            match segment {
                crate::math::Segment::Text(t) => para.push_text(t, &normal, st.transform),
                crate::math::Segment::Formula { source, display } => {
                    if !display {
                        if let Some(text) = crate::math::unicode(source) {
                            para.push_text(&text, &normal, st.transform);
                            continue;
                        }
                    }
                    if self.opts.images {
                        let fg = normal.meta.style.fg.unwrap_or(self.default_fg);
                        match crate::math::svg(source, display, self.cell_px.1, fg) {
                            Ok(svg) => {
                                let dims = media::svg_dimensions(svg.as_bytes());
                                let mut math_style = st.clone();
                                math_style.width = None;
                                math_style.height = None;
                                if display {
                                    para.push_break();
                                }
                                let atom = self.image_atom(
                                    ImageSource::Svg(svg),
                                    dims,
                                    &math_style,
                                    frame,
                                    source.into(),
                                );
                                para.push_atom(atom);
                                if display {
                                    para.push_break();
                                }
                                continue;
                            }
                            Err(_) => self.notice("unsupported math; showing TeX source"),
                        }
                    } else {
                        self.notice("math needs graphics; showing TeX source");
                    }
                    let mut fallback = normal.clone();
                    fallback.meta.style.dim = true;
                    let text = if display {
                        format!("\\[{source}\\]")
                    } else {
                        format!("\\({source}\\)")
                    };
                    para.push_text(&text, &fallback, st.transform);
                }
            }
        }
    }

    fn placeholder(&mut self, label: &str, st: &Computed, para: &mut Para) {
        let mut ts = self.tstyle(st);
        ts.meta.style.dim = true;
        ts.ws = WhiteSpace::Normal;
        para.push_text(label, &ts, crate::style::Transform::None);
    }

    fn resolve_len(len: Option<Len>, avail_px: f32) -> Option<f32> {
        match len {
            Some(Len::Px(p)) => Some(p),
            Some(Len::Percent(p)) => Some(avail_px * p / 100.0),
            None => None,
        }
    }

    fn image_atom(
        &mut self,
        source: ImageSource,
        natural: Option<(u32, u32)>,
        st: &Computed,
        frame: Frame,
        alt: String,
    ) -> Atom {
        let avail_px = frame.width as f32 * self.cell_px.0 as f32;
        let req = SizeRequest {
            natural,
            width: Self::resolve_len(st.width, avail_px),
            height: Self::resolve_len(st.height, avail_px),
            max_width: Self::resolve_len(st.max_width, avail_px),
            max_height: Self::resolve_len(
                st.max_height,
                self.opts.viewport_rows as f32 * self.cell_px.1 as f32,
            ),
            cell_px: self.cell_px,
            max_cols: frame.width,
            viewport_rows: self.opts.viewport_rows,
        };
        let (w, h) = media::image_cells(&req);
        self.images.push(ImageRec {
            source,
            width: w,
            height: h,
            alt,
        });
        let meta = Meta {
            image: Some(self.images.len() - 1),
            link: self.cur_link.clone(),
            ..Meta::default()
        };
        Atom {
            width: w,
            rows: (0..h)
                .map(|_| {
                    vec![ISpan {
                        text: spaces(w),
                        meta: meta.clone(),
                    }]
                })
                .collect(),
        }
    }

    fn image(&mut self, el: ElementRef, st: &Computed, para: &mut Para, frame: Frame) {
        let e = el.value();
        let src = e.attr("src").unwrap_or("").to_string();
        let alt = e.attr("alt").unwrap_or("").trim().to_string();
        let latex = attr_has_word(&el, "class", "latex") && !alt.is_empty();
        let name = media::display_name(&src);
        let fallback = |w: &mut Self, label: String, para: &mut Para| {
            if latex {
                // A LaTeX image's alt text is its source: more useful than a label.
                let mut s = st.clone();
                s.dim = true;
                w.text(&alt, &s, para, frame);
            } else {
                w.placeholder(&label, st, para);
            }
        };
        if st.hidden {
            return;
        }
        match media::resolve(self.media_dir, &src) {
            MediaRef::Remote(_) => {
                self.notice("remote images are not loaded");
                fallback(self, format!("[remote image: {name}]"), para);
            }
            MediaRef::Rejected(_) => fallback(self, "[image]".to_string(), para),
            MediaRef::Local(path) => {
                if !self.opts.images {
                    fallback(self, format!("[image: {name}]"), para);
                    return;
                }
                match media::image_dimensions(&path) {
                    Err(_) => fallback(self, format!("[missing image: {name}]"), para),
                    Ok(dims) => {
                        let alt = if alt.is_empty() { name } else { alt };
                        let source = if el
                            .ancestors()
                            .filter_map(ElementRef::wrap)
                            .any(|e| e.value().id() == Some("image-occlusion-container"))
                        {
                            match (&self.occlusion, dims) {
                                (Some(io), Some((w, h))) => ImageSource::Occlusion {
                                    path,
                                    overlay: io.overlay(
                                        w,
                                        h,
                                        self.opts.expanded.contains("occlusion:masks-hidden"),
                                        frame.width as f32 * self.cell_px.0 as f32,
                                    ),
                                },
                                _ => ImageSource::File(path),
                            }
                        } else if latex && self.opts.night_mode {
                            ImageSource::InvertedFile(path)
                        } else {
                            ImageSource::File(path)
                        };
                        let atom = self.image_atom(source, dims, st, frame, alt);
                        para.push_atom(atom);
                    }
                }
            }
        }
    }

    fn inline_svg(&mut self, el: ElementRef, st: &Computed, para: &mut Para, frame: Frame) {
        if st.hidden {
            return;
        }
        if !self.opts.images {
            self.placeholder("[svg image]", st, para);
            return;
        }
        let mut svg = el.html();
        if !svg.contains("xmlns=") {
            svg = svg.replacen("<svg", "<svg xmlns=\"http://www.w3.org/2000/svg\"", 1);
        }
        match media::svg_dimensions(svg.as_bytes()) {
            Some(dims) => {
                let atom =
                    self.image_atom(ImageSource::Svg(svg), Some(dims), st, frame, "svg".into());
                para.push_atom(atom);
            }
            None => self.placeholder("[svg image]", st, para),
        }
    }

    /// Collects the visible text of `el` with styles (for ruby parts).
    fn runs(&mut self, el: ElementRef, st: &Computed, frame: Frame, out: &mut Vec<(String, Meta)>) {
        for child in el.children() {
            match child.value() {
                Node::Text(t) => {
                    if !st.hidden {
                        out.push((t.to_string(), self.meta(st)));
                    }
                }
                Node::Element(_) => {
                    let Some(c) = ElementRef::wrap(child) else {
                        continue;
                    };
                    let cst = self.compute(c, st, frame);
                    if cst.display != Display::None {
                        self.runs(c, &cst, frame, out);
                    }
                }
                _ => {}
            }
        }
    }

    fn ruby(&mut self, el: ElementRef, st: &Computed, para: &mut Para, frame: Frame) {
        type StyledRuns = Vec<(String, Meta)>;
        let mut pairs: Vec<(StyledRuns, StyledRuns)> = Vec::new();
        let mut base: Vec<(String, Meta)> = Vec::new();
        for child in el.children() {
            match child.value() {
                Node::Text(t) => base.push((t.to_string(), self.meta(st))),
                Node::Element(e) => {
                    let Some(c) = ElementRef::wrap(child) else {
                        continue;
                    };
                    match e.name() {
                        "rp" => {}
                        "rt" => {
                            let mut cst = self.compute(c, st, frame);
                            cst.display = Display::Inline;
                            let mut annot = Vec::new();
                            self.runs(c, &cst, frame, &mut annot);
                            pairs.push((std::mem::take(&mut base), annot));
                        }
                        _ => {
                            let cst = self.compute(c, st, frame);
                            if cst.display != Display::None {
                                self.runs(c, &cst, frame, &mut base);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if !base.is_empty() {
            pairs.push((base, Vec::new()));
        }
        let collapse = |runs: Vec<(String, Meta)>| -> Vec<(String, Meta)> {
            let mut out: Vec<(String, Meta)> = Vec::new();
            let mut last_space = true;
            for (t, m) in runs {
                let mut s = String::new();
                for c in t.chars() {
                    if c.is_whitespace() {
                        if !last_space {
                            s.push(' ');
                            last_space = true;
                        }
                    } else if !c.is_control() {
                        s.push(c);
                        last_space = false;
                    }
                }
                if !s.is_empty() {
                    out.push((s, m));
                }
            }
            if let Some(last) = out.last_mut() {
                let t = last.0.trim_end().len();
                last.0.truncate(t);
            }
            out.retain(|(s, _)| !s.is_empty());
            out
        };
        for (base, annot) in pairs {
            let base = collapse(base);
            let mut annot = collapse(annot);
            if self.opts.ruby == RubyMode::Above {
                for (_, meta) in &mut annot {
                    meta.style.text_scale = 50;
                }
            }
            let bw: usize = base.iter().map(|(s, _)| str_width(s)).sum();
            let aw: usize = annot.iter().map(|(s, _)| str_width(s)).sum();
            let w = bw.max(aw);
            let push_runs = |p: &mut Para, runs: &[(String, Meta)]| {
                for (s, m) in runs {
                    p.push_text(
                        s,
                        &TStyle {
                            meta: m.clone(),
                            ws: st.white_space,
                        },
                        st.transform,
                    );
                }
            };
            if annot.is_empty() {
                push_runs(para, &base);
            } else if self.opts.ruby == RubyMode::Inline || w > frame.width || w == 0 {
                push_runs(para, &base);
                let paren_meta = annot.first().map(|(_, m)| m.clone()).unwrap_or_default();
                let ps = TStyle {
                    meta: paren_meta,
                    ws: st.white_space,
                };
                para.push_text("(", &ps, st.transform);
                push_runs(para, &annot);
                para.push_text(")", &ps, st.transform);
            } else {
                let centred = |runs: &[(String, Meta)], rw: usize| -> IRow {
                    let mut row = IRow::new();
                    let left = (w - rw) / 2;
                    push_span(&mut row, &spaces(left), &Meta::default());
                    for (s, m) in runs {
                        push_span(&mut row, s, m);
                    }
                    push_span(&mut row, &spaces(w - rw - left), &Meta::default());
                    row
                };
                para.push_atom(Atom {
                    width: w,
                    rows: vec![centred(&annot, aw), centred(&base, bw)],
                });
            }
        }
    }

    /// `<sub>`/`<sup>`: Unicode forms when every character has one.
    fn script(
        &mut self,
        el: ElementRef,
        st: &Computed,
        out: &mut Out,
        frame: Frame,
        para: &mut Para,
    ) {
        let sup = st.valign == VAlign::Super;
        let raw: String = el.text().collect();
        let t = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        if t.is_empty() {
            self.flow(el, st, out, frame, para);
            return;
        }
        let simple = el.children().all(|c| matches!(c.value(), Node::Text(_)));
        if simple {
            if let Some(mapped) = map_script(&t, sup) {
                let ts = self.tstyle(st);
                para.push_text(&mapped, &ts, crate::style::Transform::None);
                return;
            }
        }
        let ts = self.tstyle(st);
        let multi = t.graphemes(true).count() > 1;
        let mark = if sup { "^" } else { "_" };
        let open = if multi {
            format!("{mark}(")
        } else {
            mark.to_string()
        };
        para.push_text(&open, &ts, crate::style::Transform::None);
        self.flow(el, st, out, frame, para);
        if multi {
            para.push_text(")", &ts, crate::style::Transform::None);
        }
    }

    fn hint(&mut self, el: ElementRef, st: &Computed, para: &mut Para) {
        let Some(h) = self.hint_anchors.get(&el.id()) else {
            return;
        };
        let (key, label, expanded) = (h.key.clone(), h.label.clone(), h.expanded);
        let idx = self.add_widget(
            key,
            WidgetKind::Hint {
                label: label.clone(),
                expanded,
            },
        );
        let mut ts = self.tstyle(st);
        ts.meta.widget = Some(idx);
        ts.meta.link = None;
        ts.ws = WhiteSpace::Normal;
        let arrow = if expanded { "▾" } else { "▸" };
        para.push_text(
            &format!("{arrow}\u{a0}{label}"),
            &ts,
            crate::style::Transform::None,
        );
    }

    fn details(&mut self, el: ElementRef, st: &Computed, out: &mut Out, frame: Frame) {
        let key = self
            .details_keys
            .get(&el.id())
            .cloned()
            .unwrap_or_else(|| "details:?".into());
        let expanded = self.opts.expanded.contains(&key) || el.value().attr("open").is_some();
        let summary_el = el
            .children()
            .filter_map(ElementRef::wrap)
            .find(|c| c.value().name() == "summary");
        let label = summary_el
            .map(|s| {
                s.text()
                    .collect::<String>()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Details".to_string());
        let idx = self.add_widget(
            key,
            WidgetKind::Details {
                summary: label.clone(),
                expanded,
            },
        );
        let saved = self.cur_widget.replace(idx);
        let mut para = Self::new_para(st);
        let arrow = if expanded { "▾\u{a0}" } else { "▸\u{a0}" };
        match summary_el {
            Some(s) => {
                let sst = self.compute(s, st, frame);
                let ts = self.tstyle(&sst);
                para.push_text(arrow, &ts, crate::style::Transform::None);
                self.flow(s, &sst, out, frame, &mut para);
            }
            None => {
                let ts = self.tstyle(st);
                para.push_text(
                    &format!("{arrow}{label}"),
                    &ts,
                    crate::style::Transform::None,
                );
            }
        }
        self.flush(&mut para, out, frame);
        self.cur_widget = saved;
        if expanded {
            let mut para = Self::new_para(st);
            for child in el.children() {
                match child.value() {
                    Node::Text(t) => self.text(t, st, &mut para, frame),
                    Node::Element(e) if e.name() != "summary" => {
                        if let Some(c) = ElementRef::wrap(child) {
                            self.element(c, st, out, frame, &mut para);
                        }
                    }
                    _ => {}
                }
            }
            self.flush(&mut para, out, frame);
        }
    }

    // ---- tables ----

    fn table_row<'d>(
        &self,
        tr: ElementRef<'d>,
        trst: &Computed,
        frame: Frame,
    ) -> Vec<(ElementRef<'d>, Computed)> {
        let mut cells = Vec::new();
        for c in tr.children().filter_map(ElementRef::wrap) {
            let cst = self.compute(c, trst, frame);
            if cst.display != Display::None && matches!(c.value().name(), "td" | "th") {
                cells.push((c, cst));
            }
        }
        cells
    }

    #[allow(clippy::type_complexity)]
    fn table_rows<'d>(
        &self,
        el: ElementRef<'d>,
        st: &Computed,
        frame: Frame,
    ) -> (
        Vec<Vec<(ElementRef<'d>, Computed)>>,
        Vec<(ElementRef<'d>, Computed)>,
    ) {
        let mut rows = Vec::new();
        let mut captions = Vec::new();
        for child in el.children().filter_map(ElementRef::wrap) {
            let cst = self.compute(child, st, frame);
            if cst.display == Display::None {
                continue;
            }
            match child.value().name() {
                "caption" => captions.push((child, cst)),
                "thead" | "tbody" | "tfoot" => {
                    for tr in child.children().filter_map(ElementRef::wrap) {
                        let trst = self.compute(tr, &cst, frame);
                        if trst.display != Display::None && tr.value().name() == "tr" {
                            rows.push(self.table_row(tr, &trst, frame));
                        }
                    }
                }
                "tr" => rows.push(self.table_row(child, &cst, frame)),
                _ => {}
            }
        }
        (rows, captions)
    }

    fn sub_layout(&mut self, el: ElementRef, st: &Computed, width: usize) -> Vec<IRow> {
        let mut sub = Out::default();
        self.block_content(el, st, &mut sub, Frame { col: 0, width });
        if sub.marker.is_some() {
            sub.emit(IRow::new(), 0);
        }
        sub.rows
    }

    fn measure(&mut self, el: ElementRef, st: &Computed, width: usize) -> usize {
        let saved = (self.widgets.len(), self.images.len());
        let rows = self.sub_layout(el, st, width);
        self.widgets.truncate(saved.0);
        self.images.truncate(saved.1);
        rows.iter().map(row_width).max().unwrap_or(0)
    }

    fn table_linear(&mut self, el: ElementRef, st: &Computed, out: &mut Out, frame: Frame) {
        let (rows, captions) = self.table_rows(el, st, frame);
        for (c, cst) in captions {
            self.block_content(c, &cst, out, frame);
        }
        for (i, row) in rows.iter().enumerate() {
            if i > 0 {
                let meta = Meta {
                    style: Style {
                        dim: true,
                        ..Style::default()
                    },
                    ..Meta::default()
                };
                out.emit(
                    vec![ISpan {
                        text: "┄".repeat(frame.width.min(20)),
                        meta,
                    }],
                    frame.col,
                );
            }
            for (c, cst) in row {
                self.block_content(*c, cst, out, frame);
            }
        }
    }

    fn table(
        &mut self,
        el: ElementRef,
        st: &Computed,
        out: &mut Out,
        frame: Frame,
        parent_align: Align,
    ) {
        let (rows, captions) = self.table_rows(el, st, frame);
        for (c, cst) in &captions {
            self.block_content(*c, cst, out, frame);
        }
        // Place cells on a grid, honouring colspan and rowspan.
        struct Cell<'b> {
            el: ElementRef<'b>,
            st: Computed,
            row: usize,
            col: usize,
            colspan: usize,
            rowspan: usize,
        }
        let nrows = rows.len();
        let span_attr = |e: &ElementRef, a: &str| -> usize {
            e.value()
                .attr(a)
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(1)
        };
        let mut occupied: Vec<Vec<bool>> = vec![Vec::new(); nrows];
        let mut cells: Vec<Cell> = Vec::new();
        for (r, row) in rows.into_iter().enumerate() {
            let mut c = 0;
            for (el, st) in row {
                while occupied[r].get(c).copied().unwrap_or(false) {
                    c += 1;
                }
                let colspan = span_attr(&el, "colspan").clamp(1, 50);
                let mut rowspan = span_attr(&el, "rowspan").min(50);
                if rowspan == 0 {
                    rowspan = nrows - r;
                }
                let rowspan = rowspan.clamp(1, nrows - r);
                for occ in occupied.iter_mut().skip(r).take(rowspan) {
                    if occ.len() < c + colspan {
                        occ.resize(c + colspan, false);
                    }
                    for o in occ.iter_mut().skip(c).take(colspan) {
                        *o = true;
                    }
                }
                cells.push(Cell {
                    el,
                    st,
                    row: r,
                    col: c,
                    colspan,
                    rowspan,
                });
                c += colspan;
            }
        }
        let ncols = occupied.iter().map(|o| o.len()).max().unwrap_or(0);
        if ncols == 0 || nrows == 0 {
            return;
        }
        let overhead = 3 * ncols + 1;
        if frame.width < overhead + ncols {
            self.table_linear(el, st, out, frame);
            return;
        }
        let avail = frame.width - overhead;
        // Column sizing from min/max content.
        let mut colmax = vec![0usize; ncols];
        let mut colmin = vec![1usize; ncols];
        let mut measured: Vec<(usize, usize)> = Vec::with_capacity(cells.len());
        for cell in &cells {
            let max_w = self.measure(cell.el, &cell.st, avail.max(1));
            let min_w = min_content(cell.el).min(max_w).max(1);
            measured.push((min_w, max_w));
        }
        for (cell, &(mn, mx)) in cells.iter().zip(&measured) {
            if cell.colspan == 1 {
                colmax[cell.col] = colmax[cell.col].max(mx);
                colmin[cell.col] = colmin[cell.col].max(mn);
            }
        }
        for (cell, &(mn, mx)) in cells.iter().zip(&measured) {
            if cell.colspan > 1 {
                let range = cell.col..cell.col + cell.colspan;
                let gaps = 3 * (cell.colspan - 1);
                for (vals, need) in [(&mut colmax, mx), (&mut colmin, mn)] {
                    let have: usize = vals[range.clone()].iter().sum::<usize>() + gaps;
                    if need > have {
                        let extra = need - have;
                        for (k, v) in vals[range.clone()].iter_mut().enumerate() {
                            *v += extra / cell.colspan + usize::from(k < extra % cell.colspan);
                        }
                    }
                }
            }
        }
        for c in 0..ncols {
            colmax[c] = colmax[c].max(colmin[c]).max(1);
        }
        let sum_max: usize = colmax.iter().sum();
        let sum_min: usize = colmin.iter().sum();
        let widths: Vec<usize> = if sum_max <= avail {
            colmax.clone()
        } else if sum_min <= avail {
            let extra = avail - sum_min;
            let flex: usize = colmax.iter().zip(&colmin).map(|(a, b)| a - b).sum();
            let mut w: Vec<usize> = colmin
                .iter()
                .zip(&colmax)
                .map(|(mn, mx)| mn + ((mx - mn) * extra).checked_div(flex).unwrap_or(0))
                .collect();
            let mut left = avail - w.iter().sum::<usize>();
            for (i, wi) in w.iter_mut().enumerate() {
                if left == 0 {
                    break;
                }
                if *wi < colmax[i] {
                    *wi += 1;
                    left -= 1;
                }
            }
            w
        } else {
            // Even min-content does not fit: share the space by min-content.
            let mut w: Vec<usize> = colmin
                .iter()
                .map(|mn| (mn * avail / sum_min.max(1)).max(1))
                .collect();
            while w.iter().sum::<usize>() > avail {
                if let Some(m) = w.iter_mut().max() {
                    *m -= 1;
                }
            }
            w
        };
        let mut xs = vec![0usize; ncols + 1];
        for c in 0..ncols {
            xs[c + 1] = xs[c] + widths[c] + 3;
        }
        // Lay out each cell at its final width.
        let mut contents: Vec<Vec<IRow>> = Vec::with_capacity(cells.len());
        for cell in &cells {
            let w = xs[cell.col + cell.colspan] - xs[cell.col] - 3;
            contents.push(self.sub_layout(cell.el, &cell.st, w));
        }
        let mut heights = vec![1usize; nrows];
        for (cell, rows) in cells.iter().zip(&contents) {
            if cell.rowspan == 1 {
                heights[cell.row] = heights[cell.row].max(rows.len());
            }
        }
        for (cell, rows) in cells.iter().zip(&contents) {
            if cell.rowspan > 1 {
                let have: usize = heights[cell.row..cell.row + cell.rowspan]
                    .iter()
                    .sum::<usize>()
                    + cell.rowspan
                    - 1;
                if rows.len() > have {
                    heights[cell.row + cell.rowspan - 1] += rows.len() - have;
                }
            }
        }
        let mut ys = vec![0usize; nrows + 1];
        for r in 0..nrows {
            ys[r + 1] = ys[r] + heights[r] + 1;
        }
        let total_w = xs[ncols] + 1;
        let total_h = ys[nrows] + 1;
        let mut canvas = Canvas::new(total_w, total_h);
        let mut rects: Vec<(usize, usize, usize, usize)> = Vec::new();
        for cell in &cells {
            rects.push((cell.row, cell.col, cell.rowspan, cell.colspan));
        }
        // Empty grid slots still get borders.
        for (r, occ) in occupied.iter().enumerate() {
            for c in 0..ncols {
                if !occ.get(c).copied().unwrap_or(false) {
                    rects.push((r, c, 1, 1));
                }
            }
        }
        for &(r, c, rs, cs) in &rects {
            let (x0, x1, y0, y1) = (xs[c], xs[c + cs], ys[r], ys[r + rs]);
            canvas.hline(y0, x0, x1);
            canvas.hline(y1, x0, x1);
            canvas.vline(x0, y0, y1);
            canvas.vline(x1, y0, y1);
        }
        for (cell, rows) in cells.iter().zip(contents) {
            let x = xs[cell.col] + 2;
            let y = ys[cell.row] + 1;
            let w = xs[cell.col + cell.colspan] - xs[cell.col] - 3;
            for (i, row) in rows.into_iter().enumerate() {
                canvas.blit(x, y + i, w, row);
            }
        }
        let table_rows = canvas.into_rows();
        let free = frame.width.saturating_sub(total_w);
        let centred = parent_align == Align::WebkitCenter
            || el
                .value()
                .attr("align")
                .is_some_and(|a| a.eq_ignore_ascii_case("center"));
        let pad = if centred {
            free / 2
        } else if el
            .value()
            .attr("align")
            .is_some_and(|a| a.eq_ignore_ascii_case("right"))
            || st.rtl
        {
            free
        } else {
            0
        };
        for row in table_rows {
            out.emit(row, frame.col + pad);
        }
    }

    // ---- assembly ----

    fn finish(self, out: Out, width: usize) -> RenderedCard {
        let mut lines: Vec<Line> = Vec::with_capacity(out.rows.len());
        let mut widget_pos: Vec<Option<(usize, usize, usize)>> = vec![None; self.widgets.len()];
        let mut image_pos: Vec<Option<(usize, usize)>> = vec![None; self.images.len()];
        for (r, row) in out.rows.into_iter().enumerate() {
            let mut line = Line::default();
            let mut col = 0usize;
            for s in row {
                let text: String = s
                    .text
                    .chars()
                    .filter(|c| !matches!(c, '\u{200B}' | '\u{AD}'))
                    .map(|c| if c == '\u{a0}' { ' ' } else { c })
                    .collect();
                // Never exceed the width.
                let mut kept = String::new();
                let mut w = 0;
                for g in text.graphemes(true) {
                    let gw = text::grapheme_width(g);
                    if col + w + gw > width {
                        break;
                    }
                    kept.push_str(g);
                    w += gw;
                }
                if kept.is_empty() {
                    if text.is_empty() {
                        continue;
                    }
                    break;
                }
                if let Some(wi) = s.meta.widget {
                    match &mut widget_pos[wi] {
                        None => widget_pos[wi] = Some((r, col, w)),
                        Some((wr, wc, ww)) if *wr == r && *wc + *ww == col => *ww += w,
                        _ => {}
                    }
                }
                if let Some(ii) = s.meta.image {
                    if image_pos[ii].is_none() {
                        image_pos[ii] = Some((r, col));
                    }
                }
                let span = Span {
                    text: kept,
                    style: s.meta.style,
                    link: s.meta.link.as_ref().map(|l| l.to_string()),
                    widget: s.meta.widget,
                };
                col += w;
                match line.spans.last_mut() {
                    Some(last)
                        if last.style == span.style
                            && last.link == span.link
                            && last.widget == span.widget =>
                    {
                        last.text.push_str(&span.text)
                    }
                    _ => line.spans.push(span),
                }
                if col >= width {
                    break;
                }
            }
            // Trailing plain spaces carry nothing.
            while let Some(last) = line.spans.last_mut() {
                if last.style != Style::default() || last.link.is_some() || last.widget.is_some() {
                    break;
                }
                let t = last.text.trim_end_matches(' ').len();
                last.text.truncate(t);
                if last.text.is_empty() {
                    line.spans.pop();
                } else {
                    break;
                }
            }
            lines.push(line);
        }
        // Drop widgets whose cells were clipped away; renumber the rest.
        let mut remap = vec![None; self.widgets.len()];
        let mut widgets = Vec::new();
        for (i, (rec, pos)) in self.widgets.into_iter().zip(widget_pos).enumerate() {
            if let Some((row, col, w)) = pos {
                remap[i] = Some(widgets.len());
                widgets.push(Widget {
                    key: rec.key,
                    kind: rec.kind,
                    row,
                    col: col as u16,
                    width: w as u16,
                });
            }
        }
        for line in &mut lines {
            for s in &mut line.spans {
                s.widget = s.widget.and_then(|w| remap[w]);
            }
        }
        let images: Vec<ImageBlock> = self
            .images
            .into_iter()
            .zip(image_pos)
            .filter_map(|(rec, pos)| {
                let (row, col) = pos?;
                Some(ImageBlock {
                    source: rec.source,
                    row,
                    col: col as u16,
                    width: rec.width.min(width - col) as u16,
                    height: rec.height as u16,
                    alt: rec.alt,
                })
            })
            .collect();
        // Blank image cells are meaningful layout, including at the card's end.
        let reserved_rows = images
            .iter()
            .map(|image| image.row + image.height as usize)
            .max()
            .unwrap_or(0);
        let keep_rows = reserved_rows.max(out.answer_row.map_or(0, |a| a + 1));
        while lines.last().is_some_and(|l| l.spans.is_empty()) && lines.len() > keep_rows {
            lines.pop();
        }
        RenderedCard {
            lines,
            images,
            widgets,
            answer_row: out.answer_row,
            notices: self.notices,
        }
    }
}

/// Indent a list adds for its markers.
fn list_indent(el: ElementRef, st: &Computed) -> usize {
    let count = el
        .children()
        .filter_map(ElementRef::wrap)
        .filter(|c| c.value().name() == "li")
        .count();
    let start = el
        .value()
        .attr("start")
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(1);
    let last = (start + count as i64 - 1).max(1) as usize;
    let widest = marker_text(&st.list_style, last.max(1));
    str_width(&widest).max(2)
}

fn marker_text(ls: &ListStyle, n: usize) -> String {
    match ls {
        ListStyle::Disc => "• ".into(),
        ListStyle::Circle => "◦ ".into(),
        ListStyle::Square => "▪ ".into(),
        ListStyle::Decimal => format!("{n}. "),
        ListStyle::DecimalLeadingZero => format!("{n:02}. "),
        ListStyle::LowerAlpha => format!("{}. ", text::alpha(n, false)),
        ListStyle::UpperAlpha => format!("{}. ", text::alpha(n, true)),
        ListStyle::LowerRoman => format!("{}. ", text::roman(n, false)),
        ListStyle::UpperRoman => format!("{}. ", text::roman(n, true)),
        ListStyle::LowerGreek => format!("{}. ", text::greek(n)),
        ListStyle::None => String::new(),
        ListStyle::Text(s) => s.clone(),
    }
}

fn list_marker(el: ElementRef, st: &Computed) -> String {
    // Ordinal: the list's start, li `value` attributes, and preceding items.
    let mut n: i64 = 0;
    let mut base: Option<i64> = None;
    for sib in el.prev_siblings().filter_map(ElementRef::wrap) {
        if sib.value().name() != "li" || sib.value().attr("hidden").is_some() {
            continue;
        }
        n += 1;
        if let Some(v) = sib
            .value()
            .attr("value")
            .and_then(|v| v.trim().parse::<i64>().ok())
        {
            base = Some(v);
            break;
        }
    }
    let ordinal = if let Some(v) = el
        .value()
        .attr("value")
        .and_then(|v| v.trim().parse::<i64>().ok())
    {
        v
    } else if let Some(b) = base {
        b + n
    } else {
        let start = el
            .parent()
            .and_then(ElementRef::wrap)
            .and_then(|p| p.value().attr("start"))
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(1);
        start + n
    };
    marker_text(&st.list_style, ordinal.max(0) as usize)
}

/// The widest unbreakable piece of text in `el` (approximate min-content).
fn min_content(el: ElementRef) -> usize {
    let t: String = el.text().collect::<String>();
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut prev = 0;
    let mut best = 0;
    for (pos, _) in unicode_linebreak::linebreaks(&t) {
        best = best.max(str_width(t[prev..pos].trim_end()));
        prev = pos;
    }
    best
}

/// A character grid for drawing tables with box-drawing borders.
struct Canvas {
    w: usize,
    cells: Vec<Vec<CanvasCell>>,
}

#[derive(Clone)]
enum CanvasCell {
    Empty,
    Line(u8),
    Text(String, Meta),
    Cont,
}

const UP: u8 = 1;
const DOWN: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;

impl Canvas {
    fn new(w: usize, h: usize) -> Self {
        Canvas {
            w,
            cells: vec![vec![CanvasCell::Empty; w]; h],
        }
    }

    fn flag(&mut self, x: usize, y: usize, f: u8) {
        if let Some(c) = self.cells.get_mut(y).and_then(|r| r.get_mut(x)) {
            *c = match c {
                CanvasCell::Line(g) => CanvasCell::Line(*g | f),
                _ => CanvasCell::Line(f),
            };
        }
    }

    fn hline(&mut self, y: usize, x0: usize, x1: usize) {
        for x in x0..x1 {
            self.flag(x, y, RIGHT);
            self.flag(x + 1, y, LEFT);
        }
    }

    fn vline(&mut self, x: usize, y0: usize, y1: usize) {
        for y in y0..y1 {
            self.flag(x, y, DOWN);
            self.flag(x, y + 1, UP);
        }
    }

    fn blit(&mut self, x: usize, y: usize, w: usize, row: IRow) {
        let Some(line) = self.cells.get_mut(y) else {
            return;
        };
        let mut cx = x;
        for s in row {
            for g in s.text.graphemes(true) {
                let gw = text::grapheme_width(g);
                if gw == 0 {
                    continue;
                }
                if cx + gw > x + w || cx + gw > self.w {
                    return;
                }
                line[cx] = CanvasCell::Text(g.to_string(), s.meta.clone());
                if gw == 2 {
                    line[cx + 1] = CanvasCell::Cont;
                }
                cx += gw;
            }
        }
    }

    fn into_rows(self) -> Vec<IRow> {
        let blank = Meta::default();
        self.cells
            .into_iter()
            .map(|cells| {
                let mut row = IRow::new();
                for c in cells {
                    match c {
                        CanvasCell::Empty => push_span(&mut row, " ", &blank),
                        CanvasCell::Line(f) => push_span(&mut row, box_char(f), &blank),
                        CanvasCell::Text(g, m) => push_span(&mut row, &g, &m),
                        CanvasCell::Cont => {}
                    }
                }
                row
            })
            .collect()
    }
}

fn box_char(f: u8) -> &'static str {
    match f {
        x if x == UP | DOWN | LEFT | RIGHT => "┼",
        x if x == UP | DOWN | RIGHT => "├",
        x if x == UP | DOWN | LEFT => "┤",
        x if x == DOWN | LEFT | RIGHT => "┬",
        x if x == UP | LEFT | RIGHT => "┴",
        x if x == DOWN | RIGHT => "┌",
        x if x == DOWN | LEFT => "┐",
        x if x == UP | RIGHT => "└",
        x if x == UP | LEFT => "┘",
        x if x & (LEFT | RIGHT) != 0 && x & (UP | DOWN) == 0 => "─",
        x if x & (UP | DOWN) != 0 && x & (LEFT | RIGHT) == 0 => "│",
        _ => " ",
    }
}
