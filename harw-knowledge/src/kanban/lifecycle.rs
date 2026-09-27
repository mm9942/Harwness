//! §6.3 card-lifecycle transitions and the §6.4 approval/structural gates.
//!
//! # Verantwortung
//! Je Pfeil des §6.3-Diagramms eine `pub fn`, plus die drei §6.4-Gates an
//! genau den Übergängen, die sie betreffen (`claim`, `complete`, `archive`).
//! Jede Funktion prüft den **abgeleiteten** Ausgangszustand der übergebenen
//! [`Card`]-Sicht explizit und liefert bei unzulässigem Ausgangszustand
//! [`KnowledgeError::IllegalTransition`] — nie ein stilles No-op.
//!
//! # Job-Übergang zuerst, Karte danach (§6.3)
//! „Every transition is a `harw-job-runtime` state transition first and a
//! card render second." Darum ändert keine Funktion hier `card.state`
//! selbst: sie ruft den passenden Übergang auf [`JobTransitions`] auf und
//! baut die Sicht danach **neu aus dem Ledger** ([`CardRecord::view_with`]).
//! Lehnt das Ledger ab, bleibt die Karte unverändert. Der Trait wird gegen
//! `harw-job-core` (Job + Lease + Retry) implementiert; diese Crate
//! liefert nur die Referenz [`InMemoryJobTransitions`] (Tests,
//! Einzelprozess-Betrieb ohne durablen Ledger).
//!
//! # Was der Aufrufer persistieren muss
//! Der Kartenzustand wird nie gespeichert. Nur zwei Übergänge ändern den
//! gespeicherten [`CardRecord`]: [`triage_to_todo`] (neue `work_id`) und
//! [`archive`] (Tag [`ARCHIVED_TAG`]). Nach ihnen muss der Aufrufer
//! `card.record()` über [`crate::kanban::board::save_card`] speichern.
//!
//! # Die drei §6.4-Gates, und was hier NICHT geprüft wird
//! - **Ready -> Running** ([`claim`]): bei `RiskLevel::High`/`Critical` ist
//!   ein positiver [`ApprovalProof`] Pflicht. Woher das Risiko der Lane-Rolle
//!   stammt und wie eine Freigabe aufgelöst wird, entscheidet der Aufrufer.
//! - **Running -> Done|Blocked** ([`complete`]): eine Karte mit
//!   [`REVIEW_REQUIRED_TAG`] wird nie `Done`, sondern der Job wird mit
//!   [`BlockKind::ReviewRequired`] blockiert.
//! - **Blocked|Done -> Archived** ([`archive`]): der Aufrufer liefert die
//!   Anzahl offener Kindkarten; > 0 wird abgelehnt.
//!
//! # Nebenläufigkeit
//! Die Funktionen halten keine Sperren; [`JobTransitions`]-Implementierungen
//! müssen ihre Übergänge selbst atomar machen (die Referenz nutzt einen
//! Mutex). Ein `Card`-Wert darf nicht gleichzeitig von zwei Aufrufern
//! mutiert werden.
//!
//! # Errors
//! Jeder fehlschlagende Pfad liefert [`KnowledgeError`] / [`KnowledgeResult`].

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use harw_job_core::{JobRuntimeError, JobState};
use harw_types::{ReviewDecision, RiskLevel};

use crate::error::{KnowledgeError, KnowledgeResult};
use crate::visibility::AgentId;

use super::board::{ARCHIVED_TAG, BlockKind, Card, CardRecord, CardState, JobSnapshot};

pub use harw_job_core::WorkId;

/// Tag that routes [`complete`] to `Blocked { reason_kind: ReviewRequired }`
/// instead of `Done` (§6.4).
pub const REVIEW_REQUIRED_TAG: &str = "review-required";

/// Die Job-Übergänge, über die jede Kartenbewegung läuft (§6.2/§6.3).
///
/// # Beschreibung
/// Wird später gegen `harw-job-core` implementiert (`Job::mark_ready`,
/// `Job::claim` + `Lease`, `Job::complete`, Retry über `record_failure`).
/// Jede Methode ist ein **Ledger**-Übergang; lehnt das Ledger ihn ab, meldet
/// die Implementierung einen Fehler (üblich: [`KnowledgeError::Job`]) und
/// ändert nichts.
///
/// # Nebenläufigkeit
/// `Send + Sync`, damit eine Implementierung als
/// `Arc<dyn JobTransitions>` in einer `ServiceMap` liegen kann. Jeder
/// Übergang muss für sich atomar sein.
pub trait JobTransitions: Send + Sync {
    /// Aktueller Ledger-Stand eines Jobs; `None`, wenn das Ledger ihn nicht kennt.
    ///
    /// # Fehler
    /// Nur bei Lesefehlern des Ledgers selbst.
    fn snapshot(&self, work_id: &WorkId) -> KnowledgeResult<Option<JobSnapshot>>;

    /// Legt für eine Triage-Karte einen neuen Job im Zustand `Pending` an.
    ///
    /// # Rückgabe
    /// Die `WorkId` des neuen Jobs.
    ///
    /// # Fehler
    /// Wenn das Ledger keinen Job anlegen kann.
    fn create(&self, card: &CardRecord) -> KnowledgeResult<WorkId>;

    /// `Pending -> Ready`.
    ///
    /// # Fehler
    /// Wenn der Job nicht `Pending`/`Ready` ist.
    fn mark_ready(&self, work_id: &WorkId) -> KnowledgeResult<()>;

    /// `Ready -> Running` für `holder` (Lease).
    ///
    /// # Fehler
    /// Wenn der Job nicht `Ready` ist oder die Lease umkämpft ist.
    fn claim(&self, work_id: &WorkId, holder: &AgentId) -> KnowledgeResult<()>;

    /// `Running -> Completed`.
    ///
    /// # Fehler
    /// Wenn der Job nicht `Running` ist.
    fn complete(&self, work_id: &WorkId) -> KnowledgeResult<()>;

    /// `Running -> Blocked` mit Grund.
    ///
    /// # Fehler
    /// Wenn der Job nicht `Running` ist.
    fn block(&self, work_id: &WorkId, reason: BlockKind) -> KnowledgeResult<()>;

    /// `Blocked|Failed -> Ready`.
    ///
    /// # Fehler
    /// Wenn der Job weder `Blocked` noch `Failed` ist.
    fn unblock(&self, work_id: &WorkId) -> KnowledgeResult<()>;

    /// `Blocked { AwaitingApproval } -> Ready` als ausdrückliche Freigabe
    /// des Operators (Plan D2, Kanban-Worker).
    ///
    /// # Beschreibung
    /// Die Vorgabe ist [`Self::unblock`]. Ein durables Ledger überschreibt
    /// die Methode und hält die Freigabe so fest, dass ein Worker sie von
    /// einem bloßen `unblock` unterscheiden kann (`harw-runtime`
    /// `JobStoreTransitions`: Freigabe-Sidecar mit Kanban-Präfix).
    ///
    /// # Argumente
    /// - `approved_by` (`&AgentId`): wer freigibt (Audit).
    /// - `note` (`Option<&str>`): optionaler Freitext.
    ///
    /// # Fehler
    /// Wie [`Self::unblock`].
    fn approve(
        &self,
        work_id: &WorkId,
        approved_by: &AgentId,
        note: Option<&str>,
    ) -> KnowledgeResult<()> {
        let _ = (approved_by, note);
        self.unblock(work_id)
    }

    /// `Running -> Ready` nach totem/abgelaufenem Halter; zählt einen Versuch.
    ///
    /// # Fehler
    /// Wenn der Job nicht `Running` ist oder die Retry-Politik erschöpft ist.
    fn reclaim(&self, work_id: &WorkId) -> KnowledgeResult<()>;

    /// Beendet einen nicht abgeschlossenen Job endgültig (`-> Cancelled`).
    ///
    /// # Fehler
    /// Wenn der Job bereits `Completed` ist.
    fn cancel(&self, work_id: &WorkId) -> KnowledgeResult<()>;
}

/// Evidence that the §6.4 approval gate on a high-risk claim was resolved.
///
/// # Description
/// This crate never contacts an approval service itself — the caller obtains
/// this proof from wherever [`ReviewDecision`]s are adjudicated and passes it
/// into [`claim`], which only checks that it exists and is positive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalProof {
    /// The resolved decision.
    pub decision: ReviewDecision,
    /// Identity of whoever resolved the approval (audit trail).
    pub approved_by: AgentId,
}

impl ApprovalProof {
    /// Build a proof from a resolved decision and the identity that resolved it.
    #[must_use]
    pub fn new(decision: ReviewDecision, approved_by: AgentId) -> Self {
        Self {
            decision,
            approved_by,
        }
    }

    /// `true` for `Approved`/`ApprovedOnce`, `false` for `Rejected`.
    #[must_use]
    pub fn is_positive(&self) -> bool {
        matches!(
            self.decision,
            ReviewDecision::Approved | ReviewDecision::ApprovedOnce
        )
    }
}

/// Build an [`KnowledgeError::IllegalTransition`] from a card's current state.
fn illegal_transition(card: &Card, to: &str) -> KnowledgeError {
    KnowledgeError::IllegalTransition {
        from: card.state.label().to_owned(),
        to: to.to_owned(),
    }
}

/// `RiskLevel::High` or above requires the §6.4 approval gate on [`claim`].
fn requires_approval(risk_level: RiskLevel) -> bool {
    matches!(risk_level, RiskLevel::High | RiskLevel::Critical)
}

/// Die `WorkId` einer Karte, deren Zustand einen Job voraussetzt.
fn bound_work_id(card: &Card) -> KnowledgeResult<WorkId> {
    card.work_id.clone().ok_or_else(|| {
        KnowledgeError::ArtifactNotFound(format!("job of card {} (no work_id)", card.id))
    })
}

/// Baut die Sicht nach einem Ledger-Übergang neu aus dem Ledger.
fn refresh(jobs: &dyn JobTransitions, card: &mut Card) -> KnowledgeResult<()> {
    *card = card.record().view_with(jobs)?;
    Ok(())
}

/// `Triage -> Todo` (§6.3 "decompose"): legt den Job an (`Pending`).
///
/// Der Aufrufer muss danach `card.record()` speichern (neue `work_id`).
///
/// # Errors
/// [`KnowledgeError::IllegalTransition`] unless `card.state == Triage`;
/// Fehler aus [`JobTransitions::create`].
pub fn triage_to_todo(jobs: &dyn JobTransitions, card: &mut Card) -> KnowledgeResult<()> {
    if !matches!(card.state, CardState::Triage) {
        return Err(illegal_transition(card, "todo"));
    }
    let work_id = jobs.create(&card.record())?;
    let mut record = card.record();
    record.work_id = Some(work_id);
    *card = record.view_with(jobs)?;
    Ok(())
}

/// `Todo -> Ready` (§6.3), gated on every parent in `parent_states` being `Done`.
///
/// # Description
/// `parent_states` must be exactly the current (ledger-derived) states of
/// `card.parents`. An empty slice trivially satisfies the gate.
///
/// # Errors
/// [`KnowledgeError::IllegalTransition`] if `card.state != Todo` or a parent
/// is not `Done`; Fehler aus [`JobTransitions::mark_ready`].
pub fn todo_to_ready(
    jobs: &dyn JobTransitions,
    card: &mut Card,
    parent_states: &[CardState],
) -> KnowledgeResult<()> {
    if !matches!(card.state, CardState::Todo) {
        return Err(illegal_transition(card, "ready"));
    }
    if !parent_states
        .iter()
        .all(|state| matches!(state, CardState::Done))
    {
        return Err(illegal_transition(card, "ready (parents not all done)"));
    }
    jobs.mark_ready(&bound_work_id(card)?)?;
    refresh(jobs, card)
}

/// `Ready -> Running` (§6.3 claim), gated by §6.4's high-risk approval rule.
///
/// # Errors
/// - [`KnowledgeError::IllegalTransition`] if `card.state != Ready`.
/// - [`KnowledgeError::ClaimRequiresApproval`] if the risk gate applies and
///   `approval` is missing or not positive.
/// - Fehler aus [`JobTransitions::claim`].
pub fn claim(
    jobs: &dyn JobTransitions,
    card: &mut Card,
    holder: &AgentId,
    lane_role_risk: RiskLevel,
    approval: Option<&ApprovalProof>,
) -> KnowledgeResult<()> {
    if !matches!(card.state, CardState::Ready) {
        return Err(illegal_transition(card, "running"));
    }
    if requires_approval(lane_role_risk) && !approval.is_some_and(ApprovalProof::is_positive) {
        return Err(KnowledgeError::ClaimRequiresApproval {
            card_id: card.id.to_string(),
            risk_level: lane_role_risk.to_string(),
        });
    }
    jobs.claim(&bound_work_id(card)?, holder)?;
    refresh(jobs, card)
}

/// `Running -> Done`, or `Running -> Blocked { ReviewRequired }` for a card
/// tagged [`REVIEW_REQUIRED_TAG`] (§6.3 complete, §6.4).
///
/// # Errors
/// [`KnowledgeError::IllegalTransition`] unless `card.state == Running`;
/// Fehler aus [`JobTransitions::complete`]/[`JobTransitions::block`].
pub fn complete(jobs: &dyn JobTransitions, card: &mut Card) -> KnowledgeResult<()> {
    if !matches!(card.state, CardState::Running) {
        return Err(illegal_transition(card, "done"));
    }
    let work_id = bound_work_id(card)?;
    if card.tags.iter().any(|tag| tag == REVIEW_REQUIRED_TAG) {
        jobs.block(&work_id, BlockKind::ReviewRequired)?;
    } else {
        jobs.complete(&work_id)?;
    }
    refresh(jobs, card)
}

/// `Running -> Blocked { reason_kind }` (§6.3 block).
///
/// Callers that need the review gate must go through [`complete`]; nothing
/// here stops an explicit `BlockKind::ReviewRequired`.
///
/// # Errors
/// [`KnowledgeError::IllegalTransition`] unless `card.state == Running`;
/// Fehler aus [`JobTransitions::block`].
pub fn block(
    jobs: &dyn JobTransitions,
    card: &mut Card,
    reason_kind: BlockKind,
) -> KnowledgeResult<()> {
    if !matches!(card.state, CardState::Running) {
        return Err(illegal_transition(card, "blocked"));
    }
    jobs.block(&bound_work_id(card)?, reason_kind)?;
    refresh(jobs, card)
}

/// `Blocked -> Ready` (§6.3 unblock).
///
/// # Errors
/// [`KnowledgeError::IllegalTransition`] unless `card.state` is `Blocked { .. }`;
/// Fehler aus [`JobTransitions::unblock`].
pub fn unblock(jobs: &dyn JobTransitions, card: &mut Card) -> KnowledgeResult<()> {
    if !matches!(card.state, CardState::Blocked { .. }) {
        return Err(illegal_transition(card, "ready"));
    }
    jobs.unblock(&bound_work_id(card)?)?;
    refresh(jobs, card)
}

/// `Blocked { AwaitingApproval } -> Ready` (Plan D2): die Freigabe einer
/// Worker-Karte durch den Operator; danach darf der Kanban-Worker den
/// Rollen-Agenten starten.
///
/// # Errors
/// [`KnowledgeError::IllegalTransition`] unless `card.state` is
/// `Blocked { AwaitingApproval }`; Fehler aus [`JobTransitions::approve`].
pub fn approve(
    jobs: &dyn JobTransitions,
    card: &mut Card,
    approved_by: &AgentId,
    note: Option<&str>,
) -> KnowledgeResult<()> {
    if !matches!(
        card.state,
        CardState::Blocked {
            reason_kind: BlockKind::AwaitingApproval
        }
    ) {
        return Err(illegal_transition(card, "ready (approve)"));
    }
    jobs.approve(&bound_work_id(card)?, approved_by, note)?;
    refresh(jobs, card)
}

/// `true`, wenn die Karte auf die Freigabe des Operators wartet.
#[must_use]
pub fn awaits_approval(card: &Card) -> bool {
    matches!(
        card.state,
        CardState::Blocked {
            reason_kind: BlockKind::AwaitingApproval
        }
    )
}

/// `Running -> Ready` on reclaim (§6.3, "dead/timeout ... [retry-counted]").
///
/// # Returns
/// The card's new `retry_count` (= the job's `attempts` per the ledger).
///
/// # Errors
/// [`KnowledgeError::IllegalTransition`] unless `card.state == Running`;
/// Fehler aus [`JobTransitions::reclaim`].
pub fn reclaim(jobs: &dyn JobTransitions, card: &mut Card) -> KnowledgeResult<u32> {
    if !matches!(card.state, CardState::Running) {
        return Err(illegal_transition(card, "ready (reclaim)"));
    }
    jobs.reclaim(&bound_work_id(card)?)?;
    refresh(jobs, card)?;
    Ok(card.retry_count)
}

/// `Done|Blocked -> Archived` (§6.3 archive), gated by §6.4's structural
/// child-card invariant.
///
/// # Description
/// Ein blockierter Job wird im Ledger beendet ([`JobTransitions::cancel`]),
/// ein abgeschlossener bleibt, wie er ist. Die Karte bekommt das Tag
/// [`ARCHIVED_TAG`]; der Aufrufer muss `card.record()` danach speichern.
///
/// # Errors
/// - [`KnowledgeError::IllegalTransition`] unless `card.state` is `Done` or
///   `Blocked { .. }`.
/// - [`KnowledgeError::ArchiveBlockedByChildren`] if `unresolved_children > 0`.
/// - Fehler aus [`JobTransitions::cancel`].
pub fn archive(
    jobs: &dyn JobTransitions,
    card: &mut Card,
    unresolved_children: usize,
) -> KnowledgeResult<()> {
    let blocked = matches!(card.state, CardState::Blocked { .. });
    if !(blocked || matches!(card.state, CardState::Done)) {
        return Err(illegal_transition(card, "archived"));
    }
    if unresolved_children > 0 {
        return Err(KnowledgeError::ArchiveBlockedByChildren {
            card_id: card.id.to_string(),
            blocking_children: unresolved_children,
        });
    }
    if blocked {
        jobs.cancel(&bound_work_id(card)?)?;
    }
    let mut record = card.record();
    if !record.is_archived() {
        record.tags.push(ARCHIVED_TAG.to_owned());
    }
    *card = record.view_with(jobs)?;
    Ok(())
}

// --- Referenz-Ledger ----------------------------------------------------------

/// Prozesslokales Referenz-Ledger für [`JobTransitions`].
///
/// # Beschreibung
/// Hält je `WorkId` einen [`JobSnapshot`] hinter einem Mutex und erzwingt
/// dieselben Zustandsregeln wie `harw_job_core::Job` (`mark_ready` nur aus
/// `Pending|Ready`, `claim` nur aus `Ready`, …). Nicht durabel und ohne
/// Lease-Ablauf/Budget — gedacht für Tests und als Vorlage für den Adapter
/// über `harw-job-core`, nicht als Governance-Ledger.
#[derive(Debug, Default)]
pub struct InMemoryJobTransitions {
    jobs: Mutex<HashMap<WorkId, JobSnapshot>>,
}

impl InMemoryJobTransitions {
    /// Ein leeres Ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Setzt (oder ersetzt) den Snapshot eines Jobs direkt.
    pub fn insert(&self, work_id: WorkId, snapshot: JobSnapshot) {
        self.jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(work_id, snapshot);
    }

    /// Führt `change` atomar auf dem Snapshot von `work_id` aus.
    fn update(
        &self,
        work_id: &WorkId,
        change: impl FnOnce(&mut JobSnapshot) -> KnowledgeResult<()>,
    ) -> KnowledgeResult<()> {
        let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        let snapshot = jobs
            .get_mut(work_id)
            .ok_or_else(|| KnowledgeError::ArtifactNotFound(format!("job {work_id}")))?;
        change(snapshot)
    }
}

/// Fehler für einen Übergang aus einem unpassenden Job-Zustand.
fn invalid_state(work_id: &WorkId, expected: JobState, actual: JobState) -> KnowledgeError {
    KnowledgeError::Job(JobRuntimeError::InvalidState {
        work_id: work_id.clone(),
        expected,
        actual,
    })
}

impl JobTransitions for InMemoryJobTransitions {
    fn snapshot(&self, work_id: &WorkId) -> KnowledgeResult<Option<JobSnapshot>> {
        Ok(self
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(work_id)
            .cloned())
    }

    fn create(&self, _card: &CardRecord) -> KnowledgeResult<WorkId> {
        let work_id = WorkId::new();
        self.insert(work_id.clone(), JobSnapshot::new(JobState::Pending));
        Ok(work_id)
    }

    fn mark_ready(&self, work_id: &WorkId) -> KnowledgeResult<()> {
        self.update(work_id, |job| {
            if !matches!(job.state, JobState::Pending | JobState::Ready) {
                return Err(invalid_state(work_id, JobState::Pending, job.state));
            }
            job.state = JobState::Ready;
            Ok(())
        })
    }

    fn claim(&self, work_id: &WorkId, holder: &AgentId) -> KnowledgeResult<()> {
        self.update(work_id, |job| {
            if job.state != JobState::Ready {
                return Err(invalid_state(work_id, JobState::Ready, job.state));
            }
            job.state = JobState::Running;
            job.holder = Some(holder.clone());
            Ok(())
        })
    }

    fn complete(&self, work_id: &WorkId) -> KnowledgeResult<()> {
        self.update(work_id, |job| {
            if job.state != JobState::Running {
                return Err(invalid_state(work_id, JobState::Running, job.state));
            }
            job.state = JobState::Completed;
            job.holder = None;
            Ok(())
        })
    }

    fn block(&self, work_id: &WorkId, reason: BlockKind) -> KnowledgeResult<()> {
        self.update(work_id, |job| {
            if job.state != JobState::Running {
                return Err(invalid_state(work_id, JobState::Running, job.state));
            }
            job.state = JobState::Blocked;
            job.holder = None;
            job.block_reason = Some(reason);
            Ok(())
        })
    }

    fn unblock(&self, work_id: &WorkId) -> KnowledgeResult<()> {
        self.update(work_id, |job| {
            if !matches!(job.state, JobState::Blocked | JobState::Failed) {
                return Err(invalid_state(work_id, JobState::Blocked, job.state));
            }
            job.state = JobState::Ready;
            job.block_reason = None;
            Ok(())
        })
    }

    fn reclaim(&self, work_id: &WorkId) -> KnowledgeResult<()> {
        self.update(work_id, |job| {
            if job.state != JobState::Running {
                return Err(invalid_state(work_id, JobState::Running, job.state));
            }
            job.state = JobState::Ready;
            job.holder = None;
            job.attempts = job.attempts.saturating_add(1);
            Ok(())
        })
    }

    fn cancel(&self, work_id: &WorkId) -> KnowledgeResult<()> {
        self.update(work_id, |job| {
            if job.state == JobState::Completed {
                return Err(invalid_state(work_id, JobState::Running, job.state));
            }
            job.state = JobState::Cancelled;
            job.holder = None;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kanban::board::{CardId, LaneId};
    use crate::test_support::{TestError, TestResult};
    use crate::visibility::VisibilityScope;

    fn record() -> CardRecord {
        CardRecord {
            id: CardId::new("card-1"),
            lane_id: LaneId::new("lane-1"),
            title: "test".to_owned(),
            body: String::new(),
            work_id: None,
            parents: Vec::new(),
            tags: Vec::new(),
            visibility: VisibilityScope::SelfOnly,
        }
    }

    /// Eine Karte, deren Job im Ledger im Zustand `state` steht.
    fn card_in(jobs: &InMemoryJobTransitions, state: JobState) -> TestResult<Card> {
        let work_id = WorkId::new();
        jobs.insert(work_id.clone(), JobSnapshot::new(state));
        let mut stored = record();
        stored.work_id = Some(work_id);
        Ok(stored.view_with(jobs)?)
    }

    fn holder() -> AgentId {
        AgentId::new("worker-1")
    }

    #[test]
    fn test_triage_to_todo_creates_a_pending_job() -> TestResult {
        let jobs = InMemoryJobTransitions::new();
        let mut card = record().view(None)?;
        assert_eq!(card.state, CardState::Triage);
        triage_to_todo(&jobs, &mut card)?;
        assert_eq!(card.state, CardState::Todo);
        let work_id = card.work_id.clone().ok_or(TestError::Missing("work_id"))?;
        assert_eq!(
            jobs.snapshot(&work_id)?.map(|job| job.state),
            Some(JobState::Pending)
        );
        Ok(())
    }

    #[test]
    fn test_triage_to_todo_rejects_non_triage_source() -> TestResult {
        let jobs = InMemoryJobTransitions::new();
        let mut card = card_in(&jobs, JobState::Completed)?;
        let Err(error) = triage_to_todo(&jobs, &mut card) else {
            return Err(TestError::Unexpected(
                "done -> todo must be illegal".to_owned(),
            ));
        };
        assert!(matches!(error, KnowledgeError::IllegalTransition { .. }));
        assert_eq!(card.state, CardState::Done);
        Ok(())
    }

    #[test]
    fn test_todo_to_ready_respects_the_parent_gate() -> TestResult {
        let jobs = InMemoryJobTransitions::new();
        let mut card = card_in(&jobs, JobState::Pending)?;
        let Err(error) = todo_to_ready(&jobs, &mut card, &[CardState::Done, CardState::Running])
        else {
            return Err(TestError::Unexpected(
                "an unfinished parent must block readiness".to_owned(),
            ));
        };
        assert!(matches!(error, KnowledgeError::IllegalTransition { .. }));
        assert_eq!(card.state, CardState::Todo);

        todo_to_ready(&jobs, &mut card, &[CardState::Done])?;
        assert_eq!(card.state, CardState::Ready);
        Ok(())
    }

    #[test]
    fn test_approve_only_frees_a_card_awaiting_approval() -> TestResult {
        let jobs = InMemoryJobTransitions::new();
        let mut card = card_in(&jobs, JobState::Blocked)?;
        let Err(error) = approve(&jobs, &mut card, &holder(), None) else {
            return Err(TestError::Unexpected(
                "approve without AwaitingApproval must be illegal".to_owned(),
            ));
        };
        assert!(matches!(error, KnowledgeError::IllegalTransition { .. }));

        let work_id = card.work_id.clone().ok_or(TestError::Missing("work_id"))?;
        let mut waiting = JobSnapshot::new(JobState::Blocked);
        waiting.block_reason = Some(BlockKind::AwaitingApproval);
        jobs.insert(work_id, waiting);
        let mut card = card.record().view_with(&jobs)?;
        assert!(awaits_approval(&card));
        approve(&jobs, &mut card, &holder(), Some("passt"))?;
        assert_eq!(card.state, CardState::Ready);
        assert!(!awaits_approval(&card));
        Ok(())
    }

    #[test]
    fn test_claim_low_risk_needs_no_approval_and_records_the_holder() -> TestResult {
        let jobs = InMemoryJobTransitions::new();
        let mut card = card_in(&jobs, JobState::Ready)?;
        claim(&jobs, &mut card, &holder(), RiskLevel::Low, None)?;
        assert_eq!(card.state, CardState::Running);
        assert_eq!(card.assignee, Some(holder()));
        Ok(())
    }

    #[test]
    fn test_claim_on_high_risk_lane_without_approval_is_refused_and_ledger_untouched() -> TestResult
    {
        let jobs = InMemoryJobTransitions::new();
        let mut card = card_in(&jobs, JobState::Ready)?;
        let Err(error) = claim(&jobs, &mut card, &holder(), RiskLevel::High, None) else {
            return Err(TestError::Unexpected(
                "high-risk claim without approval must be refused".to_owned(),
            ));
        };
        assert!(matches!(
            error,
            KnowledgeError::ClaimRequiresApproval { .. }
        ));
        let work_id = card.work_id.clone().ok_or(TestError::Missing("work_id"))?;
        assert_eq!(
            jobs.snapshot(&work_id)?.map(|job| job.state),
            Some(JobState::Ready)
        );
        Ok(())
    }

    #[test]
    fn test_claim_with_rejected_proof_is_refused_and_approved_proof_passes() -> TestResult {
        let jobs = InMemoryJobTransitions::new();
        let mut card = card_in(&jobs, JobState::Ready)?;
        let rejected = ApprovalProof::new(ReviewDecision::Rejected, AgentId::new("op"));
        assert!(
            claim(
                &jobs,
                &mut card,
                &holder(),
                RiskLevel::Critical,
                Some(&rejected)
            )
            .is_err()
        );
        let approved = ApprovalProof::new(ReviewDecision::Approved, AgentId::new("op"));
        claim(
            &jobs,
            &mut card,
            &holder(),
            RiskLevel::High,
            Some(&approved),
        )?;
        assert_eq!(card.state, CardState::Running);
        Ok(())
    }

    #[test]
    fn test_complete_moves_a_plain_card_to_done() -> TestResult {
        let jobs = InMemoryJobTransitions::new();
        let mut card = card_in(&jobs, JobState::Running)?;
        complete(&jobs, &mut card)?;
        assert_eq!(card.state, CardState::Done);
        Ok(())
    }

    #[test]
    fn test_complete_blocks_a_review_required_card_in_the_ledger() -> TestResult {
        let jobs = InMemoryJobTransitions::new();
        let work_id = WorkId::new();
        jobs.insert(work_id.clone(), JobSnapshot::new(JobState::Running));
        let mut stored = record();
        stored.work_id = Some(work_id.clone());
        stored.tags.push(REVIEW_REQUIRED_TAG.to_owned());
        let mut card = stored.view_with(&jobs)?;

        complete(&jobs, &mut card)?;

        let expected = CardState::Blocked {
            reason_kind: BlockKind::ReviewRequired,
        };
        assert_eq!(card.state, expected);
        assert_eq!(
            jobs.snapshot(&work_id)?.map(|job| job.state),
            Some(JobState::Blocked)
        );
        Ok(())
    }

    #[test]
    fn test_block_and_unblock_round_trip() -> TestResult {
        let jobs = InMemoryJobTransitions::new();
        let mut card = card_in(&jobs, JobState::Running)?;
        block(&jobs, &mut card, BlockKind::NeedsInput)?;
        assert_eq!(
            card.state,
            CardState::Blocked {
                reason_kind: BlockKind::NeedsInput
            }
        );
        unblock(&jobs, &mut card)?;
        assert_eq!(card.state, CardState::Ready);
        let Err(error) = unblock(&jobs, &mut card) else {
            return Err(TestError::Unexpected(
                "ready -> ready must be illegal".to_owned(),
            ));
        };
        assert!(matches!(error, KnowledgeError::IllegalTransition { .. }));
        Ok(())
    }

    #[test]
    fn test_reclaim_counts_attempts_from_the_ledger() -> TestResult {
        let jobs = InMemoryJobTransitions::new();
        let mut card = card_in(&jobs, JobState::Running)?;
        assert_eq!(reclaim(&jobs, &mut card)?, 1);
        assert_eq!(card.state, CardState::Ready);
        assert!(card.assignee.is_none());
        claim(&jobs, &mut card, &holder(), RiskLevel::Low, None)?;
        assert_eq!(reclaim(&jobs, &mut card)?, 2);
        Ok(())
    }

    #[test]
    fn test_archive_gate_and_blocked_jobs_are_cancelled() -> TestResult {
        let jobs = InMemoryJobTransitions::new();
        let mut done = card_in(&jobs, JobState::Completed)?;
        let Err(error) = archive(&jobs, &mut done, 2) else {
            return Err(TestError::Unexpected(
                "unresolved children must block archival".to_owned(),
            ));
        };
        assert!(matches!(
            error,
            KnowledgeError::ArchiveBlockedByChildren {
                blocking_children: 2,
                ..
            }
        ));
        archive(&jobs, &mut done, 0)?;
        assert_eq!(done.state, CardState::Archived);
        assert!(done.record().is_archived());

        let mut blocked = card_in(&jobs, JobState::Blocked)?;
        archive(&jobs, &mut blocked, 0)?;
        let work_id = blocked
            .work_id
            .clone()
            .ok_or(TestError::Missing("work_id"))?;
        assert_eq!(
            jobs.snapshot(&work_id)?.map(|job| job.state),
            Some(JobState::Cancelled)
        );

        let mut running = card_in(&jobs, JobState::Running)?;
        assert!(matches!(
            archive(&jobs, &mut running, 0),
            Err(KnowledgeError::IllegalTransition { .. })
        ));
        Ok(())
    }

    /// §6.3: die Karte zeigt nie einen Zustand, dem das Ledger widerspricht —
    /// ändert sich der Job außerhalb der Karte, folgt die nächste Sicht.
    #[test]
    fn test_the_card_follows_the_ledger_not_its_own_memory() -> TestResult {
        let jobs = InMemoryJobTransitions::new();
        let card = card_in(&jobs, JobState::Running)?;
        let work_id = card.work_id.clone().ok_or(TestError::Missing("work_id"))?;
        let mut failed = JobSnapshot::new(JobState::Failed);
        failed.attempts = 3;
        jobs.insert(work_id, failed);

        let fresh = card.record().view_with(&jobs)?;
        assert_eq!(
            fresh.state,
            CardState::Blocked {
                reason_kind: BlockKind::Transient
            }
        );
        assert_eq!(fresh.retry_count, 3);
        Ok(())
    }

    #[test]
    fn test_a_ledger_refusal_leaves_the_card_unchanged() -> TestResult {
        let jobs = InMemoryJobTransitions::new();
        let mut card = card_in(&jobs, JobState::Ready)?;
        let work_id = card.work_id.clone().ok_or(TestError::Missing("work_id"))?;
        // Das Ledger ist inzwischen weiter als die (veraltete) Sicht.
        jobs.insert(work_id, JobSnapshot::new(JobState::Completed));
        let before = card.clone();
        let Err(error) = claim(&jobs, &mut card, &holder(), RiskLevel::Low, None) else {
            return Err(TestError::Unexpected(
                "the ledger must refuse a stale claim".to_owned(),
            ));
        };
        assert!(matches!(error, KnowledgeError::Job(_)));
        assert_eq!(card, before);
        Ok(())
    }
}
