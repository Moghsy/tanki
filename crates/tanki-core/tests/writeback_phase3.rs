//! `NoteCommand::UpdateNotes` through the actor on temp collections.
//!
//! Notes are created with Phase 1's `Command::AddNote`. Stored notes are read
//! directly through Anki while the actor is closed (as `tests/study.rs`
//! prepares collections), so only one opener exists at a time.
use anki::{collection::CollectionBuilder, services::NotesService};
use anki_proto as pb;
use std::path::Path;
use tanki_core::*;

fn add(actor: &CollectionActor, fields: [&str; 2]) -> i64 {
    match actor.execute(Command::AddNote {
        notetype: "Basic".into(),
        fields: fields.map(String::from).to_vec(),
    }) {
        Ok(Response::NoteAdded { id, .. }) => id,
        other => panic!("add: {other:?}"),
    }
}

/// A collection with one Basic note per entry; returns the note ids.
fn setup(notes: &[[&str; 2]]) -> (tempfile::TempDir, Vec<i64>) {
    let dir = tempfile::tempdir().unwrap();
    let actor = CollectionActor::open(dir.path()).unwrap();
    let ids = notes.iter().map(|fields| add(&actor, *fields)).collect();
    actor.close().unwrap();
    (dir, ids)
}

/// The stored notes; the actor must be closed.
fn read(dir: &Path, ids: &[i64]) -> Vec<pb::notes::Note> {
    let mut col = CollectionBuilder::new(dir.join("collection.anki2"))
        .build()
        .unwrap();
    let notes = ids
        .iter()
        .map(|&nid| NotesService::get_note(&mut col, pb::notes::NoteId { nid }).unwrap())
        .collect();
    col.close(None).unwrap();
    notes
}

fn update(actor: &CollectionActor, updates: Vec<NoteUpdate>) -> UpdateReport {
    match actor.execute(Command::Notes(NoteCommand::UpdateNotes { updates })) {
        Ok(Response::Edit(EditResponse::Updated(report))) => report,
        other => panic!("update: {other:?}"),
    }
}

fn change(note: &pb::notes::Note, fields: Vec<(u32, &str)>) -> NoteUpdate {
    NoteUpdate {
        id: note.id,
        expected_mtime: note.mtime_secs,
        expected_fields: Some(note.fields.clone()),
        fields: fields.into_iter().map(|(o, v)| (o, v.to_owned())).collect(),
        tags: None,
    }
}

fn undo_label(actor: &CollectionActor) -> Option<String> {
    match actor.execute(Command::UndoStatus) {
        Ok(Response::UndoStatus(status)) => status.undo,
        other => panic!("undo status: {other:?}"),
    }
}

#[test]
fn one_changed_field_leaves_the_others_byte_identical() {
    let back = "a<div>b</div>\n<span style=\"color: rgb(255, 0, 0);\">x</span>&nbsp; ";
    let (dir, ids) = setup(&[["front", back]]);
    let before = read(dir.path(), &ids).remove(0);
    let actor = CollectionActor::open(dir.path()).unwrap();
    let changes = actor.subscribe();
    let report = update(&actor, vec![change(&before, vec![(0, "new <b>front</b>")])]);
    assert_eq!(report.updated, ids);
    assert!(report.conflicts.is_empty() && report.missing.is_empty());
    assert!(report.changes.note);
    assert!(
        changes.recv().unwrap().note,
        "the actor broadcasts the update"
    );
    assert_eq!(undo_label(&actor).as_deref(), Some("Update Note"));
    actor.close().unwrap();
    let after = read(dir.path(), &ids).remove(0);
    assert_eq!(after.fields, ["new <b>front</b>", back]);
    assert_eq!(
        (&after.guid, &after.tags, after.notetype_id),
        (&before.guid, &before.tags, before.notetype_id)
    );
}

#[test]
fn conflicts_and_missing_notes_write_nothing() {
    let (dir, ids) = setup(&[["a", "b"], ["c", "d"]]);
    let before = read(dir.path(), &ids);
    let actor = CollectionActor::open(dir.path()).unwrap();

    let mut stale = change(&before[0], vec![(1, "x")]);
    stale.expected_mtime -= 1;
    let report = update(&actor, vec![stale]);
    assert_eq!(report.conflicts, [ids[0]]);
    assert!(report.updated.is_empty());

    let mut edited_meanwhile = change(&before[0], vec![(1, "x")]);
    edited_meanwhile.expected_fields = Some(vec!["a".into(), "older b".into()]);
    assert_eq!(update(&actor, vec![edited_meanwhile]).conflicts, [ids[0]]);

    let gone = NoteUpdate {
        id: 1,
        ..change(&before[0], vec![(0, "x")])
    };
    let report = update(&actor, vec![gone.clone()]);
    assert_eq!(report.missing, [1]);

    // All or nothing: a good update next to a conflict or a missing note.
    let good = change(&before[1], vec![(0, "changed")]);
    let mut bad = change(&before[0], vec![(0, "x")]);
    bad.expected_mtime += 7;
    let report = update(&actor, vec![good.clone(), bad]);
    assert_eq!((report.conflicts, report.updated), (vec![ids[0]], vec![]));
    let report = update(&actor, vec![good, gone]);
    assert_eq!((report.missing, report.updated), (vec![1], vec![]));
    assert_eq!(undo_label(&actor), None, "no undo step");
    actor.close().unwrap();
    assert_eq!(read(dir.path(), &ids), before);
}

#[test]
fn unchanged_notes_are_not_written() {
    let (dir, ids) = setup(&[["a", "b"]]);
    let before = read(dir.path(), &ids).remove(0);
    // mtime has one-second resolution.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let actor = CollectionActor::open(dir.path()).unwrap();
    let mut same = change(&before, vec![(0, "a"), (1, "b")]);
    same.tags = Some(before.tags.clone());
    let report = update(&actor, vec![same]);
    assert_eq!(report.unchanged, ids);
    assert!(report.updated.is_empty());
    assert_eq!(report.changes, OpChanges::default());
    assert_eq!(undo_label(&actor), None, "no undo step");
    actor.close().unwrap();
    let after = read(dir.path(), &ids).remove(0);
    assert_eq!(after, before);
    assert_eq!(
        (after.mtime_secs, after.usn),
        (before.mtime_secs, before.usn)
    );
}

#[test]
fn tags_only_change() {
    let (dir, ids) = setup(&[["a", "b"]]);
    let before = read(dir.path(), &ids).remove(0);
    let actor = CollectionActor::open(dir.path()).unwrap();
    let mut tags = change(&before, vec![]);
    tags.tags = Some(vec!["geo::europe".into(), "new".into()]);
    let report = update(&actor, vec![tags]);
    assert_eq!(report.updated, ids);
    assert!(report.changes.tag || report.changes.note);
    actor.close().unwrap();
    let after = read(dir.path(), &ids).remove(0);
    assert_eq!(after.fields, before.fields);
    assert_eq!(after.tags, ["geo::europe", "new"]);
}

#[test]
fn a_multi_note_update_is_one_undo_step() {
    let (dir, ids) = setup(&[["a", "b"], ["c", "d"], ["e", "f"]]);
    let before = read(dir.path(), &ids);
    let actor = CollectionActor::open(dir.path()).unwrap();
    let updates = before
        .iter()
        .map(|n| change(n, vec![(1, "edited")]))
        .collect();
    assert_eq!(update(&actor, updates).updated, ids);
    match actor.execute(Command::Undo) {
        Ok(Response::Undone { label, .. }) => assert_eq!(label, "Update Note"),
        other => panic!("undo: {other:?}"),
    }
    assert_eq!(undo_label(&actor), None, "no undo step");
    actor.close().unwrap();
    let after = read(dir.path(), &ids);
    for (a, b) in after.iter().zip(&before) {
        assert_eq!(a.fields, b.fields);
    }
}

#[test]
fn invalid_ordinals_are_errors() {
    let (dir, ids) = setup(&[["a", "b"]]);
    let before = read(dir.path(), &ids).remove(0);
    let actor = CollectionActor::open(dir.path()).unwrap();
    let result = actor.execute(Command::Notes(NoteCommand::UpdateNotes {
        updates: vec![change(&before, vec![(2, "x")])],
    }));
    assert!(matches!(result, Err(CoreError::Invalid(_))), "{result:?}");
    actor.close().unwrap();
}

#[derive(serde::Deserialize)]
struct Case {
    name: String,
    html: String,
}

/// What Anki stores for a value: `normalize_field` removes ASCII control
/// characters other than `\n` and `\t` and (with the default
/// `normalize_note_text`) converts to NFC.
fn anki_stores(case: &Case) -> String {
    let stripped: String = case
        .html
        .chars()
        .filter(|c| !c.is_ascii_control() || *c == '\n' || *c == '\t')
        .collect();
    match case.name.as_str() {
        "combining-decomposed" => "école café".into(),
        "hangul-jamo" => "\u{ac00}".into(),
        _ => stripped,
    }
}

#[test]
fn corpus_values_are_stored_as_sent_except_for_anki_normalisation() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/field-corpus/corpus.json");
    let cases: Vec<Case> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let placeholders: Vec<[&str; 2]> = cases.iter().map(|_| ["placeholder", "back"]).collect();
    let (dir, ids) = setup(&placeholders);
    let before = read(dir.path(), &ids);
    let actor = CollectionActor::open(dir.path()).unwrap();
    let updates = before
        .iter()
        .zip(&cases)
        .map(|(note, case)| change(note, vec![(0, &case.html)]))
        .collect();
    let report = update(&actor, updates);
    assert_eq!(report.updated.len(), cases.len());
    actor.close().unwrap();
    let after = read(dir.path(), &ids);
    let mut normalised = Vec::new();
    for (note, case) in after.iter().zip(&cases) {
        assert_eq!(note.fields[0], anki_stores(case), "{}", case.name);
        assert_eq!(note.fields[1], "back");
        if note.fields[0] != case.html {
            normalised.push(case.name.as_str());
        }
    }
    // The findings: only these corpus values are altered by Anki.
    assert_eq!(
        normalised,
        [
            "crlf",
            "lone-cr",
            "cr-at-end",
            "control-chars",
            "combining-decomposed",
            "hangul-jamo",
            "every-syntax",
        ]
    );
}
