---
id: PL-65
title: Remote Sessions — Sessions über die Cloud auf verschiedenen TUIs (Programm RS)
status: planned
date: 2026-09-27
tags: [planning, remote-sessions, cloud-home, tui, storage, network]
related:
  - local-tui-live-workbench.md
  - ../70-decisions/README.md
  - ../80-copilot-backlog/README.md
  - ../90-migration-ledger/MIGRATION_LEDGER.md
---

# Plan: Sessions via the cloud on different TUIs ("Remote Sessions", program RS)

Planning only. No files were changed and no build was run. Every statement below is based on code I read at the current HEAD; where I was not sure, the item is marked **(verify)**.

> **Transport-Profil W00:** Der ergänzende Entwurf
> [Local / Own-Cloud Session Control Plane — WebSocket transport profile](local-own-cloud-websocket.md)
> optimiert dieses Programm für die neuere WebSocket-Control-Plane-Richtung.
> Er ändert keine bereits gelandete Transportfläche: `harw-web /events` bleibt
> SSE und der DoD-Uplink bleibt NDJSON. Für die **noch nicht implementierte**
> Session-Control-Plane ersetzt der Entwurf lediglich den hier geplanten
> Zwischenweg `POST /v1/rpc` + NDJSON durch einen direkt versionierten,
> multiplexen WebSocket-Transport. Der feste W00-Umsetzungsvertrag liegt unter
> [`contracts/W00-websocket-control-plane.md`](contracts/W00-websocket-control-plane.md).

Build rule (binding, verbatim, for every agent prompt): Subagents and parallel agents must **never** run `cargo` or `rustc` in any form: no `check`, `build`, `test`, `nextest`, `clippy`, `fmt`, `run`, `doc`, `deny`, and no `make` target that calls them. They only read and edit code. At the end they report which tests they added and which commands the central build must run.

---

## 0. Grounding: what exists today (with citations)

| Building block | What the code actually does today | Consequence for this plan |
|---|---|---|
| `harw-tui` | Embeds the runtime in-process. `runtime_root.rs::run_tui` takes an `Arc<RuntimeAssembly>`. `app.rs` (14,943 lines) drives `run_turn_streaming` itself through `ChatGateway::borrow_turn_ctx()` (`gateway.rs`). `gateway.rs` already names the next step: "Ein `RemoteGateway` kann `LocalGateway` ersetzen …". | Remote TUI needs a client-side port. Refactoring `app.rs` is the riskiest part; DEC-015 therefore starts with a separate thin loop. |
| Turn driver outside the TUI | `harw-cli/src/gateway/telegram_session.rs`: per-chat FIFO; `hydrate_from_store` before every turn; parks on `TurnOutcome::AwaitingApproval`, then `resume_after_approval` with `ApprovalActor::ChannelPeer`; delta coalescing every 1.5 s (`STREAM_UPDATE_INTERVAL`). | This is the **template** for the session host (same park/resume/FIFO pattern), generalized to N clients. |
| `harw gateway` daemon | `harw-cli/src/gateway.rs`: a persistent daemon that keeps the long-lived runtime alive and already runs model turns for Telegram. "`harw` (ohne Subcommand) bleibt der TUI-Client; der Daemon läuft getrennt." | The natural home for the session host (DEC-011). |
| `harw-web` | UDS only, SO_PEERCRED (`peer.rs`), `LocalPeerIdentityResolver` → `ResolvedPeer{tier, tenant, …}` (`identity.rs`). Routes come only from `OperationRegistry` (`router.rs`). `GET /events` is SSE with `sequence: u64` but **no replay** and one stream for the whole process (`events.rs`). The web entry runs **no model turns** (`runtime_web.rs`: `EntryKind::Web`, `ModelSource::Echo`). | Reuse `peer.rs` and the identity modules as a library. Do not build sessions into the web op table. |
| `harw-node-transport` (A) | TLS 1.3 restricted to X25519MLKEM768 + mutual ML-DSA-65 transcripts bound to the TLS exporter; replay cache. `NodeTransportServer::new(local, verifier: Arc<dyn NodeVerifier>, options)`; `NodeVerifier::is_known` refuses unknown clients before the server signs anything; `AuthenticatedPeer{node_id, protocol_version}` is attached as a request extension; `AuthHubNodeSigner` is hard-wired to `HarwKeyPurpose::NodeIdentity`. NDJSON uplink with `UplinkLimits`. | A dynamic device verifier is possible **without** changing the transport. Enrollment (client not yet known) **does** need a transport change (RS2-05). |
| KMS purposes | `dod/crates/harw-dod-encrypt/src/purpose.rs`: `HarwKeyPurpose::DeviceIdentity` → `SignPurpose::DeviceHandshake` (`"harw:device-handshake:v1"`) already exists. | Device keys fit the existing purpose table. Nothing new to invent. |
| `harw-channel` | `PairingCode`: Crockford 8 characters, 40 bits, single use, TTL 15 min, scoped to one channel (`pairing.rs`); `SessionKey{tenant, channel, peer, thread}`. | Reuse `PairingCode` for device enrollment. |
| `harw-session-store` | `TranscriptRecord{session_id, thread, sequence (monotonic, 0-based), recorded_at, kind, payload}`, JSONL, fs4 lock. `TranscriptStateStore::save_history` **appends** a `history_replaced` marker instead of rewriting; `TranscriptStore::rewrite` exists for retention. `ApprovalRecord{…, tenant: Option<TenantId>}`. | The durable cursor is the transcript `sequence`. A new `generation` is needed only after a `rewrite`. |
| `harw-protocol` (F) | `RequestEnvelope/ResponseEnvelope/NotificationEnvelope`; `ProtocolVersion{major, minor}` rejects any major ≠ 1; `methods.rs` already has `turn.submit`, `turn.interrupt`, `approval.respond`, `session.close`, `event.session`, `event.turn`, `approval.request`. None of these is used anywhere outside the crate. `TurnEvent`/`SessionEvent` are internally tagged and have no unknown-variant fallback. | Reuse the envelopes. Wrap events in a frame type that has an `Unknown` fallback. |
| `harw-core` | `SessionState{Idle, Running, WaitingForApproval, WaitingForChild, Failed}`; `run_turn_durable(session, model, store, approvals, input)`; `AgentEventHub` = broadcast with capacity 8192 whose slow receivers get `Lagged` (`agent_events.rs`); `repair_open_tool_calls` on load. | The host is the single writer; a lagging client must resync instead of slowing the turn loop. |
| Session controller | `docs/architecture/session-controller.md`: queued mutations are applied at the turn boundary (`apply_pending_controller_state`). | Model/mode/effort changes from remote clients use the same boundary. |
| Jobs | Leases with fencing epoch, coordinator restart recovery (`harw-job-runtime/src/coordinator/tests.rs::restart_reattaches_the_running_job_and_finishes_it`); `JobScope{tenant, …}` (`harw-job-core/src/stored.rs`). The job store lives at `<profile>/jobs/{records,locks,approvals}`. | Jobs are already independent of sessions. Detaching never touches them. |
| Tenancy | `harw_operations::context::tenant_admits(None, _) == true` (an unscoped caller sees everything). | Remote callers **must** carry `Some(tenant)`, fail closed. |
| Identity vocabulary | `TrustZone{Local, Host, Cluster, Remote}`, `AuthStrength{…, MutualTls, HardwareBacked}`, `SecurityContext{device: Option<DeviceId>, …}`, issued only by `harw-security-hub` (`harw-types/src/security.rs`). | A device context is expressible today. |
| Decisions | DEC-001 through DEC-008 exist. DEC-009 and DEC-010 are only **proposed** as Copilot tasks C-08 and C-10 and have not been written. | DEC-010 ("cloud home") is a prerequisite. See §6. New notes start at DEC-011. |
| Work driver | The worker is at `harw-cli/src/job_worker_work_driver.rs`. All workers of a run go through the **service provider** (`ModelSource::Override`, `load_run_config`). The judge uses `InternalModelPoint::WorkDriverJudge`. Without C-09 the verify runner falls back to `NoSandboxRunner`, and a run escalates at its first Verify. | This limits §11: one model per work-driver run, and the work driver only works after C-09. |

---

## 1. Goal and non-goals

**Goal**
- An agent session (root session including children, approvals and the jobs it enqueued) lives in a Harwness host process (local daemon or cloud container).
- The session keeps running without clients.
- Any number of authenticated TUI clients can `attach`, `detach` and `re-attach`, and can watch, steer and approve concurrently, each within its own capabilities.
- Reconnecting loses nothing: replay from the transcript plus a live tail.
- Many clients never multiply model calls.

**Non-goals (v1)**
- No browser client. `webui` stays on `harw-web`.
- No shared editing of a single input line (no Google Docs cursor).
- No multi-host session migration while a turn is running (a turn is not checkpointed mid model call).
- No WorkDriver TUI slash commands (DEC-008 stays). Work-driver runs are only *observed* via job frames and `work_driver.status`.
- No provider credentials, API keys or KEKs on any client.
- No refactor of the local `app.rs` in v1 (DEC-015). Convergence is RS9.

---

## 2. Session model

### 2.1 Identity and ownership
- `SessionId` stays the key (`harw-types`). New durable record `HostedSessionRecord`, stored in the machine-local state dir (see 6a) at `<state>/hosted-sessions/<session_id>.json` and written atomically with `harw_fsutil::write_atomic`:
  ```rust
  pub struct HostedSessionRecord {
      pub session_id: SessionId,
      pub tenant: Option<TenantId>,        // Some(..) mandatory when created by a remote client
      pub owner_principal: String,         // Principal::id() of the creator
      pub workspace: Option<WorkspaceId>,
      pub entry: HostedEntry,              // Tui-equivalent profile, see 2.2
      pub created_at: Timestamp,
      pub state: HostedState,              // last durable state
      pub host_epoch: u64,                 // increments on every host start
      pub title: Option<String>,           // from harw_session_store::meta
  }
  ```
- Access rule: `tenant_admits(caller.tenant, record.tenant) && caps` (see 2.4). Only the owner or `Control` may run `session.close`.

### 2.2 Lifecycle

```text
          create                 submit                   turn ends
 (none) ────────► Idle ────────────────► Running ─────────────────────► Idle
                   ▲  ▲                    │  │ AwaitingApproval (parked)
                   │  │  approval resolved │  └─► WaitingForApproval ──┐
                   │  └────────────────────┘                           │ timeout → rejected
                   │         host restart during Running               ▼
                   └──── resume (manual/auto) ◄── Interrupted ◄── (crash/reclaim)
 close ──► Closed        unload after idle TTL: state unchanged, memory released
```

- `HostedState = Idle | Running | WaitingForApproval | WaitingForChild | Queued{depth} | Interrupted | Failed | Closed`. This mirrors `harw_core::SessionState` and adds `Queued`, `Interrupted` and `Closed`.
- **Running without clients.** Turns, children and jobs do not depend on attachments. A running turn that needs an approval parks, exactly as in `telegram_session.rs`, and survives disconnects. The approval TTL is the existing `DEFAULT_APPROVAL_TTL` (session-store) or `ApprovalRequest::default_timeout_at`. On expiry the result is `ApprovalResponse::timed_out`, a rejection (§4.4 of the interaction contract).
- **Idle and unload.** No clients, no turn and no queue for `idle_unload_after` (default 15 min) → drop the in-memory `AgentSession`. The next attach or submit rebuilds it with `AgentSession::hydrate_from_store`. This is the Telegram pattern.
- **Reclaim and restart recovery.** At host start (new `host_epoch`), every record with `state ∈ {Running, Queued}` becomes `Interrupted`. `repair_open_tool_calls` makes the history loadable again. Resume policy (`[sessions] resume = "manual" | "auto"`, default `manual`): `manual` waits for a Steer client (`session.resume`). `auto` resubmits only when the last durable item is a user message and not a tool call. Jobs are not affected: coordinator lease reclaim already exists.

### 2.3 Attach and detach semantics
- `attach(session, from: Option<Cursor>, profile)` → `AttachAck`, then a frame stream. Several attachments per client and per session are allowed.
- `detach` or a lost connection removes the attachment and nothing else. Queued submissions remain.
- Presence: the host broadcasts a `Presence` frame (label, device id, caps, `since`) on every change, so all clients see who is watching.

### 2.4 Capabilities, single writer, several steerers
- **Single writer = the host.** Only the host touches `AgentSession`, `StateStore` and `ModelProvider`. Clients send intentions.
- Capability set (wire type, see 3):

  | Cap | Allows | Minimum tier (local UDS) |
  |---|---|---|
  | `observe` | attach, history, presence | `Observer` |
  | `steer` | `turn.submit`, `turn.interrupt`, `session.resume` | `Operator` |
  | `approve` | `approval.respond` | `Operator` |
  | `control` | `session.set_model/mode/effort`, `session.close`, `session.create` for others | `Maintainer` |

  Effective caps = `device_caps ∩ caps_for_tier(tier) ∩ security_hub_context` (narrowing only, like `Principal::child_of`).
- **Input arbitration (DEC-013)**
  - A per-session FIFO queue (cap 8, configurable).
  - Each `SubmitParams` carries `expect_head: Cursor`, the durable head the client has *seen*. If a turn has completed or started since that point (`record_head.durable > expect_head.durable`), the host returns `SubmitResult::Stale{head}`. The client shows the conflict UI (5.3) and may resend with a fresh head (`force: true` means "I have seen it").
  - A `client_msg_id` (LRU of 256 per session) makes submissions idempotent across reconnects.
  - This is compare-and-swap without a lock lease, so there is no stuck "controller" after a phone goes dark.
- **Interrupt.** Any `steer` client. Idempotent per `turn_id`. Uses `TurnControl`'s `CancelToken` (already kept on the session through pauses, see `session.rs` `active_turn_control`).
- **Approvals.**
  - First writer wins, through the existing durable `ApprovalStore` state transition. Losers get `RespondResult::AlreadyResolved{by}`.
  - The actor is **never** taken from the wire. The host derives it from the client identity:
    - local UDS client: `ApprovalActor::Operator{id: "uid:<uid>"}`;
    - remote device: `ApprovalActor::ChannelPeer{channel: "remote-tui", peer: <device_id>}`. This mirrors Telegram. `Principal::approval_actor` deliberately never builds `ChannelPeer`; the host builds it the same way the Telegram path does.
  - Operations with `approval = "always"` (for example `work_driver.enqueue`, DEC-007/008) follow the same path. There is no second path.
- **Settings** (model, mode, effort). The host's `SharedSessionController` (`harw_operations::session_control`) queues the change, and it is applied at the turn boundary (`session-controller.md`).
- **Read-only observers.** `observe` only. No submit, approval or presence write, except for their own presence entry.

---

## 3. Wire protocol (DEC-012)

### 3.1 Placement and framing
- The types live in **`harw-protocol/src/session_wire.rs`** (F, additive; the crate doc says "Neue Varianten sind additive Changes").
- Requests and responses: the existing `RequestEnvelope` / `ResponseEnvelope` (JSON-RPC 2.0, `protocol: ProtocolVersion{major: 1, minor: SESSION_WIRE_MINOR}`) over `POST /v1/rpc`.
- Event stream: `GET /v1/sessions/{id}/frames?from=<cursor>&profile=full|compact` as **NDJSON** (`application/x-ndjson`, the same form as `harw-node-transport/src/uplink.rs`). One `NotificationEnvelope{method: "event.frame", params: FrameEnvelope}` per line.
- Why not SSE or WebSocket:
  - The same HTTP/1 route works unchanged over UDS (hyper) and over `NodeTransportServer::serve` (Tower).
  - WebSockets would add `tokio-tungstenite`, which today is only transitive via `thirtyfour` (Tier B, `dependency-review.md`).

### 3.2 Types (fixed interface for RS1)
```rust
// harw-protocol/src/session_wire.rs
pub const SESSION_WIRE_MINOR: u32 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor { pub generation: u32, pub durable: u64, pub live: u32 }
// generation: increments only after TranscriptStore::rewrite (retention); durable: TranscriptRecord::sequence
// of the last delivered durable record (+1 semantics documented: "next to deliver");
// live: index into the host's live ring of the running turn, reset to 0 on each new durable record.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientCaps { pub observe: bool, pub steer: bool, pub approve: bool, pub control: bool }
impl ClientCaps { pub fn narrow(self, other: Self) -> Self; pub const OBSERVER: Self; }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)] #[serde(rename_all = "snake_case")]
pub enum StreamProfile { Full, Compact }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)] #[serde(rename_all = "snake_case")]
pub enum HostedState { Idle, Running, WaitingForApproval, WaitingForChild, Queued { depth: u32 }, Interrupted, Failed, Closed }

pub struct HelloParams  { pub client_label: String, pub wire_minor: u32, pub features: Vec<String> }
pub struct HelloAck     { pub wire_minor: u32, pub features: Vec<String>, pub host_epoch: u64, pub granted: ClientCaps }
pub struct SessionSummary { pub session_id: SessionId, pub title: Option<String>, pub tenant: Option<TenantId>,
                            pub state: HostedState, pub attached: u32, pub updated_at: jiff::Timestamp, pub model: Option<String> }
pub struct CreateParams { pub workspace: Option<String>, pub title: Option<String> }
pub struct AttachParams { pub session_id: SessionId, pub from: Option<Cursor>, pub profile: StreamProfile, pub tail_items: u32 }
pub struct AttachAck    { pub session: SessionSummary, pub granted: ClientCaps, pub head: Cursor, pub replay_from: Cursor, pub host_epoch: u64 }
pub struct HistoryParams { pub session_id: SessionId, pub before: Cursor, pub limit: u32 }        // paging older items
pub struct SubmitParams { pub session_id: SessionId, pub text: String, pub expect_head: Cursor, pub client_msg_id: String, pub force: bool }
pub enum   SubmitResult { Accepted { position: u32 }, Stale { head: Cursor }, QueueFull, Denied { reason: String } }
pub struct InterruptParams { pub session_id: SessionId, pub turn_id: Option<TurnId> }
pub struct ApprovalRespondParams { pub request_id: ApprovalId, pub decision: ReviewDecision, pub reason: Option<String> } // NO actor field
pub enum   RespondResult { Resolved, AlreadyResolved { by: String }, Expired, Denied { reason: String } }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum SessionFrame {
    Turn(TurnEvent),                                   // existing harw_protocol::events
    Session(SessionEvent),
    Child { agent: SessionId, parent: Option<SessionId>, role: String, event: TurnEvent },
    ApprovalRequested(ApprovalRequest),                // existing harw_protocol::approvals
    ApprovalResolved { request_id: ApprovalId, decision: ReviewDecision, by: String },
    Job { work_id: String, kind: String, state: String, summary: Option<String> },
    Presence { attached: Vec<PresenceEntry> },
    Snapshot { turn_id: TurnId, assistant_text: String, reasoning_collapsed: bool }, // partial turn after ring loss
    HistoryReplaced { item_count: u32 },               // from the core "history_replaced" marker
    Resync { reason: String, head: Cursor },
    Lagged { resume_from: Cursor },
    HostDraining { retry_after_ms: u64 },
    Revoked,
    Heartbeat,
    #[serde(other)] Unknown,
}
pub struct FrameEnvelope { pub session_id: SessionId, pub cursor: Cursor, pub frame: SessionFrame }
pub struct PresenceEntry { pub label: String, pub device: Option<DeviceId>, pub caps: ClientCaps, pub since: jiff::Timestamp }
```
All param structs use `#[serde(deny_unknown_fields)]`.

**(verify)** `#[serde(other)]` on a unit variant of an adjacently tagged enum with the pinned serde version. This is acceptance test RS1-01-T3. If it fails, fall back to a manual `Deserialize` that maps unknown `kind` to `Unknown`.

`methods.rs` additions: `session.hello`, `session.list`, `session.create`, `session.attach`, `session.detach`, `session.history`, `session.resume`, `session.set_model`, `session.set_mode`, `session.set_effort`; the notification `event.frame`. Existing constants are reused as they are.

### 3.3 Client port (fixed interface, `harw-protocol/src/session_port.rs`, std futures only, no tokio)
```rust
pub type PortFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, PortError>> + Send + 'a>>;
pub trait FrameSource: Send { fn next(&mut self) -> PortFuture<'_, Option<FrameEnvelope>>; }
pub trait SessionPort: Send + Sync {
    fn hello(&self, p: HelloParams) -> PortFuture<'_, HelloAck>;
    fn list(&self) -> PortFuture<'_, Vec<SessionSummary>>;
    fn create(&self, p: CreateParams) -> PortFuture<'_, SessionSummary>;
    fn attach(&self, p: AttachParams) -> PortFuture<'_, (AttachAck, Box<dyn FrameSource>)>;
    fn history(&self, p: HistoryParams) -> PortFuture<'_, Vec<FrameEnvelope>>;
    fn submit(&self, p: SubmitParams) -> PortFuture<'_, SubmitResult>;
    fn interrupt(&self, p: InterruptParams) -> PortFuture<'_, ()>;
    fn respond(&self, p: ApprovalRespondParams) -> PortFuture<'_, RespondResult>;
    fn detach(&self, session: SessionId) -> PortFuture<'_, ()>;
}
#[derive(Debug)] pub enum PortError { Transport(String), Denied(String), NotFound, Protocol(String), Revoked }
```
Implementors:
- `harw-session-host::LocalPort` (in process, used for tests and an optional embedded mode);
- `harw-session-remote::RemotePort` (UDS or node transport).

The TUI depends only on `harw-protocol`.

### 3.4 Replay, live tail and cursor rules
1. `attach(from)`:
   - `from = None` → the host sends the last `tail_items` durable items (default 200) and `HistoryReplaced` as needed.
   - `from = Some(c)` with `c.generation == current` → durable replay of records with `sequence ≥ c.durable`, read with `TranscriptReader` and mapped by `replay.rs`:
     - `RecordKind::Item` → `Turn(ItemAdded)`;
     - lifecycle marker `history_replaced` → `HistoryReplaced`;
     - `core_session_state` → skipped.
   - Different generation → `Resync`.
2. Then the live ring: if the running turn's ring still holds `c.live`, replay the deltas from there. Otherwise send one `Snapshot` (text accumulated so far) and continue live.
3. Pending approvals are **not** taken from the transcript. At attach they come from `ApprovalStore`'s pending list as `ApprovalRequested` frames.
4. Live-only events (`AssistantDelta`, `ReasoningDelta`, `UsageUpdated`, `ContextUpdated`, `ChildProgress`) are never durable. Clients must treat them as disposable.

### 3.5 Backpressure
- Per client: a bounded outbound queue (1024 frames or 4 MiB).
- Above 50 % fill, consecutive `AssistantDelta`/`ReasoningDelta` of the same turn are merged, and `UsageUpdated` keeps only the latest.
- On overflow the subscription is closed with `Lagged{resume_from}` and the client re-attaches from that cursor, which replays from the durable store.
- The host never blocks the turn loop. It subscribes to `AgentEventHub` (broadcast); its own `RecvError::Lagged` becomes `Resync` for every attachment.
- `Compact` profile (phone): drops `ReasoningDelta`, `ChildProgress` and `Child` deltas, sends one `UsageUpdated` per round, and coalesces `AssistantDelta` into 250 ms windows. `Heartbeat` every 15 s.

### 3.6 Versioning
- Major stays 1 (`ProtocolVersion` already rejects any other major).
- `session.hello` negotiates `wire_minor = min(client, host)` and a feature list (`"compact"`, `"history"`, `"child_frames"`).
- New `SessionFrame` kinds are additive; old clients see `Unknown` and render a neutral line.
- A breaking change needs `major = 2` and a parallel `/v2/` route.

---

## 4. Transport and auth

### 4.1 Two listeners, one service
| Path | Transport | Identity | Default caps |
|---|---|---|---|
| Local | UDS `$HARW_RUNTIME_DIR/sessions.sock` (0600, user mode). In system mode `/run/harw/infra/sessions.sock` 0660, group `harw-control-clients` (MIG-013). | `harw_web::peer::read_peer_credentials` + `harw_web::identity::LocalPeerIdentityResolver` (tier map or security-hub mode) → `ClientIdentity` | `caps_for_tier(tier)` |
| Remote | `harw-node-transport` (`NodeTransportServer::new(local_node, Arc<DeviceVerifier>, ServerOptions)`), TCP on a private address only (§4.6) | `AuthenticatedPeer.node_id` → `DeviceRegistry::by_transport_id` → `DeviceRecord` → `ClientIdentity` | `DeviceRecord.caps` ∩ tenant policy ∩ security-hub context |

Both mount the same hyper/Tower service from `harw-session-host/src/http.rs`.

The host's identity type is independent of `harw_web::ResolvedPeer` (private fields, web only):
```rust
pub struct ClientIdentity { pub principal: Principal, pub tenant: Option<TenantId>, pub caps: ClientCaps,
                            pub device: Option<DeviceId>, pub actor: ApprovalActor, pub label: String,
                            pub zone: TrustZone, pub strength: AuthStrength }
```
- Remote identities **must** have `tenant: Some(_)`. `None` is refused (`tenant_admits(None, _)` would otherwise see everything).
- Principal:
  - remote: `Principal::trusted_ingress(PrincipalKind::Human, format!("device:{id}"), IngressSurface::RemoteClient, tier)`;
  - local: `IngressSurface::Tui`.
- The new variant `IngressSurface::RemoteClient` is additive and serde-visible. **(verify)** the exhaustive matches first (V-02; 29 files mention `IngressSurface::`).

### 4.2 Device identity and pairing (DEC-014)
- **Device key**: ML-DSA-65.
  - On machines with a local AuthHub: key `harw.device-identity/<device_id>` (purpose `DeviceIdentity`, sign purpose `DeviceHandshake`), signed through a generalized `AuthHubNodeSigner`. RS2-04 adds a purpose parameter, and `TranscriptWrappedVerifier::for_purpose(SignPurpose::DeviceHandshake)`.
  - Without AuthHub (phone or thin laptop): a software seed in `harw-secrets` V3 through `LocalHpkeDekWrapper`, file 0600 in `HARW_STATE_DIR`. `AuthStrength::MutualTls` either way; `HardwareBacked` only with TPM or Secure Enclave (later).
- **Transport id**: `NodeId("dev-<device_id>")`. It satisfies the AuthHub key-name grammar (`[A-Za-z0-9._-]{1,64}`, see `AuthHubNodeSigner::new`).
- **Pairing flow**
  1. On the host, an operator with `control` (local TUI or `harw device pair --tenant T --caps observe,steer,approve --label phone`) gets a pairing string: `harw1:<endpoint>:<host_node_id>:<host_key_fp_b32(blake3[..16])>:<CODE>`. `CODE` is `harw_channel::PairingCode` with channel `remote-tui`, TTL 15 min, single use. It is shown as text and as a QR code in the TUI.
  2. The device runs `harw attach --pair '<string>'`. It generates its key and connects to the **enrollment listener**. RS2-05 adds `harw-node-transport/src/enroll.rs`, a sans-IO exchange in the style of `handshake.rs`: TLS 1.3 X25519MLKEM768; only the **server** signs the transcript; the client checks the server key against the fingerprint from the pairing string.
  3. The client sends `{code, device_public_key, label, proof = sign(DeviceHandshake, transcript ‖ code)}`.
  4. The host checks the code (at most 5 attempts per code, then burned), enters the key with the capabilities fixed at issue time into `DeviceRegistry`, and writes an audit event.
  5. From then on the device uses the normal mutual handshake.
- The enrollment listener only knows `POST /v1/devices/enroll`, has its own rate limit and can be switched off (`[sessions.remote] enrollment = false`).
- Why this design: the existing `is_known` rule ("refuse unknown clients before the server spends a signature on them") stays intact for the normal listener.

### 4.3 Per-client capabilities and tenant scoping
- `DeviceRecord { device_id, transport_id, public_key: Vec<u8> /*1952 B*/, tenant: TenantId, principal_id, caps: ClientCaps, label, enrolled_at, last_seen, revoked_at: Option<Timestamp> }` in `harw-devices`.
- Every host method checks `tenant_admits(Some(&identity.tenant), record.tenant.as_ref())` plus the cap. List, attach and history only ever return sessions of the caller's tenant.
- A security-hub context is optional in v1 and required once enabled:
  - the host asks `harw-security-hub` for a context with `TrustZone::Remote`, `AuthStrength::MutualTls`, `device = Some(id)` and a short TTL (15 min);
  - `PolicyEngine` narrows it, and `DELETE /v1/contexts/{id}` revokes it;
  - this needs a new device branch in `harw-security-hub/src/policy.rs` (RS4-06), which today maps only SO_PEERCRED uids.

### 4.4 Revocation
- `harw device revoke <id>`:
  1. sets `revoked_at` (atomic write);
  2. `DeviceVerifier::is_known` answers false from then on (read live from the registry, no restart);
  3. the host sends `Revoked` to every stream of that device and closes it;
  4. its queued submissions are discarded;
  5. its security-hub contexts are revoked;
  6. an audit event is written.
- Defense in depth: a maximum connection lifetime (60 s default, like `harw-auth-hub/src/server.rs` `CONNECTION_MAX_LIFETIME`). Long streams reconnect with their cursor, so every reconnect re-checks the pin.

### 4.5 Where the KMS signs
| Signature | Key | Where |
|---|---|---|
| Host transcript (normal and enrollment listener) | `harw.node-identity/<host>` (NodeIdentity/NodeHandshake) | host AuthHub, `AuthHubNodeSigner` (exists) |
| Device transcript | `harw.device-identity/<device>` (DeviceIdentity/DeviceHandshake) | device AuthHub via the generalized signer, or the device software signer |
| Audit chain of device and session admin events | existing `harw-secrets` audit chain; checkpoints signed with the `AuditCheckpoint` key | host |

No bearer tokens are used anywhere. Pairing codes are single use and never persisted in clear: store `blake3(code)` in the registry-side pairing record. **(verify)** whether `PairingRegistry` journals the clear code today (`pairing.rs` "durable issue/approve … currently deferred upstream"). If it does, RS4-10 stores only the hash.

### 4.6 Threat model (short)
| Attacker | Measure |
|---|---|
| Passive network observer, harvest-now-decrypt-later | X25519MLKEM768 only (node transport) |
| Active MITM | Mutual ML-DSA-65 pins bound to the exporter; enrollment pins the host fingerprint from an out-of-band string |
| Stolen device | Revocation (4.4); short context TTL; optional step-up for `approve` on high-risk (`approval = "always"`) operations, see owner question Q4 |
| Pairing code brute force | 40 bits, 15 min, 5 attempts, single use, rate-limited enrollment listener |
| Malicious co-tenant or second person on a shared host | Tenant scoping with `Some(tenant)` mandatory; caps; presence makes watchers visible |
| Prompt-injected model tries to self-approve | Actor derived from the transport, never from params or model output (unchanged `ApprovalActor` rule) |
| Replay | Existing `ReplayCache` (nonce + window + exporter); `client_msg_id` idempotency |
| DoS | `max_connections`, handshake timeout (existing `ServerOptions`), per-device submit rate, queue cap, per-client outbound cap |
| Compromised cloud operator (hypervisor, memory) | Out of scope. The host is inside the TCB of the session. State this in DEC-018. |

**No secrets on the wire.** Provider keys, KEKs and sealed secrets never leave the host. Transcripts and tool outputs *can* contain sensitive file content; that is data for authorized clients, and the classification is `private` (6a).

### 4.7 Network placement (DEC-018)
- **Prefer a private network over a user VPN.**
  - The cloud host sits in a private subnet (VPC / private network) with **no public ingress**. The node-transport port listens only on the private address.
  - Egress: only provider API hosts and package registries, through a NAT/egress proxy with an allowlist. `harw-egress` is the in-process layer.
  - Clients reach the host through (a) a bastion or session-manager port forward, (b) a private endpoint from a corporate network, or (c) a WireGuard mesh used only as an **underlay**.
- Why not a plain user VPN:
  - a VPN puts the whole device into a flat network (lateral movement);
  - VPN identity is user-level and long-lived, not device- and capability-level;
  - a VPN IP address must never be identity (H11 exit criterion, `harw-node-transport/src/lib.rs`: "The remote node's IP address is never identity").
- Node-transport mTLS stays inside the private network (defense in depth).
- `harw-netsec` records topology only: zone `cloud-private`, a route to the hub node, and drain state. It stays "not an identity authority" (`harw-netsec/src/lib.rs`).
- Phones: first SSH or mosh to the bastion, running `harw attach` there over UDS or node transport. A native phone client is later work (Q2: aws-lc cross-compilation).

---

## 5. TUI changes

### 5.1 Architecture (DEC-015)
- **v1:** the local `harw` keeps embedding the runtime (no change to `app.rs`). New: `harw attach [--host <name>|--socket <path>] [--session <id>]` starts a **thin client loop** `harw-tui/src/attach_root.rs::run_attached_tui(port: Arc<dyn SessionPort>, opts: AttachOptions)`. It reuses the renderer building blocks (`history_cell.rs`, `markdown.rs`, `approval_dialog.rs`, `status_line.rs`, `input_editor.rs`, `choice_dialog.rs`), but no `RuntimeAssembly`.
- **v1 bonus:** the local TUI can attach to its own daemon (`harw attach --socket …`), so a laptop session survives closing the terminal.
- **RS9, later:** move the local TUI onto `SessionPort` (`LocalPort` in process), so there is only one loop. `ChatGateway` goes away.
- Slash commands in the thin client:
  - v1 supports `/resume` (session picker over `session.list`), `/model`, `/mode`, `/effort` (control), `/interrupt`, `/approve`, `/detach`, `/who` (presence).
  - Everything else says "not available when attached (v1)".
  - `op.invoke` for `Surface::Command` operations over the wire is a later feature (RS9); it has to go through `CommandAdapter` on the host with the peer principal.

### 5.2 Reconnection UX and latency hiding
- Status segment `connection_status.rs` shows `● live`, `◌ reconnecting 3s`, `⚠ stale (N new)` or `⛔ revoked`.
- Reconnect uses exponential backoff (0.5 s → 30 s, with jitter) and resumes from the last `Cursor`.
- Optimistic echo: the local user message shows as pending (grey) until the host echoes `ItemAdded(UserMessage)`, matched by `client_msg_id`.
- Input stays editable offline. A submit while offline goes into a local outbox holding one message and is sent after reconnect with its old `expect_head`.
- On attach the host sends the tail (default 200 items). Older items load on scroll-up with `session.history`.

### 5.3 Conflict UI (`conflict_dialog.rs`)
- `SubmitResult::Stale{head}` → dialog: "Session moved on by N items (last from ‹label›). [Show] [Send anyway] [Edit] [Discard]". "Send anyway" resubmits with `force: true`.
- Approval race: `ApprovalResolved{by}` closes an open approval dialog with a toast "approved by ‹label›".
- `HostDraining` → banner "host restarting, reconnecting in X s". `Interrupted` → prompt "resume last turn? [Resume] [Leave]" (requires `steer`).

### 5.4 Small terminal / phone layout (`compact_layout.rs`)
- Active when `cols < 60 || rows < 20` or with `--compact`, and it automatically requests `StreamProfile::Compact`.
- Layout rules:
  - single pane; status line of one row;
  - side panels (agents, jobs) behind `Tab`;
  - reasoning collapsed; tool calls as one-line summaries;
  - approvals as a full-screen modal with large numbered choices;
  - no required Ctrl chords: `:` opens a command palette, `Esc Esc` interrupts.

---

## 6. Cloud home interplay (DEC-010 prerequisite, DEC-019)

DEC-010 is **not written yet** (C-10 is text only). This plan assumes its outline and must be reconciled with it. Recommendation: write DEC-010 in RS0 (orchestrator) if Copilot has not delivered it.

| Data | Where in the cloud | Why |
|---|---|---|
| Transcripts, approvals, job store (leases), hosted-session records, device registry, knowledge | **One** block volume (EBS/PD-like) attached read-write to exactly one host container | fs4 locks and lease epochs are only reliable on a local filesystem; no NFS/EFS for the state dir (see 6a) |
| AuthHub sealed store | Same volume, `0600`, owned by the auth-hub user | persistent keys (host node id must stay stable so device pins survive) |
| KEK | **Never** on the volume: platform secret, systemd credential (`kek_credential`, `harw-auth-hub/src/config.rs`) or env seed | the volume alone is useless when stolen |
| Live ring, fan-out queues, `AgentEventHub`, sockets | memory or tmpfs | ephemeral by design |
| Caches, lens index | local disk (rebuildable); optionally persisted | cost only |

- **Reclaim while clients are attached.** On SIGTERM the host goes into `drain(retry_after)`:
  1. stop accepting submits (`Denied{reason: "draining"}`);
  2. broadcast `HostDraining`;
  3. interrupt running turns (the `CancelToken`, giving `TurnAborted`), flush records and set the state to `Interrupted`;
  4. close streams.

  Jobs keep their leases, and the coordinator reclaims them after restart (existing).
- **Resume.** The new container starts, `host_epoch + 1`. Clients reconnect with their cursor; the durable replay is exact, and live deltas of the aborted turn are lost (a `Snapshot` is not possible after restart, so the client shows "turn interrupted"). The resume policy is described in 2.2.
- **Idle.** A host with attachments or running turns or leased jobs is not idle. Only when nothing is running may the platform reclaim it; everything is durable by then.

---

## 6a. Speicherkarte und synchronisiertes Home

### 6a.1 Storage map (found with grep, not guessed)
Legend: sensitivity S = secret, P = private, SH = shareable. "Sync?" = safe in Dropbox, OneDrive, Syncthing and similar.

| Path | Owner crate (evidence) | Sens. | Sync? | Locking / atomicity | Who may access |
|---|---|---|---|---|---|
| `<home>/auth.toml` (0600) | `harw-home/src/paths.rs::auth_path`, `scaffold.rs::write_auth_file` + `harden_permissions`; `harw-config/src/auth_toml.rs` (`KekConfig`, credential pool) | S | **never** | atomic write; 0600 required | owner uid |
| `<home>/sealed-secrets/` + `audit.log`, `checkpoints.log`, `.secrets-rotation-stage/`, `.secrets-rotation-backup/` | `harw-cli/src/secret_store.rs` (`home.join("sealed-secrets")`), `harw-secrets/src/store.rs` constants | S | **never** (multi-directory rotation with renames; the audit hash chain breaks on partial sync) | atomic writes; audit chain | owner uid |
| legacy `<home>/secrets` (plaintext) | `secret_store.rs` doc ("never falls back to …") | S | **never**; migrate and delete | – | owner |
| KEK file (`KekProvenance::KeyFile`, path from `auth.toml [kek] key_file_path`), OS keyring, env seed | `harw-secrets/src/kek.rs` (refuses to start unless 0600) | S | **never** | – | owner |
| AuthHub store `/var/lib/harw/auth-hub/keys.store` + `<store>.lock`; KEK `/etc/harw/auth-hub/kek` or systemd credential | `harw-auth-hub/src/config.rs`, `sealed/mod.rs` (exclusive `flock` for the provider's lifetime) | S | **never** | flock + atomic rename | auth-hub service user |
| Job store `<profile>/jobs/{records,locks,approvals}` + `jobs/work_driver/<id>.json` | `harw-session-store/src/job_store.rs` (→ `harw-job-store`); root = profile dir (`gateway.rs:928`, `telegram_launcher.rs:117`) | P | **never** (leases with epochs plus per-job fs4 locks: two machines would claim the same lease) | fs4 per job, fenced lease | harw processes of the user |
| Process jobs `<project>/.harw/state/jobs/<id>/{stdout.log,stderr.log,meta.json}` | `harw-tool-job` (`job-extraction-map.md`) | P | **never** (pid + start ticks are machine-specific) | atomic tmp+rename | local |
| `<workspace>/.harw/verify.lock` | `harw-plan-bridge/src/verify_exec.rs` (`LOCK_FILE_NAME`) | – | **never** | fs4 exclusive (DEC-004) | local processes |
| Transcripts `<profile>/sessions/<id>.jsonl` (+ meta sidecar) | `harw-cli/src/runtime_entry.rs::profile_sessions_root` (`[session].store_dir`), `harw-session-store/src/store.rs` | P (tool output may contain file content) | default **no**; `sync-encrypted` opt-in (6a.3) with a single-writer host | fs4 lock on append, atomic rewrite | owner; remote only through the host |
| Approvals `<home>/approvals/` | `ApprovalStore::new(home)` (`harw-cli/src/web.rs:306`), `harw-session-store/src/approval.rs` | P (security decision, audit) | **never** (state lives in file-suffix renames; first-wins breaks across machines) | suffix rename | owner, host |
| Child leases, freezes `<home>/freeze` | `harw-session-store/src/child_lease.rs`, `freeze.rs`, `paths::freeze_dir` | P | **never** | suffix state | local |
| Knowledge `<profile>/knowledge/{diary,kanban/boards/cards,workbench,palace,dreams,topics,rollup,matrix}`, `MEMORY.md`, `<profile>/memories` | `harw-knowledge` (`src/lock.rs` fs4), `paths::knowledge_dir`, `matrix_dir` | P/SH (user content) | **conditional**: yes with one writing machine per profile, plus conflict-copy handling; fs4 does not protect across machines | fs4 local only | owner; tenants via subfolders |
| Agents, skills, bundle `<home>/agents`, `<home>/skills`, `<home>/.bundle-manifest.toml`, `<profile>/agents/worker/agent.toml`, `<project>/.harw/agents/*/definition.toml` | `harw-home/src/bundle.rs`, `bundle_manifest.rs`, `scaffold.rs`; discovery in `harw-registry-defaults/src/roster.rs`, `config_agents.rs` | SH but **authority-relevant** (definitions select tools within the clamp) | yes, but only with trust digests (6a.3) and conflict copies ignored | none (manifest SHA-256) | readers everywhere |
| Plans/goals `<home>/plans`, `<home>/goals` (root level) and `<project>/.harw/{plans,goals,memories,state}` | `paths::plans_dir/goals_dir`, `harw-home/src/project.rs::ProjectHome` | P | home level: conditional (single writer); project level travels with git or not at all (`.harw/` is gitignored in this repo) | atomic writes **(verify)** | tenant-filtered (`ScopedGoalStore`) |
| Decisions and docs `docs/planning/**` | repo | SH | via **git**, not a sync tool | git | everyone |
| Caches `<home>/cache`, `<home>/lens_store`, `<home>/index/<visibility>`, `<home>/telemetry`, `<home>/logs`, `<home>/bug-report`, `<home>/input_history`, `<home>/ring_snapshots`, `<home>/scan_reports` | `paths.rs` (`cache_dir`, `lens_store_dir`, `visibility_index_dir`, `telemetry_dir`, …); lens fs4 (`harw-lens-store`) | P (the lens index holds chunks of private code; telemetry and logs are private) | **never** (churn, fs4, size); rebuildable | fs4 (lens) | local |
| Sockets `/run/harw/infra/{control,security,secure,network}.sock`, `/run/harw/sandbox/*`, **dev `<home>/web.sock`** | `harw-cli/src/web.rs` (dev mode puts the socket **in the home**), hub configs | – | **never**. `web.sock` inside a synced home is a defect today → RS6-04 | – | per socket mode/group |
| `installation_id`, `active_profile` | `paths::installation_id_path`, `active_profile_path` | P | **never** (a synced installation id means two machines share one identity) | – | local |
| `config.toml` (home and profile) | scaffold | SH (policy), except machine paths | yes, but paths inside `[storage]` must not be absolute machine paths (6a.2) | – | owner |
| Channel state (pairing journal, Telegram chat state, attachment cache) | `harw-channel/src/pairing_store.rs`, `harw-channel-telegram` | P | **never** | fs4 | local |
| New in this plan: `<state>/devices/`, `<state>/hosted-sessions/` | `harw-devices`, `harw-session-host` | P (public keys, but integrity-critical) | **never** | fs4 + atomic | host |
| NetSec store, `/etc/harw-netsec/config.toml` | `harw-netsec` | P | **never** (system) | atomic | root/service |

### 6a.2 Split home (DEC-016)
Three roots, resolved in `harw-home/src/storage.rs`:
```rust
pub enum StorageClass { Secret, LeaseState, Lock, Socket, PrivateLocal, Cache, Transcript, Knowledge, Authoring /*agents,skills*/, Config }
pub struct StorageLayout { pub home: PathBuf /*HARW_HOME, may be synced*/, pub state: PathBuf /*HARW_STATE_DIR*/,
                           pub runtime: PathBuf /*HARW_RUNTIME_DIR*/, pub policy: StoragePolicy }
pub struct StoragePolicy { pub transcripts: TranscriptPlacement /*Local | SyncEncrypted*/, pub knowledge: KnowledgePlacement /*Sync | Local*/ }
pub fn resolve_layout(home: &Path) -> HomeResult<StorageLayout>;
pub fn root_for(layout: &StorageLayout, class: StorageClass) -> &Path;
pub fn check_placement(layout: &StorageLayout, class: StorageClass, path: &Path) -> PlacementVerdict; // Ok | Warn(String) | Refuse(String)
```
- `HARW_STATE_DIR` defaults to `$XDG_STATE_HOME/harw` (else `~/.local/state/harw`). `HARW_RUNTIME_DIR` defaults to `$XDG_RUNTIME_DIR/harw` (0700).
- **Backward compatibility:** if neither variable is set **and** `HARW_HOME` is not in a detected sync root, then `state = home`. That is the current layout; no migration needed.
- If the home *is* detected in a sync root and `HARW_STATE_DIR` is unset, the state dir moves to the XDG default. The program warns once and offers `harw home migrate-state [--dry-run]`.
- Machine paths never live in the synced `config.toml`. Only the *policy* (`[storage] transcripts = "local"|"sync-encrypted"`, `knowledge = "sync"|"local"`) lives there. Per-machine overrides come from env or `<state>/local.toml`.
- Class → root:

  | Root | Classes |
  |---|---|
  | `home` | `Authoring`, `Config`, `Knowledge` (if policy `sync`), `Transcript` (only as `sync-encrypted`) |
  | `state` | `Secret` (auth.toml, sealed-secrets, KEK file), `LeaseState` (jobs, approvals, freeze, child leases, hosted sessions, devices), `Lock`, `PrivateLocal` (installation_id, active_profile, input_history, logs, telemetry), `Cache` (cache, lens_store, index), `Transcript` (policy `local`) |
  | `runtime` | `Socket` |

- **Placement detector** `harw-fsutil/src/placement.rs` (I). `harw-fsutil` depends only on `rustix`; `harw-auth-hub` and the job crates already use it.
  ```rust
  pub enum PlacementKind { Local, SyncFolder { provider: &'static str }, NetworkFs { fs: &'static str }, Fuse, Unknown }
  pub struct Placement { pub kind: PlacementKind, pub evidence: Vec<String> }
  pub fn classify(path: &Path) -> Placement;
  ```
  - Evidence from ancestor markers: `.dropbox`, `.dropbox.cache`, `.stfolder`/`.stignore` (Syncthing), `.sync` (Resilio), `.owncloudsync.log`/`._sync_*.db` (Nextcloud/ownCloud).
  - Evidence from known prefixes: `~/Dropbox*`, `~/OneDrive*`, `~/Google Drive`, `~/Library/CloudStorage/*`, `~/Library/Mobile Documents/com~apple~CloudDocs`, `~/Nextcloud`.
  - `statfs` magic via `rustix::fs::statfs`: NFS `0x6969`, SMB `0x517B`, CIFS `0xFF534D42`, SMB2 `0xFE534D42`, FUSE `0x65735546`, 9P `0x01021997`. **(verify)** the constants and virtiofs.
- Policy:
  - `Refuse` for `Secret | LeaseState | Lock | Socket` in `SyncFolder | NetworkFs | Fuse`. There is no override for `Secret`; the others accept `HARW_ALLOW_UNSAFE_PLACEMENT=1` for tests only.
  - `Warn` for `Transcript` (local policy) and `PrivateLocal` in a sync folder.
  - `Ok` for `Authoring`, `Config`, `Knowledge`.
- Callers that refuse: `harw-auth-hub` (store, KEK file), `harw-secrets` KeyFile loading, job-store open, `ApprovalStore` open, socket bind (`web.rs`, host), `verify.lock`.
- **Conflict copies** `harw-fsutil/src/conflicts.rs`: `fn conflict_kind(file_name: &str) -> Option<ConflictKind>` recognizes:
  - Dropbox/Nextcloud: `" (… conflicted copy …)"`; localized variants **(verify)**;
  - Syncthing: `.sync-conflict-YYYYMMDD-HHMMSS-<id>`;
  - Google Drive: `" (1)"`;
  - OneDrive: `-<HOSTNAME>` suffix (heuristic).

  Handling:
  - loaders (agents/skills discovery in `config_agents.rs`, `bundle.rs`, config layers) **never** load a conflict copy; a stale or foreign definition must not be silently loaded;
  - `harw doctor storage` lists them;
  - knowledge views show a merge task;
  - nothing is deleted automatically.

### 6a.3 Encryption and access for synced data (DEC-017)
- **Must be encrypted when synced:**
  - transcripts (`sync-encrypted`);
  - knowledge of tenants marked `confidential`;
  - any hosted-session export.

  Mechanism: per-session DEK (XChaCha20-Poly1305 via `crypt_guard`), **per-line** envelope so append-only stays possible: `{"v":1,"kid":…,"n":…,"ct":…}`. The DEK is wrapped with `harw-secrets` V3 `DekWrapper` (AuthHub KMS `SecretsKek`, or `LocalHpkeDekWrapper`). The KEK and the KMS are never synced.
- **Several readers (the manager case):** the DEK is wrapped once per authorized **recipient key**, an HPKE key of purpose `ChannelBinding` held by that principal's machine or KMS. The multi-recipient header is RS7-02. Revoking a reader means rotating the DEK for new data; data already shared stays readable by that reader (be honest about this in DEC-017).
- **Tenants in a shared folder:** `<sync_root>/tenants/<tenant>/{knowledge,agents,transcripts}`. The sync tool's folder sharing is only a coarse filter; **cryptography is the boundary**. The manager is a separate principal with their own tenant and gets read-only grants (caps `observe`) per tenant. File names in encrypted areas are opaque ids, to limit metadata leakage (name, size and timing still leak; state it).
- **Integrity of synced authoring data:** agent and skill directories under the sync root get the same trust mechanism as project `.harw` (`harw-home/src/trust.rs`, digest). A changed digest means "untrusted until confirmed" (`harw trust`). Another sharer can otherwise inject tools within the clamp.
- **Single writer:** one host machine per profile writes transcripts, even encrypted ones. Others only read. The detector warns if two `installation_id`s have written to the same session file (writer id in the line envelope).

### 6a.4 Network
See 4.7 (DEC-018). Summary of what runs where:

| Location | Runs |
|---|---|
| Private subnet (cloud host) | `harw gateway` with the session host, `harw-auth-hub`, `harw-security-hub`, `harw-netsec`, and the state volume |
| Bastion | `harw attach` for phones over SSH or mosh |
| Laptop | `harw attach` directly over node transport, reaching the host through a private endpoint or a WireGuard underlay |

---

## 7. Ring and crate placement

| Crate | Ring | New/extended | Depends on | Notes |
|---|---|---|---|---|
| `harw-protocol` | F | extended: `session_wire.rs`, `session_port.rs`, `methods.rs` | `harw-types`, serde | no tokio; std futures only |
| `harw-fsutil` | I | extended: `placement.rs`, `conflicts.rs` | `rustix` (already the only dependency) | usable from J (job store) and A (auth-hub) |
| `harw-home` | I | extended: `storage.rs`, `paths.rs` | `harw-fsutil` (exists) | |
| `harw-devices` | **I, new** | `record.rs`, `registry.rs`, `lib.rs` | `harw-types`, `harw-protocol`, `harw-fsutil`, serde, jiff, fs4 | **no crypto crate**: stores public keys only, so no `aws-lc-sys` in I (`dependency-review.md`: aws-lc Tier A only for ring A) |
| `harw-session-host` | **A, new** | host core + service + listeners | `harw-core`, `harw-session-store`, `harw-protocol`, `harw-devices`, `harw-web` (peer, identity), `harw-node-transport`, `harw-operations`, hyper, tokio | A because of core, runtime-adjacent types and aws-lc via node transport |
| `harw-session-remote` | **A, new** | client | `harw-protocol`, `harw-node-transport`, hyper, tokio, `harw-secrets` (device seed) | kept separate so a thin client does not link `harw-core` |
| `harw-node-transport` | A | extended: `authhub_signer.rs` (purpose parameter), `enroll.rs` | unchanged external dependencies | |
| `harw-security-hub` | A | extended: `policy.rs` device branch | – | |
| `harw-tui` | A | new files (5.x); depends on `harw-protocol` only for the port | – | no dependency on host or remote crates |
| `harw-cli` | A | `runtime_session_host.rs`, `device_cmd.rs`, `attach_cmd.rs`, `storage_roots.rs`, gateway wiring | – | composition root |
| `harw-session-store` | I | RS7: `cipher.rs` (`TranscriptCipher` trait) | – | implementation adapter in `harw-secrets` (I→I) |

`xtask/arch-policy.toml` additions:
```toml
[packages."harw-devices"]
layer = "I"
[packages."harw-session-host"]
layer = "A"
[packages."harw-session-remote"]
layer = "A"
```
- No J or TCB package changes.
- The `*-sys` closure of I crates stays unchanged: `harw-devices` must not pull `aws-lc-rs` and does not depend on `harw-node-transport`.
- No new third-party crates. hyper, tokio, fs4, rustix and serde are all Tier A or out of scope.
- The Tier table in `dependency-review.md` needs a "used by" update for hyper (`harw-session-host`, `harw-session-remote`).

---

## 8. Cost and limits (DEC-003)

- **Invariant (DEC-011):** one session has exactly one turn loop in the host; N clients cost 0 extra model calls.
  - Attach, replay, history, presence and heartbeat never call a model.
  - Title generation, compaction and other internal calls run once per session, exactly as today (`AgentEventKind::InternalUsage`).
- **One provider instance per host process** (DEC-003). All hosted sessions, children and work-driver workers share its `ProviderRateLimiter`.
- `HostLimits` (in `harw-session-host/src/limits.rs`):

  | Limit | Default | Rule |
  |---|---|---|
  | `max_running_turns` | ≤ provider `min(max_concurrency, rate_limit.max_concurrent)` | same computation as `load_run_config` in `job_worker_work_driver.rs` |
  | `max_running_turns_per_tenant` | – | |
  | `queue_cap_per_session` | 8 | |
  | `submit_rate_per_device` | 10/min | |
  | `max_attachments_per_session` | 16 | |
  | `max_sessions_per_tenant` | – | |

  Excess submits are queued (`Queued{depth}`), not rejected, until the queue cap.
- An HTTP 429 remains a wait signal (DEC-003); the host shows `Queued` or the turn continues, and clients see `UsageUpdated`.
- Bandwidth: `Compact` profile and delta coalescing. The phone never receives reasoning deltas.

---

## 9. Phased rollout (rounds, waves, one file per worker)

Conventions:
- **Model**: S = Sonnet (code in one file), H = Haiku (mechanical or metadata), O = Opus (design or cross-cutting review).
- **Exec**:
  - WD = can run as a work-driver job (§11.4);
  - D = manual delegation by the orchestrator (`delegate_wave`) with role-specific `[models]`;
  - OR = the orchestrator itself.
- **Acceptance**: the unit tests named in the item, in the same file (`#[cfg(test)] mod tests`, using the crate's `test_support` pattern with no `unwrap`/`expect`), plus the central build.
- **Contracts as files:** before each round the orchestrator writes `docs/planning/65-remote-sessions/contracts/RSn.md` with the exact signatures from this plan. Workers read that file and code against it.
- **Central build per round** (main session only, once, after all waves of the round). In the cloud container, prefix every step with `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0`.
  1. `cargo fmt --all`
  2. `cargo clippy --workspace --all-targets -- -D warnings`
  3. `cargo test --workspace` (including doc tests)
  4. `cargo run -q -p xtask -- gates`
  5. `cargo deny check`
  6. `make -C dod clippy test`
  7. `actionlint`

  Rounds that add crates: the first build must update `Cargo.lock` (no `--locked`) before the DoD step, which uses `--locked`.

### RS0 — Decisions, contracts, checks (text only)
| ID | File | Content / interface | Acceptance | Model | Exec |
|---|---|---|---|---|---|
| RS0-01 | `docs/planning/70-decisions/DEC-010-cloud-home.md` | only if C-10 has not landed | follows the format of the existing notes | O | OR |
| RS0-02…10 | `docs/planning/70-decisions/DEC-011…019-*.md` (one note per worker) | titles in §10 | format: Entscheidung → Warum → Folgen → Wo im Code | O (011, 012, 014, 017, 018), S (013, 015, 016, 019) | D |
| RS0-11 | `docs/planning/70-decisions/README.md` | index rows | links resolve | H | D |
| RS0-12 | `docs/planning/65-remote-sessions/README.md` | this plan as a planning compartment | – | H | D |
| RS0-13 | `docs/architecture/remote-sessions.md` | architecture doc (§2–5) | – | S | D |
| V-01 | report only | Does the in-process spawner honor `[models]` of a custom worker role? | written finding with file:line | H | D |
| V-02 | report only | list every exhaustive `match` on `IngressSurface` | list of files | H | D |
| V-03 | report only | is `#[serde(other)]` supported for adjacently tagged enums in the locked serde? | finding | H | D |

Central: `cargo run -q -p xtask -- gates` only.

### RS1 — Vocabulary and storage classification (F/I)
- **W1**
  | ID | File | Interface | Acceptance | Model | Exec |
  |---|---|---|---|---|---|
  | RS1-01 | `harw-protocol/src/session_wire.rs` (new) | §3.2 | T1 roundtrip of every param type; T2 `deny_unknown_fields`; T3 an unknown `kind` → `Unknown`; T4 `Cursor` ordering; T5 `ClientCaps::narrow` is monotone | S | WD |
  | RS1-02 | `harw-protocol/src/session_port.rs` (new) | §3.3 | trait object safety test (`Box<dyn SessionPort>` compiles in a test) | S (O reviews the contract) | WD |
  | RS1-03 | `harw-protocol/src/methods.rs` | constants from §3.2 | – | H | D |
  | RS1-04 | `harw-fsutil/src/placement.rs` (new) | §6a.2 `classify` | tests with tempdir markers (`.dropbox`, `.stfolder`); prefix tests; statfs of tmp → `Local` | S | WD |
  | RS1-05 | `harw-fsutil/src/conflicts.rs` (new) | `conflict_kind` | table test over known names | **Copilot C-12** | – |
- **W2**
  | ID | File | Interface | Acceptance | Model | Exec |
  |---|---|---|---|---|---|
  | RS1-06 | `harw-protocol/src/lib.rs` | `pub mod` + re-exports | – | H | D |
  | RS1-07 | `harw-fsutil/src/lib.rs` | exports | – | H | D |
  | RS1-08 | `harw-home/src/storage.rs` (new) | §6a.2 (`StorageLayout`, `resolve_layout`, `check_placement`) | tests: legacy layout when no env and no sync; state moves when the home is under a `.dropbox` marker; `Refuse` for `Secret` | S | WD |
  | RS1-09 | `harw-home/src/lib.rs` | exports | – | H | D |

### RS2 — Device registry and transport purpose
- **W1**
  | ID | File | Interface | Acceptance | Model | Exec |
  |---|---|---|---|---|---|
  | RS2-01 | `harw-devices/Cargo.toml` (new) | dependencies per §7 | – | H | D |
  | RS2-02 | `harw-devices/src/record.rs` | `DeviceRecord` (§4.3), `validate()` (key length 1952, transport-id grammar) | tests | S | WD |
  | RS2-03 | `harw-devices/src/registry.rs` | `DeviceRegistry::open(dir) -> Result<Self>`, `enroll(rec)`, `revoke(id, at)`, `get(id)`, `by_transport_id(&str)`, `list(tenant)`, `subscribe_revocations() -> tokio::sync::watch::Receiver<u64>`; fs4 lock + `write_atomic`; `check_placement(LeaseState)` | tests: persistence roundtrip; revoke is visible to a second handle; refuses a synced dir | S | WD |
  | RS2-04 | `harw-node-transport/src/authhub_signer.rs` | `AuthHubNodeSigner::for_purpose(hub, owner, HarwKeyPurpose)`, `wrap_transcript_for(SignPurpose, &[u8])`, `TranscriptWrappedVerifier::for_purpose(..)`; existing constructors unchanged | tests: a DeviceHandshake transcript does not verify as NodeHandshake and vice versa | S (O reviews) | D |
  | RS2-05 | `harw-node-transport/src/enroll.rs` (new) | sans-IO `EnrollClient`/`EnrollServer`, server-signed transcript, `EnrollRequest{code, public_key, label, proof}` | tests: wrong fingerprint fails; replayed request fails; proof over a different code fails | **O** | D |
- **W2**
  | ID | File | Interface | Acceptance | Model | Exec |
  |---|---|---|---|---|---|
  | RS2-06 | `harw-devices/src/lib.rs` | exports | – | H | D |
  | RS2-07 | `harw-node-transport/src/lib.rs` | exports | – | H | D |
  | RS2-08 | root `Cargo.toml` | members | – | H | D |
  | RS2-09 | `xtask/arch-policy.toml` | §7 entries | gates green | H | D |
  | RS2-10 | `docs/architecture/harw-workspace-inventory.md` | new rows | – | H | D |

### RS3 — Host core (`harw-session-host`)
- **W1**, all against the contract `RS3.md`:
  | ID | File | Interface | Acceptance | Model | Exec |
  |---|---|---|---|---|---|
  | RS3-01 | `Cargo.toml` (new crate) | – | – | H | D |
  | RS3-02 | `src/identity.rs` | `ClientIdentity` (§4.1), `caps_for_tier(PermissionTier) -> ClientCaps`, `admits(&ClientIdentity, Option<&TenantId>) -> bool` (remote without a tenant → false) | tests | S | WD |
  | RS3-03 | `src/record.rs` | `HostedSessionRecord`, `RecordStore{open, put, get, list(tenant), mark_interrupted_on_start(epoch)}` | tests | S | WD |
  | RS3-04 | `src/replay.rs` | `fn replay(reader: TranscriptReader, from: Cursor, generation: u32) -> impl Iterator<Item = Result<FrameEnvelope, HostError>>` | tests over a fixture transcript including a `history_replaced` marker | S | WD |
  | RS3-05 | `src/live_ring.rs` | `LiveRing{push(frame) -> u32, since(u32) -> Option<Vec<..>>, snapshot() -> SessionFrame}` with caps (4096 frames / 8 MiB) | tests | S | WD |
  | RS3-06 | `src/fanout.rs` | `Attachment{id, identity, profile, tx}`, `Fanout::publish(&FrameEnvelope)`; coalescing; `Lagged` on overflow; compact filter | tests: overflow → `Lagged`; delta merge; compact drops reasoning | S | WD |
  | RS3-07 | `src/arbiter.rs` | `Arbiter::submit(&ClientIdentity, SubmitParams, head: Cursor) -> SubmitResult`, `pop() -> Option<QueuedInput>`, idempotency LRU | tests: stale, force, duplicate `client_msg_id`, queue full | S | WD |
  | RS3-08 | `src/approvals.rs` | `respond(store: &ApprovalStore, id, decision, actor: ApprovalActor) -> RespondResult` (first wins), `pending_for(session)` | tests: two concurrent responders give one `Resolved` and one `AlreadyResolved` | S | WD |
  | RS3-09 | `src/limits.rs` | `HostLimits` (§8), `TenantGate` | tests | H | D |
- **W2**
  | ID | File | Interface | Acceptance | Model | Exec |
  |---|---|---|---|---|---|
  | RS3-10 | `src/driver.rs` | `pub trait TurnDriver: Send + Sync { fn open(&self, rec: &HostedSessionRecord) -> BoxFuture<'_, Result<DrivenSession, HostError>>; }` where `DrivenSession` owns `AgentSession`, `Arc<dyn StateStore>`, `Arc<dyn ModelProvider>`, `Arc<ApprovalStore>` and runs `run_turn_durable` / `resume_after_approval` with a turn-event sink | test with a fake provider (`harw-core` `testing.rs`) | S (O contract) | D |
  | RS3-11 | `src/host.rs` | `SessionHost` API: `hello/list/create/attach/history/submit/interrupt/respond/detach/resume/drain` | integration test: 2 attachments, 1 submit → 1 model call; replay after detach | **O** | D |
  | RS3-12 | `src/local_port.rs` | `impl SessionPort for LocalPort` | test | S | D |
  | RS3-13 | `src/lib.rs` | exports | – | H | D |
  | RS3-14 | `xtask/arch-policy.toml`, root `Cargo.toml` | entries | gates | H | D |

### RS4 — Service, listeners, CLI
- **W1**
  | ID | File | Interface | Acceptance | Model | Exec |
  |---|---|---|---|---|---|
  | RS4-01 | `harw-session-host/src/rpc.rs` | `dispatch(&SessionHost, &ClientIdentity, RequestEnvelope) -> ResponseEnvelope` | method-table tests; unknown method → JSON-RPC error `-32601` | S | WD |
  | RS4-02 | `harw-session-host/src/http.rs` | hyper service with `/v1/rpc`, `/v1/sessions/{id}/frames` (NDJSON) and `/v1/health` (shared contract of `harw-infra-client::info`) | tests using an in-memory body | S | WD |
  | RS4-03 | `harw-session-host/src/local_listener.rs` | `bind_local(path, resolver: Arc<dyn LocalPeerIdentityResolver>)`; 0600; `check_placement(Socket)` | test with a tempdir socket, if the crate's tests allow sockets **(verify** the rule in `harw-web/src/peer.rs` tests: "binde keinen echten Socket" applied to that node; decide per crate) | S | D |
  | RS4-04 | `harw-session-host/src/remote_listener.rs` + `device_verifier.rs` | `DeviceVerifier: NodeVerifier` over `DeviceRegistry`; `serve_remote(server, host)`; max connection lifetime; revocation kill | unit tests for the verifier | S (O reviews) | D |
  | RS4-05 | `harw-cli/src/runtime_session_host.rs` (new) | `impl TurnDriver` using `RuntimeAssembly` (pattern: `telegram_session.rs::workspace_session`) | test with `ModelSource::Echo` | S (O reviews) | D |
  | RS4-06 | `harw-security-hub/src/policy.rs` | device branch in `ContextRequest`/`PolicyEngine` | tests: `Remote` zone, `MutualTls`, narrowing only | S | WD |
  | RS4-07 | `harw-cli/src/device_cmd.rs` (new) | `pair`, `list`, `revoke`; pairing string format (§4.2); code hash only | tests | S | D |
- **W2** (Haiku, one file each): `harw-cli/src/gateway.rs` (supervise: session-host subsystem; S rather than H because the file is large), `harw-cli/src/cli/gateway.rs` (`--sessions-socket`, `--remote-listen`, `--enroll-listen`), `harw-cli/src/cli/device.rs`, `harw-cli/src/cli/mod.rs`, `harw-cli/src/main.rs`.
- Central build, plus a manual smoke test by the owner: `harw gateway` and two `harw attach --socket` sessions.

### RS5 — Client and thin TUI
- **W1**
  | ID | File | Interface | Model | Exec |
  |---|---|---|---|---|
  | RS5-01 | `harw-session-remote/Cargo.toml`, `src/lib.rs` | – | H | D |
  | RS5-02 | `harw-session-remote/src/client.rs` | `RemotePort::{connect_uds(path), connect_node(endpoint, pins, signer)}`, `impl SessionPort` | S | WD |
  | RS5-03 | `harw-session-remote/src/stream.rs` | NDJSON `FrameSource`, reconnect with cursor and backoff; tests with scripted bodies | S | WD |
  | RS5-04 | `harw-session-remote/src/pairing.rs` | parse the pairing string, key generation, sealed seed in `HARW_STATE_DIR`, run the enrollment | S (O reviews) | D |
  | RS5-05 | `harw-tui/src/connection_status.rs` (new) | status-line segment | H | D |
  | RS5-06 | `harw-tui/src/conflict_dialog.rs` (new) | 5.3 | S | WD |
  | RS5-07 | `harw-tui/src/compact_layout.rs` (new) | 5.4, `fn is_compact(cols, rows) -> bool`, layout splits | S | WD |
  | RS5-08 | `harw-tui/src/remote_view.rs` (new) | `FrameEnvelope` → history cells | S | WD |
- **W2**
  | ID | File | Interface | Model | Exec |
  |---|---|---|---|---|
  | RS5-09 | `harw-tui/src/attach_root.rs` (new) | `run_attached_tui` | O | D |
  | RS5-10 | `harw-tui/src/lib.rs` | exports | H | D |
  | RS5-11 | `harw-cli/src/attach_cmd.rs` (new) | `harw attach`, `harw sessions ls` | S | D |
  | RS5-12 | `harw-cli/src/cli/attach.rs` + `cli/mod.rs` dispatch | – | H | D |

### RS6 — Enforcing the split home
- **W1**
  | ID | File | Interface | Model | Exec |
  |---|---|---|---|---|
  | RS6-01 | `harw-home/src/paths.rs` | `state_dir()`, `runtime_dir()`; old functions stay unchanged | S | WD |
  | RS6-02 | `harw-cli/src/storage_roots.rs` (new) | `StorageRoots{profile_state, sessions, jobs, approvals, secrets, sockets}` from `StorageLayout` | S | WD |
  | RS6-03 | `harw-auth-hub/src/config.rs` | refuse a store or KEK file in a sync or network placement | S | WD |
  | RS6-04 | `harw-secrets/src/kek.rs` | refuse a KeyFile in a sync placement | S | WD |
  | RS6-05 | `harw-registry-defaults/src/config_agents.rs` | skip conflict copies | S | WD |
  | RS6-06 | `harw-home/src/bundle.rs` | skip conflict copies | S | WD |
  | RS6-07 | `harw-cli/src/home.rs` | `harw home migrate-state [--dry-run]` | S | D |
  | RS6-08 | doctor storage check in `harw-cli/src/lib.rs` (the doctor entry; locate with `grep -rn "EntryKind::Doctor" harw-cli/src`) | – | S | D |
- **W2**: call sites, Haiku, one file each: `harw-cli/src/runtime_entry.rs`, `web.rs` (dev socket → runtime dir), `secret_store.rs`, `runtime_jobs.rs`, `gateway.rs`, `telegram_launcher.rs`, `runtime_web.rs`.

### RS7 — Encrypted sync and sharing (only if the owner chooses `sync-encrypted`, Q5)
| ID | File | Model |
|---|---|---|
| RS7-01 | `harw-session-store/src/cipher.rs` (`TranscriptCipher` trait, line envelope) | O |
| RS7-02 | `harw-secrets/src/recipients.rs` (multi-recipient DEK wrap, `ChannelBinding`) | O |
| RS7-03 | `harw-session-store/src/store.rs` (hook) | S |
| RS7-04 | `harw-cli/src/storage_roots.rs` wiring | S |
| RS7-05 | tenant folder layout in `harw-home/src/storage.rs` | S |

### RS8 — Cloud home, drain, deployment docs
| ID | File | Model |
|---|---|---|
| RS8-01 | `harw-session-host/src/drain.rs` (SIGTERM → drain) | S |
| RS8-02 | resume policy in `record.rs` | S |
| RS8-03 | `docs/guides/remote-sessions.md` | S |
| RS8-04 | `docs/guides/private-network-deploy.md` | S |
| RS8-05 | `deploy/systemd/harw-sessions.socket` + unit **(verify** the existing unit layout) | H |
| RS8-06 | NetSec example zone/route config | H |

### RS9 — Local TUI convergence onto `SessionPort`
Separate plan later. Large: `app.rs` is 14,943 lines.

---

## 10. Risks, open questions, decision notes

**Risks**
1. The thin client and the full TUI diverge until RS9. Mitigation: shared cell and dialog modules; DEC-015 sets a deadline.
2. Enrollment is a crypto protocol change (RS2-05). Mitigation: Opus design, sans-IO with tests, owner review, can be switched off.
3. Unknown tool outputs broadcast to many devices widen data exposure. Mitigation: caps, tenant scoping, presence visibility, compact profile.
4. Sync tools silently break locks, leases and atomicity. Mitigation: placement refusal plus the split home. The residual risk for knowledge (multi-writer) is accepted with conflict copies.
5. The job store on NFS in the cloud → double claims. Mitigation: DEC-010 and DEC-016 require a block volume with one writer.
6. `Cargo.lock` churn when new crates land. Mitigation: one lock update in the central build.
7. Phone clients: aws-lc cross-compilation and handshake clock skew (30 s policy). Mitigation: bastion first.
8. `IngressSurface` is extended, touching exhaustive matches (V-02).
9. Work-driver automation of the rollout depends on C-09 and C-06 (see §11).

**Open questions for the owner**
- Q1 Should the local TUI also go through the daemon by default (the session survives closing the terminal), or stay embedded with attach as opt-in?
- Q2 Phone: SSH/mosh via a bastion only, or a native client (Termux, iOS terminal)?
- Q3 Default arbitration: stale-view CAS with FIFO (proposed), or a single controller lease?
- Q4 May a phone approve `approval = "always"` operations (for example `work_driver.enqueue`), or is step-up or laptop-only required?
- Q5 Transcripts in a synced home: never, or `sync-encrypted` (RS7)?
- Q6 The manager: observe only, or approve for certain tenants?
- Q7 Resume after reclaim: `manual` (proposed) or `auto`?
- Q8 Target cloud and private network kind (VPC provider, bastion vs private endpoint)?
- Q9 Should approvals reach an absent owner through the bound Telegram channel?
- Q10 Accept the new `IngressSurface::RemoteClient` (serde-visible)?
- Q11 Knowledge in the sync root: accept last-writer-wins plus conflict copies?

**Decision notes to write**
- DEC-011 Session-Host im Daemon: eine Turn-Schleife je Sitzung, Clients hängen sich an
- DEC-012 Session-Wire: JSON-RPC-Envelopes, NDJSON-Frames, Cursor = Transkript-Sequenz + Live-Ring, additive Versionierung
- DEC-013 Mehrere Steuernde: FIFO mit Stale-View-Prüfung, Freigaben first-wins, Fähigkeiten observe/steer/approve/control
- DEC-014 Geräteidentität: ML-DSA-65-Geräteschlüssel (DeviceIdentity/DeviceHandshake), Pairing-Code über server-authentisierten Enrollment-Kanal, Widerruf
- DEC-015 Remote-TUI zuerst als schlanker Client; Konvergenz der lokalen TUI später
- DEC-016 Geteiltes Home: `HARW_HOME` (synchronisierbar) / `HARW_STATE_DIR` / `HARW_RUNTIME_DIR`, Platzierungsprüfung
- DEC-017 Synchronisierte Daten: nur verschlüsselt, Mehrempfänger-DEK, Mandantenordner, Konfliktkopien werden nie geladen
- DEC-018 Privates Netz statt Nutzer-VPN; mTLS innen; IP ist nie Identität; Bedrohungsmodell
- DEC-019 Drain und Wiederaufnahme nach Reclaim (setzt DEC-010 voraus)

**Ledger:** at the landing of RS4 (host usable locally) write `MIG-014 — Remote sessions: host, wire, devices`, and `MIG-015 — Split home` at RS6, using the ledger template.

---

## 11. Umsetzungsteam (local orchestrators, Harwness-internal agents)

### 11.1 Roles
- **Orchestrator (Opus), local.** The user's root TUI session with its main model set to Opus, plus the project-local child orchestrator `rs-orchestrator` (Opus).
  - It keeps only decisions (DEC-011…019) and interfaces in context. Contracts live as files: `docs/planning/65-remote-sessions/contracts/RSn.md`.
  - It cuts waves so that each item owns exactly one file (`owned_paths = [that file]`) and every W2 depends only on W1 contracts. Shared files (`lib.rs`, `Cargo.toml`, `arch-policy.toml`) always go into a separate, sequential scribe wave.
  - It builds centrally **once per round** after every wave of the round is done (the §9 sequence), never per agent. DEC-004's `verify.lock` enforces one verification per workspace.
- **Workers**
  - `rs-implementer` (Sonnet): code in one file, no shell.
  - `rs-scribe` (Haiku): `lib.rs` exports, `Cargo.toml`, `arch-policy.toml`, index and inventory docs, verification reports V-xx.
  - `rs-reviewer` (Opus, read-only): design review of crypto, host and TUI root items (RS2-04, RS2-05, RS3-11, RS4-04, RS4-05, RS5-04, RS5-09), and the `judge_role` target.
  - Workers never build (DEC-004); the task text carries the verbatim build rule.
- **Judge.** The internal work-driver judge (`InternalModelPoint::WorkDriverJudge`, DEC-002): no tools, cheap reads, at most 256 output tokens, verdict `{"passed": bool, "comment": …, "missing": […]}`, fail closed (DEC-001). Configure it on Haiku (below).

### 11.2 Placement: project-local `.harw/agents/`, versioned copy in `examples/`
- **Project-local** `<repo>/.harw/agents/<name>/definition.toml`, because:
  - these roles are specific to this repository and this program;
  - the built-in tree `harw-registry-defaults/agents/` ships to every user, and its coverage tests (`tool_admission_coverage.rs`) would demand product-wide justification;
  - the roster loads project definitions from a **trusted** project and clamps them under their base role (`roster.rs`), so they can only narrow.
- **But `.harw/` is gitignored** in this repo (`.gitignore:45`). Keep the canonical, reviewable copy in `examples/agents/harwness-rs/<name>/` (the precedent is `examples/agents/driven-orchestrator`) and copy it into `.harw/agents/`. Alternatively change `.gitignore` to `.harw/*` + `!.harw/agents/` (Copilot C-13). Lowering is guarded by a test in the style of C-05.
- Id namespace `hwdev.agent.<name>@1` (§5 of the DSL: `<namespace>.<kind>.<name>@<major>`). **(verify)** with `harw agent check`.

### 11.3 Definition sketches (lower without HARW-DRIVER-00x)
- An orchestrator role: `child-orchestrator`.
- Spawn depth > 0: the base sets `max_depth = 1`.
- `worker_role` and `judge_role` are listed in `[delegation] targets`.
- No blank `verify` entry; every value is in range.
- `[tools]`, `[spawn]`, `[work]` and similar sections belong to the base; a same-named section in the child would be discarded, so the child only adds `[delegation]`, `[work_driver]`, `[models]`, `instructions_file` and `skills`.

`.harw/agents/rs-orchestrator/definition.toml`
```toml
schema = "harwness.agent/v1"
id = "hwdev.agent.rs-orchestrator@1"
version = "1.0.0"
extends = { id = "harwness.agent.child-orchestrator-base@1" }
role = "child-orchestrator"
specialization = "rs-orchestrator"
name = "Remote Sessions Orchestrator"
description = "Drives the RS rounds: one file per worker, contracts as files, one central verification per wave."
instructions_file = "system.md"
skills = ["planning", "implementation", "verification", "contract-fanout-migration"]

[delegation]
targets = ["rs-implementer", "rs-scribe", "rs-reviewer", "explorer"]

[work_driver]
worker_role = "rs-implementer"
judge_role = "rs-reviewer"
max_iterations = 6
max_parallel_workers = 3        # ≤ provider min(max_concurrency, rate_limit.max_concurrent), DEC-003
max_attempts_per_worker = 3
stall_iterations = 2
# One command per entry, no shell syntax and no env assignments (guide §6);
# the cloud disk flags (CARGO_INCREMENTAL=0 …) must come from the job worker's environment.
verify = [
  "cargo fmt --all -- --check",
  "cargo clippy --workspace --all-targets -- -D warnings",
  "cargo test --workspace",
  "cargo run -q -p xtask -- gates",
]
token_budget = 1500000
wall_budget_secs = 7200

[models]
provider = "anthropic"
model = "claude-opus-5-5"
effort = "high"
required_env = ["ANTHROPIC_API_KEY"]
```
`system.md` (outline):
- Read `contracts/RSn.md`.
- Create one goal per wave; one criterion per item, with the acceptance tests named.
- Scope hint `owned_paths = [target file]`.
- Enqueue with `work_driver.enqueue`; approval is always required.
- Report with `work_driver.status`.
- Never build yourself.
- Hand `/goal achieve` to the user.
- Scribe and review items go through `delegate_wave`, not the driver.

`.harw/agents/rs-implementer/definition.toml`
```toml
schema = "harwness.agent/v1"
id = "hwdev.agent.rs-implementer@1"
version = "1.0.0"
extends = { id = "harwness.agent.worker-base@1" }
role = "worker"
specialization = "rs-implementer"
name = "RS Implementer"
description = "Implements exactly one file against a fixed contract, with unit tests; never builds."
instructions_file = "system.md"
skills = ["implementation", "error-type-design", "function-signature-design"]

[work]
mode = "implementation"
may_write_code = true
may_research_web = false

[tools]
admitted = ["fs.read", "fs.list", "fs.search", "fs.glob", "fs.grep", "explore.tree", "explore.find",
            "explore.relations", "skills.search", "skills.load", "parent.message", "fs.write", "fs.edit"]
forbidden = ["shell.exec", "web.fetch", "web.search", "web.docs_rs", "web.crates_io", "job.start"]

[spawn]
max_depth = 0

[spawn.budget]
max_tokens = 150000
max_tool_calls = 80
max_wall_secs = 900

[models]
provider = "anthropic"
model = "claude-sonnet-5"      # placeholder: confirm the exact id with `harw models`
effort = "medium"
required_env = ["ANTHROPIC_API_KEY"]
```
`worker-base` owns `[lifecycle]` and `[context]`, but not `[tools]` or `[spawn]`; the bundled `rust-implementer` (`harw-home/assets/agents/rust-implementer/definition.toml`) is written the same way. The roster gives a worker that only extends `worker-base` and admits `fs.write`/`fs.edit` the `WorkspaceEdit` ceiling (`roster.rs`: "Generischer Schreib-Worker").

`.harw/agents/rs-scribe/definition.toml`: the same shape as `rs-implementer`, with:
- `specialization = "rs-scribe"`, `skills = []`, `[work] mode = "implementation"`;
- tools: `fs.read`, `fs.list`, `fs.grep`, `fs.glob`, `fs.write`, `fs.edit`, `parent.message`;
- budget `max_tokens = 60000`, `max_tool_calls = 40`;
- `[models] model = "claude-haiku-…"` (placeholder, confirm), `effort = "low"`.

`.harw/agents/rs-reviewer/definition.toml`: read-only; `extends = worker-base`; `[work] mode = "review"`, `may_write_code = false`:
- tools: `fs.read`, `fs.list`, `fs.search`, `fs.glob`, `fs.grep`, `explore.*`, `deps.graph`, `parent.message`;
- `forbidden = ["fs.write", "fs.edit", "shell.exec"]`;
- `[models] model = "claude-opus-5-5"`, `effort = "high"`.

Judge model, in project `.harw/config.toml` (trusted layer):
```toml
[internal_models.work_driver_judge]
provider = "anthropic"
model = "claude-haiku-…"   # placeholder; DEC-002: cheap reads, 256 output tokens
```

### 11.4 What can run as a work-driver job
**Prerequisites**
- **C-09** (sandboxed verify runner). Without it every Verify step is `Unverifiable` and the run escalates.
- **C-06** (TPM pacing). Recommended.
- `harw serve` or the gateway job worker is running locally.

**Constraint found in the code:** all workers of one work-driver run go through the **job worker's service provider and model** (`ModelSource::Override`, `load_run_config`). A worker role's `[models]` is not honored there. So:
- WD runs = Sonnet waves only: run the job worker with the Sonnet profile;
- Haiku and Opus items run as `delegate_wave` from the orchestrator (V-01 checks that the in-process spawner honors `[models]`);
- backlog C-14 would lift this limit.

**Suitable for WD** (items marked WD in §9, one-file code items with self-contained tests): RS1-W1, RS2-W1 (except RS2-04/05), RS3-W1, RS4-W1 (RS4-01/02/06), RS5-W1 (RS5-02/03/06/07/08), RS6-W1.

**Stays with the orchestrator or manual delegation**
- RS0 (decisions);
- every scribe wave (lib.rs, Cargo.toml, arch-policy);
- the crypto and handshake items (RS2-04/05, RS5-04);
- `host.rs`, `driver.rs`, `attach_root.rs`, `gateway.rs` wiring;
- RS7 as a whole.

**DEC-003**
- `max_parallel_workers = 3`, and the driver caps with `effective_parallel` anyway.
- Never run two WD runs against the same provider at the same time.

**DEC-004**
- The verify list runs only centrally and holds `verify.lock`.
- The end-of-round central build (including `cargo deny`, `make -C dod`, `actionlint`, which are not in `verify`) is run by the main session by hand.

### 11.5 Copilot items (backlog format, separate from the orchestrator)
| ID | Task | Size | Depends on |
|---|---|---|---|
| C-11 | `docs/architecture/storage-map.md`: turn table 6a.1 into a document, each row with its grep evidence | text | – |
| C-12 | `harw-fsutil/src/conflicts.rs` + table tests (RS1-05) | small | RS1 contract |
| C-13 | `.gitignore`: `.harw/` → `.harw/*` + `!.harw/agents/`; add the `examples/agents/harwness-rs/*` copies and a lowering test (C-05 style) | small | §11.3 |
| C-14 | Honor the worker role's `[models]` in `harw-cli/src/job_worker_work_driver.rs` (per-role provider/model, respecting the provider cap) | medium | C-06 |
| C-15 | Golden NDJSON fixture `harw-protocol/tests/session_frames.ndjson` + compatibility test (old client meets a new `kind` → `Unknown`) | small | RS1 |
| C-16 | `docs/guides/remote-sessions.md` user guide (attach, pairing, revoke, compact mode) | text | RS5 |
| (existing) C-06, C-09, C-10 | prerequisites for §11.4 and §6 | – | – |

The orchestrator keeps every security-relevant item (RS2-04/05, RS4-04, RS5-04, RS6-03/04, RS7), the host core and the TUI root.

---

### Summary for the central build (per round)
The commands the main session runs are in §9 (in order: fmt, clippy `-D warnings`, test, `xtask gates`, `cargo deny`, `make -C dod clippy test`, `actionlint`, with the cloud disk flags in the container). Tests to be added are named in each item. No tests have been written yet; this is a plan.

### Critical Files for Implementation
- /home/user/Harwness/harw-protocol/src/wire.rs (with `events.rs`, `methods.rs`; the new `session_wire.rs` builds on them)
- /home/user/Harwness/harw-cli/src/gateway/telegram_session.rs (the park/resume/FIFO template for the session host)
- /home/user/Harwness/harw-node-transport/src/authhub_signer.rs (with `identity.rs`, `server.rs`; device signer, verifier, enrollment)
- /home/user/Harwness/harw-home/src/paths.rs (with `harw-fsutil/src/lib.rs`; split home and placement detector)
- /home/user/Harwness/harw-tui/src/gateway.rs (with `runtime_root.rs`; the client-port seam for the thin attach loop)
