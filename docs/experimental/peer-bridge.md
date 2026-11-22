# Peer bridge (experimental)

## Purpose and threat model

External agents (Claude Code, Codex) can today only type into the harw tmux
pane. That gives no sender identity, no delivery guarantee and no trigger. The
peer bridge adds:

- authenticated identity through the MCP principal of the caller,
- a durable inbox (peer to harw) and outbox (harw to peer),
- a wake mechanism that can start an agent turn.

Peer text is untrusted DATA. It never counts as an approval and cannot change
permissions. Everything below is experimental and may change.

## Layout

Base directory `<profile_dir>/peer-bridge/` (mode 0700):

| Path | Purpose |
|---|---|
| `inbox/<session>.jsonl` | Queue (0600). Serve appends, the TUI reads and truncates. |
| `inbox/<session>.live` | JSON `PeerLiveInfo` (0600). The TUI holds an exclusive lock for its lifetime; the lock means "live". |
| `outbox/<peer>.jsonl` | Durable outbox (0600), rotated to `.jsonl.1`. Source for `harw_peer_poll`. |
| `spool/<peer>.jsonl` | Trigger spool (0600), rotated to `.1`. Watch with `tail -F`. |

`<peer>` is 1..=64 bytes of `[A-Za-z0-9_-]`.

## Configuration

All `peer_bridge` keys are global-only (not overridable per project).

```toml
[mcp_listener]
listen = "127.0.0.1:1337"   # default; the old msg-server used this port too,
                            # change `listen` if it is still running

[[mcp_listener.principals]]
id = "claude-code"
credential_ref = "env:HARW_MCP_TOKEN"

[peer_bridge]
enabled = true
max_bytes = 16384            # per message, max 32768
queue_cap = 64               # inbox lines per session, max 1024
outbox_max_bytes = 1048576   # rotation threshold, min 65536
webhook_allow_hosts = []     # non-loopback webhook hosts (no '/')

[[peer_bridge.peers]]
id = "claude-code"
principal = "claude-code"    # must equal an mcp_listener principal id
connectors = ["spool", "mcp", "webhook"]
webhook_url = "http://127.0.0.1:9000/harw"   # only with the webhook connector
secret_ref = "env:HARW_WEBHOOK_SECRET"       # required for webhook
wake = true
wake_burst = 3
wake_interval_secs = 30
max_sends_per_minute = 30
```

## MCP tools

Visible only to principals that are configured peers. Results are MCP content
text containing JSON.

- `harw_peer_sessions` with args `{}` returns
  `{"sessions":[{"session","started_at","cwd"}]}` (live sessions only).
- `harw_peer_send` with args `{session, text, id?}` (closed object; `id` is
  1-128 of `[A-Za-z0-9._:-]`) returns
  `{"status":"queued","session","id","wake":bool}`. A repeated `(peer, id)`
  returns `{"status":"duplicate",...}` and is not appended again.
- `harw_peer_poll` with args `{after?: u64}` returns
  `{"messages":[...],"next":u64}`. It needs the `mcp` connector and returns at
  most 50 messages.

| Code | Meaning |
|---|---|
| -32001 | principal is not a configured peer |
| -32602 | invalid arguments, e.g. `invalid harw_peer_send arguments: unknown session` |
| -32003 | peer rate limit exceeded |
| -32004 | session is not live |
| -32005 | peer inbox is full |
| -32006 | peer has no mcp connector |
| -32000 | peer bridge storage error |

## Delivery into harw

Queued messages appear in the TUI as
`[Peer-Nachricht von <from> (id <id>)]` followed by an untrusted DATA block.
With `wake = true` a message may also start an agent turn; wakes are limited by
a token bucket (`wake_burst`, one token per `wake_interval_secs`).

## peer.send (harw to peer)

The UIA root agent gets the `peer.send` operation with args
`{to, text, reply_to?}`. It writes one JSON line
`{v, seq, id, from:"harw", to, session, sent_at, text, reply_to}` to the outbox
and the spool, and optionally posts the webhook. It returns
`{"status":"sent","seq","id","connectors":{"spool","webhook","mcp"}}`, each
connector reported as written/queued/available or `off`.

## Webhook

POST of the exact line bytes with headers:

- `Content-Type: application/json`
- `X-Harw-Timestamp: <unix secs>`
- `X-Harw-Signature: sha256=<hex>`

The signature is HMAC-SHA256 over `"<X-Harw-Timestamp>.<body>"` with the
secret. Only loopback hosts are allowed unless listed in
`webhook_allow_hosts`. No redirects, no proxy, 10 s timeout, up to 3 attempts
(1 s / 2 s backoff) on transport errors, 429 and 5xx.

Verification (Python):

```python
import hmac, hashlib, time

def verify(secret: bytes, ts: str, body: bytes, header: str) -> bool:
    if abs(time.time() - int(ts)) > 300:
        return False
    digest = hmac.new(secret, f"{ts}.".encode() + body, hashlib.sha256).hexdigest()
    return hmac.compare_digest("sha256=" + digest, header)
```

## Claude Code hookup

`.mcp.json`:

```json
{"mcpServers":{"harw":{"type":"http","url":"http://127.0.0.1:1337/mcp",
  "headers":{"Authorization":"Bearer ${HARW_MCP_TOKEN}"}}}}
```

Trigger: run this as a background/monitor command; each line is one trigger.

```sh
tail -F ~/.harw/profiles/<profile>/peer-bridge/spool/claude-code.jsonl
```

## Codex hookup

`~/.codex/config.toml` (check that your Codex version supports streamable HTTP):

```toml
[mcp_servers.harw]
url = "http://127.0.0.1:1337/mcp"
bearer_token_env_var = "HARW_MCP_TOKEN"
```

Optionally use the webhook connector towards a local listener.

## Security notes

- Webhooks are loopback-only unless allow-listed.
- Per-peer rate limits; dedup on `(peer, id)` over a window of 1024.
- Size caps (`max_bytes`, `queue_cap`, `outbox_max_bytes`).
- Peer text is untrusted data: no approvals or permission changes via peers.
- Secrets only via `secret_ref` / `credential_ref`, never inline.
- Files are 0600, directories 0700.
