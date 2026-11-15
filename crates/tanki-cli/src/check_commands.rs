//! `tanki check db|media|empty-cards` (PLAN 8.10, 8.12, 9). Owned by the
//! Maintenance stream (docs/implementation/phase-5-interfaces.md).
//!
//! Check Database and Check Media back up first (inside the command) and
//! change the collection, so they need `--yes` (or an answer at a terminal).
//! Empty cards only reports unless `--yes` is given.
use clap::{Args, Subcommand};
use std::io::{IsTerminal, Write};
use tanki_core::{
    maintenance::{EmptyCardsReport, MaintenanceCommand as Cmd, MaintenanceResponse as Resp},
    CollectionActor, Command, CoreError, ManageCommand, ManageResponse, Response,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Args, Debug)]
pub struct CheckArgs {
    #[command(subcommand)]
    pub what: CheckWhat,
}

#[derive(Subcommand, Debug)]
pub enum CheckWhat {
    /// Check and repair the database (backs up first; may force a one-way full sync).
    Db {
        /// Do not ask for confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Check media against notes (backs up first; may rename files and rewrite notes).
    Media {
        /// Do not ask for confirmation.
        #[arg(long)]
        yes: bool,
        /// Move the unused files to the media trash afterwards.
        #[arg(long)]
        trash_unused: bool,
        /// Permanently delete the media trash afterwards.
        #[arg(long, conflicts_with = "restore_trash")]
        empty_trash: bool,
        /// Move the media trash back into the media folder afterwards.
        #[arg(long)]
        restore_trash: bool,
    },
    /// Report cards whose front renders empty; delete them with --yes.
    EmptyCards {
        /// Delete the empty cards instead of only reporting them.
        #[arg(long)]
        yes: bool,
        /// Also delete a note when all of its cards are empty (default: keep
        /// each such note's first card).
        #[arg(long)]
        delete_notes: bool,
    },
}

fn maintenance(actor: &CollectionActor, command: Cmd) -> Result<Resp> {
    match actor.execute(Command::Manage(ManageCommand::Maintenance(command)))? {
        Response::Manage(ManageResponse::Maintenance(response)) => Ok(response),
        _ => Err("Unexpected response from the collection".into()),
    }
}

/// Runs a long command, writing Anki's progress to stderr unless `json`.
fn long(actor: &CollectionActor, command: Cmd, json: bool) -> Result<Resp> {
    let pending = actor.start(Command::Manage(ManageCommand::Maintenance(command)))?;
    let mut last = String::new();
    loop {
        match pending.try_recv()? {
            Some(Response::Manage(ManageResponse::Maintenance(response))) => return Ok(response),
            Some(_) => return Err("Unexpected response from the collection".into()),
            None => {
                if !json {
                    let p = actor.progress();
                    let line = match (p.current, p.total) {
                        (Some(c), Some(t)) => format!("{} {c}/{t}", p.stage),
                        _ => p.stage,
                    };
                    if !line.is_empty() && line != last {
                        eprintln!("{line}");
                        last = line;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
}

/// `--yes`, or a yes at a terminal. Otherwise `Ok(false)` after printing why.
fn confirmed(yes: bool, question: &str) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        eprintln!("{question}\nRun again with --yes to proceed.");
        return Ok(false);
    }
    eprint!("{question} [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

fn print_json(value: &serde_json::Value) -> Result<()> {
    writeln!(
        std::io::stdout().lock(),
        "{}",
        serde_json::to_string_pretty(value)?
    )?;
    Ok(())
}

pub fn run_check(actor: &CollectionActor, args: &CheckArgs, json: bool) -> Result<()> {
    match &args.what {
        CheckWhat::Db { yes } => check_db(actor, *yes, json),
        CheckWhat::Media {
            yes,
            trash_unused,
            empty_trash,
            restore_trash,
        } => check_media(
            actor,
            *yes,
            *trash_unused,
            *empty_trash,
            *restore_trash,
            json,
        ),
        CheckWhat::EmptyCards { yes, delete_notes } => {
            empty_cards(actor, *yes, !*delete_notes, json)
        }
    }
}

fn check_db(actor: &CollectionActor, yes: bool, json: bool) -> Result<()> {
    let question = "Check Database makes a backup first, then repairs problems, and clears undo. \
Some repairs force a one-way full sync on the next sync. Continue?";
    if !confirmed(yes, question)? {
        // Same code as every other change that may need a full sync.
        return Err(CoreError::SchemaChangeRequired.into());
    }
    let Resp::DatabaseChecked(report) = long(actor, Cmd::CheckDatabase { confirmed: true }, json)?
    else {
        return Err("Unexpected response from the collection".into());
    };
    if json {
        return print_json(&serde_json::json!({
            "problems": report.problems, "backup": report.backup
        }));
    }
    let mut out = std::io::stdout().lock();
    if let Some(backup) = &report.backup {
        writeln!(out, "Backup written: {backup}")?;
    }
    if report.problems.is_empty() {
        writeln!(out, "Database rebuilt and optimized. No problems found.")?;
    } else {
        writeln!(out, "Problems fixed:")?;
        for problem in &report.problems {
            writeln!(out, "  {problem}")?;
        }
    }
    Ok(())
}

fn check_media(
    actor: &CollectionActor,
    yes: bool,
    trash_unused: bool,
    empty_trash: bool,
    restore_trash: bool,
    json: bool,
) -> Result<()> {
    let question = "Check Media makes a backup first. It may rename media files and rewrite \
note fields that refer to them, and there is no dry run. Continue?";
    if !confirmed(yes, question)? {
        return Err("Cancelled".into());
    }
    let Resp::MediaChecked(report) = long(actor, Cmd::CheckMedia, json)? else {
        return Err("Unexpected response from the collection".into());
    };
    let mut trashed = 0;
    if trash_unused && !report.unused.is_empty() {
        let question = format!(
            "Move {} unused file(s) to the media trash (restorable)?",
            report.unused.len()
        );
        if confirmed(yes, &question)? {
            let Resp::MediaTrashed(n) = maintenance(
                actor,
                Cmd::TrashMedia {
                    files: report.unused.clone(),
                },
            )?
            else {
                return Err("Unexpected response from the collection".into());
            };
            trashed = n;
        }
    }
    let mut emptied = false;
    let mut restored = false;
    if empty_trash
        && confirmed(
            yes,
            "Permanently delete every file in the media trash? This cannot be undone.",
        )?
    {
        maintenance(actor, Cmd::EmptyMediaTrash)?;
        emptied = true;
    }
    if restore_trash {
        maintenance(actor, Cmd::RestoreMediaTrash)?;
        restored = true;
    }
    if json {
        return print_json(&serde_json::json!({
            "unused": report.unused, "missing": report.missing,
            "report": report.report, "have_trash": report.have_trash,
            "backup": report.backup, "trashed": trashed,
            "trash_emptied": emptied, "trash_restored": restored,
        }));
    }
    let mut out = std::io::stdout().lock();
    if let Some(backup) = &report.backup {
        writeln!(out, "Backup written: {backup}")?;
    }
    writeln!(out, "{}", report.report.trim_end())?;
    writeln!(
        out,
        "Unused: {}  Missing: {}  Trash: {}",
        report.unused.len(),
        report.missing.len(),
        if report.have_trash {
            "has files"
        } else {
            "empty"
        }
    )?;
    if trashed > 0 {
        writeln!(out, "Moved {trashed} unused file(s) to the media trash.")?;
    } else if !report.unused.is_empty() && !trash_unused {
        writeln!(
            out,
            "Use --trash-unused to move the unused files to the trash."
        )?;
    }
    if emptied {
        writeln!(out, "Media trash emptied.")?;
    }
    if restored {
        writeln!(out, "Media trash restored.")?;
    }
    Ok(())
}

fn empty_cards(actor: &CollectionActor, yes: bool, keep_notes: bool, json: bool) -> Result<()> {
    let Resp::EmptyCards(report): Resp = maintenance(actor, Cmd::FindEmptyCards)? else {
        return Err("Unexpected response from the collection".into());
    };
    let doomed = report.cards_to_delete(keep_notes).len();
    let mut deleted = None;
    if doomed > 0 && yes {
        let Resp::EmptyCardsDeleted { cards, .. } =
            maintenance(actor, Cmd::DeleteEmptyCards { keep_notes })?
        else {
            return Err("Unexpected response from the collection".into());
        };
        deleted = Some(cards);
    }
    if json {
        return print_json(&serde_json::json!({
            "report": report.report,
            "notes": report.notes.iter().map(|n| serde_json::json!({
                "note_id": n.note_id, "card_ids": n.card_ids,
                "all_cards_empty": n.all_cards_empty
            })).collect::<Vec<_>>(),
            "keep_notes": keep_notes,
            "would_delete": doomed,
            "deleted": deleted,
        }));
    }
    print_empty_report(&report, keep_notes, doomed, deleted)
}

fn print_empty_report(
    report: &EmptyCardsReport,
    keep_notes: bool,
    doomed: usize,
    deleted: Option<usize>,
) -> Result<()> {
    let mut out = std::io::stdout().lock();
    if report.is_empty() {
        writeln!(out, "No empty cards.")?;
        return Ok(());
    }
    writeln!(out, "{}", report.report.trim_end())?;
    match deleted {
        Some(n) => writeln!(out, "Deleted {n} empty card(s).")?,
        None => writeln!(
            out,
            "{doomed} card(s) would be deleted{}. Run with --yes to delete them.",
            if keep_notes {
                " (notes with no valid cards keep their first card)"
            } else {
                ""
            }
        )?,
    }
    Ok(())
}
