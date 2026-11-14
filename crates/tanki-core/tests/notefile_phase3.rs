//! Note file round trips over the field corpus (first half of the Phase 3
//! exit test), deterministic property tests, and the planning helper.
use tanki_core::notefile::{self, FileNote, PlanContext};
use tanki_core::*;

#[derive(serde::Deserialize)]
struct Case {
    name: String,
    source: String,
    html: String,
}

fn corpus() -> Vec<Case> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/field-corpus/corpus.json");
    let cases: Vec<Case> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert!(cases.len() >= 80);
    cases
}

fn note_with(fields: Vec<(String, String)>) -> FileNote {
    FileNote {
        id: Some(1_700_000_000_000),
        mtime: Some(1_700_000_000),
        notetype: "Basic".into(),
        deck: None,
        tags: vec!["t".into()],
        fields,
    }
}

/// What common editors do to a file the user did not touch.
fn editor_mangles(text: &str) -> Vec<(&'static str, String)> {
    let stripped: String = text
        .split('\n')
        .map(|l| l.trim_end_matches([' ', '\t']))
        .collect::<Vec<_>>()
        .join("\n");
    vec![
        ("crlf", text.replace('\n', "\r\n")),
        ("strip trailing whitespace", stripped),
        ("no final newline", text.trim_end_matches('\n').to_owned()),
        ("extra final newlines", format!("{text}\n\n")),
        ("bom", format!("\u{feff}{text}")),
        (
            "blank lines around headings",
            text.replace("\n## ", "\n\n\n## "),
        ),
    ]
}

#[test]
fn every_corpus_value_round_trips_byte_for_byte() {
    let cases = corpus();
    for case in &cases {
        for position in 0..2 {
            let mut fields = vec![
                ("Front".to_owned(), "x".to_owned()),
                ("Back".to_owned(), "y".to_owned()),
            ];
            fields[position].1 = case.html.clone();
            let notes = vec![note_with(fields)];
            let text = notefile::write(&notes);
            assert_eq!(
                notefile::parse(&text).as_ref(),
                Ok(&notes),
                "{} ({}) in field {position}:\n{text}",
                case.name,
                case.source
            );
            for (what, mangled) in editor_mangles(&text) {
                assert_eq!(
                    notefile::parse(&mangled).as_ref(),
                    Ok(&notes),
                    "{} after {what}",
                    case.name
                );
            }
        }
    }
    // The whole corpus in one file, one note per case and as one note's fields.
    let many: Vec<FileNote> = cases
        .iter()
        .map(|c| note_with(vec![(c.name.clone(), c.html.clone())]))
        .collect();
    assert_eq!(notefile::parse(&notefile::write(&many)), Ok(many));
    let one = vec![note_with(
        cases
            .iter()
            .map(|c| (c.name.clone(), c.html.clone()))
            .collect(),
    )];
    let text = notefile::write(&one);
    assert_eq!(notefile::parse(&text).as_ref(), Ok(&one));
    for (what, mangled) in editor_mangles(&text) {
        assert_eq!(notefile::parse(&mangled).as_ref(), Ok(&one), "{what}");
    }
}

#[test]
fn ordinary_editor_output_stays_plain() {
    // Only the HTML editor's trailing newline needs the quoted form.
    let quoted = ["html-editor-trailing-newline"];
    for case in corpus().iter().filter(|c| c.source.starts_with("R7")) {
        let text = notefile::write(&[note_with(vec![("F".into(), case.html.clone())])]);
        let is_quoted = text.contains("## F {quoted}");
        let expect_quoted = quoted.contains(&case.name.as_str());
        assert_eq!(is_quoted, expect_quoted, "{}:\n{text}", case.name);
        if !is_quoted && !case.html.is_empty() {
            assert!(
                text.contains(&format!("## F\n{}\n", case.html)),
                "{}",
                case.name
            );
        }
    }
}

/// xorshift64*: deterministic, no dependencies.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

const PIECES: &[&str] = &[
    "a",
    "word",
    " ",
    "  ",
    "\t",
    "\n",
    "\n\n",
    "\r\n",
    "\r",
    "#",
    "# Note",
    "## ",
    "## F",
    "{quoted}",
    "{",
    "}",
    "\"",
    "\\",
    "\\n",
    "\\u0041",
    "[",
    "]",
    ":",
    "id: 1",
    "notetype: X",
    "tags:",
    "```",
    "~~~",
    "---",
    "<div>",
    "</div>",
    "<br>",
    "&nbsp;",
    "\u{a0}",
    "{{c1::",
    "::hint}}",
    "[sound:a.mp3]",
    "\\(x\\)",
    "é",
    "e\u{301}",
    "🙂",
    "\u{200f}",
    "\u{2028}",
    "\u{2029}",
    "\u{feff}",
    "\u{1}",
    "\u{7f}",
    "\u{85}",
    "\u{3000}",
    "\u{1b}",
    "\u{0}",
];

fn random_text(rng: &mut Rng, max: usize) -> String {
    (0..rng.below(max + 1)).map(|_| rng.pick(PIECES)).collect()
}

fn random_name(rng: &mut Rng) -> String {
    // Mostly Anki-valid names; sometimes anything (write quotes those).
    if rng.below(4) == 0 {
        random_text(rng, 4)
    } else {
        rng.pick(&[
            "Front",
            "Back",
            "Back Extra",
            "Text",
            "Ünïcode",
            "a#b",
            "x/y",
            "名前",
        ])
        .to_owned()
    }
}

#[test]
fn random_notes_round_trip() {
    let mut rng = Rng(0x7a4b_1c2d_3e4f_5061);
    for round in 0..3000 {
        let notes: Vec<FileNote> = (0..rng.below(4))
            .map(|_| FileNote {
                id: (rng.below(2) == 0).then(|| rng.next() as i64),
                mtime: (rng.below(2) == 0).then(|| rng.next() as u32),
                notetype: if rng.below(3) == 0 {
                    random_text(&mut rng, 3)
                } else {
                    "Basic".into()
                },
                deck: match rng.below(3) {
                    0 => None,
                    1 => Some("Lang::French".into()),
                    _ => Some(random_text(&mut rng, 3)),
                },
                tags: (0..rng.below(4))
                    .map(|_| {
                        if rng.below(3) == 0 {
                            random_text(&mut rng, 2)
                        } else {
                            rng.pick(&["geo", "a::b", "x_y", "名"]).to_owned()
                        }
                    })
                    .collect(),
                fields: (0..rng.below(5))
                    .map(|_| (random_name(&mut rng), random_text(&mut rng, 12)))
                    .collect(),
            })
            .collect();
        let text = notefile::write(&notes);
        assert_eq!(
            notefile::parse(&text).as_ref(),
            Ok(&notes),
            "round {round}:\n{text}"
        );
        for (what, mangled) in editor_mangles(&text) {
            assert_eq!(
                notefile::parse(&mangled).as_ref(),
                Ok(&notes),
                "round {round} after {what}"
            );
        }
        // No raw CR, control character or line separator reaches the file.
        assert!(!text.contains(|c: char| {
            (c.is_control() && c != '\n' && c != '\t')
                || matches!(c, '\u{2028}' | '\u{2029}' | '\u{feff}')
        }));
    }
}

#[test]
fn parse_errors_are_actionable() {
    let cases = [
        ("Paris\n", 1, "outside a note"),
        ("# Note\nnotetype: Basic\nFront: Paris\n", 3, "unknown key"),
        ("# Note\nnotetype: Basic\nnotetype: Basic\n", 3, "twice"),
        ("# Note\nmtime: yesterday\n", 2, "mtime"),
        ("# Note\nid: 1\n\n## Front\nx\n", 1, "notetype"),
        ("# Note\nnotetype: \"Basic\n", 2, "quoted"),
        ("# Note\nnotetype: B\ntags: [a]\n", 3, "tag list"),
        ("# Note\nnotetype: B\n##\n", 3, "without a name"),
        ("# Note\nnotetype: B\n## F {html}\n", 3, "marker"),
        (
            "# Note\nnotetype: B\n## F {quoted}\n\"a\" \"b\"\n",
            4,
            "quoted",
        ),
        (
            "# Note\nnotetype: B\n## F {quoted}\n\n\nplain\n",
            6,
            "JSON string",
        ),
        ("# Note\nnotetype: B\nsomething\n", 3, "key: value"),
    ];
    for (text, line, words) in cases {
        let e = notefile::parse(text).unwrap_err();
        assert_eq!(e.line, line, "{text:?}: {e}");
        assert!(e.message.contains(words), "{text:?}: {e}");
    }
    // Comments, blank lines and an empty file are fine.
    assert_eq!(notefile::parse(""), Ok(vec![]));
    assert_eq!(notefile::parse(notefile::HEADER), Ok(vec![]));
    assert_eq!(
        notefile::parse("# a comment\n\n# Note\nnotetype: B\n")
            .unwrap()
            .len(),
        1
    );
}

// ---- planning ----------------------------------------------------------------

fn field(ord: u32, name: &str) -> FieldInfo {
    FieldInfo {
        ord,
        name: name.into(),
        ..Default::default()
    }
}

fn notetypes() -> Vec<NotetypeInfo> {
    vec![
        NotetypeInfo {
            id: 10,
            name: "Basic".into(),
            fields: vec![field(0, "Front"), field(1, "Back")],
            ..Default::default()
        },
        NotetypeInfo {
            id: 20,
            name: "Cloze".into(),
            kind: NotetypeKind::Cloze,
            fields: vec![field(0, "Text"), field(1, "Back Extra")],
            cloze_fields: vec![0],
            ..Default::default()
        },
        NotetypeInfo {
            id: 30,
            name: "Image Occlusion".into(),
            fields: vec![field(0, "Occlusion"), field(1, "Image")],
            image_occlusion: true,
            ..Default::default()
        },
    ]
}

fn decks() -> Vec<DeckName> {
    vec![
        DeckName {
            id: 1,
            name: "Default".into(),
            filtered: false,
        },
        DeckName {
            id: 2,
            name: "Lang::French".into(),
            filtered: false,
        },
        DeckName {
            id: 3,
            name: "Filtered Deck 1".into(),
            filtered: true,
        },
    ]
}

fn originals() -> Vec<NoteData> {
    vec![
        NoteData {
            id: 100,
            guid: "g1".into(),
            notetype_id: 10,
            mtime_secs: 500,
            usn: -1,
            tags: vec!["geo".into()],
            fields: vec!["Paris".into(), "France".into()],
        },
        NoteData {
            id: 200,
            guid: "g2".into(),
            notetype_id: 20,
            mtime_secs: 600,
            usn: -1,
            tags: vec![],
            fields: vec!["{{c1::x}}".into(), " ".into()],
        },
    ]
}

fn file_notes(originals: &[NoteData], notetypes: &[NotetypeInfo]) -> Vec<FileNote> {
    originals
        .iter()
        .map(|n| {
            let nt = notetypes.iter().find(|nt| nt.id == n.notetype_id).unwrap();
            notefile::from_note(n, nt)
        })
        .collect()
}

#[test]
fn unedited_file_plans_nothing() {
    let (originals, notetypes, decks) = (originals(), notetypes(), decks());
    let ctx = PlanContext {
        originals: &originals,
        notetypes: &notetypes,
        decks: &decks,
        default_deck: None,
    };
    let text = notefile::write(&file_notes(&originals, &notetypes));
    let plan = notefile::plan(&ctx, &notefile::parse(&text).unwrap());
    assert!(plan.is_empty());
    assert_eq!(plan.unchanged, [100, 200]);
    assert!(plan.refusals.is_empty());
}

#[test]
fn changed_fields_and_tags_only() {
    let (originals, notetypes, decks) = (originals(), notetypes(), decks());
    let ctx = PlanContext {
        originals: &originals,
        notetypes: &notetypes,
        decks: &decks,
        default_deck: Some(1),
    };
    let mut edited = file_notes(&originals, &notetypes);
    edited[0].fields[1].1 = "France <b>!</b>".into();
    edited[0].mtime = Some(499); // the note as the file was written
    edited[1].tags = vec!["new".into()];
    edited[1].fields.remove(1); // a missing field stays as stored
    let plan = notefile::plan(&ctx, &edited);
    assert!(plan.refusals.is_empty(), "{:?}", plan.refusals);
    assert_eq!(
        plan.updates,
        vec![
            NoteUpdate {
                id: 100,
                expected_mtime: 499,
                expected_fields: Some(originals[0].fields.clone()),
                fields: vec![(1, "France <b>!</b>".into())],
                tags: None,
            },
            NoteUpdate {
                id: 200,
                expected_mtime: 600,
                expected_fields: Some(originals[1].fields.clone()),
                fields: vec![],
                tags: Some(vec!["new".into()]),
            },
        ]
    );
}

#[test]
fn new_blocks_become_adds_and_empty_ones_are_skipped() {
    let (originals, notetypes, decks) = (originals(), notetypes(), decks());
    let ctx = PlanContext {
        originals: &[],
        notetypes: &notetypes,
        decks: &decks,
        default_deck: Some(1),
    };
    let _ = originals;
    let mut a = notefile::new_note(&notetypes[1], Some("lang::french"), &["t".into()]);
    a.fields[0].1 = "{{c1::Paris}}".into();
    let empty = notefile::new_note(&notetypes[0], None, &[]);
    let mut b = notefile::new_note(&notetypes[0], None, &[]);
    b.fields = vec![("Back".into(), "only back".into())];
    let text = notefile::write(&[a, empty, b]);
    let plan = notefile::plan(&ctx, &notefile::parse(&text).unwrap());
    assert!(plan.refusals.is_empty(), "{:?}", plan.refusals);
    assert_eq!(plan.skipped, [1]);
    assert_eq!(
        plan.adds,
        vec![
            notefile::PlannedAdd {
                index: 0,
                notetype_id: 20,
                deck_id: 2,
                tags: vec!["t".into()],
                fields: vec!["{{c1::Paris}}".into(), String::new()],
            },
            notefile::PlannedAdd {
                index: 2,
                notetype_id: 10,
                deck_id: 1,
                tags: vec![],
                fields: vec![String::new(), "only back".into()],
            },
        ]
    );
}

#[test]
fn refusals() {
    let (originals, notetypes, decks) = (originals(), notetypes(), decks());
    let ctx = PlanContext {
        originals: &originals,
        notetypes: &notetypes,
        decks: &decks,
        default_deck: None,
    };
    let base = file_notes(&originals, &notetypes);
    let mut changed_type = base[0].clone();
    changed_type.notetype = "Cloze".into();
    let mut unknown_id = base[0].clone();
    unknown_id.id = Some(999);
    let mut deck_on_existing = base[1].clone();
    deck_on_existing.deck = Some("Default".into());
    let mut bad_field = base[0].clone();
    bad_field.fields[1].0 = "Bakc".into();
    bad_field.id = Some(100);
    let new = |notetype: &str, deck: Option<&str>| FileNote {
        notetype: notetype.into(),
        deck: deck.map(Into::into),
        fields: vec![("Front".into(), "x".into())],
        ..Default::default()
    };
    let mut io = new("Image Occlusion", Some("Default"));
    io.fields = vec![("Image".into(), "<img src=\"a.png\">".into())];
    let mut repeated = new("Basic", Some("Default"));
    repeated.fields.push(("Front".into(), "y".into()));
    let edited = vec![
        changed_type,                          // 0: notetype changed
        unknown_id,                            // 1
        deck_on_existing,                      // 2
        bad_field,                             // 3: also a duplicate of 0's id
        new("Basic", None),                    // 4: no deck and no default
        new("Basic", Some("Nope")),            // 5
        new("Basic", Some("Filtered Deck 1")), // 6
        new("Basik", Some("Default")),         // 7
        io,                                    // 8
        repeated,                              // 9
    ];
    let plan = notefile::plan(&ctx, &edited);
    let by_index = |i: usize| {
        plan.refusals
            .iter()
            .filter(|r| r.index == i)
            .map(|r| r.message.as_str())
            .collect::<Vec<_>>()
            .join(" | ")
    };
    for (i, words) in [
        (0, "note type cannot be changed"),
        (1, "not one of the notes"),
        (2, "only applies to new notes"),
        (3, "appears twice"),
        (4, "without a deck"),
        (5, "no deck named"),
        (6, "filtered deck"),
        (7, "unknown note type"),
        (8, "image occlusion"),
        (9, "\"Front\" appears twice"),
    ] {
        assert!(by_index(i).contains(words), "{i}: {}", by_index(i));
    }
    assert_eq!(plan.refusals[0].note_id, Some(100));
    assert!(plan.updates.is_empty() && plan.adds.is_empty());
    // An unknown field name on a note that is otherwise fine.
    let mut only_bad = base.clone();
    only_bad[0].fields[1].0 = "Bakc".into();
    let plan = notefile::plan(&ctx, &only_bad);
    assert_eq!(plan.refusals.len(), 1);
    assert!(plan.refusals[0]
        .message
        .contains("no field \"Bakc\" (its fields: Front, Back)"));
}
