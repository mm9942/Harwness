# harw-dod-encrypt

Harw-specific crypto semantics on top of CryptGuard's typed service layer
(`crypt_guard_service`). Wave **H2** of the Crypto Masterplan v2
(`docs/planning/30-crypto-infrastructure/Harwness_Crypto_Infrastructure_Masterplan_v2.md`,
§3, §4, §5, §8–§10, §20, §21, §36).

The crate holds meaning, not machinery: no KEM/AEAD/signature code, no key
provider, no Tower stack, no Hyper server, no key storage. Those belong to
CryptGuard and to the Auth/Crypto Hub (H3).

## What is in it

| Item | Purpose |
|---|---|
| `HarwKeyPurpose` | What a key is for (§4): `NodeIdentity`, `ServiceIdentity`, `DeviceIdentity`, `UserIdentity`, `SecretsKek`, `ChannelBinding`, `RequestAuthentication`, `AuditCheckpoint`, `ArtifactSigning`, `Pseudonymization`. Stable wire code, namespace `harw.<purpose>`, `KeyClass`. |
| `SignPurpose` | Structured signature purpose (§5), one per signing key purpose, each with a versioned domain label `harw:<name>:v1`. |
| `HarwCryptoProfile` | Pinned algorithm suites with stable two-byte ids (`0x01xx` ML-DSA, `0x02xx` PQ HPKE). |
| `KeyUsagePolicy` | The purpose x operation matrix (§5). `allows(purpose, HarwKeyOp)`, `authorize_sign(purpose, &SignTranscript)`. |
| `GrantRole`, `HarwGrant` | Owner / Peer / Admin roles, intersected with the matrix. |
| `SignTranscript` | Canonical, versioned, domain-separated, length-prefixed bytes to sign. No constructor from raw bytes. |
| `SecureFrameHeader` | `HarwSecureFrameV1` header (§8) with a strict parser. |
| `IdempotencyKey`, `UnixMillis` | Request metadata (§9). |
| `ReplayWindow`, `SenderReplayWindows` | 128-bit sliding bitmap per sender, check-before-commit, bounded sender count. |
| `cg` | **The only module that names CryptGuard types**: `key_ref`, `op_set`, `namespace_policy_for`, `sign_operation`, `verify_operation`, `generate_operation`, `key_algorithm`, `HarwUsageAuthorizer`. |

## Naming convention

```text
namespace  harw.<purpose-label>        e.g. harw.node-identity
key id     <owner>                     node, service, device or user name
version    @<n>                        optional; none = current primary
```

`harw.secrets-kek/vault-main@3` is version 3 of the KEK owned by `vault-main`.

## No signing oracle

`KeyUsagePolicy::allows(purpose, Sign)` only says whether a key signs at all.
What it signs is fixed by the transcript: a `NodeIdentity` key signs only
`harw:node-handshake:v1` transcripts, a `RequestAuthentication` key only
`harw:secure-frame:v1` transcripts, and so on. `cg::sign_operation` is the
only way this crate builds a CryptGuard sign request and checks this first.

`cg::HarwUsageAuthorizer` applies the matrix on the provider side to every
operation that touches a `harw.*` namespace. It cannot read sign payloads
(CryptGuard keeps them in `SecretBytes`), so the Hub (H3) must expose only
transcript-typed sign requests.

## Sign transcript format (v1)

```text
"HARWSIG\0" || 0x01 || purpose code (u8) || domain_len (u8) || domain
            || field_count (u16 BE)
            || { tag_len (u8) || tag || value_len (u32 BE) || value }*
```

## Secure frame header (v1)

```text
"HWSF" || version u16 || profile u16 || purpose u8 || key_version u32
       || key_id_len u8 || key_id || sender_len u8 || sender
       || sequence u64 || issued_at_ms u64 || idempotency_key [16]
       || payload_len u32                         (all big-endian)
```

The parser rejects a bad magic, an unknown version (checked right after the
magic), an unknown profile or purpose, a profile that does not fit the
purpose, a zero key version, an invalid name, an oversize payload length,
truncation and trailing bytes.

## Boundaries (enforced)

- Not re-exported by the `harw-dod` facade (§3.3): `compile_fail` doctest in
  `harw-dod/src/lib.rs`, manifest test in `harw-dod/tests/facade.rs`, and the
  forbidden edge `harw-dod -> harw-dod-encrypt` in `xtask/src/gate_edges.rs`.
- Never reachable from `harw-warden`, `harw-dod-warden*`, `harw-probe-fs`,
  `harw-probe-bpf` or any sensor crate (§20, §21): `xtask gates edges`
  forbids `crypt_guard`, `crypt_guard_core`, `crypt_guard_service`,
  `crypt_guard_hyper`, `hyper`, `tower` and `harw-dod-encrypt` in their
  normal-dependency hulls.

## Tests

`cargo test -p harw-dod-encrypt` (integration tests under `tests/`):
transcript determinism and domain separation, frame round trip and every
parse rejection, replay window, policy matrix, `cg` mapping and authorizer.
