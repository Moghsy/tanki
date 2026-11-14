//! tanki-tui: the full-screen interface (PLAN 7, 11): deck list, overview,
//! reviewer, congrats and image zoom (Phase 1), sync and packages (Phase 2),
//! the add / edit screen and the browser (Phase 3), over
//! `tanki_core::CollectionActor`.
pub mod add_ui;
pub mod app;
pub mod backend;
pub mod browser_ui;
pub mod completion;
pub mod editor_flow;
pub mod fuzzy;
pub mod input;
pub mod keymap;
pub mod links;
pub mod package_ui;
pub mod reviewer;
pub mod services;
pub mod sync_ui;
pub mod terminal;
pub mod textarea;
mod ui;

#[cfg(test)]
mod testing;

/// PLAN decision 1.
pub const DISCLAIMER: &str = "tanki is an unofficial, Anki-compatible client. It is not affiliated with or endorsed by Ankitects. Anki is a trademark of Ankitects Pty Ltd.";
