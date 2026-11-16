# Neovim code-tour cards: implementation plan

Status: first release implemented, 2026-11-15. See [setup and usage](../neovim.md).

Delivery update: existing saved-tour references, CLI authoring, add/edit picker,
review holds, and standalone source checks are implemented. Following the user's
steering, missing Neovim editors launch in Herdr and Claude starts lazily on an
explicit ask/explain/refresh action. A reader reuses its own Claude connection;
blocked startup preserves its pane for manual resolution. The original proposal
below records the design; this update supersedes its initial no-launch and
no-generation scope. Portable bundles, stop links, and a Neovim-to-tanki return
callback remain deferred.

The first release lets an ordinary Anki note store a reference to an existing
saved code-tour. During tanki review, the user recalls an answer, reveals it,
opens the linked tour in Neovim, returns to the same card, and rates it through
tanki's existing scheduler. The user selected existing saved tours first;
embedding portable tours in decks is a later extension.

## 1. Product behavior and scope

Example card:

- Prompt: “How does tanki turn a rating into the next scheduled review?”
- Answer: A short explanation suitable for checking recall.
- CodeTour: `code-tour://saved/0123456789abcdef0123456789abcdef`

The question remains visible in tanki. Revealing the answer exposes an
“Open code-tour” action. Opening it holds the card while Neovim displays the
saved explanation and source. Returning does not answer the card; Again,
Hard, Good, and Easy remain explicit tanki actions.

Initial decisions:

- Open on an explicit action after answer reveal. Opening explanations from
  the question side would expose the answer; automatic opening can be a
  subsequent preference, disabled by default.
- Link a whole saved tour, starting at its beginning. Targeting an individual
  stop or box needs a separate stable navigation contract and is deferred.
- Use the existing external Neovim editor and code-tour plugin. Keep review,
  deck selection, undo, and scheduling in tanki.
- Continue reviewing ordinary cards when Neovim or a tour is unavailable.
- Treat the link as note content; treat editor addresses, executable paths,
  repository roots, and Neovim profiles as local configuration.
- A linked tour follows the latest explicitly saved revision of that ID.
  Opening never saves or overwrites it. Capture the opened revision for
  diagnostics; revision pinning is deferred.

## 2. Existing implementation and seams

These observations come from the checked-out tanki source and the locally
installed `code-tour.nvim`, rather than assumptions about another tour plugin.

| Existing component | Relevant behavior | Planned use |
| --- | --- | --- |
| `tanki-core/src/edit.rs` | `NoteData`, named note-type fields, `GetNote`, `AddNotes`, conflict-checked `UpdateNotes` | Store and edit the reference using ordinary notes |
| `tanki-core/src/study.rs` | `queue()` already fetches each queued card's note | Extract a typed reference without another collection round trip |
| `tanki-tui/src/reviewer.rs` | Holds current card, typed answer, auto timers, answer timing, rollover | Add an explicit external-activity hold |
| `tanki-tui/src/app.rs`, `keymap.rs`, `ui.rs` | Actions, effects, command palette, review rendering | Add open/resume actions and status |
| `tanki-tui/src/terminal.rs` | Gates input and suspends the terminal for an editor | Reuse the lifecycle for an optional foreground Neovim mode |
| `tanki-tui/src/editor_flow.rs` | Injectable external process execution | Follow its testability pattern; keep tour launching separate from note-file editing |
| `tanki-core/src/config.rs` | Local TOML config | Editor, profile, and repository mappings |
| `tanki-connect` | Existing in-TUI actor-backed AnkiConnect API | Optional later creation from Neovim without opening a second collection owner |
| `code-tour.nvim/bin/code-tour` | `saved list`, `saved open`, `--pane`, `--repo`, `--map-file`, JSON results | Integration boundary |
| `code-tour.nvim/lua/code-tour/{library,saved,tour}.lua` | Stable IDs, revision hashes, source relocation, freshness checks, preflight, regeneration | Remain the authority for tour storage and source validity |

Saved tour IDs currently contain 32 lowercase hexadecimal characters;
revisions are SHA-256 hashes of saved record bytes. The library is specific
to `XDG_DATA_HOME` and `NVIM_APPNAME`, under
`stdpath("data")/code-tour/tours`. Do not hard-code the author's home directory.

Important limitations of the current launcher:

1. `saved open` may discover an editor or launch one through Herdr, then focus it.
2. With a Herdr session present, origin discovery expects an authoring agent;
   a normal tanki pane can fail that discovery.
3. An open result reports initial counts/issues; regeneration may still be
   pending. Process exit does not indicate that the user finished reading.
4. Focus can fail after the tour has already opened.
5. Existing source checks can withhold stale explanations. A matching ID or
   saved revision does not prove that its explanation matches current source.

## 3. Card storage contract

Use one optional, exactly named `CodeTour` note field containing the reference
above. The canonical value is plain text, with optional surrounding whitespace.
Use strict parsing: exactly the `code-tour://saved/` prefix and one valid ID;
reject query strings, fragments, extra path components, controls, shell text,
and additional non-whitespace content. Unknown future forms produce an
unsupported-reference diagnostic and never launch a process.

Anki fields contain HTML. Handle only a small documented set of editor wrappers
and character entities before validation, using the existing HTML parsing
dependencies where suitable. For example, a single `<div>` or `<p>` wrapper
may normalize to the canonical value. Reject multiple nonempty blocks or
ambiguous content; never concatenate arbitrary HTML into an executable target.
Write the canonical string when creating or attaching a link.

Add a serde-compatible `CodeTourRef` newtype and a result that distinguishes
absent, valid, and invalid references. Add an optional defaulted attachment
field to `QueuedCard`, preserving compatibility with existing serialized
fixtures. Retain invalid-reference diagnostics separately from a valid target.
Never make a malformed optional attachment fail the study queue.

In `queue()`, obtain the note-type field ordinals through Anki service traits,
cached per note type for that request. Read `CodeTour` from the note already
fetched there. Avoid assuming a fixed ordinal or identifying types by display
name. Invalidate any longer-lived cache after note-type changes.

Provide an explicitly installed “Tanki Code Tour” normal note type with
`Prompt`, `Answer`, and `CodeTour` fields and one forward template. Render only
Prompt/Answer in the template; tanki displays the tour action in its own review
UI. A nonempty Prompt ensures normal card generation. The Answer should remain
useful in Anki clients without this integration.

Existing custom note types may opt in by adding `CodeTour`. In the first version
all sibling cards of that note share the link. Document this and recommend one
forward card per note when different prompts require different tours. No
card-specific metadata hidden in scheduler custom data.

The reference survives normal note sync and package export/import. The tour
record and source checkout remain local and are not transferred with the deck.
An unresolved ID on another machine is an expected recoverable condition.

## 4. Local configuration and bridge

Add a local `[code_tour]` config section. Proposed shape:

```toml
[code_tour]
enabled = true
command = ["code-tour"]
nvim_appname = "nvim"
launch_mode = "existing" # first release: select a running editor

[code_tour.repositories]
# Keys are repository IDs reported by code-tour metadata, not guessed names.
repo_example = "/absolute/path/to/current/checkout"
```

Allow optional explicit XDG config/data/state roots for a separate Neovim
profile. Validate absolute paths and apply the same environment to list,
inspect, and open. Never copy records between profiles to repair a mismatch.
Keep sockets ephemeral: remember a selected live editor for this tanki session,
then validate its instance identity on reconnect. Several matching editors
produce an editor picker; never pick the first arbitrarily.

Add a small integration module, initially `tanki-tui/src/code_tour.rs`, with
typed operations for capabilities, bounded listing, inspecting one ID, and
opening a tour. The CLI can reuse this module, consistent with its existing
reuse of TUI editor flows. Split it into a dedicated crate only if reuse grows.

Use executable-plus-argument arrays, captured JSON, bounded output, a bounded
request deadline, and cancellable process handling. Never use `sh -c`, build
Lua from card content, or send these references to `xdg-open`. Preserve the
current HTTP/mail-only OSC 8 policy in `links.rs`; the tour action belongs to
tanki, not terminal URL handlers.

Use the installed plugin CLI as the compatibility boundary rather than reading
its saved JSON records in Rust. Add a versioned capabilities response and
single-ID metadata lookup to avoid implementing its private validation rules
or finding one ID by arbitrarily large `saved list` calls. Report unsupported
capabilities with a useful plugin-update message.

### Required code-tour plugin additions

These are proposed changes in the separate Neovim plugin repository; this
planning task does not modify that checkout or the user's Neovim configuration.

- Add `capabilities` and `saved inspect ID`, both machine-readable and usable
  without discovering an editor, reading project source, or invoking an agent.
- Add an explicit standalone/review open mode. It must skip authoring-origin
  discovery, clear any inherited origin from the previous active tour, and
  disable regeneration delivery. Keep saved source checks active: unchanged
  prepared boxes remain readable, stale/unresolved boxes show their status.
- Disable agent-dependent ask/explain/refine/expand generation in that mode,
  with a clear message. Prepared nested calls remain navigable. Do not silently
  send study activity to a historical or ambient conversation.
- Add a no-focus option so activation success and focus success are separate.
  Return structured statuses for opened, unavailable, ambiguous, and failed,
  along with ID, revision, instance identity, counts, and issues.
- Preserve preflight-before-replacement behavior. Bad mappings or a missing
  target must leave the existing editor tour intact.

Proposed calls, to be finalized in the first milestone:

```text
code-tour capabilities
code-tour saved inspect TOUR_ID
code-tour saved list --limit 50
code-tour saved open TOUR_ID --pane SOCKET --standalone --no-focus
```

The final two flags and first two commands are new proposals, not current CLI
features. Repository overrides continue to use the existing repeatable
`--repo REPO_ID ABSOLUTE_ROOT`; renamed files use `--map-file`. Resolve mappings
from explicit local settings or a user selection, not a repository-wide search.

First support a running Neovim editor. Offer Herdr focus when the selected
instance supplies its session/pane identity; otherwise show where the tour
opened and let the user switch normally. Launching a new foreground Neovim
requires a separate bootstrap path, covered after the first working release.

## 5. Review state and timing

Add a review activity state independent of `Phase::Question/Answer`:

```text
Answer ready -> Opening tour -> Tour active -> Answer ready -> explicit rating
                      |               |
                      +-> Failure     +-> explicit return/cancel
```

Store current card ID, note ID, tour ID, an activity token, start time, prior
auto-timer state, and selected editor identity. A late result must match both
the card and activity token before changing the UI. Repeated open input while
launching must not create duplicate processes.

While opening or active:

- Prevent auto reveal/rating/bury and queue advancement. Disable rating keys
  behind an explicit “Return to card” state, so stray terminal keys cannot rate.
- Defer day-rollover queue replacement and timebox prompts until return.
  Wall-clock scheduling time continues normally.
- Stop card audio/video and clear terminal-owned graphics before handoff.
  Restore the screen and input correctly on return, cancellation, or failure.
- Exclude this external interval from the displayed answer timer and the
  duration passed to the existing answer command. Track accumulated hold
  duration explicitly instead of shifting the wall clock. Exclude it from
  active-session timebox duration as well. This is a deliberate initial policy
  for potentially long source exploration; a count-as-study option can follow.

The MVP uses an explicit “Return to card” action in tanki after the user switches
back from Neovim. A focus event is insufficient proof that the tour ended;
`saved open` exiting is also insufficient. Return restores manual review and
leaves auto advance off until explicitly re-enabled. Launch failure restores
the card with an error and leaves auto advance off, avoiding an expired timer
immediately rating it.

Continue processing actor change notifications while a tour is active. Mark
the held review invalid if another client changes its card/note, scheduler
configuration, queues, or profile context. On return, refresh invalid review
state through existing reviewer mechanisms; never submit a rating against
stale cached scheduling states. Explain when the card changed or disappeared.
With no relevant changes or rollover, preserve the card, typed answer,
revealed side, and expanded widgets exactly.

Show the linked title, open status, and any partial-source issues in tanki.
Allow retry, choose another editor, or continue reviewing without the tour.
Do not make a broken link look like a successfully read tour.

## 6. Creating and maintaining linked cards

Add a `tanki tour` command group. Commands below are proposed:

| Command | Behavior |
| --- | --- |
| `tanki tour doctor` | Check bridge capabilities, Neovim profile agreement, live editors, and local configuration without opening a collection |
| `tanki tour list --limit 50 --json` | List saved metadata and diagnostics without opening source or an Anki profile |
| `tanki tour open ID --pane SOCKET` | Open an existing tour in standalone mode without opening an Anki profile |
| `tanki tour install-note-type` | Explicitly create the recommended type through existing core operations |
| `tanki tour add --tour ID --deck NAME --prompt TEXT --answer TEXT` | Validate an existing reference and add one normal note through the actor |
| `tanki tour attach --note ID --tour TOUR_ID` | Update only the named field with existing conflict checks; require the field to exist |
| `tanki tour check --query QUERY` | Diagnose linked notes, invalid references, and missing local tours without opening every source file |

Dispatch collection-free commands before `with_collection`, like other local
commands. Creation commands require an explicitly installed compatible note
type and use existing field validation, duplicate handling, and undo semantics.
Installation is idempotent when a compatible type exists; an incompatible
same-name type produces a conflict rather than overwriting user templates.

In the add/edit UI, provide a saved-tour picker that writes `CodeTour`. Present
title, stable ID, saved time, and repository labels; duplicate titles are valid.
Provide bounded pagination or an explicit larger-limit action; do not silently
hide all but the newest 50 entries. Surface unreadable library-entry diagnostics.

Later, add `:TankiAddTourCard` in Neovim. It uses the ID of an explicitly saved
tour, asks for Prompt/Answer/deck, and calls the existing in-TUI AnkiConnect
endpoint. It must not silently save a live tour or launch a second tanki CLI
against the locked profile. Honor configured API authentication. This shortcut
is useful, but not required for the first end-to-end workflow.

## 7. Delivery milestones and acceptance gates

### Milestone 1: bridge compatibility spike

Implement the plugin standalone contract and bridge capabilities/inspect/open
operations with synthetic libraries. Validate normal shell, Herdr without an
agent, multiple editors, and a separate `NVIM_APPNAME`.

Gate: a saved fixture tour opens in a selected headless Neovim without an AI
conversation or model request; stale boxes are reported without exposing stale
prose; existing interactive agent-backed opening still works.

### Milestone 2: stored references and CLI authoring

Implement `CodeTourRef`, extraction, the optional queue attachment, local
configuration, explicit note-type installation, and `tour add/attach/list/open`.
Use existing actor commands and service traits; no vendor changes are expected.

Gate: a synthetic note stores the reference, survives an `.apkg` export/import,
and yields the correct attachment when queued. Reordered note-type fields work.
Malformed attachments leave the underlying card reviewable.

### Milestone 3: review integration

Add `open-code-tour` and `resume-code-tour` actions, a visible answer-side
button/hint, the activity hold, injected asynchronous launcher, and error UI.
Choose a default key only after checking existing bindings; initially expose
the actions through the command palette and allow user keymap overrides.

Gate: reveal, open, navigate in Neovim, return, and rate the same synthetic card;
exactly one ordinary review is recorded only after rating. Failure and delayed
results never advance or rate the card. No agent conversation is needed.

### Milestone 4: authoring UI and operational finish

Add add/edit pickers, missing-root/file diagnostics, `tour check`, setup docs,
and synthetic examples. Implement the optional Neovim creation shortcut only
after creation through tanki is proven.

Gate: a user can create a linked card without manually constructing its field,
repair a moved-root mapping, and understand what sync transfers. Setup works
without hard-coded paths from this workstation.

### Subsequent extensions

- New foreground Neovim launch: bootstrap the plugin and open a tour through a
  fixed Lua entry point with structured data. Reuse the terminal input gate;
  distinguish user exit from successful tour activation, and restore on failure.
- Optional Neovim “return to card” mapping with a private, one-shot local
  callback and matching activity token. Avoid adding reviewer control to the
  AnkiConnect HTTP surface solely for this callback.
- Optional auto-open after reveal, stop/box targets, multiple attachments,
  explicit agent-assisted refresh, revision pinning, and portable tour bundles.
  Each needs its own versioned contract rather than stretching the v1 URI.

## 8. Implementation files and verification

| Area | Expected files |
| --- | --- |
| Reference and configuration | New `crates/tanki-core/src/code_tour.rs`; `lib.rs`, `types.rs`, `config.rs`, `study.rs` |
| Launcher abstraction | New `crates/tanki-tui/src/code_tour.rs`; `lib.rs`, `terminal.rs`, test fakes |
| Review behavior | `reviewer.rs`, `app.rs`, new `app/code_tour.rs`, `keymap.rs`, `ui.rs` |
| Authoring | `add_ui.rs`, `browser_ui.rs`, related editor flows as needed |
| CLI | New `crates/tanki-cli/src/tour_commands.rs`; `main.rs`; regenerate completions/man pages through the existing mechanism |
| Plugin contract | External plugin `bin/code-tour`, `lua/code-tour/{tour,saved}.lua`, capability/metadata helpers and tests |
| User documentation | `README.md`, installation/setup guide, this plan's status and decisions |

Meaningful automated checks:

1. Reference parsing: exact IDs, wrappers/entities, field reorder, absent and
   malformed fields, unsupported versions, controls, and oversized content.
2. Actor fixtures: note creation, conflict-safe attach, one-step undo, duplicate
   handling, queue extraction, sibling-card behavior, and export/import.
3. Fake bridge processes: valid/error/malformed JSON, bounded output, timeout,
   cancellation, missing executable, ambiguity, and activation-success with
   focus-failure. Assert exact argument arrays and environment selection.
4. Reviewer tests with injected time: auto timers cannot fire during a hold;
   multiple holds subtract only their durations; timer caps and timebox policy
   hold; return preserves state; launch failure restores manual review; late
   results are ignored; external mutation and rollover invalidate safely.
5. Cell-buffer snapshots: answer-side action, opening, active/return prompt,
   partial source availability, missing tour, and disabled integration.
6. Headless Neovim contract tests: fresh, changed, missing, and moved source;
   profile mismatch; multiple editors; no-origin standalone use; no unsolicited
   regeneration; failed preflight preserves the previous active tour.
7. End-to-end synthetic review: opening/closing alone creates no revlog entry;
   explicit rating creates one; undo follows the existing review path.

Keep HOME, Anki base, Neovim XDG directories, tour library, sockets, and source
repositories under `/tmp` in integration tests. Do not read real Anki data or
the user's saved-tour library. Use deterministic fixtures and fake process
adapters; real Neovim tests exercise the protocol without Herdr where possible.

Run Cargo build/test/clippy through `python3 scripts/cargo-bounded.py`; preserve
the shared target directory and default build profiles. Start with affected
package tests, then run the appropriate workspace regression checks once the
integration is complete. Format only affected packages. Preserve the existing
dirty `vendor/anki` tree; any vendor change would require a checked-in patch.

Manual acceptance should use a generated `/tmp` profile and temporary Neovim
profile. Verify actual pane focus, readable status, keyboard return, terminal
restoration, and the complete create/review/rate workflow. Record the exact
fixture commands when implementation makes them executable.

## 9. Completion definition

The first release is complete when a user can select an existing saved tour,
store its stable reference in a card, review that card in tanki, open the tour
in Neovim without an agent session, return to the held card, and explicitly
record a normal Anki rating. Missing source, changed source, missing editor,
profile mismatch, and process failure must all have recoverable behavior.
Deck sync transfers the link, while documentation clearly explains the local
tour-library and source-checkout requirements.
