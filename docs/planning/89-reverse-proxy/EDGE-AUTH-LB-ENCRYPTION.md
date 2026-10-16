---
id: PL-89-EDGE-AUTH-LB-ENCRYPTION
title: "Reverse proxy edge: authentication, load balancing and the encryption model"
status: proposed
date: 2026-10-01
parent: README.md
sources:
  - docs/planning/30-crypto-infrastructure/Harwness_Crypto_Infrastructure_Masterplan_v2.md (sections 4, 5, 14, 19, 21, 32, 33)
  - harw-node-transport/README.md
  - docs/planning/65-cloud-sessions/CLOUD-HOME-HUB-PROFILES-V2.md (PR #82/#83)
  - harw-netsec/src/state_machine.rs
  - pingora 0.9.0 sources (pingora-core, pingora-rustls, pingora-load-balancing)
---

# Edge: authentication, load balancing, encryption

The masterplan already fixes where Pingora belongs (section 33): **at the
network edge, not in crypto primitives, and not between nodes.** This note
works out what that means for authentication, load balancing and the
encryption model, and lists what is still open. It corrects two assumptions in
`README.md` (see section 7).

## 1. Two planes, two TLS profiles

| | Inter-node plane | Browser / external edge |
|---|---|---|
| Component | `harw-node-transport` | the reverse proxy (this plan) |
| TLS | rustls + **aws-lc-rs**, TLS 1.3 only, key exchange **only** `X25519MLKEM768` (no downgrade) | classic X.509 TLS: a browser needs it |
| TLS server key | ephemeral Ed25519 raw public key (RFC 7250), no certificates, no CA, no SNI, no resumption | X.509 certificate chain |
| Identity | pinned ML-DSA-65 node key, mutual handshake inside TLS bound to the TLS exporter, replay cache, +-30 s skew | none at the TLS layer; identity comes from AuthHub after the proxy |
| Pingora | **never** (README of the crate; masterplan 19/33) | yes, if the dependency decision allows |

Consequence: the proxy cannot reuse the node-transport TLS profile, and it must
not be placed on node-to-node paths.

### Pingora's TLS path as shipped (0.9.0, read from the sources)

- TLS backends are mutually exclusive features: `openssl` (the **default** if
  none is selected), `boringssl`, `rustls`. Selecting `rustls` avoids the
  OpenSSL/BoringSSL `-sys` crates (measured in a scratch crate).
- `pingora-rustls` is built with rustls features `ring`, `tls12`, `logging`,
  `std`, and its code installs a default `ring` provider. So the proxy's TLS
  would use **ring**, TLS 1.2 and 1.3, and X.509. It would **not** offer
  `X25519MLKEM768`, which only aws-lc-rs provides in this project.
- The re-exports include `ResolvesServerCert` and `sign`, so a certificate
  resolver and a signing key that delegates to an external signer are
  expressible. Whether this works with Pingora's listener setup, and whether
  the provider can be swapped to aws-lc-rs, is **not verified** (research
  item R1 below).

Effects to decide, not to assume away:

1. No post-quantum key exchange on the edge. What crosses it (operator control
   API, session WebSocket) is exposed to harvest-now-decrypt-later by a
   recorded session. Needs an explicit risk acceptance or a different edge
   stack.
2. `ring` next to `aws-lc-rs`: both are already in `Cargo.lock` (node-transport
   README), but `deny.toml` names rustls with aws-lc-rs as the TLS policy.
3. Edge TLS private key custody. `HarwKeyPurpose` (masterplan section 4) has
   no TLS server purpose. If the key lives in the KMS, a purpose and a usage
   policy are needed so the KMS does not become a signing oracle (section 5):
   TLS `CertificateVerify` signing is a bounded transcript, but it must be
   modeled as its own purpose, not as a generic `sign`. If the key is a file,
   it contradicts the no-key-files direction of the node plane and needs its
   own custody rules (permissions, rotation, audit).

## 2. Authentication chain

```text
client --TLS--> proxy (connection and route admission only)
                  |  strips every x-harw-* header
                  v
              auth gateway / AuthHub  --> trusted SecurityContext
                  v
              Hyper/Tower control service (harw-web, session host)
```

Rules (masterplan 14, 32, 33):

1. **The proxy is not an authority.** It never invents a principal, tenant or
   tier, and never from a source IP. Source IP can feed only coarse,
   pre-authentication connection limits (Pingora has an early
   `connection_filter` hook); it is never identity.
2. **Always strip `x-harw-*` from client requests**, not only a configured
   list. The masterplan names `x-harw-principal`, `x-harw-tenant`,
   `x-harw-tier`; a prefix rule also covers names added later. (Implemented in
   the policy core.)
3. **No plaintext identity headers from the proxy.** The earlier draft let the
   proxy set identity headers for loopback upstreams; that conflicts with
   section 14, which requires identity context forwarded by a proxy to be
   cryptographically bound. Generic proxy-set headers stay in the core for
   non-identity use only.
   How identity actually reaches a service is **an open design question**, and
   the existing types constrain it: `SecurityContext` (`harw-types`) is
   `Serialize` only, can be minted only by a `SecurityContextIssuer` (the
   Auth/Crypto Hub or the local peer resolver), and a deserialized
   `SecurityContextSummary` is explicitly not a context. So the context must not
   travel as a token that an upstream turns back into a `SecurityContext`.
   Candidates:
   - (a) the proxy stays out of identity: it forwards the request as a secure
     frame (masterplan sections 8 and 9; the `RequestAuthentication` key purpose
     signs secure frames, `harw-dod-encrypt`) and the control service obtains the
     context from AuthHub itself;
   - (b) AuthHub sits in the path as an authentication hop and the control
     service receives the context through its own in-process or Unix-socket
     resolver, never from a header.
   Neither involves a header the proxy sets. Choosing between them belongs to
   the AuthHub owners.
4. **Tier is not key authorization** (section 32). Route admission by tier is a
   coarse first gate. Whether the caller may use a given key or tenant is
   decided by AuthHub, after the proxy.
5. **Rate limiting after authentication is per principal;** before it, per
   connection. Both are bounded and fail closed.
6. **Open:** where the auth gateway runs (inside the proxy process as a
   Pingora filter calling AuthHub, or as a separate hop). Preference: a
   separate hop or a thin client call, so the proxy process holds no key
   material beyond the TLS key.

## 3. Load balancing

Pingora offers `pingora-load-balancing` (feature `lb`): weighted round robin,
random, FNV hash, consistent (Ketama) selection, TCP and HTTP health checks,
service discovery. The project's constraints change how it may be used.

1. **Sessions are not load-balanced.** A hosted session has exactly one writer
   on its owner node (hub plan section 5/5.1: single writer, `placement_generation`
   fencing, no cross-node failover in v1). Session traffic routes by directory
   lookup `SessionId -> owner node`, not by hash or round robin. A stale or
   unknown location fails closed (refresh), and an unreachable owner returns
   503 with a retry hint. A failover of session traffic to another node would
   create a dual writer and is forbidden.
2. **Stateless endpoints** (read-only control-plane queries, static assets) may
   use weighted round robin or least-loaded selection over a group of
   upstreams.
3. **Every upstream address passes the route scope check** (loopback by
   default; `harw_egress::classify`). Health checks go to the same checked
   addresses only; discovery cannot add an address the scope forbids.
4. **Health does not override authority.** Node state comes from
   `harw-netsec` (`Pending`, `Active`, `Draining`, `Drained`, `Revoked`). Only
   `Active` receives new routes; `Draining` and `Drained` receive none;
   `Revoked` is removed at once and its connections are cut. A healthy but
   revoked node is not routable.
5. **Retries:** only idempotent requests, bounded, never a WebSocket upgrade,
   never a request whose body was partly sent.
6. **Draining and reload:** graceful reload and connection draining for config
   changes and certificate rotation (Pingora supports zero-downtime upgrade;
   not exercised here).

Implemented as a pure core in `harw-reverse-proxy` (`UpstreamGroup`,
`Backend`, `may_retry`): admission by scope (same address-class rule as routes),
node-state admission through `harw_netsec::NodeState` (only `Active` and healthy
receives new traffic; `Revoked` connections are listed for cutting; health never
overrides node state), deterministic weighted selection from a caller-supplied
key, retry selection that skips tried backends, and conservative retry rules
(`GET`/`HEAD`/`OPTIONS` only, never an upgrade or a request with a sent body,
bounded). Active health checking, discovery and the directory lookup for session
routes are not implemented.

## 4. Encryption dynamics end to end

| Hop | Protection | Notes |
|---|---|---|
| browser -> proxy | X.509 TLS (ring, TLS 1.2/1.3 as shipped) | no PQ; key custody open (section 1) |
| proxy -> local service on the same host | loopback or Unix socket, kernel peer credentials | no TLS needed; the upstream must be reachable only from the proxy |
| proxy -> service on another node | **open.** Pingora's peer TLS is X.509-based; it cannot perform the node handshake | candidates: a local sidecar speaking `harw-node-transport` and reached over a Unix socket; or restrict the proxy to local upstreams and let nodes talk through node-transport only |
| node <-> node | `harw-node-transport` (PQ-hybrid TLS + pinned ML-DSA-65 handshake) | unchanged, no proxy |
| proxy/service -> secrets and keys | AuthHub / KMS (`harw-secrets`), key purposes (section 4), usage policy (section 5) | upstream credentials never live in the route table |

Key lifecycle the plan must cover before implementation: rotation of the edge
key without dropping connections, revocation, behavior when the KMS is
unavailable (new handshakes fail closed; established connections continue only
within a stated bound), and an audit record for every use of the edge key.

## 5. Dependency research brief

A reviewer was brought in for dependency research. Questions, with the state of
evidence:

| # | Question | State |
|---|---|---|
| R1 | Can Pingora's rustls path use the `aws-lc-rs` provider (and thus `X25519MLKEM768`)? Can the server key be a KMS-delegating `SigningKey`? | **open** (sources show a `ring`-only build and a default `ring` install) |
| R2 | Is `zstd-sys` avoidable? | `pingora-core` depends on `zstd`; no feature to drop it found; `proxy` also pulls `pingora-cache`. Not exhaustively checked. |
| R3 | Package count and new `*-sys` crates | measured: about 240 packages, `zstd-sys` new |
| R4 | `cargo deny check` (advisories, licenses, bans, sources) with Pingora in the lockfile | **not run** (tool not available in the authoring sandbox) |
| R5 | `xtask gates` `warden-*` and `arch` with Pingora in the lockfile | **not run** with Pingora; green on the current tree |
| R6 | RustSec advisories and maintenance state of Pingora and its direct dependencies | **not checked** |
| R7 | MSRV and toolchain fit | compiled with the repo toolchain (1.98.1) in a scratch crate; not otherwise checked |
| R8 | Maturity of the rustls backend vs openssl/boringssl in Pingora | **not evaluated** |
| R9 | Binary size, memory, latency overhead vs a hyper-based proxy | **not measured** |
| R10 | Hyper-based alternative: cost of owning upgrade, pooling, hop-by-hop handling, health checks | **not estimated** |

Verified in the code while writing this note: `HarwKeyPurpose` (10 purposes,
including `RequestAuthentication`) lives in `dod/crates/harw-dod-encrypt`; there
is **no** TLS-server purpose; `SecurityContext` is mint-only as described above.

## 6. Work order (replaces section 8 of `README.md` where it differs)

1. P0 (done): plan, policy core (routes, normalization, upstream classes, header
   hygiene including the `x-harw-*` strip).
2. P0b (done): pure `UpstreamGroup` core (scope and node-state admission,
   weights, deterministic selection, no I/O).
3. P1: owner decision on the dependency using the R1-R10 results.
4. P2: adapter over the chosen stack; loopback listener; local upstreams only.
5. P3: edge TLS (X.509) with decided key custody; auth gateway integration with
   signed `SecurityContext`.
6. P4: limits, retries, graceful reload, `ProxyEdge` profile, key lifecycle,
   gates.

## 7. Corrections to earlier statements

- `README.md` section 1 called the purpose an assumption. The masterplan (section
  33) defines it: browser/external edge in front of the control services, not
  between nodes.
- The crate README of `harw-node-transport` states that Pingora brings
  BoringSSL or OpenSSL `-sys` crates. With the `rustls` feature that is not what was
  measured here (no OpenSSL or BoringSSL; `zstd-sys` and `ring` instead). The
  README statement is true for Pingora's default TLS backend, and the
  conclusion (not between nodes) still stands, for the TLS-profile reasons
  above. That README should be corrected when this plan is accepted.
