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

## 1b. Field evidence (two real harw sessions, 2026-10-02 and 2026-10-05)

Two exported operator sessions (a gateway host, two Raspberry-class nodes, a
second server, all on one tailnet) show what the missing tooling costs in
practice. Names and addresses are left out here on purpose.

| Observation | What it tells us |
|---|---|
| A hand-written systemd unit for a reverse SSH tunnel failed with `Permission denied (publickey)` and **restarted ~18,400 times in one day**; the fix was another round of `sed -i` on the unit file | No supervised, rate-limited connection unit; no `doctor` that says "this unit has failed N times and why" |
| `harw serve` crash-looped because `default_provider = "openai"` named a provider that did not exist any more | Config that makes a listener die at start should be caught by `node doctor` before the service is enabled |
| `harw web` over the tailnet refused connections from the machine's **own** tailnet address (`is this node's own address`); the operator's first probe failed for that reason alone | The refusal is correct but unexplained; `status` must say "test from another device" |
| A node was to be attached "over mTLS/HTTPS, but it must **not compile or accumulate artifacts**" and had no `harw` installed | A **thin node** role is a requirement: install a prebuilt release binary (aarch64 included), run pods and workers, never build; builds happen elsewhere |
| The operator tried several SSH users and keys by hand against two targets, then fell back to a Cloudflare tunnel that had no `cert.pem` | Onboarding by trial and error; one `join`/`enroll` path with a clear failure reason is needed |
| Old SSH forwards on fixed ports were declared forbidden; the replacement is "the tunnel is the tailnet" | Reachability profile `tailnet` is the default recommendation, not an add-on |
| A sentinel socket existed with a running process but accepted nothing | `doctor` should probe sockets, not only check that they exist |
| A tailnet listed several machines offline for weeks next to active ones | `device list` needs `last seen`, and stale devices should be easy to revoke |
| A read-only "writer" agent could not touch an absolute path outside its workspace and could not run git; the task (resolving merge conflicts in another repo) simply could not run | Remote work on a node needs an explicit, per-node workspace root and a worker role with a shell, chosen at enrollment, not discovered by failing |

These turn into requirements: (R1) `node doctor` covers unit health, config
that prevents start, own-address refusal and socket probes; (R2) a `thin`
node role with `harw node install --from-mirror` (checksummed prebuilt
binary, no compiler); (R3) a generated, supervised service unit with
back-off instead of hand-edited units; (R4) enrollment fixes the node's
workspace root and worker role; (R5) `last seen` on every device.

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
| **N3a** | `device join --show` / `device add <keyfile|code>`: key transfer and pinning without a wire change; fingerprint display | no |
| **N3b** | One-time pairing endpoint (window, secret, rate limit), only if N3a is too manual | protocol review |
| **N4** | Reachability profiles incl. `tailnet` via `harw-tailscale`; address in the code | no |
| **N5** | `harw node enroll <ssh-target>` and `peer add|list|remove`; thin-node install from the release mirror (R2); generated supervised unit (R3) | SSH use is acceptable (§5.3) |
| **N6** | Mobile/SDK side of `join` (scan the code, store the key) | mobile key storage |
| **N7** | `HostProvisioner` trait + cloud-init template, dry-run only | providers (§5.1) |

Each cycle: own branch, own PR, tests without `unwrap`/`panic`, mutation check
on every security check, `xtask gates` green, honest note on what ran against a
fake and what against a real peer.

## 5. Decisions for the owner — checked against the code

**5.1 Pairing needs a pre-authentication step (decides the wire question).**
The server verifies a client's ML-DSA-65 signature against a public key it
already holds: `node-peers.conf` pins `node_id|public_key_hex`, and the key is
1952 bytes, i.e. 3904 hex characters (`harw-cli/src/session_listener.rs`).
`ClientHello` carries only `{version, client, server, client_nonce,
client_time_ms}` (`harw-node-transport/src/handshake.rs`), and
`AuthenticatedPeer` only `{node_id, protocol_version}`. A new device's key is
therefore **unknown to the host until someone transfers it**, and the device
cannot complete the handshake before that. Pasting 3.9 KB of hex by hand is the
pain point itself. Two ways, in this order:
(a) **No wire change:** `harw device join --show` prints the device's key as a
short fingerprint plus a file/QR with the full key; the host runs
`harw device add <keyfile|code>` which pins it and writes the registry record
(host-fixed tier). Both sides compare the fingerprint out of band. Ships in N3a.
(b) **One-time pairing endpoint:** a separate, rate-limited route on the TLS
listener, reachable only while a pairing window is open, that accepts the
secret plus the device key and nothing else; the host pins on `approve`. This is
a protocol addition and needs its own review; N3b, only if (a) is judged too
manual. The pairing message does **not** go into `ClientHello`, because that
message is signed by a key the server does not know yet.
Recommendation: (a) first.

**5.2 SSH.** `harw-tool-tunnel` exists only as a scaffold (local `-L` forwards,
no daemon, keyfile by reference, bounded back-off), and the field sessions show
hand-edited SSH units failing thousands of times while the tailnet connection
worked. Recommendation: `harw node enroll` **does not use SSH by default**. A
node is brought up by running one command on it (`harw node install` then
`harw device join`); SSH is an opt-in convenience (`--via-ssh`) that only runs
those same commands, through the existing sandboxed process path.

**5.3 Reachability for the phone.** The tailnet already provides the encrypted
path (devices direct and active in the field logs) and `harw web` already
serves on it; the one failure seen was the machine's own address. Recommendation:
`local` and `tailnet` only in N4; `public` waits for the reverse-proxy plan
(PL-89, PR #92) and stays off in `doctor`'s "recommended" list.

**5.4 Provisioning at a cloud provider (N7).** Nothing in the code creates
machines, and `harw-job-executor-oci`/`harw-placement-model` place work on
machines that already exist. Recommendation: do not build a provider
integration now; N7 stays a dry-run `HostProvisioner` plan until a provider and a
hard cost cap are chosen. This is the only decision that needs the owner.

**5.5 Hardware key for `owner` approval.** Secrets already go through the
sealed store with an Auth/Crypto Hub KEK (`harw-cli/src/secret_store.rs`), so a
hardware-key step would be an additional approver in the Hub, not a new
mechanism. Deferred as before; `device set-tier owner` already requires an
explicit `--yes-owner` in N1.

**Remaining owner decisions:** (a) pairing path (a) before (b), (b) SSH only as
opt-in, (c) whether any cloud provider is wanted for N7.

## 6. Out of scope

Billing, multi-tenant hosting for third parties, and anything that lets a
device choose its own tier.
