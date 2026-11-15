//! Collection actor, study commands and adapters to the pinned Anki core.

mod actor;
mod backup;
mod browse_ops;
pub mod config;
pub mod connect;
mod custom_scheduling;
mod data;
pub mod deck_options;
pub mod deck_tools;
mod edit;
mod edit_convert;
mod fixture;
mod latex;
pub mod maintenance;
mod manage;
pub mod notefile;
mod notes_ops;
pub mod notetypes;
#[doc(hidden)]
pub mod packages;
mod replacement;
mod safety;
pub mod stats;
mod study;
mod sync;
#[doc(hidden)]
pub mod sync_ops;
pub mod transfer;
mod types;
mod writeback;
pub use actor::{CollectionActor, PendingCommand, PendingOperation};
pub use connect::{ConnectCommand, ConnectResponse};
pub use data::*;
pub use deck_options::{DeckOptionsCommand, DeckOptionsResponse};
pub use deck_tools::{DeckToolsCommand, DeckToolsResponse};
pub use edit::*;
pub use maintenance::{MaintenanceCommand, MaintenanceResponse};
pub use manage::{ManageCommand, ManageResponse};
pub use notetypes::{NotetypeCommand, NotetypeResponse};
pub use safety::validate_profile;
pub use stats::{StatsCommand, StatsResponse};
pub use sync::SyncEndpoint;
pub use transfer::{TransferCommand, TransferResponse};
pub use types::*;

#[cfg(test)]
mod tests {
    #[test]
    fn schema_change_needs_confirmation_unless_full_sync_pending() {
        let dir = tempfile::tempdir().unwrap();
        let mut col = anki::collection::CollectionBuilder::new(dir.path().join("c.anki2"))
            .build()
            .unwrap();
        assert_eq!(
            crate::actor::confirm_schema_change(&mut col, false),
            Err(crate::CoreError::SchemaChangeRequired)
        );
        assert_eq!(crate::actor::confirm_schema_change(&mut col, true), Ok(()));
        col.close(None).unwrap();
    }

    #[test]
    fn patched_progress_is_typed_and_cancellable() {
        let state = std::sync::Arc::new(std::sync::Mutex::new(
            anki::progress::ProgressState::default(),
        ));
        let _builder =
            anki::collection::CollectionBuilder::default().set_shared_progress_state(state.clone());
        let tr = anki_i18n::I18n::template_only();
        let mut progress = state.lock().unwrap();
        let message: anki_proto::collection::Progress =
            anki::progress::progress_to_proto(progress.last_progress, &tr);
        assert!(matches!(
            message.value,
            Some(anki_proto::collection::progress::Value::None(_))
        ));
        progress.last_progress = Some(anki::progress::Progress::MediaCheck(Default::default()));
        let message = anki::progress::progress_to_proto(progress.last_progress, &tr);
        assert!(matches!(
            message.value,
            Some(anki_proto::collection::progress::Value::MediaCheck(_))
        ));
        progress.want_abort = true;
        assert!(progress.want_abort);
    }
}
