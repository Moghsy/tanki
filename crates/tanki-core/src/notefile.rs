//! The note file format for `$EDITOR` round trips and `tanki add --file`
//! (PLAN 8.5, R7 §6.4, after apy's Markdown notes, R9 §1). Pure text
//! processing; it never touches a collection.
//!
//! The invariant: `parse(&write(notes)) == notes` byte for byte for every
//! field value (the Phase 3 exit test), so comparing parsed values with the
//! originals finds exactly the fields the user changed.
//!
//! # Format
//!
//! ```text
//! # tanki note file: edit, then save and quit to apply. ...   <- header comments
//!
//! # Note
//! id: 1712345678901
//! mtime: 1712345678
//! notetype: Basic
//! tags: geo europe
//!
//! ## Front
//! Paris is the capital of <b>France</b>
//!
//! ## Back {quoted}
//! "  leading spaces and a trailing newline\n"
//! "\\(x^2\\) "
//!
//! # Note
//! notetype: Cloze
//! deck: Languages::French
//! tags:
//!
//! ## Text
//! {{c1::Paris}} is the capital of {{c2::France::country}}
//!
//! ## Back Extra
//! ```
//!
//! - Lines starting with `#` before the first `# Note` are comments; `write`
//!   puts a header there that says how to save and abort.
//! - Every note starts with a line `# Note`, followed by front matter lines
//!   `key: value`: `id` (absent for a note to add; `nid` is accepted too),
//!   `mtime`, `notetype` (`model` is accepted too), `deck`, `tags`
//!   (space-separated). A value that starts with `"` is a JSON string; tags
//!   that start with `[` are a JSON array. `write` uses those forms only
//!   for names that cannot be written plainly.
//! - Each field is a heading `## Name` followed by its stored HTML. The
//!   body runs to the next `## ` heading or `# Note` line; blank lines and
//!   spaces around it are dropped, everything else is kept as typed.
//! - `## Name {quoted}` marks a field whose body is one JSON string per line,
//!   concatenated (blank lines ignored). `write` chooses it automatically,
//!   per field, when the value cannot be written plainly: leading or
//!   trailing whitespace, a line with trailing whitespace, a line starting
//!   with `#`, `\r` or other control characters, U+2028/U+2029/U+FEFF. So
//!   the file survives editors that strip trailing whitespace, convert line
//!   endings to CRLF or add a final newline. A field name that is not a valid
//!   Anki field name is written as a JSON string too.
//!
//! # What the user may change
//!
//! Field text and tags of any note; `deck` only on new notes (blocks without
//! `id`). Changing `notetype` of an existing note, adding `deck` to one,
//! duplicating an `id`, an unknown `id` and unknown or repeated field names
//! are refused by [`plan`]. A missing field leaves it unchanged (empty for a
//! new note); removing a whole block leaves that note alone. A new block whose
//! fields are all empty is skipped, so an untouched template adds nothing.
//!
//! # Saving and aborting (implemented by `tanki_tui::editor_flow`)
//!
//! Save and quit to apply. A file saved unchanged changes nothing; an editor
//! that exits with an error (`:cq` in vim) or a file emptied of notes aborts.
use crate::{DeckName, NoteData, NoteUpdate, NotetypeInfo};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt::Write as _;

/// A note as it appears in a note file. Field values are stored HTML.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileNote {
    /// `None` for a note to add.
    pub id: Option<i64>,
    /// The note's `mtime_secs` when it was written out (conflict check).
    pub mtime: Option<u32>,
    pub notetype: String,
    pub deck: Option<String>,
    pub tags: Vec<String>,
    /// (field name, stored HTML), in note-type order.
    pub fields: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseError {
    /// 1-based line in the file.
    pub line: usize,
    pub message: String,
}
impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}
impl std::error::Error for ParseError {}

/// The comment block `write` puts at the top of every file.
pub const HEADER: &str = "\
# tanki note file. Edit, then save and quit to apply. To cancel, quit
# without saving, make the editor exit with an error (:cq in vim), or
# delete everything.
#
# Each note starts with \"# Note\", then id/mtime/notetype/deck/tags lines,
# then one \"## Field\" heading per field; field text is the stored HTML.
# A \"## Field {quoted}\" field holds one JSON string per line, joined; it is
# used for text with edge spaces, trailing spaces or special characters.
# You can change fields and tags. deck: is only for new notes (blocks
# without id:). Do not change id:, mtime: or notetype:. Removing a block
# leaves that note alone; copy a block without its id: line to add a note.
# Lines starting with # before the first note are comments.
";

const NOTE_LINE: &str = "# Note";
const QUOTED: &str = "quoted";

fn is_note_line(line: &str) -> bool {
    line.trim_end() == NOTE_LINE
}

fn is_field_line(line: &str) -> bool {
    line.starts_with("## ") || line.trim_end() == "##"
}

/// Characters that never appear literally outside a JSON string.
fn special(c: char) -> bool {
    c.is_control() || matches!(c, '\u{2028}' | '\u{2029}' | '\u{feff}')
}

/// A JSON string literal that escapes every invisible or line-breaking
/// character (serde_json leaves DEL, C1 controls and U+2028 raw).
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if special(c) => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Whether `value` can be written as a plain field body and read back
/// unchanged, also after an editor stripped trailing whitespace or
/// converted line endings.
fn plain_value(value: &str) -> bool {
    !value.starts_with(char::is_whitespace)
        && !value.ends_with(char::is_whitespace)
        && !value.chars().any(|c| c != '\n' && c != '\t' && special(c))
        && value
            .split('\n')
            .all(|line| !line.ends_with(char::is_whitespace) && !line.starts_with('#'))
}

/// Front-matter values (note type, deck).
fn plain_scalar(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with(char::is_whitespace)
        && !s.ends_with(char::is_whitespace)
        && !s.starts_with('"')
        && !s.chars().any(special)
}

fn scalar(s: &str) -> String {
    if plain_scalar(s) {
        s.to_owned()
    } else {
        quote(s)
    }
}

fn plain_name(s: &str) -> bool {
    plain_scalar(s) && !s.contains(['{', '}'])
}

fn tags_value(tags: &[String]) -> String {
    let plain = tags.iter().all(|t| {
        !t.is_empty()
            && !t.chars().any(|c| c.is_whitespace() || special(c))
            && !t.starts_with(['"', '['])
    });
    if plain {
        tags.join(" ")
    } else {
        let quoted: Vec<String> = tags.iter().map(|t| quote(t)).collect();
        format!("[{}]", quoted.join(", "))
    }
}

/// Writes notes as a note file, header comment included.
pub fn write(notes: &[FileNote]) -> String {
    let mut out = String::from(HEADER);
    for note in notes {
        out.push('\n');
        out.push_str(NOTE_LINE);
        out.push('\n');
        if let Some(id) = note.id {
            let _ = writeln!(out, "id: {id}");
        }
        if let Some(mtime) = note.mtime {
            let _ = writeln!(out, "mtime: {mtime}");
        }
        let _ = writeln!(out, "notetype: {}", scalar(&note.notetype));
        if let Some(deck) = &note.deck {
            let _ = writeln!(out, "deck: {}", scalar(deck));
        }
        if note.tags.is_empty() {
            out.push_str("tags:\n");
        } else {
            let _ = writeln!(out, "tags: {}", tags_value(&note.tags));
        }
        for (name, value) in &note.fields {
            out.push_str("\n## ");
            if plain_name(name) {
                out.push_str(name);
            } else {
                out.push_str(&quote(name));
            }
            if plain_value(value) {
                out.push('\n');
                if !value.is_empty() {
                    out.push_str(value);
                    out.push('\n');
                }
            } else {
                let _ = writeln!(out, " {{{QUOTED}}}");
                for chunk in value.split_inclusive('\n') {
                    out.push_str(&quote(chunk));
                    out.push('\n');
                }
            }
        }
    }
    out
}

/// Parses a note file. Errors carry the 1-based line.
pub fn parse(text: &str) -> Result<Vec<FileNote>, ParseError> {
    Ok(parse_located(text)?
        .into_iter()
        .map(|(_, note)| note)
        .collect())
}

fn error<T>(line: usize, message: impl Into<String>) -> Result<T, ParseError> {
    Err(ParseError {
        line,
        message: message.into(),
    })
}

fn json_string(text: &str, line: usize) -> Result<String, ParseError> {
    serde_json::from_str::<String>(text).or_else(|e| {
        error(
            line,
            format!("invalid quoted text {text:?}: {e} (write it as a JSON string, like \"a \\\"b\\\"\\n\")"),
        )
    })
}

fn front_scalar(value: &str, line: usize) -> Result<String, ParseError> {
    if value.starts_with('"') {
        json_string(value, line)
    } else {
        Ok(value.to_owned())
    }
}

/// `## Name` or `## Name {quoted}` -> (name, quoted).
fn heading(line: &str, number: usize) -> Result<(String, bool), ParseError> {
    let text = line[2..].trim();
    let (name, quoted) = match (text.ends_with('}'), text.rfind('{')) {
        (true, Some(open)) => {
            let marker = &text[open + 1..text.len() - 1];
            if !marker.trim().eq_ignore_ascii_case(QUOTED) {
                return error(
                    number,
                    format!("unknown field marker {{{marker}}}; the only marker is {{{QUOTED}}}"),
                );
            }
            (text[..open].trim_end(), true)
        }
        _ => (text, false),
    };
    if name.is_empty() {
        return error(number, "field heading without a name; write \"## Name\"");
    }
    let name = if name.starts_with('"') {
        json_string(name, number)?
    } else {
        name.to_owned()
    };
    Ok((name, quoted))
}

fn plain_body(lines: &[&str]) -> String {
    lines.join("\n").trim_matches([' ', '\t', '\n']).to_owned()
}

fn quoted_body(lines: &[&str], first: usize) -> Result<String, ParseError> {
    let mut value = String::new();
    for (offset, line) in lines.iter().enumerate() {
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        let number = first + offset + 1;
        if !text.starts_with('"') {
            return error(
                number,
                format!(
                    "in a {{{QUOTED}}} field every line is one JSON string in double quotes, \
                     like \"text\\n\"; found {text:?} (or remove {{{QUOTED}}} from the heading)"
                ),
            );
        }
        value.push_str(&json_string(text, number)?);
    }
    Ok(value)
}

/// Like [`parse`], with the 1-based line of each note's `# Note` line.
pub fn parse_located(text: &str) -> Result<Vec<(usize, FileNote)>, ParseError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    // Written files never contain a literal CR (values with one are quoted),
    // so a CRLF file is an editor's conversion.
    let text = text.replace("\r\n", "\n");
    let lines: Vec<&str> = text.split('\n').collect();
    let mut i = 0;
    while i < lines.len() && !is_note_line(lines[i]) {
        let line = lines[i];
        if !line.trim().is_empty() && !line.starts_with('#') {
            return error(
                i + 1,
                "text outside a note: each note starts with a line \"# Note\", and lines \
                 before the first note must be comments starting with #",
            );
        }
        i += 1;
    }
    let mut notes = Vec::new();
    while i < lines.len() {
        let start = i + 1;
        i += 1;
        let mut note = FileNote::default();
        let mut seen = HashSet::new();
        while i < lines.len() && !is_note_line(lines[i]) && !is_field_line(lines[i]) {
            let number = i + 1;
            let line = lines[i].trim();
            i += 1;
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once(':') else {
                return error(
                    number,
                    format!(
                        "expected \"key: value\" (id, mtime, notetype, deck or tags) or a \
                         \"## Field\" heading, found {line:?}"
                    ),
                );
            };
            let key = match key.trim().to_ascii_lowercase().as_str() {
                "nid" | "id" => "id",
                "model" | "notetype" => "notetype",
                "mtime" => "mtime",
                "deck" => "deck",
                "tags" => "tags",
                other => {
                    return error(
                        number,
                        format!(
                            "unknown key {other:?}; the keys are id, mtime, notetype, deck and tags"
                        ),
                    )
                }
            };
            if !seen.insert(key) {
                return error(number, format!("{key}: appears twice in this note"));
            }
            let value = value.trim();
            match key {
                "id" => {
                    note.id = Some(value.parse().or_else(|_| {
                        error(
                            number,
                            format!("id must be a note id number, not {value:?}"),
                        )
                    })?)
                }
                "mtime" => {
                    note.mtime = Some(value.parse().or_else(|_| {
                        error(
                            number,
                            format!("mtime must be a number of seconds, not {value:?}"),
                        )
                    })?)
                }
                "notetype" => note.notetype = front_scalar(value, number)?,
                "deck" => note.deck = Some(front_scalar(value, number)?),
                _ => {
                    note.tags = if value.starts_with('[') {
                        serde_json::from_str(value).or_else(|e| {
                            error(
                                number,
                                format!("invalid tag list {value:?}: {e} (write tags separated by spaces)"),
                            )
                        })?
                    } else {
                        value.split_whitespace().map(str::to_owned).collect()
                    }
                }
            }
        }
        if !seen.contains("notetype") {
            return error(start, "this note has no \"notetype:\" line");
        }
        while i < lines.len() && is_field_line(lines[i]) {
            let (name, quoted) = heading(lines[i], i + 1)?;
            i += 1;
            let first = i;
            while i < lines.len() && !is_note_line(lines[i]) && !is_field_line(lines[i]) {
                i += 1;
            }
            let value = if quoted {
                quoted_body(&lines[first..i], first)?
            } else {
                plain_body(&lines[first..i])
            };
            note.fields.push((name, value));
        }
        notes.push((start, note));
    }
    Ok(notes)
}

// ---- conversion and planning ------------------------------------------------

/// An existing note as a file block. Existing notes are written without
/// `deck:` (a note's cards may be in several decks; see [`plan`]).
pub fn from_note(note: &NoteData, notetype: &NotetypeInfo) -> FileNote {
    FileNote {
        id: Some(note.id),
        mtime: Some(note.mtime_secs),
        notetype: notetype.name.clone(),
        deck: None,
        tags: note.tags.clone(),
        fields: note
            .fields
            .iter()
            .enumerate()
            .map(|(ord, value)| {
                let name = notetype
                    .fields
                    .get(ord)
                    .map_or_else(|| format!("Field {}", ord + 1), |f| f.name.clone());
                (name, value.clone())
            })
            .collect(),
    }
}

/// An empty block for a new note of this type (`tanki add` templates).
pub fn new_note(notetype: &NotetypeInfo, deck: Option<&str>, tags: &[String]) -> FileNote {
    FileNote {
        id: None,
        mtime: None,
        notetype: notetype.name.clone(),
        deck: deck.map(str::to_owned),
        tags: tags.to_vec(),
        fields: notetype
            .fields
            .iter()
            .map(|f| (f.name.clone(), String::new()))
            .collect(),
    }
}

/// What [`plan`] checks an edited file against.
#[derive(Debug, Clone, Copy)]
pub struct PlanContext<'a> {
    /// The notes as stored when the file was written.
    pub originals: &'a [NoteData],
    /// At least the note types of the originals and of new blocks.
    pub notetypes: &'a [NotetypeInfo],
    /// Decks new blocks may name (filtered decks are refused).
    pub decks: &'a [DeckName],
    /// Deck for new blocks without `deck:`; `None` refuses them.
    pub default_deck: Option<i64>,
}

/// A new block, ready for `NoteCommand::NewNote` + `AddNotes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedAdd {
    /// Position of the block among the parsed notes.
    pub index: usize,
    pub notetype_id: i64,
    pub deck_id: i64,
    pub tags: Vec<String>,
    /// Every field, in ordinal order.
    pub fields: Vec<String>,
}

/// Why a block cannot be applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    /// Position of the block among the parsed notes.
    pub index: usize,
    pub note_id: Option<i64>,
    pub message: String,
}

/// The result of [`plan`]. Apply nothing while `refusals` is non-empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// Changed-fields-only updates for `NoteCommand::UpdateNotes`.
    pub updates: Vec<NoteUpdate>,
    pub adds: Vec<PlannedAdd>,
    /// Existing notes in the file with no changes.
    pub unchanged: Vec<i64>,
    /// New blocks whose fields are all empty (block indexes).
    pub skipped: Vec<usize>,
    pub refusals: Vec<Refusal>,
}

impl Plan {
    /// Nothing to write.
    pub fn is_empty(&self) -> bool {
        self.updates.is_empty() && self.adds.is_empty()
    }
}

/// Maps a block's fields to ordinals of `notetype`; messages for unknown or
/// repeated names.
fn field_ords<'n>(
    note: &'n FileNote,
    notetype: &NotetypeInfo,
    messages: &mut Vec<String>,
) -> Vec<(usize, &'n str)> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (name, value) in &note.fields {
        match notetype.fields.iter().position(|f| &f.name == name) {
            Some(ord) if seen.insert(ord) => out.push((ord, value.as_str())),
            Some(_) => messages.push(format!("field {name:?} appears twice")),
            None => {
                let names: Vec<&str> = notetype.fields.iter().map(|f| f.name.as_str()).collect();
                messages.push(format!(
                    "note type {:?} has no field {name:?} (its fields: {})",
                    notetype.name,
                    names.join(", ")
                ));
            }
        }
    }
    out
}

/// Turns an edited file into changed-fields-only updates, adds and
/// refusals. `edited` is the parsed file (`parse`); refusal and add indexes
/// refer to it (`parse_located` gives the lines).
///
/// For an existing note, `expected_mtime` is the block's `mtime:` (the note
/// as the file was written), else the original's; `expected_fields` are the
/// original's fields, and only fields that differ from them are listed.
pub fn plan(ctx: &PlanContext<'_>, edited: &[FileNote]) -> Plan {
    let mut out = Plan::default();
    let mut ids = HashSet::new();
    for (index, note) in edited.iter().enumerate() {
        let mut messages = Vec::new();
        match note.id {
            Some(id) => {
                if !ids.insert(id) {
                    messages.push(format!(
                        "note {id} appears twice; remove the id: line of the copy to add it as a new note"
                    ));
                } else if let Some(original) = ctx.originals.iter().find(|n| n.id == id) {
                    plan_update(ctx, note, original, &mut messages, &mut out);
                } else {
                    messages.push(format!(
                        "note {id} is not one of the notes this file was written for; \
                         remove the id: line to add it as a new note"
                    ));
                }
            }
            None => plan_add(ctx, index, note, &mut messages, &mut out),
        }
        out.refusals
            .extend(messages.into_iter().map(|message| Refusal {
                index,
                note_id: note.id,
                message,
            }));
    }
    out
}

fn plan_update(
    ctx: &PlanContext<'_>,
    note: &FileNote,
    original: &NoteData,
    messages: &mut Vec<String>,
    out: &mut Plan,
) {
    let id = original.id;
    let Some(notetype) = ctx
        .notetypes
        .iter()
        .find(|nt| nt.id == original.notetype_id)
    else {
        messages.push(format!("the note type of note {id} no longer exists"));
        return;
    };
    if note.notetype != notetype.name {
        messages.push(format!(
            "note {id}: the note type cannot be changed here (from {:?} to {:?}); \
             change notetype: back",
            notetype.name, note.notetype
        ));
        return;
    }
    if note.deck.is_some() {
        messages.push(format!(
            "note {id}: deck: only applies to new notes; remove it and move the cards \
             with the browser's Change Deck"
        ));
    }
    let ords = field_ords(note, notetype, messages);
    if !messages.is_empty() {
        return;
    }
    let mut fields = Vec::new();
    for (ord, value) in ords {
        match original.fields.get(ord) {
            Some(stored) if stored == value => {}
            Some(_) => fields.push((ord as u32, value.to_owned())),
            None => {
                messages.push(format!("note {id} has no field {}", ord + 1));
                return;
            }
        }
    }
    fields.sort_by_key(|(ord, _)| *ord);
    let tags = (note.tags != original.tags).then(|| note.tags.clone());
    if fields.is_empty() && tags.is_none() {
        out.unchanged.push(id);
    } else {
        out.updates.push(NoteUpdate {
            id,
            expected_mtime: note.mtime.unwrap_or(original.mtime_secs),
            expected_fields: Some(original.fields.clone()),
            fields,
            tags,
        });
    }
}

fn plan_add(
    ctx: &PlanContext<'_>,
    index: usize,
    note: &FileNote,
    messages: &mut Vec<String>,
    out: &mut Plan,
) {
    let Some(notetype) = ctx.notetypes.iter().find(|nt| nt.name == note.notetype) else {
        let names: Vec<&str> = ctx.notetypes.iter().map(|nt| nt.name.as_str()).collect();
        messages.push(format!(
            "unknown note type {:?} (note types: {})",
            note.notetype,
            names.join(", ")
        ));
        return;
    };
    let ords = field_ords(note, notetype, messages);
    if !messages.is_empty() {
        return;
    }
    let mut fields = vec![String::new(); notetype.fields.len()];
    for (ord, value) in ords {
        fields[ord] = value.to_owned();
    }
    if fields.iter().all(String::is_empty) {
        out.skipped.push(index);
        return;
    }
    if notetype.image_occlusion {
        messages.push(format!(
            "tanki cannot add image occlusion notes yet ({:?})",
            notetype.name
        ));
        return;
    }
    let deck_id = match &note.deck {
        Some(name) => {
            let wanted = name.trim().to_lowercase();
            match ctx.decks.iter().find(|d| d.name.to_lowercase() == wanted) {
                Some(deck) if deck.filtered => {
                    messages.push(format!(
                        "{name:?} is a filtered deck; notes cannot be added to it"
                    ));
                    return;
                }
                Some(deck) => deck.id,
                None => {
                    messages.push(format!("there is no deck named {name:?}"));
                    return;
                }
            }
        }
        None => match ctx.default_deck {
            Some(id) => id,
            None => {
                messages.push("new note without a deck: line".into());
                return;
            }
        },
    };
    out.adds.push(PlannedAdd {
        index,
        notetype_id: notetype.id,
        deck_id,
        tags: note.tags.clone(),
        fields,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn basic() -> FileNote {
        FileNote {
            id: Some(5),
            mtime: Some(7),
            notetype: "Basic".into(),
            deck: None,
            tags: vec!["a".into(), "b::c".into()],
            fields: vec![
                ("Front".into(), "Paris".into()),
                ("Back".into(), "a<div>b</div>".into()),
            ],
        }
    }

    #[test]
    fn plain_fields_read_like_markdown() {
        let text = write(&[basic()]);
        assert!(text.starts_with(HEADER));
        assert!(text.ends_with(
            "\n# Note\nid: 5\nmtime: 7\nnotetype: Basic\ntags: a b::c\n\n## Front\nParis\n\n## Back\na<div>b</div>\n"
        ));
        assert_eq!(parse(&text).unwrap(), vec![basic()]);
    }

    #[test]
    fn awkward_values_use_the_quoted_form() {
        let mut note = basic();
        note.fields[1].1 = "x \n# Note\r\n".into();
        let text = write(&[note.clone()]);
        assert!(text.contains("## Back {quoted}\n\"x \\n\"\n\"# Note\\r\\n\"\n"));
        assert_eq!(parse(&text).unwrap(), vec![note]);
    }

    #[test]
    fn errors_name_the_line() {
        let e = parse("# c\nhello\n").unwrap_err();
        assert_eq!(e.line, 2);
        let e = parse("# Note\nnotetype: Basic\ncolour: red\n").unwrap_err();
        assert_eq!((e.line, e.message.contains("unknown key")), (3, true));
        let e = parse("# Note\nid: x\n").unwrap_err();
        assert_eq!(e.line, 2);
        let e = parse("\n# Note\ntags: a\n## F\n").unwrap_err();
        assert_eq!((e.line, e.message.contains("notetype")), (2, true));
        let e = parse("# Note\nnotetype: B\n## F {raw}\n").unwrap_err();
        assert_eq!(e.line, 3);
        let e = parse("# Note\nnotetype: B\n## F {quoted}\n\"a\"\nb\n").unwrap_err();
        assert_eq!(e.line, 5);
        assert_eq!(e.to_string().split(':').next(), Some("line 5"));
    }

    #[test]
    fn located_lines_and_aliases() {
        let text = "# Note\nnid: 1\nmodel: Basic\n## Front\nx\n\n# Note\nnotetype: \"Odd \"\ntags: [\"a b\"]\n";
        let notes = parse_located(text).unwrap();
        assert_eq!(notes[0].0, 1);
        assert_eq!(notes[1].0, 7);
        assert_eq!(notes[0].1.id, Some(1));
        assert_eq!(notes[0].1.notetype, "Basic");
        assert_eq!(notes[1].1.notetype, "Odd ");
        assert_eq!(notes[1].1.tags, ["a b"]);
    }
}
