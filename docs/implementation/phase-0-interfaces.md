# Phase 0 stream boundaries

The lead owns workspace manifests, lockfile, xtask, CLI, plan updates, integration,
and all first-open, backup, and locking code. Streams must not alter these; send a
proposed change to the lead instead.

## Fixed interfaces

Only `tanki-core` imports Anki crates (integration tests under
`crates/tanki-core/tests/` count as tanki-core and may import them).
`CollectionActor::open`, `execute`, `subscribe`, and `close` use the public types
in `src/types.rs`; those types are fixed for Phase 0. Collections live only on the
actor thread. Commands are synchronous request/reply operations; mutation
notifications carry `OpChanges`. Queue answers refer to cached scheduling states;
fetching another queue or mutating invalidates those states. Drop joins the
worker; every backup is joined synchronously. Fixture generation is an actor
command. CLI and xtask use these interfaces, never Anki imports.

`Command::Backup` closes and reopens the collection after writing the backup (see
`backup_and_relock` in `actor.rs`): Anki's backup drops the cross-process file
lock, and only a fresh connection takes it again. Undo history does not survive
a backup. Nothing outside `actor.rs` may build a `Collection` for a profile that
an actor has open, and nothing may open `collection.anki2` with `std::fs` while
it is open, for the same reason.

## Streams

Each stream works in its own Git worktree under `.worktrees/` with its own target
directory, on its own branch, and commits there. At most three Anki builds run at
once.

| Stream | Branch / worktree | Owns | Must not touch |
|---|---|---|---|
| Spike A, sync (lead) | `phase0-spike-a` | `crates/tanki-core/src/sync.rs`, `sync_spike.rs`, the tanki-core manifest | — |
| Spike B, golden renders | `phase0-spike-b` | `scripts/golden-render.py`, `tests/fixtures/golden/`, `crates/tanki-core/tests/golden_render.rs`, `crates/tanki-core/src/fixture.rs`, and only the `render`, `nodes` and `audio` functions of `actor.rs` | everything else |
| Spike C, schema edges (lead) | `main` | `crates/tanki-core/tests/schema_edges.rs`, `safety.rs` | — |
| Spike D, version skew | `phase0-spike-d` | `scripts/version-skew.py`, `crates/tanki-core/tests/version_skew.rs`, `docs/implementation/spike-d-version-skew.md` | all of `crates/*/src` |
| Spike E, terminal doctor | merged | `tanki-term` | — |

A stream that needs a `[dev-dependencies]` entry in `crates/tanki-core/Cargo.toml`
may add it and must say so in its report; the lead resolves lockfile conflicts.

## Python for the spikes

No wheel is downloaded. Both Anki versions are already on this machine:

- 25.09.4 (the desktop version): the interpreter of the installed launcher,
  `~/.local/share/AnkiProgramFiles/.venv/bin/python`, has `anki` 25.9.4.
- Pinned core (the 26.09.4 release tag): built from `vendor/anki` by
  `scripts/build-pinned-python.py` into `.tools/pinned-python` of the main
  checkout; run the same interpreter with `PYTHONPATH` set to that folder.

Always set `PYTHONDONTWRITEBYTECODE=1`, import `anki` only (never `aqt`), and
open collections only inside temp folders. Nothing may read or write anything
under `~/.local/share/Anki2`.
