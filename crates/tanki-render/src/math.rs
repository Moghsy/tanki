//! MathJax delimiters, conservative Unicode conversion and self-contained RaTeX SVGs.
use crate::Rgb;
use std::cell::RefCell;

#[derive(Debug, PartialEq)]
pub(crate) enum Segment<'a> {
    Text(&'a str),
    Formula { source: &'a str, display: bool },
}

/// Unclosed delimiters remain literal text. MathJax's reviewer configuration
/// deliberately does not interpret dollar delimiters or TeX environments.
pub(crate) fn segments(mut input: &str) -> Vec<Segment<'_>> {
    let mut out = Vec::new();
    while let Some((pos, display)) = input
        .match_indices("\\(")
        .map(|(p, _)| (p, false))
        .chain(input.match_indices("\\[").map(|(p, _)| (p, true)))
        .min_by_key(|(p, _)| *p)
    {
        let closing = if display { "\\]" } else { "\\)" };
        let Some(end) = input[pos + 2..].find(closing).map(|p| p + pos + 2) else {
            break;
        };
        out.push(Segment::Text(&input[..pos]));
        out.push(Segment::Formula {
            source: &input[pos + 2..end],
            display,
        });
        input = &input[end + 2..];
    }
    out.push(Segment::Text(input));
    out
}

pub(crate) fn unicode(source: &str) -> Option<String> {
    if source.len() > 1024 {
        return None;
    }
    // This deliberately small parser is independently maintained. Anything
    // requiring TeX layout, or lacking an exact script character, uses RaTeX.
    let mut chars = source.chars().peekable();
    let mut out = String::new();
    let mut base_seen = false;
    let mut scripts = 0;
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let mut name = String::new();
                while chars.peek().is_some_and(char::is_ascii_alphabetic) {
                    name.push(chars.next()?);
                }
                out.push(symbol(&name)?);
                base_seen = true;
                scripts = 0;
            }
            '^' | '_' => {
                let bit = if c == '^' { 1 } else { 2 };
                if !base_seen || scripts & bit != 0 {
                    return None;
                }
                let first = chars.next()?;
                let mut script = String::new();
                if first == '{' {
                    loop {
                        let next = chars.next()?;
                        if next == '}' {
                            break;
                        }
                        script.push(next);
                    }
                } else {
                    script.push(first);
                }
                if script.is_empty() {
                    return None;
                }
                out.push_str(&crate::text::map_script(&script, c == '^')?);
                scripts |= bit;
            }
            '{' | '}' | '$' | '&' | '#' | '%' => return None,
            c => {
                out.push(c);
                if !c.is_whitespace() {
                    base_seen = true;
                    scripts = 0;
                }
            }
        }
    }
    Some(out)
}

/// Common mathematical names only; unknown commands are never discarded.
fn symbol(command: &str) -> Option<char> {
    Some(match command {
        "alpha" => 'α',
        "beta" => 'β',
        "gamma" => 'γ',
        "delta" => 'δ',
        "epsilon" => 'ϵ',
        "varepsilon" => 'ε',
        "zeta" => 'ζ',
        "eta" => 'η',
        "theta" => 'θ',
        "vartheta" => 'ϑ',
        "iota" => 'ι',
        "kappa" => 'κ',
        "lambda" => 'λ',
        "mu" => 'μ',
        "nu" => 'ν',
        "xi" => 'ξ',
        "pi" => 'π',
        "rho" => 'ρ',
        "sigma" => 'σ',
        "varsigma" => 'ς',
        "tau" => 'τ',
        "upsilon" => 'υ',
        "phi" => 'ϕ',
        "varphi" => 'φ',
        "chi" => 'χ',
        "psi" => 'ψ',
        "omega" => 'ω',
        "Gamma" => 'Γ',
        "Delta" => 'Δ',
        "Theta" => 'Θ',
        "Lambda" => 'Λ',
        "Xi" => 'Ξ',
        "Pi" => 'Π',
        "Sigma" => 'Σ',
        "Upsilon" => 'Υ',
        "Phi" => 'Φ',
        "Psi" => 'Ψ',
        "Omega" => 'Ω',
        "times" => '×',
        "cdot" => '·',
        "pm" => '±',
        "mp" => '∓',
        "le" | "leq" => '≤',
        "ge" | "geq" => '≥',
        "ne" | "neq" => '≠',
        "approx" => '≈',
        "infty" => '∞',
        "to" | "rightarrow" => '→',
        "leftarrow" => '←',
        "leftrightarrow" => '↔',
        "in" => '∈',
        "notin" => '∉',
        "forall" => '∀',
        "exists" => '∃',
        "partial" => '∂',
        "nabla" => '∇',
        _ => return None,
    })
}

type CacheEntry = ((String, bool, u16, Rgb), Result<String, String>);
thread_local! {
    static CACHE: RefCell<Vec<CacheEntry>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn svg(
    source: &str,
    display: bool,
    cell_height: u16,
    fg: Rgb,
) -> Result<String, String> {
    if source.len() > 16_384 {
        return Err("formula exceeds the rendering limit".into());
    }
    let key = (source.to_owned(), display, cell_height, fg);
    if let Some(result) = CACHE.with(|c| {
        c.borrow()
            .iter()
            .find(|(k, _)| k == &key)
            .map(|(_, v)| v.clone())
    }) {
        return result;
    }
    let result = make_svg(source, display, cell_height, fg);
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= 128 {
            let _ = c.remove(0);
        }
        c.push((key, result.clone()));
    });
    result
}

fn make_svg(source: &str, display: bool, cell_height: u16, fg: Rgb) -> Result<String, String> {
    use ratex_layout::{layout, to_display_list, validate_layout, LayoutOptions};
    use ratex_types::{color::Color, math_style::MathStyle};
    let parsed = ratex_parser::parse(source).map_err(|e| e.to_string())?;
    let options = LayoutOptions::default()
        .with_style(if display {
            MathStyle::Display
        } else {
            MathStyle::Text
        })
        .with_color(Color::rgb(
            fg.0 as f32 / 255.0,
            fg.1 as f32 / 255.0,
            fg.2 as f32 / 255.0,
        ));
    let layout = layout(&parsed, &options);
    validate_layout(&layout)?;
    let list = to_display_list(&layout);
    if list.width > 1000.0 || list.total_height() > 1000.0 {
        return Err("formula exceeds the rendering limit".into());
    }
    let options = ratex_svg::SvgOptions {
        // RaTeX emits pt, resvg converts to px at 96 dpi.
        font_size: f64::from(cell_height.max(1)) * 0.75,
        padding: 1.5,
        embed_glyphs: true,
        ..Default::default()
    };
    Ok(ratex_svg::render_to_svg_with_color_syntax(
        &list,
        &options,
        ratex_svg::SvgColorSyntax::Rgb,
    ))
}

/// Prepares text-node MathJax for the bitmap tier. Generated SVGs still pass
/// through the bitmap tier's local SVG rasterizer and resource restrictions.
#[cfg(any(feature = "blitz", test))]
pub(crate) fn prepare_html(html: &str, cell_height: u16, fg: Rgb) -> String {
    if !html.contains(r"\(") && !html.contains(r"\[") {
        return html.into();
    }
    let document = html.to_ascii_lowercase().contains("<html")
        || html
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("<!doctype");
    let dom = if document {
        scraper::Html::parse_document(html)
    } else {
        scraper::Html::parse_fragment(html)
    };
    let mut out = String::new();
    if document {
        out.push_str("<!doctype html>");
    }
    for node in dom.tree.root().children() {
        if document {
            prepare_node(node, cell_height, fg, 0, &mut out);
        } else if let Some(el) = scraper::ElementRef::wrap(node) {
            // html5ever wraps every fragment in a synthetic <html> element.
            for child in el.children() {
                prepare_node(child, cell_height, fg, 0, &mut out);
            }
        } else {
            prepare_node(node, cell_height, fg, 0, &mut out);
        }
    }
    out
}

#[cfg(any(feature = "blitz", test))]
fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(any(feature = "blitz", test))]
fn prepare_node(
    node: ego_tree::NodeRef<'_, scraper::node::Node>,
    cell_height: u16,
    fg: Rgb,
    depth: usize,
    out: &mut String,
) {
    use scraper::node::Node;
    match node.value() {
        Node::Text(text) => {
            for segment in segments(text) {
                match segment {
                    Segment::Text(text) => out.push_str(&escape_html(text)),
                    Segment::Formula { source, display } => {
                        if !display {
                            if let Some(text) = unicode(source) {
                                out.push_str(&escape_html(&text));
                                continue;
                            }
                        }
                        match svg(source, display, cell_height, fg) {
                            Ok(svg) => {
                                if display {
                                    out.push_str("<div class=\"tanki-display-math\">");
                                }
                                out.push_str(&svg);
                                if display {
                                    out.push_str("</div>");
                                }
                            }
                            Err(_) => {
                                let escaped = escape_html(source);
                                if display {
                                    out.push_str(&format!(r"\[{escaped}\]"));
                                } else {
                                    out.push_str(&format!(r"\({escaped}\)"));
                                }
                            }
                        }
                    }
                }
            }
        }
        Node::Element(element) => {
            let name = element.name();
            let el = scraper::ElementRef::wrap(node).expect("element node");
            if depth >= 200
                || matches!(
                    name,
                    "script" | "style" | "pre" | "code" | "title" | "textarea" | "svg"
                )
            {
                out.push_str(&el.html());
                return;
            }
            out.push('<');
            out.push_str(name);
            for (key, value) in element.attrs() {
                out.push(' ');
                out.push_str(key);
                out.push_str("=\"");
                out.push_str(&escape_html(value));
                out.push('"');
            }
            out.push('>');
            if matches!(
                name,
                "area"
                    | "base"
                    | "br"
                    | "col"
                    | "embed"
                    | "hr"
                    | "img"
                    | "input"
                    | "link"
                    | "meta"
                    | "param"
                    | "source"
                    | "track"
                    | "wbr"
            ) {
                return;
            }
            for child in node.children() {
                prepare_node(child, cell_height, fg, depth + 1, out);
            }
            out.push_str("</");
            out.push_str(name);
            out.push('>');
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delimiters_and_unclosed_source() {
        assert_eq!(
            segments(r"a \(x\) b \[y\] c \(z"),
            vec![
                Segment::Text("a "),
                Segment::Formula {
                    source: "x",
                    display: false
                },
                Segment::Text(" b "),
                Segment::Formula {
                    source: "y",
                    display: true
                },
                Segment::Text(r" c \(z"),
            ]
        );
    }
    #[test]
    fn unicode_is_conservative() {
        assert_eq!(unicode(r"\alpha^2"), Some("α²".into()));
        assert_eq!(unicode(r"x^{n+1}_0 \leq \beta"), Some("xⁿ⁺¹₀ ≤ β".into()));
        assert_eq!(unicode(r"\Gamma \times \pi"), Some("Γ × π".into()));
        assert!(unicode(r"\frac{a}{b}").is_none());
        assert!(unicode(r"\unknown{x}").is_none());
        assert!(unicode(r"\alphabets").is_none());
        assert!(unicode(r"x^q").is_none());
        assert!(unicode(r"x^{2").is_none());
        assert!(unicode(r"x^2^3").is_none());
        assert!(unicode(r"^2").is_none());
        assert!(unicode(r"x^{}").is_none());
    }
    #[test]
    fn fraction_and_chemistry_have_embedded_shapes_and_rasterize() {
        for source in [r"\frac{a}{b}", r"\ce{H2O}"] {
            let svg = svg(source, true, 20, Rgb(233, 234, 235)).unwrap();
            assert!(svg.contains("<path"));
            assert!(!svg.contains("<text"));
            assert!(svg.contains("rgb(233,234,235)"));
            let image =
                crate::image_cache::decode(&crate::ImageSource::Svg(svg), (300, 100)).unwrap();
            assert!(image.to_rgba8().pixels().any(|p| p.0[3] != 0));
        }
        assert!(svg(r"\totallyUnknownMacro", false, 20, Rgb(0, 0, 0)).is_err());
    }

    #[test]
    fn bitmap_html_prepares_text_only_and_preserves_literal_contexts() {
        let html = r#"<p data-formula="\(attr\)">\(\alpha^2\) &amp; \[\frac{1}{x}\]</p><script>const s='\(script\)';</script><style>.x {content:'\(style\)'}</style><pre>\(pre\)</pre><code>\(code\)</code>"#;
        let out = prepare_html(html, 20, Rgb(20, 30, 40));
        assert!(out.contains("α² &amp;"), "{out}");
        assert!(out.contains("<svg"));
        assert!(out.contains("rgb(20,30,40)"));
        for source in ["attr", "script", "style", "pre", "code"] {
            assert!(out.contains(&format!(r"\({source}\)")), "{out}");
        }
        assert!(out.contains("class=\"tanki-display-math\""));
        let unsupported = prepare_html(r"<p>\(\unknown{&lt;x&gt;}\)</p>", 20, Rgb(0, 0, 0));
        assert!(unsupported.contains(r"\(\unknown{&lt;x&gt;}\)"));
        let full = prepare_html(
            r"<!doctype html><html><head><title>x</title></head><body>\(x^2\)</body></html>",
            20,
            Rgb(0, 0, 0),
        );
        assert!(full.starts_with("<!doctype html><html>"));
        assert!(full.contains("<body>x²</body>"));
    }
}
