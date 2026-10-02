---
id: PL-68-INTEGRATION-STATUS
title: "WebSocket gateway integration: status, Com layer, remaining plan"
status: in-progress
date: 2026-10-02
baseline: main@9e639f5 + branch ccr-fe134f37-xdezhn (not merged)
supersedes: "PL-68 §14 IMPLEMENTATION STATUS (stale)"
---

# WebSocket gateway integration

Companion to `README.md` (PL-68). That plan's §14 was written before most of
the pieces existed; this note records what is real now, what the **Com layer**
is, and what is left. Evidence is code and tests; nothing here is claimed
merged.

## 1. Where each work package stands

| WP | Content | State | Evidence |
|---|---|---|---|
| W00 | decision, contracts | done | PL-68, PL-65 |
| W01 | transport-neutral protocol | done | `harw-protocol` (`session_wire`, `session_port`, methods) |
| W02 | Hyper upgrade through the node transport | **done on this branch** | `harw-node-transport` server and client use `with_upgrades()`; tests `upgrade::*` |
| W03 | WebSocket session adapter | done | `harw-session-ws` (upgrade, codec, limits, dispatch, conn) |
| W04 | local UDS composition | **partly**: listener/socket in PR #91; the stack in `harw-session-com` | #91; `local::serve_unix` |
| W05 | session host + durable replay | done (host); durable store wiring open | `harw-session-host` |
| W06 | self-cloud composition | **done on this branch for the transport side** | `harw-session-com::remote`, `tests/self_cloud.rs` |
| W07 | approvals, concurrent controllers | host side exists; **no test across the real transport** | open |
| W08 | backpressure, reconnect | bounded in `harw-session-ws`; **no reconnect client** | open |
| W09 | `harw attach` | open; needs the composition root below | open |
| W10 | dependency and security gates | partly: arch gate green with the new crates; `cargo deny` not run here | open |
| W11 | operator workbench | open | open |

## 2. The Com layer (`harw-session-com`)

One service for every ingress. A listener authenticates the peer and builds a
`ClientIdentity`; the Com layer does the rest.

```text
 UDS accept (SO_PEERCRED) --------\
                                   >  ComServer  ->  tower stack  ->  upgrade
 node transport (PQ-TLS+ML-DSA) --/   serve_io      Peer/Remote layer   validate -> bind -> 101
      service_for(RemoteLayer)                      [added layers]      session.* tool.* gateway.*
```

What it fixes compared with composing the pieces by hand:

1. `tool.*` and `gateway.*` are reachable (`PortOffer::All`); before there was
   no production caller of `serve_connection_with`.
2. The local identity carries the tier's gateway caps (`gateway_caps_for_tier`).
3. Refusals are HTTP statuses (403/503) because the identity is bound before
   the 101.
4. Remote devices are tenant-scoped, 1:1 with a node id, and never get
   `gateway_*`, `tool_call` or (by default) `approve`.
5. Clients must negotiate `wire_minor` 2 to receive R18 caps.

Pitfall recorded in the crate: hyper 1.11 `keep_alive(false)` rewrites the
101's `Connection: Upgrade` to `close`.

Dependencies: `hyper`, `hyper-util`, `tokio`, `tokio-tungstenite` (through
`harw-session-ws`), `tower` (`util` only; no new lockfile package).
`hyper-tungstenite` is intentionally not used: `harw_session_ws::upgrade` is
stricter (exact subprotocol, `Origin` refused).

## 3. Remaining plan, in build order

1. **Adopt in #91** (`harw-session-daemon`): replace `serve_stream` by
   `ComServer::serve_io`, use `local::serve_unix`, keep bind/stale-socket
   handling. Owner: the #91 branch.
2. **Real `TurnDriver`**: `SessionHost` needs a driver that runs
   `harw-runtime` turns. This is the largest missing piece: the host has no
   production caller today, and an in-process driver means composing a
   `RuntimeAssembly` per session. Plan: a thin crate `harw-session-driver`
   (ring A) that implements `TurnDriver` over the runtime's turn API, with the
   event sink mapped to `SessionFrame`s. Needs a design pass with the runtime
   owners first.
3. **Composition root** in `harw-cli`: `harw gateway serve` opens the host,
   the transcript store, the approval backend, the driver, the `ComServer`,
   `serve_unix` and (optionally) the node-transport server with a
   `DeviceRegistry` from config. Shared file: scribe wave.
4. **`harw attach`** (W09) and the `RemotePort` client (reconnect with cursor
   resume, W08).
5. **Tests across the real transport**: concurrent approvals (W07), slow
   consumer and drain (W08), cross-tenant session id (W06).
6. **Gates** (W10): `cargo deny` entry for the direct `tower` and
   `tokio-tungstenite` use; confirm the arch gate keeps both out of TCB/J/D.

## 4. Decisions still open (from PL-68 §13)

Unchanged, with the recommended defaults the Com layer already follows:
remote approval is opt-in per device (`may_approve`), self-cloud is private
node transport only, pinned devices first (enrollment later).
