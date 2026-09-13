# Cluster D — Render, Style & Dynamic Output

> Tiefenanalyse der codex-rs TUI: Was macht die Ausgabe schön und dynamisch?
> Dateibasis: `codex-rs/tui/src/` — alle Pfade relativ dazu.

---

## 1. Streaming-Pipeline: Wie LLM-Deltas Zeile für Zeile erscheinen

### Architektur-Überblick

```
LLM-Delta-Stream
    │
    ▼
MarkdownStreamCollector          (markdown_stream.rs)
    │  push_delta(&str)          — akkumuliert Bytes
    │  commit_complete_source()  — gibt Quelle bis zum letzten \n zurück
    │  finalize_and_drain_source() — spült Rest beim Turn-Ende
    │
    ▼  Vec<HyperlinkLine>
render_markdown_agent_with_links_and_cwd()  (markdown.rs:~120)
    │
    ▼
StreamState.enqueue(lines)       (streaming/mod.rs:92-99)
    │  QueuedLine { line, enqueued_at: Instant }
    │
    ▼  pop_front() je Frame
StreamingAgentTailCell (live)    (history_cell/messages.rs)
    oder
AgentMarkdownCell (finalisiert)  (history_cell/messages.rs)
```

### `MarkdownStreamCollector` — `markdown_stream.rs`

```rust
pub(crate) struct MarkdownStreamCollector {
    buffer: String,
    committed_source_len: usize,
    width: Option<usize>,
}
```

- `push_delta(&str)` hängt an `buffer` an.
- `commit_complete_source() -> Option<String>`: gibt neue Quelle ab `committed_source_len`
  bis zum letzten `\n` zurück; `None` wenn kein neues `\n` existiert. Das ist der
  **Newline-Gate** — kein Partial-Render mitten in einer Zeile.
- `finalize_and_drain_source() -> String`: hängt ein `\n` an, wenn keines vorhanden ist,
  und leert den Buffer. Wird beim Turn-Ende aufgerufen.

Der Collector arbeitet auf **rohem UTF-8-Source**, nicht auf gerenderten Ratatui-Lines.

### `StreamState` — `streaming/mod.rs`

```rust
pub(crate) struct StreamState {
    pub(crate) collector: MarkdownStreamCollector,
    queued_lines: VecDeque<QueuedLine>,
    pub(crate) has_seen_delta: bool,
}
struct QueuedLine { line: HyperlinkLine, enqueued_at: Instant }
```

Wichtige Methoden:

| Methode | Semantik |
|---|---|
| `new(width, cwd)` | Erzeugt Collector + leere Queue |
| `clear()` | Setzt Collector und Queue zurück (neuer Turn) |
| `step() -> Vec<HyperlinkLine>` | Pop 1 Zeile |
| `drain_n(max) -> Vec<HyperlinkLine>` | Pop bis zu `max` Zeilen (clamps auf Queue-Länge) |
| `enqueue(lines)` | Stempelt alle mit `Instant::now()` |
| `oldest_queued_age(now) -> Option<Duration>` | Für Policy-Entscheidungen (Drosselung) |
| `clear_queue()` | Leert Queue ohne Collector zu löschen |

### Live vs. Finalisiert: `StreamingAgentTailCell` vs. `AgentMarkdownCell`

`StreamingAgentTailCell` (history_cell/messages.rs) speichert die während des Streams
pre-gerenderten `Vec<HyperlinkLine>` und wrappt sie **nicht** erneut bei Resize. Das verhindert,
dass in-progress Tabellenbordüren auf halber Strecke umgebrochen werden.

`AgentMarkdownCell` (history_cell/messages.rs) speichert dagegen:

```rust
pub(crate) struct AgentMarkdownCell {
    markdown_source: String,  // raw Markdown
    cwd: PathBuf,
}
```

Bei jedem `display_lines(width)` oder `display_hyperlink_lines(width)`-Aufruf wird die Quelle
**neu gerendert** — dadurch sind Tabellen nach Terminal-Resize korrekt ausgerichtet. Das ist der
**Re-Render-on-Resize**-Pattern: niemals gerenderte Lines persistent speichern, wenn korrekte
Breite wichtig ist.

### Übertragung auf harw-tui

- **Sofort**: Einen `MarkdownStreamCollector` (18 Zeilen Kern-Logik) in `app.rs` integrieren.
  LLM-Deltas über `push_delta()` einfüttern, Frame-Tick `commit_complete_source()` abfragen,
  gerenderte Lines in eine `VecDeque` einreihen, pro Frame eine Zeile committen.
- **Resize-sicher**: Finalisierte Agent-Messages als `{ source: String, cwd: PathBuf }` speichern
  (nicht als `Vec<Line>`), sodass `display_lines(width)` immer fresh rendern kann.
- **Keine Panik**: `commit_complete_source()` gibt `None` zurück bis das erste `\n` kommt —
  der Render-Code muss das graceful behandeln (kein `unwrap()`).

---

## 2. Markdown-Rendering: pulldown-cmark → Ratatui Lines

### Öffentliche Einstiegspunkte — `markdown.rs`

```rust
// Allgemein (kein fence-unwrapping)
pub fn append_markdown(src: &str, width: usize, cwd: &Path, lines: &mut Vec<Line<'static>>)

// Für LLM-Output (unwrappt ```md / ```markdown Fences mit Tabellen-Syntax)
pub fn append_markdown_agent(src: &str, width: usize, lines: &mut Vec<Line<'static>>)

// Mit Hyperlink-Metadaten (für klickbare Terminal-Links)
pub fn render_markdown_agent_with_links_and_cwd(
    src: &str, width: usize, cwd: &Path
) -> Vec<HyperlinkLine>
```

`unwrap_markdown_fences(src) -> Cow<'a, str>` (markdown.rs): Zero-Copy fast path wenn
keine Fences. Puffert den vollen Fence-Body und unwrappt nur `` ```md `` / `` ```markdown `` die
header+delimiter Tabellen-Syntax enthalten. Konservativ: kein versehentliches Unwrapping von
echten Code-Blöcken.

### Kern-Renderer — `markdown_render.rs`

```rust
struct Writer<'a, I: Iterator<Item = (Event<'a>, Range<usize>)>> {
    iter: I,
    output: Vec<HyperlinkLine>,
    // Zwischenpuffer für Code-Blöcke, laufende Spans, etc.
}
```

Einstiegspunkt:
```rust
pub(crate) fn render_markdown_lines_with_width_and_cwd(
    input: &str, width: usize, cwd: &Path
) -> Vec<HyperlinkLine>
```

#### Styles (`MarkdownStyles`)

Hard-kodierte Defaults in `markdown_render.rs`:

| Element | Style |
|---|---|
| H1 | `bold` + `underlined` |
| H2 | `bold` |
| Inline code | `cyan` |
| Links | `cyan` + `underlined` |
| Blockquote | `green` |
| Ordered-List-Marker | `light_blue` |

#### Tabellen-Rendering-Pipeline

1. **Filter spillover**: Zeilen die die Terminalbreite um mehr als 50% überschreiten werden
   in key/value-Fallback-Modus umgeschaltet.
2. **Spaltenklassifikation**: `TableColumnKind` — `Narrative` (Fließtext), `TokenHeavy` (IDs,
   Code, Zahlen), `Compact` (kurze Labels).
3. **Breiten-Iteration**: Schrumpft zuerst `TokenHeavy`-Spalten, dann `Narrative`, dann
   `Compact`. Iteriert bis alles in `width` passt.
4. **Render**: Zeilen werden mit `TABLE_COLUMN_GAP=2`, `TABLE_CELL_PADDING=1` abstandsiert.
   Header-Separator: `'━'` (U+2501), Body-Separator: `'─'` (U+2500).

Konstanten in `markdown_render.rs`:
```rust
const TABLE_COLUMN_GAP: usize = 2;
const TABLE_CELL_PADDING: usize = 1;
const TABLE_HEADER_SEPARATOR_CHAR: char = '━';  // heavy
const TABLE_BODY_SEPARATOR_CHAR: char = '─';    // light
```

#### Code-Block-Rendering

Code-Blöcke werden in `code_block_buffer: String` gepuffert und erst bei
`end_codeblock()` batch-highlighted via `highlight_code_to_lines(code, lang)`.
Kein span-by-span Rendering während des Parsens.

### Übertragung auf harw-tui

- `render_markdown_lines_with_width_and_cwd()` ist der einzige Aufruf den harw-tui braucht.
  Die Rückgabe `Vec<HyperlinkLine>` kann direkt in `Paragraph::new(...)` eingesetzt werden.
- `unwrap_markdown_fences()` vor dem Render aufrufen wenn LLM-Output verarbeitet wird.
- Tabellen brauchen `width: usize` — das Terminal-Breite aus `area.width as usize` übergeben.
- Für die ersten Iterationen kann die Tabellen-Pipeline übersprungen werden: einfach
  `append_markdown()` verwenden und den key/value-Fallback akzeptieren.

---

## 3. HistoryCell-Typsystem: Typisierte Ausgabe-Zellen

### Das `HistoryCell`-Trait — `history_cell/mod.rs`

```rust
pub(crate) trait HistoryCell: std::fmt::Debug + Send + Sync + Any {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>>;
    fn raw_lines(&self) -> Vec<Line<'static>>;
    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine>;
    fn desired_height(&self, width: u16) -> u16;
    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>>;
    fn transcript_animation_tick(&self) -> Option<u64>;
}
```

`HistoryRenderMode`: `Rich` | `Raw` — schaltet zwischen styled und unstyled Ausgabe.

`impl Renderable for Box<dyn HistoryCell>`: ruft zuerst `Clear.render(area, buf)` auf
(verhindert Stale-Glyphen bei Resize), dann
`Paragraph::new(...).scroll((overflow_y, 0)).render(area, buf)`.

### Konkrete Implementierungen — `history_cell/base.rs` und `history_cell/messages.rs`

| Typ | Datei | Besonderheit |
|---|---|---|
| `PlainHistoryCell { lines: Vec<Line<'static>> }` | base.rs | Einfachste Zelle |
| `WebHyperlinkHistoryCell` | base.rs | Überschreibt `display_hyperlink_lines()` mit URL-Annotierung |
| `PrefixedWrappedHistoryCell { text, initial_prefix, subsequent_prefix }` | base.rs | `adaptive_wrap_lines()` mit Prefix-Indents |
| `CompositeHistoryCell { parts: Vec<Box<dyn HistoryCell>> }` | base.rs | Verbindet Zellen mit Leerzeile |
| `UserHistoryCell` | messages.rs | Sanitiert CSI-Sequenzen, Prefix `"› ".bold().dim()` |
| `AgentMessageCell { lines, is_first_line }` | messages.rs | Prefix `"• ".dim()` auf erster Zeile |
| `AgentMarkdownCell { markdown_source, cwd }` | messages.rs | Re-rendert bei jeder `display_lines(width)`-Anfrage |
| `StreamingAgentTailCell { lines, is_first_line }` | messages.rs | Pre-gerendert, kein Re-Wrap (live stream) |
| `ReasoningSummaryCell` | messages.rs | `dim().italic()`, Prefix `"• ".dim()` |
| `StatusHistoryCell` | status/card.rs | `/status` Output mit FieldFormatter und Fortschrittsbalken |

### `StatusHistoryCell` — `status/card.rs`

`StatusHistoryCell.display_lines(width)` erzeugt eine komplette `/status`-Ausgabe mit:
- `FieldFormatter` (dynamische Label-Breite aus allen Labels berechnet)
- Token-Usage als `"1.2k total (0.9k input + 0.3k output)"` via `format_tokens_compact()`
- Rate-Limit-Progressbalken via `render_status_limit_progress_bar(percent_remaining)`
- Rahmen via `with_border_with_inner_width()` (history_cell/mod.rs)
- `Arc<RwLock<StatusRateLimitState>>` für Live-Refresh ohne Zell-Neuerstellen

`StatusHistoryHandle` (`status/card.rs:80`) ist ein billig klonbarer Handle auf den internen
`Arc<RwLock<...>>`. Der Controller hält den Handle und ruft
`finish_rate_limit_refresh(snapshots, now)` auf wenn neue Limit-Daten ankommen.

### Übertragung auf harw-tui

- `Box<dyn HistoryCell>` als Eintrags-Typ für `Vec<Box<dyn HistoryCell>>` in `app.rs` einführen.
- `CompositeHistoryCell` für den Aufbau von mehrteiligen Ausgaben (z.B. Command + Response).
- `AgentMarkdownCell { source, cwd }` als finalisierte Ausgabe-Zelle — nie `Vec<Line>` speichern.
- Für Tool-Output: `PrefixedWrappedHistoryCell` mit passendem `initial_prefix` verwenden.
- `desired_height(width)` vor dem Layout-Calc aufrufen, um Scroll-Offsets korrekt zu berechnen.

---

## 4. Render-Helpers: Komposites Layout-System

### `Renderable`-Trait — `render/renderable.rs`

```rust
pub(crate) trait Renderable {
    fn render(&self, area: Rect, buf: &mut Buffer);
    fn desired_height(&self, width: u16) -> u16;
    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)>;
    fn cursor_style(&self, area: Rect) -> SetCursorStyle;
}
```

`RenderableItem<'a>`: `Owned(Box<dyn Renderable>)` | `Borrowed(&dyn Renderable)` —
ermöglicht sowohl owned als auch borrowed Composites ohne Extra-Allocation.

### Layout-Composites (render/renderable.rs)

| Struct | Semantik |
|---|---|
| `ColumnRenderable` | Stapelt Kinder vertikal, jedes auf `desired_height()` |
| `FlexRenderable` | Flutter-artiges Flex-Layout mit Flex-Faktoren (vertikal) |
| `RowRenderable` | Stapelt Kinder horizontal mit fixen Breiten |
| `InsetRenderable` | Wendet `Insets` an bevor delegiert wird |

`Insets { left, top, right, bottom: u16 }` — Konstruktoren:
```rust
Insets::tlbr(top, left, bottom, right)  // alle vier
Insets::vh(vertical, horizontal)         // symmetrisch
```

`RectExt`-Trait (`render/mod.rs`): `fn inset(&self, insets: Insets) -> Rect` —
verkleinert ein `Rect` um die Ränder.

### Übertragung auf harw-tui

- `ColumnRenderable` sofort nutzbar: Chat-History-Items stapeln ohne manuelles `Layout::default()`.
- `InsetRenderable` für die `LIVE_PREFIX_COLS: u16 = 2` Insets links in der Chat-Ansicht.
- `FlexRenderable` für den flexiblen Split zwischen History-Scroll-Area und Composer.
- Eigenen `impl Renderable` für harw-tui Widgets schreiben; damit sind alle Layout-Primitiven
  direkt anwendbar.

---

## 5. Syntax-Highlighting: syntect + catppuccin

### Globals — `render/highlight.rs`

```rust
static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME: OnceLock<RwLock<Theme>> = OnceLock::new();
static THEME_OVERRIDE: OnceLock<Option<String>> = OnceLock::new();
static CODEX_HOME: OnceLock<Option<PathBuf>> = OnceLock::new();
```

Alle vier `OnceLock`-Globals werden in `set_theme_override(name, codex_home)` lazy
initialisiert (beim ersten Aufruf).

**Adaptive Theme-Wahl** (render/highlight.rs):
- Kein Override → `terminal_palette::default_bg()` wird gefragt.
- `is_light(rgb)` → `catppuccin-latte` (hell).
- dark/unbekannt → `catppuccin-mocha` (dunkel).
- 32 Themes über `two_face`-Crate gebündelt; `.tmTheme`-Dateien aus `CODEX_HOME` per Custom Load.

### `highlight_code_to_lines()` — `render/highlight.rs`

```rust
pub(crate) fn highlight_code_to_lines(code: &str, lang: &str) -> Vec<Line<'static>>
```

- Fallback auf Plain-Text wenn Sprache unbekannt.
- Abbruch-Schwellen: `> 512KB` oder `> 10000 Zeilen` → Plain-Text.
- Alpha-Channel-Encoding der syntect-Farben:
  - `a = 0x00` → ANSI-Palette-Index (16 Farben)
  - `a = 0x01` → Terminal-Default-Foreground
  - `a = 0xFF` → RGB-Farbe
- **Intentional**: `Modifier::ITALIC` und `Modifier::UNDERLINE` werden unterdrückt
  (zu unruhig im Terminal).

```rust
pub(crate) fn highlight_bash_to_lines(script: &str) -> Vec<Line<'static>>
// Wrapper für "bash"-Sprache
```

`foreground_style_for_scopes(scope_names: &[&str]) -> Option<Style>` — Abfrage des
aktiven Themes für TextMate-Scope-Farben. Wird für kontextabhängige Highlights genutzt.

### Übertragung auf harw-tui

- `set_theme_override(None, codex_home)` einmal beim Start aufrufen → automatische
  Light/Dark-Erkennung.
- `highlight_code_to_lines(code, lang)` gibt `Vec<Line<'static>>` zurück — direkt in
  `Paragraph::new(Text::from(lines))` einsetzbar.
- Die 512KB/10000-Zeilen-Guards sind wichtig: harw-tui sollte dieselben Grenzen anwenden.
- `syntect` + `two_face` als Cargo-Abhängigkeiten: `cargo add syntect two_face`.

---

## 6. Style/Color-System: Adaptive Palette

### Drei-Stufen-Farbsystem

```
1. Terminal-Background-Probe
   terminal_palette::default_bg() -> Option<(u8,u8,u8)>
         │
         ▼
2. Luminanz-Erkennung
   color::is_light(rgb) -> bool
   Y = 0.299r + 0.587g + 0.114b > 128.0
         │
         ▼
3. Blend-basierte Style-Berechnung
   color::blend(fg, bg, alpha: f32) -> (u8,u8,u8)
   terminal_palette::best_color(rgb) -> Color  // Ansi256-Fallback
```

### `color.rs` — Kernfunktionen

```rust
// Luminanz-basierte Hell/Dunkel-Erkennung
pub fn is_light(bg: (u8, u8, u8)) -> bool {
    let (r, g, b) = bg;
    let y = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
    y > 128.0
}

// Lineares Alpha-Compositing
pub fn blend(fg: (u8,u8,u8), bg: (u8,u8,u8), alpha: f32) -> (u8,u8,u8)

// CIE76 sRGB→XYZ→Lab Euklidische Distanz (für best_color-Suche)
pub fn perceptual_distance(a: (u8,u8,u8), b: (u8,u8,u8)) -> f32
```

### `style.rs` — Adaptive Styles

```rust
// User-Message: subtiles Hintergrund-Overlay
// Hell: 4% schwarzes Alpha; Dunkel: 12% weißes Alpha
pub fn user_message_style() -> Style  // ruft default_bg() intern

// Tabellen-Separator: 20% Alpha fg→bg
// TrueColor: Rgb(...), Ansi256: best_color(), Ansi16/Unknown: .dim()
pub(crate) fn table_separator_style() -> Style

// Akzentfarbe
// Light BG: (0,95,135) bold; Dark BG: Color::Cyan bold
pub(crate) fn accent_style() -> Style
```

`StdoutColorLevel`-Enum aus `terminal_palette`: `TrueColor` | `Ansi256` | `Ansi16` | `Unknown` —
drei-stufiger Fallback für alle Style-Berechnungen.

### Übertragung auf harw-tui

- `terminal_palette::default_bg()` + `is_light()` einmal beim Start aufrufen, Ergebnis cachen.
- `blend(fg, bg, alpha)` für User-Message-Hintergründe und Separator-Farben verwenden.
- `best_color(rgb)` für Ansi256-Fallback wenn kein TrueColor vorhanden.
- `accent_style()` ist drop-in für alle "selected/active" UI-Elemente.
- **Wichtig**: Niemals harte RGB-Werte in Widgets schreiben — immer über `best_color()` leiten
  damit die TUI auf Ansi16-Terminals nicht bricht.
- `cargo add supports-color terminal_palette` (oder äquivalent aus codex-rs).

---

## 7. Motion & Animations: Shimmer und Activity-Indicators

### `MotionMode` — `motion.rs`

```rust
pub(crate) enum MotionMode { Animated, Reduced }
pub(crate) enum ReducedMotionIndicator { Hidden, StaticBullet }
```

`activity_indicator(start_time, mode, indicator) -> Option<Span<'static>>`:

| Kombination | Ausgabe |
|---|---|
| `Animated` + TrueColor | `shimmer_spans("•")` — RGB-Shimmer |
| `Animated` + kein TrueColor | Blinken zwischen `"•"` und `"◦".dim()` alle 600ms |
| `Reduced + Hidden` | `None` |
| `Reduced + StaticBullet` | `Some("•".dim())` |

### `shimmer_spans()` — `shimmer.rs:21`

```rust
pub(crate) fn shimmer_spans(text: &str) -> Vec<Span<'static>>
```

Algorithmus:
1. `PROCESS_START: OnceLock<Instant>` — einmaliger Referenz-Zeitpunkt.
2. `pos_f = (elapsed % 2s) / 2s * (len + 2*padding)` — Sweep-Position.
3. Für jedes Zeichen: `dist = |char_pos - pos|`.
4. Wenn `dist <= band_half_width=5.0`: `t = 0.5 * (1 + cos(π * dist/5))` — Kosinus-Glättung.
5. TrueColor: `blend(default_bg, default_fg, t * 0.9)` — Highlight fades fg→bg.
6. Kein TrueColor: `dim` / normal / `bold` nach Intensitäts-Schwellen.

Jeder Charakter bekommt seinen eigenen `Span` mit eigenem `Style`. Ein `"•"` wird zu einem
einzigen Span; `"thinking..."` zu 11 Spans.

**Architektur-Regel**: Nur `motion.rs` und `shimmer.rs` dürfen `shimmer_spans()` direkt aufrufen
(durch Test enforced).

### Übertragung auf harw-tui

- `MotionMode` + `ReducedMotionIndicator` als Konfigurationsoptionen in `app.rs` einführen.
- `activity_indicator()` für den "Agent denkt"-Indikator während LLM-Streaming verwenden.
- Shimmer-Effekt braucht nur `shimmer_spans("•")` → `Vec<Span>` → in eine `Line` einbauen.
- `PROCESS_START: OnceLock<Instant>` im App-Modul einmalig beim Start initialisieren.
- Fallback-Logik: `supports_color::on_cached(Stream::Stdout).map(|l| l.has_16m)` für
  TrueColor-Erkennung.

---

## 8. Footer / StatusBar: Zustandsmaschine der Fußzeile

### `FooterMode` — `bottom_pane/footer.rs`

```rust
pub(crate) enum FooterMode {
    HistorySearch,
    QuitShortcutReminder,
    ShortcutOverlay,
    EscHint,
    ComposerEmpty,
    ComposerHasDraft,
}
```

### `FooterProps` — `bottom_pane/footer.rs`

Alle Rendering-Inputs zusammengefasst:
- `mode: FooterMode`
- `status_line_value: Option<String>` — rechte Seite, z.B. Token-Zahl
- `active_agent_label: Option<String>` — welcher Agent aktiv ist
- `key_hints: FooterKeyHints` — struct mit `Option<KeyBinding>` pro Aktion

### Layout-Kaskade: `single_line_footer_layout()`

Breiten-basiertes Progressive-Enhancement:
1. Versucht: Full hint + context zusammen.
2. Wenn zu breit: Hint allein.
3. Wenn immer noch zu breit: Kürzt Hint.
4. Immer: `FOOTER_INDENT_COLS: usize = 2` (= `LIVE_PREFIX_COLS`) linkes Padding.

`render_footer_line(area, buf, line)` — rendert mit Indent.
`render_context_right(area, buf, line)` — rechts-ausgerichtet.

### Status-Line-Indikatoren

```rust
GoalStatusIndicator.styled_span()   // magenta
CollaborationModeIndicator.styled_span()  // cyan
// passiver Status wenn kein aktiver Befehl
passive_footer_status_line(props) -> Option<Line<'static>>
```

### Übertragung auf harw-tui

- `FooterMode`-Enum für harw-tui Status-Maschine: `Idle` | `Streaming` | `AwaitingInput` | `Error`.
- `single_line_footer_layout()`-Pattern: Breite messen, Progressive-Degradation des Inhalts.
- `FOOTER_INDENT_COLS = 2` für konsistentes linkes Padding (matcht `LIVE_PREFIX_COLS`).
- `passive_footer_status_line()` als Vorbild für kontextsensitive Statuszeile die verschwindet
  wenn ein anderer Footer-Modus aktiv ist.

---

## 9. Live-Wrap und Wrapping-Infrastruktur

### `RowBuilder` — `live_wrap.rs`

Für den Live-Composer (Input-Area) während des Tippens:

```rust
pub(crate) struct RowBuilder {
    target_width: usize,
    current_line: String,
    rows: Vec<Row>,
}
```

- `push_fragment(fragment: &str)`: Behandelt `\n`, wrappt durch Unicode-Display-Breite.
- `set_width(width)`: Re-wrappt alle vorhandenen Inhalte (für Resize).
- `drain_commit_ready(max_keep) -> Vec<Row>`: Drainiert älteste committed Rows.
- `display_rows() -> Vec<Row>`: Inklusive aktueller partieller Zeile.
- `take_prefix_by_width(text, max_cols) -> (String, &str, usize)`: Unicode-korrektes
  Column-Counting (nicht Byte-Counting!).

### `RtOptions<'a>` — `wrapping.rs`

Builder für `textwrap::Options` mit Ratatui-`Line`-Indents:

```rust
pub(crate) struct RtOptions<'a> {
    width: usize,
    initial_indent: Line<'a>,
    subsequent_indent: Line<'a>,
    break_words: bool,
    wrap_algorithm: textwrap::WrapAlgorithm,
    word_separator: textwrap::WordSeparator,
    word_splitter: textwrap::WordSplitter,
}
```

Konstruktor: `RtOptions::new(width)`.

### `adaptive_wrap_line()` — `wrapping.rs`

URL-aware Wrapping:
```rust
pub(crate) fn adaptive_wrap_line<'a>(line: Line<'a>, opts: RtOptions<'a>) -> Vec<Line<'a>>
```

Logik:
1. `text_contains_url_like(text)` — prüft auf `scheme://host`, Bare-Domain+Path,
   `localhost:port/path`, IPv4+Path.
2. URL + Prosa gemischt → `mixed_url_wrap_line()`.
3. Nur URLs → `url_preserving_wrap_options()`: `AsciiSpace`-Separator,
   `NoHyphenation`, `break_words=false`.
4. Kein URL → normales `word_wrap_line()`.

`adaptive_wrap_lines<I, L>()` — Multi-Line Einstiegspunkt; `initial_indent` nur auf erster Zeile.

### `line_truncation.rs`

```rust
pub(crate) fn line_width(line: &Line) -> usize
pub(crate) fn truncate_line_to_width(line: Line<'static>, max_width: usize) -> Line<'static>
pub(crate) fn truncate_line_with_ellipsis_if_overflow(
    line: Line<'static>, max_width: usize
) -> Line<'static>
```

- Fast path: wenn `line_width(line) <= max_width` → unverändert zurück.
- Overflow: `"…"` mit dem Style des letzten Spans anhängen.
- Unicode-korrekt (nicht Byte-Index).

### Übertragung auf harw-tui

- `RowBuilder` für den Input-Composer in harw-tui: `push_fragment()` bei jedem Keystroke,
  `set_width()` bei Resize-Events, `display_rows()` für die Render-Schleife.
- `adaptive_wrap_line()` für alle Chat-Messages die URLs enthalten könnten.
- `truncate_line_with_ellipsis_if_overflow()` für Footer-Inhalte und Status-Labels.
- `RtOptions::new(width).initial_indent(prefix_line)` für Nachrichten mit Prefix-Bullet.

---

## 10. Status-Output: `/status`-Karte als `HistoryCell`

### `StatusHistoryCell` — `status/card.rs`

Implementiert `HistoryCell` vollständig. Key-Aspekte:

**`FieldFormatter`** (status/format.rs): berechnet aus allen Labels die maximale Label-Breite,
sodass Werte alle bündig ausgerichtet sind. Pattern:
```rust
let formatter = FieldFormatter::from_labels(labels.iter().map(String::as_str));
formatter.line("Model", vec![Span::from(model_name)])
formatter.continuation(vec![Span::from(detail).dim()])
```

**Rate-Limit-Progressbar**: `render_status_limit_progress_bar(percent_remaining)` gibt
einen Unicode-Balken zurück. Adaptive Darstellung:
- Wenn `full_value_spans` (Balken + Prozent + Reset-Zeit) in `value_width` passen → alles.
- Sonst: nur Prozent-Zahl.

**Live-Refresh ohne Neuerstellen**: `StatusHistoryHandle` ist ein
`Arc<RwLock<StatusRateLimitState>>`-Wrapper. `finish_rate_limit_refresh()` schreibt atomisch
neue Daten; die Zelle rendert beim nächsten Frame die neuen Daten ohne neu erstellt zu werden.

**Rahmen**: `with_border_with_inner_width(lines, inner_width)` wickelt alle Lines in einen
Unicode-Box-Drawing-Rahmen. Der `inner_width`-Wert wird aus dem Maximum der tatsächlichen
Line-Breiten berechnet (nicht aus `area.width`) — der Rahmen ist so schmal wie nötig.

### `format_tokens_compact()` — `status/helpers.rs`

```rust
// 1200 → "1.2k", 1_500_000 → "1.5M"
pub(crate) fn format_tokens_compact(tokens: i64) -> String
```

### Übertragung auf harw-tui

- `StatusHistoryCell`-Pattern für harw-tui `/info` oder `/status`-Befehl: als
  `Box<dyn HistoryCell>` in die History einreihen.
- `FieldFormatter`-Pattern für jede Key-Value-Ausgabe: einmal aus allen Labels initialisieren,
  dann `formatter.line(key, spans)` aufrufen.
- `Arc<RwLock<T>>`-Handle-Pattern für Live-Updates in bestehenden History-Einträgen
  (z.B. Token-Counter der sich während des Streams aktualisiert).
- `with_border_with_inner_width()` für gerahmte Box-Ausgaben.

---

## Gesamtbild: Was macht codex-rs TUI schön?

1. **Re-Render-Strategie**: Finalisierte Outputs speichern rohe Quelle, nicht gerenderte Lines.
   Resize triggert automatisch korrektes Re-Render.
2. **Newline-Gate**: Streaming-Deltas werden erst nach vollständiger Zeile (`\n`) gerendert.
   Keine Partial-Renders.
3. **Adaptive Palette**: Terminal-Background → luminance → blend → best_color. Niemals
   hardkodierte Farben.
4. **Composable Layout**: `Renderable`-Trait + `ColumnRenderable`/`FlexRenderable` ermöglichen
   deklaratives Layout ohne manuelle `Rect`-Arithmetik.
5. **Progressive Footer**: Width-basierte Kaskade — niemals Overflow, immer lesbar.
6. **Unicode-First**: Alle Width-Berechnungen über `UnicodeWidthStr`, nie Byte-Length.
7. **Shimmer als TrueColor-Feature**: Fällt graceful auf dim/bold ohne RGB zurück.
8. **Type-Safe History**: `Box<dyn HistoryCell>` statt `Vec<String>` — jede Zelle kennt
   ihre eigene Render-Logik.
