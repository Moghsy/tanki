//! Shared Phase 3 contracts: notes, fields, tags, media, search and the
//! browser (PLAN 8.4-8.7). Collection ownership stays with the actor.
//!
//! `Command::Notes(NoteCommand)` is run by `notes_ops::execute`, except
//! `NoteCommand::UpdateNotes`, which `writeback::update_notes` runs.
//! `Command::Browse(BrowseCommand)` is run by `browse_ops::execute`. Both
//! answer with `Response::Edit(EditResponse)`; the variant each command
//! returns is named in its doc comment. See
//! `docs/implementation/phase-3-interfaces.md`.
use crate::OpChanges;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ---- notes and note types ---------------------------------------------------

/// A note as stored (`anki_proto::notes::Note`). `fields` are the stored HTML
/// strings, byte for byte. `id` is 0 for a note that is not added yet.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteData {
    pub id: i64,
    pub guid: String,
    pub notetype_id: i64,
    pub mtime_secs: u32,
    pub usn: i32,
    pub tags: Vec<String>,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotetypeKind {
    #[default]
    Normal,
    Cloze,
}

/// One field of a note type with the per-field options the editor uses
/// (`Notetype.Field.Config`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldInfo {
    pub ord: u32,
    pub name: String,
    pub sticky: bool,
    pub rtl: bool,
    /// "Use HTML editor by default": open in the raw HTML editor.
    pub plain_text: bool,
    pub collapsed: bool,
    /// Placeholder shown while the field is empty.
    pub description: String,
    pub exclude_from_search: bool,
    /// UI-only guard in Anki (R7 §7); tanki honours it too.
    pub prevent_deletion: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateInfo {
    pub ord: u32,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotetypeInfo {
    pub id: i64,
    pub name: String,
    pub kind: NotetypeKind,
    pub fields: Vec<FieldInfo>,
    pub templates: Vec<TemplateInfo>,
    pub sort_field: u32,
    /// Ordinals of the fields `{{cloze:…}}` uses (`GetClozeFieldOrds`).
    pub cloze_fields: Vec<u32>,
    /// `original_stock_kind == IMAGE_OCCLUSION`; tanki cannot create these
    /// notes yet (Phase 6), so the add screen refuses them.
    pub image_occlusion: bool,
}

/// `NoteFieldsCheckResponse.State`, in Anki's evaluation order (R7 §1.6).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldsCheck {
    #[default]
    Normal,
    /// First field empty after stripping HTML: adding is blocked.
    Empty,
    /// Same first field as another note of this type: a warning only.
    Duplicate,
    /// Cloze note type without a cloze: ask before adding.
    MissingCloze,
    /// A cloze in a note type that is not a cloze type: blocked.
    NotetypeNotCloze,
    /// A cloze in a field that is not a cloze field: blocked.
    FieldNotCloze,
}

/// Changed-fields-only write-back of one note (PLAN 8.5 safety rule).
///
/// The update is refused as a conflict when the stored note's `mtime_secs`
/// differs from `expected_mtime`, or, when `expected_fields` is given, when
/// any stored field differs from it (mtime has one-second resolution). Only
/// the fields listed in `fields` are replaced; the others are passed back to
/// Anki as stored. Anki itself normalises every field on any write (ASCII
/// control characters other than `\n`/`\t` removed, so CR goes; NFC when
/// `normalize_note_text`), exactly as desktop's save does. C1 controls,
/// U+2028/2029, BOM and zero-width characters are kept.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteUpdate {
    pub id: i64,
    pub expected_mtime: u32,
    /// All stored fields as they were read, when known.
    pub expected_fields: Option<Vec<String>>,
    /// Changed fields only: (ordinal, new stored HTML).
    pub fields: Vec<(u32, String)>,
    /// The complete new tag list, only when the tags changed.
    pub tags: Option<Vec<String>>,
}

/// Outcome of `NoteCommand::UpdateNotes`. All or nothing: when `conflicts`
/// or `missing` is non-empty, nothing was written.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateReport {
    pub updated: Vec<i64>,
    /// Requested, but every listed field and the tags already matched.
    pub unchanged: Vec<i64>,
    pub conflicts: Vec<i64>,
    /// Deleted since they were read.
    pub missing: Vec<i64>,
    pub changes: OpChanges,
}

/// A media file added to `collection.media`. `filename` is the name Anki
/// chose (lower-cased, de-duplicated; R7 §2.7), which may differ from the one
/// requested; always reference this one (PLAN 14 rule 12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddedMedia {
    pub filename: String,
    /// What a field should contain: `<img src="…">` for images,
    /// `[sound:…]` for audio/video, else the bare name (R7 §2.7 link rules).
    pub field_text: String,
}

/// One node of the tag tree (`TagsService.TagTree`); the root is unnamed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagNode {
    /// Last `::` component.
    pub name: String,
    /// Full tag name with `::` separators; empty for the root.
    pub full_name: String,
    pub collapsed: bool,
    pub children: Vec<TagNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeckName {
    pub id: i64,
    /// Full name with `::` separators.
    pub name: String,
    pub filtered: bool,
}

/// Requests about notes, note types, tags and media. Run by
/// `notes_ops::execute`, except `UpdateNotes` (`writeback::update_notes`).
#[derive(Debug, Clone)]
pub enum NoteCommand {
    /// All note types, sorted by name -> `Notetypes`.
    Notetypes,
    /// -> `Notetype`.
    Notetype { id: i64 },
    /// Deck names; filtered decks only when asked -> `DeckNames`.
    DeckNames { include_filtered: bool },
    /// `DefaultsForAdding`; `home_deck` is the reviewer card's home deck, or
    /// 0 -> `Defaults`.
    DefaultsForAdding { home_deck: i64 },
    /// Last deck this note type added to -> `Deck`.
    DefaultDeckForNotetype { notetype_id: i64 },
    /// An empty note of that type (not added) -> `Note`.
    NewNote { notetype_id: i64 },
    /// -> `FieldsCheck`.
    CheckFields { note: NoteData },
    /// Adds the note; Anki records the last deck/note type -> `NoteAdded`.
    AddNote { note: NoteData, deck_id: i64 },
    /// Several notes in one undo step -> `NotesAdded`.
    AddNotes { notes: Vec<(NoteData, i64)> },
    /// -> `Note`.
    GetNote { id: i64 },
    /// In request order; a deleted id is an error -> `Notes`.
    GetNotes { ids: Vec<i64> },
    /// Changed-fields-only, all-or-nothing -> `Updated`.
    UpdateNotes { updates: Vec<NoteUpdate> },
    /// Cloze numbers in use across the note's fields (multi-ordinal aware,
    /// `ClozeNumbersInNote`); the next cloze is max + 1 -> `Numbers`.
    ClozeNumbers { note: NoteData },
    /// Renders an unsaved note's card (`RenderUncommittedCard`, the
    /// pipeline of `actor::render`) for the live preview -> `Rendered`.
    RenderPreview { note: NoteData, card_ord: u32 },
    /// Persists a field's sticky flag in the note type (no schema change)
    /// -> `Changes`.
    SetSticky {
        notetype_id: i64,
        field_ord: u32,
        sticky: bool,
    },
    /// `CompleteTag` -> `Strings`.
    CompleteTag { input: String, limit: u32 },
    /// -> `Strings`.
    AllTags,
    /// -> `TagTree`.
    TagTree,
    /// Space-separated tags -> `Count`.
    AddTags { note_ids: Vec<i64>, tags: String },
    /// Space-separated tags; parents remove children -> `Count`.
    RemoveTags { note_ids: Vec<i64>, tags: String },
    /// -> `Count`.
    ClearUnusedTags,
    /// Adds bytes as a media file -> `Media`.
    AddMedia { desired_name: String, data: Vec<u8> },
    /// Copies a local file into the media folder -> `Media`.
    AddMediaFromPath { path: PathBuf },
    /// Downloads with Anki's `retrieve_url` (http(s) or file://) -> `Media`.
    AddMediaFromUrl { url: String },
    /// `HtmlToTextLine` (for history labels and previews) -> `Text`.
    HtmlToText {
        html: String,
        preserve_media_filenames: bool,
    },
}

// ---- search and browser -----------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrowseMode {
    #[default]
    Cards,
    Notes,
}

/// A built-in sort (`SortOrder.Builtin`) by a browser column key. There is
/// deliberately no custom variant: Anki's custom sort is raw SQL
/// (PLAN 14 rule 11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchOrder {
    /// Anki's default: the config's sort column for that mode.
    Default,
    /// No ORDER BY.
    None,
    Column {
        key: String,
        reverse: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortDirection {
    Ascending,
    Descending,
}

/// `BrowserColumns.Column`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnInfo {
    /// Wire key, e.g. `noteFld`, `cardDue`.
    pub key: String,
    pub cards_label: String,
    pub notes_label: String,
    /// Default direction when sorting by it; `None` = unsortable.
    pub sort_cards: Option<SortDirection>,
    pub sort_notes: Option<SortDirection>,
    pub centered: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Elide {
    Left,
    #[default]
    Right,
    Middle,
    None,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserCell {
    pub text: String,
    pub rtl: bool,
    pub elide: Elide,
}

/// Row colour, precedence already applied by Anki
/// (flag > marked > suspended > buried).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RowColor {
    #[default]
    Default,
    Marked,
    Suspended,
    Buried,
    /// 1 red … 7 purple.
    Flag(u8),
}

/// One table row. A row that vanished (`AnkiError::Deleted`) has `error`
/// set and no cells.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserRow {
    pub id: i64,
    pub cells: Vec<BrowserCell>,
    pub color: RowColor,
    pub error: Option<String>,
}

/// Browser settings kept in the collection under desktop's keys (R6 §2.2:
/// `browserTableShowNotesMode`, `activeCols`, `activeNoteCols`, `sortType`,
/// `sortBackwards`, `noteSortType`, `browserNoteSortBackwards`), so desktop
/// and tanki share them and they sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserSettings {
    pub mode: BrowseMode,
    pub card_columns: Vec<String>,
    pub note_columns: Vec<String>,
    pub card_sort: String,
    pub card_sort_reverse: bool,
    pub note_sort: String,
    pub note_sort_reverse: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidebarDeck {
    pub id: i64,
    pub name: String,
    pub full_name: String,
    /// Browser collapse state (`DeckCollapseScope::Browser`).
    pub collapsed: bool,
    pub filtered: bool,
    pub children: Vec<SidebarDeck>,
}

/// What the sidebar tree is built from (R6 §3.1). The fixed sections
/// (Today, Card state, Flags) are built by the TUI.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidebarData {
    /// `savedFilters` config JSON, sorted by name: (name, search).
    pub saved_searches: Vec<(String, String)>,
    pub decks: Vec<SidebarDeck>,
    pub tags: TagNode,
    pub notetypes: Vec<NotetypeInfo>,
    /// Labels of flags 1-7 (custom or Anki's defaults).
    pub flag_names: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchJoiner {
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FindReplaceScope {
    AllFields,
    Field(String),
    /// `FindAndReplaceTag`.
    Tags,
}

/// Find and replace (PLAN 8.7). `note_ids` must not be empty: Anki treats an
/// empty list as every note, so tanki refuses it (PLAN 14 rule 11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindReplace {
    pub note_ids: Vec<i64>,
    pub search: String,
    pub replacement: String,
    pub regex: bool,
    pub match_case: bool,
    pub scope: FindReplaceScope,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositionDefaults {
    pub random: bool,
    pub shift: bool,
}

/// Search and browser requests, run by `browse_ops::execute`.
#[derive(Debug, Clone)]
pub enum BrowseCommand {
    /// Card or note ids in order -> `Ids`.
    Search {
        query: String,
        mode: BrowseMode,
        order: SearchOrder,
    },
    /// `BuildSearchString` of the text: validates and normalises; a parse
    /// error is `CoreError::Invalid` with Anki's message -> `Text`.
    NormalizeSearch { query: String },
    /// `JoinSearchNodes` (sidebar modifiers, R6 §3.3); `negate` negates
    /// `additional` first -> `Text`.
    JoinSearch {
        existing: String,
        additional: String,
        joiner: SearchJoiner,
        negate: bool,
    },
    /// `ReplaceSearchNode` -> `Text`.
    ReplaceSearch {
        existing: String,
        replacement: String,
    },
    /// -> `Columns`.
    Columns,
    /// -> `Settings`.
    Settings,
    /// Writes the config keys and sets the active columns -> `Changes`.
    SetSettings(BrowserSettings),
    /// Rows for these ids in `mode`, cells in `columns` order. Sets Anki's
    /// active columns and notes-mode flag as needed -> `Rows`.
    Rows {
        mode: BrowseMode,
        columns: Vec<String>,
        ids: Vec<i64>,
    },
    /// -> `Sidebar`.
    Sidebar,
    /// Browser-scope collapse -> `Changes`.
    SetDeckCollapsed { deck_id: i64, collapsed: bool },
    /// -> `Changes`.
    SetTagCollapsed { name: String, collapsed: bool },
    /// Replaces the whole `savedFilters` map -> `Changes`.
    SetSavedSearches { searches: Vec<(String, String)> },
    /// All cards of these notes -> `Ids`.
    CardsOfNotes { note_ids: Vec<i64> },
    /// Distinct notes of these cards -> `Ids`.
    NotesOfCards { card_ids: Vec<i64> },
    /// -> `Count`.
    SetDeck { card_ids: Vec<i64>, deck_id: i64 },
    /// Creates the deck if the name is new, then moves -> `Count`.
    SetDeckByName { card_ids: Vec<i64>, name: String },
    /// Unsuspend and unbury (`RestoreBuriedAndSuspendedCards`) -> `Changes`.
    Restore { card_ids: Vec<i64> },
    /// -> `RepositionDefaults`.
    RepositionDefaults,
    /// `SortCards` (new cards only) -> `Count`.
    Reposition {
        card_ids: Vec<i64>,
        start: i32,
        step: i32,
        randomize: bool,
        shift: bool,
    },
    /// Browser defaults (`RESTORE_POSITION_BROWSER`, `RESET_COUNTS_BROWSER`)
    /// -> `ForgetDefaults`.
    ForgetDefaults,
    /// Reset to new, browser context -> `Changes`.
    Forget {
        card_ids: Vec<i64>,
        restore_position: bool,
        reset_counts: bool,
    },
    /// Browser's default for the set-due prompt (`setDueBrowser`) -> `Text`.
    SetDueDefault,
    /// Anki's syntax (`N`, `N-M`, `N!`); remembered as the browser default
    /// -> `Changes`.
    SetDueDate { card_ids: Vec<i64>, days: String },
    /// `GradeNow` -> `Changes`.
    GradeNow {
        card_ids: Vec<i64>,
        rating: crate::Rating,
    },
    /// Field names across these notes (`FieldNamesForNotes`) -> `Strings`.
    FieldNamesForNotes { note_ids: Vec<i64> },
    /// Refuses an empty `note_ids` -> `Count`.
    FindReplace(FindReplace),
}

/// Answers to `NoteCommand` and `BrowseCommand`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EditResponse {
    Notetypes(Vec<NotetypeInfo>),
    Notetype(NotetypeInfo),
    DeckNames(Vec<DeckName>),
    Defaults {
        deck_id: i64,
        notetype_id: i64,
    },
    Deck(Option<i64>),
    Note(NoteData),
    Notes(Vec<NoteData>),
    FieldsCheck(FieldsCheck),
    NoteAdded {
        id: i64,
        changes: OpChanges,
    },
    NotesAdded {
        ids: Vec<i64>,
        changes: OpChanges,
    },
    Updated(UpdateReport),
    Numbers(Vec<u32>),
    Rendered(crate::RenderedCard),
    Strings(Vec<String>),
    TagTree(TagNode),
    /// Notes (or cards) affected.
    Count {
        count: usize,
        changes: OpChanges,
    },
    Media(AddedMedia),
    Text(String),
    Ids(Vec<i64>),
    Columns(Vec<ColumnInfo>),
    Settings(BrowserSettings),
    Rows(Vec<BrowserRow>),
    Sidebar(SidebarData),
    RepositionDefaults(RepositionDefaults),
    ForgetDefaults(crate::ForgetDefaults),
    Changes(OpChanges),
}

impl EditResponse {
    /// The changes a mutating command made; the actor broadcasts these.
    pub fn changes(&self) -> Option<&OpChanges> {
        match self {
            Self::NoteAdded { changes, .. }
            | Self::NotesAdded { changes, .. }
            | Self::Count { changes, .. }
            | Self::Changes(changes) => Some(changes),
            Self::Updated(report) if !report.updated.is_empty() => Some(&report.changes),
            _ => None,
        }
    }
}
