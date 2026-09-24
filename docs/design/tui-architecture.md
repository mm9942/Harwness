# harw-tui Architecture

> Status: implemented · Last reviewed: 2026-09-24

This document describes `harw-tui`'s own internal architecture: how the
chat loop is driven, how a redraw gets scheduled, how the chat history is
represented and rendered, how the color theme is chosen, and how first-time
setup works. It reflects the code in `harw-tui/src/{app.rs,events.rs,
frame_requester.rs,history_cell.rs,style.rs,setup.rs,input_editor.rs,
chat_scroll.rs,command_popup.rs}` as of this review.

This design was shaped in part by studying how other terminal-based coding
agents structure their event loop and rendering — an async event bus, a
coalesced redraw signal, typed history cells, and a state-machine-driven
setup flow are common answers to the same problems. The names, types, and
implementation below are Harwness's own.

---

## 1. The chat loop in one paragraph

`ChatApp` (`app.rs`) owns the interactive chat loop and renderer state, but
mounts no runtime itself — session, registry, sandbox, spawn context,
approval chain, and services are assembled elsewhere (`harw_runtime::
RuntimeAssembly`) and wired into `ChatApp` by the TUI's composition root.
The loop drives turns through `harw_core::run_turn` and runs an
**asynchronous** `tokio::select!` event loop over three sources:

- **Terminal input** (`TuiEvent`) from a blocking reader thread
  (`input_reader::spawn_input_reader`), so reading stdin never stalls the
  select loop.
- **Application events** (`HarwEvent`) from the internal event bus (§2).
- **Frame requests**, coalesced by a scheduler task that turns bursts of
  redraw requests into `TuiEvent::Draw` (§3).

The loop also folds in a handful of idle-driven wake sources alongside
these three — a waker for background-agent notifications, and idle ticks
for the kanban board and the live agent-tree view, each active only while
its panel is open, so the loop doesn't busy-poll when nothing needs it.

Terminal setup (raw mode, bracketed paste, and the alternate screen) is
held behind an RAII guard (`TerminalGuard`) that restores the terminal on
every exit path — error, panic, or a clean quit. The chat history is kept
in memory as a list of cells (`ChatApp::cells`, §4) and rendered through a
scrollable widget inside that full-screen layout, not through the
terminal's own scrollback. This is a deliberate divergence from a purely
inline-viewport design (append-only output above a fixed bottom composer,
the way some other terminal agents render): harw-tui uses the alternate
screen and owns its own scroll region instead, so it can re-render history
cells on resize (§4) and keep a stable full-screen layout with panels.
Internal scrolling (PageUp/PageDown, and more generally §6) moves a scroll
offset within that history region.

Pure, TTY-free line classification lives in `classify_line`; key handling
lives in `handle_key`, which only mutates state and emits `HarwEvent`s, so
it's testable without a terminal. Since the underlying model-provider
abstraction doesn't expose token-level streaming, an assistant response is
streamed to the screen as a simulated typing effect after the turn
completes, not as it's generated.

---

## 2. Event bus (`events.rs`)

`events.rs` defines `HarwEvent`, the channel through which widgets and
background tasks send events to the main loop without needing direct
`&mut ChatApp` access.

- **`HarwEvent`** is the enum of everything that can happen: `Submit(String)`
  (the user confirmed their input with Enter), `SystemMessage(String)` (a
  system line to show in the chat history — errors, status info),
  `Command(String)` (a submitted `/command` line, or a `!`-shell/`#`-note/
  `@`-mention line, to be run asynchronously through the operation-adapter
  pipeline), and `Quit` (a clean shutdown request).
- **Channel model**: an unbounded MPSC channel
  (`tokio::sync::mpsc::unbounded_channel`). Many senders — widgets, spawned
  tasks — each hold a cloned `HarwEventSender`; one receiver
  (`UnboundedReceiver<HarwEvent>`) lives in the main loop.
- **Concurrency**: `HarwEvent` is `Send + Sync`; `HarwEventSender` is
  `Clone + Send`. A send error (the receiver already dropped) is silently
  discarded — a lost event at shutdown does no harm.

Keeping `Command` execution as an event rather than a direct call is what
lets `handle_key` itself stay synchronous: it only decides *that* a command
should run and emits the raw line; the actual execution (including
`Operation::run`) happens in the async main loop.

## 3. Frame requester (`frame_requester.rs`)

A lightweight mechanism that lets any part of the code ask for a redraw,
without needing to know about the renderer or hold a lock on it.

- **`FrameRequester`** wraps an `UnboundedSender<()>`. It is `Clone + Send +
  Sync`; every clone shares the same channel, so it doesn't matter which
  clone calls `schedule_frame()` — it's a request for *a* redraw, not a
  specific one.
- **`schedule_frame()`** sends `()` on the internal channel immediately. A
  failed send (the receiver already closed) is ignored — a dropped draw
  signal causes no data corruption, just a possibly-stale frame that a
  later signal corrects.
- **`frame_channel()`** constructs the channel and returns
  `(FrameRequester, UnboundedReceiver<()>)`. The receiver stays with the
  event-loop's scheduler task in `app.rs`, which drains every pending
  signal, then sleeps `MIN_FRAME_INTERVAL` (8ms, roughly a 120 FPS ceiling)
  before turning the next signal into a `TuiEvent::Draw`. That coalescing —
  many cheap, uncoordinated `schedule_frame()` calls collapsing into one
  throttled redraw — is what keeps a burst of state changes (streamed
  tokens, several child agents updating at once) from spamming the
  terminal with redraws.

## 4. History cells (`history_cell.rs`)

The chat history is a list of typed cells behind one `HistoryCell` trait —
each cell owns its own render logic — with several concrete
implementations for different kinds of content:

- **`PlainHistoryCell`** — immutable lines (system messages, for example).
- **`UserHistoryCell`** — user input, rendered with a `"> "` prefix and
  word wrapping.
- **`AssistantHistoryCell`** — an assistant response; keeps the source text
  and re-wraps it differently per render width, so a terminal resize
  re-flows old messages correctly instead of leaving them wrapped for a
  width that no longer applies.
- **`ReasoningHistoryCell`** — a reasoning summary, dimmed and italic with
  a `"∴ "` marker and an optional note of which agent it came from;
  collapsed by default, expandable via Ctrl+O through a shared, mutable
  handle (`SharedReasoningCell`) so all reasoning cells expand together.
- **`SubAgentCell`** — a running or finished child agent
  (`TurnEvent::ChildSpawned`/`ChildProgress`/`ChildCompleted`); unlike the
  cells above, this one is **updatable in place** via
  `apply_progress`/`apply_completion`, so a child's status line evolves
  without appending new cells for every progress tick.
- **`PlanGraphCell`** — a compact view of a `harw_plan::Plan`, truncating
  when there are many nodes and reporting how many were left out.
- **`GoalCell`** — a goal statement rendered against a
  `harw_plan::goal::GoalReport`.
- **`ToolCell`** / **`ToolGroupCell`** — the display of one or more tool
  calls. A single, shared cell is updated across a tool call's lifecycle
  (requested → running → result) rather than appearing as two separate,
  immutable history items for the call and its result; this replaced an
  earlier two-item design. Tool-call approval no longer appears as a
  history cell at all — it runs entirely through a dedicated approval
  dialog overlay while the question is open, and only the resolved tool
  call remains in the history afterward.

Two free helper functions support all of the above: `wrap_plain` (word-wise
wrapping) and `truncate_chars` (character-safe truncation that never splits
a multi-byte character).

**Terminal safety**: every piece of text that isn't a fixed literal owned
by this module — model output, tool arguments/results, plan and goal text,
user text — is run through a sanitizer before rendering: flowing text
through a display-sanitizing function, single-line fields through an
inline-sanitizing one. (The approval dialog is the one exception with its
own reveal-safe sanitizing, since it must show *exactly* what's being
approved without swallowing anything meaningful.) This keeps ANSI escape
sequences, C0/C1 control characters, and bidi/zero-width characters from
ever reaching the ratatui buffer — a malicious tool result or file mention
can't repaint the terminal or hide characters in approval text. Truncation
always happens *before* sanitizing, so a truncation marker never lands in
the middle of a multi-byte sequence.

## 5. Style and theming (`style.rs`)

The single place that produces `ratatui` styles and colors for the rest of
the rendering code — no other module constructs a `Style` or `Color`
directly for themed output.

- **`Theme`** — a `Dark`/`Light` discriminator, `Copy` and freely usable
  across threads (it carries no state).
- **`detect_theme()`** — reads the `COLORFGBG` environment variable, which
  many terminals (rxvt, xterm, konsole, and others) set to reflect the
  configured foreground/background colors, and classifies the scheme from
  it. With no `COLORFGBG` set, it falls back to `Theme::Dark` as the safer
  default (a light-on-dark assumption fails more gracefully than the
  reverse in most terminals).
- **`is_light`**, **`accent_color`**, **`selected_style`**, **`dim_style`**
  — theme-dependent style helpers used throughout the rendering layer.
- **`luminance`** — a weighted-brightness calculation, kept as a building
  block for finer-grained RGB background detection beyond the coarse
  dark/light split.

All functions here are pure — no internal locks, no shared state — so they
can be called from any thread. Unknown or unparseable `COLORFGBG` values
fall back to safe defaults rather than erroring.

## 6. Setup and onboarding (`setup.rs`)

The interactive first-time setup flow (provider → auth → model) that
`harw-cli`'s onboarding wires up (see `docs/design/provider-tui-setup.md`
for the surrounding provider-catalog design).

- **`SetupApp`** is the **I/O-free** state machine for the whole flow —
  provider selection, endpoint selection, auth method selection, model
  selection — driven entirely through `SetupApp::on_key`, so every
  transition is unit-testable without a real terminal.
- **`SetupStage`** enumerates the flow's stages: `Provider`, `Endpoint`,
  `Api`, `Auth`, `Model`, `Done`.
- **`run_setup`** is the thin `ratatui` loop that couples `SetupApp` to a
  real terminal — reading key events, rendering the current stage, and
  restoring the terminal through the same kind of RAII guard `app.rs` uses.
- Credential detection (finding a usable key or token in an existing local
  auth file) is delegated to `harw_model_catalog::detect_local_sources`
  rather than duplicated here — `setup.rs` only drives the picker over
  whatever that function reports.
- **`SetupOutcome`** is the result of a completed setup: enough to write
  the chosen provider, model, and credential reference back to
  configuration.

`SetupApp` is a pure value type with no internal shared state; `run_setup`
runs synchronously on the calling thread and spawns no further threads of
its own.

## 7. Composer: the input editor (`input_editor.rs`)

`InputEditor` is a self-contained, in-memory editor for the chat input
line: buffer management, cursor navigation, an input-history stack, and
translation of terminal key events into editor actions. It owns all of
that state; rendering stays with the caller (`app.rs`), which reads back
`visible_lines()` and `cursor_position()` to draw the composer each frame.
`handle_key` returns an `InputAction` describing what the caller should do
next (for example, submit) rather than acting on the world itself, keeping
the editor decoupled from the event bus and the rest of the app.

A `PendingPaste` type tracks the metadata of a not-yet-expanded paste
placeholder: a large paste is held as a placeholder in the buffer rather
than inserted character-by-character, and expanded on demand, so pasting a
large block of text doesn't flood the editor with per-character key
processing.

`InputEditor` is not bound to `Send`/`Sync` — the caller is responsible for
serializing access (naturally satisfied by living entirely inside the
single-threaded TUI event loop). No mutex or `Arc` is needed.

## 8. Pager and scrolling (`chat_scroll.rs`)

`ChatScroll` encapsulates only the scroll state of the chat history view —
no rendering, no access to the history cells themselves, and no knowledge
of message content. The caller owns the cell list and passes in just
`total_lines` (the total line count) and `viewport` (the visible height)
for clamping calculations; `ChatScroll` decides where the offset should
land, and `ScrollAction` is what its event handlers return to describe the
result.

`ChatScroll` is `Copy`, so it's trivially cheap to pass around and store —
no internal mutex or `Arc` is needed there either. This split (a small,
pure scroll-state type separate from the cells it scrolls over) is what
lets both the main chat view and other scrollable overlays (the help
screen, knowledge browsers, and similar panels) reuse the same scrolling
logic instead of each reimplementing offset math and clamping.

## 9. Command popup (`command_popup.rs`)

The `/`-command completion popup that opens as soon as a line starts with
`/`. `CommandPopup` tracks its own filter text, selection, and open/closed
`PopupMode`; `PopupAction` and `TabOutcome` describe what a key press
should do to it — complete the current entry, move the selection, or close
it — following the same "state machine returns an action, the caller
applies it" shape as the input editor and setup flow above. It completes
both command names and subcommands, sourced from the command registry
described in `docs/design/tui-command-contract.md` §8.
