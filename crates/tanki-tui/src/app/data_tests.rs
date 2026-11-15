//! Sync and package operations through the app, against the fake backend.
use super::*;
use crate::services::{NoImages, PlainTextRenderer};
use crate::testing::{buffer_text, t0, FakeAudio, FakeBackend};
use ratatui::{backend::TestBackend, Terminal};
use tanki_core::{
    config::{credentials_path, Config, Credentials},
    CoreError, DataCommand, DataResponse, ImportLog, PackageCommand, SyncCommand, SyncCredentials,
    SyncDirection, SyncReport, SyncRequired,
};

const URL: &str = "http://127.0.0.1:1/";

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tanki-tui-data-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn config(&self) -> PathBuf {
        self.0.join("config").join("config.toml")
    }
    fn profile(&self) -> PathBuf {
        self.0.join("profile")
    }
    /// A configured and logged-in profile.
    fn login(&self) {
        let key = self.profile().to_string_lossy().into_owned();
        let mut config = Config::default();
        config.profile_mut(&key).sync.url = Some(URL.into());
        config.save(&self.config()).unwrap();
        let mut credentials = Credentials::default();
        credentials
            .set(
                &key,
                SyncCredentials {
                    endpoint: URL.into(),
                    hkey: "hkey".into(),
                    timeout_secs: 30,
                },
            )
            .unwrap();
        credentials.save(&credentials_path(&self.config())).unwrap();
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn app<'a>(b: &'a FakeBackend, temp: &Temp, config: bool) -> App<'a> {
    let mut app = App::new(
        b,
        Services {
            renderer: Box::new(PlainTextRenderer),
            images: Box::new(NoImages),
            audio: Box::new(FakeAudio::default()),
            display: Display {
                cell_px: Some((10, 20)),
                fg: None,
                bg: None,
                night_mode: true,
            },
            opener: Box::new(|_| Ok(())),
        },
        Profile {
            name: "fixture".into(),
            dir: temp.profile(),
            media_dir: temp.profile().join("collection.media"),
        },
        Start::Decks,
        t0(),
    );
    app.set_config(config.then(|| temp.config()));
    app
}

fn press(a: &mut App<'_>, code: KeyCode) {
    press_with(a, code, KeyModifiers::NONE);
}
fn press_with(a: &mut App<'_>, code: KeyCode, modifiers: KeyModifiers) {
    let now = t0();
    a.handle(AppEvent::Key(KeyEvent::new(code, modifiers), now.at), now);
}
fn type_text(a: &mut App<'_>, text: &str) {
    a.handle(AppEvent::Paste(text.into()), t0());
}
fn tick(a: &mut App<'_>) {
    a.handle(AppEvent::Tick, t0());
}
fn message(a: &App<'_>) -> String {
    a.visible_message()
        .map(|m| m.text.clone())
        .unwrap_or_default()
}
fn started(b: &FakeBackend) -> Vec<DataCommand> {
    b.data.borrow().started.clone()
}
fn decks_loaded(b: &FakeBackend) -> usize {
    b.names().iter().filter(|n| *n == "DeckTree").count()
}
fn synced(required: SyncRequired, new_endpoint: Option<&str>) -> DataResponse {
    DataResponse::Synced(SyncReport {
        required,
        server_message: String::new(),
        new_endpoint: new_endpoint.map(String::from),
        synced_before: false,
    })
}
fn snapshot(name: &str, a: &mut App<'_>) {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| a.draw(f)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let rows = buffer_text(&buffer).join("\n");
    let styled = format!("{rows}\n\n{buffer:?}\n");
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("{name}.snap"));
    if std::env::var_os("TANKI_UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, &styled).unwrap();
    }
    assert_eq!(styled, std::fs::read_to_string(path).unwrap());
}
fn screen_text(a: &mut App<'_>) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| a.draw(f)).unwrap();
    buffer_text(terminal.backend().buffer()).join("\n")
}

#[test]
fn sync_without_config_or_login_points_to_the_cli() {
    let temp = Temp::new();
    let b = FakeBackend::new(vec![]);
    let mut a = app(&b, &temp, false);
    press(&mut a, KeyCode::Char('y'));
    assert!(
        message(&a).contains("sync is not configured"),
        "{}",
        message(&a)
    );
    assert!(message(&a).contains("tanki sync login --url URL"));
    assert!(started(&b).is_empty());
    assert!(!a.sync_ui.is_visible());

    // A config file with no sync section is the same.
    let mut a = app(&b, &temp, true);
    press(&mut a, KeyCode::Char('y'));
    assert!(message(&a).contains("sync is not configured"));

    // Configured but not logged in.
    temp.login();
    std::fs::remove_file(credentials_path(&temp.config())).unwrap();
    press(&mut a, KeyCode::Char('y'));
    assert!(message(&a).contains("not logged in"), "{}", message(&a));
    assert!(started(&b).is_empty());
}

#[test]
fn sync_runs_with_progress_resolves_a_conflict_and_refreshes() {
    let temp = Temp::new();
    temp.login();
    let b = FakeBackend::new(vec![]);
    let mut a = app(&b, &temp, true);
    {
        let mut data = b.data.borrow_mut();
        data.hold.set(true);
        data.results.push_back(Ok(synced(
            SyncRequired::Full {
                upload: true,
                download: true,
            },
            None,
        )));
        data.results.push_back(Ok(DataResponse::Done));
    }
    press(&mut a, KeyCode::Char('y'));
    let commands = started(&b);
    let [DataCommand::Sync(SyncCommand::Run {
        auth,
        direction: SyncDirection::Normal,
        media: true,
    })] = commands.as_slice()
    else {
        panic!("unexpected {commands:?}");
    };
    assert_eq!(auth.hkey, "hkey");
    assert_eq!(auth.endpoint, URL);

    // Progress is shown while the operation is pending.
    b.data.borrow_mut().progress = tanki_core::OperationProgress {
        stage: "Syncing collection".into(),
        current: Some(2),
        total: Some(8),
    };
    tick(&mut a);
    assert!(a.sync_ui.is_running());
    let text = screen_text(&mut a);
    assert!(text.contains("Syncing collection"), "{text}");
    snapshot("decks_sync_progress", &mut a);

    // The conflict screen asks; nothing is chosen for the user.
    b.data.borrow().hold.set(false);
    tick(&mut a);
    assert!(!a.sync_ui.is_running());
    assert_eq!(started(&b).len(), 1);
    assert!(screen_text(&mut a).contains("choose one side"));

    let before = decks_loaded(&b);
    press(&mut a, KeyCode::Char('u'));
    assert!(matches!(
        started(&b)[1],
        DataCommand::Sync(SyncCommand::Run {
            direction: SyncDirection::Upload,
            ..
        })
    ));
    tick(&mut a);
    assert!(screen_text(&mut a).contains("Full upload complete."));
    assert!(decks_loaded(&b) > before, "the deck list is refreshed");
    press(&mut a, KeyCode::Enter);
    assert!(!a.sync_ui.is_visible());
    assert!(!a.data_busy());
}

#[test]
fn cancel_keys_call_cancel_and_ctrl_c_does_not_quit() {
    let temp = Temp::new();
    temp.login();
    let b = FakeBackend::new(vec![]);
    let mut a = app(&b, &temp, true);
    b.data
        .borrow_mut()
        .results
        .push_back(Err(CoreError::Interrupted));
    b.data.borrow().hold.set(true);
    press(&mut a, KeyCode::Char('y'));
    press_with(&mut a, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(b.data.borrow().cancels, 1);
    assert!(!a.should_quit());
    // The actor answers with an interruption.
    b.data.borrow().hold.set(false);
    tick(&mut a);
    assert!(screen_text(&mut a).contains("Sync cancelled"));
    press(&mut a, KeyCode::Enter);
    // Ctrl+C quits again when nothing is running.
    press_with(&mut a, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert!(a.should_quit());
}

#[test]
fn redirected_endpoint_is_persisted_to_config_and_credentials() {
    let temp = Temp::new();
    temp.login();
    let b = FakeBackend::new(vec![]);
    let mut a = app(&b, &temp, true);
    b.data.borrow_mut().results.push_back(Ok(synced(
        SyncRequired::NoChanges,
        Some("http://127.0.0.1:2/"),
    )));
    press(&mut a, KeyCode::Char('y'));
    tick(&mut a);
    let key = temp.profile().to_string_lossy().into_owned();
    let config = Config::load(&temp.config()).unwrap();
    assert_eq!(
        config.profile(&key).unwrap().sync.url.as_deref(),
        Some("http://127.0.0.1:2/")
    );
    let credentials = Credentials::load(&credentials_path(&temp.config())).unwrap();
    let auth = credentials.get(&key).unwrap().unwrap();
    assert_eq!(auth.endpoint, "http://127.0.0.1:2/");
    assert_eq!(auth.hkey, "hkey");
    assert!(message(&a).contains("saved"));
}

#[test]
fn a_rejected_login_is_forgotten() {
    let temp = Temp::new();
    temp.login();
    let b = FakeBackend::new(vec![]);
    let mut a = app(&b, &temp, true);
    b.data
        .borrow_mut()
        .results
        .push_back(Err(CoreError::Invalid(
            tanki_core::sync_ops::AUTH_REQUIRED.into(),
        )));
    press(&mut a, KeyCode::Char('y'));
    tick(&mut a);
    assert!(screen_text(&mut a).contains("Sync failed"));
    let credentials = Credentials::load(&credentials_path(&temp.config())).unwrap();
    let key = temp.profile().to_string_lossy().into_owned();
    assert!(credentials.get(&key).unwrap().is_none());
    press(&mut a, KeyCode::Enter);
    press(&mut a, KeyCode::Char('y'));
    assert!(message(&a).contains("not logged in"));
}

#[test]
fn only_one_data_operation_runs_at_a_time() {
    let temp = Temp::new();
    temp.login();
    let b = FakeBackend::new(vec![]);
    let mut a = app(&b, &temp, true);
    b.data.borrow().hold.set(true);
    a.dispatch(Action::BackupNow);
    assert!(matches!(
        started(&b)[0],
        DataCommand::Backup { force: true }
    ));
    press(&mut a, KeyCode::Char('y'));
    assert!(message(&a).contains("still running"));
    assert_eq!(started(&b).len(), 1);
    b.data.borrow().hold.set(false);
    tick(&mut a);
    assert!(message(&a).contains("Backup created"));
    press(&mut a, KeyCode::Char('y'));
    assert_eq!(started(&b).len(), 2);
}

fn log() -> DataResponse {
    let mut log = ImportLog {
        new: 2,
        updated: 1,
        found_notes: 3,
        ..Default::default()
    };
    log.entries.insert(
        "New".into(),
        vec![vec!["Capital of France?".into(), "Paris".into()]],
    );
    DataResponse::Imported {
        log,
        changes: Default::default(),
    }
}

#[test]
fn import_prompt_sends_the_typed_path_and_confirms_a_schema_change() {
    let temp = Temp::new();
    let file = temp.0.join("deck.apkg");
    std::fs::write(&file, b"x").unwrap();
    let b = FakeBackend::new(vec![]);
    let mut a = app(&b, &temp, false);
    {
        let mut data = b.data.borrow_mut();
        data.results.push_back(Err(CoreError::SchemaChangeRequired));
        data.results.push_back(Ok(log()));
    }
    press_with(&mut a, KeyCode::Char('I'), KeyModifiers::SHIFT);
    assert!(matches!(a.overlay, Some(Overlay::Prompt { .. })));
    type_text(&mut a, file.to_str().unwrap());
    press(&mut a, KeyCode::Enter);
    assert!(a.overlay.is_none());
    assert!(a.package_ui.is_some());
    assert!(started(&b).is_empty(), "the options form comes first");
    press(&mut a, KeyCode::Char('i'));
    assert!(matches!(
        &started(&b)[0],
        DataCommand::Package(PackageCommand::Import { path, options: None, schema_confirmed: false })
            if *path == file
    ));
    tick(&mut a);
    // Refused: the confirmation screen asks first.
    assert_eq!(started(&b).len(), 1);
    press(&mut a, KeyCode::Char('y'));
    assert!(matches!(
        &started(&b)[1],
        DataCommand::Package(PackageCommand::Import {
            schema_confirmed: true,
            ..
        })
    ));
    tick(&mut a);
    let text = screen_text(&mut a);
    assert!(text.contains("Capital of France?"), "{text}");
    snapshot("decks_import_result", &mut a);
    let before = decks_loaded(&b);
    press(&mut a, KeyCode::Enter);
    assert!(a.package_ui.is_none());
    assert!(decks_loaded(&b) > before);
}

#[test]
fn import_rejects_missing_files_and_colpkg_goes_to_restore_confirmation() {
    let temp = Temp::new();
    let b = FakeBackend::new(vec![]);
    let mut a = app(&b, &temp, false);
    a.dispatch(Action::Import);
    type_text(&mut a, "/no/such/file.apkg");
    press(&mut a, KeyCode::Enter);
    assert!(message(&a).contains("is not a file"));
    assert!(a.package_ui.is_none());

    let file = temp.0.join("old.colpkg");
    std::fs::write(&file, b"x").unwrap();
    a.dispatch(Action::Import);
    type_text(&mut a, file.to_str().unwrap());
    press(&mut a, KeyCode::Enter);
    assert!(screen_text(&mut a).contains("Restore"));
    assert!(started(&b).is_empty());
}

#[test]
fn export_validates_names_and_picks_the_selection() {
    let temp = Temp::new();
    let b = FakeBackend::new(vec![]);
    let mut a = app(&b, &temp, false);
    a.dispatch(Action::Export);
    type_text(&mut a, temp.0.join("out.doc").to_str().unwrap());
    press(&mut a, KeyCode::Enter);
    assert!(message(&a).contains(".apkg, .colpkg"));

    let taken = temp.0.join("taken.apkg");
    std::fs::write(&taken, b"x").unwrap();
    a.dispatch(Action::Export);
    type_text(&mut a, taken.to_str().unwrap());
    press(&mut a, KeyCode::Enter);
    assert!(message(&a).contains("exists"));
    assert!(started(&b).is_empty());

    let all = temp.0.join("all.colpkg");
    a.dispatch(Action::Export);
    type_text(&mut a, all.to_str().unwrap());
    press(&mut a, KeyCode::Enter);
    assert!(matches!(
        &started(&b)[0],
        DataCommand::Package(PackageCommand::ExportCollection { path, media: true, legacy: false })
            if *path == all
    ));
    tick(&mut a);
    assert!(screen_text(&mut a).contains("Exported the whole collection"));
    press(&mut a, KeyCode::Enter);
    assert!(a.package_ui.is_none());

    // From the overview an .apkg is the current deck.
    a.open_overview(Some(1));
    let deck = temp.0.join("deck.apkg");
    press_with(&mut a, KeyCode::Char('e'), KeyModifiers::CONTROL);
    type_text(&mut a, deck.to_str().unwrap());
    press(&mut a, KeyCode::Enter);
    assert!(matches!(
        &started(&b)[1],
        DataCommand::Package(PackageCommand::Export {
            selection: tanki_core::ExportSelection::Deck(1),
            ..
        })
    ));
}

#[test]
fn restore_is_picked_from_the_backup_list_and_needs_confirmation() {
    let temp = Temp::new();
    let b = FakeBackend::new(vec![]);
    let mut a = app(&b, &temp, false);
    let newest = temp.0.join("backup-2.colpkg");
    b.data
        .borrow_mut()
        .results
        .push_back(Ok(DataResponse::Backups(vec![
            tanki_core::BackupInfo {
                path: newest.clone(),
                bytes: 2048,
            },
            tanki_core::BackupInfo {
                path: temp.0.join("backup-1.colpkg"),
                bytes: 10,
            },
        ])));
    press_with(&mut a, KeyCode::Char('R'), KeyModifiers::SHIFT);
    assert!(matches!(started(&b)[0], DataCommand::ListBackups));
    tick(&mut a);
    let text = screen_text(&mut a);
    assert!(text.contains("backup-2.colpkg"), "{text}");
    press(&mut a, KeyCode::Enter);
    assert_eq!(started(&b).len(), 1, "picking only opens the confirmation");
    assert!(screen_text(&mut a).contains("Restore"));
    // Enter without the word does nothing.
    press(&mut a, KeyCode::Enter);
    assert_eq!(started(&b).len(), 1);
    type_text_keys(&mut a, "restore");
    press(&mut a, KeyCode::Enter);
    assert!(matches!(
        &started(&b)[1],
        DataCommand::Package(PackageCommand::Restore { path, confirmed: true }) if *path == newest
    ));
    tick(&mut a);
    assert!(screen_text(&mut a).contains("Restored the collection"));
    let before = decks_loaded(&b);
    press(&mut a, KeyCode::Enter);
    assert!(decks_loaded(&b) > before);
}

fn type_text_keys(a: &mut App<'_>, text: &str) {
    for c in text.chars() {
        press(a, KeyCode::Char(c));
    }
}

#[test]
fn new_actions_are_in_the_palette_and_help() {
    let temp = Temp::new();
    let b = FakeBackend::new(vec![]);
    let mut a = app(&b, &temp, false);
    let names: Vec<String> = a
        .palette_matches("")
        .into_iter()
        .map(|action| action.name())
        .collect();
    for wanted in ["sync", "import", "export", "backup-now", "restore-backup"] {
        assert!(names.iter().any(|n| n == wanted), "{wanted} in {names:?}");
    }
    a.dispatch(Action::Help);
    // The deck list has more keys than fit: scroll until the row shows.
    let mut text = screen_text(&mut a);
    for _ in 0..60 {
        if text.contains("Sync with the sync server") {
            break;
        }
        press(&mut a, KeyCode::Down);
        text = screen_text(&mut a);
    }
    assert!(text.contains("Sync with the sync server"), "{text}");
}

#[test]
fn shared_decks_open_ankiweb_in_the_web_browser() {
    let temp = Temp::new();
    let b = FakeBackend::new(vec![]);
    let mut a = app(&b, &temp, false);
    let opened = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = opened.clone();
    a.services.opener = Box::new(move |path| {
        seen.borrow_mut().push(path.to_path_buf());
        Ok(())
    });
    a.dispatch(Action::SharedDecks);
    assert_eq!(
        *opened.borrow(),
        [std::path::PathBuf::from(
            "https://ankiweb.net/shared/decks/"
        )]
    );
    assert!(message(&a).contains("import"), "{}", message(&a));
}
