//! The deck options form: groups in desktop's order, each setting's label,
//! help text, kind, range and visibility, and the page's other strings, all
//! from Anki's localised strings (`deck-config-*`, `scheduling-*`). Ranges
//! and step sizes follow `ts/routes/deck-options/*.svelte`.
use serde::{Deserialize, Serialize};

/// One setting of the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum FieldId {
    // Daily limits.
    NewPerDay,
    ReviewsPerDay,
    NewCardsIgnoreReviewLimit,
    ApplyAllParentLimits,
    // New cards.
    LearnSteps,
    GraduatingIntervalGood,
    GraduatingIntervalEasy,
    NewCardInsertOrder,
    // Lapses.
    RelearnSteps,
    MinimumLapseInterval,
    LeechThreshold,
    LeechAction,
    // Display order.
    NewCardGatherPriority,
    NewCardSortOrder,
    NewMix,
    InterdayLearningMix,
    ReviewOrder,
    // FSRS.
    Fsrs,
    DesiredRetention,
    FsrsParams,
    ParamSearch,
    FsrsReschedule,
    FsrsHealthCheck,
    // Burying.
    BuryNew,
    BuryReviews,
    BuryInterdayLearning,
    // Audio.
    DisableAutoplay,
    SkipQuestionWhenReplayingAnswer,
    // Timers.
    CapAnswerTimeToSecs,
    ShowTimer,
    StopTimerOnAnswer,
    // Auto advance.
    SecondsToShowQuestion,
    SecondsToShowAnswer,
    WaitForAudio,
    QuestionAction,
    AnswerAction,
    // Easy days: 0 = Monday .. 6 = Sunday.
    EasyDay(u8),
    // Advanced.
    MaximumReviewInterval,
    InitialEase,
    EasyMultiplier,
    IntervalMultiplier,
    HardMultiplier,
    LapseMultiplier,
    HistoricalRetention,
    IgnoreRevlogsBeforeDate,
    CustomScheduling,
    LoadBalancer,
    FsrsShortTermWithSteps,
}

/// Where a tabbed value is stored (desktop's tabs on the limit and
/// desired retention spin boxes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Scope {
    Preset,
    /// "This deck".
    Deck,
    /// "Today only".
    Today,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    pub value: i32,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FieldKind {
    Int {
        min: u32,
        max: u32,
    },
    Float {
        min: f32,
        max: f32,
        step: f32,
        /// Shown as a percentage (desired retention).
        percent: bool,
    },
    Bool,
    /// `fsrs` replaces `choices` while FSRS is on (review sort order).
    Choice {
        choices: Vec<Choice>,
        fsrs: Option<Vec<Choice>>,
    },
    /// Learning steps text, "1m 10m 1h".
    Steps,
    /// Comma-separated FSRS parameters; empty means defaults.
    Params,
    /// One line of free text (optimiser search).
    Text,
    /// "YYYY-MM-DD" or empty.
    Date,
    /// Minimum (0.0) / Reduced (0.5) / Normal (1.0).
    EasyDay,
    /// Multi-line text edited in `$EDITOR` (custom scheduling).
    Script,
}

/// When a setting is shown, as desktop hides SM-2-only settings with FSRS
/// on and FSRS settings with it off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Shown {
    Always,
    Fsrs,
    Sm2,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldSpec {
    pub id: FieldId,
    pub label: String,
    pub help: String,
    pub kind: FieldKind,
    /// Affects the whole collection (desktop's "global" label).
    pub global: bool,
    pub shown: Shown,
    /// Tabs, for tabbed values; empty otherwise.
    pub scopes: Vec<Scope>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub title: String,
    pub fields: Vec<FieldSpec>,
}

/// A message with a count: `one` / `other` forms with `{n}` (and `{m}`)
/// placeholders, rendered by [`Plural::format`].
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Plural {
    pub one: String,
    pub other: String,
}

const N: &str = "\u{e000}";
const M: &str = "\u{e001}";

impl Plural {
    pub fn format(&self, n: u32, m: impl std::fmt::Display) -> String {
        let template = if n == 1 { &self.one } else { &self.other };
        template
            .replace(N, &n.to_string())
            .replace(M, &m.to_string())
    }
}

/// The page's other strings (buttons, prompts, warnings).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Strings {
    pub title: String,
    pub preset: String,
    pub this_deck: String,
    pub today_only: String,
    pub affects_entire_collection: String,
    pub used_by_decks: Plural,
    pub add_preset: String,
    pub clone_preset: String,
    pub rename_preset: String,
    pub remove_preset: String,
    pub name_prompt: String,
    /// "Remove {name}?" with the name as `{n}`.
    pub confirm_remove_template: String,
    pub will_require_full_sync: String,
    pub default_cant_be_removed: String,
    pub save: String,
    pub save_to_all_subdecks: String,
    pub optimize_all: String,
    pub optimize_all_confirm: String,
    pub optimize_current: String,
    pub evaluate: String,
    pub compute_minimum_retention: String,
    pub simulator: String,
    pub simulate: String,
    pub clear_last_simulation: String,
    pub days_to_simulate: String,
    pub additional_new_cards: String,
    pub suspend_leeches: String,
    pub smooth_graph: String,
    pub sim_reviews: String,
    pub sim_time: String,
    pub sim_memorized: String,
    pub easy_minimum: String,
    pub easy_reduced: String,
    pub easy_normal: String,
    pub please_save_first: String,
    pub simulate_save_preset: String,
    pub fsrs_must_be_enabled: String,
    pub time_to_optimize: String,
    pub cancel: String,
    // Warnings.
    pub reviews_too_low: Plural,
    pub learning_step_above_graduating: String,
    pub good_above_easy: String,
    pub relearning_above_minimum: String,
    pub steps_too_large_for_fsrs: String,
    pub insertion_order_random: String,
    pub answer_secs_above_recommended: String,
    pub too_short_maximum_interval: String,
    pub retention_too_low: String,
    pub retention_too_high: String,
    pub reschedule_warning: String,
    pub easy_days_no_normal: String,
    pub easy_days_change: String,
    pub manual_params_warning: String,
    pub invalid_params: String,
}

impl Strings {
    /// "Remove {name}?"
    pub fn confirm_remove(&self, name: &str) -> String {
        self.confirm_remove_template.replace(N, name)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Form {
    pub groups: Vec<Group>,
    pub strings: Strings,
}

impl Form {
    pub fn spec(&self, id: FieldId) -> Option<&FieldSpec> {
        self.groups
            .iter()
            .flat_map(|g| g.fields.iter())
            .find(|f| f.id == id)
    }
}

/// tanki's warning when the collection has a custom scheduling script
/// (PLAN 8.1). English: Anki has no such string.
pub const CUSTOM_SCHEDULING_WARNING: &str = "This collection has a custom scheduling script. \
     tanki runs it in QuickJS; script failures leave the card's standard scheduling unchanged.";

/// The form in Anki's English template strings, for tools and tests that
/// have no collection at hand. `DeckOptionsCommand::Load` returns the
/// collection's own (localised) form.
pub fn english() -> Form {
    build(&anki_i18n::I18n::template_only())
}

pub(crate) fn build(tr: &anki_i18n::I18n) -> Form {
    use FieldId as F;
    let s = |text: std::borrow::Cow<'_, str>| anki_i18n::without_unicode_isolation(&text);
    let join = |parts: &[std::borrow::Cow<'_, str>]| {
        parts
            .iter()
            .map(|p| anki_i18n::without_unicode_isolation(p))
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    let choice = |value: i32, label: std::borrow::Cow<'_, str>| Choice {
        value,
        label: s(label),
    };
    let field = |id, label: String, help: String, kind| FieldSpec {
        id,
        label,
        help,
        kind,
        global: false,
        shown: Shown::Always,
        scopes: vec![],
    };
    let int = |min, max| FieldKind::Int { min, max };
    let float = |min, max, step| FieldKind::Float {
        min,
        max,
        step,
        percent: false,
    };
    let global = |mut f: FieldSpec| {
        f.global = true;
        f
    };
    let shown = |mut f: FieldSpec, when| {
        f.shown = when;
        f
    };
    let scoped = |mut f: FieldSpec, scopes: &[Scope]| {
        f.scopes = scopes.to_vec();
        f
    };
    let choices = |choices: Vec<Choice>| FieldKind::Choice {
        choices,
        fsrs: None,
    };
    let current_deck = tr.deck_config_display_order_will_use_current_deck();
    let limit_extra = [
        tr.deck_config_limit_deck_v3(),
        tr.deck_config_tab_description(),
    ];
    let all_scopes = [Scope::Preset, Scope::Deck, Scope::Today];

    let daily_limits = Group {
        title: s(tr.deck_config_daily_limits()),
        fields: vec![
            scoped(
                field(
                    F::NewPerDay,
                    s(tr.scheduling_new_cardsday()),
                    join(&[
                        tr.deck_config_new_limit_tooltip(),
                        limit_extra[0].clone(),
                        limit_extra[1].clone(),
                    ]),
                    int(0, 9999),
                ),
                &all_scopes,
            ),
            scoped(
                field(
                    F::ReviewsPerDay,
                    s(tr.scheduling_maximum_reviewsday()),
                    join(&[
                        tr.deck_config_review_limit_tooltip(),
                        tr.deck_config_limit_interday_bound_by_reviews(),
                        limit_extra[0].clone(),
                        limit_extra[1].clone(),
                    ]),
                    int(0, 9999),
                ),
                &all_scopes,
            ),
            global(field(
                F::NewCardsIgnoreReviewLimit,
                s(tr.deck_config_new_cards_ignore_review_limit()),
                join(&[
                    tr.deck_config_affects_entire_collection(),
                    tr.deck_config_new_cards_ignore_review_limit_tooltip(),
                ]),
                FieldKind::Bool,
            )),
            global(field(
                F::ApplyAllParentLimits,
                s(tr.deck_config_apply_all_parent_limits()),
                join(&[
                    tr.deck_config_affects_entire_collection(),
                    tr.deck_config_apply_all_parent_limits_tooltip(),
                ]),
                FieldKind::Bool,
            )),
        ],
    };

    let new_cards = Group {
        title: s(tr.scheduling_new_cards()),
        fields: vec![
            field(
                F::LearnSteps,
                s(tr.deck_config_learning_steps()),
                s(tr.deck_config_learning_steps_tooltip()),
                FieldKind::Steps,
            ),
            shown(
                field(
                    F::GraduatingIntervalGood,
                    s(tr.scheduling_graduating_interval()),
                    s(tr.deck_config_graduating_interval_tooltip()),
                    int(1, 9999),
                ),
                Shown::Sm2,
            ),
            shown(
                field(
                    F::GraduatingIntervalEasy,
                    s(tr.scheduling_easy_interval()),
                    s(tr.deck_config_easy_interval_tooltip()),
                    int(1, 9999),
                ),
                Shown::Sm2,
            ),
            field(
                F::NewCardInsertOrder,
                s(tr.deck_config_new_insertion_order()),
                s(tr.deck_config_new_insertion_order_tooltip()),
                choices(vec![
                    choice(0, tr.deck_config_new_insertion_order_sequential()),
                    choice(1, tr.deck_config_new_insertion_order_random()),
                ]),
            ),
        ],
    };

    let lapses = Group {
        title: s(tr.scheduling_lapses()),
        fields: vec![
            field(
                F::RelearnSteps,
                s(tr.deck_config_relearning_steps()),
                s(tr.deck_config_relearning_steps_tooltip()),
                FieldKind::Steps,
            ),
            shown(
                field(
                    F::MinimumLapseInterval,
                    s(tr.scheduling_minimum_interval()),
                    s(tr.deck_config_minimum_interval_tooltip()),
                    int(1, 9999),
                ),
                Shown::Sm2,
            ),
            field(
                F::LeechThreshold,
                s(tr.scheduling_leech_threshold()),
                s(tr.deck_config_leech_threshold_tooltip()),
                int(1, 9999),
            ),
            field(
                F::LeechAction,
                s(tr.scheduling_leech_action()),
                s(tr.deck_config_leech_action_tooltip()),
                choices(vec![
                    choice(0, tr.actions_suspend_card()),
                    choice(1, tr.scheduling_tag_only()),
                ]),
            ),
        ],
    };

    let review_head = |fsrs: bool| {
        let mut list = vec![
            choice(0, tr.deck_config_sort_order_due_date_then_random()),
            choice(1, tr.deck_config_sort_order_due_date_then_deck()),
            choice(2, tr.deck_config_sort_order_deck_then_due_date()),
            choice(3, tr.deck_config_sort_order_ascending_intervals()),
            choice(4, tr.deck_config_sort_order_descending_intervals()),
        ];
        if fsrs {
            list.push(choice(6, tr.deck_config_sort_order_ascending_difficulty()));
            list.push(choice(5, tr.deck_config_sort_order_descending_difficulty()));
            list.push(choice(
                7,
                tr.deck_config_sort_order_retrievability_ascending(),
            ));
            list.push(choice(
                11,
                tr.deck_config_sort_order_retrievability_descending(),
            ));
        } else {
            list.push(choice(5, tr.deck_config_sort_order_ascending_ease()));
            list.push(choice(6, tr.deck_config_sort_order_descending_ease()));
        }
        list.push(choice(12, tr.decks_relative_overdueness()));
        list.push(choice(8, tr.deck_config_sort_order_random()));
        list.push(choice(9, tr.decks_order_added()));
        list.push(choice(10, tr.decks_latest_added_first()));
        list
    };
    let mix = || {
        vec![
            choice(0, tr.deck_config_review_mix_mix_with_reviews()),
            choice(1, tr.deck_config_review_mix_show_after_reviews()),
            choice(2, tr.deck_config_review_mix_show_before_reviews()),
        ]
    };
    let display_order = Group {
        title: s(tr.deck_config_ordering_title()),
        fields: vec![
            field(
                F::NewCardGatherPriority,
                s(tr.deck_config_new_gather_priority()),
                join(&[
                    tr.deck_config_new_gather_priority_tooltip_2(),
                    current_deck.clone(),
                ]),
                choices(vec![
                    choice(0, tr.deck_config_new_gather_priority_deck()),
                    choice(
                        5,
                        tr.deck_config_new_gather_priority_deck_then_random_notes(),
                    ),
                    choice(
                        1,
                        tr.deck_config_new_gather_priority_position_lowest_first(),
                    ),
                    choice(
                        2,
                        tr.deck_config_new_gather_priority_position_highest_first(),
                    ),
                    choice(3, tr.deck_config_new_gather_priority_random_notes()),
                    choice(4, tr.deck_config_new_gather_priority_random_cards()),
                ]),
            ),
            field(
                F::NewCardSortOrder,
                s(tr.deck_config_new_card_sort_order()),
                join(&[
                    tr.deck_config_new_card_sort_order_tooltip_2(),
                    current_deck.clone(),
                ]),
                choices(vec![
                    choice(0, tr.deck_config_sort_order_template_then_gather()),
                    choice(1, tr.deck_config_sort_order_gather()),
                    choice(2, tr.deck_config_sort_order_card_template_then_random()),
                    choice(3, tr.deck_config_sort_order_random_note_then_template()),
                    choice(4, tr.deck_config_sort_order_random()),
                ]),
            ),
            field(
                F::NewMix,
                s(tr.deck_config_new_review_priority()),
                join(&[
                    tr.deck_config_new_review_priority_tooltip(),
                    current_deck.clone(),
                ]),
                choices(mix()),
            ),
            field(
                F::InterdayLearningMix,
                s(tr.deck_config_interday_step_priority()),
                join(&[
                    tr.deck_config_interday_step_priority_tooltip(),
                    current_deck.clone(),
                ]),
                choices(mix()),
            ),
            field(
                F::ReviewOrder,
                s(tr.deck_config_review_sort_order()),
                join(&[
                    tr.deck_config_review_sort_order_tooltip(),
                    current_deck.clone(),
                ]),
                FieldKind::Choice {
                    choices: review_head(false),
                    fsrs: Some(review_head(true)),
                },
            ),
        ],
    };

    let fsrs = Group {
        title: "FSRS".into(),
        fields: vec![
            global(field(
                F::Fsrs,
                "FSRS".into(),
                s(tr.deck_config_fsrs_tooltip()),
                FieldKind::Bool,
            )),
            scoped(
                shown(
                    field(
                        F::DesiredRetention,
                        s(tr.deck_config_desired_retention()),
                        join(&[
                            tr.deck_config_desired_retention_tooltip(),
                            tr.deck_config_desired_retention_tooltip2(),
                        ]),
                        FieldKind::Float {
                            min: 0.7,
                            max: 0.99,
                            step: 0.01,
                            percent: true,
                        },
                    ),
                    Shown::Fsrs,
                ),
                &[Scope::Preset, Scope::Deck],
            ),
            shown(
                field(
                    F::FsrsParams,
                    s(tr.deck_config_weights()),
                    join(&[
                        tr.deck_config_weights_tooltip2(),
                        tr.deck_config_compute_optimal_weights_tooltip2(),
                    ]),
                    FieldKind::Params,
                ),
                Shown::Fsrs,
            ),
            shown(
                field(
                    F::ParamSearch,
                    // Desktop shows the search box without a title.
                    "Optimizer search".into(),
                    s(tr.deck_config_compute_optimal_weights_tooltip2()),
                    FieldKind::Text,
                ),
                Shown::Fsrs,
            ),
            global(shown(
                field(
                    F::FsrsReschedule,
                    s(tr.deck_config_reschedule_cards_on_change()),
                    s(tr.deck_config_reschedule_cards_on_change_tooltip()),
                    FieldKind::Bool,
                ),
                Shown::Fsrs,
            )),
            global(shown(
                field(
                    F::FsrsHealthCheck,
                    s(tr.deck_config_slow_suffix(tr.deck_config_health_check())),
                    join(&[
                        tr.deck_config_affects_entire_collection(),
                        tr.deck_config_health_check_tooltip1(),
                        tr.deck_config_health_check_tooltip2(),
                    ]),
                    FieldKind::Bool,
                ),
                Shown::Fsrs,
            )),
        ],
    };

    let burying = Group {
        title: s(tr.deck_config_bury_title()),
        fields: vec![
            field(
                F::BuryNew,
                s(tr.deck_config_bury_new_siblings()),
                s(tr.deck_config_bury_new_tooltip()),
                FieldKind::Bool,
            ),
            field(
                F::BuryReviews,
                s(tr.deck_config_bury_review_siblings()),
                s(tr.deck_config_bury_review_tooltip()),
                FieldKind::Bool,
            ),
            field(
                F::BuryInterdayLearning,
                s(tr.deck_config_bury_interday_learning_siblings()),
                join(&[
                    tr.deck_config_bury_interday_learning_tooltip(),
                    tr.deck_config_bury_priority_tooltip(),
                ]),
                FieldKind::Bool,
            ),
        ],
    };

    let audio = Group {
        title: s(tr.deck_config_audio_title()),
        fields: vec![
            field(
                F::DisableAutoplay,
                s(tr.deck_config_disable_autoplay()),
                s(tr.deck_config_disable_autoplay_tooltip()),
                FieldKind::Bool,
            ),
            field(
                F::SkipQuestionWhenReplayingAnswer,
                s(tr.deck_config_skip_question_when_replaying()),
                s(tr.deck_config_always_include_question_audio_tooltip()),
                FieldKind::Bool,
            ),
        ],
    };

    let timers = Group {
        title: s(tr.deck_config_timer_title()),
        fields: vec![
            field(
                F::CapAnswerTimeToSecs,
                s(tr.deck_config_maximum_answer_secs()),
                s(tr.deck_config_maximum_answer_secs_tooltip()),
                int(1, 7200),
            ),
            field(
                F::ShowTimer,
                s(tr.scheduling_show_answer_timer()),
                s(tr.deck_config_show_answer_timer_tooltip()),
                FieldKind::Bool,
            ),
            field(
                F::StopTimerOnAnswer,
                s(tr.deck_config_stop_timer_on_answer()),
                s(tr.deck_config_stop_timer_on_answer_tooltip()),
                FieldKind::Bool,
            ),
        ],
    };

    let auto_advance = Group {
        title: s(tr.actions_auto_advance()),
        fields: vec![
            field(
                F::SecondsToShowQuestion,
                s(tr.deck_config_seconds_to_show_question()),
                s(tr.deck_config_seconds_to_show_question_tooltip_3()),
                float(0.0, 9999.0, 0.1),
            ),
            field(
                F::SecondsToShowAnswer,
                s(tr.deck_config_seconds_to_show_answer()),
                s(tr.deck_config_seconds_to_show_answer_tooltip_2()),
                float(0.0, 9999.0, 0.1),
            ),
            field(
                F::WaitForAudio,
                s(tr.deck_config_wait_for_audio()),
                s(tr.deck_config_wait_for_audio_tooltip_2()),
                FieldKind::Bool,
            ),
            field(
                F::QuestionAction,
                s(tr.deck_config_question_action()).trim().to_owned(),
                s(tr.deck_config_question_action_tool_tip()),
                choices(vec![
                    choice(0, tr.deck_config_question_action_show_answer()),
                    choice(1, tr.deck_config_question_action_show_reminder()),
                ]),
            ),
            field(
                F::AnswerAction,
                s(tr.deck_config_answer_action()),
                s(tr.deck_config_answer_action_tooltip_2()),
                choices(vec![
                    choice(0, tr.studying_bury_card()),
                    choice(1, tr.deck_config_answer_again()),
                    choice(2, tr.deck_config_answer_good()),
                    choice(3, tr.deck_config_answer_hard()),
                    choice(4, tr.deck_config_show_reminder()),
                ]),
            ),
        ],
    };

    let days = [
        tr.deck_config_easy_days_monday(),
        tr.deck_config_easy_days_tuesday(),
        tr.deck_config_easy_days_wednesday(),
        tr.deck_config_easy_days_thursday(),
        tr.deck_config_easy_days_friday(),
        tr.deck_config_easy_days_saturday(),
        tr.deck_config_easy_days_sunday(),
    ];
    let easy_days = Group {
        title: s(tr.deck_config_easy_days_title()),
        fields: days
            .into_iter()
            .enumerate()
            .map(|(i, day)| {
                field(
                    F::EasyDay(i as u8),
                    s(day),
                    s(tr.deck_config_easy_days_change()),
                    FieldKind::EasyDay,
                )
            })
            .collect(),
    };

    let advanced = Group {
        title: s(tr.deck_config_advanced_title()),
        fields: vec![
            field(
                F::MaximumReviewInterval,
                s(tr.scheduling_maximum_interval()),
                s(tr.deck_config_maximum_interval_tooltip()),
                int(1, 36500),
            ),
            shown(
                field(
                    F::InitialEase,
                    s(tr.scheduling_starting_ease()),
                    s(tr.deck_config_starting_ease_tooltip()),
                    float(1.31, 5.0, 0.01),
                ),
                Shown::Sm2,
            ),
            shown(
                field(
                    F::EasyMultiplier,
                    s(tr.scheduling_easy_bonus()),
                    s(tr.deck_config_easy_bonus_tooltip()),
                    float(1.0, 5.0, 0.01),
                ),
                Shown::Sm2,
            ),
            shown(
                field(
                    F::IntervalMultiplier,
                    s(tr.scheduling_interval_modifier()),
                    s(tr.deck_config_interval_modifier_tooltip()),
                    float(0.5, 2.0, 0.01),
                ),
                Shown::Sm2,
            ),
            shown(
                field(
                    F::HardMultiplier,
                    s(tr.scheduling_hard_interval()),
                    s(tr.deck_config_hard_interval_tooltip()),
                    float(0.5, 1.3, 0.01),
                ),
                Shown::Sm2,
            ),
            shown(
                field(
                    F::LapseMultiplier,
                    s(tr.scheduling_new_interval()),
                    s(tr.deck_config_new_interval_tooltip()),
                    float(0.0, 1.0, 0.01),
                ),
                Shown::Sm2,
            ),
            shown(
                field(
                    F::HistoricalRetention,
                    s(tr.deck_config_historical_retention()),
                    s(tr.deck_config_historical_retention_tooltip()),
                    float(0.5, 1.0, 0.01),
                ),
                Shown::Fsrs,
            ),
            shown(
                field(
                    F::IgnoreRevlogsBeforeDate,
                    s(tr.deck_config_ignore_before()),
                    s(tr.deck_config_ignore_before_tooltip_2()),
                    FieldKind::Date,
                ),
                Shown::Fsrs,
            ),
            global(field(
                F::CustomScheduling,
                s(tr.deck_config_custom_scheduling()),
                s(tr.deck_config_custom_scheduling_tooltip()),
                FieldKind::Script,
            )),
            // Backend fields without a desktop UI (PLAN 8.3); English labels.
            global(field(
                F::LoadBalancer,
                "Load balancer".into(),
                "Spreads reviews more evenly across days by choosing among the days within \
                 a card's fuzz range. Affects the entire collection; on by default."
                    .into(),
                FieldKind::Bool,
            )),
            global(shown(
                field(
                    F::FsrsShortTermWithSteps,
                    "FSRS short-term scheduling with learning steps".into(),
                    "Lets FSRS adjust same-day (re)learning intervals while learning steps \
                     are set. Affects the entire collection; off by default."
                        .into(),
                    FieldKind::Bool,
                ),
                Shown::Fsrs,
            )),
        ],
    };

    let plural = |one: std::borrow::Cow<'_, str>, other: std::borrow::Cow<'_, str>| Plural {
        one: s(one),
        other: s(other),
    };
    let strings = Strings {
        title: s(tr.deck_config_title()),
        preset: s(tr.deck_config_shared_preset()),
        this_deck: s(tr.deck_config_deck_only()),
        today_only: s(tr.deck_config_today_only()),
        affects_entire_collection: s(tr.deck_config_affects_entire_collection()),
        // Rendered with 1 (one form) and a string (other form), so the
        // placeholder can be swapped for the real count.
        used_by_decks: plural(
            tr.deck_config_used_by_decks(1).replace('1', N).into(),
            tr.deck_config_used_by_decks(2).replace('2', N).into(),
        ),
        add_preset: s(tr.deck_config_add_group()),
        clone_preset: s(tr.deck_config_clone_group()),
        rename_preset: s(tr.deck_config_rename_group()),
        remove_preset: s(tr.deck_config_remove_group()),
        name_prompt: s(tr.deck_config_name_prompt()),
        confirm_remove_template: s(tr.deck_config_confirm_remove_name(N)),
        will_require_full_sync: s(tr.deck_config_will_require_full_sync()),
        default_cant_be_removed: s(tr.scheduling_the_default_configuration_cant_be_removed()),
        save: s(tr.deck_config_save_button()),
        save_to_all_subdecks: s(tr.deck_config_save_to_all_subdecks()),
        optimize_all: s(tr.deck_config_save_and_optimize()),
        optimize_all_confirm: s(tr.deck_config_fsrs_confirm_save_and_optimize()),
        optimize_current: s(tr.deck_config_optimize_button()),
        evaluate: s(tr.deck_config_evaluate_button()),
        compute_minimum_retention: s(tr.deck_config_compute_optimal_retention()),
        simulator: s(tr.deck_config_fsrs_simulator_experimental()),
        simulate: s(tr.deck_config_simulate()),
        clear_last_simulation: s(tr.deck_config_clear_last_simulate()),
        days_to_simulate: s(tr.deck_config_days_to_simulate()),
        additional_new_cards: s(tr.deck_config_additional_new_cards_to_simulate()),
        suspend_leeches: s(tr.deck_config_suspend_leeches()),
        smooth_graph: s(tr.deck_config_smooth_graph()),
        sim_reviews: s(tr.deck_config_fsrs_simulator_y_axis_title_count()),
        sim_time: s(tr.deck_config_fsrs_simulator_y_axis_title_time()),
        sim_memorized: s(tr.deck_config_fsrs_simulator_y_axis_title_memorized()),
        easy_minimum: s(tr.deck_config_easy_days_minimum()),
        easy_reduced: s(tr.deck_config_easy_days_reduced()),
        easy_normal: s(tr.deck_config_easy_days_normal()),
        please_save_first: s(tr.deck_config_please_save_your_changes_first()),
        simulate_save_preset: s(tr.deck_config_fsrs_simulate_save_preset()),
        fsrs_must_be_enabled: s(tr.deck_config_fsrs_must_be_enabled()),
        time_to_optimize: s(tr.deck_config_time_to_optimize()),
        cancel: s(tr.actions_cancel()),
        reviews_too_low: plural(
            tr.deck_config_reviews_too_low(1, M),
            tr.deck_config_reviews_too_low(2, M).replace('2', N).into(),
        ),
        learning_step_above_graduating: s(tr.deck_config_learning_step_above_graduating_interval()),
        good_above_easy: s(tr.deck_config_good_above_easy()),
        relearning_above_minimum: s(tr.deck_config_relearning_steps_above_minimum_interval()),
        steps_too_large_for_fsrs: s(tr.deck_config_steps_too_large_for_fsrs()),
        insertion_order_random: s(tr.deck_config_new_insertion_order_random_with_v3()),
        answer_secs_above_recommended: s(tr.deck_config_maximum_answer_secs_above_recommended()),
        too_short_maximum_interval: s(tr.deck_config_too_short_maximum_interval()),
        retention_too_low: s(tr.deck_config_desired_retention_too_low()),
        retention_too_high: s(tr.deck_config_desired_retention_too_high()),
        reschedule_warning: s(tr.deck_config_reschedule_cards_warning()),
        easy_days_no_normal: s(tr.deck_config_easy_days_no_normal_days()),
        easy_days_change: s(tr.deck_config_easy_days_change()),
        manual_params_warning: s(tr.deck_config_manual_parameter_edit_warning()),
        invalid_params: s(tr.deck_config_invalid_parameters()),
    };

    Form {
        groups: vec![
            daily_limits,
            new_cards,
            lapses,
            display_order,
            fsrs,
            burying,
            audio,
            timers,
            auto_advance,
            easy_days,
            advanced,
        ],
        strings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plural_fills_placeholders() {
        let p = Plural {
            one: format!("{N} card, at least {M}"),
            other: format!("{N} cards, at least {M}"),
        };
        assert_eq!(p.format(1, 10), "1 card, at least 10");
        assert_eq!(p.format(30, 300), "30 cards, at least 300");
    }

    #[test]
    fn english_form_has_desktop_labels() {
        let form = build(&anki_i18n::I18n::template_only());
        let reviews = form.spec(FieldId::ReviewsPerDay).unwrap();
        assert_eq!(reviews.label, "Maximum reviews/day");
        assert_eq!(reviews.scopes.len(), 3);
        assert_eq!(form.strings.used_by_decks.format(1, ""), "used by 1 deck");
        assert_eq!(form.strings.used_by_decks.format(3, ""), "used by 3 decks");
        assert_eq!(
            form.strings.reviews_too_low.format(30, 300),
            "If adding 30 new cards each day, your review limit should be at least 300."
        );
        assert_eq!(
            form.strings.reviews_too_low.format(1, 10),
            "If adding 1 new card each day, your review limit should be at least 10."
        );
        assert_eq!(form.strings.confirm_remove("X"), "Remove X?");
        let ids: Vec<_> = form
            .groups
            .iter()
            .flat_map(|g| g.fields.iter().map(|f| f.id))
            .collect();
        let unique: std::collections::BTreeSet<_> = ids.iter().collect();
        assert_eq!(ids.len(), unique.len());
        assert_eq!(form.groups.len(), 11);
    }
}
