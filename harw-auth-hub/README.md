# harw-auth-hub

Local Auth/Crypto Hub (Crypto Infrastructure Masterplan v2, §6.3 and wave
H3). A small daemon that owns key material and serves the CryptGuard 3.1 KMS
surface over Hyper HTTP/1 on an `AF_UNIX` stream socket. Callers are
identified by the kernel (`SO_PEERCRED`), not by anything they send.

> **Persistence: in-memory only.** Keys live in CryptGuard's
> `InMemoryProvider`. **All keys are lost when the process exits or
> restarts.** A sealed export/import of the key store is future work
> (MIG-rest). Do not use the hub for keys whose loss is not acceptable.

## Assembly

```text
AF_UNIX accept ── SO_PEERCRED → PeerCred{uid,gid,pid}
  └─ ConnectionService (per connection)
       ├─ GET /v1/health | /v1/version | /v1/capabilities
       └─ CryptoHttpService (crypt_guard_hyper) + PeerCredAuthenticator
            └─ NetworkHandle = Buffer(ConcurrencyLimit(CryptoService))
                 └─ AuditProvider → PolicyProvider<_, NamespacePolicy> → InMemoryProvider
```

Only the `NetworkHandle` (a channel sender) is cloned per connection; the
`CryptoService` and every key stay owned by one buffer worker. The reference
route table has no key-export operation: private key material cannot leave
the process through the API.

## Routes

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/v1/health` | JSON: `{"status": "ok", "service": "harw-auth-hub"}` |
| GET | `/v1/version` | JSON: `service`, `version`, `protocol`, `cryptguard`, `store_epoch`, `boot_id` |
| GET | `/v1/capabilities` | JSON: `service`, `protocol`, `crypto_profiles`, `operations`, `algorithms` (alias of `crypto_profiles`), `persistence` (`"in-memory"` \| `"sealed-file"`), `transport`, `authentication`, `store_epoch`, `boot_id` |
| POST | `/v1/keys` | generate |
| GET | `/v1/keys/{ns}/{id}[@v]` | describe |
| GET | `/v1/keys/{ns}/{id}[@v]/public` | public key |
| POST | `/v1/keys/{ns}/{id}[@v]:{op}` | `encrypt` `decrypt` `sign` `verify` `rotate` `disable` `enable` `destroy` `wrap` `unwrap` `rewrap` |

`/v1/keys…` bodies use CryptGuard's `CGK1` frames
(`crypt_guard_hyper::codec`), not JSON. Algorithms: `pq-hpke-default`,
`ml-dsa-44`, `ml-dsa-65`, `ml-dsa-87`. Every response carries
`Cache-Control: no-store`; crypto responses also carry `x-request-id`.

The meta routes answer `application/json` and follow the §38 contract that
`harw-infra-client` (`src/info.rs`) parses: `protocol` is `1`
(`meta::PROTOCOL_VERSION`), and `crypto_profiles` lists the CryptGuard wire
names `KeyProfile::wire_name` uses (`pq-hpke-default`, `ml-dsa-44`,
`ml-dsa-65`, `ml-dsa-87`). The client ignores the extra fields.

### Store epoch and boot id

`store_epoch` and `boot_id` (both additive, 32 lowercase hex digits = 128
random bits) let a client tell a replaced key store from tampering:

- `store_epoch` names one key store. It is generated when the store is
  created (`meta::StoreIdentity`); a persistent (`sealed-file`) store keeps
  its epoch across restarts, while the in-memory store is new in every
  process, so **its epoch changes on every hub restart**.
- `boot_id` is fresh for every hub process (`meta::boot_id`).

An unwrap under a key version the (new) store does not have is answered like
a failed authentication (`422`). `harw-infra-client`'s `AuthHubDekWrapper`
therefore re-reads `/v1/version` on an unwrap `422`: a changed `store_epoch`
is reported as "key store was replaced, keys are not available"
(`DekWrapperUnavailable`), an unchanged one as the authentication failure it
is. The fields are descriptive, carry no secret, and are never authority.

The meta routes need no principal: anyone allowed to `connect(2)` to the
socket (mode `0660`) may read them. They are descriptive, never authority.

## Authentication and authorization

1. The peer's uid is listed under `[[peers]]` → that principal.
2. Otherwise, if `[[bearer_tokens]]` are configured and the request carries
   `Authorization: Bearer …` → CryptGuard's constant-time `BearerTokens`.
3. Otherwise → `401`.

Authorization is CryptGuard's deny-by-default `NamespacePolicy`, built from
the `grants` of each principal. A known principal without a grant gets
`404` (not `403`), so keys cannot be enumerated.

## Configuration

Default path `/etc/harw-auth-hub/config.toml`. Strict TOML: unknown fields
and unknown op groups are errors. The file must not be group- or
world-writable.

```toml
socket_path = "/run/harw/infra/secure.sock"   # optional; this is the default

[[peers]]
uid = 990
principal = "harw-web"
grants = [
  { namespace = "app", ops = ["read-public", "encrypt", "admin"] },
]

[[peers]]
uid = 991
principal = "harw-secrets"
grants = [ { namespace = "secrets", ops = ["encrypt", "secret-egress"] } ]

[[bearer_tokens]]                 # optional fallback
principal = "ops-admin"
token_file = "/etc/harw-auth-hub/tokens/ops-admin.token"
grants = [ { namespace = "app", ops = ["all"] } ]
```

| `ops` group | Operations |
|-------------|------------|
| `read-public` | describe, public key, verify |
| `encrypt` | encrypt, wrap |
| `secret-egress` | decrypt, unwrap (grant deliberately) |
| `sign` | sign |
| `rewrap` | rewrap |
| `admin` | generate, rotate, disable, enable, destroy |
| `all` | everything |

Token files: absolute path, regular file (no symlink), no group/other bits
(`0600`/`0400`), 16–4096 bytes of printable ASCII without whitespace, one
trailing newline allowed. Token bytes are held in zeroizing memory and never
logged.

## Running

```text
harw-auth-hub [--config PATH] [--socket PATH | --systemd-socket]
```

Without `--systemd-socket` the hub creates the socket's parent directory
(`0750`) if missing, removes a stale socket (a socket file nobody accepts
on; a live socket or any other file is an error and is left alone), binds,
sets the socket to `0660` and removes it on shutdown.

With `--systemd-socket` it adopts the single `ListenStream=` socket from
`LISTEN_PID`/`LISTEN_FDS` (same `unsafe`-free approach as `harw-warden`):

```ini
# harw-auth-hub.socket
[Socket]
ListenStream=/run/harw/infra/secure.sock
SocketMode=0660
SocketUser=harw-auth
SocketGroup=harw-secure
DirectoryMode=0750
RemoveOnStop=yes

# harw-auth-hub.service
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

`SIGTERM`/`SIGINT` stop accepting, let open connections finish their
in-flight request (graceful HTTP/1 shutdown, 10 s drain limit) and exit.

## Audit

Tracing target `harw_auth_hub::audit` (filter with `RUST_LOG`):

- `event="crypto"` per executed operation: `request_id` (= `x-request-id`),
  `principal`, `op`, `key`, `target_key` (rewrap), `created_version`,
  `outcome` (`ok` or the CryptGuard error class; denials at `WARN`).
- `event="http"` per request: `peer_uid`, `peer_pid`, `method`, `path`,
  `status` — also for requests rejected before the provider (401, 404 route,
  413).

Never logged: plaintext, key material, wrapped blobs, `info`/`aad`, bodies,
bearer tokens, header values.

## Not yet implemented

- Persistence (sealed export, MIG-rest) — see the warning above.
- Harw secure frames (Masterplan §8/§9): request bodies are protected by the
  socket permissions and `SO_PEERCRED` only.
- Replay/idempotency ledger for mutations.

## Tests

```text
cargo test -p harw-auth-hub
```

Unit tests cover config parsing (unknown fields, op groups, duplicates,
permissions of config and token files), the authenticator, audit records,
meta routes and the systemd environment checks. `tests/hub.rs` runs the hub
on a temp-dir socket and drives it with a Hyper HTTP/1 client over
`UnixStream`: meta routes, generate → public key → wrap → unwrap → rotate,
unknown uid → 401, bearer fallback, policy denial (404) and stale/live/
non-socket path handling.
