use super::*;
use crate::testing::{card, t0, FakeBackend};
use tanki_core::{AnswerOutcome, CoreError, QueueKind, SchedTiming, SchedulerStatus};

fn started(b: &FakeBackend, now: Now) -> (Reviewer, Vec<Effect>) {
    let mut r = Reviewer::new(now);
    let fx = r.start(b, now);
    (r, fx)
}

fn has_cue(fx: &[Effect], kind: CueKind) -> bool {
    fx.iter()
        .any(|e| matches!(e, Effect::Audio(c) if c.kind == kind))
}

fn notices(fx: &[Effect]) -> Vec<String> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Notice(n) => Some(n.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn question_answer_rate_and_finish() {
    let b = FakeBackend::new(vec![card(1, "Q1", "A1"), card(2, "Q2", "A2")]);
    let now = t0();
    let (mut r, fx) = started(&b, now);
    assert_eq!(
        b.names(),
        [
            "SchedTiming",
            "SchedulerStatus",
            "Queue",
            "Render",
            "Render",
            "StudyConfig"
        ]
    );
    assert!(matches!(
        b.state.borrow().log[2],
        Command::Queue { limit: 2 }
    ));
    assert_eq!(r.phase(), Phase::Question);
    assert!(has_cue(&fx, CueKind::ShowQuestion));
    assert_eq!(r.side_html().unwrap(), "Q1");
    assert_eq!(r.counts().new, 2);
    assert_eq!(r.next_rendered().unwrap().review_question(), "Q2");

    // Ratings are ignored on the question side.
    assert!(r.answer(&b, Rating::Good, now).is_empty());
    assert!(b.state.borrow().answered.is_empty());

    let fx = r.enter(&b, now.plus_millis(1500));
    assert_eq!(r.phase(), Phase::Answer);
    assert!(has_cue(&fx, CueKind::ShowAnswer));
    assert_eq!(r.side_html().unwrap(), "A1");

    // Enter on the answer side answers Good with uncapped time from the question.
    let later = now.plus_millis(125_000);
    let fx = r.enter(&b, later);
    let answered = b.state.borrow().answered[0].clone();
    assert_eq!(answered.card_id, 1);
    assert!(matches!(answered.rating, Rating::Good));
    assert_eq!(answered.milliseconds_taken, 125_000);
    assert_eq!(answered.answered_at_millis, later.wall_ms);
    assert_eq!(r.phase(), Phase::Question);
    assert_eq!(r.card().unwrap().queued.id, 2);
    assert!(has_cue(&fx, CueKind::ShowQuestion));

    r.show_answer(&b, later);
    let fx = r.answer(&b, Rating::Again, later);
    assert!(matches!(b.state.borrow().answered[1].rating, Rating::Again));
    assert!(fx.contains(&Effect::Finished));
    assert!(r.card().is_none());
}

#[test]
fn keys_are_ignored_in_transition_and_errors_restore_the_answer_side() {
    let b = FakeBackend::new(vec![card(1, "Q", "A")]);
    let now = t0();
    let (mut r, _) = started(&b, now);
    r.show_answer(&b, now);
    b.fail("Answer", CoreError::Other("boom".into()));
    let fx = r.answer(&b, Rating::Hard, now);
    assert_eq!(fx, [Effect::Error("boom".into())]);
    assert_eq!(r.phase(), Phase::Answer);
    // Timebox prompt keeps the reviewer in transition.
    b.with(|s| {
        s.failing.clear();
        s.config.timebox_secs = 60;
    });
    // Reload the card so its config has the timebox.
    let (mut r, _) = started(&b, now);
    r.show_answer(&b, now);
    let fx = r.answer(&b, Rating::Good, now.plus_millis(61_000));
    assert!(fx.contains(&Effect::Timebox {
        cards: 1,
        minutes: 1
    }));
    assert_eq!(r.phase(), Phase::Transition);
    assert!(r.enter(&b, now).is_empty());
    assert!(r.show_answer(&b, now).is_empty());
    assert!(r.undo(&b, false, now).is_empty());
    assert!(r.bury_card(&b, now).is_empty());
}

#[test]
fn timebox_counts_answers_and_continue_restarts_it() {
    let cards = (1..=4).map(|i| card(i, "Q", "A")).collect();
    let b = FakeBackend::new(cards);
    b.with(|s| s.config.timebox_secs = 120);
    let now = t0();
    let (mut r, _) = started(&b, now);
    for i in 0..2 {
        r.show_answer(&b, now);
        let fx = r.answer(&b, Rating::Good, now.plus_millis(10_000 * i));
        assert!(!fx.iter().any(|e| matches!(e, Effect::Timebox { .. })));
    }
    r.show_answer(&b, now);
    // Exactly at the limit is not past it.
    let fx = r.answer(&b, Rating::Good, now.plus_millis(120_000));
    assert!(!fx.iter().any(|e| matches!(e, Effect::Timebox { .. })));
    r.show_answer(&b, now);
    let at = now.plus_millis(120_001);
    let fx = r.answer(&b, Rating::Good, at);
    assert_eq!(
        fx,
        [Effect::Timebox {
            cards: 4,
            minutes: 2
        }]
    );
    assert!(r.timebox_pending());
    let fx = r.timebox_continue(&b, at);
    assert!(fx.contains(&Effect::Finished));
    assert!(!r.timebox_pending());
    assert_eq!(timebox_message(4, 2), "4 cards studied in 2 minutes.");
    assert_eq!(timebox_message(1, 1), "1 card studied in 1 minute.");
}

#[test]
fn type_answer_flow_compares_what_was_typed() {
    let mut c = card(
        1,
        "Capital?\n[[type:Back]]",
        "Capital?<hr id=answer>[[type:Back]]",
    );
    c.rendered.type_answer = Some(TypeAnswer::Input {
        expected: "Paris".into(),
        combining: false,
    });
    let b = FakeBackend::new(vec![c]);
    let now = t0();
    let (mut r, _) = started(&b, now);
    assert_eq!(
        r.side_html().unwrap(),
        format!("Capital?\n{}", tanki_core::TYPE_ANSWER_INPUT)
    );
    r.typed_mut().unwrap().insert_str("Pari");
    r.enter(&b, now);
    let compare = b.state.borrow().log.last().cloned().unwrap();
    assert!(matches!(
        compare,
        Command::CompareAnswer { ref expected, ref provided, combining: false }
            if expected == "Paris" && provided == "Pari"
    ));
    assert_eq!(r.phase(), Phase::Answer);
    assert!(r.typed().is_none());
    assert_eq!(
        r.side_html().unwrap(),
        "Capital?<hr id=answer>\n<div><code id=typeans>Pari|Paris</code></div>"
    );
}

#[test]
fn type_answer_with_empty_input_still_compares() {
    let mut c = card(1, "[[type:Back]]", "[[type:Back]]");
    c.rendered.type_answer = Some(TypeAnswer::Input {
        expected: "x".into(),
        combining: true,
    });
    let b = FakeBackend::new(vec![c]);
    let (mut r, _) = started(&b, t0());
    assert_eq!(r.typed().unwrap().text(), "");
    r.show_answer(&b, t0());
    assert!(b.names().contains(&"CompareAnswer".to_string()));
}

#[test]
fn undo_and_redo_refetch_the_queue() {
    let b = FakeBackend::new(vec![card(1, "Q1", "A1"), card(2, "Q2", "A2")]);
    let now = t0();
    let (mut r, _) = started(&b, now);
    r.show_answer(&b, now);
    r.answer(&b, Rating::Easy, now);
    assert_eq!(r.card().unwrap().queued.id, 2);
    b.clear_log();
    let fx = r.undo(&b, false, now);
    assert_eq!(b.names()[..2], ["Undo", "Queue"]);
    assert_eq!(notices(&fx), ["Answer Card undone"]);
    assert_eq!(r.card().unwrap().queued.id, 1);
    assert_eq!(r.phase(), Phase::Question);
    let fx = r.undo(&b, true, now);
    assert_eq!(notices(&fx), ["Answer Card redone"]);
    assert_eq!(r.card().unwrap().queued.id, 2);
    // Nothing more to redo: an error, no refetch.
    b.clear_log();
    let fx = r.undo(&b, true, now);
    assert_eq!(fx, [Effect::Error("Nothing to redo".into())]);
    assert_eq!(b.names(), ["Redo"]);
}

#[test]
fn leech_notice_mentions_suspension() {
    let b = FakeBackend::new(vec![card(1, "Q", "A"), card(2, "Q", "A")]);
    let now = t0();
    let (mut r, _) = started(&b, now);
    b.with(|s| {
        s.next_outcome = AnswerOutcome {
            leech: true,
            suspended: true,
            ..Default::default()
        }
    });
    r.show_answer(&b, now);
    let fx = r.answer(&b, Rating::Again, now);
    assert_eq!(notices(&fx), ["Card was a leech. It has been suspended."]);
    b.with(|s| {
        s.next_outcome = AnswerOutcome {
            leech: true,
            ..Default::default()
        }
    });
    r.show_answer(&b, now);
    let fx = r.answer(&b, Rating::Again, now);
    assert_eq!(notices(&fx), ["Card was a leech."]);
}

#[test]
fn day_rollover_refetches_timing_and_queue() {
    let b = FakeBackend::new(vec![card(1, "Q", "A"), card(2, "Q", "A")]);
    let now = t0();
    let next_day = now.wall_ms / 1000 + 60;
    b.with(|s| {
        s.timing = SchedTiming {
            days_elapsed: 1,
            next_day_at: next_day,
        }
    });
    let (mut r, _) = started(&b, now);
    b.clear_log();
    assert!(r.tick(&b, now.plus_millis(59_000), false, true).is_empty());
    assert!(b.names().is_empty());
    b.with(|s| {
        s.timing.next_day_at = next_day + 86_400;
        // The new day's queue starts with another card.
        s.cards.swap(0, 1);
    });
    let fx = r.tick(&b, now.plus_millis(60_000), false, true);
    assert_eq!(b.names()[..2], ["SchedTiming", "Queue"]);
    assert!(has_cue(&fx, CueKind::ShowQuestion));
    assert_eq!(r.card().unwrap().queued.id, 2);
    // The next rollover is a day later; nothing happens before it.
    b.clear_log();
    r.tick(&b, now.plus_millis(120_000), false, true);
    assert!(b.names().is_empty());
}

#[test]
fn rollover_keeps_the_same_card_and_its_side() {
    let b = FakeBackend::new(vec![card(1, "Q", "A")]);
    let now = t0();
    b.with(|s| s.timing.next_day_at = now.wall_ms / 1000);
    let (mut r, _) = started(&b, now);
    r.show_answer(&b, now);
    let fx = r.rollover(&b, now);
    assert!(fx.is_empty());
    assert_eq!(r.phase(), Phase::Answer);
}

fn auto_backend(config: impl FnOnce(&mut StudyConfig)) -> FakeBackend {
    let b = FakeBackend::new(vec![card(1, "Q1", "A1"), card(2, "Q2", "A2")]);
    b.with(|s| {
        s.config.seconds_to_show_question = 2.0;
        s.config.seconds_to_show_answer = 3.0;
        config(&mut s.config);
    });
    b
}

#[test]
fn auto_advance_is_off_until_toggled_then_shows_answer_and_answers() {
    let b = auto_backend(|c| c.answer_action = AnswerAction::AnswerGood);
    let now = t0();
    let (mut r, _) = started(&b, now);
    assert!(r.tick(&b, now.plus_millis(5000), false, true).is_empty());
    assert_eq!(
        notices(&r.toggle_auto_advance(now)),
        ["Auto Advance enabled"]
    );
    assert!(r.tick(&b, now.plus_millis(1999), false, true).is_empty());
    let fx = r.tick(&b, now.plus_millis(2000), false, true);
    assert!(has_cue(&fx, CueKind::ShowAnswer));
    assert_eq!(r.phase(), Phase::Answer);
    let at = now.plus_millis(2000);
    assert!(r.tick(&b, at.plus_millis(2999), false, true).is_empty());
    r.tick(&b, at.plus_millis(3000), false, true);
    assert!(matches!(b.state.borrow().answered[0].rating, Rating::Good));
    assert_eq!(r.card().unwrap().queued.id, 2);
    assert_eq!(r.phase(), Phase::Question);
    assert_eq!(
        notices(&r.toggle_auto_advance(now)),
        ["Auto Advance disabled"]
    );
    assert!(r.tick(&b, at.plus_millis(60_000), false, true).is_empty());
}

#[test]
fn auto_advance_answer_actions() {
    for (action, expect) in [
        (AnswerAction::AnswerAgain, Some(Rating::Again)),
        (AnswerAction::AnswerHard, Some(Rating::Hard)),
        (AnswerAction::AnswerGood, Some(Rating::Good)),
        (AnswerAction::BuryCard, None),
        (AnswerAction::ShowReminder, None),
    ] {
        let b = auto_backend(|c| c.answer_action = action);
        let now = t0();
        let (mut r, _) = started(&b, now);
        r.toggle_auto_advance(now);
        r.show_answer(&b, now);
        let fx = r.tick(&b, now.plus_millis(3000), false, true);
        let answered = b.state.borrow().answered.first().map(|a| a.rating);
        assert_eq!(
            answered.map(|r| format!("{r:?}")),
            expect.map(|r| format!("{r:?}")),
            "{action:?}"
        );
        match action {
            AnswerAction::BuryCard => {
                assert!(b.names().contains(&"BuryOrSuspend".into()));
                assert_eq!(notices(&fx), ["Card buried."]);
            }
            AnswerAction::ShowReminder => {
                assert_eq!(notices(&fx), ["Answer time elapsed"]);
                assert_eq!(r.phase(), Phase::Answer);
            }
            _ => {}
        }
    }
}

#[test]
fn auto_advance_question_reminder_and_focus_loss() {
    let b = auto_backend(|c| c.question_action = QuestionAction::ShowReminder);
    let now = t0();
    let (mut r, _) = started(&b, now);
    r.toggle_auto_advance(now);
    let fx = r.tick(&b, now.plus_millis(2000), false, true);
    assert_eq!(notices(&fx), ["Question time elapsed"]);
    assert_eq!(r.phase(), Phase::Question);
    // Fires once.
    assert!(r.tick(&b, now.plus_millis(9000), false, true).is_empty());

    let b = auto_backend(|_| {});
    let (mut r, _) = started(&b, now);
    r.toggle_auto_advance(now);
    let fx = r.tick(&b, now.plus_millis(2000), false, false);
    assert_eq!(notices(&fx), ["Auto Advance disabled"]);
    assert!(!r.auto_advance());
    assert_eq!(r.phase(), Phase::Question);
}

#[test]
fn auto_advance_waits_for_audio_then_fires_on_idle() {
    let b = auto_backend(|_| {});
    let now = t0();
    let (mut r, _) = started(&b, now);
    r.toggle_auto_advance(now);
    assert!(r.tick(&b, now.plus_millis(2500), true, true).is_empty());
    assert_eq!(r.phase(), Phase::Question);
    // Idle before the timer ran out does nothing.
    let b2 = auto_backend(|_| {});
    let (mut r2, _) = started(&b2, now);
    r2.toggle_auto_advance(now);
    assert!(r2.audio_idle(&b2, now.plus_millis(100), true).is_empty());
    // Idle after it ran out fires it.
    let fx = r.audio_idle(&b, now.plus_millis(4000), true);
    assert!(has_cue(&fx, CueKind::ShowAnswer));

    // Without wait_for_audio, playing audio does not hold it back.
    let b = auto_backend(|c| c.wait_for_audio = false);
    let (mut r, _) = started(&b, now);
    r.toggle_auto_advance(now);
    let fx = r.tick(&b, now.plus_millis(2000), true, true);
    assert!(has_cue(&fx, CueKind::ShowAnswer));
}

#[test]
fn audio_cues_carry_tags_and_settings() {
    let mut c = card(1, "Q", "A");
    c.rendered.question_audio = vec![AudioTag::SoundOrVideo("q.mp3".into())];
    c.rendered.answer_audio = vec![AudioTag::Tts {
        text: "hi".into(),
        lang: "en_US".into(),
        voices: vec![],
        speed: 1.0,
        other_args: vec![],
    }];
    let b = FakeBackend::new(vec![c]);
    b.with(|s| {
        s.config.autoplay = false;
        s.config.interrupt_audio = false;
        s.config.skip_question_when_replaying_answer = true;
    });
    let now = t0();
    let (mut r, fx) = started(&b, now);
    let Some(Effect::Audio(cue)) = fx.iter().find(|e| matches!(e, Effect::Audio(_))) else {
        panic!("no cue")
    };
    assert_eq!(cue.question, [MediaTag::Sound("q.mp3".into())]);
    assert!(!cue.autoplay && !cue.interrupt && cue.skip_question_when_replaying_answer);
    assert!(matches!(&cue.answer[0], MediaTag::Tts { lang, .. } if lang == "en_US"));
    assert!(has_cue(&r.replay(), CueKind::Replay(Side::Question)));
    r.show_answer(&b, now);
    assert!(has_cue(&r.replay(), CueKind::Replay(Side::Answer)));
    assert_eq!(
        r.tag(Side::Question, 0),
        Some(MediaTag::Sound("q.mp3".into()))
    );
    assert_eq!(r.tag(Side::Answer, 3), None);
}

#[test]
fn card_actions_send_commands_and_refresh() {
    let b = FakeBackend::new(vec![card(1, "Q1", "A1"), card(2, "Q2", "A2")]);
    let now = t0();
    let (mut r, _) = started(&b, now);
    r.show_answer(&b, now);
    b.clear_log();
    r.toggle_flag(&b, 3, now);
    assert!(matches!(
        b.state.borrow().log[0],
        Command::SetFlag { flag: 3, .. }
    ));
    assert_eq!(b.names()[1], "Queue");
    // Same card stays on its answer side with the new flag.
    assert_eq!(r.phase(), Phase::Answer);
    assert_eq!(r.card().unwrap().queued.flag, 3);
    r.toggle_flag(&b, 3, now);
    assert_eq!(r.card().unwrap().queued.flag, 0);
    b.clear_log();
    r.toggle_mark(&b, now);
    assert!(r.card().unwrap().queued.marked);
    assert!(matches!(
        b.state.borrow().log.first(),
        Some(Command::SetMarked { marked: true, note_ids }) if note_ids == &[10]
    ));

    let fx = r.suspend_note(&b, now);
    assert_eq!(notices(&fx), ["Note suspended."]);
    assert!(b.state.borrow().log.iter().any(|c| matches!(
        c,
        Command::BuryOrSuspend {
            target: Target::Notes(ids),
            mode: tanki_core::BuryMode::Suspend
        } if ids == &[10]
    )));
    // Card 1 left the queue; card 2's question is shown.
    assert_eq!(r.card().unwrap().queued.id, 2);
    assert_eq!(r.phase(), Phase::Question);

    b.clear_log();
    r.set_due_date(&b, "3!".into(), now);
    assert!(matches!(
        &b.state.borrow().log[0],
        Command::SetDueDate { card_ids, days } if card_ids == &[2] && days == "3!"
    ));
}

#[test]
fn other_card_actions() {
    for action in ["bury_card", "bury_note", "suspend_card", "forget", "delete"] {
        let b = FakeBackend::new(vec![card(1, "Q1", "A1"), card(2, "Q2", "A2")]);
        let now = t0();
        let (mut r, _) = started(&b, now);
        b.clear_log();
        let fx = match action {
            "bury_card" => r.bury_card(&b, now),
            "bury_note" => r.bury_note(&b, now),
            "suspend_card" => r.suspend_card(&b, now),
            "forget" => r.forget(&b, true, false, now),
            _ => r.delete_note(&b, now),
        };
        let first = b.state.borrow().log[0].clone();
        let ok = match action {
            "bury_card" => {
                matches!(first, Command::BuryOrSuspend { target: Target::Cards(ref c), mode: tanki_core::BuryMode::Bury } if c == &[1])
            }
            "bury_note" => {
                matches!(first, Command::BuryOrSuspend { target: Target::Notes(ref n), mode: tanki_core::BuryMode::Bury } if n == &[10])
            }
            "suspend_card" => matches!(
                first,
                Command::BuryOrSuspend {
                    target: Target::Cards(_),
                    mode: tanki_core::BuryMode::Suspend
                }
            ),
            "forget" => matches!(
                first,
                Command::Forget {
                    restore_position: true,
                    reset_counts: false,
                    ..
                }
            ),
            _ => matches!(first, Command::DeleteNotes { ref note_ids } if note_ids == &[10]),
        };
        assert!(ok, "{action}: {first:?}");
        assert_eq!(notices(&fx).len(), 1, "{action}");
        assert_eq!(r.card().unwrap().queued.id, 2, "{action}");
    }
}

#[test]
fn external_changes_follow_op_executed() {
    let b = FakeBackend::new(vec![card(1, "Q1", "A1"), card(2, "Q2", "A2")]);
    let now = t0();
    let (mut r, _) = started(&b, now);
    r.show_answer(&b, now);
    // Note text edited elsewhere: re-render, same side.
    b.with(|s| s.cards[0].rendered.answer = "edited".into());
    b.clear_log();
    let fx = r.on_changes(
        &b,
        &OpChanges {
            note_text: true,
            ..Default::default()
        },
        now,
    );
    assert!(fx.contains(&Effect::CardChanged));
    assert_eq!(r.side_html().unwrap(), "edited");
    assert_eq!(r.phase(), Phase::Answer);
    assert_eq!(b.names(), ["Render", "Queue", "Render"]);
    // Queues changed: the question is shown again.
    r.on_changes(
        &b,
        &OpChanges {
            study_queues: true,
            ..Default::default()
        },
        now,
    );
    assert_eq!(r.phase(), Phase::Question);
    // Unrelated changes do nothing.
    b.clear_log();
    assert!(r
        .on_changes(
            &b,
            &OpChanges {
                tag: true,
                ..Default::default()
            },
            now
        )
        .is_empty());
    assert!(b.names().is_empty());
}

#[test]
fn custom_scheduling_runs_without_unsupported_warning_and_scheduler_check_offers_upgrade() {
    let b = FakeBackend::new(vec![card(1, "Q", "A"), card(2, "Q", "A")]);
    b.with(|s| s.config.custom_scheduling_set = true);
    let now = t0();
    let (mut r, fx) = started(&b, now);
    assert!(!fx.iter().any(|e| matches!(e, Effect::Warning(_))));
    r.show_answer(&b, now);
    let fx = r.answer(&b, Rating::Good, now);
    assert!(!fx.iter().any(|e| matches!(e, Effect::Warning(_))));

    let b = FakeBackend::new(vec![card(1, "Q", "A")]);
    b.with(|s| {
        s.status = SchedulerStatus {
            v3: false,
            version: 1,
        }
    });
    let (mut r, fx) = started(&b, now);
    assert_eq!(fx, [Effect::SchedulerUpgradeRequired]);
    assert!(r.card().is_none());
    let fx = r.upgrade_scheduler(&b, now);
    assert!(b.names().contains(&"UpgradeScheduler".into()));
    assert_eq!(notices(&fx), ["Upgraded to the v3 scheduler."]);
    assert_eq!(r.card().unwrap().queued.id, 1);
}

#[test]
fn study_config_and_scheduler_failures_block_study() {
    let b = FakeBackend::new(vec![card(1, "Q", "A")]);
    for name in ["StudyConfig", "SchedTiming", "SchedulerStatus"] {
        b.fail(
            name,
            CoreError::Invalid(format!("Not implemented yet: {name}")),
        );
    }
    let (r, fx) = started(&b, t0());
    assert!(fx.iter().any(|e| matches!(e, Effect::Error(_))));
    assert!(r.card().is_none());
    let b = FakeBackend::new(vec![card(1, "Q", "A")]);
    b.fail("StudyConfig", CoreError::Other("bad preset".into()));
    let (r, fx) = started(&b, t0());
    assert!(r.card().is_none());
    assert!(fx.contains(&Effect::Error("bad preset".into())));
}

#[test]
fn timer_follows_the_preset() {
    let b = FakeBackend::new(vec![card(1, "Q", "A")]);
    let now = t0();
    let (r, _) = started(&b, now);
    assert_eq!(r.timer(now.at), None);
    b.with(|s| {
        s.config.show_timer = true;
        s.config.stop_timer_on_answer = true;
        s.config.cap_answer_time_secs = 30;
    });
    let (mut r, _) = started(&b, now);
    assert_eq!(
        r.timer(now.plus_millis(12_400).at),
        Some(TimerView {
            secs: 12,
            capped: false
        })
    );
    assert_eq!(
        r.timer(now.plus_millis(45_000).at),
        Some(TimerView {
            secs: 30,
            capped: true
        })
    );
    r.show_answer(&b, now.plus_millis(5_000));
    assert_eq!(r.timer(now.plus_millis(20_000).at).unwrap().secs, 5);
}

#[test]
fn counts_come_from_the_queue() {
    let mut learning = card(1, "Q", "A");
    learning.queued.queue = QueueKind::Learning;
    let b = FakeBackend::new(vec![learning, card(2, "Q", "A")]);
    b.with(|s| s.extra_counts = (0, 0, 7));
    let (r, _) = started(&b, t0());
    assert_eq!(
        r.counts(),
        Counts {
            new: 1,
            learning: 1,
            review: 7
        }
    );
}

#[test]
fn congrats_message_units() {
    assert_eq!(next_learn_message(86_400, 3), "");
    assert_eq!(
        next_learn_message(45, 1),
        "The next learning card will be ready in 45 seconds. There is one remaining learning card due later today."
    );
    assert_eq!(
        next_learn_message(90, 2),
        "The next learning card will be ready in 2 minutes. There are 2 learning cards due later today."
    );
    assert!(next_learn_message(3600, 2).contains("in 1 hour."));
    assert_eq!(undo_notice("", false), "Change undone");
}

#[test]
fn code_tour_hold_blocks_timers_and_subtracts_only_external_time() {
    let b = FakeBackend::new(vec![card(1, "Q", "A"), card(2, "Q2", "A2")]);
    b.with(|s| {
        s.config.show_timer = true;
        s.config.cap_answer_time_secs = 0;
        s.config.seconds_to_show_answer = 1.0;
        s.config.timebox_secs = 30;
    });
    let now = t0();
    let (mut r, _) = started(&b, now);
    assert!(!r.begin_hold(now));
    r.show_answer(&b, now.plus_millis(1000));
    r.toggle_auto_advance(now.plus_millis(1000));
    assert!(r.begin_hold(now.plus_millis(2000)));
    assert!(!r.begin_hold(now.plus_millis(2000)));
    assert!(r.tick(&b, now.plus_millis(60000), false, true).is_empty());
    assert!(r
        .answer(&b, Rating::Good, now.plus_millis(60000))
        .is_empty());
    assert_eq!(r.timer(now.plus_millis(60000).at).unwrap().secs, 2);
    r.end_hold(&b, now.plus_millis(62000));
    assert!(!r.auto_advance());
    r.begin_hold(now.plus_millis(63000));
    r.end_hold(&b, now.plus_millis(123000));
    let fx = r.answer(&b, Rating::Good, now.plus_millis(124000));
    assert!(!fx.iter().any(|e| matches!(e, Effect::Timebox { .. })));
    assert_eq!(b.state.borrow().answered[0].milliseconds_taken, 4000);
    assert_eq!(
        b.state.borrow().answered[0].answered_at_millis,
        now.plus_millis(124000).wall_ms
    );
}
#[test]
fn code_tour_defers_external_changes_and_refreshes_before_rating() {
    let b = FakeBackend::new(vec![card(1, "Q", "A")]);
    let now = t0();
    let (mut r, _) = started(&b, now);
    r.show_answer(&b, now);
    r.begin_hold(now);
    b.clear_log();
    r.on_changes(
        &b,
        &OpChanges {
            note_text: true,
            ..Default::default()
        },
        now,
    );
    assert!(b.names().is_empty());
    let fx = r.end_hold(&b, now.plus_millis(1000));
    assert_eq!(r.phase(), Phase::Question);
    assert!(fx
        .iter()
        .any(|e| matches!(e, Effect::Notice(s) if s.contains("reloaded"))));
    assert!(b.names().contains(&"Queue".into()));
    assert!(b.state.borrow().answered.is_empty());
}

#[test]
fn code_tour_failed_refresh_cannot_answer_cached_states() {
    let b = FakeBackend::new(vec![card(1, "Q", "A")]);
    let now = t0();
    let (mut r, _) = started(&b, now);
    r.show_answer(&b, now);
    r.begin_hold(now);
    r.on_changes(
        &b,
        &OpChanges {
            study_queues: true,
            ..Default::default()
        },
        now,
    );
    b.fail("Queue", CoreError::Other("offline collection".into()));
    r.end_hold(&b, now.plus_millis(1000));
    assert_eq!(r.phase(), Phase::Transition);
    assert!(r.answer(&b, Rating::Good, now.plus_millis(1000)).is_empty());
    assert!(b.state.borrow().answered.is_empty());
}

#[test]
fn code_tour_hold_does_not_carry_into_the_next_timebox() {
    let b = FakeBackend::new(vec![card(1, "Q", "A"), card(2, "Q2", "A2")]);
    b.with(|s| s.config.timebox_secs = 30);
    let now = t0();
    let (mut r, _) = started(&b, now);
    r.show_answer(&b, now);
    r.begin_hold(now.plus_millis(1000));
    r.end_hold(&b, now.plus_millis(61000));
    let fx = r.answer(&b, Rating::Good, now.plus_millis(91000));
    assert!(fx.iter().any(|e| matches!(e, Effect::Timebox { .. })));
    r.timebox_continue(&b, now.plus_millis(92000));
    r.show_answer(&b, now.plus_millis(92000));
    let fx = r.answer(&b, Rating::Good, now.plus_millis(123000));
    assert!(fx.iter().any(|e| matches!(e, Effect::Timebox { .. })));
}
#[test]
fn code_tour_day_rollover_waits_until_return_then_reveals_question() {
    let now = t0();
    let b = FakeBackend::new(vec![card(1, "Q", "A")]);
    b.with(|s| s.timing.next_day_at = now.wall_ms / 1000 + 1);
    let (mut r, _) = started(&b, now);
    r.show_answer(&b, now);
    r.begin_hold(now);
    b.clear_log();
    r.tick(&b, now.plus_millis(5000), false, true);
    assert!(b.names().is_empty());
    r.end_hold(&b, now.plus_millis(5000));
    assert_eq!(r.phase(), Phase::Question);
    assert!(b.names().contains(&"Queue".into()));
    assert!(b.state.borrow().answered.is_empty());
}
