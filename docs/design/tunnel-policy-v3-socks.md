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

1. **Local listener:** a private Unix socket only in v1. A TCP loopback
   listener is not permitted in v1: a TCP peer carries no kernel credentials,
   so it would need a real authenticated SOCKS method (for example RFC 1929
   username/password with a secret reference), which this addendum does not
   specify. In any case never `0.0.0.0`.
2. **Same allowlist and address-class rules as v1 section 2,** applied per
   `CONNECT`, at start and at every reconnect. Private, loopback and cloud
   metadata targets are denied unless the policy names them (metadata is never
   allowable).
3. **Only `CONNECT`.** `BIND` and `UDP ASSOCIATE` are refused.
4. **Authentication method `NO AUTH` only on a verified private Unix socket.**
   The security boundary is filesystem permissions (DAC): the socket is `0600`
   inside a `0700` directory owned by the expected UID, and the caller of
   `harw-egress` must verify exactly this before serving it (the existing
   `harw-egress` contract, see the `run` doc comment in `proxy.rs`).
   `EgressProxy` itself performs **no** peer-credential check: it accepts the
   `UnixStream` and starts `run_session` immediately, and the crate contains no
   `peer_cred`/`SO_PEERCRED` use. An implementation must therefore not assume
   the proxy verified the connecting UID. **Required follow-up before `NO AUTH`
   may be offered on any socket whose path or permissions are not verified:**
   an `SO_PEERCRED` check (the peer UID must equal the allowed owner, checked
   before each session) in `harw-egress`, with tests for a foreign-UID peer and
   a wrongly-permissioned socket. On any other listener `NO AUTH` is refused.
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
other than verified socket permissions (peer-credential checks are the follow-up
named in rule 4).
