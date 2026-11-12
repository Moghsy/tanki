# 09 - Prior art, lessons learned, ecosystem and legal constraints

Date of research: 2026-11-11. Facts marked (V) were verified against a primary source in this session (GitHub/PyPI/crates.io APIs, repo files, ankiweb.net, forums.ankiweb.net). Items marked (U) are unverified or from memory/search snippets only.

## 0. Executive summary

1. **The gap is real.** Every terminal Anki tool found either (a) needs Anki Desktop + AnkiConnect running, (b) embeds the *Python* `anki` wheel (directly or as a sidecar), or (c) uses its own format. **No project found links the Rust `anki` crate (rslib) directly.** Several 2026 newcomers (Rust TUIs on ratatui) are converging on the idea but all shell out to Python for the collection/sync.
2. **Biggest risk is legal/policy, not technical: AnkiWeb's Terms of Service prohibit third-party clients.** Verbatim (V, https://ankiweb.net/account/terms): "You may access AnkiWeb ... through the synchronization functionality incorporated into the approved clients (Anki, AnkiMobile, AnkiDroid, and AnkiUniversal). Because other clients can cause problems, AnkiWeb does not currently allow access from browser extensions or other third-party clients. Instead, please use AnkiConnect ...". dae (2022): "the service is only provided for the main Anki clients"; (2023) "AnkiWeb's terms and conditions do not allow programmatic access." Exceptions go through a private ticket system; one pending request (Recall, July 2026) shows no public outcome.
3. **rslib's sync client sends the official identity** `anki,<version> (<buildhash>),<platform>` (V, rslib/src/version.rs; buildhash defaults to "dev", platform from `PLATFORM` env or OS). A tanki binary would therefore look like Anki to AnkiWeb unless we override. We must not disguise; plan: default sync target = user-configured self-hosted server; AnkiWeb sync opt-in with a clear warning, and seek written permission via the ticket system.
4. **rslib is usable but unstable.** `publish = false` (V), crates.io `anki` is a 2019 placeholder (v0.0.1, "only available through the GitHub repo") (V). dae (2023): "The Rust crate should be usable in other crates" provided AGPL-compatible license (V, issue #2520). No semver promise; pin a git rev/tag. Rust toolchain pinned to 1.99.0 in-repo (V).
5. **Licensing is easy**: license tanki AGPL-3.0-or-later. No trademark issue if we avoid "Anki" in the name. "tanki" collides with the "Tanki Online" game ecosystem and a PyPI package; recommend a different name (section 4).
6. **AnkiConnect-compat server is valuable** (Yomitan, asbplayer, Obsidian, etc.), AnkiConnect upstream was archived on GitHub 2025-11-04 (moved to SourceHut) (V), and a new competitor "Tsunagi" (Oct 2026) claims all 122 actions. The sanctioned-by-ToS path to Anki from third parties is AnkiConnect, so a daemon mode that speaks it is also policy-aligned.

## 1. Existing terminal Anki clients / SRS tools

| Project | Lang | Stars / last push (V) | Data access | Review | Add/Edit/Browse | Sync | Images / audio | Notes |
|---|---|---|---|---|---|---|---|---|
| lervag/apy (apyanki) | Python | 363 / 2026-11-10 | Python `anki` wheel on local collection; Anki need not run | yes (`apy review` per card), no full study loop | add (Markdown), edit, list/search, export/import md | via anki lib (uses collection; no own AnkiWeb flow documented) (U) | no TUI render | MIT. Very active. Markdown note format; "not affiliated" disclaimer |
| langfield/ki | Python | 118 / 2026-10-28 | direct SQLite on `collection.anki2` with lock + backups | no | notes as git-tracked markdown files; pull/push | not AnkiWeb; relies on Anki's sync | media version-controlled | AGPL-3.0; limitation: sibling cards in different decks unsupported |
| alvenw/clanki | Python (Textual) | 44 / 2026-04-06 | Python `anki==25.9.2` on local collection; Anki Desktop must be **closed** | yes (basic, cloze) | no | `clanki sync` but needs creds cached by Desktop first | images (textual-image), audio via ffplay | No image occlusion, no custom CSS, type-in planned |
| lonaparte/aktui | Python | new | Python `anki==25.9` | yes, edit via $EDITOR as JSON (raw HTML fields) | add/browse/edit | own login + AnkiWeb sync, opt-in media sync | - | pins anki version |
| AndreasPlt/anki-tui | Rust + Python sidecar | new (2026-10) | managed venv with `anki==25.9.4`; or AnkiConnect; or add-on bridge | yes, official scheduler | review only | via sidecar | HTML + CSS-ish render, Kitty images, audio | Closest to our UX goals; Python 3.10-3.13 runtime dependency |
| kenis1108/anki-tui | Rust + Python | new (2026-09) | Python `anki` | yes | add/edit/delete/browse/suspend/deck options/stats | AnkiWeb collection+media sync via backend | mpv audio, Kitty images (ratatui-image) | Most complete feature list; still Python runtime |
| ProfessionalGriefer/anki-tui | Rust | 4 / 2026-06 | AnkiConnect (Desktop running) | yes | no | n/a | Kitty/Sixel/iTerm via ratatui-image, afplay | tmux popup while AI agent works |
| neogib/anki-tui | Rust | new | AnkiConnect | no | add only | n/a | - | Reason: couldn't add cards by keyboard in Desktop |
| ubermenchh/anki-cli | Python | 62 / 2026-11-11 | AnkiConnect or direct SQLite (auto) | yes | yes | n/a | - | JSON output "for AI agents"; MIT |
| pluveto/ankiterm | (U) | - | AnkiConnect | yes | no | n/a | - | `--base-url` for remote use |
| raine/anki-llm | Rust | 309 / 2026-10 | AnkiConnect | no | bulk LLM generation/processing | n/a | TTS | Shows demand for scripting/bulk ops |
| noelzubin/revise-tui | Rust | 21 / 2025-02 | own SQLite, FSRS | yes | yes | no | - | not Anki-compatible |
| TBS1996/speki | Rust | 509 / 2024-12 | own format (+ some Anki import) | yes | yes | no | - | GPL-2.0, unmaintained per author |
| proycon/vocage | Rust | 197 / archived 2024-07 | own plain text | yes | text file | no | - | GPL-3.0 |
| nathanlct/anki-cli | Python | 10 / 2019 | AnkiConnect | yes | basic | n/a | - | dead |
| dandrok/terminal-anki | TypeScript | 1 | own SM-2 + import/export | yes | yes | no | - | not collection-compatible |
| chanq-io/vultan | Rust | repo gone (404) | markdown notes, Anki-compatible spaced rep | yes | - | no | - | |
| flashy, deckster, crablit, mnemo | Rust | tiny | own | | | | | Toy projects |
| FooSoft/anki-connect | Python add-on | 2084 / archived 2025-11-04 (V) | in-Desktop add-on, HTTP :8765 | via GUI actions | 110 `@util.api` actions (V, grep) | uses Desktop's | | Moved to git.sr.ht/~foosoft/anki-connect |
| Tsunagi (drepic) | add-on | - | HTTP API + AnkiConnect shim, event stream | | claims all 122 actions, 27 tools tested (V) | | | Supports Anki 26.08/26.09; Rust client WIP |
| ankiconnect-rs | Rust | 11 | AnkiConnect client crate | | | | | |
| ankitui.nvim, anki.nvim, knowledge.nvim, hascard, inka/inka2, genanki | (U) | not individually checked | | | | | | genanki writes .apkg only; inka converts markdown to cards via AnkiConnect (U) |

Not verified: anki-panky, ankivim, `inka` internals, hascard (repo not found under the path I tried), memo/mochi-like tools.

### Findings / lessons
- **Nobody links rslib.** The Python `anki` wheel (a PyO3 wrapper around rslib: `rsbridge`) is the de facto integration path. That works but forces a Python runtime, version pinning (`anki==25.9.x`), and PEP 668 pains (kenis1108 README documents venv headaches). A single static Rust binary is a genuine differentiator.
- **Pain points seen in READMEs:** "Anki Desktop must be closed" (collection lock), needing Desktop to have synced once to cache credentials (clanki), full-sync upload/download conflict prompts needing explicit confirm (aktui handles via exit code 2 + typed confirmation), version alignment between wheel and Desktop, no image occlusion, no custom card CSS.
- **Render approach in the wild:** AndreasPlt maps note-type CSS (colours, bold/italic, `display:none`, `::before`) onto terminal styles; others strip HTML. Images via Kitty graphics protocol with Sixel/iTerm2/half-block fallback through `ratatui-image`. Audio via mpv/ffplay/afplay on `[sound:]`.
- **Borrowable:** apy's Markdown note format (`model:`/`tags:`/`## Field` headings, multiple `# Note` blocks per file; export-to-file/update-from-file round trip); ki's one-file-per-note repo layout with guid header and backups-before-write; clanki's `ANKI_BASE` env; aktui's exit-code convention for full-sync decisions; ubermenchh's JSON output for agents; ProfessionalGriefer's tmux popup idea.

## 2. Third-party consumers of rslib

- **AnkiDroid** (`ankidroid/Anki-Android-Backend`, 94 stars, pushed 2026-11-10, GPL-3.0) includes Anki as a **git submodule** pinned to a commit and builds via `build.sh` with ninja/n2, NDK, rustup. Version = backend version + Anki version. Keeps its own `rust-toolchain.toml` matched to the submodule. No formal API-churn policy; they bump manually (V, README). rslib has an `ankidroid/` module (db.rs, service.rs) in-tree (V) so some mobile-specific API is maintained upstream.
- **Recall (Light Phone III)**: open-source AGPL, uses AnkiDroid's published bindings, only write is `answer_card` with scheduler-provided states, pins stable backend releases (V, forum 70300).
- **anki-sync-server**: in-tree `rslib/sync` binary, installable via `cargo install --git https://github.com/ankitects/anki --tag <ver> anki-sync-server` (needs protoc) (V, docs). Env vars `SYNC_USER1`, `SYNC_BASE`, etc. Docs say third-party servers are untested/not recommended, and clients may need a matching server version.
- **Third-party Rust dependents via git**: GitHub code search requires auth so I could not enumerate; the forum thread "Minimal example to use rslib" (Oct 2024) got no answers (V). The `ankitects/anki` workspace root has `[workspace.dependencies.percent-encoding-iri]` and `linkcheck` pointing to ankitects forks of rust-url/linkcheck by git rev (V) -- these are transitive git deps that cargo must fetch; they are the reason the crate cannot go to crates.io (dae, 2020: "two git-based forks").
- **crates.io**: `anki` = 2019 placeholder v0.0.1 owned by project (V); `publish = false` in rslib/Cargo.toml (V). No official or fork on crates.io found.
- **Build gotchas (V from source):**
  - `rslib/i18n/gather.rs` reads `ftl/core/` (in-repo) and `ftl/core-repo/core` (git submodule `ankitects/anki-core-i18n`) plus `qt` / `qt-repo` unless `extra_ftl_root()` is set (the mobile hook). Cargo does fetch submodules of git dependencies, but this is the fragile area; the `extra_ftl_root` env mechanism lets us supply only our own ftl and skip qt strings. Need to test.
  - `protoc` required; `PROTOC` / `PROTOC_BINARY` env respected (rslib/proto/rust.rs). Anki's own build downloads protoc 31.1.
  - `rslib/src/version.rs` does `include_str!("../../.version")` and reads `BUILDHASH`, `PLATFORM` env.
  - Toolchain pinned to 1.99.0 in repo; workspace `rust-version = 1.80`. "older versions may fail to compile".
  - `unicase = "=2.6.0"` pinned because changes can invalidate SQLite indexes (V) -- Cargo.lock matters; as a git dependency our lockfile will resolve other versions than Anki's. Prefer vendoring Anki's `Cargo.lock` versions or building from a submodule inside our workspace (the AnkiDroid pattern).
  - Features `rustls` / `native-tls` selectable on `anki` (V) -- choose `rustls` for static binaries.
  - Build time/binary size: not measured. (U) Expect a large dependency tree (axum, reqwest, fsrs, prost, ammonia, rusqlite bundled) and multi-minute cold builds; binary tens of MB. Must benchmark.
- **Maintainer statements on third-party use (V):** Issue #2520 (opened by dae May 2023): programmatic API in rslib like AnkiConnect is wanted; dae June 2023: crate "should be usable in other crates", AGPL-compatible licensing required; April 2026 commenter notes open PR #4546 for a native API (U on status). Dev docs (api-rust) say nothing about stability. AnkiMobile has its own Rust core use but isn't open to inspection.
- **Active churn:** `ankitects/anki` is very active (pushed daily; version 26.09.4 at HEAD (V)); Anki switched to YY.MM versioning in 2025. Expect to rebase tanki on each release (every ~1-3 months).

## 3. AnkiWeb / sync policy

- **Terms (V, quoted above):** approved clients only; third-party clients not allowed; AnkiConnect recommended; right to suspend accounts at sole discretion.
- **Maintainer statements (V):** 2022 (dae): "the service is only provided for the main Anki clients"; regrets AnkiUniversal ("haven't kept up with recent updates, and don't support media syncing"). 2023 (dae, e-ink app thread): "terms and conditions do not allow programmatic access", concern is free service, misuse, and breakage causing complaints. July 2026 (Recall request): mod directs to private ticket system.
- **Client identification:** sync header `client_version` = `anki,<version> (<hash>),<platform>` (V). Media begin also sends client_version and the server rejects empty values (V). We should set `BUILDHASH`/`PLATFORM` truthfully and ideally allow a distinct identifier; whether AnkiWeb filters on this string is unknown (U). Only "meta" version checks are known: old clients are told to upgrade (U).
- **Blocking history:** I found no public instance of AnkiWeb blocking an unofficial client or banning an account for it (searched forums; hits were network/firewall errors and an IP block after crawling shared decks) (U: absence of evidence). Python-wheel tools (clanki, aktui, kenis1108) already sync with AnkiWeb using the official library's identity, so in practice they are indistinguishable from Anki -- but that is exactly the ToS grey zone.
- **Rate/hammering:** no published rate limits found (U). Sync is naturally low-frequency; do not poll.
- **Self-hosted sync server (V):** built-in, `anki --syncserver` / `python -m anki.syncserver` / `anki-sync-server` crate; plain HTTP, put behind TLS proxy/Tailscale; `MAX_SYNC_PAYLOAD_MEGS`; client protocol version must match.
- **Recommendation:** (1) first-class support for custom server URL; (2) AnkiWeb sync behind explicit opt-in with notice about ToS; (3) email/ticket Ankitects proposing an "approved/identified client" status (Recall already did; watch the outcome); (4) a no-sync mode that is safe: operate on the local collection and let Anki Desktop/AnkiDroid or a self-hosted server sync.

## 4. Licensing, trademark, name

- **License (V):** Anki is "AGPL-3.0-or-later" (workspace Cargo.toml and LICENSE), some portions BSD-3 contributed by users. Static linking rslib makes tanki a derivative work; tanki must be AGPL-3.0-or-later (or GPLv3+ combined; simplest: AGPL-3.0-or-later). Dependencies must be AGPL-compatible (MIT/Apache-2/BSD fine; GPL-2.0-only is NOT compatible, e.g. don't copy from speki). Network clause: relevant if we ship an HTTP daemon (AnkiConnect API / sync server) used over a network -- offering source to remote users is required; for a local CLI it's mostly dormant but easy to satisfy (public repo, `--source`/`about` command with URL). Logos: Anki's logo is AGPL but with a limited alternative license; don't use it.
- **Trademark (V partially):** "Anki is a registered trademark of Ankitects Pty Ltd" (docs). FAQ on knockoffs says using "Anki" in a name implies compatibility with official clients; forum users (not maintainers) say standalone competitors should drop the name. Recall chose a non-Anki name + "Anki-compatible" descriptor. A separate USPTO "ANKI" application (Digital Dream Labs, 2025, classes 9/28) exists (U, not verified; it is the robotics company Anki, unrelated). I found no formal written third-party trademark policy (U). apy's precedent: README disclaimer "not affiliated/associated with the main Anki project".
  - Lowest-risk: no "anki" in the project/crate/binary name; use "Anki-compatible" descriptively with an unaffiliation disclaimer. "tanki" (t + anki) literally embeds Anki and so arguably trades on the mark -- another reason to rename.
- **"tanki" collision (V):**
  - crates.io: `tanki` free. npm: free. Homebrew: no formula. 
  - PyPI `tanki` exists: "API wrapper for Tanki Online's ratings API" v1.0.0.
  - GitHub: dozens of Tanki Online / Tanki X (game) repos (server emulators, map files, cheats). "Tanki Online" is a well-known browser tank game (Russian, AlternativaPlatform). Search-engine collision is high and cheat/emulator association is unflattering. Not a legal blocker but SEO/discoverability poor.
- **Alternative names (crates.io availability checked via API 2026-11-11, free = no crate):** `kartei` TAKEN; `mnemos` TAKEN; `spacer` TAKEN; `recall-cli` TAKEN; `ankr` TAKEN; `kioku` TAKEN. Free: `cardigan`, `cardamom`, `retain`, `karte`, `flashterm`, `leitner-tui`, `kioskard`, `ankiv`(contains Anki; avoid), `sakki`, `tanka`, `ankor`(contains Anki; avoid), `recallr`, `rmnemo`.
  - Suggested shortlist: **cardigan** (card + cozy, no Anki substring; GitHub search for "cardigan anki" empty), **cardamom**, **retain**, **sakki** (little relationship to anything), **recallr**, **karte** (German/Japanese for card), **kioku-tui**-style names (`kioku` itself taken; Japanese for memory, `ankey`-like avoided). Still need a GitHub/web-wide collision check for each (done only for cardigan: no hits; and PyPI/npm/brew checks were done only for tanki).

## 5. Add-on ecosystem and AnkiConnect

- Most valuable add-ons users expect (U, from general knowledge; not re-verified here): Review Heatmap (stats graph), AnkiConnect, FSRS Helper (largely upstreamed: FSRS is built into Anki, rslib has `fsrs` 6.6.2 dependency (V)), Image Occlusion (built-in since 23.10; rslib has `image_occlusion` module (V)), AwesomeTTS/HyperTTS, Advanced Browser, AnkiHub (rslib has `ankihub` module (V) -- AnkiHub integration possible later), Speed Focus, More Overview Stats, Yomitan->AnkiConnect mining, Migaku (proprietary).
- **Natively worth offering:** heatmap and stats (rslib `stats` module gives data), FSRS (built-in via rslib), image occlusion rendering (cards are just HTML+mask; viewing is feasible), TTS hook (external command per note type), auto-advance/speed-focus style timers, extra browser columns (rslib `browser_table`). Not worth: add-ons that need the Qt webview/JS (AnkiHub UI, Migaku).
- **AnkiConnect-compatible API:**
  - Surface: 110 `@util.api()` actions in upstream plugin source (V, grep of plugin/__init__.py); Tsunagi claims 122 (V, forum). Categories: decks (deckNames, createDeck, changeDeck, deleteDecks, getDeckConfig), notes (addNote, addNotes, updateNoteFields, findNotes, notesInfo, deleteNotes, addTags...), cards (findCards, cardsInfo, suspend, answerCards, setEaseFactors), models (modelNames, modelFieldNames, createModel...), media (storeMediaFile, retrieveMediaFile), graphical (guiBrowse, guiAddCards, guiCurrentCard...), misc (version, sync, multi, requestPermission, getNumCardsReviewedToday).
  - Which are most used (U, no ranking found; from what I know of Yomitan/asbplayer/mining tools): `version`, `deckNames`, `modelNames`, `modelFieldNames`, `canAddNotes`/`canAddNotesWithErrorDetail`, `addNote`, `storeMediaFile`, `findNotes`, `notesInfo`, `updateNoteFields`, `guiBrowse`, `multi`, `requestPermission`. A ~25-action subset likely covers Yomitan/asbplayer/Obsidian-to-Anki/anki-llm. `apiReflect` lists actions of a running instance. Verify by running Yomitan against a stub.
  - Value: high -- it is the one third-party access path the ToS endorses; AnkiConnect upstream archived Nov 2025 (V) and the incumbent now depends on a maintainer move and a new competitor (Tsunagi); a native Rust server avoids the Desktop requirement, works headless. Port 8765, CORS allow-list (`webCorsOriginList`), optional apiKey. Conflict: both can't bind 8765; when Anki Desktop runs concurrently the collection lock conflicts -- daemon must own the collection exclusively.

## 6. What users want from terminal Anki

Evidence is thin; no direct Reddit/HN threads retrieved (search tool returned only forum/README items), so treat as inference from project READMEs and forum posts (U):
- Add cards from the shell/editor (neogib: "couldn't do so using only my keyboard"; forum thread "Adding notes to Anki from the terminal"; apy Vim wiki; ankitui.nvim).
- Review without GUI: remote/SSH (ankiterm `--base-url`), tmux popups (ProfessionalGriefer), vim keys (hjkl/asdf grading in ankitui.nvim, FlexibleGrading add-on).
- Scriptable/JSON output, bulk operations, LLM pipelines (anki-llm 309 stars; ubermenchh "JSON for AI agents").
- Concerns: collection lock when Anki is open, no media, sync friction, wheel/Python install pain.
- Accessibility (U, not researched deeply): TUIs are screen-reader-friendly only if rendering is linear text; a plain CLI/`--json` mode and not relying on images for meaning would help. Worth an explicit non-TUI mode.

## 7. Gap analysis

No project offers all of: single static binary linking rslib, full review with correct scheduler (v3, FSRS), card HTML/CSS render with Kitty/Sixel images, audio, add/edit/browse with search syntax, notetype handling, stats, media + collection sync (custom server and AnkiWeb), AnkiConnect-compatible daemon, scriptable JSON CLI. Closest: kenis1108/anki-tui (feature list, Python-dependent) and AndreasPlt/anki-tui (render quality, Python sidecar). Both are weeks-to-months old and low-star, so the window is open but being approached.

## Sources
- https://ankiweb.net/account/terms (via text mirror) 
- https://forums.ankiweb.net/t/what-is-the-process-of-getting-new-clients-approved-for-ankiweb-sync/21350
- https://forums.ankiweb.net/t/custom-app-for-eink-device-using-ankiweb/38279
- https://forums.ankiweb.net/t/permission-request-ankiweb-sync-for-recall-a-review-only-client-for-the-light-phone-iii/70300
- https://forums.ankiweb.net/t/own-re-created-anki-app/70522
- https://forums.ankiweb.net/t/developing-the-anki-crate/1095
- https://forums.ankiweb.net/t/minimal-example-to-use-rslib/50401
- https://forums.ankiweb.net/t/tsunagi-modern-ankiconnect-alternative/71411
- https://forums.ankiweb.net/t/ankiterm-review-anki-decks-in-the-terminal/36855
- https://forums.ankiweb.net/t/is-there-any-supported-way-to-authenticate-ankiweb-login-without-using-the-gui/67637
- https://github.com/ankitects/anki (rslib/Cargo.toml, rslib/src/version.rs, rslib/i18n/gather.rs, rslib/proto/rust.rs, LICENSE, .gitmodules)
- https://github.com/ankitects/anki/issues/2520
- https://dev-docs.ankiweb.net/en/latest/api-rust.html
- https://docs.ankiweb.net/sync-server.html
- https://docs.ankiweb.net/faqs/anki-knockoffs
- https://github.com/ankidroid/Anki-Android-Backend
- https://github.com/lervag/apy ; https://github.com/langfield/ki ; https://github.com/alvenw/clanki ; https://pypi.org/project/clanki/
- https://github.com/AndreasPlt/anki-tui ; https://github.com/kenis1108/anki-tui ; https://github.com/ProfessionalGriefer/anki-tui ; https://github.com/neogib/anki-tui ; https://github.com/lonaparte/aktui ; https://github.com/ubermenchh/anki-cli ; https://github.com/raine/anki-llm
- https://github.com/FooSoft/anki-connect (archived; https://git.sr.ht/~foosoft/anki-connect)
- https://crates.io/api/v1/crates/anki ; https://pypi.org/pypi/tanki/json
