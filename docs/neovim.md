# Saved code-tour cards in Neovim

Store a saved code-tour reference in a normal note, then open it during review.
tanki handles scheduling; Neovim displays the source and saved explanations.

## Setup

Use the existing `code-tour.nvim` plugin with tanki's companion patches. They
add saved-card review, metadata inspection, Neovim launching, and lazy Claude
connections. Ordinary agent-backed tours remain available. From the tanki checkout:

```sh
python3 scripts/install-code-tour-bridge.py --plugin /path/to/code-tour.nvim --check
python3 scripts/install-code-tour-bridge.py --plugin /path/to/code-tour.nvim
python3 scripts/test-code-tour-bridge.py /path/to/code-tour.nvim
```

The installer checks every patch hunk before writing and refuses incompatible
plugin versions. Restart Neovim after installing. Both patches are retained under `integrations/neovim/` for review and reapplication.

Add this to tanki's config (`$XDG_CONFIG_HOME/tanki/config.toml`, or `--config`):

```toml
[code_tour]
enabled = true
# Use the actual installed executable path if code-tour is not on PATH.
command = ["python3", "/path/to/code-tour.nvim/bin/code-tour"]
nvim_appname = "nvim"
launch_in_herdr = true # create a Neovim reader tab if no editor is running
lazy_claude = true     # start Claude only on an explicit AI request
# Optional: use the same roots as your chosen Neovim profile.
# xdg_config_home = "/absolute/config"
# xdg_data_home = "/absolute/data"
# xdg_state_home = "/absolute/state"

# Optional moved checkout mappings, keyed by repository IDs in tour metadata.
# [code_tour.repositories]
# repo_123 = "/absolute/path/to/current/checkout"
```

Run tanki inside Herdr for automatic Neovim and Claude startup. When no
registered editor exists, opening a card creates a Neovim reader tab in the
same Herdr session and repository. Claude is not started at this point.
You can also use a running Neovim; several editors produce a picker. tanki
remembers the selected live instance for later cards in the session.

`tanki tour doctor` reports capabilities and editors without opening your Anki
profile. CLI opening accepts `--pane SOCKET` when more than one editor exists.
Outside Herdr, start Neovim yourself and switch to it manually; lazy Claude
creation requires the editor to belong to a Herdr pane. Set `lazy_claude = false`
for saved explanations with generation disabled.

## Create a card

Save a tour explicitly through code-tour's normal saved-library workflow.
`tanki tour list --limit 50 --json` shows IDs, titles, save times, repository
labels, and unreadable-record diagnostics without opening project source.
Increase the limit to inspect older entries (maximum 10000).

Close Anki desktop before commands that open a collection. Install the note
type explicitly, then add a card:

```sh
tanki tour install-note-type
tanki tour add --tour TOUR_ID --deck Default \
  --prompt 'How does answering a card update its schedule?' \
  --answer 'Describe the state transition, then inspect the saved tour.'
```

Prompt and answer flags accept plain text, which tanki escapes for storage.
The recommended normal note type has `Prompt`, `Answer`, and `CodeTour`
fields and one forward card. An incompatible existing type with the same
name is reported without overwriting it. Duplicate prompts are refused by
the CLI creation command.

In tanki's add/edit form, select this note type and press **Alt+G** to choose
an existing saved tour. Selecting updates the draft's `CodeTour` field;
save the note normally. The picker offers **Load more** for older entries.
Use the browser's edit form to attach a tour to an existing note as well.

For another note type, add a field named exactly `CodeTour`. Either use the
picker or run `tanki tour attach --note NOTE_ID --tour TOUR_ID`. Attach updates
only that field and refuses conflicting edits. All sibling cards share the
note's reference. Use one forward card per note for different tour prompts.

The stored field is `code-tour://saved/` followed by a 32-character lowercase
hexadecimal ID. A single `<div>`, `<p>`, or `<span>` wrapper is accepted, along
with character entities; arbitrary HTML, extra blocks, commands, and URL
parameters are rejected. Unknown references show a diagnostic while leaving
the card reviewable.

## Review

1. Recall the answer and press Space/Enter to reveal it.
2. Press **Shift+O** (`open-code-tour`) to open the saved tour.
3. Read/navigate in Neovim. Switch back to tanki when finished.
4. Press **Ctrl+O** (`resume-code-tour`), then rate with 1–4.

Both actions appear in the command palette and accept normal keymap overrides.
Opening the tour holds the current card. Auto advance and rating are blocked;
source exploration is excluded from card and active-session timing. Returning
leaves auto advance off. Opening, closing, cancelling, and returning never
record a rating. If the collection changes or the study day rolls over, tanki
refreshes review on return before allowing a rating.

Opening does not need an AI conversation. It clears any historical origin,
reuses unchanged saved explanations, and shows stale/unresolved sections.
Prepared nested calls remain available. It does not automatically regenerate
stale text or start Claude.

Submit a question with the plugin's normal ask action, or explicitly request
an explanation/refresh. On the first submitted request, the reader creates a
Claude pane beside Neovim, waits for Herdr to detect a ready Claude process,
and binds requests and replies to that process and the current tour. Opening
the question composer alone does not start Claude. Later questions and tours
in that reader reuse the connection. Prompts include the relevant current
source and explanation so a fresh Claude session can answer without the
original authoring conversation.

If Claude needs login, repository trust, or another startup approval, complete
it in the reported pane and repeat the request. The integration preserves that
pane and never answers startup prompts or creates duplicates on retries.
Navigating away or replacing the tour during startup prevents a delayed
request from being submitted against the wrong source. A replaced Claude
process is rejected by the existing conversation identity checks.

Generated changes remain live until you explicitly save through code-tour.
Opening uses the latest saved revision of that ID and never overwrites it.

`tanki tour open TOUR_ID --pane SOCKET` opens independently of review and
does not open an Anki collection. `tanki tour check --query 'CodeTour:*'`
checks matching notes and local metadata without opening project source.
CLI operations requiring a collection cannot run beside the TUI holding it;
use the add/edit forms or the API shortcut below while reviewing.

## Optional creation from Neovim

The companion module in `integrations/neovim/lua/tanki` provides
`:TankiAddTourCard`. Add the integration directory to Neovim's runtime path
and initialize it, for example:

```lua
vim.opt.rtp:append('/absolute/path/to/tanki/integrations/neovim')
require('tanki').setup({
  endpoint = 'http://127.0.0.1:8765',
  deck = 'Default',
  -- api_key = 'your configured AnkiConnect key',
})
```

Enable tanki's existing `[connect] in_tui = true` setting and install the note
type before starting the TUI. The command requires `curl`, prompts for the
card text and deck, and adds through tanki's existing AnkiConnect endpoint.
It uses the currently opened saved tour's ID and never saves a live tour or
starts a second collection owner. No browser origin setting is needed for curl.

## Portability and recovery

Normal note sync and `.apkg` export/import preserve the reference. The saved
tour library and source checkout remain local; set up the matching library
and repository mappings on another machine. Matching titles are insufficient:
the saved ID must exist in the selected Neovim profile.

Missing editors, invalid links, stale source, profile mismatches, and process
errors leave the card available for manual review. Source-location validation
remains owned by code-tour.nvim. Use its existing file-mapping workflow for a
renamed file; whole-tour portable bundles and stop-specific links are deferred.

## Synthetic acceptance check

Use only temporary profiles for development:

```sh
mkdir -p /tmp/tanki-tour-demo
python3 scripts/cargo-bounded.py run -p xtask --features fixtures -- \
  study-fixtures --base /tmp/tanki-tour-demo
python3 scripts/cargo-bounded.py run -p tanki-cli -- \
  --base /tmp/tanki-tour-demo --profile tanki-fixture tour install-note-type
```

Use an explicitly saved synthetic tour under temporary Neovim XDG roots and a
temporary tanki config for the remaining create/review flow. Verify actual
pane focus, Shift+O opening, Ctrl+O return, and explicit rating. Automated
tests use generated collections, fake bridges, and headless Neovim fixtures.

The synthetic bridge test can also exercise a built binary and a temporary
Neovim RPC socket:

```sh
python3 scripts/test-code-tour-bridge.py /path/to/code-tour.nvim --tanki target/debug/tanki
```

Herdr launch and Claude lifecycle tests use a simulated Herdr API; they do not
start real model sessions or answer startup approvals.
