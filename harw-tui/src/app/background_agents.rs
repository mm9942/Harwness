//! Hintergrund-Agenten in der TUI (Runde 5, Teil K).
//!
//! # Verantwortungsbereich
//! Ein Orchestrator, den die UIA-Wurzel über `transfer_to_<orchestrator>`
//! startet, blockiert den Chat nicht mehr: der [`BackgroundLauncher`] koppelt
//! das bereits admittierte Kind ab (`ManagedAgentSpawner::detach_for_background`),
//! treibt es als eigene Tokio-Task über denselben Weg wie den synchronen Fall
//! (`ChildTurnDriver::drive_child` → `run_child_with_declared_budget`, seit
//! Runde 7 Teil A1 mit Budgetprüfung: dieselbe Sandbox, dasselbe
//! Budget, dieselbe Freigabekette, dieselbe Übergabe am Budget-Ende) und
//! gibt dem Eltern-Turn sofort `{child_id, status: "running", hint}` zurück.
//!
//! Ist das Kind fertig, legt die Task sein (wie im synchronen Rückgabeweg
//! gedeckeltes) Ergebnis als Benachrichtigung ab
//! (`ManagedAgentSpawner::finish_background_child`). Die Ereignisschleife
//! holt sie im Leerlauf ab ([`collect_finished`]):
//! - **idle** → [`take_auto_turn`] startet einen UIA-Turn
//!   „[Hintergrund-Agent … fertig] …", die UIA fasst zusammen;
//! - **busy** → die Benachrichtigung wartet und geht dem nächsten Turn als
//!   Kontext voran ([`attach_queued_notices`]).
//!
//! [`enqueue_background_notice`] ist die wiederverwendbare Einreihung (auch
//! für andere Meldungen an die UIA, z. B. `parent.message`).
//!
//! # Freigaben im Leerlauf
//! Ein Hintergrund-Kind kann sudo- und Host-Permit-Fragen stellen, während
//! kein UIA-Turn läuft. [`poll_idle_prompts`] holt sie im Leerlauf ab und
//! öffnet dieselben Fenster wie während eines Turns;
//! [`route_idle_prompt_event`] leitet die Tasten dorthin. Fail-closed bleibt:
//! Esc/Ctrl+C lehnen ab, ein Fenster ohne Antwort läuft wie bisher in den
//! Zeitablauf (Ablehnung).
//!
//! # Grenzen
//! - Nur die TUI-Wurzel und nur Orchestrator-Ziele (Erkennung über die
//!   Rollendefinition, [`OrchestratorRoles`]); `background: false` erzwingt
//!   synchrones Warten. Worker bleiben synchron. Jeder andere Einstieg hat
//!   keinen Launcher und bleibt synchron.
//! - Wie viele Orchestratoren gleichzeitig laufen, begrenzt der Spawner
//!   (`[agents] max_root_orchestrators`), nicht diese Datei.
//! - `/new`, `/resume` und Beenden brechen laufende Hintergrund-Kinder ab
//!   ([`cancel_all`]); `/quit` fragt vorher nach ([`confirm_quit`]).
//!
//! # Nebenläufigkeit
//! Die Launcher-Task läuft auf dem `current_thread`-Runtime der TUI; sie
//! berührt die `ChatApp` nie, nur das `Arc`-geteilte Register des Spawners.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};
use harw_core::background_children::{
    BackgroundChildren, BackgroundNotice, BackgroundRun, BackgroundStatus,
};
use harw_core::{
    AgentEvent, AgentEventHub, AgentSession, LiveEmitter, ManagedAgentSpawner, StateStore,
};
use harw_protocol::TurnEvent;
use harw_protocol::items::{ToolCallResult, TurnItem};
use harw_types::{SessionId, ToolCallId, TurnId};
use ratatui::text::Line;
use serde_json::json;
use tokio::sync::broadcast;

use super::{
    ChatApp, Role, apply_host_permit_decision, approval_dialog_key_is_armed,
    auto_grant_host_permit, open_host_permit_prompt,
};
use crate::agent_tree::AgentRow;
use crate::approval::ChildTurnDriver;
use crate::child_stream::OrchestratorRoles;
use crate::choice_dialog::ChoiceAction;
use crate::host_permit_dialog::HostPermitPromptReceiver;
use crate::tui_event::TuiEvent;

/// Der Nutzertext eines automatisch gestarteten Turns; die Benachrichtigung
/// selbst steht davor ([`attach_queued_notices`]).
pub(crate) const AUTO_TURN_PROMPT: &str = "Ein Hintergrund-Agent hat sich gemeldet (siehe oben). \
     Fasse das Ergebnis für die Nutzerin kurz zusammen und nenne, falls nötig, die nächsten \
     Schritte. Das vollständige Ergebnis liefert agent.result {child_id}.";

/// Trenner zwischen eingereihten Meldungen und dem Turn-Text
/// ([`attach_queued_notices`]).
const NOTICE_SEPARATOR: &str = "\n\n---\n";

/// Anfänge eingespeister Meldungen ([`format_notice`],
/// `ParentMessage::to_model_text`, `stale_model_text`) — auch mit dem `↩ `
/// der Anzeige-Überschreibung eines Auto-Turns ([`take_auto_turn`]).
const NOTICE_PREFIXES: &[&str] = &["[Hintergrund-Agent ", "[Nachricht von ", "[Frage von "];

/// Ob `text` mit einer eingespeisten Meldung beginnt.
fn starts_with_notice(text: &str) -> bool {
    let text = text.trim_start();
    let text = text.strip_prefix('↩').map_or(text, str::trim_start);
    NOTICE_PREFIXES
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

/// Übersetzt den Text einer Nutzerzelle in Export-Einträge und trennt dabei
/// eingespeiste Meldungen (Hintergrund-Agenten, Kind-Nachrichten, die
/// Auto-Turn-Anweisung) von dem, was die Nutzerin tatsächlich schrieb.
///
/// # Beschreibung
/// Eingespeiste Meldungen stehen im Verlauf als Nutzernachricht (die
/// Historie kennt keine Herkunft), gehören im Export aber nicht unter
/// `## Du`:
/// - ein Auto-Turn (endet mit [`AUTO_TURN_PROMPT`]) bzw. seine Anzeige
///   (`↩ [Hintergrund-Agent …]`) wird vollständig zur
///   [`ExportEntry::Notice`];
/// - vor einem getippten Turn eingereihte Meldungen
///   (`<Meldungen>\n\n---\n<Text>`) werden getrennt: Meldungen als
///   `Notice`, der Rest als `User`;
/// - alles andere bleibt `User`.
pub(crate) fn user_text_export_entries(text: &str) -> Vec<crate::export::ExportEntry> {
    use crate::export::ExportEntry;
    if text.trim_end().ends_with(AUTO_TURN_PROMPT) || starts_with_notice(text) {
        if let Some((notices, typed)) = text.rsplit_once(NOTICE_SEPARATOR)
            && starts_with_notice(notices)
            && !typed.trim_end().ends_with(AUTO_TURN_PROMPT)
            && !typed.trim().is_empty()
        {
            return vec![
                ExportEntry::Notice(notices.to_owned()),
                ExportEntry::User(typed.to_owned()),
            ];
        }
        return vec![ExportEntry::Notice(text.to_owned())];
    }
    vec![ExportEntry::User(text.to_owned())]
}

/// Zeitfenster, in dem ein zweites `/quit` trotz laufender Hintergrund-Agenten
/// beendet.
pub(crate) const QUIT_CONFIRM_WINDOW: Duration = Duration::from_secs(10);

/// Leerlauf-Takt, solange Hintergrund-Agenten laufen (Freigabe-Fragen
/// abholen, Fortschritt zeichnen).
pub(crate) const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Hinweis an das Modell im sofortigen Werkzeugergebnis.
const RUNNING_HINT: &str = "Der Agent läuft im Hintergrund weiter. Sag der Nutzerin kurz, \
     dass er gestartet ist, und beende deinen Turn — sein Ergebnis kommt automatisch als \
     Benachrichtigung. Fortschritt: agent.status {child_id}; Abbruch: agent.cancel {child_id}.";

/// Runde 7, Teil A7: Zusatz im sofortigen Werkzeugergebnis, wenn der
/// startende Turn während des Starts abgebrochen wurde (neue Nachricht,
/// Turn-Grenze, Esc) — der Start wurde trotzdem abgeschlossen.
pub(crate) const START_SURVIVED_NOTE: &str = "Der startende Turn wurde während des Starts \
     abgebrochen; der Hintergrund-Agent wurde trotzdem vollständig gestartet und läuft weiter. \
     Nicht erneut starten. Abbruch nur ausdrücklich mit agent.cancel {child_id}.";

/// Eine eingereihte Meldung an die UIA.
#[derive(Debug, Clone)]
struct QueuedNotice {
    /// Voller Text für das Modell.
    text: String,
    /// Einzeilige Anzeige für die Nutzerzelle eines Auto-Turns.
    display: String,
    /// Ob die Meldung im Leerlauf einen eigenen Turn auslöst.
    auto_turn: bool,
}

/// TUI-seitiger Zustand der Hintergrund-Agenten (Feld `ChatApp::background`).
#[derive(Debug, Default)]
pub(crate) struct BackgroundUi {
    /// Eingereihte Meldungen, älteste zuerst.
    queued: VecDeque<QueuedNotice>,
    /// Erstes `/quit` bei laufenden Hintergrund-Agenten.
    quit_armed_at: Option<Instant>,
    /// Arming-Uhr eines im Leerlauf geöffneten Host-Permit-Fensters.
    host_permit_shown_at: Option<Instant>,
}

// ── Starter ───────────────────────────────────────────────────────────────────

/// Startet Orchestrator-Handoffs der TUI-Wurzel im Hintergrund.
///
/// # Beschreibung
/// Hängt am `ApprovalDriver` der TUI-Wurzel
/// (`ApprovalDriver::with_background`); jeder andere Einstieg hat keinen und
/// bleibt synchron.
pub(crate) struct BackgroundLauncher {
    root: SessionId,
    spawner: Arc<ManagedAgentSpawner>,
    store: Arc<dyn StateStore>,
    hub: Option<AgentEventHub>,
    roles: OrchestratorRoles,
}

impl std::fmt::Debug for BackgroundLauncher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BackgroundLauncher")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl BackgroundLauncher {
    /// Baut den Starter.
    ///
    /// # Argumente
    /// - `root`: die UIA-Wurzelsitzung (nur ihre Handoffs laufen im Hintergrund).
    /// - `spawner`: derselbe Spawner, der die Kinder admittiert.
    /// - `store`: derselbe Store wie der synchrone Kind-Treiber.
    /// - `hub`: Agenten-Ereignisbus für den Fortschritt (`None` = ohne).
    /// - `roles`: Orchestrator-Erkennung über die Rollendefinitionen.
    pub(crate) fn new(
        root: SessionId,
        spawner: Arc<ManagedAgentSpawner>,
        store: Arc<dyn StateStore>,
        hub: Option<AgentEventHub>,
        roles: OrchestratorRoles,
    ) -> Self {
        Self {
            root,
            spawner,
            store,
            hub,
            roles,
        }
    }

    /// Entscheidet, ob ein Handoff im Hintergrund läuft.
    ///
    /// # Argumente
    /// - `session_is_root`: ob der Aufrufer die UIA-Wurzel ist.
    /// - `role`: Zielrolle.
    /// - `requested`: das optionale `background`-Argument des Aufrufs.
    ///
    /// # Returns
    /// `true` nur für Orchestrator-Ziele der Wurzel ohne `background: false`.
    /// Worker bleiben immer synchron (auch mit `background: true`).
    pub(crate) fn wants_background(
        &self,
        session_is_root: bool,
        role: &str,
        requested: Option<bool>,
    ) -> bool {
        session_is_root && self.roles.is_orchestrator(role) && requested != Some(false)
    }

    /// Versucht, ein soeben admittiertes Kind im Hintergrund zu starten.
    ///
    /// # Returns
    /// `Some(ergebnis)` mit `{child_id, status: "running", hint}`, wenn das
    /// Kind abgekoppelt wurde und läuft; `None`, wenn es synchron bleiben
    /// soll (Worker, `background: false`, kein Laufzeitkontext, Abkoppeln
    /// gescheitert) — dann treibt der Aufrufer es wie bisher.
    pub(crate) fn try_launch(
        &self,
        session: &AgentSession,
        child: &SessionId,
        call_id: &ToolCallId,
        role: &str,
    ) -> Option<ToolCallResult> {
        let arguments = handoff_arguments(session, call_id);
        let requested = arguments
            .as_ref()
            .and_then(|args| args.get("background"))
            .and_then(serde_json::Value::as_bool);
        if !self.wants_background(session.id() == &self.root, role, requested) {
            if requested == Some(true) {
                tracing::info!(role, "tui.background.worker_stays_synchronous");
            }
            return None;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(role, "tui.background.no_runtime");
            return None;
        };
        let task = arguments.as_ref().and_then(|args| {
            ["task", "instructions", "objective", "question"]
                .iter()
                .find_map(|field| args.get(*field).and_then(serde_json::Value::as_str))
                .map(ToOwned::to_owned)
        });
        // Runde 7, Teil A7: ein Abbruch des startenden Turns während des
        // Starts (geerbter Grund am Token des Kindes) reißt den Start nicht
        // mehr mit — `detach_for_background` schließt ihn mit frischem Token
        // ab; die UIA bekommt dazu einen klaren Hinweis.
        let start_interrupted = self
            .spawner
            .child_cancel_token(child)
            .is_some_and(|token| token.is_cancelled());
        if let Err(error) = self.spawner.detach_for_background(child, task.as_deref()) {
            tracing::warn!(child = %child, error = %error.message, "tui.background.detach_failed");
            return None;
        }
        // Fortschritt wie im synchronen Fall an den Live-Kanal der Wurzel;
        // derselbe Kanal meldet am echten Ende `ChildCompleted` (der Kern
        // unterdrückt es beim Wiederaufnehmen des Eltern-Turns).
        let completion = session.current_turn().cloned().map(|turn_id| {
            let emitter = session.live_emitter();
            let _ =
                self.spawner
                    .attach_child_progress_sink(child, turn_id.clone(), emitter.clone());
            (turn_id, emitter)
        });
        let spawner = Arc::clone(&self.spawner);
        let store = Arc::clone(&self.store);
        let events = self.hub.as_ref().map(AgentEventHub::subscribe);
        let child_id = child.clone();
        runtime.spawn(drive_in_background(
            spawner, store, events, child_id, completion,
        ));
        tracing::info!(child = %child, role, start_interrupted, "tui.background.started");
        Some(ToolCallResult::success(launch_result(
            child,
            role,
            start_interrupted,
        )))
    }
}

/// Das sofortige Werkzeugergebnis eines Hintergrund-Starts.
///
/// # Argumente
/// - `child`: das gestartete Kind.
/// - `role`: seine Rolle.
/// - `start_interrupted`: Runde 7, Teil A7 — der startende Turn wurde
///   während des Starts abgebrochen (dann mit [`START_SURVIVED_NOTE`]).
pub(crate) fn launch_result(
    child: &SessionId,
    role: &str,
    start_interrupted: bool,
) -> serde_json::Value {
    let mut value = json!({
        "child_id": child.as_str(),
        "role": role,
        "status": "running",
        "hint": RUNNING_HINT.replace("{child_id}", child.as_str()),
    });
    if start_interrupted && let Some(object) = value.as_object_mut() {
        object.insert(
            "note".to_owned(),
            json!(START_SURVIVED_NOTE.replace("{child_id}", child.as_str())),
        );
    }
    value
}

/// Die Argumente des Handoff-Aufrufs `call_id` aus dem Verlauf der Sitzung.
fn handoff_arguments(session: &AgentSession, call_id: &ToolCallId) -> Option<serde_json::Value> {
    session
        .history()
        .items()
        .iter()
        .rev()
        .find_map(|item| match item {
            TurnItem::ToolCall(call) if &call.call_id == call_id => Some(call.arguments.clone()),
            _ => None,
        })
}

/// Treibt ein abgekoppeltes Kind bis zum Ende und legt das Ergebnis ab.
async fn drive_in_background(
    spawner: Arc<ManagedAgentSpawner>,
    store: Arc<dyn StateStore>,
    mut events: Option<broadcast::Receiver<AgentEvent>>,
    child: SessionId,
    completion: Option<(TurnId, LiveEmitter)>,
) {
    let registry = Arc::clone(spawner.background_children());
    let outcome = {
        let drive = <ManagedAgentSpawner as ChildTurnDriver>::drive_child(
            spawner.as_ref(),
            &child,
            store.as_ref(),
        );
        tokio::pin!(drive);
        loop {
            tokio::select! {
                result = &mut drive => break result,
                event = next_event(&mut events) => match event {
                    Some(event) => registry.observe_event(&event),
                    None => events = None,
                },
            }
        }
    };
    let (status, text) = classify_outcome(outcome);
    let notice = spawner.finish_background_child(&child, status, text);
    if let (Some((turn_id, emitter)), Some(notice)) = (completion, notice) {
        emitter.emit(TurnEvent::ChildCompleted {
            turn_id,
            child: child.clone(),
            outcome: status.as_str().to_owned(),
            duration_ms: u64::try_from(notice.elapsed.as_millis()).unwrap_or(u64::MAX),
        });
    }
}

/// Nächstes Ereignis des Busses; ohne Bus (oder nach dessen Ende) nie fertig.
async fn next_event(events: &mut Option<broadcast::Receiver<AgentEvent>>) -> Option<AgentEvent> {
    let Some(receiver) = events.as_mut() else {
        return std::future::pending().await;
    };
    loop {
        match receiver.recv().await {
            Ok(event) => return Some(event),
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::debug!(skipped, "tui.background.events_lagged");
            }
            Err(broadcast::error::RecvError::Closed) => return None,
        }
    }
}

/// Endzustand und Text eines Kind-Laufs.
///
/// # Beschreibung
/// Der Text ist bereits wie im synchronen Rückgabeweg gedeckelt
/// (`child_final_assistant_text`, Kürzungsmarke nennt `agent.result`).
pub(crate) fn classify_outcome(
    outcome: Result<ToolCallResult, harw_extension_api::AgentSpawnError>,
) -> (BackgroundStatus, String) {
    match outcome {
        Ok(ToolCallResult::Success { value }) => (
            BackgroundStatus::Completed,
            match value {
                serde_json::Value::String(text) => text,
                other => other.to_string(),
            },
        ),
        // Runde 5, Teil M: der Endbericht eines nicht regulär beendeten
        // Kindes trägt seinen Status in der ersten Zeile.
        Ok(ToolCallResult::Error { message }) => {
            let status = match harw_core::parse_child_end(&message).map(|header| header.status) {
                Some(harw_core::ChildEndStatus::Cancelled) => BackgroundStatus::Cancelled,
                Some(_) => BackgroundStatus::Failed,
                None if message.contains("was cancelled") => BackgroundStatus::Cancelled,
                None => BackgroundStatus::Failed,
            };
            (status, message)
        }
        Err(error) => (BackgroundStatus::Failed, error.message),
    }
}

/// Hängt den Starter an den Freigabetreiber der TUI-Wurzel (Montage in
/// `runtime_root.rs`); ohne Spawner bleibt der Treiber synchron.
pub(crate) fn attach_launcher(
    driver: crate::approval::ApprovalDriver,
    assembly: &Arc<harw_runtime::RuntimeAssembly>,
) -> crate::approval::ApprovalDriver {
    let Some(spawner) = assembly.spawner().cloned() else {
        return driver;
    };
    let launcher = BackgroundLauncher::new(
        assembly.root_session_id().clone(),
        spawner,
        Arc::clone(assembly.state_store()),
        Some(assembly.agent_events().clone()),
        OrchestratorRoles::from_definitions(assembly.config().executable_agents.values()),
    );
    driver.with_background(Arc::new(launcher))
}

// ── Benachrichtigungen und Auto-Turn ──────────────────────────────────────────

/// Reiht eine Meldung an die UIA ein.
///
/// # Beschreibung
/// Wiederverwendbar für jede Meldung, die die UIA außerhalb eines Turns
/// erreichen soll (Hintergrund-Ergebnisse, `parent.message`). Die Meldung
/// geht dem nächsten Turn als Kontext voran; mit `auto_turn = true` startet
/// die TUI im Leerlauf selbst einen Turn ([`take_auto_turn`]).
///
/// # Argumente
/// - `app`: die App.
/// - `text`: der volle Text für das Modell (erste Zeile = Anzeige).
/// - `auto_turn`: ob die Meldung im Leerlauf einen Turn auslöst.
pub(crate) fn enqueue_background_notice(
    app: &mut ChatApp,
    text: impl Into<String>,
    auto_turn: bool,
) {
    let text = text.into();
    let display = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("Hintergrund-Meldung")
        .chars()
        .take(160)
        .collect();
    app.background.queued.push_back(QueuedNotice {
        text,
        display,
        auto_turn,
    });
}

/// Formatiert eine Abschlussmeldung für das Modell.
pub(crate) fn format_notice(notice: &BackgroundNotice) -> String {
    let verb = match notice.status {
        BackgroundStatus::Completed | BackgroundStatus::Running => "fertig",
        BackgroundStatus::Failed => "fehlgeschlagen",
        BackgroundStatus::Cancelled => "abgebrochen",
    };
    // Anzeige `<provider>/<modell>` des Kindes, falls bekannt.
    let route = notice
        .model_route
        .as_deref()
        .map(|route| format!(" ({route})"))
        .unwrap_or_default();
    format!(
        "[Hintergrund-Agent {} {}{route} {verb} nach {} s]\n{}",
        notice.role,
        notice.child,
        notice.elapsed.as_secs(),
        notice.text
    )
}

/// Holt die Abschlussmeldungen der Wurzel aus dem Register des Spawners und
/// reiht sie ein (mit Auto-Turn); je Meldung eine Systemzeile.
///
/// # Returns
/// `true`, wenn neue Meldungen eingereiht wurden.
pub(crate) fn collect_finished(app: &mut ChatApp) -> bool {
    let Some(spawner) = app.managed_spawner().cloned() else {
        return false;
    };
    let notices = spawner.background_children().take_notices(app.session_id());
    let any = !notices.is_empty();
    for notice in notices {
        app.push_line(
            Role::System,
            format!(
                "Hintergrund-Agent {} ({}) {}.",
                notice.role,
                notice.child,
                notice.status.label_de()
            ),
        );
        let text = format_notice(&notice);
        // Runde 6, Teil C: Ende samt Benachrichtigungstext in den Export.
        super::export_capture::export_agent_event(
            app,
            crate::export::ExportAgentEntry {
                agent_id: notice.child.as_str().to_owned(),
                role: Some(notice.role.clone()),
                parent_id: Some(notice.parent.as_str().to_owned()),
                status: Some(notice.status.as_str().to_owned()),
                summary: Some(text.clone()),
            },
        );
        enqueue_background_notice(app, text, true);
    }
    any
}

/// Startet im Leerlauf einen Auto-Turn, wenn eine Meldung ihn verlangt.
///
/// # Returns
/// `Some(turn_text)` für `run_loop` (die Meldungen selbst hängt
/// [`attach_queued_notices`] davor); `None` ohne auslösende Meldung.
pub(crate) fn take_auto_turn(app: &mut ChatApp) -> Option<String> {
    let trigger = app
        .background
        .queued
        .iter()
        .find(|notice| notice.auto_turn)?;
    let display = format!("↩ {}", trigger.display);
    app.pending_turn_user_cell_override = Some(display);
    Some(AUTO_TURN_PROMPT.to_owned())
}

/// Stellt alle eingereihten Meldungen dem Turn-Text voran und leert die
/// Warteschlange.
pub(crate) fn attach_queued_notices(app: &mut ChatApp, turn_text: String) -> String {
    if app.background.queued.is_empty() {
        return turn_text;
    }
    let notices: Vec<String> = app
        .background
        .queued
        .drain(..)
        .map(|notice| notice.text)
        .collect();
    format!("{}{NOTICE_SEPARATOR}{turn_text}", notices.join("\n\n"))
}

/// Das Register des Spawners, um im Leerlauf auf neue Meldungen zu warten.
pub(crate) fn waker(app: &ChatApp) -> Option<Arc<BackgroundChildren>> {
    app.managed_spawner()
        .map(|spawner| Arc::clone(spawner.background_children()))
}

/// Wartet auf eine neue Meldung; ohne Register nie fertig.
pub(crate) async fn wait_for_notice(registry: Option<Arc<BackgroundChildren>>) {
    match registry {
        Some(registry) => registry.notified().await,
        None => std::future::pending().await,
    }
}

// ── Anzeige und Steuerung ─────────────────────────────────────────────────────

/// Laufende Hintergrund-Läufe der Wurzel.
pub(crate) fn running(app: &ChatApp) -> Vec<BackgroundRun> {
    app.managed_spawner()
        .map(|spawner| spawner.background_children().running_for(app.session_id()))
        .unwrap_or_default()
}

/// `true`, solange ein Hintergrund-Lauf der Wurzel läuft.
pub(crate) fn has_running(app: &ChatApp) -> bool {
    !running(app).is_empty()
}

/// Agenten-Segment der Statuszeile („2 Agenten aktiv · 1 im Hintergrund").
pub(crate) fn status_suffix(app: &ChatApp, active: usize) -> String {
    status_suffix_for(active, running(app).len())
}

/// Reine Form von [`status_suffix`].
pub(crate) fn status_suffix_for(active: usize, background: usize) -> String {
    match (active, background) {
        (0 | 1, 0) => String::new(),
        (0 | 1, n) => format!(" | {n} im Hintergrund"),
        (n, 0) => format!(" | {n} Agenten aktiv"),
        (n, b) => format!(" | {n} Agenten aktiv · {b} im Hintergrund"),
    }
}

/// Markiert laufende Hintergrund-Kinder in den Zeilen des Agentenbaums.
pub(crate) fn mark_rows(app: &ChatApp, rows: &mut [AgentRow]) {
    let running = running(app);
    if running.is_empty() {
        return;
    }
    for row in rows.iter_mut() {
        if running.iter().any(|run| run.child.as_str() == row.id) {
            row.status = BackgroundStatus::Running.label_de().to_owned();
        }
    }
}

/// Eine Zeile je Lauf für `/agent bg`.
fn run_line(run: &BackgroundRun) -> String {
    let route = run
        .model_route
        .as_deref()
        .map(|route| format!(" · {route}"))
        .unwrap_or_default();
    let mut line = format!(
        "  {} ({}){route} · {} · {} s · {} Werkzeugaufrufe · {} Tokens",
        run.role,
        run.child,
        run.status.label_de(),
        run.elapsed().as_secs(),
        run.progress.tool_calls,
        // Runde 7, Teil A4: einschließlich der laufenden Runde.
        run.progress.tokens_with_live()
    );
    if run.progress.rounds > 0 {
        line.push_str(&format!(" · {} Runden", run.progress.rounds));
    }
    if let Some(step) = &run.progress.last_step {
        line.push_str(&format!(" · {step}"));
    }
    line
}

/// `/agent bg` (Liste) und `/agent cancel <id>` (Abbruch).
pub(crate) fn apply_agents_command(app: &mut ChatApp, args: &str) {
    let args = args.trim();
    let Some(spawner) = app.managed_spawner().cloned() else {
        app.push_line(Role::System, "Keine Agenten-Laufzeit in dieser Sitzung.");
        return;
    };
    if let Some(id) = args.strip_prefix("cancel") {
        let id = id.trim();
        if id.is_empty() {
            app.push_line(Role::System, "Aufruf: /agent cancel <id>");
            return;
        }
        let message = match spawner.cancel_background_child(app.session_id(), id) {
            Ok(true) => format!("Abbruch des Hintergrund-Agenten {id} angefordert."),
            Ok(false) => format!("Der Hintergrund-Agent {id} läuft nicht mehr."),
            Err(error) => error.message,
        };
        tracing::info!(child = id, "tui.background.cancel_command");
        app.push_line(Role::System, message);
        return;
    }
    let runs = spawner.background_children().runs_for(app.session_id());
    if runs.is_empty() {
        app.push_line(Role::System, "Keine Hintergrund-Agenten in dieser Sitzung.");
        return;
    }
    let mut lines = vec![Line::from("Hintergrund-Agenten:")];
    lines.extend(runs.iter().map(|run| Line::from(run_line(run))));
    lines.push(Line::from("Abbrechen: /agent cancel <id>"));
    app.push_lines(lines);
}

/// Bricht alle laufenden Hintergrund-Kinder der Wurzel ab (`/new`,
/// `/resume`, Beenden) und protokolliert das.
///
/// # Returns
/// Anzahl der abgebrochenen Läufe.
pub(crate) fn cancel_all(
    spawner: Option<&Arc<ManagedAgentSpawner>>,
    root: &SessionId,
    reason: &str,
) -> usize {
    let Some(spawner) = spawner else {
        return 0;
    };
    let cancelled = spawner.cancel_all_background(root, reason);
    if !cancelled.is_empty() {
        tracing::info!(
            count = cancelled.len(),
            reason,
            "tui.background.cancelled_on_session_end"
        );
    }
    cancelled.len()
}

/// Entscheidet über `/quit`, solange Hintergrund-Agenten laufen.
///
/// # Beschreibung
/// Ohne laufende Hintergrund-Agenten wird sofort beendet. Sonst fragt das
/// erste `/quit` nach (Systemzeile); ein zweites `/quit` innerhalb von
/// [`QUIT_CONFIRM_WINDOW`] beendet trotzdem. Ein doppeltes Ctrl+C/Ctrl+D
/// (scharfgestellte `pending_quit`) beendet immer sofort.
///
/// # Returns
/// `true`, wenn beendet werden soll.
pub(crate) fn confirm_quit(app: &mut ChatApp) -> bool {
    let running = running(app);
    if running.is_empty() || app.pending_quit.is_some() {
        return true;
    }
    if app
        .background
        .quit_armed_at
        .is_some_and(|at| at.elapsed() <= QUIT_CONFIRM_WINDOW)
    {
        return true;
    }
    app.background.quit_armed_at = Some(Instant::now());
    app.push_line(
        Role::System,
        format!(
            "{} Hintergrund-Agent(en) laufen noch und werden beim Beenden abgebrochen. \
             /quit erneut (oder Ctrl+C zweimal) beendet trotzdem.",
            running.len()
        ),
    );
    false
}

// ── Freigaben im Leerlauf ─────────────────────────────────────────────────────

/// Holt im Leerlauf wartende sudo- und Host-Permit-Fragen ab und öffnet
/// dieselben Fenster wie während eines Turns.
///
/// # Returns
/// `true`, wenn ein Fenster geöffnet wurde (neu zeichnen).
pub(crate) fn poll_idle_prompts(
    app: &mut ChatApp,
    host_permit_prompts: &mut HostPermitPromptReceiver,
) -> bool {
    let mut opened = false;
    if app.sudo.is_listening()
        && let Some(prompt) = app.sudo.try_recv()
    {
        opened |= crate::sudo_dialog::accept_prompt(app, Some(prompt));
    }
    if let Ok(prompt) = host_permit_prompts.try_recv() {
        tracing::info!(
            session = prompt.session(),
            worker = prompt.worker_definition(),
            "tui.host_permit.prompt_shown_idle"
        );
        // Full Access: Sitzungs-Lease ohne Dialog (die Systemzeile braucht
        // trotzdem ein Neuzeichnen).
        if let Some(prompt) = auto_grant_host_permit(app, prompt) {
            app.background.host_permit_shown_at = Some(open_host_permit_prompt(app, prompt));
        }
        opened = true;
    }
    opened
}

/// Ob ein im Leerlauf offenes sudo-Fenster stehen bleiben darf (statt vom
/// Schleifenanfang abgelehnt zu werden): nur solange ein Hintergrund-Agent
/// läuft, der es gestellt haben kann.
pub(crate) fn keeps_idle_prompts(app: &ChatApp) -> bool {
    has_running(app)
}

/// Leitet ein Eingabeereignis im Leerlauf an ein offenes Freigabefenster.
///
/// # Returns
/// - `Ok(redraw)`: das Fenster hat das Ereignis verbraucht.
/// - `Err(event)`: kein Fenster offen (oder Zeichnen/Größe) — normal weiter.
pub(crate) fn route_idle_prompt_event(
    app: &mut ChatApp,
    event: TuiEvent,
) -> Result<bool, TuiEvent> {
    if matches!(event, TuiEvent::Draw | TuiEvent::Resize(..)) {
        return Err(event);
    }
    if app.sudo.is_open() {
        return Ok(crate::sudo_dialog::route_event(app, event));
    }
    if app.pending_host_permit.is_none() {
        return Err(event);
    }
    let TuiEvent::Key(key) = event else {
        // Pastes beantworten nie eine Freigabe; Maus bleibt beim Verlauf.
        return match event {
            TuiEvent::Paste(_) => Ok(false),
            other => Err(other),
        };
    };
    // Ctrl+C lehnt fail-safe sofort ab, unabhängig vom Arming-Delay.
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c' | 'C'))
    {
        if let Some(prompt) = app.pending_host_permit.take() {
            prompt.deny();
        }
        app.pending_host_permit_dialog = None;
        app.background.host_permit_shown_at = None;
        app.push_line(Role::System, "Host-Ausführung abgelehnt.");
        return Ok(true);
    }
    let since_shown = app
        .background
        .host_permit_shown_at
        .map_or(Duration::ZERO, |shown| shown.elapsed());
    if !approval_dialog_key_is_armed(key, since_shown) {
        return Ok(false);
    }
    let Some(dialog) = app.pending_host_permit_dialog.as_mut() else {
        return Ok(false);
    };
    match dialog.handle_key(key) {
        ChoiceAction::Stay => Ok(true),
        ChoiceAction::Cancel => {
            if let Some(prompt) = app.pending_host_permit.take() {
                prompt.deny();
            }
            app.pending_host_permit_dialog = None;
            app.background.host_permit_shown_at = None;
            app.push_line(Role::System, "Host-Ausführung abgelehnt.");
            Ok(true)
        }
        ChoiceAction::Chosen(index) => {
            if let Some(prompt) = app.pending_host_permit.take() {
                apply_host_permit_decision(app, prompt, index);
                // Eine Host-Arbeitsphase kann Terminal-Modi berühren.
                app.needs_terminal_reassert = true;
            }
            app.pending_host_permit_dialog = None;
            app.background.host_permit_shown_at = None;
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests;
