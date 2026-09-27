# Harw Ecosystem Workspace Architecture
## Umbrella Workspace, Ownership Rings, Shared Foundations, Harwness, Jobs and DoD

**Status:** Architecture directive  
**Purpose:** Define the repository/workspace ownership model after Harw has grown beyond a single Harwness application  
**Primary implementation context:** `mm9942/Harwness`  
**Audience:** Claude Code / coding agents / maintainers / architecture reviews

---

# 1. Core decision

The repository must no longer be modeled as:

```text
Harwness application
├── helper crates
├── jobs
├── DoD
├── macros
├── compiler
└── everything else
```

That model is historically understandable, but architecturally wrong for the system that now exists.

The correct thought model is:

```text
Harw ecosystem / umbrella workspace
│
├── foundations
├── compiler stack
├── shared runtime infrastructure
├── job execution infrastructure
├── security infrastructure
├── DoD
└── Harwness application/composition
```

Harwness is a major consumer and composition root inside the Harw ecosystem.

Harwness is **not** the conceptual owner of every `harw-*` crate.

This distinction must guide future crate placement, dependency direction, naming, and workspace membership.

---

# 2. "Workspace" in this document

This document uses two related but different meanings.

## 2.1 Architectural workspace

The architectural workspace is the ownership model:

```text
HARW
├── generic foundations
├── reusable infrastructure
├── compiler
├── execution runtime
├── security domains
└── applications
```

This is the primary meaning of "übergeordneter Workspace" in the design discussion.

## 2.2 Cargo workspace

Cargo's `[workspace]` is the concrete build graph:

```toml
[workspace]
members = [...]
```

The target direction is that the repository root becomes the real **umbrella Cargo workspace** for the Harw ecosystem as well.

DoD should migrate from its current nested independent Cargo workspace into the root workspace membership.

However:

> Joining the root Cargo workspace does not remove DoD's security boundaries.

The Warden's small trusted computing base, dependency budget, privilege boundary, and build checks must remain enforced explicitly.

---

# 3. Current repository reality

The current root Cargo workspace already contains a large ecosystem rather than a single small application.

Examples include:

```text
harw-agent-artifact
harw-agent-compiler
harw-agent-dsl
harw-agent-runner
harw-authority
harw-digest
harw-types
harw-protocol
harw-macros
harw-extension-api
harw-sandbox
harw-job-runtime
harw-session-store
harw-observe
harw-runtime
harw-cli
harw
...
```

The root workspace already centralizes:

```text
edition = 2024
rust-version = 1.85
shared dependency versions
unsafe_code = "forbid"
release profiles
```

The root therefore already behaves like an ecosystem workspace in practice.

The architecture should acknowledge that explicitly.

---

# 4. Current DoD exception

At present:

```toml
exclude = ["dod"]
```

and `dod/Cargo.toml` defines its own nested workspace, lockfile, package metadata, dependency versions, lint policy, and release profile.

That arrangement was useful while DoD needed a deliberately separate build domain.

The target architecture changes that:

```text
DoD
    becomes part of the root Harw Cargo workspace
```

but **not**:

```text
DoD
    becomes an ordinary unrestricted dependency blob
```

The security boundary moves from:

```text
separate Cargo workspace
```

to:

```text
explicit dependency and privilege policy inside the umbrella workspace
```

This is an important migration, not a deletion of security constraints.

---

# 5. Target ownership rings

The system should be thought about in rings.

```text
┌─────────────────────────────────────────────────────┐
│ APPLICATION / COMPOSITION                           │
│                                                     │
│ Harwness CLI / TUI / server / agent runtime         │
│ DoD processes and security applications             │
│ future daemons                                      │
│                                                     │
│   ┌─────────────────────────────────────────────┐   │
│   │ SHARED INFRASTRUCTURE                       │   │
│   │                                             │   │
│   │ jobs / sandbox / storage / operations       │   │
│   │ secrets / transports / reusable services    │   │
│   │                                             │   │
│   │   ┌─────────────────────────────────────┐   │   │
│   │   │ FOUNDATIONS                         │   │   │
│   │   │                                     │   │   │
│   │   │ macros / digest / core value types  │   │   │
│   │   │ protocol primitives / IR contracts  │   │   │
│   │   │ observation vocabulary              │   │   │
│   │   └─────────────────────────────────────┘   │   │
│   └─────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────┘
```

Dependencies should predominantly point inward.

---

# 6. Dependency direction law

The most important rule is:

```text
applications
    ↓
shared infrastructure
    ↓
foundations
```

Never normalize the opposite direction merely because a type is convenient.

Bad:

```text
harw-macros
    ↓
harw-runtime
    ↓
harw-cli
```

Good:

```text
harw-macros
    ↑
harw-job-core
    ↑
harw-runtime
    ↑
harw-cli
```

Another acceptable graph:

```text
harw-digest
    ↑
harw-types-core
    ↑
harw-agent-artifact
    ↑
harw-agent-compiler
```

The exact crate names may change.

The dependency direction must not.

---

# 7. Ring A — foundations

Foundation crates define reusable vocabulary and compile-time mechanisms.

They should know as little as possible about runtime composition, CLI, TUI, agents, DoD processes, provider implementations, or operating-system orchestration.

Strong foundation candidates include:

```text
harw-digest
harw-macros
selected parts of harw-types
protocol value types
compiler IR contracts
small observation/event vocabulary
```

Potentially:

```text
harw-authority
harw-extension-api
```

but only after their dependency graphs and semantics are inspected.

Do not classify crates as foundations based on their names.

Classify them based on what they know.

---

# 8. harw-macros belongs outside Harwness

`harw-macros` is a clear example of an ecosystem-level crate.

It is a procedural macro crate.

It should not be conceptually owned by:

```text
Harwness CLI
Harwness runtime
DoD
job runtime
```

Instead:

```text
Harw ecosystem
    └── harw-macros
```

and consumers depend on it.

Current production dependencies are appropriately small:

```text
syn
quote
proc-macro2
```

That is exactly the kind of shape expected from an outer foundation.

---

# 9. Domain-specific macro tests are not ownership

`harw-macros` currently has dev-dependencies on higher-level crates including DoD types and operations/tool types for derive/trybuild testing.

Dev-dependencies do not create the same production dependency direction.

However, long-term architectural hygiene should consider moving domain-specific compile tests outward.

Preferred options:

```text
consumer crate compile tests
```

or:

```text
dedicated macro integration-test crate
```

rather than making the macro foundation's own test suite the place where every higher-level domain is known.

This is not an urgent blocker.

It is a cleanup target.

---

# 10. harw-types is not automatically one foundation

`harw-types` contains many useful shared types, but it is already large enough that the name alone cannot define the architecture.

A notable example is `CancelToken`, which wraps a Tokio cancellation primitive.

That means:

```text
harw-types
```

currently mixes:

```text
pure value types
+
runtime-oriented behavior
```

Do not move the whole crate into a conceptual "foundation" box without inspection.

Potential long-term split:

```text
harw-value-types
harw-runtime-types
```

or another naming scheme that preserves compatibility.

No split should be performed merely for aesthetic purity.

Split only where dependency direction and compile graph improve materially.

---

# 11. Ring B — compiler domain

Harw is now effectively an agent compiler.

The compiler domain deserves first-class ownership separate from the Harwness application composition layer.

Current relevant crates include:

```text
harw-agent-dsl
harw-agent-artifact
harw-agent-compiler
harw-agent-runner
```

Conceptually:

```text
source / DSL
    ↓
validated representation
    ↓
compiler passes
    ↓
artifact
    ↓
runner / target runtime
```

This compiler stack should be reusable independently from the full Harwness interactive application.

---

# 12. Compiler portability is different from execution portability

This distinction must be preserved.

Harw compiler/client functionality may support:

```text
Linux
macOS
Windows
```

while high-assurance local execution runtimes may target:

```text
Linux — primary / strongest execution target
macOS — first-class Darwin execution target
Windows — no equivalent execution guarantee required
```

Therefore:

```text
Can Harw compile here?
```

and:

```text
Can this compiled agent execute here with all requested isolation guarantees?
```

are separate questions.

Do not force execution backends to a lowest common denominator to preserve compiler portability.

---

# 13. Ring C — shared execution infrastructure

The new generic job system belongs here.

It is not:

```text
Harwness jobs
```

and not:

```text
DoD jobs
```

It is:

```text
Harw shared execution infrastructure
```

Expected consumers include:

```text
Harwness
DoD
future infrastructure daemons
standalone Rust consumers
remote execution services
```

Target family:

```text
harw-job-core
harw-job-store
harw-job-linux
harw-job-darwin
harw-job-tokio
harw-job-runtime
harw-job
```

Possible future adapters:

```text
harw-job-service
harw-job-hyper
harw-job-executor-bwrap
harw-job-executor-container
```

---

# 14. Job infrastructure must not depend on Harwness application semantics

The current `harw-job-runtime` is only partially generic.

It currently depends on:

```text
harw-types
harw-macros
harw-observe
serde
serde_json
jiff
```

and its stored model historically includes concepts such as:

```text
TenantId
WorkspaceId
ApprovalActor
TraceContext
Dream
Worker
```

Those are not all suitable for the lowest generic job layer.

The migration target is:

```text
generic job mechanics
    in job crates

Harwness-specific scope / authority / tracing context
    in Harwness adapters

DoD-specific job meaning
    in DoD adapters
```

---

# 15. Job platform policy

The shared job runtime should distinguish core semantics from OS execution.

```text
harw-job-core
    OS-neutral semantics

harw-job-linux
    Linux-native execution

harw-job-darwin
    macOS/Darwin-native execution
```

Do not design a lowest-common-denominator fake platform abstraction.

Linux may expose Linux semantics directly where doing so improves correctness:

```text
pidfd
cgroup v2
Landlock
capabilities
openat2
procfs
Bubblewrap integration
```

Darwin may expose its own strong primitives without pretending they are Linux concepts.

---

# 16. Windows policy

Windows support must not distort the server/execution architecture.

Allowed:

```text
Harw compiler on Windows
Harw CLI/client on Windows
artifact creation on Windows
remote submission from Windows
result inspection on Windows
```

Not required:

```text
full local high-assurance job sandbox parity
DoD server runtime
Linux security daemon parity
cgroup-like semantic emulation
fake pidfd abstraction
```

If a deployment requires a Windows-native security runtime, that is a separate backend/project problem.

Do not weaken Linux or Darwin architecture to solve it preemptively.

---

# 17. Ring D — shared sandbox/security infrastructure

`harw-sandbox`, secrets infrastructure, authority primitives, and future security services belong in shared infrastructure **if** their APIs are domain-neutral.

The key rule:

```text
security mechanism
    shared

application security policy
    owned by application/domain
```

Example:

```text
Landlock policy application
    shared execution infrastructure

"DoD scan job may read X"
    DoD policy

"Harwness coding agent may write workspace Y"
    Harwness policy
```

Mechanism and policy must not be collapsed.

---

# 18. Ring E — DoD domain

DoD is a first-class ecosystem domain.

It is not a plugin hidden beneath Harwness.

Conceptually:

```text
Harw ecosystem
    ├── Harwness
    ├── Jobs
    ├── compiler
    └── DoD
```

DoD currently contains many specialized crates:

```text
harw-dod-authlog
harw-dod-blockio
harw-dod-bpf
harw-dod-cap
harw-dod-cgroup
harw-dod-config
harw-dod-cpu
harw-dod-escalate
harw-dod-flow
harw-dod-fsmon
harw-dod-gpu
harw-dod-listener
harw-dod-memory
harw-dod-netcounters
harw-dod-netlink
harw-dod-netpolicy
harw-dod-procmon
harw-dod-readfs
harw-dod-rules
harw-dod-scanreport
harw-dod-sentinel
harw-dod-signals
harw-dod-thermal
harw-dod-warden
harw-dod-warden-proto
harw-dod-workspace
...
```

This is clearly a domain, not a helper module.

---

# 19. DoD becomes a root workspace member domain

The target is to remove the nested-workspace split.

That implies:

```text
remove root exclude = ["dod"]
```

and make DoD crates real members of the root workspace.

Possible membership style:

```toml
members = [
    ...
    "dod/crates/harw-dod-authlog",
    "dod/crates/harw-dod-blockio",
    ...
    "dod/crates/harw-dod-warden",
    ...
]
```

Use explicit entries unless a carefully reviewed glob policy is introduced.

Do not silently rely on nested workspace behavior.

---

# 20. Nested DoD workspace migration

Because Cargo workspaces cannot be meaningfully treated as both an independent nested workspace and ordinary members of the root in the target model, migration should include:

1. remove or retire `dod/Cargo.toml` as an independent `[workspace]`
2. move shared package metadata to the root `[workspace.package]`
3. move shared dependency versions to root `[workspace.dependencies]`
4. make DoD crates use `workspace = true` where appropriate
5. use the root `Cargo.lock`
6. preserve DoD-specific CI and dependency-budget checks
7. preserve Warden-specific minimal dependency rules

Do not do this as a blind text replacement.

Inventory all DoD workspace dependencies first.

---

# 21. One lockfile is acceptable

The separate DoD lockfile currently contributes to build-domain isolation.

Once DoD enters the umbrella workspace, the root lockfile becomes the dependency resolution record.

That is acceptable if the security boundary is enforced at the package graph level.

The new rule becomes:

```text
same lockfile
does not mean
same trusted computing base
```

A Warden binary can share a lockfile with the full ecosystem while still linking only a tiny approved graph.

What matters for the Warden is:

```text
what it depends on
what features are enabled
what code is linked
what capabilities it runs with
```

not whether another package exists in the same `Cargo.lock`.

---

# 22. Warden remains a hard security island

This rule survives the workspace merge unchanged:

> The Warden must remain a tiny privileged trusted computing base.

Do not let workspace unification cause:

```text
harw-dod-warden
    → harw-runtime
    → providers
    → HTTP
    → browser
    → job runtime
```

The Warden should keep narrowly audited dependencies.

Generic job execution must **not** be added to Warden merely because both are now workspace members.

Allowed:

```text
DoD orchestrator / sentinel-side process
    → harw-job-*
```

Potentially forbidden:

```text
harw-dod-warden
    → generic job runner
```

unless a future architecture review explicitly proves why it is necessary.

---

# 23. Dependency budgets replace workspace separation

After DoD joins the root workspace, add machine-enforced package budgets.

Examples:

```text
harw-dod-warden:
    approved direct dependency allowlist

harw-dod-warden-proto:
    leaf / near-leaf budget

harw-dod-readfs:
    no network stack

harw-dod-signals:
    no provider/runtime stack
```

Possible CI techniques:

```bash
cargo tree -p harw-dod-warden
cargo tree -p harw-dod-warden --edges normal
cargo metadata
```

plus a repository script that compares actual transitive package names to an approved list.

Do not rely on reviewers remembering the intended budget.

---

# 24. Ring F — Harwness application/composition

`harw-runtime` and `harw-cli` currently demonstrate what belongs close to the application/composition ring.

`harw-runtime` depends on a broad set of domain and infrastructure crates:

```text
agent artifact
DSL
catalog
config
core
DoD signals
extension API
job runtime
knowledge
memory
model catalog
operations
plan
protocol
providers
sandbox
session store
tools
...
```

That is not evidence that all those crates belong to Harwness.

It is evidence that `harw-runtime` is a composition layer.

That is exactly where broad dependency fan-in is expected.

---

# 25. harw-cli is an application edge

`harw-cli` is also a composition/application edge.

It owns:

```text
command-line entry points
compiler commands
chat/server commands
settings
lens commands
job worker integration
platform-specific user-facing commands
```

It may depend broadly.

Foundation crates must not depend back on it.

---

# 26. The `harw` name is ecosystem namespace, not application ownership

Do not rename every shared crate merely because it is no longer conceptually owned by Harwness.

`harw-*` is a useful ecosystem namespace.

Therefore:

```text
harw-job-core
```

can mean:

```text
job core belonging to the Harw ecosystem
```

not:

```text
private Harwness implementation detail
```

Likewise:

```text
harw-macros
harw-digest
harw-agent-compiler
```

remain sensible names.

Avoid an unnecessary repository-wide rename wave.

---

# 27. Proposed conceptual top-level map

This is a thought model first.

It does not require immediate physical directory moves.

```text
HARW ECOSYSTEM
│
├── Foundations
│   ├── harw-digest
│   ├── harw-macros
│   ├── selected core value types
│   ├── protocol primitives
│   └── shared observation vocabulary
│
├── Compiler
│   ├── harw-agent-dsl
│   ├── harw-agent-artifact
│   ├── harw-agent-compiler
│   └── harw-agent-runner
│
├── Shared Infrastructure
│   ├── authority
│   ├── sandbox
│   ├── operations
│   ├── storage primitives
│   └── secrets
│
├── Job Infrastructure
│   ├── harw-job-core
│   ├── harw-job-store
│   ├── harw-job-linux
│   ├── harw-job-darwin
│   ├── harw-job-tokio
│   ├── harw-job-runtime
│   └── harw-job
│
├── DoD
│   ├── sensors
│   ├── rules
│   ├── sentinel
│   ├── warden
│   └── probes
│
└── Harwness Application
    ├── harw-runtime
    ├── harw-cli
    ├── harw-tui
    ├── registry composition
    └── interactive/user-facing orchestration
```

---

# 28. Physical filesystem layout is secondary

Do **not** start this migration by moving dozens of directories.

First establish:

```text
ownership
dependency direction
public contracts
security budgets
workspace membership
```

Only then decide whether physical grouping helps.

Possible future layout:

```text
repo/
├── Cargo.toml
├── crates/
│   ├── foundations/
│   ├── compiler/
│   ├── infrastructure/
│   └── jobs/
├── dod/
│   └── crates/
├── app/
│   └── harwness/
└── xtask/
```

But this is optional.

Cargo package identity and dependency graph matter more than folder aesthetics.

---

# 29. Do not create `harw-common`

Avoid a giant generic dumping-ground crate.

Never solve ownership ambiguity by creating:

```text
harw-common
```

and placing unrelated utilities into it.

Prefer narrow leaf crates with explicit semantics.

Good:

```text
harw-digest
harw-fsutil
harw-protocol
```

Bad:

```text
harw-common::crypto::foo
harw-common::jobs::bar
harw-common::ui::baz
harw-common::random_helpers::qux
```

---

# 30. Shared type placement rule

A type belongs in a shared outer crate only when multiple independent domains need the **same semantic contract**.

Do not move a type outward merely because two crates import it today.

Ask:

```text
Is this truly shared meaning?
or
Are two applications accidentally coupled?
```

If the latter, use two domain types plus explicit conversion.

---

# 31. Authority placement rule

Authority and capability types deserve extra care.

If `harw-authority` models generic:

```text
capability
scope
permission contract
authority proof
```

it may belong in shared infrastructure/foundation.

If it embeds Harwness-specific policy names or interactive assumptions, split those outward.

Do not let authority become a backdoor dependency from foundations into applications.

---

# 32. Observation placement rule

Observation infrastructure has two layers:

```text
observation vocabulary
    low-level / shared

observation sinks and deployment
    infrastructure/application
```

For example:

```text
TraceContext
event IDs
structured fields
```

may be shared.

But:

```text
Prometheus server wiring
OTLP exporter startup
CLI flags
gateway configuration
```

belong farther outward.

---

# 33. Session-store ownership should be reviewed

`harw-session-store` currently also owns durable job storage behavior.

That does not imply generic jobs should remain owned there.

Target:

```text
harw-job-store
    generic durable job store mechanics

harw-session-store
    Harwness/session-oriented persistence
```

with adapters if necessary.

Do not make generic job semantics depend on the existence of an interactive Harwness session.

---

# 34. Existing process-job implementation should be decomposed, not deleted

`harw-tool-job` contains useful Linux process supervision behavior.

It should not simply be thrown away.

Extract generic process/supervisor mechanics into the new job infrastructure while preserving tool-facing behavior in the application adapter.

Conceptual migration:

```text
harw-tool-job
    today:
        tool API
        Linux process supervision
        ownership rules
        logs
        status
        cancellation

target:

harw-job-linux / harw-job-runtime
        Linux process supervision
        process identity
        cgroup
        recovery
        cancellation

harw-tool-job
        tool schema
        tool authorization
        session ownership presentation
        user-facing status/log commands
```

---

# 35. Compiler and job runtime should meet through explicit artifacts/contracts

Do not tightly couple the compiler to one executor implementation.

Preferred:

```text
Agent Compiler
    ↓
compiled artifact / execution requirements
    ↓
runtime admission
    ↓
target backend
```

Execution requirements may include:

```text
target OS
target arch
required tools
network policy
filesystem policy
resource limits
sandbox guarantees
eBPF requirement
kernel feature requirement
```

This lets a Windows-hosted compiler generate work for a Linux runner without pretending the local Windows runtime has equivalent sandbox semantics.

---

# 36. Platform support matrix

The ecosystem should explicitly distinguish layers.

## Compiler / artifact / client layer

```text
Linux      first-class
macOS      first-class
Windows    supported where practical
```

## High-assurance local execution layer

```text
Linux      primary and strongest
macOS      first-class Darwin implementation
Windows    not an architecture requirement
```

## DoD server/security layer

```text
Linux      primary target
macOS      only where the specific DoD component meaningfully maps
Windows    no parity promise
```

Do not publish one global "cross-platform" claim that hides these differences.

---

# 37. Linux may remain more capable than Darwin

First-class macOS support does not mean artificial feature parity.

Example:

```text
Linux:
    pidfd
    cgroup v2
    Landlock
    capabilities
    openat2
    Bubblewrap
    eBPF

Darwin:
    native process supervision
    kqueue-based eventing where appropriate
    rlimits
    Darwin/macOS sandbox primitives where available
    launchd integration where appropriate
```

The common policy layer should report what was actually enforced.

Never downgrade Linux to Darwin's common denominator.

Never pretend Darwin has a Linux primitive under another name.

---

# 38. Root workspace responsibilities

The umbrella workspace should centralize only truly shared build policy.

Appropriate root ownership:

```text
edition
rust-version
license
repository
unsafe lint policy
common dependency versions
common release profiles
common deny/audit configuration
```

Domain-specific features and dependencies stay with the packages that need them.

Do not centralize every crate version merely for tidiness if it destroys dependency clarity.

---

# 39. Unsafe policy

The root already uses:

```toml
[workspace.lints.rust]
unsafe_code = "forbid"
```

Preserve this.

Critical low-level crates may rely on reviewed dependencies that internally encapsulate unsafe boundaries.

First-party crates should remain safe Rust unless an architecture review creates a deliberately isolated exception.

Do not weaken the root rule to make one new syscall wrapper convenient.

---

# 40. Root workspace and native dependencies

A unified workspace must not accidentally cause feature unification to pull heavy/native dependencies into sensitive binaries.

Therefore audit:

```bash
cargo tree -e features
cargo tree -p harw-dod-warden
cargo tree -p harw-job-linux
cargo tree -p harw-agent-runner
```

Feature unification is part of architecture.

Not merely a build detail.

---

# 41. Root workspace membership is not permission to depend

This rule should be written explicitly in contributor docs:

> A crate being a member of the same Cargo workspace does not imply that another crate may depend on it.

Workspace membership means:

```text
shared repository/build governance
```

Dependency permission comes from:

```text
architectural ownership and layer rules
```

---

# 42. Recommended dependency classes

Use a small architectural vocabulary in reviews.

## Foundation

May be consumed broadly.

Must have low outward knowledge.

## Shared infrastructure

Reusable mechanism.

May depend on foundations.

## Domain

Owns domain semantics.

May depend on selected shared infrastructure.

## Application/composition

May compose broadly.

Should not be depended on by inner layers.

## Privileged TCB

Special restrictive domain.

Has an explicit allowlist independent of ordinary layering.

This makes review conversations much easier than arguing package-by-package without a model.

---

# 43. Example classification candidates

These are starting hypotheses, not immutable truth.

## Foundation-like

```text
harw-digest
harw-macros
parts of harw-types
parts of harw-protocol
```

## Compiler domain

```text
harw-agent-dsl
harw-agent-artifact
harw-agent-compiler
harw-agent-runner
```

## Shared infrastructure

```text
harw-authority
harw-sandbox
harw-fsutil
harw-observe
harw-extension-api
harw-operations
```

Each needs dependency review before final classification.

## Job infrastructure

```text
harw-job-*
```

## DoD domain

```text
dod/crates/*
```

## Harwness application/composition

```text
harw-runtime
harw-cli
harw-tui
registry/default composition
interactive runtime assembly
```

---

# 44. Classification must be derived bottom-up

Do not classify from names or desired architecture alone.

For every crate inspect:

```text
Cargo.toml dependencies
public signatures
domain vocabulary
feature flags
runtime assumptions
I/O assumptions
who consumes it
what it consumes
```

Then decide where it belongs.

This is especially important for crates with historically generic names such as:

```text
harw-core
harw-types
harw-ops
harw-runtime
```

---

# 45. No giant migration commit

Do not combine:

```text
DoD workspace merge
job extraction
directory relocation
crate renames
type splits
feature redesign
```

into one enormous change.

That would make regression attribution nearly impossible.

Use staged migration.

---

# 46. Migration Phase 0 — inventory

Before changing membership:

Generate a machine-readable architecture inventory.

For each crate record:

```text
package name
path
current workspace
direct dependencies
reverse dependencies
target ring
privileged?
platform-specific?
contains public domain types?
contains unsafe?
native dependencies?
```

Produce:

```text
docs/architecture/workspace-inventory.md
```

or equivalent generated report.

---

# 47. Migration Phase 1 — dependency graph classification

Classify every package:

```text
F = foundation
I = shared infrastructure
C = compiler
J = jobs
D = DoD
A = Harwness application/composition
T = privileged TCB
```

A package may have a primary class plus special constraints.

Example:

```text
harw-dod-warden
    D + T
```

Do not move files yet.

---

# 48. Migration Phase 2 — detect inversion edges

Find dependencies that point outward against the intended architecture.

Examples:

```text
foundation → Harwness runtime
shared infrastructure → CLI
job core → DoD domain
compiler IR → interactive TUI
```

For each inversion decide:

```text
move type inward
introduce adapter
split crate
invert trait
remove dependency
```

Do not use a facade dependency merely to hide an inversion.

---

# 49. Migration Phase 3 — prepare DoD for root metadata

Before removing the nested DoD workspace:

1. compare root and DoD package metadata
2. compare shared dependency versions
3. compare lint policy
4. compare profiles
5. identify DoD-only workspace dependencies
6. identify root-only versions that would change DoD resolution
7. run `cargo tree` snapshots for critical DoD binaries

Commit the inventory separately.

---

# 50. Migration Phase 4 — merge DoD into root Cargo workspace

Then:

```text
remove nested DoD workspace definition
remove root `exclude = ["dod"]`
add DoD package paths to root members
adopt root workspace package metadata
adopt root workspace dependency versions where semantically valid
regenerate root lockfile
```

Immediately compare critical dependency graphs.

Especially:

```text
harw-dod-warden
harw-dod-sentinel
harw-warden
harw-sentinel
harw-probe-bpf
harw-probe-fs
```

---

# 51. Migration Phase 5 — enforce TCB budgets

Before considering the DoD merge complete:

Add CI that fails if privileged packages gain unapproved dependencies.

The Warden budget must be machine-enforced.

A workspace merge without this step is incomplete.

---

# 52. Migration Phase 6 — extract generic job infrastructure

Only after ownership rules are clear, move existing generic job mechanics out of:

```text
harw-job-runtime
harw-session-store
harw-tool-job
harw-runtime
harw-cli/job_worker
```

into the new job family.

Do not move Harwness semantics with them.

---

# 53. Migration Phase 7 — clean outer foundation crates

Review:

```text
harw-macros
harw-types
harw-protocol
harw-observe
harw-authority
harw-extension-api
```

for accidental outward dependencies.

Where reasonable:

```text
move domain-specific tests outward
split runtime-only types
remove application vocabulary
```

Do not destabilize APIs without measurable architectural benefit.

---

# 54. Migration Phase 8 — optional physical regrouping

After dependency direction is clean, decide whether filesystem grouping improves navigation.

Possible result:

```text
crates/foundation/*
crates/compiler/*
crates/jobs/*
crates/infrastructure/*
dod/crates/*
apps/harwness/*
```

This is optional.

Do not make directory beauty a prerequisite for architectural correctness.

---

# 55. Architecture tests

The repository should eventually test architecture automatically.

Potential checks:

```text
foundations may not depend on application crates
job core may not depend on Harwness or DoD
Warden may depend only on allowlisted crates
compiler core may not depend on interactive UI
platform-specific executors stay cfg-isolated
```

Implement using:

```text
cargo metadata
a small xtask architecture checker
```

rather than manually maintaining fragile grep scripts where possible.

---

# 56. Example architecture policy file

A future machine-readable file could look conceptually like:

```toml
[package.harw-macros]
layer = "foundation"

[package.harw-job-core]
layer = "jobs"

[package.harw-runtime]
layer = "application"

[package.harw-dod-warden]
layer = "dod"
tcb = true

[rules]
foundation_may_depend_on = ["foundation"]
jobs_may_depend_on = ["foundation", "infrastructure", "jobs"]
application_may_depend_on = ["*"]
```

Exact syntax is not prescribed.

The important goal is machine-verifiable intent.

---

# 57. Public API ownership

When moving a type, preserve semantic ownership.

Example:

Bad:

```text
JobScope uses TenantId because Harwness already had TenantId.
```

Better:

```text
generic JobScopeId / authority token in job core
```

and:

```text
Harwness adapter maps TenantId/WorkspaceId into its job admission context.
```

DoD may map completely different security context.

---

# 58. Trace context ownership

Tracing is cross-cutting but not necessarily core job identity.

Avoid making the minimal job state machine depend on a full observation stack.

Possible structure:

```text
harw-job-core
    no trace dependency

harw-job-runtime
    optional trace context adapter
```

or a tiny generic correlation identifier in core.

Do not let observability become a dependency inversion.

---

# 59. Serialization ownership

The lowest generic layers should avoid assuming:

```text
all payloads are serde_json::Value
```

merely because Harwness currently uses JSON heavily.

Prefer:

```text
typed payload contracts
versioned envelopes
codec boundary
```

at the appropriate layer.

This matters if jobs become a reusable ecosystem facility.

---

# 60. Compiler artifacts and workspace ownership

Compiled agent artifacts belong to the compiler ecosystem, not to the interactive application.

Harwness can consume them.

Standalone runners can consume them.

Remote job runtimes can consume them.

DoD may potentially produce or consume restricted artifacts later without pulling in full Harwness runtime.

Keep this direction available.

---

# 61. Server-world priority

The execution and DoD architecture should optimize for the environments they are realistically deployed on.

Primary operational assumption:

```text
Linux servers
```

Secondary first-class developer/execution host:

```text
macOS
```

Windows remains useful for:

```text
Office
Visual Studio-specific workflows
enterprise desktop requirements
compiler/client use
```

but it must not determine the Linux server security model.

---

# 62. Design principle: portability where natural, native semantics where valuable

Use portability for things that are naturally portable:

```text
DSL
IR
state machines
artifact format
lease math
retry policy
protocol types
compiler passes
```

Use OS-native semantics where they materially improve correctness:

```text
pidfd
cgroup v2
Landlock
kqueue
Darwin process primitives
platform-native sandboxing
```

Do not confuse abstraction with erasure.

---

# 63. Harwness remains important

Moving shared crates "outside" Harwness does not diminish Harwness.

It clarifies its role.

Harwness becomes:

```text
the major agent compiler/runtime application
that composes the Harw ecosystem
```

rather than:

```text
a directory that conceptually owns every crate in the repository
```

That is a sign of architectural maturation, not fragmentation.

---

# 64. DoD remains independent in policy

Likewise, bringing DoD into the umbrella Cargo workspace does not make DoD "part of Harwness".

It becomes:

```text
a peer ecosystem domain
```

with its own:

```text
privilege model
TCB rules
process boundaries
security semantics
dependency budgets
```

---

# 65. The target mental model

Use this in future architecture discussions:

```text
                       HARW
                        │
       ┌────────────────┼──────────────────┐
       │                │                  │
       ▼                ▼                  ▼
  Compiler stack    Job runtime           DoD
       │                │                  │
       │                │                  │
       └────────┬───────┴──────────┬───────┘
                │                  │
                ▼                  ▼
         shared infrastructure   security foundations
                │                  │
                └────────┬─────────┘
                         ▼
                    foundations
                         ▲
                         │
                    Harwness
              application/composition
```

The arrows are conceptual dependencies, not exact Cargo edges.

The key point is:

```text
Harwness is inside Harw.
Harw is not inside Harwness.
```

---

# 66. Immediate implementation tasks

The next coding agent should **not** begin by moving directories.

First:

1. inspect all workspace packages with `cargo metadata`
2. classify them by layer
3. identify inversion edges
4. snapshot critical DoD dependency graphs
5. inspect what the DoD nested workspace contributes beyond metadata duplication
6. draft the exact root-member migration
7. inspect the job subsystem extraction boundaries
8. propose changes before implementation

Only after review should Cargo membership be changed.

---

# 67. Required deliverables before migration

Produce:

```text
docs/architecture/harw-workspace-inventory.md
docs/architecture/harw-dependency-inversions.md
docs/architecture/dod-workspace-merge-plan.md
docs/architecture/job-extraction-map.md
```

These may be generated from the repository where practical.

No mass refactor before these exist.

---

# 68. Acceptance criteria

The workspace architecture migration is successful when:

- [ ] The root workspace is explicitly treated as the Harw ecosystem workspace.
- [ ] Harwness is documented as an application/composition domain, not owner of all shared crates.
- [ ] `harw-macros` is treated as ecosystem foundation infrastructure.
- [ ] Compiler crates are treated as a reusable compiler domain.
- [ ] Generic jobs are treated as shared execution infrastructure.
- [ ] DoD is treated as a peer domain.
- [ ] DoD crates are root workspace members.
- [ ] The nested DoD Cargo workspace is retired.
- [ ] Root workspace metadata/dependency versions cover DoD intentionally.
- [ ] The Warden dependency/TCB budget is machine-enforced.
- [ ] Workspace membership is explicitly separated from dependency permission.
- [ ] Foundation crates do not depend on application/composition crates.
- [ ] Job core does not depend on Harwness or DoD semantics.
- [ ] Harwness-specific job adapters own tenant/workspace/agent semantics.
- [ ] DoD-specific job adapters own DoD semantics.
- [ ] Compiler portability is not conflated with execution-platform parity.
- [ ] Linux remains free to expose stronger native execution semantics.
- [ ] macOS remains a first-class Darwin target.
- [ ] Windows support does not weaken server/security architecture.
- [ ] `unsafe_code = "forbid"` remains workspace policy.
- [ ] Critical feature-unification/native-dependency graphs are checked in CI.
- [ ] No giant `harw-common` dumping-ground is introduced.
- [ ] Physical directory moves, if any, happen only after dependency ownership is clean.

---

# 69. Final architecture statement

The repository has outgrown the idea that Harwness is the container and every `harw-*` crate is an internal implementation detail.

The correct model is now:

```text
Harw
    = ecosystem / compiler / execution / security platform

Harwness
    = major application and composition root

harw-macros / foundational crates
    = ecosystem-level building blocks

harw-job-*
    = shared execution infrastructure

DoD
    = peer security domain with a hardened privileged TCB
```

Cargo should increasingly reflect that model.

But architecture comes before directory layout.

The migration order is:

```text
understand
→ classify
→ enforce dependency direction
→ merge workspace governance
→ extract shared infrastructure
→ move files only where useful
```

That is the target.
