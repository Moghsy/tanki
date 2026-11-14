use crate::*;
use anki::{
    collection::{Collection, CollectionBuilder},
    services::{CardRenderingService, CollectionService, NotesService, SearchService},
};
use anki_proto as pb;
use std::{
    collections::HashMap,
    path::Path,
    sync::{mpsc, Arc, Mutex},
    thread,
};

type Request = (Command, mpsc::Sender<Result<Response>>);
pub(crate) type SharedProgress = Arc<Mutex<anki::progress::ProgressState>>;
pub struct CollectionActor {
    profile: std::path::PathBuf,
    sender: Option<mpsc::Sender<Request>>,
    worker: Option<thread::JoinHandle<Result<()>>>,
    subscribers: Arc<Mutex<Vec<mpsc::Sender<OpChanges>>>>,
    progress: SharedProgress,
    tr: anki_i18n::I18n,
}
/// A data operation queued with `CollectionActor::start_data`.
pub struct PendingOperation {
    reply: mpsc::Receiver<Result<Response>>,
}
impl PendingOperation {
    /// `Ok(None)` while the actor is still working; poll
    /// `CollectionActor::progress` meanwhile.
    pub fn try_recv(&self) -> Result<Option<DataResponse>> {
        match self.reply.try_recv() {
            Ok(result) => Ok(Some(data_response(result?)?)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(CoreError::ActorStopped),
        }
    }
    /// Blocks until the operation finishes.
    pub fn wait(self) -> Result<DataResponse> {
        data_response(self.reply.recv().map_err(|_| CoreError::ActorStopped)??)
    }
}
fn data_response(response: Response) -> Result<DataResponse> {
    match response {
        Response::Data(data) => Ok(data),
        _ => Err(CoreError::Other(
            "Unexpected response to a data operation".into(),
        )),
    }
}
impl CollectionActor {
    /// Opens with the conservative defaults: a leftover WAL file is refused.
    pub fn open(profile: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(profile, OpenOptions::default())
    }
    /// `options.confirmed_closed` accepts a leftover `collection.anki2-wal`
    /// after the user confirms Anki is closed; it never skips the `/proc`
    /// check or the lock.
    pub fn open_with_options(profile: impl AsRef<Path>, options: OpenOptions) -> Result<Self> {
        let profile = profile.as_ref().to_path_buf();
        let (sender, receiver) = mpsc::channel::<Request>();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let subscribers = Arc::new(Mutex::new(Vec::<mpsc::Sender<OpChanges>>::new()));
        let subs = subscribers.clone();
        let progress = SharedProgress::default();
        let shared = progress.clone();
        let worker = thread::Builder::new()
            .name("tanki-collection".into())
            .spawn(move || {
                let opened = (|| -> Result<(Collection, std::path::PathBuf)> {
                    let profile = safety::validate_profile(&profile)?;
                    safety::check_not_in_use(&profile, options.confirmed_closed)?;
                    safety::peek_and_backup(&profile)?;
                    std::fs::create_dir_all(profile.join("collection.media"))?;
                    std::fs::create_dir_all(profile.join("backups"))?;
                    // Partial backups a killed process left in staging.
                    crate::backup::clear_incoming(&profile.join("backups"))?;
                    let col = open_collection(&profile, &shared)?;
                    // Backup failure fails the open: no command runs unbacked.
                    let (col, saved) =
                        backup_and_relock_with(col, &profile, &shared, !options.seen_before)?;
                    if let Err(error) = saved {
                        let _ = col.close(None);
                        return Err(error);
                    }
                    Ok((col, profile))
                })();
                let (mut col, profile) = match opened {
                    Ok(value) => {
                        let _ = ready_tx.send(Ok(value.1.clone()));
                        value
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.clone()));
                        return Err(e);
                    }
                };
                let mut queued = HashMap::new();
                loop {
                    // Desktop checks for a due backup every 5 minutes; so does
                    // the actor while idle. Unforced: Anki's interval applies.
                    let (command, reply) = match receiver.recv_timeout(PERIODIC_BACKUP) {
                        Ok(request) => request,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            col = match backup_and_relock_with(col, &profile, &shared, false) {
                                Ok((reopened, saved)) => {
                                    if let Err(error) = saved {
                                        eprintln!("tanki: periodic backup failed: {error}");
                                    }
                                    reopened
                                }
                                Err(error) => return Err(error),
                            };
                            continue;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    let command = match command {
                        Command::Data(data) if owns_collection(&data) => {
                            shared.lock().unwrap().reset();
                            queued.clear();
                            let (reopened, result) = owned_data(col, &profile, &shared, data);
                            match reopened {
                                Ok(reopened) => col = reopened,
                                Err(error) => {
                                    let _ = reply.send(result.and(Err(error.clone())));
                                    return Err(error);
                                }
                            }
                            if result.is_ok() {
                                subs.lock()
                                    .unwrap()
                                    .retain(|sub| sub.send(OpChanges::everything()).is_ok());
                            }
                            let _ = reply.send(result.map(Response::Data));
                            continue;
                        }
                        other => other,
                    };
                    if matches!(command, Command::Backup) {
                        queued.clear();
                        match backup_and_relock(col, &profile, &shared) {
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
                    if matches!(command, Command::Data(_)) {
                        shared.lock().unwrap().reset();
                    }
                    let result = execute(&mut col, command, &mut queued);
                    if let Some(changes) = result.as_ref().ok().and_then(Response::changes) {
                        subs.lock()
                            .unwrap()
                            .retain(|sub| sub.send(changes.clone()).is_ok());
                        queued.clear();
                    }
                    let _ = reply.send(result);
                }
                // The exit backup follows Anki's schedule, as desktop's close
                // does, and is joined before close, so no backup thread
                // outlives the actor. No relock is needed: the collection closes next.
                let saved = crate::backup::create(&mut col, &profile, false).map(|_| ());
                let closed = col.close(None).map_err(CoreError::from);
                saved.and(closed)
            })?;
        match ready_rx.recv().map_err(|_| CoreError::ActorStopped)? {
            Ok(profile) => Ok(Self {
                profile,
                sender: Some(sender),
                worker: Some(worker),
                subscribers,
                progress,
                tr: anki_i18n::I18n::new(&["en"]),
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
    /// Queues a data operation without waiting for it.
    pub fn start_data(&self, command: DataCommand) -> Result<PendingOperation> {
        let (tx, rx) = mpsc::channel();
        self.sender
            .as_ref()
            .ok_or(CoreError::ActorStopped)?
            .send((Command::Data(command), tx))
            .map_err(|_| CoreError::ActorStopped)?;
        Ok(PendingOperation { reply: rx })
    }
    /// Latest progress of the running data operation. Does not go through the
    /// actor's queue, so it answers while the actor is busy.
    pub fn progress(&self) -> OperationProgress {
        use pb::collection::progress::Value;
        let last = self.progress.lock().unwrap().last_progress;
        let text = |stage: String| OperationProgress {
            stage,
            ..Default::default()
        };
        let counted = |stage: String, current: u64, total: u64| OperationProgress {
            stage,
            current: Some(current),
            total: (total > 0).then_some(total),
        };
        let strip = |s: String| anki_i18n::without_unicode_isolation(&s);
        match anki::progress::progress_to_proto(last, &self.tr).value {
            None | Some(Value::None(_)) => OperationProgress::default(),
            Some(Value::MediaSync(p)) => {
                text(strip(format!("{} {} {}", p.checked, p.added, p.removed)))
            }
            Some(Value::MediaCheck(s) | Value::Importing(s) | Value::Exporting(s)) => {
                text(strip(s))
            }
            Some(Value::FullSync(p)) => counted(
                "Transferring collection".into(),
                p.transferred.into(),
                p.total.into(),
            ),
            Some(Value::NormalSync(p)) => {
                text(strip(format!("{} {} {}", p.stage, p.added, p.removed)))
            }
            Some(Value::DatabaseCheck(p)) => {
                counted(strip(p.stage), p.stage_current.into(), p.stage_total.into())
            }
            Some(Value::ComputeParams(p)) => {
                counted("Optimizing".into(), p.current.into(), p.total.into())
            }
            Some(Value::ComputeRetention(p)) => counted(
                "Computing retention".into(),
                p.current.into(),
                p.total.into(),
            ),
            Some(Value::ComputeMemory(p)) => {
                counted(strip(p.label), p.current_cards.into(), p.total_cards.into())
            }
            Some(Value::DownloadUpdate(_)) => text("Downloading".into()),
        }
    }
    /// Asks the running data operation to stop; it then fails with
    /// `CoreError::Interrupted`. Reset when the next data operation starts.
    pub fn cancel_operation(&self) {
        self.progress.lock().unwrap().want_abort = true;
    }
    /// The validated, canonical profile folder.
    pub fn profile_dir(&self) -> &Path {
        &self.profile
    }
    /// `<profile>/collection.media`, where card media resolves.
    pub fn media_dir(&self) -> std::path::PathBuf {
        self.profile.join("collection.media")
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
            anki::error::AnkiError::Interrupted => Self::Interrupted,
            _ => Self::Other(e.to_string()),
        }
    }
}
/// The only place a collection is built, so every reopen uses the same paths.
///
/// Real translations are loaded, as desktop does, instead of the builder's
/// English-only default: localized text inside rendered cards then matches
/// desktop byte for byte (golden test), including the U+2068/U+2069 isolation
/// marks around message arguments, which display code strips. The language is
/// fixed to English until tanki has a config (PLAN 4.5).
fn open_collection(profile: &Path, progress: &SharedProgress) -> Result<Collection> {
    Ok(CollectionBuilder::new(profile.join("collection.anki2"))
        .set_shared_progress_state(progress.clone())
        .set_media_paths(
            profile.join("collection.media"),
            profile.join("collection.media.db2"),
        )
        .set_tr(anki_i18n::I18n::new(&["en"]))
        .build()?)
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
fn backup_and_relock(
    col: Collection,
    profile: &Path,
    progress: &SharedProgress,
) -> Result<(Collection, Result<()>)> {
    backup_and_relock_with(col, profile, progress, true)
}
fn backup_and_relock_with(
    mut col: Collection,
    profile: &Path,
    progress: &SharedProgress,
    force: bool,
) -> Result<(Collection, Result<()>)> {
    let saved = crate::backup::create(&mut col, profile, force);
    if matches!(saved, Ok(false)) {
        return Ok((col, Ok(())));
    }
    col.close(None)?;
    Ok((open_collection(profile, progress)?, saved.map(|_| ())))
}
/// Guard for every operation that can mark the schema modified (PLAN 8.9).
///
/// Anki's core sets the flag silently; desktop asks first. This is
/// conservative: unless a one-way full sync is already pending, an
/// unconfirmed call fails with `SchemaChangeRequired`, including on
/// collections that never synced (the library cannot tell those apart from
/// clean ones without private timestamps). Reads only; no SQL.
pub(crate) fn confirm_schema_change(col: &mut Collection, confirmed: bool) -> Result<()> {
    use anki_proto::sync::sync_status_response::Required;
    if confirmed || col.sync_status_offline()? == Required::FullSync {
        return Ok(());
    }
    Err(CoreError::SchemaChangeRequired)
}
pub(crate) fn changes(c: pb::collection::OpChanges) -> OpChanges {
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
        level: d.level,
        collapsed: d.collapsed,
        filtered: d.filtered,
    }
}
const PERIODIC_BACKUP: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Data operations that need the collection by value or the profile folder.
/// The actor loop runs them through `owned_data`.
fn owns_collection(command: &DataCommand) -> bool {
    matches!(
        command,
        DataCommand::Sync(SyncCommand::Run {
            direction: SyncDirection::Upload | SyncDirection::Download,
            ..
        }) | DataCommand::Package(
            PackageCommand::ExportCollection { .. } | PackageCommand::Restore { .. }
        ) | DataCommand::Backup { .. }
            | DataCommand::ListBackups
    )
}
fn data(col: &mut Collection, command: DataCommand) -> Result<DataResponse> {
    match command {
        command if owns_collection(&command) => Err(CoreError::Invalid(
            "This operation is run by the actor loop".into(),
        )),
        DataCommand::Sync(command) => sync_ops::execute(col, command),
        DataCommand::Package(command) => packages::execute(col, command),
        DataCommand::Backup { .. } | DataCommand::ListBackups => unreachable!(),
    }
}
/// Runs an operation that consumes or replaces the collection. The first
/// result is the collection to continue with; an error there stops the actor.
fn owned_data(
    mut col: Collection,
    profile: &Path,
    progress: &SharedProgress,
    command: DataCommand,
) -> (Result<Collection>, Result<DataResponse>) {
    let reopen = |result: Result<DataResponse>| (open_collection(profile, progress), result);
    match command {
        DataCommand::ListBackups => (
            Ok(col),
            crate::backup::list(profile).map(DataResponse::Backups),
        ),
        DataCommand::Backup { force } => {
            match backup_and_relock_with(col, profile, progress, force) {
                Ok((col, saved)) => (Ok(col), saved.map(|()| DataResponse::Done)),
                Err(error) => (Err(error.clone()), Err(error)),
            }
        }
        DataCommand::Package(PackageCommand::ExportCollection {
            path,
            media,
            legacy,
        }) => {
            if let Err(error) = safety::check_output_path(profile, &path) {
                return (Ok(col), Err(error));
            }
            // Anki would close the live collection downgraded to schema 11,
            // leaving it upgradable by any old client until tanki reopens it.
            if legacy {
                return (
                    Ok(col),
                    Err(CoreError::Invalid(
                        "Legacy collection packages are not supported; use a normal .colpkg."
                            .into(),
                    )),
                );
            }
            reopen(
                packages::export_collection(col, &path, media, legacy).map(|()| DataResponse::Done),
            )
        }
        DataCommand::Package(PackageCommand::Restore { path, confirmed }) => {
            if !confirmed {
                return (
                    Ok(col),
                    Err(CoreError::Invalid(
                        "Restoring replaces the whole collection; confirm to continue.".into(),
                    )),
                );
            }
            // An .apkg unpacks just as well and would replace the collection
            // with a single deck.
            if !path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("colpkg"))
            {
                return (
                    Ok(col),
                    Err(CoreError::Invalid(
                        "Only a .colpkg file can replace the collection.".into(),
                    )),
                );
            }
            let stage = match replacement::new_stage(profile) {
                Ok(stage) => stage,
                Err(error) => return (Ok(col), Err(error)),
            };
            if let Err(error) = packages::restore_to_stage(&path, &stage, progress.clone()) {
                replacement::remove_stage(&stage);
                return (Ok(col), Err(error));
            }
            let (col, installed) = replace(col, profile, &stage, progress, true);
            (col, installed.map(|()| DataResponse::Done))
        }
        DataCommand::Sync(SyncCommand::Run {
            auth,
            direction: SyncDirection::Upload,
            media,
        }) => {
            let uploaded = sync_ops::upload(col, auth.clone());
            let (reopened, result) = reopen(uploaded.map(|()| DataResponse::Done));
            let Ok(mut col) = reopened else {
                return (reopened, result);
            };
            // Media follows the collection, as after a full download.
            let result = result.and_then(|done| {
                if media {
                    sync_ops::execute(&mut col, SyncCommand::Media { auth })
                } else {
                    Ok(done)
                }
            });
            (Ok(col), result)
        }
        DataCommand::Sync(SyncCommand::Run {
            auth,
            direction: SyncDirection::Download,
            media,
        }) => {
            let stage = match replacement::new_stage(profile) {
                Ok(stage) => stage,
                Err(error) => return (Ok(col), Err(error)),
            };
            let downloaded = open_collection(&stage, progress)
                .and_then(|staged| sync_ops::download(staged, auth.clone()));
            if let Err(error) = downloaded {
                replacement::remove_stage(&stage);
                return (Ok(col), Err(error));
            }
            let (reopened, installed) = replace(col, profile, &stage, progress, false);
            let Ok(mut col) = reopened else {
                return (reopened, installed.map(|()| DataResponse::Done));
            };
            let result = installed.and_then(|()| {
                if media {
                    sync_ops::execute(&mut col, SyncCommand::Media { auth })
                } else {
                    Ok(DataResponse::Done)
                }
            });
            (Ok(col), result)
        }
        DataCommand::Sync(_) | DataCommand::Package(_) => {
            let result = data(&mut col, command);
            (Ok(col), result)
        }
    }
}
/// Backs up, closes the live collection, installs `stage` under
/// `replacement`'s locks and reopens. The staging folder is removed.
fn replace(
    col: Collection,
    profile: &Path,
    stage: &Path,
    progress: &SharedProgress,
    media: bool,
) -> (Result<Collection>, Result<()>) {
    let finish = |col: Result<Collection>, result: Result<()>| {
        replacement::remove_stage(stage);
        (col, result)
    };
    let col = match backup_and_relock(col, profile, progress) {
        Ok((col, Ok(()))) => col,
        Ok((col, Err(error))) => return finish(Ok(col), Err(error)),
        Err(error) => return finish(Err(error.clone()), Err(error)),
    };
    if let Err(error) = col.close(None) {
        let error = CoreError::from(error);
        return finish(open_collection(profile, progress), Err(error));
    }
    let installed = replacement::install(profile, stage, media);
    // Never reopen beside another process that has the file open: it may
    // have lost its lock, and the lock would not keep the two apart.
    let holders = safety::other_holders(&safety::collection_files(profile));
    let reopened = if holders.is_empty() {
        open_collection(profile, progress)
    } else {
        Err(CoreError::InUse(format!(
            "Another program opened the collection during the replacement (pid {}); tanki closed it.",
            holders[0].0
        )))
    };
    finish(reopened, installed)
}
fn execute(
    col: &mut Collection,
    command: Command,
    queued: &mut study::StateCache,
) -> Result<Response> {
    Ok(match command {
        Command::Data(command) => Response::Data(data(col, command)?),
        Command::Notes(NoteCommand::UpdateNotes { updates }) => Response::Edit(
            EditResponse::Updated(writeback::update_notes(col, updates)?),
        ),
        Command::Notes(command) => Response::Edit(notes_ops::execute(col, command)?),
        Command::Browse(command) => Response::Edit(browse_ops::execute(col, command)?),
        Command::DeckTree => Response::DeckTree(deck(study::deck_tree(col)?)),
        Command::Queue { limit } => Response::Queue(study::queue(col, limit, queued)?),
        Command::Answer {
            card_id,
            rating,
            answered_at_millis,
            milliseconds_taken,
        } => Response::Answered(study::answer(
            col,
            queued,
            card_id,
            rating,
            answered_at_millis,
            milliseconds_taken,
        )?),
        Command::Undo => study::undone(CollectionService::undo(col)?),
        Command::Redo => study::undone(CollectionService::redo(col)?),
        Command::UndoStatus => Response::UndoStatus(study::undo_status(col)?),
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
            Response::CardIds(all_cards(col)?)
        }
        Command::GenerateStudyFixture => {
            crate::fixture::populate_study(col)?;
            queued.clear();
            Response::CardIds(all_cards(col)?)
        }
        // Needs to replace the collection, so the actor loop handles it.
        Command::Backup => {
            return Err(CoreError::Invalid(
                "Backup is handled by the actor loop".into(),
            ))
        }
        Command::SelectDeck { deck_id } => {
            Response::Overview(study::select_deck(col, deck_id, queued)?)
        }
        Command::Overview => Response::Overview(study::overview(col)?),
        Command::SetDeckCollapsed { deck_id, collapsed } => {
            Response::Changes(study::set_deck_collapsed(col, deck_id, collapsed)?)
        }
        Command::Congrats => Response::Congrats(study::congrats(col)?),
        Command::StudiedToday => Response::Text(study::studied_today(col)?),
        Command::SchedTiming => Response::SchedTiming(study::sched_timing(col)?),
        Command::SchedulerStatus => Response::SchedulerStatus(study::scheduler_status(col)?),
        Command::UpgradeScheduler => Response::Changes(study::upgrade_scheduler(col)?),
        Command::StudyConfig { card_id } => {
            Response::StudyConfig(study::study_config(col, card_id)?)
        }
        Command::CompareAnswer {
            expected,
            provided,
            combining,
        } => Response::Html(study::compare_answer(col, expected, provided, combining)?),
        Command::SetFlag { card_ids, flag } => {
            Response::Changes(study::set_flag(col, card_ids, flag)?)
        }
        Command::SetMarked { note_ids, marked } => {
            Response::Changes(study::set_marked(col, note_ids, marked)?)
        }
        Command::BuryOrSuspend { target, mode } => {
            Response::Changes(study::bury_or_suspend(col, target, mode)?)
        }
        Command::Unbury { deck_id, mode } => Response::Changes(study::unbury(col, deck_id, mode)?),
        Command::ForgetDefaults => Response::ForgetDefaults(study::forget_defaults(col)?),
        Command::Forget {
            card_ids,
            restore_position,
            reset_counts,
        } => Response::Changes(study::forget(
            col,
            card_ids,
            restore_position,
            reset_counts,
        )?),
        Command::SetDueDate { card_ids, days } => {
            Response::Changes(study::set_due_date(col, card_ids, days)?)
        }
        Command::DeleteNotes { note_ids } => Response::Changes(study::delete_notes(col, note_ids)?),
    })
}
fn all_cards(col: &mut Collection) -> Result<Vec<i64>> {
    Ok(SearchService::search_cards(
        col,
        pb::search::SearchRequest {
            search: "".into(),
            order: None,
        },
    )?
    .ids)
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
    use anki::services::{CardsService, ConfigService, MediaService};
    let card = CardsService::get_card(col, pb::cards::CardId { cid: card_id })?;
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
                ord: card.template_idx,
                type_answer: None,
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
    let type_answer = study::type_answer(col, &card, &question)?;
    Ok(RenderedCard {
        question,
        answer,
        css: r.css,
        question_audio: audio(q.av_tags),
        answer_audio: audio(a.av_tags),
        ord: card.template_idx,
        type_answer,
    })
}

#[cfg(test)]
#[path = "sync_spike.rs"]
mod sync_spike;
/// Forced backup for the sync spike's fixture.
#[cfg(test)]
fn backup(col: &mut Collection, profile: &Path) -> Result<bool> {
    crate::backup::create(col, profile, true)
}
