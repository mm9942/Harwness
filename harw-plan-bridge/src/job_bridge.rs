//! Die Brücke zwischen fertigen Plan-Knoten und ausführbaren Jobs.
//!
//! # Verantwortungsbereich
//! [`PlanJobBridge`] übersetzt ausführbare Plan-Knoten in `StoredJob`-Einträge
//! des `harw-session-store`-Job-Stores und führt das Ergebnis als Evidenz an
//! den Plan zurück. Das ist der Punkt, an dem aus Planung Arbeit wird — und an
//! dem aus Arbeit wieder Planwissen wird.
//!
//! # Was der Job mitbekommt
//! Der Payload eines Plan-Job ist der vollständige Ausführungsvertrag:
//! `plan_id`, `task_id`, `plan_revision`, der [`MutationContract`] aus
//! `harw_plan::admission::contract_from_node`, das `objective`, die
//! Akzeptanzkriterien sowie Lese-, Schreib- und Verbotsbereich. Ein Worker
//! braucht damit keine zweite Quelle, um zu wissen, was er darf.
//!
//! Reservierte Job-Felder (`tenant`, `budget`, `retry`, `sandbox`,
//! `capabilities`, `credentials`, `work_id`) tauchen im Payload bewusst nicht
//! auf — sie gehören der Runtime, nicht dem Plan (vgl.
//! `harw_core::admission::sanitize_task`).
//!
//! # Wer entscheidet über Identität und Budget
//! [`JobAdmissionTemplate`] trägt Mandant, Workspace, Einreicher-Identität
//! ([`ApprovalActor`]), Budget und Retry-Politik. Diese Werte werden bewusst
//! **nicht** aus dem `actor`-String abgeleitet: aus einem beliebigen
//! Akteursnamen eine vertrauenswürdige Operator-Identität zu bauen wäre eine
//! Rechteausweitung. Die Composition-Root übergibt sie explizit.
//!
//! # Exportierte Typen
//! [`JobAdmissionTemplate`], [`PlanJobBridge`].
//!
//! # Concurrency
//! [`PlanJobBridge`] ist zustandslos (`Send + Sync`). Die Mutationen laufen
//! über `PlanStore::apply` und `JobStore::admit`, die beide selbst
//! thread-sicher sind; die Bridge bildet **keine** Transaktion über beide.
//!
//! # Fehler
//! [`PlanBridgeError::Plan`], [`PlanBridgeError::JobStore`],
//! [`PlanBridgeError::NodeNotFound`], [`PlanBridgeError::NoReadyNodes`],
//! [`PlanBridgeError::Json`].

use harw_job_runtime::{Budget, Job, JobKind, JobScope, RetryPolicy, StoredJob};
use harw_observe::TelemetrySink;
use harw_plan::actions::{NodePatch, PlanAction};
use harw_plan::admission::{MutationContract, RepoRevision, contract_from_node};
use harw_plan::graph;
use harw_plan::types::Assignment;
use harw_plan::{
    EvidenceKind, EvidenceRef, InvalidationCondition, PlanNode, PlanNodeKind, PlanNodeStatus,
    PlanStore, TaskId,
};
use harw_session_store::JobStore;
use harw_types::{ApprovalActor, WorkId};
use serde_json::json;
use time::OffsetDateTime;

use crate::error::PlanBridgeError;

/// `JobKind`-Diskriminator für Jobs, die aus einem Plan-Knoten entstehen.
const PLAN_NODE_JOB_KIND: &str = "plan-node";

/// Knotenarten, die als Job an einen Worker gehen.
const JOB_KINDS: &[PlanNodeKind] = &[
    PlanNodeKind::Coding,
    PlanNodeKind::Integration,
    PlanNodeKind::Verification,
    PlanNodeKind::Docs,
];

/// Alles, was die Runtime beisteuert, wenn ein Plan-Knoten zum Job wird.
///
/// # Description
/// Bündelt die Werte, die *nicht* aus dem Plan stammen dürfen: Mandanten- und
/// Workspace-Bindung samt Einreicher-Identität ([`JobScope`]), die Deckel
/// ([`Budget`]), die Wiederholungspolitik ([`RetryPolicy`]), die Repo-Revision,
/// gegen die der Patch gelten soll, und den injizierten Zeitpunkt.
///
/// # Concurrency
/// Reiner Werttyp; `Clone` erzeugt eine unabhängige Kopie.
#[derive(Debug, Clone)]
pub struct JobAdmissionTemplate {
    /// Mandant, Workspace und Einreicher-Identität des Jobs.
    pub scope: JobScope,
    /// Ressourcendeckel, unter denen der Job läuft.
    pub budget: Budget,
    /// Wiederholungspolitik des Jobs.
    pub retry: RetryPolicy,
    /// Repo-Revision, gegen die der Mutationsvertrag gilt.
    pub base_revision: RepoRevision,
    /// Injizierter Zeitpunkt der Admission (Job-Zeitachse, `jiff`).
    ///
    /// Der Plan braucht hier keinen eigenen Zeitstempel: `PlanStore::apply`
    /// setzt `applied_at` selbst — Zeitstempelhoheit liegt beim Store.
    pub now: jiff::Timestamp,
}

impl JobAdmissionTemplate {
    /// Baut ein Template aus seinen Bestandteilen.
    ///
    /// # Arguments
    /// - `scope` (`JobScope`): Mandant, Workspace, Einreicher.
    /// - `budget` (`Budget`): Ressourcendeckel.
    /// - `retry` (`RetryPolicy`): Wiederholungspolitik.
    /// - `base_revision` (`RepoRevision`): Basis des Mutationsvertrags.
    /// - `now` (`jiff::Timestamp`): Zeitpunkt auf der Job-Zeitachse.
    ///
    /// # Returns
    /// Das fertige [`JobAdmissionTemplate`].
    #[must_use]
    pub fn new(
        scope: JobScope,
        budget: Budget,
        retry: RetryPolicy,
        base_revision: RepoRevision,
        now: jiff::Timestamp,
    ) -> Self {
        Self {
            scope,
            budget,
            retry,
            base_revision,
            now,
        }
    }

    /// Gibt die Einreicher-Identität des Templates zurück.
    #[must_use]
    pub fn submitter(&self) -> &ApprovalActor {
        self.scope.submitter()
    }
}

/// Zustandsloser Namensraum für die Plan-zu-Job-Übersetzung.
///
/// # Concurrency
/// Enthält keinen Zustand; alle Methoden sind assoziierte Funktionen.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlanJobBridge;

impl PlanJobBridge {
    /// Admittiert alle ausführbaren Arbeitsknoten als Jobs.
    ///
    /// # Description
    /// Ermittelt über `harw_plan::graph::ready_nodes` die ausführbaren Knoten,
    /// filtert auf die Arbeitsarten [`JOB_KINDS`] und legt für jeden einen
    /// `Ready`-Job im Job-Store an. Der Payload ist der vollständige
    /// Ausführungsvertrag (siehe Modul-Dokumentation).
    ///
    /// Danach wandert der Knoten im Plan auf `InProgress` und trägt die
    /// `WorkId` in seinem [`Assignment`]; ein `Draft`-Knoten geht dabei über
    /// den regulären Zwischenschritt `Ready` — die Statusmatrix von `harw-plan`
    /// kennt keinen Sprung `Draft → InProgress`. Der `attempt`-Zähler eines
    /// bereits zugewiesenen Knotens wird erhöht.
    ///
    /// Der Job wird **vor** der Plan-Mutation angelegt: ein Job ohne
    /// zugewiesenen Knoten ist ein sichtbares Waisenkind, ein Knoten mit
    /// WorkId ohne Job wäre eine stille Lüge.
    ///
    /// # Arguments
    /// - `plan` (`&dyn PlanStore`): der Plan-Store der Session.
    /// - `jobs` (`&JobStore`): der Job-Store, in den admittiert wird.
    /// - `template` (`&JobAdmissionTemplate`): die von der Runtime gesetzten
    ///   Werte (Identität, Budget, Retry, Basis-Revision, Zeit).
    /// - `actor` (`&str`): der Akteur der Plan-Mutationen; wird als
    ///   `Assignment::worker` geführt. Er ist **keine** Vertrauensidentität —
    ///   die steckt in `template.scope.submitter()`.
    ///
    /// # Returns
    /// Die Paare `(TaskId, WorkId-String)` der admittierten Knoten, in
    /// `plan.nodes`-Reihenfolge.
    ///
    /// # Errors
    /// - [`PlanBridgeError::NoReadyNodes`]: wenn kein Arbeitsknoten ausführbar
    ///   ist. Bewusst ein Fehler: eine leere Welle zu starten ist fast immer
    ///   ein Denkfehler des Aufrufers.
    /// - [`PlanBridgeError::JobStore`]: wenn der Job-Store die Aufnahme ablehnt
    ///   oder der Job nicht auf `Ready` gesetzt werden kann.
    /// - [`PlanBridgeError::Plan`]: wenn eine Plan-Mutation abgelehnt wird
    ///   (z.B. fehlende Exploration, Regel 12).
    /// - [`PlanBridgeError::Json`]: wenn der Payload nicht serialisierbar ist.
    ///
    /// # Concurrency
    /// Sicher aus mehreren Threads, aber **nicht** atomar über Job-Store und
    /// Plan-Store hinweg: bricht eine Plan-Mutation ab, bleibt der bereits
    /// admittierte Job bestehen und muss vom Aufrufer storniert werden.
    pub fn admit_ready_nodes(
        plan: &dyn PlanStore,
        jobs: &JobStore,
        template: &JobAdmissionTemplate,
        actor: &str,
    ) -> Result<Vec<(TaskId, String)>, PlanBridgeError> {
        let snapshot = plan.current()?;
        let candidates: Vec<PlanNode> = graph::ready_nodes(&snapshot)
            .into_iter()
            .filter(|node| JOB_KINDS.contains(&node.kind))
            .cloned()
            .collect();

        if candidates.is_empty() {
            return Err(PlanBridgeError::NoReadyNodes);
        }

        let mut admitted: Vec<(TaskId, String)> = Vec::with_capacity(candidates.len());
        for node in &candidates {
            let plan_revision = plan.revision();
            let contract = contract_from_node(node, template.base_revision.clone(), plan_revision);
            let payload = node_payload(snapshot.id.as_str(), node, plan_revision, &contract)?;

            let work_id = WorkId::new();
            let mut job = Job::new(
                work_id.clone(),
                JobKind::Custom(PLAN_NODE_JOB_KIND.to_owned()),
                template.budget.clone(),
                template.retry.clone(),
                template.now,
            );
            job.mark_ready(template.now)
                .map_err(|error| PlanBridgeError::JobStore(error.to_string()))?;

            let record = StoredJob {
                job,
                scope: template.scope.clone(),
                input: payload,
                submitted_at: template.now,
                not_before: template.now,
                lease: None,
                lease_epoch: 0,
                completion: None,
                cancellation: None,
                revision: 0,
                // JobAdmissionTemplate (scope, budget, retry, base_revision,
                // now) carries no trace context, and no production caller of
                // `admit_ready_nodes` exists yet to supply one.
                trace: None,
            };
            jobs.admit(&record)
                .map_err(|error| PlanBridgeError::JobStore(error.to_string()))?;

            // Ab hier gehört der Knoten dem Job.
            if node.status == PlanNodeStatus::Draft {
                plan.apply(
                    PlanAction::SetStatus {
                        id: node.id.clone(),
                        status: PlanNodeStatus::Ready,
                        reason: Some("wird als Job admittiert".to_owned()),
                    },
                    actor,
                )?;
            }

            let attempt = node
                .assignment
                .as_ref()
                .map_or(0, |assignment| assignment.attempt.saturating_add(1));
            plan.apply(
                PlanAction::UpdateNode {
                    id: node.id.clone(),
                    patch: NodePatch {
                        assignment: Some(Some(Assignment {
                            worker: actor.to_owned(),
                            attempt,
                            job: Some(work_id.as_str().to_owned()),
                        })),
                        ..Default::default()
                    },
                },
                actor,
            )?;
            plan.apply(
                PlanAction::SetStatus {
                    id: node.id.clone(),
                    status: PlanNodeStatus::InProgress,
                    reason: Some(format!("Job '{}' admittiert", work_id.as_str())),
                },
                actor,
            )?;

            tracing::info!(
                task = %node.id,
                work_id = work_id.as_str(),
                attempt = attempt,
                "Plan-Knoten als Job admittiert"
            );
            admitted.push((node.id.clone(), work_id.as_str().to_owned()));
        }

        Ok(admitted)
    }

    /// Rückkanal: der Job ist fertig — Evidenz anhängen und Knoten abschließen.
    ///
    /// # Description
    /// Hängt einen [`EvidenceRef`] der Art [`EvidenceKind::Job`] mit der
    /// `WorkId` als Lokator an und setzt den Knoten auf `Completed`. Die
    /// Reihenfolge ist zwingend: `harw-plan` verweigert `Completed` ohne
    /// vorhandene Evidenz (Regel 11).
    ///
    /// # Arguments
    /// - `plan` (`&dyn PlanStore`): der Plan-Store der Session.
    /// - `task` (`&TaskId`): der abzuschließende Knoten.
    /// - `work_id` (`&str`): die `WorkId` des beendeten Jobs (wird Lokator).
    /// - `summary` (`&str`): menschenlesbare Zusammenfassung; wird als
    ///   `reason` des Statuswechsels geführt.
    /// - `actor` (`&str`): Akteur der Mutationen.
    /// - `now` (`OffsetDateTime`): injizierter Zeitpunkt für `attached_at`.
    ///
    /// # Returns
    /// `()` bei Erfolg.
    ///
    /// # Errors
    /// - [`PlanBridgeError::NodeNotFound`]: wenn `task` im aktuellen Plan
    ///   fehlt.
    /// - [`PlanBridgeError::Plan`]: wenn eine der beiden Mutationen abgelehnt
    ///   wird (z.B. weil der Knoten nicht `InProgress` war).
    ///
    /// # Concurrency
    /// Zwei getrennte `apply`-Aufrufe; nicht atomar. Schlägt der zweite fehl,
    /// bleibt die Evidenz angehängt — das ist der harmlosere Zwischenzustand.
    pub fn on_job_completed(
        plan: &dyn PlanStore,
        task: &TaskId,
        work_id: &str,
        summary: &str,
        actor: &str,
        now: OffsetDateTime,
    ) -> Result<(), PlanBridgeError> {
        ensure_node_exists(plan, task)?;

        plan.apply(
            PlanAction::AttachEvidence {
                id: task.clone(),
                evidence: EvidenceRef {
                    kind: EvidenceKind::Job,
                    locator: work_id.to_owned(),
                    attached_at: now,
                    actor: actor.to_owned(),
                    // Nur die WorkId (ein Lokator/ID) liegt hier vor; der
                    // Job-Inhalt selbst wird an dieser Stelle nicht gelesen.
                    digest: None,
                },
            },
            actor,
        )?;
        plan.apply(
            PlanAction::SetStatus {
                id: task.clone(),
                status: PlanNodeStatus::Completed,
                reason: Some(summary.to_owned()),
            },
            actor,
        )?;

        tracing::info!(
            task = %task,
            work_id = work_id,
            "Job abgeschlossen, Knoten completed"
        );
        Ok(())
    }

    /// Wie [`Self::on_job_completed`], zusätzlich mit Telemetrie (Knoten
    /// AW1-05).
    ///
    /// # Description
    /// Ruft [`Self::on_job_completed`] unverändert auf und erhöht bei Erfolg,
    /// falls `sink` `Some` ist, [`crate::metrics::EVIDENCE_ATTACHED_TOTAL`].
    /// Dieser Rückkanal hängt Evidenz direkt an, ohne über
    /// [`crate::controller::PlanController::apply`] zu laufen — er braucht
    /// deshalb eine eigene Emissionsstelle statt sich auf
    /// [`crate::controller::PlanController::apply_observed`] zu verlassen.
    ///
    /// # Arguments
    /// Identisch zu [`Self::on_job_completed`], zusätzlich:
    /// - `sink` (`Option<&dyn TelemetrySink>`): Ziel der Messwerte; `None`
    ///   unterdrückt jede Emission.
    ///
    /// # Returns
    /// `()` bei Erfolg.
    ///
    /// # Errors
    /// Wie [`Self::on_job_completed`].
    ///
    /// # Concurrency
    /// Wie [`Self::on_job_completed`].
    pub fn on_job_completed_observed(
        plan: &dyn PlanStore,
        task: &TaskId,
        work_id: &str,
        summary: &str,
        actor: &str,
        now: OffsetDateTime,
        sink: Option<&dyn TelemetrySink>,
    ) -> Result<(), PlanBridgeError> {
        Self::on_job_completed(plan, task, work_id, summary, actor, now)?;
        if let Some(sink) = sink {
            crate::metrics::record_evidence_attached(sink);
        }
        Ok(())
    }

    /// Rückkanal: der Job ist gescheitert — Knoten invalidieren.
    ///
    /// # Description
    /// Invalidiert den Knoten mit
    /// [`InvalidationCondition::ManualInvalidate`]. Ein `Reopen` folgt
    /// *nicht* automatisch: einen gescheiterten Schritt erneut zu versuchen ist
    /// eine bewusste Entscheidung mit Begründung, keine Reflexhandlung
    /// (`PlanAction::Reopen` verlangt ein `reason`).
    ///
    /// `reason` wird als Trace-Ereignis festgehalten, nicht im Plan:
    /// [`InvalidationCondition`] hat kein Freitextfeld, und den Grund in einen
    /// `locator` zu schreiben wäre Missbrauch dieses Feldes.
    ///
    /// # Arguments
    /// - `plan` (`&dyn PlanStore`): der Plan-Store der Session.
    /// - `task` (`&TaskId`): der zu invalidierende Knoten.
    /// - `work_id` (`&str`): die `WorkId` des gescheiterten Jobs (nur Trace).
    /// - `reason` (`&str`): der Fehlergrund (nur Trace).
    /// - `actor` (`&str`): Akteur der Mutation.
    ///
    /// # Returns
    /// `()` bei Erfolg.
    ///
    /// # Errors
    /// - [`PlanBridgeError::NodeNotFound`]: wenn `task` im aktuellen Plan
    ///   fehlt.
    /// - [`PlanBridgeError::Plan`]: wenn der Knoten nicht invalidierbar ist
    ///   (etwa weil er bereits `Completed` ist — dafür gibt es `Supersede`).
    ///
    /// # Concurrency
    /// Ein einzelner `apply`-Aufruf; thread-sicher.
    pub fn on_job_failed(
        plan: &dyn PlanStore,
        task: &TaskId,
        work_id: &str,
        reason: &str,
        actor: &str,
    ) -> Result<(), PlanBridgeError> {
        ensure_node_exists(plan, task)?;

        tracing::warn!(
            task = %task,
            work_id = work_id,
            reason = reason,
            "Job gescheitert — Knoten wird invalidiert"
        );
        plan.apply(
            PlanAction::Invalidate {
                ids: vec![task.clone()],
                condition: InvalidationCondition::ManualInvalidate,
            },
            actor,
        )?;
        Ok(())
    }

    /// Wie [`Self::on_job_failed`], zusätzlich mit Telemetrie (Knoten AW1-05).
    ///
    /// # Description
    /// Ruft [`Self::on_job_failed`] unverändert auf und erhöht bei Erfolg,
    /// falls `sink` `Some` ist, [`crate::metrics::INVALIDATIONS_TOTAL`].
    /// Dieser Rückkanal invalidiert den Knoten direkt, ohne über
    /// [`crate::controller::PlanController::apply`] zu laufen — er braucht
    /// deshalb eine eigene Emissionsstelle.
    ///
    /// # Arguments
    /// Identisch zu [`Self::on_job_failed`], zusätzlich:
    /// - `sink` (`Option<&dyn TelemetrySink>`): Ziel der Messwerte; `None`
    ///   unterdrückt jede Emission.
    ///
    /// # Returns
    /// `()` bei Erfolg.
    ///
    /// # Errors
    /// Wie [`Self::on_job_failed`].
    ///
    /// # Concurrency
    /// Wie [`Self::on_job_failed`].
    pub fn on_job_failed_observed(
        plan: &dyn PlanStore,
        task: &TaskId,
        work_id: &str,
        reason: &str,
        actor: &str,
        sink: Option<&dyn TelemetrySink>,
    ) -> Result<(), PlanBridgeError> {
        Self::on_job_failed(plan, task, work_id, reason, actor)?;
        if let Some(sink) = sink {
            crate::metrics::record_invalidation(sink);
        }
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Interne Hilfsfunktionen
// ──────────────────────────────────────────────────────────────────────────────

/// Stellt sicher, dass ein Knoten im aktuellen Plan existiert.
fn ensure_node_exists(plan: &dyn PlanStore, task: &TaskId) -> Result<(), PlanBridgeError> {
    let snapshot = plan.current()?;
    if snapshot.nodes.iter().any(|node| &node.id == task) {
        return Ok(());
    }
    Err(PlanBridgeError::NodeNotFound { task: task.clone() })
}

/// Baut den Job-Payload eines Plan-Knotens.
fn node_payload(
    plan_id: &str,
    node: &PlanNode,
    plan_revision: harw_plan::RevisionId,
    contract: &MutationContract,
) -> Result<serde_json::Value, PlanBridgeError> {
    let contract_value = serde_json::to_value(contract)?;
    let criteria_value = serde_json::to_value(&node.acceptance_criteria)?;
    let read_scope = scope_strings(&node.read_scope);
    let write_scope = scope_strings(&node.write_scope);
    let forbidden_scope = scope_strings(&node.forbidden_scope);

    Ok(json!({
        "plan_id": plan_id,
        "task_id": node.id.as_str(),
        "plan_revision": plan_revision.value(),
        "contract": contract_value,
        "objective": node.objective,
        "acceptance_criteria": criteria_value,
        "read_scope": read_scope,
        "write_scope": write_scope,
        "forbidden_scope": forbidden_scope,
    }))
}

/// Wandelt eine Scope-Liste in ihre Zeichenketten-Darstellung.
fn scope_strings(scopes: &[harw_plan::PathOrSymbol]) -> Vec<&str> {
    scopes.iter().map(|scope| scope.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{EVIDENCE_ATTACHED_TOTAL, INVALIDATIONS_TOTAL};
    use crate::testing::{RecordingSink, coding_node, research_node, seeded_plan_store, timestamp};
    use harw_plan::{InMemoryPlanStore, PlanId};
    use harw_types::{TenantId, WorkspaceId};
    use jiff::SignedDuration;

    fn template() -> JobAdmissionTemplate {
        let retry = match RetryPolicy::try_new(1, SignedDuration::ZERO, 2.0, SignedDuration::ZERO) {
            Ok(retry) => retry,
            Err(error) => panic!("RetryPolicy: {error}"),
        };
        JobAdmissionTemplate::new(
            JobScope::new(
                TenantId::from_str("tenant-test"),
                WorkspaceId::from_str("workspace"),
                ApprovalActor::Operator {
                    id: "operator-1".to_owned(),
                },
            ),
            Budget::unbounded(),
            retry,
            RepoRevision("abc123".to_owned()),
            timestamp(),
        )
    }

    fn job_store() -> (JobStore, tempfile::TempDir) {
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => panic!("Temp-Verzeichnis: {error}"),
        };
        (JobStore::new(dir.path()), dir)
    }

    #[test]
    fn test_admit_ready_nodes_creates_a_job_and_moves_the_node_in_progress() {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)]);
        let (jobs, _dir) = job_store();

        let admitted = match PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template(), "runtime")
        {
            Ok(admitted) => admitted,
            Err(error) => panic!("admit schlug fehl: {error}"),
        };

        assert_eq!(admitted.len(), 1);
        let (task, work_id) = &admitted[0];
        assert_eq!(task, &TaskId::new("t-1"));
        assert!(!work_id.is_empty());

        let snapshot = match plan.current() {
            Ok(snapshot) => snapshot,
            Err(error) => panic!("current: {error}"),
        };
        let node = &snapshot.nodes[0];
        assert_eq!(node.status, PlanNodeStatus::InProgress);
        match &node.assignment {
            Some(assignment) => {
                assert_eq!(assignment.job.as_deref(), Some(work_id.as_str()));
                assert_eq!(assignment.worker, "runtime");
                assert_eq!(assignment.attempt, 0);
            }
            None => panic!("Assignment fehlt"),
        }
    }

    #[test]
    fn test_admit_ready_nodes_payload_carries_the_full_contract() {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)]);
        let (jobs, _dir) = job_store();

        let admitted = match PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template(), "runtime")
        {
            Ok(admitted) => admitted,
            Err(error) => panic!("admit schlug fehl: {error}"),
        };
        let work_id = WorkId::from_str(admitted[0].1.as_str());
        let stored = match jobs.get(&work_id) {
            Ok(stored) => stored,
            Err(error) => panic!("Job lesen: {error}"),
        };

        assert_eq!(
            stored.job.kind,
            JobKind::Custom(PLAN_NODE_JOB_KIND.to_owned())
        );
        let input = &stored.input;
        assert_eq!(input["plan_id"].as_str(), Some("p-test"));
        assert_eq!(input["task_id"].as_str(), Some("t-1"));
        assert_eq!(input["plan_revision"].as_u64(), Some(2));
        assert!(input["contract"].is_object());
        assert!(input["objective"].is_string());
        assert!(input["write_scope"].is_array());
        // Reservierte Runtime-Felder gehören nicht in den Plan-Payload.
        for reserved in [
            "tenant",
            "budget",
            "retry",
            "sandbox",
            "credentials",
            "work_id",
        ] {
            assert!(
                input.get(reserved).is_none(),
                "reserviertes Feld '{reserved}' im Payload"
            );
        }
    }

    #[test]
    fn test_admit_ready_nodes_promotes_a_draft_node_through_ready() {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Draft)]);
        let (jobs, _dir) = job_store();

        if let Err(error) = PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template(), "runtime") {
            panic!("admit schlug fehl: {error}");
        }

        let snapshot = match plan.current() {
            Ok(snapshot) => snapshot,
            Err(error) => panic!("current: {error}"),
        };
        assert_eq!(snapshot.nodes[0].status, PlanNodeStatus::InProgress);
    }

    #[test]
    fn test_admit_ready_nodes_skips_non_work_kinds() {
        let plan = seeded_plan_store(vec![research_node("r-1", PlanNodeStatus::Ready)]);
        let (jobs, _dir) = job_store();

        match PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template(), "runtime") {
            Err(PlanBridgeError::NoReadyNodes) => {}
            other => panic!("erwartet NoReadyNodes, bekommen: {other:?}"),
        }
    }

    #[test]
    fn test_admit_ready_nodes_reports_no_ready_nodes_on_an_empty_plan() {
        let plan = InMemoryPlanStore::new();
        if let Err(error) = plan.apply(
            PlanAction::Create {
                plan_id: PlanId::new("p-empty"),
                goal: "Ziel".to_owned(),
            },
            "test",
        ) {
            panic!("Plan anlegen: {error}");
        }
        let (jobs, _dir) = job_store();

        match PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template(), "runtime") {
            Err(PlanBridgeError::NoReadyNodes) => {}
            other => panic!("erwartet NoReadyNodes, bekommen: {other:?}"),
        }
    }

    #[test]
    fn test_on_job_completed_attaches_evidence_and_completes() {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)]);
        let (jobs, _dir) = job_store();
        let admitted = match PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template(), "runtime")
        {
            Ok(admitted) => admitted,
            Err(error) => panic!("admit schlug fehl: {error}"),
        };
        let (task, work_id) = &admitted[0];

        if let Err(error) = PlanJobBridge::on_job_completed(
            &plan,
            task,
            work_id,
            "cargo test grün",
            "runtime",
            OffsetDateTime::UNIX_EPOCH,
        ) {
            panic!("on_job_completed schlug fehl: {error}");
        }

        let snapshot = match plan.current() {
            Ok(snapshot) => snapshot,
            Err(error) => panic!("current: {error}"),
        };
        let node = &snapshot.nodes[0];
        assert_eq!(node.status, PlanNodeStatus::Completed);
        assert_eq!(node.evidence.len(), 1);
        assert_eq!(node.evidence[0].kind, EvidenceKind::Job);
        assert_eq!(&node.evidence[0].locator, work_id);
    }

    #[test]
    fn test_on_job_completed_rejects_an_unknown_task() {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)]);

        match PlanJobBridge::on_job_completed(
            &plan,
            &TaskId::new("t-unbekannt"),
            "work-1",
            "fertig",
            "runtime",
            OffsetDateTime::UNIX_EPOCH,
        ) {
            Err(PlanBridgeError::NodeNotFound { task }) => {
                assert_eq!(task, TaskId::new("t-unbekannt"));
            }
            other => panic!("erwartet NodeNotFound, bekommen: {other:?}"),
        }
    }

    #[test]
    fn test_on_job_failed_invalidates_the_node_without_reopening_it() {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)]);
        let (jobs, _dir) = job_store();
        let admitted = match PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template(), "runtime")
        {
            Ok(admitted) => admitted,
            Err(error) => panic!("admit schlug fehl: {error}"),
        };
        let (task, work_id) = &admitted[0];

        if let Err(error) =
            PlanJobBridge::on_job_failed(&plan, task, work_id, "clippy rot", "runtime")
        {
            panic!("on_job_failed schlug fehl: {error}");
        }

        let snapshot = match plan.current() {
            Ok(snapshot) => snapshot,
            Err(error) => panic!("current: {error}"),
        };
        assert_eq!(snapshot.nodes[0].status, PlanNodeStatus::Invalidated);
    }

    #[test]
    fn test_on_job_failed_rejects_an_unknown_task() {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)]);

        match PlanJobBridge::on_job_failed(
            &plan,
            &TaskId::new("t-unbekannt"),
            "work-1",
            "kaputt",
            "runtime",
        ) {
            Err(PlanBridgeError::NodeNotFound { .. }) => {}
            other => panic!("erwartet NodeNotFound, bekommen: {other:?}"),
        }
    }

    #[test]
    fn test_on_job_completed_observed_emits_evidence_attached_total() {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)]);
        let (jobs, _dir) = job_store();
        let admitted = match PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template(), "runtime")
        {
            Ok(admitted) => admitted,
            Err(error) => panic!("admit schlug fehl: {error}"),
        };
        let (task, work_id) = &admitted[0];
        let sink = RecordingSink::new();

        if let Err(error) = PlanJobBridge::on_job_completed_observed(
            &plan,
            task,
            work_id,
            "cargo test grün",
            "runtime",
            OffsetDateTime::UNIX_EPOCH,
            Some(&sink),
        ) {
            panic!("on_job_completed_observed schlug fehl: {error}");
        }

        assert_eq!(sink.values_for(EVIDENCE_ATTACHED_TOTAL.name).len(), 1);
        assert!(sink.values_for(INVALIDATIONS_TOTAL.name).is_empty());
    }

    #[test]
    fn test_on_job_failed_observed_emits_invalidations_total() {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)]);
        let (jobs, _dir) = job_store();
        let admitted = match PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template(), "runtime")
        {
            Ok(admitted) => admitted,
            Err(error) => panic!("admit schlug fehl: {error}"),
        };
        let (task, work_id) = &admitted[0];
        let sink = RecordingSink::new();

        if let Err(error) = PlanJobBridge::on_job_failed_observed(
            &plan,
            task,
            work_id,
            "clippy rot",
            "runtime",
            Some(&sink),
        ) {
            panic!("on_job_failed_observed schlug fehl: {error}");
        }

        assert_eq!(sink.values_for(INVALIDATIONS_TOTAL.name).len(), 1);
        assert!(sink.values_for(EVIDENCE_ATTACHED_TOTAL.name).is_empty());
    }

    #[test]
    fn test_on_job_failed_observed_without_a_sink_still_invalidates() {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)]);
        let (jobs, _dir) = job_store();
        let admitted = match PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template(), "runtime")
        {
            Ok(admitted) => admitted,
            Err(error) => panic!("admit schlug fehl: {error}"),
        };
        let (task, work_id) = &admitted[0];

        if let Err(error) =
            PlanJobBridge::on_job_failed_observed(&plan, task, work_id, "clippy rot", "runtime", None)
        {
            panic!("on_job_failed_observed schlug fehl: {error}");
        }

        let snapshot = match plan.current() {
            Ok(snapshot) => snapshot,
            Err(error) => panic!("current: {error}"),
        };
        assert_eq!(snapshot.nodes[0].status, PlanNodeStatus::Invalidated);
    }

    #[test]
    fn test_template_exposes_its_submitter() {
        let template = template();
        assert!(matches!(
            template.submitter(),
            ApprovalActor::Operator { id } if id == "operator-1"
        ));
    }
}
