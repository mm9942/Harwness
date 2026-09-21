//! §6.3 card-lifecycle transitions and the §6.4 approval/structural gates.
//!
//! # Verantwortung
//! Dieses Modul besitzt genau die acht Pfeile aus dem §6.3-Diagramm, je eine
//! `pub fn` pro Pfeil, und die drei §6.4-Vorbedingungen an genau den
//! Übergängen, die sie betreffen (`claim`, `complete`, `archive`). Jede
//! Funktion prüft den Ausgangszustand der übergebenen [`Card`] explizit und
//! liefert bei einem unzulässigen Ausgangszustand
//! [`KnowledgeError::IllegalTransition`] — nie ein stilles No-op. Dieses
//! Modul persistiert nichts selbst (das bleibt Sache von
//! [`crate::kanban::board::save_card`]) und ruft **keine**
//! `harw-job-runtime`-Funktion auf: die Karte trägt zwar einen `WorkId`, aber
//! das eigentliche Claim/Lease/Retry der Governed-Work-Maschinerie bleibt
//! vollständig beim Aufrufer (siehe §6.2 — diese Crate ist nicht von
//! `harw-job-runtime`s Laufzeitverhalten abhängig, nur von dessen `WorkId`-Typ).
//!
//! # Die drei §6.4-Gates, und was hier NICHT geprüft wird
//! - **Ready -> Running** ([`claim`]): verlangt bei
//!   `RiskLevel::High`/`RiskLevel::Critical` einen positiven [`ApprovalProof`]
//!   als Parameter. Woher der Aufrufer das Risiko der gebundenen Rolle kennt
//!   (Lane -> `AgentRoleRef` -> `RiskLevel`) und wie ein `ApprovalRequest`
//!   tatsächlich aufgelöst wird, ist **nicht** Sache dieser Crate — das ist
//!   `harw-policy`s/`harw_types::roles::ReviewDecision`s Domäne. Dieses Modul
//!   prüft nur: liegt ein `ApprovalProof` vor, und ist seine `decision`
//!   positiv.
//! - **Running -> Done|Blocked** ([`complete`]): eine Karte mit dem Tag
//!   [`REVIEW_REQUIRED_TAG`] wird nie `Done`, sondern
//!   `Blocked { reason_kind: BlockKind::ReviewRequired }`.
//! - **Blocked|Done -> Archived** ([`archive`]): verlangt vom Aufrufer die
//!   Anzahl noch nicht abgeschlossener Kindkarten (`unresolved_children`) —
//!   das Aufsuchen der Kindkarten selbst (Graphtraversal über `parents`)
//!   bleibt bewusst beim Aufrufer, der bereits Zugriff auf
//!   [`crate::kanban::board::list_cards`] hat.
//!
//! # Concurrency
//! Reine, threadsichere Funktionen auf `&mut Card` ohne I/O und ohne inneres
//! Locking. Ein `Card`-Wert darf nicht gleichzeitig von zwei Aufrufern
//! mutiert werden; das Serialisieren bleibt — wie überall in dieser Crate —
//! Sache des Aufrufers.
//!
//! # Errors
//! Jeder fehlschlagende Pfad liefert [`crate::error::KnowledgeError`] /
//! [`crate::error::KnowledgeResult`].

use harw_types::{ReviewDecision, RiskLevel};

use crate::error::{KnowledgeError, KnowledgeResult};
use crate::visibility::AgentId;

use super::board::{BlockKind, Card, CardId, CardState, LaneId};

pub use harw_job_runtime::WorkId;

/// Tag that routes [`complete`] to `Blocked { reason_kind: ReviewRequired }`
/// instead of `Done` (§6.4, "Running -> Done ... doesn't auto-flip ... on a
/// card tagged `review-required`").
pub const REVIEW_REQUIRED_TAG: &str = "review-required";

/// Evidence that the §6.4 approval gate on a high-risk claim was resolved.
///
/// # Description
/// This crate never contacts an approval service itself — the caller obtains
/// this proof from wherever [`ReviewDecision`]s are actually adjudicated
/// (`harw-policy`, an operator-facing `/kanban claim` command, ...) and
/// passes it into [`claim`]. [`claim`] only checks that the proof exists and
/// that its `decision` is positive ([`ApprovalProof::is_positive`]); it never
/// re-derives or re-requests the decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalProof {
    /// The resolved decision (matches `harw-types::roles::ReviewDecision`,
    /// §6.4: "same `ReviewDecision` type ... used elsewhere in the harness").
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

/// Short, stable label for a [`CardState`], used only in
/// [`KnowledgeError::IllegalTransition`] messages.
fn state_label(state: &CardState) -> &'static str {
    match state {
        CardState::Triage => "triage",
        CardState::Todo => "todo",
        CardState::Ready => "ready",
        CardState::Running => "running",
        CardState::Blocked { .. } => "blocked",
        CardState::Done => "done",
        CardState::Archived => "archived",
    }
}

/// Build an [`KnowledgeError::IllegalTransition`] from a card's current state
/// to the attempted target.
fn illegal_transition(card: &Card, to: &str) -> KnowledgeError {
    KnowledgeError::IllegalTransition {
        from: state_label(&card.state).to_owned(),
        to: to.to_owned(),
    }
}

/// `RiskLevel::High` or above requires the §6.4 approval gate on [`claim`].
fn requires_approval(risk_level: RiskLevel) -> bool {
    matches!(risk_level, RiskLevel::High | RiskLevel::Critical)
}

/// `Triage -> Todo` (§6.3, "decompose, optional").
///
/// # Errors
/// [`KnowledgeError::IllegalTransition`] unless `card.state == Triage`.
pub fn triage_to_todo(card: &mut Card) -> KnowledgeResult<()> {
    if !matches!(card.state, CardState::Triage) {
        return Err(illegal_transition(card, "todo"));
    }
    card.state = CardState::Todo;
    Ok(())
}

/// `Todo -> Ready` (§6.3), gated on every parent in `parent_states` being `Done`.
///
/// # Description
/// `parent_states` must be exactly the current states of `card.parents`, in
/// any order — this function does not load them itself (see the module doc's
/// "what is NOT checked here"). An empty `parent_states` (a card with no
/// parents) trivially satisfies the gate.
///
/// # Errors
/// - [`KnowledgeError::IllegalTransition`] if `card.state != Todo`, or if any
///   entry of `parent_states` is not `Done`.
pub fn todo_to_ready(card: &mut Card, parent_states: &[CardState]) -> KnowledgeResult<()> {
    if !matches!(card.state, CardState::Todo) {
        return Err(illegal_transition(card, "ready"));
    }
    if !parent_states.iter().all(|state| matches!(state, CardState::Done)) {
        return Err(illegal_transition(card, "ready (parents not all done)"));
    }
    card.state = CardState::Ready;
    Ok(())
}

/// `Ready -> Running` (§6.3 claim), gated by §6.4's high-risk approval rule.
///
/// # Description
/// `lane_role_risk` is the `RiskLevel` of the worker lane's bound
/// `AgentRoleRef` (the caller resolves this — see module doc). When it is
/// `High` or `Critical`, `approval` must be `Some` and
/// [`ApprovalProof::is_positive`] must hold, or the claim is refused. On
/// success the card records `work_id` and `holder` and moves to `Running`.
///
/// # Errors
/// - [`KnowledgeError::IllegalTransition`] if `card.state != Ready`.
/// - [`KnowledgeError::ClaimRequiresApproval`] if the risk gate applies and
///   `approval` is missing or not positive.
pub fn claim(
    card: &mut Card,
    work_id: WorkId,
    holder: AgentId,
    lane_role_risk: RiskLevel,
    approval: Option<&ApprovalProof>,
) -> KnowledgeResult<()> {
    if !matches!(card.state, CardState::Ready) {
        return Err(illegal_transition(card, "running"));
    }
    if requires_approval(lane_role_risk) {
        let positively_approved = approval.is_some_and(ApprovalProof::is_positive);
        if !positively_approved {
            return Err(KnowledgeError::ClaimRequiresApproval {
                card_id: card.id.to_string(),
                risk_level: lane_role_risk.to_string(),
            });
        }
    }
    card.work_id = Some(work_id);
    card.assignee = Some(holder);
    card.state = CardState::Running;
    Ok(())
}

/// `Running -> Done` or `Running -> Blocked { ReviewRequired }` (§6.3 complete,
/// §6.4 review-required gate).
///
/// A card carrying [`REVIEW_REQUIRED_TAG`] in `card.tags` never reaches
/// `Done` directly from this call — it lands in
/// `Blocked { reason_kind: BlockKind::ReviewRequired }` pending explicit
/// operator sign-off, per §6.4.
///
/// # Errors
/// [`KnowledgeError::IllegalTransition`] unless `card.state == Running`.
pub fn complete(card: &mut Card) -> KnowledgeResult<()> {
    if !matches!(card.state, CardState::Running) {
        return Err(illegal_transition(card, "done"));
    }
    if card.tags.iter().any(|tag| tag == REVIEW_REQUIRED_TAG) {
        card.state = CardState::Blocked {
            reason_kind: BlockKind::ReviewRequired,
        };
    } else {
        card.state = CardState::Done;
    }
    Ok(())
}

/// `Running -> Blocked { reason_kind }` (§6.3 block), for every non-review
/// block reason (`Dependency`, `NeedsInput`, `Capability`, `Transient`). A
/// review-required block is produced only by [`complete`], never by this
/// function, so callers cannot bypass the §6.4 completion gate by blocking
/// with `ReviewRequired` directly... except that nothing here stops a caller
/// from passing `BlockKind::ReviewRequired` explicitly; callers that need the
/// review gate must go through [`complete`] instead.
///
/// # Errors
/// [`KnowledgeError::IllegalTransition`] unless `card.state == Running`.
pub fn block(card: &mut Card, reason_kind: BlockKind) -> KnowledgeResult<()> {
    if !matches!(card.state, CardState::Running) {
        return Err(illegal_transition(card, "blocked"));
    }
    card.state = CardState::Blocked { reason_kind };
    Ok(())
}

/// `Blocked -> Ready` (§6.3 unblock).
///
/// # Errors
/// [`KnowledgeError::IllegalTransition`] unless `card.state` is `Blocked { .. }`.
pub fn unblock(card: &mut Card) -> KnowledgeResult<()> {
    if !matches!(card.state, CardState::Blocked { .. }) {
        return Err(illegal_transition(card, "ready"));
    }
    card.state = CardState::Ready;
    Ok(())
}

/// `Running -> Ready` on reclaim (§6.3, "dead/timeout ... [retry-counted]").
///
/// Clears `work_id`/`assignee` (the previous holder is gone) and increments
/// `card.retry_count`.
///
/// # Returns
/// The card's new `retry_count`.
///
/// # Errors
/// [`KnowledgeError::IllegalTransition`] unless `card.state == Running`.
pub fn reclaim(card: &mut Card) -> KnowledgeResult<u32> {
    if !matches!(card.state, CardState::Running) {
        return Err(illegal_transition(card, "ready (reclaim)"));
    }
    card.state = CardState::Ready;
    card.work_id = None;
    card.assignee = None;
    card.retry_count = card.retry_count.saturating_add(1);
    Ok(card.retry_count)
}

/// `Done|Blocked -> Archived` (§6.3 archive), gated by §6.4's structural
/// child-card invariant.
///
/// # Description
/// `unresolved_children` is the number of this card's children (cards whose
/// `parents` include `card.id`) that are not themselves `Done`/`Archived` —
/// the caller computes this (typically via
/// [`crate::kanban::board::list_cards`] plus [`Card::is_terminal`]), since
/// this module never loads other cards itself.
///
/// # Errors
/// - [`KnowledgeError::IllegalTransition`] unless `card.state` is
///   `Done` or `Blocked { .. }`.
/// - [`KnowledgeError::ArchiveBlockedByChildren`] if `unresolved_children > 0`.
pub fn archive(card: &mut Card, unresolved_children: usize) -> KnowledgeResult<()> {
    let from_a_terminal_state = matches!(card.state, CardState::Done | CardState::Blocked { .. });
    if !from_a_terminal_state {
        return Err(illegal_transition(card, "archived"));
    }
    if unresolved_children > 0 {
        return Err(KnowledgeError::ArchiveBlockedByChildren {
            card_id: card.id.to_string(),
            blocking_children: unresolved_children,
        });
    }
    card.state = CardState::Archived;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visibility::VisibilityScope;

    fn card(state: CardState) -> Card {
        Card {
            id: CardId::new("card-1"),
            lane_id: LaneId::new("lane-1"),
            title: "test".to_owned(),
            body: String::new(),
            work_id: None,
            state,
            parents: Vec::new(),
            assignee: None,
            tags: Vec::new(),
            visibility: VisibilityScope::SelfOnly,
            retry_count: 0,
        }
    }

    #[test]
    fn test_triage_to_todo_succeeds_from_triage() {
        let mut c = card(CardState::Triage);
        triage_to_todo(&mut c).expect("triage -> todo succeeds");
        assert_eq!(c.state, CardState::Todo);
    }

    #[test]
    fn test_triage_to_todo_rejects_non_triage_source() {
        let mut c = card(CardState::Done);
        let error = triage_to_todo(&mut c).expect_err("done -> todo must be illegal");
        assert!(matches!(error, KnowledgeError::IllegalTransition { .. }));
        assert_eq!(c.state, CardState::Done, "a rejected transition must not mutate the card");
    }

    #[test]
    fn test_todo_to_ready_succeeds_when_all_parents_done() {
        let mut c = card(CardState::Todo);
        todo_to_ready(&mut c, &[CardState::Done, CardState::Done]).expect("all parents done");
        assert_eq!(c.state, CardState::Ready);
    }

    #[test]
    fn test_todo_to_ready_succeeds_with_no_parents() {
        let mut c = card(CardState::Todo);
        todo_to_ready(&mut c, &[]).expect("no parents trivially satisfies the gate");
        assert_eq!(c.state, CardState::Ready);
    }

    #[test]
    fn test_todo_to_ready_rejects_when_a_parent_is_not_done() {
        let mut c = card(CardState::Todo);
        let error = todo_to_ready(&mut c, &[CardState::Done, CardState::Running])
            .expect_err("an unfinished parent must block readiness");
        assert!(matches!(error, KnowledgeError::IllegalTransition { .. }));
        assert_eq!(c.state, CardState::Todo);
    }

    #[test]
    fn test_todo_to_ready_rejects_non_todo_source() {
        let mut c = card(CardState::Triage);
        let error = todo_to_ready(&mut c, &[]).expect_err("triage -> ready must be illegal");
        assert!(matches!(error, KnowledgeError::IllegalTransition { .. }));
    }

    #[test]
    fn test_claim_succeeds_without_approval_when_risk_is_low() {
        let mut c = card(CardState::Ready);
        claim(&mut c, WorkId::from_str("work-1"), AgentId::new("worker-1"), RiskLevel::Low, None)
            .expect("low-risk claim needs no approval");
        assert_eq!(c.state, CardState::Running);
        assert_eq!(c.work_id, Some(WorkId::from_str("work-1")));
        assert_eq!(c.assignee, Some(AgentId::new("worker-1")));
    }

    #[test]
    fn test_claim_on_high_risk_lane_without_approval_is_refused() {
        let mut c = card(CardState::Ready);
        let error = claim(
            &mut c,
            WorkId::from_str("work-1"),
            AgentId::new("worker-1"),
            RiskLevel::High,
            None,
        )
        .expect_err("high-risk claim without approval must be refused");
        assert!(matches!(error, KnowledgeError::ClaimRequiresApproval { .. }));
        assert_eq!(c.state, CardState::Ready, "a refused claim must not mutate the card");
        assert!(c.work_id.is_none());
    }

    #[test]
    fn test_claim_on_high_risk_lane_with_rejected_proof_is_refused() {
        let mut c = card(CardState::Ready);
        let proof = ApprovalProof::new(ReviewDecision::Rejected, AgentId::new("operator-1"));
        let error = claim(
            &mut c,
            WorkId::from_str("work-1"),
            AgentId::new("worker-1"),
            RiskLevel::Critical,
            Some(&proof),
        )
        .expect_err("a rejected decision must not satisfy the gate");
        assert!(matches!(error, KnowledgeError::ClaimRequiresApproval { .. }));
    }

    #[test]
    fn test_claim_on_high_risk_lane_with_approved_proof_succeeds() {
        let mut c = card(CardState::Ready);
        let proof = ApprovalProof::new(ReviewDecision::Approved, AgentId::new("operator-1"));
        claim(
            &mut c,
            WorkId::from_str("work-1"),
            AgentId::new("worker-1"),
            RiskLevel::High,
            Some(&proof),
        )
        .expect("approved proof satisfies the gate");
        assert_eq!(c.state, CardState::Running);
    }

    #[test]
    fn test_claim_rejects_non_ready_source() {
        let mut c = card(CardState::Triage);
        let error = claim(&mut c, WorkId::from_str("work-1"), AgentId::new("worker-1"), RiskLevel::Low, None)
            .expect_err("triage -> running must be illegal");
        assert!(matches!(error, KnowledgeError::IllegalTransition { .. }));
    }

    #[test]
    fn test_complete_moves_a_plain_card_to_done() {
        let mut c = card(CardState::Running);
        complete(&mut c).expect("complete succeeds");
        assert_eq!(c.state, CardState::Done);
    }

    #[test]
    fn test_complete_moves_a_review_required_card_to_blocked_instead_of_done() {
        let mut c = card(CardState::Running);
        c.tags.push(REVIEW_REQUIRED_TAG.to_owned());
        complete(&mut c).expect("complete succeeds");
        assert_eq!(
            c.state,
            CardState::Blocked {
                reason_kind: BlockKind::ReviewRequired
            }
        );
    }

    #[test]
    fn test_complete_rejects_non_running_source() {
        let mut c = card(CardState::Ready);
        let error = complete(&mut c).expect_err("ready -> done must be illegal");
        assert!(matches!(error, KnowledgeError::IllegalTransition { .. }));
    }

    #[test]
    fn test_block_moves_running_to_blocked_with_given_reason() {
        let mut c = card(CardState::Running);
        block(&mut c, BlockKind::Dependency).expect("block succeeds");
        assert_eq!(
            c.state,
            CardState::Blocked {
                reason_kind: BlockKind::Dependency
            }
        );
    }

    #[test]
    fn test_block_rejects_non_running_source() {
        let mut c = card(CardState::Todo);
        let error = block(&mut c, BlockKind::Transient).expect_err("todo -> blocked must be illegal");
        assert!(matches!(error, KnowledgeError::IllegalTransition { .. }));
    }

    #[test]
    fn test_unblock_moves_blocked_to_ready() {
        let mut c = card(CardState::Blocked {
            reason_kind: BlockKind::NeedsInput,
        });
        unblock(&mut c).expect("unblock succeeds");
        assert_eq!(c.state, CardState::Ready);
    }

    #[test]
    fn test_unblock_rejects_non_blocked_source() {
        let mut c = card(CardState::Done);
        let error = unblock(&mut c).expect_err("done -> ready must be illegal");
        assert!(matches!(error, KnowledgeError::IllegalTransition { .. }));
    }

    #[test]
    fn test_reclaim_moves_running_to_ready_and_increments_retry_count() {
        let mut c = card(CardState::Running);
        c.work_id = Some(WorkId::from_str("work-1"));
        c.assignee = Some(AgentId::new("worker-1"));

        let first = reclaim(&mut c).expect("first reclaim succeeds");
        assert_eq!(first, 1);
        assert_eq!(c.state, CardState::Ready);
        assert!(c.work_id.is_none());
        assert!(c.assignee.is_none());

        // A second claim-then-reclaim cycle keeps counting.
        claim(&mut c, WorkId::from_str("work-2"), AgentId::new("worker-2"), RiskLevel::Low, None)
            .expect("re-claim after reclaim");
        let second = reclaim(&mut c).expect("second reclaim succeeds");
        assert_eq!(second, 2);
    }

    #[test]
    fn test_reclaim_rejects_non_running_source() {
        let mut c = card(CardState::Ready);
        let error = reclaim(&mut c).expect_err("ready -> ready (reclaim) must be illegal");
        assert!(matches!(error, KnowledgeError::IllegalTransition { .. }));
    }

    #[test]
    fn test_archive_succeeds_from_done_with_no_unresolved_children() {
        let mut c = card(CardState::Done);
        archive(&mut c, 0).expect("archive succeeds");
        assert_eq!(c.state, CardState::Archived);
    }

    #[test]
    fn test_archive_succeeds_from_blocked_with_no_unresolved_children() {
        let mut c = card(CardState::Blocked {
            reason_kind: BlockKind::Dependency,
        });
        archive(&mut c, 0).expect("archive succeeds");
        assert_eq!(c.state, CardState::Archived);
    }

    #[test]
    fn test_archive_rejects_unresolved_children() {
        let mut c = card(CardState::Done);
        let error = archive(&mut c, 2).expect_err("unresolved children must block archival");
        match error {
            KnowledgeError::ArchiveBlockedByChildren { blocking_children, .. } => {
                assert_eq!(blocking_children, 2);
            }
            other => panic!("expected ArchiveBlockedByChildren, got {other:?}"),
        }
        assert_eq!(c.state, CardState::Done, "a rejected archive must not mutate the card");
    }

    #[test]
    fn test_archive_rejects_non_terminal_source() {
        let mut c = card(CardState::Running);
        let error = archive(&mut c, 0).expect_err("running -> archived must be illegal");
        assert!(matches!(error, KnowledgeError::IllegalTransition { .. }));
    }
}
