//! Synthetic fixture collection.
//!
//! Every note carries one `golden::<label>` tag. Together with the note type
//! name and the template ordinal it names each card in the golden render
//! files (`tests/fixtures/golden/`), so those files never depend on ids or
//! timestamps. Changing anything here changes the expected renders: regenerate
//! the golden files with the command at the top of `scripts/golden-render.py`.
use crate::{CoreError, Result};
use anki::{
    collection::Collection,
    notetype::{CardTemplate, NoteField, Notetype, NotetypeConfig},
    services::{CardRenderingService, MediaService, NotesService},
};
use anki_proto as pb;
use pb::notetypes::stock_notetype::OriginalStockKind;

/// Name of the custom note type that exercises the Python-side render steps.
const GOLDEN_NOTETYPE: &str = "tanki Golden";
/// Name of the custom note type whose answer template fails to render.
const BROKEN_NOTETYPE: &str = "tanki Broken";
/// Body of the `[latex]` snippet whose image is shipped in the media folder.
const SHIPPED_LATEX: &str = "E=mc^2";

/// A 1x1 transparent PNG standing in for a pre-rendered LaTeX image.
const LATEX_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x60, 0x00, 0x02, 0x00,
    0x00, 0x05, 0x00, 0x01, 0x7a, 0x5e, 0xab, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44,
    0xae, 0x42, 0x60, 0x82,
];

/// Templates of [`GOLDEN_NOTETYPE`]: (name, front, back).
const GOLDEN_TEMPLATES: &[(&str, &str, &str)] = &[
    (
        // {{FrontSide}} with audio on the front, and a [sound:] on both sides.
        "Audio",
        "{{Front}}",
        "{{FrontSide}}\n\n<hr id=answer>\n\n{{Back}}",
    ),
    (
        // TTS on the front (carried into the answer by FrontSide), unknown
        // custom filters, an unknown filter before a built-in one, an empty
        // field name with a filter, and a hint.
        "Speech and filters",
        "{{tts en_US:Front}}\n<div>{{tts ja_JP voices=Apple_Kyoko,Microsoft_Haruka speed=0.8:Reading}}</div>\n<div>{{somefilter:Back}}</div>\n<div>{{text:somefilter:Extra}}</div>",
        "{{FrontSide}}\n\n<hr id=answer>\n\n{{hint:Extra}}\n<div>{{text:Back}}</div>{{somefilter:}}",
    ),
    (
        "Furigana",
        "{{furigana:Reading}}",
        "{{FrontSide}}\n\n<hr id=answer>\n\n{{kanji:Reading}}<br>{{kana:Reading}}",
    ),
    (
        "Typing",
        "{{Front}}\n\n{{type:Back}}",
        "{{Front}}\n\n<hr id=answer>\n\n{{type:Back}}",
    ),
    (
        // Conditionals, MathJax and [latex], and the special fields that do
        // not depend on ids.
        "Conditional",
        "{{#Extra}}<div class=extra>{{Extra}}</div>{{/Extra}}{{^Extra}}<div class=noextra>no extra</div>{{/Extra}}\n{{text:Front}}",
        "{{FrontSide}}\n\n<hr id=answer>\n\n{{#Math}}{{Math}}{{/Math}}{{^Math}}<i>no math</i>{{/Math}}\n<div class=meta>{{Type}} | {{Card}} | {{Deck}} | {{Subdeck}} | {{Tags}} | {{CardFlag}}</div>",
    ),
];

const GOLDEN_CSS: &str = ".card {\n    font-family: serif;\n    font-size: 18px;\n    text-align: left;\n}\n.card3 rt {\n    color: gray;\n}\n";

pub(crate) fn populate(col: &mut Collection) -> Result<()> {
    // Synthetic SVG and short silent PCM WAV; names returned by Anki are authoritative.
    let image = MediaService::add_media_file(col, pb::media::AddMediaFileRequest { desired_name:"fixture.svg".into(), data:br##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><rect width="100" height="100" fill="#3366cc"/></svg>"##.to_vec() })?.val;
    let mut wav = b"RIFF".to_vec();
    wav.extend_from_slice(&196u32.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&8000u32.to_le_bytes());
    wav.extend_from_slice(&16000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&160u32.to_le_bytes());
    wav.resize(204, 0);
    let sound = MediaService::add_media_file(
        col,
        pb::media::AddMediaFileRequest {
            desired_name: "fixture.wav".into(),
            data: wav,
        },
    )?
    .val;
    // One note per stock note type.
    for nt in col.get_all_notetypes()? {
        let mut note = NotesService::new_note(col, pb::notetypes::NotetypeId { ntid: nt.id.0 })?;
        for (i, field) in nt.fields.iter().enumerate() {
            note.fields[i] = match field.name.as_str() {
                "Front" => format!("<b>Question</b> <img src=\"{image}\"> [sound:{sound}]"),
                "Back" | "Back Extra" => format!("Answer [sound:{sound}]"),
                "Text" => "A {{c1::cloze}} and {{c2::second}}".into(),
                "Add Reverse" => "yes".into(),
                "Occlusion" => {
                    "{{c1::image-occlusion:rect:left=.1:top=.1:width=.4:height=.4:oi=1}}".into()
                }
                "Image" => format!("<img src=\"{image}\">"),
                "Header" => "Synthetic image occlusion".into(),
                _ => String::new(),
            };
        }
        note.tags = vec![label("stock")];
        let _ = NotesService::add_note(
            col,
            pb::notes::AddNoteRequest {
                note: Some(note),
                deck_id: 1,
            },
        )?;
    }

    // A pre-rendered LaTeX image, stored under the name Anki derives from the
    // snippet, so pylib's "image already present" path is covered. The name
    // Anki returns is used as-is; if it ever differed, the render would show
    // the missing-image path instead and the golden files would say so.
    let shipped = format!("[latex]{SHIPPED_LATEX}[/latex]");
    let latex_name = CardRenderingService::extract_latex(
        col,
        pb::card_rendering::ExtractLatexRequest {
            text: shipped.clone(),
            svg: false,
            expand_clozes: false,
        },
    )?
    .latex
    .into_iter()
    .next()
    .ok_or_else(|| CoreError::Other("Anki found no LaTeX in the fixture snippet".into()))?
    .filename;
    let _ = MediaService::add_media_file(
        col,
        pb::media::AddMediaFileRequest {
            desired_name: latex_name,
            data: LATEX_PNG.to_vec(),
        },
    )?;

    // Extra notes on stock note types.
    let basic = stock(col, OriginalStockKind::Basic)?;
    add_note(
        col,
        &basic,
        "latex-missing",
        &[
            ("Front", r"Missing image: [latex]\int_0^1 x\,dx[/latex]".into()),
            ("Back", "[$]a+b[/$] and [$$]c[/$$]".into()),
        ],
    )?;
    let cloze = stock(col, OriginalStockKind::Cloze)?;
    add_note(
        col,
        &cloze,
        "cloze-hint",
        &[
            (
                "Text",
                r"{{c1::Paris::capital city}} is in {{c2::France}}; \(y = {{c2::mx}} + b\) and {{c1::\(x^2\)}}"
                    .into(),
            ),
            ("Back Extra", format!("Extra [sound:{sound}]")),
        ],
    )?;

    // Custom note type covering the steps pylib adds after the Rust engine.
    let mut golden = Notetype {
        name: GOLDEN_NOTETYPE.into(),
        fields: ["Front", "Back", "Reading", "Extra", "Math"]
            .into_iter()
            .map(NoteField::new)
            .collect(),
        templates: GOLDEN_TEMPLATES
            .iter()
            .map(|(name, front, back)| CardTemplate::new(*name, *front, *back))
            .collect(),
        config: custom_config(&basic, GOLDEN_CSS),
        ..Default::default()
    };
    col.add_notetype(&mut golden, false)?;
    add_note(
        col,
        &golden,
        "full",
        &[
            ("Front", format!("<b>Bonjour</b> [sound:{sound}]")),
            (
                "Back",
                format!("Hello &amp; welcome [sound:{sound}] [sound:folder/nested.mp3]"),
            ),
            ("Reading", "日本語[にほんご]を 勉強[べんきょう]する".into()),
            ("Extra", "<i>extra</i> detail".into()),
            (
                "Math",
                format!(r"Inline \(x^2 + y^2\), display \[\frac{{a}}{{b}}\], {shipped}"),
            ),
        ],
    )?;
    // Empty optional fields: negated conditionals, and no Furigana card.
    add_note(
        col,
        &golden,
        "sparse",
        &[
            ("Front", "Sparse front".into()),
            ("Back", "Sparse back".into()),
        ],
    )?;

    // A note type whose answer names a missing field, which Anki only reports
    // when rendering (desktop shows the error as the card text).
    let mut broken = Notetype {
        name: BROKEN_NOTETYPE.into(),
        fields: ["Front", "Back"].into_iter().map(NoteField::new).collect(),
        templates: vec![CardTemplate::new(
            "Card 1",
            "{{Front}}",
            "{{FrontSide}}\n\n<hr id=answer>\n\n{{Missing}}",
        )],
        config: custom_config(&basic, &basic.config.css),
        ..Default::default()
    };
    col.add_notetype(&mut broken, true)?;
    add_note(
        col,
        &broken,
        "broken",
        &[
            ("Front", format!("Broken [sound:{sound}]")),
            ("Back", "Never shown".into()),
        ],
    )?;
    Ok(())
}

fn label(name: &str) -> String {
    format!("golden::{name}")
}

fn stock(col: &mut Collection, kind: OriginalStockKind) -> Result<Notetype> {
    col.get_all_notetypes()?
        .into_iter()
        .find(|nt| nt.config.original_stock_kind == kind as i32)
        .map(|nt| (*nt).clone())
        .ok_or_else(|| CoreError::Other(format!("No stock note type of kind {kind:?}")))
}

fn custom_config(base: &Notetype, css: &str) -> NotetypeConfig {
    NotetypeConfig {
        css: css.into(),
        original_stock_kind: OriginalStockKind::Unknown as i32,
        original_id: None,
        reqs: vec![],
        ..base.config.clone()
    }
}

/// Adds a note with the named fields set (others empty) and its label tag.
fn add_note(
    col: &mut Collection,
    nt: &Notetype,
    name: &str,
    values: &[(&str, String)],
) -> Result<()> {
    let mut note = NotesService::new_note(col, pb::notetypes::NotetypeId { ntid: nt.id.0 })?;
    for (field, value) in values {
        let index = nt
            .fields
            .iter()
            .position(|f| f.name == *field)
            .ok_or_else(|| CoreError::Other(format!("{} has no field {field}", nt.name)))?;
        note.fields[index] = value.clone();
    }
    note.tags = vec![label(name)];
    let _ = NotesService::add_note(
        col,
        pb::notes::AddNoteRequest {
            note: Some(note),
            deck_id: 1,
        },
    )?;
    Ok(())
}
