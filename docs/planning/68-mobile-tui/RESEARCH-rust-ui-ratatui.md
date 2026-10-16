---
id: PL-68-RESEARCH-RUST-UI
title: "Rust UI and ratatui — research and gap analysis for harw-tui"
status: research
date: 2026-10-02
tags: [research, tui, ratatui, crossterm, rust-ui, accessibility, unicode]
related:
  - README.md
  - ../../design/interaction-contract.md
  - ../../design/tui-command-contract.md
---

# Rust UI and ratatui — research and gap analysis

> Research document, no implementation claimed. Sources are linked inline and
> listed at the end; claims about harw are verified against the tree (paths
> given) and claims about third parties come from the linked pages (read
> 2026-10-02). Where a page was thin, that is said.

## 1. Where harw stands today

- `harw-tui` pins `ratatui 0.29` (feature `unstable-rendered-line-info`) and
  `crossterm 0.29`, plus `unicode-width 0.2` (`harw-tui/Cargo.toml`). Layout
  helpers live in `harw-tui-layout`.
- Fullscreen **alternate screen**, raw mode, bracketed paste and mouse capture
  behind a RAII `TerminalGuard` (`harw-tui/src/app.rs`, ~l.3559 ff.). History is
  kept in `ChatApp::cells` and drawn as a scrollable `Paragraph`; there is
  deliberately **no `insert_before` / terminal scrollback** (module doc).
- Input comes from a blocking reader thread (`input_reader`), app events from an
  internal bus, redraws are coalesced by `frame_requester`/`frame_scheduler`.
  No tokio `EventStream`.
- Streaming is *simulated* after the turn (the `ModelProvider` has no token
  stream), see `app.rs` module doc.
- Not present (grep): kitty keyboard enhancement flags, focus-change events,
  `Viewport::Inline`, `insta` snapshots (7 files use `TestBackend`),
  accessibility/`NO_COLOR` handling in `harw-tui`.
- Planned: portrait/phone layout with a pinned agent dock (`README.md` here).

## 2. ratatui: what the docs establish

**Rendering model.** Immediate mode: every frame all visible widgets render into
a buffer; `Terminal::flush` diffs against the previous buffer and writes only
changed cells ([rendering under the hood](https://ratatui.rs/concepts/rendering/under-the-hood/)).
Consequences from the [FAQ](https://ratatui.rs/faq/): one `draw()` per loop
(only the last one survives double buffering); widgets must clamp to the buffer
(`area.intersection(buf.area)`) or they panic; on Windows crossterm reports
press *and* release, so filter `KeyEventKind::Press`; mixed crossterm major
versions cause type mismatches (select via ratatui feature flags); async is
only needed when other parts of the app need it, rendering itself is cheap.

**Application patterns.** Three documented: Elm (model/update/view), Component
(trait with `init`, `handle_events`/`handle_key_events`/`handle_mouse_events`,
`update`, `render`) and Flux
([patterns](https://ratatui.rs/concepts/application-patterns/),
[components](https://ratatui.rs/concepts/application-patterns/component-architecture/)).
Component trade-off per the docs: co-located logic and private state vs.
scattered application logic and careful action propagation. Event handling may
be centralized, centralized-with-messages, or distributed
([events](https://ratatui.rs/concepts/event-handling/)). The official
templates are `async-app` (tokio, background tasks) and `component-app`
([template](https://ratatui.rs/templates/component/)); the async loop is a
`tokio::select!` over crossterm `EventStream`, a tick interval and an mpsc
channel, with an explicit `Render` event
([async stream](https://ratatui.rs/tutorials/counter-async-app/async-event-stream/)).

**0.30 / 0.30.1** ([0.30](https://ratatui.rs/highlights/v030/),
[0.30.1](https://ratatui.rs/highlights/v0301/),
[BREAKING-CHANGES](https://github.com/ratatui/ratatui/blob/main/BREAKING-CHANGES.md)):

| Area | Change | Relevance to harw |
|---|---|---|
| Workspace | split into `ratatui-core`, `-widgets`, `-crossterm`, … | widget crates can depend on `ratatui-core` only (less churn) |
| `ratatui::run()` | default init/restore wrapper | harw keeps its own guard (panic-safe, alt screen, paste, mouse); not a replacement |
| Backend trait | associated `Error`, mandatory `clear_region()` | only matters for custom backends |
| `Block::title` | `Title` removed, takes `Into<Line>`; `TitlePosition` | grep `block::Title` before upgrading |
| `Flex::SpaceAround` | semantics changed; `SpaceEvenly` = old | check dock/dialog layouts |
| `Style` | no longer `Styled` | drop `Stylize` on `Style` values |
| `List::highlight_symbol` | `Into<Line>` (styled) | non-const |
| `Alignment` | renamed `HorizontalAlignment` | mechanical |
| Layout | `Rect::layout()`/`try_layout()`/`layout_vec()`, `centered*()` | simplifies `harw-tui-layout` placement code |
| 0.30.1 | `CellDiffOption`, `CellWidth` trait, block shadows, `Cell::column_span`, `Fill` widget, iterator-based `BufferDiff` (no temp vec), MSRV 1.88 | `CellDiffOption` addresses escape-sequence cells (image/OSC); `CellWidth` is the first-party answer to the width problem (§5) |
| 0.29 → | `Table::highlight_style`→`row_highlight_style`, `Tabs::select(Into<Option<usize>>)`, `Rect::area()`→`u32` | harw is on 0.29 and already past this |

**Testing.** `TestBackend` + `insta` snapshots is the documented pattern
([snapshots](https://ratatui.rs/recipes/testing/snapshots/)); it does *not*
capture colours, and the docs suggest pseudo-terminal end-to-end tests for the
event loop. The ratatui repo itself expects unit and doc tests next to the code
and `TestBackend` for output validation
([contributing](https://github.com/tui-rs-revival/ratatui/blob/main/CONTRIBUTING.md)).
Codex CLI, the closest peer (Rust, chat + approvals), requires `insta`
snapshots for UI changes and subscribes the TUI to core events instead of
calling core synchronously
([architecture write-up](https://codex.danielvaughan.com/2026/03/28/codex-rs-rust-rewrite-architecture/)).
That write-up is a secondary source.

## 3. Ecosystem (what is worth knowing, with maturity as stated by the sources)

- Text input: `tui-textarea` (Emacs bindings, undo/redo, search, single-line
  mode) and its maintained fork `ratatui-textarea`
  ([tui-textarea](https://github.com/rhysd/tui-textarea),
  [crates.io](https://crates.io/crates/ratatui-textarea)); `tui-input` is
  headless. harw has its own `input_editor.rs`; the sources do not say how
  these handle IME or wide graphemes, so do not assume.
- `tui-widgets` workspace: popup, scrollview, scrollbar, prompts, big-text,
  qrcode, cards, bar-graph, equalizer ([repo](https://github.com/ratatui/tui-widgets)).
  `tui-popup`/`tui-scrollview` overlap harw's dialog/scroll code.
- `ratatui-image`: sixel, kitty, iTerm2, halfblocks fallback; detection by env,
  then control-sequence query, then fallback; encoding must be offloaded from the
  render thread ([repo](https://github.com/ratatui/ratatui-image)).
- `tachyonfx`: 50+ effects applied *after* widgets render
  (`process_effects()` on the frame buffer) ([repo](https://github.com/ratatui/tachyonfx)).
- Frameworks: `tui-realm` (Elm/React-like, opinionated), `rat-salsa`
  (event queue, tasks, timers, focus, dialogs), `rat-widget` (forms, focus,
  menubar), `ratatui-kit` (component hooks) — see
  [awesome-ratatui](https://github.com/ratatui/awesome-ratatui). Only the
  listing was read, not their code.
- Utilities: `terminput`, `termprofile` (capability detection),
  `tui-pantry` (component development).

## 4. Terminal input and protocol facts

- crossterm exposes key, mouse, resize, focus (opt-in) and bracketed-paste
  events; mouse capture and focus events are off until enabled; `EventStream`
  (async) cannot be mixed with `read()`/`poll()`
  ([crossterm event](https://docs.rs/crossterm/latest/crossterm/event/index.html)).
- Kitty keyboard protocol: five opt-in flags (disambiguate, event types
  press/repeat/release, alternate keys, all keys as escapes, associated text),
  negotiated with `CSI ? u`, flag stack per screen, Enter/Tab/Backspace stay
  legacy ([spec](https://sw.kovidgoyal.net/kitty/keyboard-protocol/)).
  crossterm's `KeyboardEnhancementFlags` is the Rust entry point. Value for
  harw: reliable `Esc` vs `Alt`, `Ctrl+Enter`/`Shift+Enter` for multi-line send,
  proper key-up for hold-to-confirm dialogs (`sudo_dialog.rs` already inspects
  `KeyEventKind::Repeat`).
- Inline viewports (`Viewport::Inline`, `insert_before`) and scrolling regions
  exist in ratatui (`scrolling-regions` feature reduces flicker,
  [crate docs](https://docs.rs/ratatui/latest/ratatui/)); the dedicated docs
  page returned 404, so details beyond the feature flag are unverified here.

## 5. Unicode width — the real risk for an agent TUI

Model output contains emoji, CJK, combining marks and ZWJ sequences.
`unicode-width` is per-codepoint; terminals disagree with it and with each
other, which shifts borders and cursors
([ratatui discussion](https://github.com/ratatui/ratatui/discussions/1438),
[grapheme clusters in terminals](https://mitchellh.com/writing/grapheme-clusters-in-terminals)).
Mode 2027 (query with `CSI ? 2027 $ p`) lets an app learn/enable grapheme
clustering; the recommendation is to query it rather than assume, and to
measure with a cursor-position query where layout depends on it. harw uses
`unicode-width` in `input_editor.rs` (cursor column), `matrix_view.rs` and
`app.rs`. Ratatui 0.30.1's `CellWidth` trait is the first-party hook to
centralise this.

## 6. Accessibility

Sources agree on the failure modes: full-screen redraws move the cursor, box
drawing is read aloud as noise, colour-only meaning is lost
([The text mode lie](https://www.osnews.com/story/144892/the-text-mode-lie-why-modern-tuis-are-a-nightmare-for-accessibility/),
[guide gist](https://gist.github.com/MangaD/cd8b8ab9b4f119ac5214fa4f3424ccd7)).
Mitigations they name: a plain/streaming mode as an alternative to the TUI,
decoration (colour, Unicode borders, animation) as opt-out, hide the cursor
where it is irrelevant, keep keyboard-only operation. harw already has
non-TUI entry points (`harw run`, one-shot, gateway); the gap is an explicit
documented plain mode and `NO_COLOR` handling inside `harw-tui` (grep found none).

## 7. Beyond the terminal — Rust GUI landscape (for completeness)

Per [the 2026 survey](https://wrenlearnsrust.com/posts/2026-03-11-rust-gui-landscape-2026.html)
and [the Windows guide](https://rust-pc.github.io/rust-windows-gui.html):
egui (immediate, fastest start, "debug-UI look", weak accessibility), iced
(Elm-style, native feel, thinner docs, accessibility incomplete), Slint
(declarative DSL, IME ok, commercial licence for some uses), Dioxus
(React-like over a WebView, accessibility and IME good), Xilem (pre-production),
GPUI (used by Zed, less mature). Relevant to harw only for the mobile/remote
story: the WebSocket control plane (PL-68 local-cloud) already allows a
non-TUI client, and a WebView-based client would inherit browser accessibility,
whereas a second TUI cannot.

## 8. Gap analysis and options

| # | Gap (verified in tree) | Option | Cost / risk |
|---|---|---|---|
| G1 | ratatui 0.29 while 0.30.1 is current | Upgrade after checking `block::Title`, `Flex::SpaceAround`, `Alignment`, `Style`/`Stylize` use; keep own terminal guard | medium, mechanical; MSRV 1.88 |
| G2 | No kitty keyboard flags | Negotiate `DISAMBIGUATE_ESCAPE_CODES`+`REPORT_EVENT_TYPES` when supported, fall back silently | low; must pop flags on every exit path (guard already exists) |
| G3 | Width via `unicode-width` only | Centralise a `cell_width` helper; query mode 2027 once at start; test with ZWJ/CJK/VS16 strings | medium; terminal-dependent, needs fixtures |
| G4 | Snapshot tests absent (7 `TestBackend` files, no `insta`) | Add `insta` snapshots for dialogs, dock, narrow layout (also the portrait plan) | low; text only, no colours |
| G5 | Plain/accessible mode and `NO_COLOR` | Documented flag/env; no borders/animation; linear output | low–medium |
| G6 | Simulated streaming | Real token streaming needs `ModelProvider` support (core change, out of scope here) | blocked on core |
| G7 | Reader thread vs async | Keep: FAQ says async is only worth it if needed; the app bus already decouples | none |
| G8 | Images/animation | `ratatui-image`/`tachyonfx` only if a concrete feature needs them; both need off-thread work | defer |

Not recommended from this research: adopting `tui-realm`/`rat-salsa` (a
rewrite of an app with ~78k lines of TUI code for focus handling that harw
already implements), or a GUI framework for the local client.

## 9. Open questions

1. Do we want inline mode (scrollback-preserving) as an option next to the
   alternate screen? Needed for copy/paste friendliness and accessibility;
   unverified how well `Viewport::Inline` handles harw's panels.
2. Which terminals must be supported for the phone/portrait case (Termux,
   Blink, iSH, SSH clients)? Kitty keyboard and mode 2027 support will be
   absent in many of them — the fallbacks are the real product.
3. IME/wide-input behaviour of `input_editor.rs` is not covered by any source
   read here; needs hands-on testing.

## Sources

Read in full or in part on 2026-10-02: ratatui.rs (application patterns,
component architecture, event handling, rendering, FAQ, snapshot testing,
highlights 0.30/0.30.1), ratatui `BREAKING-CHANGES.md`, docs.rs ratatui and
crossterm, `tui-textarea`, `tui-widgets`, `tachyonfx`, `ratatui-image`,
awesome-ratatui, kitty keyboard protocol spec, Mitchell Hashimoto's grapheme
article, OSNews accessibility article, Rust GUI comparisons (wrenlearnsrust,
rust-pc), Codex architecture write-up. Search-only (titles/snippets, not read
in depth): ratatui discussion #1438, the TUI accessibility gist, tui-realm /
rat-salsa pages. Pages that failed: ratatui inline-viewport page (404),
ratatui-core text module (503).
