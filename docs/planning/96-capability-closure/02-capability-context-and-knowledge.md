# PL-96 / 02 — Capability, context, skills, caching and knowledge contracts

> **TARGET** unless explicitly marked CURRENT. Read [evidence register](01-evidence-register.md) for pinned source and uncertainty.

## A. The capability-closure model

Every tool/operation/agent/skill needs **separate**, non-confusable states:

| State | Definition | Producer / authority | Observable proof |
|---|---|---|---|
| Defined | Source schema/contract exists | ToolSpec, `#[operation]`, AgentIr, SkillIndex | Stable ID + source digest |
| Built | Artifact/feature compiles | Cargo feature/package gates | Clean checkout build |
| Registered | Provider bound to an actual entry | AssembledRegistry/OperationRegistry/AgentRoster | Effective registration enumeration |
| Authorized | Principal's policy/Scope admits | RuntimeSpec + PermissionSet + SandboxSpec + approvals | Derived allow/deny + reason |
| Reachable | Model/client sees correct callable surface | ModelRequest ToolSpecs, ToolPort, Channel adapter | End-to-end discovery |
| Executable | Implementation is invoked through intended backend | ToolExecutor, JobManager, operation dispatcher | Scoped execution result |
| Observed | Audit/journal records actual effect | Turn/Job/Session events | Stable run, effect, decision IDs |
| Verified | Exact integration acceptance passes | Tests, clean CI, fault injection | SHA-bound verification record |

**Do not conflate** static `capability_catalog::CATALOG` with the runtime's *effective* tool set. A registry-derived catalog from draft #89 may list an exact tool missing from the current runner; also a real runtime provider may be absent from the static catalog. Maintain a potential catalog for search and an **effective admissible projection** for a specific session/child/turn.

### Suggested contract (illustrative, NOT committed Rust)

```rust
struct CapabilityResolution {
    id: CapabilityId,
    source_digest: ContentDigest,
    declared: bool,
    registered: bool,
    exposed: bool,
    admitted: bool,
    executable_backend: Option<BackendKind>,
    reason: Option<UnavailableReason>,
    authority_snapshot: AuthoritySnapshotId,
}
```

Do not put detailed secrets, private agent prompts, credential strings, absolute host roots, or untrusted model-supplied explanations in `UnavailableReason`. The capability listing is informational, never an independent authorization grant. Dispatch checks the *same* current authority again; stale indexes cannot authorize.

#### Tests

- A declared-only tool returns `NotRegistered`, not `Ready`.
- A registered-but-denied tool either stays hidden from model list or appears in operator diagnostics as denied, without a usable call path.
- Parent-to-child, tenant, workspace, trusted/untrusted project and Device Tier filtering are all monotonic.
- Fuzz unknown tool, path and alias; no alternate naming bypasses authority.
- Effective projection built from mounted providers is stable across sorting and feature combinations; no indiscriminate dump of all schemas.

## B. Native-tools-first command selection

**CURRENT:** #127 added 42 typed tools in `harw-tool-fsread` (16), `harw-tool-sys` (12), `harw-tool-cargo` (8), and `harw-tool-gitread` (6). PL-93 §8 explicitly says these are **not yet wired** to profiles/authority/runner. Existing tools include `fs.grep`, `fs.read` and `shell.exec`. `cargo.*` delegates externally executed Cargo through the **already-governed ShellDelegate**, not a second unmanaged process launcher. Git read tools are pure Rust implementations; see declared supported Git features, limitations and compatibility tests before assigning git's full semantics.

**TARGET:** add provider registry/wiring behind a compatibility gate; implement operation intent matching via exact structured capability metadata, not substring matching of generated shell text. If no equivalent internal tool exists, report the limitation and optionally request the **existing authorized** shell path. An agent must not silently fall back from an explicitly denied specialized tool to a privileged shell as a workaround.

Routing sequence:
1. Parse task/required effect without execution.
2. Find candidates by **actual semantics** (read files, inspect Git, run Cargo checks).
3. Filter through current authorized effective registry and enabled runner.
4. Prefer minimally capable typed tool; otherwise explain `NotSupported`, `NotRegistered` or `Denied`.
5. Optional shell fallback is a separately admitted operation, not automatic authorization.
6. Preserve truncation/errors as machine-readable data; present concise result in prompt.
7. Associate tool invocation with work_id/turn_id, effect class and observation.

Specific acceptance fixtures: `ls -la` -> allowed `fsread.ls`; `git status` -> `git.status`; `cargo metadata --no-deps` -> `cargo.metadata`; `rg` retains existing `fs.grep`; `sudo` is **never** lowered to a read-only native tool. For unrepresentable flags, preserve honesty and do not fabricate compatibility.

## C. Triggered skills in bounded context

**CURRENT:** `harw-catalog/src/skill_triggers.rs` and `skill_select.rs` implement optional `triggers.toml`, deterministic select/dedupe/byte caps and `Injection::into_fragment` (section `skills.triggered`, `Stability::Fresh`). Merged PR #126 states no runtime injector; PL-95 S3 remains the wiring step.

**TARGET:** run the deterministic selector at the turn assembly boundary with bounded, trusted input events. Derive `ToolFirstUse` from an actual successful registration/invocation, not from an arbitrary string in tool output. Tool output may **trigger** a predefined skill but must not **author** instruction text. Use a trusted SkillIndex, content SHA and immutable source reference. Respect permission ceilings for all skills declaring tools.

```text
trusted enabled skills + immutable source digest
     + admitted task/tool/error/path events
  -> deterministic trigger hits
  -> ranked candidates (budget, dedupe, visibility)
  -> harw-context Fragment (skills.triggered; Fresh)
  -> typed context admission & budget
  -> provider-neutral request
  -> injection provenance + outcome record
```

Integration tests: E0505 triggers only exact code, path-glob edge cases, >4 KiB turn and 16 KiB session caps, reset after compaction, trusted L0 listing, identical results across seed/order, no permission widening, no credential leakage, and no rewritten stable provider cache prefix.

## D. Context and provider parity

**CURRENT:** `ModelRequest::data_block` appears as a user item in OpenAI Responses and Chat; Anthropic Messages `build_messages_body` does not read it (E06). `harw-core/src/context_budget.rs` provides typestate budgeting; `turn_loop.rs` has historical fallback and newer context paths that require an end-to-end matrix (E07).

**TARGET:** a **provider-independent logical request manifest** records required/optional content, trust class, section, digest, provenance and intentional omission reason. Individual wire adapters translate it without silent loss. No string concatenation into higher-trust system instructions from raw untrusted data. Explicitly check `instruction_fragments`, `data_block`, tool results, image attachments, tool name encodings, continuation text and budgets.

Proposed testing matrix:

| Dimension | Cases |
|---|---|
| Provider | OpenAI Responses, OpenAI Chat, Anthropic Messages; configured compatible variants |
| Content | Missing/empty/non-empty `data_block`, trusted instructions, untrusted tool result, tool calls, image |
| Ceiling | Closed, LocalRoot, appropriate child ceiling, no program/program |
| Budget | enough, section full, total full, required item too large |
| Lifecycle | first turn, continuation, approved pause, compaction, resume |
| Cache | implicit prefix, explicit markers, disabled |
| Outcome | content digest preserved, correct role/channel, exact omission reason |

The new serializer parity test must catch E06 *before* claiming a provider supports full context. Run unit tests and a mock transport integration test. Do not expose provider-private reasoning traces.

## E. Prompt caching, Gateway caching and concurrency are distinct

**CURRENT:** `harw-provider-http/src/cache_strategy.rs` applies explicit/implicit caching markers per provider. `infra/cloudflare/mias-lab` has a different exact-response/cache/concurrency design (#129, #138). #138 explicitly did not deploy adaptive lanes. Treat provider cache read/write token counts as separate from *full response cache hits* and HTTP gateway attempts.

**TARGET, with keys and correctness requirements:**

| Mechanism | Typical stable key/partition | Safety/property |
|---|---|---|
| Provider prefix caching | Provider + model + stable prompt prefix/version + API cache-control semantics | Preserve stable-prefix bytes, do not assume cached reasoning equivalence |
| Exact-response cache | Method + normalized canonical request body + model route + policy/version + auth/tenant partition | No mutating tools/streaming, never share restricted response across tenants |
| In-flight deduplication | Identical admitted request + compatible principal/tenant + route + idempotency | One billable request where permissible, correct fan-out/cancel semantics |
| Model capacity lanes | Provider/model/account capacity class | Shared quotas are shared; failover must not multiply shared-limit retries |
| Model-route affinity | Explicit stable session/route affinity | Sticky routing is not an authorization boundary |
| Embedding cache | Embedding model/version + canonical redacted text digest + scope | Index presence is **not** reader authorization |

Do not claim a cache-key field is already implemented merely because it is recommended in this table; inspect existing Worker key construction and traffic logs in a separate, Cloudflare-specific read-only wave. Measure prompt tokens new/cached, cache ratio, 429s, queue latency, cache correctness, cost per verified task and duplicate execution events, segmented by model and failure category.

## F. Agent discovery and context-effective delegation

**CURRENT:** six sealed authority roles, `role_names::ALL` approved built-ins, `AgentRoster` scoped custom definitions, `visible_delegation_targets` admission projection. About 33 home-agent TOML definitions are present, plus a registry tree containing base/family/context files; counts do **not** imply simultaneously running agents.

**TARGET:** an `agents.resolve` diagnostics projection returns only startable targets to models; operator UI may additionally show not-startable definitions with reason codes. Per task choose role by skill/evidence type, expected effect, authority, available tools, model route and concurrency quota. Workers may specialize **without** becoming a seventh authority role; fan-out is explicit, bounded and lease-scoped. Independent critic receives source evidence, not the writer's unverified summary alone.

Test that user-defined role/cycle cannot expand inherited network/filesystem/parent/tenant rights, UIA-worker exclusivity is upheld, and `AgentSteward` never produces a direct privileged edit as a side effect of agent discovery.

## G. Durable learning and global knowledge: do not join private contexts

**CURRENT:** `harw-runtime/src/memory_wiring.rs` implements project capture, close-time consolidation and startup sweep; `harw-ops/src/learn.rs` has explicit proposals; PL-90 `06-live-session-dream-diary-learning-wiring.md` identifies missing in-session milestone integration. Existing Dream/Diary/Ledger/Palace/indexing components are not interchangeable.

**TARGET:** add an event-derived idempotent checkpoint dispatcher keyed by committed `(session, turn, scope, event_id)`; optional durable jobs for extraction/reflection; operator review for promotions; versioned embeddings and ACL-scoped retrieval in project and global home only after explicit policy. A project's permitted reusable fact may enter global search *as a scoped reference with provenance*, not via copying another agent's private working history.

Hard invariants: an agent's private thought, raw child transcript or unapproved research never becomes globally searchable just because an embedding exists; conflicts/expiry are visible; an active session with no idle time can still checkpoint accepted milestones; failure to start a dream job is observable as deferred, never misreported as learned. Re-evaluate context budget and token savings against task completion quality.

**Related references:** [PL-93](../93-common-command-tools/README.md), [PL-95](../95-cli-tools-skills/README.md), [PL-90 06](../90-intent-driven-composed-agents/06-live-session-dream-diary-learning-wiring.md), [Model Provider](../../architecture/model-provider-routing.md), [Cache implementation](../../../harw-provider-http/src/cache_strategy.rs).
