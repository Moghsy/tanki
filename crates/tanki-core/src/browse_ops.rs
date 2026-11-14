//! Browser stream (Phase 3): search, browser table rows, settings under
//! desktop's config keys, sidebar data, bulk card/note actions and find and
//! replace. Never uses Anki's custom (raw SQL) sort and never passes an empty
//! id list to find and replace (PLAN 14 rule 11).
use crate::actor::changes;
use crate::edit_convert::{all_notetypes, tag_node};
use crate::study::{flag_names, plain};
use crate::*;
use anki::{
    collection::Collection,
    error::AnkiError,
    services::{
        CardsService, ConfigService, DecksService, NotesService, SchedulerService, SearchService,
        TagsService,
    },
};
use anki_proto as pb;
use pb::config::config_key::{Bool as BoolKey, String as StringKey};
use pb::search::browser_columns::Sorting;

// Desktop's config keys (`pylib/anki/browser.py` `BrowserConfig`, R6 §2.2).
const ACTIVE_CARD_COLUMNS: &str = "activeCols";
const ACTIVE_NOTE_COLUMNS: &str = "activeNoteCols";
const CARD_SORT: &str = "sortType";
const NOTE_SORT: &str = "noteSortType";
const CARD_SORT_BACKWARDS: &str = "sortBackwards";
const NOTE_SORT_BACKWARDS: &str = "browserNoteSortBackwards";
/// `qt/aqt/browser/sidebar/tree.py`: `{name: search}`.
const SAVED_SEARCHES: &str = "savedFilters";
/// `BrowserDefaults` in `pylib/anki/browser.py`.
const DEFAULT_CARD_COLUMNS: [&str; 4] = ["noteFld", "template", "cardDue", "deck"];
const DEFAULT_NOTE_COLUMNS: [&str; 4] = ["noteFld", "note", "template", "noteTags"];
/// `table/model.py` `_search_inner`: an unset or unknown sort column falls
/// back to the creation time.
const FALLBACK_SORT: &str = "noteCrt";
/// The sidebar sections' collapse keys in `SidebarStage` order
/// (`qt/aqt/browser/sidebar/tree.py`, `_section_root(collapse_key=...)`).
const SECTION_KEYS: [BoolKey; 7] = [
    BoolKey::CollapseSavedSearches,
    BoolKey::CollapseToday,
    BoolKey::CollapseFlags,
    BoolKey::CollapseCardState,
    BoolKey::CollapseDecks,
    BoolKey::CollapseNotetypes,
    BoolKey::CollapseTags,
];

pub(crate) fn execute(col: &mut Collection, command: BrowseCommand) -> Result<EditResponse> {
    use EditResponse as R;
    Ok(match command {
        BrowseCommand::Search { query, mode, order } => R::Ids(search(col, &query, mode, order)?),
        BrowseCommand::NormalizeSearch { query } => R::Text(searching(
            SearchService::build_search_string(col, text_node(query)),
        )?),
        BrowseCommand::JoinSearch {
            existing,
            additional,
            joiner,
            negate,
        } => {
            use pb::search::search_node::group::Joiner;
            let additional = if negate {
                negated(text_node(additional))
            } else {
                text_node(additional)
            };
            let joiner = match joiner {
                SearchJoiner::And => Joiner::And,
                SearchJoiner::Or => Joiner::Or,
            };
            R::Text(searching(SearchService::join_search_nodes(
                col,
                pb::search::JoinSearchNodesRequest {
                    joiner: joiner as i32,
                    existing_node: Some(text_node(existing)),
                    additional_node: Some(additional),
                },
            ))?)
        }
        BrowseCommand::ReplaceSearch {
            existing,
            replacement,
        } => R::Text(searching(SearchService::replace_search_node(
            col,
            pb::search::ReplaceSearchNodeRequest {
                existing_node: Some(text_node(existing)),
                replacement_node: Some(text_node(replacement)),
            },
        ))?),
        BrowseCommand::Columns => R::Columns(columns(col)?),
        BrowseCommand::Settings => R::Settings(settings(col)?),
        BrowseCommand::SetSettings(new) => R::Changes(set_settings(col, new)?),
        BrowseCommand::Rows { mode, columns, ids } => R::Rows(rows(col, mode, columns, ids)?),
        BrowseCommand::Sidebar => R::Sidebar(sidebar(col)?),
        BrowseCommand::SetDeckCollapsed { deck_id, collapsed } => {
            R::Changes(changes(DecksService::set_deck_collapsed(
                col,
                pb::decks::SetDeckCollapsedRequest {
                    deck_id,
                    collapsed,
                    scope: pb::decks::set_deck_collapsed_request::Scope::Browser as i32,
                },
            )?))
        }
        BrowseCommand::SetTagCollapsed { name, collapsed } => {
            R::Changes(changes(TagsService::set_tag_collapsed(
                col,
                pb::tags::SetTagCollapsedRequest { name, collapsed },
            )?))
        }
        BrowseCommand::SetSectionCollapsed { section, collapsed } => {
            let key = SECTION_KEYS.get(section as usize).copied().ok_or_else(|| {
                CoreError::Invalid(format!("There is no sidebar section {section}"))
            })?;
            R::Changes(set_bool(col, key, collapsed)?)
        }
        BrowseCommand::SetSavedSearches { searches } => {
            let map: serde_json::Map<String, serde_json::Value> = searches
                .into_iter()
                .map(|(name, search)| (name, serde_json::Value::String(search)))
                .collect();
            R::Changes(set_json(
                col,
                SAVED_SEARCHES,
                serde_json::Value::Object(map),
            )?)
        }
        BrowseCommand::CardsOfNotes { note_ids } => R::Ids(cards_of_notes(col, &note_ids)?),
        BrowseCommand::NotesOfCards { card_ids } => R::Ids(notes_of_cards(col, &card_ids)?),
        BrowseCommand::SetDeck { card_ids, deck_id } => count(CardsService::set_deck(
            col,
            pb::cards::SetDeckRequest { card_ids, deck_id },
        )?),
        BrowseCommand::SetDeckByName { card_ids, name } => set_deck_by_name(col, card_ids, &name)?,
        BrowseCommand::Restore { card_ids } => R::Changes(changes(
            SchedulerService::restore_buried_and_suspended_cards(
                col,
                pb::cards::CardIds { cids: card_ids },
            )?,
        )),
        BrowseCommand::RepositionDefaults => {
            let d = SchedulerService::reposition_defaults(col)?;
            R::RepositionDefaults(RepositionDefaults {
                random: d.random,
                shift: d.shift,
            })
        }
        BrowseCommand::Reposition {
            card_ids,
            start,
            step,
            randomize,
            shift,
        } => {
            let (Ok(starting_from), Ok(step_size)) = (u32::try_from(start), u32::try_from(step))
            else {
                return Err(CoreError::Invalid(
                    "The start position and step must not be negative".into(),
                ));
            };
            count(SchedulerService::sort_cards(
                col,
                pb::scheduler::SortCardsRequest {
                    card_ids,
                    starting_from,
                    step_size,
                    randomize,
                    shift_existing: shift,
                },
            )?)
        }
        BrowseCommand::ForgetDefaults => {
            let d = SchedulerService::schedule_cards_as_new_defaults(
                col,
                pb::scheduler::ScheduleCardsAsNewDefaultsRequest {
                    context: BROWSER as i32,
                },
            )?;
            R::ForgetDefaults(ForgetDefaults {
                restore_position: d.restore_position,
                reset_counts: d.reset_counts,
            })
        }
        BrowseCommand::Forget {
            card_ids,
            restore_position,
            reset_counts,
        } => R::Changes(changes(SchedulerService::schedule_cards_as_new(
            col,
            pb::scheduler::ScheduleCardsAsNewRequest {
                card_ids,
                log: true,
                restore_position,
                reset_counts,
                context: Some(BROWSER as i32),
            },
        )?)),
        BrowseCommand::SetDueDefault => R::Text(
            ConfigService::get_config_string(
                col,
                pb::config::GetConfigStringRequest {
                    key: StringKey::SetDueBrowser as i32,
                },
            )?
            .val,
        ),
        BrowseCommand::SetDueDate { card_ids, days } => {
            let result = SchedulerService::set_due_date(
                col,
                pb::scheduler::SetDueDateRequest {
                    card_ids,
                    days,
                    config_key: Some(pb::config::OptionalStringConfigKey {
                        key: StringKey::SetDueBrowser as i32,
                    }),
                },
            );
            // A malformed `days` is invalid input; keep Anki's message.
            R::Changes(changes(result.map_err(|error| match error {
                AnkiError::InvalidInput { .. } => CoreError::Invalid(error.message(col.tr())),
                other => other.into(),
            })?))
        }
        BrowseCommand::GradeNow { card_ids, rating } => {
            use pb::scheduler::card_answer::Rating as R2;
            let rating = match rating {
                Rating::Again => R2::Again,
                Rating::Hard => R2::Hard,
                Rating::Good => R2::Good,
                Rating::Easy => R2::Easy,
            };
            R::Changes(changes(SchedulerService::grade_now(
                col,
                pb::scheduler::GradeNowRequest {
                    card_ids,
                    rating: rating as i32,
                },
            )?))
        }
        BrowseCommand::FieldNamesForNotes { note_ids } => R::Strings(
            NotesService::field_names_for_notes(
                col,
                pb::notes::FieldNamesForNotesRequest { nids: note_ids },
            )?
            .fields,
        ),
        BrowseCommand::FindReplace(request) => find_replace(col, request)?,
    })
}

const BROWSER: pb::scheduler::schedule_cards_as_new_request::Context =
    pb::scheduler::schedule_cards_as_new_request::Context::Browser;

fn count(out: pb::collection::OpChangesWithCount) -> EditResponse {
    EditResponse::Count {
        count: out.count as usize,
        changes: changes(out.changes.unwrap_or_default()),
    }
}

/// Search syntax errors (and invalid regexes) become `CoreError::Invalid`
/// with Anki's message in the `From<AnkiError>` conversion.
fn searching(result: std::result::Result<pb::generic::String, AnkiError>) -> Result<String> {
    Ok(result.map(|s| s.val)?)
}

fn text_node(text: String) -> pb::search::SearchNode {
    pb::search::SearchNode {
        filter: Some(pb::search::search_node::Filter::ParsableText(text)),
    }
}

fn negated(node: pb::search::SearchNode) -> pb::search::SearchNode {
    pb::search::SearchNode {
        filter: Some(pb::search::search_node::Filter::Negated(Box::new(node))),
    }
}

fn all_columns(col: &mut Collection) -> Result<Vec<pb::search::browser_columns::Column>> {
    Ok(SearchService::all_browser_columns(col)?.columns)
}

fn sorting(sorting: i32) -> Option<SortDirection> {
    match Sorting::try_from(sorting).unwrap_or(Sorting::None) {
        Sorting::None => None,
        Sorting::Ascending => Some(SortDirection::Ascending),
        Sorting::Descending => Some(SortDirection::Descending),
    }
}

fn columns(col: &mut Collection) -> Result<Vec<ColumnInfo>> {
    use pb::search::browser_columns::Alignment;
    Ok(all_columns(col)?
        .into_iter()
        .map(|c| ColumnInfo {
            sort_cards: sorting(c.sorting_cards),
            sort_notes: sorting(c.sorting_notes),
            centered: c.alignment == Alignment::Center as i32,
            key: c.key,
            cards_label: c.cards_mode_label,
            notes_label: c.notes_mode_label,
        })
        .collect())
}

/// The column's default direction in `mode`, or `None` when it cannot be
/// sorted on (or is unknown).
fn sort_direction(
    all: &[pb::search::browser_columns::Column],
    key: &str,
    mode: BrowseMode,
) -> Option<SortDirection> {
    let column = all.iter().find(|c| c.key == key)?;
    sorting(match mode {
        BrowseMode::Cards => column.sorting_cards,
        BrowseMode::Notes => column.sorting_notes,
    })
}

fn search(
    col: &mut Collection,
    query: &str,
    mode: BrowseMode,
    order: SearchOrder,
) -> Result<Vec<i64>> {
    use pb::search::sort_order::{Builtin, Value};
    let builtin = |column: String, reverse: bool| Value::Builtin(Builtin { column, reverse });
    let none = Value::None(pb::generic::Empty {});
    let value = match order {
        SearchOrder::None => none,
        // `Collection._build_sort_mode(order=True)`: the config's column for
        // the mode; an unsortable one means no order.
        SearchOrder::Default => {
            let all = all_columns(col)?;
            let s = settings_with(col, &all)?;
            let (key, reverse) = match mode {
                BrowseMode::Cards => (s.card_sort, s.card_sort_reverse),
                BrowseMode::Notes => (s.note_sort, s.note_sort_reverse),
            };
            if sort_direction(&all, &key, mode).is_some() {
                builtin(key, reverse)
            } else {
                none
            }
        }
        SearchOrder::Column { key, reverse } => {
            let all = all_columns(col)?;
            if sort_direction(&all, &key, mode).is_none() {
                return Err(CoreError::Invalid(format!(
                    "The browser cannot sort on the column {key:?}"
                )));
            }
            builtin(key, reverse)
        }
    };
    let request = pb::search::SearchRequest {
        search: query.to_owned(),
        order: Some(pb::search::SortOrder { value: Some(value) }),
    };
    let result = match mode {
        BrowseMode::Cards => SearchService::search_cards(col, request),
        BrowseMode::Notes => SearchService::search_notes(col, request),
    };
    Ok(result.map(|r| r.ids)?)
}

// ---- config -----------------------------------------------------------------

/// A missing key is a NotFound error; treat it as absent.
fn get_json(col: &mut Collection, key: &str) -> Option<serde_json::Value> {
    ConfigService::get_config_json(col, key.to_owned().into())
        .ok()
        .and_then(|json| serde_json::from_slice(&json.json).ok())
}

/// `Collection.set_config(key, value)`: not undoable, as desktop.
fn set_json(col: &mut Collection, key: &str, value: serde_json::Value) -> Result<OpChanges> {
    Ok(changes(ConfigService::set_config_json(
        col,
        pb::config::SetConfigJsonRequest {
            key: key.into(),
            value_json: serde_json::to_vec(&value).map_err(|e| CoreError::Other(e.to_string()))?,
            undoable: false,
        },
    )?))
}

fn get_bool(col: &mut Collection, key: BoolKey) -> Result<bool> {
    Ok(
        ConfigService::get_config_bool(col, pb::config::GetConfigBoolRequest { key: key as i32 })?
            .val,
    )
}

fn set_bool(col: &mut Collection, key: BoolKey, value: bool) -> Result<OpChanges> {
    Ok(changes(ConfigService::set_config_bool(
        col,
        pb::config::SetConfigBoolRequest {
            key: key as i32,
            value,
            undoable: false,
        },
    )?))
}

fn string_list(value: Option<serde_json::Value>) -> Option<Vec<String>> {
    let items = value?.as_array()?.clone();
    items
        .into_iter()
        .map(|v| v.as_str().map(str::to_owned))
        .collect()
}

fn settings(col: &mut Collection) -> Result<BrowserSettings> {
    let all = all_columns(col)?;
    settings_with(col, &all)
}

/// Desktop's view of the keys: missing or malformed columns use
/// `BrowserDefaults`, an unset or unknown sort column is `noteCrt`, unset
/// directions are ascending.
fn settings_with(
    col: &mut Collection,
    all: &[pb::search::browser_columns::Column],
) -> Result<BrowserSettings> {
    let defaults = |keys: [&str; 4]| keys.iter().map(|k| (*k).to_owned()).collect::<Vec<_>>();
    let card_columns = string_list(get_json(col, ACTIVE_CARD_COLUMNS))
        .unwrap_or_else(|| defaults(DEFAULT_CARD_COLUMNS));
    let note_columns = string_list(get_json(col, ACTIVE_NOTE_COLUMNS))
        .unwrap_or_else(|| defaults(DEFAULT_NOTE_COLUMNS));
    let mut sort = |key: &str| {
        get_json(col, key)
            .and_then(|v| v.as_str().map(str::to_owned))
            .filter(|key| all.iter().any(|c| &c.key == key))
            .unwrap_or_else(|| FALLBACK_SORT.into())
    };
    let card_sort = sort(CARD_SORT);
    let note_sort = sort(NOTE_SORT);
    let mut backwards = |key: &str| {
        get_json(col, key)
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    };
    let card_sort_reverse = backwards(CARD_SORT_BACKWARDS);
    let note_sort_reverse = backwards(NOTE_SORT_BACKWARDS);
    let mode = if get_bool(col, BoolKey::BrowserTableShowNotesMode)? {
        BrowseMode::Notes
    } else {
        BrowseMode::Cards
    };
    let default_search = ConfigService::get_config_string(
        col,
        pb::config::GetConfigStringRequest {
            key: StringKey::DefaultSearchText as i32,
        },
    )?
    .val;
    Ok(BrowserSettings {
        default_search,
        mode,
        card_columns,
        note_columns,
        card_sort,
        card_sort_reverse,
        note_sort,
        note_sort_reverse,
    })
}

fn merge(into: &mut OpChanges, from: OpChanges) {
    let OpChanges {
        card,
        note,
        deck,
        tag,
        notetype,
        config,
        deck_config,
        mtime,
        browser_table,
        browser_sidebar,
        note_text,
        study_queues,
    } = from;
    into.card |= card;
    into.note |= note;
    into.deck |= deck;
    into.tag |= tag;
    into.notetype |= notetype;
    into.config |= config;
    into.deck_config |= deck_config;
    into.mtime |= mtime;
    into.browser_table |= browser_table;
    into.browser_sidebar |= browser_sidebar;
    into.note_text |= note_text;
    into.study_queues |= study_queues;
}

/// Writes only the keys whose stored value differs (desktop writes a key
/// when the user changes that setting), then sets Anki's active columns for
/// the mode, as desktop's `set_browser_*_columns` does.
fn set_settings(col: &mut Collection, new: BrowserSettings) -> Result<OpChanges> {
    if new.card_columns.is_empty() || new.note_columns.is_empty() {
        return Err(CoreError::Invalid(
            "The browser needs at least one column".into(),
        ));
    }
    let mut out = OpChanges::default();
    let notes_mode = new.mode == BrowseMode::Notes;
    if get_bool(col, BoolKey::BrowserTableShowNotesMode)? != notes_mode {
        merge(
            &mut out,
            set_bool(col, BoolKey::BrowserTableShowNotesMode, notes_mode)?,
        );
    }
    let json = |v: &[String]| serde_json::Value::from(v.to_vec());
    let pairs = [
        (ACTIVE_CARD_COLUMNS, json(&new.card_columns)),
        (ACTIVE_NOTE_COLUMNS, json(&new.note_columns)),
        (CARD_SORT, new.card_sort.clone().into()),
        (CARD_SORT_BACKWARDS, new.card_sort_reverse.into()),
        (NOTE_SORT, new.note_sort.clone().into()),
        (NOTE_SORT_BACKWARDS, new.note_sort_reverse.into()),
    ];
    for (key, value) in pairs {
        if get_json(col, key).as_ref() != Some(&value) {
            merge(&mut out, set_json(col, key, value)?);
        }
    }
    let active = if notes_mode {
        new.note_columns
    } else {
        new.card_columns
    };
    SearchService::set_active_browser_columns(col, pb::generic::StringList { vals: active })?;
    Ok(out)
}

// ---- rows -------------------------------------------------------------------

fn row_color(color: i32) -> RowColor {
    use pb::search::browser_row::Color;
    match Color::try_from(color).unwrap_or(Color::Default) {
        Color::Default => RowColor::Default,
        Color::Marked => RowColor::Marked,
        Color::Suspended => RowColor::Suspended,
        Color::Buried => RowColor::Buried,
        Color::FlagRed => RowColor::Flag(1),
        Color::FlagOrange => RowColor::Flag(2),
        Color::FlagGreen => RowColor::Flag(3),
        Color::FlagBlue => RowColor::Flag(4),
        Color::FlagPink => RowColor::Flag(5),
        Color::FlagTurquoise => RowColor::Flag(6),
        Color::FlagPurple => RowColor::Flag(7),
    }
}

fn elide(mode: i32) -> Elide {
    use pb::search::browser_row::cell::TextElideMode;
    match TextElideMode::try_from(mode).unwrap_or(TextElideMode::ElideRight) {
        TextElideMode::ElideLeft => Elide::Left,
        TextElideMode::ElideRight => Elide::Right,
        TextElideMode::ElideMiddle => Elide::Middle,
        TextElideMode::ElideNone => Elide::None,
    }
}

/// `BrowserRowForId` reads the notes-mode flag from the config and needs
/// the active columns set first; both are set here. The flag is only written
/// when it differs, as when desktop toggles the mode.
fn rows(
    col: &mut Collection,
    mode: BrowseMode,
    columns: Vec<String>,
    ids: Vec<i64>,
) -> Result<Vec<BrowserRow>> {
    let notes_mode = mode == BrowseMode::Notes;
    if get_bool(col, BoolKey::BrowserTableShowNotesMode)? != notes_mode {
        set_bool(col, BoolKey::BrowserTableShowNotesMode, notes_mode)?;
    }
    SearchService::set_active_browser_columns(col, pb::generic::StringList { vals: columns })?;
    ids.into_iter()
        .map(
            |id| match SearchService::browser_row_for_id(col, pb::generic::Int64 { val: id }) {
                Ok(row) => Ok(BrowserRow {
                    id,
                    cells: row
                        .cells
                        .into_iter()
                        .map(|c| BrowserCell {
                            text: c.text,
                            rtl: c.is_rtl,
                            elide: elide(c.elide_mode),
                        })
                        .collect(),
                    color: row_color(row.color),
                    error: None,
                }),
                // The row vanished, or its note lost every card: desktop
                // shows the message in a disabled row.
                Err(error @ (AnkiError::Deleted | AnkiError::DatabaseCheckRequired)) => {
                    Ok(BrowserRow {
                        id,
                        error: Some(plain(error.message(col.tr()))),
                        ..Default::default()
                    })
                }
                Err(error) => Err(error.into()),
            },
        )
        .collect()
}

// ---- sidebar ----------------------------------------------------------------

fn sidebar_deck(node: pb::decks::DeckTreeNode, parent: &str) -> SidebarDeck {
    let full_name = if parent.is_empty() {
        node.name.clone()
    } else {
        format!("{parent}::{}", node.name)
    };
    SidebarDeck {
        children: node
            .children
            .into_iter()
            .map(|child| sidebar_deck(child, &full_name))
            .collect(),
        id: node.deck_id,
        name: node.name,
        full_name,
        collapsed: node.collapsed,
        filtered: node.filtered,
    }
}

fn saved_searches(col: &mut Collection) -> Vec<(String, String)> {
    let mut saved: Vec<(String, String)> = get_json(col, SAVED_SEARCHES)
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(name, search)| Some((name, search.as_str()?.to_owned())))
        .collect();
    // `sorted(saved.items())` in `_saved_searches_tree`.
    saved.sort();
    saved
}

fn sidebar(col: &mut Collection) -> Result<SidebarData> {
    let saved_searches = saved_searches(col);
    let mut section_collapsed = [false; 7];
    for (collapsed, key) in section_collapsed.iter_mut().zip(SECTION_KEYS) {
        *collapsed = get_bool(col, key)?;
    }
    // `now: 0` = no counts, browser-scope collapse state.
    let tree = DecksService::deck_tree(col, pb::decks::DeckTreeRequest { now: 0 })?;
    let decks = tree
        .children
        .into_iter()
        .map(|d| sidebar_deck(d, ""))
        .collect();
    let tags = tag_node(TagsService::tag_tree(col)?, "");
    Ok(SidebarData {
        saved_searches,
        decks,
        tags,
        notetypes: all_notetypes(col)?,
        flag_names: flag_names(col),
        section_collapsed,
    })
}

// ---- bulk actions -----------------------------------------------------------

/// Cards of the notes, note by note in template order (`CardsOfNote`).
fn cards_of_notes(col: &mut Collection, note_ids: &[i64]) -> Result<Vec<i64>> {
    let mut out = Vec::new();
    for &nid in note_ids {
        out.extend(NotesService::cards_of_note(col, pb::notes::NoteId { nid })?.cids);
    }
    Ok(out)
}

/// Distinct notes of the cards, in the order their first card appears.
fn notes_of_cards(col: &mut Collection, card_ids: &[i64]) -> Result<Vec<i64>> {
    if card_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for &cid in card_ids {
        let note_id = match CardsService::get_card(col, pb::cards::CardId { cid }) {
            Ok(card) => card.note_id,
            Err(AnkiError::NotFound { .. }) => continue,
            Err(error) => return Err(error.into()),
        };
        if seen.insert(note_id) {
            out.push(note_id);
        }
    }
    Ok(out)
}

fn set_deck_by_name(col: &mut Collection, card_ids: Vec<i64>, name: &str) -> Result<EditResponse> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CoreError::Invalid("A deck name is required".into()));
    }
    let mut created = OpChanges::default();
    let deck_id = match DecksService::get_deck_id_by_name(col, name.to_owned().into()) {
        Ok(id) => id.did,
        Err(AnkiError::NotFound { .. }) => {
            let mut deck = DecksService::new_deck(col)?;
            deck.name = name.to_owned();
            let added = DecksService::add_deck(col, deck)?;
            created = changes(added.changes.unwrap_or_default());
            added.id
        }
        Err(error) => return Err(error.into()),
    };
    let moved = CardsService::set_deck(col, pb::cards::SetDeckRequest { card_ids, deck_id })?;
    let mut changed = changes(moved.changes.unwrap_or_default());
    merge(&mut changed, created);
    Ok(EditResponse::Count {
        count: moved.count as usize,
        changes: changed,
    })
}

fn find_replace(col: &mut Collection, request: FindReplace) -> Result<EditResponse> {
    // Anki treats an empty list as every note (R6 §10.2): refuse it here,
    // before any service is called.
    if request.note_ids.is_empty() {
        return Err(CoreError::Invalid(
            "Find and replace needs at least one note; select notes first.".into(),
        ));
    }
    let FindReplace {
        note_ids,
        search,
        replacement,
        regex,
        match_case,
        scope,
    } = request;
    let result = match scope {
        FindReplaceScope::Tags => TagsService::find_and_replace_tag(
            col,
            pb::tags::FindAndReplaceTagRequest {
                note_ids,
                search,
                replacement,
                regex,
                match_case,
            },
        ),
        scope => SearchService::find_and_replace(
            col,
            pb::search::FindAndReplaceRequest {
                nids: note_ids,
                search,
                replacement,
                regex,
                match_case,
                field_name: match scope {
                    FindReplaceScope::Field(name) => name,
                    _ => String::new(),
                },
            },
        ),
    };
    Ok(result.map(count)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_and_elision_map_every_proto_value() {
        assert_eq!(row_color(0), RowColor::Default);
        assert_eq!(row_color(1), RowColor::Marked);
        assert_eq!(row_color(2), RowColor::Suspended);
        assert_eq!(row_color(3), RowColor::Flag(1));
        assert_eq!(row_color(9), RowColor::Flag(7));
        assert_eq!(row_color(10), RowColor::Buried);
        assert_eq!(row_color(99), RowColor::Default);
        assert_eq!(elide(2), Elide::Middle);
        assert_eq!(elide(3), Elide::None);
    }
}
