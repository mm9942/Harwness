# HARW Tunnel Policy — v2 addendum: Cloudflare (`cloudflared`)

> Status: planned contract · extends `tunnel-policy-v1.md`. Implemented so far:
> the pure policy core in `harw-tool-tunnel/src/cloudflare.rs`. Process
> start, job integration and retry are not implemented.

v1 allows only local SSH `-L` forwards and states that any extension needs its
own policy and approval review. A Cloudflare tunnel does the opposite of `-L`:
it makes a **local service reachable from the internet**. It is therefore a
separate risk class, not a variant of v1.

## 1. Modes

- **Quick**: `cloudflared tunnel --url <origin>`; random `*.trycloudflare.com`
  address, no account. The public address is only known after start and is read
  from the process output (`parse_quick_url`, accepts that domain only).
- **Named**: `cloudflared tunnel run --token-file <path>`; public hostnames are
  configured on the Cloudflare side. The policy stores the **expected**
  hostnames so that the approval shows them. They are exact names, no wildcards.
  Whether a hostname is really served is not verifiable from the client and is
  not claimed.

## 2. Rules

1. **Origin is loopback only**: `http(s)://127.0.0.1:PORT` or `[::1]:PORT`,
   no path, no userinfo, no `localhost` (not resolved), no other address.
2. **Origin port allowlist** (explicit). An empty list allows nothing.
3. **Approval before every first start and on any change** of origin, mode,
   expected hostnames or port allowlist. Quick mode is approved as "public,
   unknown hostname". Reconnect reuses an approval only for an unchanged config.
4. **Token only as file reference.** No `--token <value>` (argv is visible to
   other processes), no token in logs, errors, approval text or artifacts. The
   reference must be an absolute path; anything else is rejected because it may
   be the token itself. The path is not part of the approval string.
   Check against the installed `cloudflared` version that `--token-file` is
   supported before relying on it.
5. **No auto-update** (`--no-autoupdate`): the binary must not replace itself.
6. **Job-owned lifecycle**, bounded retry/backoff, exhausted retry is a visible
   error state; no own daemon. Same rule as v1.
7. **Egress policy** applies to the connection to Cloudflare as in v1 §5.

## 3. Not covered

Cloudflare API calls (creating tunnels/DNS), Access policies, WARP, TCP/UDP
ingress other than an HTTP(S) origin. Each needs its own review.
