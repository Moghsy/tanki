//! Deck options presets, per-deck limits and FSRS tools (PLAN 8.3).
//!
//! Owned by the Deck options stream (docs/implementation/phase-5-interfaces.md).
//! Reached through `Command::Manage(ManageCommand::DeckOptions(…))`.
//!
//! The page is loaded and saved with Anki's two calls, as desktop does:
//! `get_deck_configs_for_update` ([`DeckOptionsCommand::Load`]) and
//! `update_deck_configs` ([`DeckOptionsCommand::Save`]). Preset add / clone /
//! rename / delete / switch happen in the editing model
//! ([`model::DeckOptionsState`]) and reach the collection on save, as on
//! desktop's page. The form description ([`form::Form`]: groups, labels,
//! ranges, choices, warnings) is built from Anki's localised strings.
//!
//! Slow commands (`Save`, `Optimize`, `Evaluate`, `MinimumRetention`,
//! `Simulate`) are meant for `CollectionActor::start` with `progress()` and
//! `cancel_operation()`.
mod convert;
pub mod form;
pub mod model;
pub mod steps;

use crate::{OpChanges, Result};
use anki::collection::Collection;
use serde::{Deserialize, Serialize};

/// A preset's options: every field of Anki's `DeckConfig.Config`. Enum
/// fields hold Anki's proto values (see the choices in [`form`]).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PresetConfig {
    /// Minutes.
    pub learn_steps: Vec<f32>,
    /// Minutes.
    pub relearn_steps: Vec<f32>,
    pub fsrs_params_4: Vec<f32>,
    pub fsrs_params_5: Vec<f32>,
    pub fsrs_params_6: Vec<f32>,
    pub new_per_day: u32,
    pub reviews_per_day: u32,
    pub new_per_day_minimum: u32,
    pub initial_ease: f32,
    pub easy_multiplier: f32,
    pub hard_multiplier: f32,
    pub lapse_multiplier: f32,
    pub interval_multiplier: f32,
    pub maximum_review_interval: u32,
    pub minimum_lapse_interval: u32,
    pub graduating_interval_good: u32,
    pub graduating_interval_easy: u32,
    pub new_card_insert_order: i32,
    pub new_card_gather_priority: i32,
    pub new_card_sort_order: i32,
    pub new_mix: i32,
    pub review_order: i32,
    pub interday_learning_mix: i32,
    pub leech_action: i32,
    pub leech_threshold: u32,
    pub disable_autoplay: bool,
    pub cap_answer_time_to_secs: u32,
    pub show_timer: bool,
    pub stop_timer_on_answer: bool,
    pub seconds_to_show_question: f32,
    pub seconds_to_show_answer: f32,
    pub question_action: i32,
    pub answer_action: i32,
    pub wait_for_audio: bool,
    pub skip_question_when_replaying_answer: bool,
    pub bury_new: bool,
    pub bury_reviews: bool,
    pub bury_interday_learning: bool,
    pub desired_retention: f32,
    /// "YYYY-MM-DD" or empty.
    pub ignore_revlogs_before_date: String,
    /// Monday..Sunday; 1.0 normal, 0.5 reduced, 0.0 minimum.
    pub easy_days_percentages: Vec<f32>,
    pub historical_retention: f32,
    pub param_search: String,
    /// Legacy JSON keys Anki keeps opaque; preserved unchanged.
    pub other: Vec<u8>,
}

impl PresetConfig {
    /// The FSRS parameters in use: v6, else v5, else v4 (desktop's
    /// `fsrsParams`). Empty means Anki's defaults.
    pub fn fsrs_params(&self) -> &[f32] {
        if !self.fsrs_params_6.is_empty() {
            &self.fsrs_params_6
        } else if !self.fsrs_params_5.is_empty() {
            &self.fsrs_params_5
        } else {
            &self.fsrs_params_4
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Preset {
    /// 0 for a preset added in the editor and not saved yet.
    pub id: i64,
    pub name: String,
    pub mtime_secs: i64,
    pub usn: i32,
    pub config: PresetConfig,
    /// Normal decks using it, as loaded.
    pub use_count: u32,
}

/// Per-deck overrides of the target deck ("This deck" / "Today only").
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DeckLimits {
    pub review: Option<u32>,
    pub new: Option<u32>,
    pub review_today: Option<u32>,
    pub new_today: Option<u32>,
    /// `review_today` applies to today (else it is a past day's value).
    pub review_today_active: bool,
    pub new_today_active: bool,
    /// Per-deck FSRS desired retention.
    pub desired_retention: Option<f32>,
}

/// Collection-wide settings shown on the page (desktop's "global" label).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GlobalOptions {
    pub new_cards_ignore_review_limit: bool,
    pub apply_all_parent_limits: bool,
    pub fsrs: bool,
    pub fsrs_health_check: bool,
    /// The custom scheduling JavaScript, run in QuickJS (PLAN 8.1).
    pub card_state_customizer: String,
}

/// Collection settings with backend fields but no desktop UI (PLAN 8.3),
/// shown in the "advanced" group and saved with `SetConfigBool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AdvancedToggles {
    pub load_balancer: bool,
    pub fsrs_short_term_with_steps: bool,
}

/// Everything the deck options page shows (`DeckConfigsForUpdate` plus the
/// advanced toggles and the localised form).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeckOptionsData {
    pub deck_id: i64,
    pub deck_name: String,
    /// The preset the deck uses.
    pub deck_preset_id: i64,
    /// Presets used by parent decks.
    pub parent_preset_ids: Vec<i64>,
    /// Sorted by name.
    pub presets: Vec<Preset>,
    /// Anki's default preset options (with default FSRS parameters).
    pub defaults: PresetConfig,
    pub limits: DeckLimits,
    pub globals: GlobalOptions,
    pub advanced: AdvancedToggles,
    pub fsrs_legacy_evaluate: bool,
    /// The schema already changed since the last sync: removing a preset
    /// does not add a full sync.
    pub schema_modified: bool,
    pub days_since_last_fsrs_optimize: u32,
    /// Set when the collection has a custom scheduling script, which tanki
    /// runs in QuickJS (PLAN 8.1).
    pub custom_scheduling_warning: Option<String>,
    pub form: form::Form,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SaveMode {
    #[default]
    Normal,
    /// "Save to All Subdecks".
    ApplyToChildren,
    /// "Optimize All Presets": optimises every preset, then saves. Slow.
    OptimizeAll,
}

/// `UpdateDeckConfigsRequest`. Build it with
/// [`model::DeckOptionsState::save_request`].
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SaveRequest {
    pub target_deck_id: i64,
    /// New (id 0) and changed presets; the deck gets the **last** one.
    pub configs: Vec<Preset>,
    pub removed_preset_ids: Vec<i64>,
    pub mode: SaveMode,
    pub globals: GlobalOptions,
    pub limits: DeckLimits,
    /// "Reschedule cards on change" for this save.
    pub fsrs_reschedule: bool,
    /// Written with `SetConfigBool` when it differs from the collection.
    pub advanced: Option<AdvancedToggles>,
}

/// `SimulateFsrsReviewRequest`, also used for the minimum recommended
/// retention. Build it with [`model::DeckOptionsState::simulate_request`].
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SimulateRequest {
    pub params: Vec<f32>,
    pub desired_retention: f32,
    /// Additional new cards to simulate.
    pub deck_size: u32,
    pub days_to_simulate: u32,
    pub new_limit: u32,
    pub review_limit: u32,
    pub max_interval: u32,
    pub search: String,
    pub new_cards_ignore_review_limit: bool,
    pub easy_days_percentages: Vec<f32>,
    pub review_order: i32,
    pub suspend_after_lapse_count: Option<u32>,
    pub historical_retention: f32,
    pub learning_step_count: u32,
    pub relearning_step_count: u32,
}

/// `SimulateFsrsReviewResponse`: one value per simulated day.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Simulation {
    pub accumulated_knowledge_acquisition: Vec<f32>,
    pub daily_review_count: Vec<u32>,
    pub daily_new_count: Vec<u32>,
    /// Seconds.
    pub daily_time_cost: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Optimized {
    pub params: Vec<f32>,
    pub fsrs_items: u32,
    pub health_check_passed: Option<bool>,
    /// The parameters did not change (or no reviews were found): keep the
    /// current ones.
    pub already_optimal: bool,
    /// Desktop's message after optimising (localised), or empty.
    pub message: String,
}

/// Each variant's doc comment names its `DeckOptionsResponse` variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeckOptionsCommand {
    /// The page for a normal deck -> `Loaded`.
    Load { deck_id: i64 },
    /// Saves the page -> `Saved`. Removing a preset forces a full sync:
    /// unconfirmed, it fails with `CoreError::SchemaChangeRequired`. Can be
    /// slow (FSRS memory states; `SaveMode::OptimizeAll` trains every
    /// preset): run it with `CollectionActor::start`.
    Save {
        request: Box<SaveRequest>,
        confirmed: bool,
    },
    /// "Optimize Current Preset" (`ComputeFsrsParams`) on the preset's
    /// search, or desktop's default `preset:"<name>" -is:suspended` ->
    /// `Optimized`. Long operation.
    Optimize {
        preset_name: String,
        config: Box<PresetConfig>,
        health_check: bool,
    },
    /// "Evaluate" (`EvaluateParamsLegacy`) of the preset's current
    /// parameters -> `Evaluated`. Long operation.
    Evaluate {
        preset_name: String,
        config: Box<PresetConfig>,
    },
    /// "Compute minimum recommended retention" (`ComputeOptimalRetention`)
    /// -> `MinimumRetention`. Long operation.
    MinimumRetention(Box<SimulateRequest>),
    /// The FSRS simulator (`SimulateFsrsReview`) -> `Simulated`. Long
    /// operation.
    Simulate(Box<SimulateRequest>),
    /// Cards an "ignore reviews before" date keeps (`GetIgnoredBeforeCount`)
    /// -> `IgnoredBefore`.
    IgnoredBeforeCount { date: String, search: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeckOptionsResponse {
    Loaded(Box<DeckOptionsData>),
    Saved(OpChanges),
    Optimized(Optimized),
    Evaluated {
        log_loss: f32,
        rmse_bins: f32,
        /// Desktop's "Log loss: …, RMSE(bins): …%. Smaller numbers …".
        message: String,
    },
    MinimumRetention {
        retention: f32,
        /// "Minimum recommended retention: 0.85" (localised).
        message: String,
    },
    Simulated(Simulation),
    IgnoredBefore {
        included: u64,
        total: u64,
        /// "(Approximately) N/M cards will be used …" (localised).
        message: String,
    },
}

impl DeckOptionsResponse {
    /// The changes a mutating command made; the actor broadcasts these.
    pub fn changes(&self) -> Option<&OpChanges> {
        match self {
            Self::Saved(changes) => Some(changes),
            _ => None,
        }
    }
}

pub(crate) fn execute(
    col: &mut Collection,
    command: DeckOptionsCommand,
) -> Result<DeckOptionsResponse> {
    convert::execute(col, command)
}

/// Desktop's default optimiser search for a preset.
pub fn default_param_search(preset_name: &str) -> String {
    let escaped = preset_name.replace('\\', "\\\\").replace('"', "\\\"");
    format!("preset:\"{escaped}\" -is:suspended")
}

/// Relearning steps that fit within one day, as desktop's optimiser counts
/// them (`FsrsOptions.svelte`).
pub fn relearning_steps_in_day(relearn_steps: &[f32]) -> u32 {
    let mut total = 0.0;
    let mut count = 0;
    for step in relearn_steps {
        total += step;
        if total >= 1440.0 {
            break;
        }
        count += 1;
    }
    count
}
