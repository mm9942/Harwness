# harw-tui Redesign-Spec

> Synthese aus den Codex-TUI-Studien (Cluster A, C, D) und dem Ist-Zustand von
> `harw-tui/src/` sowie `harw-cli/src/onboarding.rs`.

---

## 1. Ist-Analyse harw-tui: Konkrete Defizite

### 1.1 Terminal-Modus: `EnterAlternateScreen` löscht Chat-History

| Datei | Zeile | Problem |
|-------|-------|---------|
| `harw-tui/src/app.rs` | 208 | `crossterm::execute!(stdout, EnterAlternateScreen)` |
| `harw-tui/src/app.rs` | 233 | `crossterm::execute!(io::stdout(), LeaveAlternateScreen)` (Fehler-Pfad) |
| `harw-tui/src/app.rs` | 233 (Drop) | `crossterm::execute!(self.terminal.backend_mut(), LeaveAlternateScreen)` |
| `harw-tui/src/setup.rs` | 523 | `crossterm::execute!(stdout, EnterAlternateScreen)` |
| `harw-tui/src/setup.rs` | 547 (Drop) | `crossterm::execute!(self.terminal.backend_mut(), LeaveAlternateScreen)` |

**Folge:** Beim Beenden der TUI wird der gesamte Viewport gelöscht. Der Nutzer sieht
sein Chat-Protokoll nicht mehr im Terminal-Scrollback (codex hingegen schreibt
abgeschlossene Turns direkt in den Scrollback, da es keinen AlternateScreen verwendet).

### 1.2 Blockierender Poll-Loop ohne `tokio::select!`

| Datei | Zeile | Problem |
|-------|-------|---------|
| `harw-tui/src/app.rs` | 330–331 | `event::poll(Duration::from_millis(100))` + synchrones `event::read()` |
| `harw-tui/src/setup.rs` | 588–591 | identisches Muster |

**Folge:** Es gibt keine Möglichkeit, asynchrone LLM-Streaming-Deltas pro Frame einzulesen
(der Turn blockiert den gesamten Loop in `drive_turn` via `block_on`, `app.rs:427`).
Dynamische Ausgaben (Lade-Spinner, Streaming-Text) sind strukturell ausgeschlossen.

### 1.3 Kein `AppEvent`-Bus: Widgets haben keinen eigenständigen Kommunikationsweg

| Datei | Zeile | Problem |
|-------|-------|---------|
| `harw-tui/src/app.rs` | 125–150 | `classify_line()` gibt `LineAction` zurück; direkter `&mut app`-Zugriff |
| `harw-tui/src/app.rs` | 398–408 | `match classify_line(...)` inline im Event-Loop |

**Folge:** Ein zukünftiges `/command`-Popup kann keinen Text in den Eingabepuffer
injizieren, ohne direkten `&mut ChatApp`-Zugriff zu haben. Die Architektur skaliert
nicht auf mehrere Widgets.

### 1.4 Kein `EnableBracketedPaste` → Paste-Events kommen nie an

| Datei | Zeile | Problem |
|-------|-------|---------|
| `harw-tui/src/app.rs` | 205–219 | `TerminalGuard::enter()` ohne `EnableBracketedPaste` |
| `harw-tui/src/setup.rs` | 519–540 | `TerminalGuard::enter()` ohne `EnableBracketedPaste` |
| `harw-tui/src/app.rs` | 334 | `Event::Paste` wird nie abgefangen, wird ignoriert |
| `harw-tui/src/setup.rs` | 591 | `Event::Paste` wird nie abgefangen, wird ignoriert |

**Folge (der primäre Nutzer-Blocker):** Das Einfügen eines API-Keys per Ctrl+V / Paste
funktioniert nicht. Bracketed-Paste-Events werden vom Terminal nicht gesendet, weil
`crossterm::event::EnableBracketedPaste` nie per `execute!()` aktiviert wurde.
Selbst wenn das Terminal Paste-Events senden würde, gäbe es im Setup-Loop keinen
`Event::Paste`-Zweig.

### 1.5 OAuth-Auswahl: Kein Sofort-Routing, kein Eingabefeld

| Datei | Zeile | Problem |
|-------|-------|---------|
| `harw-tui/src/setup.rs` | 292–309 | `on_key_auth()`: kein direkter State-Übergang bei Zahltaste `'1'`/`'2'` |
| `harw-tui/src/setup.rs` | 369–384 | `confirm_auth()`: OAuth-Option setzt `secret_ref` ohne Wizard-Schritt |
| `harw-tui/src/setup.rs` | 292 | wenn `AuthOption::ApiKey` erst per Up/Down angewählt werden muss, startet kein Direktübergang |

**Folge:** Nach der Wahl von `AuthOption::ApiKey` erfolgt kein sofortiger Sprung in
einen dedizierten Eingabeschritt. Das `/command`-artige „Zahl drücken → sofort weiter"-
Muster aus codex fehlt. Der OAuth-Pfad liefert keine Browser-URL, kein Geräte-Code-Feld
und kein Paste-Feld für den resultierenden Token.

### 1.6 Keine `/command`-Autocomplete, kein Popup-Widget

| Datei | Zeile | Problem |
|-------|-------|---------|
| `harw-tui/src/app.rs` | 132–134 | Nur `/quit` + `/exit` hardcoded |
| `harw-tui/src/app.rs` | 144 | Alle anderen Kommandos → `LineAction::System("Command noch nicht unterstützt: …")` |
| `harw-tui/src/lib.rs` | 12–28 | `CommandRegistry`, `CapabilitySet`, `CommandSpec` exportiert, aber nie an die TUI angebunden |

**Folge:** Der komplette `CommandRegistry`-Mechanismus aus `registry.rs` ist vorhanden
aber tot. Ein Popup wie in codex (ListSelectionView, live Fuzzy-Filter) fehlt vollständig.

### 1.7 Keine Resize-Behandlung

| Datei | Zeile | Problem |
|-------|-------|---------|
| `harw-tui/src/app.rs` | 334 | `Event::Resize(_, _)` wird nicht abgefangen |

**Folge:** Wenn das Terminal während eines Chats vergrößert oder verkleinert wird,
passt sich der Viewport nicht an; Artefakte bleiben stehen.

### 1.8 Synchroner Turn-Antrieb blockiert Event-Loop

| Datei | Zeile | Problem |
|-------|-------|---------|
| `harw-tui/src/app.rs` | 403–405 | `drive_turn(runtime, …)` → `runtime.block_on(run_turn(…))` |

**Folge:** Während das Modell antwortet, ist die TUI eingefroren. Kein Spinner,
kein Abbruch per `Ctrl+C`, kein Resize-Handling.

---

## 2. Ziel-Architektur

### 2.1 Überblick: Event-Loop nach Codex-Vorbild

```
┌───────────────────────────────────────────────────────────────┐
│                   HarwApp::run  (async)                       │
│                                                               │
│  tokio::select! {                                             │
│    app_event_rx.recv()      ← HarwEvent-Bus (Widget→App)     │
│    model_stream_rx.recv()   ← gepufferte LLM-Deltas           │
│    tui_events.next()        ← Terminal-Keys + Paste + Redraw  │
│  }                                                            │
└───────────────────────────────────────────────────────────────┘
```

Jeder Zweig gibt `LoopControl::Continue` oder `LoopControl::Exit` zurück.
`TuiEvent::Draw` kommt vom `FrameRequester`; jeder State-Change ruft
`frame_requester.schedule_frame()` auf.

### 2.2 `HarwEvent`-Enum (neu: `harw-tui/src/events.rs`)

```rust
pub(crate) enum HarwEvent {
    Submit(String),
    SystemMessage(String),
    AssistantDelta(String),
    AssistantComplete,
    SlashCommandComplete { prefix: String, candidates: Vec<String> },
    Quit,
}
```

`HarwEventSender(tokio::sync::mpsc::UnboundedSender<HarwEvent>)` ist `Clone`.
Widgets erhalten eine Kopie; kein `&mut HarwApp`-Zugriff nötig.

### 2.3 `TuiEvent`-Enum (neu: `harw-tui/src/tui_event.rs`)

```rust
pub(crate) enum TuiEvent {
    Key(crossterm::event::KeyEvent),
    Paste(String),
    Resize(u16, u16),
    Draw,
}
```

`TuiEvent::Draw` wird vom `FrameRequester` über einen `mpsc::UnboundedSender<()>`
ausgelöst; der Event-Stream-Task empfängt beide Quellen via `tokio::select!` und
sendet auf den `tui_event_tx`-Kanal.

### 2.4 Terminal-Modus: Inline-Viewport ohne `EnterAlternateScreen`

`InlineTerminalGuard` (ersetzt `TerminalGuard` in `app.rs` und `setup.rs`):

```rust
fn enter() -> Result<Self, TuiError> {
    enable_raw_mode()?;
    crossterm::execute!(io::stdout(), EnableBracketedPaste)?;
    // KEIN EnterAlternateScreen
    let backend = CrosstermBackend::new(io::stdout());
    Terminal::new(backend).map(|terminal| Self { terminal })
}

impl Drop for InlineTerminalGuard {
    fn drop(&mut self) {
        let _ = crossterm::execute!(io::stdout(), DisableBracketedPaste);
        let _ = disable_raw_mode();
        let _ = self.terminal.show_cursor();
    }
}
```

Chat-History wird nach jedem abgeschlossenen Turn **vor** dem Viewport in den
Terminal-Scrollback geschrieben (via `\r\n`-Druck hinter Scroll-Region-Manipulation;
vereinfachte Variante des `insert_history_lines`-Musters aus codex).

### 2.5 `FrameRequester` (neu: `harw-tui/src/frame_requester.rs`)

```rust
pub(crate) struct FrameRequester {
    tx: tokio::sync::mpsc::UnboundedSender<()>,
}
impl FrameRequester {
    pub(crate) fn schedule_frame(&self) { let _ = self.tx.send(()); }
}
```

Ein `FrameScheduler`-Task empfängt auf `rx`, drosselt auf `MIN_FRAME_INTERVAL`
(8 ms ≈ 120 FPS) und sendet `TuiEvent::Draw`. Animationen (Lade-Spinner) senden
`schedule_frame()` in regelmäßigen Abständen.

### 2.6 `OnboardingWizard`-Zustandsmaschine (neu: `harw-tui/src/onboarding/`)

Analog zu codex `onboarding/onboarding_screen.rs`:

```rust
pub(crate) enum WizardStep {
    Provider(ProviderPickerWidget),
    Auth(AuthWidget),     // gebrochener Flow → Fix in Slice 1
    Model(ModelPickerWidget),
}

pub(crate) enum StepState { Hidden, InProgress, Complete }

pub(crate) trait StepStateProvider {
    fn step_state(&self) -> StepState;
}

pub(crate) struct OnboardingWizard {
    steps: Vec<WizardStep>,
}

impl OnboardingWizard {
    fn current_steps_mut(&mut self) -> Vec<&mut WizardStep> {
        // Skip Hidden, sammle Complete, stop bei InProgress
    }
}
```

### 2.7 `AuthWidget`-Zustandsmaschine (neu: `harw-tui/src/onboarding/auth.rs`)

Analog zu codex `auth.rs:78–87`:

```rust
pub(crate) enum AuthState {
    PickMode,
    ApiKeyEntry { value: String, prepopulated: bool },
    OAuthPending { url: Option<String> },
    OAuthComplete,
    Configured,
}
```

Zustandsübergänge:
- `PickMode` + `KeyCode::Char('1')` oder `Enter` auf `ApiKey`-Option → sofort `ApiKeyEntry`
- `PickMode` + `KeyCode::Char('2')` oder `Enter` auf `OAuth`-Option → sofort `OAuthPending`
- `ApiKeyEntry` + `TuiEvent::Paste(text)` → `value = text.trim().to_owned()`, `schedule_frame()`
- `ApiKeyEntry` + `Enter` (value nicht leer) → `Configured` → `StepState::Complete`
- `OAuthPending` → Browser-URL in Feld zeigen, Paste-Feld für Token anzeigen

Quit-Schutz analog zu codex `suppress_quit_while_typing_api_key`:
```rust
fn suppress_quit(state: &AuthState, key: KeyEvent) -> bool {
    matches!(state, AuthState::ApiKeyEntry { value, .. } if !value.is_empty())
        && matches!(key.code, KeyCode::Char(_))
        && !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}
```

### 2.8 `/command`-Popup + Autocomplete (neu: `harw-tui/src/command_popup.rs`)

Analogon zu codex `bottom_pane/list_selection_view.rs`:

```rust
pub(crate) struct CommandPopup {
    items: Vec<CommandItem>,      // aus CommandRegistry
    selected_idx: usize,
    search_query: String,
    filtered_indices: Vec<usize>,
    is_visible: bool,
}

pub(crate) struct CommandItem {
    name: String,           // z.B. "quit", "status", "help"
    description: String,
    shortcut: Option<char>, // '1', '2', ... für Sofort-Auswahl
}
```

Trigger: Nutzer tippt `/` → Popup öffnet sich, zeigt alle Commands. Weiteres Tippen
filtert live. Ziffertaste → sofortige Auswahl (wie codex `list_selection_view.rs:1022–1042`).

### 2.9 Streaming-Ausgabe (schrittweise, `harw-tui/src/streaming.rs`)

Analog zu codex `MarkdownStreamCollector`:

```rust
pub(crate) struct StreamCollector {
    buffer: String,
    committed_len: usize,
}
impl StreamCollector {
    pub(crate) fn push_delta(&mut self, delta: &str) { self.buffer.push_str(delta); }
    pub(crate) fn commit_complete_lines(&mut self) -> Option<String> {
        // Gibt Text bis zum letzten \n zurück; None wenn kein \n
    }
    pub(crate) fn finalize(&mut self) -> String { /* spült Rest */ }
}
```

LLM-Deltas kommen via `model_stream_rx`; `commit_complete_lines()` liefert Zeilen die
in `Vec<HarwHistoryCell>` eingereiht werden.

### 2.10 Rendering / Style

- Finalisierte Messages als `{ source: String }` speichern, nicht als `Vec<Line>` —
  Re-Render bei Resize ist so korrekt.
- Adaptive Styles: Terminal-Background-Probe einmal beim Start; `is_light()` → Light/Dark-Theme.
- Footer: `FooterMode`-Enum (`Idle | Streaming | AwaitingInput | Error`), progressiv
  degenerierend bei schmalen Terminals.
- Animationen: `MotionMode` (`Animated | Reduced`) über CLI-Flag `--motion`; Lade-Spinner
  via `schedule_frame_in(Duration::from_millis(80))`.

---

## 3. Priorisierter Implementierungsplan

### SLICE 1 — Onboarding-OAuth-Fix (KRITISCH, Nutzer blockiert)

**Ziel:** API-Key per Paste einfügbar; Auswahl löst sofortigen State-Übergang aus.

**Betroffene Dateien:**
- `harw-tui/src/setup.rs` (Hauptänderung)
- `harw-tui/src/app.rs` (EnableBracketedPaste ebenfalls hinzufügen)

**Konkrete Änderungen:**

1. `TerminalGuard::enter()` in `setup.rs:519–540` und `app.rs:205–219`:
   - `crossterm::execute!(stdout, EnableBracketedPaste)` vor `Terminal::new(backend)` einfügen
   - `DisableBracketedPaste` im `Drop`-Impl einfügen

2. `setup_loop()` in `setup.rs:581–605`:
   - `Event::Paste`-Zweig hinzufügen:
     ```rust
     Event::Paste(text) => {
         app.on_paste(text);
         if app.outcome().is_some() {
             return Ok(app.outcome().cloned());
         }
     }
     ```

3. `SetupApp::on_paste()` (neu in `setup.rs`):
   ```rust
   pub fn on_paste(&mut self, pasted: String) {
       if self.stage == SetupStage::Auth
           && matches!(self.auth_options.get(self.selected), Some(AuthOption::ApiKey))
       {
           self.api_key = pasted.trim().to_owned();
       }
   }
   ```

4. `SetupApp::on_key_auth()` in `setup.rs:292–309`:
   - Sofort-Routing per Zifferntaste hinzufügen:
     ```rust
     KeyCode::Char(c @ '1'..='9') => {
         let idx = (c as usize) - ('1' as usize);
         if idx < self.auth_options.len() {
             self.selected = idx;
             // Sofort in ApiKeyEntry-Eingabe-Schritt
             if matches!(self.auth_options[idx], AuthOption::ApiKey) {
                 // selected = idx, Eingabe kann sofort beginnen
             } else {
                 self.confirm_auth();
             }
         }
     }
     ```

5. `body_lines()` in `setup.rs:633–665`: API-Key-Feld auch bei noch leerer `api_key`
   anzeigen, wenn `AuthOption::ApiKey` selektiert ist (bisher nur `if key != ""`):
   ```rust
   if matches!(app.auth_options.get(app.selected), Some(AuthOption::ApiKey)) {
       lines.push(Line::from(Span::styled(
           format!("Key: {}▌", mask(&app.api_key)),
           Style::default().fg(Color::Cyan),
       )));
       lines.push(Line::from(Span::styled(
           "  (tippen oder Ctrl+V zum Einfügen, Enter zum Bestätigen)",
           Style::default().fg(Color::DarkGray),
       )));
   }
   ```

6. Quit-Schutz in `setup_loop()`: wenn `stage == Auth` und `api_key` nicht leer
   und `key.code == KeyCode::Char(c)` ohne Modifier → `app.on_key(key)` statt
   `return Ok(None)` wenn `Esc`.

**Akzeptanzkriterium:**
- `cargo test -p harw-tui` grün nach den Änderungen (bestehende Tests plus neue
  `test_paste_fills_api_key`-Tests)
- Manuell: Im Onboarding-TUI `1` drücken → sofort Eingabefeld sichtbar →
  Ctrl+V / Bracketed-Paste → Key erscheint maskiert → Enter → nächste Phase

---

### SLICE 2 — `InlineTerminalGuard` + `EnableBracketedPaste` in Chat-TUI

**Ziel:** Chat-History bleibt nach Beenden im Terminal sichtbar; Paste im Chat funktioniert.

**Betroffene Dateien:**
- `harw-tui/src/app.rs`

**Konkrete Änderungen:**

1. `TerminalGuard::enter()` in `app.rs:205–219`:
   - `EnterAlternateScreen` entfernen
   - `EnableBracketedPaste` hinzufügen
   - `Drop`-Impl: `LeaveAlternateScreen` entfernen, `DisableBracketedPaste` hinzufügen

2. `event_loop()` in `app.rs:310–411`:
   - `Event::Paste(text)`-Zweig hinzufügen:
     ```rust
     Event::Paste(text) => {
         app.input.push_str(text.trim_end_matches('\n'));
     }
     ```
   - `Event::Resize(w, h)` abfangen:
     ```rust
     Event::Resize(_, _) => {
         guard.terminal().resize(...)?;
         // Frame neu zeichnen
     }
     ```

**Akzeptanzkriterium:**
- Chat-History ist nach `Ctrl+C` im Terminal-Scrollback sichtbar
- `Ctrl+V` fügt Text in Eingabefeld ein

---

### SLICE 3 — `HarwEvent`-Bus + async Event-Loop (Grundlage für Streaming)

**Ziel:** `tokio::select!`-Loop statt synchronem `event::poll()`; LLM-Antwort
kann streamen, ohne den Loop zu blockieren.

**Betroffene Dateien:**
- `harw-tui/src/app.rs` (Event-Loop-Umbau)
- `harw-tui/src/events.rs` (neu)
- `harw-tui/src/tui_event.rs` (neu)

**Konkrete Änderungen:**

1. `events.rs` (neu):
   ```rust
   pub(crate) enum HarwEvent {
       Submit(String),
       SystemMessage(String),
       AssistantDelta(String),
       AssistantComplete,
       SlashCommandComplete { prefix: String, candidates: Vec<String> },
       Quit,
   }

   pub(crate) struct HarwEventSender(
       tokio::sync::mpsc::UnboundedSender<HarwEvent>
   );
   impl HarwEventSender {
       pub(crate) fn send(&self, event: HarwEvent) { let _ = self.0.send(event); }
   }
   impl Clone for HarwEventSender { /* delegate */ }
   ```

2. `tui_event.rs` (neu):
   ```rust
   pub(crate) enum TuiEvent {
       Key(crossterm::event::KeyEvent),
       Paste(String),
       Resize(u16, u16),
       Draw,
   }
   ```

3. `app.rs`: `event_loop()` auf `async fn` umbaunen, `tokio::select!` verwenden:
   ```rust
   async fn event_loop(
       guard: &mut InlineTerminalGuard,
       app: &mut ChatApp,
       tui_event_rx: &mut mpsc::UnboundedReceiver<TuiEvent>,
       harw_event_rx: &mut mpsc::UnboundedReceiver<HarwEvent>,
       model_stream_rx: &mut mpsc::UnboundedReceiver<AssistantDelta>,
   ) -> Result<(), TuiError> {
       loop {
           tokio::select! {
               Some(event) = tui_event_rx.recv() => { /* Key/Paste/Resize/Draw */ }
               Some(event) = harw_event_rx.recv() => { /* Submit/System/Quit */ }
               Some(delta) = model_stream_rx.recv() => { /* streaming */ }
           }
       }
   }
   ```

4. Turn-Ausführung: `drive_turn` als `tokio::task::spawn` statt `block_on`;
   Deltas via `model_stream_tx.send(delta)`.

**Akzeptanzkriterium:**
- LLM-Antwort erscheint schrittweise (Zeile für Zeile) während Modell antwortet
- Ctrl+C funktioniert auch während Turn läuft

---

### SLICE 4 — `FrameRequester` + Lade-Spinner

**Ziel:** Keine unnötigen Redraws; Spinner während LLM-Streaming.

**Betroffene Dateien:**
- `harw-tui/src/frame_requester.rs` (neu)
- `harw-tui/src/app.rs`

**Konkrete Typen/Funktionen:**

```rust
// frame_requester.rs (neu)
pub(crate) struct FrameRequester {
    tx: tokio::sync::mpsc::UnboundedSender<()>,
}
impl FrameRequester {
    pub(crate) fn schedule_frame(&self) { let _ = self.tx.send(()); }
    pub(crate) fn schedule_frame_in(&self, dur: Duration) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(dur).await;
            let _ = tx.send(());
        });
    }
}
```

Ein Scheduler-Task koalesziert Anfragen auf `MIN_FRAME_INTERVAL = 8 ms` und
sendet `TuiEvent::Draw`.

Spinner-Logic in `app.rs`:
```rust
enum StreamingState { Idle, Streaming { started: Instant } }
// Im Draw-Pfad:
if let StreamingState::Streaming { started } = app.streaming_state {
    let elapsed = started.elapsed().as_millis() as usize;
    let dots = ".".repeat((elapsed / 300) % 4);
    lines.push(Line::from(Span::styled(
        format!("  asst {dots}"),
        Style::default().fg(Color::DarkGray),
    )));
    frame_requester.schedule_frame_in(Duration::from_millis(80));
}
```

**Akzeptanzkriterium:**
- CPU-Verbrauch im Idle-Zustand nahe 0% (kein 100ms-Busy-Poll)
- Spinner erscheint während LLM-Antwort und verschwindet danach

---

### SLICE 5 — `/command`-Popup + Autocomplete

**Ziel:** Tippen von `/` öffnet filterbares Popup mit allen registrierten Commands.

**Betroffene Dateien:**
- `harw-tui/src/command_popup.rs` (neu)
- `harw-tui/src/app.rs`

**Konkrete Typen/Funktionen:**

```rust
// command_popup.rs (neu)
pub(crate) struct CommandPopup {
    items: Vec<CommandItem>,
    selected_idx: usize,
    query: String,
    filtered: Vec<usize>,
}

pub(crate) struct CommandItem {
    pub name: String,
    pub description: String,
}

impl CommandPopup {
    pub(crate) fn new(registry: &CommandRegistry) -> Self { /* befülle items */ }
    pub(crate) fn on_key(&mut self, key: KeyEvent) -> Option<String> { /* None = offen */ }
    pub(crate) fn on_query_change(&mut self, query: &str) { /* filter */ }
    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer) { /* Popup zeichnen */ }
}
```

Aktivierungslogik in `event_loop()`:
- `Char('/')` → `app.command_popup = Some(CommandPopup::new(&registry))`
- Weitere Chars → `popup.on_query_change(&app.input)`
- `Esc` → Popup schließen
- `Enter` / Zifferntaste → ausgewähltes Command einsetzen, Popup schließen

**Akzeptanzkriterium:**
- `/` öffnet Popup mit mindestens `quit`, `exit`, `help`
- Weiteres Tippen filtert live
- Enter setzt Command ein, Popup verschwindet

---

### SLICE 6 — `OnboardingWizard`-Neustrukturierung nach Codex-Vorbild

**Ziel:** `SetupApp` durch `OnboardingWizard` mit `StepStateProvider`-Trait ersetzen;
OAuth erhält eigenen Schritt mit Token-Eingabefeld.

**Betroffene Dateien:**
- `harw-tui/src/onboarding/mod.rs` (neu)
- `harw-tui/src/onboarding/auth.rs` (neu, aus `setup.rs` Auth-Logik extrahiert)
- `harw-tui/src/onboarding/provider_picker.rs` (neu)
- `harw-tui/src/onboarding/model_picker.rs` (neu)
- `harw-tui/src/setup.rs` (bleibt als dünner Wrapper für Rückwärtskompatibilität)

**Kerntypen:**

```rust
// onboarding/mod.rs
pub(crate) enum WizardStep {
    Provider(ProviderPickerWidget),
    Auth(AuthWidget),
    Model(ModelPickerWidget),
}

pub(crate) enum StepState { Hidden, InProgress, Complete }
pub(crate) trait StepStateProvider { fn step_state(&self) -> StepState; }

pub(crate) struct OnboardingWizard {
    steps: Vec<WizardStep>,
    done: bool,
}
impl OnboardingWizard {
    fn current_steps_mut(&mut self) -> impl Iterator<Item = &mut WizardStep>;
    pub(crate) fn handle_key(&mut self, key: KeyEvent);
    pub(crate) fn handle_paste(&mut self, pasted: String);
    pub(crate) fn is_done(&self) -> bool;
    pub(crate) fn outcome(&self) -> Option<SetupOutcome>;
}
```

```rust
// onboarding/auth.rs
pub(crate) enum AuthState {
    PickMode { highlighted: usize },
    ApiKeyEntry { value: String, prepopulated: bool },
    OAuthPending { url: Option<String>, token: String },
    Configured { secret_ref: String },
}
pub(crate) struct AuthWidget {
    state: AuthState,
    options: Vec<AuthOption>,
}
impl StepStateProvider for AuthWidget {
    fn step_state(&self) -> StepState {
        match self.state {
            AuthState::Configured { .. } => StepState::Complete,
            _ => StepState::InProgress,
        }
    }
}
```

**Akzeptanzkriterium:**
- Provider-Auswahl → Auth-Schritt erscheint automatisch
- `1` im Auth-Schritt → sofort `ApiKeyEntry`-Feld
- Paste im `ApiKeyEntry`-State → Key wird direkt eingesetzt
- Auth-Schritt `Complete` → Model-Schritt erscheint automatisch

---

### SLICE 7 — Dynamisches Rendering: `HistoryCell`-Typsystem + Re-Render bei Resize

**Ziel:** Chat-History als `Vec<Box<dyn HistoryCell>>` statt `Vec<ChatLine>`;
Markdown-Rendering; Resize triggert korrektes Re-Render.

**Betroffene Dateien:**
- `harw-tui/src/history_cell.rs` (neu)
- `harw-tui/src/app.rs`

**Kerntypen:**

```rust
// history_cell.rs (neu)
pub(crate) trait HistoryCell: Send + Sync {
    fn display_lines(&self, width: u16) -> Vec<ratatui::text::Line<'static>>;
    fn desired_height(&self, width: u16) -> u16;
}

pub(crate) struct PlainHistoryCell { pub lines: Vec<ratatui::text::Line<'static>> }
pub(crate) struct UserHistoryCell { pub text: String }
pub(crate) struct AssistantHistoryCell { pub source: String }

impl HistoryCell for AssistantHistoryCell {
    fn display_lines(&self, width: u16) -> Vec<ratatui::text::Line<'static>> {
        // Markdown-Rendering mit width-Parameter → korrekt bei Resize
        render_markdown(&self.source, width as usize)
    }
}
```

`ChatApp.lines: Vec<ChatLine>` → `ChatApp.history: Vec<Box<dyn HistoryCell>>`.

**Akzeptanzkriterium:**
- `cargo test -p harw-tui` grün
- Nach Terminal-Resize werden Tabellen/Markdown korrekt neu gebrochen

---

### SLICE 8 — Adaptive Style-Palette

**Ziel:** Keine hartcodierten RGB-Werte; Hell/Dunkel-Erkennung über Terminal-Background.

**Betroffene Dateien:**
- `harw-tui/src/style.rs` (neu)
- `harw-tui/src/app.rs`, `harw-tui/src/setup.rs`

**Kernfunktionen:**

```rust
// style.rs (neu)
pub(crate) fn detect_theme() -> Theme { /* Terminal-Background-Probe */ }
pub(crate) fn accent_color(theme: Theme) -> ratatui::style::Color { /* Cyan/teal */ }
pub(crate) fn selected_style(theme: Theme) -> ratatui::style::Style { /* bold + accent */ }
pub(crate) fn dim_style() -> ratatui::style::Style { /* DarkGray */ }
```

Alle harten `Color::Cyan`-Referenzen in `app.rs:68` und `setup.rs:671` durch
`style::accent_color(theme)` ersetzen.

**Akzeptanzkriterium:**
- TUI ist auf hellen Terminals (Solarized Light etc.) lesbar

---

## 4. Mapping-Tabelle: codex-Konzept → harw-tui-Zieldatei

| codex-Konzept | codex-Quelldatei | harw-tui-Zieldatei |
|---|---|---|
| `TuiEvent` (Key/Paste/Resize/Draw) | `tui.rs:512–525` | `harw-tui/src/tui_event.rs` (neu) |
| `AppEvent` / `AppEventSender` | `app_event.rs`, `app_event_sender.rs` | `harw-tui/src/events.rs` (neu) |
| `FrameRequester` / `FrameScheduler` | `tui/frame_requester.rs` | `harw-tui/src/frame_requester.rs` (neu) |
| `tokio::select!`-Loop | `app.rs:1168–1227` | `harw-tui/src/app.rs:event_loop()` umbau |
| `enable_raw_mode` + `EnableBracketedPaste` (kein Alt-Screen) | `tui.rs:376–465` | `harw-tui/src/app.rs:InlineTerminalGuard` |
| `insert_history_lines` (Scrollback-Write) | `insert_history.rs:104–256` | `harw-tui/src/app.rs` (vereinfacht) |
| `OnboardingScreen` + `Step`-Enum | `onboarding/onboarding_screen.rs` | `harw-tui/src/onboarding/mod.rs` (neu) |
| `AuthModeWidget` + `SignInState` | `onboarding/auth.rs` | `harw-tui/src/onboarding/auth.rs` (neu) |
| `handle_api_key_entry_paste` | `auth.rs:746–768` | `harw-tui/src/setup.rs:on_paste()` (Slice 1) |
| `suppress_quit_while_typing_api_key` | `onboarding_screen.rs:343–353` | `harw-tui/src/setup.rs:setup_loop()` (Slice 1) |
| `ListSelectionView` + `SelectionItem` | `bottom_pane/list_selection_view.rs` | `harw-tui/src/command_popup.rs` (neu) |
| `StepStateProvider`-Trait | `onboarding_screen.rs:66–73` | `harw-tui/src/onboarding/mod.rs` (neu) |
| `MarkdownStreamCollector` | `markdown_stream.rs` | `harw-tui/src/streaming.rs` (neu) |
| `HistoryCell`-Trait | `history_cell/mod.rs` | `harw-tui/src/history_cell.rs` (neu) |
| `AgentMarkdownCell { source, cwd }` | `history_cell/messages.rs` | `harw-tui/src/history_cell.rs:AssistantHistoryCell` |
| `FooterMode` | `bottom_pane/footer.rs` | `harw-tui/src/app.rs:FooterMode` (inline) |
| `shimmer_spans` + `MotionMode` | `shimmer.rs`, `motion.rs` | `harw-tui/src/app.rs:StreamingState` (vereinfacht) |
| `color::is_light` + `blend` | `color.rs` | `harw-tui/src/style.rs` (neu) |
| `AppCommand::UserTurn` | `app_command.rs:26–105` | `harw-tui/src/events.rs:HarwEvent::Submit` |

---

## 5. Constraints und Nicht-Ziele

**Constraints (unverhandelbar):**
- Edition 2024, `#![forbid(unsafe_code)]`
- Handgeschriebene Error-Enums; kein `anyhow`, kein `thiserror`
- Keine Versionen in `Cargo.toml` hardcoden; `cargo add <crate>` verwenden
- Deutsche Doc-Kommentare (`//!` + `///`) für alle öffentlichen Items
- Verifikation via `make clippy-tests`; kein direktes `cargo`/`make` während der Impl

**Nicht-Ziele dieser Iteration:**
- Vollständige `CustomTerminal<B>`-Implementierung (Inline-Viewport-Reflow wie codex)
- Syntax-Highlighting via `syntect` (kommt nach Slice 7)
- SIGTSTP/SIGCONT Job-Control
- Zellij-Kompatibilität

---

## 6. Risiken und Abhängigkeiten

| Risiko | Mitigation |
|--------|------------|
| `InlineTerminalGuard` (kein Alt-Screen) bricht bestehende Snapshot-Tests | Tests prüfen nur `SetupApp::on_key` TTY-frei; keine Snapshot-Tests vorhanden |
| `tokio::select!` in Event-Loop setzt async `run_chat_tui` voraus | `run_chat_tui` ist bereits von einem `current_thread`-Runtime getrieben; Umbau auf `async fn` + `block_on` ist trivial |
| `EnableBracketedPaste` wird von einigen Multiplexern (tmux ≤ 3.2) nicht weitergeleitet | Dokumentieren; Fallback: manuelle Key-by-Key-Eingabe bleibt immer funktional |
| Slice 3 (async Event-Loop) ändert Signatur von `run_chat_tui` | `harw-cli/src/chat.rs:64` ruft `run_chat_tui(model)` auf → Signatur beibehalten, intern `block_on` verwenden |

---

*Spec-Version: 2026-07-15. Slices 1–2 sind unabhängig voneinander; Slice 3 baut
auf Slice 2 auf; Slices 4–8 bauen auf Slice 3 auf.*
