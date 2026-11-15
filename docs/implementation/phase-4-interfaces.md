# Phase 4: rich rendering

Implemented 2026-11-14. All agent runs use temporary synthetic profiles. Terminal placement, actual speakers/microphone and platform-specific speech voices still need the manual checks below.

## Rendering

`tanki-render` accepts the same `CardSide` interface. `RenderOptions.tier` selects `Text` (T0/T1, default) or `Bitmap` (T2). Classification suggests a tier and reports browser-dependent content; it does not enable a renderer. Core render results now carry the note type ID and all front/back template sources, so the reviewer classifies a note type once and invalidates that decision after note type changes.

- MathJax `\\(…\\)` / `\\[…\\]` delimiters use conservative Unicode conversion for simple inline math, otherwise RaTeX with embedded glyph paths, rasterized by resvg. Tall inline formulas reserve multiple rows. Unsupported macros preserve their source and show a notice. Dollar delimiters, preformatted code and script/style text stay literal.
- Legacy LaTeX files display normally, with RGB inversion and preserved alpha in night mode. `--generate-latex` explicitly enables Anki's persisted RenderLatex preference. Missing images run through a bounded background queue, using installed `latex` and `dvipng`/`dvisvgm`, the note type's pre/postamble, private temporary directories and `-no-shell-escape`. Generation never blocks the collection actor; only the actor registers completed bytes with Anki's media service. Pending/errors are visible. Disabling generation, collection replacement and shutdown cancel pending work.
- Native image occlusion parses Anki's hidden rendered shape elements. It composites rectangles, ellipses, polygons and text with normalized coordinates, rotation, active/inactive/highlight states, both review modes and CSS custom-property colours/borders. It replaces the stock setup script and canvas. **Shift+M** toggles masks; question/answer transitions and undo retain Anki's scheduling behavior.
- Ruby rows, application-side bidi and nested tables have regression snapshots. GIFs advance using frame delays and disposal-composited frames; the existing event loop redraws them. GIF decoding is capped at 256 frames / 128 MiB. Unsupported text sizing falls back to the ordinary cell grid.
- Fractional OSC 66 sizing displays ruby/smaller author text on kitty 0.40+ identified by the queried terminal version. It preserves cell allocation, hyperlinks and clipping. It is disabled through tmux. Oversized fonts retain T1's existing grid layout.

`ImageSource` adds inverted files, native occlusion overlays and shared RGBA bitmaps. The image worker owns decoding, animation and compositing. Visible image slices preserve cell scale when scrolling, including bitmaps taller than a viewport.

## Optional T2 renderer

Build with `cargo build --offline -p tanki-cli --features blitz`. **Ctrl+T** toggles text/bitmap rendering for the current note type and saves the choice under the profile in tanki's TOML configuration. Without the feature, the control explains how to enable it. Graphics must be enabled; typed answers, hints/details and occlusions retain native interaction.

Blitz (pinned 0.3.0-beta.4) resolves HTML/CSS and paints with the CPU backend on a background worker. The UI shows text while a bitmap is pending and redraws on completion. The reviewer warms the next queued card and the current answer without answering or removing any cards. The cache shares pixels and retains up to eight results / 256 MiB, with a four-job input queue. The canvas is cell-aligned, limited to 4096 pixels wide / 8192 high; longer cards show a notice and remain available through Open in Browser.

Resources are restricted to canonical files beneath `collection.media`, including linked stylesheets, imports and fonts. Network URLs, outside paths and escaping symlinks are refused. Blitz's native SVG feature is disabled because its default resolver bypasses the resource provider. Inline/fetched SVG and generated math use tanki's safe resvg decoder, then become PNG resources. No browser or card JavaScript runs in T2; JavaScript-dependent cards continue to use the browser escape hatch (T3 is Phase 6).

See [phase-4-media.md](phase-4-media.md) for speech, video and own-voice recording APIs.

## Automated exit coverage

- Renderer unit/bitmap tests cover formulas, mhchem, unsupported source, mask geometry/colours/modes, rotation, labels, GIF timing/disposal, alpha-preserving inversion and resource confinement.
- Cell snapshots cover math fallbacks, stock occlusion and text extras; the existing stock golden renders remain byte-identical to pinned Anki Python output.
- Classifier tests cover simple HTML, complex and embedded CSS, scripts/event handlers, browser embeds and the stock IO bridge.
- Core fixtures contain four `Study::Rich` cards and six `Study::Occlusion` cards, in separate decks from the original Phase 1 media session. Core tests cover their generated shape data, modes and media, note type identity and explicit/asynchronous LaTeX generation.
- TUI tests review all six occlusion cards through reveal, rating and undo, decode/composite them to terminal cells, toggle masks, save a note type's T2 preference and scroll a tall bitmap. Media tests use fake subprocesses and a local speech protocol fixture; hardware playback is opt-in.

## Manual terminal check

Generate a disposable profile (the command prints its path):

```sh
phase4_base=$(mktemp -d)
cargo run --offline -p xtask --features fixtures -- study-fixtures --base "$phase4_base"
cargo run --offline -p tanki-cli --features blitz -- --base "$phase4_base" --graphics=blocks review Study::Occlusion
```

Check the six cards: active masks are pink; inactive masks are yellow only in hide-all mode; the answer reveals the active region with a pink outline; Shift+M toggles masks; `u` restores a rated card. Repeat with automatic graphics detection to check placement in the actual terminal.

```sh
cargo run --offline -p tanki-cli --features blitz -- --base "$phase4_base" review Study::Rich
```

Check Unicode inline math, tall fractions, display math/chemistry, the inverted legacy formula, ruby/bidi/table layout and the two-frame GIF. On the flex card, Ctrl+T should switch to a two-column bitmap; the choice should survive restarting the command. `w` opens the existing browser escape hatch. `?` lists current bindings.

For media, use a disposable note containing `{{tts en_US:Front}}` with an installed speech service/engine; check autoplay/replay and interruption. `v` starts/stops a temporary voice recording; Shift+V replays it. An installed mpv is required for video; `--video-output=kitty|sixel|blocks` selects terminal playback and `window` is the default. Check pause/seek, leaving review, editor handover and terminal restoration. No external program or voice model is downloaded by these paths.
