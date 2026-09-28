# Tailscale access

> Status: implemented · Last reviewed: 2026-09-28

Reach harw's control plane from your own devices (for example a phone)
through your tailnet, without opening a public port.

## Requirements

- `tailscaled` runs on the harw host and is connected (`tailscale up`).
- harw reads its LocalAPI socket (`/var/run/tailscale/tailscaled.sock` or
  `/run/tailscale/tailscaled.sock`; override with `HARW_TAILSCALE_SOCKET`).
  harw only reads (`status`, `whois`), which `tailscaled` allows for
  unprivileged users.

## Check the node

```sh
harw tailscale status
```

shows the socket, the connection state, this node's MagicDNS name and
tailnet addresses.

## Open the control plane to the tailnet

```sh
harw web --tailnet                 # port 8443
harw web --tailnet --tailnet-port 9443
```

Besides its usual Unix socket, `harw web` then:

1. listens on this node's **tailnet address only** (never `0.0.0.0`),
2. asks `tailscaled` (`whois`) who is behind every connection and refuses
   addresses outside the tailnet ranges, this node's own addresses and
   anything `whois` cannot identify (fail closed),
3. forwards admitted connections to a second socket `tailnet.sock`
   (mode 0600) next to the main socket.

Every device of the tailnet gets the tier **Operator** (chat, agents, jobs,
approvals). Host and sudo actions still follow the approval mode. The
owner tier stays with local users of the main socket: `tailnet.sock` only
accepts harw's own UID and always resolves it to Operator, so reaching it
locally can only lower rights.

The login and node of every admitted connection are logged
(`tailnet.proxy.admitted`). Traffic inside the tailnet is encrypted by
WireGuard; the listener speaks plain HTTP on the tailnet address.

From the phone, open `http://<node>.<tailnet>.ts.net:8443` (the address
`harw web --tailnet` prints) or use the API with any HTTP client.
