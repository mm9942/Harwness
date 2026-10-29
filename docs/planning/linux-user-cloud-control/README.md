# Linux User Workload and Cloud Control

**Status: architecture / integration DRAFT. No product runtime is changed by this dossier.**

**Owner intent:** make Harw account for and govern user-associated work across Linux hosts, including work not started by an agent, while preserving a functioning local machine when Harw, a model, or the cloud is unavailable. Keep kernel tasks and essential system infrastructure outside automatic ownership. Use the existing job, authority, DoD, session and cloud foundations rather than inventing another scheduler for every integration.

**Review date:** 2026-10-16. **Repository:** `mm9942/Harwness`.

**Cloud integration refresh (2026-10-29):**
[harw-cloud delivery and integration plan](07-harw-cloud-integration.md) pins
`dev@fcb12b41`, reconciles restored cloud code and the #153/#154 dependencies,
and defines incremental acceptance gates. Its refreshed cloud observations are
current at that ref; the frozen evidence below remains historical.

## 1. Frozen evidence and limits

| Label | Exact source | Meaning |
|---|---|---|
| B | `dev@6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a` | Base of this new PR; merge of #142, 2026-10-09 |
| M | `main@90160f4f2b88f80c6721c20469ed8322ddc8861a` | Release-branch comparison, not the implementation base |
| N | `c7ff325fa7f60a95e1f6f4e3ed3536045285460d` | Shared, unmerged head of #146 and #147; 16 commits ahead of B |
| P145 | `6c228e017ac74e2420b043559bb2869df4deebb0` | #145, based on dev; N is one commit ahead |
| A143 | `db8c07116a167a121ff9ccfc3f3bf1e3f7f35a93` | Previous capability-closure audit #143; its own baseline is older than B |

The review inspected source files, manifests, PR descriptions, selected comparisons and metadata through GitHub. It inventoried the 19 open PRs visible at the cutoff and the closed PRs returned by the `base:dev` query. It did **not** independently execute every historical patch, read every review thread, or prove all branches equivalent. The PR ledger states the disposition and evidence level instead of equating `closed`, `merged`, `implemented` and `tested`.

No Cargo workspace build, privileged daemon, BPF program, container workload, Cloudflare deployment or live Pi benchmark was run in this review. Container-side GitHub cloning was unavailable; no untracked substitute manifests were invented to get a build past the missing cloud members. An empty commit-status response is not a passing Actions run. Source tests and historical test reports are evidence of different things.

The operator reports better Pi responsiveness under simultaneous builds and a browser, and says Warden is enabled on that host. These are useful field observations, not proof that a particular code path caused the improvement. Repository defaults do not override the operator's reported configuration, and neither tells us which actions actually executed.

## 2. Reading order

Current cloud code delivery: [implementation ledger](08-cloud-implementation-status.md).
The historical evidence below remains pinned to its original audit.

1. [Evidence and gap register](01-evidence-and-gaps.md): bounded claims, source anchors, priorities, closure conditions.
2. [PR reconciliation](02-pr-reconciliation.md): open work, absorbed work, duplicate heads and integration order.
3. [Host/process/job contract](03-host-process-contract.md): complete-enough inventory, identity, user scope, ownership and resource control.
4. [Cloud control contract](04-cloud-control-contract.md): enrollment, sessions, directory, placement, execution, artifacts and offline operation.
5. [Rust integration map](05-rust-integration.md): proposed types, ports, producer/consumer paths and file responsibilities.
6. [Verification and rollout](06-verification-and-rollout.md): negative tests, fault injection, benchmarks, release gates and rollback.
7. [Implementation plan](../../superpowers/plans/2026-10-16-linux-user-cloud-control.md): independently reviewable work packages with explicit outputs and tests.
8. [Machine-readable audit manifest](evidence-manifest.json): frozen refs, coverage and claims not made.

This extends #143 rather than replacing its broader tool/context/security audit. It connects PL-65/66/67, the two different PL-93 compartments, async-job PL-94, cloud-onboarding PL-94, and the session/UI work. Numeric plan names already collide; this dossier uses a descriptive directory and stable `LUC-*` finding IDs.

## 3. Decision in one paragraph

Build **one locally authoritative workload inventory and control boundary per host**, above the kernel and alongside the existing systemd managers. Correlate native jobs, user services, external applications and containers without pretending they share the same lifecycle owner. Let an authenticated cloud directory route intentions to the correct owner. Keep job admission, execution leases, resource enforcement, privilege checks and evidence validation deterministic. Models explain, propose and coordinate; they do not mint host authority, assert a successful effect, or become a synchronous dependency of every `fork` or `exec`.

## 4. What is reused, not rewritten

The existing `harw-command -> harw-job -> Coordinator -> Executor` path remains the execution mechanism for Harw-owned commands. `WorkId` remains the durable identity of admitted work; a PID is not a WorkId. `SessionHost` remains the writer of its sessions; WebSockets remain transport, not authority or persistence. DoD probes keep their narrow observation role; Warden remains a constrained actuator, not a general scheduler or an LLM host. `harw-netsec` remains the authority for node lifecycle. AuthHub and the existing device registry remain the identity roots.

Those are reuse decisions, not a claim that all connections already exist. In B, some of the mechanisms are callable libraries, some are mounted production paths, some have incomplete recovery, and some are plans. In N, the missing cloud files reappear, but several are still placeholders or incompatible adapters. The gap register makes that distinction explicit.

## 5. Target ownership topology

| Responsibility | Canonical owner | Must not be inferred from |
|---|---|---|
| Process existence / scheduling | Linux kernel | Model text or a stale UI row |
| A systemd unit's lifecycle and cgroup subtree | Its systemd manager, with explicit delegation where supported | Harw seeing the unit's PID |
| A Harw job's durable state | Job store / lease-fenced coordinator | JobManager display state or an agent saying 'done' |
| Host inventory | One node-local inventory service | Bounded `sys.ps` output treated as a complete database |
| Right to control an external workload | Fresh, scoped authorization checked at the host | Same UID, discovered name, cloud routing address |
| Session execution / transcript | Its owning SessionHost | The currently attached TUI or an edge proxy |
| Session owner generation | Durable directory authority | A reconstructible routing cache or host-local epoch |
| Provider capacity | Existing provider budget owner and actual provider limits | Number of cloud gateways or local CPU cores |
| Live model reasoning | Admitted agent runtime | Kernel probes or privileged service code |

## 6. Scope and non-goals

The first operational scope is an explicitly enrolled human user's applications, terminal work, user services and rootless containers on selected Linux nodes. It includes already-running applications and processes launched outside Harw. Observation is broader than control; both are broader than lifecycle ownership.

The following are not part of the first implementation: replacing PID 1, implementing a CPU scheduler, moving every existing process into a Harw cgroup, automatically killing arbitrary user applications, live-migrating Firefox, automatic VM purchases, silent cross-node session failover, or a custom kernel module. A missing kernel capability can become a separately justified extension after a measured interface gap, not merely to make integration feel deeper.

The design must preserve the repository's Full Access semantics: confirmation mode and authority are separate. In `ask`/`auto`, existing always-ask rules remain. In `full`, an explicit deny, scope restriction, stale identity or missing enforced capability still refuses the action; this design does not add compulsory dialogs to that mode. Existing Warden-specific proof/policy requirements must also be reconciled before adding new actuators.

## 7. Acceptance outcomes

A first usable release should demonstrate all of these:

- Starting Harw after a browser, terminal and user service are already running produces an inventory with explicit coverage and owner classification. Restarting Harw does not manufacture new jobs or restart these applications.
- An admitted Harw command has one durable job identity and observable attempts. Its subprocesses are correlated without being counted as independent admitted jobs.
- Two Harw instances cannot collectively exceed the same host reservation merely because each has its own semaphore.
- An operator can ask why a host is under pressure, inspect evidence and apply an allowed reversible action. No model reply is accepted as proof of improvement.
- A paired remote device can continue an authorized hosted session through the correct owner without widening rights or duplicating execution.
- A declared remote build either executes on the verified backend with the requested restrictions and produces verified artifacts, or fails visibly. It never falls back to an unrestricted local shell.
- Loss of cloud connectivity or model access leaves local applications, emergency administration and deterministic host supervision usable.

## 8. Evidence index

These links pin source, not deployed state. `B` links refer to the base, `N` links to the unmerged shared head. Each gap cites the relevant identifier below.

### Base source anchors

- **B01:** [workspace cloud member declarations](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/Cargo.toml#L137-L141). Fetching `harw-cloud/Cargo.toml` at this SHA returned 404; the recursive tree also lacks the source subtree.
- **B02:** [job facade](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-job/README.md), [host command port](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-command/src/port.rs), [Linux executor](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-job-runtime/src/coordinator/linux.rs).
- **B03:** [session job wiring](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-runtime/src/job_wiring.rs), [JobManager](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-tool-job/src/manager.rs), [lanes](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-job-runtime/src/lanes.rs), [permits](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-job-runtime/src/permits.rs).
- **B04:** [process ABI](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/dod/crates/harw-dod-bpf/src/abi.rs), [BPF event conversion](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/dod/crates/harw-probe-bpf/src/collect.rs), [scope model](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/dod/crates/harw-dod-bpf/src/profile.rs).
- **B05:** [bounded system tools](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-tool-sys/src/lib.rs), [ps implementation](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-tool-sys/src/ps.rs), [process identity](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-killer/src/process.rs).
- **B06:** [local session composition](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-cli/src/session_serve.rs), [daemon composition](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-session-daemon/src/compose.rs), [SessionHost](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-session-host/src/host.rs).
- **B07:** [remote composition](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-cli/src/session_serve_remote.rs), [listener configuration and AuthHub signer](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-cli/src/session_listener.rs), [canonical device registry](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-node-listener/src/identity.rs).
- **B08:** [placement minimum slice](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-placement-model/src/lib.rs), [OCI executor](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-job-executor-oci/src/lib.rs), [Kubernetes executor](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-job-executor-k8s/src/lib.rs).
- **B09:** [Cloud Home target](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/docs/planning/65-cloud-sessions/CLOUD-HOME-HUB-PROFILES-V2.md), [on-demand worker target](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/docs/planning/67-containers/on-demand-worker-boot.md), [onboarding plan](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/docs/planning/94-cloud-onboarding/README.md).
- **B10:** [proxy policy core](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-reverse-proxy/src/lib.rs), [tunnel implementation boundary](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-tool-tunnel/src/lib.rs).
- **B11:** [async job integration and remaining recovery delta](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/docs/planning/94-async-first-job-runtime/README.md), [worker supported kinds](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-cli/src/job_worker.rs).
- **B12:** [mias-lab implementation/rollout contract](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/infra/cloudflare/mias-lab/README.md), [Kimi routing](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/infra/cloudflare/kimi/README.md).
- **B13:** [project contribution rules](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/CONTRIBUTING.md), [security and Full Access policy](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/SECURITY.md), [architecture gate](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/xtask/arch-policy.toml).

### Latest unmerged source anchors

- **N01:** [cloud enrollment types](https://github.com/mm9942/Harwness/blob/c7ff325fa7f60a95e1f6f4e3ed3536045285460d/harw-cloud/harw-cloud-enroll/src/lib.rs).
- **N02:** [new YAML gateway configuration](https://github.com/mm9942/Harwness/blob/c7ff325fa7f60a95e1f6f4e3ed3536045285460d/harw-cloud/harw-cloud-gateway/src/config.rs), [optional dependency manifest](https://github.com/mm9942/Harwness/blob/c7ff325fa7f60a95e1f6f4e3ed3536045285460d/harw-cloud/harw-cloud-gateway/Cargo.toml).
- **N03:** [proxy placeholder](https://github.com/mm9942/Harwness/blob/c7ff325fa7f60a95e1f6f4e3ed3536045285460d/harw-cloud/harw-cloud-gateway/src/proxy.rs), [TCP-only status probe](https://github.com/mm9942/Harwness/blob/c7ff325fa7f60a95e1f6f4e3ed3536045285460d/harw-cloud/harw-cloud-gateway/src/status.rs).
- **N04:** [new enrollment listing adapter](https://github.com/mm9942/Harwness/blob/c7ff325fa7f60a95e1f6f4e3ed3536045285460d/harw-cloud/harw-cloud-ops/src/enrollments.rs).
- **N05:** [cloud control operations](https://github.com/mm9942/Harwness/blob/c7ff325fa7f60a95e1f6f4e3ed3536045285460d/harw-cloud/harw-cloud-ops/src/cloudctl.rs), [cloudctl binary dispatch](https://github.com/mm9942/Harwness/blob/c7ff325fa7f60a95e1f6f4e3ed3536045285460d/harw-cloud/harw-cloudctl/src/main.rs).
- **N06:** [cloud registry helper](https://github.com/mm9942/Harwness/blob/c7ff325fa7f60a95e1f6f4e3ed3536045285460d/harw-cloud/harw-cloud-ops/src/lib.rs), [job-backed container command adapter](https://github.com/mm9942/Harwness/blob/c7ff325fa7f60a95e1f6f4e3ed3536045285460d/harw-tool-container-run/src/engine.rs).
- **A01:** [prior capability-closure evidence register](https://github.com/mm9942/Harwness/blob/db8c07116a167a121ff9ccfc3f3bf1e3f7f35a93/docs/planning/96-capability-closure/01-evidence-register.md). Prior findings remain attributed to that audit until re-tested on the chosen implementation head.

### External mechanism references

- **X01:** [systemd cgroup delegation](https://systemd.io/CGROUP_DELEGATION/): ownership and delegation boundaries.
- **X02:** [systemd control-group interface](https://systemd.io/CONTROL_GROUP_INTERFACE/): manager-mediated resource control and transient units; feature-probe the target version.
- **X03:** [Linux cgroup v2](https://docs.kernel.org/admin-guide/cgroup-v2.html): controller and migration semantics.
- **X04:** [Linux PSI](https://docs.kernel.org/accounting/psi.html): resource-stall measurements.
- **X05:** [pidfd_open](https://man7.org/linux/man-pages/man2/pidfd_open.2.html): stable live handles and the distinction between readiness and reaping.
- **X06:** [BPF ring buffer](https://docs.kernel.org/6.2/bpf/ringbuf.html): bounded transport and loss handling. This is a versioned mechanism reference, not a minimum supported kernel declaration.

External documents describe mechanisms, not evidence that Harw currently wires them. New compatibility floors must come from a host capability report and a tested release matrix, not the version of a page read during design.
