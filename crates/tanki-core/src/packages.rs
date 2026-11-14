//! Phase 2 package implementation (Packages stream). Signatures are fixed by
//! `docs/implementation/phase-2-interfaces.md`; bodies are placeholders.
use crate::{actor::SharedProgress, CoreError, DataResponse, PackageCommand, Result};
use anki::collection::Collection;
use std::path::Path;

fn todo<T>() -> Result<T> {
    Err(CoreError::Invalid(
        "Packages are not implemented yet".into(),
    ))
}

/// `.apkg` import and export only. `ExportCollection` and `Restore` never
/// reach this function: the actor intercepts them.
pub fn execute(_col: &mut Collection, _command: PackageCommand) -> Result<DataResponse> {
    todo()
}

/// `.colpkg` export. Consumes the collection; the actor reopens it after.
pub fn export_collection(
    _col: Collection,
    _path: &Path,
    _media: bool,
    _legacy: bool,
) -> Result<()> {
    todo()
}

/// Unpacks a `.colpkg` into the empty staging profile `stage` (collection
/// and its own media folder). Never touches the live profile.
pub fn restore_to_stage(_path: &Path, _stage: &Path, _progress: SharedProgress) -> Result<()> {
    todo()
}
