//! Tests with a fake `Backend` that answers `Command::Notes` only.
use super::*;
use crate::services::PlainTextRenderer;
use crate::testing::buffer_text;
use std::cell::RefCell;
use std::collections::HashMap;
use tanki_core::{AddedMedia, CoreError, DeckName, FieldInfo, TemplateInfo, UpdateReport};

fn field(ord: u32, name: &str) -> FieldInfo {
    FieldInfo {
        ord,
        name: name.into(),
        ..FieldInfo::default()
    }
}

fn notetype(id: i64, name: &str, fields: &[&str], kind: NotetypeKind) -> NotetypeInfo {
    NotetypeInfo {
        id,
        name: name.into(),
        kind,
        fields: fields
            .iter()
            .enumerate()
            .map(|(i, n)| field(i as u32, n))
            .collect(),
        templates: vec![TemplateInfo {
            ord: 0,
            name: "Card 1".into(),
        }],
        cloze_fields: if kind == NotetypeKind::Cloze {
            vec![0]
        } else {
            vec![]
        },
        ..NotetypeInfo::default()
    }
}

struct Fake {
    notetypes: RefCell<Vec<NotetypeInfo>>,
    notes: RefCell<HashMap<i64, NoteData>>,
    next_id: RefCell<i64>,
    log: RefCell<Vec<String>>,
    /// Makes the next `UpdateNotes` report a conflict.
    conflict: RefCell<bool>,
}

impl Fake {
    fn new() -> Self {
        let mut basic = notetype(1, "Basic", &["Front", "Back"], NotetypeKind::Normal);
        basic.fields[1].description = "The answer".into();
        Self {
            notetypes: RefCell::new(vec![
                basic,
                notetype(2, "Cloze", &["Text", "Back Extra"], NotetypeKind::Cloze),
                notetype(
                    3,
                    "Swapped",
                    &["Back", "Front", "Extra"],
                    NotetypeKind::Normal,
                ),
                NotetypeInfo {
                    image_occlusion: true,
                    ..notetype(9, "Image Occlusion", &["Occlusion"], NotetypeKind::Cloze)
                },
            ]),
            notes: RefCell::new(HashMap::new()),
            next_id: RefCell::new(100),
            log: RefCell::new(Vec::new()),
            conflict: RefCell::new(false),
        }
    }
    fn count(&self, prefix: &str) -> usize {
        self.log
            .borrow()
            .iter()
            .filter(|l| l.starts_with(prefix))
            .count()
    }
    fn insert(&self, note: NoteData) -> i64 {
        let id = *self.next_id.borrow();
        *self.next_id.borrow_mut() += 1;
        self.notes.borrow_mut().insert(
            id,
            NoteData {
                id,
                mtime_secs: 1000,
                ..note
            },
        );
        id
    }
}

fn strip(html: &str) -> String {
    let mut out = String::new();
    let mut tag = false;
    for c in html.chars() {
        match c {
            '<' => tag = true,
            '>' => tag = false,
            c if !tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn cloze_numbers(fields: &[String]) -> Vec<u32> {
    let mut out = Vec::new();
    for f in fields {
        for part in f.split("{{c").skip(1) {
            let digits: String = part
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == ',')
                .collect();
            for n in digits.split(',').filter_map(|d| d.parse().ok()) {
                if !out.contains(&n) {
                    out.push(n);
                }
            }
        }
    }
    out
}

impl Backend for Fake {
    fn execute(&self, command: Command) -> crate::backend::Result<Response> {
        let Command::Notes(cmd) = command else {
            return Err(CoreError::Other("not a notes command".into()));
        };
        self.log.borrow_mut().push(format!("{cmd:?}"));
        let found = |id: i64| self.notetypes.borrow().iter().find(|n| n.id == id).cloned();
        let changes = OpChanges {
            note: true,
            ..OpChanges::default()
        };
        let reply = match cmd {
            NoteCommand::Notetypes => EditResponse::Notetypes(self.notetypes.borrow().clone()),
            NoteCommand::Notetype { id } => {
                EditResponse::Notetype(found(id).ok_or(CoreError::Invalid("no type".into()))?)
            }
            NoteCommand::DeckNames { .. } => EditResponse::DeckNames(vec![
                DeckName {
                    id: 1,
                    name: "Default".into(),
                    filtered: false,
                },
                DeckName {
                    id: 2,
                    name: "Spanish".into(),
                    filtered: false,
                },
            ]),
            NoteCommand::DefaultsForAdding { .. } => EditResponse::Defaults {
                deck_id: 1,
                notetype_id: 1,
            },
            NoteCommand::DefaultDeckForNotetype { notetype_id } => {
                EditResponse::Deck((notetype_id == 2).then_some(2))
            }
            NoteCommand::CheckFields { note } => {
                let nt = found(note.notetype_id).unwrap();
                let cloze = nt.kind == NotetypeKind::Cloze;
                let has = |f: &String| f.contains("{{c");
                let state = if !cloze && note.fields.iter().any(has) {
                    FieldsCheck::NotetypeNotCloze
                } else if cloze && note.fields.iter().enumerate().any(|(i, f)| i > 0 && has(f)) {
                    FieldsCheck::FieldNotCloze
                } else if strip(&note.fields[0]).trim().is_empty() {
                    FieldsCheck::Empty
                } else if cloze && !note.fields.iter().any(has) {
                    FieldsCheck::MissingCloze
                } else if self.notes.borrow().values().any(|n| {
                    n.id != note.id
                        && n.notetype_id == note.notetype_id
                        && n.fields[0] == note.fields[0]
                }) {
                    FieldsCheck::Duplicate
                } else {
                    FieldsCheck::Normal
                };
                EditResponse::FieldsCheck(state)
            }
            NoteCommand::AddNote { note, .. } => {
                let id = self.insert(note);
                EditResponse::NoteAdded { id, changes }
            }
            NoteCommand::GetNote { id } => EditResponse::Note(
                self.notes
                    .borrow()
                    .get(&id)
                    .cloned()
                    .ok_or(CoreError::Invalid("gone".into()))?,
            ),
            NoteCommand::UpdateNotes { updates } => {
                let mut report = UpdateReport::default();
                for u in updates {
                    let mut notes = self.notes.borrow_mut();
                    match notes.get_mut(&u.id) {
                        None => report.missing.push(u.id),
                        Some(n) if n.mtime_secs != u.expected_mtime || *self.conflict.borrow() => {
                            report.conflicts.push(u.id)
                        }
                        Some(n) => {
                            for (ord, v) in u.fields {
                                n.fields[ord as usize] = v;
                            }
                            if let Some(t) = u.tags {
                                n.tags = t;
                            }
                            n.mtime_secs += 1;
                            report.updated.push(u.id);
                            report.changes = changes.clone();
                        }
                    }
                }
                EditResponse::Updated(report)
            }
            NoteCommand::ClozeNumbers { note } => {
                EditResponse::Numbers(cloze_numbers(&note.fields))
            }
            NoteCommand::RenderPreview { note, card_ord } => {
                EditResponse::Rendered(tanki_core::RenderedCard {
                    question: format!("Q{}: {}", card_ord + 1, note.fields[0]),
                    answer: format!("A: {}", note.fields.get(1).cloned().unwrap_or_default()),
                    ord: card_ord,
                    ..Default::default()
                })
            }
            NoteCommand::SetSticky {
                notetype_id,
                field_ord,
                sticky,
            } => {
                self.notetypes
                    .borrow_mut()
                    .iter_mut()
                    .find(|n| n.id == notetype_id)
                    .unwrap()
                    .fields[field_ord as usize]
                    .sticky = sticky;
                EditResponse::Changes(OpChanges {
                    notetype: true,
                    ..OpChanges::default()
                })
            }
            NoteCommand::CompleteTag { input, .. } => EditResponse::Strings(
                ["lang::es", "language", "math"]
                    .iter()
                    .filter(|t| t.contains(&input.to_lowercase()))
                    .map(|t| t.to_string())
                    .collect(),
            ),
            NoteCommand::HtmlToText { html, .. } => EditResponse::Text(strip(&html)),
            NoteCommand::AddMedia { .. }
            | NoteCommand::AddMediaFromPath { .. }
            | NoteCommand::AddMediaFromUrl { .. } => EditResponse::Media(AddedMedia {
                filename: "chosen.png".into(),
                field_text: "<img src=\"chosen.png\">".into(),
            }),
            other => return Err(CoreError::Other(format!("fake: unsupported {other:?}"))),
        };
        Ok(Response::Edit(reply))
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn alt(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
}
fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}
fn ctrl_enter() -> KeyEvent {
    KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)
}

fn typed(ui: &mut AddUi, b: &Fake, s: &str) {
    for c in s.chars() {
        let k = if c == '\n' {
            key(KeyCode::Enter)
        } else {
            key(KeyCode::Char(c))
        };
        ui.on_key(b, k);
    }
}

fn new_add(b: &Fake) -> AddUi {
    AddUi::new_add(b, 0, PathBuf::from("/tmp/media")).unwrap()
}

fn screen(ui: &AddUi, w: u16, h: u16) -> Vec<String> {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    ui.render(area, &mut buf, &PlainTextRenderer);
    buffer_text(&buf)
}

fn text(rows: &[String]) -> String {
    rows.join("\n")
}

#[test]
fn defaults_and_first_render() {
    let b = Fake::new();
    let ui = new_add(&b);
    assert_eq!(ui.notetype_name(), "Basic");
    assert_eq!(ui.deck_id(), 1);
    assert_eq!(ui.check(), FieldsCheck::Empty);
    let rows = screen(&ui, 60, 14);
    assert_eq!(
        rows,
        [
            "Add · Type: Basic (Ctrl+N) · Deck: Default (Ctrl+D)",
            "Front",
            "",
            "",
            "",
            "",
            "",
            "",
            "Back",
            "The answer",
            "Tags",
            "",
            "The first field is empty (adding is blocked)",
            "",
        ]
        .map(String::from)
    );
}

#[test]
fn add_flow_with_sticky_history_and_checks() {
    let b = Fake::new();
    let mut ui = new_add(&b);
    // Empty blocks.
    assert_eq!(ui.on_key(&b, ctrl_enter()), AddIntent::None);
    assert_eq!(ui.status(), Some("The first field is empty."));
    assert_eq!(b.count("AddNote"), 0);
    typed(&mut ui, &b, "hola");
    assert_eq!(ui.check(), FieldsCheck::Normal);
    // Second field, make it sticky.
    ui.on_key(&b, key(KeyCode::Tab));
    typed(&mut ui, &b, "hello");
    ui.on_key(&b, alt('s'));
    assert!(b.notetypes.borrow()[0].fields[1].sticky);
    // Tags with completion.
    ui.on_key(&b, ctrl('t'));
    typed(&mut ui, &b, "lan");
    assert_eq!(ui.tag_popup.items, ["lang::es", "language"]);
    ui.on_key(&b, key(KeyCode::Down));
    ui.on_key(&b, key(KeyCode::Enter));
    assert_eq!(ui.tags_text(), "language ");
    typed(&mut ui, &b, "x");
    // Add.
    let intent = ui.on_key(&b, ctrl_enter());
    assert!(matches!(intent, AddIntent::Changed(_)), "{intent:?}");
    let stored = b.notes.borrow().get(&100).cloned().unwrap();
    assert_eq!(stored.fields, ["hola", "hello"]);
    assert_eq!(stored.tags, ["language", "x"]);
    assert_eq!(stored.notetype_id, 1);
    // Sticky field and tags stay, others clear.
    assert_eq!(ui.field_values(), ["", "hello"]);
    assert_eq!(ui.tags_text(), "language x");
    assert_eq!(
        ui.history(),
        [HistoryEntry {
            note_id: 100,
            label: "hola, hello".into()
        }]
    );
    // Nothing new typed: close without prompt (sticky matches the baseline).
    assert_eq!(ui.on_key(&b, key(KeyCode::Esc)), AddIntent::Close);
}

#[test]
fn history_keeps_fifteen_newest_first() {
    let b = Fake::new();
    let mut ui = new_add(&b);
    for i in 0..17 {
        typed(&mut ui, &b, &format!("n{i}"));
        ui.on_key(&b, ctrl_enter());
    }
    assert_eq!(ui.history().len(), HISTORY_LIMIT);
    assert_eq!(ui.history()[0].label, "n16");
    assert_eq!(ui.history()[14].label, "n2");
    assert_eq!(truncate_label(&"x".repeat(40)).chars().count(), 30);
}

#[test]
fn duplicate_warns_but_adds_and_blocks_for_cloze_errors() {
    let b = Fake::new();
    let mut ui = new_add(&b);
    typed(&mut ui, &b, "same");
    ui.on_key(&b, ctrl_enter());
    typed(&mut ui, &b, "same");
    assert_eq!(ui.check(), FieldsCheck::Duplicate);
    assert!(text(&screen(&ui, 80, 14)).contains("Duplicate: another note has the same first field"));
    assert!(matches!(ui.on_key(&b, ctrl_enter()), AddIntent::Changed(_)));
    // A cloze in a normal note type blocks.
    typed(&mut ui, &b, "{{c1::x}}");
    assert_eq!(ui.check(), FieldsCheck::NotetypeNotCloze);
    let before = b.count("AddNote");
    ui.on_key(&b, ctrl_enter());
    assert_eq!(b.count("AddNote"), before);
    assert!(ui.status().unwrap().contains("cloze note type"));
}

#[test]
fn missing_cloze_asks_and_field_not_cloze_blocks() {
    let b = Fake::new();
    let mut ui = new_add(&b);
    ui.on_key(&b, ctrl('n'));
    typed(&mut ui, &b, "Cloze");
    ui.on_key(&b, key(KeyCode::Enter));
    assert_eq!(ui.notetype_name(), "Cloze");
    assert_eq!(ui.deck_id(), 2, "default deck of the note type");
    typed(&mut ui, &b, "plain text");
    assert_eq!(ui.check(), FieldsCheck::MissingCloze);
    ui.on_key(&b, ctrl_enter());
    assert!(ui.has_overlay());
    assert!(text(&screen(&ui, 80, 16)).contains("Proceed?"));
    ui.on_key(&b, key(KeyCode::Char('n')));
    assert_eq!(b.count("AddNote"), 0);
    ui.on_key(&b, ctrl_enter());
    ui.on_key(&b, key(KeyCode::Char('y')));
    assert_eq!(b.count("AddNote"), 1);
    // Cloze in Back Extra blocks.
    ui.on_key(&b, key(KeyCode::Tab));
    typed(&mut ui, &b, "{{c1::z}}");
    assert_eq!(ui.check(), FieldsCheck::FieldNotCloze);
}

#[test]
fn changing_notetype_carries_content() {
    let old: Vec<String> = ["Front", "Back"].map(String::from).to_vec();
    let vals: Vec<String> = ["a", "b"].map(String::from).to_vec();
    // Names first, then by position into the free slots.
    let new: Vec<String> = ["Back", "Front", "Extra"].map(String::from).to_vec();
    assert_eq!(carry_fields(&old, &vals, &new), ["b", "a", ""]);
    let new: Vec<String> = ["Text", "Back"].map(String::from).to_vec();
    assert_eq!(carry_fields(&old, &vals, &new), ["a", "b"]);
    let new: Vec<String> = ["Q", "A", "Back"].map(String::from).to_vec();
    assert_eq!(carry_fields(&old, &vals, &new), ["a", "", "b"]);
    let empty_front: Vec<String> = ["", "b"].map(String::from).to_vec();
    let new: Vec<String> = ["X", "Y"].map(String::from).to_vec();
    assert_eq!(carry_fields(&old, &empty_front, &new), ["b", ""]);

    let b = Fake::new();
    let mut ui = new_add(&b);
    typed(&mut ui, &b, "front!");
    ui.on_key(&b, key(KeyCode::Tab));
    typed(&mut ui, &b, "back!");
    ui.on_key(&b, ctrl('t'));
    typed(&mut ui, &b, "t1");
    ui.on_key(&b, ctrl('n'));
    typed(&mut ui, &b, "Swapped");
    ui.on_key(&b, key(KeyCode::Enter));
    assert_eq!(ui.field_values(), ["back!", "front!", ""]);
    assert_eq!(ui.tags_text(), "t1");
}

#[test]
fn image_occlusion_is_refused_with_a_message() {
    let b = Fake::new();
    let mut ui = new_add(&b);
    ui.on_key(&b, ctrl('n'));
    typed(&mut ui, &b, "Image");
    ui.on_key(&b, key(KeyCode::Enter));
    assert_eq!(ui.notetype_name(), "Basic");
    assert!(ui.status().unwrap().contains("Image occlusion"));
}

#[test]
fn discard_prompt_on_close() {
    let b = Fake::new();
    let mut ui = new_add(&b);
    assert_eq!(ui.on_key(&b, key(KeyCode::Esc)), AddIntent::Close);
    typed(&mut ui, &b, "work");
    assert_eq!(ui.on_key(&b, key(KeyCode::Esc)), AddIntent::None);
    let rows = screen(&ui, 60, 16);
    assert!(
        text(&rows).contains("Discard current input?"),
        "{}",
        text(&rows)
    );
    assert_eq!(ui.on_key(&b, key(KeyCode::Char('n'))), AddIntent::None);
    assert!(!ui.has_overlay());
    ui.on_key(&b, key(KeyCode::Esc));
    assert_eq!(ui.on_key(&b, key(KeyCode::Char('y'))), AddIntent::Close);
    // A `<br>`-only field is not content.
    let mut ui = new_add(&b);
    typed(&mut ui, &b, "<br>");
    assert_eq!(ui.on_key(&b, key(KeyCode::Esc)), AddIntent::Close);
}

#[test]
fn format_commands_and_cloze_numbering_use_the_whole_note() {
    let b = Fake::new();
    let mut ui = new_add(&b);
    ui.on_key(&b, ctrl('n'));
    typed(&mut ui, &b, "Cloze");
    ui.on_key(&b, key(KeyCode::Enter));
    typed(&mut ui, &b, "{{c1,3::a}} b");
    ui.on_key(&b, key(KeyCode::Tab));
    typed(&mut ui, &b, "extra");
    ui.on_key(&b, key(KeyCode::BackTab));
    ui.on_key(&b, alt('a')); // select all
    ui.on_key(&b, alt('c'));
    assert_eq!(ui.field_values()[0], "{{c4::{{c1,3::a}} b}}");
    // Same number: max in use (4).
    ui.on_key(&b, ctrl('z'));
    ui.on_key(&b, alt('a'));
    ui.on_key(
        &b,
        KeyEvent::new(KeyCode::Char('C'), KeyModifiers::ALT | KeyModifiers::SHIFT),
    );
    assert_eq!(ui.field_values()[0], "{{c3::{{c1,3::a}} b}}");
    ui.on_key(&b, ctrl('z'));
    ui.on_key(&b, alt('a'));
    ui.on_key(&b, alt('b'));
    assert_eq!(ui.field_values()[0], "<b>{{c1,3::a}} b</b>");
    // Colour palette.
    ui.on_key(&b, ctrl('z'));
    ui.on_key(&b, alt('a'));
    ui.on_key(&b, alt('t'));
    assert!(text(&screen(&ui, 70, 20)).contains("Text colour"));
    ui.on_key(&b, key(KeyCode::Char('1')));
    assert_eq!(
        ui.field_values()[0],
        "<span style=\"color: rgb(255, 0, 0);\">{{c1,3::a}} b</span>"
    );
}

#[test]
fn media_attach_by_prompt_paste_clipboard_and_url() {
    let b = Fake::new();
    let mut ui = new_add(&b);
    // Prompt with a URL.
    ui.on_key(&b, alt('f'));
    assert!(text(&screen(&ui, 80, 20)).contains("Attach file or URL"));
    for c in "https://x.test/a.png".chars() {
        ui.on_key(&b, key(KeyCode::Char(c)));
    }
    ui.on_key(&b, key(KeyCode::Enter));
    assert_eq!(ui.field_values()[0], "<img src=\"chosen.png\">");
    assert_eq!(b.count("AddMediaFromUrl"), 1);
    // Pasted path is offered.
    let path = std::env::temp_dir().join(format!("tanki-add-paste-{}.png", std::process::id()));
    std::fs::write(&path, b"png").unwrap();
    ui.on_paste(&b, &format!("'{}'", path.display()));
    assert!(text(&screen(&ui, 80, 20)).contains("Attach file?"));
    ui.on_key(&b, key(KeyCode::Enter));
    assert_eq!(b.count("AddMediaFromPath"), 1);
    assert_eq!(
        ui.field_values()[0],
        "<img src=\"chosen.png\"><img src=\"chosen.png\">"
    );
    // `n` pastes the text instead.
    ui.on_paste(&b, &path.display().to_string());
    ui.on_key(&b, key(KeyCode::Char('n')));
    assert!(ui.field_values()[0].ends_with(&path.display().to_string()));
    std::fs::remove_file(&path).unwrap();
    // Ordinary paste is text, one undo step.
    ui.on_paste(&b, "plain\ntext");
    assert!(ui.field_values()[0].ends_with("plain\ntext"));
    // Clipboard.
    ui.set_clipboard(Box::new(|| Err(media::NO_CLIPBOARD_TOOL.to_string())));
    ui.on_key(&b, alt('v'));
    assert!(ui.status().unwrap().contains("No clipboard tool"));
    ui.set_clipboard(Box::new(|| Ok(vec![1, 2, 3])));
    ui.on_key(&b, alt('v'));
    assert_eq!(b.count("AddMedia {"), 1);
    assert!(ui.status().unwrap().contains("chosen.png"));
}

#[test]
fn external_editor_intents() {
    let b = Fake::new();
    let mut ui = new_add(&b);
    typed(&mut ui, &b, "abc");
    assert_eq!(
        ui.on_key(&b, alt('x')),
        AddIntent::ExternalEdit(ExternalEdit::Field {
            ord: 0,
            html: "abc".into()
        })
    );
    ui.apply_external_field(&b, 0, "abc<br>def".into());
    assert_eq!(ui.field_values()[0], "abc<br>def");
    let intent = ui.on_key(
        &b,
        KeyEvent::new(KeyCode::Char('X'), KeyModifiers::ALT | KeyModifiers::SHIFT),
    );
    let AddIntent::ExternalEdit(ExternalEdit::Note {
        notetype,
        deck,
        fields,
        note_id,
        ..
    }) = intent
    else {
        panic!("{intent:?}")
    };
    assert_eq!(
        (notetype.as_str(), deck.as_str(), note_id),
        ("Basic", "Default", None)
    );
    assert_eq!(fields[0], ("Front".into(), "abc<br>def".into()));
    ui.apply_external_note(&b, vec!["x".into(), "y".into()], vec!["t".into()]);
    assert_eq!(ui.field_values(), ["x", "y"]);
    assert_eq!(ui.tags_text(), "t");
}

#[test]
fn preview_toggle_and_card_choice() {
    let b = Fake::new();
    let mut ui = new_add(&b);
    typed(&mut ui, &b, "front text");
    ui.on_key(&b, alt('p'));
    let rows = screen(&ui, 120, 16);
    let all = text(&rows);
    assert!(
        all.contains("Preview") && all.contains("Q1: front text"),
        "{all}"
    );
    assert!(all.contains("A:"));
    ui.on_key(&b, alt('p'));
    assert!(!text(&screen(&ui, 120, 16)).contains("Preview"));
    // Cloze types preview one card per number.
    ui.on_key(&b, ctrl('n'));
    typed(&mut ui, &b, "Cloze");
    ui.on_key(&b, key(KeyCode::Enter));
    typed(&mut ui, &b, "{{c1::a}} {{c2::b}}");
    ui.on_key(&b, alt('p'));
    assert!(text(&screen(&ui, 120, 16)).contains("Q1:"));
    ui.on_key(&b, alt(']'));
    assert!(text(&screen(&ui, 120, 16)).contains("Q2:"));
}

fn edit_note(b: &Fake) -> i64 {
    b.insert(NoteData {
        notetype_id: 1,
        tags: vec!["old".into()],
        fields: vec!["front".into(), "back".into()],
        ..NoteData::default()
    })
}

#[test]
fn edit_current_sends_only_changed_fields_and_tags() {
    let b = Fake::new();
    let id = edit_note(&b);
    let mut ui = AddUi::new_edit(&b, id, PathBuf::from("/tmp/m")).unwrap();
    assert!(ui.is_edit());
    assert_eq!(ui.field_values(), ["front", "back"]);
    assert_eq!(ui.tags_text(), "old");
    assert!(!ui.has_unsaved_input());
    ui.on_key(&b, key(KeyCode::Tab));
    typed(&mut ui, &b, "!");
    ui.on_key(&b, ctrl('t'));
    typed(&mut ui, &b, " new");
    assert!(ui.has_unsaved_input());
    let intent = ui.on_key(&b, ctrl_enter());
    assert!(matches!(intent, AddIntent::Saved(_)), "{intent:?}");
    let sent = b
        .log
        .borrow()
        .iter()
        .find(|l| l.starts_with("UpdateNotes"))
        .cloned()
        .unwrap();
    assert!(sent.contains("fields: [(1, \"back!\")]"), "{sent}");
    assert!(sent.contains("expected_mtime: 1000"));
    assert!(sent.contains("Some([\"old\", \"new\"])"));
    let stored = b.notes.borrow().get(&id).cloned().unwrap();
    assert_eq!(stored.fields, ["front", "back!"]);
    // Unchanged form: no update at all.
    let mut ui = AddUi::new_edit(&b, id, PathBuf::from("/tmp/m")).unwrap();
    let n = b.count("UpdateNotes");
    assert_eq!(ui.on_key(&b, ctrl_enter()), AddIntent::Close);
    assert_eq!(b.count("UpdateNotes"), n);
}

#[test]
fn edit_conflict_offers_reload() {
    let b = Fake::new();
    let id = edit_note(&b);
    let mut ui = AddUi::new_edit(&b, id, PathBuf::from("/tmp/m")).unwrap();
    typed(&mut ui, &b, "X");
    // Someone else saves first.
    b.notes.borrow_mut().get_mut(&id).unwrap().mtime_secs += 5;
    assert_eq!(ui.on_key(&b, ctrl_enter()), AddIntent::None);
    let all = text(&screen(&ui, 80, 18));
    assert!(
        all.contains("changed elsewhere") && all.contains("reload"),
        "{all}"
    );
    // Esc keeps editing.
    ui.on_key(&b, key(KeyCode::Esc));
    assert!(!ui.has_overlay());
    assert_eq!(ui.field_values()[0], "frontX");
    // Reload discards the edits.
    ui.on_key(&b, ctrl_enter());
    ui.on_key(&b, key(KeyCode::Char('r')));
    assert_eq!(ui.field_values()[0], "front");
    assert!(!ui.has_unsaved_input());
    // Deleted note: reported as missing.
    b.notes.borrow_mut().remove(&id);
    typed(&mut ui, &b, "Y");
    ui.on_key(&b, ctrl_enter());
    assert!(text(&screen(&ui, 80, 18)).contains("deleted elsewhere") || ui.status().is_some());
}

#[test]
fn history_reopens_a_note_for_editing_then_returns_to_the_form() {
    let b = Fake::new();
    let mut ui = new_add(&b);
    typed(&mut ui, &b, "first");
    ui.on_key(&b, ctrl_enter());
    typed(&mut ui, &b, "draft");
    ui.on_key(&b, alt('h'));
    let all = text(&screen(&ui, 80, 20));
    assert!(
        all.contains("History of added notes") && all.contains("first"),
        "{all}"
    );
    // Open in browser intent.
    ui.on_key(&b, key(KeyCode::Char('b')));
    // (overlay was consumed by the key; reopen to edit)
    ui.on_key(&b, alt('h'));
    ui.on_key(&b, key(KeyCode::Enter));
    assert!(ui.is_edit());
    assert_eq!(ui.field_values()[0], "first");
    typed(&mut ui, &b, "2");
    let intent = ui.on_key(&b, ctrl_enter());
    assert!(matches!(intent, AddIntent::Changed(_)), "{intent:?}");
    assert!(!ui.is_edit());
    assert_eq!(ui.field_values()[0], "draft");
    assert_eq!(b.notes.borrow().get(&100).unwrap().fields[0], "first2");
}

#[test]
fn history_browser_intent_and_deleted_notes() {
    let b = Fake::new();
    let mut ui = new_add(&b);
    typed(&mut ui, &b, "one");
    ui.on_key(&b, ctrl_enter());
    ui.on_key(&b, alt('h'));
    assert_eq!(
        ui.on_key(&b, key(KeyCode::Char('b'))),
        AddIntent::OpenBrowser { note_id: 100 }
    );
    b.notes.borrow_mut().remove(&100);
    ui.on_key(&b, alt('h'));
    ui.on_key(&b, key(KeyCode::Enter));
    assert_eq!(ui.status(), Some("That note no longer exists"));
    assert!(!ui.is_edit());
}

#[test]
fn on_changes_refreshes_notetypes() {
    let b = Fake::new();
    let mut ui = new_add(&b);
    typed(&mut ui, &b, "keep");
    b.notetypes.borrow_mut()[0].fields.push(field(2, "Extra"));
    ui.on_changes(
        &b,
        &OpChanges {
            notetype: true,
            ..OpChanges::default()
        },
    );
    assert_eq!(ui.field_values(), ["keep", "", ""]);
}

// ---- snapshots -------------------------------------------------------------------

#[test]
fn snapshot_filled_form_with_sticky_and_tags_popup() {
    let b = Fake::new();
    let mut ui = new_add(&b);
    typed(&mut ui, &b, "hola\nmundo");
    ui.on_key(&b, key(KeyCode::Tab));
    typed(&mut ui, &b, "hello");
    ui.on_key(&b, alt('s'));
    ui.on_key(&b, ctrl('t'));
    typed(&mut ui, &b, "lan");
    let rows = screen(&ui, 56, 14);
    assert_eq!(
        rows,
        [
            "Add · Type: Basic (Ctrl+N) · Deck: Default (Ctrl+D)",
            "Front",
            "hola",
            "mundo",
            "Back [sticky]",
            "hello",
            "",
            " lang::es",
            " language",
            "",
            "Tags",
            "lan",
            "",
            "",
        ]
        .map(String::from)
    );
}

#[test]
fn snapshot_pickers() {
    let b = Fake::new();
    let mut ui = new_add(&b);
    ui.on_key(&b, ctrl('n'));
    let rows = screen(&ui, 60, 14);
    let all = text(&rows);
    assert!(
        all.contains("Note type") && all.contains("Cloze") && all.contains("Image Occlusion"),
        "{all}"
    );
    ui.on_key(&b, key(KeyCode::Esc));
    ui.on_key(&b, ctrl('d'));
    typed(&mut ui, &b, "span");
    let all = text(&screen(&ui, 60, 14));
    assert!(all.contains("> span") && all.contains("Spanish"), "{all}");
    ui.on_key(&b, key(KeyCode::Enter));
    assert_eq!(ui.deck_id(), 2);
    assert!(text(&screen(&ui, 60, 14)).contains("Deck: Spanish"));
}

#[test]
fn snapshot_edit_form_and_conflict_text() {
    let b = Fake::new();
    let id = edit_note(&b);
    let ui = AddUi::new_edit(&b, id, PathBuf::from("/tmp/m")).unwrap();
    let rows = screen(&ui, 50, 12);
    assert_eq!(rows[0], format!("Edit note {id} · Basic"));
    assert_eq!(&rows[1..3], ["Front", "front"]);
    assert!(ui.hints().contains("Ctrl+Enter save"));
}

#[test]
fn field_check_states_render() {
    let b = Fake::new();
    let mut ui = new_add(&b);
    assert!(text(&screen(&ui, 80, 12)).contains("The first field is empty"));
    typed(&mut ui, &b, "{{c1::x}}");
    assert!(text(&screen(&ui, 90, 12)).contains("not a cloze type (adding is blocked)"));
    let mut ui = new_add(&b);
    ui.on_key(&b, ctrl('n'));
    typed(&mut ui, &b, "Cloze");
    ui.on_key(&b, key(KeyCode::Enter));
    typed(&mut ui, &b, "no cloze");
    assert!(text(&screen(&ui, 90, 12)).contains("No cloze deletion yet (adding will ask)"));
    ui.on_key(&b, key(KeyCode::Tab));
    typed(&mut ui, &b, "{{c1::x}}");
    assert!(text(&screen(&ui, 90, 12)).contains("a field the card does not use for clozes"));
}

#[test]
fn hints_follow_the_context() {
    let b = Fake::new();
    let mut ui = new_add(&b);
    assert!(ui.hints().contains("Ctrl+Enter add"));
    ui.on_key(&b, alt('f'));
    assert!(ui.hints().contains("Tab complete"));
    assert!(KEYS.iter().any(|(k, _)| k.contains("Ctrl+Enter")));
}
