//! tanki-tui: the full-screen study interface (PLAN 7, 11 Phase 1): deck
//! list, overview, reviewer, congrats and image zoom, over
//! `tanki_core::CollectionActor`.
pub mod backend;
pub mod fuzzy;
pub mod input;
pub mod keymap;
pub mod reviewer;
pub mod services;

#[cfg(test)]
mod testing;

/// PLAN decision 1.
pub const DISCLAIMER: &str = "tanki is an unofficial, Anki-compatible client. It is not affiliated with or endorsed by Ankitects. Anki is a trademark of Ankitects Pty Ltd.";
