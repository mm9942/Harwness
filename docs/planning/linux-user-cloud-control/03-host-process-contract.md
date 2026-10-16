# Host, process and job contract

**TARGET contract; not implemented by this documentation PR.** Existing anchors B02-B05 and mechanism references X01-X06 are linked in [README](README.md#8-evidence-index). This contract closes LUC-14 through LUC-26 and supplies the local authority boundary used by the cloud contract.

## 1. Four objects, not one universal 'job'

A **process instance** is one kernel process incarnation. A **task** may be a thread of that process. A **workload** groups related activity according to an authoritative owner such as a systemd unit, container or Harw job. A **job** is admitted work with a durable lifecycle, budget and execution attempts.

These are related but not interchangeable. A browser can have many processes but no Harw job. A build job can create many compiler processes. A long-running agent job may spend time awaiting remote inference without a dedicated OS process per model turn. A single PID can execute several programs during its lifetime. A shell builtin may change shell state without creating another process.

The inventory therefore never creates `StoredJob` records for every `fork`, `exec`, thread or discovered application. It records facts and correlations. A system-management action concerning an external application can itself be a Harw job, but the target does not thereby become its owned child.

## 2. User scope is explicit and locally resolved

Enrollment produces a trusted user-scope configuration for each host. Its minimum inputs are the selected local account identity, host UID mapping, permitted login/user-manager domains and any explicit rootless-container mappings. No hard-coded UID threshold defines a human user.

Track real/effective/saved identity where available, login origin as an attributed observation, user namespace mapping, cgroup and owning unit. A user-launched privileged helper is not automatically an ordinary user-owned process. Unknown mappings remain `Unresolved`, not 'the default user'. Remote tenant IDs are not Linux UIDs; the node resolves a configured binding after authentication.

Separate three sets:

- **Observation set:** user-associated facts that this collector may read.
- **Control set:** actions explicitly granted for selected workload instances.
- **Protected set:** workloads that cannot receive automatic disruptive actions, even if they share the user's UID.

The protected set initially includes the emergency administration path, Harw's independent recovery services, the user service manager, desktop/compositor, session bus, and other operator-declared session-critical services. Protection is attached to trusted unit/process identities, not names alone. A display label is never an authorization selector. Other users, kernel workers and system services remain outside automatic management unless explicitly added through a separate policy.

## 3. Origin and control are orthogonal

The proposed registry stores a lifecycle-owner classification:

| Owner | What Harw may truthfully claim | Default action boundary |
|---|---|---|
| `HarwJob` | It admitted the job and can correlate attempts/processes. | Existing job cancellation, deadline, recovery and configured limits. |
| `UserUnit` | A particular systemd user manager owns the unit. | Manager API, constrained by an action grant; no parallel unit supervisor. |
| `Container` | A verified engine instance owns the container workload. | Engine adapter with immutable container identity and read-back. |
| `External` | It observed a process started elsewhere. | Read-only until an explicit control grant is issued. |
| `Unresolved` | Evidence is insufficient to establish ownership. | Observe and reconcile only. |

`Observed`, `Controlled` and `Owned` are useful UI descriptions, but should not be a single monotonic privilege enum. A grant can expire while the observation remains. Lifecycle ownership can stay external while a narrowly scoped signal is allowed. A user-unit restart operation does not transfer that unit to JobManager.

Use an independent `ControlGrant` scoped to principal, host, user binding, exact target, allowed actions, policy revision and expiry. Its authoritative in-memory form has private fields and can be minted only by the trusted admission path. Deserializing an action request or seeing a registry record does not construct that type.

## 4. Identity across time, threads and machines

A durable observation locator contains node identity, host boot identity, PID namespace identity, TGID and a process-birth witness. A thread record additionally carries TID and its own birth witness. Include collector epoch and source cursor on observations, not as substitutes for process identity.

The existing BPF v1 record lacks part of this identity. It must be treated as weaker evidence. Do not assign an old event to whatever `/proc/<pid>` happens to contain when processing catches up. Where a hook cannot provide a usable birth witness, mark that association uncertain and reconcile it before control.

For a live disruptive action, open and retain the appropriate kernel handle, verify the current instance against the admitted locator, check current scope/policy, and use that same handle for execution. Do not persist a pidfd integer; it is a process-local descriptor. Do not assume a PID plus a coarse timestamp is globally collision-proof. Repeated checks around acquisition and an explicit unverifiable state are required for weaker kernels or metadata.

Opening a pidfd for an external process does not make Harw its parent. Exit readiness is not a promise that Harw can reap it or recover a child exit status. Record `exit_observed` and a source-qualified status only when available, rather than inventing a return code. This distinction follows X05 and is important for both recovery and the UI.

Thread-group semantics must be deliberate. A task exit is not automatically the end of the whole application. Test leader exit while another thread remains. Successful exec updates the program image of an existing process; it does not inherently create a second process instance. Non-leader-thread exec and resulting identity changes require a supported-kernel test or a conservative reconciliation state.

## 5. Discovery: snapshot plus event stream plus reconciliation

A reliable inventory combines three mechanisms:

1. **Bootstrap discovery:** enumerate the authorized current process/unit/container population, including workloads older than Harw.
2. **Incremental observations:** creation, exec, task/process exit and ownership/scope changes where supported.
3. **Reconciliation:** compare fresh bounded-in-memory scans with the current materialized view after loss, restart, permission change, scope change and on a periodic schedule.

Subscribe before taking the initial scan, record a source watermark, collect a scan generation, and merge events by validated instance identity. `/proc` is not an atomic snapshot; an entry can change or disappear between reads. A scan's completion therefore means 'this generation was reconciled within the stated coverage', not 'nothing was missed anywhere in time'. A second reconciliation closes bootstrap races that cannot be resolved from available birth witnesses.

A dropped event, source restart, sequence gap, unknown schema or collector budget overrun changes coverage to `Degraded` or `ResyncRequired`. It never silently clears records or reports a complete idle machine. Loss counters are first-class facts, not merely logs.

Do not assume the BPF sequence is global across independently loaded programs/maps because a comment calls it global. The source key is `(boot, collector_epoch, program_id, sequence)` until a unified sequence allocator is proven. Ordering across sources uses explicit causality and observed times with uncertainty; timestamps alone do not establish total causal order.

The registry's scan API is not the model tool API. A `sys.ps` result capped at 2,000 rows is a presentation projection. Internal collection must paginate/stream and publish explicit capacity exhaustion when its own configured inventory budget is exceeded. It must never infer process exit from a row omitted by display truncation.

## 6. BPF and transport changes

The next wire contract needs versioned process creation, exec, task exit and coverage/loss information, including the identity facts required above. Retain cgroup attribution before converting events into the narrower SecurityEvent model. Derive security findings and inventory facts from the same accepted observations; do not reconstruct a process registry from security findings alone.

Maintain exact validation for schema length, flags, reserved bytes and event kinds. Do not repurpose v1 reserved bytes without a version change. The privileged probe remains push-only: adding lifecycle delivery must not introduce a command receiver or a runtime/LLM dependency into that process. Pin the installed sender/receiver schema through the package/config contract; cloud protocol negotiation is not a reason to add bidirectional control to the local probe.

Dynamic cgroup scope needs its own revision protocol. A map populated with descendant IDs once at attach cannot guarantee future descendants are covered. Choose either a supported ancestor-aware kernel filter or an atomically revised exact-ID map fed by the trusted scope resolver. If changes cannot be applied safely, report a scope-coverage gap and reconcile after the ordered replacement. Never broaden from a missing configured scope to host-wide capture.

Kernel-to-userspace buffers and userspace queues remain bounded. Per-program counters should distinguish emitted, received, decoded, rejected, lost and applied records. Security-event sampling must not silently sample away lifecycle transitions used as inventory truth. Under sustained overload, degrade explicitly, shed nonessential detail and rescan rather than allocating without bound. X06 describes why transport capacity is finite.

## 7. Local inventory layout and retention

Use one host-local inventory authority, not a registry per TUI or per agent. The durable layout separates:

- checkpointed current observations and coverage generation;
- a bounded append journal of accepted lifecycle changes;
- bounded terminal tombstones for correlating delayed events;
- management-action records and read-back evidence;
- user/tenant-scoped projections served to clients.

Reuse existing atomic-file/record-store conventions where layering permits; do not add a new database merely for this plan. Reads must not block the collector on a slow model or remote client. Live subscribers receive bounded queues and cursor/resync behavior. A global journal sequence is issued by this one inventory owner after merge; it is distinct from each probe's sequence and from a session transcript cursor.

No raw argv, full environment, user document contents, private browser URLs or hidden model reasoning is part of the default record. Add sensitive fields only through a separately authorized, time-bounded inspection. Redaction runs before persistence/export where appropriate, and before any model prompt. Process names and logs are untrusted data even when produced by a legitimate application.

Retention keeps enough identity tombstones to avoid attaching delayed events to a reused PID. A purge records the coverage boundary. It does not erase a still-needed management-effect journal. Cloud export is an allowlisted, scope-filtered projection, not a copy of the local registry directory.

## 8. Job correlation and command coverage

Keep the existing workload command port and introduce an explicit correlation seam:

`admitted WorkId -> AttemptId -> launch record -> process instance/cgroup -> inventory projection`.

The executor supplies the launch identity directly. The collector may corroborate it but cannot manufacture job ownership from a matching command name. A child can be represented in both agent and OS views without counting its CPU or completion twice. Parent/child relationships are evidence-qualified; reparenting or engine indirection can invalidate naive PPID-tree assumptions.

Command coverage has three categories:

- **Native read operations:** structured internal Rust queries, ideally against registry/snapshot ports; no extra `ps`, `top` or grep subprocesses.
- **Executable work:** a CommandRequest submitted through the common job runtime, with environment policy, cwd, timeout, output limits, sandbox and resources.
- **Shell/session state:** pipelines, redirection, builtins, terminal job control and interactive programs require an explicitly owned shell/session adapter. `cd` and `export` cannot be represented faithfully by unrelated child processes.

Do not transparently rewrite every existing desktop launcher in the first release. External applications can first be observed and controlled through their native manager. Opt-in launch integration can create tracked units/jobs for new commands. A guarantee that *every launch is admitted before any effect* would be a stronger enforcement product than host observation; it is not claimed here.

## 9. systemd and existing user applications

On systemd hosts, query unit membership and use the appropriate system or user manager for unit actions. A dedicated service port resolves unit identity, user manager, supported properties and invocation identity before asking the manager to change anything. Where necessary use transient scopes or services for newly admitted work, without pretending Harw becomes parent of arbitrary existing processes.

Respect the single-writer ownership convention from X01. Direct writes belong only under an explicitly delegated Harw subtree. Do not add `Delegate=` to arbitrary slices or move an existing application out of its manager's tree just because Harw discovered it. X03 documents that moving a parent does not recursively move existing descendants and does not simply migrate all prior stateful resource charges.

When systemd is absent or a controller is unavailable, report the actual capability. Read-only procfs inventory can still work. Required resource/ownership guarantees refuse managed launch; an explicitly chosen reduced mode advertises exactly what it lacks. No silent privileged fallback.

## 10. Node-wide capacity and local pressure policy

The current per-manager reservation fix and per-worker lanes should stay, but they cannot enforce a shared host ceiling across independent Harw processes. Add a node-local admission authority with atomic reservations for active and starting attempts. All configured Harw entry points consume it. Reserved capacity is not released merely because the submitting TUI disconnects.

A reservation includes the job/attempt owner, scope, requested resource dimensions, expiry/lease and policy revision. Crash reconciliation distinguishes a reservation with no launch, a launch with an uncertain effect, a live supervised attempt and terminal work. Do not start a replacement until the effect state permits it.

Admission should budget headroom for external interactive workloads without claiming to reserve all of their future memory. Resource observations are fresh inputs, not hard promises. Use PSI, memory/cgroup counters and queue latency together; low average CPU alone is not a success criterion. The node governor has deterministic hysteresis, minimum action spacing and a bounded action rate, so it does not oscillate on every spike.

First allowed automatic actions should be limited to Harw-owned background work: defer new admissions, reduce available permits as current jobs finish, adjust supported relative weights inside the delegated subtree, and cancel only under the job's existing policy. Freezing a memory-heavy process does not free its resident memory; it is not a general memory-pressure remedy. Cgroup `memory.high`, `memory.max`, CPU weight, CPU quota and RLIMIT_AS have different semantics; use X03, read-back and representative builds to choose a policy rather than mapping every field to 'RAM limit'.

The model can propose a diagnosis and a policy-bounded action. A fast local deterministic loop handles immediate limits even when model latency is high. No model call gates kernel scheduling. Every applied change records before-state, authorization, requested change, result, read-back and any rollback, so Harw can answer 'what did you change?' with evidence.

## 11. Safe failure contract

Stopping Harw must not kill all observed applications, steal units, leave frozen groups without a recovery path or block user login. The local recovery/administration path must be operable independently of the agent, cloud session and model provider. A Warden outage means controlled actions are unavailable; it does not promote a shell into an unrestricted substitute.

A node may continue previously admitted bounded own jobs under its local policy during a cloud partition. New remote requests with expired authority or stale placement are refused. External applications remain external. Recovery must report an uncertain effect instead of claiming success or retrying an operation that may already have happened.

All of this is tested with the host cases H01-H12 in the verification document before enabling external-process mutation.
