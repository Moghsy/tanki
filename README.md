# tanki

A terminal Anki client in Rust. Phase 0 is in progress; this is not yet a usable client.

Licensed under AGPL-3.0-or-later.

tanki is an unofficial, Anki-compatible client. It is not affiliated with or endorsed by Ankitects. Anki is a trademark of Ankitects Pty Ltd.

Development uses synthetic collections only. Never point development commands at your real Anki profile. See [the implementation plan](docs/PLAN.md).

Prerequisites: Rust 1.97.1, a C compiler, and protoc (31.1 recommended). Install these explicitly; setup does not download executable tools.

```sh
export PROTOC="$HOME/.local/bin/protoc" # or your installed protoc path
cargo xtask setup
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

`cargo xtask setup` initializes the pinned Anki checkout and only its two translation submodules, then applies the checked-in patches idempotently.
