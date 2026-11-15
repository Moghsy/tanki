//! Phase 5 note type management through a real actor on temporary profiles.
use tanki_core::notetypes::*;
use tanki_core::*;

struct Fixture {
    actor: CollectionActor,
    _dir: tempfile::TempDir,
}

impl Fixture {
    /// A fresh collection (stock note types, no notes).
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let actor = CollectionActor::open(dir.path()).unwrap();
        Self { actor, _dir: dir }
    }

    fn run(&self, command: NotetypeCommand) -> Result<NotetypeResponse> {
        match self
            .actor
            .execute(Command::Manage(ManageCommand::Notetypes(command)))?
        {
            Response::Manage(ManageResponse::Notetypes(r)) => Ok(r),
            other => panic!("unexpected {other:?}"),
        }
    }

    fn list(&self) -> Vec<NotetypeSummary> {
        let NotetypeResponse::List(all) = self.run(NotetypeCommand::List).unwrap() else {
            panic!("list")
        };
        all
    }

    fn id(&self, name: &str) -> i64 {
        self.list()
            .into_iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no note type {name}"))
            .id
    }

    fn get(&self, id: i64) -> NotetypeDetail {
        let NotetypeResponse::Detail(d) = self.run(NotetypeCommand::Get { id }).unwrap() else {
            panic!("detail")
        };
        d
    }

    fn edit(&self, id: i64, edits: Vec<NotetypeEdit>, confirmed: bool) -> Result<NotetypeDetail> {
        match self.run(NotetypeCommand::Edit {
            id,
            edits,
            confirmed,
        })? {
            NotetypeResponse::Saved { notetype, .. } => Ok(notetype),
            other => panic!("not saved: {other:?}"),
        }
    }

    fn rejected(&self, id: i64, edits: Vec<NotetypeEdit>) -> TemplateProblem {
        match self
            .run(NotetypeCommand::Edit {
                id,
                edits,
                confirmed: true,
            })
            .unwrap()
        {
            NotetypeResponse::Rejected(problem) => problem,
            other => panic!("expected a rejection: {other:?}"),
        }
    }

    fn add_note(&self, notetype: &str, fields: &[&str]) {
        self.actor
            .execute(Command::AddNote {
                notetype: notetype.into(),
                fields: fields.iter().map(|f| f.to_string()).collect(),
            })
            .unwrap();
    }

    fn note_ids(&self) -> Vec<i64> {
        let Response::Edit(EditResponse::Ids(ids)) = self
            .actor
            .execute(Command::Browse(BrowseCommand::Search {
                query: "".into(),
                mode: BrowseMode::Notes,
                order: SearchOrder::Default,
            }))
            .unwrap()
        else {
            panic!("ids")
        };
        ids
    }

    fn notes(&self) -> Vec<NoteData> {
        let ids = self.note_ids();
        let Response::Edit(EditResponse::Notes(notes)) = self
            .actor
            .execute(Command::Notes(NoteCommand::GetNotes { ids }))
            .unwrap()
        else {
            panic!("notes")
        };
        notes
    }

    fn card_count(&self) -> usize {
        let Response::CardIds(ids) = self
            .actor
            .execute(Command::Search {
                query: "deck:*".into(),
            })
            .unwrap()
        else {
            panic!("cards")
        };
        ids.len()
    }

    fn deck_id(&self, name: &str) -> i64 {
        let Response::Edit(EditResponse::DeckNames(decks)) = self
            .actor
            .execute(Command::Notes(NoteCommand::DeckNames {
                include_filtered: false,
            }))
            .unwrap()
        else {
            panic!("decks")
        };
        decks.into_iter().find(|d| d.name == name).unwrap().id
    }
}

fn field_names(d: &NotetypeDetail) -> Vec<&str> {
    d.fields.iter().map(|f| f.name.as_str()).collect()
}

#[test]
fn list_stock_kinds_add_clone_and_rename() -> Result<()> {
    let f = Fixture::new();
    let all = f.list();
    let basic = all.iter().find(|s| s.name == "Basic").unwrap();
    assert_eq!(basic.use_count, 0);
    assert_eq!(
        (basic.field_count, basic.template_count, basic.stock_kind),
        (2, 1, Some(StockKind::Basic))
    );
    assert!(all.iter().any(|s| s.kind == NotetypeKind::Cloze));
    let names: Vec<_> = all.iter().map(|s| s.name.to_lowercase()).collect();
    assert!(names.windows(2).all(|w| w[0] <= w[1]), "sorted by name");

    let NotetypeResponse::StockKinds(kinds) = f.run(NotetypeCommand::StockKinds)? else {
        panic!()
    };
    assert_eq!(kinds.len(), 6);
    assert_eq!(kinds[0].name, "Basic");

    // Add from stock: a name clash gets `+`, an explicit name is used.
    let NotetypeResponse::Added { id, changes } = f.run(NotetypeCommand::AddStock {
        kind: StockKind::BasicTyping,
        name: Some("Typing mine".into()),
    })?
    else {
        panic!("added")
    };
    assert!(changes.notetype);
    let typing = f.get(id);
    assert_eq!(typing.name, "Typing mine");
    assert_eq!(typing.stock_kind, Some(StockKind::BasicTyping));
    let NotetypeResponse::Added { id: second, .. } = f.run(NotetypeCommand::AddStock {
        kind: StockKind::Basic,
        name: None,
    })?
    else {
        panic!()
    };
    assert_eq!(f.get(second).name, "Basic+");

    // Clone: deep copy with new ids and "… copy".
    f.edit(
        id,
        vec![NotetypeEdit::SetCss(".card { color: red }".into())],
        false,
    )?;
    let NotetypeResponse::Added { id: copy, .. } =
        f.run(NotetypeCommand::Clone { id, name: None })?
    else {
        panic!()
    };
    assert_ne!(copy, id);
    let (a, b) = (f.get(id), f.get(copy));
    assert_eq!(b.name, "Typing mine copy");
    assert_eq!(field_names(&a), field_names(&b));
    assert_eq!(a.css, b.css);
    assert_eq!(a.templates.len(), b.templates.len());
    assert_eq!(a.templates[0].question, b.templates[0].question);
    assert_eq!(b.stock_kind, Some(StockKind::BasicTyping));
    // Cloning needs no confirmation and edits the copy only.
    f.edit(copy, vec![NotetypeEdit::Rename("Other".into())], false)?;
    assert_eq!(f.get(id).name, "Typing mine");
    assert!(f
        .run(NotetypeCommand::Clone {
            id,
            name: Some("  ".into())
        })
        .is_err());

    // Rename; quotes are removed, a clash gets `+`.
    let NotetypeResponse::Saved { notetype, .. } = f.run(NotetypeCommand::Rename {
        id: copy,
        name: "\"Basic\"".into(),
    })?
    else {
        panic!()
    };
    assert_eq!(notetype.name, "Basic++");
    assert!(f
        .run(NotetypeCommand::Rename {
            id: copy,
            name: "".into()
        })
        .is_err());
    Ok(())
}

#[test]
fn delete_is_guarded_and_keeps_one_note_type() -> Result<()> {
    let f = Fixture::new();
    let basic = f.id("Basic");
    f.add_note("Basic", &["q", "a"]);
    assert_eq!(
        f.list().iter().find(|s| s.id == basic).unwrap().use_count,
        1
    );
    assert_eq!(
        f.run(NotetypeCommand::Delete {
            id: basic,
            confirmed: false
        })
        .unwrap_err(),
        CoreError::SchemaChangeRequired
    );
    assert!(f.list().iter().any(|s| s.id == basic));
    let NotetypeResponse::Changed(changes) = f.run(NotetypeCommand::Delete {
        id: basic,
        confirmed: true,
    })?
    else {
        panic!()
    };
    assert!(changes.notetype);
    assert!(f.note_ids().is_empty(), "its notes went with it");
    // Delete the rest but one: the last is refused.
    let rest: Vec<_> = f.list().into_iter().map(|s| s.id).collect();
    for id in &rest[1..] {
        f.run(NotetypeCommand::Delete {
            id: *id,
            confirmed: true,
        })?;
    }
    assert!(matches!(
        f.run(NotetypeCommand::Delete {
            id: rest[0],
            confirmed: true
        }),
        Err(CoreError::Invalid(m)) if m.contains("another note type")
    ));
    assert_eq!(f.list().len(), 1);
    Ok(())
}

#[test]
fn schema_changing_field_edits_are_guarded_and_notes_follow() -> Result<()> {
    let f = Fixture::new();
    let basic = f.id("Basic");
    f.add_note("Basic", &["q1", "a1"]);
    f.add_note("Basic", &["q2", "a2"]);

    // Reorder: refused without confirmation, nothing changed.
    let swap = || vec![NotetypeEdit::MoveField { from: 1, to: 0 }];
    assert_eq!(
        f.edit(basic, swap(), false).unwrap_err(),
        CoreError::SchemaChangeRequired
    );
    assert_eq!(field_names(&f.get(basic)), ["Front", "Back"]);
    // Sort field, add and delete are guarded too.
    for edit in [
        NotetypeEdit::SetSortField { index: 1 },
        NotetypeEdit::AddField {
            name: "Extra".into(),
        },
        NotetypeEdit::DeleteField { index: 1 },
    ] {
        assert_eq!(
            f.edit(basic, vec![edit], false).unwrap_err(),
            CoreError::SchemaChangeRequired
        );
    }
    // Renaming, options, CSS and LaTeX are not schema changes.
    let renamed = f.edit(
        basic,
        vec![
            NotetypeEdit::RenameField {
                index: 0,
                name: "Question".into(),
            },
            NotetypeEdit::SetCss(".card { color: blue }".into()),
            NotetypeEdit::SetLatexPre("\\documentclass{article}".into()),
            NotetypeEdit::SetLatexSvg(true),
        ],
        false,
    )?;
    assert_eq!(field_names(&renamed), ["Question", "Back"]);
    assert_eq!(renamed.fields[0].ord, Some(0));
    assert_eq!(
        renamed.templates[0].question, "{{Question}}",
        "templates follow a rename"
    );
    assert!(renamed.latex_svg);
    assert!(renamed.latex_pre.contains("article"));
    assert_eq!(
        f.notes()[0].fields,
        ["q1", "a1"],
        "note fields stay in place"
    );

    // Confirmed: the fields swap places and every note's text follows.
    let swapped = f.edit(basic, swap(), true)?;
    assert_eq!(field_names(&swapped), ["Back", "Question"]);
    assert_eq!(swapped.sort_field, 1, "the sort field followed its field");
    let mut contents: Vec<_> = f.notes().into_iter().map(|n| n.fields).collect();
    contents.sort();
    assert_eq!(contents, [["a1", "q1"], ["a2", "q2"]]);
    assert_eq!(f.card_count(), 2);

    // Add keeps contents in place and appends an empty field.
    let added = f.edit(
        basic,
        vec![NotetypeEdit::AddField {
            name: "Extra".into(),
        }],
        true,
    )?;
    assert_eq!(field_names(&added), ["Back", "Question", "Extra"]);
    assert!(f
        .notes()
        .iter()
        .all(|n| n.fields.len() == 3 && n.fields[2].is_empty()));
    // Sort field change recomputes the sort field.
    f.edit(basic, vec![NotetypeEdit::SetSortField { index: 2 }], true)?;
    assert_eq!(f.get(basic).sort_field, 2);

    // Delete removes that column from every note.
    let deleted = f.edit(basic, vec![NotetypeEdit::DeleteField { index: 0 }], true)?;
    assert_eq!(field_names(&deleted), ["Question", "Extra"]);
    assert_eq!(deleted.sort_field, 1, "the sort field follows its field");
    let mut contents: Vec<_> = f.notes().into_iter().map(|n| n.fields).collect();
    contents.sort();
    assert_eq!(contents, [["q1", ""], ["q2", ""]]);
    // Deleting the sort field itself falls back to the first field.
    let d = f.edit(basic, vec![NotetypeEdit::DeleteField { index: 1 }], true)?;
    assert_eq!(field_names(&d), ["Question"]);
    assert_eq!(d.sort_field, 0);
    f.edit(
        basic,
        vec![NotetypeEdit::AddField {
            name: "Extra".into(),
        }],
        true,
    )?;
    // Several edits in one go keep ordinals consistent.
    let d = f.edit(
        basic,
        vec![
            NotetypeEdit::AddField { name: "Z".into() },
            NotetypeEdit::MoveField { from: 2, to: 0 },
            NotetypeEdit::RenameField {
                index: 1,
                name: "Q".into(),
            },
        ],
        true,
    )?;
    assert_eq!(field_names(&d), ["Z", "Q", "Extra"]);
    assert_eq!(
        d.fields.iter().map(|f| f.ord).collect::<Vec<_>>(),
        [Some(0), Some(1), Some(2)]
    );
    let contents: Vec<_> = f.notes().into_iter().map(|n| n.fields[1].clone()).collect();
    assert!(contents.contains(&"q1".to_string()));
    Ok(())
}

#[test]
fn field_options_persist_without_a_schema_change() -> Result<()> {
    let f = Fixture::new();
    let basic = f.id("Basic");
    let mut options = f.get(basic).fields[1].clone();
    options.rtl = true;
    options.plain_text = true;
    options.collapsed = true;
    options.exclude_from_search = true;
    options.description = "Answer here".into();
    options.font_name = "Courier".into();
    options.font_size = 14;
    options.sticky = true;
    let d = f.edit(
        basic,
        vec![NotetypeEdit::SetFieldOptions {
            index: 1,
            field: options,
        }],
        false,
    )?;
    let back = &d.fields[1];
    assert!(
        back.rtl && back.plain_text && back.collapsed && back.exclude_from_search && back.sticky
    );
    assert_eq!(
        (
            back.description.as_str(),
            back.font_name.as_str(),
            back.font_size
        ),
        ("Answer here", "Courier", 14)
    );
    // The Phase 3 view agrees.
    let Response::Edit(EditResponse::Notetype(info)) = f
        .actor
        .execute(Command::Notes(NoteCommand::Notetype { id: basic }))?
    else {
        panic!()
    };
    assert!(info.fields[1].rtl && info.fields[1].plain_text && info.fields[1].collapsed);
    assert_eq!(info.fields[1].description, "Answer here");
    // Invalid names are refused before Anki is called.
    for name in ["", "#x", "a:b"] {
        assert!(matches!(
            f.edit(
                basic,
                vec![NotetypeEdit::AddField { name: name.into() }],
                true
            ),
            Err(CoreError::Invalid(_))
        ));
    }
    assert!(f
        .edit(
            basic,
            vec![NotetypeEdit::AddField {
                name: "front".into()
            }],
            true
        )
        .is_err());
    Ok(())
}

#[test]
fn required_fields_are_protected_even_when_confirmed() -> Result<()> {
    let f = Fixture::new();
    let cloze = f.id("Cloze");
    let d = f.get(cloze);
    assert!(d.fields[0].required, "the cloze text field");
    assert!(!d.fields[1].required);
    let refused = |edits| match f.edit(cloze, edits, true) {
        Err(CoreError::Invalid(m)) => m,
        other => panic!("expected a refusal: {other:?}"),
    };
    assert!(refused(vec![NotetypeEdit::DeleteField { index: 0 }]).contains("required"));
    assert!(refused(vec![NotetypeEdit::RenameField {
        index: 0,
        name: "Body".into()
    }])
    .contains("required"));
    // A hand-made draft that drops or renames the field is refused as well.
    let mut draft = d.clone();
    draft.fields.remove(0);
    draft.sort_field = 0;
    assert!(matches!(
        f.run(NotetypeCommand::Save { notetype: draft, confirmed: true }),
        Err(CoreError::Invalid(m)) if m.contains("cannot be deleted")
    ));
    let mut draft = d.clone();
    draft.fields[0].name = "Body".into();
    assert!(matches!(
        f.run(NotetypeCommand::Save { notetype: draft, confirmed: true }),
        Err(CoreError::Invalid(m)) if m.contains("cannot be renamed")
    ));
    assert_eq!(field_names(&f.get(cloze)), ["Text", "Back Extra"]);
    // The other field can go.
    let d = f.edit(cloze, vec![NotetypeEdit::DeleteField { index: 1 }], true)?;
    assert_eq!(field_names(&d), ["Text"]);

    // Image occlusion: its fields are flagged `prevent_deletion`.
    let io = f.id("Image Occlusion");
    let d = f.get(io);
    assert!(d.fields.iter().filter(|x| x.required).count() >= 4);
    assert!(matches!(
        f.edit(io, vec![NotetypeEdit::DeleteField { index: 1 }], true),
        Err(CoreError::Invalid(_))
    ));
    Ok(())
}

#[test]
fn template_edits_preserve_cards_and_validation_is_typed() -> Result<()> {
    let f = Fixture::new();
    let nt = f.id("Basic (and reversed card)");
    f.add_note("Basic (and reversed card)", &["front", "back"]);
    assert_eq!(f.card_count(), 2);
    let d = f.get(nt);
    assert_eq!(d.templates.len(), 2);

    // Editing text is not a schema change.
    let mut t = d.templates[0].clone();
    t.question = "<div>{{Front}}</div>".into();
    t.answer = "{{FrontSide}}<hr id=answer>{{Back}}".into();
    t.browser_question = "{{Front}}".into();
    t.browser_font_name = "Mono".into();
    t.browser_font_size = 12;
    let d = f.edit(
        nt,
        vec![NotetypeEdit::SetTemplate {
            index: 0,
            template: t,
        }],
        false,
    )?;
    assert_eq!(d.templates[0].question, "<div>{{Front}}</div>");
    assert_eq!(d.templates[0].browser_question, "{{Front}}");
    assert_eq!(d.templates[0].browser_font_size, 12);
    assert_eq!(f.card_count(), 2);

    // Reorder: guarded; cards follow (the card of the old second template
    // now has ordinal 0).
    assert_eq!(
        f.edit(
            nt,
            vec![NotetypeEdit::MoveTemplate { from: 1, to: 0 }],
            false
        )
        .unwrap_err(),
        CoreError::SchemaChangeRequired
    );
    let d = f.edit(
        nt,
        vec![NotetypeEdit::MoveTemplate { from: 1, to: 0 }],
        true,
    )?;
    assert_eq!(d.templates[0].name, "Card 2");
    assert_eq!(d.templates[1].name, "Card 1");
    assert_eq!(f.card_count(), 2);
    // Rename is not a schema change.
    let d = f.edit(
        nt,
        vec![NotetypeEdit::RenameTemplate {
            index: 0,
            name: "Reverse".into(),
        }],
        false,
    )?;
    assert_eq!(d.templates[0].name, "Reverse");
    // Delete removes the cards of that card type.
    let d = f.edit(nt, vec![NotetypeEdit::DeleteTemplate { index: 0 }], true)?;
    assert_eq!(d.templates.len(), 1);
    assert_eq!(f.card_count(), 1);
    assert!(f
        .edit(nt, vec![NotetypeEdit::DeleteTemplate { index: 0 }], true)
        .is_err());

    // Typed validation errors (nothing is written).
    let before = f.get(nt);
    let template = || before.templates[0].clone();
    let set = |question: &str, answer: &str| {
        let mut t = template();
        t.question = question.into();
        t.answer = answer.into();
        NotetypeEdit::SetTemplate {
            index: 0,
            template: t,
        }
    };
    let p = f.rejected(nt, vec![set("{{Nope}}", "{{Back}}")]);
    assert_eq!(p.kind, TemplateProblemKind::UnknownField("Nope".into()));
    assert!(p.message.contains("Nope"), "{}", p.message);
    let p = f.rejected(nt, vec![set("{{#Front}}", "{{Back}}")]);
    assert_eq!(p.kind, TemplateProblemKind::ParseError);
    let p = f.rejected(nt, vec![set("just text", "{{Back}}")]);
    assert_eq!(p.kind, TemplateProblemKind::NoFrontField);
    let p = f.rejected(nt, vec![NotetypeEdit::AddTemplate { name: "Dup".into() }]);
    assert_eq!(p.kind, TemplateProblemKind::DuplicateFront { other: 0 });
    assert_eq!(p.template, 1);
    assert_eq!(f.get(nt), before);

    let cloze = f.id("Cloze");
    let mut t = f.get(cloze).templates[0].clone();
    t.question = "{{Text}}".into();
    let p = f.rejected(
        cloze,
        vec![NotetypeEdit::SetTemplate {
            index: 0,
            template: t,
        }],
    );
    assert_eq!(p.kind, TemplateProblemKind::MissingCloze);
    assert!(f
        .edit(
            cloze,
            vec![NotetypeEdit::AddTemplate { name: "x".into() }],
            true
        )
        .is_err());
    Ok(())
}

#[test]
fn deck_override_must_be_a_normal_deck() -> Result<()> {
    let f = Fixture::new();
    let nt = f.id("Basic");
    let default = f.deck_id("Default");
    let mut t = f.get(nt).templates[0].clone();
    t.target_deck_id = Some(default);
    let d = f.edit(
        nt,
        vec![NotetypeEdit::SetTemplate {
            index: 0,
            template: t.clone(),
        }],
        false,
    )?;
    assert_eq!(d.templates[0].target_deck_id, Some(default));
    t.target_deck_id = Some(987654321);
    assert!(matches!(
        f.edit(
            nt,
            vec![NotetypeEdit::SetTemplate {
                index: 0,
                template: t.clone()
            }],
            false
        ),
        Err(CoreError::Invalid(_))
    ));
    t.target_deck_id = None;
    let d = f.edit(
        nt,
        vec![NotetypeEdit::SetTemplate {
            index: 0,
            template: t,
        }],
        false,
    )?;
    assert_eq!(d.templates[0].target_deck_id, None);
    Ok(())
}

#[test]
fn preview_renders_the_uncommitted_template_for_a_note_or_an_empty_one() -> Result<()> {
    let f = Fixture::new();
    let nt = f.id("Basic");
    f.add_note("Basic", &["Hello", "World"]);
    let note_id = f.note_ids()[0];
    let mut template = f.get(nt).templates[0].clone();
    template.question = "<b>{{Front}}</b> uncommitted".into();
    template.answer = "{{FrontSide}}<hr id=answer>{{Back}}!".into();
    let preview = |note_id, template: &TemplateDetail, fill_empty| {
        let NotetypeResponse::Preview(p) = f
            .run(NotetypeCommand::Preview {
                notetype_id: nt,
                note_id,
                template: template.clone(),
                css: Some(".card { color: green }".into()),
                card_ord: 0,
                fill_empty,
            })
            .unwrap()
        else {
            panic!()
        };
        p
    };
    let p = preview(Some(note_id), &template, false);
    assert!(p.error.is_none());
    assert!(
        p.card.question.contains("<b>Hello</b> uncommitted"),
        "{}",
        p.card.question
    );
    assert!(p.card.answer.contains("World!"), "{}", p.card.answer);
    assert_eq!(p.card.css, ".card { color: green }");
    // Nothing was saved.
    assert_eq!(f.get(nt).templates[0].question, "{{Front}}");
    // An empty note with "fill empty" shows the field names.
    let p = preview(None, &template, true);
    assert!(p.card.question.contains("Front"), "{}", p.card.question);
    // A template error is reported, with the text as the card.
    template.question = "{{Missing}}".into();
    let p = preview(Some(note_id), &template, false);
    let error = p.error.expect("error");
    assert!(error.contains("Missing"), "{error}");
    assert!(p.card.question.contains("Missing"), "{}", p.card.question);
    // A note of another note type is refused.
    f.add_note("Cloze", &["{{c1::x}}", ""]);
    let other = f.note_ids().into_iter().find(|id| *id != note_id).unwrap();
    assert!(matches!(
        f.run(NotetypeCommand::Preview {
            notetype_id: nt,
            note_id: Some(other),
            template,
            css: None,
            card_ord: 0,
            fill_empty: false,
        }),
        Err(CoreError::Invalid(_))
    ));
    Ok(())
}

#[test]
fn change_notetype_maps_fields_and_templates() -> Result<()> {
    let f = Fixture::new();
    let basic = f.id("Basic");
    let reversed = f.id("Basic (and reversed card)");
    f.add_note("Basic", &["q1", "a1"]);
    f.add_note("Basic", &["q2", "a2"]);
    let ids = f.note_ids();
    let NotetypeResponse::ChangeInfo(info) = f.run(NotetypeCommand::ChangeInfo {
        note_ids: ids.clone(),
        new_notetype_id: reversed,
    })?
    else {
        panic!()
    };
    assert_eq!(info.old_notetype_id, basic);
    assert_eq!(info.old_field_names, ["Front", "Back"]);
    assert_eq!(info.new_field_names, ["Front", "Back"]);
    assert_eq!(info.new_fields, [Some(0), Some(1)], "matched by name");
    assert_eq!(info.new_templates, Some(vec![Some(0), None]));
    assert_eq!(info.note_count, 2);

    // Guard first; the notes are untouched.
    let change = |fields: Vec<Option<u32>>, templates, confirmed| {
        f.run(NotetypeCommand::Change {
            note_ids: ids.clone(),
            new_notetype_id: reversed,
            new_fields: fields,
            new_templates: templates,
            confirmed,
        })
    };
    assert_eq!(
        change(info.new_fields.clone(), info.new_templates.clone(), false).unwrap_err(),
        CoreError::SchemaChangeRequired
    );
    assert_eq!(
        f.list().iter().find(|s| s.id == basic).unwrap().use_count,
        2
    );
    // Bad maps are refused.
    assert!(matches!(
        change(vec![Some(0)], info.new_templates.clone(), true),
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        change(vec![Some(0), Some(5)], info.new_templates.clone(), true),
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        change(info.new_fields.clone(), None, true),
        Err(CoreError::Invalid(_))
    ));
    // Swap the fields while changing.
    let NotetypeResponse::NotesChanged { count, changes } =
        change(vec![Some(1), Some(0)], Some(vec![Some(0), None]), true)?
    else {
        panic!()
    };
    assert_eq!(count, 2);
    assert!(changes.note);
    assert!(f.notes().iter().all(|n| n.notetype_id == reversed));
    let mut contents: Vec<_> = f.notes().into_iter().map(|n| n.fields).collect();
    contents.sort();
    assert_eq!(contents, [["a1", "q1"], ["a2", "q2"]]);
    assert_eq!(f.card_count(), 4, "the reverse cards were generated");

    // Mixed note types are refused; so is a map for a cloze type.
    f.add_note("Basic", &["q3", "a3"]);
    assert!(f
        .run(NotetypeCommand::ChangeInfo {
            note_ids: f.note_ids(),
            new_notetype_id: basic
        })
        .is_err());
    let cloze = f.id("Cloze");
    let one = vec![f.note_ids()[0]];
    let NotetypeResponse::ChangeInfo(info) = f.run(NotetypeCommand::ChangeInfo {
        note_ids: one.clone(),
        new_notetype_id: cloze,
    })?
    else {
        panic!()
    };
    assert_eq!(info.new_templates, None, "clozes cannot be remapped");
    assert!(matches!(
        f.run(NotetypeCommand::Change {
            note_ids: one,
            new_notetype_id: cloze,
            new_fields: info.new_fields,
            new_templates: Some(vec![Some(0)]),
            confirmed: true,
        }),
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        f.run(NotetypeCommand::ChangeInfo {
            note_ids: vec![],
            new_notetype_id: cloze
        }),
        Err(CoreError::Invalid(_))
    ));
    Ok(())
}

#[test]
fn restore_needs_typed_confirmation_and_replaces_fields() -> Result<()> {
    let f = Fixture::new();
    let basic = f.id("Basic");
    f.add_note("Basic", &["q", "a"]);
    f.edit(basic, vec![NotetypeEdit::SetCss("x".into())], false)?;
    // Plan before the edit adds an extra field: nothing is lost yet.
    let NotetypeResponse::RestorePlan(plan) = f.run(NotetypeCommand::RestorePlan {
        id: basic,
        force_kind: None,
    })?
    else {
        panic!()
    };
    assert_eq!(plan.kind, Some(StockKind::Basic));
    assert!(plan.dropped_fields.is_empty() && !plan.structure_changes);

    // Guard and typed confirmation are separate refusals; nothing changes.
    let restore = |typed, confirmed| {
        f.run(NotetypeCommand::Restore {
            id: basic,
            force_kind: None,
            typed_confirmation: typed,
            confirmed,
        })
    };
    assert!(matches!(restore(false, true), Err(CoreError::Invalid(_))));
    assert_eq!(
        restore(true, false).unwrap_err(),
        CoreError::SchemaChangeRequired
    );
    assert_eq!(f.get(basic).css, "x");

    // Extra field with data is lost on restore; the plan said so.
    f.edit(
        basic,
        vec![NotetypeEdit::AddField {
            name: "Extra".into(),
        }],
        true,
    )?;
    let NotetypeResponse::RestorePlan(plan) = f.run(NotetypeCommand::RestorePlan {
        id: basic,
        force_kind: None,
    })?
    else {
        panic!()
    };
    assert_eq!(plan.dropped_fields, ["Extra"]);
    assert!(plan.structure_changes);
    assert_eq!(plan.note_count, 1);
    let NotetypeResponse::Saved { notetype, changes } = restore(true, true)? else {
        panic!()
    };
    assert!(changes.notetype);
    assert_eq!(field_names(&notetype), ["Front", "Back"]);
    assert_ne!(notetype.css, "x");
    assert_eq!(f.notes()[0].fields, ["q", "a"]);
    Ok(())
}

#[test]
fn restore_of_an_unknown_origin_needs_a_chosen_kind() -> Result<()> {
    let f = Fixture::new();
    f.actor.execute(Command::GenerateFixture)?;
    let all = f.list();
    // The fixture has notes of several note types.
    let basic = all.iter().find(|s| s.name == "Basic").unwrap();
    assert!(basic.use_count > 0);
    let custom = all
        .iter()
        .filter(|s| s.stock_kind.is_none())
        .max_by_key(|s| s.field_count)
        .expect("a note type without a recorded origin");
    let NotetypeResponse::RestorePlan(plan) = f.run(NotetypeCommand::RestorePlan {
        id: custom.id,
        force_kind: None,
    })?
    else {
        panic!()
    };
    assert!(plan.needs_kind && plan.kind.is_none());
    assert!(matches!(
        f.run(NotetypeCommand::Restore { id: custom.id, force_kind: None, typed_confirmation: true, confirmed: true }),
        Err(CoreError::Invalid(m)) if m.contains("choose")
    ));
    let NotetypeResponse::RestorePlan(plan) = f.run(NotetypeCommand::RestorePlan {
        id: custom.id,
        force_kind: Some(StockKind::Basic),
    })?
    else {
        panic!()
    };
    assert_eq!(plan.stock_field_names, ["Front", "Back"]);
    assert!(
        !plan.dropped_fields.is_empty(),
        "the custom type has more fields"
    );
    f.run(NotetypeCommand::Restore {
        id: custom.id,
        force_kind: Some(StockKind::Basic),
        typed_confirmation: true,
        confirmed: true,
    })?;
    let restored = f.get(custom.id);
    assert_eq!(field_names(&restored), ["Front", "Back"]);
    assert_eq!(restored.stock_kind, Some(StockKind::Basic));
    Ok(())
}

#[test]
fn changes_are_broadcast_to_subscribers() -> Result<()> {
    let f = Fixture::new();
    let rx = f.actor.subscribe();
    let basic = f.id("Basic");
    f.edit(basic, vec![NotetypeEdit::SetCss("y".into())], false)?;
    let changes = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert!(changes.notetype);
    Ok(())
}
