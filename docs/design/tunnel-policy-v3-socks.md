# HARW Tunnel Policy — v3 addendum: SOCKS (dynamic forwarding)

> Status: planned contract; nothing implemented. Extends `tunnel-policy-v1.md`,
> whose section 8 excludes SOCKS (`ssh -D`) until it has its own policy.

## 1. Why `ssh -D` is not allowed

`ssh -D` makes OpenSSH itself the SOCKS server and lets any client of that port
pick any destination reachable from the SSH server. There is no hook in which
HARW could apply the target allowlist of v1 section 2. `-D` would turn a
policed tunnel into an open relay. It stays forbidden.

## 2. Permitted design: a policy-enforcing SOCKS5 front

HARW provides the SOCKS5 server, not OpenSSH:

- The repo already has one: `EgressProxy`/`serve` in `harw-egress` (RFC 1928,
  Unix socket only, `EgressPolicy` per request). The tunnel SOCKS front reuses
  its parser and policy checks instead of adding a second parser.
- For each permitted `CONNECT`, HARW opens **one SSH channel per connection**
  (`ssh -W host:port`-style stdio forwarding or an in-process channel), after
  the policy check. OpenSSH never sees a destination the policy did not allow.

## 3. Rules

1. **Local listener:** Unix socket or loopback only (v1 section 3). No
   `0.0.0.0`.
2. **Same allowlist and address-class rules as v1 section 2,** applied per
   `CONNECT`, at start and at every reconnect. Private, loopback and cloud
   metadata targets are denied unless the policy names them (metadata is never
   allowable).
3. **Only `CONNECT`.** `BIND` and `UDP ASSOCIATE` are refused.
4. **Authentication method `NO AUTH` only on a private Unix socket** or
   loopback with peer-credential check; anything else is refused.
5. **Domain-name targets** (`socks5h`) are resolved by the SSH server, which
   cannot be verified locally (the same gap as hostname targets in v1; see the
   `allow_remote_resolution` opt-in in `harw-tool-tunnel`). Default: literal
   IP targets only; names need the explicit opt-in and are documented as not
   address-class checked.
6. **Approval** before the first start and on any change of allowlist, listener
   or SSH identity; bound to the effective configuration with an injective
   encoding (as in v1/v2 implementations).
7. **Bounded:** connection limit, per-connection idle and connect timeouts,
   byte accounting for the audit record.
8. **Lifecycle** through the job/work-driver like other tunnels; no own daemon.
9. **Egress policy** for the SSH hop itself as in v1 section 5.

## 4. Not covered

Remote dynamic forwarding, SOCKS over the Cloudflare path, UDP, authentication
other than the local peer credential.
