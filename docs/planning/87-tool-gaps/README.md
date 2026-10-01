---
id: PL-87-TOOL-GAPS
title: "Internal tool gap report — CLI features vs. tool surface"
status: report
date: 2026-10-01
baseline:
  repo: mm9942/Harwness
  ref: dev
  sha: 70faabed9182834792a5f36e67cd80535792542a
---

# Internal tool gap report

Scope: which internal tools exist behind the current CLI/operation surface,
which are only scaffolds, and what should be built next. Evidence is the
`dev` baseline above. Nothing here claims behavior that was not read in code.

## 1. Method

- CLI subcommands: `harw-cli/src/cli/mod.rs` (`enum Command`).
- Slash operations: `harw-ops/src/*`.
- Agent-facing tools: `harw-tool-*` crates and the capability catalog
  (`harw-registry-defaults/src/capability_catalog.rs`, `profile.rs`).
- Stub detection: grep for scaffold / `todo!` / `unimplemented!` markers and
  empty crates.

Limits: this is a static survey. Tool counts per crate are by name in the
catalog, not by running the registry.

## 2. Tool surface that exists (by family)

| Family | Tools (catalog names) | State |
|---|---|---|
| fs | read, write, edit, list, glob, grep, search | implemented (`harw-tool-fs`) |
| shell/latex | `shell.exec`, `latex.build/check/template` | implemented (`harw-tool-shell`) |
| job | start, status, logs, wait, stop, list | implemented (`harw-tool-job`) |
| web | fetch, search, crates_io, docs_rs | implemented (`harw-tool-web`) |
| browser | open, observe, find, act, wait, events, close | implemented (`harw-tool-browser`) |
| deps / explore / lens / doc / plan | graph, locked, source_*; tree, find, projects, relations; `lens.ask`; `doc.read_pdf`; plan.enter/exit/write | implemented |
| process | `process.list`, `process.kill`; `process.spawn` is a capability name | list/kill implemented; spawn is authority-registered, not a tool in `harw-tool-process` |
| gateway | status, health, logs, drain | implemented (ops/gateway) |
| agents / skills / work_driver / matrix / kanban / palace / diary / workbench | various | implemented in registry-defaults |
| **tunnel** | `tunnel.start/status/stop/list` | **scaffold only** |

## 3. Gaps

### G1 — `harw-tool-tunnel` is an empty scaffold (highest confidence)

- `harw-tool-tunnel/src/lib.rs` and `tools.rs` are doc comments only; the
  crate has no dependencies and no code.
- The four tool names are already registered in the capability catalog and
  `profile.rs`, so the registry advertises tools that do not exist.
- A policy contract exists: `docs/design/tunnel-policy-v1.md` (local `-L`
  forwards only, loopback bind, target allowlist, approval on first start and
  on allowlist change, key-file reference never key content, egress policy).
- The crate is classified in `xtask/arch-policy.toml`.

Consequence: advertised capability without implementation. Buildable now in
two layers: (a) a pure policy core (validation, canonical approval string,
argument builder, redaction), (b) job/work-driver integration and retry.

### G2 — Worker boot is planned, not built

`WorkerBootCoordinator`, `WorkerBootMode` and the shared node-wide admission
mechanism are only in plan #80. `max_running_jobs` is per session
(`JobManagerConfig`), so no node-wide limit exists today.

### G3 — Typed job provenance missing for agent children

`JobOrigin` has `call_id`, `tool`, `owner_agent` (a display name) but no typed
child-session link. Required by plan #81 (W20-A) before terminal aggregation.

### G4 — Operator commands without a tool/operation counterpart

`kill`, `tailscale`, `connect`, `sandbox`, `uia`, `catalog`, `classify`
exist as CLI subcommands. This is **not** automatically a gap: several are
operator-only by design. Open question for the owner: which of these should
also exist as governed agent tools or operations? `kill` is the sensitive one
(see plan #81: no identity-bound preview/execute API yet).

### G5 — Planned worker CLI is unmerged

`harw worker build|test|exec|status` and the Podman executor are in #74,
which is open. Tools around it depend on that merge.

## 4. Proposed order

1. **G1 tunnel policy core** — self-contained, no new dependencies, testable
   in isolation, unblocks honest registry state.
2. G3 provenance seam (small, additive, needed by #81).
3. G2 admission mechanism (needed by #80 and #82 H7).
4. G1 job/work-driver integration (needs job lifecycle decisions).
5. G4 after owner decision.

## 5. Out of scope here

No runtime behavior of existing tools is changed by this report.
