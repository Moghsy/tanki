//! `tanki import`, `tanki export` and `tanki backup` (PLAN 9, 8.11, 8.12).
//!
//! Safety rules kept here: importing never replaces the collection (a
//! `.colpkg` is refused and pointed at `tanki backup restore`); an import that
//! would force a one-way full sync needs `--confirm-full-sync`; restore needs
//! `--yes` or a typed confirmation; existing export files are only
//! overwritten with `--force`.
// The lead wires these entry points into `main.rs`.
use clap::{Args, Subcommand, ValueEnum};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tanki_core::transfer::{
    CsvDeck, CsvDelimiter, CsvMetadataInfo, CsvMetadataRequest, CsvNotetype, CsvSettings,
    DupeResolution, MatchScope, TextExportSelection, TransferCommand, TransferResponse,
};
use tanki_core::{
    BackupInfo, CollectionActor, Command, CoreError, DataCommand, DataResponse, ExportOptions,
    ExportSelection, ImportLog, ImportOptions, ManageCommand, ManageResponse, NoteCommand,
    PackageCommand, Response, UpdateCondition,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Exit codes shared with `main.rs` (phase-2-interfaces.md).
pub const EXIT_LOCKED: u8 = 20;
pub const EXIT_SCHEMA_CHANGE: u8 = 24;
pub const EXIT_PROFILE_BUSY: u8 = 25;
pub const EXIT_CANCELLED: u8 = 130;

/// Error returned by the package commands; `main` downcasts it for the exit code.
#[derive(Debug)]
pub struct PackageError {
    message: String,
    code: u8,
}
impl PackageError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: 1,
        }
    }
    pub fn exit_code(&self) -> u8 {
        self.code
    }
}
impl std::fmt::Display for PackageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for PackageError {}

fn core_error(error: CoreError) -> PackageError {
    let code = match error {
        CoreError::Locked | CoreError::InUse(_) => EXIT_LOCKED,
        CoreError::SchemaChangeRequired => EXIT_SCHEMA_CHANGE,
        CoreError::ProfileBusy(_) => EXIT_PROFILE_BUSY,
        CoreError::Interrupted => EXIT_CANCELLED,
        _ => 1,
    };
    let message = match error {
        CoreError::SchemaChangeRequired => "This import changes note types in a way that requires a one-way full sync (upload) on your next sync, and every other device must then download. Nothing was imported. Re-run with --confirm-full-sync to continue.".to_owned(),
        other => other.to_string(),
    };
    PackageError { message, code }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum UpdateArg {
    IfNewer,
    Always,
    Never,
}
impl From<UpdateArg> for UpdateCondition {
    fn from(value: UpdateArg) -> Self {
        match value {
            UpdateArg::IfNewer => Self::IfNewer,
            UpdateArg::Always => Self::Always,
            UpdateArg::Never => Self::Never,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum DelimiterArg {
    Tab,
    Pipe,
    Semicolon,
    Colon,
    Comma,
    Space,
}
impl From<DelimiterArg> for CsvDelimiter {
    fn from(value: DelimiterArg) -> Self {
        match value {
            DelimiterArg::Tab => Self::Tab,
            DelimiterArg::Pipe => Self::Pipe,
            DelimiterArg::Semicolon => Self::Semicolon,
            DelimiterArg::Colon => Self::Colon,
            DelimiterArg::Comma => Self::Comma,
            DelimiterArg::Space => Self::Space,
        }
    }
}

/// What to do with a text row that matches an existing note.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum DupesArg {
    /// Update the existing note.
    Update,
    /// Keep the existing note, skip the row.
    Keep,
    /// Import the row as a new note anyway.
    Add,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum MatchScopeArg {
    Notetype,
    Deck,
}

/// `tanki import <file> [options]`: an .apkg deck package, a text file
/// (.csv, .tsv, .txt) or a ForeignData .json file.
#[derive(Args, Debug, Clone)]
pub struct ImportArgs {
    /// The file to import: .apkg, .csv/.tsv/.txt or .json. Collection
    /// packages (.colpkg) are restored with `tanki backup restore` instead.
    pub file: PathBuf,
    /// Text files: print the detected settings and the first rows, import
    /// nothing.
    #[arg(long)]
    pub preview: bool,
    /// Text files: the note type to import into (overrides the file's header).
    #[arg(long, value_name = "NAME")]
    pub notetype: Option<String>,
    /// Text files: the deck to import into; created if missing.
    #[arg(long, value_name = "NAME")]
    pub deck: Option<String>,
    /// Text files: field to column mapping, e.g. `Front=1,Back=2` (columns
    /// count from 1; unlisted fields stay empty).
    #[arg(long, value_name = "FIELD=COL,...", value_delimiter = ',')]
    pub map: Vec<String>,
    /// Text files: the column holding space-separated tags.
    #[arg(long, value_name = "COL")]
    pub tags_column: Option<u32>,
    /// Text files: the column holding each note's GUID.
    #[arg(long, value_name = "COL")]
    pub guid_column: Option<u32>,
    /// Text files: tags added to every imported note.
    #[arg(long, value_name = "TAGS")]
    pub global_tags: Option<String>,
    /// Text and JSON files: what to do with rows that match existing notes.
    #[arg(long, value_enum)]
    pub dupes: Option<DupesArg>,
    /// Text files: match existing notes within the note type or the deck too.
    #[arg(long, value_enum)]
    pub match_scope: Option<MatchScopeArg>,
    /// Text files: the field delimiter (detected otherwise).
    #[arg(long, value_enum)]
    pub delimiter: Option<DelimiterArg>,
    /// Text files: whether the fields contain HTML (detected otherwise).
    #[arg(long)]
    pub html: Option<bool>,
    /// Merge incoming note types into existing ones. May require a full sync.
    #[arg(long)]
    pub merge_notetypes: bool,
    /// When to update existing notes.
    #[arg(long, value_enum)]
    pub update_notes: Option<UpdateArg>,
    /// When to update existing note types.
    #[arg(long, value_enum)]
    pub update_notetypes: Option<UpdateArg>,
    /// Import review history and scheduling.
    #[arg(long)]
    pub with_scheduling: bool,
    /// Import deck option presets.
    #[arg(long)]
    pub with_deck_configs: bool,
    /// Accept that this import may force a one-way full sync.
    #[arg(long)]
    pub confirm_full_sync: bool,
    /// Print up to this many notes of each log category.
    #[arg(long, value_name = "N", default_value_t = 0)]
    pub show: usize,
}
impl ImportArgs {
    /// `None` (the options saved by the last import) unless an option flag
    /// was given; given flags start from Anki's defaults, not the saved ones.
    pub fn options(&self) -> Option<ImportOptions> {
        let given = self.merge_notetypes
            || self.update_notes.is_some()
            || self.update_notetypes.is_some()
            || self.with_scheduling
            || self.with_deck_configs;
        given.then(|| ImportOptions {
            merge_notetypes: self.merge_notetypes,
            update_notes: self.update_notes.map(Into::into).unwrap_or_default(),
            update_notetypes: self.update_notetypes.map(Into::into).unwrap_or_default(),
            with_scheduling: self.with_scheduling,
            with_deck_configs: self.with_deck_configs,
        })
    }
}

/// `tanki export <file> [--deck D | --query Q] [options]`
#[derive(Args, Debug, Clone)]
pub struct ExportArgs {
    /// Output file: .apkg for decks, .colpkg for the whole collection, or
    /// .txt/.tsv/.csv for notes (or cards, with --cards) as text.
    pub file: PathBuf,
    /// Text export: write cards (rendered question and answer) instead of notes.
    #[arg(long)]
    pub cards: bool,
    /// Text export: keep HTML in the fields.
    #[arg(long)]
    pub with_html: bool,
    /// Text export of notes: add a tags column.
    #[arg(long)]
    pub with_tags: bool,
    /// Text export of notes: add a deck column.
    #[arg(long)]
    pub with_deck_column: bool,
    /// Text export of notes: add a note type column.
    #[arg(long)]
    pub with_notetype: bool,
    /// Text export of notes: add a GUID column (lets a re-import match).
    #[arg(long)]
    pub with_guid: bool,
    /// Export this deck (full name, e.g. "Spanish::Verbs") and its subdecks.
    #[arg(long, conflicts_with = "query")]
    pub deck: Option<String>,
    /// Export the notes matching this Anki search.
    #[arg(long)]
    pub query: Option<String>,
    /// Include scheduling and review history.
    #[arg(long)]
    pub with_scheduling: bool,
    /// Include deck option presets.
    #[arg(long)]
    pub with_deck_configs: bool,
    /// Leave media files out.
    #[arg(long)]
    pub no_media: bool,
    /// Write a package that older Anki versions can read.
    #[arg(long)]
    pub legacy: bool,
    /// Export the whole collection as a collection package (implied by .colpkg).
    #[arg(long)]
    pub colpkg: bool,
    /// Overwrite the output file if it exists.
    #[arg(long)]
    pub force: bool,
}

/// `tanki backup [create|list|restore <file>]`
#[derive(Args, Debug, Clone)]
pub struct BackupArgs {
    #[command(subcommand)]
    pub command: Option<BackupCommand>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum BackupCommand {
    /// Create a backup now (the default).
    Create {
        /// Back up even if the collection is unchanged since the last backup.
        #[arg(long)]
        force: bool,
    },
    /// List the profile's backups.
    List,
    /// Replace the collection with a backup or another .colpkg file.
    Restore {
        /// A .colpkg file, or the file name of a backup in the profile's backups folder.
        file: PathBuf,
        /// Do not ask for confirmation.
        #[arg(long)]
        yes: bool,
    },
}

pub fn run_import(actor: &CollectionActor, args: &ImportArgs, json: bool) -> Result<()> {
    let output = import(actor, args, json)?;
    print(&output)
}

pub fn run_export(actor: &CollectionActor, args: &ExportArgs, json: bool) -> Result<()> {
    let output = export(actor, args, json)?;
    print(&output)
}

pub fn run_backup(actor: &CollectionActor, args: &BackupArgs, json: bool) -> Result<()> {
    let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let output = backup(actor, args, json, interactive, &mut || {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        Ok(line)
    })?;
    print(&output)
}

fn print(output: &str) -> Result<()> {
    if !output.is_empty() {
        writeln!(std::io::stdout().lock(), "{output}")?;
    }
    Ok(())
}

fn import(actor: &CollectionActor, args: &ImportArgs, json: bool) -> Result<String> {
    if has_extension(&args.file, "colpkg") {
        return Err(PackageError::new(format!(
            "{} is a collection package: importing it would replace your whole collection. To do that, run `tanki backup restore {}` (a backup is taken first).",
            args.file.display(),
            args.file.display()
        ))
        .into());
    }
    if has_extension(&args.file, "json") {
        return import_json(actor, args, json);
    }
    if ["csv", "tsv", "txt", "text"]
        .iter()
        .any(|e| has_extension(&args.file, e))
    {
        return import_text_file(actor, args, json);
    }
    let response = run_data(
        actor,
        DataCommand::Package(PackageCommand::Import {
            path: absolute(&args.file)?,
            options: args.options(),
            schema_confirmed: args.confirm_full_sync,
        }),
    )?;
    let DataResponse::Imported { log, changes } = response else {
        return Err(unexpected());
    };
    Ok(if json {
        serde_json::to_string_pretty(&serde_json::json!({
            "file": args.file,
            "log": log,
            "changes": changes,
        }))?
    } else {
        import_text(&args.file, &log, args.show)
    })
}

fn import_text(file: &Path, log: &ImportLog, show: usize) -> String {
    let mut out = format!(
        "Imported {}: {} notes found, {} new, {} updated, {} duplicate (unchanged), {} conflicting (note type differs).",
        file.display(),
        log.found_notes,
        log.new,
        log.updated,
        log.duplicate,
        log.conflicting
    );
    for (category, notes) in &log.entries {
        let skipped = ["new", "updated", "duplicate", "conflicting"];
        if !skipped.contains(&category.as_str()) {
            out.push_str(&format!(
                "\n{}: {}",
                category.replace('_', " "),
                notes.len()
            ));
        }
    }
    if show > 0 {
        for (category, notes) in &log.entries {
            out.push_str(&format!("\n[{}]", category.replace('_', " ")));
            for fields in notes.iter().take(show) {
                out.push_str(&format!("\n  {}", fields.join(" | ")));
            }
            if notes.len() > show {
                out.push_str(&format!("\n  ... {} more", notes.len() - show));
            }
        }
    }
    out
}

fn export(actor: &CollectionActor, args: &ExportArgs, json: bool) -> Result<String> {
    if ["txt", "tsv", "csv", "text"]
        .iter()
        .any(|e| has_extension(&args.file, e))
    {
        return export_text(actor, args, json);
    }
    let collection = args.colpkg || has_extension(&args.file, "colpkg");
    if collection
        && (args.deck.is_some()
            || args.query.is_some()
            || args.with_scheduling
            || args.with_deck_configs)
    {
        return Err(PackageError::new(
            "A collection package (.colpkg) always holds the whole collection with its scheduling; --deck, --query, --with-scheduling and --with-deck-configs apply to .apkg exports.",
        )
        .into());
    }
    let expected = if collection { "colpkg" } else { "apkg" };
    if !has_extension(&args.file, expected) {
        return Err(
            PackageError::new(format!("The output file name must end in .{expected}")).into(),
        );
    }
    if args.file.exists() && !args.force {
        return Err(PackageError::new(format!(
            "{} exists; use --force to overwrite it",
            args.file.display()
        ))
        .into());
    }
    let path = absolute(&args.file)?;
    if collection {
        let response = run_data(
            actor,
            DataCommand::Package(PackageCommand::ExportCollection {
                path,
                media: !args.no_media,
                legacy: args.legacy,
            }),
        )?;
        if !matches!(response, DataResponse::Done) {
            return Err(unexpected());
        }
        return Ok(if json {
            serde_json::json!({ "file": args.file, "collection": true }).to_string()
        } else {
            format!("Exported the whole collection to {}", args.file.display())
        });
    }
    let selection = match (&args.deck, &args.query) {
        (Some(name), _) => ExportSelection::Deck(deck_id(actor, name)?),
        (None, Some(query)) => ExportSelection::Query(query.clone()),
        (None, None) => ExportSelection::All,
    };
    let response = run_data(
        actor,
        DataCommand::Package(PackageCommand::Export {
            path,
            options: ExportOptions {
                with_scheduling: args.with_scheduling,
                with_deck_configs: args.with_deck_configs,
                with_media: !args.no_media,
                legacy: args.legacy,
            },
            selection,
        }),
    )?;
    let DataResponse::Exported { notes } = response else {
        return Err(unexpected());
    };
    Ok(if json {
        serde_json::json!({ "file": args.file, "notes": notes }).to_string()
    } else {
        format!("Exported {notes} notes to {}", args.file.display())
    })
}

/// Runs a Phase 5 transfer command as a long operation with progress.
fn run_transfer(actor: &CollectionActor, command: TransferCommand) -> Result<TransferResponse> {
    let operation = actor
        .start(Command::Manage(ManageCommand::Transfer(command)))
        .map_err(core_error)?;
    let live = std::io::stderr().is_terminal();
    let mut shown = String::new();
    let result = loop {
        match operation.try_recv() {
            Ok(Some(Response::Manage(ManageResponse::Transfer(response)))) => break Ok(response),
            Ok(Some(_)) => break Err(unexpected()),
            Err(error) => break Err(core_error(error).into()),
            Ok(None) => {}
        }
        if live {
            let line = progress_line(&actor.progress());
            if line != shown {
                eprint!("\r\x1b[K{line}");
                shown = line;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if live && !shown.is_empty() {
        eprint!("\r\x1b[K");
    }
    result
}

fn imported_report(
    args: &ImportArgs,
    log: &ImportLog,
    changes: &tanki_core::OpChanges,
    json: bool,
) -> Result<String> {
    Ok(if json {
        serde_json::to_string_pretty(&serde_json::json!({
            "file": args.file,
            "log": log,
            "changes": changes,
        }))?
    } else {
        import_text(&args.file, log, args.show)
    })
}

fn import_json(actor: &CollectionActor, args: &ImportArgs, json: bool) -> Result<String> {
    let response = run_transfer(
        actor,
        TransferCommand::ImportJson {
            path: absolute(&args.file)?,
            schema_confirmed: args.confirm_full_sync,
        },
    )?;
    let TransferResponse::Imported { log, changes } = response else {
        return Err(unexpected());
    };
    imported_report(args, &log, &changes, json)
}

fn import_text_file(actor: &CollectionActor, args: &ImportArgs, json: bool) -> Result<String> {
    let path = absolute(&args.file)?;
    let notetype_id = match &args.notetype {
        Some(name) => Some(notetype_by_name(actor, name)?.0),
        None => None,
    };
    let request = CsvMetadataRequest {
        path: path.clone(),
        delimiter: args.delimiter.map(Into::into),
        notetype_id,
        deck_id: None,
        is_html: args.html,
    };
    let TransferResponse::CsvMetadata(info) =
        run_transfer(actor, TransferCommand::CsvMetadata(request))?
    else {
        return Err(unexpected());
    };
    let settings = apply_text_overrides(args, &info)?;
    if args.preview {
        return Ok(if json {
            serde_json::to_string_pretty(&serde_json::json!({
                "settings": settings,
                "preview": info.preview,
                "notetype": info.notetype_name,
                "fields": info.field_names,
            }))?
        } else {
            preview_text(&settings, &info)
        });
    }
    let response = run_transfer(actor, TransferCommand::ImportCsv { path, settings })?;
    let TransferResponse::Imported { log, changes } = response else {
        return Err(unexpected());
    };
    imported_report(args, &log, &changes, json)
}

/// The detected settings with the command line's choices applied.
fn apply_text_overrides(args: &ImportArgs, info: &CsvMetadataInfo) -> Result<CsvSettings> {
    let mut settings = info.settings.clone();
    if let Some(delimiter) = args.delimiter {
        settings.delimiter = delimiter.into();
    }
    if let Some(html) = args.html {
        settings.is_html = html;
    }
    if let Some(deck) = &args.deck {
        settings.deck = Some(CsvDeck::Name(deck.clone()));
    }
    if let Some(column) = args.tags_column {
        settings.tags_column = column;
    }
    if let Some(column) = args.guid_column {
        settings.guid_column = column;
    }
    if let Some(tags) = &args.global_tags {
        settings.global_tags = tags.split_whitespace().map(str::to_owned).collect();
    }
    if let Some(dupes) = args.dupes {
        settings.dupe_resolution = match dupes {
            DupesArg::Update => DupeResolution::Update,
            DupesArg::Keep => DupeResolution::Preserve,
            DupesArg::Add => DupeResolution::Duplicate,
        };
    }
    if let Some(scope) = args.match_scope {
        settings.match_scope = match scope {
            MatchScopeArg::Notetype => MatchScope::Notetype,
            MatchScopeArg::Deck => MatchScope::NotetypeAndDeck,
        };
    }
    if !args.map.is_empty() {
        let Some(CsvNotetype::Global { id, .. }) = &settings.notetype else {
            return Err(PackageError::new(
                "--map needs one note type for the whole file: give --notetype or a #notetype: header",
            )
            .into());
        };
        let columns = info.settings.columns();
        let mut field_columns = vec![0; info.field_names.len()];
        for entry in &args.map {
            let (name, column) = entry.split_once('=').ok_or_else(|| {
                PackageError::new(format!("--map expects FIELD=COLUMN, got {entry:?}"))
            })?;
            let at = info
                .field_names
                .iter()
                .position(|f| f.eq_ignore_ascii_case(name.trim()))
                .ok_or_else(|| {
                    PackageError::new(format!(
                        "The note type has no field {name:?} (fields: {})",
                        info.field_names.join(", ")
                    ))
                })?;
            let column: u32 = column.trim().parse().map_err(|_| {
                PackageError::new(format!("{column:?} is not a column number (from 1)"))
            })?;
            if column as usize > columns {
                return Err(PackageError::new(format!(
                    "Column {column} does not exist: the file has {columns}"
                ))
                .into());
            }
            field_columns[at] = column;
        }
        settings.notetype = Some(CsvNotetype::Global {
            id: *id,
            field_columns,
        });
    }
    Ok(settings)
}

fn preview_text(settings: &CsvSettings, info: &CsvMetadataInfo) -> String {
    let mut out = format!(
        "Delimiter: {}\nHTML: {}\nColumns: {}",
        settings.delimiter.name(),
        settings.is_html,
        settings.columns()
    );
    match (&settings.notetype, &info.notetype_name) {
        (Some(CsvNotetype::Global { field_columns, .. }), Some(name)) => {
            out.push_str(&format!("\nNote type: {name}"));
            for (field, column) in info.field_names.iter().zip(field_columns) {
                match column {
                    0 => out.push_str(&format!("\n  {field}: (none)")),
                    column => out.push_str(&format!("\n  {field}: column {column}")),
                }
            }
        }
        (Some(CsvNotetype::Column(c)), _) => {
            out.push_str(&format!("\nNote type: named in column {c}"))
        }
        _ => out.push_str("\nNote type: none chosen (use --notetype)"),
    }
    match &settings.deck {
        Some(CsvDeck::Name(n)) => out.push_str(&format!("\nDeck: {n}")),
        Some(CsvDeck::Id(id)) => out.push_str(&format!("\nDeck: id {id}")),
        Some(CsvDeck::Column(c)) => out.push_str(&format!("\nDeck: named in column {c}")),
        None => {}
    }
    out.push_str(&format!(
        "\nIf a note matches: {}; matched within: {}",
        settings.dupe_resolution.name(),
        settings.match_scope.name()
    ));
    for row in &info.preview {
        out.push_str(&format!("\n  {}", row.join(" | ")));
    }
    out
}

/// A note type's id and field names by name (case-insensitive).
fn notetype_by_name(actor: &CollectionActor, name: &str) -> Result<(i64, Vec<String>)> {
    let Response::Edit(tanki_core::EditResponse::Notetypes(all)) = actor
        .execute(Command::Notes(NoteCommand::Notetypes))
        .map_err(core_error)?
    else {
        return Err(unexpected());
    };
    all.iter()
        .find(|nt| nt.name.eq_ignore_ascii_case(name))
        .map(|nt| (nt.id, nt.fields.iter().map(|f| f.name.clone()).collect()))
        .ok_or_else(|| PackageError::new(format!("No note type named {name:?}")).into())
}

fn export_text(actor: &CollectionActor, args: &ExportArgs, json: bool) -> Result<String> {
    if args.colpkg || args.with_scheduling || args.with_deck_configs || args.legacy || args.no_media
    {
        return Err(PackageError::new(
            "A text export has no scheduling, deck options, media or package format; --colpkg, --with-scheduling, --with-deck-configs, --no-media and --legacy apply to .apkg and .colpkg files.",
        )
        .into());
    }
    if args.cards
        && (args.with_tags || args.with_deck_column || args.with_notetype || args.with_guid)
    {
        return Err(PackageError::new(
            "Card text has only a question and an answer column; --with-tags, --with-deck-column, --with-notetype and --with-guid apply to notes.",
        )
        .into());
    }
    if args.file.exists() && !args.force {
        return Err(PackageError::new(format!(
            "{} exists; use --force to overwrite it",
            args.file.display()
        ))
        .into());
    }
    let selection = match (&args.deck, &args.query) {
        (Some(name), _) => TextExportSelection::Deck(deck_id(actor, name)?),
        (None, Some(query)) => TextExportSelection::Query(query.clone()),
        (None, None) => TextExportSelection::All,
    };
    let path = absolute(&args.file)?;
    let command = if args.cards {
        TransferCommand::ExportCards {
            path,
            selection,
            with_html: args.with_html,
        }
    } else {
        TransferCommand::ExportNotes {
            path,
            selection,
            with_html: args.with_html,
            with_tags: args.with_tags,
            with_deck: args.with_deck_column,
            with_notetype: args.with_notetype,
            with_guid: args.with_guid,
        }
    };
    let TransferResponse::Exported { count } = run_transfer(actor, command)? else {
        return Err(unexpected());
    };
    let what = if args.cards { "cards" } else { "notes" };
    Ok(if json {
        serde_json::json!({ "file": args.file, what: count }).to_string()
    } else {
        format!("Exported {count} {what} to {}", args.file.display())
    })
}

fn deck_id(actor: &CollectionActor, name: &str) -> Result<i64> {
    let Response::DeckTree(tree) = actor.execute(Command::DeckTree).map_err(core_error)? else {
        return Err(unexpected());
    };
    tanki_tui::backend::find_deck(&tree, name)
        .ok_or_else(|| PackageError::new(format!("No deck named {name:?}")).into())
}

const RESTORE_WORD: &str = "restore";

fn backup(
    actor: &CollectionActor,
    args: &BackupArgs,
    json: bool,
    interactive: bool,
    read_line: &mut dyn FnMut() -> std::io::Result<String>,
) -> Result<String> {
    match args
        .command
        .clone()
        .unwrap_or(BackupCommand::Create { force: false })
    {
        BackupCommand::Create { force } => {
            let response = run_data(actor, DataCommand::Backup { force })?;
            Ok(match response {
                DataResponse::Backups(list) if json => serde_json::to_string_pretty(&list)?,
                DataResponse::Backups(list) => match list.first() {
                    Some(info) => format!("Backup written: {}", info.path.display()),
                    None => "The collection is unchanged since the last backup; nothing written (use --force).".into(),
                },
                DataResponse::Done if json => serde_json::json!({ "created": true }).to_string(),
                DataResponse::Done => "Backup created.".into(),
                _ => return Err(unexpected()),
            })
        }
        BackupCommand::List => {
            let DataResponse::Backups(list) = run_data(actor, DataCommand::ListBackups)? else {
                return Err(unexpected());
            };
            Ok(if json {
                serde_json::to_string_pretty(&list)?
            } else {
                backups_text(&list)
            })
        }
        BackupCommand::Restore { file, yes } => {
            let path = restore_path(actor, &file)?;
            confirm_restore(&path, yes, interactive, read_line)?;
            let response = run_data(
                actor,
                DataCommand::Package(PackageCommand::Restore {
                    path: path.clone(),
                    confirmed: true,
                }),
            )?;
            if !matches!(response, DataResponse::Done | DataResponse::Backups(_)) {
                return Err(unexpected());
            }
            Ok(if json {
                serde_json::json!({ "restored": path }).to_string()
            } else {
                format!(
                    "Restored the collection from {}. A backup of the previous collection was taken first. The next sync will be a one-way full sync.",
                    path.display()
                )
            })
        }
    }
}

fn backups_text(list: &[BackupInfo]) -> String {
    if list.is_empty() {
        return "No backups.".into();
    }
    list.iter()
        .map(|b| {
            let name = b.path.file_name().unwrap_or(b.path.as_os_str());
            format!("{:>10}  {}", human_bytes(b.bytes), name.to_string_lossy())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn human_bytes(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MiB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1} KiB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

/// A `.colpkg` path, or a bare file name looked up in `<profile>/backups`.
fn restore_path(actor: &CollectionActor, file: &Path) -> Result<PathBuf> {
    let in_backups = actor.profile_dir().join("backups").join(file);
    let path = if file.exists() {
        file.to_path_buf()
    } else if file.components().count() == 1 && in_backups.is_file() {
        in_backups
    } else {
        return Err(PackageError::new(format!("No such file: {}", file.display())).into());
    };
    if !has_extension(&path, "colpkg") {
        return Err(PackageError::new(
            "Only collection packages (.colpkg, as in the backups folder) can be restored; use `tanki import` for .apkg decks.",
        )
        .into());
    }
    absolute(&path)
}

/// Destructive: needs `--yes`, or the typed word on an interactive terminal.
fn confirm_restore(
    path: &Path,
    yes: bool,
    interactive: bool,
    read_line: &mut dyn FnMut() -> std::io::Result<String>,
) -> Result<()> {
    let warning = format!(
        "This replaces your whole collection with {}. A backup of the current collection is taken first; changes since that backup that are not in the restored file will be gone, and the next sync will be a one-way full sync.",
        path.display()
    );
    if yes {
        eprintln!("{warning}");
        return Ok(());
    }
    if !interactive {
        return Err(PackageError::new(format!("{warning}\nRe-run with --yes to confirm.")).into());
    }
    eprint!("{warning}\nType {RESTORE_WORD:?} to continue: ");
    std::io::stderr().flush()?;
    if read_line()?.trim() == RESTORE_WORD {
        Ok(())
    } else {
        Err(PackageError::new("Not confirmed; nothing was changed.").into())
    }
}

/// Starts a data operation and polls it, showing progress on an interactive stderr.
fn run_data(actor: &CollectionActor, command: DataCommand) -> Result<DataResponse> {
    let operation = actor.start_data(command).map_err(core_error)?;
    let live = std::io::stderr().is_terminal();
    let mut shown = String::new();
    let result = loop {
        match operation.try_recv() {
            Ok(Some(response)) => break Ok(response),
            Err(error) => break Err(core_error(error)),
            Ok(None) => {}
        }
        if live {
            let line = progress_line(&actor.progress());
            if line != shown {
                eprint!("\r\x1b[K{line}");
                shown = line;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    if live && !shown.is_empty() {
        eprint!("\r\x1b[K");
    }
    Ok(result?)
}

fn progress_line(progress: &tanki_core::OperationProgress) -> String {
    match (progress.current, progress.total) {
        (Some(current), Some(total)) => format!("{} {current}/{total}", progress.stage),
        (Some(current), None) => format!("{} {current}", progress.stage),
        _ => progress.stage.clone(),
    }
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(extension))
}

/// The actor resolves paths itself; make relative ones independent of it.
fn absolute(path: &Path) -> Result<PathBuf> {
    Ok(std::path::absolute(path)?)
}

fn unexpected() -> Box<dyn std::error::Error> {
    PackageError::new("Unexpected response from the collection").into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        command: Top,
    }
    #[derive(clap::Subcommand)]
    enum Top {
        Import(ImportArgs),
        Export(ExportArgs),
        Backup(BackupArgs),
    }
    fn parse(args: &[&str]) -> Top {
        Cli::try_parse_from(std::iter::once("tanki").chain(args.iter().copied()))
            .unwrap()
            .command
    }
    fn import_args(args: &[&str]) -> ImportArgs {
        match parse(&[&["import"], args].concat()) {
            Top::Import(a) => a,
            _ => unreachable!(),
        }
    }
    fn export_args(args: &[&str]) -> ExportArgs {
        match parse(&[&["export"], args].concat()) {
            Top::Export(a) => a,
            _ => unreachable!(),
        }
    }
    fn backup_args(args: &[&str]) -> BackupArgs {
        match parse(&[&["backup"], args].concat()) {
            Top::Backup(a) => a,
            _ => unreachable!(),
        }
    }
    fn code(error: &(dyn std::error::Error + 'static)) -> u8 {
        error
            .downcast_ref::<PackageError>()
            .map(PackageError::exit_code)
            .unwrap_or(0)
    }
    fn s(path: &Path) -> &str {
        path.to_str().unwrap()
    }

    /// A temporary profile with the generated fixtures.
    fn fixture() -> (tempfile::TempDir, CollectionActor) {
        let dir = tempfile::tempdir().unwrap();
        let actor = CollectionActor::open(dir.path()).unwrap();
        actor.execute(Command::GenerateFixture).unwrap();
        actor.execute(Command::GenerateStudyFixture).unwrap();
        (dir, actor)
    }

    #[test]
    fn import_options_default_to_presets_unless_a_flag_is_given() {
        assert!(import_args(&["a.apkg"]).options().is_none());
        assert!(
            import_args(&["a.apkg", "--confirm-full-sync", "--show", "3"])
                .options()
                .is_none()
        );
        let options = import_args(&["a.apkg", "--update-notes", "never", "--with-scheduling"])
            .options()
            .unwrap();
        assert!(matches!(options.update_notes, UpdateCondition::Never));
        assert!(matches!(options.update_notetypes, UpdateCondition::IfNewer));
        assert!(options.with_scheduling && !options.merge_notetypes);
        assert!(
            Cli::try_parse_from(["tanki", "export", "a.apkg", "--deck", "x", "--query", "y"])
                .is_err()
        );
        assert!(backup_args(&[]).command.is_none());
        assert!(matches!(
            backup_args(&["restore", "b.colpkg", "--yes"]).command,
            Some(BackupCommand::Restore { yes: true, .. })
        ));
    }

    #[test]
    fn export_then_import_round_trip_with_json_and_text_reports() {
        let (_source, a) = fixture();
        let out = tempfile::tempdir().unwrap();
        let file = out.path().join("deck.apkg");
        let report = export(
            &a,
            &export_args(&[s(&file), "--deck", "study::vocabulary"]),
            true,
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&report).unwrap();
        let notes = value["notes"].as_u64().unwrap();
        assert!(notes > 0);
        // Existing files need --force.
        let error = export(&a, &export_args(&[s(&file)]), false).unwrap_err();
        assert!(error.to_string().contains("--force"), "{error}");
        let text = export(
            &a,
            &export_args(&[s(&file), "--force", "--query", "tag:golden::*"]),
            false,
        )
        .unwrap();
        assert!(text.starts_with("Exported "), "{text}");
        let error = export(
            &a,
            &export_args(&[s(&file), "--force", "--deck", "Nope"]),
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("No deck named"), "{error}");
        a.close().unwrap();

        let target = tempfile::tempdir().unwrap();
        let b = CollectionActor::open(target.path()).unwrap();
        let report = import(&b, &import_args(&[s(&file), "--show", "1"]), false).unwrap();
        assert!(report.contains(" new,"), "{report}");
        assert!(report.contains("[new]"), "{report}");
        let report = import(&b, &import_args(&[s(&file)]), true).unwrap();
        let value: serde_json::Value = serde_json::from_str(&report).unwrap();
        assert_eq!(value["log"]["new"], 0);
        assert!(value["log"]["duplicate"].as_u64().unwrap() > 0);
        b.close().unwrap();
    }

    #[test]
    fn colpkg_import_is_refused_and_points_to_restore() {
        let dir = tempfile::tempdir().unwrap();
        let actor = CollectionActor::open(dir.path()).unwrap();
        let error = import(&actor, &import_args(&["backup.colpkg"]), false).unwrap_err();
        assert!(
            error.to_string().contains("tanki backup restore"),
            "{error}"
        );
        actor.close().unwrap();
    }

    #[test]
    fn schema_risk_import_needs_confirmation_with_exit_code_24() {
        let (_source, a) = fixture();
        let out = tempfile::tempdir().unwrap();
        let file = out.path().join("all.apkg");
        export(&a, &export_args(&[s(&file)]), false).unwrap();
        a.close().unwrap();
        let target = tempfile::tempdir().unwrap();
        let b = CollectionActor::open(target.path()).unwrap();
        let error = import(&b, &import_args(&[s(&file), "--merge-notetypes"]), false).unwrap_err();
        assert_eq!(code(error.as_ref()), EXIT_SCHEMA_CHANGE);
        assert!(error.to_string().contains("--confirm-full-sync"), "{error}");
        let report = import(
            &b,
            &import_args(&[s(&file), "--merge-notetypes", "--confirm-full-sync"]),
            true,
        )
        .unwrap();
        assert!(report.contains("\"new\""), "{report}");
        b.close().unwrap();
    }

    #[test]
    fn export_extension_and_flag_rules() {
        let dir = tempfile::tempdir().unwrap();
        let actor = CollectionActor::open(dir.path()).unwrap();
        let out = tempfile::tempdir().unwrap();
        let profile_file = dir.path().join("collection.anki2");
        for args in [
            vec![s(&profile_file).to_owned(), "--force".into()],
            vec![
                s(&out.path().join("x.colpkg")).to_owned(),
                "--deck".into(),
                "Default".into(),
            ],
            vec![s(&out.path().join("x.apkg")).to_owned(), "--colpkg".into()],
        ] {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            assert!(
                export(&actor, &export_args(&args), false).is_err(),
                "{args:?}"
            );
        }
        assert!(profile_file.is_file());
        actor.close().unwrap();
    }

    #[test]
    fn restore_confirmation_is_explicit() {
        let path = Path::new("/backups/backup-1.colpkg");
        let never = &mut || -> std::io::Result<String> { panic!("must not prompt") };
        assert!(confirm_restore(path, true, false, never).is_ok());
        let error = confirm_restore(path, false, false, never).unwrap_err();
        assert!(error.to_string().contains("--yes"), "{error}");
        assert!(error
            .to_string()
            .contains("backup of the current collection is taken first"));
        assert!(confirm_restore(path, false, true, &mut || Ok("restore\n".into())).is_ok());
        assert!(confirm_restore(path, false, true, &mut || Ok("y\n".into())).is_err());
    }

    #[test]
    fn restore_refuses_non_colpkg_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let actor = CollectionActor::open(dir.path()).unwrap();
        let out = tempfile::tempdir().unwrap();
        let apkg = out.path().join("deck.apkg");
        std::fs::write(&apkg, b"x").unwrap();
        let never = &mut || -> std::io::Result<String> { panic!("must not prompt") };
        for file in [s(&apkg), "missing.colpkg"] {
            let args = backup_args(&["restore", file, "--yes"]);
            assert!(
                backup(&actor, &args, false, false, never).is_err(),
                "{file}"
            );
        }
        actor.close().unwrap();
    }

    #[test]
    fn report_text_lists_categories() {
        let mut log = ImportLog {
            new: 1,
            found_notes: 2,
            duplicate: 1,
            ..Default::default()
        };
        log.entries
            .insert("new".into(), vec![vec!["a".into(), "b".into()]]);
        log.entries.insert(
            "missing_deck".into(),
            vec![vec!["c".into()], vec!["d".into()]],
        );
        let text = import_text(Path::new("x.apkg"), &log, 1);
        assert!(
            text.contains("2 notes found, 1 new, 0 updated, 1 duplicate"),
            "{text}"
        );
        assert!(text.contains("\nmissing deck: 2"), "{text}");
        assert!(text.contains("[new]\n  a | b"), "{text}");
        assert!(text.contains("... 1 more"), "{text}");
        assert_eq!(human_bytes(2048), "2.0 KiB");
    }

    #[test]
    fn backup_create_list_and_restore() {
        let (dir, actor) = fixture();
        let never = &mut || -> std::io::Result<String> { panic!("must not prompt") };
        backup(
            &actor,
            &backup_args(&["create", "--force"]),
            false,
            false,
            never,
        )
        .unwrap();
        let listed = backup(&actor, &backup_args(&["list"]), true, false, never).unwrap();
        let list: Vec<BackupInfo> = serde_json::from_str(&listed).unwrap();
        assert!(!list.is_empty());
        let name = list[0]
            .path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let restored = backup(
            &actor,
            &backup_args(&["restore", &name, "--yes"]),
            false,
            false,
            never,
        )
        .unwrap();
        assert!(
            restored.contains("backup of the previous collection"),
            "{restored}"
        );
        actor.close().unwrap();
        drop(dir);
    }

    #[test]
    fn colpkg_export_through_the_cli() {
        let (_dir, actor) = fixture();
        let out = tempfile::tempdir().unwrap();
        let file = out.path().join("all.colpkg");
        let text = export(&actor, &export_args(&[s(&file)]), false).unwrap();
        assert!(text.contains("whole collection"), "{text}");
        assert!(file.is_file());
        actor.close().unwrap();
    }
}
