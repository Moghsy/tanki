# 03 — Card rendering pipeline: from note fields to pixels, sound and speech

Source: Anki v26.08.1 at `/home/emard/projects/open-source/anki` (read-only). All paths below are relative to that repo root. Line numbers come from that tree.

Verdict scale used throughout:

- **trivial**: a direct mapping exists, under a day of work.
- **doable**: real work, but no open research questions.
- **hard**: needs a substantial subsystem (layout engine, JS engine, rasteriser) or loses fidelity.
- **impossible**: a character-cell terminal cannot represent it at all. An approximation or a fallback is the only option.

---

## 0. TL;DR pipeline

```
note fields + notetype (qfmt/afmt/css/latex_pre/latex_post/latex_svg)
   │  rslib: Collection::render_existing_card(cid, browser, partial_render)        [notetype/render.rs:46]
   │    ├─ add_special_fields (Tags, Type, Deck, Subdeck, CardFlag, Card, CardID, cN=1)
   │    └─ template::render_card → ParsedTemplate::render → apply_filters (built-ins)
   ▼
RenderCardOutput { qnodes, anodes, css, latex_svg, is_empty }                      [notetype/render.rs:18]
   │  pylib/anki/template.py TemplateRenderContext.render()                         [template.py:224]
   │    ├─ apply_custom_filters (add-on filters, {{FrontSide}} := extracted q text, tts-voices)
   │    ├─ backend.extract_av_tags(q, true)  → "[anki:play:q:N]" + Vec<AvTag>
   │    ├─ backend.extract_av_tags(a, false) → "[anki:play:a:N]" + Vec<AvTag>
   │    └─ hooks.card_did_render → latex.on_card_did_render → [latex] → <img class=latex src=latex-<sha1>.png>
   ▼
TemplateRenderOutput { question_text, answer_text, question_av_tags, answer_av_tags, css }
   │  card.question() = "<style>{css}</style>{question_text}"                        [template.py:289, cards.py:149]
   │  qt/aqt/reviewer.py _showQuestion / _showAnswer
   │    ├─ av_player.play_tags(...) if deck autoplay
   │    ├─ _mungeQA: encode_iri_paths (media filenames) → [anki:play:..] → SVG replay buttons
   │    │            → typeAnsFilter ([[type:F]] → <input id=typeans> / compare_answer diff)
   │    └─ web.eval("_showQuestion(q, a, 'card card1 isLin nightMode night_mode fancy')")
   ▼
ts/reviewer/index.ts _updateQA: lazy-load MathJax, preload img/css/fonts, #qa.innerHTML = html,
re-insert <script> tags so they run, MathJax.typesetPromise([qa]), body.className = classes
   ▼
Chromium (QtWebEngine) lays out arbitrary HTML/CSS/JS. Media is served by mediasrv.py from collection.media.
```

The key facts for tanki:

1. **All template logic lives in Rust**: parsing, conditionals, every standard filter, cloze, the type-answer placeholder, the TTS tag, furigana, hint markup, and the empty-card message.
2. **What Python adds** is small but required:
   - substituting `{{FrontSide}}` with the *AV-extracted* question text;
   - calling `extract_av_tags` on each side;
   - running LaTeX through latex/dvipng/dvisvgm;
   - the add-on filter hook;
   - the `tts-voices` filter.
3. **What Qt/Python reviewer code adds**:
   - type-in-answer input and comparison wiring;
   - replay buttons;
   - IRI-encoding of media paths;
   - body classes;
   - the audio queue and its players (mpv, mplayer, TTS).
4. **What the TS reviewer adds**:
   - script execution;
   - MathJax;
   - resource preloading;
   - **image occlusion mask drawing** on a `<canvas>`.

---

## 1. Template engine

### 1.1 Files and public entry points

| What | Where | Visibility from an external crate |
|---|---|---|
| Lexer/parser/renderer | `rslib/src/template.rs` | `pub mod template`. `render_card`, `RenderCardRequest`, `RenderCardResponse`, `RenderedNode`, `ParsedTemplate` are `pub`. `RenderContext` is `pub(crate)`. |
| Built-in filters | `rslib/src/template_filters.rs` | `pub mod`, but `apply_filters` is `pub(crate)` |
| Cloze | `rslib/src/cloze.rs` | `pub mod cloze`. `reveal_cloze_text`, `reveal_cloze_text_only`, `extract_cloze_for_typing`, `cloze_numbers_in_string`, `cloze_number_in_fields`, `parse_image_occlusions`, `expand_clozes_to_reveal_latex` are `pub`. |
| Card render orchestration | `rslib/src/notetype/render.rs` | `Collection::render_existing_card`, `render_uncommitted_card`, `render_card` are `pub fn`. `RenderCardOutput` is a `pub struct` in a private module, re-exported `pub(crate)` (`notetype/mod.rs:40`). So it is **usable but unnameable** from outside: fields are accessible through type inference. The alternative is the proto path `CardRenderingService::render_existing_card` → `anki_proto::card_rendering::RenderCardResponse`. |
| AV tags | `rslib/src/card_rendering/{mod,parser,writer}.rs` | `pub fn strip_av_tags`, `extract_av_tags`, `prettify_av_tags` |
| Service (RPC) | `rslib/src/card_rendering/service.rs` | `impl crate::services::CardRenderingService for Collection` |
| Python glue | `pylib/anki/template.py` | must be re-implemented |
| Type answer | `rslib/src/typeanswer.rs` | **private module** (`lib.rs:50 mod typeanswer;`). Only reachable via `CardRenderingService::compare_answer(CompareAnswerRequest{expected, provided, combining})`. |
| Markdown | `rslib/src/markdown.rs` | **private** (`lib.rs:28`). Via `CardRenderingService::render_markdown`. Uses pulldown-cmark. |
| LaTeX extraction | `rslib/src/latex.rs` | `pub mod latex`, `pub fn extract_latex(text, svg)`. `extract_latex_expanding_clozes` is `pub(crate)` (reachable via the `extract_latex` RPC with `expand_clozes=true`). |

Signatures (verified):

```rust
// notetype/render.rs
pub fn render_existing_card(&mut self, cid: CardId, browser: bool, partial_render: bool) -> Result<RenderCardOutput>   // :46
pub fn render_uncommitted_card(&mut self, note: &mut Note, template: &CardTemplate, card_ord: u16,
                               fill_empty: bool, partial_render: bool) -> Result<RenderCardOutput>                // :72
pub fn render_card(&mut self, note: &Note, card: &Card, nt: &Notetype, template: &CardTemplate,
                   browser: bool, partial_render: bool) -> Result<RenderCardOutput>                                 // :112
pub struct RenderCardOutput { pub qnodes: Vec<RenderedNode>, pub anodes: Vec<RenderedNode>,
                              pub css: String, pub latex_svg: bool, pub is_empty: bool }                            // :18
// .question() / .answer() return the single Text node, or the literal "not fully rendered"

// template.rs
pub enum RenderedNode { Text { text: String },
                        Replacement { field_name: String, current_text: String, filters: Vec<String> } }       // :367
pub fn render_card(req: RenderCardRequest<'_>) -> Result<RenderCardResponse>                                        // :597
pub struct RenderCardRequest<'a> { qfmt, afmt, field_map: &HashMap<&str, Cow<str>>, card_ord: u16,
                                   is_cloze: bool, browser: bool, tr: &I18n, partial_render: bool }                  // :579

// card_rendering/mod.rs
pub fn strip_av_tags<S: Into<String> + AsRef<str>>(txt: S) -> String                                               // :13
pub fn extract_av_tags<S: ...>(txt: S, question_side: bool, tr: &I18n) -> (String, Vec<anki_proto::card_rendering::AvTag>) // :19
pub fn prettify_av_tags<S: ...>(txt: S) -> String   // [sound:x] → 🔉x🔉, tts → 💬text💬                             // :29
```

### 1.2 Lexing and parsing (`template.rs:42-252`)

- **Tokens.** Tokens are `{{...}}` handlebars and `<!-- ... -->` comments; everything else is text. Comments are copied to the output verbatim (`:418-422`), so a `{{Field}}` inside an HTML comment is **not** substituted.
- **Legacy alternate delimiters.** If the template (after leading whitespace) starts with `{{=<% %>=}}`, the delimiters become `<% %>` (`:40, :137-145`).
- **Classification** (`classify_handle`, `:154`):
  - leading `{` characters are stripped and the rest trimmed;
  - `#X` opens a conditional, `^X` opens a negated conditional, `/X` closes;
  - anything else is a replacement;
  - a token shorter than 2 chars is always a replacement.
- **Filters** are split with `rsplit(':')` (`:214`). For `{{a:b:Field}}`: key = `Field`, filters = `["b", "a"]`. Filters apply **right to left** (closest to the field first).
- **Unclosed `{{`.** An unclosed `{{Field` is *not* an error with the current nom lexer: `take_until("}}")` fails and the text is emitted literally. `TemplateError::NoClosingBrackets` still exists but is effectively dead.
- **Errors** (`TemplateError`): `ConditionalNotClosed`, `ConditionalNotOpen{closed, currently_open}`, `FieldNotFound{field, filters}`, `NoSuchConditional`. `template_error_to_anki_error` (`:254`) renders them as HTML:

  ```html
  Front template has a problem:<br>Found '{{Foo}}', but there is no field called 'Foo'<br><a href='https://docs.ankiweb.net/templates/errors.html#template-syntax-error'>More information</a>
  ```

  In Python an error becomes both `question_text` and `answer_text` (`template.py:227-233`).

### 1.3 Rendering semantics (`render_into`, `:407-511`)

**Replacements.**
- `{{FrontSide}}`:
  - When `partial_for_python` is set, it becomes `Replacement{field_name:"FrontSide", filters:[], current_text:""}` for the caller to fill in.
  - Otherwise it gets the text of the first question node (`:651-658`).
  - Any filters on FrontSide are ignored.
- `{{filter:}}` (empty field name with filters):
  - with `partial_for_python` it becomes `Replacement{field_name:"", ...}`, so the filter can generate its own text (used by `{{tts-voices:}}`);
  - otherwise it renders nothing.
- An **unknown field** gives a `FieldNotFound` error.
- Filters run through `apply_filters`. If an unknown filter is met *and* `partial_for_python` is set, processing stops and the remaining filters are returned in a `Replacement` node. With `partial_render=false`, unknown filters are **silently skipped** (`template_filters.rs:51-60`).

**Conditionals.**
- `{{#X}}` is true iff `X` is in `nonempty_fields`.
- `{{^X}}` is the negation.
- `X` must be a field (including special fields) or match `c<digits>`. Otherwise you get `NoSuchConditional`.
- Children of a false branch are still rendered into a scratch buffer so that errors still surface (`:496-498`).

**Emptiness** (`field_is_empty`, `:543`). A field counts as empty if it matches `^(?:[[:space:]]|</?(?:br|div)\ ?/?>)*$` (case-insensitive). So `<div><br></div>` is empty, but `&nbsp;` or `<span></span>` is **not**.

**Special fields** (`notetype/render.rs:162-190`, `SPECIAL_FIELDS` in `notetype/mod.rs:71`). Each is inserted only if no real field has that name.

| Field | Value |
|---|---|
| `Tags` | note tags joined by `" "` |
| `Type` | notetype name |
| `Deck` | full deck name of `original_deck_id` or `deck_id` (`human_name()`, `::` separators) |
| `Subdeck` | last `::` component |
| `CardFlag` | `"flag{n}"` with n = `card.flags` (Rust gives `flag0` for "no flag". The obsolete Python `.fields()` gave `""`). Because it is always non-empty, `{{#CardFlag}}` is always true. |
| `Card` | template name |
| `CardID` | card id |
| `c{ord+1}` | `"1"`. This makes `{{#c1}}` true on card 1, and works for normal notetypes too. |
| `FrontSide` | handled in the renderer |

**Empty-front check** (`render_card`, `:624-648`). It runs only on the question side.
- Cloze notetype, when no field contains cloze number `ord+1`:

  ```html
  <div>No cloze 2 found on card.
  Please either add a cloze deletion, or use the Empty Cards tool.<br><a href='https://docs.ankiweb.net/templates/errors.html#no-cloze-filter-on-cloze-note-type'>More information</a></div>
  ```

- Normal notetype, not browser, and `!qtmpl.renders_with_fields(nonempty)`:

  ```html
  <div>The front of this card is blank.<br><a href='https://docs.ankiweb.net/templates/errors.html#front-of-card-is-blank'>More information</a></div>
  ```

  `renders_with_fields` is a purely structural check: at least one replacement of a non-empty field must be reachable through true conditionals. **Static text in the template does not count.**
- In either case the message is **appended** to qnodes, and anodes = just the message; `is_empty = true`.

**Browser templates.** `browser=true` uses `bqfmt`/`bafmt` (`templates.rs:34-48`, falling back to qfmt/afmt) and skips the "blank front" check. The browser table (`browser_table.rs:299-345`) works like this:
1. render with `browser=true, partial=true`;
2. concatenate nodes using `current_text`;
3. apply `prettify_av_tags`;
4. for the answer column, strip the question prefix;
5. finish with `html_to_text_line(html, true)`.

That is exactly the right recipe for a tanki browser list.

### 1.4 Built-in filters (`template_filters.rs:72-108`)

Special pre-processing in `apply_filters` (`:35-40`):
- filters `["cloze","type"]` (i.e. `{{type:cloze:F}}`) become `type-cloze`;
- `["nc","type"]` becomes `type-nc`;
- **any** filter list ending in `type` becomes just `type`, and all other filters are dropped.

| Filter | Output (exact) |
|---|---|
| `text` | `strip_html(text)`: drops comments, `<style>`, `<script>` blocks and all tags, decodes entities, `&nbsp;` → space |
| `furigana` | regex `" ?([^ >]+?)\[(.+?)\]"` → `<ruby><rb>$1</rb><rt>$2</rt></ruby>`. `&nbsp;` is first replaced by a space. A leading space is consumed. Matches where `$2` starts with `sound:` are left untouched. |
| `kanji` | same regex → `$1` |
| `kana` | same regex → `$2` |
| `type` | `[[type:FieldName]]` (literal field name; the field text is ignored) |
| `type-cloze` | `[[type:cloze:FieldName]]` |
| `type-nc` | `[[type:nc:FieldName]]` |
| `hint` | empty if the text is blank. Otherwise the snippet below, where `id` = first 8 bytes of blake3(text‖field_name), hex. |
| `cloze` | `cloze_filter`: see §1.6 |
| `cloze-only` | `reveal_cloze_text_only`: comma-joined plain list of the active clozes' hints (question) or texts (answer) |
| `tts <opts>` | `[anki:tts lang=<opts>]{text}[/anki:tts]`, e.g. `{{tts en_US voices=Bob,Jane speed=1.2:F}}` → `[anki:tts lang=en_US voices=Bob,Jane speed=1.2]…[/anki:tts]` |
| `""` (from `::`) | ignored |

The `hint` filter output:

```html

<a class=hint href="#"
onclick="this.style.display='none';
document.getElementById('hint83fe48607f0f3a66').style.display='block';
return false;" draggable=false>
FieldName</a>
<div id="hint83fe48607f0f3a66" class=hint style="display: none">TEXT</div>
```

Filters not in Rust, and how they are resolved:
- `tts-voices` is a Python `field_filter` hook in `qt/aqt/tts.py:149-165`. Used as `{{tts-voices:}}`, it prints the available voices.
- Add-on filters run via `hooks.field_filter` and the legacy `fmod_<name>`.
- These are the only built-in Python render hooks (verified by grep): `latex.on_card_did_render` and `tts.on_tts_voices`.
- Common add-on filters such as `edit:` (Edit Field During Review) are just dropped in tanki.

### 1.5 `{{FrontSide}}` and AV tags: the subtle ordering

`template.py:224-256`:

```python
qtext = apply_custom_filters(partial.qnodes, self, front_side=None)
qout  = backend.extract_av_tags(text=qtext, question_side=True)
atext = apply_custom_filters(partial.anodes, self, front_side=qout.text)   # FrontSide := q AFTER extraction
aout  = backend.extract_av_tags(text=atext, question_side=False)
```

So the answer's FrontSide contains `[anki:play:q:0]` refs that point into the **question** tag list. `answer_av_tags` holds only the back's own sounds. Replay on the answer side plays `question_av_tags + answer_av_tags` when the deck option "replay question" is on (`reviewer.py:74-81`, `cards.py:206`, proto `skip_question_when_replaying_answer`).

**tanki must reproduce this:**
1. call `render_existing_card(cid, false, partial_render=true)`;
2. resolve Replacement nodes yourself (FrontSide = extracted q; unknown filters = use `current_text`; `tts-voices` = optional);
3. call `extract_av_tags` per side.

Using `partial_render=false` would put the *raw* question (with `[sound:]`) into FrontSide, and the answer would then play question audio twice.

### 1.6 Cloze (`cloze.rs`)

**Tokenizer** (`:58-117`):
- `{{c` + digits/commas + `::` opens a cloze;
- `}}` closes;
- everything else is text.
- Ordinals are deduped and sorted. `{{c1,2::x}}` belongs to cards 1 and 2. `c0` is not a card (used for IO text labels).
- Nesting is allowed up to 10 deep (`:187`). A stray `}}` stays literal text.

**Hint.** Inside a cloze, the *text node* is split at the first `::`: `head` is the content and `tail` the hint (`:199-202`). Text starting with `image-occlusion:` is not split.

**Rendering** (`reveal_cloze`, `:250-330`), for card ordinal `N` (= ord+1). The question side is `context.frontside.is_none()`:

| Case | Markup |
|---|---|
| question, active | `<span class="cloze" data-cloze="{html-attr-escaped inner content, inner clozes rendered}" data-ordinal="1,2">[{hint or ...}]</span>` |
| answer, active | `<span class="cloze" data-ordinal="1">{content}</span>` |
| inactive (both sides) | `<span class="cloze-inactive" data-ordinal="2">{content}</span>` (children may be active) |

- If no cloze with ordinal N exists in the field, `cloze` returns `""` (`:401-405`). Add-ons such as "cloze overlapper" rely on this.
- Afterwards, `strip_html_inside_mathjax` removes HTML tags (keeping entities) inside `\(..\)` and `\[..\]` (`:494-512`). A cloze *inside* MathJax therefore becomes `\(x = [...]\)` on the question side, and on the answer side the `<span class="cloze">` around the content is stripped (MathJax shows plain math; the colouring is lost).
- Examples (from tests, `:612-650`):

  ```
  "foo {{c1::bar {{c2::baz}}}}", card1 Q → foo <span class="cloze" data-cloze="bar &lt;span class=&quot;cloze-inactive&quot; ...">[...]</span>
  card1 A → foo <span class="cloze" data-ordinal="1">bar <span class="cloze-inactive" data-ordinal="2">baz</span></span>
  card2 Q of "foo {{c1::bar {{c2::baz}}::qux}}" → foo <span class="cloze-inactive" data-ordinal="1">bar <span class="cloze" data-cloze="baz" data-ordinal="2">[...]</span></span>
  ```

- `cloze-only`: `reveal_cloze_text_only("foo {{c1::bar::baz}}",1,true)` = `baz`. On the answer side `{{c1::foo}} {{c1::bar}}` = `foo, bar`.
- `extract_cloze_for_typing(text, N)`: comma-joined answer texts. If all are identical they collapse into one (`:416-429`).
- Stock cloze CSS (`notetype/cloze_styling.css`): `.cloze{font-weight:bold;color:blue}` and `.nightMode .cloze{color:lightblue}`. `.cloze-inactive` has no stock style.

### 1.7 LaTeX (`rslib/src/latex.rs` + `pylib/anki/latex.py`)

**Extraction (Rust).** Regex (`latex.rs:14-25`, case-insensitive, dot-all):
- `[latex]…[/latex]` → body as-is;
- `[$]…[/$]` → `$…$`;
- `[$$]…[/$$]` → `\begin{displaymath}…\end{displaymath}`.

The body is then cleaned (`<br>`, `<br />`, `<div>` → `\n`; `strip_html`; entities decoded). Then:
- filename = `latex-{sha1hex(body)}.{png|svg}`;
- replacement = `<img class=latex alt="{attr-escaped body}" src="latex-….png">`.

**Generation (Python).** `pylib/anki/latex.py`, hooked via `collection.py:99 anki.latex.setup_hook()` into `card_did_render`, **after** AV extraction:
1. Skip if `col.media.have(filename)`. Shared decks usually ship the PNGs, so this is the common path.
2. If the config `RenderLatex` (BoolKey, **default false**, `config/bool.rs`) is off, append the localized error "LaTeX generation disabled". This is a security measure; LaTeX can run commands.
3. Write `{latex_pre}\n{body}\n{latex_post}` to `tmp.tex`. The defaults are in `notetype/header.tex` (`\documentclass[12pt]{article} \special{papersize=3in,5in} \usepackage[utf8]{inputenc} \usepackage{amssymb,amsmath} \pagestyle{empty} \setlength{\parindent}{0in} \begin{document}`) and `\end{document}`.
4. In the temp dir run:
   - PNG: `latex -interaction=nonstopmode tmp.tex` then `dvipng -bg Transparent -D 200 -T tight tmp.dvi -o tmp.png`;
   - SVG (`latex_svg` / notetype `latexsvg`): `dvisvgm --no-fonts --exact -Z 2 tmp.dvi -o tmp.svg`.
5. `col.media.write_data(filename, bytes)`.
6. On failure, append HTML with the error and the `<pre>` log.

For tanki this is about 60 lines of Rust with `std::process::Command`. It is the same external tools, so **doable**.

Reviewer CSS: `.nightMode .latex { filter: invert(100%) }` (`ts/reviewer/reviewer.scss:126-130`). tanki should invert LaTeX PNGs in dark terminals (also **trivial** with the `image` crate).

### 1.8 MathJax

- **Storage format.** Fields store `\(…\)` (inline) and `\[…\]` (display). `<anki-mathjax block="true">` is **editor-only** decoration (`ts/lib/editable/mathjax-element.svelte.ts:13-62`, `toStored`/`toUndecorated`) and never reaches the reviewer.
- **Loading.** The reviewer loads MathJax lazily if the HTML matches `/\\\[(.*?)\\\]|\\\((.*?)\\\)/su` (`ts/reviewer/index.ts:46-50`). It loads `/_anki/js/mathjax.js` (config, from `ts/mathjax/index.ts`) and then `tex-chtml-full.js`.
- **Config.** `displayMath: [["\\[","\\]"]]`, default inline `\(\)`, `processEscapes:false`, `processEnvironments:false`, packages `+noerrors +mathtools −textmacros`, `startup.typeset:false`. After `innerHTML` it calls `MathJax.typesetClear(); MathJax.typesetPromise([qa])`.
- **Terminal options:**
  - (a) Unicode approximation for simple inline math (`\alpha`→α, `^2`→², `\frac{a}{b}`→a⁄b). **Doable** for maybe 70% of short formulas.
  - (b) Rasterise: TeX → SVG with MathJax-in-a-JS-engine (rquickjs/boa + liteAdaptor) or via headless browser; then `resvg` → PNG → graphics protocol. **Hard but well-bounded.**
  - (c) Shell out to `latex`+`dvisvgm` (MathJax and LaTeX syntax mostly overlap). **Doable**, but needs TeX installed and is slow, so cache by hash.
  - A Rust crate wrapping MathJax→SVG was not verified to exist.

### 1.9 AV tags (`card_rendering/parser.rs`, `writer.rs`)

Parser nodes:
- `[sound:FILE]`: `FILE` is anything up to `]`; `[sound:]` with nothing inside is plain text.
- `[anki:NAME opts]content[/anki:NAME]`: opts are whitespace-separated `key=val`, with `val` optionally `"double quoted"`. The closing tag must match the name.
- For `name == "tts"` the recognised keys are `lang`, `voices` (comma list), `speed` (f32, default 1.0) and `cloze_blank`. Anything else goes to `options`.

`extract_av_tags(text, question_side, tr)`, `AvExtractor` (`writer.rs:122-182`):
- `[sound:x]` becomes `[anki:play:q:{idx}]` plus `AvTag::SoundOrVideo(decode_entities(x))`. Python then applies `os.path.basename` for security (`template.py:99`).
- `[anki:tts lang=L ...]content[/anki:tts]` becomes `[anki:play:q:{idx}]` plus `TtsTag{field_text: strip_html_for_tts(content).replace("[...]", cloze_blank or tr "blank"), lang, voices, speed, other_args:["k=v"]}`.
- A missing `lang` writes `[Bad anki:tts directive: lang not set]` (localized) instead of a play tag.
- Other `[anki:foo]` directives are copied verbatim.

Examples:
- `strip_av_tags("foo [sound:bar] baz")` = `"foo  baz"`.
- The display regex for refs is `AV_REF_RE = r"\[anki:(play:(.):(\d+))\]"` (`pylib/anki/sound.py:69`).

Replay buttons (`qt/aqt/sound.py:906-921`, added in `main.py:634-644` unless the config `HideAudioPlayButtons` is set, in which case `strip_av_refs`):

```html
<a class="replay-button soundLink" href=# onclick="pycmd('play:q:0'); return false;" draggable="false">
    <svg class="playImage" viewBox="0 0 64 64" version="1.1">
        <circle cx="32" cy="32" r="29" />
        <path d="M56.502,32.301l-37.502,20.101l0.329,-40.804l37.173,20.703Z" />
    </svg>
</a>
```

Terminal: render a `▶`/`🔊` glyph (optionally numbered, e.g. `▶1`) and make it clickable via mouse reporting (and OSC 8 for display). Keys `r`/F5 replay all. **Trivial.**

---

## 2. Rendering output and the reviewer wrapper

### 2.1 What rendering produces

- `question_text` / `answer_text`: HTML fragments, *after* AV extraction and LaTeX substitution.
- `card.question()` = `<style>{css}</style>{question_text}` and `card.answer()` = `<style>{css}</style>{answer_text}` (`template.py:289-293`, `cards.py:149-153`).
- `css`: the notetype's CSS string, global for all templates of that notetype. Templates select on `.card1`, `.card2`, ….
- AV tag lists for each side.
- `latex_svg`: whether LaTeX images are SVG.
- Body classes (`qt/aqt/theme.py:163-191`):

  ```
  card card{ord+1} isLin|isWin|isMac [nightMode night_mode [macos-dark-mode]] [fancy] [no-blur]
  ```

  (`reduce-motion` is never added in the reviewer.) On `<html>`: `night-mode` (doc class) plus, from `ts/reviewer/browser_selector.ts`, the user-agent classes: `linux|win|mac`, `mobile ios iphone ipad android`, `chrome|firefox|safari`. Desktop gives `linux chrome`. Desktop adds `.mobile` only via the "emulate mobile" debug toggle (`_emulateMobile`).

### 2.2 Page shell (`qt/aqt/reviewer.py:329-341`, `webview.py:stdHtml`)

```html
<!doctype html><html class="night-mode?" dir="ltr|rtl" data-bs-theme="..."><head>…css/reviewer.css, js/reviewer.js…</head>
<body class="...">
<div id="_mark" hidden>&#x2605;</div>
<div id="_flag" hidden>&#x2691;</div>
<div id="qa" dir="auto"></div>
{col.conf["reviewExtra"]}
</body></html>
```

Relevant `reviewer.css` rules (`ts/reviewer/reviewer.scss`):
- `body{margin:20px; overflow-wrap:break-word}`
- `body.nightMode{background-color:var(--canvas); color:var(--fg)}`. Its specificity beats the stock `.card{color:black;background:white}`, so **night mode overrides stock card colours**.
- `img{max-width:100%; max-height:95vh}`
- `hr{1px darkgray, margin 1em 0}`
- `li{text-align:start}`, `pre{text-align:left}`
- `#typeans{width:100%; line-height:1.75}`, `code#typeans{white-space:pre-wrap}`
- `.typeGood{background:#afa;color:black}`, `.typeBad{background:#faa;color:black}`, `.typeMissed{background:#ccc;color:black}`
- `.replay-button svg{40px}`
- `.nightMode .latex{filter:invert(100%)}`
- `.drawing{zoom:50%}`, `.nightMode img.drawing{invert(1) hue-rotate(180deg)}`
- `#_flag`/`#_mark` are fixed in the top corners; flag colour is `var(--flag-N)`.

### 2.3 Showing a card (`reviewer.py:367-484`)

`_showQuestion`:
1. `q = c.question()`.
2. If `c.autoplay()` (deck config `autoplay` = !`disable_autoplay`), play `question_av_tags`. Otherwise `setPlaybackRequiresGesture(True)`, which blocks HTML `<audio autoplay>`.
3. `q = _mungeQA(q)`, which is `typeAnsFilter(prepare_card_text_for_display(q))`:
   - `col.media.escape_media_filenames(text)` → `encode_iri_paths`: percent-encodes local filenames in `<img|audio|video|object|source src|data=…>` with the WHATWG fragment set (controls, space, `"`, `<`, `>`, `` ` ``, `#`). `http(s)://` is untouched (`text.rs:546-593`).
   - `[anki:play:…]` → replay buttons.
   - type-answer filter (§3).
4. `gui_hooks.card_will_show` (add-ons).
5. `web.eval("_showQuestion(q, a_escaped, bodyclass)")`. The answer is passed only for image/MathJax preloading.

`_showAnswer` does the same with `c.answer()` and `_showAnswer(a)`.

TS side (`ts/reviewer/index.ts:140-262`), queued serially:
1. If MathJax delimiters are present, lazy-load MathJax.
2. `preloadResources(html)`: images, `<link rel=stylesheet>`/`@import` sheets and `@font-face` URLs, with timeouts of 200/500/800 ms.
3. `qa.style.opacity=0`.
4. `setInnerHTML(qa, html)`: pauses old `<video>`s, sets innerHTML, then **replaces every `<script>` with a fresh element so it executes**, awaiting external `src` scripts in order.
5. Run `onUpdateHook`. For the question that is `scrollTo(0,0); body.className = bodyclass`. For the answer it waits for all images, then `#answer.scrollIntoView()`.
6. MathJax typeset.
7. `opacity=1`.
8. `onShownHook`: focus `#typeans`; preload answer images.

Errors render as `<div>Invalid html on card: …</div>`.

The add-on JS API is `require("anki/reviewer").onUpdateHook/onShownHook`, plus `globalThis.anki.imageOcclusion`, `anki.mutateNextCardStates`, `anki.setupImageCloze`.

Card scripts can also call `pycmd("ans")`, `pycmd("ease3")`, `pycmd("play:q:0")` (link handler, `reviewer.py:673-696`). Some shared decks use this; tanki could pattern-match it in onclick attributes.

### 2.4 Keyboard (reviewer defaults, `reviewer.py:594-641`)

| Key | Action |
|---|---|
| space / enter | show answer, or rate with the default ease |
| 1-4 | rate |
| r, F5 | replay audio |
| 5 | pause audio |
| 6 / 7 | seek −5 s / +5 s |
| v | replay own recording |
| Shift+V | record own voice |
| e | edit |
| `*` | mark |
| = / - | bury note / bury card |
| ! / @ | suspend note / suspend card |
| Ctrl+1..7 | flag |
| u | undo |
| i | card info |
| o | options |
| Shift+A | toggle auto-advance |
| M (capital, IO answer) | toggle masks |

---

## 3. Type in the answer

### 3.1 Template side

`{{type:Back}}` → `[[type:Back]]`. `{{type:cloze:Text}}` → `[[type:cloze:Text]]`. `{{type:nc:Back}}` → `[[type:nc:Back]]`. `nc` means "non-combining": accents and other combining marks are ignored. The placeholder survives Rust rendering untouched.

### 3.2 Question filter (`reviewer.py:706-751`)

1. Find the first `\[\[type:(.+?)\]\]`.
2. `cloze:` prefix → `clozeIdx = card.ord+1`. `nc:` prefix → `combining=False`.
3. Look the field up **by name** in the notetype (special fields are not allowed).
4. Set `typeCorrect = note[field]` (raw HTML). For cloze it becomes `col.extract_cloze_for_typing(text, ord+1) or None`. Remember the field's editor font (`f["font"]`) and size (`f["size"]`).
5. If no field is found, replace the placeholder with "Type answer: unknown field X". For cloze with nothing to type: "Please run Tools>Empty Cards".
6. If the field is empty, remove the placeholder.
7. Otherwise replace **all** occurrences with:

   ```html
   <center>
   <input type=text id=typeans onkeypress="_typeAnsPress();"
      style="font-family: 'Arial'; font-size: 20px;">
   </center>
   ```

   Pressing Enter in it sends `pycmd("ans")` → `getTypedAnswer()` → `_onTypedAnswer` → `_showAnswer`.

### 3.3 Answer filter (`reviewer.py:753-795`)

1. Remove the literal `<hr id=answer>` (only that exact spelling) and remember whether it was present (`hadHR`).
2. Let add-ons rewrite `(expected, provided)`.
3. `output = col.compare_answer(expected, provided, combining)`.
4. Replace every `[[type:…]]` with:

   ```html
   <hr id=answer>   ← only if hadHR (moves the separator to just before the comparison)
   <div style="font-family: 'Arial'; font-size: 20px">{output}</div>
   ```

5. If hadHR but no placeholder is left, return the original. The front copy of `[[type:…]]` via `{{FrontSide}}` is also replaced, so a template with `{{type:Back}}` in both front and back would show the diff twice. The stock "Basic (type in the answer)" template avoids FrontSide for exactly this reason (§6).

### 3.4 `compare_answer(expected, typed, combining)` (`rslib/src/typeanswer.rs`)

1. `expected` = `strip_av_tags`, then runs of `\n`, `<br>`, `<br/>`, `<div>`, `</div>` → one space, then `strip_html` (decodes entities and `&nbsp;`), then `trim()`. `typed` is used **as-is**: no trim, no case folding, no HTML stripping.
2. If `typed` is empty: `<code id=typeans>{html-escaped expected}</code>`.
3. `combining=true` (default): both sides are NFC-normalised into char vectors.
4. `combining=false` (`nc:`):
   - typed = NFKD with combining marks removed;
   - expected = NFKD with marks removed for comparison, but the marks are kept attached to their base chars for display. So `ば` typed as `は` counts as correct, and the expected line still shows `ば`.
5. Exact equality gives `<code id=typeans><span class=typeGood>{escaped original expected}</span></code>`.
6. Otherwise the diff uses difflib `SequenceMatcher(typed, expected).get_opcodes()`:
   - `equal` → good in both lines;
   - `delete` → bad in typed;
   - `insert` → typed gets `-` repeated `len` times as *missing*, expected gets *missing*;
   - `replace` → bad in typed, *missing* in expected.

   Output: `<code id=typeans>{typed spans}<br><span id=typearrow>&darr;</span><br>{expected spans}</code>`. Each span is `<span class=typeGood|typeBad|typeMissed>{escaped}</span>`. A token that *starts* with a combining mark gets a leading U+00A0 so it does not attach to the previous span.
7. Examples (from tests):

   ```
   ("123","1123") → <code id=typeans><span class=typeBad>1</span><span class=typeGood>123</span><br><span id=typearrow>&darr;</span><br><span class=typeGood>123</span></code>
   ("¿Y ahora qué vamos a hacer?", "y ahora qe vamosa hacer") typed:
       bad"y" good" ahora q" bad"e" good" vamos" missing"-" good"a hacer" missing"-"
     expected: missing"¿Y" good" ahora q" missing"ué" good" vamos" missing" " good"a hacer" missing"?"
   ```

**Terminal rendering: trivial.**
- Map typeGood to a green background with black text, typeBad to red/pink, typeMissed to grey. Use truecolor `#afa/#faa/#ccc`, or the ANSI 16 colours green/red/bright-black.
- Keep spaces visible (`pre-wrap`): render missing spaces as `␣` or a coloured background.
- Parse the output HTML (small and regular), or call the RPC and parse spans.
- The input field is native: a line editor widget with IME. Terminal IME is handled by the terminal itself.
- Field font and size are irrelevant.

---

## 4. Media

### 4.1 How `<img src="foo.jpg">` resolves

- Fields store filenames **unencoded**: `<img src="my file.jpg">`. The editor writes `<img src="${encodeURI(filename)}">` (`ts/routes/editor/rich-text-input/data-transfer.ts:167-177`), and the stored form is decoded again. At display time `encode_iri_paths` percent-encodes it (§2.3).
- The reviewer page's base URL is the local Flask server (`qt/aqt/mediasrv.py`). Any path that is not `/_anki/…` or an add-on path becomes `LocalFileRequest(root=col.media.dir(), path)` (`mediasrv.py:603-614`).
- `ensure_safe_path` blocks directory traversal (`:318-326`). Directories are refused. Cache is 1 h (CSS 10 s, JS 0). "Untrusted" files get a strict CSP (`UNTRUSTED_MEDIA_CSP`, `:75`).
- MIME types are hard-coded for: css, js, mjs, html, svg, png, jpg/jpeg, gif, webp, ico, json, woff/woff2/ttf/otf, mp3, mp4, webm, ogg, pdf, txt. Everything else falls back to `mimetypes`.
- `http(s)://` URLs load straight from the network.
- **tanki:** resolve `src` relative to `collection.media/` after `decode_entities` and percent-decoding. Apply the same traversal check (reject `/`, `..`, absolute paths). Remote images are opt-in (privacy; also needs an HTTP client and a cache). Use `text::extract_media_refs`-like logic; the regex `HTML_MEDIA_TAGS` (`text.rs:107-150`) is `pub static` and matches `<img|audio|video|object|source … src|data=…>` with double, single or no quotes.
- **Filename normalisation** on add/sync (`rslib/src/media/files.rs:94-138`):
  - NFC;
  - remove `[ ] < > : " / ? * ^ \ |`, ASCII control chars and unassigned code points;
  - NBSP → space;
  - Windows device names (`CON.`, …) → `CON_`;
  - a trailing space or `.` gets `_` appended;
  - truncate to `MAX_MEDIA_FILENAME_LENGTH` bytes.

  On Apple, NFD names on disk are accepted as NFC. tanki only *reads* names in the reviewer, so normalising is only needed when tanki adds media.
- `_`-prefixed files (`_font.woff2`, `_script.js`, `_style.css`) are the "keep even if unreferenced" convention. `extract_underscored_css_imports` / `extract_underscored_references` (`text.rs:170-207, 340-352`) find them in templates and CSS for media check and export. Shared decks use them for web fonts (`@font-face { src: url("_NotoSansJP.woff2") }`) and JS libraries (`<script src="_jquery.js">`).

### 4.2 Image formats in practice

- **What the editor offers:** `jpg jpeg png gif svg webp ico avif` (`data-transfer.ts:24`, `editor_legacy.py:66`, IO `is_image_file` at `image_occlusion/imagedata.rs:162`).
- **What Chromium renders:** those plus bmp and animated GIF/WebP/APNG/AVIF. **TIFF and HEIC are not rendered** by QtWebEngine, so decks practically never contain them.

| Format | tanki approach | Verdict |
|---|---|---|
| PNG/JPEG/BMP/ICO | `image` crate | trivial |
| GIF/WebP still | `image` | trivial |
| GIF/WebP/APNG animated | decode frames (`image::AnimationDecoder`). Kitty has an animation protocol (frames + gaps); for sixel/iTerm2 redraw frames on a timer, or show the first frame. | doable |
| AVIF | `image` with the `avif-native` feature (dav1d, C dependency) or a pure-Rust decoder. Check licensing and build. | doable |
| SVG (incl. LaTeX `dvisvgm` output, `--no-fonts` paths) | `resvg`/`usvg` rasteriser at terminal pixel size | doable |
| Inline `<svg>` in HTML | serialise the subtree and rasterise with resvg | doable |
| CSS `background-image` | needs CSS resolution; usually decorative, so skip | hard/skip |

- **Sizing.** CSS `img{max-width:100%;max-height:95vh}` plus inline `width=`/`height=`/`style="width:…"` attributes are common (the editor's image resizer writes `style="width: 300px;"` or a `width` attribute). Map CSS px to cells using the terminal cell pixel size (`TIOCGWINSZ` `ws_xpixel/ws_ypixel` or `CSI 16 t`). Clamp to the viewport width and to ~95% of its height.
- **Protocols:**
  - Kitty graphics: kitty, Ghostty, WezTerm, Konsole (partial). Unicode placeholders allow use inside tmux.
  - iTerm2 OSC 1337: iTerm2, WezTerm, mintty, VS Code.
  - Sixel: foot, xterm `-ti vt340`, mlterm, WezTerm, Windows Terminal ≥1.22, Konsole, contour.
  - Fallback: half-block `▀` truecolor mosaic.
  - The `ratatui-image` crate (unverified version) implements detection and all four.

### 4.3 Audio and video

- **`[sound:file]`** is used for audio *and video*. It is extracted to `SoundOrVideoTag(basename)` and played by `av_player` (`qt/aqt/sound.py:139-271`).
- **The queue:**
  - `play_tags` clears the queue and interrupts the current playback when `interrupt_current_audio` is true (config `InterruptAudioWhenAnswering`, default **true**, `main.py:750`);
  - otherwise it plays sequentially;
  - each tag goes to the highest-ranked `Player` that accepts it;
  - "no players found for …" appears as a tooltip.
- **Players** (`setup_audio`, `sound.py:939-979`):
  - `MpvManager`: one long-lived `mpv --idle` with JSON IPC over a unix socket (`qt/aqt/mpv.py`). Args: `--idle --no-terminal --force-window=no --ontop --audio-display=no --keep-open=no --autoload-files=no --gapless-audio=no --no-ytdl --config-dir=<base>`, plus on Windows `--af-add=lavfi=[apad=pad_dur=0.150]` and `--input-media-keys=no` when not Linux.
    - Play: `loadfile <path> replace -1 pause=no` (mpv ≥0.38; older drops the `-1`).
    - Stop: `stop`. Pause: `cycle pause`. Seek: `seek N relative`.
    - End of playback is detected via the `idle-active` property.
    - Keybinds q/Q/CLOSE_WIN/ctrl+w/ctrl+c are mapped to `stop`, so closing a video window doesn't kill mpv.
    - Video files open mpv's **own window** (because `--force-window=no` only suppresses windows for audio).
  - Windows also gets `SimpleMpvPlayer` (rank 1 for video: a separate process per video).
  - If mpv is missing, `SimpleMplayerSlaveModePlayer`: `mplayer -really-quiet -noautosub -slave -- <path>`, with commands on stdin (`pause`, `seek N 0`).
  - `is_audio_file`: `3gp flac m4a mp3 oga ogg opus spx wav` (`sound.py:86-102`). This only matters for the SoundPlayer/VideoPlayer split.
- **Autoplay rules:**
  - Deck option `disable_autoplay` (legacy `autoplay`): when false, play question tags on show-question and answer tags on show-answer.
  - "Skip question when replaying answer" (`skip_question_when_replaying_answer`, legacy `replayq`, inverted) controls whether replay on the answer side includes question tags.
  - Auto-advance has `wait_for_audio` and waits while `av_player.current_player` is set.
  - Manual replay: `r`/F5. Clicking a replay button plays that single tag (`play_clicked_audio`, `sound.py:924-932`).
- **HTML `<audio>`/`<video>` tags** are played by Chromium inside the page (controls, autoplay). With autoplay off, `setPlaybackRequiresGesture(True)`. Anki never passes them to mpv.
- **tanki:**
  - Drive mpv over IPC exactly like `MpvManager`. In Rust: the `libmpv2` crate, or spawn `mpv --idle --input-ipc-server=…` and speak JSON (no C linkage). **Doable.**
  - Audio: **trivial** once mpv is there. Fallbacks: `rodio`/`symphonia` for pure Rust (no AAC/opus licensing issues? opus via `symphonia` is partial). mpv is the pragmatic default.
  - **Video in a terminal:** mpv can render *inside* the terminal with `--vo=kitty` (kitty/Ghostty/WezTerm, best quality), `--vo=sixel` (needs mpv built with libsixel), or `--vo=tct` (truecolor half-blocks, universal), plus `--really-quiet`. tanki can temporarily hand over the alternate screen, or open an external mpv window when a GUI is available (`$DISPLAY`/`$WAYLAND_DISPLAY`). **Doable**, with fidelity depending on the terminal.
  - HTML `<audio src>`/`<video src>`: treat like `[sound:]`. Render a `▶ audio: file.mp3` chip and play on click/key. Honour the `autoplay` attribute only if deck autoplay is on. **Doable.**
- **Recording own voice** (Shift+V / V):
  - `RecordDialog` uses `QtAudioInputRecorder` (Qt Multimedia, wav), or `NativeMacRecorder` on macOS (`sound.py:599-878`).
  - The reviewer records with `encode=False` and plays the temporary wav back via `av_player.play_file`. The editor encodes to mp3 with `lame src.wav dst.mp3 --noreplaygain --quiet` (`_encode_mp3`, `:565`).
  - tanki: `cpal` + `hound` for wav, or `mpv`/`ffmpeg -f pulse|alsa|avfoundation|dshow`. A countdown and level meter in the TUI are easy. **Doable.**

### 4.4 Text to speech

- **Syntax:** `{{tts en_US:Field}}` and `{{tts ja_JP voices=Kyoko,Otoya speed=0.8:Field}}`, `{{tts en_US:cloze:Text}}`, or inline `[anki:tts lang=en_US]text[/anki:tts]`. The first argument must be an underscored language code. `cloze_blank=` overrides the word spoken for `[...]` (default: localized "blank").
- **Voice choice** (`qt/aqt/tts.py:84-110`), within each player:
  1. the first requested voice with a matching name *and* lang gets the player's default rank, minus 1 per position;
  2. else, for en_US, `Apple_Samantha*` (rank −50);
  3. else the first voice for the language (rank −100).

  Across players the highest rank wins. `{{tts-voices:}}` lists `{{tts LANG voices=NAME}}` lines.
- **Per OS** (`sound.py:961-978`):
  - macOS: `MacTTSPlayer` runs `say -v <voice> -r <170*speed> -f -` (text on stdin); voices come from `say -v ?`. `MacTTSFilePlayer` also exists.
  - Windows: `WindowsTTSPlayer` (SAPI via COM: `Rate = clamp(speed*10-10, −10, 10)`), and on Win10 1809+ `WindowsRTTTSFilePlayer`, which uses the **Rust** WinRT `SpeechSynthesizer` (`rslib/src/card_rendering/tts/windows.rs`: `all_voices`, `write_stream`) to write a file then played by mpv.
  - **Linux: no built-in TTS at all.** `tts/other.rs` returns "not implemented for this OS", and qt registers nothing. Linux users rely on add-ons (e.g. AwesomeTTS/HyperTTS, or the "Linux TTS" add-on).
- **tanki opportunity:** use `speech-dispatcher` (`spd-say -l <lang> -r <rate> -y <voice>`), `espeak-ng -v <lang>`, or `piper` (neural, offline) on Linux; `say` on macOS; WinRT via the rslib functions on Windows (`card_rendering::tts::write_stream` is `pub`). The `tts` crate (unverified) wraps speech-dispatcher/AVFoundation/WinRT. Language mapping: `en_US` → `en-US`/`en`. **Doable.**
- `TtsTag.field_text` is already plain text: HTML stripped, block tags turned into spaces.

---

## 5. Image occlusion (IO)

### 5.1 Notetype and storage (`rslib/src/image_occlusion/notetype.rs`)

- It is a **cloze** notetype (`OriginalStockKind::ImageOcclusion`).
- Fields, tagged with `ImageOcclusionField` (so their names can be localized or renamed; `get_io_field_indexes`, `imagedata.rs:182-215`, falls back to 0..3):
  0. `Occlusion` (cloze shapes)
  1. `Image` (`<img src="file.png">`)
  2. `Header`
  3. `Back Extra`
  4. `Comments` (deletable)
- CSS (`image_occlusion/notetype.css`), with **no** cloze CSS and no line-height:

  ```css
  #image-occlusion-canvas {
      --inactive-shape-color: #ffeba2;  --active-shape-color: #ff8e8e;
      --inactive-shape-border: 1px #212121;  --active-shape-border: 1px #212121;
      --highlight-shape-color: #ff8e8e00;  --highlight-shape-border: 1px #ff8e8e;
  }
  .card { font-family: arial; font-size: 20px; text-align: center; color: black; background-color: white; }
  ```

- Front template (English names):

  ```html
  {{#Header}}<div>{{Header}}</div>{{/Header}}
  <div style="display: none">{{cloze:Occlusion}}</div>
  <div id="err"></div>
  <div id="image-occlusion-container">
      {{Image}}
      <canvas id="image-occlusion-canvas"></canvas>
  </div>
  <script>
  try {
      anki.imageOcclusion.setup();
  } catch (exc) {
      document.getElementById("err").innerHTML = `Error loading image occlusion. Is your Anki version up to date?<br><br>${exc}`;
  }
  </script>
  ```

- Back template = front + `\n<div><button id="toggle">Toggle Masks</button></div>\n{{#Back Extra}}<div>{{Back Extra}}</div>{{/Back Extra}}\n`.

### 5.2 Shape encoding in the Occlusion field

Each shape is one cloze, one per line, generated by `shapeOrShapesToCloze` (`ts/routes/image-occlusion/shapes/to-cloze.ts:179-217`):

```
{{c1::image-occlusion:rect:left=.2325:top=.3261:width=.202:height=.0975:oi=1}}<br>
{{c2::image-occlusion:ellipse:left=.1:top=.5:rx=.05:ry=.03:angle=1250:oi=1}}<br>
{{c3::image-occlusion:polygon:left=.4:top=.4:points=.4,.4 .5,.42 .45,.55:oi=1}}<br>
{{c0::image-occlusion:text:left=.1:top=.05:text=Label\: A:scale=1.2:fs=.0400}}<br>
```

**Properties.** Values escape `\` as `\\` and `:` as `\:`. Parsing is `parse_image_cloze` (`image_occlusion/imageocclusion.rs:18-43`; nom `escaped_transform`).

| Key | Shapes | Meaning |
|---|---|---|
| `left`, `top` | all | **normalized 0–1** to image width/height (`floatToDisplay`: up to 4 decimals, leading zero stripped, 0 → `.0000`) |
| `width`, `height` | rect | normalized to image width/height |
| `rx`, `ry` | ellipse | radii, rx normalized by width, ry by height. `left/top` = bounding-box top-left |
| `points` | polygon | `"x,y x,y …"` normalized |
| `angle` | rect/ellipse/text | **stored in steps of 1/10000 turn**: degrees = (v % 10000)/10000·360 (`shapes/lib.ts:15-25`). Omitted if 0. |
| `fill` | all | CSS colour if not the default (`#ffeba2` for masks, `#000000` for text) |
| `oi` | all | `1` = occlude inactive ("Hide All, Guess One"). Absent = "Hide One, Guess One". |
| `text` | text | label text (may contain `\n`) |
| `scale` | text | uniform scale (scaleX = scaleY) |
| `fs` | text | font size normalized by **image height** (absolute px = fs·H; default 40 px if missing) |

- **Grouped shapes** share one ordinal: several clozes with the same `cN`. **Text labels** are always `c0`, and `c0` is not counted as a card.
- Coordinates are relative to the image's natural size. `getBoundingBoxSize()` is the image bounding rect in the editor canvas, and the reviewer multiplies by canvas size, which has the image aspect ratio.

### 5.3 What the cloze filter emits (`cloze.rs:332-359`, `get_image_cloze_data` in `imageocclusion.rs:51-131`)

For each IO cloze, depending on side and whether it is active for the card:

```html
<div class="cloze"           data-ordinal="1" data-shape="rect" data-left=".2325" data-top=".3261" data-width=".202" data-height=".0975" data-occludeInactive="1" ></div>   ← question & active, or ordinal 0
<div class="cloze-inactive"  data-ordinal="2" …></div>    ← other ordinals, both sides
<div class="cloze-highlight" data-ordinal="1" …></div>    ← answer & active
```

Emitted attributes: `data-shape` (only rect/ellipse/polygon/text), `data-left/top/angle/fill` (if non-empty), `data-width/height/rx/ry/scale` (if non-empty and ≠"0"), `data-points`, `data-occludeInactive` (from `oi`), `data-text` (attr-escaped), `data-font-size` (from `fs`). These sit inside the hidden `<div style="display:none">`.

### 5.4 How the reviewer draws masks (`ts/routes/image-occlusion/review.ts`)

1. Wait for the `<img>` to load. Set the container's `aspect-ratio`. Canvas pixel size = image natural size scaled to the container × devicePixelRatio, capped at 4096² (`canvas-scale.ts`).
2. Collect shapes from `.cloze` (active), `.cloze-inactive` and `.cloze-highlight` divs. Read the colours from the CSS variables on `#image-occlusion-canvas`; defaults are above.
3. Draw order:
   - **active** shapes: fill `--active-shape-color` (#ff8e8e), stroke `--active-shape-border` (1px #212121);
   - **inactive** shapes **only if** `occludeInactive`: fill = shape `fill` if it isn't the default mask colour, else `--inactive-shape-color` (#ffeba2); stroke 1px #212121;
   - **highlight** (answer side, the shape you were asked about): fill `#ff8e8e00` (transparent), stroke 1px #ff8e8e. That is a red outline.
4. Geometry (`drawShape`, `:268-381`). `toAbsolute`: `x = round(nx·W)`, `y = round(ny·H)`.
   - **Rect:** if `angle`, rotate around (left, top); `fillRect(left, top, w, h)`; `strokeRect` if the stroke width is non-zero.
   - **Ellipse:** centre = (left+rx, top+ry), radii rx, ry. If `angle`, rotate around (left, top).
   - **Polygon:** translate by (left − min(px), top − min(py)), then path through the points, close, fill, stroke. No angle.
   - **Text:**
     - `font = "{fs·H}px Arial"`, `textBaseline=top`, `scale(scale, scale)`;
     - lines = text split on `\n`; lineHeight = 1.5 × (ascent+descent of "M");
     - background rect `#ffffff` at (left/scale, top/scale), size (maxLineWidth + 5, totalHeight + 5), in scaled space;
     - text fill = shape `fill` or `#000`;
     - rotation around (left/scale, top/scale) if `angle`.
     - Text labels (c0) are always in the `.cloze` set, so they are always drawn. The `fill` passed for active shapes is ignored for Text.
5. **Toggle Masks** (answer side). The button is shown only when some element has `data-occludeinactive="1"`. The `M` key or a click redraws with only Text shapes (masks hidden), and again to restore.
6. **Modes:**
   - Hide All, Guess One (`oi=1`): question shows the active shape red and all others yellow; answer shows the active shape as a red outline and the others yellow, with toggle available.
   - Hide One, Guess One (no `oi`): question shows only the active shape red; answer shows only the red outline.

### 5.5 Compositing in Rust (tanki plan, **doable**)

1. Render the card normally with `partial_render=true` to get the cloze HTML (or skip HTML entirely).
2. Better: compute directly from note data.
   - Occlusion field → `cloze::parse_image_occlusions(text)` → `Vec<ImageOcclusion{ordinal, shapes: Vec<ImageOcclusionShape{shape, properties:[{name,value}]}>}>`. This is a `pub fn`, but **note**: it groups by ordinal and **loses the active/inactive distinction**. You derive that yourself: active = ordinal == card.ord+1, text = ordinal 0, inactive = the rest.
   - Alternatively `Collection::get_image_occlusion_note(note_id)` (`imagedata.rs:73`) returns occlusions + `occlude_inactive` + the image bytes.
   - Or parse the rendered `<div class="cloze|cloze-inactive|cloze-highlight" data-…>` with an HTML parser; this mirrors the JS exactly and respects card templates that users modified.
3. Load the image (`image` crate). Draw shapes at natural resolution with `tiny-skia` (anti-aliased fill/stroke, ellipses, rotations via transform). `imageproc` would also work but has no rotation or anti-aliasing. Text needs a font: bundle a Liberation Sans/Arimo (metric-compatible with Arial) TTF and use `ab_glyph`/`cosmic-text`, or tiny-skia + `fontdue`.
4. Scale the stroke width: 1 CSS px × DPR is relative to the *displayed* canvas, so when drawing at natural size use `max(1, natural_w / display_w_px)`.
5. Encode to PNG/RGBA and send through the graphics protocol. Cache per (note, card ord, side, toggle).
6. Read colours from the notetype CSS variables (regex on `--active-shape-color:` etc. in `nt.config.css`) so customised IO notetypes keep their colours.
7. **Fallback without graphics:** list the regions as text ("Region 3 of 7, at 23%,33%"). That is nearly useless, so IO effectively *requires* a graphics-capable terminal. Halfblock rendering of the image with masks is legible for coarse diagrams.

RPCs (`proto/anki/image_occlusion.proto:14-27`, `image_occlusion/service.rs`):
- `GetImageForOcclusion{path}` → bytes
- `GetImageOcclusionNote{note_id}` → `{note: {image_data, occlusions:[{ordinal, shapes}], header, back_extra, tags, image_file_name, occlude_inactive} | error}`
- `GetImageOcclusionFields{notetype_id}` → field indexes
- `AddImageOcclusionNotetype`
- `AddImageOcclusionNote{image_path, occlusions, header, back_extra, tags, notetype_id}`
- `UpdateImageOcclusionNote{note_id, occlusions, header, back_extra, tags}`

*Creating or editing* IO notes in a terminal needs a mask editor: mouse-drag rectangles over a graphics-protocol image, with SGR-pixel mouse mode (`CSI ?1016h`) for pixel coordinates. **Hard but possible** for rects and ellipses; polygons are fiddly.

---

## 6. Stock notetypes (`rslib/src/notetype/stock.rs`)

Default CSS for every normal notetype (`notetype/styling.css`):

```css
.card {
    font-family: arial;
    font-size: 20px;
    line-height: 1.5;
    text-align: center;
    color: black;
    background-color: white;
}
```

Cloze adds `notetype/cloze_styling.css`:

```css
.cloze {
    font-weight: bold;
    color: blue;
}
.nightMode .cloze {
    color: lightblue;
}
```

LaTeX header/footer: `header.tex` (§1.7) and `\end{document}`. Field and template names below are the English strings from `ftl/core/notetypes.ftl`; they are localized at creation time.

| Notetype | Fields | Template(s): front / back (exact strings) |
|---|---|---|
| **Basic** | Front, Back | Card 1: `{{Front}}` / `{{FrontSide}}\n\n<hr id=answer>\n\n{{Back}}` |
| **Basic (and reversed card)** | Front, Back | Card 1 as Basic. Card 2: `{{Back}}` / `{{FrontSide}}\n\n<hr id=answer>\n\n{{Front}}` |
| **Basic (optional reversed card)** | Front, Back, Add Reverse | Card 1 as Basic. Card 2: `{{#Add Reverse}}{{Back}}{{/Add Reverse}}` / `{{FrontSide}}\n\n<hr id=answer>\n\n{{Front}}` |
| **Basic (type in the answer)** | Front, Back | Card 1: `{{Front}}\n\n{{type:Back}}` / `{{Front}}\n\n<hr id=answer>\n\n{{type:Back}}` (no FrontSide, see §3.3) |
| **Cloze** | Text, Back Extra | `{{cloze:Text}}` / `{{cloze:Text}}<br>\n{{Back Extra}}` |
| **Image Occlusion** | Occlusion, Image, Header, Back Extra, Comments | see §5.1 (own CSS, `<script>`, `<canvas>`) |

Rendering stock notetypes perfectly in a terminal needs: centred text; field HTML (see §7); the `<hr id=answer>` separator as a full-width rule plus scroll anchor; cloze spans (bold blue or light blue); the type-answer input and diff; and the IO compositor. None of this needs a browser. The exception is that the IO front template has a `<script>` and a `<canvas>`, which tanki should **detect and replace** (notetype `original_stock_kind == ImageOcclusion`, or the presence of `#image-occlusion-container`) with native compositing.

Expected rendered HTML for Basic, front "Hello", back "World" (from test `notetype/render.rs:229`): answer = `Hello\n\n<hr id=answer>\n\nWorld`.

---

## 7. HTML/CSS/JS features in real cards and terminal feasibility

What the editor itself produces (`ts/routes/editor/editor-toolbar/*`; contenteditable in Chromium):

- **Inline formatting:** `<b>`, `<i>`, `<u>`, `<sub>`, `<sup>`.
- **Colour and highlight:** text colour `<span style="color: rgb(…)">` (`TextColorButton.svelte`); highlight `<span style="background-color: …">` (`HighlightColorButton.svelte`); old fields may have `<font color>`.
- **Lists and alignment:** `execCommand` gives `insertUnorderedList`/`insertOrderedList` → `<ul>/<ol><li>`; `justifyLeft/Center/Right/Full` → `<div style="text-align: …">`; `indent`/`outdent` → `<blockquote style="margin: 0 0 0 40px; border: none; padding: 0px;">` (Chromium behaviour, not verified in source).
- **Line structure:** Enter gives `<div>…</div>` or `<br>`.
- **Media and maths:** `<img src>`, `[sound:]`, MathJax `\(\)`, LaTeX `[$]`, cloze `{{c1::}}`.
- **HTML editor:** anything goes.
- **Paste filter** (`ts/lib/html-filter/element.ts`): the "extended" set keeps `a[href] b blockquote code dd dl dt em font[color] h1-h3 i li ol pre rp rt ruby span[style] strong table td/th[colspan,rowspan] tr u ul br img[src,alt] div sub sup`. Span styles are limited to `color`, `background-color` (light mode only), `font-weight`, `font-style` and `text-decoration-line`. Text pasted from the web is therefore mostly this subset.
- **Templates written by deck authors** add arbitrary CSS: flexbox, grid, `position`, web fonts, media queries, `.nightMode` and `.mobile` selectors; plus JS: AnKing-style hint buttons with keyboard shortcuts, tag display, "one by one" cloze reveal, randomisation, persistence across sides (`Persistence.js` via `sessionStorage`/`window`), tooltip libraries, and click-to-zoom images. These are from general knowledge of the ecosystem, not verified from deck files.

| Feature | Representable in a cell grid? | Verdict, approach |
|---|---|---|
| `<b>/<strong>`, `<i>/<em>`, `<u>`, `<s>/<strike>/<del>`, `text-decoration` | SGR 1/3/4/9. Undercurl and coloured underline need `CSI 4:3m`/`58:2::r:g:bm` support. | trivial |
| `color`, `background-color`, `<font color>` | truecolor SGR 38/48;2. Downgrade to 256/16 colours if `COLORTERM` lacks truecolor. Ensure contrast against the terminal background (query OSC 11). | trivial |
| `<sub>/<sup>` | Unicode super/subscript digits and some letters (²₃ᵃ). Otherwise dim plus `^`/`_` marker. | doable (lossy) |
| `<br>`, `<div>`, `<p>`, block flow | line breaks, block boundaries | trivial |
| `text-align: center/right/justify` (stock `.card` is centred) | pad per line; justify by distributing spaces | trivial |
| `<hr>`, `<hr id=answer>` | `─` full-width rule; mark as scroll anchor | trivial |
| `<ul>/<ol>/<li>`, nesting, `list-style-type` | bullets `•◦▪`, numbers, roman/alpha; indent | trivial |
| `<blockquote>`, margins/padding-left | indent (1 cell ≈ 8–10 px) | doable |
| `<table>` with borders, colspan | box-drawing grid; column widths by content measure; wrap cells | doable (colspan); **hard** (rowspan, nested tables, width percentages) |
| `<pre>`, `<code>`, `white-space: pre/pre-wrap` | monospace is native; preserve spaces | trivial |
| `<h1>-<h6>` / `font-size` | **impossible** in plain cells. Kitty text-sizing protocol (OSC 66, kitty ≥0.40) can draw scaled text. Otherwise bold + colour + underline for headings, ignore other sizes. | impossible (except kitty) |
| `font-family`, web fonts (`_font.woff2`), `@font-face` | the terminal font is fixed | impossible |
| `<ruby><rb>漢字</rb><rt>かんじ</rt></ruby>` (furigana) | layout engine places `rt` on an extra line above `rb`, centred. CJK is 2 cells wide (unicode-width). Fallback: `漢字(かんじ)` or rt dimmed after. | doable |
| `dir=rtl` / Arabic, Hebrew text | the terminal draws cells left to right. Bidi reordering via `unicode-bidi`. Most terminals do not shape Arabic (joining forms); some do (mlterm, Konsole; VTE partial). Hebrew is readable after reordering. | doable (Hebrew); **hard** (Arabic shaping) |
| Combining marks, Indic scripts, emoji ZWJ | width tables vary by terminal (grapheme-cluster width mode 2027 in some terminals) | doable with glitches |
| `<img>` inline | graphics protocol in a cell rectangle; a small inline image inside a text line forces a tall line | doable (block); **hard** (true inline flow) |
| Inline `<svg>` | rasterise with resvg | doable |
| `<a href>` | OSC 8 hyperlink, opened in the browser | trivial |
| `<details><summary>` | collapsible, toggled by key or mouse | doable |
| Hint filter (`a.hint` + hidden `div.hint`) | recognise the exact pattern; show `▸ FieldName` and reveal on a key (e.g. `h`) | trivial |
| `display:none` / `visibility:hidden` (inline or via CSS classes, incl. `.card1 .x {display:none}`) | needs a selector-matching CSS cascade | doable (essential) |
| `.nightMode` / `.night_mode` selectors | set the class when the terminal background is dark; match in the cascade | doable |
| `.card1`/`.card2`, `.mobile`, `.win/.mac/.linux`, `.isWin/.isMac/.isLin`, `.chrome` | synthesize classes: `card cardN isLin nightMode night_mode` on body, `linux chrome` on html. `.mobile` is off. | trivial |
| Media queries (`@media (max-width…)`, `prefers-color-scheme`) | evaluate against the virtual viewport in px (cells × cell px) | doable |
| CSS `position`, `float`, `flex`, `grid`, `transform`, absolute overlays | needs a real layout engine. Approximate flex rows as horizontal concatenation; ignore positioning. | hard (approx) / impossible faithfully |
| Borders, border-radius, box-shadow, gradients, `background-image` | box-drawing for solid borders; the rest is impossible | doable (borders) / impossible |
| `opacity`, `filter`, animations, transitions | ignore. Approximate opacity by blending fg into bg. | impossible |
| Wrapping and line-height | own wrapping (unicode-linebreak, UAX#14). Line-height is ignored. | doable |
| `<script>` (inline and `_lib.js`) | needs a JS engine **and** a DOM. A bare JS engine (boa/rquickjs) is useless without a DOM implementation. | impossible natively; raster mode only |
| `onclick` handlers, buttons, `<input>` other than typeans | pattern-match well-known idioms only (hint, toggle-next-sibling); otherwise drop | hard |
| MathJax `\(..\)` `\[..\]` | Unicode approximation, or rasterise (§1.8) | doable/hard |
| LaTeX `[latex]` images | existing PNG/SVG via the graphics protocol; generate via latex+dvipng if enabled | doable |
| IO canvas | native compositor (§5.5) | doable |
| `<audio>/<video>` HTML | play chip + mpv; video via mpv `--vo=kitty/sixel/tct` | doable |
| `<iframe>` (YouTube embeds), `<object>` | show the URL and open it externally | impossible inline |
| Text selection/copy | native terminal selection works in text mode, not on images | free (text mode) |

---

## 8. Rendering strategy for a terminal

### 8.1 Strategy A: native text renderer (recommended default)

Pipeline:
1. `render_existing_card(cid, false, true)`, resolve FrontSide and filters, `extract_av_tags`, `extract_latex` (→ `<img class=latex>`), `encode`-free filename handling.
2. **Detect special notetypes:** IO (original stock kind / `#image-occlusion-container`) goes to the IO compositor.
3. Handle `[[type:…]]` natively: on the question side insert an input widget; on the answer side compare via `CardRenderingService::compare_answer` and render the diff spans in colour, using the reviewer.py logic for the `<hr id=answer>` move.
4. **Parse:** `<style>{css}</style>` + side HTML with `html5ever` (via `scraper`, `kuchiki` or `markup5ever_rcdom`), wrapped in a synthetic `<html class="linux chrome [night-mode]"><body class="card cardN isLin [nightMode night_mode]">`.
5. **Cascade:** parse `<style>` blocks and inline `style=""` with `cssparser`/`lightningcss`; match selectors with the `selectors` crate (as `scraper` does), with specificity and inheritance for a whitelist of properties:
   `display`, `visibility`, `color`, `background-color`, `font-weight`, `font-style`, `text-decoration(-line)`, `text-align`, `white-space`, `list-style-type`, `direction`, `margin-left`/`padding-left` (indent), `font-size` (only for heading or kitty-sizing heuristics), `width`/`height` on images, `border-*` (solid only), `vertical-align` (sub/sup).

   An option worth evaluating: **use Stylo via `blitz-dom`** (DioxusLabs Blitz) purely as a style engine to get Chromium-grade computed styles, then do our own cell layout. Blitz maturity and API stability are unverified.
6. **Layout into cells:** block/inline formatting context; UAX#14 line breaking (`unicode-linebreak`); widths via `unicode-width`; bidi via `unicode-bidi`; ruby as two-line boxes; tables with a min/max-content algorithm; images as rectangles sized from the cell pixel metrics.
7. **Paint** to a ratatui buffer or a direct ANSI stream. Images go through the graphics protocol (`ratatui-image`). Interactive items (replay buttons, hints, details, links) are registered as hit regions for keyboard focus and mouse.
8. **Night mode:** query OSC 11 for the terminal background. Dark → add `nightMode` classes **and** apply the `body.nightMode` override (fg/bg = the terminal's defaults instead of the card's white/black). Ensure a minimum contrast for author colours (e.g. adjust `color: black` on a dark terminal), as Anki's paste filter does.

**Loses:**
- arbitrary layout (flex/grid/positioning/float);
- font family and size (except kitty OSC 66);
- all JS (scripted hints, persistence, randomisation, tag widgets, custom IO hooks);
- CSS visual effects;
- MathJax fidelity (unless rasterised);
- web fonts;
- iframes;
- precise image/text co-flow.

**Keeps:** text, colour, emphasis, cloze, type-answer, audio/TTS, images, IO, ruby, tables, lists, links, and selectable text. Fast (sub-millisecond to a few ms per card) and needs no dependencies beyond mpv.

Estimated coverage: stock notetypes and editor-made content are close to 100%. Heavily templated shared decks (AnKing, some language decks with JS) degrade but stay readable, *provided* `display:none` and class-based hiding are honoured.

### 8.2 Strategy B: faithful raster mode

Render the complete page (the same shell as §2.2: reviewer.css plus the card HTML and CSS, body classes, MathJax, card scripts) in a headless engine at the terminal's pixel width, screenshot it, and show the bitmap.

Engine options:

| Engine | JS | Fidelity | Cost |
|---|---|---|---|
| Headless Chromium via CDP (`chromiumoxide` / `headless_chrome` crates) | yes | identical to Anki (same engine family) | needs Chrome or Chromium installed (~150 MB), ~100–300 MB RAM; a warm render is maybe 50–200 ms (unverified estimate); full-page screenshot via `Page.captureScreenshot` with `captureBeyondViewport` |
| WebKitGTK / WPE offscreen | yes | high | Linux-centric, C dependencies |
| Servo (embeddable, `libservo`) | yes (SpiderMonkey) | improving, gaps | huge build |
| Blitz (Stylo + Taffy + Vello) | **no** | good CSS, no JS | pure Rust, young |
| litehtml / wkhtmltoimage | no / old | mediocre / obsolete | skip |

Implementation notes:
- Serve media through a tiny local HTTP server (or CDP request interception, `Fetch.requestPaused`) rooted at `collection.media`, mirroring `mediasrv.py`.
- Inject `reviewer.css`-equivalent CSS and a stub `pycmd`/`bridgeCommand` that records calls (`ans`, `ease3`, `play:q:0`) for tanki to act on.
- Bundle MathJax (or load it from the vendored Anki build).
- Port `_updateQA`: re-execute scripts and typeset MathJax, then wait for an `onShown` promise before the screenshot.
- Re-render on resize.
- Map mouse clicks from terminal cells to page pixels (`Input.dispatchMouseEvent`) to support hint buttons and custom JS.
- Keyboard can be forwarded for card-level shortcuts.
- Keep type-answer native: replace `[[type:]]` with a placeholder box, then overlay a terminal input on top, or just accept input in the status line.

**Loses:**
- crisp terminal-native text (it becomes a bitmap at an arbitrary DPI, so scaling blurs it);
- the terminal theme and font;
- text selection;
- works only on graphics-capable terminals (half-block fallback makes small text unreadable);
- latency and memory;
- an external browser dependency;
- scrolling long cards means cropping a tall bitmap;
- over SSH, sending large images on every card is slow (Kitty with shared memory only works locally);
- inside tmux, images need passthrough or Kitty unicode placeholders.

**Keeps:** everything visual, including JS and MathJax. In-page `<audio>`/`<video>` playback works only if we bridge it (Chromium plays audio headlessly, which is fine; video is not visible beyond the screenshot), so better to strip it and use mpv.

### 8.3 Strategy C: hybrid with escape hatches (recommended overall)

1. **Default to A.** Run a **complexity classifier** over the rendered HTML and CSS:
   - card `<script>` other than recognised idioms (the IO setup script, known hint/persistence snippets);
   - CSS `position:absolute|fixed`, `display:flex|grid`, `float`, `transform`;
   - `<canvas>` (non-IO), `<iframe>`, `<object>`;
   - `<input>`/`<button>` (non-typeans);
   - heavy MathJax (display math, `\begin{…}`);
   - inline `<svg>` with text;
   - web fonts for scripts the terminal lacks (rare).
2. **If complex:** (a) if a raster engine is available and the terminal supports graphics, offer or automatically use **B** for that notetype (a per-notetype setting remembered in tanki config); (b) else render A with a status-line badge "⚠ this card uses JS/CSS layout tanki can't show; press `B` to open in browser".
3. **Open in browser escape hatch:** write `<!doctype html><html class=…><head><base href="file:///…/collection.media/"><style>{reviewer-like css}{card css}</style><script src=mathjax…></script></head><body class="card cardN …">{html}</body></html>` to a temp file in the media folder (or serve it over localhost) and `xdg-open`/`open` it. It covers any card on any desktop. Remote or SSH sessions can't use it, apart from printing a localhost URL for port forwarding.
4. **Per-element fallbacks within A:**
   - MathJax: Unicode, then cached raster (latex/dvisvgm or JS MathJax → resvg), then raw TeX source in a dim colour.
   - Unknown scripts: drop, but keep their static DOM.
   - Tables too wide: horizontal scroll or reflow into "row cards".
   - Images without graphics support: `[image: name.jpg 640×480]` placeholder, with a key to open it externally.

### 8.4 rslib helpers useful to a non-web frontend

| Helper | Path | Use |
|---|---|---|
| `Collection::render_existing_card` / `render_uncommitted_card` | `notetype/render.rs:46,72` | the core render (use `partial_render=true`, §1.5) |
| `template::render_card`, `ParsedTemplate::from_text` | `template.rs:597,196` | render arbitrary templates (card layout preview) |
| `card_rendering::extract_av_tags` / `strip_av_tags` / `prettify_av_tags` | `card_rendering/mod.rs:13-33` | AV extraction, stripping, one-line display |
| `text::strip_html`, `strip_html_preserving_entities`, `decode_entities` | `text.rs:227-245` | plain-text fallback |
| `text::html_to_text_line(html, preserve_media_filenames)` | `text.rs:213` | one-line previews (browser list, deck overview); removes `[[type:]]` and `[sound:]` |
| `text::strip_html_for_tts`, `strip_html_preserving_media_filenames`, `contains_media_tag`, `is_html`, `HTML_MEDIA_TAGS` (pub static), `replace_media_refs` (pub) | `text.rs` | media discovery |
| `text::extract_media_refs`, `encode_iri_paths`, `decode_iri_paths` | `text.rs:280,559,566` | `pub(crate)`; via RPC `EncodeIriPaths`/`DecodeIriPaths` (not needed when not using a browser) |
| `cloze::reveal_cloze_text(text, ord, question)`, `reveal_cloze_text_only`, `extract_cloze_for_typing`, `cloze_numbers_in_string`, `cloze_number_in_fields`, `parse_image_occlusions` | `cloze.rs` | cloze rendering, IO shapes |
| `image_occlusion::imageocclusion::parse_image_cloze`, `get_image_cloze_data` | `image_occlusion/imageocclusion.rs:18,51` | IO shape parsing |
| `Collection::get_image_occlusion_note`, `add_…`, `update_…` | `image_occlusion/imagedata.rs` | IO data and editing |
| `latex::extract_latex(text, svg)` | `latex.rs:63` | LaTeX → img + filenames to generate |
| `CardRenderingService::compare_answer` | `card_rendering/service.rs:166` | type-answer diff (the module itself is private) |
| `CardRenderingService::render_markdown` | `service.rs:128` | Markdown → HTML (private module) |
| `card_rendering::tts::all_voices` / `write_stream` | `card_rendering/tts/mod.rs` | Windows-only TTS |
| `browser_table` row rendering | `browser_table.rs:299-345` | ready-made q/a one-liners for the browser |
| `template::field_is_empty` | `template.rs:543` (`pub(crate)`) | re-implement (one regex) |

Nothing in rslib does HTML parsing, CSS, layout, MathJax or IO raster. tanki must build or borrow those (html5ever, cssparser/selectors or Stylo, resvg, tiny-skia, image, ratatui-image).

### 8.5 Feature verdict summary

| Feature | Verdict |
|---|---|
| Template engine, filters, cloze, special fields, empty-card messages | **trivial** (reuse rslib); FrontSide/AV ordering glue is trivial |
| Add-on filters | **impossible** (no Python); drop silently, keep `current_text` |
| `tts-voices` | trivial (own implementation) |
| AV extraction, replay buttons, autoplay/replay rules, queueing | trivial |
| Audio playback (mpv IPC) | doable |
| Video | doable (mpv window or `--vo=kitty/sixel/tct`) |
| HTML `<audio>/<video>` | doable |
| Record own voice | doable |
| TTS (Linux via speech-dispatcher/espeak/piper, mac `say`, Windows via rslib WinRT) | doable (and better than Anki on Linux) |
| Type answer input + diff colours | trivial |
| LaTeX existing images | doable (graphics); generation via latex/dvipng: doable |
| MathJax | doable (Unicode approx) / hard (faithful raster) |
| Images (png/jpg/gif/webp/svg/avif) | doable with graphics protocol; impossible as faithful pictures without one (halfblocks are coarse) |
| Animated GIF/WebP | doable (kitty animation; else first frame) |
| Image occlusion review | doable (native compositor) |
| Image occlusion editing | hard |
| Bold/italic/underline/strike/colours/highlight | trivial |
| Lists, hr, alignment, blockquote, pre/code | trivial |
| Tables | doable / hard (rowspan, nested) |
| Ruby/furigana | doable |
| RTL | doable (Hebrew) / hard (Arabic shaping depends on the terminal) |
| `display:none` + class/night-mode/cardN selectors | doable (needs a cascade) |
| Font size | impossible (kitty OSC 66 exception) |
| Fonts/web fonts | impossible |
| Flex/grid/positioning/transforms | hard (approx) / impossible faithfully in A; available in B |
| Card JavaScript | impossible in A; available in B (headless Chromium) |
| `<iframe>`/embeds | impossible inline; open externally |

---

## 9. Unverified points and open questions

- Chromium's exact markup for `indent` and `justify*` execCommand output (inferred from Chromium behaviour, not from Anki source).
- Real-world shared-deck feature usage (AnKing JS, Persistence.js, and so on) comes from ecosystem knowledge; no decks were inspected.
- Crate names and maturity: `ratatui-image`, `blitz-dom`, `chromiumoxide`, `tts`, `libmpv2`, and any MathJax-to-SVG Rust crate were **not verified** in this session. Check versions before committing.
- Headless Chromium render latency figures are estimates.
- Kitty text-sizing protocol (OSC 66) availability: kitty ≥0.40 from memory; other terminals unknown.
- Whether QtWebEngine in Anki's builds plays proprietary codecs (H.264/AAC) inside HTML `<video>`/`<audio>` was not checked. tanki sidesteps this by using mpv.
- `RenderCardOutput` is unnameable outside the crate (private module, `pub(crate)` re-export). Calling `render_existing_card` from an external crate and using its fields via inference should compile, but this was not compiled here (the repo is read-only, no builds). The proto service path is the guaranteed-public alternative.
