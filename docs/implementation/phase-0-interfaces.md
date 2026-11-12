# Phase 0 stream boundaries

The lead owns workspace manifests, lockfile, xtask, CLI, plan updates, integration,
and all first-open, backup, and locking code. Agents must not alter these without
sending a proposed change to the lead.

Only `tanki-core` imports Anki crates. `CollectionActor::open`, `execute`,
`subscribe`, and `close` use the public types in `src/types.rs`. Collections live
only on the actor thread. Commands are synchronous request/reply operations;
mutation notifications carry `OpChanges`. Queue answers refer to cached scheduling
states; fetching another queue or mutating invalidates those states. Drop joins
the worker; every backup is joined synchronously. Fixture generation is an actor
command. CLI and xtask must use these interfaces, never Anki imports.

Spike A and B both require tanki-core, so they must run sequentially, each owning
only that crate. Spike C stays with the lead. Spike D also runs after other core
streams if delegated. Spike E owns only tanki-term and can run independently.
Each delegated stream uses its own Git worktree and target directory; at most
three Anki builds run concurrently. Agents commit their work on their branches.

Spike E implements `TerminalDoctor` for a public `SystemTerminalDoctor` struct
with `Default`. The lead wires it into `tanki doctor`. `DoctorOptions` and
`DoctorReport` are fixed in tanki-term. `summary` contains printable diagnostics;
image/probe escapes go directly to the terminal only when appropriate. Non-TTY
execution must exit cleanly without sending queries or image escapes.

Use GPT-6.1 Sol at high effort for demanding streams and GPT-6.1 Terra at medium
for mechanical streams (user-approved replacements for Opus/Sonnet).
