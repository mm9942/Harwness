//! R18 D-A end to end through a real assembly: a run bound to a gateway
//! (`RuntimeAssemblyBuilder::remote_tools`) offers exactly the gateway's
//! tools, keeps no local executor next to them, maps every refusal to an
//! error result, and the wire `AgentRole` stays pinned to `AgentRoleId`
//! (contract `docs/planning/65-cloud-sessions/contracts/R18-tool-gateway.md`
//! §12, P2: RP-T1..RP-T3, RP-T5).
//!
//! [`ModelSource::Echo`] as model — no network, no provider, no secret. The
//! gateway is an in-process `ToolPort` fake.

#[allow(
    dead_code,
    reason = "shared helper module; this test uses only part of it"
)]
mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::{TestError, TestResult, ctx};
use harw_agent_dsl::roles::AgentRoleId;
use harw_authority::{
    Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
};
use harw_core::{InMemoryStateStore, StateStore};
use harw_extension_api::contributors::ToolProvider;
use harw_extension_api::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolName, ToolOutput,
    ToolSpec,
};
use harw_protocol::items::{ResultTrust, ToolCallResult, ToolPlacement};
use harw_protocol::session_port::{PortError, PortFuture, ToolPort, ToolRefusal};
use harw_protocol::session_wire::{
    AgentRole, ToolApproval, ToolCallParams, ToolCallResultFrame, ToolCancelParams, ToolDescriptor,
    ToolListParams, ToolListResult,
};
use harw_registry_defaults::profile::{IdentityOverrides, RegistryProfile};
use harw_runtime::assembly::{RuntimeAssembly, RuntimeNarrowing, RuntimeStores};
use harw_runtime::contributors::{AssemblyContributor, AssemblyInputs, AssemblyParts};
use harw_runtime::error::RuntimeResult;
use harw_runtime::model::ModelSource;
use harw_runtime::spec::{
    EntryKind, OperationSurface, RuntimeSpec, SpawnerPolicy, agent_role_from_wire, wire_agent_role,
};
use harw_tool_remote::RemoteToolProvider;
use harw_tools::executor::ExecutionPlacement;
use harw_types::{
    IngressSurface, PermissionTier, Principal, PrincipalKind, SessionId, TenantId, ToolCallId,
    TurnId, WorkspaceId,
};
use tempfile::TempDir;

type SessionEventAlias = harw_protocol::events::SessionEvent;
type TurnEventAlias = harw_protocol::events::TurnEvent;

const SPY_TOOL: &str = "spy.local";

// ---------------------------------------------------------------------------
// Gateway fake and local spy
// ---------------------------------------------------------------------------

/// In-process gateway: lists fixed descriptors, answers every call the same.
struct FakeGateway {
    descriptors: Vec<ToolDescriptor>,
    answer: Result<(ToolCallResult, ToolPlacement), PortError>,
    calls: Mutex<Vec<ToolCallParams>>,
}

impl FakeGateway {
    fn new(answer: Result<(ToolCallResult, ToolPlacement), PortError>) -> Arc<Self> {
        Arc::new(Self {
            descriptors: vec![descriptor("fs.read"), descriptor("shell.exec")],
            answer,
            calls: Mutex::new(Vec::new()),
        })
    }

    fn call_count(&self) -> usize {
        self.calls
            .lock()
            .map(|calls| calls.len())
            .unwrap_or_default()
    }
}

impl ToolPort for FakeGateway {
    fn list_tools(&self, _params: ToolListParams) -> PortFuture<'_, ToolListResult> {
        let tools = self.descriptors.clone();
        Box::pin(async move { Ok(ToolListResult { tools }) })
    }

    fn call_tool(&self, params: ToolCallParams) -> PortFuture<'_, ToolCallResultFrame> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(params.clone());
        }
        let answer = self.answer.clone();
        Box::pin(async move {
            answer.map(|(result, placement)| ToolCallResultFrame {
                call_id: params.call_id,
                result,
                placement,
                duration_ms: 3,
                trust: ResultTrust::Untrusted,
            })
        })
    }

    fn cancel_tool(&self, _params: ToolCancelParams) -> PortFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

fn descriptor(name: &str) -> ToolDescriptor {
    ToolDescriptor {
        name: name.to_owned(),
        description: format!("{name} served by the gateway"),
        input_schema: serde_json::json!({ "type": "object" }),
        approval: ToolApproval::Policy,
        placement: ToolPlacement::Gateway {
            node: Some("gw-test".to_owned()),
        },
        parallel_safe: false,
    }
}

/// A local tool that must never be reachable next to the remote proxy.
struct SpyExecutor {
    runs: Arc<AtomicUsize>,
}

impl ToolExecutor for SpyExecutor {
    fn execute<'a>(
        &'a self,
        _context: &'a ToolExecutionContext,
        _call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(ToolOutput::text("ran locally")) })
    }
}

/// Offers `spy.local` and — as a shadow — `shell.exec`, both local.
struct SpyProvider {
    runs: Arc<AtomicUsize>,
}

impl ToolProvider for SpyProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        [SPY_TOOL, "shell.exec"]
            .into_iter()
            .map(|name| {
                ToolSpec::Function(harw_tools::FunctionToolSpec {
                    name: ToolName::new(name),
                    description: "local spy".to_owned(),
                    parameters: harw_tools::JsonSchema::default(),
                    strict: false,
                })
            })
            .collect()
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        [SPY_TOOL, "shell.exec"].contains(&name.as_str()).then(|| {
            Arc::new(SpyExecutor {
                runs: Arc::clone(&self.runs),
            }) as Arc<dyn ToolExecutor>
        })
    }
}

/// Registers the spy provider like any contributor would.
struct SpyContributor {
    runs: Arc<AtomicUsize>,
}

impl AssemblyContributor for SpyContributor {
    fn contribute(
        &self,
        _inputs: &AssemblyInputs<'_>,
        parts: &mut AssemblyParts,
    ) -> RuntimeResult<()> {
        let registry = std::mem::take(&mut parts.registry);
        parts.registry = registry.tool_provider(Arc::new(SpyProvider {
            runs: Arc::clone(&self.runs),
        }));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Fixture (same layout as `infrastructure.rs`)
// ---------------------------------------------------------------------------

struct Fixture {
    _dir: TempDir,
    home: PathBuf,
    project: PathBuf,
}

fn fixture() -> TestResult<Fixture> {
    let dir = tempfile::tempdir()?;
    let home = dir.path().join("home");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&home).map_err(ctx("home"))?;
    std::fs::create_dir_all(&project).map_err(ctx("project"))?;
    std::fs::write(project.join("Cargo.toml"), "[workspace]\n").map_err(ctx("marker"))?;
    write_profile(&home)?;
    Ok(Fixture {
        _dir: dir,
        home,
        project,
    })
}

fn write_profile(home: &Path) -> TestResult {
    let profile_dir = home.join("profiles").join("default");
    let agent_dir = profile_dir.join("agents").join("fixture-uia");
    std::fs::create_dir_all(&agent_dir).map_err(ctx("fixture uia dir"))?;
    std::fs::write(
        agent_dir.join("definition.toml"),
        "schema = \"harwness.agent/v1\"\nid = \"harwness.agent.fixture-uia@1\"\nversion = \"1.0.0\"\nrole = \"user-interface\"\nspecialization = \"terminal-ui\"\n",
    )
    .map_err(ctx("fixture uia definition"))?;
    std::fs::write(
        profile_dir.join("config.toml"),
        "active_uia_definition = \"harwness.agent.fixture-uia@1\"\n",
    )
    .map_err(ctx("fixture profile config"))
}

fn spec_for(fixture: &Fixture) -> RuntimeSpec {
    RuntimeSpec {
        entry: EntryKind::Tui,
        home: fixture.home.clone(),
        cwd: fixture.project.clone(),
        principal: Principal::trusted_ingress(
            PrincipalKind::Human,
            "remote-tools",
            IngressSurface::Tui,
            PermissionTier::Owner,
        ),
        mode_override: None,
        active_agent: None,
        reasoning_effort: None,
        approval_override: None,
        model_override: None,
        embedded: None,
        child_backend: None,
    }
}

fn gateway_session() -> SessionId {
    SessionId::from_str("gateway-bound-session")
}

async fn remote_provider(gateway: &Arc<FakeGateway>) -> TestResult<RemoteToolProvider> {
    let port: Arc<dyn ToolPort> = Arc::clone(gateway) as Arc<dyn ToolPort>;
    RemoteToolProvider::connect(port, gateway_session())
        .await
        .map_err(ctx("RemoteToolProvider::connect"))
}

fn assemble(
    fixture: &Fixture,
    remote: RemoteToolProvider,
    spy_runs: &Arc<AtomicUsize>,
) -> TestResult<(
    RuntimeAssembly,
    tokio::sync::mpsc::UnboundedSender<SessionEventAlias>,
)> {
    let (events, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEventAlias>();
    let state_store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
    let assembly = RuntimeAssembly::builder(spec_for(fixture))
        .model(ModelSource::Echo("echo: remote tools".to_owned()))
        .stores(RuntimeStores {
            state_store,
            job_store: None,
            approval_store: None,
        })
        .session_events(events.clone())
        .contributor(Arc::new(SpyContributor {
            runs: Arc::clone(spy_runs),
        }))
        .remote_tools(remote)
        .build()
        .map_err(TestError::Runtime)?;
    Ok((assembly, events))
}

fn execution_context(fixture: &Fixture) -> TestResult<ToolExecutionContext> {
    let harness_root = fixture
        .project
        .parent()
        .ok_or(TestError::Missing("the fixture project has a parent"))?;
    let registry = WorkspaceRegistry::build(
        harness_root,
        [WorkspaceRegistration {
            tenant: TenantId::from_str("t"),
            workspace: WorkspaceId::from_str("w"),
            root: PathBuf::from("project"),
        }],
    )
    .map_err(ctx("WorkspaceRegistry::build"))?;
    let binding = registry
        .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("w"))
        .map_err(ctx("registry.resolve"))?;
    Ok(ToolExecutionContext::new(
        SessionId::new(),
        TurnId::new(),
        SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        ),
    ))
}

fn shell_call() -> ToolCall {
    ToolCall {
        id: ToolCallId::new(),
        name: ToolName::new("shell.exec"),
        arguments: serde_json::json!({ "command": "echo hi" }),
    }
}

// ---------------------------------------------------------------------------
// RP-T1: exactly the gateway's tools, nothing else registered
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rp_t1_gateway_bound_run_offers_exactly_the_remote_tools() -> TestResult {
    let fixture = fixture()?;
    let gateway = FakeGateway::new(Err(PortError::Revoked));
    let spy_runs = Arc::new(AtomicUsize::new(0));
    let (assembly, _events) = assemble(&fixture, remote_provider(&gateway).await?, &spy_runs)?;

    let snapshot = assembly.rights_snapshot();

    assert_eq!(snapshot.tools, ["fs.read", "shell.exec"]);
    assert_eq!(
        assembly.profile().registry_profile,
        RegistryProfile::NoTools
    );
    assert_eq!(assembly.profile().spawner, SpawnerPolicy::None);
    assert_ne!(
        assembly.profile().operations,
        OperationSurface::AllWithModelTools
    );
    assert!(assembly.spawner().is_none());
    assert_eq!(gateway.call_count(), 0);
    Ok(())
}

#[tokio::test]
async fn rp_t1_only_a_no_tools_narrowing_is_allowed_next_to_remote_tools() -> TestResult {
    let fixture = fixture()?;
    let gateway = FakeGateway::new(Err(PortError::Revoked));
    let (events, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEventAlias>();
    let state_store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());

    let built = RuntimeAssembly::builder(spec_for(&fixture))
        .model(ModelSource::Echo("echo".to_owned()))
        .stores(RuntimeStores {
            state_store,
            job_store: None,
            approval_store: None,
        })
        .session_events(events)
        .narrowing(RuntimeNarrowing {
            registry_profile: RegistryProfile::Full,
            identity: IdentityOverrides::default(),
            permissions: PermissionSet::from_policy([Permission::ReadWorkspace]),
            workspace_root: None,
        })
        .remote_tools(remote_provider(&gateway).await?)
        .build();

    assert!(
        built.is_err(),
        "a narrowing that would widen NoTools must fail the build"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// RP-T2/RP-T3: through the root session's executors
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rp_t2_root_session_forwards_and_copies_the_gateway_placement() -> TestResult {
    let fixture = fixture()?;
    let gateway = FakeGateway::new(Ok((
        ToolCallResult::success(serde_json::json!({ "exit_code": 0 })),
        ToolPlacement::Gateway {
            node: Some("gw-test".to_owned()),
        },
    )));
    let spy_runs = Arc::new(AtomicUsize::new(0));
    let (assembly, events) = assemble(&fixture, remote_provider(&gateway).await?, &spy_runs)?;
    let (turn_events, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEventAlias>();
    let root = assembly
        .new_root_session(
            assembly.root_session_id().clone(),
            events,
            turn_events,
            None,
        )
        .map_err(ctx("root session"))?;

    let executor = harw_core::turn_loop::find_executor(&root.session, &ToolName::new("shell.exec"))
        .ok_or(TestError::Missing("remote shell.exec executor"))?;
    let placed = executor
        .execute_placed(&execution_context(&fixture)?, &shell_call())
        .await;

    assert!(
        matches!(placed.output, Ok(ToolOutput::Json { .. })),
        "{:?}",
        placed.output
    );
    assert_eq!(
        placed.placement,
        Some(ExecutionPlacement::Gateway {
            node: Some("gw-test".to_owned())
        })
    );
    assert_eq!(gateway.call_count(), 1);
    assert_eq!(
        spy_runs.load(Ordering::SeqCst),
        0,
        "the local shadow never runs"
    );
    Ok(())
}

#[tokio::test]
async fn rp_t3_refused_calls_never_reach_a_local_executor() -> TestResult {
    let refusals = ToolRefusal::ALL
        .into_iter()
        .map(|refusal| PortError::ToolRefused {
            refusal,
            detail: "shell.exec".to_owned(),
        })
        .chain([
            PortError::Denied("caps".to_owned()),
            PortError::NotFound,
            PortError::Revoked,
            PortError::Transport("closed".to_owned()),
        ]);
    for refusal in refusals {
        let fixture = fixture()?;
        let gateway = FakeGateway::new(Err(refusal.clone()));
        let spy_runs = Arc::new(AtomicUsize::new(0));
        let (assembly, events) = assemble(&fixture, remote_provider(&gateway).await?, &spy_runs)?;
        let (turn_events, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEventAlias>();
        let root = assembly
            .new_root_session(
                assembly.root_session_id().clone(),
                events,
                turn_events,
                None,
            )
            .map_err(ctx("root session"))?;

        assert!(
            harw_core::turn_loop::find_executor(&root.session, &ToolName::new(SPY_TOOL)).is_none(),
            "a local tool must not be reachable next to the remote proxy"
        );
        let executor =
            harw_core::turn_loop::find_executor(&root.session, &ToolName::new("shell.exec"))
                .ok_or(TestError::Missing("remote shell.exec executor"))?;
        let placed = executor
            .execute_placed(&execution_context(&fixture)?, &shell_call())
            .await;

        match placed.output {
            Ok(ToolOutput::Error { message }) => {
                assert_eq!(
                    message,
                    harw_tool_remote::refusal_message("shell.exec", &refusal)
                );
                assert!(message.contains("no local fallback"), "{message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "a refusal must be an error result, got {other:?}"
                )));
            }
        }
        assert_eq!(placed.placement, None);
        assert_eq!(gateway.call_count(), 1);
        assert_eq!(spy_runs.load(Ordering::SeqCst), 0, "{refusal}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// RP-T5: wire `AgentRole` ⇔ `AgentRoleId`
// ---------------------------------------------------------------------------

/// Every `AgentRoleId`; the `match` below fails to compile when one is added.
const ALL_ROLE_IDS: [AgentRoleId; 6] = [
    AgentRoleId::UserInterface,
    AgentRoleId::RootOrchestrator,
    AgentRoleId::ChildOrchestrator,
    AgentRoleId::Worker,
    AgentRoleId::UiaWorker,
    AgentRoleId::AgentSteward,
];

fn role_id_index(role: AgentRoleId) -> usize {
    match role {
        AgentRoleId::UserInterface => 0,
        AgentRoleId::RootOrchestrator => 1,
        AgentRoleId::ChildOrchestrator => 2,
        AgentRoleId::Worker => 3,
        AgentRoleId::UiaWorker => 4,
        AgentRoleId::AgentSteward => 5,
    }
}

fn wire_role_index(role: AgentRole) -> Option<usize> {
    match role {
        AgentRole::UserInterface => Some(0),
        AgentRole::RootOrchestrator => Some(1),
        AgentRole::ChildOrchestrator => Some(2),
        AgentRole::Worker => Some(3),
        AgentRole::UiaWorker => Some(4),
        AgentRole::AgentSteward => Some(5),
        AgentRole::Unknown => None,
    }
}

#[test]
fn rp_t5_wire_agent_role_is_pinned_to_agent_role_id() -> TestResult {
    for (index, role) in ALL_ROLE_IDS.into_iter().enumerate() {
        assert_eq!(role_id_index(role), index);
        let wire = wire_agent_role(role);
        assert_eq!(wire_role_index(wire), Some(index), "{role:?}");
        assert_eq!(agent_role_from_wire(wire), Some(role), "{role:?}");
        // Same wire name in both vocabularies, both directions.
        let dsl_name = serde_json::to_value(role).map_err(ctx("serialize AgentRoleId"))?;
        let wire_name = serde_json::to_value(wire).map_err(ctx("serialize AgentRole"))?;
        assert_eq!(dsl_name, wire_name, "{role:?}");
        let back: AgentRole =
            serde_json::from_value(dsl_name.clone()).map_err(ctx("decode as AgentRole"))?;
        assert_eq!(back, wire);
        let back: AgentRoleId =
            serde_json::from_value(wire_name).map_err(ctx("decode as AgentRoleId"))?;
        assert_eq!(back, role);
    }
    assert_eq!(agent_role_from_wire(AgentRole::Unknown), None);
    let unknown: AgentRole = serde_json::from_value(serde_json::json!("hub-steward"))
        .map_err(ctx("decode an unknown role"))?;
    assert_eq!(unknown, AgentRole::Unknown);
    Ok(())
}
