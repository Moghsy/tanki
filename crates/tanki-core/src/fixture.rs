use crate::Result;
use anki::{
    collection::Collection,
    services::{MediaService, NotesService},
};
use anki_proto as pb;

pub(crate) fn populate(col: &mut Collection) -> Result<()> {
    // Synthetic SVG and short silent PCM WAV; names returned by Anki are authoritative.
    let image = MediaService::add_media_file(col, pb::media::AddMediaFileRequest { desired_name:"fixture.svg".into(), data:br##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><rect width="100" height="100" fill="#3366cc"/></svg>"##.to_vec() })?.val;
    let mut wav = b"RIFF".to_vec();
    wav.extend_from_slice(&196u32.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&8000u32.to_le_bytes());
    wav.extend_from_slice(&16000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&160u32.to_le_bytes());
    wav.resize(204, 0);
    let sound = MediaService::add_media_file(
        col,
        pb::media::AddMediaFileRequest {
            desired_name: "fixture.wav".into(),
            data: wav,
        },
    )?
    .val;
    for nt in col.get_all_notetypes()? {
        let mut note = NotesService::new_note(col, pb::notetypes::NotetypeId { ntid: nt.id.0 })?;
        for (i, field) in nt.fields.iter().enumerate() {
            note.fields[i] = match field.name.as_str() {
                "Front" => format!("<b>Question</b> <img src=\"{image}\"> [sound:{sound}]"),
                "Back" | "Back Extra" => format!("Answer [sound:{sound}]"),
                "Text" => "A {{c1::cloze}} and {{c2::second}}".into(),
                "Add Reverse" => "yes".into(),
                "Occlusion" => {
                    "{{c1::image-occlusion:rect:left=.1:top=.1:width=.4:height=.4:oi=1}}".into()
                }
                "Image" => format!("<img src=\"{image}\">"),
                "Header" => "Synthetic image occlusion".into(),
                _ => String::new(),
            };
        }
        let _ = NotesService::add_note(
            col,
            pb::notes::AddNoteRequest {
                note: Some(note),
                deck_id: 1,
            },
        )?;
    }
    Ok(())
}
