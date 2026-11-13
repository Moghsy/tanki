//! The cascade: matching rules, specificity, `!important`, inheritance,
//! inline `style=""` and presentational attributes, for the whitelist of
//! properties the text tier honours (PLAN 6.2, R3 §8.1 step 5).

use crate::css::{self, parse_color, parse_declarations, CssColor, Declaration, Origin, RightKey, StyleSheet};
use crate::Rgb;
use scraper::{CaseSensitivity, ElementRef};
use std::collections::HashMap;
use std::rc::Rc;

/// The built-in user-agent sheet plus the reviewer rules that matter in a
/// terminal (`ts/reviewer/reviewer.scss`, R3 §2.2).
pub const UA_CSS: &str = r#"
html, body, div, p, blockquote, ul, ol, dl, dt, dd, h1, h2, h3, h4, h5, h6,
pre, hr, address, article, aside, footer, header, main, nav, section, figure,
figcaption, form, fieldset, legend, center, details, summary, hgroup, menu,
dir, listing, xmp, plaintext, search, frameset, frame, optgroup { display: block }
li { display: list-item; text-align: start }
table { display: table }
caption { display: table-caption; text-align: center }
thead, tbody, tfoot { display: table-row-group }
tr { display: table-row }
td, th { display: table-cell }
col, colgroup { display: table-column }
head, style, script, title, meta, link, base, template, noscript, area, param,
datalist, noembed, noframes, source, track { display: none }
[hidden] { display: none }
b, strong { font-weight: bold }
i, em, cite, var, dfn, address { font-style: italic }
u, ins { text-decoration: underline }
s, strike, del { text-decoration: line-through }
a[href] { text-decoration: underline }
h1 { font-weight: bold; font-size: 2em; margin: 0.67em 0 }
h2 { font-weight: bold; font-size: 1.5em; margin: 0.83em 0 }
h3 { font-weight: bold; font-size: 1.17em; margin: 1em 0 }
h4 { font-weight: bold; margin: 1.33em 0 }
h5 { font-weight: bold; font-size: 0.83em; margin: 1.67em 0 }
h6 { font-weight: bold; font-size: 0.67em; margin: 2.33em 0 }
p, dl, figure { margin: 1em 0 }
blockquote { margin: 1em 40px }
dd { margin-left: 40px }
pre, listing, xmp, plaintext { white-space: pre; margin: 1em 0 }
center { text-align: -webkit-center }
th { font-weight: bold; text-align: -webkit-center }
sub { vertical-align: sub }
sup { vertical-align: super }
mark { background-color: yellow; color: black }
ul, menu, dir { list-style-type: disc }
ol { list-style-type: decimal }
ul ul, ol ul, ul menu, ol menu { list-style-type: circle }
ul ul ul, ul ol ul, ol ul ul, ol ol ul { list-style-type: square }
ul ul, ul ol, ol ul, ol ol { margin-top: 0; margin-bottom: 0 }
ul, ol { margin: 1em 0 }
li ul, li ol { margin: 0 }
nobr { white-space: nowrap }
textarea { white-space: pre-wrap }

hr { margin: 1em 0 }
body.nightMode { color: -tanki-default; background-color: -tanki-default }
pre { text-align: left }
code#typeans { white-space: pre-wrap }
.typeGood { background: #afa; color: black }
.typeBad { color: black; background: #faa }
.typeMissed { color: black; background: #ccc }
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    None,
    Inline,
    Block,
    ListItem,
    Table,
    TableRow,
    TableCell,
    TableRowGroup,
    TableCaption,
    TableColumn,
    Contents,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Start,
    End,
    Left,
    Right,
    Center,
    Justify,
    /// `<center>` / `align=center`: also centres block children such as tables.
    WebkitCenter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhiteSpace {
    Normal,
    NoWrap,
    Pre,
    PreWrap,
    PreLine,
}

impl WhiteSpace {
    pub fn collapses_spaces(self) -> bool {
        matches!(self, WhiteSpace::Normal | WhiteSpace::NoWrap | WhiteSpace::PreLine)
    }
    pub fn keeps_newlines(self) -> bool {
        matches!(self, WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::PreLine)
    }
    pub fn wraps(self) -> bool {
        !matches!(self, WhiteSpace::NoWrap | WhiteSpace::Pre)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListStyle {
    Disc,
    Circle,
    Square,
    Decimal,
    DecimalLeadingZero,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    UpperRoman,
    LowerGreek,
    None,
    Text(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VAlign {
    Baseline,
    Sub,
    Super,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transform {
    None,
    Upper,
    Lower,
    Capitalize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Len {
    Px(f32),
    Percent(f32),
}

/// Text colour after the cascade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fg {
    /// The terminal's default foreground (night-mode override, or unset).
    Default,
    Rgb(Rgb),
    /// `color: transparent`: the text is invisible.
    Transparent,
}

#[derive(Debug, Clone)]
pub struct Computed {
    pub display: Display,
    pub hidden: bool,
    pub color: Fg,
    /// Background behind this element's text: its own, or the nearest
    /// ancestor's below `<body>` (the page canvas is not painted).
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub dim: bool,
    pub align: Align,
    pub white_space: WhiteSpace,
    pub list_style: ListStyle,
    pub rtl: bool,
    pub dir_auto: bool,
    pub indent: usize,
    pub margin_top: usize,
    pub margin_bottom: usize,
    pub width: Option<Len>,
    pub height: Option<Len>,
    pub max_width: Option<Len>,
    pub max_height: Option<Len>,
    pub valign: VAlign,
    pub font_px: f32,
    pub transform: Transform,
    pub vars: Rc<HashMap<String, String>>,
    /// Uses layout the text tier ignores (flex, grid, absolute positioning).
    pub fancy_layout: bool,
}

impl Computed {
    pub fn root(font_px: f32) -> Self {
        Computed {
            display: Display::Block,
            hidden: false,
            color: Fg::Default,
            bg: None,
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            dim: false,
            align: Align::Start,
            white_space: WhiteSpace::Normal,
            list_style: ListStyle::Disc,
            rtl: false,
            dir_auto: false,
            indent: 0,
            margin_top: 0,
            margin_bottom: 0,
            width: None,
            height: None,
            max_width: None,
            max_height: None,
            valign: VAlign::Baseline,
            font_px,
            transform: Transform::None,
            vars: Rc::new(HashMap::new()),
            fancy_layout: false,
        }
    }

    /// Copies the inherited properties and resets the others.
    fn inherit(parent: &Computed) -> Self {
        Computed {
            display: Display::Inline,
            hidden: parent.hidden,
            color: parent.color,
            bg: parent.bg,
            bold: parent.bold,
            italic: parent.italic,
            underline: parent.underline,
            strike: parent.strike,
            dim: parent.dim,
            align: parent.align,
            white_space: parent.white_space,
            list_style: parent.list_style.clone(),
            rtl: parent.rtl,
            dir_auto: false,
            indent: 0,
            margin_top: 0,
            margin_bottom: 0,
            width: None,
            height: None,
            max_width: None,
            max_height: None,
            valign: VAlign::Baseline,
            font_px: parent.font_px,
            transform: parent.transform,
            vars: parent.vars.clone(),
            fancy_layout: false,
        }
    }
}

pub struct Styler {
    pub sheet: Rc<StyleSheet>,
    pub cell_px: (f32, f32),
    /// The colour assumed behind unpainted text.
    pub canvas: Rgb,
    /// Font size of `<body>`, for the heading hint.
    pub base_font_px: f32,
}

struct Applied<'a> {
    rank: u8,
    specificity: u32,
    order: u32,
    decl: &'a Declaration,
}

fn blend(fg: (u8, u8, u8), a: f32, bg: Rgb) -> Rgb {
    let m = |f: u8, b: u8| (f as f32 * a + b as f32 * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
    Rgb(m(fg.0, bg.0), m(fg.1, bg.1), m(fg.2, bg.2))
}

/// Replaces `var(--x, fallback)` references.
fn substitute_vars(value: &str, vars: &HashMap<String, String>, depth: u32) -> String {
    if depth > 8 || !value.contains("var(") {
        return value.to_string();
    }
    let mut out = String::new();
    let mut rest = value;
    while let Some(pos) = rest.find("var(") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 4..];
        // Find the matching close paren.
        let mut depth_p = 1;
        let mut end = after.len();
        for (i, c) in after.char_indices() {
            match c {
                '(' => depth_p += 1,
                ')' => {
                    depth_p -= 1;
                    if depth_p == 0 {
                        end = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let inner = &after[..end];
        let (name, fallback) = match inner.split_once(',') {
            Some((n, f)) => (n.trim(), Some(f.trim())),
            None => (inner.trim(), None),
        };
        if let Some(v) = vars.get(name) {
            out.push_str(&substitute_vars(v, vars, depth + 1));
        } else if let Some(f) = fallback {
            out.push_str(&substitute_vars(f, vars, depth + 1));
        }
        rest = if end < after.len() { &after[end + 1..] } else { "" };
    }
    out.push_str(rest);
    out
}

fn parse_len(v: &str, em_px: f32) -> Option<Len> {
    let v = v.trim();
    if let Some(p) = v.strip_suffix('%') {
        return p.trim().parse::<f32>().ok().filter(|x| x.is_finite()).map(Len::Percent);
    }
    css::parse_length_px(v, em_px).map(Len::Px)
}

fn parse_list_style(v: &str) -> Option<ListStyle> {
    let t = v.trim();
    if (t.starts_with('"') && t.ends_with('"') || t.starts_with('\'') && t.ends_with('\'')) && t.len() >= 2 {
        return Some(ListStyle::Text(t[1..t.len() - 1].to_string()));
    }
    Some(match t.to_ascii_lowercase().as_str() {
        "disc" => ListStyle::Disc,
        "circle" => ListStyle::Circle,
        "square" => ListStyle::Square,
        "decimal" => ListStyle::Decimal,
        "decimal-leading-zero" => ListStyle::DecimalLeadingZero,
        "lower-alpha" | "lower-latin" => ListStyle::LowerAlpha,
        "upper-alpha" | "upper-latin" => ListStyle::UpperAlpha,
        "lower-roman" => ListStyle::LowerRoman,
        "upper-roman" => ListStyle::UpperRoman,
        "lower-greek" => ListStyle::LowerGreek,
        "none" => ListStyle::None,
        _ => return None,
    })
}

fn font_size_keyword(v: &str, parent: f32) -> Option<f32> {
    Some(match v {
        "xx-small" => 9.0,
        "x-small" => 10.0,
        "small" => 13.0,
        "medium" => 16.0,
        "large" => 18.0,
        "x-large" => 24.0,
        "xx-large" => 32.0,
        "xxx-large" => 48.0,
        "larger" => parent * 1.2,
        "smaller" => parent / 1.2,
        _ => return None,
    })
}

impl Styler {
    fn h_cells(&self, len: Option<Len>, font_px: f32, avail_px: f32) -> usize {
        let _ = font_px;
        let px = match len {
            Some(Len::Px(p)) => p,
            Some(Len::Percent(p)) => avail_px * p / 100.0,
            None => 0.0,
        };
        if px <= 0.0 {
            0
        } else {
            (px / self.cell_px.0).round() as usize
        }
    }

    fn v_rows(&self, len: Option<Len>) -> usize {
        match len {
            Some(Len::Px(p)) if p > 0.0 => ((p / self.cell_px.1) + 0.25).floor().min(3.0) as usize,
            _ => 0,
        }
    }

    /// Computes the style of `el` given its parent's computed style.
    /// `avail_px` is the containing block width, for percentages.
    pub fn compute(&self, el: ElementRef, parent: &Computed, avail_px: f32) -> Computed {
        let e = el.value();
        let tag = e.name();
        let mut matched: Vec<Applied> = Vec::new();
        let id = e.id();
        for rule in &self.sheet.rules {
            let ok = match &rule.key {
                RightKey::Any => true,
                RightKey::Id(i) => id == Some(i.as_str()),
                RightKey::Class(c) => e.has_class(c, CaseSensitivity::CaseSensitive),
                RightKey::Tag(t) => tag.eq_ignore_ascii_case(t),
            };
            if !ok || !rule.selector.matches(&el) {
                continue;
            }
            for d in rule.declarations.iter() {
                let rank = match (d.important, rule.origin) {
                    (false, Origin::UserAgent) => 0,
                    (false, Origin::Author) => 1,
                    (true, Origin::Author) => 3,
                    (true, Origin::UserAgent) => 4,
                };
                matched.push(Applied {
                    rank,
                    specificity: rule.specificity,
                    order: rule.order,
                    decl: d,
                });
            }
        }
        let hints = presentational_hints(el);
        for d in &hints {
            matched.push(Applied {
                rank: 1,
                specificity: 0,
                order: 0,
                decl: d,
            });
        }
        let inline = e.attr("style").map(parse_declarations).unwrap_or_default();
        for d in &inline {
            matched.push(Applied {
                rank: if d.important { 3 } else { 2 },
                specificity: u32::MAX,
                order: u32::MAX,
                decl: d,
            });
        }
        matched.sort_by_key(|a| (a.rank, a.specificity, a.order));

        let mut c = Computed::inherit(parent);
        // Custom properties first, so var() sees the element's own values.
        if matched.iter().any(|a| a.decl.name.starts_with("--")) {
            let mut vars = (*parent.vars).clone();
            for a in &matched {
                if a.decl.name.starts_with("--") {
                    vars.insert(a.decl.name.clone(), a.decl.value.clone());
                }
            }
            c.vars = Rc::new(vars);
        }
        let mut own_underline = false;
        let mut own_strike = false;
        let mut own_bg: Option<CssColor> = None;
        let mut color: Option<CssColor> = None;
        let mut opacity: f32 = 1.0;
        let mut ml = None;
        let mut pl = None;
        let mut mt = None;
        let mut mb = None;
        let mut font_px = parent.font_px;

        for a in &matched {
            let name = a.decl.name.as_str();
            if name.starts_with("--") {
                continue;
            }
            let value = substitute_vars(&a.decl.value, &c.vars, 0);
            let v = value.trim();
            let lower = v.to_ascii_lowercase();
            let kw = lower.as_str();
            if kw == "inherit" || kw == "unset" {
                apply_inherit(name, &mut c, parent);
                if name == "color" {
                    color = None;
                }
                continue;
            }
            match name {
                "display" => {
                    c.display = match kw.split_whitespace().next().unwrap_or("") {
                        "none" => Display::None,
                        "block" | "flow-root" | "flow" => Display::Block,
                        "inline" | "inline-block" | "inline-table" | "ruby" | "ruby-base" => Display::Inline,
                        "list-item" => Display::ListItem,
                        "table" => Display::Table,
                        "table-row" => Display::TableRow,
                        "table-cell" => Display::TableCell,
                        "table-row-group" | "table-header-group" | "table-footer-group" => Display::TableRowGroup,
                        "table-caption" => Display::TableCaption,
                        "table-column" | "table-column-group" => Display::TableColumn,
                        "contents" => Display::Contents,
                        "flex" | "grid" | "-webkit-box" | "-webkit-flex" | "-ms-flexbox" => {
                            c.fancy_layout = true;
                            Display::Block
                        }
                        "inline-flex" | "inline-grid" | "-webkit-inline-box" => {
                            c.fancy_layout = true;
                            Display::Inline
                        }
                        "initial" => Display::Inline,
                        _ => c.display,
                    }
                }
                "visibility" => match kw {
                    "hidden" | "collapse" => c.hidden = true,
                    "visible" | "initial" => c.hidden = false,
                    _ => {}
                },
                "opacity" => {
                    if let Some(p) = kw.strip_suffix('%') {
                        opacity = p.parse::<f32>().map(|x| x / 100.0).unwrap_or(1.0);
                    } else {
                        opacity = kw.parse::<f32>().unwrap_or(1.0);
                    }
                }
                "color" => {
                    if kw == "initial" {
                        color = Some(CssColor::Default);
                    } else if let Some(col) = parse_color(v) {
                        color = Some(col);
                    }
                }
                "background-color" | "background" => {
                    if kw == "none" || kw == "initial" {
                        own_bg = Some(CssColor::Rgba(0, 0, 0, 0.0));
                    } else if name == "background-color" {
                        if let Some(col) = parse_color(v) {
                            own_bg = Some(col);
                        }
                    } else {
                        // Shorthand: pick the colour token, if any.
                        let mut found = None;
                        for tok in css::split_top_level(v, b' ') {
                            if let Some(col) = parse_color(tok) {
                                found = Some(col);
                            }
                        }
                        own_bg = Some(found.unwrap_or(CssColor::Rgba(0, 0, 0, 0.0)));
                    }
                }
                "font-weight" => {
                    c.bold = match kw {
                        "bold" | "bolder" => true,
                        "normal" | "lighter" | "initial" => false,
                        n => n.parse::<u32>().map(|w| w >= 600).unwrap_or(c.bold),
                    }
                }
                "font-style" => {
                    c.italic = kw.starts_with("italic") || kw.starts_with("oblique");
                }
                "font" => {
                    let words: Vec<&str> = kw.split_whitespace().collect();
                    c.bold = words.iter().any(|w| *w == "bold" || *w == "bolder" || w.parse::<u32>().is_ok_and(|n| n >= 600 && n <= 1000));
                    c.italic = words.iter().any(|w| *w == "italic" || *w == "oblique");
                    for w in &words {
                        let size = w.split('/').next().unwrap_or("");
                        if let Some(px) = font_size_keyword(size, parent.font_px)
                            .or_else(|| css::parse_length_px(size, parent.font_px).filter(|_| size.ends_with("px") || size.ends_with("em") || size.ends_with("pt")))
                        {
                            font_px = px;
                        }
                    }
                }
                "text-decoration" | "text-decoration-line" => {
                    own_underline = kw.contains("underline");
                    own_strike = kw.contains("line-through");
                }
                "text-align" => {
                    c.align = match kw {
                        "left" | "-webkit-left" => Align::Left,
                        "right" | "-webkit-right" => Align::Right,
                        "center" => Align::Center,
                        "-webkit-center" | "-moz-center" => Align::WebkitCenter,
                        "justify" => Align::Justify,
                        "start" | "initial" => Align::Start,
                        "end" => Align::End,
                        "match-parent" => parent.align,
                        _ => c.align,
                    }
                }
                "white-space" => {
                    c.white_space = match kw {
                        "normal" | "initial" => WhiteSpace::Normal,
                        "nowrap" => WhiteSpace::NoWrap,
                        "pre" => WhiteSpace::Pre,
                        "pre-wrap" | "break-spaces" => WhiteSpace::PreWrap,
                        "pre-line" => WhiteSpace::PreLine,
                        _ => c.white_space,
                    }
                }
                "list-style-type" => {
                    if let Some(ls) = parse_list_style(v) {
                        c.list_style = ls;
                    }
                }
                "list-style" => {
                    for tok in v.split_whitespace() {
                        if let Some(ls) = parse_list_style(tok) {
                            c.list_style = ls;
                        }
                    }
                }
                "direction" => match kw {
                    "rtl" => c.rtl = true,
                    "ltr" | "initial" => c.rtl = false,
                    _ => {}
                },
                "margin" | "padding" => {
                    let parts: Vec<&str> = v.split_whitespace().collect();
                    let (top, left, bottom) = match parts.len() {
                        1 => (parts[0], parts[0], parts[0]),
                        2 => (parts[0], parts[1], parts[0]),
                        3 => (parts[0], parts[1], parts[2]),
                        4 => (parts[0], parts[3], parts[2]),
                        _ => continue,
                    };
                    let l = parse_len(left, font_px);
                    if name == "margin" {
                        ml = Some(l);
                        mt = Some(parse_len(top, font_px));
                        mb = Some(parse_len(bottom, font_px));
                    } else {
                        pl = Some(l);
                    }
                }
                "margin-left" | "margin-inline-start" => ml = Some(parse_len(v, font_px)),
                "padding-left" | "padding-inline-start" => pl = Some(parse_len(v, font_px)),
                "margin-top" | "margin-block-start" => mt = Some(parse_len(v, font_px)),
                "margin-bottom" | "margin-block-end" => mb = Some(parse_len(v, font_px)),
                "width" => c.width = parse_len(v, font_px),
                "height" => c.height = parse_len(v, font_px),
                "max-width" => c.max_width = parse_len(v, font_px),
                "max-height" => c.max_height = parse_len(v, font_px),
                "vertical-align" => {
                    c.valign = match kw {
                        "sub" => VAlign::Sub,
                        "super" => VAlign::Super,
                        _ => VAlign::Baseline,
                    }
                }
                "font-size" => {
                    if let Some(px) = font_size_keyword(kw, parent.font_px) {
                        font_px = px;
                    } else {
                        match parse_len(v, parent.font_px) {
                            Some(Len::Px(px)) => font_px = px,
                            Some(Len::Percent(p)) => font_px = parent.font_px * p / 100.0,
                            None => {}
                        }
                    }
                }
                "text-transform" => {
                    c.transform = match kw {
                        "uppercase" => Transform::Upper,
                        "lowercase" => Transform::Lower,
                        "capitalize" => Transform::Capitalize,
                        "none" | "initial" => Transform::None,
                        _ => c.transform,
                    }
                }
                "position" => {
                    if kw == "absolute" || kw == "fixed" {
                        c.fancy_layout = true;
                    }
                }
                _ => {}
            }
        }

        if !font_px.is_finite() || font_px <= 0.0 {
            font_px = parent.font_px;
        }
        c.font_px = font_px.min(1000.0);
        c.underline = parent.underline || own_underline;
        c.strike = parent.strike || own_strike;
        if c.font_px >= self.base_font_px * 1.5 && !matches!(tag, "body" | "html") {
            // Font size is only a heading hint in a terminal.
            c.bold = true;
        }
        if opacity <= 0.05 {
            c.hidden = true;
        } else if opacity < 0.75 {
            c.dim = true;
        }

        // Background first: alpha colours blend over what is behind them.
        let behind = parent.bg.unwrap_or(self.canvas);
        match own_bg {
            Some(CssColor::Rgba(r, g, b, a)) if a > 0.02 => {
                c.bg = Some(blend((r, g, b), a.min(1.0), behind));
            }
            Some(CssColor::Rgba(..)) => {} // transparent: keep the parent's
            Some(CssColor::Default) => c.bg = None,
            Some(CssColor::Current) | None => {}
        }
        match color {
            Some(CssColor::Rgba(_, _, _, a)) if a <= 0.02 => c.color = Fg::Transparent,
            Some(CssColor::Rgba(r, g, b, a)) => {
                c.color = Fg::Rgb(blend((r, g, b), a.min(1.0), c.bg.unwrap_or(self.canvas)))
            }
            Some(CssColor::Default) => c.color = Fg::Default,
            Some(CssColor::Current) | None => {}
        }

        let indent_px_avail = avail_px;
        let ml_c = self.h_cells(ml.flatten(), font_px, indent_px_avail);
        let pl_c = self.h_cells(pl.flatten(), font_px, indent_px_avail);
        c.indent = ml_c + pl_c;
        c.margin_top = self.v_rows(mt.flatten());
        c.margin_bottom = self.v_rows(mb.flatten());

        if let Some(dir) = e.attr("dir") {
            match dir.trim().to_ascii_lowercase().as_str() {
                "auto" => c.dir_auto = true,
                _ => {}
            }
        }
        c
    }
}

fn apply_inherit(name: &str, c: &mut Computed, p: &Computed) {
    match name {
        "visibility" => c.hidden = p.hidden,
        "color" => c.color = p.color,
        "font-weight" => c.bold = p.bold,
        "font-style" => c.italic = p.italic,
        "text-align" => c.align = p.align,
        "white-space" => c.white_space = p.white_space,
        "list-style-type" | "list-style" => c.list_style = p.list_style.clone(),
        "direction" => c.rtl = p.rtl,
        "font-size" => c.font_px = p.font_px,
        "text-transform" => c.transform = p.transform,
        "display" => c.display = p.display,
        _ => {}
    }
}

fn decl(name: &str, value: impl Into<String>) -> Declaration {
    Declaration {
        name: name.to_string(),
        value: value.into(),
        important: false,
    }
}

/// HTML attributes that act as zero-specificity author styles.
fn presentational_hints(el: ElementRef) -> Vec<Declaration> {
    let e = el.value();
    let tag = e.name();
    let mut out = Vec::new();
    if tag == "font" {
        if let Some(c) = e.attr("color") {
            out.push(decl("color", c));
        }
    }
    if let Some(c) = e.attr("bgcolor") {
        out.push(decl("background-color", c));
    }
    if let Some(a) = e.attr("align") {
        if matches!(tag, "div" | "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "td" | "th" | "tr" | "tbody" | "thead" | "tfoot" | "caption") {
            let v = match a.trim().to_ascii_lowercase().as_str() {
                "center" | "middle" => "-webkit-center",
                "left" => "-webkit-left",
                "right" => "-webkit-right",
                "justify" => "justify",
                _ => "",
            };
            if !v.is_empty() {
                out.push(decl("text-align", v));
            }
        }
    }
    if let Some(d) = e.attr("dir") {
        match d.trim().to_ascii_lowercase().as_str() {
            "rtl" => out.push(decl("direction", "rtl")),
            "ltr" => out.push(decl("direction", "ltr")),
            _ => {}
        }
    }
    if matches!(tag, "img" | "svg" | "video" | "canvas" | "iframe") {
        for (attr, prop) in [("width", "width"), ("height", "height")] {
            if let Some(v) = e.attr(attr) {
                let v = v.trim();
                if v.ends_with('%') {
                    out.push(decl(prop, v));
                } else if let Ok(n) = v.trim_end_matches("px").parse::<f32>() {
                    out.push(decl(prop, format!("{n}px")));
                }
            }
        }
    }
    if matches!(tag, "td" | "th") && e.attr("nowrap").is_some() {
        out.push(decl("white-space", "nowrap"));
    }
    if matches!(tag, "ol" | "ul" | "li") {
        if let Some(t) = e.attr("type") {
            let v = match t.trim() {
                "1" => "decimal",
                "a" => "lower-alpha",
                "A" => "upper-alpha",
                "i" => "lower-roman",
                "I" => "upper-roman",
                other => match other.to_ascii_lowercase().as_str() {
                    "disc" => "disc",
                    "circle" => "circle",
                    "square" => "square",
                    _ => "",
                },
            };
            if !v.is_empty() {
                out.push(decl("list-style-type", v));
            }
        }
    }
    out
}
