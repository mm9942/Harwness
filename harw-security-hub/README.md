# harw-security-hub

The local SecurityHub from the crypto masterplan v2 (wave **H8**, work
package **WP-14**). It answers **WHO + WHERE → SHOULD** for processes on this
host:

- **Who**: the kernel-attested `SO_PEERCRED` uid of the caller. It is never
  read from a header or body.
- **Where**: the trust zone and workspace/tenant scope set by server-side
  policy.
- **Should**: a short-lived `harw_types::SecurityContext`, minted with the
  hub's only `SecurityContextIssuer`, plus read-only DoD posture.

The architecture layer is **A** (application/composition). The internal
dependencies are `harw-types` and `harw-fsutil` (symlink-safe opening of the
config file). No DoD crate and no CryptGuard crate is linked.

## Socket

`/run/harw/infra/security.sock` is the default. It gets mode `0660` right
after `bind()`. The hub has its own socket and does not share `control.sock`,
by decision D1/D2 in `docs/architecture/crypto-drift-report.md`. This
deliberately deviates from masterplan §39.

The parent directory must be owned by the service user and must not be
world-writable. That setup belongs to tmpfiles/deploy. A stale socket file is
removed only after a connect probe gets `ECONNREFUSED`. The hub never removes
a path that is not a socket.

## API (protocol 1, HTTP/1 JSON)

| Method | Path | Caller | Response |
|---|---|---|---|
| `GET` | `/v1/health` | any peer | `{"status":"ok"}` |
| `GET` | `/v1/version` | any peer | `{"service","version","protocol"}` |
| `GET` | `/v1/capabilities` | any peer | descriptive only, never authority (§38) |
| `POST` | `/v1/contexts` | policy peers | `201 {"context_id","context":<SecurityContextSummary>}` |
| `GET` | `/v1/contexts/{id}` | verifiers, the requester | `200 {"status":"active","context":…}`, `410 {"status":"expired"\|"revoked"}`, `404` |
| `DELETE` | `/v1/contexts/{id}` | verifiers, the requester | `200 {"status":"revoked"}`, `410 {"status":"expired"}`, `404` |
| `GET` | `/v1/posture` | policy peers, verifiers | peer, principal id, host, `dod.{status,findings,…}` |

Errors look like `{"error":"<code>"}`. The codes are `unknown_peer`
(403), `workspace_not_permitted` (403), `zone_broadening` (403),
`invalid_ttl` (400), `invalid_request` (400: malformed JSON or an unknown
field), `invalid_context_id` (400), `body_too_large` (413), `table_full`
(503), `not_found` (404) and `method_not_allowed` (405).

A peer that is neither a verifier nor the requester of a context gets `404`
for that context. This prevents probing context ids.

### Issuing a context

The request body is optional. Every field in it can only narrow the context:

```json
{"workspace": "ws-1", "trust_zone": "cluster", "ttl_secs": 60}
```

| Field | Allowed | Denied |
|---|---|---|
| `workspace` | any workspace if the rule has no workspace scope; `W` if the rule is scoped to `W` | any other workspace |
| `trust_zone` | the rule's zone or a farther one (less trusted) | a closer zone |
| `ttl_secs` | `≥ 1`, silently capped at the rule/policy cap | `0` |

`tenant`, `principal`, `auth_strength` and every other field are rejected as
unknown fields.

The hub keeps the trusted context in memory. Callers get only the reference
and a non-authoritative summary (§25). Other daemons resolve the reference
with `GET /v1/contexts/{id}`.

Issued contexts are not persisted. A restart invalidates every reference,
which is the safe direction.

## Configuration

The config lives in `/etc/harw-security-hub/config.toml`. Every table uses
`deny_unknown_fields`.

The hub refuses to start unless the config file passes these checks at load
time. The startup error names the check that failed.

- It is a regular file, and the file itself is not a symlink. Parent
  directories may be symlinks. A symlink, FIFO, socket, device or directory is
  rejected, and a FIFO is rejected without blocking. Install the file itself,
  not a link to it.
- It is not writable by group or others. `0644`, `0640` and `0600` pass;
  `0664` and `0666` do not.
- It is at most 256 KiB (262144 bytes).
- At the default path `/etc/harw-security-hub/config.toml` it is owned by
  root.

```toml
[server]
socket = "/run/harw/infra/security.sock"   # default
issuer = "harw-security-hub"               # default
node = "node-1"                            # bound into every context
host = "host-a"                            # DoD correlation key
max_contexts = 10000                       # default

[findings]                                 # optional; requires server.host
path = "/var/lib/harw-dod/export/findings.jsonl"
min_severity = "high"                      # default
window_secs = 86400                        # default
max_tail_bytes = 1048576                   # default
max_line_bytes = 16384                     # default
max_records = 64                           # default

[policy]
default_ttl_secs = 300                     # default
max_ttl_secs = 900                         # default; ≤ SecurityContext::MAX_TTL (24h)
verifier_uids = [990]                      # daemons that may verify/revoke any context

[[policy.peers]]
uid = 1000
tenant = "acme"
workspace = "ws-a"                         # optional; omitted = unscoped
trust_zone = "local"                       # ceiling; default "local"
auth_strength = "peer_credential"          # default; anything stronger is rejected
max_ttl_secs = 600                         # optional per-peer cap
service_identity = "svc-x"                 # optional
principal = { kind = "human", id = "alice", surface = "cli", tier = "owner" }
```

The hub verifies only a local peer credential. A rule therefore cannot claim
`token`, `mutual_tls` or `hardware_backed`.

## DoD correlation (read-only)

DoD has no JSON findings export yet. `harw-sentinel` reports findings through
`tracing` and a telemetry counter. This crate defines the minimal JSON Lines
line format that a DoD-side exporter or the WP-15 uplink must write:

```json
{"finding_id":"f-1","host":"host-a","rule_id":"structure-drift","severity":"high","summary":"…","observed_at":"2026-09-27T10:00:00Z"}
```

The reader has these limits:

- It reads only the tail of the file (`max_tail_bytes`) and drops a partial
  first line.
- It skips lines longer than `max_line_bytes`.
- It counts malformed lines and does not treat them as fatal.
- It returns at most `max_records` findings, the newest ones.
- It sanitizes summaries: it truncates them and replaces control characters.

If the file is missing or unreadable, the posture reports
`dod.status = "unavailable"` and the endpoint still returns `200`. The hub
never writes to DoD and never talks to `sentinel.sock` or `warden.sock`.

## Running

```text
harw-security-hub --config /etc/harw-security-hub/config.toml
harw-security-hub --config … --check      # validate and exit
harw-security-hub --config … --systemd-socket   # socket activation
```

`SIGTERM` and `SIGINT` start a graceful shutdown. The hub stops accepting,
lets open connections finish (5 s grace), then removes the socket file. Set
the log level with `RUST_LOG`.

With `--systemd-socket` the hub adopts the one listening socket systemd
passes (`LISTEN_PID`/`LISTEN_FDS`, validated without `unsafe`; the
descriptor comes from the `sd-listen-fds` crate). This is how
`deploy/systemd/harw-security-hub.socket` runs it: the socket unit owns
`/run/harw/infra/security.sock` (mode `0660`, group `harw-security`),
`server.socket` from the config is ignored, and the hub neither binds nor
removes a socket file. A missing or foreign activation environment, or any
descriptor count other than one, is a startup error; there is no fallback
to binding the path.

## Tests

```text
cargo test -p harw-security-hub
```

The tests cover:

- The policy matrix: unknown uid, workspace narrowing and broadening, zone
  narrowing and broadening, TTL capping, strict parsing.
- The context table: expiry, revocation tombstones, capacity.
- Finding parsing: filters, malformed and oversize lines, record limit,
  tail/partial lines, sanitizing.
- Config validation and the load-time file checks: directory, `0666` file,
  symlink, oversized file and FIFO (rejected without blocking).
- End-to-end runs over a socket in a temp directory: issue → verify → revoke,
  expiry, denial for an unknown peer, posture with findings, socket mode
  `0660`, stale-socket handling and graceful shutdown.
- systemd activation environment checks: missing, malformed or foreign
  `LISTEN_PID`, malformed `LISTEN_FDS`, descriptor count other than one.
