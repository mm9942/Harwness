---
id: PL-89-REVERSE-PROXY
title: "Reverse proxy on Pingora: scope, dependency decision, architecture, first slices"
status: proposed
date: 2026-10-01
baseline: dev@4e49a173a8431a062f715310f7ce2bc07546e278
---

# Reverse proxy on Pingora

> Status: planning plus a first pure policy core. **No Pingora dependency is
> added by this change.** Whether to add it is an explicit owner decision
> (section 4).

## 1. Purpose

Defined by the crypto masterplan, section 33: Pingora belongs **at the network
edge** (browser/external ingress), not in crypto primitives and not between
nodes. It may do connection handling, TLS, routing, upstream health,
load/failover, rate limiting and zone routing, and it must not invent Harw
principals from a source IP. Authentication, load balancing and the encryption
model are worked out in `EDGE-AUTH-LB-ENCRYPTION.md`; read it with this file.

Still open: whether the proxy is also the origin side for Cloudflare tunnels,
and whether it serves end-user traffic or only operator/device traffic.

## 2. What the repo already has (evidence)

| Need | Existing | Source |
|---|---|---|
| SOCKS5 with policy | `EgressProxy`/`serve`, RFC 1928, Unix socket only, same `EgressPolicy` as the HTTP client | `harw-egress/src/proxy.rs`, `lib.rs` |
| Address classification | `classify(IpAddr) -> AddrClass` incl. cloud metadata, CGNAT, ULA, IPv4-mapped and NAT64 | `harw-egress/src/classify.rs` |
| Host allowlist + digest | `EgressPolicy::{check_url, check_addr, check_resolved, digest}` | `harw-egress/src/policy.rs` |
| HTTP server on a Unix socket with peer credentials | `harw-web` | `harw-web/src/server.rs` |
| Session WebSocket transport + local daemon | `harw-session-ws`, `harw-session-daemon` (#91) | PR #91 |
| Node mutual-auth transport | `harw-node-transport` | crate |
| No Pingora anywhere | none | grep over the tree |

Consequence: the proxy must **reuse** the egress classifier and policy rather
than add a third address classifier (the tunnel policy cores in #88 currently
duplicate a smaller one; see section 7).

## 3. Pingora facts (measured, not assumed)

Resolved in a scratch crate with `pingora = { default-features = false,
features = ["proxy", "rustls"] }`:

- Version **0.9.0**, about **240 packages** added.
- No `openssl`, `openssl-sys`, `native-tls` or boringssl in the tree
  (`openssl-probe` is only a certificate-path helper), so the `deny.toml` bans
  are not hit. Note that `harw-node-transport/README.md` says Pingora "would add
  BoringSSL/OpenSSL"; that holds for Pingora's default TLS backend (openssl),
  not for the `rustls` feature measured here (see `EDGE-AUTH-LB-ENCRYPTION.md`
  section 7).
- **New C code:** `zstd-sys` comes in through `pingora-core` (compression) and
  through `pingora-cache` via the `proxy` feature. It is not in `Cargo.lock`
  today. `ring` is the TLS crypto provider; both `ring` and `aws-lc-rs` are
  already in `Cargo.lock`, but `deny.toml` documents "rustls + aws-lc-rs" as the
  policy, and Pingora's rustls path uses `ring`. This is a deviation to decide,
  not to ignore.
- `proxy` pulls `pingora-cache`; there is no feature that removes it from
  `pingora-proxy`. Cache is therefore compiled in even if unused.

Spike (scratch crate outside the repo, not committed): a `ProxyHttp` adapter of
about 45 lines that calls `RouteTable::decide` in `request_filter` and picks the
peer from the decision in `upstream_peer`.

- `cargo check` of the adapter plus all Pingora dependencies: about 66 s.
- Run against a real local upstream through Pingora 0.9 on loopback:
  matching request forwarded (200, upstream body returned), unknown host 421,
  known host without a route 404, disallowed method 405, `%2e%2e` traversal 400,
  `/api/../index.html` normalized and refused 404. The policy core decided
  every case; the adapter contains no policy.
- Not exercised by the spike: header sanitization and upstream-target rewriting
  in the adapter (`upstream_request_filter`), body limits, TLS, timeouts,
  graceful reload, WebSocket upgrade, behavior under load.

Not verified: the `warden-*` and `arch` gates with Pingora in `Cargo.lock`
(they computed green on the current tree only), `cargo deny check` (not
installed here), binary size, MSRV against `rust-toolchain.toml`, behavior under
load.

## 4. Decision needed: the dependency

| Option | What it means | Risk |
|---|---|---|
| A. Pingora (`proxy` + `rustls`) | Full framework, connection pooling, HTTP/1+2, graceful upgrade | ~240 new packages, new `*-sys` crate (`zstd-sys`), `ring` vs `aws-lc-rs` policy deviation, Tier review entry needed in `docs/architecture/dependency-review.md` |
| B. `hyper` + `hyper-util` + `rustls` (already in the tree) | Own small proxy, reuse existing TLS and HTTP stack | We own the proxy semantics (hop-by-hop headers, upgrades, pooling) and their bugs |
| C. Defer | Ship the policy core only; adapter later | No running proxy yet |

Recommendation: **C now, A or B after the owner decides**, because the policy
core (section 5) is identical for both and the decision has supply-chain
consequences that should not be made inside a feature PR. If A is chosen, it
needs: a dependency-review entry, an explicit decision on `ring`, the `warden`
and `arch` gates re-run with Pingora in the lockfile, and `cargo deny check`.

## 5. Architecture

```text
client (device / node / browser)
   |  TLS (rustls), mutual auth where the route requires it
   v
listener  ->  admission (limits, allowed bind, Host/SNI checks)
   v
RoutePolicy (pure, this change)
   |  route match (host + path prefix), method/header rules,
   |  upstream resolution through harw-egress classification
   v
adapter (Pingora ProxyHttp  or  hyper)      <- thin, no policy decisions
   v
upstream (loopback / allowlisted address only)
```

Rules, each enforced in the pure core and tested there:

1. **Closed route table.** A request matches exactly one configured route or is
   refused (404 or 421); there is no default upstream.
2. **Upstream allowlist is by address class.** Upstreams default to loopback
   only. A private upstream needs an explicit per-route flag. Cloud metadata,
   link-local, CGNAT and reserved addresses are never allowed, regardless of
   flags (reuse of `harw_egress::classify`). Hostnames as upstream are rejected
   unless pinned to checked addresses, to avoid the remote-resolution gap that
   the tunnel review found.
3. **Header hygiene.** Hop-by-hop headers are dropped; inbound `Forwarded` and
   `X-Forwarded-*` are replaced, never appended to; configured internal
   headers (identity, tenant) are stripped from client requests and only set by
   the proxy.
4. **Host and path normalization before matching.** Reject ambiguous `Host`
   (userinfo, multiple values, trailing dot variants), percent-encoded or
   `..` path segments are normalized once and matched on the normalized form;
   the upstream sees the normalized path.
5. **Bounded everything.** Header size, body size per route, connection and
   request-rate limits, idle and upstream timeouts. Defaults are conservative;
   raising them is configuration, not code.
6. **No authority from the proxy.** It never decides what a caller may do; it
   forwards to services that authenticate and authorize (session host, web
   control plane). Every `x-harw-*` header from a client is dropped, and the
   proxy sets no plaintext identity header; how identity reaches a service is an
   open design question constrained by the mint-only `SecurityContext` type
   (masterplan section 14; details in `EDGE-AUTH-LB-ENCRYPTION.md`).
7. **Admin/config separation.** Route changes are a separate, approved,
   audited action; the approval is bound to the full route table (same lesson
   as the tunnel approvals: canonical, length-prefixed, injective encoding).

## 6. Deployment profile

The proxy is a **separate service profile**, not part of the Cloud Hub process:
the hub mounts identity, directory and routing; the proxy mounts only listener
and forwarding (`DeploymentProfile` in #90 gets a `ProxyEdge` entry when this
lands: no workspace write, no host shell, no container socket).

## 7. Related follow-ups found while planning

- `harw-tool-tunnel/src/policy.rs` (#88) has its own private/loopback
  classification. It should call `harw_egress::classify` instead. That also
  closes classes the tunnel check lacks (cloud metadata, CGNAT, NAT64). Not done
  here; it needs a layering check (`harw-tool-tunnel` is ring A, `harw-egress`
  is ring I, so the dependency direction is allowed).
- SOCKS for tunnels: see `docs/design/tunnel-policy-v3-socks.md`.

## 8. Work order

1. **P0** (this change): plan, SOCKS policy note, pure `RoutePolicy` core with
   tests, no new third-party dependency.
2. **P1:** owner decision on the dependency (section 4).
3. **P2:** adapter over the chosen stack, behind the policy core; loopback
   listener only; end-to-end test against a local upstream.
4. **P3:** TLS termination and mutual auth for non-loopback listeners.
5. **P4:** limits, timeouts, graceful reload, `ProxyEdge` profile, dependency
   review and gates.

## 9. Verification gates for any implementation PR

`cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace`, `cargo run -q -p xtask -- gates`, `cargo deny check`,
`actionlint`. Security-sensitive negative tests: upstream to metadata address,
IPv4-mapped loopback, `Host` smuggling, path traversal, header spoofing of
identity headers, oversized bodies, slow headers.
