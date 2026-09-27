# DoD Umbrella Workspace Integration — Planning Compartment

> Status: DRAFT / not implemented

This compartment exists so the DoD workspace merge is not hidden inside the general ecosystem plan.

## Current

- Root workspace excludes `dod/`.
- `dod/` has an independent workspace and lockfile.
- Security boundaries are partly enforced by the build-domain split plus `xtask` gates.

## Planned

- Make DoD crates normal members of the root Harw umbrella workspace.
- Retire the nested DoD workspace.
- Preserve Warden/Sentinel privilege and dependency boundaries with explicit machine-enforced gates.
- Treat workspace membership as build governance, not dependency permission.

## Primary reference

`../10-ecosystem-workspace/HARW_ECOSYSTEM_WORKSPACE_ARCHITECTURE.md`

## Current repository references

- `../../../Cargo.toml`
- `../../../dod/Cargo.toml`
- `../../../docs/design/harw-dod-charter.md`
- `../../../docs/design/harw-dod-crate-decomposition.md`
- `../../../docs/design/harw-dod-integration-and-dependencies.md`
- `../../../xtask/src/gates.rs`
