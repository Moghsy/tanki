//! Filtered decks, custom study, unbury and deck descriptions (PLAN 8.2).
//!
//! Owned by the Decks tools stream (docs/implementation/phase-5-interfaces.md).
//! Reached through `Command::Manage(ManageCommand::DeckTools(…))`.
//!
//! Each command reproduces what the matching desktop dialog does around
//! Anki's calls: `filtered_deck.py` (load, seed searches, order list, save),
//! `customstudy.py` + `taglimit.py` (defaults, six modes, tag limit),
//! `deckdescription.py` and the deck browser's add / rename / delete / drag
//! and drop. Unbury per deck is `Command::Unbury` (study module).
use crate::{actor::changes, CoreError, OpChanges, Result};
use anki::{
    collection::Collection,
    error::AnkiError,
    services::{CardRenderingService, ConfigService, DecksService, SchedulerService},
};
use anki_proto as pb;
use pb::decks::deck::filtered::search_term::Order;
use serde::{Deserialize, Serialize};

/// `DYN_MAX_SIZE` (pylib `consts.py`): the largest limit desktop's forms take.
pub const MAX_LIMIT: u32 = 99_999;
/// Desktop's spin box limit for "forgotten in the last N days".
pub const MAX_FORGOT_DAYS: u32 = 30;
/// `TagLimit.accept`: more selected tags than this are refused.
pub const MAX_TAGS: usize = 100;

// ---------------------------------------------------------------------------
// Types

/// One search term of a filtered deck.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilteredTerm {
    pub search: String,
    pub limit: u32,
    /// Anki's `Deck.Filtered.SearchTerm.Order` value; one of
    /// [`FilteredDeckForm::orders`].
    pub order: i32,
}

/// A filtered deck as desktop's dialog edits it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilteredDeck {
    /// 0 for a deck that is not saved yet.
    pub id: i64,
    pub name: String,
    /// "Reschedule cards based on my answers in this deck". When off, the
    /// preview delays apply.
    pub reschedule: bool,
    pub first: FilteredTerm,
    /// Saved only when `second_enabled`.
    pub second: FilteredTerm,
    pub second_enabled: bool,
    /// Seconds; 0 returns the card to its home deck.
    pub preview_again_secs: u32,
    pub preview_hard_secs: u32,
    pub preview_good_secs: u32,
    /// Save even when the searches match nothing.
    pub allow_empty: bool,
}

/// An order the form offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderChoice {
    pub value: i32,
    pub label: String,
}

/// Anki's strings for the filtered deck form.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilteredLabels {
    /// "Options for <deck>".
    pub title: String,
    pub name: String,
    pub filter: String,
    pub filter_2: String,
    pub search: String,
    pub limit_to: String,
    pub cards_selected_by: String,
    pub enable_second_filter: String,
    pub reschedule: String,
    pub delay_again: String,
    pub delay_hard: String,
    pub delay_good: String,
    /// "(0 = return card to original deck)".
    pub zero_hint: String,
    /// "seconds", the unit of the three delays.
    pub seconds: String,
    pub allow_empty: String,
    /// "Build" for a new deck, "Rebuild" for an existing one.
    pub build: String,
}

/// -> `DeckToolsResponse::FilteredDeck`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilteredDeckForm {
    pub deck: FilteredDeck,
    /// Orders in Anki's order; the two retrievability orders only when FSRS
    /// is enabled (`FilteredDeckConfigDialog.FSRS_ONLY_ORDERS`).
    pub orders: Vec<OrderChoice>,
    pub labels: FilteredLabels,
}

/// "Study by card state or tag" kinds, in desktop's list order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CramKind {
    /// New cards in added order, rescheduled.
    New,
    /// Due cards in due order, rescheduled.
    Due,
    /// Review cards in random order, rescheduled.
    Review,
    /// All cards in random order, not rescheduled.
    All,
}

/// The six custom study modes (`CustomStudyRequest.value`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CustomStudy {
    /// Increase (or, negative, decrease) today's new card limit.
    NewLimitDelta(i32),
    ReviewLimitDelta(i32),
    /// Review cards forgotten in the last N days.
    ForgotDays(u32),
    ReviewAheadDays(u32),
    /// Preview new cards added in the last N days.
    PreviewDays(u32),
    Cram {
        kind: CramKind,
        card_limit: u32,
        /// Cards must have one of these, if not empty.
        tags_to_include: Vec<String>,
        tags_to_exclude: Vec<String>,
    },
}

/// A tag of the deck with the choice remembered from the last custom study.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StudyTag {
    pub name: String,
    pub include: bool,
    pub exclude: bool,
}

/// A count-dependent string: `one` for 1, `other` otherwise.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plural {
    pub one: String,
    pub other: String,
}

impl Plural {
    pub fn get(&self, count: i64) -> &str {
        if count == 1 {
            &self.one
        } else {
            &self.other
        }
    }
}

/// Anki's strings for the custom study dialog.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomStudyLabels {
    /// The six radio labels, in desktop's order.
    pub modes: [String; 6],
    /// Text before the number for each mode ("Increase today's new card
    /// limit by", …, "Select").
    pub before: [String; 6],
    pub cards: Plural,
    pub days: Plural,
    pub cards_from_the_deck: Plural,
    /// "Available new cards: 3 (2 in subdecks)".
    pub available_new: String,
    pub available_review: String,
    /// The four card-state choices, in [`CramKind`] order.
    pub cram_kinds: [String; 4],
    pub ok: String,
    pub choose_tags: String,
    /// "Selective Study".
    pub tags_title: String,
    pub require_tags: String,
    pub exclude_tags: String,
    /// "Custom Study Session".
    pub session_name: String,
}

/// -> `DeckToolsResponse::CustomStudyDefaults`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomStudyDefaults {
    pub deck_id: i64,
    pub deck_name: String,
    /// The deck's tags, sorted, with the remembered include/exclude choice.
    pub tags: Vec<StudyTag>,
    /// Last positive limit increases; desktop's spinner defaults.
    pub extend_new: u32,
    pub extend_review: u32,
    pub available_new: u32,
    pub available_review: u32,
    pub available_new_in_children: u32,
    pub available_review_in_children: u32,
    /// Cards in the deck and its subdecks; with 0, desktop only allows the
    /// two limit modes.
    pub card_count: u32,
    pub labels: CustomStudyLabels,
}

/// -> `DeckToolsResponse::Description`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeckDescription {
    pub deck_id: i64,
    pub name: String,
    /// The stored text (`desc`).
    pub text: String,
    /// The deck's `md` flag: the text is Markdown rather than HTML.
    pub markdown: bool,
    /// What the overview shows: Markdown converted (sanitised), HTML as is.
    pub html: String,
    /// "Description".
    pub title: String,
    /// "Interpret as Markdown".
    pub markdown_label: String,
}

/// A deck "Move to…" can make the parent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeckTarget {
    pub id: i64,
    /// The full name.
    pub name: String,
}

/// Anki's strings for the deck list's add, rename and delete prompts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeckPromptLabels {
    /// "Create Deck".
    pub create: String,
    /// "New deck name:".
    pub new_deck_name: String,
    /// "Rename Deck".
    pub rename: String,
    /// "New name:".
    pub new_name: String,
    /// "Delete Deck".
    pub delete: String,
}

/// -> `DeckToolsResponse::DeckPrompt`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeckPromptInfo {
    /// 0 when no deck was asked for (the add prompt).
    pub deck_id: i64,
    /// The full name; empty for deck 0.
    pub name: String,
    pub filtered: bool,
    /// Cards in the deck and its subdecks.
    pub card_count: u32,
    /// Subdecks at any depth.
    pub subdeck_count: u32,
    /// Normal decks that can become the parent, by name: not the deck, its
    /// subdecks or its present parent.
    pub targets: Vec<DeckTarget>,
    pub labels: DeckPromptLabels,
}

/// Each variant's doc comment names its `DeckToolsResponse` variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeckToolsCommand {
    /// `GetOrCreateFilteredDeck`: `deck_id` 0 gives an unsaved deck named
    /// "Filtered Deck HH:MM" with searches for the current deck. `search` /
    /// `search_2` replace the search texts (the browser's "filtered deck
    /// from search"); `search_2` also enables the second filter
    /// -> `FilteredDeck`.
    FilteredDeck {
        deck_id: i64,
        search: Option<String>,
        search_2: Option<String>,
    },
    /// `AddOrUpdateFilteredDeck`: saves and (re)builds, and makes the deck
    /// current. Bad searches and, unless `allow_empty`, an empty result are
    /// `CoreError::Invalid` with Anki's message and change nothing
    /// -> `Saved`.
    SaveFilteredDeck(FilteredDeck),
    /// -> `Count` (cards now in the deck).
    RebuildFilteredDeck { deck_id: i64 },
    /// Returns the cards to their home decks -> `Changes`.
    EmptyFilteredDeck { deck_id: i64 },
    /// -> `CustomStudyDefaults`. A filtered deck is `CoreError::Invalid`.
    CustomStudyDefaults { deck_id: i64 },
    /// `CustomStudy`. The four deck-building modes create or reuse "Custom
    /// Study Session" and make it current; `Saved::deck_id` is its id (0 for
    /// the two limit modes). "No cards matched" and "rename the existing
    /// deck" are `CoreError::Invalid` with Anki's message -> `Saved`.
    CustomStudy { deck_id: i64, study: CustomStudy },
    /// -> `Description`. A filtered deck is `CoreError::Invalid`.
    Description { deck_id: i64 },
    /// -> `Changes`.
    SetDescription {
        deck_id: i64,
        text: String,
        markdown: bool,
    },
    /// What the add / rename / move / delete prompts show; `deck_id` 0 gives
    /// the strings only -> `DeckPrompt`.
    DeckPrompt { deck_id: i64 },
    /// Adds a normal deck (`::` makes parents as needed) -> `Saved`.
    CreateDeck { name: String },
    /// `new_name` is the full name -> `Changes`.
    RenameDeck { deck_id: i64, new_name: String },
    /// Deletes the decks, their subdecks and their cards (a filtered deck's
    /// cards go home instead) -> `Deleted`.
    DeleteDecks { deck_ids: Vec<i64> },
    /// "Move to…": makes the decks children of `new_parent` (0 = top level)
    /// -> `Count` (decks moved).
    ReparentDecks { deck_ids: Vec<i64>, new_parent: i64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeckToolsResponse {
    FilteredDeck(Box<FilteredDeckForm>),
    CustomStudyDefaults(Box<CustomStudyDefaults>),
    Description(DeckDescription),
    Changes(OpChanges),
    Count {
        changes: OpChanges,
        count: usize,
    },
    Saved {
        changes: OpChanges,
        deck_id: i64,
    },
    DeckPrompt(Box<DeckPromptInfo>),
    /// `count` cards were deleted; `message` is Anki's "N cards deleted.".
    Deleted {
        changes: OpChanges,
        count: usize,
        message: String,
    },
}

impl DeckToolsResponse {
    /// The changes a mutating command made; the actor broadcasts these.
    pub fn changes(&self) -> Option<&OpChanges> {
        match self {
            Self::Changes(changes)
            | Self::Count { changes, .. }
            | Self::Saved { changes, .. }
            | Self::Deleted { changes, .. } => Some(changes),
            Self::FilteredDeck(_)
            | Self::CustomStudyDefaults(_)
            | Self::Description(_)
            | Self::DeckPrompt(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers

fn plain(text: impl AsRef<str>) -> String {
    anki_i18n::without_unicode_isolation(text.as_ref())
}

fn op_changes(c: Option<pb::collection::OpChanges>) -> OpChanges {
    changes(c.unwrap_or_default())
}

/// Errors the user can remedy in the dialog (bad search, nothing matched,
/// name clash, wrong kind of deck) become `Invalid` with Anki's localised
/// message, as desktop shows them in a warning box.
fn user_error(col: &Collection, e: AnkiError) -> CoreError {
    match &e {
        AnkiError::FilteredDeckError { .. }
        | AnkiError::CustomStudyError { .. }
        | AnkiError::SearchError { .. }
        | AnkiError::InvalidInput { .. }
        | AnkiError::NotFound { .. }
        | AnkiError::SchedulerUpgradeRequired => {
            let message = plain(e.message(col.tr()));
            // Fluent keeps the source's line breaks in long messages.
            CoreError::Invalid(message.split_whitespace().collect::<Vec<_>>().join(" "))
        }
        _ => e.into(),
    }
}

/// `col.get_config("fsrs")`.
fn fsrs_enabled(col: &mut Collection) -> bool {
    ConfigService::get_config_json(col, "fsrs".to_owned().into())
        .ok()
        .and_then(|json| serde_json::from_slice::<serde_json::Value>(&json.json).ok())
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

fn is_fsrs_only(order: i32) -> bool {
    order == Order::RetrievabilityAscending as i32
        || order == Order::RetrievabilityDescending as i32
}

fn get_deck(col: &mut Collection, deck_id: i64) -> Result<pb::decks::Deck> {
    DecksService::get_deck(col, pb::decks::DeckId { did: deck_id }).map_err(|e| user_error(col, e))
}

fn find_node(node: &pb::decks::DeckTreeNode, id: i64) -> Option<&pb::decks::DeckTreeNode> {
    if node.deck_id == id {
        return Some(node);
    }
    node.children.iter().find_map(|child| find_node(child, id))
}

// ---------------------------------------------------------------------------
// Filtered decks

/// `FilteredDeckConfigDialog._load_deck` + `set_custom_searches`.
fn filtered_deck(
    col: &mut Collection,
    deck_id: i64,
    search: Option<String>,
    search_2: Option<String>,
) -> Result<FilteredDeckForm> {
    let deck = DecksService::get_or_create_filtered_deck(col, pb::decks::DeckId { did: deck_id })
        .map_err(|e| user_error(col, e))?;
    let config = deck.config.unwrap_or_default();
    let fsrs = fsrs_enabled(col);
    let orders: Vec<OrderChoice> = DecksService::filtered_deck_order_labels(col)?
        .vals
        .into_iter()
        .enumerate()
        .map(|(value, label)| OrderChoice {
            value: value as i32,
            label: plain(label),
        })
        .filter(|choice| fsrs || !is_fsrs_only(choice.value))
        .collect();
    // `_combo_row`: a saved order that is not offered shows as Random.
    let offered = |order: i32| {
        if orders.iter().any(|o| o.value == order) {
            order
        } else {
            Order::Random as i32
        }
    };
    let term = |t: &pb::decks::deck::filtered::SearchTerm| FilteredTerm {
        search: t.search.clone(),
        limit: t.limit,
        order: offered(t.order),
    };
    let existing = deck.id != 0;
    let first = config.search_terms.first().map(&term).unwrap_or_default();
    let (mut second, mut second_enabled) = match config.search_terms.get(1) {
        Some(t) => (term(t), existing),
        None => (
            FilteredTerm {
                search: String::new(),
                limit: 20,
                order: Order::Random as i32,
            },
            false,
        ),
    };
    let mut first = first;
    if let Some(search) = search {
        first.search = search;
    }
    if let Some(search) = search_2 {
        second.search = search;
        second_enabled = true;
    }
    let tr = col.tr();
    let delay = |button: String| plain(tr.decks_delay_for_button(button));
    let labels = FilteredLabels {
        title: plain(tr.actions_options_for(deck.name.as_str())),
        name: plain(tr.actions_name()),
        filter: plain(tr.decks_filter()),
        filter_2: plain(tr.decks_filter_2()),
        search: plain(tr.actions_search()),
        limit_to: plain(tr.decks_limit_to()),
        cards_selected_by: plain(tr.decks_cards_selected_by()),
        enable_second_filter: plain(tr.decks_enable_second_filter()),
        reschedule: plain(tr.decks_reschedule_cards_based_on_my_answers()),
        delay_again: delay(tr.studying_again().into()),
        delay_hard: delay(tr.studying_hard().into()),
        delay_good: delay(tr.studying_good().into()),
        zero_hint: plain(tr.decks_zero_minutes_hint()),
        seconds: plain(tr.scheduling_seconds()),
        allow_empty: plain(tr.decks_create_even_if_empty()),
        build: plain(if existing {
            tr.actions_rebuild()
        } else {
            tr.decks_build()
        }),
    };
    Ok(FilteredDeckForm {
        deck: FilteredDeck {
            id: deck.id,
            name: deck.name,
            reschedule: config.reschedule,
            first,
            second,
            second_enabled,
            preview_again_secs: config.preview_again_secs,
            preview_hard_secs: config.preview_hard_secs,
            preview_good_secs: config.preview_good_secs,
            allow_empty: false,
        },
        orders,
        labels,
    })
}

/// `FilteredDeckConfigDialog._update_deck` + `accept`: the stored config is
/// reloaded so fields the form does not show (`preview_delay`) survive, and
/// the v1 `delays` are cleared as desktop does.
fn save_filtered_deck(col: &mut Collection, deck: FilteredDeck) -> Result<DeckToolsResponse> {
    let mut update =
        DecksService::get_or_create_filtered_deck(col, pb::decks::DeckId { did: deck.id })
            .map_err(|e| user_error(col, e))?;
    let term = |t: FilteredTerm| pb::decks::deck::filtered::SearchTerm {
        search: t.search,
        limit: t.limit,
        order: t.order,
    };
    update.name = deck.name;
    update.allow_empty = deck.allow_empty;
    let config = update.config.get_or_insert_with(Default::default);
    config.reschedule = deck.reschedule;
    config.delays.clear();
    config.search_terms = vec![term(deck.first)];
    if deck.second_enabled {
        config.search_terms.push(term(deck.second));
    }
    config.preview_again_secs = deck.preview_again_secs;
    config.preview_hard_secs = deck.preview_hard_secs;
    config.preview_good_secs = deck.preview_good_secs;
    let out =
        DecksService::add_or_update_filtered_deck(col, update).map_err(|e| user_error(col, e))?;
    Ok(DeckToolsResponse::Saved {
        changes: op_changes(out.changes),
        deck_id: out.id,
    })
}

// ---------------------------------------------------------------------------
// Custom study

/// `CustomStudy.fetch_data_and_show` plus the dialog's strings.
fn custom_study_defaults(col: &mut Collection, deck_id: i64) -> Result<CustomStudyDefaults> {
    let deck = get_deck(col, deck_id)?;
    let d = SchedulerService::custom_study_defaults(
        col,
        pb::scheduler::CustomStudyDefaultsRequest { deck_id },
    )
    .map_err(|e| user_error(col, e))?;
    let tree = crate::study::deck_tree(col)?;
    let card_count = find_node(&tree, deck_id).map_or(0, |n| n.total_including_children);
    let tr = col.tr();
    // `count_with_children`.
    let with_children = |parent: u32, children: u32| {
        if children > 0 {
            format!(
                "{parent} {}",
                tr.custom_study_available_child_count(children)
            )
        } else {
            parent.to_string()
        }
    };
    let plural = |f: &dyn Fn(u32) -> String| Plural {
        one: plain(f(1)),
        other: plain(f(2)),
    };
    let labels = CustomStudyLabels {
        modes: [
            plain(tr.custom_study_increase_todays_new_card_limit()),
            plain(tr.custom_study_increase_todays_review_card_limit()),
            plain(tr.custom_study_review_forgotten_cards()),
            plain(tr.custom_study_review_ahead()),
            plain(tr.custom_study_preview_new_cards()),
            plain(tr.custom_study_study_by_card_state_or_tag()),
        ],
        before: [
            plain(tr.custom_study_increase_todays_new_card_limit_by()),
            plain(tr.custom_study_increase_todays_review_limit_by()),
            plain(tr.custom_study_review_cards_forgotten_in_last()),
            plain(tr.custom_study_review_ahead_by()),
            plain(tr.custom_study_preview_new_cards_added_in_the()),
            plain(tr.custom_study_select()),
        ],
        cards: plural(&|n| tr.custom_study_cards(n).into()),
        days: plural(&|n| tr.custom_study_days(n).into()),
        cards_from_the_deck: plural(&|n| tr.custom_study_cards_from_the_deck(n).into()),
        available_new: plain(tr.custom_study_available_new_cards_2(with_children(
            d.available_new,
            d.available_new_in_children,
        ))),
        available_review: plain(tr.custom_study_available_review_cards_2(with_children(
            d.available_review,
            d.available_review_in_children,
        ))),
        cram_kinds: [
            plain(tr.custom_study_new_cards_only()),
            plain(tr.custom_study_due_cards_only()),
            plain(tr.custom_study_all_review_cards_in_random_order()),
            plain(tr.custom_study_all_cards_in_random_order_dont()),
        ],
        ok: plain(tr.custom_study_ok()),
        choose_tags: plain(tr.custom_study_choose_tags()),
        tags_title: plain(tr.custom_study_selective_study()),
        require_tags: plain(tr.custom_study_require_one_or_more_of_these()),
        exclude_tags: plain(tr.custom_study_select_tags_to_exclude()),
        session_name: plain(tr.custom_study_custom_study_session()),
    };
    Ok(CustomStudyDefaults {
        deck_id,
        deck_name: deck.name,
        tags: d
            .tags
            .into_iter()
            .map(|t| StudyTag {
                name: t.name,
                include: t.include,
                exclude: t.exclude,
            })
            .collect(),
        extend_new: d.extend_new,
        extend_review: d.extend_review,
        available_new: d.available_new,
        available_review: d.available_review,
        available_new_in_children: d.available_new_in_children,
        available_review_in_children: d.available_review_in_children,
        card_count,
        labels,
    })
}

/// `CustomStudy.accept` + `TagLimit.accept`.
fn custom_study(
    col: &mut Collection,
    deck_id: i64,
    study: CustomStudy,
) -> Result<DeckToolsResponse> {
    use pb::scheduler::custom_study_request::{cram::CramKind as Kind, Cram, Value};
    let builds_deck = !matches!(
        study,
        CustomStudy::NewLimitDelta(_) | CustomStudy::ReviewLimitDelta(_)
    );
    let value = match study {
        CustomStudy::NewLimitDelta(delta) => Value::NewLimitDelta(delta),
        CustomStudy::ReviewLimitDelta(delta) => Value::ReviewLimitDelta(delta),
        CustomStudy::ForgotDays(days) => Value::ForgotDays(days),
        CustomStudy::ReviewAheadDays(days) => Value::ReviewAheadDays(days),
        CustomStudy::PreviewDays(days) => Value::PreviewDays(days),
        CustomStudy::Cram {
            kind,
            card_limit,
            tags_to_include,
            tags_to_exclude,
        } => {
            if tags_to_include.len() + tags_to_exclude.len() > MAX_TAGS {
                let message = plain(col.tr().errors_100_tags_max());
                return Err(CoreError::Invalid(
                    message.split_whitespace().collect::<Vec<_>>().join(" "),
                ));
            }
            Value::Cram(Cram {
                kind: match kind {
                    CramKind::New => Kind::New,
                    CramKind::Due => Kind::Due,
                    CramKind::Review => Kind::Review,
                    CramKind::All => Kind::All,
                } as i32,
                card_limit,
                tags_to_include,
                tags_to_exclude,
            })
        }
    };
    let out = SchedulerService::custom_study(
        col,
        pb::scheduler::CustomStudyRequest {
            deck_id,
            value: Some(value),
        },
    )
    .map_err(|e| user_error(col, e))?;
    let session = if builds_deck {
        let name = col.tr().custom_study_custom_study_session().to_string();
        DecksService::get_deck_id_by_name(col, name.into())?.did
    } else {
        0
    };
    Ok(DeckToolsResponse::Saved {
        changes: changes(out),
        deck_id: session,
    })
}

// ---------------------------------------------------------------------------
// Description

/// `DeckDescriptionDialog` does not open for a filtered deck; Anki has no
/// string for this.
fn normal_kind(deck: &mut pb::decks::Deck) -> Result<&mut pb::decks::deck::Normal> {
    match deck.kind.as_mut() {
        Some(pb::decks::deck::Kind::Normal(normal)) => Ok(normal),
        _ => Err(CoreError::Invalid(
            "A filtered deck has no description.".into(),
        )),
    }
}

fn description(col: &mut Collection, deck_id: i64) -> Result<DeckDescription> {
    let mut deck = get_deck(col, deck_id)?;
    let normal = normal_kind(&mut deck)?;
    let (text, markdown) = (normal.description.clone(), normal.markdown_description);
    let html = if markdown {
        // As the overview renders it (`Overview._desc`).
        CardRenderingService::render_markdown(
            col,
            pb::card_rendering::RenderMarkdownRequest {
                markdown: text.clone(),
                sanitize: true,
            },
        )?
        .val
    } else {
        text.clone()
    };
    let tr = col.tr();
    Ok(DeckDescription {
        deck_id,
        name: deck.name,
        text,
        markdown,
        html,
        title: plain(tr.scheduling_description()),
        markdown_label: plain(tr.deck_config_description_new_handling2()),
    })
}

/// `DeckDescriptionDialog.save_and_accept`.
fn set_description(
    col: &mut Collection,
    deck_id: i64,
    text: String,
    markdown: bool,
) -> Result<OpChanges> {
    let mut deck = get_deck(col, deck_id)?;
    let normal = normal_kind(&mut deck)?;
    normal.description = text;
    normal.markdown_description = markdown;
    Ok(changes(
        DecksService::update_deck(col, deck).map_err(|e| user_error(col, e))?,
    ))
}

// ---------------------------------------------------------------------------
// Deck list prompts

fn count_subdecks(node: &pb::decks::DeckTreeNode) -> u32 {
    node.children
        .iter()
        .map(|child| 1 + count_subdecks(child))
        .sum()
}

fn deck_prompt(col: &mut Collection, deck_id: i64) -> Result<DeckPromptInfo> {
    let tr = col.tr();
    let labels = DeckPromptLabels {
        create: plain(tr.decks_create_deck()),
        new_deck_name: plain(tr.decks_new_deck_name()),
        rename: plain(tr.actions_rename_deck()),
        new_name: plain(tr.actions_new_name()),
        delete: plain(tr.decks_delete_deck()),
    };
    if deck_id == 0 {
        return Ok(DeckPromptInfo {
            labels,
            ..Default::default()
        });
    }
    let deck = get_deck(col, deck_id)?;
    let tree = crate::study::deck_tree(col)?;
    let node = find_node(&tree, deck_id);
    let below = format!("{}::", deck.name);
    let parent = deck.name.rsplit_once("::").map(|(parent, _)| parent);
    let targets = DecksService::get_deck_names(
        col,
        pb::decks::GetDeckNamesRequest {
            skip_empty_default: false,
            include_filtered: false,
        },
    )?
    .entries
    .into_iter()
    .filter(|d| d.id != deck_id && !d.name.starts_with(&below) && Some(d.name.as_str()) != parent)
    .map(|d| DeckTarget {
        id: d.id,
        name: d.name,
    })
    .collect();
    Ok(DeckPromptInfo {
        deck_id,
        filtered: matches!(deck.kind, Some(pb::decks::deck::Kind::Filtered(_))),
        name: deck.name,
        card_count: node.map_or(0, |n| n.total_including_children),
        subdeck_count: node.map_or(0, count_subdecks),
        targets,
        labels,
    })
}

// ---------------------------------------------------------------------------

pub(crate) fn execute(
    col: &mut Collection,
    command: DeckToolsCommand,
) -> Result<DeckToolsResponse> {
    Ok(match command {
        DeckToolsCommand::FilteredDeck {
            deck_id,
            search,
            search_2,
        } => DeckToolsResponse::FilteredDeck(Box::new(filtered_deck(
            col, deck_id, search, search_2,
        )?)),
        DeckToolsCommand::SaveFilteredDeck(deck) => save_filtered_deck(col, deck)?,
        DeckToolsCommand::RebuildFilteredDeck { deck_id } => {
            let out =
                SchedulerService::rebuild_filtered_deck(col, pb::decks::DeckId { did: deck_id })
                    .map_err(|e| user_error(col, e))?;
            DeckToolsResponse::Count {
                changes: op_changes(out.changes),
                count: out.count as usize,
            }
        }
        DeckToolsCommand::EmptyFilteredDeck { deck_id } => DeckToolsResponse::Changes(changes(
            SchedulerService::empty_filtered_deck(col, pb::decks::DeckId { did: deck_id })
                .map_err(|e| user_error(col, e))?,
        )),
        DeckToolsCommand::CustomStudyDefaults { deck_id } => {
            DeckToolsResponse::CustomStudyDefaults(Box::new(custom_study_defaults(col, deck_id)?))
        }
        DeckToolsCommand::CustomStudy { deck_id, study } => custom_study(col, deck_id, study)?,
        DeckToolsCommand::Description { deck_id } => {
            DeckToolsResponse::Description(description(col, deck_id)?)
        }
        DeckToolsCommand::SetDescription {
            deck_id,
            text,
            markdown,
        } => DeckToolsResponse::Changes(set_description(col, deck_id, text, markdown)?),
        DeckToolsCommand::DeckPrompt { deck_id } => {
            DeckToolsResponse::DeckPrompt(Box::new(deck_prompt(col, deck_id)?))
        }
        DeckToolsCommand::CreateDeck { name } => {
            // `DeckManager.add_normal_deck_with_name`.
            let mut deck = DecksService::new_deck(col)?;
            deck.name = name;
            let out = DecksService::add_deck(col, deck).map_err(|e| user_error(col, e))?;
            DeckToolsResponse::Saved {
                changes: op_changes(out.changes),
                deck_id: out.id,
            }
        }
        DeckToolsCommand::RenameDeck { deck_id, new_name } => DeckToolsResponse::Changes(changes(
            DecksService::rename_deck(col, pb::decks::RenameDeckRequest { deck_id, new_name })
                .map_err(|e| user_error(col, e))?,
        )),
        DeckToolsCommand::DeleteDecks { deck_ids } => {
            let out = DecksService::remove_decks(col, pb::decks::DeckIds { dids: deck_ids })
                .map_err(|e| user_error(col, e))?;
            DeckToolsResponse::Deleted {
                changes: op_changes(out.changes),
                count: out.count as usize,
                message: plain(col.tr().browsing_cards_deleted(out.count)),
            }
        }
        DeckToolsCommand::ReparentDecks {
            deck_ids,
            new_parent,
        } => {
            let out = DecksService::reparent_decks(
                col,
                pb::decks::ReparentDecksRequest {
                    deck_ids,
                    new_parent,
                },
            )
            .map_err(|e| user_error(col, e))?;
            DeckToolsResponse::Count {
                changes: op_changes(out.changes),
                count: out.count as usize,
            }
        }
    })
}
