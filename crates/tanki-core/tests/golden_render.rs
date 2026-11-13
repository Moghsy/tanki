//! Golden render test: tanki-core's card pipeline against Anki's own Python
//! package at the pinned version.
//!
//! `tests/fixtures/golden/render.json` holds pylib's `card.render_output()`
//! for every card of the synthetic fixture (`src/fixture.rs`). Regenerate it
//! with the command at the top of `scripts/golden-render.py` after changing
//! the fixture or bumping Anki. These tests need no Python.
//!
//! Card keys are `<note type>|<template ordinal>|<label>`; the label is the
//! note's `golden::<label>` tag. Keys are mapped to card ids through
//! `Command::Search`, so each key must match exactly one card.
use anki::{
    collection::{Collection, CollectionBuilder},
    services::{CardRenderingService, ConfigService, MediaService},
};
use anki_proto as pb;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tanki_core::*;

const GOLDEN: &str = include_str!("../../../tests/fixtures/golden/render.json");
const PINNED_VERSION: &str = include_str!("../../../vendor/anki/.version");
const FIELDS: [&str; 5] = [
    "question",
    "answer",
    "css",
    "question_av_tags",
    "answer_av_tags",
];

fn golden() -> BTreeMap<String, Value> {
    let doc: Value = serde_json::from_str(GOLDEN).expect("golden JSON");
    serde_json::from_value(doc["cards"].clone()).expect("golden cards")
}

fn key_parts(key: &str) -> (&str, u32, &str) {
    let mut parts = key.rsplitn(3, '|');
    let label = parts.next().expect("label");
    let ord = parts.next().expect("ordinal").parse().expect("ordinal");
    let notetype = parts.next().expect("note type");
    (notetype, ord, label)
}

/// Generates the fixture in `dir` and maps every golden key to its card id,
/// checking that the golden file and the fixture cover the same cards.
/// The actor is returned open.
fn fixture_with_keys(dir: &Path) -> Result<(CollectionActor, BTreeMap<String, i64>)> {
    let actor = CollectionActor::open(dir)?;
    let Response::CardIds(all) = actor.execute(Command::GenerateFixture)? else {
        panic!("GenerateFixture did not return card ids")
    };
    let mut keys = BTreeMap::new();
    for key in golden().keys() {
        let (notetype, ord, label) = key_parts(key);
        let query = format!("\"note:{notetype}\" card:{} tag:golden::{label}", ord + 1);
        let Response::CardIds(ids) = actor.execute(Command::Search { query })? else {
            panic!("Search did not return card ids")
        };
        assert_eq!(ids.len(), 1, "golden card {key} matches cards {ids:?}");
        keys.insert(key.clone(), ids[0]);
    }
    let mapped: BTreeSet<i64> = keys.values().copied().collect();
    let all: BTreeSet<i64> = all.into_iter().collect();
    assert_eq!(mapped.len(), keys.len(), "two golden keys share a card");
    assert_eq!(
        mapped,
        all,
        "fixture cards without a golden entry: {:?}; regenerate the golden files",
        all.difference(&mapped).collect::<Vec<_>>()
    );
    Ok((actor, keys))
}

fn audio_json(tags: &[AudioTag]) -> Value {
    tags.iter()
        .map(|tag| match tag {
            AudioTag::SoundOrVideo(name) => json!({ "sound_or_video": name }),
            AudioTag::Tts {
                text,
                lang,
                voices,
                speed,
                other_args,
            } => json!({ "tts": {
                "field_text": text,
                "lang": lang,
                "voices": voices,
                // pylib reads the proto f32 into a Python float.
                "speed": f64::from(*speed),
                "other_args": other_args,
            }}),
        })
        .collect()
}

fn card_json(card: &RenderedCard) -> Value {
    json!({
        "question": card.question,
        "answer": card.answer,
        "css": card.css,
        "question_av_tags": audio_json(&card.question_audio),
        "answer_av_tags": audio_json(&card.answer_audio),
    })
}

/// Returns `key: field` for every field that differs from the golden file.
fn differences(rendered: &BTreeMap<String, Value>) -> Vec<String> {
    let golden = golden();
    let mut out = vec![];
    for (key, expected) in &golden {
        let actual = &rendered[key];
        for field in FIELDS {
            if actual[field] != expected[field] {
                out.push(format!(
                    "{key}: {field}\n  python: {}\n  tanki:  {}",
                    expected[field], actual[field]
                ));
            }
        }
    }
    out
}

#[test]
fn golden_file_is_for_the_pinned_anki() {
    let doc: Value = serde_json::from_str(GOLDEN).unwrap();
    assert_eq!(
        doc["anki_version"].as_str(),
        Some(PINNED_VERSION.trim()),
        "vendor/anki was bumped; regenerate tests/fixtures/golden (scripts/golden-render.py)"
    );
}

#[test]
fn renders_match_python_golden() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (actor, keys) = fixture_with_keys(dir.path())?;
    let mut rendered = BTreeMap::new();
    for (key, card_id) in keys {
        let Response::Rendered(card) = actor.execute(Command::Render { card_id })? else {
            panic!("Render did not return a card")
        };
        rendered.insert(key, card_json(&card));
    }
    actor.close()?;
    let diffs = differences(&rendered);
    assert!(
        diffs.is_empty(),
        "{} differences from Anki's Python output:\n{}",
        diffs.len(),
        diffs.join("\n")
    );
    Ok(())
}

/// Mirror of `render` in `src/actor.rs` with `partial_render` as a parameter.
/// Run with `true` it must reproduce the golden file exactly, which keeps the
/// mirror honest; the `false` run then differs only through that flag.
fn render_with(col: &mut Collection, card_id: i64, partial_render: bool) -> RenderedCard {
    use pb::card_rendering::rendered_template_node::Value as Node;
    let r = match CardRenderingService::render_existing_card(
        col,
        pb::card_rendering::RenderExistingCardRequest {
            card_id,
            browser: false,
            partial_render,
        },
    ) {
        Ok(r) => r,
        Err(e @ anki::error::AnkiError::TemplateError { .. }) => {
            let text = e.message(col.tr());
            return RenderedCard {
                question: text.clone(),
                answer: text,
                css: String::new(),
                question_audio: vec![],
                answer_audio: vec![],
            };
        }
        Err(e) => panic!("render {card_id}: {e}"),
    };
    let text = |nodes: Vec<pb::card_rendering::RenderedTemplateNode>, front: Option<&str>| {
        nodes
            .into_iter()
            .map(|n| match n.value {
                Some(Node::Text(s)) => s,
                Some(Node::Replacement(r)) if r.field_name == "FrontSide" => {
                    front.map_or(r.current_text, str::to_owned)
                }
                Some(Node::Replacement(r)) => r.current_text,
                None => String::new(),
            })
            .collect::<String>()
    };
    let q = CardRenderingService::extract_av_tags(
        col,
        pb::card_rendering::ExtractAvTagsRequest {
            text: text(r.question_nodes, None),
            question_side: true,
        },
    )
    .unwrap();
    let a = CardRenderingService::extract_av_tags(
        col,
        pb::card_rendering::ExtractAvTagsRequest {
            text: text(r.answer_nodes, Some(&q.text)),
            question_side: false,
        },
    )
    .unwrap();
    let render_latex = ConfigService::get_config_bool(
        col,
        pb::config::GetConfigBoolRequest {
            key: pb::config::config_key::Bool::RenderLatex as i32,
        },
    )
    .unwrap()
    .val;
    let mut latex = |html: String| {
        let out = CardRenderingService::extract_latex(
            col,
            pb::card_rendering::ExtractLatexRequest {
                text: html,
                svg: r.latex_svg,
                expand_clozes: false,
            },
        )
        .unwrap();
        let mut html = out.text;
        for image in out.latex {
            let path = MediaService::get_absolute_media_path(col, image.filename.into())
                .unwrap()
                .val;
            if !Path::new(&path).exists() && !render_latex {
                html.push_str(&col.tr().preferences_latex_generation_disabled());
                break;
            }
        }
        html
    };
    let question = latex(q.text);
    let answer = latex(a.text);
    let audio = |tags: Vec<pb::card_rendering::AvTag>| {
        use pb::card_rendering::av_tag::Value as Tag;
        tags.into_iter()
            .filter_map(|t| t.value)
            .map(|v| match v {
                Tag::SoundOrVideo(s) => {
                    AudioTag::SoundOrVideo(s.rsplit('/').next().unwrap_or_default().to_owned())
                }
                Tag::Tts(t) => AudioTag::Tts {
                    text: t.field_text,
                    lang: t.lang,
                    voices: t.voices,
                    speed: t.speed,
                    other_args: t.other_args,
                },
            })
            .collect()
    };
    RenderedCard {
        question,
        answer,
        css: r.css,
        question_audio: audio(q.av_tags),
        answer_audio: audio(a.av_tags),
    }
}

/// Settles `partial_render` (PLAN 6.0 step 1; R3 says true, R2 said false).
/// pylib calls `render_existing_card(partial_render=True)`; with `false` the
/// answer's `{{FrontSide}}` holds the question before AV extraction, so the
/// question's sounds and TTS are extracted again as answer audio, and Rust
/// skips unknown filters and applies the built-in filters after them, which
/// pylib never does.
#[test]
fn partial_render_must_be_true() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (actor, keys) = fixture_with_keys(dir.path())?;
    // The actor must be closed before anything else opens the collection.
    actor.close()?;
    let profile = dir.path();
    let mut col = CollectionBuilder::new(profile.join("collection.anki2"))
        .set_media_paths(
            profile.join("collection.media"),
            profile.join("collection.media.db2"),
        )
        .build()
        .expect("open fixture");
    let mut run = |partial| -> BTreeMap<String, Value> {
        keys.iter()
            .map(|(key, &id)| (key.clone(), card_json(&render_with(&mut col, id, partial))))
            .collect()
    };
    let with_true = run(true);
    let with_false = run(false);
    col.close(None).expect("close fixture");

    let diffs = differences(&with_true);
    assert!(diffs.is_empty(), "mirror drifted:\n{}", diffs.join("\n"));

    let differing: BTreeSet<String> = differences(&with_false)
        .iter()
        .map(|d| d.lines().next().unwrap().to_owned())
        .collect();
    let expected: BTreeSet<String> = [
        // {{FrontSide}} with [sound:] on the front: the answer re-extracts the
        // question's sound ("[anki:play:a:0]" in the text, an extra tag).
        "Basic (and reversed card)|0|stock: answer",
        "Basic (and reversed card)|0|stock: answer_av_tags",
        "Basic (and reversed card)|1|stock: answer",
        "Basic (and reversed card)|1|stock: answer_av_tags",
        "Basic (optional reversed card)|0|stock: answer",
        "Basic (optional reversed card)|0|stock: answer_av_tags",
        "Basic (optional reversed card)|1|stock: answer",
        "Basic (optional reversed card)|1|stock: answer_av_tags",
        "Basic|0|stock: answer",
        "Basic|0|stock: answer_av_tags",
        "tanki Golden|0|full: answer",
        "tanki Golden|0|full: answer_av_tags",
        "tanki Golden|4|full: answer",
        "tanki Golden|4|full: answer_av_tags",
        // TTS on the front: replayed as answer audio as well.
        "tanki Golden|1|sparse: answer",
        "tanki Golden|1|sparse: answer_av_tags",
        // Both of the above, plus {{text:somefilter:Extra}}: pylib keeps the
        // HTML (Rust stops at the unknown filter); `false` strips it.
        "tanki Golden|1|full: question",
        "tanki Golden|1|full: answer",
        "tanki Golden|1|full: answer_av_tags",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(differing, expected);

    // The concrete double-audio symptom on the plainest card.
    assert_eq!(
        with_false["Basic|0|stock"]["answer_av_tags"],
        json!([{ "sound_or_video": "fixture.wav" }, { "sound_or_video": "fixture.wav" }])
    );
    assert_eq!(
        with_true["Basic|0|stock"]["answer_av_tags"],
        json!([{ "sound_or_video": "fixture.wav" }])
    );
    Ok(())
}
