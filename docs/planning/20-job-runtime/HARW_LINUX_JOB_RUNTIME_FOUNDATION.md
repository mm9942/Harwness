# Harw Linux Job Runtime Foundation
## Implementation Specification for Claude Code / Agentic Development

**Status:** Architecture directive  
**Scope:** Linux job execution, supervision, resource control, sandboxing, persistence, recovery  
**Primary consumer:** Harwness  
**Design goal:** A reusable Rust job runtime that is not coupled to Harwness and can later be consumed by Harwness, DoD, standalone services, CLI tools, or other Rust applications.

---

# 1. Why this document exists

The job runtime is now deep enough in Linux process semantics that it must not continue growing as a collection of local helpers.

The implementation should **not** reimplement Linux primitives that are already exposed through maintained Rust libraries. The interesting work is the semantic composition:

```text
JobId
  ↓
Lease / claim
  ↓
Linux resource boundary
  ↓
Process identity
  ↓
Sandbox policy
  ↓
Execution
  ↓
Observation
  ↓
Durable outcome
```

The goal is therefore:

> Build Harw's job semantics ourselves, but consume the strongest practical Rust abstractions for the Linux ABI, process supervision, `/proc`, capabilities, filesystem authority, Landlock, and cgroups.

This document is intentionally prescriptive. It exists to stop future implementations from drifting back toward hand-written `/proc` parsing, PID-only authority, duplicated Linux syscall wrappers, or an unnecessarily large C/C++ dependency surface.

---

# 2. Non-negotiable principles

## 2.1 Do not use a numeric PID as process authority

A numeric PID is metadata.

For a live Linux process, the authoritative handle should be a **pidfd**.

The conceptual distinction is:

```text
PID
  = display / persistence / recovery metadata

pidfd
  = live kernel-backed process identity and control handle
```

The runtime must not rely on:

```text
pid + /proc/<pid>/stat starttime
```

as the primary mechanism while a pidfd is available.

That pair may still be useful for recovery after a runtime restart because pidfds are not persistable across process restarts.

---

## 2.2 One process is not one job

The primary process belongs to a larger execution boundary.

Use:

```text
pidfd
    = primary process identity

cgroup v2
    = job process tree / resource domain
```

A process may create children, grandchildren, or new process groups. Process-group signaling alone is therefore not the strongest job-wide termination primitive.

Where available, a dedicated cgroup v2 should be the job-wide process boundary.

---

## 2.3 Our crates must contain no unsafe Rust

Every new first-party crate created for this subsystem should use:

```rust
#![forbid(unsafe_code)]
```

This requirement applies to **our code**.

It does not mean every transitive dependency must contain zero unsafe internally. Safe Rust libraries necessarily encapsulate unsafe ABI boundaries somewhere.

The desired trust boundary is:

```text
Harw first-party job crates
    0 unsafe blocks

Reviewed Rust foundation crates
    may encapsulate unsafe internally

Default Linux feature graph
    no compiled C/C++ components
    no custom C shims
    no arbitrary *-sys crates
```

Any exception must be explicit, documented, and feature-gated.

---

## 2.4 Default Linux path must remain Rust-native

The default feature graph should not pull in:

```text
libbpf-sys
libseccomp
OpenSSL-sys
custom C wrappers
C helper binaries
C++ libraries
```

This is a default-path rule, not a claim that such components can never exist in optional executors.

A future specialized executor may have a broader TCB if it is isolated in a separate crate and feature.

---

## 2.5 Do not expose third-party implementation types in our public API

External crates are implementation foundations, not our public contract.

For example, public APIs should expose:

```rust
pub struct CgroupSpec { ... }
pub struct CgroupHandle { ... }
pub struct LinuxProcess { ... }
pub struct SandboxPolicy { ... }
```

instead of forcing consumers to depend on Youki-specific, procfs-specific, or Landlock-specific concrete types.

This keeps the implementation replaceable without breaking the public API.

---

# 3. Dependency decisions

The following choices are the current architecture baseline.

## 3.1 rustix — Linux/POSIX system foundation

Use `rustix` as the primary low-level OS interface.

It should be the preferred provider for:

```text
pidfd_open
pidfd_send_signal
waitid(P_PIDFD)
rlimits / prlimit
Linux capabilities
NO_NEW_PRIVS
process signaling
process groups
openat2
event primitives where useful
safe OwnedFd / AsFd based APIs
```

Do not create local wrappers around raw `libc::syscall` when rustix already exposes the needed operation.

Do not add an extra pidfd crate unless rustix is demonstrably missing a capability we require.

### Conceptual live-process type

```rust
pub struct LinuxProcess {
    pid: rustix::process::Pid,
    pidfd: std::os::fd::OwnedFd,
}
```

Requirements:

- no `Clone`
- no public raw file descriptor
- PID may be readable for diagnostics
- pidfd is the authority for live process operations
- ownership semantics must be explicit

---

## 3.2 Tokio AsyncFd — async pidfd integration

When async process supervision is needed, wrap the pidfd with Tokio:

```rust
pub struct AsyncLinuxProcess {
    pid: rustix::process::Pid,
    pidfd: tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>,
}
```

Expected lifecycle:

```text
spawn
  ↓
pidfd_open
  ↓
AsyncFd waits for readiness
  ↓
waitid(PidFd)
  ↓
typed process outcome
```

Do not add `async-pidfd` by default. Rustix + Tokio already provide the pieces we need.

---

## 3.3 procfs — Linux process introspection and recovery metadata

Use `procfs` for `/proc` parsing and introspection.

Do not expand a private Harw procfs parser into a second general procfs implementation.

Use this division:

```text
pidfd
    live process identity and control

procfs
    observation
    metadata
    recovery support
    diagnostics
```

Potential persisted recovery metadata:

```text
pid
process start time
cgroup identity
command metadata
job id
runner instance id
```

After a runtime restart, this metadata may help determine whether a previous process still exists and is plausibly associated with the persisted job.

PID alone must never be considered sufficient proof.

---

## 3.4 cap-std — capability-oriented filesystem access

Use `cap-std` where the runtime owns durable job state, logs, artifacts, or workspace-scoped filesystem access.

Prefer:

```rust
fn write_log(root: &cap_std::fs::Dir, job: JobId, ...)
```

over APIs that accept ambient absolute paths:

```rust
fn write_log(path: &Path, ...)
```

The intended security model is:

> Filesystem authority should be represented by a value that can be passed to code, not by a string that implicitly grants access to the global filesystem namespace.

Candidate uses:

```text
job state root
log root
artifact root
workspace root
temporary job root
```

Do not mix ambient `PathBuf` authority and capability-oriented `Dir` handles without a clear boundary.

---

## 3.5 rust-landlock — process sandbox restrictions

Use the `landlock` crate rather than implementing Landlock syscalls directly.

Expose our own policy model and translate it to Landlock internally.

Potential public profiles:

```rust
pub enum SandboxProfile {
    WorkspaceBuild,
    ReadOnlyAnalysis,
    NoNetwork,
    NetworkRestricted,
}
```

The public API should not make consumers reason directly about Landlock ABI versions unless they explicitly request low-level control.

Internally, decide whether a policy is:

```text
BestEffort
or
HardRequirement
```

The caller must be able to learn whether a requested sandbox was:

```text
fully enforced
partially enforced
not enforceable
```

Do not silently claim full sandboxing when the host kernel lacks required features.

---

## 3.6 libcgroups from Youki — first cgroup v2 backend

Use Youki's `libcgroups` as the first implementation candidate for cgroup v2.

Start with the smallest feature graph:

```toml
libcgroups = {
    version = "0.7",
    default-features = false,
    features = ["v2"]
}
```

Do **not** enable:

```text
cgroupsv2_devices
```

in the default backend.

That optional path brings in a BPF/C-oriented dependency surface we do not need for the initial job runtime.

Wrap the dependency behind our own trait:

```rust
pub trait CgroupBackend {
    fn create(
        &self,
        spec: &CgroupSpec,
    ) -> Result<CgroupHandle, CgroupError>;

    fn attach(
        &self,
        group: &CgroupHandle,
        process: &LinuxProcess,
    ) -> Result<(), CgroupError>;

    fn freeze(
        &self,
        group: &CgroupHandle,
    ) -> Result<(), CgroupError>;

    fn kill(
        &self,
        group: &CgroupHandle,
    ) -> Result<(), CgroupError>;

    fn stats(
        &self,
        group: &CgroupHandle,
    ) -> Result<CgroupStats, CgroupError>;
}
```

Why the wrapper exists:

- keeps public API stable
- prevents leaking Youki types
- enables a Kata backend later
- enables a small direct v2 backend later if justified
- lets tests use a fake backend

---

## 3.7 pathrs — optional, not default

Do not add `pathrs` immediately.

Start with:

```text
cap-std
+
rustix::openat2 where needed
```

Introduce `pathrs` only if we encounter a real security-sensitive path-resolution problem that is not cleanly solved by the above.

Avoid maintaining multiple overlapping path-authority abstractions without a concrete reason.

---

# 4. Explicitly deferred dependencies and features

The following are intentionally not part of v1.

## 4.1 No extra pidfd crate

Do not add:

```text
async-pidfd
pop-os/pidfd
another pidfd wrapper
```

unless rustix/Tokio cannot provide a required behavior.

---

## 4.2 No caps-rs by default

Rustix already exposes Linux capabilities.

Avoid duplicate capability abstractions.

---

## 4.3 No mandatory seccomp implementation in v1

Do not make seccomp a v1 prerequisite.

The initial Linux isolation stack is already meaningful:

```text
pidfd
+
cgroup v2
+
rlimits
+
NO_NEW_PRIVS
+
capability dropping
+
Landlock
+
capability-oriented filesystem access
```

A future seccomp backend may be introduced separately.

If it requires C/libseccomp, that must be a deliberate optional feature or separate executor with its own documented TCB.

---

## 4.4 No namespace / clone3 requirement in v1

Do not add local unsafe code merely to support:

```text
clone3
unshare
namespace construction
```

A future containerized executor may use a dedicated runtime such as Youki.

Example future crate:

```text
harw-job-executor-container
```

That crate may have a different trust and dependency budget.

The standard Linux executor must remain safe Rust.

---

## 4.5 No io_uring for the sake of io_uring

The v1 workload is:

```text
wait for process
read stdout
read stderr
write logs
react to cancellation
collect outcome
```

Tokio + pidfd + ordinary async pipes are sufficient.

Do not add io_uring until measurements show a concrete scaling problem.

---

# 5. Proposed crate structure

The subsystem should be split by semantic responsibility rather than by arbitrary file size.

```text
harw-job/
├── harw-job-core/
├── harw-job-store/
├── harw-job-linux/
├── harw-job-tokio/
├── harw-job-runtime/
└── harw-job/
```

Potential future crates:

```text
harw-job-service/
harw-job-hyper/
harw-job-executor-container/
```

---

# 6. harw-job-core

`harw-job-core` is platform-neutral.

It owns the job model and state machine.

It should contain:

```text
JobId
AttemptId
RunnerId
LeaseId / fencing token
JobSpec
JobState
JobOutcome
ExitOutcome
RetryPolicy
Deadline
Budget
CancellationCause
IdempotencyKey
Claim
Lease
transition validation
```

It should **not** depend on:

```text
rustix
Tokio
procfs
Landlock
libcgroups
Hyper
Tower
systemd
```

The core state machine must be testable on any platform.

Example lifecycle:

```text
Created
  ↓
Queued
  ↓
Claimed
  ↓
Starting
  ↓
Running
  ├──→ Succeeded
  ├──→ Failed
  ├──→ Cancelled
  ├──→ TimedOut
  └──→ Lost
```

All transitions must be validated.

Do not let persistence code write arbitrary state values.

---

# 7. harw-job-store

This crate owns durable state.

Responsibilities:

```text
job records
attempt records
leases
fencing tokens
state transitions
recovery metadata
log metadata
artifact metadata
atomic updates
crash consistency
```

Prefer capability-oriented root handles using `cap-std`.

The store API should be independent from the execution backend.

Example:

```rust
pub trait JobStore {
    fn create_job(&self, spec: JobSpec) -> Result<JobRecord, StoreError>;

    fn claim_job(
        &self,
        job: JobId,
        runner: RunnerId,
    ) -> Result<Claim, StoreError>;

    fn transition(
        &self,
        claim: &Claim,
        transition: JobTransition,
    ) -> Result<JobRecord, StoreError>;

    fn load_recovery_set(
        &self,
        runner: RunnerId,
    ) -> Result<Vec<RecoveryRecord>, StoreError>;
}
```

A stale runner must not be able to mutate a job after losing its lease.

Use a fencing token or equivalent monotonic authority.

---

# 8. harw-job-linux

This is the Linux-specific synchronous systems layer.

Expected internal modules:

```text
process/
pidfd/
cgroup/
sandbox/
capabilities/
resources/
proc/
filesystem/
recovery/
```

Primary public concepts:

```rust
pub struct LinuxProcess { ... }
pub struct LinuxJobGroup { ... }
pub struct LinuxResources { ... }
pub struct LinuxSandbox { ... }
pub struct LinuxRecoveryIdentity { ... }
```

Do not expose raw third-party handles unless explicitly required by an expert extension API.

---

# 9. Process creation and identity

The runner may initially continue to spawn with `std::process::Command` or Tokio process APIs if that is sufficient.

Immediately after spawn:

1. obtain the PID
2. convert to `rustix::process::Pid`
3. open a pidfd
4. construct `LinuxProcess`
5. attach to the dedicated job cgroup
6. persist recovery metadata
7. enter running state only after the runtime boundary is established

Conceptual flow:

```text
Job claimed
  ↓
Create cgroup
  ↓
Prepare sandbox/resources
  ↓
Spawn child
  ↓
pidfd_open
  ↓
Attach job cgroup
  ↓
Persist live identity
  ↓
Running
```

Avoid a long window where a child is running without its intended controls.

If ordering cannot be fully atomic in v1, document the race and reduce it as far as practical.

---

# 10. Job-wide termination

Cancellation should be layered.

Preferred semantic flow:

```text
cancel requested
  ↓
mark cancellation intent durably
  ↓
graceful signal to primary process
  ↓
wait grace period
  ↓
terminate entire job cgroup if still alive
  ↓
collect final process status
  ↓
persist outcome
```

The exact signal policy should be configurable.

Example:

```rust
pub struct TerminationPolicy {
    pub graceful_signal: SignalKind,
    pub grace_period: Duration,
    pub hard_kill: bool,
}
```

Do not assume process-group kill is sufficient to terminate every descendant.

---

# 11. Resource policy

Represent limits in our types.

Example:

```rust
pub struct JobResources {
    pub memory_max: Option<u64>,
    pub memory_high: Option<u64>,
    pub cpu_weight: Option<u64>,
    pub cpu_max: Option<CpuQuota>,
    pub pids_max: Option<u64>,
    pub io: Option<IoLimits>,
    pub rlimits: RlimitSet,
}
```

Translation to cgroup v2 and `setrlimit`/`prlimit` belongs in `harw-job-linux`.

Do not let callers write arbitrary cgroup filesystem strings through the normal safe API.

An expert raw-unified extension can exist later if required.

---

# 12. Sandbox policy

Expose a Harw-native sandbox model.

Example:

```rust
pub struct SandboxPolicy {
    pub filesystem: FilesystemPolicy,
    pub network: NetworkPolicy,
    pub capabilities: CapabilityPolicy,
    pub no_new_privs: bool,
    pub profile: SandboxProfile,
}
```

The Linux backend translates this into:

```text
Landlock
capability drop
NO_NEW_PRIVS
cap-std rooted access
optional rlimits/cgroup restrictions
```

Policy application must return an enforcement report.

Example:

```rust
pub struct SandboxReport {
    pub landlock: EnforcementState,
    pub capabilities: EnforcementState,
    pub no_new_privs: EnforcementState,
    pub filesystem_rooting: EnforcementState,
}
```

Do not report "sandboxed" as a single boolean if some requested controls could not be applied.

---

# 13. Async runtime layer

`harw-job-tokio` adapts synchronous Linux handles into async supervision.

Responsibilities:

```text
AsyncFd<pidfd>
stdout
stderr
stdin
timeouts
cancellation
event fan-in
log streaming
```

A useful internal event model:

```rust
pub enum ProcessEvent {
    Stdout(Bytes),
    Stderr(Bytes),
    Exited(ExitOutcome),
    Timeout,
    CancelRequested,
    SupervisorError(ProcessError),
}
```

Do not let the async layer own the durable job state machine.

It produces events; the runtime decides transitions.

---

# 14. harw-job-runtime

This crate coordinates:

```text
store
claiming
leases
executor
recovery
supervision
cancellation
retry
deadline
budget
finalization
```

Conceptual architecture:

```text
                ┌───────────────┐
                │   JobStore    │
                └───────┬───────┘
                        │
                claim / transition
                        │
                        ▼
                ┌───────────────┐
                │ Job Runtime   │
                └───┬───────┬───┘
                    │       │
                    │       └──────────────┐
                    ▼                      ▼
             Linux Executor          Tokio Supervisor
                    │                      │
                    └──────────┬───────────┘
                               ▼
                           Job Outcome
```

The runtime must be restartable.

A crash of the Harw process must not automatically imply that every job becomes unidentifiable.

---

# 15. Recovery semantics

Recovery is a first-class requirement.

Persist enough metadata to reason about a previously running job:

```rust
pub struct LinuxRecoveryIdentity {
    pub pid: u32,
    pub process_start_time: Option<u64>,
    pub cgroup_path: Option<String>,
    pub executable: Option<String>,
    pub runner_id: RunnerId,
    pub attempt_id: AttemptId,
}
```

After restart:

```text
load Running/Starting records
  ↓
inspect cgroup
  ↓
inspect PID/proc metadata
  ↓
decide:
    still running
    already exited
    identity mismatch
    unrecoverable
  ↓
transition deterministically
```

A PID match by itself is insufficient.

If identity cannot be established with enough confidence, mark the attempt as lost/unrecoverable rather than signaling an unrelated process.

---

# 16. Public facade

The top-level `harw-job` crate should make the common case small.

Example target API:

```rust
let runtime = JobRuntime::builder()
    .store(store)
    .executor(LinuxExecutor::default())
    .build()?;

let job = runtime.submit(
    JobSpec::command("cargo")
        .arg("test")
        .workspace(workspace)
        .resources(resources)
        .sandbox(SandboxProfile::WorkspaceBuild),
).await?;

let outcome = job.wait().await?;
```

The common user should not need to know:

```text
pidfd details
procfs parsing
cgroup file names
Landlock ABI versions
capability bit numbers
Tokio AsyncFd mechanics
```

The expert API may expose lower layers separately.

---

# 17. Feature design

Keep optional system features explicit.

Possible feature structure:

```toml
[features]
default = ["linux-basic"]

linux-basic = [
    "dep:rustix",
    "dep:procfs",
]

linux-sandbox = [
    "linux-basic",
    "dep:landlock",
    "dep:cap-std",
]

linux-cgroup-v2 = [
    "linux-basic",
    "dep:libcgroups",
]

tokio = [
    "dep:tokio",
]
```

Do not blindly use these exact feature names if the existing workspace has better naming conventions.

The important property is that:

```text
base model
≠
Linux implementation
≠
async implementation
≠
sandbox backend
≠
cgroup backend
```

---

# 18. Dependency policy

Before adding a dependency, answer:

1. Is the behavior already available through rustix or an existing dependency?
2. Does the crate introduce C/C++ compilation?
3. Does it introduce a new syscall abstraction overlapping rustix?
4. Does it expose a type we are about to leak into our public API?
5. Does it materially increase the trusted computing base?
6. Is the crate actively maintained?
7. Can the dependency be optional?
8. Is its unsafe boundary reviewable?

Do not add a second crate merely because its convenience API saves twenty lines.

---

# 19. Supply-chain and unsafe CI gates

Add CI checks for:

```text
cargo fmt
cargo clippy
cargo test
cargo nextest if already used
cargo deny
cargo audit / RustSec equivalent
cargo geiger report
```

First-party crates must fail CI if `unsafe` appears.

Suggested static rule:

```text
grep or dedicated lint:
all harw-job-* first-party crates must include #![forbid(unsafe_code)]
```

Maintain an explicit dependency review document for crates that encapsulate unsafe Linux ABI behavior.

Suggested categories:

```text
Tier A — trusted foundations
    rustix
    Tokio
    procfs
    cap-std
    landlock
    libcgroups (restricted features)

Tier B — optional / review before enablement
    pathrs
    container runtime integration

Tier C — forbidden in default graph
    libbpf-sys
    libseccomp
    arbitrary C shims
```

---

# 20. Dependency graph checks

CI should verify that the default Linux build does not accidentally gain unwanted native dependencies.

Useful checks:

```bash
cargo tree
cargo tree -e features
cargo tree -i libc
cargo tree -i libbpf-sys
cargo tree -i libseccomp
cargo tree -i openssl-sys
```

Do not assume an optional dependency stays optional forever.

Feature unification can silently change the effective graph in a workspace.

---

# 21. Test strategy

Tests must be split into semantic, Linux, and privileged integration levels.

## 21.1 Pure unit tests

Run everywhere.

Cover:

```text
job state transitions
lease expiry
fencing
retry rules
deadline calculations
budget accounting
idempotency
outcome mapping
```

---

## 21.2 Unprivileged Linux tests

Cover:

```text
pidfd open
pidfd signal
waitid(P_PIDFD)
stdout/stderr
process exit
NO_NEW_PRIVS
procfs metadata
basic Landlock where supported
cap-std filesystem rooting
```

Tests must gracefully skip kernel features that are genuinely unavailable unless the test explicitly requires them.

---

## 21.3 Cgroup v2 integration tests

Cover:

```text
create
attach
pids.max
memory limit
stats
freeze/thaw
kill
cleanup
descendant termination
```

Run only in CI environments where cgroup delegation is available.

Do not fake successful enforcement if the environment cannot provide it.

---

## 21.4 Recovery tests

Mandatory scenarios:

```text
runner crashes while child continues
runtime restarts and recovers matching process
PID reused by unrelated process
cgroup exists but primary PID exited
process exists but recovery identity mismatches
job completed before recovery
stale lease attempts mutation
```

The PID-reuse test is especially important.

The runtime must prove it will not kill an unrelated process merely because a persisted PID now exists again.

---

# 22. Failure semantics

Use typed errors.

Avoid stringly-typed Linux failures crossing every layer.

Possible domains:

```rust
pub enum ProcessError { ... }
pub enum CgroupError { ... }
pub enum SandboxError { ... }
pub enum RecoveryError { ... }
pub enum StoreError { ... }
pub enum RuntimeError { ... }
```

Map raw OS errors at the Linux boundary.

Higher layers should be able to distinguish:

```text
unsupported feature
permission denied
resource exhausted
identity mismatch
process already exited
cgroup unavailable
sandbox partially enforced
store conflict
lease lost
```

---

# 23. Observability

The runtime should emit structured tracing events.

Important fields:

```text
job_id
attempt_id
runner_id
pid
cgroup
transition
lease_id
fencing_token
sandbox_profile
exit_code
signal
duration
cancel_reason
recovery_result
```

Never log secrets, private key material, or arbitrary environment variables by default.

Command lines may also contain secrets; treat complete argv logging as policy-controlled.

---

# 24. Security-sensitive ordering

When starting a job, explicitly reason about the order of:

```text
resource-domain creation
process spawn
cgroup attachment
capability reduction
NO_NEW_PRIVS
Landlock
environment setup
working directory
stdio plumbing
exec
pidfd acquisition
persistence
```

Do not assume the ordering is harmless.

If a control must occur pre-exec and the safe Rust process API cannot guarantee it, document the limitation instead of introducing unreviewed unsafe code.

A later specialized executor may solve that limitation.

---

# 25. What not to do

Do not:

```text
reimplement pidfd syscalls
reimplement Linux capabilities
grow a home-grown procfs parser
use PID as sole process identity
make process group equal job boundary
enable libcgroups BPF devices in default features
pull libseccomp into the default graph
add clone3 unsafe code to first-party crates
introduce io_uring without evidence
expose Youki/procfs/Landlock types as public API
silently downgrade sandbox guarantees
claim a job is sandboxed with a bool
kill a recovered PID without identity validation
```

---

# 26. Implementation phases

## Phase 0 — inventory

Before changing code:

- locate current job execution implementation
- locate current process kill logic
- locate current procfs parser
- locate retry/cancellation/deadline logic
- locate persistence
- locate any cgroup support
- locate existing rustix/Tokio versions
- inspect workspace feature unification
- inspect current unsafe blocks
- inspect native build dependencies

Produce a short inventory before major refactoring.

---

## Phase 1 — extract core model

Create or isolate:

```text
JobId
AttemptId
JobSpec
JobState
JobOutcome
transitions
retry
deadline
lease/fencing
```

No Linux dependencies.

Add exhaustive state-machine tests.

---

## Phase 2 — pidfd process handle

Introduce `LinuxProcess`.

Replace live PID authority with pidfd.

Implement:

```text
open
signal
wait
status
diagnostic pid
```

Keep existing behavior working behind migration adapters until tests pass.

---

## Phase 3 — procfs cleanup

Replace general-purpose local proc parsing with `procfs`.

Retain only genuinely project-specific interpretation.

Separate:

```text
identity/control
from
observation/recovery
```

---

## Phase 4 — cgroup job boundary

Add `CgroupBackend`.

Implement Youki `libcgroups` v2 backend.

Create one cgroup per job attempt.

Add attach, stats, freeze, kill, cleanup.

Do not enable BPF device support.

---

## Phase 5 — sandbox policy

Add Harw-native sandbox types.

Implement:

```text
NO_NEW_PRIVS
capability drop
Landlock
cap-std rooted storage/workspace authority
```

Return an enforcement report.

---

## Phase 6 — Tokio supervisor

Introduce `AsyncFd<OwnedFd>`.

Unify:

```text
pidfd exit readiness
stdout
stderr
deadline
cancellation
```

into a single supervisor event loop.

---

## Phase 7 — durable recovery

Persist recovery identity.

Implement restart reconciliation.

Add PID reuse / identity mismatch tests.

---

## Phase 8 — supply-chain enforcement

Add:

```text
forbid unsafe
cargo-deny
RustSec
cargo-geiger report/gate
native dependency graph checks
```

Document approved foundation crates.

---

## Phase 9 — public facade

Provide a small ergonomic top-level API.

Keep lower layers separately consumable.

Do not couple it to Harwness-specific agent types.

---

# 27. Acceptance criteria

The work is not complete until all of the following are true:

- [ ] First-party job crates contain `#![forbid(unsafe_code)]`
- [ ] Live process authority uses pidfd on supported Linux
- [ ] PID is not the sole process identity
- [ ] Process exit can be observed through pidfd/waitid
- [ ] Tokio supervision can use `AsyncFd<OwnedFd>`
- [ ] General `/proc` parsing uses `procfs`
- [ ] Job-wide process ownership can use cgroup v2
- [ ] Cgroup implementation is behind a project-owned trait
- [ ] Default `libcgroups` features do not enable BPF device support
- [ ] Sandbox API is project-owned
- [ ] Landlock enforcement state is observable
- [ ] Capabilities can be reduced without a second capability crate
- [ ] `NO_NEW_PRIVS` is supported where requested
- [ ] Durable recovery handles runtime restart
- [ ] PID reuse cannot cause an unrelated process to be killed
- [ ] Default Linux dependency graph has no required C/C++ compilation
- [ ] No libbpf-sys in default graph
- [ ] No libseccomp in default graph
- [ ] No required OpenSSL-sys in this subsystem
- [ ] No namespace/clone3 unsafe code in first-party crates
- [ ] No unnecessary io_uring dependency
- [ ] State transitions are validated and tested
- [ ] Lease/fencing prevents stale runners from mutating jobs
- [ ] cgroup integration tests exist where the CI environment permits them
- [ ] supply-chain CI gates are documented and active

---

# 28. Final architecture target

```text
                    ┌──────────────────────┐
                    │       harw-job       │
                    │      facade API      │
                    └──────────┬───────────┘
                               │
                    ┌──────────▼───────────┐
                    │  harw-job-runtime    │
                    │ claims / recovery /  │
                    │ cancel / retry       │
                    └──────┬────────┬──────┘
                           │        │
              ┌────────────▼──┐  ┌──▼──────────────┐
              │ harw-job-store│  │ harw-job-tokio │
              │ durable state │  │ async supervisor│
              └───────────────┘  └──┬──────────────┘
                                     │
                            ┌────────▼────────┐
                            │ harw-job-linux  │
                            │                │
                            │ rustix         │
                            │ procfs         │
                            │ landlock       │
                            │ cap-std        │
                            │ libcgroups(v2) │
                            └────────┬────────┘
                                     │
                                     ▼
                              Linux kernel
```

The important line is:

```text
We own job semantics.
The Rust ecosystem owns the ugly Linux ABI edges.
```

That is the architecture.

---

# 29. Instruction to the implementing agent

Do not begin by rewriting the subsystem wholesale.

First inspect the existing repository and map existing behavior to this document.

Preserve working semantics unless this specification explicitly replaces them.

Prefer incremental extraction:

```text
identify
→ wrap
→ test
→ migrate caller
→ remove obsolete path
```

Before adding any low-level Linux code, search the already-selected foundation crates for an existing safe API.

Before adding any new dependency, justify why the current stack cannot provide the requirement.

Do not optimize for minimum line count.

Optimize for:

```text
correctness
safe ownership
recoverability
replaceable backends
small trusted computing base
testability
explicit enforcement state
```

If the implementation discovers that a requirement cannot be satisfied in safe Rust with the current executor design, stop at the safe boundary, document the limitation, and design a separate specialized executor rather than inserting an unreviewed unsafe escape hatch into the core runtime.
