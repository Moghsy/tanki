# tanki plugins: design and implementation plan

Status: **Phases 2-5 done** (manual terminal acceptance still open). Phase 2: `crates/tanki-plugin` (manifest, protocol,
host, registry; 17 tests with a scripted fake plugin). Phase 3: TUI hooks and
generic hold (below). Phase 4: code-tour moved out (below). Phase 5: docs and acceptance (below).
Code-tour (Neovim) is the first plugin; today it is still built into tanki and
will migrate in Phase 4.

Decisions taken: code-tour plugin in Rust; the add/edit-form picker (Alt+G) is
deferred; no "refresh stale sections" action yet. Claude stays lazy and the
Neovim plugin needs no new patch.

Acceptance (Phase 5): `crates/tanki-plugin/tests/code_tour_plugin.rs` runs the
real host against the real `tanki-code-tour` binary and a fake `code-tour` CLI
(ignored by default):

```sh
TANKI_CODE_TOUR_REPO=/path/to/tanki-code-tour TANKI_CODE_TOUR_BIN=/path/to/tanki-code-tour/binary \
  python3 scripts/cargo-bounded.py test -p tanki-plugin --test code_tour_plugin -- --ignored
```

It checks the manifest-digest grant, the handshake, field filtering (an
undeclared field is never sent), the hold and status events, and the `saved
open` call. A manual pass with a real Herdr, Neovim and Claude session remains
the user's terminal check; tests never start a real model session.

Implementation notes for Phase 4:

- The plugin lives in the sibling repository `tanki-code-tour` (Rust, no tanki
  crate dependencies). It holds the bridge to the code-tour CLI, the
  `code-tour://saved/<id>` parser, the `code-tour.nvim` patches, installers and
  setup docs. Its settings are `[plugins.code-tour.settings]` (unknown keys are
  an error); Claude stays lazy inside the Neovim reader.
- tanki lost all code-tour-specific code: `tanki-core::code_tour`,
  `CodeTourSettings`, `QueuedCard::code_tour`, the note-type installer, the
  `tanki tour` CLI, the Alt+G picker and the Shift+O/Ctrl+O tour wiring.
  Shift+O is now `plugin-actions` and Ctrl+O is `release-hold`.
- Dropped for now: creating or attaching tours from the command line or add form.
  They need `createModel`, `findNotes` and `updateNoteFields` in the AnkiConnect
  subset; until then add a `CodeTour` field to a note type and paste the
  reference.
- The plugin cannot ask which editor to use. With several Neovim editors it
  reports an error unless `editor` is set or `open --pane` is used.
- The `open` action only works for the card whose answer was revealed last,
  because the host sends note fields with the hook, not with actions.

Implementation notes for Phase 3:

- `tanki-tui` talks to plugins through the `PluginPort` trait
  (`crates/tanki-tui/src/plugins.rs`); `tanki_plugin::Registry` implements it
  and tests substitute a scripted fake.
- `[plugins.<name>]` in `config.toml` (`enabled`, `manifest_sha256`, free-form
  `settings`) is loaded by `App::set_config`; plugins live in `plugins/` next to
  the config file. Refused plugins show an error notice.
- `answer_revealed` fires from the tick once per card, only on the answer side
  and never over an overlay or layer. The card's note fields come from
  `QueuedCard::fields` (never serialized); the host filters them to the fields
  the manifest declares. The hook carries `deck_id`, not a deck name.
- The reviewer's hold (`begin_hold`/`end_hold`, formerly `begin_tour`/`end_tour`)
  is shared with the built-in tour. A plugin can hold only on the answer side and
  only when nothing else holds; the first owner keeps it. A plugin that is
  dropped while holding releases first. Ctrl+O (`resume-code-tour`) always
  force-releases.
- `plugin-actions` opens a chooser of declared actions and forwards the choice.
  Per-plugin palette entries and key bindings are deferred to Phase 4, where the
  code-tour plugin needs them.

Implementation notes for Phase 2: the grant pins the SHA-256 of `plugin.toml`
(`manifest_sha256`); a missing or stale pin refuses the plugin and reports the
digest to approve. A plugin that dies while holding review emits
`ReleaseReview` before `Dropped`. Plugins are dropped for the session after
three failed runs. Hook delivery never blocks: writes go through a bounded
queue and reads through a bounded channel (backpressure on floods).

## 1. Why, and what Anki does

Anki add-ons are in-process Python packages (`qt/aqt/addons.py`). The manager
inserts `addons21/` on `sys.path` and runs `__import__` per enabled add-on. The
add-on registers callbacks on generated hook objects
(`gui_hooks.reviewer_did_show_answer.append(fn)`; hooks are declared in
`genhooks*.py`, about 150 GUI plus 17 core). Failures are isolated (a raising
callback is removed; a failed import is reported). There is no permission
model: add-ons have full privileges.

tanki keeps two ideas: a **closed catalogue of named hooks** and a **folder +
manifest layout enabled from user config**. It does not copy in-process
full-trust loading: tanki is Rust, and the requirement is that card content can
never cause arbitrary command execution.

## 2. Principles

- A plugin is a separate executable. tanki spawns it with an argv array, never
  through a shell, and never loads plugin code in-process.
- Hooks form a closed catalogue defined in tanki. A plugin receives only hooks
  it declares **and** the user has granted.
- Card content is data. A plugin sees only the note fields its manifest
  declares. Cards never choose executables, arguments or paths.
- Failure is isolated: crash, hang, malformed or oversize output, or a timeout
  drops that plugin with a status message. Review continues.
- Nothing runs by default. A plugin is inert until enabled in `config.toml`.
- A plugin is still code the user chose to install. The allowlist protects
  against card content and undeclared hooks, not against a malicious plugin.

## 3. Layout and configuration

```
$XDG_CONFIG_HOME/tanki/plugins/<name>/plugin.toml
$XDG_CONFIG_HOME/tanki/plugins/<name>/<executable>
```

```toml
# plugin.toml
name = "code-tour"
version = "0.1.0"
api = 1
command = ["./code-tour-plugin"]     # relative to the plugin directory
hooks = ["answer_revealed"]
capabilities = ["hold_review", "status", "notify"]
fields = ["CodeTour"]                 # note fields delivered with hooks
[[actions]]
id = "open"
title = "Open code-tour"
```

```toml
# config.toml: the user's grant
[plugins.code-tour]
enabled = true
# Plugin-specific settings are passed through unchanged in `hello`.
```

The effective grant is the intersection of the manifest and config. A manifest
that later adds hooks, capabilities or fields is not honoured until the user
re-enables the plugin (tanki stores a hash of the granted manifest).

## 4. Protocol (version 1)

Newline-delimited JSON over the child's stdin and stdout. stderr is captured to
a bounded log. A line is at most 256 KiB.

tanki to plugin:

| Message | Purpose |
| --- | --- |
| `hello {api, tanki_version, settings}` | handshake; plugin replies `ready` |
| `hook {name, card_id, note_id, deck, fields}` | a granted event; `fields` holds only declared fields |
| `action {id, card_id}` | the user invoked a plugin action |
| `shutdown` | exit cleanly |

Plugin to tanki:

| Message | Needs capability |
| --- | --- |
| `ready {api}` | none |
| `hold_review {reason}` / `release_review` | `hold_review` |
| `status {text}` | `status` |
| `notify {level, text}` | `notify` |

Unknown, ungranted or malformed messages drop the plugin. Handshake timeout is
5 s. Hooks are fire-and-forget and never delay the UI. The initial catalogue is
`answer_revealed`; later additions (`question_shown`, `card_answered`,
`review_ended`) each need a design note.

## 5. Host behaviour

- **Lifecycle:** start lazily at the first granted hook, restart at most three
  times per session, kill on quit or disable.
- **Environment:** scrubbed. Pass only `HOME`, `PATH`, `XDG_*`, `HERDR_*` and
  `TERM`.
- **Review hold:** `hold_review` generalizes today's tour hold
  (`reviewer.begin_tour`/`end_tour`). It blocks rating and auto-advance and
  excludes time. **Ctrl+O always force-releases any plugin hold**, so a plugin
  cannot trap review.
- **Actions:** a plugin's declared actions appear in the command palette as
  `plugin:<name>:<id>` and can be bound in the keymap. The built-in
  `open-code-tour` action goes away.
- **Collection access:** plugins never open the collection. Writes go through
  tanki's AnkiConnect endpoint (`[connect] in_tui = true`).

## 6. Code-tour as the first plugin

Moves out of this repository into its own project (decided 2026-11-16: tanki
ships only the host, and a root `/plugins/` folder is git-ignored). The plugin
is a standalone Rust crate that copies the few small types it needs (the
`CodeTourRef` parser) instead of depending on tanki crates. It takes over:

- the `CodeTour` field parser and `CodeTourRef` (from `tanki-core/src/code_tour.rs`);
- the Neovim bridge, editor discovery and Herdr launch (from `tanki-tui/src/code_tour.rs`);
- `tanki tour doctor|list|check|open` as the plugin's own CLI;
- its settings, moved from `[code_tour]` to `[plugins.code-tour]`.

Behaviour:

1. On `answer_revealed` for a card with a valid saved reference, and when
   `auto_open = true` (default `false`), open the tour in Neovim and send
   `hold_review`. The Open action does the same on demand.
2. **Claude stays lazy.** The plugin never starts or connects Claude. The
   existing Neovim-side `lazy.ensure` connects to or creates a Claude pane only
   when the user takes an action that needs one (ask, explain or refresh). No
   new Neovim patch is required.
3. **Stale source is reported, never auto-fixed.** The Neovim plugin compares
   each box's saved evidence digest with the current code on open and marks
   boxes `ready`, `queued` (stale) or `unresolved`. Edits made while reading
   become `source_changed`. Regeneration is always an explicit action.

Unchanged: the `Tanki Code Tour` note type and the `code-tour://saved/<id>`
field format, so existing cards keep working. Existing `[code_tour]` config is
rejected with a message pointing to `[plugins.code-tour]`.

## 7. Phases

0. **Baseline:** this session's experimental edits are reverted. Committing the
   current uncommitted code-tour work as a baseline is left to the user.
1. **Design:** this document. Approval gate.
2. **`tanki-plugin` crate:** manifest, protocol, host (spawn, handshake,
   supervision, timeouts, bounded restarts), registry. Tests with a fake plugin:
   bad manifest, ungranted hook, hang, oversize or garbage output, crash loop,
   disabled plugin never spawned.
3. **`tanki-tui` integration:** registry in `App`; fire `answer_revealed` once
   per card; generic hold with forced release; plugin status and notices;
   palette and keymap actions; snapshot and unit tests with a fake registry.
   Gate: generic hold works with a fake plugin.
4. **Migrate code-tour:** build `plugins/code-tour`, remove the built-in code
   from core, TUI and CLI, drop the note-type installer and the Alt+G picker (see the
   Phase 4 notes for why).
5. **Docs and verification:** update README and PLAN.md; run tests and clippy through `scripts/cargo-bounded.py`; `cargo fmt
   -p` per package; synthetic `/tmp` profiles only. A real Herdr, Neovim and
   Claude pass is a manual terminal check.

An optional later item is an explicit "refresh stale sections" plugin action
that sends `queued` boxes to Claude through the lazy connection.

## 8. Open decisions

- Plugin language for code-tour: Rust (recommended, reuses existing types) or
  Python.
- Whether the add/edit-form tour picker (Alt+G) needs a host capability now or
  can be deferred.
- Whether to build the optional "refresh stale sections" action.
