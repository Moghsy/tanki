//! Tests with a fake `Backend` that answers the note type commands.
use super::*;
use crate::services::PlainTextRenderer;
use crate::testing::buffer_text;
use crossterm::event::KeyEventState;
use std::cell::{Cell, RefCell};
use tanki_core::notetypes::{ChangeNotetypeInfo, FieldDetail, TemplateDetail, TemplateProblemKind};
use tanki_core::{DeckName, RenderedCard};

fn field(ord: u32, name: &str) -> FieldDetail {
    FieldDetail {
        ord: Some(ord),
        name: name.into(),
        sticky: false,
        rtl: false,
        plain_text: false,
        collapsed: false,
        exclude_from_search: false,
        description: String::new(),
        font_name: "Arial".into(),
        font_size: 20,
        required: false,
    }
}

fn template(ord: u32, name: &str, question: &str, answer: &str) -> TemplateDetail {
    TemplateDetail {
        ord: Some(ord),
        name: name.into(),
        question: question.into(),
        answer: answer.into(),
        browser_question: String::new(),
        browser_answer: String::new(),
        browser_font_name: String::new(),
        browser_font_size: 0,
        target_deck_id: None,
    }
}

fn detail(id: i64, name: &str, fields: &[&str], templates: Vec<TemplateDetail>) -> NotetypeDetail {
    NotetypeDetail {
        id,
        name: name.into(),
        kind: NotetypeKind::Normal,
        stock_kind: Some(StockKind::Basic),
        use_count: 3,
        css: ".card { color: black }".into(),
        latex_pre: "\\begin{document}".into(),
        latex_post: "\\end{document}".into(),
        latex_svg: false,
        sort_field: 0,
        fields: fields
            .iter()
            .enumerate()
            .map(|(i, n)| field(i as u32, n))
            .collect(),
        templates,
    }
}

struct Fake {
    types: RefCell<Vec<NotetypeDetail>>,
    calls: RefCell<Vec<NotetypeCommand>>,
    /// Structural changes answer `SchemaChangeRequired` unless confirmed.
    guard: Cell<bool>,
    reject: RefCell<Option<TemplateProblem>>,
    next_id: Cell<i64>,
}

impl Fake {
    fn new() -> Self {
        let mut cloze = detail(
            2,
            "Cloze",
            &["Text", "Back Extra"],
            vec![template(
                0,
                "Cloze",
                "{{cloze:Text}}",
                "{{cloze:Text}}<br>{{Back Extra}}",
            )],
        );
        cloze.kind = NotetypeKind::Cloze;
        cloze.stock_kind = Some(StockKind::Cloze);
        cloze.fields[0].required = true;
        let mut custom = detail(
            3,
            "Custom",
            &["A", "B", "C"],
            vec![
                template(0, "One", "{{A}}", "{{B}}"),
                template(1, "Two", "{{B}}", "{{C}}"),
            ],
        );
        custom.stock_kind = None;
        custom.use_count = 0;
        Self {
            types: RefCell::new(vec![
                detail(
                    1,
                    "Basic",
                    &["Front", "Back"],
                    vec![template(
                        0,
                        "Card 1",
                        "{{Front}}",
                        "{{FrontSide}}<hr id=answer>{{Back}}",
                    )],
                ),
                cloze,
                custom,
            ]),
            calls: RefCell::new(Vec::new()),
            guard: Cell::new(true),
            reject: RefCell::new(None),
            next_id: Cell::new(10),
        }
    }

    fn calls(&self) -> Vec<NotetypeCommand> {
        self.calls.borrow().clone()
    }

    fn called(&self, name: &str) -> usize {
        self.calls
            .borrow()
            .iter()
            .filter(|c| format!("{c:?}").starts_with(name))
            .count()
    }

    fn summary(d: &NotetypeDetail) -> NotetypeSummary {
        NotetypeSummary {
            id: d.id,
            name: d.name.clone(),
            kind: d.kind,
            use_count: d.use_count,
            field_count: d.fields.len(),
            template_count: d.templates.len(),
            stock_kind: d.stock_kind,
        }
    }

    fn structural(old: &NotetypeDetail, new: &NotetypeDetail) -> bool {
        let identity = |ords: Vec<Option<u32>>, len: usize| {
            ords.len() != len || ords.iter().enumerate().any(|(i, o)| *o != Some(i as u32))
        };
        identity(new.fields.iter().map(|f| f.ord).collect(), old.fields.len())
            || identity(
                new.templates.iter().map(|t| t.ord).collect(),
                old.templates.len(),
            )
            || old.sort_field != new.sort_field
    }

    fn answer(&self, command: NotetypeCommand) -> tanki_core::Result<NotetypeResponse> {
        let changes = OpChanges {
            notetype: true,
            ..OpChanges::default()
        };
        let mut types = self.types.borrow_mut();
        let find = |types: &Vec<NotetypeDetail>, id: i64| {
            types
                .iter()
                .position(|t| t.id == id)
                .ok_or_else(|| CoreError::Invalid("no such note type".into()))
        };
        Ok(match command {
            NotetypeCommand::InstallCodeTour => unreachable!("Not a note-type editor action"),
            NotetypeCommand::List => {
                NotetypeResponse::List(types.iter().map(Self::summary).collect())
            }
            NotetypeCommand::StockKinds => NotetypeResponse::StockKinds(
                StockKind::ALL
                    .into_iter()
                    .map(|kind| StockKindInfo {
                        kind,
                        name: format!("{kind:?}"),
                    })
                    .collect(),
            ),
            NotetypeCommand::Get { id } => {
                NotetypeResponse::Detail(types[find(&types, id)?].clone())
            }
            NotetypeCommand::AddStock { kind, name } => {
                let id = self.next_id.replace(self.next_id.get() + 1);
                let mut d = detail(id, &name.unwrap_or_default(), &["Front", "Back"], vec![]);
                d.stock_kind = Some(kind);
                types.push(d);
                NotetypeResponse::Added { id, changes }
            }
            NotetypeCommand::Clone { id, name } => {
                let mut d = types[find(&types, id)?].clone();
                let id = self.next_id.replace(self.next_id.get() + 1);
                d.id = id;
                d.name = name.unwrap_or_default();
                types.push(d);
                NotetypeResponse::Added { id, changes }
            }
            NotetypeCommand::Rename { id, name } => {
                let i = find(&types, id)?;
                types[i].name = name;
                NotetypeResponse::Saved {
                    notetype: types[i].clone(),
                    changes,
                }
            }
            NotetypeCommand::Delete { id, confirmed } => {
                let i = find(&types, id)?;
                if self.guard.get() && !confirmed {
                    return Err(CoreError::SchemaChangeRequired);
                }
                types.remove(i);
                NotetypeResponse::Changed(changes)
            }
            NotetypeCommand::Save {
                mut notetype,
                confirmed,
            } => {
                let i = find(&types, notetype.id)?;
                if self.guard.get() && !confirmed && Self::structural(&types[i], &notetype) {
                    return Err(CoreError::SchemaChangeRequired);
                }
                if let Some(problem) = self.reject.borrow().clone() {
                    return Ok(NotetypeResponse::Rejected(problem));
                }
                for (i, f) in notetype.fields.iter_mut().enumerate() {
                    f.ord = Some(i as u32);
                }
                for (i, t) in notetype.templates.iter_mut().enumerate() {
                    t.ord = Some(i as u32);
                }
                types[i] = notetype.clone();
                NotetypeResponse::Saved { notetype, changes }
            }
            NotetypeCommand::Edit { .. } => {
                return Err(CoreError::Invalid("the form saves drafts".into()))
            }
            NotetypeCommand::Preview { template, css, .. } => {
                let render = |s: &str| {
                    s.replace("{{Front}}", "FRONT TEXT")
                        .replace("{{Back}}", "BACK TEXT")
                };
                let error = template
                    .question
                    .contains("{{Nope}}")
                    .then(|| "Field 'Nope' not found".to_owned());
                NotetypeResponse::Preview(TemplatePreview {
                    card: RenderedCard {
                        question: error.clone().unwrap_or_else(|| render(&template.question)),
                        answer: render(&template.answer),
                        css: css.unwrap_or_default(),
                        ..RenderedCard::default()
                    },
                    error,
                })
            }
            NotetypeCommand::ChangeInfo {
                note_ids,
                new_notetype_id,
            } => {
                let old = &types[find(&types, 1)?];
                let new = &types[find(&types, new_notetype_id)?];
                let by_name = |olds: &[String], news: &[String]| -> Vec<Option<u32>> {
                    news.iter()
                        .map(|n| olds.iter().position(|o| o == n).map(|i| i as u32))
                        .collect()
                };
                let old_fields: Vec<String> = old.fields.iter().map(|f| f.name.clone()).collect();
                let new_fields: Vec<String> = new.fields.iter().map(|f| f.name.clone()).collect();
                let old_templates: Vec<String> =
                    old.templates.iter().map(|f| f.name.clone()).collect();
                let new_templates: Vec<String> =
                    new.templates.iter().map(|f| f.name.clone()).collect();
                let cloze = old.kind == NotetypeKind::Cloze || new.kind == NotetypeKind::Cloze;
                NotetypeResponse::ChangeInfo(ChangeNotetypeInfo {
                    old_notetype_id: old.id,
                    old_notetype_name: old.name.clone(),
                    new_notetype_id,
                    new_notetype_name: new.name.clone(),
                    new_fields: by_name(&old_fields, &new_fields),
                    new_templates: (!cloze).then(|| by_name(&old_templates, &new_templates)),
                    old_field_names: old_fields,
                    new_field_names: new_fields,
                    old_template_names: old_templates,
                    new_template_names: new_templates,
                    note_count: note_ids.len(),
                })
            }
            NotetypeCommand::Change {
                confirmed,
                note_ids,
                ..
            } => {
                if self.guard.get() && !confirmed {
                    return Err(CoreError::SchemaChangeRequired);
                }
                NotetypeResponse::NotesChanged {
                    count: note_ids.len(),
                    changes,
                }
            }
            NotetypeCommand::RestorePlan { id, force_kind } => {
                let d = &types[find(&types, id)?];
                let kind = force_kind.or(d.stock_kind);
                NotetypeResponse::RestorePlan(RestorePlan {
                    notetype_name: d.name.clone(),
                    kind,
                    needs_kind: kind.is_none(),
                    note_count: d.use_count,
                    old_field_names: d.fields.iter().map(|f| f.name.clone()).collect(),
                    stock_field_names: vec!["Front".into(), "Back".into()],
                    dropped_fields: d.fields.iter().skip(2).map(|f| f.name.clone()).collect(),
                    old_template_names: d.templates.iter().map(|t| t.name.clone()).collect(),
                    stock_template_names: vec!["Card 1".into()],
                    structure_changes: d.fields.len() != 2,
                })
            }
            NotetypeCommand::Restore {
                id,
                typed_confirmation,
                confirmed,
                ..
            } => {
                let i = find(&types, id)?;
                if !typed_confirmation {
                    return Err(CoreError::Invalid("type the name".into()));
                }
                if self.guard.get() && !confirmed {
                    return Err(CoreError::SchemaChangeRequired);
                }
                types[i].fields.truncate(2);
                NotetypeResponse::Saved {
                    notetype: types[i].clone(),
                    changes,
                }
            }
        })
    }
}

impl Backend for Fake {
    fn execute(&self, command: Command) -> tanki_core::Result<Response> {
        match command {
            Command::Manage(ManageCommand::Notetypes(c)) => {
                self.calls.borrow_mut().push(c.clone());
                self.answer(c)
                    .map(|r| Response::Manage(ManageResponse::Notetypes(r)))
            }
            Command::Browse(BrowseCommand::Search { .. }) => {
                Ok(Response::Edit(EditResponse::Ids(vec![501, 502])))
            }
            Command::Notes(NoteCommand::DeckNames { .. }) => {
                Ok(Response::Edit(EditResponse::DeckNames(vec![
                    DeckName {
                        id: 1,
                        name: "Default".into(),
                        filtered: false,
                    },
                    DeckName {
                        id: 7,
                        name: "Study".into(),
                        filtered: false,
                    },
                ])))
            }
            Command::Notes(NoteCommand::GetNote { id }) => {
                Ok(Response::Edit(EditResponse::Note(tanki_core::NoteData {
                    id,
                    notetype_id: 1,
                    ..Default::default()
                })))
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn chr(c: char) -> KeyEvent {
    key(KeyCode::Char(c))
}

fn with(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent {
        modifiers,
        ..key(code)
    }
}

fn press(ui: &mut NotetypesUi, b: &Fake, keys: &[KeyEvent]) -> NotetypesIntent {
    let mut last = NotetypesIntent::None;
    for k in keys {
        last = ui.on_key(b, *k);
    }
    last
}

fn type_text(ui: &mut NotetypesUi, b: &Fake, text: &str) {
    for c in text.chars() {
        ui.on_key(b, chr(c));
    }
}

fn open() -> (NotetypesUi, Fake) {
    let b = Fake::new();
    let ui = NotetypesUi::new(&b, PathBuf::from("/tmp/media")).unwrap();
    (ui, b)
}

use std::path::PathBuf;

fn screen(ui: &NotetypesUi, w: u16, h: u16) -> String {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    ui.render(area, &mut buf, &PlainTextRenderer);
    buffer_text(&buf).join("\n")
}

#[test]
fn list_snapshot_and_navigation() {
    let (mut ui, b) = open();
    let text = screen(&ui, 70, 12);
    assert_eq!(
        text.lines().take(5).collect::<Vec<_>>(),
        [
            "Note types",
            "  Name                             Kind        Notes  Fields  Cards",
            "> Basic                            standard        3       2      1",
            "  Cloze                            cloze           3       2      1",
            "  Custom                           standard        0       3      2",
        ]
    );
    press(&mut ui, &b, &[chr('j'), chr('j'), chr('j')]);
    assert_eq!(ui.selected().unwrap().name, "Custom");
    press(&mut ui, &b, &[chr('k')]);
    assert_eq!(ui.selected().unwrap().name, "Cloze");
    assert_eq!(
        press(&mut ui, &b, &[key(KeyCode::Esc)]),
        NotetypesIntent::Close
    );
}

#[test]
fn add_clone_and_rename_use_prompts() {
    let (mut ui, b) = open();
    press(&mut ui, &b, &[chr('a')]);
    let text = screen(&ui, 80, 24);
    assert!(text.contains("Add: Basic"), "{text}");
    assert!(text.contains("Clone: Cloze"), "{text}");
    // Filter to the clone of Cloze, accept the suggested name.
    type_text(&mut ui, &b, "clone cloze");
    let intent = press(&mut ui, &b, &[key(KeyCode::Enter)]);
    assert_eq!(intent, NotetypesIntent::None);
    assert!(screen(&ui, 80, 24).contains("Cloze copy"));
    let intent = press(&mut ui, &b, &[key(KeyCode::Enter)]);
    assert!(matches!(intent, NotetypesIntent::Changed(c) if c.notetype));
    assert!(b.calls().iter().any(
        |c| matches!(c, NotetypeCommand::Clone { id: 2, name: Some(n) } if n == "Cloze copy")
    ));
    assert_eq!(
        ui.selected().unwrap().name,
        "Cloze copy",
        "the new type is selected"
    );

    // Stock kinds are added by their (editable) name.
    press(&mut ui, &b, &[chr('a')]);
    type_text(&mut ui, &b, "typing");
    press(&mut ui, &b, &[key(KeyCode::Enter)]);
    for _ in 0..20 {
        ui.on_key(&b, key(KeyCode::Backspace));
    }
    type_text(&mut ui, &b, "Mine");
    press(&mut ui, &b, &[key(KeyCode::Enter)]);
    assert!(b.calls().iter().any(|c| matches!(c, NotetypeCommand::AddStock { kind: StockKind::BasicTyping, name: Some(n) } if n == "Mine")));

    // Rename.
    press(&mut ui, &b, &[chr('r')]);
    assert!(screen(&ui, 80, 24).contains("Rename note type"));
    for _ in 0..10 {
        ui.on_key(&b, key(KeyCode::Backspace));
    }
    type_text(&mut ui, &b, "Renamed");
    press(&mut ui, &b, &[key(KeyCode::Enter)]);
    assert!(ui.notetypes().iter().any(|n| n.name == "Renamed"));
}

#[test]
fn delete_asks_twice_when_a_full_sync_is_needed() {
    let (mut ui, b) = open();
    press(&mut ui, &b, &[chr('d')]);
    let text = screen(&ui, 80, 24);
    assert!(text.contains("Delete Basic?"), "{text}");
    assert!(
        text.contains("Its 3 notes and their cards are deleted too."),
        "{text}"
    );
    // n keeps it.
    press(&mut ui, &b, &[chr('n')]);
    assert_eq!(b.called("Delete"), 0);
    press(&mut ui, &b, &[chr('d'), chr('y')]);
    // The core asked for the schema confirmation: shown as a second prompt.
    let text = screen(&ui, 80, 24);
    assert!(text.contains("Full sync required"), "{text}");
    assert!(text.contains("one-way full sync"), "{text}");
    assert_eq!(ui.notetypes().len(), 3);
    press(&mut ui, &b, &[chr('n')]);
    assert_eq!(ui.notetypes().len(), 3, "declined: nothing deleted");
    press(&mut ui, &b, &[chr('d'), chr('y'), chr('y')]);
    assert_eq!(ui.notetypes().len(), 2);
    assert!(b.calls().iter().any(|c| matches!(
        c,
        NotetypeCommand::Delete {
            confirmed: true,
            ..
        }
    )));
    // The last note type cannot be deleted.
    b.types.borrow_mut().truncate(1);
    ui.on_changes(
        &b,
        &OpChanges {
            notetype: true,
            ..Default::default()
        },
    );
    press(&mut ui, &b, &[chr('d')]);
    assert!(ui.status().unwrap().contains("another note type"));
}

#[test]
fn fields_editor_moves_and_saves_after_the_full_sync_prompt() {
    let (mut ui, b) = open();
    press(&mut ui, &b, &[chr('f')]);
    let text = screen(&ui, 90, 24);
    assert!(text.contains("Fields of Basic (3 notes)"), "{text}");
    assert!(text.contains("> "), "{text}");
    assert!(text.contains("1. Front") && text.contains("sort"), "{text}");
    // Move Front down: the list reorders at once, saving is later.
    press(&mut ui, &b, &[chr('J')]);
    let Screen::Fields(s) = &ui.screen else {
        panic!()
    };
    assert_eq!(s.draft.fields[0].name, "Back");
    assert_eq!(s.draft.fields[1].ord, Some(0));
    assert_eq!(s.draft.sort_field, 1, "the sort field follows Front");
    assert!(screen(&ui, 90, 24).contains("Fields of Basic (3 notes) *"));
    // Save asks for the full sync first.
    press(
        &mut ui,
        &b,
        &[with(KeyCode::Char('s'), KeyModifiers::CONTROL)],
    );
    assert!(screen(&ui, 90, 24).contains("Full sync required"));
    let intent = press(&mut ui, &b, &[chr('y')]);
    assert!(matches!(intent, NotetypesIntent::Changed(_)));
    assert_eq!(ui.status(), Some("Saved"));
    let calls = b.calls();
    let saves: Vec<_> = calls
        .iter()
        .filter_map(|c| match c {
            NotetypeCommand::Save { confirmed, .. } => Some(*confirmed),
            _ => None,
        })
        .collect();
    assert_eq!(saves, [false, true]);
    assert!(!ui.is_dirty());
    // Escape on a clean editor goes back to the list at once.
    press(&mut ui, &b, &[key(KeyCode::Esc)]);
    assert!(ui.on_list());
}

#[test]
fn fields_editor_options_prompts_and_required_protection() {
    let (mut ui, b) = open();
    press(&mut ui, &b, &[chr('f')]);
    // Toggles.
    press(&mut ui, &b, &[chr('1'), chr('2'), chr('3'), chr('4')]);
    let Screen::Fields(s) = &ui.screen else {
        panic!()
    };
    let f = &s.draft.fields[0];
    assert!(f.rtl && f.plain_text && f.collapsed && f.exclude_from_search);
    let text = screen(&ui, 90, 24);
    assert!(text.contains("RTL, HTML, collapsed, no search"), "{text}");
    // Rename through a prompt.
    press(&mut ui, &b, &[chr('n')]);
    for _ in 0..8 {
        ui.on_key(&b, key(KeyCode::Backspace));
    }
    type_text(&mut ui, &b, "Question");
    press(&mut ui, &b, &[key(KeyCode::Enter)]);
    let Screen::Fields(s) = &ui.screen else {
        panic!()
    };
    assert_eq!(s.draft.fields[0].name, "Question");
    // A bad name is refused with the core's message.
    press(&mut ui, &b, &[chr('a')]);
    type_text(&mut ui, &b, "a:b");
    press(&mut ui, &b, &[key(KeyCode::Enter)]);
    assert!(ui.status().unwrap().contains("cannot contain"));
    // Description.
    press(&mut ui, &b, &[chr('e')]);
    type_text(&mut ui, &b, "The question");
    press(&mut ui, &b, &[key(KeyCode::Enter)]);
    assert!(screen(&ui, 90, 24).contains("Description: The question"));
    // Escape with unsaved changes asks; n keeps editing, y discards.
    press(&mut ui, &b, &[key(KeyCode::Esc)]);
    assert!(screen(&ui, 90, 24).contains("Discard the changes"));
    press(&mut ui, &b, &[chr('n')]);
    assert!(!ui.on_list());
    press(&mut ui, &b, &[key(KeyCode::Esc), chr('y')]);
    assert!(ui.on_list());

    // Cloze: the text field is protected.
    press(&mut ui, &b, &[chr('j'), chr('f'), chr('d')]);
    assert!(ui.status().unwrap().contains("required by this note type"));
    assert!(screen(&ui, 90, 24).contains("required"));
    press(&mut ui, &b, &[chr('n')]);
    type_text(&mut ui, &b, "X");
    press(&mut ui, &b, &[key(KeyCode::Enter)]);
    assert!(
        ui.status().unwrap().contains("cannot be renamed"),
        "{:?}",
        ui.status()
    );
    // The other field can be deleted after confirming.
    press(&mut ui, &b, &[chr('j'), chr('d')]);
    assert!(screen(&ui, 90, 24).contains("Delete the field Back Extra?"));
    press(&mut ui, &b, &[chr('y')]);
    let Screen::Fields(s) = &ui.screen else {
        panic!()
    };
    assert_eq!(s.draft.fields.len(), 1);
}

fn open_cards(ui: &mut NotetypesUi, b: &Fake) {
    press(ui, b, &[chr('c')]);
}

#[test]
fn templates_editor_shows_a_live_preview_and_updates_it_while_typing() {
    let (mut ui, b) = open();
    open_cards(&mut ui, &b);
    let text = screen(&ui, 120, 20);
    assert!(text.contains("Cards of Basic: [Card 1]"), "{text}");
    assert!(
        text.contains("Front") && text.contains("Browser back"),
        "{text}"
    );
    assert!(text.contains("Preview (note 1/2)"), "{text}");
    assert!(text.contains("FRONT TEXT"), "{text}");
    let before = b.called("Preview");
    // Typing at the end of the front template re-renders.
    type_text(&mut ui, &b, " tail");
    assert!(b.called("Preview") > before);
    let text = screen(&ui, 120, 20);
    assert!(text.contains("{{Front}} tail"), "{text}");
    assert!(text.contains("FRONT TEXT tail"), "{text}");
    assert!(text.contains("Cards of Basic *: [Card 1]"), "{text}");
    // The last preview request carried the draft, not the stored template.
    let Some(NotetypeCommand::Preview {
        template,
        note_id,
        css,
        ..
    }) = b.calls().last().cloned()
    else {
        panic!()
    };
    assert_eq!(template.question, "{{Front}} tail");
    assert_eq!(note_id, Some(501));
    assert_eq!(css.as_deref(), Some(".card { color: black }"));
    // Alt+N previews the next note; Alt+F toggles filling empty fields.
    press(&mut ui, &b, &[with(KeyCode::Char('n'), KeyModifiers::ALT)]);
    let Some(NotetypeCommand::Preview { note_id, .. }) = b.calls().last().cloned() else {
        panic!()
    };
    assert_eq!(note_id, Some(502));
    // Unknown fields show inline from the preview.
    for _ in 0..5 {
        ui.on_key(&b, key(KeyCode::Backspace));
    }
    type_text(&mut ui, &b, "{{Nope}}");
    let text = screen(&ui, 120, 20);
    assert!(text.contains("Field 'Nope' not found"), "{text}");
}

#[test]
fn templates_editor_tabs_save_rejection_and_external_edit() {
    let (mut ui, b) = open();
    open_cards(&mut ui, &b);
    // Tab to Back, then Styling: each part has its own text.
    press(&mut ui, &b, &[key(KeyCode::Tab)]);
    let Screen::Templates(s) = &ui.screen else {
        panic!()
    };
    assert_eq!(s.area.value(), "{{FrontSide}}<hr id=answer>{{Back}}");
    press(&mut ui, &b, &[key(KeyCode::Tab)]);
    let Screen::Templates(s) = &ui.screen else {
        panic!()
    };
    assert_eq!(s.area.value(), ".card { color: black }");
    press(&mut ui, &b, &[key(KeyCode::BackTab), key(KeyCode::BackTab)]);

    // $EDITOR hand-over: the intent carries the text; the result lands in
    // the draft and re-renders.
    let intent = press(&mut ui, &b, &[with(KeyCode::Char('x'), KeyModifiers::ALT)]);
    assert_eq!(
        intent,
        NotetypesIntent::ExternalEdit {
            text: "{{Front}}".into(),
            suffix: ".html"
        }
    );
    ui.apply_external(&b, "<b>{{Front}}</b>".into());
    assert!(screen(&ui, 120, 20).contains("<b>{{Front}}</b>"));
    assert!(ui.is_dirty());

    // Anki's rejection is shown inline and nothing is saved.
    *b.reject.borrow_mut() = Some(TemplateProblem {
        kind: TemplateProblemKind::NoFrontField,
        template: 0,
        message: "The front of this card type has no field.".into(),
    });
    press(
        &mut ui,
        &b,
        &[with(KeyCode::Char('s'), KeyModifiers::CONTROL)],
    );
    let text = screen(&ui, 120, 20);
    assert!(
        text.contains("Card 1: The front of this card type has no field."),
        "{text}"
    );
    assert!(ui.is_dirty());
    assert_eq!(b.types.borrow()[0].templates[0].question, "{{Front}}");
    // Fixing and saving works (no structural change: no full-sync prompt).
    *b.reject.borrow_mut() = None;
    let intent = press(
        &mut ui,
        &b,
        &[with(KeyCode::Char('s'), KeyModifiers::CONTROL)],
    );
    assert!(matches!(intent, NotetypesIntent::Changed(_)));
    assert_eq!(
        b.types.borrow()[0].templates[0].question,
        "<b>{{Front}}</b>"
    );
    assert!(!ui.is_dirty());
}

#[test]
fn template_structure_edits_and_deck_override() {
    let (mut ui, b) = open();
    press(&mut ui, &b, &[chr('j'), chr('j'), chr('c')]);
    assert!(screen(&ui, 120, 20).contains("[One] [Two]"));
    // Alt+] moves to the second card type and shows its front.
    press(&mut ui, &b, &[with(KeyCode::Char(']'), KeyModifiers::ALT)]);
    assert!(screen(&ui, 120, 20).contains("{{B}}"));
    // Move it earlier (Alt+,): the names swap.
    press(&mut ui, &b, &[with(KeyCode::Char(','), KeyModifiers::ALT)]);
    let Screen::Templates(s) = &ui.screen else {
        panic!()
    };
    assert_eq!(s.draft.templates[0].name, "Two");
    assert_eq!(s.index, 0);
    // Add a card type, rename it, delete it.
    press(&mut ui, &b, &[with(KeyCode::Char('a'), KeyModifiers::ALT)]);
    assert!(screen(&ui, 120, 20).contains("starts as a copy"));
    press(&mut ui, &b, &[key(KeyCode::Enter)]);
    let Screen::Templates(s) = &ui.screen else {
        panic!()
    };
    assert_eq!(s.draft.templates.len(), 3);
    assert_eq!(s.draft.templates[2].ord, None);
    assert_eq!(s.index, 2);
    press(&mut ui, &b, &[with(KeyCode::Char('d'), KeyModifiers::ALT)]);
    assert!(screen(&ui, 120, 20).contains("Delete the card type Card 3?"));
    press(&mut ui, &b, &[chr('y')]);
    let Screen::Templates(s) = &ui.screen else {
        panic!()
    };
    assert_eq!(s.draft.templates.len(), 2);
    // Deck override by name; an unknown deck is an error.
    press(&mut ui, &b, &[with(KeyCode::Char('['), KeyModifiers::ALT)]);
    press(&mut ui, &b, &[with(KeyCode::Char('o'), KeyModifiers::ALT)]);
    type_text(&mut ui, &b, "Nowhere");
    press(&mut ui, &b, &[key(KeyCode::Enter)]);
    assert!(ui.status().unwrap().contains("no deck named Nowhere"));
    press(&mut ui, &b, &[with(KeyCode::Char('o'), KeyModifiers::ALT)]);
    type_text(&mut ui, &b, "study");
    press(&mut ui, &b, &[key(KeyCode::Enter)]);
    let Screen::Templates(s) = &ui.screen else {
        panic!()
    };
    assert_eq!(s.draft.templates[0].target_deck_id, Some(7));
    // Saving the reorder needs the full-sync confirmation.
    press(
        &mut ui,
        &b,
        &[with(KeyCode::Char('s'), KeyModifiers::CONTROL)],
    );
    assert!(screen(&ui, 120, 20).contains("Full sync required"));
    press(&mut ui, &b, &[chr('y')]);
    assert_eq!(b.types.borrow()[2].templates[0].name, "Two");
    assert_eq!(b.types.borrow()[2].templates[0].target_deck_id, Some(7));
}

#[test]
fn latex_options_edit_and_save() {
    let (mut ui, b) = open();
    press(&mut ui, &b, &[chr('o')]);
    let text = screen(&ui, 80, 20);
    assert!(text.contains("LaTeX options of Basic"), "{text}");
    assert!(
        text.contains("\\begin{document}") && text.contains("\\end{document}"),
        "{text}"
    );
    assert!(text.contains("[ ] Generate SVG"), "{text}");
    type_text(&mut ui, &b, "%x");
    press(
        &mut ui,
        &b,
        &[key(KeyCode::Tab), key(KeyCode::Tab), chr(' ')],
    );
    assert!(screen(&ui, 80, 20).contains("[x] Generate SVG"));
    let intent = press(
        &mut ui,
        &b,
        &[with(KeyCode::Char('s'), KeyModifiers::CONTROL)],
    );
    assert!(matches!(intent, NotetypesIntent::Changed(_)));
    let types = b.types.borrow();
    assert_eq!(types[0].latex_pre, "\\begin{document}%x");
    assert!(types[0].latex_svg);
}

#[test]
fn restore_needs_the_typed_name_and_a_stock_kind_when_unknown() {
    let (mut ui, b) = open();
    press(&mut ui, &b, &[chr('R')]);
    let text = screen(&ui, 90, 24);
    assert!(text.contains("Restore to the stock note type"), "{text}");
    assert!(text.contains("No field content is lost."), "{text}");
    // A wrong name is refused without calling the core.
    type_text(&mut ui, &b, "basic");
    press(&mut ui, &b, &[key(KeyCode::Enter)]);
    assert_eq!(b.called("Restore {"), 0);
    assert!(ui.status().unwrap().contains("exactly"));
    for _ in 0..5 {
        ui.on_key(&b, key(KeyCode::Backspace));
    }
    press(&mut ui, &b, &[key(KeyCode::Esc)]);
    assert!(b.called("Restore {") == 0);

    // The custom type has no recorded origin: choose one first; the extra
    // field's loss is announced.
    press(&mut ui, &b, &[chr('j'), chr('j'), chr('R')]);
    let text = screen(&ui, 90, 24);
    assert!(text.contains("Restore to which stock note type?"), "{text}");
    press(&mut ui, &b, &[key(KeyCode::Enter)]);
    let text = screen(&ui, 90, 24);
    assert!(
        text.contains("Content of these fields is lost: C"),
        "{text}"
    );
    type_text(&mut ui, &b, "Custom");
    let intent = press(&mut ui, &b, &[key(KeyCode::Enter)]);
    assert_eq!(intent, NotetypesIntent::None);
    assert!(screen(&ui, 90, 24).contains("Full sync required"));
    let intent = press(&mut ui, &b, &[chr('y')]);
    assert!(matches!(intent, NotetypesIntent::Changed(_)));
    let restores: Vec<_> = b
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            NotetypeCommand::Restore {
                force_kind,
                typed_confirmation,
                confirmed,
                ..
            } => Some((force_kind, typed_confirmation, confirmed)),
            _ => None,
        })
        .collect();
    assert_eq!(
        restores,
        [
            (Some(StockKind::Basic), true, false),
            (Some(StockKind::Basic), true, true)
        ]
    );
    assert_eq!(ui.selected().unwrap().field_count, 2);
}

fn change_screen(ui: &ChangeNotetypeUi) -> String {
    let area = Rect::new(0, 0, 80, 20);
    let mut buf = Buffer::empty(area);
    ui.render(area, &mut buf);
    buffer_text(&buf).join("\n")
}

#[test]
fn change_notetype_form_maps_fields_and_confirms_the_full_sync() {
    let b = Fake::new();
    let mut ui = ChangeNotetypeUi::new(&b, vec![501, 502]).unwrap();
    // Starts on the notes' own type (Basic -> Basic).
    let info = ui.info().unwrap();
    assert_eq!(info.new_notetype_name, "Basic");
    // Choose Custom (fields A, B, C): nothing matches by name.
    ui.on_key(&b, chr('t'));
    for c in "custom".chars() {
        ui.on_key(&b, chr(c));
    }
    ui.on_key(&b, key(KeyCode::Enter));
    let text = change_screen(&ui);
    assert!(
        text.contains("Change note type of 2 notes: Basic → Custom"),
        "{text}"
    );
    assert!(text.contains("A") && text.contains("(Nothing)"), "{text}");
    // Map A <- Back by cycling to the right, B <- Front.
    ui.on_key(&b, key(KeyCode::Right));
    ui.on_key(&b, key(KeyCode::Right));
    ui.on_key(&b, key(KeyCode::Down));
    ui.on_key(&b, key(KeyCode::Right));
    assert_eq!(ui.new_fields()[0], Some(1));
    assert_eq!(ui.new_fields()[1], Some(0));
    let text = change_screen(&ui);
    assert!(text.contains("A") && text.contains("←  Back"), "{text}");
    // Card types are mapped for standard types; unmapped old ones are warned about.
    assert!(ui.new_templates().is_some());
    assert!(change_screen(&ui).contains("Card types (new ← old):"));
    // Applying asks for the full sync, then changes.
    let intent = ui.on_key(&b, key(KeyCode::Enter));
    assert_eq!(intent, ChangeIntent::None);
    assert!(change_screen(&ui).contains("Full sync required"));
    let intent = ui.on_key(&b, chr('y'));
    assert!(matches!(intent, ChangeIntent::Changed { notes: 2, .. }));
    let Some(NotetypeCommand::Change {
        new_fields,
        new_notetype_id,
        confirmed,
        note_ids,
        ..
    }) = b.calls().last().cloned()
    else {
        panic!()
    };
    assert_eq!((new_notetype_id, confirmed), (3, true));
    assert_eq!(new_fields[..2], [Some(1), Some(0)]);
    assert_eq!(note_ids, [501, 502]);
    // d restores the default mapping; cloze targets have no template map.
    let mut ui = ChangeNotetypeUi::new(&b, vec![501]).unwrap();
    ui.on_key(&b, key(KeyCode::Right));
    ui.on_key(&b, chr('d'));
    assert_eq!(ui.new_fields(), [Some(0), Some(1)]);
    ui.on_key(&b, chr('t'));
    for c in "cloze".chars() {
        ui.on_key(&b, chr(c));
    }
    ui.on_key(&b, key(KeyCode::Enter));
    assert!(ui.new_templates().is_none());
    assert!(change_screen(&ui).contains("not mapped for cloze"));
    assert_eq!(ui.on_key(&b, key(KeyCode::Esc)), ChangeIntent::Close);
    assert!(ChangeNotetypeUi::new(&b, vec![]).is_err());
}
