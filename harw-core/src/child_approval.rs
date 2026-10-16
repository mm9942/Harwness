//! Freigabe-Fragen von Kind-Agenten an die Nutzerin (Runde 5, Teil O).
//!
//! # Verantwortungsbereich
//! Ein Kind hat keinen eigenen Responder (`ApprovalChain::for_child`): liefert
//! seine Freigabekette `AskUser`, pausiert sein Turn mit
//! [`TurnOutcome::AwaitingApproval`]. Ohne Pausenrecht (`[lifecycle]`,
//! `ChildRecord::allow_pause == false`) wies der Spawner diesen Ausgang
//! bisher sofort als Vertragsbruch ab — im Praxis-Transkript scheiterte
//! so ein `uia-worker` nach 8 s mit `awaiting_approval`.
//!
//! Dieses Modul gibt dem Spawner einen optionalen **Freigabe-Kanal**
//! ([`ChildApprovalRelay`] mit einem [`ChildApprovalBroker`], den nur die TUI
//! anbindet). Ist er angebunden, beantwortet [`relay_child_approvals`] jede
//! Freigabepause eines pausierverbotenen Kindes selbst:
//!
//! 1. Der festgehaltene Aufruf wird gegen die **eigene** Werkzeugfläche des
//!    Kindes geprüft ([`crate::turn_loop::find_executor`]: registriert und
//!    aktiviert). Ein Werkzeug außerhalb dieser Fläche wird abgelehnt, ohne
//!    die Nutzerin zu fragen — eine Freigabe erweitert nie Rechte.
//! 2. Sonst geht die Frage mit Absender ([`ChildApprovalRequest::role`],
//!    [`ChildApprovalRequest::tree_path`]) an die Oberfläche; das Kind wartet
//!    höchstens [`CHILD_APPROVAL_TIMEOUT`].
//! 3. Zustimmung → `resume_after_approval` mit `Approve`: das Werkzeug läuft
//!    in der Registry des Kindes. Ablehnung, Zeitablauf oder eine nicht
//!    zustellbare Frage → `Reject`: das Kind bekommt einen Werkzeugfehler
//!    und arbeitet weiter, statt abzustürzen.
//!
//! Ohne angebundenen Kanal wird die konkrete Freigabe fail-closed abgelehnt
//! und der Kind-Turn weitergeführt. Damit blockiert ein asynchroner Agent-Job
//! nicht nur deshalb, weil seine Oberfläche keine Approval-UI besitzt.
//!
//! # Nebenläufigkeit
//! [`ChildApprovalRelay`] ist `Send + Sync`; die Sperre um den Broker wird
//! nie über einen `await` gehalten. Die Antwort kommt über einen
//! `tokio::sync::oneshot`-Kanal. Wird der wartende Kind-Lauf abgebrochen
//! (Cancel-Token), fällt der Empfänger weg — die Oberfläche erkennt das an
//! `oneshot::Sender::is_closed` und schließt ihre Frage.

use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use harw_session_store::ApprovalStore;
use harw_tools::ToolCall;
use harw_types::SessionId;
use tokio::sync::oneshot;

use crate::ModelProvider;
use crate::child_controller::ManagedAgentSpawner;
use crate::error::CoreResult;
use crate::session::AgentSession;
use crate::state_store::StateStore;
use crate::turn_loop::{
    ApprovalResolution, TurnOutcome, find_executor, resume_after_approval,
    resume_after_approval_durable,
};

/// Wie lange ein Kind höchstens auf die Antwort der Nutzerin wartet.
pub const CHILD_APPROVAL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Obergrenze der Freigabe-Runden eines einzelnen Kind-Laufs.
///
/// Schutz gegen ein Modell, das nach jeder Antwort sofort wieder fragt;
/// danach gilt wieder das fail-closed-Verhalten.
pub const CHILD_APPROVAL_MAX_ROUNDS: usize = 64;

/// Ablehnungsgrund bei Zeitablauf.
pub const REASON_CHILD_APPROVAL_TIMEOUT: &str = "Freigabe nicht erteilt: keine Antwort der Nutzerin innerhalb von 10 Minuten (Zeitablauf). \
     Das Werkzeug wurde nicht ausgeführt; arbeite ohne es weiter oder melde den Bedarf im Ergebnis.";

/// Ablehnungsgrund, wenn die Frage die Oberfläche nicht erreicht.
pub const REASON_CHILD_APPROVAL_UNDELIVERABLE: &str = "Freigabe nicht erteilt: die Frage konnte der Nutzerin nicht gestellt werden. \
     Das Werkzeug wurde nicht ausgeführt.";

/// Ablehnungsgrund, wenn die Nutzerin ohne eigene Begründung ablehnt.
pub const REASON_CHILD_APPROVAL_REJECTED: &str =
    "Freigabe von der Nutzerin abgelehnt. Das Werkzeug wurde nicht ausgeführt.";

/// Ablehnungsgrund für ein Werkzeug außerhalb der Werkzeugfläche der Rolle.
pub const REASON_TOOL_OUTSIDE_ROLE: &str = "Freigabe nicht möglich: dieses Werkzeug gehört nicht zur Werkzeugfläche deiner Rolle. \
     Eine Freigabe kann keine Rechte erweitern.";

/// Eine Freigabe-Frage eines Kindes, wie die Oberfläche sie zeigt.
#[derive(Debug, Clone)]
pub struct ChildApprovalRequest {
    /// Die fragende Kind-Sitzung.
    pub child: SessionId,
    /// Registrierter Rollenname des Kindes (z. B. `uia-worker`).
    pub role: String,
    /// Rollen von der Wurzel bis zum Kind, z. B.
    /// `["root-orchestrator", "uia-worker"]` (die Wurzel selbst fehlt).
    pub tree_path: Vec<String>,
    /// Der festgehaltene Werkzeugaufruf.
    pub call: ToolCall,
    /// Wie lange das Kind auf die Antwort wartet.
    pub timeout: Duration,
}

impl ChildApprovalRequest {
    /// Die Absenderzeile des Dialogs: `<rolle> (<pfad im baum>)`.
    ///
    /// # Rückgabe
    /// Z. B. `uia-worker (Wurzel › root-orchestrator › uia-worker)`.
    #[must_use]
    pub fn requester_label(&self) -> String {
        let mut path = vec!["Wurzel".to_owned()];
        path.extend(self.tree_path.iter().cloned());
        format!("{} ({})", self.role, path.join(" › "))
    }
}

/// Die Antwort der Nutzerin auf eine [`ChildApprovalRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildApprovalAnswer {
    /// Einmalig freigeben.
    Approve,
    /// Ablehnen, optional mit getippter Begründung.
    Reject {
        /// Begründung der Nutzerin, falls vorhanden.
        reason: Option<String>,
    },
}

/// Eine Oberfläche, die Freigabe-Fragen von Kindern zustellen kann.
///
/// # Beschreibung
/// Nur die TUI implementiert das. `submit` darf nicht blockieren: es reiht
/// die Frage ein und gibt den Empfänger der Antwort zurück.
pub trait ChildApprovalBroker: Send + Sync {
    /// Stellt eine Frage zu.
    ///
    /// # Argumente
    /// - `request` ([`ChildApprovalRequest`]): die Frage samt Absender.
    ///
    /// # Rückgabe
    /// `Some(empfänger)`, wenn die Frage eingereiht wurde; `None`, wenn die
    /// Oberfläche nicht (mehr) erreichbar ist.
    fn submit(
        &self,
        request: ChildApprovalRequest,
    ) -> Option<oneshot::Receiver<ChildApprovalAnswer>>;
}

/// Der optionale Freigabe-Kanal eines Spawners.
///
/// # Beschreibung
/// Leer (Vorgabe) heißt: kein Kind kann fragen, das bisherige
/// fail-closed-Verhalten gilt. Die TUI bindet ihren Broker nach dem Bau der
/// Montage an ([`ManagedAgentSpawner::attach_child_approval_broker`]).
pub struct ChildApprovalRelay {
    broker: RwLock<Option<Arc<dyn ChildApprovalBroker>>>,
    timeout: RwLock<Duration>,
}

impl Default for ChildApprovalRelay {
    fn default() -> Self {
        Self {
            broker: RwLock::new(None),
            timeout: RwLock::new(CHILD_APPROVAL_TIMEOUT),
        }
    }
}

impl std::fmt::Debug for ChildApprovalRelay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildApprovalRelay")
            .field("available", &self.is_available())
            .field("timeout", &self.timeout())
            .finish()
    }
}

impl ChildApprovalRelay {
    /// Bindet `broker` an (ersetzt einen früheren).
    pub fn attach(&self, broker: Arc<dyn ChildApprovalBroker>) {
        *self.broker.write().unwrap_or_else(PoisonError::into_inner) = Some(broker);
    }

    /// Löst den Broker (z. B. beim Ende der TUI-Sitzung).
    pub fn detach(&self) {
        *self.broker.write().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// Ob ein Broker angebunden ist.
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.broker
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    /// Setzt die Wartezeit (Tests und künftige Konfiguration).
    pub fn set_timeout(&self, timeout: Duration) {
        *self.timeout.write().unwrap_or_else(PoisonError::into_inner) = timeout;
    }

    /// Die aktuelle Wartezeit.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        *self.timeout.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// Stellt die Frage und wartet mit Zeitlimit auf die Antwort.
    ///
    /// # Rückgabe
    /// Die [`ApprovalResolution`] für `resume_after_approval`. Nur eine
    /// ausdrückliche Zustimmung wird zu `Approve`; alles andere (Ablehnung,
    /// Zeitablauf, kein Broker, verworfener Kanal) ist `Reject`.
    pub async fn resolve(&self, request: ChildApprovalRequest) -> ApprovalResolution {
        let broker = self
            .broker
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let Some(broker) = broker else {
            return reject(REASON_CHILD_APPROVAL_UNDELIVERABLE);
        };
        let timeout = request.timeout;
        let child = request.child.clone();
        let tool = request.call.name.as_str().to_owned();
        let Some(answer) = broker.submit(request) else {
            tracing::warn!(child = %child, tool = %tool, "child_approval.undeliverable");
            return reject(REASON_CHILD_APPROVAL_UNDELIVERABLE);
        };
        match tokio::time::timeout(timeout, answer).await {
            Ok(Ok(ChildApprovalAnswer::Approve)) => {
                tracing::info!(target: "harw::audit", child = %child, tool = %tool, "child_approval.approved");
                ApprovalResolution::Approve
            }
            Ok(Ok(ChildApprovalAnswer::Reject { reason })) => {
                tracing::info!(target: "harw::audit", child = %child, tool = %tool, "child_approval.rejected");
                let reason = reason.filter(|text| !text.trim().is_empty()).map_or_else(
                    || REASON_CHILD_APPROVAL_REJECTED.to_owned(),
                    |text| format!("{REASON_CHILD_APPROVAL_REJECTED} Begründung: {text}"),
                );
                ApprovalResolution::Reject { reason }
            }
            Ok(Err(_closed)) => {
                tracing::warn!(child = %child, tool = %tool, "child_approval.answer_channel_closed");
                reject(REASON_CHILD_APPROVAL_UNDELIVERABLE)
            }
            Err(_elapsed) => {
                tracing::warn!(target: "harw::audit", child = %child, tool = %tool, "child_approval.timed_out");
                reject(REASON_CHILD_APPROVAL_TIMEOUT)
            }
        }
    }
}

fn reject(reason: &str) -> ApprovalResolution {
    ApprovalResolution::Reject {
        reason: reason.to_owned(),
    }
}

impl ManagedAgentSpawner {
    /// Bindet den Freigabe-Kanal der Oberfläche an (nur TUI).
    ///
    /// # Argumente
    /// - `broker` (`Arc<dyn ChildApprovalBroker>`): stellt Fragen zu.
    pub fn attach_child_approval_broker(&self, broker: Arc<dyn ChildApprovalBroker>) {
        self.child_approvals.attach(broker);
    }

    /// Löst den Freigabe-Kanal wieder.
    pub fn detach_child_approval_broker(&self) {
        self.child_approvals.detach();
    }

    /// Ob Kind-Freigaben an eine Oberfläche gehen können.
    #[must_use]
    pub fn child_approvals_relayable(&self) -> bool {
        self.child_approvals.is_available()
    }

    /// Der Freigabe-Kanal (Wartezeit setzen, Zustand lesen).
    #[must_use]
    pub fn child_approval_relay(&self) -> &Arc<ChildApprovalRelay> {
        &self.child_approvals
    }

    /// Rollen von der Wurzel bis `child` (Wurzel selbst ausgenommen).
    ///
    /// # Rückgabe
    /// Z. B. `["root-orchestrator", "uia-worker"]`; leer für unbekannte
    /// Sitzungen.
    #[must_use]
    pub fn child_tree_path(&self, child: &SessionId) -> Vec<String> {
        let mut roles = Vec::new();
        let mut cursor = child.clone();
        // Dieselbe Obergrenze wie der Lease-Herzschlag: Schutz gegen Zyklen.
        for _ in 0..64 {
            let Some(record) = self.child_record(&cursor) else {
                break;
            };
            roles.push(record.role.clone());
            cursor = record.parent;
        }
        roles.reverse();
        roles
    }
}

/// Beantwortet die Freigabepausen eines pausierverbotenen Kindes über den
/// Freigabe-Kanal, bis sein Turn terminal endet.
///
/// # Beschreibung
/// Siehe Moduldoku. Kehrt mit dem unveränderten `outcome` zurück, wenn kein
/// Ausgang keine Freigabepause ist, die Session keinen festgehaltenen Aufruf
/// trägt oder [`CHILD_APPROVAL_MAX_ROUNDS`]
/// erreicht ist — der Aufrufer weist eine verbleibende Pause dann wie bisher
/// fail-closed ab.
///
/// # Argumente
/// - `spawner`: liefert Kanal, Rolle und Baumpfad.
/// - `child`: die Kind-Sitzung (Rolle/Pfad).
/// - `session`: die laufende Kind-Session.
/// - `model`/`store`/`approvals`: dieselben wie beim Start des Kind-Turns.
/// - `outcome`: der Ausgang des Kind-Turns.
///
/// # Errors
/// Reicht Fehler von `resume_after_approval` durch.
pub async fn relay_child_approvals(
    spawner: &ManagedAgentSpawner,
    child: &SessionId,
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    approvals: Option<&ApprovalStore>,
    outcome: TurnOutcome,
) -> CoreResult<TurnOutcome> {
    let relay = Arc::clone(spawner.child_approval_relay());
    let mut outcome = outcome;
    for _ in 0..CHILD_APPROVAL_MAX_ROUNDS {
        if !matches!(outcome, TurnOutcome::AwaitingApproval { .. }) {
            return Ok(outcome);
        }
        let Some(pending) = session.pending_approval().cloned() else {
            return Ok(outcome);
        };
        let role = spawner
            .child_record(child)
            .map_or_else(|| "kind".to_owned(), |record| record.role);
        let resolution = if find_executor(session, &pending.call.name).is_none() {
            // Keine Rechte-Erweiterung: das Werkzeug gehört nicht zur
            // Werkzeugfläche des Kindes — nicht einmal fragen.
            tracing::warn!(
                target: "harw::audit",
                child = %child,
                role = %role,
                tool = %pending.call.name,
                "child_approval.tool_outside_role"
            );
            reject(REASON_TOOL_OUTSIDE_ROLE)
        } else if relay.is_available() {
            relay
                .resolve(ChildApprovalRequest {
                    child: child.clone(),
                    role,
                    tree_path: spawner.child_tree_path(child),
                    call: pending.call.clone(),
                    timeout: relay.timeout(),
                })
                .await
        } else {
            tracing::warn!(
                target: "harw::audit",
                child = %child,
                tool = %pending.call.name,
                "child_approval.unavailable_rejected"
            );
            reject(REASON_CHILD_APPROVAL_UNDELIVERABLE)
        };
        let actor = pending.actor.clone();
        outcome = match approvals {
            Some(approvals) => {
                resume_after_approval_durable(session, model, store, approvals, actor, resolution)
                    .await?
            }
            None => resume_after_approval(session, model, store, actor, resolution).await?,
        };
    }
    tracing::warn!(child = %child, "child_approval.round_limit_reached");
    Ok(outcome)
}
