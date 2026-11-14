//! Phase 2 package implementation (Packages stream): `.apkg` import and
//! export, `.colpkg` export, and `.colpkg` restore into a staging profile.
//!
//! Traps closed here (PLAN 4.4, 8.9, 8.11; R5 §5–6):
//!
//! - Import never replaces a collection. `.colpkg` files are refused by
//!   [`execute`]; replacing goes through [`restore_to_stage`] and the lead's
//!   locked installation.
//! - An import that would mark the schema modified (and so force a one-way
//!   full sync) needs confirmation. Merging note types is refused before
//!   anything is touched. Without merging, Anki can still mark the schema when
//!   an incoming note type with the same fields and templates has a different
//!   sort field; that case is detected afterwards through the collection's
//!   schema timestamp and undone (the import is one undoable step).
//! - Export paths must carry the right extension, so an export can never be
//!   renamed over `collection.anki2` or another profile file.
//! - Restore unpacks only into an empty staging folder with its own media
//!   folder and media DB; the live profile is never touched.
use crate::{
    actor::SharedProgress, CoreError, DataResponse, ExportOptions, ExportSelection, ImportLog,
    ImportOptions, PackageCommand, Result, UpdateCondition,
};
use anki::{
    collection::{Collection, CollectionBuilder},
    error::AnkiError,
    import_export::{ImportProgress, LogNote, NoteLog},
    services::{CollectionService, DecksService, ImportExportService, SearchService},
};
use anki_proto::import_export as pb;
use std::path::Path;

/// Names Anki writes into a staging profile.
const STAGE_COLLECTION: &str = "collection.anki2";
const STAGE_MEDIA: &str = "collection.media";
const STAGE_MEDIA_DB: &str = "collection.media.db2";

/// `.apkg` import and export only. `ExportCollection` and `Restore` never
/// reach this function: the actor intercepts them.
pub fn execute(col: &mut Collection, command: PackageCommand) -> Result<DataResponse> {
    match command {
        PackageCommand::Import {
            path,
            options,
            schema_confirmed,
        } => import(col, &path, options, schema_confirmed),
        PackageCommand::Export {
            path,
            options,
            selection,
        } => export(col, &path, &options, selection),
        PackageCommand::ExportCollection { .. } | PackageCommand::Restore { .. } => Err(
            CoreError::Invalid("Collection packages are handled by the actor".into()),
        ),
    }
}

/// `.colpkg` export. Consumes the collection; the actor reopens it after.
///
/// With `legacy`, Anki closes the collection downgraded to schema 11 before
/// packing it (as desktop does); the next open upgrades it again.
pub fn export_collection(col: Collection, path: &Path, media: bool, legacy: bool) -> Result<()> {
    if let Err(error) = check_output_path(path, "colpkg") {
        col.close(None)?;
        return Err(error);
    }
    let tr = col.tr().clone();
    col.export_colpkg(path, media, legacy)
        .map_err(|e| anki_error(e, &tr))
}

/// Unpacks a `.colpkg` into the empty staging profile `stage` (collection
/// and its own media folder). Never touches the live profile.
///
/// `stage` must be an empty folder, or not exist yet (its parent must). On
/// failure, including cancellation, the files this call created in the stage
/// are removed again. A cancel requested before the call is honoured.
pub fn restore_to_stage(path: &Path, stage: &Path, progress: SharedProgress) -> Result<()> {
    let tr = anki_i18n::I18n::new(&["en"]);
    if !path.is_file() {
        return Err(CoreError::Invalid(format!(
            "No collection package at {}",
            path.display()
        )));
    }
    let package = utf8(path)?;
    let col_path = stage.join(STAGE_COLLECTION);
    let col_path_str = utf8(&col_path)?;
    prepare_stage(stage)?;
    if progress.lock().unwrap().want_abort {
        return Err(CoreError::Interrupted);
    }
    // Anki's progress handler for `import_colpkg` can only be made by a
    // collection. A throwaway in-memory one shares the actor's progress state,
    // so `CollectionActor::progress` and `cancel_operation` work for restore.
    let handler = {
        let col = CollectionBuilder::default()
            .set_shared_progress_state(progress)
            .build()
            .map_err(|e| anki_error(e, &tr))?;
        let handler = col.new_progress_handler::<ImportProgress>();
        col.close(None).map_err(|e| anki_error(e, &tr))?;
        handler
    };
    let result = anki::import_export::package::import_colpkg(
        package,
        col_path_str,
        &stage.join(STAGE_MEDIA),
        &stage.join(STAGE_MEDIA_DB),
        handler,
    );
    result.map_err(|error| {
        clean_stage(stage);
        anki_error(error, &tr)
    })
}

fn import(
    col: &mut Collection,
    path: &Path,
    options: Option<ImportOptions>,
    schema_confirmed: bool,
) -> Result<DataResponse> {
    check_import_path(path)?;
    let options = match options {
        Some(options) => import_options(&options),
        None => ImportExportService::get_import_anki_package_presets(col)
            .map_err(|e| anki_error(e, col.tr()))?,
    };
    // Would an unconfirmed schema change be refused? (Read-only check.)
    let guarded = match crate::actor::confirm_schema_change(col, schema_confirmed) {
        Ok(()) => false,
        Err(CoreError::SchemaChangeRequired) => true,
        Err(error) => return Err(error),
    };
    if options.merge_notetypes {
        // Merging rewrites, re-targets and deletes note types: always a
        // potential full sync. Refuse before touching anything.
        crate::actor::confirm_schema_change(col, schema_confirmed)?;
    }
    let schema_before = schema_stamp(col)?;
    let response = ImportExportService::import_anki_package(
        col,
        pb::ImportAnkiPackageRequest {
            package_path: utf8(path)?.into(),
            options: Some(options),
        },
    )
    .map_err(|e| anki_error(e, col.tr()))?;
    if guarded && schema_stamp(col)? != schema_before {
        // Only possible without merging (sort field of a matching note type
        // changed). Undo the import, which is one undoable step.
        let _ = CollectionService::undo(col).map_err(|e| anki_error(e, col.tr()))?;
        if schema_stamp(col)? != schema_before {
            return Err(CoreError::Other(
                "The import changed the collection schema and could not be rolled back".into(),
            ));
        }
        return Err(CoreError::SchemaChangeRequired);
    }
    Ok(DataResponse::Imported {
        log: import_log(response.log.unwrap_or_default()),
        changes: crate::actor::changes(response.changes.unwrap_or_default()),
    })
}

fn export(
    col: &mut Collection,
    path: &Path,
    options: &ExportOptions,
    selection: ExportSelection,
) -> Result<DataResponse> {
    use pb::export_limit::Limit;
    check_output_path(path, "apkg")?;
    let limit = match selection {
        ExportSelection::All => Limit::WholeCollection(Default::default()),
        ExportSelection::Deck(did) => {
            let _ = DecksService::get_deck(col, anki_proto::decks::DeckId { did })
                .map_err(|_| CoreError::Invalid(format!("No deck with id {did}")))?;
            // Includes child decks, as desktop's deck export does.
            Limit::DeckId(did)
        }
        ExportSelection::Query(search) => {
            // Anki's search parser; a syntax error is reported as such.
            let note_ids = SearchService::search_notes(
                col,
                anki_proto::search::SearchRequest {
                    search,
                    order: None,
                },
            )
            .map_err(|e| anki_error(e, col.tr()))?
            .ids;
            if note_ids.is_empty() {
                return Err(CoreError::Invalid("No notes match the search".into()));
            }
            Limit::NoteIds(anki_proto::notes::NoteIds { note_ids })
        }
    };
    let notes = ImportExportService::export_anki_package(
        col,
        pb::ExportAnkiPackageRequest {
            out_path: utf8(path)?.into(),
            options: Some(pb::ExportAnkiPackageOptions {
                with_scheduling: options.with_scheduling,
                with_deck_configs: options.with_deck_configs,
                with_media: options.with_media,
                legacy: options.legacy,
            }),
            limit: Some(pb::ExportLimit { limit: Some(limit) }),
        },
    )
    .map_err(|e| anki_error(e, col.tr()))?
    .val;
    Ok(DataResponse::Exported { notes })
}

fn import_options(options: &ImportOptions) -> pb::ImportAnkiPackageOptions {
    pb::ImportAnkiPackageOptions {
        merge_notetypes: options.merge_notetypes,
        update_notes: condition(options.update_notes),
        update_notetypes: condition(options.update_notetypes),
        with_scheduling: options.with_scheduling,
        with_deck_configs: options.with_deck_configs,
    }
}

fn condition(condition: UpdateCondition) -> i32 {
    use pb::ImportAnkiPackageUpdateCondition as C;
    match condition {
        UpdateCondition::IfNewer => C::IfNewer as i32,
        UpdateCondition::Always => C::Always as i32,
        UpdateCondition::Never => C::Never as i32,
    }
}

/// Counts plus the library's sanitized notes (HTML stripped, newlines turned
/// into spaces, fields cut at 80 bytes), by category; empty categories are
/// left out.
fn import_log(log: NoteLog) -> ImportLog {
    let fields = |notes: &[LogNote]| -> Vec<Vec<String>> {
        notes.iter().map(|note| note.fields.clone()).collect()
    };
    let mut entries = std::collections::BTreeMap::new();
    for (name, notes) in [
        ("new", &log.new),
        ("updated", &log.updated),
        ("duplicate", &log.duplicate),
        ("conflicting", &log.conflicting),
        ("first_field_match", &log.first_field_match),
        ("missing_notetype", &log.missing_notetype),
        ("missing_deck", &log.missing_deck),
        ("empty_first_field", &log.empty_first_field),
    ] {
        if !notes.is_empty() {
            entries.insert(name.to_owned(), fields(notes));
        }
    }
    ImportLog {
        new: log.new.len(),
        updated: log.updated.len(),
        duplicate: log.duplicate.len(),
        conflicting: log.conflicting.len(),
        found_notes: log.found_notes,
        entries,
    }
}

/// The collection's schema-modified time, through the public sync metadata.
fn schema_stamp(col: &Collection) -> Result<i64> {
    Ok(col
        .sync_meta()
        .map_err(|e| anki_error(e, col.tr()))?
        .schema
        .0)
}

fn check_import_path(path: &Path) -> Result<()> {
    if has_extension(path, "colpkg") {
        return Err(CoreError::Invalid(
            "A .colpkg file is a whole collection; importing it would replace yours. Use `tanki backup restore` to replace the collection.".into(),
        ));
    }
    if !path.is_file() {
        return Err(CoreError::Invalid(format!(
            "No package file at {}",
            path.display()
        )));
    }
    Ok(())
}

/// Output files must end in `.<extension>`, so Anki's final rename can never
/// land on a collection, WAL, media DB or preferences file.
fn check_output_path(path: &Path, extension: &str) -> Result<()> {
    if !has_extension(path, extension) {
        return Err(CoreError::Invalid(format!(
            "The export file name must end in .{extension}"
        )));
    }
    if path.is_dir() {
        return Err(CoreError::Invalid(format!(
            "{} is a folder",
            path.display()
        )));
    }
    utf8(path).map(|_| ())
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(extension))
}

fn utf8(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| CoreError::Invalid(format!("Path is not UTF-8: {}", path.display())))
}

/// Creates `stage` if missing (not its parents); refuses symlinks, files,
/// folders that hold a collection, and non-empty folders.
fn prepare_stage(stage: &Path) -> Result<()> {
    match std::fs::symlink_metadata(stage) {
        Ok(meta) if meta.file_type().is_symlink() => Err(CoreError::UnsafePath(format!(
            "The staging folder {} is a symlink",
            stage.display()
        ))),
        Ok(meta) if !meta.is_dir() => Err(CoreError::Invalid(format!(
            "The staging path {} is not a folder",
            stage.display()
        ))),
        Ok(_) => {
            if stage.join(STAGE_COLLECTION).exists() {
                return Err(CoreError::Invalid(format!(
                    "The staging folder {} already holds a collection",
                    stage.display()
                )));
            }
            if std::fs::read_dir(stage)?.next().is_some() {
                return Err(CoreError::Invalid(format!(
                    "The staging folder {} is not empty",
                    stage.display()
                )));
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(stage)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

/// Removes what a failed restore left in the (previously empty) stage. Anki
/// renames the collection into place last, so normally only media remain.
fn clean_stage(stage: &Path) {
    let _ = std::fs::remove_dir_all(stage.join(STAGE_MEDIA));
    for name in [
        STAGE_COLLECTION,
        STAGE_MEDIA_DB,
        "collection.media.db2-wal",
        "collection.media.db2-shm",
    ] {
        let _ = std::fs::remove_file(stage.join(name));
    }
}

/// Keeps the typed mappings (`Interrupted`, `Locked`) and otherwise uses
/// Anki's translated message instead of its debug text.
fn anki_error(error: AnkiError, tr: &anki_i18n::I18n) -> CoreError {
    let message = anki_i18n::without_unicode_isolation(&error.message(tr)).to_string();
    match CoreError::from(error) {
        CoreError::Other(_) => CoreError::Other(message),
        mapped => mapped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_checks_protect_profile_files() {
        for bad in [
            "collection.anki2",
            "x/collection.anki2-wal",
            "prefs21.db",
            "out.colpkg",
            "out",
        ] {
            assert!(check_output_path(Path::new(bad), "apkg").is_err(), "{bad}");
        }
        assert!(check_output_path(Path::new("deck.APKG"), "apkg").is_ok());
        assert!(check_output_path(Path::new("collection.anki2"), "colpkg").is_err());
        assert!(check_output_path(Path::new("all.colpkg"), "colpkg").is_ok());
    }

    #[test]
    fn colpkg_import_is_refused_before_reading() {
        let error = check_import_path(Path::new("/nonexistent/backup.colpkg")).unwrap_err();
        assert!(
            error.to_string().contains("tanki backup restore"),
            "{error}"
        );
    }

    #[test]
    fn log_conversion_keeps_counts_and_nonempty_categories() {
        let note = |text: &str| LogNote {
            id: None,
            fields: vec![text.into(), "b".into()],
        };
        let log = import_log(NoteLog {
            new: vec![note("n1"), note("n2")],
            duplicate: vec![note("d")],
            conflicting: vec![note("c")],
            found_notes: 4,
            ..Default::default()
        });
        assert_eq!(
            (log.new, log.updated, log.duplicate, log.conflicting),
            (2, 0, 1, 1)
        );
        assert_eq!(log.found_notes, 4);
        assert_eq!(
            log.entries.keys().collect::<Vec<_>>(),
            ["conflicting", "duplicate", "new"]
        );
        assert_eq!(log.entries["new"][1], ["n2", "b"]);
    }

    #[test]
    fn interrupted_and_locked_keep_their_types() {
        let tr = anki_i18n::I18n::template_only();
        assert_eq!(
            anki_error(AnkiError::Interrupted, &tr),
            CoreError::Interrupted
        );
        assert!(matches!(
            anki_error(
                AnkiError::ImportError {
                    source: anki::import_export::ImportError::Corrupt
                },
                &tr
            ),
            CoreError::Other(_)
        ));
    }
}
