//! A small, forgiving CSS parser: style sheets, declarations, media queries
//! and colours. Only what the text tier needs (PLAN 6.2); anything it does not
//! understand is skipped, never an error.

use scraper::Selector;

/// Where a rule came from, for the cascade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    UserAgent = 0,
    Author = 1,
}

#[derive(Debug, Clone)]
pub struct Declaration {
    /// Lower-cased property name (custom properties keep their case).
    pub name: String,
    pub value: String,
    pub important: bool,
}

/// What the rightmost compound selector requires, used to skip most rules
/// without running the full matcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RightKey {
    Id(String),
    Class(String),
    Tag(String),
    Any,
}

#[derive(Debug)]
pub struct Rule {
    pub selector: Selector,
    pub specificity: u32,
    pub key: RightKey,
    pub declarations: std::rc::Rc<Vec<Declaration>>,
    pub origin: Origin,
    /// Source order across all sheets.
    pub order: u32,
}

/// Facts needed to evaluate `@media` queries.
#[derive(Debug, Clone, Copy)]
pub struct MediaEnv {
    pub width_px: f32,
    pub height_px: f32,
    pub dark: bool,
}

#[derive(Debug, Default)]
pub struct StyleSheet {
    pub rules: Vec<Rule>,
    /// Local files named by `@import` (resolved by the caller).
    pub imports: Vec<String>,
}

/// Removes `/* … */` comments, keeping strings intact.
fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut chars = css.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            out.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                out.push(c);
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    out
}

/// Finds the index of the byte that ends a block opened just before `start`
/// (the matching `}`), honouring strings and nesting. Returns `s.len()` when
/// unbalanced.
fn block_end(s: &[u8], start: usize) -> usize {
    let mut depth = 1usize;
    let mut i = start;
    let mut quote: Option<u8> = None;
    while i < s.len() {
        let c = s[i];
        if let Some(q) = quote {
            if c == b'\\' {
                i += 1;
            } else if c == q {
                quote = None;
            }
        } else {
            match c {
                b'"' | b'\'' => quote = Some(c),
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return i;
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    s.len()
}

/// Splits on `sep` at nesting level zero (outside strings, parens, brackets).
pub fn split_top_level(s: &str, sep: u8) -> Vec<&str> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut last = 0;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if let Some(q) = quote {
            if c == b'\\' {
                i += 1;
            } else if c == q {
                quote = None;
            }
        } else {
            match c {
                b'"' | b'\'' => quote = Some(c),
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                _ if c == sep && depth <= 0 => {
                    out.push(&s[last..i]);
                    last = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    out.push(&s[last..]);
    out
}

/// Parses the contents of a declaration block or a `style` attribute.
pub fn parse_declarations(block: &str) -> Vec<Declaration> {
    let mut out = Vec::new();
    for part in split_top_level(block, b';') {
        let Some(colon) = part.find(':') else {
            continue;
        };
        let name = part[..colon].trim();
        if name.is_empty() || name.contains(|c: char| c.is_whitespace() || c == '{' || c == '}')
        {
            continue;
        }
        let mut value = part[colon + 1..].trim();
        let mut important = false;
        if let Some(pos) = value.rfind('!') {
            if value[pos + 1..].trim().eq_ignore_ascii_case("important") {
                important = true;
                value = value[..pos].trim();
            }
        }
        if value.is_empty() {
            continue;
        }
        let name = if name.starts_with("--") {
            name.to_string()
        } else {
            name.to_ascii_lowercase()
        };
        out.push(Declaration {
            name,
            value: value.to_string(),
            important,
        });
    }
    out
}

impl StyleSheet {
    /// Parses `css`, appending rules. `order` is the running source-order counter.
    pub fn parse_into(&mut self, css: &str, origin: Origin, env: &MediaEnv, order: &mut u32) {
        let css = strip_comments(css);
        self.parse_block(&css, origin, env, order, 0);
    }

    fn parse_block(&mut self, css: &str, origin: Origin, env: &MediaEnv, order: &mut u32, depth: u32) {
        if depth > 16 {
            return;
        }
        let b = css.as_bytes();
        let mut i = 0;
        while i < b.len() {
            // Read a prelude up to `{` or `;`.
            let start = i;
            let mut quote: Option<u8> = None;
            let mut paren = 0i32;
            while i < b.len() {
                let c = b[i];
                if let Some(q) = quote {
                    if c == b'\\' {
                        i += 1;
                    } else if c == q {
                        quote = None;
                    }
                } else {
                    match c {
                        b'"' | b'\'' => quote = Some(c),
                        b'(' => paren += 1,
                        b')' => paren -= 1,
                        b'{' if paren <= 0 => break,
                        b';' if paren <= 0 => break,
                        b'}' if paren <= 0 => break,
                        _ => {}
                    }
                }
                i += 1;
            }
            let prelude = css[start..i.min(b.len())].trim();
            if i >= b.len() {
                break;
            }
            match b[i] {
                b';' | b'}' => {
                    if let Some(rest) = prelude.strip_prefix('@') {
                        self.at_statement(rest);
                    }
                    i += 1;
                }
                _ => {
                    // b'{'
                    let body_start = i + 1;
                    let end = block_end(b, body_start);
                    let body = &css[body_start..end.min(css.len())];
                    i = end + 1;
                    if let Some(rest) = prelude.strip_prefix('@') {
                        let (name, cond) = rest
                            .split_once(|c: char| c.is_whitespace() || c == '(')
                            .map(|(n, _)| (n, &rest[n.len()..]))
                            .unwrap_or((rest, ""));
                        match name.to_ascii_lowercase().as_str() {
                            "media" => {
                                if media_matches(cond, env) {
                                    self.parse_block(body, origin, env, order, depth + 1);
                                }
                            }
                            "supports" | "layer" | "document" | "-moz-document" | "container"
                            | "scope" => {
                                self.parse_block(body, origin, env, order, depth + 1);
                            }
                            _ => {} // font-face, keyframes, page, …
                        }
                    } else if !prelude.is_empty() {
                        self.add_rule(prelude, body, origin, order);
                    }
                }
            }
        }
    }

    fn at_statement(&mut self, rest: &str) {
        let lower = rest.to_ascii_lowercase();
        if let Some(arg) = lower.strip_prefix("import") {
            let arg = rest[rest.len() - arg.len()..].trim();
            let url = if let Some(inner) = arg.strip_prefix("url(") {
                inner.split(')').next().unwrap_or("")
            } else {
                arg.split_whitespace().next().unwrap_or("")
            };
            let url = url.trim().trim_matches(|c| c == '"' || c == '\'');
            if !url.is_empty() {
                self.imports.push(url.to_string());
            }
        }
    }

    fn add_rule(&mut self, prelude: &str, body: &str, origin: Origin, order: &mut u32) {
        // Nested rules (CSS nesting) are not supported: keep only plain
        // declarations of the block.
        let flat: String;
        let body = if body.contains('{') {
            flat = remove_nested_blocks(body);
            &flat
        } else {
            body
        };
        let decls = parse_declarations(body);
        if decls.is_empty() {
            return;
        }
        let decls = std::rc::Rc::new(decls);
        for sel_text in split_top_level(prelude, b',') {
            let sel_text = sel_text.trim();
            if sel_text.is_empty() {
                continue;
            }
            let Ok(selector) = Selector::parse(sel_text) else {
                continue;
            };
            let (specificity, key) = analyze_selector(sel_text);
            *order += 1;
            self.rules.push(Rule {
                selector,
                specificity,
                key,
                declarations: decls.clone(),
                origin,
                order: *order,
            });
        }
    }
}

fn remove_nested_blocks(body: &str) -> String {
    let b = body.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    let mut seg_start = 0;
    while i < b.len() {
        if b[i] == b'{' {
            // Drop the nested rule's prelude (text since the last `;`) and block.
            let prefix = &body[seg_start..i];
            let keep_until = prefix.rfind(';').map(|p| seg_start + p + 1).unwrap_or(seg_start);
            out.push_str(&body[seg_start..keep_until]);
            let end = block_end(b, i + 1);
            i = end + 1;
            seg_start = i.min(b.len());
            continue;
        }
        i += 1;
    }
    if seg_start < body.len() {
        out.push_str(&body[seg_start..]);
    }
    out
}

/// Computes `(specificity, rightmost key)` for one complex selector.
/// Specificity is packed as `a << 20 | b << 10 | c`.
pub fn analyze_selector(sel: &str) -> (u32, RightKey) {
    let (a, b, c, key) = analyze_inner(sel);
    (pack(a, b, c), key)
}

fn pack(a: u32, b: u32, c: u32) -> u32 {
    (a.min(1023) << 20) | (b.min(1023) << 10) | c.min(1023)
}

fn ident_end(s: &[u8], mut i: usize) -> usize {
    while i < s.len() {
        let c = s[i];
        if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c >= 0x80 {
            i += 1;
        } else if c == b'\\' && i + 1 < s.len() {
            i += 2;
        } else {
            break;
        }
    }
    i
}

fn matching_close(s: &[u8], mut i: usize, open: u8, close: u8) -> usize {
    let mut depth = 0;
    let mut quote: Option<u8> = None;
    while i < s.len() {
        let c = s[i];
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
        } else if c == b'"' || c == b'\'' {
            quote = Some(c);
        } else if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return i;
            }
        }
        i += 1;
    }
    s.len()
}

fn analyze_inner(sel: &str) -> (u32, u32, u32, RightKey) {
    let s = sel.as_bytes();
    let (mut a, mut b, mut c) = (0u32, 0u32, 0u32);
    let mut key = RightKey::Any;
    let mut i = 0;
    while i < s.len() {
        match s[i] {
            b' ' | b'\t' | b'\n' | b'>' | b'+' | b'~' => {
                // A combinator starts a new compound; the key belongs to the
                // rightmost one.
                key = RightKey::Any;
                i += 1;
            }
            b'#' => {
                let e = ident_end(s, i + 1);
                a += 1;
                key = RightKey::Id(sel[i + 1..e].to_string());
                i = e.max(i + 1);
            }
            b'.' => {
                let e = ident_end(s, i + 1);
                b += 1;
                if !matches!(key, RightKey::Id(_)) {
                    key = RightKey::Class(sel[i + 1..e].to_string());
                }
                i = e.max(i + 1);
            }
            b'[' => {
                b += 1;
                i = matching_close(s, i, b'[', b']') + 1;
            }
            b':' => {
                let pseudo_element = s.get(i + 1) == Some(&b':');
                let start = if pseudo_element { i + 2 } else { i + 1 };
                let e = ident_end(s, start);
                let name = sel[start..e].to_ascii_lowercase();
                if s.get(e) == Some(&b'(') {
                    let close = matching_close(s, e, b'(', b')');
                    let inner = &sel[e + 1..close.min(sel.len())];
                    match name.as_str() {
                        "not" | "is" | "has" | "matches" | "-webkit-any" => {
                            let best = split_top_level(inner, b',')
                                .into_iter()
                                .map(|p| {
                                    let (a, b, c, _) = analyze_inner(p.trim());
                                    pack(a, b, c)
                                })
                                .max()
                                .unwrap_or(0);
                            a += best >> 20;
                            b += (best >> 10) & 1023;
                            c += best & 1023;
                        }
                        "where" => {}
                        _ => b += 1,
                    }
                    i = close + 1;
                } else {
                    if pseudo_element
                        || matches!(
                            name.as_str(),
                            "before" | "after" | "first-line" | "first-letter"
                        )
                    {
                        c += 1;
                    } else {
                        b += 1;
                    }
                    i = e.max(start);
                }
            }
            b'*' => i += 1,
            ch if ch.is_ascii_alphabetic() || ch == b'_' || ch >= 0x80 || ch == b'-' => {
                let e = ident_end(s, i);
                c += 1;
                if key == RightKey::Any {
                    key = RightKey::Tag(sel[i..e].to_ascii_lowercase());
                }
                i = e.max(i + 1);
            }
            _ => i += 1,
        }
    }
    (a, b, c, key)
}

/// Evaluates a media query list. Unknown features make a query false;
/// a list is true when any query is.
pub fn media_matches(cond: &str, env: &MediaEnv) -> bool {
    let cond = cond.trim();
    if cond.is_empty() {
        return true;
    }
    split_top_level(cond, b',')
        .into_iter()
        .any(|q| media_query_matches(q.trim(), env))
}

fn media_query_matches(q: &str, env: &MediaEnv) -> bool {
    let lower = q.to_ascii_lowercase();
    let mut negate = false;
    let mut rest = lower.as_str();
    if let Some(r) = rest.strip_prefix("not ") {
        negate = true;
        rest = r;
    } else if let Some(r) = rest.strip_prefix("only ") {
        rest = r;
    }
    let mut result = true;
    for part in rest.split(" and ") {
        let part = part.trim();
        let ok = if part.is_empty() {
            true
        } else if let Some(inner) = part.strip_prefix('(').and_then(|p| p.strip_suffix(')')) {
            media_feature(inner.trim(), env)
        } else {
            matches!(part, "all" | "screen")
        };
        result &= ok;
    }
    result != negate
}

fn media_feature(f: &str, env: &MediaEnv) -> bool {
    let (name, value) = match f.split_once(':') {
        Some((n, v)) => (n.trim(), v.trim()),
        None => (f, ""),
    };
    let px = |v: &str| -> Option<f32> { parse_length_px(v, 16.0) };
    match name {
        "prefers-color-scheme" => (value == "dark") == env.dark,
        "max-width" => px(value).is_some_and(|v| env.width_px <= v),
        "min-width" => px(value).is_some_and(|v| env.width_px >= v),
        "max-height" => px(value).is_some_and(|v| env.height_px <= v),
        "min-height" => px(value).is_some_and(|v| env.height_px >= v),
        "max-device-width" => px(value).is_some_and(|v| env.width_px <= v),
        "min-device-width" => px(value).is_some_and(|v| env.width_px >= v),
        "orientation" => (value == "landscape") == (env.width_px >= env.height_px),
        "hover" | "any-hover" => value == "hover",
        "pointer" | "any-pointer" => value == "fine",
        "prefers-reduced-motion" => value == "no-preference",
        "color" => true,
        _ => false,
    }
}

/// Parses a length into CSS pixels. `em_px` is the font size for `em`.
pub fn parse_length_px(v: &str, em_px: f32) -> Option<f32> {
    let v = v.trim().to_ascii_lowercase();
    if v == "0" {
        return Some(0.0);
    }
    let num_end = v
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .unwrap_or(v.len());
    let n: f32 = v[..num_end].parse().ok()?;
    if !n.is_finite() {
        return None;
    }
    let unit = v[num_end..].trim();
    let px = match unit {
        "px" | "" => n,
        "em" | "rem" => n * em_px,
        "pt" => n * 96.0 / 72.0,
        "pc" => n * 16.0,
        "in" => n * 96.0,
        "cm" => n * 96.0 / 2.54,
        "mm" => n * 96.0 / 25.4,
        "ex" | "ch" => n * em_px / 2.0,
        _ => return None,
    };
    Some(px)
}

/// A colour value. `Default` is tanki's "the terminal's own colour".
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CssColor {
    Default,
    Current,
    Rgba(u8, u8, u8, f32),
}

pub fn parse_color(v: &str) -> Option<CssColor> {
    let v = v.trim();
    let lower = v.to_ascii_lowercase();
    if lower == "-tanki-default" {
        return Some(CssColor::Default);
    }
    if lower == "currentcolor" {
        return Some(CssColor::Current);
    }
    if lower == "transparent" {
        return Some(CssColor::Rgba(0, 0, 0, 0.0));
    }
    if let Some(hex) = lower.strip_prefix('#') {
        let h = |s: &str| u8::from_str_radix(s, 16).ok();
        let d = |c: char| u8::from_str_radix(&c.to_string(), 16).ok().map(|x| x * 17);
        let cs: Vec<char> = hex.chars().collect();
        return match cs.len() {
            3 | 4 => {
                let a = if cs.len() == 4 { d(cs[3])? as f32 / 255.0 } else { 1.0 };
                Some(CssColor::Rgba(d(cs[0])?, d(cs[1])?, d(cs[2])?, a))
            }
            6 | 8 if hex.is_ascii() => {
                let a = if cs.len() == 8 { h(&hex[6..8])? as f32 / 255.0 } else { 1.0 };
                Some(CssColor::Rgba(h(&hex[0..2])?, h(&hex[2..4])?, h(&hex[4..6])?, a))
            }
            _ => None,
        };
    }
    if let Some(open) = lower.find('(') {
        let func = lower[..open].trim();
        let inner = lower[open + 1..].trim_end().strip_suffix(')')?;
        let parts: Vec<&str> = inner
            .split(|c: char| c == ',' || c == '/' || c.is_whitespace())
            .filter(|p| !p.is_empty())
            .collect();
        if parts.len() < 3 {
            return None;
        }
        let alpha = match parts.get(3) {
            Some(a) => parse_alpha(a)?,
            None => 1.0,
        };
        return match func {
            "rgb" | "rgba" => {
                let ch = |p: &str| -> Option<u8> {
                    if let Some(pc) = p.strip_suffix('%') {
                        Some((pc.parse::<f32>().ok()?.clamp(0.0, 100.0) * 2.55).round() as u8)
                    } else {
                        Some(p.parse::<f32>().ok()?.clamp(0.0, 255.0).round() as u8)
                    }
                };
                Some(CssColor::Rgba(ch(parts[0])?, ch(parts[1])?, ch(parts[2])?, alpha))
            }
            "hsl" | "hsla" => {
                let h: f32 = parts[0].trim_end_matches("deg").parse().ok()?;
                let s: f32 = parts[1].trim_end_matches('%').parse().ok()?;
                let l: f32 = parts[2].trim_end_matches('%').parse().ok()?;
                let (r, g, b) = hsl_to_rgb(h, s / 100.0, l / 100.0);
                Some(CssColor::Rgba(r, g, b, alpha))
            }
            _ => None,
        };
    }
    named_color(&lower).map(|(r, g, b)| CssColor::Rgba(r, g, b, 1.0))
}

fn parse_alpha(a: &str) -> Option<f32> {
    if let Some(pc) = a.strip_suffix('%') {
        Some((pc.parse::<f32>().ok()? / 100.0).clamp(0.0, 1.0))
    } else {
        Some(a.parse::<f32>().ok()?.clamp(0.0, 1.0))
    }
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let h = ((h % 360.0) + 360.0) % 360.0 / 360.0;
    let s = s.clamp(0.0, 1.0);
    let l = l.clamp(0.0, 1.0);
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let f = |mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round().clamp(0.0, 255.0) as u8
    };
    (f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0))
}

pub fn named_color(name: &str) -> Option<(u8, u8, u8)> {
    let hex: u32 = match name {
        "aliceblue" => 0xf0f8ff,
        "antiquewhite" => 0xfaebd7,
        "aqua" | "cyan" => 0x00ffff,
        "aquamarine" => 0x7fffd4,
        "azure" => 0xf0ffff,
        "beige" => 0xf5f5dc,
        "bisque" => 0xffe4c4,
        "black" => 0x000000,
        "blanchedalmond" => 0xffebcd,
        "blue" => 0x0000ff,
        "blueviolet" => 0x8a2be2,
        "brown" => 0xa52a2a,
        "burlywood" => 0xdeb887,
        "cadetblue" => 0x5f9ea0,
        "chartreuse" => 0x7fff00,
        "chocolate" => 0xd2691e,
        "coral" => 0xff7f50,
        "cornflowerblue" => 0x6495ed,
        "cornsilk" => 0xfff8dc,
        "crimson" => 0xdc143c,
        "darkblue" => 0x00008b,
        "darkcyan" => 0x008b8b,
        "darkgoldenrod" => 0xb8860b,
        "darkgray" | "darkgrey" => 0xa9a9a9,
        "darkgreen" => 0x006400,
        "darkkhaki" => 0xbdb76b,
        "darkmagenta" => 0x8b008b,
        "darkolivegreen" => 0x556b2f,
        "darkorange" => 0xff8c00,
        "darkorchid" => 0x9932cc,
        "darkred" => 0x8b0000,
        "darksalmon" => 0xe9967a,
        "darkseagreen" => 0x8fbc8f,
        "darkslateblue" => 0x483d8b,
        "darkslategray" | "darkslategrey" => 0x2f4f4f,
        "darkturquoise" => 0x00ced1,
        "darkviolet" => 0x9400d3,
        "deeppink" => 0xff1493,
        "deepskyblue" => 0x00bfff,
        "dimgray" | "dimgrey" => 0x696969,
        "dodgerblue" => 0x1e90ff,
        "firebrick" => 0xb22222,
        "floralwhite" => 0xfffaf0,
        "forestgreen" => 0x228b22,
        "fuchsia" | "magenta" => 0xff00ff,
        "gainsboro" => 0xdcdcdc,
        "ghostwhite" => 0xf8f8ff,
        "gold" => 0xffd700,
        "goldenrod" => 0xdaa520,
        "gray" | "grey" => 0x808080,
        "green" => 0x008000,
        "greenyellow" => 0xadff2f,
        "honeydew" => 0xf0fff0,
        "hotpink" => 0xff69b4,
        "indianred" => 0xcd5c5c,
        "indigo" => 0x4b0082,
        "ivory" => 0xfffff0,
        "khaki" => 0xf0e68c,
        "lavender" => 0xe6e6fa,
        "lavenderblush" => 0xfff0f5,
        "lawngreen" => 0x7cfc00,
        "lemonchiffon" => 0xfffacd,
        "lightblue" => 0xadd8e6,
        "lightcoral" => 0xf08080,
        "lightcyan" => 0xe0ffff,
        "lightgoldenrodyellow" => 0xfafad2,
        "lightgray" | "lightgrey" => 0xd3d3d3,
        "lightgreen" => 0x90ee90,
        "lightpink" => 0xffb6c1,
        "lightsalmon" => 0xffa07a,
        "lightseagreen" => 0x20b2aa,
        "lightskyblue" => 0x87cefa,
        "lightslategray" | "lightslategrey" => 0x778899,
        "lightsteelblue" => 0xb0c4de,
        "lightyellow" => 0xffffe0,
        "lime" => 0x00ff00,
        "limegreen" => 0x32cd32,
        "linen" => 0xfaf0e6,
        "maroon" => 0x800000,
        "mediumaquamarine" => 0x66cdaa,
        "mediumblue" => 0x0000cd,
        "mediumorchid" => 0xba55d3,
        "mediumpurple" => 0x9370db,
        "mediumseagreen" => 0x3cb371,
        "mediumslateblue" => 0x7b68ee,
        "mediumspringgreen" => 0x00fa9a,
        "mediumturquoise" => 0x48d1cc,
        "mediumvioletred" => 0xc71585,
        "midnightblue" => 0x191970,
        "mintcream" => 0xf5fffa,
        "mistyrose" => 0xffe4e1,
        "moccasin" => 0xffe4b5,
        "navajowhite" => 0xffdead,
        "navy" => 0x000080,
        "oldlace" => 0xfdf5e6,
        "olive" => 0x808000,
        "olivedrab" => 0x6b8e23,
        "orange" => 0xffa500,
        "orangered" => 0xff4500,
        "orchid" => 0xda70d6,
        "palegoldenrod" => 0xeee8aa,
        "palegreen" => 0x98fb98,
        "paleturquoise" => 0xafeeee,
        "palevioletred" => 0xdb7093,
        "papayawhip" => 0xffefd5,
        "peachpuff" => 0xffdab9,
        "peru" => 0xcd853f,
        "pink" => 0xffc0cb,
        "plum" => 0xdda0dd,
        "powderblue" => 0xb0e0e6,
        "purple" => 0x800080,
        "rebeccapurple" => 0x663399,
        "red" => 0xff0000,
        "rosybrown" => 0xbc8f8f,
        "royalblue" => 0x4169e1,
        "saddlebrown" => 0x8b4513,
        "salmon" => 0xfa8072,
        "sandybrown" => 0xf4a460,
        "seagreen" => 0x2e8b57,
        "seashell" => 0xfff5ee,
        "sienna" => 0xa0522d,
        "silver" => 0xc0c0c0,
        "skyblue" => 0x87ceeb,
        "slateblue" => 0x6a5acd,
        "slategray" | "slategrey" => 0x708090,
        "snow" => 0xfffafa,
        "springgreen" => 0x00ff7f,
        "steelblue" => 0x4682b4,
        "tan" => 0xd2b48c,
        "teal" => 0x008080,
        "thistle" => 0xd8bfd8,
        "tomato" => 0xff6347,
        "turquoise" => 0x40e0d0,
        "violet" => 0xee82ee,
        "wheat" => 0xf5deb3,
        "white" => 0xffffff,
        "whitesmoke" => 0xf5f5f5,
        "yellow" => 0xffff00,
        "yellowgreen" => 0x9acd32,
        _ => return None,
    };
    Some(((hex >> 16) as u8, (hex >> 8) as u8, hex as u8))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> MediaEnv {
        MediaEnv {
            width_px: 800.0,
            height_px: 480.0,
            dark: false,
        }
    }

    #[test]
    fn specificity() {
        assert_eq!(analyze_selector(".card").0, pack(0, 1, 0));
        assert_eq!(analyze_selector("body.nightMode").0, pack(0, 1, 1));
        assert_eq!(analyze_selector("#a .b > p:first-child").0, pack(1, 2, 1));
        assert_eq!(analyze_selector(".nightMode .cloze").1, RightKey::Class("cloze".into()));
        assert_eq!(analyze_selector("code#typeans").1, RightKey::Id("typeans".into()));
        assert_eq!(analyze_selector("ul li").1, RightKey::Tag("li".into()));
        assert_eq!(analyze_selector("div :not(.x)").1, RightKey::Any);
        assert_eq!(analyze_selector("a:not(#x)").0, pack(1, 0, 1));
    }

    #[test]
    fn sheet_parsing() {
        let mut s = StyleSheet::default();
        let mut order = 0;
        s.parse_into(
            "/* c */ .a, .b:hover, p { color: red; background: #fff !important }\n\
             @media (max-width: 100px) { .x { color: blue } }\n\
             @media (prefers-color-scheme: light) { .y { color: blue } }\n\
             @font-face { font-family: x; src: url(_x.woff) }\n\
             @import url(\"_extra.css\");\n\
             .n { color: red; .inner { color: blue } font-weight: bold }",
            Origin::Author,
            &env(),
            &mut order,
        );
        let sels: Vec<_> = s.rules.iter().map(|r| r.key.clone()).collect();
        assert_eq!(
            sels,
            vec![
                RightKey::Class("a".into()),
                RightKey::Tag("p".into()),
                RightKey::Class("y".into()),
                RightKey::Class("n".into())
            ]
        );
        assert!(s.rules[0].declarations[1].important);
        assert_eq!(s.imports, vec!["_extra.css".to_string()]);
        let n: Vec<_> = s.rules[3].declarations.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(n, vec!["color", "font-weight"]);
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("#afa"), Some(CssColor::Rgba(0xaa, 0xff, 0xaa, 1.0)));
        assert_eq!(parse_color("rgb(1, 2, 3)"), Some(CssColor::Rgba(1, 2, 3, 1.0)));
        assert_eq!(parse_color("rgba(1 2 3 / 50%)"), Some(CssColor::Rgba(1, 2, 3, 0.5)));
        assert_eq!(parse_color("LightBlue"), Some(CssColor::Rgba(0xad, 0xd8, 0xe6, 1.0)));
        assert_eq!(parse_color("hsl(120, 100%, 50%)"), Some(CssColor::Rgba(0, 255, 0, 1.0)));
        assert_eq!(parse_color("nonsense"), None);
        assert_eq!(parse_color("#ggg"), None);
    }

    #[test]
    fn lengths() {
        assert_eq!(parse_length_px("40px", 16.0), Some(40.0));
        assert_eq!(parse_length_px("2em", 20.0), Some(40.0));
        assert_eq!(parse_length_px("auto", 20.0), None);
    }
}
