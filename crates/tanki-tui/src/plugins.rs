//! The TUI's view of out-of-process plugins (see `tanki-plugin`).
//!
//! `App` talks to a [`PluginPort`] so tests can substitute a scripted fake; the
//! real implementation is [`tanki_plugin::Registry`].
use std::time::Instant;
use tanki_plugin::{Event, Hook, HookContext, Registry};

pub trait PluginPort {
    fn dispatch(&mut self, hook: Hook, ctx: &HookContext, now: Instant) -> Vec<Event>;
    fn action(&mut self, plugin: &str, id: &str, card_id: i64, now: Instant) -> Vec<Event>;
    fn poll(&mut self, now: Instant) -> Vec<Event>;
    /// `(plugin, action id, title)`.
    fn actions(&self) -> Vec<(String, String, String)>;
    fn shutdown(&mut self);
}

impl PluginPort for Registry {
    fn dispatch(&mut self, hook: Hook, ctx: &HookContext, now: Instant) -> Vec<Event> {
        Registry::dispatch(self, hook, ctx, now)
    }
    fn action(&mut self, plugin: &str, id: &str, card_id: i64, now: Instant) -> Vec<Event> {
        Registry::action(self, plugin, id, card_id, now)
    }
    fn poll(&mut self, now: Instant) -> Vec<Event> {
        Registry::poll(self, now)
    }
    fn actions(&self) -> Vec<(String, String, String)> {
        Registry::actions(self)
    }
    fn shutdown(&mut self) {
        Registry::shutdown(self)
    }
}
