//! Gateway tool host (R18 D-A, §4.2, §5, §7).
//!
//! Tools execute **in the gateway**, never in an agent process: a registry
//! of [`ToolExecutor`]s, each described once as a [`ToolDescriptor`] with
//! `placement = Gateway { node }`, and a gateway-side [`SandboxSpec`] per
//! call derived from the session (workspace, tenant) and narrowed along the
//! agent's delegation chain (`child.ensure_child_of(parent)` at every hop).
//!
//! The host (`host.rs`) runs admission steps 1-7 (tenant, cap, agent
//! principal, unknown tool, grant, revocation, draining); this module adds
//! step 8 (`ToolHost::sandbox_for`: gateway sandbox available for the
//! session) and step 9 (`ToolHost::reserve`: `call_id` not in flight in
//! the session). Every refusal is a typed [`HostError::ToolRefused`]; there
//! is no fallback to host execution anywhere.
//!
//! After admission a call is a completed call in every case: argument schema
//! violations, denied or expired approvals, cancellation and executor
//! failures all end as [`ToolCallResult::Error`] so the model can react.
//! Revocation and `tool.cancel` cancel an in-flight call through its
//! [`CancelToken`]; draining does not (in-flight calls finish).
//!
//! Approvals: tools with approval `always`, `policy` or an unknown
//! requirement ask a human every time (the gateway has no policy engine of
//! its own yet, so `policy` fails closed to asking). The request is issued
//! into the session's approval store ([`ToolApprovalIssuer`], the same store
//! the host resolves `approval.respond` against, first writer wins) and the
//! call waits for the decision, bounded by the approval timeout.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistry};
use harw_protocol::session_wire::{ToolCallParams, ToolCallResultFrame};
use harw_protocol::{
    ApprovalKind, ApprovalRequest, ResultTrust, ToolApproval, ToolCallResult, ToolDescriptor,
    ToolPlacement, ToolRefusal, TurnEvent,
};
use harw_tools::{
    AdditionalProperties, JsonSchema, JsonSchemaType, ToolCall, ToolExecutionContext, ToolExecutor,
    ToolName, ToolOutput, ToolSpec, ToolsError,
};
use harw_types::cancel::{CancelReason, CancelToken};
use harw_types::{
    ApprovalId, DEFAULT_APPROVAL_TIMEOUT, ReviewDecision, RiskLevel, SessionId, TenantId,
    ToolCallId, WorkId, WorkspaceId,
};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::agents::AgentRegistry;
use crate::approvals::MemoryApprovals;
use crate::error::HostError;
use crate::identity::{ConnectionId, ToolGrant};

// ---------------------------------------------------------------------------
// Sandbox backends
// ---------------------------------------------------------------------------

/// The session a gateway sandbox is requested for.
#[derive(Clone, Copy, Debug)]
pub struct SessionScope<'a> {
    pub session_id: &'a SessionId,
    /// Tenant of the session record.
    pub tenant: Option<&'a TenantId>,
    /// Workspace of the session record, as given at `session.create`.
    pub workspace: Option<&'a str>,
}

/// Source of gateway-side sandboxes (R18 §4.2 step 8).
///
/// `None`/`false` refuses the call with
/// [`ToolRefusal::SandboxUnavailable`]; a backend never hands out host
/// authority instead.
pub trait GatewaySandbox: Send + Sync {
    /// Probe: can this gateway run sandboxed tools at all
    /// (`GatewayStatus::sandbox_available`).
    fn available(&self) -> bool;

    /// Base sandbox for tool calls of one session, before the per-agent
    /// narrowing. `None` when the session has no resolvable sandbox.
    fn session_sandbox(&self, scope: &SessionScope<'_>) -> Option<SandboxSpec>;
}

/// No sandbox backend: every `tool.call` is refused.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoGatewaySandbox;

impl GatewaySandbox for NoGatewaySandbox {
    fn available(&self) -> bool {
        false
    }

    fn session_sandbox(&self, _scope: &SessionScope<'_>) -> Option<SandboxSpec> {
        None
    }
}

/// Configuration of [`WorkspaceGatewaySandbox`].
#[derive(Clone, Debug)]
pub struct WorkspaceSandboxConfig {
    /// Configured workspaces; a session's workspace must resolve here.
    pub registry: WorkspaceRegistry,
    /// Permissions of the gateway sandbox (upper bound of every call).
    pub permissions: PermissionSet,
    /// Tenant used for unscoped (local) sessions.
    pub default_tenant: TenantId,
    /// Workspace used for sessions created without one.
    pub default_workspace: WorkspaceId,
}

/// Production backend: a workspace-bound [`SandboxSpec`] per session plus a
/// probe for the process sandbox (Bubblewrap at its pinned paths).
#[derive(Clone, Debug)]
pub struct WorkspaceGatewaySandbox {
    config: WorkspaceSandboxConfig,
    backend_available: bool,
}

impl WorkspaceGatewaySandbox {
    /// A backend with an explicit availability (tests, or a composition that
    /// probed on its own).
    #[must_use]
    pub fn new(config: WorkspaceSandboxConfig, backend_available: bool) -> Self {
        Self {
            config,
            backend_available,
        }
    }

    /// A backend whose availability is the Bubblewrap probe
    /// (`harw_sandbox::BwrapLauncher::discover`, fixed paths only).
    #[must_use]
    pub fn discover(config: WorkspaceSandboxConfig) -> Self {
        let backend_available = harw_sandbox::BwrapLauncher::discover().is_ok();
        Self::new(config, backend_available)
    }
}

impl GatewaySandbox for WorkspaceGatewaySandbox {
    fn available(&self) -> bool {
        self.backend_available
    }

    fn session_sandbox(&self, scope: &SessionScope<'_>) -> Option<SandboxSpec> {
        if !self.backend_available {
            return None;
        }
        let tenant = scope.tenant.unwrap_or(&self.config.default_tenant);
        let workspace = match scope.workspace {
            Some(name) => WorkspaceId::try_from_str(name).ok()?,
            None => self.config.default_workspace.clone(),
        };
        let binding = self.config.registry.resolve(tenant, &workspace).ok()?;
        Some(SandboxSpec::from_resolved(
            binding,
            self.config.permissions.clone(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Approvals
// ---------------------------------------------------------------------------

/// Writes a tool approval request into the session's approval store.
///
/// Must be the store the host resolves `approval.respond` against (the
/// `ApprovalBackend` passed to `SessionHost::open_with_tools`), so the
/// first-writer-wins resolution and the attach replay of pending requests
/// cover gateway tool approvals too.
pub trait ToolApprovalIssuer: Send + Sync {
    fn issue(&self, session: &SessionId, request: ApprovalRequest) -> Result<(), HostError>;
}

impl ToolApprovalIssuer for MemoryApprovals {
    fn issue(&self, session: &SessionId, request: ApprovalRequest) -> Result<(), HostError> {
        MemoryApprovals::issue(self, session, request)
    }
}

/// True when the gateway asks a human before running a tool.
/// `Policy` and `Unknown` fail closed to asking.
#[must_use]
pub const fn asks_human(approval: ToolApproval) -> bool {
    !matches!(approval, ToolApproval::Never)
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// One tool offered by the gateway tool host.
pub struct ToolRegistration {
    spec: ToolSpec,
    executor: Arc<dyn ToolExecutor>,
    approval: ToolApproval,
    parallel_safe: bool,
    host_only: bool,
}

impl fmt::Debug for ToolRegistration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolRegistration")
            .field("name", &self.spec.name())
            .field("approval", &self.approval)
            .field("parallel_safe", &self.parallel_safe)
            .field("host_only", &self.host_only)
            .finish_non_exhaustive()
    }
}

impl ToolRegistration {
    /// A tool that asks for approval on every call (fail-closed default;
    /// relax read-only tools with [`Self::approval`]).
    #[must_use]
    pub fn new(spec: ToolSpec, executor: Arc<dyn ToolExecutor>) -> Self {
        Self {
            spec,
            executor,
            approval: ToolApproval::Always,
            parallel_safe: false,
            host_only: false,
        }
    }

    /// Approval requirement of this tool.
    #[must_use]
    pub fn approval(mut self, approval: ToolApproval) -> Self {
        self.approval = approval;
        self
    }

    /// Independent calls may run concurrently.
    #[must_use]
    pub fn parallel_safe(mut self, parallel_safe: bool) -> Self {
        self.parallel_safe = parallel_safe;
        self
    }

    /// Host policy marks this tool host-only: it is served but never part
    /// of the default UIA ceiling, so no agent can be granted it.
    #[must_use]
    pub fn host_only(mut self) -> Self {
        self.host_only = true;
        self
    }
}

struct ToolEntry {
    descriptor: ToolDescriptor,
    schema: JsonSchema,
    executor: Arc<dyn ToolExecutor>,
    host_only: bool,
}

/// Builder of a [`ToolHost`].
pub struct ToolHostBuilder {
    node: Option<String>,
    tools: Vec<ToolRegistration>,
    sandbox: Arc<dyn GatewaySandbox>,
    approvals: Option<Arc<dyn ToolApprovalIssuer>>,
    uia_ceiling: Option<ToolGrant>,
    approval_timeout: Duration,
}

impl fmt::Debug for ToolHostBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolHostBuilder")
            .field("node", &self.node)
            .field("tools", &self.tools)
            .field("approval_timeout", &self.approval_timeout)
            .finish_non_exhaustive()
    }
}

impl ToolHostBuilder {
    /// Gateway node label reported in `placement` and `gateway.status`.
    #[must_use]
    pub fn node(mut self, node: impl Into<String>) -> Self {
        self.node = Some(node.into());
        self
    }

    /// Serve one more tool.
    #[must_use]
    pub fn tool(mut self, registration: ToolRegistration) -> Self {
        self.tools.push(registration);
        self
    }

    /// Approval store for tools that ask. Without one, every such call
    /// ends as an error result (never runs unapproved).
    #[must_use]
    pub fn approvals(mut self, approvals: Arc<dyn ToolApprovalIssuer>) -> Self {
        self.approvals = Some(approvals);
        self
    }

    /// Configured UIA ceiling. Always cut to the served, non-host-only tools.
    #[must_use]
    pub fn uia_ceiling(mut self, ceiling: ToolGrant) -> Self {
        self.uia_ceiling = Some(ceiling);
        self
    }

    /// How long a call waits for a human decision (default: the
    /// interaction contract's approval timeout).
    #[must_use]
    pub fn approval_timeout(mut self, timeout: Duration) -> Self {
        self.approval_timeout = timeout;
        self
    }

    /// Build the tool host. Duplicate tool names are refused.
    pub fn build(self) -> Result<ToolHost, HostError> {
        let placement = ToolPlacement::Gateway {
            node: self.node.clone(),
        };
        let mut tools = BTreeMap::new();
        for registration in self.tools {
            let ToolSpec::Function(function) = registration.spec;
            let name = function.name.as_str().trim().to_owned();
            if name.is_empty() {
                return Err(HostError::Protocol("gateway tool without name".into()));
            }
            let input_schema = serde_json::to_value(&function.parameters).map_err(|error| {
                HostError::Protocol(format!("tool `{name}`: schema not encodable: {error}"))
            })?;
            let entry = ToolEntry {
                descriptor: ToolDescriptor {
                    name: name.clone(),
                    description: function.description,
                    input_schema,
                    approval: registration.approval,
                    placement: placement.clone(),
                    parallel_safe: registration.parallel_safe,
                },
                schema: function.parameters,
                executor: registration.executor,
                host_only: registration.host_only,
            };
            if tools.insert(name.clone(), entry).is_some() {
                return Err(HostError::Protocol(format!(
                    "gateway tool `{name}` registered twice"
                )));
            }
        }
        let grantable = ToolGrant::from_names(
            tools
                .values()
                .filter(|entry| !entry.host_only)
                .map(|entry| entry.descriptor.name.as_str()),
        );
        let ceiling = match self.uia_ceiling {
            Some(configured) => configured.intersect(&grantable),
            None => grantable,
        };
        Ok(ToolHost {
            node: self.node,
            tools,
            sandbox: self.sandbox,
            approvals: self.approvals,
            approval_timeout: self.approval_timeout,
            agents: AgentRegistry::new(ceiling),
            in_flight: Mutex::new(HashMap::new()),
            waiters: Mutex::new(HashMap::new()),
        })
    }
}

/// Where a gateway event goes: the calling session's stream.
pub(crate) trait ToolEvents: Send + Sync {
    fn turn_event(&self, event: TurnEvent);
    fn approval_requested(&self, request: ApprovalRequest);
}

type CallKey = (SessionId, ToolCallId);

struct InFlight {
    agent: String,
    connection: ConnectionId,
    cancel: CancelToken,
    why: Arc<OnceLock<String>>,
}

impl InFlight {
    fn cancel(&self, why: &str) {
        let _ = self.why.set(why.to_owned());
        self.cancel.cancel(CancelReason::User);
    }
}

/// The gateway tool host: tool registry, agent registry, sandbox source,
/// in-flight calls and pending tool approvals.
pub struct ToolHost {
    node: Option<String>,
    tools: BTreeMap<String, ToolEntry>,
    sandbox: Arc<dyn GatewaySandbox>,
    approvals: Option<Arc<dyn ToolApprovalIssuer>>,
    approval_timeout: Duration,
    agents: AgentRegistry,
    in_flight: Mutex<HashMap<CallKey, InFlight>>,
    /// Tool approvals issued by this host. The sender is taken on
    /// resolution; an entry whose call ended keeps a dead sender so a late
    /// `approval.respond` is still recognized as a tool approval.
    waiters: Mutex<HashMap<ApprovalId, oneshot::Sender<ReviewDecision>>>,
}

impl fmt::Debug for ToolHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolHost")
            .field("node", &self.node)
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .field("agents", &self.agents)
            .finish_non_exhaustive()
    }
}

/// An admitted call: reserved in the in-flight map until dropped.
pub(crate) struct AdmittedCall<'a> {
    host: &'a ToolHost,
    key: CallKey,
    params: ToolCallParams,
    sandbox: SandboxSpec,
    cancel: CancelToken,
    why: Arc<OnceLock<String>>,
    started: Instant,
}

impl Drop for AdmittedCall<'_> {
    fn drop(&mut self) {
        if let Ok(mut in_flight) = self.host.in_flight.lock() {
            in_flight.remove(&self.key);
        }
    }
}

impl ToolHost {
    /// Start building a tool host on top of `sandbox`.
    #[must_use]
    pub fn builder(sandbox: Arc<dyn GatewaySandbox>) -> ToolHostBuilder {
        ToolHostBuilder {
            node: None,
            tools: Vec::new(),
            sandbox,
            approvals: None,
            uia_ceiling: None,
            approval_timeout: Duration::from_secs(
                u64::try_from(DEFAULT_APPROVAL_TIMEOUT.as_secs()).unwrap_or(300),
            ),
        }
    }

    /// A host that serves no tools and has no sandbox: every `tool.call` is
    /// refused (the W00 host without a tool gateway).
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            node: None,
            tools: BTreeMap::new(),
            sandbox: Arc::new(NoGatewaySandbox),
            approvals: None,
            approval_timeout: Duration::from_secs(300),
            agents: AgentRegistry::new(ToolGrant::none()),
            in_flight: Mutex::new(HashMap::new()),
            waiters: Mutex::new(HashMap::new()),
        }
    }

    /// The agent principal registry.
    #[must_use]
    pub fn agents(&self) -> &AgentRegistry {
        &self.agents
    }

    /// Gateway node label.
    #[must_use]
    pub fn node(&self) -> Option<&str> {
        self.node.as_deref()
    }

    /// Placement of every call served here.
    #[must_use]
    pub fn placement(&self) -> ToolPlacement {
        ToolPlacement::Gateway {
            node: self.node.clone(),
        }
    }

    /// True when `tool` is served by this host.
    #[must_use]
    pub fn serves(&self, tool: &str) -> bool {
        self.tools.contains_key(tool)
    }

    /// Number of served tools.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// True when no tool is served.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Sandbox probe (`GatewayStatus::sandbox_available`).
    #[must_use]
    pub fn sandbox_available(&self) -> bool {
        self.sandbox.available()
    }

    /// Every served tool.
    #[must_use]
    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.tools
            .values()
            .map(|entry| entry.descriptor.clone())
            .collect()
    }

    /// Exactly the served tools within `grant` (`tool.list`).
    #[must_use]
    pub fn descriptors_for(&self, grant: &ToolGrant) -> Vec<ToolDescriptor> {
        self.tools
            .values()
            .filter(|entry| grant.contains(&entry.descriptor.name))
            .map(|entry| entry.descriptor.clone())
            .collect()
    }

    /// Number of calls in flight.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.in_flight.lock().map_or(0, |calls| calls.len())
    }

    fn calls(&self) -> Result<MutexGuard<'_, HashMap<CallKey, InFlight>>, HostError> {
        self.in_flight
            .lock()
            .map_err(|_| HostError::Storage("tool call table poisoned".into()))
    }

    /// Step 8: the gateway sandbox for a call of `agent` in a session: the
    /// session sandbox, narrowed hop by hop along the delegation chain.
    pub(crate) fn sandbox_for(
        &self,
        scope: &SessionScope<'_>,
        agent: &str,
    ) -> Result<SandboxSpec, HostError> {
        let refuse = |detail: &str| HostError::ToolRefused {
            refusal: ToolRefusal::SandboxUnavailable,
            detail: detail.to_owned(),
        };
        if !self.sandbox.available() {
            return Err(refuse("no gateway sandbox backend"));
        }
        let Some(mut spec) = self.sandbox.session_sandbox(scope) else {
            return Err(refuse("no gateway sandbox for this session"));
        };
        for request in self.agents.sandbox_chain(agent)? {
            let child = spec.restrict(&request);
            if child.ensure_child_of(&spec).is_err() {
                return Err(refuse("delegated sandbox exceeds its parent"));
            }
            spec = child;
        }
        Ok(spec)
    }

    /// Step 9: reserve `call_id` in the session; a call id already in flight
    /// is refused as a duplicate.
    pub(crate) fn reserve(
        &self,
        params: ToolCallParams,
        sandbox: SandboxSpec,
        agent: &str,
        connection: ConnectionId,
    ) -> Result<AdmittedCall<'_>, HostError> {
        let key = (params.session_id.clone(), params.call_id.clone());
        let cancel = CancelToken::new();
        let why = Arc::new(OnceLock::new());
        {
            let mut calls = self.calls()?;
            if calls.contains_key(&key) {
                return Err(HostError::ToolRefused {
                    refusal: ToolRefusal::DuplicateCall,
                    detail: params.call_id.as_str().to_owned(),
                });
            }
            calls.insert(
                key.clone(),
                InFlight {
                    agent: agent.to_owned(),
                    connection,
                    cancel: cancel.clone(),
                    why: Arc::clone(&why),
                },
            );
        }
        Ok(AdmittedCall {
            host: self,
            key,
            params,
            sandbox,
            cancel,
            why,
            started: Instant::now(),
        })
    }

    /// `tool.cancel`: cancel the call only when it belongs to `agent`.
    /// Unknown, finished or foreign calls are ignored (idempotent, and a
    /// foreign call's existence does not leak).
    pub(crate) fn cancel_call(&self, session: &SessionId, call_id: &ToolCallId, agent: &str) {
        let Ok(calls) = self.calls() else { return };
        if let Some(call) = calls.get(&(session.clone(), call_id.clone())) {
            if call.agent == agent {
                call.cancel("cancelled by the caller");
            }
        }
    }

    /// Cancel every in-flight call of a revoked connection.
    pub(crate) fn cancel_connection(&self, connection: ConnectionId, why: &str) {
        let Ok(calls) = self.calls() else { return };
        for call in calls.values().filter(|call| call.connection == connection) {
            call.cancel(why);
        }
    }

    /// Cancel every in-flight call of the revoked agents `ids`.
    pub(crate) fn cancel_agents(&self, ids: &[String], why: &str) {
        let Ok(calls) = self.calls() else { return };
        for call in calls.values().filter(|call| ids.contains(&call.agent)) {
            call.cancel(why);
        }
    }

    /// Deliver a resolved decision to a waiting tool call. Returns `true`
    /// when `request` is a gateway tool approval (even when its call has
    /// ended), so the host does not resume a parked turn for it.
    pub(crate) fn resolve_approval(&self, request: &ApprovalId, decision: ReviewDecision) -> bool {
        let Ok(mut waiters) = self.waiters.lock() else {
            return false;
        };
        match waiters.remove(request) {
            Some(sender) => {
                let _ = sender.send(decision);
                true
            }
            None => false,
        }
    }

    /// Run an admitted call to its result frame, emitting
    /// `ToolCallRequested`/`ToolCallCompleted` into the session stream.
    pub(crate) async fn run(
        &self,
        call: AdmittedCall<'_>,
        events: &dyn ToolEvents,
    ) -> ToolCallResultFrame {
        events.turn_event(TurnEvent::ToolCallRequested {
            turn_id: call.params.turn_id.clone(),
            call_id: call.params.call_id.clone(),
            tool_name: call.params.tool_name.clone(),
            arguments: call.params.arguments.clone(),
        });
        let (result, trust) = self.execute(&call, events).await;
        let duration_ms = u64::try_from(call.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let placement = self.placement();
        let call_id = call.params.call_id.clone();
        let turn_id = call.params.turn_id.clone();
        // Release the call id before the completion becomes visible.
        drop(call);
        events.turn_event(TurnEvent::ToolCallCompleted {
            turn_id,
            call_id: call_id.clone(),
            result: result.clone(),
            duration_ms,
            placement: Some(placement.clone()),
        });
        ToolCallResultFrame {
            call_id,
            result,
            placement,
            duration_ms,
            trust,
        }
    }

    async fn execute(
        &self,
        call: &AdmittedCall<'_>,
        events: &dyn ToolEvents,
    ) -> (ToolCallResult, ResultTrust) {
        let params = &call.params;
        let Some(entry) = self.tools.get(&params.tool_name) else {
            // Admission already refused unknown tools; never run anything else.
            return runtime_error(format!("unknown tool `{}`", params.tool_name));
        };
        if let Err(reason) = check_arguments(&entry.schema, &params.arguments) {
            return runtime_error(format!(
                "invalid arguments for `{}`: {reason}",
                params.tool_name
            ));
        }
        if asks_human(entry.descriptor.approval) {
            if let Err(reason) = self.approve(call, events).await {
                return runtime_error(reason);
            }
        }
        let context = ToolExecutionContext::new(
            params.session_id.clone(),
            params.turn_id.clone(),
            call.sandbox.clone(),
        )
        .with_cancel(call.cancel.clone());
        let invocation = ToolCall {
            id: params.call_id.clone(),
            name: ToolName::new(params.tool_name.clone()),
            arguments: params.arguments.clone(),
        };
        tokio::select! {
            biased;
            () = call.cancel.cancelled() => runtime_error(cancelled(&call.why)),
            outcome = entry.executor.execute(&context, &invocation) => match outcome {
                Ok(ToolOutput::Text { content }) => {
                    (ToolCallResult::success(Value::String(content)), ResultTrust::Untrusted)
                }
                Ok(ToolOutput::Json { content }) => {
                    (ToolCallResult::success(content), ResultTrust::Untrusted)
                }
                Ok(ToolOutput::Error { message }) => {
                    (ToolCallResult::error(message), ResultTrust::Untrusted)
                }
                Err(ToolsError::Cancelled) => runtime_error(cancelled(&call.why)),
                Err(error) => (ToolCallResult::error(error.to_string()), ResultTrust::Untrusted),
            },
        }
    }

    /// Ask a human through the session's approval store. `Ok` only for an
    /// explicit approval; denial, expiry, cancellation and a missing store
    /// are reasons for an error result.
    async fn approve(
        &self,
        call: &AdmittedCall<'_>,
        events: &dyn ToolEvents,
    ) -> Result<(), String> {
        let params = &call.params;
        let Some(issuer) = &self.approvals else {
            return Err(format!(
                "`{}` requires approval and the gateway has no approval store",
                params.tool_name
            ));
        };
        let now = jiff::Timestamp::now();
        let wait = jiff::SignedDuration::from_millis(
            i64::try_from(self.approval_timeout.as_millis()).unwrap_or(i64::MAX),
        );
        let request = ApprovalRequest {
            id: ApprovalId::new(),
            work_id: WorkId::new(),
            kind: ApprovalKind::DynamicTool {
                turn_id: params.turn_id.clone(),
                tool_name: params.tool_name.clone(),
                arguments: params.arguments.clone(),
            },
            summary: format!("gateway tool `{}`", params.tool_name),
            risk: RiskLevel::Medium,
            requested_at: now,
            timeout_at: now.checked_add(wait).unwrap_or(now),
            decisions: vec![ReviewDecision::Approved, ReviewDecision::Rejected],
        };
        let (sender, receiver) = oneshot::channel();
        match self.waiters.lock() {
            Ok(mut waiters) => {
                waiters.insert(request.id.clone(), sender);
            }
            Err(_) => return Err("approval table unavailable".into()),
        }
        if let Err(error) = issuer.issue(&params.session_id, request.clone()) {
            if let Ok(mut waiters) = self.waiters.lock() {
                waiters.remove(&request.id);
            }
            return Err(format!("approval could not be requested: {error}"));
        }
        events.approval_requested(request);
        tokio::select! {
            biased;
            () = call.cancel.cancelled() => Err(cancelled(&call.why)),
            decision = tokio::time::timeout(self.approval_timeout, receiver) => match decision {
                Ok(Ok(ReviewDecision::Approved | ReviewDecision::ApprovedOnce)) => Ok(()),
                Ok(Ok(ReviewDecision::Rejected)) => {
                    Err(format!("approval for `{}` was denied", params.tool_name))
                }
                Ok(Err(_)) => Err("approval was withdrawn".into()),
                Err(_) => Err(format!("approval for `{}` expired", params.tool_name)),
            },
        }
    }
}

fn runtime_error(message: impl Into<String>) -> (ToolCallResult, ResultTrust) {
    (ToolCallResult::error(message), ResultTrust::Runtime)
}

fn cancelled(why: &OnceLock<String>) -> String {
    format!(
        "tool call cancelled: {}",
        why.get().map_or("cancelled", String::as_str)
    )
}

// ---------------------------------------------------------------------------
// Argument schema check
// ---------------------------------------------------------------------------

/// Check tool arguments against the tool's parameter schema (the
/// `harw_tools::JsonSchema` subset: type, properties, required,
/// additionalProperties, anyOf, items, enum). Arguments must be an object.
pub fn check_arguments(schema: &JsonSchema, arguments: &Value) -> Result<(), String> {
    if !arguments.is_object() {
        return Err("arguments must be a JSON object".into());
    }
    check_value(schema, arguments, "$")
}

fn check_value(schema: &JsonSchema, value: &Value, path: &str) -> Result<(), String> {
    if let Some(variants) = &schema.any_of {
        if !variants
            .iter()
            .any(|variant| check_value(variant, value, path).is_ok())
        {
            return Err(format!("{path}: matches no allowed variant"));
        }
    }
    if let Some(kind) = &schema.schema_type {
        if !type_matches(kind, value) {
            return Err(format!("{path}: expected {}", type_name(kind)));
        }
    }
    if let Some(allowed) = &schema.enum_values {
        if !allowed.contains(value) {
            return Err(format!("{path}: value not allowed"));
        }
    }
    if let Value::Object(map) = value {
        if let Some(required) = &schema.required {
            if let Some(missing) = required.iter().find(|name| !map.contains_key(*name)) {
                return Err(format!("{path}.{missing}: required"));
            }
        }
        for (name, field) in map {
            let nested = format!("{path}.{name}");
            match schema
                .properties
                .as_ref()
                .and_then(|properties| properties.get(name))
            {
                Some(property) => check_value(property, field, &nested)?,
                None => match schema.additional_properties.as_deref() {
                    Some(AdditionalProperties::Bool(false)) => {
                        return Err(format!("{nested}: unknown field"));
                    }
                    Some(AdditionalProperties::Schema(extra)) => {
                        check_value(extra, field, &nested)?;
                    }
                    Some(AdditionalProperties::Bool(true)) | None => {}
                },
            }
        }
    }
    if let (Value::Array(items), Some(item_schema)) = (value, &schema.items) {
        for (index, item) in items.iter().enumerate() {
            check_value(item_schema, item, &format!("{path}[{index}]"))?;
        }
    }
    Ok(())
}

fn type_matches(kind: &JsonSchemaType, value: &Value) -> bool {
    match kind {
        JsonSchemaType::String => value.is_string(),
        JsonSchemaType::Number => value.is_number(),
        JsonSchemaType::Integer => value.is_i64() || value.is_u64(),
        JsonSchemaType::Boolean => value.is_boolean(),
        JsonSchemaType::Object => value.is_object(),
        JsonSchemaType::Array => value.is_array(),
        JsonSchemaType::Null => value.is_null(),
    }
}

const fn type_name(kind: &JsonSchemaType) -> &'static str {
    match kind {
        JsonSchemaType::String => "string",
        JsonSchemaType::Number => "number",
        JsonSchemaType::Integer => "integer",
        JsonSchemaType::Boolean => "boolean",
        JsonSchemaType::Object => "object",
        JsonSchemaType::Array => "array",
        JsonSchemaType::Null => "null",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use harw_tools::{FunctionToolSpec, ToolExecutorFuture};
    use serde_json::json;

    use super::*;

    type TestResult<T = ()> = Result<T, HostError>;

    struct Echo;

    impl ToolExecutor for Echo {
        fn execute<'a>(
            &'a self,
            _context: &'a ToolExecutionContext,
            call: &'a ToolCall,
        ) -> ToolExecutorFuture<'a> {
            Box::pin(async move { Ok(ToolOutput::json(call.arguments.clone())) })
        }
    }

    fn schema() -> JsonSchema {
        let mut properties = BTreeMap::new();
        properties.insert(
            "path".to_owned(),
            JsonSchema {
                schema_type: Some(JsonSchemaType::String),
                ..JsonSchema::default()
            },
        );
        properties.insert(
            "limit".to_owned(),
            JsonSchema {
                schema_type: Some(JsonSchemaType::Integer),
                ..JsonSchema::default()
            },
        );
        JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(properties),
            required: Some(vec!["path".to_owned()]),
            additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
            ..JsonSchema::default()
        }
    }

    fn spec(name: &str) -> ToolSpec {
        ToolSpec::Function(FunctionToolSpec {
            name: ToolName::new(name),
            description: format!("{name} test tool"),
            parameters: schema(),
            strict: false,
        })
    }

    #[test]
    fn argument_check_fails_closed() {
        let schema = schema();
        assert!(check_arguments(&schema, &json!({"path": "a"})).is_ok());
        assert!(check_arguments(&schema, &json!({"path": "a", "limit": 3})).is_ok());
        assert!(check_arguments(&schema, &json!({})).is_err());
        assert!(check_arguments(&schema, &json!({"path": 1})).is_err());
        assert!(check_arguments(&schema, &json!({"path": "a", "limit": 1.5})).is_err());
        assert!(check_arguments(&schema, &json!({"path": "a", "principal": "x"})).is_err());
        assert!(check_arguments(&schema, &json!(["path"])).is_err());
    }

    #[test]
    fn descriptors_are_gateway_placed_and_ceiling_skips_host_only() -> TestResult {
        let host = ToolHost::builder(Arc::new(NoGatewaySandbox))
            .node("gw-1")
            .tool(
                ToolRegistration::new(spec("fs.read"), Arc::new(Echo))
                    .approval(ToolApproval::Never),
            )
            .tool(ToolRegistration::new(spec("host.secret"), Arc::new(Echo)).host_only())
            .build()?;
        assert_eq!(host.len(), 2);
        for descriptor in host.descriptors() {
            assert_eq!(
                descriptor.placement,
                ToolPlacement::Gateway {
                    node: Some("gw-1".into())
                }
            );
            assert_eq!(descriptor.input_schema["type"], json!("object"));
        }
        assert_eq!(
            host.agents().uia_ceiling().to_names(),
            vec!["fs.read".to_owned()]
        );
        assert!(!host.sandbox_available());
        let listed = host.descriptors_for(&ToolGrant::from_names(["fs.read", "nope"]));
        assert_eq!(listed.len(), 1);
        Ok(())
    }

    #[test]
    fn duplicate_tool_names_are_refused() {
        let built = ToolHost::builder(Arc::new(NoGatewaySandbox))
            .tool(ToolRegistration::new(spec("fs.read"), Arc::new(Echo)))
            .tool(ToolRegistration::new(spec("fs.read"), Arc::new(Echo)))
            .build();
        assert!(matches!(built, Err(HostError::Protocol(_))));
    }

    #[test]
    fn unknown_approval_asks_a_human() {
        assert!(!asks_human(ToolApproval::Never));
        assert!(asks_human(ToolApproval::Policy));
        assert!(asks_human(ToolApproval::Always));
        assert!(asks_human(ToolApproval::Unknown));
    }

    #[test]
    fn no_sandbox_backend_refuses_before_anything_runs() -> TestResult {
        let host = ToolHost::builder(Arc::new(NoGatewaySandbox))
            .tool(ToolRegistration::new(spec("fs.read"), Arc::new(Echo)))
            .build()?;
        let session = SessionId::new();
        let scope = SessionScope {
            session_id: &session,
            tenant: None,
            workspace: None,
        };
        assert!(matches!(
            host.sandbox_for(&scope, "agent:uia"),
            Err(HostError::ToolRefused {
                refusal: ToolRefusal::SandboxUnavailable,
                ..
            })
        ));
        Ok(())
    }
}
