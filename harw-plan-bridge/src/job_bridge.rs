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

use crate::controller::{apply_atomically, find_node};
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
    /// Je Knoten in drei Schritten (G-038, K4/K5):
    /// 1. Ein `Draft`-Knoten wird **vor** dem Anlegen eines Jobs atomar auf
    ///    `Ready` gesetzt. Damit prüft `harw-plan` Exploration (Regel 12),
    ///    Abhängigkeiten und Schreibbereich, bevor ein Job existiert — ein
    ///    abgelehnter Knoten hinterlässt keinen Waisen-Job.
    /// 2. Der Job wird mit einer **deterministischen** `WorkId` aus
    ///    `(Plan, Knoten, Plan-Revision)` angelegt. Existiert dieser Job schon
    ///    (ein früherer Lauf scheiterte nach dem Anlegen), wird er
    ///    wiederverwendet, sofern er noch `Pending`/`Ready` ist und zu Plan
    ///    und Knoten gehört. Pro Knoten und Revision entsteht so höchstens ein
    ///    Job.
    /// 3. `UpdateNode(assignment)` und `SetStatus(InProgress)` laufen als
    ///    **ein** `apply_batch` (bei Revisionskonflikt einmal neu gelesen).
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
    /// Plan-Store hinweg: scheitert Schritt 3, bleibt der Job `Ready` im
    /// Job-Store und der Knoten `Ready`. Ein erneuter Aufruf auf derselben
    /// Plan-Revision bindet denselben Job; hat sich die Revision inzwischen
    /// geändert, bleibt der alte Job ein Waise, den der Aufrufer stornieren
    /// muss.
    pub fn admit_ready_nodes(
        plan: &dyn PlanStore,
        jobs: &JobStore,
        template: &JobAdmissionTemplate,
        actor: &str,
    ) -> Result<Vec<(TaskId, String)>, PlanBridgeError> {
        let snapshot = plan.current()?;
        let candidates: Vec<TaskId> = graph::ready_nodes(&snapshot)
            .into_iter()
            .filter(|node| JOB_KINDS.contains(&node.kind))
            .map(|node| node.id.clone())
            .collect();

        if candidates.is_empty() {
            return Err(PlanBridgeError::NoReadyNodes);
        }

        let mut admitted: Vec<(TaskId, String)> = Vec::with_capacity(candidates.len());
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
    /// Ist der Knoten bereits `Completed`, ist der Rückkanal schon wirksam
    /// und die Methode kehrt ohne Mutation mit `Ok(())` zurück.
    ///
    /// # Concurrency
    /// Beide Aktionen laufen als **ein** `apply_batch` (G-038, K1): lehnt
    /// `harw-plan` den Abschluss ab, wird auch die Evidenz nicht angehängt.
    /// Bei Revisionskonflikt wird einmal neu gelesen.
    pub fn on_job_completed(
        plan: &dyn PlanStore,
        task: &TaskId,
        work_id: &str,
        summary: &str,
        actor: &str,
        now: OffsetDateTime,
    ) -> Result<(), PlanBridgeError> {
        ensure_node_exists(plan, task)?;

        let events = apply_atomically(plan, actor, |snapshot| {
            if find_node(snapshot, task)
                .is_some_and(|node| node.status == PlanNodeStatus::Completed)
            {
                return Vec::new();
            }
            vec![
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
            ]
        })?;
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
    /// Ist der Knoten bereits `Invalidated`, kehrt die Methode ohne Mutation
    /// mit `Ok(())` zurück (ein doppelt gemeldeter Fehlschlag ist kein Fehler).
    ///
    /// # Concurrency
    /// Ein einzelner atomarer Batch; thread-sicher.
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
        apply_atomically(plan, actor, |snapshot| {
            if find_node(snapshot, task)
                .is_some_and(|node| node.status == PlanNodeStatus::Invalidated)
            {
                return Vec::new();
            }
            vec![PlanAction::Invalidate {
                ids: vec![task.clone()],
                condition: InvalidationCondition::ManualInvalidate,
            }]
        })?;
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
    let payload = node_payload(snapshot.id.as_str(), node, revision, &contract)?;
    ensure_job_admitted(
        jobs,
        template,
        &work_id,
        payload,
        snapshot.id.as_str(),
        task,
    )?;

    // Schritt 3: Bindung und Start atomar.
    let attempt = node
        .assignment
        .as_ref()
        .map_or(0, |assignment| assignment.attempt.saturating_add(1));
    let events = apply_atomically(plan, actor, |current| match job_candidate(current, task) {
        Some(candidate) if candidate.status == PlanNodeStatus::Ready => vec![
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
        ],
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

    tracing::info!(
        task = %task,
        work_id = work_id.as_str(),
        attempt = attempt,
        "Plan-Knoten als Job admittiert"
    );
    Ok(Some(work_id.as_str().to_owned()))
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
            let reusable = matches!(existing.job.state, JobState::Pending | JobState::Ready);
            if belongs && reusable {
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

        PlanJobBridge::on_job_failed(&plan, task, work_id, "clippy rot", "runtime")
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

        PlanJobBridge::on_job_failed_observed(&plan, task, work_id, "clippy rot", "runtime", None)
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
    fn test_admit_ready_nodes_retries_once_after_a_revision_conflict() -> TestResult {
        let plan = ScriptedPlanStore::new(seeded_plan_store(vec![coding_node(
            "t-1",
            PlanNodeStatus::Ready,
        )])?);
        let (jobs, _dir) = job_store()?;
        plan.inject_conflicts(1);

        let admitted = PlanJobBridge::admit_ready_nodes(&plan, &jobs, &template()?, "runtime")
            .map_err(ctx("admit schlug fehl"))?;

        assert_eq!(admitted.len(), 1);
        assert_eq!(plan.batch_calls(), 2, "erwartet genau eine Wiederholung");
        assert_eq!(job_count(&jobs)?, 1);
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
            "fertig",
            "runtime",
            OffsetDateTime::UNIX_EPOCH,
        ) {
            Err(PlanBridgeError::Plan(PlanError::BatchActionRejected { index, .. })) => {
                assert_eq!(index, 1, "der Statuswechsel muss scheitern");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet BatchActionRejected, bekommen: {other:?}"
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
        for round in 0..2 {
            PlanJobBridge::on_job_failed(&plan, &TaskId::new("t-1"), "work-x", "rot", "runtime")
                .map_err(|error| {
                    TestError::Unexpected(format!("on_job_failed Runde {round}: {error}"))
                })?;
        }
        assert_eq!(
            snapshot_of(&plan)?.nodes[0].status,
            PlanNodeStatus::Invalidated
        );
        Ok(())
    }
}
