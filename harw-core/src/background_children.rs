//! Hintergrund-Kinder und Orchestrierungsgrenzen (Runde 5, Teil K).
//!
//! # Verantwortungsbereich
//! Zwei eng verwandte Dinge, die beide am [`ManagedAgentSpawner`] hängen:
//!
//! 1. **Orchestrierungsgrenzen** ([`OrchestrationLimits`],
//!    [`check_orchestration_admission`]): wie viele Orchestratoren die
//!    UIA-Wurzel gleichzeitig laufen lassen darf (`max_root_orchestrators`),
//!    wie viele Sub-Orchestratoren je Root-Orchestrator-Baum
//!    (`max_sub_orchestrators`) und wie tief sie verschachtelt sein dürfen
//!    (`max_sub_orchestrator_depth`). Durchgesetzt in der Admission des
//!    Spawners, fail-closed, mit einer Meldung, die mit
//!    [`ORCHESTRATION_LIMIT_MARKER`] beginnt — der Turn-Loop reicht genau
//!    diese Ablehnungen als Werkzeugfehler an das Modell weiter, statt den
//!    Turn abzubrechen ([`is_orchestration_limit_rejection`]).
//! 2. **Hintergrund-Läufe** ([`BackgroundChildren`]): ein von der UIA-Wurzel
//!    gestarteter Orchestrator kann „abgekoppelt" weiterlaufen, während der
//!    Eltern-Turn sofort endet. Solange ein Kind abgekoppelt ist, schließt
//!    `AgentSpawner::child_finished`/`child_completed` es **nicht** (siehe
//!    `child_controller.rs`); erst [`ManagedAgentSpawner::finish_background_child`]
//!    legt das Ergebnis als Benachrichtigung ab und gibt die Admission frei.
//!
//! # Sicherheit
//! Ein Hintergrund-Kind ist dasselbe admittierte Kind wie im synchronen Fall:
//! dieselbe Sandbox, dasselbe Budget, derselbe Cancel-Token, dieselbe
//! Freigabekette. Dieses Modul erweitert nichts; es verschiebt nur, **wann**
//! der Elternteil das Ergebnis sieht. Abbrechen darf nur der eigene
//! Elternteil ([`ManagedAgentSpawner::cancel_background_child`]).
//!
//! # Nebenläufigkeit
//! [`BackgroundChildren`] ist `Send + Sync` (ein `std::sync::Mutex`, der nie
//! über einen `await` gehalten wird, plus ein `tokio::sync::Notify` zum
//! Wecken der Oberfläche).

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use harw_agent_dsl::roles::AgentRoleId;
use harw_extension_api::AgentSpawnError;
use harw_types::SessionId;
use jiff::Timestamp;

use crate::agent_events::{AgentEvent, AgentEventKind};
use crate::child_controller::ManagedAgentSpawner;

/// Anfang jeder Ablehnungsmeldung einer Orchestrierungsgrenze.
///
/// Der Turn-Loop erkennt daran eine Ablehnung, die das Modell selbst lesen
/// und beantworten soll (Werkzeugfehler statt Turn-Abbruch).
pub const ORCHESTRATION_LIMIT_MARKER: &str = "Orchestrierungsgrenze:";

/// Wie viele abgeschlossene Hintergrund-Läufe je Spawner zur Anzeige
/// (`/agent bg`, `agent.status`) vorrätig bleiben.
pub const BACKGROUND_FINISHED_KEEP: usize = 16;

/// Höchstlänge der Kurzfassung eines Ergebnisses in [`BackgroundRun::summary`].
const SUMMARY_MAX_CHARS: usize = 240;

/// `true`, wenn `message` eine Ablehnung durch eine Orchestrierungsgrenze ist.
#[must_use]
pub fn is_orchestration_limit_rejection(message: &str) -> bool {
    message.starts_with(ORCHESTRATION_LIMIT_MARKER)
}

/// `true` für Root- und Kind-Orchestratoren.
#[must_use]
pub fn is_orchestrator_role(role: AgentRoleId) -> bool {
    matches!(
        role,
        AgentRoleId::RootOrchestrator | AgentRoleId::ChildOrchestrator
    )
}

// ── Orchestrierungsgrenzen ────────────────────────────────────────────────────

/// Die in der Admission durchgesetzten Orchestrierungsgrenzen.
///
/// # Beschreibung
/// Die Werte kommen aus `[agents]` (`harw_config::AgentLimitsToml`, bereits
/// geklemmt); die allgemeine Tiefe (`max_spawn_depth`) steckt dagegen in
/// [`crate::ChildLimits::max_depth`]. Die Vorgaben entsprechen der
/// Konfigurationsvorgabe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrchestrationLimits {
    /// Gleichzeitig laufende Orchestratoren der UIA-Wurzel.
    pub max_root_orchestrators: usize,
    /// Gleichzeitig laufende Sub-Orchestratoren je Root-Orchestrator-Baum.
    pub max_sub_orchestrators: usize,
    /// Höchste Verschachtelung der Sub-Orchestratoren (direkt unter dem
    /// Root-Orchestrator = 1; `0` = keine Sub-Orchestratoren).
    pub max_sub_orchestrator_depth: u32,
}

impl Default for OrchestrationLimits {
    fn default() -> Self {
        Self {
            max_root_orchestrators: 1,
            max_sub_orchestrators: 2,
            max_sub_orchestrator_depth: 2,
        }
    }
}

/// Ein admittiertes Kind, wie die Grenzprüfung es sieht.
#[derive(Debug, Clone, Copy)]
pub struct AdmittedNode<'a> {
    /// Sitzungs-ID des Kindes.
    pub child: &'a str,
    /// Sitzungs-ID seines Elternteils.
    pub parent: &'a str,
    /// Registrierter Rollenname.
    pub role_name: &'a str,
    /// Organisationsrolle der registrierten Rolle (`None`: unbekannt).
    pub organizational_role: Option<AgentRoleId>,
}

/// Prüft eine Admission gegen die Orchestrierungsgrenzen.
///
/// # Beschreibung
/// Nur Orchestrator-Ziele werden begrenzt; Worker, UIA-Worker und der
/// Agent-Steward laufen unverändert durch die übrige Admission.
///
/// - **Elternteil UIA-Wurzel** (`UserInterface`): läuft bereits
///   `max_root_orchestrators`-mal ein Orchestrator als direktes Kind der
///   Wurzel (Hintergrund oder synchron), wird jedes weitere
///   Orchestrator-Ziel abgelehnt. UIA-Worker bleiben erlaubt.
/// - **Elternteil in einem Orchestrator-Baum**: ein neuer Sub-Orchestrator
///   bekommt die Ebene „Anzahl Sub-Orchestratoren in der Ahnenkette + 1"; ist
///   sie größer als `max_sub_orchestrator_depth`, oder laufen im Baum des
///   Root-Orchestrators bereits `max_sub_orchestrators` Sub-Orchestratoren,
///   wird abgelehnt.
///
/// # Argumente
/// - `nodes`: alle derzeit admittierten Kinder dieses Spawners.
/// - `parent`: Sitzungs-ID des Elternteils der neuen Admission.
/// - `parent_role`: seine vertrauenswürdige Organisationsrolle.
/// - `target_role`: Organisationsrolle des neuen Kindes.
/// - `limits`: die wirksamen Grenzen.
///
/// # Errors
/// Eine Meldung für das Modell, beginnend mit
/// [`ORCHESTRATION_LIMIT_MARKER`].
pub fn check_orchestration_admission(
    nodes: &[AdmittedNode<'_>],
    parent: &str,
    parent_role: AgentRoleId,
    target_role: AgentRoleId,
    limits: OrchestrationLimits,
) -> Result<(), String> {
    if !is_orchestrator_role(target_role) {
        return Ok(());
    }
    if parent_role == AgentRoleId::UserInterface {
        let running: Vec<&AdmittedNode<'_>> = nodes
            .iter()
            .filter(|node| {
                node.parent == parent && node.organizational_role.is_some_and(is_orchestrator_role)
            })
            .collect();
        if running.len() >= limits.max_root_orchestrators {
            let described = running
                .iter()
                .map(|node| format!("{} ({})", node.role_name, node.child))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "{ORCHESTRATION_LIMIT_MARKER} Grenze max_root_orchestrators={} erreicht — es läuft \
                 bereits: {described}. Nutze agent.status/agent.result oder warte auf seine \
                 Benachrichtigung; für Nebenaufgaben stehen nur UIA-Worker zur Verfügung.",
                limits.max_root_orchestrators
            ));
        }
        return Ok(());
    }
    if target_role != AgentRoleId::ChildOrchestrator {
        return Ok(());
    }

    let by_child: BTreeMap<&str, &AdmittedNode<'_>> =
        nodes.iter().map(|node| (node.child, node)).collect();
    // Ahnenkette des Elternteils: Sub-Orchestratoren zählen, bis der
    // Root-Orchestrator (die Baumwurzel) erreicht ist.
    let mut sub_levels: u32 = 0;
    let mut tree_root: Option<&str> = None;
    let mut cursor = parent;
    for _ in 0..=nodes.len() {
        let Some(node) = by_child.get(cursor) else {
            // Oberstes Glied ohne Admission-Record (extern gefahrene Wurzel):
            // ist es selbst ein Orchestrator, ist es die Baumwurzel.
            if cursor == parent && is_orchestrator_role(parent_role) {
                if parent_role == AgentRoleId::ChildOrchestrator {
                    sub_levels = sub_levels.saturating_add(1);
                }
                tree_root = Some(cursor);
            } else if tree_root.is_none() && sub_levels > 0 {
                tree_root = Some(cursor);
            }
            break;
        };
        match node.organizational_role {
            Some(AgentRoleId::RootOrchestrator) => {
                tree_root = Some(node.child);
                break;
            }
            Some(AgentRoleId::ChildOrchestrator) => sub_levels = sub_levels.saturating_add(1),
            _ => {}
        }
        cursor = node.parent;
    }

    let new_level = sub_levels.saturating_add(1);
    if new_level > limits.max_sub_orchestrator_depth {
        return Err(format!(
            "{ORCHESTRATION_LIMIT_MARKER} Grenze max_sub_orchestrator_depth={} erreicht — ein \
             Sub-Orchestrator auf Ebene {new_level} ist nicht erlaubt; nutze Worker.",
            limits.max_sub_orchestrator_depth
        ));
    }

    let Some(tree_root) = tree_root else {
        return Ok(());
    };
    let in_tree = |start: &str| {
        let mut cursor = start;
        for _ in 0..=nodes.len() {
            if cursor == tree_root {
                return true;
            }
            match by_child.get(cursor) {
                Some(node) => cursor = node.parent,
                None => return false,
            }
        }
        false
    };
    let running_subs = nodes
        .iter()
        .filter(|node| {
            node.organizational_role == Some(AgentRoleId::ChildOrchestrator) && in_tree(node.parent)
        })
        .count();
    if running_subs >= limits.max_sub_orchestrators {
        return Err(format!(
            "{ORCHESTRATION_LIMIT_MARKER} Grenze max_sub_orchestrators={} erreicht; nutze Worker \
             oder warte auf ein laufendes Kind.",
            limits.max_sub_orchestrators
        ));
    }
    Ok(())
}

// ── Hintergrund-Läufe ─────────────────────────────────────────────────────────

/// Zustand eines Hintergrund-Laufs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundStatus {
    /// Läuft noch.
    Running,
    /// Regulär beendet.
    Completed,
    /// Mit Fehler beendet.
    Failed,
    /// Abgebrochen (`agent.cancel`, `/agent cancel`, `/new`, Beenden).
    Cancelled,
}

impl BackgroundStatus {
    /// Stabiles Kurzlabel (`running`, `completed`, `failed`, `cancelled`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Deutsches Anzeigewort für die Oberfläche.
    #[must_use]
    pub fn label_de(self) -> &'static str {
        match self {
            Self::Running => "läuft im Hintergrund",
            Self::Completed => "fertig",
            Self::Failed => "fehlgeschlagen",
            Self::Cancelled => "abgebrochen",
        }
    }
}

/// Fortschritt eines Hintergrund-Laufs (aus dem Agenten-Ereignisbus).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackgroundProgress {
    /// Werkzeugaufrufe des Kindes und seiner direkten Kinder.
    pub tool_calls: u32,
    /// Neue Tokens (ungecachte Eingabe plus Ausgabe) des Kindes aus
    /// **abgeschlossenen** Modellrunden.
    pub tokens: u64,
    /// Zuletzt beobachteter Schritt (Werkzeugname oder gestartetes Kind).
    pub last_step: Option<String>,
    /// Runde 7, Teil A4: neue Tokens der gerade **laufenden** Modellrunde
    /// (aus `UsageUpdated { final_round: false }`); beim Rundenende in
    /// [`Self::tokens`] übernommen und auf 0 gesetzt.
    pub live_round_tokens: u64,
    /// Runde 7, Teil A4: abgeschlossene Modellrunden des Kindes.
    pub rounds: u32,
    /// Runde 7, Teil A4: zuletzt gemeldete Kontextbelegung (Prompt-Tokens)
    /// des Kindes; `None`, solange keine gemeldet wurde.
    pub context_tokens: Option<u64>,
}

impl BackgroundProgress {
    /// Tokens einschließlich der laufenden Runde (Runde 7, Teil A4).
    #[must_use]
    pub fn tokens_with_live(&self) -> u64 {
        self.tokens.saturating_add(self.live_round_tokens)
    }
}

/// Ein Hintergrund-Lauf, wie `/agent bg` und `agent.status` ihn zeigen.
#[derive(Debug, Clone)]
pub struct BackgroundRun {
    /// Das abgekoppelte Kind.
    pub child: SessionId,
    /// Sein Elternteil (die UIA-Wurzel).
    pub parent: SessionId,
    /// Rollenname des Kindes.
    pub role: String,
    /// Kurzform des Auftrags (erste Zeile, gekürzt).
    pub task: Option<String>,
    /// Startzeitpunkt (Wanduhr).
    pub started_at: Timestamp,
    /// Startzeitpunkt (monoton, für die Laufzeit).
    started: Instant,
    /// Laufzeit bis zum Ende; `None`, solange es läuft.
    pub finished_after: Option<Duration>,
    /// Aktueller Zustand.
    pub status: BackgroundStatus,
    /// Fortschritt.
    pub progress: BackgroundProgress,
    /// Kurzfassung des Ergebnisses (nur nach dem Ende).
    pub summary: Option<String>,
    /// Provider und Modell des Kindes als `<provider>/<modell>` (bei der
    /// Abkopplung aus dem Admission-Record, [`BackgroundChildren::set_model_route`]).
    pub model_route: Option<String>,
}

impl BackgroundRun {
    /// Laufzeit bisher bzw. bis zum Ende.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.finished_after
            .unwrap_or_else(|| self.started.elapsed())
    }

    /// `true`, solange der Lauf nicht beendet ist.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.status == BackgroundStatus::Running
    }
}

/// Eine noch nicht zugestellte Abschlussmeldung für den Elternteil.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundNotice {
    /// Das beendete Kind.
    pub child: SessionId,
    /// Sein Elternteil.
    pub parent: SessionId,
    /// Rollenname.
    pub role: String,
    /// Endzustand.
    pub status: BackgroundStatus,
    /// Ergebnis bzw. Übergabe des Kindes, bereits wie im synchronen
    /// Rückgabeweg gedeckelt (Rest über `agent.result`).
    pub text: String,
    /// Laufzeit.
    pub elapsed: Duration,
    /// Provider und Modell des Kindes (`<provider>/<modell>`), falls bekannt.
    pub model_route: Option<String>,
}

#[derive(Debug, Default)]
struct BackgroundState {
    runs: BTreeMap<String, BackgroundRun>,
    notices: VecDeque<BackgroundNotice>,
}

/// Register der Hintergrund-Läufe und der Orchestrierungsgrenzen eines
/// Spawners.
#[derive(Debug)]
pub struct BackgroundChildren {
    limits: Mutex<OrchestrationLimits>,
    state: Mutex<BackgroundState>,
    wake: tokio::sync::Notify,
}

impl Default for BackgroundChildren {
    fn default() -> Self {
        Self {
            limits: Mutex::new(OrchestrationLimits::default()),
            state: Mutex::new(BackgroundState::default()),
            wake: tokio::sync::Notify::new(),
        }
    }
}

/// Erste nicht leere Zeile, auf `max_chars` Zeichen gekürzt.
fn first_line_excerpt(text: &str, max_chars: usize) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let mut excerpt: String = line.chars().take(max_chars).collect();
    if line.chars().count() > max_chars {
        excerpt.push('…');
    }
    Some(excerpt)
}

impl BackgroundChildren {
    fn with_state<R>(&self, f: impl FnOnce(&mut BackgroundState) -> R) -> R {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut state)
    }

    /// Die wirksamen Orchestrierungsgrenzen.
    #[must_use]
    pub fn limits(&self) -> OrchestrationLimits {
        *self.limits.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Setzt die Orchestrierungsgrenzen (Montage).
    pub fn set_limits(&self, limits: OrchestrationLimits) {
        *self.limits.lock().unwrap_or_else(PoisonError::into_inner) = limits;
    }

    /// Trägt einen laufenden Hintergrund-Lauf ein.
    ///
    /// # Returns
    /// `false`, wenn das Kind bereits als laufend eingetragen war.
    pub fn register(
        &self,
        child: &SessionId,
        parent: &SessionId,
        role: &str,
        task: Option<&str>,
    ) -> bool {
        self.with_state(|state| {
            if state
                .runs
                .get(child.as_str())
                .is_some_and(BackgroundRun::is_running)
            {
                return false;
            }
            state.runs.insert(
                child.as_str().to_owned(),
                BackgroundRun {
                    child: child.clone(),
                    parent: parent.clone(),
                    role: role.to_owned(),
                    task: task.and_then(|task| first_line_excerpt(task, 120)),
                    started_at: Timestamp::now(),
                    started: Instant::now(),
                    finished_after: None,
                    status: BackgroundStatus::Running,
                    progress: BackgroundProgress::default(),
                    summary: None,
                    model_route: None,
                },
            );
            true
        })
    }

    /// Hält Provider und Modell eines eingetragenen Laufs fest (Anzeige
    /// `<provider>/<modell>` in `/agent bg`, `agent.status` und der
    /// Abschlussmeldung).
    pub fn set_model_route(&self, child: &SessionId, route: Option<String>) {
        self.with_state(|state| {
            if let Some(run) = state.runs.get_mut(child.as_str()) {
                run.model_route = route;
            }
        });
    }

    /// `true`, solange `child` als laufender Hintergrund-Lauf eingetragen ist
    /// (dann schließt `child_finished` es nicht).
    #[must_use]
    pub fn is_detached(&self, child: &SessionId) -> bool {
        self.with_state(|state| {
            state
                .runs
                .get(child.as_str())
                .is_some_and(BackgroundRun::is_running)
        })
    }

    /// Schließt einen Lauf ab und legt die Benachrichtigung ab.
    ///
    /// # Returns
    /// Die abgelegte Benachrichtigung; `None`, wenn das Kind nicht als
    /// laufend eingetragen war.
    pub fn finish(
        &self,
        child: &SessionId,
        status: BackgroundStatus,
        text: String,
    ) -> Option<BackgroundNotice> {
        let notice = self.with_state(|state| {
            let run = state.runs.get_mut(child.as_str())?;
            if !run.is_running() {
                return None;
            }
            let elapsed = run.started.elapsed();
            run.status = status;
            run.finished_after = Some(elapsed);
            run.summary = first_line_excerpt(&text, SUMMARY_MAX_CHARS);
            let notice = BackgroundNotice {
                child: run.child.clone(),
                parent: run.parent.clone(),
                role: run.role.clone(),
                status,
                text,
                elapsed,
                model_route: run.model_route.clone(),
            };
            state.notices.push_back(notice.clone());
            // Nur die jüngsten abgeschlossenen Läufe bleiben zur Anzeige.
            let mut finished: Vec<(Duration, String)> = state
                .runs
                .values()
                .filter(|run| !run.is_running())
                .map(|run| (run.started.elapsed(), run.child.as_str().to_owned()))
                .collect();
            if finished.len() > BACKGROUND_FINISHED_KEEP {
                finished.sort_by_key(|entry| std::cmp::Reverse(entry.0));
                let excess = finished.len() - BACKGROUND_FINISHED_KEEP;
                for (_, id) in finished.into_iter().take(excess) {
                    state.runs.remove(&id);
                }
            }
            Some(notice)
        });
        if notice.is_some() {
            self.wake.notify_one();
        }
        notice
    }

    /// Nimmt alle noch nicht zugestellten Benachrichtigungen für `parent`.
    #[must_use]
    pub fn take_notices(&self, parent: &SessionId) -> Vec<BackgroundNotice> {
        self.with_state(|state| {
            let (mine, rest): (VecDeque<_>, VecDeque<_>) = state
                .notices
                .drain(..)
                .partition(|notice| &notice.parent == parent);
            state.notices = rest;
            mine.into_iter().collect()
        })
    }

    /// `true`, wenn für `parent` Benachrichtigungen warten.
    #[must_use]
    pub fn has_notices(&self, parent: &SessionId) -> bool {
        self.with_state(|state| state.notices.iter().any(|notice| &notice.parent == parent))
    }

    /// Wartet, bis eine neue Benachrichtigung abgelegt wurde (auch eine
    /// bereits vor dem Aufruf abgelegte weckt sofort).
    pub async fn notified(&self) {
        self.wake.notified().await;
    }

    /// Alle Läufe (laufend und zuletzt beendet) eines Elternteils, nach
    /// Startzeit sortiert.
    #[must_use]
    pub fn runs_for(&self, parent: &SessionId) -> Vec<BackgroundRun> {
        let mut runs: Vec<BackgroundRun> = self.with_state(|state| {
            state
                .runs
                .values()
                .filter(|run| &run.parent == parent)
                .cloned()
                .collect()
        });
        runs.sort_by_key(|run| run.started_at);
        runs
    }

    /// Laufende Hintergrund-Läufe eines Elternteils.
    #[must_use]
    pub fn running_for(&self, parent: &SessionId) -> Vec<BackgroundRun> {
        self.runs_for(parent)
            .into_iter()
            .filter(BackgroundRun::is_running)
            .collect()
    }

    /// Ein einzelner Lauf, nur für seinen Elternteil sichtbar.
    #[must_use]
    pub fn run_for(&self, parent: &SessionId, child: &str) -> Option<BackgroundRun> {
        self.with_state(|state| {
            state
                .runs
                .get(child)
                .filter(|run| &run.parent == parent)
                .cloned()
        })
    }

    /// Ergänzt den Fortschritt eines laufenden Kindes.
    pub fn record_progress(&self, child: &str, update: impl FnOnce(&mut BackgroundProgress)) {
        self.with_state(|state| {
            if let Some(run) = state.runs.get_mut(child).filter(|run| run.is_running()) {
                update(&mut run.progress);
            }
        });
    }

    /// Wertet ein Ereignis des Agenten-Busses für den Fortschritt aus.
    ///
    /// # Beschreibung
    /// Zählt Werkzeugaufrufe und neue Tokens des Hintergrund-Kindes selbst
    /// sowie Werkzeugaufrufe seiner **direkten** Kinder; der letzte Schritt
    /// ist der jüngste Werkzeugname bzw. „startet <rolle>".
    pub fn observe_event(&self, event: &AgentEvent) {
        let AgentEventKind::Turn(turn_event) = &event.kind else {
            return;
        };
        let own = event.agent.as_str();
        let (target, prefix) = if self.is_running_id(own) {
            (own.to_owned(), None)
        } else if let Some(parent) = event
            .parent
            .as_ref()
            .filter(|parent| self.is_running_id(parent.as_str()))
        {
            (parent.as_str().to_owned(), Some(event.role.clone()))
        } else {
            return;
        };
        let step = |name: &str| match &prefix {
            Some(role) => format!("{role}: {name}"),
            None => name.to_owned(),
        };
        match turn_event {
            harw_protocol::TurnEvent::ToolCallRequested { tool_name, .. } => {
                let step = step(tool_name.as_str());
                self.record_progress(&target, |progress| {
                    progress.tool_calls = progress.tool_calls.saturating_add(1);
                    progress.last_step = Some(step);
                });
            }
            // Runde 7, Teil A4: Zwischenstand der laufenden Runde live.
            harw_protocol::TurnEvent::UsageUpdated {
                round,
                final_round: false,
                ..
            } if prefix.is_none() => {
                let fresh = round.fresh_tokens();
                self.record_progress(&target, |progress| {
                    progress.live_round_tokens = fresh;
                });
            }
            harw_protocol::TurnEvent::UsageUpdated {
                round,
                final_round: true,
                ..
            } if prefix.is_none() => {
                let fresh = round.fresh_tokens();
                self.record_progress(&target, |progress| {
                    progress.tokens = progress.tokens.saturating_add(fresh);
                    progress.live_round_tokens = 0;
                    progress.rounds = progress.rounds.saturating_add(1);
                });
            }
            harw_protocol::TurnEvent::ContextUpdated { used_tokens, .. } if prefix.is_none() => {
                let used = *used_tokens;
                self.record_progress(&target, |progress| {
                    progress.context_tokens = Some(used);
                });
            }
            harw_protocol::TurnEvent::ChildSpawned { role, .. } => {
                let step = step(format!("startet {role}").as_str());
                self.record_progress(&target, |progress| progress.last_step = Some(step));
            }
            _ => {}
        }
    }

    fn is_running_id(&self, child: &str) -> bool {
        self.with_state(|state| state.runs.get(child).is_some_and(BackgroundRun::is_running))
    }
}

// ── Anbindung an den Spawner ──────────────────────────────────────────────────

impl ManagedAgentSpawner {
    /// Das Register der Hintergrund-Läufe und Orchestrierungsgrenzen.
    #[must_use]
    pub fn background_children(&self) -> &Arc<BackgroundChildren> {
        &self.background
    }

    /// Setzt die Orchestrierungsgrenzen (Montage, Runde 5 Teil K).
    #[must_use]
    pub fn with_orchestration_limits(self, limits: OrchestrationLimits) -> Self {
        self.background.set_limits(limits);
        self
    }

    /// Koppelt ein admittiertes Kind ab: es läuft im Hintergrund weiter,
    /// während der Elternteil sein Werkzeugergebnis sofort bekommt.
    ///
    /// # Beschreibung
    /// Danach schließt `child_finished`/`child_completed` das Kind **nicht**;
    /// das tut erst [`Self::finish_background_child`]. Rechte, Budget und
    /// Sandbox bleiben die der Admission; der Cancel-Token wird vom
    /// Eltern-Turn entkoppelt (Runde 5, Teil O, siehe unten).
    ///
    /// # Errors
    /// [`AgentSpawnError`], wenn das Kind nicht (mehr) admittiert ist oder
    /// bereits abgekoppelt läuft.
    pub fn detach_for_background(
        &self,
        child: &SessionId,
        task: Option<&str>,
    ) -> Result<BackgroundRun, AgentSpawnError> {
        let record = self.child_record(child).ok_or_else(|| AgentSpawnError {
            message: format!("child {child} is not admitted; it cannot run in the background"),
        })?;
        if !self
            .background
            .register(child, &record.parent, &record.role, task)
        {
            return Err(AgentSpawnError {
                message: format!("child {child} already runs in the background"),
            });
        }
        // Anzeige `<provider>/<modell>`: das Modell, das das Kind bei der
        // Admission bekommen hat.
        self.background.set_model_route(child, record.model_route());
        // Runde 5, Teil O: eigener Token — ein Abbruch des startenden
        // UIA-Turns reißt das Hintergrund-Kind nicht mehr mit.
        let own_token = self.detach_cancel_token(child);
        tracing::info!(
            child = %child,
            parent = %record.parent,
            role = %record.role,
            own_token,
            "background_child.detached"
        );
        self.background
            .run_for(&record.parent, child.as_str())
            .ok_or_else(|| AgentSpawnError {
                message: format!("background run of {child} vanished"),
            })
    }

    /// Schließt einen Hintergrund-Lauf ab: Benachrichtigung ablegen, dann
    /// die Admission freigeben (wie `child_finished` im synchronen Fall).
    ///
    /// # Returns
    /// Die abgelegte Benachrichtigung (`None`, wenn das Kind nicht
    /// abgekoppelt lief — dann wird trotzdem freigegeben).
    pub fn finish_background_child(
        &self,
        child: &SessionId,
        status: BackgroundStatus,
        text: String,
    ) -> Option<BackgroundNotice> {
        let notice = self.background.finish(child, status, text);
        tracing::info!(
            child = %child,
            status = status.as_str(),
            "background_child.finished"
        );
        self.close_child(child);
        notice
    }

    /// Bricht einen eigenen Hintergrund-Lauf ab.
    ///
    /// # Argumente
    /// - `caller`: die aufrufende Sitzung (aus dem Ausführungskontext).
    /// - `child`: die Kind-ID.
    ///
    /// # Returns
    /// `Ok(true)`, wenn der Abbruch angefordert wurde; `Ok(false)`, wenn der
    /// Lauf bereits beendet war.
    ///
    /// # Errors
    /// [`AgentSpawnError`] mit immer derselben Meldung, wenn `child` kein
    /// Hintergrund-Lauf von `caller` ist — kein Orakel über fremde Kinder.
    pub fn cancel_background_child(
        &self,
        caller: &SessionId,
        child: &str,
    ) -> Result<bool, AgentSpawnError> {
        let run = self
            .background
            .run_for(caller, child)
            .ok_or_else(|| AgentSpawnError {
                message: format!("kein eigener Hintergrund-Agent mit der ID {child}"),
            })?;
        if !run.is_running() {
            return Ok(false);
        }
        let requested = self.request_cancellation(&run.child);
        tracing::info!(child = %run.child, requested, "background_child.cancel_requested");
        Ok(requested)
    }

    /// Bricht alle laufenden Hintergrund-Läufe eines Elternteils ab
    /// (`/new`, `/resume`, Beenden) und protokolliert das.
    ///
    /// # Returns
    /// Die abgebrochenen Läufe.
    pub fn cancel_all_background(&self, parent: &SessionId, reason: &str) -> Vec<BackgroundRun> {
        let running = self.background.running_for(parent);
        for run in &running {
            let requested = self.request_cancellation(&run.child);
            tracing::info!(
                child = %run.child,
                role = %run.role,
                reason,
                requested,
                "background_child.cancelled_on_session_end"
            );
        }
        running
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn node<'a>(child: &'a str, parent: &'a str, role: AgentRoleId) -> AdmittedNode<'a> {
        AdmittedNode {
            child,
            parent,
            role_name: match role {
                AgentRoleId::RootOrchestrator => "root-orchestrator",
                AgentRoleId::ChildOrchestrator => "coding-orchestrator",
                AgentRoleId::UiaWorker => "uia-worker",
                _ => "worker",
            },
            organizational_role: Some(role),
        }
    }

    const UIA: &str = "uia-root";

    #[test]
    fn a_second_root_orchestrator_is_rejected_with_a_clear_message() -> TestResult {
        let limits = OrchestrationLimits::default();
        assert!(
            check_orchestration_admission(
                &[],
                UIA,
                AgentRoleId::UserInterface,
                AgentRoleId::RootOrchestrator,
                limits
            )
            .is_ok()
        );
        let nodes = [node("root-1", UIA, AgentRoleId::RootOrchestrator)];
        let Err(message) = check_orchestration_admission(
            &nodes,
            UIA,
            AgentRoleId::UserInterface,
            AgentRoleId::RootOrchestrator,
            limits,
        ) else {
            return Err(TestError::Unexpected(
                "zweiter Root-Orchestrator zugelassen".into(),
            ));
        };
        assert!(is_orchestration_limit_rejection(&message));
        assert!(message.contains("root-orchestrator (root-1)"));
        assert!(message.contains("agent.status"));
        assert!(message.contains("UIA-Worker"));
        Ok(())
    }

    #[test]
    fn another_orchestrator_from_the_uia_is_rejected_but_uia_workers_pass() {
        let limits = OrchestrationLimits::default();
        let nodes = [node("root-1", UIA, AgentRoleId::RootOrchestrator)];
        assert!(
            check_orchestration_admission(
                &nodes,
                UIA,
                AgentRoleId::UserInterface,
                AgentRoleId::ChildOrchestrator,
                limits
            )
            .is_err()
        );
        for role in [
            AgentRoleId::UiaWorker,
            AgentRoleId::AgentSteward,
            AgentRoleId::Worker,
        ] {
            assert!(
                check_orchestration_admission(
                    &nodes,
                    UIA,
                    AgentRoleId::UserInterface,
                    role,
                    limits
                )
                .is_ok(),
                "{role:?} bleibt erlaubt"
            );
        }
    }

    #[test]
    fn the_root_limit_of_two_admits_exactly_two() {
        let limits = OrchestrationLimits {
            max_root_orchestrators: 2,
            ..OrchestrationLimits::default()
        };
        let one = [node("root-1", UIA, AgentRoleId::RootOrchestrator)];
        assert!(
            check_orchestration_admission(
                &one,
                UIA,
                AgentRoleId::UserInterface,
                AgentRoleId::RootOrchestrator,
                limits
            )
            .is_ok()
        );
        let two = [
            node("root-1", UIA, AgentRoleId::RootOrchestrator),
            node("root-2", UIA, AgentRoleId::RootOrchestrator),
        ];
        assert!(
            check_orchestration_admission(
                &two,
                UIA,
                AgentRoleId::UserInterface,
                AgentRoleId::RootOrchestrator,
                limits
            )
            .is_err()
        );
    }

    #[test]
    fn after_the_orchestrator_ends_a_new_one_is_allowed_again() {
        // „Beendet" heißt: kein Admission-Record mehr — nur Worker übrig.
        let nodes = [node("helper", UIA, AgentRoleId::UiaWorker)];
        assert!(
            check_orchestration_admission(
                &nodes,
                UIA,
                AgentRoleId::UserInterface,
                AgentRoleId::RootOrchestrator,
                OrchestrationLimits::default()
            )
            .is_ok()
        );
    }

    #[test]
    fn orchestrator_internal_spawns_are_not_affected_by_the_root_limit() {
        // Der Root-Orchestrator selbst startet Sub-Orchestratoren und Worker.
        let nodes = [node("root-1", UIA, AgentRoleId::RootOrchestrator)];
        let limits = OrchestrationLimits::default();
        assert!(
            check_orchestration_admission(
                &nodes,
                "root-1",
                AgentRoleId::RootOrchestrator,
                AgentRoleId::ChildOrchestrator,
                limits
            )
            .is_ok()
        );
        assert!(
            check_orchestration_admission(
                &nodes,
                "root-1",
                AgentRoleId::RootOrchestrator,
                AgentRoleId::Worker,
                limits
            )
            .is_ok()
        );
    }

    #[test]
    fn the_sub_orchestrator_count_is_per_root_tree() -> TestResult {
        let limits = OrchestrationLimits::default(); // max 2
        let nodes = [
            node("root-1", UIA, AgentRoleId::RootOrchestrator),
            node("sub-a", "root-1", AgentRoleId::ChildOrchestrator),
            node("sub-b", "sub-a", AgentRoleId::ChildOrchestrator),
            node("worker", "sub-b", AgentRoleId::Worker),
        ];
        let Err(message) = check_orchestration_admission(
            &nodes,
            "root-1",
            AgentRoleId::RootOrchestrator,
            AgentRoleId::ChildOrchestrator,
            limits,
        ) else {
            return Err(TestError::Unexpected(
                "Grenze max_sub_orchestrators hätte greifen müssen".into(),
            ));
        };
        assert!(message.contains("max_sub_orchestrators=2"));
        // Ein anderer Baum ist unberührt.
        let other = [
            node("root-2", "other-uia", AgentRoleId::RootOrchestrator),
            node("sub-a", "root-1", AgentRoleId::ChildOrchestrator),
            node("sub-b", "root-1", AgentRoleId::ChildOrchestrator),
        ];
        assert!(
            check_orchestration_admission(
                &other,
                "root-2",
                AgentRoleId::RootOrchestrator,
                AgentRoleId::ChildOrchestrator,
                limits
            )
            .is_ok()
        );
        Ok(())
    }

    #[test]
    fn the_sub_orchestrator_depth_is_enforced() {
        let limits = OrchestrationLimits {
            max_sub_orchestrators: 6,
            max_sub_orchestrator_depth: 2,
            ..OrchestrationLimits::default()
        };
        let nodes = [
            node("root-1", UIA, AgentRoleId::RootOrchestrator),
            node("sub-1", "root-1", AgentRoleId::ChildOrchestrator),
            node("sub-2", "sub-1", AgentRoleId::ChildOrchestrator),
        ];
        // Ebene 2 (unter sub-1) ist erlaubt …
        assert!(
            check_orchestration_admission(
                &nodes[..2],
                "sub-1",
                AgentRoleId::ChildOrchestrator,
                AgentRoleId::ChildOrchestrator,
                limits
            )
            .is_ok()
        );
        // … Ebene 3 (unter sub-2) nicht.
        let result = check_orchestration_admission(
            &nodes,
            "sub-2",
            AgentRoleId::ChildOrchestrator,
            AgentRoleId::ChildOrchestrator,
            limits,
        );
        assert!(
            result
                .as_ref()
                .is_err_and(|message| message.contains("max_sub_orchestrator_depth=2")),
            "{result:?}"
        );
    }

    #[test]
    fn background_registry_tracks_runs_and_delivers_notices_once() {
        let registry = BackgroundChildren::default();
        let parent = SessionId::new();
        let child = SessionId::new();
        assert!(registry.register(&child, &parent, "root-orchestrator", Some("Baue X\nmehr")));
        assert!(!registry.register(&child, &parent, "root-orchestrator", None));
        assert!(registry.is_detached(&child));
        assert_eq!(registry.running_for(&parent).len(), 1);
        assert_eq!(
            registry.running_for(&parent)[0].task.as_deref(),
            Some("Baue X")
        );
        registry.record_progress(child.as_str(), |progress| progress.tool_calls = 3);

        let notice = registry.finish(&child, BackgroundStatus::Completed, "Ergebnis".to_owned());
        assert!(notice.is_some());
        assert!(!registry.is_detached(&child));
        assert!(registry.running_for(&parent).is_empty());
        assert!(registry.has_notices(&parent));
        let notices = registry.take_notices(&parent);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].text, "Ergebnis");
        assert!(registry.take_notices(&parent).is_empty());
        // Ein zweites Beenden legt nichts erneut ab.
        assert!(
            registry
                .finish(&child, BackgroundStatus::Failed, String::new())
                .is_none()
        );
        let run = registry.run_for(&parent, child.as_str());
        assert!(run.is_some_and(|run| run.progress.tool_calls == 3 && run.summary.is_some()));
        // Fremde Elternteile sehen den Lauf nicht.
        assert!(
            registry
                .run_for(&SessionId::new(), child.as_str())
                .is_none()
        );
    }

    /// Anzeige `<provider>/<modell>`: die bei der Abkopplung festgehaltene
    /// Route erscheint im Lauf und in der Abschlussmeldung.
    #[test]
    fn model_route_reaches_run_and_notice() {
        let registry = BackgroundChildren::default();
        let parent = SessionId::new();
        let child = SessionId::new();
        assert!(registry.register(&child, &parent, "root-orchestrator", None));
        registry.set_model_route(&child, Some("openai/gpt-5".to_owned()));
        assert_eq!(
            registry
                .run_for(&parent, child.as_str())
                .and_then(|run| run.model_route)
                .as_deref(),
            Some("openai/gpt-5")
        );
        let notice = registry.finish(&child, BackgroundStatus::Failed, "x".to_owned());
        assert_eq!(
            notice.and_then(|notice| notice.model_route).as_deref(),
            Some("openai/gpt-5")
        );
    }

    // --- Runde 7, Teil A4: Live-Status ------------------------------------

    fn usage_event(child: &SessionId, fresh_output: u64, final_round: bool) -> AgentEvent {
        let round = harw_types::TokenUsage {
            output_tokens: fresh_output,
            ..harw_types::TokenUsage::default()
        };
        AgentEvent {
            agent: child.clone(),
            parent: None,
            role: "root-orchestrator".to_owned(),
            kind: crate::agent_events::AgentEventKind::Turn(
                harw_protocol::TurnEvent::UsageUpdated {
                    turn_id: harw_types::TurnId::new(),
                    round: round.clone(),
                    turn_total: round,
                    final_round,
                },
            ),
        }
    }

    #[test]
    fn live_round_tokens_are_visible_during_a_round_and_taken_over_at_its_end() -> TestResult {
        let registry = BackgroundChildren::default();
        let parent = SessionId::new();
        let child = SessionId::new();
        assert!(registry.register(&child, &parent, "root-orchestrator", Some("Analyse")));

        registry.observe_event(&usage_event(&child, 120, false));
        let run = registry
            .run_for(&parent, child.as_str())
            .ok_or(TestError::Missing("laufender Lauf"))?;
        assert_eq!(run.progress.tokens, 0);
        assert_eq!(run.progress.live_round_tokens, 120);
        assert_eq!(
            run.progress.tokens_with_live(),
            120,
            "nicht mehr 0 während der Runde"
        );

        registry.observe_event(&usage_event(&child, 300, true));
        let run = registry
            .run_for(&parent, child.as_str())
            .ok_or(TestError::Missing("laufender Lauf"))?;
        assert_eq!(run.progress.tokens, 300);
        assert_eq!(run.progress.live_round_tokens, 0);
        assert_eq!(run.progress.rounds, 1);

        registry.observe_event(&AgentEvent {
            agent: child.clone(),
            parent: None,
            role: "root-orchestrator".to_owned(),
            kind: crate::agent_events::AgentEventKind::Turn(
                harw_protocol::TurnEvent::ContextUpdated {
                    turn_id: harw_types::TurnId::new(),
                    used_tokens: 42_000,
                    window_tokens: 200_000,
                    history_items_dropped: 0,
                    estimated_next_tokens: None,
                    threshold_tokens: None,
                    reserve_tokens: None,
                },
            ),
        });
        let run = registry
            .run_for(&parent, child.as_str())
            .ok_or(TestError::Missing("laufender Lauf"))?;
        assert_eq!(run.progress.context_tokens, Some(42_000));
        Ok(())
    }
}
