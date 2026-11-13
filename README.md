# tanki

A terminal Anki client in Rust. Phase 0 (foundations and risk spikes) is done; this is not yet a usable client.

Licensed under AGPL-3.0-or-later.

tanki is an unofficial, Anki-compatible client. It is not affiliated with or endorsed by Ankitects. Anki is a trademark of Ankitects Pty Ltd.

Development uses synthetic collections only. Never point development commands at your real Anki profile. See [the implementation plan](docs/PLAN.md).

Prerequisites: Rust 1.97.1, a C compiler, protoc (31.1 recommended), and the ALSA development headers plus pkg-config on Linux (`libasound2-dev` and `pkg-config` on Debian/Ubuntu). Install these explicitly; setup does not download executable tools.

```sh
export PROTOC="$HOME/.local/bin/protoc" # or your installed protoc path
cargo xtask setup
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

`cargo xtask setup` initializes the pinned Anki checkout and only its two translation submodules, then applies the checked-in patches idempotently.

Generate a synthetic profile (all six stock note types, media, three reviews):

```sh
fixture_base=$(mktemp -d)
cargo run -p xtask --features fixtures -- fixtures --base "$fixture_base"
```

Two checks compare against Anki's own Python package and need it on the machine (see `docs/implementation/phase-0-interfaces.md`); plain `cargo test` does not:

```sh
# Regenerate the golden card renders after moving the Anki pin
python3 scripts/build-pinned-python.py --python /path/to/python-with-anki-deps
PYTHONPATH="$PWD/.tools/pinned-python" PYTHONDONTWRITEBYTECODE=1 /path/to/python-with-anki-deps \
    scripts/golden-render.py tests/fixtures/golden/render.json

# Version-skew test against an installed Anki 25.09.4
TANKI_SKEW_PYTHON=/path/to/python-with-anki-25.09.4 PYTHONDONTWRITEBYTECODE=1 \
    cargo test -p tanki-core --test version_skew -- --nocapture
```

Do not run `cargo fmt --all`: it follows path dependencies into `vendor/anki`. Format per package (`cargo fmt -p tanki-core`).
