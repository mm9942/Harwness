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
//! `plan_id`, `task_id`, `plan_revision`, `assignment_attempt`, `dispatch_owner`, der [`MutationContract`] aus
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
//! [`PlanJobBridge`] ist zustandslos (`Send + Sync`). Plan-Mutationen laufen
//! als atomare `PlanStore::apply_batch`-Aufrufe mit Revisionsprüfung, Jobs
//! über `JobStore::admit`; beide sind selbst thread-sicher. Über beide Stores
//! bildet die Bridge **keine** Transaktion — die Admission ist stattdessen
//! idempotent (deterministische `WorkId` je Knoten und Plan-Revision).
//!
//! # Fehler
//! [`PlanBridgeError::Plan`], [`PlanBridgeError::JobStore`],
//! [`PlanBridgeError::NodeNotFound`], [`PlanBridgeError::NoReadyNodes`],
//! [`PlanBridgeError::Json`].

use harw_job_runtime::{Budget, Job, JobKind, JobScope, JobState, RetryPolicy, StoredJob};
use harw_observe::TelemetrySink;
use harw_plan::actions::{NodePatch, PlanAction};
use harw_plan::admission::{MutationContract, RepoRevision, contract_from_node};
use harw_plan::graph;
use harw_plan::types::Assignment;
use harw_plan::{
    EvidenceKind, EvidenceRef, InvalidationCondition, Plan, PlanId, PlanNode, PlanNodeKind,
    PlanNodeStatus, PlanStore, RevisionId, TaskId,
};
use harw_session_store::{JobStore, SessionStoreError};
use harw_types::{ApprovalActor, ContentDigest, WorkId};
use serde_json::json;
use time::OffsetDateTime;

use crate::controller::{apply_atomically, apply_atomically_fenced, find_node};
use crate::error::PlanBridgeError;

/// `JobKind`-Diskriminator für Jobs, die aus einem Plan-Knoten entstehen.
const PLAN_NODE_JOB_KIND: &str = "plan-node";

/// Persistierter Routing-Eigentümer eines Plan-Jobs, keine Autoritätsquelle.
///
/// Parent-ID und Epoche sind nur eine Bindung für einen künftigen vertrauenswürdigen
/// Admission-Pfad. Sie ersetzen weder einen lebenden Parent-Claim noch dessen
/// Budget-Reservierung. Prerequisite-Jobs dürfen aktuell nirgendwo ausgeführt werden.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanDispatchOwner {
    /// Bestehender PlanJobBridge-Pfad, einschließlich alter Payloads ohne Owner.
    LegacyPlanJobs {},
    /// Reserviertes Routing für WorkDriver-Prerequisites; derzeit fail-closed.
    WorkDriverPrerequisite {
        /// Durable Identität des übergeordneten WorkDriver-Jobs (keine Berechtigung).
        parent_work_id: WorkId,
        /// Claim-Epoche des Parents (keine Lease oder Budget-Autorität).
        parent_lease_epoch: std::num::NonZeroU64,
    },
}

impl PlanDispatchOwner {
    /// Liest nur den Routing-Eigentümer; alle übrigen Payload-Verträge bleiben Pflicht.
    ///
    /// # Errors
    /// Nur ein fehlendes Feld erhält den Legacy-Default. Null, unbekannte Varianten,
    /// fremde Felder und ungültige Parent-Bindungen werden abgelehnt.
    pub fn from_job_input(input: &serde_json::Value) -> Result<Self, String> {
        let object = input
            .as_object()
            .ok_or("plan-node input must be an object")?;
        match object.get("dispatch_owner") {
            None => Ok(Self::LegacyPlanJobs {}),
            Some(value) => serde_json::from_value(value.clone())
                .map_err(|_| "invalid plan-node dispatch_owner".to_owned()),
        }
    }

    /// Verhindert, dass Prerequisites in die Legacy-Ausführung oder deren Coding-Folgejobs gelangen.
    ///
    /// # Errors
    /// WorkDriver-Prerequisites bleiben blockiert, bis Parent-Claim und Budget
    /// von einem eigenen vertrauenswürdigen Admission-Pfad geprüft werden.
    pub fn require_legacy_execution(&self) -> Result<(), String> {
        match self {
            Self::LegacyPlanJobs {} => Ok(()),
            Self::WorkDriverPrerequisite { .. } => Err(
                "WorkDriver prerequisite dispatch is disabled until trusted parent-claim and budget admission is implemented; legacy execution and follow-up are forbidden".to_owned(),
            ),
        }
    }
}

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
    /// Veröffentlicht nach einem Neustart noch ausstehende, bereits gebundene Jobs.
    ///
    /// # Errors
    /// Propagiert Plan-/JobStore-Fehler. Nur `Pending` mit unveränderter
    /// Plan-/Task-/Attempt-Bindung wird `Ready`; fremde/stale Jobs bleiben liegen.
    /// Dies ist idempotente Recovery, keine Transaktion über beide Stores.
    pub fn recover_pending_admissions(
        plan: &dyn PlanStore,
        jobs: &JobStore,
        now: jiff::Timestamp,
    ) -> Result<Vec<(TaskId, String)>, PlanBridgeError> {
        let snapshot = plan.current()?;
        let mut published = Vec::new();
        for node in &snapshot.nodes {
            if node.status != PlanNodeStatus::InProgress || !JOB_KINDS.contains(&node.kind) {
                continue;
            }
            let Some(assignment) = node.assignment.as_ref() else {
                continue;
            };
            let Some(id) = assignment.job.as_deref() else {
                continue;
            };
            let work_id = WorkId::try_from_str(id)
                .map_err(|error| PlanBridgeError::JobStore(error.to_string()))?;
            let record = match jobs.get(&work_id) {
                Ok(record) => record,
                Err(SessionStoreError::JobNotFound { .. }) => continue,
                Err(error) => return Err(PlanBridgeError::JobStore(error.to_string())),
            };
            if record.job.state != JobState::Pending
                || record.job.kind != JobKind::Custom(PLAN_NODE_JOB_KIND.to_owned())
                || record
                    .input
                    .get("plan_id")
                    .and_then(serde_json::Value::as_str)
                    != Some(snapshot.id.as_str())
                || record
                    .input
                    .get("task_id")
                    .and_then(serde_json::Value::as_str)
                    != Some(node.id.as_str())
                || record
                    .input
                    .get("assignment_attempt")
                    .and_then(serde_json::Value::as_u64)
                    != Some(u64::from(assignment.attempt))
            {
                continue;
            }
            let Some(revision) = record
                .input
                .get("plan_revision")
                .and_then(serde_json::Value::as_u64)
            else {
                continue;
            };
            let Some(raw_contract) = record.input.get("contract") else {
                continue;
            };
            let Ok(contract) = serde_json::from_value::<MutationContract>(raw_contract.clone())
            else {
                continue;
            };
            let expected_contract =
                contract_from_node(node, contract.base_revision, RevisionId::new(revision));
            let expected_payload = node_payload(
                snapshot.id.as_str(),
                node,
                RevisionId::new(revision),
                assignment.attempt,
                &expected_contract,
            )?;
            if !legacy_payload_matches(&record.input, &expected_payload) {
                continue;
            }
            if Self::validate_execution_binding(
                plan,
                snapshot.id.as_str(),
                &node.id,
                id,
                assignment.attempt,
            )
            .is_err()
            {
                continue;
            }
            publish_bound_job(
                plan,
                jobs,
                snapshot.id.as_str(),
                &node.id,
                &work_id,
                assignment.attempt,
                now,
            )?;
            published.push((node.id.clone(), id.to_owned()));
        }
        Ok(published)
    }

    /// Prüft die Admission-Bindung vor Ausführung; keine Lease-/Claim-Garantie.
    ///
    /// # Errors
    /// Lehnt fremde Pläne, fehlende Zuweisungen, alte Attempts und nicht
    /// laufende Knoten ab. Der Aufrufer darf Attempts nicht aus dem Plan raten.
    pub fn validate_execution_binding(
        plan: &dyn PlanStore,
        plan_id: &str,
        task: &TaskId,
        work_id: &str,
        assignment_attempt: u32,
    ) -> Result<(), PlanBridgeError> {
        let snapshot = plan.current()?;
        if snapshot.id.as_str() != plan_id {
            return Err(callback_rejected(task, work_id, "current plan differs"));
        }
        let node = assigned_callback_node(&snapshot, task, work_id, assignment_attempt)?;
        if node.status != PlanNodeStatus::InProgress {
            return Err(callback_rejected(task, work_id, "node is not in progress"));
        }
        Ok(())
    }

    /// Admittiert alle ausführbaren Arbeitsknoten als Jobs.
    ///
    /// # Description
    /// Ermittelt über `harw_plan::graph::ready_nodes` die ausführbaren Knoten,
    /// filtert auf die Arbeitsarten [`JOB_KINDS`] und legt für jeden einen
    /// `Ready`-Job im Job-Store an. Der Payload ist der vollständige
    /// Ausführungsvertrag (siehe Modul-Dokumentation).
    ///
    /// Je Knoten in drei Schritten (G-038, K4/K5):
    /// 1. Ein `Draft`-Knoten wird **vor** dem Anlegen eines Jobs atomar auf
    ///    `Ready` gesetzt. Damit prüft `harw-plan` Exploration (Regel 12),
    ///    Abhängigkeiten und Schreibbereich, bevor ein Job existiert — ein
    ///    abgelehnter Knoten hinterlässt keinen Waisen-Job.
    /// 2. Der Job wird mit einer **deterministischen** `WorkId` aus
    ///    `(Plan, Knoten, Plan-Revision)` angelegt. Existiert dieser Job schon
    ///    (ein früherer Lauf scheiterte nach dem Anlegen), wird er
    ///    wiederverwendet, sofern er noch `Pending` ist und sein Payload exakt
    ///    übereinstimmt. Pro Knoten und Revision entsteht so höchstens ein
    ///    Job.
    /// 3. `UpdateNode(assignment)` und `SetStatus(InProgress)` laufen als
    ///    **ein** `apply_batch`; eine geänderte Quellrevision wird nicht gebunden.
    /// 4. Erst danach wird der `Pending`-Job als `Ready` veröffentlicht.
    ///
    /// Der Job wird vor der Bindung im Plan angelegt: ein Job ohne
    /// zugewiesenen Knoten ist ein sichtbares (und beim nächsten Lauf
    /// wiederverwendetes) Waisenkind, ein Knoten mit WorkId ohne Job wäre
    /// eine stille Lüge. Der `attempt`-Zähler eines bereits zugewiesenen
    /// Knotens wird erhöht.
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
    /// Plan-Store hinweg: scheitert Schritt 3, bleibt der Job `Pending` im
    /// Job-Store und der Knoten `Ready`. Ein erneuter Aufruf auf derselben
    /// Plan-Revision bindet denselben Job; hat sich die Revision inzwischen
    /// geändert, bleibt der alte Job ein Waise, den der Aufrufer stornieren
    /// muss. Ein Absturz zwischen Bindung und Veröffentlichung wird durch
    /// `recover_pending_admissions` im Worker-Poller repariert.
    pub fn admit_ready_nodes(
        plan: &dyn PlanStore,
        jobs: &JobStore,
        template: &JobAdmissionTemplate,
        actor: &str,
    ) -> Result<Vec<(TaskId, String)>, PlanBridgeError> {
        let mut admitted = Self::recover_pending_admissions(plan, jobs, template.now)?;
        let snapshot = plan.current()?;
        let candidates: Vec<TaskId> = graph::ready_nodes(&snapshot)
            .into_iter()
            .filter(|node| JOB_KINDS.contains(&node.kind))
            .map(|node| node.id.clone())
            .collect();

        if candidates.is_empty() && admitted.is_empty() {
            return Err(PlanBridgeError::NoReadyNodes);
        }
        for task in &candidates {
            if let Some(work_id) = admit_node(plan, jobs, template, actor, task)? {
                admitted.push((task.clone(), work_id));
            }
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
    /// - `assignment_attempt` (`u32`): unveränderter Attempt aus dem Admission-Payload.
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
    /// [`PlanBridgeError::JobCallbackRejected`] lehnt fehlende/fremde
    /// Zuweisungen und unpassende Status ab. `Completed` ist nur dann ein
    /// erfolgreicher No-op, wenn die aktuelle Zuweisung und eine gespeicherte
    /// Job-Evidenz beide dieselbe `work_id` tragen. Ältere Abschlüsse ohne
    /// diesen Nachweis werden fail-closed abgelehnt.
    ///
    /// # Concurrency
    /// Beide Aktionen laufen als **ein** `apply_batch` (G-038, K1): lehnt
    /// `harw-plan` den Abschluss ab, wird auch die Evidenz nicht angehängt.
    /// Bei Revisionskonflikt werden Plan und Zuweisung erneut geprüft.
    /// Dies ist keine Job-Lease-/Claim-Prüfung und keine Transaktion über
    /// JobStore und PlanStore. Der Assignment-Attempt stammt aus der
    /// Admission, nicht aus einem aktuellen Lookup oder dem JobClaim-Retry.
    /// Durable Worker müssen [`Self::on_job_completed_claimed`] verwenden.
    pub fn on_job_completed(
        plan: &dyn PlanStore,
        task: &TaskId,
        work_id: &str,
        assignment_attempt: u32,
        summary: &str,
        actor: &str,
        now: OffsetDateTime,
    ) -> Result<(), PlanBridgeError> {
        Self::on_job_completed_checked(
            plan,
            task,
            work_id,
            assignment_attempt,
            summary,
            actor,
            now,
            None,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "callback fields plus trusted publication check"
    )]
    fn on_job_completed_checked(
        plan: &dyn PlanStore,
        task: &TaskId,
        work_id: &str,
        assignment_attempt: u32,
        summary: &str,
        actor: &str,
        now: OffsetDateTime,
        before_publish: Option<&dyn Fn() -> harw_plan::PlanResult<()>>,
    ) -> Result<(), PlanBridgeError> {
        let events = apply_atomically_fenced(
            plan,
            actor,
            |snapshot| {
                let node = assigned_callback_node(snapshot, task, work_id, assignment_attempt)?;
                if node.status == PlanNodeStatus::Completed {
                    if node
                        .evidence
                        .iter()
                        .any(|e| e.kind == EvidenceKind::Job && e.locator == work_id)
                    {
                        return Ok(Vec::new());
                    }
                    return Err(callback_rejected(
                        task,
                        work_id,
                        "completion evidence missing",
                    ));
                }
                if node.status != PlanNodeStatus::InProgress {
                    return Err(callback_rejected(task, work_id, "node is not in progress"));
                }
                Ok(vec![
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
                    PlanAction::SetStatus {
                        id: task.clone(),
                        status: PlanNodeStatus::Completed,
                        reason: Some(summary.to_owned()),
                    },
                ])
            },
            before_publish,
        )?;
        if events.is_empty() {
            tracing::debug!(task = %task, work_id = work_id, "Knoten war bereits completed");
            return Ok(());
        }

        tracing::info!(
            task = %task,
            work_id = work_id,
            "Job abgeschlossen, Knoten completed"
        );
        Ok(())
    }

    /// Completes a node only while the exact durable job claim is live.
    ///
    /// Lock order is job record -> plan write lock. No admission, other job
    /// mutation, or await occurs under this guard. History publication and
    /// the later job terminal transition are NOT a cross-store transaction.
    ///
    /// # Errors
    /// Rejects stale/expired claims, assignment mismatches and store errors.
    #[allow(
        clippy::too_many_arguments,
        reason = "explicit callback identity and lease authority"
    )]
    pub fn on_job_completed_claimed(
        plan: &dyn PlanStore,
        jobs: &JobStore,
        token: &harw_job_runtime::LeaseToken,
        task: &TaskId,
        assignment_attempt: u32,
        summary: &str,
        actor: &str,
        now: OffsetDateTime,
    ) -> Result<(), PlanBridgeError> {
        let held = jobs
            .hold_live_lease(token)
            .map_err(|error| PlanBridgeError::JobStore(error.to_string()))?;
        let check = || {
            held.still_live()
                .map_err(|error| harw_plan::PlanError::Io(std::io::Error::other(error.to_string())))
        };
        Self::on_job_completed_checked(
            plan,
            task,
            token.work_id.as_str(),
            assignment_attempt,
            summary,
            actor,
            now,
            Some(&check),
        )
    }

    /// Invalidates a node under the same live-claim fence as completion.
    ///
    /// # Errors
    /// Rejects stale/expired claims, assignment mismatches and store errors.
    pub fn on_job_failed_claimed(
        plan: &dyn PlanStore,
        jobs: &JobStore,
        token: &harw_job_runtime::LeaseToken,
        task: &TaskId,
        assignment_attempt: u32,
        reason: &str,
        actor: &str,
    ) -> Result<(), PlanBridgeError> {
        let held = jobs
            .hold_live_lease(token)
            .map_err(|error| PlanBridgeError::JobStore(error.to_string()))?;
        let check = || {
            held.still_live()
                .map_err(|error| harw_plan::PlanError::Io(std::io::Error::other(error.to_string())))
        };
        Self::on_job_failed_checked(
            plan,
            task,
            token.work_id.as_str(),
            assignment_attempt,
            reason,
            actor,
            Some(&check),
        )
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
    #[allow(
        clippy::too_many_arguments,
        reason = "observed callback carries immutable assignment attempt and telemetry"
    )]
    pub fn on_job_completed_observed(
        plan: &dyn PlanStore,
        task: &TaskId,
        work_id: &str,
        assignment_attempt: u32,
        summary: &str,
        actor: &str,
        now: OffsetDateTime,
        sink: Option<&dyn TelemetrySink>,
    ) -> Result<(), PlanBridgeError> {
        Self::on_job_completed(plan, task, work_id, assignment_attempt, summary, actor, now)?;
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
    /// - `work_id` (`&str`): muss der aktuellen Job-Zuweisung entsprechen.
    /// - `assignment_attempt` (`u32`): unveränderter Attempt aus dem Admission-Payload.
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
    /// [`PlanBridgeError::JobCallbackRejected`] lehnt fehlende/fremde
    /// Zuweisungen und Status außerhalb `InProgress`/`Invalidated` ab.
    /// `Invalidated` ist nur bei passender aktueller Zuweisung ein No-op;
    /// daraus wird kein Nachweis über die Ursache der Invalidierung abgeleitet.
    ///
    /// # Concurrency
    /// Ein einzelner atomarer Batch; Zuweisung und Status werden bei jedem
    /// CAS-Retry erneut geprüft. Keine Job-Lease-/Claim-Prüfung.
    /// Durable Worker müssen [`Self::on_job_failed_claimed`] verwenden.
    pub fn on_job_failed(
        plan: &dyn PlanStore,
        task: &TaskId,
        work_id: &str,
        assignment_attempt: u32,
        reason: &str,
        actor: &str,
    ) -> Result<(), PlanBridgeError> {
        Self::on_job_failed_checked(plan, task, work_id, assignment_attempt, reason, actor, None)
    }

    fn on_job_failed_checked(
        plan: &dyn PlanStore,
        task: &TaskId,
        work_id: &str,
        assignment_attempt: u32,
        reason: &str,
        actor: &str,
        before_publish: Option<&dyn Fn() -> harw_plan::PlanResult<()>>,
    ) -> Result<(), PlanBridgeError> {
        tracing::warn!(
            task = %task,
            work_id = work_id,
            reason = reason,
            "Job gescheitert — Knoten wird invalidiert"
        );
        apply_atomically_fenced(
            plan,
            actor,
            |snapshot| {
                let node = assigned_callback_node(snapshot, task, work_id, assignment_attempt)?;
                if node.status == PlanNodeStatus::Invalidated {
                    return Ok(Vec::new());
                }
                if node.status != PlanNodeStatus::InProgress {
                    return Err(callback_rejected(task, work_id, "node is not in progress"));
                }
                Ok(vec![PlanAction::Invalidate {
                    ids: vec![task.clone()],
                    condition: InvalidationCondition::ManualInvalidate,
                }])
            },
            before_publish,
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
        assignment_attempt: u32,
        reason: &str,
        actor: &str,
        sink: Option<&dyn TelemetrySink>,
    ) -> Result<(), PlanBridgeError> {
        Self::on_job_failed(plan, task, work_id, assignment_attempt, reason, actor)?;
        if let Some(sink) = sink {
            crate::metrics::record_invalidation(sink);
        }
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Interne Hilfsfunktionen
// ──────────────────────────────────────────────────────────────────────────────

/// Domänentrenner für die deterministische Admission-`WorkId`.
const ADMISSION_DOMAIN: &[u8] = b"harw-plan-bridge:plan-node-admission:v1\0";

/// Präfix der deterministischen Admission-`WorkId`.
const ADMISSION_WORK_ID_PREFIX: &str = "plan-node-";

/// Ein Knoten, der gerade als Job admittiert werden darf (Draft/Ready,
/// Abhängigkeiten abgeschlossen, Arbeitsart).
fn job_candidate<'a>(snapshot: &'a Plan, task: &TaskId) -> Option<&'a PlanNode> {
    graph::ready_nodes(snapshot)
        .into_iter()
        .find(|node| &node.id == task && JOB_KINDS.contains(&node.kind))
}

/// Admittiert genau einen Knoten (siehe [`PlanJobBridge::admit_ready_nodes`]).
///
/// # Returns
/// `Some(WorkId)`, wenn der Knoten an einen Job gebunden wurde; `None`, wenn
/// er zwischenzeitlich nicht mehr admittierbar war.
fn admit_node(
    plan: &dyn PlanStore,
    jobs: &JobStore,
    template: &JobAdmissionTemplate,
    actor: &str,
    task: &TaskId,
) -> Result<Option<String>, PlanBridgeError> {
    // Schritt 1: Draft → Ready, bevor ein Job existiert (K5).
    apply_atomically(plan, actor, |snapshot| {
        match job_candidate(snapshot, task) {
            Some(node) if node.status == PlanNodeStatus::Draft => vec![PlanAction::SetStatus {
                id: task.clone(),
                status: PlanNodeStatus::Ready,
                reason: Some("wird als Job admittiert".to_owned()),
            }],
            _ => Vec::new(),
        }
    })?;

    let snapshot = plan.current()?;
    let Some(node) =
        job_candidate(&snapshot, task).filter(|node| node.status == PlanNodeStatus::Ready)
    else {
        tracing::debug!(task = %task, "Knoten ist nicht mehr admittierbar — übersprungen");
        return Ok(None);
    };

    // Schritt 2: Job mit deterministischer Identität (idempotent).
    let revision = snapshot.revision;
    let work_id = admission_work_id(&snapshot.id, &node.id, revision)?;
    let contract = contract_from_node(node, template.base_revision.clone(), revision);
    let attempt = match node.assignment.as_ref() {
        Some(assignment) => assignment.attempt.checked_add(1).ok_or_else(|| {
            callback_rejected(task, work_id.as_str(), "assignment attempt exhausted")
        })?,
        None => 0,
    };
    let payload = node_payload(snapshot.id.as_str(), node, revision, attempt, &contract)?;
    ensure_job_admitted(
        jobs,
        template,
        &work_id,
        payload,
        snapshot.id.as_str(),
        task,
    )?;

    // Schritt 3: Bindung und Start atomar.
    let events = apply_atomically(plan, actor, |current| match job_candidate(current, task) {
        Some(candidate)
            if current.id == snapshot.id
                && current.revision == revision
                && candidate.status == PlanNodeStatus::Ready =>
        {
            vec![
                PlanAction::UpdateNode {
                    id: task.clone(),
                    patch: NodePatch {
                        assignment: Some(Some(Assignment {
                            worker: actor.to_owned(),
                            attempt,
                            job: Some(work_id.as_str().to_owned()),
                        })),
                        ..Default::default()
                    },
                },
                PlanAction::SetStatus {
                    id: task.clone(),
                    status: PlanNodeStatus::InProgress,
                    reason: Some(format!("Job '{}' admittiert", work_id.as_str())),
                },
            ]
        }
        _ => Vec::new(),
    })?;
    if events.is_empty() {
        tracing::warn!(
            task = %task,
            work_id = work_id.as_str(),
            "Job angelegt, Knoten inzwischen nicht mehr admittierbar — Job ist verwaist"
        );
        return Ok(None);
    }

    // Erst nach dauerhafter Plan-Bindung claimbar machen. Ein Absturz hier
    // hinterlässt Pending + InProgress und wird vom Poller wieder aufgenommen.
    publish_bound_job(
        plan,
        jobs,
        snapshot.id.as_str(),
        task,
        &work_id,
        attempt,
        template.now,
    )?;

    tracing::info!(
        task = %task,
        work_id = work_id.as_str(),
        attempt = attempt,
        "Plan-Knoten als Job admittiert"
    );
    Ok(Some(work_id.as_str().to_owned()))
}

/// Veröffentlichung darf eine bereits von Recovery gestartete Ausführung
/// weder zurücksetzen noch fälschlich als fehlgeschlagene Admission melden.
fn publish_bound_job(
    plan: &dyn PlanStore,
    jobs: &JobStore,
    plan_id: &str,
    task: &TaskId,
    work_id: &WorkId,
    assignment_attempt: u32,
    now: jiff::Timestamp,
) -> Result<(), PlanBridgeError> {
    PlanJobBridge::validate_execution_binding(
        plan,
        plan_id,
        task,
        work_id.as_str(),
        assignment_attempt,
    )?;
    match jobs.mark_ready(work_id, now) {
        Ok(_) => Ok(()),
        Err(error) => {
            let existing = jobs
                .get(work_id)
                .map_err(|error| PlanBridgeError::JobStore(error.to_string()))?;
            let same_running_job = existing.job.state == JobState::Running
                && existing.job.kind == JobKind::Custom(PLAN_NODE_JOB_KIND.to_owned())
                && existing
                    .input
                    .get("plan_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(plan_id)
                && existing
                    .input
                    .get("task_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(task.as_str())
                && existing
                    .input
                    .get("assignment_attempt")
                    .and_then(serde_json::Value::as_u64)
                    == Some(u64::from(assignment_attempt));
            if same_running_job
                && PlanJobBridge::validate_execution_binding(
                    plan,
                    plan_id,
                    task,
                    work_id.as_str(),
                    assignment_attempt,
                )
                .is_ok()
            {
                return Ok(());
            }
            Err(PlanBridgeError::JobStore(error.to_string()))
        }
    }
}

/// Leitet die `WorkId` einer Admission deterministisch ab.
///
/// # Description
/// `plan-node-<BLAKE3-Hex>` über Domäne ‖ längenpräfixierte Plan-ID ‖
/// längenpräfixierte Task-ID ‖ Revision (u64 BE). Die Längenpräfixe machen
/// die Kodierung injektiv; das Ergebnis besteht nur aus `[a-z0-9-]` und ist
/// damit ein gültiges Job-Store-Pfadsegment.
///
/// # Errors
/// [`PlanBridgeError::JobStore`], falls die ID abgelehnt würde (nicht
/// erreichbar, da nie leer).
fn admission_work_id(
    plan_id: &PlanId,
    task: &TaskId,
    revision: RevisionId,
) -> Result<WorkId, PlanBridgeError> {
    let mut material: Vec<u8> = Vec::with_capacity(ADMISSION_DOMAIN.len() + 96);
    material.extend_from_slice(ADMISSION_DOMAIN);
    for part in [plan_id.as_str(), task.as_str()] {
        material.extend_from_slice(&(part.len() as u64).to_be_bytes());
        material.extend_from_slice(part.as_bytes());
    }
    material.extend_from_slice(&revision.value().to_be_bytes());
    let digest = ContentDigest::of(&material);
    WorkId::try_from_str(format!("{ADMISSION_WORK_ID_PREFIX}{digest}"))
        .map_err(|error| PlanBridgeError::JobStore(error.to_string()))
}

/// Legt den Job an oder übernimmt einen bereits vorhandenen, passenden Job.
///
/// # Errors
/// [`PlanBridgeError::JobStore`], wenn die Aufnahme scheitert oder unter der
/// `WorkId` ein nicht wiederverwendbarer Job liegt (terminal oder fremd).
fn ensure_job_admitted(
    jobs: &JobStore,
    template: &JobAdmissionTemplate,
    work_id: &WorkId,
    payload: serde_json::Value,
    plan_id: &str,
    task: &TaskId,
) -> Result<(), PlanBridgeError> {
    let job = Job::new(
        work_id.clone(),
        JobKind::Custom(PLAN_NODE_JOB_KIND.to_owned()),
        template.budget.clone(),
        template.retry.clone(),
        template.now,
    );
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
    match jobs.admit(&record) {
        Ok(()) => Ok(()),
        Err(SessionStoreError::JobAlreadyExists { .. }) => {
            let existing = jobs
                .get(work_id)
                .map_err(|error| PlanBridgeError::JobStore(error.to_string()))?;
            let belongs = existing.job.kind == JobKind::Custom(PLAN_NODE_JOB_KIND.to_owned())
                && existing
                    .input
                    .get("plan_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(plan_id)
                && existing
                    .input
                    .get("task_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(task.as_str());
            let reusable = existing.job.state == JobState::Pending;
            if belongs
                && (!legacy_payload_matches(&existing.input, &record.input)
                    || existing.scope != record.scope
                    || existing.job.budget != record.job.budget
                    || existing.job.retry != record.job.retry)
            {
                return Err(PlanBridgeError::JobStore(format!(
                    "Job '{}' has an incompatible immutable admission payload, scope, budget or retry policy; cancel the old job, explicitly invalidate then reopen its unfinished plan node and re-admit it; no assignment was bound",
                    work_id.as_str()
                )));
            }
            if belongs && reusable && legacy_payload_matches(&existing.input, &record.input) {
                tracing::debug!(
                    task = %task,
                    work_id = work_id.as_str(),
                    "Job dieser Knoten-Revision existiert bereits — wird wiederverwendet"
                );
                Ok(())
            } else {
                Err(PlanBridgeError::JobStore(format!(
                    "Job '{}' für Knoten '{task}' existiert bereits und ist nicht \
                     wiederverwendbar (Zustand {:?})",
                    work_id.as_str(),
                    existing.job.state
                )))
            }
        }
        Err(error) => Err(PlanBridgeError::JobStore(error.to_string())),
    }
}

/// Bindet den Rückkanal an die aktuelle Zuweisung im CAS-Snapshot.
/// Kein Claim-/Lease-Nachweis: diese Grenze prüft WorkId und Assignment-Attempt.
fn assigned_callback_node<'a>(
    snapshot: &'a Plan,
    task: &TaskId,
    work_id: &str,
    assignment_attempt: u32,
) -> Result<&'a PlanNode, PlanBridgeError> {
    let node = find_node(snapshot, task)
        .ok_or_else(|| PlanBridgeError::NodeNotFound { task: task.clone() })?;
    if work_id.is_empty()
        || node
            .assignment
            .as_ref()
            .and_then(|assignment| assignment.job.as_deref())
            != Some(work_id)
    {
        return Err(callback_rejected(
            task,
            work_id,
            "current job assignment differs",
        ));
    }
    if node
        .assignment
        .as_ref()
        .map(|assignment| assignment.attempt)
        != Some(assignment_attempt)
    {
        return Err(callback_rejected(
            task,
            work_id,
            "current assignment attempt differs",
        ));
    }
    Ok(node)
}

fn callback_rejected(task: &TaskId, work_id: &str, reason: &'static str) -> PlanBridgeError {
    PlanBridgeError::JobCallbackRejected {
        task: task.clone(),
        work_id: work_id.to_owned(),
        reason,
    }
}

// Ausschließlich das fehlende Owner-Feld eines vollständigen alten Legacy-
// Payloads normalisieren. Alle übrigen Felder müssen exakt übereinstimmen;
// Scope, Budget und Retry bleiben zusätzlich separat geprüft.
fn legacy_payload_matches(existing: &serde_json::Value, expected: &serde_json::Value) -> bool {
    for input in [existing, expected] {
        if PlanDispatchOwner::from_job_input(input)
            .and_then(|owner| owner.require_legacy_execution())
            .is_err()
        {
            return false;
        }
    }
    let mut normalized = existing.clone();
    if let Some(object) = normalized.as_object_mut() {
        object
            .entry("dispatch_owner")
            .or_insert_with(|| json!(PlanDispatchOwner::LegacyPlanJobs {}));
    }
    normalized == *expected
}

/// Baut den Job-Payload eines Plan-Knotens.
fn node_payload(
    plan_id: &str,
    node: &PlanNode,
    plan_revision: harw_plan::RevisionId,
    assignment_attempt: u32,
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
        "assignment_attempt": assignment_attempt,
        "dispatch_owner": PlanDispatchOwner::LegacyPlanJobs {},
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
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testing::{
        RecordingSink, ScriptedPlanStore, admission_template, coding_node, exploration_config,
        job_count, plan_id, research_node, seeded_plan_store, seeded_plan_store_with_config,
        temp_job_store,
    };
    use harw_plan::InMemoryPlanStore;
    use harw_plan::error::PlanError;

    fn template() -> TestResult<JobAdmissionTemplate> {
        admission_template()
    }

    fn job_store() -> TestResult<(JobStore, tempfile::TempDir)> {
        temp_job_store()
    }

    fn snapshot_of(plan: &dyn PlanStore) -> TestResult<Plan> {
        plan.current().map_err(ctx("current"))
    }

    #[test]
    fn dispatch_owner_roundtrip_and_missing_only_legacy_default() -> TestResult {
        let legacy = PlanDispatchOwner::from_job_input(&json!({})).map_err(ctx("legacy"))?;
        assert_eq!(legacy, PlanDispatchOwner::LegacyPlanJobs {});
        let raw = json!({"kind": "work_driver_prerequisite", "parent_work_id": "parent", "parent_lease_epoch": 7});
        let owner: PlanDispatchOwner = serde_json::from_value(raw.clone()).map_err(ctx("owner"))?;
        assert_eq!(serde_json::to_value(&owner).map_err(ctx("serialize"))?, raw);
        assert!(owner.require_legacy_execution().is_err());
        for invalid in [
            serde_json::Value::Null,
            json!({"kind": "unknown"}),
            json!({"kind": "legacy_plan_jobs", "parent_work_id": "forged"}),
            json!({"kind": "work_driver_prerequisite", "parent_work_id": "", "parent_lease_epoch": 7}),
            json!({"kind": "work_driver_prerequisite", "parent_work_id": "parent", "parent_lease_epoch": 0}),
            json!({"kind": "work_driver_prerequisite", "parent_work_id": "parent"}),
        ] {
            assert!(
                PlanDispatchOwner::from_job_input(&json!({"dispatch_owner": invalid})).is_err()
            );
        }
        assert!(PlanDispatchOwner::from_job_input(&json!(null)).is_err());
        Ok(())
    }

    #[test]
    fn pending_recovery_defaults_missing_owner_but_refuses_prerequisite_owner() -> TestResult {
        for owner in [
            None,
            Some(json!({"kind": "legacy_plan_jobs"})),
            Some(
                json!({"kind": "work_driver_prerequisite", "parent_work_id": "parent", "parent_lease_epoch": 1}),
            ),
            Some(serde_json::Value::Null),
        ] {
            let plan = seeded_plan_store(vec![assigned_node(PlanNodeStatus::InProgress)])?;
            let snapshot = snapshot_of(&plan)?;
            let node = &snapshot.nodes[0];
            let (jobs, _dir) = job_store()?;
            let template = template()?;
            let contract =
                contract_from_node(node, template.base_revision.clone(), snapshot.revision);
            let mut payload = node_payload("p-test", node, snapshot.revision, 1, &contract)
                .map_err(ctx("payload"))?;
            let object = payload
                .as_object_mut()
                .ok_or(TestError::Missing("object"))?;
            object.remove("dispatch_owner");
            let should_publish = owner
                .as_ref()
                .is_none_or(|value| value == &json!({"kind": "legacy_plan_jobs"}));
            if let Some(owner) = owner {
                object.insert("dispatch_owner".into(), owner);
            }
            let id = WorkId::from_str("current-job");
            ensure_job_admitted(&jobs, &template, &id, payload, "p-test", &node.id)
                .map_err(ctx("stage"))?;
            let published = PlanJobBridge::recover_pending_admissions(&plan, &jobs, template.now)
                .map_err(ctx("recover"))?;
            assert_eq!(published.len(), usize::from(should_publish));
            assert_eq!(
                jobs.get(&id).map_err(ctx("get"))?.job.state,
                if should_publish {
                    JobState::Ready
                } else {
                    JobState::Pending
                }
            );
        }
        Ok(())
    }

    #[test]
    fn test_admit_ready_nodes_creates_a_job_and_moves_the_node_in_progress() -> TestResult {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)])?;
        let (jobs, _dir) = job_store()?;

        let admitted = PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template()?, "runtime")
            .map_err(ctx("admit schlug fehl"))?;

        assert_eq!(admitted.len(), 1);
        let (task, work_id) = &admitted[0];
        assert_eq!(task, &TaskId::new("t-1"));
        assert!(!work_id.is_empty());

        let snapshot = plan.current().map_err(ctx("current"))?;
        let node = &snapshot.nodes[0];
        assert_eq!(node.status, PlanNodeStatus::InProgress);
        let assignment = node
            .assignment
            .as_ref()
            .ok_or(TestError::Missing("Assignment fehlt"))?;
        assert_eq!(assignment.job.as_deref(), Some(work_id.as_str()));
        assert_eq!(assignment.worker, "runtime");
        assert_eq!(assignment.attempt, 0);
        Ok(())
    }

    #[test]
    fn test_admit_ready_nodes_payload_carries_the_full_contract() -> TestResult {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)])?;
        let (jobs, _dir) = job_store()?;
        let revision_before = plan.revision();

        let admitted = PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template()?, "runtime")
            .map_err(ctx("admit schlug fehl"))?;
        let work_id = WorkId::from_str(admitted[0].1.as_str());
        let stored = jobs.get(&work_id).map_err(ctx("Job lesen"))?;

        assert_eq!(
            stored.job.kind,
            JobKind::Custom(PLAN_NODE_JOB_KIND.to_owned())
        );
        let input = &stored.input;
        assert_eq!(input["plan_id"].as_str(), Some("p-test"));
        assert_eq!(input["task_id"].as_str(), Some("t-1"));
        assert_eq!(input["assignment_attempt"].as_u64(), Some(0));
        assert_eq!(input["dispatch_owner"], json!({"kind": "legacy_plan_jobs"}));
        assert_eq!(
            input["plan_revision"].as_u64(),
            Some(revision_before.value())
        );
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
        Ok(())
    }

    #[test]
    fn test_admit_ready_nodes_promotes_a_draft_node_through_ready() -> TestResult {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Draft)])?;
        let (jobs, _dir) = job_store()?;

        PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template()?, "runtime")
            .map_err(ctx("admit schlug fehl"))?;

        let snapshot = plan.current().map_err(ctx("current"))?;
        assert_eq!(snapshot.nodes[0].status, PlanNodeStatus::InProgress);
        Ok(())
    }

    #[test]
    fn test_admit_ready_nodes_skips_non_work_kinds() -> TestResult {
        let plan = seeded_plan_store(vec![research_node("r-1", PlanNodeStatus::Ready)])?;
        let (jobs, _dir) = job_store()?;

        match PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template()?, "runtime") {
            Err(PlanBridgeError::NoReadyNodes) => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet NoReadyNodes, bekommen: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_admit_ready_nodes_reports_no_ready_nodes_on_an_empty_plan() -> TestResult {
        let plan = InMemoryPlanStore::new();
        plan.apply(
            PlanAction::Create {
                plan_id: plan_id("p-empty")?,
                goal: "Ziel".to_owned(),
            },
            "test",
        )
        .map_err(ctx("Plan anlegen"))?;
        let (jobs, _dir) = job_store()?;

        match PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template()?, "runtime") {
            Err(PlanBridgeError::NoReadyNodes) => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet NoReadyNodes, bekommen: {other:?}"
                )));
            }
        }
        Ok(())
    }

    struct PublicationProbe {
        inner: InMemoryPlanStore,
        probe: Box<dyn Fn() + Send + Sync>,
    }

    impl PlanStore for PublicationProbe {
        fn current(&self) -> harw_plan::PlanResult<Plan> {
            self.inner.current()
        }
        fn revision(&self) -> RevisionId {
            self.inner.revision()
        }
        fn apply(
            &self,
            action: PlanAction,
            actor: &str,
        ) -> harw_plan::PlanResult<harw_plan::PlanEvent> {
            self.inner.apply(action, actor)
        }
        fn history(
            &self,
            since: Option<RevisionId>,
        ) -> harw_plan::PlanResult<Vec<harw_plan::PlanEvent>> {
            self.inner.history(since)
        }
        fn apply_batch(
            &self,
            plan: &PlanId,
            actions: Vec<PlanAction>,
            actor: &str,
            expected_rev: RevisionId,
        ) -> harw_plan::PlanResult<harw_plan::PlanRevision> {
            self.inner.apply_batch(plan, actions, actor, expected_rev)
        }
        fn apply_batch_checked(
            &self,
            plan: &PlanId,
            actions: Vec<PlanAction>,
            actor: &str,
            expected_rev: RevisionId,
            check: &dyn Fn() -> harw_plan::PlanResult<()>,
        ) -> harw_plan::PlanResult<harw_plan::PlanRevision> {
            self.inner
                .apply_batch_checked(plan, actions, actor, expected_rev, &|| {
                    (self.probe)();
                    check()
                })
        }
    }

    #[test]
    fn claimed_callbacks_reject_expired_nonce_attempt_and_reclaimed_tokens() -> TestResult {
        for failure in [false, true] {
            let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)])
                .map_err(ctx("claimed callback fixture"))?;
            let (jobs, _dir) = job_store().map_err(ctx("claimed callback fixture"))?;
            let mut template = template().map_err(ctx("claimed callback fixture"))?;
            template.retry.max_attempts = 3;
            let admitted = PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template, "runtime")
                .map_err(ctx("claimed callback fixture"))?;
            let (task, work_id) = &admitted[0];
            let id = WorkId::from_str(work_id);
            let expired = jobs
                .claim(
                    &id,
                    &harw_session_store::ClaimRequest {
                        worker_id: "old".to_owned(),
                        lease_ttl: jiff::SignedDuration::from_secs(1),
                        now: template.now,
                    },
                )
                .map_err(ctx("claimed callback fixture"))?;
            let callback = |token: &harw_job_runtime::LeaseToken, attempt| {
                if failure {
                    PlanJobBridge::on_job_failed_claimed(
                        &plan, &jobs, token, task, attempt, "failed", "runtime",
                    )
                } else {
                    PlanJobBridge::on_job_completed_claimed(
                        &plan,
                        &jobs,
                        token,
                        task,
                        attempt,
                        "done",
                        "runtime",
                        OffsetDateTime::UNIX_EPOCH,
                    )
                }
            };
            let before = plan.revision();
            assert!(callback(&expired.token, 0).is_err());
            jobs.reclaim(
                &id,
                jiff::Timestamp::now(),
                ApprovalActor::Operator {
                    id: "test".to_owned(),
                },
            )
            .map_err(ctx("claimed callback fixture"))?;
            let live = jobs
                .claim(
                    &id,
                    &harw_session_store::ClaimRequest {
                        worker_id: "replacement".to_owned(),
                        lease_ttl: jiff::SignedDuration::from_secs(60),
                        now: jiff::Timestamp::now(),
                    },
                )
                .map_err(ctx("claimed callback fixture"))?;
            assert!(callback(&expired.token, 0).is_err());
            let mut forged = live.token.clone();
            forged.nonce.push_str("wrong");
            assert!(callback(&forged, 0).is_err());
            assert!(callback(&live.token, 1).is_err());
            assert_eq!(plan.revision(), before);
            callback(&live.token, 0).map_err(ctx("claimed callback fixture"))?;
            assert!(plan.revision() > before);
        }
        Ok(())
    }

    #[test]
    fn callback_holds_lease_against_concurrent_reclaim_and_checks_expiry_at_publish() -> TestResult
    {
        for expire in [false, true] {
            let inner = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)])
                .map_err(ctx("claimed callback fixture"))?;
            let (jobs, _dir) = job_store().map_err(ctx("claimed callback fixture"))?;
            let jobs = std::sync::Arc::new(jobs);
            let admitted = PlanJobBridge::admit_ready_nodes(
                &inner,
                jobs.as_ref(),
                &template().map_err(ctx("claimed callback fixture"))?,
                "runtime",
            )
            .map_err(ctx("claimed callback fixture"))?;
            let (task, work_id) = &admitted[0];
            let id = WorkId::from_str(work_id);
            let claim = jobs
                .claim(
                    &id,
                    &harw_session_store::ClaimRequest {
                        worker_id: "worker".to_owned(),
                        lease_ttl: jiff::SignedDuration::from_millis(if expire {
                            100
                        } else {
                            60_000
                        }),
                        now: jiff::Timestamp::now(),
                    },
                )
                .map_err(ctx("claimed callback fixture"))?;
            let probing_jobs = std::sync::Arc::clone(&jobs);
            let probing_id = id.clone();
            let plan = PublicationProbe {
                inner,
                probe: Box::new(move || {
                    std::thread::scope(|scope| {
                        let attempt = scope.spawn(|| {
                            probing_jobs.reclaim(
                                &probing_id,
                                jiff::Timestamp::now(),
                                ApprovalActor::Operator {
                                    id: "racer".to_owned(),
                                },
                            )
                        });
                        assert!(matches!(
                            attempt.join(),
                            Ok(Err(SessionStoreError::JobLockContended { .. }))
                        ));
                    });
                    assert!(matches!(
                        probing_jobs.cancel(
                            &probing_id,
                            &harw_session_store::CancelRequest {
                                cancelled_at: jiff::Timestamp::now(),
                                cancelled_by: ApprovalActor::Operator {
                                    id: "cancel-racer".to_owned()
                                },
                                reason: "cancel during callback".to_owned(),
                            },
                        ),
                        Err(SessionStoreError::JobLockContended { .. })
                    ));
                    if expire {
                        std::thread::sleep(std::time::Duration::from_millis(150));
                    }
                }),
            };
            let before = plan.revision();
            let result = PlanJobBridge::on_job_completed_claimed(
                &plan,
                jobs.as_ref(),
                &claim.token,
                task,
                0,
                "done",
                "runtime",
                OffsetDateTime::UNIX_EPOCH,
            );
            assert_eq!(result.is_err(), expire);
            if expire {
                assert_eq!(plan.revision(), before);
            }
            // The callback never retains the job lock after returning.
            jobs.reclaim(
                &id,
                jiff::Timestamp::now(),
                ApprovalActor::Operator {
                    id: "after".to_owned(),
                },
            )
            .map_err(ctx("claimed callback fixture"))?;
        }
        Ok(())
    }

    #[test]
    fn test_on_job_completed_attaches_evidence_and_completes() -> TestResult {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)])?;
        let (jobs, _dir) = job_store()?;
        let admitted = PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template()?, "runtime")
            .map_err(ctx("admit schlug fehl"))?;
        let (task, work_id) = &admitted[0];

        PlanJobBridge::on_job_completed(
            &plan,
            task,
            work_id,
            0,
            "cargo test grün",
            "runtime",
            OffsetDateTime::UNIX_EPOCH,
        )
        .map_err(ctx("on_job_completed schlug fehl"))?;

        let snapshot = plan.current().map_err(ctx("current"))?;
        let node = &snapshot.nodes[0];
        assert_eq!(node.status, PlanNodeStatus::Completed);
        assert_eq!(node.evidence.len(), 1);
        assert_eq!(node.evidence[0].kind, EvidenceKind::Job);
        assert_eq!(&node.evidence[0].locator, work_id);
        Ok(())
    }

    #[test]
    fn test_on_job_completed_rejects_an_unknown_task() -> TestResult {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)])?;

        match PlanJobBridge::on_job_completed(
            &plan,
            &TaskId::new("t-unbekannt"),
            "work-1",
            0,
            "fertig",
            "runtime",
            OffsetDateTime::UNIX_EPOCH,
        ) {
            Err(PlanBridgeError::NodeNotFound { task }) => {
                assert_eq!(task, TaskId::new("t-unbekannt"));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet NodeNotFound, bekommen: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_on_job_failed_invalidates_the_node_without_reopening_it() -> TestResult {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)])?;
        let (jobs, _dir) = job_store()?;
        let admitted = PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template()?, "runtime")
            .map_err(ctx("admit schlug fehl"))?;
        let (task, work_id) = &admitted[0];

        PlanJobBridge::on_job_failed(&plan, task, work_id, 0, "clippy rot", "runtime")
            .map_err(ctx("on_job_failed schlug fehl"))?;

        let snapshot = plan.current().map_err(ctx("current"))?;
        assert_eq!(snapshot.nodes[0].status, PlanNodeStatus::Invalidated);
        Ok(())
    }

    #[test]
    fn test_on_job_failed_rejects_an_unknown_task() -> TestResult {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)])?;

        match PlanJobBridge::on_job_failed(
            &plan,
            &TaskId::new("t-unbekannt"),
            "work-1",
            0,
            "kaputt",
            "runtime",
        ) {
            Err(PlanBridgeError::NodeNotFound { .. }) => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet NodeNotFound, bekommen: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_on_job_completed_observed_emits_evidence_attached_total() -> TestResult {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)])?;
        let (jobs, _dir) = job_store()?;
        let admitted = PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template()?, "runtime")
            .map_err(ctx("admit schlug fehl"))?;
        let (task, work_id) = &admitted[0];
        let sink = RecordingSink::new();

        PlanJobBridge::on_job_completed_observed(
            &plan,
            task,
            work_id,
            0,
            "cargo test grün",
            "runtime",
            OffsetDateTime::UNIX_EPOCH,
            Some(&sink),
        )
        .map_err(ctx("on_job_completed_observed schlug fehl"))?;

        assert_eq!(sink.values_for(EVIDENCE_ATTACHED_TOTAL.name).len(), 1);
        assert!(sink.values_for(INVALIDATIONS_TOTAL.name).is_empty());
        Ok(())
    }

    #[test]
    fn test_on_job_failed_observed_emits_invalidations_total() -> TestResult {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)])?;
        let (jobs, _dir) = job_store()?;
        let admitted = PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template()?, "runtime")
            .map_err(ctx("admit schlug fehl"))?;
        let (task, work_id) = &admitted[0];
        let sink = RecordingSink::new();

        PlanJobBridge::on_job_failed_observed(
            &plan,
            task,
            work_id,
            0,
            "clippy rot",
            "runtime",
            Some(&sink),
        )
        .map_err(ctx("on_job_failed_observed schlug fehl"))?;

        assert_eq!(sink.values_for(INVALIDATIONS_TOTAL.name).len(), 1);
        assert!(sink.values_for(EVIDENCE_ATTACHED_TOTAL.name).is_empty());
        Ok(())
    }

    #[test]
    fn test_on_job_failed_observed_without_a_sink_still_invalidates() -> TestResult {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)])?;
        let (jobs, _dir) = job_store()?;
        let admitted = PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template()?, "runtime")
            .map_err(ctx("admit schlug fehl"))?;
        let (task, work_id) = &admitted[0];

        PlanJobBridge::on_job_failed_observed(
            &plan,
            task,
            work_id,
            0,
            "clippy rot",
            "runtime",
            None,
        )
        .map_err(ctx("on_job_failed_observed schlug fehl"))?;

        let snapshot = plan.current().map_err(ctx("current"))?;
        assert_eq!(snapshot.nodes[0].status, PlanNodeStatus::Invalidated);
        Ok(())
    }

    #[test]
    fn test_template_exposes_its_submitter() -> TestResult {
        let template = template()?;
        assert!(matches!(
            template.submitter(),
            ApprovalActor::Operator { id } if id == "operator-1"
        ));
        Ok(())
    }

    #[test]
    fn test_admit_ready_nodes_twice_admits_exactly_one_job() -> TestResult {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)])?;
        let (jobs, _dir) = job_store()?;

        PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template()?, "runtime")
            .map_err(ctx("erste Admission schlug fehl"))?;
        match PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template()?, "runtime") {
            Err(PlanBridgeError::NoReadyNodes) => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet NoReadyNodes, bekommen: {other:?}"
                )));
            }
        }
        assert_eq!(job_count(&jobs)?, 1);
        Ok(())
    }

    #[test]
    fn test_admit_ready_nodes_is_idempotent_after_a_failed_plan_batch() -> TestResult {
        let plan = ScriptedPlanStore::new(seeded_plan_store(vec![coding_node(
            "t-1",
            PlanNodeStatus::Ready,
        )])?);
        let (jobs, _dir) = job_store()?;
        plan.inject_failures(1);

        match PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template()?, "runtime") {
            Err(PlanBridgeError::Plan(PlanError::Io(_))) => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet injizierten Batch-Fehler, bekommen: {other:?}"
                )));
            }
        }
        // Der Job existiert, der Knoten ist aber noch nicht gebunden.
        assert_eq!(job_count(&jobs)?, 1);
        let first = snapshot_of(&plan)?;
        assert_eq!(first.nodes[0].status, PlanNodeStatus::Ready);
        assert!(first.nodes[0].assignment.is_none());

        let admitted = PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template()?, "runtime")
            .map_err(ctx("Wiederholung schlug fehl"))?;

        // Kein zweiter Job: derselbe Job wird gebunden.
        assert_eq!(job_count(&jobs)?, 1);
        assert_eq!(admitted.len(), 1);
        let bound = snapshot_of(&plan)?;
        let node = &bound.nodes[0];
        assert_eq!(node.status, PlanNodeStatus::InProgress);
        assert_eq!(
            node.assignment
                .as_ref()
                .and_then(|assignment| assignment.job.as_deref()),
            Some(admitted[0].1.as_str())
        );
        let page = jobs
            .list(&harw_session_store::JobListQuery::default())
            .map_err(ctx("Jobs auflisten"))?;
        assert_eq!(page.jobs[0].job.id.as_str(), admitted[0].1);
        Ok(())
    }

    #[test]
    fn test_admit_ready_nodes_does_not_bind_a_stale_payload_after_a_revision_conflict() -> TestResult
    {
        let plan = ScriptedPlanStore::new(seeded_plan_store(vec![coding_node(
            "t-1",
            PlanNodeStatus::Ready,
        )])?);
        let (jobs, _dir) = job_store()?;
        plan.inject_conflicts(1);

        let admitted = PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template()?, "runtime")
            .map_err(ctx("admit schlug fehl"))?;

        assert!(
            admitted.is_empty(),
            "stale source revision must not be bound"
        );
        assert_eq!(plan.batch_calls(), 1);
        assert_eq!(job_count(&jobs)?, 1, "cross-store orphan remains explicit");
        let old = jobs
            .list(&harw_session_store::JobListQuery::default())
            .map_err(ctx("list"))?;
        let orphan = old.jobs[0].job.id.as_str();
        assert!(
            PlanJobBridge::validate_execution_binding(
                &plan,
                "p-test",
                &TaskId::new("t-1"),
                orphan,
                0
            )
            .is_err()
        );
        let retry = PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template()?, "runtime")
            .map_err(ctx("fresh admission"))?;
        assert_eq!(retry.len(), 1);
        assert_ne!(retry[0].1, orphan);
        assert_eq!(job_count(&jobs)?, 2);
        assert_eq!(
            snapshot_of(&plan)?.nodes[0].status,
            PlanNodeStatus::InProgress
        );
        Ok(())
    }

    #[test]
    fn test_admit_ready_nodes_creates_no_job_when_exploration_is_missing() -> TestResult {
        let plan = seeded_plan_store_with_config(
            exploration_config(),
            vec![coding_node("t-1", PlanNodeStatus::Draft)],
        )?;
        let (jobs, _dir) = job_store()?;

        match PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template()?, "runtime") {
            Err(PlanBridgeError::Plan(PlanError::BatchActionRejected { source, .. })) => {
                assert!(
                    matches!(*source, PlanError::ExplorationRequired { .. }),
                    "unerwartete Ursache: {source}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet ExplorationRequired, bekommen: {other:?}"
                )));
            }
        }
        assert_eq!(
            job_count(&jobs)?,
            0,
            "Waisen-Job trotz abgelehnter Admission"
        );
        assert_eq!(snapshot_of(&plan)?.nodes[0].status, PlanNodeStatus::Draft);
        Ok(())
    }

    #[test]
    fn test_admission_work_id_is_deterministic_per_node_and_revision() -> TestResult {
        let plan = plan_id("p-test")?;
        let task = TaskId::new("t-1");
        let first = admission_work_id(&plan, &task, RevisionId::new(3));
        let second = admission_work_id(&plan, &task, RevisionId::new(3));
        let other_revision = admission_work_id(&plan, &task, RevisionId::new(4));
        let other_task = admission_work_id(&plan, &TaskId::new("t-2"), RevisionId::new(3));
        match (first, second, other_revision, other_task) {
            (Ok(first), Ok(second), Ok(other_revision), Ok(other_task)) => {
                assert_eq!(first, second);
                assert_ne!(first, other_revision);
                assert_ne!(first, other_task);
                assert!(first.as_str().starts_with(ADMISSION_WORK_ID_PREFIX));
                assert!(
                    first
                        .as_str()
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "WorkId-Ableitung schlug fehl: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_on_job_completed_twice_is_a_noop() -> TestResult {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)])?;
        let (jobs, _dir) = job_store()?;
        let admitted = PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template()?, "runtime")
            .map_err(ctx("admit schlug fehl"))?;
        let (task, work_id) = &admitted[0];
        for round in 0..2 {
            PlanJobBridge::on_job_completed(
                &plan,
                task,
                work_id,
                0,
                "fertig",
                "runtime",
                OffsetDateTime::UNIX_EPOCH,
            )
            .map_err(|error| {
                TestError::Unexpected(format!("on_job_completed Runde {round}: {error}"))
            })?;
        }
        let revision_after_first = plan.revision();
        PlanJobBridge::on_job_completed(
            &plan,
            task,
            work_id,
            0,
            "fertig",
            "runtime",
            OffsetDateTime::UNIX_EPOCH,
        )
        .map_err(ctx("on_job_completed dritte Runde"))?;
        assert_eq!(
            plan.revision(),
            revision_after_first,
            "No-op erzeugte eine Revision"
        );
        let snapshot = snapshot_of(&plan)?;
        assert_eq!(snapshot.nodes[0].status, PlanNodeStatus::Completed);
        assert_eq!(snapshot.nodes[0].evidence.len(), 1);
        Ok(())
    }

    #[test]
    fn test_on_job_completed_on_a_ready_node_attaches_nothing() -> TestResult {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)])?;

        match PlanJobBridge::on_job_completed(
            &plan,
            &TaskId::new("t-1"),
            "work-x",
            0,
            "fertig",
            "runtime",
            OffsetDateTime::UNIX_EPOCH,
        ) {
            Err(PlanBridgeError::JobCallbackRejected { .. }) => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet JobCallbackRejected, bekommen: {other:?}"
                )));
            }
        }
        let snapshot = snapshot_of(&plan)?;
        assert!(
            snapshot.nodes[0].evidence.is_empty(),
            "Evidenz trotz Abbruch angehängt"
        );
        assert_eq!(snapshot.nodes[0].status, PlanNodeStatus::Ready);
        Ok(())
    }

    #[test]
    fn test_on_job_failed_twice_is_a_noop() -> TestResult {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)])?;
        let (jobs, _dir) = job_store()?;
        let admitted = PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template()?, "runtime")
            .map_err(ctx("admit"))?;
        let (task, work_id) = &admitted[0];
        for round in 0..2 {
            PlanJobBridge::on_job_failed(&plan, task, work_id, 0, "rot", "runtime").map_err(
                |error| TestError::Unexpected(format!("on_job_failed Runde {round}: {error}")),
            )?;
        }
        assert_eq!(
            snapshot_of(&plan)?.nodes[0].status,
            PlanNodeStatus::Invalidated
        );
        Ok(())
    }
    fn assigned_node(status: PlanNodeStatus) -> PlanNode {
        let mut node = coding_node("t-1", status);
        node.assignment = Some(Assignment {
            worker: "runtime".to_owned(),
            attempt: 1,
            job: Some("current-job".to_owned()),
        });
        node
    }

    #[test]
    fn callbacks_reject_foreign_jobs_without_mutation() -> TestResult {
        for status in [
            PlanNodeStatus::InProgress,
            PlanNodeStatus::Completed,
            PlanNodeStatus::Invalidated,
        ] {
            let plan = seeded_plan_store(vec![assigned_node(status)])?;
            let before = serde_json::to_value(snapshot_of(&plan)?).map_err(ctx("serialize"))?;
            assert!(
                PlanJobBridge::on_job_completed(
                    &plan,
                    &TaskId::new("t-1"),
                    "old-job",
                    1,
                    "done",
                    "runtime",
                    OffsetDateTime::UNIX_EPOCH
                )
                .is_err()
            );
            assert!(
                PlanJobBridge::on_job_failed(
                    &plan,
                    &TaskId::new("t-1"),
                    "old-job",
                    1,
                    "failed",
                    "runtime"
                )
                .is_err()
            );
            assert_eq!(
                serde_json::to_value(snapshot_of(&plan)?).map_err(ctx("serialize"))?,
                before
            );
        }
        Ok(())
    }

    #[test]
    fn completed_without_matching_job_evidence_is_not_a_successful_replay() -> TestResult {
        let plan = seeded_plan_store(vec![assigned_node(PlanNodeStatus::Completed)])?;
        let revision = plan.revision();
        assert!(
            PlanJobBridge::on_job_completed(
                &plan,
                &TaskId::new("t-1"),
                "current-job",
                1,
                "done",
                "runtime",
                OffsetDateTime::UNIX_EPOCH
            )
            .is_err()
        );
        assert_eq!(plan.revision(), revision);
        Ok(())
    }

    #[test]
    fn failed_callback_cannot_invalidate_completed_or_reopened_work() -> TestResult {
        for status in [
            PlanNodeStatus::Completed,
            PlanNodeStatus::Draft,
            PlanNodeStatus::Ready,
        ] {
            let plan = seeded_plan_store(vec![assigned_node(status)])?;
            let revision = plan.revision();
            assert!(
                PlanJobBridge::on_job_failed(
                    &plan,
                    &TaskId::new("t-1"),
                    "current-job",
                    1,
                    "failed",
                    "runtime"
                )
                .is_err()
            );
            assert_eq!(plan.revision(), revision);
        }
        Ok(())
    }

    struct ReassigningStore {
        inner: InMemoryPlanStore,
        reassign: std::sync::atomic::AtomicBool,
        replacement_job: &'static str,
        staged_jobs: Option<std::sync::Arc<JobStore>>,
    }

    impl PlanStore for ReassigningStore {
        fn current(&self) -> harw_plan::PlanResult<Plan> {
            self.inner.current()
        }
        fn revision(&self) -> RevisionId {
            self.inner.revision()
        }
        fn apply(
            &self,
            action: PlanAction,
            actor: &str,
        ) -> harw_plan::PlanResult<harw_plan::PlanEvent> {
            self.inner.apply(action, actor)
        }
        fn history(
            &self,
            since: Option<RevisionId>,
        ) -> harw_plan::PlanResult<Vec<harw_plan::PlanEvent>> {
            self.inner.history(since)
        }
        fn apply_batch(
            &self,
            plan: &PlanId,
            actions: Vec<PlanAction>,
            actor: &str,
            expected_rev: RevisionId,
        ) -> harw_plan::PlanResult<harw_plan::PlanRevision> {
            if let Some(jobs) = &self.staged_jobs {
                for action in &actions {
                    let PlanAction::UpdateNode { patch, .. } = action else {
                        continue;
                    };
                    let Some(Some(assignment)) = &patch.assignment else {
                        continue;
                    };
                    let Some(id) = &assignment.job else {
                        continue;
                    };
                    let id = WorkId::from_str(id);
                    let record = jobs
                        .get(&id)
                        .map_err(|error| PlanError::Io(std::io::Error::other(error.to_string())))?;
                    assert_eq!(
                        record.job.state,
                        JobState::Pending,
                        "job must not be claimable before binding"
                    );
                    assert!(
                        jobs.claim(
                            &id,
                            &harw_session_store::ClaimRequest {
                                worker_id: "racer".to_owned(),
                                lease_ttl: jiff::SignedDuration::from_secs(60),
                                now: record.submitted_at
                            }
                        )
                        .is_err()
                    );
                }
            }
            if self
                .reassign
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                self.inner.apply(
                    PlanAction::UpdateNode {
                        id: TaskId::new("t-1"),
                        patch: NodePatch {
                            assignment: Some(Some(Assignment {
                                worker: "replacement".to_owned(),
                                attempt: 2,
                                job: Some(self.replacement_job.to_owned()),
                            })),
                            ..NodePatch::default()
                        },
                    },
                    "replacement",
                )?;
            }
            self.inner.apply_batch(plan, actions, actor, expected_rev)
        }
    }

    #[test]
    fn claimed_callback_fails_closed_on_store_without_publication_hook() -> TestResult {
        let inner = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)])?;
        let (jobs, _dir) = job_store()?;
        let admitted = PlanJobBridge::admit_ready_nodes(&inner, &jobs, &template()?, "runtime")
            .map_err(ctx("admit"))?;
        let (task, work_id) = &admitted[0];
        let claim = jobs
            .claim(
                &WorkId::from_str(work_id),
                &harw_session_store::ClaimRequest {
                    worker_id: "worker".to_owned(),
                    lease_ttl: jiff::SignedDuration::from_secs(60),
                    now: jiff::Timestamp::now(),
                },
            )
            .map_err(ctx("claim"))?;
        let plan = ReassigningStore {
            inner,
            reassign: std::sync::atomic::AtomicBool::new(false),
            replacement_job: "unused",
            staged_jobs: None,
        };
        let before = plan.revision();
        assert!(matches!(
            PlanJobBridge::on_job_completed_claimed(
                &plan,
                &jobs,
                &claim.token,
                task,
                0,
                "done",
                "runtime",
                OffsetDateTime::UNIX_EPOCH,
            ),
            Err(PlanBridgeError::Plan(PlanError::CatalogUnsupported {
                operation: "apply_batch_checked"
            }))
        ));
        assert_eq!(plan.revision(), before);
        Ok(())
    }

    #[test]
    fn callbacks_revalidate_assignment_after_cas_conflict() -> TestResult {
        for (completion, replacement_job) in [
            (true, "replacement-job"),
            (false, "replacement-job"),
            (true, "current-job"),
            (false, "current-job"),
        ] {
            let plan = ReassigningStore {
                inner: seeded_plan_store(vec![assigned_node(PlanNodeStatus::InProgress)])?,
                reassign: std::sync::atomic::AtomicBool::new(true),
                replacement_job,
                staged_jobs: None,
            };
            let result = if completion {
                PlanJobBridge::on_job_completed(
                    &plan,
                    &TaskId::new("t-1"),
                    "current-job",
                    1,
                    "done",
                    "runtime",
                    OffsetDateTime::UNIX_EPOCH,
                )
            } else {
                PlanJobBridge::on_job_failed(
                    &plan,
                    &TaskId::new("t-1"),
                    "current-job",
                    1,
                    "failed",
                    "runtime",
                )
            };
            assert!(result.is_err());
            let node = &snapshot_of(&plan)?.nodes[0];
            assert_eq!(node.status, PlanNodeStatus::InProgress);
            assert!(node.evidence.is_empty());
            assert_eq!(
                node.assignment.as_ref().and_then(|a| a.job.as_deref()),
                Some(replacement_job)
            );
        }
        Ok(())
    }

    #[test]
    fn callback_binding_and_completion_replay_survive_restart() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let task = TaskId::new("t-1");
        {
            let plan = harw_plan::FilePlanStore::new(dir.path()).map_err(ctx("file store"))?;
            plan.apply(
                PlanAction::Create {
                    plan_id: plan_id("p-test")?,
                    goal: "test".to_owned(),
                },
                "test",
            )
            .map_err(ctx("create"))?;
            plan.apply(
                PlanAction::AddNode {
                    node: assigned_node(PlanNodeStatus::Draft),
                },
                "test",
            )
            .map_err(ctx("add"))?;
            for status in [PlanNodeStatus::Ready, PlanNodeStatus::InProgress] {
                plan.apply(
                    PlanAction::SetStatus {
                        id: task.clone(),
                        status,
                        reason: None,
                    },
                    "test",
                )
                .map_err(ctx("status"))?;
            }
            PlanJobBridge::on_job_completed(
                &plan,
                &task,
                "current-job",
                1,
                "done",
                "runtime",
                OffsetDateTime::UNIX_EPOCH,
            )
            .map_err(ctx("complete"))?;
        }
        let plan = harw_plan::FilePlanStore::new(dir.path()).map_err(ctx("reload"))?;
        let before = serde_json::to_value(snapshot_of(&plan)?).map_err(ctx("serialize"))?;
        PlanJobBridge::on_job_completed(
            &plan,
            &task,
            "current-job",
            1,
            "done",
            "runtime",
            OffsetDateTime::UNIX_EPOCH,
        )
        .map_err(ctx("replay"))?;
        assert!(
            PlanJobBridge::on_job_completed(
                &plan,
                &task,
                "old-job",
                1,
                "done",
                "runtime",
                OffsetDateTime::UNIX_EPOCH
            )
            .is_err()
        );
        assert!(
            PlanJobBridge::on_job_failed(&plan, &task, "old-job", 1, "failed", "runtime").is_err()
        );
        assert_eq!(
            serde_json::to_value(snapshot_of(&plan)?).map_err(ctx("serialize"))?,
            before
        );
        Ok(())
    }
    #[test]
    fn callbacks_reject_previous_attempt_even_when_work_id_is_reused() -> TestResult {
        let plan = seeded_plan_store(vec![assigned_node(PlanNodeStatus::Invalidated)])?;
        let task = TaskId::new("t-1");
        plan.apply(
            PlanAction::Reopen {
                id: task.clone(),
                reason: "retry".to_owned(),
            },
            "operator",
        )
        .map_err(ctx("reopen"))?;
        for status in [PlanNodeStatus::Ready, PlanNodeStatus::InProgress] {
            plan.apply(
                PlanAction::SetStatus {
                    id: task.clone(),
                    status,
                    reason: None,
                },
                "operator",
            )
            .map_err(ctx("status"))?;
        }
        let revision = plan.revision();
        assert!(
            PlanJobBridge::on_job_completed(
                &plan,
                &task,
                "current-job",
                1,
                "done",
                "runtime",
                OffsetDateTime::UNIX_EPOCH
            )
            .is_err()
        );
        assert!(
            PlanJobBridge::on_job_failed(&plan, &task, "current-job", 1, "failed", "runtime")
                .is_err()
        );
        assert_eq!(plan.revision(), revision);
        PlanJobBridge::on_job_completed(
            &plan,
            &task,
            "current-job",
            2,
            "done",
            "runtime",
            OffsetDateTime::UNIX_EPOCH,
        )
        .map_err(ctx("current attempt completes"))?;
        let completed_revision = plan.revision();
        PlanJobBridge::on_job_completed(
            &plan,
            &task,
            "current-job",
            2,
            "done",
            "runtime",
            OffsetDateTime::UNIX_EPOCH,
        )
        .map_err(ctx("current attempt replays"))?;
        assert_eq!(plan.revision(), completed_revision);
        Ok(())
    }
    #[test]
    fn admission_refuses_exhausted_attempt_before_creating_job() -> TestResult {
        let mut node = assigned_node(PlanNodeStatus::Ready);
        if let Some(assignment) = node.assignment.as_mut() {
            assignment.attempt = u32::MAX;
        }
        let plan = seeded_plan_store(vec![node])?;
        let (jobs, _dir) = job_store()?;
        let revision = plan.revision();
        assert!(PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template()?, "runtime").is_err());
        assert_eq!(job_count(&jobs)?, 0);
        assert_eq!(plan.revision(), revision);
        Ok(())
    }
    #[test]
    fn admission_publishes_ready_only_after_plan_binding() -> TestResult {
        let (jobs, _dir) = job_store()?;
        let jobs = std::sync::Arc::new(jobs);
        let plan = ReassigningStore {
            inner: seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)])?,
            reassign: std::sync::atomic::AtomicBool::new(false),
            replacement_job: "unused",
            staged_jobs: Some(std::sync::Arc::clone(&jobs)),
        };
        let template = template()?;
        let admitted = PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template, "runtime")
            .map_err(ctx("admit"))?;
        let (task, work_id) = &admitted[0];
        PlanJobBridge::validate_execution_binding(&plan, "p-test", task, work_id, 0)
            .map_err(ctx("binding"))?;
        jobs.claim(
            &WorkId::from_str(work_id),
            &harw_session_store::ClaimRequest {
                worker_id: "worker".to_owned(),
                lease_ttl: jiff::SignedDuration::from_secs(60),
                now: template.now,
            },
        )
        .map_err(ctx("claim published"))?;
        let before = jobs
            .get(&WorkId::from_str(work_id))
            .map_err(ctx("running"))?;
        publish_bound_job(
            &plan,
            &jobs,
            "p-test",
            task,
            &WorkId::from_str(work_id),
            0,
            template.now,
        )
        .map_err(ctx("concurrent recovery already claimed"))?;
        let after = jobs
            .get(&WorkId::from_str(work_id))
            .map_err(ctx("still running"))?;
        assert_eq!(after.job.state, JobState::Running);
        assert_eq!(after.revision, before.revision);
        assert_eq!(after.lease, before.lease);
        assert!(
            publish_bound_job(
                &plan,
                &jobs,
                "p-test",
                task,
                &WorkId::from_str(work_id),
                1,
                template.now
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn legacy_staged_payload_is_never_reused_for_new_assignment() -> TestResult {
        let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)])?;
        let (jobs, _dir) = job_store()?;
        let snapshot = snapshot_of(&plan)?;
        let template = template()?;
        let node = &snapshot.nodes[0];
        let work_id =
            admission_work_id(&snapshot.id, &node.id, snapshot.revision).map_err(ctx("id"))?;
        let contract = contract_from_node(node, template.base_revision.clone(), snapshot.revision);
        let mut payload = node_payload(snapshot.id.as_str(), node, snapshot.revision, 0, &contract)
            .map_err(ctx("payload"))?;
        payload
            .as_object_mut()
            .ok_or(TestError::Missing("object"))?
            .remove("assignment_attempt");
        ensure_job_admitted(
            &jobs,
            &template,
            &work_id,
            payload,
            snapshot.id.as_str(),
            &node.id,
        )
        .map_err(ctx("legacy stage"))?;
        assert!(PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template, "runtime").is_err());
        assert_eq!(plan.revision(), snapshot.revision);
        assert_eq!(
            jobs.get(&work_id).map_err(ctx("get"))?.job.state,
            JobState::Pending
        );
        Ok(())
    }

    #[test]
    fn staged_bound_admission_is_recovered_after_file_store_restart() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let (jobs, _jobs_dir) = job_store()?;
        let template = template()?;
        {
            let plan = harw_plan::FilePlanStore::new(dir.path()).map_err(ctx("file"))?;
            plan.apply(
                PlanAction::Create {
                    plan_id: plan_id("p-test")?,
                    goal: "test".to_owned(),
                },
                "test",
            )
            .map_err(ctx("create"))?;
            plan.apply(
                PlanAction::AddNode {
                    node: assigned_node(PlanNodeStatus::Draft),
                },
                "test",
            )
            .map_err(ctx("add"))?;
            for status in [PlanNodeStatus::Ready, PlanNodeStatus::InProgress] {
                plan.apply(
                    PlanAction::SetStatus {
                        id: TaskId::new("t-1"),
                        status,
                        reason: None,
                    },
                    "test",
                )
                .map_err(ctx("status"))?;
            }
            let snapshot = snapshot_of(&plan)?;
            let node = &snapshot.nodes[0];
            let contract =
                contract_from_node(node, template.base_revision.clone(), snapshot.revision);
            let payload = node_payload("p-test", node, snapshot.revision, 1, &contract)
                .map_err(ctx("payload"))?;
            ensure_job_admitted(
                &jobs,
                &template,
                &WorkId::from_str("current-job"),
                payload,
                "p-test",
                &node.id,
            )
            .map_err(ctx("stage"))?;
        }
        let plan = harw_plan::FilePlanStore::new(dir.path()).map_err(ctx("restart"))?;
        let revision = plan.revision();
        assert_eq!(
            PlanJobBridge::recover_pending_admissions(&plan, &jobs, template.now)
                .map_err(ctx("recover"))?
                .len(),
            1
        );
        assert!(
            PlanJobBridge::recover_pending_admissions(&plan, &jobs, template.now)
                .map_err(ctx("recover twice"))?
                .is_empty()
        );
        assert_eq!(plan.revision(), revision);
        assert_eq!(
            jobs.get(&WorkId::from_str("current-job"))
                .map_err(ctx("get"))?
                .job
                .state,
            JobState::Ready
        );
        Ok(())
    }
    #[test]
    fn recovery_never_publishes_legacy_stale_or_changed_payloads() -> TestResult {
        for field in ["assignment_attempt", "objective", "contract"] {
            let plan = seeded_plan_store(vec![assigned_node(PlanNodeStatus::InProgress)])?;
            let snapshot = snapshot_of(&plan)?;
            let node = &snapshot.nodes[0];
            let (jobs, _dir) = job_store()?;
            let template = template()?;
            let contract =
                contract_from_node(node, template.base_revision.clone(), snapshot.revision);
            let mut payload = node_payload("p-test", node, snapshot.revision, 1, &contract)
                .map_err(ctx("payload"))?;
            payload
                .as_object_mut()
                .ok_or(TestError::Missing("object"))?
                .remove(field);
            let id = WorkId::from_str("current-job");
            ensure_job_admitted(&jobs, &template, &id, payload, "p-test", &node.id)
                .map_err(ctx("stage"))?;
            assert!(
                PlanJobBridge::recover_pending_admissions(&plan, &jobs, template.now)
                    .map_err(ctx("recover"))?
                    .is_empty()
            );
            assert_eq!(
                jobs.get(&id).map_err(ctx("get"))?.job.state,
                JobState::Pending
            );
        }
        Ok(())
    }
    #[test]
    fn unbound_pending_legacy_owner_default_preserves_admission() -> TestResult {
        for owner in [
            None,
            Some(serde_json::Value::Null),
            Some(
                json!({"kind": "work_driver_prerequisite", "parent_work_id": "parent", "parent_lease_epoch": 1}),
            ),
        ] {
            let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)])?;
            let (jobs, _dir) = job_store()?;
            let snapshot = snapshot_of(&plan)?;
            let node = &snapshot.nodes[0];
            let template = template()?;
            let id =
                admission_work_id(&snapshot.id, &node.id, snapshot.revision).map_err(ctx("id"))?;
            let contract =
                contract_from_node(node, template.base_revision.clone(), snapshot.revision);
            let mut payload = node_payload("p-test", node, snapshot.revision, 0, &contract)
                .map_err(ctx("payload"))?;
            let should_reuse = owner.is_none();
            let object = payload
                .as_object_mut()
                .ok_or(TestError::Missing("object"))?;
            object.remove("dispatch_owner");
            if let Some(owner) = owner {
                object.insert("dispatch_owner".into(), owner);
            }
            ensure_job_admitted(&jobs, &template, &id, payload, "p-test", &node.id)
                .map_err(ctx("stage"))?;
            let admitted = PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template, "runtime");
            assert_eq!(
                admitted.is_ok(),
                should_reuse,
                "only absent legacy owner may normalize"
            );
            assert_eq!(
                jobs.get(&id).map_err(ctx("get"))?.job.state,
                if should_reuse {
                    JobState::Ready
                } else {
                    JobState::Pending
                }
            );
        }
        Ok(())
    }

    #[test]
    fn staged_reuse_rejects_changed_authority_budget_or_retry() -> TestResult {
        for field in ["scope", "budget", "retry"] {
            let plan = seeded_plan_store(vec![coding_node("t-1", PlanNodeStatus::Ready)])?;
            let (jobs, _dir) = job_store()?;
            let snapshot = snapshot_of(&plan)?;
            let node = &snapshot.nodes[0];
            let original = template()?;
            let id =
                admission_work_id(&snapshot.id, &node.id, snapshot.revision).map_err(ctx("id"))?;
            let contract =
                contract_from_node(node, original.base_revision.clone(), snapshot.revision);
            let mut payload = node_payload("p-test", node, snapshot.revision, 0, &contract)
                .map_err(ctx("payload"))?;
            payload
                .as_object_mut()
                .ok_or(TestError::Missing("object"))?
                .remove("dispatch_owner");
            ensure_job_admitted(&jobs, &original, &id, payload, "p-test", &node.id)
                .map_err(ctx("stage"))?;
            let mut changed = template()?;
            match field {
                "scope" => {
                    changed.scope = JobScope::new(
                        harw_types::TenantId::from_str("different"),
                        harw_types::WorkspaceId::from_str("workspace"),
                        ApprovalActor::Operator {
                            id: "different-operator".to_owned(),
                        },
                    )
                }
                "budget" => changed.budget.max_tokens = Some(1),
                _ => changed.retry.max_attempts = 2,
            }
            assert!(
                PlanJobBridge::admit_ready_nodes(&plan, &jobs, &changed, "runtime").is_err(),
                "changed {field} must not reuse staged authority"
            );
            assert_eq!(plan.revision(), snapshot.revision);
            let record = jobs.get(&id).map_err(ctx("get"))?;
            assert_eq!(record.job.state, JobState::Pending);
            assert_eq!(record.scope, original.scope);
            assert_eq!(record.job.budget, original.budget);
            assert_eq!(record.job.retry, original.retry);
        }
        Ok(())
    }
}
