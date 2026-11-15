//! The Tools screen against a real collection actor on a temp study fixture.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{buffer::Buffer, layout::Rect};
use std::path::Path;
use tanki_core::maintenance::{MaintenanceCommand, MaintenanceResponse};
use tanki_core::{
    CollectionActor, Command, EditResponse, ManageCommand, ManageResponse, NoteCommand, NoteUpdate,
    Response,
};
use tanki_tui::tools_ui::{ToolsIntent, ToolsUi};

fn ch(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn screen(ui: &ToolsUi) -> String {
    let area = Rect::new(0, 0, 90, 24);
    let mut buf = Buffer::empty(area);
    ui.render(area, &mut buf);
    let text = |y| {
        (0..area.width)
            .map(|x| buf[(x, y)].symbol())
            .collect::<String>()
            .trim_end()
            .to_owned()
    };
    (0..area.height).map(text).collect::<Vec<_>>().join("\n")
}

/// Ticks until the running check finishes; returns the last non-empty intent.
fn finish(ui: &mut ToolsUi, actor: &CollectionActor) -> ToolsIntent {
    let mut last = ToolsIntent::None;
    for _ in 0..600 {
        let intent = ui.tick(actor);
        if intent != ToolsIntent::None {
            last = intent;
        }
        if !ui.is_busy() {
            return last;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("check did not finish");
}

fn backups(profile: &Path) -> usize {
    std::fs::read_dir(profile.join("backups"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".colpkg"))
        .count()
}

fn maintenance(actor: &CollectionActor, c: MaintenanceCommand) -> MaintenanceResponse {
    match actor
        .execute(Command::Manage(ManageCommand::Maintenance(c)))
        .unwrap()
    {
        Response::Manage(ManageResponse::Maintenance(r)) => r,
        other => panic!("{other:?}"),
    }
}

fn fixture() -> (tempfile::TempDir, CollectionActor) {
    let dir = tempfile::tempdir().unwrap();
    let actor = CollectionActor::open(dir.path()).unwrap();
    actor.execute(Command::GenerateStudyFixture).unwrap();
    (dir, actor)
}

#[test]
fn check_database_and_media_run_on_the_real_actor() {
    let (dir, actor) = fixture();
    let mut ui = ToolsUi::new(None, None, None);
    let before = backups(dir.path());

    ui.on_key(ch('1'), &actor);
    assert!(screen(&ui).contains("will first write a backup"));
    assert_eq!(backups(dir.path()), before, "nothing before y");
    ui.on_key(ch('y'), &actor);
    let intent = finish(&mut ui, &actor);
    assert!(matches!(intent, ToolsIntent::Changed(_)), "{intent:?}");
    let text = screen(&ui);
    assert!(text.contains("Backup written: backup-"), "{text}");
    assert!(text.contains("No problems found"), "{text}");
    assert!(backups(dir.path()) > before);
    ui.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &actor);

    let stray = dir.path().join("collection.media/stray-file.txt");
    std::fs::write(&stray, b"x").unwrap();
    ui.on_key(ch('2'), &actor);
    ui.on_key(ch('y'), &actor);
    finish(&mut ui, &actor);
    let text = screen(&ui);
    assert!(text.contains("stray-file.txt"), "{text}");
    assert!(stray.exists(), "listing deletes nothing");
    ui.on_key(ch('t'), &actor);
    ui.on_key(ch('y'), &actor);
    assert!(!stray.exists());
    assert!(dir.path().join("media.trash/stray-file.txt").exists());
    ui.on_key(ch('r'), &actor);
    assert!(stray.exists(), "restored");
    // The collection is usable after the checks.
    assert!(matches!(
        maintenance(&actor, MaintenanceCommand::GetPreferences),
        MaintenanceResponse::Preferences(_)
    ));
}

fn note_with_empty_card(actor: &CollectionActor) {
    let edit = |c| match actor.execute(Command::Notes(c)).unwrap() {
        Response::Edit(r) => r,
        other => panic!("{other:?}"),
    };
    let EditResponse::Notetypes(all) = edit(NoteCommand::Notetypes) else {
        panic!()
    };
    let nt = all
        .iter()
        .find(|n| n.name == "Basic (optional reversed card)")
        .unwrap();
    let EditResponse::Note(mut note) = edit(NoteCommand::NewNote { notetype_id: nt.id }) else {
        panic!()
    };
    note.fields = vec!["f".into(), "b".into(), "y".into()];
    let EditResponse::NoteAdded { id, .. } = edit(NoteCommand::AddNote { note, deck_id: 1 }) else {
        panic!()
    };
    let EditResponse::Note(note) = edit(NoteCommand::GetNote { id }) else {
        panic!()
    };
    edit(NoteCommand::UpdateNotes {
        updates: vec![NoteUpdate {
            id,
            expected_mtime: note.mtime_secs,
            expected_fields: None,
            fields: vec![(2, String::new())],
            tags: None,
        }],
    });
}

#[test]
fn empty_cards_are_reported_then_deleted_after_asking() {
    let (_dir, actor) = fixture();
    note_with_empty_card(&actor);
    let mut ui = ToolsUi::new(None, None, None);
    ui.on_key(ch('3'), &actor);
    finish(&mut ui, &actor);
    let text = screen(&ui);
    assert!(text.contains("1 card(s) would be deleted"), "{text}");
    ui.on_key(ch('d'), &actor);
    ui.on_key(ch('n'), &actor);
    assert!(matches!(
        maintenance(&actor, MaintenanceCommand::FindEmptyCards),
        MaintenanceResponse::EmptyCards(r) if !r.is_empty()
    ));
    ui.on_key(ch('d'), &actor);
    let intent = ui.on_key(ch('y'), &actor);
    assert!(
        matches!(intent, ToolsIntent::Changed(ref c) if c.card),
        "{intent:?}"
    );
    assert!(matches!(
        maintenance(&actor, MaintenanceCommand::FindEmptyCards),
        MaintenanceResponse::EmptyCards(r) if r.is_empty()
    ));
}

#[test]
fn preferences_save_through_the_form() {
    let (_dir, actor) = fixture();
    let mut ui = ToolsUi::new(None, None, None);
    ui.on_key(ch('4'), &actor);
    // First field: the rollover hour.
    ui.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &actor);
    for _ in 0..3 {
        ui.on_key(
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            &actor,
        );
    }
    ui.on_key(ch('7'), &actor);
    ui.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &actor);
    // An out-of-range hour is refused by the form itself.
    ui.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &actor);
    ui.on_key(ch('9'), &actor);
    ui.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &actor);
    assert!(screen(&ui).contains("from 0 to 23"));
    let intent = ui.on_key(ch('s'), &actor);
    assert!(
        matches!(intent, ToolsIntent::Changed(ref c) if c.config),
        "{intent:?}"
    );
    let MaintenanceResponse::Preferences(p) =
        maintenance(&actor, MaintenanceCommand::GetPreferences)
    else {
        panic!()
    };
    assert_eq!(p.rollover, 7);
}

#[test]
fn profiles_screen_manages_folders_beside_the_open_profile() {
    let base = tempfile::tempdir().unwrap();
    let open = base.path().join("User 1");
    std::fs::create_dir(&open).unwrap();
    let actor = CollectionActor::open(&open).unwrap();
    let cfg = base.path().join("scratch.toml");
    let mut ui = ToolsUi::new(
        Some(base.path().to_path_buf()),
        Some("User 1".into()),
        Some(cfg),
    );
    ui.on_key(ch('5'), &actor);
    ui.on_key(ch('n'), &actor);
    ui.paste("Second");
    ui.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &actor);
    assert!(base.path().join("Second/collection.anki2").is_file());
    let text = screen(&ui);
    assert!(
        text.contains("Second") && text.contains("* User 1"),
        "{text}"
    );
    // Sorted: "Second" comes before "User 1".
    assert_eq!(
        ui.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &actor),
        ToolsIntent::SwitchProfile("Second".into())
    );
    ui.on_key(ch('j'), &actor);
    ui.on_key(ch('d'), &actor);
    assert!(screen(&ui).contains("open and cannot be deleted"));
    assert!(open.join("collection.anki2").is_file());
    actor.close().unwrap();
}
