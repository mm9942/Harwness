---
id: R18-TOOL-GATEWAY
title: "R18 Contract — Tools via the UIA through the gateway, AI-manageable gateway, WorkDriver reports"
status: proposed
date: 2026-09-28
parent:
  - ./W00-websocket-control-plane.md
  - ../README.md
---

# R18 Contract — Tool gateway

Round R18 "tools, AI and effective control of isolation". This contract fixes
the decisions taken with Mia, the wire vocabulary (landed in wave 1), the
identity and rights model, the fail-closed rules and the file ownership of the
wave-2 work packages. Wave-2 coders implement against this document; where the
code of wave 1 and this document disagree, the code is the reference and this
document is corrected.

**Baseline:** branch `claude/r18a-tools-gateway`, based on W00 round 1
(`f852f2a`).

---

## 1. Decisions

### D-A — Tools only via the UIA through the gateway

Tools execute **in the gateway** (the persistent session host process, W00
D9), inside a gateway-side sandbox built from `harw-authority`
(`SandboxSpec`) and `harw-sandbox`. They do not execute in agent processes.

- Only an agent principal with role `UserInterface` (the UIA,
  `harw_agent_dsl::roles::AgentRoleId::UserInterface`) holds tool rights of
  its own.
- Sub-agents and workers reach tools **only by UIA delegation**, with a grant
  that is a subset of the delegating parent's grant (§4). This mirrors
  `SandboxSpec::ensure_child_of` (`harw-authority/src/lib.rs`): a child never
  holds more than its parent.
- An agent process connected this way has **no local `ToolProvider`s**: it
  assembles with `RegistryProfile::NoTools` plus the remote proxy executors of
  `harw-tool-remote` (§10, P2), which forward every call as `tool.call`.

Rationale: isolation is only effective if the place that holds the authority
(the gateway: sandbox, approvals, audit, revocation) is also the place that
executes. An agent process that runs tools locally makes every gateway control
advisory. Field evidence F1/F3/F7 (§11) shows agents escaping into
`shell.exec` because the gateway was not reachable as a tool.

### D-B — AI-manageable gateway

New `gateway.*` operations. Read operations are free model tools; mutations
are model tools with `approval = "always"`. Key operations
(`infra.auth.keys.*`) stay excluded from every model surface.

This **deliberately narrows** the rule in `harw-ops/src/infra.rs` ("a
language model must never drive key operations or probe infrastructure on its
own"). The new rule, verbatim for the `infra.rs` module doc (P3):

> A language model never drives key operations (`infra.auth.keys.*`) and
> never reaches `infra.*`. It may inspect the **gateway** through the
> read-only `gateway.*` model tools and may request gateway mutations through
> the mutating `gateway.*` model tools, each of which asks a human every time
> (`approval = "always"`). Approval never expands authority: the operation
> still runs with the caller's gateway caps and tenant scope.

Rationale: agents already try to manage the gateway (F1, via `harw` in
`shell.exec`, which fails in the sandbox and needs a 62 s approval on the
host). A typed, approved tool is safer than a shell escape.

### D-C — Hub and worker vocabulary (R18b, vocabulary only)

Hub kinds and worker kinds are defined in §9 now so that R18a code, docs and
UI use one set of words. No code in R18a depends on them.

### D-D — Placement instead of command lines

Surfaces (TUI first) show **where** a tool call ran — `host`, `sandbox` or
`gateway` — instead of a shell command line. The placement is set by the
executing side, never by the model (§5).

### D-E — Structured WorkDriver return

WorkDriver workers report their outcome by calling the tool
`work_driver.report` (§8). Free-text status lines and embedded JSON are no
longer parsed.

---

## 2. Wire (harw-protocol, wire minor 2)

All types are in `harw-protocol` (std + serde only; ARC-01 unchanged).
`SESSION_WIRE_MINOR` is now `2`; `TOOL_GATEWAY_WIRE_MINOR = 2`.

### 2.1 Method table

W00 D5 stays: there is **no generic operation execution**. R18 adds three
closed tables (`harw_protocol::methods`), disjoint from `SESSION_METHODS`:

| Method | Constant | Params | Result | Cap | Extra admission |
|---|---|---|---|---|---|
| `tool.list` | `METHOD_TOOL_LIST` | `ToolListParams` | `ToolListResult` | `tool_call` | agent principal, session bound to principal (§4.2 step 1) |
| `tool.call` | `METHOD_TOOL_CALL` | `ToolCallParams` | `ToolCallResultFrame` | `tool_call` | agent principal, session bound to principal, tool known, tool granted, not draining, sandbox available, call id unique |
| `tool.cancel` | `METHOD_TOOL_CANCEL` | `ToolCancelParams` | `()` | `tool_call` | session bound to principal; own call only; unknown/finished call = `Ok(())` |
| `gateway.status` | `METHOD_GATEWAY_STATUS` | — | `GatewayStatus` | `gateway_read` | — |
| `gateway.connections.list` | `METHOD_GATEWAY_CONNECTIONS_LIST` | — | `GatewayConnectionsResult` | `gateway_read` | tenant filter |
| `gateway.sessions.list` | `METHOD_GATEWAY_SESSIONS_LIST` | — | `Vec<SessionSummary>` | `gateway_read` | tenant filter |
| `gateway.listeners.list` | `METHOD_GATEWAY_LISTENERS_LIST` | — | `GatewayListenersResult` | `gateway_read` | — |
| `gateway.tools.list` | `METHOD_GATEWAY_TOOLS_LIST` | — | `GatewayToolsResult` | `gateway_read` | tenant filter on grants |
| `gateway.connections.revoke` | `METHOD_GATEWAY_CONNECTIONS_REVOKE` | `GatewayRevokeParams` | `GatewayRevokeResult` | `gateway_admin` | tenant filter; W00 §7 revocation semantics |
| `gateway.drain` | `METHOD_GATEWAY_DRAIN` | `GatewayDrainParams` | `GatewayStatus` | `gateway_admin` | unscoped caller only |
| `gateway.listeners.set` | `METHOD_GATEWAY_LISTENERS_SET` | `GatewayListenerSetParams` | `GatewayListenerInfo` | `gateway_admin` | unscoped caller only; unknown name = `NOT_FOUND` |
| `gateway.tools.grant` | `METHOD_GATEWAY_TOOLS_GRANT` | `GatewayToolRightsParams` | `GatewayToolRights` | `gateway_admin` | within ceiling (§4) |
| `gateway.tools.narrow` | `METHOD_GATEWAY_TOOLS_NARROW` | `GatewayToolRightsParams` | `GatewayToolRights` | `gateway_admin` | — |

Tables: `TOOL_METHODS`, `GATEWAY_READ_METHODS`, `GATEWAY_ADMIN_METHODS`.
Hello features: `features::TOOLS` (`"tools"`), `features::GATEWAY`
(`"gateway"`). Every method still requires a successful `session.hello`
first (W00 §3.3).

Rust ports (`harw_protocol::session_port`), separate from `SessionPort` so
existing implementations are unchanged:

```rust
pub trait ToolPort: Send + Sync {
    fn list_tools(&self, params: ToolListParams) -> PortFuture<'_, ToolListResult>;
    fn call_tool(&self, params: ToolCallParams) -> PortFuture<'_, ToolCallResultFrame>;
    fn cancel_tool(&self, params: ToolCancelParams) -> PortFuture<'_, ()>;
}

pub trait GatewayPort: Send + Sync {
    fn status(&self) -> PortFuture<'_, GatewayStatus>;
    fn connections(&self) -> PortFuture<'_, GatewayConnectionsResult>;
    fn sessions(&self) -> PortFuture<'_, Vec<SessionSummary>>;
    fn listeners(&self) -> PortFuture<'_, GatewayListenersResult>;
    fn tools(&self) -> PortFuture<'_, GatewayToolsResult>;
    fn revoke_connection(&self, params: GatewayRevokeParams) -> PortFuture<'_, GatewayRevokeResult>;
    fn drain(&self, params: GatewayDrainParams) -> PortFuture<'_, GatewayStatus>;
    fn set_listener(&self, params: GatewayListenerSetParams) -> PortFuture<'_, GatewayListenerInfo>;
    fn grant_tools(&self, params: GatewayToolRightsParams) -> PortFuture<'_, GatewayToolRights>;
    fn narrow_tools(&self, params: GatewayToolRightsParams) -> PortFuture<'_, GatewayToolRights>;
}
```

### 2.2 Types (`harw_protocol::session_wire`)

```rust
pub enum AgentRole { UserInterface, RootOrchestrator, ChildOrchestrator, Worker,
                     UiaWorker, AgentSteward, #[serde(other)] Unknown }   // kebab-case
pub enum ToolApproval { Never, Policy, Always, #[serde(other)] Unknown } // snake_case

pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub approval: ToolApproval,
    pub placement: ToolPlacement,
    #[serde(default)] pub parallel_safe: bool,
}
pub struct ToolListParams { pub session_id: SessionId }
pub struct ToolListResult { pub tools: Vec<ToolDescriptor> }
pub struct ToolCallParams {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub call_id: ToolCallId,
    pub tool_name: String,
    pub arguments: serde_json::Value,
    #[serde(default)] pub parent_call_id: Option<ToolCallId>,
}
pub struct ToolCallResultFrame {
    pub call_id: ToolCallId,
    pub result: ToolCallResult,
    pub placement: ToolPlacement,
    pub duration_ms: u64,
    #[serde(default)] pub trust: ResultTrust,   // default Untrusted
}
pub struct ToolCancelParams { pub session_id: SessionId, pub call_id: ToolCallId }

pub struct GatewayStatus { pub host_epoch: u64, pub node: Option<String>, pub draining: bool,
    pub connections: u32, pub sessions: u32, pub running_turns: u32, pub listeners: u32,
    pub tools: u32, pub sandbox_available: bool }
pub enum PrincipalSummary {                     // tag = "kind"
    Device { device: Option<DeviceId> },
    Agent { agent: String, role: AgentRole, parent: Option<String> },
    #[serde(other)] Unknown,
}
pub struct GatewayConnectionInfo { pub connection: u64, pub label: String,
    pub principal: PrincipalSummary, pub tenant: Option<TenantId>,
    pub granted: Option<ClientCaps>, pub attached: u32, pub since: jiff::Timestamp }
pub struct GatewayConnectionsResult { pub connections: Vec<GatewayConnectionInfo> }
pub enum ListenerKind { LocalUds, Node, #[serde(other)] Unknown }
pub struct GatewayListenerInfo { pub name: String, pub kind: ListenerKind,
    pub address: String, pub enabled: bool }
pub struct GatewayListenersResult { pub listeners: Vec<GatewayListenerInfo> }
pub struct GatewayToolRights { pub agent: String, pub role: AgentRole, pub tools: Vec<String> }
pub struct GatewayToolsResult { pub tools: Vec<ToolDescriptor>, pub grants: Vec<GatewayToolRights> }
pub struct GatewayRevokeParams { pub connection: u64, pub reason: String }
pub struct GatewayRevokeResult { pub revoked: bool }
pub struct GatewayDrainParams { pub retry_after_ms: u64 }
pub struct GatewayListenerSetParams { pub name: String, pub enabled: bool }
pub struct GatewayToolRightsParams { pub agent: String, pub tools: Vec<String> }
```

Rules shared with W00: request params are `deny_unknown_fields` (no identity
smuggling: a `principal`, `tenant`, `role` or `actor` field in any params is a
decode error); results are `deny_unknown_fields` too, so any result change
bumps the wire minor. `AgentRole` mirrors `AgentRoleId` value for value; the
pin test lives at the composition root (P2, §10).

`ToolDescriptor.placement` of a gateway-served tool is always
`ToolPlacement::Gateway { .. }`. `ToolApproval::Unknown` must be rendered and
planned like `Always`.

### 2.3 Caps

`ClientCaps` gains three fields, all `#[serde(default,
skip_serializing_if = "is_false")]`:

| Cap | Grants | Tier ceiling |
|---|---|---|
| `tool_call` | `tool.*` | **never** from a tier; only agent principals (§3) |
| `gateway_read` | read `gateway.*` | Maintainer, Owner (`gateway_caps_for_tier`) |
| `gateway_admin` | mutating `gateway.*` | Owner |

- `ClientCaps::ALL` stays the four W00 caps and **does not** include any R18
  cap. New constants: `TOOL_CALL`, `GATEWAY_READ`, `GATEWAY_ADMIN`
  (`GATEWAY_ADMIN` = read + admin).
- `ClientCaps::with` composes a **ceiling** from constants (listener side
  only). Effective caps stay `requested ∩ ceiling`.
- `ClientCaps::for_wire_minor(minor)`: the host applies it to the granted
  caps at hello (`negotiated = min(client, host)`); below minor 2 all R18 caps
  are masked, so a minor-1 client never receives a field it cannot decode
  (`ClientCaps` is `deny_unknown_fields`). P1 wires this into
  `HostConnection::hello_sync`.
- A false R18 cap is not serialized: the minor-1 shape is byte-identical
  (frozen test `frozen_v1_hello_ack_is_unchanged_by_r18_caps`).

### 2.4 Error codes

| Code | Constant | `PortError` | When |
|---|---|---|---|
| -32002 | `DENIED` | `Denied` | missing cap; caller is not an agent principal; invalid agent principal; grant outside ceiling |
| -32003 | `NOT_FOUND` | `NotFound` | unknown/foreign session (other tenant, or same tenant but not bound to the calling agent, §4.2), unknown agent/connection/listener |
| -32004 | `REVOKED` | `Revoked` | connection or agent principal revoked |
| -32010 | `TOOL_UNKNOWN` | `ToolRefused { UnknownTool }` | tool not served by the gateway |
| -32011 | `TOOL_NOT_GRANTED` | `ToolRefused { NotGranted }` | tool outside the principal's grant |
| -32012 | `TOOL_SANDBOX_UNAVAILABLE` | `ToolRefused { SandboxUnavailable }` | no gateway-side sandbox |
| -32013 | `HOST_DRAINING` | `ToolRefused { Draining }` | host draining |
| -32014 | `TOOL_CALL_DUPLICATE` | `ToolRefused { DuplicateCall }` | `call_id` already in flight in the session |

`harw_protocol::ToolRefusal` carries `code()`/`from_code()`; `PortError::
from_code` maps the five codes back losslessly. `harw_session_host::HostError`
has the matching variant `ToolRefused { refusal, detail }`.

A tool that **ran** and failed (including argument schema violations,
approval denied, tool timeout, cancellation) is **not** a wire error: it is a
`ToolCallResultFrame` with `ToolCallResult::Error` so the model can react.

### 2.5 Streaming

`tool.call` is request/response in R18a. No `ToolDelta` frame is defined.
Long-running work belongs in jobs (§11 F8). Revisit in R18b if a use case
needs live tool output over the wire.

---

## 3. Identity: the agent principal

`harw_session_host::ClientIdentity` gains `agent: Option<AgentPrincipal>`.
`None` is a human/device caller (W00 unchanged).

```rust
pub struct AgentPrincipal {
    pub id: String,            // gateway-issued, stable agent principal id
    pub role: AgentRole,
    pub parent: Option<String>,// parent agent principal id; None exactly for the UIA
    pub tools: ToolGrant,      // exact tool names
}
```

| Field | Source | Rule |
|---|---|---|
| kind | `agent.is_some()` | agent vs device, shown as `PrincipalSummary` |
| role | agent credential | `Unknown` is refused (`AgentPrincipal::validate`) |
| parent | delegation record | UIA: `None`; every other role: `Some`, never itself |
| tenant | `ClientIdentity::tenant` | a delegate always carries its parent's tenant; remote callers must have one (W00) |
| granted tool set | delegation record | `ToolGrant`, subset of the parent's (§4) |

The agent principal is built by a listener from a **gateway-issued agent
credential** bound to one agent process (the gateway launched or admitted
it). Like every W00 identity field it never comes from a payload. The
credential format is W04/W05 work (local UDS: registered peer pid/uid; node
transport: enrolled agent key); P1 provides the in-memory registry that maps a
credential to an `AgentPrincipal` (`harw-session-host/src/agents.rs`).

`ClientIdentity::validate` additionally refuses a caller that holds the
`tool_call` cap without an agent principal (listener bug, fail closed).

---

## 4. Rights narrowing and fail-closed admission

### 4.1 Narrowing

- The UIA's grant is bounded by the configured UIA ceiling (gateway
  configuration; default: the tools the gateway tool host serves minus
  anything the host policy marks host-only).
- `AgentPrincipal::delegate(child_id, role, requested)` produces a child
  whose grant is exactly `requested`, which must be a subset of the parent's
  grant. A name outside the parent grant is `Denied` (`ToolGrantError::
  OutsideCeiling`), **never silently dropped**.
- Delegation is transitive: a grandchild is bounded by the child, never by
  the UIA.
- `gateway.tools.grant` replaces a grant within the same ceiling;
  `gateway.tools.narrow` removes names. Narrowing a parent narrows every
  descendant to `descendant ∩ parent` immediately (P1 registry). In-flight
  calls finish; new calls see the new grant.
- The gateway-side `SandboxSpec` of a delegate must satisfy
  `child.ensure_child_of(parent)`; a failure refuses the delegation.

### 4.2 Admission order for `tool.call`

The session binding of step 1 (host, `HostConnection::tool_admission`,
before anything else about the call is revealed), then
`harw_session_host::identity::admit_tool_call` (tenant again and steps 2-5,
pure), then the tool host (steps 6-9):

1. the session is bound to the calling agent, else `NOT_FOUND` (exactly
   like an unknown session, so no existence oracle): the session record's
   tenant matches, **and** the session was created by the calling agent or
   by an ancestor in its delegation chain (`HostedSessionRecord::
   owner_agent`), or the host composition bound one of them to it
   (`SessionHost::bind_tool_agent`, `tool_agents`; not a wire method).
   Same tenant alone is not enough: another agent of the tenant would
   otherwise reach that session's workspace sandbox. Applies to
   `tool.list`, `tool.call` and `tool.cancel`;
2. `tool_call` cap in the granted caps (→ `DENIED`);
3. agent principal present and valid (→ `DENIED`);
4. tool served by the gateway (→ `TOOL_UNKNOWN`);
5. tool in the principal's grant (→ `TOOL_NOT_GRANTED`);
6. connection/principal not revoked (→ `REVOKED`);
7. host not draining (→ `HOST_DRAINING`);
8. gateway sandbox available for this session (→ `TOOL_SANDBOX_UNAVAILABLE`);
9. `call_id` not in flight in the session (→ `TOOL_CALL_DUPLICATE`).

Then: argument schema check (violation → result `Error`), approval per
`ToolDescriptor.approval` through the session's approval store (W00
approvals, first-writer-wins; denial → result `Error`), execution in the
sandbox, result frame with `placement = Gateway { node }`.

**No fallback, anywhere.** Neither the gateway nor the remote proxy (P2) ever
executes a refused call on the host or in the agent process. The proxy maps
every refusal to a `ToolCallResult::Error` with runtime trust and a message
that names the refusal (the model sees why), never to a local execution.

### 4.3 Events

For every admitted call the gateway emits, into the calling session's stream,
`TurnEvent::ToolCallRequested` and `TurnEvent::ToolCallCompleted { placement:
Some(Gateway { .. }), .. }`, so attached human clients see gateway tool calls
like local ones.

---

## 5. Placement

```rust
// harw_protocol::items
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolPlacement { Host, Sandbox, Gateway { node: Option<String> }, #[serde(other)] Unknown }
```

| Where | Field |
|---|---|
| `TurnEvent::ToolCallCompleted` | `placement: Option<ToolPlacement>` (`serde(default, skip_serializing_if = "Option::is_none")`; old senders read as `None`) |
| `ToolCallResultFrame` | `placement: ToolPlacement` (required) |
| `ToolDescriptor` | `placement: ToolPlacement` |

Who sets it: the gateway tool host (`Gateway`), the remote proxy copies the
frame's placement into its completion, the local turn loop sets `Sandbox` for
sandboxed local executors and `Host` for approved host escalations (P2, via
executor metadata). `None` means unknown: surfaces show no placement rather
than guessing `host`. `ToolResultItem` (persisted transcript) is unchanged in
R18a.

---

## 6. `gateway.*` operations (P3)

Implemented once in `harw-ops/src/gateway_ops.rs` over
`Arc<dyn harw_protocol::GatewayPort>` from the `OpContext` (like
`infra.rs` reads `InfrastructureAvailability`); no socket of their own.
The ten port-backed operations are registered only when a gateway port is
present (`register_gateway`, runtime `GatewayContributor` via
`RuntimeAssemblyBuilder::gateway_port`; the port reaches the Slash,
model-tool and Web service maps, never Job). The diagnostics group
(`register_gateway_diagnostics`, runtime `GatewayDiagnosticsContributor`,
every UIA root) needs no port: `gateway.health`, `gateway.logs` (local harw
home) and — moved there in the R18a integration because F1 asks for them
exactly when no gateway session is connected — `gateway.channels.list` and
`gateway.channels.connect_info` (channel configuration). Model tools run in
the gateway (placement `Gateway`). Approval lists
(`harw-registry-defaults`): the nine read tools are in
`AUTO_APPROVED_TOOLS`, the five mutations in `ALWAYS_ASK_TOOLS`
(`profile::GATEWAY_READ_TOOLS`/`GATEWAY_MUTATION_TOOLS`, cross-checked in
`harw-ops/tests/gateway_ops.rs`).

| Operation | Model tool | Approval | Slash | Web | Backed by |
|---|---|---|---|---|---|
| `gateway.status` | yes, readonly | none | `/gateway-status` | `GET /api/gateway/status` | `GatewayPort::status` |
| `gateway.connections.list` | yes, readonly | none | `/gateway-connections` | `GET /api/gateway/connections` | `connections` |
| `gateway.sessions.list` | yes, readonly | none | — | `GET /api/gateway/sessions` | `sessions` |
| `gateway.listeners.list` | yes, readonly | none | `/gateway-listeners` | `GET /api/gateway/listeners` | `listeners` |
| `gateway.tools.list` | yes, readonly | none | `/gateway-tools` | `GET /api/gateway/tools` | `tools` |
| `gateway.channels.list` | yes, readonly | none | `/gateway-channels` | `GET /api/gateway/channels` | channel config (F1), diagnostics group, no port needed |
| `gateway.channels.connect_info` | yes, readonly | none | — | `GET /api/gateway/channels/connect-info` | how to connect a channel (what `harw channel connect --help` tells a human), diagnostics group, no port needed, never secrets |
| `gateway.health` | yes, readonly | none | `/gateway-health` | `GET /api/gateway/health` | local harw home (daemon, sockets, logs); port only adds the host status |
| `gateway.logs` | yes, readonly | none | `/gateway-logs` | `GET /api/gateway/logs` | local log files, folded and sanitized |
| `gateway.connections.revoke` | yes | **always** | `/gateway-revoke` | `POST`, always | `revoke_connection` |
| `gateway.drain` | yes | **always** | `/gateway-drain` | `POST`, always | `drain` |
| `gateway.listeners.set` | yes | **always** | — | `POST`, always | `set_listener` |
| `gateway.tools.grant` | yes | **always** | — | `POST`, always | `grant_tools` |
| `gateway.tools.narrow` | yes | **always** | — | `POST`, always | `narrow_tools` |
| `infra.auth.keys.*` | **no** | — | unchanged | unchanged | excluded from model surfaces (test) |

Every `gateway.*` tool description states: "Use this instead of running
`harw gateway …`/`harw channel …` through `shell.exec`; the `harw` binary is
not available inside the sandbox." (F1). Output never contains tokens, keys
or credentials; daemon-provided text is sanitized like `infra.rs`.

---

## 7. Hello and drain semantics touched by R18

- `gateway.drain` sets the host draining: `SessionFrame::HostDraining` to all
  attachments, new `turn.submit` → `Denied`, new `tool.call` →
  `HOST_DRAINING`; running turns and in-flight tool calls finish.
- Revocation of a connection or agent principal (W00 §7) also cancels that
  principal's in-flight tool calls (result `Error`, `trust = runtime`) and
  refuses new ones with `REVOKED`.

---

## 8. WorkDriver return contract (D-E)

Tool `work_driver.report` (`harw_plan_bridge::WORK_DRIVER_REPORT_TOOL`),
arguments = `WorkerReport` (`deny_unknown_fields`, schema
`WorkerReport::input_schema()`):

```rust
pub enum WorkerReportStatus { Done, Partial, Blocked, Failed }   // snake_case
pub struct WorkerReport {
    pub status: WorkerReportStatus,
    #[serde(default)] pub criteria_addressed: Vec<usize>,
    #[serde(default)] pub changed_paths: Vec<String>,
    pub summary: String,
    #[serde(default)] pub blockers: Vec<String>,
}
impl WorkerReport {
    pub fn validate(&self, criteria_total: usize) -> Result<(), WorkerReportError>;
    pub fn into_summary(self) -> WorkerResultSummary;
    pub fn input_schema() -> serde_json::Value;
}
impl WorkerResultSummary { pub fn no_report(last_text: &str) -> Self; }
```

Rules (P4 implements in the job worker):

- The worker's prompt (`RETURN_CONTRACT`) tells it to finish **by calling
  `work_driver.report`**; the free-text status line and JSON block are gone.
- `validate` fails closed: empty summary, `blocked` without a non-empty
  blocker, criterion index out of range, absolute path / `..` / `:` in a
  path. A rejected report is returned to the worker as a tool error and does
  not count as a report.
- The last valid report of the round wins. `into_summary` maps
  `blocked`/`failed` to their reason (joined blockers, else the summary) and
  `changed_paths` to `artifacts`.
- **No report** → `WorkerResultSummary::no_report(last_text)`: outcome
  `Partial`, summary starting with `"no report"`, `suggested_next` asking for
  the tool. Never silently `Done`. `parse_worker_return` is deleted.
- `criteria_addressed` feeds P4's routing (which criteria a worker claims),
  not the goal status: only verification and a human set criteria/goals.

---

## 9. Hub and worker vocabulary (D-C, R18b)

`HubKind` (R18b, `Custom(String)` for everything else):

| Kind | Definition |
|---|---|
| `gateway` | The persistent session host: identity admission, sessions, attach/replay, tool execution in its sandbox (this contract). |
| `auth` | Holds and uses keys; issues/rotates credentials and signs; the only place key material lives (`harw-auth-hub`). |
| `security` | Security contexts, policy decisions, revocation and audit of access (`harw-security-hub`). |
| `network` | Egress control and network scopes for sandboxes and workers (`harw-netsec`/`harw-egress`). |
| `placement` | Decides where execution runs (host, sandbox, Zellhost, remote node) and brokers the placement. |
| `communication` | External channels (Telegram, …) and scheduled background agents (knowledge/dream schedulers) — today's `harw gateway` daemon duties. |
| `production` | Runs approved long-lived production workloads/deployments. **Definition to confirm with Mia.** |
| `Custom(String)` | Operator-defined hub; no built-in semantics. |

`WorkerKind` (R18b, `Custom(String)` for everything else):

| Kind | Definition |
|---|---|
| `executor` | Runs narrowly scoped process commands and returns a summary (today `executor`). |
| `implementer` | Edits code within an owned path scope (WorkDriver default worker). |
| `explorer` | Read-only workspace exploration. |
| `researcher` | Web/dependency research without workspace writes. |
| `verifier` | Runs the central verification commands; never edits. |
| `judge` | Decides criteria that evidence alone cannot; no tools. |
| `steward` | Maintains definitions/memory (agent steward, memory steward). |
| `Custom(String)` | Operator-defined worker. |

R18b adds a spawn-time check that a worker kind's required tool classes are
covered by its granted tool set (F5).

---

## 10. Work packages and file ownership

Wave 1 (this contract, done): the files in §10.1. Wave 2: P1-P6 in parallel.
**One file, one owner.** A package that needs a change in a file it does not
own asks the owner (or the coordinator) and keeps its own change out of it.
The coordinator alone edits the root `Cargo.toml`, `Cargo.lock` and
`xtask/arch-policy.toml`.

### 10.1 Wave 1 (R18a-W1, landed with this contract)

| File | Change |
|---|---|
| `docs/planning/65-cloud-sessions/contracts/R18-tool-gateway.md` | this contract |
| `docs/planning/65-cloud-sessions/contracts/W00-websocket-control-plane.md` | D5 pointer |
| `harw-protocol/src/session_wire.rs` | wire minor 2, caps, tool/gateway types, error codes, tests |
| `harw-protocol/src/session_port.rs` | `ToolPort`, `GatewayPort`, `ToolRefusal`, `PortError::ToolRefused` |
| `harw-protocol/src/methods.rs` | method constants and tables |
| `harw-protocol/src/items.rs` | `ToolPlacement` |
| `harw-protocol/src/events.rs` | `ToolCallCompleted::placement` |
| `harw-protocol/src/lib.rs` | exports |
| `harw-protocol/tests/session_wire_compat.rs` | frozen v2 shapes |
| `harw-session-host/src/identity.rs` | `AgentPrincipal`, `ToolGrant`, `admit_tool_call`, gateway tier caps |
| `harw-session-host/src/error.rs` | `HostError::ToolRefused` |
| `harw-session-host/src/lib.rs` | exports |
| `harw-session-host/src/host/tests.rs` | wire minor assertion |
| `harw-session-ws/tests/e2e_host.rs` | `agent: None` |
| `harw-plan-bridge/src/work_driver.rs` | `WorkerReport` & co |
| `harw-plan-bridge/src/lib.rs` | exports |
| `harw-core/src/turn_loop.rs`, `harw-tui/src/{app.rs,app/turn_safety/tests.rs,agent_monitor.rs,child_stream.rs,agent_tree_live.rs}`, `harwness-sdk/src/event.rs` | `placement: None` in `ToolCallCompleted` constructors only |

### 10.2 Wave 2 ownership

| File | Owner |
|---|---|
| `harw-session-host/src/tool_host.rs` (new) | P1 |
| `harw-session-host/src/agents.rs` (new) | P1 |
| `harw-session-host/src/host.rs`, `harw-session-host/src/host/tests.rs` | P1 |
| `harw-session-host/src/identity.rs`, `error.rs`, `lib.rs` | P1 |
| `harw-session-host/Cargo.toml` (+ `harw-tools`, `harw-authority`, `harw-sandbox`) | P1 |
| `harw-session-host/tests/tool_gateway.rs` (new) | P1 |
| `harw-session-ws/src/dispatch.rs`, `harw-session-ws/src/conn.rs`, `harw-session-ws/src/conn/tests.rs`, `harw-session-ws/tests/e2e_host.rs` | P1 |
| `harw-tool-remote/**` (new crate) | P2 |
| `harw-tools/src/executor.rs` (placement metadata on `ToolExecutor`, default `None`) | P2 |
| `harw-core/src/turn_loop.rs` (fill `placement`) | P2 |
| `harw-registry-defaults/src/profile.rs` | P2 |
| `harw-runtime/src/spec.rs`, `harw-runtime/src/assembly.rs`, `harw-runtime/Cargo.toml` | P2 (assembly: also the one-line UIA `WorkDriverCaller` hook for P4, F6) |
| `harw-runtime/tests/remote_tools.rs` (new) | P2 |
| `harw-ops/src/gateway_ops.rs` (new), `harw-ops/src/lib.rs`, `harw-ops/src/infra.rs` (doc rule), `harw-ops/Cargo.toml` (+ `harw-protocol`) | P3 |
| `harw-ops/tests/gateway_ops.rs` (new) | P3 |
| `harw-runtime/src/contributors.rs` (`GatewayContributor`) | P3 |
| `harw-cli/src/job_worker_work_driver.rs`, `harw-cli/src/job_worker.rs` | P4 |
| `harw-ops/src/work_driver.rs` | P4 |
| `harw-plan-bridge/src/work_driver.rs`, `harw-plan-bridge/src/lib.rs` | P4 |
| `docs/guides/work-driver.md` | P4 |
| `harw-tui/src/history_cell.rs`, `harw-tui/src/export.rs` | P5 |
| `harw-tui/src/agent_monitor.rs`, `harw-tui/src/child_stream.rs`, `harw-tui/src/app.rs` | P5 |
| `harw-tui/src/jobs_panel.rs`, `harw-tui/src/app/jobs_glue.rs` | P5 |
| `harw-tui/src/approval_dialog.rs` | P5 |
| `harw-tool-job/src/model.rs`, `harw-tool-job/src/event.rs` | P5 |
| `harw-runtime/src/auto_classifier.rs`, `harw-config/src/role_models.rs` | P6 |
| `harw-tool-shell/src/exec.rs` | P6 |
| `harw-tool-fs/src/edit.rs`, `harw-tool-fs/src/write.rs` (descriptions) | P6 |
| `harw-tool-job/src/tools.rs`, `harw-tool-job/src/manager.rs`, `harw-tool-job/src/lib.rs` | P6 |
| `harw-runtime/src/job_wiring.rs` | P6 |
| the UIA prompt source (P6 locates it and reports the path before editing) | P6 |
| root `Cargo.toml` (member `harw-tool-remote`), `Cargo.lock`, `xtask/arch-policy.toml` (`harw-tool-remote` layer A) | coordinator |

### 10.3 Packages

**P1 — gateway tool host** (`harw-session-host`, `harw-session-ws`)
- `tool_host.rs`: tool registry of `Arc<dyn ToolExecutor>` + `ToolSpec` →
  `ToolDescriptor`; per-session gateway `SandboxSpec` (`ensure_child_of` for
  delegates); admission §4.2 (use `admit_tool_call`); in-flight map keyed by
  `(SessionId, ToolCallId)` with cancel tokens; approval via the session's
  approval store; emits `ToolCallRequested`/`ToolCallCompleted` with
  `placement`; `sandbox_available` probe.
- `agents.rs`: registry credential → `AgentPrincipal`; enroll UIA, delegate,
  grant, narrow (cascading), revoke.
- `HostConnection` implements `ToolPort` and `GatewayPort`; hello applies
  `ClientCaps::for_wire_minor`; drain flag.
- `dispatch.rs`: route `TOOL_METHODS`/`GATEWAY_*_METHODS` to the ports with
  strict param decoding; unknown methods stay `METHOD_NOT_FOUND`. For
  `PortError::ToolRefused` the wire `message` is the bare `detail` (the tool
  name or reason), so `PortError::from_code` on the client restores the same
  value without double wrapping.
- Tests: TG-01..TG-12 (§12).

**P2 — remote tool proxy** (`harw-tool-remote`, runtime wiring)
- `RemoteToolProvider: ToolProvider` built from `ToolPort::list_tools`;
  `RemoteToolExecutor: ToolExecutor` forwarding to `call_tool`, cancel via
  `cancel_tool` on the execution context's `CancelToken`.
- Every `PortError` → `ToolOutput`/result `Error` with runtime trust and the
  refusal named; no local fallback, no local executor registered next to it.
- Runtime: an entry/spec for "agent connected to a gateway" assembles
  `RegistryProfile::NoTools` + `RemoteToolProvider`; placement plumbing
  (`ToolExecutor` metadata → `ToolCallCompleted.placement`).
- Pin test `AgentRole` ⇔ `AgentRoleId` (all six names, both directions).
- Tests: RP-T1..RP-T6.

**P3 — `gateway.*` operations** (`harw-ops`)
- §6 table; `register_gateway`; `infra.rs` doc rule (§1 D-B verbatim);
  runtime `GatewayContributor` puts `Arc<dyn GatewayPort>` into the op
  context only when the runtime is connected to a gateway.
- Tests: GO-01..GO-06.

**P4 — WorkDriver precision** (`harw-cli`, `harw-ops`, `harw-plan-bridge`)
- §8; delete `parse_worker_return` and the free-text `RETURN_CONTRACT`;
  register `work_driver.report` for WorkDriver workers only; job-worker
  concurrency config in `job_worker.rs`.
- F6: `work_driver.enqueue` usable by the UIA (approval `always`) through a
  UIA `WorkDriverCaller` with a default spec from config
  (`harw-ops/src/work_driver.rs`); the runtime hook in `assembly.rs` is done by
  P2 exactly as P4 specifies it.
- Docs: `docs/guides/work-driver.md`.
- Tests: WD-01..WD-06.

**P5 — TUI** (`harw-tui`, `harw-tool-job` model/event)
- D-D: tool cells show placement (`host`/`sandbox`/`gateway`) instead of a
  command line; `None` shows nothing.
- F3: `shell.exec` label reflects the real interpreter: `Shell(...)` (not
  `Bash(...)`), in `history_cell.rs` and `export.rs`.
- F4: plan tool cells show only the delta (changed node + progress bar);
  the full node list only when expanded.
- F7: heredoc bodies in shell labels/cells are collapsed.
- F9: jobs group shows the failure reason per failed job.
- F2 (render side): the approval dialog shows the classifier fallback reason
  in plain words ("classifier unavailable: <reason>; asking you").
- `harw-tool-job` `model.rs`/`event.rs`: jobs carry the originating
  `call_id` so the TUI can link a job to its tool cell.
- Tests: TUI-01..TUI-07.

**P6 — execution semantics** (field evidence §11)
- F2: `auto_classifier.rs` retries once on an empty classifier reply
  (`harw_core::one_shot` "model returned an empty text response"), then uses
  a configured fallback classifier model (`harw-config/src/role_models.rs`),
  then fails closed to asking (`AutoVerdict::fallback_ask`) with a reason
  that names the cause.
- F3: `shell.exec` runs `/bin/sh`; its description states "POSIX sh (not
  bash): no `source`, use `.`; no arrays" (decision: document, do not switch
  the interpreter — keeps sandbox images minimal and behavior identical on
  host and sandbox).
- F7: `shell.exec`, `fs.edit`, `fs.write` descriptions and the UIA prompt
  steer edits to `fs.*` ("never edit files through shell heredocs or
  `python3 -`").
- F8: `job.wait` capped to a short poll (≤ 60 s, documented as such);
  job completion is delivered as a notification to the waiting agent
  (`manager.rs` → `job_wiring.rs`), so agents do not block.
- Tests: EX-01..EX-06.

---

## 11. Field evidence (2026-09-28)

Observed in live harw sessions (screenshots from Mia). Each item names its
package.

| # | Observation | Consequence | Package |
|---|---|---|---|
| F1 | Agents manage the gateway by running `harw gateway status` / `harw channel connect --help` through `shell.exec`; in the sandbox: `/bin/sh: 1: harw: not found`; on the host: a 62 s approval. | `gateway.*` tools cover status and channel list/connect-info; descriptions say to use them instead of the `harw` binary. | P3 |
| F2 | Approval dialog: "Auto-Modus: classifier-unavailable – Klassifizierer-Fehler: model returned an empty text response". | Retry once, then configured fallback classifier model, then fail closed to asking; render the reason clearly. | P6 (logic), P5 (render) |
| F3 | `shell.exec` runs `/bin/sh` (dash) but the TUI labels it `Bash(...)`; `source "$HOME/.cargo/env"` fails with `source: not found`. | Label `Shell(...)`; description states POSIX sh. | P5, P6 |
| F4 | `plan(action:"step", …)` re-renders the whole node list (95 nodes, "70 weitere Knoten ausgeblendet") on every step. | Delta rendering; full list only expanded. | P5 |
| F5 | Worker `executor` (`harw-registry-defaults/agents/executor.toml`, extends `worker-base`) ended with blocker "no execution/shell tools, only job_list/logs/status/stop/wait"; discovered only after it ran. | Spawn-time check that a worker's granted tools can do its declared job; fail at spawn with a clear reason. | R18b (`harw-runtime/src/children.rs`, `WorkerKind`) |
| F6 | `work_driver.enqueue` not available to the UIA ("keine [work_driver]-Sektion in meiner Definition"); had to hand off to the root orchestrator. | Consistent with D-A: the UIA may start a WorkDriver, with approval (backlog C-04). | P4 (+ P2 hook) |
| F7 | Agents edit files with `Bash(cd … && python3 - <<'EOF')` instead of `fs.edit`. | Descriptions/UIA prompt steer to `fs.*`; TUI collapses heredoc bodies. | P6, P5 |
| F8 | `job.wait(timeout_secs: 600)` blocks the assistant for long periods ("wartet 1h13m"). | Completion as notification; `job.wait` capped and documented as a short poll. | P6 |
| F9 | Many failed jobs (11/28, 21/40) without a visible reason in the job summary line. | Jobs group shows the failure reason per job. | P5 |

---

## 12. Tests per package

Wave 1 (landed): see §10.1; test names in the report of R18a-W1.

**P1**
- TG-01 human (device) identity with forged `tool_call` cap → `validate` fails; `tool.call` → `DENIED`.
- TG-02 UIA calls a granted tool → result frame, `placement = Gateway`, `ToolCallRequested`/`Completed` in the session stream.
- TG-03 unknown tool → `TOOL_UNKNOWN`; not granted → `TOOL_NOT_GRANTED`; foreign-tenant session → `NOT_FOUND`.
- TG-13 (integration) `tool_call_into_a_foreign_session_of_the_same_tenant_is_refused`: a second UIA of the same tenant gets `NOT_FOUND` for `tool.call`/`tool.list`/`tool.cancel` on a session it neither created nor was bound to (same answer as an unknown session); the creator's delegate is admitted; `bind_tool_agent` admits an agent (and its delegates) to a human-created session; cross-tenant binding → `DENIED`.
- TG-04 no sandbox backend → `TOOL_SANDBOX_UNAVAILABLE`, executor never invoked (spy).
- TG-05 draining → `HOST_DRAINING`; in-flight call completes.
- TG-06 duplicate `call_id` in flight → `TOOL_CALL_DUPLICATE`.
- TG-07 delegate grant ⊄ parent → `DENIED`; narrowing the parent narrows the child for the next call.
- TG-08 revoke agent principal mid-call → call cancelled (`Error`, runtime trust), next call `REVOKED`.
- TG-09 approval `Always` tool: denied approval → result `Error`, executor never invoked.
- TG-10 minor-1 hello never receives R18 caps; minor-2 agent hello receives `tool_call`.
- TG-11 WS dispatch: `tool.call` with an extra `principal` field → `INVALID_PARAMS`, port not called; unknown `tool.*` method → `METHOD_NOT_FOUND`.
- TG-12 `gateway.*` read without `gateway_read` → `DENIED`; admin without `gateway_admin` → `DENIED`.

**P2**
- RP-T1 proxy lists exactly the port's descriptors; nothing else registered (`NoTools`).
- RP-T2 proxy call returns the frame's result and placement.
- RP-T3 each `PortError`/`ToolRefusal` → result `Error` naming the refusal; a local spy executor is never called.
- RP-T4 cancel token → `tool.cancel` sent once.
- RP-T5 `AgentRole` ⇔ `AgentRoleId` pin.
- RP-T6 local sandboxed executor completion carries `placement = Sandbox`; host escalation carries `Host`.

**P3**
- GO-01 read ops declare `ModelTool { readonly: true, approval: None }`.
- GO-02 mutating ops declare `approval: Always` on every model/web surface.
- GO-03 no `infra.auth.keys.*` op has a model surface (registry scan).
- GO-04 without a gateway port every port-backed op answers `NotAvailable`; `gateway.health`/`gateway.logs` need the bound harw home; `gateway.channels.*` work without a port and answer `NotAvailable` only without the channel configuration.
- GO-05 outputs contain no token/key-shaped strings (fixture with secrets).
- GO-06 descriptions of `gateway.status`/`gateway.channels.*` mention not to shell out to `harw`.

**P4**
- WD-01 worker calls `work_driver.report` → summary equals `into_summary`.
- WD-02 no report → `Partial`, summary starts with `no report`.
- WD-03 invalid report → tool error to the worker, not counted.
- WD-04 last valid report of the round wins.
- WD-05 `work_driver.report` is not offered outside WorkDriver workers.
- WD-06 UIA `work_driver.enqueue` requires approval and uses the UIA caller spec.

**P5**
- TUI-01 tool cell shows placement label; `None` shows none.
- TUI-02 `shell.exec` label is `Shell(...)`.
- TUI-03 plan step cell renders delta only; expanded renders full list.
- TUI-04 heredoc body collapsed in shell cell.
- TUI-05 failed job line shows its reason.
- TUI-06 approval dialog shows classifier fallback reason.
- TUI-07 job ↔ tool cell link via `call_id`.

**P6**
- EX-01 empty classifier reply → one retry, then fallback model, then ask (never allow).
- EX-02 fallback reason text names the cause.
- EX-03 `shell.exec` description mentions POSIX sh.
- EX-04 `fs.edit`/`shell.exec` descriptions steer edits to `fs.*`.
- EX-05 `job.wait` above the cap → `InvalidArguments` (no silent clamp).
- EX-06 job completion produces a notification to the waiting session.

---

## 12a. Integration notes (R18a integration pass)

- **Drain cancels running turns.** `SessionHost::drain` keeps the W00
  behaviour: running turns are cancelled (they end `Interrupted` and resume
  after the restart by policy); only in-flight **tool calls** finish. This
  is intended for R18a; §7's "running turns … finish" is to be read as
  "in-flight tool calls finish, running turns are interrupted and resumed".
- **Two approvals in remote mode.** An agent connected to the gateway can
  be asked twice for one tool call: by its local approval chain (the remote
  proxy tool is an ordinary tool to the turn loop) and by the gateway tool
  host (`ToolDescriptor.approval`). Known; R18b collapses this to the
  gateway approval.
- **FullAccess bypasses `ALWAYS_ASK_TOOLS`.** Unchanged existing semantics
  (user decision 2026-09-24): under `FullAccess` harw never asks, including
  the mutating `gateway.*` tools. The gateway still checks `gateway_admin`
  and the tenant scope, so approval never expands authority.
- **`trust` is dropped by the proxy.** `harw_tools::ToolOutput` has no trust
  field, so `ToolCallResultFrame.trust` from the gateway is not carried into
  the local turn loop (the loop treats tool output as untrusted anyway).
  Follow-up: a trust field on `ToolOutput` or a placement/trust side channel.
- **Session binding.** See §4.2 step 1: a hosted session admits gateway tool
  calls only from its creating agent, that agent's delegates and agents the
  composition bound with `SessionHost::bind_tool_agent`.

---

## 13. Open questions

1. Agent credential format per transport (W04/W05) — UDS peer pid/uid
   registration vs. enrolled agent key on the node transport.
2. Production composition of the gateway tool host depends on W04 (no
   production `SessionHost` composition exists yet); R18a proves the path
   in process and over the WS test harness.
3. A WS client implementing `ToolPort` for remote agent processes belongs to
   `harw-session-remote` (W00 §2.5, not yet existing); P2 targets the
   in-process port until then.
4. `production` hub definition (§9) — to confirm with Mia.
5. Whether `ToolResultItem` (persisted transcript) should also carry
   `placement` (would need a transcript compat step; not in R18a).
6. Live tool output over the wire (`ToolDelta`) — deferred (§2.5).

---

## 14. Verification

Central build only, once, after all wave-2 packages (CLAUDE.md):

```text
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace   (including doc tests)
cargo run -q -p xtask -- gates
cargo deny check
make -C dod clippy test
actionlint
```
