//! Native review of Anki's rendered image-occlusion elements. SVG is only an
//! intermediate drawing list: the image worker rasterizes it with tiny-skia.
use crate::css::{parse_color, parse_declarations, CssColor, StyleSheet};
use scraper::{ElementRef, Html};
use std::collections::HashMap;
use std::fmt::Write;

#[derive(Default)]
pub(crate) struct Occlusion {
    shapes: Vec<Shape>,
    variables: HashMap<String, String>,
    pub toggle: bool,
}

struct Shape {
    kind: String,
    state: usize,
    attrs: HashMap<String, String>,
}

impl Occlusion {
    pub fn parse(dom: &Html, sheet: &StyleSheet) -> Option<Self> {
        let elements: Vec<_> = dom
            .tree
            .root()
            .descendants()
            .filter_map(ElementRef::wrap)
            .collect();
        let canvas = elements
            .iter()
            .find(|e| e.value().id() == Some("image-occlusion-canvas"))?;
        let mut variables = HashMap::new();
        let mut ancestors: Vec<_> = canvas.ancestors().filter_map(ElementRef::wrap).collect();
        ancestors.reverse();
        ancestors.push(*canvas);
        for el in ancestors {
            let mut winners = HashMap::new();
            for rule in &sheet.rules {
                if !rule.selector.matches(&el) {
                    continue;
                }
                for d in rule
                    .declarations
                    .iter()
                    .filter(|d| d.name.starts_with("--"))
                {
                    let rank = (d.important, rule.specificity, rule.order);
                    let old = winners
                        .entry(d.name.clone())
                        .or_insert((rank, d.value.clone()));
                    if rank >= old.0 {
                        *old = (rank, d.value.clone());
                    }
                }
            }
            for d in parse_declarations(el.value().attr("style").unwrap_or_default()) {
                if !d.name.starts_with("--") {
                    continue;
                }
                let rank = (d.important, u32::MAX, u32::MAX);
                let old = winners.entry(d.name).or_insert((rank, d.value.clone()));
                if rank >= old.0 {
                    *old = (rank, d.value);
                }
            }
            variables.extend(winners.into_iter().map(|(key, (_, value))| (key, value)));
        }
        let mut result = Self {
            variables,
            ..Self::default()
        };
        for el in elements
            .into_iter()
            .filter(|e| e.value().attr("data-shape").is_some())
            .take(1000)
        {
            let state = if el
                .value()
                .has_class("cloze", scraper::CaseSensitivity::CaseSensitive)
            {
                0
            } else if el
                .value()
                .has_class("cloze-inactive", scraper::CaseSensitivity::CaseSensitive)
            {
                1
            } else if el
                .value()
                .has_class("cloze-highlight", scraper::CaseSensitivity::CaseSensitive)
            {
                2
            } else {
                continue;
            };
            let attrs: HashMap<_, _> = el
                .value()
                .attrs()
                .filter_map(|(k, v)| {
                    k.strip_prefix("data-")
                        .map(|k| (k.to_owned(), v.to_owned()))
                })
                .collect();
            result.toggle |= attrs.get("occludeinactive").is_some_and(|s| s == "1");
            result.shapes.push(Shape {
                kind: attrs.get("shape").cloned().unwrap_or_default(),
                state,
                attrs,
            });
        }
        Some(result)
    }

    fn variable(&self, name: &str, fallback: &str) -> String {
        let mut value = self
            .variables
            .get(name)
            .map(String::as_str)
            .unwrap_or(fallback)
            .to_owned();
        for _ in 0..8 {
            let Some(inner) = value.strip_prefix("var(").and_then(|v| v.strip_suffix(')')) else {
                break;
            };
            let (name, fallback) = inner.split_once(',').unwrap_or((inner, fallback));
            value = self
                .variables
                .get(name.trim())
                .map(String::as_str)
                .unwrap_or(fallback.trim())
                .to_owned();
        }
        value
    }

    pub fn overlay(&self, width: u32, height: u32, hidden: bool, display_width: f32) -> String {
        let (w, h) = (width as f64, height as f64);
        let stroke_scale = (w / display_width.max(1.0) as f64).max(1.0);
        let mut svg = format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">");
        // Match Anki's active, inactive, highlight draw order, including groups.
        for state in 0..3 {
            for s in self.shapes.iter().filter(|s| s.state == state) {
                let label = s.kind == "text";
                if !label
                    && (hidden
                        || (state == 1 && s.attrs.get("occludeinactive").is_none_or(|v| v != "1")))
                {
                    continue;
                }
                let n = |key: &str, default: f64| {
                    s.attrs
                        .get(key)
                        .and_then(|v| v.parse::<f64>().ok())
                        .filter(|v| v.is_finite() && v.abs() < 1e6)
                        .unwrap_or(default)
                };
                let (x, y) = ((n("left", 0.0) * w).round(), (n("top", 0.0) * h).round());
                let (prefix, default_fill, default_border) = match state {
                    0 => ("active", "#ff8e8e", "1px #212121"),
                    1 => ("inactive", "#ffeba2", "1px #212121"),
                    _ => ("highlight", "#ff8e8e00", "1px #ff8e8e"),
                };
                let mut fill = self.variable(&format!("--{prefix}-shape-color"), default_fill);
                if state == 1 {
                    if let Some(custom) = s
                        .attrs
                        .get("fill")
                        .filter(|v| !v.eq_ignore_ascii_case("#ffeba2"))
                    {
                        fill = custom.clone();
                    }
                }
                let border = self.variable(&format!("--{prefix}-shape-border"), default_border);
                let (border_w, border_color) = border
                    .split_once(char::is_whitespace)
                    .unwrap_or(("0", "transparent"));
                let sw = border_w
                    .trim_end_matches("px")
                    .parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite() && *v >= 0.0)
                    .unwrap_or(0.0)
                    * stroke_scale;
                let (fill, opacity) = color(&fill);
                let (stroke, stroke_opacity) = color(border_color.trim());
                let angle = n("angle", 0.0) % 10000.0 / 10000.0 * 360.0;
                let _ = write!(svg,"<g fill=\"{fill}\" fill-opacity=\"{opacity}\" stroke=\"{stroke}\" stroke-opacity=\"{stroke_opacity}\" stroke-width=\"{sw}\" transform=\"rotate({angle} {x} {y})\">");
                match s.kind.as_str() {
                    "rect" => {
                        let _ = write!(
                            svg,
                            "<rect x=\"{x}\" y=\"{y}\" width=\"{}\" height=\"{}\"/>",
                            (n("width", 0.0) * w).max(0.0),
                            (n("height", 0.0) * h).max(0.0)
                        );
                    }
                    "ellipse" => {
                        let (rx, ry) = ((n("rx", 0.0) * w).max(0.0), (n("ry", 0.0) * h).max(0.0));
                        let _ = write!(
                            svg,
                            "<ellipse cx=\"{}\" cy=\"{}\" rx=\"{rx}\" ry=\"{ry}\"/>",
                            x + rx,
                            y + ry
                        );
                    }
                    "polygon" => {
                        let points: Vec<_> = s
                            .attrs
                            .get("points")
                            .map(String::as_str)
                            .unwrap_or("")
                            .split_whitespace()
                            .take(1000)
                            .filter_map(|p| {
                                let (px, py) = p.split_once(',')?;
                                let (px, py) = (px.parse::<f64>().ok()?, py.parse::<f64>().ok()?);
                                (px.is_finite()
                                    && py.is_finite()
                                    && px.abs() < 1e6
                                    && py.abs() < 1e6)
                                    .then_some((px * w, py * h))
                            })
                            .collect();
                        if points.len() >= 3 {
                            let min_x = points.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
                            let min_y = points.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
                            svg.push_str("<polygon points=\"");
                            for (px, py) in points {
                                let _ = write!(svg, "{},{} ", x + px - min_x, y + py - min_y);
                            }
                            svg.push_str("\"/>");
                        }
                    }
                    "text" => {
                        let size = n("font-size", 40.0 / h).clamp(0.0, 2.0) * h;
                        let scale = n("scale", 1.0).clamp(0.01, 100.0);
                        let text = s.attrs.get("text").map(String::as_str).unwrap_or("");
                        let lines: Vec<_> = text.split('\n').take(100).collect();
                        let longest = lines
                            .iter()
                            .map(|line| line.chars().take(1000).count())
                            .max()
                            .unwrap_or(0);
                        let (fill, opacity) =
                            color(s.attrs.get("fill").map(String::as_str).unwrap_or("black"));
                        let _=write!(svg,"<g transform=\"translate({x} {y}) scale({scale})\" stroke=\"none\"><rect width=\"{}\" height=\"{}\" fill=\"white\" fill-opacity=\"1\"/>",longest as f64*size*0.65+5.0,lines.len() as f64*size*1.5+5.0);
                        for (i, line) in lines.iter().enumerate() {
                            let _=write!(svg,"<text x=\"0\" y=\"{}\" font-size=\"{size}\" font-family=\"Arial, sans-serif\" fill=\"{fill}\" fill-opacity=\"{opacity}\">{}</text>",size*(1.0+i as f64*1.5),escape(&line.chars().take(1000).collect::<String>()));
                        }
                        svg.push_str("</g>");
                    }
                    _ => {}
                }
                svg.push_str("</g>");
            }
        }
        svg.push_str("</svg>");
        svg
    }
}

fn color(value: &str) -> (String, f32) {
    match parse_color(value) {
        Some(CssColor::Rgba(r, g, b, a)) => (format!("#{r:02x}{g:02x}{b:02x}"), a),
        _ => ("#000000".into(), 1.0),
    }
}
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::{MediaEnv, Origin};
    fn fixture(shape: &str, css: &str) -> Occlusion {
        let dom=Html::parse_document(&format!("<body class=nightMode><div style='display:none'>{shape}</div><div id=image-occlusion-container><img src=x.png><canvas id=image-occlusion-canvas></canvas></div></body>"));
        let mut sheet = StyleSheet::default();
        sheet.parse_into(
            css,
            Origin::Author,
            &MediaEnv {
                width_px: 100.0,
                height_px: 100.0,
                dark: true,
            },
            &mut 0,
        );
        Occlusion::parse(&dom, &sheet).unwrap()
    }
    #[test]
    fn modes_colours_cascade_and_answer_outline() {
        let shapes="<div class=cloze data-shape=rect data-left=.1 data-top=.2 data-width=.3 data-height=.4></div><div class=cloze-inactive data-shape=rect data-left=.6 data-width=.2 data-height=.2></div><div class=cloze-highlight data-shape=rect data-width=.1 data-height=.1></div>";
        let io=fixture(shapes,"#image-occlusion-canvas {--active-shape-color:blue} .nightMode #image-occlusion-canvas {--active-shape-color:#123456}");
        let svg = io.overlay(100, 100, false, 100.0);
        assert!(svg.contains("fill=\"#123456\""));
        assert!(svg.contains("x=\"10\" y=\"20\""));
        assert_eq!(svg.matches("<rect").count(), 2);
        assert!(svg.contains("fill-opacity=\"0\""));
        let all = fixture(
            &shapes.replace(
                "class=cloze-inactive",
                "class=cloze-inactive data-occludeinactive=1",
            ),
            "",
        );
        assert!(all.toggle);
        assert_eq!(
            all.overlay(100, 100, false, 100.0).matches("<rect").count(),
            3
        );
        assert_eq!(
            all.overlay(100, 100, true, 100.0).matches("<rect").count(),
            0
        );
    }
    #[test]
    fn rotated_ellipse_polygon_and_labels_are_safe() {
        let io=fixture("<div class=cloze data-shape=ellipse data-left=.1 data-top=.2 data-rx=.1 data-ry=.2 data-angle=2500></div><div class=cloze data-shape=polygon data-left=.4 data-top=.5 data-points='.1,.1 .3,.1 .2,.3'></div><div class=cloze data-shape=text data-font-size=.1 data-text='A &amp; &lt;b&gt;' data-fill=green></div>","");
        let svg = io.overlay(100, 100, false, 100.0);
        assert!(svg.contains("rotate(90 10 20)"));
        assert!(svg.contains("cx=\"20\" cy=\"40\""));
        assert!(svg.contains("40,50 60,50 50,70"));
        assert!(svg.contains("A &amp; &lt;b&gt;"));
        assert!(io.overlay(100, 100, true, 100.0).contains("<text"));
        let image = crate::image_cache::decode(&crate::ImageSource::Svg(svg), (100, 100)).unwrap();
        assert_eq!(image.width(), 100);
    }
}
