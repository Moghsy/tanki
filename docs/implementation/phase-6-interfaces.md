# Phase 6 contracts and ownership

Phase 6 (ecosystem and polish, PLAN 11) has three priority streams; the
optional ones (T3 tier, image-occlusion creation, built-in markup editor) are
not done unless the user asks. Phase 1–5 interfaces stay fixed. Only the actor
thread touches collections. All agent runs use synthetic profiles under
`/tmp`; never real Anki data (PLAN 14 rule 1).

## Streams and files

| Stream | Who, model, effort | Owned files | Tests |
|---|---|---|---|
| Packaging | lead | `main.rs` (completions, man), `xtask`, `.github/workflows/release.yml`, `README.md`, `docs/install.md` | `tanki-cli/tests/packaging_cli.rs` |
| AnkiConnect | subagent, Sonnet, high | `tanki-core/src/connect.rs` (+ `connect/`), `crates/tanki-connect/**` (incl. its `Cargo.toml`), `tanki-cli/src/serve_commands.rs` | `tanki-core/tests/connect_phase6.rs`, `crates/tanki-connect/tests/*`, `tanki-cli/tests/serve_cli.rs` |
| Custom scheduling JS | subagent, Opus, high | `tanki-core/src/custom_scheduling.rs` (+ `custom_scheduling/`), `queue()` in `study.rs`, `Queue` in `types.rs` (one new field), the custom-scheduling warning text in `tanki-tui/src/reviewer.rs` and `tanki-core/src/deck_options/model.rs` / `deck_options.rs`, `Queue { … }` literals in TUI fakes/tests | `tanki-core/tests/custom_scheduling_phase6.rs` |

The lead owns `manage.rs`, `lib.rs`, `actor.rs`, `config.rs`, workspace
manifests and lockfile, `tanki-core/Cargo.toml`, `tanki-cli/Cargo.toml`, CLI
`main.rs`, TUI `app.rs`, `terminal.rs`, all docs and exit verification.
Changes needed in shared files go in the final report.

Already wired by the lead:

- `Command::Manage(ManageCommand::Connect(ConnectCommand))` →
  `Response::Manage(ManageResponse::Connect(ConnectResponse))`; stub enums in
  `tanki-core/src/connect.rs` with the Phase 5 module pattern
  (`pub(crate) fn execute(col, command)`, `ConnectResponse::changes()`).
- `tanki-core/src/custom_scheduling.rs` (empty private module, declared in
  `lib.rs`); `rquickjs = "0.14"` (MIT) in `tanki-core/Cargo.toml`.
- `crates/tanki-connect` with tokio, axum 0.8, serde_json, base64.
- `tanki serve` (`CliCommand::Serve(serve_commands::ServeArgs)`), dispatched
  inside `with_collection` to `serve_commands::run(actor, &args, json)`.
- `config::ConnectSettings` as `[connect]` in tanki's config file: `bind`,
  `api_key`, `cors_origins`, `in_tui`.

Shared checkout rules are those of `phase-5-interfaces.md` ("Shared checkout
rules", AGENTS.md): every Cargo command through
`python3 scripts/cargo-bounded.py` with `CARGO_BUILD_JOBS=3`; keep files
compiling; build `-p tanki-core -p tanki-tui` together; format only your own
files with `rustfmt --edition 2021 <files>`; retry builds that fail in files
you do not own; do not commit.

## AnkiConnect brief (PLAN 8.14, R9 §5)

- **Wire format** as AnkiConnect: `POST /` with JSON
  `{"action", "version", "params", "key"}`; reply `{"result", "error"}` for
  `version >= 5`, bare result for older versions (AnkiConnect's behaviour; a
  `GET /` answers like desktop's add-on does). `multi` runs a list of actions.
  Unknown actions: `"unsupported action"`.
- **Actions** (start set, ~30): `version` (report 6), `requestPermission`,
  `apiReflect`, `multi`, `deckNames`, `deckNamesAndIds`, `createDeck`,
  `changeDeck`, `modelNames`, `modelNamesAndIds`, `modelFieldNames`,
  `modelFieldsOnTemplates`, `findNotes`, `findCards`, `notesInfo`,
  `cardsInfo`, `canAddNotes`, `canAddNotesWithErrorDetail`, `addNote`,
  `addNotes`, `updateNoteFields`, `updateNote`, `addTags`, `removeTags`,
  `getTags`, `deleteNotes`, `storeMediaFile` (`data` base64 / `path` / `url`
  — `url` download only if simple with existing deps, else unsupported;
  `deleteExisting`), `retrieveMediaFile`, `getMediaFilesNames`,
  `getMediaDirPath`, `getNumCardsReviewedToday`, `guiBrowse` (headless:
  returns the matching card ids without a window). `addNote` honours
  `options.allowDuplicate`, `duplicateScope` (`deck`/collection),
  `duplicateScopeOptions.deckName/checkChildren/checkAllModels`, `tags`, and
  `audio`/`video`/`picture` media attachments (`data`/`path`, `fields`).
  Error strings match AnkiConnect's where a client may test them
  (`"cannot create note because it is a duplicate"`, `"cannot create note
  because it is empty"`, `"model was not found: …"`, `"deck was not found:
  …"`).
- **Verify against a real client**: derive the exact request sequences
  Yomitan and one other popular client send (from their public source, as
  data; do not copy code) and replay them in an integration test against a
  real server on an ephemeral port over a fixture. Record the action subset
  actually needed in the report (PLAN 13 item "The AnkiConnect action subset
  real tools need").
- **Core**: all collection work goes through `ConnectCommand` variants in
  `connect.rs` (fully qualified Anki service calls, rule 11; media names as
  Anki returns them, rule 12). Reuse existing commands (`Command::AddNote`,
  `NoteCommand`, `BrowseCommand`, `Command::Search`) where they already fit.
- **Server** (`tanki-connect`): a blocking
  `pub fn serve(backend: &dyn ConnectBackend, options: &ServeOptions, stop: &AtomicBool) -> Result<…>`
  usable both from `tanki serve` and from a scoped thread beside the TUI
  (the lead wires the TUI from `config.connect.in_tui`). `ConnectBackend`
  is a small `Send + Sync` trait implemented for `CollectionActor`
  (`execute`, `media_dir`). Requests are served one at a time against the
  actor. Defaults: `127.0.0.1:8765`; a startup warning when binding a
  non-loopback address (PLAN 12); optional `apiKey`; CORS from
  `http://localhost` plus `cors_origins` (`*` = any); preflight `OPTIONS`.
  `requestPermission` grants for allowed origins and otherwise says how to
  allow one (`--allow-origin` / config) — there is no dialog.
- **CLI**: `tanki serve [--bind ADDR] [--api-key KEY] [--allow-origin O]…`
  overriding the config; prints the address; Ctrl+C stops cleanly (the
  collection closes and waits for backups, rule 6). Exit 20 when the
  collection is locked by desktop (existing path).

## Custom scheduling brief (PLAN 8.1, R2 §3)

Reproduce `mutateNextCardStates` (`vendor/anki/ts/reviewer/answering.ts`,
`qt/aqt/reviewer.py` around `_run_state_mutation_hook`) in tanki-core:

- In `study::queue`, when `StringKey::CardStateCustomizer` is non-empty, for
  each queued card after `current.custom_data` is filled and **before**
  `describe_next_states` and caching: build `states` as protobuf JSON with
  default values emitted (camelCase; exactly what `toJson({emitDefaultValues:
  true})` gives, including how oneofs, optional fields and 64-bit integers
  appear), `customData` = four `JSON.parse(current.customData)` copies (`{}`
  on parse failure), and `ctx` = `SchedulingContext` (deck name, seed,
  decay, desired retention, as `reviewer.py` `get_scheduling_context`
  computes them); run the script as the body of
  `async (states, customData, ctx) => { … }`; drive pending jobs until the
  promise settles; parse `states` back (`fromJson`) and pack each
  `customData.X` as `JSON.stringify` into `states.X.customData`. The mutated
  states feed both the button labels and `answer` (via the cache).
- A script error, rejected promise, timeout (interrupt handler; about one
  second per card) or memory-limit hit leaves that card's states unchanged,
  as desktop does, and reports the error once per queue in a new
  `Queue::custom_scheduling_error: Option<String>` (`#[serde(default)]`);
  the reviewer shows it as a warning instead of the current "tanki does not
  run it" text, which goes away. `console.log`/`warn`/`error` exist and are
  discarded (or kept for the error text). No `fetch`, DOM or timers.
- One QuickJS runtime per queue call is fine; do not keep JS state across
  cards beyond what desktop would (desktop evaluates the script fresh per
  card).
- Custom data limits (`validate_custom_data`: object, keys ≤ 8 bytes,
  ≤ 100 bytes) are enforced by Anki at answer time; surface that error as
  is.
- Tests: fixture collections with scripts that cap intervals, set
  `customData`, read `ctx.deckName`, use `await`, throw, loop forever, and
  are syntactically invalid; assert labels, the answered card's interval and
  `custom_data`, and that a broken script changes nothing. Compare the JSON
  shape with Anki's own test (`ts/reviewer/answering.test.ts` if present).

## Packaging (lead)

Release binaries for Linux x86_64 and aarch64 built against glibc (not musl:
PipeWire/Pulse audio, R8) in a tag-triggered workflow; `tanki completions
<shell>` (clap_complete); man pages (`cargo xtask man`, clap_mangen); install
instructions. No Homebrew/AUR/Nix recipes until publishing. Measure binary
size and cold-start time (PLAN 13).

## Stream results

Implemented the three priority streams in the shared checkout; optional T3,
occlusion creation and markup editing remain deferred. No new vendor edits.
The existing progress patch still exactly represents the vendor diff.

### AnkiConnect verification

The adapter supports 33 actions, including `createModel` for normal (non-cloze) note types (the exact list is returned by `apiReflect`).
It serves serialized requests through the collection actor, with v2/v4 bare
success responses and v5/v6 envelopes. Errors always have `result`/`error`,
including old protocol versions. Each nested `multi` request uses its own
version and API key, matching upstream.

`crates/tanki-connect/tests/clients.rs` replays request data derived on
2026-11-15 from these primary sources, without copying their implementation:

- [Yomitan client](https://github.com/yomidevs/yomitan/blob/master/ext/js/comm/anki-connect.js):
  version 2, model/deck discovery, capability reflection, duplicate checks,
  adding notes, and bare-result `multi` searches plus note information.
- [Obsidian-to-Anki client](https://github.com/ObsidianToAnki/Obsidian_to_Anki/blob/master/obsidian_to_anki.py):
  version 6 model discovery, media storage, nested `multi`/`addNotes`, field
  updates, note/card information, deck moves, tag replacement and deletion.
- [AnkiConnect protocol](https://github.com/FooSoft/anki-connect/blob/master/plugin/web.py):
  old-version error envelopes and per-child request handling.

These are HTTP protocol replays over an ephemeral localhost port and a
synthetic collection, rather than interactive browser-extension sessions.
Additional checks cover optional API keys, permission discovery, CORS,
preflight, media attachments, duplicate scope and unsupported actions.
`serve_cli.rs` checks CLI/config overrides, lock conflict exit 20, SIGINT
shutdown, and reopening the collection afterwards. `connect.in_tui` starts
the same server in a scoped thread and joins it before closing the actor.
Media URL downloads are explicitly unsupported. `guiBrowse` is headless.

### Custom scheduling verification

The pinned backend supplies `QueuedCard.context` directly. It need not be
reconstructed from presets. Protobuf reflection emits defaults and preserves
oneof/optional presence. `ctx.seed` is a bigint; absent optional context
fields remain undefined. A fresh QuickJS runtime per card has a 16 MiB memory
limit, 512 KiB stack limit and one-second interrupt deadline. Console methods
are available and discarded. There are no host networking, DOM or timer APIs.

Unit tests exercise defaults, oneofs, optional presence, bigint context,
independent custom data, `await`, throws, rejected/unsettled promises, syntax
errors, invalid states, infinite loops and memory exhaustion. The actor
fixture test asserts changed interval labels, stored interval and custom
data after answering, unchanged labels after a failed transform, and Anki's
custom-data validation at answer time. The answer adapter now preserves
InvalidInput explanations instead of reducing them to the variant name. The
reviewer reports execution failures from the queue; the unsupported-script
warning has been replaced with execution information in deck options.

### Packaging verification

Five shell completion formats and recursive man pages pass CLI tests with a
fake HOME. The release workflow prepares Linux x86_64/aarch64 glibc archives,
completion scripts, man pages and SHA-256 files, and creates a draft release
for version tags. Nothing has been published. Hosted runner labels were
checked against [GitHub's runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).
The install checksum command was corrected to run beside the archive before
changing into its extracted directory.

The build guard now creates Cargo's `CACHEDIR.TAG` when it creates the target
directory, so recent Cargo can clean a cache created by the wrapper. Its 52
regression tests pass. All commands retain the default debug/incremental
settings and use the shared bounded target directory.


### Final checks and measurements (2026-11-15)

- `cargo-bounded.py test -p tanki-core -p tanki-tui -p tanki-connect --lib`:
  83 core and 238 TUI tests pass (321 total).
- Targeted CLI/HTTP/scheduling integration tests pass; `serve_cli` covers
  signal shutdown and collection lock release.
- Clippy over core, TUI, connect and CLI with all targets, locked dependencies
  and `-D warnings` passes.
- Release build with default features and `--locked` passes. All five shell
  completions and man pages pass CLI tests; the release generates 32 pages.
- License metadata gate passes for 619 active packages; source whitespace checks pass (cell-buffer snapshots retain intentional
  trailing padding). The existing vendor diff still matches its checked-in patch.
- Build-guard regression suite: 52 tests pass.

The optimized, stripped x86_64 binary is 63,751,880 bytes (60.8 MiB).
`ldd` confirms glibc and ALSA dynamic linkage. On this Linux x86_64 host
(glibc 2.39), first `--version` launch took 8.16 ms, with a median 2.73 ms
for ten further new-process launches. Creating and listing a fresh synthetic
profile took 52.19 ms; reopening/listing it took median 11.18 ms over ten
launches. All measurements used a temp `--base` and fake HOME/XDG folders.
The filesystem cache was warm; these are process startup measurements,
not a disk-cold benchmark. The commands used default release settings.
The Ubuntu 22.04 x86_64/aarch64 hosted release jobs remain to be exercised
when publishing; no release or registry package was published here.
