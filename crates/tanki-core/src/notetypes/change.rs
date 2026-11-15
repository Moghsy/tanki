//! Change note type and restore to stock (PLAN 8.6).
use super::{
    anki_error, changes, invalid, is_cloze, load, stock_json, NotetypeResponse, StockKind,
};
use crate::actor::confirm_schema_change;
use crate::{CoreError, Result};
use anki::{
    collection::Collection,
    services::{NotesService, NotetypesService},
};
use anki_proto as pb;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// What the mapping form starts from: Anki's default maps (fields matched by
/// name, leftovers in order; templates likewise, none for cloze types).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeNotetypeInfo {
    pub old_notetype_id: i64,
    pub old_notetype_name: String,
    pub new_notetype_id: i64,
    pub new_notetype_name: String,
    pub old_field_names: Vec<String>,
    pub old_template_names: Vec<String>,
    pub new_field_names: Vec<String>,
    pub new_template_names: Vec<String>,
    /// Per new field, the old field index feeding it.
    pub new_fields: Vec<Option<u32>>,
    /// Per new template, the old template index; `None` when either side is
    /// a cloze type ("clozes can't be remapped").
    pub new_templates: Option<Vec<Option<u32>>>,
    pub note_count: usize,
}

/// What `Restore` would replace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestorePlan {
    pub notetype_name: String,
    /// The stock kind that will be used; `None` while the note type has no
    /// recorded origin and none was forced (`needs_kind`).
    pub kind: Option<StockKind>,
    pub needs_kind: bool,
    pub note_count: u32,
    pub old_field_names: Vec<String>,
    pub stock_field_names: Vec<String>,
    /// Fields whose position is beyond the stock count: their content is
    /// dropped from every note.
    pub dropped_fields: Vec<String>,
    pub old_template_names: Vec<String>,
    pub stock_template_names: Vec<String>,
    /// Whether the field or template count differs (a likely schema change).
    pub structure_changes: bool,
}

/// The single note type of the notes, as desktop requires.
fn single_notetype(col: &mut Collection, note_ids: &[i64]) -> Result<i64> {
    if note_ids.is_empty() {
        return invalid("No notes selected");
    }
    let mut types = BTreeSet::new();
    for &nid in note_ids {
        let note = NotesService::get_note(col, pb::notes::NoteId { nid })
            .map_err(|e| anki_error(col, e))?;
        types.insert(note.notetype_id);
        if types.len() > 1 {
            return invalid("Please select notes from only one note type");
        }
    }
    Ok(types.into_iter().next().expect("one note type"))
}

fn to_option(values: &[i32]) -> Vec<Option<u32>> {
    values
        .iter()
        .map(|&v| (v >= 0).then_some(v as u32))
        .collect()
}

pub(super) fn info(
    col: &mut Collection,
    note_ids: Vec<i64>,
    new_notetype_id: i64,
) -> Result<ChangeNotetypeInfo> {
    let old_id = single_notetype(col, &note_ids)?;
    let new = load(col, new_notetype_id)?;
    let i = NotetypesService::get_change_notetype_info(
        col,
        pb::notetypes::GetChangeNotetypeInfoRequest {
            old_notetype_id: old_id,
            new_notetype_id,
        },
    )
    .map_err(|e| anki_error(col, e))?;
    let input = i.input.unwrap_or_default();
    Ok(ChangeNotetypeInfo {
        old_notetype_id: old_id,
        old_notetype_name: i.old_notetype_name,
        new_notetype_id,
        new_notetype_name: new.name,
        old_field_names: i.old_field_names,
        old_template_names: i.old_template_names,
        new_field_names: i.new_field_names,
        new_template_names: i.new_template_names,
        new_fields: to_option(&input.new_fields),
        new_templates: (!input.new_templates.is_empty()).then(|| to_option(&input.new_templates)),
        note_count: note_ids.len(),
    })
}

pub(super) fn change(
    col: &mut Collection,
    note_ids: Vec<i64>,
    new_notetype_id: i64,
    new_fields: Vec<Option<u32>>,
    new_templates: Option<Vec<Option<u32>>>,
    confirmed: bool,
) -> Result<NotetypeResponse> {
    let old_id = single_notetype(col, &note_ids)?;
    let old = load(col, old_id)?;
    let new = load(col, new_notetype_id)?;
    if new_fields.len() != new.fields.len() {
        return invalid("The field map needs one entry per field of the new note type");
    }
    if new_fields
        .iter()
        .flatten()
        .any(|&i| i as usize >= old.fields.len())
    {
        return invalid("The field map refers to a field the old note type does not have");
    }
    match (&new_templates, is_cloze(&old) || is_cloze(&new)) {
        (Some(_), true) => return invalid("Card types cannot be mapped to or from a cloze type"),
        (None, false) => return invalid("A card type map is required"),
        (Some(map), false) => {
            if map.len() != new.templates.len() {
                return invalid(
                    "The card type map needs one entry per card type of the new note type",
                );
            }
            if map
                .iter()
                .flatten()
                .any(|&i| i as usize >= old.templates.len())
            {
                return invalid(
                    "The card type map refers to a card type the old note type does not have",
                );
            }
        }
        (None, true) => {}
    }
    confirm_schema_change(col, confirmed)?;
    // Anki's optimistic lock on the schema stamp: take the current one.
    let current = NotetypesService::get_change_notetype_info(
        col,
        pb::notetypes::GetChangeNotetypeInfoRequest {
            old_notetype_id: old_id,
            new_notetype_id,
        },
    )?
    .input
    .unwrap_or_default()
    .current_schema;
    let count = note_ids.len();
    let r = NotetypesService::change_notetype(
        col,
        pb::notetypes::ChangeNotetypeRequest {
            note_ids,
            new_fields: new_fields
                .iter()
                .map(|f| f.map_or(-1, |i| i as i32))
                .collect(),
            new_templates: new_templates
                .iter()
                .flatten()
                .map(|t| t.map_or(-1, |i| i as i32))
                .collect(),
            old_notetype_id: old_id,
            new_notetype_id,
            current_schema: current,
            old_notetype_name: old.name,
            is_cloze: is_cloze(&new),
        },
    )
    .map_err(|e| anki_error(col, e))?;
    Ok(NotetypeResponse::NotesChanged {
        count,
        changes: changes(r),
    })
}

fn names(json: &serde_json::Value, key: &str) -> Vec<String> {
    json[key]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|i| i["name"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn restore_plan(
    col: &mut Collection,
    id: i64,
    force_kind: Option<StockKind>,
) -> Result<RestorePlan> {
    let nt = load(col, id)?;
    let recorded =
        StockKind::from_original(nt.config.clone().unwrap_or_default().original_stock_kind);
    let kind = force_kind.or(recorded);
    let old_field_names: Vec<String> = nt.fields.iter().map(|f| f.name.clone()).collect();
    let old_template_names: Vec<String> = nt.templates.iter().map(|t| t.name.clone()).collect();
    let note_count = super::use_counts(col)?.get(&id).copied().unwrap_or(0);
    let (stock_field_names, stock_template_names) = match kind {
        Some(kind) => {
            let json = stock_json(col, kind)?;
            (names(&json, "flds"), names(&json, "tmpls"))
        }
        None => (Vec::new(), Vec::new()),
    };
    let dropped_fields = match kind {
        Some(_) => old_field_names
            .iter()
            .skip(stock_field_names.len())
            .cloned()
            .collect(),
        None => Vec::new(),
    };
    Ok(RestorePlan {
        notetype_name: nt.name,
        kind,
        needs_kind: kind.is_none(),
        note_count,
        structure_changes: kind.is_some()
            && (stock_field_names.len() != old_field_names.len()
                || stock_template_names.len() != old_template_names.len()),
        old_field_names,
        stock_field_names,
        dropped_fields,
        old_template_names,
        stock_template_names,
    })
}

pub(super) fn restore(
    col: &mut Collection,
    id: i64,
    force_kind: Option<StockKind>,
    typed_confirmation: bool,
    confirmed: bool,
) -> Result<NotetypeResponse> {
    let plan = restore_plan(col, id, force_kind)?;
    if plan.needs_kind {
        return Err(CoreError::Invalid(
            "This note type has no recorded original kind; choose which stock note type to restore"
                .into(),
        ));
    }
    if !typed_confirmation {
        return invalid(
            "Restoring replaces the fields, card types and styling with the stock ones; confirm by typing the note type name",
        );
    }
    confirm_schema_change(col, confirmed)?;
    let r = NotetypesService::restore_notetype_to_stock(
        col,
        pb::notetypes::RestoreNotetypeToStockRequest {
            notetype_id: Some(pb::notetypes::NotetypeId { ntid: id }),
            force_kind: force_kind.map(|k| k.to_proto() as i32),
        },
    )
    .map_err(|e| anki_error(col, e))?;
    Ok(NotetypeResponse::Saved {
        notetype: super::get(col, id)?,
        changes: changes(r),
    })
}
