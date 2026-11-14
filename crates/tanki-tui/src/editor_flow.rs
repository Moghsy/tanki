//! Editors stream (Phase 3): the `$EDITOR` round trip shared by the TUI and
//! `tanki edit`: write notes to a note file, run the editor, parse, reopen on
//! errors, then write back only the fields that changed.
