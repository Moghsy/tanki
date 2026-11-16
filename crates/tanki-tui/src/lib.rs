//! tanki-tui: the full-screen interface (PLAN 7, 11): deck list, overview,
//! reviewer, congrats and image zoom (Phase 1), sync and packages (Phase 2),
//! the add / edit screen and the browser (Phase 3), management screens
//! (Phase 5: deck options, deck tools, note types, statistics, import/export
//! extras, tools), over
//! `tanki_core::CollectionActor`.
pub mod add_ui;
pub mod app;
pub mod backend;
pub mod browser_ui;
pub mod charts;
pub mod completion;
pub mod deck_options_ui;
pub mod deck_tools_ui;
pub mod editor_flow;
pub mod fuzzy;
pub mod input;
pub mod keymap;
pub mod links;
pub mod notetypes_ui;
pub mod package_ui;
pub mod plugins;
pub mod reviewer;
pub mod services;
pub mod stats_ui;
pub mod sync_ui;
pub mod terminal;
pub mod textarea;
pub mod tools_ui;
pub mod transfer_ui;
mod ui;

#[cfg(test)]
mod testing;

/// PLAN decision 1.
pub const DISCLAIMER: &str = "tanki is an unofficial, Anki-compatible client. It is not affiliated with or endorsed by Ankitects. Anki is a trademark of Ankitects Pty Ltd.";
