//! Backups through the real collection actor, driven by the app's
//! non-blocking data operations on a temporary profile.
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use tanki_core::CollectionActor;
use tanki_tui::{
    app::{App, AppEvent, Overlay, Profile, Services, Start},
    keymap::Action,
    reviewer::Now,
    services::{AnkiRenderer, Display, MediaAudio, NoImages},
};

struct FixtureDir(PathBuf);
impl FixtureDir {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("tanki-tui-data-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn wait_idle(app: &mut App<'_>) {
    let started = Instant::now();
    while app.data_busy() {
        assert!(started.elapsed() < Duration::from_secs(120), "timed out");
        app.handle(AppEvent::Tick, Now::current());
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn backup_now_and_restore_list_run_through_the_real_actor() {
    let dir = FixtureDir::new();
    let actor = CollectionActor::open(&dir.0).unwrap();
    let services = Services {
        renderer: Box::new(AnkiRenderer),
        images: Box::new(NoImages),
        audio: Box::new(MediaAudio {
            audio: tanki_media::Audio::start(tanki_media::AudioConfig {
                media_dir: actor.media_dir(),
                backend: tanki_media::BackendChoice::Off,
                ..Default::default()
            }),
        }),
        display: Display {
            cell_px: Some((10, 20)),
            fg: None,
            bg: None,
            night_mode: true,
        },
        opener: Box::new(|_| panic!("no browser")),
    };
    let profile = Profile {
        name: "fixture".into(),
        dir: actor.profile_dir().to_owned(),
        media_dir: actor.media_dir(),
    };
    let mut app = App::new(&actor, services, profile, Start::Decks, Now::current());
    // Without a config path, sync is unavailable and says so.
    app.dispatch(Action::Sync);
    assert!(app
        .visible_message()
        .unwrap()
        .text
        .contains("sync is not configured"));

    app.dispatch(Action::BackupNow);
    assert!(
        app.data_busy(),
        "the backup runs while the app keeps polling"
    );
    wait_idle(&mut app);
    let text = app.visible_message().unwrap().text.clone();
    assert!(text.starts_with("Backup"), "{text}");
    assert!(!app.visible_message().unwrap().error, "{text}");

    app.dispatch(Action::RestoreBackup);
    wait_idle(&mut app);
    let Some(Overlay::Choice { options, .. }) = &app.overlay else {
        panic!("expected the backup list, got {:?}", app.visible_message());
    };
    assert!(options.len() >= 2, "one backup and Cancel");
    assert!(options[0].0.contains(".colpkg"), "{}", options[0].0);
    app.shutdown();
    drop(app);
    actor.close().unwrap();
}
