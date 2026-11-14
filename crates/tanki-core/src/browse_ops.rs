//! Browser stream (Phase 3): search, browser table rows, settings under
//! desktop's config keys, sidebar data, bulk card/note actions and find and
//! replace. Never uses Anki's custom (raw SQL) sort and never passes an empty
//! id list to find and replace (PLAN 14 rule 11).
use crate::*;
use anki::collection::Collection;

pub(crate) fn execute(_col: &mut Collection, command: BrowseCommand) -> Result<EditResponse> {
    Err(CoreError::Invalid(format!(
        "Not implemented yet: {command:?}"
    )))
}
