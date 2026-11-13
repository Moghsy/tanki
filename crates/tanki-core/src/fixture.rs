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
            (
                "Front",
                r"Missing image: [latex]\int_0^1 x\,dx[/latex]".into(),
            ),
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

// ---------------------------------------------------------------------------
// Study fixture

/// Top deck of the study fixture; `populate_study` makes it the current deck.
pub(crate) const STUDY_DECK: &str = "Study";
/// Filtered deck of the study fixture (outside the "Study" tree).
pub(crate) const STUDY_FILTERED_DECK: &str = "Study Cram";

/// Decks of the study fixture: (name, preset name or "" for the default
/// preset, description, description is Markdown).
const STUDY_DECKS: &[(&str, &str, &str, bool)] = &[
    (
        STUDY_DECK,
        "",
        "Welcome to the **tanki** study fixture.\n\n- new, learning and review cards\n- images and audio in *Study::Media*",
        true,
    ),
    ("Study::Vocabulary", "", "", false),
    (
        "Study::Vocabulary::French",
        "Study: French",
        "Siblings are buried.",
        false,
    ),
    ("Study::Vocabulary::Japanese", "", "", false),
    (
        "Study::Science",
        "Study: Science",
        "<i>Leeches</i> are suspended after one lapse.",
        false,
    ),
    ("Study::Media", "Study: Media", "", false),
    ("Study::Later", "", "", false),
];

/// One note of the study fixture: deck, stock kind, fields by name, tags.
/// Tags under `study::` set up its cards' state (see `populate_study`).
struct StudyNote {
    deck: &'static str,
    kind: OriginalStockKind,
    fields: &'static [(&'static str, &'static str)],
    tags: &'static [&'static str],
}

/// `{png}`, `{svg}` and `{wav}` in field text are replaced with the media
/// filenames Anki returned.
const STUDY_NOTES: &[StudyNote] = &[
    StudyNote {
        deck: STUDY_DECK,
        kind: OriginalStockKind::Basic,
        fields: &[
            ("Front", "Welcome to tanki"),
            ("Back", "This deck tests studying."),
        ],
        tags: &[],
    },
    StudyNote {
        deck: STUDY_DECK,
        kind: OriginalStockKind::Basic,
        fields: &[
            ("Front", "What does <b>Study Now</b> do?"),
            ("Back", "Shows the next due card."),
        ],
        tags: &["study::review-3"],
    },
    StudyNote {
        deck: "Study::Vocabulary::French",
        kind: OriginalStockKind::BasicAndReversed,
        fields: &[("Front", "le chat"), ("Back", "the cat")],
        tags: &["study::review-7"],
    },
    StudyNote {
        deck: "Study::Vocabulary::French",
        kind: OriginalStockKind::BasicAndReversed,
        fields: &[("Front", "le chien"), ("Back", "the dog")],
        tags: &["study::again"],
    },
    StudyNote {
        deck: "Study::Vocabulary::French",
        kind: OriginalStockKind::BasicAndReversed,
        fields: &[("Front", "la maison"), ("Back", "the house")],
        tags: &["study::relearn"],
    },
    StudyNote {
        deck: "Study::Vocabulary::French",
        kind: OriginalStockKind::BasicAndReversed,
        fields: &[("Front", "le livre"), ("Back", "the book")],
        tags: &["study::flag"],
    },
    StudyNote {
        deck: "Study::Vocabulary::French",
        kind: OriginalStockKind::BasicAndReversed,
        fields: &[("Front", "l'arbre"), ("Back", "the tree")],
        tags: &[],
    },
    StudyNote {
        deck: "Study::Vocabulary::French",
        kind: OriginalStockKind::BasicAndReversed,
        fields: &[("Front", "la pomme"), ("Back", "the apple")],
        tags: &["Marked"],
    },
    StudyNote {
        deck: "Study::Vocabulary::Japanese",
        kind: OriginalStockKind::BasicTyping,
        fields: &[("Front", "猫"), ("Back", "ねこ")],
        tags: &[],
    },
    StudyNote {
        deck: "Study::Vocabulary::Japanese",
        kind: OriginalStockKind::BasicTyping,
        fields: &[("Front", "犬"), ("Back", "いぬ")],
        tags: &["study::good"],
    },
    StudyNote {
        deck: "Study::Vocabulary::Japanese",
        kind: OriginalStockKind::BasicTyping,
        fields: &[("Front", "水"), ("Back", "みず")],
        tags: &["study::review-15"],
    },
    StudyNote {
        deck: "Study::Vocabulary::Japanese",
        kind: OriginalStockKind::BasicTyping,
        fields: &[("Front", "火"), ("Back", "ひ")],
        tags: &["study::hard"],
    },
    StudyNote {
        deck: "Study::Science",
        kind: OriginalStockKind::Cloze,
        fields: &[
            ("Text", "{{c1::Water}} boils at {{c2::100 °C}} at sea level."),
            ("Back Extra", "At 1 atm."),
        ],
        tags: &["study::review-40"],
    },
    StudyNote {
        deck: "Study::Science",
        kind: OriginalStockKind::Cloze,
        fields: &[("Text", "The {{c1::mitochondrion}} is the powerhouse of the cell.")],
        tags: &["study::again"],
    },
    StudyNote {
        deck: "Study::Science",
        kind: OriginalStockKind::Cloze,
        fields: &[(
            "Text",
            "{{c1::Hydrogen::element}} is the first element, {{c2::helium}} the second.",
        )],
        tags: &[],
    },
    StudyNote {
        deck: "Study::Science",
        kind: OriginalStockKind::Basic,
        fields: &[("Front", "Speed of light?"), ("Back", "≈ 3×10<sup>8</sup> m/s")],
        tags: &["study::review-100"],
    },
    StudyNote {
        deck: "Study::Science",
        kind: OriginalStockKind::Basic,
        fields: &[
            ("Front", "Avogadro constant?"),
            ("Back", "6.022×10<sup>23</sup> mol<sup>−1</sup>"),
        ],
        tags: &["study::cram"],
    },
    StudyNote {
        deck: "Study::Science",
        kind: OriginalStockKind::Basic,
        fields: &[
            ("Front", "Planck constant?"),
            ("Back", "6.626×10<sup>−34</sup> J·s"),
        ],
        tags: &["study::cram"],
    },
    StudyNote {
        deck: "Study::Media",
        kind: OriginalStockKind::Basic,
        fields: &[
            ("Front", "<img src=\"{png}\"><br>Which colours does this gradient run between?"),
            ("Back", "Blue to green [sound:{wav}]"),
        ],
        tags: &[],
    },
    StudyNote {
        deck: "Study::Media",
        kind: OriginalStockKind::Basic,
        fields: &[
            ("Front", "Listen: [sound:{wav}]"),
            ("Back", "A 440 Hz tone (A4)"),
        ],
        tags: &["study::review-1"],
    },
    StudyNote {
        deck: "Study::Media",
        kind: OriginalStockKind::Basic,
        fields: &[("Front", "<img src=\"{svg}\">"), ("Back", "A blue square")],
        tags: &["study::suspended"],
    },
    StudyNote {
        deck: "Study::Media",
        kind: OriginalStockKind::BasicOptionalReversed,
        fields: &[
            ("Front", "<img src=\"{png}\">"),
            ("Back", "A gradient"),
            ("Add Reverse", "y"),
        ],
        tags: &["study::buried"],
    },
    StudyNote {
        deck: "Study::Media",
        kind: OriginalStockKind::ImageOcclusion,
        fields: &[
            (
                "Occlusion",
                "{{c1::image-occlusion:rect:left=.1:top=.2:width=.3:height=.5:oi=1}}{{c2::image-occlusion:rect:left=.6:top=.2:width=.3:height=.5:oi=1}}",
            ),
            ("Image", "<img src=\"{png}\">"),
            ("Header", "Gradient regions"),
        ],
        tags: &[],
    },
    StudyNote {
        deck: "Study::Later",
        kind: OriginalStockKind::Basic,
        fields: &[("Front", "Due in five days"), ("Back", "Not today")],
        tags: &["study::later"],
    },
];

/// Builds the study fixture: the decks above with three presets, the notes
/// above, media (a PNG gradient, an SVG and a WAV tone), and these states,
/// all set through Anki's own operations:
///
/// - `study::review-N`: review cards with interval N due today (`SetDueDate`
///   "N", then "0", which keeps the interval);
/// - `study::later`: a review card due in five days;
/// - `study::again` / `study::hard` / `study::good`: first card answered once
///   now with that rating (`GradeNow`), so it is in (intraday) learning;
/// - `study::relearn`: first card made a due review, then answered Again, so
///   it is relearning;
/// - `study::suspended`, `study::buried` (second card, manually buried);
/// - `study::flag`: first card has the orange flag; `Marked` is the mark tag
///   in another case;
/// - `study::cram`: pulled into the filtered deck "Study Cram".
///
/// "Study: French" buries new and review siblings, "Study: Science" makes a
/// card a leech on its first lapse and suspends it (and turns autoplay off),
/// "Study: Media" changes the reviewer settings. "Study" (Markdown description) becomes current.
pub(crate) fn populate_study(col: &mut Collection) -> Result<()> {
    use anki::services::{
        CardsService, DeckConfigService, DecksService, SchedulerService, SearchService,
    };
    let png = MediaService::add_media_file(
        col,
        pb::media::AddMediaFileRequest {
            desired_name: "study-gradient.png".into(),
            data: gradient_png(96, 64),
        },
    )?
    .val;
    let svg = MediaService::add_media_file(
        col,
        pb::media::AddMediaFileRequest {
            desired_name: "study-square.svg".into(),
            data: br##"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="80"><rect width="120" height="80" fill="#3366cc"/><circle cx="60" cy="40" r="25" fill="#ffcc00"/></svg>"##.to_vec(),
        },
    )?
    .val;
    let wav = MediaService::add_media_file(
        col,
        pb::media::AddMediaFileRequest {
            desired_name: "study-tone.wav".into(),
            data: tone_wav(440.0, 0.6),
        },
    )?
    .val;

    // Presets, from the default preset's legacy JSON.
    let json_error = |e: serde_json::Error| CoreError::Other(format!("Preset JSON: {e}"));
    let default: serde_json::Value = serde_json::from_slice(
        &DeckConfigService::get_deck_config_legacy(col, pb::deck_config::DeckConfigId { dcid: 1 })?
            .json,
    )
    .map_err(json_error)?;
    let presets: &[(&str, &[(&str, serde_json::Value)])] = &[
        (
            "Study: French",
            &[
                ("new.perDay", 6.into()),
                ("new.bury", true.into()),
                ("rev.bury", true.into()),
            ],
        ),
        (
            "Study: Science",
            &[
                ("lapse.leechFails", 1.into()),
                // LEECH_ACTION_SUSPEND
                ("lapse.leechAction", 0.into()),
                // Tells this preset apart in `StudyConfig` tests.
                ("autoplay", false.into()),
            ],
        ),
        (
            "Study: Media",
            &[
                ("timer", 1.into()),
                ("maxTaken", 30.into()),
                ("replayq", false.into()),
                ("stopTimerOnAnswer", true.into()),
                ("secondsToShowQuestion", 20.0.into()),
                ("secondsToShowAnswer", 10.0.into()),
                // QUESTION_ACTION_SHOW_REMINDER, ANSWER_ACTION_ANSWER_HARD
                ("questionAction", 1.into()),
                ("answerAction", 3.into()),
                ("waitForAudio", false.into()),
            ],
        ),
    ];
    let mut preset_ids = std::collections::HashMap::new();
    for (name, changes) in presets {
        let mut conf = default.clone();
        conf["id"] = 0.into();
        conf["name"] = (*name).into();
        for (path, value) in changes.iter() {
            let mut target = &mut conf;
            for part in path.split('.') {
                target = &mut target[part];
            }
            *target = value.clone();
        }
        let id = DeckConfigService::add_or_update_deck_config_legacy(
            col,
            pb::generic::Json {
                json: serde_json::to_vec(&conf).map_err(json_error)?,
            },
        )?
        .dcid;
        preset_ids.insert(*name, id);
    }

    let mut deck_ids = std::collections::HashMap::new();
    for (name, preset, description, markdown) in STUDY_DECKS {
        let mut deck = DecksService::new_deck(col)?;
        deck.name = (*name).into();
        if let Some(pb::decks::deck::Kind::Normal(normal)) = deck.kind.as_mut() {
            normal.description = (*description).into();
            normal.markdown_description = *markdown;
            if !preset.is_empty() {
                normal.config_id = preset_ids[preset];
            }
        }
        let id = DecksService::add_deck(col, deck)?.id;
        deck_ids.insert(*name, id);
    }

    for spec in STUDY_NOTES {
        let nt = stock(col, spec.kind)?;
        let mut note = NotesService::new_note(col, pb::notetypes::NotetypeId { ntid: nt.id.0 })?;
        for (field, value) in spec.fields {
            let index = nt
                .fields
                .iter()
                .position(|f| f.name == *field)
                .ok_or_else(|| CoreError::Other(format!("{} has no field {field}", nt.name)))?;
            note.fields[index] = value
                .replace("{png}", &png)
                .replace("{svg}", &svg)
                .replace("{wav}", &wav);
        }
        note.tags = spec.tags.iter().map(|t| (*t).to_owned()).collect();
        let _ = NotesService::add_note(
            col,
            pb::notes::AddNoteRequest {
                note: Some(note),
                deck_id: deck_ids[spec.deck],
            },
        )?;
    }

    let search = |col: &mut Collection, query: &str| -> Result<Vec<i64>> {
        let ids = SearchService::search_cards(
            col,
            pb::search::SearchRequest {
                search: query.into(),
                order: None,
            },
        )?
        .ids;
        if ids.is_empty() {
            return Err(CoreError::Other(format!(
                "Study fixture: no card matches {query}"
            )));
        }
        Ok(ids)
    };
    let set_due = |col: &mut Collection, card_ids: Vec<i64>, days: &str| -> Result<()> {
        let _ = SchedulerService::set_due_date(
            col,
            pb::scheduler::SetDueDateRequest {
                card_ids,
                days: days.into(),
                config_key: None,
            },
        )?;
        Ok(())
    };
    let grade = |col: &mut Collection, card_ids: Vec<i64>, rating: i32| -> Result<()> {
        let _ =
            SchedulerService::grade_now(col, pb::scheduler::GradeNowRequest { card_ids, rating })?;
        Ok(())
    };
    for days in [1, 3, 7, 15, 40, 100] {
        let ids = search(col, &format!("tag:study::review-{days}"))?;
        set_due(col, ids.clone(), &days.to_string())?;
        set_due(col, ids, "0")?;
    }
    let later = search(col, "tag:study::later")?;
    set_due(col, later, "5")?;
    let relearn = search(col, "tag:study::relearn card:1")?;
    set_due(col, relearn.clone(), "4")?;
    set_due(col, relearn.clone(), "0")?;
    grade(col, relearn, 0)?;
    for (tag, rating) in [("again", 0), ("hard", 1), ("good", 2)] {
        let ids = search(col, &format!("tag:study::{tag} card:1"))?;
        grade(col, ids, rating)?;
    }
    let suspended = search(col, "tag:study::suspended")?;
    let _ = SchedulerService::bury_or_suspend_cards(
        col,
        pb::scheduler::BuryOrSuspendCardsRequest {
            card_ids: suspended,
            note_ids: vec![],
            mode: pb::scheduler::bury_or_suspend_cards_request::Mode::Suspend as i32,
        },
    )?;
    let buried = search(col, "tag:study::buried card:2")?;
    let _ = SchedulerService::bury_or_suspend_cards(
        col,
        pb::scheduler::BuryOrSuspendCardsRequest {
            card_ids: buried,
            note_ids: vec![],
            mode: pb::scheduler::bury_or_suspend_cards_request::Mode::BuryUser as i32,
        },
    )?;
    let flagged = search(col, "tag:study::flag card:1")?;
    let _ = CardsService::set_flag(
        col,
        pb::cards::SetFlagRequest {
            card_ids: flagged,
            flag: 2,
        },
    )?;

    let mut filtered =
        DecksService::get_or_create_filtered_deck(col, pb::decks::DeckId { did: 0 })?;
    filtered.name = STUDY_FILTERED_DECK.into();
    let config = filtered
        .config
        .as_mut()
        .ok_or_else(|| CoreError::Other("New filtered deck has no config".into()))?;
    config.reschedule = true;
    config.search_terms = vec![pb::decks::deck::filtered::SearchTerm {
        search: "tag:study::cram".into(),
        limit: 100,
        order: pb::decks::deck::filtered::search_term::Order::Added as i32,
    }];
    let _ = DecksService::add_or_update_filtered_deck(col, filtered)?;

    let _ = DecksService::set_current_deck(
        col,
        pb::decks::DeckId {
            did: deck_ids[STUDY_DECK],
        },
    )?;
    Ok(())
}

/// A `width`×`height` RGB PNG running from blue (left) to green (right),
/// darker towards the bottom, written with stored (uncompressed) deflate
/// blocks so no compressor is needed.
fn gradient_png(width: u32, height: u32) -> Vec<u8> {
    let mut raw = Vec::with_capacity(((width * 3 + 1) * height) as usize);
    for y in 0..height {
        raw.push(0); // filter: none
        let shade = 255 - y * 96 / height.max(1);
        for x in 0..width {
            let t = x * 255 / (width - 1).max(1);
            raw.extend_from_slice(&[40, (t * shade / 255) as u8, ((255 - t) * shade / 255) as u8]);
        }
    }
    let mut zlib = vec![0x78, 0x01];
    let mut blocks = raw.chunks(0xffff).peekable();
    while let Some(block) = blocks.next() {
        zlib.push(u8::from(blocks.peek().is_none()));
        let len = block.len() as u16;
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&(!len).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in &raw {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8], data: &[u8]| {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = png.len();
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        let crc = crc32(&png[start..]);
        png.extend_from_slice(&crc.to_be_bytes());
    };
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB
    chunk(b"IHDR", &header);
    chunk(b"IDAT", &zlib);
    chunk(b"IEND", &[]);
    png
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// A mono 16-bit 8 kHz PCM WAV sine tone with short fades.
fn tone_wav(frequency: f32, seconds: f32) -> Vec<u8> {
    const RATE: u32 = 8000;
    let samples = (RATE as f32 * seconds) as u32;
    let fade = RATE / 50;
    let mut data = Vec::with_capacity(samples as usize * 2);
    for i in 0..samples {
        let envelope = (i.min(samples - 1 - i).min(fade) as f32) / fade as f32;
        let t = i as f32 / RATE as f32;
        let value = (std::f32::consts::TAU * frequency * t).sin() * 9000.0 * envelope;
        data.extend_from_slice(&(value as i16).to_le_bytes());
    }
    let mut wav = b"RIFF".to_vec();
    wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
    wav.extend_from_slice(&data);
    wav
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_reference() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b"IEND"), 0xae42_6082);
    }

    #[test]
    fn png_and_wav_are_well_formed() {
        let png = gradient_png(96, 64);
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(png.ends_with(&[0xae, 0x42, 0x60, 0x82]));
        assert_eq!(&png[12..16], b"IHDR");
        let wav = tone_wav(440.0, 0.6);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(wav.len(), 44 + 4800 * 2);
        assert_eq!(
            u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize,
            wav.len() - 8
        );
    }
}
