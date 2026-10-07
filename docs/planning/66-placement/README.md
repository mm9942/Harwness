---
id: PLACEMENT
title: Placement engine — requirements → node + provider + runtime
status: proposed
date: 2026-09-27
tags: [placement, scheduling, providers, nodes, cost, cache]
related:
  - ../70-decisions/DEC-003-provider-limits.md
  - ../65-cloud-sessions/README.md
  - ../75-harness-patterns/gateway-contract.md
  - ../75-harness-patterns/claude-code.md
  - ../85-gap-hunt/patterns.md
---

# Placement engine

Starting picture (Mia):

```
Agent requirement
- model capability · context size · network allowed? · secrets allowed?
- write access? · CPU/RAM · latency class · cost ceiling · provider constraints
        ↓
  placement engine
        ↓
Node + Provider + Runtime
```

This came out of a workflow: three mappers read the code (read-only,
with `file:line` references), then a design agent designed the engine.
The additions from the session are at the end.

## Status today per dimension

| Dimension | Status | Where decided today |
|---|---|---|
| Write access (filesystem) | partial | Compile time: DeriveRequirements pass (harw-agent-compiler/src/passes/requirements.rs:52-66) fixes the required sandbox level into the IR. Run time: harw-agent-… |
| Network allowed | partial | Manifest requirement fixed at compile time (DeriveRequirements); process-level network confinement checked at admission time against the single local host; per-… |
| CPU / RAM (resources) | partial | Enforcement decision is made at job-start by the coordinator (harw-job-runtime/src/coordinator/linux.rs:315-317), from whatever ResourceRequest the JobSpec's bu… |
| Node selection (which machine runs the agent) | missing | Today a human operator (or whoever runs `harw gateway` / provisions the cloud host) picks the node at deployment/pairing time. No code enumerates candidate node… |
| Runtime selection (execution backend once a host is fixed) | partial | In-process, at start-up, on whichever single machine is currently running the runner/coordinator: HostReport::probe() (host.rs:125-135) always reads std::env::c… |
| Model capability | partial | Nowhere at dispatch time. grep shows zero production consumers of ModelCapabilities/bootstrap_descriptors outside harw-model-catalog itself (only harw-knowledge… |
| Context size | present | Per turn, inside harw-runtime/src/assembly.rs, after a model is already chosen — feeds ModelRequest::with_context_budget/with_context_program in harw-core/src/m… |
| Latency class | missing | Nowhere as a placement input. FAST_MODEL_HINTS is scoped only to InternalModelPoint::AutoClassifier/WorkDriverJudge (internal_models.rs:60-72,338-356 in role_mo… |
| Cost ceiling | missing | RateLimitToml (harw-config/src/provider_toml.rs:465-511) and ProviderToml.max_concurrency (41-59) govern throughput/quota, not $ spend. DEC-003 (docs/planning/7… |
| Provider constraints | present | At config load/validation (harw-config), at router construction (RoutingModelProvider::new/with_unavailable, routing.rs:132-200), and continuously per request/w… |
| Provider side of Node+Provider+Runtime output | partial | Provider is fixed either explicitly (ModelRequest.provider_id via with_provider_id, harw-core/src/model.rs:396-399) or through the config resolution chain (Mode… |
| Secrets allowed (KMS/DEK access itself) | partial | Compile/config time: harw-auth-hub's TOML (peers by uid + grants) builds a static, per-process NamespacePolicy at startup (HubConfig::policy, config.rs:396); en… |
| Network allowed (egress policy side) | partial | Construction time, per process/session: whatever composes a job/agent run builds one EgressPolicy from config ([network] section) and hands it to the sandboxed … |
| Tenant / trust constraints | present | harw-security-hub's SecurityPolicy TOML fixes tenant/trust-zone/auth-strength ceilings per local uid at hub startup (policy.rs:74-113); harw-web resolves it per… |
| Approvals | partial | Per tool-call, interactively, inside a running session: ApprovalModeCell + AllowRuleSet live in the OpContext ServiceMap (harw-ops/src/permissions.rs:41-54) and… |
| Audit | present | Write time, inside each hub/store itself, immediately after a durable state change (store.rs:499-514 durability-then-audit ordering). Verification is a separate… |
| docs/planning/70-decisions (DEC-004..008) relevance to policy-secrets | missing | N/A… |

---

# Placement engine for harw: design note

**Summary.** harw has no placement engine today. There is no `AgentRequirements`, `NodeOffer` or `PlacementDecision`, and a repo-wide grep for these finds nothing. What exists is a check that asks whether this one host can run a compiled agent: `harw-agent-runner/src/admission.rs:130` checks a `HostReport::probe()` (`harw-job-runtime/src/host.rs:125`). Provider selection is a lookup by ID (`harw-provider-http/src/routing.rs:119-244`).

The proposal:
- A new ring-**I** crate `harw-placement-model` holds the vocabulary and the pure filter and score functions.
- A new ring-**A** crate `harw-placement` holds the engine, leases and audit.
- The crates that own offers today produce them in the I vocabulary. J and A crates may depend on I (`xtask/arch-policy.toml:45-53`).
- The compiler (C) may use the I vocabulary for new DSL keys, but it never sees node topology.

---

## 1. Problem and scope

The engine turns requirements into **Node + Provider + Runtime**, with a lease. It places five kinds of work:
- **agent sessions**: the turn loop, locally or on a cloud host (65-cloud-sessions §8).
- **work-driver workers**: `harw-cli/src/job_worker_work_driver.rs:468`.
- **the judge**: `ModelRole::WorkDriverJudge` (`harw-config/src/role_models.rs:41-70`).
- **verify jobs**: `VerifyRunner` (`harw-plan-bridge/src/verify_exec.rs:237`).
- **tool jobs**: MCP- and job-backed tools.

Two things are out of scope. It does not execute anything (the coordinator stays `harw-job-runtime`). It does not route requests inside a turn once a lease is bound.

## 2. Requirement model

A new type, `AgentRequirements`, lives in `harw-placement-model` (ring I). It describes what the work *needs*. A separate type, `PlacementGrant`, holds the *ceiling*: operator, tenant and parent. This keeps the IR's rule that requirements "describe the artifact, not the live grant" (`harw-agent-dsl/src/ir_v2.rs:1059-1060`).

| Dimension | Existing source | New |
|---|---|---|
| Model capability | `ModelCapabilities` (`harw-model-catalog/src/descriptor.rs:229-246`); tools in `Permissions` (`ir_v2.rs:778`) imply tool calling | `CapabilitySet` bitset, `[placement] capabilities` |
| Context size | `Limits.max_context_tokens` (`ir_v2.rs:570-581`) | `min_context_tokens` |
| Network | `NetworkRequirement` (`ir_v2.rs:900`) | – |
| Secrets | `Models.required_env` (`ir_v2.rs:560`), `Permissions.required_env` (`ir_v2.rs:798`), auth-hub grants (`harw-auth-hub/src/config.rs:396`) | `SecretScope{None, Purposes(..)}`, a harw-owned type (the CryptGuard `NamespacePolicy` stays out of public APIs) |
| Write access | `filesystem_write` and `sandbox.filesystem` (`ir_v2.rs:1073-1076`) | – |
| CPU/RAM | `ResourceRequirements` (`ir_v2.rs:1017`); `ResourceRequest.cpu_weight` (`harw-job-core/src/spec.rs:188`) | `cpu_weight` in the requirement; fill in memory/pids, which are never derived today (`harw-agent-compiler/src/passes/requirements.rs:19,96-98`) |
| Latency class | none (only the `FAST_MODEL_HINTS` heuristic, `harw-config/src/internal_models.rs:429`) | `LatencyClass{Interactive, Standard, Batch}`, defaulted per `ModelRole` |
| Cost ceiling | `Budget.max_tokens` (`ir_v2.rs:437-446`); `Pricing` exists but is unpopulated (`descriptor.rs:282,354`) | `CostCeiling{per_request, per_session: MicroUsd}` |
| Provider constraints | `Models{provider, model, fallbacks}` (`ir_v2.rs:550-561`), `origin_allowlist` (`harw-config/src/provider_toml.rs:35`) | `ProviderConstraint{allow, deny, local_only, pin}` |

It also carries `tenant: TenantId` and `trust_ceiling: TrustZone` (`harw-types/src/security.rs:88`, as used in `harw-security-hub/src/policy.rs:47-49`). Every dimension has a `Level{NotNeeded, BestEffort, Required}` that mirrors `RequirementLevel` (`ir_v2.rs:961`).

**How requirements are declared**
- Round P4 converts the existing IR (`ExecutionRequirements`, `Models`, `Limits`) in ring A.
- Round P6 adds `[placement]` DSL keys and a compiler pass `passes/placement.rs`. This is allowed because C may depend on I (`arch-policy.toml:47`).

**How requirements are inherited**
- `ExecutionRequirements::union_child` (`ir_v2.rs:1086-1131`) stays for families placed together.
- For a child placed separately, the child's grant is `parent_grant.meet(child_ask)`. That means network ⊆, secrets ⊆, cost ≤ what is left of the parent's budget, and trust no closer than the parent's. If the child asks for more than the parent's grant, it is refused and never widened.

**How operators and tenants narrow**
- Grants only ever meet (intersect), in the same spirit as `ContextRequest` narrowing (`harw-security-hub/src/policy.rs:11-23`) and `AgentIr::clamped_to` (`ir_v2.rs:1306`).

## 3. Offer model

- **`NodeOffer`**
  - Identity and topology: `NodeId`, zone, state (only Active nodes are eligible), route metric. Source: `NodeRecord`/`Route` (`harw-netsec/src/model.rs:157-180,293-304`).
  - `HostFacts`. This type is already serde (`harw-job-runtime/src/host.rs:78-81`). The hub recomputes `best_report_from` (`host.rs:179`) instead of trusting a remote's verdict.
  - Capacity and headroom: memory, CPUs, pids.
  - Egress reach, as an `EgressPolicy` digest (`harw-egress/src/policy.rs:277`) plus a host set.
  - Secret purposes available on the node.
  - Permitted tenants.
  - Freshness (`last_seen`).
- **`RuntimeOffer`**: one per backend (Landlock trampoline, bwrap, Darwin, none), each with a per-dimension `EnforcementState` (`harw-job-core/src/enforcement.rs:17,50`). Today's admission uses the best state over all backends, which is an upper bound (`admission.rs:29-31`). Per-backend offers let the engine bind one specific backend.
- **`ProviderOffer`**
  - Static part: capabilities, `context_window` (`descriptor.rs:345`, config override per `harw-runtime/src/assembly.rs:4450-4476`), pricing, `is_local`, origin allowlist.
  - Live `ProviderLoad` (DEC-003):
    - `min(max_concurrency, rate_limit.max_concurrent)` minus `RESERVED_PROVIDER_SLOTS` (`job_worker_work_driver.rs:164,374-380`)
    - leases in flight
    - `pacing_wait` (`harw-core/src/model.rs:647`)
    - a `BudgetSnapshot` (`harw-provider-http/src/budget.rs:723,753`)

## 4. Engine

1. **Filter** (hard constraints, fail closed).
   - Candidates are triples (node, runtime, provider). Every predicate is evaluated, without short-circuiting, so each rejection records every reason, the way `findings` does (`admission.rs:150`).
   - Checks: needs ⊆ grant, then needs ⊆ offer.
   - Cross-checks: the node's egress must reach the provider's endpoint, and the node must hold the provider's credential purpose.
   - Unknown offer data counts as unmet. Example: `pricing: None` with a `Required` cost ceiling is rejected.
   - Degraded modes follow the admission table exactly: `Required` can never be overridden, and `BestEffort` passes degraded only with `allow_degraded` (`admission.rs:10-27`).
2. **Score**. Deterministic integer scoring, ties broken by stable IDs. Factors:
   - estimated cost (token estimate × price)
   - fit to the latency class
   - locality (route metric, local = 0)
   - headroom (1 − in-flight/capacity)
   - prompt-cache warmth: same provider, model and node as the parent's last turn, when `prompt_caching` is supported (`descriptor.rs:239`), and **only within the same tenant**
3. **Bind**. `CapacityLedger::reserve` issues a `Lease` with a TTL.
   - The provider ledger is implemented over the single per-process `ProviderRateLimiter`/`ProviderBudgets` (DEC-003). Placement therefore can never overshoot provider limits.
   - Node reservations are summed against the node's capacity.
4. **Explain**. A `PlacementDecision` is written to the `DecisionSink` for every outcome, including refusals and dry-runs. It contains:
   - digests of the request, grant and offers
   - the chosen candidate, its degraded findings and score breakdown
   - every rejected candidate with its reasons
   - the caller
5. **Re-place**. Triggered by:
   - lease loss (TTL, node drain or loss)
   - a provider becoming unavailable (`routing.rs:66`)
   - the 429 budget running out

   The engine re-runs with the failed candidate excluded and the *same* grant. Attempts are bounded. A running session is moved only at a turn boundary.

## 5. API sketch

```rust
// harw-placement-model (ring I; deps: harw-types, serde)
pub struct AgentRequirements { pub tenant: TenantId, pub trust_ceiling: TrustZone,
  pub targets: Vec<TargetReq>, pub capabilities: Need<CapabilitySet>,
  pub min_context_tokens: Need<u64>, pub network: Need<NetworkNeed>,
  pub secrets: SecretScope, pub write: Need<WriteNeed>, pub sandbox: SandboxLevels,
  pub resources: ResourceNeed, pub latency: LatencyClass, pub cost: Need<CostCeiling>,
  pub providers: ProviderConstraint }
pub struct PlacementGrant { /* same axes, ceilings */ }
impl PlacementGrant { pub fn meet(&self, other: &Self) -> Self; pub fn covers(&self, r: &AgentRequirements) -> Vec<Rejection>; }
pub struct Candidate { pub node: NodeKey, pub runtime: RuntimeBackend, pub provider: ProviderKey }
pub fn filter(req: &AgentRequirements, grant: &PlacementGrant, c: &CandidateView<'_>, allow_degraded: bool) -> Verdict;
pub fn score(req: &AgentRequirements, c: &CandidateView<'_>, w: &ScoreWeights) -> Score;
pub trait CapacityLedger: Send + Sync {
  fn reserve(&self, claim: &Claim) -> Result<LeaseGrant, CapacityShort>;
  fn release(&self, lease: LeaseId);
  fn available(&self, provider: &ProviderKey) -> Option<u32>; }

// harw-placement (ring A; may depend on "*")
pub trait OfferSource: Send + Sync { fn snapshot(&self) -> OfferSnapshot; }
pub trait DecisionSink: Send + Sync { fn record(&self, d: &PlacementDecision); }
impl PlacementEngine {
  pub fn new(src: Arc<dyn OfferSource>, ledger: Arc<dyn CapacityLedger>, sink: Arc<dyn DecisionSink>, policy: EnginePolicy) -> Self;
  pub fn place(&self, req: &PlacementRequest) -> Result<Placement, PlacementRefused>;
  pub fn explain(&self, req: &PlacementRequest) -> PlacementDecision;
  pub fn replace(&self, lease: LeaseId, cause: LeaseLoss) -> Result<Placement, PlacementRefused>; }
pub fn requirements_from_ir(ir: &AgentIr, role: Option<ModelRoleKey>) -> AgentRequirements;
```

The engine is synchronous and does no I/O; adapters take the snapshots. Public APIs contain only harw-owned types: `MicroUsd(u64)` and `UnixMillis(u64)` instead of `jiff`, and no `serde_json::Value`.

Add to `arch-policy.toml`: `harw-placement-model` in layer I and `harw-placement` in layer A.

## 6. Integration points

- **Agent runner.** `admission::check` (`admission.rs:130`) becomes the local-node case, with a test that the engine and admission give the same verdict. `run_requirements` (`admission.rs:466`) also prints the decision.
- **Work driver.** `parallel_ceiling` (`job_worker_work_driver.rs:374`) and `effective_cap` (`harw-plan-bridge/src/work_driver.rs:980`) read `ledger.available()`. There is one lease per worker, and a 429 calls `penalize` (`budget.rs:653`).
- **Judge and internal models.** Requirements come from the role (`WorkDriverJudge`: structured output, `Standard` latency) instead of `FAST_MODEL_HINTS`.
- **Verify runner (C-09).** The engine picks the `RuntimeOffer`. If any level is `Required`, `NoSandboxRunner` (`verify_exec.rs:257`) is excluded, and the `CoordinatorVerifyRunner` path (`verify_exec.rs:309`, `harw-cli/src/verify_sandbox.rs`) is used.
- **Remote nodes (65-cloud-sessions).**
  - The remote sends its `HostFacts` over `harw-node-transport`.
  - `HostLimits.max_running_turns` (65-cloud-sessions §8) comes from the ledger.
  - The remote runs admission again before it starts anything.
- **MCP tools.** Tool jobs carry the tenant already used in submitter keys (`harw-mcp-server/src/supervisor.rs:391`), plus a network and secrets scope.
- **Gap-hunt caps.** The "CPUs − 2 per workflow" cap (`docs/planning/85-gap-hunt/kit/skills/gap-hunt/SKILL.md:24-27`) becomes node capacity. Nested workflows take child leases from the parent's lease. DEC-004 (no parallel builds) becomes an exclusive `BuildSlot` capacity of 1 per workspace.

## 7. Security

- **Secrets.** A candidate is rejected unless every purpose in `SecretScope` is available on the node *and* permitted to the tenant. Secret material is resolved only on the bound node after the lease. One prerequisite: secret records need an owner/policy tag, which they do not have today (`harw-secrets/src/store.rs:502` audits as `Actor::System`).
- **Egress.** The effective egress is need ∩ grant ∩ node reach. It must be non-empty for `Hosts`. The bound runtime receives an `EgressPolicy` built from that intersection, and its digest goes into the decision record.
- **Tenants.** Leases are keyed by tenant, per-tenant caps apply, and cache affinity never crosses tenants.
- **Audit.** Every decision is recorded with the real caller.
- **Remote offers.** A remote node's facts are claims. They are accepted only over mTLS node transport and capped by the node's trust zone.
- **Semantics mismatch.** `harw-job-core` `SandboxRequirement::Required` means "every dimension enforced" (`spec.rs:255-256`), but IR `Required` also accepts `partial` (`admission.rs:21-27`). The engine must translate between them explicitly and never collapse them silently.

## 8. Roadmap (one agent per file)

- **P0**: DEC notes plus `docs/planning/66-placement/README.md` (text only).
- **P1**: `harw-placement-model` with `requirements.rs`, `offer.rs`, `decision.rs`, `lib.rs`, plus the arch-policy entry. Tests: meet/narrow laws, serde round trips.
- **P2**: `filter.rs` and `score.rs`. Tests: the admission-table equivalence and determinism. Depends on P1.
- **P3**: offer producers. Depends on P1.
  - `harw-job-runtime/src/offer.rs` (J→I)
  - `harw-model-catalog/src/offer.rs`
  - `harw-provider-http/src/ledger.rs`
  - `harw-netsec/src/offer.rs`
- **P4**: `harw-placement` with `engine.rs`, `lease.rs`, `from_ir.rs`, `sink.rs`. Depends on P2 and P3.
- **P5**: local integration in `admission.rs`, `job_worker_work_driver.rs`, `verify_sandbox.rs` and the judge wiring. Depends on P4.
- **P6**: `[placement]` DSL keys and `harw-agent-compiler/src/passes/placement.rs`, with an IR schema/hash bump. Can run in parallel with P4 and P5.
- **P7**: remote `HostFacts`, lease renewal and freshness. Depends on 65-cloud-sessions RS2 and RS3.

## 9. Open decisions

`DEC-001`–`DEC-019` are already taken in `docs/planning`, so numbering starts at 020.

- **DEC-020**: ring split: vocabulary in I, engine in A; C gets no topology.
- **DEC-021**: fail closed on unknown offer data (unknown price, stale node).
- **DEC-022**: needs versus grants; the child ⊆ parent rules; `union_child` stays for families placed together.
- **DEC-023**: the ledger is the single authority for provider slots, extending DEC-003.
- **DEC-024**: remote facts are self-reported, the hub recomputes, the remote admits again; lease TTL.
- **DEC-025**: where prices come from (catalog, config or `harw-provider`) and the `MicroUsd` unit.
- **DEC-026**: scoring weights, and cache affinity only within a tenant.
- **DEC-027**: IR `Required` versus job-core `Required` semantics, and the per-`ModelRole` latency defaults.

**Relevant paths:**
- `/home/user/Harwness/xtask/arch-policy.toml`
- `/home/user/Harwness/harw-agent-runner/src/admission.rs`
- `/home/user/Harwness/harw-job-runtime/src/host.rs`
- `/home/user/Harwness/harw-agent-dsl/src/ir_v2.rs`
- `/home/user/Harwness/harw-provider-http/src/budget.rs`
- `/home/user/Harwness/harw-provider-http/src/routing.rs`
- `/home/user/Harwness/harw-cli/src/job_worker_work_driver.rs`
- `/home/user/Harwness/harw-plan-bridge/src/verify_exec.rs`
- `/home/user/Harwness/harw-netsec/src/model.rs`
- `/home/user/Harwness/docs/planning/70-decisions/DEC-003-provider-limits.md`
- `/home/user/Harwness/docs/planning/65-cloud-sessions/README.md`

This was a read-only pass: no files were edited, no tests were added, and no build commands were run. The central build has nothing new to run for this report.

## Additions from the session (2026-09-27)

### Two-stage placement with its own gateway
In operation, a **dedicated Cloudflare Worker** sits in front of Workers AI,
with one or more bindings or lanes (see
[gateway-contract](../75-harness-patterns/gateway-contract.md)). The engine
chooses the *provider*, that is this Worker, plus role and prefix group. The
Worker chooses lane and affinity. For the `ProviderOffer`, the Worker is **one**
provider with capacity (`max_concurrency`, `[rate_limit]`); harw does not
model the bindings.

### Cache affinity by prefix, not by identity
The "prompt cache warm" score factor refers to **prefix groups**:
same system prompt, same tools, same repo context, within one
tenant. It does not refer only to the parent agent's last turn.
- Short-lived workers with the same prefix share an affinity key
  (`x-harw-cache-affinity`).
- Switching the model or provider costs one cache write.
- At around 98% cache reads, the **cache-read price** determines the
  cost, not the list price for input.

### Cost score
The expected cost is derived from:
- `cache_read_price × expected hit rate`;
- `input_price` for fresh tokens;
- `cache_write_price` on a cold start;
- `output_price × expected output`.

The ratio of cache read to input varies between 3 and 20% depending on the model,
for example DeepSeek v4 Flash 3%, the Luna family 10%, GLM-5.3 Flash 20%.
Prices live in data, not in code, and can be updated. Price data for GLM-5.3 and
5.3 Flash is still missing from the catalog today.

### Reference workhorse
For large jobs, **GLM-5.3 Flash via Workers AI** is the reference model
(as of the session):
- $0.15 input, $0.03 cache read, $0.50 output per 1M tokens;
- 1.31M context, function calling, reasoning, vision.

The frontier models on Workers AI have 20 and 50 requests per minute per
model and account, respectively. That is why the gateway's lane capacity belongs in the
ledger (DEC-023).

### Workflow throughput
The "CPUs − 2 per workflow" limit (gap hunt, pattern P6) becomes
node capacity. The parallel partitioning, that is several runs on separate
areas, is a placement decision.
