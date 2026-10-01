---
id: PL-65-CLOUD-HOME-HUB-PROFILES-V2
title: "Cloud Home Hub v2 — hosted sessions, deployment profiles and multi-node control"
status: proposed
date: 2026-10-01
baseline:
  repo: mm9942/Harwness
  ref: dev
  sha: 70faabed9182834792a5f36e67cd80535792542a
related:
  - README.md
  - local-own-cloud-websocket.md
  - contracts/W00-websocket-control-plane.md
  - contracts/R18-tool-gateway.md
  - ../68-local-cloud-websocket/README.md
  - ../67-containers/on-demand-worker-boot.md
---

# Cloud Home Hub v2

## 0. Why this document exists

The control-plane work has moved far enough that the planning baseline must stop
describing already-landed foundations as hypothetical.

Two important implementation rounds already exist on `dev`:

- W00 round 1 landed `harw-protocol::SessionPort`, the single-writer
  `harw-session-host` and the `harw.session.v1` WebSocket transport.
- R18a landed the remote tool/gateway vocabulary, `ToolPort`,
  `GatewayPort`, tool hosting, gateway operations and remote-tool support.

The remaining problem is therefore not "design a WebSocket session system from
scratch". The remaining problem is **composition**:

1. turn the existing session host/gateway pieces into a real Cloud Home;
2. make the first-party TUI a client of that home when requested;
3. route local and remote Harw instances through one identity/session/job
   topology;
4. give Coding, Cloud Hub, Worker Hub, Container Hub and Compile Hub distinct
   service profiles instead of one process accumulating every capability;
5. preserve job/sandbox/authority invariants while sessions outlive clients.

This document is a source-of-truth refresh and implementation handoff. It does
not change runtime code.

## 1. CURRENT

### 1.1 Landed session-host core

The repository already has a transport-independent `SessionPort` and a
single-writer `harw-session-host`.

The host owns:

- hosted session records;
- cursor-based durable replay plus a bounded live ring;
- input arbitration with `expect_head`;
- idempotent `client_msg_id` handling;
- first-writer-wins approvals;
- bounded fan-out / lag handling;
- presence;
- revoke and drain semantics;
- restart recovery state.

The WebSocket layer is separate and drives that port rather than becoming the
session owner.

### 1.2 Landed gateway/tool plane

The repository also already has:

- `ToolPort` and `GatewayPort`;
- `tool.list/call/cancel`;
- `gateway.*` protocol vocabulary and capability gates;
- a host-side tool gateway;
- agent-principal delegation/narrowing;
- cascading revocation;
- remote-tool provider support;
- gateway health/log/diagnostic operations.

This is the correct base for a hub. A second "cloud API" must not reimplement
tool/session authority.

### 1.3 Still missing as a first-party user path

The important remaining gap is the client/composition side.

Current code contains `SessionPort`, host and WS dispatch, but there is no
first-party `harw attach` / `harw-session-remote` path that turns the normal
TUI into a thin client of a production hosted session.

The embedded TUI still owns its local `RuntimeAssembly` and turn loop.

That means:

- closing the embedded TUI is still not equivalent to merely detaching a
  client;
- cross-device continuation is not yet the default user path;
- the Cloud Home cannot yet be treated as the canonical interactive session
  owner by the normal TUI.

### 1.4 Existing job/worker foundations

The job stack already gives Harw a stronger execution primitive than a
session-local spawned task:

- durable job records;
- explicit ownership/lineage;
- cancellation;
- logs and progress;
- process identity/recovery mechanisms;
- restart/reclaim work in the durable job stack;
- compiled child agents can already be run through JobManager.

The on-demand worker plan additionally defines the intended activation rule:

- unused workers remain cold;
- boot is job-owned;
- concurrent first demand is single-flight;
- no hidden host fallback;
- node state gates activation;
- `idle_ttl = 0` is the high-assurance default.

The Cloud Home should reuse that instead of inventing a separate worker
supervisor.

## 2. Target mental model

Harw should distinguish **session ownership**, **cloud routing/policy** and
**execution placement**.

```text
                         authenticated clients
                   laptop / phone / pi / cli / web
                              │
                              ▼
                    ┌──────────────────────┐
                    │      Cloud Hub       │
                    │ identity + directory │
                    │ policy + routing     │
                    └──────────┬───────────┘
                               │
                SessionId -> owning SessionHost
                               │
              ┌────────────────┴─────────────────┐
              ▼                                  ▼
      Hosted Session A                    Hosted Session B
      single writer                       single writer
      replay/live state                   replay/live state
              │                                  │
              └──────────── intent ──────────────┘
                               │
                               ▼
                     durable job control plane
                               │
              ┌────────────────┼────────────────┐
              ▼                ▼                ▼
         Worker Hub       Container Hub     Compile Hub
         agent/process     Podman/cells      build/test/lint
```

The Gateway/Cloud Hub is the **hub**, but it is not a giant mutable god object.

It owns directory, identity, policy and routing. The session host remains the
single writer of each session. Jobs remain the execution/supervision primitive.

## 3. Stable identity vocabulary

Do not overload "root", "worker" or a display label as identity.

Keep distinct identifiers:

```text
TenantId
Principal / SecurityContext
DeviceId
NodeId
HarwInstanceId
SessionId
TurnId
Agent child SessionId
JobId
ContainerInstance / worker runtime identity
```

Routing by address is allowed. Authority by address is not.

### 3.1 Human/admin roles

Keep these independent:

**Global System Admin**
- verified system/operator identity;
- may administer machine-level Harw services according to policy;
- not equivalent to an agent role;
- not automatically equivalent to Cloud Admin.

**Global Cloud Admin**
- manages cloud-wide tenants/nodes/routing/policy;
- may be the same human account as Global System Admin, but the capabilities
  remain separately granted/auditable.

**Root agent**
- organizational/model role inside one agent tree;
- not a human/system administrator;
- never gains host/cloud admin merely because it is the root of its tree.

This separation must survive TUI labels, gateway APIs and config.

## 4. Deployment profiles

A deployment profile is a **composition profile**, not an agent permission
profile.

Each profile mounts only the services it needs.

### 4.1 Coding profile

Purpose:

- interactive development;
- local chat/work;
- code agents;
- workspace tools;
- optional local hosted sessions.

Typical services:

- TUI/attached TUI;
- provider clients;
- local SessionPort client;
- local JobManager/job client;
- development tool registry.

May request work from Worker/Container/Compile hubs but does not become those
hubs.

### 4.2 Cloud Hub profile

Purpose:

- Cloud Home coordination;
- authenticated node/device inventory;
- session directory;
- routing;
- tenant/policy administration;
- hosted-session discovery;
- job placement coordination.

Must not silently gain:

- unrestricted workspace write;
- generic host shell;
- container-engine socket access;
- compile-worker execution.

The hub routes requests to dedicated execution profiles.

### 4.3 Worker Hub profile

Purpose:

- constrained agent/process jobs;
- child-agent execution;
- model/tool worker jobs.

Default:

- cold until an admitted job needs it;
- explicit sandbox profile required;
- rights derived from job + caller + worker capability;
- no implicit host fallback.

### 4.4 Container Hub profile

Purpose:

- rootless container/cell lifecycle;
- isolated workloads;
- container placement and inspection.

Rules:

- rootless Podman by default;
- container uid 0 != host root;
- no engine socket exposed to untrusted cells;
- no `--privileged`;
- no capability widening;
- network deny unless explicitly admitted;
- boot/reuse follows job-owned worker lifecycle.

### 4.5 Compile Hub profile

Purpose:

- reproducible builds;
- tests;
- lint/static verification;
- artifact preparation;
- heavyweight build cache.

This deserves its own profile because compile workloads have different
scheduling/resource/cache semantics from generic workers.

Initial policy:

- compile requests become typed jobs;
- one workspace/verification lane may be exclusive when required;
- multiple unrelated workspaces may run concurrently subject to resource
  policy;
- build results are artifacts/results, never an implicit authority upgrade;
- a compile failure never causes silent execution elsewhere.

## 5. Cloud Home directory

The hub needs a small explicit routing directory.

Conceptually:

```rust
struct HostedSessionLocation {
    session: SessionId,
    tenant: TenantId,
    owner_node: NodeId,
    owner_instance: HarwInstanceId,
    host_epoch: u64,
    state: HostedState,
}
```

V1 does **not** live-migrate an active session.

It routes a client to the current owner.

Why:

- single-writer ownership remains simple;
- reconnect can be implemented before live migration;
- failover/restart can use host epochs and durable recovery;
- session migration can later become an explicit fenced transfer protocol
  instead of accidental dual ownership.

A stale location must fail closed and force directory refresh.

## 6. Multi-node topology for the first real deployment

The first useful target is deliberately small:

```text
Cloud Home
  ├─ Raspberry Pi node
  ├─ Cloud node A
  └─ Cloud node B
```

Each node registers:

- stable NodeId/device identity;
- authenticated transport identity;
- profile(s) it is allowed to serve;
- current lifecycle state;
- capacity hints;
- endpoint/routing metadata;
- policy/attestation epoch where required.

Do not infer node role from hostname.

### 6.1 Node lifecycle

At minimum:

```text
Pending
Active
Draining
Drained
Revoked
Offline
```

Only Active nodes receive new automatic placements.

Draining:

- no new sessions/jobs;
- existing bounded work may finish according to policy.

Revoked:

- new connections/admission fail;
- active delegated capability is revoked/cancelled as specified;
- node cannot self-reactivate.

## 7. Session UX: Continue vs New Session

This is not cosmetic. It defines whether session identity remains stable.

### Continue

"Continue" means:

1. choose an existing `SessionId`;
2. establish a new authenticated client connection;
3. derive capabilities again;
4. `session.attach(from = last_cursor)`;
5. replay durable state;
6. resync if generation/cursor requires it;
7. show current live agents/jobs;
8. if the host marks the session Interrupted and policy requires explicit
   resume, invoke the existing session-resume intent.

Continue does **not** create another root session.

### New Session

"New Session" means:

1. create a new `SessionId`;
2. leave the previous hosted session record intact;
3. do not stop the previous session's durable jobs merely because the client
   chose New;
4. attach the TUI to the new session.

The old session remains discoverable according to retention policy.

### TUI opening flow

Target first-party flow:

```text
harw
  │
  ├─ Cloud Home/session host reachable?
  │      │
  │      ├─ yes -> discover sessions
  │      │          ├─ Continue existing
  │      │          └─ New Session
  │      │
  │      └─ no -> explicit local embedded/offline path
  │
  └─ never silently convert a failed cloud attach into broader local authority
```

A fallback may exist, but it must be visible and must not silently widen
capabilities.

## 8. First-party attached TUI

The next major product slice is not another backend protocol. It is an attached
TUI loop over `SessionPort`.

The attached TUI should reuse rendering/input components but not own a
`RuntimeAssembly`.

```text
attached TUI
    │
    ├─ SessionPort
    ├─ FrameSource
    ├─ Tool/Gateway intents as admitted
    └─ local presentation state
```

It receives:

- transcript/history;
- turn deltas;
- approvals;
- session state;
- agent orchestration state;
- managed job state;
- presence/reconnect/resync state.

It sends typed intent:

- submit;
- interrupt;
- approval response;
- session controls;
- admitted operation/tool/gateway controls.

No client owns the host's session writer.

## 9. Operation and UI authority rule

The same architectural rule used by the local TUI should carry into Cloud Home:

**surface != authority owner**

A keyboard shortcut, web button or Telegram action can be a new input gesture.
It should map to an existing typed operation/session/tool/gateway intent rather
than call a manager directly.

Examples:

- job stop -> canonical job/operation control path;
- agent cancellation -> agent/session control path;
- gateway mutation -> GatewayPort capability + approval path;
- config mutation -> owning operation/service;
- UI-only focus/scroll -> local presentation state.

This prevents TUI/Web/Telegram/Gateway from becoming four authorization
implementations.

## 10. Rights inheritance across agents and jobs

The effective rights of a spawned child/job are an intersection, never a
union.

Conceptually:

```text
effective =
    caller ceiling
  ∩ agent/worker manifest
  ∩ deployment-profile capability
  ∩ node policy
  ∩ job specification
  ∩ sandbox grants
  ∩ current approval/policy state
```

A missing mandatory sandbox is a refusal, not a reason to run on the host.

A child job therefore carries enough typed context to reconstruct/audit:

- tenant;
- parent/root session lineage;
- agent identity/role where applicable;
- requested worker/runtime profile;
- sandbox/profile digest;
- job id;
- placement/node;
- effective authority summary.

Never derive those from display text.

## 11. Documents / chat / work transitions

The interactive modes should share a session/control substrate rather than
spawn unrelated runtimes.

### Chat

Optimized for:

- conversation;
- documents/context;
- lightweight tools;
- optional background jobs.

### Work

Optimized for:

- persistent task graph;
- agents;
- jobs;
- code/research/build actions;
- explicit artifacts/results.

### Shell

Operator-controlled shell interaction remains distinct from model-controlled
shell execution.

A mode transition must not recreate identity/authority accidentally.

The session remains the anchor; mode changes alter admitted behavior/tools
according to policy.

Document processing that requires workers becomes a job/worker placement,
rather than an implicit local process spawned by UI code.

## 12. Failure semantics

### Client disconnect

- detach client only;
- hosted turn/child/jobs continue if policy permits;
- reconnect/replay restores view.

### Cloud Hub disconnect from one node

- existing node-owned work does not gain new authority;
- routing marks location uncertain/offline as appropriate;
- no second session writer starts until fencing/recovery says it may.

### Worker unavailable

- job remains queued/fails according to placement policy;
- no hidden host fallback.

### Sandbox unavailable

- refuse start;
- report explicit reason.

### Provider unavailable

- session remains owned;
- turn reports provider/rate state;
- queued work does not duplicate due reconnect.

### Reconnect

- re-authenticate;
- re-derive capabilities;
- stale/revoked clients do not inherit old rights.

## 13. Implementation sequence

### H0 — status/source-of-truth cleanup

- update stale PL-68 status text so landed W00/R18 code is no longer described
  as absent;
- mark older gap-hunt observations as historical where later merged code
  superseded them;
- link this profile plan from PL-65/PL-68.

Exit: docs clearly distinguish landed core from missing composition.

### H1 — composition profile vocabulary

Add a typed deployment/service profile in an application/composition layer:

```text
Coding
CloudHub
WorkerHub
ContainerHub
CompileHub
```

It selects services and default capability ceilings. It does not replace
`EntryProfile`, `RegistryProfile` or agent IR; those remain runtime/agent
concerns.

Tests:

- every profile mounts only intended services;
- no profile silently gains another profile's privileged backend;
- CloudHub has no generic compile/container/host execution by default.

### H2 — production local SessionHost composition

Wire the already-landed session host/WS stack into a real daemon composition.

Deliver:

- local sessions socket;
- kernel-derived local identity;
- one process owns hosted session records;
- directory/health surface;
- drain/restart behavior;
- no TUI yet required.

### H3 — first-party session client

Create the missing client-side adapter (crate/name to be confirmed against
current architecture policy).

Deliver:

- UDS WebSocket client;
- `SessionPort` implementation;
- hello/correlation/frame demux;
- reconnect with cursor;
- bounded queues/backoff;
- no `harw-core` linkage in the thin client.

### H4 — attached TUI usable slice

Deliver:

- `harw attach --socket ...`;
- session picker;
- Continue;
- New Session;
- transcript replay/live tail;
- approvals;
- interrupt;
- model/mode/effort controls where already supported;
- agents/jobs projection from hosted state.

Acceptance:

- close terminal;
- hosted turn/job continues;
- reopen;
- Continue shows same SessionId and current state.

### H5 — Cloud Home directory + node registry

Deliver:

- session location directory;
- known-node registry for Pi + two cloud nodes;
- Active/Draining/Revoked admission;
- host epoch/stale-location handling;
- gateway status views.

No live migration yet.

### H6 — remote authenticated attach

Mount the same session service behind the authenticated node transport.

Deliver:

- remote client connection;
- mandatory tenant;
- node/device identity -> capabilities;
- no bearer/IP fallback;
- reconnect/cursor parity with local UDS.

Acceptance:

- start/continue on laptop;
- attach from another authenticated device;
- same SessionId;
- same hosted state;
- capability changes/revocation visible on reconnect.

### H7 — Worker/Container/Compile placement

Integrate the job-owned on-demand-worker lifecycle.

Deliver:

- WorkerHub placement;
- ContainerHub placement;
- CompileHub placement;
- single-flight boot;
- explicit worker/node capability matching;
- no hidden fallback;
- compile-lane policy;
- terminal job/agent correlation.

### H8 — global admin separation

Deliver explicit capabilities/audit for:

- Global System Admin;
- Global Cloud Admin;
- tenant admin/operator as needed.

Tests prove these are not implied by:

- root agent role;
- local uid alone unless policy maps it;
- container uid 0;
- gateway presence.

### H9 — client convergence

After attached-TUI behavior reaches parity:

- plain `harw` may prefer Cloud Home according to explicit config;
- embedded local runtime becomes an explicit local/offline mode;
- duplicate TUI turn-driving logic can be reduced;
- all frontends converge on the same hosted-session intents.

Do not do this before H4-H6 are stable.

## 14. What to build next

The shortest high-value path is:

```text
H0 docs/status
  ↓
H2 local host composition
  ↓
H3 UDS SessionPort client
  ↓
H4 attached TUI Continue/New
  ↓
H5 small Cloud Home directory
  ↓
H6 second-device / second-node attach
```

Worker/container/compile profile execution can progress in parallel after H1
because it builds on the job plane, not on TUI rendering.

This avoids spending another round redesigning protocols that already exist.

## 15. Verification

Implementation waves use the full repository sequence:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -q -p xtask -- gates
cargo deny check
make -C dod clippy test
actionlint
```

Security-sensitive waves additionally need explicit negative tests for:

- cross-tenant session access;
- stale/revoked device reconnect;
- capability widening;
- second-writer session ownership;
- job start without required sandbox;
- worker fallback to host;
- container-root -> host-root confusion;
- GlobalCloudAdmin/GlobalSystemAdmin/root-agent role confusion.

## 16. Acceptance criteria

The Cloud Home v2 slice is successful when:

1. the session host remains the single writer of a hosted session;
2. a TUI can detach and later continue the same SessionId;
3. New Session does not destroy the previous hosted session/jobs;
4. Pi and two cloud nodes can be represented as authenticated distinct nodes;
5. session routing uses stable identity/location, not IP as authority;
6. reconnect re-derives capabilities;
7. CloudHub does not accumulate Worker/Container/Compile authority;
8. WorkerHub, ContainerHub and CompileHub use job-owned execution;
9. missing sandbox/placement fails closed;
10. child/job rights only narrow;
11. job/agent control remains on canonical typed authority paths;
12. Global System Admin, Global Cloud Admin and root-agent are distinct roles;
13. the first-party attached TUI uses SessionPort rather than owning the hosted
    turn loop;
14. the implementation can later converge local `harw` onto the same client
    model without rewriting the control plane again.
