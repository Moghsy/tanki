//! Check database, check media, empty cards and preferences (PLAN 8.10, 8.12, 8.13).
//!
//! Owned by the Maintenance stream (docs/implementation/phase-5-interfaces.md).
//! Reached through `Command::Manage(ManageCommand::Maintenance(…))`; profile
//! folders are managed by [`profiles`], which needs no collection.
//!
//! Check Database and Check Media have no dry run (Check Media rewrites note
//! HTML; Check Database repairs and vacuums), so each forces a backup first
//! (PLAN 14 rule 5) and runs only after that backup succeeded. Both are slow:
//! start them with `CollectionActor::start` and poll `progress()`.
//!
//! The backup reads the collection file through a second descriptor, which
//! drops this process's SQLite lock until the collection is reopened
//! (`actor::backup_and_relock`). The actor reopens after `Command::Backup`;
//! for these commands it should reopen after the command too, see
//! [`MaintenanceCommand::backs_up`].
pub mod profiles;

use crate::actor::{changes, confirm_schema_change};
use crate::{CoreError, OpChanges, Result};
use anki::{
    collection::Collection,
    services::{
        CardRenderingService, CardsService, CollectionService, ConfigService, MediaService,
    },
};
use anki_proto as pb;
use serde::{Deserialize, Serialize};

/// Each variant's doc comment names its `MaintenanceResponse` variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaintenanceCommand {
    /// Backs up (forced), then runs Anki's database check -> `DatabaseChecked`.
    /// Some repairs force a one-way full sync, which cannot be known in
    /// advance: unless `confirmed`, this fails with
    /// `CoreError::SchemaChangeRequired` before anything is touched. Clears
    /// the undo history.
    CheckDatabase { confirmed: bool },
    /// Backs up (forced), then checks the media folder against the notes ->
    /// `MediaChecked`. May rename files and rewrite note fields.
    CheckMedia,
    /// Moves the listed files of the media folder to the media trash ->
    /// `MediaTrashed`. Pass `MediaReport::unused`.
    TrashMedia { files: Vec<String> },
    /// Permanently deletes the media trash -> `Done`.
    EmptyMediaTrash,
    /// Moves trashed files back into the media folder -> `Done`.
    RestoreMediaTrash,
    /// Read-only -> `EmptyCards`.
    FindEmptyCards,
    /// Recomputes the empty cards, then deletes them -> `EmptyCardsDeleted`.
    /// With `keep_notes`, a note whose cards are all empty keeps its first
    /// card (desktop's "Keep notes with no valid cards").
    DeleteEmptyCards { keep_notes: bool },
    /// -> `Preferences`.
    GetPreferences,
    /// -> `PreferencesSaved`.
    SetPreferences(Preferences),
}

impl MaintenanceCommand {
    /// True when the command takes a forced backup first. The backup drops
    /// the SQLite lock; the actor should close and reopen the collection
    /// after such a command.
    pub fn backs_up(&self) -> bool {
        matches!(self, Self::CheckDatabase { .. } | Self::CheckMedia)
    }
}

/// Result of Check Database: the problems Anki fixed, as its localised lines
/// (empty when the database was fine).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseReport {
    pub problems: Vec<String>,
    /// File name of the newest backup in `backups/` when the check began.
    pub backup: Option<String>,
}

/// Result of Check Media.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaReport {
    /// Files in the media folder no note uses.
    pub unused: Vec<String>,
    /// Names notes refer to that are not in the folder.
    pub missing: Vec<String>,
    pub missing_media_notes: Vec<i64>,
    /// Anki's own localised summary, including renames and warnings.
    pub report: String,
    /// The media trash holds files (restorable or deletable).
    pub have_trash: bool,
    pub backup: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmptyCardNote {
    pub note_id: i64,
    pub card_ids: Vec<i64>,
    /// Every card of the note is empty.
    pub all_cards_empty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmptyCardsReport {
    /// Anki's localised report text.
    pub report: String,
    pub notes: Vec<EmptyCardNote>,
}

impl EmptyCardsReport {
    /// The cards a deletion removes: all empty cards, except that with
    /// `keep_notes` a note whose cards are all empty keeps its first card.
    pub fn cards_to_delete(&self, keep_notes: bool) -> Vec<i64> {
        let mut out = Vec::new();
        for note in &self.notes {
            let skip = usize::from(keep_notes && note.all_cards_empty);
            out.extend(note.card_ids.iter().skip(skip));
        }
        out
    }
    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }
}

/// The collection-stored preferences (Anki's `Preferences`). Desktop's
/// Qt-only settings are not here (PLAN 8.13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preferences {
    // Scheduling.
    /// Hour of the day the scheduler day rolls over (0..=23).
    pub rollover: u32,
    pub learn_ahead_secs: u32,
    /// 0 distribute, 1 reviews first, 2 new first.
    pub new_review_mix: i32,
    /// v2 scheduler only.
    pub new_timezone: bool,
    /// v2 scheduler only.
    pub day_learn_first: bool,
    // Reviewing.
    pub hide_audio_play_buttons: bool,
    pub interrupt_audio_when_answering: bool,
    pub show_remaining_due_counts: bool,
    pub show_intervals_on_buttons: bool,
    pub time_limit_secs: u32,
    pub load_balancer_enabled: bool,
    pub fsrs_short_term_with_steps_enabled: bool,
    // Editing.
    pub adding_defaults_to_current_deck: bool,
    pub paste_images_as_png: bool,
    pub paste_strips_formatting: bool,
    pub default_search_text: String,
    pub ignore_accents_in_search: bool,
    pub render_latex: bool,
    // Backups.
    pub backup_daily: u32,
    pub backup_weekly: u32,
    pub backup_monthly: u32,
    pub backup_minimum_interval_mins: u32,
}

impl From<pb::config::Preferences> for Preferences {
    fn from(p: pb::config::Preferences) -> Self {
        let s = p.scheduling.unwrap_or_default();
        let r = p.reviewing.unwrap_or_default();
        let e = p.editing.unwrap_or_default();
        let b = p.backups.unwrap_or_default();
        Self {
            rollover: s.rollover,
            learn_ahead_secs: s.learn_ahead_secs,
            new_review_mix: s.new_review_mix,
            new_timezone: s.new_timezone,
            day_learn_first: s.day_learn_first,
            hide_audio_play_buttons: r.hide_audio_play_buttons,
            interrupt_audio_when_answering: r.interrupt_audio_when_answering,
            show_remaining_due_counts: r.show_remaining_due_counts,
            show_intervals_on_buttons: r.show_intervals_on_buttons,
            time_limit_secs: r.time_limit_secs,
            load_balancer_enabled: r.load_balancer_enabled,
            fsrs_short_term_with_steps_enabled: r.fsrs_short_term_with_steps_enabled,
            adding_defaults_to_current_deck: e.adding_defaults_to_current_deck,
            paste_images_as_png: e.paste_images_as_png,
            paste_strips_formatting: e.paste_strips_formatting,
            default_search_text: e.default_search_text,
            ignore_accents_in_search: e.ignore_accents_in_search,
            render_latex: e.render_latex,
            backup_daily: b.daily,
            backup_weekly: b.weekly,
            backup_monthly: b.monthly,
            backup_minimum_interval_mins: b.minimum_interval_mins,
        }
    }
}

impl From<Preferences> for pb::config::Preferences {
    fn from(p: Preferences) -> Self {
        use pb::config::preferences::{BackupLimits, Editing, Reviewing, Scheduling};
        Self {
            scheduling: Some(Scheduling {
                rollover: p.rollover,
                learn_ahead_secs: p.learn_ahead_secs,
                new_review_mix: p.new_review_mix,
                new_timezone: p.new_timezone,
                day_learn_first: p.day_learn_first,
            }),
            reviewing: Some(Reviewing {
                hide_audio_play_buttons: p.hide_audio_play_buttons,
                interrupt_audio_when_answering: p.interrupt_audio_when_answering,
                show_remaining_due_counts: p.show_remaining_due_counts,
                show_intervals_on_buttons: p.show_intervals_on_buttons,
                time_limit_secs: p.time_limit_secs,
                load_balancer_enabled: p.load_balancer_enabled,
                fsrs_short_term_with_steps_enabled: p.fsrs_short_term_with_steps_enabled,
            }),
            editing: Some(Editing {
                adding_defaults_to_current_deck: p.adding_defaults_to_current_deck,
                paste_images_as_png: p.paste_images_as_png,
                paste_strips_formatting: p.paste_strips_formatting,
                default_search_text: p.default_search_text,
                ignore_accents_in_search: p.ignore_accents_in_search,
                render_latex: p.render_latex,
            }),
            backups: Some(BackupLimits {
                daily: p.backup_daily,
                weekly: p.backup_weekly,
                monthly: p.backup_monthly,
                minimum_interval_mins: p.backup_minimum_interval_mins,
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MaintenanceResponse {
    /// For commands without a payload.
    Done,
    DatabaseChecked(DatabaseReport),
    MediaChecked(MediaReport),
    /// Files moved to the trash.
    MediaTrashed(usize),
    EmptyCards(EmptyCardsReport),
    EmptyCardsDeleted {
        cards: usize,
        changes: OpChanges,
    },
    Preferences(Preferences),
    PreferencesSaved(OpChanges),
}

impl MaintenanceResponse {
    /// The changes a mutating command made; the actor broadcasts these.
    pub fn changes(&self) -> Option<&OpChanges> {
        match self {
            Self::EmptyCardsDeleted { changes, .. } | Self::PreferencesSaved(changes) => {
                Some(changes)
            }
            // Check Database and Check Media can change anything.
            Self::DatabaseChecked(_) | Self::MediaChecked(_) => Some(&EVERYTHING),
            _ => None,
        }
    }
}

static EVERYTHING: OpChanges = OpChanges {
    card: true,
    note: true,
    deck: true,
    tag: true,
    notetype: true,
    config: true,
    deck_config: true,
    mtime: true,
    browser_table: true,
    browser_sidebar: true,
    note_text: true,
    study_queues: true,
};

/// The profile folder, found through the media folder Anki was opened with
/// (`<profile>/collection.media`).
fn profile_dir(col: &mut Collection) -> Result<std::path::PathBuf> {
    let path = MediaService::get_absolute_media_path(col, String::new().into())?.val;
    std::path::Path::new(&path)
        .parent()
        .map(std::path::Path::to_path_buf)
        .ok_or_else(|| CoreError::Other("Cannot find the profile folder".into()))
}

/// Forced backup; returns the newest backup's file name. Failing here stops
/// the command: nothing runs unbacked.
fn backup_first(col: &mut Collection) -> Result<Option<String>> {
    let profile = profile_dir(col)?;
    crate::backup::create(col, &profile, true)?;
    Ok(crate::backup::list(&profile)?.into_iter().next().map(|b| {
        b.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }))
}

pub(crate) fn execute(
    col: &mut Collection,
    command: MaintenanceCommand,
) -> Result<MaintenanceResponse> {
    Ok(match command {
        MaintenanceCommand::CheckDatabase { confirmed } => {
            confirm_schema_change(col, confirmed)?;
            let backup = backup_first(col)?;
            let result = CollectionService::check_database(col)?;
            MaintenanceResponse::DatabaseChecked(DatabaseReport {
                problems: result
                    .problems
                    .into_iter()
                    .map(|s| anki_i18n::without_unicode_isolation(&s))
                    .collect(),
                backup,
            })
        }
        MaintenanceCommand::CheckMedia => {
            let backup = backup_first(col)?;
            let r = MediaService::check_media(col)?;
            MaintenanceResponse::MediaChecked(MediaReport {
                unused: r.unused,
                missing: r.missing,
                missing_media_notes: r.missing_media_notes,
                report: anki_i18n::without_unicode_isolation(&r.report),
                have_trash: r.have_trash,
                backup,
            })
        }
        MaintenanceCommand::TrashMedia { files } => {
            let count = files.len();
            if count > 0 {
                MediaService::trash_media_files(
                    col,
                    pb::media::TrashMediaFilesRequest { fnames: files },
                )?;
            }
            MaintenanceResponse::MediaTrashed(count)
        }
        MaintenanceCommand::EmptyMediaTrash => {
            MediaService::empty_trash(col)?;
            MaintenanceResponse::Done
        }
        MaintenanceCommand::RestoreMediaTrash => {
            MediaService::restore_trash(col)?;
            MaintenanceResponse::Done
        }
        MaintenanceCommand::FindEmptyCards => MaintenanceResponse::EmptyCards(empty_cards(col)?),
        MaintenanceCommand::DeleteEmptyCards { keep_notes } => {
            let ids = empty_cards(col)?.cards_to_delete(keep_notes);
            if ids.is_empty() {
                return Ok(MaintenanceResponse::EmptyCardsDeleted {
                    cards: 0,
                    changes: OpChanges::default(),
                });
            }
            let done =
                CardsService::remove_cards(col, pb::cards::RemoveCardsRequest { card_ids: ids })?;
            MaintenanceResponse::EmptyCardsDeleted {
                cards: done.count as usize,
                changes: changes(done.changes.unwrap_or_default()),
            }
        }
        MaintenanceCommand::GetPreferences => {
            MaintenanceResponse::Preferences(ConfigService::get_preferences(col)?.into())
        }
        MaintenanceCommand::SetPreferences(prefs) => {
            let out = ConfigService::set_preferences(col, prefs.into())?;
            MaintenanceResponse::PreferencesSaved(changes(out))
        }
    })
}

fn empty_cards(col: &mut Collection) -> Result<EmptyCardsReport> {
    let r = CardRenderingService::get_empty_cards(col)?;
    Ok(EmptyCardsReport {
        report: anki_i18n::without_unicode_isolation(&r.report),
        notes: r
            .notes
            .into_iter()
            .map(|n| EmptyCardNote {
                note_id: n.note_id,
                card_ids: n.card_ids,
                all_cards_empty: n.will_delete_note,
            })
            .collect(),
    })
}
