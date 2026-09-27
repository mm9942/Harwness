# Harw Status Quo & Planning Structure
## Living Architecture Baseline with Referenced Planning Compartments

> **Status:** planning index / architecture control document  
> **Baseline commit:** `d3e0b965696e7a631687a73618c934cda7ad3d2c`  
> **Baseline date:** 2026-09-25  
> **Planning snapshot date:** 2026-09-26  
> **Rule:** Code at the baseline commit is the current state. Planning documents describe intended deltas only. A plan is never treated as implemented until code, tests, Cargo metadata, and the relevant repository documentation have been updated.

---

# 1. Purpose

The repository now contains enough implemented architecture and enough active planning that current reality and intended architecture must be tracked separately.

This document establishes three layers of truth:

```text
1. STATUS QUO
   What exists in the repository now.

2. PLANNING COMPARTMENTS
   Independently reviewable intended changes, each with explicit references.

3. MIGRATION LEDGER
   The exact bridge from current state to planned state.
```

The purpose is to prevent two equally dangerous mistakes:

```text
Mistake A:
    treat a plan as if it were already implemented

Mistake B:
    redesign from memory and lose the working status quo
```

Every future architecture update should therefore state:

```text
CURRENT
PLANNED
DELTA
REFERENCES
IMPLEMENTATION STATUS
```

---

# 2. Source-of-truth hierarchy

The repository's own documentation already states:

> The code is the source of truth; if documentation and code disagree, the code wins.

This planning structure keeps that rule.

Order of authority:

```text
1. Current source code + Cargo metadata
2. Current tests / CI gates
3. Implemented repository documentation
4. Accepted ADRs
5. Planning documents in docs/planning/
6. Discussion / chat / uncommitted ideas
```

A planning document may supersede another planning document, but it does not supersede code until implemented.

---

# 3. Baseline repository snapshot

The current repository baseline is pinned to:

```text
mm9942/Harwness
main
d3e0b965696e7a631687a73618c934cda7ad3d2c
"Align docs/setup/install.md with source install order"
```

All "Current" statements in this planning index refer to that baseline unless explicitly refreshed.

When the repository changes, update:

```text
baseline commit
baseline date
affected current-state sections
migration ledger
```

Do not silently rewrite historical current-state claims.

---

# 4. Current top-level Cargo structure

## 4.1 Product/root workspace — CURRENT

The root `Cargo.toml` is a large Cargo workspace with roughly the current Harw/Harwness product ecosystem.

It already owns shared policy:

```text
edition = 2024
rust-version = 1.85
publish = false
MIT OR Apache-2.0
shared dependency versions
unsafe_code = "forbid"
release profiles
```

The root workspace includes, among many others:

```text
compiler:
    harw-agent-artifact
    harw-agent-compiler
    harw-agent-dsl
    harw-agent-runner

foundational/shared:
    harw-types
    harw-digest
    harw-protocol
    harw-macros
    harw-authority
    harw-extension-api
    harw-observe
    harw-sandbox

runtime/application:
    harw-core
    harw-runtime
    harw-cli
    harw-tui
    harw

jobs:
    harw-job-runtime
    harw-tool-job
    job persistence inside harw-session-store
    execution/wiring inside harw-core / harw-runtime / harw-cli

tools:
    harw-tool-*

memory / knowledge / retrieval:
    harw-memory
    harw-knowledge
    harw-lens-*

services / ingress / web:
    harw-web
    harw-channel-*
    harw-mcp-*
```

This is already more than a single application crate graph.

### Current reference

- `Cargo.toml`
- `docs/design/build-history.md`
- `docs/design/runtime-contracts.md`

---

## 4.2 DoD workspace — CURRENT

DoD is currently excluded from the root workspace:

```toml
exclude = ["dod"]
```

`dod/Cargo.toml` defines a separate Cargo workspace with its own:

```text
members
workspace.package
workspace.dependencies
unsafe_code = "forbid"
release profile
Cargo.lock
```

It contains the `harw-dod-*` sensor/rule/security crates plus facade/process crates such as:

```text
harw-dod
harw-sentinel
harw-probe-fs
harw-probe-bpf
harw-warden
```

This separation currently acts as a build-domain boundary.

### Current references

- `Cargo.toml`
- `dod/Cargo.toml`
- `docs/design/harw-dod-charter.md`
- `docs/design/harw-dod-crate-decomposition.md`
- `docs/design/harw-dod-integration-and-dependencies.md`
- `docs/design/build-history.md`

---

# 5. Current architectural layer model

`docs/design/build-history.md` currently describes these implemented groups:

```text
Vocabulary / shared types
Core runtime
Providers & models
Memory & knowledge
Retrieval / Lens
Telemetry
Tools
Security / DoD
Sandbox / egress / secrets
Front ends
Agents & extensions
```

This model remains the **current implemented description**.

The new ecosystem planning does not retroactively make it false.

Instead, the ecosystem plan is the proposed next ownership model for the same repository.

---

# 6. Current runtime composition — CURRENT

`harw-runtime` is currently the high-level composition layer.

The repository contract says:

```text
harw-runtime sits at dependency layer L12
it may depend on the rest of the workspace
the rest of the workspace may not depend on it
```

It produces a single `RuntimeAssembly` for:

```text
TUI
one-shot CLI
web
MCP server
durable jobs
gateways
```

This is important because the future ecosystem split must preserve the existing successful rule:

> broad fan-in belongs at the composition edge, not in foundations.

### Current reference

- `docs/design/runtime-contracts.md`
- `harw-runtime/Cargo.toml`

---

# 7. Current compiler domain — CURRENT

The repository already contains a real compiler-oriented stack:

```text
harw-agent-dsl
harw-agent-artifact
harw-agent-compiler
harw-agent-runner
```

The accepted compiler ADR is:

```text
docs/adr/0001-agent-compiler.md
```

The compiler work is partially implemented and continues to evolve independently from the generic job-runtime work.

### Current references

- `docs/adr/0001-agent-compiler.md`
- `docs/design/agent-definition-dsl.md`
- `docs/design/agent-artifact-v1.md`
- `docs/design/agent-ir-v1.md`
- `harw-agent-*`

---

# 8. Current job systems — CURRENT

There are currently **two distinct job concepts** that must not be falsely described as already unified.

## 8.1 `harw-job-runtime`

Current scope:

```text
governed background-work primitives
Job
Budget
Lease
RetryPolicy
StoredJob
```

It explicitly does **not** own scheduling or execution.

Current scheduling/execution is distributed into callers:

```text
harw-session-store::JobStore
harw-core::DurableJobRunner
harw-cli job worker
```

Current crate statement:

```text
pure governance arithmetic
no threads
no locks
scheduling/execution live in callers
```

### Current references

- `harw-job-runtime/src/lib.rs`
- `harw-job-runtime/src/*`
- `harw-job-runtime/Cargo.toml`

---

## 8.2 `harw-tool-job`

Current scope:

```text
long-running local process jobs
job.start
job.status
job.logs
job.stop
job.list
job.wait
```

It owns concrete process supervision behavior:

```text
stdout/stderr logs
meta.json persistence
process-group signaling
restart reload
detached/unknown recovery
progress detection
tool-facing ownership rules
```

Current Linux process identity uses:

```text
PID
+
/proc/<pid>/stat field 22 start ticks
```

Current signaling uses `rustix` process APIs and process groups.

### Current references

- `harw-tool-job/src/lib.rs`
- `harw-tool-job/src/manager.rs`
- `harw-tool-job/src/procfs.rs`
- `harw-tool-job/src/model.rs`
- `harw-tool-job/Cargo.toml`

---

# 9. Current sandbox / installer state — CURRENT

The repository already thinks about sandboxed execution as a first-class concern.

Current installation documentation is Linux-focused.

The source installer currently:

```text
downloads/unpacks source
installs Rustup if needed
installs Bubblewrap if missing
supports multiple Linux package managers
runs make install
```

Bubblewrap is therefore already part of the operational execution/sandbox story.

Current install docs state:

```text
Platform: Linux
```

This remains the current documented install support even though planning now introduces a first-class Darwin execution target.

### Current references

- `docs/setup/install.md`
- `docs/setup/build-prerequisites.md`
- `docs/design/mediated-process-execution.md`
- `harw-sandbox`
- install scripts / Makefile

---

# 10. Current unsafe policy — CURRENT

The root workspace and DoD workspace both currently enforce:

```toml
[workspace.lints.rust]
unsafe_code = "forbid"
```

This is an implemented invariant and must survive all planning migrations.

### Current references

- root `Cargo.toml`
- `dod/Cargo.toml`
- `docs/design/build-history.md`

---

# 11. Current DoD security boundary — CURRENT

DoD is currently intentionally separated and has a small audited privileged enforcement path.

Existing documented invariants include:

```text
Warden has a strict dependency budget
privilege rules are machine-gated
forbidden dependency edges are checked
no C build in Warden dependency hull
authorization remains narrow
```

These are current security properties.

A future umbrella-workspace merge must preserve them with new mechanisms rather than treating workspace unification as permission to broaden dependencies.

### Current references

- `docs/design/build-history.md`
- `docs/design/harw-dod-charter.md`
- `docs/design/harw-dod-crate-decomposition.md`
- `docs/design/harw-dod-integration-and-dependencies.md`
- `xtask/src/gates.rs`
- CI configuration

---

# 12. Planning compartment model

Each active planning area gets its own compartment.

A planning compartment contains:

```text
A. Current baseline
B. Target state
C. Planned delta
D. Dependencies / ordering
E. Primary planning reference
F. Current repository references
G. Status
```

The compartments may evolve independently.

They are connected through the migration ledger.

---

# 13. PL-10 — Harw ecosystem / umbrella workspace

## Current

```text
root product workspace
+
separate excluded DoD workspace
+
Harwness-oriented historical ownership language
```

## Target

```text
Harw = umbrella ecosystem / root ownership model

Harwness
    = application + composition root

Compiler
    = first-class domain

Jobs
    = shared infrastructure

DoD
    = peer security domain

Foundations
    = outside application ownership
```

DoD eventually becomes root Cargo workspace membership while retaining hard dependency/TCB gates.

`harw-macros` and other true foundations are conceptually ecosystem-owned rather than Harwness-owned.

## Primary planning reference

```text
docs/planning/10-ecosystem-workspace/HARW_ECOSYSTEM_WORKSPACE_ARCHITECTURE.md
```

## Current repository references

```text
Cargo.toml
dod/Cargo.toml
docs/design/build-history.md
docs/design/runtime-contracts.md
docs/design/harw-dod-*.md
```

## Status

```text
PLANNED
No workspace-membership change has occurred yet.
```

---

# 14. PL-20 — Generic job execution infrastructure

## Current

```text
harw-job-runtime
    generic-ish governance data

harw-session-store
    durable JobStore

harw-core / harw-cli
    durable execution

harw-tool-job
    concrete local-process supervision

harw-runtime
    job wiring / composition
```

These implementations overlap in concerns but are not one generic runtime today.

## Target

A shared job family:

```text
harw-job-core
harw-job-store
harw-job-linux
harw-job-darwin
harw-job-tokio
harw-job-runtime
harw-job
```

with later optional adapters:

```text
harw-job-service
harw-job-hyper
harw-job-executor-bwrap
harw-job-executor-container
```

## Primary planning reference

```text
docs/planning/20-job-runtime/HARW_LINUX_JOB_RUNTIME_FOUNDATION.md
```

## Secondary planning reference

```text
docs/planning/10-ecosystem-workspace/HARW_ECOSYSTEM_WORKSPACE_ARCHITECTURE.md
```

## Current repository references

```text
harw-job-runtime/*
harw-tool-job/*
harw-session-store/*
harw-core/*
harw-runtime/*
harw-cli/src/job_worker.rs
docs/design/runtime-contracts.md
```

## Status

```text
PLANNED
Current job systems remain authoritative until extraction lands.
```

---

# 15. PL-30 — Linux-native job execution foundation

## Current

The local process job path currently uses:

```text
rustix
PID + /proc start ticks
process groups
log files
detached recovery
```

## Target

Linux execution should progressively use:

```text
pidfd
waitid(P_PIDFD)
cgroup v2
procfs crate
cap-std
Landlock
NO_NEW_PRIVS
Linux capabilities
rlimits
Tokio AsyncFd
```

The default path should remain first-party safe Rust with:

```text
#![forbid(unsafe_code)]
```

and no mandatory compiled C/C++ dependency graph.

## Primary planning reference

```text
docs/planning/20-job-runtime/HARW_LINUX_JOB_RUNTIME_FOUNDATION.md
```

## Status

```text
PLANNED
```

---

# 16. PL-40 — Bubblewrap / stronger Linux sandbox executor

## Current

Bubblewrap is already installed/expected by the Linux source-install flow.

It is part of the practical sandbox environment but is not yet modeled as a generic job-executor backend in the new architecture.

## Target

Treat Bubblewrap as an optional stronger Linux executor/backend:

```text
generic SandboxPolicy
        │
        ├── native safe-Rust Linux enforcement
        │
        └── Bubblewrap executor
              namespaces
              mount isolation
              additional filesystem isolation
```

Bubblewrap should remain an external executable integration, not a reason to introduce C into the Rust dependency graph.

## Primary planning references

```text
docs/planning/20-job-runtime/HARW_LINUX_JOB_RUNTIME_FOUNDATION.md
docs/setup/install.md
docs/design/mediated-process-execution.md
```

## Status

```text
PLANNED / existing operational dependency, not yet extracted backend
```

---

# 17. PL-50 — Darwin/macOS execution backend

## Current

The new generic job-runtime extraction has not yet created a Darwin backend.

Current installation documentation is Linux-focused.

## Target

macOS/Darwin remains a first-class execution platform in the new job architecture.

Design rule:

```text
shared job semantics
    portable

Linux backend
    Linux-native semantics

Darwin backend
    Darwin-native semantics
```

Do not force Linux and Darwin into fake identical primitives.

Do not weaken Linux to a Unix lowest common denominator.

Windows execution parity is not required.

## Planning references

```text
docs/planning/10-ecosystem-workspace/HARW_ECOSYSTEM_WORKSPACE_ARCHITECTURE.md
docs/planning/20-job-runtime/HARW_LINUX_JOB_RUNTIME_FOUNDATION.md
```

The job-foundation document is Linux-heavy and must be updated during implementation to include the Darwin sibling backend explicitly.

## Status

```text
PLANNED
```

---

# 18. PL-60 — DoD workspace integration into umbrella workspace

## Current

```text
root Cargo workspace
exclude = ["dod"]

dod/
    own Cargo.toml
    own Cargo.lock
    own workspace dependencies
```

## Target

DoD becomes a peer domain inside the root umbrella Cargo workspace.

The nested DoD workspace is retired.

Security isolation moves from workspace separation to explicit:

```text
dependency allowlists
TCB budgets
architecture gates
privilege boundaries
feature-graph checks
```

The Warden remains a hardened island and must not gain generic job runtime dependencies merely because everything shares one workspace.

## Primary planning reference

```text
docs/planning/10-ecosystem-workspace/HARW_ECOSYSTEM_WORKSPACE_ARCHITECTURE.md
```

## Current references

```text
Cargo.toml
dod/Cargo.toml
docs/design/harw-dod-charter.md
docs/design/harw-dod-crate-decomposition.md
docs/design/harw-dod-integration-and-dependencies.md
docs/design/dod-system-operations-authority.md
xtask gates
```

## Status

```text
PLANNED
```

---

# 19. PL-70 — Crypto / infrastructure architecture

## Current

The repository already contains:

```text
harw-secrets
crypt_guard integration
authority and sandbox infrastructure
DoD privileged security architecture
```

The detailed future crypto/infrastructure expansion is maintained separately because it is too large to collapse into the workspace plan.

## Primary planning reference

```text
docs/planning/30-crypto-infrastructure/Harwness_Crypto_Infrastructure_Masterplan_v2.md
```

## Supporting planning reference

```text
docs/planning/40-cryptguard-service/crypt_guard_service_hyper_tower_plan.md
```

## Current repository references

```text
harw-secrets/*
docs/setup/crypt-guard.md
harw-authority/*
harw-operations/*
harw-runtime/*
```

## Status

```text
PLANNED / partially intersects existing infrastructure
```

---

# 20. PL-80 — CryptGuard service / Hyper / Tower / KMS composition

## Current

Cryptographic integration exists, but the broader service/composition architecture described in the dedicated plan is not current code by default.

## Target

Keep generic crypto mechanisms and service composition separate from Harwness policy.

Preserve narrow privilege/dependency boundaries.

Do not pull service stacks into privileged DoD/Warden code without explicit review.

## Primary planning reference

```text
docs/planning/40-cryptguard-service/crypt_guard_service_hyper_tower_plan.md
```

## Related planning reference

```text
docs/planning/30-crypto-infrastructure/Harwness_Crypto_Infrastructure_Masterplan_v2.md
```

## Status

```text
PLANNED
```

---

# 21. PL-90 — Compiler ↔ execution target contract

## Current

The agent compiler stack and execution stack coexist, but platform/execution requirements are not yet the unified target contract proposed in planning.

## Target

Keep two questions separate:

```text
Can Harw compile on this host?

Can this artifact execute on this target with its required guarantees?
```

Future compiled-agent requirements may describe:

```text
target OS
target architecture
required tools
network policy
filesystem policy
resource limits
sandbox guarantees
kernel capabilities
DoD/eBPF requirements
```

A Windows machine may compile/submit an artifact for a Linux runner without requiring equivalent local Windows sandbox semantics.

## Primary references

```text
docs/adr/0001-agent-compiler.md
docs/planning/10-ecosystem-workspace/HARW_ECOSYSTEM_WORKSPACE_ARCHITECTURE.md
docs/planning/20-job-runtime/HARW_LINUX_JOB_RUNTIME_FOUNDATION.md
```

## Status

```text
PLANNED extension of an already partially implemented compiler domain
```

---

# 22. Planning directory target

The planning material should be physically arranged as:

```text
docs/
└── planning/
    ├── README.md
    │
    ├── 00-status-quo/
    │   └── BASELINE.md
    │
    ├── 10-ecosystem-workspace/
    │   └── HARW_ECOSYSTEM_WORKSPACE_ARCHITECTURE.md
    │
    ├── 20-job-runtime/
    │   └── HARW_LINUX_JOB_RUNTIME_FOUNDATION.md
    │
    ├── 30-crypto-infrastructure/
    │   └── Harwness_Crypto_Infrastructure_Masterplan_v2.md
    │
    ├── 40-cryptguard-service/
    │   └── crypt_guard_service_hyper_tower_plan.md
    │
    ├── 50-dod-integration/
    │   └── README.md
    │
    ├── 60-platform-execution/
    │   └── README.md
    │
    ├── 65-cloud-sessions/
    │   └── README.md
    │
    ├── 70-decisions/
    │   └── README.md            (DEC-001 …)
    │
    ├── 80-copilot-backlog/
    │   └── README.md
    │
    ├── 85-gap-hunt/
    │   ├── README.md            (reusable gap-hunt kit)
    │   ├── patterns.md          (pattern catalog M1–M8, P1–P8)
    │   ├── R15-patterns.md      (run log)
    │   └── kit/                 (workflows, skill, agent types)
    │
    └── 90-migration-ledger/
        └── MIGRATION_LEDGER.md
```

This structure intentionally separates:

```text
implemented design docs
    docs/design/

architecture decisions
    docs/adr/

future planning
    docs/planning/
```

Do not mark planning documents as implemented in `docs/README.md`.

---

# 23. Status labels for planning documents

Every planning document should use one of:

```text
DRAFT
APPROVED PLAN
IN IMPLEMENTATION
PARTIALLY LANDED
SUPERSEDED
COMPLETED / MOVED TO DESIGN
```

Never use only "implemented" inside `docs/planning/`.

When a plan is fully implemented:

1. update code
2. update tests / gates
3. update implemented docs
4. create/update ADR if architectural
5. mark plan completed
6. move normative surviving content into `docs/design/` or `docs/architecture/`

Planning files are not permanent substitutes for implemented documentation.

---

# 24. Migration ledger format

Every real implementation wave should create a ledger entry:

```text
MIG-XXX
Planning compartment:
Baseline commit:
Implementation commit/PR:
Affected crates:
Current state before:
Target state:
Delta landed:
Compatibility retained:
Tests added:
Gates changed:
Docs updated:
Remaining plan items:
```

This gives us an exact bridge between discussion and code.

---

# 25. Initial migration ledger

## MIG-000 — Planning structure only

```text
Planning compartment:
    all

Baseline:
    d3e0b965696e7a631687a73618c934cda7ad3d2c

Code changes:
    none

Purpose:
    establish planning structure without pretending architecture changes have landed

Current state after:
    exactly the same repository behavior as baseline

Plans registered:
    ecosystem workspace architecture
    generic job runtime foundation
    crypto/infrastructure masterplan
    CryptGuard service/Hyper/Tower plan

Implementation status:
    planning only
```

This is deliberately the first entry.

---

# 26. Ordering between planning compartments

Not every plan can land independently.

Recommended dependency order:

```text
PL-10 ecosystem ownership model
        │
        ├────────────┐
        ▼            ▼
PL-60 DoD merge   PL-20 job extraction
        │            │
        │            ├── PL-30 Linux execution
        │            ├── PL-40 Bubblewrap
        │            └── PL-50 Darwin
        │
        └────────────┬───────────────┐
                     ▼               ▼
               PL-70 crypto      PL-90 compiler/execution
                     │
                     ▼
               PL-80 CryptGuard service
```

This does **not** mean all of PL-10 must be implemented before any job work.

It means ownership decisions should be settled before final crate placement is frozen.

---

# 27. What is explicitly not changed yet

At this planning snapshot:

```text
DoD is still a separate Cargo workspace.
Root still excludes dod.
harw-job-runtime is still the existing governance crate.
harw-tool-job still owns local process jobs.
PID + proc start ticks still exist in current process recovery.
Process-group kill is still current behavior.
No generic harw-job-linux crate exists yet.
No harw-job-darwin crate exists yet.
Bubblewrap is not yet a generic job backend.
Current installer remains Linux-focused.
harw-runtime remains current L12 composition root.
The current Warden dependency boundary remains as implemented.
```

This section must be kept accurate.

---

# 28. What planning now changes conceptually

Although code is unchanged, future architecture work should now assume:

```text
Harw is the umbrella ecosystem.

Harwness is an application/composition domain.

DoD is a peer security domain.

Generic job execution belongs outside both.

Compiler is a first-class domain.

Linux is the primary high-assurance execution target.

macOS/Darwin is first-class.

Windows must not constrain the server/security execution architecture.

Safe Rust and explicit TCB boundaries remain mandatory.
```

These are planning decisions, not yet implementation claims.

---

# 29. Update procedure after each coding session

After a meaningful implementation session:

1. record new HEAD commit
2. compare against the baseline section affected
3. mark each landed item in the relevant planning compartment
4. add a `MIG-*` ledger entry
5. update current-state text only for code that actually landed
6. keep unimplemented target text in the planning compartment
7. refresh references to files/crates if paths changed
8. update `docs/README.md` only when a plan becomes implemented/partially implemented repository documentation

This prevents the planning index from becoming another stale mega-document.

---

# 30. Reference registry

## Current repository references

```text
Cargo.toml
dod/Cargo.toml
docs/README.md
docs/design/build-history.md
docs/design/runtime-contracts.md
docs/adr/0001-agent-compiler.md
docs/design/harw-dod-charter.md
docs/design/harw-dod-crate-decomposition.md
docs/design/harw-dod-integration-and-dependencies.md
docs/design/dod-system-operations-authority.md
docs/design/mediated-process-execution.md
docs/setup/install.md
docs/setup/build-prerequisites.md
harw-job-runtime/src/lib.rs
harw-tool-job/src/lib.rs
harw-tool-job/src/procfs.rs
harw-runtime/Cargo.toml
harw-cli/Cargo.toml
```

## Registered planning references

```text
docs/planning/10-ecosystem-workspace/HARW_ECOSYSTEM_WORKSPACE_ARCHITECTURE.md

docs/planning/20-job-runtime/HARW_LINUX_JOB_RUNTIME_FOUNDATION.md

docs/planning/30-crypto-infrastructure/Harwness_Crypto_Infrastructure_Masterplan_v2.md

docs/planning/40-cryptguard-service/crypt_guard_service_hyper_tower_plan.md
```

---

# 31. Final rule

The planning system should always make this sentence answerable:

> What exists now, what do we intend to change, and which document is the authority for that intended change?

If those three answers are not explicit, the architecture record is incomplete.

The working model is therefore:

```text
STATUS QUO
    never inferred from plans

PLANS
    never presented as current code

MIGRATION LEDGER
    proves when one became the other
```

That is the structure going forward.
