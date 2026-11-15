//! Cell-buffer snapshots and key handling with a fake backend.
use super::*;
use crate::testing::buffer_text;
use std::cell::RefCell;
use std::collections::VecDeque;
use tanki_core::deck_options::form;
use tanki_core::deck_options::{
    AdvancedToggles, DeckLimits, GlobalOptions, Optimized, Preset, PresetConfig, SaveRequest,
    Simulation,
};
use tanki_core::OpChanges;

/// Anki's default preset values (`rslib/src/deckconfig/mod.rs`).
fn defaults() -> PresetConfig {
    PresetConfig {
        learn_steps: vec![1.0, 10.0],
        relearn_steps: vec![10.0],
        new_per_day: 20,
        reviews_per_day: 200,
        initial_ease: 2.5,
        easy_multiplier: 1.3,
        hard_multiplier: 1.2,
        lapse_multiplier: 0.0,
        interval_multiplier: 1.0,
        maximum_review_interval: 36500,
        minimum_lapse_interval: 1,
        graduating_interval_good: 1,
        graduating_interval_easy: 4,
        leech_action: 1,
        leech_threshold: 8,
        cap_answer_time_to_secs: 60,
        wait_for_audio: true,
        desired_retention: 0.9,
        easy_days_percentages: vec![1.0; 7],
        historical_retention: 0.9,
        ..Default::default()
    }
}

fn data() -> DeckOptionsData {
    let mut heavy = defaults();
    heavy.new_per_day = 50;
    heavy.learn_steps = vec![1.0, 10.0, 60.0];
    DeckOptionsData {
        deck_id: 7,
        deck_name: "Languages::French".into(),
        deck_preset_id: 1,
        parent_preset_ids: vec![],
        presets: vec![
            Preset {
                id: 1,
                name: "Default".into(),
                config: defaults(),
                use_count: 3,
                ..Default::default()
            },
            Preset {
                id: 5,
                name: "Heavy".into(),
                config: heavy,
                use_count: 1,
                ..Default::default()
            },
        ],
        defaults: defaults(),
        limits: DeckLimits::default(),
        globals: GlobalOptions::default(),
        advanced: AdvancedToggles {
            load_balancer: true,
            fsrs_short_term_with_steps: false,
        },
        fsrs_legacy_evaluate: false,
        schema_modified: false,
        days_since_last_fsrs_optimize: 0,
        custom_scheduling_warning: None,
        form: form::english(),
    }
}

struct Op(RefCell<Option<Result<Response>>>, Rc<Cell<bool>>);

impl CommandOp for Op {
    fn poll(&self) -> Result<Option<Response>> {
        if self.1.get() {
            return Ok(None);
        }
        Ok(Some(self.0.borrow_mut().take().expect("polled twice")?))
    }
}

use std::rc::Rc;

/// Answers `Load` with [`data`], and each started command with the next
/// queued result (default: `Saved`).
struct Fake {
    data: DeckOptionsData,
    started: RefCell<Vec<DeckOptionsCommand>>,
    results: RefCell<VecDeque<Result<DeckOptionsResponse>>>,
    hold: Rc<Cell<bool>>,
    progress: RefCell<OperationProgress>,
    cancels: Cell<u32>,
}

impl Fake {
    fn new(data: DeckOptionsData) -> Self {
        Self {
            data,
            started: RefCell::default(),
            results: RefCell::default(),
            hold: Rc::new(Cell::new(false)),
            progress: RefCell::default(),
            cancels: Cell::new(0),
        }
    }
    fn push(&self, result: Result<DeckOptionsResponse>) {
        self.results.borrow_mut().push_back(result);
    }
    fn last_save(&self) -> (SaveRequest, bool) {
        match self.started.borrow().last() {
            Some(DeckOptionsCommand::Save { request, confirmed }) => {
                ((**request).clone(), *confirmed)
            }
            other => panic!("not a save: {other:?}"),
        }
    }
}

impl Backend for Fake {
    fn execute(&self, command: Command) -> Result<Response> {
        match command {
            Command::Manage(ManageCommand::DeckOptions(DeckOptionsCommand::Load { .. })) => {
                Ok(Response::Manage(ManageResponse::DeckOptions(
                    DeckOptionsResponse::Loaded(Box::new(self.data.clone())),
                )))
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    fn start(&self, command: Command) -> Result<Box<dyn CommandOp>> {
        let Command::Manage(ManageCommand::DeckOptions(command)) = command else {
            panic!("unexpected {command:?}")
        };
        self.started.borrow_mut().push(command);
        let result = self
            .results
            .borrow_mut()
            .pop_front()
            .unwrap_or(Ok(DeckOptionsResponse::Saved(OpChanges::default())));
        Ok(Box::new(Op(
            RefCell::new(Some(
                result.map(|r| Response::Manage(ManageResponse::DeckOptions(r))),
            )),
            self.hold.clone(),
        )))
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

fn press(ui: &mut DeckOptionsUi, fake: &Fake, keys: &[KeyEvent]) -> DeckOptionsIntent {
    let mut last = DeckOptionsIntent::None;
    for k in keys {
        last = ui.on_key(*k, fake);
    }
    last
}

fn draw(ui: &DeckOptionsUi, width: u16, height: u16) -> Vec<String> {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    ui.render(area, &mut buf);
    buffer_text(&buf)
}

fn open(fake: &Fake) -> DeckOptionsUi {
    DeckOptionsUi::open(fake, 7).unwrap()
}

fn status(ui: &DeckOptionsUi) -> String {
    draw(ui, 100, 24).last().cloned().unwrap_or_default()
}

#[test]
fn draws_the_form() {
    let fake = Fake::new(data());
    let ui = open(&fake);
    assert_eq!(
        draw(&ui, 100, 24),
        [
            "Deck Options — Languages::French",
            "Preset: Default (used by 3 decks)   p presets  [ ] switch",
            "Daily Limits",
            "› New cards/day                                  20  [Preset] This deck  Today only",
            "  Maximum reviews/day                            200  [Preset] This deck  Today only",
            "  New cards ignore review limit (global)         [ ]",
            "  Limits start from top (global)                 [ ]",
            "",
            "New Cards",
            "  Learning steps                                 1m 10m",
            "  Graduating interval                            1",
            "  Easy interval                                  4",
            "  Insertion order                                ‹ Sequential (oldest cards first) ›",
            "",
            "Lapses",
            "  Relearning steps                               10m",
            "  Minimum interval                               1",
            "  Leech threshold                                8                                                 ↓",
            "────────────────────────────────────────────────────────────────────────────────────────────────────",
            "The maximum number of new cards to introduce in a day, if new cards are available. Because new",
            "material will increase your short-term review workload, this should typically be at least 10x",
            "smaller than your review limit.",
            "When studying a deck that has subdecks inside it, the limits set on each subdeck control the maximum",
            "",
        ]
    );
}

#[test]
fn edits_values_tabs_and_saves() {
    let fake = Fake::new(data());
    let mut ui = open(&fake);
    assert!(!ui.is_dirty());
    press(&mut ui, &fake, &[key(KeyCode::Right), key(KeyCode::Right)]);
    assert_eq!(ui.state().current().config.new_per_day, 22);
    // "This deck" tab, then type a value.
    press(&mut ui, &fake, &[ch('t'), key(KeyCode::Enter)]);
    for _ in 0..3 {
        press(&mut ui, &fake, &[key(KeyCode::Backspace)]);
    }
    press(&mut ui, &fake, &[ch('1'), ch('5'), key(KeyCode::Enter)]);
    assert_eq!(ui.state().limits().new, Some(15));
    let text = draw(&ui, 100, 24);
    assert_eq!(text[0], "Deck Options — Languages::French  [modified]");
    assert_eq!(
        text[3],
        "› New cards/day                                  15   Preset [This deck] Today only   ↺"
    );
    // A bad number keeps the editor open with the error.
    press(
        &mut ui,
        &fake,
        &[ch('j'), key(KeyCode::Enter), ch('x'), key(KeyCode::Enter)],
    );
    let text = draw(&ui, 100, 24);
    assert!(
        text.iter().any(|l| l.contains("Not a number: 200x")),
        "{text:#?}"
    );
    press(&mut ui, &fake, &[key(KeyCode::Esc)]);
    // Steps.
    ui.focus(FieldId::LearnSteps);
    press(&mut ui, &fake, &[key(KeyCode::Enter)]);
    for c in " 2d".chars() {
        press(&mut ui, &fake, &[ch(c)]);
    }
    press(&mut ui, &fake, &[key(KeyCode::Enter)]);
    assert_eq!(
        ui.state().current().config.learn_steps,
        vec![1.0, 10.0, 2880.0]
    );
    let text = draw(&ui, 100, 24);
    assert!(
        text.iter()
            .any(|l| l.contains("⚠ The graduating interval should be at least as long")),
        "{text:#?}"
    );
    // Reset to the default.
    press(&mut ui, &fake, &[ch('r')]);
    assert_eq!(ui.state().current().config.learn_steps, vec![1.0, 10.0]);

    assert_eq!(press(&mut ui, &fake, &[ch('s')]), DeckOptionsIntent::None);
    assert!(ui.is_busy());
    let (request, confirmed) = fake.last_save();
    assert!(!confirmed);
    assert_eq!(request.mode, SaveMode::Normal);
    assert_eq!(request.limits.new, Some(15));
    assert_eq!(request.configs.last().unwrap().config.new_per_day, 22);
    assert_eq!(
        ui.tick(&fake),
        DeckOptionsIntent::Saved("Deck options saved.".into())
    );
    assert!(!ui.is_busy());
}

#[test]
fn closing_with_changes_asks() {
    let fake = Fake::new(data());
    let mut ui = open(&fake);
    assert_eq!(press(&mut ui, &fake, &[ch('q')]), DeckOptionsIntent::Close);
    press(&mut ui, &fake, &[ch(' ')]);
    assert_eq!(
        press(&mut ui, &fake, &[key(KeyCode::Esc)]),
        DeckOptionsIntent::None
    );
    let text = draw(&ui, 100, 24);
    assert!(text
        .iter()
        .any(|l| l.contains("Save your changes to the deck options?")));
    assert_eq!(ui.hints()[1], ("d", "discard"));
    // Esc keeps editing; then discard.
    press(&mut ui, &fake, &[key(KeyCode::Esc)]);
    assert!(ui.is_dirty());
    assert_eq!(
        press(&mut ui, &fake, &[key(KeyCode::Esc), ch('d')]),
        DeckOptionsIntent::Close
    );
    // Or save from the prompt.
    let mut ui = open(&fake);
    press(&mut ui, &fake, &[ch(' '), key(KeyCode::Esc), ch('s')]);
    assert!(ui.is_busy());
    assert!(matches!(ui.tick(&fake), DeckOptionsIntent::Saved(_)));
}

#[test]
fn preset_dialog_add_switch_rename_remove() {
    let fake = Fake::new(data());
    let mut ui = open(&fake);
    press(&mut ui, &fake, &[ch('p')]);
    let text = draw(&ui, 100, 24);
    println!("{}", text.join("\n"));
    assert!(
        text.iter()
            .any(|l| l.contains("› ● Default (used by 3 decks)")),
        "{text:#?}"
    );
    assert!(
        text.iter()
            .any(|l| l.contains("    Heavy (used by 1 deck)")),
        "{text:#?}"
    );
    // Add "Extra" (Anki's defaults), made current.
    press(&mut ui, &fake, &[ch('a')]);
    for c in "Extra".chars() {
        press(&mut ui, &fake, &[ch(c)]);
    }
    press(&mut ui, &fake, &[key(KeyCode::Enter)]);
    assert_eq!(ui.state().current().name, "Extra");
    assert_eq!(ui.state().current().id, 0);
    assert!(draw(&ui, 100, 24)[1].starts_with("Preset: Extra (used by 1 deck)"));
    // Removing an unsaved preset needs no full sync.
    press(&mut ui, &fake, &[ch('p'), ch('d')]);
    let text = draw(&ui, 100, 24);
    assert!(text.iter().any(|l| l.contains("Remove Extra?")));
    assert!(!text.iter().any(|l| l.contains("one-way sync")));
    press(&mut ui, &fake, &[ch('y')]);
    assert!(ui.state().presets().iter().all(|p| p.name != "Extra"));
    // The default preset cannot be removed.
    press(
        &mut ui,
        &fake,
        &[ch('p'), key(KeyCode::Up), key(KeyCode::Up), ch('d')],
    );
    assert_eq!(status(&ui), "The default configuration can't be removed.");
    // Switch to Heavy with ], rename it, then remove it: a full sync.
    press(&mut ui, &fake, &[key(KeyCode::Esc), ch(']')]);
    assert_eq!(ui.state().current().name, "Heavy");
    press(
        &mut ui,
        &fake,
        &[ch('p'), ch('r'), ch('!'), key(KeyCode::Enter)],
    );
    assert_eq!(ui.state().current().name, "Heavy!");
    press(&mut ui, &fake, &[ch('p'), ch('d')]);
    let text = draw(&ui, 100, 24);
    assert!(text.iter().any(|l| l.contains("one-way sync")), "{text:#?}");
    press(&mut ui, &fake, &[ch('y')]);
    assert_eq!(ui.state().removed(), [5]);
    press(&mut ui, &fake, &[ch('s')]);
    let (request, confirmed) = fake.last_save();
    assert!(confirmed, "the removal was confirmed");
    assert_eq!(request.removed_preset_ids, [5]);
    assert_eq!(request.configs.last().unwrap().id, 1);
}

#[test]
fn unconfirmed_full_sync_asks_and_resends() {
    let fake = Fake::new(data());
    let mut ui = open(&fake);
    fake.push(Err(CoreError::SchemaChangeRequired));
    press(&mut ui, &fake, &[ch(' '), ch('s')]);
    assert_eq!(ui.tick(&fake), DeckOptionsIntent::None);
    let text = draw(&ui, 100, 24);
    assert!(text.iter().any(|l| l.contains("one-way sync")), "{text:#?}");
    press(&mut ui, &fake, &[ch('y')]);
    assert!(fake.last_save().1);
    assert!(matches!(ui.tick(&fake), DeckOptionsIntent::Saved(_)));
}

fn fsrs_data() -> DeckOptionsData {
    let mut data = data();
    data.globals.fsrs = true;
    data
}

#[test]
fn fsrs_rows_optimize_with_progress_and_cancel() {
    let fake = Fake::new(data());
    let mut ui = open(&fake);
    // FSRS off: no FSRS rows, the tools refuse.
    press(&mut ui, &fake, &[ch('o')]);
    assert_eq!(status(&ui), "FSRS must be enabled first.");
    ui.focus(FieldId::Fsrs);
    press(&mut ui, &fake, &[key(KeyCode::Enter)]);
    assert!(ui.state().fsrs());
    ui.focus(FieldId::DesiredRetention);
    let text = draw(&ui, 100, 40);
    println!("{}", text.join("\n"));
    assert!(
        text.iter()
            .any(|l| l.contains("› Desired retention") && l.contains("90%  [Preset] This deck")),
        "{text:#?}"
    );
    assert!(
        text.iter()
            .any(|l| l.contains("▶ Optimize Current Preset  o")),
        "{text:#?}"
    );
    assert!(!text.iter().any(|l| l.contains("Graduating interval")));

    let fake = Fake::new(fsrs_data());
    let mut ui = open(&fake);
    fake.hold.set(true);
    *fake.progress.borrow_mut() = OperationProgress {
        stage: "Optimizing".into(),
        current: Some(3),
        total: Some(12),
    };
    fake.push(Ok(DeckOptionsResponse::Optimized(Optimized {
        params: vec![0.25, 1.5],
        fsrs_items: 420,
        already_optimal: false,
        ..Default::default()
    })));
    press(&mut ui, &fake, &[ch('o')]);
    assert!(matches!(
        fake.started.borrow().last(),
        Some(DeckOptionsCommand::Optimize { preset_name, health_check: false, .. }) if preset_name == "Default"
    ));
    assert_eq!(ui.tick(&fake), DeckOptionsIntent::None);
    assert_eq!(status(&ui), "Optimizing… 3/12 (25%)  (Esc cancels)");
    assert_eq!(ui.hints(), vec![("Esc", "cancel")]);
    press(&mut ui, &fake, &[key(KeyCode::Esc)]);
    assert_eq!(fake.cancels.get(), 1);
    ui.tick(&fake);
    assert_eq!(status(&ui), "Optimizing… 3/12 (25%) — cancelling");
    // The result arrives (the cancel came too late).
    fake.hold.set(false);
    ui.tick(&fake);
    assert!(!ui.is_busy());
    assert_eq!(ui.state().current().config.fsrs_params_6, vec![0.25, 1.5]);
    let text = draw(&ui, 100, 24);
    assert!(
        text.iter()
            .any(|l| l.contains("FSRS parameters: 0.25, 1.5 (420 reviews). Save to keep them.")),
        "{text:#?}"
    );
    press(&mut ui, &fake, &[key(KeyCode::Enter)]);

    // An interrupted run says so.
    fake.push(Err(CoreError::Interrupted));
    press(&mut ui, &fake, &[ch('s'), ch('e')]);
    ui.tick(&fake);
    // Saving refuses nothing here; evaluate needs a saved assignment only.
    assert!(matches!(
        fake.started.borrow().last(),
        Some(DeckOptionsCommand::Save { .. })
    ));
}

#[test]
fn evaluate_and_insufficient_history_messages() {
    let fake = Fake::new(fsrs_data());
    let mut ui = open(&fake);
    fake.push(Ok(DeckOptionsResponse::Evaluated {
        log_loss: 0.3,
        rmse_bins: 0.05,
        message: "Log loss: 0.3000, RMSE(bins): 5.00%. Smaller numbers indicate a better fit to your review history.".into(),
    }));
    press(&mut ui, &fake, &[ch('e')]);
    ui.tick(&fake);
    assert!(draw(&ui, 100, 24)
        .iter()
        .any(|l| l.contains("Log loss: 0.3000, RMSE(bins): 5.00%.")));
    press(&mut ui, &fake, &[key(KeyCode::Enter)]);
    fake.push(Err(CoreError::Invalid(
        "Insufficient review history to perform this operation.".into(),
    )));
    press(&mut ui, &fake, &[ch('o')]);
    ui.tick(&fake);
    assert!(draw(&ui, 100, 24)
        .iter()
        .any(|l| l.contains("Insufficient review history")));
    press(&mut ui, &fake, &[key(KeyCode::Esc)]);
    fake.push(Err(CoreError::Interrupted));
    press(&mut ui, &fake, &[ch('o')]);
    ui.tick(&fake);
    assert_eq!(status(&ui), "Cancelled.");
    // After switching presets, optimising asks to save first.
    press(&mut ui, &fake, &[ch(']'), ch('o')]);
    assert_eq!(status(&ui), "Please save your changes first.");
}

#[test]
fn simulator_chart_and_minimum_retention() {
    let fake = Fake::new(fsrs_data());
    let mut ui = open(&fake);
    press(&mut ui, &fake, &[ch('v')]);
    let text = draw(&ui, 100, 30);
    assert!(
        text.iter()
            .any(|l| l.contains("FSRS Simulator (Experimental)")),
        "{text:#?}"
    );
    assert!(
        text.iter()
            .any(|l| l.contains("› Days to simulate") && l.contains("‹ 365 ›")),
        "{text:#?}"
    );
    // Fewer days, then simulate.
    press(&mut ui, &fake, &[key(KeyCode::Left)]);
    let days: Vec<u32> = (0..335).collect();
    fake.push(Ok(DeckOptionsResponse::Simulated(Simulation {
        accumulated_knowledge_acquisition: days.iter().map(|d| *d as f32 * 2.0).collect(),
        daily_review_count: days.iter().map(|d| 50 + d % 7).collect(),
        daily_new_count: vec![20; 335],
        daily_time_cost: vec![600.0; 335],
    })));
    press(&mut ui, &fake, &[key(KeyCode::Enter)]);
    match fake.started.borrow().last() {
        Some(DeckOptionsCommand::Simulate(request)) => {
            assert_eq!(request.days_to_simulate, 335);
            assert_eq!(request.search, "preset:\"Default\" -is:suspended");
            assert_eq!(
                request.suspend_after_lapse_count, None,
                "Default tags leeches"
            );
            assert_eq!(request.new_limit, 20);
        }
        other => panic!("{other:?}"),
    }
    ui.tick(&fake);
    let text = draw(&ui, 100, 30);
    println!("{}", text.join("\n"));
    let braille = text
        .iter()
        .flat_map(|l| l.chars())
        .filter(|c| ('\u{2801}'..='\u{28ff}').contains(c))
        .count();
    assert!(braille > 30, "{text:#?}");
    assert!(
        text.iter().any(|l| l.contains("[Review Count/Day]")),
        "{text:#?}"
    );
    // Other chart.
    press(&mut ui, &fake, &[key(KeyCode::Tab)]);
    assert!(draw(&ui, 100, 30)
        .iter()
        .any(|l| l.contains("[Review Time/Day]")));
    fake.push(Ok(DeckOptionsResponse::MinimumRetention {
        retention: 0.83,
        message: "Minimum recommended retention: 0.83".into(),
    }));
    press(&mut ui, &fake, &[ch('m')]);
    ui.tick(&fake);
    assert!(draw(&ui, 100, 30)
        .iter()
        .any(|l| l.contains("Minimum recommended retention: 0.83")));
    // Esc leaves the simulator, the form stays.
    assert_eq!(
        press(&mut ui, &fake, &[key(KeyCode::Esc)]),
        DeckOptionsIntent::None
    );
    assert!(!draw(&ui, 100, 30)
        .iter()
        .any(|l| l.contains("Days to simulate")));
    // Unsaved parameters: desktop asks to save first.
    ui.focus(FieldId::FsrsParams);
    press(
        &mut ui,
        &fake,
        &[key(KeyCode::Enter), ch('1'), key(KeyCode::Enter), ch('v')],
    );
    assert_eq!(
        status(&ui),
        "After optimizing, please save your deck preset before running the simulator."
    );
}

#[test]
fn custom_scheduling_warning_and_editor() {
    let mut data = data();
    data.globals.card_state_customizer = "states.good = states.easy;".into();
    data.custom_scheduling_warning = Some(form::CUSTOM_SCHEDULING_WARNING.into());
    let fake = Fake::new(data);
    let mut ui = open(&fake);
    let text = draw(&ui, 100, 24);
    assert!(
        text[2].starts_with("⚠ This collection has a custom scheduling script."),
        "{text:#?}"
    );
    ui.focus(FieldId::CustomScheduling);
    assert_eq!(
        press(&mut ui, &fake, &[key(KeyCode::Enter)]),
        DeckOptionsIntent::EditScript("states.good = states.easy;".into())
    );
    ui.set_custom_scheduling(String::new());
    assert!(ui.is_dirty());
    press(&mut ui, &fake, &[ch('s')]);
    assert_eq!(fake.last_save().0.globals.card_state_customizer, "");
}
