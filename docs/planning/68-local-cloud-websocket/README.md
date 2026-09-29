---
id: PL-68
title: Local / Self-Cloud WebSocket Control Plane
status: planned
date: 2026-09-27
baseline:
  repo: mm9942/Harwness
  ref: dev
  sha: 1ad90219d83aa305f5bfe2ca531a30b5ab41a829
related:
  - ../65-cloud-sessions/README.md
  - ../66-placement/README.md
  - ../67-containers/README.md
  - ../75-harness-patterns/openclaw.md
  - ../75-harness-patterns/gateway-contract.md
---

# PL-68 — Local / Self-Cloud WebSocket Control Plane

## Status and scope

This is a planning compartment for the W00/W11 control-plane follow-up. It is
grounded on `dev@1ad90219d83aa305f5bfe2ca531a30b5ab41a829`.

It does **not** claim that remote sessions or a first-party WebSocket control
transport are implemented. The current code remains authoritative.

This plan keeps the session semantics from PL-65 and changes the preferred
transport shape:

- one host owns session state and model turns;
- clients attach, detach and reconnect without owning the turn loop;
- durable replay comes from the transcript cursor/generation, never from an
  in-memory transport sequence;
- local and self-cloud clients use the same versioned `harw-protocol`
  envelopes and the same `SessionPort` semantics;
- **WebSocket becomes the primary bidirectional session-control/live-event
  transport**;
- existing HTTP JSON-RPC, NDJSON and SSE paths remain migration/compatibility
  surfaces until their consumers move;
- the WebSocket adapter is built on the existing Hyper stack and existing
  authenticated transport boundaries. It is not a new authority path.

This plan supersedes only the framing choice in PL-65 §3.1 ("Why not
WebSocket"). The rest of PL-65 remains the source for hosted-session semantics
unless this document explicitly narrows or refines it.

---

## 1. CURRENT — verified on dev

### 1.1 Local web/control surface

`harw-web` currently:

- binds a Unix domain socket;
- identifies the peer once after `accept()` with kernel `SO_PEERCRED`;
- resolves tier / tenant / optional SecurityHub context server-side;
- executes only routes derived from the operation registry;
- exposes `GET /events` as SSE;
- has a process-wide bounded event bus with a monotonically increasing
  `sequence`;
- detects lag, but the SSE sequence is **not** durable session replay;
- uses Hyper 1 + hyper-util + http-body-util.

The UDS boundary is a security property: a browser cannot open the local
control socket. Do not replace it with unauthenticated loopback TCP.

### 1.2 Protocol vocabulary

`harw-protocol/src/wire.rs` already contains:

- `RequestEnvelope`;
- `ResponseEnvelope`;
- `NotificationEnvelope`;
- `WireMessage`;
- `ProtocolVersion { major, minor }`;
- rejection of unsupported major versions.

The module already names WebSocket as a possible transport. There is no
first-party session-specific WebSocket adapter yet.

PL-65 has already specified the intended session vocabulary and
`SessionPort`, including:

- cursor = generation + durable + live;
- `session.hello`, list/create/attach/detach/history/resume;
- `turn.submit`, interrupt, approvals and settings;
- `FrameEnvelope` / `SessionFrame`;
- idempotent submit with `client_msg_id`;
- optimistic head checks;
- replay from the durable transcript followed by a live tail.

Those semantics stay transport-independent.

### 1.3 Remote node transport

`harw-node-transport` currently:

- establishes TLS 1.3 with X25519MLKEM768;
- authenticates nodes with ML-DSA-65 transcripts bound to the TLS exporter;
- injects an authenticated `AuthenticatedPeer` into every Hyper request;
- serves Hyper HTTP/1 over the authenticated channel;
- exposes an HTTP client after the same authenticated handshake;
- enforces handshake, replay, connection and header-read limits.

At this baseline the server drives:

```text
http1::Builder::serve_connection(...)
```

without `with_upgrades()`, and the client connection is likewise driven as a
plain HTTP/1 connection. Therefore a standard Hyper WebSocket upgrade is **not
available yet** across the node transport.

That is the first remote transport delta. Do not work around it by opening a
second unauthenticated TCP listener.

### 1.4 WebSocket dependency

`tokio-tungstenite` is present in `Cargo.lock` transitively through the
Thirtyfour browser adapter. It is not a first-party control-plane dependency.
If adopted here, it becomes an explicit reviewed Layer-A dependency owned by
the session/control adapter.

It must not enter:

- Warden;
- DoD probes/sensors;
- the privileged TCB;
- foundational `harw-protocol`.

---

## 2. TARGET — one control protocol, two trusted ingress modes

```text
                        +------------------------------+
                        |      harw-session-host       |
                        |  single writer per session   |
                        |  queue / approvals / replay  |
                        +---------------+--------------+
                                        |
                               Session service
                                        |
                         versioned harw-protocol
                                        |
                    +-------------------+-------------------+
                    |                                       |
         Local ingress                                Self-cloud ingress
                    |                                       |
      Unix socket + SO_PEERCRED                harw-node-transport
                    |                            PQC TLS + ML-DSA
                    |                                       |
             Hyper HTTP/1                             Hyper HTTP/1
                 upgrade                                 upgrade
                    |                                       |
                    +------------ WebSocket ---------------+
                                  JSON wire
                                      |
                   Request / Response / event.frame
                                      |
                            SessionPort client
```

The transport differs. Session semantics, authorization decisions, replay and
single-writer ownership do not.

### 2.1 Local mode

Default local endpoint:

```text
$HARW_RUNTIME_DIR/sessions.sock
```

Rules:

- Unix socket only; no default loopback TCP listener;
- user mode: 0600;
- system mode: 0660 with the configured control-client group;
- read `SO_PEERCRED` immediately after `accept()`;
- resolve tenant/tier/SecurityHub context before accepting a session-control
  request;
- then upgrade the authenticated Hyper connection to WebSocket;
- actor, tenant and capabilities come from the resolved peer, never from a
  frame field.

This preserves the current local property that ordinary browser JavaScript
cannot address the transport.

### 2.2 Self-cloud mode

The cloud endpoint is **not** a public generic `wss://` listener.

The preferred path is:

```text
private network / port forward
        ->
harw-node-transport
        ->
PQC TLS + node/device identity
        ->
Hyper HTTP/1 upgrade
        ->
same WebSocket session protocol
```

Rules:

- IP address is routing only, never identity;
- the `AuthenticatedPeer` extension is authoritative for transport identity;
- device/tenant/capability lookup narrows that identity;
- remote callers must always have `tenant: Some(_)`;
- no bearer token fallback;
- no public internet bind by default;
- a bastion, session-manager forward, private endpoint or private mesh may be
  used as underlay, but does not replace node identity;
- disconnecting a client detaches only the client. It does not cancel a turn,
  child agent or job.

---

## 3. W00 transport decision

### Decision

Use one first-party WebSocket connection for:

- session request/response control;
- live session frames;
- approvals;
- presence;
- job/agent status frames associated with the attached session.

Keep these existing paths during migration:

- `harw-web` operation HTTP routes;
- `harw-web GET /events` SSE for the current web operational event surface;
- PL-65 HTTP JSON-RPC / NDJSON session transport as compatibility/fallback
  until all first-party clients speak WS;
- the DoD NDJSON uplink, which remains read-only and independent.

Do **not** put Warden control, privileged DoD actions or a new operation
registry behind the WebSocket.

### Why

The session control plane is naturally bidirectional:

- requests and correlated responses;
- server-originated live events;
- approvals that race among several clients;
- presence;
- reconnect / resync;
- multiplexing several attached sessions on one client connection.

A single connection removes the need to coordinate a write channel plus a
separate event stream while retaining PL-65's transport-independent replay
contract.

### What WebSocket does not become

WebSocket is an ephemeral delivery mechanism. It is **not**:

- the durable event log;
- identity;
- authority;
- a lock lease for session ownership;
- the source of approval actors;
- a replacement for the transcript store.

---

## 4. Wire contract

### 4.1 Framing

V1 uses one UTF-8 JSON `WireMessage` per WebSocket text message.

Binary messages are rejected in v1 except for Ping/Pong/Close protocol frames.
This avoids a second codec while the contract is still stabilizing.

The existing envelopes remain authoritative:

```text
client -> host: RequestEnvelope
host   -> client: ResponseEnvelope
host   -> client: NotificationEnvelope(method = "event.frame")
```

The first application message must be `session.hello`.

`hello` negotiates:

- protocol minor;
- feature flags;
- stream profile support;
- maximum accepted encoded message size;
- heartbeat interval;
- host epoch;
- granted capabilities.

The peer may propose a label and supported features. It may not propose its
principal, tenant, approval actor or granted capabilities.

### 4.2 Multiplexing

One WebSocket may attach to multiple sessions.

Multiplexing uses the existing identifiers, not a second ad-hoc channel id:

- requests carry method-specific session ids where needed;
- `FrameEnvelope` carries `session_id`;
- request `id` correlates responses.

A client does not gain access to a session because it knows a `SessionId`.
Every attach/history/submit/approval path checks the connection identity and
session tenant/capability policy.

### 4.3 Cursor and replay

Keep PL-65 cursor semantics:

```text
Cursor {
  generation,
  durable,
  live
}
```

Reconnect:

1. authenticate the new transport;
2. negotiate `session.hello`;
3. `session.attach(from = last_cursor)`;
4. replay durable transcript records;
5. if the generation changed, emit `Resync`;
6. replay or snapshot the live ring;
7. attach to live events.

A WebSocket reconnect never trusts an old connection's capabilities. They are
re-derived from the new authenticated transport.

### 4.4 Submit and approval semantics

Keep the PL-65 rules:

- the host is the single writer;
- submit uses `expect_head`;
- `client_msg_id` is idempotent across reconnects;
- stale clients receive the current head rather than silently overwriting;
- approval first-writer-wins through the existing durable approval state;
- approval actor is derived from the connection identity;
- a frame-supplied actor field, if ever introduced by a buggy client, is
  ignored/rejected and can never become authority.

---

## 5. Backpressure, limits and lifecycle

The WebSocket adapter must be bounded before it is mergeable.

### 5.1 Connection limits

Configurable, with conservative defaults:

- max authenticated WS connections per host;
- max attachments per connection;
- max attachments per session;
- max inbound text-message bytes;
- max outbound text-message bytes;
- max queued outbound frames;
- max queued outbound bytes;
- hello deadline;
- idle ping interval;
- pong deadline;
- close/drain deadline.

The exact defaults are an implementation decision, but all limits are finite
and covered by tests.

### 5.2 Slow consumers

The turn loop never waits for a client.

Per connection:

1. coalesce disposable deltas when the queue passes a soft threshold;
2. keep the latest usage/state update where replacement is safe;
3. if the bounded queue still overflows, emit a terminal
   `Lagged { resume_from }` / `Resync` indication when possible;
4. close that attachment/connection;
5. the client reconnects and replays from the durable cursor.

No unbounded `mpsc`, `Vec` or per-client history buffer is permitted.

### 5.3 Heartbeat

Use WebSocket Ping/Pong for connection liveness.

Application `SessionFrame::Heartbeat` may remain for compatibility and UI
state but is not the transport's only dead-peer detector.

### 5.4 Drain

On host shutdown:

- stop accepting new upgrades;
- send `HostDraining`;
- stop new submits;
- allow a short bounded flush;
- close connections;
- leave jobs and durable session records consistent for restart/reclaim.

---

## 6. Identity and authority boundaries

### 6.1 Local

Identity source:

```text
UnixStream
  -> SO_PEERCRED
  -> LocalPeerIdentityResolver
  -> principal / tenant / tier / security context
  -> ClientIdentity
```

A `session.hello` label is display metadata only.

### 6.2 Self-cloud

Identity source:

```text
NodeTransport TLS + ML-DSA handshake
  -> AuthenticatedPeer
  -> DeviceRegistry / node policy
  -> tenant + narrowed capabilities
  -> optional SecurityHub context
  -> ClientIdentity
```

The request body cannot override the `AuthenticatedPeer` extension.

### 6.3 Authorization

Every state-changing request passes one shared session-service authorization
path. WebSocket does not define a parallel permission system.

Capability vocabulary remains:

- observe;
- steer;
- approve;
- control.

Effective capabilities only narrow.

---

## 7. Ownership and dependency design

### Foundation: `harw-protocol`

Additive only:

- `session_wire.rs`;
- `session_port.rs`;
- session method constants;
- transport-neutral errors / feature negotiation.

Must not depend on Hyper, Tokio or Tungstenite.

### Application/shared session host: `harw-session-host` (planned in PL-65)

Owns:

- hosted session records;
- session queues;
- single-writer turn ownership;
- replay/live-tail bridge;
- capability checks;
- connection-independent `SessionService`;
- WebSocket connection driver only if it can stay independent of local/remote
  identity acquisition.

It must not trust wire identity.

### Remote client: `harw-session-remote` (planned in PL-65)

Owns:

- `RemotePort: SessionPort`;
- reconnect/backoff;
- local-UDS and node-transport dialing adapters;
- WebSocket request correlation and frame demultiplexing.

### `harw-node-transport`

Small additive change:

- server connection future supports Hyper upgrades;
- client connection future supports Hyper upgrades;
- retain the authenticated-peer extension on the upgrade request;
- no WebSocket-specific session policy inside this crate.

### `harw-web`

Keep current operation/SSE surface.

Reuse its local peer/identity logic from composition. Do not move session
methods into the operation route table merely to avoid a new host service.

### `harw-cli`

Composition root:

- owns local sessions.sock lifecycle;
- mounts the session host;
- mounts the same host service behind node transport for self-cloud;
- maps local/remote authenticated transport identities into `ClientIdentity`;
- never accepts identity from a session frame.

### TCB rule

No Tungstenite dependency may enter Warden, DoD sensors/probes or any package
classified as privileged TCB.

Add an architecture/dependency gate when the direct dependency is introduced.

---

## 8. Compatibility strategy

### Phase A — additive

- implement protocol/session host;
- implement WS endpoint;
- retain HTTP JSON-RPC + NDJSON;
- retain current SSE;
- first-party WS clients opt in.

### Phase B — first-party default

- `harw attach` prefers WebSocket;
- local daemon attach uses UDS WebSocket;
- self-cloud attach uses node-transport WebSocket;
- HTTP/NDJSON remains fallback and test oracle.

### Phase C — retire duplicate session transport

Only after all first-party clients have migrated and replay equivalence is
tested:

- deprecate session HTTP/NDJSON routes;
- do not remove `harw-web` operation HTTP or operational SSE merely because
  session WS exists.

---

## 9. Work packages

### W00 — decision and contracts

Files:

- this plan;
- decision note for session transport;
- architecture doc;
- contract file for session wire.

Exit:

- owners and dependency direction agreed;
- local UDS and self-cloud node-transport paths explicitly separated;
- no open ambiguity about identity, authority or replay ownership.

### W01 — transport-neutral protocol

Implement PL-65 session types and `SessionPort` in `harw-protocol`.

Tests:

- round-trip;
- unknown fields fail;
- unknown frame kinds degrade safely;
- capability narrowing is monotone;
- cursor ordering / generation behavior.

### W02 — Hyper upgrade capability in node transport

Server:

- drive the HTTP/1 connection with upgrades enabled.

Client:

- drive the HTTP/1 connection with upgrades enabled;
- permit the session remote adapter to consume the upgraded stream.

Tests:

- authenticated peer is present on the upgrade request;
- forged request extension is overwritten;
- upgraded connection closes with the node transport;
- connection and handshake limits still apply.

### W03 — WebSocket session adapter

Implement:

- HTTP upgrade validation;
- `tokio-tungstenite` connection driver;
- bounded codec;
- hello deadline;
- request correlation;
- event-frame fanout;
- ping/pong and close.

Tests:

- wrong path / method / missing upgrade headers;
- binary application message rejected;
- oversize inbound frame rejected before application processing;
- no request before hello;
- unsupported protocol major rejected;
- bounded outbound queue.

### W04 — local UDS composition

Implement:

- sessions.sock listener;
- peer credential read before request handling;
- local identity resolver;
- mapping to `ClientIdentity`;
- local `RemotePort` dialer over UDS.

Tests:

- peer identity is kernel-derived;
- a claimed actor/tenant in params never changes the context;
- unknown uid / denied context fails closed;
- disconnect only detaches.

### W05 — session host + durable replay

Implement PL-65 host semantics.

Tests:

- replay after reconnect;
- old generation => resync;
- live-ring miss => snapshot/resync;
- duplicate submit;
- stale head;
- queue cap;
- disconnect during running turn does not cancel it.

### W06 — self-cloud composition

Mount the same service behind `harw-node-transport`.

Tests:

- authenticated peer maps to exactly one device/node identity;
- remote tenant is mandatory;
- cross-tenant session id is denied;
- revoked identity cannot reconnect;
- IP/address changes do not alter identity.

### W07 — approvals and concurrent controllers

Tests across the real transport boundary:

- concurrent approval first-writer-wins;
- loser receives already-resolved;
- actor comes from transport;
- observe-only peer cannot steer/approve;
- control-only methods require control capability.

### W08 — backpressure and reconnect

Tests:

- slow consumer cannot stall turn loop;
- overflow yields visible lag/resync;
- reconnect continues from cursor;
- pong timeout closes only the client;
- host drain gives bounded reconnect signal.

### W09 — first-party TUI attach

`harw attach` uses `SessionPort`.

Local and self-cloud differ only in dial/auth adapter.

### W10 — dependency and security gates

- dependency-review entry for direct `tokio-tungstenite`;
- architecture gate that keeps it out of TCB/J/D packages;
- check no session WS handler bypasses the shared authorization path;
- central build.

### W11 — operator workbench / control-plane observability

Expose through session frames:

- root run / child-agent scope;
- queued/running state;
- why an agent is waiting;
- host capacity vs provider/rate/approval/plan dependency;
- connection/presence state;
- resync reason.

This is presentation/telemetry over the same protocol, not a second control
channel.

---

## 10. Acceptance matrix

The PR series is not complete until these behaviors are covered through actual
transport boundaries:

| Case | Local UDS | Self-cloud node transport |
|---|---:|---:|
| forged actor / tenant in frame | deny / ignore | deny / ignore |
| unknown / revoked identity | fail closed | fail closed |
| cross-tenant session replay | deny | deny |
| duplicate submit after reconnect | idempotent | idempotent |
| stale `expect_head` | visible stale result | visible stale result |
| concurrent approval | first wins | first wins |
| slow consumer | gap/resync, no host stall | gap/resync, no host stall |
| old generation cursor | resync | resync |
| disconnect while turn runs | turn continues | turn continues |
| host restart | durable replay / interrupted semantics | same |
| oversize message | reject before dispatch | reject before dispatch |
| capability escalation claim | impossible | impossible |

---

## 11. Security invariants

1. Localhost is not identity.
2. IP address is not identity.
3. Wire payload is not identity.
4. WebSocket connection state is not durable state.
5. The host remains the single writer of a session.
6. Capabilities narrow; they never widen from client input.
7. Approval actors are derived from authenticated ingress.
8. A slow client cannot apply backpressure to the model turn loop.
9. Every queue and frame has an explicit bound.
10. Remote callers are always tenant-scoped.
11. No WebSocket dependency enters the Warden or DoD TCB.
12. Existing HTTP/SSE consumers are migrated incrementally, not broken by the
    first WS landing.

---

## 12. Central verification

Implementation rounds use one central build after a merged wave, not per
worker:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -q -p xtask -- gates
cargo deny check
make -C dod clippy test
actionlint
```

When the final implementation SHA is frozen, every command is recorded against
that exact SHA. A later merge invalidates the gate result.

---

## 13. Open operator decisions

The implementation can begin through W03 without resolving every product UX
question, but these choices must be explicit before remote rollout:

1. Local TUI default: embedded runtime or daemon/WS by default?
2. Remote ingress: private node transport only, or an optional public edge
   proxy later?
3. Device enrollment in this PR series, or pinned nodes first and enrollment
   as the next series?
4. May a remote/phone client approve `approval = "always"` operations, or
   does that require step-up?
5. When all first-party clients use WS, keep HTTP/NDJSON indefinitely as a
   debug/automation interface or deprecate it?

Recommended defaults for the first implementation:

- local embedded TUI remains available, `harw attach` is the first WS client;
- self-cloud uses private node transport only;
- pinned/known devices first, enrollment follows;
- high-risk approval remains laptop/local unless policy explicitly enables
  remote approval;
- keep HTTP/NDJSON until replay and migration equivalence is proven.

---

## 14. IMPLEMENTATION STATUS

At the pinned baseline:

- transport-neutral JSON-RPC envelopes: **implemented**;
- local UDS HTTP/SSE and SO_PEERCRED identity: **implemented**;
- authenticated node transport over Hyper HTTP/1: **implemented**;
- durable transcript primitives: **implemented**;
- session-specific wire types / SessionPort: **planned in PL-65**;
- hosted session service: **planned in PL-65**;
- first-party WebSocket session adapter: **not implemented**;
- Hyper upgrades through node transport: **not implemented**;
- local sessions.sock WebSocket endpoint: **not implemented**;
- self-cloud session WebSocket endpoint: **not implemented**.

No item above should be described as landed until code, tests and gates prove
it on the target branch.
