# 01 — rslib as an embeddable library: architecture and API surface

Research for **tanki**, a terminal Anki client. It asks how a third-party Rust
binary can link Anki's Rust core (`rslib`, crate name `anki`) and what API it
gets.

- Source examined: `/home/emard/projects/open-source/anki`. It is on **`main` at
  commit `4aa9c0836` (2026-08-30)**, not exactly the `26.08.1` tag. `26.08.1` is
  on a release branch and is not an ancestor of HEAD; HEAD has 102 commits
  not in the tag and the tag has 19 cherry-picks not in HEAD. Between the tag
  and HEAD, the proto change is +1 line (`frontend.proto`) and `rslib/src`
  changed in 28 files. `.version` says `26.08.1`.
- All paths below are relative to the anki repo root unless they are absolute.
  Line numbers refer to that commit.
- **Verified by the coordinator, so not re-derived here:** a standalone
  crate outside Anki's workspace compiles and runs. Its setup is
  `anki = { path = ".../rslib", features = ["rustls"] }`, a copy of Anki's
  `Cargo.lock`, and `PROTOC` set. It has called: open collection,
  `get_notetype_by_name`, `new_note`, `add_note`, `deck_tree`,
  `get_queued_cards`, `render_existing_card`, `describe_next_states`,
  `answer_card`, and `close`.

---

## 1. Layering

```
 ts/ (Svelte/TS web pages) ──protobuf-es over HTTP (mediasrv)──┐
 qt/aqt (PyQt GUI) ── Python ──> pylib/anki (Python library)   │
                                     │ _backend.py / _backend_generated.py
                                     ▼  (service_idx, method_idx, proto bytes)
                               pylib/rsbridge (pyo3 cdylib, "_rsbridge")
                                     │ Backend::run_service_method()
                                     ▼
                         rslib  (crate `anki`)  ── Backend ─ Mutex<Option<Collection>>
                                                   └─ Collection ─ SqliteStorage (rusqlite, bundled SQLite)
```

- **rslib** (`rslib/`, crate `anki`) holds almost all the logic: storage,
  scheduler (v3 + FSRS), templates and rendering, search, import/export,
  sync client and server, media, undo, i18n, and stats.
- **pylib** (`pylib/anki/`) is a thin Python wrapper. Each RPC goes through
  `RustBackend._run_command(service, method, bytes)`
  (`pylib/anki/_backend.py:159`). That calls the pyo3 class in
  `pylib/rsbridge/lib.rs`. `Backend.command()` (lines 49–63) releases the GIL
  and calls `anki::backend::Backend::run_service_method(service, method, input)`.
  `db_command()` (lines 67–81) calls `run_db_command_bytes`, a JSON-encoded
  raw-SQL proxy used by legacy Python code (`col.db.execute(...)`).
- **qt/aqt** is the Qt GUI. It embeds web views, which are the **ts/** Svelte
  pages: deck options, editor, stats graphs, import dialogs, card info, and
  more. The pages call the same protobuf RPCs over HTTP through aqt's
  mediasrv. Some pages also call back into Qt through `FrontendService`.
- `docs/architecture.md:51`: *"At the moment, the protobuf is not considered
  public API."*

### 1.1 Workspace crates (root `Cargo.toml:9-23`)

| crate (path) | role |
|---|---|
| `anki` (`rslib`) | core library |
| `anki_proto` (`rslib/proto`) | prost-generated message types for every `proto/anki/*.proto`. Its build script also writes the Python and TS RPC stubs |
| `anki_proto_gen` (`rslib/proto_gen`) | "Helpers for interface code generation": `get_services()` splits and indexes services, and `descriptors_path()` |
| `anki_i18n` (`rslib/i18n`) | Fluent translations. `build.rs` compiles every `.ftl` file into the binary (phf maps) and generates one typed method per string |
| `anki_io` (`rslib/io`) | fs wrappers with better error context |
| `anki_process` (`rslib/process`) | process-spawn wrappers with better errors (build tooling) |
| `anki-sync-server` (`rslib/sync`) | 30-line binary that runs `anki::sync::http_server::SimpleServer` |
| `linkchecker`, `ftl`, `build/*`, `tools/minilints`, `rsbridge` | tooling and the Python bridge |

### 1.2 Code generation chain

1. **`rslib/proto/build.rs`** calls `rust::write_rust_protos`
   (`rslib/proto/rust.rs:17`).
   - It runs `prost_build` over every `../../proto/anki/*.proto` file and needs
     **protoc** (see §6).
   - It writes `OUT_DIR/anki.<pkg>.rs`, which `rslib/proto/src/lib.rs`
     includes as `anki_proto::<pkg>`.
   - It writes the descriptor set to `descriptors_path()`. That is either
     `$DESCRIPTORS_BIN` or `OUT_DIR/../../anki_descriptors.bin`, which is
     `target/<profile>/build/anki_descriptors.bin` and is shared with rslib's
     build script (`rslib/proto_gen/src/lib.rs:278-284`).
   - It also writes the Python (`python.rs`) and TypeScript (`typescript.rs`)
     RPC stubs.
2. **`rslib/build.rs`** reads `../out/buildhash` (optional, becomes the
   `BUILDHASH` env var) and decodes the descriptor pool. It then calls
   `rust_interface::write_rust_interface` (`rslib/rust_interface.rs:19`), which
   writes `OUT_DIR/backend.rs`. `rslib/src/services.rs` includes that file
   with `include!`.
   - The generated file contains `pub trait XService` (methods take
     `&mut self`) for every **collection service**. It skips
     `FrontendService`.
   - It contains `pub trait BackendXService` (methods take `&self`) for every
     **backend service**.
   - It contains one **private** forwarding method on `Backend` for each
     collection method that has no Backend override:
     `fn x(&self, input) -> Result<..> { self.with_col(|col| XService::x(col, input)) }`
     (`rust_interface.rs:117-126`). These have **no `pub`**.
   - It contains `Collection::run_service_method(&mut self, ...)` and
     `Backend::run_service_method(&self, service: u32, method: u32, input: &[u8]) -> Result<Vec<u8>, Vec<u8>>`
     (`rust_interface.rs:129-163`). An error comes back as an encoded
     `anki.backend.BackendError`.
3. **Service split rule** (`rslib/proto_gen/src/lib.rs:28-44`):
   - Every `.proto` file defines `FooService` and `BackendFooService`.
   - A method only in `BackendFooService` exists only on `Backend`.
   - A method only in `FooService` is auto-forwarded on `Backend`.
   - A method in both has separate implementations.
   - **Indices:** the service index is `service.index()` in the descriptor
     pool. The method index on a Backend service is: Backend-trait methods
     first, then the delegated collection methods, offset by the number of
     Backend methods (`lib.rs:73-79`). These numbers are not stable. In the
     current generated Python (`out/pylib/anki/_backend_generated.py`, a stale
     local build) `open_collection` is `(3,0)`, `deck_tree` is `(7,4)`,
     `get_queued_cards` is `(13,3)` and `answer_card` is `(13,4)`. Any proto
     edit can renumber them.
4. **`rslib/i18n/build.rs`** reads `../../ftl/core/*.ftl` and
   `../../ftl/qt/*.ftl` (English templates), plus the git submodules
   `ftl/core-repo` and `ftl/qt-repo` (translations). `EXTRA_FTL_ROOT` swaps
   the qt set for a mobile client's own set (`rslib/i18n/gather.rs:4-6,92`).
   The build emits `strings.rs`, with one method per message, for example
   `pub fn studying_again<'a>(&'a self) -> Cow<'a, str>`
   (`write_strings.rs:71`). Optional env vars `STRINGS_JSON`, `STRINGS_PY` and
   `STRINGS_TS` write extra outputs.
5. `rslib/src/version.rs:8`: `include_str!("../../.version")`. The crate must
   be built from a full repo checkout.

---

## 2. Two ways to program against rslib

### (a) Native Rust API: `Collection` plus inherent methods plus `anki::services::*` traits

```rust
use anki::collection::{Collection, CollectionBuilder};
let mut col: Collection = CollectionBuilder::new(path).set_media_paths(..).set_tr(tr).build()?;
let tree = col.deck_tree(Some(TimestampSecs::now()))?;          // inherent, pub
use anki::services::CollectionService;
let problems = CollectionService::check_database(&mut col)?;     // trait method (proto in/out)
```

### (b) Byte-level protobuf: `anki::backend::Backend`

```rust
let backend = anki::backend::Backend::new(I18n::new(&["en"]), /*server*/ false);
// or anki::backend::init_backend(&BackendInit{..}.encode_to_vec())
use anki::services::BackendCollectionService;
backend.open_collection(OpenCollectionRequest { collection_path, media_folder_path, media_db_path })?; // typed: Backend trait
let out: Vec<u8> = backend.run_service_method(SVC, METHOD, &req.encode_to_vec())
    .map_err(|e| BackendError::decode(&*e).unwrap())?;          // everything else: bytes + indices
```

### 2.1 Visibility facts that decide the choice

| Item | Visibility | Consequence |
|---|---|---|
| `pub trait XService` / `BackendXService` in `pub mod services` (`lib.rs:40`) | public | **Every collection-service RPC is callable natively on `&mut Collection`**, and every backend-service RPC on `&Backend`, from an external crate. Exception: `FrontendService` (Qt callbacks) is not generated in Rust. |
| Backend forwarding methods for collection RPCs | private `fn` (`rust_interface.rs:120`) | A **`Backend` user cannot call collection RPCs typed**. The only way is `run_service_method` with magic indices. |
| `Backend::with_col` | `pub(crate)` (`backend/mod.rs:117`) | You **cannot get the `Collection` out of a `Backend`**. With `Backend` you are all-bytes for collection work. |
| `Backend::new`, `init_backend`, `run_service_method`, `run_db_command_bytes`, `i18n()` | `pub` | — |
| `Collection.storage` | `pub` field, but `SqliteStorage` is `pub(crate) use` from a private module and `.db` is `pub(crate)` (`storage/sqlite.rs:46-49`) | The type is unnameable, but its `pub` methods (for example `col.storage.get_card(cid)`, `get_note`, `all_tags`) are still callable through inference. **There is no raw SQL from outside** except `Backend::run_db_command_bytes`. |
| `Collection::transact` / `transact_no_undo` | `pub(crate)` (`collection/transact.rs:59,68`) | **You cannot make your own undoable or atomic multi-step ops.** Use the provided ops. |
| Modules `scheduler::queue`, `progress`, `stats`, `card_rendering::service`, `notes::service`, `import_export::package::{apkg,colpkg}` | private or `pub(crate)` | Inherent `pub fn`s defined there are still callable on `Collection`, and their return types (`QueuedCards`, `ThrottlingProgressHandler`, `ProgressState`) are usable but **unnameable**. You cannot write them in type signatures or match enum variants (for example `progress::Progress`). |
| `anki_proto` | **not re-exported** by `anki` | The frontend crate must also depend on `anki_proto` from the same source (path/git). |

### 2.2 Functionality that is NOT reachable natively (or only half-reachable) but IS reachable through service traits

These are concrete cases (method visibility from §2.1 checked in source):

| Need | Native method | Reach it via |
|---|---|---|
| Check Database | `Collection::check_database` is `pub(crate)` (`dbcheck.rs:110`) | `CollectionService::check_database` (proto `CheckDatabaseResponse`, localized strings) |
| Per-deck "today" counts | `counts_for_deck_today` is `pub(crate)` (`decks/counts.rs:41`) | `SchedulerService::counts_for_deck_today` |
| Check Media | no inherent method. The service body wraps `media_checker()` in `transact_no_undo` (`media/service.rs:20`) | `MediaService::check_media` |
| Stats graphs | `graph_data_for_search` is `pub(crate)` (`stats/graphs/mod.rs:31`) | `StatsService::graphs`, `get_graph_preferences` |
| Extend new/review limits today | `extend_limits` is `pub(crate)` (`decks/stats.rs:48`) | `SchedulerService::extend_limits` |
| Legacy "update stats" | `update_deck_stats` is `pub(crate)` | `SchedulerService::update_stats` |
| Sched timing today (proto) | `sched_timing_today` is `pub(crate)` (`scheduler/timing.rs:152`); `timing_today()` is `pub` | `SchedulerService::sched_timing_today` |
| Default deck for notetype | `pub(crate)` (`adding.rs:105`) | `NotesService::default_deck_for_notetype` |
| Current deck id | `get_current_deck_id` is `pub(crate)`; `get_current_deck()` is `pub` | either |
| Learn-ahead secs | `pub(crate)` (`config/mod.rs:233`) | `get_config_json("collapseTime")` or the `Preferences` proto |
| Empty-cards report | `empty_cards()` is pub, but the report builder is service-only | `CardRenderingService::get_empty_cards` |
| Browser column state | no inherent method | `SearchService::set_active_browser_columns`, `all_browser_columns`, `browser_row_for_id` |
| `cards_of_note`, `studied_today_message`, `get_config_json`, `remove_decks` (inherent name is `remove_decks_and_child_decks`) | service-only names | service traits |
| **Progress for long ops** | `CollectionService::latest_progress` and `set_wants_abort` need `&mut Collection`, which the long op itself holds. For cross-thread use, pass an `Arc<Mutex<ProgressState>>` to `CollectionBuilder::set_shared_progress_state` (`collection/mod.rs:120`). `ProgressState` is unnameable but `Default` and has `pub want_abort` / `pub last_progress`. `last_progress: Option<progress::Progress>` **cannot be pattern-matched** outside the crate (private module, only `Debug`), and `progress_to_proto` is `pub(crate)` (`progress.rs:148`). | **Abort is reachable natively** (set `want_abort = true`). **Structured progress display is not**, short of Debug-string scraping. Backend has `latest_progress()` through `BackendCollectionService` (`backend/collection.rs:85`). |
| Media-sync `server_usn` hand-off | `SyncOutput.server_media_usn` is `pub(crate)` (`sync/collection/normal.rs:153`) | Pass `None` to `MediaManager::sync_media` (it re-queries) or use `BackendSyncService::sync_collection{sync_media:true}` |
| Raw SQL | `storage.db` is `pub(crate)` | `Backend::run_db_command_bytes` (JSON) only |

Things that are **fine natively** (all `pub`): `CollectionBuilder`,
`Collection::{close, deck_tree, current_deck_tree, get_queued_cards,
get_next_card, get_scheduling_states, describe_next_states, answer_card,
render_existing_card, render_uncommitted_card, undo, redo, undo_status,
add_note, update_note, add_notes, remove_notes, get_notetype_by_name,
get_notetype, get_all_notetypes, get_deck, get_deck_id, set_current_deck,
get_current_deck, search_cards, search_notes, find_and_replace, set_deck,
set_card_flag, bury_or_suspend_cards, unbury_deck, set_due_date, sort_cards,
reschedule_cards_as_new, custom_study, rebuild_filtered_deck, empty_filtered_deck,
get_preferences, set_preferences, get/set_config_bool/string, set_config_json,
tag_tree, rename_tag, remove_tags, add_tags_to_notes, card_stats,
get_review_logs, studied_today, congrats_info, import_apkg, export_apkg,
export_colpkg, import_csv, get_csv_metadata, compute_params, evaluate_params,
compute_memory_state, maybe_backup, media(), new_progress_handler,
normal_sync (async), full_upload / full_download (async, consume self),
sync_status_offline, sync_meta}`. Free functions:
`anki::sync::login::sync_login`,
`anki::sync::collection::status::online_sync_status_check`,
`anki::sync::collection::progress::sync_abort`,
`anki::import_export::package::import_colpkg`,
`anki::card_rendering::{extract_av_tags, strip_av_tags}`, and
`anki::text::*`.

**Name-collision gotcha.** Many trait methods share a name with an inherent
method, for example `answer_card`, `deck_tree`, `get_queued_cards` and
`check_database`. Method-call syntax resolves to the **inherent** one. To call
the proto/trait version, use UFCS:
`SchedulerService::answer_card(&mut col, proto)`.

### 2.3 Recommendation for an in-process Rust frontend

**Own a `Collection` directly (option a).** Use inherent methods where they
are `pub`, and fall back to `anki::services::*` traits through UFCS for the
gaps in §2.2. Reimplement the thin Backend-only orchestration yourself. It is
small and built entirely from public pieces; see `backend/sync.rs`,
`backend/collection.rs` and `backend/import_export.rs`:

- open/close (`CollectionBuilder`, `Collection::close`)
- backup (`maybe_backup` returns a `JoinHandle`)
- sync (`sync_login`, `normal_sync`, `full_upload`/`full_download`, then
  rebuild with `as_builder().build()`)
- media sync on a background thread (`col.media()?.sync_media(col.new_progress_handler(), auth, client, None)`)
- abort: `futures::future::Abortable` for network calls, `want_abort` for
  CPU work
- your own tokio runtime

Why not `Backend`:

- every collection RPC becomes untyped bytes with renumber-prone indices;
- you lose access to `Collection` entirely;
- Backend's only real advantages are the canned background media sync,
  `latest_progress` in proto form, and `run_db_command_bytes`.

If structured progress matters, two options:

- (i) show a spinner plus Debug-parsed text;
- (ii) carry a small patch (`pub use progress::Progress` / `progress_to_proto`)
  in a fork;
- or go hybrid: use `Backend` only for long-running ops. That is impossible
  at the same time as a native `Collection` because of the SQLite exclusive
  lock (§4.3).

You can generate typed wrappers for the Backend byte API in your own
`build.rs` with `anki_proto_gen::get_services` ("helpers for code generation
in external crates, that ensure indexes match", `proto_gen/src/lib.rs:4-5`).
That works, but it is extra machinery.

---

## 3. Master API inventory (all services in `proto/anki/*.proto`)

Format: `Method(Input) -> Output — meaning`. "Col" means it is implemented on
`Collection` (trait `XService`). "Backend" means it is implemented on
`Backend` only (trait `BackendXService`). Collection methods are also
auto-forwarded on Backend. An empty Backend service is listed for
completeness. The line number is where the service starts.

### collection.proto
**CollectionService (Col), L13**
- `CheckDatabase(Empty) -> CheckDatabaseResponse` — run DB check/repair; returns localized problem list
- `GetUndoStatus(Empty) -> UndoStatus` — labels of next undo/redo + last step counter
- `Undo(Empty) -> OpChangesAfterUndo` — undo last op
- `Redo(Empty) -> OpChangesAfterUndo` — redo
- `AddCustomUndoEntry(String) -> UInt32` — push named custom undo step, returns counter
- `MergeUndoEntries(UInt32) -> OpChanges` — merge steps since counter into one
- `LatestProgress(Empty) -> Progress` — last progress snapshot
- `SetWantsAbort(Empty) -> Empty` — request abort of current long op
- `SetLoadBalancerEnabled(Bool) -> OpChanges` — toggle FSRS load balancer (undoable)
- `GetCustomColours(Empty) -> GetCustomColoursResponse` — saved colour-picker palette

**BackendCollectionService (Backend), L28**
- `OpenCollection(OpenCollectionRequest{collection_path, media_folder_path, media_db_path}) -> Empty`
- `CloseCollection(CloseCollectionRequest{downgrade_to_schema11}) -> Empty` — aborts media sync, closes, optional downgrade
- `CreateBackup(CreateBackupRequest{backup_folder, force, wait_for_completion}) -> Bool` — no-media colpkg backup; false if too soon or unchanged
- `AwaitBackupCompletion(Empty) -> Empty` — join pending backup thread
- `LatestProgress`, `SetWantsAbort` — backend-level versions (work while col mutex is held)

### cards.proto
**CardsService (Col), L12**
- `GetCard(CardId) -> Card`
- `UpdateCards(UpdateCardsRequest) -> OpChanges` — write modified card(s)
- `RemoveCards(RemoveCardsRequest) -> OpChangesWithCount`
- `SetDeck(SetDeckRequest) -> OpChangesWithCount` — move cards to deck
- `SetFlag(SetFlagRequest) -> OpChangesWithCount` — set flag 0–7

**BackendCardsService** — empty

### card_rendering.proto
**CardRenderingService (Col), L14**
- `ExtractAvTags(ExtractAvTagsRequest{text, question_side}) -> ExtractAvTagsResponse` — replace `[sound:]`/`[anki:tts]` with `[anki:play:q:N]`, return tag list
- `ExtractLatex(..) -> ExtractLatexResponse` — find LaTeX blocks → image filenames
- `GetEmptyCards(Empty) -> EmptyCardsReport`
- `RenderExistingCard(RenderExistingCardRequest{card_id, browser, partial_render}) -> RenderCardResponse` — render q/a nodes + css
- `RenderUncommittedCard(..) -> RenderCardResponse` — render note/template not yet saved (editor preview)
- `RenderUncommittedCardLegacy(..) -> RenderCardResponse` — same, schema11 JSON template
- `StripAvTags(String) -> String`
- `RenderMarkdown(RenderMarkdownRequest) -> String` — markdown→HTML
- `EncodeIriPaths(String) -> String` / `DecodeIriPaths(String) -> String` — media filename URL-escaping in HTML
- `StripHtml(StripHtmlRequest) -> String` — HTML→text (modes incl. preserve media filenames)
- `HtmlToTextLine(HtmlToTextLineRequest) -> String` — HTML → single text line (browser cells)
- `CompareAnswer(CompareAnswerRequest) -> String` — type-in-answer diff HTML
- `ExtractClozeForTyping(..) -> String` — cloze text expected for type-answer

**BackendCardRenderingService (Backend), L37**
- `StripHtml` (Backend copy, no collection needed)
- `AllTtsVoices(AllTtsVoicesRequest) -> AllTtsVoicesResponse` — OS TTS voices (Windows impl)
- `WriteTtsStream(WriteTtsStreamRequest) -> Empty` — synthesize TTS to file

### config.proto
**ConfigService (Col), L13** (impl lives in `backend/config.rs:58`)
- `GetConfigJson(String) -> Json`
- `SetConfigJson(SetConfigJsonRequest) -> OpChanges` (undoable)
- `SetConfigJsonNoUndo(..) -> Empty`
- `RemoveConfig(String) -> OpChanges`
- `GetAllConfig(Empty) -> Json`
- `GetConfigBool(GetConfigBoolRequest) -> Bool` / `SetConfigBool(..) -> OpChanges`
- `GetConfigString(..) -> String` / `SetConfigString(..) -> OpChanges`
- `GetPreferences(Empty) -> Preferences` / `SetPreferences(Preferences) -> OpChanges` — scheduling/reviewing/editing/backup prefs bundle

**BackendConfigService** — empty

### deck_config.proto
**DeckConfigService (Col), L16**
- `AddOrUpdateDeckConfigLegacy(Json) -> DeckConfigId`
- `GetDeckConfig(DeckConfigId) -> DeckConfig`
- `AllDeckConfigLegacy(Empty) -> Json`
- `GetDeckConfigLegacy(DeckConfigId) -> Json`
- `NewDeckConfigLegacy(Empty) -> Json`
- `RemoveDeckConfig(DeckConfigId) -> Empty`
- `GetDeckConfigsForUpdate(DeckId) -> DeckConfigsForUpdate` — everything the deck-options screen needs
- `UpdateDeckConfigs(UpdateDeckConfigsRequest) -> OpChanges` — save deck-options screen (presets, limits, FSRS)
- `GetIgnoredBeforeCount(..) -> ..` — cards excluded by "ignore reviews before" date
- `GetRetentionWorkload(..) -> ..` — workload estimate per desired retention

**BackendDeckConfigService** — empty

### decks.proto
**DecksService (Col), L13**
- `NewDeck(Empty) -> Deck` — default-filled deck, not saved
- `AddDeck(Deck) -> OpChangesWithId`
- `AddDeckLegacy(Json) -> OpChangesWithId`
- `AddOrUpdateDeckLegacy(..) -> DeckId`
- `DeckTree(DeckTreeRequest{now}) -> DeckTreeNode` — tree with due counts (now=0 → no counts)
- `DeckTreeLegacy(Empty) -> Json`
- `GetAllDecksLegacy(Empty) -> Json`
- `GetDeckIdByName(String) -> DeckId`
- `GetDeck(DeckId) -> Deck`
- `UpdateDeck(Deck) -> OpChanges` / `UpdateDeckLegacy(Json) -> OpChanges`
- `SetDeckCollapsed(..) -> OpChanges` — browser or study-screen collapse state
- `GetDeckLegacy(DeckId) -> Json`
- `GetDeckNames(GetDeckNamesRequest) -> DeckNames` — (skip_empty_default, include_filtered)
- `GetDeckAndChildNames(DeckId) -> DeckNames`
- `NewDeckLegacy(Bool) -> Json`
- `RemoveDecks(DeckIds) -> OpChangesWithCount` — delete decks + children + cards
- `ReparentDecks(..) -> OpChangesWithCount` — drag-and-drop move
- `RenameDeck(..) -> OpChanges`
- `GetOrCreateFilteredDeck(DeckId) -> FilteredDeckForUpdate` — 0 → new filtered deck template
- `AddOrUpdateFilteredDeck(FilteredDeckForUpdate) -> OpChangesWithId` — save + build
- `FilteredDeckOrderLabels(Empty) -> StringList`
- `SetCurrentDeck(DeckId) -> OpChanges`
- `GetCurrentDeck(Empty) -> Deck`

**BackendDecksService** — empty

### i18n.proto
**I18nService (Col), L12** and **BackendI18nService (Backend), L20** — the same three methods:
- `TranslateString(TranslateStringRequest{module_index, message_index, args}) -> String`
- `FormatTimespan(FormatTimespanRequest) -> String` — "3.2 months", etc.
- `I18nResources(I18nResourcesRequest{modules}) -> Json` — ftl bundle for web pages

### image_occlusion.proto
**ImageOcclusionService (Col), L13**
- `GetImageForOcclusion(..)` — load image bytes for IO editor
- `GetImageOcclusionNote(..)` — existing IO note's shapes/image
- `GetImageOcclusionFields(..)` — field indices of IO notetype
- `AddImageOcclusionNotetype(Empty) -> OpChanges` — create IO notetype if missing
- `AddImageOcclusionNote(..) -> OpChanges` / `UpdateImageOcclusionNote(..) -> OpChanges`

**BackendImageOcclusionService** — empty

### import_export.proto
**ImportExportService (Col), L15**
- `ImportAnkiPackage(ImportAnkiPackageRequest) -> ImportResponse` — .apkg import (undoable Op::Import)
- `GetImportAnkiPackagePresets(Empty) -> ImportAnkiPackageOptions`
- `ExportAnkiPackage(ExportAnkiPackageRequest) -> UInt32` — .apkg/.colpkg export (legacy option), returns note count
- `GetCsvMetadata(CsvMetadataRequest) -> CsvMetadata` — sniff delimiter/columns/notetype
- `ImportCsv(ImportCsvRequest) -> ImportResponse`
- `ExportNoteCsv(..) -> UInt32` / `ExportCardCsv(..) -> UInt32`
- `ImportJsonFile(String) -> ImportResponse` / `ImportJsonString(String) -> ImportResponse` — CSV import driven by JSON spec

**BackendImportExportService (Backend), L30**
- `ImportCollectionPackage(ImportCollectionPackageRequest) -> Empty` — replace whole collection from .colpkg (collection must be closed)
- `ExportCollectionPackage(ExportCollectionPackageRequest) -> Empty` — export whole collection (+media) as .colpkg (closes/reopens col)

### links.proto
**LinksService (Col), L12**
- `HelpPageLink(HelpPageLinkRequest) -> String` — manual URL for a help page enum

**BackendLinksService** — empty

### media.proto
**MediaService (Col), L13**
- `CheckMedia(Empty) -> CheckMediaResponse` — unused/missing files report
- `AddMediaFile(AddMediaFileRequest{desired_name, data}) -> String` — write into media folder, returns final (deduped) name
- `AddMediaFromPath(..) -> String`
- `TrashMediaFiles(..) -> Empty` — move to media trash
- `EmptyTrash(Empty)` / `RestoreTrash(Empty)`
- `ExtractStaticMediaFiles(NotetypeId) -> StringList` — `_files` referenced by templates/CSS
- `ExtractMediaFiles(String) -> StringList` — media refs in HTML
- `GetAbsoluteMediaPath(String) -> String`

**BackendMediaService (Backend), L28**
- `AddMediaFromUrl(AddMediaFromUrlRequest) -> AddMediaFromUrlResponse` — download URL into media

### notes.proto
**NotesService (Col), L15**
- `NewNote(NotetypeId) -> Note`
- `AddNote(AddNoteRequest{note, deck_id}) -> AddNoteResponse`
- `AddNotes(AddNotesRequest) -> AddNotesResponse` — bulk
- `DefaultsForAdding(..) -> DeckAndNotetype` — add-screen defaults
- `DefaultDeckForNotetype(NotetypeId) -> DeckId`
- `UpdateNotes(UpdateNotesRequest) -> OpChanges`
- `GetNote(NoteId) -> Note`
- `RemoveNotes(RemoveNotesRequest) -> OpChangesWithCount`
- `ClozeNumbersInNote(Note) -> ..`
- `AfterNoteUpdates(..) -> OpChangesWithCount` — regenerate cards / mark modified after bulk changes
- `FieldNamesForNotes(..) -> ..`
- `NoteFieldsCheck(Note) -> NoteFieldsCheckResponse` — empty/duplicate/cloze problems
- `CardsOfNote(NoteId) -> CardIds`
- `GetSingleNotetypeOfNotes(NoteIds) -> NotetypeId` — error if mixed

**BackendNotesService** — empty

### notetypes.proto
**NotetypesService (Col), L13**
- `AddNotetype(Notetype) -> OpChangesWithId`
- `UpdateNotetype(Notetype) -> OpChanges`
- `AddNotetypeLegacy(Json)`, `UpdateNotetypeLegacy(..)`, `AddOrUpdateNotetype(..) -> NotetypeId`
- `GetStockNotetypeLegacy(StockNotetype) -> Json`
- `GetNotetype(NotetypeId) -> Notetype` / `GetNotetypeLegacy(NotetypeId) -> Json`
- `GetNotetypeNames(Empty) -> NotetypeNames`
- `GetNotetypeNamesAndCounts(Empty) -> NotetypeUseCounts`
- `GetNotetypeIdByName(String) -> NotetypeId`
- `RemoveNotetype(NotetypeId) -> OpChanges`
- `GetAuxNotetypeConfigKey(..) -> String` / `GetAuxTemplateConfigKey(..) -> String` — per-notetype config key names
- `GetChangeNotetypeInfo(..) -> ChangeNotetypeInfo` / `ChangeNotetype(..) -> OpChanges`
- `GetFieldNames(NotetypeId) -> StringList`
- `RestoreNotetypeToStock(..) -> OpChanges`
- `GetClozeFieldOrds(NotetypeId) -> ..`

**BackendNotetypesService** — empty

### scheduler.proto
**SchedulerService (Col), L17**
- `GetQueuedCards(GetQueuedCardsRequest{fetch_limit, intraday_learning_only}) -> QueuedCards` — next cards with states + counts
- `AnswerCard(CardAnswer) -> OpChanges` — apply rating (undoable)
- `SchedTimingToday(Empty) -> SchedTimingTodayResponse` — days elapsed / next rollover
- `StudiedToday(Empty) -> String` — "Studied N cards in M minutes today"
- `StudiedTodayMessage(..) -> String`
- `UpdateStats(..) -> Empty` — legacy deck today counters
- `ExtendLimits(..) -> Empty` — raise today's new/review limit
- `CountsForDeckToday(DeckId) -> ..`
- `CongratsInfo(Empty) -> CongratsInfoResponse` — data for "Congratulations" screen
- `RestoreBuriedAndSuspendedCards(CardIds) -> OpChanges`
- `UnburyDeck(..) -> OpChanges`
- `BuryOrSuspendCards(..) -> OpChangesWithCount` — mode: suspend / bury-sched / bury-user
- `EmptyFilteredDeck(DeckId) -> OpChanges` / `RebuildFilteredDeck(DeckId) -> OpChangesWithCount`
- `ScheduleCardsAsNew(..) -> OpChanges` — Forget
- `ScheduleCardsAsNewDefaults(..) -> ..`
- `SetDueDate(..) -> OpChanges` — "0", "1-7", "3!" syntax
- `GradeNow(..) -> OpChanges` — answer arbitrary cards from browser
- `SortCards(..) -> OpChangesWithCount` — reposition new cards
- `SortDeck(..) -> OpChangesWithCount`
- `GetSchedulingStates(CardId) -> SchedulingStates`
- `DescribeNextStates(SchedulingStates) -> StringList` — button interval labels
- `StateIsLeech(SchedulingState) -> Bool`
- `UpgradeScheduler(Empty) -> Empty` — v1→v2 upgrade
- `CustomStudy(CustomStudyRequest) -> OpChanges` / `CustomStudyDefaults(..) -> ..`
- `RepositionDefaults(Empty) -> ..`
- `ComputeFsrsParams(..) -> ComputeFsrsParamsResponse` — FSRS optimize (long, progress, abortable)
- `GetOptimalRetentionParameters(..)` / `ComputeOptimalRetention(SimulateFsrsReviewRequest)` — "help me decide" / CMRR
- `SimulateFsrsReview(..)` / `SimulateFsrsWorkload(..)` — simulator
- `EvaluateParams(..)` / `EvaluateParamsLegacy(..) -> EvaluateParamsResponse` — log loss / RMSE
- `ComputeMemoryState(CardId) -> ..` — FSRS S/D/R for card
- `FuzzDelta(..) -> ..` — days fuzz applied last review

**BackendSchedulerService (Backend), L71**
- `ComputeFsrsParamsFromItems(..)` — optimize on supplied items (no collection)
- `FsrsBenchmark(..)` — research benchmark
- `ExportDataset(..) -> Empty` — export revlogs for research

### search.proto
**SearchService (Col), L13**
- `BuildSearchString(SearchNode) -> String` — structured node → search text
- `SearchCards(SearchRequest{search, order}) -> SearchResponse` / `SearchNotes(..)` — ids
- `JoinSearchNodes(..) -> String` / `ReplaceSearchNode(..) -> String` — sidebar search composition
- `FindAndReplace(..) -> OpChangesWithCount`
- `AllBrowserColumns(Empty) -> BrowserColumns`
- `BrowserRowForId(Int64) -> BrowserRow` — rendered cells for one row (uses active columns)
- `SetActiveBrowserColumns(StringList) -> Empty`

**BackendSearchService** — empty

### stats.proto
**StatsService (Col), L13**
- `CardStats(CardId) -> CardStatsResponse` — card info screen
- `GetReviewLogs(CardId) -> ReviewLogs`
- `Graphs(GraphsRequest{search, days}) -> GraphsResponse` — raw data for all stats graphs
- `GetGraphPreferences(Empty)` / `SetGraphPreferences(..)`

**BackendStatsService** — empty

### sync.proto
**SyncService (Col)** — empty

**BackendSyncService (Backend), L15**
- `SyncMedia(SyncAuth) -> Empty` — start background media sync
- `AbortMediaSync(Empty)`
- `MediaSyncStatus(Empty) -> MediaSyncStatusResponse` — active? + progress; surfaces error once
- `SyncLogin(SyncLoginRequest{username,password,endpoint}) -> SyncAuth` — get hkey
- `SyncStatus(SyncAuth) -> SyncStatusResponse` — offline + online check whether sync needed
- `SyncCollection(SyncCollectionRequest{auth, sync_media}) -> SyncCollectionResponse` — normal sync; may say full sync required
- `FullUploadOrDownload(FullUploadOrDownloadRequest{auth, upload, server_usn}) -> Empty` — closes, transfers, reopens
- `AbortSync(Empty)`
- `SetCustomCertificate(String) -> Bool` — custom root CA (rustls only)

### tags.proto
**TagsService (Col), L13**
- `ClearUnusedTags(Empty) -> OpChangesWithCount`
- `AllTags(Empty) -> StringList`
- `RemoveTags(String) -> OpChangesWithCount`
- `SetTagCollapsed(..) -> OpChanges`
- `TagTree(Empty) -> TagTreeNode`
- `ReparentTags(..) -> OpChangesWithCount` / `RenameTags(..) -> OpChangesWithCount`
- `AddNoteTags(NoteIdsAndTagsRequest)` / `RemoveNoteTags(..) -> OpChangesWithCount`
- `FindAndReplaceTag(..) -> OpChangesWithCount`
- `CompleteTag(CompleteTagRequest) -> CompleteTagResponse` — autocomplete

**BackendTagsService** — empty

### ankiweb.proto / github.proto / ankihub.proto (Backend only; Col services empty)
- `BackendAnkiwebService`: `GetAddonInfo(..)` (add-on metadata, ≤25), `CheckForUpdate(..)` (add-on updates)
- `BackendGithubService`: `GetLatestRelease(..)`, `DownloadRelease(GithubRelease) -> String` (desktop self-update)
- `BackendAnkiHubService`: `AnkihubLogin(..)`, `AnkihubLogout(..)`

### ankidroid.proto (AnkiDroid compatibility)
**AnkidroidService (Col), L10** — raw SQL/cursor paging for AnkiDroid's legacy Java DB layer:
`RunDbCommand`, `RunDbCommandProto`, `InsertForId`, `RunDbCommandForRowCount`,
`FlushAllQueries`, `FlushQuery`, `GetNextResultPage`, `GetColumnNamesFromQuery`,
`GetActiveSequenceNumbers`. The `ankidroid` module is `pub(crate)`, but the
trait impl is public through `anki::services::AnkidroidService`.

**BackendAnkidroidService (Backend), L25**: `SchedTimingTodayLegacy`,
`LocalMinutesWestLegacy`, `SetPageSize`, `DebugProduceError`.

### frontend.proto (NOT generated for Rust)
**FrontendService, L17** is implemented by Qt/Python and called from web
pages. It is filtered out in `rust_interface.rs:24-31`. Methods:
`GetSchedulingStatesWithContext`, `SetSchedulingStates` (custom-scheduler JS
hook), `ImportDone`, `SearchInBrowser`, `deckOptionsRequireClose`,
`deckOptionsReady`, `ConvertPastedImage`, `OpenFilePicker`, `OpenMedia`,
`ShowInMediaFolder`, `RecordAudio`, `PlayFile`, `CloseAddCards`,
`CloseEditCurrent`, `OpenLink`, `AskUser`, `ShowMessageBox`,
`OpenFieldsDialog`, `OpenCardsDialog`, `Get/SetProfileConfigJson`,
`Get/SetMetaJson`, `ReadClipboard`, `WriteClipboard`, `SaveCustomColours`.
A terminal client has to provide its own equivalents (for example custom
scheduling JS, clipboard, and audio playback).

### backend.proto (messages only)
`BackendInit{preferred_langs, locale_folder_path, server}` and
`BackendError{message, kind, help_page, context, backtrace}`. `kind` is an
enum of 25 values: INVALID_INPUT, UNDO_EMPTY, INTERRUPTED, TEMPLATE_PARSE,
IO_ERROR, DB_ERROR, NETWORK_ERROR, SYNC_AUTH_ERROR, SYNC_SERVER_MESSAGE,
SYNC_OTHER_ERROR, JSON_ERROR, PROTO_ERROR, NOT_FOUND_ERROR, EXISTS,
FILTERED_DECK_ERROR, SEARCH_ERROR, CUSTOM_STUDY_ERROR, IMPORT_ERROR, DELETED,
CARD_TYPE_ERROR, ANKIDROID_PANIC_ERROR, OS_ERROR, SCHEDULER_UPGRADE_REQUIRED,
INVALID_CERTIFICATE_FORMAT and INVALID_CHECKSUM.

Count, from a script over `proto/anki/*.proto`:
- **18 collection services with 205 RPCs.** Per service: Ankidroid 9,
  CardRendering 14, Cards 5, Collection 10, Config 11, DeckConfig 10,
  Decks 24, I18n 3, ImageOcclusion 6, ImportExport 9, Links 1, Media 9,
  Notes 14, Notetypes 19, Scheduler 36, Search 9, Stats 5, Tags 11.
- **11 backend services with 37 RPCs.** Per service: Ankidroid 4, AnkiHub 2,
  Ankiweb 2, CardRendering 3, Collection 6, Github 2, I18n 3,
  ImportExport 2, Media 1, Scheduler 3, Sync 9.
- **FrontendService has 26 RPCs**, none of them generated for Rust.
- Empty: Cards/Config/DeckConfig/Decks/ImageOcclusion/Links/Notes/Notetypes/
  Search/Stats/Tags have empty Backend services; Sync/Ankiweb/Github/AnkiHub
  have empty collection services.

---

## 4. Collection lifecycle

### 4.1 `CollectionBuilder` (`rslib/src/collection/mod.rs:35-124`)
| method | effect |
|---|---|
| `CollectionBuilder::new(col_path)` | path. `Default::default()` gives an in-memory `:memory:` DB |
| `set_media_paths(folder, db)` | media folder and media SQLite DB |
| `with_desktop_media_paths()` | **uses `foo.media` + `foo.mdb`** and creates the folder. **Warning:** desktop Anki uses `collection.media` + **`collection.media.db2`** (`pylib/anki/media.py:23-26`). This helper is only used by rslib tests. tanki must call `set_media_paths(<col>.media, <col>.media.db2)` explicitly to share media sync state with desktop. |
| `set_server(bool)` | server mode: real USNs instead of -1, and no local-offset config on create |
| `set_tr(I18n)` | translations (default: `I18n::template_only()`, English) |
| `set_check_integrity(bool)` | run `pragma integrity_check` on open |
| `set_shared_progress_state(Arc<Mutex<ProgressState>>)` | cross-thread progress/abort |
| `build() -> Result<Collection>` | opens via `SqliteStorage::open_or_create` |

`Collection::as_builder()` rebuilds an equivalent builder. It is used to
reopen after a full sync (`backend/sync.rs:422,436`).
`Collection::close(self, Option<SchemaVersion>)` closes, optionally after
downgrading.

### 4.2 Open/close internals (`rslib/src/storage/sqlite.rs`)
- `open_or_create_collection_db` (L52–80) sets:
  - `busy_timeout(0)`
  - **`locking_mode=exclusive`**
  - `page_size=4096`, `cache_size=-40MB`, `legacy_file_format=false`
  - **`journal_mode=wal`**
  - custom SQL functions: `field_at_index`, regexp variants, `process_text`,
    `fnvhash`, and FSRS sort functions
  - `TRACESQL` env var prints every statement.
- Schema: `SCHEMA_MIN_VERSION=11`, `SCHEMA_STARTING_VERSION=11` (new files),
  `SCHEMA_MAX_VERSION=18` (`storage/upgrades/mod.rs:5-9`).
  - `<11` gives `FileTooOld`; `>18` gives `FileTooNew`.
  - **12 and 13 are refused** as `FileTooNew` (old beta schema;
    `sqlite.rs:486-492`).
  - Anything below 18 is upgraded in place on open inside `begin exclusive`
    (`sqlite.rs:505-545`). Opening an old file with tanki therefore upgrades
    it.
  - A new collection gets the default deck config, the default deck, and the
    stock notetypes (localized through `tr`).
- `close(desired_version)` (L549) calls `downgrade_to(V11)` when requested and
  switches to `journal_mode=delete`. Desktop normally closes with
  `downgrade=False` (`qt/aqt/main.py:737`). It only downgrades on the
  explicit "Downgrade & Quit" profile action (`qt/aqt/profiles.py:342`).
  `SchemaVersion` is `{V11, V18}` (`storage/mod.rs:25`).
- No explicit WAL checkpoint happens on a plain close. SQLite checkpoints and
  removes the `-wal` file when the last connection closes. `checkpoint()` is
  called before backups and uploads.

### 4.3 Two processes on the same collection
Exclusive locking mode plus WAL plus a zero busy timeout means the first
connection to touch the DB holds the file lock until it closes. A second
opener gets `SQLITE_BUSY`. That maps to `AnkiError::DbError{kind: DbErrorKind::Locked}`
(`error/db.rs:45-52`), and its localized message is *"Anki already open, or
media currently syncing."* (`error/db.rs:103`). The mechanism is derived from
the code and was **not tested with two live processes**.

Consequences:
- **tanki and Anki desktop cannot have the same profile open at the same
  time.** Whichever opens second fails immediately; there is no waiting.
- A long-running TUI should open the collection, do its work, and close it.
  It should not keep the collection open while idle if the user also uses
  desktop.
- Error handling must expect `Locked`.
- The media DB is **not** exclusively locked on the client
  (`sync/media/database/client/mod.rs:317-319`, WAL only). The server's media
  DB is exclusive.
- `Backend` enforces one collection per Backend:
  `CollectionAlreadyOpen` / `CollectionNotOpen` (`backend/collection.rs:97-111`).

### 4.4 Backups (`rslib/src/collection/backup.rs`)
- `Collection::maybe_backup(folder, force) -> Result<Option<JoinHandle<Result<()>>>>`
  (L28). It skips the backup when nothing changed since the last backup, or
  when not forced and a backup newer than `minimum_interval_mins` exists.
- Otherwise it checkpoints the WAL, reads the DB bytes, and writes
  `backup-%Y-%m-%d-%H.%M.%S.colpkg` (no media) on a **spawned std thread**.
- It thins old backups per `Preferences.BackupLimits` (daily, weekly,
  monthly).
- Desktop's backup folder is `<profile>/backups` (`qt/aqt/profiles.py:317`).
  Desktop backs up on close (`main.py:730`, `force=False`) and on a periodic
  timer.

### 4.5 DB check
`CollectionService::check_database` (`dbcheck.rs:110` is `pub(crate)` natively).
- It reports through `DatabaseCheckProgress`.
- It returns localized problem strings.
- It runs **without undo**, and it clears the undo queue.
- Media check is `MediaService::check_media`.

### 4.6 Sketch: opening exactly like desktop does
```rust
use std::path::PathBuf;
use anki::collection::CollectionBuilder;
use anki::prelude::*; // Collection, I18n, AnkiError, Result, TimestampSecs, DeckId, CardId…

fn open_profile(profile_dir: &std::path::Path, langs: &[&str]) -> anki::error::Result<Collection> {
    let col_path = profile_dir.join("collection.anki2");
    let media_folder = profile_dir.join("collection.media");
    let media_db = profile_dir.join("collection.media.db2"); // NOT .mdb
    std::fs::create_dir_all(&media_folder)?;
    CollectionBuilder::new(&col_path)
        .set_media_paths(media_folder, media_db)
        .set_tr(I18n::new(langs))
        .build()
    // Err(AnkiError::DbError{ source }) with source.kind == DbErrorKind::Locked => desktop has it open
}
// on exit:  col.close(None)?;   // or Some(SchemaVersion::V11) for "downgrade & quit"
```
Desktop profile root on Linux is `~/.local/share/Anki2/<profile>/`. This was
not verified in this pass; see the aqt `profiles.py` base-dir logic.

---

## 5. Cross-cutting systems

### 5.1 Undo/redo
- `Op` enum (`rslib/src/ops.rs:7-53`) lists the user-visible undoable
  operations: AddDeck, AddNote, AnswerCard, Bury, Suspend, SetDueDate,
  UpdateNote, RemoveNote, RenameDeck, SetFlag, Import, UpdateDeckConfig,
  Custom(String), and others. `SkipUndo` means "don't record, don't clear".
  `Op::describe(&I18n)` gives the menu label ("Undo Answer Card").
- `Collection::transact(op, f)` is `pub(crate)`. It wraps the work in a
  `savepoint rust`, records `UndoableChange`s, bumps mtime, clears or
  rebuilds study queues as needed, and returns
  `OpOutput<T>{output, changes: OpChanges{op, changes: StateChanges}}`.
  `transact_no_undo` **clears both undo and redo queues**
  (`undo/mod.rs:72-76`, `begin_step(None)`). Examples that do this:
  check_database, check_media, update_stats, extend_limits, upgrade_scheduler,
  and full-sync preparation.
- Undo is held **in memory only**, with `UNDO_LIMIT = 30` steps
  (`undo/mod.rs:15`). It is lost on close. A new normal op clears redo.
- Public API:
  - `col.undo()` / `col.redo() -> Result<OpOutput<UndoOutput>>` (`undo/mod.rs:180-193`)
  - `col.undo_status() -> UndoStatus{undo: Option<Op>, redo: Option<Op>, last_step}`
  - `add_custom_undo_step(name) -> usize` and `merge_undoable_ops(start) -> Result<OpChanges>`
    for grouping
  - `OpChanges` helpers: `had_change`, `requires_browser_table_redraw`,
    `requires_browser_sidebar_redraw`, `requires_note_text_redraw`,
    `requires_study_queue_rebuild` (`ops.rs:144-170`). The proto `OpChanges`
    carries the same flags (`collection.proto:51-68`). A TUI should drive its
    redraws from these.
- Raw-SQL modification through dbproxy discards undo
  (`update_state_after_dbproxy_modification`).

### 5.2 Errors
- `pub type Result<T, E = AnkiError>` (`error/mod.rs:35`).
- `AnkiError` (L38) is a snafu enum. Variants include InvalidInput,
  TemplateError, CardTypeError, FileIoError, DbError{DbErrorKind: FileTooNew,
  FileTooOld, MissingEntity, Corrupt, Locked, Utf8, Other}, NetworkError,
  SyncError, JsonError, ProtoError, Interrupted, CollectionNotOpen,
  CollectionAlreadyOpen, NotFound, Deleted, Existing, FilteredDeckError,
  SearchError, InvalidRegex, UndoEmpty, MultipleNotetypesSelected,
  DatabaseCheckRequired, MediaCheckRequired, CustomStudyError, ImportError,
  InvalidId, the Fsrs* variants, SchedulerUpgradeRequired, and
  InvalidChecksum.
- `err.message(&tr)` gives localized text. `help_page()`, `context()` and
  `backtrace()` (needs `RUST_BACKTRACE`) are also available.
- `into_protobuf(&tr)` maps to `BackendError.Kind` (`backend/error.rs`).

### 5.3 i18n
- `anki::prelude::I18n` is a re-export of `anki_i18n::I18n`.
- `I18n::new(&["de", "en"])` builds Fluent bundles from **translations
  compiled into the binary**; no files are needed at runtime. English is
  always the fallback. `I18n::template_only()` is English only and disables
  Unicode isolation marks.
- `I18n` is `Clone`, holds `Arc<Mutex<..>>`, and is Send+Sync.
- Typed accessor per ftl message: `tr.studying_again()`,
  `tr.decks_<…>()`, and so on, each returning `Cow<str>`. Module names come
  from `ftl/core/*.ftl` (actions, adding, browsing, card-stats, …, studying,
  sync, undo) plus `ftl/qt/*.ftl`.
- `translate_via_index(module, msg, args)` is the dynamic form.
- `col.tr()` returns the collection's `I18n`.
- Strings contain FSI/PDI isolation characters (U+2068/U+2069) around
  arguments when real locales are requested. **Strip them for terminal
  output** with `anki_i18n::without_unicode_isolation` (`i18n/src/lib.rs:473`).
  `anki` re-exports only the `I18n` type, so tanki needs a direct `anki_i18n`
  dependency (same git source) for this helper.

### 5.4 Progress and abort
- `ThrottlingProgressHandler<P>` (`progress.rs:35`) writes into the shared
  `Arc<Mutex<ProgressState{want_abort, last_progress}>>` at most every 0.1 s.
  The next non-throttled update returns `AnkiError::Interrupted` if
  `want_abort` was set.
- Progress kinds (`progress.rs:134`): MediaSync, MediaCheck, FullSync,
  NormalSync, DatabaseCheck, Import, Export, ComputeParams (FSRS optimize),
  ComputeRetention, ComputeMemory, DownloadUpdate.
- Network operations are also aborted through
  `futures::future::Abortable`. On an aborted normal sync, Backend rolls back
  the transaction and tells the server `sync_abort` (`backend/sync.rs:365-381`).
- Native limits are covered in §2.2: abort works through the shared state's
  `want_abort`, but typed progress is not matchable from outside.
- `col.new_progress_handler::<P>()` is `pub` (`progress.rs:339`). It is
  needed to call `MediaManager::sync_media`.

### 5.5 Async/tokio
- `rslib` depends on tokio (rt-multi-thread) and reqwest 0.12.
- Backend lazily creates **its own** multi-thread runtime with
  `worker_threads(1)` (`backend/mod.rs:130-141`) and `block_on`s the async
  sync, login and media futures.
- Native public sync APIs are `async fn`:
  `sync_login(user, pass, endpoint, reqwest::Client)`,
  `Collection::normal_sync(&mut self, SyncAuth, Client)`,
  `full_upload(self, ..)`, `full_download(self, ..)`,
  `MediaManager::sync_media(self, progress, auth, client, server_usn)`, and
  `online_sync_status_check`.
- tanki needs a tokio runtime, which is natural with an async TUI loop. It
  also needs the **same reqwest major version** (0.12) to build a `Client`.
- Backend builds clients with `Client::builder().http1_only()`
  (`backend/mod.rs:179-185`). Nginx-proxied AnkiWeb wants HTTP/1, so mirror
  that.
- Everything outside sync (rendering, scheduling, search, import) is
  synchronous and CPU/SQLite-bound.

### 5.6 Thread-safety
- `Collection` is **Send but not Sync**. It owns a `rusqlite::Connection`
  (Send, !Sync). Send is proven by Backend storing it in a `Mutex` inside an
  `Arc` that is moved into `std::thread::spawn` (`backend/sync.rs:206`).
- `Backend` is `Clone` (it wraps `Arc<BackendInner>`) and is Send+Sync.
  - It serializes every collection call through `Mutex<Option<Collection>>`
    (`backend/mod.rs:57,114-125`).
  - It has separate mutexes for sync-abort, progress, backup task, media
    sync task, web client and state.
  - Long ops (sync, FSRS optimize, import) **hold the collection mutex** for
    their whole duration. Only `latest_progress` and `set_wants_abort` on the
    Backend trait avoid it.
- Recommended tanki model: one "collection actor" thread (or a
  `tokio::sync::Mutex<Collection>` / `spawn_blocking`) owns the `Collection`.
  The UI thread sends commands and reads progress from the shared
  `ProgressState` Arc.

---

## 6. Build facts for an external dependent crate

### 6.1 Toolchain and environment
- `rust-toolchain.toml`: `channel = "1.97.1"`. The workspace declares
  `rust-version = "1.80"`, but the code uses recent std APIs (`LazyLock`,
  `OnceLock`, let-else). Pin tanki to the same toolchain or newer.
- **protoc is required.** prost-build 0.13 does not vendor it.
  `rslib/proto/rust.rs:96-104` honours `PROTOC_BINARY` (overrides) or
  `PROTOC`; otherwise prost-build looks for `protoc` on `PATH`.
  - Anki's own `.cargo/config.toml` sets
    `PROTOC=out/extracted/protoc/bin/protoc` and
    `DESCRIPTORS_BIN=out/rslib/proto/descriptors.bin`.
  - **That config file is not applied when anki is a dependency of another
    workspace**, so tanki must provide protoc itself: a system package, its
    own `.cargo/config.toml` `[env]`, or CI setup.
  - The sync-server docs confirm this: "Protobuf (protoc) will need to be
    installed" (`docs-site/manual/sync-server.mdx:79`). The Dockerfile does
    `apk add protobuf` (`docs/syncserver/Dockerfile:5`).
- `DESCRIPTORS_BIN` is optional. Without it, `OUT_DIR/../../anki_descriptors.bin`
  is used. This relies on `anki_proto` and `anki` sharing a target dir, which
  is always true within one build.
- Optional env vars:
  - `BUILDHASH`: comes from `../out/buildhash`; missing means `"dev"`. It
    ends up in the sync client version string
    `"anki,26.08.1 (dev),linux"` (`version.rs:15-24`), which AnkiWeb sees.
  - `PLATFORM` overrides the OS in that string.
  - `EXTRA_FTL_ROOT`, `STRINGS_JSON`, `ANKI_TEST_MODE` (disables fuzz),
    `TRACESQL`, `RUST_LOG`.
- **Git submodules `ftl/core-repo` and `ftl/qt-repo` must be present**,
  because the i18n build script `read_dir().unwrap()`s them. Cargo git
  dependencies fetch submodules automatically. Path dependencies need
  `git submodule update --init`.
- The crate must be built from a **full repo checkout**. Build scripts use
  relative paths: `../../proto`, `../../ftl`, `../../.version`, and
  `../out/buildhash`.

### 6.2 Features and dependencies
- `anki` features (`rslib/Cargo.toml:12-15`):
  `rustls = ["reqwest/rustls-tls", "reqwest/rustls-tls-native-roots"]`,
  `native-tls`, and `bench`.
  - **There is no default TLS.** Workspace reqwest uses
    `default-features = false` (`Cargo.toml:115`). Without a TLS feature,
    HTTPS sync fails at runtime.
  - The sync server uses rustls on non-Windows (`rslib/sync/Cargo.toml`).
    tanki should enable `rustls`.
  - `SetCustomCertificate` only works with rustls.
- Heavy or important dependencies:
  - rusqlite 0.36 with `bundled` (compiles SQLite C, needs a C compiler)
  - **fsrs 6.6.1**. In this version the lockfile shows it depends on
    ndarray, rayon and priority-queue. **No burn/wgpu**, so it is much
    lighter than older FSRS versions.
  - tokio, reqwest, axum + hyper + tower-http (sync server code is compiled
    into the lib unconditionally), zstd (C), zip, flate2, regex, fluent,
    ammonia, pulldown-cmark, chrono, serde, prost.
  - `unicase = "=2.6.0"` is pinned; changing it could invalidate sqlite
    indexes (`Cargo.toml:50`).
  - Git dependency `percent-encoding-iri` comes from the `ankitects/rust-url`
    fork at a pinned rev.
  - The whole `Cargo.lock` has 595 packages, including build tooling and
    pyo3.
- **`publish = false`** on every crate. They are not on crates.io, and the
  workspace version is `0.0.0`. The only options are a git dependency (pin a
  tag or rev) or a path dependency, such as a vendored submodule.

### 6.3 Realism of an external dependency
- **It works.** The coordinator verified it with a path dependency, a copied
  `Cargo.lock` and `PROTOC`; see the top of this document. Upstream does the
  same for `cargo install --locked --git https://github.com/ankitects/anki.git --tag <ver> anki-sync-server`
  (`docs-site/manual/sync-server.mdx:74`).
- Recommended manifest:
  ```toml
  [dependencies]
  anki       = { git = "https://github.com/ankitects/anki.git", tag = "26.08.1", features = ["rustls"] }
  anki_proto = { git = "https://github.com/ankitects/anki.git", tag = "26.08.1" }
  # same reqwest major so you can build reqwest::Client for sync:
  reqwest = { version = "0.12", default-features = false, features = ["rustls-tls"] }
  ```
- What breaks or hurts:
  1. A missing `protoc` breaks the build.
  2. A missing ftl submodules breaks the build when using a path dependency.
  3. **Lockfile drift.** Anki's `Cargo.lock` is ignored for dependencies, so
     tanki resolves fresh versions. Anki relies on its lock (for example the
     exact `unicase`, and deps with an MSRV above Anki's). Copy Anki's
     `Cargo.lock` into tanki as the starting lock, which is what the
     coordinator did, and update deliberately.
  4. A rustc older than 1.97 may fail.
  5. Long compile times. Expect several minutes cold, with SQLite and zstd C
     builds.
  6. Upgrading Anki means re-pinning, and the API may break (§8).
  7. The git checkout pulls the whole repo, including the
     `qt/installer/*-template` submodules, which are harmless but slow.
- Other consumers:
  - The in-repo `anki-sync-server` and `rsbridge` are path consumers.
  - AnkiDroid's backend (rsdroid, external repo, not present here) consumes
    rslib through the Backend byte API. Evidence in this repo:
    `ankidroid.proto`, the `rslib/src/ankidroid/` module,
    `EXTRA_FTL_ROOT` "so the mobile clients can use their own translations",
    and the `set_media_paths` comment "Mobile clients use different paths"
    (`collection/mod.rs:92-94`).
  - No docs encourage third-party Rust use. `docs/api-rust.md` only says to
    run `cargo doc`.

---

## 7. License

- `LICENSE`: **GNU AGPL v3 or later**. Some portions contributed by users are
  BSD-3 (see `CONTRIBUTORS`). The workspace `license = "AGPL-3.0-or-later"`
  (`Cargo.toml:5`).
- Listed exceptions cover only vendored, non-Rust files:
  - `pylib/statsbg.py` (CC BY 4.0)
  - Qt translations (BSD/public domain), `mpv.py` and `winpaths.py` (MIT),
    MathJax (Apache 2), jQuery/jQuery-UI (MIT), `plot.js` (MIT),
    protobuf.js (BSD-3)
  - docs-site docs (CC BY-SA 4.0)
  - the Anki logo, which has a limited alternative license for referring to
    Anki in content (unmodified, with a link).
- **None of the exceptions cover rslib.** A binary that statically links the
  `anki` crate is a derivative work of AGPL code. tanki must be distributed
  under AGPL-3.0-or-later or a compatible license, with full corresponding
  source.
- AGPL §13 also applies if tanki is ever offered over a network, for example
  a hosted web-TTY.
- Dependencies' own licenses (MIT/Apache and others; see
  `cargo/licenses.json`) need attribution in binary distributions.
- Don't use Anki's name or logo in a way that implies it is official; the
  logo license requires making that clear.
- `CONTRIBUTORS`: contributions back upstream are requested under BSD-3.

---

## 8. API stability

- Explicit statement: *"At the moment, the protobuf is not considered public
  API"* (`docs/architecture.md:51`; `docs-site/developers/architecture.mdx:45`).
- No stability promise exists for the Rust API either:
  - every crate is `publish = false`, version `0.0.0`;
  - `docs/api-rust.md` is just "run cargo doc";
  - comments like `// currently public for dbproxy` (`storage/sqlite.rs:45`)
    and `// pub for tests` show visibility is incidental.
- Protos are reshuffled freely. `docs/protobuf.md` says field-number
  compatibility "is usually not a concern" except for messages stored in the
  DB.
- Churn, from `git log --oneline -30 -- proto/`:
  - the last 30 proto commits span 2025-04-27 to 2026-08-13;
  - 7 proto commits in the last 12 months versus 58 rslib/src commits
    (380 total commits);
  - most are FSRS/deck-options features: per-deck desired retention, CMRR,
    simulator, `last_review_time`, decay;
  - the scope includes adding and then **reverting** a `from_queue` field
    on `CardAnswer` (May 2025). The native `CardAnswer` struct has since
    gained `from_queue` anyway (`scheduler/answering/mod.rs:56`). That kind
    of field addition breaks struct-literal construction in a dependent
    crate.
- Expect small breaking changes on most Anki minor releases:
  - new struct fields (struct literals break, so prefer `..Default::default()`
    where `Default` exists);
  - renamed methods;
  - service index renumbering (only matters if you use `run_service_method`).
- Mitigation:
  - Wrap rslib behind a tanki-internal `core` module or adapter, so only one
    place touches `anki::*`.
  - Pin an exact tag.
  - Upgrade deliberately with integration tests against a sample collection.

---

## 9. Code sketches (signatures checked in source; the coordinator's spike exercised the same calls)

```rust
use std::sync::{Arc, Mutex};
use anki::collection::{Collection, CollectionBuilder};
use anki::prelude::*;                                   // Collection, I18n, CardId, DeckId, TimestampSecs, TimestampMillis, Result…
use anki::scheduler::answering::{CardAnswer, Rating};   // pub mod answering (scheduler/mod.rs:9)
use anki::services::{SchedulerService, CardRenderingService, CollectionService};

// ---- open (see §4.6), with cross-thread abort ----
let progress = Arc::new(Mutex::new(Default::default())); // infers anki's (unnameable) ProgressState
let mut col: Collection = CollectionBuilder::new(&col_path)
    .set_media_paths(media_folder, media_db)
    .set_tr(I18n::new(&["en"]))
    .set_shared_progress_state(progress.clone())
    .build()?;
// another thread can later do: progress.lock().unwrap().want_abort = true;

// ---- deck list ----
// decks/tree.rs:252  pub fn deck_tree(&mut self, timestamp: Option<TimestampSecs>) -> Result<DeckTreeNode>
let root: anki_proto::decks::DeckTreeNode = col.deck_tree(Some(TimestampSecs::now()))?;
for d in &root.children {
    println!("{} new={} learn={} due={}", d.name, d.new_count, d.learn_count, d.review_count);
}

// ---- choose deck & study ----
col.set_current_deck(DeckId(root.children[0].deck_id))?;          // decks/current.rs:10 -> OpOutput<()>
// scheduler/queue/mod.rs:90  pub fn get_queued_cards(&mut self, fetch_limit: usize, intraday_learning_only: bool) -> Result<QueuedCards>
let queued = col.get_queued_cards(1, false)?;                       // type QueuedCards is unnameable: just use the value
println!("counts: {} {} {}", queued.new_count, queued.learning_count, queued.review_count);
let Some(qc) = queued.cards.first().cloned() else { /* congrats: col.congrats_info()? */ return Ok(()) };

// ---- render ----
// notetype/render.rs:46  pub fn render_existing_card(&mut self, cid: CardId, browser: bool, partial_render: bool) -> Result<RenderCardOutput>
let out = col.render_existing_card(qc.card.id, false, /*partial_render*/ false)?;
// partial_render=false => Rust resolves {{FrontSide}} and filters itself; question()/answer() valid
let (q_html, q_av) = anki::card_rendering::extract_av_tags(out.question().into_owned(), true, col.tr());
let (a_html, a_av) = anki::card_rendering::extract_av_tags(out.answer().into_owned(), false, col.tr());
// q_html still HTML (+ out.css). TUI must convert HTML->text (anki::text::html_to_text_line / strip_html, or own renderer),
// and play q_av/a_av (AvTag::SoundOrVideo(filename) | Tts) itself; files live in the media folder.

// ---- answer buttons ----
// scheduler/answering/mod.rs:263  pub fn describe_next_states(&mut self, choices: &SchedulingStates) -> Result<Vec<String>>
let labels: Vec<String> = col.describe_next_states(&qc.states)?;   // ["<1m","6m","10m","4d"]

// ---- answer ----
// scheduler/answering/mod.rs:311  pub fn answer_card(&mut self, answer: &mut CardAnswer) -> Result<OpOutput<()>>
let mut ans = CardAnswer {
    card_id: qc.card.id,
    current_state: qc.states.current,
    new_state: qc.states.good,
    rating: Rating::Good,
    answered_at: TimestampMillis::now(),
    milliseconds_taken: 8_000,        // capped to deck preset inside
    custom_data: None,                // None = keep existing custom data
    from_queue: true,
};
let changes = col.answer_card(&mut ans)?.changes;                  // OpChanges; changes.requires_study_queue_rebuild() etc.

// ---- undo ----
if col.undo_status().undo.is_some() { col.undo()?; }

// ---- proto-trait equivalents (UFCS because inherent names collide) ----
let q = SchedulerService::get_queued_cards(&mut col,
    anki_proto::scheduler::GetQueuedCardsRequest { fetch_limit: 1, intraday_learning_only: false })?;
let problems = CollectionService::check_database(&mut col)?.problems;   // only reachable via trait

// ---- add a note ----
let nt = col.get_notetype_by_name("Basic")?.expect("stock notetype"); // notetype/mod.rs:199 -> Option<Arc<Notetype>>
let mut note = nt.new_note();                                          // notetype/mod.rs:128
note.set_field(0, "front")?; note.set_field(1, "back")?;               // notes/mod.rs:62
col.add_note(&mut note, DeckId(1))?;                                   // notes/mod.rs:90 -> OpOutput<usize>

// ---- close ----
col.close(None)?;
```

**Sync sketch (native; mirrors `backend/sync.rs`).** Not compiled in this
pass:

```rust
let rt = tokio::runtime::Runtime::new()?;
let client = reqwest::Client::builder().http1_only().build()?;
let auth = rt.block_on(anki::sync::login::sync_login(user, pass, None, client.clone()))?; // SyncAuth{hkey, endpoint, io_timeout_secs}
let out = rt.block_on(col.normal_sync(auth.clone(), client.clone()))?;                   // SyncOutput{required, server_message, host_number, new_endpoint}
match out.required {
    anki::sync::collection::normal::SyncActionRequired::FullSyncRequired { upload_ok, download_ok } => {
        let mut b = col.as_builder();
        rt.block_on(col.full_download(auth.clone(), client.clone()))?;   // consumes col; caller must reopen
        col = b.build()?;
    }
    _ => {}
}
let mgr = col.media()?;                                  // MediaManager owns its own DB connection
rt.block_on(mgr.sync_media(col.new_progress_handler(), auth, client, None))?; // could run on another thread
```
- Persist `auth.hkey` and `out.new_endpoint` (the AnkiWeb shard). Desktop
  stores these in the profile; rslib does not.
- On abort, desktop calls `storage.rollback_trx()`. tanki cannot do that,
  because `rollback_trx` is `pub(crate)`. The method is `pub(crate)` on an
  unnameable type, so it is not callable either way. **Risk:** aborting a
  native normal sync mid-way may leave an open transaction. Treat a native
  sync abort as "drop and reopen the collection", or let the sync finish.

---

## 10. Surprises and risks (summary)

1. `with_desktop_media_paths()` uses `.mdb` while desktop uses
   `.media.db2`. Using the helper silently forks media sync state.
2. Exclusive SQLite lock with a 0 ms busy timeout: tanki and desktop are
   mutually exclusive per profile, and the second one fails instantly with
   `Locked`.
3. A `Backend` user cannot call collection RPCs typed. They are private
   forwarding fns; only `run_service_method(svc_idx, method_idx, bytes)`
   works. `with_col` is `pub(crate)`.
4. Native is the better path, but there are gaps: `check_database`,
   `counts_for_deck_today`, graphs and check-media are trait-only;
   `transact` is crate-private; typed progress is unmatchable; and there is
   no sync-abort rollback.
5. Trait and inherent name collisions require UFCS.
6. Opening an older collection auto-upgrades it to schema 18. Schemas 12 and
   13 are refused.
7. No TLS by default: enable `features = ["rustls"]`.
8. protoc must be supplied, because anki's `.cargo/config.toml` is ignored
   for dependencies. The ftl submodules must be present.
9. AGPL-3.0-or-later applies to tanki as a whole.
10. Unstable API by explicit statement. Pin a tag and wrap rslib.
11. Desktop features that live outside rslib must be reimplemented in tanki:
    - custom-scheduling JS (`FrontendService.SetSchedulingStates`);
    - audio playback and TTS;
    - HTML card display (the output is HTML+CSS, possibly MathJax/LaTeX);
    - the profile manager (`prefs21.db`) and the sync-key storage;
    - the deck-options, stats and editor web UIs (rslib provides only their
      data RPCs).
