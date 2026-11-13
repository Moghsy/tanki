# Phase 1 results — 2026-11-13

Status: complete. All streams are integrated, automated checks pass, and the user
confirmed fixture image/colour display, audio playback and terminal restoration.
Phase 2 has not started. The development profile guard remains enabled.

## Implemented

- Collection actor study commands: card rendering and type-answer comparison,
  queue/counts/intervals, answers with timing and custom-data preservation,
  undo/redo, card actions, deck overview/congrats and scheduler upgrade.
- HTML/CSS text layout, hiding and night-mode cascade, lists/tables/ruby/bidi,
  native widgets, answer diff colours, local image decoding/cache and stock-card
  cell snapshots. Terminal controls in card text are filtered.
- Terminal probing, image protocols and live graphics switching; built-in audio
  and optional mpv IPC with autoplay/replay/interrupt rules.
- Deck/overview/review/congrats screens, image zoom, browser escape hatch,
  timers/timebox/auto-advance, help/palette, scoped remappable keys, `review` and
  `decks` commands. OSC 8 links remain outside width-counted buffer symbols.

The shared interfaces were committed before stream work (`5ac8cf9`). Unfinished
core/renderer/TUI worktrees were resumed; completed media/terminal streams were
preserved. Every stream merge received independent lead workspace verification.
The lead reviewed open/backup/relock code and hardened audio media paths and the
Python replay helper's real-profile guard. All collections used for development
and testing were generated fixtures; no real profile was opened or copied.

## Lead verification

```sh
cargo build --workspace --all-features --locked --offline
TANKI_PINNED_PYTHON=/home/emard/.local/share/AnkiProgramFiles/.venv/bin/python \
TANKI_PINNED_PYTHONPATH="$PWD/.tools/pinned-python" \
  cargo test --workspace --all-features --locked --offline -- --nocapture
cargo clippy --workspace --all-targets --all-features --locked --offline -- -D warnings
python3 scripts/test-license-check.py
CARGO_NET_OFFLINE=true python3 scripts/check-licenses.py
python3 scripts/test-study-replay.py
```

All passed. Final clippy finished in 5.92 seconds with no diagnostics. The full
test log is `/tmp/tanki-phase1-workspace-tests.log`. Per-package formatting,
`git diff --check`, and a reverse-application check of Anki patch 0001 also pass.
The only changes in `vendor/anki` remain that checked-in visibility patch.

The phase's exit-test output from the final integrated run:

```text
study_replay: Anki 26.09.4 | 59 answers in lock step, identical queue order and counts at every step | queues new/learning/review: [11, 41, 7] | ratings again/hard/good/easy: [17, 8, 25, 9] | leeches: 1 | type-in cards: 9
study_replay: 34 cards (all service fields), 64 exported revlog rows (all scheduling fields), 24 notes: exactly equal; no clock tolerance or normalization
test replay_matches_python ... ok
```

The replay compares two copies of a closed fixture using the same answers and
timestamps. It uses public card/note services and Anki's dataset export, with no
raw SQL. Calls run early within the same wall-clock second because upstream also
uses its own clock; a crossed second retries a fresh fixture instead of accepting
a tolerance. Answer counts can vary with upstream queue ordering/fuzz, while
each pair of runs must match exactly.

Renderer: 25 unit tests, 10 integration tests and ten cell snapshots. TUI: 57
unit tests, eleven screen snapshots and two real actor/renderer/image integration
tests. CLI: one unit test and ten integration tests. Media: 24 tests. Terminal:
16 tests plus one ignored encoding benchmark. Core study-specific integration:
12 tests plus the live replay. Existing core golden, locking, schema and loopback
sync suites also pass. Licence gate: five tests and 512 active packages; replay
path guards: two tests.

## Manual exit

The user ran:

```sh
target/debug/tanki doctor
target/debug/tanki --base /tmp/tanki-phase1-manual-hnBtXn review 'Study::Media'
```

Checklist: doctor image; fixture images and colours; Space reveal / 4 rate Easy;
`r` tone replay; `z` zoom / Esc close; Ctrl+Q exit and terminal restoration.
The user replied: "Passed: image, audio and terminal restoration work."
The terminal name was not supplied.

To recreate a fresh fixture:

```sh
study_base=$(mktemp -d)
cargo run --offline -p xtask --features fixtures -- study-fixtures --base "$study_base"
target/debug/tanki --base "$study_base" review 'Study::Media'
```

## Corrections and remaining limits

- Linux rodio builds need ALSA development headers and pkg-config, absent from
  the original prerequisite list. README and CI now name/install them.
- The licence gate was checking disabled optional terminal backends included by
  Cargo metadata. It now checks the enabled all-features Linux graph. Its
  conservative expression policy remains; the explicitly reviewed Apache-2.0
  option is selected for dual-licensed `self_cell` 1.3.0. `unicase` remains 2.6.0.
- Dataset export excludes manual scheduling-only logs and zeroes their sync
  USNs. The replay verifies answer-log scheduling fields exactly; it cannot
  establish equality of log sync USNs through that API.
- Image-occlusion snapshots show a missing-mask notice. Compositing and MathJax
  typesetting remain Phase 4 tasks; TTS is reported as unsupported. Full-card
  raster/browser rendering and custom scheduling JS remain later-phase work.
- Key remapping currently uses explicit `--keymap FILE` JSON overrides/null
  unbinding. Automatic TOML config/profile discovery remains Phase 2 work.
- Live mpv playback was skipped because mpv is not installed. Its IPC parsing
  and audio routing have unit tests; the user confirmed the built-in player.
  The optional encoding benchmark was ignored. The Phase 0 desktop 25.09.4
  version-skew test was not rerun with its interpreter variable set.
- The full terminal/multiplexer/SSH matrix, native OSC 8 click behaviour, browser
  launch appearance, and CI on a real runner remain unverified. No claim about
  those follows from the single accepted manual fixture session.
