//! Kanban-Job-Ledger über den durablen, gefencten [`JobStore`].
//!
//! # Beschreibung
//! [`JobStoreTransitions`] implementiert
//! [`harw_knowledge::kanban::lifecycle::JobTransitions`] gegen
//! `harw_session_store::JobStore` (Job + Lease + Retry aus
//! `harw-job-runtime`). Damit laufen Kartenbewegungen von `/kanban` über
//! denselben governten, dateibasierten Ledger wie alle anderen Jobs, statt
//! über das nicht-durable `InMemoryJobTransitions`.
//!
//! # Abbildung der Übergänge
//! | Trait-Methode | `JobStore` |
//! |---|---|
//! | `snapshot` | `get` (`JobNotFound` → `None`) |
//! | `create` | `admit` (neuer Job `Pending`, Art [`KANBAN_JOB_KIND`]) |
//! | `mark_ready` | `mark_ready` (`Pending → Ready`, idempotent für `Ready`) |
//! | `claim` | `claim` (Lease mit [`DEFAULT_KANBAN_LEASE_TTL`]) |
//! | `complete` | `complete` mit `JobOutcome::Succeeded` und dem Token der gespeicherten Lease |
//! | `block` | `complete` mit `JobOutcome::Blocked { reason: "kanban:<BlockKind>" }` |
//! | `unblock` | `unblock` (`Blocked`) bzw. `retry` (`Failed`) |
//! | `approve` | `unblock` mit Freigabe-Notiz [`KANBAN_APPROVAL_NOTE_PREFIX`] (nur `Blocked`) |
//! | `reclaim` | `reclaim` (`Running → Ready`, zählt einen Versuch; bei erschöpfter Retry-Politik `Failed`) |
//! | `cancel` | `cancel` (`Pending|Ready|Running`) bzw. `deny_blocked` (`Blocked`) |
//!
//! `reclaim` verlangt keine abgelaufene Lease: der Store entzieht die Lease
//! des Halters, schaltet die Fencing-Epoch weiter und zählt über
//! `record_failure` einen Versuch — dieselben Schritte, die der Supervisor
//! über `reconcile_expired` für abgelaufene Leases ausführt. Ist die
//! Retry-Politik erschöpft, steht der Job danach `Failed`, und die Karte
//! zeigt ihn über `snapshot` als blockiert.
//!
//! # Abgrenzung zu anderen Job-Arten
//! Kanban-Jobs tragen `JobKind::Custom(`[`KANBAN_JOB_KIND`]`)`. Der
//! CLI-Job-Worker (`harw-cli/src/job_worker.rs`) beansprucht sie seit Plan
//! D2 ebenfalls — aber nur mit einer Freigabe des Operators
//! ([`is_kanban_approval`]); ohne sie blockiert er den Job mit
//! `kanban:AwaitingApproval`, ohne einen Agenten zu starten. Umgekehrt
//! mutiert dieser Adapter nur Jobs seiner eigenen Art; eine Karte, deren
//! `work_id` auf einen fremden Job zeigt, darf dessen Zustand lesen
//! (`snapshot`), aber nie bewegen (fail closed, `PermissionDenied`).
//! `complete`/`block` verwenden das Token der gespeicherten Lease — auch
//! über einen Prozessneustart hinweg; hält der Worker die Lease, geht das
//! Beenden über ihn (die Lease wird dann laufend verlängert).
//!
//! # Freigabe (Plan D2)
//! `approve` ist `JobStore::unblock` mit einer Notiz, die mit
//! [`KANBAN_APPROVAL_NOTE_PREFIX`] beginnt. Der Freigabe-Sidecar
//! (`JobApproval`) trägt die Revision, die das Freigeben erzeugt hat; der
//! Worker startet nur, wenn diese Revision der aktuellen entspricht, die
//! Notiz das Präfix trägt und ein Operator freigegeben hat. Ein bloßes
//! `unblock` (`/kanban unblock`, `/approve`) ist keine Kanban-Freigabe.
//!
//! # Sperrgrund
//! `JobState::Blocked` trägt keinen Grund. Der [`BlockKind`] wird im
//! `JobOutcome::Blocked`-Grund als `kanban:<Label>` abgelegt und von
//! `snapshot` wieder gelesen; ein fremder oder fehlender Grund ergibt
//! `None` (die Karte zeigt dann die Vorgabe aus `CardState::from_job_state`).
//!
//! # Nebenläufigkeit
//! `Send + Sync`, zustandslos bis auf den geteilten `Arc<JobStore>`. Jeder
//! Übergang ist genau ein Store-Aufruf unter dessen Datensatz-Sperre; die
//! vorherige Zustandsprüfung ist nur eine Auswahl des passenden Aufrufs —
//! der Store prüft den Zustand unter der Sperre erneut.
//!
//! # Fehler
//! Jeder Pfad liefert [`KnowledgeResult`]; die Abbildung der
//! [`SessionStoreError`]-Varianten steht an [`map_store_error`].

use std::io::ErrorKind;
use std::sync::Arc;

use harw_job_runtime::{
    Budget, Job, JobKind, JobOutcome, JobRuntimeError, JobScope, JobState, RetryPolicy, StoredJob,
    WorkId,
};
use harw_knowledge::kanban::board::{BlockKind, CardRecord, JobSnapshot};
use harw_knowledge::kanban::lifecycle::JobTransitions;
use harw_knowledge::{AgentId, KnowledgeError, KnowledgeResult};
use harw_session_store::{
    CancelRequest, ClaimRequest, CompleteRequest, JobApproval, JobStore, RetryRequest,
    SessionStoreError,
};
use harw_types::ApprovalActor;
use jiff::{SignedDuration, Timestamp};

/// `JobKind::Custom`-Diskriminator aller vom Kanban angelegten Jobs.
pub const KANBAN_JOB_KIND: &str = "kanban_card";

/// Präfix des `JobOutcome::Blocked`-Grunds, hinter dem das [`BlockKind`]-Label steht.
pub const KANBAN_BLOCK_REASON_PREFIX: &str = "kanban:";

/// Präfix der Freigabe-Notiz, an der der Worker eine Kanban-Freigabe erkennt.
pub const KANBAN_APPROVAL_NOTE_PREFIX: &str = "kanban-freigabe:";

/// `true`, wenn `approval` eine gültige Kanban-Freigabe für den Job in der
/// Revision `current_revision` ist (siehe Moduldoku „Freigabe").
///
/// # Argumente
/// - `approval` (`&JobApproval`): der Freigabe-Sidecar des Jobs.
/// - `current_revision` (`u64`): die Revision des Jobs, wie der Worker ihn
///   gerade gelesen hat (`Ready`).
#[must_use]
pub fn is_kanban_approval(approval: &JobApproval, current_revision: u64) -> bool {
    approval.revision == current_revision
        && matches!(approval.approved_by, ApprovalActor::Operator { .. })
        && approval
            .note
            .as_deref()
            .is_some_and(|note| note.starts_with(KANBAN_APPROVAL_NOTE_PREFIX))
}

/// `true` für die Job-Art, die der Kanban-Adapter anlegt.
#[must_use]
pub fn is_kanban_job_kind(kind: &JobKind) -> bool {
    is_kanban_kind(kind)
}

/// Lease-Dauer eines Kanban-Claims (24 h): Karten sind langlebige Arbeit,
/// die nicht an einem Worker-Heartbeat hängt.
pub const DEFAULT_KANBAN_LEASE_TTL: SignedDuration = SignedDuration::from_hours(24);

/// Anzahl der Versuche, die `retry` (Failed → Ready) einem Kanban-Job gewährt.
const KANBAN_MAX_ATTEMPTS: u32 = 3;

/// [`JobTransitions`] über den durablen [`JobStore`].
///
/// # Beschreibung
/// Siehe Moduldoku für die Abbildung je Übergang. `scope` ist die
/// Autoritätsgrenze jedes neu zugelassenen Jobs; sein `submitter` ist
/// zugleich der Akteur für `unblock`/`retry`/`cancel`.
pub struct JobStoreTransitions {
    store: Arc<JobStore>,
    scope: JobScope,
    lease_ttl: SignedDuration,
}

impl std::fmt::Debug for JobStoreTransitions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobStoreTransitions")
            .field("root", &self.store.root())
            .field("scope", &self.scope)
            .field("lease_ttl", &self.lease_ttl)
            .finish()
    }
}

impl JobStoreTransitions {
    /// Baut den Adapter.
    ///
    /// # Argumente
    /// - `store` (`Arc<JobStore>`): der geteilte, durable Job-Speicher.
    /// - `scope` ([`JobScope`]): Autoritätsgrenze neuer Kanban-Jobs; der
    ///   `submitter` handelt auch bei `unblock`/`retry`/`cancel`.
    #[must_use]
    pub fn new(store: Arc<JobStore>, scope: JobScope) -> Self {
        Self {
            store,
            scope,
            lease_ttl: DEFAULT_KANBAN_LEASE_TTL,
        }
    }

    /// Setzt eine abweichende Lease-Dauer für `claim` (muss positiv sein,
    /// sonst lehnt der Store jeden Claim ab).
    #[must_use]
    pub fn with_lease_ttl(mut self, lease_ttl: SignedDuration) -> Self {
        self.lease_ttl = lease_ttl;
        self
    }

    /// Der Akteur für Freigabe-, Retry- und Abbruchübergänge.
    fn actor(&self) -> ApprovalActor {
        self.scope.submitter().clone()
    }

    /// Liest den Datensatz eines Jobs, den dieser Adapter bewegen darf.
    ///
    /// # Fehler
    /// `ArtifactNotFound` bei unbekanntem Job, `PermissionDenied` bei einem
    /// Job fremder Art, sonst die Abbildung aus [`map_store_error`].
    fn owned_record(&self, work_id: &WorkId) -> KnowledgeResult<StoredJob> {
        let record = self.store.get(work_id).map_err(map_store_error)?;
        if !is_kanban_kind(&record.job.kind) {
            return Err(ledger_error(
                ErrorKind::PermissionDenied,
                format!(
                    "Job {work_id} gehört nicht zum Kanban-Ledger (Art {:?}); Übergang abgelehnt",
                    record.job.kind
                ),
            ));
        }
        Ok(record)
    }

    /// Beendet einen laufenden Kanban-Job mit `outcome` über das Token der
    /// gespeicherten Lease.
    fn finish(&self, work_id: &WorkId, outcome: JobOutcome, to: &str) -> KnowledgeResult<()> {
        let record = self.owned_record(work_id)?;
        if record.job.state != JobState::Running {
            return Err(illegal(record.job.state, to));
        }
        let Some(lease) = record.lease else {
            return Err(ledger_error(
                ErrorKind::InvalidData,
                format!("Job {work_id} läuft laut Ledger, trägt aber keine Lease"),
            ));
        };
        self.store
            .complete(
                work_id,
                &CompleteRequest {
                    token: lease.token(),
                    completed_at: Timestamp::now(),
                    outcome,
                },
            )
            .map_err(map_store_error)?;
        Ok(())
    }
}

impl JobTransitions for JobStoreTransitions {
    fn snapshot(&self, work_id: &WorkId) -> KnowledgeResult<Option<JobSnapshot>> {
        let record = match self.store.get(work_id) {
            Ok(record) => record,
            Err(SessionStoreError::JobNotFound { .. }) => return Ok(None),
            Err(error) => return Err(map_store_error(error)),
        };
        Ok(Some(snapshot_of(&record)))
    }

    fn create(&self, card: &CardRecord) -> KnowledgeResult<WorkId> {
        let now = Timestamp::now();
        let job = Job::new(
            WorkId::new(),
            JobKind::Custom(KANBAN_JOB_KIND.to_owned()),
            Budget::unbounded(),
            RetryPolicy {
                max_attempts: KANBAN_MAX_ATTEMPTS,
                base_delay: SignedDuration::ZERO,
                factor: 1.0,
                max_delay: SignedDuration::ZERO,
            },
            now,
        );
        let work_id = job.id.clone();
        let record = StoredJob {
            job,
            scope: self.scope.clone(),
            input: serde_json::json!({
                "kanban_card": card.id.as_str(),
                "lane": card.lane_id.as_str(),
                "title": card.title,
            }),
            submitted_at: now,
            not_before: now,
            lease: None,
            lease_epoch: 0,
            completion: None,
            cancellation: None,
            revision: 0,
            // Die Kanban-Naht trägt keinen Trace-Kontext.
            trace: None,
        };
        self.store.admit(&record).map_err(map_store_error)?;
        Ok(work_id)
    }

    fn mark_ready(&self, work_id: &WorkId) -> KnowledgeResult<()> {
        let record = self.owned_record(work_id)?;
        match record.job.state {
            JobState::Ready => Ok(()),
            JobState::Pending => {
                self.store
                    .mark_ready(work_id, Timestamp::now())
                    .map_err(map_store_error)?;
                Ok(())
            }
            other => Err(illegal(other, "ready")),
        }
    }

    fn claim(&self, work_id: &WorkId, holder: &AgentId) -> KnowledgeResult<()> {
        self.owned_record(work_id)?;
        self.store
            .claim(
                work_id,
                &ClaimRequest {
                    worker_id: holder.as_str().to_owned(),
                    lease_ttl: self.lease_ttl,
                    now: Timestamp::now(),
                },
            )
            .map_err(map_store_error)?;
        Ok(())
    }

    fn complete(&self, work_id: &WorkId) -> KnowledgeResult<()> {
        self.finish(
            work_id,
            JobOutcome::Succeeded {
                result: serde_json::json!({ "source": "kanban" }),
            },
            "completed",
        )
    }

    fn block(&self, work_id: &WorkId, reason: BlockKind) -> KnowledgeResult<()> {
        self.finish(
            work_id,
            JobOutcome::Blocked {
                reason: format!("{KANBAN_BLOCK_REASON_PREFIX}{}", reason.label()),
            },
            "blocked",
        )
    }

    fn unblock(&self, work_id: &WorkId) -> KnowledgeResult<()> {
        let record = self.owned_record(work_id)?;
        let now = Timestamp::now();
        match record.job.state {
            JobState::Blocked => {
                self.store
                    .unblock(
                        work_id,
                        now,
                        self.actor(),
                        Some("Kanban: Karte freigegeben".to_owned()),
                    )
                    .map_err(map_store_error)?;
                Ok(())
            }
            JobState::Failed => {
                self.store
                    .retry(
                        work_id,
                        &RetryRequest {
                            retried_at: now,
                            retried_by: self.actor(),
                        },
                    )
                    .map_err(map_store_error)?;
                Ok(())
            }
            other => Err(illegal(other, "ready")),
        }
    }

    fn approve(
        &self,
        work_id: &WorkId,
        approved_by: &AgentId,
        note: Option<&str>,
    ) -> KnowledgeResult<()> {
        let record = self.owned_record(work_id)?;
        if record.job.state != JobState::Blocked {
            return Err(illegal(record.job.state, "ready (approve)"));
        }
        let note = match note.map(str::trim).filter(|note| !note.is_empty()) {
            Some(note) => format!("{KANBAN_APPROVAL_NOTE_PREFIX} {approved_by}: {note}"),
            None => format!("{KANBAN_APPROVAL_NOTE_PREFIX} {approved_by}"),
        };
        self.store
            .unblock(work_id, Timestamp::now(), self.actor(), Some(note))
            .map_err(map_store_error)?;
        Ok(())
    }

    fn reclaim(&self, work_id: &WorkId) -> KnowledgeResult<()> {
        let record = self.owned_record(work_id)?;
        if record.job.state != JobState::Running {
            return Err(illegal(record.job.state, "ready (reclaim)"));
        }
        self.store
            .reclaim(work_id, Timestamp::now(), self.actor())
            .map_err(map_store_error)?;
        Ok(())
    }

    fn cancel(&self, work_id: &WorkId) -> KnowledgeResult<()> {
        let record = self.owned_record(work_id)?;
        let request = CancelRequest {
            cancelled_at: Timestamp::now(),
            cancelled_by: self.actor(),
            reason: "Kanban: Karte abgebrochen".to_owned(),
        };
        match record.job.state {
            JobState::Cancelled => Ok(()),
            JobState::Pending | JobState::Ready | JobState::Running => {
                self.store
                    .cancel(work_id, &request)
                    .map_err(map_store_error)?;
                Ok(())
            }
            JobState::Blocked => {
                self.store
                    .deny_blocked(work_id, &request)
                    .map_err(map_store_error)?;
                Ok(())
            }
            JobState::Failed => Err(ledger_error(
                ErrorKind::Unsupported,
                format!(
                    "Job {work_id}: der JobStore kann einen fehlgeschlagenen Job nicht abbrechen"
                ),
            )),
            JobState::Completed => Err(illegal(JobState::Completed, "cancelled")),
        }
    }
}

/// `true` für die Job-Art, die dieser Adapter anlegt und bewegen darf.
fn is_kanban_kind(kind: &JobKind) -> bool {
    matches!(kind, JobKind::Custom(name) if name == KANBAN_JOB_KIND)
}

/// Projiziert einen gespeicherten Job auf die Ledger-Sicht der Karte.
fn snapshot_of(record: &StoredJob) -> JobSnapshot {
    let block_reason = match record.completion.as_ref().map(|done| &done.outcome) {
        Some(JobOutcome::Blocked { reason }) => reason
            .strip_prefix(KANBAN_BLOCK_REASON_PREFIX)
            .and_then(BlockKind::parse),
        _ => None,
    };
    JobSnapshot {
        state: record.job.state,
        holder: record
            .lease
            .as_ref()
            .map(|lease| AgentId::new(lease.holder.clone())),
        attempts: record.job.attempts,
        block_reason,
    }
}

/// Kurzname eines Job-Zustands für Fehlermeldungen.
fn state_label(state: JobState) -> String {
    format!("{state:?}").to_lowercase()
}

/// Ein unzulässiger Übergang aus `from` nach `to`.
fn illegal(from: JobState, to: &str) -> KnowledgeError {
    KnowledgeError::IllegalTransition {
        from: state_label(from),
        to: to.to_owned(),
    }
}

/// Ein Ledger-Fehler mit deutscher Meldung.
fn ledger_error(kind: ErrorKind, message: String) -> KnowledgeError {
    KnowledgeError::Io(std::io::Error::new(kind, message))
}

/// Übersetzt [`SessionStoreError`] in [`KnowledgeError`].
///
/// # Beschreibung
/// - `JobNotFound` → `ArtifactNotFound("job <id>")` (wie die Referenz).
/// - Zustandskonflikte → `IllegalTransition` (Aufruferfehler).
/// - `JobRetryLimitExhausted` → `Job(RetryExhausted)`,
///   `JobLeaseExpired` → `Job(LeaseExpired)`.
/// - `Io` bleibt `Io`; alles andere wird ein `Io`-Fehler mit Kontext.
fn map_store_error(error: SessionStoreError) -> KnowledgeError {
    match error {
        SessionStoreError::JobNotFound { work_id } => {
            KnowledgeError::ArtifactNotFound(format!("job {work_id}"))
        }
        SessionStoreError::JobNotClaimable { state, .. } => illegal(state, "running"),
        SessionStoreError::JobNotCancellable { state, .. }
        | SessionStoreError::JobNotDeniable { state, .. } => illegal(state, "cancelled"),
        SessionStoreError::JobAlreadyTerminal { state, .. } => illegal(state, "completed"),
        SessionStoreError::JobNotBlocked { state, .. }
        | SessionStoreError::JobNotRetryable { state, .. } => illegal(state, "ready"),
        SessionStoreError::JobRetryLimitExhausted { attempts, .. } => {
            KnowledgeError::Job(JobRuntimeError::RetryExhausted { attempts })
        }
        SessionStoreError::JobLeaseExpired {
            work_id,
            expired_at,
        } => KnowledgeError::Job(JobRuntimeError::LeaseExpired {
            work_id,
            expired_at,
        }),
        SessionStoreError::Io(io) => KnowledgeError::Io(io),
        other => ledger_error(ErrorKind::Other, format!("Job-Ledger: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_knowledge::VisibilityScope;
    use harw_knowledge::kanban::board::{CardId, LaneId};
    use harw_knowledge::kanban::lifecycle::InMemoryJobTransitions;
    use harw_types::{TenantId, WorkspaceId};

    fn scope() -> JobScope {
        JobScope::new(
            TenantId::from_str("local"),
            WorkspaceId::from_str("kanban"),
            ApprovalActor::Operator {
                id: "local-tui".to_owned(),
            },
        )
    }

    fn card() -> CardRecord {
        CardRecord {
            id: CardId::new("card-1"),
            lane_id: LaneId::new("lane-1"),
            title: "Test".to_owned(),
            body: String::new(),
            work_id: None,
            parents: Vec::new(),
            tags: Vec::new(),
            visibility: VisibilityScope::SelfOnly,
        }
    }

    fn ledger() -> TestResult<(JobStoreTransitions, Arc<JobStore>, tempfile::TempDir)> {
        let dir = tempfile::tempdir()?;
        let store = Arc::new(JobStore::new(dir.path()));
        Ok((
            JobStoreTransitions::new(Arc::clone(&store), scope()),
            store,
            dir,
        ))
    }

    /// Legt einen Job beliebiger Art direkt im Zustand `Ready` an (für
    /// fremde Arten, die der Adapter nicht anlegen kann).
    fn admit_ready(store: &JobStore, kind: JobKind) -> TestResult<WorkId> {
        let now = Timestamp::now();
        let mut job = Job::new(
            WorkId::new(),
            kind,
            Budget::unbounded(),
            RetryPolicy {
                max_attempts: KANBAN_MAX_ATTEMPTS,
                base_delay: SignedDuration::ZERO,
                factor: 1.0,
                max_delay: SignedDuration::ZERO,
            },
            now,
        );
        job.mark_ready(now).map_err(ctx("mark ready"))?;
        let work_id = job.id.clone();
        store
            .admit(&StoredJob {
                job,
                scope: scope(),
                input: serde_json::json!({}),
                submitted_at: now,
                not_before: now,
                lease: None,
                lease_epoch: 0,
                completion: None,
                cancellation: None,
                revision: 0,
                trace: None,
            })
            .map_err(ctx("admit"))?;
        Ok(work_id)
    }

    fn state_of(ledger: &JobStoreTransitions, work_id: &WorkId) -> TestResult<JobSnapshot> {
        ledger
            .snapshot(work_id)
            .map_err(ctx("snapshot"))?
            .ok_or(TestError::Missing("snapshot"))
    }

    fn kanban() -> JobKind {
        JobKind::Custom(KANBAN_JOB_KIND.to_owned())
    }

    #[test]
    fn the_adapter_is_usable_as_a_shared_trait_object() -> TestResult {
        let (ledger, _store, _dir) = ledger()?;
        let shared: Arc<dyn JobTransitions> = Arc::new(ledger);
        assert!(
            shared
                .snapshot(&WorkId::new())
                .map_err(ctx("snapshot"))?
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn create_admits_a_pending_kanban_job() -> TestResult {
        let (ledger, store, _dir) = ledger()?;
        let work_id = ledger.create(&card()).map_err(ctx("create"))?;
        let stored = store.get(&work_id).map_err(ctx("get"))?;
        assert_eq!(stored.job.state, JobState::Pending);
        assert_eq!(stored.job.kind, kanban());
        assert_eq!(stored.input["kanban_card"], "card-1");
        assert_eq!(state_of(&ledger, &work_id)?.state, JobState::Pending);
        Ok(())
    }

    #[test]
    fn mark_ready_moves_a_created_job_to_ready_and_is_idempotent() -> TestResult {
        let (ledger, store, _dir) = ledger()?;
        let work_id = ledger.create(&card()).map_err(ctx("create"))?;
        ledger.mark_ready(&work_id).map_err(ctx("mark ready"))?;
        let stored = store.get(&work_id).map_err(ctx("get"))?;
        assert_eq!(stored.job.state, JobState::Ready);
        ledger
            .mark_ready(&work_id)
            .map_err(ctx("mark ready twice"))?;
        assert_eq!(
            store.get(&work_id).map_err(ctx("get again"))?.revision,
            stored.revision
        );
        ledger
            .claim(&work_id, &AgentId::new("worker-1"))
            .map_err(ctx("claim"))?;
        assert_eq!(state_of(&ledger, &work_id)?.state, JobState::Running);
        Ok(())
    }

    #[test]
    fn mark_ready_rejects_a_running_job() -> TestResult {
        let (ledger, _store, _dir) = ledger()?;
        let work_id = admit_ready(&ledger.store, kanban())?;
        ledger
            .claim(&work_id, &AgentId::new("worker-1"))
            .map_err(ctx("claim"))?;
        let Err(KnowledgeError::IllegalTransition { .. }) = ledger.mark_ready(&work_id) else {
            return Err(TestError::Unexpected(
                "Running → Ready über mark_ready muss abgelehnt werden".to_owned(),
            ));
        };
        assert_eq!(state_of(&ledger, &work_id)?.state, JobState::Running);
        Ok(())
    }

    #[test]
    fn claim_complete_round_trip_records_holder_and_done() -> TestResult {
        let (ledger, _store, _dir) = ledger()?;
        let work_id = admit_ready(&ledger.store, kanban())?;
        ledger
            .mark_ready(&work_id)
            .map_err(ctx("ready is idempotent"))?;
        ledger
            .claim(&work_id, &AgentId::new("worker-1"))
            .map_err(ctx("claim"))?;
        let running = state_of(&ledger, &work_id)?;
        assert_eq!(running.state, JobState::Running);
        assert_eq!(running.holder, Some(AgentId::new("worker-1")));

        ledger.complete(&work_id).map_err(ctx("complete"))?;
        let done = state_of(&ledger, &work_id)?;
        assert_eq!(done.state, JobState::Completed);
        assert_eq!(done.holder, None);

        let Err(KnowledgeError::IllegalTransition { .. }) = ledger.complete(&work_id) else {
            return Err(TestError::Unexpected(
                "ein zweites complete muss abgelehnt werden".to_owned(),
            ));
        };
        Ok(())
    }

    #[test]
    fn block_keeps_the_reason_and_unblock_returns_to_ready() -> TestResult {
        let (ledger, _store, _dir) = ledger()?;
        let work_id = admit_ready(&ledger.store, kanban())?;
        ledger
            .claim(&work_id, &AgentId::new("worker-1"))
            .map_err(ctx("claim"))?;
        ledger
            .block(&work_id, BlockKind::NeedsInput)
            .map_err(ctx("block"))?;
        let blocked = state_of(&ledger, &work_id)?;
        assert_eq!(blocked.state, JobState::Blocked);
        assert_eq!(blocked.block_reason, Some(BlockKind::NeedsInput));

        ledger.unblock(&work_id).map_err(ctx("unblock"))?;
        let ready = state_of(&ledger, &work_id)?;
        assert_eq!(ready.state, JobState::Ready);
        assert_eq!(ready.block_reason, None);
        Ok(())
    }

    #[test]
    fn approve_marks_a_kanban_approval_that_plain_unblock_does_not() -> TestResult {
        let (ledger, store, _dir) = ledger()?;
        let work_id = admit_ready(&ledger.store, kanban())?;
        ledger
            .claim(&work_id, &AgentId::new("worker-1"))
            .map_err(ctx("claim"))?;
        ledger
            .block(&work_id, BlockKind::AwaitingApproval)
            .map_err(ctx("block"))?;
        assert_eq!(
            state_of(&ledger, &work_id)?.block_reason,
            Some(BlockKind::AwaitingApproval)
        );
        ledger
            .approve(&work_id, &AgentId::new("operator"), Some("los"))
            .map_err(ctx("approve"))?;
        let stored = store.get(&work_id).map_err(ctx("get"))?;
        assert_eq!(stored.job.state, JobState::Ready);
        let approval = store
            .get_approval(&work_id)
            .map_err(ctx("approval"))?
            .ok_or(TestError::Missing("approval sidecar"))?;
        assert!(is_kanban_approval(&approval, stored.revision));
        assert!(!is_kanban_approval(&approval, stored.revision + 1));
        let Err(KnowledgeError::IllegalTransition { .. }) =
            ledger.approve(&work_id, &AgentId::new("operator"), None)
        else {
            return Err(TestError::Unexpected(
                "approve außerhalb von Blocked muss abgelehnt werden".to_owned(),
            ));
        };

        // Ein bloßes unblock ist keine Kanban-Freigabe.
        ledger
            .claim(&work_id, &AgentId::new("worker-1"))
            .map_err(ctx("claim again"))?;
        ledger
            .block(&work_id, BlockKind::NeedsInput)
            .map_err(ctx("block again"))?;
        ledger.unblock(&work_id).map_err(ctx("unblock"))?;
        let stored = store.get(&work_id).map_err(ctx("get again"))?;
        let approval = store
            .get_approval(&work_id)
            .map_err(ctx("approval again"))?
            .ok_or(TestError::Missing("approval sidecar"))?;
        assert!(!is_kanban_approval(&approval, stored.revision));
        Ok(())
    }

    #[test]
    fn cancel_covers_blocked_and_is_idempotent() -> TestResult {
        let (ledger, _store, _dir) = ledger()?;
        let work_id = admit_ready(&ledger.store, kanban())?;
        ledger
            .claim(&work_id, &AgentId::new("worker-1"))
            .map_err(ctx("claim"))?;
        ledger
            .block(&work_id, BlockKind::Dependency)
            .map_err(ctx("block"))?;
        ledger.cancel(&work_id).map_err(ctx("cancel blocked"))?;
        assert_eq!(state_of(&ledger, &work_id)?.state, JobState::Cancelled);
        ledger.cancel(&work_id).map_err(ctx("cancel twice"))?;

        let pending = ledger.create(&card()).map_err(ctx("create"))?;
        ledger.cancel(&pending).map_err(ctx("cancel pending"))?;
        assert_eq!(state_of(&ledger, &pending)?.state, JobState::Cancelled);
        Ok(())
    }

    #[test]
    fn reclaim_returns_a_running_job_to_ready_and_counts_an_attempt() -> TestResult {
        let (ledger, _store, _dir) = ledger()?;
        let work_id = ledger.create(&card()).map_err(ctx("create"))?;
        ledger.mark_ready(&work_id).map_err(ctx("mark ready"))?;
        ledger
            .claim(&work_id, &AgentId::new("worker-1"))
            .map_err(ctx("claim"))?;
        ledger.reclaim(&work_id).map_err(ctx("reclaim"))?;
        let ready = state_of(&ledger, &work_id)?;
        assert_eq!(ready.state, JobState::Ready);
        assert_eq!(ready.holder, None);
        assert_eq!(ready.attempts, 1);

        // Der Kanban-Backoff ist null: die Karte ist sofort wieder beanspruchbar.
        ledger
            .claim(&work_id, &AgentId::new("worker-2"))
            .map_err(ctx("claim again"))?;
        assert_eq!(
            state_of(&ledger, &work_id)?.holder,
            Some(AgentId::new("worker-2"))
        );
        Ok(())
    }

    #[test]
    fn reclaim_rejects_a_job_that_is_not_running() -> TestResult {
        let (ledger, _store, _dir) = ledger()?;
        let work_id = admit_ready(&ledger.store, kanban())?;
        let Err(KnowledgeError::IllegalTransition { .. }) = ledger.reclaim(&work_id) else {
            return Err(TestError::Unexpected(
                "reclaim eines Ready-Jobs muss abgelehnt werden".to_owned(),
            ));
        };
        assert_eq!(state_of(&ledger, &work_id)?.state, JobState::Ready);
        Ok(())
    }

    #[test]
    fn reclaim_and_mark_ready_never_move_a_foreign_job() -> TestResult {
        let (ledger, store, _dir) = ledger()?;
        let work_id = admit_ready(&ledger.store, JobKind::Worker)?;
        store
            .claim(
                &work_id,
                &ClaimRequest {
                    worker_id: "worker-1".to_owned(),
                    lease_ttl: SignedDuration::from_secs(60),
                    now: Timestamp::now(),
                },
            )
            .map_err(ctx("claim foreign"))?;
        for result in [ledger.reclaim(&work_id), ledger.mark_ready(&work_id)] {
            let Err(KnowledgeError::Io(error)) = result else {
                return Err(TestError::Unexpected(
                    "ein fremder Job darf nicht bewegt werden".to_owned(),
                ));
            };
            assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        }
        let stored = store.get(&work_id).map_err(ctx("get"))?;
        assert_eq!(stored.job.state, JobState::Running);
        assert_eq!(stored.job.attempts, 0);
        Ok(())
    }

    #[test]
    fn foreign_jobs_are_readable_but_never_moved() -> TestResult {
        let (ledger, _store, _dir) = ledger()?;
        let work_id = admit_ready(&ledger.store, JobKind::Worker)?;
        assert_eq!(state_of(&ledger, &work_id)?.state, JobState::Ready);
        let Err(KnowledgeError::Io(error)) = ledger.claim(&work_id, &AgentId::new("worker-1"))
        else {
            return Err(TestError::Unexpected(
                "ein fremder Job darf nicht beansprucht werden".to_owned(),
            ));
        };
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert_eq!(state_of(&ledger, &work_id)?.state, JobState::Ready);
        Ok(())
    }

    #[test]
    fn unknown_jobs_have_no_snapshot_and_refuse_transitions() -> TestResult {
        let (ledger, _store, _dir) = ledger()?;
        let unknown = WorkId::new();
        assert!(
            ledger
                .snapshot(&unknown)
                .map_err(ctx("snapshot"))?
                .is_none()
        );
        let Err(KnowledgeError::ArtifactNotFound(_)) = ledger.cancel(&unknown) else {
            return Err(TestError::Unexpected(
                "ein unbekannter Job muss ArtifactNotFound liefern".to_owned(),
            ));
        };
        Ok(())
    }

    /// Der durable Adapter und die Referenz stimmen auf dem gemeinsamen
    /// Pfad `Ready → Running → Blocked → Ready → Running → Completed` überein.
    #[test]
    fn matches_the_in_memory_reference_on_the_shared_path() -> TestResult {
        let (ledger, _store, _dir) = ledger()?;
        let durable = admit_ready(&ledger.store, kanban())?;
        let reference = InMemoryJobTransitions::new();
        let in_memory = WorkId::new();
        reference.insert(in_memory.clone(), JobSnapshot::new(JobState::Ready));

        let holder = AgentId::new("worker-1");
        let durable_jobs: &dyn JobTransitions = &ledger;
        let reference_jobs: &dyn JobTransitions = &reference;
        let pairs = [(durable_jobs, &durable), (reference_jobs, &in_memory)];
        for (jobs, work_id) in pairs {
            jobs.claim(work_id, &holder).map_err(ctx("claim"))?;
            jobs.block(work_id, BlockKind::Capability)
                .map_err(ctx("block"))?;
            jobs.unblock(work_id).map_err(ctx("unblock"))?;
            jobs.claim(work_id, &holder).map_err(ctx("claim again"))?;
            jobs.complete(work_id).map_err(ctx("complete"))?;
        }
        let left = state_of(&ledger, &durable)?;
        let right = reference
            .snapshot(&in_memory)
            .map_err(ctx("reference snapshot"))?
            .ok_or(TestError::Missing("reference snapshot"))?;
        assert_eq!(left.state, right.state);
        assert_eq!(left.holder, right.holder);
        Ok(())
    }
}
