# 02 — Study/review experience and the scheduler (Anki 26.08.1)

Source tree: `/home/emard/projects/open-source/anki` (read-only). All paths below are relative to it.
Line numbers refer to that checkout. "rslib" = Rust crate `anki` (`rslib/src`), "pylib" = `pylib/anki`,
"aqt" = `qt/aqt`, "ts" = `ts/`.

Verdict key: **trivial** / **doable** / **hard** / **impossible** (in a terminal).

---

## 0. TL;DR for implementers

* **The scheduler is entirely in rslib.** Queue building, counts, learn-ahead, day rollover, burying,
  limits, answer state transitions, FSRS, the load balancer, easy days, fuzz, leech handling, filtered decks,
  custom study, set-due-date, forget, reposition, grade-now: all of it is in Rust. pylib's `v3.py` is a thin
  wrapper. aqt's `reviewer.py` is the **state machine and UI glue** (about 1 250 lines) that tanki has to
  rewrite: show question, show answer, timers, auto-advance, the type-in-answer field, the menu, shortcuts,
  timebox, leech tooltip, audio playback, custom-scheduling JS.
* What a frontend **must** keep itself: the card's start time (for `milliseconds_taken`), the
  `SchedulingStates` it got with the card (possibly changed by custom JS), the reviewer phase
  (question/answer/transition), the timebox start time and rep count, the auto-advance timers, and the
  recorded-voice path.
* The study loop is three RPCs: `GetQueuedCards` → (render) → `DescribeNextStates` → `AnswerCard`. Undo is
  `Undo`/`Redo` on the collection service. The queue cache is kept inside `Collection.state.card_queues`.
* **Two ways to call it from Rust:** (a) the inherent `Collection` methods (`get_queued_cards(usize,bool)`,
  `answer_card(&mut CardAnswer)`, `describe_next_states(&SchedulingStates)`, …), which use Rust domain types.
  Some return types (`QueuedCards`, `QueuedCard`) are `pub struct`s inside the `pub(crate) mod queue`, so
  you can use them but **cannot name** them in your own signatures. (b) The generated
  `anki::services::SchedulerService` trait, implemented for `Collection` with proto types. Call it as
  `SchedulerService::get_queued_cards(&mut col, req)`, because the inherent methods with the same names
  shadow it. **Recommendation:** use the proto/service layer for everything that crosses the
  scheduler/UI boundary. The proto types are stable, they are what the desktop uses, and they are complete.
  Note `CardState::leeched()` is `pub(crate)`; use `state_is_leech` or read `ReviewState.leeched`.
* **v1/v2 schedulers are not supported for studying.** v3 is the only path. rslib itself does not check the
  `sched2021` flag; the UI does.
* **Custom scheduling JS** is a one-line text field. It runs in the reviewer webview and changes the
  `SchedulingStates` proto. Without a JS engine you lose it silently. **QuickJS/Boa embedding is
  feasible**, because the input/output is plain JSON (protobuf JSON mapping of `SchedulingStates` +
  `customData` + `SchedulingContext`).

---

## 1. Review loop end-to-end

### 1.1 Screens and state machine (aqt)

`qt/aqt/main.py` moves between states `deckBrowser` → `overview` → `review` (`mw.moveToState`).

| Screen | File | Data source (RPC) | Where logic lives |
|---|---|---|---|
| Deck browser | `qt/aqt/deckbrowser.py` | `DeckTree{now}` (`col.sched.deck_due_tree()` → `pylib/anki/scheduler/base.py:76-82`), `StudiedToday`, `v3_scheduler()` | tree+counts: rslib `decks/tree.rs:252`; HTML: aqt |
| Overview | `qt/aqt/overview.py` | `GetQueuedCards` counts (`col.sched.counts()`), `DeckTree` subtree, `CongratsInfo` | aqt HTML, rslib data |
| Congrats ("finished") | `ts/routes/congrats/CongratsPage.svelte` loaded by `overview.py:208-209` | `CongratsInfo` (polled every 60 s, `CongratsPage.svelte:33-44`) | rslib `scheduler/congrats.rs:17` |
| Reviewer | `qt/aqt/reviewer.py` + `ts/reviewer/index.ts` + `qt/aqt/data/web/js/reviewer-bottom.ts` | `GetQueuedCards`, `DescribeNextStates`, `AnswerCard`, `StateIsLeech`, `RenderExistingCard`, `CompareAnswer` | aqt state machine; rslib scheduling; ts rendering |

Reviewer phases: `self.state: Literal["question","answer","transition"]` (`reviewer.py:159`).
`"transition"` is set while `AnswerCard` runs in the background (`reviewer.py:560`); key presses in that phase are ignored
(`_answerCard` checks `state != "answer"` → return, `reviewer.py:536-537`).

### 1.2 Flow, step by step (desktop)

1. **Overview "Study Now"** (`overview.py:97-101`): `col.startTimebox()` (pylib `collection.py:1232`, which stores the
   wall-clock time and `sched.reps`), then `moveToState("review")`.
2. **`Reviewer.show()`** (`reviewer.py:174-185`): if `sched_ver()==1 or not v3_scheduler()` → back to the deck browser and show
   `scheduling_update_required`. Installs shortcuts, bridge handlers, reads `cardStateCustomizer` config
   (custom JS), sets refresh=QUEUES.
3. **`nextCard()`** (`reviewer.py:247-263`) → `_get_next_v3_card()` (`:265-272`): `col.sched.get_queued_cards()`
   (fetch_limit=1) → `V3CardInfo.from_queue` (`:96-103`), which **copies `card.custom_data` into
   `states.current.custom_data`** (the backend leaves it empty, `scheduler.proto:128-131`). Builds a pylib `Card`
   from the proto card and calls `card.start_timer()` (`pylib/anki/cards.py:191`, `time.time()`).
   No cards → `moveToState("overview")`, and the overview shows the congrats page because `_is_finished()`.
4. **`_showQuestion()`** (`reviewer.py:370-405`): renders `c.question()` (rslib `RenderExistingCard`), autoplays
   question AV tags if `autoplay` (deck option `disable_autoplay` inverted), runs the type-answer filter, **starts the custom
   scheduling JS** (`_run_state_mutation_hook`, `:286-297`), evals `_showQuestion(q, a, bodyclass)` in the webview,
   draws the flag/mark, draws the "Show Answer" button with remaining counts (`_showAnswerButton`, `:841-857`), starts the
   auto-advance timer if enabled.
5. **Show answer** (Space/Enter or click → `onEnterKey` `:657-663` → `_getTypedAnswer` → JS `getTypedAnswer()` →
   `_onTypedAnswer` → `_showAnswer` `:460-484`): renders the answer, autoplays answer audio, does the type-answer comparison,
   `_showEaseButtons()` (`:859-867`). **That function waits (polls every 50 ms) until the custom-scheduling JS has
   finished** (`_states_mutated`), then builds the buttons with `describe_next_states(self._v3.states)`.
6. **Answer** (`_answerCard(ease)` `:531-563`): `gui_hooks.reviewer_will_answer_card` → `sched.build_answer(card,
   states, rating)` (pylib `v3.py:66-92`) → background `answer_card` op → `after_answer`: if
   `state_is_leech(answer.new_state)` show the leech tooltip; `_after_answering` → `check_timebox()` → `nextCard()`.

### 1.3 Exact data contract

#### What the frontend receives: `GetQueuedCards`

```proto
// proto/anki/scheduler.proto:134-156
message GetQueuedCardsRequest { uint32 fetch_limit = 1; bool intraday_learning_only = 2; }
message QueuedCards {
  enum Queue { NEW = 0; LEARNING = 1; REVIEW = 2; }
  message QueuedCard {
    cards.Card card = 1;            // full card row (incl. custom_data, flags, memory_state…)
    Queue queue = 2;                // which count to underline
    SchedulingStates states = 3;    // current + again/hard/good/easy
    SchedulingContext context = 4;  // deck_name, seed, decay, desired_retention (for custom JS)
  }
  repeated QueuedCard cards = 1;
  uint32 new_count = 2; uint32 learning_count = 3; uint32 review_count = 4;
}
message SchedulingContext { string deck_name = 1; uint64 seed = 2; optional float decay = 3; optional float desired_retention = 4; }
```

Rust: `Collection::get_queued_cards(&mut self, fetch_limit: usize, intraday_learning_only: bool) -> Result<QueuedCards>`
(`rslib/src/scheduler/queue/mod.rs:90-138`). Every call fetches the card, **checks `card.mtime == entry.mtime`
(else error "card modified without updating queue")**, and computes next states with `get_scheduling_states`.
It is **idempotent**: it does not pop anything. Cards are popped only by `AnswerCard`.

```proto
// proto/anki/scheduler.proto:80-132, 264-270
message SchedulingState {
  message New { uint32 position = 1; }
  message Learning { uint32 remaining_steps = 1; uint32 scheduled_secs = 2; uint32 elapsed_secs = 3; optional cards.FsrsMemoryState memory_state = 6; }
  message Review { uint32 scheduled_days = 1; uint32 elapsed_days = 2; float ease_factor = 3; uint32 lapses = 4; bool leeched = 5; optional cards.FsrsMemoryState memory_state = 6; }
  message Relearning { Review review = 1; Learning learning = 2; }
  message Normal { oneof kind { New new = 1; Learning learning = 2; Review review = 3; Relearning relearning = 4; } }
  message Preview { uint32 scheduled_secs = 1; bool finished = 2; }
  message ReschedulingFilter { Normal original_state = 1; }
  message Filtered { oneof kind { Preview preview = 1; ReschedulingFilter rescheduling = 2; } }
  oneof kind { Normal normal = 1; Filtered filtered = 2; }
  optional string custom_data = 3;   // NOT filled by backend; frontend copies card.custom_data into current
}
message SchedulingStates { SchedulingState current = 1; again = 2; hard = 3; good = 4; easy = 5; }
```

Rust equivalents: `rslib/src/scheduler/states/mod.rs:33-37` (`CardState`), `:157-164` (`SchedulingStates`, all fields
`pub`), state structs `NewState{position}`, `LearnState{remaining_steps, scheduled_secs, elapsed_secs, memory_state}`,
`ReviewState{scheduled_days, elapsed_days, ease_factor, lapses, leeched, memory_state}`,
`RelearnState{learning, review}`, `PreviewState{scheduled_secs, finished}`,
`ReschedulingFilterState{original_state}`.

#### What the frontend sends back: `AnswerCard`

```proto
// proto/anki/scheduler.proto:272-286
message CardAnswer {
  enum Rating { AGAIN = 0; HARD = 1; GOOD = 2; EASY = 3; }
  int64 card_id = 1;
  SchedulingState current_state = 2;   // states.current exactly as received (+custom_data)
  SchedulingState new_state = 3;       // states.again|hard|good|easy (possibly mutated by custom JS; its custom_data is persisted)
  Rating rating = 4;
  int64 answered_at_millis = 5;        // wall-clock now, ms
  uint32 milliseconds_taken = 6;       // time since question shown, UNcapped (backend caps)
}
```

pylib builds it in `v3.py:66-92`: `milliseconds_taken=card.time_taken(capped=False)` and
`answered_at_millis=int_time(1000)`. Rust: `scheduler/answering/mod.rs:48-57`:

```rust
pub struct CardAnswer {
    pub card_id: CardId, pub current_state: CardState, pub new_state: CardState,
    pub rating: Rating, pub answered_at: TimestampMillis, pub milliseconds_taken: u32,
    pub custom_data: Option<String>, pub from_queue: bool,
}
pub fn answer_card(&mut self, answer: &mut CardAnswer) -> Result<OpOutput<()>>   // answering/mod.rs:311
```

The proto → Rust conversion (`scheduler/service/answering.rs:12-27`) takes `new_state.custom_data` as
`custom_data` and sets `from_queue: true`.

What `answer_card_inner` (`answering/mod.rs:315-388`) does:
* caps `milliseconds_taken` to `cap_answer_time_to_secs*1000` (deck option, default 60 s) (`:324`, `:60-62`);
* recomputes the current state and requires it to equal `answer.current_state`, after equalising `elapsed_secs`
  for (re)learning (`:325-333`, `:577-604`). **If the card changed under you, you get an error.** Always send back the
  `current` you received;
* applies the state, writes the revlog (`add_partial_revlog`, `:398-413`), updates the deck's daily studied counters
  (`update_deck_stats_from_answer` `:415-439`: new counter +1 if it came from the New queue, review counter +1 if from
  Review/DayLearn; learning cards do not count against limits), buries siblings per deck config (`:390-396`), sets
  `last_review_time` (except preview), stores custom_data (validated), adds the `leech` tag if `new_state.leeched()`,
  suspends if leech action = Suspend (`:194-196`), feeds the load balancer, and **pops the card from the cached queue**
  (`update_queues_after_answering_card`, `queue/mod.rs:217-243`), which requeues intraday learning cards.

Leech notification: the backend does not report "it was suspended". aqt checks `card.queue < 0` after reload and
`state_is_leech(answer.new_state)` (`reviewer.py:551-558`, `StateIsLeech` RPC → `CardState::leeched()`).
Tooltip text: `studying_card_was_a_leech` + `studying_it_has_been_suspended` (`reviewer.py:956-961`).

#### Answer-button labels: `DescribeNextStates`

```rust
pub fn describe_next_states(&mut self, choices: &SchedulingStates) -> Result<Vec<String>>   // answering/mod.rs:263
```
For each of again/hard/good/easy: `interval_kind().maybe_as_days(secs_until_rollover)` converts second-based intervals
that cross the rollover into days (`states/interval_kind.rs:12-23`). Then
`answer_button_time_collapsible(secs, learn_ahead_secs, tr)` (`timespan.rs:23-32`) gives `"End"` when 0 (preview
finished), `"<10m"` when below the learn-ahead limit, otherwise `"10m"`, `"1d"`, `"3.5mo"`… These strings are
**localised by rslib i18n**, so they are terminal-ready. Labels are shown only if the `estTimes` config bool is on
(`BoolKey::ShowIntervalsAboveAnswerButtons`, default true; `reviewer.py:946-951`).

Button count: `col.sched.answerButtons(card)` (pylib legacy, `reviewer.py:887-910`). In v3 it is always 4 (2 and 3
exist only for legacy add-ons). Default button for Enter/Space = Good (3) (`_defaultEase` `:884`). The configurable
answer keys come from the **profile** (`pm.get_answer_key`, default "1".."4"), not the collection.

#### Counts and which one is underlined

`QueuedCards.{new,learning,review}_count`. The underlined index follows `QueuedCard.queue`: NEW→0, LEARNING→1,
REVIEW→2 (`reviewer.py:108-122`, `_remaining()` `:869-882`). Shown only if the `dueCounts` config bool is set
(`BoolKey::ShowRemainingDueCountsInStudy`, default true). That is the "show remaining count" preference.
Interday learning (DayLearn queue) cards are counted as **learning** and queued as `LEARNING`, but they live in the
main queue (`queue/main.rs:14-19`).

Counting quirk: the learning count includes intraday cards due within `now + learn_ahead_secs` at build time,
plus all interday learning cards (`queue/builder/mod.rs:194-197`). `CardQueues::counts()` refreshes the learning
cutoff only when all counts are zero (`queue/mod.rs:190-196`).

#### Queue order, learn-ahead, intraday learning

* Built lazily in `get_queues()` (`queue/mod.rs:246-254`) for the **current deck** (`curDeck` config) and its children
  (and parents if "apply all parent limits"). `QueueBuilder::new/gather_cards/build` (`queue/builder/mod.rs:130-229`,
  `gathering.rs:14-21`). Gather order: intraday learning → interday learning (review limit) → reviews (review limit) →
  new cards (new limit). Burying of siblings at gather time is in `burying.rs:75-93`.
* Presentation order (`CardQueues::iter`, `queue/mod.rs:156-161`): **intraday learning due now** → **main queue**
  (reviews, interday learning and new cards mixed per `interday_learning_mix` and `new_mix`, `builder/mod.rs:251-278`) →
  **intraday learning due within learn-ahead** ("learn ahead": shown only when nothing else is left,
  `queue/learning.rs:40-47`).
* Learn-ahead limit = collection config `collapseTime`, default 1200 s (`config/mod.rs:233-236`), set in
  Preferences (`Preferences.Scheduling.learn_ahead_secs`).
* Requeue after answering: an intraday learning card due before the next rollover goes back into the sorted learning queue. If
  it would be shown again at once while the main queue is empty, it is placed 1 s after the next learning card
  (`queue/learning.rs:72-126`). Learning cards are sorted by (has been reviewed before, due)
  (`cmp_by_reps_then_due`, `:22-28`).
* `intraday_learning_only=true` returns only learning cards (used by AnkiDroid-style "learning only" fetches).
* The cache is cleared after any op that `requires_study_queue_rebuild()` other than AnswerCard
  (`queue/mod.rs:211-215`). Undo of an answer restores the queue entry (`queue/undo.rs`).

#### Day rollover and timezone

* `SchedTimingToday { now, days_elapsed, next_day_at }` (`scheduler/timing.rs:12-19`). The v2 "new timezone" computation
  (`sched_timing_today_v2_new`, `:27-55`) uses collection creation time + creation UTC offset + the **current local UTC
  offset** + the rollover hour (`rollover` config, default 4, `Preferences.Scheduling.rollover`).
* `local_utc_offset_for_user()` (`scheduler/mod.rs:90-107`): on a client, **if the OS timezone differs from the stored
  `localOffset` config it rewrites the config**. The terminal process timezone (`TZ` env) therefore affects the scheduling
  day. Be careful when running tanki under a different TZ than the desktop (for example over SSH).
* `scheduler_info()` caches the timing until `next_day_at` (`scheduler/mod.rs:35-48`).
* Rollover handling: `get_queues()` → `clear_queues_if_day_changed()` discards the queue and undo, and unburies
  (`queue/mod.rs:277-290`). `SchedTimingToday` RPC and `DeckTree{now}` also unbury when the day changed
  (`service/mod.rs:34-40`, `bury_and_suspend.rs:32-50`). **A long-running TUI must periodically re-query** (for example on every
  `get_queued_cards`, which already handles it, and on a timer for the deck list after midnight/4 am).

#### Congrats screen data: `CongratsInfo`

```proto
// scheduler.proto:186-196
message CongratsInfoResponse {
  uint32 learn_remaining = 1; uint32 secs_until_next_learn = 2;  // 86400 => none later today
  bool review_remaining = 3; bool new_remaining = 4;              // limits reached but cards exist
  bool have_sched_buried = 5; bool have_user_buried = 6;
  bool is_filtered_deck = 7; bool bridge_commands_supported = 8; string deck_description = 9; // rendered HTML
}
```
`scheduler/congrats.rs:17-41`, SQL `storage/card/congrats.sql`. `secs_until_next_learn` is already reduced by learn-ahead and is at least
60. Page logic (`ts/routes/congrats/lib.ts`, `CongratsPage.svelte`): "Congratulations! You have finished this deck for
now."; next-learning message if `< 86400`; "today's review/new limit reached" lines; an "unbury them" link if buried; a "custom
study" link unless filtered; deck description. Re-poll every 60 s so it switches back when learning cards come due.

#### Overview counts and description (`overview.py:182-275`)

* Counts = `sched.counts()` (= `GetQueuedCards` counts). Buried difference per row = `DeckTree` node count − queue count
  (`:233-240`), shown as `+N` with tooltip `studying_counts_differ`.
* Description: deck `desc` (HTML; or markdown if the `md` flag → `RenderMarkdown`). For filtered decks a fixed explanatory
  text is shown instead.
* Bottom bar: Options (o), Rebuild (r) / Empty (e) for filtered decks, Custom Study (c) for normal decks, Unbury (u) if
  `have_buried()`, Description (edit) for normal decks (`_renderBottom` `:283-314`, shortcuts `:126-133`).
* Unbury chooser when both kinds exist (`on_unbury` `:152-175`): Manually buried / Buried siblings / All →
  `UnburyDeck{mode: USER_ONLY|SCHED_ONLY|ALL}`.

#### Deck tree with counts: `DeckTree{now}`

```proto
// decks.proto:157-183, DeckTreeRequest{int64 now}: 0 = no counts
message DeckTreeNode {
  int64 deck_id = 1; string name = 2; uint32 level = 4; bool collapsed = 5;
  uint32 review_count = 6; uint32 learn_count = 7; uint32 new_count = 8;      // with children + limits
  uint32 intraday_learning = 9; uint32 interday_learning_uncapped = 10;
  uint32 new_uncapped = 11; uint32 review_uncapped = 12; uint32 total_in_deck = 13;  // own deck, no limits
  uint32 total_including_children = 14; bool filtered = 16;
  repeated DeckTreeNode children = 3;
}
```
`Collection::deck_tree(Option<TimestampSecs>)` (`decks/tree.rs:252-290`). The root node is a dummy; render
`top.children`. An empty Default deck is hidden. Collapse state: `SetDeckCollapsed{deck_id, collapsed, scope: REVIEWER}`.
`collapsed` is the study-screen flag when counts are requested.
Deck browser actions (`deckbrowser.py:105-138`): open (sets current deck → overview), opts menu (Rename / Options / Export /
Delete), collapse, drag-reparent, create deck, import, shared decks, shift-click select, plus the v3 upgrade banner.
"Studied N cards in X minutes today" = `StudiedToday` RPC (localised string).

#### Study limits

`decks/limits.rs`. Effective limit per deck: today-only override (`review_limit_today`/`new_limit_today`, a `DayLimit{limit,
today}` valid only when `today == days_elapsed`) → else deck-specific override (`review_limit`/`new_limit`) → else the preset
(`reviews_per_day`/`new_per_day`) (`:25-51`). Remaining = limit − studied today (`Deck.common.{new,review}_studied`
for `last_day_studied`). Unless `newCardsIgnoreReviewLimit`, **new cards studied also use up the review limit, and the new limit is capped
by the remaining review limit** (`:91-116`). Interday learning cards use the review limit (`gathering.rs:35-60` uses
`LimitKind::Review` for both). Parent limits cap children in the tree (`LimitTreeMap`). With `applyAllParentLimits`,
the limits of the current deck's ancestors also apply (`builder/mod.rs:136-142`). Custom study "increase today's limit" =
`ExtendLimits`/`custom_study(new_limit_delta)`, which *decrements the studied counters* (`decks/stats.rs:48`).

Verdicts for section 1: deck list with counts **trivial**; overview **trivial**; congrats **trivial**; queue/answer loop
**trivial** (everything in rslib); timezone/rollover **doable** (just keep re-querying; mind the TZ env).

---
## 2. Reviewer features and keyboard shortcuts

Source: `Reviewer._shortcutKeys()` `qt/aqt/reviewer.py:594-641`, Korean-layout aliases `:574-592`,
context ("More") menu `_contextMenu()` `:991-1049`, bottom bar `_bottomHTML()` `:810-839`.
Global main-window shortcuts also apply (Undo Ctrl+Z / Redo Ctrl+Shift+Z from `qt/aqt/forms/main.ui:154,252`; Edit menu).

| Key | Action | Implementation / backend | Logic lives | Terminal verdict + approach |
|---|---|---|---|---|
| Space / Enter / Return | Show answer; on the answer side rate with the focused or default button (Good) if `spacebar_rates_card` (profile pref) | `onEnterKey` `:657-671` | aqt | **trivial** |
| 1 2 3 4 (configurable per profile: `pm.get_answer_key`) | Again/Hard/Good/Easy (only in the answer phase) | `_answerCard` `:531-563` → `AnswerCard` | rslib | **trivial** |
| e | Edit current note | `mw.onEditCurrent` (editor window) | aqt+ts editor | **hard** (needs a terminal note editor, see the editor research); a minimal field editor or `$EDITOR` is doable |
| m | Show "More" context menu | `showContextMenu` | aqt | **trivial** (popup list) |
| r, F5 | Replay audio (q side; a side = answer tags, plus question tags unless `skip_question_when_replaying_answer`) | `replay_audio` `:74-81`, `Card.replay_question_audio_on_answer_side` (pylib `cards.py:206`, legacy key `replayq`) | aqt+pylib (AV tags from rslib render) | **doable**: spawn `mpv --no-video` for `[sound:]`; TTS tags need a TTS engine (espeak/say) |
| 5 | Pause/resume audio | `av_player.toggle_pause` | aqt | **doable** with mpv IPC (`--input-ipc-server`) |
| 6 / 7 | Seek −5 s / +5 s | `av_player.seek_relative` | aqt | **doable** (mpv IPC) |
| v | Replay own recording | `onReplayRecorded` `:1195-1202` | aqt | **doable** |
| Shift+V | Record own voice | `record_audio` (Qt `QAudioSource` → wav → optionally lame mp3, `qt/aqt/sound.py:634-866`) | aqt | **doable** via `arecord`/`pw-record`/`sox`/`ffmpeg` subprocess or `cpal`; UI = "recording… press key to stop" |
| Ctrl+1..7 | Toggle flag 1-7 (same flag again → 0) | `set_flag_on_current_card` `:1092-1101` → `SetUserFlagForCards` | rslib | **trivial**; flag names come from `mw.flags` (`qt/aqt/flags.py`; custom names in collection config JSON `flagLabels`, a dict index→label, default labels Red/Orange/Green/Blue/Pink/Turquoise/Purple) |
| * | Toggle "marked" tag on note | add/remove tag `marked` `:1106-1121` | rslib | **trivial** |
| = | Bury note | `BuryOrSuspendCards{note_ids, BURY_USER}` | rslib | **trivial** |
| - | Bury card | `BuryOrSuspendCards{card_ids, BURY_USER}` | rslib | **trivial** |
| ! | Suspend note | `BuryOrSuspendCards{note_ids, SUSPEND}` | rslib | **trivial** |
| @ | Suspend card | `BuryOrSuspendCards{card_ids, SUSPEND}` | rslib | **trivial** |
| Ctrl+Alt+N | Forget / reset card (dialog: restore original position, reset rep+lapse counts; defaults per context REVIEWER) | `forget_cards` (`qt/aqt/operations/scheduling.py:95-131`) → `ScheduleCardsAsNew{context: REVIEWER}`; defaults `ScheduleCardsAsNewDefaults` | rslib | **trivial** (2 checkboxes) |
| Ctrl+Alt+E | Create copy (open Add dialog prefilled) | `on_create_copy` `:1174-1178` | aqt | **hard/doable** (depends on the Add screen) |
| Ctrl+Delete (mac Ctrl+Backspace) | Delete note | `RemoveNotes` | rslib | **trivial** (confirm prompt recommended) |
| Ctrl+Shift+D | Set due date (text prompt; default remembered in `setDueReviewer` config, default "1") | `set_due_date_dialog` → `SetDueDate{days, config_key: SET_DUE_REVIEWER}` | rslib parsing | **trivial** |
| o | Deck options of the card's deck (asks which deck if it's in a filtered deck) | `confirm_deck_then_display_options` | ts deck-options | **hard** (full screen to rebuild, see section 4) |
| i | Card info (current) | `ReviewerCardInfo` → ts `card-info` page (`CardStats` RPC) | rslib data, ts UI | **doable** (table + revlog list) |
| Ctrl+Alt+I | Card info of previous card | `PreviousReviewerCardInfo` | same | **doable** |
| u | Undo | `mw.undo` → `Undo` RPC (`OpChangesAfterUndo`) | rslib | **trivial**; after undo, re-fetch the queue. The undone card is back on top (`queue/undo.rs`) |
| (menu) Ctrl+Shift+Z | Redo | `Redo` RPC | rslib | **trivial** (not in the reviewer shortcut list, only the main menu) |
| Shift+A | Toggle auto-advance | `toggle_auto_advance` `:1212-1218` | aqt | **trivial** (timers in the event loop) |
| click `play:q:N` / `play:a:N` | Play one AV tag | `play_clicked_audio` (`sound.py:924-932`) | aqt | **doable**: number the audio refs, `p1`.. or a mouse click on the glyph |
| Korean aliases (ㄷ ㅡ ㄱ ㅍ …) | same as e/m/r/v/o/i/u | `:574-592` | aqt | **trivial** if you map keycodes by physical key; with crossterm you get chars. Optional |

There is **no shortcut** for "grade now", "reposition", "reschedule" in the reviewer (those are browser actions).

Feature notes:

* **Timer display** (`reviewer-bottom.ts`): bottom-right "m:ss", counting from 0 each question; capped at `maxTime`
  (= `cap_answer_time_to_secs`, only if `show_timer`), shown in red when the cap is reached. If `stop_timer_on_answer`, it stops on the
  answer side (`reviewer.py:841-867`). Note the timer shown in the UI is cosmetic; the real `milliseconds_taken` is
  `time.time() - timer_started` measured in Python (`cards.py:215-221`) and capped by rslib. **trivial**.
* **Timebox** (`check_timebox` `reviewer.py:966-985`, pylib `collection.py:1232-1244`): collection config
  `timeLim` seconds (Preferences "Timebox time limit", `Preferences.Reviewing.time_limit_secs`). After each answer, if
  elapsed > timeLim: a modal "N cards studied in M minutes" with Continue / Finish (Finish → deck browser; Continue →
  restart timebox). **Pure frontend state** (start time + rep counter). **trivial**.
* **Auto advance** (`reviewer.py:407-526`): per-preset `seconds_to_show_question` (after this, `question_action`: 0=show
  answer, 1=show reminder tooltip) and `seconds_to_show_answer` (then `answer_action`: 0=bury card, 1=Again, 2=Good, 3=Hard,
  4=show reminder; **enum order in Python `AnswerAction` matches the proto: BURY_CARD=0, AGAIN=1, GOOD=2, HARD=3, REMINDER=4**).
  `wait_for_audio`: if audio is still playing when the timer fires, do nothing; the `av_player_did_end_playing` hook re-fires the
  handler (`:309-324`). Auto-advance switches itself off when the main window loses focus (`:427-433`). It is off by default
  per session; toggle with Shift+A. **trivial/doable** (needs an audio-finished notification from the mpv subprocess).
* **Type-in-the-answer** (`reviewer.py:698-805`): a regex over `[[type:Field]]`, `[[type:cloze:Field]]`,
  `[[type:nc:Field]]` (nc = no combining, ignore diacritics). The question side replaces it with an `<input id=typeans>`;
  the answer side calls `CompareAnswer{expected, provided, combining}` (rslib `typeanswer.rs:38`) which returns **HTML**:
  `<code id=typeans><span class=typeGood|typeBad|typeMissed>…</span><br><span id=typearrow>&darr;</span><br>…</code>`.
  Cloze expected text: `ExtractClozeForTyping{text, ordinal}`. Font/size come from the note type field config.
  **doable**: inline line-editor on the question side; map the `typeGood/Bad/Missed` spans to green/red/grey styles. Note that
  this parsing is done in **aqt**, not rslib: the `[[type:…]]` marker survives template rendering and the frontend must
  substitute it.
* **Leech**: tooltip only (see 1.3). The leech tag and suspension are done by rslib. **trivial**.
* **"Show next review time above answer buttons"** = `estTimes` bool; **"show remaining card count"** = `dueCounts` bool.
  Both are collection config (Preferences → Review). **trivial**.
* **Interrupt audio when answering**: `interruptAudioWhenAnswering` bool, default true. It controls whether starting
  new playback stops current audio (`main.py:748-752`). **doable**.
* **Hide audio play buttons**: `hideAudioPlayButtons` bool (`main.py:634-639`). **trivial**.
* **Autoplay**: deck option `disable_autoplay`. The question side plays `question_av_tags` in order. The answer side plays
  `answer_av_tags` only. Question audio coming in through `{{FrontSide}}` is **not** replayed on reveal: pylib
  (`pylib/anki/template.py:236-241`) runs `ExtractAvTags` on the question first, and the already-converted question text
  (with `[anki:play:q:N]` refs) is injected as FrontSide, so those refs stay question-side. The replay key (r) prepends question
  tags unless `skip_question_when_replaying_answer`.
* **Flag/mark indicators**: `#_flag` (★/⚑ glyphs coloured `--flag-N`) and `#_mark` (`revHtml` `:329-340`). **trivial**.
* **Bottom bar**: [Edit] … [Show Answer + counts] … [More ▾ + timer]; answer side: 2-4 buttons with interval labels. **trivial**.
* **Hooks** (`gui_hooks.reviewer_*`): add-on API only. Out of scope.

---

## 3. Custom scheduling JS ("card state customizer")

### 3.1 How it works

* Stored in collection config string `cardStateCustomizer` (`StringKey::CardStateCustomizer`,
  `rslib/src/config/string.rs:14`). It is **global (whole collection)**, edited in Deck Options → Advanced → "Custom scheduling"
  (`ts/routes/deck-options/AdvancedOptions.svelte:330-334`, `CardStateCustomizer.svelte`), saved through
  `UpdateDeckConfigsRequest.card_state_customizer` (`deck_config.proto:258`, written in `deckconfig/update.rs` at the end of
  `update_deck_configs_inner`). Read in `Reviewer.show()` (`reviewer.py:182`).
* For each question shown, aqt evaluates in the reviewer webview (`reviewer.py:1240-1244`):
  ```js
  anki.mutateNextCardStates('{key}', async (states, customData, ctx) => {
      {js}
  }).finally(() => bridgeCommand('statesMutated'));
  ```
  `key` is a random u64 per reviewer instance (`:162`) so that only the reviewer can set states.
* `mutateNextCardStates` (`ts/reviewer/answering.ts:45-52`):
  1. `getSchedulingStatesWithContext({})`: an HTTP POST from the webview to aqt's media server, which calls
     `Reviewer.get_scheduling_states()` / `get_scheduling_context()` (`reviewer.py:274-278`). That is the
     `FrontendService.GetSchedulingStatesWithContext` RPC (`frontend.proto:19-22, 68-76`), **implemented in Python, not
     rslib**.
  2. Converts `states` to **protobuf JSON** with `emitDefaultValues: true` (camelCase field names, e.g.
     `states.good.normal.review.scheduledDays`).
  3. `customData`: `{again, hard, good, easy}`. **Each is `JSON.parse(states.current.customData)`** (all four start
     as copies of the current card's custom data, `answering.ts:17-31`).
  4. `await transform(statesJson, customData, ctx)` where `ctx` is the `SchedulingContext` message (deckName, seed,
     decay, desiredRetention).
  5. `SchedulingStates.fromJson(statesJson)`, then pack `customData.X` as JSON strings into `states.X.customData`
     (`:33-41`).
  6. `setSchedulingStates({key, states})` → `Reviewer.set_scheduling_states` (`reviewer.py:280-284`): replaces
     `self._v3.states` if the key matches.
* `_showEaseButtons` blocks until `statesMutated` arrives (`reviewer.py:859-862`). If the JS has a syntax error, the eval
  returns `None` and `_states_mutated` is set to True (`:287-290`), so the reviewer does not hang.
* The mutated `states` are used **both** for the button labels (`describe_next_states`) and for `build_answer`. rslib
  accepts any `new_state` (it only validates `current_state`), so the JS can set arbitrary intervals/eases/steps.
  `new_state.custom_data` is persisted to `cards.data` (`answering/mod.rs:349-352`; validated by
  `validate_custom_data`, `rslib/src/storage/card/data.rs:135-149`: must be a JSON object, keys ≤ 8 bytes, serialised ≤ 100 bytes; violations make `AnswerCard` fail).

### 3.2 What a frontend without JS loses

Only users who have pasted custom scheduling code lose anything. For them, intervals silently fall back to stock
v3/FSRS (wrong labels, wrong scheduling, `customData` not updated). Typical uses: interval caps, "Easy days" hacks,
the old FSRS4Anki helper scripts (now mostly obsolete because FSRS is built in), custom ease logic. **Mitigation even without
an engine:** detect a non-empty `cardStateCustomizer` and warn once ("custom scheduling code is not supported;
answers will use the standard scheduler"), or refuse to study.

### 3.3 Embedding a JS engine: feasible

Input = JSON (states with defaults emitted + customData + ctx); output = JSON states + customData. No DOM is needed in practice,
but scripts may call `console.log` and use `async` code. Options: **`rquickjs`** (QuickJS, small, supports async/promises,
easy JSON round-trip) or **`boa_engine`** (pure Rust). Recipe:
1. Proto → JSON: the generated `anki_proto` structs do **not** derive serde for `SchedulingStates`
   (`rslib/proto/rust.rs:26-51` adds serde only to a few types). Use **`prost-reflect`** (already a dependency of `anki`,
   workspace `prost-reflect = 0.14.7`; with its `serde` feature, `DynamicMessage` serialises with protobuf JSON mapping) and the
   descriptor set that `anki_proto` build writes. Alternatively hand-write the small mapping (about 10 messages).
2. Must match the TS behaviour: `emitDefaultValues`, camelCase names, int64 as strings (only `seed`, which is uint64 in
   `SchedulingContext`, but ctx is passed as a message object, not JSON. In TS, `ctx.seed` is a `bigint`;
   scripts rarely use it), `customData` copied from `current` into each of the four slots.
3. Provide a `console` shim and a timeout (interrupt handler) so a bad script cannot hang the TUI.
Verdict: **doable** (around 300-500 lines with rquickjs). Risk: scripts using browser globals (`document`, `fetch`)
would fail; those cases are rare. Unverified: I did not test with real community scripts.

---
## 4. Deck options

### 4.1 Model: presets vs decks vs global

* A **preset** = `DeckConfig {id, name, mtime_secs, usn, config: DeckConfig.Config}` (`proto/anki/deck_config.proto:59-204`;
  Rust `rslib/src/deckconfig/mod.rs:31-38`, `inner: DeckConfigInner` = the proto `Config`). Each **normal deck** points to one
  preset via `Deck.Normal.config_id` (`decks.proto:73-93`). Filtered decks have no preset; they use the home deck's preset
  for each card.
* **Deck-level overrides** (stored on the deck, not the preset): `review_limit`, `new_limit` ("This deck"),
  `review_limit_today`/`new_limit_today` (`DayLimit{limit, today}`, "Today only"), and `desired_retention` (FSRS
  per-deck desired retention). The UI shows these as tabs on the limit/retention spinboxes: "Preset" / "This deck" / "Today only"
  (`DailyLimits.svelte:72-118`, `FsrsOptions.svelte:94-110`).
* **Collection-global settings** shown on the deck options page (marked with a "global" label): FSRS on/off, new cards
  ignore review limit, apply all parent limits, custom scheduling JS, reschedule cards on change, health check
  (`DeckConfigsForUpdate` fields 6-12, `UpdateDeckConfigsRequest` fields 5-11).
* Global settings **not** on this page (they are in Preferences, `Preferences.Scheduling/Reviewing`, `config.proto:105-150`;
  `GetPreferences`/`SetPreferences`): next-day-starts-at (rollover), learn-ahead limit, timebox, show intervals on buttons,
  show remaining count, hide play buttons, interrupt audio, **load balancer enabled** (`load_balancer_enabled`, default true)
  and **FSRS short-term with steps** (`fsrs_short_term_with_steps_enabled`). Note: in 26.08.1 I found **no Qt/TS UI** for the
  load balancer or short-term toggles (only the proto fields, the `SetLoadBalancerEnabled` RPC `collection.proto:22`, and
  pylib properties `collection.py:992-1010`). Treat as unverified whether the desktop exposes them anywhere.

### 4.2 RPCs powering the screen

| RPC | Purpose | Rust |
|---|---|---|
| `GetDeckConfigsForUpdate(DeckId) → DeckConfigsForUpdate` | everything needed to open the page | `deckconfig/update.rs:51-81` |
| `UpdateDeckConfigs(UpdateDeckConfigsRequest) → OpChanges` | save (undoable `Op::UpdateDeckConfig`) | `update.rs:84-89, 160-329` |
| `ComputeFsrsParams{search, current_params, ignore_revlogs_before_ms, num_of_relearning_steps, health_check}` | "Optimize" | `scheduler/fsrs/params.rs:81` |
| `EvaluateParamsLegacy{params, search, ignore_revlogs_before_ms}` | "Evaluate" button (only if `fsrs_legacy_evaluate`) | `params.rs:311` |
| `EvaluateParams` | newer evaluate (not used by the TS page) | `params.rs:278` |
| `SimulateFsrsReview(SimulateFsrsReviewRequest)` | simulator chart | `fsrs/simulator.rs:252` |
| `SimulateFsrsWorkload` | "Help me decide" workload modal | `simulator.rs:280` |
| `ComputeOptimalRetention` | simulator → optimal DR | `fsrs/retention.rs:17` |
| `GetRetentionWorkload{w, search} → map<u32,float>` | desired-retention change info ("workload ×1.3") | `deckconfig/service.rs:105` |
| `GetIgnoredBeforeCount{date, search}` | "N of M cards will be used" for "ignore reviews before" | `service.rs:86-103` |
| `GetOptimalRetentionParameters` | simulator defaults | `service/mod.rs:332` |
| `LatestProgress` / `SetWantsAbort` | progress bar / cancel during optimize (`collection.proto:20-21`) | collection service |
| `FrontendService.deckOptionsReady/RequireClose` | Qt window plumbing only | aqt |

`DeckConfigsForUpdate` (`deck_config.proto:206-243`): `all_config[] {config, use_count}` (sorted by name; empty
fsrs_params_6 is pre-filled from v5/v4 params, `update.rs:92-116`), `current_deck {name, config_id, parent_config_ids[], limits}`,
`defaults` (the default preset with FSRS default params), `schema_modified` (deleting a preset forces a full sync;
warn), `card_state_customizer`, `new_cards_ignore_review_limit`, `fsrs`, `fsrs_health_check`, `fsrs_legacy_evaluate`,
`apply_all_parent_limits`, `days_since_last_fsrs_optimize`.

`UpdateDeckConfigsRequest` (`:251-265`): `target_deck_id`, `configs[]` (**modified presets; the selected one MUST be
last**, the deck gets assigned the last one; new presets have id 0), `removed_config_ids[]`, `mode` (NORMAL /
APPLY_TO_CHILDREN = "Save to all subdecks" / COMPUTE_ALL_PARAMS = "Save & optimize all presets"), the global fields, `limits`
(deck overrides for the target deck), `fsrs_reschedule`. TS builder: `ts/routes/deck-options/lib.ts:247-280`.

Side effects of save (`update.rs:160-329`): clamps `ignore_revlogs_before_date` to today; empty FSRS6 params clear v4/v5 params;
validates params with `FSRS::new`; re-sorts new cards if `new_card_insert_order` changed; **recomputes FSRS memory states**
(and reschedules if `fsrs_reschedule`) for decks whose params/DR/FSRS toggle (or easy days with reschedule) changed; adjusts remaining
(re)learning steps of in-flight cards when steps change (`:332-361`). Saving can be **slow** (memory-state recompute):
run it on a worker thread with a progress indicator.

Preset management on the page (`ConfigSelector.svelte`, `SaveButton.svelte`): switch preset (dropdown, with use counts),
Add, Clone, Rename, Remove (not the default id 1, `schedulingTheDefaultConfigurationCantBeRemoved`), Save (Ctrl+Enter),
Save to all subdecks. Add-ons can inject sections (`Addons.svelte`, out of scope).

### 4.3 Every option (proto `DeckConfig.Config` field → UI → default)

Defaults: `rslib/src/deckconfig/mod.rs:41-103`. Legacy JSON (schema11) names in parentheses where pylib uses them
(`deckconfig/schema11.rs`).

**Daily limits** (`DailyLimits.svelte`)
| Option | Field | Default / range | Notes |
|---|---|---|---|
| New cards/day | `new_per_day`(9) + deck `new_limit` / `new_limit_today` | 20 | tabs: Preset / This deck / Today only |
| Maximum reviews/day | `reviews_per_day`(10) + deck `review_limit` / `review_limit_today` | 200 | warning if lower than 10 × new |
| New cards ignore review limit (global) | `newCardsIgnoreReviewLimit` | false | |
| Apply limits of all parent decks (global) | `applyAllParentLimits` | false | |
| (unused) | `new_per_day_minimum`(35) | 0 | not in UI |

**New cards** (`NewOptions.svelte`)
| Option | Field | Default / range | Notes |
|---|---|---|---|
| Learning steps | `learn_steps`(1) floats in minutes, UI syntax "1m 10m 1h 2d" (`steps.ts`) | [1, 10] |
| Graduating interval (hidden with FSRS) | `graduating_interval_good`(18) days | 1 |
| Easy interval (hidden with FSRS) | `graduating_interval_easy`(19) | 4 |
| Insertion order | `new_card_insert_order`(20): DUE(sequential)=0, RANDOM=1 | Due | changing it re-sorts the deck on save |
Warnings: steps longer than graduating interval; FSRS: steps ≥ 1 day are too large.

**Lapses** (`LapseOptions.svelte`)
| Option | Field | Default / range | Notes |
|---|---|---|---|
| Relearning steps | `relearn_steps`(2) | [10] |
| Minimum interval (hidden with FSRS) | `minimum_lapse_interval`(17) | 1, min 1 |
| Leech threshold | `leech_threshold`(22) | 8, min 1 |
| Leech action | `leech_action`(21): SUSPEND=0, TAG_ONLY=1 | Tag only |

**Display order** (`DisplayOrder.svelte`, labels in `choices.ts`)
| Option | Field | Default / range | Notes |
|---|---|---|---|
| New card gather order | `new_card_gather_priority`(34): DECK=0, DECK_THEN_RANDOM_NOTES=5, LOWEST_POSITION=1, HIGHEST_POSITION=2, RANDOM_NOTES=3, RANDOM_CARDS=4 | Deck |
| New card sort order | `new_card_sort_order`(32): TEMPLATE=0, NO_SORT=1, TEMPLATE_THEN_RANDOM=2, RANDOM_NOTE_THEN_TEMPLATE=3, RANDOM_CARD=4 | Template; some combinations disabled (`DisplayOrder.svelte:40-75`) |
| New/review order | `new_mix`(30): MIX=0, AFTER_REVIEWS=1, BEFORE_REVIEWS=2 | Mix |
| Interday learning/review order | `interday_learning_mix`(31) same enum | Mix |
| Review sort order | `review_order`(33): DAY=0, DAY_THEN_DECK=1, DECK_THEN_DAY=2, INTERVALS_ASC=3, INTERVALS_DESC=4, EASE_ASC=5 (FSRS: difficulty), EASE_DESC=6, RETRIEVABILITY_ASC=7, RETRIEVABILITY_DESC=11, RELATIVE_OVERDUENESS=12, RANDOM=8, ADDED=9, REVERSE_ADDED=10 | Day; retrievability orders only shown with FSRS |
Display order options use the preset of the **deck you click Study on** (not subdecks), see `deckConfigDisplayOrderWillUseCurrentDeck` and `queue/builder/mod.rs:232-249`.

**FSRS** (`FsrsOptionsOuter.svelte`, `FsrsOptions.svelte`)
| Option | Field | Default / range | Notes |
|---|---|---|---|
| FSRS on/off (global) | `fsrs` bool | false |
| Desired retention | `desired_retention`(37) + deck override `Deck.Normal.desired_retention` | 0.9, range 0.70-0.99; tabs Preset / This deck; warnings for too high/low; workload-change info via `GetRetentionWorkload` |
| "Help me decide" (experimental) | workload simulator modal (`SimulateFsrsWorkload`) | |
| FSRS parameters | `fsrs_params_6`(6) (falls back to `_5`(5), `_4`(3) when reading) | empty = defaults; comma list editor |
| Optimize search | `param_search`(45) | default `preset:"<name>" -is:suspended` |
| Reschedule cards on change (global, per-save) | `UpdateDeckConfigsRequest.fsrs_reschedule` | false; warning |
| Health check (global, slow) | `fsrsHealthCheck` | default true in rslib (`config/bool.rs:70`) |
| Optimize | `ComputeFsrsParams` (num_of_relearning_steps = relearning steps that fit within 1 day, `FsrsOptions.svelte:214-224`) | progress + cancel |
| Evaluate | `EvaluateParamsLegacy` → "Log loss: x, RMSE(bins): y%" | only if `fsrs_legacy_evaluate` |
| Save & optimize all presets | `mode=COMPUTE_ALL_PARAMS` | warns when `days_since_last_fsrs_optimize` is large |
| Simulator (experimental) | `SimulateFsrsReview` / `ComputeOptimalRetention` modal; inputs: days to simulate, deck size, new/day, reviews/day, max interval, review order, new ignores review limit, suspend leeches, easy days | charts (`renderSimulationChart`) |
| Historical retention (Advanced, FSRS only) | `historical_retention`(40) | 0.9, 0.5-1.0 |
| Ignore reviews before (Advanced, FSRS only) | `ignore_revlogs_before_date`(46) "YYYY-MM-DD" | "" ; shows `GetIgnoredBeforeCount` |
| (minimum recommended retention / "compute optimal retention") | `ComputeOptimalRetention` inside the simulator | |

**Burying** (`BuryOptions.svelte`): `bury_new`(27), `bury_reviews`(28), `bury_interday_learning`(29), all false.

**Audio** (`AudioOptions.svelte`): `disable_autoplay`(23) false (legacy `autoplay`), `skip_question_when_replaying_answer`(26) false
(legacy `replayq` inverted).

**Timer** (`TimerOptions.svelte`): `cap_answer_time_to_secs`(24) 60 (1-7200; legacy `maxTaken`), `show_timer`(25) false (legacy
`timer`), `stop_timer_on_answer`(38) false.

**Auto advance** (`AutoAdvance.svelte`): `seconds_to_show_question`(41) 0.0 (step 0.1; 0 = off), `seconds_to_show_answer`(42) 0.0,
`wait_for_audio`(44) true, `question_action`(36) SHOW_ANSWER=0 / SHOW_REMINDER=1, `answer_action`(43) BURY_CARD=0 / ANSWER_AGAIN=1 /
ANSWER_GOOD=2 / ANSWER_HARD=3 / SHOW_REMINDER=4.

**Easy days** (`EasyDays.svelte`, `EasyDaysInput.svelte`): `easy_days_percentages`(4) 7 floats Monday..Sunday (`EasyDaysInput.svelte:9-17`), sliders 0.0/0.5/1.0 (Minimum / Reduced / Normal), default all 1.0. Used by the load balancer and the
FSRS rescheduler/simulator (`states/load_balancer.rs:284-330`).

**Advanced** (`AdvancedOptions.svelte`)
| Option | Field | Default / range | Notes |
|---|---|---|---|
| Maximum interval | `maximum_review_interval`(16) | 36500, 1-36500 |
| Starting ease (SM-2 only) | `initial_ease`(11) | 2.5, 1.31-5 |
| Easy bonus (SM-2) | `easy_multiplier`(12) | 1.3, 1-5 |
| Interval modifier (SM-2) | `interval_multiplier`(15) | 1.0, 0.5-2 |
| Hard interval (SM-2) | `hard_multiplier`(13) | 1.2, 0.5-1.3 |
| New interval (SM-2) | `lapse_multiplier`(14) | 0.0, 0-1 |
| Custom scheduling (global) | `cardStateCustomizer` | "" ; see §3 |
| (opaque) | `other`(255) bytes | legacy JSON keys preserved |

Verdict for the deck options screen: **doable but large**. All logic is in rslib; tanki needs a form with about 45 widgets,
tabs for limit scopes, a step-string parser/formatter (port `ts/routes/deck-options/steps.ts`), validation warnings (port the
`Warning` conditions), preset CRUD and the save modes. FSRS optimize/evaluate: **trivial** to call, needs a progress UI via
`LatestProgress`. **Simulator charts: hard** (draw a line chart with ratatui `Chart`/braille; doable but fiddly).
Help modals: copy the text from ftl.

---
## 5. Filtered decks and custom study

### 5.1 Filtered decks (rslib `scheduler/filtered/**`, aqt `qt/aqt/filtered_deck.py`)

Config proto (`decks.proto:94-125`):
```proto
message Deck.Filtered {
  message SearchTerm { string search = 1; uint32 limit = 2; Order order = 3; }
  // Order: OLDEST_REVIEWED_FIRST=0, RANDOM=1, INTERVALS_ASCENDING=2, INTERVALS_DESCENDING=3, LAPSES=4, ADDED=5,
  //        DUE=6, REVERSE_ADDED=7, RETRIEVABILITY_ASCENDING=8, RETRIEVABILITY_DESCENDING=9, RELATIVE_OVERDUENESS=10
  bool reschedule = 1;            // "Reschedule cards based on my answers in this deck"
  repeated SearchTerm search_terms = 2;   // UI supports 1 or 2 ("second filter"); rslib uses .take(2)
  repeated float delays = 3;      // v1 only
  uint32 preview_delay = 4;       // v2/old v3 only
  uint32 preview_again_secs = 7;  // used when reschedule=false; 0 => card leaves the deck (finished)
  uint32 preview_hard_secs = 5;
  uint32 preview_good_secs = 6;   // (Easy always finishes)
}
message FilteredDeckForUpdate { int64 id = 1; string name = 2; Deck.Filtered config = 3; bool allow_empty = 4; }
```
RPCs: `GetOrCreateFilteredDeck(DeckId)` (id 0 → new deck "Filtered Deck HH:MM", prefilled with `deck:"<current>" is:due` /
`is:new` terms, `filtered/mod.rs:206-233`), `AddOrUpdateFilteredDeck(FilteredDeckForUpdate) → OpChangesWithId` (normalises searches,
saves, rebuilds; **fails with `SearchReturnedNoCards` unless `allow_empty`**; makes the deck current, `:150-186`),
`RebuildFilteredDeck(DeckId) → OpChangesWithCount`, `EmptyFilteredDeck(DeckId)`, `FilteredDeckOrderLabels() → StringList`
(localised labels in enum order). Build: each term's search gets ` -is:suspended -is:buried -deck:filtered` appended, sorted by
`order_and_limit_for_search` (`storage/card/filtered.rs`), cards moved with negative positions from −100000
(`filtered/mod.rs:103-141`). The v1 scheduler gives `SchedulerUpgradeRequired`.

Study behaviour: `reschedule=true` → `ReschedulingFilter{original_state}` (normal scheduling, card returns home on
graduation/review); `reschedule=false` → `Preview` states with the configured delays (`states/preview_filter.rs:25-48`;
the button label is "End" when finished). Filtered decks have no preset; limits do not apply (`RemainingLimits::default()` = 9999,
`decks/limits.rs:159-167`), new-card sort is NoSort, and display order comes from defaults (`queue/builder/mod.rs:242-247`).

Dialog fields (`filtered_deck.py:120-161, 251-285`): name, search 1 + limit + order, optional second filter (default order
index 5 = Added, limit 20), reschedule checkbox (hides the preview-delay widget when checked), preview again/hard/good seconds,
"allow empty" checkbox, search buttons that open the Browser with the term, a hint about `is:suspended` etc.
Overview actions: Rebuild (r), Empty (e). Creating one: Tools → Create Filtered Deck, shortcut `F` (`qt/aqt/forms/main.ui:222-225`, `main.py:1423-1424,1456`).

Verdict: **trivial** (form + 4 RPCs). Rebuilding from the overview is one keystroke.

### 5.2 Custom study (rslib `filtered/custom_study.rs`, aqt `qt/aqt/customstudy.py`, `qt/aqt/taglimit.py`)

```proto
// scheduler.proto:288-350
message CustomStudyRequest {
  message Cram { CramKind kind = 1; uint32 card_limit = 2; repeated string tags_to_include = 3; repeated string tags_to_exclude = 4; }
  // CramKind: DUE=0 (due order, reschedule), NEW=1 (added order, reschedule), REVIEW=2 (random, reschedule), ALL=3 (random, NO reschedule)
  int64 deck_id = 1;
  oneof value { int32 new_limit_delta = 2; int32 review_limit_delta = 3; uint32 forgot_days = 4;
                uint32 review_ahead_days = 5; uint32 preview_days = 6; Cram cram = 7; }
}
message CustomStudyDefaultsResponse { repeated Tag tags = 1 /*name, include, exclude (remembered per deck)*/;
  uint32 extend_new = 2; uint32 extend_review = 3; uint32 available_new = 4; uint32 available_review = 5;
  uint32 available_new_in_children = 6; uint32 available_review_in_children = 7; }
```
Modes (`custom_study.rs:100-158, 197-292`):
1. Increase today's new limit by N (may be negative): `extend_limits` + remembers `extend_new` on the deck.
2. Increase today's review limit by N: same for reviews.
3. Review forgotten cards (last N days, max 30): filtered `rated:N:1 deck:X`, Random, reschedule=false.
4. Review ahead N days: `prop:due<=N deck:X`, Due order, reschedule=true.
5. Preview new cards added in last N days: `is:new added:N deck:X`, Added order, reschedule=false.
6. Study by card state or tag: Cram kind + limit + tag include/exclude chooser (TagLimit dialog). Include/exclude tags are
   remembered in deck-scoped config keys `customStudyIncludeTags`/`customStudyExcludeTags`.
Modes 3-6 create or reuse a filtered deck named by `custom_study_custom_study_session` ("Custom Study Session"); error
`ExistingDeck` if a normal deck has that name; `NoMatchingCards` on an empty result. Preview delays for custom study are fixed:
again 60 s, hard 600 s, good 0 (`:203-215`). The dialog title shows available counts (`count_with_children`).

Verdict: **trivial** (radio list + number + optional tag multi-select).

---

## 6. Card-level scheduling operations

| Op | RPC (proto) | Rust | Semantics | Verdict |
|---|---|---|---|---|
| Bury card/note | `BuryOrSuspendCards{card_ids \| note_ids, mode: BURY_USER}` → `OpChangesWithCount` | `bury_and_suspend.rs:117-130` | queue = UserBuried (−3); suspended cards untouched; if `card_ids` is empty, note ids are expanded to their cards (`service/mod.rs:104-116`) | trivial |
| Sibling bury (automatic) | (inside AnswerCard / queue build) | `bury_siblings` `:132-…`, `queue/builder/burying.rs` | SchedBuried (−2) | n/a |
| Suspend card/note | `BuryOrSuspendCards{…, SUSPEND}` | same | queue = Suspended (−1) | trivial |
| Unbury/unsuspend cards | `RestoreBuriedAndSuspendedCards(CardIds)` | `unbury_or_unsuspend_cards` `:64-69` | queue restored from type | trivial |
| Unbury deck | `UnburyDeck{deck_id, mode: ALL \| SCHED_ONLY \| USER_ONLY}` | `:71-82` | includes children | trivial |
| Auto unbury | `SchedTimingToday`, `DeckTree{now}`, queue build | `unbury_if_day_rolled_over` `:32-40` | on day change, not marked modified | n/a |
| Forget / reset | `ScheduleCardsAsNew{card_ids, log, restore_position, reset_counts, context}`; defaults `ScheduleCardsAsNewDefaults{context}` | `new.rs:153-207` | back to the new queue at the end (or original position); optionally zero reps/lapses; logs a "Manual" revlog; remembers checkbox defaults per context (Browser/Reviewer) | trivial |
| Set due date | `SetDueDate{card_ids, days, config_key?}` | `reviews.rs:128-178`, parser `parse_due_date_str` `:91-122` (pub) | `days` = `N`, `N-M` (random in range, inclusive), suffix `!` = reset interval to the new due distance (SM-2); `0` = today. New/learning cards become review cards. With FSRS the interval = days since last review + N. Logs a Manual revlog. `config_key` remembers the last input (`SET_DUE_BROWSER` default "0", `SET_DUE_REVIEWER` default "1") | trivial |
| Reposition new cards | `SortCards{card_ids, starting_from, step_size, randomize, shift_existing}`; defaults `RepositionDefaults` | `new.rs:209-258` | only new cards; remembers randomize/shift | trivial |
| Re-sort whole deck | `SortDeck{deck_id, randomize}` (legacy) | `new.rs` `sort_deck_legacy` | now done automatically when insertion order changes | n/a |
| Grade now | `GradeNow{card_ids, rating}` | `reviews.rs:180-206` | answers each card with its computed state, 0 ms, `from_queue=false` (no queue pop); browser-only in the desktop | trivial |
| FSRS reschedule on change | `UpdateDeckConfigs{fsrs_reschedule: true}` | `fsrs/memory_state.rs:83` `update_memory_state` + `fsrs/rescheduler.rs` | recomputes memory states and re-dues review cards (uses load balancer + easy days if enabled, `memory_state.rs:165`) | trivial (one flag) |
| Compute memory state | `ComputeMemoryState(CardId)` | `memory_state.rs:353` | card info | trivial |
| Fuzz delta | `FuzzDelta{card_id, interval}` | | for add-ons | n/a |
| Timing | `SchedTimingToday() → {days_elapsed, next_day_at}` | `service/mod.rs:36-40` | **also unburies on rollover** | trivial |
| Extend limits | `ExtendLimits{deck_id, new_delta, review_delta}` | `decks/stats.rs:48` | used by custom study | trivial |
| Counts for deck today | `CountsForDeckToday(DeckId) → {new, review}` | `decks/counts.rs:41` | studied today | trivial |

aqt dialogs to recreate: set-due-date text prompt with hint `scheduling_set_due_date_prompt_hint` (`operations/scheduling.py:30-65`),
forget dialog with 2 checkboxes (`forms/forget.ui`), reposition dialog (start, step, randomize, shift; shows queue top/bottom,
`operations/scheduling.py:134-208`).

---

## 7. Legacy schedulers (v1 / v2)

* Collection config `schedVer` (1 or 2) + bool `sched2021`. **"v3" = schedVer 2 + sched2021 = true**
  (`rslib/src/config/mod.rs:213-224`; pylib `collection.py:207-244`). New collections are created with `schedVer: 2,
  sched2021: true` (`config/schema11.rs:25-27`). Missing `schedVer` → treated as **V1** (`config/mod.rs:213-216`).
* pylib: v1 → `DummyScheduler`, v2 without sched2021 → `DummyScheduler` (`collection.py:217-230`), whose
  `getCard/answerCard` raise "v1/v2 scheduler no longer supported" (`pylib/anki/scheduler/dummy.py`). The v1/v2 Python schedulers
  have been **deleted**; only `legacy.py` (compat shims) remains.
* aqt: the deck browser shows a "scheduler update required" banner with an Update button (`deckbrowser.py:407-438`): v1 →
  `col.mod_schema(check=True)` (full-sync warning) + `upgrade_to_v2_scheduler()` (`UpgradeScheduler` RPC → rslib
  `scheduler/upgrade.rs:110-128`: converts revlog and cards, enables the new timezone, forces a full sync), then
  `set_v3_scheduler(True)`. The reviewer refuses to start (`reviewer.py:175-178`).
* rslib: v1 gives `SchedulerUpgradeRequired` for bury/suspend, sort, filtered rebuild, rollover config, apkg card import.
  **`get_queued_cards`/`answer_card` do not check `sched2021`**, so a v2-without-v3 collection would be studied with v3 logic
  (the DB schema is identical). On V1 the timing code falls back to the v1 day computation (`sched_timing_today`, `timing.rs:152-178`; `scheduler/mod.rs:61-75`).
* pylib also enables the new timezone on load when v3 is active and `creationOffset` is missing (`collection.py:224-228`).
  tanki should do the same: if `creationOffset` is missing, call `SetPreferences` with `scheduling.new_timezone = true`.
* **tanki recommendation:** on open, if `!v3_enabled()`, show a one-time prompt "Upgrade to v3?". v1 needs the schema-change confirmation
  (full sync) → `UpgradeScheduler` + `SetConfigBool(SCHED_2021, true)`; v2 only needs the bool. Verdict: **trivial**.

---

## 8. Webview-dependent parts of the study flow

### 8.1 Bridge commands handled by the reviewer (`Reviewer._linkHandler`, `reviewer.py:673-693`)

| Command | Sent by | Meaning |
|---|---|---|
| `ans` | Show Answer button; Enter in the `#typeans` input (`_typeAnsPress`, `ts/reviewer/index.ts:263-268`) | show answer (reads the typed answer first) |
| `ease1`..`ease4` | answer buttons (bottom bar) | answer |
| `edit` | Edit button | edit current note |
| `more` | More button | context menu |
| `play:q:N` / `play:a:N` | `[sound:]` play icons (`av_refs_to_play_icons`) | play one AV tag |
| `updateToolbar` | after every QA update (`index.ts:181`) | refresh toolbar background image |
| `repaintNeeded` | after scrolling to the answer (`index.ts:227-230`) | Qt repaint workaround |
| `statesMutated` | end of custom-scheduling JS (`reviewer.py:1243`) | unblock answer buttons |

Overview bridge commands (`overview.py:96-124`): `study, opts, cram, refresh, empty, decks, review (shared deck page),
studymore, customStudy, unbury, description, http…`. Deck browser (`deckbrowser.py:105-138`): `open:<id>, opts:<id>, shared,
import, create, drag:<src>,<dst>, collapse:<id>, v2upgrade, v2upgradeinfo, select:<id>`. Congrats page uses
`bridgeLink("unbury")`/`bridgeLink("customStudy")`. JS→Python calls (`evalWithCallback`): `getTypedAnswer()`,
`selectedAnswerButton()` (bottom bar focus), `window.pageYOffset`.
Webview HTTP RPCs from the reviewer page: `GetSchedulingStatesWithContext`, `SetSchedulingStates` (Python), plus image
occlusion helpers.

### 8.2 Rendering pipeline (`ts/reviewer/index.ts`)

* `_showQuestion(q, a, bodyclass)` / `_showAnswer(a)`: queued through a promise chain (`_queueAction`, `:81-85`). `_updateQA`
  (`:146-198`): clears and installs `onUpdateHook`/`onShownHook`, lazily loads MathJax if `\(..\)`/`\[..\]` is present,
  preloads resources (fonts/images), sets innerHTML, **re-creates `<script>` tags so card JS runs** (`replaceScript`, `:90-130`),
  runs `onUpdateHook`, typesets MathJax, fades in, runs `onShownHook` (focus `#typeans`, preload answer images). The body class
  is `card cardN` plus night-mode classes (`theme_manager.body_classes_for_card_ord`).
* Public JS API for card templates: `anki/reviewer` package `{onUpdateHook, onShownHook}`, `globalThis.anki.{mutateNextCardStates,
  imageOcclusion, setupImageCloze}`, `ankiPlatform = "desktop"`.

### 8.3 Card rendering is partly in pylib (important)

`Card.question()/answer()` → `TemplateRenderContext.render()` (`pylib/anki/template.py:224-256`): calls
`RenderExistingCard{card_id, browser, partial_render: true}`, then pylib applies **add-on custom filters** and fills
`{{FrontSide}}` (rslib defers both when `partial_render` is set, `rslib/src/template.rs:423-435`), then calls
`ExtractAvTags{text, question_side}` on each side, which replaces `[sound:…]`/TTS with `[anki:play:q|a:N]` refs and returns the tag
list. Then aqt `prepare_card_text_for_display` (`main.py:641-644`) escapes media filenames and turns refs into play icons (or strips
them if `hideAudioPlayButtons`). **For tanki:** call `RenderExistingCard` with `partial_render: false` (rslib then does FrontSide
itself; there are no custom filters to apply), then run `ExtractAvTags` per side yourself, mirroring the pylib order (question first,
then answer). The `[[type:…]]` marker is left for the frontend (see §2). The output includes `<style>` CSS (`question_and_style()`).

### 8.4 Terminal feasibility of webview-dependent features

| Feature | Verdict | Reason / approach |
|---|---|---|
| Plain HTML card text (b/i/u, lists, tables, `<br>`, colours) | **doable** | HTML → styled terminal text (e.g. `html2text`-like converter + ratatui spans). Handle `<hr id=answer>` (scroll/separator), `{{FrontSide}}` |
| Card CSS (fonts, sizes, layout, centring) | **impossible** to honour fully | at best map colour/bold/italic/alignment; ignore the rest |
| JavaScript in templates (hints, tabs, randomisation, `onShownHook`) | **impossible** without a browser engine | JS that manipulates the DOM needs a DOM. Could embed a headless engine (no). Show an "[card uses JS]" notice; `{{hint:}}` fields can be done natively (reveal on key) |
| MathJax `\(…\)` `\[…\]` | **hard** | render TeX source verbatim, or convert simple TeX to Unicode (e.g. `unicodeit`-style); full typesetting impossible in a text grid (possible as images via sixel/kitty graphics + external `mathjax-node`/`latex`→PNG) |
| LaTeX (`[latex]`, pre-rendered PNG/SVG in media) | **hard** | image protocols (kitty/sixel/iTerm2) via `ratatui-image`; else show the alt text/source |
| Images | **hard → doable** on kitty/WezTerm/iTerm2/foot (sixel); **impossible** on plain terminals (show `[image: file.jpg]`, open in an external viewer on a key) |
| Audio `[sound:]`, autoplay order, replay, pause/seek | **doable** | external `mpv` (IPC socket for pause/seek/end-of-playback events); keep the question→answer tag order from `RenderExistingCard` AV tags |
| TTS `[anki:tts …]` | **doable** | `espeak-ng`/`say`/speech-dispatcher; voices differ from desktop |
| Video | **hard** | `mpv` in a separate window, or `--vo=tct`/`kitty` |
| Type-in-answer | **doable** | native input line + `CompareAnswer` HTML → coloured spans |
| Image occlusion masks | **hard** | needs the image + shapes drawn over it (`ts/routes/image-occlusion/review.ts` uses canvas). With a graphics protocol, composite the masks into the image with the `image` crate before display (shapes are in the cloze text, see `extractShapesFromRenderedClozes`); otherwise **impossible** (text fallback: label list) |
| Custom scheduling JS | **doable** | embedded QuickJS (§3.3) |
| Night mode / themes | **trivial** | terminal palette |
| Flag/mark glyphs, timer, counts | **trivial** | |
| `onUpdateHook`/`onShownHook` | **impossible** (no DOM) | ignore |
| Clickable play buttons | **doable** | mouse support or numbered keys |

---

## 9. Implementation checklist for tanki's reviewer

1. On deck select: `SetCurrentDeck(did)`; overview = `GetQueuedCards{fetch_limit:1}` counts + `DeckTree` node diff + `CongratsInfo` when empty.
2. Study start: record `timebox_start = now`, `timebox_reps = 0`.
3. Loop:
   - `q = GetQueuedCards{fetch_limit: 1}` (optionally 2 to prefetch/pre-render the next card). Empty → congrats screen (`CongratsInfo`, re-poll every 60 s).
   - `states = q.cards[0].states`; `states.current.custom_data = card.custom_data` (**required for custom JS and for keeping custom data**).
   - If custom JS is set and the engine is enabled: `states = run_js(states, ctx)`.
   - Render the question (`RenderExistingCard{card_id, browser:false}`) → process `[[type:]]`, AV tags; autoplay unless `disable_autoplay`.
   - `shown_at = Instant::now()`; start auto-advance timers if toggled.
   - Show answer → render the answer, `CompareAnswer` if typing, labels = `DescribeNextStates(states)` (if `estTimes`).
   - Rate → `AnswerCard{card_id, current_state: states.current, new_state: states.<rating>, rating, answered_at_millis: now_ms, milliseconds_taken: shown_at.elapsed()}`; if `StateIsLeech(new_state)` → toast "leech (+suspended if card.queue == -1)".
   - Timebox check (`timeLim` seconds; `0` = off).
4. Undo: `Undo` → if the result's op was an answer, just re-fetch (the queue is restored by rslib). Use `GetUndoStatus` for menu labels.
5. Refresh rules: after any op with `OpChanges.study_queues` (or `card`/`note_text`), re-fetch/redraw (aqt `op_executed`, `reviewer.py:221-235`).
6. Long-running process: rely on `get_queued_cards` for rollover; refresh the deck list on a timer.

---

## 10. Not verified / open questions

* Load balancer and FSRS "short-term with steps" toggles: proto/RPC/pylib only; I found no Qt or TS UI in 26.08.1
  (`grep` over `qt/` and `ts/`). The defaults are `loadBalancerEnabled=true` and short-term `false`.
* Custom scheduling JS under QuickJS/Boa: the design is checked against `ts/reviewer/answering.ts`, but no real community script was run.
* `prost-reflect` JSON serialisation of `SchedulingStates`: the dependency exists in the workspace (`Cargo.toml:108`), but whether
  `anki_proto` exposes the descriptor pool publicly for reuse was not checked. Hand-written serde is the fallback.
* Exact HTML→terminal fidelity, image protocols and MathJax fallbacks are design suggestions, not tested.
* The `QueuedCards` naming issue (pub struct in a `pub(crate)` module) was found by reading the code, not by compiling.
  Same for calling `anki::services::SchedulerService` methods from an external crate (trait and module are `pub`; `lib.rs:40`).
* The intraday learn-ahead edge cases (`requeue_learning_entry`) are documented from code, not tested.
