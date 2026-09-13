# Codex TUI – Events & App-Loop (Cluster A)

Quellverzeichnis: `/srv/dev-shared/projects/rust/codex/codex-rs/tui/src`

---

## 1. Überblick: Vier Event-Quellen, ein Select-Loop

Codex betreibt die TUI über **vier gleichzeitig überwachte Streams**, die in einem
einzigen `tokio::select!`-Loop vereinigt werden (`app.rs:1168–1227`):

```
┌───────────────────────────────────────────────────────────────┐
│                       App::run (async)                        │
│                                                               │
│  tokio::select! {                                             │
│    app_event_rx.recv()          ← interne Widget→App-Msg     │
│    active_thread_rx.recv()      ← gepufferte Server-Events   │
│    tui_events.next()            ← Terminal-Keys + Redraw     │
│    app_server.next_event()      ← Agent-Server-Notifications │
│  }                                                            │
└───────────────────────────────────────────────────────────────┘
```

Jeder Zweig gibt `AppRunControl::Continue` oder `AppRunControl::Exit(ExitReason)`
zurück (`app.rs:420–423`). Exit beendet die Loop sofort; ansonsten wird der
nächste Zweig angewartet.

---

## 2. TuiEvent – der Terminal-Eingabe-Stream

**Typ:** `pub enum TuiEvent` (`tui.rs:512–525`)

```rust
pub enum TuiEvent {
    Key(crossterm::event::KeyEvent),
    Paste(String),
    Resize,
    Draw,
}
```

`TuiEvent::Draw` ist **kein** Terminal-Event – es kommt vom `FrameRequester`-Akteuer
(siehe Abschnitt 5). `TuiEvent::Resize` signalisiert eine Größenänderung des
Terminals **und** löst ebenfalls einen vollständigen Neuzeichnungs-Pfad aus.

Der Stream wird erzeugt über:

```rust
let tui_events = tui.event_stream();
tokio::pin!(tui_events);
// tui.rs:716–732
```

`Tui::event_stream()` erzeugt einen `TuiEventStream` (`tui/event_stream.rs`), der
intern einen `EventBroker` (crossterm-Wrapper), den `draw_tx`-Broadcast-Kanal
(Frame-Anfragen) und auf Unix den `SuspendContext` (SIGTSTP/SIGCONT) bündelt.

### Übertragung auf harw-tui

harw-tui nutzt `crossterm::event::poll(100ms)` + `event::read()` synchron in einem
blocking-Loop (`app.rs:330–336`). Für dynamisches Neuzeichnen (Animationen,
async-Antworten) müsste dieser Loop auf `tokio::select!` mit einem separaten
`UnboundedReceiver<TuiEvent>` umgestellt werden. `TuiEvent::Draw` würde via
`FrameRequester.schedule_frame()` ausgelöst (s. Abschnitt 5).

---

## 3. AppEvent – der interne Widget-Bus

**Typ:** `pub(crate) enum AppEvent` (`app_event.rs:149–1073`)

AppEvent ist der zentrale **Nachrichtenbus** vom Widget-Layer zum `App`-Layer. Mit
über 100 Varianten deckt er alles ab, was ein Widget der App mitteilen kann – ohne
direkte Referenz auf `App`-Interna zu benötigen.

Wichtige Varianten-Gruppen:

| Gruppe | Beispiele |
|--------|-----------|
| Agent-Ops | `CodexOp(AppCommand)`, `SubmitThreadOp { thread_id, op }` |
| History / Scrollback | `InsertHistoryCell(Box<dyn HistoryCell>)`, `ConsolidateAgentMessage { source, cwd, .. }` |
| Lifecycle | `Exit(ExitMode)`, `NewSession`, `ClearUi` |
| Async-Ergebnisse | `FileSearchResult { query, matches }`, `RateLimitsLoaded { .. }` |
| Animations | `StartCommitAnimation`, `CommitTick`, `StopCommitAnimation` |
| Scrollback-Puffer | `BeginInitialHistoryReplayBuffer`, `EndInitialHistoryReplayBuffer` |

`ExitMode` (`app_event.rs:1090–1098`) trennt zwei Pfade:

```rust
pub(crate) enum ExitMode {
    ShutdownFirst,   // Kern-Shutdown abwarten (normaler Nutzer-Exit)
    Immediate,       // Sofort, ohne Cleanup (Last-Resort-Pfad)
}
```

### Übertragung auf harw-tui

harw-tui hat kein AppEvent-Enum – `classify_line()` (`app.rs:125–150`) gibt direkt
`LineAction` zurück, das im Event-Loop per `match` verarbeitet wird. Das verhindert,
dass Widgets unabhängig Events senden können (z. B. eine Autocomplete-Liste kann
keinen `/command`-Text einsetzen ohne direkten `&mut app`-Zugriff).

Empfehlung: `AppEvent`-Enum einführen, zunächst mit 5–10 Varianten:
`Submit(String)`, `SystemMessage(String)`, `AssistantReply(String)`,
`SlashCommandComplete { prefix, candidates: Vec<String> }`, `Quit`.

---

## 4. AppEventSender – typsicher ohne Arc-Mutex

**Typ:** `pub(crate) struct AppEventSender` (`app_event_sender.rs:23–25`)

```rust
pub(crate) struct AppEventSender {
    pub app_event_tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
}
```

`AppEventSender` ist `Clone + Debug`. Widgets halten eine Kopie; kein `Arc<Mutex<App>>`
nötig. Der Sender loggt jeden Event via `session_log::log_inbound_app_event`
(`app_event_sender.rs:34–43`), außer `CodexOp`-Events (die werden an der
Einreichungsstelle geloggt).

Hilfsmethoden kapseln häufige Muster:

```rust
pub(crate) fn interrupt(&self) {
    self.send(AppEvent::CodexOp(AppCommand::interrupt()));
}
pub(crate) fn compact(&self) { .. }
pub(crate) fn exec_approval(&self, thread_id, id, decision) { .. }
```

Der Channel wird in `App::run` gebaut:

```rust
let (app_event_tx, mut app_event_rx) = unbounded_channel();
let app_event_tx = AppEventSender::new(app_event_tx);
// app.rs:782–783
```

### Übertragung auf harw-tui

Eine `HarwEventSender(UnboundedSender<AppEvent>)` mit `clone()` könnte dem
Composer-Widget (`input.rs` bzw. neuem `composer.rs`) übergeben werden, damit es
`AppEvent::SlashCommandComplete { .. }` schickt, ohne direkten `App`-Zugriff zu
brauchen.

---

## 5. FrameRequester – Redraw-Koaleszierung

**Typ:** `pub struct FrameRequester` (`tui/frame_requester.rs:31–57`)

```rust
pub struct FrameRequester {
    frame_schedule_tx: mpsc::UnboundedSender<Instant>,
}
impl FrameRequester {
    pub fn schedule_frame(&self) {
        let _ = self.frame_schedule_tx.send(Instant::now());
    }
    pub fn schedule_frame_in(&self, dur: Duration) { .. }
}
```

Intern läuft ein `FrameScheduler`-Task (gekoppelt via `mpsc`), der auf dem
`broadcast::Sender<()>` (`draw_tx`) sendet. `TuiEventStream` subscribed auf diesen
Broadcast und liefert `TuiEvent::Draw`.

**Koaleszierung:** Mehrere `schedule_frame()`-Aufrufe → ein einziges `draw_tx.send(())`
(nach `MIN_FRAME_INTERVAL` aus `frame_rate_limiter.rs`, max. 120 FPS).

Der `FrameRequester` wird beim `Tui::new()` angelegt (`tui.rs:577`):

```rust
let (draw_tx, _) = broadcast::channel(1);
let frame_requester = FrameRequester::new(draw_tx.clone());
```

Widgets erhalten ihn über `tui.frame_requester()` und speichern eine Kopie davon in
`ChatWidgetInit::frame_requester`.

Animationen (z. B. `CommitTick`-Events) senden `schedule_frame_in(COMMIT_ANIMATION_TICK)`
nachdem der FrameRequester einen `AppEvent::StartCommitAnimation` empfangen hat.

### Übertragung auf harw-tui

harw-tui zeichnet alle 100 ms neu (Poll-Timeout). Mit `FrameRequester` würde der
Loop nur dann zeichnen, wenn Widgets/Background-Tasks es anfordern. Minimal:
`schedule_frame()` nach jedem `AppEvent`-Empfang + nach async-Antworten. Animations
(Thinking-Spinner) würden `schedule_frame_in(Duration::from_millis(80))` nutzen.

---

## 6. AppCommand – Agent-Server-Ops

**Typ:** `pub(crate) enum AppCommand` (`app_command.rs:26–105`)

```rust
pub(crate) enum AppCommand {
    Interrupt { behavior: InterruptBehavior },
    UserTurn { items: Vec<UserInput>, cwd: PathBuf, model: String, .. },
    ExecApproval { id: String, decision: CommandExecutionApprovalDecision },
    Shutdown,
    ThreadRollback { num_turns: u32 },
    Review { target: ReviewTarget },
    // ... (14 Varianten total)
}
```

`AppCommand` ist der Gegenpol zu `AppEvent`: `AppEvent` fließt Widget→App,
`AppCommand` fließt App→Agent-Server. Der Weg:

```
Widget → AppEvent::CodexOp(AppCommand::user_turn(..))
       → AppEventSender.send(event)
       → app_event_rx
       → App::handle_event() → dispatches to AppServerSession
```

### Übertragung auf harw-tui

`LineAction::Chat(text)` in harw-tui ist das Äquivalent zu `AppCommand::UserTurn`.
Für mehrschrittiges Approval, Interrupt, Rollback braucht harw-tui analoge Varianten.
Einstieg: `HarwCommand::UserTurn(String)`, `HarwCommand::Interrupt`, `HarwCommand::Quit`.

---

## 7. Terminal-Setup und Teardown

**Kein AlternateScreen für den Haupt-Chat!**

`tui::init()` (`tui.rs:376–465`):

```rust
enable_raw_mode()?;
execute!(stdout(), EnableBracketedPaste)?;
keyboard_modes::enable_keyboard_enhancement();
execute!(stdout(), EnableFocusChange);
// Terminal-Probe: Cursor-Position, Standardfarben, Keyboard-Enhancement
```

Das Terminal bleibt im **Inline-Viewport-Modus**: Die Chat-History liegt im normalen
Terminal-Scrollback, der Chat-Input am unteren Rand des sichtbaren Bereichs.
`EnterAlternateScreen` wird **nur** für Overlays (Pager, Resume-Picker) über
`Tui::enter_alt_screen()` / `leave_alt_screen()` verwendet (`tui.rs:736–770`).

`tui::restore()` / `tui::restore_after_exit()` (`tui.rs:282–313`):

```rust
DisableBracketedPaste
DisableFocusChange
disable_raw_mode()
SetCursorStyle::DefaultUserShape
crossterm::cursor::Show
```

Bei `restore_after_exit` wird zusätzlich `terminal_stderr::finish()` aufgerufen, das
stderr-Umleitung beendet.

Panik-Hook (`tui.rs:504–510`):
```rust
panic::set_hook(Box::new(move |panic_info| {
    let _ = restore_after_exit();
    hook(panic_info);
}));
```

### Übertragung auf harw-tui

harw-tui nutzt `EnterAlternateScreen` + `LeaveAlternateScreen` (`app.rs:208, 233`)
für **den gesamten Chat** – das löscht beim Beenden die Chat-History. Codex-Muster:
Inline-Viewport ohne AlternateScreen. Änderung in `TerminalGuard::enter()`:
`EnterAlternateScreen` weglassen, stattdessen nur `enable_raw_mode()` +
`EnableBracketedPaste`. Die Chat-History bleibt dann im Scrollback erhalten.

---

## 8. Resize-Behandlung

Zwei Redraw-Pfade in `Tui::draw()`:

### 8a. Legacy-Pfad: `Tui::draw()` (`tui.rs:880–953`)

```rust
fn pending_viewport_area(&mut self) -> Result<Option<Rect>> {
    // Cursor-Position-Heuristik: wenn Cursor Y sich änderte, Viewport versetzen
}
```

Bei Resize: `pending_viewport_area()` fragt Cursor-Position ab → passt
`viewport_area` an → `clear_for_viewport_change()` löscht ab dem neuen Viewport-Top.

### 8b. Resize-Reflow-Pfad: `Tui::draw_with_resize_reflow()` (`tui.rs:1016–1067`)

```rust
fn update_inline_viewport_for_resize_reflow(terminal, height) -> Result<bool> {
    // Passt viewport_area an ohne scroll_region_up bei shrink
    // Returns needs_full_repaint
}
```

Wenn Viewport sich ändert: `terminal.invalidate_viewport()` → komplettes Neu-Diff
aller Zellen im nächsten Frame. Der Transcript-Reflow-State (`TranscriptReflowState`)
baut den Scrollback aus `transcript_cells` neu auf.

`App::handle_tui_event()` (`app.rs:1271`) erkennt `TuiEvent::Draw | TuiEvent::Resize`
und ruft dann `render_chat_widget_frame(tui)` via `draw_with_resize_reflow`.

### Übertragung auf harw-tui

harw-tui reagiert auf Resize nicht. Einstieg: `TuiEvent::Resize` erkennen →
`terminal.resize()` aufrufen → `terminal.clear()` → erneut zeichnen. Für Inline-
Viewport: `update_inline_viewport_for_resize_reflow`-Logik adaptieren.

---

## 9. insert_history – Terminal-Scrollback als History-Speicher

Das zentrale Design-Prinzip: **abgeschlossene Chat-Cells werden direkt in den
Terminal-Scrollback geschrieben**, nicht als ratatui-Buffer gerendert.

**Einstiegspunkt:** `insert_history_hyperlink_lines_with_mode_and_wrap_policy()`
(`insert_history.rs:104–256`)

```rust
pub(crate) fn insert_history_hyperlink_lines_with_mode_and_wrap_policy<B>(
    terminal: &mut custom_terminal::Terminal<B>,
    lines: Vec<HyperlinkLine>,
    mode: InsertHistoryMode,     // Standard | ZellijRaw
    wrap_policy: HistoryLineWrapPolicy, // PreWrap | Terminal
) -> io::Result<()>
```

**Standard-Pfad** (nicht Zellij):

```
1. Pre-wrap: Zeilen auf Viewport-Breite umbrechen (adaptive_wrap_line)
2. SetScrollRegion(1..area.top())  -- Scroll-Region = Bereich ÜBER dem Viewport
3. MoveTo(0, cursor_top)
4. für jede Zeile: Print("\r\n") + write_history_line(...)
5. ResetScrollRegion
6. MoveTo(last_cursor_pos.x, last_cursor_pos.y)  -- Cursor zurück
```

Der Viewport bleibt dabei unberührt. Scroll-Region-Manipulation
(`\x1b[top;bottomr`) bewirkt, dass `\r\n` in der Zeile über dem Viewport
einfügt, statt durch den Viewport zu scrollen.

```
Vorher:                Nach insert_history_lines(1 Zeile):
┌──────────────┐      ┌──────────────┐
│              │      │ neue Zeile   │  ← in Scrollback geschrieben
│              │      ├──────────────┤
├──────────────┤      │              │
│  Viewport    │      │  Viewport    │
│  (Input)     │      │  (Input)     │
└──────────────┘      └──────────────┘
```

`Tui::pending_history_lines` (`tui.rs:553–556`) puffert Zeilen bis zum nächsten
`draw()`-Aufruf, wo `flush_pending_history_lines()` aufgerufen wird – innerhalb
des `stdout().sync_update(|_| { ... })` um Tearing zu verhindern.

Wrap-Strategien (`insert_history.rs:42–46`):
- `HistoryLineWrapPolicy::PreWrap`: Codex bricht selbst um (Standard), damit
  Terminal-Selection die unwrapped Source kopiert.
- `HistoryLineWrapPolicy::Terminal`: Zeile ungebrochen, Terminal umbreicht.

### Übertragung auf harw-tui

harw-tui hält `Vec<ChatLine>` in Speicher und rendert sie im Paragraph-Widget
(`app.rs:480–485`). Beim Scrollen durch lange Sessions wird das teuer.

Codex-Muster: Abgeschlossene Chat-Cells mit `Tui::insert_history_lines()`
direkt in den Scrollback schreiben. Der ratatui-Viewport zeigt nur den
Eingabebereich. Voraussetzung: Inline-Viewport-Modus (kein AlternateScreen).

Einstieg für harw-tui:
1. `InlineTerminalGuard` ohne `EnterAlternateScreen`
2. Typ `HarwHistoryCell` mit einer `render_to_lines() -> Vec<Line<'static>>`-Methode
3. Nach jedem abgeschlossenen Turn: `tui.insert_history_lines(cell.render_to_lines())`
4. Viewport (`terminal.viewport_area`) auf die letzten N Zeilen begrenzen

---

## 10. CustomTerminal – Inline-Viewport-Verwaltung

Codex nutzt **nicht** `ratatui::Terminal` direkt. `custom_terminal::Terminal<B>`
(`custom_terminal.rs:145–168`) erweitert es um:

```rust
pub struct Terminal<B: Backend + Write> {
    // Standard ratatui-Felder:
    backend: B,
    buffers: [Buffer; 2],
    current: usize,
    hidden_cursor: bool,
    // Codex-Erweiterungen:
    pub viewport_area: Rect,          // nur der sichtbare Input-Bereich
    pub last_known_screen_size: Size, // für Resize-Erkennung
    pub last_known_cursor_pos: Position, // für Viewport-Repositionierung
    visible_history_rows: u16,        // Anzahl geschriebener History-Zeilen
}
```

Wichtige Methoden:
- `set_viewport_area(Rect)` – ändert aktiven Zeichenbereich + passt Buffer-Größe an
- `invalidate_viewport()` – setzt Back-Buffer zurück → zwingt zum vollen Rediff
- `clear_after_position(Position)` – löscht ab pos bis Bildschirmende
- `note_history_rows_inserted(n)` – verfolgt sichtbare History-Zeilen
- `diff_buffers()` – eigene Diff-Implementierung mit `ClearToEnd`-Optimierung

**Typ-Alias:** `pub type Terminal = CustomTerminal<CrosstermBackend<Stdout>>`
(`tui.rs:70`)

### Übertragung auf harw-tui

harw-tui nutzt `ratatui::Terminal<CrosstermBackend<Stdout>>` direkt
(`app.rs:196–197`). Der `TerminalGuard` könnte um ein `viewport_area`-Feld erweitert
werden. Für Inline-Viewport ist `viewport_area` zwingend, da `ratatui::Terminal`
stets das volle Terminal als Buffer behandelt. Alternative: `CustomTerminal<B>` aus
codex direkt als Dependency übernehmen (ist `pub` und liegt in `codex_tui`-Crate).

---

## 11. Suspend / Resume (Unix Job-Control)

`SuspendContext` (`tui/job_control.rs`, von `tui.rs:537` und `draw()`-Pfad
referenziert) implementiert SIGTSTP (Ctrl+Z) / SIGCONT-Handling:

```rust
// Vor jedem synchronized update geprüft:
let mut prepared_resume = self.suspend_context
    .prepare_resume_action(&mut self.alt_saved_viewport);
// tui.rs:889
```

Nach SIGCONT:
- `reapply_raw_mode_after_resume()` (`tui.rs:293–296`): disables dann re-enables
  raw mode, um Kernel-/crossterm-State zu synchronisieren
- Terminal-Modus-Setup wird erneut angewendet
- `flush_terminal_input_buffer()` leert stdin (ttyflush)

`Tui::with_restored()` (`tui.rs:648–686`) ist der generelle Escape-Pfad für
externe Programme (Editor, Shell), der:
1. Events pausiert (`pause_events()`)
2. Alt-Screen verlässt
3. Terminal-Modes restauriert
4. Externes Programm ausführt
5. Terminal-Modes reaktiviert
6. Events resumiert

### Übertragung auf harw-tui

harw-tui hat kein Suspend-Handling. Minimales Äquivalent:
`unsafe { libc::kill(libc::getpid(), libc::SIGSTOP) }` nach Terminal-Restore,
dann auf SIGCONT warten und `enable_raw_mode()` + `flush_terminal_input_buffer()`
wiederholen. Für `/edit`-Kommando: `Tui::with_restored()`-Muster.

---

## 12. Zusammenfassung: Was macht codex-TUI „dynamisch"

| Aspekt | codex-TUI | harw-tui (aktuell) |
|--------|-----------|---------------------|
| Event-Loop | `tokio::select!` über 4 Streams | `event::poll(100ms)` blocking |
| Redraw-Trigger | `FrameRequester` (Koaleszierung, 120 FPS) | 100 ms Timeout immer |
| Terminal-Modus | Inline-Viewport (kein Alt-Screen) | AlternateScreen |
| Chat-History | Terminal-Scrollback direkt | `Vec<ChatLine>` im Buffer |
| Widget→App | `AppEvent` über `UnboundedSender` | direkter `&mut app`-Zugriff |
| Async-Ergebnisse | `AppEvent::FileSearchResult` etc. | nicht vorhanden |
| Animationen | `CommitTick`-Events via FrameRequester | nicht vorhanden |
| /commands | `slash_command.rs` + AppEvent-Bus | nur `/quit`/`/exit` hardcoded |
| Resize | `TuiEvent::Resize` + Reflow | nicht behandelt |
| Suspend | `SuspendContext` + job_control | nicht behandelt |

**Die drei wichtigsten Übertragungen für harw-tui (Priorität):**

1. **`AppEvent`-Enum + `AppEventSender`**: Entkopplung Widget↔App ermöglicht
   asynchrone Ergebnisse und spätere /command-Autocomplete ohne Architektur-Umbau.

2. **Inline-Viewport ohne AlternateScreen**: Chat-History bleibt nach Beenden im
   Terminal sichtbar; `insert_history_lines()` schreibt finalisierte Turns in den
   Scrollback. Erfordert `TerminalGuard`-Umbau.

3. **`FrameRequester` + `TuiEvent::Draw`**: Statt 100ms-Busy-Poll ein Akteuer-Modell,
   das nur bei tatsächlichem Redraw-Bedarf zeichnet (async-Antworten, Animationen,
   Autocomplete-Updates).
