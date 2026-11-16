//! Plugin hooks, review holds and actions. The reviewer's single hold is
//! owned by a plugin; `plugin_hold` records who.
use super::*;
use crate::plugins::PluginPort;
use tanki_core::config::{plugins_dir, Config};
use tanki_plugin::{registry::Limits, Event, EventKind, Grant, Hook, HookContext, Registry};

pub(super) struct PluginHint {
    pub plugin: String,
    pub card_id: i64,
    pub text: String,
}

pub(super) struct PluginHold {
    pub plugin: String,
    pub status: String,
}

impl App<'_> {
    pub(crate) fn holding(&self) -> bool {
        matches!(&self.screen, Screen::Review(v) if v.reviewer.holding())
    }
    /// The plugin's one-line hint for the card on screen, shown with the
    /// answer. It is tied to a card id, so a late reply never lands on the
    /// next card.
    pub(crate) fn card_hint(&self, card_id: i64) -> Option<&str> {
        self.plugin_hint
            .as_ref()
            .filter(|h| h.card_id == card_id)
            .map(|h| h.text.as_str())
    }

    /// The plugin's status line while it holds the review.
    pub(crate) fn hold_status(&self) -> Option<&str> {
        self.plugin_hold.as_ref().map(|h| h.status.as_str())
    }

    /// Replaces the plugin host; `None` disables plugins. Used by tests.
    pub fn set_plugins(&mut self, port: Option<Box<dyn PluginPort>>) {
        if let Some(old) = self.plugins.as_mut() {
            old.shutdown();
        }
        self.plugins = port;
        self.plugin_hold = None;
        self.reveal_fired = None;
    }

    /// Loads enabled, approved plugins named in the config file.
    pub(super) fn load_plugins(&mut self, config: &Config) {
        let Some(path) = self.config_path.clone() else {
            return;
        };
        let grants = config
            .plugins
            .iter()
            .map(|(name, c)| {
                let settings = serde_json::to_value(&c.settings).unwrap_or_default();
                (
                    name.clone(),
                    Grant {
                        enabled: c.enabled,
                        approved_digest: c.manifest_sha256.clone(),
                        settings,
                    },
                )
            })
            .collect();
        let (registry, events) =
            Registry::discover(&plugins_dir(&path), &grants, Limits::default());
        self.set_plugins(Some(Box::new(registry)));
        self.handle_plugin_events(events);
    }

    pub(super) fn poll_plugins(&mut self) {
        let now = self.now.at;
        let Some(port) = self.plugins.as_mut() else {
            return;
        };
        let events = port.poll(now);
        self.handle_plugin_events(events);
        self.fire_reveal_hooks();
    }

    /// Fires `answer_revealed` once per card, never while something else holds
    /// the screen, so a plugin never opens over an overlay.
    fn fire_reveal_hooks(&mut self) {
        if self.plugins.is_none()
            || self.holding()
            || self.overlay.is_some()
            || !self.layers.is_empty()
        {
            return;
        }
        let Screen::Review(v) = &self.screen else {
            return;
        };
        let Some(card) = v
            .reviewer
            .card()
            .filter(|_| v.reviewer.phase() == Phase::Answer)
        else {
            return;
        };
        if self.reveal_fired == Some(card.queued.id) {
            return;
        }
        self.reveal_fired = Some(card.queued.id);
        let ctx = HookContext {
            card_id: card.queued.id,
            note_id: card.queued.note_id,
            deck_id: card.queued.deck_id,
            fields: card.queued.fields.clone(),
        };
        let now = self.now.at;
        let events = self
            .plugins
            .as_mut()
            .unwrap()
            .dispatch(Hook::AnswerRevealed, &ctx, now);
        self.handle_plugin_events(events);
    }

    /// Lists plugin actions in a chooser; the choice is sent to that plugin.
    pub(super) fn plugin_actions(&mut self) {
        let Some(port) = self.plugins.as_ref() else {
            self.notice("No plugins are enabled");
            return;
        };
        let actions = port.actions();
        if actions.is_empty() {
            self.notice("No plugin actions available");
            return;
        }
        self.plugin_action_list = actions.clone();
        self.overlay = Some(Overlay::Choice {
            title: "Plugin actions".into(),
            message: "Run a plugin action for this card".into(),
            options: actions
                .iter()
                .enumerate()
                .map(|(i, (plugin, _, title))| {
                    (format!("{title} ({plugin})"), ChoiceValue::PluginAction(i))
                })
                .collect(),
            selected: 0,
            cancel: ChoiceValue::Cancel,
        });
    }

    pub(super) fn run_plugin_action(&mut self, index: usize) {
        let Some((plugin, id, _)) = self.plugin_action_list.get(index).cloned() else {
            return;
        };
        let card_id = match &self.screen {
            Screen::Review(v) => v.reviewer.card().map_or(0, |c| c.queued.id),
            _ => 0,
        };
        let now = self.now.at;
        let Some(port) = self.plugins.as_mut() else {
            return;
        };
        let events = port.action(&plugin, &id, card_id, now);
        self.handle_plugin_events(events);
    }

    pub(super) fn handle_plugin_events(&mut self, events: Vec<Event>) {
        for Event { plugin, kind } in events {
            match kind {
                EventKind::HoldReview(reason) => self.begin_plugin_hold(plugin, reason),
                EventKind::ReleaseReview => {
                    if self
                        .plugin_hold
                        .as_ref()
                        .is_some_and(|h| h.plugin == plugin)
                    {
                        self.release_plugin_hold();
                    }
                }
                EventKind::Status(text) => {
                    if let Some(hold) = self.plugin_hold.as_mut().filter(|h| h.plugin == plugin) {
                        hold.status = text;
                    }
                }
                EventKind::Hint { card_id, text } => {
                    self.plugin_hint = text.map(|text| PluginHint {
                        plugin,
                        card_id,
                        text,
                    });
                }
                EventKind::Notify { level, text } => {
                    let text = format!(
                        "{plugin}: {}",
                        text.chars().filter(|c| !c.is_control()).collect::<String>()
                    );
                    match level {
                        tanki_plugin::protocol::Level::Info => self.notice(text),
                        _ => self.error(text),
                    }
                }
                EventKind::Dropped { reason, permanent } => {
                    if self
                        .plugin_hint
                        .as_ref()
                        .is_some_and(|h| h.plugin == plugin)
                    {
                        self.plugin_hint = None;
                    }
                    let tail = if permanent {
                        " (disabled for this session)"
                    } else {
                        ""
                    };
                    self.error(format!("Plugin {plugin} stopped: {reason}{tail}"));
                }
                EventKind::Refused(reason) => {
                    self.error(format!("Plugin {plugin} not loaded: {reason}"));
                }
            }
        }
    }

    fn begin_plugin_hold(&mut self, plugin: String, reason: String) {
        if self.holding() || self.plugin_hold.is_some() {
            return; // One hold at a time; the first owner keeps it.
        }
        let now = self.now;
        let Some(view) = self.review() else {
            return;
        };
        if !view.reviewer.begin_hold(now) {
            return;
        }
        self.message = None;
        self.services.audio.stop();
        self.clear_images();
        let reason: String = reason.chars().filter(|c| !c.is_control()).collect();
        self.plugin_hold = Some(PluginHold {
            plugin,
            status: reason,
        });
    }

    /// Returns to the card. Always works, whatever the plugin is doing.
    pub(super) fn release_hold(&mut self) {
        if !self.holding() {
            return;
        }
        self.plugin_hold = None;
        self.overlay = None;
        self.with_reviewer(|r, b, now| r.end_hold(b, now));
        self.after_editor();
        self.ignore_keys_before = Some(self.now.at);
    }

    fn release_plugin_hold(&mut self) {
        if self.plugin_hold.is_some() {
            self.release_hold();
        }
    }
}
