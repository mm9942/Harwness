# Codex TUI – History-Scrolling & Pager-Overlay (Cluster B)

Quellverzeichnis: `/srv/dev-shared/projects/rust/codex/codex-rs/tui/src`

Ergänzt `01-events-and-app-loop.md` (Event-Loop, `insert_history.rs`, Inline-Viewport-Resize).
Dieses Dokument behandelt den bisher unbehandelten Teil: den dedizierten
Pager-/Transcript-Overlay (`pager_overlay.rs`) für interaktives Zurückscrollen,
plus die Frage nach sichtbarer Zeilenzahl und Tastennavigation.

---

## 1. Gibt es einen dedizierten Scrollback-/Pager-Modus?

**Ja.** `pager_overlay.rs` (1570 Zeilen) implementiert einen Alternate-Screen-Overlay
mit zwei Varianten (`pager_overlay.rs:53–56`):

```rust
pub(crate) enum Overlay {
    Transcript(TranscriptOverlay),  // Ctrl+T — volle Chat-Historie
    Static(StaticOverlay),          // z. B. Diff-Anzeige, Hilfe-Text
}
```

Beide teilen sich ein generisches `PagerView`-Widget (`pager_overlay.rs:120–314`),
das Scroll-Offset, Header, Content-Rendering und eine Bottom-Bar mit
Scroll-Prozentanzeige verwaltet.

### Öffnen

`Ctrl+T` (Default-Binding `open_transcript`, `keymap.rs:912`) wird in
`app/input.rs:167–176` behandelt:

```rust
if app_keymap_shortcuts_available && self.keymap.app.open_transcript.is_pressed(key_event) {
    let _ = tui.enter_alt_screen();
    self.overlay = Some(Overlay::new_transcript(
        self.transcript_cells.clone(),
        self.keymap.pager.clone(),
    ));
    tui.frame_requester().schedule_frame();
    return;
}
```

Äquivalent kapselt `App::open_transcript_overlay()` (`app_backtrack.rs:275–282`)
denselben Ablauf für den Backtrack-Einstieg (siehe Abschnitt 5).

`Overlay::new_transcript(cells, keymap)` klont **alle bisherigen**
`Arc<dyn HistoryCell>`-Zellen aus `App::transcript_cells` (billig — `Arc::clone`,
kein Deep-Copy) und baut daraus die Pager-Renderables.

### Schließen

Zwei Bindings in `PagerKeymap` (`keymap.rs:1111–1112`):
- `close`: `q` oder `Ctrl+C` → beendet **jeden** Overlay (Transcript und Static).
- `close_transcript`: `Ctrl+T` (identisch zur Öffnen-Taste) → schließt **nur** den
  Transcript-Overlay (Toggle-Verhalten).

```rust
e if self.view.keymap.close.is_pressed(e)
    || self.view.keymap.close_transcript.is_pressed(e) =>
{
    self.is_done = true;
    Ok(())
}
```
(`pager_overlay.rs:786–791`, nur in `TranscriptOverlay`; `StaticOverlay` kennt
nur `close`, nicht `close_transcript`.)

Das eigentliche Schließen (Alt-Screen verlassen, Backtrack-State zurücksetzen,
zurückgehaltene History-Zeilen nachträglich flushen) übernimmt
`App::close_transcript_overlay()` (`app_backtrack.rs:285–302`):

```rust
pub(crate) fn close_transcript_overlay(&mut self, tui: &mut tui::Tui) {
    let _ = tui.leave_alt_screen();
    let was_backtrack = self.backtrack.overlay_preview_active;
    if !self.deferred_history_lines.is_empty() {
        let lines = std::mem::take(&mut self.deferred_history_lines);
        tui.insert_history_hyperlink_lines_with_wrap_policy(lines, self.history_line_wrap_policy());
    }
    self.overlay = None;
    self.backtrack.overlay_preview_active = false;
    tui.frame_requester().schedule_frame();
    if was_backtrack { self.reset_backtrack_state(); }
}
```

`tui.leave_alt_screen()` (`tui.rs:758–770`) restauriert exakt das gespeicherte
`alt_saved_viewport` — der Inline-Chat-Viewport erscheint danach unverändert an
derselben Stelle.

---

## 2. Scroll-Navigation: Taste → Aktion

Tabelle aus `PagerKeymap`-Defaults (`keymap.rs:1094–1112`), verarbeitet in
`PagerView::handle_key_event` (`pager_overlay.rs:253–292`):

| Taste(n) | Feld | Aktion |
|---|---|---|
| `↑` / `k` | `scroll_up` | `scroll_offset -= 1` |
| `↓` / `j` | `scroll_down` | `scroll_offset += 1` |
| `PageUp` / `Shift+Space` / `Ctrl+B` | `page_up` | `scroll_offset -= page_height` |
| `PageDown` / `Space` / `Ctrl+F` | `page_down` | `scroll_offset += page_height` |
| `Ctrl+U` | `half_page_up` | `scroll_offset -= ⌈content_height/2⌉` |
| `Ctrl+D` | `half_page_down` | `scroll_offset += ⌈content_height/2⌉` |
| `Home` | `jump_top` | `scroll_offset = 0` |
| `End` | `jump_bottom` | `scroll_offset = usize::MAX` (Sentinel „ganz unten", wird beim Rendern geclampt) |
| `q` / `Ctrl+C` | `close` | Overlay schließen |
| `Ctrl+T` | `close_transcript` | nur Transcript-Overlay schließen (Toggle) |
| `Esc` (im Transcript-Overlay, außerhalb Backtrack-Preview) | — | startet Backtrack-Preview (highlight letzte User-Message) |
| `Esc` / `←` (in Backtrack-Preview) | — | eine User-Message weiter zurück |
| `→` (in Backtrack-Preview) | — | eine User-Message weiter vor |
| `Enter` (in Backtrack-Preview) | — | Rollback zur ausgewählten Message bestätigen |

Alle Bindings sind über `tui.keymap.pager.*` in der Config re-mapbar
(`RuntimeKeymap::resolve`, `keymap.rs:758–769`); Mausrad wird laut Code-Kommentar
in `enter_alt_screen()` (`tui.rs:741–742`) über `EnableAlternateScroll`
terminal-nativ auf Pfeiltasten übersetzt (kein eigener `MouseEvent`-Handler in
`pager_overlay.rs`).

`page_height()` (`pager_overlay.rs:299–302`) nutzt bevorzugt die zuletzt
gerenderte Content-Höhe (`last_content_height`), fällt sonst auf
`content_area(viewport_area).height` zurück — Paging ist also nie um mehr als
eine sichtbare Seite daneben, selbst vor dem ersten Render.

`is_scrolled_to_bottom()` (`pager_overlay.rs:317–335`) wird genutzt, um beim
Einfügen neuer Zellen (`insert_cell`) automatisch am unteren Ende zu bleiben
("follow along"), aber nur wenn der Nutzer nicht manuell hochgescrollt hat.

---

## 3. Sichtbare Zeilenzahl: Berechnung & Dynamik

**Vollständig dynamisch, kein Hardcoding.** Der Overlay läuft im Alt-Screen und
bekommt bei jedem Draw die **volle aktuelle Terminalgröße**:

```rust
// pager_overlay.rs:794–798 (TranscriptOverlay::handle_event)
TuiEvent::Draw | TuiEvent::Resize => {
    tui.draw(u16::MAX, |frame| {
        self.render(frame.area(), frame.buffer);
    })?;
    Ok(())
}
```

`u16::MAX` als `height`-Parameter an `Tui::draw()` bedeutet "nimm die volle
Terminalhöhe" (`tui.rs:913`: `area.height = height.min(size.height)`).
`frame.area()` liefert also die komplette Bildschirmgröße.

Von dort wird die Höhe in drei Etappen reduziert:

```
Terminal-Höhe (frame.area().height)
 └─ TranscriptOverlay::render (pager_overlay.rs:773–779)
     top_h    = area.height - 3      // unten: 3 Zeilen Key-Hints
     ┌─ PagerView::render (pager_overlay.rs:156–175)
     │   content_area = content_area(top)
     └─ PagerView::content_area (pager_overlay.rs:308–313)
         area.y      += 1            // 1 Zeile Header ("/ TRANSCRIPT")
         area.height -= 2            // 1 Header + 1 Bottom-Separator/%-Anzeige
```

**Ergebnis: `sichtbare_zeilen = terminal_höhe - 5`** (3 für Key-Hints unten,
1 für Header oben, 1 für Trennlinie/Prozentanzeige direkt unter dem Content).

Bei `TuiEvent::Resize` löst derselbe Draw-Pfad aus — es gibt **keinen**
separaten Resize-Handler für den Overlay; jede Render-Iteration berechnet
`content_height(width)` (`pager_overlay.rs:149–154`) und `page_height()` neu
aus der aktuellen `area.width`/`area.height`. Der Scroll-Offset selbst
(`usize`, absolute Zeilenzahl vom Content-Anfang) bleibt beim Resize erhalten
und wird nur nachträglich geclampt:

```rust
// pager_overlay.rs:168–170
self.scroll_offset = self
    .scroll_offset
    .min(content_height.saturating_sub(content_area.height as usize));
```

Das bedeutet: Scroll-Position bleibt bei Fensterverkleinerung "kleben" (bottom
bleibt bottom dank `usize::MAX`-Sentinel via `is_scrolled_to_bottom()`), aber
bei Verbreiterung/Verschmälerung ändert sich die Zeilen-zu-Row-Zuordnung
(Rewrap), da `content_height` von `desired_height(width)` pro Renderable
abhängt (Abschnitt 4) — der Offset ist ein Row-Offset im **neu gewrappten**
Content, kein stabiler Cell-Index. Ein Test dazu:
`transcript_overlay_paging_is_continuous_and_round_trips`
(`pager_overlay.rs:1397–1463`) verifiziert, dass PageDown/PageUp bei fester
Breite exakt round-tripped.

---

## 4. Wie eine History-Zelle ihre Zeilenzahl meldet

Kern-Trait `HistoryCell` (`history_cell/mod.rs:189–298`) definiert zwei
parallele Höhen-Methoden:

```rust
fn desired_height(&self, width: u16) -> u16 {
    self.desired_height_for_mode(width, HistoryRenderMode::Rich)
}
fn desired_height_for_mode(&self, width: u16, mode: HistoryRenderMode) -> u16 {
    Paragraph::new(Text::from(self.display_lines_for_mode(width, mode)))
        .wrap(Wrap { trim: false })
        .line_count(width)
        .try_into()
        .unwrap_or(0)
}
```

Das ist die Höhe für den **Haupt-Chat-Viewport** (Live-Ansicht). Für den
**Transcript-Overlay** gibt es eine separate, potenziell abweichende
Repräsentation:

```rust
// history_cell/mod.rs:243–279
fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
    self.display_lines(width)   // Default: identisch zur Live-Ansicht
}
fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
    plain_hyperlink_lines(self.transcript_lines(width))
}
fn desired_transcript_height(&self, width: u16) -> u16 {
    let lines = visible_lines(self.transcript_hyperlink_lines(width));
    // Workaround: ratatui zählt eine reine Whitespace-Zeile fälschlich als 2.
    if let [line] = &lines[..] && line.spans.iter().all(|s| s.content.chars().all(char::is_whitespace)) {
        return 1;
    }
    Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }).line_count(width).try_into().unwrap_or(0)
}
```

Konkrete Zellen überschreiben `transcript_lines()`, wenn sich die
Transcript-Darstellung von der Live-Darstellung unterscheidet (Kommentar:
z. B. `ExecCell` zeigt im Transcript alle Aufrufe mit `$`-Präfix und
Exit-Status, während die Live-Ansicht nur den letzten Call kompakt zeigt).

Im Overlay selbst nutzt `CellRenderable` (`pager_overlay.rs:394–412`) genau
diese Methode als `Renderable::desired_height`:

```rust
impl Renderable for CellRenderable {
    fn desired_height(&self, width: u16) -> u16 {
        self.cell.desired_transcript_height(width)
    }
}
```

Damit die potenziell teure Neuberechnung (Paragraph-Wrap über alle Zeilen)
nicht bei jedem Frame läuft, wird jedes `CellRenderable` in
`CachedRenderable` (`pager_overlay.rs:364–392`) gewrappt, das `height` nur bei
geändertem `width` neu berechnet (`std::cell::Cell<Option<u16>>`-Cache).
`PagerView::content_height(width)` (`pager_overlay.rs:149–154`) summiert dann
`desired_height(width)` über alle Renderables — das ist der "Gesamtzeilen"-Wert,
gegen den `scroll_offset` geclampt wird.

---

## 5. Verhältnis: Inline-Live-Viewport vs. interaktiver Pager

**Zwei getrennte Render-Pfade, dieselbe Datenquelle (`App::transcript_cells:
Vec<Arc<dyn HistoryCell>>`).**

| | Live-Viewport (Inline) | Transcript-Overlay (`Ctrl+T`) |
|---|---|---|
| Terminal-Modus | kein Alt-Screen (`tui.rs:246`, s. Doc 01) | `tui.enter_alt_screen()` |
| Zeilenmethode | `display_lines(width)` / `desired_height(width)` | `transcript_lines(width)` / `desired_transcript_height(width)` |
| Fertige Cells | via `insert_history_hyperlink_lines_with_mode_and_wrap_policy()` direkt in Terminal-Scrollback geschrieben (append-only, s. Doc 01 §9) | als `Vec<Box<dyn Renderable>>` im `PagerView` gehalten, jeden Frame neu in einen ratatui-`Buffer` gerendert |
| Scroll-Modell | kein internes Scrollen — Terminal-natives Scrollback | eigener `scroll_offset: usize` in `PagerView`, jeden Frame reclamped |
| Aktive/streamende Zelle | `ChatWidget.active_cell` wird direkt gerendert | als **"live tail"** gecached angehängt (s. u.) |
| Resize-Verhalten | `TranscriptReflowState` baut Scrollback aus `transcript_cells` neu (Doc 01 §8b) | `PagerView` wrapped bei jedem Draw automatisch neu — kein separater Reflow-Mechanismus nötig |

Während der Overlay offen ist, wird der Resize-Reflow des Inline-Viewports
explizit **pausiert** (`app/resize_reflow.rs:371–373`):

```rust
if self.overlay.is_some() {
    return Ok(());   // "Reflow is deferred while an overlay is active
                      //  because the overlay owns the current draw surface."
}
```

Der Overlay besitzt kein eigenes Modell für in-flight/streamende Inhalte —
`ChatWidget` bleibt Quelle der Wahrheit für die aktuell aktive Zelle. Damit der
Overlay während des Streamens nicht "hinterherhinkt", pflegt `App` bei jedem
Draw einen gecachten **"live tail"**:

```rust
// TranscriptOverlay::sync_live_tail (pager_overlay.rs:637–671)
pub(crate) fn sync_live_tail(
    &mut self,
    width: u16,
    active_key: Option<ActiveCellTranscriptKey>,
    compute_lines: impl FnOnce(u16) -> Option<Vec<HyperlinkLine>>,
) { /* nur neu berechnen, wenn sich width/revision/animation_tick geändert haben */ }
```

Der `ActiveCellTranscriptKey` (Revision-Zähler + Stream-Continuation-Flag +
optionaler Animation-Tick) wird von `App::overlay_forward_event()`
(`app_backtrack.rs:423–…`) bei jedem `TuiEvent::Draw` neu abgefragt und nur bei
Änderung neu gerendert — verhindert teures Re-Wrapping pro Frame, während der
Overlay trotzdem "live" wirkt. Committed Cells werden über
`insert_cell()` / `replace_cells()` / `consolidate_cells()`
(`pager_overlay.rs:532–623`) synchron zum Haupt-`transcript_cells`-Vektor
gehalten (jeweils vom `App` bei jedem entsprechenden State-Change aufgerufen).

**Zusatzfunktion des Overlays — Backtrack/Edit-Previous:** Der Transcript-Overlay
ist nicht nur ein reiner Pager, sondern auch die UI für "editiere eine frühere
User-Message" (`app_backtrack.rs`, 1003 Zeilen). Erstes `Esc` im Overlay
highlightet die letzte User-Message (`TranscriptOverlay::set_highlight_cell`,
`pager_overlay.rs:673–679`, ruft intern `scroll_chunk_into_view` auf, damit die
Selektion sichtbar bleibt), `←`/`→` wandern zwischen User-Messages, `Enter`
löst einen `AppCommand::thread_rollback(num_turns)` aus. Das ist funktional vom
reinen Scrollen getrennt (`BacktrackState` in `app_backtrack.rs:54–74`), teilt
sich aber denselben Overlay/Alt-Screen-Lifecycle.

---

## 6. Übertragung auf harw-tui

### Ist-Zustand (`harw-tui/src/app.rs`, `harw-tui/src/chat_scroll.rs`)

harw-tui hat bereits **mehr** als "kein Scroll-Modell": Es existiert
`ChatScroll` (`harw-tui/src/chat_scroll.rs`, 545 Zeilen) mit einem
offset-basierten Modell (`offset()`, `follows_tail()`, `page_up`/`page_down`,
`jump_to_top`/`jump_to_bottom`, `handle_key`, `handle_mouse`) — strukturell
ähnlich zu `PagerView`. Und `HistoryCell` (`harw-tui/src/history_cell.rs:69–92`)
hat bereits ein `desired_height(width)` mit Default-Implementierung über
`Paragraph`/`Wrap`, analog zu Codex.

Die entscheidenden Lücken gegenüber Codex:

1. **Kein Alt-Screen-Pager-Overlay.** Alles läuft in genau einem
   Alternate-Screen-Viewport (`harw-tui/src/app.rs:21–22`: "Kein
   `insert_before` / Terminal-Scrollback", Kommentar bestätigt fehlendes
   Inline+Overlay-Modell). Scrollen bedeutet: derselbe `Paragraph` wird mit
   anderem `.scroll((y, 0))`-Offset neu gezeichnet (`draw_viewport`,
   `harw-tui/src/app.rs:1553–1556`). Es gibt keine zweite,
   Full-History-fokussierte Ansicht, die vom Composer/Status-Footer getrennt
   ist.

2. **Hardcodierte Platzhalterwerte statt echter Content-Höhe beim
   Key-Handling.** `handle_key()` ruft
   `app.scroll.handle_key(key, 1000, 20)` (`harw-tui/src/app.rs:1166–1167`,
   Kommentar: "Konservative Schätzung: 1000 Zeilen, 20 sichtbar — ChatScroll
   clamped selbst"). `draw_viewport()` selbst rechnet zwar korrekt mit
   `all_lines.len()` und `history_area.height` (`app.rs:1541–1551`), aber die
   Tastenverarbeitung (PageUp-Schrittweite, Home/End-Clamping) verwendet feste
   Fantasiewerte statt der echten, breitenabhängigen Zeilenzahl — bei langen
   Sessions oder schmalen Terminals ist die Page-Schrittweite falsch
   bemessen (analog zu Codex' Bug, den `PagerView::page_height()` bewusst
   vermeidet, s. Abschnitt 2).

3. **Kein `transcript_lines()`-Äquivalent** — harw-tui's `HistoryCell` kennt
   nur eine Darstellung (`display_lines`), keine separate,
   ausführlichere Transcript-Variante für einen Pager-Modus.

### Konkreter Vorschlag für neue Typen/Module

- **`harw-tui/src/pager_overlay.rs`** (neu): Analog zu Codex' `PagerView` —
  ein `TranscriptOverlay { cells: Vec<Arc<dyn HistoryCell>>, scroll: ChatScroll
  (wiederverwenden!), highlight: Option<usize> }`. Rendert im Alt-Screen mit
  echter `frame.area().height` (kein Hardcoding).
- **`ChatScroll` erweitern**: `handle_key(key, total_lines, viewport)` sollte
  vom Aufrufer die **echten** Werte bekommen. Einfachster Fix ohne
  Architekturumbau: in `handle_key()` (`app.rs:1111`) zuerst `history_area`
  approximieren (letzte bekannte Terminalgröße minus Input/Status-Höhe) statt
  `1000, 20` zu hardcoden, oder `draw_viewport`'s Berechnung
  (`all_lines.len()`, `history_area.height`) in eine wiederverwendbare
  Funktion `fn history_layout(app, terminal_size) -> (total_lines, viewport)`
  auslagern und sowohl in `handle_key` als auch `draw_viewport` aufrufen.
- **`HistoryCell::transcript_lines(&self, width: u16) -> Vec<Line<'static>>`**
  mit Default-Delegation auf `display_lines()` — Vorbereitung für Zellen, die
  im Pager mehr Detail zeigen wollen (z. B. volle Tool-Ausgabe statt
  gekürzter Live-Ansicht).
- **Tastenbindung `Ctrl+T`**: neuer `HarwEvent`/`LineAction`-Fall
  `OpenTranscript`, analog zu Codex' `open_transcript`-Binding, der
  `guard.terminal()` in einen zweiten Alt-Screen-Zustand versetzt (oder,
  einfacher für harw-tui: da bereits alles im Alt-Screen läuft, reicht ein
  reiner Modus-Flag `ChatApp.pager_active: bool`, der `draw_viewport` auf ein
  Vollbild-Pager-Layout ohne Composer/Status umschaltet).
- **`is_scrolled_to_bottom()` / "follow along"**: `ChatScroll::follows_tail()`
  existiert bereits (`chat_scroll.rs:107`) und wird über `on_new_content()`
  gepflegt — dieser Teil ist funktional bereits äquivalent zu Codex'
  `is_scrolled_to_bottom()`/`follow_bottom`-Pattern in `insert_cell()`.

Priorität: Punkt 2 (hardcodierte 1000/20) ist der günstigste Fix mit sofortigem
Korrektheitsgewinn; Punkt 1 (dedizierter Pager) ist die strukturell größere
Änderung, die harw-tui überhaupt erst ein "Ctrl+T"-artiges Feature ermöglicht.
