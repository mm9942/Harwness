# codex-rs TUI Deep Read — Composer/Textarea Cursor & Wrap Model

Source read in full: `codex-rs/tui/src/bottom_pane/textarea.rs` (3837 lines),
`bottom_pane/textarea/vim.rs` (320), `bottom_pane/chat_composer.rs` (11438 lines,
read at struct/dispatch granularity — see note in §2), `bottom_pane/chat_composer/{draft_state,footer_state,popup_state,slash_input,history_search}.rs`,
`bottom_pane/chat_composer_history.rs` (1505), `bottom_pane/paste_burst.rs` (580),
`clipboard_paste.rs` (568), `app/input.rs` (300), plus `wrapping.rs` (1657) and
`keymap.rs` (defaults block, lines 909–1153) which the textarea depends on for
wrap algorithm and key bindings respectively. Compared against harw-tui's
`src/input_editor.rs` (1070), `src/app.rs` (input wiring), `src/tui_event.rs`.

---

## 1. Text-buffer and cursor data model

### `TextArea` (`bottom_pane/textarea.rs:100`)

```rust
pub(crate) struct TextArea {
    text: String,                          // raw UTF-8 buffer, single String, \n = line break
    cursor_pos: usize,                      // BYTE offset into `text`, always on a char boundary
    wrap_cache: RefCell<Option<WrapCache>>, // memoized wrap-line byte ranges, keyed by width
    preferred_col: Option<usize>,           // "sticky column" for vertical movement
    elements: Vec<TextElement>,             // atomic placeholder spans (paste refs, /commands, @mentions)
    next_element_id: u64,
    kill_buffer: String,                    // single-entry (not a ring) Emacs-style kill buffer
    kill_buffer_kind: KillBufferKind,        // Characterwise | Linewise (drives Vim p/P semantics)
    vim_enabled: bool,
    vim_mode: VimMode,                       // Normal | Insert
    vim_pending: VimPending,                 // None | Operator(op) | TextObject{op,scope}
    editor_keymap: EditorKeymap,
    vim_normal_keymap: VimNormalKeymap,
    vim_operator_keymap: VimOperatorKeymap,
    vim_text_object_keymap: VimTextObjectKeymap,
}

struct WrapCache { width: u16, lines: Vec<Range<usize>> }  // byte ranges per wrapped line

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct TextAreaState { scroll: u16 }  // index of first visible wrapped line

struct TextElement { id: u64, range: Range<usize> }  // atomic span, e.g. "[Paste #1 34 lines]"
```

**Cursor unit: byte offset, not line+column and not grapheme index.** All
movement functions compute a target byte offset and then snap it to a valid
boundary via `clamp_pos_to_char_boundary` / `clamp_pos_to_nearest_boundary`
(textarea.rs:1529–1563). There is no separate `(row, col)` field — row/col is
always *derived* on demand from `cursor_pos` plus the current wrap cache
(`cursor_pos_with_state`, textarea.rs:415–426) or from `\n` scanning for
logical-line queries (`beginning_of_line`/`end_of_line`, textarea.rs:464–488).

**Grapheme awareness**: single-character movement steps by *extended grapheme
cluster*, not Unicode scalar value, using
`unicode_segmentation::GraphemeCursor` (`prev_atomic_boundary` /
`next_atomic_boundary`, textarea.rs:1634–1684). This also treats an entire
`TextElement` span (e.g. a paste placeholder or `/command` name) as one atomic
unit — cursor movement jumps over the whole element instead of stepping
through its characters (same two functions, checked before falling back to
grapheme segmentation).

**Column semantics for vertical movement**: `current_display_col()`
(textarea.rs:432) and `preferred_col: Option<usize>` compute/cache the
*display width* (via `unicode_width::UnicodeWidthStr::width()`, not char
count) of the prefix from line-start to cursor. This is the "sticky column"
that survives across repeated `Up`/`Down` through short lines, exactly like
a normal terminal editor.

### Vim sub-state (`bottom_pane/textarea/vim.rs`)

```rust
enum VimMode { Normal, Insert }
enum VimOperator { Delete, Yank, Change }
enum VimPending { None, Operator(VimOperator), TextObject { operator, scope: VimTextObjectScope } }
enum VimMotion { Left, Right, Up, Down, WordForward, WordBackward, WordEnd, LineStart, LineEnd }
enum VimTextObjectScope { Inner, Around }
enum VimTextObject { Word, BigWord, Parentheses, Brackets, Braces, DoubleQuote, SingleQuote, Backtick }
```

Text objects (`iw`, `aw`, `i(`, `a"`, …) are computed on demand by scanning
the buffer for matching delimiter/word runs around the cursor
(`word_text_object_range`, `paired_text_object_range`,
`quoted_text_object_range` — vim.rs:130–298); nothing is cached.

### `ChatComposer` (`bottom_pane/chat_composer.rs:369`)

The textarea is one field inside a larger `ChatComposer`, wrapped by
`DraftState` (`bottom_pane/chat_composer/draft_state.rs:11`):

```rust
pub(super) struct DraftState {
    pub(super) textarea: TextArea,
    pub(super) textarea_state: RefCell<TextAreaState>,   // scroll offset, mutated during render
    pub(super) is_bash_mode: bool,                        // "!" prefix mode
    pub(super) pending_pastes: Vec<(String, String)>,      // placeholder -> real pasted text
    pub(super) input_enabled: bool,
    pub(super) input_disabled_placeholder: Option<String>,
    pub(super) paste_burst: PasteBurst,
    pub(super) disable_paste_burst: bool,
    pub(super) mention_bindings: HashMap<u64, ComposerMentionBinding>,
    pub(super) recent_submission_mention_bindings: Vec<MentionBinding>,
}
```

`ChatComposer` itself (chat_composer.rs:369–409) additionally owns:
`popups: PopupState`, `history: ChatComposerHistory`, `footer: FooterState`,
`attachments: AttachmentState`, plus cached keymap snapshots
(`editor_keymap`, `vim_normal_keymap`, `submit_keys`, `queue_keys`, …) copied
from `RuntimeKeymap` at construction/config-update time (not re-resolved per
keystroke).

---

## 2. Key → action table

Two independent keymaps matter: the **editor keymap** (non-Vim, always
active unless Vim normal mode intercepts first) and the **Vim keymaps**
(normal/operator/text-object), all resolved from `RuntimeKeymap::defaults()`
at `keymap.rs:909` (`built_in_defaults()`). Bindings are `Vec<KeyBinding>` —
several physical keys can map to one action (cross-terminal modifier
reporting differences, e.g. AltGr, Alt+Backspace vs Ctrl+Backspace).

### 2a. Editor keymap (non-Vim insert mode) — `keymap.rs:942-1002`

| Action | Default bindings |
|---|---|
| `insert_newline` | `Ctrl+J`, `Ctrl+M`, `Enter`, `Shift+Enter`, `Alt+Enter` |
| `move_left` | `Left`, `Ctrl+B` |
| `move_right` | `Right`, `Ctrl+F` |
| `move_up` | `Up`, `Ctrl+P` |
| `move_down` | `Down`, `Ctrl+N` |
| `move_word_left` | `Alt+B`, `Alt+Left`, `Ctrl+Left` |
| `move_word_right` | `Alt+F`, `Alt+Right`, `Ctrl+Right` |
| `move_line_start` | `Home`, `Ctrl+A` |
| `move_line_end` | `End`, `Ctrl+E` |
| `delete_backward` | `Backspace`, `Shift+Backspace`, `Ctrl+H` |
| `delete_forward` | `Delete`, `Shift+Delete`, `Ctrl+D` |
| `delete_backward_word` | `Alt+Backspace`, `Ctrl+Backspace`, `Ctrl+Shift+Backspace`, `Ctrl+W`, `Ctrl+Alt+H` |
| `delete_forward_word` | `Alt+Delete`, `Ctrl+Delete`, `Ctrl+Shift+Delete`, `Alt+D` |
| `kill_line_start` (kill to BOL, Emacs `Ctrl+U`) | `Ctrl+U` |
| `kill_whole_line` | *(unbound by default)* |
| `kill_line_end` (kill to EOL, Emacs `Ctrl+K`) | `Ctrl+K` |
| `yank` (paste kill buffer) | `Ctrl+Y` |

`Home`/`Ctrl+A` at beginning-of-line toggles to *previous* line's start
(`move_up_at_bol`, textarea.rs:580–591); `End`/`Ctrl+E` at end-of-line
advances to the *next* line's end (`move_down_at_eol`, :592–603) — Emacs-style
"repeat wraps to adjacent line" behavior, but only for the `a`/`e` chord, not
for plain `Home`/`End`.

Plain printable `KeyCode::Char(c)` with `NONE`/`SHIFT` modifiers and not
`is_ascii_control()` inserts the character (textarea.rs:605–617). AltGr
combos (`ALT|CONTROL` on Windows) are special-cased to insert the character
literally instead of triggering a Ctrl-shortcut (textarea.rs:515–526,
`is_altgr()`).

### 2b. Vim normal-mode keymap — `keymap.rs:1003-1046`

| Action | Default | Effect |
|---|---|---|
| `enter_insert` | `i`, `Insert` | → Insert mode at cursor |
| `append_after_cursor` | `a` | cursor += 1 grapheme, → Insert |
| `append_line_end` | `A`/`shift-a` | cursor = EOL, → Insert |
| `insert_line_start` | `I`/`shift-i` | cursor = first non-blank, → Insert |
| `open_line_below` | `o` | insert `\n` after EOL, → Insert |
| `open_line_above` | `O`/`shift-o` | insert `\n` before BOL, → Insert |
| `move_left/right/up/down` | `h`/`l`/`k`/`j` + arrows | grapheme/visual-line motion |
| `move_word_forward` | `w` | → `beginning_of_next_word()` |
| `move_word_backward` | `b` | → `beginning_of_previous_word()` |
| `move_word_end` | `e` | → `vim_word_end_cursor()` |
| `move_line_start` | `0` | → BOL |
| `move_line_end` | `$`/`shift-$` | → last char before EOL |
| `delete_char` | `x` | delete-forward-kill 1 grapheme |
| `substitute_char` | `s` | delete char under cursor, → Insert |
| `delete_to_line_end` | `D`/`shift-d` | kill cursor..EOL |
| `change_to_line_end` | `C`/`shift-c` | kill cursor..EOL, → Insert |
| `yank_line` | `Y`/`shift-y` | yank whole current line (linewise) |
| `paste_after` | `p` | paste kill buffer after cursor (or as new line if linewise) |
| `start_delete_operator` | `d` | → `VimPending::Operator(Delete)` |
| `start_yank_operator` | `y` | → `VimPending::Operator(Yank)` |
| `start_change_operator` | `c` | → `VimPending::Operator(Change)` |
| `cancel_operator` | `Esc` | clear pending |

### 2c. Vim operator-pending keymap (after `d`/`y`/`c`) — `keymap.rs:1048-1066`

| Action | Default | Notes |
|---|---|---|
| `delete_line` / `yank_line` | `d` / `y` (repeat) | `dd`/`yy` = linewise whole-line |
| `motion_left/right/up/down` | `h`/`l`/`k`/`j` | characterwise/linewise range to cursor |
| `motion_word_forward/backward/end` | `w`/`b`/`e` | word-motion range |
| `motion_line_start/end` | `0` / `$` | |
| `select_inner_text_object` | `i` | → `VimPending::TextObject{scope:Inner}` |
| `select_around_text_object` | `a` | → `VimPending::TextObject{scope:Around}` |
| `cancel` | `Esc` | abort operator |

### 2d. Vim text-object keymap (after `di`/`da`/etc.) — `keymap.rs:1067-1093`

`w`→Word, `W`/`shift-w`→BigWord, `(`/`)`/`b`→Parentheses,
`[`/`]`→Brackets, `{`/`}`/`B`/`shift-b`→Braces, `"`→DoubleQuote,
`'`→SingleQuote, `` ` ``→Backtick, `Esc`→cancel.

### 2e. Composer-level (outside the textarea proper) — `keymap.rs:932-940`

`submit`=`Enter`, `queue`=`Tab` (queue draft while a turn is running),
`toggle_shortcuts`=`?`/`Shift+?`, `history_search_previous`=`Ctrl+R`,
`history_search_next`=`Ctrl+S`. These are checked *before* falling through
to the textarea's own keymap (`handle_key_event_without_popup`,
chat_composer.rs:3081–3211).

---

## 3. Line-wrap algorithm and recalculation timing

Wrapping is delegated to the `textwrap` crate via a thin adapter,
**recomputed lazily and cached per width**:

```rust
fn wrapped_lines(&self, width: u16) -> Ref<'_, Vec<Range<usize>>> {   // textarea.rs:1817
    let mut cache = self.wrap_cache.borrow_mut();
    let needs_recalc = match cache.as_ref() {
        Some(c) => c.width != width,
        None => true,
    };
    if needs_recalc {
        let lines = crate::wrapping::wrap_ranges(
            &self.text,
            Options::new(width as usize).wrap_algorithm(textwrap::WrapAlgorithm::FirstFit),
        );
        *cache = Some(WrapCache { width, lines });
    }
    ...
}
```

**Invalidation, not eager recompute**: every mutating op
(`insert_str_at`, `replace_range_raw`, `replace_element_payload`, …) calls
`self.wrap_cache.replace(None)` (e.g. textarea.rs:349, :374, :1417) to drop
the cache. The next call that needs wrapped lines — cursor positioning
(`cursor_pos_with_state`), rendering (`render_ref`/`render_ref_masked`), or
`desired_height(width)` — triggers exactly one recompute, memoized by width
until the next edit or a width change (terminal resize). So recompute
frequency is *at most once per render frame*, driven by whichever caller asks
first, not once per keystroke on top of that.

**Wrap algorithm**: `textwrap::WrapAlgorithm::FirstFit` (greedy), no
hyphenation config override in the textarea path (default word-wrap at
Unicode word boundaries, using `unicode_width` for column math, so double-width
CJK/emoji glyphs count as 2 columns — see `wide_unicode_wraps_by_display_width`
test in wrapping.rs). Note: the composer *input field* wrapping does **not**
use the URL-preserving `adaptive_wrap_line` path (that's reserved for
transcript/history-cell rendering in `wrapping.rs`) — the input field always
uses plain `wrap_ranges` with default options.

`wrap_ranges` (wrapping.rs:42) returns byte `Range<usize>` per wrapped line
**including trailing whitespace plus a +1 sentinel byte** (used so
`cursor_pos_with_state` can place the cursor one column past the last visible
char, e.g. at true EOL). Handling `Cow::Owned` lines from `textwrap` (which
happen when it inserts a hyphenation penalty char not present in the source)
requires `map_owned_wrapped_line_to_range` to walk char-by-char and
reconstruct the true source byte range — this is fully unit-tested against
edge cases like indent-prefix/source-char collisions (wrapping.rs:1508-1656).

**Height**: `desired_height(width) = wrapped_lines(width).len() as u16`
(textarea.rs:405-407) — i.e. the textarea's contribution to composer height is
simply the wrapped-line count, **uncapped inside the textarea itself**. The
composer computes total height as
`textarea.desired_height(inner_width) + remote_images_height + separator + 2 (borders) + footer/popup height`
(`desired_height_with_textarea_right_reserve`, chat_composer.rs:4161-4193).
No hard max-height clamp exists in the textarea or composer layer — a very
long multi-line draft grows the input box until it would exceed the terminal;
that is handled by the enclosing pane layout, not textarea.rs.

**Cursor position on screen**: `cursor_pos_with_state` (textarea.rs:415-426)
looks up which wrapped-line range contains `cursor_pos`
(`wrapped_line_index_by_start`, a `partition_point` binary search over
line-start offsets), computes the in-line display column via
`.width()`, and combines with `effective_scroll()` (textarea.rs:1843-1869) —
a small state machine that keeps the cursor's wrapped-line index inside
`[scroll, scroll+area_height)`, scrolling minimally, and snapping to 0 when
content fits entirely.

**Up/Down across wraps**: `move_cursor_up`/`move_cursor_down`
(textarea.rs:1194-1323) first try the *wrap cache* (visual-line movement across
wrapped segments of one logical line), falling back to logical `\n`-scanning
only if no wrap cache exists yet. Both preserve `preferred_col` (sticky
column) the same way logical-line movement does.

---

## 4. History-recall mechanic

Implemented in `bottom_pane/chat_composer_history.rs` as a dedicated state
machine, `ChatComposerHistory`, decoupled from the textarea/composer widgets
(pure logic, unit-tested independently, ~1500 lines including tests).

**Combined offset space**: persistent (cross-session, on-disk) history
entries at offsets `[0, persistent_entry_count)`, followed by in-session
`local_history: Vec<HistoryEntry>` at
`[persistent_entry_count, persistent_entry_count + local_history.len())`.
`history_cursor: Option<isize>` is `None` when not browsing.

```rust
pub(crate) struct HistoryEntry {
    text: String,
    text_elements: Vec<TextElement>,        // placeholder ranges (large-paste, etc.)
    local_image_paths: Vec<PathBuf>,
    remote_image_urls: Vec<String>,
    mention_bindings: Vec<MentionBinding>,   // @tool / $app references
    pending_pastes: Vec<(String, String)>,   // placeholder -> real text, restored verbatim
}
```

So Up/Down recall restores not just the string but the *entire draft state*
(placeholders, attachments, mentions) — not plain text substitution.

**Trigger gate** (`should_handle_navigation`, chat_composer_history.rs:343-361):
Up/Down only navigate history if either (a) the textarea is empty, or (b) the
current text exactly equals `last_history_text` (the last entry recalled) AND
the cursor sits at byte 0 or at `text.len()` (a line boundary). This is the
mechanism that lets multi-line drafts use Up/Down for normal cursor movement
once the user has edited a recalled entry or moved the cursor into the
interior — it stops "feeling like" shell history at that point. In the
composer's key dispatch this check happens *before* falling back to plain
`Up`/`Down` cursor movement (`handle_key_event_without_popup`,
chat_composer.rs:3176-3210): if navigation applies, it wins; otherwise the key
falls through to `handle_input_basic` → `TextArea::move_cursor_up/down`.

**Async persistent lookups**: `navigate_up`/`navigate_down`
(chat_composer_history.rs:368-424) return `Option<HistoryEntry>` immediately
for local (in-memory) entries but return `None` and fire
`AppEvent::LookupMessageHistoryEntry{thread_id, offset, log_id}` when the
target offset is a persistent entry not yet cached in `fetched_history:
HashMap<usize, HistoryEntry>`. The response re-enters via
`on_entry_response(log_id, offset, entry, tx)` (chat_composer_history.rs:433),
which is guarded by `persistent_log_id` so a stale response after a session
reset is silently ignored (`HistoryEntryResponse::Ignored`).

**Ctrl+R incremental search** is a separate state machine
(`HistorySearchState`, chat_composer_history.rs:187-221) layered on top: it
keeps `unique_matches: Vec<UniqueHistoryMatch>` (already-discovered matches in
newest→oldest order) plus `seen_texts: HashSet<String>` for
exact-text de-duplication scoped to the active search session, and an
`awaiting: Option<PendingHistorySearch>` for in-flight async persistent
lookups so a boundary hit (`HistorySearchResult::AtBoundary`) is
distinguishable from a genuine miss (`NotFound`) and from a still-pending
fetch (`Pending`).

---

## 5. Paste handling (burst detection, bracketed paste, large-paste placeholders)

Two independent paste ingestion paths converge on one function,
`ChatComposer::handle_paste(pasted: String) -> bool` (chat_composer.rs:889):

```rust
pub fn handle_paste(&mut self, pasted: String) -> bool {
    let pasted = pasted.replace("\r\n", "\n").replace('\r', "\n");
    let pasted = sanitize_user_text(&pasted);
    let char_count = pasted.chars().count();
    if char_count > LARGE_PASTE_CHAR_THRESHOLD {          // = 1000 (chat_composer.rs:281)
        let placeholder = self.next_large_paste_placeholder(char_count);
        self.draft.textarea.insert_element(&placeholder);  // atomic element, e.g. "[Paste #1 +842 chars]"
        self.draft.pending_pastes.push((placeholder, pasted));  // real text kept aside, swapped back in on submit
    } else if char_count > 1 && self.image_paste_enabled() && self.handle_paste_image_path(pasted.clone()) {
        self.draft.textarea.insert_str(" ");                // path looked like an image; attach + placeholder
    } else {
        self.insert_str(&pasted);                            // small text paste: literal insert
    }
    self.draft.paste_burst.clear_after_explicit_paste();
    self.sync_popups();
    true
}
```

### 5a. Bracketed paste path (terminals that support it)

`crossterm::event::Event::Paste(text)` arrives as one atomic event from the
terminal (bracketed paste mode enabled at startup). The app-level loop
(`app.rs:1282-1289`) normalizes `\r`→`\n` and calls
`self.chat_widget.handle_paste(pasted)` directly — one call, one string, no
burst heuristics involved, because the terminal has already told us
unambiguously "this is a paste."

### 5b. Paste-burst path (terminals without reliable bracketed paste — notably Windows)

`bottom_pane/paste_burst.rs` implements `PasteBurst`, a **pure state
machine that never touches the textarea directly** — callers interpret its
`CharDecision`/`FlushResult` and apply edits. Rationale (from the module's
own doc comment): on some platforms a paste arrives as a rapid stream of
individual `KeyCode::Char`/`Enter` events indistinguishable from very fast
typing, so the composer must infer "this looks like a paste" from timing.

Key constants (paste_burst.rs:154-165):

```rust
const PASTE_BURST_MIN_CHARS: u16 = 3;                       // chars needed before assuming a burst
const PASTE_ENTER_SUPPRESS_WINDOW: Duration = 120ms;         // Enter → newline instead of submit, briefly after burst activity
const PASTE_BURST_CHAR_INTERVAL: Duration = 8ms;             // max gap between chars to count as one burst
const PASTE_BURST_ACTIVE_IDLE_TIMEOUT: Duration = 8ms (60ms on windows);  // idle time before flushing buffer as Paste
```

State machine flow (driven from `ChatComposer::handle_input_basic_with_time`,
chat_composer.rs:3269-3412):

1. First fast ASCII char → `CharDecision::RetainFirstChar`: **not** inserted
   yet, held in `pending_first_char: Option<(char, Instant)>` to avoid
   flicker if a burst follows.
2. Second char within 8ms → `BeginBufferFromPending`: starts `buffer: String`
   with the held char plus this one; `active = true`.
3. Subsequent chars within 8ms → `BufferAppend`, accumulated into `buffer`.
4. `Enter` while buffering → appended as `\n` into the buffer (not submit).
5. On a UI tick or before any non-plain-char key, `flush_if_due(now)` checks
   elapsed time against the idle timeout: if exceeded while `active`, emits
   `FlushResult::Paste(buffer)` which is routed through the same
   `handle_paste()` as bracketed paste; if only a lone `pending_first_char`
   timed out with no burst following, emits `FlushResult::Typed(ch)` — the
   char is inserted as ordinary typing.
6. **Retro-capture**: if `>= 3` consecutive fast chars have *already* been
   inserted as normal typing before the burst is recognized (can happen on
   the non-ASCII/IME path, which never holds the first char),
   `decide_begin_buffer` retroactively removes the already-inserted prefix
   from the textarea (`replace_range(start_byte..cursor, "")`) and moves it
   into the burst buffer, so the eventual paste event still sees one
   contiguous string. Heuristic for "looks pastey enough to retro-grab":
   contains whitespace OR is `>= 16` chars (paste_burst.rs:391-411).
7. `newline_should_insert_instead_of_submit(now)`: true while actively
   buffering OR within the 120ms Enter-suppression window after the last
   buffer activity — this is what prevents a multi-line paste's embedded
   newlines from prematurely submitting the message.

### 5c. Large-paste placeholder

Both ingestion paths funnel through `handle_paste`, so the 1000-char
threshold applies uniformly regardless of *how* the paste was detected. The
placeholder is inserted as an atomic `TextElement` (`insert_element`,
textarea.rs:1461) — cursor movement, deletion, and Ctrl+K/Ctrl+U/Ctrl+W
treat it as one indivisible unit (`prev_atomic_boundary`/`next_atomic_boundary`
consult `elements` before falling back to grapheme segmentation). The real
text is kept in `DraftState.pending_pastes: Vec<(placeholder, real_text)>`
and swapped back in at submit time; deleting the placeholder element removes
the matching `pending_pastes` entry (`reconcile_deleted_elements`,
chat_composer.rs:3421-3434, called after every `handle_input_basic_with_time`
edit that could have deleted an element).

### 5d. Image-path paste

If the pasted content isn't over threshold but parses as a filesystem path
to a readable image (`normalize_pasted_path` in `clipboard_paste.rs:251`,
handling `file://` URLs, quoted paths, and Windows/WSL path conversion), it's
attached as an image instead of inserted as literal text
(`handle_paste_image_path`, chat_composer.rs:910-931).

---

## 6. `app/input.rs` — app-level key dispatch above the composer

`App::handle_key_event` (`app/input.rs:92-255`) sits one layer above
`ChatComposer` and intercepts, in order:

1. **Agent-switch fallback shortcuts** (Alt+Left/Right for
   previous/next-agent navigation) — but *only* when the composer draft is
   empty, explicitly to avoid stealing word-motion keys from the textarea
   when there's text to edit (`allow_agent_word_motion_fallback`,
   input.rs:102-138).
2. **Side-conversation return shortcut**.
3. **App-scope keymap shortcuts** (`toggle_vim_mode`, `toggle_fast_mode`,
   `toggle_raw_output`, `open_transcript`, `open_external_editor`) — gated by
   `app_keymap_shortcuts_available()` (no overlay, no modal/popup active).
4. **Esc**: routed to backtrack-priming logic
   (`should_handle_backtrack_esc`/`should_reject_side_backtrack_esc`) *unless*
   `chat_widget.should_handle_vim_insert_escape(key_event)` is true — i.e.
   Vim insert-mode Escape (→ Normal mode) always wins over app-level Esc
   semantics like backtrack-priming.
5. Anything else, plus the fallback of all the above: forwarded to
   `self.chat_widget.handle_key_event(key_event)`, which is the entry point
   into `ChatComposer::handle_key_event` described in §2.

This confirms the layering: **App owns global/cross-cutting shortcuts and
explicitly special-cases "composer is empty" to avoid conflicting with
in-progress edits; ChatComposer owns everything once a key reaches it,
dispatching first to whichever popup is active, then to the Vim/editor
keymap, then to the raw TextArea.**

---

## 7. Comparison — current harw-tui state

`harw-tui/src/input_editor.rs` (`InputEditor`, 1070 lines incl. tests) is
already meaningfully more than a "primitive input line" — it has cursor
movement, Home/End, word-jump, Ctrl+Backspace/Delete, and a
history-back/forward stack with snapshot-restore-on-Escape. But compared to
codex's textarea it differs in several structural ways:

| Aspect | codex `TextArea` | harw-tui `InputEditor` |
|---|---|---|
| Cursor unit | byte offset, snapped to grapheme cluster boundary via `GraphemeCursor` | byte offset, snapped to **char** boundary only (`prev_char_boundary` uses `is_char_boundary`, not grapheme segmentation) — will place the cursor mid-grapheme-cluster for combining marks/ZWJ emoji |
| Vertical movement column | display-width-based (`unicode_width`), sticky `preferred_col`, cache-aware (visual-line-first, logical fallback) | char-count-based (`chars().count()`), no display-width awareness, no sticky-column cache — CJK/wide chars will misalign row math |
| Wrapping | `textwrap` FirstFit word-wrap, cached per width, invalidated on edit, unicode-width aware | fixed-width **hard char cut** (`visible_lines`, textarea.rs-equivalent at input_editor.rs:486-511) — breaks words mid-token, no width awareness for wide glyphs |
| Multi-line vs history heuristic | boundary-gated: Up/Down navigate history only if text is empty or (matches last-recalled AND cursor at line boundary) | binary: `is_multiline()` (buffer contains any `\n`) decides Up/Down globally — a single-line draft with cursor mid-text will still trigger history recall since there's no "matches last recalled" gate |
| Atomic elements (paste placeholders, slash-commands) | first-class `TextElement` spans; cursor/delete/word-jump all element-aware | none — no placeholder concept |
| Paste handling | bracketed paste + non-bracketed burst-detection state machine + 1000-char placeholder threshold | bracketed paste only, direct `insert_str`, no CR normalization, no burst detection, no large-paste placeholder |
| Vim mode | full modal editing (operators, text objects, motions) | none |
| Kill/yank (Ctrl+K/U/W/Y) | full Emacs-style kill buffer, linewise/characterwise | none (only Ctrl+Backspace/Delete word-delete) |
| Keymap | data-driven `RuntimeKeymap`, user-configurable, conflict-validated | hardcoded `match` in `handle_key` |

### Concrete migration proposal for harw-tui

1. **New module `harw-tui/src/textarea.rs`** (parallel structure to codex's,
   not a verbatim port — no Vim mode needed initially, no `TextElement`
   system needed until harw-tui gets paste-placeholder/mention features):
   - Own `text: String`, `cursor_pos: usize` (byte offset), `preferred_col:
     Option<usize>`, and a `wrap_cache: RefCell<Option<WrapCache>>` keyed by
     width, invalidated on every mutating call.
   - Replace `prev_char_boundary`/char-count-based movement with
     `unicode_segmentation::GraphemeCursor` for `move_left`/`move_right`,
     and `unicode_width::UnicodeWidthStr::width()` for column math in
     vertical movement — this is the single highest-value fix since it's a
     silent correctness bug today (grapheme-cluster cursor corruption) with
     German/EU text (combining diacritics) as a live risk, not just an edge
     case.
   - Replace `visible_lines()`'s hard-cut wrapping with a call to the
     `textwrap` crate (already a transitive dep via other TUI machinery, or
     add directly) using `WrapAlgorithm::FirstFit`, mirroring
     `wrap_ranges`/`wrapped_lines` (textarea.rs:1817-1836) — cache-and-invalidate
     the same way.
   - `desired_height(width) -> u16` replacing the current fixed height
     assumption in whatever draws the input box in `app.rs`, so the composer
     grows/shrinks with wrapped line count exactly as codex's does (§3).
2. **`harw-tui/src/chat_composer_history.rs`** (new, or extend
   `InputEditor`'s existing `history`/`history_pos`/`history_snapshot`
   fields in place): add the **boundary gate**
   (`should_handle_navigation`) so Up/Down only recall history when the
   textarea is empty or the cursor sits at byte 0/`len()` *and* the current
   text matches the last-recalled entry — this directly fixes the
   single-line-mid-cursor Up/Down ambiguity noted above. `harw-tui`'s
   history is currently local-session-only (`Vec<String>` capped at 100), so
   the async-persistent-lookup half of codex's design (§4) is not needed
   unless harw-tui grows cross-session history persistence later.
3. **`harw-tui/src/app.rs` `TuiEvent::Paste` handler** (currently
   `app.rs:853-857`, a bare `app.input.insert_str(&text)`): add `\r\n`/`\r`
   normalization to match codex's `handle_paste` (chat_composer.rs:890), and
   optionally a large-paste threshold if harw-tui's transcript rendering
   should avoid dumping thousands of characters into one visible line —
   codex's `TextElement`/placeholder system is the more involved piece and
   can be deferred; a simpler first cut could just truncate/summarize in the
   footer without full atomic-element tracking.
   - Paste-burst detection (§5b) is lower priority for harw-tui unless it
     specifically targets Windows terminals without bracketed paste — if it
     does, `paste_burst.rs` can be ported close to verbatim since it's
     already a self-contained, terminal-agnostic pure state machine with no
     dependency on `TextArea` internals.
4. **Key dispatch layering**: harw-tui's `handle_key` (`app.rs:1111` region)
   currently mixes popup-key-handling and plain-input handling in one large
   function. Codex's structure — `App::handle_key_event` (app-global
   shortcuts) → `ChatComposer::handle_key_event` (popup dispatch) →
   `handle_key_event_without_popup` (textarea keymap) — is a useful target
   shape if/when harw-tui's popup surface grows beyond the current command
   popup, since it keeps "is a popup eating this key" and "textarea consumed
   this key" as clearly separated concerns.

Files most relevant on the harw-tui side for this migration:
`/srv/dev-shared/projects/rust/harwness/harw-tui/src/input_editor.rs`,
`/srv/dev-shared/projects/rust/harwness/harw-tui/src/app.rs` (composer wiring
around lines 240, 352, 845-857, 1111-1290),
`/srv/dev-shared/projects/rust/harwness/harw-tui/src/tui_event.rs` (Paste
event already plumbed via bracketed paste, `Event::Paste` → `TuiEvent::Paste`
at line 139).
