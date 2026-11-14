mod package_commands;
mod sync_commands;

use clap::{Parser, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};
use tanki_core::{CollectionActor, Command, CoreError, Deck, Response};
use tanki_term::TerminalDoctor;
use tanki_tui::{
    app::Start,
    backend,
    keymap::{Action, Chord, Keymap, Scope},
};

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
    /// Print machine-readable output for commands such as decks.
    #[arg(long, global = true)]
    json: bool,
    /// Open even though collection.anki2-wal exists, after making sure Anki is
    /// closed (it is left behind by a crash). Other-process checks still apply.
    #[arg(long, global = true)]
    confirm_closed: bool,
    /// JSON array of {scope, key, action} overrides; null action unbinds a key.
    #[arg(long, global = true)]
    keymap: Option<PathBuf>,
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
    /// Start interactive study of the current deck, or the named deck.
    Review { deck: Option<String> },
    /// Print every deck with New, Learn and Due counts.
    Decks,
    /// Probe terminal capabilities and display a test image.
    Doctor,
    /// Import an .apkg deck package (never replaces the collection).
    Import(package_commands::ImportArgs),
    /// Export notes as .apkg, or the whole collection as .colpkg.
    Export(package_commands::ExportArgs),
    /// Create, list or restore backups (restore replaces the collection).
    Backup(package_commands::BackupArgs),
    /// Dump fixture card HTML/audio for the Phase 0 comparison harness.
    #[command(hide = true)]
    FixtureDump,
}
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn profile_path(args: &Args) -> Result<PathBuf> {
    let base = args
        .base
        .as_ref()
        .ok_or("Fixture commands require explicit --base during development")?;
    let base = tanki_core::validate_profile(base)?;
    if args.profile.is_empty()
        || args.profile == "."
        || args.profile == ".."
        || args.profile.contains(['/', '\\'])
    {
        return Err("--profile must be a single folder name".into());
    }
    Ok(base.join(&args.profile))
}

/// Always call close, even when the operation failed: close joins the exit backup.
fn with_collection<T>(
    path: &Path,
    confirmed_closed: bool,
    operation: impl FnOnce(&CollectionActor) -> Result<T>,
) -> Result<T> {
    let actor =
        CollectionActor::open_with_options(path, tanki_core::OpenOptions { confirmed_closed })?;
    let result = operation(&actor);
    let closed = actor.close();
    match (result, closed) {
        (Err(error), Err(close)) => {
            Err(format!("{error}; closing the collection also failed: {close}").into())
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
        (Ok(value), Ok(())) => Ok(value),
    }
}

fn load_keymap(path: Option<&Path>) -> Result<Keymap> {
    let mut map = Keymap::default();
    if let Some(path) = path {
        let rows: Vec<serde_json::Value> = serde_json::from_slice(&std::fs::read(path)?)?;
        for (index, row) in rows.into_iter().enumerate() {
            let value = |name| {
                row.get(name)
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| format!("keymap row {} needs {name}", index + 1))
            };
            let scope = Scope::from_name(value("scope")?).ok_or("unknown keymap scope")?;
            let chord = Chord::parse(value("key")?).ok_or("unknown keymap key")?;
            if row.get("action").is_some_and(serde_json::Value::is_null) {
                map.unbind(scope, chord);
                continue;
            }
            let action = Action::from_name(value("action")?).ok_or("unknown keymap action")?;
            map.bind(scope, chord, action);
        }
    }
    Ok(map)
}

fn decks_text(tree: &Deck) -> String {
    fn walk(deck: &Deck, depth: usize, output: &mut String) {
        output.push_str(&format!(
            "{}{:<width$} {:>7} {:>7} {:>7}\n",
            "  ".repeat(depth),
            deck.name,
            deck.new,
            deck.learning,
            deck.review,
            width = 36usize.saturating_sub(depth * 2)
        ));
        for child in &deck.children {
            walk(child, depth + 1, output);
        }
    }
    let mut out = format!("{:<36} {:>7} {:>7} {:>7}\n", "Deck", "New", "Learn", "Due");
    for deck in &tree.children {
        walk(deck, 0, &mut out);
    }
    out
}

fn run(args: Args) -> Result<()> {
    match &args.command {
        Some(CliCommand::Doctor) => {
            let report = tanki_term::SystemTerminalDoctor.run(&tanki_term::DoctorOptions {
                graphics: args.graphics.into(),
                show_image: true,
            })?;
            println!("{}", report.summary);
        }
        Some(CliCommand::Import(command)) => {
            with_collection(&profile_path(&args)?, args.confirm_closed, |actor| {
                package_commands::run_import(actor, command, args.json)
            })?;
        }
        Some(CliCommand::Export(command)) => {
            with_collection(&profile_path(&args)?, args.confirm_closed, |actor| {
                package_commands::run_export(actor, command, args.json)
            })?;
        }
        Some(CliCommand::Backup(command)) => {
            with_collection(&profile_path(&args)?, args.confirm_closed, |actor| {
                package_commands::run_backup(actor, command, args.json)
            })?;
        }
        Some(CliCommand::FixtureDump) => {
            with_collection(&profile_path(&args)?, args.confirm_closed, |actor| {
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
            })?;
        }
        Some(CliCommand::Decks) => {
            let output = with_collection(&profile_path(&args)?, args.confirm_closed, |actor| {
                let tree = backend::deck_tree(actor)?;
                if args.json {
                    Ok(serde_json::to_string_pretty(&tree)?)
                } else {
                    Ok(decks_text(&tree))
                }
            })?;
            // Finish/backup before writing, including when stdout is a closed pipe.
            use std::io::Write;
            writeln!(std::io::stdout().lock(), "{output}")?;
        }
        Some(CliCommand::Review { .. }) | None => {
            if args.json {
                return Err("--json is available for decks, not interactive study".into());
            }
            let keymap = load_keymap(args.keymap.as_deref())?;
            let path = profile_path(&args)?;
            with_collection(&path, args.confirm_closed, |actor| {
                if let Some(CliCommand::Review { deck: Some(name) }) = &args.command {
                    let tree = backend::deck_tree(actor)?;
                    let id = backend::find_deck(&tree, name)
                        .ok_or_else(|| format!("No deck named {name:?}"))?;
                    backend::select_deck(actor, id)?;
                }
                tanki_tui::terminal::run(
                    actor,
                    tanki_tui::terminal::Options {
                        graphics: args.graphics.into(),
                        start: if args.command.is_some() {
                            Start::Review
                        } else {
                            Start::Decks
                        },
                        keymap,
                    },
                )?;
                Ok(())
            })?;
        }
    }
    Ok(())
}
/// PLAN 9 exit codes; see `docs/implementation/phase-2-interfaces.md`.
fn exit_code(error: &(dyn std::error::Error + 'static)) -> u8 {
    if let Some(error) = error.downcast_ref::<package_commands::PackageError>() {
        return error.exit_code();
    }
    match error.downcast_ref::<CoreError>() {
        Some(CoreError::Locked | CoreError::InUse(_)) => 20,
        Some(CoreError::SchemaChangeRequired) => 24,
        Some(CoreError::ProfileBusy(_)) => 25,
        _ => 1,
    }
}
fn main() -> std::process::ExitCode {
    match run(Args::parse()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tanki: {e}");
            std::process::ExitCode::from(exit_code(e.as_ref()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keymap_overrides_and_unbinds_are_reflected_in_hints() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.json");
        std::fs::write(
            &path,
            r#"[
            {"scope":"review","key":"space","action":null},
            {"scope":"review","key":"enter","action":null},
            {"scope":"review","key":"f8","action":"show-answer"}
        ]"#,
        )
        .unwrap();
        let map = load_keymap(Some(&path)).unwrap();
        assert_eq!(
            map.hint(&[Scope::Review, Scope::Global], Action::ShowAnswerOrGood),
            "F8"
        );
        assert_eq!(
            map.lookup(&[Scope::Review], &Chord::parse("space").unwrap()),
            None
        );
    }
}
