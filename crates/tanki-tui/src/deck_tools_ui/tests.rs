//! Cell-buffer snapshots and key handling of the four widgets, against a
//! fake `Backend` that answers the deck tools commands.
use super::*;
use crate::services::{Display, PlainTextRenderer};
use crate::testing::buffer_text;
use std::cell::RefCell;
use std::path::Path;
use tanki_core::deck_tools::{
    CramKind, CustomStudy, CustomStudyDefaults, CustomStudyLabels, DeckDescription, DeckPromptInfo,
    DeckPromptLabels, DeckTarget, FilteredDeck, FilteredDeckForm, FilteredLabels, FilteredTerm,
    OrderChoice, Plural, StudyTag,
};

const ORDERS: [&str; 9] = [
    "Oldest seen first",
    "Random",
    "Increasing intervals",
    "Decreasing intervals",
    "Most lapses",
    "Order added",
    "Order due",
    "Latest added first",
    "Relative overdueness",
];

fn deck_changes() -> OpChanges {
    OpChanges {
        deck: true,
        study_queues: true,
        ..Default::default()
    }
}

fn form(id: i64) -> FilteredDeckForm {
    FilteredDeckForm {
        deck: FilteredDeck {
            id,
            name: if id == 0 {
                "Filtered Deck 09:30".into()
            } else {
                "Cram".into()
            },
            reschedule: true,
            first: FilteredTerm {
                search: "deck:Study is:due".into(),
                limit: 100,
                order: 6,
            },
            second: FilteredTerm {
                search: String::new(),
                limit: 20,
                order: 1,
            },
            second_enabled: false,
            preview_again_secs: 60,
            preview_hard_secs: 600,
            preview_good_secs: 0,
            allow_empty: false,
        },
        orders: ORDERS
            .iter()
            .enumerate()
            .map(|(value, label)| OrderChoice {
                value: value as i32,
                label: (*label).into(),
            })
            .collect(),
        labels: FilteredLabels {
            title: "Options for Filtered Deck 09:30".into(),
            name: "Name".into(),
            filter: "Filter".into(),
            filter_2: "Filter 2".into(),
            search: "Search".into(),
            limit_to: "Limit to".into(),
            cards_selected_by: "cards selected by".into(),
            enable_second_filter: "Enable second filter".into(),
            reschedule: "Reschedule cards based on my answers in this deck".into(),
            delay_again: "Delay for Again".into(),
            delay_hard: "Delay for Hard".into(),
            delay_good: "Delay for Good".into(),
            zero_hint: "(0 = return card to original deck)".into(),
            seconds: "seconds".into(),
            allow_empty: "Create even if empty".into(),
            build: if id == 0 { "Build" } else { "Rebuild" }.into(),
        },
    }
}

fn plural(one: &str, other: &str) -> Plural {
    Plural {
        one: one.into(),
        other: other.into(),
    }
}

fn defaults(tags: &[(&str, bool, bool)], card_count: u32) -> CustomStudyDefaults {
    CustomStudyDefaults {
        deck_id: 7,
        deck_name: "Study".into(),
        tags: tags
            .iter()
            .map(|(name, include, exclude)| StudyTag {
                name: (*name).into(),
                include: *include,
                exclude: *exclude,
            })
            .collect(),
        extend_new: 10,
        extend_review: 50,
        available_new: 3,
        available_review: 5,
        available_new_in_children: 2,
        available_review_in_children: 0,
        card_count,
        labels: CustomStudyLabels {
            modes: [
                "Increase today's new card limit".into(),
                "Increase today's review card limit".into(),
                "Review forgotten cards".into(),
                "Review ahead".into(),
                "Preview new cards".into(),
                "Study by card state or tag".into(),
            ],
            before: [
                "Increase today's new card limit by".into(),
                "Increase today's review limit by".into(),
                "Review cards forgotten in last".into(),
                "Review ahead by".into(),
                "Preview new cards added in the last".into(),
                "Select".into(),
            ],
            cards: plural("card", "cards"),
            days: plural("day", "days"),
            cards_from_the_deck: plural("card from the deck", "cards from the deck"),
            available_new: "Available new cards: 3 (2 in subdecks)".into(),
            available_review: "Available review cards: 5".into(),
            cram_kinds: [
                "New cards only".into(),
                "Due cards only".into(),
                "All review cards in random order".into(),
                "All cards in random order (don't reschedule)".into(),
            ],
            ok: "OK".into(),
            choose_tags: "Choose Tags".into(),
            tags_title: "Selective Study".into(),
            require_tags: "Require one or more of these tags:".into(),
            exclude_tags: "Select tags to exclude:".into(),
            session_name: "Custom Study Session".into(),
        },
    }
}

fn prompt_info(deck_id: i64, name: &str) -> DeckPromptInfo {
    let target = |id: i64, name: &str| DeckTarget {
        id,
        name: name.into(),
    };
    DeckPromptInfo {
        deck_id,
        name: name.into(),
        filtered: false,
        card_count: if deck_id == 0 { 0 } else { 12 },
        subdeck_count: if deck_id == 0 { 0 } else { 2 },
        targets: if deck_id == 0 {
            vec![]
        } else {
            vec![
                target(1, "Default"),
                target(20, "Languages"),
                target(21, "Languages::French"),
                target(30, "Science"),
            ]
        },
        labels: DeckPromptLabels {
            create: "Create Deck".into(),
            new_deck_name: "New deck name:".into(),
            rename: "Rename Deck".into(),
            new_name: "New name:".into(),
            delete: "Delete Deck".into(),
        },
    }
}

fn describe(text: &str, markdown: bool) -> DeckDescription {
    DeckDescription {
        deck_id: 7,
        name: "Study".into(),
        text: text.into(),
        markdown,
        // The fake "renders" Markdown by wrapping it.
        html: if markdown && !text.is_empty() {
            format!("<p>{text}</p>")
        } else {
            text.into()
        },
        title: "Description".into(),
        markdown_label: "Interpret as Markdown".into(),
    }
}

/// Answers every deck tools command and records it.
struct Fake {
    calls: RefCell<Vec<DeckToolsCommand>>,
    /// Mutations fail with this `Invalid` message.
    refuse: RefCell<Option<String>>,
    tags: Vec<(&'static str, bool, bool)>,
    description: RefCell<DeckDescription>,
}

impl Fake {
    fn new() -> Self {
        Self {
            calls: RefCell::default(),
            refuse: RefCell::default(),
            tags: vec![],
            description: RefCell::new(describe("Hello <b>deck</b>", false)),
        }
    }
    fn refuse(&self, message: &str) {
        *self.refuse.borrow_mut() = Some(message.into());
    }
    /// The mutations sent so far.
    fn sent(&self) -> Vec<DeckToolsCommand> {
        self.calls
            .borrow()
            .iter()
            .filter(|c| {
                !matches!(
                    c,
                    DeckToolsCommand::FilteredDeck { .. }
                        | DeckToolsCommand::CustomStudyDefaults { .. }
                        | DeckToolsCommand::Description { .. }
                        | DeckToolsCommand::DeckPrompt { .. }
                )
            })
            .cloned()
            .collect()
    }
    fn last(&self) -> DeckToolsCommand {
        self.sent().pop().expect("a mutation was sent")
    }
}

impl Backend for Fake {
    fn execute(&self, command: Command) -> crate::backend::Result<Response> {
        let Command::Manage(ManageCommand::DeckTools(command)) = command else {
            return Err(CoreError::Other("not a deck tools command".into()));
        };
        self.calls.borrow_mut().push(command.clone());
        let changes = deck_changes();
        let refused = || match self.refuse.borrow().clone() {
            Some(message) => Err(CoreError::Invalid(message)),
            None => Ok(()),
        };
        let response = match command {
            DeckToolsCommand::FilteredDeck {
                deck_id,
                search,
                search_2,
            } => {
                let mut form = form(deck_id);
                if let Some(search) = search {
                    form.deck.first.search = search;
                }
                if let Some(search) = search_2 {
                    form.deck.second.search = search;
                    form.deck.second_enabled = true;
                }
                DeckToolsResponse::FilteredDeck(Box::new(form))
            }
            DeckToolsCommand::CustomStudyDefaults { .. } => {
                DeckToolsResponse::CustomStudyDefaults(Box::new(defaults(&self.tags, 40)))
            }
            DeckToolsCommand::Description { .. } => {
                DeckToolsResponse::Description(self.description.borrow().clone())
            }
            DeckToolsCommand::DeckPrompt { deck_id } => {
                DeckToolsResponse::DeckPrompt(Box::new(prompt_info(
                    deck_id,
                    if deck_id == 0 {
                        ""
                    } else {
                        "Languages::Spanish"
                    },
                )))
            }
            DeckToolsCommand::SaveFilteredDeck(deck) => {
                refused()?;
                DeckToolsResponse::Saved {
                    changes,
                    deck_id: if deck.id == 0 { 99 } else { deck.id },
                }
            }
            DeckToolsCommand::CustomStudy { study, .. } => {
                refused()?;
                let limit = matches!(
                    study,
                    CustomStudy::NewLimitDelta(_) | CustomStudy::ReviewLimitDelta(_)
                );
                DeckToolsResponse::Saved {
                    changes,
                    deck_id: if limit { 0 } else { 55 },
                }
            }
            DeckToolsCommand::SetDescription { text, markdown, .. } => {
                refused()?;
                *self.description.borrow_mut() = describe(&text, markdown);
                DeckToolsResponse::Changes(changes)
            }
            DeckToolsCommand::CreateDeck { .. } => {
                refused()?;
                DeckToolsResponse::Saved {
                    changes,
                    deck_id: 77,
                }
            }
            DeckToolsCommand::RenameDeck { .. } | DeckToolsCommand::EmptyFilteredDeck { .. } => {
                refused()?;
                DeckToolsResponse::Changes(changes)
            }
            DeckToolsCommand::RebuildFilteredDeck { .. } => {
                refused()?;
                DeckToolsResponse::Count { changes, count: 4 }
            }
            DeckToolsCommand::ReparentDecks { deck_ids, .. } => {
                refused()?;
                DeckToolsResponse::Count {
                    changes,
                    count: deck_ids.len(),
                }
            }
            DeckToolsCommand::DeleteDecks { .. } => {
                refused()?;
                DeckToolsResponse::Deleted {
                    changes,
                    count: 12,
                    message: "12 cards deleted.".into(),
                }
            }
        };
        Ok(Response::Manage(ManageResponse::DeckTools(response)))
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn ch(c: char) -> KeyEvent {
    key(KeyCode::Char(c))
}
fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn display() -> Display {
    Display {
        cell_px: None,
        fg: None,
        bg: None,
        night_mode: true,
    }
}

/// Draws into a `width` × `height` buffer and returns it as text.
fn screen(width: u16, height: u16, draw: impl FnOnce(Rect, &mut Buffer)) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    draw(area, &mut buf);
    buffer_text(&buf).join("\n")
}

#[track_caller]
fn assert_screen(actual: &str, expected: &str) {
    let expected = expected.strip_prefix('\n').unwrap_or(expected);
    assert!(
        actual == expected,
        "screen differs\n--- actual ---\n{actual}\n--- expected ---\n{expected}"
    );
}

// ---- filtered deck ---------------------------------------------------------

fn filtered(ui: &FilteredDeckUi) -> String {
    screen(78, 20, |area, buf| ui.render(area, buf))
}

fn filtered_keys(ui: &mut FilteredDeckUi, fake: &Fake, keys: &[KeyEvent]) {
    for k in keys {
        assert_eq!(ui.on_key(fake, *k), DeckToolsIntent::None, "{k:?}");
    }
}

#[test]
fn filtered_deck_form_snapshot() {
    let fake = Fake::new();
    let ui = FilteredDeckUi::new(&fake).unwrap();
    assert_eq!(ui.deck_id(), 0);
    assert_screen(
        &filtered(&ui),
        r"




 ┌ Options for Filtered Deck 09:30 ─────────────────────────────────────────┐
 │  Name                Filtered Deck 09:30                                 │
 │  Filter                                                                  │
 │>   Search            deck:Study is:due                                   │
 │    Limit to          100                                                 │
 │    cards selected by ‹ Order due ›                                       │
 │  [ ] Enable second filter                                                │
 │  [x] Reschedule cards based on my answers in this deck                   │
 │  [ ] Create even if empty                                                │
 │                                                                          │
 │  Enter Build · Esc cancel                                                │
 └──────────────────────────────────────────────────────────────────────────┘



",
    );
    assert_eq!(
        ui.hints(),
        vec![
            ("Tab/↓", "next"),
            ("type", "edit"),
            ("Enter", "build"),
            ("Esc", "cancel")
        ]
    );
}

#[test]
fn filtered_deck_second_filter_and_preview_delays_snapshot() {
    let fake = Fake::new();
    let mut ui = FilteredDeckUi::edit(&fake, 5).unwrap();
    assert_eq!(ui.deck_id(), 5);
    // Search → limit → order → second filter: enable it.
    let down = key(KeyCode::Down);
    filtered_keys(&mut ui, &fake, &[down, down, down, ch(' ')]);
    // Second search, limit, order, then the reschedule flag: turn it off.
    filtered_keys(
        &mut ui,
        &fake,
        &[down, ch('i'), ch('s'), ch(':'), ch('n'), ch('e'), ch('w')],
    );
    filtered_keys(&mut ui, &fake, &[down, down, down, ch(' ')]);
    assert_screen(
        &filtered(&ui),
        r"
 ┌ Options for Filtered Deck 09:30 ─────────────────────────────────────────┐
 │  Name                Cram                                                │
 │  Filter                                                                  │
 │    Search            deck:Study is:due                                   │
 │    Limit to          100                                                 │
 │    cards selected by ‹ Order due ›                                       │
 │  [x] Enable second filter                                                │
 │  Filter 2                                                                │
 │    Search            is:new                                              │
 │    Limit to          20                                                  │
 │    cards selected by ‹ Random ›                                          │
 │> [ ] Reschedule cards based on my answers in this deck                   │
 │    Delay for Again   60     seconds                                      │
 │    Delay for Hard    600    seconds                                      │
 │    Delay for Good    0      seconds                                      │
 │                      (0 = return card to original deck)                  │
 │  [ ] Create even if empty                                                │
 │                                                                          │
 │  Enter Rebuild · Esc cancel                                              │
 └──────────────────────────────────────────────────────────────────────────┘",
    );
    assert_eq!(ui.hints()[1], ("Space", "toggle"));
    assert_eq!(ui.hints()[2], ("Enter", "rebuild"));
}

#[test]
fn filtered_deck_offers_nine_orders_and_saves_the_form() {
    let fake = Fake::new();
    let mut ui = FilteredDeckUi::new(&fake).unwrap();
    let down = key(KeyCode::Down);
    // The order row cycles through all nine orders and wraps.
    filtered_keys(&mut ui, &fake, &[down, down]);
    assert_eq!(ui.hints()[1], ("←/→", "order"));
    let mut seen = vec![];
    for _ in 0..9 {
        seen.push(ui.deck().first.order);
        filtered_keys(&mut ui, &fake, &[key(KeyCode::Right)]);
    }
    assert_eq!(seen, vec![6, 7, 8, 0, 1, 2, 3, 4, 5]);
    assert_eq!(ui.deck().first.order, 6);
    filtered_keys(&mut ui, &fake, &[key(KeyCode::Left)]);
    assert_eq!(ui.deck().first.order, 5);

    // Limit: digits only, clamped like desktop's spin box.
    let up = key(KeyCode::Up);
    filtered_keys(&mut ui, &fake, &[up, ctrl('u'), ch('x'), ch('0')]);
    assert_eq!(ui.deck().first.limit, 1);
    filtered_keys(&mut ui, &fake, &[ch('7'), ch('5')]);
    assert_eq!(ui.deck().first.limit, 75);

    // Second filter on; reschedule off; delays; allow empty.
    filtered_keys(&mut ui, &fake, &[down, down, ch(' '), down]);
    ui.on_paste("is:new\nprop:ivl=0");
    filtered_keys(&mut ui, &fake, &[down, down, down, ch(' ')]);
    filtered_keys(&mut ui, &fake, &[down, ctrl('u'), ch('9'), ch('0')]);
    filtered_keys(&mut ui, &fake, &[down, down, down, ch(' ')]);
    // Rename.
    filtered_keys(&mut ui, &fake, &[down, ctrl('u'), ch('M'), ch('y')]);

    let intent = ui.on_key(&fake, key(KeyCode::Enter));
    assert_eq!(
        intent,
        DeckToolsIntent::OpenDeck {
            deck_id: 99,
            changes: deck_changes()
        }
    );
    let DeckToolsCommand::SaveFilteredDeck(deck) = fake.last() else {
        panic!()
    };
    assert_eq!(
        deck,
        FilteredDeck {
            id: 0,
            name: "My".into(),
            reschedule: false,
            first: FilteredTerm {
                search: "deck:Study is:due".into(),
                limit: 75,
                order: 5,
            },
            second: FilteredTerm {
                search: "is:new prop:ivl=0".into(),
                limit: 20,
                order: 1,
            },
            second_enabled: true,
            preview_again_secs: 90,
            preview_hard_secs: 600,
            preview_good_secs: 0,
            allow_empty: true,
        }
    );
}

#[test]
fn filtered_deck_from_search_keeps_the_form_open_on_anki_errors() {
    let fake = Fake::new();
    let mut ui = FilteredDeckUi::from_search(&fake, "tag:hard").unwrap();
    assert_eq!(ui.deck().first.search, "tag:hard");
    assert!(filtered(&ui).contains("tag:hard"));
    fake.refuse("No cards matched the criteria you provided.");
    assert_eq!(
        ui.on_key(&fake, ctrl('s')),
        DeckToolsIntent::Message("No cards matched the criteria you provided.".into())
    );
    assert_eq!(ui.on_key(&fake, key(KeyCode::Esc)), DeckToolsIntent::Close);
    assert_eq!(fake.sent().len(), 1);
}

#[test]
fn filtered_deck_rebuild_and_empty_helpers() {
    let fake = Fake::new();
    assert_eq!(rebuild_filtered(&fake, 5).unwrap(), deck_changes());
    assert!(matches!(
        fake.last(),
        DeckToolsCommand::RebuildFilteredDeck { deck_id: 5 }
    ));
    assert_eq!(empty_filtered(&fake, 5).unwrap(), deck_changes());
    assert!(matches!(
        fake.last(),
        DeckToolsCommand::EmptyFilteredDeck { deck_id: 5 }
    ));
    fake.refuse("no");
    assert!(rebuild_filtered(&fake, 5).is_err());
}

// ---- custom study ----------------------------------------------------------

fn custom(ui: &CustomStudyUi) -> String {
    screen(70, 16, |area, buf| ui.render(area, buf))
}

fn study_sent(fake: &Fake) -> CustomStudy {
    match fake.last() {
        DeckToolsCommand::CustomStudy { deck_id, study } => {
            assert_eq!(deck_id, 7);
            study
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn custom_study_modes_snapshot() {
    let fake = Fake::new();
    let ui = CustomStudyUi::open(&fake, 7).unwrap();
    assert_eq!(ui.deck_id(), 7);
    assert_screen(
        &custom(&ui),
        r"

   ┌ Custom Study: Study ─────────────────────────────────────────┐
   │> (•) Increase today's new card limit                         │
   │  ( ) Increase today's review card limit                      │
   │  ( ) Review forgotten cards                                  │
   │  ( ) Review ahead                                            │
   │  ( ) Preview new cards                                       │
   │  ( ) Study by card state or tag                              │
   │                                                              │
   │  Available new cards: 3 (2 in subdecks)                      │
   │  Increase today's new card limit by 10   cards               │
   │                                                              │
   │                                                              │
   │  Enter OK · Esc cancel                                       │
   └──────────────────────────────────────────────────────────────┘
",
    );
}

#[test]
fn custom_study_six_modes_send_ankis_defaults() {
    let fake = Fake::new();
    let done = DeckToolsIntent::Done {
        changes: deck_changes(),
        message: None,
    };
    let session = DeckToolsIntent::OpenDeck {
        deck_id: 55,
        changes: deck_changes(),
    };
    let enter = key(KeyCode::Enter);
    let expected = [
        (CustomStudy::NewLimitDelta(10), &done),
        (CustomStudy::ReviewLimitDelta(50), &done),
        (CustomStudy::ForgotDays(1), &session),
        (CustomStudy::ReviewAheadDays(1), &session),
        (CustomStudy::PreviewDays(1), &session),
        (
            CustomStudy::Cram {
                kind: CramKind::New,
                card_limit: 100,
                tags_to_include: vec![],
                tags_to_exclude: vec![],
            },
            &session,
        ),
    ];
    for (mode, (study, intent)) in expected.into_iter().enumerate() {
        let mut ui = CustomStudyUi::open(&fake, 7).unwrap();
        for _ in 0..mode {
            ui.on_key(&fake, ch('j'));
        }
        assert_eq!(&ui.on_key(&fake, enter), intent, "mode {mode}");
        assert_eq!(study_sent(&fake), study, "mode {mode}");
    }
}

#[test]
fn custom_study_amounts_are_edited_and_clamped() {
    let fake = Fake::new();
    let enter = key(KeyCode::Enter);
    // A negative delta decreases today's limit.
    let mut ui = CustomStudyUi::open(&fake, 7).unwrap();
    for k in [key(KeyCode::Backspace), ch('5'), ch('-')] {
        ui.on_key(&fake, k);
    }
    ui.on_key(&fake, enter);
    assert_eq!(study_sent(&fake), CustomStudy::NewLimitDelta(-15));

    // Forgotten days stop at 30; the sign key does nothing there.
    let mut ui = CustomStudyUi::open(&fake, 7).unwrap();
    for k in [ch('j'), ch('j'), ch('-'), ch('9'), ch('9')] {
        ui.on_key(&fake, k);
    }
    assert!(custom(&ui).contains("Review cards forgotten in last 199"));
    ui.on_key(&fake, enter);
    assert_eq!(study_sent(&fake), CustomStudy::ForgotDays(30));

    // Review ahead; the mode keeps its number, "day" follows the count.
    let mut ui = CustomStudyUi::open(&fake, 7).unwrap();
    for _ in 0..3 {
        ui.on_key(&fake, key(KeyCode::Down));
    }
    assert!(custom(&ui).contains("Review ahead by 1    day"));
    ui.on_key(&fake, key(KeyCode::Backspace));
    ui.on_key(&fake, ch('3'));
    assert!(custom(&ui).contains("Review ahead by 3    days"));
    ui.on_key(&fake, enter);
    assert_eq!(study_sent(&fake), CustomStudy::ReviewAheadDays(3));

    // Anki's refusal keeps the dialog open.
    fake.refuse("No cards matched the criteria you provided.");
    assert_eq!(
        ui.on_key(&fake, enter),
        DeckToolsIntent::Message("No cards matched the criteria you provided.".into())
    );
    assert_eq!(ui.on_key(&fake, key(KeyCode::Esc)), DeckToolsIntent::Close);
}

#[test]
fn custom_study_card_state_and_tag_chooser_snapshot() {
    let mut fake = Fake::new();
    fake.tags = vec![
        ("grammar", false, false),
        ("hard", true, false),
        ("leech", false, true),
        ("verbs", false, false),
    ];
    let mut ui = CustomStudyUi::open(&fake, 7).unwrap();
    ui.on_key(&fake, ch('k'));
    // Due cards only, 25 of them.
    for k in [
        key(KeyCode::Right),
        key(KeyCode::Backspace),
        key(KeyCode::Backspace),
        key(KeyCode::Backspace),
        ch('2'),
        ch('5'),
    ] {
        ui.on_key(&fake, k);
    }
    assert_screen(
        &custom(&ui),
        r"

   ┌ Custom Study: Study ─────────────────────────────────────────┐
   │  ( ) Increase today's new card limit                         │
   │  ( ) Increase today's review card limit                      │
   │  ( ) Review forgotten cards                                  │
   │  ( ) Review ahead                                            │
   │  ( ) Preview new cards                                       │
   │> (•) Study by card state or tag                              │
   │                                                              │
   │                                                              │
   │  Select 25   cards from the deck                             │
   │  ‹ Due cards only ›                                          │
   │                                                              │
   │  Enter Choose Tags · Esc cancel                              │
   └──────────────────────────────────────────────────────────────┘
",
    );
    assert_eq!(ui.hints()[3], ("Enter", "choose tags"));

    // Enter opens the chooser with the remembered choices.
    assert_eq!(ui.on_key(&fake, key(KeyCode::Enter)), DeckToolsIntent::None);
    assert!(fake.sent().is_empty());
    assert_screen(
        &custom(&ui),
        r"



   ┌ Selective Study ─────────────────────────────────────────────┐
   │  [+] Require one or more of these tags:                      │
   │  [-] Select tags to exclude:                                 │
   │> [ ] grammar                                                 │
   │  [+] hard                                                    │
   │  [-] leech                                                   │
   │  [ ] verbs                                                   │
   │                                                              │
   │  1 required · 1 excluded · Enter OK                          │
   └──────────────────────────────────────────────────────────────┘


",
    );
    // Require "grammar"; "hard" from required to excluded; "leech" cleared.
    for k in [ch('i'), ch('j'), ch('x'), ch('j'), ch('x')] {
        assert_eq!(ui.on_key(&fake, k), DeckToolsIntent::None);
    }
    // Esc goes back to the modes, Enter returns with the remembered choice.
    assert_eq!(ui.on_key(&fake, key(KeyCode::Esc)), DeckToolsIntent::None);
    assert!(custom(&ui).contains("Custom Study: Study"));
    ui.on_key(&fake, key(KeyCode::Enter));
    for k in [ch(' '), ch('G'), ch(' '), ch(' ')] {
        ui.on_key(&fake, k);
    }
    assert_eq!(
        ui.on_key(&fake, key(KeyCode::Enter)),
        DeckToolsIntent::OpenDeck {
            deck_id: 55,
            changes: deck_changes()
        }
    );
    assert_eq!(
        study_sent(&fake),
        CustomStudy::Cram {
            kind: CramKind::Due,
            card_limit: 25,
            tags_to_include: vec!["grammar".into(), "hard".into()],
            tags_to_exclude: vec!["leech".into(), "verbs".into()],
        }
    );
}

#[test]
fn custom_study_on_an_empty_deck_only_changes_limits() {
    let fake = Fake::new();
    let mut ui = CustomStudyUi::from_defaults(defaults(&[], 0));
    ui.on_key(&fake, ch('j'));
    ui.on_key(&fake, ch('j'));
    assert!(matches!(
        ui.on_key(&fake, key(KeyCode::Enter)),
        DeckToolsIntent::Message(_)
    ));
    assert!(fake.sent().is_empty());
    ui.on_key(&fake, ch('k'));
    assert!(matches!(
        ui.on_key(&fake, key(KeyCode::Enter)),
        DeckToolsIntent::Done { .. }
    ));
}

// ---- deck prompts ----------------------------------------------------------

fn prompt(ui: &DeckPromptUi) -> String {
    screen(70, 18, |area, buf| ui.render(area, buf))
}

fn type_text(ui: &mut DeckPromptUi, fake: &Fake, text: &str) {
    for c in text.chars() {
        assert_eq!(ui.on_key(fake, ch(c)), DeckToolsIntent::None);
    }
}

fn done(message: Option<&str>) -> DeckToolsIntent {
    DeckToolsIntent::Done {
        changes: deck_changes(),
        message: message.map(Into::into),
    }
}

#[test]
fn add_deck_prompt_snapshot_and_create() {
    let fake = Fake::new();
    let mut ui = DeckPromptUi::add(&fake, "Languages::").unwrap();
    assert_eq!(ui.kind(), DeckPromptKind::Add);
    assert_eq!(ui.deck_id(), 0);
    type_text(&mut ui, &fake, "Italian");
    assert_screen(
        &prompt(&ui),
        r"






   ┌ Create Deck ─────────────────────────────────────────────────┐
   │  New deck name:                                              │
   │  Languages::Italian                                          │
   │                                                              │
   │  Enter create · Esc cancel                                   │
   └──────────────────────────────────────────────────────────────┘





",
    );
    assert_eq!(ui.on_key(&fake, key(KeyCode::Enter)), done(None));
    let DeckToolsCommand::CreateDeck { name } = fake.last() else {
        panic!()
    };
    assert_eq!(name, "Languages::Italian");

    // An empty name is not sent; Anki's refusal keeps the prompt.
    let mut ui = DeckPromptUi::add(&fake, "").unwrap();
    assert!(matches!(
        ui.on_key(&fake, key(KeyCode::Enter)),
        DeckToolsIntent::Message(_)
    ));
    assert_eq!(fake.sent().len(), 1);
    ui.on_paste("Clash");
    fake.refuse("A deck with this name exists.");
    assert_eq!(
        ui.on_key(&fake, key(KeyCode::Enter)),
        DeckToolsIntent::Message("A deck with this name exists.".into())
    );
    assert_eq!(ui.on_key(&fake, key(KeyCode::Esc)), DeckToolsIntent::Close);
}

#[test]
fn rename_deck_prompt_snapshot_and_rename() {
    let fake = Fake::new();
    let mut ui = DeckPromptUi::rename(&fake, 22).unwrap();
    assert_screen(
        &prompt(&ui),
        r"






   ┌ Rename Deck: Languages::Spanish ─────────────────────────────┐
   │  New name:                                                   │
   │  Languages::Spanish                                          │
   │                                                              │
   │  Enter rename · Esc cancel                                   │
   └──────────────────────────────────────────────────────────────┘





",
    );
    // The unchanged name closes without a command.
    assert_eq!(
        ui.on_key(&fake, key(KeyCode::Enter)),
        DeckToolsIntent::Close
    );
    assert!(fake.sent().is_empty());
    for _ in "Spanish".chars() {
        ui.on_key(&fake, key(KeyCode::Backspace));
    }
    type_text(&mut ui, &fake, "Castilian");
    assert_eq!(ui.on_key(&fake, key(KeyCode::Enter)), done(None));
    let DeckToolsCommand::RenameDeck { deck_id, new_name } = fake.last() else {
        panic!()
    };
    assert_eq!((deck_id, new_name.as_str()), (22, "Languages::Castilian"));
}

#[test]
fn move_deck_prompt_snapshot_filter_and_reparent() {
    let fake = Fake::new();
    let mut ui = DeckPromptUi::move_to(&fake, 22).unwrap();
    ui.on_key(&fake, key(KeyCode::Down));
    ui.on_key(&fake, key(KeyCode::Down));
    assert_screen(
        &prompt(&ui),
        r"



   ┌ Move Languages::Spanish to… ─────────────────────────────────┐
   │  Filter:                                                     │
   │                                                              │
   │  (top level)                                                 │
   │  Default                                                     │
   │> Languages                                                   │
   │  Languages::French                                           │
   │  Science                                                     │
   │                                                              │
   │  type filter · ↑/↓ choose · Enter move · Esc cancel          │
   └──────────────────────────────────────────────────────────────┘



",
    );
    // Typing filters and returns to the first match.
    type_text(&mut ui, &fake, "sci");
    assert_screen(
        &prompt(&ui),
        r"





   ┌ Move Languages::Spanish to… ─────────────────────────────────┐
   │  Filter: sci                                                 │
   │                                                              │
   │> Science                                                     │
   │                                                              │
   │  type filter · ↑/↓ choose · Enter move · Esc cancel          │
   └──────────────────────────────────────────────────────────────┘





",
    );
    assert_eq!(
        ui.on_key(&fake, key(KeyCode::Enter)),
        done(Some("Moved Spanish to Science."))
    );
    let DeckToolsCommand::ReparentDecks {
        deck_ids,
        new_parent,
    } = fake.last()
    else {
        panic!()
    };
    assert_eq!((deck_ids, new_parent), (vec![22], 30));

    // A subdeck can go to the top level; nothing matching sends nothing.
    let mut ui = DeckPromptUi::move_to(&fake, 22).unwrap();
    assert_eq!(
        ui.on_key(&fake, key(KeyCode::Enter)),
        done(Some("Moved Spanish to the top level."))
    );
    assert!(matches!(
        fake.last(),
        DeckToolsCommand::ReparentDecks { new_parent: 0, .. }
    ));
    let mut ui = DeckPromptUi::move_to(&fake, 22).unwrap();
    type_text(&mut ui, &fake, "zzz");
    assert!(prompt(&ui).contains("No deck matches."));
    assert!(matches!(
        ui.on_key(&fake, key(KeyCode::Enter)),
        DeckToolsIntent::Message(_)
    ));
    assert_eq!(fake.sent().len(), 2);

    // A top-level deck is not offered the top level.
    let ui = DeckPromptUi::from_info(DeckPromptKind::Move, prompt_info(30, "Science"));
    assert!(!prompt(&ui).contains("(top level)"));
}

#[test]
fn delete_deck_prompt_snapshot_and_confirm() {
    let fake = Fake::new();
    let mut ui = DeckPromptUi::delete(&fake, 22).unwrap();
    assert_screen(
        &prompt(&ui),
        r"





   ┌ Delete Deck ─────────────────────────────────────────────────┐
   │  Languages::Spanish                                          │
   │                                                              │
   │  12 cards and 2 subdecks will be deleted.                    │
   │                                                              │
   │  y delete · Esc cancel                                       │
   └──────────────────────────────────────────────────────────────┘





",
    );
    // Only `y` deletes.
    assert_eq!(ui.on_key(&fake, key(KeyCode::Enter)), DeckToolsIntent::None);
    assert_eq!(ui.on_key(&fake, ch('n')), DeckToolsIntent::Close);
    assert!(fake.sent().is_empty());
    assert_eq!(ui.on_key(&fake, ch('y')), done(Some("12 cards deleted.")));
    let DeckToolsCommand::DeleteDecks { deck_ids } = fake.last() else {
        panic!()
    };
    assert_eq!(deck_ids, vec![22]);

    let mut info = prompt_info(40, "Cram");
    info.filtered = true;
    info.card_count = 1;
    let ui = DeckPromptUi::from_info(DeckPromptKind::Delete, info);
    assert!(prompt(&ui).contains("1 card will return to their home decks."));
}

// ---- description -----------------------------------------------------------

fn description(ui: &DeckDescriptionUi, height: u16) -> String {
    screen(60, height, |area, buf| {
        ui.render(area, buf, &PlainTextRenderer, &display(), Path::new("/tmp"))
    })
}

#[test]
fn description_view_snapshot_and_scroll() {
    let fake = Fake::new();
    let ui = DeckDescriptionUi::open(&fake, 7).unwrap();
    assert_eq!(ui.deck_id(), 7);
    assert!(!ui.is_editing());
    assert_screen(
        &description(&ui, 12),
        r"

┌ Description: Study ──────────────────────────────────────┐
│ Hello deck                                               │
│                                                          │
│                                                          │
│                                                          │
│                                                          │
│                                                          │
│                                                          │
│ e edit · j/k scroll · Esc close                          │
└──────────────────────────────────────────────────────────┘
",
    );

    let long = (1..=30)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("<br>");
    let mut ui = DeckDescriptionUi::from_description(describe(&long, false));
    let text = description(&ui, 12);
    assert!(
        text.contains("line 1") && !text.contains("line 9"),
        "{text}"
    );
    for k in [ch('j'), ch('j'), key(KeyCode::Up)] {
        assert_eq!(ui.on_key(&fake, k), DeckToolsIntent::None);
    }
    let text = description(&ui, 12);
    assert!(
        text.contains("line 2") && !text.contains("line 1\n"),
        "{text}"
    );
    ui.on_key(&fake, ch('G'));
    let text = description(&ui, 12);
    assert!(text.contains("line 30") && text.contains("30/30"), "{text}");
    assert_eq!(ui.on_key(&fake, ch('q')), DeckToolsIntent::Close);

    let ui = DeckDescriptionUi::from_description(describe("", false));
    assert!(description(&ui, 12).contains("This deck has no description."));
}

#[test]
fn description_editor_snapshot_markdown_flag_and_save() {
    let fake = Fake::new();
    let mut ui = DeckDescriptionUi::open(&fake, 7).unwrap();
    assert_eq!(ui.on_key(&fake, ch('e')), DeckToolsIntent::None);
    assert!(ui.is_editing());
    // The flag, then new text at the start.
    assert_eq!(ui.on_key(&fake, ctrl('t')), DeckToolsIntent::None);
    for k in [ch('#'), ch(' '), ch('H'), ch('i'), key(KeyCode::Enter)] {
        ui.on_key(&fake, k);
    }
    assert_screen(
        &description(&ui, 12),
        r"

┌ Description: Study ──────────────────────────────────────┐
│ [x] Interpret as Markdown                                │
│                                                          │
│ # Hi                                                     │
│ Hello <b>deck</b>                                        │
│                                                          │
│                                                          │
│                                                          │
│ Ctrl+S save · Ctrl+T Markdown · Esc cancel               │
└──────────────────────────────────────────────────────────┘
",
    );
    assert_eq!(
        ui.hints(),
        vec![
            ("Ctrl+S", "save"),
            ("Ctrl+T", "Markdown"),
            ("Esc", "cancel")
        ]
    );

    // Anki's refusal keeps the editor; then the save shows the new text.
    fake.refuse("nope");
    assert_eq!(
        ui.on_key(&fake, ctrl('s')),
        DeckToolsIntent::Message("nope".into())
    );
    assert!(ui.is_editing());
    *fake.refuse.borrow_mut() = None;
    assert_eq!(
        ui.on_key(&fake, ctrl('s')),
        DeckToolsIntent::Changed(deck_changes())
    );
    let DeckToolsCommand::SetDescription {
        deck_id,
        text,
        markdown,
    } = fake.last()
    else {
        panic!()
    };
    assert_eq!(
        (deck_id, text.as_str(), markdown),
        (7, "# Hi\nHello <b>deck</b>", true)
    );
    assert!(!ui.is_editing());
    assert!(description(&ui, 12).contains("# Hi"));
}

#[test]
fn description_editor_warns_before_discarding() {
    let fake = Fake::new();
    let esc = key(KeyCode::Esc);
    // Opened in the editor: leaving it closes the dialog.
    let mut ui = DeckDescriptionUi::edit(&fake, 7).unwrap();
    assert!(ui.is_editing());
    assert_eq!(ui.on_key(&fake, esc), DeckToolsIntent::Close);

    let mut ui = DeckDescriptionUi::edit(&fake, 7).unwrap();
    ui.on_paste("pasted\ntext ");
    assert!(matches!(ui.on_key(&fake, esc), DeckToolsIntent::Message(_)));
    // Another key withdraws the warning.
    ui.on_key(&fake, ch('!'));
    assert!(matches!(ui.on_key(&fake, esc), DeckToolsIntent::Message(_)));
    assert_eq!(ui.on_key(&fake, esc), DeckToolsIntent::Close);
    assert!(fake.sent().is_empty());

    // A direct editor is done after the save.
    let mut ui = DeckDescriptionUi::edit(&fake, 7).unwrap();
    ui.on_key(&fake, ch('x'));
    assert_eq!(
        ui.on_key(&fake, ctrl('s')),
        DeckToolsIntent::Done {
            changes: deck_changes(),
            message: None
        }
    );

    // From the view, a discarded edit returns to the view.
    let mut ui = DeckDescriptionUi::open(&fake, 7).unwrap();
    ui.on_key(&fake, key(KeyCode::Enter));
    assert_eq!(ui.on_key(&fake, esc), DeckToolsIntent::None);
    assert!(!ui.is_editing());
    assert_eq!(ui.on_key(&fake, esc), DeckToolsIntent::Close);
}
