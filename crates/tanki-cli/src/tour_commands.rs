use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use tanki_core::{
    code_tour::{self, Attachment, CodeTourRef},
    config::CodeTourSettings,
    *,
};
use tanki_tui::code_tour as bridge;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
#[derive(clap::Args)]
pub struct TourArgs {
    #[command(subcommand)]
    command: TourCommand,
}
#[derive(clap::Subcommand)]
enum TourCommand {
    /// Inspect local bridge capabilities and running Neovim editors.
    Doctor,
    /// List saved metadata; does not open an Anki profile or source files.
    List {
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Open a saved tour without opening an Anki collection.
    Open {
        id: String,
        #[arg(long)]
        pane: Option<String>,
    },
    /// Explicitly install the recommended note type (idempotent).
    InstallNoteType,
    /// Add a note linked to an existing saved tour.
    Add {
        #[arg(long)]
        tour: String,
        #[arg(long, default_value = "Default")]
        deck: String,
        #[arg(long)]
        prompt: String,
        #[arg(long)]
        answer: String,
    },
    /// Change only the CodeTour field of an existing note.
    Attach {
        #[arg(long)]
        note: i64,
        #[arg(long)]
        tour: String,
    },
    /// Check references and local metadata without opening project source.
    Check {
        #[arg(long, default_value = "CodeTour:*")]
        query: String,
    },
}
impl TourArgs {
    pub fn needs_collection(&self) -> bool {
        !matches!(
            self.command,
            TourCommand::Doctor | TourCommand::List { .. } | TourCommand::Open { .. }
        )
    }
}
fn reference(id: &str) -> Result<CodeTourRef> {
    Ok(if id.starts_with("code-tour:") {
        CodeTourRef::try_from(id.to_owned())?
    } else {
        CodeTourRef::from_id(id)?
    })
}
fn notes(actor: &CollectionActor, command: NoteCommand) -> Result<EditResponse> {
    match actor.execute(Command::Notes(command))? {
        Response::Edit(r) => Ok(r),
        _ => Err("Unexpected note response".into()),
    }
}
fn metadata(settings: &CodeTourSettings, r: &CodeTourRef) -> Result<Value> {
    let cancel = AtomicBool::new(false);
    bridge::capabilities(settings, &cancel)?;
    Ok(bridge::inspect(settings, r, &cancel)?)
}
pub fn run(
    actor: Option<&CollectionActor>,
    args: &TourArgs,
    settings: &CodeTourSettings,
    json_output: bool,
) -> Result<()> {
    let cancel = AtomicBool::new(false);
    let value = match &args.command {
        TourCommand::Doctor => bridge::doctor(settings)?,
        TourCommand::List { limit } => bridge::list(settings, *limit, &cancel)?,
        TourCommand::Open { id, pane } => {
            let r = reference(id)?;
            let editors = bridge::editors(settings, &cancel)?;
            let editors = editors.as_array().ok_or("Invalid editors")?;
            let matches: Vec<_> = editors
                .iter()
                .filter(|e| {
                    pane.as_ref()
                        .is_none_or(|p| e["address"] == *p || e["pane_id"] == *p)
                })
                .collect();
            if matches.is_empty() && pane.is_none() {
                bridge::open(settings, &r, &json!({"launch":true}), &cancel)?
            } else if matches.len() != 1 {
                return Err(format!(
                    "Choose one running editor with --pane SOCKET; available editors: {}",
                    serde_json::to_string(editors)?
                )
                .into());
            } else {
                bridge::open(settings, &r, matches[0], &cancel)?
            }
        }
        TourCommand::InstallNoteType => {
            match actor
                .unwrap()
                .execute(Command::Manage(ManageCommand::Notetypes(
                    NotetypeCommand::InstallCodeTour,
                )))? {
                Response::Manage(ManageResponse::Notetypes(NotetypeResponse::Added {
                    id, ..
                })) => json!({"notetype_id":id}),
                _ => return Err("Unexpected note-type response".into()),
            }
        }
        TourCommand::Add {
            tour,
            deck,
            prompt,
            answer,
        } => {
            let actor = actor.unwrap();
            let r = reference(tour)?;
            metadata(settings, &r)?;
            let EditResponse::Notetypes(types) = notes(actor, NoteCommand::Notetypes)? else {
                return Err("No note types".into());
            };
            let nt = types
                .iter()
                .find(|n| n.name == code_tour::NOTETYPE && n.kind == NotetypeKind::Normal)
                .ok_or("Run tanki tour install-note-type first")?;
            if nt
                .fields
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>()
                != ["Prompt", "Answer", "CodeTour"]
            {
                return Err("Incompatible Tanki Code Tour fields".into());
            }
            let EditResponse::DeckNames(decks) = notes(
                actor,
                NoteCommand::DeckNames {
                    include_filtered: false,
                },
            )?
            else {
                return Err("No decks".into());
            };
            let deck = decks
                .iter()
                .find(|d| d.name == *deck)
                .ok_or("No matching ordinary deck")?;
            let EditResponse::Note(mut note) =
                notes(actor, NoteCommand::NewNote { notetype_id: nt.id })?
            else {
                return Err("No note".into());
            };
            note.fields = vec![escape(prompt), escape(answer), r.uri()];
            let EditResponse::FieldsCheck(check) =
                notes(actor, NoteCommand::CheckFields { note: note.clone() })?
            else {
                return Err("Field check failed".into());
            };
            if check != FieldsCheck::Normal {
                return Err(format!("Card was not added: {check:?}").into());
            }
            match notes(
                actor,
                NoteCommand::AddNote {
                    note,
                    deck_id: deck.id,
                },
            )? {
                EditResponse::NoteAdded { id, .. } => json!({"note_id":id,"tour_id":r.id()}),
                _ => return Err("Note was not added".into()),
            }
        }
        TourCommand::Attach { note, tour } => {
            let actor = actor.unwrap();
            let r = reference(tour)?;
            metadata(settings, &r)?;
            let EditResponse::Note(stored) = notes(actor, NoteCommand::GetNote { id: *note })?
            else {
                return Err("No note".into());
            };
            let EditResponse::Notetype(nt) = notes(
                actor,
                NoteCommand::Notetype {
                    id: stored.notetype_id,
                },
            )?
            else {
                return Err("No note type".into());
            };
            let ord = nt
                .fields
                .iter()
                .position(|f| f.name == code_tour::FIELD)
                .ok_or("Add a CodeTour field to this note type first")?;
            let update = NoteUpdate {
                id: stored.id,
                expected_mtime: stored.mtime_secs,
                expected_fields: Some(stored.fields.clone()),
                fields: vec![(ord as u32, r.uri())],
                tags: None,
            };
            match notes(
                actor,
                NoteCommand::UpdateNotes {
                    updates: vec![update],
                },
            )? {
                EditResponse::Updated(report)
                    if report.conflicts.is_empty() && report.missing.is_empty() =>
                {
                    json!({"note_id":stored.id,"tour_id":r.id()})
                }
                _ => return Err("Note changed; attachment was not written".into()),
            }
        }
        TourCommand::Check { query } => {
            let actor = actor.unwrap();
            let Response::Edit(EditResponse::Ids(ids)) =
                actor.execute(Command::Browse(BrowseCommand::Search {
                    query: query.clone(),
                    mode: BrowseMode::Notes,
                    order: SearchOrder::Default,
                }))?
            else {
                return Err("Search failed".into());
            };
            let mut results = vec![];
            for id in ids {
                let EditResponse::Note(note) = notes(actor, NoteCommand::GetNote { id })? else {
                    continue;
                };
                let EditResponse::Notetype(nt) = notes(
                    actor,
                    NoteCommand::Notetype {
                        id: note.notetype_id,
                    },
                )?
                else {
                    continue;
                };
                let attachment = nt
                    .fields
                    .iter()
                    .position(|f| f.name == code_tour::FIELD)
                    .and_then(|i| note.fields.get(i))
                    .and_then(|f| code_tour::parse_field(f));
                let result = match attachment {
                    Some(Attachment::Saved(r)) => match metadata(settings, &r) {
                        Ok(v) => json!({"status":"available","tour":v}),
                        Err(e) => json!({"status":"unavailable","error":e.to_string()}),
                    },
                    Some(Attachment::Invalid(e)) => json!({"status":"invalid","error":e}),
                    None => json!({"status":"absent"}),
                };
                results.push(json!({"note_id":id,"result":result}));
            }
            json!(results)
        }
    };
    if json_output {
        println!("{}", serde_json::to_string(&value)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&value)?);
    }
    Ok(())
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\n', "<br>")
}
