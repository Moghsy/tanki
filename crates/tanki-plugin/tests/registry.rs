use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, Instant},
};
use tanki_plugin::{manifest, registry::Limits, EventKind, Grant, Hook, HookContext, Registry};

const MANIFEST: &str = "name='fake'\nversion='1'\napi=1\ncommand=['python3','-I','fake.py']\n\
hooks=['answer_revealed']\ncapabilities=['hold_review','status']\nfields=['CodeTour']\n\
[[actions]]\nid='open'\ntitle='Open'\n";

/// A scripted plugin. `BODY` runs after the handshake unless the mode skips it.
fn plugin(root: &Path, script: &str) -> BTreeMap<String, Grant> {
    let dir = root.join("fake");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plugin.toml"), MANIFEST).unwrap();
    std::fs::write(dir.join("fake.py"), script).unwrap();
    let digest = manifest::load(&dir).unwrap().digest;
    BTreeMap::from([(
        "fake".to_owned(),
        Grant {
            enabled: true,
            approved_digest: Some(digest),
            settings: serde_json::json!({"k": 1}),
        },
    )])
}

const PRELUDE: &str = "import sys, json\n\
def send(m):\n    print(json.dumps(m)); sys.stdout.flush()\n\
hello = json.loads(sys.stdin.readline())\n";

fn ctx() -> HookContext {
    HookContext {
        card_id: 7,
        note_id: 8,
        deck: "Default".into(),
        fields: BTreeMap::from([
            ("CodeTour".into(), "code-tour://saved/x".into()),
            ("Secret".into(), "must not leak".into()),
        ]),
    }
}

fn pump(
    reg: &mut Registry,
    want: impl Fn(&[tanki_plugin::Event]) -> bool,
) -> Vec<tanki_plugin::Event> {
    let mut all = Vec::new();
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        all.extend(reg.poll(Instant::now()));
        if want(&all) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    all
}

fn limits() -> Limits {
    Limits {
        handshake: Duration::from_millis(300),
    }
}

#[test]
fn hook_round_trip_filters_fields_and_passes_settings() {
    let dir = tempfile::tempdir().unwrap();
    let script = format!(
        "{PRELUDE}{}",
        r#"send({'type':'ready','api':1})
for line in sys.stdin:
    m = json.loads(line)
    if m['type'] == 'hook':
        send({'type':'status','text':json.dumps([hello['settings'], m['fields']], sort_keys=True)})
        send({'type':'hold_review','reason':'open'})
"#
    );
    let grants = plugin(dir.path(), &script);
    let (mut reg, refused) = Registry::discover(dir.path(), &grants, limits());
    assert!(refused.is_empty(), "{refused:?}");
    assert!(reg
        .dispatch(Hook::AnswerRevealed, &ctx(), Instant::now())
        .is_empty());
    let events = pump(&mut reg, |e| e.len() >= 2);
    assert_eq!(
        events[0].kind,
        EventKind::Status(r#"[{"k": 1}, {"CodeTour": "code-tour://saved/x"}]"#.into())
    );
    assert_eq!(events[1].kind, EventKind::HoldReview("open".into()));
    reg.shutdown();
}

#[test]
fn disabled_plugin_is_never_started() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("started");
    let script = format!("open({:?}, 'w').close()\n", marker.to_str().unwrap());
    let mut grants = plugin(dir.path(), &script);
    grants.get_mut("fake").unwrap().enabled = false;
    let (mut reg, refused) = Registry::discover(dir.path(), &grants, limits());
    assert!(refused.is_empty() && reg.names().is_empty());
    reg.dispatch(Hook::AnswerRevealed, &ctx(), Instant::now());
    std::thread::sleep(Duration::from_millis(200));
    assert!(!marker.exists());
}

#[test]
fn unapproved_or_changed_manifest_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut grants = plugin(dir.path(), "");
    grants.get_mut("fake").unwrap().approved_digest = None;
    let (reg, refused) = Registry::discover(dir.path(), &grants, limits());
    assert!(reg.names().is_empty());
    assert!(matches!(&refused[0].kind, EventKind::Refused(m) if m.contains("manifest_sha256")));
    grants.get_mut("fake").unwrap().approved_digest = Some("0".repeat(64));
    let (reg, refused) = Registry::discover(dir.path(), &grants, limits());
    assert!(reg.names().is_empty() && refused.len() == 1);
}

fn dropped(events: &[tanki_plugin::Event]) -> Option<&EventKind> {
    events
        .iter()
        .map(|e| &e.kind)
        .find(|k| matches!(k, EventKind::Dropped { .. }))
}

fn run_bad(script: String, needle: &str) {
    let dir = tempfile::tempdir().unwrap();
    let grants = plugin(dir.path(), &script);
    let (mut reg, _) = Registry::discover(dir.path(), &grants, limits());
    let mut events = reg.dispatch(Hook::AnswerRevealed, &ctx(), Instant::now());
    events.extend(pump(&mut reg, |e| dropped(e).is_some()));
    match dropped(&events) {
        Some(EventKind::Dropped { reason, permanent }) => {
            assert!(reason.contains(needle), "{reason}");
            assert!(!permanent);
        }
        other => panic!("not dropped: {other:?} {events:?}"),
    }
}

#[test]
fn silent_plugin_misses_the_handshake() {
    run_bad(
        format!("{PRELUDE}import time\ntime.sleep(30)\n"),
        "handshake",
    );
}
#[test]
fn garbage_output_drops_the_plugin() {
    run_bad(
        format!("{PRELUDE}print('not json')\nsys.stdout.flush()\nimport time\ntime.sleep(30)\n"),
        "malformed",
    );
}
#[test]
fn unknown_message_drops_the_plugin() {
    run_bad(
        format!("{PRELUDE}send({{'type':'ready','api':1}})\nsend({{'type':'exec','cmd':'id'}})\nimport time\ntime.sleep(30)\n"),
        "malformed",
    );
}
#[test]
fn oversize_line_drops_the_plugin() {
    run_bad(
        format!("{PRELUDE}sys.stdout.write('x'*300000+'\\n')\nsys.stdout.flush()\nimport time\ntime.sleep(30)\n"),
        "too long",
    );
}
#[test]
fn ungranted_capability_drops_the_plugin() {
    run_bad(
        format!("{PRELUDE}send({{'type':'ready','api':1}})\nsend({{'type':'notify','level':'info','text':'x'}})\nimport time\ntime.sleep(30)\n"),
        "ungranted",
    );
}
#[test]
fn message_before_ready_drops_the_plugin() {
    run_bad(
        format!("{PRELUDE}send({{'type':'status','text':'x'}})\nimport time\ntime.sleep(30)\n"),
        "before ready",
    );
}

#[test]
fn crash_releases_a_hold_and_restarts_are_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let script = format!(
        "{PRELUDE}send({{'type':'ready','api':1}})\nsend({{'type':'hold_review','reason':'r'}})\nsys.stderr.write('boom')\nsys.exit(3)\n"
    );
    let grants = plugin(dir.path(), &script);
    let (mut reg, _) = Registry::discover(dir.path(), &grants, limits());
    let mut permanent = false;
    for round in 0..6 {
        let mut events = reg.dispatch(Hook::AnswerRevealed, &ctx(), Instant::now());
        events.extend(pump(&mut reg, |e| dropped(e).is_some()));
        if !reg.names().is_empty() || round == 3 {
            // Each failed run reports the release before the drop.
            let hold = events
                .iter()
                .position(|e| matches!(e.kind, EventKind::HoldReview(_)));
            let release = events
                .iter()
                .position(|e| e.kind == EventKind::ReleaseReview);
            assert!(hold.is_some() && release > hold, "{events:?}");
        }
        if let Some(EventKind::Dropped {
            permanent: p,
            reason,
        }) = dropped(&events)
        {
            assert!(
                reason.contains("boom") || reason.contains("exited"),
                "{reason}"
            );
            permanent = *p;
        }
        if permanent {
            break;
        }
    }
    assert!(permanent && reg.names().is_empty());
    assert!(reg
        .dispatch(Hook::AnswerRevealed, &ctx(), Instant::now())
        .is_empty());
}

#[test]
fn actions_must_be_declared() {
    let dir = tempfile::tempdir().unwrap();
    let script = format!(
        "{PRELUDE}send({{'type':'ready','api':1}})\nfor line in sys.stdin:\n    m=json.loads(line)\n    if m['type']=='action': send({{'type':'status','text':m['id']}})\n"
    );
    let grants = plugin(dir.path(), &script);
    let (mut reg, _) = Registry::discover(dir.path(), &grants, limits());
    assert!(reg
        .action("fake", "undeclared", 1, Instant::now())
        .is_empty());
    reg.action("fake", "open", 1, Instant::now());
    let events = pump(&mut reg, |e| !e.is_empty());
    assert_eq!(events[0].kind, EventKind::Status("open".into()));
    reg.shutdown();
}
