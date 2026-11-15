//! Additional actor operations needed by the AnkiConnect adapter.
use crate::{actor::changes, CoreError, OpChanges, Result};
use anki::{
    collection::Collection,
    services::{
        CardsService, DecksService, NotesService, NotetypesService, SearchService, StatsService,
    },
};
use anki_proto as pb;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConnectCommand {
    ModelFieldsOnTemplates {
        id: i64,
    },
    ReviewedToday,
    Duplicate {
        note: crate::NoteData,
        deck_name: Option<String>,
        check_children: bool,
        all_models: bool,
    },
    CreateDeck {
        name: String,
    },
    CardsInfo {
        ids: Vec<i64>,
    },
    CardsOfNote {
        id: i64,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectResponse {
    pub result: Value,
    pub changes: Option<OpChanges>,
}
impl ConnectResponse {
    pub fn changes(&self) -> Option<&OpChanges> {
        self.changes.as_ref()
    }
}
pub(crate) fn execute(col: &mut Collection, command: ConnectCommand) -> Result<ConnectResponse> {
    let mut changed = None;
    let result = match command {
        ConnectCommand::ReviewedToday => json!(
            StatsService::graphs(
                col,
                pb::stats::GraphsRequest {
                    search: String::new(),
                    days: 1
                }
            )?
            .today
            .unwrap_or_default()
            .answer_count
        ),
        ConnectCommand::ModelFieldsOnTemplates { id } => {
            let nt = NotetypesService::get_notetype(col, pb::notetypes::NotetypeId { ntid: id })?;
            let mut result = serde_json::Map::new();
            for template in nt.templates {
                let config = template.config.unwrap_or_default();
                let front = template_fields(&config.q_format);
                let back: Vec<_> = template_fields(&config.a_format)
                    .into_iter()
                    .filter(|f| !front.contains(f))
                    .collect();
                result.insert(template.name, json!([front, back]));
            }
            Value::Object(result)
        }
        ConnectCommand::Duplicate {
            note,
            deck_name,
            check_children,
            all_models,
        } => {
            let field = note
                .fields
                .first()
                .ok_or_else(|| CoreError::Invalid("note has no fields".into()))?;
            let first = anki::text::strip_html_preserving_media_filenames(field);
            let search = if all_models {
                String::new()
            } else {
                SearchService::build_search_string(
                    col,
                    pb::search::SearchNode {
                        filter: Some(pb::search::search_node::Filter::Dupe(
                            pb::search::search_node::Dupe {
                                notetype_id: note.notetype_id,
                                first_field: first.to_string(),
                            },
                        )),
                    },
                )?
                .val
            };
            let found = SearchService::search_notes(
                col,
                pb::search::SearchRequest {
                    search,
                    order: Some(pb::search::SortOrder {
                        value: Some(pb::search::sort_order::Value::None(pb::generic::Empty {})),
                    }),
                },
            )?
            .ids;
            let mut duplicate = false;
            for id in found {
                let existing = NotesService::get_note(col, pb::notes::NoteId { nid: id })?;
                if (!all_models && existing.notetype_id != note.notetype_id)
                    || existing.fields.first().is_none_or(|f| {
                        anki::text::strip_html_preserving_media_filenames(f) != first
                    })
                {
                    continue;
                }
                if let Some(name) = &deck_name {
                    let cards =
                        NotesService::cards_of_note(col, pb::notes::NoteId { nid: id })?.cids;
                    let mut matches = false;
                    for cid in cards {
                        let card = CardsService::get_card(col, pb::cards::CardId { cid })?;
                        let deck = DecksService::get_deck(
                            col,
                            pb::decks::DeckId {
                                did: if card.original_deck_id != 0 {
                                    card.original_deck_id
                                } else {
                                    card.deck_id
                                },
                            },
                        )?;
                        if deck.name == *name
                            || (check_children && deck.name.starts_with(&format!("{name}::")))
                        {
                            matches = true;
                            break;
                        }
                    }
                    if !matches {
                        continue;
                    }
                }
                duplicate = true;
                break;
            }
            json!(duplicate)
        }
        ConnectCommand::CreateDeck { name } => {
            if name.trim().is_empty() {
                return Err(CoreError::Invalid("deck name is empty".into()));
            }
            let names = DecksService::get_deck_names(
                col,
                pb::decks::GetDeckNamesRequest {
                    skip_empty_default: false,
                    include_filtered: true,
                },
            )?;
            if let Some(deck) = names
                .entries
                .iter()
                .find(|d| d.name.eq_ignore_ascii_case(&name))
            {
                json!(deck.id)
            } else {
                let mut deck = DecksService::new_deck(col)?;
                deck.name = name;
                let result = DecksService::add_deck(col, deck)?;
                changed = result.changes.map(changes);
                json!(result.id)
            }
        }
        ConnectCommand::CardsOfNote { id } => {
            json!(NotesService::cards_of_note(col, pb::notes::NoteId { nid: id })?.cids)
        }
        ConnectCommand::CardsInfo { ids } => {
            let mut cards = Vec::new();
            for id in ids {
                let c = CardsService::get_card(col, pb::cards::CardId { cid: id })?;
                let deck = DecksService::get_deck(col, pb::decks::DeckId { did: c.deck_id })?;
                cards.push(json!({"cardId":c.id,"note":c.note_id,"deckName":deck.name,"ord":c.template_idx,"type":c.ctype,"queue":c.queue,"due":c.due,"interval":c.interval,"factor":c.ease_factor,"reps":c.reps,"lapses":c.lapses,"left":c.remaining_steps,"mod":c.mtime_secs,"customData":c.custom_data}));
            }
            json!(cards)
        }
    };
    Ok(ConnectResponse {
        result,
        changes: changed,
    })
}

fn template_fields(template: &str) -> Vec<String> {
    template
        .split("{{")
        .skip(1)
        .filter_map(|part| {
            let token = part.split_once("}}")?.0;
            if token.starts_with(['#', '/', '^']) {
                return None;
            }
            let field = token.rsplit(':').next()?.trim();
            (field != "FrontSide").then(|| field.to_owned())
        })
        .collect()
}
