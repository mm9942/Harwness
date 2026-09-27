# CryptGuard Service / Tower / Hyper Planning

> Status: architecture and implementation plan based on direct inspection of the current public GitHub repositories for `mm9942/crypt_guard`, `hyperium/hyper`, `hyperium/hyper-util`, `tower-rs/tower`, `tokio-rs/bytes`, `hyperium/http-body`, and RustCrypto `zeroize`.
>
> Primary design goal: turn CryptGuard into a reusable, strongly typed crypto-service substrate from which KMS / key-encryption services can be built, while keeping the default `crypt_guard` dependency free of HTTP/Tower transport baggage and preserving CryptGuard's compile-time cryptographic invariants.

## 1. Source snapshot reviewed

The plan below is based on these repository snapshots / package versions as inspected:

- `mm9942/crypt_guard` at `cb51196b0a45ccf584efdc8957cf24c691105c80`
  - package `crypt_guard 3.0.2`
  - current root package is the real crypto implementation
  - workspace use is currently limited to `crypt_guard_proc`
  - `zeroize = 1.8.1` with `derive`
- `hyperium/hyper` current public tree
  - `hyper 1.11.1`
  - Hyper's `Service` uses `call(&self, req)` and does not itself expose Tower-style readiness
- `hyperium/hyper-util`
  - current `0.1.21`
  - `TowerToHyperService<S>` is the official Tower -> Hyper bridge
  - importantly, the same adapter already exists in `0.1.20`
- `tower-rs/tower`
  - `tower 0.5.3`
  - `tower-service 0.3.3`
  - `tower-layer 0.3.3`
- `tokio-rs/bytes`
  - `bytes 1.12.1`
- `hyperium/http-body`
  - `http-body 1.1.0`
  - `Body::Data: Buf`
  - `http-body-util` provides `Full`, `Limited`, `BodyExt::collect`, streaming adapters, etc.
- RustCrypto `zeroize`
  - CryptGuard currently uses `1.8.1`
  - current upstream is `1.9.0`, but that release raises its MSRV to Rust 1.85
  - `1.8.1` documents Rust 1.72 as MSRV

### Immediate dependency conclusion

Do **not** mix the workspace/service refactor with an unnecessary `zeroize` major-minor bump.

The existing `zeroize = 1.8.1` is already sufficient for the required ownership model and preserves the current Rust baseline better. `zeroize 1.9.0` can be reviewed separately later.

Likewise, do not blindly require `hyper-util 0.1.21` just because it is the newest release. `0.1.21` is edition 2024 / Rust 1.85, while `0.1.20` already contains `TowerToHyperService` and has a much lower MSRV. The first adapter implementation should target the API available in `0.1.20` unless there is a concrete reason to raise the transport crate's MSRV.

---

## 2. Core design decision

CryptGuard becomes a real multi-crate workspace with a stable public facade.

The critical split is:

```text
crypt_guard-core
    cryptographic mechanisms and protocol implementations

crypt_guard-service
    typed crypto/KMS service semantics
    Tower Service implementation
    NO HTTP
    NO Hyper
    NO Bytes-based secret storage

crypt_guard-hyper
    network adapter
    Hyper / http / http-body / bytes
    TowerToHyperService bridge
    cloneable network handle only

crypt_guard
    public facade
    optional feature re-exports
```

The public UX remains:

```bash
cargo add crypt_guard
```

for normal crypto users, and:

```bash
cargo add crypt_guard --features service
```

or:

```bash
cargo add crypt_guard --features hyper
```

for the higher layers.

The default feature set must not pull Hyper, HTTP, Tower buffering, Tokio networking, or other server dependencies into normal CryptGuard consumers.

---

## 3. The most important invariant: only the network element is cloneable

This is the central design constraint for the service work.

Cryptographic state must stay linear / ownership-driven.

The network adapter is the one place where cheap cloning is useful and expected because Hyper's official Tower bridge requires it.

Current `hyper-util` implements approximately:

```rust
impl<S, R> hyper::service::Service<R> for TowerToHyperService<S>
where
    S: tower_service::Service<R> + Clone,
{
    // ...
}
```

Therefore `.to_owned()` does **not** replace the requirement here. The bound is literally `S: Clone`.

The solution is not to make the crypto engine cloneable.

The solution is to make a tiny **network-facing service handle** cloneable while the real crypto service remains non-Clone.

Tower already has exactly the primitive needed for this: `tower::buffer::Buffer`.

`Buffer` owns a channel handle and a background worker. Its `Clone` implementation clones only the sender/worker handle. It does not clone the inner service.

That gives us this architecture:

```text
CryptoEngine
    non-Clone
    owns keys / contexts / providers
        |
        v
CryptoService
    non-Clone
    impl tower_service::Service<CryptoRequest>
        |
        v
Crypto invariant / policy / concurrency layers
    still non-Clone
        |
        v
tower::buffer::Buffer
    CLONEABLE NETWORK HANDLE
    moves requests through channel
        |
        v
CryptoHttpService
    Clone because it contains only network/config handles
        |
        v
hyper_util::service::TowerToHyperService
        |
        v
Hyper
```

This is the clean answer to the Hyper/Tower clone mismatch.

### 3.1 Compile-time rule

The new service layer should explicitly guarantee:

```text
SecretBytes                 !Clone
CryptoRequest               !Clone
CryptoResponse              !Clone if it can contain secret output
CryptoEngine                !Clone
CryptoService               !Clone
KeyLease / KeyMaterial      !Clone
SenderContext               !Clone
RecipientContext            !Clone

NetworkServiceHandle         Clone
NetworkBytes                 Clone
HTTP adapter handle          Clone
```

The cloneable boundary is transport infrastructure, not cryptographic state.

### 3.2 Important existing CryptGuard exception

The modern v3 CryptGuard types already follow this philosophy well:

- ML-KEM secret key types are `ZeroizeOnDrop` and are not Clone.
- KEM shared secret types are `ZeroizeOnDrop` and are not Clone.
- ML-DSA signing key is secret and not Clone.
- PQ HPKE `SenderContext` and `RecipientContext` are intentionally not Clone because cloning sequence state could cause nonce reuse.
- `RecipientKeyPair` intentionally has a private member that is not Clone.

However, the older generic `key_control::Key` currently derives `Clone` regardless of whether `key_type` is `SecretKey` or `SharedSecret`.

Therefore:

> The new service/KMS implementation must not use legacy `key_control::Key` as its secret-bearing internal representation.

Do not weaken the new service model to match that legacy type. If desired, legacy `Key` can be hardened/deprecated separately in a breaking or migration-oriented change.

---

## 4. `.to_owned()` policy

`.to_owned()` remains useful, but it must not become an accidental way around the ownership model.

There are three different cases.

### 4.1 Public / network data

For public keys, ciphertext, signatures, encoded envelopes, key IDs, routing metadata, and HTTP payloads that are already non-secret:

```rust
let owned = borrowed.to_owned();
```

is completely normal.

### 4.2 Secret ingress

Calling `.to_owned()` on a secret `&[u8]` creates a real duplicate allocation.

That is sometimes necessary at an I/O boundary, but it must be explicit and audited.

The service API should prefer something like:

```rust
SecretBytes::copy_from_slice(secret)
```

over generic `.to_owned()` so that a secret copy is visually obvious during review.

### 4.3 TowerToHyperService

`ToOwned` cannot satisfy Hyper-util's `S: Clone` bound.

Even if a type implements `ToOwned`, `TowerToHyperService` still requires `Clone`.

Therefore the service passed to `TowerToHyperService` must be the cloneable **network handle**, never the crypto engine itself.

---

## 5. Secret memory model: Zeroize under a non-Clone newtype

Do not use:

```rust
type SecretBytes = zeroize::Zeroizing<Vec<u8>>;
```

as the public secret type.

Why: `Zeroizing<Z>` itself implements `Clone` when `Z: Clone`. `Vec<u8>` is Clone.

Instead use a private-field newtype that intentionally does **not** implement Clone:

```rust
pub struct SecretBytes {
    inner: zeroize::Zeroizing<Box<[u8]>>,
}
```

or, where capacity management is required:

```rust
pub struct SecretBuffer {
    inner: zeroize::Zeroizing<Vec<u8>>,
}
```

with no `Clone` implementation and no public access to the inner owner.

### 5.1 Prefer fixed / non-reallocating storage once inside the crypto boundary

The Zeroize documentation is explicit that `Vec` zeroization cannot erase copies left behind by prior reallocations.

Therefore:

- fixed-size secrets -> arrays, e.g. `[u8; 32]`, `[u8; 64]`
- variable-size long-lived secret material -> preferably `Box<[u8]>`
- temporary build buffers -> `Zeroizing<Vec<u8>>` only with pre-sized / exact capacity and no growth after secret data is inserted

This maps well to CryptGuard's current design:

- session keys: fixed arrays
- provenance seeds: fixed arrays
- ML-KEM private seed formats: fixed-size secret material
- plaintext request bodies: variable-size ephemeral secret buffers

### 5.2 Secret APIs must not expose accidental clone affordances

The service crate should not publicly expose:

```rust
pub fn as_vec(&self) -> &Vec<u8>
pub fn into_inner(self) -> Vec<u8>
```

for secret key material.

If byte access is required for a cryptographic primitive, keep it borrowing:

```rust
impl AsRef<[u8]> for SecretBytes
```

or better expose narrowly-scoped internal methods.

For secret output that must leave the process, require an explicit egress transition.

---

## 6. Bytes belongs at the network boundary

`bytes::Bytes` is excellent for Hyper because:

- it is designed for networking,
- it is cheap to clone,
- slices can share storage,
- Hyper incoming bodies already use `Bytes`,
- `http-body::Body` works with `Data: Buf`,
- `http-body-util::Full<Bytes>` is a natural response body.

Those exact strengths make `Bytes` the wrong default container for private keys and internal secret state.

So the rule is:

```text
Cryptographic side:
    SecretBytes / typed keys / non-Clone contexts

Network side:
    Bytes / BytesMut / Body<Data = Bytes>
```

### 6.1 Public wire output

Use `Bytes` freely for:

```text
ciphertext
CGH3 envelopes
signatures
public keys
key references
protocol frames
error payloads
```

These values can be cheap-cloned by the network stack.

### 6.2 Inbound plaintext

Hyper produces network `Bytes`.

Before plaintext crosses into the cryptographic service:

```text
Hyper Bytes
    |
    | one explicit ownership/copy boundary
    v
SecretBytes
    |
    v
CryptoService
```

After that conversion, the service layer no longer deals in `Bytes` for plaintext.

This intentionally sacrifices a tiny amount of zero-copy purity to recover a much stronger ownership and memory-erasure model.

### 6.3 Outbound decrypted plaintext: use Bytes::from_owner carefully

`bytes::Bytes::from_owner` is particularly interesting for an explicit plaintext egress response.

It can hold an owner object and share the same underlying owner across cloned `Bytes` handles. The owner is dropped only when the last clone is gone.

That means the Hyper adapter can do:

```rust
let owned = Zeroizing::new(secret_vec);
let bytes = Bytes::from_owner(owned);
let body = Full::new(bytes);
```

The network layer may clone `Bytes`, but all clones point at one owner. When the last network handle is dropped, the `Zeroizing` owner is dropped and erases its allocation.

This is a good fit for the rule "only the network element may clone".

Caveat:

- owner-backed `Bytes` conversions back into mutable / Vec storage may deep-copy.
- do not bounce a `Bytes::from_owner(secret)` back into crypto-side buffers.
- use it as a one-way egress representation.
- once plaintext reaches TLS/kernel/socket buffers, those external copies are outside Zeroize's guarantees.

Therefore define a dedicated internal type / constructor for this transition rather than sprinkling `Bytes::from_owner` around the codebase.

---

## 7. Workspace layout

Recommended transitional layout that preserves the existing top-level package name:

```text
crypt_guard/
|
|-- Cargo.toml                  # package crypt_guard + workspace root
|-- src/
|   `-- lib.rs                  # thin public facade
|
|-- crates/
|   |-- core/
|   |   |-- Cargo.toml          # package crypt_guard-core
|   |   `-- src/                # today's crypto implementation
|   |
|   |-- service/
|   |   |-- Cargo.toml          # package crypt_guard-service
|   |   `-- src/
|   |
|   `-- hyper/
|       |-- Cargo.toml          # package crypt_guard-hyper
|       `-- src/
|
`-- crypt_guard_proc/
    |-- Cargo.toml
    `-- src/
```

The root stays the package users install:

```toml
crypt_guard = "..."
```

The other packages are published individually to crates.io, but normal users do not need to list them directly.

---

## 8. Dependency graph

The graph must be acyclic:

```text
crypt_guard_proc
       |
       v
crypt_guard-core
       |
       v
crypt_guard-service
       |
       v
crypt_guard-hyper

crypt_guard facade
   |-- crypt_guard-core        always
   |-- crypt_guard-service     optional
   `-- crypt_guard-hyper       optional
```

Crucially:

```text
crypt_guard-service  X--> crypt_guard facade
crypt_guard-hyper    X--> crypt_guard facade
```

They depend on the core/service packages below them, not on the facade above them.

That is what makes:

```toml
crypt_guard = { version = "...", features = ["hyper"] }
```

possible without a Cargo dependency cycle.

---

## 9. Feature model

Proposed public facade features:

```toml
[features]
default = [
    "ml-kem-backend",
    "ml-dsa-backend",
]

ml-kem-backend = ["crypt_guard-core/ml-kem-backend"]
ml-dsa-backend = ["crypt_guard-core/ml-dsa-backend"]
sign-slhdsa    = ["crypt_guard-core/sign-slhdsa"]
cgv2-compat    = ["crypt_guard-core/cgv2-compat"]
legacy-pqclean = ["crypt_guard-core/legacy-pqclean"]

service = [
    "dep:crypt_guard-service",
]

hyper = [
    "service",
    "dep:crypt_guard-hyper",
]
```

Possible later features:

```text
tower-stack
hyper-client
hyper-server
http1
http2
kms-example
```

Do not enable "full" features from Tower/Hyper by default.

Every dependency should be chosen intentionally.

---

## 10. Facade API

Root `crypt_guard/src/lib.rs` should remain boring.

Conceptually:

```rust
pub use crypt_guard_core::*;

#[cfg(feature = "service")]
pub mod service {
    pub use crypt_guard_service::*;
}

#[cfg(feature = "hyper")]
pub mod hyper {
    pub use crypt_guard_hyper::*;
}
```

The existing paths should remain stable wherever possible:

```rust
crypt_guard::pq_hpke::HpkeEnvelope
crypt_guard::sign::...
crypt_guard::kem::...
```

even though the implementation has moved to `crypt_guard-core`.

---

## 11. Proc-macro compatibility

`crypt_guard_proc` currently generates paths such as:

```rust
crypt_guard::log::initialize_logger(...)
```

That is actually desirable for downstream users because `crypt_guard` remains the public facade.

The refactor must therefore preserve public macro expansion paths.

Potential issue: if a proc macro is used while compiling `crypt_guard-core` itself, an expansion referencing `crypt_guard::...` may no longer resolve naturally because the package is named `crypt_guard_core`.

Recommended compatibility technique inside core:

```rust
extern crate self as crypt_guard;
```

only where needed for internal macro expansion compatibility.

Add trybuild tests for both:

```text
downstream facade usage
core-internal macro usage
```

No macro should force downstream users to depend directly on `crypt_guard-core`.

---

## 12. crypt_guard-service: responsibilities

`crypt_guard-service` is not an HTTP crate.

It defines:

- KMS/key reference types
- key metadata
- supported operation types
- secret/public output ownership classes
- provider/executor traits
- `CryptoService`
- Tower `Service<CryptoRequest>`
- policy hooks
- audit hooks
- concurrency/readiness semantics
- service-level errors

It should not know:

- URI paths
- HTTP headers
- status codes
- Hyper bodies
- TCP/TLS listeners
- cookies/sessions
- Web frameworks

---

## 13. Key service model

Do not make private key bytes the normal service API.

The service should be handle-oriented.

```rust
pub struct KeyRef {
    namespace: KeyNamespace,
    id: KeyId,
    version: KeyVersion,
}
```

Operations:

```rust
pub enum CryptoOperation {
    Generate(GenerateKey),
    Rotate(RotateKey),
    Disable(DisableKey),
    Destroy(DestroyKey),

    PublicKey(GetPublicKey),

    Encrypt(Encrypt),
    Decrypt(Decrypt),

    Sign(Sign),
    Verify(Verify),

    WrapKey(WrapKey),
    UnwrapKey(UnwrapKey),
    RewrapKey(RewrapKey),
}
```

A normal decrypt request is:

```text
KeyRef + ciphertext + context
```

not:

```text
private key bytes + ciphertext
```

A normal sign request is:

```text
KeyRef + message/digest
```

not:

```text
signing key bytes + message
```

---

## 14. Provider abstraction

Prefer an operation-oriented provider over a provider that hands raw private keys to the service.

Bad generic shape:

```rust
trait KeyProvider {
    fn private_key(&self, key: KeyRef) -> SecretKey;
}
```

Better:

```rust
trait CryptoProvider {
    fn generate(...);
    fn public_key(...);

    fn encrypt(...);
    fn decrypt(...);

    fn sign(...);
    fn verify(...);

    fn rotate(...);
}
```

The first provider can use CryptGuard's in-process keys.

Later providers can map the same contract to:

```text
filesystem-backed encrypted store
OS keyring
TPM
PKCS#11
HSM
remote KMS
Harwness secure daemon
```

without ever changing the caller-facing API.

This also lets "private key never leaves provider" become an architectural property instead of a convention.

---

## 15. Request / response ownership model

A request should be moved, not cloned.

Example:

```rust
pub struct CryptoRequest {
    pub request_id: RequestId,
    pub operation: CryptoOperation,
}
```

No `Clone`.

Sensitive request types own `SecretBytes`.

Example:

```rust
pub struct Encrypt {
    pub key: KeyRef,
    pub plaintext: SecretBytes,
    pub context: CryptoContext,
}
```

Ciphertext inputs can use a non-secret owned blob type:

```rust
pub struct CiphertextBlob(Box<[u8]>);
```

The service response should likewise distinguish classes:

```rust
pub enum CryptoResponse {
    KeyCreated {
        key: KeyRef,
        public: PublicBlob,
    },

    Ciphertext(CiphertextBlob),

    Plaintext(SecretBytes),

    Signature(SignatureBlob),

    Verification(VerificationResult),

    PublicKey(PublicBlob),
}
```

No `Clone` on the enum because one variant may contain secret data.

---

## 16. The Tower service

Core shape:

```rust
pub struct CryptoService<P> {
    provider: P,
    limits: CryptoLimits,
}
```

It implements:

```rust
tower_service::Service<CryptoRequest>
```

and remains non-Clone.

`poll_ready` is meaningful:

- provider unavailable
- worker pool saturated
- HSM/TPM queue full
- memory/admission ceiling reached
- key-store temporarily unavailable

This is better than hiding every overload behind ad-hoc semaphores in the HTTP layer.

---

## 17. The clone bridge: Tower Buffer

Recommended construction:

```rust
let crypto = CryptoService::new(provider);

let crypto = ConcurrencyLimit::new(
    crypto,
    max_in_flight,
);

let network = Buffer::new(
    crypto,
    queue_bound,
);
```

At this point:

```text
crypto service: non-Clone
network Buffer handle: Clone
```

The request is **moved** into the buffer channel.

No secret request data needs `Clone`.

This is almost exactly the abstraction we need.

### Why not simply derive Clone on CryptoService?

Because that would either:

- clone key state,
- clone context state,
- force `Arc` around cryptographic state,
- or silently weaken the ownership guarantees.

None are necessary.

### Why Buffer is preferable to a home-grown clone shim

Tower Buffer already has:

- readiness semantics
- bounded queue
- a worker
- cloneable handles
- error propagation
- established behavior in the Tower ecosystem

Use the existing ecosystem primitive unless benchmarks or security review reveal a specific reason not to.

---

## 18. Concurrency model

There are two distinct capacities:

```text
queue capacity
execution concurrency
```

Keep them explicit.

Example:

```text
queue_bound = 128
max_crypto_in_flight = 32
```

Do not confuse the two.

Tower's documentation makes layer order significant.

Recommended conceptual order:

```text
network admission
    |
bounded queue
    |
concurrency limit
    |
crypto service
```

For a high-security KMS it may be preferable to reject overload rather than queue excessively.

An optional load-shed profile can return a typed overload response when capacity is exhausted.

---

## 19. Timeout policy

Tower's `Timeout` aborts the response future when the timeout elapses.

That is useful, but dangerous to interpret incorrectly for mutations.

A timeout must **not** mean:

```text
the key rotation definitely did not happen
```

Therefore:

- read-only operations can use normal deadlines
- mutation requests need request IDs / idempotency records
- create/rotate/destroy must expose an "unknown completion" state if the caller loses the response
- no automatic retry of mutations

---

## 20. Retry policy

Do not install `tower::retry` globally.

Default:

```text
NO automatic retries
```

Potentially retryable after explicit classification:

- get public key
- verify
- metadata read
- health/capability query

Potentially safe but semantically surprising:

- encrypt (new randomness may produce a different valid ciphertext)
- sign (signature may differ depending on backend/mode and creates duplicate audit events)

Not automatically retryable:

- generate
- rotate
- disable
- destroy
- import
- rewrap with state mutation

---

## 21. crypt_guard-hyper: responsibilities

`crypt_guard-hyper` owns only the transport mapping.

It defines a cloneable network adapter roughly like:

```rust
#[derive(Clone)]
pub struct CryptoHttpService<S> {
    inner: S,              // Buffer / network service handle
    config: Arc<HttpConfig>,
}
```

That clone contains no keys and no crypto contexts.

It implements:

```rust
tower_service::Service<http::Request<B>>
```

where the body eventually yields `bytes::Bytes`.

Then:

```rust
TowerToHyperService::new(http_service)
```

turns it into Hyper's own service trait.

---

## 22. HTTP body handling

Hyper's incoming body already yields:

```rust
Body<Data = Bytes>
```

Use `http-body-util::Limited` before collection for one-shot operations.

For initial KMS endpoints:

```text
HTTP body
    |
Limited(max)
    |
collect
    |
Bytes
    |
decode
    |
Secret ingress boundary / public wire decode
    |
CryptoRequest
```

Never `collect()` an unbounded body.

Different operation classes should have different maxima:

```text
key metadata request     tiny
sign request             bounded
encrypt/decrypt request  configured
key import               strict
```

---

## 23. HTTP adapter should be binary-first internally

Do not couple the service ABI to JSON.

The service operates on typed Rust requests.

The Hyper adapter can expose one or more codecs:

```text
application/json           optional convenience/admin
application/cbor           possible later
application/octet-stream   raw data endpoints
CryptGuard binary frame    preferred for machine-to-machine
```

The transport codec is not the crypto service contract.

This also allows Unix socket / custom frame transports later without changing the service.

---

## 24. KMS HTTP surface

A reasonable first reference mapping:

```text
POST   /v1/keys
GET    /v1/keys/{key}
GET    /v1/keys/{key}/public

POST   /v1/keys/{key}:encrypt
POST   /v1/keys/{key}:decrypt
POST   /v1/keys/{key}:sign
POST   /v1/keys/{key}:verify

POST   /v1/keys/{key}:rotate
POST   /v1/keys/{key}:disable
POST   /v1/keys/{key}:destroy

POST   /v1/keys/{key}:wrap
POST   /v1/keys/{key}:unwrap
POST   /v1/keys/{key}:rewrap
```

These paths belong to the **reference adapter**, not to `crypt_guard-core`.

---

## 25. Authentication and authorization

The generic Hyper adapter should not pretend it is a complete authentication system.

It should expose hooks/layers for:

```text
Principal extraction
Request authentication
Key namespace authorization
Operation authorization
Secret-egress authorization
Audit context
```

The adapter can ship traits and extension points, but not hard-code Harwness-specific identity.

Example model:

```rust
pub struct RequestContext {
    pub principal: Principal,
    pub request_id: RequestId,
    pub peer: PeerContext,
}
```

The service sees only validated context.

---

## 26. Error mapping

Keep core cryptographic errors typed.

Do not leak backend details into unauthenticated HTTP responses.

Important CryptGuard behavior that should be preserved:

`HpkeEnvelope::open` maps mismatched info/AAD and ciphertext tampering to opaque authentication failure.

The Hyper layer should not turn that into distinguishable oracle responses.

Example response classes:

```text
400 / 422   malformed request or cryptographic input
401         no authenticated caller
403         authenticated but not allowed
404         key not visible / not found (policy-dependent)
409         lifecycle/version conflict
413         body too large
429         policy/rate limit
503         crypto provider / capacity unavailable
500         internal invariant / unexpected failure
```

For protected namespaces, consider returning the same external response for "not found" and "not authorized" to reduce key enumeration.

---

## 27. Observability rules

CryptGuard already uses tracing.

For service/KMS work, logs must never include:

```text
plaintext
private key bytes
provenance seeds
shared secrets
session keys
wrapped secret internals
authorization tokens
```

Safe structured fields include:

```text
request_id
operation
key_ref
key version
algorithm/profile id
input size
output size
latency
result class
provider id
```

`Debug` for secret-bearing types should be redacted or non-exhaustive.

---

## 28. Existing CryptGuard primitives that map directly into the service

The service should compose existing mechanisms, not reimplement them.

### PQ HPKE / envelopes

Use:

```text
pq_hpke::HpkeEnvelope
pq_hpke::setup_base_sender
pq_hpke::setup_base_receiver
pq_hpke::setup_psk_sender
pq_hpke::setup_psk_receiver
derive_recipient_key_pair
generate_recipient_key_pair
```

### Signatures

Use the generic:

```text
SignAlgorithm
```

with ML-DSA/SLH-DSA backends.

### Typed secret ownership

Reuse / extend the current pattern:

```text
ZeroizeOnDrop
non-Clone secret key
non-Clone shared secret
```

### Stateful channels

Keep Sender/Recipient contexts non-Clone.

The service must never wrap them in a public `Arc<Mutex<_>>` merely to make Tower cloning easy.

If long-lived context routing is needed, store contexts in a single owner/actor and reference them by opaque channel IDs.

---

## 29. Long-lived secure channel model

For a future stateful KMS transport or Harwness secure channel:

```text
ChannelId
    |
    v
single owner task
    |
    +-- SenderContext (non-Clone)
    `-- RecipientContext (non-Clone)
```

Network requests carry `ChannelId`, never the context itself.

The actor serializes access to sequence state.

This directly preserves CryptGuard's current nonce-reuse prevention invariant.

---

## 30. Public keys / ciphertext / signatures and Clone

Current core types legitimately allow Clone for some non-secret artifacts:

- public keys
- ciphertext
- signatures
- envelopes
- suite identifiers

The service-layer invariant should be stricter without forcing an unrelated breaking change in core.

Recommendation:

- do not remove existing public-artifact Clone impls as part of this workspace migration
- do not expose core crypto object cloning as part of the service API
- convert public results into a network-owned representation at the egress boundary
- enforce that **service state** and **secret-bearing state** are non-Clone

If a future major release wants "only wire objects implement Clone" across all CryptGuard APIs, do that as a separate semver decision.

---

## 31. Compile-fail / invariant tests

This project should use the compiler as a security test.

Add trybuild tests such as:

```text
fail/clone_secret_bytes.rs
fail/clone_crypto_request.rs
fail/clone_crypto_response.rs
fail/clone_crypto_service.rs
fail/clone_sender_context.rs
fail/clone_recipient_context.rs
pass/clone_network_service_handle.rs
pass/tower_to_hyper_service.rs
```

Examples of required failures:

```rust
let b = secret.clone();          // must not compile
let s2 = crypto_service.clone(); // must not compile
```

Required success:

```rust
let h2 = network_handle.clone();
```

This is not style; it is part of the security contract.

---

## 32. Memory behavior tests

Add tests for:

- `SecretBytes` Drop executes zeroization
- no `Clone` impl
- no debug output of contents
- fixed-size secret types remain fixed-size
- secret builders do not reallocate after secret insertion
- egress owner-backed `Bytes` retains owner until final clone drop
- final owner drop zeroizes the owned user-space plaintext allocation
- network clone does not create a second plaintext allocation when using owner-backed `Bytes`

Use instrumentation test owners rather than trying to inspect freed memory unsafely.

---

## 33. KMS reference implementation strategy

Do not begin by publishing a giant "production KMS server".

First build the reusable layers.

Phase 1 example:

```text
examples/kms_server.rs
```

or a non-published workspace binary.

It demonstrates:

- in-memory/file-backed provider
- `CryptoService`
- Tower buffer clone boundary
- body limits
- Hyper adapter
- Unix/TCP listener example
- no private key export
- key creation/encrypt/decrypt/sign/verify

After the service API stabilizes, consider a separate:

```text
crypt_guard-kms
```

package.

The KMS binary should be a consumer of the same public service APIs third parties use.

---

## 34. Cargo publishing order

Because the facade depends on crates.io packages below it, publish in dependency order:

```text
1. crypt_guard-proc
2. crypt_guard-core
3. crypt_guard-service
4. crypt_guard-hyper
5. crypt_guard facade
```

Version them in lockstep initially.

Example:

```text
3.1.0
```

for all packages.

That makes compatibility obvious while the workspace is young.

---

## 35. Migration phases

### Phase A - Freeze current API behavior

Before moving code:

- full test pass
- docs tests
- public API snapshot
- KATs for PQ HPKE
- provenance re-derivation tests
- signed envelope tests
- feature matrix tests
- legacy opt-in tests

No service work yet.

### Phase B - Extract `crypt_guard-core`

Move the current implementation into `crates/core`.

Root `crypt_guard` becomes facade.

Acceptance:

```text
existing downstream import paths remain valid
existing examples compile
existing macros work
no HTTP/Tower/Bytes dependency in core
```

### Phase C - Proc macro compatibility

Prove generated paths still work.

Add `extern crate self as crypt_guard` internally only if needed.

No public macro path change.

### Phase D - Add service types

Create:

```text
SecretBytes
KeyRef
KeyMetadata
CryptoOperation
CryptoRequest
CryptoResponse
CryptoServiceError
CryptoProvider
```

No Hyper.

No network Bytes.

### Phase E - Implement non-Clone `CryptoService`

Implement Tower `Service<CryptoRequest>`.

Wire existing CryptGuard operations.

Add invariant compile tests.

### Phase F - Add Tower execution stack

Introduce:

```text
ConcurrencyLimit
Buffer
optional LoadShed
```

The Buffer output becomes the cloneable network handle.

Prove inner service remains non-Clone.

### Phase G - Add `crypt_guard-hyper`

Add:

```text
http
http-body
http-body-util
hyper
hyper-util
bytes
```

Implement:

```text
CryptoHttpService
body limiting
request decoding
response encoding
error mapping
TowerToHyperService constructor
```

### Phase H - Secret/network boundary hardening

Implement:

```text
SecretBytes
NetworkBytes
SecretEgress / owner-backed Bytes path
redacted Debug
no secret tracing
body-size classes
```

### Phase I - KMS reference server

Build a small reference server using only public APIs.

Test TCP and optionally Unix sockets.

### Phase J - Facade features

Expose:

```text
crypt_guard::service
crypt_guard::hyper
```

under optional features.

### Phase K - crates.io release gates

Publish lower crates, then facade.

Check dependency tree for every feature combination.

---

## 36. Feature / dependency CI matrix

Required CI:

```bash
cargo check -p crypt_guard-core --no-default-features
cargo check -p crypt_guard-core --all-features

cargo test -p crypt_guard-service
cargo test -p crypt_guard-hyper

cargo test -p crypt_guard
cargo test -p crypt_guard --features service
cargo test -p crypt_guard --features hyper

cargo tree -p crypt_guard
cargo tree -p crypt_guard --features service
cargo tree -p crypt_guard --features hyper
```

Hard gate:

```text
default crypt_guard dependency tree MUST NOT contain:
hyper
hyper-util
http-body
tower
tokio net/runtime transport machinery
```

Service-only gate:

```text
crypt_guard --features service MUST NOT require Hyper
```

Hyper gate:

```text
crypt_guard --features hyper MAY include network stack
```

---

## 37. Security review checklist

Before first public release of the service crates:

- no private key export operation by default
- no secret-bearing type derives Clone
- no secret-bearing service request/response derives Clone
- no core crypto context wrapped in cloneable network state
- no secret `Debug`
- no plaintext in tracing
- bounded HTTP bodies
- typed key refs
- versioned operations
- key lifecycle state machine
- opaque crypto authentication failures
- no default retries for mutations
- idempotency story for create/rotate/destroy
- readiness/backpressure tests
- queue bounds
- overload behavior
- provider failure behavior
- cancellation behavior
- crash/restart behavior
- provenance re-derivation KATs
- fuzzing for wire/body codecs
- malformed frame tests
- compile-fail invariant tests

---

## 38. Harwness / harw-dod-encrypt consumption

Once this exists, Harwness should not invent a parallel Hyper/Tower crypto stack.

It consumes the generic CryptGuard service layer:

```text
crypt_guard-core
        |
crypt_guard-service
        |
crypt_guard-hyper
        |
harw-dod-encrypt
```

Harw adds only Harwness-specific semantics:

```text
Harw KeyRef namespaces
Node identities
AuthHub policy
SecurityHub policy
secure.sock
fleet / node routing
DoD audit context
```

The generic service remains usable without Harwness.

That is the architectural payoff.

---

## 39. Recommended first implementation skeleton

The first practical code target should look approximately like:

```rust
// crypt_guard-service

pub struct SecretBytes {
    inner: Zeroizing<Box<[u8]>>,
}

pub struct CryptoService<P> {
    provider: P,
}

impl<P> Service<CryptoRequest> for CryptoService<P>
where
    P: CryptoProvider + Send + 'static,
{
    type Response = CryptoResponse;
    type Error = CryptoServiceError;
    type Future = CryptoFuture;

    fn poll_ready(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        // provider/admission readiness
    }

    fn call(&mut self, req: CryptoRequest) -> Self::Future {
        // request is MOVED
    }
}
```

Then:

```rust
// crypt_guard-hyper

let crypto = CryptoService::new(provider);

let crypto = tower::limit::ConcurrencyLimit::new(
    crypto,
    32,
);

let network = tower::buffer::Buffer::new(
    crypto,
    128,
);

// This is the intentionally cloneable boundary.
let http = CryptoHttpService::new(network);

let hyper = hyper_util::service::TowerToHyperService::new(
    http,
);
```

The meaningful detail is not the exact numbers.

It is this property:

```text
TowerToHyperService clones a network handle.
It never clones a key.
It never clones an HPKE context.
It never clones a plaintext SecretBytes.
```

---

## 40. Final architecture statement

The intended stack is:

```text
                     application / KMS
                            |
                            v
                 crypt_guard-service
                 typed semantic operations
                 non-Clone crypto state
                            |
                            v
                     crypt_guard-core
                  actual cryptography
                            |
                            |
              -----------------------------
              transport ownership boundary
              -----------------------------
                            |
                            v
                 tower::buffer::Buffer
                 cloneable network handle
                            |
                            v
                 crypt_guard-hyper
               HTTP/body codec + Bytes
                            |
                            v
             TowerToHyperService
                            |
                            v
                         Hyper
```

The core principle is:

> Cryptographic ownership rules stay cryptographic.  
> Network cloning stays networking.

`Bytes` is embraced where it is strongest: the wire.

`Zeroize` remains on the secret side, behind non-Clone wrapper types.

Tower provides service composition, readiness, bounded buffering, and backpressure.

Hyper remains transport machinery.

`hyper-util::TowerToHyperService` is used exactly for what upstream designed it for, but the Clone it requires is satisfied by a **network handle**, never by making CryptGuard's cryptographic state cloneable.

That preserves the existing CryptGuard philosophy instead of weakening it to fit an HTTP framework.

---

## 41. Definition of Done for the first release

The first service-enabled CryptGuard release is complete when all of the following are true:

1. `cargo add crypt_guard` still behaves as a crypto library and does not pull HTTP/Tower transport dependencies.
2. `cargo add crypt_guard --features service` exposes a typed Tower crypto service without Hyper.
3. `cargo add crypt_guard --features hyper` exposes a Hyper-ready service using the official Tower adapter.
4. `CryptoService` is non-Clone.
5. secret request/response types are non-Clone.
6. stateful HPKE contexts remain non-Clone.
7. only the network handle required by the Tower/Hyper bridge is Clone.
8. the cloneable network handle does not contain key material or cryptographic contexts.
9. `Bytes` is limited to network/public wire data and explicit secret egress.
10. secret internal buffers are zeroized and cannot be cloned through the public service API.
11. HTTP bodies are bounded.
12. no private key export exists as a default service operation.
13. mutation retries are disabled by default.
14. compile-fail tests prove the ownership invariants.
15. a reference KMS server can be built from the public APIs without reaching into private CryptGuard internals.
16. Harwness can consume the same service crates rather than carrying its own forked crypto/Hyper/Tower composition.

This is the point at which CryptGuard stops being "a crypto crate that can be wrapped in a server" and becomes a crypto platform that deliberately supports building safe key-management services without forcing every user to re-wire KEM/KDF/AEAD/signature/memory/network semantics themselves.
