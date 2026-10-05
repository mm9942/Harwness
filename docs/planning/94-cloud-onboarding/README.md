---
id: PL-94
title: "Cloud onboarding: connect, secure and add hosts and nodes without editing files"
status: proposed
date: 2026-10-05
baseline: dev@ee1bb10
---

# Cloud onboarding (PL-94)

Goal: a person can bring up a gateway, connect a phone / laptop / second
machine to it and add a new host or node **with commands and a short pairing
code**, in minutes, without hand-editing `node-devices.conf`, `node-peers.conf`
or `config.toml`, and without a way to end up half-open.

## 1. What exists (evidence in the code)

| Piece | Where | State |
|---|---|---|
| Own-cloud ingress, PQ-TLS + ML-DSA mutual handshake | `harw-node-transport`, `harw-node-listener` | done, default off, loopback only |
| Node identity key in the Auth/Crypto Hub (`harw.node-identity/<node_id>`) | `harw-node-transport::authhub_signer` | done; the process never sees the private half |
| Device registry `node-devices.conf` (`node_id|device_id|tenant|tier|status|label`), re-read on every handshake, live revocation | `harw-node-listener::identity` (`DeviceRegistry::{enroll,records,record_for,mark_revoked}`) | library only, **no CLI** |
| Pinned peers `node-peers.conf` | `harw-cli::session_listener` | operator-written file |
| `[session_listener]` (enabled, listen, allow_non_loopback, node_id, tier, approval_device) | `harw-config::session_listener_toml` | global-only, hand-edited |
| Local UDS ingress, `harw attach`, session client, mobile client, SDK `remote` | `harw-session-com`, `harw-cli::attach_cmd`, `harw-session-client`, `harw-mobile-core`, `harwness-sdk` | done |
| Tailscale helper | `harw-tailscale`, `harw tailscale` | exists |
| Telegram pairing (one-time code bound to the sender id) | `harw-cli::connect` | the one pairing flow that exists; a good pattern |

## 2. The gaps (why it is hard today)

1. **No command to manage devices.** Enrolling or revoking means editing a
   pipe-separated file by hand; a typo is silently ignored (fail closed, but
   invisible).
2. **No key bootstrap.** The node key must be created in the Auth Hub and its
   public half copied to every peer by hand.
3. **No pairing.** There is no exchange of "this device, this tenant, this
   tier" that the operator confirms on the host and the device accepts.
4. **Three config surfaces** (`config.toml`, two `.conf` files) with no
   single "is it set up right?" check.
5. **No host/node provisioning.** A new machine has to be installed, given a
   home, a key, a listener config and peers manually.
6. **No visibility.** Which devices are enrolled, which are connected now,
   when did one last connect, what tier does it hold.
7. **Safe defaults are good but invisible**: loopback bind, observer tier,
   approvals parked. The user cannot see *why* a connection is refused.

## 3. Design

### 3.1 One command family: `harw node`

```text
harw node init <node-id>            # create the node key in the Auth Hub (once), write [session_listener]
                                    # (disabled, loopback), print the public key fingerprint
harw node status                    # listener config, key fingerprint, bind, enrolled/connected devices, problems
harw node doctor                    # checks with one-line fixes (see 3.4)
harw device list [--json]           # enrolled devices: id, label, tenant, tier, status, last seen
harw device pair [--tier T] [--label L] [--ttl 10m]
                                    # prints a one-time pairing code (and a QR/URI) valid for TTL
harw device approve <code>          # host side: confirm the request that used the code (shows fingerprint)
harw device set-tier <device> <tier>
harw device revoke <device>         # live: takes effect on the next handshake and drops live connections
harw peer add|list|remove           # pinned peer nodes (gateway <-> gateway)
```

All of them write through the existing `DeviceRegistry` (atomic write, mode
0600), never by string-editing files, and print what changed.

### 3.2 Pairing protocol (device joins a host)

1. Host: `harw device pair --tier observer --ttl 10m` creates a **one-time
   secret** (>= 128 bit, stored only as a digest, bound to tier/label/ttl) and
   prints a code `harw-pair:<host-fingerprint>:<secret>` plus a QR.
2. Device: `harw device join <code> --name "phone"` (or the mobile app scans
   the QR). It generates its own node key (kept in its Auth Hub / platform
   keystore), connects to the host **pinned to `<host-fingerprint>`** from the
   code (no trust-on-first-use of an unknown key), and presents the secret
   inside the handshake channel.
3. Host: shows the device's key fingerprint and the requested tier;
   `harw device approve <code>` (or `--auto` for pairing windows the operator
   opened explicitly) writes the record and consumes the secret.
4. The secret is single use, expires, rate-limited (failed attempts burn it),
   and never grants more than the tier fixed at step 1. The tier is never taken
   from the device's request.

Security properties to prove with tests: single use, expiry, replay after
consume refused, wrong fingerprint refused, no tier escalation, a pairing
window does not enable the listener on its own, secrets never logged, a
revoked device cannot re-pair without a new code.

### 3.3 Reachability (making the connection easy)

Offer exactly three profiles, selected by `harw node init --reach <p>`:

| Profile | Bind | How the device finds it | Default |
|---|---|---|---|
| `local` | loopback / UDS | same machine only | yes |
| `tailnet` | the tailnet address (via `harw-tailscale`) | MagicDNS name, no open port | recommended for phone/laptop |
| `public` | a given address, needs `allow_non_loopback` and a TLS-terminating proxy or direct PQ-TLS | DNS name | opt-in with a loud warning in `doctor` |

The code printed by `pair` carries the address for the chosen profile.

### 3.4 `harw node doctor` checks

Key exists and is usable; listener enabled and bound as configured; bind is
loopback or explicitly allowed; tier is the intended one; registry parses
(reports lines the loader would ignore, which today are silent); peers file
parses; no device without a label; revoked devices still holding a live
connection; clock skew vs. a peer; expired pairing windows left open.

### 3.5 Adding a new host or node

Two levels, in this order:

1. **Join an existing machine** (SSH access exists): `harw node enroll <ssh-target>`
   copies nothing secret: it runs `harw node init` remotely over SSH, collects
   the public fingerprint, pins it in `node-peers.conf` on both sides, and
   prints the result. Requires `ssh` access; uses the existing sandboxed
   process path, no new spawn mechanism.
2. **Create a machine at a provider** (Hetzner, AWS, ...): out of scope for
   the first rounds. It needs provider credentials and cost control, which is a
   decision for the owner (see §5). The interface is a `HostProvisioner` trait
   with a dry-run plan first; the first implementation would be a cloud-init
   template that installs `harw`, runs `node init` and pairs back.

### 3.6 Hardening that should ship with it

- Device records gain `created_at`, `last_seen`, `paired_by` (audit); the
  parser stays tolerant of the old six-field lines.
- Per-device rate limit and a connection cap on the listener.
- Registry and pairing files: owner-only, atomic, checked on read (reject a
  group/world-writable file instead of silently using it).
- Optional second factor for `approve` on the host (the deferred secret-store
  idea: hardware key); designed in, not built now.
- All refusals carry a stable reason code that `harw node status` shows, so
  "why did it refuse me" is answerable without reading logs.

## 4. Cycles

| Cycle | Content | Needs a decision |
|---|---|---|
| **N0** | This plan | no |
| **N1** | `harw device list|set-tier|revoke`, `harw node status` on top of `DeviceRegistry` (read and change paths, tolerant new fields) | no |
| **N2** | `harw node init` (key bootstrap, config writer that edits `[session_listener]` through the config crate, not by string) and `harw node doctor` | no |
| **N3** | Pairing: one-time secret store, `device pair|approve|join`, protocol tests | wire format of the pairing message (§5.2) |
| **N4** | Reachability profiles incl. `tailnet` via `harw-tailscale`; address in the code | no |
| **N5** | `harw node enroll <ssh-target>` and `peer add|list|remove` | SSH use is acceptable (§5.3) |
| **N6** | Mobile/SDK side of `join` (scan the code, store the key) | mobile key storage |
| **N7** | `HostProvisioner` trait + cloud-init template, dry-run only | providers (§5.1) |

Each cycle: own branch, own PR, tests without `unwrap`/`panic`, mutation check
on every security check, `xtask gates` green, honest note on what ran against a
fake and what against a real peer.

## 5. Decisions for the owner

1. Which providers, if any, should N7 target (Hetzner, AWS, any)? Is cost
   control (a hard instance/budget cap) required before a provisioner may
   create anything?
2. Pairing message: carried inside the existing node-transport handshake
   (preferred: no new port) or as a separate short-lived HTTP endpoint?
3. May `harw node enroll` use the user's SSH access, or must nodes always be
   brought up by running a command locally on them?
4. Default reach profile for a phone: `tailnet` only, or also `public` behind
   a reverse proxy (PL-89)?
5. Is a hardware-key confirmation (YubiKey / Ledger) a requirement for
   `approve` of an `owner`-tier device, or later?

## 6. Out of scope

Billing, multi-tenant hosting for third parties, and anything that lets a
device choose its own tier.
