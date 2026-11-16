//! Note types, fields, templates and change note type (PLAN 8.6).
//!
//! Owned by the Note types stream (docs/implementation/phase-5-interfaces.md).
//! Reached through `Command::Manage(ManageCommand::Notetypes(…))`.
//!
//! Editing works on a draft (`NotetypeDetail`, see `draft`): `Get` loads one,
//! the pure `NotetypeDetail::apply` edits it keeping every surviving field's
//! and template's original ordinal, and `Save` (or `Edit`, which does both)
//! writes it back through `update_notetype`, which diffs the ordinals and
//! rewrites notes and cards. Two guards that exist only in desktop's UI are
//! added here (PLAN 8.6): a schema-changing save needs `confirmed`
//! (`crate::actor::confirm_schema_change`), and required fields (cloze text,
//! image occlusion, `prevent_deletion`) cannot be deleted or renamed.
use crate::actor::{changes, confirm_schema_change, finish_render};
use crate::{CoreError, NoteData, NotetypeKind, OpChanges, RenderedCard, Result};
use anki::{
    collection::Collection,
    error::{AnkiError, CardTypeErrorDetails},
    services::{CardRenderingService, DecksService, NotesService, NotetypesService},
};
use anki_proto as pb;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

mod change;
mod draft;
pub use change::{ChangeNotetypeInfo, RestorePlan};
pub use draft::{
    check_field_name, FieldDetail, NotetypeDetail, NotetypeEdit, NotetypeSummary, StockKind,
    StockKindInfo, TemplateDetail,
};

/// Each variant's doc comment names its `NotetypeResponse` variant. Commands
/// that can force a one-way full sync take `confirmed`: false answers
/// `CoreError::SchemaChangeRequired` before Anki is called (unless a full
/// sync is already pending), so the UI can ask and repeat with true.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NotetypeCommand {
    /// All note types sorted by name, with use counts -> `List`.
    List,
    /// -> `StockKinds`.
    StockKinds,
    /// One note type as an editable draft -> `Detail`.
    Get { id: i64 },
    /// Adds a stock note type; `name` overrides the stock name
    /// (a clash gets `+` appended by Anki) -> `Added`.
    AddStock {
        kind: StockKind,
        name: Option<String>,
    },
    /// Deep copy with new ids, named "<name> copy" unless `name` is given
    /// (desktop's `models.copy`) -> `Added`.
    Clone { id: i64, name: Option<String> },
    /// -> `Saved`.
    Rename { id: i64, name: String },
    /// Deletes the note type and all its notes. Refuses the last note type
    /// -> `Changed`. Schema change: needs `confirmed`.
    Delete { id: i64, confirmed: bool },
    /// Loads the note type, applies the edits in order, saves ->
    /// `Saved` or `Rejected`. Schema change when they add, delete or reorder
    /// fields or templates or change the sort field.
    Edit {
        id: i64,
        edits: Vec<NotetypeEdit>,
        confirmed: bool,
    },
    /// Saves a whole draft (the templates editor's "Save") -> `Saved` or
    /// `Rejected`. The schema-change test compares it with the stored note
    /// type, so a draft that only edits text never needs `confirmed`.
    Save {
        notetype: NotetypeDetail,
        confirmed: bool,
    },
    /// Renders an uncommitted template against an existing note of the note
    /// type, or an empty one -> `Preview`.
    Preview {
        notetype_id: i64,
        /// The note to render; `None` for an empty note (with `fill_empty`,
        /// field names show instead of nothing).
        note_id: Option<i64>,
        template: TemplateDetail,
        /// The draft's CSS; the stored one when `None`.
        css: Option<String>,
        /// 0-based: the template index, or the cloze number - 1.
        card_ord: u32,
        fill_empty: bool,
    },
    /// Default field/template mapping for changing these notes to the note
    /// type -> `ChangeInfo`.
    ChangeInfo {
        note_ids: Vec<i64>,
        new_notetype_id: i64,
    },
    /// Changes the note type of the notes. `new_fields[i]` is the old field
    /// index feeding new field `i`; `new_templates` likewise for card types
    /// (`None` when either side is a cloze type, `Some` otherwise). Cards of
    /// unmapped templates are deleted -> `NotesChanged`. Always a schema
    /// change: needs `confirmed`.
    Change {
        note_ids: Vec<i64>,
        new_notetype_id: i64,
        new_fields: Vec<Option<u32>>,
        new_templates: Option<Vec<Option<u32>>>,
        confirmed: bool,
    },
    /// What restoring to stock would replace -> `RestorePlan`.
    RestorePlan {
        id: i64,
        force_kind: Option<StockKind>,
    },
    /// Replaces fields, templates and CSS with the stock ones; data in
    /// fields beyond the stock count is lost. Needs both
    /// `typed_confirmation` (the user typed the note type's name; the UI
    /// sets it) and `confirmed` (schema) -> `Saved`. `force_kind` is
    /// required when the note type has no recorded stock origin.
    Restore {
        id: i64,
        force_kind: Option<StockKind>,
        typed_confirmation: bool,
        confirmed: bool,
    },
}

/// Why Anki refused a template (`CardTypeError`), typed for inline display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TemplateProblemKind {
    /// "Unable to parse the template" (Anki says: see the preview).
    ParseError,
    /// Another card type has the same front (index of the other one).
    DuplicateFront {
        other: u32,
    },
    /// The front has no field reference.
    NoFrontField,
    UnknownField(String),
    /// A cloze note type needs `{{cloze:…}}` on both sides.
    MissingCloze,
}

/// A rejected save: nothing was written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateProblem {
    pub kind: TemplateProblemKind,
    /// 0-based index of the offending card type (as in the saved draft).
    pub template: u32,
    /// Anki's localised message, plain text.
    pub message: String,
}

/// A live preview. `card.question`/`answer` hold the error text when the
/// template does not render; `error` repeats it separately.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplatePreview {
    pub card: RenderedCard,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NotetypeResponse {
    List(Vec<NotetypeSummary>),
    StockKinds(Vec<StockKindInfo>),
    Detail(NotetypeDetail),
    Added {
        id: i64,
        changes: OpChanges,
    },
    Changed(OpChanges),
    /// The note type as stored after the save (names may differ: Anki strips
    /// quotes and de-duplicates).
    Saved {
        notetype: NotetypeDetail,
        changes: OpChanges,
    },
    /// Anki refused a template; nothing was saved.
    Rejected(TemplateProblem),
    Preview(TemplatePreview),
    ChangeInfo(ChangeNotetypeInfo),
    NotesChanged {
        count: usize,
        changes: OpChanges,
    },
    RestorePlan(RestorePlan),
}

impl NotetypeResponse {
    /// The changes a mutating command made; the actor broadcasts these.
    pub fn changes(&self) -> Option<&OpChanges> {
        match self {
            Self::Added { changes, .. }
            | Self::Saved { changes, .. }
            | Self::NotesChanged { changes, .. }
            | Self::Changed(changes) => Some(changes),
            _ => None,
        }
    }
}

pub(crate) fn execute(col: &mut Collection, command: NotetypeCommand) -> Result<NotetypeResponse> {
    Ok(match command {
        NotetypeCommand::List => NotetypeResponse::List(list(col)?),
        NotetypeCommand::StockKinds => NotetypeResponse::StockKinds(stock_kinds(col)?),
        NotetypeCommand::Get { id } => NotetypeResponse::Detail(get(col, id)?),
        NotetypeCommand::AddStock { kind, name } => add_stock(col, kind, name)?,
        NotetypeCommand::Clone { id, name } => clone(col, id, name)?,
        NotetypeCommand::Rename { id, name } => {
            let mut draft = get(col, id)?;
            draft.apply(NotetypeEdit::Rename(name))?;
            save(col, draft, false)?
        }
        NotetypeCommand::Delete { id, confirmed } => delete(col, id, confirmed)?,
        NotetypeCommand::Edit {
            id,
            edits,
            confirmed,
        } => {
            let mut draft = get(col, id)?;
            for edit in edits {
                draft.apply(edit)?;
            }
            save(col, draft, confirmed)?
        }
        NotetypeCommand::Save {
            notetype,
            confirmed,
        } => save(col, notetype, confirmed)?,
        NotetypeCommand::Preview {
            notetype_id,
            note_id,
            template,
            css,
            card_ord,
            fill_empty,
        } => NotetypeResponse::Preview(preview(
            col,
            notetype_id,
            note_id,
            template,
            css,
            card_ord,
            fill_empty,
        )?),
        NotetypeCommand::ChangeInfo {
            note_ids,
            new_notetype_id,
        } => NotetypeResponse::ChangeInfo(change::info(col, note_ids, new_notetype_id)?),
        NotetypeCommand::Change {
            note_ids,
            new_notetype_id,
            new_fields,
            new_templates,
            confirmed,
        } => change::change(
            col,
            note_ids,
            new_notetype_id,
            new_fields,
            new_templates,
            confirmed,
        )?,
        NotetypeCommand::RestorePlan { id, force_kind } => {
            NotetypeResponse::RestorePlan(change::restore_plan(col, id, force_kind)?)
        }
        NotetypeCommand::Restore {
            id,
            force_kind,
            typed_confirmation,
            confirmed,
        } => change::restore(col, id, force_kind, typed_confirmation, confirmed)?,
    })
}

// ---- helpers ----------------------------------------------------------------

fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(CoreError::Invalid(message.into()))
}

/// Anki's message for an error, localised and without isolation marks; user
/// input problems become `CoreError::Invalid`.
pub(crate) fn anki_error(col: &Collection, error: AnkiError) -> CoreError {
    match error {
        AnkiError::InvalidInput { .. } | AnkiError::NotFound { .. } => {
            CoreError::Invalid(plain(&error.message(col.tr())))
        }
        other => other.into(),
    }
}

/// Anki's messages are HTML-ish: `<br>` separates lines.
fn plain(message: &str) -> String {
    anki_i18n::without_unicode_isolation(message)
        .replace("<br>", ": ")
        .replace("<br/>", ": ")
}

pub(crate) fn load(col: &mut Collection, id: i64) -> Result<pb::notetypes::Notetype> {
    NotetypesService::get_notetype(col, pb::notetypes::NotetypeId { ntid: id })
        .map_err(|e| anki_error(col, e))
}

fn use_counts(col: &mut Collection) -> Result<HashMap<i64, u32>> {
    Ok(NotetypesService::get_notetype_names_and_counts(col)?
        .entries
        .into_iter()
        .map(|e| (e.id, e.use_count))
        .collect())
}

fn is_cloze(nt: &pb::notetypes::Notetype) -> bool {
    use pb::notetypes::notetype::config::Kind;
    nt.config.as_ref().map(|c| c.kind) == Some(Kind::Cloze as i32)
}

fn random_id() -> i64 {
    use std::hash::{BuildHasher, Hasher};
    (std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
        >> 1) as i64
}

fn list(col: &mut Collection) -> Result<Vec<NotetypeSummary>> {
    let counts = NotetypesService::get_notetype_names_and_counts(col)?.entries;
    let mut out = Vec::with_capacity(counts.len());
    for entry in counts {
        let nt = load(col, entry.id)?;
        let config = nt.config.clone().unwrap_or_default();
        out.push(NotetypeSummary {
            id: entry.id,
            name: entry.name,
            kind: if is_cloze(&nt) {
                NotetypeKind::Cloze
            } else {
                NotetypeKind::Normal
            },
            use_count: entry.use_count,
            field_count: nt.fields.len(),
            template_count: nt.templates.len(),
            stock_kind: StockKind::from_original(config.original_stock_kind),
        });
    }
    out.sort_by_key(|s| s.name.to_lowercase());
    Ok(out)
}

fn stock_json(col: &mut Collection, kind: StockKind) -> Result<serde_json::Value> {
    let json = NotetypesService::get_stock_notetype_legacy(
        col,
        pb::notetypes::StockNotetype {
            kind: kind.to_proto() as i32,
        },
    )?
    .json;
    serde_json::from_slice(&json).map_err(|e| CoreError::Other(e.to_string()))
}

fn stock_kinds(col: &mut Collection) -> Result<Vec<StockKindInfo>> {
    StockKind::ALL
        .into_iter()
        .map(|kind| {
            let json = stock_json(col, kind)?;
            Ok(StockKindInfo {
                kind,
                name: json["name"].as_str().unwrap_or_default().to_string(),
            })
        })
        .collect()
}

/// The note type as a draft. `required` marks the fields that cannot be
/// deleted or renamed.
pub(crate) fn get(col: &mut Collection, id: i64) -> Result<NotetypeDetail> {
    let nt = load(col, id)?;
    detail(col, nt)
}

fn detail(col: &mut Collection, nt: pb::notetypes::Notetype) -> Result<NotetypeDetail> {
    let cloze = is_cloze(&nt);
    let cloze_fields = if cloze {
        NotetypesService::get_cloze_field_ords(col, pb::notetypes::NotetypeId { ntid: nt.id })?.ords
    } else {
        Vec::new()
    };
    let use_count = use_counts(col)?.get(&nt.id).copied().unwrap_or(0);
    let config = nt.config.unwrap_or_default();
    let fields = nt
        .fields
        .into_iter()
        .enumerate()
        .map(|(index, f)| {
            let c = f.config.unwrap_or_default();
            let ord = f.ord.map_or(index as u32, |o| o.val);
            FieldDetail {
                ord: Some(ord),
                name: f.name,
                sticky: c.sticky,
                rtl: c.rtl,
                plain_text: c.plain_text,
                collapsed: c.collapsed,
                exclude_from_search: c.exclude_from_search,
                description: c.description,
                font_name: c.font_name,
                font_size: c.font_size,
                required: c.prevent_deletion || cloze_fields.contains(&(index as u32)),
            }
        })
        .collect();
    let templates = nt
        .templates
        .into_iter()
        .enumerate()
        .map(|(index, t)| {
            let c = t.config.unwrap_or_default();
            TemplateDetail {
                ord: Some(t.ord.map_or(index as u32, |o| o.val)),
                name: t.name,
                question: c.q_format,
                answer: c.a_format,
                browser_question: c.q_format_browser,
                browser_answer: c.a_format_browser,
                browser_font_name: c.browser_font_name,
                browser_font_size: c.browser_font_size,
                target_deck_id: (c.target_deck_id != 0).then_some(c.target_deck_id),
            }
        })
        .collect();
    Ok(NotetypeDetail {
        id: nt.id,
        name: nt.name,
        kind: if cloze {
            NotetypeKind::Cloze
        } else {
            NotetypeKind::Normal
        },
        stock_kind: StockKind::from_original(config.original_stock_kind),
        use_count,
        css: config.css,
        latex_pre: config.latex_pre,
        latex_post: config.latex_post,
        latex_svg: config.latex_svg,
        sort_field: config.sort_field_idx as usize,
        fields,
        templates,
    })
}

fn added(r: pb::collection::OpChangesWithId) -> NotetypeResponse {
    NotetypeResponse::Added {
        id: r.id,
        changes: changes(r.changes.unwrap_or_default()),
    }
}

fn add_stock(
    col: &mut Collection,
    kind: StockKind,
    name: Option<String>,
) -> Result<NotetypeResponse> {
    let mut json = stock_json(col, kind)?;
    if let Some(name) = name {
        let name = name.replace('"', "");
        if name.trim().is_empty() {
            return invalid("The note type name is empty");
        }
        json["name"] = serde_json::Value::String(name.trim().to_string());
    }
    let bytes = serde_json::to_vec(&json).map_err(|e| CoreError::Other(e.to_string()))?;
    let r = NotetypesService::add_notetype_legacy(col, pb::generic::Json { json: bytes })
        .map_err(|e| anki_error(col, e))?;
    Ok(added(r))
}

/// Rewrite of desktop's `models.copy` (a Python deep copy): the structure is
/// kept, the note type id is assigned on adding, field and card type ids are
/// new, the import origin is dropped and the name is "<name> copy".
fn clone(col: &mut Collection, id: i64, name: Option<String>) -> Result<NotetypeResponse> {
    let mut nt = load(col, id)?;
    nt.name = match name {
        Some(name) if !name.trim().is_empty() => name.replace('"', "").trim().to_string(),
        Some(_) => return invalid("The note type name is empty"),
        None => anki_i18n::without_unicode_isolation(&col.tr().notetypes_copy(&nt.name)),
    };
    nt.id = 0;
    nt.mtime_secs = 0;
    nt.usn = 0;
    if let Some(config) = nt.config.as_mut() {
        config.original_id = None;
    }
    for field in &mut nt.fields {
        field.ord = None;
        if let Some(config) = field.config.as_mut() {
            config.id = Some(random_id());
        }
    }
    for template in &mut nt.templates {
        template.ord = None;
        template.mtime_secs = 0;
        template.usn = 0;
        if let Some(config) = template.config.as_mut() {
            config.id = Some(random_id());
        }
    }
    let r = NotetypesService::add_notetype(col, nt).map_err(|e| anki_error(col, e))?;
    Ok(added(r))
}

fn delete(col: &mut Collection, id: i64, confirmed: bool) -> Result<NotetypeResponse> {
    let _ = load(col, id)?;
    if NotetypesService::get_notetype_names(col)?.entries.len() < 2 {
        return invalid("Please add another note type first");
    }
    confirm_schema_change(col, confirmed)?;
    let r = NotetypesService::remove_notetype(col, pb::notetypes::NotetypeId { ntid: id })
        .map_err(|e| anki_error(col, e))?;
    Ok(NotetypeResponse::Changed(changes(r)))
}

// ---- saving a draft -----------------------------------------------------------

/// Fields that the stored note type protects: the cloze fields, those flagged
/// `prevent_deletion` (image occlusion's). Desktop enforces this in the fields
/// dialog only; core does not.
fn check_required_fields(stored: &NotetypeDetail, draft: &NotetypeDetail) -> Result<()> {
    for (ord, field) in stored
        .fields
        .iter()
        .enumerate()
        .filter(|(_, field)| field.required)
    {
        match draft.fields.iter().find(|f| f.ord == Some(ord as u32)) {
            None => {
                return invalid(format!(
                    "The field {} is required by this note type and cannot be deleted",
                    field.name
                ))
            }
            Some(f) if f.name.trim() != field.name => {
                return invalid(format!(
                    "The field {} is required by this note type and cannot be renamed",
                    field.name
                ))
            }
            Some(_) => {}
        }
    }
    Ok(())
}

fn distinct_ords(ords: impl Iterator<Item = Option<u32>>, len: usize, what: &str) -> Result<()> {
    let mut seen = vec![false; len];
    for ord in ords.flatten() {
        match seen.get_mut(ord as usize) {
            Some(slot) if !*slot => *slot = true,
            _ => {
                return invalid(format!(
                    "The draft's {what} do not match the stored note type"
                ))
            }
        }
    }
    Ok(())
}

fn ords_changed(ords: impl ExactSizeIterator<Item = Option<u32>>, stored_len: usize) -> bool {
    ords.len() != stored_len
        || ords
            .enumerate()
            .any(|(index, ord)| ord != Some(index as u32))
}

fn save(col: &mut Collection, draft: NotetypeDetail, confirmed: bool) -> Result<NotetypeResponse> {
    let stored_proto = load(col, draft.id)?;
    let stored = detail(col, stored_proto.clone())?;
    if draft.fields.is_empty() || draft.templates.is_empty() {
        return invalid("A note type needs at least one field and one card type");
    }
    if draft.kind != stored.kind {
        return invalid("The kind of a note type cannot change");
    }
    distinct_ords(
        draft.fields.iter().map(|f| f.ord),
        stored.fields.len(),
        "fields",
    )?;
    distinct_ords(
        draft.templates.iter().map(|t| t.ord),
        stored.templates.len(),
        "card types",
    )?;
    check_required_fields(&stored, &draft)?;
    let mut folded = std::collections::HashSet::new();
    for field in &draft.fields {
        let renamed = field
            .ord
            .and_then(|o| stored.fields.get(o as usize))
            .is_none_or(|old| old.name != field.name);
        if renamed {
            crate::notetypes::check_field_name(&field.name)?;
        }
        if !folded.insert(field.name.trim().to_lowercase()) {
            return invalid(format!("The field name {} is used twice", field.name));
        }
    }
    let sort_ord = draft
        .fields
        .get(draft.sort_field)
        .ok_or_else(|| CoreError::Invalid("The sort field is not a field".into()))?
        .ord
        .ok_or_else(|| {
            CoreError::Invalid(
                "A new field can only become the sort field after it is saved".into(),
            )
        })?;
    for template in &draft.templates {
        if let Some(deck) = template.target_deck_id {
            let known = DecksService::get_deck_names(
                col,
                pb::decks::GetDeckNamesRequest {
                    skip_empty_default: false,
                    include_filtered: false,
                },
            )?
            .entries
            .iter()
            .any(|e| e.id == deck);
            if !known {
                return invalid(format!(
                    "The deck override of card type {} is not a normal deck",
                    template.name
                ));
            }
        }
    }

    let mut nt = stored_proto.clone();
    nt.name = draft.name.clone();
    let config = nt.config.get_or_insert_with(Default::default);
    let previous_sort = config.sort_field_idx;
    config.css = draft.css.clone();
    config.latex_pre = draft.latex_pre.clone();
    config.latex_post = draft.latex_post.clone();
    config.latex_svg = draft.latex_svg;
    // Anki reads the sort field as an *original* ordinal and repositions it.
    config.sort_field_idx = sort_ord;
    nt.fields = draft
        .fields
        .iter()
        .map(|f| {
            let mut field = f
                .ord
                .and_then(|o| stored_proto.fields.get(o as usize).cloned())
                .unwrap_or_else(|| pb::notetypes::notetype::Field {
                    config: Some(pb::notetypes::notetype::field::Config {
                        id: Some(random_id()),
                        ..Default::default()
                    }),
                    ..Default::default()
                });
            field.ord = f.ord.map(|val| pb::generic::UInt32 { val });
            field.name = f.name.clone();
            let c = field.config.get_or_insert_with(Default::default);
            c.sticky = f.sticky;
            c.rtl = f.rtl;
            c.plain_text = f.plain_text;
            c.collapsed = f.collapsed;
            c.exclude_from_search = f.exclude_from_search;
            c.description = f.description.clone();
            c.font_name = f.font_name.clone();
            c.font_size = f.font_size;
            field
        })
        .collect();
    nt.templates = draft
        .templates
        .iter()
        .map(|t| {
            let mut template = t
                .ord
                .and_then(|o| stored_proto.templates.get(o as usize).cloned())
                .unwrap_or_else(|| pb::notetypes::notetype::Template {
                    config: Some(pb::notetypes::notetype::template::Config {
                        id: Some(random_id()),
                        ..Default::default()
                    }),
                    ..Default::default()
                });
            template.ord = t.ord.map(|val| pb::generic::UInt32 { val });
            template.name = t.name.clone();
            let c = template.config.get_or_insert_with(Default::default);
            c.q_format = t.question.clone();
            c.a_format = t.answer.clone();
            c.q_format_browser = t.browser_question.clone();
            c.a_format_browser = t.browser_answer.clone();
            c.browser_font_name = t.browser_font_name.clone();
            c.browser_font_size = t.browser_font_size;
            c.target_deck_id = t.target_deck_id.unwrap_or(0);
            template
        })
        .collect();

    let schema_change = ords_changed(draft.fields.iter().map(|f| f.ord), stored.fields.len())
        || ords_changed(
            draft.templates.iter().map(|t| t.ord),
            stored.templates.len(),
        )
        || sort_ord != previous_sort;
    if schema_change {
        confirm_schema_change(col, confirmed)?;
    }

    match NotetypesService::update_notetype(col, nt) {
        Ok(r) => Ok(NotetypeResponse::Saved {
            notetype: get(col, draft.id)?,
            changes: changes(r),
        }),
        Err(error @ AnkiError::CardTypeError { .. }) => {
            let AnkiError::CardTypeError { source } = &error else {
                unreachable!()
            };
            let kind = match &source.source {
                CardTypeErrorDetails::TemplateParseError => TemplateProblemKind::ParseError,
                CardTypeErrorDetails::Duplicate { index } => TemplateProblemKind::DuplicateFront {
                    other: *index as u32,
                },
                CardTypeErrorDetails::NoFrontField => TemplateProblemKind::NoFrontField,
                CardTypeErrorDetails::NoSuchField { field } => {
                    TemplateProblemKind::UnknownField(field.clone())
                }
                CardTypeErrorDetails::MissingCloze => TemplateProblemKind::MissingCloze,
            };
            Ok(NotetypeResponse::Rejected(TemplateProblem {
                kind,
                template: source.ordinal as u32,
                message: plain(&error.message(col.tr())),
            }))
        }
        Err(e) => Err(anki_error(col, e)),
    }
}

// ---- preview -------------------------------------------------------------------

fn preview(
    col: &mut Collection,
    notetype_id: i64,
    note_id: Option<i64>,
    template: TemplateDetail,
    css: Option<String>,
    card_ord: u32,
    fill_empty: bool,
) -> Result<TemplatePreview> {
    let _ = load(col, notetype_id)?;
    let note: NoteData = match note_id {
        Some(nid) => {
            let note = NotesService::get_note(col, pb::notes::NoteId { nid })
                .map_err(|e| anki_error(col, e))?;
            if note.notetype_id != notetype_id {
                return invalid("That note does not belong to this note type");
            }
            note.into()
        }
        None => {
            NotesService::new_note(col, pb::notetypes::NotetypeId { ntid: notetype_id })?.into()
        }
    };
    let proto_template = pb::notetypes::notetype::Template {
        ord: template.ord.map(|val| pb::generic::UInt32 { val }),
        name: template.name.clone(),
        mtime_secs: 0,
        usn: 0,
        config: Some(pb::notetypes::notetype::template::Config {
            q_format: template.question,
            a_format: template.answer,
            q_format_browser: template.browser_question,
            a_format_browser: template.browser_answer,
            target_deck_id: template.target_deck_id.unwrap_or(0),
            browser_font_name: template.browser_font_name,
            browser_font_size: template.browser_font_size,
            ..Default::default()
        }),
    };
    let rendered = CardRenderingService::render_uncommitted_card(
        col,
        pb::card_rendering::RenderUncommittedCardRequest {
            note: Some(note.into()),
            card_ord,
            template: Some(proto_template),
            fill_empty,
            partial_render: true,
        },
    );
    let error = match &rendered {
        Err(e @ AnkiError::TemplateError { .. }) => Some(plain(&e.message(col.tr()))),
        _ => None,
    };
    let mut card = finish_render(col, rendered, card_ord, notetype_id)?;
    if let Some(css) = css {
        card.css = css;
    }
    Ok(TemplatePreview { card, error })
}
