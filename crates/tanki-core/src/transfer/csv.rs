//! CSV/TSV import metadata and the editable mapping (R5 §5.4).
//!
//! The settings are a serde mirror of Anki's `CsvMetadata`, so the TUI and
//! CLI never touch protobuf types. Column numbers are one-based; 0 means
//! "none", as in Anki.
use super::{anki_error, check_input, utf8};
use crate::{CoreError, Result};
use anki::{
    collection::Collection,
    services::{ImportExportService, NotetypesService},
};
use anki_proto::import_export as pb;
use pb::csv_metadata as m;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CsvDelimiter {
    #[default]
    Tab,
    Pipe,
    Semicolon,
    Colon,
    Comma,
    Space,
}

impl CsvDelimiter {
    pub const ALL: [Self; 6] = [
        Self::Tab,
        Self::Pipe,
        Self::Semicolon,
        Self::Colon,
        Self::Comma,
        Self::Space,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Tab => "Tab",
            Self::Pipe => "Pipe",
            Self::Semicolon => "Semicolon",
            Self::Colon => "Colon",
            Self::Comma => "Comma",
            Self::Space => "Space",
        }
    }
    fn to_proto(self) -> m::Delimiter {
        match self {
            Self::Tab => m::Delimiter::Tab,
            Self::Pipe => m::Delimiter::Pipe,
            Self::Semicolon => m::Delimiter::Semicolon,
            Self::Colon => m::Delimiter::Colon,
            Self::Comma => m::Delimiter::Comma,
            Self::Space => m::Delimiter::Space,
        }
    }
    fn from_proto(value: m::Delimiter) -> Self {
        match value {
            m::Delimiter::Tab => Self::Tab,
            m::Delimiter::Pipe => Self::Pipe,
            m::Delimiter::Semicolon => Self::Semicolon,
            m::Delimiter::Colon => Self::Colon,
            m::Delimiter::Comma => Self::Comma,
            m::Delimiter::Space => Self::Space,
        }
    }
}

/// What to do with a row that matches an existing note.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DupeResolution {
    /// Update the existing note.
    #[default]
    Update,
    /// Keep the existing note and skip the row.
    Preserve,
    /// Import the row as a new note anyway.
    Duplicate,
}

impl DupeResolution {
    pub const ALL: [Self; 3] = [Self::Update, Self::Preserve, Self::Duplicate];
    pub fn name(self) -> &'static str {
        match self {
            Self::Update => "Update current",
            Self::Preserve => "Keep current",
            Self::Duplicate => "Keep both",
        }
    }
}

/// Which notes a row is matched against.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MatchScope {
    #[default]
    Notetype,
    NotetypeAndDeck,
}

impl MatchScope {
    pub const ALL: [Self; 2] = [Self::Notetype, Self::NotetypeAndDeck];
    pub fn name(self) -> &'static str {
        match self {
            Self::Notetype => "Note type",
            Self::NotetypeAndDeck => "Note type and deck",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CsvDeck {
    /// An existing deck.
    Id(i64),
    /// The deck is named in this column.
    Column(u32),
    /// A deck to create when it does not exist.
    Name(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CsvNotetype {
    /// One note type for all rows. `field_columns[i]` is the column of field
    /// `i` (0 = unmapped).
    Global { id: i64, field_columns: Vec<u32> },
    /// The note type is named in this column; fields map by position.
    Column(u32),
}

/// Anki's `CsvMetadata`, minus the preview.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CsvSettings {
    pub delimiter: CsvDelimiter,
    pub is_html: bool,
    pub global_tags: Vec<String>,
    /// Applied only to notes that already exist and are updated.
    pub updated_tags: Vec<String>,
    /// Labels from a `#columns:` header; empty strings otherwise. Also
    /// gives the number of columns.
    pub column_labels: Vec<String>,
    pub deck: Option<CsvDeck>,
    pub notetype: Option<CsvNotetype>,
    pub tags_column: u32,
    pub guid_column: u32,
    pub dupe_resolution: DupeResolution,
    pub match_scope: MatchScope,
    /// The file's own header named the delimiter / HTML flag.
    pub force_delimiter: bool,
    pub force_is_html: bool,
}

impl CsvSettings {
    pub(super) fn to_proto(&self) -> pb::CsvMetadata {
        pb::CsvMetadata {
            delimiter: self.delimiter.to_proto() as i32,
            is_html: self.is_html,
            global_tags: self.global_tags.clone(),
            updated_tags: self.updated_tags.clone(),
            column_labels: self.column_labels.clone(),
            deck: self.deck.as_ref().map(|d| match d {
                CsvDeck::Id(id) => m::Deck::DeckId(*id),
                CsvDeck::Column(c) => m::Deck::DeckColumn(*c),
                CsvDeck::Name(n) => m::Deck::DeckName(n.clone()),
            }),
            notetype: self.notetype.as_ref().map(|n| match n {
                CsvNotetype::Global { id, field_columns } => {
                    m::Notetype::GlobalNotetype(m::MappedNotetype {
                        id: *id,
                        field_columns: field_columns.clone(),
                    })
                }
                CsvNotetype::Column(c) => m::Notetype::NotetypeColumn(*c),
            }),
            tags_column: self.tags_column,
            force_delimiter: self.force_delimiter,
            force_is_html: self.force_is_html,
            preview: Vec::new(),
            guid_column: self.guid_column,
            dupe_resolution: match self.dupe_resolution {
                DupeResolution::Update => m::DupeResolution::Update,
                DupeResolution::Preserve => m::DupeResolution::Preserve,
                DupeResolution::Duplicate => m::DupeResolution::Duplicate,
            } as i32,
            match_scope: match self.match_scope {
                MatchScope::Notetype => m::MatchScope::Notetype,
                MatchScope::NotetypeAndDeck => m::MatchScope::NotetypeAndDeck,
            } as i32,
        }
    }

    fn from_proto(meta: &pb::CsvMetadata) -> Self {
        Self {
            delimiter: CsvDelimiter::from_proto(meta.delimiter()),
            is_html: meta.is_html,
            global_tags: meta.global_tags.clone(),
            updated_tags: meta.updated_tags.clone(),
            column_labels: meta.column_labels.clone(),
            deck: meta.deck.as_ref().map(|d| match d {
                m::Deck::DeckId(id) => CsvDeck::Id(*id),
                m::Deck::DeckColumn(c) => CsvDeck::Column(*c),
                m::Deck::DeckName(n) => CsvDeck::Name(n.clone()),
            }),
            notetype: meta.notetype.as_ref().map(|n| match n {
                m::Notetype::GlobalNotetype(g) => CsvNotetype::Global {
                    id: g.id,
                    field_columns: g.field_columns.clone(),
                },
                m::Notetype::NotetypeColumn(c) => CsvNotetype::Column(*c),
            }),
            tags_column: meta.tags_column,
            guid_column: meta.guid_column,
            dupe_resolution: match meta.dupe_resolution() {
                m::DupeResolution::Update => DupeResolution::Update,
                m::DupeResolution::Preserve => DupeResolution::Preserve,
                m::DupeResolution::Duplicate => DupeResolution::Duplicate,
            },
            match_scope: match meta.match_scope() {
                m::MatchScope::Notetype => MatchScope::Notetype,
                m::MatchScope::NotetypeAndDeck => MatchScope::NotetypeAndDeck,
            },
            force_delimiter: meta.force_delimiter,
            force_is_html: meta.force_is_html,
        }
    }

    /// Number of columns in the file.
    pub fn columns(&self) -> usize {
        self.column_labels.len()
    }
}

/// Overrides for the metadata call. `None` lets Anki decide (header
/// directives first, then detection, then the collection's defaults).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CsvMetadataRequest {
    pub path: PathBuf,
    pub delimiter: Option<CsvDelimiter>,
    /// Re-guess the mapping for this note type.
    pub notetype_id: Option<i64>,
    pub deck_id: Option<i64>,
    pub is_html: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CsvMetadataInfo {
    pub settings: CsvSettings,
    /// The first records, HTML-stripped and shortened by Anki for display.
    pub preview: Vec<Vec<String>>,
    /// Name and field names of the globally mapped note type, if any.
    pub notetype_name: Option<String>,
    pub field_names: Vec<String>,
}

pub(super) fn metadata(
    col: &mut Collection,
    request: &CsvMetadataRequest,
) -> Result<CsvMetadataInfo> {
    check_input(&request.path)?;
    let meta = ImportExportService::get_csv_metadata(
        col,
        pb::CsvMetadataRequest {
            path: utf8(&request.path)?.into(),
            delimiter: request.delimiter.map(|d| d.to_proto() as i32),
            notetype_id: request.notetype_id,
            deck_id: request.deck_id,
            is_html: request.is_html,
        },
    )
    .map_err(|e| anki_error(e, col.tr()))?;
    let settings = CsvSettings::from_proto(&meta);
    let (notetype_name, field_names) = match &settings.notetype {
        Some(CsvNotetype::Global { id, .. }) => {
            let notetype = NotetypesService::get_notetype(
                col,
                anki_proto::notetypes::NotetypeId { ntid: *id },
            )
            .map_err(|e| anki_error(e, col.tr()))?;
            (
                Some(notetype.name),
                notetype.fields.into_iter().map(|f| f.name).collect(),
            )
        }
        _ => (None, Vec::new()),
    };
    if settings.columns() == 0 {
        return Err(CoreError::Invalid("The file has no columns".into()));
    }
    Ok(CsvMetadataInfo {
        settings,
        preview: meta.preview.into_iter().map(|row| row.vals).collect(),
        notetype_name,
        field_names,
    })
}
