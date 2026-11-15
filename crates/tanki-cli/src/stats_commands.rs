//! `tanki stats` (PLAN 8.8, 9). Owned by the Statistics stream
//! (docs/implementation/phase-5-interfaces.md).
//!
//! One graphs query with desktop's initial panel choices and the stored
//! graph preferences, printed as text, JSON (the raw aggregates, the
//! options and every computed panel) or a standalone SVG document.
use clap::{Args, ValueEnum};
use tanki_core::stats::{self, History, StatsCommand, StatsReport, StatsResponse, StatsScope};
use tanki_core::{CollectionActor, Command, ManageCommand, ManageResponse, Response};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum StatsFormat {
    Text,
    Json,
    Svg,
}

/// Print collection or deck statistics.
#[derive(Args, Debug)]
pub struct StatsArgs {
    /// Only this deck and its subdecks [default: the whole collection].
    #[arg(long, conflicts_with = "search")]
    deck: Option<String>,
    /// Only the cards matching an Anki search.
    #[arg(long)]
    search: Option<String>,
    /// Load all review history instead of the last 12 months.
    #[arg(long)]
    all_history: bool,
    /// Output format (`--json` is the same as `--format json`).
    #[arg(long, value_enum, default_value = "text")]
    format: StatsFormat,
}

pub fn run_stats(
    actor: &CollectionActor,
    args: &StatsArgs,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let format = if json { StatsFormat::Json } else { args.format };
    let report = report(actor, args)?;
    let output = match format {
        StatsFormat::Text => stats::export::text(&report),
        StatsFormat::Json => serde_json::to_string_pretty(&report)? + "\n",
        StatsFormat::Svg => stats::export::svg(&report),
    };
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(output.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

fn scope(args: &StatsArgs) -> StatsScope {
    match (&args.deck, &args.search) {
        (Some(deck), _) => StatsScope::Deck(deck.clone()),
        (None, Some(search)) if !search.trim().is_empty() => StatsScope::Search(search.clone()),
        _ => StatsScope::Collection,
    }
}

fn report(
    actor: &CollectionActor,
    args: &StatsArgs,
) -> Result<StatsReport, Box<dyn std::error::Error>> {
    let command = StatsCommand::Graphs {
        scope: scope(args),
        history: if args.all_history {
            History::All
        } else {
            History::Year
        },
        options: None,
    };
    match actor.execute(Command::Manage(ManageCommand::Stats(command)))? {
        Response::Manage(ManageResponse::Stats(StatsResponse::Report(report))) => Ok(*report),
        _ => Err("Unexpected response from the collection".into()),
    }
}
