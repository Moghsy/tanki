//! Acceptance check against the real `tanki-code-tour` plugin (separate repo).
//! Ignored by default; run with
//! `TANKI_CODE_TOUR_REPO=/path/to/tanki-code-tour` after building its binary.
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use tanki_plugin::{manifest, registry::Limits, EventKind, Grant, Hook, HookContext, Registry};

const ID: &str = "0123456789abcdef0123456789abcdef";

#[test]
#[ignore = "needs the tanki-code-tour repository and a built binary"]
fn real_plugin_opens_a_tour_holds_review_and_is_released() {
    let repo = std::path::PathBuf::from(std::env::var("TANKI_CODE_TOUR_REPO").unwrap());
    let binary = std::env::var("TANKI_CODE_TOUR_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| repo.join("target/debug/tanki-code-tour"));
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("plugins/code-tour");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(repo.join("plugin.toml"), dir.join("plugin.toml")).unwrap();
    std::fs::copy(&binary, dir.join("tanki-code-tour")).unwrap();

    // A stand-in code-tour CLI, so no editor or Claude session starts.
    let fake = root.path().join("fake.py");
    let log = root.path().join("calls.log");
    std::fs::write(
        &fake,
        format!(
            r#"import sys, json
a = sys.argv[1:]
open({log:?}, 'a').write(json.dumps(a) + '\n')
if a == ['capabilities']:
    print(json.dumps(dict(protocol=1, standalone=True, inspect=True, no_focus=True, lazy_agent=True, new_editor=True)))
elif a == ['--list']:
    print(json.dumps([dict(address='/tmp/nvim.sock', instance_id='one', cwd='/tmp')]))
elif a[:2] == ['saved', 'open']:
    print(json.dumps(dict(id=a[2], opened=2, title='Demo tour', issues=[], counts=dict(ready=2))))
"#,
            log = log.to_str().unwrap()
        ),
    )
    .unwrap();

    let digest = manifest::load(&dir).unwrap().digest;
    let grants = BTreeMap::from([(
        "code-tour".to_owned(),
        Grant {
            enabled: true,
            approved_digest: Some(digest),
            settings: serde_json::json!({
                "command": ["python3", fake.to_str().unwrap()],
                "auto_open": true,
            }),
        },
    )]);
    let (mut reg, refused) = Registry::discover(
        &root.path().join("plugins"),
        &grants,
        Limits {
            handshake: Duration::from_secs(5),
        },
    );
    assert!(refused.is_empty(), "{refused:?}");

    let ctx = HookContext {
        card_id: 1,
        note_id: 2,
        deck_id: 3,
        fields: BTreeMap::from([
            ("CodeTour".into(), format!("code-tour://saved/{ID}")),
            ("Secret".into(), "not declared, never sent".into()),
        ]),
    };
    let mut events = reg.dispatch(Hook::AnswerRevealed, &ctx, Instant::now());
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end && events.len() < 2 {
        events.extend(reg.poll(Instant::now()));
        std::thread::sleep(Duration::from_millis(20));
    }
    let kinds: Vec<_> = events.iter().map(|e| e.kind.clone()).collect();
    assert!(matches!(&kinds[0], EventKind::HoldReview(_)), "{kinds:?}");
    match &kinds[1] {
        EventKind::Status(text) => assert!(text.starts_with("Demo tour"), "{text}"),
        other => panic!("{other:?}"),
    }
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        calls.contains(&format!("\"saved\", \"open\", \"{ID}\"")),
        "{calls}"
    );
    assert!(!calls.contains("not declared"));
    reg.shutdown();
}
