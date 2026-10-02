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
