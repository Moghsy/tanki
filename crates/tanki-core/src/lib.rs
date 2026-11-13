//! tanki core: Phase 0 workspace foundation.

mod actor;
mod fixture;
mod safety;
mod study;
mod sync;
mod types;
pub use actor::CollectionActor;
pub use safety::validate_profile;
pub use sync::SyncEndpoint;
pub use types::*;

#[cfg(test)]
mod tests {
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
