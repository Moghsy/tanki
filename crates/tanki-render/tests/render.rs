//! Synthetic HTML only. The golden input was generated from the pinned Anki
//! version by the Phase 0 fixture harness; these snapshots inspect painted cells.
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier},
};
use tanki_render::*;
use unicode_width::UnicodeWidthStr;

fn side(html: &str, css: &str) -> CardSide {
    CardSide {
        html: html.into(),
        css: css.into(),
        body_classes: "card card1 isLin".into(),
        ..Default::default()
    }
}

fn plain(card: &RenderedCard) -> String {
    card.lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.text.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn cells(card: &RenderedCard, width: u16) -> String {
    use std::fmt::Write;
    let area = Rect::new(0, 0, width, card.lines.len().max(1) as u16);
    let mut buf = Buffer::empty(area);
    paint(card, 0, area, &mut buf);
    let mut out = String::new();
    for y in 0..area.height {
        let symbols: String = (0..width).map(|x| buf[(x, y)].symbol()).collect();
        writeln!(out, "{y:02}|{symbols}|").unwrap();
        let mut start = 0;
        while start < width {
            let c = &buf[(start, y)];
            let mut end = start + 1;
            while end < width {
                let n = &buf[(end, y)];
                if (c.fg, c.bg, c.modifier) != (n.fg, n.bg, n.modifier) {
                    break;
                }
                end += 1;
            }
            if c.fg != Color::Reset || c.bg != Color::Reset || !c.modifier.is_empty() {
                writeln!(
                    out,
                    "  {start}..{end}: {:?} {:?} {:?}",
                    c.fg, c.bg, c.modifier
                )
                .unwrap();
            }
            start = end;
        }
    }
    writeln!(out, "answer_row: {:?}", card.answer_row).unwrap();
    writeln!(out, "widgets: {:?}", card.widgets).unwrap();
    writeln!(out, "notices: {:?}", card.notices).unwrap();
    out
}

#[test]
fn stock_notetypes_cell_buffers() {
    let golden: serde_json::Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/golden/render.json")).unwrap();
    let cards = golden["cards"].as_object().unwrap();
    let mut kinds = std::collections::BTreeSet::new();
    for (key, card) in cards.iter().filter(|(k, _)| k.ends_with("|stock")) {
        kinds.insert(key.split('|').next().unwrap());
        let mut snapshot = String::new();
        for night_mode in [false, true] {
            for face in ["question", "answer"] {
                // These are the pure reviewer filters' outputs at the render
                // boundary. Core owns testing the actual input/diff filters.
                let replacement = if face == "question" {
                    "<center><input type=text id=typeans></center>"
                } else {
                    "<code id=typeans><span class=typeGood>Ans</span><span class=typeBad>x</span><br>↓<br><span class=typeGood>Ans</span><span class=typeMissed>wer</span></code>"
                };
                let html = card[face]
                    .as_str()
                    .unwrap()
                    .replace("[[type:Back]]", replacement);
                let options = RenderOptions {
                    width: 40,
                    night_mode,
                    ..Default::default()
                };
                let rendered = render(&side(&html, card["css"].as_str().unwrap()), &options);
                snapshot.push_str(&format!(
                    "{face}, night={night_mode}\n{}\n",
                    cells(&rendered, options.width)
                ));
            }
        }
        let name = key.replace([' ', '(', ')', '|'], "_");
        insta::assert_snapshot!(name, snapshot);
    }
    assert_eq!(kinds.len(), 6, "all six stock note types must be covered");
}

#[test]
fn cascade_hiding_specificity_inline_and_night_override() {
    let card = side("<span class=gone>SECRET</span><b id=x class=word style='color:#123456'>Visible</b><span hidden>HIDDEN</span>",
        ".card {color:black;background:white} .gone {display:none} .word {color:green} #x {color:red !important}");
    let day = render(&card, &RenderOptions::default());
    assert_eq!(plain(&day), "Visible");
    assert!(render(
        &side("hidden card", ".card {display:none}"),
        &RenderOptions::default()
    )
    .lines
    .is_empty());
    assert_eq!(day.lines[0].spans[0].style.fg, Some(Rgb(255, 0, 0)));
    let stock = side("Normal <span class=cloze>cloze</span>", ".card {color:black;background:white} .cloze {color:blue;font-weight:bold} .nightMode .cloze {color:lightblue}");
    let night = render(
        &stock,
        &RenderOptions {
            night_mode: true,
            ..Default::default()
        },
    );
    assert_eq!(night.lines[0].spans[0].style.fg, None);
    assert_eq!(night.lines[0].spans[1].style.fg, Some(Rgb(173, 216, 230)));
    assert!(night.lines[0].spans[1].style.bold);
    // An author's stronger night rule can still set a deliberate colour.
    let custom = side("text", ".card.nightMode {color:yellow}");
    assert_eq!(
        render(
            &custom,
            &RenderOptions {
                night_mode: true,
                ..Default::default()
            }
        )
        .lines[0]
            .spans[0]
            .style
            .fg,
        Some(Rgb(255, 255, 0))
    );
}

#[test]
fn hints_details_replay_and_typeanswer() {
    let html = "<a class=hint href='#' onclick=\"this.style.display='none';document.getElementById('hint1').style.display='block';return false\">Hint</a><div class=hint id=hint1 style='display:none'>secret</div><details><summary>More</summary>details secret</details>[anki:play:q:2] [anki:play:a:0]<center><input id=typeans></center>";
    let card = side(html, "");
    let closed = render(&card, &RenderOptions::default());
    assert!(!plain(&closed).contains("secret"));
    assert_eq!(closed.widgets.len(), 5);
    assert!(matches!(
        closed.widgets[2].kind,
        WidgetKind::Replay {
            side: AudioSide::Question,
            index: 2
        }
    ));
    let expanded = ["hint:0".into(), "details:0".into()].into();
    let open = render(
        &card,
        &RenderOptions {
            expanded,
            ..Default::default()
        },
    );
    assert!(plain(&open).contains("secret"));
    assert!(plain(&open).contains("details secret"));
    assert_eq!(
        closed.widgets.iter().map(|w| &w.key).collect::<Vec<_>>(),
        open.widgets.iter().map(|w| &w.key).collect::<Vec<_>>()
    );
    let hidden = render(
        &card,
        &RenderOptions {
            hide_audio_buttons: true,
            ..Default::default()
        },
    );
    assert_eq!(hidden.widgets.len(), 3);
    assert!(!plain(&hidden).contains("[anki:"));
    for w in &open.widgets {
        assert!(w.width > 0);
        assert!(w.row < open.lines.len());
        assert!(w.col + w.width <= 80);
    }
}

#[test]
fn editor_html_cell_buffer() {
    let html = "<div align=right>right <b>bold</b> <i>italic</i> <u>under</u> <s>strike</s></div><p><font color='#008000'>green</font> <mark>mark</mark> H<sub>2</sub>O x<sup>2</sup> <sup>Q</sup> <a href='https://example.org'>link</a></p><ol start=3><li>third<ul><li>nested</li></ul></li><li>fourth</li></ol><table><tr><th>A</th><th>B</th></tr><tr><td>one</td><td>two</td></tr></table><hr id=answer><ruby>漢字<rt>かんじ</rt></ruby><div dir=rtl>אבג 123</div><pre> a  b\n c</pre>";
    let options = RenderOptions {
        width: 40,
        ..Default::default()
    };
    let rendered = render(&side(html, ""), &options);
    assert!(rendered.answer_row.is_some());
    assert!(rendered
        .lines
        .iter()
        .flat_map(|l| &l.spans)
        .any(|s| s.link.as_deref() == Some("https://example.org")));
    insta::assert_snapshot!("editor_html", cells(&rendered, options.width));
}

#[test]
fn images_reserve_their_blank_cells_and_use_real_cell_size() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("picture.png");
    image::RgbaImage::new(100, 80).save(&path).unwrap();
    let mut card = side(
        "<img src=picture.png alt=picture style='display:block'>",
        "",
    );
    card.media_dir = dir.path().into();
    let options = RenderOptions {
        images: true,
        width: 40,
        cell_px: Some((10, 20)),
        ..Default::default()
    };
    let rendered = render(&card, &options);
    assert_eq!(rendered.images.len(), 1);
    assert_eq!(
        (rendered.images[0].width, rendered.images[0].height),
        (10, 4)
    );
    assert_eq!(rendered.lines.len(), 4, "trailing image rows are reserved");
    assert!(plain(&rendered).trim().is_empty());
    assert!(plain(&render(&card, &RenderOptions::default())).contains("[image: picture.png]"));
    card.html =
        "<img src=https://example.org/x.png><img src=../secret><img src=missing.png>".into();
    let missing = render(&card, &options);
    assert!(missing.images.is_empty());
    let placeholder_text = plain(&missing)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        placeholder_text.contains("[missing image: missing.png]"),
        "{placeholder_text:?}"
    );
    assert!(missing.notices.iter().any(|n| n.contains("remote")));
}

#[test]
fn stylesheets_are_local_and_svg_is_reserved() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("local.css"),
        ".hide {display:none} .show {font-weight:bold}",
    )
    .unwrap();
    let mut card = side("<link rel=stylesheet href=local.css><span class=hide>secret</span><span class=show>shown</span><svg width=20 height=40><rect width=20 height=40 fill=red /></svg>", "");
    card.media_dir = dir.path().into();
    let rendered = render(
        &card,
        &RenderOptions {
            images: true,
            ..Default::default()
        },
    );
    assert!(!plain(&rendered).contains("secret"));
    assert!(rendered
        .lines
        .iter()
        .flat_map(|l| &l.spans)
        .any(|s| s.text.contains("shown") && s.style.bold));
    assert_eq!(rendered.images.len(), 1);
    assert!(matches!(rendered.images[0].source, ImageSource::Svg(_)));
}

#[cfg(unix)]
#[test]
fn media_symlinks_cannot_escape_media_root() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("escape.png")).unwrap();
    assert!(matches!(
        resolve_media(root.path(), "escape.png"),
        MediaRef::Rejected(_)
    ));
    std::fs::write(root.path().join("inside.png"), "fixture").unwrap();
    std::os::unix::fs::symlink(root.path().join("inside.png"), root.path().join("safe.png"))
        .unwrap();
    assert!(matches!(
        resolve_media(root.path(), "safe.png"),
        MediaRef::Rejected(_)
    ));
}

#[test]
fn widths_and_widget_indices_hold_for_malformed_unicode_html() {
    for width in 1..=40 {
        for html in [
            "<p>one <b>two<div>three",
            "你好 🇬🇧 👨‍👩‍👧‍👦 e\u{301} <sub>2</sub><ruby>漢<rt>かん</rt></ruby>",
            "[anki:play:🌍:1] [anki:play:q:] [anki:play:a:9999999999999999999999999]",
            "<table><tr><td>longlonglong<td>界界<tr><td colspan=2>x</table>",
            "<details open><summary>long summary text</summary><ul><li>test</ul></details>",
        ] {
            let card = render(
                &side(html, ""),
                &RenderOptions {
                    width,
                    ..Default::default()
                },
            );
            for line in &card.lines {
                let text: String = line.spans.iter().map(|s| s.text.as_str()).collect();
                assert!(text.width() <= width as usize, "width={width}: {text:?}");
                assert!(line
                    .spans
                    .iter()
                    .all(|s| s.widget.is_none_or(|i| i < card.widgets.len())));
            }
        }
    }
}

#[test]
fn paint_clears_old_foreground_and_scrolls_inside_area() {
    let card = render(
        &side("one<br><b>two</b><br>three", ""),
        &RenderOptions::default(),
    );
    let mut buf = Buffer::empty(Rect::new(0, 0, 12, 4));
    for cell in &mut buf.content {
        cell.set_symbol("x").set_fg(Color::Red).set_bg(Color::Blue);
    }
    paint(&card, 1, Rect::new(2, 1, 6, 2), &mut buf);
    assert_eq!(buf[(2, 1)].symbol(), "t");
    assert!(buf[(2, 1)].modifier.contains(Modifier::BOLD));
    assert_eq!(buf[(5, 1)].symbol(), " ");
    assert_eq!(buf[(5, 1)].fg, Color::Reset);
    assert_eq!(buf[(5, 1)].bg, Color::Blue);
    assert_eq!(buf[(0, 0)].symbol(), "x");
}

#[test]
fn terminal_controls_never_reach_text_cells() {
    // Para::push_text already strips terminal controls after consuming the
    // whitespace used for layout; HTML parsing alone is not the safety boundary.
    let card = render(
        &side("<p>hello\x1b[2Jworld\x07</p>", ""),
        &RenderOptions::default(),
    );
    assert!(card
        .lines
        .iter()
        .flat_map(|l| &l.spans)
        .all(|s| !s.text.chars().any(char::is_control)));
    assert_eq!(plain(&card).trim(), "hello[2Jworld");

    let controls: String = ('\u{1}'..='\u{9f}')
        .filter(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let card = render(
        &side(&format!("<div>before{controls}after</div>"), ""),
        &RenderOptions::default(),
    );
    assert!(card
        .lines
        .iter()
        .flat_map(|l| &l.spans)
        .all(|s| !s.text.chars().any(char::is_control)));
    let area = Rect::new(0, 0, 80, card.lines.len() as u16);
    let mut buffer = Buffer::empty(area);
    paint(&card, 0, area, &mut buffer);
    assert!(buffer
        .content
        .iter()
        .all(|c| !c.symbol().chars().any(char::is_control)));

    // Layout consumes tabs and line breaks; Unicode format/combining characters
    // are not terminal controls and must survive in the resulting graphemes.
    let card = render(
        &side("<pre>A\tB\nC e\u{301} 👨‍👩‍👧‍👦</pre>", ""),
        &RenderOptions::default(),
    );
    assert_eq!(card.lines.len(), 2);
    assert!(plain(&card).starts_with("A       B\nC e\u{301} 👨‍👩‍👧‍👦"));
    assert!(card
        .lines
        .iter()
        .flat_map(|l| &l.spans)
        .all(|s| !s.text.chars().any(char::is_control)));
}
