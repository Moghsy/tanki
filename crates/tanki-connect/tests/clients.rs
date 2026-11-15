//! Request data derived from Yomitan's ext/js/comm/anki-connect.js and
//! Obsidian_to_Anki's obsidian_to_anki.py (URLs in docs/implementation/phase-6-interfaces.md).
//! These replay client protocols over HTTP, without copying client code.
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tanki_connect::{serve_listener, ServeOptions};
use tanki_core::{CollectionActor, Command};
fn http(addr: SocketAddr, method: &str, origin: Option<&str>, body: &str) -> (String, String) {
    let mut socket = TcpStream::connect(addr).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let origin = origin
        .map(|o| format!("Origin: {o}\r\n"))
        .unwrap_or_default();
    write!(socket,"{method} / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\n{origin}Content-Length: {}\r\n\r\n{body}",body.len()).unwrap();
    let mut output = String::new();
    socket.read_to_string(&mut output).unwrap();
    let (head, body) = output.split_once("\r\n\r\n").unwrap();
    (head.into(), body.into())
}
fn call(addr: SocketAddr, value: Value) -> Value {
    let (head, body) = http(addr, "POST", None, &value.to_string());
    assert!(head.contains("200 OK"), "{head}: {body}");
    serde_json::from_str(&body).unwrap()
}
fn v6(action: &str, params: Value) -> Value {
    json!({"action":action,"params":params,"version":6})
}
fn result(addr: SocketAddr, action: &str, params: Value) -> Value {
    let response = call(addr, v6(action, params));
    assert!(response["error"].is_null(), "{response}");
    response["result"].clone()
}
fn with_server(test: impl FnOnce(SocketAddr, &CollectionActor)) {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let actor = CollectionActor::open(dir.path()).unwrap();
    actor.execute(Command::GenerateFixture).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            serve_listener(
                &actor,
                &ServeOptions {
                    bind: addr,
                    ..Default::default()
                },
                listener,
                &stop,
            )
            .map_err(|e| e.to_string())
        });
        let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| test(addr, &actor)));
        stop.store(true, Ordering::Relaxed);
        worker.join().unwrap().unwrap();
        if let Err(error) = checked {
            std::panic::resume_unwind(error);
        }
    });
    actor.close().unwrap();
}
fn note(front: &str) -> Value {
    json!({"deckName":"Default","modelName":"Basic","fields":{"Front":front,"Back":"answer"},"tags":["mined"],"options":{"allowDuplicate":false,"duplicateScope":"deck"}})
}
#[test]
fn yomitan_version_two_mining_and_obsidian_version_six_nested_batches() {
    with_server(|addr, _| {
        // Yomitan: bare results, capability discovery, duplicate checks and mining.
        let yomitan = |action: &str, params: Value| {
            call(addr, json!({"action":action,"params":params,"version":2}))
        };
        assert_eq!(yomitan("version", json!({})), 6);
        assert_eq!(yomitan("missing", json!({}))["error"], "unsupported action");
        assert!(yomitan("deckNames", json!({}))
            .as_array()
            .unwrap()
            .contains(&json!("Default")));
        assert!(yomitan("modelNames", json!({}))
            .as_array()
            .unwrap()
            .contains(&json!("Basic")));
        assert_eq!(
            yomitan("modelFieldNames", json!({"modelName":"Basic"})),
            json!(["Front", "Back"])
        );
        let reflected = yomitan(
            "apiReflect",
            json!({"scopes":["actions"],"actions":["canAddNotesWithErrorDetail","missing"]}),
        );
        assert_eq!(reflected["actions"], json!(["canAddNotesWithErrorDetail"]));
        assert_eq!(
            yomitan(
                "canAddNotesWithErrorDetail",
                json!({"notes":[note("mining")]})
            ),
            json!([{"canAdd":true,"error":null}])
        );
        let id = yomitan("addNote", json!({"note":note("mining")}));
        assert!(id.is_i64());
        let duplicates = yomitan(
            "multi",
            json!({"actions":[{"action":"findNotes","params":{"query":"Front:mining"}},{"action":"notesInfo","params":{"notes":[id]}}]}),
        );
        assert_eq!(duplicates[0], json!([id]));
        assert_eq!(duplicates[1][0]["fields"]["Front"]["value"], "mining");
        // Obsidian: model discovery with nested v6 envelopes, media and nested add.
        let fields = result(
            addr,
            "multi",
            json!({"actions":[v6("modelFieldNames",json!({"modelName":"Basic"}))]}),
        );
        assert_eq!(fields[0]["result"], json!(["Front", "Back"]));
        assert_eq!(
            result(
                addr,
                "storeMediaFile",
                json!({"filename":"obsidian.txt","data":"aGVsbG8="})
            ),
            "obsidian.txt"
        );
        let added = result(
            addr,
            "multi",
            json!({"actions":[v6("multi",json!({"actions":[v6("addNotes",json!({"notes":[note("obsidian")]}))]}))]}),
        );
        let new_id = added[0]["result"][0]["result"][0].clone();
        assert!(new_id.is_i64(), "{added}");
        result(
            addr,
            "updateNoteFields",
            json!({"note":{"id":new_id,"fields":{"Back":"edited"}}}),
        );
        let info = result(addr, "notesInfo", json!({"notes":[new_id]}));
        assert_eq!(info[0]["fields"]["Back"]["value"], "edited");
        let cards = info[0]["cards"].clone();
        result(addr, "createDeck", json!({"deck":"Imported"}));
        result(addr, "changeDeck", json!({"cards":cards,"deck":"Imported"}));
        result(addr, "removeTags", json!({"notes":[new_id],"tags":"mined"}));
        result(
            addr,
            "addTags",
            json!({"notes":[new_id],"tags":"Obsidian_to_Anki"}),
        );
        let info = result(addr, "cardsInfo", json!({"cards":cards}));
        assert_eq!(info[0]["deckName"], "Imported");
        assert!(info[0]["answer"].as_str().unwrap().contains("edited"));
        result(addr, "deleteNotes", json!({"notes":[new_id]}));
        assert_eq!(
            result(addr, "notesInfo", json!({"notes":[new_id]})),
            json!([null])
        );
    });
}
#[test]
fn scoped_duplicates_media_attachments_and_error_contracts() {
    with_server(|addr, _| {
        let id = result(addr, "addNote", json!({"note":note("same")}));
        assert_eq!(
            call(addr, v6("addNote", json!({"note":note("same")})))["error"],
            "cannot create note because it is a duplicate"
        );
        result(addr, "createDeck", json!({"deck":"Other"}));
        let mut other = note("same");
        other["deckName"] = json!("Other");
        result(addr, "addNote", json!({"note":other}));
        other["options"]["duplicateScopeOptions"] = json!({"deckName":"Default"});
        assert_eq!(
            result(addr, "canAddNotes", json!({"notes":[other]})),
            json!([false])
        );
        assert_eq!(
            call(addr, v6("addNote", json!({"note":note("")})))["error"],
            "cannot create note because it is empty"
        );
        let mut attached = note("picture");
        attached["picture"] = json!([{"filename":"image.png","data":"aGVsbG8=","fields":["Back"]}]);
        let image_id = result(addr, "addNote", json!({"note":attached}));
        let mut image_only = note("");
        image_only["picture"] =
            json!([{"filename":"only.png","data":"aGVsbG8=","fields":["Front"]}]);
        assert_eq!(
            result(addr, "canAddNotes", json!({"notes":[image_only]})),
            json!([true])
        );
        result(addr, "addNote", json!({"note":image_only}));
        let info = result(addr, "notesInfo", json!({"notes":[image_id]}));
        assert!(info[0]["fields"]["Back"]["value"]
            .as_str()
            .unwrap()
            .contains("image.png"));
        assert_eq!(
            result(addr, "retrieveMediaFile", json!({"filename":"image.png"})),
            "aGVsbG8="
        );
        assert_eq!(
            result(addr, "getMediaFilesNames", json!({"pattern":"image*.png"})),
            json!(["image.png"])
        );
        assert!(call(
            addr,
            v6(
                "retrieveMediaFile",
                json!({"filename":"../collection.anki2"})
            )
        )["error"]
            .is_string());
        result(addr, "createDeck", json!({"deck":"Parent::Child"}));
        let mut child = note("child-only");
        child["deckName"] = json!("Parent::Child");
        result(addr, "addNote", json!({"note":child}));
        child["deckName"] = json!("Parent");
        child["options"]["duplicateScopeOptions"] = json!({"checkChildren":false});
        assert_eq!(
            result(addr, "canAddNotes", json!({"notes":[child]})),
            json!([true])
        );
        child["options"]["duplicateScopeOptions"]["checkChildren"] = json!(true);
        assert_eq!(
            result(addr, "canAddNotes", json!({"notes":[child]})),
            json!([false])
        );
        let mut cross_model = note("same");
        cross_model["modelName"] = json!("Basic (and reversed card)");
        cross_model["options"]["duplicateScope"] = json!("collection");
        assert_eq!(
            result(addr, "canAddNotes", json!({"notes":[cross_model]})),
            json!([true])
        );
        cross_model["options"]["duplicateScopeOptions"] = json!({"checkAllModels":true});
        assert_eq!(
            result(addr, "canAddNotes", json!({"notes":[cross_model]})),
            json!([false])
        );
        let renamed = result(
            addr,
            "storeMediaFile",
            json!({"filename":"image.png","data":"d29ybGQ=","deleteExisting":false}),
        );
        assert_ne!(renamed, "image.png");
        assert_eq!(
            result(addr, "retrieveMediaFile", json!({"filename":"image.png"})),
            "aGVsbG8="
        );
        result(
            addr,
            "storeMediaFile",
            json!({"filename":"image.png","data":"d29ybGQ=","deleteExisting":true}),
        );
        assert_eq!(
            result(addr, "retrieveMediaFile", json!({"filename":"image.png"})),
            "d29ybGQ="
        );
        let local = tempfile::NamedTempFile::new_in("/tmp").unwrap();
        std::fs::write(local.path(), b"local").unwrap();
        result(
            addr,
            "storeMediaFile",
            json!({"filename":"local.txt","path":local.path()}),
        );
        assert_eq!(
            result(addr, "retrieveMediaFile", json!({"filename":"local.txt"})),
            "bG9jYWw="
        );
        assert_eq!(result(addr, "getNumCardsReviewedToday", json!({})), 0);
        let templates = result(addr, "modelFieldsOnTemplates", json!({"modelName":"Basic"}));
        assert_eq!(templates["Card 1"], json!([["Front"], ["Back"]]));
        assert_eq!(
            call(addr, v6("missing", json!({})))["error"],
            "unsupported action"
        );
        assert_eq!(
            result(addr, "findNotes", json!({"query":format!("nid:{id}")})),
            json!([id])
        );
    });
}
#[test]
fn auth_cors_preflight_and_shutdown() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let actor = CollectionActor::open(dir.path()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = AtomicBool::new(false);
    let options = ServeOptions {
        bind: addr,
        api_key: Some("secret".into()),
        cors_origins: vec!["https://yomitan.test".into()],
    };
    std::thread::scope(|scope| {
        let worker = scope
            .spawn(|| serve_listener(&actor, &options, listener, &stop).map_err(|e| e.to_string()));
        let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert!(call(addr, v6("version", json!({})))["error"].is_string());
            let mut valid = v6("version", json!({}));
            valid["key"] = json!("secret");
            assert_eq!(call(addr, valid.clone())["result"], 6);
            assert_eq!(
                call(addr, v6("requestPermission", json!({})))["result"]["requireApiKey"],
                true
            );
            assert!(
                http(addr, "POST", Some("https://evil.test"), &valid.to_string())
                    .0
                    .contains("403")
            );
            assert!(http(
                addr,
                "POST",
                Some("https://yomitan.test"),
                &valid.to_string()
            )
            .0
            .to_lowercase()
            .contains("access-control-allow-origin: https://yomitan.test"));
            assert!(http(addr, "OPTIONS", Some("http://localhost"), "")
                .0
                .contains("204"));
            assert!(http(addr, "GET", None, "").1.contains("AnkiConnect"));
        }));
        stop.store(true, Ordering::Relaxed);
        worker.join().unwrap().unwrap();
        if let Err(error) = checked {
            std::panic::resume_unwind(error);
        }
    });
    actor.close().unwrap();
}
