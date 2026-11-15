//! Explicit rendering tiers and a template classifier. Classification is a
//! suggestion only: card content cannot opt itself into a heavier renderer.
use crate::{CardSide, RenderOptions, RenderedCard};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderTier {
    #[default]
    Text,
    Bitmap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub suggested: RenderTier,
    pub needs_browser: bool,
    pub native_occlusion: bool,
    pub reasons: Vec<String>,
}

/// Inspect both templates and CSS once per note type. Stock IO scripts are
/// replaced by native masks, so they do not request the future browser tier.
pub fn classify(question: &str, answer: &str, css: &str) -> Classification {
    let html = format!("{question}\n{answer}");
    let dom = Html::parse_fragment(&html);
    let has = |selector: &str| {
        dom.select(&Selector::parse(selector).unwrap())
            .next()
            .is_some()
    };
    let native_occlusion = has("#image-occlusion-container");
    let mut reasons = Vec::new();
    let scripts = dom
        .select(&Selector::parse("script").unwrap())
        .any(|script| {
            script.value().attr("src").is_some()
                || !native_occlusion
                || !native_io_script(&script.text().collect::<String>())
        })
        || dom
            .tree
            .root()
            .descendants()
            .filter_map(scraper::ElementRef::wrap)
            .any(|el| {
                el.value()
                    .attrs()
                    .any(|(name, value)| name.starts_with("on") && !value.trim().is_empty())
            });
    let embedded = has("iframe, object, embed");
    if scripts {
        reasons.push("Card uses JavaScript; use Open in Browser for scripts".into());
    }
    if embedded {
        reasons.push("Card embeds browser content".into());
    }
    // Whitespace and case should not affect a declaration's classification.
    // Strip comments so a commented-out experiment does not trigger a badge.
    let inline = dom
        .select(&Selector::parse("[style]").unwrap())
        .filter_map(|el| el.value().attr("style"))
        .collect::<Vec<_>>()
        .join(";");
    let embedded_css = dom
        .select(&Selector::parse("style").unwrap())
        .map(|el| el.text().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    let all_css = format!("{css};{inline};{embedded_css}");
    let mut stripped = String::new();
    let mut rest = all_css.as_str();
    while let Some(start) = rest.find("/*") {
        stripped.push_str(&rest[..start]);
        rest = rest[start + 2..]
            .split_once("*/")
            .map_or("", |(_, tail)| tail);
    }
    stripped.push_str(rest);
    let compact: String = stripped
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    let complex = [
        "display:flex",
        "display:inline-flex",
        "display:grid",
        "display:inline-grid",
        "position:absolute",
        "position:fixed",
        "transform:",
        "@font-face",
    ]
    .iter()
    .any(|declaration| compact.contains(declaration));
    if complex && !native_occlusion {
        reasons.push("Complex CSS: bitmap rendering is available with the blitz feature".into());
    }
    Classification {
        suggested: if complex && !native_occlusion {
            RenderTier::Bitmap
        } else {
            RenderTier::Text
        },
        needs_browser: scripts || embedded,
        native_occlusion,
        reasons,
    }
}

/// Only the stock native-IO bridge is exempt from JavaScript warnings. A
/// modified template can contain arbitrary additional scripts, even in IO.
pub(crate) fn native_io_script(script: &str) -> bool {
    let compact: String = script.chars().filter(|c| !c.is_whitespace()).collect();
    if compact == "anki.imageOcclusion.setup();" || compact == "anki.imageOcclusion.setup()" {
        return true;
    }
    let Some(message)=compact.strip_prefix("try{anki.imageOcclusion.setup();}catch(exc){document.getElementById(\"err\").innerHTML=`").and_then(|s|s.strip_suffix("<br><br>${exc}`;}")) else {return false};
    !message.contains(['`', '\\', '$'])
}

pub(crate) fn render(side: &CardSide, opts: &RenderOptions) -> RenderedCard {
    let mut text = crate::layout::render(side, opts);
    if opts.tier != RenderTier::Bitmap {
        return text;
    }
    if !opts.images {
        text.notices
            .push("Bitmap tier needs graphics; showing text".into());
        return text;
    }
    // Keep native interaction and masks when a bitmap would hide a control.
    if side.html.contains("image-occlusion-container")
        || text
            .widgets
            .iter()
            .any(|w| !matches!(w.kind, crate::WidgetKind::Replay { .. }))
    {
        text.notices
            .push("Using native rendering for interactive card controls".into());
        return text;
    }
    #[cfg(feature = "blitz")]
    match bitmap::request(side, opts) {
        Some(Ok(card)) => return card,
        Some(Err(error)) => text
            .notices
            .push(format!("Bitmap renderer: {error}; showing text")),
        None => text.notices.push("Rendering bitmap…".into()),
    }
    #[cfg(not(feature = "blitz"))]
    text.notices
        .push("Bitmap tier unavailable: build with --features blitz".into());
    text
}

/// Starts a render without waiting; the reviewer uses this for the next card.
pub fn pre_render(side: &CardSide, opts: &RenderOptions) {
    #[cfg(feature = "blitz")]
    if opts.tier == RenderTier::Bitmap && opts.images {
        let _ = bitmap::request(side, opts);
    }
    #[cfg(not(feature = "blitz"))]
    let _ = (side, opts);
}

/// Changes when a background bitmap completes, to invalidate screen caches.
pub fn revision() -> u64 {
    #[cfg(feature = "blitz")]
    return bitmap::revision();
    #[cfg(not(feature = "blitz"))]
    0
}

#[cfg(feature = "blitz")]
mod bitmap;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_classifier_corpus() {
        for (html, css, tier, browser) in [
            ("{{Front}}", ".card {text-align:center}", RenderTier::Text, false),
            ("<div style='DISPLAY: GRID'>x</div>", "", RenderTier::Bitmap, false),
            ("<script>run()</script>", "", RenderTier::Text, true),
            ("<iframe src='x'></iframe>", "", RenderTier::Text, true),
            ("<div>simple</div>", "/* display:flex */", RenderTier::Text, false),
            ("<ruby>字<rt>じ</rt></ruby>", "", RenderTier::Text, false),
            ("<div id='image-occlusion-container'><script>anki.imageOcclusion.setup()</script></div>", "canvas{position:absolute}", RenderTier::Text, false),
            ("<div id='image-occlusion-container'><script>anki.imageOcclusion.setup(); alert(1)</script></div>", "", RenderTier::Text, true),
            ("<style>.x {display:flex}</style>", "", RenderTier::Bitmap, false),
            ("<div onclick='run()'>x</div>", "", RenderTier::Text, true),
        ] {
            let classified = classify(html, "", css);
            assert_eq!((classified.suggested, classified.needs_browser), (tier, browser), "{html} {css}");
        }
    }

    #[test]
    fn choosing_bitmap_without_graphics_keeps_content() {
        let card = render(
            &CardSide {
                html: "hello".into(),
                ..Default::default()
            },
            &RenderOptions {
                tier: RenderTier::Bitmap,
                ..Default::default()
            },
        );
        assert!(card
            .lines
            .iter()
            .flat_map(|l| &l.spans)
            .any(|s| s.text.contains("hello")));
        assert!(card.notices.iter().any(|n| n.contains("needs graphics")));
    }
}
