use crate::*;
use anki::{
    collection::{Collection, CollectionBuilder},
    services::{
        CardRenderingService, CollectionService, DecksService, NotesService, SchedulerService,
        SearchService,
    },
};
use anki_proto as pb;
use std::{
    collections::HashMap,
    path::Path,
    sync::{mpsc, Arc, Mutex},
    thread,
};

type Request = (Command, mpsc::Sender<Result<Response>>);
pub struct CollectionActor {
    sender: Option<mpsc::Sender<Request>>,
    worker: Option<thread::JoinHandle<Result<()>>>,
    subscribers: Arc<Mutex<Vec<mpsc::Sender<OpChanges>>>>,
}
impl CollectionActor {
    pub fn open(profile: impl AsRef<Path>) -> Result<Self> {
        let profile = profile.as_ref().to_path_buf();
        let (sender, receiver) = mpsc::channel::<Request>();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let subscribers = Arc::new(Mutex::new(Vec::<mpsc::Sender<OpChanges>>::new()));
        let subs = subscribers.clone();
        let worker = thread::Builder::new()
            .name("tanki-collection".into())
            .spawn(move || {
                let opened = (|| -> Result<(Collection, std::path::PathBuf)> {
                    let profile = safety::validate_profile(&profile)?;
                    safety::peek_and_backup(&profile)?;
                    std::fs::create_dir_all(profile.join("collection.media"))?;
                    std::fs::create_dir_all(profile.join("backups"))?;
                    let mut col = CollectionBuilder::new(profile.join("collection.anki2"))
                        .set_media_paths(
                            profile.join("collection.media"),
                            profile.join("collection.media.db2"),
                        )
                        .build()?;
                    if let Err(error) = backup(&mut col, &profile) {
                        let _ = col.close(None);
                        return Err(error);
                    }
                    Ok((col, profile))
                })();
                let (mut col, profile) = match opened {
                    Ok(value) => {
                        let _ = ready_tx.send(Ok(()));
                        value
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.clone()));
                        return Err(e);
                    }
                };
                let mut queued = HashMap::new();
                for (command, reply) in receiver {
                    let result = execute(&mut col, &profile, command, &mut queued);
                    if let Ok(Response::Changes(changes) | Response::NoteAdded { changes, .. }) =
                        &result
                    {
                        subs.lock()
                            .unwrap()
                            .retain(|sub| sub.send(changes.clone()).is_ok());
                        queued.clear();
                    }
                    let _ = reply.send(result);
                }
                // Backups are joined synchronously; no background backup survives this loop.
                let saved = backup(&mut col, &profile);
                let closed = col.close(None).map_err(CoreError::from);
                saved.and(closed)
            })?;
        match ready_rx.recv().map_err(|_| CoreError::ActorStopped)? {
            Ok(()) => Ok(Self {
                sender: Some(sender),
                worker: Some(worker),
                subscribers,
            }),
            Err(error) => {
                let _ = worker.join();
                Err(error)
            }
        }
    }
    pub fn execute(&self, command: Command) -> Result<Response> {
        let (tx, rx) = mpsc::channel();
        self.sender
            .as_ref()
            .ok_or(CoreError::ActorStopped)?
            .send((command, tx))
            .map_err(|_| CoreError::ActorStopped)?;
        rx.recv().map_err(|_| CoreError::ActorStopped)?
    }
    pub fn subscribe(&self) -> mpsc::Receiver<OpChanges> {
        let (tx, rx) = mpsc::channel();
        self.subscribers.lock().unwrap().push(tx);
        rx
    }
    pub fn close(mut self) -> Result<()> {
        self.shutdown()
    }
    fn shutdown(&mut self) -> Result<()> {
        self.sender.take();
        match self.worker.take() {
            Some(worker) => worker.join().map_err(|_| CoreError::ActorStopped)?,
            None => Ok(()),
        }
    }
}
impl Drop for CollectionActor {
    fn drop(&mut self) {
        if let Err(e) = self.shutdown() {
            eprintln!("tanki collection shutdown: {e}");
        }
    }
}
impl From<anki::error::AnkiError> for CoreError {
    fn from(e: anki::error::AnkiError) -> Self {
        match &e {
            anki::error::AnkiError::DbError { source }
                if source.kind == anki::error::DbErrorKind::Locked =>
            {
                Self::Locked
            }
            _ => Self::Other(e.to_string()),
        }
    }
}
fn backup(col: &mut Collection, profile: &Path) -> Result<()> {
    if let Some(handle) = col.maybe_backup(profile.join("backups"), true)? {
        handle
            .join()
            .map_err(|_| CoreError::Other("Backup worker panicked".into()))??;
    }
    Ok(())
}
fn changes(c: pb::collection::OpChanges) -> OpChanges {
    OpChanges {
        card: c.card,
        note: c.note,
        deck: c.deck,
        tag: c.tag,
        notetype: c.notetype,
        config: c.config,
        deck_config: c.deck_config,
        mtime: c.mtime,
        browser_table: c.browser_table,
        browser_sidebar: c.browser_sidebar,
        note_text: c.note_text,
        study_queues: c.study_queues,
    }
}
fn deck(d: pb::decks::DeckTreeNode) -> Deck {
    Deck {
        id: d.deck_id,
        name: d.name,
        new: d.new_count,
        learning: d.learn_count,
        review: d.review_count,
        children: d.children.into_iter().map(deck).collect(),
    }
}
fn execute(
    col: &mut Collection,
    profile: &Path,
    command: Command,
    queued: &mut HashMap<i64, pb::scheduler::SchedulingStates>,
) -> Result<Response> {
    Ok(match command {
        Command::DeckTree => Response::DeckTree(deck(DecksService::deck_tree(
            col,
            pb::decks::DeckTreeRequest {
                now: anki::prelude::TimestampSecs::now().0,
            },
        )?)),
        Command::Queue { limit } => {
            let q = SchedulerService::get_queued_cards(
                col,
                pb::scheduler::GetQueuedCardsRequest {
                    fetch_limit: limit,
                    intraday_learning_only: false,
                },
            )?;
            queued.clear();
            let mut cards = Vec::new();
            for c in q.cards {
                let card = c
                    .card
                    .ok_or_else(|| CoreError::Other("Missing queued card".into()))?;
                let states = c
                    .states
                    .ok_or_else(|| CoreError::Other("Missing scheduling states".into()))?;
                let intervals = SchedulerService::describe_next_states(col, states.clone())?
                    .vals
                    .into_iter()
                    .map(|s| anki_i18n::without_unicode_isolation(&s).to_string())
                    .collect();
                queued.insert(card.id, states);
                cards.push(QueuedCard {
                    id: card.id,
                    intervals,
                });
            }
            Response::Queue(Queue {
                new: q.new_count,
                learning: q.learning_count,
                review: q.review_count,
                cards,
            })
        }
        Command::Answer {
            card_id,
            rating,
            answered_at_millis,
            milliseconds_taken,
        } => {
            let state = queued.get(&card_id).cloned().ok_or_else(|| {
                CoreError::Invalid("Fetch a fresh queue before answering.".into())
            })?;
            let (new_state, rating) = match rating {
                Rating::Again => (state.again, 0),
                Rating::Hard => (state.hard, 1),
                Rating::Good => (state.good, 2),
                Rating::Easy => (state.easy, 3),
            };
            Response::Changes(changes(SchedulerService::answer_card(
                col,
                pb::scheduler::CardAnswer {
                    card_id,
                    current_state: state.current,
                    new_state,
                    rating,
                    answered_at_millis,
                    milliseconds_taken,
                },
            )?))
        }
        Command::Undo => Response::Changes(changes(
            CollectionService::undo(col)?.changes.unwrap_or_default(),
        )),
        Command::Render { card_id } => Response::Rendered(render(col, card_id)?),
        Command::Search { query } => Response::CardIds(
            SearchService::search_cards(
                col,
                pb::search::SearchRequest {
                    search: query,
                    order: None,
                },
            )?
            .ids,
        ),
        Command::AddNote { notetype, fields } => {
            let nt = col
                .get_notetype_by_name(&notetype)?
                .ok_or_else(|| CoreError::Invalid(format!("Unknown notetype: {notetype}")))?;
            let mut note =
                NotesService::new_note(col, pb::notetypes::NotetypeId { ntid: nt.id.0 })?;
            if fields.len() != note.fields.len() {
                return Err(CoreError::Invalid("Wrong number of fields".into()));
            }
            note.fields = fields;
            let result = NotesService::add_note(
                col,
                pb::notes::AddNoteRequest {
                    note: Some(note),
                    deck_id: 1,
                },
            )?;
            queued.clear();
            Response::NoteAdded {
                id: result.note_id,
                changes: changes(result.changes.and_then(|c| c.changes).unwrap_or_default()),
            }
        }
        Command::GenerateFixture => {
            crate::fixture::populate(col)?;
            queued.clear();
            Response::CardIds(
                SearchService::search_cards(
                    col,
                    pb::search::SearchRequest {
                        search: "".into(),
                        order: None,
                    },
                )?
                .ids,
            )
        }
        Command::Backup => {
            backup(col, profile)?;
            Response::Backup
        }
    })
}
fn nodes(nodes: Vec<pb::card_rendering::RenderedTemplateNode>, front: Option<&str>) -> String {
    use pb::card_rendering::rendered_template_node::Value;
    nodes
        .into_iter()
        .map(|n| match n.value {
            Some(Value::Text(s)) => s,
            Some(Value::Replacement(r)) if r.field_name == "FrontSide" => {
                front.unwrap_or_default().to_owned()
            }
            Some(Value::Replacement(r)) => r.current_text,
            None => String::new(),
        })
        .collect()
}
fn audio(tags: Vec<pb::card_rendering::AvTag>) -> Vec<AudioTag> {
    use pb::card_rendering::av_tag::Value;
    tags.into_iter()
        .filter_map(|t| {
            t.value.map(|v| match v {
                Value::SoundOrVideo(s) => AudioTag::SoundOrVideo(s),
                Value::Tts(t) => AudioTag::Tts {
                    text: t.field_text,
                    lang: t.lang,
                    voices: t.voices,
                    speed: t.speed,
                    other_args: t.other_args,
                },
            })
        })
        .collect()
}
fn render(col: &mut Collection, card_id: i64) -> Result<RenderedCard> {
    let r = CardRenderingService::render_existing_card(
        col,
        pb::card_rendering::RenderExistingCardRequest {
            card_id,
            browser: false,
            partial_render: true,
        },
    )?;
    let q = CardRenderingService::extract_av_tags(
        col,
        pb::card_rendering::ExtractAvTagsRequest {
            text: nodes(r.question_nodes, None),
            question_side: true,
        },
    )?;
    let a = CardRenderingService::extract_av_tags(
        col,
        pb::card_rendering::ExtractAvTagsRequest {
            text: nodes(r.answer_nodes, Some(&q.text)),
            question_side: false,
        },
    )?;
    Ok(RenderedCard {
        question: q.text,
        answer: a.text,
        css: r.css,
        question_audio: audio(q.av_tags),
        answer_audio: audio(a.av_tags),
    })
}
