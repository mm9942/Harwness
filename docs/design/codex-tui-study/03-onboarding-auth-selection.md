# Codex TUI – Onboarding / Auth / Selection Deep-Read

Quellen: `/srv/dev-shared/projects/rust/codex/codex-rs/tui/src/onboarding/` (alle Dateien),
`bottom_pane/list_selection_view.rs`, `bottom_pane/multi_select_picker.rs`,
`bottom_pane/selection_tabs.rs`, `bottom_pane/request_user_input/{mod,render,layout}.rs`,
`cwd_prompt.rs`, `oss_selection.rs`, `resume_picker.rs`.

---

## 1. Die Zustandsmaschine des Onboarding-Wizards

### 1.1 Top-Level: `Step`-Enum und `OnboardingScreen`

**Datei:** `onboarding/onboarding_screen.rs:54-58`

```rust
#[allow(clippy::large_enum_variant)]
enum Step {
    Welcome(WelcomeWidget),
    Auth(AuthModeWidget),
    TrustDirectory(TrustDirectoryWidget),
}
```

`OnboardingScreen` besitzt einen `Vec<Step>` (nicht Stack, nicht Map) – die Reihenfolge ist starr:
**Welcome → Auth → TrustDirectory**. Steps werden beim Bau in `OnboardingScreen::new` dynamisch
befüllt: Welcome immer, Auth nur wenn `show_login_screen`, TrustDirectory nur wenn
`show_trust_screen` (`onboarding_screen.rs:116–162`).

### 1.2 Schritt-Sichtbarkeit: `StepState`-Trait

**Datei:** `onboarding_screen.rs:66-73`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepState {
    Hidden,
    InProgress,
    Complete,
}

pub(crate) trait StepStateProvider {
    fn get_step_state(&self) -> StepState;
}
```

Jeder Step-Typ implementiert `StepStateProvider`. Der Screen nutzt
`current_steps()` / `current_steps_mut()` (`onboarding_screen.rs:171–199`): Er iteriert über alle
Steps, überspringt `Hidden`, sammelt alle `Complete`-Steps und hält beim ersten `InProgress` an.
So werden abgeschlossene Schritte **immer noch gerendert** (gestapelt oben), der aktive Step
unten.

Konkreter Ablauf:
- `WelcomeWidget::get_step_state()` → `Hidden` wenn `is_logged_in`, sonst `Complete`
  (`welcome.rs:108–113`). Welcome ist **nie** `InProgress` – es ist entweder sichtbar-fertig oder
  ausgeblendet. Das bedeutet: Welcome blockiert den Flow nie.
- `AuthModeWidget::get_step_state()` → `InProgress` solange `SignInState` nicht
  `ChatGptSuccess | ApiKeyConfigured`, dann `Complete` (`auth.rs:966–977`).
- `TrustDirectoryWidget::get_step_state()` → `Complete` wenn `selection.is_some()` oder
  `should_quit`, sonst `InProgress` (`trust_directory.rs:154–162`).

**Übergang Welcome → Auth:** Welcome meldet sofort `Complete` (da `is_logged_in=false`), daher
erscheint Auth als nächster `InProgress`-Step automatisch im selben Frame. Kein expliziter
Übergangs-Code nötig – `current_steps()` liefert `[Welcome, Auth]`, wobei Auth der letzte ist
und alle Key-Events empfängt.

**Übergang Auth → TrustDirectory:** Sobald `sign_in_state` auf `ChatGptSuccess` oder
`ApiKeyConfigured` wechselt, meldet Auth `Complete`. Beim nächsten Render-Aufruf liefert
`current_steps()` `[Welcome, Auth, TrustDirectory]`, wobei TrustDirectory als `InProgress`
alle Events bekommt.

### 1.3 Event-Routing

**Datei:** `onboarding_screen.rs:276–323`

Key-Events landen in `OnboardingScreen::handle_key_event`. Die Logik:

1. Quit-Erkennung (mit API-Key-Guard – siehe Abschnitt 3).
2. Welcome bekommt **immer** alle Key-Events (für Animation-Toggle-Shortcut).
3. Der letzte Step in `current_steps_mut()` bekommt alle anderen Events.
4. Nach jedem Key-Event: `request_frame.schedule_frame()`.

Paste-Events gehen nur an den letzten aktiven Step (`onboarding_screen.rs:325–334`).

### 1.4 `run_onboarding_app`: Der Event-Loop

**Datei:** `onboarding_screen.rs:474–575`

```rust
pub(crate) async fn run_onboarding_app(
    args: OnboardingScreenArgs,
    mut app_server: Option<&mut AppServerSession>,
    tui: &mut Tui,
) -> Result<OnboardingResult>
```

Doppelter `tokio::select!` auf `tui_events` und `app_server.next_event()`. Für TUI-Events:
- `TuiEvent::Key` → `handle_key_event` + optionales `persist_selected_trust`
- `TuiEvent::Paste` → `handle_paste`
- `TuiEvent::Draw | TuiEvent::Resize` → vollständiger Redraw + Post-Login-Cleanup

Server-Notifications (`AccountLoginCompleted`, `AccountUpdated`) werden direkt an
`AuthModeWidget` weitergeleitet.

---

## 2. Auth-Step: `AuthModeWidget` und `SignInState`

### 2.1 Die Auth-Zustands-Maschine

**Datei:** `onboarding/auth.rs:78–87`

```rust
#[derive(Clone)]
pub(crate) enum SignInState {
    PickMode,
    ChatGptContinueInBrowser(ContinueInBrowserState),
    ChatGptDeviceCode(ContinueWithDeviceCodeState),
    ChatGptSuccessMessage,
    ChatGptSuccess,
    ApiKeyEntry(ApiKeyInputState),
    ApiKeyConfigured,
}
```

State-Übergänge – vollständiger Graph:

```
PickMode
  ├─(Enter / '1' / '2' / '3' auf ChatGpt)─→ ChatGptContinueInBrowser  (tokio::spawn startet OAuth)
  ├─(DeviceCode gewählt)─────────────────→ ChatGptDeviceCode           (tokio::spawn startet headless login)
  └─(ApiKey gewählt)─────────────────────→ ApiKeyEntry(ApiKeyInputState{value, prepopulated_from_env})

ChatGptContinueInBrowser
  ├─(ServerNotification::AccountLoginCompleted, success=true)──→ ChatGptSuccessMessage
  ├─(ServerNotification::AccountLoginCompleted, success=false)─→ PickMode + error gesetzt
  └─(Esc / Cancel)─────────────────────────────────────────────→ PickMode (cancel_login_attempt via spawn)

ChatGptDeviceCode
  ├─(ServerNotification::AccountLoginCompleted, success=true)──→ ChatGptSuccessMessage
  ├─(ServerNotification::AccountLoginCompleted, success=false)─→ PickMode + error gesetzt
  └─(Esc / Cancel)─────────────────────────────────────────────→ PickMode

ChatGptSuccessMessage
  └─(Enter)──→ ChatGptSuccess   [→ StepState::Complete → TrustDirectory erscheint]

ApiKeyEntry
  ├─(Enter, value nicht leer)───→ tokio::spawn save_api_key → ApiKeyConfigured  [→ StepState::Complete]
  ├─(Enter, value leer)─────────→ error "API key cannot be empty"
  ├─(Esc)───────────────────────→ PickMode
  ├─(Backspace)─────────────────→ letztes Zeichen entfernen (oder clear wenn prepopulated_from_env)
  ├─(Char c, kein Modifier)─────→ value.push(c)
  └─(Paste)─────────────────────→ handle_api_key_entry_paste → value = trimmed paste
```

### 2.2 `AuthModeWidget`-Struktur

**Datei:** `auth.rs:226–237`

```rust
pub(crate) struct AuthModeWidget {
    pub request_frame: FrameRequester,
    pub highlighted_mode: SignInOption,
    pub error: Arc<RwLock<Option<String>>>,
    pub sign_in_state: Arc<RwLock<SignInState>>,
    pub login_status: LoginStatus,
    pub app_server_request_handle: AppServerRequestHandle,
    pub forced_login_method: Option<ForcedLoginMethod>,
    pub animations_enabled: bool,
    pub animations_suppressed: Cell<bool>,
}
```

`sign_in_state` und `error` sind `Arc<RwLock<...>>` weil `tokio::spawn`-Tasks (OAuth-Flow,
API-Key-Speicherung) diese aus einem anderen Thread heraus schreiben und dann
`request_frame.schedule_frame()` aufrufen. `animations_suppressed` ist `Cell<bool>` (kein Lock)
weil es nur im Render-Thread geschrieben wird.

### 2.3 API-Key-Eingabe: Sofort-Paste-Akzeptanz

**Datei:** `auth.rs:746–768` (`handle_api_key_entry_paste`)

```rust
fn handle_api_key_entry_paste(&mut self, pasted: String) -> bool {
    let trimmed = pasted.trim();
    if trimmed.is_empty() { return false; }
    let mut guard = self.sign_in_state.write().unwrap();
    if let SignInState::ApiKeyEntry(state) = &mut *guard {
        if state.prepopulated_from_env {
            state.value = trimmed.to_string();
            state.prepopulated_from_env = false;
        } else {
            state.value.push_str(trimmed);
        }
        self.set_error(None);
    } else {
        return false;
    }
    drop(guard);
    self.request_frame.schedule_frame();
    true
}
```

Paste wird automatisch getrimmt (keine Newlines im Key). Wenn der Key aus `OPENAI_API_KEY`
vorausgefüllt war (`prepopulated_from_env=true`), **ersetzt** das Paste den vorhandenen Wert
statt anzuhängen – wichtig für das UX.

### 2.4 Quit-Guard während API-Key-Eingabe

**Datei:** `onboarding_screen.rs:343–353`

```rust
fn suppress_quit_while_typing_api_key(
    key_event: KeyEvent,
    api_key_entry_context: ApiKeyEntryContext,
) -> bool {
    api_key_entry_context.active
        && api_key_entry_context.has_text
        && matches!(key_event.code, KeyCode::Char(_))
        && !key_event.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}
```

Nur unterdrückt wenn: API-Key-Entry aktiv AND Feld nicht leer AND Taste ist `Char` ohne
Modifier. Ctrl+C / Ctrl+D funktionieren immer als Notaus.

### 2.5 Animations-Suppression

Wenn der Browser-Auth-URL sichtbar ist (zum Kopieren), wird die gesamte Onboarding-Renderloop
eingefroren (`should_suppress_animations()` → `true`), damit Terminal-Selektion nicht durch
Redraws unterbrochen wird. `OnboardingScreen::render_ref` setzt
`widget.set_animations_suppressed(suppress_animations)` für jeden sichtbaren Step.

---

## 3. Modus-Auswahl: `SignInOption` und Highlighting

**Datei:** `auth.rs:89–94`

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SignInOption {
    ChatGpt,
    DeviceCode,
    ApiKey,
}
```

Navigation:
- Up/Down (`keys::MOVE_UP/MOVE_DOWN`) → `move_highlight(±1)` mit Modulo-Wrap
- `'1'` / `'2'` / `'3'` → `select_option_by_index(idx)` → sofort `handle_sign_in_option`
  (keine Enter-Bestätigung nötig!)
- Enter → `handle_sign_in_option(self.highlighted_mode)`

`keys::SELECT_FIRST/SECOND/THIRD` (`auth.rs:189–200`) erlauben Sofort-Selektion mit Ziffern:
Zahl drücken = Auswahl + sofortiger Übergang. Das ist der „Direkt-Routing"-Mechanismus.

`displayed_sign_in_options()` vs. `selectable_sign_in_options()` (`auth.rs:315–336`): Die
dargestellten Options können mehr enthalten als die navigierbaren (z.B. disabled ChatGPT-Login
wenn `ForcedLoginMethod::Api`).

---

## 4. `TrustDirectoryWidget`: Einfache Zwei-Option-Auswahl

**Datei:** `trust_directory.rs:24–178`

Einfachstes Muster: zwei `TrustDirectorySelection`-Varianten (`Trust`, `Quit`), ein
`highlighted`-Feld. Key-Handling:
- Up/Down → `highlighted` wechseln
- `'1'` / `'y'` → `handle_trust()` (setzt `selection = Some(Trust)`)
- `'2'` / `'n'` / Esc / 'q' → `handle_quit()` (setzt `should_quit = true`)
- Enter → je nach `highlighted`

Übergang zum nächsten Step: `handle_trust()` setzt `selection = Some(Trust)` → `get_step_state()`
gibt `Complete` zurück → `current_steps()` zeigt TrustDirectory nicht mehr als aktiven Step. Der
Screen ist dann `is_done()` weil kein Step mehr `InProgress` ist.

---

## 5. `ListSelectionView`: Generisches Auswahl-Popup

**Datei:** `bottom_pane/list_selection_view.rs`

### 5.1 Schlüssel-Typen

```rust
pub(crate) struct SelectionItem {
    pub name: String,
    pub name_prefix_spans: Vec<Span<'static>>,
    pub toggle: Option<SelectionToggle>,         // für Multi-State-Items
    pub display_shortcut: Option<KeyBinding>,    // Shortcut-Buchstabe in der Zeile
    pub description: Option<String>,
    pub selected_description: Option<String>,    // andere Beschreibung wenn ausgewählt
    pub is_current: bool,
    pub is_default: bool,
    pub is_disabled: bool,
    pub actions: Vec<SelectionAction>,           // Box<dyn Fn(&AppEventSender)>
    pub dismiss_on_select: bool,                 // Popup schließt sofort nach Auswahl
    pub dismiss_parent_on_child_accept: bool,    // Parent-Popup schließt beim Kind-Accept
    pub search_value: Option<String>,
    pub disabled_reason: Option<String>,
}
```

### 5.2 `SelectionViewParams`: Konfigurations-Builder

`SelectionViewParams` ist ein Default-befülltes Struct, kein Builder-Pattern. Alle Felder haben
sinnvolle Defaults:

```rust
pub(crate) struct SelectionViewParams {
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub footer_note: Option<Line<'static>>,
    pub footer_hint: Option<Line<'static>>,
    pub items: Vec<SelectionItem>,
    pub tabs: Vec<SelectionTab>,
    pub is_searchable: bool,
    pub search_placeholder: Option<String>,
    pub col_width_mode: ColumnWidthMode,   // AutoVisible | AutoAllRows | Fixed
    pub row_display: SelectionRowDisplay,  // Wrapped | SingleLine
    pub side_content: Box<dyn Renderable>, // Preview-Panel rechts
    pub side_content_width: SideContentWidth,
    pub on_selection_changed: OnSelectionChangedCallback,  // Live-Preview
    pub on_cancel: OnCancelCallback,
    pub allow_cancel: bool,
    // ...
}
```

### 5.3 Nummer-Tasten = Sofort-Auswahl (ohne Enter)

**Datei:** `list_selection_view.rs:1022–1042`

```rust
KeyEvent { code: KeyCode::Char(c), modifiers, .. }
    if !self.is_searchable
        && !modifiers.contains(KeyModifiers::CONTROL)
        && !modifiers.contains(KeyModifiers::ALT) =>
{
    if let Some(idx) = c
        .to_digit(10)
        .map(|d| d as usize)
        .and_then(|number| self.actual_idx_for_enabled_number(number))
    {
        self.state.selected_idx = Some(idx);
        self.accept();   // Fires all actions + sets completion
    }
}
```

Wenn `is_searchable=false`, aktiviert eine Zifferntaste die entsprechende Zeile **direkt** ohne
Enter. Das ist das Muster, das „Auswahl treffen → sofort weiter" implementiert.

### 5.4 `accept()`: Actions-Dispatch

**Datei:** `list_selection_view.rs:762–792`

```rust
fn accept(&mut self) {
    // ...
    for act in &item.actions {
        act(&self.app_event_tx);  // Alle registrierten Actions feuern
    }
    if item.dismiss_on_select {
        self.completion = Some(ViewCompletion::Accepted);
    }
}
```

`ViewCompletion::Accepted` signalisiert dem BottomPane, dieses View zu entfernen. Die eigentliche
Logik steckt in den `actions: Vec<SelectionAction>` (closures über `AppEventSender`).

### 5.5 Live-Filtering (Suchbar-Modus)

Wenn `is_searchable=true`: Printable-Chars gehen in `search_query`, nicht in Navigation.
`apply_filter()` wird nach jeder Query-Änderung aufgerufen und aktualisiert `filtered_indices`.
Paste in searchable List → `normalize_pasted_search_query` (entfernt Newlines) → Query erweitern.

### 5.6 Tabs via `SelectionTab`

**Datei:** `bottom_pane/selection_tabs.rs:16–21`

```rust
pub(crate) struct SelectionTab {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) header: Box<dyn Renderable>,
    pub(crate) items: Vec<SelectionItem>,
}
```

Tab-Wechsel: Links/Rechts-Pfeile wenn `tabs_enabled()`. Jeder Tab hat eigene Items + eigenen
Header. `render_tab_bar()` wrapping-fähig (wenn Tabs nicht in eine Zeile passen).

---

## 6. `MultiSelectPicker`: Mehrfachauswahl mit Fuzzy-Suche

**Datei:** `bottom_pane/multi_select_picker.rs`

Unterschied zu `ListSelectionView`: Jedes Item hat `enabled: bool`-Zustand (Checkbox `[x]`/`[ ]`).
Auswahl-Mechanismus: Leertaste toggled, Enter bestätigt alle aktivierten Items.

Builder-Pattern:
```rust
MultiSelectPicker::builder(title, subtitle, tx)
    .items(items)
    .enable_ordering()       // Links/Rechts = Reihenfolge ändern
    .on_preview(|items| ...) // Live-Preview-Zeile
    .on_confirm(|ids, tx| ...)
    .on_cancel(|tx| ...)
    .build()
```

Fuzzy-Matching via `codex_utils_fuzzy_match::fuzzy_match` – scorebasiert, sortiert nach Score.
Printable Keys gehen **immer** in `search_query` (kein Navigation-Alias für `j`/`k` wenn Text).

---

## 7. `RequestUserInputOverlay`: Freeform-Eingabe + Option-Auswahl kombiniert

**Datei:** `bottom_pane/request_user_input/mod.rs`

Hybrides Widget: Options-Liste (wie `ListSelectionView`) + ChatComposer-Feld (Notizen/Freitext)
in einer Ansicht. Fokus-Enum:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Options,
    Notes,
}
```

**Tab** wechselt zwischen Options und Notes. **Typing in Options-Focus** wechselt automatisch zu
Notes. Paste in Options-Focus → automatisch zu Notes. Dieses „Jump-to-Notes-on-Type" ist ein
kritisches UX-Detail: Der User muss `Tab` nicht erst drücken.

Wichtig für harw-tui: `ChatComposer` wird mit `ChatComposerConfig::plain_text()` verwendet –
Slash-Commands, Image-Attachments etc. sind deaktiviert. Das zeigt wie man denselben
Composer-Widget ohne alle Features nutzt.

---

## 8. `CwdPromptAction` / `CwdSelection`: Minimales Prompt-Pattern

**Datei:** `cwd_prompt.rs:27–58`

```rust
pub(crate) enum CwdPromptAction { Resume, Fork }
pub(crate) enum CwdSelection { Current, Session }
pub(crate) enum CwdPromptOutcome { Selection(CwdSelection), Exit }
```

`run_cwd_selection_prompt` ist eine vollständige synchrone TUI-Schleife (eigener
`tokio::select!`-Loop), nicht Teil des Onboarding-Flows. Zeigt das Muster für kleine,
eigenständige modals die ihren eigenen Event-Loop besitzen und einen Outcome zurückgeben.

---

## 9. Rendering-Architektur: Schicht für Schicht

### 9.1 `WidgetRef` vs. `Renderable`

Onboarding-Steps implementieren `ratatui::widgets::WidgetRef` (Ratatui-nativer Trait).
Bottom-Pane-Popups implementieren ein internes `Renderable`-Trait:

```rust
pub(crate) trait Renderable {
    fn desired_height(&self, width: u16) -> u16;
    fn render(&self, area: Rect, buf: &mut Buffer);
}
```

`desired_height` ist entscheidend: Jeder Step/View berechnet seine benötigte Höhe dynamisch
(abhängig von Terminalbreite). `OnboardingScreen::render_ref` misst jeden sichtbaren Step in
einem Scratch-Buffer (`Buffer::empty`) und stacked sie dann von oben nach unten.

### 9.2 Layout-Muster in `AuthModeWidget::render_api_key_entry`

**Datei:** `auth.rs:617–682`

```rust
let [intro_area, input_area, footer_area] = Layout::vertical([
    Constraint::Min(4),
    Constraint::Length(3),
    Constraint::Min(2),
])
.areas(area);
```

Dreiteiler: Intro-Text oben, Eingabefeld (3 Zeilen = Block mit Border) in der Mitte, Footer unten.
Das Eingabefeld nutzt `Block::default().title("API key").borders(Borders::ALL).border_type(BorderType::Rounded)`.
Cyan als Highlight-Farbe für aktive Felder.

---

## 10. Keyboard-Binding-System: `KeyBinding`-Arrays

**Datei:** `onboarding/keys.rs`

```rust
pub(crate) const CONFIRM: [KeyBinding; 1] = [key_hint::plain(KeyCode::Enter)];
pub(crate) const CANCEL: [KeyBinding; 1] = [key_hint::plain(KeyCode::Esc)];
pub(crate) const QUIT: [KeyBinding; 3] = [
    key_hint::plain(KeyCode::Char('q')),
    key_hint::ctrl(KeyCode::Char('c')),
    key_hint::ctrl(KeyCode::Char('d')),
];
```

Der Trait `KeyBindingListExt` (implementiert für `[KeyBinding; N]`) stellt `is_pressed(key_event)
→ bool` bereit. Damit können alle Bindings einer Aktion prüfen ohne `match`-Spaghetti.

---

## 11. Übertragung auf harw-tui: Konkrete Handlungsempfehlungen

### 11.1 Wizard-Zustandsmaschine

**Problem harw-tui:** Kein strukturierter Wizard-Flow, Schritte nicht modelliert.

**Lösung nach Codex-Vorbild:**

```rust
// harw-tui/src/onboarding/mod.rs (neu)
enum Step {
    Welcome(WelcomeWidget),
    Auth(AuthWidget),          // Unser gebrochener Flow
    Config(ConfigWidget),
}

pub(crate) trait StepStateProvider {
    fn get_step_state(&self) -> StepState;
}

pub(crate) struct OnboardingScreen {
    steps: Vec<Step>,
    is_done: bool,
}

impl OnboardingScreen {
    fn current_steps_mut(&mut self) -> Vec<&mut Step> {
        // Identisch mit codex: Skip Hidden, sammle Complete, stop bei InProgress
    }
}
```

**Key-Insight:** Welcome muss `Complete` (nicht `InProgress`) zurückgeben um nicht zu blockieren.
Auth ist `InProgress` bis Key gespeichert. Kein manuelles Step-Routing nötig.

### 11.2 Auth-Flow: Der gebrochene Übergang

**Problem harw-tui:** Nach Auswahl des Auth-Modus (z.B. API-Key) kein sofortiger Übergang zum
Eingabefeld.

**Codex-Lösung:** `handle_sign_in_option` wird direkt beim Key-Press aufgerufen (nicht beim
nächsten Frame). Es ändert `sign_in_state` → `ApiKeyEntry`, ruft
`request_frame.schedule_frame()`. Der neue State wird beim nächsten Frame gerendert.

**Konkret in harw-tui (`src/app.rs`):**
1. `AppState::AuthModeSelection` → äquivalent zu `SignInState::PickMode`
2. Bei Zahl-Taste '1'/'2': sofort `AppState` wechseln + Frame schedulen:
   ```rust
   KeyCode::Char('1') => {
       self.state = AppState::ApiKeyEntry(ApiKeyInputState::default());
       self.request_frame.schedule_frame();
   }
   ```
3. Kein `match` auf `AppState` in der Render-Funktion notwendig wenn jeder State sein eigenes
   Widget rendert (wie `WidgetRef for Step`).

### 11.3 Paste-Akzeptanz für API-Key

**Problem harw-tui:** Paste landet nicht im API-Key-Feld.

**Codex-Lösung:** `TuiEvent::Paste(text)` wird in `run_onboarding_app:509` abgefangen und direkt
an `onboarding_screen.handle_paste` delegiert. In `auth.rs` (`handle_api_key_entry_paste`):
1. Trim durchführen
2. Wenn `prepopulated_from_env`: ersetzen statt anhängen
3. `schedule_frame()`

**In harw-tui `app.rs`:** Im Event-Loop `TuiEvent::Paste` behandeln:
```rust
TuiEvent::Paste(text) => {
    if let AppState::ApiKeyEntry(ref mut state) = self.state {
        state.value = text.trim().to_string();
        self.frame_requester.schedule_frame();
    }
}
```
Crossterm Paste-Events kommen nur wenn `crossterm::execute!(stdout, EnableBracketedPaste)` in
`setup.rs` aktiviert ist.

### 11.4 Quit-Schutz während Text-Eingabe

**Problem harw-tui:** `q` bricht die App ab während man einen API-Key tippt.

**Codex-Lösung:** `suppress_quit_while_typing_api_key` prüft ob Feld nicht leer AND kein
Modifier. In harw-tui `handle_key_event`:
```rust
let suppress_quit = self.state.is_api_key_entry_active()
    && self.state.api_key_has_text()
    && matches!(key.code, KeyCode::Char(_))
    && !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);

if !suppress_quit && keys::QUIT.is_pressed(key) {
    self.is_done = true;
    return;
}
```

### 11.5 `ListSelectionView` als harw-tui-Widget

Für `/command`-Autocompletion und Menüs: `ListSelectionView` vollständig portierbar. Minimales
eigenes Äquivalent:

```rust
pub(crate) struct SelectionPopup {
    items: Vec<SelectionItem>,
    selected_idx: usize,
    search_query: String,       // wenn is_searchable
    filtered_indices: Vec<usize>,
    completion: Option<ViewCompletion>,
}
```

**Sofort-Auswahl via Zahl-Taste** (ohne Enter): `'1'` → `self.accept(0)` → direkt Action feuern
→ `completion = Accepted`. Das ist der Kern des „Auswahl → sofort weiter"-Flows.

### 11.6 `FrameRequester::schedule_frame()` Pattern

Codex plant Frames explizit – kein festes Tick-Interval. In harw-tui `setup.rs`:
- Expose `FrameRequester` oder äquivalenten `mpsc`-Sender
- Nach jedem State-Change: `schedule_frame()` aufrufen
- Event-Loop: `TuiEvent::Draw` wenn ein Frame angefordert wurde

Dieses Muster verhindert unnötige Redraws und CPU-Verbrauch.

### 11.7 Animationssuppression für Copyable-Content

Wenn ein Auth-URL oder API-Key sichtbar ist (User muss kopieren können): komplette
Animations-Pause via Flag. In harw-tui: `AnimationSuppressed`-State-Flag, das alle
`schedule_frame_in`-Calls blockiert.

---

## Anhang: Datei-Referenz-Tabelle

| Datei | Schlüssel-Typ | Zweck |
|-------|--------------|-------|
| `onboarding/onboarding_screen.rs` | `Step`, `OnboardingScreen`, `StepState`, `KeyboardHandler` | Wizard-Orchestration |
| `onboarding/auth.rs` | `AuthModeWidget`, `SignInState`, `ApiKeyInputState` | Auth-Flow-State-Machine |
| `onboarding/auth/headless_chatgpt_login.rs` | `start_headless_chatgpt_login`, `render_device_code_login` | Device-Code-Flow |
| `onboarding/welcome.rs` | `WelcomeWidget` | Animations-Splash, immer `Complete` |
| `onboarding/trust_directory.rs` | `TrustDirectoryWidget`, `TrustDirectorySelection` | Binäre Ja/Nein-Auswahl |
| `onboarding/keys.rs` | `CONFIRM`, `CANCEL`, `QUIT`, `MOVE_UP/DOWN`, `SELECT_FIRST/SECOND/THIRD` | Onboarding-Shortcuts |
| `bottom_pane/list_selection_view.rs` | `ListSelectionView`, `SelectionItem`, `SelectionViewParams` | Generisches Auswahl-Popup |
| `bottom_pane/multi_select_picker.rs` | `MultiSelectPicker`, `MultiSelectPickerBuilder`, `MultiSelectItem` | Mehrfachauswahl mit Fuzzy |
| `bottom_pane/selection_tabs.rs` | `SelectionTab`, `render_tab_bar` | Tab-Navigation in Popups |
| `bottom_pane/request_user_input/mod.rs` | `RequestUserInputOverlay`, `Focus`, `AnswerState` | Hybrid Options+Freeform |
| `cwd_prompt.rs` | `CwdPromptAction`, `CwdPromptOutcome`, `run_cwd_selection_prompt` | Eigenständige Modal-Schleife |
