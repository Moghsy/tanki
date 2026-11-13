# Phase 1 interfaces and stream boundaries

Fixed by the lead before the streams start (PLAN 11, Phase 1, "Agree first").
A stream may add items to the crate it owns. Changing or removing an item listed
here needs the lead: describe the change in your report instead of making it.

## Dependency direction

```
tanki-cli ─> tanki-tui ─> tanki-core ─> anki, anki_proto, anki_i18n
                      ├─> tanki-render   (no tanki-core, no anki)
                      ├─> tanki-term     (no tanki-core, no anki)
                      └─> tanki-media    (no tanki-core, no anki)
```

This is stricter than PLAN 4.2, which lets render, term and media depend on
tanki-core. They are leaf crates here, so they build without Anki in seconds, and
only the TUI maps between their types and tanki-core's (`AudioTag` to
`MediaTag`, `tanki_core::RenderedCard` to `tanki_render::CardSide`).

Shared external versions live in the root `[workspace.dependencies]`: `ratatui`
0.30, `crossterm` 0.29, `ratatui-image` 11.1 (no libchafa), `image` 0.25 (png,
jpeg, gif, webp), `serde`, `serde_json`. Use `workspace = true` for these. Add
other dependencies to your own crate's manifest and say so in your report; the
lead resolves `Cargo.lock` conflicts at merge.

## tanki-core (`crates/tanki-core/src/types.rs`)

The actor contract from Phase 0 still holds (`phase-0-interfaces.md`): one
thread owns the collection; `CollectionActor::execute(Command) -> Result<Response>`
is synchronous; mutating commands return `OpChanges` and the actor broadcasts
them (`Response::changes`); any mutation clears the cached scheduling states, so
the caller fetches a fresh `Queue` before the next `Answer`.

New in Phase 1:

- `CollectionActor::profile_dir()`, `media_dir()`.
- `Command` gained the study commands; each doc comment names its `Response`
  variant. The actor returns `CoreError::Invalid("Not implemented yet: …")`
  for those the core stream has not built yet.
- `Command::Answer` returns `Response::Answered(AnswerOutcome)` (leech flags),
  `Undo`/`Redo` return `Response::Undone { changes, label }`.
- `Deck` gained `level`, `collapsed`, `filtered`. `QueuedCard` gained
  `note_id`, `deck_id`, `queue` (`QueueKind`, the count to underline), `flag`,
  `marked`.
- `RenderedCard` gained `ord` and `type_answer: Option<TypeAnswer>`.
  `question`/`answer` stay byte-identical to pylib's `render_output()` (the
  golden test), so they keep `[[type:…]]`. The reviewer-side filters are pure
  methods, already implemented with tests: `review_question()`,
  `review_answer(comparison_html)` (port of `reviewer.py`
  `typeAnsQuestionFilter`/`typeAnsAnswerFilter`) and `body_classes(night)`.
  The question's input is exactly `TYPE_ANSWER_INPUT`
  (`<center><input type=text id=typeans></center>`).
- `StudyConfig` (per card: home deck preset + collection prefs),
  `DeckOverview`, `Congrats`, `SchedTiming`, `SchedulerStatus`, `Target`,
  `BuryMode`, `UnburyMode`, `ForgetDefaults`, `UndoStatus`.

## tanki-render (`crates/tanki-render/src/lib.rs`)

- Input `CardSide { html, css, body_classes, media_dir }` and
  `RenderOptions { width, viewport_rows, cell_px, night_mode, fg, bg, images,
  hide_audio_buttons, ruby, expanded }`.
- Output `RenderedCard { lines: Vec<Line>, images: Vec<ImageBlock>, widgets:
  Vec<Widget>, answer_row, notices }`; `Line { spans }`, `Span { text, style,
  link, widget }`, `Style`, `Rgb`.
- `ImageBlock { source: ImageSource, row, col, width, height, alt }`: blank
  cells reserved in `lines`; the caller draws the image.
- `Widget { key, kind, row, col, width }`, `WidgetKind::{Replay, TypeAnswer,
  Hint, Details}`.
- `render(&CardSide, &RenderOptions) -> RenderedCard`;
  `paint(&RenderedCard, scroll, Rect, &mut Buffer)`.
- `ImageCache` (`new`, `request`, `get`, `subscribe`) decodes off-thread.

## tanki-term (`crates/tanki-term/src/lib.rs`)

- `Capabilities { graphics, detected, cell_px, background, foreground,
  keyboard_enhancement, synchronized_output, version, tmux }` and `dark()`.
- `probe(GraphicsMode) -> io::Result<Capabilities>`, called before the event reader.
- `Images` (`new`, `mode`, `set_mode`, `cycle`, `render(key, &DynamicImage,
  Rect, &mut Buffer)`, `clear`) over ratatui-image.
- The Phase 0 `TerminalDoctor`/`SystemTerminalDoctor`/`DoctorOptions` stay.

## tanki-media (`crates/tanki-media/src/lib.rs`)

- `MediaTag::{Sound, Tts}`, `AudioConfig { media_dir, backend, mpv }`,
  `BackendChoice`, `BackendStatus`, `AudioEvent`.
- `Audio::start(config)`, `status`, `play(tags, interrupt)`, `stop`,
  `toggle_pause`, `seek`, `is_playing`, `subscribe`.
- `rules::{on_show_question, on_show_answer, replay}`.

## Streams

Each stream works in its own git worktree under `.worktrees/`, on its own
branch, with its own target directory, and commits there early and often. It
edits only the paths it owns.

| Stream | Branch | Owns | Model |
|---|---|---|---|
| Core | `phase1-core` | `crates/tanki-core/**` except `safety.rs` and the open/backup/relock code in `actor.rs` (`CollectionActor::open`, `open_collection`, `backup`, `backup_and_relock`, the actor loop); new `scripts/study-replay.py` | Opus, high |
| Renderer | `phase1-render` | `crates/tanki-render/**` | Opus, high |
| TUI/CLI | `phase1-tui` | `crates/tanki-tui/**`, `crates/tanki-cli/**` | Opus, high |
| Media | `phase1-media` | `crates/tanki-media/**` | Sonnet, medium |
| Terminal | `phase1-term` | `crates/tanki-term/**` | Sonnet, medium |

The lead owns the root manifest, `Cargo.lock`, `xtask`, `docs/PLAN.md`, CI and
all first-open, backup and locking code, and merges streams into `main`.
