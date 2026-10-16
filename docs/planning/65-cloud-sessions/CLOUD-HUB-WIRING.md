---
id: PL-65-CLOUD-HUB-WIRING
title: "Cloud Hub — what exists, what is unwired, and the wiring order"
status: proposed
date: 2026-10-01
baseline: dev@70faabed9182834792a5f36e67cd80535792542a
related:
  - CLOUD-HOME-HUB-PROFILES-V2.md
  - ../66-placement/README.md
  - ../67-containers/on-demand-worker-boot.md
---

# Cloud Hub wiring

Design source of truth: `CLOUD-HOME-HUB-PROFILES-V2.md` (PR #82, patched in
#83). This note records what the code actually does today and the concrete
wiring order.

## 1. Observed state on `dev`

- `harw-session-host` (single writer, replay, approvals) and `harw-session-ws`
  exist with tests.
- **`SessionHost::open` has no production caller.** Outside tests and the
  crate itself nothing opens a host. `harw-cli` does not depend on
  `harw-session-ws`. So the control plane is built but not composed into any
  binary: there is no daemon, no socket, no `harw attach`.
- `harw-node-transport` provides the authenticated handshake used for nodes.
- No directory, no node registry, no deployment profile type exists.

Consequence: "wire the hub" means composition work, not protocol work.

## 2. Wiring order (maps to H1-H6 in the hub plan)

| Step | Deliverable | Where | Notes |
|---|---|---|---|
| W1 | `DeploymentProfile` enum + service-mount table | new composition module, not in agent IR | tests: each profile mounts only its services |
| W2 | Local daemon opens `SessionHost`, serves `harw-session-ws` on a UDS | `harw-cli` (service command) | kernel-derived local identity; drain/restart |
| W3 | UDS `SessionPort` client | new thin crate, no `harw-core` dependency | cursor reconnect, bounded queues |
| W4 | `harw attach` + session picker (Continue / New) | `harw-tui`, `harw-cli` | TUI uses `SessionPort`, no `RuntimeAssembly` |
| W5 | Directory + node registry with `placement_generation` | hub profile | cache rebuilt from node reports; no cross-node failover in v1 |
| W6 | Remote attach over `harw-node-transport` | session-ws mount | tenant mandatory; no bearer/IP fallback |

W2-W4 is the shortest path to something a person can use (close terminal,
reopen, same session). W5/W6 only add routing and remote reach.

## 3. Defaults and discoverability

The profile and hub settings must follow `TOOL-DISCOVERABILITY.md`: every
default appears in the generated defaults TOML, and the hub services appear in
the tool/operation index with their profile gating.

## 4. Not decided here

Storage replication, live migration, multi-tenant admin model beyond the
separation already described in the hub plan.
