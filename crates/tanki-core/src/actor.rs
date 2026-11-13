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
                    let col = open_collection(&profile)?;
                    // Backup failure fails the open: no command runs unbacked.
                    let (col, saved) = backup_and_relock(col, &profile)?;
                    if let Err(error) = saved {
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
                    if matches!(command, Command::Backup) {
                        queued.clear();
                        match backup_and_relock(col, &profile) {
                            Ok((reopened, saved)) => {
                                col = reopened;
                                let _ = reply.send(saved.map(|()| Response::Backup));
                            }
                            // The collection could not be reopened; stop the actor.
                            Err(error) => {
                                let _ = reply.send(Err(error.clone()));
                                return Err(error);
                            }
                        }
                        continue;
                    }
                    let result = execute(&mut col, command, &mut queued);
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
                // The exit backup is joined before close, so no backup thread
                // outlives the actor. No relock is needed: the collection closes next.
                let saved = backup(&mut col, &profile).map(|_| ());
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
/// The only place a collection is built, so every reopen uses the same paths.
fn open_collection(profile: &Path) -> Result<Collection> {
    Ok(CollectionBuilder::new(profile.join("collection.anki2"))
        .set_media_paths(
            profile.join("collection.media"),
            profile.join("collection.media.db2"),
        )
        .build()?)
}
/// Forces a backup and waits for its thread. Returns whether Anki read the
/// collection file, which is the step that loses the file lock (see below).
fn backup(col: &mut Collection, profile: &Path) -> Result<bool> {
    match col.maybe_backup(profile.join("backups"), true) {
        Ok(Some(handle)) => {
            handle
                .join()
                .map_err(|_| CoreError::Other("Backup worker panicked".into()))??;
            Ok(true)
        }
        Ok(None) => Ok(false),
        Err(error) => Err(error.into()),
    }
}
/// Backs up, then closes and reopens the collection.
///
/// Anki's `maybe_backup` reads `collection.anki2` through a second file
/// descriptor. On POSIX, closing any descriptor of a file releases every
/// `fcntl` lock the process holds on it, so after a backup other processes can
/// open the collection although this one still has it open (verified against
/// 25.09.4 and the pinned core; `tests/lock_across_processes.rs`). SQLite
/// only takes the lock when a connection first touches the database, so a fresh
/// connection is the only way to get it back.
///
/// The outer error means the collection is gone (another process took it in
/// the gap, or reopening failed). The inner result is the backup's outcome.
/// Reopening discards undo history.
fn backup_and_relock(mut col: Collection, profile: &Path) -> Result<(Collection, Result<()>)> {
    let saved = backup(&mut col, profile);
    if matches!(saved, Ok(false)) {
        return Ok((col, Ok(())));
    }
    col.close(None)?;
    Ok((open_collection(profile)?, saved.map(|_| ())))
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
        // Needs to replace the collection, so the actor loop handles it.
        Command::Backup => {
            return Err(CoreError::Invalid(
                "Backup is handled by the actor loop".into(),
            ))
        }
    })
}
/// Completes a partial render the way pylib's `apply_custom_filters` does
/// (`pylib/anki/template.py`). Rust stops at the first filter it does not know
/// and returns the text so far plus the remaining filters; pylib passes that
/// text through add-on filter hooks, which tanki does not have, so the text is
/// used as-is (built-in filters after an unknown one are not applied, as on
/// desktop). `{{FrontSide}}` on the answer becomes `front`, the question after
/// AV-tag extraction; elsewhere it keeps the text Rust gave it.
fn nodes(nodes: Vec<pb::card_rendering::RenderedTemplateNode>, front: Option<&str>) -> String {
    use pb::card_rendering::rendered_template_node::Value;
    nodes
        .into_iter()
        .map(|n| match n.value {
            Some(Value::Text(s)) => s,
            Some(Value::Replacement(r)) if r.field_name == "FrontSide" => {
                front.map_or(r.current_text, str::to_owned)
            }
            Some(Value::Replacement(r)) => r.current_text,
            None => String::new(),
        })
        .collect()
}
/// Converts AV tags as pylib's `av_tag_to_native` does: sound and video
/// filenames are reduced to their last path component (`os.path.basename`
/// on POSIX), so a `[sound:../x.mp3]` cannot point outside the media folder.
fn audio(tags: Vec<pb::card_rendering::AvTag>) -> Vec<AudioTag> {
    use pb::card_rendering::av_tag::Value;
    tags.into_iter()
        .filter_map(|t| {
            t.value.map(|v| match v {
                Value::SoundOrVideo(s) => {
                    AudioTag::SoundOrVideo(s.rsplit('/').next().unwrap_or_default().to_owned())
                }
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
/// Renders a card as pylib's `Card.render_output()` does
/// (`TemplateRenderContext.render` in `pylib/anki/template.py`), checked
/// against Anki's Python output by `tests/golden_render.rs`:
///
/// 1. `render_existing_card` with `partial_render = true` (with `false`, the
///    answer's `{{FrontSide}}` would hold the question before AV extraction
///    and replay its audio, and unknown filters would be skipped rather than
///    ending filtering). A template error becomes the text of both sides,
///    with no CSS and no audio.
/// 2. Question: complete the nodes, then extract AV tags.
/// 3. Answer: complete the nodes with `{{FrontSide}}` = the extracted
///    question, then extract AV tags.
/// 4. LaTeX, as pylib's `card_did_render` hook does after extraction:
///    `[latex]`, `[$]` and `[$$]` become `<img class=latex>` tags. If an image
///    is missing from the media folder and LaTeX generation is off (the
///    default), the localized "disabled" message is appended once per side.
///    tanki does not run LaTeX yet, so with generation on, a missing image is
///    left missing (pylib would generate it).
fn render(col: &mut Collection, card_id: i64) -> Result<RenderedCard> {
    use anki::services::{ConfigService, MediaService};
    let r = match CardRenderingService::render_existing_card(
        col,
        pb::card_rendering::RenderExistingCardRequest {
            card_id,
            browser: false,
            partial_render: true,
        },
    ) {
        Ok(r) => r,
        Err(e @ anki::error::AnkiError::TemplateError { .. }) => {
            let text = e.message(col.tr());
            return Ok(RenderedCard {
                question: text.clone(),
                answer: text,
                css: String::new(),
                question_audio: vec![],
                answer_audio: vec![],
            });
        }
        Err(e) => return Err(e.into()),
    };
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
    let generate_latex = ConfigService::get_config_bool(
        col,
        pb::config::GetConfigBoolRequest {
            key: pb::config::config_key::Bool::RenderLatex as i32,
        },
    )?
    .val;
    let latex_svg = r.latex_svg;
    let mut latex = |html: String| -> Result<String> {
        let out = CardRenderingService::extract_latex(
            col,
            pb::card_rendering::ExtractLatexRequest {
                text: html,
                svg: latex_svg,
                expand_clozes: false,
            },
        )?;
        let mut html = out.text;
        for image in out.latex {
            let path = MediaService::get_absolute_media_path(col, image.filename.into())?.val;
            if !Path::new(&path).exists() && !generate_latex {
                html.push_str(&col.tr().preferences_latex_generation_disabled());
                break;
            }
        }
        Ok(html)
    };
    let question = latex(q.text)?;
    let answer = latex(a.text)?;
    Ok(RenderedCard {
        question,
        answer,
        css: r.css,
        question_audio: audio(q.av_tags),
        answer_audio: audio(a.av_tags),
    })
}

#[cfg(test)]
#[path = "sync_spike.rs"]
mod sync_spike;
