# Architecture documents

The code and Cargo metadata are the source of truth. Planning lives in `docs/planning/`.
The machine-enforced layering policy is `xtask/arch-policy.toml` (arch gate in `xtask`).

## Subsystems (implemented)

| Document | Topic |
|---|---|
| [model-provider-routing.md](model-provider-routing.md) | Model and provider routing |
| [operation-registry.md](operation-registry.md) | Operation registry |
| [session-controller.md](session-controller.md) | Session controller |

## Ecosystem workspace migration (R11, Eco-Doc §67 deliverables)

| Document | Topic |
|---|---|
| [harw-workspace-inventory.md](harw-workspace-inventory.md) | Per-crate inventory: ring (F/I/C/J/D/A/T), deps, privilege, platform, unsafe, native deps |
| [harw-dependency-inversions.md](harw-dependency-inversions.md) | Edges against the ring direction law, with fix and wave |
| [dod-workspace-merge-plan.md](dod-workspace-merge-plan.md) | Merging the nested `dod/` workspace into the root workspace |
| [job-extraction-map.md](job-extraction-map.md) | Current job code → `harw-job-*` target crates, migration order |
| [crypto-drift-report.md](crypto-drift-report.md) | Masterplan H0: CryptGuard version drift, systemd drift, `/run/harw` decisions |
