# tanki — plan for a terminal Anki client

Status: **Phase 1 complete (2026-11-13). Phase 2 complete (2026-11-14); see below. All Phase 1 streams are integrated; the lead's full build/tests/clippy and exact Python study replay pass. The user confirmed fixture images, audio and terminal restoration. Results are in section 13 and `docs/implementation/phase-1-results.md`. Phase 2 (sync and data safety) is complete (2026-11-14): its automated exit test passes, the real-profile guard is removed, and the user confirmed the desktop-running checks on the real profile and the TUI data screens on a fixture; results are in section 13 and `docs/implementation/phase-2-interfaces.md`. Phase 3 (add, edit, browse) is complete (2026-11-14): its automated exit test passes and the user confirmed the add screen, browser and `$EDITOR` hand-over in their terminal; results are in `docs/implementation/phase-3-interfaces.md`. Phase 4 (rich rendering) is implemented and its automated exit passes (2026-11-15); the manual terminal checklist is pending. Phase 5 (management) is implemented and its automated exit passes (2026-11-15); the manual terminal checklist is pending; results are in `docs/implementation/phase-5-interfaces.md`. The pin moved to the 26.09.4 release tag during Phase 0 (4.1).** Written 2026-11-12 against the Anki checkout at `../anki` (`main` at `4aa9c0836`, 2026-08-30, just past the 26.08.1 tag).

## 0. How to use this document

This is the handoff for whoever implements tanki.

- **Section 11 is the work list.** Phases run in order; each has tasks, parallel streams and an exit test.
- **Section 14 holds hard rules**, mostly about protecting the user's real Anki profile. Read it before writing any code.
- **Sections 4–9 say what to build.** Section 2 records decisions already made; do not reopen them.
- **The research reports hold the detail**: file and line references into `../anki`, message shapes, markup examples. Each phase lists which to read first. Do not re-research what they cover.
- **Section 13 lists what is still unverified.** Phase 0 exists to close most of it.

Detailed evidence lives in `docs/research/01`–`09`. This plan references them as R1–R9:

| Ref | Report | Covers |
|---|---|---|
| R1 | `01-rslib-architecture.md` | Anki's Rust core as a library, full RPC inventory (268 calls), build facts |
| R2 | `02-scheduler-study.md` | Review loop, reviewer features, deck options, filtered decks |
| R3 | `03-card-rendering.md` | Template pipeline, type-answer, media, image occlusion, rendering strategy |
| R4 | `04-gui-feature-inventory.md` | Every desktop screen, dialog, preference and shortcut |
| R5 | `05-storage-sync-import-export.md` | On-disk layout, locking, sync, media, import/export, maintenance |
| R6 | `06-browser-search-stats.md` | Search language, browser table, tags, stats, config keys |
| R7 | `07-editor-notetypes.md` | Add Cards, editor, stored field HTML, note types, IO editor |
| R8 | `08-terminal-capabilities.md` | Terminal graphics/keyboard/clipboard matrices, Rust stack, measured render latencies |
| R9 | `09-prior-art-ecosystem.md` | Existing tools, AnkiWeb terms, licensing, naming, AnkiConnect |

Confidence labels used below: **[spike]** = compiled and run in this session; **[code]** = read from Anki source, not run; **[web]** = from 2026 web research; **[unverified]** = needs checking in Phase 0.

---

## 1. Goal and shape

A single self-contained binary that gives the whole Anki experience in a terminal: study, add, edit, browse, sync, statistics, deck options, note types, import/export. It opens the same `collection.anki2` files as Anki desktop, so scheduling, FSRS, undo and sync behave identically because they run Anki's own code.

Two faces on one binary:

- **TUI** (`tanki` with no arguments): full-screen, keyboard-first, mouse optional.
- **CLI** (`tanki <command>`): scriptable one-shot commands with `--json` output and meaningful exit codes.

Non-goals: running Anki's Python/Qt add-ons; pixel-identical rendering in text mode; syncing with AnkiWeb (decision 2).

---

## 2. Decisions (settled by the user, 2026-11-12)

| # | Decision | What it means for the build |
|---|---|---|
| 1 | Name stays **tanki**, presented as unofficial | README, `--version` and the about screen carry: "tanki is an unofficial, Anki-compatible client. It is not affiliated with or endorsed by Ankitects. Anki is a trademark of Ankitects Pty Ltd." Do not use Anki's logo or icons. |
| 2 | **Self-hosted sync only** | No AnkiWeb support. There is no default sync endpoint; the user must configure a server URL. tanki refuses any host that is `ankiweb.net` or a subdomain of it, with an error explaining why (AnkiWeb's terms exclude third-party clients, R9). `tanki sync-server` ships in Phase 2 so there is something to sync with. |
| 3 | **Use the existing Anki profile in place** | tanki opens the profile folders under `~/.local/share/Anki2` (or `ANKI_BASE` / `--base`) directly. No separate data folder, no migration. All the sharing rules in 4.4 are mandatory. |
| 4 | **Carry a small visibility patch on Anki** | Two one-line changes exposing progress reporting (4.1). Kept as patch files applied to the pinned submodule. |

Consequences the implementer must handle:

- **One collection, one sync server.** A collection's sync state is relative to the server it last synced with. If Anki desktop keeps syncing this profile to AnkiWeb while tanki syncs the same file to a self-hosted server, each side will see the other's state as a conflict and demand a full upload/download **[reasoned from the sync protocol, not tested; cover in the Phase 2 sync tests]**. So:
  - tanki's sync is off until the user configures a server URL for that profile.
  - When sync is first configured on a collection that has synced before, tanki warns that the first sync with a new server is a one-way full upload or download, and that every other device (desktop, phone) must then be pointed at the same server.
  - Because tanki and desktop share one file on this machine, they never need sync to see each other's changes.
- **Desktop version skew.** The Anki desktop installed on this machine is 25.09.4. tanki links a newer core (26.09.4). Mixed versions writing one collection is the same situation as a phone and a desktop on different versions, which Anki tolerates. **[spike D]** For this pair nothing is dropped on ordinary use: 25.09.4 opens, edits and closes a collection written by the newer core without touching the schema or forcing a full sync, and untouched cards keep their scheduling state. Four things differ, and the one-time notice states them (wording in `docs/implementation/spike-d-version-skew.md` section 4): Empty Cards in 25.09.4 deletes cards the newer core generates from templates using `{{#Tags}}`-style special fields; the "relative overdueness" review order is studied as "due date" there; saving a filtered deck with that order in 25.09.4's dialog loses the order; the "retrievability" sort orders mean different things in the two versions.

---

## 3. What cannot be done

### 3.1 Physically impossible in a terminal

| Thing | Why | What tanki does instead |
|---|---|---|
| Fonts, font families, web fonts in text | A terminal has one font | Raster tiers (6.1 T2/T3) render them as a bitmap |
| Font sizes in text | Fixed cell grid | Same; on kitty only, OSC 66 text sizing gives big/small text **[web]** |
| Pixel-exact CSS layout in text (absolute positioning, flex/grid, shadows, transforms) | Cell grid | Raster tiers |
| Images on terminals with no graphics protocol: Alacritty, GNOME Terminal/VTE, Apple Terminal, anything under mosh or screen **[web]** | No way to send pixels | Coarse half-block approximation, plus "open in external viewer" |
| Inline image aligned to the text baseline | Images occupy whole cells | Images and tall formulas become blocks or multi-row inline boxes |
| True ruby/furigana layout | No half-size text | Furigana on a reserved row above, same size; or `漢字(かんじ)`; raster tiers for exact layout |
| Audio through an SSH session | The terminal carries no audio | Audio plays on the machine running tanki; UI says so when no device exists |
| Dropping image *data* onto the terminal | Terminals paste a path at most | Path paste detection; clipboard image paste; kitty's drag-and-drop protocol is still experimental **[web]** |
| Embedded iframes / web video embeds | Needs a browser | "Open in browser" |

### 3.2 Impossible for other reasons

| Thing | Why | Substitute |
|---|---|---|
| Anki add-ons (150 GUI hooks + 17 core hooks, R4) | They are Python against Qt | AnkiConnect-compatible HTTP API (8.14); native versions of the popular ones (heatmap, TTS) |
| Running alongside Anki desktop on the same profile | Exclusive SQLite lock **[R5 reproduced in one process; across processes it holds only until the holder's first backup, see 4.4 correction]** | Clear "Anki has this profile open" message; sequential use is fine |
| AnkiWeb sync | Its terms exclude third-party clients (R9) | Self-hosted sync only (decision 2, 8.9) |
| Card JavaScript in text mode | No DOM | Optional headless-browser tier (T3); non-interactive |
| Clicking inside JS-driven cards | Would need to forward input to a browser | Out of scope; "open in browser" |
| WYSIWYG rich-text editing | No rich text widget | Source-level editing with preview (8.5) |
| "Save PDF" of statistics | Qt WebEngine feature | JSON, SVG and HTML export |
| Writing Anki desktop's `prefs21.db` safely | Python pickles containing Qt enum objects **[R5 verified]** | tanki keeps its own config; reads profile names at most |
| Arabic shaping / bidi in text | Terminal-dependent **[web]** | App-side bidi reordering; raster tier for Arabic |

### 3.3 Degraded, not impossible

Image occlusion and MathJax were marked impossible by two of the reports; they are not. Both render to bitmaps (6.3, 6.4) and display through the graphics protocol. They are unusable only on terminals from the first table's "no graphics" row.

---

## 4. Architecture

### 4.1 Language and core dependency

Rust, linking Anki's core crate `anki` (in `rslib/`) directly. No terminal client found in R9 does this; the others need Anki desktop running, or ship a Python runtime.

Verified in this session **[spike]** (code in Appendix A):

- A crate outside Anki's workspace builds against `anki` by path with `features = ["rustls"]`.
- It opened a collection, added a note, read the deck tree and queue, rendered a card, got button intervals, answered, searched, fetched card info and stats graph data, and opened/closed a collection through `Backend`.
- Clean release build: 1 min 17 s on 16 cores. Stripped binary: 15 MB, dynamically linking only libc/libm/libgcc. The full app will be larger; R8's 40–70 MB figure is an estimate.
- Building did not modify the Anki checkout.

**Vendoring**

- `vendor/anki` is a git submodule of `https://github.com/ankitects/anki`, pinned to the **26.09.4 release tag, commit `c06370ff4`** (moved in Phase 0 from `4aa9c0836`, the untagged `main` commit the first spike ran against). `../anki` is still a checkout of `4aa9c0836`: the research reports' line numbers refer to it, and it has the 25.09.4 tag for comparisons, but `vendor/anki` is the source that is built.
- Initialise only its `ftl/core-repo` and `ftl/qt-repo` submodules; the build reads them. The installer-template submodules are not needed.
- The build also reads relative paths such as `../../.version`, so the submodule must be a full checkout (R1).
- `anki` is `publish = false`, so tanki cannot be published to crates.io. Distribution is release binaries and `cargo install --git`.

**Patches (decision 4)**

- Patch files live in `patches/anki/`. A setup command (`cargo xtask setup`) applies them to `vendor/anki` idempotently and fails loudly if one does not apply. CI runs the same command.
- `0001-expose-progress.patch` **[code; not yet compiled — verify in Phase 0]**:
  - `rslib/src/lib.rs`: `mod progress;` → `pub mod progress;`
  - `rslib/src/progress.rs`: `pub(crate) fn progress_to_proto(` → `pub fn progress_to_proto(`
- With it, tanki passes its own `Arc<Mutex<ProgressState>>` to `CollectionBuilder::set_shared_progress_state`, polls `last_progress`, converts with `progress_to_proto(progress, &tr)` into the typed, localised `anki_proto::collection::Progress`, and sets `want_abort` to cancel.
- Keep the patch set this small. If a further need appears, prefer composing public calls over adding a patch. Moving from patch files to a fork branch is housekeeping for later and needs the user's GitHub account.
- Propose the patch upstream once it is proven.

**Build rules** (R1, R9)

- Seed tanki's `Cargo.lock` from `vendor/anki/Cargo.lock`. A dependency's lockfile is ignored, and Anki pins `unicase =2.6.0` deliberately because changing it can invalidate SQLite indexes.
- `rust-toolchain.toml` mirrors Anki's (1.97.1 at this commit).
- `protoc` must be supplied through the `PROTOC` env var (set it in tanki's `.cargo/config.toml` `[env]` or the xtask). Anki pins 31.1.
- Enable the `rustls` feature; with no TLS feature, HTTPS sync fails at runtime **[code]**.
- Depend on `anki_proto` and `anki_i18n` by path from the same submodule (they are not re-exported), and on `prost` 0.13 to match.
- Licence: AGPL-3.0-or-later, so tanki is AGPL-3.0-or-later. No GPL-2.0-only code may be borrowed (R9).

**Prerequisites on the user's machine** (checked 2026-11-12)

| Tool | State | Action |
|---|---|---|
| rustup, cargo | Installed; toolchain 1.97.1 auto-installs | None |
| C compiler | Present (spike built bundled SQLite and zstd) | None |
| `protoc` | **31.1 installed and verified** | User installed at `~/.local/bin/protoc`; set `PROTOC` to this path for builds. Distro 3.21 compatibility is no longer needed for this setup. |
| ffmpeg / ffplay | Installed | None |
| mpv | Not installed | Optional; audio falls back to the built-in player |
| chafa | Not installed | Optional; keep libchafa off by default |
| Terminals | kitty, Ghostty, WezTerm (flatpak), tmux, zellij; daily driver is herdr 0.9.5 | Used for the manual test matrix |
| Anki desktop | 25.09.4 | Older than the core tanki links; see section 2 consequences |

**Pin policy**

- Started at `4aa9c0836`; Phase 0 moved to **26.09.4**, the newest stable tag on 2026-11-13 (26.11 is in beta). R9's "Rust 1.99" was wrong for this tag: it still pins 1.97.1, so no toolchain change. The patch applies unchanged, tanki needed no source change, the storage and sync protocol files are identical to the old pin, and the golden renders are identical apart from the version stamp. Bump procedure, as done: check out the tag in `vendor/anki` with its two `ftl` submodules, update `PIN` in `xtask/src/setup.rs`, reseed `Cargo.lock` from `vendor/anki/Cargo.lock` and build once without `--locked`, rebuild the Python package (`scripts/build-pinned-python.py`), regenerate the golden file (`scripts/golden-render.py`), then run the whole suite including the live skew test.
- API stability: Anki's docs say the protobuf interface is not public API (R1). Twelve months saw 7 proto commits and 58 to `rslib/src`. Only `tanki-core` imports `anki::*`; every pin bump runs the upgrade gate (section 10).

### 4.2 Workspace layout

```
tanki/
  Cargo.toml            workspace
  Cargo.lock            seeded from vendor/anki/Cargo.lock
  rust-toolchain.toml   mirrors Anki's
  .cargo/config.toml    PROTOC and friends
  LICENSE               AGPL-3.0-or-later
  README.md             includes the unofficial disclaimer
  vendor/anki/          git submodule, pinned commit
  patches/anki/         visibility patch(es)
  xtask/                setup (submodules + patches), fixture generation, golden regeneration
  crates/
    tanki-core/         the only crate that touches anki::*; collection actor, typed commands, progress, sync orchestration
    tanki-render/       card HTML+CSS -> layout -> cells + graphics blocks; math; image-occlusion compositing; raster tiers
    tanki-term/         capability probe, graphics protocol glue, custom escape layer
    tanki-media/        audio playback, TTS, recording, clipboard, external players
    tanki-tui/          screens, widgets, keymap
    tanki-connect/      AnkiConnect-compatible HTTP API
    tanki-cli/          clap commands, JSON output; produces the `tanki` binary
  tests/fixtures/       synthetic collections, golden renders
  docs/                 this plan and the research reports
```

Dependency direction: `tanki-cli` → `tanki-tui`, `tanki-connect` → `tanki-render`, `tanki-media`, `tanki-term` → `tanki-core`. `tanki-render` takes HTML/CSS strings and a media resolver; it must not depend on `anki`.

### 4.3 Calling Anki

- **Own a `Collection` directly and call the generated service traits** (`anki::services::SchedulerService` etc.) with `anki_proto` messages. All 18 collection services are public and implemented for `Collection` **[spike]**. Going through the `Backend` object instead would force untyped byte calls for collection operations, because its typed forwarding methods are private **[spike: confirmed in generated code]**.
- Call traits in the fully qualified form (`SchedulerService::answer_card(&mut col, req)`). Inherent methods with the same names shadow them (R1).
- Strip U+2068/U+2069 isolation marks from translated strings with `anki_i18n::without_unicode_isolation` (R1).
- The `Backend`-only layer is rebuilt in `tanki-core` from public functions: `sync_login`, `normal_sync`, `full_upload`, `full_download`, `sync_media`, `maybe_backup` (R1 §4–5, §9). R5 recommended `Backend` for these; R1 found the underlying functions public. **Confirmed in Phase 0 spike A against the in-process server.** Two traps: the `SyncAuth` that `sync_login` returns has no endpoint, and no endpoint means AnkiWeb, so tanki always rebuilds the auth with its configured endpoint; and Anki follows a server's 308 `Location` to any host by itself, so the configured-URL check alone does not keep requests away from AnkiWeb (8.9).
- R1 §3 is the master inventory: 205 collection RPCs in 18 services, 37 `Backend` RPCs in 11 services. The 26 `FrontendService` RPCs are Qt callbacks and do not exist in Rust.

Known gaps (R1):

| Gap | Effect | Plan |
|---|---|---|
| Progress values sit in a private module | No structured progress bars for sync, import, FSRS optimise | Patch 0001 (4.1). If the patch fails to compile, fall back to handing the collection to a temporary `Backend` for long operations; its `latest_progress()` is typed **[spike]** |
| `transact` is private, no raw SQL | tanki cannot build its own atomic/undoable operations | Compose from existing RPCs only; this is also a safety benefit |
| Aborting a native sync may leave a transaction open | Possible inconsistent in-memory state | **[spike A]** Abort by setting `want_abort`: upstream returns `Interrupted`, rolls back and tells the server, and the same connection keeps working. Only when the sync future is dropped instead is close-and-reopen needed; that also works |
| Some return types cannot be named outside the crate | Minor | Use the proto types from the service traits |
| The client version string sent to sync servers is fixed by Anki (`anki,<version> (<buildhash>),<platform>`) | Cannot be customised except through the `BUILDHASH` build env and `PLATFORM` runtime env | Leave Anki's format. **[spike A]** `BUILDHASH` cannot be set from tanki's build: `rslib/build.rs` always overrides it from `vendor/anki/out/buildhash`, and without that file the hash is empty (`anki,26.08.1 (),linux`). The server only logs the string and accepted it. Identify tanki through `PLATFORM` at runtime instead |

### 4.4 Collection actor and concurrency

- `Collection` is `Send` but not `Sync`, and every call blocks (R1, R6). One dedicated thread owns it; the UI sends typed commands over a channel and receives results and `OpChanges`.
- Every mutating call returns `OpChanges` flags (`browser_table`, `study_queues`, `note_text`, …). Screens subscribe to these to know what to refresh. This is the same contract desktop uses (R6).
- tanki runs its own tokio runtime for network work; sync needs a reqwest 0.12 client built with `http1_only()` (R1).
- Undo is in memory, capped at 30 steps, lost on close, and cleared by any non-undoable operation such as Check Database (R1). The TUI keeps the collection open for the session; CLI commands open, act, close.

**Sharing the profile with Anki desktop (decision 3) — mandatory rules** (R5)

- The database opens with `locking_mode=exclusive`, WAL, `busy_timeout(0)`. The second opener fails immediately with a "locked" error; R5 reproduced this on a copy. tanki maps that error to: "This profile is open in Anki (or another tanki). Close it and try again." with its own exit code.
- **Correction (Phase 0, 2026-11-13; the source wins over R5).** The lock does not last for the holder's whole session. `Collection::maybe_backup` reads `collection.anki2` through a second file descriptor (`anki_io::read_locked_db_file`), and on POSIX closing any descriptor of a file releases every `fcntl` lock the process holds on it. After its first backup a process still has the collection open but other processes can open it too; later writes do not take the lock back. Reproduced with two processes against Anki 25.09.4, `4aa9c0836` and 26.09.4 (`crates/tanki-core/tests/lock_across_processes.rs`). R5's check used two connections in one process, where SQLite's own bookkeeping hides this.
  - tanki's side is fixed: the actor closes and reopens the collection after every backup it writes (`backup_and_relock`), which is the only way to take the lock again. Cost: undo history does not survive a backup. Nothing in tanki may open `collection.anki2` with plain file I/O while the collection is open.
  - Desktop's side cannot be fixed from here: Anki desktop backs up on a timer (every 30 minutes by default when something changed), so a desktop session that has made a backup no longer blocks tanki from opening the same profile. **The "locked" error is therefore not a reliable sign that desktop is closed.** Phase 2 adds a second check before the real profile is opened (decided by the user, 2026-11-13): refuse when `collection.anki2-wal` exists before tanki's own peek (this also fires after a desktop crash, so the message must say so and offer a way to proceed once the user confirms Anki is closed), and on Linux look for another process holding `collection.anki2` under `/proc`. Probing Anki's single-instance socket was considered and not chosen. The user decided not to report the bug upstream.
- Not covered by that lock: collection-package restore and full-sync download rename a file over `collection.anki2`; the media folder and media DB have no cross-process protection. tanki only performs those while it holds the collection lock itself, and backs up first.
- **Media paths:** call `set_media_paths("<profile>/collection.media", "<profile>/collection.media.db2")` explicitly. Never use `with_desktop_media_paths()`: it uses a different DB filename and would silently fork media sync state (R1, R5). Media trash is `<profile>/media.trash`.
- **Never read or write `prefs21.db`.** It holds Python pickles with Qt enum objects, and Anki resets a profile whose pickle fails to load (R5). Profiles are discovered by listing folders under the base that contain `collection.anki2`.
- **Never touch `addons21/`.**
- **First open of a collection tanki has not seen before:**
  1. Peek at the schema version with a plain read-only SQLite connection (`select ver from col`). If that fails with "locked", stop with the message above.
  2. If the version is below 18, opening will upgrade the file in place (R5). Copy `collection.anki2` to `backups/tanki-pre-upgrade-<timestamp>.anki2` first and tell the user.
  3. If the version is above what the linked Anki supports, refuse to open and say tanki needs updating **[exact behaviour of the library in this case is unverified; test in Phase 0]**.
  4. Open through the library (this takes the exclusive lock), then immediately force a normal Anki backup into the profile's `backups/` folder and wait for it to finish, before any mutating command.
- That read-only peek is the only raw SQLite access allowed anywhere. Anki's `unicase` collation makes other raw access fail or corrupt (R5).
- Automatic backups thereafter follow Anki's own schedule and retention (8.12). Every process exit waits for the backup thread (R5).
- Opening does not downgrade on close, matching desktop (R1).

### 4.5 Configuration and profiles

- tanki config: TOML at `$XDG_CONFIG_HOME/tanki/config.toml`: keymap, theme, render tier defaults, per-note-type render overrides, audio backend, last-used profile, per-profile sync server URL and username, and the list of collections already seen (for the first-open rule).
- Sync credentials (the host key returned by login): OS keyring where available, else a `0600` file in the config folder.
- Base folder: `--base` flag, then `ANKI_BASE`, then the platform default (`~/.local/share/Anki2` on Linux). Profile: `--profile`, then last used, then the only one present, else a picker.
- Settings that Anki stores in the collection (R6 §8: scheduling, reviewing, editing, backup limits, browser columns, saved searches, flag names) stay in the collection under the same keys, so desktop sees the same values.
- tanki state that is not Anki's (render overrides, search history) never goes into the collection.

---

## 5. Terminal layer

### 5.1 Stack (versions from R8, checked November 2026)

| Purpose | Choice |
|---|---|
| TUI framework | ratatui 0.30 + crossterm 0.29 |
| Images | ratatui-image 11.x (sliced partial images for scrolling, tmux detection); libchafa kept optional |
| Text input | ratatui-textarea 0.9 (tui-textarea is unmaintained) |
| CLI | clap, clap_complete, clap_mangen |
| HTML/CSS | html5ever + cssparser/selectors |
| Raster | image, resvg, tiny-skia |
| Math | RaTeX 0.1 (KaTeX-compatible, mhchem), native conservative Unicode fallback |
| Audio | rodio 0.23 + symphonia with `mp3, aac, isomp4, alac` features; mpv over JSON IPC when installed |
| Clipboard | arboard 3.7 |
| Optional tiers | Blitz (pure-Rust HTML/CSS renderer), chromiumoxide (headless Chrome), rquickjs |

crossterm lacks pixel mouse mode, the kitty clipboard protocol, text sizing and general reply parsing (R8). `tanki-term` adds a thin escape layer for those.

### 5.2 Capability detection

Your current terminal, herdr, strips the outer terminal's environment markers and reports `TERM=xterm-256color` (R8). So detection must query the terminal, never trust env vars.

- At startup send one batch of queries terminated by DA1 as a sentinel: kitty graphics query, XTVERSION, cell pixel size (`CSI 16 t` / `14 t`), kitty keyboard query, background colour (OSC 11), and mode queries for synchronized output, theme reporting, clipboard protocol and pixel mouse.
- Never send graphics sequences blindly: Apple Terminal prints them as text (R8).
- `tanki doctor` prints the detected capabilities and shows a test image, so users can report what their setup supports.
- `--graphics=kitty|iterm|sixel|blocks|none` overrides detection, and a key in the TUI cycles the mode live, because multiplexers can make images silently fail (R8).

Protocol choice **[web]**:

| Terminal | Protocol |
|---|---|
| kitty, Ghostty, Rio | Kitty graphics with Unicode placeholders |
| herdr, tmux (with passthrough), zellij ≥ 0.45 | Kitty Unicode placeholders; herdr has open bugs (stretched images, images lost on pane switch) |
| WezTerm, iTerm2 | iTerm2 inline images |
| foot, xterm, Windows Terminal ≥ 1.22, tmux ≥ 3.4 built with sixel | Sixel |
| Alacritty, GNOME Terminal, Apple Terminal, mosh, screen | Half-blocks only |

Unicode placeholders are preferred everywhere they work: the image becomes text, so it clips, scrolls and survives multiplexers.

### 5.3 Input

- All core actions work with plain keys. The kitty keyboard protocol (Ctrl+Enter, key release) is missing in Apple Terminal and VTE (R8), so "Add note" gets a fallback binding alongside Ctrl+Enter.
- Default keymap mirrors Anki's (R4 §22 lists about 190 shortcuts) with vim-style navigation added; fully remappable. Where Anki reuses a key across windows, tanki scopes bindings per screen.
- Mouse: optional click/scroll everywhere; pixel-precise mode where supported, cell resolution otherwise.
- Bracketed paste; a pasted file path is recognised as "attach this file".

---

## 6. Card rendering

This is the hardest part. Anki cards are arbitrary HTML/CSS/JS shown in Chromium.

### 6.0 Pipeline that tanki must reproduce (R3)

The template engine (fields, conditionals, all built-in filters, cloze, hints, furigana, type-answer placeholder, TTS tags, empty-card messages) is entirely in Anki's Rust code. Python and TypeScript add a small set of steps that tanki re-implements:

1. Render with `partial_render = true`; resolve unknown filters to their current text. A template error is not a failure: its message is shown as both sides, with no CSS or audio **[spike B]**.
2. Extract audio tags from the question; substitute `{{FrontSide}}` in the answer with that audio-stripped question; then extract the answer's audio tags. Rendering with `false` would play question audio twice (R3). R2 suggested `false`; the Phase 0 golden test against Anki's Python output confirmed `true`. Sound filenames are reduced to their last path component. After extraction, each side's `[latex]`, `[$]` and `[$$]` blocks become image tags (`extract_latex`); a missing image with generation off appends Anki's "disabled" message **[spike B]**.
3. Replace `[[type:Field]]` with an input widget; on the answer side call `compare_answer` and move the answer divider (R3 §3).
4. Resolve media filenames against `collection.media`.
5. Apply body classes (`card cardN nightMode isLin …`) and let night mode override the stock card colours, as desktop does (R3).
6. Draw image-occlusion masks, typeset math, play audio per the deck's autoplay setting.

### 6.1 Rendering tiers

| Tier | What | When | Cost **[R8 measured]** |
|---|---|---|---|
| T0 text | Own HTML walker to styled cells; images as labelled placeholders or half-blocks | No graphics support | Negligible |
| T1 text + graphics blocks (default) | T0 layout, with images, math and occlusion images as real bitmaps | Graphics available | Image decode/scale, off-thread, cached |
| T2 full-card bitmap, no browser | Blitz renders HTML+CSS headlessly | Cards with complex CSS; per-note-type opt-in | 85–95 ms per card |
| T3 full-card bitmap, browser | Persistent headless Chrome over CDP; runs card JS | JS-dependent cards; opt-in; needs Chrome installed | 45–75 ms warm, 0.65–1.2 s cold, several hundred MB RAM |
| Escape hatch | Write a temp HTML file with a base URL at the media folder and open the system browser | Any card, one key | — |

- A classifier inspects each note type's templates and CSS once (scripts, flex/grid/absolute positioning, iframes) and suggests a tier; the user can pin a tier per note type.
- The next card is rendered ahead while the current one is shown, so T2/T3 latency is hidden.
- T2 gaps today: ruby laid out inline, `<code>` not monospace, no JS (R8). Blitz is beta; it is optional, not on the critical path.

### 6.2 T1 text layout: supported subset

| HTML/CSS | Terminal mapping |
|---|---|
| bold, italic, underline, strikethrough, colour, background/highlight | SGR attributes, truecolour |
| `<br>`, `<div>`, `<p>`, `<hr id=answer>`, alignment | Line layout, rule, left/centre/right |
| Lists, nested lists | Bullets/numbers with indent |
| Tables | Box-drawing grid with column sizing |
| `<sub>`, `<sup>` | Unicode sub/superscripts where they exist, else `_x` / `^x` |
| `<ruby>` | Reserved row above, or parenthesised (config) |
| `<details>`, hint filter markup, replay buttons | Key-toggled widgets (R3) |
| `display:none`, class-based hiding | Honoured; templates rely on it heavily (R3) |
| `dir=rtl` | App-side bidi reordering |
| `<img>` | Graphics block sized from cell pixel size; zoom key for full-screen view |
| Links | OSC 8 hyperlinks |
| Font family/size, positioning, effects | Ignored in T1 (T2/T3 handle them) |
| `<script>` | Ignored in T1; classifier flags the note type |

The six stock note types (R3 §6) and anything the Anki editor produces must render correctly in T1 without a browser.

### 6.3 Math

Phase 4 source correction: `unicodeit` 0.2.2 declares `MIT AND LPPL-1.3c`; the license gate rejected it. It is not a dependency. A small independently written converter handles common Greek symbols and exact Unicode scripts, and other expressions use RaTeX or visible-source fallback.

- MathJax `\( \)` and `\[ \]`: RaTeX renders to SVG, resvg rasterises, shown as a graphics block. Measured 0.1–0.4 ms per formula warm (R8). Simple inline formulas become Unicode text through a small native converter; taller inline formulas get a multi-row inline box.
- Legacy `[latex]` tags: Anki's Rust code only produces `<img src="latex-<hash>.png">`; the images usually ship in the media folder, so the common case is displaying a file, inverted in night mode (R3). Generating missing ones means running `latex` + `dvipng`/`dvisvgm` exactly as Anki's Python does; this is off by default in Anki for security and stays opt-in.
- RaTeX is KaTeX-compatible, not MathJax-identical. Unsupported macros fall back to showing the source. An exact MathJax engine (`mathjax_svg`, embeds V8) is a possible optional backend **[web]**.

### 6.4 Image occlusion

- Each shape is stored as a cloze like `{{c1::image-occlusion:rect:left=.23:top=.33:width=.2:height=.1:oi=1}}`, with coordinates normalised 0–1 and angles in 1/10000 turns (R3 §5, R7 §5).
- Anki's Rust code parses shapes; the masks are drawn by TypeScript on a canvas. tanki composites them onto the bitmap with tiny-skia using the note type's colours, then displays the result. Both "hide all, guess one" and "hide one" modes are covered by the shape data.
- Creating/editing occlusions: Phase 6. Rectangles first, drawn with the mouse (pixel mode where available, cell resolution otherwise) plus keyboard nudge/resize; ellipses and polygons later.
- When adding IO notes, fill the fields and call the ordinary add-note path; the dedicated RPC ignores the chosen deck (R7).

### 6.5 Audio, video, TTS, recording

- `[sound:…]` playback follows Anki's rules: per-deck autoplay, replay key, pause, seek ±5 s, interrupt on answer (R3 §4).
- Backend order: mpv over JSON IPC if installed (widest codec support, same as desktop); otherwise built-in rodio/symphonia. Opus is not natively ready in symphonia (R8), so those files need mpv or ffmpeg. Neither mpv nor chafa is installed on this machine; ffmpeg/ffplay is.
- Video files: mpv in its own window by default; in-terminal playback via mpv's kitty/sixel output as an option.
- TTS: Anki desktop has no TTS on Linux at all (R3). tanki supports `{{tts lang:Field}}` through speech-dispatcher/espeak-ng, optionally Piper; `say` on macOS.
- Record own voice: cpal + hound, or shell out to `pw-record`/ffmpeg.
- No audio device (typical over SSH): show an indicator instead of failing silently.

---

## 7. Screens

Full-screen layout with a persistent status line (profile, sync state, due counts, pending background task).

| Screen | Contents | Anki equivalent |
|---|---|---|
| Decks | Collapsible deck tree with new/learn/due counts, studied-today line | Deck browser |
| Overview | Deck description, counts, study / custom study / options / unbury | Overview |
| Review | Card area, answer buttons with next intervals, counts with the active queue underlined, timer | Reviewer |
| Congrats | Finished-for-today info and next steps | Congrats page |
| Add / Edit | Note type and deck choosers, field list, tags, preview pane | Add Cards / Edit Current |
| Browser | Sidebar tree, virtualised table, preview pane, search bar | Browser |
| Card info | Stats rows, review log, forgetting curve | Card Info |
| Stats | 14 graph panels with range and scope filters | Statistics |
| Deck options | Preset form, FSRS tools | Deck Options |
| Note types | Types list, fields editor, card templates editor with live preview | Manage Note Types |
| Sync | Progress, conflict choice, media sync status | Sync dialogs |
| Import / Export | Option forms and result log | Import/Export pages |
| Tools | Check database, check media, empty cards, backups, preferences | Tools menu |
| Command palette | Fuzzy-search every action | (new) |

Long operations show a progress line and stay cancellable; none block the UI thread.

---

## 8. Feature coverage

Verdicts: **T** trivial (wrap an Anki call), **D** doable, **H** hard. "Rewrite" names logic that exists only in Anki's Python/Qt/TypeScript.

### 8.1 Study (R2)

| Feature | Verdict | Notes |
|---|---|---|
| Queue / answer loop | T **[spike]** | Three calls: get queued cards, describe next states, answer card |
| Counts, underlined active count, button intervals | T | Labels come localised from Anki |
| Undo / redo incl. undoing answers | T | Anki restores the queue entry |
| Flags 1–7, mark, bury card/note, suspend card/note, forget, set due date, delete note | T | Suspend/bury/mark are toggles decided client-side (R6) |
| Edit current, card info, previous card info, deck options from reviewer | D | Reuses other screens |
| Type in the answer | D | Native input; diff spans mapped to colours (R3 §3) |
| Timer, time-box prompt, auto-advance, leech notice | D | Rewrite of `reviewer.py` state machine (~1250 lines) |
| Day rollover while left open | D | Re-query on a timer; rollover is lazy inside Anki's calls (R2) |
| Custom data passthrough | T | Must copy `card.custom_data` into the current state before answering (R2) |
| Custom scheduling JavaScript | D, late | Collection-wide script runs in QuickJS (Phase 6); per-card timeout/memory limits, async jobs and error fallback warnings |
| Old scheduler collections | T | Only v3 exists now; tanki must check the flag and offer the upgrade call (forces a full sync from v1) (R2) |
| Timezone | — | Anki rewrites the stored UTC offset to the running process's zone; warn when `TZ` differs from the collection's last-known offset (R2) |

### 8.2 Decks, filtered decks, custom study (R2, R4)

| Feature | Verdict | Notes |
|---|---|---|
| Deck tree, create, rename, delete, collapse | T | |
| Reparent | T | "Move to…" prompt replaces drag and drop |
| Deck description | D | Markdown or HTML per deck flag |
| Filtered decks: create, rebuild, empty, two search terms, 9 orders, reschedule flag, preview delays | T | |
| Custom study: 6 modes | T | |
| Unbury modes | T | |

### 8.3 Deck options and FSRS (R2 §4)

- About 75 settings in 14 groups; loaded and saved by two calls. The selected preset must be last in the list on save (R2).
- Includes per-deck "this deck" / "today only" limit overrides, Easy Days, and the global toggles that live on this page.
- FSRS optimise, evaluate, reschedule on change, compute minimum recommended retention: all Anki calls; run off the UI thread with progress. Verdict D.
- Simulator charts: braille line charts. Verdict H (most chart work in the app).
- Load balancer and FSRS short-term toggles have backend fields but no desktop UI (R2, R4); tanki exposes them under "advanced".

### 8.4 Adding notes (R7 §1)

- The new Svelte editor re-implements the whole Add flow using backend calls only, so it serves as the spec: defaults for note type/deck, sticky fields, 15-entry history, field checks, discard prompt.
- Field check states surfaced inline: empty first field, duplicate, missing cloze, cloze on non-cloze note type.
- Tag field with completion (`complete_tag`).
- CLI: `tanki add --deck D --type Basic --field Front=… --field Back=… --tag t`, or from a file/stdin in the note file format (8.5).

### 8.5 Editing fields (R7)

Anki's Rust code stores and validates notes but has nothing for producing HTML: no editing model, no paste sanitiser, no HTML↔Markdown conversion (R7).

Stored field HTML is raw Chromium output: `a<div>b</div>` for Enter, `<br>` for Shift+Enter, `&nbsp;`, `<img src="name.png">`, `[sound:x.mp3]`, MathJax as plain `\( \)`, clozes as `{{c1::text::hint}}` (R7; the `<div>`/`&nbsp;` details are from known browser behaviour, not a test run).

Safety rule for every editor mode: **write back only fields the user changed; markup the editor does not understand is kept as an opaque token.**

| Mode | Phase | Description |
|---|---|---|
| `$EDITOR` round-trip | 3 | Note(s) as a Markdown-style file with front matter (note type, deck, tags, one heading per field), after apy's format (R9). Supports bulk edits; checks for concurrent modification; raw-HTML fallback per field |
| Built-in raw HTML field editor | 3 | ratatui-textarea with live rendered preview; also the mode for fields flagged "HTML editor by default" |
| Built-in markup editor | 6 | Light markup projection with opaque tokens. Ships only after a "no edit ⇒ identical bytes" test passes on real decks |

Editor commands: bold/italic/underline, colours, lists, cloze insert (new number / same number), MathJax inline/block/chemistry wrappers, attach media, record audio.

- Next cloze number comes from Anki's own cloze-number set, not a regex; desktop's buttons miss multi-ordinal clozes (R7).

Attaching media:

- Path prompt with completion; pasted path detection (this is what drag-and-drop onto a terminal produces).
- Clipboard image via arboard (local). Over SSH, the kitty clipboard protocol works only on kitty and Ghostty 1.4 (R8).
- URL fetch via Anki's `retrieve_url`.
- Always use the filename Anki returns: new files are lower-cased, and the name may differ from the request (R5, R7).

### 8.6 Note types (R7 §4)

| Feature | Verdict | Notes |
|---|---|---|
| Add from stock, rename, delete | T | |
| Clone | D | Rewrite; desktop does a Python deep copy |
| Fields: add, rename, delete, reorder, sort field, RTL, HTML-by-default, collapse, exclude from search, description | T | Keep each surviving item's position; Anki diffs and rewrites notes |
| Card templates: front/back/CSS editing, add/remove/rename/reorder, browser appearance, deck override | D | Built-in editor or `$EDITOR`, with live preview via `render_uncommitted_card` |
| Template validation errors | T | Returned by Anki (parse error, duplicate front, unknown field, missing cloze) |
| Change note type with field/template mapping | D | Mapping form |
| LaTeX header/footer, SVG flag | T | |
| Restore to default | T, guarded | Also replaces fields, so data in extra fields is lost (R7); require typed confirmation |

Guards tanki must add because they exist only in desktop's UI:

- **"This requires a full sync" confirmation** before adding/deleting/reordering fields or templates, changing the sort field, deleting a note type, or changing a note's type (R5, R7).
- **Required-field protection** for cloze and image-occlusion fields; Anki's core does not enforce it (R7).

### 8.7 Browser and search (R6)

- The search language (parser, SQL generation, node building) is all in Anki's Rust code. R6 §1.3 has the full term table.
- Search returns the whole id list in one blocking call; rows are fetched lazily per visible row and cached. Only the Question/Answer columns trigger rendering.
- 19 columns plus custom; cards mode and notes mode. Row colour precedence: flag > marked > suspended > buried.
- Sidebar tree is built client-side from Anki's deck/tag/note-type trees plus fixed lists (Today, Card State, Flags); saved searches under the same config key as desktop. Modifier-clicks become separate keys: replace, AND, OR, negate.
- Actions: change deck, change note type, add/remove tags, clear unused tags, mark, flags, suspend, bury, forget, set due date, reposition, grade now, delete, select all/invert, preview, card info, export selection, filtered deck from search.
- Find and replace: Anki call. An empty id list means **all notes** (R6), so the UI always passes explicit ids and the CLI needs `--all`.
- Find duplicates and the reposition dialog's position range are Python-only; rewrite both (R6).
- Never expose the raw-SQL custom sort mode (R6).
- Reuse desktop's config key names for columns and sort so settings sync (R6).
- CLI: `tanki search 'deck:Spanish is:due' --notes --format json`.

### 8.8 Statistics and card info (R6 §7)

- One call returns raw aggregates for 14 graphs. All maths after it is TypeScript and must be ported: binning, percentiles, streaks, table summaries, range filters.
- Rendering: eighth-block bars, five-colour stacked bars, pie as one stacked bar, calendar heatmap as coloured cells, forgetting curve as braille. Optional bitmap charts later.
- Card info rows and review log are plain tables. The card-stats call writes back to the card, so it is not a pure read (R6).
- CLI: `tanki stats --format json|svg`.

### 8.9 Sync — self-hosted only (decision 2; R5 §3)

tanki syncs only with a server URL the user configures. Hosts under `ankiweb.net` are refused. Sync is off for a profile until a URL is set.

| Step | Notes |
|---|---|
| Configure | `tanki sync login --url https://host[:port]/` prompts for username/password, calls `sync_login`, stores the returned host key (4.5). If the server answers with a redirect, persist the new endpoint |
| First-time warning | If the collection has synced before (non-zero last-sync time), warn that the first sync with this server is a one-way full upload or download and all other devices must switch too (decision 2 consequences) |
| Normal sync | Outcomes: no changes, normal, full upload, full download, or a conflict needing a choice |
| Conflict | Prompt in the TUI; CLI exits with a distinct code and accepts `--upload` / `--download` (after aktui, R9) |
| Full download | Take a backup first, as desktop does. Only while holding the collection lock (4.4) |
| Schema-change guard | Before any operation that forces a full sync, show "this will require a full upload" and require confirmation. Anki's core sets the flag but the prompt exists only in desktop's UI (R5, R7) |
| Media sync | Runs after collection sync; the CLI must poll the media sync status until inactive, because that is the only place a background error surfaces (R5) |
| Auto-sync | On open and close, plus periodic media sync; off by default, configurable per profile |
| Abort | Supported through the shared progress state; afterwards close and reopen the collection (4.3) |
| TLS | `rustls`; the custom-certificate call exists in Anki for self-signed servers, though desktop never uses it (R5) |

**`tanki sync-server`** (Phase 2)

- Anki's built-in sync server (`SimpleServer`) is compiled into the library anyway (R1, R5). tanki exposes it with flags for bind address, port, data folder and users, mapping to the `SYNC_*` settings the server reads.
- It serves plain HTTP. Document putting it behind a TLS reverse proxy or a private network (for example Tailscale) before exposing it.
- Anki desktop, AnkiDroid and AnkiMobile can all be pointed at a custom sync server in their own settings, so this one server can replace AnkiWeb for every device. Anki's docs call third-party servers untested (R9); tanki's is Anki's own server code.
- The server needs its own data folder. It must never be pointed at a profile folder.

### 8.10 Media management (R5 §4)

- Check Media: unused, missing, trash, restore. It rewrites note HTML and has no dry run, so tanki backs up first and says what will change.
- Media sync detects changes by folder modification time, so files edited in place are missed (R5); document it.

### 8.11 Import and export (R5 §5–6)

| Format | Direction | Verdict | Notes |
|---|---|---|---|
| `.apkg` | Import, export | T | Options: merge note types, update conditions, scheduling, deck configs. Import options persist into the collection config |
| `.colpkg` | Import (replace), export | D | Needs the collection closed; lock and back up first (4.4) |
| CSV / TSV | Import | D | Header directives, delimiter detection, field-mapping form, duplicate handling |
| Notes / cards as text | Export | T | |
| JSON (`ForeignData`) | Import | T | The hook for any external converter |
| Mnemosyne | Import | D, low priority | Only remaining Python importer |
| Shared decks | — | T | Opens ankiweb.net in a browser; then import the file |

### 8.12 Maintenance (R5 §7)

- Check Database (includes vacuum; some repairs force a full sync; clears undo).
- Empty Cards with the keep-notes rule (small rewrite).
- Backups: Anki's retention scheme (12 daily, 10 weekly, 9 monthly, 30-minute minimum interval). Short CLI runs must wait for the backup thread or a backup is cut off mid-write (R5).
- Restore from backup = collection-package import.

### 8.13 Preferences and profiles (R4, R5)

- Collection-stored preferences: full form.
- Qt-only settings (video driver, UI scale, widget style, window geometry) do not apply.
- Profiles: list, create, rename, delete, switch; tanki's own metadata, not `prefs21.db`.
- Theme: follow the terminal's dark/light report; night-mode card class toggles with it.
- UI language: Anki's translations cover backend strings; tanki's own UI strings start English-only.

### 8.14 Add-on substitutes (R9)

- **AnkiConnect-compatible HTTP API** on port 8765 (`tanki serve`, or while the TUI runs). It is the one programmatic access path AnkiWeb's terms endorse, and it lets Yomitan, browser extensions and editor plugins add cards. Upstream has about 110 actions; start with the ~25 that mining tools use (`version`, `deckNames`, `modelNames`, `modelFieldNames`, `canAddNotes`, `addNote`, `storeMediaFile`, `findNotes`, `notesInfo`, `updateNoteFields`, `multi`, …). The exact subset is a guess; verify by running Yomitan against a stub. The server must own the collection, so it conflicts with desktop running.
- Native equivalents of popular add-ons: review heatmap (in stats), TTS, extra browser columns.

---

## 9. Command-line surface

```
tanki                          full TUI
tanki review [DECK]            jump straight into study
tanki decks [--json]           deck tree with counts
tanki add [...]                add a note from flags, a file, or stdin
tanki edit <query|id>          open matching notes in $EDITOR
tanki search <query> [--notes] [--format table|json|ids]
tanki sync login --url URL    configure a self-hosted server
tanki sync [--upload|--download] [--no-media]
tanki import <file> [options]
tanki export <file> [--deck D | --query Q] [options]
tanki stats [--deck D] [--format json|svg]
tanki check db|media|empty-cards
tanki backup [create|list|restore]
tanki serve                    AnkiConnect-compatible API
tanki sync-server              self-hosted sync endpoint
tanki doctor                   terminal capability report + test image
tanki completions <shell>
```

Global flags: `--base`, `--profile`, `--json`, `--graphics`, `--no-color`. Exit codes distinguish "collection locked by another program", "full sync choice required", "auth required" and "sync not configured".

---

## 10. Testing

- **Fixtures only.** Tests run on synthetic collections under temp `--base` folders, never a real profile.
- **Golden card output.** Generate question/answer HTML and audio-tag lists from Anki's own Python package (the `anki` wheel at the pinned version, in a throwaway virtualenv) for a fixture collection; assert tanki's pipeline matches. This settles the `partial_render` question and catches drift on every Anki bump.
- **Renderer snapshots.** Cell-buffer snapshots for the stock note types and a corpus of editor-produced HTML; bitmap hashes for math and occlusion compositing.
- **Editor round-trip.** "Open and save without changes ⇒ identical bytes" over a corpus of real field HTML.
- **Sync integration.** Run Anki's built-in sync server in-process; test normal sync, conflict, full upload/download, abort.
- **Lock behaviour.** Two-process tests for the "already open" path and restore safety.
- **Upgrade gate.** Bumping the Anki pin runs all of the above plus a compile check of every service call used.
- **Terminal matrix (manual, per release).** kitty, Ghostty, WezTerm, foot, Alacritty; each also inside tmux, zellij and herdr; one SSH run. `tanki doctor` output recorded per cell.
- **Fuzzing** of the HTML/CSS walker.

---

## 11. Phases

Work the phases in order. A phase is done when every task is ticked and its exit test passes. Sizes are relative (S/M/L), not time estimates. "Streams" are groups of tasks that touch different crates and can be built in parallel once the phase's shared interfaces are agreed.

### Phase 0 — foundations and risk spikes (M)

Read first: R1 (all), R5 §0–§3, R8 §1–§2, Appendix A.

Tasks, in order (mostly sequential; little to parallelise):

- [x] `git init`; `LICENSE` (AGPL-3.0-or-later); `README.md` with the unofficial disclaimer; `.gitignore`.
- [x] Confirm `protoc` is available (4.1 prerequisites). If not, stop and ask the user to install it.
- [x] Add `vendor/anki` submodule at `4aa9c0836` with the two `ftl` submodules; `rust-toolchain.toml`; seed `Cargo.lock`; workspace skeleton from 4.2 with empty crates.
- [x] `xtask setup`: init submodules, apply `patches/anki/*.patch` idempotently.
- [x] Write patch 0001 and prove it compiles; if it cannot, implement the `Backend` hand-over fallback (4.3) and record the outcome in this plan.
- [x] `tanki-core`: collection actor (thread + channels), open/close with explicit media paths, typed commands for deck tree, queued cards, render, answer, undo; `OpChanges` broadcast.
- [x] Fixture generator in `xtask`: builds synthetic collections in a temp folder using only tanki-core (notes of every stock note type, media files, a few reviews).
- [x] Guard: the binary refuses to open a base folder that is not a temp/fixture folder unless `TANKI_ALLOW_REAL_PROFILE=1` is set (removed at Phase 2 exit; see section 14).
- [x] Spike A — sync with public functions only: run Anki's `SimpleServer` in-process on a temp folder, then login, normal sync, full upload, full download, media sync, abort. Record what worked.
- [x] Spike B — golden render harness: script that renders every fixture card with Anki's Python package at the pinned version and stores question/answer HTML plus audio-tag lists; a Rust test compares tanki-core's pipeline (6.0). Settle `partial_render`.
- [x] Spike C — schema edges: open a schema-11 fixture (upgrade path and pre-upgrade copy); behaviour with a schema newer than supported (simulate by editing a copy's version field).
- [x] Spike D — desktop version skew: have the 25.09.4 Python package open and save a fixture collection written by tanki-core (deck config edited, cards answered); diff what changed. Document any dropped fields.
- [x] Spike E — `tanki doctor`: capability probe (5.2) and a test image through each protocol. The user runs it in herdr, kitty and Ghostty and reports what displays; the agent cannot see the terminal.
- [x] Evaluate bumping the pin to the newest stable upstream tag; keep the bump only if everything above still passes. **Kept: 26.09.4.**
- [x] CI: build, tests, patch-apply check, `cargo deny`/licence check. Written as `.github/workflows/ci.yml` with a licence-metadata script in place of `cargo deny` (not installed; installing it means downloading a binary). Every step passes locally; the workflow itself has never run because the repository has no remote.

Exit test: `cargo test` green including golden renders and the in-process sync tests; spikes A–E each have a written result in section 13; `tanki doctor` output from the user's three terminals recorded.

### Phase 1 — study MVP (L)

Read first: R2 §0–§2, §8–§9; R3 §0–§4, §6–§8; R8 §1–§3, §5; R4 reviewer and deck-browser sections.

Agree first (one short design step): the `tanki-core` command/response types for study; the `tanki-render` input (`CardSide { html, css, body_classes, media_dir }`) and output (`RenderedCard { lines of styled spans, graphics blocks, widgets }`); the `tanki-media` player interface.

- [x] Shared interfaces committed (`5ac8cf9`); contracts and stream ownership are in `docs/implementation/phase-1-interfaces.md`.

Streams (parallel after the interfaces exist):

- **Core** (`tanki-core`):
  - [x] Card pipeline per 6.0: render, `{{FrontSide}}`, audio-tag extraction, type-answer placeholder detection, `compare_answer`.
  - [x] Study commands: queue, counts, next-state labels, answer with timing and `custom_data` copy, undo/redo.
  - [x] Card actions: flags, mark, bury card/note, suspend card/note, forget, set due date, delete note.
  - [x] Deck tree with counts, overview data, congrats info, studied-today line.
  - [x] Scheduler-version check and upgrade call.
- **Renderer** (`tanki-render`):
  - [x] html5ever parse; CSS subset cascade including `display:none`, class selectors, night-mode override (6.0 step 5).
  - [x] T0 layout per 6.2: inline styles, blocks, lists, tables, alignment, `<hr id=answer>`, sub/superscript, links.
  - [x] Cloze spans, hint and `<details>` toggles, replay-button widgets, type-answer diff colours.
  - [x] Image blocks: decode (jpeg, png, gif first frame, webp, svg via resvg), scale to cell pixel size, cache, off-thread.
  - [x] Snapshot tests for all six stock note types (R3 §6). IO shows a mask-unavailable notice until the Phase 4 compositor; its image must not be treated as a completed occlusion rendering.
- **Terminal** (`tanki-term`):
  - [x] Finish capability probe; ratatui-image integration with Unicode-placeholder, iTerm2, Sixel and half-block modes; `--graphics` override; live mode cycle. (`probe`, `Images`; encoding is synchronous on a cache miss, 10–50 ms per image depending on size. ratatui-image never deletes kitty images, so the TUI writes `Images::take_cleanup()` on screen changes. Inside tmux, ratatui-image runs `tmux set -p allow-passthrough on` for its pane.)
- **Media** (`tanki-media`):
  - [x] Audio queue with Anki's autoplay/replay/interrupt rules; built-in player; mpv IPC backend when present; "no audio device" state. (rodio 0.23 with mp3/aac/mp4/alac/flac/vorbis/wav; needs the ALSA headers `libasound2-dev` to build. Text to speech is reported as unsupported until Phase 4.)
- **TUI/CLI** (`tanki-tui`, `tanki-cli`) — depends on the others' interfaces:
  - [x] App shell: event loop, status line, keymap with per-screen scopes, command palette, help overlay. Remapping/unbinding uses `--keymap FILE` JSON; automatic TOML config loading belongs to Phase 2.
  - [x] Decks, overview, review, congrats screens; image zoom view; "open card in browser" escape hatch. OSC 8 links are emitted separately from cell symbols; href-only changes, overlays and image cells have regression tests.
  - [x] Reviewer state machine (question → answer → transition), timer, time-box prompt, auto-advance, leech notice, day-rollover refresh.
  - [x] `tanki review`, `tanki decks`.

Exit test: on a fixture collection, a scripted session answers every due card and the resulting card states and review log equal those produced by Anki's Python package given the same answers and timestamps. Manual: the user reviews a fixture deck with images and audio in their terminal.

- [x] Automated exit: final integrated run recorded 59 answers in lock step with Anki 26.09.4; all card fields and exported answer-log scheduling fields exactly equal (34 cards, 64 logs, 24 notes). Full workspace build, tests and strict clippy pass.
- [x] Manual exit: user confirmed doctor image, fixture image/colour display, tone playback and terminal restoration after `review 'Study::Media'` (2026-11-13; terminal name not supplied).

### Phase 2 — sync and data safety (M)

Read first: R5 (all), R1 §4–§5, R9 §3.

Streams:

- **Safety** (`tanki-core`):
  - [x] First-open procedure (4.4): version peek, pre-upgrade copy, forced backup. (The backup is forced only for collections not in the config's seen list; later opens follow Anki's schedule.)
  - [x] Lock-error mapping and exit code; two-process tests. (Exit 20 for the SQLite lock and for `/proc` holders, `CoreError::InUse`.)
  - [x] Desktop-is-running checks that do not rely on the SQLite lock (4.4 correction): refuse on an existing `collection.anki2-wal` before the peek, with a confirmed-closed override (`--confirm-closed`, exit 25); on Linux, refuse when another process holds `collection.anki2` (`/proc`). Two-process tests where the holder has already made a backup.
  - [x] Backups: Anki's schedule/retention, wait-on-exit, list, restore (lock + backup first). (Written atomically; see 13.)
  - [x] Schema-change confirmation hook used by every caller that can trigger it. (Phase 2 caller: `.apkg` import. Later phases' Check Database and note type changes must use it too.)
- **Sync** (`tanki-core`, `tanki-tui`, `tanki-cli`):
  - [x] Everything in 8.9: configure/login, AnkiWeb host refusal, first-time warning, normal sync, conflict, full upload/download, media sync with status polling, abort, auto-sync, progress UI. (TUI: `y` syncs from the deck list and overview, with progress, cancel on Esc or Ctrl+C, and the upload/download choice; login stays in the CLI. Import `I`, export `E`, backup `B`, restore `R`. Auto-sync runs when the TUI opens and closes; periodic media sync is not done. Credentials live in a 0600 file; the OS keyring is deferred. CLI Ctrl+C kills instead of cancelling.)
  - [x] `tanki sync-server` with docs (`docs/sync-server.md`).
- **Packages** (`tanki-core`, `tanki-cli`, `tanki-tui`):
  - [x] `.apkg` import with options and result log; `.apkg` export; `.colpkg` export and restore. (Legacy `.colpkg` export is refused: it would leave the live collection at schema 11.)
  - [x] `tanki sync`, `tanki import`, `tanki export`, `tanki backup`.
- [x] Remove the `TANKI_ALLOW_REAL_PROFILE` guard, and the hard refusal of real Anki base locations in `safety.rs`, once the exit test passes.

Exit test (automated, in-process server): two clients — one driven by Anki's Python package, one by tanki — share a server; edits and reviews made on each arrive on the other with no full-sync prompt and identical media. Plus: restore-while-locked is refused; a killed process leaves no partial backup.

After this phase the user can run tanki on their real profile.

### Phase 3 — add, edit, browse (L)

Read first: R7 §0–§3, §6; R6 §1–§6; R9 §8.

Streams (contracts, ownership and stream results: `docs/implementation/phase-3-interfaces.md`):

- **Notes** (`tanki-core`, `tanki-tui`):
  - [x] Add screen per 8.4: defaults, note type/deck pickers with field carry-over, field checks inline, sticky fields (persisted, no full sync), 15-entry session history, discard prompt, IO refused. (`notes_ops.rs`, `add_ui`.)
  - [x] Edit current note (reviewer `e`): changed fields only, conflict prompt with reload.
  - [x] Tags with completion (shared `completion.rs`, `CompleteTag`).
- **Editors** (`tanki-core`, `tanki-tui`, `tanki-cli`):
  - [x] Note file format: parser, writer, planning helper; byte-exact round trip, robust to CRLF, stripped trailing whitespace, BOM. Unwritable values use a per-field `{quoted}` JSON-lines form instead of R7 §6.4's fence.
  - [x] `$EDITOR` flow (`editor_flow`), changed-fields-only, all-or-nothing write-back in one undo step (`writeback.rs`); reopen with errors; edited file kept on conflict.
  - [x] Raw-HTML field editor (own `textarea.rs`; no ratatui-textarea) with live preview (`RenderUncommittedCard`).
  - [x] Editor commands: bold/italic/underline, colour (`<span style="color: rgb(…)">`, as desktop stores it), lists, cloze new/same number from Anki's cloze-number set, MathJax inline/block/chemistry.
  - [x] Media attach: path prompt with completion, pasted/dropped path, clipboard image via `wl-paste`/`xclip` (no arboard), URL via `retrieve_url`. Audio recording is not done.
- **Browser** (`tanki-core`, `tanki-tui`):
  - [x] Search bar with term/deck/tag/note-type completion and history (saved in tanki's config).
  - [x] Virtualised table (visible rows ± 20 fetched, cached; tested on 50k ids), column chooser (no reordering yet), sort by built-in columns only, cards/notes modes, row colours; settings under desktop's keys.
  - [x] Sidebar tree in `SidebarStage` order, with replace/AND/OR/negate keys, collapse state under desktop's keys, saved searches; desktop's default search text.
  - [x] Preview pane (text tier; no images yet).
  - [x] Bulk actions from 8.7 (change deck, tags, clear unused tags, mark, flag, suspend, bury, forget, set due, reposition, grade now, delete, select all/invert); find and replace with explicit ids (empty refused before Anki is called). Not done here: find duplicates, change note type (Phase 5 guard), export selection, card info and filtered deck from search (both "not available yet (Phase 5)").
- **CLI**:
  - [x] `tanki add` (flags, `--file`/stdin, `--edit`), `tanki edit` (`$EDITOR` or `--file`; exit 26 on conflict), `tanki search` (`--format table|json|ids`, built-in sort), `tanki delete` (`--yes`).

Exit test: byte-identical round-trip test over a corpus of field HTML (stock editor output shapes from R7 §2 plus adversarial cases); add → search → edit → delete scripted through the CLI on a fixture.

- [x] Automated exit: `tanki-core/tests/notefile_phase3.rs::every_corpus_value_round_trips_byte_for_byte` (83-case corpus in `tests/fixtures/field-corpus/corpus.json`, also after editor manglings) and `tanki-cli/tests/notes_cli.rs::exit_add_search_edit_delete` pass, with the full workspace build, tests and strict clippy.
- [x] Manual: the user ran the checklist (add screen, `$EDITOR` hand-over, browser, edit current in review) on a fixture in their terminal and reported that it worked (2026-11-14; reported as a whole, not step by step; terminal name not supplied).

### Phase 4 — rich rendering (L)

Read first: R3 §1 (filters), §4–§5, §7–§8; R8 §3–§6.

Streams (independent of each other; results: `docs/implementation/phase-4-interfaces.md`, `phase-4-media.md`):

- [x] **Math**: RaTeX → SVG → bitmap blocks; Unicode inline path (native converter; `unicodeit` failed the licence gate, 6.3); multi-row inline boxes; legacy LaTeX image display with night-mode inversion; opt-in generation of missing LaTeX images (`--generate-latex`).
- [x] **Image occlusion review**: shape parsing from the rendered hidden elements, compositing per R3 §5, both modes, colours from the note type's CSS variables; Shift+M toggles masks.
- [x] **Text extras**: ruby, app-side bidi, nested tables, animated GIF, OSC 66 sizing on kitty 0.40+ (off through tmux).
- [x] **T2 tier**: Blitz bitmap rendering behind the `blitz` cargo feature; note-type classifier; per-note-type tier override (Ctrl+T, saved in tanki's config); next-card pre-render.
- [x] **Media extras**: TTS backends; video (external mpv window default, `--video-output=kitty|sixel|blocks`); record (`v`) and replay (Shift+V) own voice.

Exit test: snapshot/bitmap tests for each stream; a fixture image-occlusion deck is reviewable end to end; classifier unit tests over sample templates.

- [x] Automated exit (lead, 2026-11-15): full workspace build with all features, 472 tests (0 failed) and strict clippy pass; the licence gate and replay guards pass. TUI tests review all six `Study::Occlusion` cards through reveal, rating and undo.
- [ ] Manual: the terminal checklist in `docs/implementation/phase-4-interfaces.md` (occlusion colours, math, GIF, Ctrl+T, TTS/video/recording) is pending the user.

### Phase 5 — management (L)

Read first: R2 §4–§6; R7 §4; R6 §7–§8; R5 §5–§7; R4 preferences and tools sections.

Streams (independent screens over existing core plumbing; contracts, ownership and results: `docs/implementation/phase-5-interfaces.md`):

- [x] **Deck options**: full form (8.3), presets, per-deck overrides, FSRS optimise/evaluate/reschedule with progress, simulator chart.
- [x] **Decks tools**: filtered decks, custom study, unbury modes, deck description. (Also create/rename/"Move to…"/delete prompts on the deck list.)
- [x] **Note types**: list/add/clone/rename/delete; fields editor; templates editor with live preview and validation errors; change note type mapping; guards from 8.6.
- [x] **Statistics**: port the graph maths; 14 panels; card info with forgetting curve; JSON/SVG export. (`tanki stats`; no PDF.)
- [x] **Import/export extras**: CSV import with mapping; text exports; JSON import; find duplicates; reposition; grade now. (Reposition and grade now were already complete in Phase 3's browser, apart from the gaps listed below.)
- [x] **Maintenance**: check database, check media, empty cards; preferences; profile management.
- [x] Every screen reachable from the app (`app/manage.rs`, keys in `keymap.rs`); profile switch from the Tools screen through `terminal::run`'s return value.

Exit test: every row of section 8 marked T or D is implemented or listed as deferred in this plan with a reason.

- [x] Automated exit (lead, 2026-11-15): section 8 audit of every T/D row (41 rows), 39 implemented, 2 deferred (below). Full workspace build with all features, 673 tests (0 failed), strict clippy, licence gate and replay/wrapper script tests pass.
- [ ] Manual: the terminal checklist in `docs/implementation/phase-5-interfaces.md` is pending the user.

Deferred from section 8, with reasons:

- **Custom scheduling JavaScript** (8.1): deferred from Phase 5, completed in Phase 6 with QuickJS. Deck options explains execution; the reviewer warns on script failures.
- **Mnemosyne import** (8.11, "D, low priority"): not done. It is the last Python-only importer and its users are few; a Mnemosyne collection can be converted to JSON (`ForeignData`) or imported once with desktop. Revisit on request.

Not T/D rows, noted for completeness: the browser's full-screen preview (the preview pane shows both sides in the text tier); the reposition dialog's queue top/bottom hint (desktop reads it with raw SQL, rule 4) and saving randomise/shift as new defaults; periodic media sync during a session (Phase 2).

### Phase 6 — ecosystem and polish (M–L)

Read first: R9 §5; R2 §3; R7 §5; R8 §3, §8–§9.

Streams (independent), in priority order (set 2026-11-15 by value to a daily terminal user). Do the priority streams; the optional ones only on request.

Priority:

1. [x] **Packaging** (small): release binaries (glibc on Linux, because fully static musl breaks PipeWire/Pulse audio, R8), `tanki completions <shell>`, man pages, install instructions. Homebrew/AUR/Nix recipes only when publishing.
2. [x] **AnkiConnect-compatible API** (`tanki-connect`, `tanki serve`): start with the subset in 8.14; verify against a real client. Worth it only for users who add cards from other tools (Yomitan, browser extensions, editor plugins).
3. [x] **Custom scheduling JS**: QuickJS with protobuf-JSON parity, async jobs, context/custom-data support, resource limits and failure warnings. Matters only for collections whose custom scheduling script is non-empty (deck options → Advanced); the user checks their own profile (rule 1).

4. [x] **Neovim saved code-tour cards** (user requested, 2026-11-15): typed note references, held review, Herdr reader launch and lazy Claude connection. **Moved out of tanki on 2026-11-16 into the standalone `tanki-code-tour` plugin** (sibling repository); tanki keeps only the generic plugin host. The CLI `tour add/attach/check/install-note-type` commands and the Alt+G picker were dropped (AnkiConnect lacks `createModel`/`findNotes`/`updateNoteFields`).
5. [x] **Plugins** (user requested, 2026-11-16): out-of-process plugins with a closed hook catalogue and user-granted manifests; code-tour becomes the first plugin. Phases 2-4 done (host crate, TUI hooks and hold, code-tour migrated to the standalone `tanki-code-tour` repo); the real-terminal Herdr/Neovim/Claude check is manual. See `docs/implementation/plugins.md`.

Optional (low value in a terminal; existing paths cover them):

- **T3 tier**: headless Chrome over CDP behind a cargo feature. Only for cards that need JavaScript; the browser escape hatch (`w`) already handles them.
- **Image-occlusion creation**: rectangles first. Usually done once, in desktop.
- **Built-in markup editor** (8.5), gated on the byte-identical test. `$EDITOR` and the raw-HTML editor already cover editing.

---

## 12. Risks

| Risk | Likelihood / impact | Mitigation |
|---|---|---|
| Data loss on the user's real, shared profile | Low / severe | 4.4 rules; development guard until Phase 2 exit; backups before every destructive step; two-process tests |
| Desktop (25.09.4) and tanki (newer core) writing one collection | Unknown / medium | Phase 0 spike D; one-time notice to update desktop |
| Desktop and tanki open on one profile at the same time, because Anki's backup drops the file lock (4.4 correction) | Likely once desktop has run 30 minutes / severe | tanki reopens after its own backups; Phase 2 adds the WAL-file and `/proc` checks (4.4 correction) before the guard is lifted |
| Two sync servers on one collection | Likely if desktop keeps using AnkiWeb / medium | Sync off by default; first-time warning (decision 2 consequences) |
| Anki's Rust API changes under us | Certain / medium | Single adapter crate, pinned commit, upgrade gate |
| Patch 0001 does not compile or stops applying | Low / low | `Backend` hand-over fallback (4.3) |
| Card rendering fidelity disappoints on heavily styled decks | Likely for some decks / medium | Tiers T2/T3, per-note-type override, browser escape hatch |
| Images fail inside multiplexers (herdr has open bugs) | Likely / medium | Query-based detection, live mode switch, `tanki doctor`, test matrix |
| Blitz and RaTeX are young (0.3 beta, 0.1) | Medium / low–medium | Both optional behind tiers; text fallbacks exist |
| Trademark objection to the name | Unknown / medium | Disclaimer (decision 1); the binary name is the only place it appears |
| Custom scheduling JS users get different scheduling | Low share / high for them | QuickJS executes the transform; script failures preserve standard states and warn |
| Lossy field editing corrupts notes | Medium without care / high | Changed-fields-only rule, opaque tokens, byte-identical test |
| Timezone shift when running over SSH in another zone | Low / medium | Detect and warn (8.1) |
| Self-hosted sync server exposed over plain HTTP | User-dependent / high | Docs and a startup warning when binding to a non-loopback address |

---

## 13. Unverified items and spike results

Each item names the phase that resolves it. Replace the entry with the result when it is known.

| Item | Source | Resolved in |
|---|---|---|
| **Study commands and Python parity (2026-11-13).** Core pipeline, all listed card actions, undo/redo, timing caps, custom-data preservation, deck screens and scheduler upgrade pass fixture tests. Lead ran the full workspace build, tests and clippy after the core merge. Live replay with the pinned Python package: 59 answers; queue order/counts identical at every step; 34 cards, 64 exported review-log rows and 24 notes exactly equal, including learning due times, modification times and FSRS state. No tolerances or normalization in tanki's comparison. Upstream dataset export excludes manual scheduling-only log entries and normalizes log USNs; those sync metadata are outside this check. See `tests/study_replay.rs` and `scripts/study-replay.py`. | Phase 1 exit test | Phase 1 core verified |
| **T0/T1 renderer (2026-11-13).** Lead's full workspace build/test/clippy passed after merge. 25 renderer unit tests, 10 integration tests, ten cell-buffer snapshots from stock golden HTML and editor markup; cascade/hiding/night override, widgets, narrow/CJK/bidi layout, local CSS/media paths, image cells, all five required codecs and asynchronous cache checked. Raw terminal controls are filtered while combining characters and ZWJ emoji survive. IO explicitly reports missing masks (Phase 4); MathJax stays source text until Phase 4. User confirmed fixture images and colours in the manual session; the broader terminal/multiplexer matrix remains a release check. | R3, R8 | Phase 1 renderer and manual exit verified |
| **Licence metadata gate corrected (2026-11-13).** Cargo metadata includes disabled optional terminal backends, so the gate now selects the active all-features Linux graph through `cargo tree`. Conservative expression policy remains; an exact-version choice records Apache-2.0 for `self_cell` 1.3.0, which ships that licence alongside its alternative GPL-2.0-only option. Five gate tests and 512 active packages pass. `unicase` remains pinned to 2.6.0. | 4.1, rule 17 | Phase 1 integration verified |
| **TUI/CLI and terminal integration (2026-11-13).** 57 TUI unit tests (including nine OSC 8 regressions), two fixture integration tests using the real actor/renderer/image decoder, CLI unit test and ten CLI integration tests pass. Screen cell snapshots cover decks, overview, review, typing, congrats, zoom and overlays. Tests cover timer/timebox/auto-advance/leech/rollover, stale transition keys, error-path collection closure and remapping. User confirmed the doctor image and Study::Media image/audio review, including terminal restoration. `cargo xtask` fixture tooling additionally exposes `study-fixtures --base TEMP`. Linux audio needs pkg-config and ALSA development headers; CI and README now include them. | R2, R3, R4, R8 | Phase 1 complete |
| Patch 0001 compiles; tanki-core test passes shared state to the builder and converts idle/media-check progress to typed protobuf variants. Setup applies once, skips on rerun, rejects drift. | 4.1 | Phase 0 foundation verified |
| **Sync works with public functions only (2026-11-13).** Against `SimpleServer` in-process on loopback: login (wrong password refused), first normal sync reports full sync required, full upload, full download (fixture only, backup first), normal sync in both directions between two clients, media sync (files byte-identical), typed progress for all three through patch 0001. Abort: `want_abort` gives `Interrupted` and a clean rollback on the same connection; dropping the future needs close-and-reopen, after which committed notes are intact and the next sync passes. `sync_login` returns an auth with no endpoint (which would mean AnkiWeb); Anki follows 308 redirects itself and reports `new_endpoint`, which Phase 2 must validate before saving. Server users come from `SYNC_USER*` variables read at start. Test: `actor::sync_spike::public_sync_round_trip`. | R1, R5 | Phase 0 spike A done |
| **`partial_render` must be `true` (2026-11-13); R3 was right, R2 wrong.** `scripts/golden-render.py` renders a 22-card fixture with Anki's Python package built from the pinned source and stores question, answer, CSS and audio tags in `tests/fixtures/golden/render.json`; `tests/golden_render.rs` requires tanki-core to match exactly. With `false`, 19 fields differ: every `{{FrontSide}}` answer replays the question's audio or TTS, and built-in filters placed after an unknown filter get applied when desktop skips them. Three pipeline steps were missing and are now in `render`: a template error becomes the text of both sides; sound filenames are cut to their last path part; `[latex]`, `[$]`, `[$$]` become image tags, with Anki's "generation disabled" message when the image is absent. Collections are opened with real translations so localized card text matches desktop, isolation marks included. Not covered (they live in `aqt`): the `tts-voices` filter, add-on filters, reviewer-side media path encoding. | R2 vs R3 | Phase 0 spike B done |
| **Schema edges (2026-11-13).** Schema 11 file: tanki copies it byte for byte to `backups/tanki-pre-upgrade-<time>.anki2`, then the library upgrades it in place to 18; a non-empty WAL beside an old file is refused. Schema 19 (version field edited on a closed temp fixture): tanki's peek refuses before the library is called; the library on its own also refuses with `DbErrorKind::FileTooNew`; the file is unchanged in both cases. Test: `tests/schema_edges.rs`. | R5 | Phase 0 spike C done |
| **Version skew with desktop 25.09.4 (2026-11-13).** No stored field or key is new; only two enum values are (review order 12 and filtered-deck order 10, both "relative overdueness"). Across nine scenarios run with the real 25.09.4 Python package (open/close, deck options, legacy deck config, daily use, Check Database, Empty Cards, filtered decks, FSRS recompute) the schema stays 18, the schema-modified time is unchanged, the newer core reopens the file, and FSRS states recomputed by 25.09.4 equal the newer core's. Losses: Empty Cards deletes `{{#Tags}}`-style cards; the filtered deck dialog stores -1 for the unknown order. No guard needed, only the notice (section 2). Passes unchanged against 26.09.4. Not run: desktop's Svelte and Qt screens (the dialog's behaviour was checked with a bare Qt combo box), import/export, change note type, long FSRS histories. Test: `tests/version_skew.rs` (needs `TANKI_SKEW_PYTHON`; skips otherwise). Details: `docs/implementation/spike-d-version-skew.md`. | this plan | Phase 0 spike D done |
| Image display through herdr, kitty, Ghostty on this machine. **Partial (2026-11-13):** herdr (XTVERSION libghostty, TERM_PROGRAM=herdr) auto-detected Kitty direct placement, 15×34 px cells, Kitty keyboard flags 0, sixel unconfirmed; user confirmed the test image displayed. Plain Ghostty 1.3.0 (TERM=xterm-ghostty, outside herdr): same detection (Kitty direct placement, 15×34 px cells, 3735×1938 px text area, keyboard flags 0, sixel unconfirmed); user confirmed the Kitty-protocol image displayed. Standalone kitty terminal: user confirmed the test image displayed (output not pasted). **Spike E complete for all three terminals.** | R8 | Phase 0 spike E |
| User installed protoc 31.1; full workspace build and tests pass with it. Distro 3.21 was not tested and is no longer required for this environment. | 4.1 | Phase 0 foundation verified |
| **Build hash (2026-11-13).** Cannot be customised from tanki's build (see 4.3); it is empty, and the server accepted the resulting client string for every request. | 4.3 | Phase 0 spike A done |
| **Cross-process lock is lost after any backup (found 2026-11-13).** See the correction in 4.4. tanki now reopens after each backup; a running desktop that has already dropped its lock will be detected by the WAL-file and `/proc` checks decided in 4.4. Also seen: backup file names have one-second resolution, so two backups in the same second overwrite each other. | R5 | tanki side done in Phase 0; desktop-side checks are built in Phase 2 |
| The CI workflow on a real runner: never run (no remote). The runner's distro `protoc` is 3.21, which is untested here; Anki's proto files are plain proto3, so it should work | 4.1 | First push |
| **Two servers on one collection (2026-11-14): confirmed.** A collection synced with server A, pointed at server B that already holds data, reports `Full{upload, download}`; after uploading to B, going back to A again reports `Full{upload, download}`. Every switch forces a one-way full sync, so the first-time warning (shown only when `SyncReport::synced_before`) stands. Test: `tests/sync_phase2.rs`. | decision 2 | Phase 2 verified |
| **Phase 2 exit test (2026-11-14): passes.** `tests/sync_interop.rs` + `scripts/sync-interop.py`: one in-process Anki sync server on loopback; tanki (actor, full upload through the actor) and the pinned Anki 26.09.4 Python package (its own full download on a temp copy) sync three rounds of edits, marks, flags, reviews and media both ways. After the initial one-way sync no full-sync prompt appears; 26 notes, 36 cards and 13 review-log rows are exactly equal (all service fields including mod and usn) and 5 media files byte-identical. Restore-while-locked is refused at two levels: `replacement::tests` (a holder with SQLite's lock is refused by OFD locks; one that lost its lock after a backup by `/proc`) and `safety_phase2::restore_while_another_process_holds_the_file_is_refused` through the actor, live collection intact. A killed process leaves no partial backup: `safety_phase2::killed_process_leaves_no_partial_backup` kills a backup loop six times (two kills landed mid-write in the recorded run); every package in `backups/` is complete. | Phase 2 exit | Phase 2 verified |
| **Real profile, user-run (2026-11-14).** With Anki desktop (25.09.4, native launcher) open on the profile, `tanki decks` refused with exit 20 naming desktop's `python` process: the `/proc` check found the one process holding `collection.anki2` (QtWebEngine helpers hold nothing). An earlier run with desktop closed refused with exit 25 because a WAL file from an earlier unclean exit was present; after desktop opened and quit normally the WAL was gone and `tanki decks` listed the collection (exit 0, first-open backup). Cosmetic: deck names longer than the 36-column field push the counts out of alignment in `tanki decks`. | 4.4 | Phase 2 manual check passed |
| **TUI data screens, user-run on a fixture (2026-11-14).** From the deck list: `y` shows "sync is not configured", `B` reports the backup, `I` opens the path prompt, `R` lists backups. Not yet confirmed by eye: the import options form and result log, the restore confirmation, Ctrl+C cancelling a running operation (all covered by cell-buffer snapshots and fake-backend tests in `app/data_tests.rs`). | 7, 8.9, 8.11 | Phase 2 manual check (partial) |
| **Anki's backups are not atomic (found 2026-11-14; the source wins over R5).** `export_colpkg_from_data` creates `backups/backup-<time>.colpkg` with `File::create` and fills it from a thread, so a killed process leaves a truncated package where desktop's "Open Backup" lists it. tanki has Anki write into `backups/.tanki-incoming/`, syncs and renames the finished file into `backups/`, and applies Anki's interval check and retention itself (ported from `collection/backup.rs`; `backup::tests::retention_matches_anki` compares it with Anki's own thinning over two years of history). Same-second backups take the next free second instead of replacing each other. | R5 §7.2 | Phase 2 verified |
| **Replacing the collection (2026-11-14).** Restore and full download unpack into `<profile>/.tanki-stage-<ns>/`; after a forced backup and close, `replacement::install` takes Linux OFD write locks over SQLite's lock bytes on both files, re-checks `/proc`, verifies inode identity and the absence of a WAL, removes stale `-shm` index files, moves restored media in, and renames. A clean close leaves `collection.anki2-shm` behind (only an index SQLite rebuilds), which the first version wrongly refused; the CLI smoke test found it. Other platforms refuse replacement. If another process opened the file meanwhile, tanki does not reopen beside it: the actor stops with `InUse`. | 4.4 | Phase 2 verified |
| **Phase 5 integration (2026-11-15).** Check Database and Check Media back up inside the command, which drops SQLite's cross-process lock like every backup (4.4 correction); the actor now closes and reopens after either, and resets a stale cancel flag first. Test: `lock_across_processes::actor_holds_the_lock_after_check_database_and_media`. Anki's text importer counts rows matching an existing note by first field under `first_field_match`, not `duplicate`; tanki's summaries list that category. "Get Shared" opens `https://ankiweb.net/shared/decks/` (desktop's `aqt.appShared + "decks/"`) in the web browser; that is a web page, not a sync endpoint (rule 7). | 8.2–8.13 | Phase 5 verified |
| Kitty graphics in VS Code's terminal; placeholders in Konsole; bidi per terminal; text sizing outside kitty | R8 | Phase 4, best effort |
| Exact browser output for indent/justify markup; prevalence of JS in shared decks | R3, R7 | Phase 3–4 corpus |
| **AnkiConnect subset (2026-11-15): 35 actions (`createModel`, `modelTemplates`, `updateModelTemplates` added 2026-11-16 for the code-tour plugin).** Fixture HTTP replays cover Yomitan v2 mining/discovery and Obsidian-to-Anki v6 nested batches, updates, media, deck moves and tags. Old-version errors remain envelopes; nested requests use their own versions. These are source-derived protocol replays, not interactive extension sessions. See `crates/tanki-connect/tests/clients.rs` and `docs/implementation/phase-6-interfaces.md`. | R9 | Phase 6 verified |
| **Neovim tour integration (2026-11-15; now the `tanki-code-tour` plugin).** Synthetic actor tests verify note references, reordered fields, undo, and package round trips. Review tests cover timing, launch failure, stale replies, external changes, and rollover. The real binary-to-Neovim RPC path is fixture-tested. Lazy Claude lifecycle and Herdr launch use a simulated API; no real Claude session or startup approval was exercised. | User-requested extension | Saved-tour release implemented; interactive focus/startup remains manual acceptance |
| **Release size/startup (2026-11-15):** Linux x86_64 default features, optimized/stripped binary: 63,751,880 bytes (60.8 MiB), dynamically linked to glibc/ALSA. First `--version`: 8.16 ms; median of 10 subsequent new-process launches: 2.73 ms. Fresh synthetic profile creation/list: 52.19 ms; existing synthetic profile open/list median: 11.18 ms (10 launches). Warm filesystem cache, no cache eviction; not a disk-cold benchmark. Host glibc 2.39; release CI targets 2.35 on Ubuntu 22.04 for x86_64/aarch64. Hosted releases have not been run or published. | R8 | Phase 6 measured |

---

## 14. Rules for the implementing agent

Hard constraints. If a task seems to require breaking one, stop and ask the user.

User-approved Phase 0 exceptions (2026-11-12): Spike C may update only `col.ver` in a generated, closed temporary fixture to test future-schema rejection. No other raw SQL writes are allowed. The user also approved Spike A full download on isolated temporary fixtures only, after a completed backup and without competing processes: upstream releases its SQLite lock before replacement. Production full download remains blocked pending a safe locking design. GPT-6.1 Sol at high effort (complex spikes) or GPT-6.1 Terra at medium effort (mechanical spikes) substitutes for unavailable Opus/Sonnet agents.

**Data safety**

1. Never open, copy, read or modify anything under `~/.local/share/Anki2` (or the user's `ANKI_BASE`). All development and tests use generated fixture collections in temp folders via `--base`. Only the user runs tanki on the real profile, and only after Phase 2's exit test passes.
2. Until then, keep the `TANKI_ALLOW_REAL_PROFILE` guard in place. *(Removed at the Phase 2 exit, 2026-11-14. Rule 1 still binds the implementing agent: tests and agent runs always pass a temp `--base`; CLI tests run with a fake `HOME`.)*
3. Never read or write `prefs21.db`; never touch `addons21/`.
4. No raw SQLite access to a collection, except the read-only schema-version peek in 4.4.
5. Back up before: the first open of an unseen collection, full download, collection restore, Check Database, Check Media.
6. Every process exit path waits for Anki's backup thread.

**Network**

7. Never contact `ankiweb.net` sync endpoints, in code paths or in tests. Sync tests use the in-process server.
8. Do not download and execute external binaries (for example `protoc`) without the user's approval; ask instead.

**Code boundaries**

9. Only `tanki-core` imports `anki`, `anki_proto` or `anki_i18n`. Other crates use tanki-core's types.
10. `../anki` is read-only reference. `vendor/anki` changes only through files in `patches/anki/`.
11. Call Anki's service traits fully qualified (4.3). Never expose the raw-SQL sort mode or call find-and-replace with an empty id list (8.7).
12. Always use the media filename Anki returns (8.5).
13. The collection is touched from the actor thread only.

**Process**

14. Every feature lands with tests: unit tests for logic, cell-buffer snapshots for rendering, fixture-based integration tests for core commands.
15. Tick tasks in section 11 as they are finished, and replace entries in section 13 with results. If the pinned Anki source contradicts this plan or a research report, the source wins; note the correction here.
16. The agent cannot see a terminal's graphics. Anything that needs eyes (`tanki doctor`, image placement, colour) is handed to the user as a short checklist with the exact command to run.
17. Keep the licence AGPL-3.0-or-later and do not copy code from GPL-2.0-only projects.

---

## Appendix A — verified spike

Run on 2026-11-12 against `../anki` at `4aa9c0836`, Rust 1.97.1, with `PROTOC` pointing at a protoc wrapper and a copy of Anki's `Cargo.lock` beside the manifest. It built and ran; output is below the code. Paths are shown relative to a `vendor/anki` layout.

```toml
[package]
name = "tanki-spike"
version = "0.0.0"
edition = "2021"

[dependencies]
anki = { path = "vendor/anki/rslib", features = ["rustls"] }
anki_proto = { path = "vendor/anki/rslib/proto" }
prost = "0.13"

[profile.release]
strip = true
```

Part 1 — native methods (adds a note, studies it):

```rust
use anki::collection::CollectionBuilder;
use anki::prelude::*;
use anki::scheduler::answering::{CardAnswer, Rating};

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args().nth(1).expect("dir");
    std::fs::create_dir_all(&dir)?;
    // NOTE: the spike used with_desktop_media_paths(); real code must call
    // set_media_paths(<dir>/collection.media, <dir>/collection.media.db2) instead (4.4).
    let mut col = CollectionBuilder::new(format!("{dir}/collection.anki2"))
        .with_desktop_media_paths()
        .build()?;
    let nt = col.get_notetype_by_name("Basic")?.expect("basic");
    let mut note = nt.new_note();
    note.set_field(0, "What is <b>2+2</b>? <img src=\"x.png\">")?;
    note.set_field(1, "4 [sound:a.mp3] \\(x^2\\)")?;
    col.add_note(&mut note, DeckId(1))?;
    let tree = col.deck_tree(Some(TimestampSecs::now()))?;
    println!("deck tree: {} children, new={}", tree.children.len(), tree.children[0].new_count);
    let queued = col.get_queued_cards(1, false)?;
    println!("counts new={} lrn={} rev={}", queued.new_count, queued.learning_count, queued.review_count);
    let qc = &queued.cards[0];
    let out = col.render_existing_card(qc.card.id(), false, false)?;
    println!("Q: {}\nA: {}", out.question(), out.answer());
    println!("buttons: {:?}", col.describe_next_states(&qc.states)?);
    let mut ans = CardAnswer {
        card_id: qc.card.id(),
        current_state: qc.states.current,
        new_state: qc.states.good,
        rating: Rating::Good,
        answered_at: TimestampMillis::now(),
        milliseconds_taken: 3000,
        custom_data: None,
        from_queue: true,
    };
    col.answer_card(&mut ans)?;
    let queued = col.get_queued_cards(1, false)?;
    println!("after answer: new={} lrn={} rev={}", queued.new_count, queued.learning_count, queued.review_count);
    col.close(None)?;
    Ok(())
}
```

Output:

```
deck tree: 1 children, new=1
counts new=1 lrn=0 rev=0
Q: What is <b>2+2</b>? <img src="x.png">
A: What is <b>2+2</b>? <img src="x.png">

<hr id=answer>

4 [sound:a.mp3] \(x^2\)
buttons: ["<1m", "<6m", "<10m", "5d"]
after answer: new=0 lrn=1 rev=0
```

Part 2 — service traits on a `Collection`, then `Backend` (run on the collection from part 1). This is the calling style tanki-core uses:

```rust
use anki::backend::{init_backend, Backend};
use anki::collection::CollectionBuilder;
use anki::services::{BackendCollectionService, CardRenderingService, SchedulerService, SearchService, StatsService};
use anki_proto as pb;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args().nth(1).expect("dir");
    let mut col = CollectionBuilder::new(format!("{dir}/collection.anki2")).build()?;
    let q = SchedulerService::get_queued_cards(
        &mut col,
        pb::scheduler::GetQueuedCardsRequest { fetch_limit: 1, intraday_learning_only: false },
    )?;
    println!("svc queued: new={} lrn={} rev={} cards={}", q.new_count, q.learning_count, q.review_count, q.cards.len());
    let ids = SearchService::search_cards(
        &mut col,
        pb::search::SearchRequest { search: "deck:*".into(), order: None },
    )?;
    if let Some(cid) = ids.ids.first() {
        let r = CardRenderingService::render_existing_card(
            &mut col,
            pb::card_rendering::RenderExistingCardRequest { card_id: *cid, browser: false, partial_render: false },
        )?;
        println!("svc render: {} qnodes, css {}b", r.question_nodes.len(), r.css.len());
        let st = StatsService::card_stats(&mut col, pb::cards::CardId { cid: *cid })?;
        println!("svc card_stats: reviews={} revlog rows={}", st.reviews, st.revlog.len());
    }
    let g = StatsService::graphs(&mut col, pb::stats::GraphsRequest { search: "".into(), days: 365 })?;
    println!("svc graphs: has today={}", g.today.is_some());
    col.close(None)?;

    let init = pb::backend::BackendInit { preferred_langs: vec!["en".into()], locale_folder_path: "".into(), server: false };
    let backend: Backend = init_backend(&prost::Message::encode_to_vec(&init))?;
    backend.open_collection(pb::collection::OpenCollectionRequest {
        collection_path: format!("{dir}/collection.anki2"),
        media_folder_path: format!("{dir}/collection.media"),
        media_db_path: format!("{dir}/collection.media.db2"),
    })?;
    println!("backend opened; latest_progress ok={}", backend.latest_progress().is_ok());
    backend.close_collection(pb::collection::CloseCollectionRequest { downgrade_to_schema11: false })?;
    Ok(())
}
```

Output:

```
svc queued: new=0 lrn=1 rev=0 cards=1
svc search: 1 cards
svc render: 1 qnodes, css 148b
svc card_stats: reviews=1 revlog rows=1
svc graphs: has today=true
backend opened; latest_progress ok=true
```

Not covered by the spike: sync, media sync, import/export, backups, patch 0001, anything terminal-side.

