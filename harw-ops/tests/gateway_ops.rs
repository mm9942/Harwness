//! R18 P3 — `gateway.*` operations (contract
//! `docs/planning/65-cloud-sessions/contracts/R18-tool-gateway.md` §6, §12).
//!
//! - GO-01 read ops declare `ModelTool { readonly: true, approval: None }`.
//! - GO-02 mutating ops declare `approval: Always` on every model/web surface
//!   and are never auto-approved by the default approval policy.
//! - GO-03 no `infra.*` op (in particular `infra.auth.keys.*`) has a model
//!   surface, also with the gateway surface registered next to it.
//! - GO-04 without a gateway port every port-backed op answers
//!   `NotAvailable`; `gateway.health`/`gateway.logs` answer `NotAvailable`
//!   without the bound harw home; `gateway.channels.*` work without a port
//!   (F1: asked exactly when no gateway session is connected) and answer
//!   `NotAvailable` only without the channel configuration.
//! - GO-05 outputs contain no token/key-shaped strings (fixture with
//!   secrets in every gateway-provided text field and in a log file).
//! - GO-06 every `gateway.*` description tells the model not to shell out to
//!   `harw`; the diagnostics also say not to use ps/ls/tail.
//!
//! Plus the coordinator addendum: `gateway.health` finds a stale regular
//! file where a socket belongs, `gateway.logs` folds repeated lines.

use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
use harw_extension_api::{ToolCall, ToolName};
use harw_operations::registry::OperationRegistry;
use harw_operations::{
    ApprovalPolicy, OpContext, OpError, OpInput, OpOutput, Operation, ServiceMap, Surface,
    WebMethod,
};
use harw_ops::gateway_ops::{
    GATEWAY_DIAGNOSTICS_OPS, GATEWAY_HOME_DIAGNOSTICS_OPS, GATEWAY_MUTATION_OPS, GATEWAY_READ_OPS,
    SHELL_DIAGNOSTICS_HINT, SHELL_HINT, register_gateway, register_gateway_diagnostics,
};
use harw_protocol::session_wire::{
    GatewayConnectionInfo, GatewayConnectionsResult, GatewayDrainParams, GatewayListenerInfo,
    GatewayListenerSetParams, GatewayListenersResult, GatewayRevokeParams, GatewayRevokeResult,
    GatewayStatus, GatewayToolRights, GatewayToolRightsParams, GatewayToolsResult, HostedState,
    ListenerKind, PrincipalSummary, SessionSummary,
};
use harw_protocol::{
    AgentRole, ClientCaps, GatewayPort, PortError, PortFuture, ToolApproval, ToolDescriptor,
    ToolPlacement,
};
use harw_registry_defaults::{AUTO_APPROVED_TOOLS, DefaultApprovalPolicy};
use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
use serde_json::{Value, json};

// ── Test error type (no unwrap/expect/panic) ─────────────────────────────────

#[derive(Debug)]
enum TestError {
    Missing(&'static str),
    Unexpected(String),
    Context {
        context: &'static str,
        source: String,
    },
}

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(what) => write!(f, "missing: {what}"),
            Self::Unexpected(message) => write!(f, "unexpected: {message}"),
            Self::Context { context, source } => write!(f, "{context}: {source}"),
        }
    }
}

impl std::error::Error for TestError {}

type TestResult<T = ()> = Result<T, TestError>;

fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}

// ── Fixture: secrets and a fake gateway port ─────────────────────────────────

/// Token-shaped strings planted in every gateway-provided text field.
const SECRETS: [&str; 5] = [
    "ghp_abcdefghijklmnopqrstuvwxyz0123",
    "sk-abcdefghijklmnopqrstuvwxyz",
    "AKIAABCDEFGHIJKLMNOPQRST",
    "xoxb-1234567890-abcdefghij",
    "bearerSecretValue0123456789abcdef",
];

fn planted(text: &str) -> String {
    format!(
        "{text} ghp_abcdefghijklmnopqrstuvwxyz0123 token=sk-abcdefghijklmnopqrstuvwxyz \
         AKIAABCDEFGHIJKLMNOPQRST xoxb-1234567890-abcdefghij \
         Bearer bearerSecretValue0123456789abcdef"
    )
}

fn timestamp() -> jiff::Timestamp {
    jiff::Timestamp::UNIX_EPOCH
}

fn status() -> GatewayStatus {
    GatewayStatus {
        host_epoch: 7,
        node: Some(planted("node-a")),
        draining: false,
        connections: 1,
        sessions: 1,
        running_turns: 0,
        listeners: 1,
        tools: 1,
        sandbox_available: true,
    }
}

fn listener() -> GatewayListenerInfo {
    GatewayListenerInfo {
        name: planted("uds"),
        kind: ListenerKind::LocalUds,
        address: planted("/run/harw/gw.sock?"),
        enabled: true,
    }
}

fn rights() -> GatewayToolRights {
    GatewayToolRights {
        agent: planted("uia-1"),
        role: AgentRole::UserInterface,
        tools: vec![planted("fs.read")],
    }
}

/// A gateway port that answers with the secret-laden fixture and records
/// every call.
#[derive(Default)]
struct FakePort {
    calls: Mutex<Vec<String>>,
}

impl FakePort {
    fn record(&self, call: String) {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(call);
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls
            .lock()
            .map(|calls| calls.clone())
            .unwrap_or_default()
    }
}

fn ready<'a, T: Send + 'a>(value: T) -> PortFuture<'a, T> {
    Box::pin(std::future::ready(Ok::<T, PortError>(value)))
}

impl GatewayPort for FakePort {
    fn status(&self) -> PortFuture<'_, GatewayStatus> {
        self.record("status".to_owned());
        ready(status())
    }

    fn connections(&self) -> PortFuture<'_, GatewayConnectionsResult> {
        self.record("connections".to_owned());
        ready(GatewayConnectionsResult {
            connections: vec![
                GatewayConnectionInfo {
                    connection: 3,
                    label: planted("phone"),
                    principal: PrincipalSummary::Agent {
                        agent: planted("uia-1"),
                        role: AgentRole::UserInterface,
                        parent: Some(planted("root")),
                    },
                    tenant: Some(TenantId::from_str("t")),
                    granted: Some(ClientCaps::OPERATE),
                    attached: 1,
                    since: timestamp(),
                },
                GatewayConnectionInfo {
                    connection: 4,
                    label: planted("tablet"),
                    principal: PrincipalSummary::Device { device: None },
                    tenant: None,
                    granted: None,
                    attached: 0,
                    since: timestamp(),
                },
            ],
        })
    }

    fn sessions(&self) -> PortFuture<'_, Vec<SessionSummary>> {
        self.record("sessions".to_owned());
        ready(vec![SessionSummary {
            session_id: SessionId::new(),
            title: Some(planted("title")),
            tenant: Some(TenantId::from_str("t")),
            state: HostedState::Queued { depth: 2 },
            attached: 1,
            updated_at: timestamp(),
            model: Some(planted("model")),
        }])
    }

    fn listeners(&self) -> PortFuture<'_, GatewayListenersResult> {
        self.record("listeners".to_owned());
        ready(GatewayListenersResult {
            listeners: vec![listener()],
        })
    }

    fn tools(&self) -> PortFuture<'_, GatewayToolsResult> {
        self.record("tools".to_owned());
        ready(GatewayToolsResult {
            tools: vec![ToolDescriptor {
                name: planted("fs.read"),
                description: planted("Reads a file."),
                input_schema: json!({ "type": "object" }),
                approval: ToolApproval::Never,
                placement: ToolPlacement::Gateway {
                    node: Some(planted("node-a")),
                },
                parallel_safe: true,
            }],
            grants: vec![rights()],
        })
    }

    fn revoke_connection(
        &self,
        params: GatewayRevokeParams,
    ) -> PortFuture<'_, GatewayRevokeResult> {
        self.record(format!("revoke {} {}", params.connection, params.reason));
        ready(GatewayRevokeResult { revoked: true })
    }

    fn drain(&self, params: GatewayDrainParams) -> PortFuture<'_, GatewayStatus> {
        self.record(format!("drain {}", params.retry_after_ms));
        ready(GatewayStatus {
            draining: true,
            ..status()
        })
    }

    fn set_listener(
        &self,
        params: GatewayListenerSetParams,
    ) -> PortFuture<'_, GatewayListenerInfo> {
        self.record(format!("set_listener {} {}", params.name, params.enabled));
        ready(listener())
    }

    fn grant_tools(&self, params: GatewayToolRightsParams) -> PortFuture<'_, GatewayToolRights> {
        self.record(format!("grant {} {}", params.agent, params.tools.join(",")));
        ready(rights())
    }

    fn narrow_tools(&self, params: GatewayToolRightsParams) -> PortFuture<'_, GatewayToolRights> {
        self.record(format!(
            "narrow {} {}",
            params.agent,
            params.tools.join(",")
        ));
        ready(rights())
    }
}

// ── Context helpers ──────────────────────────────────────────────────────────

fn sandbox(root: &Path) -> TestResult<SandboxSpec> {
    std::fs::create_dir_all(root.join("ws")).map_err(ctx("create workspace"))?;
    let registry = WorkspaceRegistry::build(
        root,
        [WorkspaceRegistration {
            tenant: TenantId::from_str("t"),
            workspace: WorkspaceId::from_str("ws"),
            root: "ws".into(),
        }],
    )
    .map_err(ctx("workspace registry"))?;
    let binding = registry
        .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("ws"))
        .map_err(ctx("resolve workspace"))?;
    Ok(SandboxSpec::from_resolved(binding, PermissionSet::empty()))
}

fn home_context(home: &Path) -> TestResult<Arc<harw_home::ResolvedHomeContext>> {
    let project_dir = home.join("project");
    let project = harw_home::ProjectRoot {
        root: project_dir.clone(),
        trust_key: project_dir,
        kind: harw_home::ProjectKind::Directory,
    };
    Ok(Arc::new(
        harw_home::ResolvedHomeContext::new(home, "default".to_owned(), project)
            .map_err(ctx("ResolvedHomeContext::new"))?,
    ))
}

struct Services {
    port: Option<Arc<FakePort>>,
    home: Option<Arc<harw_home::ResolvedHomeContext>>,
}

fn op_context(root: &Path, services: &Services) -> TestResult<OpContext> {
    op_context_with(root, services, true)
}

fn op_context_with(root: &Path, services: &Services, with_config: bool) -> TestResult<OpContext> {
    let mut map = ServiceMap::new();
    if let Some(port) = &services.port {
        let port: Arc<dyn GatewayPort> = Arc::clone(port) as Arc<dyn GatewayPort>;
        map.insert(port);
    }
    if let Some(home) = &services.home {
        map.insert(Arc::clone(home));
    }
    if with_config {
        map.insert(Arc::new(harw_config::ResolvedConfig::default()));
    }
    Ok(OpContext::new(
        SessionId::new(),
        TurnId::new(),
        sandbox(root)?,
        map,
    ))
}

fn gateway_registry() -> TestResult<OperationRegistry> {
    let mut registry = OperationRegistry::new();
    register_gateway(&mut registry).map_err(ctx("register_gateway"))?;
    register_gateway_diagnostics(&mut registry).map_err(ctx("register_gateway_diagnostics"))?;
    Ok(registry)
}

fn op<'a>(
    registry: &'a OperationRegistry,
    name: &'static str,
) -> TestResult<&'a Arc<dyn Operation>> {
    registry.find_by_name(name).ok_or(TestError::Missing(name))
}

/// Valid model-tool arguments for every operation.
fn valid_args(name: &str) -> Value {
    match name {
        "gateway.connections.revoke" => json!({ "connection": 3, "reason": "lost phone" }),
        "gateway.drain" => json!({ "retry_after_ms": 5000 }),
        "gateway.listeners.set" => json!({ "name": "uds", "enabled": false }),
        "gateway.tools.grant" | "gateway.tools.narrow" => {
            json!({ "agent": "uia-1", "tools": ["fs.read"] })
        }
        "gateway.logs" => json!({ "lines": 10 }),
        _ => Value::Null,
    }
}

fn model_call(name: &str) -> ToolCall {
    ToolCall {
        id: Default::default(),
        name: ToolName::new(name),
        arguments: Default::default(),
    }
}

fn assert_no_secret(name: &str, output: &OpOutput) -> TestResult {
    let data = output
        .data
        .as_ref()
        .map(Value::to_string)
        .unwrap_or_default();
    for secret in SECRETS {
        if output.text.contains(secret) || data.contains(secret) {
            return Err(TestError::Unexpected(format!(
                "{name} leaks {secret}:\n{}\n{data}",
                output.text
            )));
        }
    }
    Ok(())
}

// ── GO-01 / GO-02 / GO-06: declarations ──────────────────────────────────────

#[test]
fn go01_read_ops_are_free_readonly_model_tools() -> TestResult {
    let registry = gateway_registry()?;
    for name in GATEWAY_READ_OPS.into_iter().chain(GATEWAY_DIAGNOSTICS_OPS) {
        let meta = op(&registry, name)?.meta();
        let model: Vec<&Surface> = meta
            .surfaces
            .iter()
            .filter(|surface| matches!(surface, Surface::ModelTool { .. }))
            .collect();
        assert_eq!(
            model,
            [&Surface::ModelTool {
                readonly: true,
                approval: ApprovalPolicy::None,
            }],
            "{name}"
        );
        for surface in &meta.surfaces {
            if let Surface::Web {
                method, approval, ..
            } = surface
            {
                assert_eq!(*method, WebMethod::Get, "{name}");
                assert_eq!(*approval, ApprovalPolicy::None, "{name}");
            }
        }
    }
    Ok(())
}

#[test]
fn go02_mutating_ops_always_ask_on_every_surface() -> TestResult {
    let registry = gateway_registry()?;
    for name in GATEWAY_MUTATION_OPS {
        let meta = op(&registry, name)?.meta();
        let mut model_surfaces = 0;
        let mut web_surfaces = 0;
        for surface in &meta.surfaces {
            match surface {
                Surface::ModelTool { readonly, approval } => {
                    model_surfaces += 1;
                    assert!(!readonly, "{name} must not be declared readonly");
                    assert_eq!(*approval, ApprovalPolicy::Always, "{name}");
                }
                Surface::Web {
                    method, approval, ..
                } => {
                    web_surfaces += 1;
                    assert_eq!(*method, WebMethod::Post, "{name}");
                    assert_eq!(*approval, ApprovalPolicy::Always, "{name}");
                }
                _ => {}
            }
        }
        assert_eq!(model_surfaces, 1, "{name} needs exactly one model surface");
        assert_eq!(web_surfaces, 1, "{name} needs exactly one web surface");
        assert!(
            !AUTO_APPROVED_TOOLS.contains(&name),
            "{name} must never be auto-approved"
        );
        assert!(
            DefaultApprovalPolicy::requires_explicit_approval(&model_call(name)),
            "{name} must require explicit approval"
        );
    }
    Ok(())
}

#[test]
fn go06_descriptions_steer_away_from_the_harw_binary_in_the_shell() -> TestResult {
    let registry = gateway_registry()?;
    assert_eq!(
        registry.len(),
        GATEWAY_READ_OPS.len() + GATEWAY_MUTATION_OPS.len() + GATEWAY_DIAGNOSTICS_OPS.len()
    );
    for operation in registry.iter() {
        let meta = operation.meta();
        assert!(
            meta.summary.contains(SHELL_HINT),
            "{} description lacks the shell hint: {}",
            meta.name,
            meta.summary
        );
    }
    for name in GATEWAY_HOME_DIAGNOSTICS_OPS {
        assert!(GATEWAY_DIAGNOSTICS_OPS.contains(&name), "{name}");
        let summary = op(&registry, name)?.meta().summary;
        assert!(
            summary.contains(SHELL_DIAGNOSTICS_HINT),
            "{name}: {summary}"
        );
    }
    Ok(())
}

/// The approval lists of `harw-registry-defaults` name exactly the
/// operations of this module: reads auto-approved, mutations always asking.
#[test]
fn registry_defaults_approval_lists_match_the_gateway_operations() {
    use harw_registry_defaults::ALWAYS_ASK_TOOLS;
    use harw_registry_defaults::profile::{GATEWAY_MUTATION_TOOLS, GATEWAY_READ_TOOLS};

    let mut reads: Vec<&str> = GATEWAY_READ_OPS
        .into_iter()
        .chain(GATEWAY_DIAGNOSTICS_OPS)
        .collect();
    reads.sort_unstable();
    let mut listed = GATEWAY_READ_TOOLS.to_vec();
    listed.sort_unstable();
    assert_eq!(reads, listed);

    let mut mutations = GATEWAY_MUTATION_OPS.to_vec();
    mutations.sort_unstable();
    let mut listed = GATEWAY_MUTATION_TOOLS.to_vec();
    listed.sort_unstable();
    assert_eq!(mutations, listed);

    for name in &reads {
        assert!(AUTO_APPROVED_TOOLS.contains(name), "{name}");
    }
    for name in &mutations {
        assert!(ALWAYS_ASK_TOOLS.contains(name), "{name}");
    }
}

// ── GO-03: keys stay off every model surface ─────────────────────────────────

#[test]
fn go03_no_infra_op_has_a_model_surface_next_to_the_gateway_ops() -> TestResult {
    let mut registry = OperationRegistry::new();
    harw_ops::register_all(&mut registry);
    harw_ops::infra::register_infrastructure(&mut registry)
        .map_err(ctx("register_infrastructure"))?;
    register_gateway(&mut registry).map_err(ctx("register_gateway"))?;
    register_gateway_diagnostics(&mut registry).map_err(ctx("register_gateway_diagnostics"))?;

    let mut key_ops = 0;
    for operation in registry.iter() {
        let meta = operation.meta();
        let model_reachable = meta.surfaces.iter().any(|surface| {
            matches!(
                surface,
                Surface::ModelTool { .. } | Surface::AgentTool { .. }
            )
        });
        if meta.name.starts_with("infra.auth.keys.") {
            key_ops += 1;
        }
        if meta.name.starts_with("infra.") {
            assert!(
                !model_reachable,
                "{} must not be reachable by a model",
                meta.name
            );
        }
        if model_reachable {
            assert!(
                !meta.name.contains("keys"),
                "{} looks like a key operation with a model surface",
                meta.name
            );
        }
    }
    assert!(
        key_ops >= 2,
        "the key operations must be registered for this scan"
    );
    Ok(())
}

// ── GO-04: fail closed without the services ──────────────────────────────────

#[tokio::test]
async fn go04_every_op_is_not_available_without_its_service() -> TestResult {
    let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let op_ctx = op_context(
        dir.path(),
        &Services {
            port: None,
            home: None,
        },
    )?;
    let registry = gateway_registry()?;
    for operation in registry.iter() {
        let name = operation.meta().name;
        let result = operation
            .run(&op_ctx, OpInput::model_tool(valid_args(name)))
            .await;
        let channel_op = GATEWAY_DIAGNOSTICS_OPS.contains(&name)
            && !GATEWAY_HOME_DIAGNOSTICS_OPS.contains(&name);
        match result {
            // F1: the channel reads need only the configuration, no port.
            Ok(_) if channel_op => {}
            Err(OpError::NotAvailable(reason)) if !channel_op => {
                if GATEWAY_HOME_DIAGNOSTICS_OPS.contains(&name) {
                    assert!(reason.contains("harw home"), "{name}: {reason}");
                } else {
                    assert!(
                        reason.contains("gateway not configured"),
                        "{name}: {reason}"
                    );
                }
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "{name}: unexpected answer without a gateway port and home: {other:?}"
                )));
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn go04_channel_ops_are_not_available_without_the_channel_configuration() -> TestResult {
    let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let op_ctx = op_context_with(
        dir.path(),
        &Services {
            port: None,
            home: None,
        },
        false,
    )?;
    let registry = gateway_registry()?;
    for name in ["gateway.channels.list", "gateway.channels.connect_info"] {
        let result = op(&registry, name)?
            .run(&op_ctx, OpInput::model_tool(valid_args(name)))
            .await;
        assert!(
            matches!(&result, Err(OpError::NotAvailable(reason)) if reason.contains("channel configuration")),
            "{name}: {result:?}"
        );
    }
    Ok(())
}

// ── GO-05: no secrets in any output ──────────────────────────────────────────

#[tokio::test]
async fn go05_outputs_never_contain_token_shaped_strings() -> TestResult {
    let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let home = dir.path().join("home");
    let logs = home.join("logs");
    std::fs::create_dir_all(&logs).map_err(ctx("create logs"))?;
    std::fs::write(
        logs.join("tui.log"),
        format!("WARN auth: {}\n", planted("login")),
    )
    .map_err(ctx("write tui.log"))?;
    let port = Arc::new(FakePort::default());
    let op_ctx = op_context(
        dir.path(),
        &Services {
            port: Some(Arc::clone(&port)),
            home: Some(home_context(&home)?),
        },
    )?;
    let registry = gateway_registry()?;
    for operation in registry.iter() {
        let name = operation.meta().name;
        let output = operation
            .run(&op_ctx, OpInput::model_tool(valid_args(name)))
            .await
            .map_err(|error| TestError::Unexpected(format!("{name}: {error}")))?;
        assert_no_secret(name, &output)?;
    }
    // Every port method was reached through its operation.
    let calls = port.calls();
    for expected in [
        "connections",
        "sessions",
        "listeners",
        "tools",
        "revoke 3 lost phone",
    ] {
        assert!(
            calls.iter().any(|call| call == expected),
            "{expected} missing in {calls:?}"
        );
    }
    for prefix in [
        "drain 5000",
        "set_listener uds false",
        "grant uia-1",
        "narrow uia-1",
    ] {
        assert!(
            calls.iter().any(|call| call.starts_with(prefix)),
            "{prefix} missing in {calls:?}"
        );
    }
    Ok(())
}

// ── Argument validation before any gateway call ──────────────────────────────

#[tokio::test]
async fn invalid_mutation_arguments_never_reach_the_gateway() -> TestResult {
    let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let port = Arc::new(FakePort::default());
    let op_ctx = op_context(
        dir.path(),
        &Services {
            port: Some(Arc::clone(&port)),
            home: None,
        },
    )?;
    let registry = gateway_registry()?;
    let cases = [
        ("gateway.connections.revoke", json!({ "connection": 3 })),
        ("gateway.connections.revoke", json!({ "reason": "x" })),
        (
            "gateway.connections.revoke",
            json!({ "connection": 3, "reason": "x", "principal": "root" }),
        ),
        ("gateway.listeners.set", json!({ "name": "uds" })),
        (
            "gateway.tools.grant",
            json!({ "agent": "uia-1", "tools": [] }),
        ),
        ("gateway.tools.narrow", json!({ "tools": ["fs.read"] })),
    ];
    for (name, args) in cases {
        let result = op(&registry, name)?
            .run(&op_ctx, OpInput::model_tool(args.clone()))
            .await;
        assert!(
            matches!(result, Err(OpError::InvalidArguments(_))),
            "{name} {args}: {result:?}"
        );
    }
    assert!(port.calls().is_empty(), "{:?}", port.calls());
    Ok(())
}

#[tokio::test]
async fn drain_defaults_the_retry_hint_and_reports_draining() -> TestResult {
    let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let port = Arc::new(FakePort::default());
    let op_ctx = op_context(
        dir.path(),
        &Services {
            port: Some(Arc::clone(&port)),
            home: None,
        },
    )?;
    let registry = gateway_registry()?;
    let output = op(&registry, "gateway.drain")?
        .run(&op_ctx, OpInput::model_tool(Value::Null))
        .await
        .map_err(ctx("gateway.drain"))?;
    assert!(
        output.text.contains("draining       yes"),
        "{}",
        output.text
    );
    assert_eq!(
        port.calls(),
        [format!(
            "drain {}",
            harw_ops::gateway_ops::DEFAULT_DRAIN_RETRY_AFTER_MS
        )]
    );
    Ok(())
}

// ── Coordinator addendum: health and logs ────────────────────────────────────

#[tokio::test]
async fn health_reports_a_stale_regular_file_socket_and_large_logs() -> TestResult {
    let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join("logs")).map_err(ctx("create logs"))?;
    // The field case: `web.sock` is a regular file, not a socket.
    std::fs::write(home.join("web.sock"), b"").map_err(ctx("write web.sock"))?;
    std::fs::write(home.join("logs").join("tui.log"), "INFO a: b\n")
        .map_err(ctx("write tui.log"))?;
    let op_ctx = op_context(
        dir.path(),
        &Services {
            port: None,
            home: Some(home_context(&home)?),
        },
    )?;
    let registry = gateway_registry()?;
    let output = op(&registry, "gateway.health")?
        .run(&op_ctx, OpInput::model_tool(Value::Null))
        .await
        .map_err(ctx("gateway.health"))?;
    assert!(
        output.text.contains("session gateway  not connected"),
        "{}",
        output.text
    );
    let data = output.data.ok_or(TestError::Missing("health data"))?;
    let problems = data["problems"]
        .as_array()
        .ok_or(TestError::Missing("problems"))?;
    assert!(
        problems.iter().any(|problem| problem
            .as_str()
            .is_some_and(|text| text.starts_with("web.sock is a regular file"))),
        "{problems:?}"
    );
    let web = data["sockets"]
        .as_array()
        .and_then(|sockets| sockets.iter().find(|socket| socket["name"] == "web.sock"))
        .ok_or(TestError::Missing("web.sock entry"))?;
    assert_eq!(web["kind"], "regular file");
    assert_eq!(data["logs"][0]["file"], "tui.log");
    Ok(())
}

#[tokio::test]
async fn health_includes_the_session_gateway_status_when_connected() -> TestResult {
    let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).map_err(ctx("create home"))?;
    let port = Arc::new(FakePort::default());
    let op_ctx = op_context(
        dir.path(),
        &Services {
            port: Some(Arc::clone(&port)),
            home: Some(home_context(&home)?),
        },
    )?;
    let registry = gateway_registry()?;
    let output = op(&registry, "gateway.health")?
        .run(&op_ctx, OpInput::model_tool(Value::Null))
        .await
        .map_err(ctx("gateway.health"))?;
    assert!(output.text.contains("host epoch     7"), "{}", output.text);
    assert_eq!(port.calls(), ["status"]);
    Ok(())
}

#[tokio::test]
async fn logs_fold_repeated_lines_and_bound_the_request() -> TestResult {
    let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let home = dir.path().join("home");
    let logs = home.join("logs");
    std::fs::create_dir_all(&logs).map_err(ctx("create logs"))?;
    let mut content = String::new();
    for second in 0..312 {
        content.push_str(&format!(
            "2026-09-28T10:{:02}:{:02}.000000Z  WARN harw_model_catalog: unresolved catalog \
             reference — affected entry disabled provider=\"zhipu\"\n",
            second / 60,
            second % 60
        ));
    }
    content.push_str("2026-09-28T11:00:00.000000Z  INFO harw_cli: ready\n");
    std::fs::write(logs.join("tui.log"), content).map_err(ctx("write tui.log"))?;
    let op_ctx = op_context(
        dir.path(),
        &Services {
            port: None,
            home: Some(home_context(&home)?),
        },
    )?;
    let registry = gateway_registry()?;
    let logs_op = op(&registry, "gateway.logs")?;

    let output = logs_op
        .run(&op_ctx, OpInput::model_tool(json!({ "file": "tui.log" })))
        .await
        .map_err(ctx("gateway.logs"))?;
    assert!(
        output
            .text
            .contains("×312 WARN harw_model_catalog: unresolved catalog reference"),
        "{}",
        output.text
    );

    let warn_only = logs_op
        .run(
            &op_ctx,
            OpInput::model_tool(json!({ "level": "warn", "contains": "ZHIPU" })),
        )
        .await
        .map_err(ctx("gateway.logs warn"))?;
    assert!(
        !warn_only.text.contains("harw_cli: ready"),
        "{}",
        warn_only.text
    );

    for args in [
        json!({ "lines": 0 }),
        json!({ "lines": 501 }),
        json!({ "level": "loud" }),
        json!({ "file": "../secrets.log" }),
        json!({ "file": "missing.log" }),
    ] {
        let result = logs_op
            .run(&op_ctx, OpInput::model_tool(args.clone()))
            .await;
        assert!(
            matches!(result, Err(OpError::InvalidArguments(_))),
            "{args}: {result:?}"
        );
    }
    Ok(())
}
