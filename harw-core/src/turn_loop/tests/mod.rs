use super::*;

/// Test submitter: records every submitted child and answers with a fresh
/// durable work id (stands in for the runtime's background executor).
struct RecordingJobSubmitter {
    submitted: Mutex<Vec<(SessionId, Option<String>)>>,
}

impl RecordingJobSubmitter {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            submitted: Mutex::new(Vec::new()),
        })
    }

    fn submitted(&self) -> Vec<(SessionId, Option<String>)> {
        self.submitted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl harw_extension_api::AgentJobSubmitter for RecordingJobSubmitter {
    fn submit_child<'a>(
        &'a self,
        child: &'a SessionId,
        task: Option<&'a str>,
    ) -> harw_extension_api::AgentJobFuture<'a> {
        self.submitted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push((child.clone(), task.map(ToOwned::to_owned)));
        let child = child.clone();
        Box::pin(async move {
            Ok(harw_extension_api::AgentJobHandle {
                work_id: harw_types::WorkId::new(),
                child,
            })
        })
    }
}

/// Session with a spawner and (optionally) a background job submitter.
fn delegation_session_with(
    spawner: Arc<dyn AgentSpawner>,
    submitter: Option<Arc<RecordingJobSubmitter>>,
) -> TestResult<AgentSession> {
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut builder = ExtensionRegistryBuilder::default().spawner(spawner);
    if let Some(submitter) = submitter {
        builder = builder.agent_job_submitter(submitter);
    }
    Ok(
        AgentSession::new(AgentRole::Assistant, None, builder.build(), tx)
            .with_spawn_context(test_spawn_context()?),
    )
}

#[test]
fn background_only_refusal_rejects_legacy_inline_flags_and_missing_executor() -> TestResult {
    let spawner: Arc<dyn AgentSpawner> = Arc::new(FixedChildSpawner {
        child: SessionId::new(),
    });
    let with =
        delegation_session_with(Arc::clone(&spawner), Some(RecordingJobSubmitter::new()))?;
    let without = delegation_session_with(spawner, None)?;

    // Admissible: no flags, or the harmless legacy values.
    for arguments in [
        serde_json::json!({}),
        serde_json::json!({"background": true}),
        serde_json::json!({"wait": false}),
    ] {
        assert!(
            background_only_refusal(&with, &arguments).is_none(),
            "{arguments}"
        );
    }
    // Legacy inline requests get a detailed error.
    for arguments in [
        serde_json::json!({"background": false}),
        serde_json::json!({"wait": true}),
    ] {
        let Some(ToolCallResult::Error { message }) =
            background_only_refusal(&with, &arguments)
        else {
            return Err(TestError::Unexpected(format!("no refusal for {arguments}")));
        };
        assert!(message.contains("background-only"), "{message}");
        assert!(
            message.contains("delivered to you automatically"),
            "{message}"
        );
    }
    // No executor: error, never a blocking fallback.
    let Some(ToolCallResult::Error { message }) =
        background_only_refusal(&without, &serde_json::json!({}))
    else {
        return Err(TestError::Unexpected(
            "no refusal without executor".to_owned(),
        ));
    };
    assert!(
        message.contains("no background executor is configured"),
        "{message}"
    );
    Ok(())
}

#[test]
fn start_result_is_a_one_time_confirmation() -> TestResult {
    let handle = harw_extension_api::AgentJobHandle {
        work_id: harw_types::WorkId::new(),
        child: SessionId::new(),
    };
    let ToolCallResult::Success { value } = agent_job_started_result(&handle, "worker") else {
        return Err(TestError::Unexpected("start must succeed".to_owned()));
    };
    assert_eq!(value["child_id"], serde_json::json!(handle.child.as_str()));
    assert_eq!(value["work_id"], serde_json::json!(handle.work_id.as_str()));
    assert_eq!(value["role"], serde_json::json!("worker"));
    assert_eq!(value["status"], serde_json::json!("started"));
    let hint = value["hint"].as_str().unwrap_or_default();
    assert!(hint.contains("delivered to you automatically"), "{hint}");
    assert!(!hint.contains("work.result"), "no polling advice: {hint}");
    Ok(())
}

fn started_child_ids(session: &AgentSession) -> Vec<String> {
    tool_results(session)
        .into_iter()
        .filter_map(|result| match result {
            ToolCallResult::Success { value } => {
                value["child_id"].as_str().map(ToOwned::to_owned)
            }
            ToolCallResult::Error { .. } => None,
        })
        .collect()
}

#[tokio::test]
async fn worker_delegation_starts_in_the_background_and_the_turn_continues() -> TestResult {
    let child = SessionId::new();
    let submitter = RecordingJobSubmitter::new();
    let mut session = delegation_session_with(
        Arc::new(FixedChildSpawner {
            child: child.clone(),
        }),
        Some(Arc::clone(&submitter)),
    )?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![
        response_with(vec![ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("transfer_to_worker"),
            arguments: serde_json::json!({"task": "do it"}),
        }]),
        crate::model::ModelResponse::text("started, continuing"),
    ]);
    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("go"))
        .await
        .map_err(ctx("the start is a one-time return"))?;
    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    assert_eq!(
        submitter.submitted(),
        vec![(child.clone(), Some("do it".to_owned()))]
    );
    assert_eq!(started_child_ids(&session), vec![child.as_str().to_owned()]);
    Ok(())
}

#[tokio::test]
async fn delegation_without_executor_fails_fast_and_spawns_nothing() -> TestResult {
    let spawner = Arc::new(CapturingSpawner {
        child: SessionId::new(),
        targets: vec!["worker".to_owned()],
        captured: Mutex::new(None),
    });
    let mut session = delegation_session_with(spawner.clone(), None)?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![
        response_with(vec![ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("transfer_to_worker"),
            arguments: serde_json::json!({"task": "do it"}),
        }]),
        crate::model::ModelResponse::text("understood"),
    ]);
    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("go"))
        .await
        .map_err(ctx("a refused start is a tool error, not a pause"))?;
    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    assert!(
        spawner
            .captured
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_none(),
        "no child may be spawned"
    );
    expect_single_error(
        &session,
        "Delegation is background-only and no background executor",
    )
}

#[tokio::test]
async fn legacy_inline_flags_fail_with_a_detailed_error() -> TestResult {
    for flags in [
        serde_json::json!({"background": false}),
        serde_json::json!({"wait": true}),
    ] {
        let submitter = RecordingJobSubmitter::new();
        let mut session = delegation_session_with(
            Arc::new(FixedChildSpawner {
                child: SessionId::new(),
            }),
            Some(Arc::clone(&submitter)),
        )?;
        let store = crate::state_store::InMemoryStateStore::new();
        let mut arguments = serde_json::json!({"task": "do it"});
        if let (Some(target), Some(extra)) = (arguments.as_object_mut(), flags.as_object()) {
            target.extend(extra.clone());
        }
        let model = ScriptedModel::new(vec![
            response_with(vec![ToolCall {
                id: ToolCallId::new(),
                name: ToolName::new("transfer_to_worker"),
                arguments,
            }]),
            crate::model::ModelResponse::text("understood"),
        ]);
        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("go"))
            .await
            .map_err(ctx("legacy flags are a tool error"))?;
        assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
        assert!(
            submitter.submitted().is_empty(),
            "nothing may start for {flags}"
        );
        expect_single_error(&session, "Delegation is background-only: the parameters")?;
    }
    Ok(())
}
#[tokio::test]
async fn host_background_launcher_counts_as_executor_and_yields_awaiting_child() -> TestResult {
    let child = SessionId::new();
    let mut session = delegation_session_with(
        Arc::new(FixedChildSpawner {
            child: child.clone(),
        }),
        None,
    )?;
    session.set_host_background_launcher(true);
    assert!(background_only_refusal(&session, &serde_json::json!({})).is_none());
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![response_with(vec![ToolCall {
        id: ToolCallId::new(),
        name: ToolName::new("transfer_to_worker"),
        arguments: serde_json::json!({"task": "do it"}),
    }])]);
    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("go"))
        .await
        .map_err(ctx("the host launcher detaches the pause"))?;
    let TurnOutcome::AwaitingChild {
        child: paused,
        role,
        ..
    } = outcome
    else {
        return Err(TestError::Unexpected(format!(
            "expected AwaitingChild, got {outcome:?}"
        )));
    };
    assert_eq!(paused, child);
    assert_eq!(role, "worker");
    Ok(())
}

struct FailingJobSubmitter;

impl harw_extension_api::AgentJobSubmitter for FailingJobSubmitter {
    fn submit_child<'a>(
        &'a self,
        _child: &'a SessionId,
        _task: Option<&'a str>,
    ) -> harw_extension_api::AgentJobFuture<'a> {
        Box::pin(async {
            Err(harw_extension_api::AgentSpawnError::new(
                "job store unavailable",
            ))
        })
    }
}

#[tokio::test]
async fn failed_submit_releases_the_child_and_returns_a_tool_error() -> TestResult {
    let spawner = Arc::new(CompletionTrackingSpawner {
        child_completed: AtomicUsize::new(0),
        child_finished: AtomicUsize::new(0),
        events: Arc::new(Mutex::new(Vec::new())),
    });
    let (tx, _rx) = mpsc::unbounded_channel();
    let registry = ExtensionRegistryBuilder::default()
        .spawner(spawner.clone())
        .agent_job_submitter(Arc::new(FailingJobSubmitter))
        .build();
    let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx)
        .with_spawn_context(test_spawn_context()?);
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![
        response_with(vec![ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("transfer_to_worker"),
            arguments: serde_json::json!({"task": "do it"}),
        }]),
        crate::model::ModelResponse::text("understood"),
    ]);
    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("go"))
        .await
        .map_err(ctx("a failed submit is a tool error, not a turn abort"))?;
    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    assert_eq!(
        spawner.child_finished.load(Ordering::SeqCst),
        1,
        "the admitted child must be released"
    );
    expect_single_error(
        &session,
        "The 'worker' agent was admitted but could not be started",
    )
}

/// Ein Turn-Grenzen-Ende vermerkt die konkrete Grenze mit Wert (statt
/// „Token-Budget oder Turn-Wächter"); der erste Grund gewinnt.
#[test]
fn budget_checkpoints_record_the_concrete_limit() {
    let rounds = TurnControl::new().with_limits(TurnLimits {
        max_model_rounds: 0,
        ..TurnLimits::unlimited()
    });
    assert_eq!(rounds.stop_detail(), None);
    assert_eq!(rounds.model_checkpoint(), Some(CancelReason::Budget));
    assert_eq!(
        rounds.stop_detail().as_deref(),
        Some("Rundenlimit des Turns erreicht (0/0 Modellrunden)")
    );
    rounds.note_stop_detail("später");
    assert!(
        rounds
            .stop_detail()
            .is_some_and(|d| d.starts_with("Rundenlimit"))
    );

    let tools = TurnControl::new().with_limits(TurnLimits {
        max_tool_calls: 2,
        ..TurnLimits::unlimited()
    });
    assert_eq!(tools.tool_checkpoint(2), None);
    assert_eq!(tools.tool_checkpoint(3), Some(CancelReason::Budget));
    // Klone teilen den Grund (so liest ihn der Kind-Spawner).
    assert_eq!(
        tools.clone().stop_detail().as_deref(),
        Some("Werkzeug-Limit des Turns erreicht (3/2 Aufrufe)")
    );

    let budget = TurnControl::new().with_token_budget(TurnTokenBudget::new(100, 150));
    assert_eq!(budget.model_checkpoint(), Some(CancelReason::Budget));
    assert_eq!(
        budget.stop_detail().as_deref(),
        Some("Token-Budget erschöpft (150 von 100 neuen Tokens; Cache-Lesungen zählen nicht)")
    );
}
use crate::activation::{SessionActivation, ToolProfile};
use crate::session::AgentSession;
use crate::test_support::{TestError, TestResult, ctx};
use harw_authority::SandboxSpec;
use harw_catalog::AgentSuggestions;
use harw_extension_api::contributors::ToolProvider;
use harw_extension_api::{
    AgentSpawnError, AgentSpawner, ExtensionRegistryBuilder, SpawnFuture, SpawnInput,
};
use harw_tools::{
    FunctionToolSpec, JsonSchema, ToolCall, ToolExecutionContext, ToolExecutor,
    ToolExecutorFuture, ToolName, ToolOutput, ToolSpec, ToolsError,
};
use harw_types::{AgentRole, TurnId};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

struct LoadFailStore;

#[test]
fn approval_resolution_timed_out_is_a_reject_with_the_canonical_reason() {
    assert_eq!(
        ApprovalResolution::timed_out(),
        ApprovalResolution::Reject {
            reason: crate::session::APPROVAL_TIMEOUT_REASON.to_owned(),
        }
    );
}

#[test]
fn sequence_exhaustion_rejects_the_turn() {
    let session = SessionId::new();
    let error = state_store_error(
        StateStoreError::SequenceExhausted {
            session: session.clone(),
        },
        "persistence",
    );

    assert!(matches!(
        error,
        CoreError::TurnRejected(reason)
            if reason.contains("state store persistence failed")
                && reason.contains(session.as_str())
    ));
}

impl StateStore for LoadFailStore {
    fn save_turn<'a>(
        &'a self,
        _sid: &'a SessionId,
        _item: &'a TurnItem,
    ) -> harw_extension_api::ExtFuture<'a, crate::state_store::StateStoreResult<()>> {
        Box::pin(async { Ok(()) })
    }

    fn load_history<'a>(
        &'a self,
        _sid: &'a SessionId,
    ) -> harw_extension_api::ExtFuture<
        'a,
        crate::state_store::StateStoreResult<crate::history::ConversationHistory>,
    > {
        Box::pin(async {
            Err(crate::state_store::StateStoreError::PoisonedMutex {
                operation: "test_load_history",
            })
        })
    }
}

// Always fails with a retryable `ModelError::RateLimited`, so tests can
// assert `transition_after_turn_failure` returns the session to `Idle`.
struct RateLimitedModel;

impl ModelProvider for RateLimitedModel {
    fn respond<'a>(&'a self, _request: ModelRequest) -> crate::model::ModelFuture<'a> {
        Box::pin(async {
            Err(crate::model::ModelError::RateLimited {
                retry_after_secs: 30,
                message: "test provider limit".to_owned(),
            })
        })
    }
}

// Always fails with a retryable `ModelError::Transient` (HTTP 500), the
// real-world Cloudflare AI Gateway guardrails case that exposed the bug
// in `transition_after_turn_failure`; tests assert it also resolves to
// `Idle`, not just `RateLimited`.
struct TransientModel;

impl ModelProvider for TransientModel {
    fn respond<'a>(&'a self, _request: ModelRequest) -> crate::model::ModelFuture<'a> {
        Box::pin(async {
            Err(crate::model::ModelError::Transient {
                status: Some(500),
                retry_after_secs: None,
                message: "gateway guardrails block".to_owned(),
            })
        })
    }
}

// Always fails with a retryable `ModelError::Timeout`, so tests can
// assert `transition_after_turn_failure` returns the session to `Idle`.
struct TimeoutModel;

impl ModelProvider for TimeoutModel {
    fn respond<'a>(&'a self, _request: ModelRequest) -> crate::model::ModelFuture<'a> {
        Box::pin(async {
            Err(crate::model::ModelError::Timeout {
                message: "test provider timeout".to_owned(),
            })
        })
    }
}

// Always fails with a non-retryable `ModelError::Refusal`, so tests can
// assert `transition_after_turn_failure` moves the session to the
// terminal `Failed` state instead of `Idle`.
struct RefusalModel;

impl ModelProvider for RefusalModel {
    fn respond<'a>(&'a self, _request: ModelRequest) -> crate::model::ModelFuture<'a> {
        Box::pin(async {
            Err(crate::model::ModelError::Refusal {
                detail: Some("policy violation".to_owned()),
            })
        })
    }
}

// Always fails with a non-retryable `ModelError::QuotaExceeded`, so tests
// can assert `transition_after_turn_failure` moves the session to the
// terminal `Failed` state, not `Idle` — a provider exhausting its
// contingent is not a transient condition a retry can fix, and this is
// an easy variant to accidentally make retryable later.
struct QuotaExceededModel;

impl ModelProvider for QuotaExceededModel {
    fn respond<'a>(&'a self, _request: ModelRequest) -> crate::model::ModelFuture<'a> {
        Box::pin(async {
            Err(crate::model::ModelError::QuotaExceeded {
                message: "test provider quota exhausted".to_owned(),
            })
        })
    }
}

struct MissingContextRecoveryModel {
    call_id: ToolCallId,
    responses: AtomicUsize,
    saw_rejected_tool_result: AtomicBool,
}

impl ModelProvider for MissingContextRecoveryModel {
    fn respond<'a>(&'a self, request: ModelRequest) -> crate::model::ModelFuture<'a> {
        let response_index = self.responses.fetch_add(1, Ordering::SeqCst);
        let call_id = self.call_id.clone();
        if response_index > 0 {
            self.saw_rejected_tool_result.store(
                request.history.items().iter().any(|item| {
                    matches!(item, TurnItem::ToolResult(result) if !result.result.is_success())
                }),
                Ordering::SeqCst,
            );
        }
        Box::pin(async move {
            if response_index == 0 {
                Ok(crate::model::ModelResponse {
                    message: None,
                    tool_calls: vec![ToolCall {
                        id: call_id,
                        name: ToolName::new("fs.read"),
                        arguments: serde_json::json!({"path": "untrusted"}),
                    }],
                    usage: Default::default(),
                    ..Default::default()
                })
            } else {
                Ok(crate::model::ModelResponse::text(
                    "I can continue without that tool.",
                ))
            }
        })
    }
}

struct CompletionTrackingSpawner {
    child_completed: AtomicUsize,
    child_finished: AtomicUsize,
    events: Arc<Mutex<Vec<&'static str>>>,
}

impl AgentSpawner for CompletionTrackingSpawner {
    fn spawn_child<'a>(
        &'a self,
        _role: &'a str,
        _input: SpawnInput,
        _sandbox: SandboxSpec,
        _suggestions: Option<AgentSuggestions>,
    ) -> SpawnFuture<'a> {
        Box::pin(async { Ok(SessionId::new()) })
    }

    fn child_finished(&self, _child: &SessionId) {
        self.child_finished.fetch_add(1, Ordering::SeqCst);
    }

    fn child_completed(
        &self,
        _child: &SessionId,
        _completed_at: jiff::Timestamp,
    ) -> Result<(), AgentSpawnError> {
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(events.as_slice(), ["persisted_child_result"]);
        events.push("durably_completed_child");
        self.child_completed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct ChildResultStore {
    fail_child_result: bool,
    events: Arc<Mutex<Vec<&'static str>>>,
}

impl StateStore for ChildResultStore {
    fn save_turn<'a>(
        &'a self,
        _sid: &'a SessionId,
        item: &'a TurnItem,
    ) -> harw_extension_api::ExtFuture<'a, crate::state_store::StateStoreResult<()>> {
        let is_child_result = matches!(item, TurnItem::ToolResult(_));
        let fail_child_result = self.fail_child_result;
        let events = self.events.clone();
        Box::pin(async move {
            if is_child_result && fail_child_result {
                return Err(crate::state_store::StateStoreError::PoisonedMutex {
                    operation: "child_result_persistence",
                });
            }
            if is_child_result {
                events
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push("persisted_child_result");
            }
            Ok(())
        })
    }

    fn load_history<'a>(
        &'a self,
        _sid: &'a SessionId,
    ) -> harw_extension_api::ExtFuture<
        'a,
        crate::state_store::StateStoreResult<crate::history::ConversationHistory>,
    > {
        Box::pin(async { Ok(crate::history::ConversationHistory::new()) })
    }
}

fn paused_child_session(
    spawner: Arc<CompletionTrackingSpawner>,
) -> TestResult<(AgentSession, SessionId, ToolCallId)> {
    let (tx, _rx) = mpsc::unbounded_channel();
    let registry = ExtensionRegistryBuilder::default().spawner(spawner).build();
    let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx);
    session.try_start_turn().map_err(ctx("test turn starts"))?;
    let child = SessionId::new();
    let call_id = ToolCallId::new();
    session
        .begin_handoff(child.clone(), call_id.clone(), "worker".to_owned())
        .map_err(ctx("test handoff pauses"))?;
    Ok((session, child, call_id))
}

#[tokio::test]
async fn durable_child_completion_follows_persisted_child_result() -> TestResult {
    let events = Arc::new(Mutex::new(Vec::new()));
    let spawner = Arc::new(CompletionTrackingSpawner {
        child_completed: AtomicUsize::new(0),
        child_finished: AtomicUsize::new(0),
        events: events.clone(),
    });
    let (mut session, child, call_id) = paused_child_session(spawner.clone())?;
    let store = ChildResultStore {
        fail_child_result: false,
        events: events.clone(),
    };
    let temp = tempfile::tempdir().map_err(ctx("temporary approval directory"))?;
    let approvals = ApprovalStore::new(temp.path());

    let outcome = resume_after_child_durable(
        &mut session,
        &crate::model::EchoModelProvider::new("parent resumed"),
        &store,
        &approvals,
        child,
        call_id,
        ToolCallResult::success(serde_json::json!({"child": "done"})),
    )
    .await
    .map_err(ctx("durable child result resumes the parent"))?;

    assert!(matches!(outcome, TurnOutcome::Completed));
    assert_eq!(spawner.child_completed.load(Ordering::SeqCst), 1);
    assert_eq!(spawner.child_finished.load(Ordering::SeqCst), 0);
    assert_eq!(
        events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_slice(),
        ["persisted_child_result", "durably_completed_child"]
    );
    Ok(())
}

#[tokio::test]
async fn failed_child_result_persistence_does_not_release_durable_child() -> TestResult {
    let events = Arc::new(Mutex::new(Vec::new()));
    let spawner = Arc::new(CompletionTrackingSpawner {
        child_completed: AtomicUsize::new(0),
        child_finished: AtomicUsize::new(0),
        events: events.clone(),
    });
    let (mut session, child, call_id) = paused_child_session(spawner.clone())?;
    let store = ChildResultStore {
        fail_child_result: true,
        events: events.clone(),
    };
    let temp = tempfile::tempdir().map_err(ctx("temporary approval directory"))?;
    let approvals = ApprovalStore::new(temp.path());

    let Err(error) = resume_after_child_durable(
        &mut session,
        &crate::model::EchoModelProvider::new("must not run"),
        &store,
        &approvals,
        child,
        call_id,
        ToolCallResult::success(serde_json::json!({"child": "done"})),
    )
    .await
    else {
        return Err(TestError::Unexpected(
            "failed child-result persistence rejects the resume".to_owned(),
        ));
    };

    assert!(matches!(error, CoreError::TurnRejected(_)));
    assert_eq!(spawner.child_completed.load(Ordering::SeqCst), 0);
    assert_eq!(spawner.child_finished.load(Ordering::SeqCst), 0);
    assert!(
        events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty(),
        "a failed persistence attempt must not release the child"
    );
    Ok(())
}

// ------------------------------------------------------------------
// Minimal stub ToolProvider
// ------------------------------------------------------------------

struct StubToolProvider {
    specs: Vec<ToolSpec>,
}

impl StubToolProvider {
    fn with_names(names: &[&str]) -> Arc<Self> {
        let specs = names
            .iter()
            .map(|n| {
                ToolSpec::Function(FunctionToolSpec {
                    name: ToolName::new(*n),
                    description: format!("stub tool {n}"),
                    parameters: JsonSchema::default(),
                    strict: false,
                })
            })
            .collect();
        Arc::new(Self { specs })
    }
}

impl ToolProvider for StubToolProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        self.specs.clone()
    }
    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        if self.specs.iter().any(|s| s.name() == name.as_str()) {
            Some(Arc::new(StubExecutor))
        } else {
            None
        }
    }
}

struct StubExecutor;

impl ToolExecutor for StubExecutor {
    fn execute<'a>(
        &'a self,
        _ctx: &'a ToolExecutionContext,
        _call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async {
            Ok(ToolOutput::Text {
                content: "ok".to_owned(),
            })
        })
    }
}

fn make_session(
    provider: Arc<dyn ToolProvider>,
    activation: SessionActivation,
) -> AgentSession {
    let (tx, _rx) = mpsc::unbounded_channel();
    let registry = ExtensionRegistryBuilder::default()
        .tool_provider(provider)
        .build();
    AgentSession::new(AgentRole::Assistant, None, registry, tx).with_activation(activation)
}

// ------------------------------------------------------------------
// collect_tools filter tests
// ------------------------------------------------------------------

async fn workflow_request(session: &AgentSession) -> TestResult<ModelRequest> {
    let turn_id = harw_types::TurnId::new();
    let input = TurnInputContext {
        session_id: session.id().clone(),
        turn_id: turn_id.clone(),
        metadata: serde_json::Value::Null,
    };
    assemble_round_request(
        session,
        &input,
        &TurnControl::new(),
        &turn_id,
        &harw_types::TokenUsage::default(),
        None,
        None,
    )
    .await
    .map_err(ctx("assemble workflow request"))
}

#[tokio::test]
async fn workflow_uses_composed_tools_and_activation_at_request_time() -> TestResult {
    // No baseline tool inventory: these tools arrive via registry composition.
    let names = [
        "fs.read",
        "plan",
        "work_driver.enqueue",
        "work_driver.status",
        "work_driver.stop",
    ];
    let full = make_session(
        StubToolProvider::with_names(&names),
        SessionActivation::new(ToolProfile::Full),
    );
    let request = workflow_request(&full).await?;
    assert!(
        request
            .instruction_fragments
            .iter()
            .any(|text| text.contains("### Governed durable execution"))
    );
    let repeated = workflow_request(&full).await?;
    assert_eq!(
        request.instruction_fragments,
        repeated.instruction_fragments
    );

    let mut activation = SessionActivation::new(ToolProfile::Full);
    activation.disable_tool(ToolName::new("work_driver.enqueue"));
    let disabled = make_session(StubToolProvider::with_names(&names), activation);
    let request = workflow_request(&disabled).await?;
    assert!(
        !request
            .instruction_fragments
            .iter()
            .any(|text| text.contains("### Governed durable execution"))
    );
    Ok(())
}

/// Exercise the merged request path, not the workflow/skill renderers in
/// isolation: triggered evidence must never join trusted workflow text.
#[tokio::test]
async fn workflow_and_skills_preserve_trust_ledger_and_cache_prefix() -> TestResult {
    use crate::skill_injection::{SkillInjectionSettings, SkillInjector};
    use harw_context::{ContextBudgetSpec, ContextCeiling, SectionName, TrustClass};
    use harw_context_ledger::{LedgerKind, MemoryLedger};

    let bundle = [
        (
            "skills/trusted/skill.toml",
            "name = \"trusted\"\ndescription = \"Trusted fixture\"\n",
        ),
        (
            "skills/trusted/triggers.toml",
            "[triggers]\nkeywords = [\"merge fixture\"]\n",
        ),
        (
            "skills/trusted/instructions.md",
            "# Trusted\n\n## When to use\n\nBUNDLED_MERGE_MARKER\n",
        ),
        (
            "skills/imported/skill.toml",
            "name = \"imported\"\ndescription = \"Imported fixture\"\n",
        ),
        (
            "skills/imported/triggers.toml",
            "[triggers]\nkeywords = [\"merge fixture\"]\n",
        ),
        (
            "skills/imported/instructions.md",
            "# Imported\n\n## When to use\n\nIMPORTED_MERGE_MARKER\n",
        ),
    ];
    let index = Arc::new(harw_catalog::SkillIndex::build_with_bundle(&[], &bundle));
    for with_program in [false, true] {
        let injector = Arc::new(
            SkillInjector::new(
                Arc::clone(&index),
                SkillInjectionSettings {
                    inject_imported: true,
                    ..SkillInjectionSettings::default()
                },
            )
            .with_imported_skills(["imported"]),
        );
        let ledger = Arc::new(MemoryLedger::new());
        let mut spawn = test_spawn_context()?;
        spawn.ceiling = Some(ContextCeiling {
            sections: harw_context::ceiling::ROOT_CONTEXT_SECTIONS
                .iter()
                .map(|name| SectionName::try_new(*name))
                .collect::<Result<_, _>>()?,
            max_trust: TrustClass::Instruction,
            budget: ContextBudgetSpec {
                total: harw_lens_types::BudgetSpec { total: 1_000_000 },
                per_section: Default::default(),
            },
        });
        let mut session = make_session(
            StubToolProvider::with_names(&[
                "fs.read",
                "plan",
                "work_driver.enqueue",
                "work_driver.status",
                "work_driver.stop",
            ]),
            SessionActivation::new(ToolProfile::Full),
        )
        .with_spawn_context(spawn)
        .with_skill_injector(Some(injector))
        .with_context_ledger(Some(ledger.clone()));
        if with_program {
            session = session.with_context_program(Default::default());
        }
        session.history_mut().push_user_text("hello");
        let before = workflow_request(&session).await?;
        assert!(
            before
                .instruction_fragments
                .iter()
                .any(|text| { text.contains("### Governed durable execution") })
        );
        session.history_mut().push_user_text("merge fixture");
        let injected = workflow_request(&session).await?;
        let repeated = workflow_request(&session).await?;
        assert_eq!(before.system_prompt, injected.system_prompt);
        assert!(
            injected
                .instruction_fragments
                .starts_with(&before.instruction_fragments)
        );
        assert_eq!(
            injected.instruction_fragments,
            repeated.instruction_fragments
        );
        assert_eq!(injected.data_block, repeated.data_block);
        let instructions = injected.instruction_fragments.join("\n");
        assert!(instructions.contains("BUNDLED_MERGE_MARKER"));
        assert!(!instructions.contains("IMPORTED_MERGE_MARKER"));
        assert!(!injected.system_prompt.contains("IMPORTED_MERGE_MARKER"));
        assert!(
            injected
                .data_block
                .as_deref()
                .is_some_and(|data| { data.contains("IMPORTED_MERGE_MARKER") })
        );
        for (label, trust) in [
            ("skill:trusted", "instruction"),
            ("skill:imported", "evidence"),
        ] {
            assert!(
                injected
                    .context_assembly
                    .included_fragment_labels
                    .iter()
                    .any(|value| value == label)
            );
            let entries: Vec<_> = ledger
                .entries()
                .into_iter()
                .filter(|entry| entry.label == label)
                .collect();
            assert_eq!(
                entries.len(),
                2,
                "one record per assembled request: {label}"
            );
            assert!(entries.iter().all(|entry| {
                entry.kind == LedgerKind::Offered && entry.trust.as_deref() == Some(trust)
            }));
        }
        let ledger_json = serde_json::to_string(&ledger.entries())?;
        assert!(!ledger_json.contains("BUNDLED_MERGE_MARKER"));
        assert!(!ledger_json.contains("IMPORTED_MERGE_MARKER"));
    }
    Ok(())
}

#[tokio::test]
async fn workflow_plan_mode_and_missing_spawner_do_not_advertise_execution() -> TestResult {
    let names = [
        "fs.read",
        "fs.write",
        "delegate_wave",
        "plan",
        "work_driver.enqueue",
        "work_driver.status",
        "work_driver.stop",
    ];
    let session = make_session(
        StubToolProvider::with_names(&names),
        SessionActivation::new(ToolProfile::Full),
    )
    .with_mode(crate::mode::InteractionMode::Plan);
    let request = workflow_request(&session).await?;
    let text = request.instruction_fragments.join("\n");
    assert!(!text.contains("### Governed durable execution"));
    assert!(!text.contains("### Direct implementation"));
    assert!(!text.contains("### Parallel delegation"));
    Ok(())
}

#[tokio::test]
async fn workflow_without_spawner_preserves_role_routing_and_can_be_disabled() -> TestResult {
    let names = ["fs.read", "fs.write", "delegate_wave"];
    let session = make_session(
        StubToolProvider::with_names(&names),
        SessionActivation::new(ToolProfile::Full),
    );
    let request = workflow_request(&session).await?;
    let text = request.instruction_fragments.join("\n");
    assert!(!text.contains("### Parallel delegation"));
    assert!(text.contains("Organization routing rules take precedence"));
    assert!(text.contains("missing delegation is a blocker"));

    let mut activation = SessionActivation::new(ToolProfile::Full);
    activation.disable_instructions("workflow");
    let disabled = make_session(StubToolProvider::with_names(&names), activation);
    let request = workflow_request(&disabled).await?;
    assert!(
        !request
            .instruction_fragments
            .iter()
            .any(|text| text.contains("### Direct implementation"))
    );
    Ok(())
}

#[test]
fn test_collect_tools_full_profile_includes_all() -> TestResult {
    let provider = StubToolProvider::with_names(&["fs.read", "custom.tool"]);
    let session = make_session(provider, SessionActivation::new(ToolProfile::Full));
    let tools = collect_tools(&session).map_err(ctx("collect_tools should not fail"))?;
    let names: Vec<_> = tools.iter().map(|t| t.name().to_owned()).collect();
    assert!(names.contains(&"fs.read".to_owned()));
    assert!(names.contains(&"custom.tool".to_owned()));
    Ok(())
}

#[test]
fn test_collect_tools_coding_profile_excludes_unlisted() -> TestResult {
    let provider = StubToolProvider::with_names(&["fs.read", "custom.tool"]);
    let session = make_session(provider, SessionActivation::new(ToolProfile::Coding));
    let tools = collect_tools(&session).map_err(ctx("collect_tools should not fail"))?;
    let names: Vec<_> = tools.iter().map(|t| t.name().to_owned()).collect();
    assert!(
        names.contains(&"fs.read".to_owned()),
        "fs.read must be visible"
    );
    assert!(
        !names.contains(&"custom.tool".to_owned()),
        "custom.tool must be hidden by Coding profile"
    );
    Ok(())
}

#[test]
fn test_collect_tools_disable_override_hides_allowlisted_tool() -> TestResult {
    let provider = StubToolProvider::with_names(&["fs.read", "fs.write"]);
    let mut activation = SessionActivation::new(ToolProfile::Full);
    activation.disable_tool(ToolName::new("fs.write"));
    let session = make_session(provider, activation);
    let tools = collect_tools(&session).map_err(ctx("collect_tools should not fail"))?;
    let names: Vec<_> = tools.iter().map(|t| t.name().to_owned()).collect();
    assert!(names.contains(&"fs.read".to_owned()));
    assert!(
        !names.contains(&"fs.write".to_owned()),
        "fs.write must be hidden by override"
    );
    Ok(())
}

#[test]
fn test_collect_tools_sorted_by_name() -> TestResult {
    // Absichtlich unsortiert registriert — die Provider-Reihenfolge darf
    // die Reihenfolge im Tool-Array nicht bestimmen (Prompt-Cache).
    let provider = StubToolProvider::with_names(&["zeta.tool", "alpha.tool", "mid.tool"]);
    let session = make_session(provider, SessionActivation::new(ToolProfile::Full));
    let tools = collect_tools(&session).map_err(ctx("collect_tools should not fail"))?;
    let names: Vec<_> = tools.iter().map(ToolSpec::name).collect();
    assert_eq!(names, vec!["alpha.tool", "mid.tool", "zeta.tool"]);
    Ok(())
}

// ------------------------------------------------------------------
// find_executor filter tests
// ------------------------------------------------------------------

#[test]
fn test_find_executor_returns_none_when_tool_disabled() {
    let provider = StubToolProvider::with_names(&["shell.exec"]);
    let mut activation = SessionActivation::new(ToolProfile::Full);
    activation.disable_tool(ToolName::new("shell.exec"));
    let session = make_session(provider, activation);
    let result = find_executor(&session, &ToolName::new("shell.exec"));
    assert!(result.is_none(), "disabled tool must have no executor");
}

#[test]
fn test_find_executor_returns_some_when_tool_enabled() {
    let provider = StubToolProvider::with_names(&["fs.read"]);
    let session = make_session(provider, SessionActivation::new(ToolProfile::Full));
    let result = find_executor(&session, &ToolName::new("fs.read"));
    assert!(result.is_some(), "enabled tool must return an executor");
}

// Fix C (Moduldoku „Achter Nachtrag"): `shell.exec` is one of
// `crate::guard::POLLING_TOOLS` — repeating the exact same successful
// call is legitimate while waiting on a long-running background job, so
// every repeat must still count as round progress. Before the fix, only
// the *first* occurrence of a call signature counted, so a model
// legitimately polling a background process saw the "no progress" hint
// and misread it as proof of a response cache.
#[tokio::test]
async fn apply_tool_guard_repeated_shell_exec_success_always_counts_as_progress() {
    let provider = StubToolProvider::with_names(&["shell.exec"]);
    let session = make_session(provider, SessionActivation::new(ToolProfile::Full));
    let store = crate::state_store::InMemoryStateStore::new();
    let mut guard = TurnGuard::new(crate::guard::GuardPolicy::default(), session.id());
    let mut seen_success_signatures: HashSet<String> = HashSet::new();
    let args = serde_json::json!({"cmd": "jobs -l"});

    // More rounds than the default `no_progress_rounds_abort` (8), to
    // prove polling never trips the guard, not merely within a narrow
    // window.
    for round in 0..10 {
        let mut round_progressed = false;
        let mut pending_hint = None;
        let mut result = ToolCallResult::success(serde_json::json!("still running"));
        let abort_reason = apply_tool_guard(
            &session,
            &store,
            Some(&mut guard),
            &mut seen_success_signatures,
            &mut pending_hint,
            &mut round_progressed,
            "shell.exec",
            &args,
            &mut result,
        )
        .await;
        assert!(abort_reason.is_none(), "round {round}: unexpected abort");
        assert!(
            round_progressed,
            "round {round}: a repeated successful shell.exec call must count as progress"
        );
        assert!(
            matches!(
                guard.observe_round_end(round_progressed),
                GuardVerdict::Continue
            ),
            "round {round}: polling a running process must never trigger the no-progress hint"
        );
    }
}

// Companion to the polling exemption above: an *unexempted* tool
// (`fs.read`) repeated with identical arguments must still trip the
// no-progress warning within the default threshold — the fix narrows the
// exemption to `crate::guard::POLLING_TOOLS`, it does not weaken the
// guard generally.
#[tokio::test]
async fn apply_tool_guard_repeated_fs_read_success_still_warns_no_progress() {
    let provider = StubToolProvider::with_names(&["fs.read"]);
    let session = make_session(provider, SessionActivation::new(ToolProfile::Full));
    let store = crate::state_store::InMemoryStateStore::new();
    let mut guard = TurnGuard::new(crate::guard::GuardPolicy::default(), session.id());
    let mut seen_success_signatures: HashSet<String> = HashSet::new();
    let args = serde_json::json!({"path": "same.txt"});

    let mut saw_warn = false;
    for _ in 0..crate::guard::GuardPolicy::default().no_progress_rounds_warn + 1 {
        let mut round_progressed = false;
        let mut pending_hint = None;
        let mut result = ToolCallResult::success(serde_json::json!("contents"));
        let _ = apply_tool_guard(
            &session,
            &store,
            Some(&mut guard),
            &mut seen_success_signatures,
            &mut pending_hint,
            &mut round_progressed,
            "fs.read",
            &args,
            &mut result,
        )
        .await;
        if matches!(
            guard.observe_round_end(round_progressed),
            GuardVerdict::Warn { .. }
        ) {
            saw_warn = true;
            break;
        }
    }
    assert!(
        saw_warn,
        "repeated identical fs.read calls must still trigger the no-progress hint \
             within the default threshold"
    );
}

#[tokio::test]
async fn rate_limited_model_failure_leaves_the_session_retryable() -> TestResult {
    let provider = StubToolProvider::with_names(&[]);
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
    let store = crate::state_store::InMemoryStateStore::new();

    let Err(error) = run_turn(
        &mut session,
        &RateLimitedModel,
        &store,
        TurnInput::user("retry me"),
    )
    .await
    else {
        return Err(TestError::Unexpected(
            "rate limiting reaches the caller".to_owned(),
        ));
    };

    assert!(matches!(
        error,
        CoreError::Model(crate::model::ModelError::RateLimited { .. })
    ));
    assert!(
        matches!(session.state(), crate::session::SessionState::Idle),
        "a retryable provider failure must not terminalize the session"
    );

    let outcome = run_turn(
        &mut session,
        &crate::model::EchoModelProvider::new("recovered"),
        &store,
        TurnInput::user("try again"),
    )
    .await
    .map_err(ctx("the same session accepts a later retry"))?;
    assert!(matches!(outcome, TurnOutcome::Completed));
    Ok(())
}

#[tokio::test]
async fn transient_model_failure_leaves_the_session_retryable() -> TestResult {
    let provider = StubToolProvider::with_names(&[]);
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
    let store = crate::state_store::InMemoryStateStore::new();

    let Err(error) = run_turn(
        &mut session,
        &TransientModel,
        &store,
        TurnInput::user("retry me"),
    )
    .await
    else {
        return Err(TestError::Unexpected(
            "a transient provider failure reaches the caller".to_owned(),
        ));
    };

    assert!(matches!(
        error,
        CoreError::Model(crate::model::ModelError::Transient { .. })
    ));
    assert!(
        matches!(session.state(), crate::session::SessionState::Idle),
        "a transient (e.g. HTTP 500 guardrails block) provider failure must not terminalize the session"
    );

    let outcome = run_turn(
        &mut session,
        &crate::model::EchoModelProvider::new("recovered"),
        &store,
        TurnInput::user("try again"),
    )
    .await
    .map_err(ctx("the same session accepts a later retry"))?;
    assert!(matches!(outcome, TurnOutcome::Completed));
    Ok(())
}

#[tokio::test]
async fn timeout_model_failure_leaves_the_session_retryable() -> TestResult {
    let provider = StubToolProvider::with_names(&[]);
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
    let store = crate::state_store::InMemoryStateStore::new();

    let Err(error) = run_turn(
        &mut session,
        &TimeoutModel,
        &store,
        TurnInput::user("retry me"),
    )
    .await
    else {
        return Err(TestError::Unexpected(
            "a provider timeout reaches the caller".to_owned(),
        ));
    };

    assert!(matches!(
        error,
        CoreError::Model(crate::model::ModelError::Timeout { .. })
    ));
    assert!(
        matches!(session.state(), crate::session::SessionState::Idle),
        "a provider timeout must not terminalize the session"
    );

    let outcome = run_turn(
        &mut session,
        &crate::model::EchoModelProvider::new("recovered"),
        &store,
        TurnInput::user("try again"),
    )
    .await
    .map_err(ctx("the same session accepts a later retry"))?;
    assert!(matches!(outcome, TurnOutcome::Completed));
    Ok(())
}

#[tokio::test]
async fn refusal_model_failure_terminalizes_the_session() -> TestResult {
    let provider = StubToolProvider::with_names(&[]);
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
    let store = crate::state_store::InMemoryStateStore::new();

    let Err(error) = run_turn(
        &mut session,
        &RefusalModel,
        &store,
        TurnInput::user("do the thing"),
    )
    .await
    else {
        return Err(TestError::Unexpected(
            "a refusal reaches the caller".to_owned(),
        ));
    };

    assert!(matches!(
        error,
        CoreError::Model(crate::model::ModelError::Refusal { .. })
    ));
    assert!(
        matches!(session.state(), crate::session::SessionState::Failed(_)),
        "a non-retryable provider failure (Refusal) must still terminalize the session"
    );
    Ok(())
}

// `transition_after_turn_failure`'s retryable branch calls
// `session.complete_turn(...)` to return the session to `Idle`. That call
// itself can be rejected (e.g. a `TurnHandle` that no longer names the
// session's active turn) — a rare, best-effort case per the function's
// doc comment: the session is left in its prior state rather than
// inventing a state transition the caller never asked for. Driving this
// through the public `run_turn` API isn't practical (nothing in that
// path can make the turn handle stale mid-flight), so this test calls
// the private function directly with a `TurnInputContext` whose
// `turn_id` deliberately does not match the session's actual active
// turn, forcing `complete_turn` to reject it.
#[test]
fn transition_after_turn_failure_logs_but_does_not_panic_when_complete_turn_itself_fails()
-> TestResult {
    let provider = StubToolProvider::with_names(&[]);
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));

    let real_handle = session.try_start_turn().map_err(ctx(
        "a freshly created session starts Idle and accepts a turn",
    ))?;
    assert!(matches!(
        session.state(),
        crate::session::SessionState::Running
    ));

    // Same session, but a turn id that is not the one `try_start_turn`
    // actually activated — `complete_turn` must reject this handle.
    let stale_ctx = harw_extension_api::TurnInputContext {
        session_id: real_handle.session_id.clone(),
        turn_id: TurnId::new(),
        metadata: serde_json::Value::Null,
    };
    let retryable_error = CoreError::Model(crate::model::ModelError::RateLimited {
        retry_after_secs: 1,
        message: "test provider limit".to_owned(),
    });

    transition_after_turn_failure(&mut session, &stale_ctx, &retryable_error);

    // The original error was surfaced (via `tracing::error!` inside
    // `transition_after_turn_failure`, not asserted here directly), and
    // the session was neither corrupted nor silently marked `Idle` or
    // `Failed` — it stays exactly where `complete_turn` left it: still
    // `Running`, still owning its real, original turn.
    assert!(
        matches!(session.state(), crate::session::SessionState::Running),
        "a failed best-effort completion must leave the session in its prior state"
    );
    assert_eq!(
        session.current_turn(),
        Some(&real_handle.turn_id),
        "the session's real active turn must be untouched by the rejected completion"
    );
    Ok(())
}

#[tokio::test]
async fn quota_exceeded_model_failure_terminalizes_the_session() -> TestResult {
    let provider = StubToolProvider::with_names(&[]);
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
    let store = crate::state_store::InMemoryStateStore::new();

    let Err(error) = run_turn(
        &mut session,
        &QuotaExceededModel,
        &store,
        TurnInput::user("do the thing"),
    )
    .await
    else {
        return Err(TestError::Unexpected(
            "a quota-exceeded failure reaches the caller".to_owned(),
        ));
    };

    assert!(matches!(
        error,
        CoreError::Model(crate::model::ModelError::QuotaExceeded { .. })
    ));
    assert!(
        matches!(session.state(), crate::session::SessionState::Failed(_)),
        "an exhausted quota is not a transient condition a retry can fix, \
             so the session must terminalize instead of staying Idle"
    );
    Ok(())
}

#[tokio::test]
async fn missing_tool_execution_context_is_model_visible_and_recoverable() -> TestResult {
    let provider = StubToolProvider::with_names(&["fs.read"]);
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
    let store = crate::state_store::InMemoryStateStore::new();
    let model = MissingContextRecoveryModel {
        call_id: ToolCallId::new(),
        responses: AtomicUsize::new(0),
        saw_rejected_tool_result: AtomicBool::new(false),
    };

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("read this"))
        .await
        .map_err(ctx("the model can recover from a rejected tool boundary"))?;

    assert!(matches!(outcome, TurnOutcome::Completed));
    assert_eq!(model.responses.load(Ordering::SeqCst), 2);
    assert!(
        model.saw_rejected_tool_result.load(Ordering::SeqCst),
        "the follow-up model request must include the rejected tool result"
    );
    assert!(matches!(
        session.state(),
        crate::session::SessionState::Idle
    ));
    assert_eq!(session.history().len(), 4);
    Ok(())
}

#[tokio::test]
async fn empty_live_history_is_hydrated_before_a_new_user_turn() -> TestResult {
    let provider = StubToolProvider::with_names(&[]);
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
    let store = crate::state_store::InMemoryStateStore::new();
    let mut persisted = crate::history::ConversationHistory::new();
    persisted.push_user_text("previous question");
    persisted.push_assistant_text("previous answer", None);
    store
        .save_history(session.id(), &persisted)
        .await
        .map_err(ctx("seed history"))?;

    run_turn(
        &mut session,
        &crate::model::EchoModelProvider::new("new answer"),
        &store,
        TurnInput::user("new question"),
    )
    .await
    .map_err(ctx("hydrated turn runs"))?;

    assert_eq!(session.history().len(), 4);
    assert_eq!(store.turn_count(session.id()), 4);
    Ok(())
}

#[tokio::test]
async fn history_load_error_fails_closed_before_user_item_is_admitted() -> TestResult {
    let provider = StubToolProvider::with_names(&[]);
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));

    let Err(error) = run_turn(
        &mut session,
        &crate::model::EchoModelProvider::new("must not run"),
        &LoadFailStore,
        TurnInput::user("must not be persisted"),
    )
    .await
    else {
        return Err(TestError::Unexpected(
            "history load failure must reject the turn".to_owned(),
        ));
    };

    assert!(matches!(error, CoreError::TurnRejected(_)));
    // The user item must never be admitted. The only permitted entry is
    // the error record `transition_after_turn_failure` appends so the
    // failure stays visible in export/resume.
    let items = session.history().items();
    assert_eq!(items.len(), 1, "only the failure record may be recorded");
    assert!(
        matches!(&items[0], TurnItem::Error(_)),
        "expected a single error item, got {:?}",
        items[0]
    );
    assert!(matches!(
        session.state(),
        crate::session::SessionState::Failed(_)
    ));
    Ok(())
}

#[tokio::test]
async fn duplicate_tool_catalog_failure_remains_terminal() -> TestResult {
    let (tx, _rx) = mpsc::unbounded_channel();
    let registry = ExtensionRegistryBuilder::default()
        .tool_provider(StubToolProvider::with_names(&["duplicate"]))
        .tool_provider(StubToolProvider::with_names(&["duplicate"]))
        .build();
    let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx);
    let store = crate::state_store::InMemoryStateStore::new();

    let Err(error) = run_turn(
        &mut session,
        &crate::model::EchoModelProvider::new("must not run"),
        &store,
        TurnInput::user("go"),
    )
    .await
    else {
        return Err(TestError::Unexpected(
            "duplicate tools are an invariant violation".to_owned(),
        ));
    };

    assert!(matches!(error, CoreError::DuplicateTool { .. }));
    assert!(matches!(
        session.state(),
        crate::session::SessionState::Failed(_)
    ));
    Ok(())
}

// ------------------------------------------------------------------
// W2-14: Parallel-Dispatch mit Approval-Vorprüfung
// ------------------------------------------------------------------

/// Liefert eine vorprogrammierte Folge von Antworten, eine pro Model-Call.
struct ScriptedModel {
    responses: Mutex<std::collections::VecDeque<crate::model::ModelResponse>>,
}

impl ScriptedModel {
    fn new(responses: Vec<crate::model::ModelResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
        }
    }
}

impl ModelProvider for ScriptedModel {
    fn respond<'a>(&'a self, _request: ModelRequest) -> crate::model::ModelFuture<'a> {
        let next = self
            .responses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front();
        Box::pin(async move { next.ok_or(crate::model::ModelError::EmptyResponse) })
    }
}

/// Approval-Handler, der jede Befragung protokolliert und je Call-Id eine
/// vorprogrammierte Entscheidung liefert. Nicht gelistete Calls → `Allow`.
struct CountingApproval {
    scripted: Vec<(ToolCallId, ApprovalDecision)>,
    reviews: Mutex<Vec<ToolCallId>>,
}

impl CountingApproval {
    fn new(scripted: Vec<(ToolCallId, ApprovalDecision)>) -> Self {
        Self {
            scripted,
            reviews: Mutex::new(Vec::new()),
        }
    }

    fn allow_everything() -> Self {
        Self::new(Vec::new())
    }

    fn reviews_for(&self, call_id: &ToolCallId) -> usize {
        self.reviews
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|reviewed| *reviewed == call_id)
            .count()
    }

    fn total_reviews(&self) -> usize {
        self.reviews
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }
}

impl harw_extension_api::ApprovalHandler for CountingApproval {
    fn review<'a>(
        &'a self,
        call: &'a ToolCall,
    ) -> harw_extension_api::ExtFuture<'a, ApprovalDecision> {
        self.reviews
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(call.id.clone());
        let decision = self
            .scripted
            .iter()
            .find(|(id, _)| id == &call.id)
            .map_or(ApprovalDecision::Allow, |(_, decision)| decision.clone());
        Box::pin(async move { decision })
    }
}

/// Werkzeug, das seine Aufrufe zählt und — mit Barriere — erst zurückkehrt,
/// wenn die geforderte Zahl an Aufrufen gleichzeitig in Flight ist. Damit
/// unterscheidet ein Test echte Nebenläufigkeit von serieller Ausführung.
struct BarrierExecutor {
    executions: Arc<AtomicUsize>,
    barrier: Option<Arc<tokio::sync::Barrier>>,
}

impl ToolExecutor for BarrierExecutor {
    fn execute<'a>(
        &'a self,
        _ctx: &'a ToolExecutionContext,
        _call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        let barrier = self.barrier.clone();
        Box::pin(async move {
            if let Some(barrier) = barrier {
                barrier.wait().await;
            }
            Ok(ToolOutput::Text {
                content: "ok".to_owned(),
            })
        })
    }
}

struct StubParallelProvider {
    names: Vec<ToolName>,
    /// Teilmenge von `names`, die als `parallel_safe` gemeldet wird.
    parallel: Vec<ToolName>,
    executions: Arc<AtomicUsize>,
    barrier: Option<Arc<tokio::sync::Barrier>>,
}

impl StubParallelProvider {
    fn instant(names: &[&str], executions: Arc<AtomicUsize>) -> Arc<Self> {
        let names: Vec<ToolName> = names.iter().map(|name| ToolName::new(*name)).collect();
        Arc::new(Self {
            parallel: names.clone(),
            names,
            executions,
            barrier: None,
        })
    }

    fn joined(names: &[&str], executions: Arc<AtomicUsize>, parties: usize) -> Arc<Self> {
        let names: Vec<ToolName> = names.iter().map(|name| ToolName::new(*name)).collect();
        Arc::new(Self {
            parallel: names.clone(),
            names,
            executions,
            barrier: Some(Arc::new(tokio::sync::Barrier::new(parties))),
        })
    }

    fn with_serial_tool(
        names: &[&str],
        serial: &str,
        executions: Arc<AtomicUsize>,
    ) -> Arc<Self> {
        let names: Vec<ToolName> = names.iter().map(|name| ToolName::new(*name)).collect();
        let parallel = names
            .iter()
            .filter(|name| name.as_str() != serial)
            .cloned()
            .collect();
        Arc::new(Self {
            names,
            parallel,
            executions,
            barrier: None,
        })
    }
}

impl ToolProvider for StubParallelProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        self.names
            .iter()
            .map(|name| {
                ToolSpec::Function(FunctionToolSpec {
                    name: name.clone(),
                    description: "stub tool".to_owned(),
                    parameters: JsonSchema::default(),
                    strict: false,
                })
            })
            .collect()
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        self.names.contains(name).then(|| {
            Arc::new(BarrierExecutor {
                executions: Arc::clone(&self.executions),
                barrier: self.barrier.clone(),
            }) as Arc<dyn ToolExecutor>
        })
    }

    fn parallel_safe(&self, name: &ToolName) -> bool {
        self.parallel.contains(name)
    }
}

/// Sandbox auf dem echten Harness-Verzeichnis. Ohne aufgelösten
/// Spawn-Kontext lehnt der Turn-Loop jede Tool-Ausführung ab, der
/// Parallel-Pfad wäre also gar nicht erreichbar.
fn test_sandbox() -> TestResult<SandboxSpec> {
    let harness_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or(TestError::Missing(
            "harw-core hat ein Workspace-Elternverzeichnis",
        ))?
        .to_path_buf();
    let registry = harw_authority::WorkspaceRegistry::build(
        &harness_root,
        [harw_authority::WorkspaceRegistration {
            tenant: harw_types::TenantId::from_str("test-tenant"),
            workspace: harw_types::WorkspaceId::from_str("core-parallel-tests"),
            root: std::path::PathBuf::from("harw-core"),
        }],
    )
    .map_err(ctx("Test-Workspace ist registrierbar"))?;
    Ok(SandboxSpec::from_resolved(
        registry
            .resolve(
                &harw_types::TenantId::from_str("test-tenant"),
                &harw_types::WorkspaceId::from_str("core-parallel-tests"),
            )
            .map_err(ctx("Test-Workspace löst auf"))?,
        harw_authority::PermissionSet::from_policy([
            harw_authority::Permission::ReadWorkspace,
            harw_authority::Permission::WriteWorkspace,
            harw_authority::Permission::ExecuteProcess,
        ]),
    ))
}

fn test_spawn_context() -> TestResult<SpawnContext> {
    Ok(SpawnContext {
        sandbox: test_sandbox()?,
        suggestions: None,
        capability_snapshot: None,
        approval_actor: Some(ApprovalActor::Operator {
            id: "test-operator".to_owned(),
        }),
        organizational_role: harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
        allowed_child_orchestrators: Vec::new(),
        trace: None,
        // `None` reads fail-closed (most restrictive ceiling), not
        // "unbounded" — see `SpawnContext::ceiling`'s own doc. Existing
        // tests that build a session via this helper never exercised a
        // ceiling before this field existed, so `None` keeps their
        // behavior unchanged. Tests that need a real ceiling build their
        // own `SpawnContext` literal instead of this shared helper.
        ceiling: None,
    })
}

fn guarded_session(
    provider: Arc<dyn ToolProvider>,
    handler: Arc<CountingApproval>,
) -> TestResult<AgentSession> {
    let (tx, _rx) = mpsc::unbounded_channel();
    let registry = ExtensionRegistryBuilder::default()
        .tool_provider(provider)
        .approval_handler(handler)
        .build();
    Ok(AgentSession::new(AgentRole::Assistant, None, registry, tx)
        .with_spawn_context(test_spawn_context()?))
}

/// Wie [`guarded_session`], aber mit mehreren Handlern in genau der
/// übergebenen Registrierungsreihenfolge (W1-05: Aggregation).
fn multi_guarded_session(
    provider: Arc<dyn ToolProvider>,
    handlers: &[Arc<CountingApproval>],
) -> TestResult<AgentSession> {
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut builder = ExtensionRegistryBuilder::default().tool_provider(provider);
    for handler in handlers {
        // Erst konkret klonen, dann beim Argument zu `Arc<dyn …>` coercen.
        let handler: Arc<CountingApproval> = Arc::clone(handler);
        builder = builder.approval_handler(handler);
    }
    Ok(
        AgentSession::new(AgentRole::Assistant, None, builder.build(), tx)
            .with_spawn_context(test_spawn_context()?),
    )
}

fn call(id: &ToolCallId, name: &str) -> ToolCall {
    ToolCall {
        id: id.clone(),
        name: ToolName::new(name),
        arguments: serde_json::json!({"part": name}),
    }
}

fn response_with(calls: Vec<ToolCall>) -> crate::model::ModelResponse {
    crate::model::ModelResponse {
        message: None,
        tool_calls: calls,
        usage: Default::default(),
        ..Default::default()
    }
}

fn drain_turn_events(rx: &mut mpsc::UnboundedReceiver<TurnEvent>) -> Vec<TurnEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

#[tokio::test]
async fn a_registered_approval_handler_no_longer_disables_parallel_dispatch() -> TestResult {
    // Regression W2-14: früher genügte ein registrierter Handler, um den
    // JoinSet-Pfad abzuschalten. Die Barriere über zwei Parteien beweist,
    // dass beide Calls tatsächlich gleichzeitig laufen — seriell würde der
    // erste Aufruf nie zurückkehren und der Timeout zuschlagen.
    let first = ToolCallId::new();
    let second = ToolCallId::new();
    let executions = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(CountingApproval::allow_everything());
    let provider = StubParallelProvider::joined(&["lookup"], Arc::clone(&executions), 2);
    let mut session = guarded_session(provider, Arc::clone(&handler))?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![
        response_with(vec![call(&first, "lookup"), call(&second, "lookup")]),
        crate::model::ModelResponse::text("beide Ergebnisse liegen vor"),
    ]);

    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        run_turn(&mut session, &model, &store, TurnInput::user("parallel")),
    )
    .await
    .map_err(ctx(
        "die Calls müssen gleichzeitig laufen, sonst blockiert die Barriere",
    ))?
    .map_err(ctx("der Turn läuft durch"))?;

    assert!(matches!(outcome, TurnOutcome::Completed));
    assert_eq!(executions.load(Ordering::SeqCst), 2);
    Ok(())
}

/// Ladybird-Export „Werkzeug-Budget erschöpft (25 von 16 Aufrufen)“: eine
/// Antwort mit mehr Aufrufen als dem Rest des Werkzeug-Budgets führt nur
/// den Rest aus (parallel und sequenziell), beantwortet die übrigen mit
/// „nicht ausgeführt: …“ und beendet den Turn mit `Budget`.
#[tokio::test]
async fn batch_larger_than_remaining_tool_budget_executes_only_the_remainder() -> TestResult {
    // (Vorverbrauch, Aufrufe in der Antwort, erwartet ausgeführt)
    for (used_before, batch, expected) in [(13_u32, 5_usize, 3_usize), (15, 3, 1), (16, 2, 0)] {
        let ids: Vec<ToolCallId> = (0..batch).map(|_| ToolCallId::new()).collect();
        let executions = Arc::new(AtomicUsize::new(0));
        let handler = Arc::new(CountingApproval::allow_everything());
        let provider = StubParallelProvider::instant(&["lookup"], Arc::clone(&executions));
        let mut session = guarded_session(provider, Arc::clone(&handler))?;
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![
            response_with(ids.iter().map(|id| call(id, "lookup")).collect()),
            crate::model::ModelResponse::text("darf nicht mehr angefragt werden"),
        ]);
        let control = TurnControl::new().with_tool_call_budget(16, used_before);
        let check = control.clone();

        let outcome = run_turn(
            &mut session,
            &model,
            &store,
            TurnInput::user("viele Aufrufe").with_control(control),
        )
        .await
        .map_err(ctx("das Budget beendet den Turn regulär"))?;

        assert!(
            matches!(
                outcome,
                TurnOutcome::Cancelled {
                    reason: CancelReason::Budget
                }
            ),
            "{used_before}/{batch}: {outcome:?}"
        );
        assert_eq!(
            executions.load(Ordering::SeqCst),
            expected,
            "{used_before}/{batch}"
        );
        assert_eq!(
            check.tool_calls(),
            u32::try_from(expected).unwrap_or(u32::MAX)
        );
        let skipped = batch - expected;
        assert_eq!(
            check.tool_budget_skipped(),
            u32::try_from(skipped).unwrap_or(0)
        );
        assert_eq!(
            check.stop_detail().as_deref(),
            Some("Werkzeug-Budget erschöpft (16 von 16 Aufrufen)")
        );
        // Jeder Aufruf hat ein Ergebnis; die übersprungenen nennen das Budget.
        let skip_messages: Vec<String> = session
            .history()
            .items()
            .iter()
            .filter_map(|item| match item {
                TurnItem::ToolResult(result) => match &result.result {
                    ToolCallResult::Error { message }
                        if message.starts_with(TOOL_BUDGET_SKIP_PREFIX) =>
                    {
                        Some(message.clone())
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(skip_messages.len(), skipped, "{used_before}/{batch}");
        assert!(
            skip_messages
                .iter()
                .all(|message| message.ends_with("(16 von 16)")),
            "{skip_messages:?}"
        );
        let results = session
            .history()
            .items()
            .iter()
            .filter(|item| matches!(item, TurnItem::ToolResult(_)))
            .count();
        assert_eq!(results, batch, "jeder tool_call hat sein tool_result");
    }
    Ok(())
}

// ------------------------------------------------------------------
// ToolCallMeter: Zulassung und Buchung vor der Ausführung
// ------------------------------------------------------------------

/// Meter, der die ersten `allowed` Aufrufe zulässt und jeden weiteren
/// abweist. Er hält fest, welche Werkzeuge angefragt wurden und wie viele
/// Ausführungen zum Zeitpunkt der jeweiligen Anfrage schon liefen.
struct LimitedMeter {
    allowed: usize,
    asked: Mutex<Vec<(String, usize)>>,
    executions: Arc<AtomicUsize>,
}

impl LimitedMeter {
    fn new(allowed: usize, executions: Arc<AtomicUsize>) -> Arc<Self> {
        Arc::new(Self {
            allowed,
            asked: Mutex::new(Vec::new()),
            executions,
        })
    }

    fn asked(&self) -> Vec<(String, usize)> {
        self.asked
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl crate::tool_call_meter::ToolCallMeter for LimitedMeter {
    fn admit_tool_call<'a>(
        &'a self,
        tool: &'a str,
    ) -> crate::tool_call_meter::ToolCallAdmissionFuture<'a> {
        let index = {
            let mut asked = self
                .asked
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            asked.push((tool.to_owned(), self.executions.load(Ordering::SeqCst)));
            asked.len().saturating_sub(1)
        };
        let limit = self.allowed;
        Box::pin(async move {
            if index < limit {
                Ok(crate::tool_call_meter::ToolCallPermit::unguarded())
            } else {
                Err(format!("budget: tool-call limit of {limit} reached"))
            }
        })
    }
}

/// Fehlermeldungen aller Tool-Ergebnisse des Verlaufs samt Call-Id.
fn error_results(session: &AgentSession) -> Vec<(ToolCallId, String)> {
    session
        .history()
        .items()
        .iter()
        .filter_map(|item| match item {
            TurnItem::ToolResult(result) => match &result.result {
                ToolCallResult::Error { message } => {
                    Some((result.call_id.clone(), message.clone()))
                }
                ToolCallResult::Success { .. } => None,
            },
            _ => None,
        })
        .collect()
}

fn tool_result_count(session: &AgentSession) -> usize {
    session
        .history()
        .items()
        .iter()
        .filter(|item| matches!(item, TurnItem::ToolResult(_)))
        .count()
}

fn error_of<'a>(errors: &'a [(ToolCallId, String)], id: &ToolCallId) -> Option<&'a str> {
    errors
        .iter()
        .find(|(candidate, _)| candidate == id)
        .map(|(_, message)| message.as_str())
}

/// Sequenzieller Pfad: Weist der Meter den zweiten Aufruf ab, läuft nur der
/// erste. Der zweite bekommt das Abweisungsergebnis, der dritte wird nicht
/// einmal mehr angefragt und vor der Ausführung abgebrochen, und der Turn
/// endet mit `Budget`. Der erste Aufruf wurde gebucht, bevor er lief.
#[tokio::test]
async fn meter_refusing_the_second_call_skips_it_and_ends_the_turn_sequentially() -> TestResult
{
    let ids: Vec<ToolCallId> = (0..3).map(|_| ToolCallId::new()).collect();
    let [first, second, third] = ids.as_slice() else {
        return Err(TestError::Missing("drei Call-Ids"));
    };
    let executions = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(CountingApproval::allow_everything());
    // `lookup` ist nicht parallel-sicher: die Antwort läuft sequenziell.
    let provider =
        StubParallelProvider::with_serial_tool(&["lookup"], "lookup", Arc::clone(&executions));
    let mut session = guarded_session(provider, Arc::clone(&handler))?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![
        response_with(ids.iter().map(|id| call(id, "lookup")).collect()),
        crate::model::ModelResponse::text("darf nicht mehr angefragt werden"),
    ]);
    let meter = LimitedMeter::new(1, Arc::clone(&executions));
    let concrete = Arc::clone(&meter);
    let control = TurnControl::new().with_tool_call_meter(concrete);
    let check = control.clone();

    let outcome = run_turn(
        &mut session,
        &model,
        &store,
        TurnInput::user("drei Aufrufe").with_control(control),
    )
    .await
    .map_err(ctx("die Abweisung beendet den Turn regulär"))?;

    assert!(
        matches!(
            outcome,
            TurnOutcome::Cancelled {
                reason: CancelReason::Budget
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(executions.load(Ordering::SeqCst), 1, "nur der erste lief");
    assert_eq!(
        meter.asked(),
        vec![("lookup".to_owned(), 0), ("lookup".to_owned(), 1)],
        "gebucht, bevor ausgeführt; nach der Abweisung wird nicht weiter gefragt"
    );
    let stop = check.stop_detail();
    assert!(
        stop.as_deref()
            .is_some_and(|detail| detail.contains("tool-call limit of 1 reached")),
        "{stop:?}"
    );
    let errors = error_results(&session);
    assert_eq!(error_of(&errors, first), None, "der erste Aufruf lief");
    assert!(
        error_of(&errors, second)
            .is_some_and(|message| message.starts_with(TOOL_CALL_REFUSED_PREFIX)),
        "{errors:?}"
    );
    assert!(
        error_of(&errors, third)
            .is_some_and(|message| message.starts_with("turn cancelled before execution")),
        "{errors:?}"
    );
    assert_eq!(
        tool_result_count(&session),
        3,
        "jeder tool_call hat sein tool_result"
    );
    let remaining = model
        .responses
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .len();
    assert_eq!(remaining, 1, "nach der Abweisung folgt keine Modellrunde");
    Ok(())
}

/// Paralleler Pfad: Die zugelassenen Aufrufe laufen, der abgewiesene und
/// alle folgenden werden nicht gestartet; der Turn endet mit `Budget`.
#[tokio::test]
async fn meter_refusal_in_the_parallel_path_starts_only_the_admitted_prefix() -> TestResult {
    let ids: Vec<ToolCallId> = (0..3).map(|_| ToolCallId::new()).collect();
    let [first, second, third] = ids.as_slice() else {
        return Err(TestError::Missing("drei Call-Ids"));
    };
    let executions = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(CountingApproval::allow_everything());
    let provider = StubParallelProvider::instant(&["lookup"], Arc::clone(&executions));
    let mut session = guarded_session(provider, Arc::clone(&handler))?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![
        response_with(ids.iter().map(|id| call(id, "lookup")).collect()),
        crate::model::ModelResponse::text("darf nicht mehr angefragt werden"),
    ]);
    let meter = LimitedMeter::new(2, Arc::clone(&executions));
    let concrete = Arc::clone(&meter);
    let control = TurnControl::new().with_tool_call_meter(concrete);
    let check = control.clone();

    let outcome = run_turn(
        &mut session,
        &model,
        &store,
        TurnInput::user("parallel").with_control(control),
    )
    .await
    .map_err(ctx("die Abweisung beendet den Turn regulär"))?;

    assert!(
        matches!(
            outcome,
            TurnOutcome::Cancelled {
                reason: CancelReason::Budget
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        executions.load(Ordering::SeqCst),
        2,
        "nur zwei Jobs gestartet"
    );
    assert_eq!(
        meter.asked().len(),
        3,
        "der dritte wurde gefragt und abgewiesen"
    );
    assert!(
        check
            .stop_detail()
            .is_some_and(|detail| detail.contains("tool-call limit of 2 reached"))
    );
    let errors = error_results(&session);
    assert_eq!(error_of(&errors, first), None);
    assert_eq!(error_of(&errors, second), None);
    assert!(
        error_of(&errors, third)
            .is_some_and(|message| message.starts_with(TOOL_CALL_REFUSED_PREFIX)),
        "{errors:?}"
    );
    assert_eq!(
        tool_result_count(&session),
        3,
        "jeder tool_call hat sein tool_result"
    );
    Ok(())
}

/// Ein Meter, der schon den ersten Aufruf abweist, lässt nichts laufen.
#[tokio::test]
async fn meter_refusing_the_first_call_executes_nothing() -> TestResult {
    let id = ToolCallId::new();
    let executions = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(CountingApproval::allow_everything());
    let provider = StubParallelProvider::instant(&["lookup"], Arc::clone(&executions));
    let mut session = guarded_session(provider, Arc::clone(&handler))?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![response_with(vec![call(&id, "lookup")])]);
    let meter = LimitedMeter::new(0, Arc::clone(&executions));
    let concrete = Arc::clone(&meter);
    let control = TurnControl::new().with_tool_call_meter(concrete);

    let outcome = run_turn(
        &mut session,
        &model,
        &store,
        TurnInput::user("ein Aufruf").with_control(control),
    )
    .await
    .map_err(ctx("die Abweisung beendet den Turn regulär"))?;

    assert!(
        matches!(
            outcome,
            TurnOutcome::Cancelled {
                reason: CancelReason::Budget
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert_eq!(tool_result_count(&session), 1);
    Ok(())
}

/// Ein von der Guardrail verweigerter Aufruf wird nicht gebucht: der Meter
/// sieht nur den Aufruf, der tatsächlich laufen würde.
#[tokio::test]
async fn meter_is_not_asked_for_calls_the_guardrail_denied() -> TestResult {
    let denied = ToolCallId::new();
    let allowed = ToolCallId::new();
    let executions = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(CountingApproval::new(vec![(
        denied.clone(),
        ApprovalDecision::Deny("policy".to_owned()),
    )]));
    let provider =
        StubParallelProvider::with_serial_tool(&["lookup"], "lookup", Arc::clone(&executions));
    let mut session = guarded_session(provider, Arc::clone(&handler))?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![
        response_with(vec![call(&denied, "lookup"), call(&allowed, "lookup")]),
        crate::model::ModelResponse::text("fertig"),
    ]);
    let meter = LimitedMeter::new(5, Arc::clone(&executions));
    let concrete = Arc::clone(&meter);
    let control = TurnControl::new().with_tool_call_meter(concrete);

    let outcome = run_turn(
        &mut session,
        &model,
        &store,
        TurnInput::user("verweigert und erlaubt").with_control(control),
    )
    .await
    .map_err(ctx("der Turn läuft durch"))?;

    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(
        meter.asked(),
        vec![("lookup".to_owned(), 0)],
        "nur der erlaubte Aufruf wurde gebucht"
    );
    Ok(())
}

/// Fortsetzungspfad nach einer Rückfrage: Auch der nun genehmigte Aufruf
/// wird erst gebucht; ein abweisender Meter verhindert die Ausführung.
#[tokio::test]
async fn meter_refusal_blocks_an_approved_call_on_the_ask_user_resume_path() -> TestResult {
    let call_id = ToolCallId::new();
    let executions = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(CountingApproval::allow_everything());
    let provider = StubParallelProvider::instant(&["lookup"], Arc::clone(&executions));
    let mut session = guarded_session(provider, Arc::clone(&handler))?;
    session.try_start_turn().map_err(ctx("test turn starts"))?;
    let actor = ApprovalActor::Operator {
        id: "test-operator".to_owned(),
    };
    session
        .begin_approval(
            call(&call_id, "lookup"),
            harw_types::ItemId::new(),
            actor.clone(),
            jiff::Timestamp::now(),
        )
        .map_err(ctx("pause for approval"))?;
    let meter = LimitedMeter::new(0, Arc::clone(&executions));
    let concrete = Arc::clone(&meter);
    session.set_active_turn_control(TurnControl::new().with_tool_call_meter(concrete));
    let store = crate::state_store::InMemoryStateStore::new();
    let model = crate::model::EchoModelProvider::new("must not be reached");

    let outcome = resume_after_approval(
        &mut session,
        &model,
        &store,
        actor,
        ApprovalResolution::Approve,
    )
    .await
    .map_err(ctx("die Abweisung beendet den Turn regulär"))?;

    assert!(
        matches!(
            outcome,
            TurnOutcome::Cancelled {
                reason: CancelReason::Budget
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert_eq!(meter.asked(), vec![("lookup".to_owned(), 0)]);
    let errors = error_results(&session);
    assert!(
        error_of(&errors, &call_id)
            .is_some_and(|message| message.starts_with(TOOL_CALL_REFUSED_PREFIX)),
        "{errors:?}"
    );
    Ok(())
}

// ------------------------------------------------------------------
// ToolCallPermit: Lease-Zaun bis zur Seiteneffekt-Grenze
// ------------------------------------------------------------------

/// Gemeinsames Protokoll der Zaun-Tests: Ausführungen, freigegebene
/// Guards, Abweisungen, Modellrunden und Reclaim-Versuche in
/// Eintrittsreihenfolge.
type FenceLog = Arc<Mutex<Vec<&'static str>>>;

fn fence_log() -> FenceLog {
    Arc::new(Mutex::new(Vec::new()))
}

fn push_fence(log: &Mutex<Vec<&'static str>>, entry: &'static str) {
    log.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(entry);
}

fn fence_entries(log: &Mutex<Vec<&'static str>>) -> Vec<&'static str> {
    log.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Protokolleinträge (Ausführung, Freigabe des Guards) je Werkzeugname.
fn fence_labels(tool: &str) -> (&'static str, &'static str) {
    match tool {
        "alpha" => ("executed alpha", "dropped alpha"),
        "beta" => ("executed beta", "dropped beta"),
        _ => ("executed", "dropped"),
    }
}

/// Guard eines Permits: protokolliert seine Freigabe und hält optional
/// die Lese-Seite einer Lease, die ein Reclaim exklusiv braucht.
struct FenceGuard {
    log: FenceLog,
    label: &'static str,
    _lease: Option<tokio::sync::OwnedRwLockReadGuard<()>>,
}

impl Drop for FenceGuard {
    fn drop(&mut self) {
        // Läuft, bevor `_lease` freigegeben wird (Felder fallen danach).
        push_fence(&self.log, self.label);
    }
}

/// Meter, der die ersten `allowed` Aufrufe mit einem protokollierenden
/// Guard zulässt und jeden weiteren abweist.
struct FenceMeter {
    allowed: usize,
    asked: AtomicUsize,
    log: FenceLog,
}

impl FenceMeter {
    fn new(allowed: usize, log: &FenceLog) -> Arc<Self> {
        Arc::new(Self {
            allowed,
            asked: AtomicUsize::new(0),
            log: Arc::clone(log),
        })
    }

    fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }
}

impl crate::tool_call_meter::ToolCallMeter for FenceMeter {
    fn admit_tool_call<'a>(
        &'a self,
        tool: &'a str,
    ) -> crate::tool_call_meter::ToolCallAdmissionFuture<'a> {
        Box::pin(async move {
            if self.asked.fetch_add(1, Ordering::SeqCst) < self.allowed {
                Ok(crate::tool_call_meter::ToolCallPermit::guarded(Box::new(
                    FenceGuard {
                        log: Arc::clone(&self.log),
                        label: fence_labels(tool).1,
                        _lease: None,
                    },
                )))
            } else {
                push_fence(&self.log, "refused");
                Err(format!(
                    "budget: tool-call limit of {} reached",
                    self.allowed
                ))
            }
        })
    }
}

/// Meter, dessen Guard die Lese-Seite einer Lease hält. Unmittelbar nach
/// der Buchung — vor dem Start des Werkzeugs — stößt er einen Reclaim an,
/// der die Lease exklusiv will, und gibt den Permit erst heraus, wenn der
/// Reclaim es versucht hat.
struct LeaseMeter {
    lease: Arc<tokio::sync::RwLock<()>>,
    log: FenceLog,
    reclaim: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl LeaseMeter {
    fn new(log: &FenceLog) -> Arc<Self> {
        Arc::new(Self {
            lease: Arc::new(tokio::sync::RwLock::new(())),
            log: Arc::clone(log),
            reclaim: Mutex::new(None),
        })
    }

    fn take_reclaim(&self) -> Option<tokio::task::JoinHandle<()>> {
        self.reclaim
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

impl crate::tool_call_meter::ToolCallMeter for LeaseMeter {
    fn admit_tool_call<'a>(
        &'a self,
        tool: &'a str,
    ) -> crate::tool_call_meter::ToolCallAdmissionFuture<'a> {
        Box::pin(async move {
            let held = Arc::clone(&self.lease).read_owned().await;
            let (tried, attempted) = tokio::sync::oneshot::channel();
            let lease = Arc::clone(&self.lease);
            let log = Arc::clone(&self.log);
            let reclaim = tokio::spawn(async move {
                if lease.try_write().is_err() {
                    push_fence(&log, "reclaim blocked");
                }
                if tried.send(()).is_err() {
                    return;
                }
                let _reclaimed = lease.write_owned().await;
                push_fence(&log, "reclaimed");
            });
            *self
                .reclaim
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(reclaim);
            if attempted.await.is_err() {
                return Err("reclaim attempt never ran".to_owned());
            }
            Ok(crate::tool_call_meter::ToolCallPermit::guarded(Box::new(
                FenceGuard {
                    log: Arc::clone(&self.log),
                    label: fence_labels(tool).1,
                    _lease: Some(held),
                },
            )))
        })
    }
}

/// Werkzeug, das jede Ausführung protokolliert und — mit Barriere — erst
/// zurückkehrt, wenn alle Parteien laufen.
struct FenceExecutor {
    log: FenceLog,
    barrier: Option<Arc<tokio::sync::Barrier>>,
}

impl ToolExecutor for FenceExecutor {
    fn execute<'a>(
        &'a self,
        ctx: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            assert!(
                ctx.lifetime_guard().is_some(),
                "admitted guard must reach executor"
            );
            push_fence(&self.log, fence_labels(call.name.as_str()).0);
            if let Some(barrier) = &self.barrier {
                barrier.wait().await;
            }
            Ok(ToolOutput::Text {
                content: "ok".to_owned(),
            })
        })
    }
}

struct FenceProvider {
    names: Vec<ToolName>,
    parallel: bool,
    log: FenceLog,
    barrier: Option<Arc<tokio::sync::Barrier>>,
}

impl FenceProvider {
    /// Nicht parallel-sichere Werkzeuge: jede Antwort läuft sequenziell.
    fn serial(names: &[&str], log: &FenceLog) -> Arc<Self> {
        Arc::new(Self {
            names: names.iter().map(|name| ToolName::new(*name)).collect(),
            parallel: false,
            log: Arc::clone(log),
            barrier: None,
        })
    }

    /// Parallel-sichere Werkzeuge, deren Ausführungen sich an einer
    /// Barriere für `parties` Aufrufe treffen.
    fn joined(names: &[&str], log: &FenceLog, parties: usize) -> Arc<Self> {
        Arc::new(Self {
            names: names.iter().map(|name| ToolName::new(*name)).collect(),
            parallel: true,
            log: Arc::clone(log),
            barrier: Some(Arc::new(tokio::sync::Barrier::new(parties))),
        })
    }
}

impl ToolProvider for FenceProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        self.names
            .iter()
            .map(|name| {
                ToolSpec::Function(FunctionToolSpec {
                    name: name.clone(),
                    description: "fence tool".to_owned(),
                    parameters: JsonSchema::default(),
                    strict: false,
                })
            })
            .collect()
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        self.names.contains(name).then(|| {
            Arc::new(FenceExecutor {
                log: Arc::clone(&self.log),
                barrier: self.barrier.clone(),
            }) as Arc<dyn ToolExecutor>
        })
    }

    fn parallel_safe(&self, name: &ToolName) -> bool {
        self.parallel && self.names.contains(name)
    }
}

/// Models the filesystem tools' spawn_blocking boundary. The outer tool
/// future may disappear, while this closure still owns execution context.
struct BlockingFenceProvider {
    log: FenceLog,
    started: tokio::sync::mpsc::UnboundedSender<()>,
    finished: tokio::sync::mpsc::UnboundedSender<()>,
    release: Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
}

impl ToolProvider for BlockingFenceProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        ["alpha", "beta"]
            .into_iter()
            .map(|name| {
                ToolSpec::Function(FunctionToolSpec {
                    name: ToolName::new(name),
                    description: "blocking fence".to_owned(),
                    parameters: JsonSchema::default(),
                    strict: false,
                })
            })
            .collect()
    }
    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        matches!(name.as_str(), "alpha" | "beta").then(|| {
            Arc::new(BlockingFenceExecutor {
                log: Arc::clone(&self.log),
                started: self.started.clone(),
                finished: self.finished.clone(),
                release: Arc::clone(&self.release),
            }) as Arc<dyn ToolExecutor>
        })
    }
    fn parallel_safe(&self, name: &ToolName) -> bool {
        matches!(name.as_str(), "alpha" | "beta")
    }
}

struct BlockingFenceExecutor {
    log: FenceLog,
    started: tokio::sync::mpsc::UnboundedSender<()>,
    finished: tokio::sync::mpsc::UnboundedSender<()>,
    release: Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
}
impl ToolExecutor for BlockingFenceExecutor {
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        let context = context.clone();
        let log = Arc::clone(&self.log);
        let started = self.started.clone();
        let finished = self.finished.clone();
        let release = Arc::clone(&self.release);
        let label = fence_labels(call.name.as_str()).0;
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let _ = started.send(());
                release
                    .lock()
                    .map_err(|e| ToolsError::ExecutionFailed(e.to_string()))?
                    .recv_timeout(Duration::from_secs(10))
                    .map_err(|e| ToolsError::ExecutionFailed(e.to_string()))?;
                push_fence(&log, label);
                drop(context);
                let _ = finished.send(());
                Ok(ToolOutput::text("effect finished"))
            })
            .await
            .map_err(|e| ToolsError::ExecutionFailed(e.to_string()))?
        })
    }
}

async fn check_blocking_effect_survives_abort(
    parallel: bool,
    cancel_cooperatively: bool,
) -> TestResult {
    let log = fence_log();
    let (started, mut start_rx) = tokio::sync::mpsc::unbounded_channel();
    let (finished, mut finish_rx) = tokio::sync::mpsc::unbounded_channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let provider = Arc::new(BlockingFenceProvider {
        log: Arc::clone(&log),
        started,
        finished,
        release: Arc::new(Mutex::new(release_rx)),
    });
    let mut session =
        guarded_session(provider, Arc::new(CountingApproval::allow_everything()))?;
    let mut calls = vec![call(&ToolCallId::new(), "alpha")];
    if parallel {
        calls.push(call(&ToolCallId::new(), "beta"));
    }
    let count = calls.len();
    let model = ScriptedModel::new(vec![response_with(calls)]);
    let store = crate::state_store::InMemoryStateStore::new();
    let control = TurnControl::new().with_tool_call_meter(FenceMeter::new(count, &log));
    let cancel = control.cancel_token().clone();
    let turn = tokio::spawn(async move {
        run_turn(
            &mut session,
            &model,
            &store,
            TurnInput::user("run").with_control(control),
        )
        .await
    });
    for _ in 0..count {
        tokio::time::timeout(Duration::from_secs(5), start_rx.recv())
            .await
            .map_err(ctx("blocking effect starts"))?
            .ok_or(TestError::Missing("blocking start signal"))?;
    }
    if cancel_cooperatively {
        cancel.cancel(CancelReason::User);
        let outcome = tokio::time::timeout(Duration::from_secs(5), turn)
            .await
            .map_err(ctx("cancel finishes the async turn"))?
            .map_err(ctx("join cancelled turn"))?
            .map_err(ctx("cancelled outcome"))?;
        assert!(matches!(outcome, TurnOutcome::Cancelled { .. }));
    } else {
        turn.abort();
        let result = turn.await;
        assert!(result.is_err_and(|error| error.is_cancelled()));
    }
    let before_release = fence_entries(&log);
    // Release blocking work even when an assertion below would fail.
    for _ in 0..count {
        release.send(()).map_err(ctx("release blocking effect"))?;
    }
    for _ in 0..count {
        tokio::time::timeout(Duration::from_secs(5), finish_rx.recv())
            .await
            .map_err(ctx("blocking effect finishes"))?
            .ok_or(TestError::Missing("blocking finish signal"))?;
    }
    assert!(
        before_release.is_empty(),
        "lease released before effect: {before_release:?}"
    );
    let entries = fence_entries(&log);
    for name in if parallel {
        vec!["alpha", "beta"]
    } else {
        vec!["alpha"]
    } {
        let (executed, dropped) = fence_labels(name);
        let effect = entries
            .iter()
            .position(|e| *e == executed)
            .ok_or(TestError::Missing("effect log"))?;
        let released = entries
            .iter()
            .position(|e| *e == dropped)
            .ok_or(TestError::Missing("guard drop log"))?;
        assert!(effect < released, "{entries:?}");
    }
    Ok(())
}

#[tokio::test]
async fn blocking_effect_keeps_permit_after_outer_turn_abort() -> TestResult {
    check_blocking_effect_survives_abort(false, false).await
}

#[tokio::test]
async fn parallel_blocking_effects_keep_permits_after_outer_turn_abort() -> TestResult {
    check_blocking_effect_survives_abort(true, false).await
}

#[tokio::test]
async fn blocking_effect_keeps_permit_after_cooperative_cancel() -> TestResult {
    check_blocking_effect_survives_abort(false, true).await
}

#[tokio::test]
async fn parallel_blocking_effects_keep_permits_after_cooperative_cancel() -> TestResult {
    check_blocking_effect_survives_abort(true, true).await
}

/// Modell, das jede Runde protokolliert und mit Text antwortet.
struct FenceModel {
    log: FenceLog,
}

impl ModelProvider for FenceModel {
    fn respond<'a>(&'a self, _request: ModelRequest) -> crate::model::ModelFuture<'a> {
        push_fence(&self.log, "model");
        Box::pin(async { Ok(crate::model::ModelResponse::text("fertig")) })
    }
}

/// Sequenzieller Pfad: der Guard des Permits lebt, bis das Werkzeug
/// gelaufen ist, und fällt erst danach.
#[tokio::test]
async fn permit_is_held_until_the_tool_has_executed() -> TestResult {
    let id = ToolCallId::new();
    let log = fence_log();
    let handler = Arc::new(CountingApproval::allow_everything());
    let mut session = guarded_session(FenceProvider::serial(&["lookup"], &log), handler)?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![
        response_with(vec![call(&id, "lookup")]),
        crate::model::ModelResponse::text("fertig"),
    ]);
    let control = TurnControl::new().with_tool_call_meter(FenceMeter::new(1, &log));

    let outcome = run_turn(
        &mut session,
        &model,
        &store,
        TurnInput::user("ein Aufruf").with_control(control),
    )
    .await
    .map_err(ctx("der Turn läuft durch"))?;

    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    assert_eq!(
        fence_entries(&log),
        vec!["executed", "dropped"],
        "der Guard fällt erst, nachdem das Werkzeug gelaufen ist"
    );
    Ok(())
}

/// Paralleler Pfad: jeder Job hält seinen eigenen Permit. Die Barriere
/// zwingt beide Ausführungen gleichzeitig in Flight; jeder Guard fällt
/// erst nach der Ausführung seines eigenen Aufrufs.
#[tokio::test]
async fn parallel_calls_each_hold_their_own_permit() -> TestResult {
    let log = fence_log();
    let handler = Arc::new(CountingApproval::allow_everything());
    let mut session =
        guarded_session(FenceProvider::joined(&["alpha", "beta"], &log, 2), handler)?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![
        response_with(vec![
            call(&ToolCallId::new(), "alpha"),
            call(&ToolCallId::new(), "beta"),
        ]),
        crate::model::ModelResponse::text("fertig"),
    ]);
    let meter = FenceMeter::new(2, &log);
    let concrete = Arc::clone(&meter);
    let control = TurnControl::new().with_tool_call_meter(concrete);

    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        run_turn(
            &mut session,
            &model,
            &store,
            TurnInput::user("parallel").with_control(control),
        ),
    )
    .await
    .map_err(ctx(
        "beide Jobs laufen gleichzeitig, sonst hinge die Barriere",
    ))?
    .map_err(ctx("der Turn läuft durch"))?;

    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    assert_eq!(meter.asked(), 2, "jeder Aufruf wurde einzeln zugelassen");
    let entries = fence_entries(&log);
    assert_eq!(
        entries.len(),
        4,
        "zwei Ausführungen, zwei Guards: {entries:?}"
    );
    let at = |entry: &str| {
        entries
            .iter()
            .position(|logged| *logged == entry)
            .ok_or_else(|| TestError::Unexpected(format!("{entry} fehlt: {entries:?}")))
    };
    let executed_alpha = at("executed alpha")?;
    let dropped_alpha = at("dropped alpha")?;
    let executed_beta = at("executed beta")?;
    let dropped_beta = at("dropped beta")?;
    assert!(executed_alpha < dropped_alpha, "{entries:?}");
    assert!(executed_beta < dropped_beta, "{entries:?}");
    assert!(
        executed_alpha.max(executed_beta) < dropped_alpha.min(dropped_beta),
        "beide Permits leben gleichzeitig, solange ihre Werkzeuge laufen: {entries:?}"
    );
    Ok(())
}

/// Ein abgewiesener Aufruf läuft nie und bekommt keinen Permit; der
/// Permit des zugelassenen Vorgängers ist zu diesem Zeitpunkt schon
/// gefallen (vor der nächsten Zulassung).
#[tokio::test]
async fn refused_admission_never_executes() -> TestResult {
    let admitted = ToolCallId::new();
    let refused = ToolCallId::new();
    let log = fence_log();
    let handler = Arc::new(CountingApproval::allow_everything());
    let mut session = guarded_session(FenceProvider::serial(&["lookup"], &log), handler)?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![
        response_with(vec![call(&admitted, "lookup"), call(&refused, "lookup")]),
        crate::model::ModelResponse::text("darf nicht mehr angefragt werden"),
    ]);
    let control = TurnControl::new().with_tool_call_meter(FenceMeter::new(1, &log));

    let outcome = run_turn(
        &mut session,
        &model,
        &store,
        TurnInput::user("zwei Aufrufe").with_control(control),
    )
    .await
    .map_err(ctx("die Abweisung beendet den Turn regulär"))?;

    assert!(
        matches!(
            outcome,
            TurnOutcome::Cancelled {
                reason: CancelReason::Budget
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        fence_entries(&log),
        vec!["executed", "dropped", "refused"],
        "der erste Permit fällt vor der nächsten Zulassung; der abgewiesene Aufruf läuft nie"
    );
    let errors = error_results(&session);
    assert_eq!(error_of(&errors, &admitted), None, "der erste Aufruf lief");
    assert!(
        error_of(&errors, &refused)
            .is_some_and(|message| message.starts_with(TOOL_CALL_REFUSED_PREFIX)),
        "{errors:?}"
    );
    Ok(())
}

/// Fortsetzungspfad nach einer Rückfrage: der Permit des genehmigten
/// Aufrufs lebt bis nach der Ausführung und fällt, bevor der Turn mit der
/// nächsten Modellrunde weiterläuft.
#[tokio::test]
async fn permit_is_released_before_the_resumed_turn_continues() -> TestResult {
    let call_id = ToolCallId::new();
    let log = fence_log();
    let handler = Arc::new(CountingApproval::allow_everything());
    let mut session = guarded_session(FenceProvider::serial(&["lookup"], &log), handler)?;
    session.try_start_turn().map_err(ctx("test turn starts"))?;
    let actor = ApprovalActor::Operator {
        id: "test-operator".to_owned(),
    };
    session
        .begin_approval(
            call(&call_id, "lookup"),
            harw_types::ItemId::new(),
            actor.clone(),
            jiff::Timestamp::now(),
        )
        .map_err(ctx("pause for approval"))?;
    session.set_active_turn_control(
        TurnControl::new().with_tool_call_meter(FenceMeter::new(1, &log)),
    );
    let store = crate::state_store::InMemoryStateStore::new();
    let model = FenceModel {
        log: Arc::clone(&log),
    };

    let outcome = resume_after_approval(
        &mut session,
        &model,
        &store,
        actor,
        ApprovalResolution::Approve,
    )
    .await
    .map_err(ctx("der fortgesetzte Turn läuft durch"))?;

    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    assert_eq!(
        fence_entries(&log),
        vec!["executed", "dropped", "model"],
        "der Permit fällt nach der Ausführung und vor der nächsten Modellrunde"
    );
    Ok(())
}

/// Barriere-Test zum Lease-Zaun: ein Reclaim, der nach der Buchung, aber
/// vor dem Start des Werkzeugs versucht wird, kommt nicht durch, solange
/// der Permit lebt. Er gelingt erst, nachdem das Werkzeug gelaufen und der
/// Permit gefallen ist; danach führt der alte Worker nichts mehr aus.
#[tokio::test]
async fn reclaim_between_admission_and_execution_waits_for_the_permit() -> TestResult {
    let id = ToolCallId::new();
    let log = fence_log();
    let handler = Arc::new(CountingApproval::allow_everything());
    let mut session = guarded_session(FenceProvider::serial(&["lookup"], &log), handler)?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![
        response_with(vec![call(&id, "lookup")]),
        crate::model::ModelResponse::text("fertig"),
    ]);
    let meter = LeaseMeter::new(&log);
    let concrete = Arc::clone(&meter);
    let control = TurnControl::new().with_tool_call_meter(concrete);

    let outcome = run_turn(
        &mut session,
        &model,
        &store,
        TurnInput::user("ein Aufruf").with_control(control),
    )
    .await
    .map_err(ctx("der Turn läuft durch"))?;
    let reclaim = meter
        .take_reclaim()
        .ok_or(TestError::Missing("der Meter hat einen Reclaim angestoßen"))?;
    tokio::time::timeout(Duration::from_secs(5), reclaim)
        .await
        .map_err(ctx("der Reclaim kommt nach dem Fallen des Permits durch"))?
        .map_err(ctx("der Reclaim-Task endet regulär"))?;

    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    assert_eq!(
        fence_entries(&log),
        vec!["reclaim blocked", "executed", "dropped", "reclaimed"],
        "der Reclaim wartet auf den Permit; nach ihm läuft beim alten Worker nichts mehr"
    );
    Ok(())
}

#[tokio::test]
async fn parallel_preflight_asks_every_handler_exactly_once() -> TestResult {
    let first = ToolCallId::new();
    let second = ToolCallId::new();
    let executions = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(CountingApproval::allow_everything());
    let provider = StubParallelProvider::instant(&["lookup"], Arc::clone(&executions));
    let mut session = guarded_session(provider, Arc::clone(&handler))?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![
        response_with(vec![call(&first, "lookup"), call(&second, "lookup")]),
        crate::model::ModelResponse::text("fertig"),
    ]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("parallel"))
        .await
        .map_err(ctx("der Turn läuft durch"))?;

    assert!(matches!(outcome, TurnOutcome::Completed));
    assert_eq!(executions.load(Ordering::SeqCst), 2);
    assert_eq!(handler.reviews_for(&first), 1);
    assert_eq!(handler.reviews_for(&second), 1);
    assert_eq!(
        handler.total_reviews(),
        2,
        "im Parallel-Pfad darf kein Handler ein zweites Mal gefragt werden"
    );
    Ok(())
}

#[tokio::test]
async fn ask_user_falls_back_to_the_sequential_path_and_pauses_the_turn() -> TestResult {
    let allowed = ToolCallId::new();
    let asked = ToolCallId::new();
    let request = harw_types::ItemId::new();
    let executions = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(CountingApproval::new(vec![(
        asked.clone(),
        ApprovalDecision::AskUser(request.clone()),
    )]));
    let provider = StubParallelProvider::instant(&["lookup"], Arc::clone(&executions));
    let mut session = guarded_session(provider, Arc::clone(&handler))?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![response_with(vec![
        call(&allowed, "lookup"),
        call(&asked, "lookup"),
    ])]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("ask me"))
        .await
        .map_err(ctx("der Turn pausiert statt zu scheitern"))?;

    match &outcome {
        TurnOutcome::AwaitingApproval {
            call_id,
            request: paused_request,
        } => {
            assert_eq!(call_id, &asked);
            assert_eq!(paused_request, &request);
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "erwartet wurde eine Approval-Pause, nicht {other:?}"
            )));
        }
    }
    assert!(matches!(
        session.state(),
        crate::session::SessionState::WaitingForApproval
    ));
    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "nur der erlaubte Call darf ausgeführt worden sein"
    );
    assert_eq!(handler.reviews_for(&allowed), 1);
    assert_eq!(handler.reviews_for(&asked), 1);
    assert_eq!(
        handler.total_reviews(),
        2,
        "die Vorprüfung reicht ihre Entscheidungen an den sequenziellen \
             Pfad weiter, statt erneut zu fragen"
    );
    Ok(())
}

#[tokio::test]
async fn denied_call_falls_back_to_the_sequential_path_without_asking_twice() -> TestResult {
    let denied = ToolCallId::new();
    let allowed = ToolCallId::new();
    let executions = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(CountingApproval::new(vec![(
        denied.clone(),
        ApprovalDecision::Deny("policy".to_owned()),
    )]));
    let provider = StubParallelProvider::instant(&["lookup"], Arc::clone(&executions));
    let mut session = guarded_session(provider, Arc::clone(&handler))?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![
        response_with(vec![call(&denied, "lookup"), call(&allowed, "lookup")]),
        crate::model::ModelResponse::text("weiter ohne das verbotene Werkzeug"),
    ]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("deny one"))
        .await
        .map_err(ctx("ein Deny beendet den Turn nicht"))?;

    assert!(matches!(outcome, TurnOutcome::Completed));
    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "der verbotene Call darf nicht ausgeführt werden"
    );
    assert_eq!(handler.reviews_for(&denied), 1);
    assert_eq!(handler.reviews_for(&allowed), 1);
    assert_eq!(handler.total_reviews(), 2);
    Ok(())
}

#[tokio::test]
async fn a_non_parallel_safe_call_skips_the_preflight_entirely() -> TestResult {
    // Die Vorprüfung steht bewusst hinter der Executor-Auflösung: ein
    // Handler darf nicht für einen Parallel-Pfad gefragt werden, der ohnehin
    // verworfen wird.
    let parallel = ToolCallId::new();
    let serial = ToolCallId::new();
    let executions = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(CountingApproval::allow_everything());
    let provider = StubParallelProvider::with_serial_tool(
        &["lookup", "mutate"],
        "mutate",
        Arc::clone(&executions),
    );
    let mut session = guarded_session(provider, Arc::clone(&handler))?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![
        response_with(vec![call(&parallel, "lookup"), call(&serial, "mutate")]),
        crate::model::ModelResponse::text("seriell erledigt"),
    ]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("mixed"))
        .await
        .map_err(ctx("der gemischte Fall läuft seriell durch"))?;

    assert!(matches!(outcome, TurnOutcome::Completed));
    assert_eq!(executions.load(Ordering::SeqCst), 2);
    assert_eq!(handler.reviews_for(&parallel), 1);
    assert_eq!(handler.reviews_for(&serial), 1);
    assert_eq!(handler.total_reviews(), 2);
    Ok(())
}

#[tokio::test]
async fn a_single_call_never_enters_the_parallel_path() -> TestResult {
    let only = ToolCallId::new();
    let executions = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(CountingApproval::allow_everything());
    let provider = StubParallelProvider::instant(&["lookup"], Arc::clone(&executions));
    let mut session = guarded_session(provider, Arc::clone(&handler))?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![
        response_with(vec![call(&only, "lookup")]),
        crate::model::ModelResponse::text("einzeln erledigt"),
    ]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("single"))
        .await
        .map_err(ctx("ein einzelner Call läuft seriell"))?;

    assert!(matches!(outcome, TurnOutcome::Completed));
    assert_eq!(handler.total_reviews(), 1);
    Ok(())
}

// ------------------------------------------------------------------
// Addendum B: Projektgedächtnis-Erfassung (`ToolOutcomeObserver`)
// ------------------------------------------------------------------

/// Zeichnet jedes gemeldete Tool-Ergebnis auf, statt es irgendwo
/// abzulegen — genügt, um zu prüfen, dass `run_turn` den Beobachter
/// tatsächlich mit Name und Status erreicht.
struct RecordingObserver {
    outcomes: Mutex<Vec<(String, crate::capture::ToolOutcomeStatus)>>,
    turns_finished: AtomicUsize,
}

impl RecordingObserver {
    fn new() -> Self {
        Self {
            outcomes: Mutex::new(Vec::new()),
            turns_finished: AtomicUsize::new(0),
        }
    }
}

impl crate::capture::ToolOutcomeObserver for RecordingObserver {
    fn on_tool_outcome(
        &self,
        _session_id: &harw_types::SessionId,
        outcome: &crate::capture::ToolOutcome<'_>,
    ) {
        self.outcomes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push((outcome.tool_name.to_owned(), outcome.status));
    }

    fn on_turn_finished(&self, _session_id: &harw_types::SessionId) {
        self.turns_finished.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn tool_outcome_observer_sees_a_successful_tool_call() -> TestResult {
    let only = ToolCallId::new();
    let executions = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(CountingApproval::allow_everything());
    let provider = StubParallelProvider::instant(&["lookup"], Arc::clone(&executions));
    let observer = Arc::new(RecordingObserver::new());
    let mut session = guarded_session(provider, Arc::clone(&handler))?
        .with_tool_outcome_observer(Some(
            Arc::clone(&observer) as Arc<dyn crate::capture::ToolOutcomeObserver>
        ));
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![
        response_with(vec![call(&only, "lookup")]),
        crate::model::ModelResponse::text("erledigt"),
    ]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("single"))
        .await
        .map_err(ctx("ein einzelner Call läuft seriell"))?;

    assert!(matches!(outcome, TurnOutcome::Completed));
    let recorded = observer
        .outcomes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0, "lookup");
    assert_eq!(recorded[0].1, crate::capture::ToolOutcomeStatus::Success);
    assert_eq!(observer.turns_finished.load(Ordering::SeqCst), 1);
    Ok(())
}

// ------------------------------------------------------------------
// W1-05: Aggregation Deny > AskUser > Allow über alle Handler
// ------------------------------------------------------------------

#[tokio::test]
async fn check_approval_lets_a_later_deny_beat_an_earlier_ask_user() -> TestResult {
    // Regression G-004: früher gewann das vorn registrierte `AskUser`, der
    // Nutzer hätte freigeben können, was eine spätere Politik verbietet.
    let id = ToolCallId::new();
    let asking = Arc::new(CountingApproval::new(vec![(
        id.clone(),
        ApprovalDecision::AskUser(harw_types::ItemId::new()),
    )]));
    let denying = Arc::new(CountingApproval::new(vec![(
        id.clone(),
        ApprovalDecision::Deny("policy".to_owned()),
    )]));
    let session = multi_guarded_session(
        StubToolProvider::with_names(&["lookup"]),
        &[Arc::clone(&asking), Arc::clone(&denying)],
    )?;

    let decision = check_approval(&session, &call(&id, "lookup")).await;

    match decision {
        ApprovalDecision::Deny(reason) => assert_eq!(reason, "policy"),
        other => {
            return Err(TestError::Unexpected(format!(
                "ein späteres Deny muss gewinnen, war: {other:?}"
            )));
        }
    }
    assert_eq!(asking.reviews_for(&id), 1, "jeder Handler genau einmal");
    assert_eq!(denying.reviews_for(&id), 1, "jeder Handler genau einmal");
    Ok(())
}

#[tokio::test]
async fn check_approval_allows_only_when_every_handler_allows() -> TestResult {
    let id = ToolCallId::new();
    let first = Arc::new(CountingApproval::allow_everything());
    let second = Arc::new(CountingApproval::allow_everything());
    let session = multi_guarded_session(
        StubToolProvider::with_names(&["lookup"]),
        &[Arc::clone(&first), Arc::clone(&second)],
    )?;

    let decision = check_approval(&session, &call(&id, "lookup")).await;

    assert!(matches!(decision, ApprovalDecision::Allow));
    assert_eq!(first.reviews_for(&id), 1);
    assert_eq!(second.reviews_for(&id), 1);
    Ok(())
}

#[tokio::test]
async fn check_approval_keeps_the_first_ask_user_over_allow_and_later_requests() -> TestResult {
    let id = ToolCallId::new();
    let first_request = harw_types::ItemId::new();
    let asking = Arc::new(CountingApproval::new(vec![(
        id.clone(),
        ApprovalDecision::AskUser(first_request.clone()),
    )]));
    let allowing = Arc::new(CountingApproval::allow_everything());
    let asking_later = Arc::new(CountingApproval::new(vec![(
        id.clone(),
        ApprovalDecision::AskUser(harw_types::ItemId::new()),
    )]));
    let session = multi_guarded_session(
        StubToolProvider::with_names(&["lookup"]),
        &[
            Arc::clone(&asking),
            Arc::clone(&allowing),
            Arc::clone(&asking_later),
        ],
    )?;

    let decision = check_approval(&session, &call(&id, "lookup")).await;

    match decision {
        ApprovalDecision::AskUser(request) => assert_eq!(
            request, first_request,
            "die erste Anfrage-ID gilt, spätere werden verworfen"
        ),
        other => {
            return Err(TestError::Unexpected(format!(
                "AskUser + Allow muss AskUser ergeben, war: {other:?}"
            )));
        }
    }
    assert_eq!(asking.reviews_for(&id), 1);
    assert_eq!(allowing.reviews_for(&id), 1);
    assert_eq!(asking_later.reviews_for(&id), 1);
    Ok(())
}

// ------------------------------------------------------------------
// PL-100 Phase 4 (B22, O1): DoD-Taint je Turn
// ------------------------------------------------------------------

/// Werkzeuge der Taint-Tests; alle sequenziell (nicht `parallel_safe`).
const TAINT_TOOLS: &[&str] = &[
    "dod.findings",
    "job.stop",
    "job.start",
    "job.logs",
    "process.kill",
    "host.sudo_exec",
];

/// Sitzung mit `allow` (gibt alles frei — steht für Auto-Modus,
/// Sitzungsfreigabe oder gemerkte Regel) und, falls `mode` gesetzt ist,
/// der `DodTaintPolicy` über einer Zelle in diesem Modus.
fn taint_session(
    allow: Arc<CountingApproval>,
    mode: Option<harw_extension_api::approval_mode::ApprovalMode>,
) -> TestResult<AgentSession> {
    let allow: Arc<dyn harw_extension_api::ApprovalHandler> = allow;
    let mut handlers = vec![allow];
    if let Some(mode) = mode {
        handlers.push(taint_policy(mode));
    }
    taint_session_with(handlers, true)
}

/// Die `DodTaintPolicy` über einer Zelle im Modus `mode`.
fn taint_policy(
    mode: harw_extension_api::approval_mode::ApprovalMode,
) -> Arc<dyn harw_extension_api::ApprovalHandler> {
    Arc::new(crate::policy::DodTaintPolicy::new().with_mode(
        harw_extension_api::approval_mode::ApprovalModeCell::new(mode),
    ))
}

/// Sitzung mit genau `handlers`; `approval_actor = false` lässt den
/// Freigabe-Akteur weg (wie ein Job-Worker ohne Oberfläche).
fn taint_session_with(
    handlers: Vec<Arc<dyn harw_extension_api::ApprovalHandler>>,
    approval_actor: bool,
) -> TestResult<AgentSession> {
    taint_session_on(
        StubToolProvider::with_names(TAINT_TOOLS),
        handlers,
        approval_actor,
    )
}

/// Wie [`taint_session_with`], aber über dem Werkzeuganbieter `provider`.
fn taint_session_on(
    provider: Arc<dyn ToolProvider>,
    handlers: Vec<Arc<dyn harw_extension_api::ApprovalHandler>>,
    approval_actor: bool,
) -> TestResult<AgentSession> {
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut builder = ExtensionRegistryBuilder::default().tool_provider(provider);
    for handler in handlers {
        builder = builder.approval_handler(handler);
    }
    let mut context = test_spawn_context()?;
    if !approval_actor {
        context.approval_actor = None;
    }
    Ok(
        AgentSession::new(AgentRole::Assistant, None, builder.build(), tx)
            .with_spawn_context(context),
    )
}

/// Gibt alles frei und meldet `label` — steht für die Ask-Auflösung
/// eines nicht interaktiven Einstiegs (`harw-runtime`), die den Taint
/// nicht kennt.
struct LabelledAllow(&'static str);

impl harw_extension_api::ApprovalHandler for LabelledAllow {
    fn review<'a>(
        &'a self,
        _call: &'a ToolCall,
    ) -> harw_extension_api::ExtFuture<'a, ApprovalDecision> {
        Box::pin(async { ApprovalDecision::Allow })
    }

    fn label(&self) -> &'static str {
        self.0
    }
}

/// Die Antworten „`dod.findings`, dann `gated`, dann Text".
fn dod_then(gated: &ToolCallId, tool: &str) -> Vec<crate::model::ModelResponse> {
    vec![
        response_with(vec![call(&ToolCallId::new(), "dod.findings")]),
        response_with(vec![call(gated, tool)]),
        crate::model::ModelResponse::text("erledigt"),
    ]
}

/// Prüft, dass genau der zweite Aufruf mit dem Taint-Grund abgelehnt
/// wurde (der erste war `dod.findings`).
fn expect_taint_denial(session: &AgentSession) -> TestResult {
    let results = tool_results(session);
    let denied = results
        .get(1)
        .ok_or(TestError::Missing("Ergebnis des taint-pflichtigen Aufrufs"))?;
    match denied {
        ToolCallResult::Error { message }
            if message.starts_with("denied: ")
                && message.contains(crate::dod_taint::TAINT_REASON) =>
        {
            Ok(())
        }
        other => Err(TestError::Unexpected(format!(
            "erwartet eine Taint-Ablehnung, bekommen {other:?}"
        ))),
    }
}

/// Führt einen Turn mit den vorprogrammierten Antworten `script` aus.
async fn run_taint_turn(
    session: &mut AgentSession,
    store: &crate::state_store::InMemoryStateStore,
    script: Vec<crate::model::ModelResponse>,
    control: TurnControl,
) -> TestResult<TurnOutcome> {
    let model = ScriptedModel::new(script);
    Ok(run_turn(
        session,
        &model,
        store,
        TurnInput::user("taint").with_control(control),
    )
    .await?)
}

/// Die Call-Id, auf deren Freigabe der Turn wartet.
fn awaited_call(outcome: &TurnOutcome) -> Option<&ToolCallId> {
    match outcome {
        TurnOutcome::AwaitingApproval { call_id, .. } => Some(call_id),
        _ => None,
    }
}

/// Auto-Modus + `dod.findings`, danach `job.stop` → Rückfrage, obwohl
/// ein anderer Handler freigibt; die Oberfläche bekommt den Grund.
#[tokio::test]
async fn dod_taint_asks_for_job_stop_after_dod_findings_in_auto_mode() -> TestResult {
    let dod = ToolCallId::new();
    let stop = ToolCallId::new();
    let mut session = taint_session(
        Arc::new(CountingApproval::allow_everything()),
        Some(harw_extension_api::approval_mode::ApprovalMode::Delegated),
    )?;
    let store = crate::state_store::InMemoryStateStore::new();

    let outcome = run_taint_turn(
        &mut session,
        &store,
        vec![
            response_with(vec![call(&dod, "dod.findings")]),
            response_with(vec![call(&stop, "job.stop")]),
            crate::model::ModelResponse::text("erledigt"),
        ],
        TurnControl::new(),
    )
    .await?;

    assert_eq!(awaited_call(&outcome), Some(&stop), "{outcome:?}");
    let reason = crate::dod_taint::pending_ask_reason(&session)
        .ok_or(TestError::Missing("Taint-Grund der offenen Rückfrage"))?;
    assert!(reason.contains(crate::dod_taint::TAINT_REASON), "{reason}");
    Ok(())
}

/// Entscheidung O1: unter Full Access fragt auch der Taint nicht.
#[tokio::test]
async fn dod_taint_never_asks_under_full_access() -> TestResult {
    let mut session = taint_session(
        Arc::new(CountingApproval::allow_everything()),
        Some(harw_extension_api::approval_mode::ApprovalMode::FullAccess),
    )?;
    let store = crate::state_store::InMemoryStateStore::new();

    let outcome = run_taint_turn(
        &mut session,
        &store,
        vec![
            response_with(vec![call(&ToolCallId::new(), "dod.findings")]),
            response_with(vec![call(&ToolCallId::new(), "job.stop")]),
            crate::model::ModelResponse::text("erledigt"),
        ],
        TurnControl::new(),
    )
    .await?;

    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    Ok(())
}

/// Ohne `dod.*`-Aufruf bleibt die Entscheidung unverändert.
#[tokio::test]
async fn dod_taint_leaves_turns_without_dod_calls_unchanged() -> TestResult {
    let mut session = taint_session(
        Arc::new(CountingApproval::allow_everything()),
        Some(harw_extension_api::approval_mode::ApprovalMode::Delegated),
    )?;
    let store = crate::state_store::InMemoryStateStore::new();

    let outcome = run_taint_turn(
        &mut session,
        &store,
        vec![
            response_with(vec![call(&ToolCallId::new(), "job.stop")]),
            crate::model::ModelResponse::text("erledigt"),
        ],
        TurnControl::new(),
    )
    .await?;

    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    Ok(())
}

/// Der Taint endet mit seinem Turn — auch wenn der Aufrufer denselben
/// Steuerblock im nächsten Turn wiederverwendet.
#[tokio::test]
async fn dod_taint_ends_with_its_turn() -> TestResult {
    let mut session = taint_session(
        Arc::new(CountingApproval::allow_everything()),
        Some(harw_extension_api::approval_mode::ApprovalMode::Delegated),
    )?;
    let store = crate::state_store::InMemoryStateStore::new();
    let control = TurnControl::new();

    let first = run_taint_turn(
        &mut session,
        &store,
        vec![
            response_with(vec![call(&ToolCallId::new(), "dod.findings")]),
            crate::model::ModelResponse::text("Befunde gelesen"),
        ],
        control.clone(),
    )
    .await?;
    assert!(matches!(first, TurnOutcome::Completed), "{first:?}");

    let second = run_taint_turn(
        &mut session,
        &store,
        vec![
            response_with(vec![call(&ToolCallId::new(), "job.stop")]),
            crate::model::ModelResponse::text("gestoppt"),
        ],
        control,
    )
    .await?;
    assert!(matches!(second, TurnOutcome::Completed), "{second:?}");
    Ok(())
}

/// Lesende Job-Werkzeuge bleiben nach einer DoD-Abfrage frei.
#[tokio::test]
async fn dod_taint_leaves_read_only_job_tools_alone() -> TestResult {
    let mut session = taint_session(
        Arc::new(CountingApproval::allow_everything()),
        Some(harw_extension_api::approval_mode::ApprovalMode::Delegated),
    )?;
    let store = crate::state_store::InMemoryStateStore::new();

    let outcome = run_taint_turn(
        &mut session,
        &store,
        vec![
            response_with(vec![call(&ToolCallId::new(), "dod.findings")]),
            response_with(vec![call(&ToolCallId::new(), "job.logs")]),
            crate::model::ModelResponse::text("erledigt"),
        ],
        TurnControl::new(),
    )
    .await?;

    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    Ok(())
}

/// `process.kill`, `host.sudo_exec` und `job.start` fragen nach einer
/// DoD-Abfrage ebenfalls, auch wenn jeder andere Handler freigibt.
#[tokio::test]
async fn dod_taint_gates_host_tools_and_job_start() -> TestResult {
    for tool in ["process.kill", "host.sudo_exec", "job.start"] {
        let gated = ToolCallId::new();
        let mut session = taint_session(
            Arc::new(CountingApproval::allow_everything()),
            Some(harw_extension_api::approval_mode::ApprovalMode::Delegated),
        )?;
        let store = crate::state_store::InMemoryStateStore::new();

        let outcome = run_taint_turn(
            &mut session,
            &store,
            vec![
                response_with(vec![call(&ToolCallId::new(), "dod.findings")]),
                response_with(vec![call(&gated, tool)]),
                crate::model::ModelResponse::text("erledigt"),
            ],
            TurnControl::new(),
        )
        .await?;

        assert_eq!(awaited_call(&outcome), Some(&gated), "{tool}: {outcome:?}");
    }
    Ok(())
}

/// Ein abgelehnter (nie ausgeführter) `dod.*`-Aufruf taintet den Turn
/// trotzdem (fail-closed).
#[tokio::test]
async fn dod_taint_counts_a_denied_dod_call() -> TestResult {
    let dod = ToolCallId::new();
    let stop = ToolCallId::new();
    let denying = Arc::new(CountingApproval::new(vec![(
        dod.clone(),
        ApprovalDecision::Deny("regel".to_owned()),
    )]));
    let mut session = taint_session(
        denying,
        Some(harw_extension_api::approval_mode::ApprovalMode::Delegated),
    )?;
    let store = crate::state_store::InMemoryStateStore::new();

    let outcome = run_taint_turn(
        &mut session,
        &store,
        vec![
            response_with(vec![call(&dod, "dod.findings")]),
            response_with(vec![call(&stop, "job.stop")]),
            crate::model::ModelResponse::text("erledigt"),
        ],
        TurnControl::new(),
    )
    .await?;

    assert_eq!(awaited_call(&outcome), Some(&stop), "{outcome:?}");
    Ok(())
}

/// Liefert für `dod.*` ein Werkzeugfehler-Ergebnis, für alles andere
/// `ok` — steht für einen DoD-Export, der sich nicht lesen lässt.
struct DodFailsExecutor;

impl ToolExecutor for DodFailsExecutor {
    fn execute<'a>(
        &'a self,
        _ctx: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        let output = if crate::dod_taint::is_dod_tool(call.name.as_str()) {
            ToolOutput::error("dod export unreadable")
        } else {
            ToolOutput::text("ok")
        };
        Box::pin(async move { Ok(output) })
    }
}

/// Bietet [`TAINT_TOOLS`] an; jedes läuft über [`DodFailsExecutor`].
struct DodFailsProvider {
    // Die angebotenen Werkzeuge (wie `StubToolProvider`).
    inner: Arc<StubToolProvider>,
}

impl ToolProvider for DodFailsProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        self.inner.tools()
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        self.inner
            .executor(name)
            .map(|_| Arc::new(DodFailsExecutor) as Arc<dyn ToolExecutor>)
    }
}

/// Ein abgefertigter `dod.*`-Aufruf, der bei der Ausführung scheitert,
/// taintet den Turn ebenfalls — gleich ob der Ausführer einen Fehler
/// liefert oder es gar keinen Ausführer gibt (die Sitzung bietet das
/// Werkzeug nicht an: „no executor for tool").
#[tokio::test]
async fn dod_taint_counts_a_dod_call_that_fails_in_execution() -> TestResult {
    let providers: [(&str, Arc<dyn ToolProvider>); 2] = [
        (
            "Ausführer meldet Fehler",
            Arc::new(DodFailsProvider {
                inner: StubToolProvider::with_names(TAINT_TOOLS),
            }) as Arc<dyn ToolProvider>,
        ),
        (
            "kein Ausführer",
            StubToolProvider::with_names(&["job.stop"]) as Arc<dyn ToolProvider>,
        ),
    ];
    for (case, provider) in providers {
        let stop = ToolCallId::new();
        let allow: Arc<dyn harw_extension_api::ApprovalHandler> =
            Arc::new(CountingApproval::allow_everything());
        let mut session = taint_session_on(
            provider,
            vec![
                allow,
                taint_policy(harw_extension_api::approval_mode::ApprovalMode::Delegated),
            ],
            true,
        )?;
        let store = crate::state_store::InMemoryStateStore::new();

        let outcome = run_taint_turn(
            &mut session,
            &store,
            dod_then(&stop, "job.stop"),
            TurnControl::new(),
        )
        .await?;

        match tool_results(&session).first() {
            Some(ToolCallResult::Error { .. }) => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "{case}: der DoD-Aufruf muss scheitern, Ergebnis {other:?}"
                )));
            }
        }
        assert_eq!(awaited_call(&outcome), Some(&stop), "{case}: {outcome:?}");
        let reason = crate::dod_taint::pending_ask_reason(&session).ok_or(
            TestError::Missing("Taint-Grund nach gescheitertem DoD-Aufruf"),
        )?;
        assert!(
            reason.contains(crate::dod_taint::TAINT_REASON),
            "{case}: {reason}"
        );
    }
    Ok(())
}

/// Der Taint gilt für den **Rest** des Turns und überlebt die
/// Rückfrage-Pause: nach Freigabe wie nach Ablehnung des ersten
/// `job.stop` fragt ein zweites `job.stop` im selben Turn erneut, mit dem
/// Taint-Grund (der Steuerblock des Turns wird beim Fortsetzen
/// wiederverwendet).
#[tokio::test]
async fn dod_taint_survives_the_approval_pause_for_the_rest_of_the_turn() -> TestResult {
    for resolution in [
        ApprovalResolution::Approve,
        ApprovalResolution::Reject {
            reason: "nicht stoppen".to_owned(),
        },
    ] {
        let first = ToolCallId::new();
        let second = ToolCallId::new();
        let mut session = taint_session(
            Arc::new(CountingApproval::allow_everything()),
            Some(harw_extension_api::approval_mode::ApprovalMode::Delegated),
        )?;
        let store = crate::state_store::InMemoryStateStore::new();

        let paused = run_taint_turn(
            &mut session,
            &store,
            dod_then(&first, "job.stop"),
            TurnControl::new(),
        )
        .await?;
        assert_eq!(awaited_call(&paused), Some(&first), "{paused:?}");
        let turn = session
            .current_turn()
            .cloned()
            .ok_or(TestError::Missing("laufender Turn in der Pause"))?;

        let model = ScriptedModel::new(vec![
            response_with(vec![call(&second, "job.stop")]),
            crate::model::ModelResponse::text("erledigt"),
        ]);
        let resumed = resume_after_approval(
            &mut session,
            &model,
            &store,
            ApprovalActor::Operator {
                id: "test-operator".to_owned(),
            },
            resolution.clone(),
        )
        .await?;

        assert_eq!(
            awaited_call(&resumed),
            Some(&second),
            "{resolution:?}: {resumed:?}"
        );
        assert_eq!(
            session.current_turn(),
            Some(&turn),
            "{resolution:?}: derselbe Turn"
        );
        let reason = crate::dod_taint::pending_ask_reason(&session)
            .ok_or(TestError::Missing("Taint-Grund der zweiten Rückfrage"))?;
        assert!(
            reason.contains(crate::dod_taint::TAINT_REASON),
            "{resolution:?}: {reason}"
        );
    }
    Ok(())
}

/// Ohne installierte `DodTaintPolicy` fragt der Turn-Loop selbst
/// (modusblind, fail-closed).
#[tokio::test]
async fn dod_taint_fallback_asks_without_an_installed_policy() -> TestResult {
    let stop = ToolCallId::new();
    let mut session = taint_session(Arc::new(CountingApproval::allow_everything()), None)?;
    let store = crate::state_store::InMemoryStateStore::new();

    let outcome = run_taint_turn(
        &mut session,
        &store,
        vec![
            response_with(vec![call(&ToolCallId::new(), "dod.findings")]),
            response_with(vec![call(&stop, "job.stop")]),
            crate::model::ModelResponse::text("erledigt"),
        ],
        TurnControl::new(),
    )
    .await?;

    assert_eq!(awaited_call(&outcome), Some(&stop), "{outcome:?}");
    let reason = crate::dod_taint::pending_ask_reason(&session)
        .ok_or(TestError::Missing("Taint-Grund der Fallback-Rückfrage"))?;
    assert!(reason.contains(crate::dod_taint::TAINT_REASON), "{reason}");
    Ok(())
}

/// Ohne Freigabe-Akteur (Job-Worker) lehnt der Fallback ab, statt eine
/// Rückfrage zu öffnen, die den Turn mit `MissingApprovalActor`
/// scheitern ließe; der Turn läuft weiter.
#[tokio::test]
async fn dod_taint_denies_instead_of_asking_without_an_approval_actor() -> TestResult {
    for mode in [
        None,
        Some(harw_extension_api::approval_mode::ApprovalMode::Delegated),
    ] {
        let allow: Arc<dyn harw_extension_api::ApprovalHandler> =
            Arc::new(CountingApproval::allow_everything());
        let mut handlers = vec![allow];
        if let Some(mode) = mode {
            handlers.push(taint_policy(mode));
        }
        let mut session = taint_session_with(handlers, false)?;
        let store = crate::state_store::InMemoryStateStore::new();

        let outcome = run_taint_turn(
            &mut session,
            &store,
            dod_then(&ToolCallId::new(), "job.stop"),
            TurnControl::new(),
        )
        .await?;

        assert!(
            matches!(outcome, TurnOutcome::Completed),
            "{mode:?}: {outcome:?}"
        );
        expect_taint_denial(&session)?;
    }
    Ok(())
}

/// Ein nicht interaktiver Einstieg (Ask-Auflösung `ask:*` in der Kette,
/// die den Taint nicht kennt) bekommt eine deterministische Ablehnung
/// statt einer Pause — mit installierter Politik wie im Fallback.
#[tokio::test]
async fn dod_taint_denies_in_a_non_interactive_entry() -> TestResult {
    for label in crate::policy::NON_INTERACTIVE_ASK_LABELS {
        for mode in [
            None,
            Some(harw_extension_api::approval_mode::ApprovalMode::Delegated),
        ] {
            let allow: Arc<dyn harw_extension_api::ApprovalHandler> =
                Arc::new(CountingApproval::allow_everything());
            let mut handlers: Vec<Arc<dyn harw_extension_api::ApprovalHandler>> =
                vec![allow, Arc::new(LabelledAllow(label))];
            if let Some(mode) = mode {
                handlers.push(taint_policy(mode));
            }
            let mut session = taint_session_with(handlers, true)?;
            let store = crate::state_store::InMemoryStateStore::new();

            let outcome = run_taint_turn(
                &mut session,
                &store,
                dod_then(&ToolCallId::new(), "job.stop"),
                TurnControl::new(),
            )
            .await?;

            assert!(
                matches!(outcome, TurnOutcome::Completed),
                "{label} {mode:?}: {outcome:?}"
            );
            expect_taint_denial(&session)?;
        }
    }
    Ok(())
}

/// Fragt unter Full Access ein anderer Handler, nennt die Oberfläche den
/// Taint-Grund nicht: der Taint hat diese Rückfrage nicht verursacht.
#[tokio::test]
async fn dod_taint_reason_is_not_claimed_for_an_ask_it_did_not_cause() -> TestResult {
    let stop = ToolCallId::new();
    let asking: Arc<dyn harw_extension_api::ApprovalHandler> =
        Arc::new(CountingApproval::new(vec![(
            stop.clone(),
            ApprovalDecision::AskUser(harw_types::ItemId::new()),
        )]));
    let mut session = taint_session_with(
        vec![
            asking,
            taint_policy(harw_extension_api::approval_mode::ApprovalMode::FullAccess),
        ],
        true,
    )?;
    let store = crate::state_store::InMemoryStateStore::new();

    let outcome = run_taint_turn(
        &mut session,
        &store,
        dod_then(&stop, "job.stop"),
        TurnControl::new(),
    )
    .await?;

    assert_eq!(awaited_call(&outcome), Some(&stop), "{outcome:?}");
    assert_eq!(crate::dod_taint::pending_ask_reason(&session), None);
    Ok(())
}

/// Stehen `dod.*` und ein taint-pflichtiger Aufruf in derselben Antwort,
/// läuft sie sequenziell: die Vorprüfung entscheidet nichts vorab, der
/// spätere Aufruf sieht den Taint, und kein Handler wird doppelt gefragt.
#[tokio::test]
async fn dod_taint_serialises_a_mixed_parallel_response() -> TestResult {
    let dod = ToolCallId::new();
    let stop = ToolCallId::new();
    let executions = Arc::new(AtomicUsize::new(0));
    let allow = Arc::new(CountingApproval::allow_everything());
    let provider =
        StubParallelProvider::instant(&["dod.findings", "job.stop"], Arc::clone(&executions));
    let mut session = guarded_session(provider, Arc::clone(&allow))?;
    let store = crate::state_store::InMemoryStateStore::new();

    let outcome = run_taint_turn(
        &mut session,
        &store,
        vec![
            response_with(vec![call(&dod, "dod.findings"), call(&stop, "job.stop")]),
            crate::model::ModelResponse::text("erledigt"),
        ],
        TurnControl::new(),
    )
    .await?;

    assert_eq!(awaited_call(&outcome), Some(&stop), "{outcome:?}");
    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "nur dod.findings lief"
    );
    assert_eq!(allow.reviews_for(&dod), 1);
    assert_eq!(allow.reviews_for(&stop), 1);
    Ok(())
}

#[tokio::test]
async fn parallel_preflight_aggregates_a_later_deny_over_an_earlier_ask_user() -> TestResult {
    // Der Parallel-Vorprüfpfad muss identisch aggregieren: statt einer
    // Approval-Pause (früheres `AskUser`) wird der Call abgelehnt, der
    // erlaubte Call läuft, und kein Handler wird doppelt gefragt.
    let allowed = ToolCallId::new();
    let contested = ToolCallId::new();
    let executions = Arc::new(AtomicUsize::new(0));
    let asking = Arc::new(CountingApproval::new(vec![(
        contested.clone(),
        ApprovalDecision::AskUser(harw_types::ItemId::new()),
    )]));
    let denying = Arc::new(CountingApproval::new(vec![(
        contested.clone(),
        ApprovalDecision::Deny("policy".to_owned()),
    )]));
    let provider = StubParallelProvider::instant(&["lookup"], Arc::clone(&executions));
    let mut session =
        multi_guarded_session(provider, &[Arc::clone(&asking), Arc::clone(&denying)])?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![
        response_with(vec![call(&allowed, "lookup"), call(&contested, "lookup")]),
        crate::model::ModelResponse::text("weiter ohne den abgelehnten Call"),
    ]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("aggregate"))
        .await
        .map_err(ctx("ein Deny beendet den Turn nicht"))?;

    assert!(
        matches!(outcome, TurnOutcome::Completed),
        "Deny muss AskUser schlagen, der Turn darf nicht pausieren: {outcome:?}"
    );
    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "nur der erlaubte Call darf ausgeführt werden"
    );
    for handler in [&asking, &denying] {
        assert_eq!(handler.reviews_for(&allowed), 1);
        assert_eq!(handler.reviews_for(&contested), 1);
        assert_eq!(handler.total_reviews(), 2);
    }
    Ok(())
}

#[test]
fn prepared_approvals_yield_each_decision_at_most_once() {
    let mut prepared = PreparedApprovals {
        decisions: vec![Some(ApprovalDecision::Allow), None],
    };

    assert!(matches!(prepared.take(0), Some(ApprovalDecision::Allow)));
    assert!(
        prepared.take(0).is_none(),
        "eine verbrauchte Entscheidung darf nicht erneut geliefert werden"
    );
    assert!(prepared.take(1).is_none());
    assert!(prepared.take(99).is_none(), "ein Index außerhalb ist None");
}

// ------------------------------------------------------------------
// W2-14: Child-Events
// ------------------------------------------------------------------

#[test]
fn child_question_accepts_only_a_string_field() {
    assert_eq!(
        child_question(&serde_json::json!({"question": "welche Datei?"})),
        Some("welche Datei?".to_owned())
    );
    assert_eq!(child_question(&serde_json::json!({"question": 7})), None);
    assert_eq!(
        child_question(&serde_json::json!({"question": {"text": "x"}})),
        None
    );
    assert_eq!(child_question(&serde_json::Value::Null), None);
}

#[test]
fn child_question_prefers_the_task_field() {
    assert_eq!(
        child_question(&serde_json::json!({"task": "Baue X"})),
        Some("Baue X".to_owned())
    );
    assert_eq!(
        child_question(&serde_json::json!({"task": "Baue X", "question": "alt"})),
        Some("Baue X".to_owned())
    );
    assert_eq!(
        child_question(&serde_json::json!({"task": "  ", "question": "alt"})),
        Some("alt".to_owned())
    );
    assert_eq!(child_question(&serde_json::json!({"task": 3})), None);
}

struct FixedChildSpawner {
    child: SessionId,
}

impl AgentSpawner for FixedChildSpawner {
    fn spawn_child<'a>(
        &'a self,
        _role: &'a str,
        _input: SpawnInput,
        _sandbox: SandboxSpec,
        _suggestions: Option<AgentSuggestions>,
    ) -> SpawnFuture<'a> {
        let child = self.child.clone();
        Box::pin(async move { Ok(child) })
    }
}

#[tokio::test]
async fn handoff_emits_child_spawned_with_role_and_question() -> TestResult {
    let child = SessionId::new();
    let (tx, _rx) = mpsc::unbounded_channel();
    let (turn_tx, mut turn_rx) = mpsc::unbounded_channel();
    let registry = ExtensionRegistryBuilder::default()
        .spawner(Arc::new(FixedChildSpawner {
            child: child.clone(),
        }))
        .agent_job_submitter(RecordingJobSubmitter::new())
        .build();
    let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx)
        .with_spawn_context(test_spawn_context()?)
        .with_turn_event_sink(turn_tx);
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![
        response_with(vec![ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("transfer_to_worker"),
            arguments: serde_json::json!({"question": "wo liegt der Fehler?"}),
        }]),
        crate::model::ModelResponse::text("started"),
    ]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("handoff"))
        .await
        .map_err(ctx("the background start does not pause the turn"))?;

    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    let spawned = drain_turn_events(&mut turn_rx)
        .into_iter()
        .find(|event| matches!(event, TurnEvent::ChildSpawned { .. }))
        .ok_or(TestError::Missing("ein Handoff meldet ein ChildSpawned"))?;
    match spawned {
        TurnEvent::ChildSpawned {
            child: spawned_child,
            role,
            question,
            ..
        } => {
            assert_eq!(spawned_child, child);
            assert_eq!(role, "worker");
            assert_eq!(question.as_deref(), Some("wo liegt der Fehler?"));
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "erwartet wurde ChildSpawned, nicht {other:?}"
            )));
        }
    }
    Ok(())
}

/// Runde 5, Teil K: ein Spawner, der jede Admission mit einer
/// Orchestrierungsgrenze ablehnt.
struct LimitRejectingSpawner;

impl AgentSpawner for LimitRejectingSpawner {
    fn spawn_child<'a>(
        &'a self,
        _role: &'a str,
        _input: SpawnInput,
        _sandbox: SandboxSpec,
        _suggestions: Option<AgentSuggestions>,
    ) -> SpawnFuture<'a> {
        Box::pin(async move {
            Err(harw_extension_api::AgentSpawnError {
                kind: Default::default(),
                message: format!(
                    "{} Grenze max_root_orchestrators=1 erreicht — es läuft bereits: \
                         root-orchestrator (r1).",
                    crate::background_children::ORCHESTRATION_LIMIT_MARKER
                ),
            })
        })
    }
}

/// Ein Spawner mit genau einem laufenden Hintergrund-Kind, dessen
/// Abschlussmeldung ab der `ready_after`-ten Abfrage bereitliegt, bis sie
/// bestätigt wird.
struct BackgroundResultSpawner {
    polls: AtomicUsize,
    ready_after: usize,
    child: harw_types::SessionId,
    acknowledged: AtomicUsize,
    cancelled: AtomicUsize,
}

impl BackgroundResultSpawner {
    fn new(ready_after: usize) -> Arc<Self> {
        Arc::new(Self {
            polls: AtomicUsize::new(0),
            ready_after,
            child: harw_types::SessionId::new(),
            acknowledged: AtomicUsize::new(0),
            cancelled: AtomicUsize::new(0),
        })
    }

    fn done(&self) -> bool {
        self.acknowledged.load(Ordering::SeqCst) > 0
            || self.cancelled.load(Ordering::SeqCst) > 0
    }
}

impl AgentSpawner for BackgroundResultSpawner {
    fn spawn_child<'a>(
        &'a self,
        _role: &'a str,
        _input: SpawnInput,
        _sandbox: SandboxSpec,
        _suggestions: Option<AgentSuggestions>,
    ) -> SpawnFuture<'a> {
        Box::pin(async move { Err(harw_extension_api::AgentSpawnError::new("unused")) })
    }

    fn pending_background_notices(
        &self,
        _parent: &harw_types::SessionId,
    ) -> Vec<harw_extension_api::PendingBackgroundNotice> {
        let polls = self.polls.fetch_add(1, Ordering::SeqCst);
        if polls >= self.ready_after && !self.done() {
            vec![harw_extension_api::PendingBackgroundNotice {
                child: self.child.clone(),
                text: "[Hintergrund-Agent explorer k1 fertig nach 3 s]\nBefund X".to_owned(),
            }]
        } else {
            Vec::new()
        }
    }

    fn acknowledge_background_notices(
        &self,
        _parent: &harw_types::SessionId,
        children: &[harw_types::SessionId],
    ) {
        if children.contains(&self.child) {
            self.acknowledged.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn running_background_children(&self, _parent: &harw_types::SessionId) -> usize {
        usize::from(!self.done())
    }

    fn cancel_background_children(
        &self,
        _parent: &harw_types::SessionId,
        _reason: &str,
    ) -> usize {
        self.cancelled.fetch_add(1, Ordering::SeqCst);
        1
    }
}

fn orchestrator_child_session(
    spawner: &Arc<BackgroundResultSpawner>,
) -> TestResult<AgentSession> {
    let (tx, _rx) = mpsc::unbounded_channel();
    let registry = ExtensionRegistryBuilder::default()
        .spawner(Arc::clone(spawner) as Arc<dyn AgentSpawner>)
        .build();
    Ok(AgentSession::new(
        AgentRole::Assistant,
        Some(harw_types::SessionId::new()),
        registry,
        tx,
    )
    .with_spawn_context(test_spawn_context()?))
}

fn user_texts_containing(session: &AgentSession, needle: &str) -> usize {
    session
        .history()
        .items()
        .iter()
        .filter(|item| match item {
            TurnItem::UserMessage(message) => format!("{:?}", message.content).contains(needle),
            _ => false,
        })
        .count()
}

#[tokio::test]
async fn an_orchestrator_child_waits_for_its_background_children_before_finishing() -> TestResult
{
    // Abfrage 0: Rundenbeginn, 1: Turn-Ende (läuft noch, eine
    // Warteschleife), 2: Ergebnis da.
    let spawner = BackgroundResultSpawner::new(2);
    let mut session = orchestrator_child_session(&spawner)?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![
        crate::model::ModelResponse::text("Welle gestartet, warte auf Ergebnisse."),
        crate::model::ModelResponse::text("Synthese mit Befund X."),
    ]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("auftrag"))
        .await
        .map_err(ctx("der Turn läuft durch"))?;

    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    assert_eq!(user_texts_containing(&session, "Befund X"), 1);
    assert_eq!(spawner.acknowledged.load(Ordering::SeqCst), 1);
    assert_eq!(spawner.cancelled.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn a_cancellation_while_waiting_is_a_cancel_not_a_success() -> TestResult {
    let spawner = BackgroundResultSpawner::new(usize::MAX);
    let mut session = orchestrator_child_session(&spawner)?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![crate::model::ModelResponse::text("warte")]);
    let token = crate::cancel::CancelToken::new();
    let control = TurnControl::new().with_cancel(token.clone());
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        token.cancel(CancelReason::User);
    });

    let outcome = run_turn(
        &mut session,
        &model,
        &store,
        TurnInput::user("auftrag").with_control(control),
    )
    .await
    .map_err(ctx("der Turn endet mit Abbruch"))?;
    canceller
        .await
        .map_err(|error| TestError::Unexpected(error.to_string()))?;

    assert!(
        matches!(
            outcome,
            TurnOutcome::Cancelled {
                reason: CancelReason::User
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        spawner.cancelled.load(Ordering::SeqCst),
        1,
        "die wartenden Hintergrund-Kinder werden mit abgebrochen"
    );
    Ok(())
}

/// Ein Kind ist fertig (Meldung wartet, sobald `ready`), ein Geschwister
/// läuft noch.
struct TwoChildrenSpawner {
    finished: harw_types::SessionId,
    ready: std::sync::atomic::AtomicBool,
    acknowledged: AtomicUsize,
    cancelled: AtomicUsize,
}

impl TwoChildrenSpawner {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            finished: harw_types::SessionId::new(),
            ready: std::sync::atomic::AtomicBool::new(false),
            acknowledged: AtomicUsize::new(0),
            cancelled: AtomicUsize::new(0),
        })
    }
}

impl AgentSpawner for TwoChildrenSpawner {
    fn spawn_child<'a>(
        &'a self,
        _role: &'a str,
        _input: SpawnInput,
        _sandbox: SandboxSpec,
        _suggestions: Option<AgentSuggestions>,
    ) -> SpawnFuture<'a> {
        Box::pin(async move { Err(harw_extension_api::AgentSpawnError::new("unused")) })
    }

    fn pending_background_notices(
        &self,
        _parent: &harw_types::SessionId,
    ) -> Vec<harw_extension_api::PendingBackgroundNotice> {
        if self.ready.load(Ordering::SeqCst) && self.acknowledged.load(Ordering::SeqCst) == 0 {
            vec![harw_extension_api::PendingBackgroundNotice {
                child: self.finished.clone(),
                text: "[Hintergrund-Agent explorer k1 fertig nach 3 s]\nBefund Y".to_owned(),
            }]
        } else {
            Vec::new()
        }
    }

    fn acknowledge_background_notices(
        &self,
        _parent: &harw_types::SessionId,
        children: &[harw_types::SessionId],
    ) {
        if children.contains(&self.finished) {
            self.acknowledged.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn running_background_children(&self, _parent: &harw_types::SessionId) -> usize {
        usize::from(self.cancelled.load(Ordering::SeqCst) == 0)
    }

    fn cancel_background_children(
        &self,
        _parent: &harw_types::SessionId,
        _reason: &str,
    ) -> usize {
        self.cancelled.fetch_add(1, Ordering::SeqCst);
        1
    }
}

#[tokio::test]
async fn a_cancelled_parent_keeps_a_finished_result_and_cancels_the_running_sibling()
-> TestResult {
    let spawner = TwoChildrenSpawner::new();
    let (tx, _rx) = mpsc::unbounded_channel();
    let registry = ExtensionRegistryBuilder::default()
        .spawner(Arc::clone(&spawner) as Arc<dyn AgentSpawner>)
        .build();
    let mut session = AgentSession::new(
        AgentRole::Assistant,
        Some(harw_types::SessionId::new()),
        registry,
        tx,
    )
    .with_spawn_context(test_spawn_context()?);
    let store = crate::state_store::InMemoryStateStore::new();
    // Der Token ist schon abgebrochen, wenn das Turn-Ende erreicht wird:
    // der Abbruch kommt nach der Modellantwort, vor dem Warten.
    let token = crate::cancel::CancelToken::new();
    let model = ScriptedModel::new(vec![crate::model::ModelResponse::text("warte auf Kinder")]);
    let control = TurnControl::new().with_cancel(token.clone());
    // Während das Turn-Ende wartet: ein Kind wird fertig, dann kommt der
    // Abbruch — die nächste Prüfung sieht beides zugleich.
    let _finisher = finish_then_cancel(Arc::clone(&spawner), token);

    let outcome = run_turn(
        &mut session,
        &model,
        &store,
        TurnInput::user("auftrag").with_control(control),
    )
    .await
    .map_err(ctx("der Turn endet mit Abbruch"))?;

    assert!(
        matches!(
            outcome,
            TurnOutcome::Cancelled {
                reason: CancelReason::User
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        spawner.acknowledged.load(Ordering::SeqCst),
        1,
        "das fertige Ergebnis wird geschrieben und bestätigt"
    );
    assert_eq!(user_texts_containing(&session, "Befund Y"), 1);
    assert_eq!(
        spawner.cancelled.load(Ordering::SeqCst),
        1,
        "das laufende Geschwister wird mit abgebrochen"
    );
    Ok(())
}

#[tokio::test]
async fn a_failed_write_on_cancel_still_cancels_the_sibling_and_keeps_the_notice() -> TestResult
{
    let spawner = TwoChildrenSpawner::new();
    let (tx, _rx) = mpsc::unbounded_channel();
    let registry = ExtensionRegistryBuilder::default()
        .spawner(Arc::clone(&spawner) as Arc<dyn AgentSpawner>)
        .build();
    let mut session = AgentSession::new(
        AgentRole::Assistant,
        Some(harw_types::SessionId::new()),
        registry,
        tx,
    )
    .with_spawn_context(test_spawn_context()?);
    let store = FailOnceStore {
        inner: crate::state_store::InMemoryStateStore::new(),
        needle: "Befund Y",
        failed: std::sync::atomic::AtomicBool::new(false),
    };
    let token = crate::cancel::CancelToken::new();
    let model = ScriptedModel::new(vec![crate::model::ModelResponse::text("warte auf Kinder")]);
    let control = TurnControl::new().with_cancel(token.clone());
    // Während das Turn-Ende wartet: ein Kind wird fertig, dann kommt der
    // Abbruch — die nächste Prüfung sieht beides zugleich.
    let _finisher = finish_then_cancel(Arc::clone(&spawner), token);

    let outcome = run_turn(
        &mut session,
        &model,
        &store,
        TurnInput::user("auftrag").with_control(control),
    )
    .await;

    assert!(
        outcome.is_err(),
        "der Schreibfehler bleibt sichtbar: {outcome:?}"
    );
    assert_eq!(
        spawner.cancelled.load(Ordering::SeqCst),
        1,
        "das laufende Geschwister wird trotz Schreibfehler genau einmal abgebrochen"
    );
    assert_eq!(
        spawner.acknowledged.load(Ordering::SeqCst),
        0,
        "die nicht geschriebene Meldung bleibt unbestätigt im Register"
    );
    assert_eq!(user_texts_containing(&session, "Befund Y"), 0);
    Ok(())
}

/// Macht nach kurzer Zeit ein Kind fertig und bricht dann den Token ab.
fn finish_then_cancel(
    spawner: Arc<TwoChildrenSpawner>,
    token: crate::cancel::CancelToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        spawner.ready.store(true, Ordering::SeqCst);
        token.cancel(CancelReason::User);
    })
}

#[tokio::test]
async fn wall_time_expiry_while_waiting_ends_as_budget() -> TestResult {
    let spawner = BackgroundResultSpawner::new(usize::MAX);
    let mut session = orchestrator_child_session(&spawner)?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![crate::model::ModelResponse::text("warte")]);
    let control = TurnControl::new().with_limits(TurnLimits {
        wall_time: Duration::from_secs(1),
        ..TurnLimits::unlimited()
    });

    let outcome = run_turn(
        &mut session,
        &model,
        &store,
        TurnInput::user("auftrag").with_control(control),
    )
    .await
    .map_err(ctx("der Turn endet mit Budget-Ende"))?;

    assert!(
        matches!(
            outcome,
            TurnOutcome::Cancelled {
                reason: CancelReason::Budget
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(spawner.cancelled.load(Ordering::SeqCst), 1);
    Ok(())
}

/// Scheitert genau einmal beim Schreiben einer Nutzer-Nachricht, die
/// `needle` enthält; sonst wie ein `InMemoryStateStore`.
struct FailOnceStore {
    inner: crate::state_store::InMemoryStateStore,
    needle: &'static str,
    failed: std::sync::atomic::AtomicBool,
}

impl StateStore for FailOnceStore {
    fn save_turn<'a>(
        &'a self,
        sid: &'a SessionId,
        item: &'a TurnItem,
    ) -> harw_extension_api::ExtFuture<'a, crate::state_store::StateStoreResult<()>> {
        let hit = matches!(item, TurnItem::UserMessage(message)
            if format!("{:?}", message.content).contains(self.needle));
        if hit && !self.failed.swap(true, Ordering::SeqCst) {
            return Box::pin(async {
                Err(crate::state_store::StateStoreError::PoisonedMutex {
                    operation: "test_fail_once",
                })
            });
        }
        self.inner.save_turn(sid, item)
    }

    fn load_history<'a>(
        &'a self,
        sid: &'a SessionId,
    ) -> harw_extension_api::ExtFuture<
        'a,
        crate::state_store::StateStoreResult<crate::history::ConversationHistory>,
    > {
        self.inner.load_history(sid)
    }
}

#[tokio::test]
async fn a_failed_write_keeps_the_notice_for_exactly_one_later_delivery() -> TestResult {
    let spawner = BackgroundResultSpawner::new(0);
    let mut session = orchestrator_child_session(&spawner)?;
    let store = FailOnceStore {
        inner: crate::state_store::InMemoryStateStore::new(),
        needle: "Befund X",
        failed: std::sync::atomic::AtomicBool::new(false),
    };

    let first = run_turn(
        &mut session,
        &ScriptedModel::new(vec![crate::model::ModelResponse::text("eins")]),
        &store,
        TurnInput::user("auftrag"),
    )
    .await;
    assert!(first.is_err(), "der Schreibfehler bricht den Turn ab");
    assert_eq!(
        spawner.acknowledged.load(Ordering::SeqCst),
        0,
        "nicht geschriebene Meldungen werden nicht bestätigt"
    );
    assert_eq!(user_texts_containing(&session, "Befund X"), 0);

    // Der reguläre Weg aus `Failed`: Recovery, dann der Neuversuch.
    session
        .recover()
        .map_err(ctx("die Session lässt sich wiederherstellen"))?;
    let second = run_turn(
        &mut session,
        &ScriptedModel::new(vec![crate::model::ModelResponse::text("Synthese")]),
        &store,
        TurnInput::user("weiter"),
    )
    .await
    .map_err(ctx("der Neuversuch läuft durch"))?;
    assert!(matches!(second, TurnOutcome::Completed), "{second:?}");
    assert_eq!(user_texts_containing(&session, "Befund X"), 1);
    assert_eq!(spawner.acknowledged.load(Ordering::SeqCst), 1);
    Ok(())
}

/// Runde 5, Teil K: eine Orchestrierungsgrenze bricht den Turn nicht ab,
/// sondern kommt als Werkzeugfehler beim Modell an.
#[tokio::test]
async fn an_orchestration_limit_rejection_is_a_tool_error_not_a_turn_failure() -> TestResult {
    let (tx, _rx) = mpsc::unbounded_channel();
    let registry = ExtensionRegistryBuilder::default()
        .spawner(Arc::new(LimitRejectingSpawner))
        .agent_job_submitter(RecordingJobSubmitter::new())
        .build();
    let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx)
        .with_spawn_context(test_spawn_context()?);
    let store = crate::state_store::InMemoryStateStore::new();
    let call_id = ToolCallId::new();
    let model = ScriptedModel::new(vec![
        response_with(vec![ToolCall {
            id: call_id.clone(),
            name: ToolName::new("transfer_to_root-orchestrator"),
            arguments: serde_json::json!({"task": "zweiter Lauf"}),
        }]),
        response_with(Vec::new()),
    ]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("los")).await;
    assert!(
        !matches!(outcome, Err(CoreError::HandoffFailed { .. })),
        "die Grenze darf den Turn nicht als Handoff-Fehler beenden: {outcome:?}"
    );
    let result = session
        .history()
        .items()
        .iter()
        .find_map(|item| match item {
            TurnItem::ToolResult(result) if result.call_id == call_id => {
                Some(result.result.clone())
            }
            _ => None,
        })
        .ok_or(TestError::Missing(
            "der abgelehnte Handoff bekommt ein Werkzeugergebnis",
        ))?;
    match result {
        ToolCallResult::Error { message } => {
            assert!(crate::background_children::is_orchestration_limit_rejection(&message));
            assert!(message.contains("root-orchestrator (r1)"));
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "erwartet ein Werkzeugfehler, nicht {other:?}"
            )));
        }
    }
    Ok(())
}

#[tokio::test]
async fn handoff_with_only_a_task_argument_reports_the_task() -> TestResult {
    let child = SessionId::new();
    let (tx, _rx) = mpsc::unbounded_channel();
    let (turn_tx, mut turn_rx) = mpsc::unbounded_channel();
    let registry = ExtensionRegistryBuilder::default()
        .spawner(Arc::new(FixedChildSpawner {
            child: child.clone(),
        }))
        .agent_job_submitter(RecordingJobSubmitter::new())
        .build();
    let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx)
        .with_spawn_context(test_spawn_context()?)
        .with_turn_event_sink(turn_tx);
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![
        response_with(vec![ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("transfer_to_reviewer"),
            arguments: serde_json::json!({"task": "review"}),
        }]),
        crate::model::ModelResponse::text("started"),
    ]);

    run_turn(&mut session, &model, &store, TurnInput::user("handoff"))
        .await
        .map_err(ctx("der Handoff pausiert den Turn"))?;

    let spawned = drain_turn_events(&mut turn_rx)
        .into_iter()
        .find(|event| matches!(event, TurnEvent::ChildSpawned { .. }))
        .ok_or(TestError::Missing("ein Handoff meldet ein ChildSpawned"))?;
    // Teil C: `task` ist der angezeigte Auftrag — früher stand hier
    // `None`, und die Oberfläche zeigte „(kein Auftrag angegeben)".
    assert!(matches!(
        spawned,
        TurnEvent::ChildSpawned { question: Some(question), role, .. }
            if role == "reviewer" && question == "review"
    ));
    Ok(())
}

#[tokio::test]
async fn child_completion_emits_child_completed_after_the_result_is_persisted() -> TestResult {
    let (tx, _rx) = mpsc::unbounded_channel();
    let (turn_tx, mut turn_rx) = mpsc::unbounded_channel();
    let child = SessionId::new();
    let registry = ExtensionRegistryBuilder::default()
        .spawner(Arc::new(FixedChildSpawner {
            child: child.clone(),
        }))
        .build();
    let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx)
        .with_spawn_context(test_spawn_context()?)
        .with_turn_event_sink(turn_tx);
    session.try_start_turn().map_err(ctx("Turn startet"))?;
    let call_id = ToolCallId::new();
    session
        .begin_handoff(child.clone(), call_id.clone(), "worker".to_owned())
        .map_err(ctx("Handoff pausiert"))?;
    let store = crate::state_store::InMemoryStateStore::new();

    let outcome = resume_after_child(
        &mut session,
        &crate::model::EchoModelProvider::new("Eltern-Turn läuft weiter"),
        &store,
        child.clone(),
        call_id,
        ToolCallResult::success(serde_json::json!({"child": "done"})),
    )
    .await
    .map_err(ctx("das Kind-Ergebnis nimmt den Eltern-Turn wieder auf"))?;

    assert!(matches!(outcome, TurnOutcome::Completed));
    let completed = drain_turn_events(&mut turn_rx)
        .into_iter()
        .find(|event| matches!(event, TurnEvent::ChildCompleted { .. }))
        .ok_or(TestError::Missing(
            "ein terminiertes Kind meldet ChildCompleted",
        ))?;
    match completed {
        TurnEvent::ChildCompleted {
            child: completed_child,
            outcome,
            duration_ms,
            ..
        } => {
            assert_eq!(completed_child, child);
            assert_eq!(outcome, "completed");
            assert_eq!(
                duration_ms, 0,
                "die Laufzeit des Kindes ist an dieser Stelle nicht bekannt \
                     und wird deshalb nicht geraten"
            );
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "erwartet wurde ChildCompleted, nicht {other:?}"
            )));
        }
    }
    Ok(())
}

/// Runde 5, Teil O: der Handoff-Aufruf bekommt sein echtes Ergebnis als
/// `ToolCallCompleted` (gleiche `call_id`, gleiche Dauer wie
/// `ChildCompleted`), und der persistierte `ToolResult` trägt dieselbe
/// Dauer statt fest 0 ms. Ohne das Ereignis blieb die Zelle der TUI auf
/// „läuft“ und wurde später als „unvollständig (abgebrochen)“ markiert.
#[tokio::test]
async fn child_completion_reports_the_real_handoff_result_with_its_duration() -> TestResult {
    let (tx, _rx) = mpsc::unbounded_channel();
    let (turn_tx, mut turn_rx) = mpsc::unbounded_channel();
    let child = SessionId::new();
    let registry = ExtensionRegistryBuilder::default()
        .spawner(Arc::new(FixedChildSpawner {
            child: child.clone(),
        }))
        .build();
    let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx)
        .with_spawn_context(test_spawn_context()?)
        .with_turn_event_sink(turn_tx);
    session.try_start_turn().map_err(ctx("Turn startet"))?;
    let call_id = ToolCallId::new();
    session
        .begin_handoff(
            child.clone(),
            call_id.clone(),
            "root-orchestrator".to_owned(),
        )
        .map_err(ctx("Handoff pausiert"))?;
    let store = crate::state_store::InMemoryStateStore::new();

    resume_after_child(
        &mut session,
        &crate::model::EchoModelProvider::new("Zusammenfassung"),
        &store,
        child.clone(),
        call_id.clone(),
        ToolCallResult::success(serde_json::json!({"report": "Exploration fertig"})),
    )
    .await
    .map_err(ctx("das Kind-Ergebnis nimmt den Eltern-Turn wieder auf"))?;

    let events = drain_turn_events(&mut turn_rx);
    let child_duration = events
        .iter()
        .find_map(|event| match event {
            TurnEvent::ChildCompleted { duration_ms, .. } => Some(*duration_ms),
            _ => None,
        })
        .ok_or(TestError::Missing("ChildCompleted"))?;
    let (result, tool_duration) = events
        .iter()
        .find_map(|event| match event {
            TurnEvent::ToolCallCompleted {
                call_id: completed,
                result,
                duration_ms,
                ..
            } if completed == &call_id => Some((result.clone(), *duration_ms)),
            _ => None,
        })
        .ok_or(TestError::Missing(
            "der Handoff-Aufruf meldet ToolCallCompleted",
        ))?;
    assert!(
        result.is_success(),
        "das echte Ergebnis, kein Fehler: {result:?}"
    );
    assert_eq!(tool_duration, child_duration);

    let persisted = session
        .history()
        .items()
        .iter()
        .find_map(|item| match item {
            TurnItem::ToolResult(result) if result.call_id == call_id => Some(result.clone()),
            _ => None,
        })
        .ok_or(TestError::Missing("persistierter ToolResult des Handoffs"))?;
    assert_eq!(persisted.duration_ms, child_duration);
    assert!(persisted.result.is_success());
    Ok(())
}

// Keep the unused import lint quiet — ToolsError is used in the
// ToolExecutorFuture return type via the type alias.
#[allow(dead_code)]
fn _assert_tools_error_used(_: ToolsError) {}

// ------------------------------------------------------------------
// TurnControl wiring (Moduldoku „Fünfter Nachtrag"): ein gerissenes
// Budget endet als `Cancelled { reason: Budget }`, ein Turn innerhalb
// der Grenzen bleibt `Completed`.
// ------------------------------------------------------------------

#[tokio::test]
async fn turn_exceeding_its_model_round_budget_is_cancelled_with_budget_reason() -> TestResult {
    let provider = StubToolProvider::with_names(&[]);
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
    let store = crate::state_store::InMemoryStateStore::new();
    // `max_model_rounds: 0` reißt bereits am allerersten Prüfpunkt, vor
    // jedem Modellaufruf — der Provider darf deshalb nie befragt werden.
    let control = TurnControl::new().with_limits(TurnLimits {
        max_model_rounds: 0,
        ..TurnLimits::unlimited()
    });
    let input = TurnInput::user("hello").with_control(control);

    let outcome = run_turn(
        &mut session,
        &crate::model::EchoModelProvider::new("must never be called"),
        &store,
        input,
    )
    .await
    .map_err(ctx("a tripped budget ends the turn cleanly, not as an Err"))?;

    assert!(
        matches!(
            outcome,
            TurnOutcome::Cancelled {
                reason: CancelReason::Budget
            }
        ),
        "expected Cancelled{{reason: Budget}}, got {outcome:?}"
    );
    assert!(
        matches!(session.state(), crate::session::SessionState::Idle),
        "a cancelled turn returns the session to Idle, exactly like Completed"
    );
    Ok(())
}

#[tokio::test]
async fn turn_within_its_limits_still_completes() -> TestResult {
    let provider = StubToolProvider::with_names(&[]);
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
    let store = crate::state_store::InMemoryStateStore::new();
    let control = TurnControl::new().with_limits(TurnLimits {
        max_model_rounds: 4,
        max_tool_calls: 4,
        max_output_tokens_total: 1_000,
        wall_time: Duration::from_secs(30),
        tool_result_max_bytes: 4_096,
    });
    // Ein Klon, um nach dem Turn zu prüfen, dass der Steuerblock die
    // Modellrunde tatsächlich mitgezählt hat (Moduldoku „Fünfter
    // Nachtrag") — `input` verbraucht das Original.
    let control_check = control.clone();
    let input = TurnInput::user("hello").with_control(control);

    let outcome = run_turn(
        &mut session,
        &crate::model::EchoModelProvider::new("hi there"),
        &store,
        input,
    )
    .await
    .map_err(ctx("a turn within its budget completes normally"))?;

    assert!(
        matches!(outcome, TurnOutcome::Completed),
        "expected Completed, got {outcome:?}"
    );
    assert!(
        matches!(session.state(), crate::session::SessionState::Idle),
        "a completed turn returns the session to Idle"
    );
    assert_eq!(
        control_check.model_rounds(),
        1,
        "drive_turn must record the one model round it actually ran"
    );
    Ok(())
}

// ------------------------------------------------------------------
// Cancel mid-flight (Moduldoku „Sechster Nachtrag"): der Modellaufruf
// selbst und eine laufende Werkzeugausführung racen jetzt gegen
// `control.cancel_token()` statt nur an den Prüfpunkten davor/danach.
// ------------------------------------------------------------------

/// Modell, dessen `respond()` erst nach `delay` antwortet — lang genug,
/// dass ein Test ohne funktionierende Racing-Verdrahtung an seinem
/// `tokio::time::timeout` scheitert statt fälschlich grün zu werden.
/// `started` feuert, sobald `respond()` betreten wurde, damit der Test
/// genau dann abbrechen kann, wenn der Aufruf wirklich in Flight ist.
struct DelayedModelProvider {
    delay: Duration,
    started: Arc<tokio::sync::Notify>,
}

impl ModelProvider for DelayedModelProvider {
    fn respond<'a>(&'a self, _request: ModelRequest) -> crate::model::ModelFuture<'a> {
        self.started.notify_one();
        let delay = self.delay;
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            Ok(crate::model::ModelResponse::text(
                "too late — cancellation should have won",
            ))
        })
    }
}

/// Ausführer, der erst zurückkehrt, wenn sein
/// [`ToolExecutionContext::cancel`]-Token feuert — dann mit
/// `ToolsError::Cancelled`, genau wie ein wohlerzogener, cancel-bewusster
/// Ausführer (W3/C-CANCEL) reagieren muss. Fehlt die Cancel-Verdrahtung
/// (`ctx.cancel()` liefert `None`), schläft er stattdessen lange genug,
/// dass der Test am `tokio::time::timeout` scheitert statt zu hängen.
/// `started` feuert bei jedem Aufruf, damit ein Test exakt weiß, wann
/// mindestens ein Aufruf tatsächlich läuft.
struct CancelAwareExecutor {
    executions: Arc<AtomicUsize>,
    started: Arc<tokio::sync::Notify>,
}

impl ToolExecutor for CancelAwareExecutor {
    fn execute<'a>(
        &'a self,
        ctx: &'a ToolExecutionContext,
        _call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        Box::pin(async move {
            match ctx.cancel() {
                Some(token) => {
                    token.cancelled().await;
                    Err(ToolsError::Cancelled)
                }
                None => {
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    Ok(ToolOutput::Text {
                        content: "unreachable without cancel wiring".to_owned(),
                    })
                }
            }
        })
    }
}

/// Einzelnes, **nicht** `parallel_safe` markiertes Werkzeug `"block"` —
/// erzwingt den sequenziellen Dispatch-Pfad.
struct CancelAwareToolProvider {
    executions: Arc<AtomicUsize>,
    started: Arc<tokio::sync::Notify>,
}

impl ToolProvider for CancelAwareToolProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![ToolSpec::Function(FunctionToolSpec {
            name: ToolName::new("block"),
            description: "blocks until its cancel token fires".to_owned(),
            parameters: JsonSchema::default(),
            strict: false,
        })]
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        (name.as_str() == "block").then(|| {
            Arc::new(CancelAwareExecutor {
                executions: Arc::clone(&self.executions),
                started: Arc::clone(&self.started),
            }) as Arc<dyn ToolExecutor>
        })
    }
}

/// Wie [`CancelAwareToolProvider`], aber `"block"` ist `parallel_safe` —
/// erzwingt den `JoinSet`-Pfad ([`try_execute_parallel_calls`]).
struct CancelAwareParallelProvider {
    executions: Arc<AtomicUsize>,
    started: Arc<tokio::sync::Notify>,
}

impl ToolProvider for CancelAwareParallelProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![ToolSpec::Function(FunctionToolSpec {
            name: ToolName::new("block"),
            description: "blocks until its cancel token fires".to_owned(),
            parameters: JsonSchema::default(),
            strict: false,
        })]
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        (name.as_str() == "block").then(|| {
            Arc::new(CancelAwareExecutor {
                executions: Arc::clone(&self.executions),
                started: Arc::clone(&self.started),
            }) as Arc<dyn ToolExecutor>
        })
    }

    fn parallel_safe(&self, name: &ToolName) -> bool {
        name.as_str() == "block"
    }
}

#[tokio::test]
async fn model_call_cancelled_mid_flight_ends_the_turn_quickly() -> TestResult {
    let provider = StubToolProvider::with_names(&[]);
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
    let store = crate::state_store::InMemoryStateStore::new();
    let started = Arc::new(tokio::sync::Notify::new());
    let model = DelayedModelProvider {
        delay: Duration::from_secs(30),
        started: Arc::clone(&started),
    };
    let control = TurnControl::new();
    let cancel_token = control.cancel_token().clone();
    let input = TurnInput::user("hello").with_control(control);

    let run = run_turn(&mut session, &model, &store, input);
    let canceller = async {
        started.notified().await;
        cancel_token.cancel(CancelReason::User);
    };

    let (outcome, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(run, canceller)
    })
    .await
    .map_err(ctx(
        "cancellation must win long before the provider's 30s delay elapses — \
             the model call is not actually racing the cancel token",
    ))?;
    let outcome = outcome.map_err(ctx(
        "a cancelled model call ends the turn cleanly, not as Err",
    ))?;

    assert!(
        matches!(
            outcome,
            TurnOutcome::Cancelled {
                reason: CancelReason::User
            }
        ),
        "expected Cancelled{{reason: User}}, got {outcome:?}"
    );
    assert!(
        matches!(session.state(), crate::session::SessionState::Idle),
        "a cancelled turn returns the session to Idle, exactly like Completed"
    );
    Ok(())
}

#[tokio::test]
async fn tool_call_cancelled_mid_flight_aborts_pending_calls_sequential_path() -> TestResult {
    let first = ToolCallId::new();
    let second = ToolCallId::new();
    let executions = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(CancelAwareToolProvider {
        executions: Arc::clone(&executions),
        started: Arc::clone(&started),
    });
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full))
        .with_spawn_context(test_spawn_context()?);
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![response_with(vec![
        call(&first, "block"),
        call(&second, "block"),
    ])]);
    let control = TurnControl::new();
    let cancel_token = control.cancel_token().clone();
    let input = TurnInput::user("go").with_control(control);

    let run = run_turn(&mut session, &model, &store, input);
    let canceller = async {
        started.notified().await;
        cancel_token.cancel(CancelReason::User);
    };

    let (outcome, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(run, canceller)
    })
    .await
    .map_err(ctx(
        "cancellation must resolve the blocked sequential call quickly",
    ))?;
    let outcome = outcome.map_err(ctx(
        "a cancelled sequential tool call ends the turn cleanly, not as Err",
    ))?;

    assert!(
        matches!(
            outcome,
            TurnOutcome::Cancelled {
                reason: CancelReason::User
            }
        ),
        "expected Cancelled{{reason: User}}, got {outcome:?}"
    );
    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "the second call must never start — it was still pending in `tool_call_iter` \
             when cancellation hit, exactly like a `tool_checkpoint()` pre-emptive hit"
    );
    let items = session.history().items();
    assert_eq!(
        items
            .iter()
            .filter(|item| matches!(item, TurnItem::ToolCall(_)))
            .count(),
        2,
        "both calls must have a tool_call entry (the second one synthesized by \
             cancel_turn_with_pending_calls)"
    );
    assert_eq!(
        items
            .iter()
            .filter(|item| matches!(item, TurnItem::ToolResult(_)))
            .count(),
        2,
        "every tool_call must be paired with a tool_result, or the history is not \
             provider-valid for the next model call"
    );
    Ok(())
}

#[tokio::test]
async fn tool_call_cancelled_mid_flight_aborts_pending_calls_parallel_path() -> TestResult {
    let first = ToolCallId::new();
    let second = ToolCallId::new();
    let executions = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let handler = Arc::new(CountingApproval::allow_everything());
    let provider = Arc::new(CancelAwareParallelProvider {
        executions: Arc::clone(&executions),
        started: Arc::clone(&started),
    });
    let mut session = guarded_session(provider, Arc::clone(&handler))?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![response_with(vec![
        call(&first, "block"),
        call(&second, "block"),
    ])]);
    let control = TurnControl::new();
    let cancel_token = control.cancel_token().clone();
    let input = TurnInput::user("go").with_control(control);

    let run = run_turn(&mut session, &model, &store, input);
    let canceller = async {
        started.notified().await;
        cancel_token.cancel(CancelReason::User);
    };

    let (outcome, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(run, canceller)
    })
    .await
    .map_err(ctx(
        "cancellation must resolve the blocked parallel calls quickly",
    ))?;
    let outcome = outcome.map_err(ctx(
        "a cancelled parallel tool call ends the turn cleanly, not as Err",
    ))?;

    assert!(
        matches!(
            outcome,
            TurnOutcome::Cancelled {
                reason: CancelReason::User
            }
        ),
        "expected Cancelled{{reason: User}}, got {outcome:?}"
    );
    let items = session.history().items();
    assert_eq!(
        items
            .iter()
            .filter(|item| matches!(item, TurnItem::ToolCall(_)))
            .count(),
        2,
        "the JoinSet path pushes both tool_call entries up front, before spawning"
    );
    let results: Vec<ToolCallResult> = items
        .iter()
        .filter_map(|item| match item {
            TurnItem::ToolResult(result) => Some(result.result.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        results.len(),
        2,
        "every tool_call must be paired with a tool_result, or the history is not \
             provider-valid for the next model call"
    );
    // Angepasst für Fix C (Moduldoku „Siebter Nachtrag"): vor Fix C konnte
    // dieser Test einen deterministischen 1-und-1-Split annehmen — ein
    // Call lieferte über `CancelAwareExecutor`s eigenes
    // `Err(ToolsError::Cancelled)` aus, der andere war beim `TurnGuard`-
    // artigen Abort noch unzugestellt. Fix C lässt die Reaper-Schleife
    // selbst gegen `control.cancel_token()` racen und `joins.abort_all()`
    // rufen, sobald sie gewinnt — das kann jetzt auch einen an sich
    // Cancel-bewussten Job abbrechen, bevor er seine eigene
    // `Err(ToolsError::Cancelled)`-Antwort zurückgeben konnte (ein
    // `abort()`ter Task wird nie zu Ende poll't). Welcher der beiden Wege
    // im Einzelfall gewinnt, ist jetzt eine echte, gewollte
    // Scheduler-Race — der Test prüft deshalb nur noch die Invariante,
    // die für beide Fälle gilt: jedes Ergebnis ist ein
    // Cancel-Fehlerergebnis, keines ein generischer Tool-Fehler.
    let all_cancelled = results.iter().all(|result| {
        matches!(
            result,
            ToolCallResult::Error { message }
                if message == "tool execution was cancelled"
                    || message.contains("cancelled before delivery")
        )
    });
    assert!(
        all_cancelled,
        "every result must be cancellation-shaped — either the executor's own \
             ToolsError::Cancelled message or the synthetic 'cancelled before delivery' \
             fill-in — got {results:?}"
    );
    Ok(())
}

// ------------------------------------------------------------------
// Fix A/B/C (Moduldoku „Siebter Nachtrag"): der Steuerblock übersteht
// jetzt eine Handoff-/Rückfrage-Pause, und zwei bisher ungeracete
// Ausführungspfade (Rückfrage-Fortsetzung sequenziell; paralleler
// Join-Loop) racen jetzt auch gegen `control.cancel_token()`.
// ------------------------------------------------------------------

#[tokio::test]
async fn cancelling_the_original_control_after_a_handoff_pause_cancels_the_resumed_turn()
-> TestResult {
    // Delegation never pauses any more; the pause below is a persisted /
    // legacy `AwaitingChild` session that `resume_after_child` still resumes.
    let child = SessionId::new();
    let (tx, _rx) = mpsc::unbounded_channel();
    let registry = ExtensionRegistryBuilder::default()
        .spawner(Arc::new(FixedChildSpawner {
            child: child.clone(),
        }))
        .build();
    let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx)
        .with_spawn_context(test_spawn_context()?);
    let store = crate::state_store::InMemoryStateStore::new();
    let control = TurnControl::new();
    let cancel_token = control.cancel_token().clone();
    session.try_start_turn().map_err(ctx("the turn starts"))?;
    session.set_active_turn_control(control);
    let call_id_returned = ToolCallId::new();
    session
        .begin_handoff(child.clone(), call_id_returned.clone(), "worker".to_owned())
        .map_err(ctx("the handoff pauses the turn"))?;
    let child_returned = child;

    // Ctrl+C-Äquivalent: der URSPRÜNGLICHE Token wird erst NACH der Pause
    // abgebrochen. Vor Fix A verschluckte `resume_after_child_with_approvals`s
    // frischer, unbegrenzter `TurnControl::new()` genau diesen Fall — dieser
    // Test muss vor Fix A rot sein (Ergebnis `Completed`), danach grün
    // (Ergebnis `Cancelled`).
    cancel_token.cancel(CancelReason::User);

    let outcome = resume_after_child(
        &mut session,
        &crate::model::EchoModelProvider::new("must not be reached"),
        &store,
        child_returned,
        call_id_returned,
        ToolCallResult::success(serde_json::json!({"child": "done"})),
    )
    .await
    .map_err(ctx(
        "a cancelled resume still ends the turn cleanly, not as Err",
    ))?;

    assert!(
        matches!(
            outcome,
            TurnOutcome::Cancelled {
                reason: CancelReason::User
            }
        ),
        "AgentSession::active_turn_control must carry the original control across the \
             handoff pause so the resumed turn observes its cancellation — got {outcome:?}"
    );
    assert!(
        matches!(session.state(), crate::session::SessionState::Idle),
        "a cancelled turn returns the session to Idle, exactly like Completed"
    );
    Ok(())
}

/// Ausführer, der `ctx.cancel()` NIE selbst abfragt — schläft `delay`
/// lang und liefert danach ein gewöhnliches Ergebnis. Anders als
/// [`CancelAwareExecutor`] oben: dieser Fixture prüft, dass Fix B/C auch
/// einen unkooperativen Ausführer unterbrechen, nicht nur einen, der den
/// Token selbst abfragt.
struct DelayedNonCancelAwareExecutor {
    delay: Duration,
    started: Arc<tokio::sync::Notify>,
}

impl ToolExecutor for DelayedNonCancelAwareExecutor {
    fn execute<'a>(
        &'a self,
        _ctx: &'a ToolExecutionContext,
        _call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        self.started.notify_one();
        let delay = self.delay;
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            Ok(ToolOutput::Text {
                content: "too late — the outer cancel race should have won".to_owned(),
            })
        })
    }
}

/// Einzelnes, **nicht** `parallel_safe` markiertes Werkzeug `"slow"` —
/// erzwingt den sequenziellen Dispatch-Pfad.
struct SlowToolProvider {
    started: Arc<tokio::sync::Notify>,
}

impl ToolProvider for SlowToolProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![ToolSpec::Function(FunctionToolSpec {
            name: ToolName::new("slow"),
            description: "ignores cancellation and sleeps".to_owned(),
            parameters: JsonSchema::default(),
            strict: false,
        })]
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        (name.as_str() == "slow").then(|| {
            Arc::new(DelayedNonCancelAwareExecutor {
                delay: Duration::from_secs(30),
                started: Arc::clone(&self.started),
            }) as Arc<dyn ToolExecutor>
        })
    }
}

/// Wie [`SlowToolProvider`], aber `"slow"` ist `parallel_safe` — erzwingt
/// den `JoinSet`-Pfad ([`try_execute_parallel_calls`]).
struct SlowParallelToolProvider {
    started: Arc<tokio::sync::Notify>,
}

impl ToolProvider for SlowParallelToolProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![ToolSpec::Function(FunctionToolSpec {
            name: ToolName::new("slow"),
            description: "ignores cancellation and sleeps".to_owned(),
            parameters: JsonSchema::default(),
            strict: false,
        })]
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        (name.as_str() == "slow").then(|| {
            Arc::new(DelayedNonCancelAwareExecutor {
                delay: Duration::from_secs(30),
                started: Arc::clone(&self.started),
            }) as Arc<dyn ToolExecutor>
        })
    }

    fn parallel_safe(&self, name: &ToolName) -> bool {
        name.as_str() == "slow"
    }
}

#[tokio::test]
async fn tool_call_ignoring_cancel_itself_is_still_interrupted_sequential_path() -> TestResult {
    let call_id = ToolCallId::new();
    let started = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(SlowToolProvider {
        started: Arc::clone(&started),
    });
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full))
        .with_spawn_context(test_spawn_context()?);
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![response_with(vec![call(&call_id, "slow")])]);
    let control = TurnControl::new();
    let cancel_token = control.cancel_token().clone();
    let input = TurnInput::user("go").with_control(control);

    let run = run_turn(&mut session, &model, &store, input);
    let canceller = async {
        started.notified().await;
        cancel_token.cancel(CancelReason::User);
    };

    let (outcome, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(run, canceller)
    })
    .await
    .map_err(ctx(
        "Fix B must race the executor's own await against the cancel token — an \
             executor that never checks cancellation itself must not be able to block the \
             turn for its full 30s delay",
    ))?;
    let outcome = outcome.map_err(ctx(
        "a cancelled tool call ends the turn cleanly, not as Err",
    ))?;

    assert!(
        matches!(
            outcome,
            TurnOutcome::Cancelled {
                reason: CancelReason::User
            }
        ),
        "expected Cancelled{{reason: User}}, got {outcome:?}"
    );
    Ok(())
}

#[tokio::test]
async fn tool_call_ignoring_cancel_itself_is_still_interrupted_ask_user_resume_path()
-> TestResult {
    let call_id = ToolCallId::new();
    let started = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(SlowToolProvider {
        started: Arc::clone(&started),
    });
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full))
        .with_spawn_context(test_spawn_context()?);
    session.try_start_turn().map_err(ctx("test turn starts"))?;
    let actor = ApprovalActor::Operator {
        id: "test-operator".to_owned(),
    };
    let pending_call = ToolCall {
        id: call_id.clone(),
        name: ToolName::new("slow"),
        arguments: serde_json::json!({}),
    };
    session
        .begin_approval(
            pending_call,
            harw_types::ItemId::new(),
            actor.clone(),
            jiff::Timestamp::now(),
        )
        .map_err(ctx("pause for approval"))?;
    // Fix B testet den Rückfrage-Fortsetzungspfad unabhängig von Fix A:
    // der Steuerblock wird hier direkt über den neuen Setter hinterlegt,
    // statt über einen vollen `run_turn`-Durchlauf.
    let control = TurnControl::new();
    let cancel_token = control.cancel_token().clone();
    session.set_active_turn_control(control);
    let store = crate::state_store::InMemoryStateStore::new();
    let model = crate::model::EchoModelProvider::new("must not be reached");

    let run = resume_after_approval(
        &mut session,
        &model,
        &store,
        actor,
        ApprovalResolution::Approve,
    );
    let canceller = async {
        started.notified().await;
        cancel_token.cancel(CancelReason::User);
    };

    let (outcome, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(run, canceller)
    })
    .await
    .map_err(ctx(
        "Fix B must race the resume-after-approval executor call against the cancel \
             token too — this path had no was_cancelled handling at all before this node",
    ))?;
    let outcome = outcome.map_err(ctx(
        "a cancelled resumed tool call ends the turn cleanly, not as Err",
    ))?;

    assert!(
        matches!(
            outcome,
            TurnOutcome::Cancelled {
                reason: CancelReason::User
            }
        ),
        "expected Cancelled{{reason: User}}, got {outcome:?}"
    );
    Ok(())
}

// Fix A (Moduldoku „Achter Nachtrag"): a transport error on the model
// call that `drive_turn` makes right after a resumed approval's tool
// executes must not leave the session stuck in `Running` forever.
// Before the fix, every error exit of `resume_after_approval_with_store`
// after `resolve_approval` succeeded (here: the trailing `drive_turn`
// call) returned directly, bypassing `transition_after_turn_failure` —
// the next `try_start_turn` then failed with "session not idle:
// Running" even though the failure itself was retryable.
#[tokio::test]
async fn resume_after_approval_retryable_provider_failure_leaves_session_idle_not_running()
-> TestResult {
    let call_id = ToolCallId::new();
    let provider = StubToolProvider::with_names(&["fs.read"]);
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full))
        .with_spawn_context(test_spawn_context()?);
    session.try_start_turn().map_err(ctx("test turn starts"))?;
    let actor = ApprovalActor::Operator {
        id: "test-operator".to_owned(),
    };
    let pending_call = ToolCall {
        id: call_id.clone(),
        name: ToolName::new("fs.read"),
        arguments: serde_json::json!({}),
    };
    session
        .begin_approval(
            pending_call,
            harw_types::ItemId::new(),
            actor.clone(),
            jiff::Timestamp::now(),
        )
        .map_err(ctx("pause for approval"))?;
    session.set_active_turn_control(TurnControl::new());
    let store = crate::state_store::InMemoryStateStore::new();

    let Err(error) = resume_after_approval(
        &mut session,
        &TransientModel,
        &store,
        actor,
        ApprovalResolution::Approve,
    )
    .await
    else {
        return Err(TestError::Unexpected(
            "the transport error surfaces to the caller".to_owned(),
        ));
    };

    assert!(matches!(
        error,
        CoreError::Model(crate::model::ModelError::Transient { .. })
    ));
    assert!(
        matches!(session.state(), crate::session::SessionState::Idle),
        "a retryable provider failure on the post-approval drive_turn call must not leave \
             the session stuck in Running, got {:?}",
        session.state()
    );

    let outcome = run_turn(
        &mut session,
        &crate::model::EchoModelProvider::new("recovered"),
        &store,
        TurnInput::user("try again"),
    )
    .await
    .map_err(ctx(
        "the same session accepts a new turn after the retryable failure",
    ))?;
    assert!(matches!(outcome, TurnOutcome::Completed));
    Ok(())
}

// Companion to the retryable case above: a non-retryable provider
// failure on the post-approval `drive_turn` call must still move the
// session to the terminal `Failed` state via `transition_after_turn_failure`
// — never leave it hanging in `Running`.
#[tokio::test]
async fn resume_after_approval_non_retryable_provider_failure_terminalizes_session()
-> TestResult {
    let call_id = ToolCallId::new();
    let provider = StubToolProvider::with_names(&["fs.read"]);
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full))
        .with_spawn_context(test_spawn_context()?);
    session.try_start_turn().map_err(ctx("test turn starts"))?;
    let actor = ApprovalActor::Operator {
        id: "test-operator".to_owned(),
    };
    let pending_call = ToolCall {
        id: call_id.clone(),
        name: ToolName::new("fs.read"),
        arguments: serde_json::json!({}),
    };
    session
        .begin_approval(
            pending_call,
            harw_types::ItemId::new(),
            actor.clone(),
            jiff::Timestamp::now(),
        )
        .map_err(ctx("pause for approval"))?;
    session.set_active_turn_control(TurnControl::new());
    let store = crate::state_store::InMemoryStateStore::new();

    let Err(error) = resume_after_approval(
        &mut session,
        &RefusalModel,
        &store,
        actor,
        ApprovalResolution::Approve,
    )
    .await
    else {
        return Err(TestError::Unexpected(
            "the refusal surfaces to the caller".to_owned(),
        ));
    };

    assert!(matches!(
        error,
        CoreError::Model(crate::model::ModelError::Refusal { .. })
    ));
    assert!(
        matches!(session.state(), crate::session::SessionState::Failed(_)),
        "a non-retryable provider failure on the post-approval drive_turn call must \
             terminalize the session instead of leaving it Running, got {:?}",
        session.state()
    );
    Ok(())
}

#[tokio::test]
async fn parallel_dispatch_aborts_still_running_non_cancel_aware_calls_on_cancel() -> TestResult
{
    let first = ToolCallId::new();
    let second = ToolCallId::new();
    let started = Arc::new(tokio::sync::Notify::new());
    let handler = Arc::new(CountingApproval::allow_everything());
    let provider = Arc::new(SlowParallelToolProvider {
        started: Arc::clone(&started),
    });
    let mut session = guarded_session(provider, Arc::clone(&handler))?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![response_with(vec![
        call(&first, "slow"),
        call(&second, "slow"),
    ])]);
    let control = TurnControl::new();
    let cancel_token = control.cancel_token().clone();
    let input = TurnInput::user("go").with_control(control);

    let run = run_turn(&mut session, &model, &store, input);
    let canceller = async {
        started.notified().await;
        cancel_token.cancel(CancelReason::User);
    };

    let (outcome, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(run, canceller)
    })
    .await
    .map_err(ctx(
        "Fix C must race the JoinSet reaper loop against the cancel token and \
             abort_all() still-running jobs — executors that never check cancellation \
             themselves must not be able to block the turn for their full 30s delay",
    ))?;
    let outcome = outcome.map_err(ctx(
        "a cancelled parallel dispatch ends the turn cleanly, not as Err",
    ))?;

    assert!(
        matches!(
            outcome,
            TurnOutcome::Cancelled {
                reason: CancelReason::User
            }
        ),
        "expected Cancelled{{reason: User}}, got {outcome:?}"
    );
    let items = session.history().items();
    assert_eq!(
        items
            .iter()
            .filter(|item| matches!(item, TurnItem::ToolCall(_)))
            .count(),
        2,
        "the JoinSet path pushes both tool_call entries up front, before spawning"
    );
    let results: Vec<ToolCallResult> = items
        .iter()
        .filter_map(|item| match item {
            TurnItem::ToolResult(result) => Some(result.result.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        results.len(),
        2,
        "every tool_call must be paired with a tool_result — including the calls that \
             were still in flight when abort_all() fired, which the synthetic fill-in \
             (Fix C) must cover"
    );
    assert!(
        results.iter().all(|result| matches!(
            result,
            ToolCallResult::Error { message } if message.contains("cancelled before delivery")
        )),
        "neither call checks cancellation itself, so abort_all() must be the only way \
             either of them ever resolves, and both must get the synthetic 'cancelled \
             before delivery' fill-in — got {results:?}"
    );
    Ok(())
}

// ------------------------------------------------------------------
// maybe_compact tests
// ------------------------------------------------------------------

/// Sammelt den letzten `CompactionOutcome`, den `maybe_compact` über
/// `compact_for_budget` erzeugt hat — Test-Double für
/// [`crate::compaction::CompactionObserver`].
struct RecordingCompactionObserver {
    last: Mutex<Option<crate::compaction::CompactionOutcome>>,
}

impl RecordingCompactionObserver {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            last: Mutex::new(None),
        })
    }
}

impl crate::compaction::CompactionObserver for RecordingCompactionObserver {
    fn on_compacted(
        &self,
        _session_id: &SessionId,
        outcome: &crate::compaction::CompactionOutcome,
    ) {
        *self
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(outcome.clone());
    }
}

#[tokio::test]
async fn test_maybe_compact_noop_without_policy() {
    // `AgentSession::auto_compact` ist standardmäßig `None` — bestehende
    // Sessions, die nie opt-in, dürfen von `maybe_compact` nicht berührt
    // werden.
    let provider = StubToolProvider::with_names(&[]);
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
    session.history_mut().push_user_text("hallo");
    let observer = RecordingCompactionObserver::new();
    session = session.with_compaction_observer(Some(observer.clone()));
    let store = crate::state_store::InMemoryStateStore::new();
    let usage = harw_types::TokenUsage {
        cache_separate: false,
        input_tokens: u64::MAX,
        output_tokens: 0,
        reasoning_tokens: None,
        cached_tokens: None,
        cache_write_tokens: None,
    };

    maybe_compact(
        &mut session,
        &crate::model::EchoModelProvider::default(),
        &store,
        &usage,
        0,
        true,
        &mut None,
    )
    .await;

    assert!(
        observer
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none(),
        "no policy set ⇒ compact_session must never run"
    );
}

#[tokio::test]
async fn test_maybe_compact_triggers_and_shrinks_history_with_tiny_policy() -> TestResult {
    // Winziges Kontextfenster ⇒ winzige Schwellen (70/30 Tokens), leicht
    // von einer kleinen künstlichen Nutzung überschritten.
    let policy = crate::auto_compact::AutoCompactPolicy::for_context_window(100);
    let provider = StubToolProvider::with_names(&[]);
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full))
        .with_auto_compact(Some(policy));
    let observer = RecordingCompactionObserver::new();
    session = session.with_compaction_observer(Some(observer.clone()));
    // Genug wiederholte, deterministisch deduplizierbare Items, damit
    // `deterministic_pass` die Historie sichtbar schrumpft.
    for i in 0..20 {
        session.history_mut().push_user_text("frage");
        session.history_mut().push_assistant_text(
            format!("wiederholte, ausführliche Antwort Nummer {i} mit etwas mehr Text"),
            None,
        );
    }
    let bytes_before: usize = session
        .history()
        .items()
        .iter()
        .map(|item| serde_json::to_vec(item).map(|v| v.len()).unwrap_or(0))
        .sum();
    let store = crate::state_store::InMemoryStateStore::new();
    // Über der Task-Ende-Schwelle (30), unter der Budget-Schwelle (70)
    // ⇒ `CompactDecision::TaskCompleted`.
    let usage = harw_types::TokenUsage {
        cache_separate: false,
        input_tokens: 40,
        output_tokens: 0,
        reasoning_tokens: None,
        cached_tokens: None,
        cache_write_tokens: None,
    };

    maybe_compact(
        &mut session,
        &crate::model::EchoModelProvider::default(),
        &store,
        &usage,
        0,
        true,
        &mut None,
    )
    .await;

    let outcome = observer
        .last
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .ok_or(TestError::Missing(
            "policy set and thresholds exceeded ⇒ compact_session must run",
        ))?;
    assert_eq!(
        outcome.reason,
        Some(crate::auto_compact::CompactDecision::TaskCompleted)
    );
    let bytes_after: usize = session
        .history()
        .items()
        .iter()
        .map(|item| serde_json::to_vec(item).map(|v| v.len()).unwrap_or(0))
        .sum();
    assert!(
        bytes_after <= bytes_before,
        "compaction must not grow the history (before={bytes_before}, after={bytes_after})"
    );
    Ok(())
}

// ------------------------------------------------------------------
// maybe_hard_compact_at_turn_start tests (Addendum D)
// ------------------------------------------------------------------

#[tokio::test]
async fn test_turn_start_hard_compact_noop_without_target() {
    // Policy ohne `with_turn_start_target` ⇒ kein Turn-Start-Compact,
    // unabhängig von der Verlaufsgröße (Worker-Kinder betrifft das).
    let policy = crate::auto_compact::AutoCompactPolicy::for_context_window(200_000);
    let provider = StubToolProvider::with_names(&[]);
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full))
        .with_auto_compact(Some(policy));
    let observer = RecordingCompactionObserver::new();
    session = session.with_compaction_observer(Some(observer.clone()));
    for i in 0..20 {
        session.history_mut().push_user_text("frage");
        session
            .history_mut()
            .push_assistant_text(format!("antwort {i}"), None);
    }
    let store = crate::state_store::InMemoryStateStore::new();

    maybe_hard_compact_at_turn_start(
        &mut session,
        &crate::model::EchoModelProvider::default(),
        &store,
    )
    .await;

    assert!(
        observer
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none(),
        "no turn_start_target ⇒ compact_session must never run"
    );
}

#[tokio::test]
async fn test_turn_start_hard_compact_triggers_above_target() -> TestResult {
    // Winziges Ziel ⇒ die künstlich aufgeblähte Historie liegt sicher
    // darüber und löst die harte Verdichtung aus.
    let policy = crate::auto_compact::AutoCompactPolicy::for_context_window(200_000)
        .with_turn_start_target(Some(10));
    let provider = StubToolProvider::with_names(&[]);
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full))
        .with_auto_compact(Some(policy));
    let observer = RecordingCompactionObserver::new();
    session = session.with_compaction_observer(Some(observer.clone()));
    for i in 0..20 {
        session.history_mut().push_user_text("frage");
        session.history_mut().push_assistant_text(
            format!("wiederholte, ausführliche Antwort Nummer {i} mit etwas mehr Text"),
            None,
        );
    }
    let store = crate::state_store::InMemoryStateStore::new();

    maybe_hard_compact_at_turn_start(
        &mut session,
        &crate::model::EchoModelProvider::default(),
        &store,
    )
    .await;

    let outcome = observer
        .last
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .ok_or(TestError::Missing(
            "history far above the tiny target ⇒ compact_session must run",
        ))?;
    assert_eq!(
        outcome.reason,
        Some(crate::auto_compact::CompactDecision::TurnStart)
    );
    Ok(())
}

// ------------------------------------------------------------------
// last_round_usage → maybe_compact wiring (Teil 1)
// ------------------------------------------------------------------

#[tokio::test]
async fn test_last_round_usage_from_response_drives_auto_compaction() -> TestResult {
    // `last_round_usage` wird in `drive_turn` bei jeder Modellrunde aus
    // `response.usage` gesetzt und an `maybe_compact` weitergereicht
    // (Turn-Ende-Aufruf). Mit `TokenUsage::default()` (der ungenutzte
    // Anfangswert) könnte `decide()` bei einer 70-Token-Schwelle nie
    // `BudgetExceeded` liefern — nur die tatsächliche Antwort-Nutzung
    // (80 Tokens) kann das. Der Test beweist also konkret, dass der
    // reale Wert ankommt, nicht der Default.
    let policy = crate::auto_compact::AutoCompactPolicy::for_context_window(100);
    let provider = StubToolProvider::with_names(&[]);
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full))
        .with_auto_compact(Some(policy));
    let observer = RecordingCompactionObserver::new();
    session = session.with_compaction_observer(Some(observer.clone()));
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![crate::model::ModelResponse {
        usage: harw_types::TokenUsage {
            cache_separate: false,
            input_tokens: 80,
            output_tokens: 5,
            reasoning_tokens: None,
            cached_tokens: None,
            cache_write_tokens: None,
        },
        ..crate::model::ModelResponse::text("fertig")
    }]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("hi"))
        .await
        .map_err(ctx("der Turn läuft durch"))?;
    assert!(matches!(outcome, TurnOutcome::Completed));

    let compaction_outcome = observer
        .last
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .ok_or(TestError::Missing(
            "die tatsächliche Antwort-Nutzung (80 > 70er-Schwelle) muss den \
                 Turn-Ende-Compact auslösen — geschieht das nicht, wurde \
                 `last_round_usage` nicht an `maybe_compact` durchgereicht",
        ))?;
    assert_eq!(
        compaction_outcome.reason,
        Some(crate::auto_compact::CompactDecision::BudgetExceeded)
    );
    Ok(())
}

// ------------------------------------------------------------------
// Reasoning-Persistenz (Welle 3 — 3e, seit Runde 2 für jede Session)
// ------------------------------------------------------------------

/// UIA-Root-Fixture: kein Parent, `organizational_role` =
/// `AgentRoleId::UserInterface` — identisches Kriterium zu
/// `is_uia_root_session` in `harw-tui/src/session_controller.rs`.
fn uia_root_session(provider: Arc<dyn ToolProvider>) -> TestResult<AgentSession> {
    let (tx, _rx) = mpsc::unbounded_channel();
    let registry = ExtensionRegistryBuilder::default()
        .tool_provider(provider)
        .build();
    Ok(
        AgentSession::new(AgentRole::Assistant, None, registry, tx).with_spawn_context(
            SpawnContext {
                organizational_role: harw_agent_dsl::roles::AgentRoleId::UserInterface,
                ..test_spawn_context()?
            },
        ),
    )
}

fn anthropic_reasoning(blocks: Vec<serde_json::Value>) -> OpaqueReasoning {
    OpaqueReasoning {
        provider: "anthropic".to_owned(),
        model: "claude-test".to_owned(),
        blocks,
    }
}

#[tokio::test]
async fn test_thinking_block_emits_reasoning_item_for_uia_root_session() -> TestResult {
    let provider = StubToolProvider::with_names(&[]);
    let mut session = uia_root_session(provider)?;
    let (turn_tx, mut turn_rx) = mpsc::unbounded_channel();
    session = session.with_turn_event_sink(turn_tx);
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![crate::model::ModelResponse {
        reasoning: Some(anthropic_reasoning(vec![serde_json::json!({
            "type": "thinking",
            "thinking": "Der Nutzer fragt nach X, also prüfe ich zuerst Y.",
        })])),
        ..crate::model::ModelResponse::text("Antwort")
    }]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("frage"))
        .await
        .map_err(ctx("der Turn läuft durch"))?;
    assert!(matches!(outcome, TurnOutcome::Completed));

    // History: Reasoning-Item kommt vor der AssistantMessage.
    let items = session.history().items();
    let reasoning_pos = items
        .iter()
        .position(|item| matches!(item, TurnItem::Reasoning(_)))
        .ok_or(TestError::Missing(
            "ein Reasoning-Item muss in der History stehen",
        ))?;
    let assistant_pos = items
        .iter()
        .position(|item| matches!(item, TurnItem::AssistantMessage(_)))
        .ok_or(TestError::Missing(
            "die AssistantMessage muss in der History stehen",
        ))?;
    assert!(
        reasoning_pos < assistant_pos,
        "Reasoning muss vor der AssistantMessage stehen (Denken vor Antwort)"
    );
    match &items[reasoning_pos] {
        TurnItem::Reasoning(item) => {
            assert_eq!(
                item.summary_text,
                vec!["Der Nutzer fragt nach X, also prüfe ich zuerst Y.".to_owned()]
            );
            assert!(item.raw_content.is_empty());
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "erwartete TurnItem::Reasoning, war {other:?}"
            )));
        }
    }

    // Event: TurnEvent::ItemAdded mit TurnItem::Reasoning wurde emittiert.
    let events = drain_turn_events(&mut turn_rx);
    let reasoning_event = events.iter().find(|event| {
        matches!(
            event,
            TurnEvent::ItemAdded {
                item: TurnItem::Reasoning(_),
                ..
            }
        )
    });
    assert!(
        reasoning_event.is_some(),
        "ein TurnEvent::ItemAdded für das Reasoning-Item muss emittiert werden"
    );
    Ok(())
}

#[tokio::test]
async fn test_thinking_block_emits_reasoning_item_for_non_uia_session() -> TestResult {
    // Identische Antwort wie im UIA-Test, aber eine Session mit
    // Standard-Rolle ohne `SpawnContext` — seit Runde 2 wird Reasoning
    // für JEDE Session persistiert, nicht nur für die UIA-Root.
    let provider = StubToolProvider::with_names(&[]);
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![crate::model::ModelResponse {
        reasoning: Some(anthropic_reasoning(vec![serde_json::json!({
            "type": "thinking",
            "thinking": "Denkprozess einer Nicht-UIA-Session",
        })])),
        ..crate::model::ModelResponse::text("Antwort")
    }]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("frage"))
        .await
        .map_err(ctx("der Turn läuft durch"))?;
    assert!(matches!(outcome, TurnOutcome::Completed));

    assert!(
        session.history().items().iter().any(|item| matches!(
            item,
            TurnItem::Reasoning(reasoning)
                if reasoning.summary_text
                    == vec!["Denkprozess einer Nicht-UIA-Session".to_owned()]
        )),
        "auch eine Nicht-UIA-Session muss ihr Reasoning-Item bekommen"
    );
    Ok(())
}

/// Skriptiertes Modell, das jede Anfrage protokolliert: Werkzeuge,
/// History-Items und die provider-neutrale Nachrichtensicht, die echte
/// Provider an das Modell senden.
struct RecordingModel {
    responses: Mutex<std::collections::VecDeque<crate::model::ModelResponse>>,
    requests: Mutex<Vec<RecordedRequest>>,
}

struct RecordedRequest {
    tools: Vec<ToolSpec>,
    items: Vec<TurnItem>,
    messages: Vec<crate::history::ModelMessage>,
}

impl RecordingModel {
    fn new(responses: Vec<crate::model::ModelResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn take_requests(&self) -> Vec<RecordedRequest> {
        std::mem::take(
            &mut *self
                .requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }
}

impl ModelProvider for RecordingModel {
    fn respond<'a>(&'a self, request: ModelRequest) -> crate::model::ModelFuture<'a> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(RecordedRequest {
                tools: request.tools.clone(),
                items: request.history.items().to_vec(),
                messages: request.history.to_model_messages(),
            });
        let next = self
            .responses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front();
        Box::pin(async move { next.ok_or(crate::model::ModelError::EmptyResponse) })
    }
}

#[tokio::test]
async fn exact_retry_replaces_the_selected_prompt_without_deduping_intentional_repetition() -> TestResult {
    let mut session = make_session(StubToolProvider::with_names(&[]), SessionActivation::new(ToolProfile::Full));
    let store = crate::state_store::InMemoryStateStore::new();
    let prefix = session.history_mut().push_user_text("same words");
    let target = session.history_mut().push_user_text("same words");
    let suffix = session.history_mut().push_user_text("must not reach model");
    store.save_history(session.id(), session.history()).await.map_err(ctx("save fixture"))?;
    let checkpoint_notice = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observer = checkpoint_notice.clone();
    let model = RecordingModel::new(vec![crate::model::ModelResponse::text("new answer")]);
    let input = TurnInput::user("")
        .with_rewind_before(Some(target.as_str().into()))
        .with_control(TurnControl::new().with_history_replaced(Arc::new(move || {
            observer.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        })));
    let outcome = run_turn(&mut session, &model, &store, input).await.map_err(ctx("run retry"))?;
    assert!(matches!(outcome, TurnOutcome::Completed));
    assert_eq!(checkpoint_notice.load(std::sync::atomic::Ordering::Relaxed), 1);
    let requests = model.take_requests();
    assert_eq!(requests.len(), 1);
    let users = requests[0].items.iter().filter_map(|item| match item {
        TurnItem::UserMessage(prompt) => Some(prompt),
        _ => None,
    }).collect::<Vec<_>>();
    assert_eq!(users.len(), 2);
    assert_eq!(users[0].id, prefix);
    assert_ne!(users[1].id, target);
    assert_ne!(users[1].id, suffix);
    assert!(matches!(&users[1].content[..], [harw_protocol::items::ContentPart::Text { text }] if text == "same words"));
    Ok(())
}

#[tokio::test]
async fn an_attached_image_reaches_the_model_request_with_its_bytes() -> TestResult {
    use harw_media::MemorySource;
    let source = std::sync::Arc::new(MemorySource::new());
    crate::media::install_media_source(source.clone());
    let png = {
        let chunk = |kind: &[u8; 4], data: &[u8]| {
            let mut out = u32::try_from(data.len())
                .unwrap_or(0)
                .to_be_bytes()
                .to_vec();
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            out.extend_from_slice(&[0, 0, 0, 0]);
            out
        };
        let mut ihdr = 12u32.to_be_bytes().to_vec();
        ihdr.extend_from_slice(&9u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        out.extend_from_slice(&chunk(b"IHDR", &ihdr));
        out.extend_from_slice(&chunk(b"IDAT", b"turn-loop-image"));
        out.extend_from_slice(&chunk(b"IEND", b""));
        out
    };
    let media = source
        .put(&png)
        .map_err(|e| TestError::Unexpected(e.to_string()))?;

    let (tx, _rx) = mpsc::unbounded_channel();
    let registry = ExtensionRegistryBuilder::default()
        .tool_provider(StubToolProvider::with_names(&[]))
        .build();
    let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx);
    let store = crate::state_store::InMemoryStateStore::new();
    let model = RecordingModel::new(vec![crate::model::ModelResponse::text("a small image")]);

    run_turn(
        &mut session,
        &model,
        &store,
        TurnInput::user("what is this?").with_images([media.clone()]),
    )
    .await
    .map_err(ctx("turn with an image"))?;

    let requests = model.take_requests();
    let first = requests
        .first()
        .ok_or(TestError::Missing("model request"))?;
    let Some(crate::ModelMessage::User { text, images }) = first.messages.first() else {
        return Err(TestError::Unexpected(format!("{:?}", first.messages)));
    };
    assert_eq!(text, "what is this?");
    assert_eq!(images.len(), 1);
    let image = images.first().ok_or(TestError::Missing("image"))?;
    assert_eq!(image.media, media);
    assert!(image.bytes().is_some_and(|b| b.starts_with(b"\x89PNG")));
    // The persisted transcript holds the reference, not the pixels.
    let stored = serde_json::to_string(session.history())
        .map_err(|e| TestError::Unexpected(e.to_string()))?;
    assert!(stored.contains("\"type\":\"media\""));
    assert!(!stored.contains("turn-loop-image"));
    Ok(())
}

#[tokio::test]
async fn an_image_without_text_starts_a_turn_too() -> TestResult {
    use harw_media::MemorySource;
    let source = std::sync::Arc::new(MemorySource::new());
    crate::media::install_media_source(source.clone());
    let png = {
        let chunk = |kind: &[u8; 4], data: &[u8]| {
            let mut out = u32::try_from(data.len())
                .unwrap_or(0)
                .to_be_bytes()
                .to_vec();
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            out.extend_from_slice(&[0, 0, 0, 0]);
            out
        };
        let mut ihdr = 5u32.to_be_bytes().to_vec();
        ihdr.extend_from_slice(&5u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        out.extend_from_slice(&chunk(b"IHDR", &ihdr));
        out.extend_from_slice(&chunk(b"IDAT", b"image-only"));
        out.extend_from_slice(&chunk(b"IEND", b""));
        out
    };
    let media = source
        .put(&png)
        .map_err(|e| TestError::Unexpected(e.to_string()))?;
    let (tx, _rx) = mpsc::unbounded_channel();
    let registry = ExtensionRegistryBuilder::default()
        .tool_provider(StubToolProvider::with_names(&[]))
        .build();
    let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx);
    let store = crate::state_store::InMemoryStateStore::new();
    let model = RecordingModel::new(vec![crate::model::ModelResponse::text("ok")]);
    let input = TurnInput {
        user_images: vec![media],
        ..TurnInput::default()
    };
    run_turn(&mut session, &model, &store, input)
        .await
        .map_err(ctx("image-only turn"))?;
    let requests = model.take_requests();
    let Some(crate::ModelMessage::User { text, images }) = requests
        .first()
        .and_then(|request| request.messages.first())
    else {
        return Err(TestError::Missing("user message"));
    };
    assert!(text.is_empty());
    assert_eq!(images.len(), 1);
    Ok(())
}

#[tokio::test]
async fn child_session_persists_reasoning_but_never_sends_it_to_the_model() -> TestResult {
    // Kind-Session (mit Parent): das Reasoning-Item landet in History und
    // Store, der nächste Modell-Request sieht es aber nicht als Nachricht.
    let (tx, _rx) = mpsc::unbounded_channel();
    let registry = ExtensionRegistryBuilder::default()
        .tool_provider(StubToolProvider::with_names(&[]))
        .build();
    let mut session =
        AgentSession::new(AgentRole::Assistant, Some(SessionId::new()), registry, tx);
    let store = crate::state_store::InMemoryStateStore::new();
    let thought = "geheimer Gedankengang des Kindes";
    let model = RecordingModel::new(vec![
        crate::model::ModelResponse {
            reasoning: Some(anthropic_reasoning(vec![serde_json::json!({
                "type": "thinking",
                "thinking": thought,
            })])),
            ..crate::model::ModelResponse::text("erste Antwort")
        },
        crate::model::ModelResponse::text("zweite Antwort"),
    ]);

    run_turn(&mut session, &model, &store, TurnInput::user("eins"))
        .await
        .map_err(ctx("erster Turn"))?;
    assert!(
        session
            .history()
            .items()
            .iter()
            .any(|item| matches!(item, TurnItem::Reasoning(_))),
        "die Kind-Session muss das Reasoning-Item persistieren"
    );
    let persisted = store
        .load_history(session.id())
        .await
        .map_err(ctx("History aus dem Store laden"))?;
    assert!(
        persisted
            .items()
            .iter()
            .any(|item| matches!(item, TurnItem::Reasoning(_))),
        "das Reasoning-Item muss auch im StateStore stehen"
    );

    run_turn(&mut session, &model, &store, TurnInput::user("zwei"))
        .await
        .map_err(ctx("zweiter Turn"))?;
    let requests = model.take_requests();
    let second = requests
        .get(1)
        .ok_or(TestError::Missing("ein zweiter Modell-Request"))?;
    assert!(
        second
            .items
            .iter()
            .any(|item| matches!(item, TurnItem::Reasoning(_))),
        "die mitgegebene History trägt das Item weiterhin"
    );
    assert!(
        !format!("{:?}", second.messages).contains(thought),
        "Reasoning-Text darf nie Teil der Modell-Nachrichten werden"
    );
    assert!(
        format!("{:?}", second.messages).contains("erste Antwort"),
        "die übrige History wird normal übertragen"
    );
    Ok(())
}

// ------------------------------------------------------------------
// Handoff: Auftrag + Werkzeugdefinitionen (Runde 2)
// ------------------------------------------------------------------

#[test]
fn handoff_instructions_prefer_task_fields_then_fall_back_to_json() {
    assert_eq!(
        handoff_instructions(&serde_json::json!({"task": "  prüfe X  "})),
        Some("prüfe X".to_owned())
    );
    assert_eq!(
        handoff_instructions(&serde_json::json!({"task": "prüfe X", "context": "Pfad a/b"})),
        Some("prüfe X\n\nKontext:\nPfad a/b".to_owned())
    );
    assert_eq!(
        handoff_instructions(&serde_json::json!({"instructions": "baue Y"})),
        Some("baue Y".to_owned())
    );
    assert_eq!(
        handoff_instructions(&serde_json::json!({"objective": "Ziel Z", "task": ""})),
        Some("Ziel Z".to_owned())
    );
    assert_eq!(
        handoff_instructions(&serde_json::json!({"question": "wo?"})),
        Some("wo?".to_owned())
    );
    assert_eq!(
        handoff_instructions(&serde_json::json!({"files": ["a.rs"], "task": 3})),
        Some(r#"{"files":["a.rs"],"task":3}"#.to_owned())
    );
    assert_eq!(
        handoff_instructions(&serde_json::json!("nackter Auftrag")),
        Some("nackter Auftrag".to_owned())
    );
    assert_eq!(handoff_instructions(&serde_json::json!({})), None);
    assert_eq!(handoff_instructions(&serde_json::Value::Null), None);
}

/// Spawner, der den übergebenen `SpawnInput` festhält und feste
/// Delegationsziele meldet.
struct CapturingSpawner {
    child: SessionId,
    targets: Vec<String>,
    captured: Mutex<Option<SpawnInput>>,
}

impl AgentSpawner for CapturingSpawner {
    fn spawn_child<'a>(
        &'a self,
        _role: &'a str,
        input: SpawnInput,
        _sandbox: SandboxSpec,
        _suggestions: Option<AgentSuggestions>,
    ) -> SpawnFuture<'a> {
        *self
            .captured
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(input);
        let child = self.child.clone();
        Box::pin(async move { Ok(child) })
    }

    fn delegation_target_names(&self, _parent_session_id: &SessionId) -> Vec<String> {
        self.targets.clone()
    }
}

#[tokio::test]
async fn handoff_fills_spawn_instructions_and_offers_transfer_tools() -> TestResult {
    let spawner = Arc::new(CapturingSpawner {
        child: SessionId::new(),
        targets: vec![
            "explorer".to_owned(),
            "worker".to_owned(),
            "bad name".to_owned(),
        ],
        captured: Mutex::new(None),
    });
    let (tx, _rx) = mpsc::unbounded_channel();
    let registry = ExtensionRegistryBuilder::default()
        .tool_provider(StubToolProvider::with_names(&["fs.read"]))
        .spawner(spawner.clone())
        .agent_job_submitter(RecordingJobSubmitter::new())
        .build();
    let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx)
        .with_activation(SessionActivation::new(ToolProfile::Minimal))
        .with_spawn_context(test_spawn_context()?);
    let store = crate::state_store::InMemoryStateStore::new();
    let model = RecordingModel::new(vec![
        response_with(vec![ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("transfer_to_worker"),
            arguments: serde_json::json!({"task": "finde den Fehler", "context": "in lib.rs"}),
        }]),
        crate::model::ModelResponse::text("started"),
    ]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("delegiere"))
        .await
        .map_err(ctx("the background start does not pause the turn"))?;
    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");

    // (1) Auftrag im SpawnInput.
    let input = spawner
        .captured
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
        .ok_or(TestError::Missing("der Spawner muss aufgerufen werden"))?;
    assert_eq!(
        input.instructions.as_deref(),
        Some("finde den Fehler\n\nKontext:\nin lib.rs")
    );
    assert_eq!(
        input.context,
        serde_json::json!({"task": "finde den Fehler", "context": "in lib.rs"})
    );

    // (3) Werkzeugdefinitionen: auch unter `Minimal` sichtbar, gültige
    // Namen nur, stabil sortiert.
    let requests = model.take_requests();
    let first = requests
        .first()
        .ok_or(TestError::Missing("ein Modell-Request"))?;
    let names: Vec<&str> = first.tools.iter().map(ToolSpec::name).collect();
    assert!(names.contains(&"transfer_to_explorer"));
    assert!(names.contains(&"transfer_to_worker"));
    assert!(!names.iter().any(|name| name.contains(' ')));
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "Werkzeugliste bleibt nach Namen sortiert");
    let worker = first
        .tools
        .iter()
        .find(|spec| spec.name() == "transfer_to_worker")
        .ok_or(TestError::Missing("transfer_to_worker-Definition"))?;
    let ToolSpec::Function(function) = worker;
    assert_eq!(function.parameters.required, Some(vec!["task".to_owned()]));
    let properties = function
        .parameters
        .properties
        .as_ref()
        .ok_or(TestError::Missing("Schema-Properties"))?;
    assert!(properties.contains_key("task"));
    assert!(properties.contains_key("context"));
    // Delegation is background-only: no mode parameters exist any more.
    assert!(!properties.contains_key("background"));
    assert!(!properties.contains_key("wait"));
    // Runde 5, Teil J: optionale Fortsetzung, der Auftrag bleibt Pflicht.
    assert!(properties.contains_key("continue_from"));
    // Runde 9, E4: optionale Freigaben der Nutzerin.
    assert!(properties.contains_key("user_approved"));
    Ok(())
}

/// Runde 9, E4: `user_approved` wird zur ersten Zeile des Auftrags; ohne
/// das Feld bleibt der Auftrag unverändert.
#[test]
fn handoff_instructions_lead_with_the_user_approval() {
    assert_eq!(
        handoff_instructions(&serde_json::json!({
            "task": "Spiele das Szenario harw-oss-fuenfjahre",
            "user_approved": ["scenario"],
        }))
        .as_deref(),
        Some(
            "Die Nutzerin hat vorab freigegeben: scenario.\n\n\
                 Spiele das Szenario harw-oss-fuenfjahre"
        )
    );
    assert_eq!(
        handoff_instructions(&serde_json::json!({
            "task": "Spiele",
            "user_approved": ["alles"],
        }))
        .as_deref(),
        Some("Spiele")
    );
}

/// Spawner, der ein beendetes eigenes Kind als fortsetzbar meldet
/// (Runde 9, E3), den fragenden Aufrufer und den `SpawnInput` festhält.
struct ResumingSpawner {
    finished: SessionId,
    spawned: SessionId,
    callers: Mutex<Vec<SessionId>>,
    captured: Mutex<Option<(String, SpawnInput)>>,
}

impl AgentSpawner for ResumingSpawner {
    fn spawn_child<'a>(
        &'a self,
        role: &'a str,
        input: SpawnInput,
        _sandbox: SandboxSpec,
        _suggestions: Option<AgentSuggestions>,
    ) -> SpawnFuture<'a> {
        *self
            .captured
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((role.to_owned(), input));
        let child = self.spawned.clone();
        Box::pin(async move { Ok(child) })
    }

    fn resumable_child_role(&self, caller: &SessionId, child_id: &str) -> Option<String> {
        self.callers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(caller.clone());
        (child_id == self.finished.as_str()).then(|| "matrix-game-master".to_owned())
    }
}

/// Runde 9, E3: `agent.message` an ein eigenes, beendetes Kind wird zur
/// Fortsetzung über `continue_from` mit der Nachricht als Auftrag; der
/// Aufrufer kommt aus der Sitzung, nie aus den Argumenten.
#[tokio::test]
async fn agent_message_to_a_finished_own_child_resumes_it() -> TestResult {
    let finished = SessionId::new();
    let spawner = Arc::new(ResumingSpawner {
        finished: finished.clone(),
        spawned: SessionId::new(),
        callers: Mutex::new(Vec::new()),
        captured: Mutex::new(None),
    });
    let (tx, _rx) = mpsc::unbounded_channel();
    let registry = ExtensionRegistryBuilder::default()
        .tool_provider(StubToolProvider::with_names(&["agent.message"]))
        .spawner(spawner.clone())
        .agent_job_submitter(RecordingJobSubmitter::new())
        .build();
    let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx)
        .with_activation(SessionActivation::new(ToolProfile::Minimal))
        .with_spawn_context(test_spawn_context()?);
    let store = crate::state_store::InMemoryStateStore::new();
    let model = RecordingModel::new(vec![
        response_with(vec![ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(crate::child_comms::AGENT_MESSAGE_TOOL),
            arguments: serde_json::json!({
                "child_id": finished.as_str(),
                "text": "Die Nutzerin gibt das Szenario frei.",
            }),
        }]),
        crate::model::ModelResponse::text("resumed"),
    ]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("antworte"))
        .await
        .map_err(ctx("the resumption starts in the background"))?;
    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    let (spawned_role, input) = spawner
        .captured
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
        .ok_or(TestError::Missing("der Spawner muss aufgerufen werden"))?;
    assert_eq!(spawned_role, "matrix-game-master");
    assert_eq!(
        input.context,
        serde_json::json!({
            "task": "Die Nutzerin gibt das Szenario frei.",
            "continue_from": finished.as_str(),
        })
    );
    assert_eq!(
        input.instructions.as_deref(),
        Some("Die Nutzerin gibt das Szenario frei.")
    );
    assert_eq!(&input.parent_session_id, session.id());
    let callers = spawner
        .callers
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    assert!(callers.iter().all(|caller| caller == session.id()));

    // Andere Aufrufe bleiben unberührt.
    let other = ToolCall {
        id: ToolCallId::new(),
        name: ToolName::new(crate::child_comms::AGENT_MESSAGE_TOOL),
        arguments: serde_json::json!({ "child_id": SessionId::new().as_str(), "text": "x" }),
    };
    assert!(message_resume_handoff(&session, &other).is_none());
    let not_message = ToolCall {
        id: ToolCallId::new(),
        name: ToolName::new("agent.status"),
        arguments: serde_json::json!({ "child_id": finished.as_str(), "text": "x" }),
    };
    assert!(message_resume_handoff(&session, &not_message).is_none());
    let empty = ToolCall {
        id: ToolCallId::new(),
        name: ToolName::new(crate::child_comms::AGENT_MESSAGE_TOOL),
        arguments: serde_json::json!({ "child_id": finished.as_str(), "text": "  " }),
    };
    assert!(message_resume_handoff(&session, &empty).is_none());
    Ok(())
}

/// `transfer_to_uia-shell-worker` sagt, dass sudo über diesen Agenten
/// funktioniert (Freigabe und Passwort im TUI-Fenster); andere
/// Handoff-Beschreibungen bleiben ohne Zusatz.
#[test]
fn uia_shell_worker_handoff_names_the_sudo_path() {
    let ToolSpec::Function(shell) = handoff_tool_spec("uia-shell-worker", None);
    for phrase in [
        "sudo funktioniert über diesen Agenten",
        "`host.sudo_exec`",
        "gibt dort sein Passwort ein",
        "nie ein Passwort",
    ] {
        assert!(
            shell.description.contains(phrase),
            "{phrase}: {}",
            shell.description
        );
    }
    let ToolSpec::Function(explorer) = handoff_tool_spec("explorer", None);
    assert!(
        !explorer.description.contains("sudo"),
        "{}",
        explorer.description
    );
}

#[test]
fn explicitly_disabled_handoff_tool_is_not_offered() {
    let mut activation = SessionActivation::new(ToolProfile::Full);
    activation.disable_tool(ToolName::new("transfer_to_worker"));
    let session = make_session(StubToolProvider::with_names(&[]), activation);
    let mut tools = Vec::new();
    append_handoff_tools(&session, &mut tools, &target_infos(&["explorer", "worker"]));
    let names: Vec<&str> = tools.iter().map(ToolSpec::name).collect();
    assert_eq!(names, vec![AGENTS_CATALOG_TOOL, "transfer_to_explorer"]);
}

/// Plan R9, E1: ein Spawner ohne Lese-Kenntnis (Default des Traits)
/// bietet im Plan-Modus weiterhin kein Ziel an — erst der Katalog des
/// `ManagedAgentSpawner` hält lesende Ziele sichtbar.
#[test]
fn plan_mode_without_read_only_knowledge_offers_no_handoff_tools() -> TestResult {
    let spawner = CapturingSpawner {
        child: SessionId::new(),
        targets: vec!["worker".to_owned()],
        captured: Mutex::new(None),
    };
    let delegation = spawner
        .delegation_targets(&SessionId::new(), true)
        .map_err(|error| TestError::Unexpected(error.to_string()))?;
    assert!(delegation.targets.is_empty());
    assert!(delegation.is_withheld_by_plan_mode("worker"));
    let session = make_session(
        StubToolProvider::with_names(&[]),
        SessionActivation::new(ToolProfile::Full),
    );
    let mut tools = Vec::new();
    append_handoff_tools(&session, &mut tools, &delegation.targets);
    assert!(
        tools.is_empty(),
        "ohne lesende Ziele keine Delegationswerkzeuge"
    );
    Ok(())
}

#[test]
fn registry_tool_wins_over_generated_handoff_definition() -> TestResult {
    let session = make_session(
        StubToolProvider::with_names(&["transfer_to_worker"]),
        SessionActivation::new(ToolProfile::Full),
    );
    let mut tools = collect_tools(&session).map_err(ctx("Werkzeuge sammeln"))?;
    append_handoff_tools(&session, &mut tools, &target_infos(&["worker"]));
    let handoffs = tools
        .iter()
        .filter(|spec| spec.name() == "transfer_to_worker")
        .count();
    assert_eq!(handoffs, 1, "kein Duplikat neben dem Registry-Werkzeug");
    Ok(())
}

// ── Plan R9, Teil C/E1: Katalog, agents.delegate, Schwelle, Plan-Modus ──

fn target_infos(names: &[&str]) -> Vec<DelegationTargetInfo> {
    names
        .iter()
        .map(|name| DelegationTargetInfo::named((*name).to_owned()))
        .collect()
}

fn catalog_entry(
    name: &str,
    role: &str,
    read_only: bool,
    custom: bool,
    description: &str,
) -> DelegationTargetInfo {
    DelegationTargetInfo {
        name: name.to_owned(),
        role: role.to_owned(),
        description: Some(description.to_owned()),
        skills: vec!["evidence-quality-review".to_owned()],
        profile_summary: if read_only {
            "read-only; 12 tools".to_owned()
        } else {
            "writes/executes; 20 tools".to_owned()
        },
        read_only,
        budget_tokens: Some(200_000),
        custom,
    }
}

/// Katalog wie im Lauf: lesende und schreibende Ziele gemischt.
fn analysis_catalog() -> Vec<DelegationTargetInfo> {
    vec![
        catalog_entry(
            "evidence-critic",
            "worker",
            true,
            true,
            "Prüft Belege eines Analyseprodukts. Read-only.",
        ),
        catalog_entry(
            "executor",
            "worker",
            false,
            false,
            "Führt Befehle aus und schreibt Dateien.",
        ),
        catalog_entry(
            "explorer",
            "worker",
            true,
            false,
            "Erkundet den Workspace lesend.",
        ),
    ]
}

/// Spawner mit Katalogdaten und derselben Plan-Modus-Regel wie der
/// `ManagedAgentSpawner` ([`crate::delegation_visibility::delegable_in_mode`]).
struct CatalogSpawner {
    child: SessionId,
    catalog: Vec<DelegationTargetInfo>,
    captured: Mutex<Option<(String, SpawnInput)>>,
}

impl CatalogSpawner {
    fn new(catalog: Vec<DelegationTargetInfo>) -> Arc<Self> {
        Arc::new(Self {
            child: SessionId::new(),
            catalog,
            captured: Mutex::new(None),
        })
    }

    fn take(&self) -> Option<(String, SpawnInput)> {
        self.captured
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

impl AgentSpawner for CatalogSpawner {
    fn spawn_child<'a>(
        &'a self,
        role: &'a str,
        input: SpawnInput,
        _sandbox: SandboxSpec,
        _suggestions: Option<AgentSuggestions>,
    ) -> SpawnFuture<'a> {
        *self
            .captured
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((role.to_owned(), input));
        let child = self.child.clone();
        Box::pin(async move { Ok(child) })
    }

    fn delegation_target_names(&self, _parent_session_id: &SessionId) -> Vec<String> {
        self.catalog
            .iter()
            .map(|target| target.name.clone())
            .collect()
    }

    fn delegation_targets(
        &self,
        _parent_session_id: &SessionId,
        plan_mode: bool,
    ) -> Result<harw_extension_api::DelegationTargets, harw_extension_api::DelegationUnavailable>
    {
        let mut result = harw_extension_api::DelegationTargets {
            plan_mode,
            ..harw_extension_api::DelegationTargets::default()
        };
        for target in &self.catalog {
            if crate::delegation_visibility::delegable_in_mode(plan_mode, target.read_only) {
                result.targets.push(target.clone());
            } else {
                result.withheld_by_plan_mode.push(target.clone());
            }
        }
        Ok(result)
    }
}

fn catalog_session(spawner: Arc<CatalogSpawner>) -> TestResult<AgentSession> {
    catalog_session_with_approval(spawner, None)
}

fn catalog_session_with_approval(
    spawner: Arc<CatalogSpawner>,
    approval: Option<Arc<dyn harw_extension_api::ApprovalHandler>>,
) -> TestResult<AgentSession> {
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut registry = ExtensionRegistryBuilder::default()
        .tool_provider(StubToolProvider::with_names(&["fs.read"]))
        .spawner(spawner)
        .agent_job_submitter(RecordingJobSubmitter::new());
    if let Some(approval) = approval {
        registry = registry.approval_handler(approval);
    }
    Ok(
        AgentSession::new(AgentRole::Assistant, None, registry.build(), tx)
            .with_activation(SessionActivation::new(ToolProfile::Minimal))
            .with_spawn_context(test_spawn_context()?),
    )
}

#[tokio::test]
async fn agents_catalog_runs_after_explicit_approval() -> TestResult {
    catalog_after_explicit_approval(false).await
}

#[tokio::test]
async fn agents_catalog_disabled_during_approval_stays_unavailable() -> TestResult {
    catalog_after_explicit_approval(true).await
}

async fn catalog_after_explicit_approval(disabled: bool) -> TestResult {
    let id = ToolCallId::new();
    let approval = Arc::new(CountingApproval::new(vec![(
        id.clone(),
        ApprovalDecision::AskUser(harw_types::ItemId::new()),
    )]));
    let mut session =
        catalog_session_with_approval(CatalogSpawner::new(analysis_catalog()), Some(approval))?;
    session.set_mode(crate::mode::InteractionMode::Plan);
    let model = RecordingModel::new(vec![
        response_with(vec![ToolCall {
            id,
            name: ToolName::new(AGENTS_CATALOG_TOOL),
            arguments: serde_json::json!({}),
        }]),
        crate::model::ModelResponse::text("done"),
    ]);
    let store = crate::state_store::InMemoryStateStore::new();
    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("research"))
        .await
        .map_err(ctx("catalog approval"))?;
    assert!(matches!(outcome, TurnOutcome::AwaitingApproval { .. }));
    if disabled {
        *session.activation_mut() = SessionActivation::new(ToolProfile::Full);
        session
            .activation_mut()
            .disable_tool(ToolName::new(AGENTS_CATALOG_TOOL));
    }
    let actor = session
        .pending_approval()
        .ok_or(TestError::Missing("pending approval"))?
        .actor
        .clone();
    let outcome = resume_after_approval(
        &mut session,
        &model,
        &store,
        actor,
        ApprovalResolution::Approve,
    )
    .await
    .map_err(ctx("approved catalog"))?;
    assert!(matches!(outcome, TurnOutcome::Completed));
    let results = tool_results(&session);
    if disabled {
        assert!(
            matches!(results.as_slice(), [ToolCallResult::Error { .. }]),
            "{results:?}"
        );
        return Ok(());
    }
    let [ToolCallResult::Success { value }] = results.as_slice() else {
        return Err(TestError::Unexpected(format!("{results:?}")));
    };
    assert_eq!(value["count"], serde_json::json!(2));
    assert!(!value["agents"].to_string().contains("executor"));
    Ok(())
}

/// Generated tools have no registry executor, but must survive the child
/// approval relay and dispatch through the same path as an ordinary turn.
#[tokio::test]
async fn generated_delegation_tools_survive_child_approval() -> TestResult {
    use crate::child_approval::{
        ChildApprovalAnswer, ChildApprovalBroker, ChildApprovalRequest,
    };
    struct Approve;
    impl ChildApprovalBroker for Approve {
        fn submit(
            &self,
            _request: ChildApprovalRequest,
        ) -> Option<tokio::sync::oneshot::Receiver<ChildApprovalAnswer>> {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = tx.send(ChildApprovalAnswer::Approve);
            Some(rx)
        }
    }

    for (name, disabled, allowed) in [
        (AGENTS_CATALOG_TOOL, false, true),
        ("transfer_to_explorer", false, true),
        (AGENTS_DELEGATE_TOOL, false, true),
        (AGENTS_CATALOG_TOOL, true, false),
        ("transfer_to_explorer", true, false),
        (AGENTS_DELEGATE_TOOL, true, false),
        ("transfer_to_executor", false, false),
        ("host.sudo_exec", false, false),
    ] {
        let mut catalog = analysis_catalog();
        if name == AGENTS_DELEGATE_TOOL {
            for n in 0..8 {
                catalog.push(catalog_entry(
                    &format!("reader-{n}"),
                    "worker",
                    true,
                    true,
                    "read",
                ));
            }
        }
        let spawner = CatalogSpawner::new(catalog);
        let id = ToolCallId::new();
        let approval = Arc::new(CountingApproval::new(vec![(
            id.clone(),
            ApprovalDecision::AskUser(harw_types::ItemId::new()),
        )]));
        let mut session = catalog_session_with_approval(Arc::clone(&spawner), Some(approval))?;
        if name != "host.sudo_exec" {
            session.set_mode(crate::mode::InteractionMode::Plan);
        }
        let model = RecordingModel::new(vec![
            response_with(vec![ToolCall {
                id,
                name: ToolName::new(name),
                arguments: if name == AGENTS_CATALOG_TOOL {
                    serde_json::json!({})
                } else {
                    serde_json::json!({"agent": "explorer", "task": "read"})
                },
            }]),
            crate::model::ModelResponse::text("done"),
        ]);
        let store = crate::state_store::InMemoryStateStore::new();
        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("research"))
            .await
            .map_err(ctx("pause for approval"))?;
        assert!(
            matches!(outcome, TurnOutcome::AwaitingApproval { .. }),
            "{outcome:?}"
        );
        if disabled {
            *session.activation_mut() = SessionActivation::new(ToolProfile::Full);
            session.activation_mut().disable_tool(ToolName::new(name));
        }
        let (tx, _rx) = mpsc::unbounded_channel();
        let controller = crate::child_controller::ManagedAgentSpawner::new(
            Arc::new(Mutex::new(crate::SessionManager::new(tx))),
            crate::child_controller::ChildLimits::default(),
        );
        controller.attach_child_approval_broker(Arc::new(Approve));
        let child = session.id().clone();
        let outcome = crate::child_approval::relay_child_approvals(
            &controller,
            &child,
            &mut session,
            &model,
            &store,
            None,
            outcome,
        )
        .await
        .map_err(ctx("relay generated tool approval"))?;
        assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
        let results = tool_results(&session);
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].is_success(),
            allowed,
            "{name}, disabled={disabled}: {results:?}"
        );
        if allowed && name == AGENTS_CATALOG_TOOL {
            let ToolCallResult::Success { value } = &results[0] else {
                return Err(TestError::Unexpected(format!("{results:?}")));
            };
            assert_eq!(value["count"], serde_json::json!(2));
            assert!(!value["agents"].to_string().contains("executor"));
        }
        assert_eq!(
            spawner.take().is_some(),
            allowed && name != AGENTS_CATALOG_TOOL
        );
    }
    Ok(())
}

fn tool_results(session: &AgentSession) -> Vec<ToolCallResult> {
    session
        .history()
        .items()
        .iter()
        .filter_map(|item| match item {
            TurnItem::ToolResult(result) => Some(result.result.clone()),
            _ => None,
        })
        .collect()
}

/// Genau ein Werkzeugergebnis, ein Fehler, der mit `expected` beginnt
/// (ein Wächter-Hinweis dürfte angehängt sein).
fn expect_single_error(session: &AgentSession, expected: &str) -> TestResult {
    match tool_results(session).as_slice() {
        [ToolCallResult::Error { message }] if message.starts_with(expected) => Ok(()),
        other => Err(TestError::Unexpected(format!(
            "erwartet genau den Fehler „{expected}“, bekommen {other:?}"
        ))),
    }
}

fn find_function<'a>(tools: &'a [ToolSpec], name: &str) -> TestResult<&'a FunctionToolSpec> {
    tools
        .iter()
        .find_map(|spec| {
            let ToolSpec::Function(function) = spec;
            (function.name.as_str() == name).then_some(function)
        })
        .ok_or_else(|| TestError::Unexpected(format!("Werkzeug {name} fehlt")))
}

/// Mehr als 8 Ziele: nur `agents.delegate` (Enum der Ziele, sortiert)
/// plus `agents.catalog`, kein einziges `transfer_to_*`.
#[test]
fn more_than_eight_targets_switch_to_agents_delegate() -> TestResult {
    let session = make_session(
        StubToolProvider::with_names(&[]),
        SessionActivation::new(ToolProfile::Minimal),
    );
    let names: Vec<String> = (0..9).map(|index| format!("agent-{index}")).collect();
    let infos: Vec<DelegationTargetInfo> = names
        .iter()
        .map(|name| DelegationTargetInfo::named(name.clone()))
        .collect();
    let mut tools = Vec::new();
    append_handoff_tools(&session, &mut tools, &infos);
    let offered: Vec<&str> = tools.iter().map(ToolSpec::name).collect();
    assert_eq!(offered, vec![AGENTS_CATALOG_TOOL, AGENTS_DELEGATE_TOOL]);
    let delegate = find_function(&tools, AGENTS_DELEGATE_TOOL)?;
    let properties = delegate
        .parameters
        .properties
        .as_ref()
        .ok_or(TestError::Missing("Properties"))?;
    let agent = properties.get("agent").ok_or(TestError::Missing("agent"))?;
    let expected: Vec<serde_json::Value> = names
        .iter()
        .map(|name| serde_json::Value::String(name.clone()))
        .collect();
    assert_eq!(agent.enum_values.as_ref(), Some(&expected));
    for field in ["task", "context", "continue_from", "user_approved"] {
        assert!(properties.contains_key(field), "{field}");
    }
    for field in ["background", "wait"] {
        assert!(!properties.contains_key(field), "{field} must not exist");
    }
    assert_eq!(
        delegate.parameters.required,
        Some(vec!["agent".to_owned(), "task".to_owned()])
    );

    // Genau 8 Ziele: die Einzelwerkzeuge bleiben.
    let mut tools = Vec::new();
    append_handoff_tools(&session, &mut tools, &infos[..8]);
    assert_eq!(
        tools
            .iter()
            .filter(|spec| spec.name().starts_with(HANDOFF_PREFIX))
            .count(),
        8
    );
    assert!(!tools.iter().any(|spec| spec.name() == AGENTS_DELEGATE_TOOL));
    Ok(())
}

/// Die UIA (≤ 8 Ziele) behält `transfer_to_*`; jede Beschreibung trägt
/// die Einzeilen-Beschreibung des Ziels, gekappt auf 90 Zeichen.
#[test]
fn uia_keeps_transfer_tools_with_one_line_descriptions() -> TestResult {
    let session = make_session(
        StubToolProvider::with_names(&[]),
        SessionActivation::new(ToolProfile::Minimal),
    );
    let long = "Sehr lange Beschreibung ".repeat(10);
    let targets = vec![
        catalog_entry(
            "root-orchestrator",
            "root-orchestrator",
            true,
            false,
            "Plant und verteilt größere Aufgaben. Zweiter Satz bleibt draußen.",
        ),
        catalog_entry("uia-writer", "uia-worker", false, false, &long),
    ];
    let mut tools = Vec::new();
    append_handoff_tools(&session, &mut tools, &targets);
    let root = find_function(&tools, "transfer_to_root-orchestrator")?;
    assert!(
        root.description
            .contains("Plant und verteilt größere Aufgaben."),
        "{}",
        root.description
    );
    assert!(!root.description.contains("Zweiter Satz"));
    let writer = find_function(&tools, "transfer_to_uia-writer")?;
    let summary = writer
        .description
        .split("polling). ")
        .nth(1)
        .ok_or(TestError::Missing("Einzeiler"))?;
    assert!(
        summary.chars().count() <= HANDOFF_DESCRIPTION_CHARS,
        "{summary}"
    );
    assert!(summary.ends_with('…'));
    assert!(tools.iter().any(|spec| spec.name() == AGENTS_CATALOG_TOOL));
    Ok(())
}

/// `agents.delegate` spawnt über denselben Handoff-Weg wie
/// `transfer_to_*`: gleiche Rolle, Auftrag samt Freigabe-Zeile, Argumente
/// ohne `agent`; der Start kehrt sofort zurück (Hintergrund, kein Turn-Halt).
#[tokio::test]
async fn agents_delegate_spawns_through_the_handoff_path() -> TestResult {
    let spawner = CatalogSpawner::new(analysis_catalog());
    let mut session = catalog_session(Arc::clone(&spawner))?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = RecordingModel::new(vec![
        response_with(vec![ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(AGENTS_DELEGATE_TOOL),
            arguments: serde_json::json!({
                "agent": "evidence-critic",
                "task": "Prüfe die Quellen",
                "user_approved": ["plan"],
            }),
        }]),
        crate::model::ModelResponse::text("started"),
    ]);
    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("delegiere"))
        .await
        .map_err(ctx("agents.delegate starts in the background"))?;
    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    let (spawned_role, input) = spawner
        .take()
        .ok_or(TestError::Missing("der Spawner muss aufgerufen werden"))?;
    assert_eq!(spawned_role, "evidence-critic");
    assert_eq!(
        input.instructions.as_deref(),
        Some("Die Nutzerin hat vorab freigegeben: plan.\n\nPrüfe die Quellen")
    );
    assert!(input.context.get("agent").is_none());
    assert!(input.context.get("background").is_none());
    assert_eq!(&input.parent_session_id, session.id());
    Ok(())
}

/// Ein nicht sichtbares Ziel über `agents.delegate` ist ein benannter
/// Werkzeugfehler (nur sichtbare Namen), kein Turn-Abbruch und kein Spawn.
#[tokio::test]
async fn agents_delegate_names_the_visible_targets_for_an_unknown_agent() -> TestResult {
    let spawner = CatalogSpawner::new(analysis_catalog());
    let mut session = catalog_session(Arc::clone(&spawner))?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = RecordingModel::new(vec![
        response_with(vec![ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(AGENTS_DELEGATE_TOOL),
            arguments: serde_json::json!({ "agent": "geheim", "task": "x" }),
        }]),
        crate::model::ModelResponse::text("verstanden"),
    ]);
    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("delegiere"))
        .await
        .map_err(ctx("der Turn läuft weiter"))?;
    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    assert!(spawner.take().is_none(), "kein Spawn");
    expect_single_error(
        &session,
        "Ziel geheim ist für dich nicht delegierbar; delegierbar sind: \
             evidence-critic, executor, explorer",
    )?;
    Ok(())
}

/// `agents.catalog` zeigt nur sichtbare Ziele (mit Rolle, Skills, Rechten,
/// Budget, Herkunft), filtert nach Rolle und rankt nach Suchbegriffen.
#[tokio::test]
async fn agents_catalog_lists_only_visible_targets() -> TestResult {
    let spawner = CatalogSpawner::new(analysis_catalog());
    let mut session = catalog_session(Arc::clone(&spawner))?;
    session.set_mode(crate::mode::InteractionMode::Plan);
    let store = crate::state_store::InMemoryStateStore::new();
    let model = RecordingModel::new(vec![
        response_with(vec![ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(AGENTS_CATALOG_TOOL),
            arguments: serde_json::json!({}),
        }]),
        crate::model::ModelResponse::text("ok"),
    ]);
    run_turn(&mut session, &model, &store, TurnInput::user("wer?"))
        .await
        .map_err(ctx("Katalog"))?;
    let results = tool_results(&session);
    let Some(ToolCallResult::Success { value }) = results.first() else {
        return Err(TestError::Unexpected(format!("{results:?}")));
    };
    let names: Vec<&str> = value["agents"]
        .as_array()
        .ok_or(TestError::Missing("agents"))?
        .iter()
        .filter_map(|agent| agent["name"].as_str())
        .collect();
    // Plan-Modus: der schreibende `executor` ist nicht sichtbar.
    assert_eq!(names, vec!["evidence-critic", "explorer"]);
    assert_eq!(value["agents"][0]["source"], serde_json::json!("custom"));
    assert_eq!(value["agents"][1]["source"], serde_json::json!("built-in"));
    assert_eq!(value["agents"][0]["read_only"], serde_json::json!(true));
    assert_eq!(
        value["agents"][0]["budget_tokens"],
        serde_json::json!(200_000)
    );
    assert!(
        value["note"]
            .as_str()
            .is_some_and(|note| note.contains("Plan-Modus"))
    );

    // Außerhalb des Plan-Modus: Suche und Rollenfilter.
    let all = spawner
        .delegation_targets(session.id(), false)
        .map_err(|error| TestError::Unexpected(error.to_string()))?;
    let ToolCallResult::Success { value } =
        agents_catalog_result(&all, &serde_json::json!({ "query": "Belege prüfen" }))
    else {
        return Err(TestError::Unexpected("Katalog-Fehler".to_owned()));
    };
    assert_eq!(value["count"], serde_json::json!(1));
    assert_eq!(
        value["agents"][0]["name"],
        serde_json::json!("evidence-critic")
    );
    let ToolCallResult::Success { value } =
        agents_catalog_result(&all, &serde_json::json!({ "role": "child-orchestrator" }))
    else {
        return Err(TestError::Unexpected("Katalog-Fehler".to_owned()));
    };
    assert_eq!(value["count"], serde_json::json!(0));
    Ok(())
}

/// Plan R9, E1: im Plan-Modus bleiben lesende Ziele delegierbar
/// (`transfer_to_explorer` wird angeboten und spawnt), ein schreibendes
/// Ziel wird mit der Plan-Modus-Meldung abgelehnt — als Werkzeugfehler.
#[tokio::test]
async fn plan_mode_keeps_read_only_targets_and_refuses_writers() -> TestResult {
    let spawner = CatalogSpawner::new(analysis_catalog());
    let mut session = catalog_session(Arc::clone(&spawner))?;
    session.set_mode(crate::mode::InteractionMode::Plan);
    let store = crate::state_store::InMemoryStateStore::new();
    let model = RecordingModel::new(vec![
        response_with(vec![ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("transfer_to_executor"),
            arguments: serde_json::json!({ "task": "baue es" }),
        }]),
        response_with(vec![ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("transfer_to_explorer"),
            arguments: serde_json::json!({ "task": "erkunde es" }),
        }]),
        crate::model::ModelResponse::text("started"),
    ]);
    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("plane"))
        .await
        .map_err(ctx("Plan-Modus-Delegation"))?;
    // The explorer starts in the background; the turn is not paused.
    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    let (role, _) = spawner
        .take()
        .ok_or(TestError::Missing("the explorer must be spawned"))?;
    assert_eq!(role, "explorer");
    // First call (writer) is refused; second call (explorer) started.
    let results = tool_results(&session);
    match results.as_slice() {
        [
            ToolCallResult::Error { message },
            ToolCallResult::Success { .. },
        ] => assert!(
            message.starts_with(
                "Plan-Modus: nur lesende Ziele delegierbar (evidence-critic, explorer); \
                     Schreib-/Ausführungsziele erst nach Planfreigabe."
            ),
            "{message}"
        ),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    let requests = model.take_requests();
    let first = requests
        .first()
        .ok_or(TestError::Missing("ein Modell-Request"))?;
    let offered: Vec<&str> = first.tools.iter().map(ToolSpec::name).collect();
    assert!(offered.contains(&"transfer_to_explorer"));
    assert!(offered.contains(&"transfer_to_evidence-critic"));
    assert!(!offered.contains(&"transfer_to_executor"));
    Ok(())
}

/// `delegate_wave` bekommt `targets[].role` als Enum der sichtbaren Ziele.
#[test]
fn delegate_wave_role_becomes_an_enum_of_visible_targets() -> TestResult {
    let role = JsonSchema {
        schema_type: Some(JsonSchemaType::String),
        ..JsonSchema::default()
    };
    let item = closed_object(BTreeMap::from([("role".to_owned(), role)]), &["role"]);
    let targets = JsonSchema {
        schema_type: Some(JsonSchemaType::Array),
        items: Some(Box::new(item)),
        ..JsonSchema::default()
    };
    let mut tools = vec![ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(DELEGATE_WAVE_TOOL_NAME),
        description: String::new(),
        parameters: closed_object(BTreeMap::from([("targets".to_owned(), targets)]), &[]),
        strict: false,
    })];
    restrict_delegate_wave_roles(&mut tools, &["explorer".to_owned(), "planner".to_owned()]);
    let ToolSpec::Function(wave) = &tools[0];
    let enum_values = wave
        .parameters
        .properties
        .as_ref()
        .and_then(|properties| properties.get("targets"))
        .and_then(|targets| targets.items.as_deref())
        .and_then(|item| item.properties.as_ref())
        .and_then(|properties| properties.get("role"))
        .and_then(|role| role.enum_values.clone());
    assert_eq!(
        enum_values,
        Some(vec![
            serde_json::json!("explorer"),
            serde_json::json!("planner")
        ])
    );
    Ok(())
}

/// Das Werkzeugschema vor Plan R9 (Kopie des alten `handoff_tool_spec`),
/// nur für den Größenvergleich.
fn legacy_handoff_tool_spec(role: &str) -> ToolSpec {
    let string_property = |description: &str| JsonSchema {
        schema_type: Some(JsonSchemaType::String),
        description: Some(description.to_owned()),
        ..JsonSchema::default()
    };
    let mut properties = BTreeMap::new();
    properties.insert(
        "task".to_owned(),
        string_property("Konkreter, eigenständig verständlicher Arbeitsauftrag."),
    );
    properties.insert(
        "context".to_owned(),
        string_property("Optionaler Zusatzkontext (Fakten, Pfade, Randbedingungen)."),
    );
    properties.insert(
        "continue_from".to_owned(),
        string_property(
            "Optional: ID eines eigenen, beendeten Kindes dieser Rolle; das neue Kind setzt \
                 mit dessen Übergabe und frischem Budget fort (höchstens 3 Fortsetzungen).",
        ),
    );
    properties.insert(
        crate::user_approval::USER_APPROVED_FIELD.to_owned(),
        crate::user_approval::user_approved_schema(),
    );
    properties.insert(
        "background".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::Boolean),
            description: Some(
                "Optional: im Hintergrund laufen lassen (nur Orchestratoren der UIA in der \
                     TUI; Vorgabe dort true): sofortige Rückkehr mit {child_id, status, hint}, \
                     das Ergebnis kommt als Benachrichtigung. false erzwingt synchrones Warten."
                    .to_owned(),
            ),
            ..JsonSchema::default()
        },
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(format!("{HANDOFF_PREFIX}{role}")),
        description: format!(
            "Delegiert eine Aufgabe an den Unteragenten '{role}' und wartet auf sein \
                 Ergebnis (ein Orchestrator der UIA läuft in der TUI im Hintergrund).{}",
            handoff_role_hint(role)
        ),
        parameters: closed_object(properties, &["task"]),
        strict: false,
    })
}

fn schema_bytes(tools: &[ToolSpec]) -> TestResult<usize> {
    serde_json::to_vec(tools)
        .map(|bytes| bytes.len())
        .map_err(|error| TestError::Unexpected(error.to_string()))
}

/// Plan R9, Teil C, Prompt-Messung: Bytes der Delegationswerkzeuge vorher
/// (je Ziel ein altes `transfer_to_*`, nur eingebaute Rollen) und nachher
/// (neuer Zuschnitt, eingebaute plus benutzerdefinierte Ziele) für UIA,
/// Root-Orchestrator und einen Child-Orchestrator. Der Root-Orchestrator
/// muss schrumpfen, obwohl er jetzt mehr Ziele sieht.
#[test]
#[allow(clippy::print_stderr)]
fn delegation_tool_schema_bytes_before_and_after() -> TestResult {
    let session = make_session(
        StubToolProvider::with_names(&[]),
        SessionActivation::new(ToolProfile::Minimal),
    );
    let description = "Erledigt einen klar umrissenen Auftrag und liefert ein belegtes \
                           Ergebnis an den Auftraggeber zurück.";
    let uia = [
        "agent-steward",
        "matrix-game-master",
        "root-orchestrator",
        "uia-explorer",
        "uia-latex-writer",
        "uia-shell-worker",
        "uia-worker",
        "uia-writer",
    ];
    // 21 eingebaute Ziele des Root-Orchestrators (vorher), dazu die rund
    // 31 benutzerdefinierten Agenten des Rosters (nachher).
    let root_builtin: Vec<String> = (0..21)
        .map(|index| format!("builtin-role-{index:02}"))
        .collect();
    let root_custom: Vec<String> = (0..31)
        .map(|index| format!("custom-agent-{index:02}"))
        .collect();
    let child = [
        "evidence-collector",
        "evidence-critic",
        "method-auditor",
        "pattern-analyst",
        "synthesis-writer",
        "systems-modeller",
    ];
    let infos = |names: &[String]| -> Vec<DelegationTargetInfo> {
        names
            .iter()
            .map(|name| DelegationTargetInfo {
                description: Some(description.to_owned()),
                ..DelegationTargetInfo::named(name.clone())
            })
            .collect()
    };
    let before = |names: &[String]| -> TestResult<usize> {
        schema_bytes(
            &names
                .iter()
                .map(|name| legacy_handoff_tool_spec(name))
                .collect::<Vec<_>>(),
        )
    };
    let after = |names: &[String]| -> TestResult<usize> {
        let mut tools = Vec::new();
        append_handoff_tools(&session, &mut tools, &infos(names));
        schema_bytes(&tools)
    };
    let owned = |names: &[&str]| -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    };
    let root_all: Vec<String> = root_builtin.iter().chain(&root_custom).cloned().collect();

    let uia_before = before(&owned(&uia))?;
    let uia_after = after(&owned(&uia))?;
    let root_before = before(&root_builtin)?;
    let root_after = after(&root_all)?;
    let child_before = before(&owned(&child))?;
    let child_after = after(&owned(&child))?;
    eprintln!(
        "Delegationswerkzeuge (Bytes JSON-Schema) vorher → nachher: \
             UIA {uia_before} → {uia_after}, Root-Orchestrator {root_before} → {root_after} \
             ({} → {} Ziele), Child-Orchestrator {child_before} → {child_after}",
        root_builtin.len(),
        root_all.len()
    );
    assert!(
        root_after * 4 < root_before,
        "der Root-Orchestrator muss deutlich schrumpfen: {root_before} → {root_after}"
    );
    assert!(
        uia_after <= uia_before + 1024,
        "die UIA darf höchstens um Katalog und Einzeiler wachsen: {uia_before} → {uia_after}"
    );
    assert!(
        child_after <= child_before + 1024,
        "Child-Orchestrator: {child_before} → {child_after}"
    );
    Ok(())
}

#[tokio::test]
async fn test_redacted_thinking_emits_no_reasoning_item_and_does_not_error() -> TestResult {
    // `redacted_thinking`-Blöcke tragen keinen lesbaren Text — die
    // Extraktion muss robust `None` liefern statt zu paniken, auch für
    // eine UIA-Root-Session.
    let provider = StubToolProvider::with_names(&[]);
    let mut session = uia_root_session(provider)?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![crate::model::ModelResponse {
        reasoning: Some(anthropic_reasoning(vec![serde_json::json!({
            "type": "redacted_thinking",
            "data": "verschlüsselter-blob",
        })])),
        ..crate::model::ModelResponse::text("Antwort")
    }]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("frage"))
        .await
        .map_err(ctx(
            "ein redacted_thinking-Block darf den Turn nicht scheitern lassen",
        ))?;
    assert!(matches!(outcome, TurnOutcome::Completed));

    assert!(
        !session
            .history()
            .items()
            .iter()
            .any(|item| matches!(item, TurnItem::Reasoning(_))),
        "redacted_thinking liefert keinen extrahierbaren Text ⇒ kein Reasoning-Item"
    );
    Ok(())
}

#[tokio::test]
async fn test_no_reasoning_data_leaves_behavior_unchanged() -> TestResult {
    // `response.reasoning == None` (kein Extended Thinking) — auch für
    // eine UIA-Root-Session darf sich am bisherigen Verhalten nichts
    // ändern: nur die AssistantMessage landet in der History.
    let provider = StubToolProvider::with_names(&[]);
    let mut session = uia_root_session(provider)?;
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![crate::model::ModelResponse::text(
        "Antwort ohne Denken",
    )]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("frage"))
        .await
        .map_err(ctx("der Turn läuft durch"))?;
    assert!(matches!(outcome, TurnOutcome::Completed));

    let items = session.history().items();
    assert!(
        !items
            .iter()
            .any(|item| matches!(item, TurnItem::Reasoning(_))),
        "ohne Reasoning-Daten darf kein Reasoning-Item entstehen"
    );
    assert!(
        items
            .iter()
            .any(|item| matches!(item, TurnItem::AssistantMessage(_))),
        "die AssistantMessage muss weiterhin in der History stehen"
    );
    Ok(())
}

#[test]
fn test_extract_thinking_text_joins_multiple_thinking_blocks() {
    let reasoning = anthropic_reasoning(vec![
        serde_json::json!({"type": "thinking", "thinking": "erster Gedanke"}),
        serde_json::json!({"type": "thinking", "thinking": "zweiter Gedanke"}),
    ]);
    assert_eq!(
        extract_thinking_text(&reasoning),
        Some("erster Gedanke\nzweiter Gedanke".to_owned())
    );
}

#[test]
fn test_extract_thinking_text_skips_redacted_blocks() {
    let reasoning = anthropic_reasoning(vec![serde_json::json!({
        "type": "redacted_thinking",
        "data": "opak",
    })]);
    assert_eq!(extract_thinking_text(&reasoning), None);
}

#[test]
fn test_extract_thinking_text_handles_missing_field_without_panicking() {
    // Ein `thinking`-Block ohne das `thinking`-Textfeld (z. B. ein
    // zukünftiges Provider-Format) darf nicht paniken — nur ausgelassen
    // werden.
    let reasoning = anthropic_reasoning(vec![serde_json::json!({"type": "thinking"})]);
    assert_eq!(extract_thinking_text(&reasoning), None);
}

#[test]
fn test_extract_thinking_text_none_for_empty_blocks() {
    let reasoning = anthropic_reasoning(vec![]);
    assert_eq!(extract_thinking_text(&reasoning), None);
}

#[test]
fn test_extract_thinking_text_joins_reasoning_summary_text_entries() {
    // OpenAI-Responses-Transport: ein "reasoning"-Block trägt mehrere
    // summary_text-Einträge, die verkettet werden müssen.
    let reasoning = anthropic_reasoning(vec![serde_json::json!({
        "type": "reasoning",
        "summary": [
            {"type": "summary_text", "text": "erster Abschnitt"},
            {"type": "summary_text", "text": "zweiter Abschnitt"},
        ],
    })]);
    assert_eq!(
        extract_thinking_text(&reasoning),
        Some("erster Abschnitt\nzweiter Abschnitt".to_owned())
    );
}

#[test]
fn test_extract_thinking_text_takes_reasoning_content_text() {
    // Chat-Transport (DeepSeek/Kimi/GLM-Konvention): Feld "text" wird
    // direkt übernommen.
    let reasoning = anthropic_reasoning(vec![serde_json::json!({
        "type": "reasoning_content",
        "text": "Gedankengang aus reasoning_content",
    })]);
    assert_eq!(
        extract_thinking_text(&reasoning),
        Some("Gedankengang aus reasoning_content".to_owned())
    );
}

#[test]
fn test_extract_thinking_text_skips_unknown_block_type() {
    // Ein Block, der weder "thinking" noch "reasoning" noch
    // "reasoning_content" ist, darf weiterhin kein Item/keinen Fehler
    // erzeugen — nur stillschweigend übersprungen werden.
    let reasoning = anthropic_reasoning(vec![serde_json::json!({
        "type": "some_future_block_type",
        "text": "sollte ignoriert werden",
    })]);
    assert_eq!(extract_thinking_text(&reasoning), None);
}

#[test]
fn test_extract_thinking_text_none_for_empty_reasoning_summary_array() {
    // Ein leeres summary-Array darf nicht als „gefunden" zählen — kein
    // leerer String, sondern `None`.
    let reasoning = anthropic_reasoning(vec![serde_json::json!({
        "type": "reasoning",
        "summary": [],
    })]);
    assert_eq!(extract_thinking_text(&reasoning), None);
}

// ------------------------------------------------------------------
// Welle 3: Kontextfenster-Wächter (Pre-flight, ContextLength-Recovery,
// MaxTokens-Wiederholung)
// ------------------------------------------------------------------

/// Modell-Double für die Fenster-Wächter: Turn-Requests (erkennbar an
/// mitgelieferten Tools) werden protokolliert und aus dem Skript
/// bedient (leer ⇒ Text „fertig"); Requests ohne Tools sind
/// Zusammenfassungs-Aufrufe der Verdichtung und bekommen immer eine
/// kurze Zusammenfassung, ohne das Skript zu verbrauchen.
struct GuardModel {
    script: Mutex<
        std::collections::VecDeque<
            Result<crate::model::ModelResponse, crate::model::ModelError>,
        >,
    >,
    /// Je Turn-Request: geschätzte Bytes und gesetztes Ausgabelimit.
    requests: Mutex<Vec<(u64, Option<u32>)>>,
    /// Je Turn-Request: die Debug-Darstellung (für Anweisungs-Prüfungen).
    rendered: Mutex<Vec<String>>,
}

impl GuardModel {
    fn new(script: Vec<Result<crate::model::ModelResponse, crate::model::ModelError>>) -> Self {
        Self {
            script: Mutex::new(script.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
            rendered: Mutex::new(Vec::new()),
        }
    }

    fn rendered(&self) -> Vec<String> {
        self.rendered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn requests(&self) -> Vec<(u64, Option<u32>)> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl ModelProvider for GuardModel {
    fn respond<'a>(&'a self, request: ModelRequest) -> crate::model::ModelFuture<'a> {
        if request.tools.is_empty() {
            return Box::pin(async {
                Ok(crate::model::ModelResponse::text(
                    "Zusammenfassung: ältere Runden lasen große Dateien.",
                ))
            });
        }
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((
                crate::context_budget::estimate_request_bytes(&request),
                request.max_output_tokens,
            ));
        self.rendered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(format!("{request:?}"));
        let next = self
            .script
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
            .unwrap_or_else(|| Ok(crate::model::ModelResponse::text("fertig")));
        Box::pin(async move { next })
    }
}

/// Session mit dem Tool `lookup` (damit Turn-Requests Tools tragen),
/// optionaler Auto-Compact-Policy und zwölf älteren Runden mit je einem
/// 20-KB-Tool-Ergebnis (rund 60–80 k Tokens, noch innerhalb des
/// 256-KiB-Verlaufsbudgets des Requests) — genug Masse, die eine Verdichtung sicher
/// verkleinert.
fn bulky_session(policy: Option<crate::auto_compact::AutoCompactPolicy>) -> AgentSession {
    let mut session = make_session(
        StubToolProvider::with_names(&["lookup"]),
        SessionActivation::new(ToolProfile::Full),
    )
    .with_auto_compact(policy);
    for i in 0..12 {
        let call_id = ToolCallId::new();
        session.history_mut().push_user_text(format!("frage {i}"));
        session.history_mut().push_tool_call(
            call_id.clone(),
            "lookup",
            serde_json::json!({ "i": i }),
        );
        session.history_mut().push_tool_result(
            call_id,
            ToolCallResult::success(serde_json::json!("x".repeat(20_000))),
            0,
        );
        session
            .history_mut()
            .push_assistant_text(format!("antwort {i}"), None);
    }
    session
}

#[tokio::test]
async fn context_length_error_triggers_one_emergency_compaction_and_a_retry() -> TestResult {
    let policy = crate::auto_compact::AutoCompactPolicy::for_context_window(200_000);
    let mut session = bulky_session(Some(policy));
    let store = crate::state_store::InMemoryStateStore::new();
    let model = GuardModel::new(vec![
        Err(crate::model::ModelError::ContextLength {
            message: "prompt is too long".to_owned(),
        }),
        Ok(crate::model::ModelResponse::text(
            "nach Verdichtung erledigt",
        )),
    ]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("weiter"))
        .await
        .map_err(ctx("ContextLength muss einmal abgefangen werden"))?;

    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    let requests = model.requests();
    assert_eq!(
        requests.len(),
        2,
        "genau ein fehlgeschlagener Request plus eine Wiederholung"
    );
    assert!(
        requests[1].0 < requests[0].0,
        "die Wiederholung muss nach der Notfall-Verdichtung kleiner sein \
             (vorher={}, nachher={})",
        requests[0].0,
        requests[1].0
    );
    Ok(())
}

#[tokio::test]
async fn a_second_context_length_error_is_returned_to_the_caller() -> TestResult {
    let policy = crate::auto_compact::AutoCompactPolicy::for_context_window(200_000);
    let mut session = bulky_session(Some(policy));
    let store = crate::state_store::InMemoryStateStore::new();
    let model = GuardModel::new(vec![
        Err(crate::model::ModelError::ContextLength {
            message: "prompt is too long".to_owned(),
        }),
        Err(crate::model::ModelError::ContextLength {
            message: "prompt is still too long".to_owned(),
        }),
    ]);

    let result = run_turn(&mut session, &model, &store, TurnInput::user("weiter")).await;

    assert!(
        matches!(
            result,
            Err(CoreError::Model(
                crate::model::ModelError::ContextLength { .. }
            ))
        ),
        "der zweite Fehlschlag geht an den Aufrufer: {result:?}"
    );
    assert_eq!(model.requests().len(), 2, "höchstens eine Wiederholung");
    Ok(())
}

#[tokio::test]
async fn pending_compaction_runs_an_emergency_compaction_before_the_first_request() -> TestResult
{
    let policy = crate::auto_compact::AutoCompactPolicy::for_context_window(100_000);
    let mut session = bulky_session(Some(policy));
    session.set_pending_compaction(true);
    let bytes_before: u64 = session
        .history()
        .items()
        .iter()
        .map(|item| serde_json::to_vec(item).map_or(0, |v| v.len() as u64))
        .sum();
    let store = crate::state_store::InMemoryStateStore::new();
    let model = GuardModel::new(vec![Ok(crate::model::ModelResponse::text("ok"))]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("weiter"))
        .await
        .map_err(ctx("der Turn läuft nach der Notfall-Verdichtung durch"))?;

    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    assert!(
        !session.pending_compaction(),
        "die vorgemerkte Verdichtung ist nach dem Lauf erledigt"
    );
    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].0 < bytes_before / 2,
        "schon der erste Request muss verdichtet sein (Request={}, Verlauf vorher={})",
        requests[0].0,
        bytes_before
    );
    Ok(())
}

#[tokio::test]
async fn a_request_that_cannot_fit_ends_with_context_exhausted_before_sending() -> TestResult {
    // 1 000-Token-Fenster, aber eine einzelne Nutzernachricht von rund
    // 3 000 Tokens: die aktuelle Nachricht lässt sich nicht verdichten.
    let policy = crate::auto_compact::AutoCompactPolicy::for_context_window(1_000);
    let mut session = make_session(
        StubToolProvider::with_names(&["lookup"]),
        SessionActivation::new(ToolProfile::Full),
    )
    .with_auto_compact(Some(policy));
    let store = crate::state_store::InMemoryStateStore::new();
    let model = GuardModel::new(Vec::new());

    let result = run_turn(
        &mut session,
        &model,
        &store,
        TurnInput::user("y".repeat(9_000)),
    )
    .await;

    match result {
        Err(CoreError::Model(crate::model::ModelError::ContextLength { message })) => {
            assert!(
                message.contains("Kontext erschöpft"),
                "klare deutsche Meldung erwartet: {message}"
            );
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "erwartet wurde „Kontext erschöpft\", war: {other:?}"
            )));
        }
    }
    assert!(
        model.requests().is_empty(),
        "ein Request, der nicht passt, darf nie gesendet werden"
    );
    Ok(())
}

#[tokio::test]
async fn interrupted_stream_preserves_partial_text_without_retrying_or_running_tools()
-> TestResult {
    const PARTIAL: &str = "Abgebrochener Antwortanfang";
    let mut session = make_session(
        StubToolProvider::with_names(&["lookup"]),
        SessionActivation::new(ToolProfile::Full),
    );
    let store = crate::state_store::InMemoryStateStore::new();
    let model = GuardModel::new(vec![
        Ok(crate::model::ModelResponse {
            message: Some(PARTIAL.to_owned()),
            stop: crate::model::StopReason::StreamInterrupted,
            tool_calls: vec![call(&ToolCallId::new(), "lookup")],
            ..crate::model::ModelResponse::default()
        }),
        Ok(crate::model::ModelResponse::text("unexpected retry")),
    ]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("los"))
        .await
        .map_err(ctx("partial response goes through durable completion"))?;

    assert!(matches!(outcome, TurnOutcome::Truncated), "{outcome:?}");
    assert_eq!(model.requests().len(), 1);
    let durable = store
        .load_history(session.id())
        .await
        .map_err(ctx("persist partial response and interruption notice"))?;
    assert!(durable.items().iter().any(|item| matches!(
        item, TurnItem::AssistantMessage(message)
            if message.content.iter().any(|part| matches!(part, ContentPart::Text { text } if text == PARTIAL))
    )));
    assert!(
        !durable
            .items()
            .iter()
            .any(|item| matches!(item, TurnItem::ToolCall(_) | TurnItem::ToolResult(_)))
    );
    assert!(matches!(durable.last(), Some(TurnItem::Error(error))
        if error.message.contains("Antwortstream") && !error.message.contains("limit") && !error.retryable));
    Ok(())
}

#[tokio::test]
async fn max_tokens_with_tool_calls_retries_once_with_a_doubled_output_limit() -> TestResult {
    let policy = crate::auto_compact::AutoCompactPolicy::for_context_window(200_000);
    let mut session = make_session(
        StubToolProvider::with_names(&["lookup"]),
        SessionActivation::new(ToolProfile::Full),
    )
    .with_auto_compact(Some(policy));
    session.set_max_output_tokens(Some(1_000));
    let store = crate::state_store::InMemoryStateStore::new();
    let truncated_call = ToolCallId::new();
    let model = GuardModel::new(vec![
        Ok(crate::model::ModelResponse {
            stop: crate::model::StopReason::MaxTokens,
            ..response_with(vec![call(&truncated_call, "lookup")])
        }),
        Ok(crate::model::ModelResponse::text("mit mehr Platz erledigt")),
    ]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("los"))
        .await
        .map_err(ctx("die Wiederholung mit doppeltem Limit läuft durch"))?;

    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    let limits: Vec<Option<u32>> = model.requests().into_iter().map(|(_, l)| l).collect();
    assert_eq!(limits, vec![Some(1_000), Some(2_000)]);
    assert!(
        !session
            .history()
            .items()
            .iter()
            .any(|item| matches!(item, TurnItem::ToolCall(_))),
        "abgeschnittene Tool-Calls dürfen nie in den Verlauf gelangen"
    );
    Ok(())
}

/// Ein eindeutiger `lookup`-Aufruf (andere Argumente je `part`), damit
/// der Schleifen-Wächter wiederholte Runden nicht als Stillstand wertet.
fn distinct_lookup(part: usize) -> crate::model::ModelResponse {
    response_with(vec![ToolCall {
        id: ToolCallId::new(),
        name: ToolName::new("lookup"),
        arguments: serde_json::json!({"part": part}),
    }])
}

fn truncated_text(text: &str) -> crate::model::ModelResponse {
    crate::model::ModelResponse {
        stop: crate::model::StopReason::MaxTokens,
        ..crate::model::ModelResponse::text(text)
    }
}

#[test]
fn unknown_spawn_rejections_get_a_stable_code_and_known_ones_keep_their_text() {
    let unknown =
        harw_extension_api::AgentSpawnError::new("child role 'ghost' is not registered");
    let text = spawn_rejection_text("ghost", &unknown);
    assert!(text.starts_with("[HARW-ORCH-001]"), "{text}");
    assert!(text.contains("not registered"));
    let continuation = harw_extension_api::AgentSpawnError::new(
        "continue_from: kein eigenes, beendetes Kind mit der ID x",
    );
    assert_eq!(
        spawn_rejection_text("explorer", &continuation),
        continuation.message
    );
}

const TEXT_CONTINUATION_MARK: &str = "Setze exakt an ihrer letzten Stelle fort";
const SPLIT_MARK: &str = "NICHT ausgeführt";

#[tokio::test]
async fn the_continuation_instruction_is_sent_only_right_after_a_truncation() -> TestResult {
    let mut session = make_session(
        StubToolProvider::with_names(&["lookup"]),
        SessionActivation::new(ToolProfile::Full),
    );
    let store = crate::state_store::InMemoryStateStore::new();
    let model = GuardModel::new(vec![
        Ok(truncated_text("Teil eins")),
        Ok(distinct_lookup(1)),
        Ok(distinct_lookup(2)),
        Ok(crate::model::ModelResponse::text("fertig")),
    ]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("los"))
        .await
        .map_err(ctx("der Turn läuft durch"))?;

    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    let marks: Vec<bool> = model
        .rendered()
        .iter()
        .map(|request| request.contains(TEXT_CONTINUATION_MARK))
        .collect();
    assert_eq!(marks, vec![false, true, false, false]);
    Ok(())
}

#[tokio::test]
async fn separate_truncations_in_a_long_turn_do_not_end_it() -> TestResult {
    let mut session = make_session(
        StubToolProvider::with_names(&["lookup"]),
        SessionActivation::new(ToolProfile::Full),
    );
    let store = crate::state_store::InMemoryStateStore::new();
    let mut script = Vec::new();
    for part in 0..5 {
        script.push(Ok(truncated_text("Zwischenstand")));
        script.push(Ok(distinct_lookup(part)));
    }
    script.push(Ok(crate::model::ModelResponse::text("fertig")));
    let model = GuardModel::new(script);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("los"))
        .await
        .map_err(ctx("der lange Turn läuft durch"))?;

    assert!(
        matches!(outcome, TurnOutcome::Completed),
        "fünf getrennte Abschneidungen sind kein Grund für Truncated: {outcome:?}"
    );
    assert_eq!(model.requests().len(), 11);
    Ok(())
}

#[tokio::test]
async fn provider_stop_notice_distinguishes_filter_and_refusal() -> TestResult {
    for (stop, expected) in [
        (
            crate::model::StopReason::ContentFilter,
            "Inhaltsfilter des Anbieters",
        ),
        (
            crate::model::StopReason::Refusal {
                detail: Some("Provider-Prüfung".into()),
            },
            "Grund laut Anbieter: Provider-Prüfung",
        ),
    ] {
        let mut session = make_session(
            StubToolProvider::with_names(&["lookup"]),
            SessionActivation::new(ToolProfile::Full),
        );
        let store = crate::state_store::InMemoryStateStore::new();
        let model = GuardModel::new(vec![Ok(crate::model::ModelResponse {
            stop,
            ..crate::model::ModelResponse::text("Teilantwort")
        })]);
        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("los"))
            .await
            .map_err(ctx("provider stop"))?;
        assert!(matches!(outcome, TurnOutcome::Refused { .. }));
        assert_eq!(model.requests().len(), 1, "no automatic retry");
        let history = store
            .load_history(session.id())
            .await
            .map_err(ctx("load notice"))?;
        assert!(matches!(
            history.last(),
            Some(TurnItem::Error(error)) if !error.retryable && error.message.contains(expected)
        ));
    }
    Ok(())
}

#[tokio::test]
async fn consecutive_truncations_still_stop_after_the_limit() -> TestResult {
    let mut session = make_session(
        StubToolProvider::with_names(&["lookup"]),
        SessionActivation::new(ToolProfile::Full),
    );
    let store = crate::state_store::InMemoryStateStore::new();
    let model = GuardModel::new((0..6).map(|_| Ok(truncated_text("…"))).collect());

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("los"))
        .await
        .map_err(ctx("der Turn endet ehrlich"))?;

    assert!(matches!(outcome, TurnOutcome::Truncated), "{outcome:?}");
    assert_eq!(model.requests().len(), 4, "1 + 3 Fortsetzungen");
    let durable = store
        .load_history(session.id())
        .await
        .map_err(ctx("truncation stays visible after replay"))?;
    assert!(
        durable
            .items()
            .iter()
            .any(|item| matches!(item, TurnItem::AssistantMessage(_)))
    );
    assert_eq!(
        durable
            .items()
            .iter()
            .filter(|item| matches!(item, TurnItem::Error(error) if error.message.starts_with("Antwort unvollständig")))
            .count(),
        1,
    );
    assert!(matches!(durable.last(), Some(TurnItem::Error(error)) if error.retryable));
    Ok(())
}

#[tokio::test]
async fn oversized_tool_calls_after_the_retry_ask_for_smaller_calls() -> TestResult {
    let policy = crate::auto_compact::AutoCompactPolicy::for_context_window(200_000);
    let mut session = make_session(
        StubToolProvider::with_names(&["lookup"]),
        SessionActivation::new(ToolProfile::Full),
    )
    .with_auto_compact(Some(policy));
    session.set_max_output_tokens(Some(1_000));
    let store = crate::state_store::InMemoryStateStore::new();
    let truncated = || {
        Ok(crate::model::ModelResponse {
            stop: crate::model::StopReason::MaxTokens,
            ..distinct_lookup(9)
        })
    };
    let model = GuardModel::new(vec![
        truncated(),
        truncated(),
        Ok(distinct_lookup(1)),
        Ok(crate::model::ModelResponse::text("kleiner erledigt")),
    ]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("los"))
        .await
        .map_err(ctx("der Turn läuft mit kleineren Aufrufen weiter"))?;

    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    let rendered = model.rendered();
    assert_eq!(rendered.len(), 4);
    assert!(!rendered[1].contains(SPLIT_MARK));
    assert!(rendered[2].contains(SPLIT_MARK));
    assert!(!rendered[3].contains(SPLIT_MARK));
    let executed = session
        .history()
        .items()
        .iter()
        .filter(|item| matches!(item, TurnItem::ToolCall(_)))
        .count();
    assert_eq!(executed, 1, "nur der kleinere Aufruf wird ausgeführt");
    Ok(())
}

#[test]
fn raised_output_limit_is_capped_by_reserve_and_half_window() {
    let budget = RoundBudget {
        window_tokens: 10_000,
        reserve_tokens: 4_000,
        threshold_tokens: None,
    };
    assert_eq!(budget.raised_output_limit(1_000), 2_000);
    assert_eq!(
        budget.raised_output_limit(3_000),
        5_000,
        "Fenster/2 deckelt"
    );
    let small_reserve = RoundBudget {
        window_tokens: 100_000,
        reserve_tokens: 1_500,
        threshold_tokens: None,
    };
    assert_eq!(small_reserve.raised_output_limit(1_000), 2_000);
    assert_eq!(
        small_reserve.raised_output_limit(2_000),
        3_000,
        "2 × Reserve deckelt"
    );
    assert!(budget.overflows(6_001));
    assert!(!budget.overflows(6_000));
    assert_eq!(budget.emergency_target(60), 3_600);
}

// --- Runde 7, Teile A2/A5: sitzungsweite Wächter ---------------------

/// Sitzung mit sechs identischen `fs.read`-Aufrufen in einer Antwort;
/// `parent` entscheidet, ob es eine Kind-Sitzung ist.
async fn run_six_reads(
    parent: Option<harw_types::SessionId>,
    organizational_role: harw_agent_dsl::roles::AgentRoleId,
) -> TestResult<(usize, Vec<ToolCallResult>)> {
    run_reads(6, parent, organizational_role).await
}

/// Sitzung mit `count` verschiedenen `fs.read`-Aufrufen in einer Antwort.
async fn run_reads(
    count: usize,
    parent: Option<harw_types::SessionId>,
    organizational_role: harw_agent_dsl::roles::AgentRoleId,
) -> TestResult<(usize, Vec<ToolCallResult>)> {
    let executions = Arc::new(AtomicUsize::new(0));
    let provider = StubParallelProvider::instant(&["fs.read"], Arc::clone(&executions));
    let (tx, _rx) = mpsc::unbounded_channel();
    let registry = ExtensionRegistryBuilder::default()
        .tool_provider(provider)
        .build();
    let mut context = test_spawn_context()?;
    context.organizational_role = organizational_role;
    let mut session = AgentSession::new(AgentRole::Assistant, parent, registry, tx)
        .with_spawn_context(context);
    let store = crate::state_store::InMemoryStateStore::new();
    let calls: Vec<ToolCall> = (0..count)
        .map(|index| ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("fs.read"),
            arguments: serde_json::json!({ "path": format!("src/f{index}.rs") }),
        })
        .collect();
    let model = ScriptedModel::new(vec![
        response_with(calls),
        crate::model::ModelResponse::text("fertig"),
    ]);
    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("überblick"))
        .await
        .map_err(ctx("der Turn läuft durch"))?;
    if !matches!(outcome, TurnOutcome::Completed) {
        return Err(TestError::Unexpected(format!("outcome {outcome:?}")));
    }
    let results = session
        .history()
        .items()
        .iter()
        .filter_map(|item| match item {
            TurnItem::ToolResult(result) => Some(result.result.clone()),
            _ => None,
        })
        .collect();
    Ok((executions.load(Ordering::SeqCst), results))
}

#[tokio::test]
async fn the_thirteenth_read_of_an_orchestrator_child_is_denied() -> TestResult {
    let (executed, results) = run_reads(
        13,
        Some(harw_types::SessionId::new()),
        harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
    )
    .await?;
    assert_eq!(executed, 12, "only twelve own reads may run");
    assert_eq!(results.len(), 13);
    let Some(ToolCallResult::Error { message }) = results.last() else {
        return Err(TestError::Unexpected(format!(
            "the thirteenth read must be an error: {results:?}"
        )));
    };
    assert!(message.contains("Lesebudget erschöpft"), "{message}");
    assert!(message.contains("delegate_wave"), "{message}");
    let Some(ToolCallResult::Success { value }) = results.get(9) else {
        return Err(TestError::Unexpected("the tenth read runs".to_owned()));
    };
    assert!(
        value.to_string().contains("Lesezugriff 10 von 12"),
        "the tenth read carries a warning: {value}"
    );
    Ok(())
}

#[tokio::test]
async fn non_orchestrators_and_root_sessions_read_without_a_budget() -> TestResult {
    let (worker, _) = run_six_reads(
        Some(harw_types::SessionId::new()),
        harw_agent_dsl::roles::AgentRoleId::Worker,
    )
    .await?;
    assert_eq!(worker, 6, "a worker child reads freely");
    let (root, _) =
        run_six_reads(None, harw_agent_dsl::roles::AgentRoleId::RootOrchestrator).await?;
    assert_eq!(root, 6, "a root session without parent reads freely");
    Ok(())
}

#[tokio::test]
async fn repeated_agent_status_on_the_same_child_carries_a_polling_hint() -> TestResult {
    let executions = Arc::new(AtomicUsize::new(0));
    let provider = StubParallelProvider::instant(&["agent.status"], Arc::clone(&executions));
    let mut session =
        guarded_session(provider, Arc::new(CountingApproval::allow_everything()))?;
    let store = crate::state_store::InMemoryStateStore::new();
    let status_call = || ToolCall {
        id: ToolCallId::new(),
        name: ToolName::new("agent.status"),
        arguments: serde_json::json!({ "child_id": "kind-1" }),
    };
    let model = ScriptedModel::new(vec![
        response_with(vec![status_call()]),
        response_with(vec![status_call()]),
        crate::model::ModelResponse::text("warte"),
    ]);
    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("status"))
        .await
        .map_err(ctx("der Turn läuft durch"))?;
    assert!(matches!(outcome, TurnOutcome::Completed));
    let results: Vec<String> = session
        .history()
        .items()
        .iter()
        .filter_map(|item| match item {
            TurnItem::ToolResult(result) => Some(format!("{:?}", result.result)),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 2);
    assert!(!results[0].contains("Benachrichtigung"), "{}", results[0]);
    assert!(results[1].contains("Benachrichtigung"), "{}", results[1]);
    assert_eq!(
        executions.load(Ordering::SeqCst),
        2,
        "polling is not blocked"
    );
    Ok(())
}

// --- Runde 7, Teil B5: Pitfall-Hinweis nur am gleichen Werkzeug -------

/// Berater, der nur für `plan.update` einen Pitfall meldet.
struct PlanPitfall;

impl crate::guard::PitfallAdvisor for PlanPitfall {
    fn advise(&self, tool_name: &str, _arguments: &serde_json::Value) -> Option<String> {
        (tool_name == "plan.update")
            .then(|| "PLAN-PITFALL: Plan nicht überschreiben".to_owned())
    }
}

async fn pitfall_turn(
    policy: crate::guard::GuardPolicy,
    responses: Vec<crate::model::ModelResponse>,
) -> TestResult<Vec<(String, String)>> {
    let executions = Arc::new(AtomicUsize::new(0));
    let provider = StubParallelProvider::with_serial_tool(
        &["plan.update", "goal.set"],
        "plan.update",
        Arc::clone(&executions),
    );
    let mut session =
        guarded_session(provider, Arc::new(CountingApproval::allow_everything()))?
            .with_guard_policy(Some(policy))
            .with_pitfall_advisor(Some(
                Arc::new(PlanPitfall) as Arc<dyn crate::guard::PitfallAdvisor>
            ));
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(responses);
    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("plane"))
        .await
        .map_err(ctx("der Turn läuft durch"))?;
    if !matches!(outcome, TurnOutcome::Completed) {
        return Err(TestError::Unexpected(format!("outcome {outcome:?}")));
    }
    let mut names = std::collections::HashMap::new();
    let mut results = Vec::new();
    for item in session.history().items() {
        match item {
            TurnItem::ToolCall(call) => {
                names.insert(call.call_id.clone(), call.tool_name.clone());
            }
            TurnItem::ToolResult(result) => results.push((
                names.get(&result.call_id).cloned().unwrap_or_default(),
                format!("{:?}", result.result),
            )),
            _ => {}
        }
    }
    Ok(results)
}

fn assert_pitfall_only_on_plan(results: &[(String, String)]) -> TestResult {
    let plan = results
        .iter()
        .find(|(name, _)| name == "plan.update")
        .ok_or(TestError::Missing("plan.update-Ergebnis"))?;
    let goal = results
        .iter()
        .find(|(name, _)| name == "goal.set")
        .ok_or(TestError::Missing("goal.set-Ergebnis"))?;
    assert!(plan.1.contains("PLAN-PITFALL"), "{}", plan.1);
    assert!(!goal.1.contains("PLAN-PITFALL"), "{}", goal.1);
    Ok(())
}

#[tokio::test]
async fn a_pitfall_hint_stays_on_the_same_tool_in_one_response() -> TestResult {
    let results = pitfall_turn(
        crate::guard::GuardPolicy::default(),
        vec![
            response_with(vec![
                call(&ToolCallId::new(), "plan.update"),
                call(&ToolCallId::new(), "goal.set"),
            ]),
            crate::model::ModelResponse::text("fertig"),
        ],
    )
    .await?;
    assert_pitfall_only_on_plan(&results)
}

#[tokio::test]
async fn a_pitfall_hint_is_attached_even_without_an_active_turn_guard() -> TestResult {
    let results = pitfall_turn(
        crate::guard::GuardPolicy {
            enabled: false,
            ..crate::guard::GuardPolicy::default()
        },
        vec![
            response_with(vec![call(&ToolCallId::new(), "plan.update")]),
            response_with(vec![call(&ToolCallId::new(), "goal.set")]),
            crate::model::ModelResponse::text("fertig"),
        ],
    )
    .await?;
    assert_pitfall_only_on_plan(&results)
}

// --- Runde 7, Teil B3: "null"-Normalisierung --------------------------

#[test]
fn null_strings_are_removed_only_from_optional_top_level_fields() {
    let schema = JsonSchema {
        required: Some(vec!["path".to_owned()]),
        ..JsonSchema::default()
    };
    let mut arguments = serde_json::json!({
        "path": "null",
        "pattern": "null",
        "limit": 5,
        "nested": { "inner": "null" },
        "text": "kein null"
    });
    let removed = strip_null_string_arguments(&mut arguments, &schema);
    assert_eq!(removed, vec!["pattern".to_owned()]);
    assert_eq!(
        arguments,
        serde_json::json!({
            "path": "null",
            "limit": 5,
            "nested": { "inner": "null" },
            "text": "kein null"
        })
    );
}

#[test]
fn normalization_uses_the_tool_schema_and_skips_unknown_tools() {
    let provider = StubToolProvider::with_names(&["fs.glob"]);
    let session = make_session(provider, SessionActivation::new(ToolProfile::Full));
    let mut calls = vec![
        ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("fs.glob"),
            arguments: serde_json::json!({ "pattern": "**/*.rs", "path": "null" }),
        },
        ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("unbekannt"),
            arguments: serde_json::json!({ "path": "null" }),
        },
    ];
    normalize_null_string_arguments(&session, &mut calls);
    assert_eq!(
        calls[0].arguments,
        serde_json::json!({ "pattern": "**/*.rs" })
    );
    assert_eq!(
        calls[1].arguments,
        serde_json::json!({ "path": "null" }),
        "ohne Schema bleibt der Aufruf unverändert"
    );
}

// --- Runde 7, Teil L9: kompakte Werkzeugschemas -----------------------

#[test]
fn compact_tool_specs_shorten_descriptions_but_keep_the_contract() {
    let mut properties = std::collections::BTreeMap::new();
    properties.insert(
        "path".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some("x".repeat(200)),
            ..JsonSchema::default()
        },
    );
    let spec = ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new("fs.read"),
        description: "Liest eine Datei. Weitere lange Erklärungen folgen hier.".to_owned(),
        parameters: JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(properties),
            required: Some(vec!["path".to_owned()]),
            ..JsonSchema::default()
        },
        strict: false,
    });
    let ToolSpec::Function(compact) = compact_tool_spec(spec);
    assert_eq!(compact.description, "Liest eine Datei.");
    assert_eq!(compact.parameters.required, Some(vec!["path".to_owned()]));
    let path = compact
        .parameters
        .properties
        .as_ref()
        .and_then(|properties| properties.get("path"))
        .and_then(|path| path.description.clone())
        .unwrap_or_default();
    assert_eq!(path.chars().count(), COMPACT_PARAMETER_DESCRIPTION_CHARS);
}

#[test]
fn collect_tools_compacts_only_when_the_session_asks_for_it() -> TestResult {
    let provider = StubToolProvider::with_names(&["fs.read"]);
    let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
    let full = collect_tools(&session).map_err(ctx("collect_tools"))?;
    session.set_compact_tool_schemas(true);
    let compact = collect_tools(&session).map_err(ctx("collect_tools kompakt"))?;
    assert_eq!(full.len(), compact.len());
    assert_eq!(
        compact.first().map(|spec| spec.name().to_owned()),
        Some("fs.read".to_owned())
    );
    Ok(())
}

// ------------------------------------------------------------------
// R18 D-D (RP-T6): Platzierung aus den Ausführer-Metadaten
// ------------------------------------------------------------------

/// Liefert JSON wie `shell.exec`; `on_host` setzt das Eskalations-Feld.
struct ShellLikeExecutor {
    on_host: bool,
}

impl ToolExecutor for ShellLikeExecutor {
    fn execute<'a>(
        &'a self,
        _ctx: &'a ToolExecutionContext,
        _call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        let mut content = serde_json::json!({ "exit_code": 0 });
        if self.on_host {
            content[harw_tools::executor::EXECUTED_ON_FIELD] = serde_json::json!("host");
        }
        Box::pin(async move { Ok(ToolOutput::Json { content }) })
    }
}

/// `sandboxed`/`escalated` tragen Sandbox-Metadaten, `unplaced` keine.
struct PlacementProvider {
    parallel: bool,
}

impl ToolProvider for PlacementProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        ["sandboxed", "escalated", "unplaced"]
            .into_iter()
            .map(|name| {
                ToolSpec::Function(FunctionToolSpec {
                    name: ToolName::new(name),
                    description: "placement stub".to_owned(),
                    parameters: JsonSchema::default(),
                    strict: false,
                })
            })
            .collect()
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        let placed = |on_host: bool| -> Arc<dyn ToolExecutor> {
            Arc::new(harw_tools::executor::PlacedToolExecutor::new(
                Arc::new(ShellLikeExecutor { on_host }),
                ExecutionPlacement::Sandbox,
            ))
        };
        match name.as_str() {
            "sandboxed" => Some(placed(false)),
            "escalated" => Some(placed(true)),
            "unplaced" => Some(Arc::new(ShellLikeExecutor { on_host: false })),
            _ => None,
        }
    }

    fn parallel_safe(&self, _name: &ToolName) -> bool {
        self.parallel
    }
}

fn completed_placement(
    events: &[TurnEvent],
    id: &ToolCallId,
) -> TestResult<Option<ToolPlacement>> {
    events
        .iter()
        .find_map(|event| match event {
            TurnEvent::ToolCallCompleted {
                call_id, placement, ..
            } if call_id == id => Some(placement.clone()),
            _ => None,
        })
        .ok_or(TestError::Missing("ToolCallCompleted for the call"))
}

async fn run_placement_turn(parallel: bool) -> TestResult {
    let sandboxed = ToolCallId::new();
    let escalated = ToolCallId::new();
    let unplaced = ToolCallId::new();
    let (turn_tx, mut turn_rx) = mpsc::unbounded_channel();
    let handler = Arc::new(CountingApproval::allow_everything());
    let mut session = guarded_session(Arc::new(PlacementProvider { parallel }), handler)?
        .with_turn_event_sink(turn_tx);
    let store = crate::state_store::InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![
        response_with(vec![
            call(&sandboxed, "sandboxed"),
            call(&escalated, "escalated"),
            call(&unplaced, "unplaced"),
        ]),
        crate::model::ModelResponse::text("fertig"),
    ]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("placement"))
        .await
        .map_err(ctx("der Turn mit drei platzierten Aufrufen läuft durch"))?;

    assert!(matches!(outcome, TurnOutcome::Completed));
    let events = drain_turn_events(&mut turn_rx);
    assert_eq!(
        completed_placement(&events, &sandboxed)?,
        Some(ToolPlacement::Sandbox),
        "parallel={parallel}"
    );
    assert_eq!(
        completed_placement(&events, &escalated)?,
        Some(ToolPlacement::Host),
        "parallel={parallel}"
    );
    assert_eq!(
        completed_placement(&events, &unplaced)?,
        None,
        "without executor metadata the placement stays unknown (parallel={parallel})"
    );
    Ok(())
}

#[tokio::test]
async fn sequential_tool_completion_carries_executor_placement() -> TestResult {
    run_placement_turn(false).await
}

#[tokio::test]
async fn parallel_tool_completion_carries_executor_placement() -> TestResult {
    run_placement_turn(true).await
}

#[test]
fn wire_placement_maps_every_execution_placement() {
    assert_eq!(wire_placement(None), None);
    assert_eq!(
        wire_placement(Some(ExecutionPlacement::Host)),
        Some(ToolPlacement::Host)
    );
    assert_eq!(
        wire_placement(Some(ExecutionPlacement::Sandbox)),
        Some(ToolPlacement::Sandbox)
    );
    assert_eq!(
        wire_placement(Some(ExecutionPlacement::Gateway {
            node: Some("gw-1".to_owned())
        })),
        Some(ToolPlacement::Gateway {
            node: Some("gw-1".to_owned())
        })
    );
}

fn nine_names() -> Vec<String> {
    [
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
    ]
    .iter()
    .map(|name| (*name).to_owned())
    .collect()
}

fn fragment_body(names: &[String], offered: &[String]) -> TestResult<String> {
    Ok(delegation_targets_fragment(names, true, true, offered)
        .ok_or(TestError::Missing("ein Delegationsziel-Fragment"))?
        .body)
}

#[test]
fn large_target_lists_render_offers_without_hiding_delegate_names() -> TestResult {
    let names = nine_names();
    let offered = vec!["charlie".to_owned(), "india".to_owned()];

    let body = fragment_body(&names, &offered)?;

    assert!(body.contains("Empfohlen"), "{body}");
    assert!(body.contains("9 Ziele"), "{body}");
    assert!(body.contains("charlie, india"), "{body}");
    assert!(body.contains("Details: `agents.catalog`."), "{body}");
    for hidden in [
        "alpha", "bravo", "delta", "echo", "foxtrot", "golf", "hotel",
    ] {
        assert!(!body.contains(hidden), "{hidden} steht im Block: {body}");
    }

    // Darstellung, nicht Sichtbarkeit: das Werkzeugschema nennt weiter
    // jeden Namen, also bleibt jedes Ziel aufrufbar.
    let ToolSpec::Function(function) = agents_delegate_tool_spec(&names);
    let enum_values = function
        .parameters
        .properties
        .as_ref()
        .and_then(|properties| properties.get("agent"))
        .and_then(|agent| agent.enum_values.as_ref())
        .ok_or(TestError::Missing("agent-Enum im Delegationsschema"))?;
    let schema_names: Vec<&str> = enum_values.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(
        schema_names,
        names.iter().map(String::as_str).collect::<Vec<_>>()
    );
    Ok(())
}

#[test]
fn small_or_unoffered_target_lists_keep_the_full_name_list() -> TestResult {
    let names = nine_names();
    let offered = vec!["charlie".to_owned()];

    // Höchstens acht Ziele: die Empfehlung bleibt unbeachtet.
    let small: Vec<String> = names.iter().take(8).cloned().collect();
    let body = fragment_body(&small, &offered)?;
    assert_eq!(
        body,
        "Delegierbare Ziele (agents.delegate): alpha, bravo, charlie, delta, echo, \
             foxtrot, golf, hotel. Details: `agents.catalog`."
    );

    // Große Liste ohne Empfehlung: ebenfalls die vollständige Liste.
    let body = fragment_body(&names, &[])?;
    assert!(!body.contains("Empfohlen"), "{body}");
    assert!(body.contains("alpha, bravo"), "{body}");
    assert!(body.contains("india"), "{body}");
    Ok(())
}

#[test]
fn wanted_tags_are_lowercase_and_capped() {
    let text = "Bitte REVIEW den Rust-Code; fix es, und Tests! \
                    alpha bravo charlie delta echo foxtrot golf hotel";
    let tags = wanted_tags(text);

    assert_eq!(tags.len(), 8);
    assert!(tags.iter().all(|tag| *tag == tag.to_lowercase()));
    assert!(tags.iter().all(|tag| tag.chars().count() >= 4));
    // In Textreihenfolge: die ersten acht Wörter ab vier Zeichen.
    let expected: BTreeSet<String> = [
        "bitte",
        "review",
        "rust-code",
        "tests",
        "alpha",
        "bravo",
        "charlie",
        "delta",
    ]
    .iter()
    .map(|tag| (*tag).to_owned())
    .collect();
    assert_eq!(tags, expected);

    // Kurze Wörter, Duplikate und Randzeichen fallen weg.
    let tags = wanted_tags("Der -Plan- plan PLAN, ein Test");
    let expected: BTreeSet<String> = ["plan", "test"].iter().map(|t| (*t).to_owned()).collect();
    assert_eq!(tags, expected);
    assert!(wanted_tags("").is_empty());
}
