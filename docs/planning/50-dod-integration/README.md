# DoD Umbrella Workspace Integration — Planning Compartment

> Status: PARTIALLY LANDED — PL-60 workspace merge (Phase 4) landed; Phase 5 TCB budgets partly enforced (`xtask gates`); the Harw-DoD suite surfaces of PL-97 D1–D5 landed on draft PR #154, see [Harw-DoD suite](#harw-dod-suite-pl-97) below

This compartment exists so the DoD workspace merge is not hidden inside the general ecosystem plan.

## Current (after PL-60)

- DoD crates under `dod/crates/` are explicit members of the root workspace
  (separate commented block in the root `members`); `exclude = ["dod"]` is gone.
- `dod/Cargo.toml` and `dod/Cargo.lock` are retired; the root `Cargo.lock` is
  the single resolution record.
- Security boundaries are enforced by `xtask gates` (`edges`, `privileges`,
  `warden-deps`, `warden-cbuild`, `arch`) and the package-scoped CI job
  `dod`, not by a build-domain split.
- Merge plan, version/lock diffs and TCB invariants:
  `../../architecture/dod-workspace-merge-plan.md`.

## Harw-DoD suite (PL-97)

"Harw-DoD (Detect · Orient · Defend)" is the suite name in human-facing
text since PL-97 D1; crate, binary, unit, socket, group and path names are
unchanged (`harw-dod`, `harw-sentinel`, `harw-dod.target`, …). What PL-97
landed on `ccr-54aa3dfd-o46he2` (draft PR #154 against `dev`), per work
package ([PL-97 §9](../97-harw-dod-mission-control-chain/README.md#9-landed-state-x1-2026-10-29),
ledger `MIG-016` and `MIG-019`):

| WP | Commit | What |
|---|---|---|
| D1 | `30b44321` | Suite naming; `harw-sentinel.service` with `--config`, `--findings-export` and the atomic `--health-export`; findings line `schema_version: 2` + optional `scope`; group `harw-dod-readers` (empty by default); `[dod] export_dir`, `findings_window_hours` |
| D2 | `7903df02` | `harw-dod-client` (layer A): bounded, symlink-refusing readers of the exports, config check, unit state, `DodStatus`; arch policy forbids it from linking the Warden, the escalation ladder or a daemon (invariant S1) |
| D3 | `af2b53a0` | `harw dod …` CLI, `/dod` operation, read-only auto-approved model tools `dod.status`, `dod.findings`, `dod.sensors`; triage roles admit exactly these |
| D4 | `703d9745` | Bundled `harw-dod` skill with `triggers.toml`; role-bound read-only `dod.*` for triage child sessions |
| D5 | `2ce5b2fd` | TUI `/dod view`, status-line mark `⛨ n high`, Finished/Failed suffix |
| E3b / E3c | `1b63871c`, `a214d590` | Probe follows the active profile (refuses a missing, untrusted or invalid config) and picks up new `harw-job-<attempt>` cgroups by a 2 s rescan |
| E3 / E5 | `f31d639f` | Shared delegated cgroup root, `harw dod config suggest [--drop-in]`, `ExecutionChainReport` in `harw dod doctor` |
| Review fixes | `4b080e50`, `3eaccea2`, `64d3831c` | Only the system export is verified; an unreadable system export is reported, never replaced by the home fallback |

Not landed: E4 (jobs ↔ findings correlation; only the client half
`harw-dod-client/src/correlate.rs`), `harw dod enable --grant-read`, the
Warden v2 proof path (PL-96 W01), and the "no silent services" question for
`make -C dod install`/`stage` (PL-97 §8.4). Operator flow:
[`docs/setup/dod.md`](../../setup/dod.md#operator-flow-pl-97); CLI:
[`docs/cli.md`](../../cli.md#harw-dod-harw-dod).

## Before PL-60

- Root workspace excluded `dod/`.
- `dod/` had an independent workspace and lockfile.
- Security boundaries were partly enforced by the build-domain split plus `xtask` gates.

## Planned

- Make DoD crates normal members of the root Harw umbrella workspace.
- Retire the nested DoD workspace.
- Preserve Warden/Sentinel privilege and dependency boundaries with explicit machine-enforced gates.
- Treat workspace membership as build governance, not dependency permission.

## Primary reference

`../10-ecosystem-workspace/HARW_ECOSYSTEM_WORKSPACE_ARCHITECTURE.md`

## Current repository references

- `../../../Cargo.toml`
- `../../../dod/Makefile` (packaging; `-p` builds against the root manifest)
- `../../../docs/design/harw-dod-charter.md`
- `../../../docs/design/harw-dod-crate-decomposition.md`
- `../../../docs/design/harw-dod-integration-and-dependencies.md`
- `../../../xtask/src/gates.rs`
