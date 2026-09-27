# DoD Umbrella Workspace Integration — Planning Compartment

> Status: PL-60 (workspace merge, Phase 4) implemented; Phase 5 TCB budgets partly enforced (`xtask gates`), see below

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
