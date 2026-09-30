---
id: ON-DEMAND-WORKER-BOOT
title: "On-demand boot: native bundle and worker runtimes stay cold until a job requires them"
status: proposed
date: 2026-09-30
tags: [jobs, workers, podman, native, boot, placement, security]
---

# On-demand boot for native bundles and worker runtimes

> **Decision direction.** Compiling or packaging a worker into a native Harw
> bundle does not mean starting it at Harw boot. Worker processes, Podman
> containers, remote builder runtimes and other execution placements remain
> cold until a concrete admitted job requires them. The job system owns the
> start/stop lifecycle. Runtime policy decides *when* a worker may boot; the
> agent IR only describes *what* the compiled agent requires.

This note is intentionally a plan. It does not claim that a generic boot
coordinator exists today.

## 1. Pinned baseline and scope

The implementation baseline for this note is the stacked P2 builder branch:

`feat/worker-builder-p2@e5aa9cc27a122a8e630bd744307e9684263fc2f3`

That branch adds the first concrete `harw-job-executor-podman`, the
`[builder]` configuration and `harw worker build|test|exec|status`.
The long-term container/placement references remain
`docs/planning/66-placement/README.md` and
`docs/planning/67-containers/{README.md,cell.md}`.

Scope:

- native compiled worker bundles,
- builder workers,
- rootless Podman workers / Zellhosts,
- remote worker targets,
- job-owned boot, drain, idle and stop transitions,
- node-state and cryptographic authority gates.

Out of scope:

- changing the agent IR into a live scheduler,
- auto-starting a rootful container engine,
- giving a container worker host-root authority,
- hidden fallback from a refused worker to host execution,
- treating boot as an agent/model side effect.

## 2. CURRENT

### 2.1 Native / child execution

Compiled child agents already have a job-managed process path:
`harw-agent-runner/src/job_child_backend.rs` uses
`JobManager::start_piped` in production, and cancellation goes through the
same job manager.

This is already close to the desired lifecycle rule: an agent process is
created for work, supervised as a job and stopped through job control. It is
not evidence that a generic lazy-boot resource registry exists.

### 2.2 Background job system

`harw-tool-job` already provides:

- background `job.start`,
- one shared `JobManager` per runtime assembly,
- ownership / ancestor control,
- durable job metadata and logs,
- reload into `Detached` or `Unknown` only when process identity can or
  cannot be verified,
- start / progress / finish notifications,
- configured `max_running_jobs`.

The job system is therefore the correct execution owner for boot processes.
It should not acquire builder-, Podman- or organization-specific policy.

### 2.3 P2 Podman worker

The P2 branch's `harw-job-executor-podman` currently translates one
`JobSpec` plus sandbox policy into an inspectable `podman run` command.

Current properties include:

- rootless-oriented `--userns=keep-id`,
- `--network=none`,
- RAM limit via `--memory`,
- workspace and configured bind mounts,
- optional Podman SSH remote,
- capability keep-lists refused,
- only `NetworkPolicy::Deny` admitted,
- `podman run --rm` for the concrete invocation.

This is naturally cold for an individual container run, but it is not yet a
general worker boot state machine. There is no `BootMode` type or
single-flight boot coordinator.

### 2.4 Container planning

The container plans already require rootless Podman by default, no Podman
socket inside a cell, digest-pinned images, no privileged containers and no
implicit rootful socket use.

Keep those rules. On-demand boot is an activation policy layered above them,
not a reason to relax them.

## 3. TARGET

### 3.1 Default: cold until required

The default worker boot policy is:

```text
Cold
  |
  | admitted job requires this worker/runtime
  v
Booting
  |
  | boot job succeeds + runtime identity verified
  v
Ready
  |
  | workload claims it
  v
Busy
  |
  | workload ends
  v
Idle
  |
  +---- idle_ttl == 0 ----------------------> Stopping -> Cold
  |
  +---- another admitted job before ttl ---> Busy
```

Failure and administrative transitions:

```text
Booting --failure---------------------------> Failed -> Cold
Ready/Busy/Idle --node drain---------------> Draining
Draining --no active workloads-------------> Stopping -> Cold
any non-terminal state --node revoke-------> Stopping -> Revoked
```

`Revoked` means the current node/runtime identity cannot be auto-booted
again until it is re-enrolled / replaced according to the owning subsystem.

### 3.2 Proposed runtime vocabulary

The first implementation may use an application-layer enum:

```rust
pub enum WorkerBootMode {
    OnDemand,
    AlwaysOn,
    Manual,
}
```

Default: `OnDemand`.

Semantics:

- `OnDemand`: no persistent worker process/container is started during
  normal Harw boot. The first admitted job starts it.
- `AlwaysOn`: explicit operator opt-in for a worker that should be started
  during the owning service's startup.
- `Manual`: jobs may use a ready worker but must never auto-start it.

This policy belongs in runtime/builder/placement configuration, not in the
portable agent rights manifest.

### 3.3 IR boundary

The IR answers:

```text
What does this agent need?
What targets/runtimes can satisfy it?
What is its authority ceiling?
```

The boot policy answers:

```text
When should a concrete runtime instance exist?
```

Therefore:

- IR / artifact may declare execution requirements;
- IR must not carry `booted=true`, live process identity or node lifecycle;
- native compilation does not itself start any worker;
- boot mode is not a way to widen the compiled permission manifest;
- placement/runtime config may only narrow what the compiled artifact can do.

### 3.4 Job-owned boot

A worker boot is an explicit supervised system job, not an invisible side
effect of a model tool call.

Conceptually:

```text
admitted workload
      |
      v
WorkerBootCoordinator (application policy)
      |
      | get-or-start single flight
      v
JobManager / job runtime
      |
      +-- boot job --------------------+
      |                                |
      v                                v
process / rootless container      durable logs/events
      |
      v
verified ReadyInstance
      |
      v
workload job
```

The generic job runtime owns process lifetime, logs, cancellation and
recovery. A higher application layer owns the meaning of "worker boot".

### 3.5 Single-flight boot

Two jobs concurrently requiring the same cold worker must not launch two
copies accidentally.

Define a stable `WorkerKey` from the resolved placement, for example:

```text
tenant
workspace
worker/runtime profile
node
image/binary digest
security profile
```

The boot coordinator permits one `Cold -> Booting` transition per
`WorkerKey`. Further jobs wait for that same boot result, subject to their
own deadline/cancellation.

No model-controlled string becomes a `WorkerKey` without validation.

### 3.6 Ephemeral containers versus persistent workers

Do not force one lifecycle onto both cases.

**Ephemeral Podman job**

The P2 `podman run --rm ...` model is already on-demand: the workload itself
is the container lifecycle. It needs no persistent `Ready` worker.

```text
Cold -> workload container -> exit -> Cold
```

**Persistent worker / Zellhost / remote runtime**

A runtime reused by more than one workload gets the explicit
`Booting -> Ready -> Busy/Idle -> Stopping` lifecycle.

This distinction prevents inventing an idle daemon merely to implement
"on-demand".

### 3.7 Idle policy

Default for high-assurance workers:

```text
idle_ttl = 0
```

No work means no persistent worker.

A non-zero idle TTL is an explicit optimization and never changes authority.
Warm reuse must remain keyed at least by tenant, immutable image/binary
digest and security profile. A worker must not cross those boundaries.

## 4. Node-state integration

Infrastructure node state is an admission gate for automatic boot:

| Node state | New auto-boot | Existing workload |
|---|---:|---|
| Pending | deny | none |
| Active | allow subject to policy | allow |
| Draining | deny | bounded completion only |
| Drained | deny | none |
| Revoked | deny | cancel/stop, fail closed |

A job must not "wake" a Draining, Drained or Revoked node by itself.

Re-placement, if implemented, is a placement decision using the same workload
requirements. It must not silently fall back to host execution.

## 5. Security and root boundary

On-demand boot must preserve the separate authority axes:

```text
organizational root
!= human Owner / gateway_admin
!= container uid 0
!= host execution
!= host root
```

A rootless Podman worker remains in the container execution domain even when
its process observes uid 0 inside a user namespace.

### 5.1 Host-root invariant

Automatic boot never bypasses the host-root rule:

> Only an eligible native executable with a valid cryptographic HostRoot
> attestation can enter the host-root authorization path.

Even then it is only *eligible*. Current node state, security context,
runtime policy and the concrete privileged operation must still be admitted.

Portable artifacts, a generic runner with an appended artifact, ordinary
AgentBuild signatures and container-root identity are structurally
ineligible for host root.

### 5.2 Attestation is checked before privileged boot

For a root-eligible runtime the boot coordinator must verify, before any
privileged start:

```text
IR snapshot digest
artifact digest
final executable digest
target
authority ceiling
attestation purpose
node binding / policy epoch when required
```

The private trust-root key remains outside the executable.

### 5.3 Podman safety

On-demand Podman boot keeps:

- rootless engine as default,
- no privileged container,
- no capability widening,
- no engine socket mounted into a cell,
- immutable image digest once image-backed execution lands,
- no rootful Podman/Docker socket unless a separate explicit root-authority
  path admits it.

A Podman worker cannot become host root merely because it is native or signed.

## 6. Configuration direction

For the P2 builder surface, the additive shape may be:

```toml
[builder]
template = "rootless-podman"
boot = "on-demand"

[builder.runtime]
idle_ttl = "0s"
```

Exact placement is not binding yet. Before implementation, reconcile it with
the placement/container configuration so the repository does not gain two
independent worker boot policies.

Rules:

- missing `boot` resolves to `on-demand`;
- `always-on` is operator configuration only;
- project/untrusted layers cannot widen `manual` or `on-demand` into
  `always-on`;
- `idle_ttl` is bounded and cannot create cross-tenant warm reuse.

## 7. DELTA / implementation waves

### W1 — vocabulary and config

- introduce `WorkerBootMode` in the owning application/config layer;
- add validated config with `OnDemand` default;
- define `WorkerKey`, `WorkerState` and read-only status projection;
- do not change generic `harw-job-core` semantics for builder policy.

### W2 — boot coordinator over jobs

- add one application-layer `WorkerBootCoordinator`;
- start boot processes through the existing job system;
- persist enough runtime identity for safe recovery;
- route boot/start/stop events through existing job notifications;
- implement single-flight start and cancellation-safe waiters.

### W3 — P2 Podman integration

- keep one-shot `podman run` as the minimal cold path;
- add persistent ReadyInstance only where reuse is actually required;
- no hidden host fallback on Podman/remote failure;
- preserve rootless/network/capability constraints.

### W4 — node and crypto gates

- bind worker placement to node state;
- deny boot on Pending/Draining/Drained/Revoked as specified above;
- add native-attestation verification before any host-root-capable start;
- ensure container/rootless attestations cannot satisfy HostRoot purpose.

### W5 — drain, idle and observability

- idle stop with zero default TTL;
- explicit drain behavior;
- metrics/events for cold start, shared boot, boot failure and idle stop;
- status surfaces show `cold|booting|ready|busy|idle|draining|stopping|failed`
  without inventing a second source of truth for job process state.

## 8. Verification

Required tests:

1. Harw starts with an OnDemand worker configured and no worker process or
   container is started.
2. The first admitted workload starts exactly one worker/runtime.
3. Two concurrent first workloads share one boot attempt.
4. A boot failure fails closed and does not fall back to host execution.
5. `idle_ttl=0` returns the worker to Cold after the final workload.
6. Draining refuses new boot while permitting only already-admitted bounded
   completion.
7. Revoked causes boot denial and shutdown/cancel of the affected runtime.
8. An unsigned or portable artifact can never satisfy HostRoot eligibility.
9. An AgentBuild-signed native worker without HostRoot purpose cannot satisfy
   HostRoot eligibility.
10. A rootless Podman/container-root worker cannot call the host-root path.
11. Cancellation of all waiters does not orphan a boot job.
12. Restart recovery never trusts PID/container name alone; persisted runtime
    identity must be re-verified.

Failure injection:

- Podman executable missing,
- remote Podman endpoint unreachable,
- node changes Active -> Draining during boot,
- node changes Active -> Revoked during workload,
- boot process exits before readiness,
- readiness succeeds but workload admission is cancelled,
- stale cryptographic policy epoch / wrong node binding.

## 9. Acceptance criteria

This plan is implemented only when all are true:

- the default native bundle starts no unused worker runtime;
- job demand is the trigger for OnDemand activation;
- boot is supervised by the job system;
- generic job crates remain free of Harwness-specific builder policy;
- concurrent demand cannot stampede worker boot;
- node state gates new boot;
- no automatic path widens to host execution;
- host-root remains impossible without native cryptographic eligibility plus
  live runtime authorization;
- Podman/rootless workers stay separate from host-root authority;
- tests prove cold boot, recovery, drain, revoke and negative root cases.

## 10. Implementation status

**CURRENT on the pinned P2 branch**

- background job supervision: implemented;
- job-managed compiled child processes: implemented;
- initial rootless Podman builder executor: implemented on the stacked P2
  branch;
- worker CLI surface: implemented on the stacked P2 branch.

**NOT IMPLEMENTED by this planning PR**

- `WorkerBootMode`;
- `WorkerBootCoordinator`;
- persistent worker ReadyInstance registry;
- automatic boot single-flight;
- node-state boot admission wiring;
- native HostRoot build attestation;
- cryptographic root eligibility enforcement.

This PR is deliberately documentation-only so review can settle the lifecycle
and authority boundaries before code grows around them.
