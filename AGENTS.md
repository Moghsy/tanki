# Repository working rules

Read `docs/PLAN.md` section 14 before implementation. Preserve existing changes
in `vendor/anki`; vendor edits must be represented by checked-in patches.
Development and tests use synthetic profiles under `/tmp`, never real Anki data.

## Disk and build artifacts

- Use `python3 scripts/cargo-bounded.py` for Cargo build, test, run and clippy
  commands. The wrapper serializes builds and uses one shared target directory
  for the main checkout and any worktrees. Do not run concurrent Cargo builds
  with separate target directories or switch debug/incremental settings just
  for verification.
- Default development/test profiles omit debug symbols and incremental caches.
  Keep these defaults. A deliberate debugging session may opt in; clean its
  extra artifacts immediately afterwards.
- The wrapper enforces a monitored 12 GiB build-cache budget and a 2 GiB
  `.worktrees` budget. If the build budget is reached, it stops the build and
  clears generated artifacts before returning an error. Do not repeatedly retry
  an over-budget command without reducing the artifact set.
- Do not create worktrees merely to run subagents. Share this checkout and
  assign disjoint files. If the user explicitly needs isolation, retain at most
  one temporary worktree, share the main target directory, and remove it when
  finished only after preserving any uncommitted work. Never delete source or
  another person's work to reclaim disk space.
- Use the wrapper's cleanup mode after a temporary build configuration or when
  the cached artifacts are no longer useful. Never create backup copies of
  `target/`, and never put build artifacts under `.worktrees/`.
- These guards govern repository commands; an OS filesystem quota is required
  for a strict byte ceiling against arbitrary processes that bypass the wrapper.

Format specific packages (`cargo fmt -p tanki-render`), never `cargo fmt --all`,
which follows path dependencies into the vendor checkout.
