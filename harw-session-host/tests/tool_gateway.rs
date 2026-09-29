//! R18 gateway tool host against the P1 test matrix (contract
//! `docs/planning/65-cloud-sessions/contracts/R18-tool-gateway.md` §12,
//! TG-01..TG-10; TG-11/TG-12 at the transport level live in
//! `harw-session-ws`). Drives the real host through its public ports.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use harw_authority::{
    Permission, PermissionRequest, PermissionSet, WorkspaceRegistration, WorkspaceRegistry,
};
use harw_protocol::session_wire::{
    ApprovalRespondParams, AttachParams, CreateParams, GatewayDrainParams, GatewayListenerInfo,
    GatewayListenerSetParams, GatewayRevokeParams, GatewayToolRightsParams, HelloParams,
    ListenerKind, RespondResult, ToolCallParams, ToolCallResultFrame, ToolCancelParams,
    ToolListParams,
};
use harw_protocol::{
    AgentRole, ClientCaps, FrameSource, GatewayPort, PortError, ResultTrust, SessionFrame,
    SessionPort, StreamProfile, ToolApproval, ToolCallResult, ToolPlacement, ToolPort, ToolRefusal,
    TurnEvent,
};
use harw_session_host::approvals::{ApprovalBackend, MemoryApprovals};
use harw_session_host::driver::{
    CancelSignal, DriverFuture, EventSink, Setting, TurnDriver, TurnInput, TurnOutcome,
};
use harw_session_host::replay::MemoryTranscripts;
use harw_session_host::{
    AgentCredential, ClientIdentity, ConnectionId, Delegation, HostConfig, HostConnection,
    HostError, NoGatewaySandbox, SessionHost, ToolGrant, ToolHost, ToolRegistration,
    WorkspaceGatewaySandbox, WorkspaceSandboxConfig, caps_for_tier, gateway_caps_for_tier,
};
use harw_tools::{
    AdditionalProperties, FunctionToolSpec, JsonSchema, JsonSchemaType, ToolCall,
    ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolName, ToolOutput, ToolSpec,
};
use harw_types::{
    ApprovalActor, AuthStrength, IngressSurface, PermissionTier, Principal, PrincipalKind,
    ReviewDecision, SessionId, TenantId, ToolCallId, TrustZone, TurnId, WorkspaceId,
};
use serde_json::json;
use tokio::sync::Notify;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const WAIT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct IdleDriver;

impl TurnDriver for IdleDriver {
    fn create_session(&self, _: &SessionId, _: Option<&str>) -> DriverFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn run_turn(
        &self,
        _: TurnInput,
        _: CancelSignal,
        _: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        Box::pin(async { Ok(TurnOutcome::Completed) })
    }
    fn resume_after_approval(
        &self,
        _: &SessionId,
        _: CancelSignal,
        _: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        Box::pin(async { Ok(TurnOutcome::Completed) })
    }
    fn apply_setting(&self, _: &SessionId, _: Setting) -> DriverFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn model_name(&self, _: &SessionId) -> Option<String> {
        None
    }
}

/// Spy executor: counts invocations, reports its sandbox, optionally waits
/// on a gate (for in-flight tests).
#[derive(Default)]
struct Spy {
    calls: AtomicUsize,
    entered: Notify,
    gate: Option<Notify>,
}

impl Spy {
    fn gated() -> Self {
        Self {
            gate: Some(Notify::new()),
            ..Self::default()
        }
    }

    fn count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn release(&self) {
        if let Some(gate) = &self.gate {
            gate.notify_one();
        }
    }

    async fn wait_entered(&self) -> TestResult {
        tokio::time::timeout(WAIT, self.entered.notified()).await?;
        Ok(())
    }
}

impl ToolExecutor for Spy {
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.notify_one();
            if let Some(gate) = &self.gate {
                gate.notified().await;
            }
            Ok(ToolOutput::json(json!({
                "tool": call.name.as_str(),
                "workspace": context.sandbox().workspace().workspace().as_str(),
                "permissions": context.sandbox().permissions().iter().count(),
            })))
        })
    }
}

fn spec(name: &str) -> ToolSpec {
    let mut properties = std::collections::BTreeMap::new();
    properties.insert(
        "path".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            ..JsonSchema::default()
        },
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(name),
        description: format!("{name} (test)"),
        parameters: JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(properties),
            additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
            ..JsonSchema::default()
        },
        strict: false,
    })
}

struct Fixture {
    _dir: tempfile::TempDir,
    host: SessionHost,
    fast: Arc<Spy>,
    slow: Arc<Spy>,
    danger: Arc<Spy>,
}

fn fixture_with(sandbox_available: bool) -> TestResult<Fixture> {
    let dir = tempfile::tempdir()?;
    std::fs::create_dir_all(dir.path().join("state"))?;
    std::fs::create_dir_all(dir.path().join("ws"))?;
    let registry = WorkspaceRegistry::build(
        dir.path(),
        [WorkspaceRegistration {
            tenant: TenantId::try_from_str("local")?,
            workspace: WorkspaceId::try_from_str("w")?,
            root: "ws".into(),
        }],
    )?;
    let sandbox = WorkspaceGatewaySandbox::new(
        WorkspaceSandboxConfig {
            registry,
            permissions: PermissionSet::from_policy([
                Permission::ReadWorkspace,
                Permission::WriteWorkspace,
            ]),
            default_tenant: TenantId::try_from_str("local")?,
            default_workspace: WorkspaceId::try_from_str("w")?,
        },
        sandbox_available,
    );
    let approvals = Arc::new(MemoryApprovals::new());
    let fast = Arc::new(Spy::default());
    let slow = Arc::new(Spy::gated());
    let danger = Arc::new(Spy::default());
    let tools = ToolHost::builder(Arc::new(sandbox))
        .node("gw-1")
        .tool(
            ToolRegistration::new(spec("fs.read"), Arc::clone(&fast) as Arc<dyn ToolExecutor>)
                .approval(ToolApproval::Never)
                .parallel_safe(true),
        )
        .tool(
            ToolRegistration::new(spec("fs.write"), Arc::clone(&fast) as Arc<dyn ToolExecutor>)
                .approval(ToolApproval::Never),
        )
        .tool(
            ToolRegistration::new(spec("slow.op"), Arc::clone(&slow) as Arc<dyn ToolExecutor>)
                .approval(ToolApproval::Never),
        )
        .tool(ToolRegistration::new(
            spec("danger.op"),
            Arc::clone(&danger) as Arc<dyn ToolExecutor>,
        ))
        .approvals(Arc::clone(&approvals) as Arc<dyn harw_session_host::ToolApprovalIssuer>)
        .approval_timeout(WAIT)
        .build()?;
    let host = SessionHost::open_with_tools(
        HostConfig::new(dir.path().join("state")),
        Arc::new(IdleDriver),
        Arc::new(MemoryTranscripts::new()),
        approvals as Arc<dyn ApprovalBackend>,
        tools,
    )?;
    host.agents().enroll_uia(
        AgentCredential::new("cred-uia")?,
        "agent:uia",
        None,
        &ToolGrant::from_names(["fs.read", "slow.op", "danger.op"]),
    )?;
    Ok(Fixture {
        _dir: dir,
        host,
        fast,
        slow,
        danger,
    })
}

fn fixture() -> TestResult<Fixture> {
    fixture_with(true)
}

fn human_identity(uid: u32, tier: PermissionTier, tenant: Option<TenantId>) -> ClientIdentity {
    ClientIdentity {
        principal: Principal::trusted_ingress(
            PrincipalKind::Human,
            format!("uid:{uid}"),
            IngressSurface::Tui,
            tier,
        ),
        tenant,
        caps: caps_for_tier(tier).with(gateway_caps_for_tier(tier)),
        device: None,
        actor: ApprovalActor::Operator {
            id: format!("uid:{uid}"),
        },
        label: format!("human-{uid}"),
        zone: TrustZone::Local,
        strength: AuthStrength::PeerCredential,
        connection: ConnectionId::next(),
        agent: None,
    }
}

/// What a listener does for an agent process: resolve the credential and
/// build the identity from the registration, never from a payload.
fn agent_identity(host: &SessionHost, credential: &str) -> TestResult<ClientIdentity> {
    let resolved = host
        .agents()
        .resolve(&AgentCredential::new(credential)?)
        .ok_or("unknown agent credential")?;
    let id = resolved.principal.id.clone();
    Ok(ClientIdentity {
        principal: Principal::trusted_ingress(
            PrincipalKind::Model,
            id.clone(),
            IngressSurface::Gateway,
            PermissionTier::Operator,
        ),
        tenant: resolved.tenant,
        caps: caps_for_tier(PermissionTier::Operator).with(ClientCaps::TOOL_CALL),
        device: None,
        actor: ApprovalActor::Operator { id: id.clone() },
        label: id,
        zone: TrustZone::Local,
        strength: AuthStrength::PeerCredential,
        connection: ConnectionId::next(),
        agent: Some(resolved.principal),
    })
}

async fn hello(connection: &HostConnection, wire_minor: u32) -> TestResult<ClientCaps> {
    let ack = connection
        .hello(HelloParams {
            client_label: "test".into(),
            wire_minor,
            features: vec!["tools".into(), "gateway".into()],
            requested_caps: None,
        })
        .await?;
    Ok(ack.granted)
}

async fn connect(host: &SessionHost, identity: ClientIdentity) -> TestResult<HostConnection> {
    let connection = host.connect(identity)?;
    hello(&connection, 2).await?;
    Ok(connection)
}

async fn agent(host: &SessionHost, credential: &str) -> TestResult<HostConnection> {
    connect(host, agent_identity(host, credential)?).await
}

fn call(session: &SessionId, call_id: &str, tool: &str) -> TestResult<ToolCallParams> {
    Ok(ToolCallParams {
        session_id: session.clone(),
        turn_id: TurnId::try_from_str("turn-1")?,
        call_id: ToolCallId::try_from_str(call_id)?,
        tool_name: tool.into(),
        arguments: json!({"path": "src/lib.rs"}),
        parent_call_id: None,
    })
}

fn refusal(result: Result<ToolCallResultFrame, PortError>) -> Option<ToolRefusal> {
    match result {
        Err(PortError::ToolRefused { refusal, .. }) => Some(refusal),
        _ => None,
    }
}

fn spawn_call(
    connection: &Arc<HostConnection>,
    params: ToolCallParams,
) -> tokio::task::JoinHandle<Result<ToolCallResultFrame, PortError>> {
    let connection = Arc::clone(connection);
    tokio::spawn(async move { connection.call_tool(params).await })
}

async fn attach(
    connection: &HostConnection,
    session: &SessionId,
) -> TestResult<Box<dyn FrameSource>> {
    let (_, frames) = connection
        .attach(AttachParams {
            session_id: session.clone(),
            from: None,
            profile: StreamProfile::Full,
            tail_items: 200,
        })
        .await?;
    Ok(frames)
}

async fn wait_for(
    source: &mut Box<dyn FrameSource>,
    pick: impl Fn(&SessionFrame) -> bool,
) -> TestResult<SessionFrame> {
    for _ in 0..64 {
        let frame = tokio::time::timeout(WAIT, source.next())
            .await??
            .ok_or("stream ended")?;
        if pick(&frame.frame) {
            return Ok(frame.frame);
        }
    }
    Err("frame not seen".into())
}

async fn wait_in_flight(host: &SessionHost, expected: usize) -> TestResult {
    for _ in 0..500 {
        if host.tool_host().in_flight() == expected {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    Err("in-flight count never reached".into())
}

fn is_error(frame: &ToolCallResultFrame, needle: &str) -> bool {
    matches!(&frame.result, ToolCallResult::Error { message } if message.contains(needle))
}

// ---------------------------------------------------------------------------
// TG-01..TG-10
// ---------------------------------------------------------------------------

/// TG-01: a human caller never reaches tools; a forged `tool_call` cap on a
/// human identity is a listener bug that fails validation.
#[tokio::test]
async fn tg01_human_identity_is_never_admitted_to_tools() -> TestResult {
    let fx = fixture()?;
    let mut forged = human_identity(1, PermissionTier::Owner, None);
    forged.caps = forged.caps.with(ClientCaps::TOOL_CALL);
    assert!(forged.validate().is_err());
    assert!(matches!(fx.host.connect(forged), Err(HostError::Denied(_))));

    let human = connect(&fx.host, human_identity(2, PermissionTier::Owner, None)).await?;
    let session = human.create(CreateParams::default()).await?.session_id;
    assert!(matches!(
        human.call_tool(call(&session, "c-1", "fs.read")?).await,
        Err(PortError::Denied(_))
    ));
    assert!(matches!(
        human
            .list_tools(ToolListParams {
                session_id: session
            })
            .await,
        Err(PortError::Denied(_))
    ));
    assert_eq!(fx.fast.count(), 0);
    Ok(())
}

/// TG-02: the UIA calls a granted tool: result frame with gateway
/// placement, events in the session stream, session-derived sandbox.
#[tokio::test]
async fn tg02_uia_call_runs_in_the_gateway_and_streams_events() -> TestResult {
    let fx = fixture()?;
    let uia = agent(&fx.host, "cred-uia").await?;
    let session = uia.create(CreateParams::default()).await?.session_id;
    let observer = connect(&fx.host, human_identity(3, PermissionTier::Observer, None)).await?;
    let mut frames = attach(&observer, &session).await?;

    let listed = uia
        .list_tools(ToolListParams {
            session_id: session.clone(),
        })
        .await?;
    let names: Vec<&str> = listed.tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, vec!["danger.op", "fs.read", "slow.op"]);

    let frame = uia.call_tool(call(&session, "c-1", "fs.read")?).await?;
    let gateway = ToolPlacement::Gateway {
        node: Some("gw-1".into()),
    };
    assert_eq!(frame.placement, gateway);
    assert_eq!(frame.trust, ResultTrust::Untrusted);
    let ToolCallResult::Success { value } = &frame.result else {
        return Err(format!("expected success, got {:?}", frame.result).into());
    };
    assert_eq!(value["workspace"], json!("w"));
    assert_eq!(value["permissions"], json!(2));
    assert_eq!(fx.fast.count(), 1);

    wait_for(&mut frames, |f| {
        matches!(f, SessionFrame::Turn(TurnEvent::ToolCallRequested { tool_name, .. }) if tool_name == "fs.read")
    })
    .await?;
    let completed = wait_for(&mut frames, |f| {
        matches!(f, SessionFrame::Turn(TurnEvent::ToolCallCompleted { .. }))
    })
    .await?;
    let SessionFrame::Turn(TurnEvent::ToolCallCompleted { placement, .. }) = completed else {
        return Err("expected completion".into());
    };
    assert_eq!(placement, Some(gateway));
    assert_eq!(fx.host.tool_host().in_flight(), 0);
    Ok(())
}

/// TG-03: unknown tool, tool outside the grant, foreign-tenant session.
#[tokio::test]
async fn tg03_unknown_ungranted_and_foreign_sessions_are_refused() -> TestResult {
    let fx = fixture()?;
    let uia = agent(&fx.host, "cred-uia").await?;
    let session = uia.create(CreateParams::default()).await?.session_id;
    assert_eq!(
        refusal(uia.call_tool(call(&session, "c-1", "fs.nope")?).await),
        Some(ToolRefusal::UnknownTool)
    );
    assert_eq!(
        refusal(uia.call_tool(call(&session, "c-2", "fs.write")?).await),
        Some(ToolRefusal::NotGranted)
    );

    let a = TenantId::try_from_str("tenant-a")?;
    let b = TenantId::try_from_str("tenant-b")?;
    fx.host.agents().enroll_uia(
        AgentCredential::new("cred-b")?,
        "agent:uia-b",
        Some(b),
        &ToolGrant::from_names(["fs.read"]),
    )?;
    let alice = connect(
        &fx.host,
        human_identity(4, PermissionTier::Operator, Some(a)),
    )
    .await?;
    let foreign = alice.create(CreateParams::default()).await?.session_id;
    let uia_b = agent(&fx.host, "cred-b").await?;
    assert!(matches!(
        uia_b.call_tool(call(&foreign, "c-3", "fs.read")?).await,
        Err(PortError::NotFound)
    ));
    assert_eq!(fx.fast.count(), 0);
    Ok(())
}

/// TG-04: without a sandbox backend every call is refused and the executor
/// never runs.
#[tokio::test]
async fn tg04_no_sandbox_backend_refuses_without_running() -> TestResult {
    let fx = fixture_with(false)?;
    let uia = agent(&fx.host, "cred-uia").await?;
    let session = uia.create(CreateParams::default()).await?.session_id;
    assert_eq!(
        refusal(uia.call_tool(call(&session, "c-1", "fs.read")?).await),
        Some(ToolRefusal::SandboxUnavailable)
    );
    assert_eq!(fx.fast.count(), 0);
    assert!(!fx.host.tool_host().sandbox_available());

    // The W00 host without a tool host refuses the same way.
    let dir = tempfile::tempdir()?;
    let plain = SessionHost::open_with_tools(
        HostConfig::new(dir.path().to_path_buf()),
        Arc::new(IdleDriver),
        Arc::new(MemoryTranscripts::new()),
        Arc::new(MemoryApprovals::new()),
        ToolHost::builder(Arc::new(NoGatewaySandbox))
            .tool(
                ToolRegistration::new(
                    spec("fs.read"),
                    Arc::clone(&fx.fast) as Arc<dyn ToolExecutor>,
                )
                .approval(ToolApproval::Never),
            )
            .build()?,
    )?;
    plain.agents().enroll_uia(
        AgentCredential::new("cred-uia")?,
        "agent:uia",
        None,
        &ToolGrant::from_names(["fs.read"]),
    )?;
    let uia = agent(&plain, "cred-uia").await?;
    let session = uia.create(CreateParams::default()).await?.session_id;
    assert_eq!(
        refusal(uia.call_tool(call(&session, "c-1", "fs.read")?).await),
        Some(ToolRefusal::SandboxUnavailable)
    );
    assert_eq!(fx.fast.count(), 0);
    Ok(())
}

/// TG-05: draining refuses new calls; the in-flight call completes.
#[tokio::test]
async fn tg05_draining_refuses_new_calls_and_lets_in_flight_finish() -> TestResult {
    let fx = fixture()?;
    let uia = Arc::new(agent(&fx.host, "cred-uia").await?);
    let session = uia.create(CreateParams::default()).await?.session_id;
    let running = spawn_call(&uia, call(&session, "c-1", "slow.op")?);
    fx.slow.wait_entered().await?;

    let owner = connect(&fx.host, human_identity(5, PermissionTier::Owner, None)).await?;
    let status = owner
        .drain(GatewayDrainParams {
            retry_after_ms: 100,
        })
        .await?;
    assert!(status.draining);
    assert_eq!(
        refusal(uia.call_tool(call(&session, "c-2", "fs.read")?).await),
        Some(ToolRefusal::Draining)
    );
    fx.slow.release();
    let frame = tokio::time::timeout(WAIT, running).await???;
    assert!(frame.result.is_success(), "{frame:?}");
    assert_eq!(fx.fast.count(), 0);
    Ok(())
}

/// TG-06: a `call_id` already in flight in the session is a duplicate.
#[tokio::test]
async fn tg06_duplicate_call_id_in_flight_is_refused() -> TestResult {
    let fx = fixture()?;
    let uia = Arc::new(agent(&fx.host, "cred-uia").await?);
    let session = uia.create(CreateParams::default()).await?.session_id;
    let running = spawn_call(&uia, call(&session, "c-1", "slow.op")?);
    fx.slow.wait_entered().await?;
    assert_eq!(
        refusal(uia.call_tool(call(&session, "c-1", "fs.read")?).await),
        Some(ToolRefusal::DuplicateCall)
    );
    fx.slow.release();
    tokio::time::timeout(WAIT, running).await???;
    // Once finished, the id is free again.
    assert!(
        uia.call_tool(call(&session, "c-1", "fs.read")?)
            .await
            .is_ok()
    );
    Ok(())
}

/// TG-07: a delegate grant outside the parent is refused; narrowing the
/// parent narrows the child for its next call; the child's sandbox is
/// narrowed along the chain.
#[tokio::test]
async fn tg07_delegation_only_narrows_and_narrowing_cascades() -> TestResult {
    let fx = fixture()?;
    let outside = fx.host.agents().delegate(
        "agent:uia",
        AgentCredential::new("cred-w0")?,
        Delegation {
            child_id: "agent:w0".into(),
            role: AgentRole::Worker,
            tools: ToolGrant::from_names(["fs.write"]),
            sandbox: None,
        },
    );
    assert!(matches!(outside, Err(HostError::Denied(_))));
    fx.host.agents().delegate(
        "agent:uia",
        AgentCredential::new("cred-w1")?,
        Delegation {
            child_id: "agent:w1".into(),
            role: AgentRole::Worker,
            tools: ToolGrant::from_names(["fs.read", "slow.op"]),
            sandbox: Some(PermissionRequest::from_permissions([
                Permission::ReadWorkspace,
            ])),
        },
    )?;
    let uia = agent(&fx.host, "cred-uia").await?;
    let session = uia.create(CreateParams::default()).await?.session_id;
    let worker = agent(&fx.host, "cred-w1").await?;
    let frame = worker.call_tool(call(&session, "c-1", "fs.read")?).await?;
    let ToolCallResult::Success { value } = &frame.result else {
        return Err(format!("expected success, got {:?}", frame.result).into());
    };
    assert_eq!(value["permissions"], json!(1), "child sandbox narrowed");

    let owner = connect(&fx.host, human_identity(6, PermissionTier::Owner, None)).await?;
    let rights = owner
        .narrow_tools(GatewayToolRightsParams {
            agent: "agent:uia".into(),
            tools: vec!["fs.read".into()],
        })
        .await?;
    assert_eq!(
        rights.tools,
        vec!["danger.op".to_owned(), "slow.op".to_owned()]
    );
    assert_eq!(
        refusal(worker.call_tool(call(&session, "c-2", "fs.read")?).await),
        Some(ToolRefusal::NotGranted)
    );
    // Granting beyond the ceiling is refused, never silently dropped.
    assert!(matches!(
        owner
            .grant_tools(GatewayToolRightsParams {
                agent: "agent:w1".into(),
                tools: vec!["fs.read".into()],
            })
            .await,
        Err(PortError::Denied(_))
    ));
    let tools = owner.tools().await?;
    let worker_rights = tools
        .grants
        .iter()
        .find(|rights| rights.agent == "agent:w1")
        .ok_or("worker rights missing")?;
    assert_eq!(worker_rights.tools, vec!["slow.op".to_owned()]);
    Ok(())
}

/// TG-08: revoking the agent principal cancels its in-flight call (error
/// result, runtime trust) and refuses the next call.
#[tokio::test]
async fn tg08_revoking_the_agent_cancels_in_flight_calls() -> TestResult {
    let fx = fixture()?;
    let uia = Arc::new(agent(&fx.host, "cred-uia").await?);
    let session = uia.create(CreateParams::default()).await?.session_id;
    let running = spawn_call(&uia, call(&session, "c-1", "slow.op")?);
    fx.slow.wait_entered().await?;
    let revoked = fx.host.revoke_agent("agent:uia")?;
    assert_eq!(revoked, vec!["agent:uia".to_owned()]);
    let frame = tokio::time::timeout(WAIT, running).await???;
    assert!(is_error(&frame, "revoked"), "{frame:?}");
    assert_eq!(frame.trust, ResultTrust::Runtime);
    assert!(matches!(
        uia.call_tool(call(&session, "c-2", "fs.read")?).await,
        Err(PortError::Revoked)
    ));
    assert!(agent_identity(&fx.host, "cred-uia").is_err());
    Ok(())
}

/// TG-09: an `always` tool asks; a denial ends the call with an error and
/// the executor never runs; an approval runs it.
#[tokio::test]
async fn tg09_denied_approval_never_runs_the_tool() -> TestResult {
    let fx = fixture()?;
    let uia = Arc::new(agent(&fx.host, "cred-uia").await?);
    let session = uia.create(CreateParams::default()).await?.session_id;
    let human = connect(&fx.host, human_identity(7, PermissionTier::Operator, None)).await?;
    let mut frames = attach(&human, &session).await?;

    for (call_id, decision) in [
        ("c-1", ReviewDecision::Rejected),
        ("c-2", ReviewDecision::Approved),
    ] {
        let running = spawn_call(&uia, call(&session, call_id, "danger.op")?);
        let requested = wait_for(&mut frames, |f| {
            matches!(f, SessionFrame::ApprovalRequested(_))
        })
        .await?;
        let SessionFrame::ApprovalRequested(request) = requested else {
            return Err("expected approval request".into());
        };
        // The calling agent can never approve its own call.
        assert!(matches!(
            uia.respond(ApprovalRespondParams {
                request_id: request.id.clone(),
                decision: ReviewDecision::Approved,
                reason: None,
            })
            .await,
            Err(PortError::Denied(_))
        ));
        let answer = human
            .respond(ApprovalRespondParams {
                request_id: request.id,
                decision,
                reason: None,
            })
            .await?;
        assert_eq!(answer, RespondResult::Resolved);
        let frame = tokio::time::timeout(WAIT, running).await???;
        match decision {
            ReviewDecision::Rejected => {
                assert!(is_error(&frame, "denied"), "{frame:?}");
                assert_eq!(frame.trust, ResultTrust::Runtime);
                assert_eq!(fx.danger.count(), 0);
            }
            _ => {
                assert!(frame.result.is_success(), "{frame:?}");
                assert_eq!(fx.danger.count(), 1);
            }
        }
    }
    Ok(())
}

/// TG-10: a minor-1 hello never receives an R18 cap; a minor-2 agent hello
/// receives `tool_call`.
#[tokio::test]
async fn tg10_r18_caps_follow_the_negotiated_wire_minor() -> TestResult {
    let fx = fixture()?;
    let old = fx.host.connect(agent_identity(&fx.host, "cred-uia")?)?;
    let granted = hello(&old, 1).await?;
    assert!(!granted.tool_call);
    let json = serde_json::to_value(granted)?;
    assert!(json.get("tool_call").is_none());
    let session = old.create(CreateParams::default()).await?.session_id;
    assert!(matches!(
        old.call_tool(call(&session, "c-1", "fs.read")?).await,
        Err(PortError::Denied(_))
    ));

    let new = fx.host.connect(agent_identity(&fx.host, "cred-uia")?)?;
    let ack = new
        .hello(HelloParams {
            client_label: "agent".into(),
            wire_minor: 2,
            features: vec!["tools".into()],
            requested_caps: None,
        })
        .await?;
    assert!(ack.granted.tool_call);
    assert_eq!(ack.features, vec!["tools".to_owned()]);
    Ok(())
}

// ---------------------------------------------------------------------------
// Gateway port and tool.cancel
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tool_cancel_only_cancels_own_calls() -> TestResult {
    let fx = fixture()?;
    let uia = Arc::new(agent(&fx.host, "cred-uia").await?);
    let session = uia.create(CreateParams::default()).await?.session_id;
    fx.host.agents().delegate(
        "agent:uia",
        AgentCredential::new("cred-w1")?,
        Delegation {
            child_id: "agent:w1".into(),
            role: AgentRole::Worker,
            tools: ToolGrant::from_names(["fs.read"]),
            sandbox: None,
        },
    )?;
    let worker = agent(&fx.host, "cred-w1").await?;
    let running = spawn_call(&uia, call(&session, "c-1", "slow.op")?);
    fx.slow.wait_entered().await?;
    let cancel = ToolCancelParams {
        session_id: session.clone(),
        call_id: ToolCallId::try_from_str("c-1")?,
    };
    // Foreign and unknown calls: Ok, nothing happens.
    worker.cancel_tool(cancel.clone()).await?;
    uia.cancel_tool(ToolCancelParams {
        session_id: session.clone(),
        call_id: ToolCallId::try_from_str("nope")?,
    })
    .await?;
    assert_eq!(fx.host.tool_host().in_flight(), 1);
    uia.cancel_tool(cancel).await?;
    let frame = tokio::time::timeout(WAIT, running).await???;
    assert!(is_error(&frame, "cancelled"), "{frame:?}");
    assert_eq!(frame.trust, ResultTrust::Runtime);
    wait_in_flight(&fx.host, 0).await?;
    Ok(())
}

#[tokio::test]
async fn gateway_reads_and_revocation_are_admitted_and_tenant_filtered() -> TestResult {
    let fx = fixture()?;
    let uia = Arc::new(agent(&fx.host, "cred-uia").await?);
    let session = uia.create(CreateParams::default()).await?.session_id;
    fx.host.register_listener(GatewayListenerInfo {
        name: "local".into(),
        kind: ListenerKind::LocalUds,
        address: "/run/harw/session.sock".into(),
        enabled: true,
    });

    // Operators hold no gateway caps (TG-12 at the host).
    let operator = connect(&fx.host, human_identity(8, PermissionTier::Operator, None)).await?;
    assert!(matches!(operator.status().await, Err(PortError::Denied(_))));
    let maintainer = connect(
        &fx.host,
        human_identity(9, PermissionTier::Maintainer, None),
    )
    .await?;
    let status = maintainer.status().await?;
    assert!(status.sandbox_available);
    assert_eq!(status.tools, 4);
    assert_eq!(status.node.as_deref(), Some("gw-1"));
    assert_eq!(status.listeners, 1);
    assert!(matches!(
        maintainer
            .drain(GatewayDrainParams { retry_after_ms: 1 })
            .await,
        Err(PortError::Denied(_))
    ));
    let connections = maintainer.connections().await?;
    let uia_id = uia.identity().connection.0;
    assert!(
        connections.connections.iter().any(
            |info| info.connection == uia_id && info.granted.is_some_and(|caps| caps.tool_call)
        )
    );

    // A scoped owner sees neither the unscoped connections nor the agents.
    let scoped = connect(
        &fx.host,
        human_identity(
            10,
            PermissionTier::Owner,
            Some(TenantId::try_from_str("t")?),
        ),
    )
    .await?;
    assert!(
        scoped
            .connections()
            .await?
            .connections
            .iter()
            .all(|info| info.tenant.is_some())
    );
    assert!(scoped.tools().await?.grants.is_empty());
    assert!(matches!(
        scoped
            .revoke_connection(GatewayRevokeParams {
                connection: uia_id,
                reason: "test".into(),
            })
            .await,
        Err(PortError::NotFound)
    ));
    assert!(matches!(
        scoped
            .set_listener(GatewayListenerSetParams {
                name: "local".into(),
                enabled: false,
            })
            .await,
        Err(PortError::Denied(_))
    ));

    // The unscoped owner revokes the agent's connection mid-call.
    let owner = connect(&fx.host, human_identity(11, PermissionTier::Owner, None)).await?;
    let listener = owner
        .set_listener(GatewayListenerSetParams {
            name: "local".into(),
            enabled: false,
        })
        .await?;
    assert!(!listener.enabled);
    assert_eq!(fx.host.listener_enabled("local"), Some(false));
    assert!(matches!(
        owner
            .set_listener(GatewayListenerSetParams {
                name: "nope".into(),
                enabled: true,
            })
            .await,
        Err(PortError::NotFound)
    ));
    let running = spawn_call(&uia, call(&session, "c-1", "slow.op")?);
    fx.slow.wait_entered().await?;
    let result = owner
        .revoke_connection(GatewayRevokeParams {
            connection: uia_id,
            reason: "test".into(),
        })
        .await?;
    assert!(result.revoked);
    let frame = tokio::time::timeout(WAIT, running).await???;
    assert!(is_error(&frame, "revoked"), "{frame:?}");
    assert!(matches!(
        uia.call_tool(call(&session, "c-2", "fs.read")?).await,
        Err(PortError::Revoked)
    ));
    let again = owner
        .revoke_connection(GatewayRevokeParams {
            connection: uia_id,
            reason: "test".into(),
        })
        .await?;
    assert!(!again.revoked);
    assert!(
        owner
            .sessions()
            .await?
            .iter()
            .any(|s| s.session_id == session)
    );
    Ok(())
}

#[tokio::test]
async fn unregistered_or_mismatched_agents_cannot_connect() -> TestResult {
    let fx = fixture()?;
    let mut identity = agent_identity(&fx.host, "cred-uia")?;
    if let Some(agent) = identity.agent.as_mut() {
        agent.id = "agent:ghost".into();
    }
    assert!(matches!(
        fx.host.connect(identity),
        Err(HostError::Denied(_))
    ));
    let mut wrong_tenant = agent_identity(&fx.host, "cred-uia")?;
    wrong_tenant.tenant = Some(TenantId::try_from_str("other")?);
    assert!(matches!(
        fx.host.connect(wrong_tenant),
        Err(HostError::Denied(_))
    ));
    Ok(())
}

// ---------------------------------------------------------------------------
// Session binding (R18 §4: same tenant is not enough)
// ---------------------------------------------------------------------------

/// A second UIA of the same tenant must not reach a session (and with it
/// the workspace sandbox) it neither created nor was bound to: `tool.call`,
/// `tool.list` and `tool.cancel` answer exactly like an unknown session and
/// the executor never runs. The creator's delegates keep access; an
/// explicit composition binding admits the other agent.
#[tokio::test]
async fn tool_call_into_a_foreign_session_of_the_same_tenant_is_refused() -> TestResult {
    let fx = fixture()?;
    fx.host.agents().enroll_uia(
        AgentCredential::new("cred-uia-2")?,
        "agent:uia-2",
        None,
        &ToolGrant::from_names(["fs.read"]),
    )?;
    fx.host.agents().delegate(
        "agent:uia",
        AgentCredential::new("cred-w1")?,
        Delegation {
            child_id: "agent:w1".into(),
            role: AgentRole::Worker,
            tools: ToolGrant::from_names(["fs.read"]),
            sandbox: None,
        },
    )?;
    let uia = agent(&fx.host, "cred-uia").await?;
    let other = agent(&fx.host, "cred-uia-2").await?;
    let session = uia
        .create(CreateParams {
            workspace: Some("w".into()),
            title: None,
        })
        .await?
        .session_id;

    // Same tenant, not bound: indistinguishable from an unknown session.
    let unknown = SessionId::new();
    for target in [&session, &unknown] {
        assert!(matches!(
            other.call_tool(call(target, "c-1", "fs.read")?).await,
            Err(PortError::NotFound)
        ));
        assert!(matches!(
            other
                .list_tools(ToolListParams {
                    session_id: target.clone()
                })
                .await,
            Err(PortError::NotFound)
        ));
        assert!(matches!(
            other
                .cancel_tool(ToolCancelParams {
                    session_id: target.clone(),
                    call_id: ToolCallId::try_from_str("c-1")?,
                })
                .await,
            Err(PortError::NotFound)
        ));
    }
    assert_eq!(fx.fast.count(), 0);

    // The creator and its delegate keep access.
    uia.call_tool(call(&session, "c-2", "fs.read")?).await?;
    let worker = agent(&fx.host, "cred-w1").await?;
    worker.call_tool(call(&session, "c-3", "fs.read")?).await?;
    assert_eq!(fx.fast.count(), 2);

    // A human-created session admits no agent until the composition binds
    // one; the binding also admits that agent's delegates.
    let human = connect(&fx.host, human_identity(12, PermissionTier::Operator, None)).await?;
    let hosted = human.create(CreateParams::default()).await?.session_id;
    assert!(matches!(
        uia.call_tool(call(&hosted, "c-4", "fs.read")?).await,
        Err(PortError::NotFound)
    ));
    fx.host.bind_tool_agent(&hosted, "agent:uia")?;
    uia.call_tool(call(&hosted, "c-5", "fs.read")?).await?;
    worker.call_tool(call(&hosted, "c-6", "fs.read")?).await?;
    assert!(matches!(
        other.call_tool(call(&hosted, "c-7", "fs.read")?).await,
        Err(PortError::NotFound)
    ));
    assert_eq!(fx.fast.count(), 4);

    // Binding across tenants is refused; unknown agents and sessions too.
    fx.host.agents().enroll_uia(
        AgentCredential::new("cred-t")?,
        "agent:uia-t",
        Some(TenantId::try_from_str("tenant-t")?),
        &ToolGrant::from_names(["fs.read"]),
    )?;
    assert!(matches!(
        fx.host.bind_tool_agent(&hosted, "agent:uia-t"),
        Err(HostError::Denied(_))
    ));
    assert!(matches!(
        fx.host.bind_tool_agent(&hosted, "agent:ghost"),
        Err(HostError::NotFound)
    ));
    assert!(matches!(
        fx.host.bind_tool_agent(&unknown, "agent:uia"),
        Err(HostError::NotFound)
    ));
    Ok(())
}
