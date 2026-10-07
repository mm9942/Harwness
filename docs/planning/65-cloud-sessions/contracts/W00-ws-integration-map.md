---
id: W00-WS-INTEGRATION-MAP
title: "W00 WebSocket Control Plane: Integration Map (skeleton, relations, scope ownership)"
status: proposed
date: 2026-10-01
parent:
  - ./W00-websocket-control-plane.md
---

# W00 integration map

Orchestrator-owned and frozen for Wave A. It records the relations the ten
scopes (S01-S10) must not drift from, the interface stubs the skeleton
commit added, and which scope owns which files. Workers edit only the files
listed for their scope. Shared registration (workspace members, arch-policy,
`mod` lines, Cargo dependencies, the CLI command enum) is done by the
skeleton and must not be touched by workers; a needed change goes to the
orchestrator.

Branches: `ws/skeleton-dev` (from `origin/dev`) carries S01-S04 and S06-S09.
`ws/skeleton-stack` (stacked on the #91/#92 branches) carries **S05 and S10**,
because `harw-session-daemon` and `harw-reverse-proxy` only exist there. Both
merge into `ws/integration` by the orchestrator; nothing goes to `dev`
without per-PR approval.

## 1. Layer and relation map

```text
TUI / `harw attach` --SessionPort--> harw-session-remote (client)           S06, S07, S09
   | connect: UDS (SO_PEERCRED) | NodeTransport (ML-DSA-65 pinned, TLS1.3 X25519MLKEM768)
   v
[edge: harw-reverse-proxy WS route policy] (remote/browser only, optional)   S10 (stack)
   v
harw-node-transport (HTTP/1 upgrade, AuthenticatedPeer kept)                 S02
harw-node-listener  (peer -> ClientIdentity, revocation)                     S08
   v
harw-session-ws (upgrade, hello, codec, limits, dispatch)  one WS : N sessions   (landed, R1)
   v
harw-session-host (single writer, replay, fanout, arbiter, approvals)
   |  ports: TurnDriver / ApprovalBackend / TranscriptSource
   |    TurnDriver        -> harw-session-driver (over harw-core)            S03
   |    ApprovalBackend   -> DurableApprovals (session-store)                S04
   |    TranscriptSource  -> DurableTranscripts (session-store)              S04
   v
harw-core (via bridge) + harw-session-store (durable)
harw-session-daemon (#91, UDS composition of the above)                      S05 (stack)
```

## 2. Relation table (frozen)

| # | Relation | Rule | Owner scope | Checked by |
|---|----------|------|-------------|------------|
| R1 | connection : session attachment | 1:N; attachment map is not authority, authorization is rechecked where revocation requires it | S01 (doc), host/ws landed | WS-06 |
| R2 | session : host | 1:1 owner host (node + `placement_generation`); no cross-node failover in v1 | S01 | RP-02, RP-03 |
| R3 | identity chain | transport credential (uid, node key, device) -> `ClientIdentity` -> principal/tier/tenant; never from the wire payload; PermissionTier is not key authorization | S02, S08, S05 | ID-01, ID-02, ID-05 |
| R4 | remote tenant | `ClientIdentity::validate` refuses a remote caller without tenant; tenant and cap ceiling are host-fixed per device; requested caps can narrow, never widen | S08 | ID-03, ID-04 |
| R5 | cursor `(generation, durable, live)` | the transcript is truth; replay is outside WS; stale generation forces resync; a bad cursor is answered with resync, never fabricated by the client | S06, S07 | RP-01, RP-02 |
| R6 | slow clients | lossy for live deltas only; the turn loop is never back-pressured; `Lagged{resume_from}` then reattach | S06 (client side), landed (host) | BP-01 |
| R7 | approvals | first writer wins; the backend is durable; actor is host-derived | S04, S03 | APP-01, ID-05 |
| R8 | upgrade ordering | NodeTransport TLS + mutual transcript auth -> `AuthenticatedPeer` -> device/tenant/cap resolution -> HTTP/1 upgrade -> session protocol; the peer is injected before the request reaches the service and is retained by the upgraded I/O | S02, S08 | ID-02 |
| R9 | revocation | closes live connections, drops queued inputs, revokes contexts, audits; "reconnect refused" alone is insufficient | S08 | REV-01, REV-02 |
| R10 | Origin | any `Origin` header is refused in v1 (browser control is not a v1 capability); the edge must not forward one | S10 (stack), landed (ws) | WS-04 |
| R11 | subprotocol | exactly `harw.session.v1`, pinned at the edge and in the transport | S10 (stack), landed (ws) | WS-01 |
| R12 | dependency rings | tungstenite is owned by `harw-session-ws` only; clients use its `WebSocketStream`/`Role` re-export; `harw-protocol` stays free of async and transport crates | S06, S08 | ARC-01, ARC-02 |
| R13 | host alias | machine-local `$HARW_STATE_DIR/hosts/<alias>.json`; endpoint is a locator only; trust change is an explicit action; nothing auto-creates an alias | S07 | W00 section 8 |

### Numbering cross-map

W00 work packages W01-W08 versus the hub H0-H9 numbering versus this
program's W1-W6 slices. The hub and local-slice columns are the planning
labels used in `../README.md` and the round plan; where one W00 package maps
to several, all are listed.

| W00 package | Hub | Slice (W1-W6) | Scopes |
|-------------|-----|---------------|--------|
| W01 contract | H0 | W1 | S01 |
| W02-01/02 NodeTransport upgrade | H3 | W2 | S02 |
| W02-03/04/05 WS substrate | H3 | W2 | landed (R1) |
| W03 host core | H4 | W3 | landed (R1); S03, S04 add production ports |
| W04 local usable slice | H5 | W4 | S03, S05, S06, S09 |
| W05 own-cloud usable slice | H6 | W5 | S02, S06, S07, S08 |
| W06 thin TUI | H7 | W6 | S09 |
| W07 lifecycle/storage/deploy | H8 | W6 | S05 (hardening) |
| W08 hardening | H9 | W6 | backlog (observability) |

S01 may refine this table inside `W00-websocket-control-plane.md`; the
numbering in this file is not authoritative for H-labels.

## 3. Interface stubs added by the skeleton

All bodies return a typed not-implemented error; no panics, `todo!`,
`unimplemented!` (workspace lints). Public signatures below are frozen:
workers fill bodies and may add items, not change these signatures. A needed
signature change goes to the orchestrator.

### harw-session-remote (new crate, modules `conn`, `port`, `reconnect`, `alias`, `lib`)

```rust
// lib.rs
pub enum RemoteError { NotImplemented(&'static str), Connect(String), Protocol(String), State(String) } // non_exhaustive
impl From<RemoteError> for PortError

// conn.rs (S06)
pub enum Endpoint { Unix(PathBuf), Node(NodeEndpoint) }
pub struct NodeEndpoint { pub addr: SocketAddr, pub expected_node: NodeId }
pub struct ConnectOptions { pub client_label: String, pub requested_caps: Option<ClientCaps>, pub limits: WsLimits }
impl ConnectOptions { pub fn new(client_label: impl Into<String>) -> Self }
pub struct RemoteConnection { /* private */ }
pub async fn connect_unix(path: PathBuf, options: ConnectOptions) -> Result<RemoteConnection, RemoteError>
pub async fn connect_node(endpoint: NodeEndpoint, local: &LocalNode, verifier: &dyn NodeVerifier,
                          options: ConnectOptions) -> Result<RemoteConnection, RemoteError>

// port.rs (S06)
pub struct RemotePort { /* private */ }
impl RemotePort { pub fn new(connection: RemoteConnection) -> Self }
impl SessionPort for RemotePort { /* all 14 methods */ }

// reconnect.rs (S07)
pub struct BackoffPolicy { pub initial: Duration, pub max: Duration, pub jitter_permille: u16 }
impl BackoffPolicy { pub fn delay(&self, attempt: u32, entropy: u64) -> Result<Duration, RemoteError> }
pub struct ResumeCursors { /* private */ }
impl ResumeCursors {
    pub fn new() -> Self;
    pub fn record(&mut self, session: &SessionId, cursor: Cursor) -> Result<(), RemoteError>;
    pub fn cursor(&self, session: &SessionId) -> Result<Option<Cursor>, RemoteError>;
}

// alias.rs (S07)
pub struct HostAlias { pub alias: String, pub endpoint: String, pub expected_node: Option<String> }
pub struct AliasStore { /* private */ }
impl AliasStore {
    pub fn new(state_dir: &Path) -> Self;
    pub fn get(&self, alias: &str) -> Result<Option<HostAlias>, RemoteError>;
    pub fn put(&self, record: &HostAlias) -> Result<(), RemoteError>;
    pub fn list(&self) -> Result<Vec<HostAlias>, RemoteError>;
    pub fn remove(&self, alias: &str) -> Result<bool, RemoteError>;
}
```

### harw-session-driver (new crate, modules `lib`, `bridge`)

```rust
pub enum DriverBridgeError { NotImplemented(&'static str), Runtime(String) } // non_exhaustive
impl From<DriverBridgeError> for harw_session_host::HostError
pub struct CoreDriverConfig { pub sessions_root: PathBuf }
impl CoreDriverConfig { pub fn new(sessions_root: PathBuf) -> Self }
pub struct CoreTurnDriver { /* private */ }
impl CoreTurnDriver { pub fn new(config: CoreDriverConfig) -> Result<Self, DriverBridgeError> }
impl harw_session_host::driver::TurnDriver for CoreTurnDriver { /* create_session, run_turn,
    resume_after_approval, apply_setting, model_name: existing trait, unchanged */ }
```

### harw-node-listener (new crate, modules `lib`, `identity`, `revoke`)

```rust
// lib.rs
pub enum ListenerError { NotImplemented(&'static str), UnknownPeer, Host(String), Io(String) } // non_exhaustive
pub struct NodeListener { /* private */ }
impl NodeListener {
    pub fn new(server: NodeTransportServer, host: Arc<SessionHost>,
               mapper: Arc<dyn IdentityMapper>, limits: WsLimits) -> Self;
    pub async fn serve<F: Future<Output = ()>>(&self, listener: TcpListener, shutdown: F)
        -> Result<(), ListenerError>;
}

// identity.rs: the listener identity-mapping trait
pub type MapFuture<'a> = Pin<Box<dyn Future<Output = Result<ClientIdentity, ListenerError>> + Send + 'a>>;
pub trait IdentityMapper: Send + Sync {
    fn map(&self, peer: &AuthenticatedPeer, connection: ConnectionId) -> MapFuture<'_>;
}
pub struct RegistryIdentityMapper { /* private */ }
impl RegistryIdentityMapper { pub fn new(state_dir: &Path) -> Self }

// revoke.rs
pub struct RevocationReport { pub connections_closed: usize, pub inputs_dropped: usize, pub contexts_revoked: usize }
pub trait RevocationSink: Send + Sync {
    fn revoke_device(&self, device: &DeviceId) -> Result<RevocationReport, ListenerError>;
}
pub struct HostRevoker { /* private */ }
impl HostRevoker { pub fn new(host: Arc<SessionHost>) -> Self }
```

### harw-session-host (additions: `durable_approvals`, `durable_replay`, `HostError::NotImplemented`)

```rust
pub struct DurableApprovals { /* private */ }
impl DurableApprovals { pub fn open(root: &Path) -> Result<Self, HostError> }
impl ApprovalBackend for DurableApprovals { pending, session_of, resolve }   // existing trait

pub struct DurableTranscripts { /* private */ }
impl DurableTranscripts { pub fn open(root: &Path) -> Result<Self, HostError> }
impl TranscriptSource for DurableTranscripts { read_from, head }             // existing trait

// error.rs
HostError::NotImplemented(&'static str)   // maps to PortError::Transport("host error")
```

The `ApprovalBackend`, `TranscriptSource` and `TurnDriver` traits are not
changed. `StoreTranscripts` in `replay.rs` stays as is.

### harw-node-transport (addition: `upgrade` module)

```rust
pub type UpgradedIo = TokioIo<hyper::upgrade::Upgraded>;
pub enum UpgradeError { NotImplemented(&'static str), MissingPeer, NotSwitched(u16), Http(hyper::Error) } // non_exhaustive
pub struct UpgradedServerIo { /* peer + io */ }
impl UpgradedServerIo { pub fn peer(&self) -> &AuthenticatedPeer; pub fn into_parts(self) -> (AuthenticatedPeer, UpgradedIo) }
pub struct UpgradedClientIo { pub peer: AuthenticatedPeer, pub response: Response<Incoming>, pub io: UpgradedIo }
pub fn peer_of<B>(request: &Request<B>) -> Result<AuthenticatedPeer, UpgradeError>
pub async fn accept_upgrade(request: Request<Incoming>) -> Result<UpgradedServerIo, UpgradeError>
impl NodeTransportServer {
    pub async fn serve_upgradable<S, B, F>(&self, listener: TcpListener, service: S, shutdown: F)
        -> Result<(), UpgradeError>   // same bounds as serve_with_shutdown
}
impl NodeTransportClient {
    pub async fn upgrade(self, request: Request<NodeBody>) -> Result<UpgradedClientIo, UpgradeError>
}
```

Existing `serve`, `serve_with_shutdown`, `accept`, `connect`, `send_request`
are unchanged (W00 §2.3 "Must not change").

### harw-session-ws (one additive re-export)

`pub use tokio_tungstenite::WebSocketStream;` and `pub use ...::Role;` so
client crates do not take a direct tungstenite dependency (R12).

### harw-tui (addition: `attach` module)

```rust
pub enum AttachError { NotImplemented(&'static str), Port(String) } // non_exhaustive
pub async fn run_attach(port: Arc<dyn SessionPort>, session: Option<SessionId>) -> Result<(), AttachError>
```

### harw-cli

`Command::Attach(AttachArgs)` (`harw attach [SESSION] [--socket PATH | --host ALIAS]`),
`cli/attach.rs` (arguments), `attach_cmd::run(&AttachArgs) -> Result<(), String>`
(returns a "not implemented" error). `harw-cli` depends on
`harw-session-remote`.

## 4. Scope to files ownership (each at most 5 files)

"Files" are the only paths a worker may create or edit. Everything else is
read-only for them. Test files named here are new unless noted.

| Scope | Branch | Files (owner edits only these) | Depends on |
|-------|--------|--------------------------------|------------|
| S01 contract delta + fixtures | `ws/S01` | `docs/planning/65-cloud-sessions/contracts/W00-websocket-control-plane.md`; `harw-protocol/src/session_wire.rs`; `harw-protocol/tests/session_wire_compat.rs` (existing); `harw-protocol/tests/fixtures/session_wire_golden.json` | none |
| S02 NodeTransport WS upgrade | `ws/S02` | `harw-node-transport/src/upgrade.rs`; `harw-node-transport/src/server.rs`; `harw-node-transport/src/client.rs`; `harw-node-transport/tests/upgrade_loopback.rs`; `harw-node-transport/README.md` | S01 |
| S03 production TurnDriver bridge | `ws/S03` | `harw-session-driver/src/bridge.rs`; `harw-session-driver/src/lib.rs`; `harw-session-driver/tests/bridge_turns.rs`; `harw-session-driver/tests/bridge_approvals.rs` | S01 |
| S04 durable ApprovalBackend + TranscriptSource | `ws/S04` | `harw-session-host/src/durable_approvals.rs`; `harw-session-host/src/durable_replay.rs`; `harw-session-host/tests/durable_adapters.rs`; `harw-session-store/src/approval.rs` (additive only, if the store needs a method) | S01 |
| S05 daemon hardening + composition | `ws/S05` on **`ws/skeleton-stack`** | `harw-session-daemon/**`, at most 5 files, fixed by the stacked skeleton's own addendum to this map (wires S03 `CoreTurnDriver` and S04 `Durable*` into #91) | S03, S04 |
| S06 client core | `ws/S06` | `harw-session-remote/src/conn.rs`; `harw-session-remote/src/port.rs`; `harw-session-remote/src/lib.rs`; `harw-session-remote/tests/client_loopback.rs`; `harw-session-remote/tests/client_negative.rs` | S01, S02 stub |
| S07 reconnect, cursors, alias store | `ws/S07` | `harw-session-remote/src/reconnect.rs`; `harw-session-remote/src/alias.rs`; `harw-session-remote/tests/reconnect.rs`; `harw-session-remote/tests/alias.rs` | S06 |
| S08 own-cloud node listener | `ws/S08` | `harw-node-listener/src/lib.rs`; `harw-node-listener/src/identity.rs`; `harw-node-listener/src/revoke.rs`; `harw-node-listener/tests/listener_identity.rs`; `harw-node-listener/tests/revocation.rs` | S02 |
| S09 TUI attach + `harw attach` | `ws/S09` | `harw-cli/src/attach_cmd.rs`; `harw-cli/src/cli/attach.rs`; `harw-tui/src/attach.rs` (skeleton stub, `mod` line registered); `harw-cli/src/cli/tests.rs` (existing, parse tests only) | S06 |
| S10 edge WS route policy | `ws/S10` on **`ws/skeleton-stack`** | `harw-reverse-proxy/**`, at most 5 files, fixed by the stacked skeleton (pure core, subprotocol pinned, Origin refusal, limits aligned to `WsLimits`) | S01 |

Notes:

- S06 and S07 share a crate but not a file. `lib.rs` re-exports are frozen by
  the skeleton; S06 may add items to `lib.rs`, S07 must not edit it.
- S09 has one `harw-tui` leaf (`attach.rs`, `run_attach(port, session)`);
  its `mod` line is already registered. Further TUI files are not allowed
  without an orchestrator registration commit.
- No worker edits a `Cargo.toml`, `Cargo.lock`, `xtask/arch-policy.toml`,
  `docs/architecture/dependency-review.md`, or any `lib.rs` `mod` line unless
  named above. A missing dependency is requested from the orchestrator.
- Join order (DAG): S01, then {S02, S03, S04, S10}, then {S05, S06, S08},
  then {S07, S09}.

## 5. Skeleton verification record

Run per new or touched crate, against `origin/dev` at `4e49a17`:

- `cargo fmt --all -- --check`
- `cargo clippy -p <crate> --all-targets -- -D warnings` for
  `harw-session-remote`, `harw-session-driver`, `harw-node-listener`,
  `harw-session-host`, `harw-node-transport`, `harw-session-ws`, `harw-cli`
- `cargo test -p <crate>` for the same set
- `cargo run -q -p xtask -- gates arch` (new packages classified layer A; no
  new internal edge violates `[rules]`; tungstenite stays unreachable from
  rings F, I, C, J, D)
- `cargo run -q -p xtask -- gates edges`, `gates privileges`

Known environment limits: `cargo deny check`, the `warden-*` gates,
`make -C dod clippy test` and `actionlint` are not run in the worker sandbox;
they belong to the central gate on the frozen integration SHA (W00 section
11). `cargo clippy` on `harw-cli` additionally needs
`-A clippy::doc_nested_refdefs`, because `harw-ops/src/bug_report.rs` already
fails that lint on `origin/dev` with the 1.98 toolchain (pre-existing, not
part of this skeleton).
