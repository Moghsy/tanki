//! Find duplicates: a rewrite of pylib's `find_dupes` (`collection.py`) and
//! the browser's "Tag Duplicates" (R6). Anki has no such call; desktop does
//! it in Python with raw SQL, so this searches for notes that have the field
//! and reads each with `GetNote`.
use super::{anki_error, changes, TransferResponse};
use crate::{CoreError, Result};
use anki::{
    collection::Collection,
    services::{NotesService, NotetypesService, SearchService, TagsService},
    text::strip_html_preserving_media_filenames,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

/// Notes whose chosen field holds the same text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicateGroup {
    /// The shared text with HTML stripped (media file names kept), as
    /// desktop compares it.
    pub value: String,
    /// At least two note ids, in search order.
    pub note_ids: Vec<i64>,
}

/// Field names of every note type, deduplicated and sorted.
pub(super) fn field_names(col: &mut Collection) -> Result<Vec<String>> {
    let names = NotetypesService::get_notetype_names(col).map_err(|e| anki_error(e, col.tr()))?;
    let mut fields = BTreeSet::new();
    for entry in names.entries {
        let list = NotetypesService::get_field_names(
            col,
            anki_proto::notetypes::NotetypeId { ntid: entry.id },
        )
        .map_err(|e| anki_error(e, col.tr()))?;
        fields.extend(list.vals);
    }
    Ok(fields.into_iter().collect())
}

/// Escapes a field name for a quoted `field:text` search term.
fn escape_field(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if matches!(c, '\\' | '"' | '*' | '_' | ':' | '(' | ')' | '-') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub(super) fn find(
    col: &mut Collection,
    field_name: &str,
    filter: &str,
) -> Result<Vec<DuplicateGroup>> {
    let field_name = field_name.trim();
    if field_name.is_empty() {
        return Err(CoreError::Invalid("Choose a field".into()));
    }
    // Desktop: `build_search_string(search, SearchNode(field_name=...))`,
    // i.e. the filter AND "the field is not empty".
    let term = format!("\"{}:_*\"", escape_field(field_name));
    let search = if filter.trim().is_empty() {
        term
    } else {
        format!("({}) {term}", filter.trim())
    };
    let ids = SearchService::search_notes(
        col,
        anki_proto::search::SearchRequest {
            search,
            order: None,
        },
    )
    .map_err(|e| anki_error(e, col.tr()))?
    .ids;
    let mut ordinals: HashMap<i64, Option<usize>> = HashMap::new();
    let mut groups: Vec<DuplicateGroup> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for id in ids {
        let note = NotesService::get_note(col, anki_proto::notes::NoteId { nid: id })
            .map_err(|e| anki_error(e, col.tr()))?;
        let ordinal = match ordinals.get(&note.notetype_id) {
            Some(found) => *found,
            None => {
                let names = NotetypesService::get_field_names(
                    col,
                    anki_proto::notetypes::NotetypeId {
                        ntid: note.notetype_id,
                    },
                )
                .map_err(|e| anki_error(e, col.tr()))?
                .vals;
                let found = names
                    .iter()
                    .position(|n| n.eq_ignore_ascii_case(field_name));
                ordinals.insert(note.notetype_id, found);
                found
            }
        };
        let Some(text) = ordinal.and_then(|o| note.fields.get(o)) else {
            continue;
        };
        let value = strip_html_preserving_media_filenames(text)
            .trim()
            .to_owned();
        if value.is_empty() {
            continue;
        }
        match index.get(&value) {
            Some(&at) => groups[at].note_ids.push(id),
            None => {
                index.insert(value.clone(), groups.len());
                groups.push(DuplicateGroup {
                    value,
                    note_ids: vec![id],
                });
            }
        }
    }
    groups.retain(|g| g.note_ids.len() > 1);
    Ok(groups)
}

/// Desktop's "Tag Duplicates": the localised word "duplicate".
pub(super) fn tag(col: &mut Collection, note_ids: Vec<i64>) -> Result<TransferResponse> {
    if note_ids.is_empty() {
        return Err(CoreError::Invalid("There are no duplicates to tag".into()));
    }
    let word = anki_i18n::without_unicode_isolation(&col.tr().browsing_duplicate()).to_string();
    let result = TagsService::add_note_tags(
        col,
        anki_proto::tags::NoteIdsAndTagsRequest {
            note_ids,
            tags: word,
        },
    )
    .map_err(|e| anki_error(e, col.tr()))?;
    Ok(TransferResponse::Tagged {
        count: result.count as usize,
        changes: changes(result.changes.unwrap_or_default()),
    })
}
