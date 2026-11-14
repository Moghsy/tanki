//! Phase 2 sync implementation (Sync stream). Signatures are fixed by
//! `docs/implementation/phase-2-interfaces.md`; bodies are placeholders.
use crate::{CoreError, DataResponse, Result, SyncCommand, SyncCredentials, SyncServerOptions};
use anki::collection::Collection;

fn todo<T>() -> Result<T> {
    Err(CoreError::Invalid("Sync is not implemented yet".into()))
}

/// Login, status, normal sync and media sync. `Run` with a non-normal
/// direction never reaches this function: the actor intercepts it.
pub fn execute(_col: &mut Collection, _command: SyncCommand) -> Result<DataResponse> {
    todo()
}

/// One-way full upload. Consumes the collection, as Anki's call does.
pub fn upload(_col: Collection, _auth: SyncCredentials) -> Result<()> {
    todo()
}

/// One-way full download. Called ONLY with a staging collection by the actor.
pub fn download(_col: Collection, _auth: SyncCredentials) -> Result<()> {
    todo()
}

/// Runs Anki's sync server until it fails. Blocking.
pub fn run_server(_options: SyncServerOptions) -> Result<()> {
    todo()
}
