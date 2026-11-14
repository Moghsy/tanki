//! The note file format for `$EDITOR` round trips and `tanki add --file`
//! (PLAN 8.5, R7 §6.4): one block per note with front matter (note type,
//! deck, tags) and one heading per field. Pure text processing; it never
//! touches a collection. Owned by the Editors stream.
//!
//! The invariant: `parse(&write(notes)) == notes` byte for byte for every
//! field value (the Phase 3 exit test), so comparing parsed values with the
//! originals finds exactly the fields the user changed.
use serde::{Deserialize, Serialize};

/// A note as it appears in a note file. Field values are stored HTML.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileNote {
    /// `None` for a note to add.
    pub id: Option<i64>,
    /// The note's `mtime_secs` when it was written out (conflict check).
    pub mtime: Option<u32>,
    pub notetype: String,
    pub deck: Option<String>,
    pub tags: Vec<String>,
    /// (field name, stored HTML), in note-type order.
    pub fields: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseError {
    /// 1-based line in the file.
    pub line: usize,
    pub message: String,
}
impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}
impl std::error::Error for ParseError {}

pub fn write(_notes: &[FileNote]) -> String {
    unimplemented!("notefile::write")
}

pub fn parse(_text: &str) -> Result<Vec<FileNote>, ParseError> {
    unimplemented!("notefile::parse")
}
