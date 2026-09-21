# Secrets & Audit

Status: design draft. No code exists yet for `harw-secrets` — this document
specifies the shape it must have before implementation starts.

> **Ist-Stand (2026-09)**: The "no code exists yet" claim above is outdated.
> `harw-secrets` exists and implements the envelope (DEK/KEK), with
> ML-KEM-1024 as the default policy, rotation, KEK provenance, the
> hash-chained audit log, and ML-DSA checkpoints as designed below.
> `crypt_guard` is pinned to `=3.0.1` (not the `2.0.3`/`"2"` referenced in
> §2.1/§5.1). Still open: audit events are only emitted for `secret.*`
> actions (approval/channel/plugin events from §4.4 are not wired up yet),
> and at-rest encryption of the session journal and knowledge artifacts
> (§1.b) is not implemented.

## 0. Why one document

Harwness holds three kinds of material that must not leak in cleartext and
whose handling must be independently checkable after the fact: provider API
keys and channel/bot tokens (needed on every process start), optional
at-rest encryption of session-journal and knowledge-artifact content (needed
only when an operator opts in), and a tamper-evident record of who did what
(needed continuously, whether or not encryption is turned on). These three
needs share one root problem — "prove nothing was read or changed without
authorization" — so they get one crate, `harw-secrets`, and one document,
rather than a secret-store bolted on separately from an audit-log bolted on
separately from a session-encryption flag.

`harw-secrets` is deliberately small. It is a DEK/KEK envelope library plus an
append-only, hash-chained event log. It does not run a server, does not touch
Postgres, and does not know about channels, providers, or the turn loop — it
is called *by* `harw-provider`, `harw-config`, and (once built) the session
and knowledge crates, the same way `harw-macros::HarwError` is called by
everything without any of those crates being aware of proc-macro internals.

## 1. Scope

**a. At-rest protection of provider API keys and channel/bot tokens.**
Today `harw-provider::auth` holds tokens in `secrecy::SecretString` in
memory (see `harw-provider/src/auth.rs`) but `harw-config` still needs to
read them from *somewhere* durable at process start — currently plain
environment variables or a plaintext config file. `harw-secrets` gives
`harw-config` a `SecretStore` so that on disk those values are only ever
DEK-sealed envelopes, never plaintext, while the in-memory representation
after unsealing continues to be `secrecy::SecretString` /
`secrecy::SecretBox<[u8]>` exactly as today.

**b. Optional at-rest encryption of session-journal and knowledge
artifacts.** `harw-core::history` and the planned `harw-knowledge` crate
(see `docs/design/knowledge-surfaces.md`) write markdown/JSON files to disk.
Encrypting those is opt-in per deployment (`CryptoPolicy::disabled()` is a
legitimate choice for a single-user local install) but when turned on it
must use the same envelope shape as secrets, so there is exactly one sealing
primitive in the codebase, not two.

**c. Tamper-evident audit records.** Every privileged action — approval
decisions, channel commands, plugin installs, secret access — gets an
append-only, hash-chained `AuditEvent`. This must be independently
verifiable (`harw doctor`) without trusting the process that wrote it,
because the whole point of an audit trail is that a compromised or buggy
agent cannot quietly rewrite its own history.

Explicitly out of scope for v1: multi-tenant key isolation, remote/HSM key
custody, and any network-facing secret service. Harwness is a
single-operator local agent harness; the threat model is "protect against
disk theft, casual `grep`, and accidental leakage into logs," not "protect
against a hostile co-tenant on shared infrastructure."

## 2. DEK/KEK envelope design

### 2.1 Cryptographic primitive

`harw-secrets` does not implement any cryptography itself. All AEAD sealing,
ML-KEM encapsulation, and HKDF key scheduling are delegated to `crypt_guard`
(crates.io, MIT, currently `2.0.3` — pin resolved via `cargo add crypt_guard`
at implementation time, not hand-typed here). `crypt_guard`'s safe path —
`Encryptor::<MlKemNNN, XChaCha20Poly1305>::new().recipient(pk).plaintext(b).seal()`
producing a self-describing CGv2 envelope `{ header, kem_ciphertext, nonce,
ciphertext }` — is exactly the primitive Harwness needs for "wrap a small
blob under a KEM-derived key," both for the DEK-wrapping step below and,
optionally, for direct small-secret sealing. `harw-secrets` never takes a
direct dependency on `ml-kem`, `ml-dsa`, `hkdf`, or `chacha20poly1305`; those
stay transitive through `crypt_guard`, matching the pattern `harw-provider`
already follows for `secrecy` (a thin, audited wrapper, not a bag of raw
primitive crates).

### 2.2 Two-key model

Each stored secret gets its own random **DEK** (data-encryption key), used
once via `crypt_guard`'s AEAD to seal that one secret's bytes. The DEK itself
is then wrapped ("sealed to") the deployment's **KEK** (key-encryption key),
an ML-KEM keypair held locally and never persisted in plaintext — only its
public half needs to be resident to seal, the secret half is required only to
unseal.  This is the standard envelope-encryption split: rotating the KEK
means re-wrapping every DEK, not re-encrypting every secret's ciphertext,
and compromise of one sealed record's ciphertext does not expose the KEK.

```text
seal:
  dek            = random 256-bit key
  ciphertext     = AEAD.seal(dek, nonce, plaintext)          // via crypt_guard
  kem_ciphertext = ML-KEM.encapsulate(kek_public, ...) -> wraps dek
                   (crypt_guard Encryptor::<MlKemNNN, _>::new()
                      .recipient(kek_public).plaintext(dek).seal())
  record         = { ciphertext, nonce, wrapped_dek: kem_ciphertext,
                     kem_algo, key_version }

unseal:
  dek        = ML-KEM.decapsulate(kek_secret, wrapped_dek)   // via crypt_guard
  plaintext  = AEAD.open(dek, nonce, ciphertext)
  dek zeroized immediately after use
```

Because `crypt_guard`'s `Encryptor`/`Decryptor` already emit/consume a
self-describing CGv2 envelope (header carries `kem_id`/`kdf_id`/`aead_id`),
the DEK-wrapping step is literally one `crypt_guard` seal call with the
6-byte-or-so DEK as its plaintext and the KEK's ML-KEM public key as
recipient — `harw-secrets` does not hand-roll a second envelope format for
the wrap step, it reuses CGv2 for both layers (secret-under-DEK,
DEK-under-KEK) and stores the DEK-wrapping CGv2 envelope verbatim in
`wrapped_dek`.

### 2.3 Typed Rust sketch

```rust
// harw-secrets/src/id.rs
//! Opaque identifiers. Never derive Display/Debug that would leak the
//! wrapped uuid alongside secret content in a shared log line by accident —
//! these are safe to log; the *record* types below are not.

use uuid::Uuid;

/// Identifies one stored secret across its lifetime, independent of key
/// rotation (the same `SecretId` persists through every `rotate`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct SecretId(Uuid);

impl SecretId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

/// Monotonic generation counter for a deployment's KEK. Bumped on every
/// rotation; every `SecretRecord` remembers which generation wrapped its DEK.
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub struct KeyVersion(pub u32);
```

```rust
// harw-secrets/src/policy.rs
//! Crypto preset selection. Mirrors the "all levels usable, one sensible
//! default" stance: an operator on constrained hardware may reasonably pick
//! ML-KEM-512; a compliance-driven deployment may require ML-KEM-1024. The
//! crate never hardcodes a single allowed level.

/// Selects the ML-KEM security level and AEAD used for new envelopes.
/// Existing envelopes keep decrypting under whatever `kem_algo` they recorded
/// at seal time — `CryptoPolicy` only governs *new* seals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KemAlgo {
    MlKem512,
    MlKem768,
    MlKem1024,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AeadAlgo {
    XChaCha20Poly1305,
    AesGcmSiv,
}

/// A named preset bundling KEM level + AEAD choice for new seals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CryptoPolicy {
    pub kem: KemAlgo,
    pub aead: AeadAlgo,
}

impl CryptoPolicy {
    /// Highest supported security level: ML-KEM-1024 + XChaCha20-Poly1305.
    /// This is the default for new deployments; callers may explicitly pick
    /// `ml_kem_512()` or `ml_kem_768()` when they have measured the
    /// performance trade-off and accept the lower margin.
    #[must_use]
    pub fn strongest() -> Self {
        Self { kem: KemAlgo::MlKem1024, aead: AeadAlgo::XChaCha20Poly1305 }
    }

    #[must_use]
    pub fn ml_kem_512() -> Self {
        Self { kem: KemAlgo::MlKem512, aead: AeadAlgo::XChaCha20Poly1305 }
    }

    #[must_use]
    pub fn ml_kem_768() -> Self {
        Self { kem: KemAlgo::MlKem768, aead: AeadAlgo::XChaCha20Poly1305 }
    }
}
```

```rust
// harw-secrets/src/record.rs
//! On-disk record shape. `SecretRecord` is what gets serialized to the
//! secrets store; `SecretMetadata` is the safe-to-log, safe-to-display
//! subset. Raw secret bytes and the DEK never appear in `SecretMetadata`,
//! never appear in a `Debug` impl, and never appear in an `AuditEvent`.

use jiff::Timestamp;

use crate::id::{KeyVersion, SecretId};
use crate::policy::{AeadAlgo, KemAlgo};

/// Full on-disk record for one sealed secret. Every byte field here is
/// ciphertext, a nonce, or a KEM-wrapped DEK — none of it is meaningful
/// without the KEK's secret half.
#[derive(Clone, Debug)]
pub struct SecretRecord {
    pub id: SecretId,
    /// AEAD ciphertext of the secret's plaintext bytes, sealed under the DEK.
    pub ciphertext: Vec<u8>,
    /// AEAD nonce used for `ciphertext` (crypt_guard generates and embeds
    /// this inside its own CGv2 envelope already, so in practice `ciphertext`
    /// stores the full CGv2 envelope and this field is populated only if a
    /// future non-crypt_guard AEAD path needs a bare nonce).
    pub nonce: Vec<u8>,
    /// CGv2 envelope produced by wrapping the random DEK under the
    /// deployment KEK's ML-KEM public key.
    pub wrapped_dek: Vec<u8>,
    pub kem_algo: KemAlgo,
    pub aead_algo: AeadAlgo,
    /// Which KEK generation wrapped `wrapped_dek`; used to detect records
    /// that still need re-wrapping after a rotation.
    pub key_version: KeyVersion,
}

/// Admin-visible metadata for a stored secret. Never contains raw secret
/// material, the DEK, or any derived key — only fields safe to render in a
/// `harw secrets list` table or to attach to an `AuditEvent`.
#[derive(Clone, Debug)]
pub struct SecretMetadata {
    pub id: SecretId,
    /// Human label, e.g. `"openai-api-key"`, `"telegram-bot-token"`.
    pub name: String,
    /// Free-text purpose, e.g. `"provider-auth"`, `"channel-auth"`.
    pub purpose: String,
    pub key_version: KeyVersion,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
```

### 2.4 Rotation

Rotation bumps `KeyVersion` and re-wraps every `SecretRecord.wrapped_dek`
under the new KEK — it never re-encrypts `ciphertext`, because the DEK (not
the KEK) protects the payload. Procedure:

1. Generate a new KEK keypair, store it as the *next* `KeyVersion` alongside
   the current one (old KEK secret half must remain available until rewrap
   completes, since it is needed to decapsulate old `wrapped_dek` values).
2. For every `SecretRecord`: decapsulate `wrapped_dek` under the old KEK,
   re-encapsulate the same DEK under the new KEK, write the new
   `wrapped_dek` and bump `key_version`. The DEK and `ciphertext` are
   untouched.
3. Once every record's `key_version` matches the new generation, the old KEK
   secret half may be safely destroyed. Until then it must be retained (a
   `harw doctor` check should flag "rotation in progress" state).

This mirrors why the DEK/KEK split exists in the first place: rotation cost
is O(number of secrets) and constant per secret (one decapsulate + one
encapsulate), never a bulk re-encryption of potentially large session
journals or knowledge artifacts sealed under the same scheme.

## 3. KEK provenance

The KEK's secret half must survive process restarts without ever touching
disk in plaintext, or must be re-derivable identically on every start. Three
options, compared:

| Option | Persistence | Headless/CI friendly | Restart-safe | Risk if disk is stolen |
|---|---|---|---|---|
| Env-provided seed | none (env var each start) | yes — ideal for CI | yes, as long as the seed is re-supplied | low (nothing on disk), but leaks via process env/`/proc`, shell history, orchestrator secrets |
| Key file (KEK secret bytes or a seed, restrictive perms) | file on disk, `0600`, owned by the running user | yes — no interactive prompt needed | yes | medium — file itself is the crown jewel; mitigated by filesystem perms + optional additional wrap layer |
| OS keyring (`keyring` crate: Secret Service / macOS Keychain / Windows Credential Manager) | OS-managed secure storage | no — most keyrings need an unlocked login session, awkward under `systemd --user` without a session or in a container | yes, when a session is available | low — OS enforces access control, but headless boxes commonly don't have an unlocked keyring |

**Recommendation for v1**: **key file** as the default provenance —
a KEK seed (32 bytes) stored at a configurable path (default
`~/.config/harwness/kek.seed` or equivalent XDG path), created with `0600`
permissions and refused to start if permissions are looser (`harw doctor`
checks this). Layered on top:

- **OS keyring support as an opt-in alternative** (`keyring` crate,
  already the standard Rust crate for this) for desktop deployments where a
  session is reliably present — selected via config, not auto-detected,
  so headless behavior stays predictable.
- **Env-provided seed as the CI/headless override** — if
  `HARWNESS_KEK_SEED` (or equivalent) is set, it takes precedence over the
  key file, so ephemeral CI runners never need a persisted file at all.

Key file wins the default slot because it is the only option that is both
restart-safe *and* headless-friendly without requiring the operator to wire
up a secrets-manager integration Harwness doesn't otherwise need. The
keyring path exists for operators who already keep their desktop keyring
unlocked and want one fewer file on disk; the env-seed path exists purely
for stateless CI/test runs and must be documented as "do not use for a
persistent deployment" (a seed passed via env is one `ps`/orchestrator-log
leak away from full compromise).

## 4. Audit layer

### 4.1 Event shape

```rust
// harw-secrets/src/audit/event.rs
//! Append-only audit event record. Records are immutable once written and
//! chained (see §4.2) so tampering with an old record is detectable even by
//! a party that only has read access to the log file.

use jiff::Timestamp;
use uuid::Uuid;

/// Identifies one audit event. UUIDv7 so IDs are naturally time-ordered,
/// which matters here: verification (§4.3) walks the chain in creation
/// order and a v7 ID lets tooling sanity-check ordering independent of the
/// hash chain itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct AuditEventId(Uuid);

impl AuditEventId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

/// Who or what performed the audited action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Actor {
    /// A human operator identified by their configured operator name.
    Operator(String),
    /// An agent acting autonomously within a session.
    Agent { session_id: String },
    /// The harness itself (startup, scheduled maintenance, key rotation).
    System,
}

/// A single reference to the entity an event acted upon — kept generic so
/// the audit layer does not need to know about every domain type in the
/// codebase (secret IDs, tool names, channel IDs, plugin names, ...).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubjectRef {
    /// Namespace of the referenced entity, e.g. `"secret"`, `"channel"`,
    /// `"plugin"`.
    pub kind: String,
    /// Stable identifier within that namespace.
    pub id: String,
}

/// One append-only audit record.
#[derive(Clone, Debug)]
pub struct AuditEvent {
    pub id: AuditEventId,
    pub actor: Actor,
    /// Dot-separated namespace.verb, e.g. `"secret.access"`,
    /// `"approval.grant"`, `"channel.command"`, `"plugin.install"`.
    pub action: String,
    pub subjects: Vec<SubjectRef>,
    pub recorded_at: Timestamp,
    /// SHA-256 of the previous event's canonical bytes (all-zero for the
    /// first event in a chain). See §4.2.
    pub prev_hash: [u8; 32],
}
```

### 4.2 Hash-chaining and checkpoints

No existing crate or reference project in scope provides tamper-evident
chaining for this shape of event log, so it is hand-rolled, kept small on
purpose:

```text
event[0].prev_hash = [0u8; 32]                       // genesis
event[n].prev_hash = SHA-256(canonical_bytes(event[n-1]))
chain_head_hash    = SHA-256(canonical_bytes(event[last]))
```

`canonical_bytes` is a fixed, deterministic serialization (field order
fixed, no map/HashMap types, lengths length-prefixed) — this is the one
place correctness matters most, since any two serializations of "the same"
event that hash differently break the chain. Appending an event means:
compute `prev_hash` from the current chain head, construct the new event,
write it, and only then advance the recorded chain-head hash — the write
must be atomic (write-temp-then-rename, matching the pattern already used
elsewhere in Harwness for durable file writes) so a crash mid-append cannot
leave a half-written record that silently breaks the chain.

Hash-chaining alone proves *internal consistency* (no record was altered or
reordered without recomputing every hash after it) but not that the whole
chain is genuine and complete relative to some earlier known-good point in
time — an attacker with enough
access could, in principle, delete the log and rebuild a new self-consistent
chain from scratch. To close that gap, `harw-secrets` periodically emits a
**signed checkpoint**: every N events (or every T minutes, whichever comes
first), the current chain-head hash is signed with an ML-DSA key (via
`crypt_guard`'s signing path) and the checkpoint `{ chain_head_hash,
event_count, signed_at, signature }` is written to a separate
append-only checkpoint file. Checkpoints are themselves chained
(`prev_checkpoint_hash`) so even the checkpoint file can be verified for
gaps. The ML-DSA signing key is a second, distinct key from the KEK (it
signs, it does not encrypt) and should be rotatable independently.

### 4.3 Verification procedure (`harw doctor` integration)

`harw doctor` gains an audit-integrity check that:

1. Reads the event log front-to-back, recomputing `SHA-256(canonical_bytes(event[n-1]))`
   and comparing against `event[n].prev_hash` for every adjacent pair —
   any mismatch is reported with the offending event ID and position.
2. Reads the checkpoint file, verifies each checkpoint's ML-DSA signature
   against the configured verification key, and confirms the checkpoint's
   recorded `chain_head_hash` matches the hash actually reached by replaying
   the event log up to `event_count` events.
3. Confirms checkpoint `event_count` values are monotonically increasing and
   that no checkpoint references an `event_count` beyond the current log
   length (which would indicate log truncation after the checkpoint was
   signed).
4. Reports a clear pass/fail per check, not just an aggregate boolean —
   an operator investigating "is my history intact" needs to know *which*
   segment broke, not just that something did.

### 4.4 What gets audited

At minimum, one `AuditEvent` per:

- Approval decisions (`"approval.grant"`, `"approval.deny"`) — subject:
  the tool call or action being approved.
- Channel commands received and acted upon (`"channel.command"`) — subject:
  the channel and command name, never the full message body (that belongs
  in the session journal, not the audit log).
- Plugin installs and removals (`"plugin.install"`, `"plugin.remove"`) —
  subject: plugin name and source.
- Secret access (`"secret.access"`, `"secret.rotate"`, `"secret.create"`,
  `"secret.delete"`) — subject: `SecretId`/`SecretMetadata.name`, **never**
  the unsealed value.

Audited actions never include raw secret bytes, full message content, or
anything that would itself need to be a secret — the audit log is designed
to be shareable with an auditor without additional redaction.

## 5. Crate integration

### 5.1 Dependencies

```toml
[dependencies]
crypt_guard = "2"          # resolve exact current version via `cargo add crypt_guard`
secrecy      = "0.8"        # match harw-provider's existing version
uuid         = { version = "1", features = ["v7"] }
jiff         = "0.1"        # or current — resolve via `cargo add jiff`
harw-macros  = { path = "../harw-macros" }
```

No direct dependency on `ml-kem`, `ml-dsa`, `slh-dsa`, `hkdf`, or
`chacha20poly1305`/`aes-gcm-siv` — those stay transitive through
`crypt_guard`, exactly as `crypt_guard` itself keeps them behind its own
feature flags. `harw-secrets` enables `crypt_guard`'s `ml-kem-backend` and
`ml-dsa-backend` features (both already crypt_guard defaults) and nothing
from the `legacy-pqclean` path.

### 5.2 Interaction with `secrecy`

`crypt_guard`'s DEK, and any plaintext handed to `Encryptor::plaintext(...)`,
must be built from `secrecy::SecretBox<[u8]>` / `Zeroizing<Vec<u8>>` at the
`harw-secrets` boundary, converted to a bare `&[u8]` only for the instant of
the `crypt_guard` call. Unsealed secret values returned to callers (e.g. to
`harw-provider::auth`) come back as `secrecy::SecretString` /
`secrecy::SecretBox<[u8]>`, matching the type `harw-provider` already uses,
so no crate downstream of `harw-secrets` ever holds a bare `String`/`Vec<u8>`
secret. `crypt_guard`'s own unconditional zeroize behavior covers the DEK and
any intermediate shared-secret material inside the `seal`/`open` call; the
`secrecy` wrapper covers the plaintext before and after that call.

### 5.3 Error enum sketch

```rust
// harw-secrets/src/error.rs
use harw_macros::HarwError;

use crate::id::{KeyVersion, SecretId};

#[derive(Debug, HarwError)]
pub enum SecretsError {
    #[msg("secret '{id:?}' not found")]
    NotFound { id: SecretId },

    #[msg("secret '{id:?}' already exists")]
    AlreadyExists { id: SecretId },

    #[msg("KEK provenance '{kind}' unavailable: {reason}")]
    KekUnavailable { kind: String, reason: String },

    #[msg("KEK secret file at '{path}' has unsafe permissions {mode:o}, refusing to start")]
    UnsafeKeyFilePermissions { path: String, mode: u32 },

    #[msg("record for '{id:?}' references key_version {found:?} but rotation to {expected:?} is in progress")]
    RotationIncomplete { id: SecretId, found: KeyVersion, expected: KeyVersion },

    #[msg("crypt_guard sealing failed: {0}")]
    Seal(#[from] crypt_guard::error::CryptError),

    #[msg("crypt_guard unsealing failed: {0}")]
    Open(crypt_guard::error::CryptError),

    #[msg("i/o error accessing secret store: {0}")]
    #[from]
    Io(std::io::Error),
}

#[derive(Debug, HarwError)]
pub enum AuditError {
    #[msg("hash chain broken at event {index}: expected prev_hash {expected:?}, found {found:?}")]
    ChainBroken { index: u64, expected: [u8; 32], found: [u8; 32] },

    #[msg("checkpoint at event_count {event_count} has invalid signature")]
    InvalidCheckpointSignature { event_count: u64 },

    #[msg("checkpoint references event_count {referenced} beyond log length {actual}")]
    CheckpointBeyondLog { referenced: u64, actual: u64 },

    #[msg("checkpoint sequence is not monotonically increasing at index {index}")]
    NonMonotonicCheckpoint { index: u64 },

    #[msg("i/o error accessing audit log: {0}")]
    #[from]
    Io(std::io::Error),
}
```

(`Seal`/`Open` cannot both carry `#[from] crypt_guard::error::CryptError`
in the same enum — only one `#[from]` per source type is legal, matching
`harw-macros`'s existing single-`From`-per-inner-type rule seen in
`harw-provider::error::ProviderError`. The implementation will pick one of
the two to be the `#[from]` target and construct the other explicitly via
`.map_err(SecretsError::Open)`.)

## 6. Explicit non-adoption

The following are explicitly **not** part of this design, regardless of how
similar the reference material may look:

- **No secureHUB deployment.** `harw-secrets` is not secureHUB, does not run
  as a service, and has no HTTP surface.
- **No `securehub-*` crate dependencies.** `securehub-domain`,
  `securehub-error`, and any other `securehub-*` crate are studied here as a
  *reference for problem shape* (how one other in-house project modeled
  secret metadata and audit events) — no code, type, or trait from them is
  reused, imported, or path-dependency-linked. Every type in §2–§4 above is
  an original Harwness type with its own fields and its own semantics (e.g.
  `SecretMetadata` here has no `organization_id`/multi-tenant fields at all,
  because Harwness is single-operator).
- **No Postgres, no row-level security, no HTTP secret service.** Storage is
  local files (secret store + audit log + checkpoint file) under the
  Harwness config directory, consistent with the rest of Harwness's
  filesystem-first storage model (see `docs/design/knowledge-surfaces.md`
  §1 for the sibling filesystem-based design).
  Nothing here talks to a database or a network service.
- **No `sgh-flow` coupling of any kind.** No shared crate, no shared wire
  format, no shared identifiers, no assumption that Harwness and sgh-flow
  ever run in the same process or exchange data. The `crypt_guard` dependency
  is a coincidental shared *library* dependency (both projects use the same
  published crate for PQC sealing), not an architectural coupling between
  the two projects.

## 7. Open questions

- **Checkpoint signing key custody**: should the ML-DSA checkpoint-signing
  key share KEK provenance (§3) or get its own, independently rotatable
  provenance? Leaning toward independent, since compromising the KEK
  (decrypt secrets) and compromising the signing key (forge audit history)
  are different failure modes with different blast radii — but this needs a
  decision before `harw-secrets` v1 locks its config schema.
- **Session-journal/knowledge-artifact encryption granularity**: per-file
  envelope (simplest, matches §2's per-secret DEK model) versus per-session
  DEK shared across many journal entries (fewer KEM operations, but a wider
  blast radius per compromised DEK and a fuzzier rotation story). This
  document assumes per-file for consistency with §2 but the performance
  trade-off has not been measured against realistic session-journal write
  volumes.
- **`keyring` crate feature selection across platforms**: the `keyring`
  crate's default backends differ meaningfully between Linux (Secret
  Service via D-Bus, which needs a running session) and headless Linux
  (frequently absent) — needs a concrete `harw doctor` check that detects
  "keyring configured but unavailable" and fails loudly rather than silently
  falling back to a less secure path.
- **Canonical serialization format for `canonical_bytes` (§4.2)**: candidates
  are a hand-rolled fixed-order binary encoding versus a constrained
  deterministic subset of an existing format (e.g. CBOR with sorted map
  keys forbidden by construction via using only structs/vecs, never maps).
  Needs a decision paired with a test that fails if a future field addition
  accidentally changes the hash of old, already-written events.
- **Rotation atomicity across many `SecretRecord`s**: §2.4 describes
  per-record rewrap but does not yet specify crash-recovery behavior if the
  process dies mid-rotation with some records on the old `key_version` and
  some on the new one — likely needs a rotation-in-progress marker file, but
  the exact recovery procedure is not yet designed.
