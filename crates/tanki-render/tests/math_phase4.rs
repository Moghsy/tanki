use ratatui::{buffer::Buffer, layout::Rect};
use tanki_render::*;

fn side(html: &str) -> CardSide {
    CardSide {
        html: html.into(),
        ..Default::default()
    }
}
fn text(card: &RenderedCard) -> String {
    card.lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.text.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn simple_inline_unicode_and_unsupported_formula_snapshot() {
    let options = RenderOptions {
        width: 40,
        ..Default::default()
    };
    let card = render(
        &side(r"<div>Energy \(E=mc^2\); \(\alpha + \beta\).</div><div>\(\unknown{x}\)</div>"),
        &options,
    );
    assert!(card.images.is_empty());
    let area = Rect::new(0, 0, 40, card.lines.len() as u16);
    let mut buf = Buffer::empty(area);
    paint(&card, 0, area, &mut buf);
    let snapshot = (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!("math_unicode_source", snapshot);
    assert!(text(&card).contains("E=mc²"));
    assert!(text(&card).contains(r"\unknown{x}"));
}

#[test]
fn tall_inline_and_display_math_reserve_image_cells() {
    let card = render(
        &side(r"before \(\frac{1}{x}\) after <div>\[\sum_{i=0}^n i\]</div>"),
        &RenderOptions {
            images: true,
            width: 40,
            ..Default::default()
        },
    );
    assert_eq!(card.images.len(), 2);
    assert!(card.images[0].height >= 2);
    assert!(text(&card).contains("before"));
    assert!(text(&card).contains("after"));
    assert!(card.notices.is_empty(), "{:?}", card.notices);
    for image in &card.images {
        assert!(image.col + image.width <= 40);
        assert!(image.row + usize::from(image.height) <= card.lines.len());
        assert!(matches!(&image.source, ImageSource::Svg(svg) if svg.contains("<path")));
    }
}

#[test]
fn legacy_latex_uses_night_inversion_without_altering_other_images() {
    let dir = tempfile::tempdir().unwrap();
    image::RgbaImage::from_pixel(3, 3, image::Rgba([0, 0, 0, 255]))
        .save(dir.path().join("latex.png"))
        .unwrap();
    let side = CardSide {
        html: r#"<img class="latex" alt="x" src="latex.png"><img src="latex.png">"#.into(),
        media_dir: dir.path().into(),
        ..Default::default()
    };
    let card = render(
        &side,
        &RenderOptions {
            images: true,
            night_mode: true,
            ..Default::default()
        },
    );
    assert!(matches!(
        card.images[0].source,
        ImageSource::InvertedFile(_)
    ));
    assert!(matches!(card.images[1].source, ImageSource::File(_)));
}
