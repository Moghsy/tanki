//! CLI stream (Phase 3): `tanki add`, `tanki edit`, `tanki search` and
//! `tanki delete` (PLAN 8.4, 8.5, 8.7, 9).
use std::io::{IsTerminal, Read, Write};
use tanki_core::{
    notefile, BrowseCommand, BrowseMode, BrowserRow, CollectionActor, ColumnInfo, Command,
    DeckName, EditResponse, FieldsCheck, NoteCommand, NoteData, NotetypeInfo, Response,
    SearchOrder,
};
use tanki_tui::editor_flow::{self, Applied, FlowOptions, Outcome};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Add a note from flags, a note file, or stdin.
#[derive(clap::Args)]
pub struct AddArgs {
    /// Deck name [default: the deck Anki would pick].
    #[arg(long)]
    deck: Option<String>,
    /// Note type name [default: the one Anki would pick].
    #[arg(long = "type")]
    notetype: Option<String>,
    /// A field value as NAME=VALUE (stored HTML, verbatim); repeatable.
    #[arg(long = "field", value_name = "NAME=VALUE")]
    fields: Vec<String>,
    /// A tag (or several, space separated); repeatable.
    #[arg(long = "tag")]
    tags: Vec<String>,
    /// Add the notes of a note file (`-` reads stdin).
    #[arg(long, value_name = "PATH", conflicts_with_all = ["fields", "tags", "notetype", "edit"])]
    file: Option<String>,
    /// Open $EDITOR on a template of the note type.
    #[arg(long, conflicts_with_all = ["fields"])]
    edit: bool,
    /// Add a Cloze note even without a cloze deletion.
    #[arg(long)]
    force: bool,
}
/// Edit matching notes in $EDITOR.
#[derive(clap::Args)]
pub struct EditArgs {
    /// Note ids, or a search query (Anki syntax; notes mode).
    #[arg(required_unless_present = "file")]
    targets: Vec<String>,
    /// Apply an edited note file instead of opening an editor (`-` reads
    /// stdin). Without targets, the notes named in the file are used.
    #[arg(long, value_name = "PATH")]
    file: Option<String>,
}
/// Search cards or notes.
#[derive(clap::Args)]
pub struct SearchArgs {
    /// Anki search text.
    #[arg(required = true)]
    query: Vec<String>,
    /// Search notes instead of cards.
    #[arg(long)]
    notes: bool,
    #[arg(long, value_enum, default_value = "table")]
    format: Format,
    /// Sort by a browser column (key or label).
    #[arg(long, value_name = "COLUMN")]
    sort: Option<String>,
    #[arg(long)]
    reverse: bool,
    /// Columns to show (keys or labels), comma separated.
    #[arg(long, value_delimiter = ',')]
    columns: Vec<String>,
    /// Show at most this many rows.
    #[arg(long)]
    limit: Option<usize>,
}
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Format {
    Table,
    Json,
    Ids,
}
/// Delete matching notes.
#[derive(clap::Args)]
pub struct DeleteArgs {
    /// Note ids, or a search query (Anki syntax; notes mode).
    #[arg(required = true)]
    targets: Vec<String>,
    /// Do not ask for confirmation.
    #[arg(long, short = 'y')]
    yes: bool,
}

/// Errors with their own exit code (PLAN 9): 26 = a note changed since it
/// was read (edit conflict).
#[derive(Debug)]
pub struct NoteCliError {
    pub message: String,
    pub code: u8,
}
impl NoteCliError {
    pub fn exit_code(&self) -> u8 {
        self.code
    }
}
impl std::fmt::Display for NoteCliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for NoteCliError {}

fn fail<T>(message: impl Into<String>) -> Result<T> {
    Err(message.into().into())
}
fn unexpected<T>() -> Result<T> {
    fail("Unexpected response from the collection")
}

fn notes(actor: &CollectionActor, command: NoteCommand) -> Result<EditResponse> {
    match actor.execute(Command::Notes(command))? {
        Response::Edit(response) => Ok(response),
        _ => unexpected(),
    }
}
fn browse(actor: &CollectionActor, command: BrowseCommand) -> Result<EditResponse> {
    match actor.execute(Command::Browse(command))? {
        Response::Edit(response) => Ok(response),
        _ => unexpected(),
    }
}
fn notetypes(actor: &CollectionActor) -> Result<Vec<NotetypeInfo>> {
    match notes(actor, NoteCommand::Notetypes)? {
        EditResponse::Notetypes(list) => Ok(list),
        _ => unexpected(),
    }
}
fn decks(actor: &CollectionActor) -> Result<Vec<DeckName>> {
    match notes(
        actor,
        NoteCommand::DeckNames {
            include_filtered: true,
        },
    )? {
        EditResponse::DeckNames(list) => Ok(list),
        _ => unexpected(),
    }
}
fn get_notes(actor: &CollectionActor, ids: &[i64]) -> Result<Vec<NoteData>> {
    match notes(actor, NoteCommand::GetNotes { ids: ids.to_vec() })? {
        EditResponse::Notes(list) => Ok(list),
        _ => unexpected(),
    }
}
fn print_json(value: &serde_json::Value) -> Result<()> {
    writeln!(
        std::io::stdout().lock(),
        "{}",
        serde_json::to_string_pretty(value)?
    )?;
    Ok(())
}
fn read_input(path: &str) -> Result<String> {
    let mut text = String::new();
    if path == "-" {
        std::io::stdin().read_to_string(&mut text)?;
    } else {
        text = std::fs::read_to_string(path).map_err(|e| format!("Cannot read {path}: {e}"))?;
    }
    Ok(text)
}

/// Note ids from ids or a search query (notes mode). A query that matches
/// nothing is refused; so is a deleted id.
fn resolve_targets(actor: &CollectionActor, targets: &[String]) -> Result<Vec<i64>> {
    let all_ids = targets
        .iter()
        .flat_map(|t| t.split([',', ' ']))
        .filter(|t| !t.is_empty())
        .map(|t| t.parse::<i64>().ok().filter(|id| *id > 0))
        .collect::<Option<Vec<_>>>();
    let mut ids = match all_ids {
        Some(ids) if !ids.is_empty() => {
            get_notes(actor, &ids)?;
            ids
        }
        _ => match browse(
            actor,
            BrowseCommand::Search {
                query: targets.join(" "),
                mode: BrowseMode::Notes,
                order: SearchOrder::Default,
            },
        )? {
            EditResponse::Ids(ids) => ids,
            _ => return unexpected(),
        },
    };
    if ids.is_empty() {
        return fail(format!("No notes match {:?}", targets.join(" ")));
    }
    let mut seen = std::collections::HashSet::new();
    ids.retain(|id| seen.insert(*id));
    Ok(ids)
}

// ---- add ---------------------------------------------------------------------

fn find_deck(decks: &[DeckName], name: &str) -> Result<i64> {
    let deck = decks
        .iter()
        .find(|d| d.name == name)
        .or_else(|| decks.iter().find(|d| d.name.eq_ignore_ascii_case(name)))
        .ok_or_else(|| format!("No deck named {name:?}"))?;
    if deck.filtered {
        return fail(format!(
            "{name:?} is a filtered deck; notes cannot be added to it"
        ));
    }
    Ok(deck.id)
}
fn find_notetype<'a>(list: &'a [NotetypeInfo], name: &str) -> Result<&'a NotetypeInfo> {
    list.iter()
        .find(|n| n.name == name)
        .or_else(|| list.iter().find(|n| n.name.eq_ignore_ascii_case(name)))
        .ok_or_else(|| {
            let names: Vec<&str> = list.iter().map(|n| n.name.as_str()).collect();
            format!("No note type named {name:?} (have: {})", names.join(", ")).into()
        })
}
fn split_tags(tags: &[String]) -> Vec<String> {
    tags.iter()
        .flat_map(|t| t.split_whitespace())
        .map(str::to_owned)
        .collect()
}

pub fn run_add(actor: &CollectionActor, args: &AddArgs, json: bool) -> Result<()> {
    let deck_list = decks(actor)?;
    let (default_deck, default_type) =
        match notes(actor, NoteCommand::DefaultsForAdding { home_deck: 0 })? {
            EditResponse::Defaults {
                deck_id,
                notetype_id,
            } => (deck_id, notetype_id),
            _ => return unexpected(),
        };
    let deck_id = match &args.deck {
        Some(name) => find_deck(&deck_list, name)?,
        None => default_deck,
    };
    if let Some(path) = &args.file {
        let text = read_input(path)?;
        let applied = editor_flow::apply_text(actor, &text, &[], Some(deck_id))?;
        return report_applied(applied, json, true);
    }
    let types = notetypes(actor)?;
    let notetype = match &args.notetype {
        Some(name) => find_notetype(&types, name)?,
        None => types
            .iter()
            .find(|n| n.id == default_type)
            .or(types.first())
            .ok_or("The collection has no note types")?,
    };
    if notetype.image_occlusion {
        return fail("Image occlusion notes cannot be added yet");
    }
    let tags = split_tags(&args.tags);
    if args.edit {
        let deck_name = deck_list
            .iter()
            .find(|d| d.id == deck_id)
            .map(|d| d.name.clone());
        let template = notefile::new_note(notetype, deck_name.as_deref(), &tags);
        let command = editor_command();
        let outcome = editor_flow::add_notes(
            actor,
            &[template],
            &FlowOptions {
                default_deck: Some(deck_id),
                ..FlowOptions::default()
            },
            &mut |path| editor_flow::spawn_editor(&command, path),
        )
        .map_err(|e| e.to_string())?;
        return report_outcome(outcome, json, true);
    }
    // Fields from flags.
    let EditResponse::Note(mut note) = notes(
        actor,
        NoteCommand::NewNote {
            notetype_id: notetype.id,
        },
    )?
    else {
        return unexpected();
    };
    if args.fields.is_empty() {
        return fail("Give the field values with --field NAME=VALUE (or use --file or --edit)");
    }
    let mut set = vec![false; note.fields.len()];
    for item in &args.fields {
        let (name, value) = item
            .split_once('=')
            .ok_or_else(|| format!("--field wants NAME=VALUE, got {item:?}"))?;
        let ord = notetype
            .fields
            .iter()
            .position(|f| f.name == name)
            .or_else(|| {
                notetype
                    .fields
                    .iter()
                    .position(|f| f.name.eq_ignore_ascii_case(name))
            })
            .ok_or_else(|| {
                let names: Vec<&str> = notetype.fields.iter().map(|f| f.name.as_str()).collect();
                format!(
                    "Note type {:?} has no field {name:?} (fields: {})",
                    notetype.name,
                    names.join(", ")
                )
            })?;
        if std::mem::replace(&mut set[ord], true) {
            return fail(format!("Field {name:?} given more than once"));
        }
        note.fields[ord] = value.to_owned();
    }
    note.tags = tags;
    let EditResponse::FieldsCheck(check) =
        notes(actor, NoteCommand::CheckFields { note: note.clone() })?
    else {
        return unexpected();
    };
    match check {
        FieldsCheck::Normal => {}
        FieldsCheck::Duplicate => eprintln!("tanki: warning: a note with this first field exists"),
        FieldsCheck::MissingCloze if args.force => {}
        FieldsCheck::Empty => return fail("The first field is empty"),
        FieldsCheck::MissingCloze => return fail(
            "The Cloze note has no cloze deletion like {{c1::text}} (use --force to add it anyway)",
        ),
        FieldsCheck::NotetypeNotCloze => {
            return fail("Cloze deletions only work in a Cloze note type")
        }
        FieldsCheck::FieldNotCloze => return fail("Cloze deletions only work in the cloze field"),
    }
    let EditResponse::NoteAdded { id, .. } = notes(actor, NoteCommand::AddNote { note, deck_id })?
    else {
        return unexpected();
    };
    if json {
        print_json(&serde_json::json!({ "id": id }))?;
    } else {
        println!("{id}");
    }
    Ok(())
}

// ---- edit --------------------------------------------------------------------

fn editor_command() -> Vec<String> {
    tanki_core::config::default_path()
        .and_then(|path| tanki_core::config::Config::load(&path).ok())
        .unwrap_or_default()
        .editor
        .resolve()
}

fn id_list(ids: &[i64]) -> String {
    ids.iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn conflict_error(
    conflicts: &[i64],
    missing: &[i64],
    kept: Option<&std::path::Path>,
) -> Box<dyn std::error::Error> {
    let mut message = String::new();
    if !conflicts.is_empty() {
        message += &format!(
            "Changed since they were read: note {}. ",
            id_list(conflicts)
        );
    }
    if !missing.is_empty() {
        message += &format!("Deleted since they were read: note {}. ", id_list(missing));
    }
    message += "Nothing was written.";
    if let Some(path) = kept {
        message += &format!(" Your edited file is kept at {}", path.display());
    }
    Box::new(NoteCliError { message, code: 26 })
}

fn print_done(updated: &[i64], unchanged: &[i64], added: &[i64], json: bool) -> Result<()> {
    if json {
        return print_json(&serde_json::json!({
            "updated": updated, "unchanged": unchanged, "added": added
        }));
    }
    if updated.is_empty() && added.is_empty() {
        println!("No changes");
        return Ok(());
    }
    if !updated.is_empty() {
        println!("Updated {} note(s): {}", updated.len(), id_list(updated));
    }
    if !unchanged.is_empty() {
        println!(
            "Unchanged {} note(s): {}",
            unchanged.len(),
            id_list(unchanged)
        );
    }
    if !added.is_empty() {
        println!("Added {} note(s): {}", added.len(), id_list(added));
    }
    Ok(())
}

/// `ids_only`: `tanki add` output (the new ids, one per line).
fn report_applied(applied: Applied, json: bool, ids_only: bool) -> Result<()> {
    match applied {
        Applied::Problems(problems) => {
            let lines: Vec<String> = problems
                .iter()
                .map(|p| format!("line {}: {}", p.line, p.message))
                .collect();
            fail(format!("Nothing was written:\n{}", lines.join("\n")))
        }
        Applied::Conflicts { conflicts, missing } => {
            Err(conflict_error(&conflicts, &missing, None))
        }
        Applied::Done {
            updated,
            unchanged,
            added,
            skipped,
            ..
        } => {
            for line in skipped {
                eprintln!("tanki: warning: line {line}: new note with all fields empty skipped");
            }
            if ids_only {
                if added.is_empty() {
                    return fail("No notes were added");
                }
                if json {
                    let list: Vec<_> = added
                        .iter()
                        .map(|id| serde_json::json!({"id": id}))
                        .collect();
                    return print_json(&serde_json::Value::Array(list));
                }
                for id in added {
                    println!("{id}");
                }
                return Ok(());
            }
            print_done(&updated, &unchanged, &added, json)
        }
    }
}

fn report_outcome(outcome: Outcome, json: bool, ids_only: bool) -> Result<()> {
    match outcome {
        Outcome::Applied {
            updated,
            unchanged,
            added,
            ..
        } => report_applied(
            Applied::Done {
                updated,
                unchanged,
                added,
                skipped: Vec::new(),
                changes: tanki_core::OpChanges::default(),
            },
            json,
            ids_only,
        ),
        Outcome::Unchanged => {
            if ids_only {
                return fail("No notes were added");
            }
            print_done(&[], &[], &[], json)
        }
        Outcome::Aborted { kept } => {
            let mut message = "Aborted; nothing was written".to_owned();
            if let Some(path) = kept {
                message += &format!(" (your edits are kept at {})", path.display());
            }
            fail(message)
        }
        Outcome::Conflicts {
            conflicts,
            missing,
            kept,
        } => Err(conflict_error(&conflicts, &missing, Some(&kept))),
    }
}

pub fn run_edit(actor: &CollectionActor, args: &EditArgs, json: bool) -> Result<()> {
    if let Some(path) = &args.file {
        let text = read_input(path)?;
        let ids = if args.targets.is_empty() {
            let blocks = notefile::parse(&text).map_err(|e| e.to_string())?;
            let ids: Vec<i64> = blocks.iter().filter_map(|b| b.id).collect();
            if ids.is_empty() {
                return fail("The file names no existing notes (no id: lines)");
            }
            get_notes(actor, &ids)?;
            ids
        } else {
            resolve_targets(actor, &args.targets)?
        };
        let originals = get_notes(actor, &ids)?;
        let applied = editor_flow::apply_text(actor, &text, &originals, None)?;
        return report_applied(applied, json, false);
    }
    let ids = resolve_targets(actor, &args.targets)?;
    let command = editor_command();
    let outcome = editor_flow::edit_notes(actor, &ids, &FlowOptions::default(), &mut |path| {
        editor_flow::spawn_editor(&command, path)
    })
    .map_err(|e| e.to_string())?;
    report_outcome(outcome, json, false)
}

// ---- search ------------------------------------------------------------------

fn char_width(c: char) -> usize {
    let u = c as u32;
    match u {
        0..=0x1f | 0x7f..=0x9f => 0,
        0x300..=0x36f | 0x200b..=0x200f | 0xfe00..=0xfe0f | 0x20d0..=0x20ff => 0,
        0x1100..=0x115f
        | 0x2e80..=0x303e
        | 0x3041..=0x33ff
        | 0x3400..=0x4dbf
        | 0x4e00..=0x9fff
        | 0xa000..=0xa4cf
        | 0xac00..=0xd7a3
        | 0xf900..=0xfaff
        | 0xfe30..=0xfe6f
        | 0xff00..=0xff60
        | 0xffe0..=0xffe6
        | 0x1f300..=0x1f64f
        | 0x1f900..=0x1f9ff
        | 0x20000..=0x3fffd => 2,
        _ => 1,
    }
}
fn text_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}
fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn resolve_column<'a>(
    columns: &'a [ColumnInfo],
    name: &str,
    mode: BrowseMode,
) -> Result<&'a ColumnInfo> {
    let label = |c: &'a ColumnInfo| match mode {
        BrowseMode::Cards => c.cards_label.as_str(),
        BrowseMode::Notes => c.notes_label.as_str(),
    };
    columns
        .iter()
        .find(|c| c.key.eq_ignore_ascii_case(name) || label(c).eq_ignore_ascii_case(name))
        .ok_or_else(|| {
            let keys: Vec<&str> = columns.iter().map(|c| c.key.as_str()).collect();
            format!("Unknown column {name:?} (columns: {})", keys.join(", ")).into()
        })
}

pub fn run_search(actor: &CollectionActor, args: &SearchArgs, json: bool) -> Result<()> {
    let mode = if args.notes {
        BrowseMode::Notes
    } else {
        BrowseMode::Cards
    };
    let format = if json { Format::Json } else { args.format };
    let EditResponse::Columns(columns) = browse(actor, BrowseCommand::Columns)? else {
        return unexpected();
    };
    let order = match &args.sort {
        Some(name) => {
            let column = resolve_column(&columns, name, mode)?;
            let sortable = match mode {
                BrowseMode::Cards => column.sort_cards.is_some(),
                BrowseMode::Notes => column.sort_notes.is_some(),
            };
            if !sortable {
                return fail(format!("Cannot sort by column {name:?}"));
            }
            SearchOrder::Column {
                key: column.key.clone(),
                reverse: args.reverse,
            }
        }
        None if args.reverse => {
            let EditResponse::Settings(s) = browse(actor, BrowseCommand::Settings)? else {
                return unexpected();
            };
            match mode {
                BrowseMode::Cards => SearchOrder::Column {
                    key: s.card_sort,
                    reverse: !s.card_sort_reverse,
                },
                BrowseMode::Notes => SearchOrder::Column {
                    key: s.note_sort,
                    reverse: !s.note_sort_reverse,
                },
            }
        }
        None => SearchOrder::Default,
    };
    let EditResponse::Ids(mut ids) = browse(
        actor,
        BrowseCommand::Search {
            query: args.query.join(" "),
            mode,
            order,
        },
    )?
    else {
        return unexpected();
    };
    if let Some(limit) = args.limit {
        ids.truncate(limit);
    }
    if format == Format::Ids {
        let mut out = std::io::stdout().lock();
        for id in ids {
            writeln!(out, "{id}")?;
        }
        return Ok(());
    }
    let keys: Vec<String> = if args.columns.is_empty() {
        let EditResponse::Settings(s) = browse(actor, BrowseCommand::Settings)? else {
            return unexpected();
        };
        match mode {
            BrowseMode::Cards => s.card_columns,
            BrowseMode::Notes => s.note_columns,
        }
    } else {
        args.columns
            .iter()
            .map(|n| resolve_column(&columns, n, mode).map(|c| c.key.clone()))
            .collect::<Result<_>>()?
    };
    let rows: Vec<BrowserRow> = if ids.is_empty() {
        Vec::new()
    } else {
        match browse(
            actor,
            BrowseCommand::Rows {
                mode,
                columns: keys.clone(),
                ids,
            },
        )? {
            EditResponse::Rows(rows) => rows,
            _ => return unexpected(),
        }
    };
    let cell = |row: &BrowserRow, i: usize| {
        row.cells
            .get(i)
            .map_or_else(String::new, |c| c.text.clone())
    };
    if format == Format::Json {
        let list: Vec<serde_json::Value> = rows
            .iter()
            .map(|row| {
                let mut object = serde_json::Map::new();
                object.insert("id".into(), row.id.into());
                for (i, key) in keys.iter().enumerate() {
                    object.insert(key.clone(), cell(row, i).into());
                }
                serde_json::Value::Object(object)
            })
            .collect();
        return print_json(&serde_json::Value::Array(list));
    }
    let labels: Vec<String> = keys
        .iter()
        .map(|k| {
            columns.iter().find(|c| &c.key == k).map_or_else(
                || k.clone(),
                |c| match mode {
                    BrowseMode::Cards => c.cards_label.clone(),
                    BrowseMode::Notes => c.notes_label.clone(),
                },
            )
        })
        .collect();
    let mut table: Vec<Vec<String>> = vec![std::iter::once("id".to_owned())
        .chain(labels.iter().map(|l| one_line(l)))
        .collect()];
    for row in &rows {
        table.push(
            std::iter::once(row.id.to_string())
                .chain((0..keys.len()).map(|i| one_line(&cell(row, i))))
                .collect(),
        );
    }
    let widths: Vec<usize> = (0..=keys.len())
        .map(|c| table.iter().map(|r| text_width(&r[c])).max().unwrap_or(0))
        .collect();
    let mut out = std::io::stdout().lock();
    for row in &table {
        let mut line = String::new();
        for (i, text) in row.iter().enumerate() {
            line.push_str(text);
            if i + 1 < row.len() {
                line.push_str(&" ".repeat(widths[i] - text_width(text) + 2));
            }
        }
        writeln!(out, "{}", line.trim_end())?;
    }
    Ok(())
}

// ---- delete ------------------------------------------------------------------

pub fn run_delete(actor: &CollectionActor, args: &DeleteArgs, json: bool) -> Result<()> {
    let ids = resolve_targets(actor, &args.targets)?;
    if !args.yes {
        if !std::io::stdin().is_terminal() || ids.len() > 1 {
            return fail(format!(
                "Refusing to delete {} note(s) without --yes",
                ids.len()
            ));
        }
        eprint!("Delete {} note(s)? [y/N] ", ids.len());
        std::io::stderr().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            return fail("Cancelled");
        }
    }
    match actor.execute(Command::DeleteNotes {
        note_ids: ids.clone(),
    }) {
        Ok(Response::Changes(_)) => {}
        Ok(_) => return unexpected(),
        Err(e) => return Err(e.into()),
    }
    if json {
        print_json(&serde_json::json!({"deleted": ids.len(), "ids": ids}))
    } else {
        println!("Deleted {} note(s)", ids.len());
        Ok(())
    }
}
