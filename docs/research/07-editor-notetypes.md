# 07 — Note creation/editing and note type management

Source tree studied: Anki v26.08.1 at `/home/emard/projects/open-source/anki` (read-only).
Line numbers refer to that tree. "V" = verified by reading the code; "U" = unverified / from memory / inferred.

Legend for feasibility verdicts: **trivial** / **doable** / **hard** / **physically impossible**.

---

## 0. Executive orientation (read this first)

1. **There are two editors in 26.08.** The default Add/Edit/Browser editor is still the *legacy* one
   (`qt/aqt/editor_legacy.py`, `qt/aqt/addcards_legacy.py`), where Python owns the `Note` object and the
   Svelte page is "just" the field UI. A new *Svelte-owned* editor (`qt/aqt/editor.py` `NewEditor`,
   `qt/aqt/addcards.py` `NewAddCards`, logic in `ts/routes/editor/NoteEditor.svelte`) is behind the
   experiment flag `ExperimentFlag.SVELTE_EDITOR`; Shift inverts the choice
   (`qt/aqt/main.py:1305-1325`, `qt/aqt/browser/browser.py:612-630`). (V) In the new editor nearly all
   Add-Cards logic (defaults, sticky, history, duplicate check, validation, close prompt) lives in TS
   and talks to rslib via generated RPCs — which is good news: it is a ready-made, language-neutral
   spec of the flow tanki must re-implement, expressed only in rslib calls.
2. **What rslib gives us for editing:** `new_note`, `add_note`, `update_notes` (+60-second undo
   coalescing), `note_fields_check`, `defaults_for_adding`, `default_deck_for_notetype`, field
   normalisation (control chars + NFC), sort field/checksum, `strip_html*`, `html_to_text_line`,
   `encode/decode_iri_paths`, `render_markdown` (pulldown-cmark), cloze parsing, media `add_file`,
   `retrieve_url`, `complete_tag`, all notetype CRUD with schema-change handling, change-notetype,
   restore-to-stock, card rendering with `fill_empty`, and IO note add/get/update. (V)
3. **What rslib does NOT give us:** any HTML editing model. Everything about how HTML gets *produced*
   (contenteditable behaviour, `<b>` vs `<strong>`, `<div>` per line, `&nbsp;`, paste sanitising
   `filterHTML`, MathJax `<anki-mathjax>` decoration, cloze-number choice, image resize, IO mask
   drawing and cloze export) is TS (Chromium + Svelte) and must be re-implemented. There is no Rust
   HTML sanitizer used on the editing path (only `ammonia` for markdown/empty-cards). (V)
4. **Stored field HTML is whatever Chromium's contenteditable emitted**, lightly post-processed. There
   is no canonical form. A terminal editor must therefore treat field HTML as an opaque, arbitrary HTML
   fragment and only rewrite what the user actually changed.

---

## 1. Add Cards window

### 1.1 Where the logic lives

| Concern | Legacy (default) | New Svelte editor | rslib |
|---|---|---|---|
| Window, buttons, history menu | `qt/aqt/addcards_legacy.py` | `qt/aqt/addcards.py` (thin) + `ts/routes/editor/NoteEditor.svelte`, `ActionButtons.svelte`, `HistoryModal.svelte` | — |
| Notetype / deck chooser | `qt/aqt/notetypechooser.py` (Ctrl+N, l.70), `qt/aqt/deckchooser.py` (Ctrl+D, l.55) | `ts/lib/components/NotetypeChooser.svelte:61`, `DeckChooser.svelte:61` | `defaults_for_adding`, `default_deck_for_notetype` (`rslib/src/adding.rs`) |
| Sticky fields | `addcards_legacy.py:218-229`, toggle via bridge `toggleSticky` (`editor_legacy.py:487-517`) | `NoteEditor.svelte:229-255, 1150-1157`; F9 per field (`StickyBadge.svelte:29`), Shift+F9 all (`NoteEditor.svelte:1307`) | stored in `Notetype.Field.Config.sticky` (proto `notetypes.proto`) |
| Duplicate / cloze checks | `addcards_legacy.py:315-343`, `editor_legacy.py:712-746` | `NoteEditor.svelte:532-589` | `Collection::note_fields_check` (`rslib/src/notes/mod.rs:597`) |
| Add | `aqt.operations.note.add_note` | `addNote` RPC (`NoteEditor.svelte:591-623`) | `Collection::add_note` (`rslib/src/notes/mod.rs:90`, inner l.373) |

### 1.2 Defaults: `defaults_for_adding` (rslib, V)

`rslib/src/adding.rs:25`

```rust
pub fn defaults_for_adding(&mut self, home_deck_of_reviewer_card: DeckId) -> Result<DeckAndNotetype>
pub(crate) fn default_deck_for_notetype(&mut self, ntid: NotetypeId) -> Result<Option<DeckId>>
```

- If config `addToCur` (`BoolKey::AddingDefaultsToCurrentDeck`, **defaults to true**,
  `rslib/src/config/bool.rs:63`): deck = current deck if not filtered, else home deck of the card
  being reviewed, else Default (id 1); notetype = last notetype used with that deck, else global
  current notetype, else first notetype.
- Else: notetype = global current notetype; deck = last deck that notetype added to (if exists and
  not filtered), else the current-deck fallback chain.
- RPC: `DefaultsForAdding{home_deck_of_current_review_card}` → `DeckAndNotetype` (`notes/service.rs:58`).
- After a successful add, `add_note_inner` records `last_deck_for_notetype`, `last_notetype_for_deck`
  and the global current notetype (`notes/mod.rs:385-388`). So tanki gets "remember last
  deck/notetype" for free by calling `add_note`.
- When the user changes notetype in the chooser, legacy calls `default_deck_for_notetype` and switches
  deck only if one is remembered (`addcards_legacy.py:165-171`).

**Feasibility: trivial.** Call rslib; render two fuzzy pickers (Ctrl+N / Ctrl+D equivalents).

### 1.3 Changing notetype with content in the form (Python/TS, V)

`addcards_legacy.py:176-207` (identical algorithm in `NoteEditor.svelte:1111-1149`): first copy
non-empty fields whose **names match**; then copy remaining non-empty old fields **by position** into
the remaining unfilled new slots; keep tags. Must be re-implemented (≈30 lines). **Trivial.**

### 1.4 Sticky fields (V)

After add, the new blank note copies fields whose notetype field config has `sticky=true` and
always copies tags (`addcards_legacy.py:218-229`; the new editor copies only *non-empty* sticky
values, `NoteEditor.svelte:1150-1157`). Toggling sticky persists to the notetype via
`update_notetype` (no schema change). Toggle-all semantics: if any field sticky, unstick all sticky
ones; else make all sticky (`editor_legacy.py:487-505`). **Trivial.**

### 1.5 History (V)

Last 15 added note ids (legacy `addcards_legacy.py:249-277`, label = `html_to_text_line(", ".join(fields))`
truncated to 30 chars; deleted notes shown disabled; click opens Browser with `nid:`).
Shortcut Ctrl+H (Ctrl+Shift+H on mac). rslib: `html_to_text_line(html, preserve_media_filenames)`
(`rslib/src/text.rs:213`). **Trivial.**

### 1.6 Field checks: `note_fields_check` (rslib, V)

`rslib/src/notes/mod.rs:594-685`. Order of evaluation:

1. Cloze-in-wrong-place check (`field_cloze_check`): for cloze notetypes, a cloze (`{{cN::` with N≠0)
   in a field that is not a cloze field → `FIELD_NOT_CLOZE`; for normal notetypes any cloze →
   `NOTETYPE_NOT_CLOZE`. Cloze fields = fields referenced by `{{cloze:Field}}` on the first front
   template (`notetype/mod.rs:661`).
2. First field (NFC-normalised if `normalize_note_text`), `strip_html_preserving_media_filenames`,
   trimmed empty → `EMPTY`. (A field containing only `<img src=x.jpg>` is **not** empty.)
3. Cloze notetype with no cloze at all → `MISSING_CLOZE`.
4. Duplicate: SHA-1-derived checksum of stripped first field, same notetype, exact stripped-text
   compare → `DUPLICATE`.

Proto enum (`proto/anki/notes.proto:112`): `NORMAL=0 EMPTY=1 DUPLICATE=2 MISSING_CLOZE=3 NOTETYPE_NOT_CLOZE=4 FIELD_NOT_CLOZE=5`.

UI semantics (Python/TS, must re-implement):

| State | While editing | On Add |
|---|---|---|
| DUPLICATE | first field gets red "dupe" background + "Show duplicates" link → Browser search `dupe:ntid,text` (`editor_legacy.py:730-761`) | **allowed** (warning only) |
| EMPTY | — | blocked: "The first field is empty." (IO: "no occlusion created") |
| MISSING_CLOZE | — | yes/no prompt "You have a cloze notetype but have not made any cloze deletions. Proceed?" |
| NOTETYPE_NOT_CLOZE / FIELD_NOT_CLOZE | hint banner | blocked with message |

Add-on hooks `add_cards_will_add_note` / `add_cards_might_add_note` are Python-only. **Trivial.**

Note: a note that would produce no cards still gets card 0 (empty card) — `new_cards_required(..., ensure_not_empty=true)` (`rslib/src/notetype/cardgen.rs:96-117`). (V)

### 1.7 Tags field with completion (V)

- Legacy: Qt `aqt/tagedit.py` line edit; new: `ts/lib/tag-editor/TagEditor.svelte` calling
  `completeTag({input, matchLimit: 500})` (l.71). rslib: `Collection::complete_tag(input, limit)`
  (`rslib/src/tags/complete.rs:9`) — case-insensitive, per `::` component substring regex,
  prefix-ish matches prioritised. Shortcut Ctrl+Shift+T focuses tags (`NoteEditor.svelte:1625`).
- Tags are canonified on save (`canonify_note_tags`, `notes/mod.rs:366`) — case is unified with
  existing tags. **Trivial–doable** (inline completion popup in TUI).

### 1.8 Add shortcut, close prompt (V)

- Add: Ctrl+Enter (and Ctrl+numpad-Enter) (`addcards_legacy.py:127-133`; `AddButton.svelte:12`).
  Terminal note: Ctrl+Enter is **not distinguishable from Enter** in legacy terminal encoding; it needs
  the Kitty keyboard protocol (CSI u) or modifyOtherKeys. Provide a fallback (e.g. Ctrl+S / Alt+Enter
  / `:w`). (U, general terminal knowledge.)
- Escape closes. Close prompt: after saving pending edits, if any field differs from `""`/`<br>`
  (ignoring `<br>` and whitespace) *and* differs from the sticky value carried over from the last
  added note → ask "Discard current input?" (`addcards_legacy.py:370-388`, `editor_legacy.py:763-774`,
  TS `NoteEditor.svelte:458-475`). **Trivial.**

### 1.9 Image-occlusion notetype in Add Cards

If the notetype's `original_stock_kind == IMAGE_OCCLUSION`, the editor shows an IO picker / mask
editor instead of plain fields; on Add it first serialises shapes into the Occlusion field
(`saveOcclusions()`), then uses the **ordinary** `add_note` (legacy `addcards_legacy.py:279-313`,
new `NoteEditor.svelte:612-623`). See §5.

---

## 2. The note editor

### 2.1 Architecture and data flow (V)

- Field UI: `ts/routes/editor/EditorField.svelte` with two inputs per field:
  - **Rich text** `rich-text-input/RichTextInput.svelte` — a contenteditable `<div>` in a shadow root.
  - **Plain text / HTML editor** `plain-text-input/PlainTextInput.svelte` — CodeMirror 5 in `htmlanki`
    mode, toggled with Ctrl+Shift+X (`PlainTextBadge.svelte:20`, `RichTextBadge.svelte:20`).
  Both are bound to one `content` store holding the *stored* HTML.
- Saving: per-field debounce of **600 ms** (`NoteEditor.svelte:417-437`), also forced on blur and on
  `saveNow()` (before Add, notetype switch, close, toolbar actions that need Python).
  - Legacy: bridge message `key:<ord>:<nid>:<html>` → Python runs `editor_will_munge_html` filters
    (`editor_legacy.py:1811-1828`): `""` if html is `<br>` or `<div><br></div>`; strip NUL bytes;
    `decode_iri_paths` (undo the percent-encoding added for display). Then `update_note` (not in add mode).
  - New: same three transformations in TS `transformContentBeforeSave` (`NoteEditor.svelte:385-398`)
    + removes ` data-editor-shrink="true|false"` attributes, then `updateNotes({notes:[note], skipUndoEntry:false})`.
- Loading: field values are passed through `encode_iri_paths` so that `<img src="my pic.jpg">` displays
  (`editor_legacy.py:607-609`, `NoteEditor.svelte:1159-1164`).
- Undo: every save is an `Op::UpdateNote` undo step, but rslib coalesces consecutive updates of the
  same note within 60 s (`rslib/src/notes/undo.rs:62-86`). In-field undo (Ctrl+Z inside the field) is
  Chromium's native contenteditable/CodeMirror history, entirely frontend. (V)

### 2.2 Toolbar functions and shortcuts (V unless marked)

| Function | Shortcut | Implementation | Produced HTML |
|---|---|---|---|
| Bold | Ctrl+B | `editor-toolbar/BoldButton.svelte` (surround, `tagName="b"`; also removes `font-weight` styles/`<strong>`) | `<b>…</b>` |
| Italic | Ctrl+I | `ItalicButton.svelte` (`tagName="i"`) | `<i>…</i>` |
| Underline | Ctrl+U | `UnderlineButton.svelte` (`tagName="u"`) | `<u>…</u>` |
| Superscript | Ctrl+= | `SuperscriptButton.svelte` (`sup`) | `<sup>…</sup>` |
| Subscript | Ctrl+Shift+= | `SubscriptButton.svelte` (`sub`) | `<sub>…</sub>` |
| Text colour (apply last / pick) | F7 / F8 | `TextColorButton.svelte:118-119` | `<span style="color: rgb(r, g, b);">` (U: exact serialisation is Chromium's CSSOM) |
| Highlight colour | (button) | `HighlightColorButton.svelte` | `<span style="background-color: …;">` |
| Remove formatting | Ctrl+R | `RemoveFormatButton.svelte:63` | strips inline formatting |
| Unordered / ordered list | Ctrl+, / Ctrl+. | `BlockButtons.svelte` → `execCommand("insertUnorderedList"/"insertOrderedList")` | `<ul><li>…</li></ul>` / `<ol>…` |
| Indent / outdent (lists only) | Ctrl+Shift+. / Ctrl+Shift+, | `BlockButtons.svelte:43-71` (alerts if not in list) | nested `<ul>` |
| Justify left/center/right/full | (dropdown) | `execCommand("justify*")` | `<div style="text-align: center;">` (U) |
| Cloze, new number | Ctrl+Shift+C | `ClozeButtons.svelte:33-86` | `{{cN::sel}}` with N = max existing + 1 |
| Cloze, same number | Ctrl+Alt+Shift+C | same | N = max existing (min 1) |
| Attach media | F3 | `TemplateButtons.svelte:38` → file dialog → `add_media_file` | `<img src="name.jpg">` or `[sound:name.mp3]` |
| Record audio | F5 | `TemplateButtons.svelte:75` → Qt recorder (wav → `lame` → mp3, `qt/aqt/sound.py:565-591, 859`) | `[sound:rec<ts>.mp3]` (U: exact name) |
| MathJax inline / block / chemistry | Ctrl+M,M / Ctrl+M,E / Ctrl+M,C | `LatexButton.svelte:30-75` | stored `\(…\)` / `\[…\]` / `\(\ce{…}\)` |
| LaTeX / equation / math env | Ctrl+T,T / Ctrl+T,E / Ctrl+T,M | same | `[latex]…[/latex]`, `[$]…[/$]`, `[$$]…[/$$]` |
| Toggle HTML editor (per field) | Ctrl+Shift+X | `PlainTextBadge.svelte` | — |
| Sticky field | F9 / Shift+F9 all | `StickyBadge.svelte:29` | — |
| Fields… dialog | (button) | `NotetypeButtons.svelte` → `fields` bridge / `openFieldsDialog` | — |
| Cards… (card layout) | Ctrl+L | `NotetypeButtons.svelte:24` | — |
| Preview (browser) | Ctrl+Shift+P | `PreviewButton.svelte:25` | — |
| Image occlusion mask editor toggle | Ctrl+Alt+Shift+M | `ImageOcclusionButton.svelte`, `image-occlusion/tools/shortcuts.ts` | — |
| Jump to tags | Ctrl+Shift+T | `NoteEditor.svelte:1625` | — |
| Help | F1 | `HelpButton.svelte:13` | — |
| History (add mode) | Ctrl+H | `HistoryButton.svelte:17` | — |

Cloze numbering detail: the regex is `/\{\{c(\d+)::/gu` over **all** fields' current HTML
(`ClozeButtons.svelte:33-56`). It ignores the multi-ordinal syntax `{{c1,2::…}}` that rslib's tokenizer
now supports (`rslib/src/cloze.rs:58-80`), so the button can under-count if a note uses `c3,4`. The
cloze button is only enabled in fields that are cloze fields of a cloze notetype (`ClozeButtons.svelte:79-83`).
There is **no `next_cloze_number` in rslib**; rslib only has `cloze_numbers_in_string(html) -> HashSet<u16>`
(`cloze.rs:456`) and `cloze_number_in_fields` (`cloze.rs:482`, RPC `ClozeNumbersInNote`). tanki should use
`cloze_number_in_fields(...).max()+1` (handles multi-ordinals correctly). Wrapping preserves surrounding
whitespace: leading/trailing whitespace of the selection stays outside the braces, `&nbsp;` treated as
whitespace (`ts/lib/tslib/wrap.ts:6-14`).

**Bug spotted (V, harmless):** MathJax block wrap uses closing tag `</anki-matjax>` (typo,
`LatexButton.svelte:38`); the HTML parser auto-closes the element so it works by accident.

### 2.3 MathJax decoration and the overlay editor (V)

Three states (`ts/lib/editable/decorated.ts:4-9`): **stored** `\(\alpha\)` → **undecorated**
`<anki-mathjax>\alpha</anki-mathjax>` (what the HTML editor shows) → **decorated** (rendered SVG image in
the rich editor). Conversion regexes in `ts/lib/editable/mathjax-element.svelte.ts:13-66`:

- stored→undecorated: `\[(.*?)\]` → `<anki-mathjax block="true">…`, `\((.*?)\)` → `<anki-mathjax>…`
  (only when `renderMathjax` config is true).
- undecorated→stored: `<anki-mathjax[ block="..."]>(.*?)</anki-mathjax>` → `\[…\]` if block attr present
  and not "false", else `\(…\)`. `<br>` inside are converted to `\n` and leading/trailing newlines trimmed.

So **stored MathJax is always plain `\(…\)` / `\[…\]` text**, possibly containing literal `\n` and HTML
entities (`&lt;` for `<`). Clicking a formula opens `mathjax-overlay/MathjaxOverlay.svelte` (CodeMirror
popup; Enter accepts, Shift+Enter newline, buttons inline/block/delete). In a terminal: edit the TeX
source directly (it is already plain text in storage). Preview via rendering MathJax → image is hard
(needs a JS engine or external `mathjax-node`/`latex`→PNG); rslib does **not** render MathJax.
LaTeX (`[latex]`) images are generated by Python/`latex`+`dvipng` in aqt (`rslib/src/latex.rs` only
extracts/names them). **Feasibility:** inserting delimiters — trivial; live MathJax preview — hard
(external renderer) — show raw TeX instead.

### 2.4 Rich text vs plain text defaults, collapse, per-field options (V)

From `NotetypeField.Config` (`proto/anki/notetypes.proto`): `plain_text` (field opens in HTML editor and
rich editor hidden: `NoteEditor.svelte:175-186`), `collapsed` (field collapsed by default), `font_name`,
`font_size` (editor only), `rtl`, `description` (placeholder shown via CSS `::before` when empty,
`RichTextInput.svelte` style), `sticky`, `exclude_from_search` (search only), `prevent_deletion`,
`tag` (IO/cloze field identity), `id`. Session-level collapsed/hidden state is remembered per notetype
in memory only. Font/size are meaningless in a terminal (could map size to nothing); RTL needs bidi
support in the TUI text widget (**hard** for proper bidi editing; display via unicode-bidi crate is
doable).

### 2.5 Images in the editor (V)

- `<img src="file.jpg">` with **unencoded** filename in storage (`text.rs:557-575` comment: "Anki
  unfortunately stores the references in unencoded form"). Display path: `encode_iri_paths`.
- Resize handles (`image-overlay/ImageOverlay.svelte:186-219`): sets the `width` **attribute** only
  (keeps aspect ratio, clears `style.width/height`): `<img src="a.png" width="250">`. "Restore" removes
  the attribute. Float buttons set `style="float: left;"` (`FloatButtons.svelte`).
- "Shrink images" (`shrinkEditorImages`, editor-only CSS; attribute `data-editor-shrink` stripped on save).

### 2.6 Paste, drop and HTML sanitising (V)

Two implementations: legacy Python (`editor_legacy.py:1056-1120`, `EditorWebView._processMime` l.1650-1748)
and new TS (`rich-text-input/data-transfer.ts`). Same algorithm:

1. **Mode**: `extended = !(PASTE_STRIPS_FORMATTING xor Shift)` (`editor_legacy.py:1586-1592`,
   `data-transfer.ts:66-74`). Config `pasteStripsFormatting` defaults false, so a plain paste keeps
   (a filtered subset of) formatting and **Shift+paste strips** — the reverse when the option is on.
2. **Source priority**: HTML clipboard flavour first (marked *external*). Else if first URL is
   `file://`: urls → image → (audio file) → text; else image → urls → text (these are *internal*).
3. **Pre-filter** (`_pastePreFilter` / `runPreFilter`): external HTML — drop `script/iframe/object/style`,
   `p`→`div`; every `<img src>`: `http(s)/ftp/file` URL → download into media (`retrieve_url`, 30 s
   timeout, UA "Mozilla/5.0 (compatible; Anki)", extension from MIME); `data:image/(jpg|jpeg|png|gif);base64`
   → write `paste-<sha1>.<ext>`; internal pastes rewrite `http://127.0.0.1:<port>/x` → `x`.
4. **Filter** `filterHTML(html, internal, extended)` (`ts/lib/html-filter/index.ts:35`):
   - *Basic* (strip): keep only `br`, `img[src,alt]`, `div`, `sub`, `sup`; `p`→`div`; remove
     title/script/iframe/object/style; other elements **unwrapped**; all whitespace runs collapsed to one
     space; comments removed.
   - *Extended*: additionally `a[href] b blockquote code dd dl dt em font[color] h1-3 i li ol pre rp rt ruby
     span[style] strong table td/th[colspan,rowspan] tr u ul`; span styles filtered to
     `color`, `background-color` (≠transparent), `font-weight`, `font-style`, `text-decoration-line`
     (night mode drops colours) (`element.ts:55-96`, `styling.ts`).
   - *Internal* (copy within Anki fields): keep everything except `font-size`, `font-family`,
     `width/height/max-*`, `vertical-align` (non-img) styles.
5. **Plain text** (`_processText` / `processText`): HTML-escape, tabs→4 spaces, runs of spaces → first
   space normal then `&nbsp;` for the rest… precisely `" ( +)"` → `"&nbsp;…&nbsp; "`, newlines → `<br>`;
   in extended mode bare URLs to media are downloaded and `data:image` tokens become images; other URLs
   become `<a href>`.
6. **Clipboard image** → `paste-<sha1>.png` (if `pasteImagesAsPng`) or `.jpg` (quality 80; PNG quality 50)
   (`editor_legacy.py:979-997`).
7. Inserted with `execCommand("inserthtml")`.

Drop: same pipeline; in IO picker state a dropped image starts the mask editor (`editor_legacy.py:1621-1648`).

**Bug spotted (V):** in the new editor `add_media_from_url` returns the *requested* filename, not the name
actually chosen by `MediaManager::add_file` (`rslib/src/backend/media.rs:20-27` discards the result of
`add_file`). Combined with the lowercasing below, a pasted remote `Foo.PNG` would be saved as `foo.png` but
referenced as `Foo.PNG`. Legacy path is unaffected (Python uses `write_data`'s return). tanki must use the
returned name from `add_file`.

### 2.7 Media add semantics (rslib, V)

`MediaManager::add_file(desired_name, data) -> Result<Cow<str>>` (`rslib/src/media/mod.rs:66`):
`normalize_filename` (NFC, strip disallowed chars, nbsp→space, Windows device names, length cap) then
`add_data_to_folder_uniquely` (`rslib/src/media/files.rs:167-209`):
same-hash file exists → reuse; **if the name contains uppercase it retries with the lowercased name**;
new file → written; name clash with different content → `stem-<sha1>.ext`.
**Surprise:** new media filenames are lowercased (`Photo.JPG` → `photo.jpg`). Always use the returned name.
RPCs: `AddMediaFile{desired_name,data}`, `AddMediaFromPath{path}` (`media/service.rs:38-56`, on
`Collection`), `AddMediaFromUrl` (on `Backend`, not `Collection`; with direct linking call
`anki::editor::retrieve_url(url).await` + `add_file`).

Link generation (Python `fnameToLink` `editor_legacy.py:926-933`, TS `filenameToLink` `data-transfer.ts:168-178`):
image extensions (`jpg jpeg png gif svg webp ico avif`) → `<img src="…">`; audio/video list
(`3gp aac avi flac flv m4a mkv mov mp3 mp4 mpeg mpg oga ogg ogv ogx opus spx swf wav webm`) →
`[sound:name]` (HTML-escaped without quote escaping) and auto-plays; else (TS) bare filename.
Note the TS version `encodeURI`s the src at insert time (then decoded on save); Python `urllib.parse.quote`s
then decodes on save — net stored value is the raw name.

### 2.8 What ends up stored — canonical examples

These follow from the code paths above plus Chromium contenteditable defaults. Items marked (U) are Chromium
behaviour I did not execute here; they match long-standing observed Anki data.

| User action | Stored field HTML |
|---|---|
| Type `hello` | `hello` (trailing `<br>` added on load for inline content is removed on save, `rich-text-input/transform.ts:19-41`) |
| Type `a`, Enter, `b` | `a<div>b</div>` (U: Chromium default paragraph separator) |
| Type `a`, Shift+Enter, `b` | `a<br>b` (U) |
| Two spaces between words | `a&nbsp; b` or `a &nbsp;b` (U: Chromium) |
| Clear a field that had content | `""` (munge rule maps `<br>` / `<div><br></div>` → `""`) |
| Bold "x" | `<b>x</b>` |
| Colour | `<span style="color: rgb(255, 0, 0);">x</span>` (U exact form) |
| List | `<ul><li>a</li><li>b</li></ul>` |
| Attach `My Pic.PNG` | `<img src="my pic.png">` (lowercased by rslib, space unencoded) |
| Resize image | `<img src="my pic.png" width="250">` |
| Attach audio | `[sound:clip.mp3]` |
| Paste plain text `x<y` | `x&lt;y` |
| MathJax inline | `\(\frac{1}{2}\)`; with `<`: `\(a &lt; b\)` |
| MathJax block, multi-line | `\[a\\\nb\]` (literal newline inside) |
| Cloze | `{{c1::Paris}} is the capital of {{c2::France::country}}` |
| HTML editor edits | stored exactly as typed except `script`/`link` tags removed (`plain-text-input/remove-prohibited.ts`) and re-serialised through the browser DOM (attribute quoting normalised, entities normalised) |

Things a terminal editor must **preserve byte-for-byte when not edited**: arbitrary legacy markup
(`<font color>`, `<span style=…>`, tables, `<a>`), entities (`&nbsp;`, `&amp;`), `[sound:]`, `\(…\)`,
cloze markup including hints and `c1,2`, IO cloze strings, `<!-- comments -->`, unknown attributes,
`[[type:…]]`-like text, and raw newlines (which display as nothing in HTML but are preserved).
rslib only normalises **control characters other than `\n`/`\t`** (removed) and **NFC**
(`normalize_field`, `notes/mod.rs:290-298`; NFC only if `normalize_note_text`, default true).

### 2.9 Field-related helper verdicts

| Feature | Verdict | Approach |
|---|---|---|
| Bold/italic/underline/sub/sup | doable | In the markup projection: `**`, `*`, `__`/`<u>`, `^{}`/`_{}` or literal tags; render with terminal SGR bold/italic/underline |
| Text colour / highlight | doable | Insert `<span style="color: …">`; render with truecolor SGR |
| Remove formatting | doable | Strip inline tags inside selection on the DOM-ish tree |
| Lists / indent | doable | Markdown `- ` / `1. ` projection |
| Alignment | doable (rare) | raw HTML token |
| Cloze insert (both semantics) | trivial | `cloze_number_in_fields` + wrap |
| Attach media | trivial | path prompt with completion → `add_file` |
| Record audio | doable | spawn `ffmpeg`/`arecord`/`pw-record`, or `cpal` + encoder |
| MathJax/LaTeX insert | trivial | wrap with delimiters |
| MathJax live preview | hard | external renderer → image via Kitty; else raw |
| HTML editor toggle | trivial | raw-HTML mode (§6 B) |
| Per-field font/size | physically impossible (font) / ignore | terminal has one font |
| RTL | hard | bidi-aware editing widget |
| Description placeholder | trivial | dim placeholder text |
| Collapse field | trivial | fold |
| Image resize handles | doable | numeric width prompt; mouse-drag only with pixel mouse + graphics |
| Paste HTML from clipboard (sanitised) | doable | read `text/html` via wl-paste/xclip/arboard, port `filterHTML` to Rust (`html5ever`/`scraper` or `ammonia` with custom allow-lists) |
| Paste image | doable | wl-paste/xclip `image/png` or arboard → `paste-<sha1>.png` |
| Drag-drop file | doable | terminal pastes path (bracketed paste) → detect existing path / `file://` URI |
| Remote image fetch | trivial | `anki::editor::retrieve_url` |
| In-field undo | doable | own undo stack in TUI editor (tui-textarea has one) |
| Note-level undo | trivial | rslib undo (`col.undo()`), coalescing already handled |
| Preview card | doable | `render_uncommitted_card` + the tanki HTML→terminal renderer |
| Duplicate highlighting | trivial | `note_fields_check` on debounce |

---

## 3. Rust-side helpers relevant to editing (all V)

| Function | Location / signature | Notes |
|---|---|---|
| `Notetype::new_note` | `rslib/src/notetype/mod.rs:128` `pub fn new_note(&self) -> Note` | random base91 guid, empty fields |
| `Collection::add_note` | `notes/mod.rs:90` `pub fn add_note(&mut self, note: &mut Note, did: DeckId) -> Result<OpOutput<usize>>` | canonify tags, `prepare_for_update`, card gen (template deck overrides honoured), records last deck/notetype |
| `add_notes` | `notes/mod.rs:94` `(&mut self, requests: &mut [AddNoteRequest])` | bulk, single undo op |
| `update_note` | `notes/mod.rs:393` `pub fn update_note(&mut self, note: &mut Note) -> Result<OpOutput<()>>` | no-op if unchanged (`note_differs_from_db`), regenerates cards |
| `update_notes_maybe_undoable` | `notes/mod.rs:397` `pub(crate)`; public via `NotesService::update_notes(UpdateNotesRequest{notes, skip_undo_entry})` | bulk edit |
| `Note::prepare_for_update` | `notes/mod.rs:193` `pub(crate)` | field count check, `normalize_field`, sort field = `strip_html_preserving_media_filenames(field[sort_idx])`, checksum = first 4 bytes SHA-1 of stripped field 0 |
| `note_fields_check` | `notes/mod.rs:597` `pub fn note_fields_check(&mut self, note: &Note) -> Result<NoteFieldsState>` | see §1.6 |
| `defaults_for_adding` | `adding.rs:25` `pub` | `default_deck_for_notetype` is `pub(crate)`; use `NotesService::default_deck_for_notetype` |
| `Note::set_field` / `fields_mut` | `notes/mod.rs:63, 182` `pub` | |
| `strip_html` | `text.rs:227` `pub fn strip_html(html: &str) -> Cow<str>` | regex tag strip + entity decode, nbsp→space |
| `strip_html_preserving_entities`, `decode_entities` | `text.rs:231, 235` `pub` | |
| `strip_html_preserving_media_filenames` | `text.rs:361` `pub` | replaces media tags with ` fname ` |
| `html_to_text_line` | `text.rs:213` `pub fn html_to_text_line(html: &str, preserve_media_filenames: bool) -> Cow<str>` | `<br>`/`<div>`/`\n`→space, drops `[[type:]]`, sound tags |
| `strip_html_for_tts` | `text.rs:255` `pub` | |
| `is_html`, `contains_media_tag` | `text.rs:209, 367` `pub` | |
| `extract_media_refs`, `replace_media_refs` | `text.rs:280, 315` | (`replace_media_refs` pub) |
| `encode_iri_paths` / `decode_iri_paths` | `text.rs:559/566` **`pub(crate)`**; public via `CardRenderingService::{encode,decode}_iri_paths` | |
| `render_markdown` | `markdown.rs:7` **`pub(crate)`** (pulldown-cmark, no extensions); public via `CardRenderingService::render_markdown{markdown, sanitize}` (sanitize = ammonia, no images) | Not suitable as tanki's projection renderer (lossy, no tables/strikethrough, drops images if sanitised) — use pulldown-cmark/comrak directly |
| `sanitize_html_no_images` | `text.rs:376` `pub(crate)` ammonia | |
| Cloze: `cloze_numbers_in_string`, `add_cloze_numbers_in_string`, `cloze_number_in_fields` | `cloze.rs:456, 477, 482` `pub` | supports `c1,2` |
| `contains_cloze` | `cloze.rs:449` `pub(crate)` | ignores c0 |
| `reveal_cloze_text`, `extract_cloze_for_typing`, `parse_image_occlusions` | `cloze.rs:385, 416, 361` `pub` | |
| `retrieve_url` | `editor.rs:16` `pub async fn retrieve_url(url: &str) -> Result<(String, Vec<u8>)>` | the entire content of `editor.rs` |
| `MediaManager::add_file` | `media/mod.rs:66` `pub` | lowercases; returns chosen name |
| `complete_tag` | `tags/complete.rs:9` `pub` | |
| `render_uncommitted_card` | `notetype/render.rs:72` `pub fn render_uncommitted_card(&mut self, note: &mut Note, template: &CardTemplate, card_ord: u16, fill_empty: bool, partial_render: bool)` | for Add-window preview and card-layout preview |
| `field_is_empty` (card gen) | `template.rs:543` `pub(crate)` | only whitespace and empty `<br>`/`<div>` tags count as empty (`&nbsp;` is NOT empty) |

There is **no** HTML→Markdown or Markdown→HTML round-trip utility, no HTML sanitiser for pasting, no HTML
pretty-printer and no next-cloze helper in rslib. Field "normalisation on save" is limited to control-char
removal + NFC.

Visibility caveat (U): with direct crate linking, `pub(crate)` helpers are reachable only through the
generated `services::*Service` traits implemented on `Collection` (`rslib/src/services.rs` includes
`OUT_DIR/backend.rs`). Verify at build time that those traits are `pub` and importable from the `anki` crate.

---

## 4. Notetype management

### 4.1 Where the logic lives (V)

| Screen | UI (re-implement) | Logic |
|---|---|---|
| Manage Note Types | `qt/aqt/models.py` (Qt) | list with use counts: `NotetypesService::get_notetype_names_and_counts`; add/rename/delete/options |
| Add notetype (stock / clone) | `models.py:166-187, 257-311`, `pylib/anki/stdmodels.py:29-60` | stock: `get_stock_notetype_legacy(kind)` (rslib `notetype/stock.rs`); clone = **Python** deep-copy with name "X copy", `id=0`, `originalId=None` (`pylib/anki/models.py:259-271`); then `add_notetype` |
| Fields editor | `qt/aqt/fields.py` (Qt) | field list mutated in Python (`pylib/anki/models.py:291-334`), saved via `update_notetype_legacy`; all schema logic in rslib |
| Card templates editor | `qt/aqt/clayout.py` (Qt, 1019 lines) | preview via `render_uncommitted_card`; save via `update_notetype_legacy`; restore via `restore_notetype_to_stock` |
| Change notetype | `qt/aqt/changenotetype.py` + `ts/routes/change-notetype/*` | `get_change_notetype_info`, `change_notetype` (rslib `notetype/notetypechange.rs`) |
| Empty cards | `qt/aqt/emptycards.py` | `get_empty_cards` (`notetype/emptycards.rs`), then `remove_cards` |

### 4.2 Data model (proto `proto/anki/notetypes.proto`, Rust `rslib/src/notetype/mod.rs:84`)

`Notetype { id, name, mtime_secs, usn, fields: Vec<NoteField>, templates: Vec<CardTemplate>, config }` — all `pub`.
- `config`: `kind` (NORMAL/CLOZE), `sort_field_idx`, `css`, `latex_pre`, `latex_post`, `latex_svg`, `reqs`
  (computed), `original_stock_kind` (UNKNOWN/BASIC/…/CLOZE/IMAGE_OCCLUSION; only set on 2.1.62+ notetypes),
  `original_id` (import merging), `other` (unknown JSON, preserved).
- `NoteField { ord: Option<u32>, name, config }` — `ord` is the *previous* position; `None` = new field.
  Config listed in §2.4. Defaults: Arial 20 (`notetype/fields.rs:36-55`).
- `CardTemplate { ord, mtime_secs, usn, name, config: {q_format, a_format, q_format_browser,
  a_format_browser, target_deck_id (deck override), browser_font_name, browser_font_size, id} }`.

**The `ord` convention is the whole schema-change API**: to reorder/add/delete fields or templates you
edit the `Vec` (keeping each surviving element's `ord`) and call `update_notetype`; rslib diffs ords.

### 4.3 Operations

| Operation | rslib call | Schema change (forces one-way full sync)? | Feasibility |
|---|---|---|---|
| Add from stock | `get_stock_notetype_legacy` / `notetype::stock::all_stock_notetypes` + `add_notetype(&mut nt, skip_checks)` (`mod.rs:147`) | no | trivial |
| Clone | copy struct, `id=0`, clear ords?, `original_id=None`, rename → `add_notetype` | no | trivial (re-implement Python copy) |
| Rename | set `name`, `update_notetype` (names de-duplicated by appending `+`, `mod.rs:721-738`; `"` removed) | no | trivial |
| Delete | `remove_notetype(ntid)` (`mod.rs:243`) — deletes all its notes/cards; `set_schema_modified` (`mod.rs:786-814`). UI asks `confirm_schema_modification` first and refuses deleting the last notetype (`models.py:189-208`) | **yes** | trivial |
| Options (LaTeX header/footer, SVG) | `config.latex_pre/post/svg` + `update_notetype` | no | trivial ($EDITOR for header) |
| Add / delete / reposition field | edit `fields` Vec, `update_notetype` → `update_notes_for_changed_fields` rewrites every note (`schemachange.rs:67-120`) | **yes** | trivial |
| Rename field | edit name; rslib rewrites `{{Field}}` references in templates (`mod.rs:530-608`) | no (rslib), Qt only `mark_basic` | trivial |
| Sort field | `config.sort_field_idx` → all notes' sort field recomputed | **yes** (`schemachange.rs:77-95`; Qt `mark_schema` `fields.py:218`) | trivial |
| Field options (font,size,rtl,plain-text default, collapse, exclude from search, description, sticky) | field config | no | trivial |
| Field name validation | rslib `NoteField::fix_name` strips `: { } "`, leading `# / ^` and whitespace; Qt additionally rejects case-insensitive duplicates (`fields.py:131-148`); rslib `ensure_names_unique` | — | trivial |
| `prevent_deletion` (cloze Text/Back Extra, IO fields) | enforced by **UI only** (`fields.py:188`) | — | must replicate |
| Add / remove / reorder card type | edit `templates`; `update_cards_for_changed_templates` removes cards of deleted templates, remaps ords, regenerates if fronts changed (`schemachange.rs:124-160`) | **yes** | trivial |
| Rename card type | name only | no | trivial |
| Edit front/back/CSS | template config; may generate new cards (fronts changed) — **never deletes** now-empty cards → "Empty Cards" tool needed | no | doable ($EDITOR or built-in) |
| Browser appearance | `q_format_browser`, `a_format_browser`, browser font | no | trivial |
| Deck override | `target_deck_id` (Qt uses a deck-name line edit) | no | trivial |
| Flip (swap front/back) | Python regex on `<hr id=answer>` (`clayout.py:743-755`) | no | trivial |
| Insert field into template | Python string concat with `<div style='font-family…'>{{F}}</div>` (`clayout.py:894-926`) | no | trivial |
| Restore to default | `restore_notetype_to_stock(ntid, force_kind)` (`restore.rs:15`) — replaces templates, CSS **and fields** with stock ones, ords 0..n (extra user fields beyond the stock count are dropped from notes) | yes if field/template count changes | trivial (warn loudly) |
| Template validation | on save `check_templates` (`mod.rs:520-528`): duplicate fronts (unless `{{Card}}` used), front with no field, unknown field, unparsable template, cloze notetype without `{{cloze:}}` on both sides. Errors: `AnkiError::CardTypeError{CardTypeErrorDetails::{TemplateParseError, Duplicate{index}, NoFrontField, NoSuchField{field}, MissingCloze}}` (`rslib/src/error/mod.rs:43-47,137-153,322-328`), localised via `error.message(tr)` | — | trivial |
| Preview (incl. "fill empty", night mode, mobile class) | `render_uncommitted_card(note, template, ord, fill_empty, partial)`; type-answer box faked by Python `maybeTextInput` | — | doable (needs tanki's card renderer) |
| Change notetype | §4.4 | **yes, always** | doable |
| Empty cards | `get_empty_cards` → report, `remove_cards` | no | trivial |

Schema-change UX: before a schema-modifying op the GUI calls `confirm_schema_modification()` which asks
only if `schema_changed()` is false (i.e. first such change since last sync) (`qt/aqt/main.py:1701-1706`).
rslib itself never asks; it just bumps `scm` (`collection/timestamps.rs:36`). tanki must reproduce the prompt
("The requested change will require a full upload…").

### 4.4 Change notetype (V)

- `notetype_change_info(old_ntid, new_ntid) -> NotetypeChangeInfo` (`notetypechange.rs:73`). Default maps:
  fields matched by name, leftover old fields assigned in order to unmatched new fields
  (`default_field_map` l.174); templates likewise, but **`None` (no template map) if either side is cloze**
  ("clozes can't be remapped", l.122-130).
- `change_notetype_of_notes(ChangeNotetypeInput{current_schema, note_ids, old_notetype_name,
  old_notetype_id, new_notetype_id, new_fields: Vec<Option<usize>>, new_templates: Option<Vec<Option<usize>>>})`
  (`notetypechange.rs:115`). `new_fields[i]` = old field index feeding new field i (an old field may feed several
  new fields; `None` = empty). Requires `current_schema` to equal the stored schema stamp (optimistic lock),
  always `set_schema_modified`. Unmapped templates' cards are deleted; cloze → normal keeps only cards whose
  ord has a template.
- RPC encodes `None` as `-1` (`proto ChangeNotetypeRequest`). TS page is just a mapping table
  (`ts/routes/change-notetype/lib.ts:21-109`) with "(Nothing)" option and a list of unused old items.
- **Feasibility: doable** — a two-column mapping table in a TUI.

### 4.5 Cloze vs standard; `original_stock_kind`

- `kind` drives: template count (cloze has exactly one), card generation per cloze number, field checks,
  cloze button enablement, change-notetype template mapping.
- `original_stock_kind` is used to detect IO notetypes (`editor_legacy.py:1129-1136`), to drive
  restore-to-default, and to pick IO field indices (with field `tag` as the primary mechanism,
  `image_occlusion/imagedata.rs:183-200`). Pre-2.1.62 notetypes have UNKNOWN; restore then needs a user-chosen
  kind (`clayout.py:715-724`, `SelectStockNotetype`).

---

## 5. Image occlusion

### 5.1 Pieces (V)

- Notetype: `rslib/src/image_occlusion/notetype.rs:59-117` — cloze kind, fields Occlusion (tag 0), Image (1),
  Header (2), Back Extra (3), Comments (4); first four `prevent_deletion`. Front template hides
  `{{cloze:Occlusion}}` in a `display:none` div and runs `anki.imageOcclusion.setup()` (JS) to draw masks on a
  `<canvas>` — **review-time rendering is TS** (`ts/routes/image-occlusion/review.ts`).
- RPCs (`proto/anki/image_occlusion.proto`): `GetImageForOcclusion{path}` (just reads bytes),
  `GetImageOcclusionNote{note_id}` (parsed shapes + image bytes + header/back extra/tags),
  `GetImageOcclusionFields{notetype_id}`, `AddImageOcclusionNotetype`, `AddImageOcclusionNote{image_path,
  occlusions, header, back_extra, tags, notetype_id}` (adds to the **current deck**, not a chosen one,
  `imagedata.rs:47`), `UpdateImageOcclusionNote{note_id, occlusions, header, back_extra, tags}`.
  The GUI's Add path doesn't use `AddImageOcclusionNote`; it fills fields and calls `add_note` with the chosen deck.
- Editor: `ts/routes/image-occlusion/**` built on **fabric.js** canvas.

### 5.2 Mask encoding in the Occlusion field (V)

Generated by `shapes/to-cloze.ts:179-219`; parsed by `rslib/src/cloze.rs:361` + `image_occlusion/imageocclusion.rs:18`.

```
{{c1::image-occlusion:rect:left=.2325:top=.3261:width=.202:height=.0975:oi=1}}<br>{{c2::image-occlusion:ellipse:left=.51:top=.1:rx=.05:ry=.04:oi=1}}<br>{{c3::image-occlusion:polygon:left=.1:top=.6:points=.1,.6 .3,.65 .2,.8:oi=1}}<br>{{c0::image-occlusion:text:left=.4:top=.05:text=Label\: A:scale=1:fs=.0400}}<br>
```

- Coordinates **normalised 0–1** relative to the image bounding box (`shapes/position.ts`), formatted with
  4 decimals and leading/trailing zeros stripped (`floatToDisplay`: `0.2325`→`.2325`, `0`→`.0000`).
- `angle` stored as integer steps of 360/10000 when non-zero (`shapes/lib.ts`). `fill` only when not the
  default mask colour. Values escape `\` and `:` with backslash.
- `oi=1` on every shape = "Hide all, guess one" (occlude inactive); absent = "Hide one, guess one". The
  editor-wide toggle is `hideAllGuessOne` (`Toolbar.svelte:232`).
- **Groups** (G) → several cloze strings with the **same** ordinal. Text shapes get ordinal 0 (always shown,
  not a card). New shapes fill gaps in existing ordinals first (`to-cloze.ts:35-75`). Shapes < 5 px or
  transparent fill are dropped.
- Each shape followed by `<br>`.

### 5.3 Editor tools and shortcuts (V, `tools/shortcuts.ts`)

Select S, Rectangle R, Ellipse E, Polygon P, Text T, Fill colour C, Magnify M, zoom `[` `]`, reset zoom F,
translucent masks L, Delete, Duplicate D, Group G / Ungroup U, Select all A, align left/h-centre/right/top/
v-centre/bottom Shift+L/H/R/T/V/B, undo/redo Ctrl+Z/Ctrl+Y, toggle mask editor Ctrl+Alt+Shift+M. Header /
Back Extra editable in `Notes.svelte`; tags in `Tags.svelte`.

### 5.4 Terminal assessment

- **Reviewing existing IO notes: doable.** Read shapes with `parse_image_occlusions`/`GetImageOcclusionNote`,
  composite masks onto the image in Rust (`image` + `imageproc` for rect/ellipse/polygon, text via
  `ab_glyph`), send the bitmap via Kitty/Sixel/iTerm protocols. Must replicate `review.ts` semantics
  (active shape colour, inactive shapes hidden or shown per `oi`, answer side highlight, "toggle masks").
- **Creating rectangles/ellipses: doable (with capable terminals).** Display the image with Kitty graphics
  at a known cell rectangle; enable SGR-Pixels mouse mode (`CSI ? 1016 h`) to get pixel coordinates on
  press/drag/release; convert to image-normalised coordinates using the placement's pixel size (query
  cell pixel size with `CSI 14 t`/`CSI 16 t` or `TIOCGWINSZ` ws_xpixel). Redraw the overlay per drag event by
  re-uploading a composited frame or (better) placing translucent mask images as separate Kitty placements
  with z-index. Supported by kitty, WezTerm, foot, Ghostty, recent xterm (U: exact version list). Fallback for
  cell-only mouse: coarse grid (cell resolution ≈ 8–20 px) plus keyboard nudging (hjkl move, HJKL resize)
  — perfectly usable because coordinates are normalised anyway.
- **Polygon: doable** (click-click-close), **text shapes: doable** (prompt), rotation/group/align: doable but low value.
- **Physically impossible:** nothing fundamental; the constraint is terminal support (no graphics → no IO
  creation except "blind" coordinate entry; tmux passthrough adds friction).
- Recommendation: MVP = review + rect-only creation with keyboard and mouse; edit existing = load shapes,
  allow delete/move/resize, re-export with the same ordinal rules (port `exportShapesToClozeDeletions`).

---

## 6. Terminal editing strategies

### 6.1 Constraints recap

- Field HTML is arbitrary; Anki desktop/AnkiDroid/AnkiMobile re-render it as HTML. Any change in bytes of an
  unedited field (re-serialisation) shows up as a sync change and can subtly alter rendering — so the core
  invariant is **"unchanged projection ⇒ unchanged bytes"**.
- Comparing on save: rslib `update_note` already skips writes when nothing differs, so byte-stable round
  trips also avoid bumping mtime/usn.

### 6.2 Strategy A — built-in TUI editor on a lightweight-markup projection

Design:
1. Parse field HTML with a lenient HTML5 fragment parser (`html5ever`/`scraper` or a hand-written tokenizer that
   keeps source spans). Keep the **original source span of every node**.
2. Project to an editable text where a *whitelisted* subset becomes markup:
   text; `<br>` → newline; top-level `<div>X</div>` → newline + X; `<b>/<strong>` → `**…**`; `<i>/<em>` → `*…*`;
   `<u>` → `__…__`; `<sub>/<sup>` → `~…~`/`^…^`; `<ul>/<ol><li>` → `- `/`1. `; `<img src=F>` → `![](F)` or an
   atomic chip `[img:F]`; `[sound:F]` stays literal; `\(…\)` and `{{c1::…}}` stay literal (they are already
   text). `&nbsp;` → U+00A0 shown as `·`; entities decoded.
3. Everything else (spans with style, tables, links, fonts, comments, unknown attributes, even a `<b>` that
   carries attributes) becomes an **opaque token**: a single atomic, non-editable glyph run like
   `⟦span style=…⟧text⟦/span⟧` where the inner text may remain editable but the tag tokens are atomic and map
   back to their original source bytes.
4. Serialise only if the projection changed; even then, **re-emit original source spans for untouched
   regions** and only generate canonical HTML for edited runs (tree-diff on node level, or simpler: diff the
   projection by lines/runs and splice). Generated canonical forms should match what Anki itself produces
   (`<b>`, `<i>`, `<u>`, `<div>` per line after the first, `<br>` for soft breaks, `&nbsp;` for leading/
   multiple spaces, `&lt;&gt;&amp;` escaping) so the output looks native to other clients.
5. Verification: property test "parse→project→serialise without edits == input" over real collections
   (e.g., AnkiWeb shared decks) — any mismatch falls back to raw mode for that field.

Pros: best UX; WYSIWYG-ish with SGR styling; cloze/media/tags integrate. Cons: largest effort; must
handle IME, wide chars, bidi; projection ambiguity (a literal `**` typed by the user must be escaped).
Feasibility: **hard but bounded**; lossless guarantee achievable via span-preservation + "untouched ⇒ original
bytes" + fallback.

### 6.3 Strategy B — raw HTML mode

Edit the stored HTML verbatim (like Anki's Ctrl+Shift+X HTML editor), with syntax highlighting
(tree-sitter-html) and optional auto-close tags. Lossless by construction; Anki itself strips only `script`/`link`
(can skip or mirror). Optionally pretty-print **for display only** — do not save the pretty-printed form
(Anki's own HTML editor does not reformat). Feasibility: **trivial**. Required as fallback and for
`plain_text` fields (fields configured "use HTML editor by default").

### 6.4 Strategy C — `$EDITOR` round-trip

File format proposal (apy-compatible in spirit; apy's exact syntax unverified here):

```markdown
# Note
nid: 1712345678901        # absent for new notes
model: Basic
deck: Languages::French
tags: geo europe

## Front
Paris is the capital of {{c1::France}}

## Back
<!-- raw -->
<span style="color: rgb(255, 0, 0);">Unchanged legacy markup</span>
```

- One `# Note` block per note → bulk editing of many notes in one file (search → edit → apply).
- Per field choose representation: projection (Strategy A serializer) or raw HTML (marker line `<!-- raw -->` or
  a fenced ```` ```html ```` block). Default to raw for fields whose projection is not provably lossless.
- On return: parse, compare each field's text with what was written out; **only fields whose text changed are
  converted and written**. Report parse errors by reopening the editor with an error comment at the top
  (like `git commit`).
- Concurrency: store `mtime` per note in the header; if the DB note changed meanwhile, conflict → show diff.
- Media: `![](path/outside/collection.png)` → on save `add_file` and rewrite to the returned name.
Feasibility: **doable, low risk**; great for power users, scripting, and as the first shipped editor.

### 6.5 Strategy D — media attach flows

| Flow | How | Verdict |
|---|---|---|
| File path prompt with completion | own completer (or `$EDITOR` path) → `add_file` → link | trivial |
| Drag-and-drop a file onto terminal | terminals paste the path (quoted or `file://` URI, varies by terminal) inside bracketed paste; detect "pasted text is a single existing path/URI with media extension" → offer attach | doable |
| Bracketed paste of a URL | `retrieve_url` (mirror Anki's extended-paste behaviour) | trivial |
| Paste image from clipboard | shell out to `wl-paste --type image/png` / `xclip -selection clipboard -t image/png -o` / `pngpaste`/`osascript`, or the `arboard` crate (X11/Wayland/macOS/Windows image support); name `paste-<sha1>.png/jpg`, honour `pasteImagesAsPng` | doable (local only; over SSH impossible without terminal protocol) |
| Paste HTML from clipboard | `wl-paste -t text/html` / `xclip -t text/html`; run Rust port of `filterHTML` + pre-filter (download remote imgs, data: URIs) | doable |
| Terminal clipboard protocols | OSC 52 is text-only; kitty's OSC 5522 extended clipboard can transfer arbitrary MIME types incl. images (U: kitty-only, recent versions; verify spec) — the only way to get images over SSH | hard / terminal-specific |
| Record audio | spawn `ffmpeg -f pulse -i default -c:a libmp3lame out.mp3` / `pw-record` / `arecord`, or `cpal` capture + encoder crate; show level meter + stop key; name like Anki (`rec<ts>.mp3`) | doable |
| Video | attach as `[sound:x.mp4]` like Anki | trivial |

### 6.6 Recommendation

- **Primary:** Strategy C (`$EDITOR`, Markdown-with-front-matter, per-field raw fallback, only-changed-fields
  write-back) for v1 — fastest path to a correct, lossless, bulk-capable editor; reuse its parser/serialiser later.
- **In-app:** Strategy B raw HTML editor in the TUI from day one (needed for `plain_text` fields and as the
  safety valve), then evolve to Strategy A with opaque tokens once a lossless test corpus passes.
- **Media:** path prompt + bracketed-paste path detection + `arboard`/wl-paste image paste; ffmpeg recording
  behind a feature check. OSC 5522 opportunistically.
- **IO:** review first; creation of rectangles with SGR-Pixels + Kitty graphics, keyboard fallback.

---

## 7. Risks, surprises, things not verified

- Default editor in 26.08 is still the legacy Python one; the Svelte editor is experimental — both produce the
  same HTML, but logic is duplicated (Python vs TS). (V)
- rslib **lowercases new media filenames** (`media/files.rs:187-195`). (V)
- New-editor `AddMediaFromUrl` returns the requested, not the stored, filename (`backend/media.rs:20-27`). (V, bug)
- MathJax block closing-tag typo `</anki-matjax>` (`LatexButton.svelte:38`). (V, benign)
- Editor cloze numbering ignores `{{c1,2::}}` multi-ordinals that rslib now parses. (V)
- `AddImageOcclusionNote` RPC ignores the deck chooser (uses current deck). (V)
- `restore_notetype_to_stock` replaces fields too (possible data loss for extra fields). (V)
- `prevent_deletion` is UI-enforced only. (V)
- Chromium contenteditable output details (`<div>` on Enter, `&nbsp;` placement, colour serialisation as
  `rgb()`) — (U), consistent with observed Anki data but not executed here.
- Exact apy file syntax, kitty OSC 5522 details, SGR-Pixels support matrix, drag-and-drop path format per
  terminal — (U).
- Whether the generated `services::*Service` traits are reachable as `pub` from an external crate — (U), check at
  build time; otherwise go through `Backend::run_service_method` with protobuf bytes.
