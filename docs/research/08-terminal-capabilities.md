# 08 — Terminal capabilities (2026) and the Rust building blocks

Status: research draft, November 2026. Sources are linked inline; "unverified" marks claims I could not confirm against a primary source.

> Legend used throughout
> - **(i) Solid everywhere**: works in every mainstream terminal, over SSH, inside tmux.
> - **(ii) Capable terminals only**: needs runtime detection and a fallback.
> - **(iii) Physically impossible**: no terminal can do it; design around it.

---

## 0. The local environment: what is `herdr`?

`TERM_PROGRAM=herdr` (v0.9.5 here) is **herdr**, an open-source terminal multiplexer and workspace manager for running several AI coding agents (Claude Code, Codex, Amp, ...) side by side. It is a single Rust binary built on ratatui; a background server keeps panes alive (tmux-like detach/attach, `herdr --remote <ssh-host>`), and a sidebar shows each agent's state (working / blocked / done / idle). Repo: <https://github.com/herdrdev/herdr> (formerly `ogulcancelik/herdr`); docs: <https://herdr.dev/llms-full.txt>.

What it does to the terminal features we care about, from its docs (<https://herdr.dev/llms-full.txt>, sections "Kitty graphics", "Pane graphics", config reference):

| Feature | herdr behaviour |
|---|---|
| Environment | Panes get `TERM=xterm-256color`, `COLORTERM=truecolor`, `TERM_PROGRAM=herdr`. It **strips** outer markers (`KITTY_*`, `WEZTERM_*`, `TMUX`, iTerm2's `LC_TERMINAL`, ...). So env-var sniffing of the real terminal fails inside herdr; we must query. |
| Kitty graphics | **Supported and on by default** (`[terminal] kitty_graphics = true`). Not raw passthrough: herdr *parses* the kitty protocol from the pane, owns placement/clipping/visibility, and re-emits to the outer terminal (inline base64, or temp files on local Ghostty). Overlays crop images. Unicode placeholders are handled (with bugs). |
| Sixel / iTerm2 OSC 1337 | Not mentioned anywhere in docs: assume **not supported**. |
| Kitty keyboard protocol | Forwarded per the modes the pane app requests. |
| OSC 8 hyperlinks | Handled by herdr (Ctrl-click) when mouse capture is on; passed to outer terminal when `ui.mouse_capture=false`. |
| Clipboard images | Its own bridge for `--remote` (Ctrl+V copies the image to a remote temp file and pastes the path). |

Open issues worth knowing (GitHub search, Nov 2026): kitty sequences leak as raw text into Terminal.app (#5295); unicode-placeholder images stretched / aspect lost (#4858); placeholder images missing after pane switch until redraw (#5614); placements surviving app exit (#5705); 32 MB frame cap dropping large images (#3946); no tmux DCS wrapping when the herdr client itself runs inside tmux (#5116); images not shown when attached from iTerm2 (#3941); GPU memory growth on redraw (#5783).

**Takeaway**: herdr is representative of a new class of "smart" multiplexers (herdr, zellij, tmux 3.6+) that interpose on graphics. Our app must (1) detect by query, not env, (2) prefer kitty *with* unicode placeholders, (3) be robust to images silently not appearing, and (4) offer a one-key fallback to character-cell rendering.

---

## 1. Image display protocols

### 1.1 Kitty graphics protocol (the best one)

Spec: <https://sw.kovidgoyal.net/kitty/graphics-protocol/>. kitty is at **0.50.0 (2026-11-03)** (<https://sw.kovidgoyal.net/kitty/changelog/>).

- **Framing**: APC `ESC _G <key=value,...> ; <base64 payload> ESC \`. Terminals that don't understand APC ignore it (mostly; Terminal.app is known to print it, see herdr #5295).
- **Transmission media** (`t=`): `d` direct (in-band base64, chunked with `m=1`… `m=0`, ≤4096 bytes per chunk, all chunks but last multiple of 4) — the only one that works over SSH; `f` file path; `t` temp file (deleted by terminal; path must contain `tty-graphics-protocol`); `s` POSIX shared memory. `f`/`t`/`s` only work when the terminal and app share a machine — big win locally (no base64, no pty bandwidth). Failures uniformly return `EBADF`.
- **Formats** (`f=`): 32 RGBA (default), 24 RGB, 100 PNG; **new in 0.50: `f=8`/`f=16` palette-indexed images that recolour with the theme** (nice for monochrome math glyphs: they'd follow dark/light theme automatically — kitty-only for now). `o=z` zlib compression.
- **Query/detection**: `a=q` with an id; reply `ESC_Gi=<id>;OK`. Send it followed by DA1 (`ESC[c`); if DA1 answers first with no graphics reply → unsupported. **New in 0.50: feature queries** (`e=<action>`) to probe individual features (animation `e=f/a/c`, placeholders `e=T,U=1`, indexed `f=8`).
- **Placement**: `a=T` (transmit+display) or `a=p` (display stored image). `c`/`r` scale to a cell box; `x,y,w,h` source crop in pixels; `X,Y` sub-cell pixel offsets; `p=` placement id — re-sending same `i`+`p` **replaces without flicker**. `C=1` = do not move the cursor (essential for TUIs).
- **z-index**: `z<0` under text; `z < -1073741824` under cells with non-default background too. Lets us draw text *over* images (e.g. labels on image-occlusion masks).
- **Unicode placeholders** (kitty ≥0.28): create a *virtual placement* `a=p,U=1,i=…,c=…,r=…`, then print `U+10EEEE` cells whose foreground colour encodes the image id and whose combining diacritics encode row/column. Because the image is now *text*, it survives multiplexers (tmux, herdr), scrolls/clips/reflows with the grid, and can be partially shown by printing only some cells. This is the mode to use in a ratatui app.
- **Relative placements** (≥0.31): `P=`/`Q=` parent image/placement, `H`/`V` cell offsets; children die with parent.
- **Deletion**: `a=d,d=<x>`; lowercase deletes placements but keeps data (cheap redisplay), uppercase frees data. Targets: all visible, by id, by id range, at cursor, cell, row, column, z-index, frames. Virtual placements are only affected by `i/I/r/R/n/N`.
- **Scrolling**: images scroll with text; `ESC[2J`, reset and switching to the alternate screen (1049) clear images; other erase ops don't. Hence TUIs must explicitly delete stale placements.
- **Animation** (≥0.20): `a=f` frames with per-frame gap (ms), `a=a` play/stop/loop control, `a=c` frame composition. GIFs can be uploaded once and animated by the terminal with zero app CPU.
- **Quota**: kitty ~320 MB per buffer; older images evicted. Must handle re-upload after eviction (`ENOENT`).

### 1.2 iTerm2 inline images (OSC 1337)

`OSC 1337 ; File=inline=1;width=…;height=…;preserveAspectRatio=…: <base64 file> BEL` (<https://iterm2.com/documentation-images.html>). Sends any image file format the terminal can decode (PNG/JPEG/GIF incl. animation in iTerm2). Simple, but: no ids, no placements, no deletion — the image is "printed" into cells, so a TUI must overwrite the cells to remove it and re-send the whole file to redraw. Supported by iTerm2, WezTerm, VS Code (xterm.js image addon), Warp, Tabby, Rio, Bobcat, mintty, Konsole (partial). ratatui-image uses it as the preferred protocol on WezTerm (its kitty impl is buggy) and Rio.

### 1.3 Sixel

DEC's 1980s bitmap format: `DCS q … ST`, six-pixel-tall bands, **palette of up to 256 registers** (often 1024 in xterm with `-ti 340` / numColorRegisters); real-world encoders (libsixel, `icy_sixel`) quantise + dither → photos look OK, gradients band. Large: an uncompressed text stream, often 3–10× the PNG size. No ids or deletion: overwrite cells to remove; drawing on the last row scrolls the screen (ratatui-image #57). Detected by DA1 response containing `4`. Pixel geometry via `XTSMGRAPHICS`. Widest support of any bitmap protocol (xterm, foot, mlterm, WezTerm, Windows Terminal ≥1.22, Konsole, VS Code, Contour, tmux built with `--enable-sixel`, zellij, Black Box, st+patch).

### 1.4 Character-cell fallbacks

All work everywhere (truecolor needed for good results; 256-colour degrades):

| Mode | Glyphs | Effective resolution per cell | Notes |
|---|---|---|---|
| Half blocks | `▀ ▄` fg+bg | 1×2 | Universal, decent for photos. ratatui-image `Halfblocks`. |
| Quadrants | `▖▗▘▝▚▞…` | 2×2 (2 colours/cell) | Unicode 1.1, universal fonts. |
| Sextants | Legacy Computing `🬀…🬻` | 2×3 | Unicode 13; needs font support (most terminals draw them built-in now: kitty, Ghostty, foot, WezTerm). |
| Octants | `𜴀…` | 2×4 | Unicode 16 (2024); built-in in kitty/Ghostty/foot; spotty elsewhere. |
| Braille | `⠁…⣿` | 2×4 (1 colour/cell) | Great for line art/charts, poor for photos. |
| chafa symbol mixing | all of the above + ASCII | adaptive | libchafa ≥1.16 picks best glyph per cell; best quality fallback. |

**chafa** (C library, 1.18.3, 2026-09-23, <https://hpjansson.org/chafa/>) is the de-facto best text-art renderer; ratatui-image now links it by default (`chafa-dyn`, or `chafa-static`). yazi requires chafa ≥1.16 as its last-resort fallback.

### 1.5 Image-protocol support matrix (November 2026)

Confidence: **V** = verified against a primary source (project docs / changelog / release notes) in this research; **S** = secondary sources (yazi / ratatui-image compatibility tables, ucs-detect, terminfo.dev, community posts); **M** = from memory / not re-verified, treat as likely.

| Terminal (latest seen) | Kitty graphics | Kitty unicode placeholders | iTerm2 OSC 1337 | Sixel | Notes |
|---|---|---|---|---|---|
| **kitty** 0.50.0 (2026-11-03) | Yes, reference impl (V) | Yes ≥0.28 (V) | No | No, by policy (S) | 0.50 adds indexed-colour images + feature queries (V). |
| **Ghostty** 1.4.0 (2026-10-27) | Yes; 1.4 claims 100% of spec incl. animation and relative placements (V) | Yes (V, ratatui-image) | No (M) | No (S) | Local machine has 1.3.0 (no animation). No Windows GUI planned (V). |
| **WezTerm** 20240203 (last *stable*; nightlies continue) | Partial/buggy (S: ratatui-image) | Not reliable (S) | Yes, best path (S) | Yes (S) | No stable release since Feb 2024 (V, GitHub releases). yazi and ratatui-image both pick iTerm2 protocol on WezTerm. |
| **iTerm2** 3.6.x / 3.7.x | Yes since ~3.6, still bug-fixing (S: release notes mention kitty-image fixes in 3.6.7) | No (S) | Yes, reference (V) | Yes (M) | macOS only. |
| **foot** | No (M) | No | No | Yes, very good (S) | Wayland only. |
| **Konsole** | Yes, "old" kitty (direct placements) (S: yazi `KgpOld`) | Unclear/likely no (S) | Partial (M) | Yes but buggy clearing (S: ratatui-image "not really fixed in 24.12") | |
| **Alacritty** 0.17.0 (2026-04-06) | No (S) | No | No | No; upstream PR stalled, out-of-tree sixel fork (S) | Still the big "no graphics" terminal. Character-cell fallback or Überzug++ overlay. |
| **Windows Terminal** 1.25.x (2026-11) | No (S: herdr docs "does not expose the Kitty graphics path") | No | No (M) | Yes since 1.22 (V: yazi docs `>= v1.22.10352.0`) | Kitty *keyboard* protocol since 1.24/1.25 (S: MS devblog). |
| **VS Code / Cursor** (xterm.js 6.0.0) | Conflicting; display not confirmed (S) | No | Yes (S) | Yes (S) | Images are behind `terminal.integrated.enableImages` (not on by default per multiple reports, S). `TIOCGWINSZ` pixel size missing (S: yazi). |
| **GNOME Terminal / Ptyxis / VTE** | No (M) | No | No | Compile-time + runtime opt-in (`enable-sixel`), off in most distro builds (S) | Treat as no graphics. Black Box flatpak has sixel enabled (S). |
| **Warp** | Yes on macOS/Linux since 2025 (S) | No (S: ratatui-image "unicode-placeholders not implemented") | Yes but doesn't clear (S) | No (S) | |
| **Rio** 0.5.29 (2026-11-01) | Yes since 0.3.0; unicode placeholders since 0.3.11 (S: release notes) | Yes ≥0.3.11 (S) | Yes but sizing bugs (S: yazi) | Yes, glitchy (S) | Fast-moving; re-test. |
| **Contour** 0.8.0 (2026-10-23) | No (M) | No | No | Yes but "does not clear graphics" (S) | |
| **xterm** | No | No | No | Yes with `-ti 340`/build option (S) | Reference for Sixel and `XTSMGRAPHICS`. |
| **Apple Terminal** (macOS 26 Tahoe) | No (S) | No | No | No (conflicting S; treat as no) | Gained 24-bit colour in Tahoe (S). Known to *print* raw APC payloads (herdr #5295) → never send kitty sequences blindly. |
| **mlterm, Bobcat, Tabby, Black Box, st+patch** | — | — | Bobcat/Tabby: yes | mlterm/Black Box/st: yes | Niche. |

Multiplexers and transports:

| Layer | Kitty graphics | Sixel | Notes |
|---|---|---|---|
| **tmux** 3.8 (2026-09) | Only via `allow-passthrough on` + DCS wrapping (`ESC Ptmux; …`); positioned placements break on pane switch/scroll, **unicode placeholders work** because the image is text (S, V for tmux side) | Native since 3.4 when built `--enable-sixel` (V); 3.6 keeps images across alt-screen, 3.7 raises max images to 20 (V) | tmux 3.6 added mode 2031 theme reporting, 3.7 added DECSET 2026 sync output, 3.8 built-in light/dark themes (V, tmux CHANGES). Local machine has tmux 3.4. |
| **zellij** 0.46.0 (2026-11-11) | Implemented natively since 0.45 (Aug 2026), queried from host at startup, `support_kitty_graphics_protocol false` to disable (S: zellij news) | Since 0.31, quality complaints ("completely broken" per chawan docs up to 0.45) (S) | Re-test; give users an off-switch. |
| **herdr** 0.9.5 | Re-rendered natively, on by default; placeholders supported with open bugs (V: herdr docs, issues) | No (V by omission) | See §0. |
| **GNU screen** | No | No | Treat as character-cell only (M). |
| **SSH** | Works with **direct** (`t=d`) transmission only; file/temp/shm need same host (V spec). Bandwidth is fine for card-sized images (tens–hundreds of KB). | Works | OpenSSH forwards window pixel size in `window-change`, so `TIOCGWINSZ` pixel fields usually survive (M). Query round-trips add latency → detect once at startup with a DA1 sentinel. |
| **mosh** | No: mosh re-synthesises the screen from its own cell model; APC/DCS graphics are dropped (M) | No (M) | Character-cell fallback only. |

### 1.6 Runtime detection recipe

Environment variables are hints only — multiplexers (herdr, tmux) rewrite or strip them. Recommended startup sequence (what yazi-adapter and ratatui-image converge on):

1. Read hints: `TERM`, `TERM_PROGRAM(_VERSION)`, `KITTY_WINDOW_ID`, `WEZTERM_EXECUTABLE`, `GHOSTTY_RESOURCES_DIR`, `WT_SESSION`, `VSCODE_*`, `TMUX`, `ZELLIJ`, `HERDR_ENV`, `SSH_CONNECTION`, `COLORTERM`.
2. In raw mode, write **one batch** of queries and terminate with **DA1** (`CSI c`) as the sentinel (every terminal answers DA1; anything not answered before it is unsupported):
   - kitty graphics: `ESC_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA ESC\` (and on kitty ≥0.50 feature queries `e=…`).
   - `CSI > 0 q` (XTVERSION → terminal name + version, e.g. `kitty(0.50.0)`, `ghostty 1.4.0`, `WezTerm …`, `tmux 3.8`).
   - `CSI 16 t` (cell size in px → `CSI 6;h;w t`) and `CSI 14 t` (text-area px). Fallback: `ioctl(TIOCGWINSZ)` `ws_xpixel/ws_ypixel` ÷ cols/rows (0 on many terminals; absent on Windows/ConPTY, VS Code).
   - `CSI ? u` (kitty keyboard flags), `OSC 11 ; ? ST` (background colour), `CSI ? 2026 $ p` (sync output), `CSI ? 2031 $ p` (theme reports), `CSI ? 5522 $ p` (kitty clipboard paste events), `CSI ? 1016 $ p` (pixel mouse).
   - DA1 reply containing `;4` → sixel available; then `CSI ? 2 ; 1 ; 0 S` (XTSMGRAPHICS) for max sixel geometry.
3. Inside tmux: wrap kitty/sixel queries in tmux DCS passthrough and/or ask tmux for `#{client_termname}`; inside herdr: kitty works, don't try sixel.
4. Timeout: 100–200 ms locally, up to ~1 s over SSH — but the DA1 sentinel normally ends the wait in one RTT.
5. Persist the result per `(TERM_PROGRAM, XTVERSION)` and expose an override (`--graphics=kitty|sixel|iterm|blocks|none`). Users *will* need it.

`ratatui-image`'s `Picker::from_query_stdio()` implements most of steps 2–3 (kitty, sixel, iTerm2, font size, background colour, tmux detection, kitty zlib compression detection as of 11.1) — use it rather than reimplementing, but run it before starting the crossterm event reader.

---

## 2. Rust TUI stack

### 2.1 Core crates (versions from crates.io, Nov 2026)

| Crate | Version (date) | Role / notes |
|---|---|---|
| `ratatui` | 0.30.2 (2026-06-19) | De-facto Rust TUI. 0.30 split into `ratatui-core`, `ratatui-widgets`, `ratatui-crossterm`/`-termion`/`-termwiz`; `no_std`; `ratatui::run()` helper. <https://ratatui.rs/highlights/v030/> |
| `crossterm` | 0.29.0 (2025-04-05) | Default backend; cross-platform incl. Windows. Has kitty keyboard flags (`PushKeyboardEnhancementFlags`), bracketed paste, focus events, SGR mouse, `BeginSynchronizedUpdate`, OSC 52 copy, underline styles/colour. **Missing**: SGR-Pixels (1016) mouse parsing, generic terminal-reply parsing (DA1/kitty/OSC 11), kitty clipboard/DnD. Plan for a small custom input-parser layer or patches. |
| `termion` | 4.0.6 (2025-11) | Unix-only, minimal. No reason to pick it. |
| `termwiz` | 0.23.3 (2025-03) | WezTerm's library: richest escape-sequence model, but tied to WezTerm's slowed release cadence and ratatui-image says its termwiz backend "does not work". |
| `ratatui-image` | 11.1.0 (2026-09-16); 12.0.0-rc.3 (2026-10-17) | Kitty (unicode placeholders, one placeholder per cell since 11.1), Sixel, iTerm2, halfblocks, **chafa** (feature `chafa-dyn` default / `chafa-static`). `StatefulImage` resizes at render time; `ThreadProtocol` (optionally tokio) offloads resize+encode; **11.0 added `SlicedImage`/`SlicedProtocol` for images partially scrolled out of a viewport** (sixel split by 6-px bands, iTerm2 by PNG slices, kitty natively). Limits: sixel on the last row scrolls the screen (#57); sixel/iTerm2 can't be overdrawn by text; terminals that don't clear graphics (Contour, Warp, Alacritty fork) leave ghosts; termwiz backend broken. <https://github.com/ratatui/ratatui-image> |
| `viuer` | 0.11.0 (2025-12) | "Print an image to the terminal" (kitty/iTerm/sixel/blocks). Not TUI-aware; fine for a `tanki show` CLI subcommand. |
| `ratatui-textarea` | 0.9.4 (2026-10-31) | Maintained successor of `tui-textarea` (0.7.0, last release 2024-10). Multi-line editor with undo, search, selection. |
| `edtui` | 0.11.7 (2026-08) | Vim-modal editor widget with syntax highlighting. |
| `tui-input` | 0.15.5 | Single-line input. |
| `tui-tree-widget` | 0.24.1 (2026-08) | Deck tree. |
| `tui-markdown` | 0.3.13 (2026-11-01) | Markdown → ratatui `Text` (pulldown-cmark based). |
| `tui-big-text` / `tui-scrollview` (tui-widgets) | 0.8.10 / 0.6.8 (2026-09) | Big block-letter headings (universal substitute for font sizes); scroll views. |
| `tui-piechart` | 1.4.4 (2026-11-12) | Pie charts. |
| `syntect` | 5.3.0 (2025-09) | Syntax highlighting for code on cards (Sublime grammars). |
| `terminal-colorsaurus` | 1.0.3 (2025-12) | OSC 10/11 fg/bg query with sane timeouts → dark/light detection. |
| `unicode-width` 0.2.3, `unicode-segmentation` 1.13.3, `unicode-bidi` 0.3.18, `icu_segmenter` 2.4.0, `textwrap` 0.16.4 | | Text measurement/wrapping. |

Table virtualisation: ratatui's `Table`/`List` only *draw* visible rows but take the full `Vec` of rows; for a 100k-card browser build only the visible window of rows each frame from your own index (trivial, and the pattern used by large ratatui apps). No dedicated crate needed.

### 2.2 Terminal feature support (beyond images)

| Feature | Escape | Support (Nov 2026) | Crate support | Class |
|---|---|---|---|---|
| Truecolor, bold, italic, underline, reverse, strikethrough | SGR | All modern terminals; Apple Terminal truecolor only since macOS 26 (S); dim is occasionally ignored | crossterm | (i) |
| Underline styles (curly/dotted/dashed) + underline colour | SGR 4:x, 58 | kitty, Ghostty, WezTerm, foot, iTerm2, VTE, Konsole, Windows Terminal, Alacritty, Contour; tmux needs `Smulx`/`Setulc` features (M) | crossterm `Attribute::Undercurled`, `SetUnderlineColor` | (ii) degrade to plain underline |
| Bracketed paste | `?2004` | Universal | crossterm `EnableBracketedPaste` | (i) |
| Focus events | `?1004` | Universal in modern terminals; tmux forwards (3.6 also for popups) | crossterm | (i)/(ii) |
| Mouse (SGR cell) | `?1000/1002/1006` | Universal | crossterm | (i) |
| Mouse (SGR-Pixels) | `?1016` | xterm, foot (≥1.11), Ghostty, kitty, WezTerm(partial), Contour (S) | **not in crossterm** | (ii) |
| Kitty keyboard protocol | `CSI > flags u` | kitty, Ghostty, WezTerm, foot, Alacritty, iTerm2, Rio, Warp, xterm.js/VS Code, Windows Terminal ≥1.24/1.25 (V: kitty docs list + MS devblog); tmux translates to its extended-keys; Apple Terminal/GNOME Terminal (VTE) no (S) | crossterm `PushKeyboardEnhancementFlags`, `supports_keyboard_enhancement()` | (ii) — needed to distinguish Ctrl+Enter / Shift+Enter (flag 8 `REPORT_ALL_KEYS`) and get key-release (flag 2). Never make a core action depend on it. |
| OSC 8 hyperlinks | `OSC 8` | kitty, Ghostty, WezTerm, iTerm2, foot, VTE, Konsole, Windows Terminal, Alacritty, VS Code, tmux ≥3.4 (V for tmux) | manual (ratatui has a hyperlink example; width bookkeeping is fiddly) | (ii) harmless if unsupported |
| OSC 52 clipboard write | `OSC 52` | Most; read is usually disabled for security; tmux 3.7 added opt-in `get-clipboard` (V) | crossterm `clipboard::CopyToClipboard` | (ii) text only |
| Kitty clipboard (MIME incl. image/png) | `OSC 5522` + mode 5522 paste events | kitty (≥0.42 permissions, 0.44.1 paste events) and **Ghostty 1.4** (V) | none — implement | (ii) |
| Synchronized output | `?2026` | kitty, Ghostty, WezTerm, foot, iTerm2, Alacritty, Contour, Windows Terminal, xterm.js; **tmux 3.7+** (V) | crossterm `BeginSynchronizedUpdate` | (ii) harmless if unsupported; always wrap frames. |
| Dark/light detection | `OSC 10/11 ?` query; `?2031` change notifications (`CSI ? 997;1/2 n`) | OSC 11 query: nearly universal; 2031: kitty, Ghostty, iTerm2(S), Contour, tmux 3.6+ (V) | `terminal-colorsaurus` | (ii) |
| Text sizing | `OSC 66` | **kitty ≥0.40 only** for full support; foot reported (S); Ghostty parses but doesn't render (S); nobody else (V spec lists no other implementers) | none | (ii) kitty-only luxury — use `tui-big-text` elsewhere |
| Grapheme clustering mode | `?2027` | Contour, Ghostty, WezTerm (M) | none | (ii) |

**OSC 66 detail**: `OSC 66 ; s=<1-7>:w=<0-7>:n=<0-15>:d=<0-15>:v=:h= ; text ST` renders text in an `s*w × s` cell block, with fractional scales for *smaller* text (good for furigana!). Detection: CPR before/after `OSC 66;w=2; ` and `OSC 66;s=2; `. <https://sw.kovidgoyal.net/kitty/text-sizing-protocol/>. On kitty this gives real big headwords and small ruby text above kanji — a genuinely nicer card on kitty only.

### 2.3 Inline viewport vs alternate screen

| | Fullscreen alternate screen (lazygit/yazi) | Inline viewport, `Viewport::Inline(n)` (Claude Code-like) |
|---|---|---|
| Layout | Full control, panes, browser, stats | Fixed-height region at the bottom, history scrolls up into normal scrollback |
| Images | Clean: alt-screen switch clears kitty images; we own all placements | Works (kitty placements and sixel scroll with text) but images left in scrollback are frozen and may be evicted by kitty's quota; sixel on last row scrolls |
| Resize | Robust | Historically fragile in ratatui (viewport height vs screen height, reflow of scrollback) (M) |
| Review UX | Best for long sessions, big cards, image occlusion, editing | Great for a "quick 10 cards between commits" CLI mode and for leaving a reviewed-card log in scrollback |
| Multiplexers | Fine | Fine |

Recommendation: primary UI = fullscreen alt-screen TUI; secondary `tanki review --inline` that renders each card into the scrollback (essentially a CLI that prints cards and reads a key) — cheaper than a true inline ratatui viewport and plays nicely with tmux/herdr scrollback.

### 2.4 Non-Rust alternatives (for completeness)

| Stack | Strengths | Weaknesses for this app |
|---|---|---|
| Go: Bubble Tea v2 + Lip Gloss v2 + Bubbles v2 (stable 2026-02-23, <https://charm.land/blog/v2/>) | New "Cursed" renderer, kitty keyboard, polished components, easy single static binary | Anki's core (`rslib`) is Rust: would need FFI/IPC to Anki's backend or a reimplementation of scheduler/sync/collection — a large, risky duplication. Image support is DIY. |
| Python: Textual + `textual-image` | Fastest to prototype; CSS-like styling; kitty/sixel via textual-image | Startup time, distribution, performance with large collections; Anki's Python layer (`anki` pylib) is usable but ties to Anki's Python release packaging. |
| TypeScript: Ink/React (Claude Code's lineage) | Familiar React model; Claude Code proves polish is possible | Node runtime, no Anki core, images DIY, heavy memory. |
| **Rust: ratatui + crossterm + ratatui-image** | Links Anki's own `rslib` directly (scheduler, FSRS, sync, DB, media), best image crate ecosystem, Blitz/RaTeX/resvg/typst in-process, single binary | Some protocol gaps in crossterm (pixel mouse, OSC 5522, reply parsing) → small custom layer. |

Rust is clearly best here; the Anki core being Rust dominates everything else.

---

## 3. Rendering Anki's HTML+CSS cards

Anki cards are HTML fragments + note-type CSS (+ optional JS, which AnkiDroid/desktop run). A terminal client needs two renderers:

### 3.1 (a) HTML → styled terminal text (always available)

| Crate | Version | Use |
|---|---|---|
| `html5ever` 0.40.1 / `scraper` 0.29.0 / `kuchikiki` 0.8.2 | 2026 | Spec-compliant parsing + DOM; write our own HTML→ratatui `Text` walker. |
| `html2text` 0.17.4 (2026-10-18) | | Ready-made HTML→text with *rich annotations* (emphasis, strong, strikeout, code, links, images, colours) and an optional `css` feature that applies simple `<style>`/`style=` rules (colour, background, `display:none`). Tables rendered with box borders and column fitting; lists/indentation good. Ruby: no special layout (M). Good baseline; we likely want our own walker for card-specific needs (cloze, `[sound:]`, `type:` answers, furigana). |
| `lol_html` 3.0.3 | | Streaming rewriter — handy to pre-process Anki templates (strip scripts, rewrite media URLs). |
| `ammonia` 4.2.4 | | Sanitiser (strip JS/unsafe attrs) before any rendering. |
| `cssparser` 0.38 / `lightningcss` 1.0.0-alpha.72 | | Parse note-type CSS to extract the few properties a text renderer can honour: `color`, `background-color`, `font-weight/style`, `text-decoration`, `text-align`, `display:none`, maybe `font-size` (→ OSC 66 on kitty or big-text). Selector matching via `selectors`/scraper. |

What maps well: bold/italic/underline/strike, colours (truecolor), lists, blockquotes, `<hr>`, headings (bold/colour/OSC 66), tables (box drawing, CJK width aware), `<pre>/<code>` (syntect), links (OSC 8), `<img>` (→ graphics protocol inline block), cloze highlighting, centring. What doesn't: arbitrary layout (flex/grid/floats/positioning), fonts and sizes (except OSC 66), images *inline within a text line*, ruby above base, JS-driven cards.

### 3.2 (b) HTML+CSS → bitmap without a browser

**Blitz** (DioxusLabs; Stylo + Taffy + Parley + vello/vello_cpu via `anyrender`) — <https://github.com/DioxusLabs/blitz>. Status per README: **beta**, "can already render many popular no-JS websites". crates.io: `blitz-dom`/`blitz-html`/`blitz-paint` **0.3.0-beta.4 (2026-10-16)**, `anyrender_vello_cpu` 0.18.0. No JavaScript (by design). There is an upstream `examples/screenshot.rs` that renders headlessly to an RGBA buffer.

**Spike run for this report** (local, Linux, 16 cores, release build, crates.io 0.3.0-beta.4, `vello_cpu`, 2× DPR, 600 CSS px wide): a Japanese vocab card with `<ruby>`, `<sup>/<sub>`, list, bordered table, flexbox, border-radius:

- parse + style + layout ≈ **85–95 ms** per fresh document; a trivial `<p>hi</p>` still costs ≈ 40–75 ms → it is mostly fixed per-document setup (font collection/UA stylesheet) that should be amortisable by reusing a document/font context (unverified).
- paint to RGBA ≈ **3–4 ms**.
- Binary ≈ **23 MB** (unstripped, release).
- Quality: excellent text rendering, CJK via system fonts, tables/flex/rounded corners correct. **Gaps seen**: `<ruby>` rendered inline (no ruby layout), `<code>` not monospace by default. MathML not supported (M).

Comparison render of the same card via headless Chrome looked equivalent except ruby (Chrome lays out furigana above).

Other no-browser options:

| Option | State 2026 | Verdict |
|---|---|---|
| **Servo** as a library (`servo` crate 0.8.0, 2026-11-07; crates.io releases since early 2026) | Embedding API (`ServoBuilder`, `WebView`, `take_screenshot`) with software rendering; community headless-PNG tools exist (`servo-fetch`) (S). Has JS (SpiderMonkey). API still evolving; very heavy build (SpiderMonkey, ~minutes, large binary). | Interesting future option when cards *need* JS; too heavy as default. |
| **litehtml** (C++; Rust binding `litehtml` 0.2.6, 2026-07) | Mature small CSS2.1-ish engine, you supply the painter (fonts, drawing). No flex/grid. | Workable but Blitz is pure Rust and more modern. |
| **Ultralight** | Proprietary SDK, free tier with revenue limits | Licence-incompatible with an OSS Anki client. |
| **wkhtmltoimage** | Archived/unmaintained (QtWebKit) | Dead. |

### 3.3 (c) With a real browser

Measured locally (Google Chrome 157, same card, 600×400):

- `google-chrome --headless=new --screenshot` cold start: **0.65–1.2 s** per card, ~210 MB peak RSS for the main process.
- **Persistent headless Chrome over CDP** (`Page.setDocumentContent` + `Page.captureScreenshot`): **~45–75 ms per card warm** (first ~140 ms); process tree RSS summed ~1.2 GB (RSS double-counts shared pages; real footprint is a few hundred MB).
- Firefox 160 `--headless --screenshot` failed in this sandboxed run (exit 1); historically ~1–2 s cold (M).

Rust drivers: `chromiumoxide` 0.9.1 (2026-02, async/tokio), `headless_chrome` 1.0.22 (2026-06, sync). WebKitGTK/WPE headless is possible on Linux but adds a large system dependency (M).

Use case: opt-in "exact rendering" mode for complex/JS cards when Chrome/Chromium is present locally; pre-render ahead of time (render card N+1, N+2 while user looks at N) so the ~50 ms is hidden.

### 3.4 (d) What browser-in-terminal projects prove

| Project | Approach | State |
|---|---|---|
| **awrit** (<https://github.com/chase/awrit>) | Chromium (CEF) frames → kitty graphics | Works best in kitty ≥0.28; last release `awrit-native-rs-2.0.3` 2025-08. Proves full-fidelity web content at interactive rates over kitty protocol. |
| **Carbonyl** | Chromium → Unicode cells | Last release v0.0.3, 2023-02: unmaintained. |
| **Browsh** | Headless Firefox → cells | Last release 1.8.3, 2024-01. |
| **chawan** | Own HTML/CSS engine (Nim) → cells + sixel/kitty images | Active; proves a CSS-aware *text-mode* layout engine is feasible; its docs disable images under zellij because zellij's support was broken (up to 0.45). |

Feasibility is proven: bitmaps of web content refresh at tens of fps over kitty's protocol locally; the bottleneck is base64+pty bandwidth over SSH (a 1200×800 PNG card ≈ 50–150 KB → fine per card, not for video).

### 3.5 Recommended tiered card renderer

| Tier | When | Engine | Expected latency/quality |
|---|---|---|---|
| **T0 Text** | Always available; default on non-graphics terminals, over mosh, screen readers, `--plain` | Own html5ever walker (+ html2text ideas), CSS subset, CJK-aware wrapping, images as `[image: name]` or chafa/half-block thumbnails, math via RaTeX→bitmap where graphics exist else `unicodeit` | <1 ms; reads like a good e-mail client |
| **T1 Hybrid** (default on kitty/Ghostty/WezTerm/iTerm2/foot/WT) | Most cards | Text as T0 + **images, math and tables-that-don't-fit as graphics blocks** (ratatui-image; RaTeX for math) | 1–10 ms + image encode; looks native in the terminal theme |
| **T2 Bitmap card** | Cards with heavy CSS (layout, fonts, colours, ruby-free) or user toggles "render as image" | **Blitz → RGBA → kitty/sixel/iTerm2** | ~50–100 ms per card now (pre-render next card), near-browser quality, no JS |
| **T3 Browser** | Opt-in, when Chrome/Chromium found; JS cards | Persistent headless Chrome via `chromiumoxide` | ~50 ms warm, ~1 s cold, +hundreds of MB RAM; exact Anki desktop look |

Bitmaps lose: text selection/copy, theme following (unless re-rendered on OSC 11 change), crispness at odd cell sizes (render at cell-multiple pixel sizes), and they cost bandwidth over SSH.

---

## 4. Math

Anki: MathJax for `\( … \)` / `\[ … \]` (and `[$]…[/$]` legacy LaTeX tags rendered by an external `latex` + `dvipng`/`dvisvgm` into `latex-<sha>.png/svg` files stored in `collection.media` — **those images sync**, so a client mostly just needs to display them).

| Option | Output | Fidelity | Size/speed | Verdict |
|---|---|---|---|---|
| **RaTeX** (`ratex-parser`/`-layout`/`-render`/`-svg` 0.1.15, 2026-10-16) <https://github.com/erweixin/RaTeX> | PNG (tiny-skia), SVG, PDF; display list | KaTeX-compatible syntax + mhchem `\ce`, `\pu`, bussproofs | **Measured: 0.1–0.4 ms per formula warm, ≤7 ms first (mhchem); 6 MB stripped spike binary with embedded KaTeX fonts**; output looked identical to KaTeX | **Primary choice.** Pure Rust, no JS. Gaps: `\includegraphics`, `\html*` trust commands; MathJax-only extensions (some `\require{}` packages) need a fallback. |
| `katex-rs` 0.3.0 (2026-09) | HTML/MathML (Rust port of KaTeX) (M) | KaTeX | — | Useful if we wanted MathML/HTML; still needs a renderer. |
| `katex` 0.4.6 (2023) | HTML via embedded JS (quick-js/duktape) | KaTeX | stale | Skip. |
| `mathjax_svg` 3.2.0 (2025-09) | SVG via MathJax in V8/deno_core | **MathJax-exact** (what Anki uses) | +tens of MB (V8), slower startup | Fallback for formulas RaTeX rejects, maybe as optional feature. |
| `pulldown-latex` 0.8.0 / `math-core` 0.9.0 | MathML only | good | tiny | Only helpful with a MathML-capable renderer (Chrome tier). |
| `mitex` (0.2.7 GitHub, 2026-04; crate 0.2.4) + `typst` 0.15.1 (`typst-render`/`typst-svg`) | LaTeX math → Typst → PNG/SVG | very high typographic quality | large dependency (typst + fonts), tens of ms per render (M) | Strong alternative if we also want Typst features; heavier than RaTeX. |
| Tectonic 0.17.0 | Full LaTeX engine | exact | needs TeX bundle download (hundreds of MB) | Only to *regenerate* legacy `[latex]` images that are missing; normally use synced media. |
| `resvg` 0.50.0 / `usvg` 0.50.0 (2026-11-11) | SVG → RGBA | — | fast, pure Rust | Rasterise RaTeX/MathJax/Anki SVGs, card SVG images. |
| `unicodeit` 0.2.2 (2026-10-27) | LaTeX → Unicode (`\alpha^2` → `α²`) | approximate, inline only | trivial | **Text-tier fallback** for inline math on non-graphics terminals. |
| utftex / libtexprintf (C) | 2-D Unicode art of formulas | readable for fractions/sums | C dependency, no crate | Optional nicety for text tier. |

Practical design: parse MathJax delimiters in T0/T1; on graphics terminals render each formula with RaTeX at `cell_height × k` pixel size, colour = terminal fg (or kitty `f=8` indexed images on kitty ≥0.50 so formulas recolour with theme), and place inline as a graphics block (display math) or approximate inline (inline math must occupy whole cells — baseline alignment within a text line is impossible except via kitty unicode placeholders spanning cells).

---

## 5. Audio, video, TTS, recording

### 5.1 Playback

| Crate | Version | Notes |
|---|---|---|
| `rodio` | 0.23.0 (2026-10-11) | High-level playback (pause, speed, seek) on `cpal`; decodes via Symphonia. |
| `cpal` | 0.18.2 (2026-08) | Low-level I/O: ALSA/PulseAudio/PipeWire (via ALSA or native), CoreAudio, WASAPI. |
| `kira` | 0.12.6 (2026-10) | Game-audio engine (tweens, clocks); overkill. |
| `symphonia` | 0.6.1 (2026-08-13) | Pure-Rust decoders. **Default features are royalty-free only**; per README: WAV/PCM, FLAC, Vorbis, OGG, MKV/WebM, ADPCM on by default; **MP3 (`mp3`), AAC-LC (`aac`), ALAC (`alac`), MP4/M4A (`isomp4`), AIFF, CAF are opt-in features**; HE-AAC via libfdk adapter; **Opus not natively ready** (adapter over libopus). |
| `libmpv2` 7.0.1 (2026-11-07), `mpvipc` 1.3.1 | | In-process libmpv, or control an external `mpv --input-ipc-server` (JSON IPC: `cycle pause`, `seek -5`, `set speed 1.25`, `observe_property time-pos`). |
| `ffmpeg-sidecar` 2.7.0 | | Drive the `ffmpeg`/`ffplay` binaries. |

Anki decks overwhelmingly ship **MP3** (TTS add-ons, Forvo, shared decks), plus OGG/Vorbis, WAV, some M4A/AAC, occasional Opus/WebM and MP4 video. Enable symphonia features `mp3, aac, isomp4, alac` → covers ~all audio; fall back to `mpv`/`ffplay` (if installed) for Opus/odd containers and for video. (`mpv` is not installed on this machine; `ffplay` is.)

### 5.2 Video

- `mpv --vo=kitty` (with `--vo-kitty-use-shm=yes` locally), `--vo=sixel`, `--vo=tct` (truecolor half-blocks) can play inside the terminal (M; mpv docs). Fine for short clips on kitty/Ghostty locally; poor over SSH; herdr has a bug where mpv kitty video blocks input over `--remote` (#5663).
- Simpler and better: if a GUI session exists (`DISPLAY`/`WAYLAND_DISPLAY`/macOS/Windows), open an external mpv/OS-player window; otherwise show a poster frame (ffmpeg thumbnail) + "video not playable here".

### 5.3 TTS (Anki `{{tts lang:Field}}`)

| Option | Notes |
|---|---|
| `tts` crate 0.26.3 (last release 2024-07) | speech-dispatcher (Linux), AVFoundation (macOS), WinRT/SAPI (Windows), web. Works; maintenance slowed. `spd-say` is present locally. |
| espeak-ng (`espeakng` crate 0.2.0, 2023) | Robotic but universal, many languages. |
| Piper neural voices (`piper-rs` 0.2.0, 2026-05) | Good quality offline; ONNX Runtime dependency (large); upstream Piper moved projects in 2025 (M). |
| `sherpa-rs` 0.6.8 (sherpa-onnx: Piper, Kokoro, …) | Same trade-off. |
| `msedge-tts` 0.4.1 | Online Microsoft voices; privacy/ToS concerns. |

Make TTS a pluggable backend: `system` (tts crate) default, `piper` optional feature, `command` (user-supplied shell template).

### 5.4 Recording (Anki "record own voice")

`cpal` capture + `hound` 3.5.1 (WAV) and optional encoding; or shell out to `pw-record` / `arecord` / `ffmpeg -f pulse` (all present locally). Simple and reliable.

### 5.5 Over SSH

Audio plays on the **remote** host's sound device (usually none) — **there is no terminal protocol for audio** (iii). Options to mention to users: run tanki locally and sync instead; PipeWire/PulseAudio network sink forwarded over SSH (`ssh -R` to the local pulse TCP socket); or a future "play on client" via a small local helper. Detect `SSH_CONNECTION` + no audio device and show a clear status-line hint instead of failing silently.

---

## 6. Text shaping limits

| Topic | Reality in 2026 | Strategy | Class |
|---|---|---|---|
| CJK wide chars | Solid; `unicode-width` 0.2 matches terminals for East Asian Wide | ratatui handles; avoid splitting wide chars at borders | (i) |
| Emoji | Mostly fine; ZWJ sequences, VS16 and flags disagree between terminals (ucs-detect scores vary widely) | Prefer simple emoji; mode 2027 where available; never align tables on emoji-heavy text | (ii) |
| Combining marks (Vietnamese, IPA, Devanagari) | Usually fine for Latin combining; Indic conjunct shaping is terminal-dependent (kitty/Ghostty/WezTerm shape with HarfBuzz; many don't) | Accept; offer bitmap tier for Indic-script decks | (ii) |
| RTL / bidi (Arabic, Hebrew) | Terminal-side implicit bidi: VTE/GNOME Terminal (since VTE 0.58), Konsole, mlterm, iTerm2 (3.5+), WezTerm (`bidi_enabled`, opt-in) (S/M). **No bidi** in kitty, Ghostty, foot, Alacritty, Windows Terminal (M). Implicit bidi fights a TUI's absolute positioning. Arabic contextual joining depends on the terminal's shaper. | Do bidi in the app (`unicode-bidi` reorder per line, terminals in "explicit" mode per the freedesktop terminal-wg recommendation); for Arabic decks recommend T2 bitmap rendering | (ii)/(iii) proper shaping on non-shaping terminals |
| Ruby / furigana | No terminal supports it natively. OSC 66 fractional scale on kitty can draw small text above (kitty only). | Default `漢字[かんじ]`/`漢字(かんじ)` inline (Anki's own furigana syntax is `漢字[かんじ]`); optional "ruby line" above the base row with CJK width alignment; bitmap tier via Chrome (Blitz lacks ruby layout today) | (ii)/(iii) |
| Font family / size | Impossible in a cell grid (single font, single size) except kitty OSC 66 scaling | Bold/colour/`tui-big-text` for headings; OSC 66 on kitty; bitmap tier | (iii) except kitty |
| Sub/superscript | Unicode has `⁰¹²³⁴⁵⁶⁷⁸⁹⁺⁻⁼⁽⁾ⁿⁱ` and `₀…₉₊₋₌₍₎ₐₑₒₓₔₕₖₗₘₙₚₛₜ` only | Map where possible, else `^(…)`/`_(…)`; OSC 66 `n/d` on kitty | (ii) |
| Tables | Box drawing universal; width = sum of cell widths; long cells need wrapping | Own table layout with `unicode-width`; fall back to "record view" or bitmap when too wide | (i) |
| Italic | Supported by all modern terminals but depends on font having italics | — | (i) |
| Dim/faint | Mostly supported; some themes make it invisible | Avoid for essential info | (i)/(ii) |

---

## 7. Charts for statistics

- **Built-in ratatui**: `BarChart` (new constructors in 0.30), `Chart` (line/scatter, markers Dot/Block/Bar/Braille/HalfBlock; newer quadrant/sextant/octant markers — M), `Sparkline`, `Gauge`/`LineGauge`, `Canvas` (braille shapes). Enough for review counts, intervals, retention, ease/FSRS difficulty histograms, future-due forecast.
- **Calendar heatmap** (Anki's "review heatmap"): trivial to build with a `Paragraph` of coloured `■`/half-block cells (7 rows × 53 columns) — no crate needed; `tui-piechart` 1.4.4 for card-type breakdowns.
- **Bitmap charts**: `plotters` 0.3.7 (last release 2024-09; stable but slow-moving) with `BitMapBackend` → RGBA → ratatui-image. Gives anti-aliased, Anki-desktop-like charts on graphics terminals at the cost of crispness at odd scales and theme mismatch. `charming` 0.6.0 (ECharts) needs a JS runtime for image output — avoid.
- Recommendation: cell charts everywhere (they look intentional and theme-aware, like btop/lazygit), optional bitmap "high-res stats" view on kitty-class terminals.

---

## 8. Clipboard, file input, pixel mouse

| Need | Option | Works where | Class |
|---|---|---|---|
| Copy text | OSC 52 (crossterm) | Nearly all terminals, through tmux (`set-clipboard on`), over SSH | (i)/(ii) |
| Read text | Bracketed paste (user presses paste) | Universal | (i) |
| Paste **image** locally | `arboard` 3.7.0 (2026-10-15): images on X11, Wayland (data-control), macOS, Windows; `wl-clipboard-rs` 0.9.5; `clipboard-rs` 0.3.5 (rich text/html/files); or shell out to `wl-paste`/`xclip` (both present here) | Local sessions only | (ii) |
| Paste image **over SSH** | Kitty OSC 5522 read `image/png`; with mode 5522 the terminal *tells us* when a paste contains an image | kitty, Ghostty 1.4 only (V) | (ii) |
| Drag-and-drop a file | Most terminals paste the (quoted) path as text → we read the file | Local; remote only if the path exists remotely | (i) locally |
| Drag-and-drop data | kitty DnD protocol (kitty 0.47+, marked "under development"): app advertises accepted MIME types, receives drop data base64 | kitty only (V) | (ii) |
| Pixel-precise mouse (image occlusion rectangles) | SGR-Pixels `?1016` | xterm, foot, Ghostty, kitty, (WezTerm partial) (S) — needs own parser (crossterm lacks it) | (ii); fallback: cell-resolution drag with half-cell handles is adequate since masks can snap to cells |

herdr adds its own bridge (`herdr --remote` Ctrl+V copies a local clipboard image to a remote temp file and pastes its path) — so "accept a pasted file path" handles that too.

---

## 9. Distribution

- **Single binary**: feasible. Pure-Rust stack (ratatui, crossterm, ratatui-image *without chafa*, RaTeX, resvg, Blitz, symphonia, rustls) builds statically. Measured spikes: Blitz renderer ~23 MB unstripped, RaTeX ~6 MB stripped; with Anki's `rslib` expect a 40–70 MB binary (M) — acceptable (cf. herdr ~10 MB, yazi ~15 MB).
- **Problem dependencies**: `libchafa` (C, depends on GLib) — keep behind a feature, prefer `chafa-dyn` on distro packages and pure-Rust half-blocks in static builds; **audio on Linux** — `cpal` links ALSA; a fully static musl build cannot load PipeWire/Pulse ALSA plugins, so ship glibc-dynamic Linux builds (or dlopen ALSA) for working audio; V8 (`mathjax_svg`) and Servo are huge → optional features only; headless Chrome is runtime-detected, never bundled.
- **Cross-platform graphics**: macOS (iTerm2, Ghostty, kitty, WezTerm, Warp; Apple Terminal = text only), Windows (Windows Terminal sixel ≥1.22, WezTerm; ConPTY may filter APC → kitty graphics generally unavailable natively; no `TIOCGWINSZ` pixel sizes → must query `CSI 16 t`), Linux (everything).
- **Tooling**: `cargo-dist` 0.34.0 (2026-11-02): GitHub Releases, shell/PowerShell installers, Homebrew tap, MSI; AUR (`-bin` PKGBUILD), Nix (flake / nixpkgs `rustPlatform`), Scoop/winget manually. `clap_complete` 4.6.11 (bash/zsh/fish/elvish/PowerShell; dynamic completion via `CompleteEnv`), `clap_mangen` 0.3.3 for man pages.
- Anki's `rslib` build needs `protoc` (protobuf) at build time — a CI concern, not a runtime one.

---

## 10. Summary classification

### (i) Solid everywhere (incl. tmux, SSH)
Truecolor + bold/italic/underline/strikethrough, CJK width, box-drawing tables, half-block/quadrant image previews, braille/bar charts and heatmaps, SGR cell mouse, bracketed paste, focus events, OSC 52 copy (mostly), alternate screen, synchronized output (harmless if ignored), text-tier HTML rendering, Unicode-approximated math, local audio playback/recording/TTS.

### (ii) Capable terminals only → detect + fallback
Bitmap images (kitty / Ghostty / WezTerm-iTerm2 / iTerm2 / foot-sixel / Windows Terminal-sixel / Konsole / Rio / Warp; tmux with passthrough or sixel build; herdr kitty-only; zellij 0.45+), GIF animation by terminal (kitty, Ghostty 1.4), bitmap card rendering (Blitz/Chrome), bitmap math (RaTeX), kitty keyboard protocol (Ctrl+Enter, key release), OSC 66 big/small text (kitty only), curly/coloured underlines, OSC 8 links, dark/light change notifications (2031), image clipboard over SSH (OSC 5522: kitty, Ghostty 1.4), drag-and-drop data (kitty DnD), pixel mouse (1016), in-terminal video (mpv `--vo=kitty`), bidi (some terminals; better done in-app).

### (iii) Physically impossible in a terminal
Arbitrary fonts/font sizes and proportional text (except kitty OSC 66), true ruby layout in text mode, sub-cell positioning of *text*, inline images flowing inside a line of text with baseline alignment (images always occupy whole cells), JavaScript-interactive cards without an embedded browser, audio through the terminal connection (SSH/mosh), any graphics over mosh or GNU screen, Arabic shaping on terminals without a shaper, text selection inside bitmap-rendered cards.

---

## 11. Recommendations

1. **Stack**: Rust + `ratatui` 0.30 + `crossterm` 0.29 + `ratatui-image` (track 12.x) + a thin custom escape layer for: terminal-reply parsing, SGR-Pixels mouse, OSC 5522, OSC 66, kitty DnD.
2. **Graphics policy**: prefer kitty **unicode placeholders** (survive tmux/herdr/zellij, clip and scroll correctly); then iTerm2 (WezTerm, VS Code, Warp), then sixel (foot, Windows Terminal, xterm, tmux-sixel), then chafa/half-blocks; per-user override flag; DA1-sentinel detection at startup.
3. **Card rendering**: tiered T0 text → T1 hybrid (default) → T2 Blitz bitmap (opt-in per note type / toggle key) → T3 headless Chrome (opt-in, JS/ruby fidelity). Pre-render the next card(s).
4. **Math**: RaTeX in-process (sub-millisecond); `unicodeit` in text tier; display synced `latex-*.png/svg` media for legacy LaTeX; optional MathJax (`mathjax_svg`) feature for exactness.
5. **Audio**: rodio+symphonia with `mp3,aac,isomp4,alac` features; mpv IPC for video and odd codecs; tts crate with pluggable backends; clear SSH limitation messaging.
6. **UI mode**: fullscreen alt-screen TUI as primary; `--inline`/CLI quick-review mode printing to scrollback.
7. **Test matrix in CI/manual QA**: kitty, Ghostty, WezTerm, foot, Konsole, Windows Terminal, iTerm2, VS Code, Alacritty, GNOME Terminal; each inside tmux 3.4/3.8, zellij and herdr; plus SSH and mosh. ratatui-image's VM screenshot QA is a model to copy.

## 12. Things I could not verify

- Exact kitty-graphics status in VS Code/xterm.js 6.0 (sources conflict), Konsole unicode placeholders, iTerm2 sixel, Apple Terminal sixel (one table says yes; others no).
- OSC 66 in foot (reported by ucs-detect but not checked in foot docs) and whether Ghostty 1.4 renders it (1.4 notes are silent; 1.3.1 parsed only).
- Bidi status of Ghostty/kitty/foot/Windows Terminal (from memory: none).
- mosh/screen graphics behaviour (from memory: dropped).
- Blitz per-document overhead being amortisable (plausible; not measured).
- typst+mitex per-formula latency and binary size (not spiked).
- mpv `--vo=kitty` details (mpv not installed locally).
- terminfo.dev tables are noisy (e.g. mark Terminal.app as supporting kitty graphics/sixel) — used only as weak evidence.

## Sources

- kitty graphics protocol: <https://sw.kovidgoyal.net/kitty/graphics-protocol/>; text sizing: <https://sw.kovidgoyal.net/kitty/text-sizing-protocol/>; keyboard: <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>; clipboard OSC 5522: <https://sw.kovidgoyal.net/kitty/clipboard/>; DnD: <https://sw.kovidgoyal.net/kitty/dnd-protocol/>; changelog: <https://sw.kovidgoyal.net/kitty/changelog/>
- Ghostty 1.4.0 release notes: <https://ghostty.org/docs/install/release-notes/1-4-0>
- tmux CHANGES: <https://raw.githubusercontent.com/tmux/tmux/master/CHANGES>; releases: <https://github.com/tmux/tmux/releases>
- zellij 0.45 news: <https://zellij.dev/news/nested-sessions-kitty-graphics-new-ui/>
- herdr docs: <https://herdr.dev/llms-full.txt>; repo: <https://github.com/herdrdev/herdr>
- yazi image preview: <https://yazi-rs.github.io/docs/image-preview/>
- ratatui-image: <https://github.com/ratatui/ratatui-image> (README + CHANGELOG)
- ratatui 0.30 highlights: <https://ratatui.rs/highlights/v030/>
- Windows Terminal kitty keyboard: <https://devblogs.microsoft.com/commandline/?p=11224>
- Rio releases: <https://newreleases.io/project/github/raphamorim/rio/release/v0.3.0>, <https://newreleases.io/project/github/raphamorim/rio/release/v0.3.11>
- Blitz: <https://github.com/DioxusLabs/blitz> (README, `examples/screenshot.rs`)
- Servo crate exploration: <https://simonwillison.net/2026/Apr/13/servo-crate-exploration/>
- awrit: <https://github.com/chase/awrit>; chawan image docs: <https://chawan.net/doc/cha/image.html>
- RaTeX: <https://github.com/erweixin/RaTeX>
- Symphonia: <https://github.com/pdeljanov/Symphonia>
- Charm v2: <https://charm.land/blog/v2/>
- freedesktop terminal-wg bidi: <https://terminal-wg.pages.freedesktop.org/bidi/recommendation/basic-modes.html>
- ucs-detect results: <https://ucs-detect.readthedocs.io/results.html>; terminfo.dev (weak): <https://terminfo-dev.pages.dev/>
- crates.io API for all crate versions/dates (queried 2026-11-11/12).

Spike code used for measurements lived in the session scratchpad (Blitz `blitz-*` 0.3.0-beta.4 + `anyrender_vello_cpu` 0.18; RaTeX 0.1.15; Chrome 157 via raw CDP from Node 22).
