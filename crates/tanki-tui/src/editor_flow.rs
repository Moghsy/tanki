//! Editors stream (Phase 3): the `$EDITOR` round trip shared by the TUI and
//! `tanki edit`: write notes to a note file, run the editor, parse, reopen on
//! errors, then write back only the fields that changed.
//!
//! - The file (`tanki_core::notefile` format) lives in a private temporary
//!   folder (0700; the file is 0600) that is removed afterwards, unless it
//!   holds text the user typed that was not applied: then its path is in the
//!   outcome (`kept`) so nothing is lost.
//! - Running the editor is injected (`run_editor`), so tests need no terminal
//!   and the TUI can suspend its screen around it; [`spawn_editor`] runs a
//!   resolved command line (`Config::editor.resolve()`).
//! - Saving the file unchanged changes nothing ([`Outcome::Unchanged`]). An
//!   editor that exits with an error, or a file emptied of notes, aborts.
//! - Parse errors, refused blocks (`notefile::plan`) and failed field checks
//!   of new notes reopen the file with the problems as `#` comments at the
//!   top, like `git commit`. Saving that file unchanged aborts and keeps it.
//! - Updates go through one `UpdateNotes` (all or nothing, one undo step);
//!   new blocks through one `AddNotes` afterwards. When a note changed in the
//!   meantime nothing is written and the outcome lists the conflicts.
use crate::backend::Backend;
use std::{
    fs, io,
    path::{Path, PathBuf},
};
use tanki_core::{
    notefile::{self, FileNote, PlanContext},
    Command, CoreError, EditResponse, FieldsCheck, NoteCommand, NoteData, OpChanges, Response,
};

/// How the editor process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorExit {
    Success,
    /// Non-zero exit (`:cq` in vim): the user aborted.
    Failure,
}

impl From<std::process::ExitStatus> for EditorExit {
    fn from(status: std::process::ExitStatus) -> Self {
        if status.success() {
            Self::Success
        } else {
            Self::Failure
        }
    }
}

/// Runs `command` (program and arguments, as `EditorSettings::resolve`
/// returns them) with `path` appended, on the inherited terminal, and waits.
pub fn spawn_editor(command: &[String], path: &Path) -> io::Result<EditorExit> {
    let (program, args) = command.split_first().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "No editor command configured")
    })?;
    Ok(std::process::Command::new(program)
        .args(args)
        .arg(path)
        .status()?
        .into())
}

#[derive(Debug, Clone, Default)]
pub struct FlowOptions {
    /// Where the private temporary folder is made (default: the system's).
    pub temp_dir: Option<PathBuf>,
    /// Deck for new blocks without `deck:`; default: `DefaultsForAdding`.
    pub default_deck: Option<i64>,
    /// Reopen the file at most this often after problems (0 = 20).
    pub max_rounds: usize,
}

/// The result of an editor round trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Changes were written: `updated` notes (one undo step), `added` notes
    /// (another one). `unchanged` notes were in the file without changes.
    Applied {
        updated: Vec<i64>,
        unchanged: Vec<i64>,
        added: Vec<i64>,
        changes: OpChanges,
    },
    /// Saved without changing any note.
    Unchanged,
    /// The editor failed, the file was emptied, or a reopened file was saved
    /// unchanged. `kept` is the file when it holds unapplied text.
    Aborted { kept: Option<PathBuf> },
    /// These notes changed (`conflicts`) or were deleted (`missing`) since
    /// the file was written; nothing was written. The edited file is kept.
    Conflicts {
        conflicts: Vec<i64>,
        missing: Vec<i64>,
        kept: PathBuf,
    },
}

/// A failure of the flow itself (editor not found, collection error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowError {
    pub message: String,
    /// The edited file, kept when it holds text the user typed.
    pub kept: Option<PathBuf>,
}

impl std::fmt::Display for FlowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)?;
        if let Some(path) = &self.kept {
            write!(f, " (your edits are kept in {})", path.display())?;
        }
        Ok(())
    }
}

impl std::error::Error for FlowError {}

/// Something in the file that has to be fixed, at a 1-based line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub line: usize,
    pub message: String,
}

/// The result of [`apply_text`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// Nothing was written.
    Problems(Vec<Problem>),
    /// Nothing was written (`UpdateNotes` is all or nothing).
    Conflicts {
        conflicts: Vec<i64>,
        missing: Vec<i64>,
    },
    /// Everything was written; all lists empty when nothing changed.
    Done {
        updated: Vec<i64>,
        unchanged: Vec<i64>,
        added: Vec<i64>,
        /// New blocks with all fields empty, by 1-based line.
        skipped: Vec<usize>,
        changes: OpChanges,
    },
}

type Result<T> = std::result::Result<T, CoreError>;

fn notes(backend: &dyn Backend, command: NoteCommand) -> Result<EditResponse> {
    match backend.execute(Command::Notes(command))? {
        Response::Edit(response) => Ok(response),
        _ => Err(CoreError::Other(
            "Unexpected response from the collection".into(),
        )),
    }
}

fn unexpected<T>() -> Result<T> {
    Err(CoreError::Other(
        "Unexpected response from the collection".into(),
    ))
}

fn merge(into: &mut OpChanges, other: &OpChanges) {
    into.card |= other.card;
    into.note |= other.note;
    into.deck |= other.deck;
    into.tag |= other.tag;
    into.notetype |= other.notetype;
    into.config |= other.config;
    into.deck_config |= other.deck_config;
    into.mtime |= other.mtime;
    into.browser_table |= other.browser_table;
    into.browser_sidebar |= other.browser_sidebar;
    into.note_text |= other.note_text;
    into.study_queues |= other.study_queues;
}

fn check_message(check: FieldsCheck) -> Option<&'static str> {
    match check {
        FieldsCheck::Normal | FieldsCheck::Duplicate => None,
        FieldsCheck::Empty => Some("the first field is empty"),
        FieldsCheck::MissingCloze => {
            Some("this Cloze note has no cloze deletion; add one like {{c1::text}}")
        }
        FieldsCheck::NotetypeNotCloze => Some("cloze deletions only work in a Cloze note type"),
        FieldsCheck::FieldNotCloze => Some("cloze deletions only work in the cloze field"),
    }
}

/// Applies note-file text without an editor (`tanki add --file`, and each
/// round of the editor flow). `originals` are the notes as they were when
/// the file was written (empty when only adding). Nothing is written while
/// there are problems or conflicts.
pub fn apply_text(
    backend: &dyn Backend,
    text: &str,
    originals: &[NoteData],
    default_deck: Option<i64>,
) -> Result<Applied> {
    let located = match notefile::parse_located(text) {
        Ok(located) => located,
        Err(e) => {
            return Ok(Applied::Problems(vec![Problem {
                line: e.line,
                message: e.message,
            }]))
        }
    };
    let lines: Vec<usize> = located.iter().map(|(line, _)| *line).collect();
    let edited: Vec<FileNote> = located.into_iter().map(|(_, note)| note).collect();
    let EditResponse::Notetypes(notetypes) = notes(backend, NoteCommand::Notetypes)? else {
        return unexpected();
    };
    let adding = edited.iter().any(|n| n.id.is_none());
    let decks = if adding {
        match notes(
            backend,
            NoteCommand::DeckNames {
                include_filtered: true,
            },
        )? {
            EditResponse::DeckNames(decks) => decks,
            _ => return unexpected(),
        }
    } else {
        Vec::new()
    };
    let default_deck = match default_deck {
        Some(id) => Some(id),
        None if edited.iter().any(|n| n.id.is_none() && n.deck.is_none()) => {
            match notes(backend, NoteCommand::DefaultsForAdding { home_deck: 0 })? {
                EditResponse::Defaults { deck_id, .. } => Some(deck_id),
                _ => return unexpected(),
            }
        }
        None => None,
    };
    let plan = notefile::plan(
        &PlanContext {
            originals,
            notetypes: &notetypes,
            decks: &decks,
            default_deck,
        },
        &edited,
    );
    let mut problems: Vec<Problem> = plan
        .refusals
        .iter()
        .map(|r| Problem {
            line: lines[r.index],
            message: r.message.clone(),
        })
        .collect();
    if !problems.is_empty() {
        return Ok(Applied::Problems(problems));
    }
    let mut new_notes = Vec::new();
    for add in &plan.adds {
        let EditResponse::Note(mut note) = notes(
            backend,
            NoteCommand::NewNote {
                notetype_id: add.notetype_id,
            },
        )?
        else {
            return unexpected();
        };
        note.fields = add.fields.clone();
        note.tags = add.tags.clone();
        let EditResponse::FieldsCheck(check) =
            notes(backend, NoteCommand::CheckFields { note: note.clone() })?
        else {
            return unexpected();
        };
        if let Some(message) = check_message(check) {
            problems.push(Problem {
                line: lines[add.index],
                message: format!("new note: {message}"),
            });
        }
        new_notes.push((note, add.deck_id));
    }
    if !problems.is_empty() {
        return Ok(Applied::Problems(problems));
    }
    let mut changes = OpChanges::default();
    let mut updated = Vec::new();
    let mut unchanged = plan.unchanged.clone();
    if !plan.updates.is_empty() {
        let EditResponse::Updated(report) = notes(
            backend,
            NoteCommand::UpdateNotes {
                updates: plan.updates.clone(),
            },
        )?
        else {
            return unexpected();
        };
        if !report.conflicts.is_empty() || !report.missing.is_empty() {
            return Ok(Applied::Conflicts {
                conflicts: report.conflicts,
                missing: report.missing,
            });
        }
        merge(&mut changes, &report.changes);
        updated = report.updated;
        unchanged.extend(report.unchanged);
    }
    let mut added = Vec::new();
    if !new_notes.is_empty() {
        let EditResponse::NotesAdded { ids, changes: c } =
            notes(backend, NoteCommand::AddNotes { notes: new_notes })?
        else {
            return unexpected();
        };
        merge(&mut changes, &c);
        added = ids;
    }
    Ok(Applied::Done {
        updated,
        unchanged,
        added,
        skipped: plan.skipped.iter().map(|&i| lines[i]).collect(),
        changes,
    })
}

/// Edits existing notes in the editor. Blocks the user adds (without
/// `id:`) are added as new notes.
pub fn edit_notes(
    backend: &dyn Backend,
    note_ids: &[i64],
    options: &FlowOptions,
    run_editor: &mut dyn FnMut(&Path) -> io::Result<EditorExit>,
) -> std::result::Result<Outcome, FlowError> {
    let fail = |e: CoreError| FlowError {
        message: e.to_string(),
        kept: None,
    };
    let EditResponse::Notes(originals) = notes(
        backend,
        NoteCommand::GetNotes {
            ids: note_ids.to_vec(),
        },
    )
    .map_err(fail)?
    else {
        return Err(fail(CoreError::Other(
            "Unexpected response from the collection".into(),
        )));
    };
    let EditResponse::Notetypes(notetypes) =
        notes(backend, NoteCommand::Notetypes).map_err(fail)?
    else {
        return Err(fail(CoreError::Other(
            "Unexpected response from the collection".into(),
        )));
    };
    let mut blocks = Vec::new();
    for note in &originals {
        let notetype = notetypes
            .iter()
            .find(|nt| nt.id == note.notetype_id)
            .ok_or_else(|| {
                fail(CoreError::Other(format!(
                    "The note type of note {} was not found",
                    note.id
                )))
            })?;
        blocks.push(notefile::from_note(note, notetype));
    }
    run(backend, &originals, &blocks, options, run_editor)
}

/// Adds notes from template blocks (`notefile::new_note`), e.g. for an
/// editor-based `tanki add`. An untouched template adds nothing.
pub fn add_notes(
    backend: &dyn Backend,
    template: &[FileNote],
    options: &FlowOptions,
    run_editor: &mut dyn FnMut(&Path) -> io::Result<EditorExit>,
) -> std::result::Result<Outcome, FlowError> {
    run(backend, &[], template, options, run_editor)
}

/// A private temporary folder holding the note file.
struct Workspace {
    dir: PathBuf,
    file: PathBuf,
}

impl Workspace {
    fn create(base: Option<&Path>) -> io::Result<Self> {
        let base = base.map_or_else(std::env::temp_dir, Path::to_path_buf);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        for attempt in 0..100 {
            let dir = base.join(format!(
                "tanki-edit-{}-{stamp}-{attempt}",
                std::process::id()
            ));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&dir) {
                Ok(()) => {
                    let file = dir.join("notes.md");
                    return Ok(Self { dir, file });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Could not create a temporary folder",
        ))
    }

    fn write(&self, text: &str) -> io::Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        io::Write::write_all(&mut options.open(&self.file)?, text.as_bytes())
    }

    fn remove(&self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Replaces the leading comments of `text` with the problems and the
/// header; line numbers are moved to match the new file.
fn annotate(text: &str, problems: &[Problem]) -> String {
    let stripped = text
        .split('\n')
        .take_while(|l| l.trim().is_empty() || (l.starts_with('#') && l.trim_end() != "# Note"))
        .count();
    let rest: Vec<&str> = text.split('\n').skip(stripped).collect();
    let mut prefix = String::from("# tanki could not apply this file:\n");
    let count = problems.len() + 3 + notefile::HEADER.lines().count() + 1;
    for p in problems {
        let line = (p.line + count).saturating_sub(stripped);
        prefix.push_str(&format!(
            "#   line {line}: {}\n",
            p.message.replace(['\n', '\r'], " ")
        ));
    }
    prefix.push_str(
        "# Fix the problems and save again. To cancel, quit without saving or delete everything.\n#\n",
    );
    prefix.push_str(notefile::HEADER);
    prefix.push('\n');
    debug_assert_eq!(prefix.matches('\n').count(), count);
    prefix + &rest.join("\n")
}

fn run(
    backend: &dyn Backend,
    originals: &[NoteData],
    blocks: &[FileNote],
    options: &FlowOptions,
    run_editor: &mut dyn FnMut(&Path) -> io::Result<EditorExit>,
) -> std::result::Result<Outcome, FlowError> {
    let io_error = |what: &str, e: io::Error, kept: Option<PathBuf>| FlowError {
        message: format!("{what}: {e}"),
        kept,
    };
    let workspace = Workspace::create(options.temp_dir.as_deref())
        .map_err(|e| io_error("Could not create the note file", e, None))?;
    let initial = notefile::write(blocks);
    if let Err(e) = workspace.write(&initial) {
        workspace.remove();
        return Err(io_error("Could not write the note file", e, None));
    }
    let max_rounds = if options.max_rounds == 0 {
        20
    } else {
        options.max_rounds
    };
    let path = workspace.file.clone();
    let mut written = initial.clone();
    let mut round = 0;
    loop {
        // Keeps the file only when it differs from what tanki first wrote.
        let keep = |text: Option<&str>| -> Option<PathBuf> {
            match text {
                Some(text) if text == initial => {
                    workspace.remove();
                    None
                }
                None if written == initial => {
                    workspace.remove();
                    None
                }
                _ => Some(path.clone()),
            }
        };
        let exit = match run_editor(&path) {
            Ok(exit) => exit,
            Err(e) => {
                let kept = keep(fs::read_to_string(&path).ok().as_deref());
                return Err(io_error("Could not run the editor", e, kept));
            }
        };
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => return Err(io_error("Could not read the note file", e, keep(None))),
        };
        if exit == EditorExit::Failure {
            return Ok(Outcome::Aborted {
                kept: keep(Some(&text)),
            });
        }
        if text == written {
            return Ok(if round == 0 {
                workspace.remove();
                Outcome::Unchanged
            } else {
                Outcome::Aborted { kept: Some(path) }
            });
        }
        if notefile::parse(&text).is_ok_and(|notes| notes.is_empty()) {
            workspace.remove();
            return Ok(Outcome::Aborted { kept: None });
        }
        let applied =
            apply_text(backend, &text, originals, options.default_deck).map_err(|e| FlowError {
                message: e.to_string(),
                kept: Some(path.clone()),
            })?;
        match applied {
            Applied::Problems(problems) => {
                round += 1;
                if round >= max_rounds {
                    return Ok(Outcome::Aborted { kept: Some(path) });
                }
                written = annotate(&text, &problems);
                workspace.write(&written).map_err(|e| {
                    io_error("Could not write the note file", e, Some(path.clone()))
                })?;
            }
            Applied::Conflicts { conflicts, missing } => {
                return Ok(Outcome::Conflicts {
                    conflicts,
                    missing,
                    kept: path,
                })
            }
            Applied::Done {
                updated,
                unchanged,
                added,
                changes,
                ..
            } => {
                workspace.remove();
                return Ok(if updated.is_empty() && added.is_empty() {
                    Outcome::Unchanged
                } else {
                    Outcome::Applied {
                        updated,
                        unchanged,
                        added,
                        changes,
                    }
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;
    use tanki_core::{DeckName, FieldInfo, NoteUpdate, NotetypeInfo, NotetypeKind, UpdateReport};

    fn field(ord: u32, name: &str) -> FieldInfo {
        FieldInfo {
            ord,
            name: name.into(),
            ..Default::default()
        }
    }

    /// Answers `Command::Notes` like the actor would, from memory.
    struct Fake {
        notes: RefCell<BTreeMap<i64, NoteData>>,
        notetypes: Vec<NotetypeInfo>,
        log: RefCell<Vec<String>>,
        next_id: Cell<i64>,
    }

    impl Fake {
        fn new() -> Self {
            let notes = [
                NoteData {
                    id: 100,
                    guid: "a".into(),
                    notetype_id: 10,
                    mtime_secs: 50,
                    usn: -1,
                    tags: vec!["geo".into()],
                    fields: vec!["Paris".into(), "France".into()],
                },
                NoteData {
                    id: 200,
                    guid: "b".into(),
                    notetype_id: 10,
                    mtime_secs: 60,
                    usn: -1,
                    tags: vec![],
                    fields: vec!["Rome".into(), "Italy\n".into()],
                },
            ];
            Self {
                notes: RefCell::new(notes.into_iter().map(|n| (n.id, n)).collect()),
                notetypes: vec![
                    NotetypeInfo {
                        id: 10,
                        name: "Basic".into(),
                        fields: vec![field(0, "Front"), field(1, "Back")],
                        ..Default::default()
                    },
                    NotetypeInfo {
                        id: 20,
                        name: "Cloze".into(),
                        kind: NotetypeKind::Cloze,
                        fields: vec![field(0, "Text"), field(1, "Back Extra")],
                        cloze_fields: vec![0],
                        ..Default::default()
                    },
                ],
                log: RefCell::new(Vec::new()),
                next_id: Cell::new(1000),
            }
        }

        fn note(&self, id: i64) -> NoteData {
            self.notes.borrow()[&id].clone()
        }

        fn calls(&self) -> Vec<String> {
            self.log.borrow().clone()
        }

        fn update(&self, updates: Vec<NoteUpdate>) -> UpdateReport {
            let mut report = UpdateReport::default();
            let mut notes = self.notes.borrow_mut();
            for u in &updates {
                match notes.get(&u.id) {
                    None => report.missing.push(u.id),
                    Some(n)
                        if n.mtime_secs != u.expected_mtime
                            || u.expected_fields.as_ref().is_some_and(|f| *f != n.fields) =>
                    {
                        report.conflicts.push(u.id)
                    }
                    Some(_) => {}
                }
            }
            if !report.conflicts.is_empty() || !report.missing.is_empty() {
                return report;
            }
            for u in updates {
                let n = notes.get_mut(&u.id).unwrap();
                for (ord, value) in u.fields {
                    n.fields[ord as usize] = value;
                }
                if let Some(tags) = u.tags {
                    n.tags = tags;
                }
                n.mtime_secs += 1;
                report.updated.push(u.id);
            }
            report.changes.note = true;
            report
        }
    }

    impl Backend for Fake {
        fn execute(&self, command: Command) -> Result<Response> {
            let Command::Notes(command) = command else {
                panic!("unexpected {command:?}")
            };
            let name = format!("{command:?}");
            self.log
                .borrow_mut()
                .push(name.split([' ', '{', '(']).next().unwrap().to_owned());
            Ok(Response::Edit(match command {
                NoteCommand::GetNotes { ids } => EditResponse::Notes(
                    ids.iter()
                        .map(|id| {
                            self.notes
                                .borrow()
                                .get(id)
                                .cloned()
                                .ok_or_else(|| CoreError::Invalid(format!("no note {id}")))
                        })
                        .collect::<Result<_>>()?,
                ),
                NoteCommand::Notetypes => EditResponse::Notetypes(self.notetypes.clone()),
                NoteCommand::DeckNames { .. } => EditResponse::DeckNames(vec![
                    DeckName {
                        id: 1,
                        name: "Default".into(),
                        filtered: false,
                    },
                    DeckName {
                        id: 2,
                        name: "French".into(),
                        filtered: false,
                    },
                ]),
                NoteCommand::DefaultsForAdding { .. } => EditResponse::Defaults {
                    deck_id: 1,
                    notetype_id: 10,
                },
                NoteCommand::NewNote { notetype_id } => EditResponse::Note(NoteData {
                    guid: "new".into(),
                    notetype_id,
                    fields: vec![String::new(); 2],
                    ..Default::default()
                }),
                NoteCommand::CheckFields { note } => {
                    EditResponse::FieldsCheck(if note.fields[0].is_empty() {
                        FieldsCheck::Empty
                    } else {
                        FieldsCheck::Normal
                    })
                }
                NoteCommand::UpdateNotes { updates } => EditResponse::Updated(self.update(updates)),
                NoteCommand::AddNotes { notes } => {
                    let mut ids = Vec::new();
                    for (mut note, deck) in notes {
                        assert_eq!(note.guid, "new");
                        let id = self.next_id.get();
                        self.next_id.set(id + 1);
                        note.id = id;
                        note.usn = deck as i32; // remembered for the test
                        self.notes.borrow_mut().insert(id, note);
                        ids.push(id);
                    }
                    EditResponse::NotesAdded {
                        ids,
                        changes: OpChanges {
                            note: true,
                            card: true,
                            ..Default::default()
                        },
                    }
                }
                other => panic!("unexpected {other:?}"),
            }))
        }
    }

    /// A temporary base folder for the flow's private folder.
    struct Base(PathBuf);
    impl Base {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("tanki-flow-test-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn options(&self) -> FlowOptions {
            FlowOptions {
                temp_dir: Some(self.0.clone()),
                ..Default::default()
            }
        }
        fn is_empty(&self) -> bool {
            fs::read_dir(&self.0).unwrap().next().is_none()
        }
    }
    impl Drop for Base {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// An "editor" that applies one edit per round.
    fn script(
        mut steps: Vec<Box<dyn FnMut(String) -> Option<String>>>,
    ) -> impl FnMut(&Path) -> io::Result<EditorExit> {
        steps.reverse();
        move |path| {
            let mut step = steps.pop().expect("editor opened too often");
            let text = fs::read_to_string(path)?;
            match step(text) {
                Some(new) => {
                    fs::write(path, new)?;
                    Ok(EditorExit::Success)
                }
                None => Ok(EditorExit::Success),
            }
        }
    }

    fn step(
        f: impl FnMut(String) -> Option<String> + 'static,
    ) -> Box<dyn FnMut(String) -> Option<String>> {
        Box::new(f)
    }

    #[test]
    fn an_edited_field_is_written_alone() {
        let fake = Fake::new();
        let base = Base::new("edit");
        let mut editor = |path: &Path| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode(path), 0o600);
                assert_eq!(mode(path.parent().unwrap()), 0o700);
            }
            let text = fs::read_to_string(path)?;
            assert!(text.starts_with(notefile::HEADER));
            assert!(text.contains("## Back {quoted}\n\"Italy\\n\"\n"));
            fs::write(path, text.replace("France", "France <b>!</b>"))?;
            Ok(EditorExit::Success)
        };
        let outcome = edit_notes(&fake, &[100, 200], &base.options(), &mut editor).unwrap();
        assert_eq!(
            outcome,
            Outcome::Applied {
                updated: vec![100],
                unchanged: vec![200],
                added: vec![],
                changes: OpChanges {
                    note: true,
                    ..Default::default()
                },
            }
        );
        assert_eq!(fake.note(100).fields, ["Paris", "France <b>!</b>"]);
        assert_eq!(fake.note(200).fields, ["Rome", "Italy\n"]);
        assert!(base.is_empty(), "the temporary folder is removed");
    }

    #[test]
    fn unchanged_failed_and_emptied_files() {
        let fake = Fake::new();
        let base = Base::new("abort");
        let mut untouched = |_: &Path| Ok(EditorExit::Success);
        assert_eq!(
            edit_notes(&fake, &[100], &base.options(), &mut untouched),
            Ok(Outcome::Unchanged)
        );
        let mut failed = script(vec![step(|t| Some(t.replace("Paris", "Lyon")))]);
        let mut failing = |p: &Path| failed(p).map(|_| EditorExit::Failure);
        let outcome = edit_notes(&fake, &[100], &base.options(), &mut failing).unwrap();
        let Outcome::Aborted { kept: Some(kept) } = outcome else {
            panic!("{outcome:?}")
        };
        assert!(fs::read_to_string(&kept).unwrap().contains("Lyon"));
        fs::remove_dir_all(kept.parent().unwrap()).unwrap();
        let mut emptied = script(vec![step(|_| Some("# all gone\n".into()))]);
        assert_eq!(
            edit_notes(&fake, &[100], &base.options(), &mut emptied),
            Ok(Outcome::Aborted { kept: None })
        );
        let mut missing = |_: &Path| Err(io::Error::from(io::ErrorKind::NotFound));
        let error = edit_notes(&fake, &[100], &base.options(), &mut missing).unwrap_err();
        assert!(error.message.starts_with("Could not run the editor"));
        assert_eq!(error.kept, None);
        assert!(base.is_empty());
        assert_eq!(fake.note(100).fields, ["Paris", "France"]);
        assert!(!fake.calls().contains(&"UpdateNotes".to_owned()));
    }

    #[test]
    fn parse_errors_reopen_the_file_with_comments() {
        let fake = Fake::new();
        let base = Base::new("reopen");
        let mut editor = script(vec![
            step(|t| Some(t.replace("tags: geo", "tags: geo\ncolour: red"))),
            step(|t| {
                assert!(t.starts_with("# tanki could not apply this file:\n#   line "));
                let first = t.lines().nth(1).unwrap();
                let line: usize = first["#   line ".len()..]
                    .split(':')
                    .next()
                    .unwrap()
                    .parse()
                    .unwrap();
                assert!(first.contains("unknown key \"colour\""), "{first}");
                assert_eq!(t.lines().nth(line - 1), Some("colour: red"), "{t}");
                Some(t.replace("colour: red\n", "").replace("Paris", "Lyon"))
            }),
        ]);
        let outcome = edit_notes(&fake, &[100], &base.options(), &mut editor).unwrap();
        assert!(matches!(outcome, Outcome::Applied { ref updated, .. } if updated == &[100]));
        assert_eq!(fake.note(100).fields[0], "Lyon");
        assert!(base.is_empty());
    }

    #[test]
    fn refusals_reopen_and_saving_unchanged_keeps_the_file() {
        let fake = Fake::new();
        let base = Base::new("refuse");
        let mut editor = script(vec![
            step(|t| {
                Some(
                    t.replace("notetype: Basic", "notetype: Cloze")
                        .replace("Paris", "Lyon"),
                )
            }),
            step(|t| {
                assert!(t.contains("the note type cannot be changed"), "{t}");
                None
            }),
        ]);
        let outcome = edit_notes(&fake, &[100], &base.options(), &mut editor).unwrap();
        let Outcome::Aborted { kept: Some(kept) } = outcome else {
            panic!("{outcome:?}")
        };
        assert!(fs::read_to_string(&kept).unwrap().contains("Lyon"));
        assert_eq!(fake.note(100).fields[0], "Paris");
        // A scripted editor that never fixes anything gives up eventually.
        let stubborn = |p: &Path| {
            let t = fs::read_to_string(p)?;
            fs::write(p, format!("{t}\n# again"))?;
            Ok(EditorExit::Success)
        };
        let options = FlowOptions {
            max_rounds: 3,
            ..base.options()
        };
        let mut first = true;
        let mut breaking = |p: &Path| {
            if first {
                first = false;
                let t = fs::read_to_string(p)?;
                fs::write(p, t.replace("notetype: Basic", "notetype: Cloze"))?;
                return Ok(EditorExit::Success);
            }
            stubborn(p)
        };
        let outcome = edit_notes(&fake, &[100], &options, &mut breaking).unwrap();
        assert!(
            matches!(outcome, Outcome::Aborted { kept: Some(_) }),
            "{outcome:?}"
        );
    }

    #[test]
    fn conflicts_keep_the_edited_file() {
        let fake = Fake::new();
        let base = Base::new("conflict");
        let fake_ref = &fake;
        let mut editor = |p: &Path| {
            // Someone else edits note 100 while the editor is open.
            fake_ref
                .notes
                .borrow_mut()
                .get_mut(&100)
                .unwrap()
                .mtime_secs += 5;
            let t = fs::read_to_string(p)?;
            fs::write(p, t.replace("France", "Frankreich").replace("Rome", "Roma"))?;
            Ok(EditorExit::Success)
        };
        let outcome = edit_notes(&fake, &[100, 200], &base.options(), &mut editor).unwrap();
        let Outcome::Conflicts {
            conflicts,
            missing,
            kept,
        } = outcome
        else {
            panic!("{outcome:?}")
        };
        assert_eq!((conflicts, missing), (vec![100], vec![]));
        let text = fs::read_to_string(&kept).unwrap();
        assert!(text.contains("Frankreich") && text.contains("Roma"));
        assert_eq!(fake.note(200).fields[0], "Rome", "all or nothing");
    }

    #[test]
    fn adding_from_a_template() {
        let fake = Fake::new();
        let base = Base::new("add");
        let template = [
            notefile::new_note(&fake.notetypes[0], Some("French"), &["new".into()]),
            notefile::new_note(&fake.notetypes[0], None, &[]),
        ];
        let mut editor = script(vec![
            // Fill only the Back of the first block: the field check refuses.
            step(|t| Some(t.replacen("## Back\n", "## Back\nchat\n", 1))),
            step(|t| {
                assert!(t.contains("new note: the first field is empty"), "{t}");
                Some(t.replacen("## Front\n", "## Front\nle chat\n", 1))
            }),
        ]);
        let outcome = add_notes(&fake, &template, &base.options(), &mut editor).unwrap();
        let Outcome::Applied { added, updated, .. } = outcome else {
            panic!("{outcome:?}")
        };
        assert_eq!((added, updated), (vec![1000], vec![]));
        let note = fake.note(1000);
        assert_eq!(note.fields, ["le chat", "chat"]);
        assert_eq!(note.tags, ["new"]);
        assert_eq!(note.usn, 2, "added to the French deck");
        // An untouched template adds nothing.
        let mut untouched = |_: &Path| Ok(EditorExit::Success);
        assert_eq!(
            add_notes(&fake, &template, &base.options(), &mut untouched),
            Ok(Outcome::Unchanged)
        );
        assert!(base.is_empty());
    }

    #[test]
    fn apply_text_without_an_editor() {
        let fake = Fake::new();
        let text = "# Note\nnotetype: Basic\n\n## Front\nhund\n\n## Back\ndog\n";
        let applied = apply_text(&fake, text, &[], None).unwrap();
        let Applied::Done { added, .. } = applied else {
            panic!("{applied:?}")
        };
        assert_eq!(fake.note(added[0]).usn, 1, "DefaultsForAdding deck");
        assert!(fake.calls().contains(&"DefaultsForAdding".to_owned()));
        assert_eq!(
            apply_text(&fake, "# Note\nnotetype: Basic\nid: 5\n", &[], None),
            Ok(Applied::Problems(vec![Problem {
                line: 1,
                message: "note 5 is not one of the notes this file was written for; remove the id: line to add it as a new note".into(),
            }]))
        );
        assert!(matches!(
            apply_text(&fake, "oops", &[], None),
            Ok(Applied::Problems(p)) if p[0].line == 1
        ));
    }

    #[test]
    fn annotations_move_line_numbers() {
        let text = "# old header\n# more\n\n# Note\nnotetype: X\nbad line\n";
        let out = annotate(
            text,
            &[Problem {
                line: 6,
                message: "a\nb".into(),
            }],
        );
        let first = out.lines().nth(1).unwrap();
        assert_eq!(
            first,
            format!(
                "#   line {}: a b",
                out.lines().position(|l| l == "bad line").unwrap() + 1
            )
        );
        assert!(!out.contains("# old header"));
        assert_eq!(
            notefile::parse(&out).unwrap_err().message,
            notefile::parse(text).unwrap_err().message
        );
    }
}
