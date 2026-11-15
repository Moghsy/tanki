//! Cell-buffer snapshots and key handling of the Tools screen, against a
//! fake `Backend` that answers the maintenance commands.
use super::*;
use crate::backend::{Backend, CommandOp};
use crate::testing::buffer_text;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use tanki_core::maintenance::{
    DatabaseReport, EmptyCardNote, EmptyCardsReport, MaintenanceCommand as Cmd,
    MaintenanceResponse as Resp, MediaReport, Preferences,
};
use tanki_core::OperationProgress;

fn prefs() -> Preferences {
    Preferences {
        rollover: 4,
        learn_ahead_secs: 1200,
        new_review_mix: 0,
        new_timezone: false,
        day_learn_first: false,
        hide_audio_play_buttons: false,
        interrupt_audio_when_answering: true,
        show_remaining_due_counts: true,
        show_intervals_on_buttons: false,
        time_limit_secs: 0,
        load_balancer_enabled: true,
        fsrs_short_term_with_steps_enabled: false,
        adding_defaults_to_current_deck: true,
        paste_images_as_png: false,
        paste_strips_formatting: false,
        default_search_text: String::new(),
        ignore_accents_in_search: false,
        render_latex: true,
        backup_daily: 12,
        backup_weekly: 10,
        backup_monthly: 9,
        backup_minimum_interval_mins: 30,
    }
}

fn media_report() -> MediaReport {
    MediaReport {
        unused: vec!["old.png".into(), "stray.mp3".into()],
        missing: vec!["gone.jpg".into()],
        missing_media_notes: vec![7],
        report: "Missing files: 1\nUnused files: 2".into(),
        have_trash: false,
        backup: Some("backup-2026-11-15-10.00.00.colpkg".into()),
    }
}

fn empty_report() -> EmptyCardsReport {
    EmptyCardsReport {
        report: "Basic (and reversed card)\n  note 1: 1 empty card".into(),
        notes: vec![
            EmptyCardNote {
                note_id: 1,
                card_ids: vec![10],
                all_cards_empty: false,
            },
            EmptyCardNote {
                note_id: 2,
                card_ids: vec![20, 21],
                all_cards_empty: true,
            },
        ],
    }
}

struct Op {
    hold: Rc<Cell<bool>>,
    result: RefCell<Option<tanki_core::Result<Response>>>,
}
impl CommandOp for Op {
    fn poll(&self) -> tanki_core::Result<Option<Response>> {
        if self.hold.get() {
            return Ok(None);
        }
        match self.result.borrow_mut().take() {
            Some(r) => r.map(Some),
            None => Ok(None),
        }
    }
}

struct Fake {
    log: RefCell<Vec<Cmd>>,
    hold: Rc<Cell<bool>>,
    progress: RefCell<OperationProgress>,
    cancels: Cell<u32>,
    /// Result of the next started command.
    started: RefCell<VecDeque<tanki_core::Result<Resp>>>,
    /// Error for the next executed command.
    fail: RefCell<Option<tanki_core::CoreError>>,
}

impl Fake {
    fn new() -> Self {
        Self {
            log: RefCell::new(Vec::new()),
            hold: Rc::new(Cell::new(false)),
            progress: RefCell::new(OperationProgress::default()),
            cancels: Cell::new(0),
            started: RefCell::new(VecDeque::new()),
            fail: RefCell::new(None),
        }
    }
    fn queue(&self, r: tanki_core::Result<Resp>) {
        self.started.borrow_mut().push_back(r);
    }
    fn log(&self) -> Vec<Cmd> {
        self.log.borrow().clone()
    }
}

fn wrap_resp(r: Resp) -> Response {
    Response::Manage(ManageResponse::Maintenance(r))
}

fn changes() -> OpChanges {
    OpChanges {
        card: true,
        ..Default::default()
    }
}

impl Backend for Fake {
    fn execute(&self, command: Command) -> tanki_core::Result<Response> {
        let Command::Manage(ManageCommand::Maintenance(c)) = command else {
            panic!("unexpected {command:?}");
        };
        self.log.borrow_mut().push(c.clone());
        if let Some(e) = self.fail.borrow_mut().take() {
            return Err(e);
        }
        Ok(wrap_resp(match c {
            Cmd::GetPreferences => Resp::Preferences(prefs()),
            Cmd::SetPreferences(_) => Resp::PreferencesSaved(OpChanges {
                config: true,
                ..Default::default()
            }),
            Cmd::TrashMedia { files } => Resp::MediaTrashed(files.len()),
            Cmd::DeleteEmptyCards { keep_notes } => Resp::EmptyCardsDeleted {
                cards: if keep_notes { 2 } else { 3 },
                changes: changes(),
            },
            Cmd::EmptyMediaTrash | Cmd::RestoreMediaTrash => Resp::Done,
            other => panic!("not executed synchronously: {other:?}"),
        }))
    }
    fn start(&self, command: Command) -> tanki_core::Result<Box<dyn CommandOp>> {
        let Command::Manage(ManageCommand::Maintenance(c)) = command else {
            panic!("unexpected {command:?}");
        };
        self.log.borrow_mut().push(c);
        let result = self
            .started
            .borrow_mut()
            .pop_front()
            .expect("no queued result")
            .map(wrap_resp);
        Ok(Box::new(Op {
            hold: self.hold.clone(),
            result: RefCell::new(Some(result)),
        }))
    }
    fn data_progress(&self) -> OperationProgress {
        self.progress.borrow().clone()
    }
    fn cancel_data(&self) {
        self.cancels.set(self.cancels.get() + 1);
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn ch(c: char) -> KeyEvent {
    key(KeyCode::Char(c))
}

fn screen(ui: &ToolsUi) -> String {
    let area = Rect::new(0, 0, 78, 14);
    let mut buf = Buffer::empty(area);
    ui.render(area, &mut buf);
    buffer_text(&buf).join("\n").trim_end().to_owned()
}

#[track_caller]
fn assert_screen(actual: &str, expected: &str) {
    let expected = expected.strip_prefix('\n').unwrap_or(expected).trim_end();
    assert!(
        actual == expected,
        "screen differs\n--- actual ---\n{actual}\n--- expected ---\n{expected}"
    );
}

fn ui() -> ToolsUi {
    ToolsUi::new(None, None, None)
}

fn press(ui: &mut ToolsUi, fake: &Fake, keys: &[KeyEvent]) -> Vec<ToolsIntent> {
    keys.iter().map(|k| ui.on_key(*k, fake)).collect()
}

#[test]
fn menu_snapshot_and_navigation() {
    let fake = Fake::new();
    let mut ui = ui();
    assert_screen(
        &screen(&ui),
        r"
Tools

 1 Check Database   Repair and optimize; backs up first, clears undo
 2 Check Media      Unused and missing files, media trash; backs up first
 3 Empty Cards      Find cards whose front renders empty, then delete them
 4 Preferences      Scheduling, reviewing, editing and backups
 5 Profiles         List, create, rename and delete profiles
",
    );
    press(&mut ui, &fake, &[ch('j'), ch('j'), ch('k')]);
    assert_eq!(ui.menu, 1);
    assert_eq!(ui.on_key(key(KeyCode::Esc), &fake), ToolsIntent::Close);
    assert!(fake.log().is_empty(), "opening the menu touches nothing");
}

#[test]
fn check_database_explains_then_runs_with_progress() {
    let fake = Fake::new();
    let mut ui = ui();
    ui.on_key(ch('1'), &fake);
    assert_screen(
        &screen(&ui),
        r"
Check Database

Check Database will first write a backup of the collection. It then repairs
problems it finds, rebuilds and optimizes the file, and clears the undo
history.

Some repairs force a one-way full sync (upload or download) on the next sync.

Press y to run it, Esc to go back.
",
    );
    assert!(fake.log().is_empty(), "nothing runs before y");
    assert_eq!(ui.hints()[0], ("y", "run"));

    fake.hold.set(true);
    fake.queue(Ok(Resp::DatabaseChecked(DatabaseReport {
        problems: vec!["Fixed 2 cards with invalid properties.".into()],
        backup: Some("backup-2026-11-15-10.00.00.colpkg".into()),
    })));
    ui.on_key(ch('y'), &fake);
    assert_eq!(fake.log(), [Cmd::CheckDatabase { confirmed: true }]);
    assert!(ui.is_busy());
    *fake.progress.borrow_mut() = OperationProgress {
        stage: "Checking notes".into(),
        current: Some(3),
        total: Some(10),
    };
    assert_eq!(ui.tick(&fake), ToolsIntent::None);
    assert_screen(&screen(&ui), "Check Database\n\nChecking notes 3/10");
    assert_eq!(ui.hints(), [("Esc", "cancel")]);

    fake.hold.set(false);
    assert_eq!(
        ui.tick(&fake),
        ToolsIntent::Changed(OpChanges::everything())
    );
    assert!(!ui.is_busy());
    assert_screen(
        &screen(&ui),
        r"
Check Database

Backup written: backup-2026-11-15-10.00.00.colpkg
Problems fixed:
  Fixed 2 cards with invalid properties.
",
    );
    assert_eq!(ui.on_key(key(KeyCode::Esc), &fake), ToolsIntent::None);
    assert!(screen(&ui).starts_with("Tools"));
}

#[test]
fn escape_cancels_a_running_check_and_failures_are_shown() {
    let fake = Fake::new();
    let mut ui = ui();
    ui.on_key(ch('1'), &fake);
    fake.hold.set(true);
    fake.queue(Err(CoreError::Interrupted));
    ui.on_key(ch('y'), &fake);
    ui.on_key(key(KeyCode::Esc), &fake);
    assert_eq!(fake.cancels.get(), 1);
    assert!(ui.is_busy(), "the operation ends when the actor answers");
    fake.hold.set(false);
    ui.tick(&fake);
    assert!(screen(&ui).contains("Failed: Cancelled"));

    // A backup failure stops the check and says so.
    let mut ui = self::ui();
    ui.on_key(ch('1'), &fake);
    fake.queue(Err(CoreError::Other("Backup failed: disk full".into())));
    ui.on_key(ch('y'), &fake);
    assert_eq!(ui.tick(&fake), ToolsIntent::None);
    assert!(screen(&ui).contains("Failed: Backup failed: disk full"));
}

#[test]
fn check_media_says_what_changes_and_lists_unused_and_missing() {
    let fake = Fake::new();
    let mut ui = ui();
    ui.on_key(ch('2'), &fake);
    let text = screen(&ui);
    assert!(text.contains("will first write a backup"), "{text}");
    assert!(text.contains("rewrite the"), "{text}");
    assert!(fake.log().is_empty());
    fake.queue(Ok(Resp::MediaChecked(media_report())));
    ui.on_key(ch('y'), &fake);
    assert_eq!(fake.log(), [Cmd::CheckMedia]);
    assert!(matches!(ui.tick(&fake), ToolsIntent::Changed(_)));
    assert_screen(
        &screen(&ui),
        r"
Check Media

Backup written: backup-2026-11-15-10.00.00.colpkg
Missing files: 1
Unused files: 2
Trash: empty
Unused (2):
  old.png
  stray.mp3
Missing (1):
  gone.jpg
",
    );
}

#[test]
fn media_trash_actions_ask_first() {
    let fake = Fake::new();
    let mut ui = ui();
    ui.on_key(ch('2'), &fake);
    fake.queue(Ok(Resp::MediaChecked(media_report())));
    ui.on_key(ch('y'), &fake);
    ui.tick(&fake);
    fake.log.borrow_mut().clear();

    // Declining does nothing.
    press(&mut ui, &fake, &[ch('t'), ch('n')]);
    assert!(fake.log().is_empty());
    press(&mut ui, &fake, &[ch('t')]);
    assert!(screen(&ui).contains("Move 2 unused file(s) to the media trash (restorable)? y/n"));
    press(&mut ui, &fake, &[ch('y')]);
    assert_eq!(
        fake.log(),
        [Cmd::TrashMedia {
            files: vec!["old.png".into(), "stray.mp3".into()]
        }]
    );
    let text = screen(&ui);
    assert!(
        text.contains("Moved 2 unused file(s) to the media trash."),
        "{text}"
    );
    assert!(
        text.contains("Unused (0):") && text.contains("Trash: has files"),
        "{text}"
    );

    // Emptying the trash is permanent: asks, and states it.
    fake.log.borrow_mut().clear();
    press(&mut ui, &fake, &[ch('e')]);
    assert!(screen(&ui).contains("Permanently delete every file in the media trash?"));
    press(&mut ui, &fake, &[ch('n')]);
    assert!(fake.log().is_empty());
    press(&mut ui, &fake, &[ch('e'), ch('y')]);
    assert_eq!(fake.log(), [Cmd::EmptyMediaTrash]);
    assert!(screen(&ui).contains("Trash: empty"));
    press(&mut ui, &fake, &[ch('r')]);
    assert_eq!(fake.log().last(), Some(&Cmd::RestoreMediaTrash));
}

#[test]
fn empty_cards_report_keep_notes_and_delete() {
    let fake = Fake::new();
    let mut ui = ui();
    fake.queue(Ok(Resp::EmptyCards(empty_report())));
    assert_eq!(ui.on_key(ch('3'), &fake), ToolsIntent::None);
    assert_eq!(fake.log(), [Cmd::FindEmptyCards], "reading needs no prompt");
    ui.tick(&fake);
    assert_screen(
        &screen(&ui),
        r"
Empty Cards

Basic (and reversed card)
  note 1: 1 empty card

2 card(s) would be deleted. Keep notes with no valid cards: yes (k)
",
    );
    press(&mut ui, &fake, &[ch('k')]);
    assert!(screen(&ui).contains("3 card(s) would be deleted. Keep notes with no valid cards: no"));
    press(&mut ui, &fake, &[ch('k'), ch('d')]);
    assert!(screen(&ui).contains("Delete 2 empty card(s)? y/n"));
    let intents = press(&mut ui, &fake, &[ch('y')]);
    assert_eq!(intents, [ToolsIntent::Changed(changes())]);
    assert_eq!(
        fake.log().last(),
        Some(&Cmd::DeleteEmptyCards { keep_notes: true })
    );
    let text = screen(&ui);
    assert!(
        text.contains("No empty cards.") && text.contains("Deleted 2 empty card(s)."),
        "{text}"
    );
}

#[test]
fn empty_cards_with_nothing_to_delete_offers_no_delete() {
    let fake = Fake::new();
    let mut ui = ui();
    fake.queue(Ok(Resp::EmptyCards(EmptyCardsReport {
        report: String::new(),
        notes: vec![],
    })));
    ui.on_key(ch('3'), &fake);
    ui.tick(&fake);
    press(&mut ui, &fake, &[ch('d'), ch('y')]);
    assert_eq!(fake.log(), [Cmd::FindEmptyCards]);
}

fn open_prefs(fake: &Fake) -> ToolsUi {
    let mut ui = ui();
    assert_eq!(ui.on_key(ch('4'), fake), ToolsIntent::None);
    ui
}

#[test]
fn preferences_form_snapshot_edit_and_save() {
    let fake = Fake::new();
    let mut ui = open_prefs(&fake);
    assert_screen(
        &screen(&ui),
        r"
Preferences

Scheduling
  Next day starts at (hour)                    4
  Learn ahead limit (seconds)                  1200
  New/review order                             Mix new cards and reviews
  Show day-learning cards first (v2)           [ ]
  New timezone support (v2)                    [ ]
Reviewing
  Show remaining card count                    [x]
  Show next review time above buttons          [ ]
  Hide audio play buttons                      [ ]
",
    );
    // Number: edit, bad value is refused with a message, good value applies.
    ui.on_key(key(KeyCode::Enter), &fake);
    press(
        &mut ui,
        &fake,
        &[key(KeyCode::Backspace), ch('x'), key(KeyCode::Enter)],
    );
    assert!(screen(&ui).contains("Enter a whole number from 0 to 23."));
    ui.on_key(key(KeyCode::Enter), &fake);
    press(
        &mut ui,
        &fake,
        &[
            key(KeyCode::Backspace),
            key(KeyCode::Backspace),
            ch('6'),
            key(KeyCode::Enter),
        ],
    );
    assert!(screen(&ui).contains("Next day starts at (hour)                    6"));
    assert!(screen(&ui).contains("[modified]"));
    // Choice and bool.
    press(&mut ui, &fake, &[ch('j'), ch('j'), key(KeyCode::Right)]);
    assert!(screen(&ui).contains("Show after reviews"));
    press(&mut ui, &fake, &[ch('j'), ch(' ')]);
    assert!(screen(&ui).contains("Show day-learning cards first (v2)           [x]"));
    assert!(!fake
        .log()
        .iter()
        .any(|c| matches!(c, Cmd::SetPreferences(_))));

    let intents = press(&mut ui, &fake, &[ch('s')]);
    assert!(matches!(&intents[0], ToolsIntent::Changed(c) if c.config));
    let Some(Cmd::SetPreferences(saved)) = fake.log().last().cloned() else {
        panic!("not saved")
    };
    assert_eq!(saved.rollover, 6);
    assert_eq!(saved.new_review_mix, 1);
    assert!(saved.day_learn_first);
    assert_eq!(saved.learn_ahead_secs, 1200, "untouched fields are kept");
    assert!(screen(&ui).contains("Preferences saved."));
    assert!(!screen(&ui).contains("[modified]"));
}

#[test]
fn preferences_text_field_and_anki_errors_and_discard() {
    let fake = Fake::new();
    let mut ui = open_prefs(&fake);
    // Headings are skipped: "Default search text" is the 16th selectable row.
    for _ in 0..15 {
        ui.on_key(ch('j'), &fake);
    }
    let text = screen(&ui);
    assert!(text.contains("Default search text"), "{text}");
    ui.on_key(key(KeyCode::Enter), &fake);
    ui.paste("deck:current");
    ui.on_key(key(KeyCode::Enter), &fake);
    assert!(screen(&ui).contains("deck:current"));
    // Anki's refusal is shown and the form stays open and dirty.
    *fake.fail.borrow_mut() = Some(CoreError::Invalid("Invalid rollover".into()));
    // `fail` applies to the next executed command, the save.
    let intents = press(&mut ui, &fake, &[ch('s')]);
    assert_eq!(intents, [ToolsIntent::None]);
    let text = screen(&ui);
    assert!(
        text.contains("Invalid rollover") && text.contains("[modified]"),
        "{text}"
    );
    // Esc with unsaved changes asks; any other key keeps editing.
    ui.on_key(key(KeyCode::Esc), &fake);
    assert!(screen(&ui).contains("Discard unsaved changes? y/n"));
    ui.on_key(ch('n'), &fake);
    assert!(screen(&ui).starts_with("Preferences"));
    ui.on_key(key(KeyCode::Esc), &fake);
    ui.on_key(ch('y'), &fake);
    assert!(screen(&ui).starts_with("Tools"));
}

fn base_with(names: &[&str]) -> tempfile::TempDir {
    let base = tempfile::tempdir().unwrap();
    for n in names {
        let dir = base.path().join(n);
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("collection.anki2"), vec![0u8; 2048]).unwrap();
    }
    base
}

fn open_profiles(base: &std::path::Path, open: &str) -> ToolsUi {
    let fake = Fake::new();
    let mut ui = ToolsUi::new(Some(base.to_path_buf()), Some(open.into()), None);
    ui.on_key(ch('5'), &fake);
    ui
}

#[test]
fn profiles_list_and_switch_intent() {
    let base = base_with(&["User 1", "Other"]);
    std::fs::write(base.path().join("prefs21.db"), b"x").unwrap();
    std::fs::create_dir(base.path().join("addons21")).unwrap();
    let fake = Fake::new();
    let mut ui = open_profiles(base.path(), "User 1");
    let text = screen(&ui);
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines[0], "Profiles");
    assert_eq!(lines[3], "   Other                            2.0 KiB");
    assert_eq!(lines[4], " * User 1                           2.0 KiB");
    assert_eq!(lines.len(), 5, "prefs21.db and addons21 are not profiles");
    // Switching to the open one is a no-op; to another asks the app.
    ui.on_key(ch('j'), &fake);
    ui.on_key(key(KeyCode::Enter), &fake);
    assert!(screen(&ui).contains("Already open."));
    ui.on_key(ch('k'), &fake);
    assert_eq!(
        ui.on_key(key(KeyCode::Enter), &fake),
        ToolsIntent::SwitchProfile("Other".into())
    );
}

#[test]
fn profiles_rename_delete_and_refuse_the_open_one() {
    let base = base_with(&["User 1", "Other"]);
    let b = base.path();
    let fake = Fake::new();
    let mut ui = open_profiles(b, "User 1");
    // Selected: "Other" (sorted first).
    ui.on_key(ch('r'), &fake);
    for _ in 0..5 {
        ui.on_key(key(KeyCode::Backspace), &fake);
    }
    ui.paste("Renamed");
    ui.on_key(key(KeyCode::Enter), &fake);
    assert!(b.join("Renamed/collection.anki2").exists() && !b.join("Other").exists());
    assert!(screen(&ui).contains("Renamed Other to Renamed."));

    // The open profile can be neither renamed nor deleted.
    ui.on_key(ch('j'), &fake);
    ui.on_key(ch('d'), &fake);
    assert!(screen(&ui).contains("open and cannot be deleted"));
    ui.on_key(ch('r'), &fake);
    assert!(screen(&ui).contains("is open; switch to another one"));
    assert!(b.join("User 1/collection.anki2").exists());

    // Delete needs the typed name.
    ui.on_key(ch('k'), &fake);
    ui.on_key(ch('d'), &fake);
    assert!(screen(&ui).contains("Type the name to confirm"));
    ui.paste("Rename");
    ui.on_key(key(KeyCode::Enter), &fake);
    assert!(screen(&ui).contains("type its name"), "{}", screen(&ui));
    assert!(b.join("Renamed").exists());
    ui.on_key(ch('d'), &fake);
    ui.paste("Renamed");
    ui.on_key(key(KeyCode::Enter), &fake);
    assert!(!b.join("Renamed").exists());
    assert!(screen(&ui).contains("Deleted Renamed"));
    assert_eq!(
        std::fs::read_dir(b.join(".tanki-trash")).unwrap().count(),
        1
    );
}

#[test]
fn profiles_without_a_base_say_so() {
    let fake = Fake::new();
    let mut ui = ui();
    ui.on_key(ch('5'), &fake);
    assert!(screen(&ui).contains("No Anki base folder is known."));
}
