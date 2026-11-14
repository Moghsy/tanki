use tanki_core::*;

fn run(actor: &CollectionActor, command: NoteCommand) -> Result<EditResponse> {
    match actor.execute(Command::Notes(command))? {
        Response::Edit(r) => Ok(r),
        other => panic!("unexpected {other:?}"),
    }
}

fn notetype(actor: &CollectionActor, name: &str) -> NotetypeInfo {
    let EditResponse::Notetypes(all) = run(actor, NoteCommand::Notetypes).unwrap() else {
        panic!("notetypes")
    };
    all.into_iter().find(|nt| nt.name == name).unwrap()
}

fn new_note(actor: &CollectionActor, notetype_id: i64, fields: &[&str]) -> NoteData {
    let EditResponse::Note(mut note) = run(actor, NoteCommand::NewNote { notetype_id }).unwrap()
    else {
        panic!("note")
    };
    note.fields = fields.iter().map(|f| f.to_string()).collect();
    note
}

fn check(actor: &CollectionActor, note: &NoteData) -> FieldsCheck {
    let EditResponse::FieldsCheck(c) =
        run(actor, NoteCommand::CheckFields { note: note.clone() }).unwrap()
    else {
        panic!("check")
    };
    c
}

fn card_count(actor: &CollectionActor) -> usize {
    let Response::CardIds(ids) = actor
        .execute(Command::Search {
            query: "deck:*".into(),
        })
        .unwrap()
    else {
        panic!("ids")
    };
    ids.len()
}

fn media(actor: &CollectionActor, command: NoteCommand) -> AddedMedia {
    let EditResponse::Media(m) = run(actor, command).unwrap() else {
        panic!("media")
    };
    m
}

#[test]
fn notetypes_defaults_and_decks() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actor = CollectionActor::open(dir.path())?;
    let EditResponse::Notetypes(all) = run(&actor, NoteCommand::Notetypes)? else {
        panic!()
    };
    let names: Vec<_> = all.iter().map(|n| n.name.to_lowercase()).collect();
    assert!(names.windows(2).all(|w| w[0] <= w[1]));
    let basic = notetype(&actor, "Basic");
    let cloze = notetype(&actor, "Cloze");
    assert_eq!(basic.kind, NotetypeKind::Normal);
    assert_eq!(cloze.kind, NotetypeKind::Cloze);
    assert!(all.iter().any(|n| n.image_occlusion));
    let EditResponse::Notetype(again) = run(&actor, NoteCommand::Notetype { id: basic.id })? else {
        panic!()
    };
    assert_eq!(again, basic);
    let EditResponse::Defaults {
        deck_id,
        notetype_id,
    } = run(&actor, NoteCommand::DefaultsForAdding { home_deck: 0 })?
    else {
        panic!()
    };
    assert!(deck_id > 0 && notetype_id > 0);
    // Nothing was added with this note type yet.
    assert!(matches!(
        run(
            &actor,
            NoteCommand::DefaultDeckForNotetype {
                notetype_id: cloze.id
            }
        )?,
        EditResponse::Deck(None)
    ));
    for include_filtered in [false, true] {
        let EditResponse::DeckNames(decks) =
            run(&actor, NoteCommand::DeckNames { include_filtered })?
        else {
            panic!()
        };
        assert!(decks.iter().any(|d| d.name == "Default" && !d.filtered));
    }
    actor.close()
}

#[test]
fn check_add_get_and_defaults_follow_the_last_add() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actor = CollectionActor::open(dir.path())?;
    let changes = actor.subscribe();
    let basic = notetype(&actor, "Basic");
    let cloze = notetype(&actor, "Cloze");
    let note = new_note(&actor, basic.id, &["", ""]);
    assert_eq!(note.id, 0);
    assert_eq!(check(&actor, &note), FieldsCheck::Empty);
    let note = new_note(&actor, basic.id, &["front", "back"]);
    assert_eq!(check(&actor, &note), FieldsCheck::Normal);
    let EditResponse::NoteAdded { id, changes: c } = run(
        &actor,
        NoteCommand::AddNote {
            note: note.clone(),
            deck_id: 1,
        },
    )?
    else {
        panic!()
    };
    assert!(id > 0 && c.note);
    assert!(changes.recv().unwrap().note);
    assert_eq!(check(&actor, &note), FieldsCheck::Duplicate);
    let EditResponse::Note(got) = run(&actor, NoteCommand::GetNote { id })? else {
        panic!()
    };
    assert_eq!(got.fields, ["front", "back"]);
    assert_eq!(got.notetype_id, basic.id);
    let EditResponse::Defaults { notetype_id, .. } =
        run(&actor, NoteCommand::DefaultsForAdding { home_deck: 0 })?
    else {
        panic!()
    };
    assert_eq!(notetype_id, basic.id);
    assert!(matches!(
        run(
            &actor,
            NoteCommand::DefaultDeckForNotetype {
                notetype_id: basic.id
            }
        )?,
        EditResponse::Deck(Some(1))
    ));
    // Cloze checks.
    assert_eq!(
        check(&actor, &new_note(&actor, cloze.id, &["no cloze", ""])),
        FieldsCheck::MissingCloze
    );
    assert_eq!(
        check(&actor, &new_note(&actor, basic.id, &["{{c1::x}}", "b"])),
        FieldsCheck::NotetypeNotCloze
    );
    assert_eq!(
        check(&actor, &new_note(&actor, cloze.id, &["{{c1::x}}", ""])),
        FieldsCheck::Normal
    );
    actor.close()
}

#[test]
fn add_notes_is_one_undo_step_and_get_notes_keeps_order() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actor = CollectionActor::open(dir.path())?;
    let basic = notetype(&actor, "Basic");
    let notes = (0..3)
        .map(|i| (new_note(&actor, basic.id, &[&format!("q{i}"), "a"]), 1))
        .collect();
    let EditResponse::NotesAdded { ids, changes } = run(&actor, NoteCommand::AddNotes { notes })?
    else {
        panic!()
    };
    assert_eq!(ids.len(), 3);
    assert!(changes.note);
    assert_eq!(card_count(&actor), 3);
    let reversed: Vec<i64> = ids.iter().rev().copied().collect();
    let EditResponse::Notes(got) = run(
        &actor,
        NoteCommand::GetNotes {
            ids: reversed.clone(),
        },
    )?
    else {
        panic!()
    };
    assert_eq!(got.iter().map(|n| n.id).collect::<Vec<_>>(), reversed);
    assert_eq!(got[0].fields[0], "q2");
    assert!(run(
        &actor,
        NoteCommand::GetNotes {
            ids: vec![ids[0], 42]
        }
    )
    .is_err());
    assert!(run(&actor, NoteCommand::GetNote { id: 42 }).is_err());
    actor.execute(Command::Undo)?;
    assert_eq!(card_count(&actor), 0);
    actor.close()
}

#[test]
fn cloze_numbers_and_previews() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actor = CollectionActor::open(dir.path())?;
    let basic = notetype(&actor, "Basic");
    let cloze = notetype(&actor, "Cloze");
    let note = new_note(
        &actor,
        cloze.id,
        &["{{c1,3::x}} {{c2::y}} {{c3::z}}", "extra"],
    );
    let EditResponse::Numbers(n) = run(&actor, NoteCommand::ClozeNumbers { note: note.clone() })?
    else {
        panic!()
    };
    assert_eq!(n, [1, 2, 3]);
    // Card ord is the cloze number - 1.
    let EditResponse::Rendered(card) = run(
        &actor,
        NoteCommand::RenderPreview {
            note: note.clone(),
            card_ord: 1,
        },
    )?
    else {
        panic!()
    };
    assert_eq!(card.ord, 1);
    assert!(card.question.contains("[...]"), "{}", card.question);
    assert!(card.question.contains('x'), "{}", card.question);
    assert!(card.answer.contains("extra"));
    let basic_note = new_note(&actor, basic.id, &["unsaved front", "unsaved back"]);
    let EditResponse::Rendered(card) = run(
        &actor,
        NoteCommand::RenderPreview {
            note: basic_note,
            card_ord: 0,
        },
    )?
    else {
        panic!()
    };
    assert!(card.question.contains("unsaved front"));
    assert!(card.answer.contains("unsaved back"));
    assert_eq!(card_count(&actor), 0);
    actor.close()
}

#[test]
fn sticky_persists_and_emits_changes() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actor = CollectionActor::open(dir.path())?;
    let changes = actor.subscribe();
    let basic = notetype(&actor, "Basic");
    let EditResponse::Changes(c) = run(
        &actor,
        NoteCommand::SetSticky {
            notetype_id: basic.id,
            field_ord: 1,
            sticky: true,
        },
    )?
    else {
        panic!()
    };
    assert!(c.notetype);
    assert!(changes.recv().unwrap().notetype);
    actor.close()?;
    // The flag survives a reopen.
    let actor = CollectionActor::open(dir.path())?;
    let basic = notetype(&actor, "Basic");
    assert!(!basic.fields[0].sticky && basic.fields[1].sticky);
    actor.close()
}

#[test]
fn tags_complete_tree_add_remove_clear() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actor = CollectionActor::open(dir.path())?;
    let changes = actor.subscribe();
    let basic = notetype(&actor, "Basic");
    let mut note = new_note(&actor, basic.id, &["f", "b"]);
    note.tags = vec!["lang::french".into()];
    let EditResponse::NoteAdded { id, .. } =
        run(&actor, NoteCommand::AddNote { note, deck_id: 1 })?
    else {
        panic!()
    };
    while changes.try_recv().is_ok() {}
    let EditResponse::Count { count, changes: c } = run(
        &actor,
        NoteCommand::AddTags {
            note_ids: vec![id],
            tags: "lang::german extra".into(),
        },
    )?
    else {
        panic!()
    };
    assert_eq!(count, 1);
    assert!(c.tag || c.note);
    assert!(changes.recv().is_ok());
    let EditResponse::Strings(all) = run(&actor, NoteCommand::AllTags)? else {
        panic!()
    };
    assert_eq!(all, ["extra", "lang::french", "lang::german"]);
    let EditResponse::Strings(found) = run(
        &actor,
        NoteCommand::CompleteTag {
            input: "lang::g".into(),
            limit: 5,
        },
    )?
    else {
        panic!()
    };
    assert_eq!(found, ["lang::german"]);
    let EditResponse::TagTree(root) = run(&actor, NoteCommand::TagTree)? else {
        panic!()
    };
    assert_eq!(root.full_name, "");
    let lang = root.children.iter().find(|n| n.name == "lang").unwrap();
    assert_eq!(lang.full_name, "lang");
    assert_eq!(
        lang.children
            .iter()
            .map(|n| n.full_name.as_str())
            .collect::<Vec<_>>(),
        ["lang::french", "lang::german"]
    );
    // Removing the parent removes its children.
    let EditResponse::Count { count, .. } = run(
        &actor,
        NoteCommand::RemoveTags {
            note_ids: vec![id],
            tags: "lang".into(),
        },
    )?
    else {
        panic!()
    };
    assert_eq!(count, 1);
    let EditResponse::Note(got) = run(&actor, NoteCommand::GetNote { id })? else {
        panic!()
    };
    assert_eq!(got.tags, ["extra"]);
    let EditResponse::Count { count, .. } = run(&actor, NoteCommand::ClearUnusedTags)? else {
        panic!()
    };
    assert_eq!(count, 2);
    let EditResponse::Strings(all) = run(&actor, NoteCommand::AllTags)? else {
        panic!()
    };
    assert_eq!(all, ["extra"]);
    actor.close()
}

#[test]
fn media_from_bytes_path_and_file_url() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actor = CollectionActor::open(dir.path())?;
    let m = media(
        &actor,
        NoteCommand::AddMedia {
            desired_name: "Cat Pic.PNG".into(),
            data: b"png-bytes".to_vec(),
        },
    );
    assert_eq!(m.filename, "cat pic.png");
    assert_eq!(m.field_text, "<img src=\"cat%20pic.png\">");
    assert!(actor.media_dir().join(&m.filename).is_file());
    // Same name, other content: de-duplicated, the returned name is used.
    let m2 = media(
        &actor,
        NoteCommand::AddMedia {
            desired_name: "CAT PIC.png".into(),
            data: b"other".to_vec(),
        },
    );
    assert_ne!(m2.filename, m.filename);
    assert!(actor.media_dir().join(&m2.filename).is_file());

    let src = tempfile::tempdir()?;
    let path = src.path().join("Clip.MP3");
    std::fs::write(&path, b"mp3")?;
    let m = media(&actor, NoteCommand::AddMediaFromPath { path });
    assert_eq!(m.filename, "clip.mp3");
    assert_eq!(m.field_text, "[sound:clip.mp3]");

    let file = src.path().join("Notes.TXT");
    std::fs::write(&file, b"text")?;
    let m = media(
        &actor,
        NoteCommand::AddMediaFromUrl {
            url: format!("file://{}", file.display()),
        },
    );
    assert_eq!(m.filename, "notes.txt");
    assert_eq!(m.field_text, "notes.txt");
    assert!(run(
        &actor,
        NoteCommand::AddMediaFromUrl {
            url: format!("file://{}", src.path().join("missing.png").display())
        }
    )
    .is_err());
    actor.close()
}

#[test]
fn html_to_text_and_image_occlusion_refused() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actor = CollectionActor::open(dir.path())?;
    let EditResponse::Text(t) = run(
        &actor,
        NoteCommand::HtmlToText {
            html: "<b>bold</b><br>line &amp; <img src=\"a.png\">".into(),
            preserve_media_filenames: true,
        },
    )?
    else {
        panic!()
    };
    assert!(
        t.contains("bold") && t.contains("& ") && t.contains("a.png"),
        "{t}"
    );
    let io = notetype(&actor, "Image Occlusion");
    assert!(io.image_occlusion);
    let note = new_note(&actor, io.id, &vec!["x"; io.fields.len()]);
    assert!(matches!(
        run(
            &actor,
            NoteCommand::AddNote {
                note: note.clone(),
                deck_id: 1
            }
        ),
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        run(
            &actor,
            NoteCommand::AddNotes {
                notes: vec![(note, 1)]
            }
        ),
        Err(CoreError::Invalid(_))
    ));
    assert_eq!(card_count(&actor), 0);
    actor.close()
}
