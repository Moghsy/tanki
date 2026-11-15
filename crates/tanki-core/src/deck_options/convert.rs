//! Anki calls and proto conversions for the deck options commands. Every
//! call is a fully qualified service-trait call (PLAN 4.3).
use super::*;
use crate::actor::{changes, confirm_schema_change};
use crate::CoreError;
use anki::error::AnkiError;
use anki::services::{ConfigService, DeckConfigService, SchedulerService};
use anki_proto as pb;
use pb::deck_config::deck_config::Config as PbConfig;
use pb::deck_config::deck_configs_for_update::current_deck::Limits as PbLimits;

macro_rules! mirror {
    ($($field:ident),* $(,)?) => {
        impl From<PbConfig> for PresetConfig {
            fn from(c: PbConfig) -> Self {
                Self { $($field: c.$field),* }
            }
        }
        impl From<PresetConfig> for PbConfig {
            fn from(c: PresetConfig) -> Self {
                Self { $($field: c.$field),* }
            }
        }
    };
}

mirror!(
    learn_steps,
    relearn_steps,
    fsrs_params_4,
    fsrs_params_5,
    fsrs_params_6,
    new_per_day,
    reviews_per_day,
    new_per_day_minimum,
    initial_ease,
    easy_multiplier,
    hard_multiplier,
    lapse_multiplier,
    interval_multiplier,
    maximum_review_interval,
    minimum_lapse_interval,
    graduating_interval_good,
    graduating_interval_easy,
    new_card_insert_order,
    new_card_gather_priority,
    new_card_sort_order,
    new_mix,
    review_order,
    interday_learning_mix,
    leech_action,
    leech_threshold,
    disable_autoplay,
    cap_answer_time_to_secs,
    show_timer,
    stop_timer_on_answer,
    seconds_to_show_question,
    seconds_to_show_answer,
    question_action,
    answer_action,
    wait_for_audio,
    skip_question_when_replaying_answer,
    bury_new,
    bury_reviews,
    bury_interday_learning,
    desired_retention,
    ignore_revlogs_before_date,
    easy_days_percentages,
    historical_retention,
    param_search,
    other,
);

macro_rules! limits {
    ($($field:ident),* $(,)?) => {
        impl From<PbLimits> for DeckLimits {
            fn from(l: PbLimits) -> Self {
                Self { $($field: l.$field),* }
            }
        }
        impl From<DeckLimits> for PbLimits {
            fn from(l: DeckLimits) -> Self {
                Self { $($field: l.$field),* }
            }
        }
    };
}

limits!(
    review,
    new,
    review_today,
    new_today,
    review_today_active,
    new_today_active,
    desired_retention,
);

fn preset(config: pb::deck_config::DeckConfig, use_count: u32) -> Preset {
    Preset {
        id: config.id,
        name: config.name,
        mtime_secs: config.mtime_secs,
        usn: config.usn,
        config: config.config.unwrap_or_default().into(),
        use_count,
    }
}

impl From<Preset> for pb::deck_config::DeckConfig {
    fn from(p: Preset) -> Self {
        Self {
            id: p.id,
            name: p.name,
            mtime_secs: p.mtime_secs,
            usn: p.usn,
            config: Some(p.config.into()),
        }
    }
}

fn plain(text: impl AsRef<str>) -> String {
    anki_i18n::without_unicode_isolation(text.as_ref())
}

/// FSRS and deck options errors with Anki's localised message
/// ("Insufficient review history …", invalid parameters, …).
fn localized(col: &Collection, error: AnkiError) -> CoreError {
    match error {
        AnkiError::Interrupted => CoreError::Interrupted,
        AnkiError::DbError { .. } => error.into(),
        other => CoreError::Invalid(plain(other.message(col.tr()))),
    }
}

fn bool_key(key: pb::config::config_key::Bool) -> pb::config::GetConfigBoolRequest {
    pb::config::GetConfigBoolRequest { key: key as i32 }
}

fn advanced(col: &mut Collection) -> Result<AdvancedToggles> {
    use pb::config::config_key::Bool;
    Ok(AdvancedToggles {
        load_balancer: ConfigService::get_config_bool(col, bool_key(Bool::LoadBalancerEnabled))?
            .val,
        fsrs_short_term_with_steps: ConfigService::get_config_bool(
            col,
            bool_key(Bool::FsrsShortTermWithStepsEnabled),
        )?
        .val,
    })
}

fn load(col: &mut Collection, deck_id: i64) -> Result<DeckOptionsData> {
    let data =
        DeckConfigService::get_deck_configs_for_update(col, pb::decks::DeckId { did: deck_id })
            .map_err(|e| localized(col, e))?;
    let current = data.current_deck.unwrap_or_default();
    let customizer = data.card_state_customizer;
    Ok(DeckOptionsData {
        deck_id,
        deck_name: current.name,
        deck_preset_id: current.config_id,
        parent_preset_ids: current.parent_config_ids,
        presets: data
            .all_config
            .into_iter()
            .filter_map(|c| Some(preset(c.config?, c.use_count)))
            .collect(),
        defaults: data
            .defaults
            .and_then(|d| d.config)
            .unwrap_or_default()
            .into(),
        limits: current.limits.unwrap_or_default().into(),
        custom_scheduling_warning: (!customizer.trim().is_empty())
            .then(|| form::CUSTOM_SCHEDULING_WARNING.to_owned()),
        globals: GlobalOptions {
            new_cards_ignore_review_limit: data.new_cards_ignore_review_limit,
            apply_all_parent_limits: data.apply_all_parent_limits,
            fsrs: data.fsrs,
            fsrs_health_check: data.fsrs_health_check,
            card_state_customizer: customizer,
        },
        advanced: advanced(col)?,
        fsrs_legacy_evaluate: data.fsrs_legacy_evaluate,
        schema_modified: data.schema_modified,
        days_since_last_fsrs_optimize: data.days_since_last_fsrs_optimize,
        form: form::build(col.tr()),
    })
}

fn merge(into: &mut OpChanges, other: OpChanges) {
    let OpChanges {
        card,
        note,
        deck,
        tag,
        notetype,
        config,
        deck_config,
        mtime,
        browser_table,
        browser_sidebar,
        note_text,
        study_queues,
    } = other;
    into.card |= card;
    into.note |= note;
    into.deck |= deck;
    into.tag |= tag;
    into.notetype |= notetype;
    into.config |= config;
    into.deck_config |= deck_config;
    into.mtime |= mtime;
    into.browser_table |= browser_table;
    into.browser_sidebar |= browser_sidebar;
    into.note_text |= note_text;
    into.study_queues |= study_queues;
}

fn save(col: &mut Collection, request: SaveRequest, confirmed: bool) -> Result<OpChanges> {
    use pb::deck_config::UpdateDeckConfigsMode as Mode;
    if request.configs.is_empty() {
        return Err(CoreError::Invalid("No preset to save".into()));
    }
    // Removing a preset marks the schema modified (`remove_deck_config_inner`).
    // Nothing else on this page does: FSRS toggles, memory-state
    // recomputation and rescheduling are normal, incremental changes.
    if !request.removed_preset_ids.is_empty() {
        confirm_schema_change(col, confirmed)?;
    }
    if request.mode == SaveMode::OptimizeAll && !request.globals.fsrs {
        return Err(CoreError::Invalid(plain(
            col.tr().deck_config_fsrs_must_be_enabled(),
        )));
    }
    let mode = match request.mode {
        SaveMode::Normal => Mode::Normal,
        SaveMode::ApplyToChildren => Mode::ApplyToChildren,
        SaveMode::OptimizeAll => Mode::ComputeAllParams,
    };
    let globals = request.globals;
    let mut all = changes(
        DeckConfigService::update_deck_configs(
            col,
            pb::deck_config::UpdateDeckConfigsRequest {
                target_deck_id: request.target_deck_id,
                configs: request.configs.into_iter().map(Into::into).collect(),
                removed_config_ids: request.removed_preset_ids,
                mode: mode as i32,
                card_state_customizer: globals.card_state_customizer,
                limits: Some(request.limits.into()),
                new_cards_ignore_review_limit: globals.new_cards_ignore_review_limit,
                fsrs: globals.fsrs,
                apply_all_parent_limits: globals.apply_all_parent_limits,
                fsrs_reschedule: request.fsrs_reschedule,
                fsrs_health_check: globals.fsrs_health_check,
            },
        )
        .map_err(|e| localized(col, e))?,
    );
    if let Some(wanted) = request.advanced {
        use pb::config::config_key::Bool;
        let current = advanced(col)?;
        for (key, now, want) in [
            (
                Bool::LoadBalancerEnabled,
                current.load_balancer,
                wanted.load_balancer,
            ),
            (
                Bool::FsrsShortTermWithStepsEnabled,
                current.fsrs_short_term_with_steps,
                wanted.fsrs_short_term_with_steps,
            ),
        ] {
            if now != want {
                let set = ConfigService::set_config_bool(
                    col,
                    pb::config::SetConfigBoolRequest {
                        key: key as i32,
                        value: want,
                        undoable: true,
                    },
                )?;
                merge(&mut all, changes(set));
            }
        }
    }
    Ok(all)
}

/// Desktop sends `new Date("YYYY-MM-DD").getTime()`: UTC midnight.
fn ignore_before_ms(date: &str) -> Result<i64> {
    if date.trim().is_empty() {
        return Ok(0);
    }
    chrono::NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d")
        .map(|d| {
            d.and_hms_opt(0, 0, 0)
                .unwrap_or_default()
                .and_utc()
                .timestamp_millis()
        })
        .map_err(|_| CoreError::Invalid(format!("Not a date (YYYY-MM-DD): {date}")))
}

fn search_for(preset_name: &str, config: &PresetConfig) -> String {
    if config.param_search.trim().is_empty() {
        default_param_search(preset_name)
    } else {
        config.param_search.clone()
    }
}

fn optimize(
    col: &mut Collection,
    preset_name: &str,
    config: &PresetConfig,
    health_check: bool,
) -> Result<Optimized> {
    let current = config.fsrs_params().to_vec();
    let response = SchedulerService::compute_fsrs_params(
        col,
        pb::scheduler::ComputeFsrsParamsRequest {
            search: search_for(preset_name, config),
            current_params: current.clone(),
            ignore_revlogs_before_ms: ignore_before_ms(&config.ignore_revlogs_before_date)?,
            num_of_relearning_steps: relearning_steps_in_day(&config.relearn_steps),
            health_check,
        },
    )
    .map_err(|e| localized(col, e))?;
    // Desktop compares to four decimals (`FsrsOptions.svelte`).
    let already_optimal = response.params.is_empty()
        || (!current.is_empty()
            && current.len() == response.params.len()
            && current
                .iter()
                .zip(&response.params)
                .all(|(a, b)| format!("{a:.4}") == format!("{b:.4}")));
    let tr = col.tr();
    let mut message = vec![];
    if already_optimal {
        message.push(if response.fsrs_items > 0 {
            plain(tr.deck_config_fsrs_params_optimal())
        } else {
            plain(tr.deck_config_fsrs_params_no_reviews())
        });
    }
    if let Some(passed) = response.health_check_passed {
        message.push(if passed {
            plain(tr.deck_config_fsrs_good_fit())
        } else {
            plain(tr.deck_config_fsrs_bad_fit_warning())
        });
    }
    Ok(Optimized {
        params: response.params,
        fsrs_items: response.fsrs_items,
        health_check_passed: response.health_check_passed,
        already_optimal,
        message: message.join("\n\n"),
    })
}

impl From<SimulateRequest> for pb::scheduler::SimulateFsrsReviewRequest {
    fn from(r: SimulateRequest) -> Self {
        Self {
            params: r.params,
            desired_retention: r.desired_retention,
            deck_size: r.deck_size,
            days_to_simulate: r.days_to_simulate,
            new_limit: r.new_limit,
            review_limit: r.review_limit,
            max_interval: r.max_interval,
            search: r.search,
            new_cards_ignore_review_limit: r.new_cards_ignore_review_limit,
            easy_days_percentages: r.easy_days_percentages,
            review_order: r.review_order,
            suspend_after_lapse_count: r.suspend_after_lapse_count,
            historical_retention: r.historical_retention,
            learning_step_count: r.learning_step_count,
            relearning_step_count: r.relearning_step_count,
        }
    }
}

pub(super) fn execute(
    col: &mut Collection,
    command: DeckOptionsCommand,
) -> Result<DeckOptionsResponse> {
    Ok(match command {
        DeckOptionsCommand::Load { deck_id } => {
            DeckOptionsResponse::Loaded(Box::new(load(col, deck_id)?))
        }
        DeckOptionsCommand::Save { request, confirmed } => {
            DeckOptionsResponse::Saved(save(col, *request, confirmed)?)
        }
        DeckOptionsCommand::Optimize {
            preset_name,
            config,
            health_check,
        } => DeckOptionsResponse::Optimized(optimize(col, &preset_name, &config, health_check)?),
        DeckOptionsCommand::Evaluate {
            preset_name,
            config,
        } => {
            let r = SchedulerService::evaluate_params_legacy(
                col,
                pb::scheduler::EvaluateParamsLegacyRequest {
                    params: config.fsrs_params().to_vec(),
                    search: search_for(&preset_name, &config),
                    ignore_revlogs_before_ms: ignore_before_ms(&config.ignore_revlogs_before_date)?,
                },
            )
            .map_err(|e| localized(col, e))?;
            DeckOptionsResponse::Evaluated {
                log_loss: r.log_loss,
                rmse_bins: r.rmse_bins,
                message: format!(
                    "Log loss: {:.4}, RMSE(bins): {:.2}%. {}",
                    r.log_loss,
                    r.rmse_bins * 100.0,
                    plain(col.tr().deck_config_smaller_is_better())
                ),
            }
        }
        DeckOptionsCommand::MinimumRetention(request) => {
            let retention = SchedulerService::compute_optimal_retention(col, (*request).into())
                .map_err(|e| localized(col, e))?
                .optimal_retention;
            DeckOptionsResponse::MinimumRetention {
                retention,
                message: plain(
                    col.tr()
                        .deck_config_predicted_optimal_retention(format!("{retention:.2}")),
                ),
            }
        }
        DeckOptionsCommand::Simulate(request) => {
            let r = SchedulerService::simulate_fsrs_review(col, (*request).into())
                .map_err(|e| localized(col, e))?;
            DeckOptionsResponse::Simulated(Simulation {
                accumulated_knowledge_acquisition: r.accumulated_knowledge_acquisition,
                daily_review_count: r.daily_review_count,
                daily_new_count: r.daily_new_count,
                daily_time_cost: r.daily_time_cost,
            })
        }
        DeckOptionsCommand::IgnoredBeforeCount { date, search } => {
            let r = DeckConfigService::get_ignored_before_count(
                col,
                pb::deck_config::GetIgnoredBeforeCountRequest {
                    ignore_revlogs_before_date: date,
                    search,
                },
            )
            .map_err(|e| localized(col, e))?;
            DeckOptionsResponse::IgnoredBefore {
                included: r.included,
                total: r.total,
                message: plain(
                    col.tr().deck_config_ignore_before_info(
                        r.included.to_string(),
                        r.total.to_string(),
                    ),
                ),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignore_before_is_utc_midnight() {
        assert_eq!(ignore_before_ms("").unwrap(), 0);
        assert_eq!(ignore_before_ms("1970-01-02").unwrap(), 86_400_000);
        assert!(ignore_before_ms("02/01/1970").is_err());
    }

    #[test]
    fn default_search_escapes_quotes() {
        assert_eq!(
            default_param_search(r#"My "x" \ y"#),
            r#"preset:"My \"x\" \\ y" -is:suspended"#
        );
        assert_eq!(relearning_steps_in_day(&[10.0, 1440.0, 5.0]), 1);
        assert_eq!(relearning_steps_in_day(&[10.0, 20.0]), 2);
    }

    #[test]
    fn config_round_trips_through_proto() {
        let config = PresetConfig {
            learn_steps: vec![1.0, 10.0],
            review_order: 7,
            other: b"{\"x\":1}".to_vec(),
            ..Default::default()
        };
        let back: PresetConfig = PbConfig::from(config.clone()).into();
        assert_eq!(back, config);
    }
}
