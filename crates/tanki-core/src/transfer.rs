//! CSV/JSON import, text exports and find duplicates (PLAN 8.11).
//!
//! Owned by the Import/export extras stream (docs/implementation/phase-5-interfaces.md).
//! Reached through `Command::Manage(ManageCommand::Transfer(…))`.
//!
//! - CSV/TSV import is two calls: [`TransferCommand::CsvMetadata`] reads the
//!   file's header directives, guesses the delimiter and mapping and returns a
//!   [`CsvSettings`] the mapping form edits; [`TransferCommand::ImportCsv`]
//!   runs the import (start it through `CollectionActor::start` for progress
//!   and cancellation) and returns the result log.
//! - JSON (`ForeignData`) import takes a file; one that creates note types is
//!   guarded like the package import (schema change needs confirmation).
//! - Notes and cards as text export Anki's own text formats.
//! - Find duplicates is a rewrite of pylib's `find_dupes` (R6): it exists in
//!   Python only, over raw SQL, so this goes through search and `GetNote`.
use crate::{actor::changes, CoreError, ImportLog, OpChanges, Result};
use anki::{
    collection::Collection,
    error::AnkiError,
    services::{CollectionService, ImportExportService, SearchService},
};
use anki_proto::import_export as pb;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

mod csv;
mod dupes;

pub use csv::{
    CsvDeck, CsvDelimiter, CsvMetadataInfo, CsvMetadataRequest, CsvNotetype, CsvSettings,
    DupeResolution, MatchScope,
};
pub use dupes::DuplicateGroup;

/// What a text export covers. A search is resolved to ids first, since
/// Anki's export limit has no search type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextExportSelection {
    All,
    /// The deck and its children.
    Deck(i64),
    Query(String),
    /// For a notes export; a cards export takes `CardIds`.
    NoteIds(Vec<i64>),
    CardIds(Vec<i64>),
}

/// Each variant's doc comment names its `TransferResponse` variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TransferCommand {
    /// Header directives, delimiter and a preview of a text file, with the
    /// guessed mapping -> `CsvMetadata`.
    CsvMetadata(CsvMetadataRequest),
    /// Long operation. Imports with these (edited) settings -> `Imported`.
    ImportCsv {
        path: PathBuf,
        settings: CsvSettings,
    },
    /// A `ForeignData` JSON file -> `Imported`.
    ImportJson {
        path: PathBuf,
        /// A file that adds note types needs this, or the call fails with
        /// `SchemaChangeRequired` and nothing is imported.
        schema_confirmed: bool,
    },
    /// Notes as a text file (tab separated, with `#` header lines the
    /// importer reads back) -> `Exported`.
    ExportNotes {
        path: PathBuf,
        selection: TextExportSelection,
        with_html: bool,
        with_tags: bool,
        with_deck: bool,
        with_notetype: bool,
        with_guid: bool,
    },
    /// Cards as rendered question/answer text -> `Exported`.
    ExportCards {
        path: PathBuf,
        selection: TextExportSelection,
        with_html: bool,
    },
    /// Names of the fields of all note types, sorted: the field choice of
    /// find duplicates -> `FieldNames`.
    DuplicateFieldNames,
    /// Groups of notes with equal text in a field -> `Duplicates`.
    FindDuplicates {
        field_name: String,
        /// An optional filter in Anki's search syntax.
        search: String,
    },
    /// Adds the localised tag "duplicate" -> `Tagged`.
    TagDuplicates { note_ids: Vec<i64> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TransferResponse {
    CsvMetadata(Box<CsvMetadataInfo>),
    Imported { log: ImportLog, changes: OpChanges },
    Exported { count: u32 },
    FieldNames(Vec<String>),
    Duplicates(Vec<DuplicateGroup>),
    Tagged { count: usize, changes: OpChanges },
}

impl TransferResponse {
    /// The changes a mutating command made; the actor broadcasts these.
    pub fn changes(&self) -> Option<&OpChanges> {
        match self {
            Self::Imported { changes, .. } | Self::Tagged { changes, .. } => Some(changes),
            _ => None,
        }
    }
}

pub(crate) fn execute(col: &mut Collection, command: TransferCommand) -> Result<TransferResponse> {
    match command {
        TransferCommand::CsvMetadata(request) => {
            csv::metadata(col, &request).map(|info| TransferResponse::CsvMetadata(Box::new(info)))
        }
        TransferCommand::ImportCsv { path, settings } => {
            check_input(&path)?;
            let response = ImportExportService::import_csv(
                col,
                pb::ImportCsvRequest {
                    path: utf8(&path)?.into(),
                    metadata: Some(settings.to_proto()),
                },
            )
            .map_err(|e| anki_error(e, col.tr()))?;
            Ok(imported(response))
        }
        TransferCommand::ImportJson {
            path,
            schema_confirmed,
        } => import_json(col, &path, schema_confirmed),
        TransferCommand::ExportNotes {
            path,
            selection,
            with_html,
            with_tags,
            with_deck,
            with_notetype,
            with_guid,
        } => {
            check_output(&path)?;
            let limit = limit(col, selection, true)?;
            let count = ImportExportService::export_note_csv(
                col,
                pb::ExportNoteCsvRequest {
                    out_path: utf8(&path)?.into(),
                    with_html,
                    with_tags,
                    with_deck,
                    with_notetype,
                    with_guid,
                    limit: Some(limit),
                },
            )
            .map_err(|e| anki_error(e, col.tr()))?
            .val;
            Ok(TransferResponse::Exported { count })
        }
        TransferCommand::ExportCards {
            path,
            selection,
            with_html,
        } => {
            check_output(&path)?;
            let limit = limit(col, selection, false)?;
            let count = ImportExportService::export_card_csv(
                col,
                pb::ExportCardCsvRequest {
                    out_path: utf8(&path)?.into(),
                    with_html,
                    limit: Some(limit),
                },
            )
            .map_err(|e| anki_error(e, col.tr()))?
            .val;
            Ok(TransferResponse::Exported { count })
        }
        TransferCommand::DuplicateFieldNames => {
            dupes::field_names(col).map(TransferResponse::FieldNames)
        }
        TransferCommand::FindDuplicates { field_name, search } => {
            dupes::find(col, &field_name, &search).map(TransferResponse::Duplicates)
        }
        TransferCommand::TagDuplicates { note_ids } => dupes::tag(col, note_ids),
    }
}

fn import_json(col: &mut Collection, path: &Path, confirmed: bool) -> Result<TransferResponse> {
    check_input(path)?;
    // A file with its own note types can mark the schema modified.
    let guarded = match crate::actor::confirm_schema_change(col, confirmed) {
        Ok(()) => false,
        Err(CoreError::SchemaChangeRequired) => true,
        Err(error) => return Err(error),
    };
    let before = schema_stamp(col)?;
    let response = ImportExportService::import_json_file(
        col,
        anki_proto::generic::String {
            val: utf8(path)?.into(),
        },
    )
    .map_err(|e| anki_error(e, col.tr()))?;
    if guarded && schema_stamp(col)? != before {
        let _ = CollectionService::undo(col).map_err(|e| anki_error(e, col.tr()))?;
        if schema_stamp(col)? != before {
            return Err(CoreError::Other(
                "The import changed the collection schema and could not be rolled back".into(),
            ));
        }
        return Err(CoreError::SchemaChangeRequired);
    }
    Ok(imported(response))
}

fn schema_stamp(col: &Collection) -> Result<i64> {
    Ok(col
        .sync_meta()
        .map_err(|e| anki_error(e, col.tr()))?
        .schema
        .0)
}

fn imported(response: pb::ImportResponse) -> TransferResponse {
    TransferResponse::Imported {
        log: import_log(response.log.unwrap_or_default()),
        changes: changes(response.changes.unwrap_or_default()),
    }
}

fn import_log(log: pb::import_response::Log) -> ImportLog {
    let mut entries = std::collections::BTreeMap::new();
    let mut counts = [0usize; 4];
    for (index, (name, notes)) in [
        ("new", &log.new),
        ("updated", &log.updated),
        ("duplicate", &log.duplicate),
        ("conflicting", &log.conflicting),
        ("first_field_match", &log.first_field_match),
        ("missing_notetype", &log.missing_notetype),
        ("missing_deck", &log.missing_deck),
        ("empty_first_field", &log.empty_first_field),
    ]
    .into_iter()
    .enumerate()
    {
        if index < 4 {
            counts[index] = notes.len();
        }
        if !notes.is_empty() {
            entries.insert(
                name.to_owned(),
                notes.iter().map(|note| note.fields.clone()).collect(),
            );
        }
    }
    ImportLog {
        new: counts[0],
        updated: counts[1],
        duplicate: counts[2],
        conflicting: counts[3],
        found_notes: log.found_notes,
        entries,
    }
}

/// Anki's export limit for a selection. Searches are resolved to ids.
fn limit(
    col: &mut Collection,
    selection: TextExportSelection,
    notes: bool,
) -> Result<pb::ExportLimit> {
    use pb::export_limit::Limit;
    let search = |col: &mut Collection, query: String| -> Result<Vec<i64>> {
        let request = anki_proto::search::SearchRequest {
            search: query,
            order: None,
        };
        let found = if notes {
            SearchService::search_notes(col, request)
        } else {
            SearchService::search_cards(col, request)
        };
        Ok(found.map_err(|e| anki_error(e, col.tr()))?.ids)
    };
    let ids = |col: &mut Collection, ids: Vec<i64>| -> Limit {
        let _ = col;
        if notes {
            Limit::NoteIds(anki_proto::notes::NoteIds { note_ids: ids })
        } else {
            Limit::CardIds(anki_proto::cards::CardIds { cids: ids })
        }
    };
    let limit = match selection {
        TextExportSelection::All => Limit::WholeCollection(Default::default()),
        TextExportSelection::Deck(did) => Limit::DeckId(did),
        TextExportSelection::Query(query) => {
            let found = search(col, query)?;
            if found.is_empty() {
                return Err(CoreError::Invalid("Nothing matches the search".into()));
            }
            ids(col, found)
        }
        TextExportSelection::NoteIds(found) | TextExportSelection::CardIds(found) => {
            if found.is_empty() {
                return Err(CoreError::Invalid("Nothing is selected".into()));
            }
            ids(col, found)
        }
    };
    Ok(pb::ExportLimit { limit: Some(limit) })
}

fn check_input(path: &Path) -> Result<()> {
    if !path.is_file() {
        return Err(CoreError::Invalid(format!("No file at {}", path.display())));
    }
    utf8(path).map(|_| ())
}

/// A text export may not be renamed over a profile file: refuse the file
/// names Anki's profile folder uses, and folders.
fn check_output(path: &Path) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let reserved = name.starts_with("collection.") || name.starts_with("prefs21");
    let allowed = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["txt", "tsv", "csv", "text"].contains(&e.to_ascii_lowercase().as_str()));
    if reserved || !allowed {
        return Err(CoreError::Invalid(
            "The text export file name must end in .txt, .tsv or .csv".into(),
        ));
    }
    if path.is_dir() {
        return Err(CoreError::Invalid(format!(
            "{} is a folder",
            path.display()
        )));
    }
    utf8(path).map(|_| ())
}

pub(crate) fn utf8(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| CoreError::Invalid(format!("Path is not UTF-8: {}", path.display())))
}

pub(crate) fn anki_error(error: AnkiError, tr: &anki_i18n::I18n) -> CoreError {
    let message = anki_i18n::without_unicode_isolation(&error.message(tr)).to_string();
    match CoreError::from(error) {
        CoreError::Other(_) => CoreError::Other(message),
        mapped => mapped,
    }
}
