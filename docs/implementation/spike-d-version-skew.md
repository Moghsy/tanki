# Spike D: desktop version skew

Status: done, 2026-11-13. The pair under test is Anki desktop **25.09.4**, the
user's installed launcher, and tanki's pinned core **26.08.1 at `4aa9c0836`**.

Confidence labels follow the plan: **[spike]** means run in
`crates/tanki-core/tests/version_skew.rs`, **[code]** means read from the
source, not run.

## How it was tested

- `crates/tanki-core/tests/version_skew.rs` builds a fixture with the pinned
  core. It first uses the actor: the stock fixture, six answers with SM-2 and
  three extra Basic notes. It then uses `anki` directly:
  - edits the Default preset to use the new review order `RELATIVE_OVERDUENESS`
    (12), desired retention 0.87, explicit FSRS-6 parameters,
    `historical_retention`, `ignore_revlogs_before_date`, easy days and a new
    cards/day limit;
  - adds a second preset with the same review order;
  - turns FSRS on;
  - writes the config keys `experimentalFeatures` and
    `customColorPickerPalette`;
  - adds the note type "Skew special fields", whose card 2 is
    `{{#Tags}}{{Back}}{{/Tags}}`, and one tagged note of that type;
  - adds a normal deck with deck-level limits and a deck-level desired
    retention of 0.93;
  - answers three cards with FSRS on, one of them storing custom data;
  - moves a card that has an FSRS state into the override deck;
  - adds a filtered deck whose search term uses the new order
    `RELATIVE_OVERDUENESS` (10).
- The test snapshots everything the pinned core can read back into sorted
  `path = value` lines. A snapshot covers every deck preset, deck, note type,
  config key, the preferences, every card (including its FSRS state, desired
  retention, decay, last review time and custom data), every note, every review
  log entry, the schema version, `sync_status_offline`, and the pinned core's
  own Empty Cards result. Two snapshots of the same file must be identical, so
  the snapshot is shown to change nothing.
- For each scenario, a fresh copy of the closed fixture is opened, changed and
  closed by `scripts/version-skew.py` using the 25.09.4 `anki` package (it
  checks `anki.buildinfo.version`). The pinned core then reopens the copy,
  takes a new snapshot and diffs it against the first. Each difference is
  classified as bookkeeping (mtime/USN), deliberate (the edited object and its
  direct effects) or unexpected. The test asserts which unexpected differences
  occur (none) and also what survives.
- Python reads `ver`, `scm`, `ls` and `mod` through `col.db`, read-only, on
  opening and again just before closing.
- **Control:** with `TANKI_SKEW_PINNED_PYTHONPATH` set, every scenario also runs
  with the pinned 26.08.1 Python package. The test then prints how the two end
  states differ. In the run described here, the only differences were the e2
  and e4 findings below. The rebuild in e3 also picked or ordered cards
  differently, which is expected (see item 3).

Run it:

```
TANKI_SKEW_PYTHON=$HOME/.local/share/AnkiProgramFiles/.venv/bin/python PYTHONDONTWRITEBYTECODE=1 \
  cargo test -p tanki-core --test version_skew -- --nocapture
# optional: TANKI_SKEW_PINNED_PYTHONPATH=<main checkout>/.tools/pinned-python (control run)
# optional: TANKI_SKEW_DUMP_DIR=<dir> (writes every snapshot as text)
```

When `TANKI_SKEW_PYTHON` is not set, the test skips and prints why.

## 1. Inventory: what the newer core can store that 25.09.4 does not know

Source: `git diff 25.09.4 4aa9c0836` over the protos and rslib folders listed
in the brief, plus `revlog`, `notes`, `tags`, `collection`,
`image_occlusion` and `pylib`/`qt` config-key uses. Commit numbers are
upstream PRs; "first in" is the first release tag that contains the commit.

**No new field, table, column, config key or card-data key is written by the
pinned core.** The evidence:

- The schema is 18 in both versions.
- `notetypes.proto` and `cards.proto` are unchanged. `DeckConfig` and `Deck`
  have no new fields.
- `rslib/src/storage/card/data.rs` (the card `data` JSON with keys `pos`, `s`,
  `d`, `dr`, `decay`, `lrt` and `cd`) is unchanged.
- `BoolKey` and `StringKey` are unchanged. The only addition to `ConfigKey` is
  an accessor for `customColorPickerPalette`, which 25.09.4's Python side
  already wrote.
- The remaining proto changes are runtime-only messages: `SchedulingContext`,
  `SimulateFsrsWorkloadResponse` and the `ExperimentalFeatureFlag` enum.

What did change:

| # | Item | Where stored | Kind | Since |
|---|---|---|---|---|
| 1 | Review sort order `REVIEW_CARD_ORDER_RELATIVE_OVERDUENESS = 12` | `deck_config.config` protobuf, field `review_order` | new enum value | #4400 (`db811351e`), first in 26.05 |
| 2 | Review sort orders `RETRIEVABILITY_ASCENDING` (7) / `_DESCENDING` (11) | same field | **meaning changed**: with FSRS, 25.09.4 sorts by retrievability *relative to desired retention* and 26.05+ by absolute retrievability. Without FSRS, 25.09.4 uses an SM-2 overdueness formula; 26.05+ uses `extract_fsrs_retrievability`, which is NULL for cards without FSRS state, so those cards are effectively unsorted. The old behaviour moved to value 12 | #4400 |
| 3 | Filtered deck order `RELATIVE_OVERDUENESS = 10` | `decks.kind` protobuf, `search_terms[].order` | new enum value | #4400 |
| 4 | Filtered deck orders `RETRIEVABILITY_ASCENDING` (8) / `_DESCENDING` (9) | same | **meaning changed** as in item 2. Without FSRS, the pinned core builds an empty sort expression (`order by , fnvhash(...)`), which looks like it fails [code, not run] | #4400 |
| 5 | Cards from templates that depend on a special field (`{{#Tags}}`, `{{#Deck}}`, `{{#Card}}`, `{{#Subdeck}}`, …) | `cards` rows | **card generation changed**: the newer core treats special fields as non-empty (Tags only when the note has tags) and creates the card. 25.09.4 neither creates it nor considers it non-empty | #4533 (`c41668026`, 26.05) and #4807 (`3186cc31c`, 26.08) |
| 6 | Config key `experimentalFeatures` (`{"<flag>": bool}`) | `config` table | new key, written only by 26.08 *desktop* (Python), never by the core | #5057 (`db98bb675`), 26.08 |

Behaviour-only differences. Nothing new is stored, but the same action gives
different results in the two programs [code]:

- **FSRS short-term steps with empty (default) parameters.** The newer core
  allows them (#4412). 25.09.4 does not, so the same learning card can be
  scheduled differently depending on which program answers it.
- **Moving a card between decks.** The newer core recomputes the FSRS state and
  the stored desired retention for cards without a memory state when they move
  (#5339, pinned `main` after 26.08.1). 25.09.4 leaves them empty until the
  next answer or optimisation.
- **Cards in filtered decks** use the home deck's preset and deck-level desired
  retention for answering and load balancing in the newer core (#4413, #4574).
- **`ignore_revlogs_before_date` set in the future** is clamped to today on
  save by the newer core (#5294).
- **Check Database** in the newer core also corrects a wrong
  `last_review_time` in card data (#4648).
- **Intraday learning order** prefers cards that were attempted before (#4393).
- **The FSRS crate** is 5.1.0 in 25.09.4 and 6.6.1 in the pinned core (#4956).
  Recomputing memory states gave identical stability and difficulty values in
  this fixture (scenario e5). The fixture's review histories are only one or
  two reviews long, so this is weak evidence for long histories.

## 2. Results per scenario [spike]

Every scenario: 25.09.4 opened the file. `ver` stayed 18, and **`scm` and `ls`
were unchanged** (no forced full sync). The pinned core reopened the copy
without error. `sync_status_offline` reports `NoChanges` because the fixture
has never synced (`ls = 0`), so `scm` is the real check. Cards the scenario did
not touch kept identical due, interval, FSRS state, desired retention, decay
and custom data.

| Scenario | What 25.09.4 did | Unexpected differences | Notes |
|---|---|---|---|
| a | open, close | none; zero differences of any kind, `mod` included | |
| b | deck options round trip (`get_deck_configs_for_update` → `update_deck_configs`, current preset only), new/day +1 | none | review order 12 kept on both presets |
| c | legacy `decks.get_config` / `update_config` and `decks.save` on both presets | none | schema-11 JSON carries `reviewOrder: 12` as a plain number |
| d | answered a card (v3), edited the special-field note, set `collapseTime`, renamed the override deck, saved the note type unchanged | none | deck-level limits and desired retention survive the rename. Both cards of the special-field note survive the edit. The moved card's FSRS state is untouched |
| e1 | Check Database | none | report "Database rebuilt and optimized." |
| e2 | Empty Cards, then delete as the dialog does ("keep notes") | the `{{#Tags}}` card is **deleted** | the pinned core's Empty Cards lists nothing. The control (26.08.1) deletes nothing |
| e3 | filtered deck proto round trip, legacy saves of a filtered and a normal deck, rebuild | none | order 10 is kept, but 25.09.4 rebuilt using its default order (see item 3) |
| e4 | filtered deck saved as 25.09.4's dialog would | order 10 becomes **-1** | the dialog's order list has 10 entries (0–9). Qt 6.9's `QComboBox.setCurrentIndex(10)` gives `currentIndex() == -1` (checked once with the launcher's PyQt6, offscreen, outside the driver) |
| e5 | deck options: desired retention 0.88, FSRS w0 0.3→0.35 (recomputes memory states) | none | desired retention and stability values are identical to the pinned core doing the same edit (18 values, max relative difference 0). The deck-level 0.93 override is kept |

Outcome per inventory item:

| Item | a | b | c | d | e1 | e2 | e3 | e4 | e5 |
|---|---|---|---|---|---|---|---|---|---|
| 1 review order 12 (both presets) | kept | kept | kept | kept | kept | kept | kept | kept | kept |
| 3 filtered order 10 | kept | kept | kept | kept | kept | kept | kept (ignored by the rebuild) | **reset** (-1, treated as order 0) | kept |
| 5 special-field card | kept | kept | kept | kept (edit, note type save) | kept | **lost** (deleted) | kept | kept | kept |
| 6 `experimentalFeatures` | kept | kept | kept | kept | kept | kept | kept | kept | kept |
| control: custom data, card data JSON, deck-level overrides, FSRS params, other preset fields | kept | kept | kept | kept | kept | kept | kept | kept | kept (deliberate edits only) |

Items 2 and 4 are meaning changes. They involve no stored difference, so they
cannot be kept or lost; they mean something different in each program.

How 25.09.4 *uses* the unknown values while it has the file [code, not run
through the UI]:

- **Review order 12.** The prost getter `review_order()` maps the unknown value
  to the default, `DAY`, so 25.09.4 schedules such a deck as "Due date, then
  random". The stored value stays 12 (25.09.4's Python reads back 12 through
  both the proto and the legacy API [spike]).
- **25.09.4's deck options screen.** `EnumSelector.svelte` finds no label for 12
  and shows an empty selection. It sends the value back unchanged unless the
  user picks another order.
- **Filtered order 10.** `term.order()` maps it to `OLDEST_REVIEWED_FIRST`. In
  e3, 25.09.4's rebuild ordered the cards differently from the pinned core. The
  test prints which cards were picked but does not assert it, because ties are
  broken by a hash of id and mtime.

## 3. Consequences for a user alternating between the two programs

- **Nothing is silently dropped by opening, studying, editing, renaming,
  saving presets (either API), Check Database or saving note types in
  25.09.4.** No action tested forces a full sync.
- **Desktop 25.09.4 Tools > Empty Cards deletes cards that the newer core
  created for special-field templates.** The deletion is recorded as a grave,
  so it propagates by sync. The card's scheduling state goes with it; review
  log rows stay, as for any deletion [code]. The newer core creates such cards
  again the next time the note is edited or its note type is saved [code], so
  the cards can come and go as the user switches programs.
- **A preset set to "Relative overdueness" in tanki is studied in "Due date,
  then random" order in 25.09.4.** The preset keeps the setting, and tanki
  honours it again.
- **The retrievability sort orders differ between the two programs** for the
  same preset (relative versus absolute retrievability; and without FSRS,
  overdueness versus effectively unsorted).
- **Saving a filtered deck in 25.09.4's dialog without touching the order
  resets "Relative overdueness" to -1**, which both programs treat as "Oldest
  seen first". The user sees an empty order box in 25.09.4. Rebuilding it in
  25.09.4 uses that order anyway.
- **Answering the same kind of card can schedule differently** with default
  FSRS parameters (short-term steps), and moved cards get an FSRS state sooner
  in tanki. Both are behaviour differences, not data loss.

## 4. Recommendation for tanki

- **No refusal and no guard on open are needed.** The file round-trips
  cleanly. Keep the planned one-time notice, and make it factual:

  > Anki 25.09.4 can open this collection safely, but until you update it to
  > 26.08 or later:
  > - Do not run *Tools > Empty Cards* in it. It deletes cards that newer
  >   versions create from templates using `{{#Tags}}`, `{{#Deck}}` and
  >   similar fields.
  > - Decks set to the "Relative overdueness" review order are studied by due
  >   date there.
  > - Saving a filtered deck that uses "Relative overdueness" in its dialog
  >   loses that order.
  > - The "Retrievability" sort orders mean different things in the two
  >   versions.

- **Nothing needs re-applying.** Neither stored value that 25.09.4 cannot
  interpret is rewritten by any of its save paths except the filtered deck
  dialog.
- **Optional, cheap:**
  - tanki's filtered deck editor can show an out-of-range order such as -1 as
    "Oldest seen first (reset by an older Anki)" instead of failing.
  - Before tanki offers its own Empty Cards, it needs nothing special: the
    pinned core reports the special-field cards as non-empty.
- **Upstream (open question):** check whether a filtered deck using a
  "Retrievability" order with FSRS off fails to build in the pinned core
  (item 4).

## 5. Limitations

- The Svelte deck options screen, the Qt filtered deck dialog and the
  reviewer's custom-scheduling JavaScript were not run. Their behaviour comes
  from code reading plus the one-off `QComboBox` check.
- `ls = 0` in every fixture, so "would this force a full sync" is answered by
  comparing `scm` before and after, not by a sync.
- The fixture's review histories are short, so the identical FSRS recompute
  result does not exclude small numeric differences on long histories (the
  FSRS crate changed major version).
- Only the actions listed were tested. Import/export, change note type,
  optimise and reschedule were not run in 25.09.4.

## Summary for the plan's results table

```
Spike D (2026-11-13): 25.09.4 opens, edits and closes a pinned-core (26.08.1) collection
cleanly: schema 18, scm/ls unchanged (no full sync), pinned core reopens; untouched cards'
due/interval/FSRS state identical. The newer core adds no stored field or key, only
new enum values: review order 12 and filtered order 10 (relative overdueness).
Both survive every 25.09.4 save path tested, except the filtered deck dialog
(stores -1, read as "oldest seen first"). 25.09.4 schedules review order 12 as
"due date". Retrievability sort orders 7/11 and 8/9 changed meaning. 25.09.4's
Empty Cards deletes cards the newer core generates for {{#Tags}}-style templates.
No guard needed; show a factual one-time notice (spike-d-version-skew.md section 4).
```
