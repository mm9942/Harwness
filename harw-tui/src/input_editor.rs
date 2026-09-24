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
//! - [`PendingPaste`] — Metadaten eines noch nicht expandierten Paste-Platzhalters
//!   (siehe Abschnitt "Paste-Platzhalter" unten).
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

/// Zeilenschwelle für den Paste-Platzhalter (siehe Moduldoku
/// "Paste-Platzhalter"): mehr als diese Zeilenzahl löst einen Platzhalter
/// statt einer wörtlichen Einfügung aus.
const PASTE_PLACEHOLDER_LINE_THRESHOLD: usize = 3;

/// Zeichenschwelle für den Paste-Platzhalter: mehr als diese Zeichenzahl
/// löst ebenfalls einen Platzhalter aus, auch bei wenigen Zeilen.
const PASTE_PLACEHOLDER_CHAR_THRESHOLD: usize = 800;

/// Maximaler Abstand zwischen einem bar `Esc` und dem vermuteten CSI-
/// Einleitungszeichen (`[`/`O`), damit das CSI-Sicherheitsnetz greift
/// (siehe Moduldoku "CSI-Sicherheitsnetz").
const CSI_ESC_WINDOW: Duration = Duration::from_millis(30);

/// Maximale Länge der gesammelten CSI-Sequenz (ohne das einleitende `Esc`),
/// bevor die Sammlung als gescheitert gilt und die Zeichen als normaler
/// Text eingefügt werden.
const CSI_MAX_LEN: usize = 32;

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
// Paste-Platzhalter (dtach/zsh-Härtung, Register „CSI-Sicherheitsnetz und
// Paste-Platzhalter")
// ---------------------------------------------------------------------------
//
// Ein großer eingefügter Text (echtes Bracketed Paste ODER ein per
// `insert_paste` erkannter Paste-Burst-Fallback, siehe unten) wird NICHT
// wörtlich in den Puffer geschrieben, sondern durch einen kompakten
// Platzhalter `[Pasted text #<id> +<n> lines]` ersetzt. Grund: ein sehr
// langer wörtlicher Paste macht die einzeilige/wenige-Zeilen-Composer-Ansicht
// unbedienbar (Cursor-Navigation, Zeilenumbruch-Darstellung) und bläht jede
// Undo-/History-Anzeige auf. Der volle Text bleibt unter einer stabilen,
// nie wiederverwendeten `id` in [`InputEditor::pastes`] gemerkt und wird
// beim Absenden (Enter, siehe `handle_key_at`) automatisch wieder expandiert
// — das Modell/die Befehls-Pipeline sieht also immer den vollen Text, nie
// den Platzhalter. `backspace`/`delete` entfernen einen an der Cursor-
// Position angrenzenden Platzhalter atomar (ein Tastendruck statt Dutzender),
// `prune_pastes` hält `pastes` konsistent, sobald ein Platzhalter durch eine
// andere Operation (Wort-Löschung, mittiges Tippen) beschädigt wurde.
//
// # Paste-Burst-Fallback
// Ohne Bracketed Paste (siehe `PasteBurstDetector` oben) kommt ein großer
// Paste als Strom einzelner `KeyCode::Char`-Ereignisse an. Diese werden
// weiterhin sofort und einzeln in den sichtbaren Puffer geschrieben (damit
// jede Zwischenbeobachtung von `text()` während des Bursts weiterhin den
// tatsächlich bereits getippten/eingefügten Text zeigt — siehe die
// Regressionstests zu G-051). Erst wenn der Burst tatsächlich ENDET (ein
// nachfolgendes Ereignis ist nicht mehr "schnell") wird die seit
// `InputEditor::burst_start` angesammelte Spanne rückwirkend kollabiert:
// ist sie groß genug für einen Platzhalter, wird sie aus dem Puffer entfernt
// und stattdessen über `insert_paste` als Platzhalter neu eingefügt; sonst
// bleibt sie unverändert als normaler Text stehen. Dieser rückwirkende
// Ansatz ist bewusst konservativ: normales, langsames Tippen bleibt exakt
// unverändert (burst_start wird nie gesetzt), und ein Burst, der nie endet
// (z. B. weil der Test-/Eingabestrom einfach aufhört), hinterlässt den
// bereits geschriebenen Text unverändert — kein rückwirkendes Löschen ohne
// eine sichere Abschlussgrenze.
//
// ---------------------------------------------------------------------------
// CSI-Sicherheitsnetz (dtach/zsh auf Raspberry Pi)
// ---------------------------------------------------------------------------
//
// Auf dieser Plattform (dtach-Relay + zsh, siehe Aufgabenkontext) werden
// mehrbyte-ANSI-Escape-Sequenzen (CSI, `ESC [ …` bzw. `ESC O …`) gelegentlich
// über mehrere Terminal-Reads verteilt an crossterm ausgeliefert. Crossterm
// wartet nicht beliebig lange auf Folgebytes und liefert dann einen bar
// `KeyCode::Esc` gefolgt von den restlichen Bytes als GEWÖHNLICHE
// `KeyCode::Char`-Ereignisse aus — nicht als der eigentlich gemeinte
// Spezial-Key. Beobachtete Symptome: Pos1/Ende "tun nichts" (die Sequenz
// `ESC[H`/`ESC[1~`/`ESC OH` bzw. `ESC[F`/`ESC[4~`/`ESC OF` wird nie als
// `KeyCode::Home`/`KeyCode::End` erkannt) und nach einer Host-`shell.exec`-
// Phase erscheinen rohe SGR-Maus-Reports (`[<65;22;8M…`) als Text im
// Composer, weil auch Maus-Events auf demselben Weg zerstückelt ankommen.
//
// Das Sicherheitsnetz merkt sich einen bar `Esc` (`pending_esc_at`) für
// [`CSI_ESC_WINDOW`]. Folgt innerhalb dieses Fensters ein `[` oder `O` als
// nächstes Zeichen, beginnt eine Sammlung (`csi_collect`) bis zu einem
// Abschluss-Byte (`A-Za-z` oder `~`, siehe ECMA-48). Die vollständige
// Sequenz wird dann einer Editor-Aktion zugeordnet (Pos1/Ende/Wortsprung/
// Entf) oder verworfen (SGR-Maus-Reports und alles Unbekannte). Bleibt der
// erwartete Folge-Byte aus oder läuft das Fenster ab, gelten die
// gesammelten Zeichen als tatsächlich getippter Text und werden ganz normal
// eingefügt — normales, schnelles Tippen von z. B. `[)` bleibt dadurch
// unangetastet.
//
// # App-seitiges Doppel-Esc (siehe `app.rs`, „escape_armed")
// `app.rs` scharft bei einem ersten bar `Esc` eine „zweites Esc leert den
// Composer"-Logik. Da ein durch dieses Sicherheitsnetz verschluckter `Esc`
// KEIN bewusster Tastendruck war, würde diese Scharfstellung sonst
// fälschlich reagieren, sobald der Nutzer als Nächstes wirklich `Esc`
// drückt. [`InputEditor::take_swallowed_escape`] liefert dafür ein
// Einmal-Signal: sobald eine gesammelte Sequenz sauber (mit Abschluss-Byte)
// aufgelöst wurde, ist klar, dass der auslösende `Esc` verschluckt war, und
// `app.rs` macht die Scharfstellung direkt beim nächsten Tastenereignis
// wieder rückgängig (siehe dortige Doku am Aufrufort). Der destruktive
// „zweites Esc löscht sofort"-Zweig selbst bleibt unverändert: er wird nur
// erreicht, wenn `escape_armed` bereits `true` ist, und das kann nach dieser
// Korrektur nur noch durch einen tatsächlich verschluckten ODER einen
// echten ersten `Esc` geschehen — im ersten Fall korrigiert genau dieses
// Signal die Scharfstellung, bevor ein zweites `Esc`-Ereignis überhaupt
// eintreffen kann (dieselbe Terminal-Charge liefert das Folgezeichen
// praktisch im selben Zug).
//
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
    /// Cursor-Position, an der der aktuell laufende Paste-Burst begonnen hat
    /// (siehe Moduldoku "Paste-Burst-Fallback"). `None`, solange kein Burst
    /// aktiv ist.
    burst_start: Option<usize>,
    /// Noch nicht expandierte Paste-Platzhalter dieser Puffer-Instanz (siehe
    /// Moduldoku "Paste-Platzhalter").
    pastes: Vec<PendingPaste>,
    /// Nächste zu vergebende Paste-ID. Startet bei 1 und wird nie
    /// zurückgesetzt oder wiederverwendet (auch nicht nach `clear`/Submit).
    next_paste_id: u32,
    /// Zeitpunkt des letzten bar `Esc`, solange offen ist, ob unmittelbar
    /// danach eine zerstückelte CSI-Sequenz folgt (siehe Moduldoku
    /// "CSI-Sicherheitsnetz").
    pending_esc_at: Option<Instant>,
    /// Im Aufbau befindliche, mutmaßliche CSI-Sequenz (ohne das einleitende
    /// `Esc`), solange das CSI-Sicherheitsnetz aktiv sammelt.
    csi_collect: Option<String>,
    /// Einmal-Signal: `true`, wenn der zuletzt verarbeitete bar `Esc`
    /// rückwirkend als Anfang einer zerstückelten CSI-Sequenz erkannt wurde
    /// — also KEIN bewusster Tastendruck war. Von
    /// [`Self::take_swallowed_escape`] konsumiert.
    swallowed_escape: bool,
}

/// Metadaten eines im Composer nur als kompakter Platzhalter angezeigten
/// eingefügten Texts (siehe Moduldoku "Paste-Platzhalter").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPaste {
    /// Stabile, innerhalb dieser [`InputEditor`]-Instanz nie wiederverwendete
    /// Kennung dieses Pastes.
    pub id: u32,
    /// Der vollständige, eingefügte Text — wird beim Absenden (Enter) an
    /// Stelle des Platzhalters expandiert.
    pub text: String,
    /// Anzahl der Zeilen in `text` (für die Platzhalter-Anzeige
    /// `[Pasted text #<id> +<n> lines]`).
    pub line_count: usize,
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
            burst_start: None,
            pastes: Vec::new(),
            next_paste_id: 1,
            pending_esc_at: None,
            csi_collect: None,
            swallowed_escape: false,
        }
    }

    // -----------------------------------------------------------------------
    // Introspektion
    // -----------------------------------------------------------------------

    /// Gibt den aktuellen Textinhalt des Puffers zurück.
    ///
    /// # Returns
    /// Borrowed `&str` des internen Puffers (beinhaltet ggf. `\n`-Zeichen).
    /// Paste-Platzhalter stehen hier noch als `[Pasted text #<id> +<n>
    /// lines]` — wer den Text **absendet**, nimmt
    /// [`Self::submission_text`].
    pub fn text(&self) -> &str {
        &self.buffer
    }

    /// Der Text, der beim Absenden ans Modell bzw. die Befehls-Pipeline
    /// geht: der Puffer mit jedem Paste-Platzhalter zurück in seinen vollen
    /// Text aufgelöst (siehe Moduldoku "Paste-Platzhalter"). Der Puffer
    /// selbst bleibt unverändert.
    #[must_use]
    pub fn submission_text(&self) -> String {
        self.expand_pastes()
    }

    /// Ersetzt den Puffer durch `text` und setzt den Cursor auf das
    /// Byte-Offset `cursor` (auf die nächste gültige Grenze geklemmt), ohne
    /// die gemerkten Pastes zu verlieren: Platzhalter, die in `text`
    /// weiterhin vorkommen, bleiben auflösbar (anders als
    /// [`Self::clear`] + [`Self::insert_str`]).
    pub fn replace_text_keeping_pastes(&mut self, text: &str, cursor: usize) {
        self.buffer = text.to_owned();
        let mut cursor = cursor.min(self.buffer.len());
        while !self.buffer.is_char_boundary(cursor) {
            cursor -= 1;
        }
        self.cursor = cursor;
        self.last_recalled = None;
        self.burst_start = None;
        self.prune_pastes();
    }

    /// Gibt die aktuelle Cursor-Position als Byte-Offset zurück.
    ///
    /// # Returns
    /// Byte-Offset im UTF-8-Puffer. Immer an einer Char-Grenze.
    #[cfg(test)]
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

    /// `true`, solange der Puffer unverändert einen per Hoch/Runter
    /// zurückgeholten History-Eintrag zeigt (Runde 5, Teil G).
    ///
    /// # Beschreibung
    /// Die TUI öffnet währenddessen kein `/`- oder `@`-Popup, damit das
    /// nächste Hoch/Runter weiter durch die History blättert, statt im Popup
    /// eines zurückgeholten `/befehl`s zu landen. Sobald getippt oder
    /// editiert wird, weicht der Puffer vom Eintrag ab und das Popup gilt
    /// wieder.
    #[must_use]
    pub fn is_browsing_history(&self) -> bool {
        self.history_pos.is_some()
            && self
                .last_recalled
                .as_deref()
                .is_some_and(|recalled| recalled == self.buffer)
    }

    /// Gibt die gespeicherten History-Einträge zurück (älteste vorne, jüngste hinten).
    ///
    /// # Returns
    /// Slice der History-Einträge.
    #[cfg(test)]
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
        self.prune_pastes();
        debug_assert!(self.buffer.is_char_boundary(self.cursor));
    }

    /// Fügt einen String an der aktuellen Cursor-Position ein und bewegt den Cursor.
    ///
    /// # Argumente
    /// - `s` (`&str`): Der einzufügende Text.
    pub fn insert_str(&mut self, s: &str) {
        self.buffer.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.prune_pastes();
        debug_assert!(self.buffer.is_char_boundary(self.cursor));
    }

    /// Fügt einen Zeilenumbruch (`\n`) an der aktuellen Cursor-Position ein.
    pub fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    /// Fügt einen eingefügten Text (Paste) an der Cursor-Position ein (siehe
    /// Moduldoku "Paste-Platzhalter").
    ///
    /// # Beschreibung
    /// Ist `text` klein (höchstens [`PASTE_PLACEHOLDER_LINE_THRESHOLD`]
    /// Zeilen UND höchstens [`PASTE_PLACEHOLDER_CHAR_THRESHOLD`] Zeichen),
    /// wird er wörtlich eingefügt (wie [`Self::insert_str`]). Sonst wird
    /// stattdessen ein kompakter, sichtbarer Platzhalter `[Pasted text #<id>
    /// +<n> lines]` eingefügt und der vollständige Text unter einer neuen,
    /// nie wiederverwendeten `id` in [`Self::pastes`] gemerkt —
    /// [`Self::handle_key_at`] expandiert ihn beim Absenden (Enter) wieder
    /// zurück in den vollen Text.
    ///
    /// # Argumente
    /// - `text` (`&str`): der einzufügende (eingefügte) Text.
    pub fn insert_paste(&mut self, text: &str) {
        let line_count = text.split('\n').count();
        let char_count = text.chars().count();
        if line_count > PASTE_PLACEHOLDER_LINE_THRESHOLD
            || char_count > PASTE_PLACEHOLDER_CHAR_THRESHOLD
        {
            let id = self.next_paste_id;
            self.next_paste_id = self.next_paste_id.saturating_add(1);
            let pending = PendingPaste {
                id,
                text: text.to_owned(),
                line_count,
            };
            let marker = Self::placeholder_text(&pending);
            self.pastes.push(pending);
            self.insert_str(&marker);
        } else {
            self.insert_str(text);
        }
    }

    /// Löscht das Zeichen links vom Cursor (Backspace-Semantik).
    ///
    /// Ist der Cursor am Anfang, ist die Operation ein No-Op.
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        // Endet unmittelbar vor dem Cursor ein noch bekannter Paste-
        // Platzhalter (siehe Moduldoku "Paste-Platzhalter"), wird er als
        // Ganzes entfernt statt grapheme-weise zerlegt zu werden.
        if let Some((start, end)) = self.placeholder_ending_at(self.cursor) {
            self.buffer.drain(start..end);
            self.cursor = start;
            self.prune_pastes();
            debug_assert!(self.buffer.is_char_boundary(self.cursor));
            return;
        }
        // Schritt zurück zur vorherigen Char-Grenze
        let prev = self.prev_grapheme_boundary(self.cursor);
        self.buffer.drain(prev..self.cursor);
        self.cursor = prev;
        self.prune_pastes();
        debug_assert!(self.buffer.is_char_boundary(self.cursor));
    }

    /// Löscht auf der aktuellen Zeile alles rechts vom Cursor (Ctrl+K-Semantik,
    /// Emacs `kill-line`).
    ///
    /// Steht der Cursor noch vor dem Zeilenende, wird nur bis zum Zeilenende
    /// gelöscht; der Zeilenumbruch selbst bleibt erhalten, damit Ctrl+K keinen
    /// mehrzeiligen Entwurf unerwartet verkürzt. Steht der Cursor dagegen
    /// bereits am Zeilenende — nichts mehr auf dieser Zeile zu löschen — und
    /// folgt eine weitere Zeile, wird stattdessen der Zeilenumbruch selbst
    /// entfernt: die nächste Zeile rückt an die aktuelle heran (klassisches
    /// Emacs-`kill-line`-Verhalten). Steht der Cursor bereits am Ende des
    /// gesamten Puffers (keine weitere Zeile zum Anhängen), ist die Operation
    /// ein No-Op.
    pub fn delete_to_end(&mut self) {
        let end = self.current_line_end();
        if self.cursor < end {
            self.buffer.drain(self.cursor..end);
        } else if end < self.buffer.len() {
            // `end` zeigt auf den Zeilenumbruch selbst (buffer[end] == '\n').
            let next = end + '\n'.len_utf8();
            self.buffer.drain(end..next);
        }
        self.prune_pastes();
        debug_assert!(self.buffer.is_char_boundary(self.cursor));
    }

    /// Löscht die komplette aktuelle Zeile (nicht nur bis Zeilenende) —
    /// nachfolgende Zeilen rücken automatisch nach oben. Anders als
    /// `delete_to_end` (Kürzung ab Cursor) wird hier der ganze Zeilenbereich
    /// entfernt, unabhängig von der Cursor-Position innerhalb der Zeile.
    pub fn delete_current_line(&mut self) {
        let line_start = self.current_line_start();
        // Ende der Zeile inklusive des nachfolgenden `\n`, damit die
        // nächste Zeile nach oben rückt. Gibt es keinen weiteren `\n`
        // (letzte Zeile), reicht das Ende bis zum Puffer-Ende.
        let line_end = match self.buffer[self.cursor..].find('\n') {
            Some(nl_offset) => self.cursor + nl_offset + 1,
            None => self.buffer.len(),
        };
        self.buffer.replace_range(line_start..line_end, "");
        self.cursor = line_start;
        self.prune_pastes();
        debug_assert!(self.buffer.is_char_boundary(self.cursor));
    }

    /// Löscht das Zeichen rechts vom Cursor (Vorwärts-Delete).
    ///
    /// Ist der Cursor am Ende, ist die Operation ein No-Op. Beginnt
    /// unmittelbar ab dem Cursor ein noch bekannter Paste-Platzhalter
    /// (siehe Moduldoku "Paste-Platzhalter"), wird er als Ganzes entfernt
    /// statt grapheme-weise zerlegt zu werden.
    pub fn delete(&mut self) {
        if self.cursor >= self.buffer.len() {
            return;
        }
        if let Some((start, end)) = self.placeholder_starting_at(self.cursor) {
            self.buffer.drain(start..end);
            self.prune_pastes();
            debug_assert!(self.buffer.is_char_boundary(self.cursor));
            return;
        }
        let next = self.next_grapheme_boundary(self.cursor);
        self.buffer.drain(self.cursor..next);
        self.prune_pastes();
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
        self.prune_pastes();
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
        self.prune_pastes();
        debug_assert!(self.buffer.is_char_boundary(self.cursor));
    }

    /// Löscht das Wort links vom Cursor (Ctrl+Backspace, Ctrl+W) — behandelt
    /// einen unmittelbar davor endenden Paste-Platzhalter (siehe Moduldoku
    /// "Paste-Platzhalter") dabei als EIN Wort und entfernt ihn atomar,
    /// statt ihn wortweise in seine Bestandteile zu zerlegen.
    ///
    /// # Beschreibung
    /// Ohne einen dort endenden Platzhalter identisch zu
    /// [`Self::delete_word_left`].
    pub fn delete_word_before_cursor(&mut self) {
        if let Some((start, end)) = self.placeholder_ending_at(self.cursor) {
            self.buffer.drain(start..end);
            self.cursor = start;
            self.prune_pastes();
            debug_assert!(self.buffer.is_char_boundary(self.cursor));
            return;
        }
        self.delete_word_left();
    }

    /// Bewegt den Cursor an den Anfang des GESAMTEN Puffers, nicht nur der
    /// aktuellen Zeile (Ctrl+Pos1) — Gegenstück zu [`Self::move_home`].
    pub fn move_buffer_start(&mut self) {
        self.cursor = 0;
    }

    /// Bewegt den Cursor an das Ende des GESAMTEN Puffers, nicht nur der
    /// aktuellen Zeile (Ctrl+Ende) — Gegenstück zu [`Self::move_end`].
    pub fn move_buffer_end(&mut self) {
        self.cursor = self.buffer.len();
    }

    /// Leert den Puffer und setzt den Cursor auf 0.
    ///
    /// # Beschreibung
    /// Verwirft außerdem alle noch offenen Paste-Platzhalter (siehe
    /// Moduldoku "Paste-Platzhalter") — ein geleerter Puffer kann per
    /// Definition keinen gültigen Platzhalter mehr enthalten — und einen
    /// evtl. noch nicht kollabierten Paste-Burst (siehe "Paste-Burst-
    /// Fallback").
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.last_recalled = None;
        self.pastes.clear();
        self.burst_start = None;
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
        // Ein History-Eintrag ist immer schon expandierter Text (siehe
        // Moduldoku "Paste-Platzhalter") — `prune_pastes` räumt jeden
        // Platzhalter des vorherigen Puffers auf, dessen Text im neu
        // geladenen Eintrag nicht mehr vorkommt.
        self.prune_pastes();
        self.burst_start = None;
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
        self.prune_pastes();
        self.burst_start = None;
        true
    }

    /// Bricht das History-Browsing ab und stellt den Buffer-Snapshot wieder her.
    ///
    /// No-Op wenn kein Browsing aktiv ist.
    ///
    /// # Beschreibung
    /// Ruft [`Self::prune_pastes`] statt eines unbedingten `pastes.clear()`
    /// auf: der wiederhergestellte Snapshot ist der Puffer-Stand VOR dem
    /// History-Browsing und kann daher noch gültige Paste-Platzhalter
    /// enthalten (siehe Moduldoku "Paste-Platzhalter") — die dürfen die
    /// Rückkehr aus dem Browsing überleben.
    pub fn cancel_history(&mut self) {
        if let Some(snapshot) = self.history_snapshot.take() {
            self.buffer = snapshot;
            self.cursor = self.buffer.len();
        }
        self.history_pos = None;
        self.last_recalled = None;
        self.prune_pastes();
        self.burst_start = None;
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
    /// - Absenden ersetzt jeden noch offenen Paste-Platzhalter durch seinen
    ///   vollen Text (`expand_pastes`, siehe Moduldoku "Paste-Platzhalter")
    /// - Druckbares Zeichen (ohne Ctrl) → `insert_char` + `Redraw`; ein
    ///   erkannter Paste-Burst (siehe Moduldoku "Paste-Burst-Fallback")
    ///   wird nach Ende auf Platzhalter-Größe geprüft
    /// - Ein eingefügter Paste (`insert_paste`, siehe Aufrufer in `app.rs`)
    ///   wird ab [`PASTE_PLACEHOLDER_LINE_THRESHOLD`] Zeilen bzw.
    ///   [`PASTE_PLACEHOLDER_CHAR_THRESHOLD`] Zeichen als Platzhalter
    ///   eingefügt statt wörtlich
    /// - `Backspace` → `backspace` + `Redraw` (entfernt einen angrenzenden
    ///   Paste-Platzhalter atomar)
    /// - `Delete` → `delete` + `Redraw` (ebenfalls Platzhalter-atomar)
    /// - `Left`/`Right` → `move_left`/`move_right` + `Redraw`
    /// - `Ctrl+Left`/`Ctrl+Right`, `Alt+Left`/`Alt+Right`, `Alt+b`/`Alt+f` →
    ///   `move_word_left`/`move_word_right` + `Redraw`
    /// - `Home`/`End` → `move_home`/`move_end` (aktuelle Zeile) + `Redraw`
    /// - `Ctrl+Home`/`Ctrl+End` → `move_buffer_start`/`move_buffer_end`
    ///   (gesamter Puffer) + `Redraw`
    /// - `Ctrl+A`/`Ctrl+E` → `move_home`/`move_end` + `Redraw`
    /// - `Ctrl+Backspace`/`Ctrl+W` → `delete_word_before_cursor` + `Redraw`
    ///   (Platzhalter-atomar)
    /// - `Ctrl+Delete` → `delete_word_right` + `Redraw`
    /// - `Up` (Single-Line) → `history_back` → `Redraw` oder `Passthrough`
    /// - `Down` (Single-Line) → `history_forward`
    /// - `Up`/`Down` (Multi-Line) → `move_cursor_line` + `Redraw`
    /// - `Escape` während History-Browse → `cancel_history` + `Redraw`
    /// - Jeder bar `Escape` merkt sich zusätzlich einen möglichen
    ///   CSI-Sequenz-Anfang (siehe Moduldoku "CSI-Sicherheitsnetz");
    ///   `Ctrl+H` bleibt bewusst unbelegt (siehe `app.rs`)
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
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        // ---- CSI-Sicherheitsnetz (siehe Moduldoku "CSI-Sicherheitsnetz") ----
        // Eine laufende Sammlung geht jeder anderen Interpretation von `key`
        // vor.
        if self.csi_collect.is_some() {
            return self.continue_csi_collection(key, ctrl, now);
        }
        // Ein zuvor markierter bar `Esc`: prüft, ob DIESES Event ihn als
        // Anfang einer zerstückelten CSI-Sequenz bestätigt.
        if let Some(esc_at) = self.pending_esc_at {
            if now.duration_since(esc_at) <= CSI_ESC_WINDOW {
                if let KeyCode::Char(introducer @ ('[' | 'O')) = key.code {
                    if !ctrl {
                        self.pending_esc_at = None;
                        self.csi_collect = Some(introducer.to_string());
                        return InputAction::Redraw;
                    }
                }
            }
            // Timeout oder Nicht-Treffer: der vorherige `Esc` war ein
            // echter, bewusster Tastendruck — `key` normal weiterverarbeiten.
            self.pending_esc_at = None;
        }

        // ---- Paste-Burst-Fallback (siehe Moduldoku "Paste-Burst-Fallback")
        // ---- Jedes Nicht-Char-/Nicht-Enter-Event beendet einen laufenden
        // Burst sofort, damit es auf einem konsistenten Puffer operiert.
        if self.burst_start.is_some() && !matches!(key.code, KeyCode::Char(_) | KeyCode::Enter) {
            self.collapse_burst_if_any();
        }

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
            KeyCode::Char('k' | 'K') if ctrl => {
                self.delete_to_end();
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
                    if self.burst_start.is_none() {
                        self.burst_start = Some(self.cursor);
                    }
                    self.insert_newline();
                    return InputAction::Redraw;
                }
                // Burst (falls einer lief) ist hiermit vorbei: einen evtl.
                // gepufferten Rest zuerst kollabieren (siehe
                // `collapse_burst_if_any`), bevor über Submit entschieden wird.
                self.collapse_burst_if_any();
                let text = self.buffer.trim().to_owned();
                if text.is_empty() {
                    InputAction::Passthrough
                } else {
                    // Jeden noch offenen Paste-Platzhalter zurück in seinen
                    // vollen Text auflösen (siehe Moduldoku
                    // "Paste-Platzhalter") — der Aufrufer sieht nie den
                    // Platzhalter, nur den echten Inhalt.
                    let expanded = self.expand_pastes();
                    self.clear();
                    InputAction::Submit(expanded)
                }
            }

            // Wortweise Cursor-Bewegung per Emacs-Konvention (Alt+b/Alt+f),
            // vor dem generischen Zeichen-Einfüge-Zweig, damit sie nicht als
            // 'b'/'f' eingefügt werden.
            KeyCode::Char('b' | 'B') if alt && !ctrl => {
                self.move_word_left();
                InputAction::Redraw
            }
            KeyCode::Char('f' | 'F') if alt && !ctrl => {
                self.move_word_right();
                InputAction::Redraw
            }
            // Strg+A / Strg+E (Emacs-Konvention „Zeilenanfang"/„Zeilenende"),
            // in app.rs unbelegt (siehe dortige Tastenbelegung).
            KeyCode::Char('a' | 'A') if ctrl => {
                self.move_home();
                InputAction::Redraw
            }
            KeyCode::Char('e' | 'E') if ctrl => {
                self.move_end();
                InputAction::Redraw
            }
            // Strg+W (Emacs-/Shell-Konvention „Wort vor Cursor löschen"),
            // gleichbedeutend mit Strg+Backspace — beide atomar bzgl. eines
            // angrenzenden Paste-Platzhalters (siehe
            // `delete_word_before_cursor`). Ctrl+H bleibt hier bewusst
            // UNBELEGT: app.rs fängt es bereits vor dem Editor ab, um eine
            // laufende Host-Arbeitsphase zu beenden (siehe dortige Doku).
            KeyCode::Char('w' | 'W') if ctrl => {
                self.delete_word_before_cursor();
                InputAction::Redraw
            }

            // Druckbare Zeichen (kein Ctrl)
            KeyCode::Char(ch) if !ctrl => {
                if self.paste_burst.observe(now) {
                    if self.burst_start.is_none() {
                        self.burst_start = Some(self.cursor);
                    }
                } else {
                    self.collapse_burst_if_any();
                }
                self.insert_char(ch);
                InputAction::Redraw
            }

            // Backspace
            KeyCode::Backspace if ctrl => {
                self.delete_word_before_cursor();
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

            // Cursor-Movement — Strg ODER Alt bewegt wortweise.
            KeyCode::Left if ctrl || alt => {
                self.move_word_left();
                InputAction::Redraw
            }
            KeyCode::Left => {
                self.move_left();
                InputAction::Redraw
            }
            KeyCode::Right if ctrl || alt => {
                self.move_word_right();
                InputAction::Redraw
            }
            KeyCode::Right => {
                self.move_right();
                InputAction::Redraw
            }
            // Strg+Pos1/Strg+Ende springen an Anfang/Ende des GESAMTEN
            // Puffers; einfaches Pos1/Ende bleibt zeilenbezogen (siehe
            // `move_home`/`move_end`). In app.rs geht Strg+Pos1/Strg+Ende
            // bei nicht-leerer Eingabe an den Editor statt an den
            // Transkript-Scroll (siehe dortige `scroll_claims_key`-Doku).
            KeyCode::Home if ctrl => {
                self.move_buffer_start();
                InputAction::Redraw
            }
            KeyCode::End if ctrl => {
                self.move_buffer_end();
                InputAction::Redraw
            }
            // Pos1/Ende springen an Anfang/Ende der GESAMTEN Eingabe (auch
            // mehrzeilig) — zeilenweise Bewegung bleibt bei Strg+A/Strg+E.
            KeyCode::Home => {
                self.move_buffer_start();
                InputAction::Redraw
            }
            KeyCode::End => {
                self.move_buffer_end();
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
                // Auch dieser Esc kann in Wahrheit eine zerstückelte
                // CSI-Sequenz einleiten (siehe Moduldoku
                // "CSI-Sicherheitsnetz") — vormerken, ohne das bestehende
                // Verhalten (History-Abbruch) zu ändern.
                self.pending_esc_at = Some(now);
                self.cancel_history();
                InputAction::Redraw
            }
            // Jeder andere bar Esc: Verhalten unverändert (Passthrough),
            // zusätzlich als möglichen CSI-Sequenz-Anfang vormerken.
            KeyCode::Esc => {
                self.pending_esc_at = Some(now);
                InputAction::Passthrough
            }

            _ => InputAction::Passthrough,
        }
    }

    /// Konsumiert das Einmal-Signal aus [`Self::swallowed_escape`] (siehe
    /// Moduldoku "CSI-Sicherheitsnetz").
    ///
    /// # Rückgabe
    /// `true` genau einmal, wenn der zuletzt verarbeitete bar `Esc`
    /// rückwirkend als Anfang einer zerstückelten CSI-Sequenz erkannt wurde
    /// und daher KEIN bewusster Tastendruck war — der Aufrufer (app.rs)
    /// macht damit eine deswegen bereits vorgenommene Escape-Scharfstellung
    /// rückgängig.
    pub fn take_swallowed_escape(&mut self) -> bool {
        std::mem::take(&mut self.swallowed_escape)
    }

    /// Gibt die noch nicht expandierten Paste-Platzhalter zurück (siehe
    /// Moduldoku "Paste-Platzhalter").
    ///
    /// # Rückgabe
    /// Slice der aktuell offenen [`PendingPaste`]-Einträge.
    #[cfg(test)]
    pub fn pastes(&self) -> &[PendingPaste] {
        &self.pastes
    }

    // -----------------------------------------------------------------------
    // Private Hilfsmethoden
    // -----------------------------------------------------------------------

    /// Gibt `true` zurück, wenn der Puffer mindestens einen `\n` enthält.
    fn is_multiline(&self) -> bool {
        self.buffer.contains('\n')
    }

    // -- Paste-Platzhalter (siehe Moduldoku "Paste-Platzhalter") -----------

    /// Baut den sichtbaren Platzhaltertext für einen [`PendingPaste`].
    fn placeholder_text(paste: &PendingPaste) -> String {
        format!("[Pasted text #{} +{} lines]", paste.id, paste.line_count)
    }

    /// Sucht unter [`Self::pastes`] einen Platzhalter, dessen Text im
    /// Puffer unmittelbar VOR `pos` endet (für atomares [`Self::backspace`]
    /// und [`Self::delete_word_before_cursor`]).
    ///
    /// # Rückgabe
    /// `(start, end)` Byte-Offsets des gesamten Platzhaltertexts im Puffer,
    /// mit `end == pos`, falls einer gefunden wurde.
    fn placeholder_ending_at(&self, pos: usize) -> Option<(usize, usize)> {
        self.pastes.iter().find_map(|paste| {
            let marker = Self::placeholder_text(paste);
            let marker_len = marker.len();
            if marker_len > pos {
                return None;
            }
            let start = pos - marker_len;
            if !self.buffer.is_char_boundary(start) {
                return None;
            }
            if self.buffer[start..pos] == marker {
                Some((start, pos))
            } else {
                None
            }
        })
    }

    /// Wie [`Self::placeholder_ending_at`], aber für einen Platzhalter,
    /// dessen Text im Puffer unmittelbar AB `pos` beginnt (für atomares
    /// [`Self::delete`]).
    ///
    /// # Rückgabe
    /// `(start, end)` Byte-Offsets des gesamten Platzhaltertexts im Puffer,
    /// mit `start == pos`, falls einer gefunden wurde.
    fn placeholder_starting_at(&self, pos: usize) -> Option<(usize, usize)> {
        self.pastes.iter().find_map(|paste| {
            let marker = Self::placeholder_text(paste);
            let end = pos.checked_add(marker.len())?;
            if end > self.buffer.len() || !self.buffer.is_char_boundary(end) {
                return None;
            }
            if self.buffer[pos..end] == marker {
                Some((pos, end))
            } else {
                None
            }
        })
    }

    /// Entfernt jeden gemerkten Paste, dessen Platzhaltertext nicht mehr
    /// unverändert im Puffer vorkommt (siehe Moduldoku "Paste-Platzhalter")
    /// — z. B. weil ein Backspace/Delete ihn nur teilweise getroffen, oder
    /// der Nutzer mitten hineingetippt hat. Wird nach jeder Puffer-Mutation
    /// aufgerufen.
    fn prune_pastes(&mut self) {
        let buffer = &self.buffer;
        self.pastes
            .retain(|paste| buffer.contains(Self::placeholder_text(paste).as_str()));
    }

    /// Ersetzt jeden im Puffer noch vorhandenen Paste-Platzhalter durch
    /// seinen vollständigen, gemerkten Text (siehe Moduldoku
    /// "Paste-Platzhalter"). Wird beim Absenden (Enter) aufgerufen, bevor
    /// [`InputAction::Submit`] zurückgegeben wird — das Modell/die
    /// Befehls-Pipeline sieht dadurch immer den vollen Text, nie den
    /// Platzhalter. `self.buffer` selbst bleibt unverändert.
    fn expand_pastes(&self) -> String {
        if self.pastes.is_empty() {
            return self.buffer.clone();
        }
        let mut result = self.buffer.clone();
        for paste in &self.pastes {
            let marker = Self::placeholder_text(paste);
            if result.contains(marker.as_str()) {
                result = result.replace(marker.as_str(), paste.text.as_str());
            }
        }
        result
    }

    // -- Paste-Burst-Fallback (siehe Moduldoku "Paste-Burst-Fallback") -----

    /// Beendet einen zuvor erkannten, noch nicht kollabierten Paste-Burst.
    ///
    /// # Beschreibung
    /// Ist die seit [`Self::burst_start`] eingefügte Spanne groß genug für
    /// einen Paste-Platzhalter, wird sie aus dem sichtbaren Puffer entfernt
    /// und stattdessen über [`Self::insert_paste`] als Platzhalter wieder
    /// eingefügt (der volle Text bleibt in [`Self::pastes`] erhalten).
    /// Kleine Bursts bleiben unverändert als normaler Text stehen. No-Op,
    /// wenn gerade kein Burst lief oder der Cursor seither (z. B. durch
    /// einen korrigierenden Backspace) vor `burst_start` zurückgefallen ist.
    fn collapse_burst_if_any(&mut self) {
        let Some(start) = self.burst_start.take() else {
            return;
        };
        if start >= self.cursor
            || self.cursor > self.buffer.len()
            || !self.buffer.is_char_boundary(start)
        {
            return;
        }
        let burst_text = self.buffer[start..self.cursor].to_owned();
        let line_count = burst_text.split('\n').count();
        let char_count = burst_text.chars().count();
        if line_count <= PASTE_PLACEHOLDER_LINE_THRESHOLD
            && char_count <= PASTE_PLACEHOLDER_CHAR_THRESHOLD
        {
            // Zu klein für einen Platzhalter — als normaler Text stehen lassen.
            return;
        }
        self.buffer.drain(start..self.cursor);
        self.cursor = start;
        self.insert_paste(&burst_text);
    }

    // -- CSI-Sicherheitsnetz (siehe Moduldoku "CSI-Sicherheitsnetz") --------

    /// Setzt eine laufende CSI-Sammlung mit `key` fort.
    ///
    /// # Beschreibung
    /// Sammelt weitere Zeichen bis zu einem Abschluss-Byte (`A-Za-z`/`~`)
    /// oder bis [`CSI_MAX_LEN`] erreicht ist — dann wird die Sequenz über
    /// [`Self::resolve_csi_sequence`] aufgelöst. Ein Ctrl-modifiziertes
    /// oder ein Nicht-Char-Event mitten in der Sammlung bricht sie ab: die
    /// bisher gesammelten Zeichen gelten dann rückwirkend als tatsächlich
    /// getippter Text (werden normal eingefügt) und `key` selbst wird
    /// danach ganz normal weiterverarbeitet.
    fn continue_csi_collection(&mut self, key: KeyEvent, ctrl: bool, now: Instant) -> InputAction {
        if let KeyCode::Char(ch) = key.code {
            if !ctrl {
                let mut collected = self.csi_collect.take().unwrap_or_default();
                collected.push(ch);
                let is_final = ch.is_ascii_alphabetic() || ch == '~';
                if is_final {
                    return self.resolve_csi_sequence(&collected);
                }
                if collected.len() >= CSI_MAX_LEN {
                    self.insert_str(&collected);
                    return InputAction::Redraw;
                }
                self.csi_collect = Some(collected);
                return InputAction::Redraw;
            }
        }
        // Abbruch: Ctrl-Kombination oder Nicht-Char-Event mitten in der
        // Sammlung — die bisher gesammelten Zeichen waren offenbar
        // tatsächlich getippter Text, nicht Teil einer CSI-Sequenz.
        if let Some(collected) = self.csi_collect.take() {
            self.insert_str(&collected);
        }
        self.handle_key_at(key, now)
    }

    /// Ordnet eine vollständig gesammelte CSI-Sequenz (inklusive
    /// Einleitungszeichen `[`/`O` und Abschluss-Byte, ohne das vorherige
    /// `Esc`) einer Editor-Aktion zu, oder verwirft sie (Maus-Reports,
    /// unbekannte Sequenzen). Markiert in jedem Fall den auslösenden `Esc`
    /// als unbewusst ([`Self::swallowed_escape`]), da eine sauber
    /// abgeschlossene CSI-Sequenz per Definition kein bewusster
    /// Tastendruck war.
    fn resolve_csi_sequence(&mut self, seq: &str) -> InputAction {
        self.swallowed_escape = true;
        match seq {
            "[H" | "[1~" | "[7~" | "OH" => {
                self.move_buffer_start();
                InputAction::Redraw
            }
            "[F" | "[4~" | "[8~" | "OF" => {
                self.move_buffer_end();
                InputAction::Redraw
            }
            "[1;5D" => {
                self.move_word_left();
                InputAction::Redraw
            }
            "[1;5C" => {
                self.move_word_right();
                InputAction::Redraw
            }
            "[3~" => {
                self.delete();
                InputAction::Redraw
            }
            _ => {
                // SGR-Maus-Reports (`[<…M`/`[<…m`) und alle übrigen, hier
                // nicht abgebildeten Sequenzen: verwerfen (kein sichtbarer
                // Puffer-Effekt).
                InputAction::Redraw
            }
        }
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
    use crate::test_support::{TestError, TestResult};
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

    // 5b. delete_current_line: removes only the line the cursor is on and
    // shifts subsequent lines up.
    #[test]
    fn test_delete_current_line_removes_only_that_line_and_shifts_up() {
        let mut ed = InputEditor::new();
        ed.insert_str("erste\nzweite\ndritte");
        // Cursor steht nach insert_str am Ende (Byte 19). Auf Byte 9 bewegen,
        // das liegt mitten in "zweite" (nach "erste\nzwe").
        for _ in 0..10 {
            ed.move_left();
        }
        assert_eq!(ed.cursor(), 9);
        ed.delete_current_line();
        assert_eq!(ed.text(), "erste\ndritte");
        assert_eq!(ed.cursor(), 6); // Anfang der (jetzt zweiten) Zeile "dritte"
    }

    // 5c. delete_current_line on the last line with no trailing newline: only
    // that line's content is removed, the preceding newline is untouched.
    #[test]
    fn test_delete_current_line_on_last_line_with_no_trailing_newline() {
        let mut ed = InputEditor::new();
        ed.insert_str("eins\nzwei");
        // Cursor irgendwo in "zwei" (letzte Zeile, kein abschließendes \n).
        // insert_str lässt den Cursor am Ende (Byte 9); zwei Schritte zurück
        // landen auf Byte 7, mitten in "zwei".
        ed.move_left();
        ed.move_left();
        assert_eq!(ed.cursor(), 7);
        ed.delete_current_line();
        assert_eq!(ed.text(), "eins\n");
        assert_eq!(ed.cursor(), 5); // Anfang der (nun leeren) letzten Zeile
    }

    // 5d. delete_current_line on a single-line buffer clears everything.
    #[test]
    fn test_delete_current_line_single_line_buffer() {
        let mut ed = InputEditor::new();
        ed.insert_str("nur eine zeile");
        // Cursor irgendwo innerhalb der einzigen Zeile positionieren.
        ed.move_left();
        ed.move_left();
        ed.move_left();
        assert_eq!(ed.cursor(), 11);
        ed.delete_current_line();
        assert_eq!(ed.text(), "");
        assert_eq!(ed.cursor(), 0);
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

    // 21. ctrl_k_deletes_only_to_end_of_current_line
    //
    // Emacs-`kill-line`-Semantik: steht der Cursor noch vor dem Zeilenende,
    // löscht Ctrl+K nur bis dahin und lässt den Zeilenumbruch stehen. Steht
    // der Cursor schon am Zeilenende (nichts mehr auf dieser Zeile), löscht
    // ein weiteres Ctrl+K stattdessen den Zeilenumbruch selbst und zieht die
    // nächste Zeile heran — ist der Cursor bereits am Ende des gesamten
    // Puffers (keine weitere Zeile), ist die Operation ein No-Op.
    #[test]
    fn test_ctrl_k_deletes_to_end_of_current_line_without_moving_cursor() {
        let mut ed = InputEditor::new();
        ed.insert_str("before\nafter");
        for _ in 0..5 {
            ed.move_left();
        }
        let cursor = ed.cursor();
        assert_eq!(
            ed.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL)),
            InputAction::Redraw
        );
        assert_eq!(ed.text(), "before\n");
        assert_eq!(ed.cursor(), cursor);

        // Cursor steht jetzt bereits am Ende der (leeren) zweiten Zeile UND
        // am Ende des gesamten Puffers — keine weitere Zeile zum Anhängen,
        // also No-Op.
        ed.move_home();
        assert_eq!(
            ed.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL)),
            InputAction::Redraw
        );
        assert_eq!(ed.text(), "before\n");
        assert_eq!(ed.cursor(), cursor);

        // Gegenprobe für den Join-Fall: Cursor am Ende der ersten Zeile,
        // direkt vor dem Zeilenumbruch, mit einer zweiten Zeile dahinter —
        // Ctrl+K entfernt hier den Zeilenumbruch und zieht "after" heran.
        ed.clear();
        ed.insert_str("before\nafter");
        for _ in 0..6 {
            ed.move_left();
        }
        assert_eq!(ed.cursor(), 6);
        assert_eq!(
            ed.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL)),
            InputAction::Redraw
        );
        assert_eq!(ed.text(), "beforeafter");
        assert_eq!(ed.cursor(), 6);
    }

    // 22. utf8_boundary_safety
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

    // -----------------------------------------------------------------------
    // Paste-Platzhalter (siehe Moduldoku "Paste-Platzhalter")
    // -----------------------------------------------------------------------

    // 30. small_paste_inserts_inline
    #[test]
    fn test_small_paste_inserts_inline() {
        let mut ed = InputEditor::new();
        ed.insert_paste("line1\nline2");
        assert_eq!(ed.text(), "line1\nline2");
        assert!(ed.pastes().is_empty());
    }

    // Runde 4: Absende-Text löst Platzhalter auf, Ersetzen behält Pastes.
    #[test]
    fn test_submission_text_expands_and_replace_keeps_pastes() {
        let mut editor = InputEditor::new();
        let big: String = (0..40).map(|i| format!("zeile {i}\n")).collect();
        editor.insert_str("bitte prüfen: ");
        editor.insert_paste(&big);
        assert!(editor.text().contains("[Pasted text #"));
        assert!(editor.submission_text().contains("zeile 39"));
        assert!(!editor.submission_text().contains("[Pasted text #"));

        let rewritten = format!("@datei.rs {}", editor.text());
        editor.replace_text_keeping_pastes(&rewritten, 9);
        assert!(
            editor
                .submission_text()
                .starts_with("@datei.rs bitte prüfen: ")
        );
        assert!(editor.submission_text().contains("zeile 39"));
    }

    // 31. large_paste_becomes_placeholder
    #[test]
    fn test_large_paste_becomes_placeholder() {
        let mut ed = InputEditor::new();
        let big = "line\n".repeat(10); // 11 Zeilen nach split('\n') > Schwelle 3
        ed.insert_paste(&big);
        assert_eq!(ed.text(), "[Pasted text #1 +11 lines]");
        assert_eq!(ed.pastes().len(), 1);
        assert_eq!(ed.pastes()[0].id, 1);
        assert_eq!(ed.pastes()[0].text, big);
        assert_eq!(ed.pastes()[0].line_count, 11);
    }

    // 32. submit_expands_pending_paste
    #[test]
    fn test_submit_expands_pending_paste() -> TestResult {
        let mut ed = InputEditor::new();
        let big = "line\n".repeat(10);
        ed.insert_paste(&big);
        ed.insert_str("tail");
        let action = ed.handle_key_at(key(KeyCode::Enter), Instant::now());
        match action {
            InputAction::Submit(text) => {
                assert!(text.contains(&big), "expandierter Text fehlt: {text:?}");
                assert!(text.ends_with("tail"));
                assert!(!text.contains("[Pasted text"));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Submit, got {other:?}"
                )));
            }
        }
        assert!(ed.pastes().is_empty());
        assert!(ed.is_empty());
        Ok(())
    }

    // 33. backspace_and_delete_remove_placeholder_atomically_and_prune
    #[test]
    fn test_backspace_and_delete_remove_placeholder_atomically_and_prune() {
        let mut ed = InputEditor::new();
        let big = "line\n".repeat(10);
        ed.insert_paste(&big);
        assert_eq!(ed.pastes().len(), 1);
        // Cursor steht direkt hinter dem Platzhalter — ein Backspace entfernt
        // ihn als Ganzes (nicht Zeichen für Zeichen).
        ed.backspace();
        assert_eq!(ed.text(), "");
        assert!(
            ed.pastes().is_empty(),
            "prune_pastes muss den Eintrag entfernen"
        );

        ed.insert_paste(&big);
        ed.move_home();
        // Cursor steht direkt vor dem Platzhalter — ein Delete entfernt ihn
        // ebenfalls als Ganzes.
        ed.delete();
        assert_eq!(ed.text(), "");
        assert!(ed.pastes().is_empty());
    }

    // 34. ctrl_backspace_and_ctrl_w_remove_placeholder_as_one_word
    #[test]
    fn test_ctrl_backspace_and_ctrl_w_remove_placeholder_as_one_word() {
        let mut ed = InputEditor::new();
        let big = "line\n".repeat(10);
        ed.insert_paste(&big);
        let action = ed.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL));
        assert_eq!(action, InputAction::Redraw);
        assert_eq!(ed.text(), "");
        assert!(ed.pastes().is_empty());

        ed.insert_paste(&big);
        let action = ed.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(action, InputAction::Redraw);
        assert_eq!(ed.text(), "");
        assert!(ed.pastes().is_empty());
    }

    // 35. burst_fallback_collapses_into_placeholder_after_burst_ends
    //
    // Während des Bursts bleibt jedes Zeichen sofort im sichtbaren Puffer
    // (siehe Moduldoku "Paste-Burst-Fallback" und Test 27 oben) — erst ein
    // danach folgendes, deutlich langsameres Zeichen beendet den Burst und
    // kollabiert die seither eingefügte Spanne rückwirkend in einen
    // Platzhalter.
    #[test]
    fn test_burst_fallback_collapses_into_placeholder_after_burst_ends() {
        let mut ed = InputEditor::new();
        let mut t = Instant::now();
        let big = "line\n".repeat(10);
        for ch in big.chars() {
            let key_event = if ch == '\n' {
                key(KeyCode::Enter)
            } else {
                key(KeyCode::Char(ch))
            };
            ed.handle_key_at(key_event, t);
            t += Duration::from_millis(1);
        }
        assert_eq!(ed.text(), big);
        assert!(ed.pastes().is_empty());

        t += Duration::from_millis(50); // Ruhephase beendet den Burst.
        let action = ed.handle_key_at(key(KeyCode::Char('x')), t);
        assert_eq!(action, InputAction::Redraw);
        assert!(
            ed.text().contains("[Pasted text #1"),
            "erwartete Platzhalter im Text: {:?}",
            ed.text()
        );
        assert!(ed.text().ends_with('x'));
        assert_eq!(ed.pastes().len(), 1);
        assert!(ed.pastes()[0].text.ends_with("line\n"));
    }

    // -----------------------------------------------------------------------
    // Wortweise Bewegung / Puffer-weite Pos1/Ende (siehe Punkt 5)
    // -----------------------------------------------------------------------

    // 36. ctrl_left_right_moves_by_word_via_handle_key
    #[test]
    fn test_ctrl_left_right_moves_by_word_via_handle_key() {
        let mut ed = InputEditor::new();
        ed.insert_str("hello world");
        let action = ed.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(action, InputAction::Redraw);
        assert_eq!(ed.cursor(), 6);
        let action = ed.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(action, InputAction::Redraw);
        assert_eq!(ed.cursor(), 0);
        let action = ed.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL));
        assert_eq!(action, InputAction::Redraw);
        assert_eq!(ed.cursor(), 6);
    }

    // 37. home_end_on_multiline_buffer_via_handle_key
    //
    // Regressionstest für den ursprünglichen Fehlerbericht: Pos1/Ende
    // springen über `handle_key` an Anfang/Ende der GESAMTEN Eingabe, auch
    // mit Strg; zeilenweise bleibt Strg+A/Strg+E.
    #[test]
    fn test_home_end_on_multiline_buffer_via_handle_key() {
        let mut ed = InputEditor::new();
        ed.insert_str("abc\ndef");
        assert_eq!(ed.cursor(), 7);
        let action = ed.handle_key(key(KeyCode::Home));
        assert_eq!(action, InputAction::Redraw);
        assert_eq!(ed.cursor(), 0); // Anfang der GESAMTEN Eingabe
        let action = ed.handle_key(key(KeyCode::End));
        assert_eq!(action, InputAction::Redraw);
        assert_eq!(ed.cursor(), 7); // Ende der GESAMTEN Eingabe

        let action = ed.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL));
        assert_eq!(action, InputAction::Redraw);
        assert_eq!(ed.cursor(), 0); // Anfang des GESAMTEN Puffers
        let action = ed.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL));
        assert_eq!(action, InputAction::Redraw);
        assert_eq!(ed.cursor(), 7); // Ende des GESAMTEN Puffers
    }

    // -----------------------------------------------------------------------
    // CSI-Sicherheitsnetz (siehe Moduldoku "CSI-Sicherheitsnetz")
    // -----------------------------------------------------------------------

    // 38. sgr_mouse_report_after_esc_is_dropped
    #[test]
    fn test_sgr_mouse_report_after_esc_is_dropped() {
        let mut ed = InputEditor::new();
        let t0 = Instant::now();
        let esc_action = ed.handle_key_at(key(KeyCode::Esc), t0);
        assert_eq!(esc_action, InputAction::Passthrough);
        let mut t = t0;
        for ch in "[<65;22;8M".chars() {
            t += Duration::from_millis(1); // deutlich unter CSI_ESC_WINDOW
            let action = ed.handle_key_at(key(KeyCode::Char(ch)), t);
            assert_eq!(action, InputAction::Redraw);
        }
        // Verworfen: kein Zeichen des Maus-Reports landet im Puffer.
        assert_eq!(ed.text(), "");
    }

    // 39. esc_then_bracket_h_moves_home
    #[test]
    fn test_esc_then_bracket_h_moves_home() {
        let mut ed = InputEditor::new();
        ed.insert_str("hello");
        ed.move_left();
        ed.move_left();
        let cursor_before = ed.cursor();
        assert!(cursor_before > 0 && cursor_before < 5);

        let t0 = Instant::now();
        let esc_action = ed.handle_key_at(key(KeyCode::Esc), t0);
        assert_eq!(esc_action, InputAction::Passthrough);
        let bracket_action =
            ed.handle_key_at(key(KeyCode::Char('[')), t0 + Duration::from_millis(2));
        assert_eq!(bracket_action, InputAction::Redraw);
        let h_action = ed.handle_key_at(key(KeyCode::Char('H')), t0 + Duration::from_millis(3));
        assert_eq!(h_action, InputAction::Redraw);

        assert_eq!(ed.cursor(), 0);
        assert_eq!(ed.text(), "hello"); // kein Zeichen der Sequenz eingefügt
    }

    // 40. esc_then_bracket_f_moves_end
    #[test]
    fn test_esc_then_bracket_f_moves_end() {
        let mut ed = InputEditor::new();
        ed.insert_str("hello");
        ed.move_home();

        let t0 = Instant::now();
        let esc_action = ed.handle_key_at(key(KeyCode::Esc), t0);
        assert_eq!(esc_action, InputAction::Passthrough);
        let bracket_action =
            ed.handle_key_at(key(KeyCode::Char('[')), t0 + Duration::from_millis(2));
        assert_eq!(bracket_action, InputAction::Redraw);
        let f_action = ed.handle_key_at(key(KeyCode::Char('F')), t0 + Duration::from_millis(3));
        assert_eq!(f_action, InputAction::Redraw);

        assert_eq!(ed.cursor(), 5);
        assert_eq!(ed.text(), "hello");
    }

    // 41. esc_then_slow_char_inserts_normally
    //
    // Nach Ablauf von CSI_ESC_WINDOW gilt der Esc als bewusster, abgeschlossener
    // Tastendruck — ein danach getipptes Zeichen wird ganz normal eingefügt.
    #[test]
    fn test_esc_then_slow_char_inserts_normally() {
        let mut ed = InputEditor::new();
        let t0 = Instant::now();
        let esc_action = ed.handle_key_at(key(KeyCode::Esc), t0);
        assert_eq!(esc_action, InputAction::Passthrough);
        let char_action = ed.handle_key_at(key(KeyCode::Char('x')), t0 + Duration::from_millis(50));
        assert_eq!(char_action, InputAction::Redraw);
        assert_eq!(ed.text(), "x");
    }

    // 42. take_swallowed_escape_signals_once_after_resolved_csi
    #[test]
    fn test_take_swallowed_escape_signals_once_after_resolved_csi() {
        let mut ed = InputEditor::new();
        let t0 = Instant::now();
        assert!(!ed.take_swallowed_escape());
        ed.handle_key_at(key(KeyCode::Esc), t0);
        assert!(!ed.take_swallowed_escape(), "noch nicht aufgelöst");
        ed.handle_key_at(key(KeyCode::Char('[')), t0 + Duration::from_millis(1));
        ed.handle_key_at(key(KeyCode::Char('H')), t0 + Duration::from_millis(2));
        assert!(
            ed.take_swallowed_escape(),
            "Esc war Anfang einer aufgelösten CSI-Sequenz"
        );
        assert!(!ed.take_swallowed_escape(), "Einmal-Signal");
    }
}
