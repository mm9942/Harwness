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

State is on the integration branch `consolidate/main` plus this branch
(merged into it for verification). Nothing is merged to `dev`.

| WP | Content | State | Where |
|---|---|---|---|
| W00 | decision, contracts | done | PL-68, PL-65, `W00-ws-integration-map.md` |
| W01 | transport-neutral protocol | done | `harw-protocol` |
| W02 | Hyper upgrade through the node transport | done | `harw-node-transport::upgrade` (`serve_upgradable`, `NodeTransportClient::upgrade`, `peer_of`) |
| W03 | WebSocket session adapter | done | `harw-session-ws` |
| W04 | local UDS composition | **split**: socket/stale-socket lifecycle in PR #91; stack, identity and accept loop in `harw-session-com` | #91; `harw-session-com::local` |
| W05 | session host + replay + real driver | host done; production `CoreTurnDriver` exists | `harw-session-host`, `harw-session-driver` |
| W06 | self-cloud composition | done | `harw-node-listener` (registry, mapper, revocation); `harw-session-com::remote` composes it behind Tower |
| W07 | approvals, concurrent controllers across the real transport | open | |
| W08 | backpressure, reconnect | server bounded; client `harw-session-remote` exists; no end-to-end reconnect test through the Com layer | |
| W09 | `harw attach` | open | |
| W10 | dependency and security gates | arch gate green with all crates; `cargo deny` not run here | |
| W11 | operator workbench | open | |

## 2. The Com layer (`harw-session-com`)

One Tower-composed service for every ingress. A listener authenticates the
peer and builds a `ClientIdentity`; the Com layer validates the upgrade, binds
the identity **before** the 101, and serves `session.*`, `tool.*`, `gateway.*`.

```text
 UDS accept (SO_PEERCRED) ---------\
   local::serve_unix                 >  ComServer -> tower: Peer/Remote layer -> [added layers] -> UpgradeService
 node transport (PQ-TLS+ML-DSA) ---/     serve_io      validate -> bind (HTTP status on refusal) -> 101 -> session
   serve_upgradable + service_for(RemoteLayer)
```

What it adds over composing the pieces by hand:

1. `tool.*` and `gateway.*` reachable on the **local** socket
   (`PortOffer::All`, tier-derived gateway caps in `local::local_identity`).
   PR #91 serves the session table only and builds identities without gateway
   caps.
2. Refusals are HTTP statuses (403/503) because the identity is bound before
   the 101.
3. Ingress policy is a Tower layer (`ComServer::layer`).
4. One bounded server: connection limit covering the whole WebSocket session,
   header timeout and bounded headers, graceful drain.
5. Clients must negotiate `wire_minor` 2 to receive R18 caps.

Relation to `harw-node-listener`: both implement identity -> upgrade -> serve.
The listener owns the device registry, the identity mapper and revocation of
live connections; the Com layer is the Tower stack and the local ingress. They
meet at `RemoteLayer`, whose async resolver is the listener's `IdentityMapper`
(see `tests/self_cloud.rs`). Whether the listener's `ListenerService` should
be built from `ComServer::service_for` is an open decision for its owner.

Pitfall recorded in the crate: hyper 1.11 `keep_alive(false)` rewrites the
101's `Connection: Upgrade` to `close`; refusals close through an explicit
header instead.

Dependencies: `hyper`, `hyper-util`, `tokio`, `tokio-tungstenite` (through
`harw-session-ws`), `tower` (`util` only). `hyper-tungstenite` is intentionally
not used: `harw_session_ws::upgrade` is stricter (exact subprotocol, `Origin`
refused).

## 2a. Verification (scratch build on this branch merged with `consolidate/main`; not a frozen-SHA central build)

- `harw-session-com`: 28 tests pass: 11 unit, 13 end to end against a real
  `SessionHost` (in-memory stream and a real Unix socket through
  `serve_unix`), 4 self-cloud end to end through the real node transport
  (PQ-TLS + ML-DSA) using `harw-node-listener`'s device registry: an enrolled
  device runs a session; gateway rights follow the device tier; an unenrolled
  but transport-trusted node gets 403; a revoked device cannot reconnect.
- `clippy --all-targets -D warnings`, `fmt --check` clean; `cargo xtask gates`
  (edges, privileges, warden budget, no-c-build, arch) green. `cargo deny` was
  not run.

## 3. Remaining plan, in build order

1. **Adopt in #91** (`harw-session-daemon`): replace `serve_stream` by
   `ComServer::serve_io`, use `local::serve_unix`, keep bind/stale-socket
   handling. Owner: the #91 branch.
2. **Composition root** in `harw-cli`: `harw gateway serve` opens the host,
   the transcript store, the approval backend, the driver, the `ComServer`,
   `serve_unix` and (optionally) the node-transport server with a
   `DeviceRegistry` from config. Shared file: scribe wave.
3. **`harw attach`** (W09) over `harw-session-remote`, with an end-to-end
   reconnect test through the Com layer (W08).
4. **Tests across the real transport**: concurrent approvals (W07), slow
   consumer and drain (W08), cross-tenant session id (W06).
5. **Gates** (W10): `cargo deny` entry for the direct `tower` and
   `tokio-tungstenite` use; confirm the arch gate keeps both out of TCB/J/D.

## 4. Decisions still open (from PL-68 §13)

Unchanged. One conflict to resolve: PL-68 §13 recommends that high-risk
approval stays local unless policy enables remote approval, but
`harw-node-listener`'s mapper builds remote caps as `caps_for_tier` plus
`gateway_caps_for_tier`, so every remote operator-or-above device holds
`approve` (and maintainers/owners hold gateway caps). Either document that as
the decision or add a per-device opt-in.
