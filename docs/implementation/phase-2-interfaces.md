# Phase 2 contracts and ownership

Shared contracts are `tanki-core/src/data.rs`, exported at the crate root,
with `Command::Data` and `Response::Data` as the actor envelope. Existing study
interfaces remain fixed. Only the actor thread owns/touches collections.

The lead owns `data.rs`, `types.rs`, `lib.rs`, `actor.rs`, `safety.rs`, a new
`replacement.rs`, workspace manifests/lockfile, CLI `main.rs`, TUI `app.rs`,
`terminal.rs`, keymap/drawing integration, all docs and exit verification.

Each stream works and commits in its own worktree, edits only its named files,
and uses its own target. Send shared-file changes to the lead. No more than
three Anki builds run at once.

## Safety (lead)

`CollectionActor::open_with_options(profile, OpenOptions)` adds the confirmed
closed WAL override; ordinary `open` keeps the conservative default. WAL and
Linux `/proc` checks precede the schema peek. The override never bypasses
process checks or locks. Backups relock after every library read of the DB.

`CollectionActor::start_data(DataCommand) -> Result<PendingOperation>` enqueues
without waiting; `PendingOperation::try_recv() -> Result<Option<DataResponse>>`
lets UI/CLI poll; `CollectionActor::progress() -> OperationProgress` and
`cancel_operation()` are independent of the actor's command queue.

Incoming full downloads and restores go into a separate temporary profile.
Installation closes the live collection after a completed backup, takes Linux
OFD locks over SQLite's pending/reserved/shared lock bytes on both old and new
files, rejects other `/proc` holders, verifies path identities, and renames only
while both locks are held. Failure to acquire a lock leaves the target intact.
Unsupported platforms refuse destructive replacement until equivalent locking
exists. No database bytes are accessed outside Anki except the schema peek and
approved pre-upgrade copy. Stage media is installed under the replacement lock.

The lead supplies `pub(crate) fn confirm_schema_change(col: &mut Collection,
confirmed: bool) -> Result<()>` for risky operations. It is conservative: a
potential new full-sync requirement needs confirmation. No SQL is used.

## Sync stream

Owns only `tanki-core/src/sync.rs`, new `sync_ops.rs`, `config.rs`,
`tanki-core/tests/sync_phase2.rs`, `tanki-cli/src/sync_commands.rs`, and
`tanki-tui/src/sync_ui.rs`. The lead wires module declarations and callers.

Implement these core functions:

- `sync_ops::execute(col: &mut Collection, command: SyncCommand) -> Result<DataResponse>`
  handles login, status, normal sync and media. Run with a non-normal direction
  is intercepted by the actor, not this function.
- `sync_ops::upload(col: Collection, auth: SyncCredentials) -> Result<()>`
- `sync_ops::download(col: Collection, auth: SyncCredentials) -> Result<()>`
  (called ONLY with a staging collection by the lead).
- `sync_ops::run_server(options: SyncServerOptions) -> Result<()>` (blocking).

Each sync builds an http1-only, no-proxy, no-HTTP-redirect client with the
AnkiWeb-blocking DNS resolver from the spike. Every auth has an explicit
validated endpoint; validate protocol redirects before returning/persisting.
Media sync runs to completion synchronously on the actor, so all errors are
observed; UI polls progress while the actor works. Cancel is shared progress.

`config.rs` owns TOML configuration and 0600 fallback credentials; config must
never read desktop prefs. Expose clean public types/functions and report their
signatures. CLI exposes clap `SyncArgs`, `SyncServerArgs`, and
`run(actor: &CollectionActor, args: &SyncArgs, json: bool, config: &Path) ->
Result<(), Box<dyn Error>>`; `run_server(args: &SyncServerArgs)` similarly.
TUI owns a self-contained sync progress/conflict widget/state, using the fixed
core commands; report integration API to the lead. Credentials must be redacted
in Debug and output. Dependencies go through the lead.

## Packages stream

Owns only new `tanki-core/src/packages.rs`,
`tanki-core/tests/packages_phase2.rs`, `tanki-cli/src/package_commands.rs`, and
`tanki-tui/src/package_ui.rs`. The lead wires module declarations/callers.

Implement:

- `packages::execute(col: &mut Collection, command: PackageCommand) -> Result<DataResponse>`
  handles APKG Import/Export only. Call `crate::actor::confirm_schema_change`
  before schema-risk imports. Use saved import presets when options are None.
- `packages::export_collection(col: Collection, path: &Path, media: bool,
legacy: bool) -> Result<()>` (lead closes/reopens around it).
- `packages::restore_to_stage(path: &Path, stage: &Path,
progress: Arc<Mutex<anki::progress::ProgressState>>) -> Result<()>`; use public
Anki package calls targeting ONLY the staging profile. Never replace the live
collection. Keep staged media separate; the lead installs it under locks.

CLI exposes clap `ImportArgs`, `ExportArgs`, `BackupArgs` (create/list/restore),
and `run_import`, `run_export`, `run_backup`, each taking
`(&CollectionActor, &ArgsType, bool /* json */) -> Result<(), Box<dyn Error>>`.
TUI owns a self-contained package operation widget/state over core commands;
report its integration API. Destructive restore is explicit and confirmed;
ordinary import never silently replaces a collection because of a filename.
