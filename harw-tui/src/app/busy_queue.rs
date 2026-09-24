//! Befehle und Datenabrufe während eines laufenden Turns (Runde 4, Teil H).
//!
//! # Beschreibung
//! Während ein Turn läuft, dürfen `Immediate`- und `Staged`-Befehle
//! ([`BusyAvailability`]) sowie die Datenabrufe offener Ansichten den Turn
//! nicht blockieren. Dieses Modul startet sie deshalb als eigene Tokio-Tasks
//! mit geklonten `Arc`s ([`BusyDispatch`]) und liefert ihre Ergebnisse über
//! einen Kanal zurück ([`BusyJobs::recv`]), den die Busy-`select!`-Schleifen
//! in `app.rs` neben `turn_event_rx` abfragen. Jeder Auftrag hat ein
//! Zeitlimit ([`BUSY_COMMAND_TIMEOUT`]).
//!
//! Außerdem baut es den sichtbaren Warteschlangen-Block über dem Composer
//! ([`queue_block_lines`]).
//!
//! # Nebenläufigkeit
//! Die Tasks teilen nur unveränderliche Daten (Montage, Adapter, Sandbox,
//! Session-ID). Session-Mutationen laufen weiterhin ausschließlich an
//! Turn-Grenzen (`ChatApp::apply_pending_controller_state`); `Staged`-Befehle
//! merken ihre Änderung nur im Controller vor.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use harw_authority::SandboxSpec;
use harw_operations::OpOutput;
#[cfg(test)]
use harw_operations::PermissionTier;
use harw_operations::adapter::CommandAdapter;
use harw_operations::operation::BusyAvailability;
use harw_types::SessionId;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use unicode_width::UnicodeWidthChar;

use crate::frame_requester::FrameRequester;
#[cfg(test)]
use crate::session_controller::TuiSessionController;

/// Zeitlimit für einen einzelnen Befehl oder Datenabruf während eines Turns.
pub(super) const BUSY_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

/// Höchstzahl der Anzeigezeilen je eingereihter Nachricht im
/// Warteschlangen-Block.
const QUEUE_LINES_PER_MESSAGE: usize = 3;

/// Höchstzahl der Zeilen des gesamten Warteschlangen-Blocks (inkl. Kopf).
const QUEUE_BLOCK_MAX_LINES: usize = 10;

/// Ein während eines Turns sofort auszuführender Befehl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BusyCommand {
    /// Die abgeschickte `/command`-Zeile.
    pub(super) raw: String,
    /// `Immediate` oder `Staged`.
    pub(super) class: BusyAvailability,
}

/// Ziel eines Datenabrufs (Spiegel von `DataFetch` in `app.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FetchTarget {
    /// Generische Ansicht der angegebenen Generation.
    Overlay {
        /// Generation der Ansicht beim Start des Abrufs.
        generation: u64,
    },
    /// Werkbank-Panel.
    Workbench,
}

/// Ergebnis eines abgeschlossenen Busy-Auftrags.
#[derive(Debug)]
pub(super) enum BusyJobDone {
    /// Ein Befehl lief; `text` ist seine Anzeigeausgabe.
    Command {
        /// Der ausgeführte Befehl.
        command: BusyCommand,
        /// Anzeigetext (auch Fehlermeldung oder Zeitlimit-Hinweis).
        text: String,
        /// `false`, wenn der Befehl scheiterte (Fehler, Ablehnung,
        /// Zeitlimit) — dann gilt auch ein `Staged`-Befehl nicht „ab nächstem
        /// Turn“.
        succeeded: bool,
    },
    /// Ein Datenabruf lief.
    Fetch {
        /// Wohin das Ergebnis gehört.
        target: FetchTarget,
        /// Strukturierte Ausgabe oder Fehlertext.
        result: Result<OpOutput, String>,
    },
}

/// Geklonte, `'static` Dispatch-Daten für einen Busy-Task.
#[derive(Clone)]
pub(super) struct BusyDispatch {
    /// Runtime-Montage (Berechtigungsstufe und Slash-Dienste).
    pub(super) runtime: Option<Arc<harw_runtime::RuntimeAssembly>>,
    /// Command-Adapter der Sitzung.
    pub(super) adapters: Arc<[CommandAdapter]>,
    /// Authority-Boundary.
    pub(super) sandbox: SandboxSpec,
    /// Sitzung.
    pub(super) session_id: SessionId,
    /// Nur Tests: Dispatch ohne Montage mit Operator-Stufe und Diensten um
    /// diesen Controller (siehe [`BusyJobs::enable_dispatch_without_runtime`]).
    #[cfg(test)]
    pub(super) test_controller: Option<Arc<TuiSessionController>>,
}

impl BusyDispatch {
    /// Führt eine `/command`-Zeile aus und liefert ihren Anzeigetext.
    async fn command_text(&self, raw: &str) -> String {
        #[cfg(test)]
        {
            if let (None, Some(controller)) = (&self.runtime, &self.test_controller) {
                let adapters = Arc::clone(&self.adapters);
                return crate::command_exec::execute_command_as(
                    &self.adapters,
                    &self.sandbox,
                    &self.session_id,
                    PermissionTier::Operator,
                    raw,
                    || {
                        crate::command_exec::build_services(
                            &adapters, None, None, controller, None, None,
                        )
                    },
                )
                .await;
            }
        }
        crate::command_exec::dispatch_slash_command(
            self.runtime.as_ref(),
            &self.adapters,
            &self.sandbox,
            &self.session_id,
            raw,
        )
        .await
    }

    /// Führt eine `/command`-Zeile als Datenabruf aus.
    async fn fetch(&self, raw: &str) -> Result<OpOutput, String> {
        #[cfg(test)]
        {
            if let (None, Some(controller)) = (&self.runtime, &self.test_controller) {
                let adapters = Arc::clone(&self.adapters);
                return crate::command_data::execute_command_with_data(
                    &self.adapters,
                    &self.sandbox,
                    &self.session_id,
                    PermissionTier::Operator,
                    raw,
                    || {
                        crate::command_exec::build_services(
                            &adapters, None, None, controller, None, None,
                        )
                    },
                )
                .await;
            }
        }
        super::fetch_command_data(
            self.runtime.as_ref(),
            &self.adapters,
            &self.sandbox,
            &self.session_id,
            raw,
        )
        .await
    }
}

/// Warteschlange, laufende Aufträge und Ergebniskanal der Busy-Befehle.
pub(super) struct BusyJobs {
    /// Sender, den jeder Task klont.
    tx: UnboundedSender<BusyJobDone>,
    /// Empfänger; in den Busy-`select!`-Schleifen und an der Spitze von
    /// `run_loop` geleert.
    rx: UnboundedReceiver<BusyJobDone>,
    /// Eingestufte, noch nicht gestartete Befehle (FIFO).
    queued: VecDeque<BusyCommand>,
    /// Anzahl gestarteter, noch nicht zurückgemeldeter Aufträge.
    running: usize,
    /// `true`, solange ein Werkbank-Abruf läuft (verhindert Doppelabrufe im
    /// Spinner-Takt).
    workbench_in_flight: bool,
    /// Weckt `run_loop` nach einem Ergebnis, auch wenn der Turn schon vorbei
    /// ist.
    waker: Option<FrameRequester>,
    /// Zeitlimit je Auftrag.
    timeout: Duration,
    /// Nur Tests: ohne Montage mit Operator-Stufe dispatchen.
    #[cfg(test)]
    dispatch_without_runtime: bool,
}

impl std::fmt::Debug for BusyJobs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BusyJobs")
            .field("queued", &self.queued)
            .field("running", &self.running)
            .field("workbench_in_flight", &self.workbench_in_flight)
            .finish_non_exhaustive()
    }
}

impl BusyJobs {
    /// Leerer Zustand mit frischem Kanal.
    pub(super) fn new() -> Self {
        let (tx, rx) = unbounded_channel();
        Self {
            tx,
            rx,
            queued: VecDeque::new(),
            running: 0,
            workbench_in_flight: false,
            waker: None,
            timeout: BUSY_COMMAND_TIMEOUT,
            #[cfg(test)]
            dispatch_without_runtime: false,
        }
    }

    /// Hinterlegt den Frame-Anforderer, über den ein fertiger Auftrag die
    /// Hauptschleife weckt.
    pub(super) fn set_waker(&mut self, waker: FrameRequester) {
        self.waker = Some(waker);
    }

    /// Reiht einen eingestuften Befehl zum Start ein.
    pub(super) fn queue(&mut self, command: BusyCommand) {
        self.queued.push_back(command);
    }

    /// Eingestufte, noch nicht gestartete Befehle.
    pub(super) fn queued(&self) -> &VecDeque<BusyCommand> {
        &self.queued
    }

    /// Anzahl laufender Aufträge.
    #[cfg(test)]
    pub(super) fn running(&self) -> usize {
        self.running
    }

    /// `true`, solange ein Werkbank-Abruf läuft.
    pub(super) fn workbench_in_flight(&self) -> bool {
        self.workbench_in_flight
    }

    /// Nur Tests: Zeitlimit je Auftrag setzen.
    #[cfg(test)]
    pub(super) fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Nur Tests: ohne Montage mit Operator-Stufe und Diensten um den
    /// Sitzungs-Controller dispatchen.
    #[cfg(test)]
    pub(super) fn enable_dispatch_without_runtime(&mut self) {
        self.dispatch_without_runtime = true;
    }

    /// `true`, wenn Tests den Dispatch ohne Montage verlangt haben.
    #[cfg(test)]
    pub(super) fn dispatches_without_runtime(&self) -> bool {
        self.dispatch_without_runtime
    }

    /// Startet alle eingereihten Befehle als eigene Tasks.
    ///
    /// # Nebenläufigkeit
    /// Muss innerhalb einer Tokio-Runtime aufgerufen werden
    /// (`tokio::spawn`). Kehrt sofort zurück.
    pub(super) fn start_queued(&mut self, dispatch: &BusyDispatch) {
        while let Some(command) = self.queued.pop_front() {
            let dispatch = dispatch.clone();
            let tx = self.tx.clone();
            let waker = self.waker.clone();
            let timeout = self.timeout;
            self.running += 1;
            tracing::debug!(command = %command.raw, class = ?command.class, "tui.busy.command_started");
            tokio::spawn(async move {
                let (text, succeeded) = match tokio::time::timeout(
                    timeout,
                    dispatch.command_text(&command.raw),
                )
                .await
                {
                    Ok(text) => {
                        let succeeded = !is_failure_text(&text);
                        (text, succeeded)
                    }
                    Err(_) => (
                        format!(
                            "Zeitlimit überschritten ({} s) — Befehl abgebrochen.",
                            timeout.as_secs_f32()
                        ),
                        false,
                    ),
                };
                let _ = tx.send(BusyJobDone::Command {
                    command,
                    text,
                    succeeded,
                });
                if let Some(waker) = waker {
                    waker.schedule_frame();
                }
            });
        }
    }

    /// Startet einen Datenabruf als eigenen Task.
    ///
    /// # Nebenläufigkeit
    /// Wie [`Self::start_queued`].
    pub(super) fn start_fetch(
        &mut self,
        dispatch: &BusyDispatch,
        target: FetchTarget,
        raw: String,
    ) {
        let dispatch = dispatch.clone();
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        let timeout = self.timeout;
        self.running += 1;
        if target == FetchTarget::Workbench {
            self.workbench_in_flight = true;
        }
        tokio::spawn(async move {
            let result = match tokio::time::timeout(timeout, dispatch.fetch(&raw)).await {
                Ok(result) => result,
                Err(_) => Err(format!(
                    "Zeitlimit überschritten ({} s).",
                    timeout.as_secs_f32()
                )),
            };
            let _ = tx.send(BusyJobDone::Fetch { target, result });
            if let Some(waker) = waker {
                waker.schedule_frame();
            }
        });
    }

    /// Wartet auf das nächste Ergebnis (für `select!`).
    ///
    /// # Rückgabe
    /// `None` nie im Betrieb (der Sender lebt so lange wie `self`).
    pub(super) async fn recv(&mut self) -> Option<BusyJobDone> {
        let done = self.rx.recv().await;
        if let Some(done) = &done {
            self.finish(done);
        }
        done
    }

    /// Holt ein bereits vorliegendes Ergebnis ohne zu warten.
    pub(super) fn try_recv(&mut self) -> Option<BusyJobDone> {
        let done = self.rx.try_recv().ok()?;
        self.finish(&done);
        Some(done)
    }

    /// Buchhaltung für ein angekommenes Ergebnis.
    fn finish(&mut self, done: &BusyJobDone) {
        self.running = self.running.saturating_sub(1);
        if matches!(
            done,
            BusyJobDone::Fetch {
                target: FetchTarget::Workbench,
                ..
            }
        ) {
            self.workbench_in_flight = false;
        }
    }
}

/// `true`, wenn `text` eine Fehlermeldung des Befehlspfads ist.
///
/// # Beschreibung
/// Der Befehlspfad (`command_exec::execute_with_context`) liefert nur Text;
/// seine Fehlerpfade beginnen mit festen Präfixen: Operationsfehler
/// (`Fehler: …`), unbekannter Befehl, verweigerte Berechtigung und
/// abgelehnte Eingabe/Shell.
fn is_failure_text(text: &str) -> bool {
    const FAILURE_PREFIXES: &[&str] = &[
        "Fehler:",
        "Unbekannter Command:",
        "Berechtigung verweigert:",
        "Eingabe abgelehnt:",
        "Shell-Ausführung abgelehnt:",
    ];
    let text = text.trim_start();
    FAILURE_PREFIXES
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

/// Anzeigezeilen eines Befehlsergebnisses, das während eines Turns lief.
///
/// # Beschreibung
/// Kopfzeile `"<befehl> (während Turn)"`, bei einem **erfolgreichen**
/// `Staged`-Befehl ergänzt um „— gilt ab nächstem Turn“ (ein gescheiterter
/// gilt nie), danach die Ausgabe zeilenweise.
pub(super) fn command_result_lines(
    command: &BusyCommand,
    text: &str,
    succeeded: bool,
) -> Vec<Line<'static>> {
    let staged = if succeeded && command.class == BusyAvailability::Staged {
        " — gilt ab nächstem Turn"
    } else {
        ""
    };
    let mut lines = vec![Line::from(Span::styled(
        format!("{} (während Turn){staged}", command.raw.trim()),
        Style::default().add_modifier(Modifier::DIM),
    ))];
    lines.extend(text.split('\n').map(|line| Line::from(line.to_owned())));
    lines
}

/// Zeilen des Warteschlangen-Blocks über dem Composer.
///
/// # Beschreibung
/// Kopfzeile „Wartet auf den nächsten Turn“, danach jede eingereihte
/// Nachricht mit vollem Text, auf [`QUEUE_LINES_PER_MESSAGE`] Anzeigezeilen
/// gekürzt (`…` markiert die Kürzung), und jeder zurückgestellte Befehl als
/// eine Zeile. Der Block ist auf [`QUEUE_BLOCK_MAX_LINES`] Zeilen begrenzt;
/// der Rest wird als „… und n weitere“ zusammengefasst. Leer, wenn nichts
/// wartet.
///
/// # Argumente
/// - `messages`: eingereihte Nachrichten (`pending_turns`), älteste zuerst.
/// - `commands`: zurückgestellte Befehle (aus `deferred_input`).
/// - `width`: verfügbare Breite in Spalten.
pub(super) fn queue_block_lines<'a>(
    messages: impl IntoIterator<Item = &'a str>,
    commands: impl IntoIterator<Item = &'a str>,
    width: u16,
) -> Vec<Line<'static>> {
    let messages: Vec<&str> = messages.into_iter().collect();
    let commands: Vec<&str> = commands.into_iter().collect();
    let total = messages.len() + commands.len();
    if total == 0 {
        return Vec::new();
    }
    let dim = Style::default().add_modifier(Modifier::DIM);
    let text_width = usize::from(width).saturating_sub(4).max(8);
    let mut lines = vec![Line::from(Span::styled(
        format!("Wartet auf den nächsten Turn ({total}) · Alt+↑ holt die letzte Nachricht zurück"),
        dim.add_modifier(Modifier::BOLD),
    ))];
    let mut shown = 0_usize;
    let entries = messages
        .iter()
        .map(|text| (true, *text))
        .chain(commands.iter().map(|text| (false, *text)));
    for (is_message, text) in entries {
        let per_entry = if is_message {
            QUEUE_LINES_PER_MESSAGE
        } else {
            1
        };
        let wrapped = wrap_preview(text.trim(), text_width, per_entry);
        // Platz für die Schlusszeile „… und n weitere“ freihalten.
        let reserve = usize::from(shown + 1 < total);
        if lines.len() + wrapped.len() + reserve > QUEUE_BLOCK_MAX_LINES && shown > 0 {
            break;
        }
        for (index, line) in wrapped.into_iter().enumerate() {
            let prefix = match (index, is_message) {
                (0, true) => "  › ",
                (0, false) => "  ⏎ ",
                _ => "    ",
            };
            lines.push(Line::from(vec![Span::styled(prefix, dim), Span::raw(line)]));
        }
        shown += 1;
    }
    if shown < total {
        lines.push(Line::from(Span::styled(
            format!("  … und {} weitere", total - shown),
            dim,
        )));
    }
    lines
}

/// Bricht `text` auf `width` Spalten um und kürzt auf `max_lines` Zeilen;
/// eine Kürzung endet mit `…`.
fn wrap_preview(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for source in text.split('\n') {
        let mut current = String::new();
        let mut current_width = 0_usize;
        for ch in source.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if current_width + ch_width > width && !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                current_width = 0;
            }
            current.push(ch);
            current_width += ch_width;
        }
        lines.push(current);
        if lines.len() > max_lines {
            break;
        }
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        if let Some(last) = lines.last_mut() {
            if last.chars().count() >= width {
                last.pop();
            }
            last.push('…');
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn queue_block_is_empty_without_entries() {
        assert!(queue_block_lines([], [], 80).is_empty());
    }

    #[test]
    fn queue_block_shows_full_short_message_and_commands() {
        let lines = plain(&queue_block_lines(["hallo welt"], ["/compact"], 80));
        assert!(lines[0].starts_with("Wartet auf den nächsten Turn (2)"));
        assert_eq!(lines[1], "  › hallo welt");
        assert_eq!(lines[2], "  ⏎ /compact");
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn queue_block_truncates_long_messages_to_three_lines() {
        let long = "zeile\n".repeat(20);
        let lines = plain(&queue_block_lines([long.as_str()], [], 80));
        // Kopf + drei Vorschauzeilen.
        assert_eq!(lines.len(), 1 + QUEUE_LINES_PER_MESSAGE);
        assert!(lines[3].ends_with('…'), "{lines:?}");
    }

    #[test]
    fn queue_block_wraps_by_width_and_caps_total_lines() {
        let wide = "x".repeat(200);
        let many: Vec<String> = (0..20).map(|index| format!("nachricht {index}")).collect();
        let lines = plain(&queue_block_lines(
            std::iter::once(wide.as_str()).chain(many.iter().map(String::as_str)),
            [],
            24,
        ));
        assert!(lines.len() <= QUEUE_BLOCK_MAX_LINES, "{lines:?}");
        assert!(lines[3].ends_with('…'));
        assert!(
            lines.last().is_some_and(|line| line.contains("weitere")),
            "{lines:?}"
        );
    }

    #[test]
    fn staged_result_header_mentions_next_turn() {
        let command = BusyCommand {
            raw: "/effort high".to_owned(),
            class: BusyAvailability::Staged,
        };
        let lines = plain(&command_result_lines(&command, "ok", true));
        assert_eq!(
            lines[0],
            "/effort high (während Turn) — gilt ab nächstem Turn"
        );
        assert_eq!(lines[1], "ok");
    }

    #[test]
    fn failed_staged_result_does_not_claim_the_next_turn() {
        let command = BusyCommand {
            raw: "/effort ultra".to_owned(),
            class: BusyAvailability::Staged,
        };
        let text = "Fehler: ungültiger Effort";
        assert!(is_failure_text(text));
        let lines = plain(&command_result_lines(&command, text, false));
        assert_eq!(lines[0], "/effort ultra (während Turn)");
        assert_eq!(lines[1], text);
        for failure in [
            "Unbekannter Command: /x",
            "Berechtigung verweigert: /x erfordert Operator",
            "Eingabe abgelehnt: leer",
            "Shell-Ausführung abgelehnt: aus",
        ] {
            assert!(is_failure_text(failure), "{failure}");
        }
        assert!(!is_failure_text("Reasoning-Effort gesetzt: high"));
    }
}
