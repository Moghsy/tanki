use anki::{
    backend::Backend,
    error::{AnkiError, DbErrorKind},
    services::BackendCollectionService,
};
use tanki_core::{CollectionActor, CoreError};

fn schema(path: &std::path::Path) -> i64 {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    conn.query_row("select ver from col", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn schema_11_upgrade_preserves_pre_upgrade_bytes() {
    std::thread::Builder::new()
        .name("tanki-schema-actor".into())
        .spawn(|| {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("collection.anki2");
            let backups = dir.path().join("backups");
            std::fs::create_dir(&backups).unwrap();
            std::fs::create_dir(dir.path().join("collection.media")).unwrap();
            let backend = Backend::new(anki_i18n::I18n::template_only(), false);
            BackendCollectionService::open_collection(
                &backend,
                anki_proto::collection::OpenCollectionRequest {
                    collection_path: file.to_string_lossy().into(),
                    media_folder_path: dir.path().join("collection.media").to_string_lossy().into(),
                    media_db_path: dir
                        .path()
                        .join("collection.media.db2")
                        .to_string_lossy()
                        .into(),
                },
            )
            .unwrap();
            let _created = BackendCollectionService::create_backup(
                &backend,
                anki_proto::collection::CreateBackupRequest {
                    backup_folder: backups.to_string_lossy().into(),
                    force: true,
                    wait_for_completion: true,
                },
            )
            .unwrap();
            BackendCollectionService::close_collection(
                &backend,
                anki_proto::collection::CloseCollectionRequest {
                    downgrade_to_schema11: true,
                },
            )
            .unwrap();
            BackendCollectionService::await_backup_completion(&backend).unwrap();
            assert_eq!(schema(&file), 11);
            let before = std::fs::read(&file).unwrap();
            CollectionActor::open(dir.path()).unwrap().close().unwrap();
            assert_eq!(schema(&file), 18);
            let copies: Vec<_> = std::fs::read_dir(&backups)
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| {
                    p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("tanki-pre-upgrade-")
                })
                .collect();
            assert_eq!(copies.len(), 1);
            assert_eq!(std::fs::read(&copies[0]).unwrap(), before);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn future_schema_is_refused_by_wrapper_and_library() {
    std::thread::Builder::new().name("tanki-future-schema-actor".into()).spawn(|| {
        let dir = tempfile::tempdir().unwrap();
        CollectionActor::open(dir.path()).unwrap().close().unwrap();
        let file = dir.path().join("collection.anki2");
        // Explicit user-approved exception to PLAN section14 rule4: only a
        // generated, closed temp fixture's col.ver is changed, for this spike.
        let conn = rusqlite::Connection::open(&file).unwrap();
        conn.execute("update col set ver=19", []).unwrap();
        drop(conn);
        let before = std::fs::read(&file).unwrap();
        assert!(matches!(CollectionActor::open(dir.path()), Err(CoreError::SchemaTooNew(19))));
        assert_eq!(std::fs::read(&file).unwrap(),before);
        let result = anki::collection::CollectionBuilder::new(&file).build();
        assert!(matches!(result, Err(AnkiError::DbError { source }) if source.kind == DbErrorKind::FileTooNew));
        assert_eq!(schema(&file), 19);
    }).unwrap().join().unwrap();
}
