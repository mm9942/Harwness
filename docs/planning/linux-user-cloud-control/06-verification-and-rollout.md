# Verification, deployment and measured rollout

**This file specifies future checks. It does not report them as passed.** The current PR contains documentation and a manifest only. The [implementation plan](../../superpowers/plans/2026-10-16-linux-user-cloud-control.md) gives task ownership; source/gap IDs are in the companion register.

## 1. Evidence levels

Every result must say which level was exercised:

- **Static:** tracked source/manifest/metadata or a schema review.
- **Pure test:** deterministic reducer, policy, parser or fake service.
- **Composed test:** real runtime registration and admission with controlled dependencies.
- **Kernel/engine integration:** the actual Linux kernel, user manager, cgroups or container engine on a documented host.
- **Deployment acceptance:** the shipped artifact, config and service identities on the intended device/node topology.

A passing fake-engine test does not prove a real engine applied limits. A kube-apiserver test does not prove pod execution by a kubelet. A model answer does not prove an action or test succeeded. Historical green worktrees are not fresh clean-checkout validation. The harness records SHA, clean/dirty state, toolchain, OS/kernel/architecture, feature set, command, exit code, elapsed time and log/artifact references.

## 2. Host and inventory matrix

| Test ID / proposed regression name | Setup and assertions | Gaps |
|---|---|---|
| H01 `bootstrap_discovers_preexisting_user_workloads` | Start a browser-like process tree, terminal process and user service before inventory. Bootstrap finds them without creating Harw jobs or altering their cgroups. Scope/coverage is explicit. | 18-20 |
| H02 `fork_without_exec_is_visible` | Create a child that does not exec; verify a distinct process instance, while a subsequent exec updates that instance rather than duplicating it. | 14 |
| H03 `leader_exit_does_not_end_live_thread_group` | Keep another thread alive after leader exit; task termination does not mark the whole workload done. Exercise non-leader exec separately on the supported kernel. | 14-16 |
| H04 `stale_pid_event_cannot_mutate_new_incarnation` | Replay old create/exit/action requests against reused PIDs and a changed boot/namespace. Old evidence never deletes or controls the replacement. | 16 |
| H05 `event_loss_requires_visible_resync` | Drop/reorder/duplicate records and restart one source epoch. Coverage degrades, a bounded scan reconciles, and no false complete/idle state appears. | 15-18 |
| H06 `scope_revision_covers_new_cgroup_descendants` | Create child cgroups after probe attach. Either the supported revised filter covers them or coverage explicitly excludes them until a safe update/resync. No host-wide fallback. | 17 |
| H07 `display_limit_is_not_inventory_limit` | Supply more than 2,000 processes; paginated internal inventory retains all within its configured capacity while display truncation reports omissions. Capacity exhaustion is visible. | 18 |
| H08 `permission_loss_is_not_process_exit` | Make selected proc/unit metadata unreadable midway through a scan. Records become unverifiable/stale, not terminal. | 16,18-20 |
| H09 `external_pidfd_does_not_claim_child_reaping` | Open an external process handle, observe exit readiness, and ensure the API does not promise child wait status, adoption or a restart recipe. | 20 |
| H10 `systemd_ownership_is_preserved` | Observe and adjust an admitted user unit through its manager. No direct cgroup migration or controller writes outside the delegated subtree occur. Test manager restart/invocation change. | 21 |
| H11 `user_scope_handles_namespaces_and_helpers` | Explicit human user, service UID, rootless container mapping and user-launched elevated helper. Only configured ownership is accepted; ambiguous/elevated cases stay read-only. | 19 |
| H12 `observer_failure_leaves_user_session_usable` | Kill/restart inventory, agent and cloud connection independently. Desktop/terminal/native manager remain usable; emergency disable and effect-aware rollback work. | 20-21,35,40 |

## 3. Job, resource and recovery matrix

| Test ID / proposed regression name | Required behavior | Gaps |
|---|---|---|
| J01 `two_clients_share_one_node_reservation` | Two independent Harw processes race for a single host slot: exactly one admission wins; failed starts release it. A local semaphore test alone cannot satisfy this. | 23 |
| J02 `required_cgroup_limit_refuses_unconfigured_executor` | Required limits without a delegated root/controller refuse before workload execution. Optional reduced mode reports exactly what was not enforced. | 22,25 |
| J03 `attempt_correlation_does_not_double_count_children` | One WorkId with compiler subprocesses and agent projections is one admitted job; resource totals are attributed once according to the chosen accounting source. | 20,23 |
| J04 `cancel_and_deadline_reach_owned_workload_tree` | Use the actual native/group/cgroup backend; cancellation, deadline and lease loss reach allowed descendants and leave an accurate terminal/effect state. Unrelated processes remain alive. | 22-25 |
| J05 `restart_reconciles_pending_agent_ownership` | Crash before ownership transfer, after Ready, during execution and before terminal commit. Rehydrate with fresh policy or visibly terminate/block unreconstructable work; never strand it silently. | 24 |
| J06 `ambiguous_launch_does_not_duplicate_side_effect` | Drop the start acknowledgement after a backend effect; retry/recovery inspects attempt identity before any new launch. State fencing alone is not accepted as proof. | 24,35 |
| J07 `resource_dimensions_are_not_interchangeable` | Test memory high/max, CPU weight/quota, pids, output bounds and RLIMIT_AS independently. A virtual-memory refusal is not mislabeled physical-memory exhaustion. | 25 |
| J08 `pressure_governor_has_hysteresis_and_headroom` | Replay bursty pressure and recovery; verify bounded action rate, minimum spacing, no permit oscillation and protected interactive work. Freeze is not counted as memory reclaimed. | 23,25-26 |
| J09 `job_exit_and_observed_exit_remain_distinct` | A disappeared process with no authoritative result is not marked successful; an agent's verbal completion does not finalize a durable job. | 20,24 |
| J10 `all_command_surfaces_use_the_declared_owner` | Exercise shell, operator command, LaTeX, job.start, compiled child and container helper through their real ports. Explicit infrastructure exceptions remain named in the spawn gate. | 9,13,22 |

## 4. Cloud integration matrix

| Test ID / proposed regression name | Required behavior | Gaps |
|---|---|---|
| C01 `restored_cloud_members_build_from_clean_checkout` | No local substitute manifests. Locked metadata, layer/license/version checks and intended feature builds use only tracked files. | 1-5 |
| C02 `cloud_enrollment_list_matches_canonical_registry` | Six-field and optional-approve seven-field records round-trip with node/device order intact; non-default profiles remain isolated; rejected lines are reported. | 6-7 |
| C03 `device_actions_are_not_service_enablement` | Enroll/revoke reach the enrollment registry/revocation path, not systemctl enable/disable; service operations reach only the declared service adapter. | 8-9 |
| C04 `unrelated_tcp_listener_is_not_cloud_ready` | A fake port listener, wrong node, missing mounted service, expired identity or unavailable AuthHub never yields full readiness. | 10-13 |
| C05 `pairing_consumes_once_under_concurrency` | Two presentations race; only one enrollment commits. Expiry at the boundary, restart mid-transaction, wrong fingerprint and stale policy have defined refusal/recovery outcomes. | 12 |
| C06 `revocation_reaches_live_and_queued_authority` | Revoked device cannot reconnect, continue queued mutations or retain allowed in-flight authority beyond policy. Unrelated local work continues. | 12,35,37 |
| C07 `two_devices_one_session_one_execution` | Two authenticated clients attach and race a duplicate submit: one execution, scoped replay, first-writer approval, no rights inherited from the other client. | 27-28 |
| C08 `directory_restart_rejects_stale_owner_generation` | Competing old/current owner reports after hub restart; stale report cannot reconstruct itself as owner. Owner-unreachable is explicit, never silent failover. | 30,35 |
| C09 `placement_filters_before_scoring_and_reserves_before_start` | Stale, drained, rootful-without-opt-in, incompatible or insufficiently enforced candidates are rejected regardless of score. No unreserved launch. | 29-32 |
| C10 `container_readback_failure_prevents_start` | Real selected engine/API: denied or dropped restriction, unknown timeout, identity mismatch and failed read-back prevent workload start. Include #147's advertised timeout cases. | 3,25,31 |
| C11 `single_flight_worker_boot_is_job_owned` | Parallel first demand creates one boot attempt, with bounded deadline/retries; lost boot acknowledgement is reconciled before replacement. | 32 |
| C12 `remote_artifacts_remain_scoped_and_verified` | Wrong source revision, target architecture, digest, tenant or policy revision refuses execution/result admission; no raw-home replication. | 33 |
| C13 `partition_preserves_local_safety_without_remote_bypass` | Isolate hub, provider and node transport independently. Local supervision survives, expired remote requests fail, and no broad local fallback is mounted. | 35 |
| C14 `proxy_and_tunnel_use_policy_and_bounded_lifecycle` | Unknown route, spoofable headers, oversized body, upgrade refusal, repeated reconnect and cancellation all follow the shared policy and release resources. | 11,34 |
| C15 `model_lane_releases_permits_on_all_terminal_paths` | Success, stream end, subscriber cancellation, timeout, 429/capacity response and retry exhaustion release/account permits correctly; coalesced subscribers are independent. | 36 |
| C16 `cache_lookup_respects_data_sharing_scope` | Identical payloads from disallowed sharing domains do not reuse private responses. Validate request-field canonicalization, model/settings and tool/streaming eligibility. | 36,40 |
| C17 `profile_mounts_only_its_service_capabilities` | CloudHub cannot obtain worker shell/engine sockets by selecting a display mode; unavailable services are typed refusals. | 13,30 |
| C18 `provider_wire_carries_selected_host_evidence_as_data` | All supported providers receive the admitted bounded evidence without silently dropping or promoting it to instructions. Use golden wire fixtures and real mounted context paths. | 38,40 |

## 5. UI, privacy and boundedness

U01 verifies that managed jobs, external processes and logical agents can appear in one view while retaining different ownership, count and control semantics. U02 checks stale target selection against a reused PID and changed policy; confirmation cannot authorize a replacement. U03 checks the 40-59-column portrait cases, x/y resize boundaries, modal/focus behavior and deterministic Mini Live fallback without a model.

U04 injects command-looking process names, secrets in argv/environment and oversized records. Default telemetry/model/cloud views remain allowlisted and redacted. U05 slows one cloud/UI subscriber until it falls behind: bounded memory, visible lag/resync, no stalled collector or model-permit leak. U06 purges retention data and replays delayed events: tombstone/coverage boundaries prevent false association and management journals remain available as policy requires.

These tests close LUC-39/LUC-40; they also demonstrate that the user-facing story corresponds to actual state rather than an attractive but authoritative-looking projection.

## 6. Performance experiment for the Pi observation

Do not deliberately recreate a destructive low-memory crash on the daily-use machine. Use a recoverable test image or an equivalent node with a console, saved data and bounded stress limits. Preserve the user's existing working installation.

Freeze hardware, kernel, power/thermal conditions, storage, swap/zram settings, compiler/linker flags, crate revision, cargo job counts, cache state and browser workload. Record cold and warm builds separately; sccache or incremental changes can dominate apparent speedup. Keep local versus remote execution explicit.

Compare at least: baseline supervision, inventory-only, inventory plus own-job capacity policy, and the approved full managed profile. Do not disable security mechanisms casually on a production host; use equivalent isolated runs. Collect per-process/cgroup attribution and the action journal so changes can be linked to actual decisions.

Measure browser/desktop interaction latency, completed build wall time and throughput, memory and I/O PSI, OOM events, swap traffic, scheduler/run-queue indicators, temperature/throttling, idle and peak observer CPU/RSS, descriptor count, dropped events, queue wait and cumulative resource use. Report distributions and repeated trials, not one screenshot or a single average CPU percentage.

Success means useful concurrent work and responsiveness improve or meet the declared envelope while observation/control overhead stays within a budget chosen before the trial. Reduced CPU can also mean throttling or stalled work; higher CPU can mean better throughput. Do not publish a causal Harw speedup without controlled evidence.

## 7. Build and verification gate

Run the repository's current documented gate matrix on a frozen implementation SHA, from an ordinary unprivileged build account. The minimum proposed sequence is:

```sh
git status --porcelain
git rev-parse HEAD
cargo metadata --locked --no-deps --format-version 1
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo xtask gates all
cargo xtask gates spawn
cargo nextest run --workspace --all-features
cargo test --workspace --all-features --doc
cargo deny check
actionlint
```

Check actual gate/feature names against the chosen SHA; a changed command interface requires an updated command log, not a silently skipped gate. Run documented package-scoped DoD checks without enabling/loading probes. If all-features includes unsupported platform or mutually exclusive combinations, define and execute the supported matrix explicitly; do not label an omitted combination green. Exercise native x86_64 and aarch64 Linux, delegated and non-delegated cgroups, supported/unsupported BPF capabilities, and systemd/non-systemd reduced mode where promised.

Privileged integration tests require a separate disposable host/VM and explicit operator consent. They are never hidden in ordinary unit tests, install checks or model-triggered background builds. Real engine tests must state engine version, rootless/rootful mode, kernel and, for Kubernetes, whether actual kubelet execution occurred.

## 8. Rollout sequence and rollback

| Phase | Enable | Keep disabled / required rollback proof |
|---|---|---|
| R0 | Static readiness and configuration validation | No new listener or actuation. Restore canonical tracked baseline. |
| R1 | Read-only inventory for one explicit user on one node | No process moves/signals. Verify coverage/loss and bounded overhead. |
| R2 | Correlation and limits for Harw-owned jobs | External apps remain observed. Stop controller without affecting them. |
| R3 | Explicit user-unit controls through native manager | Protect session-critical units; journal and revision-aware rollback. |
| R4 | Authenticated read-only cloud summaries and session routing | No automatic placement/failover. Test revocation, partitions and privacy. |
| R5 | Admitted remote jobs on one reviewed backend | No implicit fallback; verify artifacts, reservations and ambiguous effects. |
| R6 | Broader user scopes and selected reversible automation | Only after independent review, measured results and emergency-path drills. |

Configuration changes stay opt-in and narrowed by untrusted layers. Rollback disables new admissions first, drains or safely stops only owned work as policy permits, restores only still-owned changes, records incomplete effects, and leaves native system administration reachable. It does not recursively kill all observed user processes or overwrite a newer operator setting.

## 9. Validation of this documentation PR

Performed: GitHub source/metadata reads; pinned dev and latest-head checks; comparison of B -> N and #145 -> N; source-level inspection of restoration adapters and representative job/process/session seams; PR description reconciliation. The PR is based directly on B and is intended to modify only the listed new documentation/manifest files.

Not performed: Cargo, clippy, tests, BPF load, systemd actions, container execution, cloud deployment or performance measurement. The Rust blocks are interface sketches, not compiled proof. After creation, verify the PR's base, draft flag, head parent and changed-file list; do not describe implementation or deployment work as complete merely because this dossier is published.
