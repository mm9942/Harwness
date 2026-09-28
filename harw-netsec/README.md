# harw-netsec

NetSec local network-control daemon (crypto masterplan v2 §6.2, §18, §34 H7).

NetSec owns **topology**: which nodes exist, where they can be reached,
which zone/route leads there, and whether a node is being drained. It is
**not** an identity authority — the Auth/Crypto Hub answers "is this
cryptographically node X?", NetSec answers "where is node X and should it be
drained?". A node record carries only a *reference* to its identity key; no
address is ever treated as evidence of identity.

`network.sock` is the local control API of the daemon, not the inter-node
transport (§19 / H11 are out of scope here).

## Transport and authorization

- `AF_UNIX` / `SOCK_STREAM`, Hyper HTTP/1, default path
  `/run/harw/infra/network.sock` (crypto drift report D1).
- The caller is identified solely by `SO_PEERCRED`. Peers whose uid is not in
  `allowed_uids` receive `403 peer_not_allowed` on every request.
- Request bodies are JSON with `deny_unknown_fields`, bounded by
  `max_body_bytes` (default 64 KiB, hard cap 1 MiB), with header/body read
  timeouts and a concurrent connection limit.

## API (protocol 1)

| Method | Path | Result |
|---|---|---|
| GET | `/v1/health` | `{"status":"ok"}` |
| GET | `/v1/version` | `{"service","version","protocol"}` |
| GET | `/v1/capabilities` | `{"service","protocol","operations":[…]}` (descriptive, not authority) |
| GET | `/v1/nodes` | `{"nodes":[NodeRecord…]}` |
| POST | `/v1/nodes` | `201` + new `pending` `NodeRecord` |
| GET | `/v1/nodes/{id}` | `NodeRecord` |
| POST | `/v1/nodes/{id}:activate` | `pending → active` |
| POST | `/v1/nodes/{id}:drain` | `active → draining` |
| POST | `/v1/nodes/{id}:complete-drain` | `draining → drained` |
| POST | `/v1/nodes/{id}:undrain` | `draining/drained → active` |
| POST | `/v1/nodes/{id}:revoke` | any non-revoked → `revoked` (terminal; prunes routes) |
| GET | `/v1/zones` | `{"zones":[Zone…]}` |
| GET | `/v1/routes` | `{"routes":[Route…]}` |

Action bodies are optional: `{}` or `{"reason":"…"}`. The reason and the
peer uid are recorded in `last_transition`.

Errors: `{"error":{"code":"…","message":"…"}}` with codes `not_found`,
`method_not_allowed`, `peer_not_allowed`, `invalid_json`, `invalid_request`,
`body_too_large`, `unsupported_media_type`, `body_timeout`, `node_exists`,
`node_not_found`, `invalid_transition` (409), `unknown_zone`,
`capacity_exhausted`, `internal`.

Register example:

```sh
curl --unix-socket /run/harw/infra/network.sock -X POST \
  -H 'content-type: application/json' \
  -d '{"id":"node-7","display_name":"Rack 3 / 7","addresses":["10.0.0.7:7443"],
       "zone":"lan","identity_key_ref":"authhub:node/node-7"}' \
  http://localhost/v1/nodes
curl --unix-socket /run/harw/infra/network.sock -X POST http://localhost/v1/nodes/node-7:activate
curl --unix-socket /run/harw/infra/network.sock -X POST http://localhost/v1/nodes/node-7:drain
```

Node, zone and route ids are tokens: 1–128 bytes of `[A-Za-z0-9._-]`, not
starting with `.`.

## Node state machine

```text
Pending --activate--> Active --drain--> Draining --complete_drain--> Drained
                        ^                  |                            |
                        +----- undrain ----+----------- undrain --------+
Pending | Active | Draining | Drained --revoke--> Revoked (terminal)
```

The table lives in `src/state_machine.rs` (`TRANSITIONS`); every state change
goes through `next_state`. Repeated events (draining a draining node) are
conflicts, not no-ops.

## State store

`<state_dir>/state.json` (default `/var/lib/harw-netsec`), mode `0600`, plus
`state.lock` (single-writer lock). Every mutation is applied to a copy,
written to a temp file, `fsync`ed, renamed over `state.json`, and the
directory is `fsync`ed; only then does the in-memory state change. A corrupt,
oversized, symlinked, unknown-field or inconsistent state file stops the
daemon at start-up — topology is never silently reset.

## Configuration

`/etc/harw-netsec/config.toml` (no fallback to `$HOME` or the working
directory):

```toml
# socket = "/run/harw/infra/network.sock"
# state_dir = "/var/lib/harw-netsec"
allowed_uids = [0]          # required, non-empty
# max_body_bytes = 65536
# max_nodes = 4096
# max_connections = 64

[[zones]]
id = "local"
display_name = "Local host"
```

## Running

```text
harw-netsec [--config PATH] [--systemd-socket]
```

SIGTERM/SIGINT stop accepting, let in-flight requests finish (5 s grace) and
remove a socket the daemon bound itself. With `--systemd-socket` the
listener comes from systemd (`LISTEN_PID` must match, `LISTEN_FDS=1`), the
same contract as `harw-warden`:

```ini
# harw-netsec.socket
[Socket]
ListenStream=/run/harw/infra/network.sock
SocketMode=0660
SocketUser=harw-netsec
SocketGroup=harw-network
DirectoryMode=0750
RemoveOnStop=yes

[Install]
WantedBy=sockets.target

# harw-netsec.service
[Service]
Type=simple
User=harw-netsec
ExecStart=/usr/libexec/harw-netsec --systemd-socket
StateDirectory=harw-netsec
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
RestrictAddressFamilies=AF_UNIX
```

## Not yet here

Route/zone write endpoints, heartbeats (`last_seen`), AuthHub-driven
activation (H3), remote node transport (H11), Tower middleware.
