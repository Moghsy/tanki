# Phase 3 contracts and ownership

Shared contracts are `tanki-core/src/edit.rs`, exported at the crate root,
with `Command::Notes(NoteCommand)` and `Command::Browse(BrowseCommand)` as
the actor envelopes. Both answer `Response::Edit(EditResponse)`; each
command's doc comment names its response variant. `Response::changes()`
includes `EditResponse::changes()`, so the actor broadcasts `OpChanges` and
clears its cached scheduling states after every Phase 3 mutation. Phase 1
and Phase 2 interfaces stay fixed. Only the actor thread touches collections.

The lead owns `edit.rs`, `types.rs`, `lib.rs`, `actor.rs`, `config.rs`,
workspace manifests and lockfile, CLI `main.rs`, TUI `lib.rs`, `app.rs`,
`app/*`, `ui.rs`, `terminal.rs`, `keymap.rs`, all docs and exit verification.
Streams edit only their files; changes to shared files go through the lead.

## Dispatch (already wired)

`actor::execute` sends `NoteCommand::UpdateNotes` to
`writeback::update_notes`, every other `NoteCommand` to
`notes_ops::execute`, and every `BrowseCommand` to `browse_ops::execute`.
The stubs return `CoreError::Invalid("Not implemented yet …")`, so streams
test end to end through `CollectionActor::execute(Command::…)` at once.

## Streams and files

| Stream | Model | Owns |
|---|---|---|
| Notes core | Sonnet, medium | `tanki-core/src/notes_ops.rs`, `tanki-core/tests/notes_phase3.rs` |
| Editors core | Opus, high | `tanki-core/src/notefile.rs`, `tanki-core/src/writeback.rs`, `tanki-core/tests/notefile_phase3.rs`, `tanki-core/tests/writeback_phase3.rs`, `tests/fixtures/field-corpus/`, `tanki-tui/src/editor_flow.rs` |
| Browser | Opus, high | `tanki-core/src/browse_ops.rs`, `tanki-core/tests/browse_phase3.rs`, `tanki-tui/src/browser_ui.rs` (+ `browser_ui/`), `tanki-tui/src/completion.rs` |
| Notes TUI (wave 2) | Sonnet, medium | `tanki-tui/src/add_ui.rs` (+ `add_ui/`), `tanki-tui/src/textarea.rs` |
| CLI (wave 2) | Sonnet, medium | `tanki-cli/src/note_commands.rs`, `tanki-cli/tests/notes_cli.rs` |

At most three streams build at once. Wave 2 starts as wave 1 slots free up.

## Core rules every stream follows

- Call Anki's service traits fully qualified (`NotesService::add_note(col,
  …)`) with `anki_proto` messages (PLAN 4.3). No raw SQL, ever.
- Media: always return and use the filename Anki chose
  (`MediaService::add_media_file` returns it; `AddedMedia.filename`).
- Search order: `SearchOrder::Column` maps to `SortOrder.Builtin` only. The
  custom (raw SQL) order is never built or exposed.
- Find and replace: `FindReplace.note_ids` empty → `CoreError::Invalid`
  before Anki is called. The CLI requires either ids/a query or `--all`,
  which the CLI expands to explicit ids with a search first.
- Anything that can force a full sync goes through
  `crate::actor::confirm_schema_change(col, confirmed)` (exit code 24 in the
  CLI). Phase 3 has no such operation: changing a note's type is Phase 5,
  and a note file that changes `notetype:` of an existing note is refused.
- tanki state that is not Anki's (search history, editor command) lives in
  `config.rs` (`ProfileConfig::search_history` + `push_search`,
  `Config::editor` + `EditorSettings::resolve`), never in the collection.
  Browser columns, sort, notes mode and saved searches are Anki's and stay in
  the collection under desktop's keys.

## Editors contract

- `NoteUpdate` is changed-fields-only: `fields` lists only changed
  ordinals; the update is refused as a conflict when the stored mtime
  differs from `expected_mtime` or, if given, any stored field differs from
  `expected_fields`. `UpdateNotes` is all-or-nothing: with any conflict or
  missing note nothing is written and `UpdateReport` lists them. Updated
  notes are written with one `NotesService::update_notes` call (one undo
  step). Notes whose listed fields and tags already match are `unchanged`
  and are not written (no mtime/usn bump).
- `notefile::{FileNote, ParseError, write, parse}` hold the fixed names.
  Field values in a `FileNote` are stored HTML; the invariant is
  `parse(&write(n)) == n` byte for byte. Editors adds the planning helper that
  turns originals + parsed file into `NoteUpdate`s and adds, and reports
  its signature.
- `editor_flow` (tanki-tui) is shared by the TUI and `tanki edit`. Running the
  editor process is injected (a closure), so tests need no terminal and the
  TUI can suspend its screen around it.

## TUI widget pattern (as Phase 2's `SyncUi` / `PackageUi`)

Each stream's screen is a self-contained widget that the lead wires into
`app.rs`. It handles raw `crossterm::event::KeyEvent`s with its own key
table (shown through `hints()`), takes `&dyn Backend` for commands, and
returns an intent enum for what only the app can do (close, open another
screen, show a message, run an external editor). It draws into a `Rect` of a
ratatui `Buffer`; the browser and add screens also receive the
`&dyn services::CardRenderer` for previews. Every widget gets cell-buffer
snapshot tests. `crate::testing` (lead-owned, `#[cfg(test)]`) has
`buffer_text` and a study-only `FakeBackend`; write a small fake `Backend`
answering `Command::Notes` / `Command::Browse` in the stream's own test
module, and use a real `CollectionActor` on a temp fixture for integration
tests (as `tanki-tui/tests/study_flow.rs` does).

## CLI

`note_commands.rs` exposes clap `AddArgs`, `EditArgs`, `SearchArgs`,
`DeleteArgs` and `run_add/run_edit/run_search/run_delete(&CollectionActor,
&Args, json) -> Result<(), Box<dyn Error>>`, already wired in `main.rs`.
Errors with their own exit code use `NoteCliError { message, code }`;
`main` downcasts it. New exit code: **26 = a note changed since it was read
(edit conflict)**. Existing: 20 locked/in use, 21 full-sync choice, 22 auth,
23 sync not configured, 24 schema change needs confirmation, 25 profile busy.

## Plan corrections found while preparing

- PLAN Phase 3 "Read first" lists R9 §8; R9 ends at §7. The relevant R9
  material is §1 (apy's Markdown note format, ki's layout), used instead.
- The plan names `ratatui-textarea` for the raw HTML editor. No such crate
  is in the tree and ratatui 0.30 support of the textarea crates is
  unverified; tanki writes its own `textarea.rs` over `input.rs` instead.
- Clipboard images: `arboard` is not in the tree; Phase 3 shells out to
  `wl-paste` / `xclip` when installed (already-installed tools, not
  downloaded binaries), and reports clearly when neither is present.
