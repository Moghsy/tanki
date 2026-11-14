//! Notes stream (Phase 3): note types, adding, fields check, cloze numbers,
//! preview, sticky fields, tags and media. Every Anki call is a fully
//! qualified service-trait call (PLAN 4.3).
use crate::*;
use anki::collection::Collection;

/// Runs a `NoteCommand`. `NoteCommand::UpdateNotes` never reaches here: the
/// actor sends it to `writeback::update_notes`.
pub(crate) fn execute(_col: &mut Collection, command: NoteCommand) -> Result<EditResponse> {
    Err(CoreError::Invalid(format!(
        "Not implemented yet: {command:?}"
    )))
}
