# Cloud control and execution contract

**TARGET contract.** It reuses B06-B12, reconciles N01-N06 and closes LUC-06-LUC-13 and LUC-27-LUC-36. Source anchors are in [README](README.md#8-evidence-index). Nothing here asserts that the full fleet is already deployed.

## 1. Three independent planes

**Host plane:** each Linux node owns its inventory, kernel handles, job supervision, local resource admission and constrained actuators. It knows which effects actually occurred there.

**Session/control plane:** authenticated clients send intentions through the existing SessionPort/ToolPort/GatewayPort contracts. The session owner controls its transcript and agent turns; a directory resolves that owner. Authority is derived from the authenticated connection and policy, not from transport payloads.

**Inference plane:** provider routing, model limits, queues and caching decide how admitted model calls reach backends. The Cloudflare AI Worker is not the session gateway and is not the host's CPU scheduler.

This separation prevents a network route, a provider key or a UI session from becoming universal host authority. It also permits a Pi to retain local supervision while using remote inference or an explicitly placed remote build.

## 2. Repair restoration adapters before extending them

The N source tree is useful as a restoration candidate, not a canonical second implementation of identity. Reconcile these concrete seams first:

1. `cloud.enrollments.list` consumes `DeviceRegistry::scan` through a resolved home/profile service. The canonical wire/storage order is `node|device|tenant|tier|status|label[|approve]`; no independent parser or implicit `profiles/default` remains.
2. Service start/stop/restart are named service operations. Device enroll/revoke operate on device identity and live authority. `systemctl enable/disable` must not masquerade as pairing or revocation.
3. Replace the `harw-ctl`/`harw-cloudctl` naming split with one declared adapter or remove the wrapper. Reconcile the referenced service name with the canonical `deploy/` assets before advertising a command.
4. Cloud operations receive typed, context-bound service ports. Do not resolve sensitive state through global environment variables or perform unbounded blocking subprocess output collection inside async handlers.
5. The proxy adapter consumes the existing RouteTable and header policy. The placeholder type is not a service; optional dependency presence is not readiness.
6. Prefer the existing strict configuration hierarchy. If legacy YAML import remains useful, make it an explicit validated migration into one normalized configuration model, not a competing live authority source.

Operation discovery reports `implemented`, `mounted`, `permitted` and `ready` separately, with reason codes. A tool can exist in a catalog while its required service is absent. The caller should learn that distinction before a model repeatedly retries it.

## 3. Canonical identity, enrollment and revocation

Use the existing AuthHub node-key purpose, pinned peer verification and device registry as the roots. A successful encrypted connection does not by itself admit a device or assign a tenant/tier. The new enrollment DTOs must be adapters over this authority, not parallel registries.

An enrollment transaction has explicit states: `Issued -> Presented -> Approved -> Committed`, with terminal `Expired`, `Rejected` and `Consumed`. The proposed service stores only a protected digest of the enrollment secret, a typed expiry, the host fingerprint, the intended device/key binding, requested display metadata, and the maximum tier/scope set by the operator. Secret presentation alone never determines a higher tier.

A trusted transaction boundary atomically consumes the invitation and persists the validated peer pin plus device-policy record, or records a recoverable incomplete transaction that grants no access. Concurrent presentations cannot both commit. Repeating a committed request returns its existing result only when its idempotency binding matches. Expiry uses a typed clock interface so equality, skew, restart and malformed-time behavior can be tested; it is not a free-form timestamp-string comparison.

This design intentionally specifies safety requirements rather than replacing cryptographic primitives. Choose reviewed comparison, secret generation and storage helpers from the existing stack. Protocol/bootstrap changes and newly discovered security defects require the repository's private security review process before public enablement.

Revocation is a lifecycle event, not merely a file edit: publish a new registry/policy revision, refuse fresh authentication/admission, close affected live connections, invalidate scoped contexts and queued requests, and cancel affected in-flight remote work according to its admitted policy. It must not terminate unrelated local applications. Distinguish device tier changes, key rotation, node drain and node revocation. No display string or cached node address can self-reactivate a revoked identity.

## 4. Deployment profiles and readiness

Reconcile #90's pure profile work with the Cloud Home design and #142's parked status. A `DeploymentProfile` selects **services**, not agent privileges. The proposed mount table is:

| Profile | Mounts | Does not acquire implicitly |
|---|---|---|
| Coding | Local user interface, scoped tools, job client, optional local hosted sessions | Cloud administration or other users' workloads |
| Cloud Hub | Device/node directory, routing, policy administration and session-owner lookup | Generic host shell, workspace write or container-engine socket |
| Worker Hub | Admitted agent/process execution and node-local job supervision | Tenant-independent data or unrestricted host root |
| Container Hub | Reviewed OCI/K8s adapters and isolated workload lifecycle | Engine socket inside untrusted workloads or privileged containers |
| Compile Hub | Pinned source/build/test jobs, artifact handling and build-cache policy | Silent execution of unverified artifacts or arbitrary workspace migration |

Profiles can be combined only where the capability model explicitly allows it. Do not combine Cloud Hub administrative authority and execution-engine access in one process merely for convenience. Use existing architecture rings and service identities; new `harw-cloud` members must be classified and dependency-checked before they become default builds.

A readiness report contains separate stages: configured, dependency reachable, peer authenticated, protocol compatible, service mounted, scope bound, required restrictions available, and admission accepting. `Reachable` may be true while `Ready` is false. A refused authentication, unavailable AuthHub, unknown device, missing tool gateway, stale offer, draining node or full capacity has a distinct reason. TCP acceptance alone proves none of those stages.

## 5. One session owner, multiple authenticated clients

B currently uses separate local and remote SessionHosts to avoid transferring local-owner rights to remote devices. Preserve that boundary until a canonical shared-owner design is implemented; changing the remote factory to the local owner tier is not acceptable convergence.

The target keeps one session execution ceiling and authorizes each connection separately. Clients can attach to the same SessionId with different capabilities. Read-only attach cannot resume or mutate; an approval actor is derived from the authenticated identity and existing opt-in rules. An agent cannot approve its own tool call. The human session-control route and the registered-agent ToolPort route remain distinct as in #57.

A session-owner service is constructed once with explicit driver, transcript, approval and optional ToolHost services. Method-table exposure without a real ToolHost remains an intentional refusal. Tests must exercise actual composition, not only a port trait or in-memory mock.

Continue means reauthenticate, derive current capabilities, resolve the owner, attach using the durable cursor and replay. New means create a new session without implicitly cancelling unrelated durable jobs. A network disconnect detaches a client; it is not a job cancellation. Host shutdown/drain is a different explicit event whose treatment of running work must be documented and tested.

## 6. Directory and fencing

Maintain two distinct stores:

- **Location cache:** where an owner currently reports a session; reconstructible from authenticated owner reports.
- **Generation authority:** the durable highest ownership generation issued for a SessionId; not reconstructible by trusting whichever old owner reports first.

The owner record binds SessionId, tenant, NodeId, host instance, placement generation, owner-local epoch and liveness. A host-local epoch is not comparable across nodes. The node validates generation and authorization before acting, not only when the cloud router first accepts a request.

V1 uses one directory authority and does not live-migrate or silently fail over an active session. When its owner is unavailable, the session is unavailable until that owner returns or a separately reviewed transfer protocol is used. Sharing transcript/job directories across hosts does not implement fencing. Multi-hub high availability would require a consensus-backed ownership design and is out of this first implementation.

Node lifecycle remains the existing `harw-netsec` state machine. Online/offline observation is separate from lifecycle state: an offline Active node is not implicitly Revoked, and an online Revoked node is not eligible. A node cannot grant itself Active status. Drain stops new admission while existing bounded work follows its policy.

## 7. Placement is filter, reserve, execute and reconcile

`NodeOffer` and `RuntimeOffer` describe observed capabilities and freshness. They are not permission grants or reserved capacity. The complete placement transaction must include:

1. Resolve a trusted job intent: tenant, workspace snapshot, required runtime/sandbox dimensions, model/provider constraints, budget and execution policy.
2. Reject candidates that are stale, unreachable, non-Active, unauthorized, incompatible with target architecture or unable to enforce required restrictions.
3. Score the remaining candidates deterministically by the configured priorities such as locality, predicted queue delay, cache affinity and cost. Record why candidates were excluded. Unknown cost or capacity is explicit, not a fabricated zero.
4. Reserve node-local resources and the relevant execution lane under a lease before launch. Provider capacity is acquired only for model calls that are actually ready to run, not during arbitrary host queueing.
5. Ask the selected node to revalidate the scope, policy revision, artifact provenance and current restrictions. A cloud admission is not permission to skip the node's checks.
6. Persist the attempt/effect intent, start through the existing executor, correlate the resulting process/container identity and commit terminal state under the lease fence.
7. On timeout, partition or ambiguous acknowledgement, reconcile by attempt and immutable instance identity. Never blindly duplicate an external side effect because a response was lost.

Reservations across multiple services need an explicit order, deadlines and compensating releases; no nested locks across network waits. V1 can use reserve-node then acquire the short-lived provider permit when required, releasing the node reservation if the bounded wait expires. A reservation journal handles crashes between these steps. This is not a claim of distributed exactly-once side effects: fencing protects state writes, while effect reconciliation and idempotency protect executions where the backend supports them.

No candidate means `PlacementUnavailable`, with reasons. There is no silent host-shell or alternate-tenant fallback. Moving future jobs to another host is supported earlier than live process migration; do not describe an existing browser process as portable just because jobs have stable IDs.

## 8. Worker boot, OCI and Kubernetes

Use one job-owned activation coordinator for on-demand workers. A runtime key includes node, tenant, immutable image/bundle identity, profile, toolchain and policy revision. Concurrent first demand shares one boot attempt through node-local admission. Failed boot is visible and releases reservations after effect reconciliation. A warm reusable worker is not trusted merely because its name matches; verify identity and restrictions again.

Integrate existing OCI/K8s executors through a backend factory instead of rebuilding a Podman supervisor from an older branch. Preserve create -> inspect/read-back -> verify -> start ordering. Kubernetes admission can change a requested pod; read back the admitted object before lifting its scheduling gate. A kube-apiserver test is not a kubelet execution test.

Make backend limitations explicit: process, cgroup, OCI and pod controls do not enforce identical dimensions. Test timeout read-back attributed to #147 independently on the selected engine/version. Requested timeout is not applied timeout. Engine credentials and rootful access are operator-controlled; untrusted workloads never receive the engine socket.

The `container.run` tool, the OCI Executor implementation and automatic multi-node placement are three different integration claims. Each needs its own reachable path and tests.

## 9. Source, artifacts, storage and caches

A remote build request carries a source revision/digest, trusted workspace mapping, toolchain/target tuple, allowed dependencies or image pin, output contract, resource budget and retry/effect policy. Absolute local paths are not portable authority. Resolve input artifacts under the receiving node's own scope.

The job records which input and backend actually ran. Result retrieval verifies content digests, provenance and access scope before exposing artifacts to the caller or admitting them as executable code. Bounded logs are evidence, not instructions. A cache key includes all inputs that affect correctness, with tenant/data-sharing policy enforced before cache lookup. Separate public reproducible build inputs from private source, secrets and per-user state.

Do not replicate entire HARW home directories as a shortcut to Cloud Home. Session transcripts, job leases, key material, observation journals, build artifacts and disposable caches have different owners, durability and retention rules. Provider credentials and private key material remain at their designated service; remote clients receive references and constrained outcomes.

Automatic provider VM creation is not part of this phase. A later provisioner needs explicit provider selection, spend ceiling, idempotency, teardown and orphan-resource accounting. Existing job placement on known hosts is not provisioning new infrastructure.

## 10. Inference gateway correlation

Reuse the existing model-scoped lanes and bounded queues in `mias-lab`. Connect host JobId/WorkId to provider request correlation through a non-authoritative tracing reference, not by sending raw inventory or host credentials. Model concurrency, host CPU parallelism and build slots are distinct budgets.

Audit permit lifetime across success, streaming completion, disconnect, cancellation, timeout and shared in-flight requests. One coalesced request may have multiple subscribers; losing one subscriber must neither cancel the others accidentally nor retain a permit forever. Retry budgets cover the whole request path so retries in Harw, Worker and gateway do not multiply without a bound. Shared provider capacity errors produce backpressure, not gateway fan-out intended to evade quotas.

Response reuse must respect tenant/data-sharing policy, model settings, tool/streaming behavior and all semantically relevant request fields. Prefix affinity is not an authorization boundary. Adaptive model-lane code in the repo is not proof of deployed settings or improved production cache efficiency. Provider-wire context parity from #143 remains a prerequisite for trusting model explanations based on the host's selected evidence.

## 11. Offline and emergency operation

Local job supervision and deterministic resource limits must work without the cloud. A node retains last valid local policy within its allowed lifetime, rejects stale remote grants and continues only work its policy permits. Loss of the cloud does not erase local inventory or automatically terminate external user applications.

Cloud reconnect performs fresh authentication and policy resolution, then replays scoped state from cursors. It cannot revive revoked authority or overwrite a newer owner generation. A failed or unavailable model leaves deterministic status and safe operations available.

The operator needs an independent local administration path to stop/disable Harw management and release explicitly managed freezes or limits safely. Rollback acts only on changes Harw owns and whose current revision still matches its action journal. It must not overwrite a later human or systemd change. Test this path before advertising unattended all-user-workload control.
