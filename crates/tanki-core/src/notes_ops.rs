//! Notes stream (Phase 3): note types, adding, fields check, cloze numbers,
//! preview, sticky fields, tags and media. Every Anki call is a fully
//! qualified service-trait call (PLAN 4.3).
use crate::actor::{changes, finish_render};
use crate::edit_convert::{all_notetypes, notetype_info, tag_node};
use crate::*;
use anki::{
    collection::Collection,
    services::{
        CardRenderingService, DecksService, MediaService, NotesService, NotetypesService,
        TagsService,
    },
};
use anki_proto as pb;

/// Runs a `NoteCommand`. `NoteCommand::UpdateNotes` never reaches here: the
/// actor sends it to `writeback::update_notes`.
pub(crate) fn execute(col: &mut Collection, command: NoteCommand) -> Result<EditResponse> {
    Ok(match command {
        NoteCommand::Notetypes => EditResponse::Notetypes(all_notetypes(col)?),
        NoteCommand::Notetype { id } => EditResponse::Notetype(notetype_info(col, id)?),
        NoteCommand::DeckNames { include_filtered } => {
            EditResponse::DeckNames(deck_names(col, include_filtered)?)
        }
        NoteCommand::DefaultsForAdding { home_deck } => {
            let d = NotesService::defaults_for_adding(
                col,
                pb::notes::DefaultsForAddingRequest {
                    home_deck_of_current_review_card: home_deck,
                },
            )?;
            EditResponse::Defaults {
                deck_id: d.deck_id,
                notetype_id: d.notetype_id,
            }
        }
        NoteCommand::DefaultDeckForNotetype { notetype_id } => {
            let d = NotesService::default_deck_for_notetype(
                col,
                pb::notetypes::NotetypeId { ntid: notetype_id },
            )?;
            EditResponse::Deck((d.did != 0).then_some(d.did))
        }
        NoteCommand::NewNote { notetype_id } => EditResponse::Note(
            NotesService::new_note(col, pb::notetypes::NotetypeId { ntid: notetype_id })?.into(),
        ),
        NoteCommand::CheckFields { note } => {
            use pb::notes::note_fields_check_response::State;
            let state = NotesService::note_fields_check(col, note.into())?.state();
            EditResponse::FieldsCheck(match state {
                State::Normal => FieldsCheck::Normal,
                State::Empty => FieldsCheck::Empty,
                State::Duplicate => FieldsCheck::Duplicate,
                State::MissingCloze => FieldsCheck::MissingCloze,
                State::NotetypeNotCloze => FieldsCheck::NotetypeNotCloze,
                State::FieldNotCloze => FieldsCheck::FieldNotCloze,
            })
        }
        NoteCommand::AddNote { note, deck_id } => {
            refuse_image_occlusion(col, &note)?;
            let r = NotesService::add_note(
                col,
                pb::notes::AddNoteRequest {
                    note: Some(note.into()),
                    deck_id,
                },
            )?;
            EditResponse::NoteAdded {
                id: r.note_id,
                changes: changes(r.changes.and_then(|c| c.changes).unwrap_or_default()),
            }
        }
        NoteCommand::AddNotes { notes } => {
            for (note, _) in &notes {
                refuse_image_occlusion(col, note)?;
            }
            let r = NotesService::add_notes(
                col,
                pb::notes::AddNotesRequest {
                    requests: notes
                        .into_iter()
                        .map(|(note, deck_id)| pb::notes::AddNoteRequest {
                            note: Some(note.into()),
                            deck_id,
                        })
                        .collect(),
                },
            )?;
            EditResponse::NotesAdded {
                ids: r.nids,
                changes: changes(r.changes.unwrap_or_default()),
            }
        }
        NoteCommand::GetNote { id } => EditResponse::Note(get_note(col, id)?),
        NoteCommand::GetNotes { ids } => EditResponse::Notes(
            ids.into_iter()
                .map(|id| get_note(col, id))
                .collect::<Result<Vec<_>>>()?,
        ),
        NoteCommand::UpdateNotes { .. } => {
            return Err(CoreError::Invalid(
                "UpdateNotes is handled by the write-back module".into(),
            ))
        }
        NoteCommand::ClozeNumbers { note } => {
            let mut numbers = NotesService::cloze_numbers_in_note(col, note.into())?.numbers;
            numbers.sort_unstable();
            numbers.dedup();
            EditResponse::Numbers(numbers)
        }
        NoteCommand::RenderPreview { note, card_ord } => {
            EditResponse::Rendered(render_preview(col, note, card_ord)?)
        }
        NoteCommand::SetSticky {
            notetype_id,
            field_ord,
            sticky,
        } => EditResponse::Changes(set_sticky(col, notetype_id, field_ord, sticky)?),
        NoteCommand::CompleteTag { input, limit } => EditResponse::Strings(
            TagsService::complete_tag(
                col,
                pb::tags::CompleteTagRequest {
                    input,
                    match_limit: limit,
                },
            )?
            .tags,
        ),
        NoteCommand::AllTags => EditResponse::Strings(TagsService::all_tags(col)?.vals),
        NoteCommand::TagTree => EditResponse::TagTree(tag_node(TagsService::tag_tree(col)?, "")),
        NoteCommand::AddTags { note_ids, tags } => count(TagsService::add_note_tags(
            col,
            pb::tags::NoteIdsAndTagsRequest { note_ids, tags },
        )?),
        NoteCommand::RemoveTags { note_ids, tags } => count(TagsService::remove_note_tags(
            col,
            pb::tags::NoteIdsAndTagsRequest { note_ids, tags },
        )?),
        NoteCommand::ClearUnusedTags => count(TagsService::clear_unused_tags(col)?),
        NoteCommand::AddMedia { desired_name, data } => {
            EditResponse::Media(add_media(col, desired_name, data)?)
        }
        NoteCommand::AddMediaFromPath { path } => {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| CoreError::Invalid("The path has no file name".into()))?
                .to_string();
            let data = std::fs::read(&path)
                .map_err(|e| CoreError::Invalid(format!("{}: {e}", path.display())))?;
            EditResponse::Media(add_media(col, name, data)?)
        }
        NoteCommand::AddMediaFromUrl { url } => {
            let (name, data) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| CoreError::Other(format!("Starting the download runtime: {e}")))?
                .block_on(anki::editor::retrieve_url(&url))?;
            EditResponse::Media(add_media(col, name, data)?)
        }
        NoteCommand::HtmlToText {
            html,
            preserve_media_filenames,
        } => EditResponse::Text(
            CardRenderingService::html_to_text_line(
                col,
                pb::card_rendering::HtmlToTextLineRequest {
                    text: html,
                    preserve_media_filenames,
                },
            )?
            .val,
        ),
    })
}

fn count(r: pb::collection::OpChangesWithCount) -> EditResponse {
    EditResponse::Count {
        count: r.count as usize,
        changes: changes(r.changes.unwrap_or_default()),
    }
}

impl From<NoteData> for pb::notes::Note {
    fn from(n: NoteData) -> Self {
        Self {
            id: n.id,
            guid: n.guid,
            notetype_id: n.notetype_id,
            mtime_secs: n.mtime_secs,
            usn: n.usn,
            tags: n.tags,
            fields: n.fields,
        }
    }
}

impl From<pb::notes::Note> for NoteData {
    fn from(n: pb::notes::Note) -> Self {
        Self {
            id: n.id,
            guid: n.guid,
            notetype_id: n.notetype_id,
            mtime_secs: n.mtime_secs,
            usn: n.usn,
            tags: n.tags,
            fields: n.fields,
        }
    }
}

fn get_note(col: &mut Collection, id: i64) -> Result<NoteData> {
    Ok(NotesService::get_note(col, pb::notes::NoteId { nid: id })?.into())
}

fn refuse_image_occlusion(col: &mut Collection, note: &NoteData) -> Result<()> {
    if notetype_info(col, note.notetype_id)?.image_occlusion {
        return Err(CoreError::Invalid(
            "Adding image occlusion notes is not supported yet".into(),
        ));
    }
    Ok(())
}

fn deck_names(col: &mut Collection, include_filtered: bool) -> Result<Vec<DeckName>> {
    let request = |include_filtered| pb::decks::GetDeckNamesRequest {
        skip_empty_default: false,
        include_filtered,
    };
    let normal = DecksService::get_deck_names(col, request(false))?.entries;
    if !include_filtered {
        return Ok(normal
            .into_iter()
            .map(|e| DeckName {
                id: e.id,
                name: e.name,
                filtered: false,
            })
            .collect());
    }
    let normal_ids: std::collections::HashSet<i64> = normal.iter().map(|e| e.id).collect();
    Ok(DecksService::get_deck_names(col, request(true))?
        .entries
        .into_iter()
        .map(|e| DeckName {
            filtered: !normal_ids.contains(&e.id),
            id: e.id,
            name: e.name,
        })
        .collect())
}

/// The template of the note type at the card ordinal (the cloze number - 1
/// for cloze types, which have a single template).
fn render_preview(col: &mut Collection, note: NoteData, card_ord: u32) -> Result<RenderedCard> {
    use pb::notetypes::notetype::config::Kind;
    let nt = NotetypesService::get_notetype(
        col,
        pb::notetypes::NotetypeId {
            ntid: note.notetype_id,
        },
    )?;
    let notetype_id = note.notetype_id;
    let cloze = nt.config.as_ref().map(|c| c.kind) == Some(Kind::Cloze as i32);
    let template_ord = if cloze { 0 } else { card_ord as usize };
    let template = nt
        .templates
        .into_iter()
        .nth(template_ord)
        .ok_or_else(|| CoreError::Invalid(format!("The note type has no card {card_ord}")))?;
    let rendered = CardRenderingService::render_uncommitted_card(
        col,
        pb::card_rendering::RenderUncommittedCardRequest {
            note: Some(note.into()),
            card_ord,
            template: Some(template),
            fill_empty: false,
            partial_render: true,
        },
    );
    finish_render(col, rendered, card_ord, notetype_id)
}

/// Changes the field's sticky flag in place. The note type's fields and
/// templates are passed back unchanged, so Anki sees no structural change and
/// leaves the schema modification time alone.
fn set_sticky(
    col: &mut Collection,
    notetype_id: i64,
    field_ord: u32,
    sticky: bool,
) -> Result<OpChanges> {
    let mut nt =
        NotetypesService::get_notetype(col, pb::notetypes::NotetypeId { ntid: notetype_id })?;
    let field = nt
        .fields
        .get_mut(field_ord as usize)
        .ok_or_else(|| CoreError::Invalid(format!("The note type has no field {field_ord}")))?;
    let config = field.config.get_or_insert_with(Default::default);
    if config.sticky == sticky {
        return Ok(OpChanges::default());
    }
    config.sticky = sticky;
    Ok(changes(NotetypesService::update_notetype(col, nt)?))
}

/// Extensions the editor turns into `<img>` (desktop's `pics`) and into
/// `[sound:]` (desktop's `audio`), compared lower-cased.
const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "gif", "svg", "webp", "ico", "avif"];
const AUDIO_EXTENSIONS: &[&str] = &[
    "3gp", "aac", "avi", "flac", "flv", "m4a", "mkv", "mov", "mp3", "mp4", "mpeg", "mpg", "oga",
    "ogg", "ogv", "ogx", "opus", "spx", "swf", "wav", "webm",
];

fn add_media(col: &mut Collection, desired_name: String, data: Vec<u8>) -> Result<AddedMedia> {
    let filename =
        MediaService::add_media_file(col, pb::media::AddMediaFileRequest { desired_name, data })?
            .val;
    let field_text = field_text(&filename);
    Ok(AddedMedia {
        filename,
        field_text,
    })
}

/// Desktop's `fnameToLink`: the image `src` is `urllib.parse.quote`d, the
/// sound name HTML-escaped without quotes.
fn field_text(filename: &str) -> String {
    let ext = filename
        .rsplit_once('.')
        .map_or("", |(_, e)| e)
        .to_lowercase();
    if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        format!("<img src=\"{}\">", url_quote(filename))
    } else if AUDIO_EXTENSIONS.contains(&ext.as_str()) {
        format!("[sound:{}]", html_escape(filename))
    } else {
        filename.to_string()
    }
}

/// Python's `urllib.parse.quote(s.encode("utf8"))` (safe characters `/` and
/// `_.-~` plus ASCII alphanumerics).
fn url_quote(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"_.-~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `html.escape(s, quote=False)`.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_links_follow_desktop() {
        assert_eq!(field_text("a b&c.PNG"), "<img src=\"a%20b%26c.PNG\">");
        assert_eq!(field_text("x&y.mp3"), "[sound:x&amp;y.mp3]");
        assert_eq!(field_text("doc.pdf"), "doc.pdf");
        assert_eq!(field_text("é.jpg"), "<img src=\"%C3%A9.jpg\">");
    }

    #[test]
    fn sticky_is_persisted_without_a_schema_change() {
        let dir = tempfile::tempdir().unwrap();
        let mut col = anki::collection::CollectionBuilder::new(dir.path().join("c.anki2"))
            .build()
            .unwrap();
        let basic = all_notetypes(&mut col)
            .unwrap()
            .into_iter()
            .find(|nt| nt.name == "Basic")
            .unwrap();
        assert!(!basic.fields[0].sticky);
        // Not a full sync before ...
        assert_eq!(
            crate::actor::confirm_schema_change(&mut col, false),
            Err(CoreError::SchemaChangeRequired)
        );
        let changes = set_sticky(&mut col, basic.id, 0, true).unwrap();
        assert!(changes.notetype);
        assert!(notetype_info(&mut col, basic.id).unwrap().fields[0].sticky);
        // ... and still not after: no one-way sync was forced.
        assert_eq!(
            crate::actor::confirm_schema_change(&mut col, false),
            Err(CoreError::SchemaChangeRequired)
        );
        set_sticky(&mut col, basic.id, 0, false).unwrap();
        assert!(!notetype_info(&mut col, basic.id).unwrap().fields[0].sticky);
        assert!(set_sticky(&mut col, basic.id, 9, true).is_err());
        col.close(None).unwrap();
    }
}
