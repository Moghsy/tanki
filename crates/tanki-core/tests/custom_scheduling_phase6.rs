//! End-to-end transforms: labels and cached answer states use the same result.
use tanki_core::deck_options::{model::DeckOptionsState, *};
use tanki_core::*;

fn options(actor: &CollectionActor, command: DeckOptionsCommand) -> DeckOptionsResponse {
    match actor
        .execute(Command::Manage(ManageCommand::DeckOptions(command)))
        .unwrap()
    {
        Response::Manage(ManageResponse::DeckOptions(response)) => response,
        other => panic!("{other:?}"),
    }
}
fn script(actor: &CollectionActor, deck_id: i64, script: &str) {
    let DeckOptionsResponse::Loaded(data) = options(actor, DeckOptionsCommand::Load { deck_id })
    else {
        panic!()
    };
    let mut state = DeckOptionsState::new(*data);
    state.set_custom_scheduling(script.into());
    options(
        actor,
        DeckOptionsCommand::Save {
            request: Box::new(state.save_request(SaveMode::Normal)),
            confirmed: false,
        },
    );
}
fn queue(actor: &CollectionActor) -> Queue {
    let Response::Queue(q) = actor.execute(Command::Queue { limit: 100 }).unwrap() else {
        panic!()
    };
    q
}
#[test]
fn transformed_labels_and_answer_are_cached_and_errors_restore_standard_states() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let actor = CollectionActor::open(dir.path()).unwrap();
    actor.execute(Command::GenerateStudyFixture).unwrap();
    let original = queue(&actor);
    let top = original.cards.first().unwrap();
    let deck_id = top.deck_id;
    script(
        &actor,
        deck_id,
        r#"
        await Promise.resolve();
        for (const rating of ['again','hard','good','easy']) {
            states[rating] = { normal: { review: { scheduledDays: 7, elapsedDays: 0, easeFactor: 2.5, lapses: 0, leeched: false } } };
            customData[rating].d = ctx.deckName.slice(0, 20);
        }
    "#,
    );
    let mutated = queue(&actor);
    assert!(
        mutated.custom_scheduling_error.is_none(),
        "{:?}",
        mutated.custom_scheduling_error
    );
    let top = mutated.cards.first().unwrap();
    assert_ne!(top.intervals, original.cards.first().unwrap().intervals);
    let id = top.id;
    actor
        .execute(Command::Answer {
            card_id: id,
            rating: Rating::Good,
            answered_at_millis: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64,
            milliseconds_taken: 1000,
        })
        .unwrap();
    // Card info is backed by Anki's stored state (no direct database access).
    let Response::Manage(ManageResponse::Stats(stats::StatsResponse::CardInfo(info))) = actor
        .execute(Command::Manage(ManageCommand::Stats(
            stats::StatsCommand::CardInfo { card_id: id },
        )))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(info.revlog[0].interval, 7 * 86400);
    let Response::Manage(ManageResponse::Connect(saved)) = actor
        .execute(Command::Manage(ManageCommand::Connect(
            ConnectCommand::CardsInfo { ids: vec![id] },
        )))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(saved.result[0]["interval"], 7);
    let data: serde_json::Value =
        serde_json::from_str(saved.result[0]["customData"].as_str().unwrap()).unwrap();
    assert!(data["d"].as_str().is_some_and(|d| !d.is_empty()));
    script(&actor, deck_id, "");
    let standard = queue(&actor);
    script(&actor, deck_id, "states.good = {}; throw Error('broken');");
    let broken = queue(&actor);
    assert!(broken.custom_scheduling_error.is_some());
    for (a, b) in standard.cards.iter().zip(&broken.cards) {
        assert_eq!(a.id, b.id);
        assert_eq!(a.intervals, b.intervals);
    }
    actor.close().unwrap();
}

#[test]
fn anki_validates_custom_data_at_answer_time() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let actor = CollectionActor::open(dir.path()).unwrap();
    actor.execute(Command::GenerateStudyFixture).unwrap();
    let deck_id = queue(&actor).cards[0].deck_id;
    script(&actor, deck_id, "customData.good.longer_than_eight = 1;");
    let q = queue(&actor);
    assert!(q.custom_scheduling_error.is_none());
    let error = actor
        .execute(Command::Answer {
            card_id: q.cards[0].id,
            rating: Rating::Good,
            answered_at_millis: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64,
            milliseconds_taken: 1000,
        })
        .unwrap_err();
    assert!(error.to_string().contains("8 bytes"), "{error}");
    actor.close().unwrap();
}
