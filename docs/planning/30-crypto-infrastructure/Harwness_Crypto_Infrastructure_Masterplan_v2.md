# Harwness Infrastructure / Crypto / Control Plane Masterplan v2

> **Purpose:** upgrade the previous Harwness master plan with the current Harwness repository reality, the new CryptGuard service/Tower/Hyper design, the three-socket control model, and the decisions from the architecture discussion.
>
> **Harwness source baseline inspected:** `mm9942/Harwness` `main` at `d3e0b965696e7a631687a73618c934cda7ad3d2c`
>
> **CryptGuard source baseline inspected:** `mm9942/crypt_guard` `main` at `cb51196b0a45ccf584efdc8957cf24c691105c80`
>
> **Important:** this document is a planning contract, not permission to weaken existing compile-time security invariants. Existing DoD privilege boundaries, CryptGuard non-Clone secret/context invariants, Harwness authority narrowing, operation approval semantics, and durable secret compatibility remain constraints.

---

# 0. Executive summary

The previous plan had the right large-scale direction but is now too coarse in four places.

First, the original “one `control.sock` multiplexes Auth + Security + NetSec” design is superseded by the three-domain public IPC contract:

```text
/run/harw/control.sock
/run/harw/network.sock
/run/harw/secure.sock
```

The existing DoD privilege-boundary sockets remain separate and are **not** folded into those three:

```text
/run/harw-dod/sentinel.sock
/run/harw-dod/warden.sock
```

Second, the generic crypto-service/Tower/Hyper composition no longer belongs in Harwness. It belongs in CryptGuard itself. Harwness consumes the resulting service API. `harw-dod-encrypt` becomes the **Harw-specific composition/policy/protocol layer**, not a second generic KMS framework.

Third, Harwness already contains the correct hooks to integrate this cleanly:

- `harw-web` already uses Hyper 1.x, `hyper-util`, `http-body-util`, `bytes`, Tokio and Unix sockets.
- `harw-web` already performs `SO_PEERCRED`, body limits, read timeouts and operation-registry routing.
- `harw-operations` already provides one operation contract with `PermissionTier`, `ApprovalPolicy`, explicit Web method and surfaces.
- `harw-runtime::RuntimeServices` already centralizes service injection across entry points.
- `harw-runtime::AssemblyContributor` already exists specifically so security/network subsystems can contribute operations without being allowed to widen authority.

Therefore the Harw side must **reuse** these paths rather than creating parallel REST routers, parallel permission models or parallel runtime assembly.

Fourth, `harw-secrets` is already envelope-encryption-shaped but still owns long-term KEK provenance in-process and still performs payload AEAD directly using `aes-gcm-siv` / `chacha20poly1305`. The KMS transition should preserve its durable `SecretRecord` layout and migrate the long-term key boundary first: Harw keeps local payload encryption with ephemeral DEKs, while the Auth/Crypto service owns KEKs/private keys and only wraps/unwraps DEKs by `KeyRef`.

The target architecture is:

```text
                           ┌─────────────────────────┐
                           │       Harwness UI       │
                           │ Web / TUI / CLI / MCP   │
                           └────────────┬────────────┘
                                        │
                               harw-operations
                                        │
                         ┌──────────────┴──────────────┐
                         │   RuntimeAssembly/Services  │
                         └──────┬────────┬────────┬────┘
                                │        │        │
                     control    │        │        │ secure
                                │        │        │
                 /run/harw/control.sock  │  /run/harw/secure.sock
                                         │
                              /run/harw/network.sock
                                         │
                  ┌──────────────────────┼──────────────────────┐
                  │                      │                      │
          Security/Control Hub        NetSec              Auth/Crypto Hub
                  │                      │                      │
                  │                      │              harw-dod-encrypt
                  │                      │                      │
                  │                      │              crypt_guard service
                  │                      │                      │
                  └──────────────┬───────┴───────────────┬─────┘
                                 │                       │
                             DoD correlation         key providers
                                 │                       │
                 ┌───────────────┴───────────────┐       │
                 │                               │       │
      /run/harw-dod/sentinel.sock   /run/harw-dod/warden.sock
                 │                               │
            push-only probes               tiny privileged TCB
```

The control questions remain deliberately separated:

```text
control.sock  = WHAT is requested / orchestrated?
network.sock  = WHERE / through which network may it flow?
secure.sock   = WHO is this / which key operation is permitted?
Security/DoD  = SHOULD the action happen?
```

---

# 1. Repository reality that changes the old plan

## 1.1 `harw-web` is already the control transport substrate

Current `harw-web/Cargo.toml` already depends on:

```toml
hyper = { version = "1", features = ["server", "http1"] }
hyper-util = { version = "0.1", features = ["server", "http1", "tokio"] }
http-body-util = { version = "0.1", features = ["channel"] }
bytes = "1"
tokio = { version = "1", features = ["rt", "net", "sync", "macros", "io-util", "time"] }
rustix = { workspace = true, features = ["net"] }
```

`harw-web/src/server.rs` already provides:

- Hyper HTTP/1 over `tokio::net::UnixStream`
- `SO_PEERCRED`
- `http_body_util::Limited`
- 64 KiB body ceiling
- request body timeout
- header timeout
- stale-socket protection
- SSE event streaming
- no direct route execution outside `harw-operations`

Therefore:

> Do not build a second local HTTP stack for `control.sock`.

The first implementation should evolve `harw-web` into the local Control API transport rather than replacing it.

Initial path change:

```text
old illustrative path:
/run/harw/web.sock

target:
/run/harw/control.sock
```

The crate may remain named `harw-web` initially. Renaming crates during the infrastructure migration would create churn with no security benefit.

## 1.2 `harw-operations` is already the correct authority surface

The current Web adapter does not invent operations. Routes are generated from `OperationMeta::surfaces`.

Existing metadata already includes:

```rust
OperationMeta {
    permission: PermissionTier,
    surfaces: Vec<Surface>,
    approval: ApprovalPolicy,
    ...
}
```

and Web routes carry explicit methods.

Therefore infrastructure controls should appear as operations such as:

```text
infra.auth.keys.list
infra.auth.keys.rotate
infra.auth.devices.list
infra.network.nodes.list
infra.network.nodes.drain
infra.network.routes.list
infra.network.routes.update
infra.security.incidents.list
infra.security.policy.inspect
infra.security.containment.propose
```

with the same authority/approval path as the TUI, CLI and Web.

There must be no second `if admin { ... }` HTTP authorization system.

## 1.3 `RuntimeServices` is the composition root we were looking for

`harw-runtime/src/services.rs` already exists to eliminate divergent service assembly.

The new clients belong there.

Conceptually:

```rust
pub struct RuntimeServicesParts {
    // existing fields ...

    pub auth_hub: Option<Arc<AuthHubClient>>,
    pub network_control: Option<Arc<NetworkControlClient>>,
    pub security_hub: Option<Arc<SecurityHubClient>>,
}
```

Then `service_map()` exposes them only on declared surfaces.

The exact visibility matrix must be explicit and tested.

Example first-pass policy:

```text
                         Slash  ModelTool  Web  Job
AuthHubClient              Y       N        Y    N
NetworkControlClient       Y       N        Y    N
SecurityHubClient          Y       N        Y    Y(read-only subset)
```

The model must not automatically inherit infrastructure-administration clients simply because the human-facing runtime has them.

Operations may themselves mediate narrow model-safe capabilities later.

## 1.4 `AssemblyContributor` is the correct registration hook

Current `AssemblyContributor` is intentionally unable to mutate the sandbox, permission ceiling or approval mode.

That is almost perfect for infrastructure integration.

Add, for example:

```rust
pub struct InfrastructureContributor {
    auth: Option<Arc<AuthHubClient>>,
    network: Option<Arc<NetworkControlClient>>,
    security: Option<Arc<SecurityHubClient>>,
}
```

It may:

- register operations,
- register read-only tools,
- add lifecycle hooks,
- inspect the current network scope,

but it must not grant new rights.

This preserves the existing monotone-authority design.

---

# 2. Revised crate architecture

The CryptGuard side is handled by the separate CryptGuard plan. Harwness should consume it.

Recommended new Harwness-side crates:

```text
dod/crates/harw-dod-encrypt/
    Harw crypto profiles
    Harw key-purpose vocabulary
    secure-frame / binding protocol
    Harw <-> crypt_guard service mapping
    NO generic Hyper framework reimplementation

harw-auth-hub/
    independent daemon + library
    identity / device / node / service registration
    key lifecycle metadata
    secure.sock owner
    KMS provider wiring

harw-netsec/
    independent daemon + library
    node topology
    zones
    listeners/routes
    Pingora/remote transport integration
    network.sock owner

harw-security-hub/
    independent daemon + library
    capabilities/policy/approvals/security context
    DoD correlation
    control-plane security operations

harw-infra-client/
    transport-neutral typed clients
    AuthHubClient
    NetworkControlClient
    SecurityHubClient
```

Optional later:

```text
harw-control-gateway/
```

only if the control-plane broker genuinely needs to be detached from the Harw runtime.

Do not create it prematurely.

---

# 3. `harw-dod-encrypt`: revised responsibility after CryptGuard service work

The earlier plan put too much generic KMS machinery here.

After the CryptGuard workspace change, `harw-dod-encrypt` should contain only Harw-specific semantics.

## 3.1 It owns

```text
HarwKeyPurpose
HarwCryptoProfile
HarwKeyRef namespace conventions
Harw secure message binding
node/service/device identity bindings
Harw-specific domain separation
SecurityContext binding
replay/idempotency metadata
mapping Harw operations -> crypt_guard service calls
```

## 3.2 It does not own

```text
KEM implementation
AEAD implementation
signature implementation
generic KeyProvider
generic CryptoService
generic Tower Service
generic Hyper adapter
generic HTTP body codec
generic secret-memory primitive
```

Those belong to CryptGuard.

## 3.3 It must not be re-exported by the `harw-dod` facade

The current DoD charter explicitly treats the facade as a reduction surface and excludes infrastructure/enforcement paths.

Keep that rule.

`harw-dod-encrypt` can live in the DoD workspace for ownership/security reasons without becoming reachable from:

```rust
use harw_dod::*;
```

Add a compile-fail gate proving that the facade does not expose it.

---

# 4. Harw key-purpose model

Harw needs semantic key purposes over generic CryptGuard `KeyRef`s.

Example:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HarwKeyPurpose {
    NodeIdentity,
    ServiceIdentity,
    DeviceIdentity,
    UserIdentity,

    SecretsKek,
    ChannelBinding,
    RequestAuthentication,

    AuditCheckpoint,
    ArtifactSigning,
    Pseudonymization,
}
```

The `KeyRef` itself should remain an opaque service handle.

Example Harw wrapper:

```rust
pub struct HarwKeyRef {
    inner: crypt_guard::service::KeyRef,
    purpose: HarwKeyPurpose,
}
```

Do not provide:

```rust
fn private_bytes(...)
```

Normal operations are:

```text
public_key
sign
verify
encrypt
decrypt
wrap
unwrap
rotate
disable
destroy
```

Key purpose is checked before dispatch.

A `NodeIdentity` key must not become an arbitrary document-signing oracle.

---

# 5. Key usage policy: prevent the KMS from becoming a signing oracle

A KMS with `sign(key_ref, arbitrary_bytes)` is structurally dangerous for high-value identity keys.

Harw-specific sensitive key operations should bind a structured purpose.

Example:

```rust
pub struct SignRequest {
    pub key: HarwKeyRef,
    pub purpose: SignPurpose,
    pub audience: Audience,
    pub digest: [u8; 32],
    pub context: SignContext,
}
```

Possible purposes:

```rust
pub enum SignPurpose {
    NodeHandshake,
    ServiceHandshake,
    ArtifactManifest,
    AuditCheckpoint,
    SecureFrame,
}
```

The Harw layer computes canonical domain-separated transcripts.

Example:

```text
"harw:node-handshake:v1" ||
node_id ||
challenge ||
peer_id ||
issued_at
```

The key provider never receives an unclassified “please sign this arbitrary blob” request for system identity keys.

---

# 6. Three public local sockets

## 6.1 Control

```text
/run/harw/control.sock
```

Transport:

```text
AF_UNIX / SOCK_STREAM
Hyper HTTP/1
SO_PEERCRED
harw-operations
SSE event stream
```

This is deliberately aligned with the existing `harw-web` implementation.

Responsibilities:

```text
sessions
agents
jobs
plans
artifacts
approvals
configuration
fleet summaries
security UI operations
auth/network administration operations exposed through typed clients
```

Control operations do not directly touch private key material.

## 6.2 Network

```text
/run/harw/network.sock
```

Recommended initial transport:

```text
AF_UNIX / SOCK_STREAM
Hyper HTTP/1
SO_PEERCRED
Tower service
```

Responsibilities:

```text
node registration state
node heartbeat state
routes
listeners
zones
upstreams
drain/undrain
proxy configuration
fleet topology
remote transport status
```

Important distinction:

> `network.sock` is the **local control API of the network daemon**.  
> It is not the actual transport between hosts.

Actual remote node traffic may use:

```text
TCP/TLS
HTTP/2
QUIC later
Pingora data plane
CryptGuard-authenticated Harw frames
```

## 6.3 Secure

```text
/run/harw/secure.sock
```

Recommended initial transport:

```text
AF_UNIX / SOCK_STREAM
Hyper HTTP/1
SO_PEERCRED
application-level encrypted/authenticated Harw secure frames
CryptGuard service underneath
```

Responsibilities:

```text
identity authentication
device/node enrollment
key lifecycle
public-key lookup
sign/verify
wrap/unwrap
encrypt/decrypt where policy permits
channel binding
service identity
security-context minting/validation
```

Private key material never leaves the daemon.

---

# 7. Why Hyper on the three public sockets and SeqPacket on DoD internals

This is an intentional split.

Public infrastructure sockets are management APIs. Hyper gives:

- request/response semantics,
- body limits,
- existing Harwness implementation reuse,
- Tower integration,
- remote transport reuse,
- future Pingora integration,
- standard observability.

Private DoD sockets serve a different purpose.

Keep:

```text
sentinel.sock -> SOCK_SEQPACKET, push-only probes
warden.sock   -> SOCK_SEQPACKET, systemd activation
```

because message boundaries and tiny privilege-boundary contracts are desirable there.

Do not “standardize” these away merely for aesthetic symmetry.

---

# 8. Secure frame contract

`secure.sock` is locally protected by Unix permissions and `SO_PEERCRED`, but the Harw secure protocol should additionally authenticate and encrypt sensitive request bodies.

First version:

```text
HarwSecureFrameV1
    header
    encrypted payload
    sender authentication
```

Example shape:

```rust
pub struct SecureFrameHeader {
    pub version: u16,
    pub profile: CryptoProfileId,

    pub request_id: RequestId,

    pub sender_key: HarwKeyRef,
    pub recipient_key: HarwKeyRef,

    pub service: ServiceId,
    pub operation: OperationId,

    pub issued_at: UnixMillis,
    pub ttl_ms: u32,
    pub nonce: [u8; 16],
}
```

The encrypted payload contains the typed secure request.

AAD binds the routing metadata:

```text
version
profile
request_id
sender key id
recipient key id
service
operation
issued_at
ttl
nonce
```

The identity signature binds:

```text
"harw:secure-frame:v1" || canonical_header || encrypted_frame_bytes
```

Do not let a client claim its own authenticated principal in a JSON field.

The receiver derives caller identity from:

```text
SO_PEERCRED
+
verified cryptographic identity
+
AuthHub registration state
```

---

# 9. Secure request processing order

Fail closed, and do not consume replay state before authenticity is established.

Suggested order:

```text
1. HTTP method/path/body bound
2. frame version / maximum sizes
3. sender key reference lookup
4. sender authentication / signature verification
5. issued_at + TTL validation
6. decrypt/open
7. typed payload decode
8. identity / tenant / device binding
9. operation policy
10. replay/idempotency ledger
11. execute
12. audit
```

For mutations, distinguish:

```text
crypto nonce
```

from:

```text
request_id / idempotency key
```

A network timeout after `key.rotate` cannot tell the caller whether the mutation committed.

---

# 10. Cryptographic clone invariant on Harw side

The CryptGuard plan defines the core rule:

> Cryptographic ownership rules stay cryptographic; network cloning stays networking.

Harwness must preserve that rule.

Harw-side types:

```text
HarwSecureClientHandle     Clone
NetworkControlClient       Clone
ControlClient              Clone

SecureRequest              !Clone if secret-bearing
SecretPayload              !Clone
KeyLease                   !Clone
session crypto context     !Clone
```

The cloneable client wraps a cloneable Tower/network handle, not crypto state.

Never solve a Hyper/Tower `Clone` requirement with:

```text
Arc<Mutex<PrivateKey>>
Arc<Mutex<SenderContext>>
```

The existing CryptGuard stateful contexts are intentionally non-Clone to prevent nonce-sequence duplication.

---

# 11. Harw runtime integration

## 11.1 New service bundle

Prefer one explicit infrastructure bundle instead of three unrelated loose values once the API stabilizes:

```rust
pub struct InfrastructureClients {
    pub auth: Arc<AuthHubClient>,
    pub network: Arc<NetworkControlClient>,
    pub security: Arc<SecurityHubClient>,
}
```

But do not require all three to exist for every runtime profile.

A builder can hold:

```rust
pub struct InfrastructureAvailability {
    pub auth: Option<Arc<AuthHubClient>>,
    pub network: Option<Arc<NetworkControlClient>>,
    pub security: Option<Arc<SecurityHubClient>>,
}
```

and assemble only valid profiles.

## 11.2 `RuntimeServicesParts`

Add clients at the composition root.

No operation may open `/run/harw/secure.sock` directly.

Bad:

```rust
let stream = UnixStream::connect("/run/harw/secure.sock").await?;
```

inside an arbitrary operation.

Good:

```rust
let auth = ctx
    .service::<Arc<AuthHubClient>>()
    .ok_or(OpError::NotAvailable)?;

auth.keys().rotate(...).await?;
```

This makes transport availability a declared runtime service.

## 11.3 Contributor

Add:

```rust
pub struct InfrastructureContributor;
```

which registers operations if the associated service is present.

It cannot alter the sandbox/authority ceiling because the existing `AssemblyContributor` API does not expose those fields mutably.

Keep that invariant.

---

# 12. Operation examples

## 12.1 List keys

```rust
#[operation(
    name = "infra.auth.keys.list",
    domain = "catalog_config",
    permission = "maintainer",
    web = "GET /api/infra/auth/keys"
)]
pub struct ListKeysOperation;
```

Implementation:

```rust
async fn run(
    &self,
    ctx: &OpContext,
    _input: OpInput,
) -> Result<OpOutput, OpError> {
    let client = ctx
        .service::<Arc<AuthHubClient>>()
        .ok_or(OpError::NotAvailable)?;

    let keys = client.list_public_metadata().await
        .map_err(map_auth_error)?;

    Ok(OpOutput::structured(
        render_key_summary(&keys),
        serde_json::to_value(keys).map_err(map_encode_error)?,
    ))
}
```

No private material is representable in the returned DTO.

## 12.2 Rotate key

```rust
#[operation(
    name = "infra.auth.keys.rotate",
    domain = "catalog_config",
    permission = "owner",
    approval = "always",
    web = "POST /api/infra/auth/keys/rotate"
)]
pub struct RotateKeyOperation;
```

Payload:

```rust
pub struct RotateKeyArgs {
    pub key: KeyId,
    pub expected_generation: KeyGeneration,
    pub idempotency_key: RequestId,
}
```

The operation goes through the same approval machinery as any other Harw operation.

## 12.3 Drain node

```text
operation:
infra.network.node.drain

permission:
Maintainer

approval:
RequireForEffect or Always, depending final effect metadata

request:
NodeId + reason + expected topology generation

backend:
NetworkControlClient
```

The network daemon performs the route/topology mutation.

The Harw runtime never rewrites Pingora config directly.

---

# 13. Identity model upgrade

Current `harw_types::Principal` is already valuable because:

- it cannot be deserialized,
- trusted ingress is explicit,
- child principals only narrow,
- model children cannot become Owner.

Do not throw that away.

However, it is not yet a full infrastructure identity record. It currently has:

```text
kind
id
surface
tier
```

and `PrincipalKind` currently covers:

```text
Human
Model
Operation
Channel
```

The AuthHub needs more:

```text
tenant
workspace
device
node/service identity
authentication strength
channel binding
key identity
session / expiry
```

Do not stuff all of that immediately into `Principal`.

Introduce a separate trusted type:

```rust
pub struct SecurityContext {
    principal: Principal,
    tenant_id: TenantId,
    workspace_id: Option<WorkspaceId>,

    device_id: Option<DeviceId>,
    node_id: Option<NodeId>,

    auth_strength: AuthStrength,
    identity_key: Option<KeyId>,

    issued_at: Timestamp,
    expires_at: Timestamp,
}
```

No `Deserialize`.

Construction only in trusted adapters after AuthHub verification.

Later, if the runtime contract proves it needs `Service`/`Node` as first-class `PrincipalKind`s, that can be an explicit API change rather than being smuggled into the KMS migration.

---

# 14. Upgrade `harw-web` authentication without creating a second authority system

Today:

```text
SO_PEERCRED
    ->
PeerAuthorizer
    ->
PermissionTier
```

That is correct for a local single-user Unix socket but insufficient for multiuser/device/tenant identity.

Target:

```text
SO_PEERCRED
    ->
LocalPeerIdentityResolver
    ->
AuthHub
    ->
SecurityContext
    ->
Principal + Tenant + Workspace + Tier
    ->
existing route decision / operation execution
```

The HTTP body cannot provide:

```text
tenant_id
principal_id
tier
```

as trusted values.

For remote browser access:

```text
Browser
   ->
remote gateway
   ->
AuthHub authentication
   ->
trusted SecurityContext
   ->
Control API
```

External headers such as:

```text
x-harw-principal
x-harw-tenant
x-harw-tier
```

must never be accepted directly from an untrusted client.

If a proxy forwards identity context, it must be cryptographically bound and the external copy of those headers must be stripped.

---

# 15. Multi-user / tenant / RLS direction

Tenant isolation is an orthogonal axis to `PermissionTier`.

`Owner` does not mean “owner of every tenant”.

Effective authorization is roughly:

```text
identity membership
∩ tenant/workspace scope
∩ PermissionTier / capability
∩ runtime authority
∩ ingress-zone ceiling
∩ operation policy
∩ approval state
```

Harw already has `TenantId` and `WorkspaceId` types.

Add store/query APIs that receive trusted scope from `SecurityContext`, never from the request body.

For future SQL-backed state, set transaction context server-side before queries.

For filesystem stores, use tenant/workspace-rooted repositories rather than trusting caller-supplied paths.

---

# 16. `harw-secrets` KMS migration

This is one of the strongest places to consume the new infrastructure.

## 16.1 Current state

Current `harw-secrets`:

- pins `crypt_guard = "=3.0.2"`,
- uses v3 PQ HPKE to wrap the per-secret DEK,
- performs local payload encryption,
- stores `KekMaterial` with:
  - public recipient bytes,
  - a secret root HPKE seed,
- supports key-file/keyring/env provenance,
- preserves a durable V1/V2 record distinction.

This is already very close to envelope encryption in a KMS architecture.

## 16.2 Target state

The Harw process should stop owning the long-lived KEK seed.

Target record flow:

```text
secret plaintext
      |
random DEK
      |
local AEAD
      |
ciphertext --------------------------+
                                      |
DEK                                   |
 |                                    |
 | secure KMS request                 |
 v                                    |
Auth/Crypto Hub                       |
 |                                    |
wrap under KeyRef / generation        |
 v                                    |
wrapped DEK --------------------------+
      |
persist SecretRecord
```

Opening:

```text
SecretRecord
   |
wrapped DEK
   |
Auth/Crypto Hub unwrap
   |
ephemeral DEK in SecretBytes
   |
local payload decrypt
   |
secret plaintext
```

The KMS never needs the full provider token / secret payload for this mode.

The Harw process still sees the short-lived DEK, but no long-term KEK/private key.

## 16.3 Migration without re-encrypting payload

Because V2 separates payload encryption from KEK wrapping:

```text
old wrapped DEK
  -> old KEK open once
  -> DEK
  -> new AuthHub KMS wrap
  -> new wrapped DEK
```

The payload `ciphertext` and `nonce` remain unchanged.

This is a very useful migration property and should be preserved.

## 16.4 New format

Add an explicit durable discriminator rather than silently changing V2 semantics.

Example:

```rust
pub enum SecretEnvelopeFormat {
    LegacyDirectHpke,
    DekWrappedV2,
    KmsWrappedV3,
}
```

`KmsWrappedV3` additionally stores:

```text
key_id
key_generation
crypto_profile_id
wrapped_dek
```

Do not delete V1/V2 readers during the migration.

---

# 17. Fix current CryptGuard documentation drift before migration

Current Harwness source is mixed:

- `harw-secrets/Cargo.toml` correctly pins `crypt_guard = "=3.0.2"`.
- several comments/docs still state `3.0.1`.
- several comments still claim deterministic recipient derivation is hybrid-only.
- current CryptGuard 3.0.2 now has `derive_recipient_key_pair(kem, seed)` and tests provenance reconstruction more broadly.

Therefore P0 work before KMS migration:

```text
1. establish exact 3.0.2 behavior with Harwness KATs
2. update stale 3.0.1 comments
3. update hybrid-only claims where no longer true
4. do NOT reinterpret existing persisted records
5. keep exact version pin until compatibility is deliberately re-blessed
```

This is documentation/contract repair, not a forced secret-format migration.

---

# 18. Network daemon model

`harw-netsec` owns topology and network control.

It must not become an identity authority.

Example node record:

```rust
pub struct NodeRecord {
    pub node_id: NodeId,

    pub addresses: Vec<NodeAddress>,
    pub labels: BTreeMap<String, String>,

    pub zone: TrustZone,
    pub health: NodeHealth,

    pub capabilities: NodeCapabilities,
    pub resources: ResourceSnapshot,

    pub last_seen: Timestamp,

    // reference, not private material
    pub identity_key: KeyId,
}
```

AuthHub answers:

```text
Is this cryptographically Node X?
```

NetSec answers:

```text
Where is Node X now?
Through which route/zone can it be reached?
Should it be drained?
```

Do not infer identity from IP/MAC.

---

# 19. Remote node transport

The local `network.sock` controls NetSec.

Inter-node communication is separate.

Suggested remote chain:

```text
Node A
  |
Harw secure node frame
  |
Hyper/Tower
  |
Pingora
  |
route / zone / upstream decision
  |
Node B
```

Authentication:

```text
long-term node identity
+
challenge
+
CryptGuard key establishment/signature
```

Then optionally a long-lived session:

```text
PQC/hybrid establishment
    ->
exporter/session keys
    ->
directional AEAD traffic keys
    ->
sequence-numbered records
```

Do not sign every high-rate data-plane frame with ML-DSA.

Use expensive asymmetric primitives to establish/authenticate the channel, then symmetric keys for traffic.

Initial management RPC may remain one-shot until benchmarks justify session state.

---

# 20. DoD integration

DoD remains structurally separate.

Local flow stays:

```text
privileged probes
    ->
sentinel.sock
    ->
Sentinel
    ->
rules / triage
```

Enforcement stays:

```text
authorized escalation
    ->
warden.sock
    ->
Warden
```

Do not add CryptGuard, Hyper or generic KMS dependencies to:

```text
harw-warden
harw-probe-bpf
harw-probe-fs
sensor crates
```

The Warden's tiny keyed proof path stays tiny.

New crypto is used only when security information leaves that local boundary or when a central SecurityHub needs authenticated host identity.

Remote event example:

```text
local Sentinel finding
    |
SecurityHub uplink
    |
harw-dod-encrypt frame
    |
authenticated/encrypted remote transport
    |
central SecurityHub
```

---

# 21. Dependency-budget gates

Extend the existing privilege/dependency CI idea.

Hard forbidden edges:

```text
harw-warden            -> crypt_guard
harw-probe-bpf         -> crypt_guard
harw-probe-fs          -> crypt_guard
harw-dod-thermal       -> crypt_guard
harw-dod-cpu           -> crypt_guard
... normal sensors     -> crypt_guard
```

Allowed:

```text
harw-auth-hub          -> crypt_guard service/hyper
harw-dod-encrypt       -> crypt_guard service
harw-secrets           -> crypt_guard core or infra client during migration
harw-netsec            -> infra client / secure identity client
```

Also gate against accidental Hyper ingress into sensor/warden TCBs.

---

# 22. Packaging and systemd: collapse the two current sources of truth

The current repository has both:

```text
deploy/systemd/*
dod/packaging/systemd/*
```

and the files already differ materially in path/user/group/runtime behavior.

That is precisely the drift we wanted to eliminate.

## 22.1 New rule

Each daemon has exactly one canonical unit/template source.

Recommended approach:

```rust
pub struct EmbeddedDeploymentAsset {
    pub name: &'static str,
    pub kind: AssetKind,
    pub contents: &'static str,
}
```

Each daemon or a daemon-owned deployment crate embeds:

```rust
const SERVICE: &str = include_str!("../packaging/harw-auth-hub.service");
const SOCKET: &str  = include_str!("../packaging/harw-auth-hub.socket");
```

The installer renders/install these assets.

Tests inspect the same embedded text that production installation uses.

No second manually-maintained copy under another tree.

## 22.2 New units

Eventually:

```text
harw-control.socket
harw-control.service

harw-netsec.socket
harw-netsec.service

harw-auth-hub.socket
harw-auth-hub.service

harw-security-hub.service
```

DoD keeps its existing units separately.

## 22.3 Runtime directories

Public Harw infrastructure:

```text
/run/harw/
    control.sock
    network.sock
    secure.sock
```

DoD internal:

```text
/run/harw-dod/
    sentinel.sock
    warden.sock
```

Persistent state remains out of `$HOME`:

```text
/etc/harw-*/        configuration
/var/lib/harw-*/    persistent state
/var/log/harw-*/    audit/log state
/run/harw*/         runtime IPC only
```

No production fallback to project-relative or `~/.harw` sockets.

---

# 23. Systemd socket activation example for AuthHub

Conceptual unit:

```ini
[Unit]
Description=Harw Auth/Crypto Hub socket

[Socket]
ListenStream=/run/harw/secure.sock
SocketMode=0660
SocketUser=harw-auth
SocketGroup=harw-secure
DirectoryMode=0750
Service=harw-auth-hub.service
RemoveOnStop=yes

[Install]
WantedBy=sockets.target
```

Service:

```ini
[Service]
Type=simple
User=harw-auth
Group=harw-auth
ExecStart=/usr/libexec/harw-auth-hub --systemd-socket
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
RestrictAddressFamilies=AF_UNIX
```

If remote transport is enabled, it must be an explicit different deployment profile because `RestrictAddressFamilies=AF_UNIX` would intentionally forbid it.

Do not silently widen the local KMS service because someone turned on a config flag.

---

# 24. Local client example

```rust
#[derive(Clone)]
pub struct AuthHubClient {
    inner: SecureNetworkHandle,
}

impl AuthHubClient {
    pub async fn rotate_key(
        &self,
        request: RotateKeyRequest,
    ) -> Result<RotateKeyResult, AuthHubError> {
        self.inner.call(request).await
    }
}
```

The client is Clone because it owns only a network/service handle.

A secret-bearing request itself is moved:

```rust
let result = auth_client
    .wrap_key(WrapKeyRequest {
        key,
        plaintext_key: dek, // moved SecretBytes
        idempotency_key,
    })
    .await?;
```

No `dek.clone()`.

---

# 25. Security context propagation

Do not make every request call all three daemons serially.

Instead mint a short-lived trusted context after authentication.

Example:

```rust
pub struct SecurityContext {
    context_id: SecurityContextId,

    principal: Principal,
    tenant: TenantId,
    workspace: Option<WorkspaceId>,

    device: Option<DeviceId>,
    node: Option<NodeId>,

    ingress_zone: TrustZone,
    auth_strength: AuthStrength,

    issued_at: Timestamp,
    expires_at: Timestamp,
}
```

Local processes may carry the typed object in memory.

Across process/network boundaries, send only an authenticated representation or a short-lived context reference.

Never let the client construct one.

---

# 26. End-to-end example A: local Web rotates a key

```text
Browser UI
  |
local Web frontend / control client
  |
/run/harw/control.sock
  |
SO_PEERCRED
  |
AuthHub-backed peer resolution
  |
SecurityContext
  |
harw-operations:
infra.auth.keys.rotate
  |
PermissionTier + approval
  |
AuthHubClient
  |
/run/harw/secure.sock
  |
Auth/Crypto Hub
  |
KeyRef policy
  |
crypt_guard service
  |
rotate
  |
audit event
```

No private key bytes enter Web, `harw-web`, the operation registry or the browser.

---

# 27. End-to-end example B: Harw resolves a provider secret

```text
Provider needs OpenAI credential
  |
SecretRef::Secrets(id)
  |
harw-secrets
  |
read KmsWrappedV3 record
  |
AuthHubClient.unwrap_key(KeyRef, wrapped_dek)
  |
ephemeral DEK
  |
local payload decrypt
  |
SecretBox plaintext
  |
provider request construction
```

Long-term KEK remains in Auth/Crypto Hub.

---

# 28. End-to-end example C: node joins fleet

```text
new node
  |
presents cryptographic node identity
  |
AuthHub verifies/enrolls
  |
NodePrincipal established
  |
NetSec registers:
address / zone / route / resources
  |
SecurityHub evaluates policy
  |
control plane exposes node
```

The network daemon never says:

```text
"10.0.0.7 therefore trusted node harw-07"
```

Identity and topology remain separate.

---

# 29. End-to-end example D: DoD incident on remote node

```text
harw-node-03 probe
   ->
local sentinel
   ->
finding
   ->
local deterministic triage
   ->
signed/encrypted uplink
   ->
central SecurityHub
   ->
correlation with:
    node identity
    route/zone
    agent snapshot
    authority provenance
   ->
proposed containment
   ->
approval where required
   ->
remote/local enforcement request
   ->
warden proof path remains local and tiny
```

---

# 30. Web UI implications

The Web UI can add infrastructure sections without inventing backend APIs:

```text
Security
  Overview
  Incidents
  Policies
  Audit

Identity & Auth
  Users
  Devices
  Nodes
  Service Identities
  Keys
  Channel Bindings

Network
  Nodes
  Routes
  Zones
  Upstreams
  Listeners
  Drain State
```

All actions call generated `harw-operations` Web routes.

UI visibility is a convenience only.

Backend authorization remains authoritative.

---

# 31. Operation domain cleanup

The current `OperationDomain` has only:

```text
Session
Agents
Execution
CatalogConfig
Knowledge
Misc
```

Infrastructure work will otherwise collapse into `Misc` or `CatalogConfig`.

Plan an explicit domain expansion:

```rust
pub enum OperationDomain {
    Session,
    Agents,
    Execution,
    CatalogConfig,
    Knowledge,

    Identity,
    Network,
    Security,
    Crypto,

    Misc,
}
```

This improves:

- help/discovery,
- policy filtering,
- Web navigation generation,
- audit classification,
- future capability mapping.

This change should happen before dozens of infra operations ship.

---

# 32. Do not confuse `PermissionTier` with cryptographic authorization

Current tier:

```text
Observer
Operator
Maintainer
Owner
```

is still useful.

But:

```text
Owner
```

does not imply:

```text
may use every KeyRef
may sign every purpose
may access every tenant
may act from every ingress zone
```

Therefore the effective key authorization occurs in Auth/Security Hub after operation-tier admission.

The Harw control plane answers:

```text
May this caller invoke "rotate key" as a class of operation?
```

AuthHub answers:

```text
May this caller rotate THIS key in THIS tenant for THIS purpose?
```

Both are required.

---

# 33. Remote Web and Pingora

Pingora belongs at the network edge, not in crypto primitives.

Suggested remote path:

```text
Browser / external client
    |
TLS
    |
Pingora
    |
strip untrusted identity headers
route by surface / zone
    |
auth gateway / AuthHub
    |
trusted security context
    |
Hyper/Tower control service
```

Pingora may perform:

```text
connection handling
TLS
routing
upstream health
load/failover
rate limiting
zone routing
```

It must not invent Harw principals from source IP alone.

---

# 34. First implementation waves

## H0 - Reconcile and freeze contracts

Deliverables:

- record current Harwness main SHA
- record CryptGuard service API version/commit used by the branch
- re-run `harw-secrets` KATs
- document stale 3.0.1 references
- document current `deploy/systemd` vs `dod/packaging/systemd` divergence
- assert no existing DoD privilege boundary is changed

Exit:

```text
no functional change
contracts and drift documented
```

## H1 - Infrastructure type vocabulary

Add:

```text
NodeId
DeviceId
ServiceIdentityId
SecurityContextId
KeyId / KeyGeneration if not imported from CryptGuard service
TrustZone
AuthStrength
```

Prefer zero-dependency/shared type crates where they genuinely fan in.

Exit:

```text
typed IDs; no raw string identity plumbing
```

## H2 - `harw-dod-encrypt`

Implement Harw-specific:

```text
HarwKeyPurpose
HarwCryptoProfile
Harw key policy
secure frame canonical transcript
replay/idempotency types
CryptGuard service mapping
```

Exit:

```text
no Hyper server
no long-term key storage
no harw-dod facade re-export
```

## H3 - Auth/Crypto Hub local service

Build `harw-auth-hub` with:

```text
KeyRef metadata
provider
generate/public/sign/verify/wrap/unwrap/rotate
secure.sock
SO_PEERCRED
CryptGuard Tower service
Hyper adapter
audit
```

Exit:

```text
private key export impossible through normal API
```

## H4 - Infrastructure clients + RuntimeServices

Build:

```text
AuthHubClient
NetworkControlClient skeleton
SecurityHubClient skeleton
```

Wire into `RuntimeServicesParts` and ServiceMap.

Exit:

```text
operations never dial sockets directly
```

## H5 - Control-plane operations

Register:

```text
key metadata
key rotate
device/node identity views
service status
health/capabilities
```

through `harw-operations`.

Expose Web routes via existing registry.

Exit:

```text
no parallel REST router
```

## H6 - `harw-secrets` KMS migration

Add V3 record.

Migrate DEK wrapping first.

Preserve existing V1/V2 reading.

Exit:

```text
new stores can operate without long-term KEK seed in Harw runtime
```

## H7 - NetSec local daemon

Build `network.sock` API and Node/Route/Zone state.

No remote node transport yet.

Exit:

```text
local node topology/control works
```

## H8 - SecurityHub

Build policy/security-context/DoD correlation service.

Connect AuthHub identity and NetSec ingress context.

Exit:

```text
WHO + WHERE -> SHOULD
```

## H9 - Three-socket Control Plane

Standardize:

```text
control.sock
network.sock
secure.sock
```

Add health/version/capabilities to each.

Exit:

```text
all local public infra APIs are runtime-path based and independently supervised
```

## H10 - systemd/package source-of-truth

Embed/render canonical unit assets.

Delete or generate the duplicate source tree only after tests prove parity.

Exit:

```text
one canonical service/socket definition per daemon
```

## H11 - Remote node transport

Use Hyper/Tower/Pingora and authenticated node identity.

Add long-lived traffic-key mode only after benchmarks.

Exit:

```text
remote node control never trusts IP as identity
```

## H12 - Multiuser/RLS Web expansion

Replace UID->tier-only assumptions with AuthHub-backed SecurityContext.

Add tenant/workspace scoping.

Exit:

```text
tenant cannot be changed by request-body editing
```

---

# 35. Work packages for Claude Code Cloud

## WP-01 Repository reconciliation

Read before modifying:

```text
docs/design/harw-dod-charter.md
docs/design/harw-dod-crate-decomposition.md
docs/design/harw-dod-integration-and-dependencies.md
harw-web/src/*
harw-operations/src/*
harw-runtime/src/assembly.rs
harw-runtime/src/services.rs
harw-runtime/src/contributors.rs
harw-secrets/src/*
harw-types/src/principal.rs
dod/packaging/*
deploy/systemd/*
```

Produce a drift report first.

## WP-02 Dependency graph proof

Generate and check that new security crates do not enter Warden/probe/sensor graphs.

## WP-03 Type vocabulary

Implement new IDs and security context leaf types.

## WP-04 Harw crypto policy layer

Implement `harw-dod-encrypt` only after the CryptGuard service API is available.

## WP-05 AuthHub core

Implement provider/lifecycle without HTTP first.

## WP-06 secure.sock transport

Attach CryptGuard Hyper adapter, SO_PEERCRED and encrypted Harw request binding.

## WP-07 typed AuthHub client

Client must be cloneable only as network handle.

## WP-08 RuntimeServices integration

No direct socket opening outside client/transport crates.

## WP-09 infrastructure operations

Register through `harw-operations`.

## WP-10 Web exposure

Use existing WebAdapter/route generation.

## WP-11 secrets V3

Add KMS wrapping format + migration tests.

## WP-12 NetSec core

Node/topology state and local control.

## WP-13 network.sock

Hyper/Tower local control endpoint.

## WP-14 SecurityHub

Policy/context correlation.

## WP-15 DoD uplink

Remote authenticated security event transport, no local DoD TCB widening.

## WP-16 deployment assets

Canonical embedded service/socket assets.

## WP-17 sysusers/tmpfiles

Explicit per-daemon runtime/state/config permissions.

## WP-18 remote fleet transport

Pingora + authenticated node channels.

## WP-19 Web identity upgrade

AuthHub-backed identity, tenant/workspace context.

## WP-20 observability

Trace:

```text
control request
-> auth identity
-> policy
-> network route
-> runtime operation
-> node/agent
```

without secret fields.

## WP-21 security fuzz/compile gates

Compile-fail invariants, malformed wire frames, body limits, replay, idempotency.

---

# 36. Required compile-time/security tests

Examples:

```text
harw_dod_facade_cannot_import_encrypt
warden_dependency_graph_has_no_crypt_guard
probe_bpf_dependency_graph_has_no_crypt_guard
probe_fs_dependency_graph_has_no_crypt_guard

secure_request_is_not_clone
secret_payload_is_not_clone
crypto_session_state_is_not_clone
auth_client_handle_is_clone

principal_cannot_deserialize
security_context_cannot_deserialize

operation_cannot_construct_private_key_export
```

Wire/adversarial tests:

```text
tampered secure header
tampered ciphertext
tampered signature
wrong recipient
wrong sender
expired TTL
clock skew
replay nonce
duplicate mutation request id
wrong tenant
wrong workspace
wrong node audience
wrong key purpose
oversize body
chunked oversize body
unknown frame version
unknown crypto profile
```

---

# 37. `harw-secrets` compatibility tests

Preserve:

```text
V1 legacy record read
V2 record read
V2 rewrap
KEM KATs
audit chain
checkpoint signatures
```

Add:

```text
V3 KMS record roundtrip
V2 -> V3 rewrap without payload ciphertext change
KMS unavailable -> fail closed
revoked KEK generation -> typed failure
wrong KMS key generation -> typed failure
```

Never silently rewrite every store during startup.

Migration must be explicit.

---

# 38. Infrastructure health/capabilities contract

Every public daemon should expose a small common set:

```text
GET /v1/health
GET /v1/version
GET /v1/capabilities
```

Capabilities are descriptive, not authority.

Example AuthHub response:

```json
{
  "service": "harw-auth-hub",
  "protocol": 1,
  "crypto_profiles": [
    "harw-strong-v1"
  ],
  "operations": [
    "key.wrap",
    "key.unwrap",
    "key.sign",
    "key.verify",
    "key.rotate"
  ]
}
```

Do not use capability discovery to silently downgrade crypto.

---

# 39. Configuration principles

Each infrastructure daemon owns its own config.

Example:

```text
/etc/harw-auth-hub/config.toml
/etc/harw-netsec/config.toml
/etc/harw-security-hub/config.toml
```

Harwness config only contains connection/profile references:

```toml
[infrastructure.auth]
socket = "/run/harw/secure.sock"

[infrastructure.network]
socket = "/run/harw/network.sock"

[infrastructure.security]
socket = "/run/harw/control.sock"
```

Production defaults should be compiled constants for standard system paths.

Tests may override with tempdirs.

No automatic fallback to project home.

---

# 40. One-source-of-truth deployment test

Add a test that compares the embedded/rendered asset set against the manifest:

```text
all installed units are embedded
all embedded units are manifested
no duplicate service filename has divergent contents
no unresolved @PLACEHOLDER@ remains
```

Optional CLI:

```bash
harw-auth-hub --print-systemd socket
harw-auth-hub --print-systemd service
harw-netsec --print-systemd socket
harw-netsec --print-systemd service
```

This is useful for distro packaging and inspection.

---

# 41. Non-goals for the first Harw-side implementation

Do not simultaneously:

```text
replace every Harw store with SQL
add full remote Web auth
move every secret into a remote HSM
rewrite Warden IPC
replace DoD JSON wire format
introduce QUIC
rename every crate
redesign the entire UI
```

The first goal is a **correct service boundary and dependency graph**.

---

# 42. Acceptance criteria

The first Harw infrastructure milestone is accepted when:

1. `/run/harw/control.sock`, `network.sock`, and `secure.sock` have documented, versioned ownership.
2. Existing `sentinel.sock` and `warden.sock` remain independent privilege boundaries.
3. `harw-dod-encrypt` does not duplicate generic CryptGuard Tower/Hyper functionality.
4. `harw-dod-encrypt` is not reachable through the `harw-dod` facade.
5. Auth/Crypto Hub owns long-term private key material.
6. Normal key APIs operate on `KeyRef`.
7. Private key export is not part of the normal service contract.
8. CryptGuard non-Clone key/context invariants survive the Harw integration.
9. Only network/service handles are cloneable for Hyper/Tower integration.
10. `harw-web` remains the single local Hyper control transport rather than being bypassed by a second router.
11. Infrastructure actions are registered through `harw-operations`.
12. PermissionTier and ApprovalPolicy remain active for UI/TUI/CLI parity.
13. RuntimeAssembly/RuntimeServices are the only standard service wiring point.
14. No arbitrary operation opens infrastructure sockets directly.
15. `harw-secrets` can create a KMS-backed record without storing a long-term KEK seed in the Harw process.
16. Existing secret V1/V2 records remain readable according to their existing rules.
17. NetSec never treats IP/MAC as cryptographic identity.
18. AuthHub identity and NetSec topology remain separate domains.
19. Remote node identity is cryptographically authenticated before topology trust.
20. Warden/probes/sensors do not gain CryptGuard/Hyper/Tower dependencies.
21. systemd/package assets have one canonical source.
22. production sockets do not live in `$HOME` or project directories.
23. body size and timeout limits exist on all Hyper-exposed management endpoints.
24. mutation requests have idempotency semantics.
25. no automatic retry is enabled for destructive/mutating crypto/network operations.
26. secret-bearing logs/debug output are redacted.
27. tenant/workspace scope is derived server-side once multiuser mode is introduced.
28. external clients cannot inject trusted identity headers/context.
29. key-purpose policy prevents arbitrary signing-oracle behavior for system identity keys.
30. CI proves the critical dependency and compile-time invariants.

---

# 43. Final architectural rule set

The upgraded plan can be summarized in ten rules:

```text
1. CryptGuard owns generic cryptographic mechanisms and generic KMS service composition.
2. Harw owns Harw-specific key purposes, policy, identity binding and orchestration.
3. Long-term private keys stay behind Auth/Crypto Hub.
4. Keys are addressed by KeyRef, not moved between services.
5. Control, network and secure concerns have separate public local sockets.
6. DoD privilege sockets remain separate and tiny.
7. Harw operations remain the one user-visible action contract.
8. RuntimeServices remains the one service-composition root.
9. Crypto state is linear/non-Clone; only network handles are Clone.
10. Identity, topology, authority and approval remain separate inputs to an action.
```

The practical result is that Harwness stops treating crypto as an implementation detail inside `harw-secrets` and starts consuming a real key-management infrastructure — without turning the harness itself into the key server, without weakening DoD privilege boundaries, and without introducing a second HTTP/authority stack beside the one that already exists.
