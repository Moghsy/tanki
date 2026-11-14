mod note_commands;
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
    /// Anki base folder holding the profiles [default: $ANKI_BASE, then
    /// $XDG_DATA_HOME/Anki2, then ~/.local/share/Anki2, as desktop uses].
    #[arg(long, global = true)]
    base: Option<PathBuf>,
    /// Profile folder name [default: the last one used, or the only one].
    #[arg(long, global = true)]
    profile: Option<String>,
    #[arg(long, global = true, value_enum, default_value = "auto")]
    graphics: GraphicsArg,
    /// Print machine-readable output for commands such as decks.
    #[arg(long, global = true)]
    json: bool,
    /// Open even though collection.anki2-wal exists, after making sure Anki is
    /// closed (it is left behind by a crash). Other-process checks still apply.
    #[arg(long, global = true)]
    confirm_closed: bool,
    /// tanki's config file [default: $XDG_CONFIG_HOME/tanki/config.toml].
    #[arg(long, global = true)]
    config: Option<PathBuf>,
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
    /// Sync with your self-hosted server (`sync login --url URL` first).
    Sync(sync_commands::SyncArgs),
    /// Run Anki's own sync server for your devices (plain HTTP).
    SyncServer(sync_commands::SyncServerArgs),
    /// Import an .apkg deck package (never replaces the collection).
    Import(package_commands::ImportArgs),
    /// Export notes as .apkg, or the whole collection as .colpkg.
    Export(package_commands::ExportArgs),
    /// Create, list or restore backups (restore replaces the collection).
    Backup(package_commands::BackupArgs),
    /// Add a note from flags, a note file, or stdin.
    Add(note_commands::AddArgs),
    /// Edit matching notes in $EDITOR (only changed fields are written).
    Edit(note_commands::EditArgs),
    /// Search cards or notes with Anki's search syntax.
    Search(note_commands::SearchArgs),
    /// Delete matching notes and their cards.
    Delete(note_commands::DeleteArgs),
    /// Dump fixture card HTML/audio for the Phase 0 comparison harness.
    #[command(hide = true)]
    FixtureDump,
}
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Desktop's resolution order (R5 §1.1): `--base`, `ANKI_BASE`, then
/// `$XDG_DATA_HOME/Anki2`, falling back to `~/.local/share/Anki2`.
fn base_dir(args: &Args) -> Result<PathBuf> {
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    args.base
        .clone()
        .or_else(|| var("ANKI_BASE"))
        .or_else(|| var("XDG_DATA_HOME").map(|d| d.join("Anki2")))
        .or_else(|| var("HOME").map(|h| h.join(".local/share/Anki2")))
        .ok_or_else(|| "No Anki base folder: pass --base or set ANKI_BASE".into())
}

/// Profiles are the base's subfolders holding a `collection.anki2`;
/// desktop's `prefs21.db` is never read (PLAN 4.4).
fn profiles(base: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(base)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "addons21" || name.starts_with('.') || !entry.file_type()?.is_dir() {
            continue;
        }
        if entry.path().join("collection.anki2").is_file() {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

fn valid_profile_name(name: &str) -> bool {
    !(name.is_empty()
        || name == "."
        || name == ".."
        || name == "addons21"
        || name.contains(['/', '\\']))
}

/// `--profile`, then the last profile used (if it is still there), then the
/// only profile in the base; otherwise an error listing them.
fn profile_path(args: &Args) -> Result<PathBuf> {
    let base = base_dir(args)?;
    let base = tanki_core::validate_profile(&base)
        .map_err(|e| format!("Anki base folder {}: {e}", base.display()))?;
    if let Some(name) = &args.profile {
        if !valid_profile_name(name) {
            return Err("--profile must be a single folder name".into());
        }
        return Ok(base.join(name));
    }
    let found = profiles(&base)?;
    let last = config_path(args)
        .and_then(|file| tanki_core::config::Config::load(&file).ok())
        .and_then(|config| config.last_profile)
        .filter(|name| found.contains(name));
    match (last, found.as_slice()) {
        (Some(name), _) => Ok(base.join(name)),
        (None, [only]) => Ok(base.join(only)),
        (None, []) => Err(format!("No Anki profiles in {}", base.display()).into()),
        (None, many) => Err(format!(
            "Several profiles in {}: {}. Choose one with --profile NAME.",
            base.display(),
            many.join(", ")
        )
        .into()),
    }
}

/// Always call close, even when the operation failed: close joins the exit backup.
fn config_path(args: &Args) -> Option<PathBuf> {
    args.config
        .clone()
        .or_else(tanki_core::config::default_path)
}

/// Opens the collection. The first open of a collection tanki has not seen
/// forces a backup; later opens follow Anki's backup schedule (PLAN 4.4).
fn with_collection<T>(
    path: &Path,
    args: &Args,
    operation: impl FnOnce(&CollectionActor) -> Result<T>,
) -> Result<T> {
    let config = config_path(args);
    let mut loaded = match &config {
        Some(file) => Some(tanki_core::config::Config::load(file)?),
        None => None,
    };
    let collection = path.canonicalize()?.join("collection.anki2");
    let seen_before = loaded.as_ref().is_some_and(|c| c.has_seen(&collection));
    let actor = CollectionActor::open_with_options(
        path,
        tanki_core::OpenOptions {
            confirmed_closed: args.confirm_closed,
            seen_before,
        },
    )?;
    if let (Some(file), Some(loaded)) = (&config, loaded.as_mut()) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        let profile_changed = loaded.last_profile != name;
        loaded.last_profile = name;
        if loaded.mark_seen(&collection) || profile_changed {
            if let Err(error) = loaded.save(file) {
                eprintln!(
                    "tanki: could not record the collection in {}: {error}",
                    file.display()
                );
            }
        }
    }
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

/// Auto-sync on open and close (PLAN 8.9): only when the profile has a
/// server, a stored login and auto-sync turned on. Never fatal, and never
/// makes a full-sync choice.
fn auto_sync(actor: &CollectionActor, args: &Args, when: &str) {
    use tanki_core::{config, DataCommand, DataResponse, SyncCommand, SyncDirection, SyncRequired};
    let Some(file) = config_path(args) else {
        return;
    };
    let profile = actor.profile_dir().to_string_lossy().into_owned();
    let enabled = config::Config::load(&file).ok().is_some_and(|c| {
        c.profile(&profile).is_some_and(|p| p.sync.auto_sync)
            && c.sync_endpoint(&profile).ok().flatten().is_some()
    });
    if !enabled {
        return;
    }
    let auth = match config::Credentials::load(&config::credentials_path(&file))
        .and_then(|c| c.get(&profile))
    {
        Ok(Some(auth)) => auth,
        _ => {
            eprintln!("tanki: auto sync on {when} skipped: log in with `tanki sync login`.");
            return;
        }
    };
    let run = DataCommand::Sync(SyncCommand::Run {
        auth,
        direction: SyncDirection::Normal,
        media: true,
    });
    match actor.start_data(run).and_then(|pending| pending.wait()) {
        Ok(DataResponse::Synced(report))
            if matches!(report.required, SyncRequired::Full { .. }) =>
        {
            eprintln!(
                "tanki: auto sync on {when} skipped: a one-way full sync must be chosen; run `tanki sync`."
            )
        }
        Ok(_) => {}
        Err(error) => eprintln!("tanki: auto sync on {when} failed: {error}"),
    }
}

fn run(args: Args) -> Result<()> {
    match &args.command {
        Some(CliCommand::SyncServer(command)) => {
            // First, before anything starts a thread: the server sets its
            // users through process environment variables.
            sync_commands::run_server(command)?;
        }
        Some(CliCommand::Sync(command)) => {
            let config = config_path(&args)
                .ok_or("No config folder: set XDG_CONFIG_HOME or HOME, or pass --config")?;
            with_collection(&profile_path(&args)?, &args, |actor| {
                sync_commands::run(actor, command, args.json, &config)
            })?;
        }
        Some(CliCommand::Doctor) => {
            let report = tanki_term::SystemTerminalDoctor.run(&tanki_term::DoctorOptions {
                graphics: args.graphics.into(),
                show_image: true,
            })?;
            println!("{}", report.summary);
        }
        Some(CliCommand::Import(command)) => {
            with_collection(&profile_path(&args)?, &args, |actor| {
                package_commands::run_import(actor, command, args.json)
            })?;
        }
        Some(CliCommand::Export(command)) => {
            with_collection(&profile_path(&args)?, &args, |actor| {
                package_commands::run_export(actor, command, args.json)
            })?;
        }
        Some(CliCommand::Backup(command)) => {
            with_collection(&profile_path(&args)?, &args, |actor| {
                package_commands::run_backup(actor, command, args.json)
            })?;
        }
        Some(CliCommand::Add(command)) => {
            with_collection(&profile_path(&args)?, &args, |actor| {
                note_commands::run_add(actor, command, args.json)
            })?;
        }
        Some(CliCommand::Edit(command)) => {
            with_collection(&profile_path(&args)?, &args, |actor| {
                note_commands::run_edit(actor, command, args.json)
            })?;
        }
        Some(CliCommand::Search(command)) => {
            with_collection(&profile_path(&args)?, &args, |actor| {
                note_commands::run_search(actor, command, args.json)
            })?;
        }
        Some(CliCommand::Delete(command)) => {
            with_collection(&profile_path(&args)?, &args, |actor| {
                note_commands::run_delete(actor, command, args.json)
            })?;
        }
        Some(CliCommand::FixtureDump) => {
            // A development harness: never resolves a default (real) base.
            if args.base.is_none() {
                return Err("fixture-dump requires an explicit --base".into());
            }
            with_collection(&profile_path(&args)?, &args, |actor| {
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
            let output = with_collection(&profile_path(&args)?, &args, |actor| {
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
            with_collection(&path, &args, |actor| {
                if let Some(CliCommand::Review { deck: Some(name) }) = &args.command {
                    let tree = backend::deck_tree(actor)?;
                    let id = backend::find_deck(&tree, name)
                        .ok_or_else(|| format!("No deck named {name:?}"))?;
                    backend::select_deck(actor, id)?;
                }
                auto_sync(actor, &args, "open");
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
                        config: config_path(&args),
                    },
                )?;
                auto_sync(actor, &args, "close");
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
    if let Some(error) = error.downcast_ref::<sync_commands::SyncCliError>() {
        return error.exit_code();
    }
    if let Some(error) = error.downcast_ref::<note_commands::NoteCliError>() {
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
