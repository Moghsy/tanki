const ACTIONS: &[&str] = &[
    "version",
    "requestPermission",
    "apiReflect",
    "multi",
    "deckNames",
    "deckNamesAndIds",
    "createDeck",
    "changeDeck",
    "modelNames",
    "modelNamesAndIds",
    "modelFieldNames",
    "modelFieldsOnTemplates",
    "createModel",
    "findNotes",
    "findCards",
    "notesInfo",
    "cardsInfo",
    "canAddNotes",
    "canAddNotesWithErrorDetail",
    "addNote",
    "addNotes",
    "updateNoteFields",
    "updateNote",
    "addTags",
    "removeTags",
    "getTags",
    "deleteNotes",
    "storeMediaFile",
    "retrieveMediaFile",
    "getMediaFilesNames",
    "getMediaDirPath",
    "getNumCardsReviewedToday",
    "guiBrowse",
];
// AnkiConnect protocol adapter. Collection access stays on the core actor.
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use tanki_core::*;

pub trait ConnectBackend: Send + Sync {
    fn execute(&self, command: Command) -> tanki_core::Result<Response>;
    fn media_dir(&self) -> std::path::PathBuf;
}
impl ConnectBackend for CollectionActor {
    fn execute(&self, command: Command) -> tanki_core::Result<Response> {
        CollectionActor::execute(self, command)
    }
    fn media_dir(&self) -> std::path::PathBuf {
        CollectionActor::media_dir(self)
    }
}
#[derive(Clone, Debug)]
pub struct ServeOptions {
    pub bind: std::net::SocketAddr,
    pub api_key: Option<String>,
    pub cors_origins: Vec<String>,
}
impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8765".parse().unwrap(),
            api_key: None,
            cors_origins: vec![],
        }
    }
}
impl ServeOptions {
    pub fn from_settings(
        settings: &tanki_core::config::ConnectSettings,
    ) -> std::result::Result<Self, std::net::AddrParseError> {
        Ok(Self {
            bind: settings
                .bind
                .as_deref()
                .unwrap_or("127.0.0.1:8765")
                .parse()?,
            api_key: settings.api_key.clone(),
            cors_origins: settings.cors_origins.clone(),
        })
    }
}
fn allowed(options: &ServeOptions, origin: &str) -> bool {
    origin == "http://localhost" || options.cors_origins.iter().any(|s| s == "*" || s == origin)
}
fn error(message: impl ToString) -> Value {
    json!({"result":null,"error":message.to_string()})
}
pub fn request(backend: &dyn ConnectBackend, options: &ServeOptions, request: Value) -> Value {
    request_inner(backend, options, request, 0)
}
fn request_inner(
    backend: &dyn ConnectBackend,
    options: &ServeOptions,
    request: Value,
    depth: u8,
) -> Value {
    if depth > 16 {
        return error("multi nesting limit exceeded");
    }
    let modern = request["version"].as_u64().unwrap_or(4) >= 5;
    let result = if options
        .api_key
        .as_deref()
        .is_some_and(|key| request["key"].as_str() != Some(key))
        && request["action"] != "requestPermission"
    {
        Err("valid api key must be provided".into())
    } else if request["action"] == "requestPermission" {
        Ok(json!({"permission":"granted","requireApiKey":options.api_key.is_some(),"version":6}))
    } else if request["action"] == "multi" {
        match request["params"]["actions"].as_array() {
            Some(actions) => Ok(json!(actions
                .iter()
                .map(|r| request_inner(backend, options, r.clone(), depth + 1))
                .collect::<Vec<_>>())),
            None => Err("invalid actions".into()),
        }
    } else {
        action(
            backend,
            request["action"].as_str().unwrap_or(""),
            &request["params"],
        )
    };
    match result {
        Ok(result) if modern => json!({"result":result,"error":null}),
        Ok(result) => result,
        Err(e) => error(e),
    }
}
type ApiResult = std::result::Result<Value, String>;
fn execute(
    backend: &dyn ConnectBackend,
    command: Command,
) -> std::result::Result<Response, String> {
    backend.execute(command).map_err(|e| e.to_string())
}
fn notes(
    backend: &dyn ConnectBackend,
    command: NoteCommand,
) -> std::result::Result<EditResponse, String> {
    match execute(backend, Command::Notes(command))? {
        Response::Edit(r) => Ok(r),
        _ => Err("unexpected core response".into()),
    }
}
fn ids(params: &Value, key: &str) -> std::result::Result<Vec<i64>, String> {
    serde_json::from_value(params[key].clone()).map_err(|_| format!("invalid {key}"))
}
fn text<'a>(params: &'a Value, key: &str) -> std::result::Result<&'a str, String> {
    params[key].as_str().ok_or_else(|| format!("invalid {key}"))
}
fn connect(backend: &dyn ConnectBackend, command: connect::ConnectCommand) -> ApiResult {
    match execute(backend, Command::Manage(ManageCommand::Connect(command)))? {
        Response::Manage(ManageResponse::Connect(r)) => Ok(r.result),
        _ => Err("unexpected core response".into()),
    }
}
fn manage_notetype(
    backend: &dyn ConnectBackend,
    command: tanki_core::NotetypeCommand,
) -> std::result::Result<tanki_core::NotetypeResponse, String> {
    match execute(backend, Command::Manage(ManageCommand::Notetypes(command)))? {
        Response::Manage(ManageResponse::Notetypes(r)) => Ok(r),
        _ => Err("unexpected core response".into()),
    }
}

/// AnkiConnect `createModel` for normal (non-cloze) note types: fields in
/// order, one or more card templates, optional CSS. An existing name is refused,
/// as AnkiConnect does. This is a schema change, which needs a full sync.
fn create_model(backend: &dyn ConnectBackend, p: &Value) -> ApiResult {
    use tanki_core::notetypes::{NotetypeEdit, StockKind, TemplateDetail};
    use tanki_core::NotetypeCommand::{AddStock, Edit};
    let name = text(p, "modelName")?.trim().to_owned();
    if name.is_empty() {
        return Err("modelName is empty".into());
    }
    if p["isCloze"].as_bool().unwrap_or(false) {
        return Err("cloze models are not supported by createModel".into());
    }
    let fields: Vec<String> =
        serde_json::from_value(p["inOrderFields"].clone()).map_err(|_| "invalid inOrderFields")?;
    let templates: Vec<Value> =
        serde_json::from_value(p["cardTemplates"].clone()).map_err(|_| "invalid cardTemplates")?;
    if fields.is_empty() || templates.is_empty() {
        return Err("a model needs at least one field and one card template".into());
    }
    let EditResponse::Notetypes(existing) = notes(backend, NoteCommand::Notetypes)? else {
        unreachable!()
    };
    if existing.iter().any(|m| m.name == name) {
        return Err("Model name already exists".into());
    }
    let tanki_core::NotetypeResponse::Added { id, .. } = manage_notetype(
        backend,
        AddStock {
            kind: StockKind::Basic,
            name: Some(name.clone()),
        },
    )?
    else {
        return Err("unexpected core response".into());
    };
    // Stock Basic has Front/Back and one card type; reshape it to the request.
    let mut edits = Vec::new();
    for (i, field) in fields.iter().enumerate() {
        edits.push(if i < 2 {
            NotetypeEdit::RenameField {
                index: i,
                name: field.clone(),
            }
        } else {
            NotetypeEdit::AddField {
                name: field.clone(),
            }
        });
    }
    if fields.len() == 1 {
        edits.push(NotetypeEdit::DeleteField { index: 1 });
    }
    for (i, template) in templates.iter().enumerate() {
        let get = |key: &str| template[key].as_str().unwrap_or_default().to_owned();
        let tname = get("Name");
        if i == 0 {
            edits.push(NotetypeEdit::RenameTemplate {
                index: 0,
                name: tname,
            });
        } else {
            edits.push(NotetypeEdit::AddTemplate { name: tname });
        }
        edits.push(NotetypeEdit::SetTemplate {
            index: i,
            template: TemplateDetail {
                ord: None,
                name: String::new(),
                question: get("Front"),
                answer: get("Back"),
                browser_question: String::new(),
                browser_answer: String::new(),
                browser_font_name: String::new(),
                browser_font_size: 0,
                target_deck_id: None,
            },
        });
    }
    if let Some(css) = p["css"].as_str() {
        edits.push(NotetypeEdit::SetCss(css.to_owned()));
    }
    let outcome = manage_notetype(
        backend,
        Edit {
            id,
            edits,
            confirmed: true,
        },
    );
    match outcome {
        Ok(tanki_core::NotetypeResponse::Saved { notetype, .. }) => {
            Ok(json!({"id": notetype.id, "name": notetype.name}))
        }
        other => {
            // Leave nothing half-built behind.
            let _ = manage_notetype(
                backend,
                tanki_core::NotetypeCommand::Delete {
                    id,
                    confirmed: true,
                },
            );
            Err(match other {
                Ok(tanki_core::NotetypeResponse::Rejected(problem)) => {
                    format!("template rejected: {problem:?}")
                }
                Ok(_) => "unexpected core response".into(),
                Err(e) => e,
            })
        }
    }
}

fn model(backend: &dyn ConnectBackend, name: &str) -> std::result::Result<NotetypeInfo, String> {
    let EditResponse::Notetypes(models) = notes(backend, NoteCommand::Notetypes)? else {
        unreachable!()
    };
    models
        .into_iter()
        .find(|m| m.name == name)
        .ok_or_else(|| format!("model was not found: {name}"))
}
fn build_note(
    backend: &dyn ConnectBackend,
    value: &Value,
) -> std::result::Result<(NoteData, i64), String> {
    let model = model(backend, text(value, "modelName")?)?;
    let EditResponse::DeckNames(decks) = notes(
        backend,
        NoteCommand::DeckNames {
            include_filtered: false,
        },
    )?
    else {
        unreachable!()
    };
    let deck = text(value, "deckName")?;
    let deck = decks
        .iter()
        .find(|d| d.name == deck)
        .ok_or_else(|| format!("deck was not found: {deck}"))?;
    let EditResponse::Note(mut note) = notes(
        backend,
        NoteCommand::NewNote {
            notetype_id: model.id,
        },
    )?
    else {
        unreachable!()
    };
    let fields = value["fields"].as_object().ok_or("invalid fields")?;
    for (name, value) in fields {
        let field = model
            .fields
            .iter()
            .find(|f| f.name == *name)
            .ok_or_else(|| format!("field was not found: {name}"))?;
        note.fields[field.ord as usize] = value.as_str().ok_or("invalid field value")?.into();
    }
    note.tags = if value.get("tags").is_some() {
        serde_json::from_value(value["tags"].clone()).map_err(|_| "invalid tags")?
    } else {
        vec![]
    };
    Ok((note, deck.id))
}
fn validate_note(
    backend: &dyn ConnectBackend,
    value: &Value,
    note: &NoteData,
    deck_name: &str,
) -> std::result::Result<(), String> {
    let EditResponse::FieldsCheck(check) =
        notes(backend, NoteCommand::CheckFields { note: note.clone() })?
    else {
        unreachable!()
    };
    if check == FieldsCheck::Empty {
        return Err("cannot create note because it is empty".into());
    }
    if !value["options"]["allowDuplicate"]
        .as_bool()
        .unwrap_or(false)
    {
        let scope = value["options"]["duplicateScope"]
            .as_str()
            .unwrap_or("collection");
        if scope != "collection" && scope != "deck" {
            return Err("invalid duplicateScope".into());
        }
        let opts = &value["options"]["duplicateScopeOptions"];
        let duplicate = connect(
            backend,
            connect::ConnectCommand::Duplicate {
                note: note.clone(),
                deck_name: (scope == "deck")
                    .then(|| opts["deckName"].as_str().unwrap_or(deck_name).to_owned()),
                check_children: opts["checkChildren"].as_bool().unwrap_or(false),
                all_models: opts["checkAllModels"].as_bool().unwrap_or(false),
            },
        )?;
        if duplicate == true {
            return Err("cannot create note because it is a duplicate".into());
        }
    }
    Ok(())
}
fn prepare(
    backend: &dyn ConnectBackend,
    value: &Value,
) -> std::result::Result<(NoteData, i64), String> {
    let (mut note, deck_id) = build_note(backend, value)?;
    let model = model(backend, text(value, "modelName")?)?;
    // Duplicate/empty checks account for attachments without writing media.
    for kind in ["audio", "video", "picture"] {
        if let Some(attachments) = value[kind].as_array() {
            for attachment in attachments {
                let name = text(attachment, "filename")?;
                let markup = if kind == "picture" {
                    format!("<img src=\"{name}\">")
                } else {
                    format!("[sound:{name}]")
                };
                for field in attachment["fields"]
                    .as_array()
                    .ok_or("invalid attachment fields")?
                {
                    let f = model
                        .fields
                        .iter()
                        .find(|f| Some(f.name.as_str()) == field.as_str())
                        .ok_or("invalid attachment field")?;
                    note.fields[f.ord as usize].push_str(&markup);
                }
            }
        }
    }
    validate_note(backend, value, &note, text(value, "deckName")?)?;
    Ok((note, deck_id))
}
fn media(backend: &dyn ConnectBackend, p: &Value) -> std::result::Result<AddedMedia, String> {
    use base64::Engine;
    let command = if let Some(data) = p["data"].as_str() {
        NoteCommand::AddMedia {
            desired_name: text(p, "filename")?.into(),
            data: base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|e| e.to_string())?,
        }
    } else if let Some(path) = p["path"].as_str() {
        NoteCommand::AddMedia {
            desired_name: text(p, "filename")?.into(),
            data: std::fs::read(path).map_err(|e| e.to_string())?,
        }
    } else {
        return Err("media requires data or path; URL downloads are unsupported".into());
    };
    if p["deleteExisting"].as_bool().unwrap_or(true) {
        let filename = text(p, "filename")?;
        if filename.is_empty()
            || filename.contains(['/', '\\'])
            || filename == "."
            || filename == ".."
        {
            return Err("invalid filename".into());
        }
        let path = backend.media_dir().join(filename);
        if path.exists() {
            std::fs::remove_file(path).map_err(|e| e.to_string())?;
        }
    }
    let EditResponse::Media(media) = notes(backend, command)? else {
        unreachable!()
    };
    Ok(media)
}
fn add(backend: &dyn ConnectBackend, value: &Value) -> ApiResult {
    let (mut note, deck_id) = build_note(backend, value)?;
    let model = model(backend, text(value, "modelName")?)?;
    for kind in ["audio", "video", "picture"] {
        if let Some(attachments) = value[kind].as_array() {
            for attachment in attachments {
                let added = media(backend, attachment)?;
                let markup = if kind == "picture" {
                    added.field_text
                } else {
                    format!("[sound:{}]", added.filename)
                };
                for field in attachment["fields"]
                    .as_array()
                    .ok_or("invalid attachment fields")?
                {
                    let field = model
                        .fields
                        .iter()
                        .find(|f| Some(f.name.as_str()) == field.as_str())
                        .ok_or("invalid attachment field")?;
                    note.fields[field.ord as usize].push_str(&markup);
                }
            }
        }
    }
    validate_note(backend, value, &note, text(value, "deckName")?)?;
    let EditResponse::NoteAdded { id, .. } =
        notes(backend, NoteCommand::AddNote { note, deck_id })?
    else {
        unreachable!()
    };
    Ok(json!(id))
}
fn cards_info(backend: &dyn ConnectBackend, ids: Vec<i64>) -> ApiResult {
    let mut result = connect(backend, connect::ConnectCommand::CardsInfo { ids })?;
    for card in result.as_array_mut().ok_or("invalid card response")? {
        let id = card["cardId"].as_i64().ok_or("invalid card id")?;
        let Response::Rendered(rendered) = execute(backend, Command::Render { card_id: id })?
        else {
            unreachable!()
        };
        let EditResponse::Note(note) = notes(
            backend,
            NoteCommand::GetNote {
                id: card["note"].as_i64().unwrap(),
            },
        )?
        else {
            unreachable!()
        };
        let EditResponse::Notetype(model) = notes(
            backend,
            NoteCommand::Notetype {
                id: note.notetype_id,
            },
        )?
        else {
            unreachable!()
        };
        card["modelName"] = json!(model.name);
        card["fields"] = Value::Object(
            model
                .fields
                .iter()
                .map(|f| {
                    (
                        f.name.clone(),
                        json!({"value":note.fields[f.ord as usize],"order":f.ord}),
                    )
                })
                .collect(),
        );
        card["question"] = json!(rendered.question);
        card["answer"] = json!(rendered.answer);
        card["css"] = json!(rendered.css);
    }
    Ok(result)
}
fn action(backend: &dyn ConnectBackend, action: &str, p: &Value) -> ApiResult {
    Ok(match action {
        "addNote" => add(backend, &p["note"])?,
        "addNotes" => json!(p["notes"]
            .as_array()
            .ok_or("invalid notes")?
            .iter()
            .map(|n| add(backend, n).unwrap_or(Value::Null))
            .collect::<Vec<_>>()),
        "canAddNotes" | "canAddNotesWithErrorDetail" => json!(p["notes"]
            .as_array()
            .ok_or("invalid notes")?
            .iter()
            .map(|n| {
                let r = prepare(backend, n);
                if action == "canAddNotes" {
                    json!(r.is_ok())
                } else {
                    match r {
                        Ok(_) => json!({"canAdd":true,"error":null}),
                        Err(e) => json!({"canAdd":false,"error":e}),
                    }
                }
            })
            .collect::<Vec<_>>()),
        "notesInfo" => {
            let mut result = Vec::new();
            for id in ids(p, "notes")? {
                let note = match notes(backend, NoteCommand::GetNote { id }) {
                    Ok(EditResponse::Note(note)) => note,
                    Err(_) => {
                        result.push(Value::Null);
                        continue;
                    }
                    _ => unreachable!(),
                };
                let EditResponse::Notetype(model) = notes(
                    backend,
                    NoteCommand::Notetype {
                        id: note.notetype_id,
                    },
                )?
                else {
                    unreachable!()
                };
                let fields: serde_json::Map<String, Value> = model
                    .fields
                    .iter()
                    .map(|f| {
                        (
                            f.name.clone(),
                            json!({"value":note.fields[f.ord as usize],"order":f.ord}),
                        )
                    })
                    .collect();
                result.push(json!({"noteId":id,"modelName":model.name,"tags":note.tags,"fields":fields,"mod":note.mtime_secs,"cards":connect(backend,connect::ConnectCommand::CardsOfNote {id})?}));
            }
            json!(result)
        }
        "updateNoteFields" | "updateNote" => {
            let value = &p["note"];
            let id = value["id"].as_i64().ok_or("invalid note id")?;
            let EditResponse::Note(note) = notes(backend, NoteCommand::GetNote { id })? else {
                unreachable!()
            };
            let EditResponse::Notetype(model) = notes(
                backend,
                NoteCommand::Notetype {
                    id: note.notetype_id,
                },
            )?
            else {
                unreachable!()
            };
            let mut fields = Vec::new();
            if let Some(values) = value["fields"].as_object() {
                for (name, v) in values {
                    let f = model
                        .fields
                        .iter()
                        .find(|f| f.name == *name)
                        .ok_or("invalid field")?;
                    fields.push((f.ord, v.as_str().ok_or("invalid field value")?.into()));
                }
            }
            let tags = if action == "updateNote" && value.get("tags").is_some() {
                Some(serde_json::from_value(value["tags"].clone()).map_err(|_| "invalid tags")?)
            } else {
                None
            };
            let EditResponse::Updated(report) = notes(
                backend,
                NoteCommand::UpdateNotes {
                    updates: vec![NoteUpdate {
                        id,
                        expected_mtime: note.mtime_secs,
                        expected_fields: Some(note.fields),
                        fields,
                        tags,
                    }],
                },
            )?
            else {
                unreachable!()
            };
            if !report.conflicts.is_empty() || !report.missing.is_empty() {
                return Err("note changed while updating".into());
            }
            Value::Null
        }
        "storeMediaFile" => json!(media(backend, p)?.filename),
        "retrieveMediaFile" => {
            use base64::Engine;
            let name = text(p, "filename")?;
            if std::path::Path::new(name).components().count() != 1
                || name.starts_with('.')
                || name.contains('/')
                || name.contains('\\')
            {
                return Err("invalid filename".into());
            }
            match std::fs::read(backend.media_dir().join(name)) {
                Ok(data) => json!(base64::engine::general_purpose::STANDARD.encode(data)),
                Err(_) => json!(false),
            }
        }
        "apiReflect" => {
            let scopes = p["scopes"].as_array().ok_or("scopes has invalid value")?;
            if scopes.iter().any(|s| s == "actions") {
                let actions = if let Some(requested) = p["actions"].as_array() {
                    requested
                        .iter()
                        .filter(|a| a.as_str().is_some_and(|a| ACTIONS.contains(&a)))
                        .cloned()
                        .collect::<Vec<_>>()
                } else {
                    ACTIONS.iter().map(|a| json!(a)).collect()
                };
                json!({"scopes":["actions"],"actions":actions})
            } else {
                json!({"scopes":[]})
            }
        }
        "modelFieldsOnTemplates" => connect(
            backend,
            connect::ConnectCommand::ModelFieldsOnTemplates {
                id: model(backend, text(p, "modelName")?)?.id,
            },
        )?,
        "getNumCardsReviewedToday" => connect(backend, connect::ConnectCommand::ReviewedToday)?,
        "getMediaFilesNames" => {
            let pattern = glob::Pattern::new(p["pattern"].as_str().unwrap_or("*"))
                .map_err(|e| e.to_string())?;
            let mut names = Vec::new();
            for entry in std::fs::read_dir(backend.media_dir()).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if entry.file_type().map_err(|e| e.to_string())?.is_file() && pattern.matches(&name)
                {
                    names.push(name);
                }
            }
            names.sort();
            json!(names)
        }
        "version" => json!(6),
        "requestPermission" => json!({"permission":"granted","requireApiKey":false,"version":6}),
        "deckNames" | "deckNamesAndIds" => {
            let EditResponse::DeckNames(decks) = notes(
                backend,
                NoteCommand::DeckNames {
                    include_filtered: true,
                },
            )?
            else {
                unreachable!()
            };
            if action == "deckNames" {
                json!(decks.iter().map(|d| &d.name).collect::<Vec<_>>())
            } else {
                Value::Object(decks.into_iter().map(|d| (d.name, json!(d.id))).collect())
            }
        }
        "createDeck" => connect(
            backend,
            connect::ConnectCommand::CreateDeck {
                name: text(p, "deck")?.into(),
            },
        )?,
        "changeDeck" => {
            execute(
                backend,
                Command::Browse(BrowseCommand::SetDeckByName {
                    card_ids: ids(p, "cards")?,
                    name: text(p, "deck")?.into(),
                }),
            )?;
            Value::Null
        }
        "modelNames" | "modelNamesAndIds" | "modelFieldNames" => {
            let EditResponse::Notetypes(models) = notes(backend, NoteCommand::Notetypes)? else {
                unreachable!()
            };
            match action {
                "modelNames" => json!(models.iter().map(|m| &m.name).collect::<Vec<_>>()),
                "modelNamesAndIds" => {
                    Value::Object(models.into_iter().map(|m| (m.name, json!(m.id))).collect())
                }
                _ => {
                    let name = text(p, "modelName")?;
                    let model = models
                        .iter()
                        .find(|m| m.name == name)
                        .ok_or_else(|| format!("model was not found: {name}"))?;
                    json!(model.fields.iter().map(|f| &f.name).collect::<Vec<_>>())
                }
            }
        }
        "createModel" => create_model(backend, p)?,
        "findNotes" | "findCards" | "guiBrowse" => {
            let Response::Edit(EditResponse::Ids(found)) = execute(
                backend,
                Command::Browse(BrowseCommand::Search {
                    query: text(p, "query")?.into(),
                    mode: if action == "findNotes" {
                        BrowseMode::Notes
                    } else {
                        BrowseMode::Cards
                    },
                    order: SearchOrder::None,
                }),
            )?
            else {
                unreachable!()
            };
            json!(found)
        }
        "cardsInfo" => cards_info(backend, ids(p, "cards")?)?,
        "getTags" => {
            let EditResponse::Strings(tags) = notes(backend, NoteCommand::AllTags)? else {
                unreachable!()
            };
            json!(tags)
        }
        "addTags" | "removeTags" => {
            let note_ids = ids(p, "notes")?;
            let tags = text(p, "tags")?.into();
            notes(
                backend,
                if action == "addTags" {
                    NoteCommand::AddTags { note_ids, tags }
                } else {
                    NoteCommand::RemoveTags { note_ids, tags }
                },
            )?;
            Value::Null
        }
        "deleteNotes" => {
            execute(
                backend,
                Command::DeleteNotes {
                    note_ids: ids(p, "notes")?,
                },
            )?;
            Value::Null
        }
        "getMediaDirPath" => json!(backend.media_dir()),
        _ => return Err("unsupported action".into()),
    })
}

/// The HTTP task communicates with a scoped collection dispatcher, so neither
/// the borrowed backend nor collection lifetime escapes shutdown.
pub fn serve(
    backend: &dyn ConnectBackend,
    options: &ServeOptions,
    stop: &AtomicBool,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let listener = std::net::TcpListener::bind(options.bind)?;
    serve_listener(backend, options, listener, stop)
}
pub fn serve_listener(
    backend: &dyn ConnectBackend,
    options: &ServeOptions,
    listener: std::net::TcpListener,
    stop: &AtomicBool,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    use axum::{
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::get,
        Json, Router,
    };
    use std::sync::mpsc;
    type Work = (Value, tokio::sync::oneshot::Sender<Value>);
    #[derive(Clone)]
    struct HttpState {
        tx: mpsc::Sender<Work>,
        options: ServeOptions,
    }
    async fn handle(
        State(state): State<HttpState>,
        headers: HeaderMap,
        body: axum::body::Bytes,
    ) -> (StatusCode, HeaderMap, Json<Value>) {
        let mut out = HeaderMap::new();
        let value: Value = match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(e) => return (StatusCode::OK, out, Json(error(e))),
        };
        if let Some(origin) = headers.get("origin") {
            if !origin.to_str().is_ok_and(|s| allowed(&state.options, s)) {
                if value["action"] == "requestPermission" {
                    out.insert("access-control-allow-origin", origin.clone());
                    let denied = json!({"permission":"denied","requireApiKey":state.options.api_key.is_some(),"version":6,"reason":"Add this origin with --allow-origin or connect.cors_origins"});
                    let reply = if value["version"].as_u64().unwrap_or(4) >= 5 {
                        json!({"result":denied,"error":null})
                    } else {
                        denied
                    };
                    return (StatusCode::OK, out, Json(reply));
                }
                return (
                    StatusCode::FORBIDDEN,
                    out,
                    Json(error("origin is not allowed; use --allow-origin")),
                );
            }
            out.insert("access-control-allow-origin", origin.clone());
            out.insert("vary", "Origin".parse().unwrap());
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        if state.tx.send((value, tx)).is_err() {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                out,
                Json(error("server stopped")),
            );
        }
        let result = rx.await.unwrap_or_else(|_| error("server stopped"));
        (StatusCode::OK, out, Json(result))
    }
    async fn preflight(
        State(state): State<HttpState>,
        headers: HeaderMap,
    ) -> (StatusCode, HeaderMap) {
        let mut out = HeaderMap::new();
        if let Some(origin) = headers.get("origin") {
            if !origin.to_str().is_ok_and(|s| allowed(&state.options, s)) {
                return (StatusCode::FORBIDDEN, out);
            }
            out.insert("access-control-allow-origin", origin.clone());
        }
        out.insert(
            "access-control-allow-methods",
            "GET, POST, OPTIONS".parse().unwrap(),
        );
        out.insert(
            "access-control-allow-headers",
            "Content-Type".parse().unwrap(),
        );
        if headers
            .get("access-control-request-private-network")
            .is_some_and(|v| v == "true")
        {
            out.insert(
                "access-control-allow-private-network",
                "true".parse().unwrap(),
            );
        }
        (StatusCode::NO_CONTENT, out)
    }
    listener.set_nonblocking(true)?;
    if !options.bind.ip().is_loopback() {
        eprintln!(
            "Warning: AnkiConnect is listening on a non-loopback address {}",
            options.bind
        );
    }
    let (tx, rx) = mpsc::channel::<Work>();
    let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    std::thread::scope(|scope| {
        let worker = scope.spawn(move || -> std::io::Result<()> {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            rt.block_on(async move {
                let router = Router::new()
                    .route(
                        "/",
                        get(|| async { "AnkiConnect v.6" })
                            .post(handle)
                            .options(preflight),
                    )
                    .with_state(HttpState {
                        tx,
                        options: options.clone(),
                    });
                let server = axum::serve(tokio::net::TcpListener::from_std(listener)?, router)
                    .with_graceful_shutdown(async { let _ = shutdown_rx.await; });
                tokio::select! {
                    result = async { server.await } => result,
                    _ = async {
                        while !stop.load(Ordering::Relaxed) { tokio::time::sleep(std::time::Duration::from_millis(50)).await; }
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    } => Ok(()),
                }
            })
        });
        while !stop.load(Ordering::Relaxed) {
            match rx.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok((value, reply)) => {
                    let _ = reply.send(request(backend, options, value));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
        }
        let _ = shutdown.send(());
        // Drain requests already accepted while graceful shutdown completes.
        while !worker.is_finished() {
            if let Ok((value, reply)) = rx.recv_timeout(std::time::Duration::from_millis(10)) {
                let _ = reply.send(request(backend, options, value));
            }
        }
        worker
            .join()
            .map_err(|_| std::io::Error::other("HTTP worker panicked"))?
    })?;
    Ok(())
}
