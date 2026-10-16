//! End-to-end-Tests für den Turn-Loop: Model-Call, Tool-Ausführung,
//! Guardrails und Handoff-Detection.

use harw_agent_dsl::roles::AgentRoleId;
use harw_authority::{
    Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
};
use harw_core::{
    AgentSession, ApprovalResolution, ConfigApprovalPolicy, EchoModelProvider, InMemoryStateStore,
    ModelFuture, ModelProvider, ModelRequest, ModelResponse, SessionState, SpawnContext, TurnInput,
    TurnOutcome, resume_after_approval, resume_after_approval_durable, run_turn, run_turn_durable,
};
use harw_extension_api::{
    AgentJobFuture, AgentJobHandle, AgentJobSubmitter, AgentSpawnError, AgentSpawner,
    ApprovalDecision, ApprovalHandler, ExtensionRegistry, ExtensionRegistryBuilder, SpawnFuture,
    SpawnInput, ToolExecutor, ToolExecutorFuture, ToolName, ToolOutput, ToolProvider, ToolSpec,
};
use harw_session_store::ApprovalStore;
use harw_tools::ToolCall;
use harw_tools::schema::JsonSchema;
use harw_tools::spec::FunctionToolSpec;
use harw_types::{
    AgentRole, ApprovalActor, ItemId, ReviewDecision, SessionId, TenantId, ToolCallId, WorkId,
    WorkspaceId,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;

mod common;
use common::{TestError, TestResult, ctx};

fn tool_spec(name: &str) -> ToolSpec {
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(name),
        description: "test tool".to_owned(),
        parameters: JsonSchema::default(),
        strict: false,
    })
}

fn new_session(
    registry: ExtensionRegistry,
) -> TestResult<(
    AgentSession,
    mpsc::UnboundedReceiver<harw_protocol::events::SessionEvent>,
)> {
    let (tx, rx) = mpsc::unbounded_channel();
    Ok((
        AgentSession::new(AgentRole::Assistant, None, registry, tx).with_spawn_context(
            SpawnContext {
                sandbox: test_sandbox()?,
                suggestions: None,
                capability_snapshot: None,
                approval_actor: Some(test_approval_actor()),
                organizational_role: AgentRoleId::RootOrchestrator,
                allowed_child_orchestrators: Vec::new(),
                // Generic fixture — not exercising trace propagation.
                trace: None,
                // Generic fixture — not exercising context-ceiling propagation.
                ceiling: None,
            },
        ),
        rx,
    ))
}

fn test_approval_actor() -> ApprovalActor {
    ApprovalActor::Operator {
        id: "test-operator".to_owned(),
    }
}

fn test_sandbox() -> TestResult<SandboxSpec> {
    let harness_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or(TestError::Missing("harw-core has a workspace parent"))?
        .to_path_buf();
    let registry = WorkspaceRegistry::build(
        &harness_root,
        [WorkspaceRegistration {
            tenant: TenantId::from_str("test-tenant"),
            workspace: WorkspaceId::from_str("core-tests"),
            root: PathBuf::from("harw-core"),
        }],
    )
    .map_err(ctx("test workspace is registered"))?;
    Ok(SandboxSpec::from_resolved(
        registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("core-tests"),
            )
            .map_err(ctx("test workspace resolves"))?,
        PermissionSet::from_policy([Permission::ReadWorkspace]),
    ))
}

// --- Test-Doubles --------------------------------------------------------

/// Liefert eine vorprogrammierte Folge von Responses, eine pro Model-Call.
struct ScriptedModel {
    responses: std::sync::Mutex<std::collections::VecDeque<ModelResponse>>,
}

impl ScriptedModel {
    fn new(responses: Vec<ModelResponse>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses.into_iter().collect()),
        }
    }
}

impl ModelProvider for ScriptedModel {
    fn respond<'a>(&'a self, _req: ModelRequest) -> ModelFuture<'a> {
        let next = self
            .responses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front();
        Box::pin(async move { next.ok_or(harw_core::ModelError::EmptyResponse) })
    }
}

/// Models the manager half of an "agent as a tool" interaction. Its second
/// invocation proves that the regular tool result was committed to the
/// requesting session's model-visible history before the model was run again.
struct ToolResultAwareModel {
    calls: AtomicUsize,
    tool_call_id: ToolCallId,
}

/// Verifies that a tool-execution rejection is returned to the model as an
/// error result, allowing the turn to complete without reaching the executor.
struct MissingContextAwareModel {
    calls: AtomicUsize,
    tool_call_id: ToolCallId,
}

impl ModelProvider for MissingContextAwareModel {
    fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
        let call_number = self.calls.fetch_add(1, Ordering::SeqCst);
        let call_id = self.tool_call_id.clone();
        Box::pin(async move {
            if call_number == 0 {
                return Ok(ModelResponse {
                    message: None,
                    tool_calls: vec![ToolCall {
                        id: call_id,
                        name: ToolName::new("echo"),
                        arguments: serde_json::json!({"attempt": "ambient authority"}),
                    }],
                    usage: Default::default(),
                    ..Default::default()
                });
            }

            let rejected = request
                .history
                .to_model_messages()
                .into_iter()
                .find_map(|message| match message {
                    harw_core::ModelMessage::ToolResult { call_id, result }
                        if call_id == self.tool_call_id =>
                    {
                        Some(result)
                    }
                    _ => None,
                })
                .ok_or_else(|| {
                    harw_core::ModelError::RequestFailed(
                        "rejected tool result was absent from the next model request".to_owned(),
                    )
                })?;

            match rejected {
                harw_protocol::ToolCallResult::Error { message }
                    if message.contains("without a trusted sandbox context") =>
                {
                    Ok(ModelResponse::text("tool execution was rejected safely"))
                }
                other => Err(harw_core::ModelError::RequestFailed(format!(
                    "expected missing-context tool error, got {other:?}"
                ))),
            }
        })
    }
}

impl ModelProvider for ToolResultAwareModel {
    fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
        let call_number = self.calls.fetch_add(1, Ordering::SeqCst);
        let call_id = self.tool_call_id.clone();
        Box::pin(async move {
            if call_number == 0 {
                return Ok(ModelResponse {
                    message: None,
                    tool_calls: vec![ToolCall {
                        id: call_id,
                        name: ToolName::new("micro_agent"),
                        arguments: serde_json::json!({"invoice_volume": 900}),
                    }],
                    usage: Default::default(),
                    ..Default::default()
                });
            }

            let returned = request
                .history
                .to_model_messages()
                .into_iter()
                .find_map(|message| match message {
                    harw_core::ModelMessage::ToolResult { call_id, result }
                        if call_id == self.tool_call_id =>
                    {
                        Some(result)
                    }
                    _ => None,
                })
                .ok_or_else(|| {
                    harw_core::ModelError::RequestFailed(
                        "tool result was absent from the next model request".to_owned(),
                    )
                })?;

            Ok(ModelResponse::text(format!(
                "manager received: {returned:?}"
            )))
        })
    }
}

struct FailingModel;

impl ModelProvider for FailingModel {
    fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
        Box::pin(async {
            Err(harw_core::ModelError::RequestFailed(
                "unavailable".to_owned(),
            ))
        })
    }
}

struct EchoTool {
    calls: Arc<AtomicUsize>,
}

impl ToolExecutor for EchoTool {
    fn execute<'a>(
        &'a self,
        _context: &'a harw_extension_api::ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let args = call.arguments.clone();
        Box::pin(async move { Ok(ToolOutput::json(args)) })
    }
}

struct EchoToolProvider {
    name: ToolName,
    calls: Arc<AtomicUsize>,
}

struct ParallelEchoToolProvider {
    name: ToolName,
    calls: Arc<AtomicUsize>,
}

impl ToolProvider for ParallelEchoToolProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![tool_spec(self.name.as_str())]
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        if name == &self.name {
            Some(Arc::new(EchoTool {
                calls: self.calls.clone(),
            }))
        } else {
            None
        }
    }

    fn parallel_safe(&self, name: &ToolName) -> bool {
        name == &self.name
    }
}

struct AskApproval {
    request: ItemId,
}

impl ApprovalHandler for AskApproval {
    fn review<'a>(
        &'a self,
        _call: &'a ToolCall,
    ) -> harw_extension_api::ExtFuture<'a, ApprovalDecision> {
        let request = self.request.clone();
        Box::pin(async move { ApprovalDecision::AskUser(request) })
    }
}

impl ToolProvider for EchoToolProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![tool_spec(self.name.as_str())]
    }
    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        if name == &self.name {
            Some(Arc::new(EchoTool {
                calls: self.calls.clone(),
            }))
        } else {
            None
        }
    }
}

struct StubSpawner {
    child: SessionId,
    spawns: Arc<AtomicUsize>,
    finished: Arc<AtomicUsize>,
}

impl AgentSpawner for StubSpawner {
    fn spawn_child<'a>(
        &'a self,
        _role: &'a str,
        _input: SpawnInput,
        sandbox: SandboxSpec,
        _suggestions: Option<harw_catalog::AgentSuggestions>,
    ) -> SpawnFuture<'a> {
        self.spawns.fetch_add(1, Ordering::SeqCst);
        let child = self.child.clone();
        Box::pin(async move {
            assert_eq!(sandbox.workspace().workspace().as_str(), "core-tests");
            assert!(sandbox.permissions().contains(Permission::ReadWorkspace));
            Ok::<_, AgentSpawnError>(child)
        })
    }

    fn child_finished(&self, child: &SessionId) {
        assert_eq!(child, &self.child);
        self.finished.fetch_add(1, Ordering::SeqCst);
    }
}

// --- Tests ---------------------------------------------------------------

#[tokio::test]
async fn echo_model_completes_turn() -> TestResult {
    let registry = ExtensionRegistryBuilder::default().build();
    let (mut session, _rx) = new_session(registry)?;
    let store = InMemoryStateStore::new();
    let model = EchoModelProvider::new("hi there");

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("hello"))
        .await
        .map_err(ctx("turn runs"))?;

    assert!(matches!(outcome, TurnOutcome::Completed));
    assert_eq!(*session.state(), SessionState::Idle);
    // user + assistant
    assert_eq!(session.history().len(), 2);
    // persisted both items
    assert_eq!(store.turn_count(session.id()), 2);
    Ok(())
}

#[tokio::test]
async fn tool_call_then_final() -> TestResult {
    let calls = Arc::new(AtomicUsize::new(0));
    let registry = ExtensionRegistryBuilder::default()
        .tool_provider(Arc::new(EchoToolProvider {
            name: ToolName::new("echo"),
            calls: calls.clone(),
        }))
        .build();
    let (mut session, _rx) = new_session(registry)?;
    let store = InMemoryStateStore::new();

    let model = ScriptedModel::new(vec![
        // first call: request the echo tool
        ModelResponse {
            message: Some("let me use a tool".to_owned()),
            tool_calls: vec![ToolCall {
                id: ToolCallId::new(),
                name: ToolName::new("echo"),
                arguments: serde_json::json!({"k": "v"}),
            }],
            usage: Default::default(),
            ..Default::default()
        },
        // second call: final answer, no tools
        ModelResponse::text("done"),
    ]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("go"))
        .await
        .map_err(ctx("turn runs"))?;

    assert!(matches!(outcome, TurnOutcome::Completed));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "tool executed once");
    assert_eq!(*session.state(), SessionState::Idle);
    // user, assistant(commentary), tool_call, tool_result, assistant(final)
    assert_eq!(session.history().len(), 5);
    Ok(())
}

#[tokio::test]
async fn duplicate_tool_names_fail_before_any_model_or_tool_dispatch() -> TestResult {
    let calls = Arc::new(AtomicUsize::new(0));
    let registry = ExtensionRegistryBuilder::default()
        .tool_provider(Arc::new(EchoToolProvider {
            name: ToolName::new("ambiguous"),
            calls: calls.clone(),
        }))
        .tool_provider(Arc::new(EchoToolProvider {
            name: ToolName::new("ambiguous"),
            calls: calls.clone(),
        }))
        .build();
    let (mut session, _rx) = new_session(registry)?;
    let store = InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![ModelResponse::text("must not run")]);

    let Err(error) = run_turn(&mut session, &model, &store, TurnInput::user("go")).await else {
        return Err(TestError::Unexpected(
            "ambiguous tool ownership is unsafe".to_owned(),
        ));
    };
    assert!(matches!(
        error,
        harw_core::CoreError::DuplicateTool { ref name } if name == "ambiguous"
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn tool_execution_without_a_server_resolved_sandbox_fails_closed() -> TestResult {
    let calls = Arc::new(AtomicUsize::new(0));
    let registry = ExtensionRegistryBuilder::default()
        .tool_provider(Arc::new(EchoToolProvider {
            name: ToolName::new("echo"),
            calls: calls.clone(),
        }))
        .build();
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx);
    let store = InMemoryStateStore::new();
    let model = MissingContextAwareModel {
        calls: AtomicUsize::new(0),
        tool_call_id: ToolCallId::new(),
    };

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("go"))
        .await
        .map_err(ctx("missing execution context is returned to the model"))?;

    assert!(matches!(outcome, TurnOutcome::Completed));
    assert_eq!(calls.load(Ordering::SeqCst), 0, "tool was never executed");
    assert_eq!(
        model.calls.load(Ordering::SeqCst),
        2,
        "model saw the rejection"
    );
    assert_eq!(*session.state(), SessionState::Idle);
    Ok(())
}

#[tokio::test]
async fn micro_agent_tool_result_returns_to_requesting_model_before_final_answer() -> TestResult {
    let calls = Arc::new(AtomicUsize::new(0));
    let registry = ExtensionRegistryBuilder::default()
        .tool_provider(Arc::new(EchoToolProvider {
            name: ToolName::new("micro_agent"),
            calls: calls.clone(),
        }))
        .build();
    let (mut session, _rx) = new_session(registry)?;
    let store = InMemoryStateStore::new();
    let model = ToolResultAwareModel {
        calls: AtomicUsize::new(0),
        tool_call_id: ToolCallId::new(),
    };

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("help me"))
        .await
        .map_err(ctx("micro-agent tool loop completes"))?;

    assert!(matches!(outcome, TurnOutcome::Completed));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    assert_eq!(*session.state(), SessionState::Idle);
    Ok(())
}

#[tokio::test]
async fn explicitly_parallel_safe_tool_calls_are_joined_before_the_next_model_run() -> TestResult {
    let calls = Arc::new(AtomicUsize::new(0));
    let registry = ExtensionRegistryBuilder::default()
        .tool_provider(Arc::new(ParallelEchoToolProvider {
            name: ToolName::new("lookup"),
            calls: calls.clone(),
        }))
        .build();
    let (mut session, _rx) = new_session(registry)?;
    let store = InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![
        ModelResponse {
            message: None,
            tool_calls: vec![
                ToolCall {
                    id: ToolCallId::new(),
                    name: ToolName::new("lookup"),
                    arguments: serde_json::json!({"part": 1}),
                },
                ToolCall {
                    id: ToolCallId::new(),
                    name: ToolName::new("lookup"),
                    arguments: serde_json::json!({"part": 2}),
                },
            ],
            usage: Default::default(),
            ..Default::default()
        },
        ModelResponse::text("both lookups are available"),
    ]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("parallel"))
        .await
        .map_err(ctx("parallel tool response completes"))?;

    assert!(matches!(outcome, TurnOutcome::Completed));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(*session.state(), SessionState::Idle);
    Ok(())
}

/// Records submitted children and answers with a fresh durable work id.
struct StubJobSubmitter {
    submitted: Arc<AtomicUsize>,
}

impl AgentJobSubmitter for StubJobSubmitter {
    fn submit_child<'a>(
        &'a self,
        child: &'a SessionId,
        _task: Option<&'a str>,
    ) -> AgentJobFuture<'a> {
        self.submitted.fetch_add(1, Ordering::SeqCst);
        let child = child.clone();
        Box::pin(async move {
            Ok(AgentJobHandle {
                work_id: WorkId::new(),
                child,
            })
        })
    }
}

fn handoff_model(handoff_call_id: &ToolCallId, arguments: serde_json::Value) -> ScriptedModel {
    ScriptedModel::new(vec![
        ModelResponse {
            message: None,
            tool_calls: vec![ToolCall {
                id: handoff_call_id.clone(),
                name: ToolName::new("transfer_to_worker"),
                arguments,
            }],
            usage: Default::default(),
            ..Default::default()
        },
        ModelResponse::text("started, continuing"),
    ])
}

#[tokio::test]
async fn handoff_starts_in_the_background_without_pausing_the_turn() -> TestResult {
    let child_id = SessionId::new();
    let spawns = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(AtomicUsize::new(0));
    let submitted = Arc::new(AtomicUsize::new(0));
    let registry = ExtensionRegistryBuilder::default()
        .spawner(Arc::new(StubSpawner {
            child: child_id,
            spawns: spawns.clone(),
            finished,
        }))
        .agent_job_submitter(Arc::new(StubJobSubmitter {
            submitted: submitted.clone(),
        }))
        .build();
    let (mut session, _rx) = new_session(registry)?;
    let store = InMemoryStateStore::new();
    let model = handoff_model(&ToolCallId::new(), serde_json::json!({"task": "sub"}));

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("delegate"))
        .await
        .map_err(ctx("turn runs"))?;

    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    assert_eq!(spawns.load(Ordering::SeqCst), 1);
    assert_eq!(submitted.load(Ordering::SeqCst), 1);
    assert_eq!(*session.state(), SessionState::Idle);
    Ok(())
}

#[tokio::test]
async fn handoff_without_a_background_executor_is_a_tool_error_and_spawns_nothing() -> TestResult {
    let spawns = Arc::new(AtomicUsize::new(0));
    let registry = ExtensionRegistryBuilder::default()
        .spawner(Arc::new(StubSpawner {
            child: SessionId::new(),
            spawns: spawns.clone(),
            finished: Arc::new(AtomicUsize::new(0)),
        }))
        .build();
    let (mut session, _rx) = new_session(registry)?;
    let store = InMemoryStateStore::new();
    let model = handoff_model(&ToolCallId::new(), serde_json::json!({"task": "sub"}));

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("delegate"))
        .await
        .map_err(ctx("turn runs"))?;

    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    assert_eq!(spawns.load(Ordering::SeqCst), 0);
    assert_eq!(*session.state(), SessionState::Idle);
    Ok(())
}

#[tokio::test]
async fn approval_pause_resumes_the_original_call_then_runs_the_model_again() -> TestResult {
    let calls = Arc::new(AtomicUsize::new(0));
    let request = ItemId::new();
    let registry = ExtensionRegistryBuilder::default()
        .tool_provider(Arc::new(EchoToolProvider {
            name: ToolName::new("echo"),
            calls: calls.clone(),
        }))
        .approval_handler(Arc::new(AskApproval {
            request: request.clone(),
        }))
        .build();
    let (mut session, _rx) = new_session(registry)?;
    let store = InMemoryStateStore::new();
    let call_id = ToolCallId::new();
    let model = ScriptedModel::new(vec![
        ModelResponse {
            message: None,
            tool_calls: vec![ToolCall {
                id: call_id.clone(),
                name: ToolName::new("echo"),
                arguments: serde_json::json!({"approved": true}),
            }],
            usage: Default::default(),
            ..Default::default()
        },
        ModelResponse::text("approved tool result consumed"),
    ]);

    let paused = run_turn(&mut session, &model, &store, TurnInput::user("go"))
        .await
        .map_err(ctx("turn pauses for approval"))?;
    assert!(matches!(
        paused,
        TurnOutcome::AwaitingApproval {
            call_id: observed,
            request: observed_request,
        } if observed == call_id && observed_request == request
    ));
    assert_eq!(*session.state(), SessionState::WaitingForApproval);
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let wrong_actor = ApprovalActor::Operator {
        id: "other-operator".to_owned(),
    };
    let Err(mismatch) = resume_after_approval(
        &mut session,
        &model,
        &store,
        wrong_actor,
        ApprovalResolution::Approve,
    )
    .await
    else {
        return Err(TestError::Unexpected(
            "a different actor cannot approve this tool call".to_owned(),
        ));
    };
    assert!(matches!(
        mismatch,
        harw_core::CoreError::ApprovalActorMismatch { .. }
    ));
    assert_eq!(*session.state(), SessionState::WaitingForApproval);

    let resumed = resume_after_approval(
        &mut session,
        &model,
        &store,
        test_approval_actor(),
        ApprovalResolution::Approve,
    )
    .await
    .map_err(ctx("approval resumes original call"))?;

    assert!(matches!(resumed, TurnOutcome::Completed));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(*session.state(), SessionState::Idle);
    Ok(())
}

#[tokio::test]
async fn configured_policy_section_is_a_runtime_approval_handler() -> TestResult {
    let calls = Arc::new(AtomicUsize::new(0));
    let registry = ExtensionRegistryBuilder::default()
        .tool_provider(Arc::new(EchoToolProvider {
            name: ToolName::new("shell"),
            calls: calls.clone(),
        }))
        .approval_handler(Arc::new(ConfigApprovalPolicy::new(["shell".to_owned()])))
        .build();
    let (mut session, _rx) = new_session(registry)?;
    let store = InMemoryStateStore::new();
    let model = ScriptedModel::new(vec![ModelResponse {
        message: None,
        tool_calls: vec![ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("shell"),
            arguments: serde_json::json!({"command": "echo governed"}),
        }],
        usage: Default::default(),
        ..Default::default()
    }]);

    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("run it"))
        .await
        .map_err(ctx("configured policy pauses rather than dispatching"))?;
    assert!(matches!(outcome, TurnOutcome::AwaitingApproval { .. }));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn double_start_is_rejected() -> TestResult {
    let registry = ExtensionRegistryBuilder::default().build();
    let (mut session, _rx) = new_session(registry)?;
    // manually move to Running and try to start again
    let _h = session.try_start_turn().map_err(ctx("first start ok"))?;
    let err = session.try_start_turn();
    assert!(err.is_err(), "second start must be rejected while running");
    Ok(())
}

#[tokio::test]
async fn model_failure_transitions_the_active_session_to_failed() -> TestResult {
    let registry = ExtensionRegistryBuilder::default().build();
    let (mut session, _rx) = new_session(registry)?;
    let store = InMemoryStateStore::new();

    let Err(error) = run_turn(
        &mut session,
        &FailingModel,
        &store,
        TurnInput::user("hello"),
    )
    .await
    else {
        return Err(TestError::Unexpected(
            "model failure reaches caller".to_owned(),
        ));
    };

    assert!(error.to_string().contains("unavailable"));
    assert!(
        matches!(session.state(), SessionState::Failed(reason) if reason.contains("unavailable"))
    );
    Ok(())
}

#[tokio::test]
async fn durable_approval_is_written_then_consumed_once_before_resuming() -> TestResult {
    let calls = Arc::new(AtomicUsize::new(0));
    let request = ItemId::new();
    let registry = ExtensionRegistryBuilder::default()
        .tool_provider(Arc::new(EchoToolProvider {
            name: ToolName::new("echo"),
            calls: calls.clone(),
        }))
        .approval_handler(Arc::new(AskApproval {
            request: request.clone(),
        }))
        .build();
    let (mut session, _rx) = new_session(registry)?;
    let store = InMemoryStateStore::new();
    let temp = tempfile::tempdir().map_err(ctx("temp directory"))?;
    let approvals = ApprovalStore::new(temp.path());
    let call_id = ToolCallId::new();
    let model = ScriptedModel::new(vec![
        ModelResponse {
            message: None,
            tool_calls: vec![ToolCall {
                id: call_id.clone(),
                name: ToolName::new("echo"),
                arguments: serde_json::json!({"durable": true}),
            }],
            usage: Default::default(),
            ..Default::default()
        },
        ModelResponse::text("durable approval consumed"),
    ]);

    let paused = run_turn_durable(
        &mut session,
        &model,
        &store,
        &approvals,
        TurnInput::user("go"),
    )
    .await
    .map_err(ctx("durable request is issued before the turn pauses"))?;
    assert!(matches!(paused, TurnOutcome::AwaitingApproval { .. }));
    let durable = approvals
        .pending(session.id(), &request)
        .map_err(ctx("request survives outside the in-memory session"))?;
    assert_eq!(durable.call_id, call_id);
    assert_eq!(durable.actor, test_approval_actor());

    let resumed = resume_after_approval_durable(
        &mut session,
        &model,
        &store,
        &approvals,
        test_approval_actor(),
        ApprovalResolution::Approve,
    )
    .await
    .map_err(ctx("the matching actor consumes and resumes once"))?;
    assert!(matches!(resumed, TurnOutcome::Completed));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let Err(replay) = approvals.resolve(
        session.id(),
        &request,
        ReviewDecision::Approved,
        None,
        &test_approval_actor(),
        &harw_types::SystemClock,
    ) else {
        return Err(TestError::Unexpected(
            "the durable request cannot be consumed twice".to_owned(),
        ));
    };
    assert!(matches!(
        replay,
        harw_session_store::SessionStoreError::ApprovalAlreadyResolved { .. }
    ));
    Ok(())
}

/// Liefert genau ein Kontextfragment (für den Ledger-Test).
struct NoteProvider;

impl harw_extension_api::contributors::ContextProvider for NoteProvider {
    fn contribute<'a>(
        &'a self,
        _ctx: &'a harw_extension_api::TurnInputContext,
    ) -> harw_extension_api::contributors::ExtFuture<'a, Vec<harw_extension_api::ContextFragment>>
    {
        Box::pin(async {
            vec![harw_extension_api::ContextFragment {
                label: "note".to_owned(),
                content: "secret content that must never reach the ledger".to_owned(),
            }]
        })
    }

    fn namespace(&self) -> &'static str {
        "ledger-test.note"
    }
}

#[tokio::test]
async fn context_ledger_records_offered_fragments_without_content() -> TestResult {
    let registry = ExtensionRegistryBuilder::default()
        .context_provider(Arc::new(NoteProvider))
        .map_err(ctx("provider registers"))?
        .build();
    let (session, _rx) = new_session(registry)?;
    let ledger = Arc::new(harw_context_ledger::MemoryLedger::new());
    let mut session = session.with_context_ledger(Some(
        Arc::clone(&ledger) as Arc<dyn harw_context_ledger::LedgerSink>
    ));
    let store = InMemoryStateStore::new();
    let model = EchoModelProvider::new("ok");

    run_turn(&mut session, &model, &store, TurnInput::user("hello"))
        .await
        .map_err(ctx("turn runs"))?;

    let entries = ledger.entries();
    let note = entries
        .iter()
        .find(|entry| entry.label == "note")
        .ok_or_else(|| {
            TestError::Unexpected(format!("no ledger entry for the note: {entries:?}"))
        })?;
    assert_eq!(note.kind, harw_context_ledger::LedgerKind::Offered);
    assert_eq!(note.session_id, session.id().as_str());
    assert!(note.bytes.is_some_and(|bytes| bytes > 0));
    let dump = format!("{entries:?}");
    assert!(
        !dump.contains("secret content"),
        "the ledger must never hold fragment content"
    );
    Ok(())
}

#[tokio::test]
async fn without_a_ledger_nothing_is_recorded_and_the_turn_is_unchanged() -> TestResult {
    let registry = ExtensionRegistryBuilder::default()
        .context_provider(Arc::new(NoteProvider))
        .map_err(ctx("provider registers"))?
        .build();
    let (mut session, _rx) = new_session(registry)?;
    assert!(session.context_ledger().is_none());
    let store = InMemoryStateStore::new();
    let model = EchoModelProvider::new("ok");
    let outcome = run_turn(&mut session, &model, &store, TurnInput::user("hello"))
        .await
        .map_err(ctx("turn runs"))?;
    assert!(matches!(outcome, TurnOutcome::Completed));
    Ok(())
}
