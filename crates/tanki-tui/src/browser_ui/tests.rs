//! Browser widget tests with a fake collection answering `Command::Browse`,
//! `Command::Notes` and the Phase 1 card commands.
use super::*;
use crate::services::PlainTextRenderer;
use crate::testing::buffer_text;
use std::cell::{Cell, RefCell};
use tanki_core::{
    FieldInfo, ForgetDefaults, NotetypeInfo, RepositionDefaults, SidebarDeck, TagNode, TemplateInfo,
};

struct Fake {
    cards: usize,
    log: RefCell<Vec<Command>>,
    settings: RefCell<BrowserSettings>,
    saved: RefCell<Vec<(String, String)>>,
    rows_requested: Cell<usize>,
    deleted_notes: RefCell<HashSet<i64>>,
    sections: RefCell<[bool; 7]>,
}

fn note_of(card: i64) -> i64 {
    1000 + (card + 1) / 2
}

fn table_changes() -> OpChanges {
    OpChanges {
        card: true,
        browser_table: true,
        ..Default::default()
    }
}

impl Fake {
    fn new(cards: usize) -> Self {
        Self {
            cards,
            log: RefCell::new(Vec::new()),
            settings: RefCell::new(BrowserSettings {
                mode: BrowseMode::Cards,
                card_columns: ["noteFld", "template", "cardDue", "deck"]
                    .map(String::from)
                    .to_vec(),
                note_columns: ["noteFld", "note", "template", "noteTags"]
                    .map(String::from)
                    .to_vec(),
                card_sort: "noteFld".into(),
                card_sort_reverse: false,
                note_sort: "noteCrt".into(),
                note_sort_reverse: false,
                default_search: String::new(),
            }),
            saved: RefCell::new(vec![("Due cards".into(), "is:due".into())]),
            rows_requested: Cell::new(0),
            deleted_notes: RefCell::new(HashSet::new()),
            sections: RefCell::new([false; 7]),
        }
    }

    fn logged(&self, pred: impl Fn(&Command) -> bool) -> Vec<Command> {
        self.log
            .borrow()
            .iter()
            .filter(|c| pred(c))
            .cloned()
            .collect()
    }

    /// The last command other than the row and preview fetches of `prepare`.
    fn last(&self) -> Option<Command> {
        self.log
            .borrow()
            .iter()
            .rev()
            .find(|c| {
                !matches!(
                    c,
                    Command::Browse(BrowseCommand::Rows { .. }) | Command::Render { .. }
                )
            })
            .cloned()
    }

    fn last_browse(&self) -> Option<BrowseCommand> {
        self.log.borrow().iter().rev().find_map(|c| match c {
            Command::Browse(b) if !matches!(b, BrowseCommand::Rows { .. }) => Some(b.clone()),
            _ => None,
        })
    }

    fn all_cards(&self) -> Vec<i64> {
        let deleted = self.deleted_notes.borrow();
        (1..=self.cards as i64)
            .filter(|c| !deleted.contains(&note_of(*c)))
            .collect()
    }

    fn column(key: &str, label: &str, cards: Option<SortDirection>, centered: bool) -> ColumnInfo {
        ColumnInfo {
            key: key.into(),
            cards_label: label.into(),
            notes_label: if key == "template" {
                "Cards".into()
            } else {
                label.into()
            },
            sort_cards: cards,
            sort_notes: cards,
            centered,
        }
    }

    fn search(&self, query: &str, mode: BrowseMode, order: &SearchOrder) -> Vec<i64> {
        if query == "none" {
            return vec![];
        }
        // `cid:N term` / `nid:N term`: the toggles' state checks.
        if let Some((filter, term)) = query.split_once(' ') {
            if let Some(id) = filter
                .strip_prefix("cid:")
                .or_else(|| filter.strip_prefix("nid:"))
                .and_then(|n| n.parse::<i64>().ok())
            {
                let card = if filter.starts_with("nid:") {
                    (id - 1000) * 2 - 1
                } else {
                    id
                };
                let hit = match term {
                    "is:suspended" => card == 4 || card == 3,
                    "is:buried" => card == 5,
                    "tag:marked" => card == 3 || card == 4,
                    _ => false,
                };
                return if hit { vec![card] } else { vec![] };
            }
        }
        let mut ids = match mode {
            BrowseMode::Cards => self.all_cards(),
            BrowseMode::Notes => {
                let mut notes: Vec<i64> = self.all_cards().into_iter().map(note_of).collect();
                notes.dedup();
                notes
            }
        };
        if matches!(order, SearchOrder::Column { reverse: true, .. }) {
            ids.reverse();
        }
        ids
    }

    fn row(&self, id: i64, mode: BrowseMode, columns: &[String]) -> BrowserRow {
        let note = match mode {
            BrowseMode::Cards => note_of(id),
            BrowseMode::Notes => id,
        };
        if self.deleted_notes.borrow().contains(&note) {
            return BrowserRow {
                id,
                error: Some("(deleted)".into()),
                ..Default::default()
            };
        }
        let cells = columns
            .iter()
            .map(|key| tanki_core::BrowserCell {
                text: match key.as_str() {
                    "noteFld" => format!("Front {id}"),
                    "template" => "Card 1".into(),
                    "cardDue" => "2026-11-14".into(),
                    "deck" => "Lang::French".into(),
                    "note" => "Basic".into(),
                    "noteTags" => "leech".into(),
                    other => other.into(),
                },
                rtl: false,
                elide: if key == "deck" {
                    Elide::Middle
                } else {
                    Elide::Right
                },
            })
            .collect();
        let color = match (mode, id) {
            (BrowseMode::Cards, 2) => RowColor::Flag(1),
            (BrowseMode::Cards, 3) => RowColor::Marked,
            (BrowseMode::Cards, 4) => RowColor::Suspended,
            (BrowseMode::Cards, 5) => RowColor::Buried,
            _ => RowColor::Default,
        };
        BrowserRow {
            id,
            cells,
            color,
            error: None,
        }
    }

    fn sidebar(&self) -> SidebarData {
        SidebarData {
            section_collapsed: *self.sections.borrow(),
            saved_searches: self.saved.borrow().clone(),
            decks: vec![
                SidebarDeck {
                    id: 1,
                    name: "Default".into(),
                    full_name: "Default".into(),
                    ..Default::default()
                },
                SidebarDeck {
                    id: 5,
                    name: "Lang".into(),
                    full_name: "Lang".into(),
                    children: vec![SidebarDeck {
                        id: 6,
                        name: "French".into(),
                        full_name: "Lang::French".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ],
            tags: TagNode {
                children: vec![TagNode {
                    name: "leech".into(),
                    full_name: "leech".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            notetypes: vec![NotetypeInfo {
                id: 7,
                name: "Basic".into(),
                fields: vec![
                    FieldInfo {
                        name: "Front".into(),
                        ..Default::default()
                    },
                    FieldInfo {
                        ord: 1,
                        name: "Back".into(),
                        ..Default::default()
                    },
                ],
                templates: vec![TemplateInfo {
                    ord: 0,
                    name: "Card 1".into(),
                }],
                ..Default::default()
            }],
            flag_names: [
                "Red",
                "Orange",
                "Green",
                "Blue",
                "Pink",
                "Turquoise",
                "Purple",
            ]
            .map(String::from)
            .to_vec(),
        }
    }

    fn browse(&self, command: BrowseCommand) -> Result<EditResponse> {
        use EditResponse as R;
        Ok(match command {
            BrowseCommand::Settings => R::Settings(self.settings.borrow().clone()),
            BrowseCommand::SetSettings(s) => {
                *self.settings.borrow_mut() = s;
                R::Changes(OpChanges {
                    config: true,
                    ..Default::default()
                })
            }
            BrowseCommand::Columns => R::Columns(vec![
                Self::column("cardDue", "Due", Some(SortDirection::Descending), true),
                Self::column("deck", "Deck", Some(SortDirection::Ascending), false),
                Self::column("note", "Note", Some(SortDirection::Ascending), false),
                Self::column("question", "Question", None, false),
                Self::column(
                    "noteFld",
                    "Sort Field",
                    Some(SortDirection::Ascending),
                    false,
                ),
                Self::column("noteTags", "Tags", Some(SortDirection::Ascending), false),
                Self::column("template", "Card", Some(SortDirection::Descending), true),
            ]),
            BrowseCommand::Sidebar => R::Sidebar(self.sidebar()),
            BrowseCommand::NormalizeSearch { query } => {
                if query.contains("bad(") {
                    return Err(CoreError::Invalid("Missing a closing parenthesis".into()));
                }
                R::Text(query.split_whitespace().collect::<Vec<_>>().join(" "))
            }
            BrowseCommand::JoinSearch {
                existing,
                additional,
                joiner,
                negate,
            } => {
                let additional = if negate {
                    format!("-{additional}")
                } else {
                    additional
                };
                R::Text(match joiner {
                    SearchJoiner::And => format!("{existing} {additional}"),
                    SearchJoiner::Or => format!("{existing} OR {additional}"),
                })
            }
            BrowseCommand::ReplaceSearch { replacement, .. } => R::Text(replacement),
            BrowseCommand::Search { query, mode, order } => {
                R::Ids(self.search(&query, mode, &order))
            }
            BrowseCommand::Rows { mode, columns, ids } => {
                self.rows_requested
                    .set(self.rows_requested.get() + ids.len());
                R::Rows(ids.iter().map(|&id| self.row(id, mode, &columns)).collect())
            }
            BrowseCommand::CardsOfNotes { note_ids } => R::Ids(
                note_ids
                    .iter()
                    .flat_map(|n| [(n - 1000) * 2 - 1, (n - 1000) * 2])
                    .collect(),
            ),
            BrowseCommand::NotesOfCards { card_ids } => {
                let mut notes: Vec<i64> = card_ids.iter().map(|&c| note_of(c)).collect();
                notes.dedup();
                R::Ids(notes)
            }
            BrowseCommand::SetDeckCollapsed { .. } | BrowseCommand::SetTagCollapsed { .. } => {
                R::Changes(OpChanges::default())
            }
            BrowseCommand::SetSectionCollapsed { section, collapsed } => {
                self.sections.borrow_mut()[section as usize] = collapsed;
                R::Changes(OpChanges {
                    config: true,
                    ..Default::default()
                })
            }
            BrowseCommand::SetSavedSearches { searches } => {
                *self.saved.borrow_mut() = searches;
                R::Changes(OpChanges {
                    config: true,
                    ..Default::default()
                })
            }
            BrowseCommand::ForgetDefaults => R::ForgetDefaults(ForgetDefaults {
                restore_position: true,
                reset_counts: false,
            }),
            BrowseCommand::RepositionDefaults => R::RepositionDefaults(RepositionDefaults {
                random: false,
                shift: true,
            }),
            BrowseCommand::SetDueDefault => R::Text("0".into()),
            BrowseCommand::FieldNamesForNotes { .. } => {
                R::Strings(vec!["Back".into(), "Front".into()])
            }
            BrowseCommand::FindReplace(f) => {
                assert!(!f.note_ids.is_empty(), "the widget always passes ids");
                R::Count {
                    count: f.note_ids.len(),
                    changes: table_changes(),
                }
            }
            BrowseCommand::SetDeck { card_ids, .. }
            | BrowseCommand::SetDeckByName { card_ids, .. }
            | BrowseCommand::Reposition { card_ids, .. } => R::Count {
                count: card_ids.len(),
                changes: table_changes(),
            },
            BrowseCommand::Restore { .. }
            | BrowseCommand::Forget { .. }
            | BrowseCommand::SetDueDate { .. }
            | BrowseCommand::GradeNow { .. } => R::Changes(table_changes()),
        })
    }
}

impl Backend for Fake {
    fn execute(&self, command: Command) -> Result<Response> {
        self.log.borrow_mut().push(command.clone());
        Ok(match command {
            Command::Browse(b) => Response::Edit(self.browse(b)?),
            Command::Notes(NoteCommand::AddTags { note_ids, .. })
            | Command::Notes(NoteCommand::RemoveTags { note_ids, .. }) => {
                Response::Edit(EditResponse::Count {
                    count: note_ids.len(),
                    changes: OpChanges {
                        note: true,
                        tag: true,
                        browser_table: true,
                        ..Default::default()
                    },
                })
            }
            Command::Notes(NoteCommand::ClearUnusedTags) => Response::Edit(EditResponse::Count {
                count: 2,
                changes: OpChanges {
                    tag: true,
                    browser_sidebar: true,
                    ..Default::default()
                },
            }),
            Command::SetFlag { .. } | Command::SetMarked { .. } | Command::BuryOrSuspend { .. } => {
                Response::Changes(table_changes())
            }
            Command::DeleteNotes { note_ids } => {
                self.deleted_notes.borrow_mut().extend(note_ids);
                Response::Changes(table_changes())
            }
            Command::Render { card_id } => Response::Rendered(RenderedCard {
                question: format!("Question {card_id} [[type:Back]]"),
                answer: format!("Question {card_id}<hr id=answer>Answer {card_id}"),
                ..Default::default()
            }),
            Command::Undo => Response::Undone {
                changes: table_changes(),
                label: "Set Flag".into(),
            },
            other => return Err(CoreError::Other(format!("fake: {other:?}"))),
        })
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

fn open(fake: &Fake) -> BrowserUi {
    BrowserUi::open(
        fake,
        BrowserOptions {
            history: vec!["tag:leech".into()],
            ..Default::default()
        },
    )
    .unwrap()
}

fn draw(ui: &mut BrowserUi, fake: &Fake, width: u16, height: u16) -> (Vec<String>, Buffer) {
    let area = Rect::new(0, 0, width, height);
    ui.prepare(fake, area);
    let mut buf = Buffer::empty(area);
    ui.render(area, &mut buf, &PlainTextRenderer);
    (buffer_text(&buf), buf)
}

fn press(ui: &mut BrowserUi, fake: &Fake, keys: &[KeyEvent]) -> BrowserIntent {
    let mut last = BrowserIntent::None;
    for k in keys {
        last = ui.on_key(fake, *k);
        ui.prepare(fake, Rect::new(0, 0, 100, 16));
    }
    last
}

fn type_text(ui: &mut BrowserUi, fake: &Fake, text: &str) {
    for c in text.chars() {
        ui.on_key(fake, ch(c));
    }
}

#[test]
fn opens_on_the_default_search_and_draws_the_table() {
    let fake = Fake::new(8);
    let mut ui = open(&fake);
    assert_eq!(ui.query(), "deck:current");
    assert!(matches!(
        &fake.logged(|c| matches!(c, Command::Browse(BrowseCommand::Search { .. })))[0],
        Command::Browse(BrowseCommand::Search { order: SearchOrder::Column { key, reverse: false }, .. }) if key == "noteFld"
    ));
    let (text, buf) = draw(&mut ui, &fake, 100, 14);
    assert_eq!(
        text,
        [
            "Search: deck:current                                                                        [Cards]",
            "▾ Saved Searches           │  Sort Field ▲                      Card        Due     Deck",
            "    Due cards              │  Front 1                          Card 1    2026-11-14 Lang::French",
            "▾ Today                    │ ⚑Front 2                          Card 1    2026-11-14 Lang::French",
            "    Due Today              │ ★Front 3                          Card 1    2026-11-14 Lang::French",
            "    Added Today            │ !Front 4                          Card 1    2026-11-14 Lang::French",
            "    Edited Today           │ ~Front 5                          Card 1    2026-11-14 Lang::French",
            "    Studied Today          │  Front 6                          Card 1    2026-11-14 Lang::French",
            "    First Review           │  Front 7                          Card 1    2026-11-14 Lang::French",
            "    Rescheduled            │  Front 8                          Card 1    2026-11-14 Lang::French",
            "    Again Today            │",
            "    Overdue                │",
            "8 cards shown",
            "/ search  Tab sidebar  Space select  v range  C-a all  * invert  Enter edit  p preview  P side  M ca",
        ]
    );
    // Cursor row reversed; row colours from Anki's precedence.
    assert!(buf[(30, 2)].modifier.contains(Modifier::REVERSED));
    assert_eq!(buf[(30, 3)].fg, Color::Red);
    assert_eq!(buf[(30, 6)].fg, Color::Rgb(190, 120, 30));
}

#[test]
fn rows_are_fetched_lazily_for_the_visible_window() {
    let fake = Fake::new(50_000);
    let mut ui = open(&fake);
    assert_eq!(ui.ids().len(), 50_000);
    assert_eq!(
        fake.rows_requested.get(),
        0,
        "nothing fetched before a draw"
    );
    draw(&mut ui, &fake, 100, 30);
    let first = fake.rows_requested.get();
    assert!(first > 0 && first <= 27 + OVERSCAN, "{first}");
    // Redrawing fetches nothing new.
    draw(&mut ui, &fake, 100, 30);
    assert_eq!(fake.rows_requested.get(), first);
    ui.on_key(&fake, key(KeyCode::End));
    let (text, _) = draw(&mut ui, &fake, 100, 30);
    assert!(text.iter().any(|l| l.contains("Front 50000")));
    let total = fake.rows_requested.get();
    assert!(total <= first + 27 + OVERSCAN * 2, "{total}");
    assert!(ui.cached_rows() < 200);
    // Changes invalidate the cache: the window is fetched again.
    ui.on_changes(&fake, &table_changes());
    assert_eq!(ui.cached_rows(), 0);
    draw(&mut ui, &fake, 100, 30);
    assert!(fake.rows_requested.get() > total);
}

#[test]
fn search_bar_completes_validates_and_records_history() {
    let fake = Fake::new(4);
    let mut ui = open(&fake);
    ui.on_key(&fake, ch('/'));
    assert_eq!(ui.focus(), Focus::Search);
    // Start from an empty input.
    ui.on_key(&fake, ctrl('u'));
    type_text(&mut ui, &fake, "tag:le");
    let (text, _) = draw(&mut ui, &fake, 100, 14);
    assert!(text[0].starts_with("Search: tag:le ") && text[0].ends_with("[Cards]"));
    assert!(text.iter().any(|l| l.contains("│tag:leech")), "{text:?}");
    ui.on_key(&fake, key(KeyCode::Enter)); // accept the completion
    let intent = ui.on_key(&fake, key(KeyCode::Enter));
    assert_eq!(intent, BrowserIntent::SearchCommitted("tag:leech".into()));
    assert_eq!(ui.query(), "tag:leech");
    assert_eq!(ui.focus(), Focus::Table);
    // A parse error keeps the search bar open and shows Anki's message.
    ui.on_key(&fake, ch('/'));
    ui.on_key(&fake, ctrl('u'));
    type_text(&mut ui, &fake, "bad(");
    assert_eq!(ui.on_key(&fake, key(KeyCode::Enter)), BrowserIntent::None);
    assert_eq!(ui.focus(), Focus::Search);
    assert_eq!(ui.message(), Some("Missing a closing parenthesis"));
    let (text, _) = draw(&mut ui, &fake, 100, 14);
    assert!(text[12].contains("Missing a closing parenthesis"));
    // History with Up; Esc restores the shown query.
    ui.on_key(&fake, key(KeyCode::Up));
    assert_eq!(ui.search.text(), "tag:leech");
    ui.on_key(&fake, key(KeyCode::Esc));
    assert_eq!(ui.focus(), Focus::Table);
    assert_eq!(ui.search.text(), "tag:leech");
}

#[test]
fn selection_toggle_range_all_and_invert() {
    let fake = Fake::new(10);
    let mut ui = open(&fake);
    draw(&mut ui, &fake, 100, 16);
    press(&mut ui, &fake, &[ch(' '), ch(' ')]);
    assert_eq!(ui.targets(), [1, 2]);
    press(
        &mut ui,
        &fake,
        &[ch('v'), ch('j'), ch('j'), key(KeyCode::Esc)],
    );
    assert_eq!(ui.targets(), [1, 2, 3, 4, 5]);
    press(&mut ui, &fake, &[ch('*')]);
    assert_eq!(ui.targets(), [6, 7, 8, 9, 10]);
    press(&mut ui, &fake, &[ctrl('a')]);
    assert_eq!(ui.targets().len(), 10);
    let (text, _) = draw(&mut ui, &fake, 100, 16);
    assert_eq!(text[14], "10 cards shown, 10 selected");
    assert!(text[2].contains("•"));
    press(&mut ui, &fake, &[key(KeyCode::Esc)]);
    assert_eq!(ui.targets(), [5], "no selection: the current row");
    assert_eq!(ui.on_key(&fake, key(KeyCode::Esc)), BrowserIntent::Close);
}

#[test]
fn card_actions_use_phase_one_commands_with_toggles() {
    let fake = Fake::new(10);
    let mut ui = open(&fake);
    draw(&mut ui, &fake, 100, 16);
    // Row 2 has the red flag: pressing 1 there clears it.
    press(&mut ui, &fake, &[ch('j'), ch('1')]);
    assert!(matches!(
        fake.last().as_ref(),
        Some(Command::SetFlag { card_ids, flag: 0 }) if card_ids == &[2]
    ));
    press(&mut ui, &fake, &[ch('3')]);
    assert!(matches!(
        fake.last().as_ref(),
        Some(Command::SetFlag { flag: 3, .. })
    ));
    assert_eq!(ui.message(), Some("Flag 3 set on 1 card"));
    // Row 4 is suspended: `s` restores it; row 1 gets suspended.
    press(&mut ui, &fake, &[ch('j'), ch('j'), ch('s')]);
    assert!(matches!(
        fake.last_browse(),
        Some(BrowseCommand::Restore { card_ids }) if card_ids == [4]
    ));
    press(
        &mut ui,
        &fake,
        &[ch('g'), ch(' '), ch(' '), ch('k'), ch('k'), ch('s')],
    );
    assert!(matches!(
        fake.last().as_ref(),
        Some(Command::BuryOrSuspend { target: Target::Cards(ids), mode: BuryMode::Suspend }) if ids == &[1, 2]
    ));
    assert_eq!(ui.message(), Some("Suspended 2 cards"));
    press(&mut ui, &fake, &[ch('b')]);
    assert!(matches!(
        fake.last().as_ref(),
        Some(Command::BuryOrSuspend {
            mode: BuryMode::Bury,
            ..
        })
    ));
    // Mark: note of card 1 is unmarked -> marked, by note id.
    press(&mut ui, &fake, &[ch('m')]);
    assert!(matches!(
        fake.last().as_ref(),
        Some(Command::SetMarked { note_ids, marked: true }) if note_ids == &[1001]
    ));
    // Undo reports Anki's label.
    press(&mut ui, &fake, &[ch('u')]);
    assert_eq!(ui.message(), Some("Undone: Set Flag"));
}

#[test]
fn prompts_for_deck_tags_due_reposition_forget_and_grade() {
    let fake = Fake::new(6);
    let mut ui = open(&fake);
    draw(&mut ui, &fake, 100, 16);
    // Change deck with completion over deck names.
    press(&mut ui, &fake, &[ch('c')]);
    type_text(&mut ui, &fake, "fr");
    ui.on_key(&fake, key(KeyCode::Tab));
    ui.on_key(&fake, key(KeyCode::Enter));
    let (text, _) = draw(&mut ui, &fake, 100, 16);
    assert!(text.iter().any(|l| l.contains("Lang::French")), "{text:?}");
    ui.on_key(&fake, key(KeyCode::Enter));
    assert!(matches!(
        fake.last_browse(),
        Some(BrowseCommand::SetDeckByName { card_ids, name }) if card_ids == [1] && name == "Lang::French"
    ));
    assert_eq!(ui.message(), Some("Moved 1 card to Lang::French"));
    // Add and remove tags by note id.
    press(&mut ui, &fake, &[ch('t')]);
    type_text(&mut ui, &fake, "new one");
    ui.on_key(&fake, key(KeyCode::Enter));
    assert!(matches!(
        fake.logged(|c| matches!(c, Command::Notes(_))).last(),
        Some(Command::Notes(NoteCommand::AddTags { note_ids, tags })) if note_ids == &[1001] && tags == "new one"
    ));
    // Tag changes reload the sidebar (and its completion names).
    assert!(matches!(
        fake.last(),
        Some(Command::Browse(BrowseCommand::Sidebar))
    ));
    press(&mut ui, &fake, &[ch('T')]);
    type_text(&mut ui, &fake, "leech");
    ui.on_key(&fake, key(KeyCode::Enter));
    assert!(matches!(
        fake.logged(|c| matches!(c, Command::Notes(_))).last(),
        Some(Command::Notes(NoteCommand::RemoveTags { tags, .. })) if tags == "leech"
    ));
    press(&mut ui, &fake, &[ch('U')]);
    assert_eq!(ui.message(), Some("Removed 2 unused tags"));
    // Set due date prefilled with the browser default.
    press(&mut ui, &fake, &[ch('d')]);
    let (text, _) = draw(&mut ui, &fake, 100, 16);
    assert!(text.iter().any(|l| l.contains("Set due date")));
    ui.on_key(&fake, key(KeyCode::Backspace));
    type_text(&mut ui, &fake, "3-7");
    ui.on_key(&fake, key(KeyCode::Enter));
    assert!(matches!(
        fake.last_browse(),
        Some(BrowseCommand::SetDueDate { days, .. }) if days == "3-7"
    ));
    // Reposition: defaults from Anki, edited start.
    press(&mut ui, &fake, &[ch('r')]);
    let (text, _) = draw(&mut ui, &fake, 100, 16);
    // The 52-column box centred in 100 columns.
    let dialog: Vec<String> = text[5..11]
        .iter()
        .map(|l| l.chars().skip(24).take(52).collect())
        .collect();
    assert_eq!(
        dialog,
        [
            "┌ Reposition 1 new card(s) ────────────────────────┐",
            "│> Start position 0                                │",
            "│  Step           1                                │",
            "│  [ ] Randomize order                             │",
            "│  [x] Shift position of existing cards            │",
            "└──────────────────────────────────────────────────┘",
        ]
    );
    ui.on_key(&fake, key(KeyCode::Backspace));
    type_text(&mut ui, &fake, "10");
    ui.on_key(&fake, key(KeyCode::Enter));
    assert!(matches!(
        fake.last_browse(),
        Some(BrowseCommand::Reposition {
            start: 10,
            step: 1,
            randomize: false,
            shift: true,
            ..
        })
    ));
    // Forget with the browser's defaults, one box toggled.
    press(
        &mut ui,
        &fake,
        &[ch('f'), key(KeyCode::Down), ch(' '), key(KeyCode::Enter)],
    );
    assert!(matches!(
        fake.last_browse(),
        Some(BrowseCommand::Forget {
            restore_position: true,
            reset_counts: true,
            ..
        })
    ));
    // Grade now by digit.
    press(&mut ui, &fake, &[ch('n'), ch('4')]);
    assert!(matches!(
        fake.last_browse(),
        Some(BrowseCommand::GradeNow {
            rating: Rating::Easy,
            ..
        })
    ));
}

#[test]
fn find_and_replace_always_passes_explicit_note_ids() {
    let fake = Fake::new(6);
    let mut ui = open(&fake);
    draw(&mut ui, &fake, 100, 16);
    press(&mut ui, &fake, &[ch(' '), ch(' '), ch(' '), ch('R')]);
    type_text(&mut ui, &fake, "cat");
    ui.on_key(&fake, key(KeyCode::Down));
    type_text(&mut ui, &fake, "dog");
    // Scope: All fields -> Tags -> Back.
    press(
        &mut ui,
        &fake,
        &[key(KeyCode::Down), key(KeyCode::Right), key(KeyCode::Right)],
    );
    press(
        &mut ui,
        &fake,
        &[key(KeyCode::Down), ch(' '), key(KeyCode::Down), ch(' ')],
    );
    let (text, _) = draw(&mut ui, &fake, 100, 16);
    assert!(
        text.iter().any(|l| l.contains("In:      ‹ Back ›")),
        "{text:?}"
    );
    assert!(text.iter().any(|l| l.contains("In 2 selected note(s)")));
    ui.on_key(&fake, key(KeyCode::Enter));
    let Some(BrowseCommand::FindReplace(f)) = fake.last_browse() else {
        panic!("{:?}", fake.last_browse())
    };
    assert_eq!(
        f,
        FindReplace {
            note_ids: vec![1001, 1002],
            search: "cat".into(),
            replacement: "dog".into(),
            regex: true,
            match_case: true,
            scope: FindReplaceScope::Field("Back".into()),
        }
    );
    assert_eq!(ui.message(), Some("2 notes updated"));
}

#[test]
fn delete_asks_first_and_refreshes_the_search() {
    let fake = Fake::new(6);
    let mut ui = open(&fake);
    draw(&mut ui, &fake, 100, 16);
    press(&mut ui, &fake, &[ch('D')]);
    let (text, _) = draw(&mut ui, &fake, 100, 16);
    assert!(text
        .iter()
        .any(|l| l.contains("Delete 1 note and all their cards?")));
    press(&mut ui, &fake, &[ch('n')]);
    assert!(fake
        .logged(|c| matches!(c, Command::DeleteNotes { .. }))
        .is_empty());
    press(&mut ui, &fake, &[ch('D'), ch('y')]);
    assert!(matches!(
        fake.last().as_ref(),
        Some(Command::Browse(BrowseCommand::Search { .. }))
    ));
    assert_eq!(ui.ids(), [3, 4, 5, 6]);
    assert_eq!(ui.current(), Some(3));
    assert_eq!(ui.message(), Some("Deleted 1 note"));
}

#[test]
fn mode_toggle_converts_selection_and_saves_the_setting() {
    let fake = Fake::new(6);
    let mut ui = open(&fake);
    draw(&mut ui, &fake, 100, 16);
    press(&mut ui, &fake, &[ch('j'), ch('j'), ch(' '), ch('M')]);
    assert_eq!(ui.mode(), BrowseMode::Notes);
    assert_eq!(fake.settings.borrow().mode, BrowseMode::Notes);
    assert_eq!(ui.ids(), [1001, 1002, 1003]);
    assert_eq!(ui.targets(), [1002]);
    assert_eq!(ui.current(), Some(1002));
    let (text, _) = draw(&mut ui, &fake, 100, 16);
    assert!(text[0].ends_with("[Notes]"));
    assert!(text[1].contains("Note") && text[1].contains("Cards") && text[1].contains("Tags"));
    assert!(text[14].starts_with("3 notes shown, 1 selected │ Showing notes"));
    // Note actions in notes mode go straight through; card actions expand.
    press(&mut ui, &fake, &[ch('s')]);
    assert!(matches!(
        fake.last().as_ref(),
        Some(Command::Browse(BrowseCommand::Restore { card_ids })) if card_ids == &[3, 4]
    ));
    assert_eq!(
        press(&mut ui, &fake, &[key(KeyCode::Enter)]),
        BrowserIntent::EditNotes(vec![1002])
    );
    press(&mut ui, &fake, &[ch('M')]);
    assert_eq!(ui.mode(), BrowseMode::Cards);
    assert_eq!(ui.targets(), [3, 4]);
}

#[test]
fn sorting_by_column_and_reversing_in_memory() {
    let fake = Fake::new(5);
    let mut ui = open(&fake);
    draw(&mut ui, &fake, 100, 16);
    press(&mut ui, &fake, &[ch('S')]);
    let (text, _) = draw(&mut ui, &fake, 100, 16);
    assert!(
        text.iter().any(|l| l.contains("1 Sort Field ▲")),
        "{text:?}"
    );
    // Due defaults to descending.
    press(&mut ui, &fake, &[ch('3')]);
    assert_eq!(fake.settings.borrow().card_sort, "cardDue");
    assert!(fake.settings.borrow().card_sort_reverse);
    assert!(matches!(
        fake.last_browse(),
        Some(BrowseCommand::Search { order: SearchOrder::Column { key, reverse: true }, .. }) if key == "cardDue"
    ));
    assert_eq!(ui.ids(), [5, 4, 3, 2, 1]);
    // Same column again: reversed without a new search.
    let searches = fake
        .logged(|c| matches!(c, Command::Browse(BrowseCommand::Search { .. })))
        .len();
    press(&mut ui, &fake, &[ch('S'), ch('3')]);
    assert_eq!(ui.ids(), [1, 2, 3, 4, 5]);
    assert!(!fake.settings.borrow().card_sort_reverse);
    assert_eq!(
        fake.logged(|c| matches!(c, Command::Browse(BrowseCommand::Search { .. })))
            .len(),
        searches
    );
    let (text, _) = draw(&mut ui, &fake, 100, 16);
    assert!(text[1].contains("Due ▲"));
    // An unsortable column is refused.
    let mut settings = fake.settings.borrow().clone();
    settings.card_columns.push("question".into());
    ui.settings = settings;
    press(&mut ui, &fake, &[ch('S'), ch('5')]);
    assert_eq!(
        ui.message(),
        Some("Sorting on this column is not supported")
    );
}

#[test]
fn column_chooser_adds_and_removes_columns() {
    let fake = Fake::new(3);
    let mut ui = open(&fake);
    draw(&mut ui, &fake, 100, 16);
    press(&mut ui, &fake, &[ch('o')]);
    let (text, _) = draw(&mut ui, &fake, 100, 16);
    assert!(text.iter().any(|l| l.contains("[x] Card")));
    assert!(text.iter().any(|l| l.contains("[ ] Question")));
    // Items sorted by label: Card, Deck, Due, Note, Question, Sort Field, Tags.
    press(
        &mut ui,
        &fake,
        &[ch('j'), ch('j'), ch('j'), ch('j'), ch(' ')],
    );
    assert_eq!(
        fake.settings.borrow().card_columns,
        ["noteFld", "template", "cardDue", "deck", "question"]
    );
    press(&mut ui, &fake, &[ch('k'), ch('k'), ch('k'), ch(' ')]);
    assert_eq!(
        fake.settings.borrow().card_columns,
        ["noteFld", "template", "cardDue", "question"]
    );
    press(&mut ui, &fake, &[key(KeyCode::Esc)]);
    assert!(!ui.has_dialog());
    let (text, _) = draw(&mut ui, &fake, 100, 16);
    assert!(text[1].contains("Question"));
    assert!(
        text[2].contains("question"),
        "rows refetched with the new columns"
    );
}

#[test]
fn sidebar_searches_with_modifiers_and_persists_collapse() {
    let fake = Fake::new(4);
    let mut ui = open(&fake);
    draw(&mut ui, &fake, 100, 40);
    ui.on_key(&fake, key(KeyCode::Tab));
    assert_eq!(ui.focus(), Focus::Sidebar);
    let index = |ui: &BrowserUi, label: &str| {
        ui.sidebar_rows
            .iter()
            .position(|r| r.label == label)
            .unwrap()
    };
    ui.sidebar_cursor = index(&ui, "French");
    assert_eq!(
        ui.on_key(&fake, key(KeyCode::Enter)),
        BrowserIntent::SearchCommitted("deck:Lang::French".into())
    );
    assert_eq!(ui.focus(), Focus::Sidebar);
    ui.sidebar_cursor = index(&ui, "leech");
    ui.on_key(&fake, ch('a'));
    assert_eq!(ui.query(), "deck:Lang::French tag:leech");
    ui.sidebar_cursor = index(&ui, "Red");
    ui.on_key(&fake, ch('n'));
    ui.on_key(&fake, ch('o'));
    assert!(matches!(
        fake.logged(|c| matches!(c, Command::Browse(BrowseCommand::JoinSearch { .. })))
            .last(),
        Some(Command::Browse(BrowseCommand::JoinSearch {
            joiner: SearchJoiner::Or,
            negate: true,
            ..
        }))
    ));
    assert_eq!(ui.query(), "deck:Lang::French tag:leech OR -flag:1");
    ui.sidebar_cursor = index(&ui, "Default");
    ui.on_key(&fake, ch('r'));
    assert_eq!(ui.query(), "deck:Default");
    ui.on_key(&fake, ch('n'));
    ui.on_key(&fake, key(KeyCode::Enter));
    assert!(matches!(
        fake.logged(|c| matches!(c, Command::Browse(BrowseCommand::NormalizeSearch { .. }))).last(),
        Some(Command::Browse(BrowseCommand::NormalizeSearch { query })) if query == "-(deck:Default)"
    ));
    // Folding a deck is stored in the collection (browser scope).
    ui.sidebar_cursor = index(&ui, "Lang");
    ui.on_key(&fake, ch(' '));
    assert!(matches!(
        fake.last_browse(),
        Some(BrowseCommand::SetDeckCollapsed {
            deck_id: 5,
            collapsed: true
        })
    ));
    assert!(!ui.sidebar_rows.iter().any(|r| r.label == "French"));
    // Sections fold at once and the state is stored (desktop's keys).
    ui.sidebar_cursor = index(&ui, "Today");
    ui.on_key(&fake, ch('h'));
    assert!(!ui.sidebar_rows.iter().any(|r| r.label == "Due Today"));
    assert!(matches!(
        fake.last_browse(),
        Some(BrowseCommand::SetSectionCollapsed {
            section: 1,
            collapsed: true
        })
    ));
    assert_eq!(
        *fake.sections.borrow(),
        [false, true, false, false, false, false, false]
    );
    ui.sidebar_cursor = index(&ui, "leech");
    ui.on_key(&fake, ch('h'));
    assert_eq!(ui.sidebar_rows[ui.sidebar_cursor].label, "Tags");
    let (text, _) = draw(&mut ui, &fake, 100, 40);
    assert!(text.iter().any(|l| l.starts_with("▸ Today")));
    // Items without a search say so.
    ui.sidebar_cursor = index(&ui, "Today");
    ui.on_key(&fake, key(KeyCode::Enter));
    assert_eq!(ui.message(), Some("This item has no search"));
}

#[test]
fn saved_searches_are_saved_and_deleted() {
    let fake = Fake::new(4);
    let mut ui = open(&fake);
    draw(&mut ui, &fake, 100, 20);
    press(&mut ui, &fake, &[ch('w')]);
    type_text(&mut ui, &fake, "Current");
    ui.on_key(&fake, key(KeyCode::Enter));
    assert_eq!(
        *fake.saved.borrow(),
        [
            ("Due cards".to_string(), "is:due".to_string()),
            ("Current".to_string(), "deck:current".to_string())
        ]
    );
    assert!(ui.sidebar_rows.iter().any(|r| r.label == "Current"));
    ui.on_key(&fake, key(KeyCode::Tab));
    ui.sidebar_cursor = 1;
    ui.on_key(&fake, ch('x'));
    ui.on_key(&fake, ch('y'));
    assert_eq!(
        *fake.saved.borrow(),
        [("Current".to_string(), "deck:current".to_string())]
    );
}

#[test]
fn preview_pane_renders_the_current_card() {
    let fake = Fake::new(3);
    let mut ui = open(&fake);
    draw(&mut ui, &fake, 120, 20);
    press(&mut ui, &fake, &[ch('j'), ch('p')]);
    let (text, _) = draw(&mut ui, &fake, 120, 20);
    assert!(
        text.iter().any(|l| l.contains("┌ Preview: question")),
        "{text:?}"
    );
    let line = text
        .iter()
        .find(|l| l.contains("│Question 2"))
        .expect("question");
    assert!(!line.contains("[[type"), "type-answer placeholder removed");
    assert!(matches!(
        fake.logged(|c| matches!(c, Command::Render { .. })).last(),
        Some(Command::Render { card_id: 2 })
    ));
    ui.on_key(&fake, ch('P'));
    let (text, _) = draw(&mut ui, &fake, 120, 20);
    assert!(text.iter().any(|l| l.contains("Preview: answer")));
    assert!(text.iter().any(|l| l.contains("Answer 2")), "{text:?}");
    // Moving the cursor renders the new card once.
    let renders = fake.logged(|c| matches!(c, Command::Render { .. })).len();
    press(&mut ui, &fake, &[ch('j')]);
    draw(&mut ui, &fake, 120, 20);
    draw(&mut ui, &fake, 120, 20);
    assert_eq!(
        fake.logged(|c| matches!(c, Command::Render { .. })).len(),
        renders + 1
    );
    assert_eq!(ui.on_key(&fake, ch('i')), BrowserIntent::CardInfo(3));
    assert_eq!(ui.on_key(&fake, ch('V')), BrowserIntent::Preview(3));
    assert_eq!(
        ui.on_key(&fake, ch('F')),
        BrowserIntent::FilteredDeck("deck:current".into())
    );
}

#[test]
fn helpers_fit_text_and_share_widths() {
    assert_eq!(fit("abcdef", 4, Elide::Right, false), "abc…");
    assert_eq!(fit("abcdef", 4, Elide::Left, false), "…def");
    assert_eq!(fit("abcdefg", 5, Elide::Middle, false), "ab…fg");
    assert_eq!(fit("ab", 4, Elide::Right, true), " ab ");
    assert_eq!(fit("日本語", 4, Elide::Right, false), "日… ");
    let keys: Vec<String> = ["noteFld", "cardDue", "question"]
        .map(String::from)
        .to_vec();
    assert_eq!(column_widths(&keys, 52), [20, 10, 20]);
    assert_eq!(strip_type_answer("a [[type:Back]] b"), "a  b");
}

#[test]
fn stored_section_state_and_default_search_apply_on_open() {
    let fake = Fake::new(4);
    *fake.sections.borrow_mut() = [true, false, false, false, false, false, true];
    fake.settings.borrow_mut().default_search = "  tag:leech ".into();
    let ui = open(&fake);
    assert_eq!(ui.query(), "tag:leech");
    let label = |l: &str| ui.sidebar_rows.iter().find(|r| r.label == l).cloned();
    assert!(label("Saved Searches").unwrap().collapsed);
    assert!(label("Due cards").is_none());
    assert!(label("Tags").unwrap().collapsed);
    assert!(label("Untagged").is_none());
    assert!(!label("Today").unwrap().collapsed);
    // An empty default means the current deck.
    fake.settings.borrow_mut().default_search = String::new();
    let ui = open(&fake);
    assert_eq!(ui.query(), "deck:current");
}
