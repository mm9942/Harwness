# Review gate: Pingora (edge proxy)

Status: **not a dependency today.** No `pingora*` crate is in any manifest or
in `Cargo.lock`. Pingora appears only in plans: Crypto Infrastructure
Masterplan v2 §12 and §33 place it at the network edge (browser/external
ingress), and `harw-node-transport/README.md` records "No Pingora" for
node-to-node traffic because it would add `-sys` TLS crates.

Apply this checklist to any PR that adds a `pingora*` dependency or an edge
crate that uses it.

## Dependency

- Version at or above the fixes for the March 2026 advisories
  (GHSA-xq2h-p299-vjwv, GHSA-hj7x-879w-vrp7, GHSA-f93w-pcj3-rggc). Check the
  affected and patched ranges on
  <https://github.com/cloudflare/pingora/security/advisories>; the 0.8.0 fix
  version was taken from secondary sources.
- Exact version pin, in its own edge crate or behind a feature. It must not
  appear in `harw-node-transport`, `harw-core` or anything in the
  `harw-warden` runtime closure (`xtask gates`: `warden-dependency-budget`
  caps that closure, `warden-no-c-build` forbids C builds).
- TLS backend is an explicit choice (`openssl`, `boringssl`, `s2n-tls` or
  `rustls`; upstream labels rustls experimental). `-sys` crates need a
  `deny.toml` review and a stated reason.
- MSRV: upstream is 1.85 on a rolling six-month policy; check it against
  `rust-toolchain.toml`.
- Pingora's cache stays off (experimental API; the default cache key once
  ignored host and scheme).

## Behavior (masterplan §33)

- Untrusted identity headers are stripped before the request reaches AuthHub.
  Test that a client-supplied identity header never arrives upstream. Use
  `request_filter` to reject and `upstream_request_filter` to modify headers.
- No `Principal` is derived from the peer IP.
- The Harw runtime never writes Pingora config; the network daemon does
  (§12).
- The hop behind Pingora is strict about HTTP/1 framing. A smuggled second
  request must not reach the `harw-session-ws` Upgrade path.
- Only one ingress proxy is authoritative for header stripping and rate
  limiting. If a `cloudflared` tunnel (PR #88) also fronts the hub, document
  which one.
