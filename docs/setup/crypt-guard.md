# `crypt_guard` integration in `harw-secrets`

`harw-secrets` is Harwness's own secrets-storage crate; `crypt_guard` is a
third-party post-quantum HPKE crate it depends on. This document is a
worked example of how a dependency migration got documented in this
project: why `harw-secrets` uses the crates.io release of `crypt_guard`,
why plain ML-KEM in that release cannot be used for the deterministic
seed→KEK derivation this project needs, and what hybrid-KEM design that
implies for `policy.rs`/`kek.rs`/`envelope.rs`. It documents the current
state of `harw-secrets/src/{policy.rs,kek.rs,envelope.rs}`, not a
proposal.

`harw-secrets/Cargo.toml` currently pins `crypt_guard = "=3.0.2"`.

## 1. Why a crates.io release, not a path dependency

At an earlier point, `harw-secrets/Cargo.toml` depended on `crypt_guard`
through a path dependency pointing at a sibling checkout of the
`crypt_guard` repository. That path does not exist on a machine that
actually builds this workspace. A path dependency makes Cargo load the
target manifest as soon as the workspace is resolved — including for
`cargo metadata --no-deps --offline`, which fails on exactly that missing
file. That blocked `make check/clippy/tests/gates/build`, `xtask` and
every `cargo`-based workflow in the whole workspace, not just the crypto
paths in `harw-secrets`.

The fix was to depend on the published crates.io release instead. That is
the only reproducible source `deny.toml` can check: a registry version
with a `source`/`checksum` entry in `Cargo.lock`, without loosening
`[sources] unknown-git = deny`.

```toml
# harw-secrets/Cargo.toml
crypt_guard = "=3.0.2"
```

## 2. What is missing for plain ML-KEM: no seed→public-key path

There is no function in `crypt_guard` that derives a public key
deterministically from a seed for the *plain* ML-KEM `Kem` variants
(`MlKem512`, `MlKem768`, `MlKem1024`). The crate exposes, in
`hpke_pq::draft_ietf_hpke_pq_05_full` (re-exported via `pq_hpke`):

- `generate_recipient_key_pair(kem: Kem) -> Result<RecipientKeyPair, Error>`
  — generates the key pair **randomly** via `OsRng`; it takes no seed, so
  it cannot give a reproducible KEK derivation from a stored 32-byte seed.
- `RecipientPrivateKey::from_seed_bytes(kem: Kem, seed: &[u8]) -> Result<Self, Error>`
  — deterministically parses a seed into a private key, for **all** `Kem`
  variants (including plain ML-KEM-512/768/1024).
- `RecipientPrivateKey::public_key(&self) -> Result<RecipientPublicKey, Error>`
  — the **only** way to get the public key from a seed-derived private
  key. Its implementation:

  ```rust
  pub fn public_key(&self) -> Result<RecipientPublicKey, Error> {
      match self.kem {
          Kem::MlKem768P256 | Kem::MlKem1024P384 | Kem::MlKem768X25519 => {
              Ok(RecipientPublicKey { kem: self.kem, inner: RecipientPublicKeyInner::Hybrid(
                  HybridPublicKey::derive(self.kem, self.as_seed_bytes())?) })
          }
          _ => Err(Error::InternalInvariant),
      }
  }
  ```

  For the three hybrid `Kem` variants, the public key is derived from the
  seed (`HybridPublicKey::derive`). For **every other** `Kem` variant —
  that is, plain `MlKem512`, `MlKem768`, `MlKem1024` — the `_` arm applies
  and returns `Err(Error::InternalInvariant)`.

The reason lies in the underlying ML-KEM implementation (`libcrux_ml_kem`):
its FIPS-203 seed format encodes only the private key; the corresponding
public-key derivation from the plain seed is not exposed there (both
halves are only generated together via `OsRng`, with no path back from a
stored ML-KEM private-key seed to the public key). `crypt_guard` closes
this gap only for the three hybrid suites, whose classical component
(P-256/P-384/X25519) allows public scalar multiplication from the seed.

**Consequence:** with the crates.io release of `crypt_guard`, a public key
can no longer be derived from a 32-byte KEK seed for plain
ML-KEM-512/768/1024 — neither deterministically (no such function exists)
nor via `RecipientPrivateKey::public_key()` (which always fails with
`Error::InternalInvariant` for these three variants). A plain-ML-KEM path
would have required a separate, untested crypto derivation outside
`crypt_guard`, which was rejected in favor of the hybrid design below.

## 3. Decision: hybrid KEM as the default

`harw-secrets` uses the three hybrid-KEM suites from
`crypt_guard::pq_hpke::draft_ietf_hpke_pq_05_full::Kem`, because only for
these does `RecipientPrivateKey::public_key()` return a public key derived
from the stored seed:

| `KemAlgo` variant | `crypt_guard::pq_hpke::Kem` | serde name | draft KEM id |
|---|---|---|---|
| Default | `MlKem1024P384` | `ml_kem_1024_p384` | `0x0051` |
| Alternative | `MlKem768P256` | `ml_kem_768_p256` | `0x0050` |
| Alternative | `MlKem768X25519` | `ml_kem_768_x25519` | `0x647a` |

`MlKem1024P384` is the default in `CryptoPolicy::strongest()` — the
highest security margin, as a hybrid suite rather than plain ML-KEM.

The existing 32-byte KEK seed (key file, env seed and OS keyring all
supply exactly 32 raw bytes, see `kek.rs`) fits the hybrid requirement
unchanged: the crate defines a `HYBRID_SEED_BYTES: usize = 32` constant as
the seed length expected by the `HybridPrivateKey`/`HybridPublicKey` path.
No format change is needed for KEK provenance — only the `Kem` value
passed to `RecipientPrivateKey::from_seed_bytes` and then to
`.public_key()` changes from a plain to a hybrid variant.

## 4. Consequence: existing plain-ML-KEM envelopes are unreadable

Any envelope sealed under `KemAlgo::MlKem512`, `MlKem768` or `MlKem1024`
references a `Kem` variant for which `crypt_guard` no longer has a
seed→public-key path (§2). Opening such a legacy record with the current
code is therefore no longer possible, regardless of envelope format
(`dek_wrapped_v2` or `direct_hpke_v1`).

This is tracked as its own typed error rather than silently mapped onto
`SecretsError::KekDerivation`/`Open`, so callers can distinguish "this
envelope predates the hybrid switch" from a genuine authentication or I/O
failure:

```rust
/// A record references a plain ML-KEM `KemAlgo` variant
/// (`ml_kem_512`/`ml_kem_768`/`ml_kem_1024`), for which crypt_guard no
/// longer offers a seed→public-key derivation (only the three hybrid
/// suites). Legacy envelope, no longer openable.
#[msg("record '{id:?}' uses legacy KEM {kem:?}, unsupported since the crypt_guard hybrid-KEM switch")]
UnsupportedLegacyKem { id: SecretId, kem: KemAlgo },
```

**If old secrets already exist:** there is no automatic migration path —
without the old plain-ML-KEM seed the plaintext can no longer be decrypted,
because the derivation itself is missing (not just the wire format).
Affected deployments must **recreate** the secrets: fetch the old
plaintext again from its source (provider dashboard, password manager,
backup) and reseal it under the current hybrid `CryptoPolicy`.

## 5. Related documents

- `docs/setup/build-prerequisites.md` describes the other prerequisites
  needed to build the workspace (crates.io resolution, lockfile cleanup).
- `docs/design/crates-inventory.md` and `docs/design/secrets-and-audit.md`
  may still describe an older `crypt_guard` version or a plain-ML-KEM
  sketch; where they disagree with this document, this document reflects
  the actual `crypt_guard` integration.

## Notes on the implementation

- **Exact pin:** `harw-secrets/Cargo.toml` pins `crypt_guard = "=3.0.2"`.
  A silent update within the 3.x line could change key derivation and make
  all sealed secrets unreadable; updates are made deliberately, with a new
  known-answer test.
- **Domain separation per KEM:** a separate 32-byte seed is derived per
  hybrid KEM from the 32-byte KEK seed (`derive_kem_seed`, SHA-256 with a
  KEM-specific context, `harw-secrets/src/kek.rs`). This means
  `ml_kem_768_p256` and `ml_kem_768_x25519` do not share an ML-KEM key
  even from the same root seed. `derive_secret_key` returns the KEM's own
  seed, not the root seed.
- **Known-answer test (bless protocol):** the KAT in `kek.rs` compares
  against `harw-secrets/tests/fixtures/kem_kat.txt`. That file is
  generated, checked and committed once with
  `HARW_BLESS=1 cargo test -p harw-secrets`; without `HARW_BLESS`, a
  missing fixture fails the test with instructions.
- **Legacy records:** reading or rotating a legacy record returns
  `SecretsError::UnsupportedLegacyKem`; the CLI
  (`harw-cli/src/secret_store.rs`) shows a notice to recreate the secret.
  `rotate` uses each record's own stored KEM.
