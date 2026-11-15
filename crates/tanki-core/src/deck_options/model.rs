//! The deck options page's editing state, a port of desktop's
//! `DeckOptionsState` (`ts/routes/deck-options/lib.ts`): the preset list
//! with add / clone / rename / remove / switch, the per-deck limit tabs
//! (`TabbedValue.svelte`), the global toggles, dirty tracking, the save
//! request (selected preset last), and the warnings the page shows. Plain
//! Rust: no collection access, usable by any front end.
use super::form::{Choice, FieldId, FieldKind, FieldSpec, Form, Scope, Shown};
use super::steps::{steps_to_string, string_to_steps};
use super::*;
use std::collections::HashMap;

/// A setting's value as the form edits it.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    Int(u32),
    Float(f32),
    Bool(bool),
    Choice(i32),
    /// Steps, parameters, search, date and script, as text.
    Text(String),
}

const EASY_DAYS_DEFAULT: [f32; 7] = [1.0; 7];

#[derive(Debug, Clone)]
pub struct DeckOptionsState {
    original: DeckOptionsData,
    /// `original.limits` as the tabs start: a "today only" value of a past
    /// day is not active.
    original_limits: DeckLimits,
    presets: Vec<Preset>,
    selected: usize,
    removed: Vec<i64>,
    limits: DeckLimits,
    globals: GlobalOptions,
    advanced: AdvancedToggles,
    fsrs_reschedule: bool,
    schema_modified: bool,
    scopes: HashMap<FieldId, Scope>,
    /// Last value of each tab, used when the tab is enabled again
    /// (`ValueTab.value` / `startValue`).
    tab_memory: HashMap<(FieldId, Scope), f64>,
}

impl DeckOptionsState {
    pub fn new(data: DeckOptionsData) -> Self {
        let mut limits = data.limits.clone();
        if !limits.new_today_active {
            limits.new_today = None;
        }
        if !limits.review_today_active {
            limits.review_today = None;
        }
        let mut presets = data.presets.clone();
        presets.sort_by(|a, b| a.name.cmp(&b.name));
        let selected = presets
            .iter()
            .position(|p| p.id == data.deck_preset_id)
            .unwrap_or(0);
        let mut tab_memory = HashMap::new();
        let mut remember = |id, scope, value: Option<f64>| {
            if let Some(v) = value {
                tab_memory.insert((id, scope), v);
            }
        };
        remember(
            FieldId::NewPerDay,
            Scope::Deck,
            data.limits.new.map(f64::from),
        );
        remember(
            FieldId::NewPerDay,
            Scope::Today,
            data.limits.new_today.map(f64::from),
        );
        remember(
            FieldId::ReviewsPerDay,
            Scope::Deck,
            data.limits.review.map(f64::from),
        );
        remember(
            FieldId::ReviewsPerDay,
            Scope::Today,
            data.limits.review_today.map(f64::from),
        );
        remember(
            FieldId::DesiredRetention,
            Scope::Deck,
            data.limits.desired_retention.map(f64::from),
        );
        let mut scopes = HashMap::new();
        scopes.insert(
            FieldId::NewPerDay,
            last_set(&[true, limits.new.is_some(), limits.new_today.is_some()]),
        );
        scopes.insert(
            FieldId::ReviewsPerDay,
            last_set(&[true, limits.review.is_some(), limits.review_today.is_some()]),
        );
        scopes.insert(
            FieldId::DesiredRetention,
            last_set(&[true, limits.desired_retention.is_some(), false]),
        );
        Self {
            original_limits: limits.clone(),
            presets,
            selected,
            removed: vec![],
            globals: data.globals.clone(),
            advanced: data.advanced,
            fsrs_reschedule: false,
            schema_modified: data.schema_modified,
            limits,
            scopes,
            tab_memory,
            original: data,
        }
    }

    pub fn data(&self) -> &DeckOptionsData {
        &self.original
    }

    pub fn form(&self) -> &Form {
        &self.original.form
    }

    pub fn presets(&self) -> &[Preset] {
        &self.presets
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn current(&self) -> &Preset {
        &self.presets[self.selected]
    }

    fn config(&self) -> &PresetConfig {
        &self.presets[self.selected].config
    }

    fn config_mut(&mut self) -> &mut PresetConfig {
        &mut self.presets[self.selected].config
    }

    pub fn globals(&self) -> &GlobalOptions {
        &self.globals
    }

    pub fn limits(&self) -> &DeckLimits {
        &self.limits
    }

    pub fn advanced(&self) -> AdvancedToggles {
        self.advanced
    }

    pub fn fsrs(&self) -> bool {
        self.globals.fsrs
    }

    pub fn fsrs_reschedule(&self) -> bool {
        self.fsrs_reschedule
    }

    /// Decks using preset `index` if the target deck is saved with the
    /// selected preset (desktop's adjusted use counts).
    pub fn use_count(&self, index: usize) -> u32 {
        let preset = &self.presets[index];
        let mut count = preset.use_count;
        if preset.id != 0 && preset.id == self.original.deck_preset_id {
            count = count.saturating_sub(1);
        }
        if index == self.selected {
            count += 1;
        }
        count
    }

    /// "Default (used by 3 decks)".
    pub fn preset_label(&self, index: usize) -> String {
        let used = self
            .form()
            .strings
            .used_by_decks
            .format(self.use_count(index), "");
        format!("{} ({used})", self.presets[index].name)
    }

    pub fn select(&mut self, index: usize) {
        if index < self.presets.len() {
            self.selected = index;
        }
    }

    /// The deck would be saved with another preset than it has, or a new
    /// one: desktop asks to save before optimising.
    pub fn assignments_changed(&self) -> bool {
        let current = self.current();
        current.id == 0 || current.id != self.original.deck_preset_id
    }

    fn unique_name(&self, name: &str) -> String {
        let name = name.trim();
        if self.presets.iter().any(|p| p.name == name) {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default();
            format!("{name}{secs}")
        } else {
            name.to_owned()
        }
    }

    fn sort_keeping_selection(&mut self) {
        let marker = self.selected;
        let mut indexed: Vec<(usize, Preset)> = std::mem::take(&mut self.presets)
            .into_iter()
            .enumerate()
            .collect();
        indexed.sort_by(|a, b| a.1.name.cmp(&b.1.name));
        self.selected = indexed.iter().position(|(i, _)| *i == marker).unwrap_or(0);
        self.presets = indexed.into_iter().map(|(_, p)| p).collect();
    }

    fn add_from(&mut self, name: &str, config: PresetConfig) {
        let name = self.unique_name(name);
        self.presets.push(Preset {
            id: 0,
            name,
            config,
            ..Default::default()
        });
        self.selected = self.presets.len() - 1;
        self.sort_keeping_selection();
    }

    /// "Add Preset": Anki's defaults, made current.
    pub fn add_preset(&mut self, name: &str) {
        self.add_from(name, self.original.defaults.clone());
    }

    /// "Clone Preset": a copy of the current preset, made current.
    pub fn clone_preset(&mut self, name: &str) {
        self.add_from(name, self.config().clone());
    }

    /// "Rename Preset"; a taken name gets a suffix, as on desktop.
    pub fn rename_preset(&mut self, name: &str) {
        if name.trim().is_empty() || self.current().name == name.trim() {
            return;
        }
        let name = self.unique_name(name);
        self.presets[self.selected].name = name;
        self.sort_keeping_selection();
    }

    /// Removing the current preset makes the next save a full sync.
    pub fn removal_needs_full_sync(&self) -> bool {
        !self.schema_modified && self.current().id != 0
    }

    pub fn can_remove(&self) -> bool {
        self.current().id != 1
    }

    /// "Remove Preset". Fails for the default preset (Anki's message).
    pub fn remove_preset(&mut self) -> std::result::Result<(), String> {
        if !self.can_remove() {
            return Err(self.form().strings.default_cant_be_removed.clone());
        }
        let removed = self.presets.remove(self.selected);
        if removed.id != 0 {
            self.removed.push(removed.id);
            self.schema_modified = true;
        }
        self.selected = self.selected.saturating_sub(1);
        Ok(())
    }

    pub fn removed(&self) -> &[i64] {
        &self.removed
    }

    fn original_preset(&self, id: i64) -> Option<&Preset> {
        (id != 0)
            .then(|| self.original.presets.iter().find(|p| p.id == id))
            .flatten()
    }

    fn preset_changed(&self, preset: &Preset) -> bool {
        match self.original_preset(preset.id) {
            Some(original) => original.name != preset.name || original.config != preset.config,
            None => true,
        }
    }

    /// Anything to save.
    pub fn is_dirty(&self) -> bool {
        !self.removed.is_empty()
            || self.assignments_changed()
            || self.presets.iter().any(|p| self.preset_changed(p))
            || self.limits != self.original_limits
            || self.globals != self.original.globals
            || self.advanced != self.original.advanced
    }

    /// The current preset's FSRS parameters are the saved ones (desktop
    /// runs the simulator only then).
    pub fn params_saved(&self) -> bool {
        self.original_preset(self.current().id)
            .is_some_and(|p| p.config.fsrs_params() == self.config().fsrs_params())
    }

    /// `UpdateDeckConfigsRequest`: new and changed presets, then the
    /// selected one last, as desktop's `dataForSaving`.
    pub fn save_request(&self, mode: SaveMode) -> SaveRequest {
        let mut configs: Vec<Preset> = self
            .presets
            .iter()
            .enumerate()
            .filter(|(i, p)| *i != self.selected && self.preset_changed(p))
            .map(|(_, p)| p.clone())
            .collect();
        configs.push(self.current().clone());
        let mut limits = self.limits.clone();
        limits.new_today_active = limits.new_today.is_some();
        limits.review_today_active = limits.review_today.is_some();
        SaveRequest {
            target_deck_id: self.original.deck_id,
            configs,
            removed_preset_ids: self.removed.clone(),
            mode,
            globals: self.globals.clone(),
            limits,
            fsrs_reschedule: self.fsrs_reschedule,
            advanced: (self.advanced != self.original.advanced).then_some(self.advanced),
        }
    }

    /// The simulator's request from the current preset, as desktop's
    /// `simulateFsrsRequest` plus the simulator's own inputs.
    pub fn simulate_request(
        &self,
        days_to_simulate: u32,
        additional_new_cards: u32,
        suspend_leeches: bool,
    ) -> SimulateRequest {
        let c = self.config();
        SimulateRequest {
            params: c.fsrs_params().to_vec(),
            desired_retention: c.desired_retention,
            deck_size: additional_new_cards,
            days_to_simulate,
            new_limit: c.new_per_day,
            review_limit: c.reviews_per_day,
            max_interval: c.maximum_review_interval,
            search: default_param_search(&self.current().name),
            new_cards_ignore_review_limit: self.globals.new_cards_ignore_review_limit,
            easy_days_percentages: self.easy_days(),
            review_order: c.review_order,
            suspend_after_lapse_count: suspend_leeches.then_some(c.leech_threshold),
            historical_retention: c.historical_retention,
            learning_step_count: c.learn_steps.len() as u32,
            relearning_step_count: c.relearn_steps.len() as u32,
        }
    }

    /// Takes optimised parameters unless they are already optimal.
    pub fn apply_optimized(&mut self, optimized: &Optimized) {
        if !optimized.already_optimal {
            self.config_mut().fsrs_params_6 = optimized.params.clone();
        }
    }

    pub fn set_custom_scheduling(&mut self, script: String) {
        self.globals.card_state_customizer = script;
    }

    fn easy_days(&self) -> Vec<f32> {
        let days = &self.config().easy_days_percentages;
        if days.len() == 7 {
            days.clone()
        } else {
            EASY_DAYS_DEFAULT.to_vec()
        }
    }

    pub fn spec(&self, id: FieldId) -> Option<&FieldSpec> {
        self.original.form.spec(id)
    }

    /// Shown with the current FSRS setting.
    pub fn visible(&self, spec: &FieldSpec) -> bool {
        match spec.shown {
            Shown::Always => true,
            Shown::Fsrs => self.fsrs(),
            Shown::Sm2 => !self.fsrs(),
        }
    }

    /// The choices offered now: the FSRS list for review order, and new
    /// card sort orders desktop disables for some gather orders.
    pub fn choices(&self, id: FieldId) -> Vec<Choice> {
        let Some(FieldKind::Choice { choices, fsrs }) = self.spec(id).map(|s| &s.kind) else {
            return vec![];
        };
        let list = match (self.fsrs(), fsrs) {
            (true, Some(fsrs)) => fsrs.clone(),
            _ => choices.clone(),
        };
        if id == FieldId::NewCardSortOrder {
            let disabled = disabled_sort_orders(self.config().new_card_gather_priority);
            return list
                .into_iter()
                .filter(|c| !disabled.contains(&c.value))
                .collect();
        }
        list
    }

    /// The active tab of a tabbed value (`Scope::Preset` otherwise).
    pub fn scope(&self, id: FieldId) -> Scope {
        self.scopes.get(&id).copied().unwrap_or(Scope::Preset)
    }

    /// Switches a tabbed value's tab, as `TabbedValue.svelte`: the new tab
    /// takes its last value or the value shown now; later tabs are cleared.
    pub fn set_scope(&mut self, id: FieldId, scope: Scope) {
        let scopes = self.spec(id).map(|s| s.scopes.clone()).unwrap_or_default();
        let Some(new_index) = scopes.iter().position(|s| *s == scope) else {
            return;
        };
        let shown = self.number(id);
        for (index, tab) in scopes.iter().enumerate() {
            if index == new_index {
                let value = self.tab_memory.get(&(id, *tab)).copied().unwrap_or(shown);
                self.set_tab(id, *tab, Some(value));
            } else if index > new_index {
                self.set_tab(id, *tab, None);
            }
        }
        self.scopes.insert(id, scope);
    }

    /// Cycles through a tabbed value's tabs.
    pub fn next_scope(&mut self, id: FieldId) {
        let scopes = self.spec(id).map(|s| s.scopes.clone()).unwrap_or_default();
        if scopes.is_empty() {
            return;
        }
        let index = scopes
            .iter()
            .position(|s| *s == self.scope(id))
            .unwrap_or(0);
        self.set_scope(id, scopes[(index + 1) % scopes.len()]);
    }

    fn set_tab(&mut self, id: FieldId, tab: Scope, value: Option<f64>) {
        let int = value.map(|v| v.round().clamp(0.0, 9999.0) as u32);
        match (id, tab) {
            (FieldId::NewPerDay, Scope::Preset) => {
                if let Some(v) = int {
                    self.config_mut().new_per_day = v;
                }
            }
            (FieldId::NewPerDay, Scope::Deck) => self.limits.new = int,
            (FieldId::NewPerDay, Scope::Today) => self.limits.new_today = int,
            (FieldId::ReviewsPerDay, Scope::Preset) => {
                if let Some(v) = int {
                    self.config_mut().reviews_per_day = v;
                }
            }
            (FieldId::ReviewsPerDay, Scope::Deck) => self.limits.review = int,
            (FieldId::ReviewsPerDay, Scope::Today) => self.limits.review_today = int,
            (FieldId::DesiredRetention, Scope::Preset) => {
                if let Some(v) = value {
                    self.config_mut().desired_retention = v as f32;
                }
            }
            (FieldId::DesiredRetention, _) => {
                self.limits.desired_retention = value.map(|v| v as f32)
            }
            _ => {}
        }
        if let Some(v) = value {
            if tab != Scope::Preset {
                self.tab_memory.insert((id, tab), v);
            }
        }
    }

    fn number(&self, id: FieldId) -> f64 {
        match self.value(id) {
            FieldValue::Int(v) => v.into(),
            FieldValue::Float(v) => v.into(),
            FieldValue::Choice(v) => v.into(),
            FieldValue::Bool(v) => u8::from(v).into(),
            FieldValue::Text(_) => 0.0,
        }
    }

    /// The value shown now (for tabbed values, the active tab's).
    pub fn value(&self, id: FieldId) -> FieldValue {
        use FieldValue::*;
        let c = self.config();
        let scope = self.scope(id);
        match id {
            FieldId::NewPerDay => Int(match scope {
                Scope::Preset => c.new_per_day,
                Scope::Deck => self.limits.new.unwrap_or(c.new_per_day),
                Scope::Today => self.limits.new_today.unwrap_or(c.new_per_day),
            }),
            FieldId::ReviewsPerDay => Int(match scope {
                Scope::Preset => c.reviews_per_day,
                Scope::Deck => self.limits.review.unwrap_or(c.reviews_per_day),
                Scope::Today => self.limits.review_today.unwrap_or(c.reviews_per_day),
            }),
            FieldId::DesiredRetention => Float(match scope {
                Scope::Preset => c.desired_retention,
                _ => self.limits.desired_retention.unwrap_or(c.desired_retention),
            }),
            FieldId::NewCardsIgnoreReviewLimit => Bool(self.globals.new_cards_ignore_review_limit),
            FieldId::ApplyAllParentLimits => Bool(self.globals.apply_all_parent_limits),
            FieldId::Fsrs => Bool(self.globals.fsrs),
            FieldId::FsrsReschedule => Bool(self.fsrs_reschedule),
            FieldId::FsrsHealthCheck => Bool(self.globals.fsrs_health_check),
            FieldId::CustomScheduling => Text(self.globals.card_state_customizer.clone()),
            FieldId::LoadBalancer => Bool(self.advanced.load_balancer),
            FieldId::FsrsShortTermWithSteps => Bool(self.advanced.fsrs_short_term_with_steps),
            other => preset_value(c, other),
        }
    }

    /// Desktop's default for the setting ("revert" button).
    pub fn default_value(&self, id: FieldId) -> FieldValue {
        use FieldValue::*;
        let d = &self.original.defaults;
        match id {
            FieldId::NewPerDay => Int(d.new_per_day),
            FieldId::ReviewsPerDay => Int(d.reviews_per_day),
            FieldId::DesiredRetention => Float(d.desired_retention),
            FieldId::NewCardsIgnoreReviewLimit
            | FieldId::ApplyAllParentLimits
            | FieldId::Fsrs
            | FieldId::FsrsReschedule
            | FieldId::FsrsHealthCheck
            | FieldId::FsrsShortTermWithSteps => Bool(false),
            FieldId::LoadBalancer => Bool(true),
            FieldId::FsrsParams => Text(String::new()),
            FieldId::CustomScheduling => Text(String::new()),
            FieldId::EasyDay(_) => Float(1.0),
            other => preset_value(d, other),
        }
    }

    pub fn is_default(&self, id: FieldId) -> bool {
        self.value(id) == self.default_value(id)
    }

    /// Restores the default ("revert"), in the active tab.
    pub fn reset(&mut self, id: FieldId) {
        let value = self.default_value(id);
        let _ = self.set_value(id, value);
    }

    /// Sets a value, clamped to the setting's range. Errors carry a message
    /// for the user.
    pub fn set_value(&mut self, id: FieldId, value: FieldValue) -> std::result::Result<(), String> {
        let kind = self
            .spec(id)
            .map(|s| s.kind.clone())
            .ok_or_else(|| format!("Unknown setting {id:?}"))?;
        let value = clamp(&kind, value);
        let scope = self.scope(id);
        use FieldValue::*;
        match (id, value) {
            (FieldId::NewPerDay | FieldId::ReviewsPerDay, Int(v)) => {
                self.set_tab(id, scope, Some(v.into()))
            }
            (FieldId::DesiredRetention, Float(v)) => self.set_tab(id, scope, Some(v.into())),
            (FieldId::NewCardsIgnoreReviewLimit, Bool(v)) => {
                self.globals.new_cards_ignore_review_limit = v
            }
            (FieldId::ApplyAllParentLimits, Bool(v)) => self.globals.apply_all_parent_limits = v,
            (FieldId::Fsrs, Bool(v)) => self.globals.fsrs = v,
            (FieldId::FsrsReschedule, Bool(v)) => self.fsrs_reschedule = v,
            (FieldId::FsrsHealthCheck, Bool(v)) => self.globals.fsrs_health_check = v,
            (FieldId::LoadBalancer, Bool(v)) => self.advanced.load_balancer = v,
            (FieldId::FsrsShortTermWithSteps, Bool(v)) => {
                self.advanced.fsrs_short_term_with_steps = v
            }
            (FieldId::CustomScheduling, Text(v)) => self.globals.card_state_customizer = v,
            (FieldId::LearnSteps, Text(v)) => self.config_mut().learn_steps = string_to_steps(&v),
            (FieldId::RelearnSteps, Text(v)) => {
                self.config_mut().relearn_steps = string_to_steps(&v)
            }
            (FieldId::FsrsParams, Text(v)) => {
                let params = string_to_params(&v)
                    .ok_or_else(|| self.form().strings.invalid_params.clone())?;
                let c = self.config_mut();
                c.fsrs_params_6 = params;
            }
            (FieldId::ParamSearch, Text(v)) => self.config_mut().param_search = v.trim().into(),
            (FieldId::IgnoreRevlogsBeforeDate, Text(v)) => {
                let v = v.trim();
                if !v.is_empty() && chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d").is_err() {
                    return Err(format!("Not a date (YYYY-MM-DD): {v}"));
                }
                self.config_mut().ignore_revlogs_before_date = v.into();
            }
            (FieldId::EasyDay(day), Float(v)) => {
                let mut days = self.easy_days();
                days[usize::from(day.min(6))] = v;
                self.config_mut().easy_days_percentages = days;
            }
            (FieldId::NewCardGatherPriority, Choice(v)) => {
                let c = self.config_mut();
                c.new_card_gather_priority = v;
                // Desktop resets a sort order the gather order disables.
                if disabled_sort_orders(v).contains(&c.new_card_sort_order) {
                    c.new_card_sort_order = 0;
                }
            }
            (id, value) => self.set_config_value(id, value)?,
        }
        Ok(())
    }

    fn set_config_value(
        &mut self,
        id: FieldId,
        value: FieldValue,
    ) -> std::result::Result<(), String> {
        use FieldValue::*;
        let c = self.config_mut();
        match (id, value) {
            (FieldId::GraduatingIntervalGood, Int(v)) => c.graduating_interval_good = v,
            (FieldId::GraduatingIntervalEasy, Int(v)) => c.graduating_interval_easy = v,
            (FieldId::MinimumLapseInterval, Int(v)) => c.minimum_lapse_interval = v,
            (FieldId::LeechThreshold, Int(v)) => c.leech_threshold = v,
            (FieldId::CapAnswerTimeToSecs, Int(v)) => c.cap_answer_time_to_secs = v,
            (FieldId::MaximumReviewInterval, Int(v)) => c.maximum_review_interval = v,
            (FieldId::NewCardInsertOrder, Choice(v)) => c.new_card_insert_order = v,
            (FieldId::LeechAction, Choice(v)) => c.leech_action = v,
            (FieldId::NewCardSortOrder, Choice(v)) => c.new_card_sort_order = v,
            (FieldId::NewMix, Choice(v)) => c.new_mix = v,
            (FieldId::InterdayLearningMix, Choice(v)) => c.interday_learning_mix = v,
            (FieldId::ReviewOrder, Choice(v)) => c.review_order = v,
            (FieldId::QuestionAction, Choice(v)) => c.question_action = v,
            (FieldId::AnswerAction, Choice(v)) => c.answer_action = v,
            (FieldId::BuryNew, Bool(v)) => c.bury_new = v,
            (FieldId::BuryReviews, Bool(v)) => c.bury_reviews = v,
            (FieldId::BuryInterdayLearning, Bool(v)) => c.bury_interday_learning = v,
            (FieldId::DisableAutoplay, Bool(v)) => c.disable_autoplay = v,
            (FieldId::SkipQuestionWhenReplayingAnswer, Bool(v)) => {
                c.skip_question_when_replaying_answer = v
            }
            (FieldId::ShowTimer, Bool(v)) => c.show_timer = v,
            (FieldId::StopTimerOnAnswer, Bool(v)) => c.stop_timer_on_answer = v,
            (FieldId::WaitForAudio, Bool(v)) => c.wait_for_audio = v,
            (FieldId::SecondsToShowQuestion, Float(v)) => c.seconds_to_show_question = v,
            (FieldId::SecondsToShowAnswer, Float(v)) => c.seconds_to_show_answer = v,
            (FieldId::InitialEase, Float(v)) => c.initial_ease = v,
            (FieldId::EasyMultiplier, Float(v)) => c.easy_multiplier = v,
            (FieldId::IntervalMultiplier, Float(v)) => c.interval_multiplier = v,
            (FieldId::HardMultiplier, Float(v)) => c.hard_multiplier = v,
            (FieldId::LapseMultiplier, Float(v)) => c.lapse_multiplier = v,
            (FieldId::HistoricalRetention, Float(v)) => c.historical_retention = v,
            (id, value) => return Err(format!("{value:?} does not fit {id:?}")),
        }
        Ok(())
    }

    /// The text an editor starts with.
    pub fn edit_text(&self, id: FieldId) -> String {
        let percent = matches!(
            self.spec(id).map(|s| &s.kind),
            Some(FieldKind::Float { percent: true, .. })
        );
        match self.value(id) {
            FieldValue::Int(v) => v.to_string(),
            FieldValue::Float(v) if percent => format_float(v * 100.0),
            FieldValue::Float(v) => format_float(v),
            FieldValue::Bool(v) => if v { "on" } else { "off" }.into(),
            FieldValue::Choice(v) => self
                .choices(id)
                .into_iter()
                .find(|c| c.value == v)
                .map_or(v.to_string(), |c| c.label),
            FieldValue::Text(v) => v,
        }
    }

    /// Parses typed text into the setting.
    pub fn set_text(&mut self, id: FieldId, text: &str) -> std::result::Result<(), String> {
        let kind = self
            .spec(id)
            .map(|s| s.kind.clone())
            .ok_or_else(|| format!("Unknown setting {id:?}"))?;
        let number = || -> std::result::Result<f64, String> {
            text.trim()
                .trim_end_matches('%')
                .trim()
                .parse::<f64>()
                .map_err(|_| format!("Not a number: {}", text.trim()))
        };
        let value = match kind {
            FieldKind::Int { .. } => FieldValue::Int(number()?.round().max(0.0) as u32),
            FieldKind::Float { percent: true, .. } => {
                let v = number()?;
                FieldValue::Float((if v > 1.0 { v / 100.0 } else { v }) as f32)
            }
            FieldKind::Float { .. } | FieldKind::EasyDay => FieldValue::Float(number()? as f32),
            FieldKind::Bool => FieldValue::Bool(matches!(
                text.trim().to_lowercase().as_str(),
                "on" | "yes" | "true" | "1" | "x"
            )),
            FieldKind::Choice { .. } => {
                let wanted = text.trim().to_lowercase();
                let choice = self
                    .choices(id)
                    .into_iter()
                    .find(|c| c.label.to_lowercase() == wanted || c.value.to_string() == wanted)
                    .ok_or_else(|| format!("Not a choice: {}", text.trim()))?;
                FieldValue::Choice(choice.value)
            }
            FieldKind::Steps
            | FieldKind::Params
            | FieldKind::Text
            | FieldKind::Date
            | FieldKind::Script => FieldValue::Text(text.to_owned()),
        };
        self.set_value(id, value)
    }

    /// One step: ±1, ± the float step, the previous / next choice, a
    /// toggle. False for text settings.
    pub fn adjust(&mut self, id: FieldId, delta: i32) -> bool {
        let Some(kind) = self.spec(id).map(|s| s.kind.clone()) else {
            return false;
        };
        let value = match (kind, self.value(id)) {
            (FieldKind::Int { .. }, FieldValue::Int(v)) => {
                FieldValue::Int((i64::from(v) + i64::from(delta)).max(0) as u32)
            }
            (FieldKind::Float { step, .. }, FieldValue::Float(v)) => {
                FieldValue::Float(v + step * delta as f32)
            }
            (FieldKind::EasyDay, FieldValue::Float(v)) => {
                FieldValue::Float(((v * 2.0).round() + delta as f32) / 2.0)
            }
            (FieldKind::Bool, FieldValue::Bool(v)) => FieldValue::Bool(!v),
            (FieldKind::Choice { .. }, FieldValue::Choice(v)) => {
                let choices = self.choices(id);
                if choices.is_empty() {
                    return false;
                }
                let n = choices.len() as i32;
                let index = choices.iter().position(|c| c.value == v).unwrap_or(0) as i32;
                FieldValue::Choice(choices[(index + delta).rem_euclid(n) as usize].value)
            }
            _ => return false,
        };
        self.set_value(id, value).is_ok()
    }

    /// Text for the value column.
    pub fn display(&self, id: FieldId) -> String {
        let s = &self.form().strings;
        match (self.spec(id).map(|s| &s.kind), self.value(id)) {
            (Some(FieldKind::EasyDay), FieldValue::Float(v)) => {
                if v == 1.0 {
                    s.easy_normal.clone()
                } else if v == 0.5 {
                    s.easy_reduced.clone()
                } else if v == 0.0 {
                    s.easy_minimum.clone()
                } else {
                    format!("{:.0}%", v * 100.0)
                }
            }
            (Some(FieldKind::Float { percent: true, .. }), FieldValue::Float(v)) => {
                format!("{}%", format_float(v * 100.0))
            }
            (Some(FieldKind::Params), FieldValue::Text(v)) if v.is_empty() => "(default)".into(),
            (Some(FieldKind::Script), FieldValue::Text(v)) => {
                if v.trim().is_empty() {
                    "(none)".into()
                } else {
                    format!("({} lines)", v.lines().count())
                }
            }
            (Some(FieldKind::Date), FieldValue::Text(v)) if v.is_empty() => "(none)".into(),
            (Some(FieldKind::Text), FieldValue::Text(v)) if v.is_empty() => {
                default_param_search(&self.current().name)
            }
            _ => self.edit_text(id),
        }
    }

    /// Desktop's warnings for a setting (shown under it).
    pub fn warnings(&self, id: FieldId) -> Vec<String> {
        let s = &self.form().strings;
        let c = self.config();
        let fsrs = self.fsrs();
        let mut out = vec![];
        let mut warn = |cond: bool, text: &str| {
            if cond && !text.is_empty() {
                out.push(text.to_owned());
            }
        };
        let last_days = |steps: &[f32]| steps.last().map_or(0.0, |m| m / 60.0 / 24.0);
        match id {
            FieldId::ReviewsPerDay => {
                let new = match self.value(FieldId::NewPerDay) {
                    FieldValue::Int(v) => v,
                    _ => 0,
                };
                let reviews = match self.value(FieldId::ReviewsPerDay) {
                    FieldValue::Int(v) => v,
                    _ => 0,
                };
                let expected = (new.saturating_mul(10)).min(9999);
                if expected > reviews {
                    out.push(s.reviews_too_low.format(new, expected));
                }
            }
            FieldId::LearnSteps => {
                let last = last_days(&c.learn_steps);
                warn(
                    !fsrs && last > c.graduating_interval_good as f32,
                    &s.learning_step_above_graduating,
                );
                warn(fsrs && last >= 1.0, &s.steps_too_large_for_fsrs);
            }
            FieldId::GraduatingIntervalEasy => warn(
                !fsrs && c.graduating_interval_good > c.graduating_interval_easy,
                &s.good_above_easy,
            ),
            FieldId::NewCardInsertOrder => {
                warn(c.new_card_insert_order == 1, &s.insertion_order_random)
            }
            FieldId::RelearnSteps => {
                let last = last_days(&c.relearn_steps);
                warn(
                    !fsrs && last > c.minimum_lapse_interval as f32,
                    &s.relearning_above_minimum,
                );
                warn(fsrs && last >= 1.0, &s.steps_too_large_for_fsrs);
            }
            FieldId::CapAnswerTimeToSecs => warn(
                c.cap_answer_time_to_secs > 600,
                &s.answer_secs_above_recommended,
            ),
            FieldId::MaximumReviewInterval => warn(
                c.maximum_review_interval < 180,
                &s.too_short_maximum_interval,
            ),
            FieldId::DesiredRetention => {
                if let FieldValue::Float(v) = self.value(id) {
                    let rounded = (v * 100.0).round() / 100.0;
                    warn(rounded < 0.8, &s.retention_too_low);
                    warn(rounded > 0.95, &s.retention_too_high);
                }
            }
            FieldId::Fsrs => warn(
                fsrs && self.original.days_since_last_fsrs_optimize > 30,
                &s.time_to_optimize,
            ),
            FieldId::FsrsReschedule => warn(self.fsrs_reschedule, &s.reschedule_warning),
            FieldId::EasyDay(6) => {
                let days = self.easy_days();
                warn(!days.contains(&1.0), &s.easy_days_no_normal);
                let before = self
                    .original_preset(self.current().id)
                    .map(|p| p.config.easy_days_percentages.clone())
                    .filter(|d| d.len() == 7)
                    .unwrap_or_else(|| EASY_DAYS_DEFAULT.to_vec());
                warn(
                    days != before && !(fsrs && self.fsrs_reschedule),
                    &s.easy_days_change,
                );
            }
            FieldId::CustomScheduling => warn(
                !self.globals.card_state_customizer.trim().is_empty(),
                form::CUSTOM_SCHEDULING_WARNING,
            ),
            _ => {}
        }
        out
    }
}

/// A preset option's value (settings stored in the preset).
fn preset_value(c: &PresetConfig, id: FieldId) -> FieldValue {
    use FieldValue::*;
    match id {
        FieldId::NewPerDay => Int(c.new_per_day),
        FieldId::ReviewsPerDay => Int(c.reviews_per_day),
        FieldId::DesiredRetention => Float(c.desired_retention),
        FieldId::LearnSteps => Text(steps_to_string(&c.learn_steps)),
        FieldId::GraduatingIntervalGood => Int(c.graduating_interval_good),
        FieldId::GraduatingIntervalEasy => Int(c.graduating_interval_easy),
        FieldId::NewCardInsertOrder => Choice(c.new_card_insert_order),
        FieldId::RelearnSteps => Text(steps_to_string(&c.relearn_steps)),
        FieldId::MinimumLapseInterval => Int(c.minimum_lapse_interval),
        FieldId::LeechThreshold => Int(c.leech_threshold),
        FieldId::LeechAction => Choice(c.leech_action),
        FieldId::NewCardGatherPriority => Choice(c.new_card_gather_priority),
        FieldId::NewCardSortOrder => Choice(c.new_card_sort_order),
        FieldId::NewMix => Choice(c.new_mix),
        FieldId::InterdayLearningMix => Choice(c.interday_learning_mix),
        FieldId::ReviewOrder => Choice(c.review_order),
        FieldId::FsrsParams => Text(params_to_string(c.fsrs_params())),
        FieldId::ParamSearch => Text(c.param_search.clone()),
        FieldId::BuryNew => Bool(c.bury_new),
        FieldId::BuryReviews => Bool(c.bury_reviews),
        FieldId::BuryInterdayLearning => Bool(c.bury_interday_learning),
        FieldId::DisableAutoplay => Bool(c.disable_autoplay),
        FieldId::SkipQuestionWhenReplayingAnswer => Bool(c.skip_question_when_replaying_answer),
        FieldId::CapAnswerTimeToSecs => Int(c.cap_answer_time_to_secs),
        FieldId::ShowTimer => Bool(c.show_timer),
        FieldId::StopTimerOnAnswer => Bool(c.stop_timer_on_answer),
        FieldId::SecondsToShowQuestion => Float(c.seconds_to_show_question),
        FieldId::SecondsToShowAnswer => Float(c.seconds_to_show_answer),
        FieldId::WaitForAudio => Bool(c.wait_for_audio),
        FieldId::QuestionAction => Choice(c.question_action),
        FieldId::AnswerAction => Choice(c.answer_action),
        FieldId::EasyDay(day) => Float(
            c.easy_days_percentages
                .get(usize::from(day))
                .copied()
                .filter(|_| c.easy_days_percentages.len() == 7)
                .unwrap_or(1.0),
        ),
        FieldId::MaximumReviewInterval => Int(c.maximum_review_interval),
        FieldId::InitialEase => Float(c.initial_ease),
        FieldId::EasyMultiplier => Float(c.easy_multiplier),
        FieldId::IntervalMultiplier => Float(c.interval_multiplier),
        FieldId::HardMultiplier => Float(c.hard_multiplier),
        FieldId::LapseMultiplier => Float(c.lapse_multiplier),
        FieldId::HistoricalRetention => Float(c.historical_retention),
        FieldId::IgnoreRevlogsBeforeDate => Text(c.ignore_revlogs_before_date.clone()),
        // Not stored in the preset.
        FieldId::NewCardsIgnoreReviewLimit
        | FieldId::ApplyAllParentLimits
        | FieldId::Fsrs
        | FieldId::FsrsReschedule
        | FieldId::FsrsHealthCheck
        | FieldId::LoadBalancer
        | FieldId::FsrsShortTermWithSteps => Bool(false),
        FieldId::CustomScheduling => Text(String::new()),
    }
}

/// Index of the last tab with a value (`lastSetTab`).
fn last_set(set: &[bool; 3]) -> Scope {
    match set.iter().rposition(|s| *s) {
        Some(2) => Scope::Today,
        Some(1) => Scope::Deck,
        _ => Scope::Preset,
    }
}

/// Sort orders desktop disables for a gather order (`DisplayOrder.svelte`).
fn disabled_sort_orders(gather: i32) -> &'static [i32] {
    match gather {
        // RANDOM_NOTES: same as TEMPLATE / NO_SORT.
        3 => &[2, 3],
        // RANDOM_CARDS.
        4 => &[2, 3, 4],
        _ => &[],
    }
}

fn clamp(kind: &FieldKind, value: FieldValue) -> FieldValue {
    match (kind, value) {
        (FieldKind::Int { min, max }, FieldValue::Int(v)) => FieldValue::Int(v.clamp(*min, *max)),
        (FieldKind::Float { min, max, step, .. }, FieldValue::Float(v)) => {
            let decimals = if *step >= 0.1 { 10.0 } else { 100.0 };
            FieldValue::Float(((v.clamp(*min, *max)) * decimals).round() / decimals)
        }
        (FieldKind::EasyDay, FieldValue::Float(v)) => FieldValue::Float(v.clamp(0.0, 1.0)),
        (_, value) => value,
    }
}

fn format_float(v: f32) -> String {
    let rounded = (v * 100.0).round() / 100.0;
    let text = format!("{rounded:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// "0.212, 1.29, …" with four decimals, as desktop's parameter box.
pub fn params_to_string(params: &[f32]) -> String {
    params
        .iter()
        .map(|p| {
            let text = format!("{p:.4}");
            text.trim_end_matches('0').trim_end_matches('.').to_owned()
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Comma- or space-separated numbers; empty means defaults.
pub fn string_to_params(text: &str) -> Option<Vec<f32>> {
    text.split([',', ' '])
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| t.parse::<f32>().ok().filter(|v| v.is_finite()))
        .collect()
}
