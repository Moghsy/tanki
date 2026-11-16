//! Out-of-process plugins. See `docs/implementation/plugins.md`.
//!
//! A plugin is a separate executable that talks newline-delimited JSON on
//! stdio. It receives only hooks and note fields that its manifest declares and
//! the user granted, and it can only use granted capabilities. Nothing here
//! touches the collection or the terminal.
pub mod host;
pub mod manifest;
pub mod protocol;
pub mod registry;

pub use manifest::{Capability, Hook, Manifest};
pub use registry::{Event, EventKind, Grant, HookContext, Registry};

/// Protocol and manifest API version implemented by this host.
pub const API: u32 = 1;
