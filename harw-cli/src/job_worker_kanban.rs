//! Kanban-Worker: `kanban_card`-Jobs mit Freigabe je Karte (Plan D2).
//!
//! # Ablauf
//! 1. **Tor vor dem Claim** ([`gate`]): nur mit Wissensspeicher
//!    ([`JobWorkerContext::knowledge`]). Die Freigabe gilt, wenn der
//!    `JobApproval`-Sidecar eine Kanban-Freigabe für genau die gelesene
//!    Revision ist (`harw_runtime::job_ledger::is_kanban_approval`) — also
//!    aus `/kanban approve`, nicht aus einem bloßen `unblock`/`/approve`.
//! 2. **Ohne Freigabe** ([`execute_kanban_claim`]): kein Modellaufruf. Der
//!    Verlauf der Karte bekommt „Freigabe angefragt" (Rolle, Risiko aus
//!    `harw_ops::kanban::role_risk`), der Job endet
//!    `Blocked { "kanban:AwaitingApproval" }`. Die TUI zeigt die Karte als
//!    wartend; `/kanban approve` bzw. `reject` entscheidet.
//! 3. **Mit Freigabe**: der Rollen-Agent läuft über den Plan-Knoten-Pfad des
//!    Workers (`EntryKind::JobPlanNode`, `RuntimeNarrowing`): lesende Rollen
//!    mit `ReadOnlyExplore` und `{Read}`, alle anderen mit `Full` und
//!    höchstens `{Read, Write}` — Prozess-, Netz- und Host-Rechte entstehen
//!    auf diesem Einstieg nie. Lease-Verlängerung (Heartbeat) übernimmt der
//!    `DurableJobRunner`; Token- und Wanduhr-Budget deckeln wie bei
//!    Prompt-Jobs (`effective_prompt_budget`).
//! 4. **Ergebnis**: Abschnitt „Ergebnis" und Verlaufseintrag an der Karte
//!    (`harw_knowledge::kanban::notes`), dann der Ledger-Übergang über den
//!    Job-Ausgang:
//!    - erledigt → `Succeeded` (Karte `done`); mit Tag `review-required`
//!      → `Blocked { kanban:ReviewRequired }` (wie `lifecycle::complete`);
//!    - Budget erschöpft mit Teilantwort (`budget_exhausted`) → „teilweise
//!      erledigt", `Blocked { kanban:NeedsInput }`;
//!    - Fehler → `Failed` (Karte `blocked (Transient)`);
//!    - pausierter Turn → `Blocked { kanban:NeedsInput }`.
//!
//! # Grenzen
//! Der Worker läuft in `harw serve`; sein Schreiben an der Karte erreicht
//! den `AgentEventHub` einer TUI in einem anderen Prozess nicht. Die TUI sieht
//! die Änderung beim nächsten Nachladen.
//!
//! # Fehler
//! Kein Pfad gibt `Err` zurück; jeder Fehlschlag ist ein [`JobOutcome`].
//! Ein Fehler beim Schreiben der Kartenanmerkungen wird geloggt und ändert den
//! Job-Ausgang nicht (das Ledger bleibt die Wahrheit).

use std::sync::Arc;

use harw_agent_dsl::roles::AgentRoleId;
use harw_core::{ExecutionControl, ModelProvider, StateStore};
use harw_job_runtime::{JobClaim, JobKind, JobOutcome, StoredJob, WorkId};
use harw_knowledge::KnowledgeStore;
use harw_knowledge::kanban::board::{self, BlockKind, BoardId, CardRecord, LaneKind};
use harw_knowledge::kanban::lifecycle::REVIEW_REQUIRED_TAG;
use harw_knowledge::kanban::notes::{
    self, CardNotes, CardResult, HistoryEntry, HistoryEvent, ResultStatus,
};
use harw_ops::kanban::{RoleAccess, risk_label, role_access};
use harw_plan::PlanNodeKind;
use harw_registry_defaults::profile::{IdentityOverrides, RegistryProfile};
use harw_runtime::RuntimeNarrowing;
use harw_runtime::job_ledger::{
    KANBAN_BLOCK_REASON_PREFIX, is_kanban_approval, is_kanban_job_kind,
};
use harw_session_store::JobStore;
use harw_types::ApprovalActor;
use jiff::{SignedDuration, Timestamp};

use super::{
    BudgetedModelProvider, JobWorkerContext, MISSING_RUNTIME_ROOT, PauseDisposition,
    PromptTokenLedger, WORKER_ID, WorkerExecutionControl, assemble_job_turn, check_claim_fence,
    durable_session_id, effective_prompt_budget, execute_turn, job_state_store,
    last_assistant_text,
};
use crate::runtime_jobs::{JobAssemblyInputs, JobEntry, job_principal, job_sandbox};

/// Höchstzahl der Kommentare, die der Auftrag an den Agenten mitnimmt.
const PROMPT_COMMENTS: usize = 10;

/// Obergrenze des Kartentexts im Auftrag in Bytes.
const PROMPT_BODY_BYTES: usize = 16 * 1024;

/// Platzhalter von `last_assistant_text` für „keine Antwort".
const NO_ASSISTANT_TEXT: &str = "(no assistant text)";

/// `true` für die Job-Art der Kanban-Karten.
pub(super) fn is_kanban_kind(kind: &JobKind) -> bool {
    is_kanban_job_kind(kind)
}

/// Ergebnis der Prüfung vor dem Claim.
#[derive(Debug, Clone)]
pub(super) struct KanbanGate {
    /// Wissensspeicher mit der Karte.
    knowledge: Arc<KnowledgeStore>,
    /// Wer freigegeben hat; `None` = keine gültige Freigabe (anfragen).
    approved_by: Option<String>,
}

/// Prüft vor dem Claim, ob ein `kanban_card`-Job bearbeitet werden kann.
///
/// # Rückgabe
/// `None`, wenn der Worker keinen Wissensspeicher hat (Job bleibt liegen);
/// sonst das Tor mit oder ohne gültige Freigabe. Ein unlesbarer
/// Freigabe-Sidecar zählt als „keine Freigabe" (fail-closed).
pub(super) fn gate(
    store: &JobStore,
    record: &StoredJob,
    context: &JobWorkerContext,
) -> Option<KanbanGate> {
    let knowledge = Arc::clone(context.knowledge.as_ref()?);
    let approved_by = match store.get_approval(&record.job.id) {
        Ok(Some(approval)) if is_kanban_approval(&approval, record.revision) => {
            Some(actor_label(&approval.approved_by))
        }
        Ok(_) => None,
        Err(error) => {
            tracing::warn!(
                work_id = %record.job.id.as_str(),
                error = %error,
                "kanban approval sidecar unreadable; asking again"
            );
            None
        }
    };
    Some(KanbanGate {
        knowledge,
        approved_by,
    })
}

fn actor_label(actor: &ApprovalActor) -> String {
    match actor {
        ApprovalActor::Operator { id } => id.clone(),
        ApprovalActor::ChannelPeer { channel, peer } => {
            format!("{}:{}", channel.as_str(), peer.as_str())
        }
    }
}

/// Die Karte eines Jobs samt Board und Rolle der Worker-Lane.
struct BoundCard {
    board_id: BoardId,
    record: CardRecord,
    role: String,
}

/// Findet Karte und Rolle zu `work_id`.
fn bound_card(store: &KnowledgeStore, work_id: &WorkId) -> Result<BoundCard, String> {
    let (board_id, record) = notes::find_card_by_work_id(store, work_id)
        .map_err(|error| format!("kanban: card lookup failed: {error}"))?
        .ok_or_else(|| "kanban: no card is bound to this job".to_owned())?;
    let (_, lanes) = board::load_board(store, &board_id)
        .map_err(|error| format!("kanban: board {board_id} unreadable: {error}"))?;
    let role = lanes
        .into_iter()
        .find_map(|lane| match lane.kind {
            LaneKind::Worker { agent_role } if lane.id == record.lane_id => {
                Some(agent_role.as_str().to_owned())
            }
            _ => None,
        })
        .ok_or_else(|| format!("kanban: lane {} is not a worker lane", record.lane_id))?;
    Ok(BoundCard {
        board_id,
        record,
        role,
    })
}

/// `Blocked`-Ausgang mit Kanban-Grund (vom Ledger wieder als [`BlockKind`] gelesen).
fn blocked(kind: BlockKind) -> JobOutcome {
    JobOutcome::Blocked {
        reason: format!("{KANBAN_BLOCK_REASON_PREFIX}{}", kind.label()),
    }
}

/// Schreibt Verlauf (und optional Ergebnis) an die Karte; Fehler nur loggen.
fn annotate(
    store: &KnowledgeStore,
    card: &BoundCard,
    entry: HistoryEntry,
    result: Option<CardResult>,
) {
    let written = notes::update_card(
        store,
        &card.board_id,
        &card.record.id,
        Timestamp::now(),
        |_, card_notes| {
            card_notes.record(entry);
            if let Some(result) = result {
                card_notes.result = Some(result);
            }
            Ok(())
        },
    );
    if let Err(error) = written {
        tracing::warn!(
            card = %card.record.id,
            board = %card.board_id,
            error = %error,
            "kanban card annotation could not be written"
        );
    }
}

/// Führt einen geclaimten `kanban_card`-Job aus (siehe Moduldoku).
pub(super) async fn execute_kanban_claim(
    claim: JobClaim,
    gate: KanbanGate,
    provider: Arc<dyn ModelProvider>,
    job_store: Arc<JobStore>,
    control: Arc<WorkerExecutionControl>,
    context: &JobWorkerContext,
) -> JobOutcome {
    let work_id = claim.job.id.clone();
    if let Err(reason) = check_claim_fence(&claim) {
        tracing::warn!(work_id = %work_id.as_str(), reason = %reason, "kanban job rejected before any model call");
        return JobOutcome::Failed { reason };
    }
    let store = gate.knowledge.as_ref();
    let card = match bound_card(store, &work_id) {
        Ok(card) => card,
        Err(reason) => {
            tracing::warn!(work_id = %work_id.as_str(), reason = %reason, "kanban job has no usable card");
            return JobOutcome::Failed { reason };
        }
    };
    let access = role_access(&card.role);
    let risk = risk_label(access.risk());

    let Some(approved_by) = gate.approved_by else {
        annotate(
            store,
            &card,
            HistoryEntry::new(
                Timestamp::now(),
                HistoryEvent::ApprovalRequested,
                WORKER_ID,
                format!(
                    "Rolle {}, Risiko {risk} — /kanban approve {} oder reject",
                    card.role, card.record.id
                ),
                Some(&work_id),
            ),
            None,
        );
        tracing::info!(work_id = %work_id.as_str(), card = %card.record.id, "kanban card awaits approval");
        return blocked(BlockKind::AwaitingApproval);
    };

    let submitter_id = match claim.scope.submitter() {
        ApprovalActor::Operator { id } if super::is_scope_identifier(id) => id.clone(),
        _ => {
            return JobOutcome::Failed {
                reason: "scope: the kanban job submitter is not a valid operator".to_owned(),
            };
        }
    };
    let Some(runtime_root) = context.runtime_root.as_ref() else {
        annotate(
            store,
            &card,
            HistoryEntry::new(
                Timestamp::now(),
                HistoryEvent::Blocked,
                WORKER_ID,
                MISSING_RUNTIME_ROOT,
                Some(&work_id),
            ),
            None,
        );
        return blocked(BlockKind::Capability);
    };

    let (entry, profile) = match access {
        RoleAccess::ReadOnly => (
            JobEntry::PlanNode {
                kind: PlanNodeKind::Explore,
                may_write: false,
            },
            RegistryProfile::ReadOnlyExplore,
        ),
        RoleAccess::Write | RoleAccess::Host => (
            JobEntry::PlanNode {
                kind: PlanNodeKind::Coding,
                may_write: true,
            },
            RegistryProfile::Full,
        ),
    };
    let sandbox = match job_sandbox(entry, &runtime_root.cwd) {
        Ok(sandbox) => sandbox,
        Err(reason) => {
            tracing::error!(work_id = %work_id.as_str(), reason = %reason, "kanban job sandbox failed");
            return finish_failed(store, &card, &work_id, super::sanitize_failure(&reason));
        }
    };
    let card_notes = notes::load_notes(store, &card.board_id, &card.record.id).unwrap_or_default();
    let narrowing = RuntimeNarrowing {
        registry_profile: profile,
        identity: kanban_identity(&card, profile),
        permissions: sandbox.permissions().clone(),
        workspace_root: Some(sandbox.workspace().canonical_root().to_path_buf()),
    };

    let budget = effective_prompt_budget(&claim.job.budget);
    let wall = budget.max_wall.map_or(SignedDuration::ZERO, |max| {
        max.saturating_sub(claim.job.usage.wall)
    });
    let Ok(wall) = std::time::Duration::try_from(wall) else {
        return finish_failed(
            store,
            &card,
            &work_id,
            "budget: the wall-clock budget is already exhausted".to_owned(),
        );
    };
    if wall.is_zero() {
        return finish_failed(
            store,
            &card,
            &work_id,
            "budget: the wall-clock budget is already exhausted".to_owned(),
        );
    }
    let ledger = PromptTokenLedger::new(budget, claim.job.usage.clone());
    let budgeted: Arc<dyn ModelProvider> = Arc::new(BudgetedModelProvider {
        inner: provider,
        ledger: Arc::clone(&ledger),
    });

    let session_id = durable_session_id(&claim);
    let state_store: Arc<dyn StateStore> = job_state_store(&context.transcript_root);
    let assembled = assemble_job_turn(
        JobAssemblyInputs {
            entry,
            home: &runtime_root.home,
            cwd: sandbox.workspace().canonical_root(),
            principal: job_principal(&submitter_id),
            session_id: session_id.clone(),
            state_store: Arc::clone(&state_store),
            job_store,
            model: budgeted,
            narrowing: Some(narrowing),
        },
        PauseDisposition::Blocked,
        Some(&sandbox),
    );
    let (setup, model) = match assembled {
        Ok(assembled) => assembled,
        Err(reason) => return finish_failed(store, &card, &work_id, reason),
    };

    annotate(
        store,
        &card,
        HistoryEntry::new(
            Timestamp::now(),
            HistoryEvent::Started,
            WORKER_ID,
            format!(
                "Rolle {}, Risiko {risk}, freigegeben von {approved_by}",
                card.role
            ),
            Some(&work_id),
        ),
        None,
    );
    tracing::info!(
        work_id = %work_id.as_str(),
        card = %card.record.id,
        role = %card.role,
        profile = ?profile,
        "kanban card agent starting"
    );

    let prompt = kanban_prompt(&card, &card_notes);
    let turn = execute_turn(claim, prompt, model, Arc::clone(&control), setup);
    let outcome = match tokio::time::timeout(wall, turn).await {
        Ok(outcome @ JobOutcome::Cancelled { .. }) => outcome,
        Ok(outcome) => match ledger.exceeded() {
            Some(reason) => budget_outcome(reason, state_store.as_ref(), &session_id).await,
            None => outcome,
        },
        Err(_elapsed) => {
            let reason = "budget: wall-clock limit exceeded".to_owned();
            ledger.close(reason.clone());
            control.force_abort();
            control.clear_abort_handle();
            budget_outcome(reason, state_store.as_ref(), &session_id).await
        }
    };
    report(store, &card, &work_id, outcome)
}

/// Ausgang nach erschöpftem Budget: Teilergebnis, wenn der Agent schon
/// geantwortet hat (`budget_exhausted`), sonst Fehlschlag.
async fn budget_outcome(
    reason: String,
    state_store: &dyn StateStore,
    session_id: &harw_types::SessionId,
) -> JobOutcome {
    let partial = state_store
        .load_history(session_id)
        .await
        .ok()
        .map(|history| last_assistant_text(&history))
        .filter(|text| text != NO_ASSISTANT_TEXT);
    match partial {
        Some(text) => JobOutcome::Succeeded {
            result: serde_json::json!({
                "assistant": text,
                "budget_exhausted": true,
                "reason": reason,
            }),
        },
        None => JobOutcome::Failed { reason },
    }
}

/// Fehlschlag vor dem Turn: Ergebnis „fehlgeschlagen" an die Karte.
fn finish_failed(
    store: &KnowledgeStore,
    card: &BoundCard,
    work_id: &WorkId,
    reason: String,
) -> JobOutcome {
    report(store, card, work_id, JobOutcome::Failed { reason })
}

/// Schreibt Ergebnis und Verlauf an die Karte und bildet den Job-Ausgang auf
/// den Ledger-Übergang ab (siehe Moduldoku, Schritt 4).
fn report(
    store: &KnowledgeStore,
    card: &BoundCard,
    work_id: &WorkId,
    outcome: JobOutcome,
) -> JobOutcome {
    let now = Timestamp::now();
    let role = Some(card.role.clone());
    match outcome {
        JobOutcome::Succeeded { result } => {
            let summary = result
                .get("assistant")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .trim()
                .to_owned();
            let partial = result
                .get("budget_exhausted")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if partial {
                annotate(
                    store,
                    card,
                    HistoryEntry::new(
                        now,
                        HistoryEvent::PartiallyCompleted,
                        WORKER_ID,
                        result
                            .get("reason")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("budget exhausted"),
                        Some(work_id),
                    ),
                    Some(CardResult::new(
                        now,
                        ResultStatus::Partial,
                        &summary,
                        role,
                        Some(work_id),
                    )),
                );
                return blocked(BlockKind::NeedsInput);
            }
            let review = card
                .record
                .tags
                .iter()
                .any(|tag| tag == REVIEW_REQUIRED_TAG);
            annotate(
                store,
                card,
                HistoryEntry::new(
                    now,
                    if review {
                        HistoryEvent::Blocked
                    } else {
                        HistoryEvent::Completed
                    },
                    WORKER_ID,
                    if review { "Review nötig" } else { "" },
                    Some(work_id),
                ),
                Some(CardResult::new(
                    now,
                    ResultStatus::Succeeded,
                    &summary,
                    role,
                    Some(work_id),
                )),
            );
            if review {
                blocked(BlockKind::ReviewRequired)
            } else {
                JobOutcome::Succeeded {
                    result: serde_json::json!({
                        "source": "kanban-worker",
                        "board": card.board_id.as_str(),
                        "card": card.record.id.as_str(),
                        "summary": notes::clip(&summary, super::MAX_REASON_BYTES),
                    }),
                }
            }
        }
        JobOutcome::Failed { reason } => {
            annotate(
                store,
                card,
                HistoryEntry::new(now, HistoryEvent::Failed, WORKER_ID, &reason, Some(work_id)),
                Some(CardResult::new(
                    now,
                    ResultStatus::Failed,
                    &reason,
                    role,
                    Some(work_id),
                )),
            );
            JobOutcome::Failed { reason }
        }
        JobOutcome::Blocked { reason } => {
            annotate(
                store,
                card,
                HistoryEntry::new(
                    now,
                    HistoryEvent::Blocked,
                    WORKER_ID,
                    &reason,
                    Some(work_id),
                ),
                None,
            );
            blocked(BlockKind::NeedsInput)
        }
        JobOutcome::Cancelled { reason } => {
            annotate(
                store,
                card,
                HistoryEntry::new(
                    now,
                    HistoryEvent::Failed,
                    WORKER_ID,
                    format!("abgebrochen: {reason}"),
                    Some(work_id),
                ),
                None,
            );
            JobOutcome::Cancelled { reason }
        }
    }
}

/// Identität des Rollen-Agenten einer Karte.
fn kanban_identity(card: &BoundCard, profile: RegistryProfile) -> IdentityOverrides {
    IdentityOverrides {
        agent_name: Some(format!("kanban-{}", card.role)),
        role_description: Some(format!(
            "{} working kanban card '{}' as role '{}'",
            profile.role_description(),
            card.record.id,
            card.role
        )),
        extra_context: vec![
            "You run unattended inside a durable job. There is no user: an approval request ends the job as blocked."
                .to_owned(),
            "The operator approved this card before you started. Finish with a concise summary of what you did and what is left."
                .to_owned(),
        ],
        organizational_role: Some(AgentRoleId::Worker),
    }
}

/// Der Auftrag an den Rollen-Agenten: Titel, Text, Belege, letzte Kommentare.
fn kanban_prompt(card: &BoundCard, card_notes: &CardNotes) -> String {
    let mut prompt = format!(
        "You are working kanban card '{}' on board '{}' as role '{}'.\n\nTitle:\n{}\n",
        card.record.id, card.board_id, card.role, card.record.title
    );
    let body = card.record.body.trim();
    if body.is_empty() {
        prompt.push_str("\nDescription: none was recorded.\n");
    } else {
        prompt.push_str(&format!(
            "\nDescription:\n{}\n",
            notes::clip(body, PROMPT_BODY_BYTES)
        ));
    }
    if !card_notes.evidence.is_empty() {
        prompt.push_str("\nEvidence references:\n");
        for reference in &card_notes.evidence {
            prompt.push_str(&format!("- {reference}\n"));
        }
    }
    if !card_notes.comments.is_empty() {
        prompt.push_str("\nRecent comments:\n");
        let skipped = card_notes.comments.len().saturating_sub(PROMPT_COMMENTS);
        for comment in card_notes.comments.iter().skip(skipped) {
            prompt.push_str(&format!("- {}: {}\n", comment.author, comment.text));
        }
    }
    if let Some(result) = &card_notes.result {
        prompt.push_str(&format!(
            "\nPrevious run ({}):\n{}\n",
            result.status.key(),
            notes::clip(result.summary.trim(), 2 * 1024)
        ));
    }
    prompt.push_str(
        "\nThis turn runs unattended as a durable job. No user is available, so do not ask for \
         approval or confirmation. When you are done, reply with a short result summary: what \
         you did, which files or sources matter, and what remains open.\n",
    );
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_knowledge::kanban::board::{CardId, LaneId};
    use harw_knowledge::{AgentId, VisibilityScope};

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn bound(store: &KnowledgeStore, tags: Vec<String>) -> TestResult<BoundCard> {
        let board_id = BoardId::new("default");
        let record = CardRecord {
            id: CardId::new("card-1"),
            lane_id: LaneId::new("worker/explorer"),
            title: "Parser prüfen".to_owned(),
            body: "Bitte die Fehlerpfade ansehen.".to_owned(),
            work_id: Some(WorkId::from_str("work-1")),
            parents: Vec::new(),
            tags,
            visibility: VisibilityScope::OperatorOnly,
        };
        board::save_card(
            store,
            &board_id,
            &record,
            &AgentId::new("operator"),
            Timestamp::now(),
        )?;
        Ok(BoundCard {
            board_id,
            record,
            role: "explorer".to_owned(),
        })
    }

    fn temporary_store() -> Result<(tempfile::TempDir, KnowledgeStore), std::io::Error> {
        let dir = tempfile::tempdir()?;
        let store = KnowledgeStore::new(dir.path());
        Ok((dir, store))
    }

    #[test]
    fn success_writes_the_result_and_completes() -> TestResult {
        let (_dir, store) = temporary_store()?;
        let card = bound(&store, Vec::new())?;
        let outcome = report(
            &store,
            &card,
            &WorkId::from_str("work-1"),
            JobOutcome::Succeeded {
                result: serde_json::json!({ "assistant": "Drei Fehlerpfade gefunden." }),
            },
        );
        assert!(matches!(outcome, JobOutcome::Succeeded { .. }));
        let card_notes = notes::load_notes(&store, &card.board_id, &card.record.id)?;
        let result = card_notes.result.ok_or("result missing")?;
        assert_eq!(result.status, ResultStatus::Succeeded);
        assert_eq!(result.summary, "Drei Fehlerpfade gefunden.");
        assert_eq!(card_notes.history[0].event, HistoryEvent::Completed);
        Ok(())
    }

    #[test]
    fn budget_exhausted_is_partial_and_needs_input() -> TestResult {
        let (_dir, store) = temporary_store()?;
        let card = bound(&store, Vec::new())?;
        let outcome = report(
            &store,
            &card,
            &WorkId::from_str("work-1"),
            JobOutcome::Succeeded {
                result: serde_json::json!({
                    "assistant": "Halb fertig.",
                    "budget_exhausted": true,
                    "reason": "budget: token limit",
                }),
            },
        );
        assert_eq!(
            outcome,
            JobOutcome::Blocked {
                reason: "kanban:NeedsInput".to_owned()
            }
        );
        let card_notes = notes::load_notes(&store, &card.board_id, &card.record.id)?;
        assert_eq!(
            card_notes.result.map(|result| result.status),
            Some(ResultStatus::Partial)
        );
        assert_eq!(
            card_notes.history[0].event,
            HistoryEvent::PartiallyCompleted
        );
        Ok(())
    }

    #[test]
    fn review_required_cards_block_instead_of_completing() -> TestResult {
        let (_dir, store) = temporary_store()?;
        let card = bound(&store, vec![REVIEW_REQUIRED_TAG.to_owned()])?;
        let outcome = report(
            &store,
            &card,
            &WorkId::from_str("work-1"),
            JobOutcome::Succeeded {
                result: serde_json::json!({ "assistant": "fertig" }),
            },
        );
        assert_eq!(
            outcome,
            JobOutcome::Blocked {
                reason: "kanban:ReviewRequired".to_owned()
            }
        );
        Ok(())
    }

    #[test]
    fn failure_is_recorded_as_failed_result() -> TestResult {
        let (_dir, store) = temporary_store()?;
        let card = bound(&store, Vec::new())?;
        let outcome = finish_failed(
            &store,
            &card,
            &WorkId::from_str("work-1"),
            "kaputt".to_owned(),
        );
        assert!(matches!(outcome, JobOutcome::Failed { .. }));
        let card_notes = notes::load_notes(&store, &card.board_id, &card.record.id)?;
        assert_eq!(
            card_notes.result.map(|result| result.status),
            Some(ResultStatus::Failed)
        );
        Ok(())
    }

    #[test]
    fn prompt_carries_card_content() -> TestResult {
        let (_dir, store) = temporary_store()?;
        let card = bound(&store, Vec::new())?;
        let mut card_notes = CardNotes::default();
        card_notes.add_evidence("docs/a.md")?;
        card_notes.add_comment(Timestamp::now(), "operator", "Achte auf Unicode")?;
        let prompt = kanban_prompt(&card, &card_notes);
        assert!(prompt.contains("Parser prüfen"));
        assert!(prompt.contains("Bitte die Fehlerpfade ansehen."));
        assert!(prompt.contains("- docs/a.md"));
        assert!(prompt.contains("operator: Achte auf Unicode"));
        Ok(())
    }
}
