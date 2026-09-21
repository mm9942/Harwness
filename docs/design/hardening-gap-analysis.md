# Hardening Gap Analysis — Bottom-Up Codebase Review

Generated from a bottom-up read of all 34 workspace crates, mapped against
`hardening-suggestive-inspiration.md` (32 sections).

The review traversed crates in dependency order: L0 leaves (types, agent-dsl,
plan, memory, home) → L1 (sandbox) → L2 (protocol, job-runtime) → L3 (config,
secrets) → L4 (catalog, operations, extension-api, session-store, tools) →
L5 (knowledge, channel, mcp-server, instructions, project-discovery,
model-catalog, provider, tool-fs, tool-shell) → L6 (core, core-bridge,
channel-telegram, oauth, registry-defaults) → L7 (provider-http, ops) → L8
(tui, install) → L9 (cli).

## Summary Verdict

The codebase has a strong security foundation: monotone permission reduction
in the sandbox, BLAKE3-content-addressed agent IR, sealed role enums,
spawn-matrix enforcement (closed in this worktree per CHANGELOG), and
consistent hand-written error types (no `anyhow`/`thiserror`). However, the
hardening document identifies **structural gaps** that the current code only
sketches — particularly in tool scope detection, typed resource scopes,
context compilation, severity envelopes, and the TurnScope unification
point. These are not bugs; they are missing architectural seams.

> **Ist-Stand (2026-09)**: G8, G11, G12, and G14 are resolved (see their
> Status fields below). G1–G7, G9, G10, and G13 are still open.

## Gap Register

### G1. Agent Template vs Concrete AgentProgram (§4, §31)

**Status**: Gap

`RawAgentDefinition` (harw-agent-dsl/src/raw.rs) requires exactly one `role`
field. The hardening doc describes a **template** concept that allows
multiple compatible roles (`roles = ["root-orchestrator", "child-orchestrator"]`)
which the compiler monomorphises into a concrete `AgentProgram` with one
role. This does not exist. A coding-orchestrator that can serve as both root
and child currently requires two separate TOML definitions.

**Evidence**:
- `harw-agent-dsl/src/raw.rs:31` — `pub role: AgentRoleId` (singular, required)
- No `AgentTemplate` type anywhere
- `ExecutableAgentIr` (executable.rs) has a single `role` field

**Hardening delta**: Introduce `RawAgentTemplate` with `roles: Vec<AgentRoleId>`
and a `template` reference field on `RawAgentDefinition`. The compiler
monomorphises template + role → concrete `ExecutableAgentIr`.

---

### G2. Family as Typed Protocol (§5, §31)

**Status**: Partial — rosters only

`ResolvedFamily` (harw-agent-dsl/src/family.rs) carries `orchestrators`,
`workers`, `defaults` (free TOML table), and `invariants` (free strings).
The hardening doc wants typed **protocol slots**: task envelope, return
envelope, severity protocol, artifact protocol, and typed invariants.

**Evidence**:
- `harw-agent-dsl/src/family.rs:166` — `ResolvedFamily` has no protocol fields
- `invariants: Vec<String>` — untyped, not compiler-checked
- `defaults: toml::Table` — free-form, no schema

**Hardening delta**: Add `FamilyProtocol` struct with typed
`task_envelope`, `return_envelope`, `severity_protocol`, `artifact_protocol`
fields (all `DefinitionRef`). Add typed invariant validation.

---

### G3. Clan/Cell Scope Typing (§6, §7, §8, §31)

**Status**: Gap — string scopes

`plan_scope` in organization types is a plain string (`"implementation/*"`).
The hardening doc wants a typed `PlanSelector` with prefix/tags/status
filters and compile-time overlap checking between clans.

**Evidence**:
- `harw-plan/src/ids.rs:163` — `PathOrSymbol(String)` — untyped
- `harw-agent-dsl/src/organization.rs` — `plan_scope` is a string field
- No `PlanSelector`, `ResourceSelector` types anywhere
- No `subset_of()`, `intersects()`, `is_disjoint()` operations on scopes

**Hardening delta**: Introduce `ResourceSelector` enum with `WorkspacePath`,
`RepositorySymbol`, `PlanNode`, `Secret`, `NetworkOrigin` variants. Add
set-algebra operations. Use for agent authority, clan delegation, mutation
leases, and tool invocation scope.

---

### G4. Tool Scope Detection — 7 Gates (§11, §12, §17, §28)

**Status**: Gap — name-based allowlist only

The current tool surface is controlled by `ToolProfile` (Minimal/Coding/Full)
with a hardcoded name set, plus per-session enable/disable overrides. The
hardening doc describes 7 distinct gates: host availability, program
eligibility, family/org policy, effective authority, task scope, turn
exposure, and invocation scope. None of these beyond the most basic
name-filter exist.

**Evidence**:
- `harw-core/src/activation.rs:50` — `ToolProfile` enum with 3 variants
- `activation.rs:88` — `allowlist()` returns a `HashSet<ToolName>` (5 names for Coding)
- No `ToolDescriptor` with `effect_kinds`, `risk_class`, `scope_schema`
- No `ToolScopeExtractor` trait
- No per-invocation scope validation

**Hardening delta**: Introduce `ToolDescriptor` internal type alongside the
model-facing `ToolSpec`. Add `effect_kinds: EffectSet`, `risk_class:
RiskClass`, `scope_schema: ScopeSchema`. Implement a `ToolResolver` that
computes callable tools from the intersection of host bindings, program
ceiling, role ceiling, family policy, org delegation, job/plan scope, and
session narrowing.

---

### G5. ToolExposure — Resident/Deferred/Hidden/Denied (§14, §15)

**Status**: Gap — mention only in a comment

`ToolExposure` is referenced in a doc comment (`activation.rs:6`) but does
not exist as a type. The hardening doc wants three exposure tiers plus a
denied state.

**Evidence**:
- `rg "ToolExposure"` → only a doc comment
- No enum with `Resident`, `Deferred`, `Hidden`, `Denied`

**Hardening delta**: Add `ToolExposure` enum. The turn-loop computes
exposure per turn from `Callable Tool Set + TurnTrigger + TaskIntent +
ModelProfile`. A tool-search tool, if added, must only search within the
already-admitted callable set.

---

### G6. TurnScope Unification (§16, §29, §32)

**Status**: Gap — does not exist

The hardening doc's deepest insight: `TurnScope` should be the single
authoritative turn description consumed by `ToolResolver`,
`ContextCompiler`, `ModelRouter`, and `ApprovalEngine`. Currently these
subsystems work independently — `collect_tools` reads from the session
activation, `context_budget::assemble` reads from fragments + history, and
there is no shared authority boundary.

**Evidence**:
- `rg "TurnScope"` → 0 results
- `ToolResolver` does not exist as a type
- `ContextCompiler` does not exist as a type
- `collect_tools` (turn_loop.rs:274) directly queries `session.activation()`
- `context_budget::assemble` (context_budget.rs:45) works on raw fragments

**Hardening delta**: Introduce `TurnScope` struct carrying role,
specialization, family, clan, cell, run_id, parent_id, trigger, goal_id,
plan_binding, read_scope, write_lease, forbidden_scope, effective_authority,
budgets, and state revisions. Both tool resolution and context assembly
consume the same `TurnScope`.

---

### G7. Context Compilation — Inclusion Classes, Severity, Overflow (§18–§21, §24–§27)

**Status**: Gap — byte-budgeted FIFO

`ContextBudget::assemble` (harw-core/src/context_budget.rs) selects
fragments in delivery order until the byte budget is exhausted. There are
no inclusion classes, no severity tiering, no trigger matching, no detail
levels, and no controlled overflow (mandatory items can be silently
dropped).

**Evidence**:
- `context_budget.rs:45` — `assemble()` iterates fragments in order, drops on overflow
- `ContextFragment` (extension-api/src/types.rs:16) — `{ label, content }` only
- No `ContextCandidate`, `InclusionClass`, `ImpactSeverity`, `Provenance`
- No `ContextManifest` with included/omitted/excluded reporting

**Hardening delta**: Introduce `ContextCandidate` with `inclusion:
InclusionClass`, `impact_severity`, `priority`, `relevance`, `confidence`,
`freshness`, `provenance`, `detail_levels`, `token_costs`. Replace
`assemble()` with a compiler that: (1) collects candidates, (2) marks
stale/superseded, (3) reserves mandatory, (4) reserves unresolved
critical/high, (5) reserves direct dependencies, (6) scores remaining by
utility, (7) selects detail level, (8) emits ContextManifest. Overflow
compresses before dropping mandatory items.

---

### G8. Severity Envelope (§22, §23, §30)

**Status**: Resolved (2026-09) — `harw-types/src/impact.rs` now exists.

**Status (original)**: Partial — domain-specific only

`harw-memory` has `Confidence` and `contradiction_index::severity` (u8
0-100), but there is no cross-cutting `ImpactSeverity` / `ImpactAssessment`
type. The hardening doc wants a runtime-level severity envelope that
aggregates across families and controls scheduling/attention — but never
authority.

**Evidence**:
- `harw-memory/src/epistemic.rs:127` — `Confidence` enum (VeryLow..VeryHigh)
- `harw-memory/src/contradiction_index.rs:78` — `severity: u8`
- `harw-types/src/roles.rs:38` — `RiskLevel` (Low/Medium/High/Critical) — used for approvals
- No `ImpactSeverity` enum at runtime level
- No `ImpactAssessment` struct with domain, confidence, evidence, provenance
- No invariant: "severity changes attention/scheduling, never authority"

**Hardening delta**: Add `ImpactSeverity` (Info/Low/Medium/High/Critical)
and `ImpactAssessment` to `harw-types` or a new `harw-impact` crate.
Families map domain events to this common schema. Critical returns can wake
parents, request verifiers, route stronger models, reserve more context,
open approval surfaces — but never authorize new tools or expand sandbox.

---

### G9. Return Envelopes (§5, §29, §32)

**Status**: Gap — string returns

`AgentToolAdapter::invoke` (harw-core-bridge/src/agent_tool.rs) returns a
string result from child agents. The hardening doc wants a structured
`AgentReturn<T>` envelope with `agent_id`, `program_snapshot`,
`plan_node_id`, `outcome`, `impact`, `priority`, `affected_scope`,
`artifacts`, `evidence`, `state_revision_after`, and typed payload `T`.

**Evidence**:
- `harw-core-bridge/src/agent_tool.rs` — returns `OpOutput` with text
- CHANGELOG: "Typed Parent-to-Child return pipeline (Agent-as-Tool) — child returns a string today"
- No `AgentReturn`, `Outcome`, `ImpactAssessment` types

**Hardening delta**: Introduce `AgentReturn<T>` envelope. Child turns
produce this instead of raw text. Parents parse the envelope to
understand outcome, scope, artifacts, evidence, and severity without
reading the full transcript.

---

### G10. Shell Tool — Write Scope Enforcement (§12, §15)

**Status**: Gap — full workspace writable

`ShellExecutor` (harw-tool-shell/src/exec.rs) runs `/bin/sh -c <command>`
with `cwd = workspace.canonical_root()`. The sandbox checks
`ExecuteProcess` permission but does not constrain what the command can
modify. A worker with a write-lease on `src/openai.rs` can `echo > src/other.rs`
via shell. The hardening doc describes a four-phase model: declare →
estimate → enforce → observe.

**Evidence**:
- `harw-tool-shell/src/exec.rs:213` — `cwd: PathBuf` from `workspace.canonical_root()`
- Only permission check: `Permission::ExecuteProcess`
- No filesystem diff after execution
- No write-scope validation
- `shell.exec` is marked `parallel_safe = false` (correct)

**Hardening delta**: The shell executor should: (1) snapshot the workspace
tree before execution, (2) run in a sandbox that mounts only the delegated
write-scope as writable, (3) diff after execution, (4) validate the diff
against the mutation contract. This is the "Harwness Borrow Checker" for
shell effects.

---

### G11. `deny_unknown_fields` Coverage (§11, cross-cutting)

**Status**: Resolved (2026-09) — now 263 occurrences across the tree.

**Status (original)**: Gap — only 4 uses

`rg "deny_unknown_fields"` returns 4 hits across the entire codebase.
Untrusted inputs (TOML configs, MCP wire payloads, channel events, tool
arguments) should reject unknown fields to prevent silent authority
injection. `JobIntent` correctly uses it (admission.rs), but most config
types and wire types do not.

**Evidence**:
- `harw-core/src/admission.rs` — `JobIntent` has `#[serde(deny_unknown_fields)]`
- `harw-tool-shell/src/exec.rs` — `ShellExecArgs` has `#[serde(deny_unknown_fields)]`
- `harw-config/src/harness_config.rs` — no `deny_unknown_fields` on most config structs
- `harw-protocol/src/wire.rs` — `RequestEnvelope` etc. have no `deny_unknown_fields`

**Hardening delta**: Audit all structs that deserialize from untrusted
input (config TOML, MCP JSON-RPC, channel wire, tool arguments). Add
`#[serde(deny_unknown_fields)]` where the struct represents a closed
authority-bearing contract.

---

### G12. `unsafe impl Send/Sync` in ShortTermMemory

**Status**: Resolved (2026-09) — no `unsafe impl` remains in `harw-memory/src/`.

**Status (original)**: Review needed

`harw-memory/src/short_term.rs:246-247` has `unsafe impl Send` and `unsafe
impl Sync` for `ShortTermMemory`. This needs review — if `ShortTermMemory`
contains `Rc` or other non-Send types, the unsafe impl is a soundness risk.

**Evidence**:
- `harw-memory/src/short_term.rs:246` — `unsafe impl Send for ShortTermMemory {}`
- `harw-memory/src/short_term.rs:247` — `unsafe impl Sync for ShortTermMemory {}`

**Hardening delta**: Review whether `ShortTermMemory` can use `Arc`/`Mutex`
instead of unsafe impls. If the unsafe impls are needed (e.g., for a
thread-localRefCell pattern), document the safety invariant.

---

### G13. DefinitionRef String Deserialization (§10)

**Status**: Gap — struct form only

`DefinitionRef` (harw-agent-dsl/src/ids.rs) deserializes from a TOML table
(`{ id = "...", version = "..." }`). The hardening doc wants ergonomic
string form (`extends = "harwness.agent.worker-base@1"`) alongside the
struct form. Family patches already parse strings manually, creating two
syntaxes.

**Evidence**:
- `harw-agent-dsl/src/ids.rs` — `DefinitionRef` has no custom deserializer
- Tests in `raw.rs` use `extends = { id = "..." }`
- `family.rs::apply_roster_patch` manually parses strings via `parse_def_ref`

**Hardening delta**: Add a custom `Deserialize` impl for `DefinitionRef`
that accepts both string and table forms.

---

### G14. ApprovalPolicy — Only None/Always (§28)

**Status**: Resolved (2026-09) — `ApprovalPolicy` now has `None`, `Always`,
`RequireForScope`, `RequireForEffect`, `RequireForRiskClass`.

**Status (original)**: Gap — no risk-class or scope-based approval

`ApprovalPolicy` (harw-operations/src/operation.rs:139) has only `None` and
`Always`. The hardening doc wants approval triggered by: outside declared
read scope, outside write lease, network access, secret access,
destructive processes.

**Evidence**:
- `harw-operations/src/operation.rs:139` — `enum ApprovalPolicy { None, Always }`
- `harw-extension-api/src/contributors.rs:34` — `ApprovalHandler::review` returns `Allow/Deny/AskUser`
- No scope-aware approval logic

**Hardening delta**: Add `ApprovalPolicy::RequireForScope`,
`RequireForEffect`, `RequireForRiskClass` variants. The approval engine
consults the `TurnScope` to determine whether an invocation needs approval
based on its requested effects vs. the agent's delegated scope.

---

## What Is Already Strong

These areas match the hardening document's target model and need no
immediate work:

1. **Sealed role enum + spawn matrix** — `AgentRoleId` is a Rust enum,
   `can_spawn` is a pure function, and `ManagedAgentSpawner::admit` enforces
   it as the first authority check before sandbox/depth/lease.
2. **Monotone permission reduction** — `PermissionSet` only supports
   `intersection` and `is_subset_of`. `SandboxSpec::restrict` never
   broadens. Child sandboxes are validated via `ensure_child_of`.
3. **Path traversal rejection** — `WorkspaceBinding::join_relative` rejects
   `..`, root, prefix components. `resolve_existing` canonicalizes and
   checks containment.
4. **BLAKE3 content addressing** — `ExecutableAgentIr` has a deterministic
   `SnapshotId` computed over stable content fields, excluding timestamps.
5. **Secret crypto** — ML-KEM 512 + AEAD envelopes, tamper-evident audit
   chain with checkpoints.
6. **No `anyhow`/`thiserror`** — all 34 crates use hand-written error enums
   per project rules.
7. **ToolExecutor requires sandbox context** — no ambient host authority;
   `ToolExecutionContext` is mandatory.
8. **Tracing redaction** — tool arguments are never logged; only byte
   lengths and status strings.

## Recommended Hardening Priority

Ordered by leverage (security impact × feasibility):

| Priority | Gap | Effort | Impact | Status (2026-09) |
|----------|-----|--------|--------|--------|
| P0 | G11 (deny_unknown_fields) | Low | High — prevents silent authority injection | Resolved |
| P0 | G12 (unsafe Send/Sync review) | Low | High — soundness | Resolved |
| P1 | G10 (shell write-scope) | Medium | Critical — shell is the broadest effect surface | Open |
| P1 | G6 (TurnScope) | Medium | High — unifies all authority projections | Open |
| P2 | G4 (ToolDescriptor + 7 gates) | High | Critical — real tool scope detection | Open |
| P2 | G7 (ContextCandidate) | High | High — prevents mandatory-context loss | Open |
| P2 | G8 (ImpactSeverity) | Medium | High — cross-family scheduling | Resolved |
| P3 | G3 (ResourceSelector) | High | High — "Harwness Borrow Checker" | Open |
| P3 | G1 (AgentTemplate) | Medium | Medium — reduces definition duplication | Open |
| P3 | G2 (Family protocols) | Medium | Medium — typed family contracts | Open |
| P3 | G9 (Return envelopes) | Medium | High — structured parent-child communication | Open |
| P4 | G5 (ToolExposure) | Low | Medium — progressive tool loading | Open |
| P4 | G13 (DefinitionRef strings) | Low | Low — DSL ergonomics | Open |
| P4 | G14 (ApprovalPolicy) | Medium | Medium — scope-aware approval | Resolved |
