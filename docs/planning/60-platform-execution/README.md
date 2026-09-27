# Platform Execution Strategy — Planning Compartment

> Status: DRAFT / not implemented

## Current

- Linux is the documented installation target.
- Bubblewrap is part of the source-install dependency flow.
- `harw-tool-job` uses Linux `/proc` and rustix for process-group control.
- Some higher-level compiler/client crates are naturally portable.

## Planned execution policy

```text
Compiler/client:
    Linux     first-class
    macOS     first-class
    Windows   supported where practical

High-assurance execution:
    Linux     primary / strongest
    macOS     first-class Darwin backend
    Windows   no parity requirement

DoD/server security:
    Linux     primary
    macOS     only where semantics meaningfully map
    Windows   no architecture promise
```

## Key rule

Portability applies where it is natural:
DSL, IR, artifacts, state machines, leases, protocols.

Native semantics apply where they improve correctness:
pidfd, cgroup v2, Landlock, kqueue, Darwin process primitives, platform sandboxing.

## References

- `../10-ecosystem-workspace/HARW_ECOSYSTEM_WORKSPACE_ARCHITECTURE.md`
- `../20-job-runtime/HARW_LINUX_JOB_RUNTIME_FOUNDATION.md`
- `../../../docs/setup/install.md`
