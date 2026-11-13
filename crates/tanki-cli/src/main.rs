use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use tanki_core::{CollectionActor, Command, CoreError, Response};
use tanki_term::TerminalDoctor;

const DISCLAIMER: &str = "tanki is an unofficial, Anki-compatible client. It is not affiliated with or endorsed by Ankitects. Anki is a trademark of Ankitects Pty Ltd.";

#[derive(Parser)]
#[command(name = "tanki", version, long_version = concat!(env!("CARGO_PKG_VERSION"), "\n", "tanki is an unofficial, Anki-compatible client. It is not affiliated with or endorsed by Ankitects. Anki is a trademark of Ankitects Pty Ltd."), after_help = DISCLAIMER)]
struct Args {
    /// Existing temporary base directory containing generated fixture profiles.
    #[arg(long, global = true)]
    base: Option<PathBuf>,
    #[arg(long, global = true, default_value = "tanki-fixture")]
    profile: String,
    #[arg(long, global = true, value_enum, default_value = "auto")]
    graphics: GraphicsArg,
    #[command(subcommand)]
    command: Option<CliCommand>,
}
#[derive(Clone, Copy, ValueEnum)]
enum GraphicsArg {
    Auto,
    Kitty,
    Sixel,
    Iterm,
    Blocks,
    None,
}
impl From<GraphicsArg> for tanki_term::GraphicsMode {
    fn from(value: GraphicsArg) -> Self {
        match value {
            GraphicsArg::Auto => Self::Auto,
            GraphicsArg::Kitty => Self::Kitty,
            GraphicsArg::Sixel => Self::Sixel,
            GraphicsArg::Iterm => Self::Iterm,
            GraphicsArg::Blocks => Self::Blocks,
            GraphicsArg::None => Self::None,
        }
    }
}

#[derive(Subcommand)]
enum CliCommand {
    /// Probe terminal capabilities and display a test image.
    Doctor,
    /// Dump fixture card HTML/audio for the Phase 0 comparison harness.
    #[command(hide = true)]
    FixtureDump,
}
fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    match args.command {
        Some(CliCommand::Doctor) => {
            let report = tanki_term::SystemTerminalDoctor.run(&tanki_term::DoctorOptions {
                graphics: args.graphics.into(),
                show_image: true,
            })?;
            println!("{}", report.summary);
        }
        Some(CliCommand::FixtureDump) => {
            let base = args
                .base
                .ok_or("Fixture commands require explicit --base")?;
            tanki_core::validate_profile(&base)?;
            if args.profile.is_empty()
                || args.profile == "."
                || args.profile == ".."
                || args.profile.contains(['/', '\\'])
            {
                return Err("--profile must be a single folder name".into());
            }
            let actor = CollectionActor::open(base.join(args.profile))?;
            let result = (|| -> Result<(), Box<dyn std::error::Error>> {
                let Response::CardIds(ids) = actor.execute(Command::Search {
                    query: String::new(),
                })?
                else {
                    return Err("Unexpected search response".into());
                };
                let mut cards = Vec::new();
                for card_id in ids {
                    let Response::Rendered(card) = actor.execute(Command::Render { card_id })?
                    else {
                        return Err("Unexpected render response".into());
                    };
                    cards.push(serde_json::json!({"id":card_id,"rendered":card}));
                }
                println!("{}", serde_json::to_string_pretty(&cards)?);
                Ok(())
            })();
            let closed = actor.close();
            result?;
            closed?;
        }
        None => println!(
            "tanki: Phase 0 foundations; interactive study is not implemented yet.\n{DISCLAIMER}"
        ),
    }
    Ok(())
}
fn main() -> std::process::ExitCode {
    match run(Args::parse()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tanki: {e}");
            if e.downcast_ref::<CoreError>() == Some(&CoreError::Locked) {
                std::process::ExitCode::from(20)
            } else {
                std::process::ExitCode::FAILURE
            }
        }
    }
}
