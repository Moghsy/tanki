# 06 - Card browser, search language, tags, bulk operations, statistics, card info

Source: Anki v26.08.1 at `/home/emard/projects/open-source/anki` (read-only; nothing was built or run, everything below is from reading code).
Paths are relative to that repo unless absolute. "rslib" = reusable Rust; "Py" = pylib (must be re-implemented); "Qt" = qt/aqt (re-implement); "TS" = ts/ Svelte (re-implement).

## 0. Executive map: what is reusable vs. what we must write

| Area | rslib (reuse) | Must re-implement in tanki |
|---|---|---|
| Search language | Parser, SQL writer, builder, normaliser, sort, `search_cards/notes`, `build_search_string`, `join_search_nodes`, `replace_search_node` | Search bar UX (history, completion, error display); saved-search storage convention; **no pagination API** (ids come back as one Vec) |
| Browser table | Column enum, per-cell text, row colour, per-row font, "notes mode" aggregation, sorting SQL | Lazy row cache + virtualised widget, selection model, column chooser UI, header sort indicator, mapping colours to terminal colours, config-key persistence (keys are NOT in rslib, see 2.5) |
| Sidebar tree | `deck_tree`, `tag_tree`, notetype names, collapse state (decks/tags), `set_deck_collapsed`, `set_tag_collapsed`, section-collapse bool keys | The whole tree composition (saved searches, Today, Flags, Card State, Decks, Tags, Note Types + fields/templates), flag label storage, modifier-combination of searches, drag/drop (TUI: move commands) |
| Browser actions | Every mutation is an rslib RPC (table in section 4) | Dialog logic: set-due prompt, reposition defaults (raw SQL in Py!), grade-now chooser, suspend/bury *toggle* decision, mark toggle, **find duplicates (Py only!)**, selection helpers |
| Tags | Everything: tree, rename, reparent, remove, bulk add/remove, clear unused, find&replace in tags, completion, canonicalisation, `marked`/`leech` semantics | Tag input UI w/ completion |
| Notes/cards/decks CRUD | All RPCs | Editor UI (other report) |
| Statistics | `graphs` RPC returns fully aggregated protobuf; `card_stats`; `get_review_logs`; `studied_today` | Every chart renderer, all table summaries/percentile/average maths that TS performs after the RPC, forgetting-curve maths, "Save PDF" |
| Preferences | `GetPreferences/SetPreferences`, Bool/String config enums, raw JSON config | Prefs UI |

**Crate-visibility caveat (verified):** many useful `Collection` methods are `pub(crate)` (e.g. `graph_data_for_search` at `rslib/src/stats/graphs/mod.rs:31`, `get_config_optional` at `rslib/src/config/mod.rs:109`, `studied_today` is pub but `get_first_day_of_week` is not). The reliable public surface is the generated service traits: `rslib/rust_interface.rs:83` emits `pub trait <X>Service { fn m(&mut self, input: ProtoIn) -> Result<ProtoOut>; }` implemented for `Collection` (e.g. `crate::services::SearchService` implemented in `rslib/src/search/service/mod.rs:23`), plus `Backend::run_service_method(service, method, bytes)` (`rust_interface.rs:141`). Plan on calling `anki::services::{SearchService, StatsService, TagsService, NotesService, CardsService, DecksService, NotetypesService, SchedulerService, ConfigService, CollectionService, ImportExportService}` with `anki_proto` messages. Proto crate: `proto/anki/*.proto`.

**Undo/refresh contract (rslib, reusable):** every mutating RPC returns `OpChanges` (`proto/anki/collection.proto`) with flags `card, note, deck, tag, notetype, config, deck_config, mtime, browser_table, browser_sidebar, note_text, study_queues`. The Qt browser uses `browser_table`/`browser_sidebar`/`note_text` to decide what to refresh (`qt/aqt/browser/browser.py:182-211`). Tanki should adopt the same: after any op, refresh table rows when `browser_table`, rebuild sidebar when `browser_sidebar`, reload editor when `note_text`. Undo/redo: `CollectionService.GetUndoStatus/Undo/Redo` (`UndoStatus{undo,redo,last_step}`; `OpChangesAfterUndo{changes,operation,reverted_to_timestamp,new_status,counter}`), implemented in `rslib/src/undo/mod.rs:180-195`. Undo names are localised via `Op::describe` (`rslib/src/ops.rs:55`).

---

## 1. Search language

### 1.1 Where it lives
- Parser (nom, rslib): `rslib/src/search/parser.rs` (1343 lines). Entry `pub fn parse(input) -> Result<Vec<Node>>` (:165). Empty/whitespace-only input => `[Search(WholeCollection)]`. All Unicode whitespace is normalised to ASCII space first (`normalize_whitespace`).
- SQL writer (rslib): `rslib/src/search/sqlwriter.rs` (1496 lines). Produces `select c.id from cards c [, notes n] where ...` or `select distinct n.id ...` + bound args.
- Normaliser/printer (rslib): `rslib/src/search/writer.rs` (`replace_search_node` :25, `write_search_node` :67).
- Programmatic builder (rslib): `rslib/src/search/builder.rs` (`SearchBuilder`, `JoinSearches::{and,or,and_flat,or_flat}`, `Negated::negated`, `SearchNode::from_deck_name/from_tag_name/from_notetype_name/from_template_name/from_note_ids/from_card_ids`). These escape `\ * _` for you.
- Public API: `rslib/src/search/mod.rs`: `Collection::search_cards(search, SortMode) -> Vec<CardId>` (:158), `search_notes` (:165), `search_notes_unordered` (:172). `search` argument is `&str`/`&String` (parsed, wrapped in a `Group`) or any `Into<Node>`/`SearchBuilder`.
- RPCs (`proto/anki/search.proto`): `BuildSearchString(SearchNode)->String`, `SearchCards(SearchRequest{search,order})->{ids}`, `SearchNotes`, `JoinSearchNodes{joiner,existing_node,additional_node}->String`, `ReplaceSearchNode{existing_node,replacement_node}->String`, `FindAndReplace`, `AllBrowserColumns`, `BrowserRowForId`, `SetActiveBrowserColumns`.
- Python only adds thin wrappers (`pylib/anki/collection.py`: `find_cards` :644, `find_notes` :676, `_build_sort_mode` :692, `build_search_string` :783). Nothing semantic.

### 1.2 Grammar (verified in parser.rs)
```
search   := node (WS node)*            ; adjacent nodes = implicit AND
node     := '-'? ( group | text )      ; '-' negates a term or a group
group    := '(' search ')'             ; empty group is an error (EmptyGroup)
text     := quoted | key ':' quoted | unquoted
unquoted := run of chars up to unescaped space/ideographic space(U+3000)/'"'/'('/')'
```
- `and` / `or` are case-insensitive keywords (`unquoted_term` :289). Implicit AND. A leading/trailing/doubled boolean is an error (`MisplacedAnd/Or`). Unbalanced `)` => `UnopenedGroup`, missing `)` => `UnclosedGroup`.
- **Precedence is SQL's:** the parser does not regroup; `a or b c` becomes `a or b and c` => `a or (b and c)`. Use parentheses.
- Quoting: `"foo bar"` (whole term) or `deck:"my deck"` (quotes must come after the colon; `partially_quoted_term`). Empty quotes error. Inside quotes `\"` is a literal quote.
- Escapes (`invalid_escape_sequence` :766): `\\ \" \: \* \_ \( \) \-` are the only legal escapes; any other `\x` is an `UnknownEscape` error (except inside `re:` values where text is passed through almost raw; only `\"`->`"`). A trailing odd `\` errors.
- A bare colon in plain text must be escaped `\:` (leading `:` => `MissingKey`).
- Wildcards (rslib/src/text.rs: `to_sql` :490, `to_re` :470, `is_glob` :453): `*` = any run of characters; `_` = exactly one character; `\*` and `\_` are literals. `%` is escaped for SQL LIKE.

### 1.3 Terms (complete list from `search_node_for_text_with_argument`, parser.rs:365)
Keys are matched case-insensitively (`key.to_ascii_lowercase()`); anything not listed is a **field search**.

| Syntax | Meaning (SQL writer fn) |
|---|---|
| `text` | Substring, any field, wraps in `%..%` and `LIKE` over `n.sfld` / `n.flds` (raw HTML included, so `b` also hits `<b>`); fields with "exclude from search" are skipped by building per-notetype field-position LIKE patterns (`write_unqualified` sqlwriter.rs:206). Respects `IgnoreAccentsInSearch` (strips combining marks). NFC-normalised if `normalize_note_text` (default true). |
| `"exact phrase"` | same, with spaces |
| `-term`, `-(group)` | negation |
| `field:text` | **whole-field match** of that field (wildcards allowed, no implicit `%`), across all notetypes having a field with that name (field name itself may contain wildcards, e.g. `fr*:x`). `field:` = empty field; `field:_*` = non-empty; `field:*` any (`write_single_field` :690). Name `*`/`_*`/`*_` => any field. |
| `field:re:regex`, `field:nc:text` | regex on that field (`(?i)` added); no-combining match |
| `re:regex` | regex over all fields (raw `n.flds` with 0x1f separators, so `^`/`$` anchor the whole joined string; `(?i)` default, use `(?-i)` to get case sensitivity) (`write_regex` :910) |
| `nc:text` | ignore accents/combining chars |
| `sc:text` | strip cloze markup before matching; only cloze notetypes |
| `w:text` | word-boundary (`\b..\b`) match, wildcards allowed (`write_word_boundary` :949) |
| `dupe:MID,text` | notes of notetype MID whose **first field** (HTML-stripped, checksummed) equals text (`write_dupe` :852) |
| `deck:NAME` | cards in deck **or its children** (also matches via original deck for cards in filtered decks). Glob allowed; `::` hierarchy; `deck:current` = selected deck(+children); `deck:filtered` = cards currently in any filtered deck (`odid != 0`); `deck:*` all (`write_deck` :505). Case-insensitive regex `^name($|\x1f)`. |
| `did:1,2,3` | those deck ids only, no children; matches current or original deck. Id list must match `^(\d+,)*\d+$`. |
| `note:NAME` | notetype name (glob ok, case-insensitive) |
| `mid:ID` | notetype id |
| `card:N` | 1-based template ordinal (clamped min 1) ; `card:NAME` template name (glob ok) |
| `nid:1,2` / `cid:1,2` | note / card ids |
| `tag:NAME` | note has tag NAME **or a descendant** (`NAME::x`). Case-insensitive, glob (`_` = any non-space char here). `tag:none` = untagged; `tag:*` = any tag; a name containing a space matches nothing. Regex `tag:re:..`, no-combining `tag:nc:..` (`write_tag` :303). |
| `is:new` / `is:review` (type review or relearn) / `is:learn` (type learn or relearn) / `is:due` (queue review/daylearn due today, learn/preview due within learn-ahead) / `is:buried` (either) / `is:buried-manually` / `is:buried-sibling` / `is:suspended` | state (`parse_state` :638, `write_state` :460). **There is no `is:leech`**: leech is `tag:leech`. |
| `flag:0..7` | 0 none, 1 red, 2 orange, 3 green, 4 blue, 5 pink, 6 turquoise, 7 purple; >7 error. SQL `(c.flags & 7) == n` |
| `added:N` | card created within N days (`c.id > cutoff`, N clamped >=1) |
| `edited:N` | **note** modified within N days (`n.mod`) |
| `introduced:N` | card's first-ever revlog entry (ease != 0) within N days |
| `rated:N` / `rated:N:B` | answered in last N days (N>=1), optionally with button B in 1..4 (`parse_rated` :630) |
| `resched:N` | manually rescheduled in last N days (revlog ease 0) |
| `prop:ivl OP n` | interval days (u32) |
| `prop:due OP n` | days relative to today (negative ok = overdue); handles review/daylearn and learn queues |
| `prop:reps OP n`, `prop:lapses OP n` | counts |
| `prop:ease OP f` | e.g. `prop:ease<2.5` (stored x1000) |
| `prop:pos OP n` | new-card position |
| `prop:rated OP -n[:B]`, `prop:resched OP -n` | relative-day variants (n must be <= 0) |
| `prop:s OP f` | FSRS stability (days) |
| `prop:d OP f` | FSRS difficulty, user scale 0..1 (stored 1..10 via `d*9+1`) |
| `prop:r OP f` | FSRS retrievability 0..1; new cards => false |
| `prop:cdn:KEY OP f` / `prop:cds:KEY OP str` | numeric / string custom data field (KEY alphanumeric) |
| `has-cd:KEY` | card has custom-data key |
| `preset:NAME` | cards whose deck uses that deck-options preset (exact name; no match => false) |

`OP` in `prop:` is one of `<= >= != = < >` (`parse_prop` :446). Internal-only nodes (no text syntax): `DeckIdWithChildren`, `HasMemoryState` (`writer.rs` prints them as "").
A proto-level filter set (`SearchNode` oneof, `search.proto`) exists for building searches without string syntax: group/negated/parsable_text/template/nid/dupe/field_name/rated/added_in_days/due_in_days/flag/card_state/nids/edited/deck/due_on_day/tag/note/introduced/field/literal_text; used by the sidebar.

Gotchas:
- Plain text is a SQL `LIKE` over raw field HTML; case-folding is SQLite's (ASCII-only for plain text; `re:` is Unicode-aware via the Rust regex crate). **Not verified at runtime.**
- `rated:` has no upper cap in code (docs elsewhere mention 365); not verified against docs.
- Searching `""` returns every card/note (`deck:*`).
- Config effects: `BoolKey::NormalizeNoteText`, `BoolKey::IgnoreAccentsInSearch`.

### 1.4 Sort orders
`SortMode { NoOrder, Builtin{column: Column, reverse}, Custom(String) }` (search/mod.rs:41). `Custom` is a **raw SQL ORDER BY fragment** (tables `c.`/`n.`); do not expose to users (injection surface; only use for internal sorts). `Builtin` reuses browser `Column` keys; per-column SQL in `card_order_from_sort_column` (:357) and `note_order_from_sort_column` (:391); some need helper temp tables created by `prepare_sort` (:416, `rslib/src/search/*.sql`: `sort_order` temp table). `Column::Question|Answer|Custom` are unsortable (`require!` error "Can't sort"); in notes mode `Stability|Difficulty|Retrievability` are unsortable too (`default_notes_order == None`). `reverse` flips asc/desc textually. Python's sort keys: `sortType`/`sortBackwards` (cards) and `noteSortType`/`browserNoteSortBackwards` (notes), default `noteFld` (`rslib/src/config/schema11.rs:21`; Py falls back to `noteCrt` if the stored key is unknown, `table/model.py:264`).

Result shape: a flat `Vec<id>` with no limit/offset. Fine for 1M cards (8 MB) but the call is synchronous and blocks the collection (`&mut self`); run it on a worker thread owning the collection, with cancellation by discarding stale results.

### 1.5 Programmatic helpers (all rslib)
- `BuildSearchString(SearchNode)` -> normalised string (what the browser shows after Enter: `qt/aqt/browser/browser.py:502-512`). Use it to validate/normalise user input: errors are structured `SearchErrorKind` (`rslib/src/error`) with localised text; Py shows them via markdown.
- `JoinSearchNodes` AND/OR flat join; `ReplaceSearchNode` replaces every same-kind term in the existing search (e.g. swap `deck:x`). These power the sidebar modifiers (section 3.3).
- Saved searches: **not in rslib**. Stored in collection config key `savedFilters` as JSON `{name: search_string}` (`qt/aqt/browser/sidebar/tree.py:1172-1178`), accessed through `ConfigService.GetConfigJson/SetConfigJson`. Syncs with the collection. Default search text for the browser: `StringKey::DefaultSearchText` (`defaultSearchText`), fallback `deck:current` (`browser.py:566-576`). Search history is in the **profile** (`profile["searchHistory"]`, 30 entries, `browser.py:537-547`), i.e. local, not in the collection; tanki should keep its own file.
- Find duplicates: **Python only** (`pylib/anki/collection.py:745`): `find_notes(search AND field)`, then raw SQL `select id, mid, flds from notes where id in ...`, field value HTML/media-stripped (`strip_html_media`), group by value, groups with >=2. rslib only has `dupe:` (single note, first field) and `note_fields_check` (add-time). Tanki must reimplement: search with `build_search_string(user_search, SearchNode{field_name})`, fetch notes via `GetNote`, resolve field ordinal by notetype (`GetFieldNames`), strip html (`StripHtml` RPC in card_rendering). Then offer "tag duplicates" (`tags.bulk_add` with the localised word `duplicate`) and "show group" via `nid:` search (`browser/find_duplicates.py:84-135`).

### 1.6 Terminal feasibility - search
- Parser/search/sort/normalise: **trivial** (direct calls).
- Search bar: **doable**. Vim-like `/` (or `:`) command line with history (own file), live validation through `BuildSearchString` on each keystroke (cheap), inline error from `SearchErrorKind` with caret; completion of: `key:` names (static list from table above), `deck:` (`GetDeckNames`), `tag:` (`CompleteTag`, rslib), `note:` (`GetNotetypeNames`), `card:`/field names (notetypes), `is:` values, `flag:`, `prop:` names. Quote/escape values when inserting completions (use `SearchNode::from_*_name` rules: escape `\ * _`).
- CLI: `tanki search 'deck:x is:due' --cards|--notes --sort noteFld --reverse --format json|ids|tsv|table --columns noteFld,deck,cardDue --limit N`; `tanki search --normalize 'foo  or  bar'` (BuildSearchString); `tanki saved list|add|rm`.

---

## 2. Browser table

### 2.1 rslib: `rslib/src/browser_table.rs` (685 lines)
`pub enum Column` (strum camelCase, :27) - the **wire key** is the string used in config and RPCs:

| Enum | Key | Cards-mode label (ftl) | Notes-mode behaviour | Default sort (cards / notes) |
|---|---|---|---|---|
| Custom | `""` | add-on | empty | none |
| Question | `question` | Question | first card's question, text-only | none (unsortable) |
| Answer | `answer` | Answer | first card's answer minus question prefix | none |
| CardMod | `cardMod` | Card Modified | max mtime | desc |
| Cards | `template` | Card (template name; cloze: `name N`) | label "Cards": number of cards | desc |
| Deck | `deck` | Deck | `(N)` if the note's cards span N decks; filtered: `Deck (Original)` | asc |
| Due | `cardDue` | Due | earliest due among non-new/non-suspended/non-buried/non-filtered cards | desc |
| Ease | `cardEase` | Ease | label "Average Ease"; avg ease% of non-new cards; new => "(new)" | desc |
| Lapses | `cardLapses` | Lapses | sum | desc |
| Interval | `cardIvl` | Interval | label "Average Interval"; avg of review/relearn | desc |
| NoteCreation | `noteCrt` | Created | note id timestamp | desc |
| NoteMod | `noteMod` | Note Modified | | desc |
| Notetype | `note` | Note (type) | | asc |
| OriginalPosition | `originalPosition` | New card position | | desc |
| Reps | `cardReps` | Reviews | sum | desc |
| SortField | `noteFld` | Sort Field | uses cell font; RTL aware | asc |
| Tags | `noteTags` | Tags | | asc |
| Stability | `stability` | FSRS stability | first card | desc / **none** |
| Difficulty | `difficulty` | FSRS difficulty (`NN%`) | first card | desc / none |
| Retrievability | `retrievability` | FSRS retrievability (`NN%`) | first card | desc / none |

Cell text details (verified): due = date or `New #N` or `(Filtered)`; parenthesised when buried/suspended (`card_due_str` :507); interval via `time_span` (localised "3 months"); Learn=>"(learning)"; dates via `date_and_time_string`. Tooltips only for Answer, CardMod, Cards, NoteMod, Notetype, Question.

`Collection::all_browser_columns()` (:267) returns `BrowserColumns{columns:[Column{key, cards_mode_label, notes_mode_label, sorting_cards, sorting_notes, uses_cell_font, alignment, cards_mode_tooltip, notes_mode_tooltip}]}` sorted by localised label (Custom excluded). `Sorting` = NONE/ASCENDING/DESCENDING (default direction). `Alignment` = START/CENTER.

`browser_row_for_id(id)` (:276): requires prior `SetActiveBrowserColumns(StringList)` (else `invalid: Active browser columns not set.`), reads `BoolKey::BrowserTableShowNotesMode` to decide whether `id` is a note id or card id, and returns `BrowserRow{cells:[Cell{text,is_rtl,elide_mode}], color, font_name, font_size}`. Errors: `AnkiError::Deleted` (row vanished), `DatabaseCheckRequired` (note without cards). The card is rendered only if Question/Answer columns are active (`card_render_required` :85), so those are the expensive columns.

Row colour (`get_row_color` :654): notes mode => `Marked` or default. Cards mode precedence: **flag (1-7) > marked tag > suspended > buried > default**. Enum `COLOR_{DEFAULT,MARKED,SUSPENDED,FLAG_RED,ORANGE,GREEN,BLUE,PINK,TURQUOISE,PURPLE,BURIED}`. Desktop palettes (ts/lib/sass/_vars.scss:332-425): flags red/orange/green/blue/fuchsia/teal/purple; suspended = yellow; buried = amber; marked = indigo/purple. Font name/size per template (`browser_font_name/size`), meaningless in a terminal.

### 2.2 Qt/Py logic (re-implement)
- `qt/aqt/browser/table/state.py`: `CardState`/`NoteState` own active columns, sort column, sort direction; toggling converts selection (card ids <-> note ids using raw SQL `select distinct nid from cards where id in ...` :35-43; in tanki use `GetNote`/`CardsOfNote`, or searches `cid:`/`nid:` + `SearchNotes`).
- `table/model.py`: holds `_items` (full id list from `search_cards/notes`) and `_rows` cache (id -> row). **Rows are loaded lazily** only when the view asks to paint them (`get_row` :68), via `browser_row_for_id`; cache entries become stale by timestamp (`_stale_cutoff`) after ops so repainting refetches only visible rows; `_block_updates` serves stale/blank rows during a running backend op (:76). Reverse-sorting an existing result is done in memory (`reverse` :282) without re-querying. Disabled rows (deleted) render as an error string and become unselectable.
- `table/table.py`: selection model (`select_all` :120, `invert_selection` :129, `select_single_card` :139, `to_row_of_unselected_note` :222: pick the nearest remaining row before deletion), prev/next/first/last (:210-219), header sort indicator + `_on_sort_column_changed` (:494: unsortable column => info dialog; switching column resets direction to its default, same column toggles by in-memory reverse), header context menu = column chooser (:462: every column in `all_browser_columns`, checkable, minimum 1 active), context menu = Cards or Notes menu of the main window (:441), selection restore across searches (`_save_selection/_restore_selection`).
- Persistence keys (Py constants, `pylib/anki/browser.py`): `activeCols` (default `["noteFld","template","cardDue","deck"]`), `activeNoteCols` (default `["noteFld","note","template","noteTags"]`), `sortType`, `noteSortType`, `sortBackwards`, `browserNoteSortBackwards`, bool `browserTableShowNotesMode`. All stored in collection config (synced). To stay compatible with desktop, tanki must use identical keys through `ConfigService.Get/SetConfigJson` (they are **not** in rslib enums) and must call `SetActiveBrowserColumns` after loading/changing them and when toggling cards/notes (the backend column set is per-collection state, `Collection.state.active_browser_columns`).
- Window title format: "Browse (x of y cards shown; z selected)" (ftl `browsing-window-title[-notes]`).

### 2.3 Terminal feasibility - table
**Doable.** A virtualised table widget (ratatui `Table` with own windowing): hold `Vec<i64>` ids in memory, fetch `BrowserRow` only for visible rows (+ a small overscan) on a worker thread, LRU cache keyed by id, invalidate on `OpChanges.browser_table`. Column widths: fixed ratios by column kind (SortField/Question/Answer flexible, Deck middle-elide as rslib specifies via `elide_mode`, dates fixed ~16 chars). Row colour => background/foreground tint (24-bit colour, fall back to 16 colours + a marker glyph column for flag/marked/suspended/buried for monochrome/accessibility). Multi-select: space toggles, `V` visual range, `ctrl-a`/`*` invert, `gg/G`, `j/k`, `/` search, `s` sort-by-column menu, `o` column chooser popup, `m` toggle cards/notes mode. The previewer and editor are separate panes (split horizontally when width >= ~120 cols, otherwise full-screen toggle).
HTML in Sort Field/Question/Answer is already flattened to one line by rslib (`html_to_text_line`), so cells are plain text; `[sound:..]` becomes a pretty marker via `prettify_av_tags`.
CLI: `tanki browse-rows --ids ... --columns ...` / `tanki search ... --columns ... --format json` reusing `BrowserRowForId` (set active columns first).

---

## 3. Sidebar tree (`qt/aqt/browser/sidebar/{item,tree,model,searchbar,toolbar}.py` - all Qt/Py, re-implement)

### 3.1 Sections (order from `SidebarStage`, built in `tree.py:519-537`) and their search nodes
Section collapse state is persisted in collection bool config: `COLLAPSE_SAVED_SEARCHES, COLLAPSE_TODAY, COLLAPSE_CARD_STATE, COLLAPSE_FLAGS, COLLAPSE_DECKS, COLLAPSE_NOTETYPES, COLLAPSE_TAGS` (`config.proto ConfigKey.Bool`; written via `set_config_bool`).

1. **Saved searches** (`_saved_searches_tree` :570): items from config `savedFilters`, sorted by name; click runs the stored string (`SearchNode(parsable_text=...)`). Actions: save current search (prompts a name, normalises via `BuildSearchString`, asks before overwrite), update with current, rename, delete (multi-select), drag a sidebar item onto the section to save it (`_handle_drag_drop_saved_search`).
2. **Today** (`_today_tree` :594): Due today `prop:due=0`; Added today `added:1`; Edited today `edited:1`; Studied today `rated:1`; First review `introduced:1`; Rescheduled `resched:1`; Again today `rated:1:1`; Overdue = `is:due -prop:due=0`.
3. **Card state** (:664): New, Learning, Review, Suspended, Buried (`is:new/learn/review/suspended/buried`).
4. **Flags** (:713): root = "any flag" (`-flag:0`), "No flag" (`flag:0`), then 7 flags; user-renamable labels stored in config JSON `flagLabels` = `{"1": "label", ...}` (`qt/aqt/flags.py:48`); empty rename restores default.
5. **Decks** (:795): "Current deck" (`deck:current`), then `DeckTree` (rslib `DecksService.DeckTree`) recursively; each item `deck:Parent::Child`; filtered decks get a different icon; expand state per deck = `SetDeckCollapsed{scope: BROWSER}` (separate `browser_collapsed` from the main-screen collapsed state). Root = `deck:_*`.
6. **Tags** (:747): "Untagged" (`-tag:_*`), then `TagTree` recursively; each item `tag:full::name`; collapse via `SetTagCollapsed`. Root = `tag:_*`.
7. **Note types** (:850): `GetNotetypeNames`-style list sorted case-insensitively (Py uses legacy `models.all()` JSON; rslib has `GetNotetype`, `GetNotetypeNames`, `GetNotetypeNamesAndCounts`); each notetype `note:NAME`, with children templates (`note:X card:N`) and fields (`note:X FieldName:_*`).
Sidebar quick filter (`searchbar.py`): a text box with 600 ms debounce; case-insensitive substring match over item names, expanding matched paths (`item.search`, `tree.py:226-271`).

### 3.2 Operations on sidebar items
- Rename: decks (`RenameDeck{deck_id,new_name}`; new name = prefix + typed name; "rename with parents" prompts for full `A::B::C`), tags (`RenameTags{current_prefix,new_prefix}`; shows "no notes updated" warning when count 0), saved searches (config JSON), flags (config JSON). Names may not contain `"`.
- Delete (Delete key / context menu): decks (`RemoveDecks`, confirmation + counts), tags (`RemoveTags{tags}` removes from all notes **and** the tag list), saved searches.
- Drag & drop reparent: decks (`ReparentDecks{deck_ids,new_parent}`; target root = id 0), tags (`ReparentTags{tags,new_parent}`; root = empty string). Allowed drop types computed in `_on_selection_changed` (:362). TUI substitute: cut/paste (`x`/`p`) or `:move` command with a target picker.
- Add note to deck from the context menu (opens Add dialog preset to that deck), "Manage note types / Cards / Fields", "Add/Remove from selected notes" (tags), "Find and Replace" (preselects the field when invoked on a field item), tree expand/collapse (+ children).

### 3.3 Click behaviour with modifiers (`tree.py:273-305`, `update_search`) - maps to RPCs
Let `previous` = the search bar text (`SearchNode(parsable_text=...)`) and `current` = the clicked item's node.
- plain click: `BuildSearchString(current)`.
- **Ctrl**: AND with previous: `JoinSearchNodes(AND, previous, current)`.
- **Shift**: OR with previous: `JoinSearchNodes(OR, ...)`.
- **Ctrl+Shift**: replace same-kind nodes: `ReplaceSearchNode(previous, current)` (e.g. swap one deck for another).
- **Alt**: negate the clicked item first (`SearchNode(negated=current)`), combined with any of the above.
- Multi-select + context "Search -> all selected (AND) / any selected (OR)" (:987).
Terminal mapping (no reliable modifier on Enter): `Enter` = replace, `a` = AND-add, `o` = OR-add, `n` = negate toggle, `r` = replace-same-kind, shown in a status hint; optionally `Shift+Enter` where the terminal supports kitty keyboard protocol.

### 3.4 Terminal feasibility - sidebar
**Doable** (tree widget with fold state; vim `h/l` fold, `j/k`, `/` filter, `Enter` search). It is a pure composition task over rslib data: ~300 lines. Drag/drop: not meaningful; use `x`/`p` or a "move to..." picker. Icons: Nerd-font/ASCII glyph fallback; colour dots for card-state and flags. Keep tree in a left pane toggled with a key (Qt: Ctrl+Shift+R toggles/focuses, Ctrl+Shift+F focuses the filter).

---

## 4. Browser actions (qt/aqt/browser/browser.py, forms/browser.ui) with RPC mapping

Desktop shortcuts from `qt/aqt/forms/browser.ui` for reference (Ctrl-based; terminal uses vim keys instead). "Py" column states what logic exists besides the RPC.

| Action | Desktop key | RPC / call | Extra logic (Py/Qt, re-implement) |
|---|---|---|---|
| Search | Ctrl+F focus bar, Enter | `BuildSearchString` then `SearchCards`/`SearchNotes` with `SortOrder` | history, error display (`browser.py:477-547`) |
| Toggle cards/notes mode | Ctrl+Alt+T | config bool `BROWSER_TABLE_SHOW_NOTES_MODE` + `SetActiveBrowserColumns` + re-search | selection conversion (`table.py:193`) |
| Add note | Ctrl+E | `NotesService.NewNote`, `DefaultsForAdding`, `NoteFieldsCheck`, `AddNote` | editor UI; deck/notetype defaults from `DefaultsForAdding` |
| Create copy | Ctrl+Alt+E | same as add, prefilled from selected note (`on_create_copy` :828) | |
| Change deck | Ctrl+D | `CardsService.SetDeck{card_ids,deck_id}` -> `OpChangesWithCount` | deck chooser (`GetDeckNames` / `GetDeckIdByName`; Py creates the deck if the typed name is new via legacy add) |
| Change notetype | Ctrl+Shift+M | `NotetypesService.GetChangeNotetypeInfo` then `ChangeNotetype{note_ids,new_fields,new_templates,old_notetype_id,new_notetype_id,current_schema,...}` (-1 = unmapped) | field/template mapping UI is Svelte (`ts/routes/change-notetype`), must be rebuilt; **requires full sync** (schema change) |
| Add tags | Ctrl+Shift+A | `TagsService.AddNoteTags{note_ids,tags}` | tag prompt w/ `CompleteTag`; whitespace-normalised |
| Remove tags | Ctrl+Alt+Shift+A | `TagsService.RemoveNoteTags{note_ids,tags}` | |
| Clear unused tags | menu | `TagsService.ClearUnusedTags` | |
| Toggle mark | Ctrl+K | `AddNoteTags`/`RemoveNoteTags` with tag `marked` (`MARKED_TAG` = "marked", `pylib/anki/tags.py:31`; check case-insensitive in `Note::is_marked`, browser_table.rs:147) | toggle decided from the *current* row's note having the tag (`browser.py:1083-1093`) |
| Flag 1-7 | Ctrl+1..Ctrl+7 (all seven have shortcuts in browser.ui) | `CardsService.SetFlag{card_ids,flag}` | pressing the current card's flag again sets 0 (`browser.py:1058-1068`) |
| Suspend toggle | Ctrl+J | suspend: `SchedulerService.BuryOrSuspendCards{card_ids,mode=SUSPEND}`; unsuspend: `RestoreBuriedAndSuspendedCards{cids}` | checked state derived from current card `queue == suspended` (:1009-1022) |
| Bury toggle | Ctrl+Shift+J | `BuryOrSuspendCards{mode=BURY_USER}` / `RestoreBuriedAndSuspendedCards` | state from current card queue (manually/sibling buried) |
| Forget (reset to new) | Ctrl+Alt+N | `SchedulerService.ScheduleCardsAsNew{card_ids,log,restore_position,reset_counts,context=BROWSER}` ; defaults via `ScheduleCardsAsNewDefaults` (bool keys `RESTORE_POSITION_BROWSER`, `RESET_COUNTS_BROWSER`) | small dialog with 2 checkboxes (`operations/scheduling.py:95`) |
| Set due date | Ctrl+Shift+D | `SchedulerService.SetDueDate{card_ids,days,config_key=SET_DUE_BROWSER}` | prompt; grammar `N`, `N-M` (random in range), trailing `!` = also reset interval (`rslib/src/scheduler/reviews.rs:91 parse_due_date_str`; `0` today, `1!` tomorrow+ivl 1); default text from string key `setDueBrowser` ("0") |
| Reposition | Ctrl+Shift+S | `SchedulerService.SortCards{card_ids,starting_from,step_size,randomize,shift_existing}`; defaults `RepositionDefaults{random,shift}` | dialog; shows queue top/bottom via **raw SQL** `select min(due),max(due) from cards where type=new and odid=0` (`operations/scheduling.py:134`) - replace with a search `is:new` + `prop:pos`/`GetCard` or `nextPos` config |
| Grade now | Ctrl+Shift+G | `SchedulerService.GradeNow{card_ids,rating}` (Again/Hard/Good/Easy) | 4-button popup |
| Delete notes | Ctrl+Del | `NotesService.RemoveNotes{note_ids}` | **Py only fires when table focused**; picks the nearest surviving row first (`to_row_of_unselected_note`) |
| Select all / invert / select notes | Ctrl+Alt+A / Ctrl+Alt+S / menu | none (UI); "Select Notes" = `build_search_string(SearchNode(nids=[...]))` then re-search + select all (`browser.py:1175`) | |
| Find & replace | Ctrl+Alt+F | `SearchService.FindAndReplace{nids,search,replacement,regex,match_case,field_name}`; for the "Tags" scope `TagsService.FindAndReplaceTag{note_ids,search,replacement,regex,match_case}` | dialog: scope "All fields", "Tags", or one field; "selected notes only" box; history. **Danger: empty `nids` means ALL notes** (`search/service/mod.rs:89-91`); Py disables the checkbox when nothing is selected - tanki must guard. Non-regex mode escapes search and `$` in replacement; case-insensitive adds `(?i)` (service/mod.rs:70-87); regex is Rust-regex syntax, `$1` capture refs. `field_names_for_notes` (`FieldNamesForNotes`) fills the field list. |
| Find duplicates | menu | Py `find_dupes` (see 1.5) + `AddNoteTags` | **no rslib implementation** |
| Preview | Ctrl+Shift+P | `CardRenderingService.RenderExistingCard{card_id,browser:true,partial_render:false}` -> `RenderCardResponse{question_nodes,answer_nodes,css,latex_svg,is_empty}` | question/answer flip, "show both sides" toggle persists in `PREVIEW_BOTH_SIDES`; autoplay if deck option; left/right arrows move through rows; replay key `R`; `B` toggles both sides (`previewer.py:103-120,278-292`); type-answer placeholders stripped (`type_ans_preview_filter`) |
| Card info | Ctrl+Shift+I | `StatsService.CardStats{cid}` | see section 7.3; follows current row |
| Export selected | Ctrl+Shift+E | `ImportExportService.ExportAnkiPackage{out_path,options,limit{note_ids}}` or `ExportNoteCsv`/`ExportCardCsv` | |
| Create filtered deck from search | Ctrl+G (Alt = second filter) | `DecksService.GetOrCreateFilteredDeck(0)` then edit `Deck.Filtered.search_terms[0].search = current search`, then `AddOrUpdateFilteredDeck{id,name,config,allow_empty}`; order labels via `FilteredDeckOrderLabels` | dialog |
| Undo / redo | Ctrl+Z / Ctrl+Shift+Z | `CollectionService.Undo/Redo`, `GetUndoStatus` | menu text from `UndoStatus.undo/redo` |
| First/Last/Prev/Next row | Home/End, Ctrl+P/N | none | |
| Focus editor / table / sidebar | Ctrl+Shift+N / Ctrl+Shift+L / Ctrl+Shift+R | none | |
| Column chooser | header right-click | `SetActiveBrowserColumns` + config JSON `activeCols/activeNoteCols` | |
| Manage notetypes | menu | Notetypes RPCs | other report |

Operations take **card ids** for card actions (flag, suspend, bury, set deck, reposition, due, forget, grade) and **note ids** for note actions (tags, delete, find/replace, change notetype, export). In notes mode `get_card_ids` expands notes to all their cards via raw SQL `select id from cards where nid in ...` (`table/state.py:41`); tanki: search `nid:1,2,3` with `SearchCards`.

All of these RPCs are undoable and return `OpChanges`. `BuryOrSuspendCardsRequest` also accepts `note_ids` (suspend/bury a whole note).

### Terminal feasibility - actions
Mostly **trivial** (single RPC each; prompts are one-line command-line inputs or small popups). **Doable**: change notetype mapper, find&replace dialog (form with fields: find, replace, scope, regex, match case, selection-only), filtered-deck creation (form), preview (needs HTML renderer, see 4.1). **Hard**: nothing in this list is blocked by the terminal. Suggested key scheme (vim-style, normal mode): `dd`/`D` delete (confirm), `m` mark, `1-7` flag (toggle), `s` suspend, `b` bury, `t` add tags, `T` remove tags, `c` change deck, `C` change notetype, `p`/`Space` preview, `i` card info, `r` reposition, `d` set due, `f` forget, `g` grade now, `R` find&replace, `u`/`ctrl-r` undo/redo, `:` command mode with all of these as commands. CLI equivalents: `tanki card suspend --search 'deck:x is:leech'`, `tanki note tag add 'tag1 tag2' --search ...`, `tanki card set-due '0' --search ...`, always with `--dry-run` printing counts (a search then count) and `--yes`.

### 4.1 Preview / card rendering in a terminal
`RenderExistingCard` returns template nodes (text with HTML, `[sound:]` tags, replaced filters) and CSS. Plain-text rendering via `html_to_text_line`/`StripHtml` is **doable** but loses layout; richer = own HTML subset renderer (b/i/u/br/ul/img/table/ruby). Images: **doable** through kitty/sixel/iTerm2 protocols (crates `ratatui-image`) with a text placeholder fallback. Audio: **doable** by spawning `mpv/ffplay` (Anki desktop shells out to mpv too). MathJax/LaTeX: **hard** (`latex_svg` via the rslib latex helper produces image files, then show as image; MathJax needs a JS engine - show raw `\(...\)`). Arbitrary CSS/JS (card templates, add-ons): **physically impossible** to reproduce faithfully; do not promise it. Offer "open in external browser/webview" as an escape hatch.

---

## 5. Tags (`rslib/src/tags/**`)

- Storage: `tags` table rows `{name, usn, expanded}`; notes store tags as a space-padded string. Hierarchy delimiter `::`. Tag names are case-insensitive but case is preserved; new tags adopt the case of an existing parent (`adjusted_case_for_parents`, `register.rs:128`). Separators are space and U+3000; control chars removed; empty component => `blank` (`register.rs:154-199`).
- Tree: `TagsService.TagTree` -> `TagTreeNode{name (leaf component), children, level, collapsed}`; missing parents are synthesised (`tags_to_tree` tree.rs:60), sorted case-insensitively. Collapse: `SetTagCollapsed{name,collapsed}` (`expanded` flag in tags table; `Op::SkipUndo`).
- `AllTags` -> StringList. `CompleteTag{input,match_limit}` -> `{tags}` (`complete.rs:9`): input split on `::`, each component a case-insensitive substring filter matched on consecutive components; prefix matches ranked first. Perfect for the tag prompt.
- Bulk: `AddNoteTags{note_ids,tags}` (`bulkadd.rs`; tags space-separated; tag matching by TagMatcher is prefix-aware), `RemoveNoteTags{note_ids,tags}` (`remove.rs:17`; removing a parent removes children too since matcher is prefix-aware: `foo` also matches `foo::bar`), `RemoveTags{tags}` removes from all notes + tag list (:12), `ClearUnusedTags` (:28), `RenameTags{current_prefix,new_prefix}` (renames the tag and all descendants; rejects spaces/empty), `ReparentTags{tags,new_parent}` (drag-drop semantic documented in `reparent.rs:20-26`: `foo`->`bar::foo`, `foo::bar`->`baz::bar`, root = no parent), `FindAndReplaceTag` (regex or literal on tag strings, `findreplace.rs`). All return `OpChangesWithCount` (count = notes changed) and are undoable.
- Special tags: **`marked`** (tag name compared case-insensitively; `Note::is_marked` browser_table.rs:147; Py const `MARKED_TAG`), **`leech`** (added by scheduler when a card becomes a leech: `rslib/src/scheduler/answering/mod.rs:355-357,566`; suspension depends on deck-option leech action). There is no "is:leech" search; use `tag:leech`. Tag ordering note: `tag:marked` also matches `marked::x`.
- Terminal: **trivial/doable**. Tag tree as a pane with `x/p` move, `r` rename (prompt prefilled with full name), `D` delete, `Enter` search; tag input with completion popup bound to `CompleteTag`. CLI: `tanki tags list [--tree] [--format json]`, `tanki tags rename OLD NEW`, `tanki tags add/remove TAGS --search ...`, `tanki tags clear-unused`.

---

## 6. Note/card/deck/notetype CRUD RPCs the browser/editor needs (all rslib)

`NotesService` (`proto/anki/notes.proto`, impl `rslib/src/notes/service.rs`):
- `NewNote(NotetypeId) -> Note{id,guid,notetype_id,mtime_secs,usn,tags[],fields[]}`
- `AddNote(AddNoteRequest{note,deck_id}) -> {changes: OpChangesWithCount, note_id}`; `AddNotes(AddNotesRequest{requests[]}) -> {changes,nids[]}`
- `DefaultsForAdding{home_deck_of_current_review_card} -> DeckAndNotetype{deck_id,notetype_id}` (`rslib/src/adding.rs:25`: if `AddingDefaultsToCurrentDeck` (key `addToCur`, default true) use current deck (home deck of reviewer card if current is filtered; else Default) and that deck's last notetype; else last-used notetype with its last deck, falling back to current deck). `DefaultDeckForNotetype(NotetypeId) -> DeckId` (last non-filtered deck cards of that notetype were added to; stored in aux config keys, `config/notetype.rs`). Both are config-backed; adding stores the choices.
- `UpdateNotes{notes[],skip_undo_entry} -> OpChanges` (the editor's save; also regenerates cards).
- `GetNote(NoteId) -> Note`; `RemoveNotes{note_ids,card_ids} -> OpChangesWithCount` (removing by card ids deletes their notes).
- `NoteFieldsCheck(Note) -> {state}` enum: `NORMAL, EMPTY, DUPLICATE, MISSING_CLOZE, NOTETYPE_NOT_CLOZE, FIELD_NOT_CLOZE` (`notes/mod.rs:597`). Logic: field-not-cloze beats everything; empty = **first field only** blank after HTML strip (media filenames preserved); then cloze errors; duplicate = another note of the **same notetype** with identical HTML-stripped **first field** (checksum then text compare; excludes self id). Use for the red "duplicate" banner and add-button gating in the Add screen.
- `ClozeNumbersInNote(Note) -> {numbers[]}`; `AfterNoteUpdates{nids,mark_notes_modified,generate_cards}`; `FieldNamesForNotes{nids} -> {fields[]}`; `CardsOfNote(NoteId) -> CardIds`; `GetSingleNotetypeOfNotes(NoteIds) -> NotetypeId` (errors if mixed; used to gate change-notetype).
`CardsService`: `GetCard`, `UpdateCards`, `RemoveCards`, `SetDeck`, `SetFlag`. `Card` message has all raw fields including `memory_state{stability,difficulty}`, `desired_retention`, `decay`, `last_review_time_secs`, `custom_data`.
`DecksService`: `NewDeck`, `AddDeck(Deck)->OpChangesWithId`, `GetDeckIdByName`, `GetDeck`, `UpdateDeck`, `RenameDeck{deck_id,new_name}` (name with `::` reparents), `ReparentDecks{deck_ids,new_parent}`, `RemoveDecks(DeckIds)->OpChangesWithCount` (deletes child decks and cards; undoable), `GetDeckNames{skip_empty_default,include_filtered}->DeckNames{entries[{id,name}]}`, `GetDeckAndChildNames`, `DeckTree{now}->DeckTreeNode{deck_id,name,level,collapsed,review/learn/new counts,... ,filtered,children[]}`, `SetDeckCollapsed{deck_id,collapsed,scope}`, `SetCurrentDeck`, `GetCurrentDeck`, filtered-deck RPCs. Rust helper `Collection::get_or_create_normal_deck(human_name)` is `pub` (decks/mod.rs:160); via RPC only legacy JSON (`AddDeckLegacy`) or `NewDeck`+`AddDeck`.
`NotetypesService`: `GetNotetypeNames -> NotetypeNames{entries[{id,name}]}`, `GetNotetypeNamesAndCounts -> {entries[{id,name,use_count}]}`, `GetNotetype`, `GetNotetypeIdByName`, `GetFieldNames`, `GetClozeFieldOrds`, `AddNotetype/UpdateNotetype/RemoveNotetype`, change-notetype pair, stock notetypes.
Terminal feasibility: all **trivial** to call; the editor UI is the hard part (other report).

---

## 7. Statistics

### 7.1 API
`StatsService` (`proto/anki/stats.proto`, impl `rslib/src/stats/service.rs`):
- `Graphs(GraphsRequest{search: string, days: u32}) -> GraphsResponse`. `search` is any search string (desktop uses `deck:current` or `""` = whole collection or a custom search); **empty string = whole collection and takes a fast path** (`graph_data_for_search`, mod.rs:31-39). The search is evaluated with `search_cards_into_table` so graphs honour full search syntax. `days` (0 = all history; desktop sends 365 or 0) **only limits the revlog window loaded** (`revlog_start = next_day_at - (days+1)*86400`, mod.rs:43); card-based graphs (card counts, intervals, ease, difficulty, stability, retrievability, future due, added) always use all searched cards.
- `GetGraphPreferences/SetGraphPreferences{calendar_first_day_of_week (SUNDAY=0/MONDAY=1/FRIDAY=5/SATURDAY=6), card_counts_separate_inactive, browser_links_supported, future_due_show_backlog}` stored as config (`FirstDayOfWeek`, `CardCountsSeparateInactive`, `FutureDueShowBacklog`).
- `CardStats(CardId)`, `GetReviewLogs(CardId)`. `SchedulerService.StudiedToday() -> String` (already localised, `rslib/src/stats/today.rs:10`: "Studied N cards in M minutes today (Xs/card)") and `StudiedTodayMessage`.

### 7.2 Graph inventory (GraphsResponse; page order from `ts/routes/graphs/+page.svelte:28-44`)
Day indexes are **relative to today (today = 0, negative = past; future-due positive)** with Anki's rollover (`rollover_hour` supplied). "Young" = interval < 21 days, "mature" >= 21 (`last_interval` for revlog-based). Maps are protobuf `map<int, ...>`, i.e. sparse; absent key = 0.

| # | Graph (TS component) | Response field & shape | Data size | Interactions on desktop | Terminal rendering |
|---|---|---|---|---|---|
| 1 | Today (`TodayStats`) | `today: Today{answer_count, answer_millis, correct_count, mature_correct, mature_count, learn_count, review_count, relearn_count, early_review_count}`; TS composes 4 text lines (studied, again count %, type counts, mature %) (`today.ts`) | 9 ints | none | **trivial**: text block |
| 2 | Future due (`FutureDue`, GraphRange 1M/3M/1Y/All) | `future_due: FutureDue{map<int32,u32> future_due, have_backlog, daily_load}`; day offset -> card count (excludes new & suspended; buried due today excluded) | sparse map, up to ~years | backlog checkbox; click bar => `prop:due>=a prop:due<=b` search; table (total, average/day, due tomorrow, daily load) | **doable**: vertical bar chart with block glyphs (`▁▂▃▄▅▆▇█`), bins into <=70 columns (TS logic `buildHistogram` future-due.ts:59: `x.ticks(min(70,range))` + cumulative line); show cumulative as second row or in status; click => select bin + Enter opens browser search |
| 3 | Calendar heatmap (`CalendarGraph`) | derived from `reviews.count` (sum of 5 kinds per day) + `rollover_hour` + first day of week pref | one value per day, up to 365 x years | year selector, hover tooltip, click => `rated:N` / `prop:rated` search; streak stats (longest streak, days learned %, daily avg) computed in TS | **doable, nice**: GitHub-style grid, 7 rows x 53 cols, each cell `█` or `■` coloured with 5 intensity steps (truecolor ramp, 256-colour fallback; sqrt scale like d3 `scaleSequentialSqrt`); 2 cols per week still fits in 120 cols. Streak numbers: re-implement |
| 4 | Reviews (`ReviewsGraph`, 1M/3M/1Y/All; counts vs time) | `reviews: ReviewCountsAndTimes{map<int32,Reviews{learn,relearn,young,mature,filtered}> count; ... time (ms)}` | sparse by day | count/time toggle; stacked bars; table (days studied %, total, avg over period, avg for days studied, avg answer time) | **doable**: stacked bar, <=70 buckets (TS bins days into `x.ticks(desiredBars)`), 5 colours; cumulative overlay optional |
| 5 | Card counts (`CardCounts`) | `card_counts: CardCounts{including_inactive, excluding_inactive: Counts{new_cards, learn, relearn, young, mature, suspended, buried}}`; pref `card_counts_separate_inactive` chooses which | 7 ints x2 | pie; table with % and search link (queries in `card-counts.ts`) | **trivial**: single stacked horizontal bar + legend table (pie is not terminal-friendly) |
| 6 | Intervals (`IntervalsGraph`; ranges 1M/50%/95%/All) | `intervals: Intervals{map<u32 days,u32 count>}` (review+relearn cards only) | up to thousands of distinct keys | percentile range, histogram; table median/avg/longest (TS computes by expanding the map into a flat sorted array) | **doable**: histogram of <=70 bins; implement percentiles over the map without expanding (cumulative walk) |
| 7 | Stability (FSRS; `StabilityGraph`) | `stability: Intervals` (stability rounded to days, only cards with memory state) | same | same as intervals | doable (same widget) |
| 8 | Ease (SM-2; `EaseGraph`) | `eases: Eases{map<u32 percent,u32>, average(median)}`; only cards **without** memory state | ~100s keys | percentile range, median | doable |
| 9 | Difficulty (FSRS) | `difficulty: Eases{map<u32 0..99%,u32>, average(median %)}` | <=100 keys | | doable |
| 10 | Retrievability (FSRS) | `retrievability: Retrievability{map<u32 0..99%,u32>, average, sum_by_card, sum_by_note}` | <=100 keys | | doable |
| 11 | True retention (`TrueRetention`) | `true_retention: TrueRetentionStats{today,yesterday,week,month,year,all_time: {young_passed,young_failed,mature_passed,mature_failed}}` (only reviews with rating and scheduling effect, cards with interval>=1d) | 24 ints | display modes Young/Mature/All/Summary; compares with desired retention | **trivial**: table |
| 12 | Hourly breakdown (`HourGraph`) | `hours: Hours{one_month,three_months,one_year,all_time: [24 x {total,correct}]}` (local time via `local_utc_offset`) | 4x24 | range radio; bars + correct-% line | **trivial**: 24-column bars + a % row |
| 13 | Answer buttons (`ButtonsGraph`) | `buttons: Buttons{one_month,..,all_time: {learning,young,mature: [4 counts]}}` | 4x3x4 | range radio, grouped bars (+ correct %) | **trivial**: grouped bars or small table |
| 14 | Added (`AddedGraph`) | `added: Added{map<int32 day,u32>}` (card creation day, ceil'd) | sparse | 1M/3M/1Y/All; cumulative | doable (bar histogram) |
| - | Meta | `rollover_hour`, `fsrs: bool` (switches Ease<->Difficulty/Stability/Retrievability visibility in TS: show SM-2 graphs when `!fsrs`, FSRS graphs when `fsrs`) | | | |

The range controls: global (`RangeBox.svelte`): scope radio **Deck** (`deck:current`) / **Collection** (`""`) / custom search text; history radio **1 year (days=365)** / **All history (days=0)**. Per-graph range (`GraphRange`: 1 month = 31 days for future/added... / 3 months = 90 / 1 year = 365 / all time) is applied **client-side in TS** on the returned data (`millisecondCutoffForRange` etc.); only the revlog window is server-side. Also in TS (re-implement): histogram binning (d3 `bin` + `ticks`), percentile range trimming (`percentageRange.ts`), median/average tables, calendar layout/streaks, cumulative overlays, tooltips, "browser link" queries (the exact strings in `future-due.ts:41`, `card-counts.ts:~60-90`: e.g. young = `("is:review" AND -"is:learn") AND "prop:ivl<21"`).
The workload/retention **simulator** (`simulator.ts`, `SimulateFsrsReview`, `SimulateFsrsWorkload` RPCs) belongs to deck-options, not stats; out of scope here.

### 7.3 Card info (Card Info screen)
RPC `CardStats(CardId) -> CardStatsResponse` (`stats.proto:29`, impl `rslib/src/stats/card.rs:16`): `revlog[StatsRevlogEntry{time, review_kind, button_chosen, interval(secs), ease(per mille), taken_secs, memory_state?, last_interval(secs)}]` (newest first, with per-entry FSRS memory state reconstructed by replaying the revlog), `card_id, note_id, deck, added, first_review?, latest_review?, due_date?, due_position?, interval(days), ease, reviews, lapses, average_secs, total_secs, card_type(template name), notetype, memory_state?, fsrs_retrievability?, custom_data, preset, original_deck?, desired_retention?, fsrs_params[]`. **Side effect:** it writes back `last_review_time` and may compute+store `memory_state` (`card.rs:23-40,59-61`), so open the collection read-write and expect an mtime-less write (no undo entry).
Rows shown (`ts/routes/card-info/lib.ts:15-125`, in this order, each conditional): Added; First review; Latest review; Due date; New card position; Interval; (FSRS: Stability [+ days when >31d], Difficulty (`(d-1)/9*100`%), Retrievability %) else Ease %; Review count; Lapse count; Average time + Total time (if any); Card template; Note type; Deck (`deck (original deck)`); Preset; Card ID; Note ID; Custom data (`k=v` pairs).
Review-log table (`Revlog.svelte`): columns Date, Time, Type (Learn/Review/Relearn/Filtered/Manual/Rescheduled; classes colour them), Rating (1..4; Again highlighted), Interval, Ease/Difficulty (ease%; for FSRS-era entries difficulty), Time taken, "Elapsed time" since previous valid review ("N/A" for manual/rescheduled/non-rated filtered), Stability. Computation of elapsed is TS (re-implement; `filterRevlogEntryByReviewKind`).
Forgetting curve (`ForgettingCurve.svelte`, `forgetting-curve.ts`; shown only with FSRS memory state): retrievability `R(t) = (1 + factor*t/S)^(-decay)`, `factor = 0.9^(1/-decay) - 1`; decay comes from `fsrs_params[20]` (FSRS-6; default 0.1542 if no params, 0.5 if <21 params = FSRS-4.5/5); sampled between review points; ranges Week/Month/Year/All chosen automatically by history length; desired-retention line. **Entirely TS** => re-implement (10 lines); data = the revlog with memory states.
`GetReviewLogs(CardId) -> ReviewLogs{entries[StatsRevlogEntry]}` is the lightweight version without memory states/FSRS replay.
Desktop "copy card info" (Ctrl+C) dumps JSON incl. raw `cards` row and revlog rows via raw SQL (`card_info.py:60-100`) - for tanki, `tanki card info ID --format json` can output `CardStatsResponse` directly (MessageToDict equivalent via serde from prost-reflect or hand-rolled).
Terminal: table rows = **trivial**; revlog table = **trivial** (scrollable); forgetting curve = **doable** as a braille/line chart (`ratatui::widgets::Chart` with `Marker::Braille`, ~1000 sample points downsampled to terminal width x 4).

### 7.4 "Save PDF" (`qt/aqt/stats.py:108-130`)
Qt WebEngine `page().printToPdf(path)` of the whole Svelte page. **Not reusable.** Terminal alternatives: (a) `tanki stats --format json` (the raw `GraphsResponse`) - trivial and most valuable; (b) static HTML or SVG export (re-implementing renderers once as SVG gives both PNG-for-terminal-graphics and file export) - doable; (c) PDF by SVG->PDF crate (`svg2pdf`/`resvg`) - doable but optional; (d) screenshot-style text export - trivial.

### 7.5 Rendering strategy verdict (terminal charts vs bitmap)
Data are small (<=~1000 numbers per chart after binning) and every chart is a bar/stacked bar/heatmap/line. Recommended:
1. Primary: native terminal charts - bars with eighth-blocks (vertical resolution 8x rows), stacked bars with 5 colours, pie => single 100%-wide stacked bar with legend, heatmap => coloured cell grid, line (forgetting curve) => braille. Works over SSH, in tmux, with no graphics protocol. **Doable.**
2. Optional enhancement: if the terminal supports kitty/sixel/iTerm graphics (detect via `ratatui-image`/`viuer`), render the same data as an SVG/PNG (e.g. `plotters`/`resvg`) for smooth axes and hover-less but crisp charts. **Doable but extra**, and also unlocks image export; not necessary for v1.
3. Not recommended: embedding the Svelte graphs (needs a JS runtime + browser). The only things lost relative to desktop: hover tooltips (replace with a cursor + status line), animated transitions, click-through to browser (replace with Enter on a bar => run search in the browser pane, using the same query strings).
CLI: `tanki stats [--search 'deck:x'] [--days 365] [--graph future|reviews|calendar|counts|intervals|ease|retention|hours|buttons|added|today] --format json|table|spark`; `tanki card info ID`.

---

## 8. Preferences / collection config

Raw collection config (JSON key/value in the `config` table; **synced** with the collection) via `ConfigService`: `GetConfigJson(key)`, `SetConfigJson{key,value_json,undoable}`, `SetConfigJsonNoUndo`, `RemoveConfig`, `GetAllConfig`, `GetConfigBool{key: ConfigKey.Bool}`, `SetConfigBool{key,value,undoable}`, `GetConfigString/SetConfigString`, `GetPreferences/SetPreferences(Preferences)`.

### 8.1 `ConfigKey.Bool` (`proto/anki/config.proto`; storage names in `rslib/src/config/bool.rs`, default false unless noted)
`BROWSER_TABLE_SHOW_NOTES_MODE`, `PREVIEW_BOTH_SIDES`, `COLLAPSE_TAGS`, `COLLAPSE_NOTETYPES`, `COLLAPSE_DECKS`, `COLLAPSE_SAVED_SEARCHES`, `COLLAPSE_TODAY`, `COLLAPSE_CARD_STATE`, `COLLAPSE_FLAGS`, `SCHED_2021` (v3 scheduler), `ADDING_DEFAULTS_TO_CURRENT_DECK` (key `addToCur`, **default true**), `HIDE_AUDIO_PLAY_BUTTONS`, `INTERRUPT_AUDIO_WHEN_ANSWERING` (**true**), `PASTE_IMAGES_AS_PNG`, `PASTE_STRIPS_FORMATTING`, `NORMALIZE_NOTE_TEXT` (`normalize_note_text`, **true**), `IGNORE_ACCENTS_IN_SEARCH`, `RESTORE_POSITION_BROWSER` (**true**), `RESTORE_POSITION_REVIEWER` (**true**), `RESET_COUNTS_BROWSER`, `RESET_COUNTS_REVIEWER`, `RANDOM_ORDER_REPOSITION`, `SHIFT_POSITION_OF_EXISTING_CARDS`, `RENDER_LATEX`, `LOAD_BALANCER_ENABLED` (**true**), `FSRS_SHORT_TERM_WITH_STEPS_ENABLED`, `FSRS_LEGACY_EVALUATE`.
Rust-only `BoolKey` extras not in the proto enum (reachable only by raw JSON key or internally): `ApplyAllParentLimits, CardCountsSeparateInactive (true), FutureDueShowBacklog (true), NewCardsIgnoreReviewLimit, MergeNotetypes, WithScheduling, WithDeckConfigs, Fsrs, FsrsHealthCheck (true), ShowDayLearningCardsFirst (`dayLearnFirst`), ShowIntervalsAboveAnswerButtons (`estTimes`, true), ShowRemainingDueCountsInStudy (`dueCounts`, true)` (`rslib/src/config/bool.rs`). Note `Fsrs` (key `fsrs`) is how graphs know the FSRS state.
### 8.2 `ConfigKey.String`
`SET_DUE_BROWSER` (default "0"), `SET_DUE_REVIEWER` (default "1"), `DEFAULT_SEARCH_TEXT` (""), `CARD_STATE_CUSTOMIZER` ("").
### 8.3 Numeric / other (`rslib/src/config/{number,mod}.rs`)
`I32ConfigKey`: `csvDuplicateResolution, matchScope, lastFsrsOptimize`. `ConfigKey` (JSON keys): `creationOffset, firstDayOfWeek, localOffset, rollover, backups, updateNotes, updateNotetypes, timeLim` (answer time limit secs), `curDeck`, `curModel`, `lastUnburied`, `collapseTime` (learn-ahead secs), `newSpread` (0 mix / 1 reviews first / 2 new first), `nextPos`, `schedVer`, `customColorPickerPalette`. Per-notetype aux keys (last deck, etc.) via `get_aux_notetype_config_key`.
### 8.4 `Preferences` message (what the desktop Preferences dialog edits; `GetPreferences/SetPreferences`, undoable `Op::UpdatePreferences`)
- `scheduling`: `rollover` (hour), `learn_ahead_secs`, `new_review_mix` (DISTRIBUTE/REVIEWS_FIRST/NEW_FIRST), `new_timezone` (v2 only), `day_learn_first` (v2 only)
- `reviewing`: `hide_audio_play_buttons, interrupt_audio_when_answering, show_remaining_due_counts, show_intervals_on_buttons, time_limit_secs, load_balancer_enabled, fsrs_short_term_with_steps_enabled`
- `editing`: `adding_defaults_to_current_deck, paste_images_as_png, paste_strips_formatting, default_search_text, ignore_accents_in_search, render_latex`
- `backups`: `daily, weekly, monthly, minimum_interval_mins`
### 8.5 Keys used by the Qt browser that exist **only in Py/Qt** (must replicate verbatim for desktop compatibility)
`activeCols`, `activeNoteCols`, `sortType`, `noteSortType`, `sortBackwards`, `browserNoteSortBackwards`, `savedFilters`, `flagLabels`, plus profile-local (not in collection): `searchHistory`, window/splitter geometry, browser layout. Collection-level tanki-specific settings should use a prefixed key (e.g. `tanki.*`) to avoid clobbering.
Terminal: **trivial** (settings screen = list of toggles; `tanki config get|set key [value]`).

---

## 9. Proposed tanki browser design (summary)

- Layout: left sidebar tree (toggle `Tab`/`Ctrl-b`), centre virtualised table, right/bottom pane switching between preview, editor, card info; top search line. Status bar: "N of M cards, K selected", undo/redo labels from `GetUndoStatus`.
- Data flow: one collection actor thread owning `Collection`; UI sends commands (search, fetch rows, op) and receives results + `OpChanges`; stale-result tokens.
- Search UX: `/` opens the search line, live-normalise, completion popup, `Ctrl-r` history, `Ctrl-s` save as saved search, sidebar `a/o/n/r` combiners.
- Selection model: cursor + set<id>; in notes mode ids are note ids; helper `selected_card_ids()` using `nid:` searches.
- Keep desktop-compatible config keys so tanki and desktop Anki can share a synced collection.
- Stats screen: top bar with scope (deck/collection/custom search) and history (1y/all); chart grid in a scrollable column or tabbed per graph; `Enter` on a bar/cell opens the browser with the matching query.

## 10. Risks, surprises, unverified

1. **Find Duplicates and Reposition min/max live in Python** (raw SQL). Reimplement.
2. **`FindAndReplace` with empty nids = all notes** (rslib `search/service/mod.rs:89`). Guard in tanki UI/CLI (`--all` flag required).
3. **Config keys for table/sidebar/saved searches are only Py conventions**; wrong keys break desktop compatibility; `SetActiveBrowserColumns` is mandatory before `BrowserRowForId`.
4. `CardStats` mutates the card (last_review_time, memory state) - not a pure read; also computing FSRS state per call can be slow for long revlogs.
5. Many `Collection` methods are `pub(crate)`; the stable surface is the generated `*Service` traits taking protobuf messages (verified for traits; not compiled).
6. Search is SQLite `LIKE`/`regexp` over raw field HTML; plain-text case-insensitivity and Unicode folding were not verified at runtime. `SortMode::Custom` is raw SQL.
7. `search_*` returns all ids at once and takes `&mut Collection`; large results block the actor; no cancellation.
8. `rated:` cap (docs) vs code (no cap) - unverified.
9. All TS-side maths (binning, percentiles, streaks, forgetting curve, revlog "elapsed", table summaries) must be ported; `GraphsResponse` is raw aggregates, not display-ready. TS also decides FSRS vs SM-2 graph visibility from `fsrs`.
10. Card preview fidelity: HTML/CSS/JS/MathJax/add-ons cannot be reproduced; plan a text+image renderer with an "open externally" fallback.
11. Change-notetype and some schema ops force a full sync prompt (`current_schema` field); needs a confirmation UX.
12. Not read in detail (out of scope/other reports): editor, deck options, import/export dialogs, filtered-deck dialog, sync.

## 11. Addendum (coordinator follow-up): coverage check of stats / card info / config

Sections 7 (stats proto, `rslib/src/stats/**` per-graph aggregation, TS post-processing, card info, PDF), 7.3 (card info rows, revlog table, forgetting curve) and 8 (Bool/String enums, Rust-only keys, `Preferences` message, Py-only keys) were completed in the first pass; this addendum records extra verified details:
- `GraphsContext` (`rslib/src/stats/graphs/mod.rs:22`) holds `revlog`, searched `cards`, `next_day_start`, `days_elapsed`, `local_offset_secs`; hours/buttons use fixed windows 30/90/365 days independent of `days`; true retention windows are today/yesterday/7/30/365/all.
- Revlog kinds excluded from reviews/today/hours: Manual and Rescheduled (hours also excludes Filtered; buttons treat Filtered/Relearning as "learning").
- Future-due: new and suspended cards skipped; `daily_load = sum(1/max(ivl,1))`.
- Prefs `GraphPreferences` are stored via config keys `firstDayOfWeek`, `cardCountsSeparateInactive`, `futureDueShowBacklog`.
