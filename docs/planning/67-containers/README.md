---
id: CONTAINERS
title: Optionales Container-System (Podman/Docker/Kubernetes als verwaltete Instanzen) + DoD-Container-Sensor
status: proposed
date: 2026-09-27
tags: [containers, oci, podman, docker, kubernetes, dod, placement, executor]
related:
  - ../66-placement/README.md
  - ../65-cloud-sessions/README.md
  - ../50-dod-integration/README.md
  - ../85-gap-hunt/patterns.md
  - ../../design/harw-dod-charter.md
---

> **Idee (Mia):** Die Ausführung wird als Container-System betrachtet, als
> optionales Feature:
> - Kubernetes, Docker und Podman werden zu Runtimes bzw. Nodes, die harw als
>   Instanzen verwaltet.
> - DoD bekommt ein eigenes Container-Crate.
>
> Entstanden aus dem Workflow `container-runtime-design`:
> - 3 Mapper haben den Code gelesen;
> - 2 Rechercheure haben die offiziellen Doku-Seiten von Podman, Docker und
>   Kubernetes ausgewertet;
> - ein Opus-Entwurf hat daraus diesen Vorschlag gemacht und dabei vier Fehler
>   aus den Vorlagen korrigiert.
>
> Kernaussage: Die Idee passt, mit einer Änderung. DoD bekommt einen reinen
> **Beobachter** (Ring D). Die Instanzverwaltung liegt in Executoren (OCI in
> Ring J, Kubernetes in Ring A wegen des TLS-Stacks). Ein gemeinsames
> Vokabular in Ring I verbindet beide.

# Design note: optional container system (Docker/Podman/Kubernetes as managed instances)

**Short answer.** The idea fits, with one change. Containers become new `Executor` implementations plus new placement offers, and the coordinator stays as it is. The "one DoD crate" part has to be split, because ring D may depend only on F/I/D (`xtask/arch-policy.toml:50`). So D can reach neither the executors (J) nor an API client (A). `harw-dod-container` can only be a host observer that reads cgroupfs and procfs.

The mapping/research input has four errors that this note corrects:
- The warden budget is **54**, not 12 (`xtask/src/gate_warden.rs:304`). "Twelve" was the count of direct dependencies (`:202-214`).
- The gate's command-line name is `warden-cbuild` (`xtask/src/gates.rs:160,171`). `warden-no-c-build` is only its report name (`gate_warden.rs:2003`). Running `gates warden-no-c-build` fails with "unknown gate".
- `ResourceRequest` already has `memory_max`, `cpu_weight`, `pids_max` and `wall_timeout` (`harw-job-core/src/spec.rs:188-200`).
- The proposed `LinuxSandboxBackend::Container` variant is the wrong shape (see §2).

## 1. Goal and non-goals
- **Goal:** harw starts, supervises, reattaches to and cleans up containers as job attempts. It exposes them as `RuntimeOffer`/`NodeOffer` values. DoD observes them and can act on them.
- **Off by default, zero cost:** everything sits behind Cargo features, following the bwrap precedent (`harw-job-runtime/Cargo.toml:14-17`, `harw-job/Cargo.toml:14`). With the features off, no container client code is compiled.
- **Non-goals:**
  - Not a general orchestrator: no services, deployments, image builds or registry hosting.
  - No container engine inside a sandboxed job body.
  - No remote DoD on Kubernetes nodes in v1.

## 2. Crates and rings

| Crate | Ring | Why |
|---|---|---|
| `harw-container-model` | I | Serde-only vocabulary: `InstanceRef`, `OwnerLabels`, `ImageDigest`, `EngineKind`. It is shared by J, A and D, because D cannot import J (`arch-policy.toml:46,50`). |
| `harw-job-executor-oci` | J | Implements `Executor` (`harw-job-runtime/src/coordinator/executor.rs:177`) for Podman and Docker. Its client is hyper over a Unix domain socket. hyper's lockfile dependencies contain no `-sys` crate (`Cargo.lock:4415-4432`), so `J.forbid_sys_crates` (`arch-policy.toml:49`) stays green. No TLS stack. Third-party types stay internal. A CLI fallback may only use a fixed trusted binary path, the same way bwrap works (`harw-job-executor-bwrap/src/executor.rs:83,95`). |
| `harw-job-executor-k8s` | **A** | It needs TLS to the API server. The lockfile entry for `rustls` lists `aws-lc-rs` (`Cargo.lock:6212-6222`), and the gate's dependency hull ignores features (`arch-policy.toml:60-61`). So `aws-lc-sys` would land in J's hull, and it is not on the allow list (`:63-76`). A may depend on anything (`:51`); `harw-provider-http` is the precedent (`:334-335`). |
| `harw-dod-container` | D | Sensor only. It reads cgroupfs and procfs through `harw-dod-readfs`. It must not use hyper or tower, because the gate forbids them for sensors (`xtask/src/gate_edges.rs:184-224`). |

**Wiring.** The container executors are separate `Executor` implementations. They are **not** a variant of `LinuxSandboxBackend`. `LinuxExecutor` spawns the process under a pidfd inside a cgroup that it manages itself (`harw-job-runtime/src/coordinator/linux.rs:111-131,162-168`). A container process is a child of the engine, so that model does not fit. `Coordinator<S, E>` is generic over the executor (`runner.rs:352`), and so is the facade builder (`harw-job/src/lib.rs:150`).

**Features:**
- `harw-job`: `oci = ["dep:harw-job-executor-oci"]`.
- The ring-A binary: `k8s`.

**Gate changes:**
- `arch-policy.toml` entries: layer I, layer J (next to `:167`), layer A, layer D.
- `SENSOR_CRATES` entry (`gate_edges.rs:99-114`).
- `CRATE_PRIVILEGE` row `Unprivileged` (`gate_privileges.rs:422`, same as `:430`).
- `warden-deps` and `warden-cbuild` are untouched: no warden code changes (§6).

**Config (proposal):**
- `[job.container]`: `enabled=false`, `engine="podman-rootless"`, `socket`, `allow_rootful_socket=false`, `image_policy="digest"`, `warm_pool.size=0`, `warm_pool.idle_ttl="15m"`, `gc_grace="10m"`.
- `[job.k8s]`: `namespace_prefix`, `runtime_class`, `in_cluster`.

## 3. Instance model
- **Identity:** `ContainerInstance{engine, container_id, created_at, image_digest}` and `PodInstance{namespace, pod_uid, container}`. Both are serde and satisfy the `Identity` bounds (`executor.rs:179`). A container ID survives a daemon restart, which a bare PID does not.
- **Lifecycle:** Requested → Created → Running → Exited → Reaped, plus Lost.
- **Ownership labels** are set at create time:
  - `harw.owner` (the `RunnerId`)
  - `harw.work_id`
  - `harw.attempt`
  - `harw.lease_epoch`
  - `harw.tenant`
  - `harw.profile`

  Secrets never go into labels or env.
- **Reattach:** `Coordinator::recover` (`runner.rs:581`) calls `probe`, which inspects the instance by ID and compares its labels with the persisted identity. On a mismatch the result is `Probe::Mismatch`, and the instance is never signalled (`executor.rs:160-170`). Each executor sets `RestartPolicy=no` and never `AutoRemove`, so the exit code survives a controller restart (https://docs.docker.com/reference/cli/docker/container/run/).
- **Garbage collection:** list instances by `harw.owner`. Remove an instance only if all of these hold:
  - its attempt record is terminal or missing,
  - it is older than `gc_grace`,
  - the caller holds the current lease epoch.

  Kubernetes additionally uses `ttlSecondsAfterFinished` (https://kubernetes.io/docs/concepts/workloads/controllers/ttlafterfinished/).
- **Warm pools:** a warm pool is reserved capacity keyed by (tenant, image digest, profile). Like prompt-cache warmth, it is only worth something inside one tenant (DEC-026, `docs/planning/66-placement/README.md:239`). Pool members are never shared across tenants or reused after a job. Idle members are charged to headroom (`README.md:139-140`), because Kubernetes reserves requests even when idle (https://kubernetes.io/docs/concepts/configuration/manage-resources-containers/).
- **Images:** only `name@sha256:…` references are accepted. A plain tag is refused (fail closed).

## 4. Executor mapping

| Trait method | OCI | Kubernetes |
|---|---|---|
| `start` (`executor.rs:190`) | Create a container with labels, resources and security options, then start it | Create a Job with one pod |
| `probe` (`:200`) | Inspect by ID and check labels | GET the pod, check `pod_uid` and labels |
| `reattach` (`:208`) | Attach to logs and wait | Watch from resourceVersion |
| `recorded_exit` (`:217`) | Read `State.ExitCode` | Read `terminated.exitCode` |
| `finished` (`:224`) | Remove the container | Delete, or leave it to the TTL controller |

- **Output:** the engine's log stream becomes `AttemptEvent` Stdout/Stderr, with bounded reads (M5).
- **Cancellation:** stop by ID and then kill; on Kubernetes, delete the pod.
- **Resources:** `ResourceRequest` (`spec.rs:188`) maps to `--memory`, `--pids-limit` and `--cpu-shares` locally, or to `resources.limits` on Kubernetes.
- **Enforcement:** the `SandboxReport` is built from the engine's **inspect read-back**, not from what was requested. `satisfies(Required)` (`enforcement.rs:132-135`) therefore keeps its meaning:
  - `filesystem`: read-only rootfs, only the workspace bound read-write, no host `/run` mount (`harw-job-executor-bwrap/src/executor.rs:16-19,48`).
  - `network`: `none`, or proxy-only.
  - Process: `no_new_privs` plus `capabilities` (drop ALL, not privileged).
  - `resource_limits`: the limits shown by inspect. If the engine or cgroup delegation silently dropped a limit, the state is `Partial` or `Unsupported`.
  - On Kubernetes, `network` is at most `Partial` unless an operator attests that the CNI plugin enforces NetworkPolicy (https://kubernetes.io/docs/concepts/services-networking/network-policies/).
  - The IR-versus-job-core `Required` translation from DEC-027 applies (`README.md:212`).
- **Secrets:** injected only after the claim, under the current lease epoch, as a tmpfs file mount. Scope is `SecretScope::Purposes` (`README.md:89`).
- **Egress:** need ∩ grant ∩ node reach (`README.md:208`), mapped to network `none` plus a relay (`harw-sandbox/src/bwrap.rs:23`).

## 5. Security
- **Rootless Podman is the default.** The Docker socket and a rootful Podman socket are root-equivalent (https://docs.docker.com/engine/security/protect-access/), so they are refused unless `allow_rootful_socket=true`. The repo already hides `/run` sockets from jobs for this reason (`harw-job-executor-bwrap/src/executor.rs:16-19`).
- **Always applied:** no `--privileged`, `--cap-drop ALL`, `no-new-privileges`, the default seccomp profile or a stricter one, read-only rootfs, network `none` by default.
- **Stronger isolation:** a gVisor or Kata `RuntimeClass` becomes a separate offer (https://kubernetes.io/docs/concepts/containers/runtime-class/).
- **Kubernetes:** one namespace per tenant, a default-deny NetworkPolicy, `automountServiceAccountToken: false`, and the Pod Security `restricted` standard (https://kubernetes.io/docs/concepts/security/pod-security-standards/).
- **Supply chain:** digest pinning is mandatory; signature verification is optional (DEC).
- **M1** (`docs/planning/85-gap-hunt/patterns.md:28-47`):
  - Default is deny; an unknown engine state returns `Unverifiable`.
  - TOCTOU: act only on the immutable container ID, never on a name.
  - Check the socket peer with `SO_PEERCRED` after connecting ("open-then-check") instead of stat-then-connect.
- **M5** (`:79-83`): cap the size and timeout of every API body, log stream and watch.
- **M6** (`:85-93`): log pumps and watches run as supervised tasks with an owner, and cancellation propagates to them.

## 6. DoD integration
- **The sensor** implements `Sensor` (`dod/crates/harw-dod-signals/src/sensor.rs:139`), following the `harw-dod-cgroup` pattern.
  - It enumerates `docker-<id>.scope`, `libpod-<id>.scope` and `kubepods*` scopes (`dod/crates/harw-dod-cgroup/src/lib.rs:17`).
  - It caps the number of scopes the way `MAX_CGROUPS` does (`sensor.rs:82`).
  - It reads `CapEff`, `NoNewPrivs`, `Seccomp` and the namespace IDs from `/proc/<pid>`.
- **It emits:**
  - privileged or full-capability containers,
  - seccomp off,
  - sharing the host network namespace,
  - instances without harw ownership.
- **New closed vocabulary:** one `Capability` variant (`dod/crates/harw-dod-cap/src/capability.rs:40`), plus EventKinds following the procedure in `docs/design/harw-dod-sensor-extensibility-plan.md`.
- **Ownership:** matching a finding to a harw instance needs an "expected instances" set as I-ring data (`harw-container-model`) in the sentinel configuration.
- **Flow:** sensor → sentinel → rules → escalate → warden. There is no sensor-to-warden edge (`gate_edges.rs:128-165`).
- **Actions:** a container's scope is a cgroup, so the existing `FreezeCgroup`, `IsolateNetwork` and `KillProcessTree` actions (`dod/crates/harw-dod-warden-proto/src/action.rs:100`) already apply. There is no new `WardenAction`, and the warden's dependency hull is unchanged.
- **Engine-level cleanup:** the executor sees the attempt as `Exited` or `Lost`, finalizes it, and garbage collection removes the container. This follows the rule in `dod/crates/harw-dod-warden/src/executor.rs:40-66` that real mechanisms stay outside the warden.

## 7. Placement integration
- **RuntimeOffer kinds:** `oci-podman-rootless`, `oci-podman-rootful`, `oci-docker`, `k8s-pod`, plus one offer per `RuntimeClass`. Each carries a per-dimension `EnforcementState` (`README.md:119`).
- **NodeOffer for a Kubernetes node pool:** capacity is what remains of the ResourceQuota, and freshness comes from a watch. Stale data fails closed (DEC-021).
- **Ledger:** warm pools and running instances reserve capacity. The placement lease TTL (DEC-024) and the job lease are separate.
- **Instance loss:** the attempt becomes `Lost`, the retry policy runs, and placement runs again without the lost offer.
- **Offer producers:** `harw-job-executor-oci/src/offer.rs` and `harw-job-executor-k8s/src/offer.rs`. They depend on placement P1 (`README.md:217-224`).

## 8. Roadmap (one agent per file)
- **C0:** text only. Add `docs/planning/67-containers/README.md` and the DEC notes.
- **C1:** `harw-container-model`: `instance.rs`, `labels.rs`, `digest.rs`, plus its `arch-policy.toml` entry.
  - Tests: serde round trips; a tag is refused; label parsing.
- **C2:** `harw-job-executor-oci`: `client.rs`, `executor.rs`, `enforcement.rs`, plus the `harw-job` feature. Depends on C1.
  - Tests: a fake engine over a Unix socket in a temp dir; a label mismatch gives `Mismatch`; the read-back reports `Partial`.
- **C3:** `gc.rs` and `pool.rs` in the OCI crate. Depends on C2.
  - Tests: epoch fencing; no reuse across tenants.
- **C4:** `harw-dod-container` plus the Capability variant, the `SENSOR_CRATES` entry and the `CRATE_PRIVILEGE` row. Depends on C1.
  - Tests: a fixture cgroup tree; the cap is enforced.
- **C5:** `harw-job-executor-k8s` (ring A). Depends on C1.
- **C6:** offer producers. Depends on placement P1, C2 and C5.

**Open decisions (numbering continues after DEC-027, `README.md:231-240`):**
- **DEC-028:** container executors are separate `Executor` implementations, not `LinuxSandboxBackend` variants. Composition happens through `harw-job` features or the A binary. Also decide: one coordinator per executor, or an enum executor.
- **DEC-029:** OCI goes in J (Unix socket, no TLS); Kubernetes goes in A (the rustls hull).
- **DEC-030:** rootless is the default; rootful needs an explicit opt-in.
- **DEC-031:** enforcement comes from the read-back; Kubernetes network is `Partial` without attestation.
- **DEC-032:** digests are mandatory; signature verification is optional.
- **DEC-033:** DoD gets a new Capability and no new WardenAction.
- **DEC-034:** use the name "ContainerInstance" to avoid a clash with the "host container" of 65-cloud-sessions (`docs/planning/65-cloud-sessions/README.md:47,392,405`).
- **DEC-035:** warm pools are keyed by tenant, digest and profile, and charged to the ledger.

**Tests added:** none (read-only mapping). **Central build:** not needed, because nothing was changed. When C1–C6 land, run the full sequence from `CLAUDE.md`, including `cargo run -q -p xtask -- gates` with all gates (`edges privileges warden-deps warden-cbuild arch`).