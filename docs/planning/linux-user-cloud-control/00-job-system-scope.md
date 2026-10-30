# Start here: existing jobs, cloud oversight, process management

**Owner clarification, 2026-10-16. Status: design priority, not a runtime implementation.**

Mia's priority is the job system already developed and discussed, the cloud infrastructure needed to oversee it across machines, and the process-management layer beneath it. This program must extend those three connected responsibilities, not turn into three replacement platforms or a general operating-system rewrite.

Read this before the [dossier](README.md), [gap register](01-evidence-and-gaps.md) and [implementation plan](../../superpowers/plans/2026-10-16-linux-user-cloud-control.md). It governs scope and interpretation of the proposed work packages; the existing security, authority and lifecycle contracts remain binding. Earlier code-status claims derived from planning documents require source-level rechecking, as the recovery example below demonstrates.

## 1. Priority and ownership

| Priority | Responsibility | Required direction |
|---|---|---|
| Core | Existing Harw job system | Preserve admitted work, identities, ownership, leases/fencing, budgets, cancellation, results and recovery. Complete the integration rather than invent another scheduler. |
| Cross-host extension | Cloud infrastructure and overview | Show where work lives, who owns it, what is running/waiting/failed, which resources are available and which observations are stale. Route authorized intentions to the existing owner. |
| Local execution and observation | Process-management layer | Relate jobs to their actual processes, user services and containers; also account for enrolled user workloads that Harw did not launch. Enforce only the control authority actually granted. |

DoD/eBPF supplies observations, Warden supplies its permitted effects, and clients supply views and intentions. None becomes a competing owner of durable job state. Cloudflare/provider routing is supporting inference infrastructure, not the primary scope of this program.

## 2. Preserve the existing job contracts

The base remains `dev@6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a`. Concrete starting points are the [job facade](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-job/README.md), [command port](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-command/src/port.rs), [coordinator](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-job-runtime/src/coordinator/mod.rs), [session job wiring](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-runtime/src/job_wiring.rs) and [durable agent-job adapter](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-runtime/src/agent_job_wiring.rs).

Keep the distinction between logical work, child work, attempts and process-manager identifiers. Preserve their existing types and validated transitions, and explicitly map their relationships. Do not merge different state machines into an invented universal enum, or make every process/thread a durable job. One logical work item may legitimately have several child jobs and attempts; the view must expose that lineage without counting each projection as new work.

For Harw-launched commands, retain `CommandPort -> harw-job -> Coordinator -> Executor`. For logical agents, retain the existing admitted-agent and durable-job adapter. For shared host capacity, reuse these admission and execution paths; add the missing cross-process reservation boundary rather than another independent execution queue.

A cloud hop must not replace a job ID or create another run just to display it. Qualify references by their owning node/job-store authority and existing scope: a bare WorkId is not assumed globally unique across unrelated stores. If an operation intentionally creates child work or an execution job, persist that parent relationship. If ownership transfer is not supported, route to the current owner rather than fabricating migration.

### Source correction: do not create a second recovery worker

A fresh read of `agent_job_wiring.rs` at the exact base SHA finds `RuntimeAgentJobSubmitter::start_recovery_worker` and `recovery_pass`. The latter calls expired-lease reconciliation, terminal-child-lease cleanup and stale-Pending cleanup, then processes eligible Ready agent records and checks child ownership. See [the source path](https://github.com/mm9942/Harwness/blob/6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a/harw-runtime/src/agent_job_wiring.rs#L88-L169).

Consequently, the earlier blanket interpretation that an agent rehydrator is absent must not drive implementation. Read LUC-24 and W07 as **audit the existing worker's mounted callers, recovery semantics and crash tests, then repair demonstrated gaps**. Source availability does not prove that every runtime entry starts it or that it passes the desired tests; those questions remain to be checked. The old async-job plan's remaining-delta text is not sufficient evidence to add another worker. No recovery test was executed in this documentation update.

## 3. Cloud overview is operational, not merely a connection status

The minimum cloud-facing view joins existing authoritative records and node-local observations. It should expose:

- Node, owner, scope, logical job/child/attempt relationships, backend and correlated process/unit/container identity.
- The actual job state, last transition, timestamps, queue/admission reason, cancellation/approval/dependency wait where supported, declared budget and measured resource use.
- Bounded logs, result/artifact references, actual enforcement report, action/read-back evidence, freshness, coverage and last successful synchronization.

These are requirements for a projection, not a new job store. Read APIs are paginated and cursor-based; a slow client cannot block execution. Offline, stale, unknown, unreachable and failed remain different states. A disconnected node does not automatically make its jobs failed, and an absent row in a truncated response does not mean a job ended.

Control requests return through the authenticated job owner and node-local authority. The cloud does not issue an unqualified PID kill or write a remote cgroup path directly. Device access, session access and permission to control a workload remain separate. Pairing, profile mounts, owner-directory correctness and revocation are in scope because they make this overview/control usable; unrelated provisioning and proxy expansion are not milestones by themselves.

## 4. User processes are first-class inventory, not fictitious owned jobs

The intended coverage includes enrolled users' browser processes, terminal work, user services and rootless containers, including those already running when Harw starts. It is not limited to processes created by agents.

For each observed workload, report its real lifecycle owner and available authorized controls. A Harw-owned job links to its processes; an externally started browser remains an external workload unless an explicit supported management relationship is established. A management action against that browser can itself be admitted work without pretending Harw started the browser, knows its previous output or can reconstruct it after a crash.

The inventory is the evidence source for these relationships, not a second scheduler. Preserve process-incarnation checks, explicit user scope, event-loss reporting and snapshot reconciliation from the [host contract](03-host-process-contract.md). Cooperate with systemd/container owners instead of taking over their trees by discovery. Observed state, requested intent and confirmed effect remain separate fields.

## 5. Code review must follow both directions

Trace every selected work type forward:

`caller -> trusted admission -> existing logical job/child/attempt -> placement/reservation when required -> existing executor or native manager -> process/container effect`.

Then trace it back:

`exit/read-back/telemetry -> authoritative job or external-workload record -> node projection -> cloud projection -> TUI/CLI/other client`.

The code map must identify which current function owns each transition, how IDs are correlated, how cancellation reaches the effect, and what is reported after a crash or missing acknowledgement. A new type, DTO, crate, catalog entry or listener is not enough. Test at least one real composed producer-to-consumer path before declaring an adapter integrated.

Use existing modules first. Proposed `harw-host-*` names in the larger dossier describe responsibility boundaries, not a requirement to create all those crates before a useful slice works. Introduce a new package only when a verified ownership/dependency boundary justifies it. Security invariants and architecture gates still apply.

## 6. Implementation focus within the existing plan

Keep the work-package dependency graph, but judge its value against these priorities:

1. Repair the tracked baseline and any canonical adapters necessary to exercise the real job/cloud paths (W00/W01). Do not make an unrelated cloud rewrite a prerequisite.
2. Recheck and complete job/attempt lineage, actual process correlation, cross-instance resource admission, cancellation and existing recovery (W02-W04/W06/W07). Add inventory coverage for external user workloads without inventing their ownership.
3. Deliver a read-only cross-node overview tied to real owners and freshness; use W08/W09 and the read side of W13. This should be usable before automatic placement or broad external mutation.
4. Add permitted management actions and explicitly placed remote work through those same contracts (W05/W10/W11), retaining node-local safety and effect reconciliation.
5. Add richer UI and inference tuning only where they improve these paths. W12 is supporting work and must not displace the job/process/cloud core.

W14 verifies each enabled slice rather than postponing all useful delivery until every optional subsystem exists. This clarification changes priority and acceptance, not the already recorded PR evidence or the rule against broad duplicate-head merges.

## 7. One end-to-end acceptance scenario

On two enrolled nodes, begin with an external browser and user service already running on the Pi. Submit local and remote build work through the existing job entry points. From an authorized client, show each logical job, any child/attempts, owner node, actual execution backend, associated process tree, resource/enforcement evidence and bounded output. Show the browser/service as external workloads with accurate control scope, not as newly submitted builds.

Detach and reconnect the client without duplicating jobs. Exercise cancellation through the original owner. Restart the relevant Harw service and verify the existing recovery path against persisted records and actual effects. Disconnect a node and make its information explicitly stale/unreachable, not fabricated success or automatic failover. Restore connectivity and reconcile. Independent local administration and unrelated user applications must remain usable throughout.

This combines the existing H/J/C/U acceptance cases; it is not claimed executed. Completion requires the composed code path and test evidence, not an agent saying it succeeded or a smoother CPU chart.

## 8. Delivery boundary

The branch contains an architecture/evidence dossier and concrete code-integration plan. This clarification adds no production Rust code, no new privileges and no deployed service. Proposed interfaces are not compiled implementations. All runtime, kernel, engine, cloud and Pi benchmark checks remain explicitly unexecuted until recorded against the chosen implementation SHA.

The next review question is whether this job-first integration scope and the named acceptance paths match the owner's intent, before implementing any new subsystem or expanding live control.
