---
id: W00-SESSION-WS
title: "W00 Contract — Local / Own-Cloud WebSocket Session Control Plane"
status: proposed
date: 2026-09-27
parent:
  - ../local-own-cloud-websocket.md
  - ../README.md
---

# W00 Contract — WebSocket Session Control Plane

This contract turns the design in
`../local-own-cloud-websocket.md` into implementation boundaries that worker
waves can consume without inventing architecture.

**Baseline used to write this contract:**
`dev@1ad90219d83aa305f5bfe2ca531a30b5ab41a829`.

W00 is documentation only. All code items below are future work.

---

## 1. Fixed decisions

### D1 — One semantic session contract

`harw-protocol` owns the transport-independent session vocabulary and
`SessionPort`.

The same semantics must work through:
- in-process `LocalPort` for tests/embedded composition;
- WebSocket over local UDS;
- WebSocket over authenticated `harw-node-transport`.

A transport must not change session authorization or lifecycle semantics.

### D2 — WebSocket is the first production session transport

Do **not** first implement PL-65's planned `POST /v1/rpc` + NDJSON session
frame stream merely as an intermediate migration.

Existing production SSE/NDJSON consumers remain unchanged.

### D3 — local != loopback TCP

Local control is AF_UNIX + `SO_PEERCRED`.
No default loopback-TCP WebSocket listener is introduced.

### D4 — remote identity precedes upgrade

Remote order is:

```text
TCP locator
-> NodeTransport TLS + mutual transcript auth
-> AuthenticatedPeer
-> device/tenant/cap/security resolution
-> HTTP/1 WebSocket upgrade
-> session protocol
```

No application frame establishes identity.

### D5 — closed session method table

v1 has explicit session methods. It does not expose generic operation
execution.

**R18 amendment (wire minor 2):** generic operation execution remains
forbidden. Tool invocation is allowed only as specified in
[`R18-tool-gateway.md`](./R18-tool-gateway.md): through the closed `tool.*`
methods (`tool.list`, `tool.call`, `tool.cancel`), for agent principals
derived from the UIA, with the `tool_call` cap and a tool grant; and gateway
inspection/administration only through the closed `gateway.*` table. Both
tables are explicit method lists, not a generic dispatcher.

### D6 — one WS can multiplex many sessions

The connection is an authenticated client channel. Session attachment is
separate and may be N-per-connection.

### D7 — durable replay remains outside WS

The transcript/cursor is truth. WebSocket live traffic is not the event store.

### D8 — slow clients are lossy, the turn loop is not

Live deltas may coalesce/resync. Durable/security/approval state must not be
silently dropped. Network writers never backpressure the model turn loop.

### D9 — persistent host != Zellhost

The session/control host is long-lived. The Zellhost is an ephemeral execution
placement behind it.

### D10 — direct Tungstenite ownership is ring A only

A dedicated ring-A adapter owns the dependency. `harw-protocol`, J, DoD and
Warden do not.

---

## 2. Target crate contracts

## 2.1 `harw-protocol`

### Files
- `src/session_wire.rs` — new
- `src/session_port.rs` — new
- `src/methods.rs` — additive constants
- `src/lib.rs` — exports
- compatibility fixture(s) under `tests/` (golden:
  `tests/fixtures/session_wire_golden.json`)

### Must expose

At minimum:

```rust
pub struct Cursor {
    pub generation: u32,
    pub durable: u64,
    pub live: u32,
}

pub struct ClientCaps {
    pub observe: bool,
    pub steer: bool,
    pub approve: bool,
    pub control: bool,
}

pub enum StreamProfile {
    Full,
    Compact,
}

pub enum HostedState {
    Idle,
    Running,
    WaitingForApproval,
    WaitingForChild,
    Queued { depth: u32 },
    Interrupted,
    Failed,
    Closed,
}

pub struct FrameEnvelope {
    pub session_id: SessionId,
    pub cursor: Cursor,
    pub frame: SessionFrame,
}

pub enum SessionFrame {
    // typed turn/session/child/approval/job/presence frames
    // snapshot/resync/lagged/drain/revoked/heartbeat
    // plus an unknown/additive fallback
}
```

Also define typed params/results for:
- `session.hello`;
- `session.list`;
- `session.create`;
- `session.attach`;
- `session.detach`;
- `session.history`;
- `session.resume`;
- `session.close`;
- `session.set_model`;
- `session.set_mode`;
- `session.set_effort`;
- `turn.submit`;
- `turn.interrupt`;
- `approval.respond`.

`SessionPort` uses std futures/boxed futures only. No Tokio types in its public
contract.

### Forbidden dependencies
- Tokio
- Hyper
- Tungstenite
- harw-node-transport
- harw-web
- harw-cli
- harw-tui
- harw-session-host

---

## 2.2 `harw-session-ws` — new ring-A adapter

### Responsibility

Transport only.

### Owns
- Hyper HTTP/1 upgrade helpers;
- exact subprotocol `harw.session.v1`;
- Tungstenite message conversion;
- message/frame limits;
- hello timeout enforcement hook;
- request-id correlation support;
- bounded connection queues;
- fair multiplexer over attachment streams;
- Ping/Pong/Close;
- stable protocol close reasons.

### Does not own
- principals;
- tenants;
- device registry;
- SecurityHub;
- permissions/cap decisions;
- ApprovalStore;
- TranscriptStore;
- RuntimeAssembly;
- model/provider logic.

### Dependency rule

Pin/review a direct `tokio-tungstenite` dependency here. Do not rely on
Thirtyfour to pull it transitively.

Architecture gates must prove no dependency path from:
- ring J;
- DoD;
- Warden/privileged TCB

to this crate/Tungstenite.

---

## 2.3 `harw-node-transport`

### Required delta

The current Hyper HTTP/1 server/client drivers must become upgrade-aware while
preserving the current authenticated context.

Server acceptance requirement:
- `AuthenticatedPeer` is finalized before HTTP is served;
- the request extension is injected before the upgrade request reaches the
  service;
- the upgraded I/O retains an immutable copy of that already-authenticated
  identity.

Client requirement:
- the HTTP/1 connection task is driven in the mode required for upgrade;
- the client verifies the expected node before it can return an upgraded
  stream.

### Must not change
- TLS provider;
- X25519MLKEM768-only key exchange contract;
- transcript signature format;
- replay/freshness policy;
- IP-is-not-identity invariant;
- existing HTTP service behavior;
- DoD uplink wire format.

---

## 2.4 `harw-session-host` — planned ring-A service

### Core API responsibility

Host is the single owner/writer of active session runtime state.

Expected internal areas:
- `identity.rs`
- `record.rs`
- `replay.rs`
- `live_ring.rs`
- `fanout.rs`
- `arbiter.rs`
- `approvals.rs`
- `limits.rs`
- `driver.rs`
- `host.rs`
- `rpc.rs`
- `local_listener.rs`
- `remote_listener.rs`
- `drain.rs`

These names are planning interfaces, not claims of existing files.

### Client identity

A resolved identity passed to the host contains, conceptually:
- authenticated `Principal`;
- mandatory remote tenant;
- effective cap ceiling;
- optional device id;
- host-derived approval actor;
- label;
- trust zone;
- auth strength;
- connection id.

No request payload can replace those fields.

### Host operations

The host owns:
- hello/list/create;
- attach/history/detach;
- submit/interrupt/resume;
- approval response;
- session settings;
- close;
- drain.

Each target-session operation performs tenant + capability admission.

---

## 2.5 `harw-session-remote` — planned client

### Responsibilities
- connect local UDS;
- connect own-cloud NodeTransport;
- perform WS upgrade;
- `session.hello`;
- multiplex request correlation;
- receive session frames;
- reconnect with backoff/jitter;
- resume attachment cursors;
- host alias store integration;
- pairing/enrollment client;
- implement `SessionPort`.

No provider credential or host secret is present in the client.

---

## 3. Wire and connection contract

### 3.1 Upgrade

Conceptual endpoint:

```http
GET /v1/session-ws HTTP/1.1
Connection: Upgrade
Upgrade: websocket
Sec-WebSocket-Protocol: harw.session.v1
```

Transport-specific identity has already been established before the
application handler accepts this upgrade.

If `Origin` is present in v1, reject it by default. Browser control is not a
v1 capability.

### 3.2 Application messages

v1:
- one UTF-8 JSON `WireMessage` per WS text message;
- binary is reserved/rejected;
- request IDs are unique while in flight;
- response ID must match one open request;
- notifications do not consume request IDs.

A malformed JSON/application message returns a protocol error or closes the
connection according to severity; it must never panic.

### 3.3 Hello gate

Before a successful `session.hello`:
- no session list;
- no attachment;
- no submit;
- no approval;
- no settings/control.

The host chooses the effective protocol minor/features/caps.

### 3.4 Multiplex

Connection keeps a map:

```text
SessionId -> Attachment {
    profile,
    cursor,
    bounded outbound queue,
    authorization snapshot/lease reference,
}
```

Authorization is rechecked where revocation/context expiration requires it;
the attachment map itself is not authority.

### 3.5 Contract delta (S01, wave A)

Frozen here so S02-S10 do not re-decide it. Code anchors are in
`harw-protocol/src/session_wire.rs`; shapes are pinned by
`harw-protocol/tests/fixtures/session_wire_golden.json` and
`tests/session_wire_compat.rs`.

1. **Connection : attachment is 1:N (R1, D6).** One authenticated
   connection holds a map `SessionId -> Attachment`. The map is a routing
   aid, not authority: every target-session operation re-runs tenant + cap
   admission, and the lease/snapshot is rechecked where revocation or context
   expiry requires it. Detaching or dropping a connection never cancels a
   running turn (SES-04).
2. **Session : host is 1:1 (R2).** A session has one owner host, identified
   by node id plus `placement_generation` (`PlacementGeneration(u64)`,
   starts at 1, never wraps, serialized as a bare integer). v1 has no
   cross-node failover. A client whose remembered placement generation is
   lower than the host's must resync instead of trusting a cached cursor.
   `placement_generation` is **not yet carried by any minor-2 message**;
   adding it is an additive field plus a wire-minor bump (decision for the
   host scope that first needs it). It is a separate axis from
   `Cursor::generation` (transcript rewrite by retention).
3. **Identity chain (R3).** transport credential (UDS uid via `SO_PEERCRED`,
   or node key + device via `AuthenticatedPeer`) -> `ClientIdentity` ->
   principal / tier / tenant / cap ceiling. No identity field exists in any
   request param (`deny_unknown_fields` rejects extras, including `tenant`,
   `principal`, `actor`). `PermissionTier` is a policy input, not key
   authorization. Requested caps can narrow, never widen (R4, ID-04).
4. **Cursor `(generation, durable, live)` (R5).** `durable` is the transcript
   sequence of the next record the client has not seen; `live` indexes the
   live ring of the running turn and is disposable; `generation` changes only
   on a transcript rewrite. The transcript is truth and replay is outside WS
   (D7). A cursor from an older generation, or one the host cannot serve,
   is answered with `Resync { reason, head }`; a client never fabricates a
   cursor. All three components are required on the wire.
5. **Approvals: first writer wins (R7, APP-01).** `approval.respond` carries
   no actor; the host derives it from the connection. The durable backend
   decides the single winner. Every later responder receives
   `RespondResult::AlreadyResolved { by }` (or `Expired`), and an
   `ApprovalResolved` frame goes to all attachments. Approval request/result
   frames are never dropped under backpressure (section 4).
6. **Compatibility rules.** `SESSION_WIRE_MINOR` is 2;
   `SESSION_WS_SUBPROTOCOL` is exactly `harw.session.v1`;
   `SESSION_WS_PATH` is `/v1/session-ws`. Requests and result structs use
   `deny_unknown_fields`; the one tolerant place is `SessionFrame`, where an
   unknown `kind` decodes to `Unknown`. Changing a golden shape is a wire
   change: keep the old shape or bump the minor.

### 3.6 Numbering cross-map (W01-W08, hub labels, slices)

Three labelings exist. Verification against the docs in this directory
(`../README.md`, `../local-own-cloud-websocket.md`) found:

- **W01-W08** are defined in this document (section 10) and in
  `../local-own-cloud-websocket.md` section 13. They agree.
- **RS0-RS9** are the hub's own program rounds (`../README.md` section 9).
  They predate the WebSocket profile; the W-packages re-cut the same work.
- **H0-H9 are not defined in the 65-cloud-sessions hub.** The only `H<n>`
  labels in the planning tree are the harness-pattern rows H1-H7 in
  `../../75-harness-patterns/README.md` (H1 daemon-owned sessions, H4 durable
  resume, ... ) and "H11" in `../README.md` (a transport exit criterion).
  They do not form a W00 numbering. The `H0..H9` column in
  `W00-ws-integration-map.md` is therefore **unverified and should not be
  relied on**; the map itself says it is not authoritative for H-labels.
- **W1-W6 slices** are orchestrator planning labels with no definition in the
  hub docs; `W1`/`W2` inside `../README.md` mean worker waves within a round.

Authoritative mapping (W-package to hub round, verified by content):

| W00 package | Hub round (`../README.md` section 9) | Orchestrator slice (unverified in hub) | Scopes |
|-------------|--------------------------------------|----------------------------------------|--------|
| W01 contract | RS0 contracts, RS1 vocabulary | W1 | S01 |
| W02 WS substrate + NodeTransport upgrade | RS2-05 (transport), RS4 (listeners, in part) | W2 | S02; substrate landed (R1) |
| W03 host core | RS3 | W3 | landed (R1); S03, S04 |
| W04 local usable slice | RS4 | W4 | S03, S05, S06, S09 |
| W05 own-cloud usable slice | RS2, RS4 (devices, enrollment, revocation) | W5 | S02, S06, S07, S08 |
| W06 thin TUI | RS5 | W6 | S09 |
| W07 lifecycle/storage/deploy | RS6, RS8 | W6 | S05 |
| W08 hardening | RS8 (docs, ledger); observability is new | W6 | backlog |

The mapping of W02 and W05 to RS2/RS4 is by content, not by an explicit hub
statement; where the hub is silent it is marked as such. Resolve the H-column
in the integration map (orchestrator-owned) by deleting it or replacing it
with the RS column above.

---

## 4. Backpressure contract

Initial targets inherited from PL-65:
- 1024 frames per attachment OR 4 MiB queued;
- coalesce compatible live deltas under pressure;
- bounded connection-level queue;
- bounded in-flight requests.

Never silently discard:
- RPC response;
- approval request/result;
- revocation/drain;
- durable session state transition required for correctness.

May compact/drop with explicit resync semantics:
- assistant/reasoning delta;
- repeated usage update;
- child progress;
- other disposable live-only progress.

On exhausted attachment:
- issue `Lagged { resume_from }` if possible;
- detach/close that stream;
- client reattaches from the cursor;
- host turn continues.

---

## 5. Local profile contract

Listener:
- AF_UNIX under `HARW_RUNTIME_DIR`;
- restrictive socket permissions;
- `SO_PEERCRED` required.

Identity:
- reuse `harw-web` peer credential / local identity resolver primitives;
- derive principal/tier/tenant/context before WS Active state.

Explicitly forbidden:
- treating `localhost` as authenticated;
- default `ws://127.0.0.1` control listener;
- bearer token as a substitute for local kernel identity.

---

## 6. Own-cloud profile contract

Network:
- private endpoint by default;
- public ingress is not part of v1;
- bastion/session manager/private endpoint/overlay may supply reachability;
- node transport still authenticates both sides.

Persistent host:
- long-lived `harw gateway`;
- stable node identity;
- single-writer block/local volume for state;
- KEK outside the volume.

Client locator:
- machine-local host alias registry;
- explicit trust/pairing;
- endpoint is locator only.

The Cloudflare Worker provider gateway, if configured, remains downstream of
model-provider calls. It is unrelated to WS client identity/session control.

---

## 7. Enrollment / revocation contract

Normal NodeTransport remains default-deny for unknown peers.

Enrollment must be a separate explicitly enabled path with:
- server-authenticated channel;
- pairing code rate limit;
- code TTL/single use;
- approved device public key;
- host-fixed tenant and cap ceiling;
- audit event.

Revocation closes current authority:
- mark registry record revoked;
- refuse new handshake;
- close active WS connection(s);
- drop queued inputs from that connection/device;
- revoke SecurityHub contexts;
- audit.

A test that only proves "reconnect is refused" is insufficient.

---

## 8. Host alias contract

Proposed local state path:

`$HARW_STATE_DIR/hosts/<alias>.json`

Required semantics:
- aliases are local operator state, not repo/project state;
- record contains endpoint locator + expected authenticated host identity;
- changing trust/pin is an explicit trusted action;
- no model/chat/repo content auto-creates a host alias;
- no URL query parameter can auto-switch the target host.

---

## 9. Test matrix

| ID | Boundary | Required result |
|---|---|---|
| WS-01 | subprotocol | missing/wrong `harw.session.v1` refused |
| WS-02 | hello | pre-hello attach/submit denied |
| WS-03 | frame size | oversized inbound frame bounded without OOM |
| WS-04 | origin | browser-style Origin refused in v1 |
| WS-05 | ids | duplicate/unknown response id handled fail-closed |
| WS-06 | multiplex | two attached sessions on one socket both make progress |
| WS-07 | fairness | delta flood cannot starve an approval/response |
| ID-01 | local | forged uid/principal/tenant JSON has no effect |
| ID-02 | remote | `AuthenticatedPeer` is preserved across upgrade |
| ID-03 | tenant | cross-tenant list/attach/history/replay denied |
| ID-04 | caps | requested caps can narrow, never widen |
| ID-05 | actor | approval actor is server-derived |
| SES-01 | idempotency | duplicate `client_msg_id` -> one model call |
| SES-02 | CAS | stale `expect_head` -> `Stale` |
| SES-03 | single writer | two clients -> one session writer/model execution |
| SES-04 | detach | disconnect does not cancel running turn/job |
| APP-01 | race | two approval responses -> one durable winner |
| BP-01 | slow peer | turn loop continues; peer receives lag/resync |
| RP-01 | cursor | reconnect replays exact durable suffix |
| RP-02 | generation | old generation forces resync |
| RP-03 | restart | new epoch; durable data restored; live-only loss explicit |
| REV-01 | live revoke | active WS closes immediately |
| REV-02 | reconnect | revoked device cannot reconnect |
| LOC-01 | local surface | no default loopback TCP listener |
| ARC-01 | protocol deps | `harw-protocol` has no async/network transport dep |
| ARC-02 | rings | Tungstenite unreachable from J/D/T/Warden |

---

## 10. Work packages and dependencies

### W01 — contract
- W01-01: `harw-protocol/src/session_wire.rs`
- W01-02: `harw-protocol/src/session_port.rs`
- W01-03: methods/exports/fixtures
- W01-R: read-only compatibility review

### W02 — WS substrate
Depends on W01.
- W02-01: NodeTransport server upgrade support
- W02-02: NodeTransport client upgrade support
- W02-03: new `harw-session-ws` crate + codec/limits
- W02-04: fair mux/backpressure
- W02-05: dependency review + architecture gate
- W02-R: security/transport review

### W03 — host core
Depends on W01; transport-independent pieces may run parallel with W02.
- hosted record / replay / live ring
- fanout
- arbiter
- approvals
- limits
- driver
- host integration

### W04 — local usable slice
Depends on W02 + W03.
- local UDS listener
- identity bridge
- gateway composition
- remote client local connector
- `harw attach --socket`
- smoke: two clients, one session, one model turn

### W05 — own-cloud usable slice
Depends on W02 + W03.
- devices
- enrollment
- remote identity bridge
- node WS listener/client
- revocation
- host alias
- remote tenant/cap tests

### W06 — thin TUI
Depends on W04; remote use additionally on W05.
- attach root
- remote view
- reconnect indicator
- stale-submit conflict UI
- compact profile
- approval/presence flow

### W07 — lifecycle/storage/deploy
Depends on W04/W05.
- state/runtime dir enforcement
- drain/restart
- persistent host deployment
- private network docs
- recovery smoke

### W08 — hardening
Depends on usable slices.
- observability
- wait reason vocabulary
- M1/M5 review
- migration ledger
- full documentation

---

## 11. Verification rule

Every round freezes one integration SHA and runs centrally:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -q -p xtask -- gates
cargo deny check
make -C dod clippy test
actionlint
```

Parallel workers do not independently run workspace builds. Any commit after a
recorded gate SHA invalidates that gate result.

---

## 11a. Implementation status

| Package | Status | Where |
|---|---|---|
| W01 contract | landed (R1) | `harw-protocol/src/session_wire.rs`, `session_port.rs`, `methods.rs`, `tests/session_wire_compat.rs`. `SessionFrame` decodes unknown kinds to `Unknown` through a manual `Deserialize` (plain `#[serde(other)]` rejects adjacent `data`, as PL-65 RS1-01-T3 anticipated). |
| W02-03/04/05 WS substrate | landed (R1) | `harw-session-ws` (upgrade, codec, limits, dispatch, conn); arch gate `[[forbidden_crates]]` (ARC-01, ARC-02); dependency review entry for `tokio-tungstenite`. |
| W02-01/02 NodeTransport upgrade | open (R2) | — |
| W03 host core | landed (R1) | `harw-session-host` (identity, record, replay, live_ring, fanout, arbiter, approvals, driver port, host). The production `TurnDriver` over `harw-core` is W04. |
| W04–W08 | open | — |

Test matrix coverage after R1: WS-01..07, ID-03..05, SES-01..04, APP-01,
BP-01, RP-01..03, REV-01 (host + transport side), ARC-01, ARC-02. Open:
ID-01 (needs the UDS listener), ID-02 and REV-02 over the node transport,
LOC-01.

## 12. Definition of Done for W00

W00 is complete when:
- this contract and its parent design are reviewed against current `dev`;
- PL-65's retained semantic invariants and changed transport decision are both
  explicit;
- the local UDS and remote NodeTransport trust paths cannot be confused;
- crate/ring ownership is fixed enough for file-scoped workers;
- the test matrix covers identity, tenancy, approval, replay, backpressure,
  revocation and restart;
- open operator decisions remain listed rather than being silently assumed.

No claim is made that W01+ has landed.
