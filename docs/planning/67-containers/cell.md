---
id: CONTAINERS-CELL
title: "Mode B: harw cell (harw in a container with locally compiled worker agents)"
status: proposed
date: 2026-09-27
tags: [containers, cell, oci, placement, workers, dod]
related:
  - README.md
  - ../66-placement/README.md
  - ../65-cloud-sessions/README.md
  - ../70-decisions/DEC-003-provider-limits.md
  - ../../design/agent-definition-dsl.md
---

> **Idea (Mia):** The container system takes over container management.
> harw itself runs in the container as a small unit with a bunch of locally
> resident, compiled worker agents, created per job and cleaned up afterwards.
> Cells, families and clans were part of the plan from the start.
>
> This sheet extends Mode A from `README.md` (the host manages containers as
> job attempts) with Mode B (a cell is one of those containers). The
> reconciliation with family/clan/cell/organization from
> `agent-definition-dsl.md` §14–15 and gap G3 follows as its own section.

# Mode B: harw cell

**Answer.** The idea fits as a second mode on top of Mode A. In Mode A (67 §1–§8), the host harw starts, supervises and reaps containers as job attempts (`67-containers/README.md:44`). In Mode B, one of those ContainerInstances is a **cell**: a harw job worker and work driver, plus N compiled worker agents running as local processes. The cell is created for one job and removed once its results are exported. None of this exists yet:
- The repo has no Containerfile or Dockerfile.
- The crates `harw-container-model`, `harw-job-executor-oci`, `harw-placement` and `harw-dod-container` do not exist.
- No code mentions "cell".

Two binding rules shape the design:
- **DEC-003:** one provider client per process, and no worker may open its own (`DEC-003-provider-limits.md`, "Folgen").
- **DEC-004:** workers never build.

## B.1 Model
- **What a cell is:** a `ContainerInstance` carrying the extra label `harw.kind=cell` next to the labels in 67 §3 (`:79-86`).
- **Lifecycle:** create → claim (secrets as a tmpfs file) → run → export → drain → executor removes it. This maps onto Requested→…→Reaped (`:78`).
- **Process tree:** init as PID 1 → `harw cell run` → work driver → N worker processes.
- **`harw cell run` reuses the `harw serve` composition:** the job worker runs on its own runtime (`harw-cli/src/lib.rs:1383`) with a 10 s shutdown grace (`:1435`).
- **The gap:** all of today's workers run in-process. `JobTurnSpawner` shares one provider in the same process (`harw-cli/src/job_worker_work_driver.rs:60-62,2508-2524`). The only out-of-process spawner relaunches `current_exe()` (`harw-agent-runner/src/job_child_backend.rs:70-84`). Its protocol is private and assumes the same executable (`child_protocol.rs:24-27`), with an exact version check (`:514`). So sibling worker binaries need a new spawner (B2). A cell with zero worker binaries, using only in-process workers, stays valid as a fallback.
- **Relation to Mode A:** the host's OCI executor starts the cell as one job attempt. Placement sees an `oci-cell` RuntimeOffer, in addition to the kinds in `67:151`. The host sees the cell's internal fan-out only through leases (B.4).

## B.2 Image

| Stage | Content |
|---|---|
| Builder | Toolchain, `cargo fetch --locked`, a cargo-chef layer cache, and `harw agent build <role> --native` per worker role. This produces the Runner flavor (`harw-agent-compiler/src/backend/native.rs:20-31,112-151`) with the artifact embedded via `include_bytes!` (`:166`). That step runs `cargo build --release` (`:410-481`), so it is a build. It happens only here or in a verify cell. |
| Worker-cell runtime | harw, `/opt/harw/agents/<role>` binaries (digests go into the cell spec), tini, CA certs. No cargo, no rustc, no bwrap by default (B.3). Base image distroless or debian-slim, pinned by digest. |
| Verify cell | Toolchain, the workspace, and a target-cache volume. At most one per workspace: the ledger's `BuildSlot` capacity is 1 (`66-placement/README.md:203`), and `.harw/verify.lock` still applies (`DEC-004-no-parallel-builds.md:45`). Build settings come from `CLAUDE.md` and `.github/workflows/copilot-setup-steps.yml`. |

- The cell image is referenced only as `name@sha256:…` (`67:96`). Signatures and an SBOM attestation are optional (DEC-032).
- Warm pools are keyed by (tenant, digest, profile) (`67:95`, DEC-035).

## B.3 Inner isolation
- **Outer boundary:** the container itself, with the §5 options unchanged: rootless, `--cap-drop ALL`, `no-new-privileges`, read-only rootfs, network `none` (`67:122-123`).
- **Inner layer per worker, default:** the Landlock trampoline (`harw-job-exec/src/lib.rs:27`, `trampoline.rs:188`). The crate has no `unshare` call, so it works without user namespaces.
- **Why bwrap is not the default:**
  - bwrap always passes `--unshare-all --unshare-net`, plus `--unshare-user` when it sets uid/gid (`harw-sandbox/src/bwrap.rs:464-481`).
  - It also mounts a fresh `--proc` (`:490`).
  - Docker's default seccomp profile blocks unprivileged `CLONE_NEWUSER`. This comes from the research input (docs.docker.com seccomp page) and was not re-fetched here.
  - Therefore bwrap is opt-in only, through a narrow seccomp profile that allows only `CLONE_NEWUSER`; never `--privileged` or `seccomp=unconfined`.
  - The bwrap binary ships only with that profile. Otherwise the host probe would over-claim, because it counts bwrap whenever the binary exists and user namespaces are not known to be off (`harw-job-runtime/src/host.rs:257`).
- **Enforcement has two scopes:**
  - **Boundary (host ↔ cell):** taken from the OCI inspect read-back (`67:111-117`). Placement admits a cell only if the read-back meets every `Required` in the union of its agents' requirements (`66:103`).
  - **Inner (worker ↔ worker and ↔ orchestrator secrets):** every worker still runs `admission::check` (`harw-agent-runner/src/admission.rs:130`, `lib.rs:302`) against the probe taken inside the cell (`host.rs:222-268`).
  - `Required` stays never-overridable (`admission.rs:13,16`).
  - The launcher passes `--allow-degraded` (`args.rs:104`) only if the grant allows a BestEffort dimension to degrade. It is never baked into the image.
  - **Rule:** the outer boundary protects the host. It never upgrades an inner `Required`.
- **Per-worker limits:**
  - Enforced only with a writable, delegated cgroup v2 subtree (`host.rs:223`). That means rootless Podman with `--cgroupns=private` plus `Delegate=` on the host.
  - Default: the engine enforces cell-level limits (read-back), and per-worker limits are `Partial` (rlimits only). `kernel.cgroup_v2` is then admitted only as degraded.

## B.4 Shared limits across cells
- **Why a shared authority is needed:** account and gateway limits apply per account, but harw's buckets are per process (`75-harness-patterns/cost-model.md:276-281`). Workers AI frontier models allow 20 or 50 requests per minute per model and account (`gateway-contract.md:50-51`). N cells are N processes, so they must share through the ledger (DEC-023, `66:236`).
- **Inside a cell there is exactly one provider client: the orchestrator.**
  - Worker binaries reach the model only through a cell-local model relay over a Unix socket, and they hold no API key.
  - The runner builds its provider through `harw_runtime::EmbeddedAgent` (`harw-agent-runner/src/lib.rs:16,64`). **Not verified:** whether a relay `base_url` fits that path. B3 must check this.
- **Across cells:**
  - The ledger grants each cell a lease of k provider slots, and the cell's `max_concurrency` is k.
  - `effective_parallel` reads it (`66:195`), and the existing reserved slot still applies (`job_worker_work_driver.rs:164,377`).
  - Lease TTL follows DEC-024. Losing the lease means drain, then re-place (`66:150-155`).
  - The own Cloudflare Worker gateway is only a backstop: a 429 there means "wait" (`gateway-contract.md:57`). harw never auto-shards across several gateways (`cost-model.md:281`).
- **Cache affinity:** one cell uses one prefix-group key, `x-harw-cache-affinity` (`gateway-contract.md:60-66`), and only within one tenant (`66:209`). Placement scores cache warmth by prefix group (`66:268-276`). The header is planned only: grep finds no code for it.

## B.5 State and cleanup

| Where | What |
|---|---|
| Outside (host) | The job store holding the cell attempt: fs4 with a fenced epoch (`harw-session-store/src/job_store.rs:14,528`), on a local volume with a single writer, never NFS (`65-cloud-sessions/README.md:392,422`). Transcripts and approvals follow 65 §6 (`:390-396`). The KEK is never on a volume. The AuthHub socket `/run/harw/infra/secure.sock` (`harw-auth-hub/src/config.rs:72`, `SO_PEERCRED` check in `auth.rs:3`) stays on the host and is not mounted into cells (`67:112,122`). |
| Injected | Secrets are resolved on the host after the claim and injected as a tmpfs file (`67:118`, `66:207`). |
| Inside (per attempt) | An ephemeral `HARW_HOME`, plus the inner job store for durable worker sessions on a per-attempt local volume. Sockets and live state live on tmpfs. |
| Export | Results go to an `/out` volume or are pushed to a git branch before exit. The exit code is the job outcome. |

- **Do not use `docker run --rm`.** Mode A requires `RestartPolicy=no` and never `AutoRemove`, so the exit code survives a controller restart (`67:88`). The executor removes the cell after reading the exit code (`67:106`). Orphans are garbage-collected by labels, the `gc_grace` rule and the lease epoch (`67:89-92`).
- **Init:** `--init` or tini as PID 1 reaps worker zombies and forwards SIGTERM.
- **SIGTERM:** drain as in 65 §6 (`:398-404`): stop the current wave, interrupt turns, flush, export partial results, and exit within the grace period.
- **Crash:** the attempt becomes `Lost`, the retry policy runs, and placement re-places without that offer (`67:154`). Inner state survives only if the per-attempt volume can be reattached; otherwise the job restarts from the last export.

## B.6 Security
- Rootless is the default (DEC-030).
- No Docker or Podman socket inside a cell, because it is root-equivalent (`67:122`). Cells never start containers themselves, which extends the non-goal at `67:48`.
- Network is `none`. Egress goes only through a relay to the gateway (`67:119`), following the relay precedent in `bwrap.rs:23-26`.
- Read-only rootfs. Only `/workspace`, `/out`, `/tmp` (tmpfs) and the per-attempt state are writable.
- The API key lives only in the orchestrator's tmpfs file, never in labels or env (`67:87`).
- DoD:
  - `harw-dod-container` (C4) sees a cell as one `libpod-<id>.scope` (`67:136`).
  - Findings per worker need delegated sub-cgroups.
  - `harw.kind=cell` goes into the expected-instances set (`67:145`).
  - `FreezeCgroup` and `KillProcessTree` act on the whole cell (`67:147`).

## B.7 Roadmap (one agent per file; no new ring)

| Round | File (ring) | Depends | Tests |
|---|---|---|---|
| B0 | This section, DEC-036–041, and DEC-010 if not yet landed (`65:591`) | – | – |
| B1 | `harw-container-model/src/cell.rs` (I) | C1 | serde round trip; missing digest or plain tag refused |
| B2a | `harw-agent-runner/src/job_child_backend.rs`: `BinaryPathSpawner` (A) | – | wrong digest refused; protocol mismatch refused |
| B2b | `harw-cli/src/cell_spawner.rs`: `WorkerSpawner` implementation (A) | B2a | fake binary; wave width ≤ lease |
| B3 | `harw-cli/src/cell_model_relay.rs` (A) | – | N clients never exceed `max_concurrency`; 429 passed on as wait; bounded frames (M5) |
| B4 | `harw-cli/src/cell.rs`: `harw cell run` (A) | B2b, B3 (stub ledger; P4 for real leases) | SIGTERM drain; export before exit |
| B5 | `harw-job-executor-oci/src/cell.rs` + `offer.rs` (J) | C2, C3, B1; offers also need P1 | two-scope enforcement; no host `/run` mounts |
| B6 | `deploy/cell/{Containerfile,verify.Containerfile,seccomp-userns.json}`, `examples/cells/…` | B4, B5, C4 | – |

**Combined order:**
- **W0:** C0, P0, B0.
- **W1:** C1, P1, B2a, B3.
- **W2:** B1, C2, C4, C5 (all after C1); P2, P3 (after P1); B2b.
- **W3:** C3 (after C2); P4 (after P2 and P3); B4.
- **W4:** P5 (after P4); C6 (after P1, C2, C5); B5.
- **W5:** B6; P6 (any time after P1); P7 (after 65 RS2 and RS3).
- **Critical path:** C1→C2→C3→B5→B6. P1→P2/P3→P4 feeds B4's real ledger.

**Open decisions (after DEC-035):**
- **DEC-036:** "Cell" is a ContainerInstance with `harw.kind=cell`. It is distinct from the 65 host container and extends DEC-034.
- **DEC-037:** one provider client per cell. Workers use the relay only and hold no keys. Extends DEC-003.
- **DEC-038:** provider slots across cells are ledger leases. The gateway is a backstop. One prefix-group key per cell. Extends DEC-023 and DEC-026.
- **DEC-039:** enforcement has two scopes. The boundary never upgrades an inner `Required`. bwrap in a cell only with the narrow user-namespace seccomp profile. Extends DEC-027 and DEC-031.
- **DEC-040:** worker cells carry no toolchain. Native builds happen only in the image build stage or in a verify cell that holds the `BuildSlot`. Extends DEC-004.
- **DEC-041:** cell state follows 65 §6; no `AutoRemove`; results go to `/out` or git. Requires DEC-010.

## B.8 Reconciliation with family, clan, cell, organization (DSL §14–15)

(`dsl.md` = `docs/design/agent-definition-dsl.md`)

**Answer.** The DSL cell and the container cell from B.1 are not the same thing.
- **DSL cell (`[[cells]]`):** a logical, run-local fan-out wave over plan nodes with separated write scopes (`harw-agent-dsl/src/organization.rs:303-420`, invariant 11 `dsl.md:1401`). It is implemented. It is used through `CellPlan::from_cell_nodes` (`harw-plan-bridge/src/cells.rs:176-229`), today only by `/analyze` (`harw-ops/src/analyze.rs:1642`).
- **Container cell from B.1:** a runtime shell, that is, a `ContainerInstance` with `harw.kind=cell`. The statement in B.1 that no code mentions "cell" holds only in this sense.

G3 must be closed before B2b and B4. Today's disjointness relation does not detect overlaps involving globs and is fail-open on invalid paths. It also lives in ring A, where the DSL (C) and the executor (J) cannot reach it.

### B.8.1 One vocabulary (DEC-042)
- **Cell** (without qualifier) is the DSL cell. It determines *who* works *concurrently* on *which disjoint* write scopes.
- **Cell host** is the Mode B container. It is *one possible runtime placement* of clan waves and carries `harw.kind=cellhost`.
  - This changes only the wording of DEC-036, not its content.
  - The file names in B.7 follow this decision.

| DSL | Placement unit | Container | Ledger lease | Write partition | Provider client (DEC-003/037) |
|---|---|---|---|---|---|
| Organization (one root, `organization.rs:11`) | none; only narrows the `PlacementGrant` (`66:82,104-107`) | none; the root stays on the host (inv. 13, `dsl.md:1403`) | parent lease of the run; clans draw child leases (`66:203`) | clan selectors pairwise disjoint | host |
| Family (`family.rs`) | none | roster → role binaries in the image (`dsl.md:913-923`, B.2) | – | invariant `disjoint_write_sets` (`dsl.md:932`); today a free string (`family.rs:131`) that no code evaluates | – |
| Clan (`plan_scope`, leader) | **yes**: the leader plus its waves | by default 1 cell host per clan | one lease per cell host with k slots (B.4) | clan `WriteScope` ⊆ clan selector | exactly one: the leader in the cell host |
| Cell | part of the clan unit; the width unit is the `CellStage` (`cells.rs:320-325`) | runs entirely inside one cell host | stage width ≤ k − reserve | `DisjointSet` per stage | – |
| Worker | no | one process | shares the slots | own `WriteScope`, applied as Landlock `read_write` | relay |

### B.8.2 G3: gap, ownership, order

**Origin.**
- G3 is in `docs/design/hardening-gap-analysis.md:79-95` (priority P3, `:393`).
- The "borrow checker" idea comes from `docs/design/agent-ir-v1.md:118-132`, which also proposes the `PartitionWriteSets` pass (`:106`).
- The evidence `ids.rs:163 PathOrSymbol(String)` in the gap analysis is outdated. Today it is an enum with `Path` and `Symbol` (`harw-plan/src/ids.rs:318-333`).

**State of the code:**
- **Organization:**
  - `plan_scope` and `members_from_plan` are strings (`organization.rs:181,350`).
  - `resolve_organization` does not check for overlap (`:680-706`). The test at `:973-986` resolves `*-synthesis` and `security-*` without error.
  - The default organization contains exactly this pair (`harw-registry-defaults/agents/organization/default.toml:81,108`). A node `security-synthesis` thus belongs to two clans.
- **Partition:**
  - It is greedy (`harw-plan/src/graph.rs:332-351`) and decides via `write_scopes_conflict`. That is a pure prefix relation (`harw-plan/src/admission.rs:791-803`).
  - **Globs are missed:** `src/*.rs` and `src/lib.rs` count as disjoint. Admission, however, turns `src/*.rs` into a glob rule (`admission.rs:608-609`) that permits `src/lib.rs`.
  - **Fail-open:** if a path cannot be normalized, the result is "no conflict" (`:792-794`). The test at `:1283` pins this behavior.
- **Second, independent relation:** `paths_overlap` checks `WorkScope.owned_paths: Vec<String>` (`harw-plan-bridge/src/work_driver.rs:170-180,1272-1294`). `WorkerRequest.owned_paths` is untyped as well (`harw-cli/src/job_worker_work_driver.rs:418`).
- **Enforcement only after the fact:**
  - It runs via snapshot and diff; there is "no path-precise sandbox" (`job_worker_work_driver.rs:46-58`).
  - Attribution is by ownership, not by writer (`:1961-1990`). If a worker writes into a sibling's path, this goes unnoticed.
- **IR:** it only knows `filesystem_write: bool` (`ir_v2.rs:1074`). There are no diagnostic codes for ORG (`diagnostics.rs:101`).
- **Precedent:** `AuthorityCeiling::is_disjoint_from` (`harw-agent-dsl/src/authority.rs:155`, used in `family.rs:1536`).

**Ownership.**
- `harw-plan` is in ring A (`xtask/arch-policy.toml:333-334`), `harw-agent-dsl` in C (`:105`), and C may only point to F, I and C (`:47`). The algebra therefore belongs in ring I, as `harw-authority/src/write_scope.rs` (`:113-114`).
- There it sits next to `NetworkScope`, which already has `intersection` and `is_subset_of` (`harw-authority/src/lib.rs:320-402`).
- The DSL already reaches `harw-authority` through `harw-context` (`harw-agent-dsl/Cargo.toml:13`, `harw-context/Cargo.toml:12`). Rings J, D and A may use I anyway (`:46,50,51`).

**Why before B2b/B4.**
- In the cell host, N worker processes share one `/workspace` (B.5).
- The only hard barrier is Landlock per process (B.3). Its policy knows only path hierarchies, no globs (`harw-job-linux/src/sandbox.rs:60-61`).
- Disjointness must therefore be decided on exactly this form. Otherwise B2b silently grants overlapping write permissions, and B4 exports a mixed-up state.

**Type sketch (DEC-043):**
```rust
// harw-authority/src/write_scope.rs (Ring I)
pub struct RepoPath(String);                             // normalized; `..`/absolute/empty ⇒ Err
pub enum WriteRoot { File(RepoPath), Subtree(RepoPath) } // Landlock-capable, no globs
pub struct WriteScope(BTreeSet<WriteRoot>);              // empty = read-only
impl WriteScope {
    pub fn parse(e: &[&str]) -> Result<Self, ScopeError>;    // glob ⇒ ScopeError::Glob
    pub fn intersects(&self, o: &Self) -> bool;
    pub fn is_subset_of(&self, ceiling: &Self) -> bool;
    pub fn roots(&self) -> impl Iterator<Item = &WriteRoot>; // → FilesystemPolicy.read_write
}
pub struct DisjointSet<K>(Vec<(K, WriteScope)>);         // proof token, only via prove
impl<K: Ord + Clone> DisjointSet<K> {
    pub fn prove(v: Vec<(K, WriteScope)>) -> Result<Self, Overlap<K>>;
}
pub enum PlanSelector { Exact(String), Prefix(String), Glob(String) } // selection, not a write permission
impl PlanSelector { pub fn may_overlap(&self, o: &Self) -> bool; }     // undecidable ⇒ true
```

**Checked in three places (DEC-044):**
1. DSL validation checks the clan selectors pairwise (new `HARW-ORG` code).
2. In the partition, `CellPlan.batches` becomes `Vec<DisjointSet<TaskId>>`.
3. Cell host admission checks before every stage, against the stage and against the running workers (`work_driver.rs:1296`). After that, `roots()` serves as the Landlock rule.

### B.8.3 Conflicts between B.1–B.7 and §14–15
1. **Name:** see B.8.1.
2. **Fan-out width (DEC-046):**
   - Today three places compute the width differently:
     - DSL: `min(Batch, max_parallel)` (`cells.rs:366-381`).
     - Work driver: `min(spec, cap−1)` (`job_worker_work_driver.rs:374-380`).
     - Cell host: k from the lease (B.4).
   - **Rule:** width = min(partition batch, `max_parallel_workers`, k − `RESERVED_PROVIDER_SLOTS`).
   - The DSL limits *who* runs together, the ledger limits *how many*. Neither side widens the other.
3. **Where `required` applies (DEC-044):**
   - Today only in `cells.rs:200-209` and only for `/analyze`. The work driver uses no org cells.
   - If the organization or the cell is missing, the wave runs undivided (`cells.rs:51-55`, `analyze.rs:1561-1572`). This is fail-open.
   - B.1–B.7 do not mention `write_partition`.
   - **Rule:** in the cell host, `advisory`/`none` count as `required` as soon as more than one member writes. There is no fallback to the undivided wave there.
4. **Clans and cell hosts (DEC-045):**
   - The DSL sees all clans in *one* root run (`dsl.md:1036-1038`). B.1 sees one container per job and says nothing about clans.
   - **Rule:**
     - The default is one cell host per clan.
     - The leader, a child orchestrator (`default.toml:30-34`), is the only provider client there.
     - A clan never runs across several cell hosts on the same workspace.
     - The roles in the image come from the family roster.
   - **Finding:** for the security family, `[orchestrators].allowed` is empty (`default.toml:97-99`).
5. **Organization and placement policy (DEC-047):**
   - An organization is frozen as an "organization revision" (`dsl.md:1226`).
   - It only narrows the grant (`meet`, inv. 7 `dsl.md:1397`, DEC-022) and carries no topology (DEC-020). `child_depth_cost` is already such a narrowing (`organization.rs:195-236`).
   - Leases and fencing belong to the runtime (inv. 24, `dsl.md:1414`). This fits DEC-038.

### B.8.4 Joint roadmap (one file per step)

| Step | File (ring) | Depends | Test |
|---|---|---|---|
| G0 | this section, DEC-042–047 | – | – |
| G1 | `harw-authority/src/write_scope.rs` (I) | – | property: `prove` ok ⇒ every path in ≤1 scope; glob/`..`/absolute rejected; serde |
| G2 | `harw-plan/src/admission.rs` (A): `conflicts` delegates to `WriteScope`; glob or invalid path ⇒ conflict | G1 | `src/*.rs` against `src/lib.rs` ⇒ conflict; flip `admission.rs:1283` |
| G3 | `harw-plan-bridge/src/work_driver.rs` (A) | G1 | cases `:2036-2041` stay green |
| G4 | `harw-agent-dsl/src/organization.rs` (C) + ORG code | G1 | overlapping clans ⇒ error |
| G5 | `harw-registry-defaults/agents/organization/default.toml` (A), in the same round as G4 | G4 | default organization resolves |
| G6 | `harw-plan-bridge/src/cells.rs` (A) | G2 | glob nodes never share a batch; no undivided fallback for writers |
| B1 | `harw-container-model/src/cell.rs` (I) + `write_scope` | C1, G1 | scope ⊆ clan selector |
| B2b | `harw-cli/src/cell_spawner.rs` (A) | B2a, G6 | fake binary cannot write into a sibling's path |
| B4 | `harw-cli/src/cell.rs` (A) | B2b, B3, G6 | overlapping stage is rejected before the spawn |
| B5 | `harw-job-executor-oci/src/cell.rs` (J) | C2, C3, B1 | read-back: only scope roots are `rw` |
| P1 | `harw-placement-model/src/requirements.rs` (I): `WriteNeed` carries `WriteScope` (66) | G1 | `meet` never widens |
| P4 | `harw-placement/src/lease.rs` (A): overlapping workspace claims ⇒ `CapacityShort` | P2, P3, G1 | two claims |

**Independent of G3:** C0–C6, P0, P2, P3, P5–P7, B0, B2a and B3. B6 depends on it only transitively.

**Waves:**
- W0: G0, C0, P0, B0
- W1: G1, C1, B2a, B3
- W2: G2, G3, G4, G5, P1, B1, C2, C4, C5
- W3: G6, P2, P3, C3
- W4: B2b, P4
- W5: B4, P5, C6
- W6: B5, P6
- W7: B6, P7

**Critical path:** G1→G2→G6→B2b→B4→B6, in parallel C1→C2→C3→B5.

### Open decisions (B.8)
- **DEC-042:** Is it called "cell host" (`harw.kind=cellhost`), or does it stay "container cell" with a mandatory qualifier?
- **DEC-043:** Does the algebra go as a module into `harw-authority` or into its own I crate `harw-scope`? Are globs in `write_scope` forbidden, or conservatively treated as a conflict?
- **DEC-044:** Symbol scopes stay file-wide (`ids.rs:283-286`). If Landlock is missing in the cell host: only one writer per stage?
- **DEC-045:** Several cell hosts on one workspace: disjoint `rw` mounts on one volume, or one clone per cell host with a merge barrier? For the default organization: rename `*-synthesis` to `synthesis-*` (this affects the node names in `/analyze`, `default.toml:19-20`) or introduce a precedence rule?
- **DEC-046:** Does the leader turn count against k, or does it get its own slot?
- **DEC-047:** Do `[clans.placement]` hints (narrowing only, ⊆ grant) arrive together with P6 or later?
