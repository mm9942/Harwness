# DEC-048: Control-plane session transport (W00)

```
id: DEC-048
status: accepted (proposed, then accepted by operator request, 2026-09-27)
supersedes-in-part: PL-65 §3.1 (carrier) and §7 ("No new third-party crates"), for tokio-tungstenite only
related: PL-65, DEC-003, DEC-007, DEC-008
```
The copy that lands in the repo must be written in German, like DEC-001 to DEC-008 (`docs/planning/70-decisions/README.md:10-26`). DEC-048 is the next free number: the docs reserve numbers up to DEC-047 (`docs/planning/67-containers/cell.md:246,289`), and a grep for DEC-048 finds nothing.

## Context
**How harw-web works today:**
- It serves hyper HTTP/1 over a Unix socket. It uses `auto::Builder…http1_only()` with `serve_connection`, so upgrades are off (`harw-web/src/server.rs:494-510,594-600`).
- It reads SO_PEERCRED once per connection (`server.rs:488`) and resolves identity again on every request (`server.rs:676-683,755-762`).
- Routes are built only from the registry (`router.rs:369-405`). The decision checks path, then method, then identity, then tier, then approval (`router.rs:262-298`).
- There is no connection cap: the JoinSet is unbounded (`server.rs:424,497`).
- `GET /events` is one bus for the whole process. Its sequence lives in memory, it has no replay and no tenant filter (`events.rs:109-126,216-254`; `server.rs:838-910`).

**harw-protocol today:**
- Envelopes are versioned and major version 1 is enforced (`wire.rs:19-40`).
- WebSocket appears only in a doc comment (`wire.rs:1-2`).
- The method constants are unused (`methods.rs:3-10`).
- There is no `session_wire.rs` and no `session_port.rs`.

**Durable session state today:**
- The only durable session state is the JSONL transcript, ordered by `sequence` (`harw-session-store/src/record.rs:37-38`).
- `rewrite` replaces the transcript without any generation counter, so it silently invalidates cursors (`store.rs:267-303`).
- There is no cross-process writer lease. The sequence cache is per process (`harw-core/src/state_store.rs:713-777`).

**What PL-65 decided:**
- Its status is "planned" (`docs/planning/65-cloud-sessions/README.md:4`).
- It chose `POST /v1/rpc` plus NDJSON frames and rejected WebSocket for two reasons: the same route works over UDS and NodeTransport, and tokio-tungstenite comes in only transitively through thirtyfour (`README.md:136-143`).
- §7 says "No new third-party crates" (`README.md:539`).
- Its decision note DEC-012 was never written (`70-decisions/README.md:19-26`).

**The dependency today:** tokio-tungstenite 0.30.0 is locked only because thirtyfour depends on it (`Cargo.lock:7142-7155`; `docs/architecture/dependency-review.md:83`). Both 0.30.0 crates declare `rust-version = "1.85"` (registry `Cargo.toml:14`).

**The "older multi-node plan with multiplexed WebSockets" is not in this repo.** The nearest plan says two things:
- Management RPC may stay one-shot (`Harwness_Crypto_Infrastructure_Masterplan_v2.md:1297`).
- "Do not build a second local HTTP stack" (`…Masterplan_v2.md:127`).

## Decision
1. **Carrier.**
   - RFC 6455 WebSocket on the existing harw-web UDS listener, at the reserved path `GET /v1/ws`.
   - Subprotocol `harw.session.v1`, which is mandatory.
   - One WebSocket text message carries exactly one `WireMessage` (`wire.rs:147`). Binary messages close the connection with code 1003.
2. **What uses WebSocket:**
   - Session control: `session.hello`, `session.list`, `session.create`, `session.attach`, `turn.submit`, `turn.interrupt`.
   - Approvals, through the existing `approval.resolve` operation (`harw-ops/src/approval.rs:404-411`). No `approval.respond` operation is added.
   - Live session frames, sent as the notification `event.frame`.
   - Any registry operation that declares `Surface::Web` can be called by its name, under exactly the HTTP authorization. There are no WebSocket-only operations.
3. **Node control: no WebSocket.** It stays one-shot HTTP/1 over harw-node-transport, which has no upgrades (`harw-node-transport/src/server.rs:292-298`). Three reasons:
   - The Masterplan allows one-shot management RPC (`Masterplan_v2.md:1297`).
   - `AuthenticatedPeer{node_id}` (`handshake.rs:321-326`) has no mapping to a tenant or device, because no DeviceRegistry exists.
   - Upgrades would widen the attack surface of the node listener.

   Remote session attach over NodeTransport needs a separate operator decision.
4. **Consumers that stay on HTTP, SSE or NDJSON:**

| Consumer | Stays until |
|---|---|
| `/api/*` routes (the same operations, one-shot) | Permanent. WebSocket is a second carrier, not a replacement. |
| `GET /events` SSE (`webui/lib/sse.ts:155`) | Until the webui has switched to `event.frame` and a follow-up DEC retires it. It is never session replay. |
| MCP streamable-HTTP SSE (`harw-mcp-server/src/transport.rs:668-671`) | Permanent. It is not control plane. |
| NodeTransport NDJSON uplink (`harw-node-transport/src/uplink.rs:35-36`) | Permanent (DoD telemetry). |
| PL-65 `POST /v1/rpc` and NDJSON `/v1/sessions/{id}/frames` | Not built for local use. If it is built later for remote clients, it is a thin adapter over the same `SessionPort` and `FrameSource`. |

5. **What happens to PL-65:**
   - Adopted with changes: §2.4 (single writer, FIFO 8, `expect_head`, `client_msg_id` LRU 256, approvals first-wins) and §3.2-3.6 (types, cursor, replay, backpressure, versioning).
   - The client port of §3.3 is renamed `SessionClient` and is not built. `SessionPort` becomes the server-side contract that every transport adapter calls.

## One contract, used by both transports
- **Where the contract lives.** Wire types go in `harw-protocol/src/session_wire.rs` and the port in `harw-protocol/src/session_port.rs`. This is ring F, using std futures only (`xtask/arch-policy.toml:45,97`).
- **One call path.** Both carriers take the same route: registry operation → `harw-ops/src/session.rs` → `Arc<dyn SessionPort>` from the `OpContext` services. There is no other path into the host.
- **Versioning.** It is checked at three points:
  - the subprotocol at upgrade;
  - `ProtocolVersion.major == 1` on every request (`wire.rs:19-40`);
  - `session.hello`, which sets `wire_minor = min(client, host)` and exchanges a features list.

  New frame kinds are additive and old clients see `SessionFrame::Unknown`. A breaking change means major 2, subprotocol `harw.session.v2` and path `/v2/ws`.
- **Error model.** Errors travel as `ResponseEnvelope.error = WireError{code,message,data:{reason}}` (`wire.rs:133-139`) with fixed codes:

| Code | Meaning |
|---|---|
| -32700 | parse error |
| -32600 | invalid request |
| -32601 | method not found (maps from `RouteDecision::NotFound`) |
| -32602 | invalid params (maps from `OpError::InvalidArguments`) |
| -32000 | forbidden; `reason` = `forbidden_reason_str` (`server.rs:1032`) or `approval_required` |
| -32001 | not available (`OpError::NotAvailable`) |
| -32002 | execution error (`OpError::Execution`) |
| -32003 | busy |
| -32004 | revoked |
| -32005 | unsupported version |

  HTTP keeps its current status mapping (`server.rs:1023-1030`).

## Owners
| Concern | Owner |
|---|---|
| Frames, cursor, caps, wait reasons, error codes | `harw-protocol/src/session_wire.rs` (F) |
| `SessionPort`, `FrameSource`, `CallerScope`, `AttachSlot` | `harw-protocol/src/session_port.rs` (F) |
| Generation, head, `read_from` | `harw-session-store/src/store.rs` (I) |
| Writer lease | `harw-session-store/src/writer_lease.rs` (I, new) |
| Tenant of a hosted session | `harw-session-store/src/hosted.rs` (I, new) |
| Single writer, arbiter, fan-out, replay, limits | `harw-session-host` (A, new, **no listener**, no harw-web dependency) |
| Session operations and `CallerScope` derivation | `harw-ops/src/session.rs` (A) |
| Upgrade, frame loop, WebSocket bounds | `harw-web/src/ws.rs` (A) |
| Upgrade-capable serving, connection cap | `harw-web/src/server.rs` |
| Turn driver over `AgentSession`, and composition | `harw-cli/src/runtime_session_host.rs`, `harw-cli/src/web.rs` |

## Security invariants
1. **Identity comes only from the connection.** It is built from SO_PEERCRED plus the `x-harw-security-context` reference of the upgrade request, run through the configured `LocalPeerIdentityResolver` (`identity.rs:309-319`).
   - A frame never carries actor, tenant or principal. The param types use `deny_unknown_fields`.
   - `CallerScope` has no `Deserialize`, so no frame can produce one.
2. **Every request frame is re-resolved and then authorized** by `decide_resolved_operation`. That uses the same `tier_permits` and `ApprovalPolicy` gate as HTTP (`router.rs:285-297`). `approval = "always"` operations stay refused over WebSocket.
3. **The actor for approvals** comes from the Principal, the peer and the resolver table (`approval.rs:411-430`). The resume actor is read from the durable resolution record (`harw-session-store/src/approval.rs:344`).
4. **Tenant isolation.** Every port call checks `tenant_admits` (`harw-operations/src/context.rs:415`) against the `HostedSessionRecord`. A foreign session looks exactly like an unknown one.
5. **No second authority path.** The WebSocket code lives inside harw-web and uses its private resolve, decide and scope functions (`server.rs:755,780`; `router.rs:262`).
6. **The Warden never depends on any of this.**
   - Ring D may not depend on ring A (`arch-policy.toml:52`).
   - The TCB allowlist (`arch-policy.toml:383-390`) blocks new edges.
   - tungstenite is added to `CRYPTO_AND_HTTP_STACK` (`xtask/src/gate_edges.rs:191`).
7. **Any request with an `Origin` header is refused** by default (ClawJacked, `docs/planning/75-harness-patterns/openclaw.md:157`).

## Limits
| Bound | Value |
|---|---|
| harw-web connections (all) | 256. Parity with `harw-auth-hub/src/server.rs:84` and `harw-node-transport/src/server.rs:53`. |
| WebSocket connections | 64. Beyond that, 503 `busy` before the upgrade. |
| Inbound message and frame size | 64 KiB, the same as `MAX_BODY_BYTES` (`server.rs:194`). Over the limit, close 1009. |
| Outbound message size | 2 MiB. Records are at most 1 MiB (`harw-session-store/src/reader.rs:22`). Larger frames become `Resync{OversizeFrame}`. |
| WebSocket write buffer | `write_buffer_size` 128 KiB and `max_write_buffer_size` 4 MiB, as a validated constant, because `from_raw_socket` panics on an invalid config (tungstenite `protocol/mod.rs:146-151`). |
| Per-attachment outbound queue | 1024 frames or 4 MiB (PL-65 `README.md:244-249`) |
| Attachments | 8 per connection, 16 per session |
| In-flight requests per connection | 4. Beyond that, `busy`. |
| Session FIFO / dedupe window | 8 / 256 |
| Running turns per host | 4, clamped to provider concurrency (DEC-003) |
| Submit text / `client_msg_id` | 48 KiB / 1-128 characters `[A-Za-z0-9._-]` |
| Timeouts | Upgrade header 10 s (`server.rs:204`). Hello 5 s. Ping every 15 s, pong within 30 s. Idle 120 s. Lifetime 10 min, then close 1001. Identity re-check on every frame and every 30 s. Write 10 s, then close 1013. |

## Slow consumers
- The host publishes with a non-blocking try-push only. When an attachment's queue overflows, the queue is dropped, the attachment gets `Lagged{resume_from}` as its final frame, and then it is closed. The turn loop is never blocked.
- If a single WebSocket write stalls past 10 s, the connection closes with 1013 and the client re-attaches from its cursor.
- If the driver's `AgentEventHub` subscription lags (`harw-core/src/agent_events.rs:20,66-104`), every attachment of that session gets `Resync{HostLagged}`.

## Reconnect
The client keeps the `cursor` of the last frame it received. On reconnect:
1. It sends `hello`, then `session.attach{from}`.
2. If the generation differs, the host answers `Resync{Generation}`. If the cursor's durable position is beyond the head, it answers `Resync{CursorAhead}`.
3. Otherwise it replays durable records with `sequence ≥ durable`, mapped as in PL-65 §3.4.
4. It then sends the pending approvals (`approval.rs:307`) and a `State` frame.
5. Then the stream continues live.

Three consequences:
- **No live ring in W11.** `Cursor.live` is always 0. The partial text of a running turn is lost until its durable item lands. `Snapshot` is deferred.
- **Restart.** `host_epoch` changes, but cursors stay valid because the generation is persisted in `<id>.gen`.
- **Detach does not cancel.** Dropping a `FrameSource` only removes the attachment. Queued submits and running turns continue.

## Workbench wait reasons
- `HostedState::Queued{depth, reason}` with `WaitReason::HostCapacity{running,limit}` or `SessionQueue{position,depth}` separates host capacity from the session's own FIFO.
- `SessionFrame::Agent(AgentOrchestrationEvent)` carries the run and agent scope, using `root_session_id` (`harw-protocol/src/orchestration.rs:24-50`).
- `session.list` (GET, observer tier) is the Workbench data source.
- Child-cap and provider waits need a pending-spawn identity first (`harw-core/src/child_controller.rs:8718`). That is an operator decision.

## Consequences
- harw-web gets a direct Tier-B dependency.
  - Builds without the browser now compile tungstenite, sha1 0.11 and data-encoding.
  - The arch gate computes closures from lockfile edges without feature filtering (`dependency-review.md:43-46`), so harw-web's closure counts rustls and aws-lc. Ring A allows that.
- harw-web switches from hyper-util `auto` to `hyper::server::conn::http1::Builder…with_upgrades()`, the same pattern as `harw-node-transport/src/server.rs:292-298`. The header-read timeout still counts from connection start (`server.rs:586-593`).
- The host becomes the transcript writer for hosted sessions. The local TUI is not converted.

## Alternatives rejected
| Alternative | Why rejected |
|---|---|
| PL-65 NDJSON as planned | Superseded by the operator's direction. Its types are kept. |
| SSE plus POST | One-directional, and the existing bus is process-wide. |
| Hand-written RFC 6455 | Brings back the parser attack surface that `dependency-review.md:68` removes. |
| A separate `harw-control-ws` crate, or a listener inside `harw-session-host` (PL-65 §4.1) | Would need harw-web's private identity and route internals made public. That risks a second authority path and a second local HTTP stack (`Masterplan_v2.md:127`). |
| `auto::serve_connection_with_upgrades` | Ignores `http1_only` and sniffs the protocol with no timer (hyper-util `auto/mod.rs:127-131,256-274`). |
| Multiplexed WebSocket for node control | No evidence in the repo, and one-shot RPC is enough. |

=====CONTRACT=====

# W11 implementation contract

**Scope of this report:**
- **Design decisions:** DEC-048 above.
- **Edges to implement:** below.
- **Tests executed:** none. This was a read-only mapping and no cargo or rustc was run.
- **Starting point:** the files `harw-web/src/{events,identity,meta,router,server}.rs`, `harw-ops/src/lib.rs`, `harw-cli/src/lib.rs`, `harw-protocol/src/{approvals,items}.rs` and `xtask/src/gate_edges.rs` are dirty (R16 round, HEAD `0fc69ed`). W11 starts after R16 merges. Re-anchor every line number by symbol name.

**Rules for every coder:**
- MSRV 1.85 and no let-chains.
- Ports use boxed `Pin<Box<dyn Future…+Send>>`.
- No unwrap, expect or panic anywhere, including tests. Tests return `TestResult`.
- Mutex poisoning maps to an error.
- Comment language:
  - German: harw-web, harw-protocol, harw-ops, harw-cli, deny.toml, arch-policy, gate_edges, 70-decisions.
  - English: `harw-session-store/src/{store,writer_lease,hosted}.rs`, all of harw-session-host, PL-65, dependency-review.

## Wave 0: policy and docs
| File | Edit |
|---|---|
| `docs/planning/70-decisions/DEC-048-control-plane-session-transport.md` | New file: DEC-048 above, in German. |
| `docs/planning/70-decisions/README.md` | Add the index row DEC-048. |
| `docs/planning/65-cloud-sessions/README.md` | Add amendment notes at §3.1 (:136-143), §3.3 (:209-230, `SessionClient` rename) and §7 (:539). |
| `docs/architecture/dependency-review.md` | Add a Tier-B row after :83 (details below). |
| `deny.toml` | Add two entries to `[bans].deny` (:77-81), shown below. Check the `wrappers` syntax against the pinned cargo-deny. |
| `xtask/arch-policy.toml` | Add `[packages."harw-session-host"]` with `layer = "A"` next to `harw-runtime` (:345). |
| `xtask/src/gate_edges.rs` | Add `"tungstenite"` and `"tokio-tungstenite"` to `CRYPTO_AND_HTTP_STACK` (:191). Extend the text of `TCB_NO_CRYPTO` (:202). Test: `test_crypto_and_http_stack_names_websocket_crates`. |
| `Cargo.toml` (root) | Add `"harw-session-host"` to `members`. No workspace-dependency entry. |

The dependency-review row: `tokio-tungstenite`/`tungstenite` 0.30.0, `default-features=false, features=["handshake"]`.
- Used by: `harw-web` (direct), `harw-session-host` (dev-dependency, test client), `thirtyfour` (transitive).
- Rings: A only, never I, J, D or T.

The deny.toml entries:
```toml
{ crate = "tokio-tungstenite", wrappers = ["thirtyfour", "harw-web", "harw-session-host"] },
{ crate = "tungstenite", wrappers = ["tokio-tungstenite"] },
```

## Wave 1: contract and store

**`harw-protocol/src/session_wire.rs` (new).** All structs derive `Debug, Clone, PartialEq, Serialize, Deserialize` with `#[serde(deny_unknown_fields)]`.
```rust
pub const SESSION_WIRE_MINOR: u32 = 0;
pub const WS_SUBPROTOCOL: &str = "harw.session.v1";
pub const MAX_SUBMIT_TEXT_BYTES: usize = 48 * 1024;
pub const MAX_CLIENT_MSG_ID_CHARS: usize = 128;
pub struct Cursor { pub generation: u32, pub durable: u64, pub live: u32 }   // + Copy, Eq, Ord, Hash
impl Cursor { pub const START: Self; }
pub struct ClientCaps { pub observe: bool, pub steer: bool, pub approve: bool, pub control: bool } // + Copy, Default
impl ClientCaps { pub const NONE: Self; pub const OBSERVER: Self;
  #[must_use] pub fn for_tier(tier: harw_types::PermissionTier) -> Self; // Observer→observe; Operator→+steer+approve; Maintainer/Owner→all
  #[must_use] pub fn narrow(self, other: Self) -> Self; }
#[serde(rename_all = "snake_case")] pub enum StreamProfile { Full, Compact } // Compact == Full in W11
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WaitReason { HostCapacity { running: u32, limit: u32 }, SessionQueue { position: u32, depth: u32 },
  Approval { request: ItemId }, Child, ProviderLimit { detail: String }, #[serde(other)] Other }
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HostedState { Idle, Running { turn_id: TurnId }, Queued { depth: u32, reason: WaitReason },
  WaitingForApproval { request: ItemId }, WaitingForChild, Interrupted, Failed { reason: String }, Closed, #[serde(other)] Unknown }
pub struct HelloParams { pub client_label: String, pub wire_minor: u32, #[serde(default)] pub features: Vec<String> }
pub struct HelloAck { pub wire_minor: u32, pub features: Vec<String>, pub host_epoch: u64, pub granted: ClientCaps }
pub struct SessionSummary { pub session_id: SessionId, pub tenant: Option<TenantId>, pub state: HostedState,
  pub attached: u32, pub head: Cursor, pub updated_at: jiff::Timestamp }
pub struct CreateParams { #[serde(default)] pub title: Option<String> }
pub struct AttachParams { pub session_id: SessionId, #[serde(default)] pub from: Option<Cursor>,
  pub profile: StreamProfile, #[serde(default = "default_tail_items")] pub tail_items: u32 } // default 200
pub struct AttachAck { pub session: SessionSummary, pub granted: ClientCaps, pub head: Cursor, pub replay_from: Cursor, pub host_epoch: u64 }
pub struct SubmitParams { pub session_id: SessionId, pub text: String, pub expect_head: Cursor,
  pub client_msg_id: String, #[serde(default)] pub force: bool }
impl SubmitParams { pub fn validate(&self) -> Result<(), String>; }
#[serde(tag = "result", rename_all = "snake_case")]
pub enum SubmitResult { Accepted { position: u32 }, Duplicate { position: Option<u32> }, Stale { head: Cursor }, QueueFull, Denied { reason: String } }
pub struct InterruptParams { pub session_id: SessionId, #[serde(default)] pub turn_id: Option<TurnId> }
#[serde(rename_all = "snake_case")] pub enum ResyncReason { Generation, CursorAhead, HostLagged, OversizeFrame }
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum SessionFrame { Turn(TurnEvent), Session(SessionEvent), Agent(AgentOrchestrationEvent),
  ApprovalRequested(ApprovalRequest), ApprovalResolved { request_id: ItemId, decision: ReviewDecision, by: String },
  State(HostedState), HistoryReplaced { item_count: u32 }, Resync { reason: ResyncReason, head: Cursor },
  Lagged { resume_from: Cursor }, HostDraining { retry_after_ms: u64 }, Revoked, Heartbeat, #[serde(other)] Unknown }
pub struct FrameEnvelope { pub session_id: SessionId, pub cursor: Cursor, pub frame: SessionFrame }
pub mod error_code { pub const PARSE_ERROR: i32 = -32700; pub const INVALID_REQUEST: i32 = -32600;
  pub const METHOD_NOT_FOUND: i32 = -32601; pub const INVALID_PARAMS: i32 = -32602; pub const FORBIDDEN: i32 = -32000;
  pub const NOT_AVAILABLE: i32 = -32001; pub const EXECUTION: i32 = -32002; pub const BUSY: i32 = -32003;
  pub const REVOKED: i32 = -32004; pub const UNSUPPORTED_VERSION: i32 = -32005; }
```
Tests:
- `test_cursor_roundtrip_and_ordering`
- `test_submit_params_reject_actor_tenant_fields`
- `test_submit_validate_bounds`
- `test_session_frame_unknown_kind_maps_to_unknown` (the PL-65 "verify" item, `README.md:196`)
- `test_wait_reason_unknown_kind_maps_to_other`
- `test_hosted_state_queued_roundtrip`
- `test_caps_for_tier_table`
- `test_caps_narrow_only_removes`

**`harw-protocol/src/session_port.rs` (new).**
```rust
pub type PortFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, PortError>> + Send + 'a>>;
pub trait FrameSource: Send { fn next(&mut self) -> PortFuture<'_, Option<FrameEnvelope>>; }
#[derive(Debug, Clone, PartialEq, Eq)]           // deliberately no Serialize/Deserialize
pub struct CallerScope { principal_id: String, tenant: Option<TenantId>, caps: ClientCaps }
impl CallerScope { #[must_use] pub fn new(principal_id: String, tenant: Option<TenantId>, caps: ClientCaps) -> Self;
  pub fn principal_id(&self) -> &str; pub fn tenant(&self) -> Option<&TenantId>; pub fn caps(&self) -> ClientCaps; }
pub trait SessionPort: Send + Sync {
  fn hello(&self, scope: CallerScope, p: HelloParams) -> PortFuture<'_, HelloAck>;
  fn list(&self, scope: CallerScope) -> PortFuture<'_, Vec<SessionSummary>>;
  fn create(&self, scope: CallerScope, p: CreateParams) -> PortFuture<'_, SessionSummary>;
  fn attach(&self, scope: CallerScope, p: AttachParams) -> PortFuture<'_, (AttachAck, Box<dyn FrameSource>)>;
  fn submit(&self, scope: CallerScope, p: SubmitParams) -> PortFuture<'_, SubmitResult>;
  fn interrupt(&self, scope: CallerScope, p: InterruptParams) -> PortFuture<'_, bool>;
  fn approval_resolved(&self, scope: CallerScope, session: SessionId, request: ItemId) -> PortFuture<'_, ()>; }
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortError { NotFound, Denied(String), WriterBusy, Busy, Protocol(String), Revoked, Internal(String) } // hand-written Display + Error
pub struct AttachSlot { inner: std::sync::Mutex<Option<Box<dyn FrameSource>>> }
impl AttachSlot { #[must_use] pub fn new() -> Self; pub fn put(&self, s: Box<dyn FrameSource>) -> Result<(), PortError>; // Err if occupied
  pub fn take(&self) -> Option<Box<dyn FrameSource>>; }   // + Default
```
Tests:
- `test_attach_slot_put_twice_is_error`
- `test_attach_slot_take_empties`
- `test_port_error_display_is_stable`

**`harw-protocol/src/methods.rs`.** After :3-10 add `METHOD_SESSION_HELLO`, `_LIST`, `_CREATE`, `_ATTACH`, `_DETACH` (`"session.detach"`, transport-only), `METHOD_APPROVAL_RESOLVE = "approval.resolve"` and `NOTIF_EVENT_FRAME = "event.frame"`. Add a doc line saying `METHOD_APPROVAL_RESP` is not served. Nothing is removed (additive rule, `lib.rs:4`).

**`harw-protocol/src/lib.rs`.** Add `pub mod session_port; pub mod session_wire;` and re-export the main types.

**`harw-session-store/src/store.rs`.**
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub struct TranscriptHead { pub generation: u32, pub next_sequence: u64 }
impl TranscriptStore {
  pub fn generation(&self, session_id: &SessionId) -> SessionStoreResult<u32>;          // `<id>.gen`, missing → 0
  pub fn head(&self, session_id: &SessionId) -> SessionStoreResult<TranscriptHead>;     // missing transcript → next_sequence 0
  pub fn read_from(&self, session_id: &SessionId, from_sequence: u64)
      -> SessionStoreResult<impl Iterator<Item = SessionStoreResult<TranscriptRecord>>>; // linear scan
}
```
- `rewrite` (:267) bumps `<id>.gen` atomically, under the same lock and **before** the transcript is persisted. A crash in between can only cause an unnecessary resync.
- A malformed generation file maps to `CorruptRecord{detail}`. No new error variants.

Tests:
- `test_rewrite_bumps_generation`
- `test_head_counts_appended_records`
- `test_read_from_skips_earlier_sequences`
- `test_generation_missing_file_is_zero`
- `test_corrupt_generation_file_is_error`

**`harw-session-store/src/writer_lease.rs` (new).**
```rust
pub struct SessionWriterLease { file: std::fs::File, session_id: SessionId }
impl SessionWriterLease { pub fn acquire(root: &Path, session_id: &SessionId) -> SessionStoreResult<Self>; // fs4 try-lock `<id>.writer.lock`; busy → LockContended{session}
  pub fn session_id(&self) -> &SessionId; }   // Drop unlocks, ignoring the error
```
Tests:
- `test_second_acquire_is_lock_contended`
- `test_drop_releases_lease`
- `test_lease_rejects_symlink_path`

**`harw-session-store/src/hosted.rs` (new).**
```rust
pub struct HostedSessionRecord { pub version: u16, pub session_id: SessionId, pub tenant: Option<TenantId>, pub created_by: String, pub created_at: jiff::Timestamp }
pub struct HostedSessionStore { root: PathBuf }
impl HostedSessionStore { pub fn new(root: &Path) -> Self;
  pub fn create(&self, r: &HostedSessionRecord) -> SessionStoreResult<()>;   // persist_noclobber (store.rs:493)
  pub fn get(&self, id: &SessionId) -> SessionStoreResult<Option<HostedSessionRecord>>;
  pub fn list(&self) -> SessionStoreResult<Vec<HostedSessionRecord>>; }
```
Tests:
- `test_create_is_noclobber`
- `test_get_roundtrip`
- `test_list_skips_foreign_files`

**`harw-session-store/src/lib.rs`.** Add the modules and re-exports (the list is at :24-52).

**`harw-operations/src/context.rs`.** Add:
```rust
#[must_use] pub fn with_service<S: Any + Send + Sync>(mut self, service: S) -> Self
```
Test: `test_with_service_is_visible_via_service`.

## Wave 2: `harw-session-host` (new crate, ring A)

**`Cargo.toml`:**
- Dependencies: `harw-protocol`, `harw-types`, `harw-session-store`, `harw-operations` (for `tenant_admits`), `tokio` (rt, sync, time, macros), `serde_json`, `jiff`, `tracing`.
- Dev-dependencies: `tempfile`, `harw-web`, `harw-ops`, `harw-authority`, and `tokio-tungstenite = { version = "0.30.0", default-features = false, features = ["handshake"] }`.
- `[lints] workspace = true`.

**`src/error.rs`:** `pub enum HostError { Store(SessionStoreError), WriterBusy, NotFound, Limits(String), Driver(String) }`. `Display` and `Error` are written by hand, plus `impl From<HostError> for PortError`.

**`src/limits.rs`:**
```rust
pub struct HostLimits { pub max_running_turns: u32, pub queue_cap_per_session: u32, pub dedupe_window: u32,
  pub max_attachments_per_session: u32, pub outbound_queue_frames: u32, pub outbound_queue_bytes: u64, pub max_frame_bytes: u64 }
```
Defaults are 4, 8, 256, 16, 1024, 4 MiB and 2 MiB. Also `pub fn validate(self) -> Result<Self, HostError>`. Test: `test_default_limits_validate`, `test_zero_limits_rejected`.

**`src/arbiter.rs`:** `pub(crate) struct Arbiter` with:
- `new(queue_cap, dedupe_window)`
- `admit(&mut self, head: Cursor, p: &SubmitParams) -> SubmitResult` — dedupe runs **before** the head check. A submit is stale when `!force` and either the generation differs or `expect_head.durable < head.durable`.
- `pop_next() -> Option<QueuedSubmit>`
- `depth() -> u32`

Tests: `test_duplicate_returns_duplicate_even_with_old_head`, `test_stale_head`, `test_force_bypasses_head`, `test_queue_full`, `test_dedupe_window_evicts_lru`.

**`src/fanout.rs`:** `pub(crate) struct Fanout`. Each attachment has a `std::sync::Mutex<VecDeque<FrameEnvelope>>`, a byte counter and a `tokio::sync::Notify`.
- `publish(&self, f: FrameEnvelope)` never awaits.
- Overflow clears the queue, pushes `Lagged{resume_from}` and closes the attachment.
- `AttachmentSource: FrameSource`. Dropping it detaches.

Tests:
- `test_publish_never_blocks_on_full_queue`
- `test_overflow_yields_lagged_then_end`
- `test_queue_bounded_by_bytes`
- `test_drop_source_detaches`
- `test_max_attachments_per_session`

**`src/replay.rs`:** `pub(crate) fn plan_replay(store: &TranscriptStore, s: &SessionId, from: Option<Cursor>, tail: u32, head: TranscriptHead, cap: u32) -> Result<ReplayPlan, HostError>`, with `enum ReplayPlan { Frames(Vec<FrameEnvelope>), Resync{reason, head} }`. The mapping follows PL-65 §3.4 (`README.md:232-242`). Past `cap` frames the plan ends with `Lagged`.

Tests: `test_generation_mismatch_resyncs`, `test_cursor_ahead_resyncs`, `test_replay_from_middle`, `test_replay_cap_ends_with_lagged`.

**`src/driver.rs`:**
```rust
pub type DriverFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, HostError>> + Send + 'a>>;
pub enum DriverOutcome { Completed, AwaitingApproval { request: ItemId }, AwaitingChild, Cancelled { reason: String }, Failed { reason: String } }
pub struct TurnSubmission { pub session_id: SessionId, pub turn_id: TurnId, pub text: String, pub submitted_by: String }
pub trait LiveSink: Send + Sync { fn turn(&self, e: TurnEvent); fn agent(&self, e: AgentOrchestrationEvent); fn lagged(&self); } // non-blocking
pub trait TurnDriver: Send + Sync + 'static {
  fn run_turn(&self, s: TurnSubmission, live: Arc<dyn LiveSink>) -> DriverFuture<'_, DriverOutcome>;
  fn resume_after_approval(&self, session: SessionId, request: ItemId, live: Arc<dyn LiveSink>) -> DriverFuture<'_, DriverOutcome>;
  fn interrupt(&self, session: &SessionId, turn: Option<&TurnId>) -> bool; }
```

**`src/host.rs`:**
```rust
pub struct HostConfig { pub transcripts: Arc<TranscriptStore>, pub approvals: Arc<ApprovalStore>,
  pub hosted: Arc<HostedSessionStore>, pub lease_root: PathBuf, pub limits: HostLimits, pub host_epoch: u64 }
pub struct LocalHost { /* per-session writer task + Arbiter + SessionWriterLease; Arc<Semaphore>(max_running_turns); Fanout */ }
impl LocalHost { pub fn open(c: HostConfig, d: Arc<dyn TurnDriver>) -> Result<Arc<Self>, HostError>; // must run inside a tokio runtime
  pub fn drain(&self, retry_after_ms: u64); pub fn running_turns(&self) -> u32; }
impl SessionPort for LocalHost { … }
```
Behaviour:
- Every method checks `tenant_admits` plus caps, and a failure is `NotFound`.
- `create` writes the `HostedSessionRecord` with `scope.tenant()`.
- `open` recovers state: pending approvals put the session into `WaitingForApproval`, and approvals that were resolved while the host was down are resumed.
- `approval_resolved` reads `ApprovalStore::resolution` and resumes exactly once.
- State reasons: waiting for a host permit gives `Queued{HostCapacity}`, and waiting behind the session's own turn gives `Queued{SessionQueue}`.

Tests:
- `test_second_host_same_root_is_writer_busy`
- `test_cross_tenant_calls_are_not_found`
- `test_detach_does_not_cancel_running_turn`
- `test_approval_resolved_resumes_once`
- `test_queue_reason_host_capacity_vs_session_queue`
- `test_interrupt_is_idempotent`

**`src/test_support.rs`** (`#[cfg(test)]`): `TestError` and `TestResult`, plus a `ScriptedDriver` that uses the real `TranscriptStore` and `ApprovalStore`, a `Notify` gate and run/resume counters.

**`src/lib.rs`:** the module declarations and re-exports.

## Wave 3: transport and operations

**`harw-web/Cargo.toml`:** add `harw-protocol = { path = "../harw-protocol" }` and `tokio-tungstenite = { version = "0.30.0", default-features = false, features = ["handshake"] }`, each with a German rationale comment in the style of :11-24.

**`harw-web/src/server.rs`:**
- **Builder.** `fn http_connection_builder(t: Duration) -> hyper::server::conn::http1::Builder` sets `.timer(TokioTimer::new()).header_read_timeout(t)`. Serving uses `.serve_connection(TokioIo::new(stream), svc).with_upgrades()`. Update the test helper at :1155-1185.
- **Connection cap.** `const MAX_CONNECTIONS: usize = 256;` with `try_acquire_owned` in the accept branch (:488-514). A connection over the cap is dropped.
- **Execution.** Add:
  ```rust
  pub(crate) struct ExecuteDeps<'a> { pub(crate) peer: &'a PeerCredentials, pub(crate) resolved: &'a ResolvedPeer, pub(crate) context_factory: &'a WebContextFactory, pub(crate) events: &'a WebEventBus }
  pub(crate) async fn execute_route(route: &WebAdapter, caller_tier: PermissionTier, deps: ExecuteDeps<'_>, args: serde_json::Value, attach_slot: Option<Arc<AttachSlot>>) -> Result<OpOutput, OpError>
  ```
  It runs factory, then `scope_op_context`, then the optional `with_service(slot)`, then `invoke`, then publishes `OperationCompleted`. The `Execute` arm of `handle` (:713-739) calls it after the `scoped_context` `None` check.
- **Output shape.** Add `pub(crate) fn op_output_json(o: &OpOutput) -> serde_json::Value`, the same shape as :1001-1021.
- **Visibility.** Make `resolve_identity` (:755) and `forbidden_reason_str` (:1032) `pub(crate)`.
- **WebSocket branch.** In `handle`, before `EVENTS_PATH` (:654), add `if path == crate::ws::WS_PATH`. `serve_until` builds `WsShared{permits, shutdown.clone(), limits}`.

Tests:
- `test_upgrade_on_ws_path_switches_protocols_over_real_socket`
- `test_connection_cap_drops_excess`
- `test_execute_route_scopes_tenant_and_publishes`

The existing test `test_incomplete_headers_close_connection_after_header_read_timeout` (:1507) must stay green.

**`harw-web/src/router.rs`:**
```rust
#[must_use] pub fn decide_resolved_operation<'a>(routes: &'a WebRouteTable, identity: Option<&ResolvedPeer>, operation: &str) -> RouteDecision<'a>
```
Extract a private `decide_entry` (the checks now at :279-297) and share it with `decide_with_tier`. This function never returns `MethodNotAllowed`.

Tests:
- `test_operation_unknown_is_not_found`
- `test_operation_without_identity_is_unknown_peer`
- `test_operation_insufficient_tier`
- `test_operation_approval_always_is_approval_required`
- `test_operation_identity_asked_after_lookup`

**`harw-web/src/meta.rs`:**
- `is_reserved_path` (:121) also returns true for `crate::ws::WS_PATH`.
- `capabilities_document` adds `"session_ws": {"path","subprotocol","wire_minor"}`.

Tests: `test_ws_path_is_reserved`, `test_registry_op_on_ws_path_is_duplicate_route`, `test_capabilities_advertise_session_ws`.

**`harw-web/src/ws.rs` (new):**
```rust
pub(crate) const WS_PATH: &str = "/v1/ws";
#[derive(Debug, Clone, Copy)] pub(crate) struct WsLimits { pub(crate) max_connections: usize, pub(crate) max_in_flight: usize,
  pub(crate) max_attachments: usize, pub(crate) max_inbound_bytes: usize, pub(crate) max_outbound_bytes: usize,
  pub(crate) hello_timeout: Duration, pub(crate) ping_interval: Duration, pub(crate) pong_timeout: Duration,
  pub(crate) idle_timeout: Duration, pub(crate) max_lifetime: Duration, pub(crate) identity_recheck: Duration, pub(crate) write_timeout: Duration }
impl Default for WsLimits   // DEC-048 values
pub(crate) fn ws_config(l: &WsLimits) -> tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
#[derive(Clone)] pub(crate) struct WsShared { pub(crate) permits: Arc<Semaphore>, pub(crate) shutdown: watch::Receiver<bool>, pub(crate) limits: WsLimits }
pub(crate) struct WsDeps { pub(crate) routes: Arc<WebRouteTable>, pub(crate) identity: Arc<dyn LocalPeerIdentityResolver>,
  pub(crate) context_factory: Arc<WebContextFactory>, pub(crate) events: Arc<WebEventBus>, pub(crate) shared: WsShared }
pub(crate) async fn handle_ws(request: Request<Incoming>, peer: PeerCredentials, deps: WsDeps) -> WebResponse;
```
The flow, in order:
1. Validate the upgrade. It must be GET, with `Upgrade: websocket`, `Sec-WebSocket-Version: 13`, a key present, and the subprotocol. Failures are 400 or 426. An `Origin` header gives 403.
2. Call `resolve_identity`. On error, answer 403 with `error.code()`.
3. Take a WebSocket permit. If none is free, answer 503 `busy`.
4. Answer 101 with `derive_accept_key`, then run `hyper::upgrade::on` and `from_raw_socket(TokioIo, Role::Server, Some(ws_config))`.
5. In the loop, each request is handled in this order:
   1. Parse `RequestEnvelope`.
   2. Require that `hello` came first.
   3. Re-resolve identity with the stored header value. A failure sends `Revoked` to every attachment and closes with 1008.
   4. Run `decide_resolved_operation`.
   5. Take an in-flight permit.
   6. Call `execute_route`. For `session.attach` this passes an `AttachSlot`, and the response result is `{attachment_id, ack}`.

   Pump tasks turn each `FrameSource::next` into `event.frame` messages. `session.detach` aborts its pump locally. A ticker handles ping, pong, re-check, idle and lifetime. Shutdown sends `HostDraining` and closes with 1001.

Tests:
- `test_ws_config_write_limits_valid`
- `test_upgrade_without_subprotocol_400`
- `test_upgrade_with_origin_403`
- `test_upgrade_unresolvable_identity_403_reason`
- `test_ws_connection_cap_503`
- `test_request_before_hello_invalid_request`
- `test_unsupported_major_rejected`
- `test_oversized_inbound_closes_1009`
- `test_binary_closes_1003`
- `test_in_flight_cap_busy`
- `test_hello_timeout_closes`
- `test_pong_timeout_closes`
- `test_lifetime_closes_1001`
- `test_identity_recheck_revokes`
- `test_unknown_method_not_found`
- `test_response_shape_matches_http_body`
- `test_attachment_cap_per_connection`

Timeouts in tests are shortened by injecting `WsLimits`; harw-web's tokio has no test-util feature.

**`harw-web/src/lib.rs`:** add `mod ws;` (private).

**`harw-ops/Cargo.toml`:** add `harw-protocol`.

**`harw-ops/src/session.rs` (new):**
```rust
pub fn caller_scope(ctx: &OpContext) -> Result<CallerScope, OpError>
```
It uses `Principal::id` and `Principal::tier` (`harw-types/src/principal.rs:150,162`), `ctx.tenant()`, and `ClientCaps::for_tier`. It fails closed when the Principal or the port is missing.

The operations, with `#[operation]` arguments following `approval.rs:404-410` (check the macro's bounds on the argument types):

| Operation | Tier | Route | Approval |
|---|---|---|---|
| `session.hello` | observer | POST `/api/session-hello` | none |
| `session.list` | observer | GET `/api/session-list` | none |
| `session.create` | operator | POST `/api/session-create` | none |
| `session.attach` | observer | GET `/api/session-attach` | none |
| `turn.submit` | operator | POST `/api/turn-submit` | none |
| `turn.interrupt` | operator | POST `/api/turn-interrupt` | none |

When `session.attach` finds an `Arc<AttachSlot>` in the services it calls `put`. Otherwise the source is dropped, which means a snapshot only.

Tests:
- `test_caller_scope_without_principal_fails_closed`
- `test_ops_not_available_without_port`
- `test_scope_tenant_comes_from_ctx`
- `test_attach_puts_source_into_slot`

**`harw-ops/src/lib.rs`:** add `pub mod session;` and register the operations in `register_all` (:343).

**`harw-ops/src/approval.rs`:**
- In `approval_resolve` (:411), retry `LockContended` up to 3 times with 10 ms between attempts, then map it to `Execution("busy")`.
- After success, call `port.approval_resolved(..)` if an `Arc<dyn SessionPort>` is in the services. A failure here is only logged.

Tests: `test_resolve_wakes_session_port_once`, `test_failed_resolve_does_not_wake`, `test_lock_contended_retried`.

## Wave 4: composition and end-to-end tests

**`harw-cli/src/runtime_session_host.rs` (new):**
- Provides `pub(crate) struct CoreTurnDriver`, which implements `TurnDriver` using `AgentSession` hydration, following `telegram_session.rs:733-760`.
- Turns run through `run_turn_durable` and `resume_after_approval_durable` (`harw-core/src/turn_loop.rs:3344,3692`).
- The resume actor comes from `ApprovalStore::resolution`.
- It bridges `AgentEventHub` and the `OrchestrationObserver` (`child_controller.rs:1376`) into `LiveSink`.
- Also provides `pub(crate) fn build_session_host(..) -> Result<Arc<LocalHost>, String>`.

Tests: `test_build_session_host_over_temp_home`, `test_interrupt_without_turn_is_false`.

**`harw-cli/src/web.rs`:**
- Build the host after the context factory (:356).
- `web_op_context` (:675) gets a new parameter `session_port: Option<&Arc<dyn SessionPort>>` and inserts it into the services.
- Clamp `max_running_turns` to the provider's concurrency.

Test: `test_web_op_context_inserts_session_port`.

**Other harw-cli files:** `harw-cli/src/lib.rs` gets the module line. `harw-cli/Cargo.toml` gets `harw-session-host`.

**`harw-session-host/tests/common/mod.rs`:** a harness built from real pieces:
- `harw_ops::register_all`, `WebRouteTable::from_registry` and `BoundWebServer::bind` with a resolver;
- `LocalHost` with `ScriptedDriver`;
- a WebSocket client via `tokio_tungstenite::client_async` over a `UnixStream`;
- tenant A and tenant B as two sockets with different `UidTenantMap` resolvers over one host;
- a `RevokingResolver` built the same way as in `harw-web/tests/identity_routes.rs:67-100`.

**`harw-session-host/tests/ws_acceptance.rs`:** the tests in the table below.

## Acceptance tests
The `ws_*` tests are in `harw-session-host/tests/ws_acceptance.rs`.

| Requirement | Tests |
|---|---|
| Forged identity | `ws_frame_claimed_actor_or_tenant_is_rejected_and_ignored`, `ws_upgrade_with_foreign_security_context_is_refused`, `ws_submit_records_peer_principal_not_frame_value` |
| Per-operation authorization | `ws_observer_cannot_submit_but_can_attach`, `ws_approval_always_operation_is_refused`; router.rs `test_operation_*` |
| Cross-tenant replay | `ws_cross_tenant_attach_with_cursor_is_not_found`, `ws_cross_tenant_approval_resolve_is_not_found` |
| Duplicate submit | `ws_duplicate_client_msg_id_runs_one_turn_across_reconnect`; arbiter.rs `test_duplicate_*` |
| Head check and single writer | `ws_stale_expect_head_returns_stale`, `ws_two_clients_submit_turns_run_serially`; host.rs `test_second_host_same_root_is_writer_busy` |
| Concurrent approval | `ws_concurrent_approval_exactly_one_resolves_and_turn_resumes_once`; approval.rs `test_lock_contended_retried` |
| Slow consumer | `ws_slow_consumer_gets_lagged_and_turn_is_not_blocked`; fanout.rs `test_publish_never_blocks_on_full_queue` |
| Old cursor | `ws_old_generation_cursor_gets_resync`, `ws_cursor_ahead_gets_resync`; replay.rs tests |
| Reconnect (durable, then live) | `ws_reconnect_from_cursor_replays_durable_then_live` |
| Revoked identity and reconnect | `ws_revoked_identity_gets_revoked_close_1008_and_reconnect_refused` |
| Restart | `ws_restart_reattach_from_cursor_after_host_reopen`, `ws_approval_resolved_while_down_resumes_after_restart`. The host is reopened in-process; there is no OS-level process restart test. |
| Detach does not cancel | `ws_disconnect_mid_turn_does_not_cancel_turn` |
| Bounds | ws.rs: `…closes_1009`, `…cap_503`, `…in_flight_cap_busy`, `…hello/pong/lifetime…`, `…attachment_cap…`. server.rs: `test_connection_cap_drops_excess`. fanout.rs: `test_queue_bounded_by_bytes`. |
| Workbench wait reason | `ws_queued_session_reports_host_capacity_reason`; host.rs `test_queue_reason_host_capacity_vs_session_queue` |

## Out of scope: needs an operator decision
1. Remote session attach over NodeTransport. The choices are WebSocket (which needs `.with_upgrades()` at `harw-node-transport/src/server.rs:292-298`) or PL-65 NDJSON. Either way it depends on DeviceRegistry and pairing (PL-65 §4.2).
2. The retirement date for `GET /events` SSE.
3. Where the host lives: the `harw web` process (as in W11) or `harw gateway` (PL-65 §6a.4).
4. The local TUI. It still owns `AgentSession`, and exiting it cancels background agents (`harw-tui/src/runtime_root.rs:889-895`). Should it take the writer lease, or become a client (DEC-015)?
5. A pending-spawn identity for queued children, so the parent child cap and provider waits can be emitted from `child_controller.rs:8718`. This also covers adding `wait` to `AgentOrchestrationEvent` (4 struct-literal sites) and rendering it in the Agents panel (`harw-tui/src/agent_monitor.rs:368`).
6. The approval policy of `turn.submit`. Proposed: `none` at Operator tier. The alternative is `always`.
7. An `Origin` allowlist for a webui proxy. The default is to reject. A grep of `webui/` found no `socketPath` or `unix`.
8. Durable `client_msg_id` dedupe across restart. W11 relies on the LRU plus the head check; `force` can still duplicate.
9. Using the hub principal as the approver in `security_hub` mode (`harw-ops/src/approval.rs:417-427`).
10. A seek index for the transcript. `read_from` is a linear scan.
11. Whether DEC-011 to DEC-013 are written separately or folded into DEC-048.
12. The `Compact` profile, the live ring and `Snapshot`.

## Tests added and central build
No tests were added and no files were changed: this was a read-only mapping.

After all W11 coders have finished, the central build runs once. In the cloud container, run every step with `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0`.
1. `cargo fmt --all`
2. `cargo clippy --workspace --all-targets -- -D warnings`. This also updates `Cargo.lock` with the new `harw-session-host` package and the new dependency lines on `harw-web` and `harw-ops`.
3. `cargo test --workspace`, including doc tests. The crates that matter are harw-protocol, harw-session-store, harw-operations, harw-session-host, harw-web, harw-ops and harw-cli.
4. `cargo run -q -p xtask -- gates`
5. `cargo deny check`
6. `make -C dod clippy test`. This runs with `--locked`, so the `Cargo.lock` from step 2 must be committed first.
7. `actionlint`