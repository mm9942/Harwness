---
id: PL-65-WS
title: "Local / Own-Cloud Session Control Plane — WebSocket transport profile"
status: proposed
date: 2026-09-27
tags: [planning, remote-sessions, websocket, local, own-cloud, control-plane]
related:
  - README.md
  - contracts/W00-websocket-control-plane.md
  - ../67-containers/cell.md
  - ../75-harness-patterns/openclaw.md
  - ../75-harness-patterns/gateway-contract.md
  - ../../architecture/dependency-review.md
---

# Local / Own-Cloud Session Control Plane — WebSocket transport profile

> **Pinned baseline:** `dev@1ad90219d83aa305f5bfe2ca531a30b5ab41a829`.
>
> **Status:** planning only. This document does not claim that remote sessions,
> device enrollment, a session host, a WebSocket adapter, or a thin remote TUI
> already exist.
>
> **Relationship to PL-65:** this document keeps PL-65's session ownership,
> cursor/replay, tenancy, capabilities, device identity, storage and drain
> semantics, but proposes a different **first production transport** for the
> new session plane: one multiplexed WebSocket connection instead of first
> implementing the planned JSON-RPC-over-POST + NDJSON frame stream and then
> migrating it.
>
> Existing production transports are not replaced: `harw-web` remains its
> UDS HTTP/SSE operation surface and the DoD uplink remains NDJSON.

---

## 1. Scope and design answer

The local laptop and an operator-owned cloud host should expose the **same
session protocol** while preserving different trust boundaries:

```text
                         harw-protocol
              SessionPort + session wire vocabulary
                                |
                       harw-session-ws
          framing / upgrade / limits / fair multiplexing
                     /                     \
                    /                       \
       Local daemon path                    Own-cloud path
       -----------------                    --------------
       Unix domain socket                   private TCP locator
       SO_PEERCRED                           harw-node-transport
       LocalPeerIdentityResolver             X25519MLKEM768 TLS 1.3
                    \                       mutual ML-DSA-65 transcript auth
                     \                     /
                      +---- harw-session-host ----+
                            single session writer
                            durable replay
                            approvals / arbitration
                            RuntimeAssembly driver
```

The WebSocket is **not** an authority boundary. It is a bidirectional framing
adapter after transport identity has already been established.

The persistent own-cloud session host is also **not** a Zellhost from
`67-containers/cell.md`. A Zellhost is an ephemeral execution placement for a
Clan/wave. The session/control host is a long-lived owner of sessions,
transcripts, approvals and client attachments. Zellhosts may run behind it and
surface their state as job/agent frames.

---

## 2. CURRENT — verified on the pinned `dev`

### 2.1 TUI and daemon

- `harw-tui/src/gateway.rs` has one implementation, `LocalGateway`. The
  current `ChatGateway` exposes borrowed `AgentSession`, `StateStore` and
  `ModelProvider` references to the embedded TUI turn loop.
- The same module already names a future `RemoteGateway`, but the existing
  borrowed-reference trait is a **construction seam**, not a viable network
  protocol. A remote endpoint cannot return Rust references to its session,
  store or provider.
- `harw-cli/src/gateway.rs` is already a long-lived daemon and already owns
  runtime-backed Telegram session execution. It is the natural composition
  root for the future session host.
- Therefore v1 should add a separate thin attached-TUI loop over
  `SessionPort`; the current embedded local TUI remains intact until the
  later convergence round.

### 2.2 Local web/control surface

- `harw-web` binds a Unix domain socket and authenticates through
  `SO_PEERCRED`.
- Its operation routes are generated only from `OperationRegistry` /
  `OperationMeta`; a raw route cannot become an operation authority path.
- `LocalPeerIdentityResolver` resolves the kernel peer into tier, optional
  tenant and optional SecurityHub context.
- `GET /events` is a process-wide bounded SSE stream with sequence numbers.
  It detects lag but does not provide durable session replay.
- This is useful identity and local-transport infrastructure, but it is not the
  session host. The new session plane reuses its public peer/identity
  primitives rather than putting model-session semantics into the web
  operation table.

### 2.3 Remote node transport

- `harw-node-transport` already gives a remote connection:
  - TLS 1.3 restricted to the hybrid `X25519MLKEM768` exchange;
  - mutual ML-DSA-65 transcript authentication bound to the TLS exporter;
  - replay protection;
  - `AuthenticatedPeer` injected server-side into requests;
  - peer IP used only as a locator, never as identity.
- `NodeTransportServer::serve` currently drives Hyper HTTP/1 without
  `with_upgrades()`.
- `NodeTransportClient` likewise drives the HTTP/1 connection without an
  upgrade-aware connection task.
- `NodeTransportServer::accept` exposes the authenticated peer and TLS stream,
  so the transport can support another protocol, but the preferred change is
  narrower: teach the existing Hyper path to preserve authenticated context
  across an HTTP/1 upgrade.

### 2.4 Protocol and durable state

- `harw-protocol/src/wire.rs` already defines strict
  `RequestEnvelope`, `ResponseEnvelope`, `NotificationEnvelope` and
  `ProtocolVersion { major, minor }`; its module contract explicitly includes
  WebSocket as a possible transport.
- `harw-protocol/src/methods.rs` already names several session-shaped methods:
  `turn.submit`, `turn.interrupt`, `approval.respond`,
  `session.close`.
- `harw-session-store` has durable transcript sequence numbers and replayable
  records. PL-65 correctly derives the durable cursor from that sequence and
  introduces a generation only when retention rewrites the transcript.
- `harw-core::AgentEventHub` is bounded; lag is already a first-class event.
  A slow client must therefore resync rather than applying backpressure to the
  turn loop.

### 2.5 Dependency reality

- `tokio-tungstenite 0.30.0` is already present in `Cargo.lock`, currently
  transitively through the Thirtyfour browser adapter.
- That does **not** make it an implicit control-plane dependency.
  A first-party session WebSocket adapter should name and review it directly.
- `docs/architecture/dependency-review.md` places the Hyper/network stack in
  ring A and explicitly keeps it out of J and the privileged TCB. The new
  adapter follows that boundary.

---

## 3. Existing PL-65 decisions retained

This profile deliberately keeps the strong parts of
`docs/planning/65-cloud-sessions/README.md`:

1. **The host is the single writer.** Clients submit intentions; no client
   directly owns `AgentSession`, a transcript writer or a model provider.
2. **Sessions outlive clients.** Detach/disconnect never cancels a turn, child
   or durable job.
3. **Replay is transcript-backed.** Reconnect uses
   `Cursor { generation, durable, live }`; live deltas are disposable.
4. **Capabilities narrow only.**
   `device_caps ∩ tier_caps ∩ security_context`.
5. **Remote tenant is mandatory.** A remote client with no tenant is denied;
   it never relies on the current `tenant_admits(None, _)` behavior.
6. **Submission arbitration stays stale-head CAS + FIFO.**
   `expect_head` prevents blind overwrite; `client_msg_id` makes reconnect
   idempotent.
7. **Approvals are first-writer-wins.** The actor is derived on the host from
   the authenticated transport identity, never accepted from a frame.
8. **Settings apply at turn boundaries** through the existing session
   controller.
9. **Pairing/device identity remains ML-DSA-65** with
   `DeviceIdentity/DeviceHandshake` and an out-of-band, single-use pairing
   code.
10. **Cloud state stays single-writer on local/block storage.** Never place
    lease/approval/session state on NFS-like shared storage.
11. **Private network is an underlay, not identity.** Node authentication is
    still required inside it.
12. **The first remote TUI is thin.** Do not rewrite the current large embedded
    TUI before the transport/session contract is proven.

---

## 4. DELTA — replace only the planned session transport

PL-65 currently plans:

```text
POST /v1/rpc                           request/response
GET /v1/sessions/{id}/frames           NDJSON event stream
```

Neither route has landed. Therefore the optimized sequence is to avoid
implementing that new transport and then replacing it.

The proposed first production session transport is:

```text
HTTP/1 Upgrade: websocket
Sec-WebSocket-Protocol: harw.session.v1

one connection:
  RequestEnvelope      client -> host
  ResponseEnvelope     host   -> client
  NotificationEnvelope host   -> client
  optional protocol control frames in both directions
```

Existing **real** transports stay unchanged:

- `harw-web /events`: SSE, process-level operation UI.
- `harw-node-transport::uplink`: NDJSON, read-only DoD uplink.
- normal `harw-web` operation requests: HTTP over UDS.
- provider HTTP APIs: unchanged.

This is a transport-profile change for a subsystem that is still only planned,
not a migration of landed session traffic.

---

## 5. WebSocket connection contract

### 5.1 Endpoint and subprotocol

The session service exposes one upgrade endpoint, conceptually:

```text
/v1/session-ws
Sec-WebSocket-Protocol: harw.session.v1
```

The exact route may be owned by `harw-session-host`; it must not become a
new `Surface::Web` operation route.

Rules:

- require an exact supported subprotocol;
- no fallback to an unversioned raw socket;
- v1 application messages are UTF-8 JSON text;
- binary application messages are reserved and rejected in v1;
- fragmented messages are reassembled only within the configured message
  bound;
- protocol/auth violations close the socket with a stable close reason;
- ordinary method errors are `ResponseEnvelope.error` and do not tear down
  the connection.

### 5.2 Connection state

```text
TransportAuthenticated
        |
        v
   WsUpgraded
        |
        v
   HelloPending --invalid/timeout--> Closed
        |
        v
      Active
        |
        +---- revocation -----------> Closed
        |
        +---- shutdown -------------> Draining ---> Closed
```

After upgrade the first application request must be `session.hello`.
Until it succeeds, no attach, list, submit, approval or control method is
accepted.

`session.hello` negotiates:

- protocol minor = min(client, host);
- feature set intersection;
- stream profile support;
- client label;
- optionally **requested** capabilities, which may only narrow authority.

The host replies with:

- negotiated minor/features;
- `host_epoch`;
- a server-generated `connection_id`;
- the effective maximum capabilities for that authenticated connection.

The client never declares its principal, tenant, approval actor or trust zone.

### 5.3 Multiplexing

A single authenticated WebSocket may attach to multiple sessions.

The connection is not the session identity:
- every session method names its `SessionId`;
- every `FrameEnvelope` names its `SessionId`;
- attachments are explicit host-side records scoped to the connection;
- detaching one session does not close the connection;
- closing the connection detaches every attachment but cancels no session.

This avoids N sockets for N tabs/sessions while preserving per-session
authorization and replay cursors.

### 5.4 Fairness and priority

Do not serialize all attached sessions through one unbounded FIFO.

Use:
- one bounded queue per attachment;
- one small connection-level control queue;
- fair round-robin/deficit scheduling over ready attachment queues.

Priority classes:

1. protocol responses, revocation/drain/security control;
2. approval requests/results and durable state transitions;
3. durable/replay frames;
4. live assistant/tool progress;
5. disposable high-frequency deltas.

A hot session cannot starve approval traffic from another session.

---

## 6. Identity and authority

### 6.1 Local daemon

Local session WebSocket traffic stays on a Unix domain socket.

```text
client
  -> AF_UNIX connect
  -> kernel SO_PEERCRED
  -> LocalPeerIdentityResolver
  -> resolved principal / tier / tenant / SecurityHub context
  -> HTTP Upgrade
  -> WebSocket
```

**Binding invariant:** there is no default `127.0.0.1:<port>` session-control
listener.

A browser cannot open the Unix socket. This preserves the current structural
defense against browser-originated localhost WebSocket attacks.

If a future browser client is wanted, it is a different ingress design with an
explicit browser authentication story; it is not enabled by changing the
local listener.

### 6.2 Own-cloud

```text
private endpoint / bastion / overlay locator
  -> NodeTransport TLS 1.3
  -> mutual node/device transcript authentication
  -> AuthenticatedPeer
  -> DeviceRegistry / tenant / caps / SecurityHub context
  -> HTTP Upgrade
  -> WebSocket
```

The network address never establishes identity. The WebSocket handshake never
accepts a principal/device/tenant claim from request JSON as identity.

The existing NodeTransport request extension must be converted to an immutable
connection identity **before** the Hyper upgrade future is handed to the
WebSocket adapter.

### 6.3 Method authorization

Every method checks both:
1. tenant admission for the target hosted session;
2. the required capability.

Minimum semantic capabilities remain PL-65's:

| Capability | Session actions |
|---|---|
| observe | list own-tenant sessions, attach, history, presence |
| steer | submit, interrupt, resume |
| approve | resolve approval requests |
| control | create/close, model/mode/effort, administrative session controls |

No `actor`, `principal`, `tenant`, `tier` or `caps` field in an
application frame can widen the resolved connection identity.

### 6.4 No generic operation bypass

v1 does **not** add a raw `op.invoke` message that can execute arbitrary
`harw-operations` entries.

The session protocol gets a closed method table. A later remote command
surface must enter through the same trusted operation metadata/adapters and
authenticated `OpContext` as existing command/web surfaces. WebSocket must
never become a side door around `OperationRegistry`, approval policy or
permission tiers.

---

## 7. Replay, backpressure and reconnect

### 7.1 Durable cursor

Keep PL-65's cursor:

```rust
Cursor {
    generation: u32,
    durable: u64,
    live: u32,
}
```

- `durable` follows transcript sequence.
- `generation` changes on retention rewrite.
- `live` indexes the in-memory live ring for the current running turn.

Attach behavior:

1. replay durable records from the cursor;
2. restore current pending approvals from `ApprovalStore`;
3. replay live-ring frames if available;
4. if live-ring history is gone, send one snapshot/resync marker and continue;
5. if generation changed, force durable resync.

The WebSocket is only the delivery mechanism; it never becomes the durable
event log.

### 7.2 Slow clients

Adopt PL-65's initial per-client target:
- up to 1024 queued frames or 4 MiB, whichever is reached first;
- coalesce compatible assistant/reasoning deltas above a pressure threshold;
- keep only the newest usage/progress update when superseded;
- never drop approval/security/durable state transitions;
- on exhaustion, close that attachment with an explicit
  `Lagged { resume_from }` / resync outcome.

The session turn loop must never await a slow network writer.

### 7.3 Connection limits

The implementation contract must bound:
- maximum WebSocket message size;
- maximum aggregate queued bytes per connection;
- maximum active attachments per connection;
- maximum in-flight request IDs;
- hello timeout;
- idle/ping timeout;
- maximum authenticated remote connections/device/tenant.

Exact byte/request defaults are implementation constants only after a
repository-wide maximum legitimate payload audit. Do not choose a small magic
number that silently truncates current tool or document outputs.

### 7.4 Liveness

Use WebSocket Ping/Pong for transport liveness.

A semantic `Heartbeat` frame may still carry host epoch/state information,
but it should not duplicate the transport ping every few seconds.

Reconnect uses bounded exponential backoff with jitter and resumes from the
last accepted cursor. A connection timeout is not a turn cancellation.

---

## 8. Device enrollment and revocation

Normal NodeTransport deliberately rejects unknown peers before the server
signs anything. Preserve that property.

Device enrollment therefore stays a separate, explicitly enabled listener as
in PL-65:

1. a local/control-authorized operator issues a short-lived single-use pairing
   record;
2. the pairing string pins the expected host identity/fingerprint and locator;
3. the enrollment protocol authenticates the server first;
4. the device proves its newly generated DeviceHandshake key plus pairing code;
5. the host stores only the approved device identity, tenant and capability
   ceiling;
6. later connections use normal mutual node transport.

### 8.1 Host aliases

For usable local/own-cloud operation add a machine-local host registry
conceptually under:

```text
$HARW_STATE_DIR/hosts/<alias>.json
```

A record contains only locator/trust metadata, for example:
- human alias;
- endpoint locator;
- expected node id;
- pinned host fingerprint/key reference;
- tenant;
- enrollment/device id reference.

The registry is machine-local state, not project config.

Never auto-connect to an arbitrary endpoint supplied by chat content, a query
parameter or an untrusted repo. Creating/changing a host record is an explicit
trusted action.

### 8.2 Live revocation

Revocation is not complete if it only blocks the next reconnect.

The device registry is read live and revocation must:
1. mark the device revoked atomically;
2. make new node handshakes fail;
3. signal every active WebSocket connection of that device;
4. discard its queued submissions;
5. revoke associated short-lived SecurityHub contexts;
6. close the connection;
7. emit an audit event.

If live revocation is implemented, the PL-65 proposed hard 60-second
connection lifetime can be relaxed to a configurable authentication lease
aligned with SecurityHub context renewal. Reconnect remains cheap and cursor
safe. Until live revocation is proven, keep the shorter fail-safe lifetime.

---

## 9. Own-cloud deployment profile

### 9.1 Host role

The own-cloud node is a **persistent session/control host**:
- `harw gateway` composition;
- session-host subsystem;
- runtime/model provider ownership;
- transcript/approval/job/session records on local persistent state;
- AuthHub/SecurityHub integration;
- optional placement/execution children.

It is not a Zellhost.

### 9.2 Storage

Keep PL-65's split:
- `HARW_HOME`: authoring/config/sync-safe material only;
- `HARW_STATE_DIR`: hosted sessions, transcripts by policy, approvals, jobs,
  device/host registry, secrets metadata, locks;
- `HARW_RUNTIME_DIR`: sockets and ephemeral runtime endpoints.

Own-cloud durable state sits on a single-writer local/block volume. Do not use
NFS/EFS-like shared storage for fs4 locks, approvals or fenced job leases.

The KEK remains outside that volume (platform secret/systemd credential or
equivalent).

### 9.3 Network

Default:
- no public session-control ingress;
- private subnet/private address;
- access via bastion/session-manager/private endpoint or an overlay used only
  as an underlay;
- NodeTransport mutual PQ authentication still runs inside the private network;
- egress through the existing policy/gateway path.

The own Cloudflare Worker described by
`75-harness-patterns/gateway-contract.md` is a **model-provider gateway**.
It is not the session-control host and does not terminate this WebSocket.

### 9.4 Drain/reclaim

On host shutdown/replacement:
1. reject new submits;
2. send `HostDraining`;
3. interrupt/flush running session turns according to the durable-turn policy;
4. persist `Interrupted` where needed;
5. close client connections with reconnect guidance;
6. let durable jobs follow their existing lease/recovery semantics.

After restart, increment `host_epoch`; reconnecting clients replay durable
state and see that live-only deltas from the old process are gone.

---

## 10. Crate and dependency ownership

### `harw-protocol` — pure contract

Own:
- `session_wire.rs`;
- `session_port.rs`;
- method names;
- cursors, frames, hello, capabilities, request/result payloads.

Must **not** depend on Tokio, Hyper, Tungstenite, node transport, TUI, CLI or
policy crates.

### `harw-session-ws` — new ring-A transport adapter

Own only:
- HTTP/1 WebSocket upgrade glue;
- Tungstenite message codec;
- subprotocol negotiation;
- message limits;
- request-id correlation;
- fair multiplexing pump;
- Ping/Pong/close semantics.

It must not own:
- tenants;
- device registry;
- approval policy;
- session store;
- model provider;
- RuntimeAssembly;
- operation authorization.

Directly review/pin `tokio-tungstenite` here instead of relying on the
browser adapter's transitive dependency.

### `harw-session-host` — planned application/session service

Own:
- hosted session record;
- single-writer lifecycle;
- durable replay/live ring;
- per-attachment fanout;
- submission arbiter/idempotency;
- approval resolution;
- capability checks against an already-resolved client identity;
- local and remote listener composition;
- drain/restart state.

### `harw-session-remote` — planned client adapter

Own:
- local UDS connection;
- own-cloud node-transport connection;
- WebSocket upgrade;
- request correlation;
- reconnect/backoff;
- cursor resume;
- `SessionPort` implementation;
- pairing/host-registry client flows.

### `harw-node-transport`

Add only the generic ability to keep an authenticated HTTP/1 connection alive
through upgrade:
- server connection upgrade support;
- client upgrade support;
- preserve `AuthenticatedPeer` into the upgraded connection context.

Do not add session semantics to this crate.

### `harw-web`

Keep:
- existing UDS operation surface;
- existing SSE stream;
- peer credential/identity primitives.

Do not move session RPCs into `WebRouteTable`.

### `harw-tui`

v1:
- add a separate attached-session loop over `SessionPort`;
- reuse rendering/input/approval components;
- leave embedded local turn ownership unchanged.

Later:
- converge local TUI onto `LocalPort`/SessionPort and retire the direct
  `ChatGateway` turn ownership once parity is proven.

---

## 11. Security invariants

These are acceptance invariants, not recommendations:

1. Local session control is UDS by default; no unauthenticated loopback TCP.
2. Remote IP/hostname is never identity.
3. WebSocket frames cannot supply or override principal, tenant, tier,
   SecurityHub context, approval actor or effective capability ceiling.
4. Remote tenant is always `Some`.
5. Requested capabilities only narrow.
6. Every session method authorizes the target session independently.
7. No generic WS operation path bypasses `OperationRegistry` or approval
   policy.
8. No provider credential, KEK or secret seed is sent to a TUI client.
9. A slow/reconnecting client cannot block or duplicate a model turn.
10. Device revocation terminates active control, not only future connects.
11. `tokio-tungstenite` is unreachable from ring J, DoD and the Warden TCB.
12. First-party Rust keeps `unsafe_code = "forbid"`.
13. Existing SSE/NDJSON consumers retain behavior until an explicit,
    separately reviewed migration exists.
14. Own-cloud session host and Zellhost remain different lifecycle/security
    concepts.

---

## 12. Observability

Add connection/session-control observability without exposing transcript text:

- active authenticated WS connections by local/remote class;
- active attachments;
- attach/detach/reconnect totals;
- resync/lag totals and reason;
- replay record count/bytes;
- outbound queued bytes/high-water mark;
- denied method totals by stable reason code;
- device revocation closes;
- handshake/hello failures;
- close reason;
- host epoch changes.

Job/agent frames should include a stable **wait reason** so a client can
distinguish:
- host worker capacity;
- provider concurrency/rate limit;
- placement unavailable;
- approval pending;
- dependency/plan block;
- sandbox/runtime capacity.

The UI should not infer these reasons from free text.

---

## 13. Migration / implementation rounds

This PR is W00: decision and contract only.

### W01 — protocol contract
- add pure `session_wire` and `SessionPort` types;
- golden compatibility fixtures;
- unknown frame fallback;
- no transport dependency.

### W02 — authenticated WebSocket substrate
- make NodeTransport HTTP/1 upgrade-aware;
- add `harw-session-ws`;
- local UDS upgrade harness;
- dependency-review + architecture gate.

### W03 — session host core
- hosted records;
- replay/live ring;
- fanout/backpressure;
- arbiter/idempotency;
- approval race handling;
- host single-writer integration tests.

### W04 — local daemon profile
- session host composed under `harw gateway`;
- UDS + SO_PEERCRED identity;
- local `harw attach` client;
- two-client smoke test.

### W05 — own-cloud/device profile
- DeviceRegistry;
- pairing/enrollment listener;
- NodeTransport WS path;
- remote tenancy/capability narrowing;
- live revocation.

### W06 — thin TUI
- attached TUI loop;
- connection/reconnect status;
- compact profile;
- stale-submit dialog;
- approvals/presence;
- no RuntimeAssembly client-side.

### W07 — cloud lifecycle/storage
- split home enforcement required by session host;
- drain/reclaim;
- host aliases;
- private-network deployment examples;
- persistent volume checks.

### W08 — observability and hardening
- queue/lag/replay/auth metrics;
- wait-reason frames;
- targeted M1/M5 review over upgrade, pairing and message-size boundaries;
- docs and migration ledger.

A later round may converge the embedded local TUI onto SessionPort. Do not make
that a prerequisite for the first useful local/own-cloud session host.

---

## 14. Required tests

The implementation is not complete until tests cross the real trust
boundaries.

### Protocol / WebSocket
- unsupported subprotocol denied;
- `session.hello` required first;
- hello timeout;
- binary app frame rejected in v1;
- inbound frame/message bound;
- too many in-flight request ids denied without unbounded allocation;
- unknown additive SessionFrame maps to `Unknown`;
- one connection multiplexes two sessions without starvation.

### Local identity
- JSON claiming another uid/principal/tenant cannot change the
  `SO_PEERCRED`-derived identity;
- local socket permissions are enforced;
- no default loopback TCP session listener exists;
- untrusted browser-style Origin is rejected if an Origin header appears.

### Remote identity
- authenticated `AuthenticatedPeer` survives the HTTP upgrade;
- frame-claimed device/tenant ignored;
- unknown/revoked device denied;
- cross-tenant list/attach/history/replay denied;
- caps can narrow but never widen.

### Session correctness
- duplicate `client_msg_id` causes one model invocation;
- stale `expect_head` returns `Stale`;
- two attached clients observe one turn/model call;
- disconnect does not cancel the turn;
- concurrent approval has one winner and actor is server-derived;
- queued submit survives unrelated client detach.

### Backpressure / recovery
- slow consumer never blocks the turn loop;
- overflow returns an explicit lag/resume cursor;
- reconnect replays durable records exactly once;
- old generation forces resync;
- restart changes host epoch and replays durable data while live-only deltas are
  correctly lost/interrupted;
- active device revocation closes the socket and a reconnect is refused.

### Architecture gates
- `harw-protocol` remains transport/runtime independent;
- `harw-session-ws` remains ring A;
- no dependency path from J/D/T or Warden to Tungstenite;
- no new raw authority path around operation/approval policy.

---

## 15. Verification gates per implementation round

Run centrally against one frozen head:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -q -p xtask -- gates
cargo deny check
make -C dod clippy test
actionlint
```

Subagents may edit/review their owned files, but they do not run independent
workspace builds in parallel. A change after the frozen verification SHA
invalidates the result.

---

## 16. Open operator decisions

These are intentionally left explicit instead of being silently guessed:

1. **Local default:** should plain `harw` eventually attach to the daemon by
   default, or remain embedded with `harw attach` opt-in until the convergence
   round?
2. **Remote auth lease:** after live revocation is proven, what maximum
   connection/auth-context lifetime should replace the conservative PL-65
   60-second reconnect cycle?
3. **Phone path:** Termux/native client over NodeTransport now, or bastion-first
   as the first supported mobile deployment?
4. **High-risk approvals:** may a remotely paired phone resolve
   `approval = "always"`, or must those require a stronger/laptop-only
   context?
5. **Browser future:** remain explicitly out of scope, or reserve a separate
   browser gateway design? It must never weaken the local UDS invariant.
6. **Synced transcripts:** remain local-only by default; if
   `sync-encrypted` is wanted, PL-65 RS7 remains a separate cryptographic
   work package.

None of these decisions blocks W01-W04.

---

## 17. Acceptance criteria for this design

This planning profile is ready to hand to implementation when:

- the existing PL-65 semantic invariants are explicitly preserved;
- the transport delta is limited to the not-yet-landed session plane;
- local UDS and remote NodeTransport identity are both upstream of WS;
- the crate ownership prevents Tungstenite from leaking into protocol/J/DoD/TCB;
- own-cloud host and Zellhost have distinct lifecycle/ownership definitions;
- replay/backpressure/revocation behavior is transport-independent;
- security and restart tests name the real boundaries;
- implementation can proceed in file-scoped waves without first building an
  NDJSON session transport that will immediately be retired.

See `contracts/W00-websocket-control-plane.md` for the fixed work-package
contract.
