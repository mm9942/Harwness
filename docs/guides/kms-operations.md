# KMS operations: the Auth/Crypto Hub

> **Status:** partially implemented. The hub, the client, V3 secret records
> and the DoD signing policy are implemented. Persistent key storage is not:
> keys are in memory only (see [section 9](#9-persistence)). Last reviewed
> 2026-09-27.

This guide is for operators who run `harw-auth-hub`, the local key
management service (KMS) of a Harwness host. It covers installation, the
config file, the key lifecycle, migrating secrets to KMS-wrapped records,
signing, and how to read the hub's status codes. The code is
authoritative; the sources are listed at the end.

## 1. Architecture

```text
 harw process (SecretStore)             DoD components (signing)
   │ AuthHubDekWrapper                     │ harw-dod-encrypt: SignTranscript,
   │ (harw-infra-client)                   │ sign_operation, HarwUsageAuthorizer
   ▼                                       ▼
 AuthHubClient ── HTTP/1 over AF_UNIX: /run/harw/infra/secure.sock
                                           │
                                  harw-auth-hub (user harw-auth)
                                   ├─ SO_PEERCRED uid → principal
                                   │   (bearer token as fallback)
                                   ├─ NamespacePolicy (deny by default)
                                   ├─ version guard (rotate CAS, primary-only wrap)
                                   └─ key store (in memory today)
```

| Part | Crate | Role |
|---|---|---|
| Hub | `harw-auth-hub` | Daemon that owns all key material. It serves the CryptGuard 3.1 KMS routes (`/v1/keys…`, `CGK1` frames) and three JSON meta routes. There is no export route: private keys cannot leave the process through the API. |
| Client | `harw-infra-client` | `AuthHubClient`, the only code that opens the socket. `AuthHubDekWrapper` plugs the hub into `harw-secrets` as a DEK wrapper. |
| Secrets V3 | `harw-secrets` | `KmsWrappedV3` records: the payload is encrypted locally under a fresh per-secret data encryption key (DEK); only the DEK is sent to the hub to be wrapped. The Harw process no longer needs the long-term KEK seed. |
| DoD encrypt policy | `harw-dod-encrypt` | The `harw.<purpose>/<owner>` key naming, the purpose × operation usage matrix, grant roles (`Owner`, `Peer`, `Admin`) and the canonical `SignTranscript` format. |

Callers are identified by the kernel (`SO_PEERCRED` uid), not by anything
they send. Everything reachable on the socket is protected by the socket
permissions (`0660 harw-auth:harw-secure`) and the uid mapping.

## 2. Install and enable

The units ship in `deploy/systemd/` and are embedded in `harw-install`
(`harw install --print-systemd harw-auth-hub.service` prints the shipped
text). The user `harw-auth` and the group `harw-secure` come from
`deploy/sysusers.d/harw.conf`; `/run/harw/infra` (`0755 root:root`) comes
from `deploy/tmpfiles.d/harw.conf`.

The hub is **socket-activated only**:

- `harw-auth-hub.socket` listens on `/run/harw/infra/secure.sock`
  (`SocketUser=harw-auth`, `SocketGroup=harw-secure`, `SocketMode=0660`,
  exactly one `ListenStream=`).
- `harw-auth-hub.service` runs `/usr/bin/harw-auth-hub --systemd-socket` as
  `harw-auth`. It has no `[Install]` section. With `--systemd-socket` the
  daemon takes the single descriptor from `LISTEN_PID`/`LISTEN_FDS` and
  fails hard if it is missing; it never falls back to binding a path.

```sh
systemd-sysusers
systemd-tmpfiles --create
# write /etc/harw-auth-hub/config.toml first (section 3)
systemctl enable --now harw-auth-hub.socket      # or: harw-infra.target
```

Enable the socket (or `harw-infra.target`, which wants all four infra
sockets), never the service. The service starts on the first connection.

Client processes connect only if their user is `harw-auth` or a member of
`harw-secure`. Group membership lets a process *connect*; what it may *do*
is decided by the config file.

The service is hardened (`ProtectSystem=strict`, `RestrictAddressFamilies=AF_UNIX`,
no capabilities, `StateDirectory=harw-auth-hub` with mode `0700`,
`UMask=0077`). A remote transport is a separate, explicit profile; do not
widen this unit.

`SIGTERM`/`SIGINT` stop accepting, let open connections finish their
current request (10 s drain limit) and exit. **With the in-memory store,
every stop or restart deletes all keys** (section 9).

## 3. Configuration

Path: `/etc/harw-auth-hub/config.toml` (the unit passes no `--config`, so
the default applies). The file is strict TOML: unknown fields and unknown
op groups are errors, so a typo cannot silently widen or narrow a grant.

```toml
# socket_path = "/run/harw/infra/secure.sock"  # ignored under socket activation

[[peers]]
uid = 991                           # SO_PEERCRED uid of the harw process
principal = "harw-secrets"
grants = [
  { namespace = "secrets", ops = ["encrypt", "secret-egress"] },
]

[[peers]]
uid = 992
principal = "harw-kms-admin"
grants = [
  { namespace = "secrets", ops = ["admin", "read-public"] },
]

[[bearer_tokens]]                   # optional fallback, see below
principal = "ops-admin"
token_file = "/etc/harw-auth-hub/tokens/ops-admin.token"
grants = [ { namespace = "secrets", ops = ["admin", "read-public"] } ]
```

### Peers and grants

- `uid` values must be unique. `principal` is 1–128 bytes of
  `[A-Za-z0-9._@-]`.
- Each grant gives `ops` in one `namespace`; `ops` must not be empty.
- At least one peer or bearer token must be configured, otherwise startup
  fails ("every request would be rejected").

| `ops` group | Operations |
|---|---|
| `read-public` | describe, public key, verify |
| `encrypt` | encrypt, wrap |
| `secret-egress` | decrypt, unwrap. Grant deliberately: this returns plaintext keys. |
| `sign` | sign |
| `rewrap` | rewrap |
| `admin` | generate, rotate, disable, enable, destroy |
| `all` | every operation |

Keep users of keys and administrators of keys apart: the process that
wraps and unwraps DEKs needs `encrypt` + `secret-egress`, not `admin`. The
`admin` group has no `describe`; add `read-public` so an operator can
check the outcome of a lifecycle call.

A principal without a grant for a namespace gets `404`, not `403`, so keys
cannot be enumerated.

### Keys under `harw.*` namespaces

`harw-dod-encrypt` defines the naming `harw.<purpose>/<owner>[@version]`
(for example `harw.node-identity/<node>`, `harw.secrets-kek/<owner>`,
`harw.artifact-signing/<owner>`), a usage matrix per purpose, and three
grant roles:

| Role | Receives |
|---|---|
| `Owner` | private-key operations and public reads, never lifecycle |
| `Peer` | describe, public key, verify, encrypt, wrap; never secret egress, never sign |
| `Admin` | lifecycle plus describe; never uses the key |

`namespace_policy_for` and `HarwUsageAuthorizer` build and enforce these
rules inside a CryptGuard service stack. **The hub's config file does not
apply them by itself:** its grants are the op groups above. When you grant
a `harw.*` namespace in the hub config, write grants that match the role
(for example `Peer` = `["read-public", "encrypt"]`), and never grant
`sign` or `secret-egress` to a principal that is not the key's owner.

### Bearer-token fallback

Order of authentication for every request:

1. The peer's uid is listed under `[[peers]]` → that principal.
2. Otherwise, if bearer tokens are configured and the request carries
   `Authorization: Bearer <token>` → that token's principal (constant-time
   comparison).
3. Otherwise `401`.

Use tokens only for principals that cannot be tied to a uid (for example an
operator tool run from a varying account). A listed uid always wins over a
token. On the client side, `InfraClientConfig.token_file` sends the token;
that file must have no "other" permission bits.

### File permissions

| File | Rule (checked at startup) |
|---|---|
| `config.toml` | Not writable by group or others (e.g. `0640 root:harw-auth` or `0600 harw-auth`). Must be readable by `harw-auth`. |
| each `token_file` | Absolute path; regular file, not a symlink; no group or other bits (`0600` or `0400`), owned by `harw-auth` so the hub can read it; 16–4096 bytes of printable ASCII without whitespace, one trailing newline allowed. |

Token bytes are held in zeroizing memory and never logged. An error about
a token file never contains the token.

To generate a token:

```sh
install -d -m 0700 -o harw-auth -g harw-auth /etc/harw-auth-hub/tokens
head -c 32 /dev/urandom | base64 | tr -d '\n=' \
  > /etc/harw-auth-hub/tokens/ops-admin.token
chown harw-auth:harw-auth /etc/harw-auth-hub/tokens/ops-admin.token
chmod 0400 /etc/harw-auth-hub/tokens/ops-admin.token
```

Config and tokens are read once at start. After a change, restart the
service, which (today) also deletes all keys.

## 4. Talking to the hub

The meta routes answer JSON and need no principal: anyone allowed to
connect may read them.

```sh
curl -s --unix-socket /run/harw/infra/secure.sock http://hub/v1/health
curl -s --unix-socket /run/harw/infra/secure.sock http://hub/v1/version
curl -s --unix-socket /run/harw/infra/secure.sock http://hub/v1/capabilities
```

`/v1/capabilities` reports `persistence` (`"in-memory"` or
`"sealed-file"`), the `crypto_profiles` (`pq-hpke-default`, `ml-dsa-44`,
`ml-dsa-65`, `ml-dsa-87`), `store_epoch` and `boot_id`. These values
describe the hub; they are never used as authority.

Key routes (`/v1/keys/{ns}/{id}[@v]…`) take and return `CGK1` binary
frames, not JSON. Programs use `AuthHubClient`. The lifecycle calls
`rotate`, `disable`, `enable` and `destroy` have an empty request body, so
an operator can issue them with curl while running as a configured uid (or
with a bearer token); the response is a `CGK1` frame:

```sh
sudo -u <admin-user> curl -s -o /dev/null -w '%{http_code}\n' -X POST \
  --unix-socket /run/harw/infra/secure.sock \
  'http://hub/v1/keys/secrets/dek-kek@2:disable'
```

Every response carries `Cache-Control: no-store`; crypto responses also
carry `x-request-id`, which appears in the audit log.

## 5. Key lifecycle

### Version numbering

CryptGuard key versions start at 1. `harw-secrets` generations start at 0.
The fixed rule is **hub version = Harwness generation + 1**
(`crypt_guard_key_version`). An `AuthHubDekWrapper` built for generation
`g` always addresses `namespace/id@(g+1)`.

### Generate

`POST /v1/keys` (client: `generate_key`) creates version 1. Needs `admin`.
Generate is idempotent for the same `(namespace, id, algorithm)`: if
version 1 still exists, is enabled and has that algorithm, a repeated
generate returns the same key instead of `409`. A different algorithm, a
key that has already been rotated, or a disabled/destroyed version 1 gives
`409`.

### Rotate with a pinned version

Rotation is a compare-and-set. Always send the version you expect to be
the current primary:

```text
POST /v1/keys/secrets/dek-kek@2:rotate     → creates @3, only if @2 is primary
```

- If `@2` is still the primary, the hub creates `@3`.
- If it is not (someone else rotated, or your earlier attempt already
  succeeded and the response was lost), the hub returns `409` and creates
  nothing. **Do not retry blindly: `describe` the key to learn the
  outcome.**
- An unpinned `…:rotate` is forwarded unchecked ("rotate latest") and is
  not retry-safe. `AuthHubClient::rotate` never sends it: with an unpinned
  key it describes first and pins the current primary.

After a `Timeout` or `503` on generate or rotate the outcome is unknown.
Describe before trying again.

### Wrap only on the primary

`encrypt`, `wrap` and the target of `rewrap` are accepted only for the
unpinned key or the pinned **current primary**. A pinned older version gets
`409`. So a process that still runs on an old generation cannot seal new
DEKs under a superseded key: `AuthHubDekWrapper` reports this as
`DekWrapperUnavailable` with the reason `key rotated; reload generation`.
Restart or reconfigure that process with the new generation.

### Unwrap old versions

`decrypt`, `unwrap` and the source of `rewrap` work under **any enabled**
version. Old data stays readable after a rotation as long as its version is
enabled and not destroyed.

Note the client side: an `AuthHubDekWrapper` serves exactly one
generation, and a `SecretStore` holds one wrapper. It refuses V3 records of
any other generation with `KeyGenerationMismatch` before calling the hub.
`SecretStore` has no path yet that re-wraps existing V3 records to a new
hub version. Until it does, rotating the hub key of a store that already
holds V3 records means the store configured for the new generation cannot
read the old records. Plan rotations of `secrets` keys accordingly.

### Disable, enable, destroy

| Call | Effect |
|---|---|
| `…@v:disable` | Version `v` becomes `Disabled`; every use of it returns `409`. Reversible. |
| `…@v:enable` | Back to `Enabled`. Not possible for a destroyed version (`409`). |
| `…@v:destroy` | Key material of `v` is dropped and zeroized immediately; a `Destroyed` tombstone remains. Irreversible. Needs an explicit version (unversioned → `400`). Using the version afterwards returns `404`. |

Destroying the current primary does not move the primary pointer: encrypt
and wrap under the key then fail with `404` until you rotate. Retire a
version in this order:

1. Rotate, and restart every writer on the new generation.
2. Re-seal or re-wrap all data that still uses the old version.
3. Disable the old version and watch for `409`s in the audit log for a
   while. Something still needing it shows up there, and you can re-enable.
4. Destroy it.

## 6. Migrating secrets to V3

New secrets are sealed as V3 (`KmsWrappedV3`) once a DEK wrapper is
configured (`SecretStore::with_dek_wrapper`). Existing V1/V2 records are
**never** rewritten implicitly, not even at startup. Migration is the
explicit call `SecretStore::migrate_all_to_v3()`.

Preconditions:

- A DEK wrapper is configured (otherwise `DekWrapperUnavailable`, nothing
  changes).
- The local KEK material is supplied (`open_with_key_material`), because
  V1/V2 records must be opened (otherwise `KekUnavailable`).
- Every V1/V2 record is at the store's current KEK generation. Finish any
  local KEK rotation first (otherwise `RotationIncomplete`).
- The hub key exists at the wrapper's pinned version and is the primary,
  and the calling principal has `encrypt` and `secret-egress` on its
  namespace.
- **The hub keeps its keys.** With today's in-memory hub, a restart after
  migration makes every V3 record permanently unreadable. Do not migrate
  secrets you cannot recreate until the hub persists its keys (section 9).

What it does:

- V2 records: the DEK is re-wrapped through the hub; payload ciphertext and
  nonce stay byte-identical.
- V1 (legacy direct HPKE) records: the payload is re-encrypted under a
  fresh DEK.
- V3 records are left untouched, so the call is idempotent.
- It is transactional: the whole replacement directory is staged and
  promoted, then the `secret.migrate_v3` audit event is persisted. Any
  failure leaves memory and disk unchanged. With nothing to migrate,
  nothing is written.

The returned `V3MigrationReport` counts `migrated_v1`, `migrated_v2` and
`already_v3`. Back up the secrets directory before the first run.

A later local KEK rotation (`SecretStore::rotate`) skips V3 records: their
DEKs belong to the hub key's lifecycle.

## 7. Signing only through `SignTranscript`

A KMS that signs whatever bytes it receives is a *signing oracle*: anyone
who can reach it can get a valid signature on a forged handshake, audit
checkpoint or artifact manifest, because the signature does not say what
it was meant for. Harwness prevents this with canonical transcripts
(`harw-dod-encrypt`, Masterplan v2 §5):

- A `SignTranscript` starts with the magic `HARWSIG\0`, a format version,
  the `SignPurpose` code and its domain label (e.g.
  `harw:node-handshake:v1`), followed by length-prefixed, tagged fields.
  There is no constructor from raw bytes.
- `cg::sign_operation` is the only way the crate builds a sign request. It
  refuses a transcript whose purpose is not the key's purpose, and any
  transcript larger than `MAX_SIGNABLE_TRANSCRIPT_LEN` (60 896 bytes, so
  sign and verify both fit CryptGuard's 64 KiB body limit). Larger data,
  such as secure frames with a big ciphertext, must be signed locally.
- `verify_operation` applies the same checks, so a signature cannot be
  "re-purposed" by a verifier that picked the wrong key.
- `HarwUsageAuthorizer` enforces the same binding on the service side for
  every `Sign`/`Verify` on a `harw.*` namespace, however the request was
  built: raw bytes, a foreign purpose or an oversized transcript are
  `Forbidden`.

Operator rules that follow from this:

- Grant `sign` only on `harw.*` signing namespaces, and only to the key's
  owner principal.
- Never grant `sign` (or `all`) on a non-`harw.*` namespace to a principal
  that other code can drive: there is no transcript check there.
- Keep signing keys and encryption keys in different namespaces, as the
  `harw.<purpose>` naming does.

## 8. Store epoch: "key store replaced"

`/v1/version` and `/v1/capabilities` report two 128-bit ids (32 lowercase
hex digits):

| Field | Changes when |
|---|---|
| `store_epoch` | The key store is **created**. A persistent store keeps it across restarts. The in-memory store is new in every process, so today it changes on every hub restart. |
| `boot_id` | Every hub process start. |

Why it matters: the hub answers an unwrap under a key version it does not
have with `422`, the same status as a tampered blob or wrong AAD (the
service deliberately does not say which). After an in-memory restart every
unwrap would therefore look like tampering.

`AuthHubDekWrapper` records `store_epoch` on its first wrap or unwrap. On an
unwrap `422` it reads `/v1/version` again:

- epoch changed → `DekWrapperUnavailable` with *"AuthHub key store was
  replaced (epoch changed); keys are not available"*;
- epoch unchanged, not reported, or not readable → an authentication error
  (`Open(AuthenticationFailed)`), i.e. real tampering or a wrong record.

Both outcomes fail closed. **"Key store replaced" means the keys that
wrapped your V3 DEKs no longer exist in this hub.** Retrying will not help.
The data is recoverable only if a store with the old epoch can be brought
back (not possible with the in-memory store). Recreate the affected
secrets.

Reading the two ids together:

| `store_epoch` | `boot_id` | Meaning |
|---|---|---|
| same | same | same process |
| same | changed | restart of a persistent store: keys are intact |
| changed | changed | new store: all earlier keys are gone |

## 9. Persistence

**Today: in memory only.** `/v1/capabilities` reports
`"persistence": "in-memory"`. All keys are lost when the hub process exits,
restarts, is stopped by systemd, or crashes (`Restart=on-failure` starts a
fresh, empty store). Do not use the hub for keys whose loss is not
acceptable.

**Coming with R14: a sealed, persistent provider.** The intended design,
described generically (the concrete config keys and flags are not final;
do not configure anything from this section yet):

- Key material is persisted as a store file that is encrypted and
  authenticated with a key-encryption key kept **outside** the store file.
- The store file lives under the unit's `StateDirectory`
  (`/var/lib/harw-auth-hub`, mode `0700`, owner `harw-auth`).
- The store keeps its `store_epoch` across restarts, and
  `/v1/capabilities` reports `"persistence": "sealed-file"`.
- Destroyed versions stay as tombstones without key material.

Until R14 has landed and `persistence` reports `sealed-file`, treat every
hub restart as the loss of all keys.

## 10. Audit

Tracing target `harw_auth_hub::audit` (filter with `RUST_LOG`; read with
`journalctl -u harw-auth-hub.service`):

- `event="crypto"` per operation: `request_id` (= `x-request-id`),
  `principal`, `op`, `key`, `target_key` (rewrap), `created_version`,
  `outcome` (`ok` or the error class; denials at `WARN`).
- `event="http"` per request: `peer_uid`, `peer_pid`, `method`, `path`,
  `status`, including requests rejected before the key store (401,
  unknown route 404, 413).

Never logged: plaintext, key material, wrapped blobs, `info`/`aad`, bodies,
bearer tokens, header values.

## 11. Troubleshooting

### HTTP status codes

| Status | Meaning | What to do |
|---|---|---|
| `200` | Success. | — |
| `400` | Malformed request: bad key path, bad `CGK1` frame, non-empty body on a lifecycle call, `destroy` without a version. | Fix the caller. |
| `401` | The peer uid is not under `[[peers]]` and no valid bearer token was sent. | Check `peer_uid` in the `event="http"` audit line against the config. |
| `403` | Rare: CryptGuard hides "forbidden" as `404` by default. | — |
| `404` | Unknown route; unknown key or version; destroyed version; destroyed primary; **or the principal has no grant for this namespace/op**. | Check the grants first, then `describe` the key with an admin principal. |
| `405` | Wrong HTTP method (e.g. `POST` on a meta route). | Fix the caller. |
| `409` | Key state or version conflict: disabled version; pinned rotate whose version is no longer the primary; encrypt/wrap pinned to an old version (`key rotated; reload generation`); generate of an existing key with another algorithm or after rotation; destroy/enable of a destroyed version. | For rotate: `describe`, never retry blindly. For wrap: move the writer to the current generation. |
| `413` | Body too large (e.g. a sign transcript above the 64 KiB limit). | Sign large data locally. |
| `422` | Authentication failed: tampered wrapped blob or ciphertext, wrong `aad`/`info`, **or** a key version this store does not have. | Compare `store_epoch` with the value from before. Changed: key store replaced (section 8). Unchanged: treat as tampering. |
| `500` | Internal error. | Check the journal. |
| `501` | Operation not supported for this key type (e.g. sign with an HPKE key, wrap with an ML-DSA key). | Use the right key type. |
| `503` | Overloaded or unavailable (queue full, worker gone, request timeout); carries `Retry-After`. | Retry reads. After a mutation, `describe` first: it may have committed. |

The client maps these to `InfraClientError`: `Unauthenticated` (401),
`Forbidden` (403), `NotFound` (404), `Unavailable` (socket missing,
connection refused, or 503), `Timeout`, `Protocol` and
`Remote(Conflict | AuthenticationFailed | …)`.

### Startup and connection failures

| Symptom | Cause |
|---|---|
| Service exits at start: insecure permissions on the config | `config.toml` is group- or world-writable. |
| Service exits: token file error | Token file is a symlink, not a regular file, has group/other bits, or is not 16–4096 printable, whitespace-free bytes. |
| Service exits: "no peers and no bearer_tokens configured" | Empty config. |
| Service exits: config parse error | Unknown field, unknown op group, or a typo. |
| Service exits: config invalid | Duplicate uid or token file, relative path, invalid principal or namespace, empty `ops`. |
| Service exits under systemd: socket error | Started without the socket unit, or with more than one listening descriptor. Enable `harw-auth-hub.socket`, not the service. |
| Client: `Unavailable`, "connection refused" / "permission denied" | Socket unit not running, or the client user is not `harw-auth` and not in `harw-secure`. |
| `V3` reads fail with `KeyGenerationMismatch` | The store's wrapper is on a different generation than the record (section 5). |
| `DekWrapperUnavailable: key store was replaced` | The hub restarted with a new in-memory store (section 8). |

## Sources

- `harw-auth-hub/README.md`, `src/config.rs`, `src/guard.rs`, `src/meta.rs`
- `harw-infra-client/README.md`, `src/auth.rs`, `src/dek.rs`
- `harw-secrets/src/dek_wrapper.rs`, `src/store.rs` (`migrate_all_to_v3`)
- `dod/crates/harw-dod-encrypt/src/cg.rs`, `src/transcript.rs`,
  `src/policy.rs`, `src/purpose.rs`
- `deploy/systemd/harw-auth-hub.{socket,service}`,
  `deploy/sysusers.d/harw.conf`, `deploy/tmpfiles.d/harw.conf`
- [Crypto / infrastructure drift report](../architecture/crypto-drift-report.md)
  (socket layout D1/D2, principal mapping D5)
