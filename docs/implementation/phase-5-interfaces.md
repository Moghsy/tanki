# Phase 5 contracts and ownership

Phase 5 (management) adds six independent screens over existing core
plumbing (PLAN 11, Phase 5). Phase 1–4 interfaces stay fixed. Only the actor
thread touches collections. All agent runs use synthetic profiles under
`/tmp`; never real Anki data (PLAN 14 rule 1).

## Envelope (lead-owned, already wired)

`tanki-core/src/manage.rs` holds the envelope:

```rust
Command::Manage(ManageCommand)   ->   Response::Manage(ManageResponse)

ManageCommand::{DeckOptions, DeckTools, Notetypes, Stats, Transfer, Maintenance}(…)
ManageResponse::{DeckOptions, DeckTools, Notetypes, Stats, Transfer, Maintenance}(…)
```

Each stream owns one public core module that defines its own `XCommand` and
`XResponse` enums plus whatever data types it needs, and a
`pub(crate) fn execute(col: &mut Collection, command: XCommand) -> Result<XResponse>`.
The stub enums are empty (`match command {}`); streams add variants. Every
`XResponse` keeps `pub fn changes(&self) -> Option<&OpChanges>`, so the actor
broadcasts `OpChanges` and clears cached scheduling states after a Phase 5
mutation. Each command's doc comment names its response variant.

Modules are public (`tanki_core::stats::…`) and are **not** glob re-exported,
so stream type names cannot collide. Only the six command/response enums are
re-exported at the crate root.

Long operations (FSRS optimise/evaluate/reschedule, check database, check
media, CSV import, empty cards on a large collection) use the generic
`CollectionActor::start(Command) -> PendingCommand` (`try_recv` →
`Ok(Some(Response))`, `wait`) and the existing `progress()` /
`cancel_operation()`. In the TUI the same is `Backend::start(Command) ->
Box<dyn CommandOp>` with `poll()`. The UI never blocks on one (PLAN 7).

## Streams and files

| Stream | Model, effort | Core module | TUI | CLI | Tests |
|---|---|---|---|---|---|
| Deck options | Opus, high | `deck_options.rs` | `deck_options_ui.rs` (+ `deck_options_ui/`) | — | `tanki-core/tests/deck_options_phase5.rs` |
| Statistics | Opus, high | `stats.rs` (+ `stats/`) | `stats_ui.rs` (+ `stats_ui/`), `charts.rs` | `stats_commands.rs` | `tanki-core/tests/stats_phase5.rs`, `tanki-cli/tests/stats_cli.rs` |
| Note types | Sonnet, high | `notetypes.rs` (+ `notetypes/`) | `notetypes_ui.rs` (+ `notetypes_ui/`) | — | `tanki-core/tests/notetypes_phase5.rs` |
| Decks tools (wave 2) | Fable, medium | `deck_tools.rs` | `deck_tools_ui.rs` (+ `deck_tools_ui/`) | — | `tanki-core/tests/deck_tools_phase5.rs` |
| Import/export extras (wave 2) | Sonnet, medium | `transfer.rs` (+ `transfer/`) | `transfer_ui.rs` (+ `transfer_ui/`) | `package_commands.rs` | `tanki-core/tests/transfer_phase5.rs`, `tanki-cli/tests/transfer_cli.rs` |
| Maintenance (wave 2) | Sonnet, medium | `maintenance.rs` | `tools_ui.rs` (+ `tools_ui/`) | `check_commands.rs`, `profile_commands.rs` | `tanki-core/tests/maintenance_phase5.rs`, `tanki-cli/tests/maintenance_cli.rs` |
| Section 8 coverage audit (exit) | Haiku | — | — | — | report to the lead |

The lead owns `manage.rs`, `types.rs`, `lib.rs`, `actor.rs`, `config.rs`,
`fixture.rs`, workspace manifests and lockfile, CLI `main.rs`, TUI `lib.rs`,
`app.rs`, `app/*`, `ui.rs`, `terminal.rs`, `keymap.rs`, `backend.rs`,
`testing.rs`, all docs and exit verification. Streams edit only their files;
changes to shared files (a new fixture, a dependency, an actor hook) go
through the lead: describe them in the final report or ask.

## Shared checkout rules (AGENTS.md)

Streams share this checkout and one Cargo target directory; there are no
worktrees. Consequences:

- Run every Cargo command through `python3 scripts/cargo-bounded.py …` with
  `CARGO_BUILD_JOBS=3`. The wrapper serializes builds, so a command may wait
  for another stream's build. Do not bypass it or change profiles.
- Keep your files compiling at every step: another stream's tests build the
  same crates. Write in small compiling increments; never leave a broken
  `tanki-core` while you think.
- If a build fails in a file you do not own, wait and retry (another stream
  is mid-edit); after three failures over several minutes, report it rather
  than editing that file.
- Build `-p tanki-core -p tanki-tui` together (`cargo test -p tanki-core`
  alone builds a second copy of anki with different features). Run only
  your own test targets/filters while iterating; clippy
  `--workspace --all-targets --all-features -- -D warnings` once at the end.
- `cargo fmt -p <crate>` only on crates you touch; never `--all`. Check
  `git diff --stat` before finishing that you changed only your files.
- Do not commit; the lead commits.

## Core rules every stream follows

- Call Anki's service traits fully qualified (`DeckConfigService::…(col,
  …)`) with `anki_proto` messages (PLAN 4.3). No raw SQL, ever (rule 4).
  Only `tanki-core` imports `anki`, `anki_proto`, `anki_i18n` (rule 9).
- Anything that can force a full sync goes through
  `crate::actor::confirm_schema_change(col, confirmed)` and returns
  `CoreError::SchemaChangeRequired` (CLI exit 24) unless confirmed. PLAN 8.6
  lists the note type operations; Check Database repairs and restore-to-default
  are also guarded.
- Back up before Check Database and Check Media (rule 5): `crate::backup`
  has the forced-backup helper; say what will change before Check Media.
- Localised strings come from Anki where Anki has them (`col.tr`), stripped
  of isolation marks (`anki_i18n::without_unicode_isolation`).
- tanki-only state lives in `config.rs` (lead). Anki state stays in the
  collection under desktop's keys so it syncs.

## TUI widget pattern (as Phase 3's `BrowserUi` / `AddUi`)

Each screen is a self-contained widget the lead wires into `app.rs` as a
`Layer`. It handles raw `crossterm::event::KeyEvent`s with its own key table
(shown through `hints()`), takes `&dyn Backend` for commands, and returns an
intent enum for what only the app can do (close, open another screen, show a
message, run `$EDITOR`, confirm a full sync). It draws into a `Rect` of a
ratatui `Buffer`. Screens with previews receive `&dyn services::CardRenderer`.
Long operations poll a `Box<dyn CommandOp>` from a `tick()` method and show
`Backend::data_progress()`; Esc cancels via `cancel_data()`.

Every widget gets cell-buffer snapshot tests (`crate::testing::buffer_text`;
write a small fake `Backend` answering your `ManageCommand` in your own test
module) and an integration test against a real `CollectionActor` on a temp
fixture (`Command::GenerateStudyFixture`), as `tanki-tui/tests/study_flow.rs`
does. Charts use ratatui's `Chart` (braille), `BarChart` and `Sparkline`;
the Statistics stream's `charts.rs` adds stacked bars and the heatmap and is
usable by others once it lands.

## Stream briefs

**Deck options** (PLAN 8.3, R2 §4). Load/save through Anki's two calls
(`get_deck_configs_for_update` / `update_deck_configs`); the selected preset
is last on save. Full form of all groups, presets (add, clone, rename,
delete, switch), per-deck "this deck" / "today only" limits, Easy Days, the
global toggles on that page, an "advanced" group for load balancer and FSRS
short-term. FSRS: optimise (one preset / all), evaluate, reschedule on change,
compute minimum recommended retention, as long operations with progress.
Simulator chart (braille line chart). Warn when the collection's custom
scheduling JS is non-empty (8.1).

**Statistics** (PLAN 8.8, R6 §7). `graphs` call → port the TypeScript maths
(binning, percentiles, streaks, summaries, range filters) for all 14 panels
with range and scope (deck/collection/search) filters; eighth-block bars,
five-colour stacked bars, pie as one stacked bar, calendar heatmap, braille
curves. Card info screen (stats rows, review log, forgetting curve; note
that the card-stats call writes back to the card). `tanki stats [--deck D]
[--format text|json|svg]`. Port with tests against fixed aggregate inputs.

**Note types** (PLAN 8.6, R7 §4). List / add from stock / clone (rewrite of
desktop's deep copy) / rename / delete; fields editor (add, rename, delete,
reorder, sort field, RTL, HTML-by-default, collapse, exclude from search,
description); templates editor (front/back/CSS, add/remove/rename/reorder,
browser appearance, deck override) with live preview via
`render_uncommitted_card` and Anki's validation errors; LaTeX header/footer
and SVG flag; change note type with field/template mapping (core +
mapping form, used from the browser); restore to default with typed
confirmation. Both guards from 8.6: full-sync confirmation and required-field
protection for cloze/image-occlusion fields. `$EDITOR` for templates reuses
the app's editor intent.

**Decks tools** (PLAN 8.2, R2). Filtered decks: create/edit with two search
terms, nine orders, reschedule flag, preview delays; rebuild, empty; "from
search" entry used by the browser. Custom study's six modes with Anki's
defaults. Unbury modes (already a reviewer choice; expose on the overview).
Deck description view/edit (Markdown flag honoured). Reparent ("Move to…")
if not already present.

**Import/export extras** (PLAN 8.11, R5 §5–6). CSV/TSV import: metadata
call (header directives, delimiter detection), field-mapping form, duplicate
handling, notetype/deck choice, as a long operation with the result log.
JSON (`ForeignData`) import. Notes/cards as text export. Find duplicates (a
rewrite; R6) as a browser-usable core call. Check what Phase 3's browser
already does for reposition and grade now and fill only gaps (the reposition
dialog's start/step/shift/randomise options, R6). Extend `tanki import` /
`tanki export` by file extension and flags.

**Maintenance** (PLAN 8.10, 8.12, 8.13, R5 §7). Check Database (backup
first; progress; clears undo; report), Check Media (backup first; list
unused/missing/trash; delete unused to trash, empty/restore trash; says what
will change), Empty Cards (report, keep-notes rule, delete). Preferences:
the collection-stored preferences form (`get_preferences` /
`set_preferences`). Profiles: list/create/rename/delete/switch over the base
folder's subfolders, tanki's metadata only (never `prefs21.db`, never
`addons21/`); delete needs typed confirmation and refuses the open profile.
`tanki check db|media|empty-cards [--yes]` and `tanki profiles
list|create|rename|delete`.

## Stream results

The first run's session was interrupted with wave 1 (deck options,
statistics, note types) and the decks tools core written. A second session
(2026-11-15) restarted the unfinished streams and wired every screen into
the app (`app/manage.rs`; app tests in `tanki-tui/tests/phase5_app.rs`).

**Deck options.** Form, presets, overrides, FSRS tools and simulator
landed in the first run (7 core tests, widget snapshots). The lead fixed two
strict-clippy lints. Opened with `o` on the deck list and overview, and from
the reviewer's `o` / More menu; a filtered deck's `o` opens its filtered-deck
dialog, as desktop does. The custom scheduling script is edited in `$EDITOR`.

**Statistics.** All 14 panels with range/scope/history filters, card info
with forgetting curve, `stats::export::{text, svg}`, `tanki stats [--deck D]
[--search Q] [--all-history] [--format text|json|svg]` (no `--deck`: whole
collection). Keys: `t` statistics (deck list/overview selection, else
collection), reviewer `i` card info, Ctrl+Alt+I previous card info (the app
tracks the previously shown card), browser `i`. A click-equivalent on a bar
opens the browser on its search. Not done: an FSRS-on integration test (the
FSRS panels are covered by unit and snapshot tests with fixed data); PDF.

**Note types.** List/add/clone/rename/delete, fields and templates editors
with live preview, LaTeX options, restore to default, change note type
mapping; both 8.6 guards. Keys: Ctrl+Shift+N (Shift+N on the deck list);
browser `N` changes the selected notes' type. Templates go to `$EDITOR`
through the app.

**Decks tools.** Filtered deck dialog (new, edit, from search), custom
study's six modes, deck description view/editor (Markdown flag), and
add/rename/"Move to…"/delete prompts. Core gained `DeckPrompt` and a
`Deleted` response with Anki's "N cards deleted." Dialogs are drawn over the
screen below (`Layer::is_dialog`), never over the reviewer's images. Keys:
global `f` new filtered deck; deck list `n` create, `e`/F2 rename, Shift+M
move, Delete/Shift+D delete; overview `c` custom study, `r` rebuild, `e`
empty, `i` description, `u` unbury (Phase 1); browser `F` filtered deck from
the search. Deviation: delete asks for `y` first (desktop does not).

**Maintenance.** Check Database (schema-change confirmation first, then a
forced backup), Check Media (backup; trash unused, empty/restore trash),
Empty Cards (keep-notes rule), Preferences, Profiles. CLI `tanki check
db|media|empty-cards` and `tanki profiles list|create|rename|delete`.
Deviations: profile delete moves the folder to `<base>/.tanki-trash/`;
Check Database always needs `confirmed` because whether it will force a full
sync is unknown beforehand. Lead's actor hooks: the cancel flag is reset
before every maintenance command, and after Check Database / Check Media
(which back up inside the command) the actor closes and reopens the
collection to retake SQLite's lock (4.4 correction;
`lock_across_processes::actor_holds_the_lock_after_check_database_and_media`).
Tools screen: Shift+T on the deck list, Ctrl+P everywhere. Profile switch:
the app quits with the chosen name, `terminal::run` returns it, and the CLI
closes the collection (joining its backup) and opens the other profile.

**Import/export extras.** CSV/TSV/text import (metadata, header
directives, delimiter, field mapping, duplicate handling, note type/deck,
long operation with result log), JSON import (schema-change guarded), notes
/ cards as text export (`.txt/.tsv/.csv`, never `collection.*`), find
duplicates (rewrite of pylib's `find_dupes`) and tag duplicates. `tanki
import` / `tanki export` dispatch by extension with new flags. In the app the
existing import prompt (Shift+I) opens the CSV form or JSON import by
extension, the export prompt (Shift+E) a text export for `.txt/.tsv/.csv`;
browser `X` finds duplicates in its search (a group's `nid:` list searches
the browser below, the results stay open) and `E` exports its rows as text.
Browser reposition already had start/step/randomise/shift; gaps: no queue
top/bottom hint (desktop uses raw SQL), randomise/shift are not saved as new
defaults, start defaults to 0.

Also: "Get Shared" (palette `shared-decks`) opens AnkiWeb's shared deck page
in the web browser (PLAN 8.11).

**Exit (lead, 2026-11-15).** Section 8 audit (Haiku, read-only): 41 T/D rows,
36 implemented at audit time, custom scheduling JS deferred to Phase 6,
shared decks then implemented by the lead and Mnemosyne import deferred with
a reason in PLAN 11. Full workspace build with all features, 673 tests (0
failed), strict clippy, licence gate and script tests pass.

## Manual checklist (for the user; the agent cannot see a terminal)

Generate a disposable profile and open it:

```sh
phase5_base=$(mktemp -d)
cargo run --offline -p xtask --features fixtures -- study-fixtures --base "$phase5_base"
cargo run --offline -p tanki-cli -- --base "$phase5_base"
```

Then, from the deck list:

1. `o` on a deck: the deck options form; change a daily limit, save, and
   reopen to see it kept. `v` shows the simulator chart.
2. Shift+N: note types; open a note type's templates, edit the front in
   `$EDITOR`, check the live preview, leave without saving.
3. `t`: statistics; scroll the panels (bars, stacked bars, calendar
   heatmap, curves look right, colours readable in your theme).
4. `n` creates a deck, `e` renames it, Shift+M moves it under another,
   Shift+D deletes it. `f` opens the filtered deck dialog.
5. Enter a deck's overview: `c` custom study, `i` description.
6. In review: `i` card info (forgetting curve), Ctrl+Alt+I previous card.
7. `b` browser: `N` change note type (Esc out), `X` find duplicates, `E`
   export rows as text to `$phase5_base/out.txt`.
8. Shift+I with a small CSV file: the mapping form and the result log.
9. Shift+T Tools: Check Database (confirm), Check Media, Empty Cards,
   Preferences; Profiles: create `second`, Enter on it switches profile
   (the app reopens on it).
10. Palette (`:`) `shared-decks`: AnkiWeb's shared deck page opens in your
    browser.

Report anything that looks wrong, with the terminal you used.
