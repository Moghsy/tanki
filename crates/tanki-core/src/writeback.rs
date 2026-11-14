//! Editors stream (Phase 3): changed-fields-only, all-or-nothing note
//! updates with conflict detection (PLAN 8.5; `NoteUpdate`).
use crate::*;
use anki::collection::Collection;

pub(crate) fn update_notes(
    _col: &mut Collection,
    _updates: Vec<NoteUpdate>,
) -> Result<UpdateReport> {
    Err(CoreError::Invalid(
        "Not implemented yet: UpdateNotes".into(),
    ))
}
