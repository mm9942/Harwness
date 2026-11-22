# Local / Self-Cloud Session Control Plane

> Planning companion to PL-68.  
> Baseline: `dev@1ad90219d83aa305f5bfe2ca531a30b5ab41a829`.  
> Status: planned; not an implementation claim.

## 1. Architectural intent

Harw should expose one session-control contract to both local and self-hosted
cloud clients while preserving the authority boundary of each ingress.

The important split is:

```text
transport authenticates the connection
        ↓
composition derives ClientIdentity
        ↓
session service authorizes intent
        ↓
host mutates session state
        ↓
durable transcript records history
        ↓
WebSocket delivers live state
```

WebSocket does not authenticate, authorize or persist anything by itself.

## 2. Current anchors

### Local

`harw-web` currently owns a Unix-socket Hyper server and reads
`SO_PEERCRED` directly after accept. It resolves permission tier, tenant and
optional SecurityHub context server-side.

Its SSE event bus is process-wide and bounded. Its `sequence` reports a gap
but is not a durable session cursor.

### Remote

`harw-node-transport` currently establishes:

- TLS 1.3;
- X25519MLKEM768 key exchange;
- ML-DSA-65 authenticated node handshake;
- replay protection;
- `AuthenticatedPeer` request extension;
- Hyper HTTP/1 service/client.

At the baseline both HTTP connection drivers are plain HTTP/1 and do not
enable upgrades.

### Protocol

`harw-protocol` already owns the JSON-RPC-style envelopes and protocol
major/minor versioning. It is the correct owner of session wire vocabulary,
not of Hyper/Tungstenite.

## 3. Target dependency direction

```text
harw-protocol
      ↑
      |
harw-session-host          harw-session-remote
      ↑                           ↑
      +-------------+-------------+
                    |
              harw-cli wiring
            /                 \
    local UDS ingress     node transport ingress
          ↑                      ↑
      harw-web            harw-node-transport
    peer/identity          authenticated peer
```

Rules:

- `harw-protocol` stays runtime/transport free.
- `harw-session-host` never derives identity from payloads.
- `harw-session-remote` never owns session state.
- `harw-node-transport` learns Hyper upgrades, not session policy.
- `harw-web` keeps operation HTTP/SSE; session methods do not become a
  parallel operation registry.
- `harw-cli` is the composition root that maps authenticated transport state
  to the shared session identity.

## 4. Connection sequence — local

```text
client
  |
  | connect Unix socket
  v
sessions.sock
  |
  | SO_PEERCRED
  v
LocalPeerIdentityResolver
  |
  | principal / tenant / tier / context
  v
Hyper request /v1/sessions/ws
  |
  | Upgrade: websocket
  v
session.hello
  |
  | server grants narrowed caps
  v
attach / submit / approve / frames
```

A client label is display metadata only.

## 5. Connection sequence — self-cloud

```text
client
  |
  | private network / port forward
  v
harw-node-transport
  |
  | PQC TLS + signed transcript
  v
AuthenticatedPeer
  |
  | device/node policy + tenant/cap narrowing
  v
Hyper request /v1/sessions/ws
  |
  | Upgrade: websocket
  v
same session.hello / attach / frame protocol
```

The network address is never promoted into identity.

## 6. Replay sequence

```text
reconnect
  ↓
new transport authentication
  ↓
new capability derivation
  ↓
session.attach(last_cursor)
  ↓
generation check
  ├─ mismatch → Resync
  └─ same
       ↓
durable transcript replay
       ↓
live-ring replay or Snapshot
       ↓
subscribe to new live frames
```

A new connection never inherits the old connection's authorization state.

## 7. Backpressure

The session host publishes into a bounded per-connection queue.

```text
turn loop
  |
  +----> durable transcript
  |
  +----> live event hub
             |
             +--> connection queue A
             +--> connection queue B
             +--> connection queue C
```

If B is slow:

- coalesce disposable deltas;
- then emit visible lag/resync if possible;
- close B;
- A and C continue;
- the turn loop never waits for B.

## 8. Transport coexistence

During migration:

```text
session service
   ├─ WebSocket    primary first-party interactive transport
   ├─ HTTP RPC     compatibility / automation
   └─ NDJSON       compatibility / replay oracle

harw-web
   └─ SSE /events  existing operational web event surface
```

These paths converge on one session service. They must not each implement
their own authorization or session state machine.

## 9. Security review checkpoints

Before implementation can be called complete, review:

1. identity is derived before any stateful session action;
2. remote callers cannot be tenantless;
3. request extensions from the client cannot forge `AuthenticatedPeer`;
4. actor fields from payloads never decide approval identity;
5. every inbound and outbound queue has a bound;
6. oversized frames fail before dispatch;
7. slow consumers cannot hold model/provider permits;
8. reconnect re-evaluates revocation and capabilities;
9. disconnect does not cancel jobs/turns by default;
10. Tungstenite stays outside privileged TCB packages.

## 10. Migration rule

Do not delete the existing local/web behavior to make the target diagram
cleaner.

The order is:

```text
wrap existing identity and host semantics
→ add shared session service
→ add WS transport
→ migrate one first-party client
→ prove replay/authority equivalence
→ only then consider retiring duplicate session transport
```

## 11. Sharing profile sessions with a remote client

Status: planned (Welle 1 of the mobile plan). Not an implementation claim.

Sessions that live in the owner's own profile (the TUI and CLI sessions) can
be offered to one remote client, for example a phone, through the
`[session_listener]` section. The sharing is opt-in: the default is `off`,
and no session is visible to a remote client until the owner sets a mode and
a tenant in the local configuration.

### 11.1 Modes

| Mode | Remote client can | Remote client cannot |
|---|---|---|
| `off` | nothing (default) | see profile sessions |
| `read` | list sessions, read history, follow live turns | attach as writer, adopt, rename, archive, run turns |
| `continue` | everything in `read`, plus adopt a session and continue it | use rights above the effective tier (see 11.3) |

`read` and `continue` both require `share_tenant`. The listener refuses to
start with a share mode that has no tenant.

### 11.2 Configuration example

All values below are placeholders. Real tenant names, paths and device data
belong only in the owner's local configuration file and never in the
repository, tests or documentation.

```toml
[session_listener]
enabled = true
tier = "<listener-tier>"            # observer | operator | maintainer
approval_device = "<approval-device-id>"

# Sessions that live in the owner's profile.
share_profile_sessions = "continue" # "off" | "read" | "continue"; default "off"
share_tenant = "<tenant>"           # required unless share_profile_sessions = "off"
share_legacy_rows = 100             # rows for old clients; maximum 1000
share_since = "<YYYY-MM-DD>"        # optional: share only newer sessions
share_roots = ["<absolute-path-to-work-folder>"]  # optional: narrows allowed work folders
```

Rules for these keys:

- All five keys are global-only and security-critical. A project-level file
  cannot set them.
- `share_roots` entries must be absolute paths without `.` or `..` segments.
- Start with `read`. Move to `continue` only after every TUI, web and gateway
  process has been restarted with the new binary (see 11.5).

### 11.3 Effective tier

The rights of a remote client are the minimum of two settings:

1. the device tier from the device registry (`node-devices.conf`), and
2. the listener tier (`[session_listener] tier`).

The app shows this effective tier as a badge. A key in the configuration can
never raise rights above either limit.

### 11.4 Holding a session: the ownership lease

Only one writer may write to a session at a time. Each session has a lease
file `<session-id>.lease` next to its transcript.

- The lease is an exclusive file lock. The holder is one of `tui`, `gateway`
  or `job`, plus the process id and the time since which it is held.
- The file never contains a hostname. Remote clients receive only the holder
  kind and the time, never the process id.
- A process that crashes releases the lock automatically. A leftover lease
  file with content but no lock is harmless and does not block writers.
- Locks work only on local file systems. Network file systems are not
  supported.
- A remote turn on a session that is held elsewhere is refused with `BUSY`
  and a reason. Reading is always allowed.

Do not delete `.lease` or `.meta.lock` files while a harw process is running.
If a session looks stuck, stop the process that holds it first, then retry.

### 11.5 Procedure: revoking a device

Use this procedure when a device is lost, replaced or no longer trusted. A
phone with the `maintainer` profile is a strong device, so act quickly.

1. Stop sharing for everyone if the device is unknown:
   set `share_profile_sessions = "off"` in the local configuration (remove the
   keys if the older binary must run again), then restart the gateway.
2. Remove the device from the device registry (`node-devices.conf`). If the
   device was the approval device, change `approval_device` as well.
3. Remove the device's pinned entry from the peer list (`node-peers.conf`), if
   it has one.
4. Close its open connections with the gateway method
   `gateway.connections.revoke`, or restart the gateway.
5. Check the result: a new connection from the device must fail during
   transport authentication, and the session list must no longer contain
   profile sessions for that device.
6. If the device held any credentials, rotate them on the server. Treat
   sessions that the device continued during the time of loss as possibly
   changed and review their transcripts.

Approvals on the phone use fresh biometric confirmation for every approval and
every step-up. There is no validity period that would survive a revocation.

### 11.6 Rollout order

1. Install the new binary on all machines. Keep the share keys absent.
2. Restart all TUI, web and gateway processes.
3. Add the keys with `share_profile_sessions = "read"`.
4. After checking the list and history, change to `"continue"` if needed.

Rollback is the reverse: remove the share keys, then start the older binary.
