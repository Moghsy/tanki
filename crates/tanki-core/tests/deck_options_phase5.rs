//! Deck options stream: every `DeckOptionsCommand` through the actor on
//! temporary collections (the study fixture, or a fresh collection with a
//! generated review history for the FSRS tools).
use tanki_core::deck_options::form::{FieldId, Scope};
use tanki_core::deck_options::model::{DeckOptionsState, FieldValue};
use tanki_core::deck_options::*;
use tanki_core::*;

fn run(actor: &CollectionActor, command: DeckOptionsCommand) -> Result<DeckOptionsResponse> {
    match actor.execute(Command::Manage(ManageCommand::DeckOptions(command)))? {
        Response::Manage(ManageResponse::DeckOptions(r)) => Ok(r),
        other => panic!("unexpected {other:?}"),
    }
}

/// Runs a long command the way the TUI does: `start`, poll `progress()`.
fn run_long(actor: &CollectionActor, command: DeckOptionsCommand) -> Result<DeckOptionsResponse> {
    let pending = actor.start(Command::Manage(ManageCommand::DeckOptions(command)))?;
    loop {
        let _ = actor.progress();
        if let Some(response) = pending.try_recv()? {
            return match response {
                Response::Manage(ManageResponse::DeckOptions(r)) => Ok(r),
                other => panic!("unexpected {other:?}"),
            };
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn load(actor: &CollectionActor, deck_id: i64) -> DeckOptionsState {
    match run(actor, DeckOptionsCommand::Load { deck_id }).unwrap() {
        DeckOptionsResponse::Loaded(data) => DeckOptionsState::new(*data),
        other => panic!("unexpected {other:?}"),
    }
}

fn save(
    actor: &CollectionActor,
    state: &DeckOptionsState,
    mode: SaveMode,
    confirmed: bool,
) -> Result<OpChanges> {
    match run_long(
        actor,
        DeckOptionsCommand::Save {
            request: Box::new(state.save_request(mode)),
            confirmed,
        },
    )? {
        DeckOptionsResponse::Saved(changes) => Ok(changes),
        other => panic!("unexpected {other:?}"),
    }
}

fn study() -> (tempfile::TempDir, CollectionActor) {
    let dir = tempfile::tempdir().unwrap();
    let actor = CollectionActor::open(dir.path()).unwrap();
    actor.execute(Command::GenerateStudyFixture).unwrap();
    (dir, actor)
}

/// Deck id by full name ("Study::Vocabulary::French").
fn deck_id(actor: &CollectionActor, name: &str) -> i64 {
    let Response::DeckTree(root) = actor.execute(Command::DeckTree).unwrap() else {
        panic!("deck tree")
    };
    let mut node = &root;
    for part in name.split("::") {
        node = node
            .children
            .iter()
            .find(|d| d.name == part)
            .unwrap_or_else(|| panic!("no deck {name}"));
    }
    node.id
}

fn preset_index(state: &DeckOptionsState, name: &str) -> usize {
    state
        .presets()
        .iter()
        .position(|p| p.name == name)
        .unwrap_or_else(|| panic!("no preset {name}"))
}

#[test]
fn load_edit_save_reload() {
    let (_dir, actor) = study();
    let french = deck_id(&actor, "Study::Vocabulary::French");
    let mut state = load(&actor, french);
    let data = state.data().clone();
    assert_eq!(data.deck_name, "Study::Vocabulary::French");
    assert_eq!(state.current().name, "Study: French");
    let names: Vec<_> = state.presets().iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        ["Default", "Study: French", "Study: Media", "Study: Science"]
    );
    assert_eq!(state.value(FieldId::NewPerDay), FieldValue::Int(6));
    assert_eq!(state.value(FieldId::BuryNew), FieldValue::Bool(true));
    assert_eq!(
        state.preset_label(preset_index(&state, "Study: French")),
        "Study: French (used by 1 deck)"
    );
    assert_eq!(data.defaults.new_per_day, 20);
    assert_eq!(
        data.defaults.fsrs_params_6.len(),
        21,
        "defaults carry FSRS-6 parameters"
    );
    assert_eq!(
        data.form.spec(FieldId::LearnSteps).unwrap().label,
        "Learning steps"
    );
    assert!(data.custom_scheduling_warning.is_none());
    assert!(data.advanced.load_balancer, "Anki's default");
    assert!(!state.is_dirty());

    state
        .set_value(FieldId::NewPerDay, FieldValue::Int(13))
        .unwrap();
    state.set_text(FieldId::LearnSteps, "2m 20m 1h").unwrap();
    state.set_text(FieldId::RelearnSteps, "15m").unwrap();
    // Tag only (1) -> wraps around to Suspend (0).
    assert!(state.adjust(FieldId::LeechAction, 1));
    state
        .set_value(FieldId::ReviewOrder, FieldValue::Choice(9))
        .unwrap();
    state
        .set_text(FieldId::MaximumReviewInterval, "99999")
        .unwrap();
    state
        .set_value(FieldId::EasyDay(6), FieldValue::Float(0.5))
        .unwrap();
    state
        .set_text(FieldId::SecondsToShowAnswer, "2.25")
        .unwrap();
    assert!(state.is_dirty());
    // Only the selected preset changed: one config, last.
    let request = state.save_request(SaveMode::Normal);
    assert_eq!(request.configs.len(), 1);
    assert_eq!(request.configs[0].name, "Study: French");
    let changes = save(&actor, &state, SaveMode::Normal, false).unwrap();
    assert!(changes.deck_config);

    let state = load(&actor, french);
    assert!(!state.is_dirty());
    let c = &state.current().config;
    assert_eq!(c.new_per_day, 13);
    assert_eq!(c.learn_steps, vec![2.0, 20.0, 60.0]);
    assert_eq!(state.display(FieldId::LearnSteps), "2m 20m 1h");
    assert_eq!(c.relearn_steps, vec![15.0]);
    assert_eq!(c.leech_action, 0);
    assert_eq!(c.review_order, 9);
    // Clamped to desktop's range.
    assert_eq!(c.maximum_review_interval, 36500);
    assert_eq!(
        c.easy_days_percentages,
        vec![1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.5]
    );
    assert_eq!(c.seconds_to_show_answer, 2.3);
    // Unrelated settings and legacy keys survive the round trip.
    assert!(c.bury_new && c.bury_reviews);
    let original = data
        .presets
        .iter()
        .find(|p| p.name == "Study: French")
        .unwrap();
    assert_eq!(c.other, original.config.other);
    // Other presets were not touched.
    let science = &state.presets()[preset_index(&state, "Study: Science")];
    assert_eq!(science.config.leech_threshold, 1);
}

#[test]
fn presets_add_clone_rename_switch_and_remove() {
    let (_dir, actor) = study();
    let later = deck_id(&actor, "Study::Later");
    let mut state = load(&actor, later);
    assert_eq!(state.current().name, "Default");
    let default_uses = state.use_count(state.selected_index());

    state.add_preset("Fresh");
    assert_eq!(state.current().name, "Fresh");
    assert_eq!(state.current().id, 0);
    assert_eq!(state.current().config, state.data().defaults);
    assert_eq!(state.use_count(state.selected_index()), 1);
    assert_eq!(
        state.use_count(preset_index(&state, "Default")),
        default_uses - 1
    );
    assert!(state.assignments_changed());
    state
        .set_value(FieldId::NewPerDay, FieldValue::Int(3))
        .unwrap();
    save(&actor, &state, SaveMode::Normal, false).unwrap();

    let mut state = load(&actor, later);
    assert_eq!(state.current().name, "Fresh");
    assert_ne!(state.current().id, 0);
    assert_eq!(state.current().config.new_per_day, 3);
    let fresh_id = state.current().id;

    // Clone, rename, and a name clash gets desktop's suffix.
    state.clone_preset("Fresh copy");
    assert_eq!(state.current().config.new_per_day, 3);
    state.rename_preset("Renamed");
    assert_eq!(state.current().name, "Renamed");
    state.rename_preset("Default");
    assert!(state.current().name.starts_with("Default") && state.current().name != "Default");
    state.rename_preset("Renamed");
    save(&actor, &state, SaveMode::Normal, false).unwrap();
    let mut state = load(&actor, later);
    assert_eq!(state.current().name, "Renamed");
    assert_eq!(state.current().config.new_per_day, 3);

    // Switch back to Fresh, rename it on the way.
    state.select(preset_index(&state, "Fresh"));
    state.rename_preset("Fresh 2");
    save(&actor, &state, SaveMode::Normal, false).unwrap();
    let mut state = load(&actor, later);
    assert_eq!(state.current().id, fresh_id);
    assert_eq!(state.current().name, "Fresh 2");

    // The default preset cannot be removed.
    state.select(preset_index(&state, "Default"));
    assert!(state.remove_preset().is_err());

    // Removing a saved preset forces a full sync: confirm first.
    state.select(preset_index(&state, "Fresh 2"));
    assert!(state.removal_needs_full_sync() || state.data().schema_modified);
    state.remove_preset().unwrap();
    assert_eq!(state.removed(), [fresh_id]);
    assert_eq!(
        save(&actor, &state, SaveMode::Normal, false),
        Err(CoreError::SchemaChangeRequired)
    );
    save(&actor, &state, SaveMode::Normal, true).unwrap();
    let state = load(&actor, later);
    assert!(state.presets().iter().all(|p| p.id != fresh_id));
    // The deck moved to the preset selected after the removal.
    assert_eq!(state.data().deck_preset_id, state.current().id);
    assert!(state.data().schema_modified);
}

#[test]
fn per_deck_limits_today_only_and_apply_to_children() {
    let (_dir, actor) = study();
    let vocabulary = deck_id(&actor, "Study::Vocabulary");
    let mut state = load(&actor, vocabulary);
    assert_eq!(state.scope(FieldId::NewPerDay), Scope::Preset);

    state.set_scope(FieldId::NewPerDay, Scope::Deck);
    // The tab starts from the value shown.
    assert_eq!(state.limits().new, Some(20));
    state
        .set_value(FieldId::NewPerDay, FieldValue::Int(7))
        .unwrap();
    state.set_scope(FieldId::ReviewsPerDay, Scope::Today);
    state
        .set_value(FieldId::ReviewsPerDay, FieldValue::Int(50))
        .unwrap();
    // The preset is untouched.
    assert_eq!(state.current().config.new_per_day, 20);
    let request = state.save_request(SaveMode::Normal);
    assert_eq!(request.limits.new, Some(7));
    assert_eq!(request.limits.review_today, Some(50));
    assert_eq!(request.limits.review, None);
    save(&actor, &state, SaveMode::Normal, false).unwrap();

    let mut state = load(&actor, vocabulary);
    let limits = state.data().limits.clone();
    assert_eq!(limits.new, Some(7));
    assert_eq!(limits.review_today, Some(50));
    assert!(limits.review_today_active);
    assert_eq!(state.scope(FieldId::NewPerDay), Scope::Deck);
    assert_eq!(state.scope(FieldId::ReviewsPerDay), Scope::Today);
    assert_eq!(state.value(FieldId::NewPerDay), FieldValue::Int(7));
    assert_eq!(state.value(FieldId::ReviewsPerDay), FieldValue::Int(50));
    let overview = match actor
        .execute(Command::SelectDeck {
            deck_id: vocabulary,
        })
        .unwrap()
    {
        Response::Overview(o) => o,
        other => panic!("{other:?}"),
    };
    assert!(overview.new <= 7, "deck limit applies: {}", overview.new);

    // Back to the preset tab clears the overrides.
    state.set_scope(FieldId::NewPerDay, Scope::Preset);
    state.set_scope(FieldId::ReviewsPerDay, Scope::Preset);
    assert_eq!(state.limits().new, None);
    assert_eq!(state.limits().review_today, None);
    // Re-enabling a tab restores its last value.
    state.set_scope(FieldId::NewPerDay, Scope::Deck);
    assert_eq!(state.limits().new, Some(7));
    state.set_scope(FieldId::NewPerDay, Scope::Preset);
    save(&actor, &state, SaveMode::Normal, false).unwrap();
    let state = load(&actor, vocabulary);
    assert_eq!(state.data().limits.new, None);
    assert!(!state.data().limits.review_today_active);
    assert_eq!(state.scope(FieldId::ReviewsPerDay), Scope::Preset);

    // "Save to All Subdecks" assigns the preset to the children too.
    let mut state = load(&actor, vocabulary);
    state.select(preset_index(&state, "Study: Media"));
    save(&actor, &state, SaveMode::ApplyToChildren, false).unwrap();
    for deck in ["Study::Vocabulary::French", "Study::Vocabulary::Japanese"] {
        let state = load(&actor, deck_id(&actor, deck));
        assert_eq!(state.current().name, "Study: Media", "{deck}");
    }
}

#[test]
fn globals_advanced_toggles_and_custom_scheduling_warning() {
    let (_dir, actor) = study();
    let study_deck = deck_id(&actor, "Study");
    let mut state = load(&actor, study_deck);
    state
        .set_value(FieldId::NewCardsIgnoreReviewLimit, FieldValue::Bool(true))
        .unwrap();
    state
        .set_value(FieldId::ApplyAllParentLimits, FieldValue::Bool(true))
        .unwrap();
    state
        .set_value(FieldId::LoadBalancer, FieldValue::Bool(false))
        .unwrap();
    state
        .set_value(FieldId::FsrsShortTermWithSteps, FieldValue::Bool(true))
        .unwrap();
    state.set_custom_scheduling("states.good.normal.review.scheduledDays = 1;".into());
    assert_eq!(state.warnings(FieldId::CustomScheduling).len(), 1);
    let request = state.save_request(SaveMode::Normal);
    assert!(request.advanced.is_some());
    let changes = save(&actor, &state, SaveMode::Normal, false).unwrap();
    assert!(changes.config);

    let state = load(&actor, study_deck);
    let data = state.data();
    assert!(data.globals.new_cards_ignore_review_limit);
    assert!(data.globals.apply_all_parent_limits);
    assert!(!data.advanced.load_balancer);
    assert!(data.advanced.fsrs_short_term_with_steps);
    assert!(data.globals.card_state_customizer.contains("scheduledDays"));
    assert_eq!(
        data.custom_scheduling_warning.as_deref(),
        Some(form::CUSTOM_SCHEDULING_WARNING)
    );
    assert_eq!(state.display(FieldId::CustomScheduling), "(1 lines)");
    // Unchanged toggles are not rewritten.
    assert!(state.save_request(SaveMode::Normal).advanced.is_none());
}

#[test]
fn warnings_follow_desktop() {
    let (_dir, actor) = study();
    let mut state = load(&actor, deck_id(&actor, "Study"));
    assert!(state.warnings(FieldId::ReviewsPerDay).is_empty());
    state
        .set_value(FieldId::NewPerDay, FieldValue::Int(30))
        .unwrap();
    assert_eq!(
        state.warnings(FieldId::ReviewsPerDay),
        ["If adding 30 new cards each day, your review limit should be at least 300."]
    );
    state.set_text(FieldId::LearnSteps, "1m 2d").unwrap();
    assert_eq!(state.warnings(FieldId::LearnSteps).len(), 1);
    state
        .set_value(FieldId::Fsrs, FieldValue::Bool(true))
        .unwrap();
    assert_eq!(
        state.warnings(FieldId::LearnSteps),
        [state.form().strings.steps_too_large_for_fsrs.clone()]
    );
    state
        .set_value(FieldId::DesiredRetention, FieldValue::Float(0.97))
        .unwrap();
    assert_eq!(state.warnings(FieldId::DesiredRetention).len(), 1);
    // Desktop's range is 0.70–0.99.
    state.set_text(FieldId::DesiredRetention, "50%").unwrap();
    assert_eq!(
        state.value(FieldId::DesiredRetention),
        FieldValue::Float(0.7)
    );
    for day in 0..7 {
        state
            .set_value(FieldId::EasyDay(day), FieldValue::Float(0.0))
            .unwrap();
    }
    assert_eq!(state.warnings(FieldId::EasyDay(6)).len(), 2);
    // Review sort order offers the FSRS orders now.
    assert!(state
        .choices(FieldId::ReviewOrder)
        .iter()
        .any(|c| c.value == 11));
    // Random cards gather order disables three sort orders and resets one.
    state
        .set_value(FieldId::NewCardSortOrder, FieldValue::Choice(4))
        .unwrap();
    state
        .set_value(FieldId::NewCardGatherPriority, FieldValue::Choice(4))
        .unwrap();
    assert_eq!(
        state.value(FieldId::NewCardSortOrder),
        FieldValue::Choice(0)
    );
    assert_eq!(state.choices(FieldId::NewCardSortOrder).len(), 2);
    assert!(state.set_text(FieldId::FsrsParams, "1, x").is_err());
    assert!(state
        .set_text(FieldId::IgnoreRevlogsBeforeDate, "yesterday")
        .is_err());
    state.reset(FieldId::NewPerDay);
    assert!(state.is_default(FieldId::NewPerDay));
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// A fresh collection with 40 Basic cards in Default and a review history:
/// a learning answer 80 days ago, then reviews every ten days, answered
/// through `Command::Answer` with past timestamps (cards are made due again
/// with `SetDueDate`, whose revlog entries FSRS ignores).
fn with_history() -> (tempfile::TempDir, CollectionActor) {
    let dir = tempfile::tempdir().unwrap();
    let actor = CollectionActor::open(dir.path()).unwrap();
    for i in 0..40 {
        actor
            .execute(Command::AddNote {
                notetype: "Basic".into(),
                fields: vec![format!("front {i}"), format!("back {i}")],
            })
            .unwrap();
    }
    let mut state = load(&actor, 1);
    state
        .set_value(FieldId::NewPerDay, FieldValue::Int(9999))
        .unwrap();
    state
        .set_value(FieldId::ReviewsPerDay, FieldValue::Int(9999))
        .unwrap();
    save(&actor, &state, SaveMode::Normal, false).unwrap();
    let Response::CardIds(all) = actor
        .execute(Command::Search {
            query: "deck:Default".into(),
        })
        .unwrap()
    else {
        panic!("search")
    };
    let day = 86_400_000;
    let start = now_millis() - 80 * day;
    for round in 0..8i64 {
        // Answering clears the cached states: fetch the queue each time.
        let mut answered = 0i64;
        loop {
            let Response::Queue(queue) = actor.execute(Command::Queue { limit: 1 }).unwrap() else {
                panic!("queue")
            };
            let Some(card) = queue.cards.first() else {
                break;
            };
            let rating = match (round, answered % 5) {
                (0, _) => Rating::Easy,
                (_, 0) => Rating::Again,
                (_, 1) => Rating::Hard,
                _ => Rating::Good,
            };
            actor
                .execute(Command::Answer {
                    card_id: card.id,
                    rating,
                    answered_at_millis: start + round * 10 * day + answered * 1000,
                    milliseconds_taken: 3000,
                })
                .unwrap();
            answered += 1;
            assert!(answered <= 200, "round {round} does not end");
        }
        assert!(answered >= 40, "round {round}: {answered}");
        actor
            .execute(Command::SetDueDate {
                card_ids: all.clone(),
                days: "0".into(),
            })
            .unwrap();
    }
    (dir, actor)
}

#[test]
fn fsrs_optimize_evaluate_retention_and_simulate() {
    let (_dir, actor) = with_history();
    let mut state = load(&actor, 1);
    state
        .set_value(FieldId::Fsrs, FieldValue::Bool(true))
        .unwrap();
    save(&actor, &state, SaveMode::Normal, false).unwrap();
    let mut state = load(&actor, 1);
    assert!(state.fsrs());

    let optimized = match run_long(
        &actor,
        DeckOptionsCommand::Optimize {
            preset_name: state.current().name.clone(),
            config: Box::new(state.current().config.clone()),
            health_check: false,
        },
    )
    .unwrap()
    {
        DeckOptionsResponse::Optimized(o) => o,
        other => panic!("{other:?}"),
    };
    assert!(optimized.fsrs_items >= 100, "{optimized:?}");
    assert_eq!(optimized.params.len(), 21);
    state.apply_optimized(&optimized);
    if !optimized.already_optimal {
        assert_eq!(state.current().config.fsrs_params_6, optimized.params);
        assert!(state.is_dirty());
    }

    // Evaluate the parameters in the form.
    match run_long(
        &actor,
        DeckOptionsCommand::Evaluate {
            preset_name: state.current().name.clone(),
            config: Box::new(state.current().config.clone()),
        },
    )
    .unwrap()
    {
        DeckOptionsResponse::Evaluated {
            log_loss,
            rmse_bins,
            message,
        } => {
            assert!(log_loss > 0.0 && rmse_bins >= 0.0, "{message}");
            assert!(message.starts_with("Log loss: "), "{message}");
        }
        other => panic!("{other:?}"),
    }

    // Saving new parameters recomputes memory states; with "reschedule".
    state
        .set_value(FieldId::FsrsReschedule, FieldValue::Bool(true))
        .unwrap();
    let changes = save(&actor, &state, SaveMode::Normal, false).unwrap();
    assert!(changes.card || optimized.already_optimal);
    let state = load(&actor, 1);
    assert!(state.params_saved());

    // Simulator.
    let request = state.simulate_request(30, 10, true);
    assert_eq!(request.search, "preset:\"Default\" -is:suspended");
    match run_long(
        &actor,
        DeckOptionsCommand::Simulate(Box::new(request.clone())),
    )
    .unwrap()
    {
        DeckOptionsResponse::Simulated(sim) => {
            assert_eq!(sim.daily_review_count.len(), 30);
            assert_eq!(sim.daily_time_cost.len(), 30);
            assert_eq!(sim.accumulated_knowledge_acquisition.len(), 30);
            assert!(sim.daily_new_count.iter().sum::<u32>() <= 10);
        }
        other => panic!("{other:?}"),
    }

    // Minimum recommended retention: a value in Anki's clamp range, or
    // Anki's "unable to determine" message on this tiny history.
    match run_long(
        &actor,
        DeckOptionsCommand::MinimumRetention(Box::new(request)),
    ) {
        Ok(DeckOptionsResponse::MinimumRetention { retention, message }) => {
            assert!((0.7..=0.95).contains(&retention), "{retention}");
            assert!(
                message.starts_with("Minimum recommended retention: "),
                "{message}"
            );
        }
        Err(CoreError::Invalid(message)) => assert!(!message.is_empty()),
        other => panic!("{other:?}"),
    }

    // "Ignore reviews before" counts.
    match run(
        &actor,
        DeckOptionsCommand::IgnoredBeforeCount {
            date: "2000-01-01".into(),
            search: "deck:Default".into(),
        },
    )
    .unwrap()
    {
        DeckOptionsResponse::IgnoredBefore {
            included,
            total,
            message,
        } => {
            assert_eq!((included, total), (40, 40));
            assert!(message.contains("40/40"), "{message}");
        }
        other => panic!("{other:?}"),
    }

    // "Optimize All Presets" saves and trains every preset.
    let mut state = load(&actor, 1);
    state.add_preset("Second");
    save(&actor, &state, SaveMode::OptimizeAll, false).unwrap();
    let state = load(&actor, 1);
    assert_eq!(state.data().days_since_last_fsrs_optimize, 0);
    assert_eq!(state.current().name, "Second");
}

#[test]
fn fsrs_without_history_and_cancel() {
    let (_dir, actor) = study();
    let study_deck = deck_id(&actor, "Study");
    let state = load(&actor, study_deck);
    let optimize = || DeckOptionsCommand::Optimize {
        preset_name: state.current().name.clone(),
        config: Box::new(state.current().config.clone()),
        health_check: true,
    };
    // The fixture's reviews all happened today: nothing to train on.
    match run_long(&actor, optimize()).unwrap() {
        DeckOptionsResponse::Optimized(o) => {
            assert_eq!(o.fsrs_items, 0);
            assert!(o.already_optimal);
            assert!(o.message.starts_with("No reviews found."), "{}", o.message);
        }
        other => panic!("{other:?}"),
    }
    // Evaluating needs history too: Anki's localised error, or a result.
    match run_long(
        &actor,
        DeckOptionsCommand::Evaluate {
            preset_name: state.current().name.clone(),
            config: Box::new(state.current().config.clone()),
        },
    ) {
        Err(CoreError::Invalid(message)) => assert!(!message.is_empty()),
        Ok(DeckOptionsResponse::Evaluated { .. }) => {}
        other => panic!("{other:?}"),
    }
    // Optimize All needs FSRS on.
    assert!(matches!(
        save(&actor, &state, SaveMode::OptimizeAll, false),
        Err(CoreError::Invalid(_))
    ));

    // Cancelling: either the operation stops with `Interrupted`, or it had
    // finished already. The actor keeps working either way.
    let pending = actor
        .start(Command::Manage(ManageCommand::DeckOptions(optimize())))
        .unwrap();
    actor.cancel_operation();
    match pending.wait() {
        Ok(_) | Err(CoreError::Interrupted) => {}
        Err(e) => panic!("{e}"),
    }
    let _ = load(&actor, study_deck);
    // Bad parameters are refused with Anki's message.
    let mut state = load(&actor, study_deck);
    state.set_text(FieldId::FsrsParams, "1, 2, 3").unwrap();
    assert!(matches!(
        save(&actor, &state, SaveMode::Normal, false),
        Err(CoreError::Invalid(_))
    ));
}
