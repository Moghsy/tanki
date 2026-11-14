//! Editors stream (Phase 3): changed-fields-only, all-or-nothing note
//! updates with conflict detection (PLAN 8.5; `NoteUpdate`).
//!
//! Every note is read first (`NotesService::get_note`). A deleted note is
//! `missing`; a note whose mtime differs from `expected_mtime`, or whose
//! stored fields differ from `expected_fields`, is a conflict. With any of
//! either nothing is written. Notes whose listed fields and tags already
//! match are `unchanged` and are not passed to Anki (no mtime/usn bump). All
//! other notes go to Anki in one `NotesService::update_notes` call: one undo
//! step. Fields not listed are passed back exactly as stored.
use crate::*;
use anki::{collection::Collection, error::AnkiError, services::NotesService};
use anki_proto as pb;
use std::collections::HashSet;

pub(crate) fn update_notes(col: &mut Collection, updates: Vec<NoteUpdate>) -> Result<UpdateReport> {
    let mut report = UpdateReport::default();
    let mut ids = HashSet::new();
    let mut to_write = Vec::new();
    for update in updates {
        if !ids.insert(update.id) {
            return Err(CoreError::Invalid(format!(
                "Note {} is listed twice in one update",
                update.id
            )));
        }
        let stored = match NotesService::get_note(col, pb::notes::NoteId { nid: update.id }) {
            Ok(note) => note,
            Err(AnkiError::NotFound { .. }) => {
                report.missing.push(update.id);
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        let mut ords = HashSet::new();
        for (ord, _) in &update.fields {
            if *ord as usize >= stored.fields.len() {
                return Err(CoreError::Invalid(format!(
                    "Note {} has {} fields; there is no field {ord}",
                    update.id,
                    stored.fields.len()
                )));
            }
            if !ords.insert(*ord) {
                return Err(CoreError::Invalid(format!(
                    "Field {ord} of note {} is listed twice",
                    update.id
                )));
            }
        }
        let conflict = stored.mtime_secs != update.expected_mtime
            || update
                .expected_fields
                .as_ref()
                .is_some_and(|expected| *expected != stored.fields);
        if conflict {
            report.conflicts.push(update.id);
            continue;
        }
        let mut note = stored.clone();
        for (ord, value) in update.fields {
            note.fields[ord as usize] = value;
        }
        if let Some(tags) = update.tags {
            note.tags = tags;
        }
        if note.fields == stored.fields && note.tags == stored.tags {
            report.unchanged.push(update.id);
        } else {
            report.updated.push(update.id);
            to_write.push(note);
        }
    }
    if !report.conflicts.is_empty() || !report.missing.is_empty() {
        report.updated.clear();
        return Ok(report);
    }
    if !to_write.is_empty() {
        let changes = NotesService::update_notes(
            col,
            pb::notes::UpdateNotesRequest {
                notes: to_write,
                skip_undo_entry: false,
            },
        )?;
        report.changes = crate::actor::changes(changes);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anki::collection::CollectionBuilder;

    fn note(col: &mut Collection, fields: [&str; 2]) -> pb::notes::Note {
        let nt = col.get_notetype_by_name("Basic").unwrap().unwrap();
        let mut note =
            NotesService::new_note(col, pb::notetypes::NotetypeId { ntid: nt.id.0 }).unwrap();
        note.fields = fields.map(String::from).to_vec();
        let id = NotesService::add_note(
            col,
            pb::notes::AddNoteRequest {
                note: Some(note),
                deck_id: 1,
            },
        )
        .unwrap()
        .note_id;
        NotesService::get_note(col, pb::notes::NoteId { nid: id }).unwrap()
    }

    #[test]
    fn invalid_ordinals_and_repeats_are_refused_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let mut col = CollectionBuilder::new(dir.path().join("c.anki2"))
            .build()
            .unwrap();
        let n = note(&mut col, ["a", "b"]);
        let update = NoteUpdate {
            id: n.id,
            expected_mtime: n.mtime_secs,
            expected_fields: None,
            fields: vec![(2, "x".into())],
            tags: None,
        };
        assert!(matches!(
            update_notes(&mut col, vec![update.clone()]),
            Err(CoreError::Invalid(_))
        ));
        let repeated = NoteUpdate {
            fields: vec![(0, "x".into()), (0, "y".into())],
            ..update.clone()
        };
        assert!(matches!(
            update_notes(&mut col, vec![repeated]),
            Err(CoreError::Invalid(_))
        ));
        let ok = NoteUpdate {
            fields: vec![(0, "x".into())],
            ..update
        };
        assert!(matches!(
            update_notes(&mut col, vec![ok.clone(), ok]),
            Err(CoreError::Invalid(_))
        ));
        let stored = NotesService::get_note(&mut col, pb::notes::NoteId { nid: n.id }).unwrap();
        assert_eq!(stored, n);
        col.close(None).unwrap();
    }
}
