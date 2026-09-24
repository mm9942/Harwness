//! Eigenes Freigabefenster für Root-Befehle (`host.sudo_exec`, Runde 5 Teil B).
//!
//! # Verantwortung
//! Zeigt eine [`SudoPrompt`] (exaktes argv, cwd, Worker, Grund) anstelle des
//! Composers, nimmt das sudo-Passwort maskiert entgegen und beantwortet die
//! Frage mit genau einer [`SudoAnswer`] — oder lehnt ab. Hält außerdem das
//! optionale Sitzungs-Merken des Passworts.
//!
//! # Sicherheitsregeln
//! - **Alles abfangen:** Solange das Fenster offen ist, gehen *alle*
//!   Eingabeereignisse (Tasten, Pastes, Maus) an [`SudoUi::handle_event`].
//!   Nichts erreicht Composer, Busy-Warteschlange oder Eingabe-Historie
//!   (`remember_input`); die Einbindung in `app.rs` prüft
//!   [`SudoUi::is_open`] deshalb **vor** jedem anderen Zweig.
//! - **Maskiert ohne Längenhinweis:** angezeigt wird nur `••••••••` (fest)
//!   oder `(leer)`.
//! - **Genullte Puffer:** der Eingabepuffer ist ein `Zeroizing<Vec<u8>>` mit
//!   fester Kapazität [`SUDO_MAX_SECRET_BYTES`] (nie umkopiert); Backspace
//!   nullt die entfernten Bytes, Esc/Ctrl+C/Ablehnung nullen den ganzen
//!   Puffer. Pastes werden sofort in `Zeroizing<String>` gehüllt.
//! - **Scharf-Verzögerung:** Die ersten [`SUDO_ARMING_DELAY`] zählen nur
//!   Esc/Ctrl+C (Ablehnung); jede andere Taste wird verschluckt.
//! - **Ablehnung ist der Default:** Schließen ohne Antwort (Drop),
//!   Turn-Ende, Esc, Ctrl+C lehnen ab.
//! - **Sitzungs-Merken:** „Für diese Sitzung“ behält eine Kopie des
//!   Passworts als [`SudoSecret`] (genullt beim Drop) höchstens
//!   `[host] sudo_session_minutes`. Gelöscht wird sie bei Fristende (Zeitgeber
//!   und bei jedem Zugriff), bei falschem Passwort (Rückruf aus der
//!   Ausführung), mit der App (`/new`, `/resume`, Beenden). Jeder weitere
//!   Root-Befehl braucht trotzdem eine eigene Freigabe in diesem Fenster.
//!
//! # Nebenläufigkeit
//! [`SudoUi`] lebt exklusiv in der `ChatApp`. Nur der gemerkte Wert liegt in
//! einem `Arc<Mutex<…>>`, weil der Ablauf-Zeitgeber und der Fehler-Rückruf
//! ihn (über `Weak`) löschen können.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use harw_tool_shell::sudo::display_argv;
use harw_tool_shell::{
    SUDO_MAX_SECRET_BYTES, SudoAnswer, SudoAuthFailureHook, SudoPrompt, SudoPromptReceiver,
    SudoSecret,
};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};
use zeroize::{Zeroize, Zeroizing};

use crate::app::{ChatApp, Role};
use crate::sanitize::{sanitize_inline, sanitize_reveal_inline};
use crate::style;
use crate::tui_event::TuiEvent;

/// Scharf-Verzögerung wie beim Freigabe-Panel (`APPROVAL_ARMING_DELAY`).
pub(crate) const SUDO_ARMING_DELAY: Duration = Duration::from_millis(700);

/// Vorgabe der Merkfrist, falls die App ohne Konfiguration gebaut wird.
pub(crate) const DEFAULT_SUDO_SESSION: Duration = Duration::from_secs(10 * 60);

/// Feste Maske (kein Längenhinweis).
const MASK: &str = "••••••••";

/// Höchsthöhe des Fensters in Zeilen.
const MAX_DIALOG_HEIGHT: u16 = 18;

/// Mindesthöhe des Fensters in Zeilen.
const MIN_DIALOG_HEIGHT: u16 = 7;

// ── Passwortpuffer ────────────────────────────────────────────────────────────

/// Eingabepuffer mit fester Kapazität; nie umkopiert, immer genullt.
struct PasswordBuffer(Zeroizing<Vec<u8>>);

impl PasswordBuffer {
    fn new() -> Self {
        Self(Zeroizing::new(Vec::with_capacity(SUDO_MAX_SECRET_BYTES)))
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Hängt ein Zeichen an. `false` bei Steuerzeichen oder vollem Puffer.
    fn push_char(&mut self, c: char) -> bool {
        if c.is_control() {
            return false;
        }
        let mut utf8 = [0_u8; 4];
        let encoded = c.encode_utf8(&mut utf8).len();
        let fits = self.0.len() + encoded <= SUDO_MAX_SECRET_BYTES;
        if fits {
            self.0.extend_from_slice(&utf8[..encoded]);
        }
        utf8.zeroize();
        fits
    }

    /// Hängt Text ganz oder gar nicht an. `false` bei Steuerzeichen oder zu
    /// langem Text.
    fn push_str(&mut self, text: &str) -> bool {
        if text.chars().any(char::is_control) || self.0.len() + text.len() > SUDO_MAX_SECRET_BYTES {
            return false;
        }
        self.0.extend_from_slice(text.as_bytes());
        true
    }

    /// Entfernt das letzte Zeichen und nullt dessen Bytes.
    fn backspace(&mut self) {
        let start = last_char_start(&self.0);
        zero_from(&mut self.0, start);
        self.0.truncate(start);
    }

    /// Nullt den ganzen Puffer (Inhalt und Reservekapazität).
    fn clear(&mut self) {
        self.0.zeroize();
    }

    /// Übergibt den Inhalt als [`SudoSecret`]; der Puffer ist danach leer.
    fn take_secret(&mut self) -> Result<SudoSecret, harw_tool_shell::SudoSecretError> {
        let bytes = std::mem::take(&mut *self.0);
        SudoSecret::from_zeroizing(Zeroizing::new(bytes))
    }

    /// Nur Tests: Länge in Bytes.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.len()
    }
}

/// Beginn des letzten UTF-8-Zeichens in `bytes` (0 für leer).
fn last_char_start(bytes: &[u8]) -> usize {
    let mut index = bytes.len();
    while index > 0 {
        index -= 1;
        if bytes[index] & 0b1100_0000 != 0b1000_0000 {
            return index;
        }
    }
    0
}

/// Überschreibt `bytes[start..]` mit Nullen.
fn zero_from(bytes: &mut [u8], start: usize) {
    if let Some(tail) = bytes.get_mut(start..) {
        tail.zeroize();
    }
}

// ── Sitzungs-Merken ───────────────────────────────────────────────────────────

/// Ein gemerktes Sitzungspasswort.
struct Remembered {
    secret: SudoSecret,
    expires_at: Instant,
    generation: u64,
}

type RememberedSlot = Arc<Mutex<Option<Remembered>>>;

/// Führt `f` auf dem gemerkten Wert aus; eine vergiftete Sperre wird
/// übernommen, damit das Löschen nie ausfällt.
fn with_slot<R>(
    slot: &Mutex<Option<Remembered>>,
    f: impl FnOnce(&mut Option<Remembered>) -> R,
) -> R {
    let mut guard = slot.lock().unwrap_or_else(PoisonError::into_inner);
    f(&mut guard)
}

/// Löscht den gemerkten Wert, sofern er noch zur Generation `generation`
/// gehört (ein neuerer Wert bleibt unberührt).
fn clear_generation(slot: &Weak<Mutex<Option<Remembered>>>, generation: u64) {
    if let Some(slot) = slot.upgrade() {
        with_slot(&slot, |remembered| {
            if remembered
                .as_ref()
                .is_some_and(|entry| entry.generation == generation)
            {
                // Drop → `SudoSecret` wird genullt.
                *remembered = None;
            }
        });
    }
}

// ── Fensterzustand ────────────────────────────────────────────────────────────

/// Welche Form das Fenster hat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DialogKind {
    /// Passwortloses sudo: „Freigeben / Ablehnen“.
    Passwordless,
    /// Gemerktes Sitzungspasswort: „Freigeben / Ablehnen“.
    RememberedSession,
    /// Passworteingabe mit „Einmalig“ / „Für diese Sitzung“.
    Password,
}

/// Ein offenes Fenster.
struct OpenDialog {
    prompt: SudoPrompt,
    shown_at: Instant,
    kind: DialogKind,
    input: PasswordBuffer,
    selected: usize,
    notice: Option<&'static str>,
}

impl OpenDialog {
    /// Beschriftungen der wählbaren Optionen.
    fn options(&self, session_allowed: bool) -> Vec<&'static str> {
        match self.kind {
            DialogKind::Passwordless => vec!["Freigeben", "Ablehnen"],
            DialogKind::RememberedSession => {
                vec!["Freigeben (gemerktes Sitzungspasswort)", "Ablehnen"]
            }
            DialogKind::Password if session_allowed => vec!["Einmalig", "Für diese Sitzung"],
            DialogKind::Password => vec!["Einmalig"],
        }
    }
}

/// Was ein Ereignis bewirkt hat.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct SudoEventOutcome {
    /// Neu zeichnen.
    pub(crate) redraw: bool,
    /// Systemzeile für den Verlauf (nie mit Geheimnis).
    pub(crate) notice: Option<String>,
}

impl SudoEventOutcome {
    fn redraw() -> Self {
        Self {
            redraw: true,
            notice: None,
        }
    }

    fn closed(notice: String) -> Self {
        Self {
            redraw: true,
            notice: Some(notice),
        }
    }
}

/// Zustand des sudo-Fensters in der `ChatApp`.
pub(crate) struct SudoUi {
    receiver: Option<SudoPromptReceiver>,
    open: Option<OpenDialog>,
    remembered: RememberedSlot,
    session_ttl: Duration,
    next_generation: u64,
}

impl Default for SudoUi {
    fn default() -> Self {
        Self::new(None, DEFAULT_SUDO_SESSION)
    }
}

impl fmt::Debug for SudoUi {
    /// Nur Zustandskennzeichen — nie Eingabe oder gemerktes Passwort.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SudoUi")
            .field("listening", &self.receiver.is_some())
            .field("open", &self.open.is_some())
            .field("session_ttl", &self.session_ttl)
            .finish_non_exhaustive()
    }
}

impl Drop for SudoUi {
    /// Mit der App (Sitzungsende, `/new`, `/resume`, Beenden) gehen ein
    /// offenes Fenster (Ablehnung, Puffer genullt) und ein gemerktes
    /// Passwort (genullt) ausdrücklich verloren — unabhängig davon, ob noch
    /// ein Zeitgeber oder Rückruf (nur `Weak`) aussteht.
    fn drop(&mut self) {
        let _ = self.deny_open();
        self.forget_remembered();
    }
}

impl SudoUi {
    /// Baut den Zustand.
    ///
    /// # Argumente
    /// - `receiver`: Empfänger des sudo-Fragekanals (nur TUI-Montage).
    /// - `session_ttl`: Merkfrist; `Duration::ZERO` schaltet „Für diese
    ///   Sitzung“ ab.
    pub(crate) fn new(receiver: Option<SudoPromptReceiver>, session_ttl: Duration) -> Self {
        Self {
            receiver,
            open: None,
            remembered: Arc::new(Mutex::new(None)),
            session_ttl,
            next_generation: 0,
        }
    }

    /// `true`, solange ein Fragekanal gepollt werden soll.
    pub(crate) fn is_listening(&self) -> bool {
        self.receiver.is_some()
    }

    /// Wartet auf die nächste Frage; ohne Kanal nie fertig.
    pub(crate) async fn recv(&mut self) -> Option<SudoPrompt> {
        match self.receiver.as_mut() {
            Some(receiver) => receiver.recv().await,
            None => std::future::pending().await,
        }
    }

    /// Runde 5, Teil K: nicht blockierendes Abholen einer Frage — für den
    /// Leerlauf der TUI, wenn ein Hintergrund-Agent sudo anfragt.
    ///
    /// # Returns
    /// `Some(frage)`, wenn eine wartet; `None` ohne Frage. Ein geschlossener
    /// Kanal wird dabei abgemeldet (wie [`Self::channel_closed`]).
    pub(crate) fn try_recv(&mut self) -> Option<SudoPrompt> {
        let receiver = self.receiver.as_mut()?;
        match receiver.try_recv() {
            Ok(prompt) => Some(prompt),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => None,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                self.receiver = None;
                None
            }
        }
    }

    /// Der Kanal ist zu; nicht mehr pollen.
    pub(crate) fn channel_closed(&mut self) {
        self.receiver = None;
    }

    /// `true`, solange ein Fenster offen ist (dann gehört ihm jede Eingabe).
    pub(crate) fn is_open(&self) -> bool {
        self.open.is_some()
    }

    fn session_allowed(&self) -> bool {
        !self.session_ttl.is_zero()
    }

    /// `true`, wenn ein gültiges Sitzungspasswort gemerkt ist; ein
    /// abgelaufenes wird dabei gelöscht.
    fn has_valid_remembered(&self, now: Instant) -> bool {
        with_slot(&self.remembered, |remembered| {
            if remembered
                .as_ref()
                .is_some_and(|entry| entry.expires_at <= now)
            {
                *remembered = None;
            }
            remembered.is_some()
        })
    }

    /// Kopie des gemerkten Passworts samt Generation, falls noch gültig.
    fn remembered_copy(&self, now: Instant) -> Option<(SudoSecret, u64)> {
        with_slot(&self.remembered, |remembered| {
            if remembered
                .as_ref()
                .is_some_and(|entry| entry.expires_at <= now)
            {
                *remembered = None;
            }
            remembered
                .as_ref()
                .map(|entry| (entry.secret.try_clone(), entry.generation))
        })
    }

    /// Merkt `secret` bis `now + session_ttl` und startet den Zeitgeber.
    fn remember(&mut self, secret: SudoSecret, now: Instant) -> u64 {
        self.next_generation = self.next_generation.wrapping_add(1);
        let generation = self.next_generation;
        let expires_at = now + self.session_ttl;
        with_slot(&self.remembered, |remembered| {
            *remembered = Some(Remembered {
                secret,
                expires_at,
                generation,
            });
        });
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let weak = Arc::downgrade(&self.remembered);
            let ttl = self.session_ttl;
            handle.spawn(async move {
                tokio::time::sleep(ttl).await;
                clear_generation(&weak, generation);
            });
        }
        generation
    }

    /// Rückruf für die Ausführung: löscht die Generation bei falschem
    /// Passwort.
    fn failure_hook(&self, generation: u64) -> SudoAuthFailureHook {
        let weak = Arc::downgrade(&self.remembered);
        SudoAuthFailureHook::new(move || clear_generation(&weak, generation))
    }

    /// Löscht ein gemerktes Sitzungspasswort sofort.
    pub(crate) fn forget_remembered(&self) {
        with_slot(&self.remembered, |remembered| *remembered = None);
    }

    /// Öffnet ein Fenster für `prompt`. Eine noch offene ältere Frage wird
    /// abgelehnt (nie still überschrieben).
    ///
    /// # Rückgabe
    /// Systemzeile, falls eine ältere Frage abgelehnt wurde.
    pub(crate) fn open(&mut self, prompt: SudoPrompt, now: Instant) -> Option<String> {
        let stale = self.deny_open();
        let kind = if prompt.passwordless() {
            DialogKind::Passwordless
        } else if self.has_valid_remembered(now) {
            DialogKind::RememberedSession
        } else {
            DialogKind::Password
        };
        // „Ablehnen“ ist bei reiner Freigabe vorausgewählt.
        let selected = match kind {
            DialogKind::Passwordless | DialogKind::RememberedSession => 1,
            DialogKind::Password => 0,
        };
        self.open = Some(OpenDialog {
            prompt,
            shown_at: now,
            kind,
            input: PasswordBuffer::new(),
            selected,
            notice: None,
        });
        stale
    }

    /// Lehnt ein offenes Fenster ab und nullt den Puffer.
    ///
    /// # Rückgabe
    /// Systemzeile, falls ein Fenster offen war.
    pub(crate) fn deny_open(&mut self) -> Option<String> {
        let mut open = self.open.take()?;
        open.input.clear();
        let argv = display_argv(open.prompt.argv());
        open.prompt.deny();
        Some(format!("sudo-Anfrage abgelehnt: {argv}"))
    }

    /// Verarbeitet **jedes** Eingabeereignis, solange das Fenster offen ist.
    pub(crate) fn handle_event(&mut self, event: TuiEvent, now: Instant) -> SudoEventOutcome {
        match event {
            TuiEvent::Key(key) => self.handle_key(key, now),
            TuiEvent::Paste(text) => self.handle_paste(Zeroizing::new(text), now),
            TuiEvent::Resize(..) | TuiEvent::Draw => SudoEventOutcome::redraw(),
            // Maus (Scrollen, Klicks) wird verschluckt.
            TuiEvent::Mouse(_) => SudoEventOutcome::default(),
        }
    }

    fn deny_outcome(&mut self) -> SudoEventOutcome {
        match self.deny_open() {
            Some(notice) => SudoEventOutcome::closed(notice),
            None => SudoEventOutcome::default(),
        }
    }

    fn handle_key(&mut self, key: KeyEvent, now: Instant) -> SudoEventOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // Esc und Ctrl+C lehnen immer ab — auch vor der Scharfschaltung.
        if key.code == KeyCode::Esc || (ctrl && matches!(key.code, KeyCode::Char('c' | 'C'))) {
            return self.deny_outcome();
        }
        let session_allowed = self.session_allowed();
        let Some(open) = self.open.as_mut() else {
            return SudoEventOutcome::default();
        };
        if now.saturating_duration_since(open.shown_at) < SUDO_ARMING_DELAY {
            return SudoEventOutcome::default();
        }
        let option_count = open.options(session_allowed).len();
        match key.code {
            KeyCode::Enter if key.kind != KeyEventKind::Repeat => self.confirm(now),
            KeyCode::Tab
            | KeyCode::BackTab
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Up
            | KeyCode::Down => {
                open.selected = (open.selected + 1) % option_count.max(1);
                SudoEventOutcome::redraw()
            }
            KeyCode::Backspace if open.kind == DialogKind::Password => {
                open.input.backspace();
                open.notice = None;
                SudoEventOutcome::redraw()
            }
            KeyCode::Char('u' | 'U') if ctrl && open.kind == DialogKind::Password => {
                open.input.clear();
                open.notice = None;
                SudoEventOutcome::redraw()
            }
            KeyCode::Char(c) if open.kind == DialogKind::Password && !ctrl && !alt => {
                open.notice = (!open.input.push_char(c)).then_some("Passwort zu lang");
                SudoEventOutcome::redraw()
            }
            // Alles andere wird verschluckt.
            _ => SudoEventOutcome::default(),
        }
    }

    fn handle_paste(&mut self, text: Zeroizing<String>, now: Instant) -> SudoEventOutcome {
        let Some(open) = self.open.as_mut() else {
            return SudoEventOutcome::default();
        };
        if now.saturating_duration_since(open.shown_at) < SUDO_ARMING_DELAY
            || open.kind != DialogKind::Password
        {
            return SudoEventOutcome::default();
        }
        let trimmed = text.trim_end_matches(['\r', '\n']);
        open.notice = (!open.input.push_str(trimmed))
            .then_some("Einfügen abgelehnt (Steuerzeichen oder zu lang)");
        SudoEventOutcome::redraw()
        // `text` wird hier genullt (Drop).
    }

    /// Enter: die gewählte Option ausführen.
    fn confirm(&mut self, now: Instant) -> SudoEventOutcome {
        let session_allowed = self.session_allowed();
        let Some(mut open) = self.open.take() else {
            return SudoEventOutcome::default();
        };
        let argv = display_argv(open.prompt.argv());
        match open.kind {
            DialogKind::Passwordless => {
                if open.selected != 0 {
                    open.prompt.deny();
                    return SudoEventOutcome::closed(format!("sudo-Anfrage abgelehnt: {argv}"));
                }
                open.prompt.approve(SudoAnswer::ApproveOnce);
                SudoEventOutcome::closed(format!("sudo freigegeben (passwortlos): {argv}"))
            }
            DialogKind::RememberedSession => {
                if open.selected != 0 {
                    open.prompt.deny();
                    return SudoEventOutcome::closed(format!("sudo-Anfrage abgelehnt: {argv}"));
                }
                match self.remembered_copy(now) {
                    Some((secret, generation)) => {
                        let hook = self.failure_hook(generation);
                        open.prompt
                            .approve_with_failure_hook(SudoAnswer::ApproveSession { secret }, hook);
                        SudoEventOutcome::closed(format!(
                            "sudo freigegeben (Sitzungspasswort): {argv}"
                        ))
                    }
                    None => {
                        // Inzwischen abgelaufen: zurück zur Passworteingabe.
                        open.kind = DialogKind::Password;
                        open.selected = 0;
                        open.notice = Some("Sitzungspasswort abgelaufen — bitte Passwort eingeben");
                        self.open = Some(open);
                        SudoEventOutcome::redraw()
                    }
                }
            }
            DialogKind::Password => {
                if open.input.is_empty() {
                    open.notice = Some("Bitte Passwort eingeben (Esc lehnt ab)");
                    self.open = Some(open);
                    return SudoEventOutcome::redraw();
                }
                let secret = match open.input.take_secret() {
                    Ok(secret) => secret,
                    Err(_) => {
                        open.input.clear();
                        open.notice = Some("Passwort ungültig — bitte erneut eingeben");
                        self.open = Some(open);
                        return SudoEventOutcome::redraw();
                    }
                };
                if session_allowed && open.selected == 1 {
                    let generation = self.remember(secret.try_clone(), now);
                    let hook = self.failure_hook(generation);
                    open.prompt.approve_with_failure_hook(
                        SudoAnswer::Password {
                            secret,
                            remember: true,
                        },
                        hook,
                    );
                    SudoEventOutcome::closed(format!(
                        "sudo freigegeben (für diese Sitzung gemerkt): {argv}"
                    ))
                } else {
                    open.prompt.approve(SudoAnswer::Password {
                        secret,
                        remember: false,
                    });
                    SudoEventOutcome::closed(format!("sudo freigegeben (einmalig): {argv}"))
                }
            }
        }
    }

    /// Baut die Zeilen des offenen Fensters.
    fn lines(&self, theme: style::Theme, now: Instant) -> Option<Vec<Line<'static>>> {
        let open = self.open.as_ref()?;
        let prompt = &open.prompt;
        let dim = style::dim_style(theme);
        let mut lines = vec![
            Line::styled(
                format!(
                    "Worker: {} · Sitzung: {}",
                    sanitize_inline(prompt.worker()),
                    sanitize_inline(prompt.session())
                ),
                dim,
            ),
            Line::from(format!(
                "Verzeichnis: {}",
                sanitize_reveal_inline(&prompt.cwd().display().to_string())
            )),
            Line::from(vec![
                Span::raw("Befehl (als root): "),
                Span::styled(
                    sanitize_reveal_inline(&display_argv(prompt.argv())),
                    style::warning_style(theme),
                ),
            ]),
            Line::from(format!(
                "Grund: {}",
                sanitize_reveal_inline(prompt.reason())
            )),
            Line::from(""),
        ];
        match open.kind {
            DialogKind::Passwordless => lines.push(Line::styled(
                "Passwortloses sudo — keine Passworteingabe nötig.",
                dim,
            )),
            DialogKind::RememberedSession => lines.push(Line::styled(
                "Gemerktes Sitzungspasswort wird verwendet (jeder Befehl braucht diese Freigabe).",
                dim,
            )),
            DialogKind::Password => {
                let shown = if open.input.is_empty() {
                    "(leer)"
                } else {
                    MASK
                };
                lines.push(Line::from(format!("Passwort: {shown}")));
            }
        }
        let mut options: Vec<Span<'static>> = Vec::new();
        for (index, label) in open.options(self.session_allowed()).into_iter().enumerate() {
            if index > 0 {
                options.push(Span::raw("   "));
            }
            let style = if index == open.selected {
                style::selected_style(theme)
            } else {
                Style::default()
            };
            let marker = if index == open.selected { "❯ " } else { "  " };
            options.push(Span::styled(format!("{marker}[{label}]"), style));
        }
        lines.push(Line::from(options));
        if let Some(notice) = open.notice {
            lines.push(Line::styled(notice, style::warning_style(theme)));
        }
        let footer = if now.saturating_duration_since(open.shown_at) < SUDO_ARMING_DELAY {
            "Fenster wird gleich scharf … · Esc/Ctrl+C lehnt ab"
        } else {
            "Enter bestätigen · Tab/←→ wählen · Esc/Ctrl+C ablehnen"
        };
        lines.push(Line::styled(footer, dim));
        Some(lines)
    }

    /// Benötigte Höhe (inkl. Rahmen) oder `None` ohne offenes Fenster.
    pub(crate) fn desired_height(&self, width: u16, theme: style::Theme) -> Option<u16> {
        let lines = self.lines(theme, Instant::now())?;
        let inner_width = width.saturating_sub(2).max(1);
        let rows = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .line_count(inner_width);
        let rows = u16::try_from(rows).unwrap_or(u16::MAX).saturating_add(2);
        Some(rows.clamp(MIN_DIALOG_HEIGHT, MAX_DIALOG_HEIGHT))
    }

    /// Zeichnet das Fenster anstelle des Composers.
    ///
    /// # Rückgabe
    /// `true`, wenn ein Fenster gezeichnet wurde.
    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer, theme: style::Theme) -> bool {
        let Some(lines) = self.lines(theme, Instant::now()) else {
            return false;
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(style::warning_style(theme))
            .title(" sudo-Freigabe · Root-Befehl ");
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block)
            .render(area, buf);
        true
    }
}

// ── Einbindung in die ChatApp ─────────────────────────────────────────────────

/// Nimmt eine neue Frage (oder das Kanalende) aus dem `select!` entgegen.
///
/// # Rückgabe
/// `true`, wenn neu gezeichnet werden soll.
pub(crate) fn accept_prompt(app: &mut ChatApp, maybe_prompt: Option<SudoPrompt>) -> bool {
    match maybe_prompt {
        Some(prompt) => {
            tracing::info!(
                session = prompt.session(),
                worker = prompt.worker(),
                passwordless = prompt.passwordless(),
                "tui.sudo.prompt_shown"
            );
            if let Some(notice) = app.sudo.open(prompt, Instant::now()) {
                tracing::warn!("tui.sudo.stale_prompt_closed");
                app.push_line(Role::System, notice);
            }
            true
        }
        None => {
            tracing::warn!("tui.sudo.prompt_channel_ended");
            app.sudo.channel_closed();
            false
        }
    }
}

/// Leitet ein Eingabeereignis an das offene Fenster (die Einbindung ruft
/// das nur bei [`SudoUi::is_open`] und **vor** jedem anderen Zweig auf).
///
/// # Rückgabe
/// `true`, wenn neu gezeichnet werden soll.
pub(crate) fn route_event(app: &mut ChatApp, event: TuiEvent) -> bool {
    let outcome = app.sudo.handle_event(event, Instant::now());
    if let Some(notice) = outcome.notice {
        app.push_line(Role::System, notice);
    }
    outcome.redraw
}

/// Lehnt ein offenes Fenster ab (Turn-Ende, Leerlauf).
///
/// # Rückgabe
/// `true`, wenn ein Fenster offen war.
pub(crate) fn deny_open(app: &mut ChatApp) -> bool {
    match app.sudo.deny_open() {
        Some(notice) => {
            app.push_line(Role::System, notice);
            true
        }
        None => false,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use harw_tool_shell::sudo_prompt_channel;
    use std::path::PathBuf;

    const PASSWORD: &str = "geheim-Ä1";

    fn prompt(passwordless: bool) -> (SudoPrompt, harw_tool_shell::sudo::SudoAnswerReceiver) {
        SudoPrompt::new(
            "s1".to_owned(),
            "uia-shell-worker".to_owned(),
            vec![
                "apt-get".to_owned(),
                "install".to_owned(),
                "ripgrep".to_owned(),
            ],
            PathBuf::from("/workspace"),
            "Werkzeug fehlt".to_owned(),
            passwordless,
        )
    }

    fn key(code: KeyCode) -> TuiEvent {
        TuiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(c: char) -> TuiEvent {
        TuiEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    /// Ein bereits scharfes Fenster: `shown_at` liegt vor der Verzögerung.
    fn armed_now(shown: Instant) -> Instant {
        shown + SUDO_ARMING_DELAY + Duration::from_millis(1)
    }

    fn type_password(ui: &mut SudoUi, now: Instant, text: &str) {
        for c in text.chars() {
            ui.handle_event(key(KeyCode::Char(c)), now);
        }
    }

    #[test]
    fn test_password_buffer_backspace_zeroes_and_keeps_utf8_boundaries() -> TestResult {
        let mut buffer = PasswordBuffer::new();
        assert!(buffer.push_char('a'));
        assert!(buffer.push_char('Ä'));
        assert_eq!(buffer.len(), 3);
        buffer.backspace();
        assert_eq!(buffer.len(), 1);
        assert!(!buffer.push_char('\u{7}'), "Steuerzeichen abgelehnt");
        let capacity = buffer.0.capacity();
        assert!(buffer.push_str("xyz"));
        assert_eq!(buffer.0.capacity(), capacity, "nie umkopiert");
        let mut bytes = vec![1_u8, 2, 3, 4];
        zero_from(&mut bytes, 2);
        assert_eq!(bytes, vec![1, 2, 0, 0]);
        assert_eq!(last_char_start("aÄ".as_bytes()), 1);
        let secret = buffer
            .take_secret()
            .map_err(|_| TestError::Missing("secret"))?;
        assert!(buffer.is_empty());
        assert_eq!(format!("{secret:?}"), "SudoSecret(<redacted>)");
        Ok(())
    }

    #[test]
    fn test_password_buffer_rejects_overflow_without_reallocation() {
        let mut buffer = PasswordBuffer::new();
        let capacity = buffer.0.capacity();
        assert!(buffer.push_str(&"x".repeat(SUDO_MAX_SECRET_BYTES)));
        assert!(!buffer.push_char('y'));
        assert!(!buffer.push_str("y"));
        assert_eq!(buffer.0.capacity(), capacity);
        buffer.clear();
        assert!(buffer.is_empty());
    }

    #[tokio::test]
    async fn test_password_once_delivers_the_secret_and_nothing_is_remembered() -> TestResult {
        let (sudo_prompt, answer) = prompt(false);
        let mut ui = SudoUi::new(None, DEFAULT_SUDO_SESSION);
        let shown = Instant::now();
        assert!(ui.open(sudo_prompt, shown).is_none());
        let now = armed_now(shown);
        type_password(&mut ui, now, PASSWORD);
        let outcome = ui.handle_event(key(KeyCode::Enter), now);
        let notice = outcome.notice.ok_or(TestError::Missing("notice"))?;
        assert!(!notice.contains(PASSWORD), "{notice}");
        assert!(notice.contains("apt-get install ripgrep"), "{notice}");
        assert!(!ui.is_open());
        assert!(!ui.has_valid_remembered(now));
        match answer.into_answer().await {
            Some(SudoAnswer::Password { remember, .. }) => assert!(!remember),
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_session_remembering_skips_the_password_but_still_asks_each_time() -> TestResult {
        let mut ui = SudoUi::new(None, Duration::from_secs(600));
        let shown = Instant::now();
        let (first, first_answer) = prompt(false);
        ui.open(first, shown);
        let now = armed_now(shown);
        type_password(&mut ui, now, PASSWORD);
        ui.handle_event(key(KeyCode::Tab), now);
        ui.handle_event(key(KeyCode::Enter), now);
        assert!(matches!(
            first_answer.into_answer().await,
            Some(SudoAnswer::Password { remember: true, .. })
        ));
        assert!(ui.has_valid_remembered(now));

        // Zweiter Befehl: kein Passwortfeld, aber wieder eine Freigabe
        // („Ablehnen“ vorausgewählt).
        let (second, second_answer) = prompt(false);
        ui.open(second, now);
        assert_eq!(
            ui.open.as_ref().map(|open| open.kind),
            Some(DialogKind::RememberedSession)
        );
        let later = armed_now(now);
        ui.handle_event(key(KeyCode::Tab), later);
        ui.handle_event(key(KeyCode::Enter), later);
        assert!(matches!(
            second_answer.into_answer().await,
            Some(SudoAnswer::ApproveSession { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_remembered_password_expires_and_is_forgotten() {
        let mut ui = SudoUi::new(None, Duration::from_secs(60));
        let now = Instant::now();
        let secret = SudoSecret::from_zeroizing(Zeroizing::new(PASSWORD.as_bytes().to_vec()));
        let Ok(secret) = secret else {
            return;
        };
        ui.remember(secret, now);
        assert!(ui.has_valid_remembered(now));
        assert!(!ui.has_valid_remembered(now + Duration::from_secs(61)));
        assert!(
            with_slot(&ui.remembered, |remembered| remembered.is_none()),
            "abgelaufener Wert wird gelöscht"
        );
    }

    #[test]
    fn test_failure_hook_and_forget_clear_the_remembered_password() {
        let mut ui = SudoUi::new(None, Duration::from_secs(60));
        let now = Instant::now();
        let Ok(secret) = SudoSecret::from_zeroizing(Zeroizing::new(PASSWORD.as_bytes().to_vec()))
        else {
            return;
        };
        let generation = ui.remember(secret, now);
        // Ein Rückruf einer älteren Generation lässt den neuen Wert stehen.
        clear_generation(&Arc::downgrade(&ui.remembered), generation.wrapping_add(7));
        assert!(ui.has_valid_remembered(now));
        clear_generation(&Arc::downgrade(&ui.remembered), generation);
        assert!(!ui.has_valid_remembered(now));

        let Ok(secret) = SudoSecret::from_zeroizing(Zeroizing::new(PASSWORD.as_bytes().to_vec()))
        else {
            return;
        };
        ui.remember(secret, now);
        ui.forget_remembered();
        assert!(!ui.has_valid_remembered(now));
    }

    #[test]
    fn test_session_option_is_hidden_when_ttl_is_zero() {
        let (sudo_prompt, _answer) = prompt(false);
        let mut ui = SudoUi::new(None, Duration::ZERO);
        ui.open(sudo_prompt, Instant::now());
        let options = ui
            .open
            .as_ref()
            .map(|open| open.options(ui.session_allowed()))
            .unwrap_or_default();
        assert_eq!(options, vec!["Einmalig"]);
    }

    #[tokio::test]
    async fn test_escape_and_ctrl_c_deny_and_zero_even_before_arming() -> TestResult {
        for event in [key(KeyCode::Esc), ctrl('c')] {
            let (sudo_prompt, answer) = prompt(false);
            let mut ui = SudoUi::new(None, DEFAULT_SUDO_SESSION);
            let shown = Instant::now();
            ui.open(sudo_prompt, shown);
            type_password(&mut ui, armed_now(shown), "abc");
            let outcome = ui.handle_event(event, shown);
            assert!(outcome.notice.is_some());
            assert!(!ui.is_open());
            assert!(answer.into_answer().await.is_none(), "Ablehnung");
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_keys_before_arming_are_swallowed_not_typed() -> TestResult {
        let (sudo_prompt, answer) = prompt(true);
        let mut ui = SudoUi::new(None, DEFAULT_SUDO_SESSION);
        let shown = Instant::now();
        ui.open(sudo_prompt, shown);
        // Vor der Scharfschaltung: Tab + Enter bewirken nichts.
        ui.handle_event(key(KeyCode::Tab), shown);
        ui.handle_event(key(KeyCode::Enter), shown);
        assert!(ui.is_open());
        // Danach: „Ablehnen“ ist vorausgewählt → Enter lehnt ab.
        ui.handle_event(key(KeyCode::Enter), armed_now(shown));
        assert!(!ui.is_open());
        assert!(answer.into_answer().await.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn test_passwordless_approval_sends_approve_once() -> TestResult {
        let (sudo_prompt, answer) = prompt(true);
        let mut ui = SudoUi::new(None, DEFAULT_SUDO_SESSION);
        let shown = Instant::now();
        ui.open(sudo_prompt, shown);
        let now = armed_now(shown);
        // Buchstaben tippen nichts in einem passwortlosen Fenster.
        type_password(&mut ui, now, "xyz");
        ui.handle_event(key(KeyCode::Left), now);
        ui.handle_event(key(KeyCode::Enter), now);
        assert!(matches!(
            answer.into_answer().await,
            Some(SudoAnswer::ApproveOnce)
        ));
        Ok(())
    }

    #[test]
    fn test_debug_and_render_never_show_the_password() {
        let (sudo_prompt, _answer) = prompt(false);
        let mut ui = SudoUi::new(None, DEFAULT_SUDO_SESSION);
        let shown = Instant::now();
        ui.open(sudo_prompt, shown);
        type_password(&mut ui, armed_now(shown), PASSWORD);
        assert!(!format!("{ui:?}").contains(PASSWORD));
        let theme = style::Theme::Dark;
        let area = Rect::new(0, 0, 80, 14);
        let mut buffer = Buffer::empty(area);
        assert!(ui.render(area, &mut buffer, theme));
        let rendered: String = buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(!rendered.contains("geheim"), "{rendered}");
        assert!(rendered.contains(MASK), "{rendered}");
        assert!(ui.desired_height(80, theme).is_some());
    }

    #[test]
    fn test_channel_state_and_default_ui() {
        let (_sender, receiver) = sudo_prompt_channel();
        let mut ui = SudoUi::new(Some(receiver), DEFAULT_SUDO_SESSION);
        assert!(ui.is_listening());
        ui.channel_closed();
        assert!(!ui.is_listening());
        assert!(!SudoUi::default().is_listening());
        assert!(
            SudoUi::default()
                .desired_height(80, style::Theme::Dark)
                .is_none()
        );
    }
}
