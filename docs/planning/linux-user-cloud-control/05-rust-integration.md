# Rust interfaces, production paths and file ownership

**All signatures below are proposed contracts, not compiled implementations or promises of existing symbols.** Existing symbols are identified explicitly. Reuse the repository's actual ID, error, boxed-future and tracing conventions when implementing. The implementation plan assigns the first producer, consumer and negative test for every new seam; a crate with only types is not a completed feature.

## 1. Dependency and ownership map

| File / component | State | Responsibility / dependencies |
|---|---|---|
| `harw-host-model/src/{identity,observation,workload,query}.rs` | Proposed new F crate | Transport-free value vocabulary. Reuse F-layer IDs such as WorkId/NodeId where available; no runtime, kernel, service client, model or job-coordinator imports. |
| `harw-host-inventory/src/{reducer,bootstrap,reconcile,store,service}.rs` | Proposed new A crate | One host inventory owner; bounded state, lifecycle merge, checkpoints, scoped queries and projections. Reuses existing filesystem/record-store patterns. |
| `harw-host-inventory/src/{proc_source,dod_source,unit_source,job_source}.rs` | Proposed adapters | Translate existing procfs, DoD, systemd and job observations into the common model. No model inference. Explicitly review dependency edges. |
| `dod/crates/harw-dod-bpf/src/{abi,contract,profile}.rs` and `dod/bpf/src/` | Existing, modify in a separate ABI slice | Versioned creation/lifecycle facts and scope coverage. Do not import the A-layer inventory into privileged code. |
| `dod/crates/harw-probe-bpf/src/{collect,sink}.rs` | Existing, extend narrowly | Preserve lifecycle facts on a bounded push-only route, separately from the SecurityEvent projection. |
| `harw-host-control/src/{admission,target,systemd,effects,governor}.rs` | Proposed new A crate | Trusted control admission, retained target identity, manager adapters, action journal and local pressure policy. No authority constructor exposed to models. |
| `harw-host-control/src/{capacity,reservations}.rs` | Proposed | One node-wide capacity owner with recoverable reservations; no duplicate durable job lifecycle. |
| `harw-command/src/{port,global}.rs` | Existing, modify | Inject explicitly configured execution/capacity services. Avoid ambient fallback when a required service is absent. |
| `harw-runtime/src/{job_wiring,agent_job_wiring,assembly,services}.rs` | Existing, modify at composition seams | Connect jobs and agent attempts to node admission/inventory; keep original durable ownership and authority checks. |
| `harw-job-runtime/src/coordinator/{linux,runner,executor}.rs` | Existing, limited changes | Existing process/attempt effects and result reporting; adapters remain outside J when they require A/I dependencies. |
| `harw-cloud/harw-cloud-ops/src/{enrollments,cloudctl,status}.rs` | Present only in N, repair before integration | Canonical registry/context and typed service clients, not another parser or direct unmanaged execution path. |
| `harw-cloud/harw-cloud-enroll/src/{lib,service,store}.rs` | N helper plus proposed service files | Stateful enrollment over existing node registry and AuthHub; no independent identity source. |
| `harw-cloud/harw-cloud-gateway/src/{config,status,proxy}.rs` | N placeholder/adapter layer | Normalize configuration, report staged readiness, adapt the established proxy policy after dependency review. |
| `harw-cloud/src/{directory,profiles,placement,dispatch,artifacts}.rs` | Proposed composition modules after restoration | Owner routing, profile mounts, candidate selection, remote job dispatch and artifact validation. Reuse existing ports/executors. |
| `harw-cli/src/{session_serve,session_serve_remote,session_listener,gateway}.rs` | Existing | Explicit assembly; no second authentication or policy implementation. |
| `harw-session-host/src/{host,identity,tool_host}.rs` | Existing | Same-session ownership, per-connection authorization and explicit optional ToolHost mounting. |
| `harw-node-listener/src/{identity,revoke}.rs` | Existing | Canonical device registry and live revocation; add transactional update semantics. |
| `harw-protocol/src/host_wire.rs` | Proposed additive protocol module | Versioned read/control envelopes using F-layer types; no runtime or privilege authority inside the protocol crate. |
| `harw-ops/src/host.rs` | Proposed operation family | Register bounded queries and admitted management actions through the existing Operation registry. |
| `harw-tui/src/app/host_glue.rs` | Proposed presentation adapter | Consume typed state, preserve Mini Live direction, surface freshness and exact target identity. No direct signals. |
| `xtask/{arch-policy,spawn-policy}.toml`, root manifests/lock, deployment assets | Existing shared files | One integrator edits these after leaf interfaces agree; run architecture, dependency, spawn and deployment-parity gates. |

These new crates are a proposed responsibility cut, not a requirement to introduce all of them immediately. Keep F vocabulary small; start inventory/control as clearly separated A modules if that reduces packaging overhead without mixing authority. In either case, the probe, J layer and Warden must not gain reverse dependencies on the application controller. Lifting an existing ID from J to F, if necessary, is an explicit compatibility change with re-exports, not a second competing ID type.

## 2. Inventory identity and state vocabulary

The model must not encode 'observable therefore controllable'. A representative contract is:

```rust,ignore
// DESIGN CONTRACT ONLY. These are proposed F-layer types.
pub struct ProcessInstanceRef {
    pub node: NodeId,
    pub boot: HostBootId,
    pub pid_namespace: NamespaceIdentity,
    pub tgid: u32,
    pub birth: BirthWitness,
}

pub enum BirthWitness {
    KernelStart { value: u64, clock: BirthClock },
    ProcStartTicks { value: u64, ticks_per_second: u64 },
    Unknown,
}

pub struct SourceCursor {
    pub collector_epoch: CollectorEpoch,
    pub program: SourceId,
    pub sequence: u64,
}

pub enum CoverageState {
    Bootstrapping,
    Reconciled { generation: u64 },
    Degraded { generation: u64, reasons: Vec<CoverageReason> },
    ResyncRequired { generation: u64 },
}

pub enum LifecycleOwner {
    HarwJob { work: WorkId },
    UserUnit { manager: ManagerInstanceId, unit: UnitRef },
    Container { engine: EngineInstanceRef, instance: ContainerRef },
    External,
    Unresolved,
}

pub struct ProcessRecord {
    pub instance: ProcessInstanceRef,
    pub observed_user: UserObservation,
    pub owner: LifecycleOwner,
    pub program: ProgramObservation,
    pub state: ProcessObservationState,
    pub last_cursor: SourceCursor,
    pub coverage_generation: u64,
}
```

`HostBootId`, `CollectorEpoch`, `SourceId`, namespace and unit identities are validated newtypes, not arbitrary authority-bearing strings. `BirthClock` prevents comparing different kernel clock/tick domains as if they were equal. An `Unknown` birth witness allows a read-only record but is insufficient for an identity-sensitive action. `UnitRef` includes the manager domain and invocation identity when available, so a restarted unit is not silently the previous instance.

The A-layer job adapter adds existing AttemptId/runner/lease metadata to a correlation table keyed by ProcessInstanceRef. Do not add a dependency from F model types to J's coordinator merely to include that metadata. The full tenant/user authorization context is carried by the query service, not guessed from the F-layer observation.

## 3. Lifecycle ingestion and reducer

```rust,ignore
// Proposed interfaces; source-specific payload validation happens first.
pub enum HostObservation {
    ProcessCreated(ProcessCreated),
    ProgramReplaced(ProgramReplaced),
    TaskExited(TaskExited),
    ProcessExitConfirmed(ProcessExitConfirmed),
    OwnershipObserved(OwnershipObserved),
    CoverageChanged(CoverageChange),
}

pub trait ProcessSource: Send + Sync {
    fn bootstrap<'a>(&'a self, scope: &'a ResolvedUserScope)
        -> HostFuture<'a, Result<SnapshotBatch, InventoryError>>;
    fn next_batch<'a>(&'a self)
        -> HostFuture<'a, Result<ObservationBatch, InventoryError>>;
}

pub fn apply_observation(
    state: &mut InventoryState,
    observation: ValidatedObservation,
) -> Result<ApplyOutcome, InventoryError>;

pub fn reconcile_snapshot(
    state: &mut InventoryState,
    snapshot: ValidatedSnapshot,
) -> Result<ReconcileOutcome, InventoryError>;
```

`ResolvedUserScope` is produced locally from trusted configuration; wire clients cannot deserialize one into an authority grant. Snapshot and observation validators bind source identity, limits, schema and coverage before the reducer sees them. `HostFuture` uses the repository's boxed Send future pattern. All budgets, clocks and source epochs are injected; the pure reducer does not call the system clock or perform I/O.

`ApplyOutcome` distinguishes applied, duplicate, stale, rejected and resync-required. An unknown late PID event cannot delete a newer process incarnation. A task exit removes/updates only the task until a supported process-exit observation or reconciliation proves the group terminal. `ReconcileOutcome` records missing/unreadable entries separately from confirmed exits. Do not interpret permission denial as process death.

A scan uses a generation and per-source high-watermarks, not one fake globally ordered BPF number. Write a checkpoint and accepted-journal cursor atomically according to the chosen store contract. On restart replay only accepted records after that checkpoint; if a journal gap exists, enter ResyncRequired before claiming current coverage.

## 4. Query and control ports

```rust,ignore
pub trait HostInventoryPort: Send + Sync {
    fn snapshot<'a>(&'a self, query: InventoryQuery,
                    caller: &'a AuthenticatedHostCaller)
        -> HostFuture<'a, Result<InventoryPage, HostError>>;
    fn inspect<'a>(&'a self, target: ProcessInstanceRef,
                   caller: &'a AuthenticatedHostCaller)
        -> HostFuture<'a, Result<WorkloadView, HostError>>;
}

pub struct InventoryPage {
    pub revision: InventoryRevision,
    pub coverage: CoverageState,
    pub records: Vec<WorkloadView>,
    pub next: Option<InventoryCursor>,
    pub omitted: OmissionSummary,
}

pub enum HostActionRequest {
    AdjustResources { target: WorkloadRef, patch: ResourcePatch },
    Stop { target: WorkloadRef, mode: StopMode },
    RestartUnit { target: UnitRef },
    FreezeOwned { target: WorkloadRef },
    ReleaseOwned { target: WorkloadRef },
}

pub trait HostControlPort: Send + Sync {
    fn admit<'a>(&'a self, request: HostActionRequest,
                 caller: &'a AuthenticatedHostCaller,
                 expected: InventoryRevision)
        -> HostFuture<'a, Result<AdmittedHostAction, HostError>>;
    fn execute<'a>(&'a self, action: AdmittedHostAction)
        -> HostFuture<'a, Result<HostActionOutcome, HostError>>;
}
```

`AuthenticatedHostCaller` and `AdmittedHostAction` are proposed trusted A-layer types with private constructors, no Deserialize and no model-visible secret content. The latter binds the target, permitted effect, policy revision, confirmation disposition, lifetime and one-shot action ID. Admission may ask the existing approval system according to the selected mode, but a previously displayed query result is not an approval token.

`execute` must recheck policy and identity immediately before the effect and retain the required target handles. The controller records intent, calls the appropriate job/systemd/container/Warden adapter and reads back the result. `HostActionOutcome` has `Applied`, `Rejected`, `Unavailable`, `Partial` and `EffectUnknown` variants; failure to read back cannot become success.

The operation layer exposes bounded status/inspect and action submission, not arbitrary shell strings. Actions can themselves be durable jobs with WorkId. Disruptive requests for protected or ambiguously owned workloads are denied independently of confirmation mode. The model may propose an adjustment, but it does not choose the user mapping, mint the grant or bypass the executor.

## 5. Node-local reservation contract

```rust,ignore
pub trait NodeCapacityPort: Send + Sync {
    fn reserve<'a>(&'a self, request: CapacityRequest,
                   admission: &'a TrustedJobAdmission)
        -> HostFuture<'a, Result<CapacityLease, CapacityError>>;
    fn bind_attempt<'a>(&'a self, lease: CapacityLease,
                        attempt: ExistingAttemptIdentity)
        -> HostFuture<'a, Result<BoundCapacityLease, CapacityError>>;
    fn reconcile<'a>(&'a self, observed: NodeAttemptSnapshot)
        -> HostFuture<'a, Result<CapacityReconciliation, CapacityError>>;
}
```

This is an A-level service contract around existing job identities, not another JobState machine. `reserve` atomically accounts for starting and running work across all local clients. Its persistent state uses a single owner or transactional compare-and-swap/locking; the exact storage implementation must not rely on process-local semaphores for cross-process exclusion.

A lease is a reservation, not evidence that controllers are enabled. Before start, the existing executor produces/validates its SandboxReport and applied resource dimensions. A required memory limit without a usable delegated controller fails. On failed launch, release the reservation; on an uncertain launch, reconcile before reuse. A client disconnect does not release a reservation whose workload still runs.

Pressure handling changes future admission and supported own-workload controls. It does not globally rewrite the Linux scheduler. Record rejected placements, reserved capacity, actual cgroup usage, external pressure and observer overhead separately so a future tuning result is interpretable.

## 6. Cloud service ports and canonical composition

Introduce interfaces in the cloud composition layer with the existing authenticated context:

```rust,ignore
pub trait EnrollmentPort: Send + Sync {
    fn issue<'a>(&'a self, policy: PairingPolicy,
                 caller: &'a AuthenticatedCloudCaller)
        -> CloudFuture<'a, Result<PairingInvitation, CloudError>>;
    fn approve<'a>(&'a self, pending: PairingRequestId,
                   caller: &'a AuthenticatedCloudCaller)
        -> CloudFuture<'a, Result<EnrollmentReceipt, CloudError>>;
    fn revoke<'a>(&'a self, device: DeviceId,
                  caller: &'a AuthenticatedCloudCaller)
        -> CloudFuture<'a, Result<RevocationReceipt, CloudError>>;
}

pub trait SessionDirectory: Send + Sync {
    fn locate<'a>(&'a self, session: SessionId,
                  caller: &'a AuthenticatedCloudCaller)
        -> CloudFuture<'a, Result<SessionOwnerLease, CloudError>>;
}

pub trait FleetJobPort: Send + Sync {
    fn submit<'a>(&'a self, intent: FleetJobIntent,
                  caller: &'a AuthenticatedCloudCaller)
        -> CloudFuture<'a, Result<ExistingDurableJobHandle, CloudError>>;
}
```

These signatures specify application composition, not a replacement authentication protocol. `PairingInvitation`'s secret is delivered through an operator channel and redacted from Debug/logs. Listing delegates to the canonical registry scan under the resolved profile. Revocation receipts describe durable revision and completed/pending live cleanup, not a fake all-or-nothing success if a peer is offline.

`FleetJobIntent` is non-authoritative task data: source/artifact references, workspace alias and desired execution requirements. The server derives tenant, issuer, maximum resources, secrets scope and policy. A client-supplied name cannot assert an authenticated NodeId. `SessionOwnerLease` is issued by the directory authority and verified at the owner; its portable representation is not sufficient to mint a local target-control token without fresh checks.

Suggested application path:

```text
client / agent operation
 -> existing transport authentication and identity resolver
 -> existing Operation / SessionPort / ToolPort admission
 -> typed service port from the composition ServiceMap
 -> durable job admission when an effect is asynchronous
 -> directory / placement / node-local reservation
 -> node-side authority + target/artifact revalidation
 -> existing CommandPort / executor or native service-manager adapter
 -> effect journal + read-back
 -> durable result and bounded live projection
```

Do not put systemd actions inside the pure profile enum or make the network proxy a principal issuer. Do not allow Cloud Hub to become Worker/Container Hub just because all service objects are technically available in the same binary.

## 7. Concrete current-to-target repairs

### Cloud enrollment list

Replace `node_devices_path()` and `EnrollmentRow::parse` in N's enrollment operation with an injected read-only registry service. That service receives the bound home/profile at assembly and calls the canonical DeviceRegistry parser. Preserve rejected-line reporting and approval opt-in. Add a composed operation test with a non-default profile and a seven-field row; asserting only the private parser's own six-field fixture is insufficient.

### Cloud service CLI

Split current N `cloudctl` dispatch into service lifecycle and device identity routes. `up/down/restart` use HostServicePort or a reviewed job-backed fixed command. `enroll/revoke` use EnrollmentPort. Make binary/unit names come from the shipped deployment contract. Unknown/unavailable services return a typed failure; a status command never creates or starts a service just to check it.

### Command runtime limits

Replace implicit host executor construction at the composition root with validated options derived from the selected profile and node capability report. Keep `LinuxExecutorOptions::default()` conservative for library callers. Do not silently set a guessed cgroup path. Wire NodeCapacityPort outside the J coordinator where necessary, and retain per-JobManager start reservation as defense in depth rather than deleting it prematurely.

### Local/remote session convergence

Extend the daemon composition inputs to carry explicit optional tool/gateway services. Keep `ToolHost::disabled` when not configured. First add tests proving each current ingress's existing restrictions. Then introduce directory-mediated same-owner attach with per-connection rights. Avoid moving all local sessions into a remote observer factory or broadening that factory to local-owner rights.

### DoD lifecycle preservation

Keep v1 conversion compatibility, add a versioned lifecycle path and source-specific loss/cursor metadata, and make inventory consume that path directly. Security rules receive a derived projection. An unknown lifecycle schema degrades inventory rather than silently suppressing process-end facts. The Warden dependency boundary remains unchanged unless a separately reviewed minimal protocol addition is necessary.

## 8. Required evidence for every adapter

Each adapter PR must name: input authority source; effective scope; producer and consumer; operation/tool/profile registration; configuration resolution; output bounds and redaction; lifecycle owner; cancellation path; crash/ambiguous-effect path; applied restriction report; and the exact test that reaches it through real composition. A private helper test is valuable, but it cannot replace this list.

All new observation fields are data, never instructions. A model receiving a process name such as a command-looking string must see it in the untrusted data block. The provider serialization tests from the previous audit are therefore part of system-management correctness, not an unrelated prompt-polish task.
