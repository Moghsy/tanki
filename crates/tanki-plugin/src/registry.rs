use crate::{
    host::{Host, Incoming},
    manifest::{self, Capability, Hook, Loaded},
    protocol::{FromPlugin, Level, ToPlugin},
    API,
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, Instant},
};

/// A plugin is dropped for the session after this many failed runs.
pub const MAX_RESTARTS: u32 = 3;
const POLL_BATCH: usize = 64;

/// The user's grant, from `[plugins.<name>]` in `config.toml`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Grant {
    pub enabled: bool,
    /// Must equal the SHA-256 of the installed `plugin.toml`. A missing or
    /// stale value refuses the plugin, so a manifest that later gains hooks or
    /// capabilities is never honoured silently.
    pub approved_digest: Option<String>,
    pub settings: Value,
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub handshake: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            handshake: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookContext {
    pub card_id: i64,
    pub note_id: i64,
    pub deck: String,
    /// All note fields; each plugin receives only those its manifest declares.
    pub fields: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub plugin: String,
    pub kind: EventKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    HoldReview(String),
    ReleaseReview,
    Status(String),
    Notify {
        level: Level,
        text: String,
    },
    /// The plugin was stopped; `permanent` means it will not restart.
    Dropped {
        reason: String,
        permanent: bool,
    },
    /// The plugin was enabled but not loaded.
    Refused(String),
}

struct Running {
    host: Host,
    started: Instant,
    ready: bool,
}

struct Slot {
    loaded: Loaded,
    settings: Value,
    running: Option<Running>,
    failures: u32,
    gone: bool,
    holding: bool,
}

pub struct Registry {
    slots: Vec<Slot>,
    limits: Limits,
}

fn event(plugin: &str, kind: EventKind) -> Event {
    Event {
        plugin: plugin.to_owned(),
        kind,
    }
}

impl Registry {
    /// Loads enabled plugins from `root/<name>/plugin.toml`. Disabled and
    /// ungranted plugins are never executed and their manifests are not read.
    pub fn discover(
        root: &Path,
        grants: &BTreeMap<String, Grant>,
        limits: Limits,
    ) -> (Self, Vec<Event>) {
        let mut slots = Vec::new();
        let mut events = Vec::new();
        for (name, grant) in grants.iter().filter(|(_, g)| g.enabled) {
            let loaded = match manifest::load(&root.join(name)) {
                Ok(l) => l,
                Err(e) => {
                    events.push(event(name, EventKind::Refused(e)));
                    continue;
                }
            };
            if loaded.manifest.name != *name {
                events.push(event(name, EventKind::Refused("name mismatch".into())));
                continue;
            }
            if grant.approved_digest.as_deref() != Some(loaded.digest.as_str()) {
                events.push(event(
                    name,
                    EventKind::Refused(format!(
                        "manifest is not approved; set manifest_sha256 = \"{}\" after reviewing plugin.toml",
                        loaded.digest
                    )),
                ));
                continue;
            }
            slots.push(Slot {
                loaded,
                settings: grant.settings.clone(),
                running: None,
                failures: 0,
                gone: false,
                holding: false,
            });
        }
        (Self { slots, limits }, events)
    }

    pub fn names(&self) -> Vec<&str> {
        self.slots
            .iter()
            .filter(|s| !s.gone)
            .map(|s| s.loaded.manifest.name.as_str())
            .collect()
    }

    /// Delivers a hook to every granted subscriber, starting plugins lazily.
    pub fn dispatch(&mut self, hook: Hook, ctx: &HookContext, now: Instant) -> Vec<Event> {
        let mut events = Vec::new();
        for slot in &mut self.slots {
            if slot.gone || !slot.loaded.manifest.hooks.contains(&hook) {
                continue;
            }
            if slot.running.is_none() {
                match start(slot, now) {
                    Ok(()) => {}
                    Err(reason) => {
                        stop(slot, reason, &mut events);
                        continue;
                    }
                }
            }
            let fields = slot
                .loaded
                .manifest
                .fields
                .iter()
                .filter_map(|f| ctx.fields.get(f).map(|v| (f.clone(), v.clone())))
                .collect();
            let message = ToPlugin::Hook {
                name: hook,
                card_id: ctx.card_id,
                note_id: ctx.note_id,
                deck: ctx.deck.clone(),
                fields,
            };
            if let Err(e) = slot.running.as_ref().unwrap().host.send(&message) {
                stop(slot, e, &mut events);
            }
        }
        events
    }

    /// Delivers a user-invoked action to one plugin.
    pub fn action(&mut self, plugin: &str, id: &str, card_id: i64, now: Instant) -> Vec<Event> {
        let mut events = Vec::new();
        let Some(slot) = self
            .slots
            .iter_mut()
            .find(|s| !s.gone && s.loaded.manifest.name == plugin)
        else {
            return events;
        };
        if !slot.loaded.manifest.actions.iter().any(|a| a.id == id) {
            return events;
        }
        if slot.running.is_none() {
            if let Err(reason) = start(slot, now) {
                stop(slot, reason, &mut events);
                return events;
            }
        }
        let message = ToPlugin::Action {
            id: id.to_owned(),
            card_id,
        };
        if let Err(e) = slot.running.as_ref().unwrap().host.send(&message) {
            stop(slot, e, &mut events);
        }
        events
    }

    /// Collects plugin output and enforces the handshake deadline. Call from
    /// the UI tick.
    pub fn poll(&mut self, now: Instant) -> Vec<Event> {
        let mut events = Vec::new();
        let handshake = self.limits.handshake;
        for slot in &mut self.slots {
            let Some(running) = slot.running.as_mut() else {
                continue;
            };
            let mut failure = None;
            for item in running.host.drain(POLL_BATCH) {
                match item {
                    Incoming::Closed => failure = Some("exited".to_owned()),
                    Incoming::Bad(e) => failure = Some(e),
                    Incoming::Msg(message) => {
                        if let Err(e) = handle(
                            &mut running.ready,
                            &slot.loaded,
                            message,
                            &mut slot.holding,
                            &mut events,
                        ) {
                            failure = Some(e);
                        }
                    }
                }
                if failure.is_some() {
                    break;
                }
            }
            if failure.is_none()
                && !running.ready
                && now.duration_since(running.started) > handshake
            {
                failure = Some("did not complete the handshake".into());
            }
            if let Some(reason) = failure {
                let excerpt = running.host.stderr_excerpt();
                let reason = if excerpt.is_empty() {
                    reason
                } else {
                    format!("{reason} (stderr: {excerpt})")
                };
                stop(slot, reason, &mut events);
            }
        }
        events
    }

    /// Stops every plugin; call on quit.
    pub fn shutdown(&mut self) {
        for slot in &mut self.slots {
            if let Some(running) = slot.running.take() {
                let _ = running.host.send(&ToPlugin::Shutdown);
            }
        }
    }
}

fn start(slot: &mut Slot, now: Instant) -> Result<(), String> {
    let host = Host::spawn(&slot.loaded)?;
    host.send(&ToPlugin::Hello {
        api: API,
        tanki_version: env!("CARGO_PKG_VERSION").to_owned(),
        settings: slot.settings.clone(),
    })?;
    slot.running = Some(Running {
        host,
        started: now,
        ready: false,
    });
    Ok(())
}

/// Kills the process and records a failure. Releases a hold the plugin owned,
/// so a dead plugin can never leave review blocked.
fn stop(slot: &mut Slot, reason: String, events: &mut Vec<Event>) {
    slot.running = None;
    slot.failures += 1;
    slot.gone = slot.failures > MAX_RESTARTS;
    let name = slot.loaded.manifest.name.clone();
    if std::mem::take(&mut slot.holding) {
        events.push(event(&name, EventKind::ReleaseReview));
    }
    events.push(event(
        &name,
        EventKind::Dropped {
            reason,
            permanent: slot.gone,
        },
    ));
}

fn handle(
    ready: &mut bool,
    loaded: &Loaded,
    message: FromPlugin,
    holding: &mut bool,
    events: &mut Vec<Event>,
) -> Result<(), String> {
    let name = loaded.manifest.name.as_str();
    let allowed = |c: Capability| {
        if loaded.manifest.capabilities.contains(&c) {
            Ok(())
        } else {
            Err(format!("used ungranted capability {c:?}"))
        }
    };
    match message {
        FromPlugin::Ready { api } => {
            if api != API || *ready {
                return Err("unexpected ready".into());
            }
            *ready = true;
        }
        _ if !*ready => return Err("sent a message before ready".into()),
        FromPlugin::HoldReview { reason } => {
            allowed(Capability::HoldReview)?;
            *holding = true;
            events.push(event(name, EventKind::HoldReview(reason)));
        }
        FromPlugin::ReleaseReview => {
            allowed(Capability::HoldReview)?;
            *holding = false;
            events.push(event(name, EventKind::ReleaseReview));
        }
        FromPlugin::Status { text } => {
            allowed(Capability::Status)?;
            events.push(event(name, EventKind::Status(text)));
        }
        FromPlugin::Notify { level, text } => {
            allowed(Capability::Notify)?;
            events.push(event(name, EventKind::Notify { level, text }));
        }
    }
    Ok(())
}
