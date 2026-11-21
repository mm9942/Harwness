# harw-infra-client

Typed local clients for the Harwness infrastructure daemons (architecture
layer **I**, crypto masterplan v2 §11, §24, §34 H4).

| Client | Daemon | Default socket |
|---|---|---|
| `AuthHubClient` | Auth/Crypto Hub | `/run/harw/infra/secure.sock` |
| `NetworkControlClient` (skeleton) | NetSec local control API | `/run/harw/infra/network.sock` |
| `SecurityHubClient` (skeleton) | SecurityHub | `/run/harw/infra/security.sock` |

Socket layout follows `docs/architecture/crypto-drift-report.md` D1/D2.

This crate is the only place that opens these sockets. The runtime builds an
`InfrastructureAvailability` once at the composition root and hands the
clients to operations. Operations never dial a socket themselves (H4 exit
criterion).

## Usage

```rust,ignore
use harw_infra_client::{
    InfraClientConfig, InfrastructureAvailability, KeyContext, KeyProfile, KeyRef,
};
use zeroize::Zeroizing;

let config: InfraClientConfig = toml::from_str(r#"
    auth_socket = "/run/harw/infra/secure.sock"
    token_file  = "/etc/harw/infra/auth.token"
"#)?;
let infra = InfrastructureAvailability::from_config(&config)?;
let auth = infra.auth.ok_or("AuthHub not configured")?;

let kek = KeyRef::latest("secrets", "kek")?;
auth.generate_key(&kek, KeyProfile::PqHpkeDefault).await?;

let ctx = KeyContext::new(b"harw:dek:v1".to_vec(), b"record-7".to_vec());
let wrapped = auth.wrap_key(&kek, &ctx, Zeroizing::new(dek_bytes)).await?; // DEK moved in
let dek: Zeroizing<Vec<u8>> = auth.unwrap_key(&kek, &ctx, &wrapped).await?;
```

## Configuration

`InfraClientConfig` (`deny_unknown_fields`, every field optional):

| Field | Meaning |
|---|---|
| `auth_socket` | AuthHub socket |
| `network_socket` | NetSec control socket |
| `security_socket` | SecurityHub socket |
| `token_file` | AuthHub bearer token. The file must have no "other" permission bits. Setting it without `auth_socket` is an error. |

An empty configuration yields no clients. `InfraClientConfig::system_defaults()`
opts in to the standard paths. Relative socket paths are rejected, and the
client never falls back to a working or home directory (§39).

## Wire protocol

Each call opens a new `tokio::net::UnixStream`, does a hyper HTTP/1
`client::conn::http1::handshake`, sends one request and reads one response.
Connect, request and full response body all run under one
`ClientOptions::timeout` (default 5 s). Response bodies are capped at
`ClientOptions::max_response_bytes` (default 1 MiB). There is no connection
pool and no automatic retry.

Key routes are the CryptGuard reference KMS table (`crypt_guard_hyper::route`).
Frame bodies use the `CGK1` codec (`crypt_guard_hyper::codec`, content type
`application/vnd.cryptguard.kms.v1`):

| Method | Route | Request body | Response |
|---|---|---|---|
| `generate_key` | `POST /v1/keys` | CGK1 `namespace` `id` `profile` | CGK1 KeyCreated |
| `describe` | `GET /v1/keys/{ns}/{id}[@v]` | — | CGK1 Metadata |
| `public_key` | `GET …/public` | — | octets |
| `rotate` | `POST …:rotate` | — | CGK1 KeyCreated |
| `wrap_key` | `POST …:wrap` | CGK1 `info` `aad` `material`🔒 | octets |
| `unwrap_key` | `POST …:unwrap` | CGK1 `info` `aad` `wrapped` | octets🔒 |
| `rewrap_key` | `POST …:rewrap` | CGK1 8 fields (see `codec::request`) | octets |

The hub has no list route, so the client has no `list` method.

All three daemons serve the common JSON surface (§38). This is the contract
the daemons must implement:

```text
GET /v1/health        200 {"status": "ok" | "degraded", "service"?: "…"}   (empty 2xx body = ok)
GET /v1/version       200 {"service": "…", "version": "…", "protocol": 1,
                           "store_epoch"?: "<32 hex>", "boot_id"?: "<32 hex>"}
GET /v1/capabilities  200 {"service": "…", "protocol": 1, "crypto_profiles": […], "operations": […],
                           "persistence"?: "in-memory" | "sealed-file",
                           "store_epoch"?: "<32 hex>", "boot_id"?: "<32 hex>"}
```

`store_epoch` names the daemon's key store and changes when the store is
replaced (an in-memory AuthHub gets a new one on every restart; a persistent
store keeps its epoch). `boot_id` changes with every daemon process. Both are
optional (`VersionInfo`/`Capabilities` parse them as `Option<String>`), so
daemons that predate them still parse.

Capabilities only describe the daemon. The client does not use them to grant
authority or to downgrade anything.

### SecurityHub posture

`SecurityHubClient::posture()` reads `GET /v1/posture` into a typed
`Posture` (PL-100 phase 4). The client does not depend on `harw-types`, so
the document has its own types: `PosturePeer`, `DodPosture`,
`DodPostureStatus`, `PostureSeverity`, `PostureFinding`, `PostureId`,
`DisplayId`, `InstructionId` and `UntrustedText`.

```text
GET /v1/posture  200 {"peer": {"uid", "gid", "pid"?}, "principal_id"?, "host"?,
                      "dod": {"status": "ok" | "unavailable" | "not_configured",
                              "min_severity"?, "since"?, "findings"?: [...],
                              "skipped_malformed"?, "skipped_oversize"?, "truncated"?}}
                 403 {"error": "unknown_peer"} → InfraClientError::Forbidden
```

The reader is reader-first (PL-100 I11). It ignores unknown fields. It maps
an unknown `dod.status` or severity to `Unknown`, and fields the hub leaves
out in the `unavailable`/`not_configured` states get their defaults. Only a
document without `peer`, `dod` or `dod.status`, or a finding without its
identifying fields, is a `Protocol` error.

Three finding fields come straight from the DoD findings export and are
untrusted (PL-100 I4, threat model B3). The hub replaces only control
characters (`Cc`) in `summary` and `rule_id`, so bidi overrides, zero-width
and tag-block characters pass. It checks `finding_id` only for being
non-blank. The client therefore never exposes these fields as plain
strings:

- `summary` is an `UntrustedText`. It has no `Display`, no `Deref`, and a
  `Debug` that shows only its length. `escaped_line(max_chars)` renders it
  through `harw_untrusted_text::escape_line`; `as_untrusted_str()` is the
  explicitly named raw accessor.
- `finding_id` and `rule_id` are a `PostureId`. `Valid(DisplayId)` means
  the value passed the grammar of the DoD export reader (`harw-dod-client`).
  For `finding_id` that is non-blank, at most 256 bytes and nothing the
  escaper would change; for `rule_id` it is 1 to 128 bytes of
  `[A-Za-z0-9._:-]`. Any other value becomes `Invalid(UntrustedText)`. The
  finding is kept, never dropped (I3).
- `dedup_key` is kept only as 32 lowercase hex characters, the same filter
  the hub applies.

The other strings (`host`, `principal_id`, `since`, `observed_at`) come from
the hub's own configuration and timestamp rendering.

A valid id is only safe to *display*. The `finding_id` grammar accepts any
display-clean text, including "Ignore previous instructions", so a valid id
is still export text and never an instruction (I4):

- `DisplayId` has a private field and no public constructor. A `Valid` can
  only come out of the grammar: decoding, or `PostureId::from_finding_id` /
  `PostureId::from_rule_id`, which sort a value the way the decoder does;
  a plain `String` cannot be wrapped as `Valid`. `DisplayId` has no `Display`
  and no plain-`&str` accessor, only `escaped_line(max_chars)` and the
  explicitly named `as_untrusted_str()`.
- `InstructionId` is the only id type for a model instruction template. It
  holds a canonical lowercase UUIDv4 (36 characters, hyphens at 8/13/18/23,
  version `4`, variant `8`/`9`/`a`/`b`; uppercase, braced, URN, simple and
  nil UUIDs are refused), the same grammar as
  `harw_dod_client::findings::is_uuid_v4` and the node uplink.
  `InstructionId::parse` is its only constructor, and it has no
  `Deserialize`. `PostureFinding::finding_instruction_id()` (or
  `PostureId::to_instruction_id()`) returns one only for such an id.
  `Invalid` values are never promoted.

### Codec dependency

The client uses `crypt_guard_hyper`'s public `FrameWriter` and `FrameReader`,
plus `codec::frame::{MAGIC, VERSION}`. `FrameWriter` is documented as "only
for non-secret output", so the one secret-bearing body (`wrap`) is encoded in
`src/codec.rs`. It follows `codec/frame.rs` byte for byte and writes into a
zeroizing buffer that is sized exactly once. A unit test pins that encoding
to `FrameWriter`'s output. No CryptGuard type appears in this crate's public
API.

## Errors

`InfraClientError`: `Unavailable` (socket missing, connection refused or
dropped, or `503`), `Unauthenticated` (`401`), `Forbidden` (`403`),
`NotFound` (`404`), `Protocol(&'static str)` (bad HTTP, content type, frame,
JSON, or an oversized body), `Timeout`, and `Remote(RemoteErrorKind)` for
everything else (`422` → `AuthenticationFailed`, `409` → `Conflict`, and so
on).

Errors carry no payload bytes. After a `Timeout` on a mutation (`generate`,
`rotate`), the outcome is unknown. Check with `describe` before retrying.

CryptGuard hides `Forbidden` as `404` by default, so a key the caller may not
use usually shows up as `NotFound`.

## Clone boundary and secrets

- Each client is `Clone + Send + Sync` as a network handle only: an `Arc`
  around immutable parameters (socket, token, limits). The clones share no
  pool, lock, cache or crypto state (§10).
- Secret inputs are moved in. `wrap_key` takes `Zeroizing<Vec<u8>>` by value.
  Plaintext comes back as `Zeroizing<Vec<u8>>` in the same buffer it was
  received into.
- `BearerToken` is zeroizing and not `Clone`. Its `Debug` output is
  `[REDACTED]`, and the client's `Debug` shows only whether a token is set.

### Zeroization boundary

Secret request bodies go to hyper as `Bytes::from_owner(<zeroizing owner>)`.
Response bodies are copied chunk by chunk into a zeroizing buffer. When that
buffer grows, it moves to a new allocation and the old one is zeroized.
Some memory is outside this crate's control and is **not** zeroized:
hyper's internal read and write buffers, the `Authorization` header value,
and kernel socket buffers.

## Secret-store DEK wrapper

`AuthHubDekWrapper` implements `harw_secrets::DekWrapper`, so a
`SecretStore` can seal V3 (`KmsWrappedV3`) records through the hub instead of
the local KEK:

```rust,ignore
use std::sync::Arc;
use harw_infra_client::{AuthHubDekWrapper, KeyRef};

let key = KeyRef::latest("secrets", "dek-kek")?;
let wrapper = AuthHubDekWrapper::new(auth.clone(), &key, generation)?; // hub version = generation + 1
let store = SecretStore::open(root, policy, provenance, key_version)?
    .with_dek_wrapper(Arc::new(wrapper));
```

| Item | Value |
|---|---|
| `profile_id()` | `authhub-cgk1-v1` (`AUTHHUB_DEK_PROFILE_ID`) |
| `key_id()` | `namespace/id` (unversioned, as `validate_wrapper_identity` requires) |
| hub key | `namespace/id@(generation + 1)` (`crypt_guard_key_version`) |
| HPKE `info` | `harwness:secrets:dek-wrap:v3:authhub` (`AUTHHUB_DEK_WRAP_INFO`) |
| HPKE `aad` | the record AAD `harw-secrets` passes in |

`unwrap_dek` for any generation other than the wrapper's is refused with
`SecretsError::KeyGenerationMismatch` before the hub is called.

Error mapping (reasons are payload-free):

| Client error | `SecretsError` |
|---|---|
| `Remote(AuthenticationFailed)` (`422`) on unwrap, hub `store_epoch` changed | `DekWrapperUnavailable` ("AuthHub key store was replaced (epoch changed); keys are not available") |
| `Remote(AuthenticationFailed)` (`422`) on unwrap, otherwise | `Open(pq_hpke::Error::AuthenticationFailed)` |
| `Unavailable`, `Timeout` | `DekWrapperUnavailable` |
| anything else, and bridge failures | `DekWrapperUnavailable` with the client error's text |

**Store epoch.** The hub answers an unwrap under a key version its store
does not have with the same `422` as a tampered blob, so a restarted
in-memory hub would look like tampering. The wrapper records the hub's
`store_epoch` on its first wrap or unwrap (one extra `GET /v1/version`) and,
on an unwrap `422`, reads it again: a different epoch is reported as
`DekWrapperUnavailable` (key store replaced). An unchanged epoch, a hub
without one, or a failed re-read keep `Open`. Both outcomes fail closed.

**Sync/async bridge.** The trait is synchronous and the client is async.
Calling `Handle::block_on` from a runtime worker panics, so every
wrap/unwrap runs on a dedicated short-lived OS thread
(`std::thread::scope`) with its own current-thread tokio runtime. That is
safe from plain threads and from inside a runtime. Each call costs one
thread spawn and one runtime build on top of the hub round trip. That is
acceptable because `SecretStore` calls are infrequent. The calling thread is
blocked for up to `ClientOptions::timeout`, so async callers should wrap
store calls in `spawn_blocking`.

## Tests

`cargo test -p harw-infra-client`. The tests start a small hyper HTTP/1
server on a tempdir Unix socket (`src/test_server.rs`). They cover:

- health, version and capabilities
- the CGK1 key routes
- the wrap/unwrap/rewrap round trip
- status mapping for 401, 403, 404, 409, 418, 422 and 503
- timeouts
- a missing socket and a non-socket path
- malformed frames, the wrong content type and oversized bodies
- sending and rejecting the bearer token
- that clones share one immutable `Arc`
- that the token never appears in `Debug`
- config parsing (`deny_unknown_fields`, relative paths, token-file permissions)
- `AuthHubDekWrapper`: round trip from a plain thread and from inside a
  tokio runtime, generation mismatch, hub down, timeout and error mapping,
  a `SecretStore` V3 create/get round trip through the mock hub, and the
  store-epoch check (epoch changed → unavailable, unchanged or unreported →
  `Open`; the mock hub's epoch is settable via `MockState::set_epoch`)
- parsing `store_epoch`/`boot_id` with and without the fields
- the posture document (`src/posture.rs`): every state of the hub's
  format, unknown fields and values, malformed documents, `403`, the wrong
  content type and an unreachable hub; ids outside their grammar (ANSI
  escape, bidi, zero-width, tag block, overlong) kept as `Invalid` and only
  rendered escaped; `UntrustedText` escaping, bounding and redacted `Debug`
