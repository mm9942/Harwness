# Cloud workload handoff: source boundaries and blockers

This note reconciles the candidate source with [Cloud Home profiles H7](CLOUD-HOME-HUB-PROFILES-V2.md),
[placement](../66-placement/README.md), [job-owned worker boot](../67-containers/on-demand-worker-boot.md),
and [Cloud Hub transport T5](../99-cloud-hub-mtls-tor/README.md). It records a source audit,
not a runtime verification or a new wire format. There is no Cloud workload submission in this slice.

## Reuse the job owner for each lane

| Lane | Existing local entrypoint | Missing before a Cloud handoff |
| --- | --- | --- |
| Compile Hub | `harw-plan-bridge/src/verify_exec.rs`: `CoordinatorVerifyRunner` converts a local `CommandRequest` to a `JobSpecEnvelope` with mandatory sandbox. `harw-cli/src/verify_sandbox.rs` provides the actual workspace, backend and limited environment. `harw-job-runtime/src/coordinator/runner.rs`: `start_existing` atomically claims an exact, locally admitted process record; it cannot replace its command. | A trusted resolver for a closed/versioned compile operation or approved, immutable plan-step reference. It must bind the content/revision, workspace and caller before creating the record. The existing verify command accepts program/argv; argv validation and absence of a shell are not remote compile authorization. Preserve workspace exclusivity where required and the approved backend; do not inherit a verifier fallback. |
| Worker Hub | `harw-runtime/src/agent_job_wiring.rs`: `RuntimeAgentJobSubmitter::submit_child` requires an already-admitted managed child, captures the recovery view, and only makes its durable record claimable after local lifecycle ownership transfer. The owner drives it under a job lease. | A durable worker-side authority and child re-admission path binding lineage, compiled agent/artifact identity and limits. A serialized recovery snapshot or session ID is not a grant. Agent orchestration is not an arbitrary process `JobSpec`. |
| Container Hub + Compile Hub | `harw-tool-container/src/plan.rs` and `harw-tool-container-run/src/{tools,engine}.rs` build and run a validated container **run** plan using approved image aliases and rootless Podman. | Typed **build** intake, immutable source/context binding, build artifact identity, dedicated owner-side executor and containment verification. The existing run path neither builds images nor admits rootful Docker. Do not use caller-specified engine flags, command wrappers, exposed sockets, or an implicit backend substitution. |

The current `ManualCloudWorkCommandV1` in `harw-protocol/src/session_wire.rs` and
`HostConnection::manual_cloud_work` in `harw-session-host/src/host.rs` only handle
an empty hosted session and its idempotent resolution. They are not a workload API.

## Why a placement pointer or serialized job lease is insufficient

1. `harw-runtime/src/deployment/cloud_placement.rs` filters an independently observed
   worker against policy and produces in-process, expiring advice. A policy row,
   advisory decision, or mount name is not admission, a capacity reservation, or
   proof that a job is running.
2. `harw-job-core/src/spec.rs` validates executable/program arguments, resource syntax
   and workspace-relative paths. `JobSpecEnvelope` versions that payload; it does
   not authenticate a source, bind a target, or authorize a caller. `JobScope` in
   `harw-job-core/src/stored.rs` is resolved and persisted by an admission boundary.
3. `harw-session-store/src/job_store.rs` owns job epoch/nonce, claim and renewal; the
   local process coordinator uses its own atomic exact-record claim contract in
   `harw-job-runtime/src/coordinator/store.rs`. Deserializing a `Lease`/token or
   receiving one over authenticated transport neither verifies that claim nor proves
   it is still current. Placement reservation and job-attempt fencing are separate.
4. The routed inner device-to-node session required by PL-99 T5 is not the browser-edge
   Pingora placeholder in `harw-cloud/harw-cloud-gateway/src/proxy.rs`. A successful
   direct connection or a transport signature does not grant workload admission.

## Gates for the first real handoff

- Start with a single closed operation on one explicitly chosen execution lane. Resolve
  all caller, tenant, workspace, content and authorization bindings at the host. Use
  the existing owner-local executor and durable job lifecycle rather than copying them
  into the hub. Keep Cloud Hub as coordination, not an executor.
- Before defining a portable signed authorization, identify its issuing authority and
  key purpose, validating worker, and durable replay/fencing authority. The authorizer
  must cover the operation identity, exact arguments or content-bound typed payload,
  issuer/subject and tenant/workspace/lineage, selected node and worker instance,
  mounted profile and backend, placement policy revision, sandbox/approval binding,
  intended worker/audience, and expiry. The worker independently rechecks current
  authorization, lifecycle, and enforcement, and claims once at the owning store.
  Do not place job lease secrets, replay tokens, or raw specs in discovery output.
- Prefer the same-scope durable attempt/response model under retries and reconnect.
  A missing or ambiguous acknowledgment is not permission to run again elsewhere.
  Refuse missing/wrong scope, stale/revoked workers, unsafe sandbox, mismatched
  source or profiles, and unknown ownership or outcome.
- Keep the Podman build and rootful Docker work explicitly unavailable until an
  actual typed executor and separate root-authority approval path exist.

No common local intent wrapper is introduced here: none of the existing source
provides a common closed, authority-bearing input for the three workload lanes.

### Registered local compile preflight

`harw-runtime/src/deployment/cloud_compile_handoff.rs` now defines one fixed
`cargo test --locked --offline` template with a required no-network sandbox and
bounded resources. Its read-only preflight binds an independently authenticated
local owner, immutable source/toolchain byte pins, job scope and exact record revision,
current compile placement/activation, and the existing approval store's exact
fresh `ApprovedOnce` receipt. Only an untouched Ready process record is accepted.
No request field can change the executable, arguments, environment, or sandbox.

The owner-local input store must contain private regular files named
`source-<blake3>` and `toolchain-<blake3>`. No symlink is followed on the
configured absolute store path or the fixed object names. The loader
bounds reads to 64 MiB for source and 256 MiB for the toolchain, verifies the
exact bytes against both approved digests, and keeps immutable in-memory
snapshots. Non-private or changed objects fail closed. The local receipt
binds both digests; older source-only and byte-pin-only receipts are not accepted.

This does not issue approvals or records, extract either bundle, prove what was
installed or mounted, or claim a lease. A future owner-side integration must
safely materialize the retained bytes, verify the installed executable and
checkout identities through launch, re-admit the worker, and atomically claim
the same checked record. A successful preflight is not a credential or proof
that an attempt ran.

Preflight retains the exact checked process record internally, bound to the
approval store and process store that supplied it. Read-only revalidation rejects
any change to that complete record even if the revision is unchanged; it does not
expose the executable record, consume a receipt, or allocate a lease. Future
dispatch must pass that retained record to the existing under-lock exact-record
claim and never call the work-ID-only start API as a second step. Project trust
covers repository configuration, not an immutable runnable checkout or Cargo
executable. The byte pins are a prerequisite; a runnable binding remains absent.

### Validated bundle plan (no materialization)

The V3 local approval domain now requires both input pins to pass the
uncompressed compile bundle V1 parser. The object begins with the ASCII bytes
`HARW-COMPILE-BUNDLE` plus NUL, a big-endian 32-bit JSON manifest length,
the manifest, then exactly the contiguous regular-file payload bytes. The
manifest has `version=1`, the exact `source` or `toolchain` role, and a
strictly path-ordered `files` array. Each entry has `path`, `mode` (`data`
or `executable`), `bytes`, and its BLAKE3 `digest`. Validation checks the
manifest and archive limits, every file's range and digest, and refuses
unknown metadata, trailing bytes, unsafe path components, case-folded
duplicates, and file/directory prefix conflicts. Links and special files have
no representation. Source requires a nonempty root `Cargo.toml` and
`Cargo.lock`; the toolchain requires nonempty executable `bin/cargo` and
`bin/rustc`. Repository-controlled Cargo/toolchain config and `.git` are
not admitted.

The plan only holds normalized relative paths, closed modes, and ranges into
the pinned bytes. It does not infer binary format/ABI, validate that vendored
dependencies are complete, create directories, unpack files, install a
toolchain, acquire a lease, or start a process. A future trusted materializer
still has to preserve the source/toolchain identities through actual launch.
