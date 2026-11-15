# tanki

A terminal Anki client in Rust. Study, add, edit, browse, sync (with a self-hosted server only), import, export and back up your existing Anki profile in place.

Licensed under AGPL-3.0-or-later.

tanki is an unofficial, Anki-compatible client. It is not affiliated with or endorsed by Ankitects. Anki is a trademark of Ankitects Pty Ltd.

See [the implementation plan](docs/PLAN.md). Development and tests use synthetic collections only.

## Installing

Linux release archives (x86_64 and aarch64, glibc 2.35 or newer) contain the binary, man pages and shell completions. Install steps, runtime requirements and the source build are in [docs/install.md](docs/install.md). `tanki completions bash|zsh|fish|elvish|powershell` prints a completion script.

## Using your Anki profile

tanki finds profiles where Anki desktop does: `--base`, then `ANKI_BASE`, then `$XDG_DATA_HOME/Anki2`, then `~/.local/share/Anki2`. A profile is chosen with `--profile NAME`, else the last one used, else the only one. tanki never reads `prefs21.db` and never touches `addons21/`.

- **Close Anki desktop first.** tanki refuses to open a profile that another program has open (exit 20), and refuses when `collection.anki2-wal` exists, which means desktop is running or crashed (exit 25). If you are sure Anki is closed, retry with `--confirm-closed`.
- The first time tanki opens a collection it makes a backup in the profile's `backups/` folder; afterwards it follows Anki's backup schedule and retention. `tanki backup list|create|restore FILE` manages them. Restore replaces the whole collection (a backup is taken first).
- `tanki import deck.apkg`, `tanki export out.apkg [--deck D | --query Q]`, `tanki export all.colpkg`.
- Imports that would force a one-way full sync ask for `--confirm-full-sync` (exit 24).
- Desktop 25.09.4 and tanki (Anki 26.09.4 core) can share a profile; see the notes in `docs/implementation/spike-d-version-skew.md`.

## Sync (self-hosted only)

AnkiWeb's terms exclude third-party clients, so tanki refuses AnkiWeb and syncs only with a server you run. `tanki sync-server --data DIR --users-file FILE` runs Anki's own sync server; see [docs/sync-server.md](docs/sync-server.md).

```sh
tanki sync login --url https://your-server:8080/
tanki sync            # --upload / --download when a one-way full sync must be chosen (exit 21)
tanki sync status
tanki sync auto on    # sync when the TUI opens and closes
```

If the collection synced with another server before (for example AnkiWeb), the first sync with yours is a one-way full sync, and every other device must then use your server too.

## AnkiConnect and custom scheduling

`tanki serve` exposes an AnkiConnect-compatible API on `127.0.0.1:8765` for
card-mining and editor tools. Close Anki desktop first: the server holds the
collection lock until it exits. Ctrl+C closes it cleanly.

```sh
tanki serve
# For a browser extension, allow its exact origin and optionally require a key:
tanki serve --allow-origin chrome-extension://EXTENSION_ID --api-key YOUR_KEY
```

The initial subset covers model/deck discovery, duplicate checks, adding and
updating notes, tags, searching, media, card information and nested `multi`
requests. `guiBrowse` returns matching card IDs. Media accepts base64 `data`
or a local `path`; URL downloads are unsupported. `apiReflect` lists the
supported actions. Request formats from Yomitan and Obsidian-to-Anki are
replayed against a fixture-backed HTTP server in the test suite.

To serve while the TUI runs, add this to tanki's config file:

```toml
[connect]
in_tui = true
bind = "127.0.0.1:8765"
# api_key = "YOUR_KEY"
cors_origins = ["chrome-extension://EXTENSION_ID"]
```

Collection custom scheduling scripts run in QuickJS, including `await`,
`customData`, and Anki's scheduling context. Each card has a one-second
execution limit and a 16 MiB runtime limit. Script failures show a warning
and leave that card's standard scheduling states unchanged.

## Development

Prerequisites: Rust 1.97.1, a C compiler, protoc (31.1 recommended), and the ALSA development headers plus pkg-config on Linux (`libasound2-dev` and `pkg-config` on Debian/Ubuntu). Install these explicitly; setup does not download executable tools.

```sh
export PROTOC="$HOME/.local/bin/protoc" # or your installed protoc path
cargo xtask setup
python3 scripts/cargo-bounded.py build --workspace --all-features
python3 scripts/cargo-bounded.py test --workspace --all-features
python3 scripts/cargo-bounded.py clippy --workspace --all-targets --all-features -- -D warnings
```

`cargo xtask setup` initializes the pinned Anki checkout and only its two translation submodules, then applies the checked-in patches idempotently.

The build wrapper serializes commands, shares one `target/` across worktrees,
and stops/cleans builds that exceed a monitored 12 GiB cache budget. It refuses
an oversized `.worktrees/` (2 GiB budget), without deleting source. Development
and test profiles omit debug symbols and incremental caches. Use
`python3 scripts/cargo-bounded.py --clean` to discard the cache when done.
Direct Cargo commands bypass the size guard; a strict limit against arbitrary
processes requires an operating-system filesystem quota.

Generate a synthetic profile (all six stock note types, media, three reviews):

```sh
fixture_base=$(mktemp -d)
cargo run -p xtask --features fixtures -- fixtures --base "$fixture_base"
```

For interactive study, generate the study fixture instead. It includes new,
learning and review cards, typed answers, visible images and an audible tone:

```sh
study_base=$(mktemp -d)
cargo run --offline -p xtask --features fixtures -- study-fixtures --base "$study_base"
cargo run --offline -p tanki-cli -- --base "$study_base" decks --json
cargo run --offline -p tanki-cli -- --base "$study_base" review 'Study::Media'
# Full deck browser:
cargo run --offline -p tanki-cli -- --base "$study_base"
```

Use Space/Enter to reveal, 1–4 to rate, `r` to replay audio, `u` to undo and
`?` for the active screen's keys. `--graphics=blocks` provides a terminal-cell
image fallback. `--keymap FILE` accepts a JSON array of `{scope, key, action}`
overrides (a null action unbinds a key). Native rendering supports math,
image-occlusion review and animated GIFs. Shift+M toggles occlusion masks;
`v` starts/stops recording your voice and Shift+V replays it. Local TTS uses
speech-dispatcher/espeak-ng (or macOS say); `--tts=piper --piper-model FILE`
uses an installed Piper model. Video needs mpv; `--video-output=window`
is the default, with kitty/sixel/blocks terminal modes available.

Build with `--features blitz` for optional full-card HTML/CSS bitmaps, then
use Ctrl+T during review to save the rendering choice for that note type.
Card JavaScript uses the browser escape hatch. `--generate-latex` opts into
generating missing legacy LaTeX images with installed local tools.
See [Phase 4's fixtures and terminal checks](docs/implementation/phase-4-interfaces.md).

Two checks compare against Anki's own Python package and need it on the machine (see `docs/implementation/phase-0-interfaces.md`); plain `cargo test` does not:

```sh
# Regenerate the golden card renders after moving the Anki pin
python3 scripts/build-pinned-python.py --python /path/to/python-with-anki-deps
PYTHONPATH="$PWD/.tools/pinned-python" PYTHONDONTWRITEBYTECODE=1 /path/to/python-with-anki-deps \
    scripts/golden-render.py tests/fixtures/golden/render.json

# Version-skew test against an installed Anki 25.09.4
TANKI_SKEW_PYTHON=/path/to/python-with-anki-25.09.4 PYTHONDONTWRITEBYTECODE=1 \
    cargo test -p tanki-core --test version_skew -- --nocapture

# Phase 1 exit test: compare an entire study session with the pinned Python package
TANKI_PINNED_PYTHON=/path/to/python-with-anki-deps \
TANKI_PINNED_PYTHONPATH="$PWD/.tools/pinned-python" \
    cargo test --offline -p tanki-core --test study_replay -- --nocapture
```

Do not run `cargo fmt --all`: it follows path dependencies into `vendor/anki`. Format per package (`cargo fmt -p tanki-core`).
