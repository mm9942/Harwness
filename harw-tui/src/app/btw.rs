//! `/btw <frage>` — flüchtige Nebenfrage an das UIA-Modell (Runde 5, Teil L).
//!
//! # Beschreibung
//! Eine schnelle Nebenfrage wie in Claude Code: Sie läuft jederzeit, gerade
//! auch während der Agent arbeitet, unterbricht den laufenden Turn nicht und
//! hinterlässt keine Spuren im Gespräch.
//!
//! - **Aufruf:** genau ein Modellaufruf **ohne Werkzeuge** (leere
//!   Werkzeugliste, keine Turn-Schleife, keine Freigabekette) an das aktive
//!   UIA-Modell der Wurzelsitzung (`active_model`/`active_provider`), über
//!   denselben Wurzel-Provider wie ein Turn.
//! - **Kontext:** ein Schnappschuss ([`BtwSnapshot`]) des Sitzungsverlaufs —
//!   dieselbe Projektion, die ein Turn sendet
//!   ([`ModelRequest::with_context_budget`] mit dem Kontextbudget der
//!   Sitzung: gedeckelt, notfalls die letzten Einträge plus angeheftete
//!   Verdichtungs-Zusammenfassung). Im Leerlauf wird er unmittelbar vor der
//!   Frage genommen, während eines Turns stammt er vom Turn-Beginn (plus der
//!   gerade bearbeiteten Nutzeranfrage). Geheimnis-Redaction: Der
//!   Schnappschuss ist genau das, was der Provider ohnehin bekäme.
//! - **Flüchtig:** Frage und Antwort gehen weder in den Sitzungsverlauf noch
//!   in Transkript/Export, Verdichtung, spätere Turns oder die
//!   Eingabe-Historie des Composers. Die Token-Nutzung zählt als interner
//!   Verbrauch (`AgentEventKind::InternalUsage { purpose: "btw" }`).
//! - **Anzeige:** eine abgesetzte Verlaufszelle ([`BtwCell`], Präfix
//!   „btw ›“, gedimmter Rahmen), die nicht in `export_entries` landet.
//!   Während des Wartens „btw … denkt“; Esc bricht ab, ohne den Haupt-Turn
//!   zu berühren.
//! - **Grenzen:** höchstens eine Nebenfrage gleichzeitig, Zeitlimit
//!   [`BTW_TIMEOUT`], Fehler als Systemzeile.
//!
//! # Nebenläufigkeit
//! Der Modellaufruf läuft als eigener Tokio-Task. Er schreibt sein Ergebnis
//! direkt in die geteilte Zelle (`Arc<Mutex<BtwCell>>`, sichtbar beim
//! nächsten Zeichnen) und meldet den Abschluss über einen Kanal, den
//! [`ChatApp::poll_btw`] an sicheren Stellen der Hauptschleife leert. Ein
//! Frame-Anforderer weckt die Schleife nach dem Ende.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use harw_core::cancel::{CancelReason, CancelToken};
use harw_core::{
    AgentEventHub, AgentSession, ContextBudget, ConversationHistory, ModelProvider, ModelRequest,
    UsageReportingProvider,
};
use harw_extension_api::LoadedInstructions;
use harw_types::{ModelId, ProviderId};
use ratatui::text::{Line, Span};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;

use super::ChatApp;
use crate::frame_requester::FrameRequester;
use crate::history_cell::{AssistantHistoryCell, HistoryCell, truncate_chars};
use crate::sanitize::sanitize_inline;
use crate::style;

/// Zeitlimit einer Nebenfrage.
pub(crate) const BTW_TIMEOUT: Duration = Duration::from_secs(60);

/// Zweck-Kennung der internen Token-Nutzung (`InternalUsage`).
pub(crate) const BTW_USAGE_PURPOSE: &str = "btw";

/// Kurzer Systemhinweis des Nebenfrage-Aufrufs.
pub(crate) const BTW_SYSTEM_PROMPT: &str = "Beantworte eine Nebenfrage knapp anhand des \
     bisherigen Gesprächs. Keine Werkzeuge, keine Aktionen.";

/// Nutzungshinweis für `/btw` ohne Frage.
pub(crate) const BTW_USAGE_HINT: &str = "Nutzung: /btw <frage> — stellt eine flüchtige \
     Nebenfrage zum bisherigen Gespräch, ohne den laufenden Agenten zu unterbrechen. \
     Frage und Antwort kommen nicht in den Verlauf.";

/// Obergrenze der Ausgabe-Tokens einer Nebenantwort.
const BTW_MAX_OUTPUT_TOKENS: u32 = 2048;

/// Angezeigte Höchstlänge der Frage im Zellkopf.
const QUESTION_PREVIEW_CHARS: usize = 100;

/// Höchstlänge der mitgeschickten, gerade laufenden Nutzeranfrage.
const IN_FLIGHT_PREVIEW_CHARS: usize = 2000;

/// `true` für eine `/btw`-Zeile (mit oder ohne Frage).
///
/// # Argumente
/// - `line` (`&str`): abgeschickte Eingabezeile.
pub(crate) fn is_btw_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    let Some(rest) = trimmed.strip_prefix("/btw") else {
        return false;
    };
    rest.is_empty() || rest.starts_with(char::is_whitespace)
}

// ─── Schnappschuss ────────────────────────────────────────────────────────────

/// Gesprächsstand, gegen den eine Nebenfrage beantwortet wird.
#[derive(Debug, Clone)]
pub(super) struct BtwSnapshot {
    /// Kopie des Sitzungsverlaufs (dieselben Items, die ein Turn sendet).
    history: ConversationHistory,
    /// Kontextbudget der Sitzung (deckelt die Projektion).
    budget: ContextBudget,
    /// Aktives UIA-Modell.
    model: Option<ModelId>,
    /// Aktiver UIA-Provider.
    provider: Option<ProviderId>,
    /// Nutzeranfrage des gerade laufenden Turns (noch nicht im Verlauf).
    in_flight: Option<String>,
}

impl BtwSnapshot {
    /// Baut den Schnappschuss aus Einzelteilen.
    pub(super) fn new(
        history: ConversationHistory,
        budget: ContextBudget,
        model: Option<ModelId>,
        provider: Option<ProviderId>,
        in_flight: Option<String>,
    ) -> Self {
        Self {
            history,
            budget,
            model,
            provider,
            in_flight,
        }
    }

    /// Nimmt den Stand der Wurzelsitzung.
    ///
    /// # Argumente
    /// - `session` (`&AgentSession`): die Wurzelsitzung (nur gelesen).
    /// - `in_flight` (`Option<&str>`): Nutzertext des gerade startenden Turns.
    pub(super) fn from_session(session: &AgentSession, in_flight: Option<&str>) -> Self {
        Self::new(
            session.history().clone(),
            session.context_budget(),
            session.active_model().cloned(),
            session.active_provider().cloned(),
            in_flight
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(|text| truncate_chars(text, IN_FLIGHT_PREVIEW_CHARS)),
        )
    }

    /// Baut den werkzeuglosen Modell-Request für `question`.
    ///
    /// # Beschreibung
    /// Kopiert den Verlauf, hängt die Frage (ggf. mit Hinweis auf die gerade
    /// laufende Anfrage) als letzte Nutzernachricht an und projiziert über
    /// [`ModelRequest::with_context_budget`] mit dem Sitzungsbudget. Der
    /// Schnappschuss selbst bleibt unverändert.
    pub(super) fn request(&self, question: &str, cancel: CancelToken) -> ModelRequest {
        let mut history = self.history.clone();
        history.push_user_text(question_prompt(question, self.in_flight.as_deref()));
        ModelRequest::with_context_budget(
            LoadedInstructions {
                system_prompt: BTW_SYSTEM_PROMPT.to_owned(),
                fragments: Vec::new(),
            },
            Vec::new(),
            history,
            // Keine Werkzeuge: `/btw` darf nie etwas tun.
            Vec::new(),
            self.budget,
        )
        .with_model_id(self.model.clone())
        .with_provider_id(self.provider.clone())
        .with_max_output_tokens(Some(BTW_MAX_OUTPUT_TOKENS))
        .with_cancel_token(cancel)
    }
}

/// Text der Nutzernachricht einer Nebenfrage.
fn question_prompt(question: &str, in_flight: Option<&str>) -> String {
    let mut prompt = String::new();
    if let Some(request) = in_flight {
        prompt.push_str(
            "(Hinweis: Der Agent bearbeitet gerade noch diese Anfrage, ihr Ergebnis \
             steht noch aus:)\n",
        );
        prompt.push_str(request);
        prompt.push_str("\n\n");
    }
    prompt.push_str("Nebenfrage (btw), bitte knapp beantworten:\n");
    prompt.push_str(question);
    prompt
}

// ─── Verlaufszelle ────────────────────────────────────────────────────────────

/// Zustand einer [`BtwCell`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BtwPhase {
    /// Die Antwort lädt.
    Thinking,
    /// Die Antwort ist da.
    Answered(String),
    /// Mit Esc abgebrochen.
    Cancelled,
    /// Fehlgeschlagen (Details als Systemzeile).
    Failed,
}

/// Abgesetzte Verlaufszelle einer Nebenfrage — nie Teil des Exports.
#[derive(Debug)]
pub(super) struct BtwCell {
    /// Die Frage (roh; beim Rendern bereinigt).
    question: String,
    /// Aktueller Zustand.
    phase: BtwPhase,
    /// Startzeitpunkt (für die Wartezeit-Anzeige).
    started: Instant,
}

/// Geteilte [`BtwCell`]: der Verlauf hält eine Referenz, der Task die andere.
pub(super) type SharedBtwCell = Arc<Mutex<BtwCell>>;

impl BtwCell {
    fn new(question: &str) -> Self {
        Self {
            question: question.to_owned(),
            phase: BtwPhase::Thinking,
            started: Instant::now(),
        }
    }

    fn display_lines(&self, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
        let frame = style::dim_style(theme);
        let accent = style::assistant_style(theme);
        let question = truncate_chars(
            sanitize_inline(self.question.trim()).as_str(),
            QUESTION_PREVIEW_CHARS,
        );
        let mut lines = vec![Line::from(vec![
            Span::styled("╭─ ", frame),
            Span::styled("btw › ", accent),
            Span::styled(question, frame),
        ])];
        let footer = match &self.phase {
            BtwPhase::Thinking => format!(
                "btw … denkt ({} s, Esc bricht ab)",
                self.started.elapsed().as_secs()
            ),
            BtwPhase::Answered(answer) => {
                // Markdown wie eine Assistenten-Antwort; deren `» `-Präfix
                // (erster Span) weicht dem gedimmten Rahmen.
                let body = AssistantHistoryCell {
                    source: answer.clone(),
                };
                for line in body.display_lines(width.saturating_sub(2).max(1), theme) {
                    let mut spans = vec![Span::styled("│ ", frame)];
                    spans.extend(line.spans.into_iter().skip(1));
                    lines.push(Line::from(spans).style(line.style));
                }
                "Nebenantwort · nicht im Verlauf".to_owned()
            }
            BtwPhase::Cancelled => "btw abgebrochen".to_owned(),
            BtwPhase::Failed => "btw fehlgeschlagen".to_owned(),
        };
        lines.push(Line::from(vec![
            Span::styled("╰─ ", frame),
            Span::styled(footer, frame),
        ]));
        lines
    }
}

impl HistoryCell for SharedBtwCell {
    /// Delegiert an die geteilte [`BtwCell`]; ein vergifteter Lock ergibt
    /// eine Hinweiszeile statt eines Panics.
    fn display_lines(&self, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
        match self.lock() {
            Ok(guard) => guard.display_lines(width, theme),
            Err(_) => vec![Line::from(Span::styled(
                "⚠ btw-Zelle nicht lesbar (Sperre vergiftet)".to_owned(),
                style::warning_style(theme),
            ))],
        }
    }
}

/// Setzt den Zustand der Zelle (vergifteter Lock: still ignoriert).
fn set_phase(cell: &SharedBtwCell, phase: BtwPhase) {
    if let Ok(mut guard) = cell.lock() {
        guard.phase = phase;
    }
}

/// `true`, solange die Zelle auf ihre Antwort wartet.
fn is_thinking(cell: &SharedBtwCell) -> bool {
    cell.lock()
        .map(|guard| guard.phase == BtwPhase::Thinking)
        .unwrap_or(false)
}

// ─── Zustand in ChatApp ───────────────────────────────────────────────────────

/// Abschlussmeldung eines Nebenfrage-Tasks.
#[derive(Debug)]
struct BtwDone {
    /// Kennung des Auftrags.
    id: u64,
    /// Fehlertext, falls der Aufruf scheiterte.
    error: Option<String>,
}

/// Ein laufender Nebenfrage-Auftrag.
#[derive(Debug)]
struct BtwJob {
    id: u64,
    cell: SharedBtwCell,
    cancel: CancelToken,
    handle: JoinHandle<()>,
}

/// Modell-Anbindung einer Nebenfrage: Provider plus optionaler Bus für die
/// interne Nutzungsmeldung.
#[derive(Clone)]
pub(super) struct BtwBackend {
    provider: Arc<dyn ModelProvider>,
    hub: Option<AgentEventHub>,
}

impl BtwBackend {
    /// Baut die Anbindung.
    pub(super) fn new(provider: Arc<dyn ModelProvider>, hub: Option<AgentEventHub>) -> Self {
        Self { provider, hub }
    }
}

/// `/btw`-Zustand der TUI (Feld `ChatApp::btw`).
pub(crate) struct BtwState {
    /// Zuletzt genommener Gesprächsstand.
    snapshot: Option<BtwSnapshot>,
    /// Der (höchstens eine) laufende Auftrag.
    job: Option<BtwJob>,
    tx: UnboundedSender<BtwDone>,
    rx: UnboundedReceiver<BtwDone>,
    /// Weckt die Hauptschleife nach dem Ende eines Auftrags.
    waker: Option<FrameRequester>,
    next_id: u64,
    timeout: Duration,
    /// Feste Anbindung statt der Runtime-Montage (Tests).
    backend_override: Option<BtwBackend>,
}

impl std::fmt::Debug for BtwState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BtwState")
            .field("has_snapshot", &self.snapshot.is_some())
            .field("job", &self.job.as_ref().map(|job| job.id))
            .finish_non_exhaustive()
    }
}

impl Default for BtwState {
    fn default() -> Self {
        Self::new()
    }
}

impl BtwState {
    /// Leerer Zustand mit frischem Kanal.
    pub(crate) fn new() -> Self {
        let (tx, rx) = unbounded_channel();
        Self {
            snapshot: None,
            job: None,
            tx,
            rx,
            waker: None,
            next_id: 0,
            timeout: BTW_TIMEOUT,
            backend_override: None,
        }
    }

    /// Hinterlegt den Frame-Anforderer der Hauptschleife.
    pub(crate) fn set_waker(&mut self, waker: FrameRequester) {
        self.waker = Some(waker);
    }

    /// `true`, solange eine Nebenfrage auf ihre Antwort wartet.
    pub(crate) fn is_pending(&self) -> bool {
        self.job.as_ref().is_some_and(|job| is_thinking(&job.cell))
    }

    /// Nur Tests: feste Modell-Anbindung und Zeitlimit.
    #[cfg(test)]
    pub(super) fn set_backend_for_test(&mut self, backend: BtwBackend, timeout: Duration) {
        self.backend_override = Some(backend);
        self.timeout = timeout;
    }

    /// Setzt den Gesprächsstand direkt.
    pub(super) fn set_snapshot(&mut self, snapshot: BtwSnapshot) {
        self.snapshot = Some(snapshot);
    }

    /// Nur Tests: aktueller Gesprächsstand.
    #[cfg(test)]
    pub(super) fn snapshot(&self) -> Option<&BtwSnapshot> {
        self.snapshot.as_ref()
    }
}

impl ChatApp {
    /// Merkt den Gesprächsstand der Wurzelsitzung für `/btw` vor.
    ///
    /// # Beschreibung
    /// Einhängepunkte: Turn-Beginn (`run_turn_streaming`, mit dem Nutzertext
    /// des Turns) und eine im Leerlauf abgeschickte `/btw`-Zeile (`run_loop`).
    pub(super) fn capture_btw_snapshot(&mut self, session: &AgentSession, in_flight: Option<&str>) {
        self.btw
            .set_snapshot(BtwSnapshot::from_session(session, in_flight));
    }

    /// Modell-Anbindung: Test-Vorgabe, sonst Wurzel-Provider der Montage mit
    /// deren Live-Bus.
    fn btw_backend(&self) -> Option<BtwBackend> {
        if let Some(backend) = &self.btw.backend_override {
            return Some(backend.clone());
        }
        self.runtime()
            .map(|rt| BtwBackend::new(Arc::clone(rt.model()), Some(rt.agent_events().clone())))
    }

    /// Startet eine Nebenfrage (`/btw <frage>`), ohne den Turn zu berühren.
    ///
    /// # Beschreibung
    /// Leere Frage → Nutzungshinweis. Läuft schon eine → Hinweis, keine
    /// zweite. Ohne Gesprächsstand oder Modell-Anbindung → Systemzeile.
    /// Sonst: Zelle „btw … denkt“ anhängen und den werkzeuglosen Aufruf als
    /// Task starten (Zeitlimit [`BTW_TIMEOUT`]). Nichts davon berührt
    /// Sitzungsverlauf, Export oder Eingabe-Historie.
    ///
    /// # Nebenläufigkeit
    /// Muss in einer Tokio-Runtime laufen (`tokio::spawn`).
    pub(super) fn start_btw(&mut self, question: &str) {
        let question = question.trim();
        if question.is_empty() {
            self.push_lines(vec![Line::from(BTW_USAGE_HINT)]);
            return;
        }
        if self.btw.is_pending() {
            self.push_lines(vec![Line::from(
                "Es läuft bereits eine /btw-Nebenfrage — bitte warten oder mit Esc abbrechen.",
            )]);
            return;
        }
        let Some(snapshot) = self.btw.snapshot.as_ref() else {
            self.push_lines(vec![Line::from(
                "⚠ /btw: kein Gesprächsstand verfügbar — bitte nach dem ersten Turn erneut versuchen.",
            )]);
            return;
        };
        let Some(backend) = self.btw_backend() else {
            self.push_lines(vec![Line::from(
                "⚠ /btw: kein Modell angebunden (keine Runtime-Montage).",
            )]);
            return;
        };
        let cancel = CancelToken::new();
        let request = snapshot.request(question, cancel.clone());
        let provider: Arc<dyn ModelProvider> = match backend.hub {
            Some(hub) => Arc::new(UsageReportingProvider::new(
                backend.provider,
                hub,
                self.session_id().clone(),
                BTW_USAGE_PURPOSE,
            )),
            None => backend.provider,
        };

        let cell: SharedBtwCell = Arc::new(Mutex::new(BtwCell::new(question)));
        self.push_cell(Box::new(Arc::clone(&cell)));
        self.btw.next_id = self.btw.next_id.wrapping_add(1);
        let id = self.btw.next_id;
        let tx = self.btw.tx.clone();
        let waker = self.btw.waker.clone();
        let timeout = self.btw.timeout;
        let task_cell = Arc::clone(&cell);
        let task_cancel = cancel.clone();
        tracing::debug!(id, "tui.btw.started");
        let handle = tokio::spawn(async move {
            let outcome = tokio::time::timeout(timeout, provider.respond(request)).await;
            if task_cancel.is_cancelled() {
                return;
            }
            let error = match outcome {
                Err(_) => Some(format!("Zeitlimit überschritten ({} s)", timeout.as_secs())),
                Ok(Err(error)) => Some(error.to_string()),
                Ok(Ok(response)) => {
                    let text = response.message.unwrap_or_default().trim().to_owned();
                    if text.is_empty() {
                        Some("das Modell hat keine Antwort geliefert".to_owned())
                    } else {
                        set_phase(&task_cell, BtwPhase::Answered(text));
                        None
                    }
                }
            };
            if error.is_some() {
                set_phase(&task_cell, BtwPhase::Failed);
            }
            let _ = tx.send(BtwDone { id, error });
            if let Some(waker) = waker {
                waker.schedule_frame();
            }
        });
        self.btw.job = Some(BtwJob {
            id,
            cell,
            cancel,
            handle,
        });
    }

    /// Bricht die laufende Nebenfrage ab (Esc). Der Haupt-Turn bleibt
    /// unberührt.
    ///
    /// # Rückgabe
    /// `true`, wenn eine wartende Nebenfrage abgebrochen wurde.
    pub(super) fn cancel_btw(&mut self) -> bool {
        if !self.btw.is_pending() {
            return false;
        }
        let Some(job) = self.btw.job.take() else {
            return false;
        };
        job.cancel.cancel(CancelReason::User);
        job.handle.abort();
        set_phase(&job.cell, BtwPhase::Cancelled);
        tracing::debug!(id = job.id, "tui.btw.cancelled");
        true
    }

    /// Esc bei wartender Nebenfrage: bricht sie ab, sofern die Taste keinem
    /// Popup, Overlay oder Dialog gehört.
    ///
    /// # Rückgabe
    /// `true`, wenn die Taste verbraucht wurde.
    pub(super) fn btw_esc_cancels(&mut self, key: &KeyEvent) -> bool {
        if key.code != KeyCode::Esc || key.modifiers != KeyModifiers::NONE {
            return false;
        }
        if self.has_popup()
            || self.mention_popup.as_ref().is_some_and(|p| !p.is_empty())
            || self.overlay.is_some()
            || self.pending_approval_dialog.is_some()
            || self.pending_host_permit_dialog.is_some()
        {
            return false;
        }
        self.cancel_btw()
    }

    /// Übernimmt Abschlussmeldungen der Nebenfrage-Tasks.
    ///
    /// # Beschreibung
    /// Die Antwort selbst steht schon in der Zelle; hier wird der Auftrag
    /// freigegeben und ein Fehler als Systemzeile gezeigt.
    ///
    /// # Rückgabe
    /// `true`, wenn sich Sichtbares änderte.
    pub(super) fn poll_btw(&mut self) -> bool {
        let mut changed = false;
        while let Ok(done) = self.btw.rx.try_recv() {
            changed = true;
            if self.btw.job.as_ref().is_some_and(|job| job.id == done.id) {
                self.btw.job = None;
            }
            if let Some(error) = done.error {
                tracing::debug!(id = done.id, "tui.btw.failed");
                self.push_lines(vec![Line::from(format!(
                    "⚠ /btw fehlgeschlagen: {}",
                    sanitize_inline(&error)
                ))]);
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use harw_core::testing::RecordingModelProvider;
    use harw_core::{AgentEventKind, ModelFuture};

    use super::super::tests::test_chat_app;
    use super::super::{BusyKeyOutcome, handle_busy_event, route_busy_command};
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use crate::tui_event::TuiEvent;

    /// Provider, der nie antwortet (für Abbruch und Parallelität).
    struct NeverProvider;

    impl ModelProvider for NeverProvider {
        fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
            Box::pin(std::future::pending())
        }
    }

    fn unlimited_budget() -> ContextBudget {
        ContextBudget {
            max_context_bytes: usize::MAX,
            max_history_bytes: usize::MAX,
        }
    }

    fn snapshot_with_history() -> BtwSnapshot {
        let mut history = ConversationHistory::new();
        history.push_user_text("Bitte baue das Modul X.");
        BtwSnapshot::new(
            history,
            unlimited_budget(),
            Some(ModelId::from("claude-opus-5-5")),
            None,
            Some("Bitte baue das Modul X.".to_owned()),
        )
    }

    fn rendered(app: &ChatApp) -> String {
        app.cells
            .iter()
            .flat_map(|cell| cell.display_lines(120, style::Theme::Dark))
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// App im simulierten Busy-Zustand mit Schnappschuss und Anbindung.
    fn busy_app(
        provider: Arc<dyn ModelProvider>,
        hub: Option<AgentEventHub>,
    ) -> TestResult<(ChatApp, CancelToken)> {
        let mut app = test_chat_app()?;
        let turn_cancel = CancelToken::new();
        app.active_cancel = Some(turn_cancel.clone());
        app.btw.set_snapshot(snapshot_with_history());
        app.btw
            .set_backend_for_test(BtwBackend::new(provider, hub), Duration::from_secs(5));
        Ok((app, turn_cancel))
    }

    /// Wartet, bis der Task seine Abschlussmeldung geschickt hat.
    async fn wait_until_done(app: &mut ChatApp) -> TestResult {
        for _ in 0..200 {
            if app.poll_btw() && !app.btw.is_pending() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Err(TestError::Unexpected("btw-Task wurde nicht fertig".into()))
    }

    #[test]
    fn is_btw_line_matches_only_the_command() {
        assert!(is_btw_line("/btw"));
        assert!(is_btw_line("/btw was macht X?"));
        assert!(is_btw_line("  /btw  frage"));
        assert!(!is_btw_line("/btwx"));
        assert!(!is_btw_line("btw frage"));
        assert!(!is_btw_line("/status"));
    }

    /// `/btw` im Busy-Zustand startet die Nebenfrage sofort, ohne den Turn
    /// abzubrechen oder zurückgestellt zu werden; die Antwort erscheint als
    /// btw-Zelle.
    #[tokio::test]
    async fn btw_while_busy_starts_without_interrupting_the_turn() -> TestResult {
        let recording = Arc::new(RecordingModelProvider::with_response("Modul X nutzt Y."));
        let (mut app, turn_cancel) = busy_app(recording.clone(), None)?;

        let outcome = route_busy_command(&mut app, "/btw welches Modul?".to_owned());
        assert_eq!(outcome, BusyKeyOutcome::Local);
        assert!(!turn_cancel.is_cancelled(), "Turn darf nicht abbrechen");
        assert!(app.deferred_input.is_empty(), "nicht zurückgestellt");
        assert!(app.pending_turns.is_empty(), "kein Chat-Turn");
        assert!(rendered(&app).contains("btw … denkt"));

        wait_until_done(&mut app).await?;
        let text = rendered(&app);
        assert!(text.contains("btw › welches Modul?"), "{text}");
        assert!(text.contains("Modul X nutzt Y."), "{text}");
        assert!(text.contains("Nebenantwort"), "{text}");
        assert!(!turn_cancel.is_cancelled());
        assert_eq!(recording.recorded().len(), 1);
        Ok(())
    }

    /// Der Aufruf bietet keine Werkzeuge an, trägt den Systemhinweis und
    /// schickt Verlauf plus Frage.
    #[tokio::test]
    async fn btw_request_offers_no_tools() -> TestResult {
        let recording = Arc::new(RecordingModelProvider::with_response("ok"));
        let (mut app, _turn) = busy_app(recording.clone(), None)?;
        app.start_btw("was ist Y?");
        wait_until_done(&mut app).await?;

        let request = recording
            .last()
            .ok_or(TestError::Missing("aufgezeichneter Request"))?;
        assert!(request.tools.is_empty(), "keine Werkzeuge");
        assert_eq!(request.system_prompt, BTW_SYSTEM_PROMPT);
        assert_eq!(
            request.model_id.as_ref().map(|id| id.as_str().to_owned()),
            Some("claude-opus-5-5".to_owned())
        );
        let messages = format!("{:?}", request.history.items());
        assert!(messages.contains("Bitte baue das Modul X."), "{messages}");
        assert!(messages.contains("was ist Y?"), "{messages}");
        Ok(())
    }

    /// Verlauf (Schnappschuss), Export und Eingabe-Historie bleiben
    /// unverändert.
    #[tokio::test]
    async fn btw_leaves_history_export_and_input_history_untouched() -> TestResult {
        let recording = Arc::new(RecordingModelProvider::with_response("Antwort"));
        let (mut app, _turn) = busy_app(recording, None)?;
        let history_before = app
            .btw
            .snapshot()
            .map(|snapshot| snapshot.history.len())
            .ok_or(TestError::Missing("Schnappschuss"))?;
        let export_before = app.export_entries.len();
        let input_history_before = app.input.history().len();

        // Derselbe Weg wie ein abgeschicktes Enter im Composer.
        app.remember_input("/btw geheim?");
        let _ = route_busy_command(&mut app, "/btw geheim?".to_owned());
        wait_until_done(&mut app).await?;

        assert_eq!(app.input.history().len(), input_history_before);
        assert_eq!(app.export_entries.len(), export_before);
        let history_after = app
            .btw
            .snapshot()
            .map(|snapshot| snapshot.history.len())
            .ok_or(TestError::Missing("Schnappschuss"))?;
        assert_eq!(history_after, history_before);
        Ok(())
    }

    /// Höchstens eine Nebenfrage gleichzeitig.
    #[tokio::test]
    async fn only_one_btw_runs_at_a_time() -> TestResult {
        let (mut app, _turn) = busy_app(Arc::new(NeverProvider), None)?;
        app.start_btw("erste");
        let cells = app.cells_len();
        app.start_btw("zweite");
        assert!(app.btw.is_pending());
        assert_eq!(
            app.cells_len(),
            cells + 1,
            "nur ein Hinweis, keine zweite Zelle"
        );
        let text = rendered(&app);
        assert!(text.contains("läuft bereits"), "{text}");
        assert!(!text.contains("btw › zweite"), "{text}");
        assert!(app.cancel_btw());
        Ok(())
    }

    /// Ohne Frage erscheint der Nutzungshinweis, kein Aufruf startet.
    #[tokio::test]
    async fn btw_without_question_shows_usage() -> TestResult {
        let recording = Arc::new(RecordingModelProvider::with_response("x"));
        let (mut app, _turn) = busy_app(recording.clone(), None)?;
        let _ = route_busy_command(&mut app, "/btw".to_owned());
        assert!(!app.btw.is_pending());
        assert!(rendered(&app).contains("Nutzung: /btw <frage>"));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(recording.recorded().is_empty());
        Ok(())
    }

    /// Esc bricht die wartende Nebenfrage ab, nicht den Turn.
    #[tokio::test]
    async fn esc_cancels_btw_but_not_the_turn() -> TestResult {
        let (mut app, turn_cancel) = busy_app(Arc::new(NeverProvider), None)?;
        app.start_btw("dauert");
        assert!(app.btw.is_pending());

        let esc = TuiEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let outcome = handle_busy_event(&mut app, esc);
        assert_eq!(outcome, BusyKeyOutcome::Redraw);
        assert!(!app.btw.is_pending());
        assert!(!turn_cancel.is_cancelled(), "erstes Esc gilt nur /btw");
        assert!(rendered(&app).contains("btw abgebrochen"));

        // Ein zweites Esc unterbricht wie gewohnt den Turn.
        let esc = TuiEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let _ = handle_busy_event(&mut app, esc);
        assert!(turn_cancel.is_cancelled());
        Ok(())
    }

    /// Die Token-Nutzung wird als interner Verbrauch (`btw`) gemeldet.
    #[tokio::test]
    async fn btw_usage_is_reported_as_internal_usage() -> TestResult {
        let hub = AgentEventHub::new(8);
        let mut rx = hub.subscribe();
        let recording = Arc::new(RecordingModelProvider::with_response("ok"));
        let (mut app, _turn) = busy_app(recording, Some(hub))?;
        app.start_btw("zähl mich");
        wait_until_done(&mut app).await?;

        let event = rx
            .try_recv()
            .map_err(|error| TestError::Unexpected(format!("kein InternalUsage-Event: {error}")))?;
        assert!(matches!(
            event.kind,
            AgentEventKind::InternalUsage { ref purpose, .. } if purpose == BTW_USAGE_PURPOSE
        ));
        Ok(())
    }

    /// Ein Fehler erscheint als Systemzeile, die Zelle als fehlgeschlagen.
    #[tokio::test]
    async fn btw_timeout_shows_system_line() -> TestResult {
        let (mut app, _turn) = busy_app(Arc::new(NeverProvider), None)?;
        app.btw.timeout = Duration::from_millis(20);
        app.start_btw("zu langsam");
        wait_until_done(&mut app).await?;
        let text = rendered(&app);
        assert!(text.contains("⚠ /btw fehlgeschlagen: Zeitlimit"), "{text}");
        assert!(text.contains("btw fehlgeschlagen"), "{text}");
        Ok(())
    }
}
