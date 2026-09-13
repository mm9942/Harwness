#![allow(dead_code)] // pub API-Fläche; volle Nutzung folgt in späteren Waves
//! Autarker In-Memory-Editor für die Chat-Eingabezeile.
//!
//! # Verantwortung
//! Dieses Modul besitzt den gesamten Editorzustand: Pufferverwaltung, Cursor-Navigation,
//! History-Stack und die Umwandlung von Terminal-Key-Events in Aktionen. Rendering liegt
//! beim Aufrufer (`app.rs`), der `visible_lines()` und `cursor_position()` abfragt.
//!
//! # Exportierte Typen
//! - [`InputEditor`] — State-Struct des Editors.
//! - [`InputAction`] — Rückgabetyp von `handle_key`, beschreibt was der Aufrufer tun soll.
//!
//! # Nebenläufigkeitsmodell
//! `InputEditor` ist **nicht** `Send`/`Sync`-gebunden. Der Aufrufer serialisiert jeden
//! Zugriff (z. B. innerhalb der TUI-Event-Loop). Kein Mutex, kein Arc erforderlich.
//!
//! # Fehler
//! Dieses Modul produziert keine eigenen Fehlertypen. Ungültige Cursor-Positionen
//! werden intern geklemmt und führen nie zu Panics.
//!
//! # Beispiel
//! ```ignore
//! use harw_tui::input_editor::{InputEditor, InputAction};
//! use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
//!
//! let mut ed = InputEditor::new();
//! ed.insert_str("Hallo Welt");
//! let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
//! assert!(matches!(ed.handle_key(key), InputAction::Submit(_)));
//! ```

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

// ---------------------------------------------------------------------------
// Paste-Burst-Erkennung (P0.10 / Register G-051)
// ---------------------------------------------------------------------------
//
// Ohne funktionierendes Bracketed Paste (`Event::Paste`, siehe `tui_event.rs`/
// `app.rs`, außerhalb dieses Moduls) liefert crossterm einen eingefügten Text
// stattdessen als schnellen Strom einzelner `KeyCode::Char`/`KeyCode::Enter`-
// Ereignisse. Ohne Gegenmaßnahme träfe jedes eingefügte `\n` auf den normalen
// `KeyCode::Enter`-Zweig in `handle_key` und würde den bis dahin aufgelaufenen
// Puffer sofort absenden — beginnt dieser Teilpuffer mit `/` oder `!`, würde er
// über `classify_input` (`input.rs:17-33`) automatisch als Slash-/Shell-Befehl
// ausgeführt, ohne dass der Nutzer je bewusst Enter für genau diese Zeile
// gedrückt hat.
//
// `PasteBurstDetector` ist eine reine Zustandsmaschine ohne Bezug zum
// Editor-Puffer: sie bekommt für jedes Zeichen-/Enter-Ereignis einen
// Zeitstempel übergeben (`observe`) und entscheidet rein aus den zeitlichen
// Abständen, ob gerade ein Burst läuft. Die Zeitquelle ist damit injizierbar
// (kein `Instant::now()` im internen Zustand) — Tests können deterministische
// `Instant`-Werte vorgeben.

/// Maximaler Abstand zwischen zwei Tastaturereignissen, damit sie noch als
/// Teil desselben Paste-Bursts gelten.
const PASTE_BURST_INTERVAL: Duration = Duration::from_millis(8);

/// Mindestanzahl aufeinanderfolgender schneller Ereignisse (Zeichen und Enter
/// zählen gleichermaßen), ab der ein Burst als aktiv gilt.
const PASTE_BURST_MIN_EVENTS: u32 = 3;

/// Erkennt Paste-Bursts anhand der zeitlichen Abstände zwischen
/// Zeichen-/Enter-Tastaturereignissen.
///
/// # Beschreibung
/// Reine Zustandsmaschine ohne Seiteneffekte auf den Editor-Puffer. Für jedes
/// Zeichen- oder Enter-Ereignis wird [`observe`](Self::observe) mit einem
/// Zeitstempel aufgerufen. Liegen mindestens [`PASTE_BURST_MIN_EVENTS`]
/// aufeinanderfolgende Ereignisse mit einem Abstand unter
/// [`PASTE_BURST_INTERVAL`] vor, gilt ein Burst als aktiv. Ein einzelnes
/// Ereignis mit größerem Abstand (Ruhephase) beendet einen laufenden Burst
/// sofort wieder — ein danach folgendes, bewusst gedrücktes Enter wird also
/// wieder normal behandelt.
///
/// # Nebenläufigkeit
/// Nicht `Send`/`Sync` erforderlich; wird ausschließlich vom [`InputEditor`]
/// auf demselben Thread genutzt.
#[derive(Debug, Clone, Default)]
struct PasteBurstDetector {
    /// Zeitstempel des zuletzt beobachteten Ereignisses.
    last_event_at: Option<Instant>,
    /// Anzahl aufeinanderfolgender schneller Ereignisse (Abstand < Schwellwert).
    fast_streak: u32,
    /// Ob aktuell ein Burst als aktiv gilt.
    active: bool,
}

impl PasteBurstDetector {
    /// Meldet ein Zeichen- oder Enter-Ereignis zum Zeitpunkt `now`.
    ///
    /// # Argumente
    /// - `now` (`Instant`): Zeitstempel des Ereignisses (injizierbar für Tests).
    ///
    /// # Returns
    /// `true`, wenn ab diesem Ereignis (einschließlich) ein Paste-Burst als
    /// aktiv gilt.
    fn observe(&mut self, now: Instant) -> bool {
        // `Instant::duration_since` sättigt seit Rust 1.60 bei `now < prev`
        // auf `Duration::ZERO`, statt zu paniken — hier unkritisch, da ein
        // (theoretisch) rückwärtslaufender Zeitstempel dann als "schnell"
        // gilt, was höchstens einen Burst zu früh erkennt, nie zu spät.
        let is_fast = self
            .last_event_at
            .is_some_and(|prev| now.duration_since(prev) < PASTE_BURST_INTERVAL);
        self.last_event_at = Some(now);
        if is_fast {
            self.fast_streak = self.fast_streak.saturating_add(1);
        } else {
            // Ruhephase (oder erstes Ereignis überhaupt) beendet einen
            // laufenden Burst sofort.
            self.fast_streak = 1;
            self.active = false;
        }
        if self.fast_streak >= PASTE_BURST_MIN_EVENTS {
            self.active = true;
        }
        self.active
    }
}

// ---------------------------------------------------------------------------
// Öffentliche Typen
// ---------------------------------------------------------------------------

/// Puffer, Cursor und History-Stack für die Chat-Eingabezeile.
///
/// # Beschreibung
/// `InputEditor` ist ein reines State-Struct ohne Rendering-Logik. Der Aufrufer
/// übergibt Key-Events via [`handle_key`](InputEditor::handle_key) und rendert
/// die Ausgabe mit [`visible_lines`](InputEditor::visible_lines) und
/// [`cursor_position`](InputEditor::cursor_position).
///
/// # Nebenläufigkeit
/// Nicht `Send`/`Sync`. Zugriff ist durch die TUI-Event-Loop serialisiert.
#[derive(Debug, Clone)]
pub struct InputEditor {
    /// Aktueller Textinhalt (UTF-8, kann `\n` enthalten für Multi-Line).
    buffer: String,
    /// Cursor-Position als Byte-Offset in `buffer`. Immer an einer char-Grenze.
    cursor: usize,
    /// Vergangene abgeschickte Prompts, jüngster hinten.
    history: Vec<String>,
    /// Aktueller History-Browse-Index. `None` = kein Browsen aktiv.
    history_pos: Option<usize>,
    /// Snapshot des Buffers vor Beginn des History-Browsings (zum Wiederherstellen bei Escape).
    history_snapshot: Option<String>,
    /// Der letzte per History-Recall geladene Text (für Boundary-Gate).
    last_recalled: Option<String>,
    /// Max History-Länge (Ring-Buffer).
    history_cap: usize,
    /// Zustandsmaschine zur Paste-Burst-Erkennung (siehe [`PasteBurstDetector`],
    /// P0.10 / Register G-051).
    paste_burst: PasteBurstDetector,
}

/// Rückgabe von [`InputEditor::handle_key`]: was der Aufrufer tun soll.
///
/// # Varianten
/// - [`Redraw`](InputAction::Redraw) — Key konsumiert, UI neu zeichnen.
/// - [`Submit`](InputAction::Submit) — Buffer fertiggestellt, enthält den Text.
/// - [`Passthrough`](InputAction::Passthrough) — Key gehört nicht zum Editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    /// Der Key wurde konsumiert; UI muss neu gezeichnet werden.
    Redraw,
    /// Der Buffer wurde per Enter fertiggestellt. Enthält den bereinigten Text.
    Submit(String),
    /// Der Key gehört nicht zum Editor — der Aufrufer entscheidet weiter.
    Passthrough,
}

// ---------------------------------------------------------------------------
// Implementierung
// ---------------------------------------------------------------------------

impl InputEditor {
    /// Erstellt einen neuen, leeren Editor mit Standard-History-Kapazität (100).
    ///
    /// # Returns
    /// Ein frisch initialisierter [`InputEditor`].
    ///
    /// # Beispiel
    /// ```ignore
    /// use harw_tui::input_editor::InputEditor;
    /// let ed = InputEditor::new();
    /// assert!(ed.is_empty());
    /// ```
    pub fn new() -> Self {
        Self::with_capacity(100)
    }

    /// Erstellt einen neuen Editor mit angegebener History-Kapazität.
    ///
    /// # Argumente
    /// - `history_cap` (`usize`): Maximale Anzahl gespeicherter History-Einträge.
    ///
    /// # Returns
    /// Ein frisch initialisierter [`InputEditor`] mit der gewünschten Kapazität.
    ///
    /// # Beispiel
    /// ```ignore
    /// use harw_tui::input_editor::InputEditor;
    /// let ed = InputEditor::with_capacity(50);
    /// assert!(ed.is_empty());
    /// ```
    pub fn with_capacity(history_cap: usize) -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_pos: None,
            history_snapshot: None,
            last_recalled: None,
            history_cap,
            paste_burst: PasteBurstDetector::default(),
        }
    }

    // -----------------------------------------------------------------------
    // Introspektion
    // -----------------------------------------------------------------------

    /// Gibt den aktuellen Textinhalt des Puffers zurück.
    ///
    /// # Returns
    /// Borrowed `&str` des internen Puffers (beinhaltet ggf. `\n`-Zeichen).
    pub fn text(&self) -> &str {
        &self.buffer
    }

    /// Gibt die aktuelle Cursor-Position als Byte-Offset zurück.
    ///
    /// # Returns
    /// Byte-Offset im UTF-8-Puffer. Immer an einer Char-Grenze.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Gibt zurück, ob der Puffer leer ist.
    ///
    /// # Returns
    /// `true` wenn der Puffer keinen Text enthält.
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Gibt die gespeicherten History-Einträge zurück (älteste vorne, jüngste hinten).
    ///
    /// # Returns
    /// Slice der History-Einträge.
    pub fn history(&self) -> &[String] {
        &self.history
    }

    // -----------------------------------------------------------------------
    // Manipulation
    // -----------------------------------------------------------------------

    /// Fügt ein einzelnes Zeichen an der aktuellen Cursor-Position ein und bewegt den Cursor.
    ///
    /// # Argumente
    /// - `ch` (`char`): Das einzufügende Zeichen.
    ///
    /// # Nebenläufigkeit
    /// Nicht thread-safe; Aufrufer serialisiert Zugriff.
    pub fn insert_char(&mut self, ch: char) {
        self.buffer.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
        debug_assert!(self.buffer.is_char_boundary(self.cursor));
    }

    /// Fügt einen String an der aktuellen Cursor-Position ein und bewegt den Cursor.
    ///
    /// # Argumente
    /// - `s` (`&str`): Der einzufügende Text.
    pub fn insert_str(&mut self, s: &str) {
        self.buffer.insert_str(self.cursor, s);
        self.cursor += s.len();
        debug_assert!(self.buffer.is_char_boundary(self.cursor));
    }

    /// Fügt einen Zeilenumbruch (`\n`) an der aktuellen Cursor-Position ein.
    pub fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    /// Löscht das Zeichen links vom Cursor (Backspace-Semantik).
    ///
    /// Ist der Cursor am Anfang, ist die Operation ein No-Op.
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        // Schritt zurück zur vorherigen Char-Grenze
        let prev = self.prev_grapheme_boundary(self.cursor);
        self.buffer.drain(prev..self.cursor);
        self.cursor = prev;
        debug_assert!(self.buffer.is_char_boundary(self.cursor));
    }

    /// Löscht das Zeichen rechts vom Cursor (Vorwärts-Delete).
    ///
    /// Ist der Cursor am Ende, ist die Operation ein No-Op.
    pub fn delete(&mut self) {
        if self.cursor >= self.buffer.len() {
            return;
        }
        let next = self.next_grapheme_boundary(self.cursor);
        self.buffer.drain(self.cursor..next);
        debug_assert!(self.buffer.is_char_boundary(self.cursor));
    }

    // -----------------------------------------------------------------------
    // Cursor-Movement
    // -----------------------------------------------------------------------

    /// Bewegt den Cursor einen Char nach links. No-Op am Puffer-Anfang.
    pub fn move_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.cursor = self.prev_grapheme_boundary(self.cursor);
        debug_assert!(self.buffer.is_char_boundary(self.cursor));
    }

    /// Bewegt den Cursor einen Char nach rechts. No-Op am Puffer-Ende.
    pub fn move_right(&mut self) {
        if self.cursor >= self.buffer.len() {
            return;
        }
        self.cursor = self.next_grapheme_boundary(self.cursor);
        debug_assert!(self.buffer.is_char_boundary(self.cursor));
    }

    /// Bewegt den Cursor eine Zeile nach oben (Multi-Line) oder triggert History-Back
    /// (Single-Line). Gibt `true` zurück, wenn eine History-Navigation stattfand.
    ///
    /// # Returns
    /// `true` wenn History navigiert wurde, `false` bei Cursor-Bewegung.
    pub fn move_up(&mut self) -> bool {
        if self.is_multiline() {
            self.move_cursor_line(-1);
            false
        } else {
            self.history_back()
        }
    }

    /// Bewegt den Cursor eine Zeile nach unten (Multi-Line) oder triggert History-Forward
    /// (Single-Line). Gibt `true` zurück, wenn eine History-Navigation stattfand.
    ///
    /// # Returns
    /// `true` wenn History navigiert wurde, `false` bei Cursor-Bewegung.
    pub fn move_down(&mut self) -> bool {
        if self.is_multiline() {
            self.move_cursor_line(1);
            false
        } else {
            self.history_forward()
        }
    }

    /// Bewegt den Cursor zum Anfang der aktuellen Zeile.
    pub fn move_home(&mut self) {
        let line_start = self.current_line_start();
        self.cursor = line_start;
        debug_assert!(self.buffer.is_char_boundary(self.cursor));
    }

    /// Bewegt den Cursor zum Ende der aktuellen Zeile (vor dem `\n`).
    pub fn move_end(&mut self) {
        let line_end = self.current_line_end();
        self.cursor = line_end;
        debug_assert!(self.buffer.is_char_boundary(self.cursor));
    }

    /// Bewegt den Cursor ein Wort nach links (Ctrl+Left).
    ///
    /// Wort-Grenzen: ASCII-Whitespace und nicht-alphanumerische Zeichen.
    /// Algorithmus: zuerst Whitespace rückwärts überspringen, dann Word-Zeichen.
    pub fn move_word_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        // Rückwärts über Whitespace
        while self.cursor > 0 {
            let prev = self.prev_grapheme_boundary(self.cursor);
            let ch = self.buffer[prev..].chars().next().unwrap_or('\0');
            if !ch.is_whitespace() {
                break;
            }
            self.cursor = prev;
        }
        // Rückwärts über Wort-Zeichen
        while self.cursor > 0 {
            let prev = self.prev_grapheme_boundary(self.cursor);
            let ch = self.buffer[prev..].chars().next().unwrap_or('\0');
            if ch.is_whitespace() || (!ch.is_alphanumeric() && ch != '_') {
                break;
            }
            self.cursor = prev;
        }
        debug_assert!(self.buffer.is_char_boundary(self.cursor));
    }

    /// Bewegt den Cursor ein Wort nach rechts (Ctrl+Right).
    ///
    /// Algorithmus: zuerst Word-Zeichen vorwärts überspringen, dann Whitespace.
    pub fn move_word_right(&mut self) {
        let len = self.buffer.len();
        if self.cursor >= len {
            return;
        }
        while self.cursor < len {
            let next = self.next_grapheme_boundary(self.cursor);
            let ch = self.buffer[self.cursor..next]
                .chars()
                .next()
                .unwrap_or('\0');
            if ch.is_whitespace() || (!ch.is_alphanumeric() && ch != '_') {
                break;
            }
            self.cursor = next;
        }
        while self.cursor < len {
            let next = self.next_grapheme_boundary(self.cursor);
            let ch = self.buffer[self.cursor..next]
                .chars()
                .next()
                .unwrap_or('\0');
            if !ch.is_whitespace() {
                break;
            }
            self.cursor = next;
        }
        debug_assert!(self.buffer.is_char_boundary(self.cursor));
    }

    /// Löscht das Wort links vom Cursor (Ctrl+Backspace).
    pub fn delete_word_left(&mut self) {
        let end = self.cursor;
        self.move_word_left();
        let start = self.cursor;
        if start < end {
            self.buffer.drain(start..end);
        }
        debug_assert!(self.buffer.is_char_boundary(self.cursor));
    }

    /// Löscht das Wort rechts vom Cursor (Ctrl+Delete).
    pub fn delete_word_right(&mut self) {
        let start = self.cursor;
        self.move_word_right();
        let end = self.cursor;
        if start < end {
            self.buffer.drain(start..end);
            self.cursor = start;
        }
        debug_assert!(self.buffer.is_char_boundary(self.cursor));
    }

    /// Leert den Puffer und setzt den Cursor auf 0.
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.last_recalled = None;
    }

    // -----------------------------------------------------------------------
    // History
    // -----------------------------------------------------------------------

    /// Fügt einen Prompt zur History hinzu. Identische aufeinanderfolgende Einträge
    /// werden dedupliziert. Bei Überschreitung der Kapazität wird der älteste Eintrag entfernt.
    ///
    /// # Argumente
    /// - `prompt` (`String`): Der zu speichernde Prompt-Text.
    pub fn push_history(&mut self, prompt: String) {
        // Deduplizierung: letzter Eintrag identisch → nicht hinzufügen
        if self.history.last().map(|s| s.as_str()) == Some(prompt.as_str()) {
            return;
        }
        if self.history.len() >= self.history_cap && self.history_cap > 0 {
            self.history.remove(0);
        }
        self.history.push(prompt);
        // History-Browsing-Status zurücksetzen
        self.history_pos = None;
        self.history_snapshot = None;
    }

    /// Navigiert in der History rückwärts (älterer Eintrag). Up-Semantik in Single-Line.
    ///
    /// Beim ersten Aufruf wird der aktuelle Buffer als Snapshot gespeichert.
    ///
    /// # Returns
    /// `true` wenn ein History-Eintrag geladen wurde, `false` wenn keine History vorhanden.
    pub fn history_back(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }

        // Snapshot beim ersten Mal
        if self.history_pos.is_none() {
            self.history_snapshot = Some(self.buffer.clone());
        }

        let new_pos = match self.history_pos {
            None => self.history.len().saturating_sub(1),
            Some(0) => 0, // bereits am ältesten
            Some(pos) => pos - 1,
        };

        self.history_pos = Some(new_pos);
        self.buffer = self.history[new_pos].clone();
        self.cursor = self.buffer.len();
        self.last_recalled = Some(self.buffer.clone());
        true
    }

    /// Navigiert in der History vorwärts (neuerer Eintrag). Down-Semantik in Single-Line.
    ///
    /// Kehrt nach dem jüngsten Eintrag zum Snapshot zurück.
    ///
    /// # Returns
    /// `true` wenn eine Navigation stattfand (auch Rückkehr zum Snapshot), `false` wenn
    /// kein History-Browsing aktiv ist.
    pub fn history_forward(&mut self) -> bool {
        let pos = match self.history_pos {
            None => return false,
            Some(p) => p,
        };

        if pos + 1 >= self.history.len() {
            // Zurück zum Snapshot
            self.buffer = self.history_snapshot.take().unwrap_or_default();
            self.cursor = self.buffer.len();
            self.history_pos = None;
        } else {
            let new_pos = pos + 1;
            self.history_pos = Some(new_pos);
            self.buffer = self.history[new_pos].clone();
            self.cursor = self.buffer.len();
            self.last_recalled = Some(self.buffer.clone());
        }
        true
    }

    /// Bricht das History-Browsing ab und stellt den Buffer-Snapshot wieder her.
    ///
    /// No-Op wenn kein Browsing aktiv ist.
    pub fn cancel_history(&mut self) {
        if let Some(snapshot) = self.history_snapshot.take() {
            self.buffer = snapshot;
            self.cursor = self.buffer.len();
        }
        self.history_pos = None;
        self.last_recalled = None;
    }

    // -----------------------------------------------------------------------
    // Rendering-Support
    // -----------------------------------------------------------------------

    /// Gibt die sichtbaren, gewrappten Zeilen bei gegebener Terminal-Breite zurück.
    ///
    /// # Beschreibung
    /// Behandelt explizite `\n`-Zeichen als harte Zeilenumbrüche. Innerhalb einer
    /// Zeile wird bei `width` Chars weich gebrochen (kein Word-Wrap, Version 1).
    /// Bei `width == 0` wird jede Zeile als eine ungeteilte Zeile zurückgegeben.
    ///
    /// # Argumente
    /// - `width` (`usize`): Terminalbreite in Zeichen (Chars, nicht Bytes).
    ///
    /// # Returns
    /// `Vec<String>` der sichtbaren Zeilen. Mindestens eine Zeile (ggf. leer).
    ///
    /// # Nebenläufigkeit
    /// Nicht thread-safe; Aufrufer serialisiert Zugriff.
    ///
    /// # Beispiel
    /// ```ignore
    /// use harw_tui::input_editor::InputEditor;
    /// let mut ed = InputEditor::new();
    /// ed.insert_str("abcde");
    /// let lines = ed.visible_lines(3);
    /// assert_eq!(lines.len(), 2); // "abc" + "de"
    /// ```
    pub fn visible_lines(&self, width: usize) -> Vec<String> {
        let mut result = Vec::new();
        // Harte Zeilenumbrüche zuerst
        for hard_line in self.buffer.split('\n') {
            if width == 0 {
                result.push(hard_line.to_owned());
                continue;
            }
            // Display-width-aware soft wrapping
            let full_w = hard_line.width();
            if full_w == 0 {
                result.push(String::new());
                continue;
            }
            if full_w <= width {
                result.push(hard_line.to_owned());
                continue;
            }
            // Wrap by display width: walk grapheme clusters
            let mut current = String::new();
            let mut current_w = 0usize;
            for gc in hard_line.graphemes(true) {
                let gc_w = gc.width();
                if current_w + gc_w > width && !current.is_empty() {
                    result.push(std::mem::take(&mut current));
                    current_w = 0;
                }
                current.push_str(gc);
                current_w += gc_w;
            }
            if !current.is_empty() || result.is_empty() {
                result.push(current);
            }
        }
        if result.is_empty() {
            result.push(String::new());
        }
        result
    }

    /// Gibt die `(row, col)` Position des Cursors in `visible_lines(width)`-Koordinaten zurück.
    ///
    /// # Argumente
    /// - `width` (`usize`): Terminalbreite in Chars (muss identisch mit `visible_lines`-Aufruf sein).
    ///
    /// # Returns
    /// `(row, col)` — beide 0-basiert. `row` ist der Zeilenindex in `visible_lines`,
    /// `col` ist die Char-Spalte innerhalb der Zeile.
    ///
    /// # Nebenläufigkeit
    /// Nicht thread-safe; Aufrufer serialisiert Zugriff.
    pub fn cursor_position(&self, width: usize) -> (usize, usize) {
        let cursor_line_start = match self.buffer[..self.cursor].rfind('\n') {
            Some(pos) => pos + 1,
            None => 0,
        };

        // Use the same grapheme wrapping as the displayed text, including
        // empty hard lines and unused columns before a wide grapheme.
        let row = if cursor_line_start == 0 {
            0
        } else {
            let mut preceding = Self::new();
            preceding.buffer = self.buffer[..cursor_line_start - 1].to_owned();
            preceding.visible_lines(width).len()
        };

        let line_before_cursor = &self.buffer[cursor_line_start..self.cursor];

        if width == 0 {
            let col = UnicodeWidthStr::width(line_before_cursor);
            return (row, col);
        }

        let hard_line = self.buffer[cursor_line_start..]
            .split('\n')
            .next()
            .unwrap_or("");
        let mut col_width = 0usize;
        let mut soft_row = 0usize;
        for (offset, grapheme) in hard_line.grapheme_indices(true) {
            let gw = grapheme.width();
            if col_width + gw > width && col_width > 0 {
                soft_row += 1;
                col_width = 0;
            }
            if cursor_line_start + offset >= self.cursor {
                break;
            }
            col_width += gw;
        }

        (row + soft_row, col_width)
    }

    /// High-level Convenience: nimmt ein [`KeyEvent`] und ruft die passende Methode auf.
    ///
    /// # Beschreibung
    /// Verarbeitet Standard-Tastatur-Shortcuts für die Chat-Eingabe und gibt eine
    /// [`InputAction`] zurück, die dem Aufrufer mitteilt, was als Nächstes zu tun ist.
    ///
    /// Verhalten (Tabelle aus dem Design-Dokument):
    /// - `Enter` (nicht-leer, keine Modifier, **kein aktiver Paste-Burst**) →
    ///   `Submit(text)`
    /// - `Enter` während eines erkannten Paste-Bursts (siehe
    ///   [`PasteBurstDetector`], P0.10 / G-051) → `insert_newline` + `Redraw`
    ///   statt Submit
    /// - `Shift+Enter` oder `Ctrl+J` → `insert_newline` + `Redraw`
    /// - `Enter` auf leerem Buffer → `Passthrough`
    /// - Druckbares Zeichen (ohne Ctrl) → `insert_char` + `Redraw`
    /// - `Backspace` → `backspace` + `Redraw`
    /// - `Delete` → `delete` + `Redraw`
    /// - `Left`/`Right` → `move_left`/`move_right` + `Redraw`
    /// - `Ctrl+Left`/`Ctrl+Right` → `move_word_left`/`move_word_right` + `Redraw`
    /// - `Home`/`End` → `move_home`/`move_end` + `Redraw`
    /// - `Ctrl+Backspace` → `delete_word_left` + `Redraw`
    /// - `Ctrl+Delete` → `delete_word_right` + `Redraw`
    /// - `Up` (Single-Line) → `history_back` → `Redraw` oder `Passthrough`
    /// - `Down` (Single-Line) → `history_forward`
    /// - `Up`/`Down` (Multi-Line) → `move_up`/`move_down` + `Redraw`
    /// - `Escape` während History-Browse → `cancel_history` + `Redraw`
    /// - Alle anderen → `Passthrough`
    ///
    /// # Argumente
    /// - `key` ([`KeyEvent`]): Das zu verarbeitende Tastatur-Event.
    ///
    /// # Returns
    /// [`InputAction`] die dem Aufrufer signalisiert, was zu tun ist.
    ///
    /// # Nebenläufigkeit
    /// Nicht thread-safe; Aufrufer serialisiert Zugriff.
    /// Prueft, ob Up/Down History-Recall ausloesen darf.
    ///
    /// Gate-Bedingung: leerer Buffer ODER (Cursor am Anfang/Ende UND Text entspricht last_recalled).
    fn should_recall_history(&self) -> bool {
        if self.buffer.is_empty() {
            return true;
        }
        if self.cursor != 0 && self.cursor != self.buffer.len() {
            return false;
        }
        match &self.last_recalled {
            Some(recalled) => self.buffer.as_str() == recalled.as_str(),
            None => false,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> InputAction {
        self.handle_key_at(key, Instant::now())
    }

    /// Wie [`handle_key`](Self::handle_key), aber mit injizierbarer Zeitquelle für
    /// deterministische Paste-Burst-Erkennung (siehe [`PasteBurstDetector`],
    /// P0.10 / Register G-051). `handle_key` ist ein dünner Wrapper, der
    /// `Instant::now()` übergibt; für Tests wird direkt diese Funktion mit
    /// konstruierten `Instant`-Werten aufgerufen.
    ///
    /// # Argumente
    /// - `key` ([`KeyEvent`]): Das zu verarbeitende Tastatur-Event.
    /// - `now` (`Instant`): Zeitstempel des Ereignisses, ausschließlich für die
    ///   Paste-Burst-Heuristik relevant.
    ///
    /// # Returns
    /// [`InputAction`] die dem Aufrufer signalisiert, was zu tun ist.
    pub fn handle_key_at(&mut self, key: KeyEvent, now: Instant) -> InputAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        // Alle anderen Modifier (ALT etc.) ignorieren für unbekannte Keys → Passthrough

        match key.code {
            // Enter
            KeyCode::Enter if shift => {
                self.insert_newline();
                InputAction::Redraw
            }
            KeyCode::Char('j') if ctrl => {
                self.insert_newline();
                InputAction::Redraw
            }
            KeyCode::Enter => {
                // Paste-Burst-Schutz (P0.10 / G-051): Enter innerhalb eines
                // erkannten Bursts fügt nur einen Zeilenumbruch ein, statt den
                // Puffer abzusenden. Absenden geschieht erst nach einer
                // Ruhephase (Abstand ≥ PASTE_BURST_INTERVAL) und einem danach
                // bewusst gedrückten Enter — genau dieses Enter meldet
                // `observe` dann wieder als "kein Burst".
                if self.paste_burst.observe(now) {
                    self.insert_newline();
                    return InputAction::Redraw;
                }
                let text = self.buffer.trim().to_owned();
                if text.is_empty() {
                    InputAction::Passthrough
                } else {
                    let submitted = self.buffer.clone();
                    self.clear();
                    InputAction::Submit(submitted)
                }
            }

            // Druckbare Zeichen (kein Ctrl)
            KeyCode::Char(ch) if !ctrl => {
                self.paste_burst.observe(now);
                self.insert_char(ch);
                InputAction::Redraw
            }

            // Backspace
            KeyCode::Backspace if ctrl => {
                self.delete_word_left();
                InputAction::Redraw
            }
            KeyCode::Backspace => {
                self.backspace();
                InputAction::Redraw
            }

            // Delete
            KeyCode::Delete if ctrl => {
                self.delete_word_right();
                InputAction::Redraw
            }
            KeyCode::Delete => {
                self.delete();
                InputAction::Redraw
            }

            // Cursor-Movement
            KeyCode::Left if ctrl => {
                self.move_word_left();
                InputAction::Redraw
            }
            KeyCode::Left => {
                self.move_left();
                InputAction::Redraw
            }
            KeyCode::Right if ctrl => {
                self.move_word_right();
                InputAction::Redraw
            }
            KeyCode::Right => {
                self.move_right();
                InputAction::Redraw
            }
            KeyCode::Home => {
                self.move_home();
                InputAction::Redraw
            }
            KeyCode::End => {
                self.move_end();
                InputAction::Redraw
            }

            // Up / Down
            KeyCode::Up => {
                if self.is_multiline() {
                    self.move_cursor_line(-1);
                    InputAction::Redraw
                } else if self.should_recall_history() && self.history_back() {
                    InputAction::Redraw
                } else {
                    InputAction::Passthrough
                }
            }
            KeyCode::Down => {
                if self.is_multiline() {
                    self.move_cursor_line(1);
                    InputAction::Redraw
                } else if self.should_recall_history() && self.history_forward() {
                    InputAction::Redraw
                } else {
                    InputAction::Passthrough
                }
            }

            // Escape während History-Browsing
            KeyCode::Esc if self.history_pos.is_some() => {
                self.cancel_history();
                InputAction::Redraw
            }

            _ => InputAction::Passthrough,
        }
    }

    // -----------------------------------------------------------------------
    // Private Hilfsmethoden
    // -----------------------------------------------------------------------

    /// Gibt `true` zurück, wenn der Puffer mindestens einen `\n` enthält.
    fn is_multiline(&self) -> bool {
        self.buffer.contains('\n')
    }

    /// Liefert den Byte-Offset der vorherigen Char-Grenze links von `pos`.
    ///
    /// # Panics
    /// Nur in Debug-Modus (assert), wenn `pos` nicht an einer Char-Grenze liegt.
    fn prev_grapheme_boundary(&self, pos: usize) -> usize {
        if pos == 0 {
            return 0;
        }
        // Use grapheme cluster indices for reliable boundary detection.
        let mut prev_end = 0;
        for (gb, ge) in self.buffer.grapheme_indices(true) {
            if gb + ge.len() >= pos {
                return prev_end;
            }
            prev_end = gb + ge.len();
        }
        prev_end
    }

    fn next_grapheme_boundary(&self, pos: usize) -> usize {
        if pos >= self.buffer.len() {
            return self.buffer.len();
        }
        for (gb, _ge) in self.buffer.grapheme_indices(true) {
            if gb > pos {
                return gb;
            }
        }
        self.buffer.len()
    }

    /// Byte-Offset des Anfangs der aktuellen Zeile (links vom ersten `\n` vor cursor).
    fn current_line_start(&self) -> usize {
        // Suche das letzte \n vor cursor
        match self.buffer[..self.cursor].rfind('\n') {
            Some(nl_pos) => nl_pos + 1,
            None => 0,
        }
    }

    /// Byte-Offset des Endes der aktuellen Zeile (vor dem nächsten `\n` oder Puffer-Ende).
    fn current_line_end(&self) -> usize {
        match self.buffer[self.cursor..].find('\n') {
            Some(nl_offset) => self.cursor + nl_offset,
            None => self.buffer.len(),
        }
    }

    /// Bewegt den Cursor `delta` Zeilen (positiv = runter, negativ = hoch) im Multi-Line-Modus.
    ///
    /// Versucht die Char-Spalte beizubehalten. Klemmt an Puffer-Anfang/Ende.
    fn move_cursor_line(&mut self, delta: i32) {
        // Bestimme aktuelle Zeile und Spalte (char-basiert)
        let before = &self.buffer[..self.cursor];
        let current_line_idx = before.chars().filter(|&c| c == '\n').count() as i32;
        let line_start = self.current_line_start();
        let col_chars = self.buffer[line_start..self.cursor].chars().count();

        let target_line = (current_line_idx + delta).max(0) as usize;

        // Sammle alle Zeilen
        let lines: Vec<&str> = self.buffer.split('\n').collect();
        let target_line = target_line.min(lines.len().saturating_sub(1));

        // Berechne Byte-Offset der Ziel-Zeile
        let mut byte_offset = 0usize;
        for (i, line) in lines.iter().enumerate() {
            if i == target_line {
                // Springe zur Spalte (ggf. Zeilenende)
                let target_col = col_chars.min(line.chars().count());
                let col_bytes: usize = line
                    .char_indices()
                    .nth(target_col)
                    .map(|(b, _)| b)
                    .unwrap_or(line.len());
                self.cursor = byte_offset + col_bytes;
                debug_assert!(self.buffer.is_char_boundary(self.cursor));
                return;
            }
            byte_offset += line.len() + 1; // +1 für \n
        }
    }
}

impl Default for InputEditor {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn shift_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    // 1. new_empty
    #[test]
    fn test_new_empty() {
        let ed = InputEditor::new();
        assert!(ed.is_empty());
        assert_eq!(ed.cursor(), 0);
    }

    // 2. insert_char_advances_cursor
    #[test]
    fn test_insert_char_advances_cursor() {
        let mut ed = InputEditor::new();
        ed.insert_char('a');
        ed.insert_char('b');
        ed.insert_char('c');
        assert_eq!(ed.text(), "abc");
        assert_eq!(ed.cursor(), 3);
    }

    // 3. backspace_at_end
    #[test]
    fn test_backspace_at_end() {
        let mut ed = InputEditor::new();
        ed.insert_str("abc");
        ed.backspace();
        assert_eq!(ed.text(), "ab");
        assert_eq!(ed.cursor(), 2);
    }

    // 4. backspace_at_start_is_noop
    #[test]
    fn test_backspace_at_start_is_noop() {
        let mut ed = InputEditor::new();
        ed.insert_str("abc");
        ed.move_home();
        ed.backspace();
        assert_eq!(ed.text(), "abc");
    }

    // 5. delete_forward
    #[test]
    fn test_delete_forward() {
        let mut ed = InputEditor::new();
        ed.insert_str("abc");
        ed.move_home();
        ed.move_right(); // cursor=1
        ed.delete();
        assert_eq!(ed.text(), "ac");
    }

    // 6. move_left_right
    #[test]
    fn test_move_left_right() {
        let mut ed = InputEditor::new();
        ed.insert_str("abc");
        assert_eq!(ed.cursor(), 3);
        ed.move_left();
        assert_eq!(ed.cursor(), 2);
        ed.move_left();
        assert_eq!(ed.cursor(), 1);
        ed.move_right();
        assert_eq!(ed.cursor(), 2);
    }

    // 7. move_home_end (multi-line)
    #[test]
    fn test_move_home_end_multiline() {
        let mut ed = InputEditor::new();
        ed.insert_str("abc\ndef");
        // cursor ist am Ende (7)
        ed.move_home(); // sollte Anfang der zweiten Zeile sein = Byte 4
        assert_eq!(ed.cursor(), 4);
        ed.move_end(); // Byte 7 (Ende der zweiten Zeile)
        assert_eq!(ed.cursor(), 7);
        // Erste Zeile prüfen
        ed.move_left(); // 6
        ed.move_left(); // 5
        ed.move_left(); // 4
        ed.move_left(); // 3 = \n
        ed.move_left(); // 2
        ed.move_home(); // Anfang der ersten Zeile
        assert_eq!(ed.cursor(), 0);
        ed.move_end(); // Ende der ersten Zeile (vor \n) = Byte 3
        assert_eq!(ed.cursor(), 3);
    }

    // 8. word_left_right_ascii
    #[test]
    fn test_word_left_right_ascii() {
        let mut ed = InputEditor::new();
        ed.insert_str("hello world");
        assert_eq!(ed.cursor(), 11);
        ed.move_word_left();
        assert_eq!(ed.cursor(), 6);
        ed.move_word_left();
        assert_eq!(ed.cursor(), 0);
    }

    // 9. insert_newline_multiline
    #[test]
    fn test_insert_newline_multiline() {
        let mut ed = InputEditor::new();
        ed.insert_char('a');
        ed.insert_newline();
        ed.insert_char('b');
        assert_eq!(ed.text(), "a\nb");
        assert_eq!(ed.cursor(), 3);
    }

    // 10. visible_lines_wraps_at_width
    #[test]
    fn test_visible_lines_wraps_at_width() {
        let mut ed = InputEditor::new();
        ed.insert_str("abcdefghijklmnopqrstuvwxyzabcd"); // 30 chars
        let lines = ed.visible_lines(10);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "abcdefghij");
        assert_eq!(lines[1], "klmnopqrst");
        assert_eq!(lines[2], "uvwxyzabcd");
    }

    // 11. visible_lines_respects_explicit_newlines
    #[test]
    fn test_visible_lines_explicit_newlines() {
        let mut ed = InputEditor::new();
        ed.insert_str("a\nb");
        let lines = ed.visible_lines(80);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "a");
        assert_eq!(lines[1], "b");
    }

    // 12. cursor_position_matches_layout
    #[test]
    fn test_cursor_position_multiline() {
        let mut ed = InputEditor::new();
        ed.insert_str("abc\ndef");
        // Cursor am Ende "def" → row=1, col=3
        let (row, col) = ed.cursor_position(80);
        assert_eq!(row, 1);
        assert_eq!(col, 3);
    }

    #[test]
    fn cursor_counts_empty_lines_and_wide_wrapped_lines() {
        let mut editor = InputEditor::new();
        editor.insert_str("\n\nabc");
        assert_eq!(editor.cursor_position(5), (2, 3));

        editor.clear();
        editor.insert_str("界界界界界\nx");
        assert_eq!(editor.visible_lines(5), ["界界", "界界", "界", "x"]);
        assert_eq!(editor.cursor_position(5), (3, 1));
    }

    #[test]
    fn cursor_before_wrapped_character_is_on_its_displayed_row() {
        let mut editor = InputEditor::new();
        editor.insert_str("abc界");
        editor.move_left();
        assert_eq!(editor.visible_lines(4), ["abc", "界"]);
        assert_eq!(editor.cursor_position(4), (1, 0));
    }

    // 13. history_push_and_back
    #[test]
    fn test_history_push_and_back() {
        let mut ed = InputEditor::new();
        ed.push_history("first".to_owned());
        ed.push_history("second".to_owned());
        assert!(ed.history_back());
        assert_eq!(ed.text(), "second");
        assert!(ed.history_back());
        assert_eq!(ed.text(), "first");
    }

    // 14. history_forward_returns_to_original
    #[test]
    fn test_history_forward_returns_to_original() {
        let mut ed = InputEditor::new();
        ed.push_history("first".to_owned());
        ed.push_history("second".to_owned());
        ed.history_back();
        ed.history_back(); // auf "first"
        ed.history_forward(); // auf "second"
        assert_eq!(ed.text(), "second");
        ed.history_forward(); // zurück zum leeren Snapshot
        assert_eq!(ed.text(), "");
    }

    // 15. history_deduplicates_consecutive
    #[test]
    fn test_history_deduplicates_consecutive() {
        let mut ed = InputEditor::new();
        ed.push_history("x".to_owned());
        ed.push_history("x".to_owned());
        assert_eq!(ed.history().len(), 1);
    }

    // 16. history_cap_evicts_oldest
    #[test]
    fn test_history_cap_evicts_oldest() {
        let mut ed = InputEditor::with_capacity(3);
        ed.push_history("a".to_owned());
        ed.push_history("b".to_owned());
        ed.push_history("c".to_owned());
        ed.push_history("d".to_owned());
        ed.push_history("e".to_owned());
        assert_eq!(ed.history().len(), 3);
        assert_eq!(ed.history()[0], "c");
        assert_eq!(ed.history()[2], "e");
    }

    // 17. cancel_history_restores_snapshot
    #[test]
    fn test_cancel_history_restores_snapshot() {
        let mut ed = InputEditor::new();
        ed.push_history("old".to_owned());
        ed.insert_str("abc");
        ed.history_back();
        assert_eq!(ed.text(), "old");
        ed.cancel_history();
        assert_eq!(ed.text(), "abc");
    }

    // 18. handle_key_enter_submits
    #[test]
    fn test_handle_key_enter_submits() {
        let mut ed = InputEditor::new();
        ed.insert_str("hi");
        let action = ed.handle_key(key(KeyCode::Enter));
        assert_eq!(action, InputAction::Submit("hi".to_owned()));
        assert!(ed.is_empty());
    }

    // 19. handle_key_shift_enter_inserts_newline
    #[test]
    fn test_handle_key_shift_enter_inserts_newline() {
        let mut ed = InputEditor::new();
        ed.insert_char('a');
        let action = ed.handle_key(shift_key(KeyCode::Enter));
        assert_eq!(action, InputAction::Redraw);
        assert!(ed.text().contains('\n'));
    }

    // 20. handle_key_up_in_multiline_moves_up
    #[test]
    fn test_handle_key_up_in_multiline_moves_up() {
        let mut ed = InputEditor::new();
        ed.insert_str("a\nbb");
        // Cursor ist am Ende der zweiten Zeile (Byte 4)
        assert_eq!(ed.cursor(), 4);
        let action = ed.handle_key(key(KeyCode::Up));
        assert_eq!(action, InputAction::Redraw);
        // Cursor sollte jetzt auf der ersten Zeile sein (Byte <= 1)
        assert!(ed.cursor() <= 1);
    }

    // 21. utf8_boundary_safety
    #[test]
    fn test_utf8_boundary_safety() {
        let mut ed = InputEditor::new();
        ed.insert_str("äöü");
        // Jedes Zeichen ist 2 Bytes → buffer.len() = 6
        assert_eq!(ed.cursor(), 6);
        ed.move_left();
        assert!(ed.buffer.is_char_boundary(ed.cursor()));
        ed.move_left();
        assert!(ed.buffer.is_char_boundary(ed.cursor()));
        ed.move_right();
        assert!(ed.buffer.is_char_boundary(ed.cursor()));
        ed.backspace();
        assert!(ed.buffer.is_char_boundary(ed.cursor()));
        // Kein Panic: alle Operationen sicher
    }

    // 22. combining_diacritic_cursor_movement
    #[test]
    fn test_combining_diacritic_cursor_movement() {
        let mut ed = InputEditor::new();
        // 'a' + combining diaeresis U+0308 = one grapheme cluster, then 'b'
        ed.insert_str("a\u{0308}b");
        // buffer = "äb" = 4 bytes (a=1, U+0308=2, b=1), 3 chars, 2 graphemes
        assert_eq!(ed.cursor(), 4);
        // move_left should skip past the entire "ä" grapheme to byte 3
        ed.move_left();
        assert_eq!(ed.cursor(), 3);
        ed.move_left();
        assert_eq!(ed.cursor(), 0);
        // backspace at start is no-op
        ed.backspace();
        assert_eq!(ed.cursor(), 0);
        // move_right should advance past the entire "ä" grapheme
        ed.move_right();
        assert_eq!(ed.cursor(), 3);
        // backspace should delete the entire grapheme cluster
        ed.backspace();
        assert_eq!(ed.text(), "b");
        assert_eq!(ed.cursor(), 0);
    }

    // 23. wide_char_wrapping
    #[test]
    fn test_wide_char_wrapping() {
        let mut ed = InputEditor::new();
        // 6 CJK characters, each display width 2
        ed.insert_str("你好你好你好");
        let lines = ed.visible_lines(4); // width 4 → 2 CJK chars per line
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "你好");
        assert_eq!(lines[1], "你好");
        assert_eq!(lines[2], "你好");

        // Cursor at end → row 2, col 4
        let (row, col) = ed.cursor_position(4);
        assert_eq!(row, 2);
        assert_eq!(col, 4);
    }

    // 24. history_gate_mid_text_no_recall
    #[test]
    fn test_history_gate_mid_text_no_recall() {
        let mut ed = InputEditor::new();
        ed.push_history("first".to_owned());
        ed.push_history("second".to_owned());
        // Recall first entry to set last_recalled
        ed.handle_key(key(KeyCode::Up)); // recalls "second", last_recalled = "second"
        assert_eq!(ed.text(), "second");
        // Now modify text so cursor is mid-text
        ed.move_home(); // cursor at 0, text = "second", last_recalled = "second"
        ed.insert_str("x"); // text = "xsecond", cursor at 1, not at 0 or len
        let action = ed.handle_key(key(KeyCode::Up));
        assert_eq!(action, InputAction::Passthrough); // gate fails, no recall
    }

    // -----------------------------------------------------------------------
    // Paste-Burst-Erkennung (P0.10 / Register G-051)
    // -----------------------------------------------------------------------

    // 25. paste_burst_detector_activates_after_min_events
    #[test]
    fn test_paste_burst_detector_activates_after_min_events() {
        let mut det = PasteBurstDetector::default();
        let t0 = Instant::now();
        // Erstes Ereignis: nie "schnell", da kein Vorgänger existiert.
        assert!(!det.observe(t0));
        // Zweites Ereignis 1ms später: streak=2, noch nicht aktiv (< PASTE_BURST_MIN_EVENTS).
        assert!(!det.observe(t0 + Duration::from_millis(1)));
        // Drittes Ereignis 1ms später: streak=3 → Burst aktiv.
        assert!(det.observe(t0 + Duration::from_millis(2)));
        // Burst bleibt aktiv, solange die Abstände klein bleiben.
        assert!(det.observe(t0 + Duration::from_millis(3)));
    }

    // 26. paste_burst_detector_quiet_gap_resets
    #[test]
    fn test_paste_burst_detector_quiet_gap_resets() {
        let mut det = PasteBurstDetector::default();
        let t0 = Instant::now();
        assert!(!det.observe(t0));
        assert!(!det.observe(t0 + Duration::from_millis(1)));
        assert!(det.observe(t0 + Duration::from_millis(2)));
        // Ruhephase (50ms, deutlich über PASTE_BURST_INTERVAL) beendet den
        // Burst sofort wieder, auch für genau dieses Ereignis.
        assert!(!det.observe(t0 + Duration::from_millis(52)));
    }

    // 27. paste_burst_multiline_slash_bang_stays_in_buffer
    //
    // Kernszenario aus G-051: ohne funktionierendes Bracketed Paste kommt ein
    // Copy-Paste von "/quit\n!rm -rf x\n" als schneller Strom einzelner
    // Char-/Enter-Events an. Ohne Burst-Erkennung würde das erste `\n` sofort
    // "/quit" absenden (→ Slash-Befehl), das zweite "!rm -rf x" (→ Shell-
    // Befehl). Mit Burst-Erkennung bleibt der komplette Text im Puffer.
    #[test]
    fn test_paste_burst_multiline_slash_bang_stays_in_buffer() {
        let mut ed = InputEditor::new();
        let text = "/quit\n!rm -rf x\n";
        let mut t = Instant::now();
        for ch in text.chars() {
            let key_event = if ch == '\n' {
                key(KeyCode::Enter)
            } else {
                key(KeyCode::Char(ch))
            };
            let action = ed.handle_key_at(key_event, t);
            assert!(
                !matches!(action, InputAction::Submit(_)),
                "Burst-Ereignis {ch:?} darf niemals absenden"
            );
            t += Duration::from_millis(1); // deutlich unter PASTE_BURST_INTERVAL
        }
        assert_eq!(ed.text(), text);
        assert!(!ed.is_empty());
    }

    // 28. normal_typing_with_slow_enter_submits
    //
    // Gegenprobe: normales, langsames Tippen (Abstände deutlich über dem
    // Burst-Schwellwert) darf durch die neue Erkennung nicht beeinträchtigt
    // werden — ein bewusst gedrücktes Enter sendet weiterhin ab.
    #[test]
    fn test_normal_typing_with_slow_enter_submits() {
        let mut ed = InputEditor::new();
        let mut t = Instant::now();
        for ch in "hallo".chars() {
            ed.handle_key_at(key(KeyCode::Char(ch)), t);
            t += Duration::from_millis(50); // deutlich über PASTE_BURST_INTERVAL
        }
        let action = ed.handle_key_at(key(KeyCode::Enter), t);
        assert_eq!(action, InputAction::Submit("hallo".to_owned()));
        assert!(ed.is_empty());
    }

    // 29. paste_event_with_slash_command_does_not_auto_submit
    //
    // Der Bracketed-Paste-Pfad (app.rs: `Event::Paste` → `insert_str`) läuft
    // nie über `handle_key`/`handle_key_at` und kann daher nie automatisch
    // absenden, unabhängig vom Inhalt oder der Anzahl enthaltener `\n`. Erst
    // ein danach bewusst gedrücktes Enter (normaler Abstand) sendet den
    // gesamten eingefügten Block als eine Einheit ab.
    #[test]
    fn test_paste_event_with_slash_command_does_not_auto_submit() {
        let mut ed = InputEditor::new();
        ed.insert_str("/cmd\n!echo hi\n");
        assert_eq!(ed.text(), "/cmd\n!echo hi\n");
        assert!(!ed.is_empty());

        let action = ed.handle_key_at(key(KeyCode::Enter), Instant::now());
        assert_eq!(action, InputAction::Submit("/cmd\n!echo hi\n".to_owned()));
        assert!(ed.is_empty());
    }
}
