##### docs/design/agent-definition-dsl.md

# Harwness Agent Definition DSL

> Status: partially implemented · Last reviewed: 2026-09-24

**Status:** Normative design draft  
**Scope:** User-extensible TOML definitions for roles, specializations, families, clans, cells, context policies, return contracts, and organization templates  
**Companion documents:** [`philosophy.md`](../philosophy/philosophy.md), [`coding-philosophy.md`](../philosophy/coding-philosophy.md)

> Users may extend behavior, composition, models, context policies, families, clans, and organization templates.  
> Users may never extend authority beyond the runtime maximum or violate the role hierarchy.

---

## 1. Purpose

Harwness agents should not be hard-coded as a fixed list of Rust enums and `match` arms.

The runtime must provide a small set of strongly typed authority roles:

```text
UserInterfaceAgent
RootOrchestrator
ChildOrchestrator
Worker
```

On top of these roles, users must be able to define, extend, combine, replace, version, and share reusable agent specializations such as:

```text
FocusedCodingOrchestrator
FocusedPureCodingTaskAgent
FocusedCodingPlanningTaskAgent
FocusedDepsDocArtifactResearcher
FocusedDocsWebResearcher
FocusedInCodeDocsWriter
FocusedObsidianVaultUpdateAgent
FocusedDocsUpdateAgent
FocusedVerificationAgent
FocusedBenchmarkAgent
FocusedDependencyUpdateAgent
```

The configuration system must also support reusable:

- agent families;
- organization templates;
- clans;
- temporary cells;
- context policies;
- return contracts;
- tool policies;
- model-routing policies;
- verification profiles;
- user and project overlays.

The resulting system is a TOML-based declarative DSL compiled by Rust into validated, typed runtime definitions.

---

## 2. Four Orthogonal Dimensions

Every instantiated agent is defined by four independent dimensions.

```text
Role
→ authority and place in the orchestration tree

Lifecycle
→ defined, provisioned, running, completed, failed, cancelled

Specialization
→ work contract, tools, context, model policy, and return schema

Organization
→ family, clan, cell, parent, and run-tree membership
```

The DSL may configure specializations and organizations extensively.

The DSL may select a permitted role.

The DSL may not invent a new authority role or alter the built-in spawn matrix.

---

## 3. Authority Roles Are Closed

Roles are a closed Rust-level set:

```rust
pub enum AgentRoleId {
    UserInterface,
    RootOrchestrator,
    ChildOrchestrator,
    Worker,
}
```

The role hierarchy is not user-extensible through TOML.

The runtime enforces:

| Caller | Root | Child | Worker | AgentTool |
|---|---:|---:|---:|---:|
| UserInterfaceAgent | yes | no | no | policy |
| RootOrchestrator | no | yes | yes | yes |
| ChildOrchestrator | no | yes, within depth | yes | yes |
| Worker | no | no | no | yes |
| AgentTool | no | no | no | nested tools by policy |

No user definition, family, clan, mixin, plugin metadata, or organization template may override this table.

---

## 4. Definition Layers

Definitions are resolved through explicit layers.

```text
1. Built-in Harwness definitions
2. Installed agent packs
3. User-global definitions
4. Workspace definitions
5. Project overlays
6. Run-local instantiation overrides
```

Recommended locations:

```text
harwness built-ins
  embedded://agents/
  embedded://families/
  embedded://policies/

installed packs
  ~/.config/harwness/packs/<pack-id>/

user-global
  ~/.config/harwness/agents/
  ~/.config/harwness/families/
  ~/.config/harwness/organizations/
  ~/.config/harwness/policies/

workspace
  <workspace>/.harwness/agents/
  <workspace>/.harwness/families/
  <workspace>/.harwness/organizations/
  <workspace>/.harwness/policies/

project
  <project>/.harwness/overlays/

run-local
  persisted in the run snapshot, not silently written back to source TOML
```

Later layers may refine earlier layers only through explicit merge operations.

---

## 5. Namespaced Identifiers

Every reusable definition has a stable namespaced ID.

```toml
id = "harwness.agent.focused-pure-coding@1"
id = "acme.agent.rust-pqc-worker@1"
id = "harwness.family.focused-coding@1"
id = "acme.organization.crypt-guard-release@2"
```

The syntax is:

```text
<namespace>.<kind>.<name>@<major-version>
```

Examples of kinds:

```text
agent
family
organization
policy
context
return
model-routing
verification
toolset
mixin
```

The major version is part of compatibility resolution.

Definitions should also include a full semantic version:

```toml
version = "1.4.0"
```

---

## 6. Single Base Plus Explicit Mixins

To avoid ambiguous multiple inheritance, each definition may have:

- zero or one `extends` base;
- zero or more ordered `mixins`.

```toml
extends = "harwness.agent.worker-base@1"

mixins = [
  "harwness.mixin.rust-coding@1",
  "harwness.mixin.file-scoped-write@1",
  "acme.mixin.no-clone-preference@1",
]
```

`extends` provides the structural base.

`mixins` provide reusable additive policy fragments.

Mixins may not change the role.

A mixin that requires a role declares that requirement:

```toml
id = "harwness.mixin.file-scoped-write@1"
version = "1.0.0"

[requires]
roles = ["worker"]
```

---

## 7. Explicit Merge Semantics

Harwness must not use an invisible recursive TOML merge.

Each inherited field follows one explicit operation:

```text
replace
append
prepend
remove
intersect
min
max-within-parent
```

A user overlay may express:

```toml
[patch.tools]
append = ["cargo-test", "cargo-clippy"]
remove = ["web-search"]

[patch.context.must_include]
append = ["task.acceptance_criteria"]

[patch.limits]
max_tool_calls = { min = 24 }
max_context_tokens = { min = 24000 }

[patch.models.preferred]
replace = ["anthropic/claude-sonnet-5", "openai/gpt-5.6-codex"]
```

For authority-bearing sets, only `intersect` and removal are allowed:

```toml
[patch.authority.capabilities]
intersect = ["filesystem.read", "filesystem.write.scoped", "process.spawn.sandboxed"]
```

Attempts to append authority fail validation.

---

## 8. Core Agent Definition Syntax

Example: built-in pure coding worker.

```toml
schema = "harwness.agent/v1"
id = "harwness.agent.focused-pure-coding@1"
version = "1.0.0"

extends = "harwness.agent.worker-base@1"

name = "Focused Pure Coding Task Agent"
description = "Implements one bounded coding task without redesigning architecture."

role = "worker"
specialization = "focused-pure-coding"

[compatibility]
min_harwness = "0.1.0"

[binding]
required = "plan-node"
requires_parent_orchestrator = true

[work]
mode = "implementation"
may_research_web = false
may_change_plan = false
may_change_architecture = false
may_spawn_agents = false
may_invoke_agent_tools = true

[scope]
mode = "declared-write-set"
require_base_revision = true
require_plan_revision = true
reject_out_of_scope_diff = true

[context]
policy = "harwness.context.focused-worker@1"
budget_tokens = 18000
include_full_transcript = false
load_details = "on-demand"

[return]
contract = "harwness.return.coding-task@1"
rerun_parent_on_success = true
rerun_parent_on_failure = true
reset_tool_choice = true

[tools]
policy = "harwness.toolset.focused-coding@1"
default_choice = "auto"
forced_choice_scope = "single-turn-single-use"

[models]
routing = "harwness.model-routing.focused-coding@1"

[verification]
profile = "harwness.verification.rust-focused-task@1"

[limits]
max_tool_calls = 40
max_agent_tool_calls = 4
max_wall_time_seconds = 1800
```

---

## 9. User Extension Example

A user may build on the pure coding worker:

```toml
schema = "harwness.agent/v1"
id = "acme.agent.rust-pqc-pure-coder@1"
version = "1.0.0"

extends = "harwness.agent.focused-pure-coding@1"

name = "Rust PQC Pure Coding Agent"
description = "A file-scoped Rust worker for cryptographic code with explicit ownership discipline."

mixins = [
  "harwness.mixin.rust-coding@1",
  "acme.mixin.cryptographic-zeroize@1",
  "acme.mixin.explicit-ownership-elevation@1",
]

[patch.models.preferred]
replace = [
  "anthropic/claude-opus-4.8",
  "openai/gpt-5.6-codex",
]

[patch.tools]
remove = ["web-search", "web-fetch"]
append = ["cargo-test", "cargo-clippy", "cargo-fmt"]

[patch.context.must_include]
append = [
  "project.philosophy",
  "project.cryptographic_invariants",
  "task.required_trait_contract",
]

[patch.context.may_include]
remove = ["global.user_memory", "unrelated_run_history"]

[patch.verification.commands]
append = [
  "cargo test -p ${crate} --lib",
  "cargo clippy -p ${crate} --all-targets -- -D warnings",
]

[custom]
ownership_style = "borrow-first"
secret_clone_policy = "forbidden"
zeroize_required = true
```

The `[custom]` table is preserved as namespaced user metadata.

It does not affect authority unless a registered compiler extension understands and validates the fields.

---

## 10. Specialization Contracts

Specializations are registered by Rust code or installed plugins.

TOML selects and configures a specialization; it does not dynamically create executable Rust behavior from arbitrary strings.

```rust
pub trait AgentSpecialization<R: AgentRole>: sealed::Sealed {
    type Config: DeserializeOwned + Validate;
    type Return: Serialize + DeserializeOwned;
    type ContextPolicy: ContextCompiler<R>;
    type ToolPolicy: ToolResolver<R>;
}
```

A registry maps DSL names to typed compilers:

```rust
registry.register::<Worker, FocusedPureCoding>(
    "focused-pure-coding",
    focused_pure_coding_schema(),
);
```

User-created agents may freely combine registered specializations, policies, mixins, and templates.

A user wishing to create an entirely new specialization implementation may install a trusted plugin that registers:

- its config schema;
- role compatibility;
- tool policy;
- context compiler;
- return schema;
- validation hooks.

---

## 11. Focused Agent Specializations

### 11.1 Focused Coding Orchestrator

Valid roles:

```text
RootOrchestrator
ChildOrchestrator
```

```toml
schema = "harwness.agent/v1"
id = "harwness.agent.focused-coding-orchestrator@1"
version = "1.0.0"

extends = "harwness.agent.orchestrator-base@1"

name = "Focused Coding Orchestrator"
specialization = "focused-coding-orchestrator"

[roles]
allowed = ["root-orchestrator", "child-orchestrator"]

[planning]
requires_plan = true
requires_dependency_graph = true
requires_write_set_partition = true
requires_acceptance_criteria = true

[delegation]
allowed_worker_specializations = [
  "focused-pure-coding",
  "focused-coding-planning",
  "focused-deps-doc-artifact-researcher",
  "focused-docs-web-researcher",
  "focused-in-code-docs-writer",
  "focused-docs-update",
  "focused-obsidian-vault-update",
  "focused-verification",
]
child_orchestrators = "within-runtime-depth"
workers = "within-budget"

[context]
policy = "harwness.context.coding-orchestrator@1"
include_full_worker_transcripts = false
aggregate_returns = true
details = "artifact-references"

[return]
rerun_on_child_return = true
rerun_on_worker_return = true
rerun_on_tool_return = true
reset_tool_choice = true
```

### 11.2 Focused Coding Planning Task Agent

This is a worker that produces a plan artifact.

It does not mutate the authoritative plan directly.

```toml
id = "harwness.agent.focused-coding-planning@1"
version = "1.0.0"

extends = "harwness.agent.worker-base@1"

role = "worker"
specialization = "focused-coding-planning"

[work]
mode = "plan-proposal"
may_change_plan = false
may_write_code = false

[tools]
policy = "harwness.toolset.planning-readonly@1"

[return]
contract = "harwness.return.plan-proposal@1"
```

### 11.3 Focused Dependency and Artifact Researcher

```toml
id = "harwness.agent.focused-deps-doc-artifact-researcher@1"
version = "1.0.0"

extends = "harwness.agent.worker-base@1"

role = "worker"
specialization = "focused-deps-doc-artifact-researcher"

[research]
sources = ["local-docs", "source-tree", "lockfiles", "artifacts", "release-notes"]
primary_sources_required = true
output = "structured-evidence"
may_write_code = false
```

### 11.4 Focused Web Documentation Researcher

```toml
id = "harwness.agent.focused-docs-web-researcher@1"
version = "1.0.0"

extends = "harwness.agent.worker-base@1"

role = "worker"
specialization = "focused-docs-web-researcher"

[research]
sources = ["official-docs", "official-repositories", "standards", "release-notes"]
freshness = "required"
primary_sources_required = true
output = "verified-research-records"
```

### 11.5 Focused In-Code Documentation Writer

```toml
id = "harwness.agent.focused-in-code-docs-writer@1"
version = "1.0.0"

extends = "harwness.agent.worker-base@1"

role = "worker"
specialization = "focused-in-code-docs-writer"

[work]
mode = "code-documentation"
allowed_mutations = ["comments", "rustdoc", "examples", "doctests"]
behavioral_code_changes = false
```

### 11.6 Focused Docs Update Agent

```toml
id = "harwness.agent.focused-docs-update@1"
version = "1.0.0"

extends = "harwness.agent.worker-base@1"

role = "worker"
specialization = "focused-docs-update"

[work]
mode = "documentation"
allowed_file_types = ["md", "mdx", "toml", "yaml"]
source_grounded = true
behavioral_code_changes = false
```

### 11.7 Focused Obsidian Vault Update Agent

```toml
id = "harwness.agent.focused-obsidian-vault-update@1"
version = "1.0.0"

extends = "harwness.agent.focused-docs-update@1"

role = "worker"
specialization = "focused-obsidian-vault-update"

[obsidian]
preserve_frontmatter = true
preserve_block_ids = true
preserve_wikilinks = true
update_backlinks = true
avoid_orphan_notes = true
canvas_updates = "explicit-only"
```

---

## 12. Context Policy Definitions

Context policies are reusable TOML definitions.

```toml
schema = "harwness.context/v1"
id = "harwness.context.focused-worker@1"
version = "1.0.0"

[budget]
tokens = 18000
reserved_for_output = 6000

[must_include]
items = [
  "agent.role_contract",
  "agent.specialization_contract",
  "task.objective",
  "task.acceptance_criteria",
  "task.write_scope",
  "task.forbidden_scope",
  "task.required_dependencies",
  "task.plan_revision",
  "task.base_revision",
  "new.trigger_return",
]

[should_include]
items = [
  "relevant.interface_contracts",
  "direct_dependency_returns",
  "current_failure_context",
]

[may_include]
items = [
  "recent_related_decisions",
  "selected_project_memory",
]

[exclude]
items = [
  "full_parent_transcript",
  "sibling_transcripts",
  "unrelated_plan_nodes",
  "global_user_history",
  "successful_raw_tool_logs",
]

[selection]
priority = true
severity = true
relevance_weight = true
confidence = true
freshness = true

[details]
mode = "references"
load_on_demand = true
```

---

## 13. Return Contracts and LLM Reruns

Every terminal tool, worker, or child return is persisted before the parent rerun.

```toml
schema = "harwness.return/v1"
id = "harwness.return.coding-task@1"
version = "1.0.0"

[fields]
required = [
  "agent_id",
  "plan_node_id",
  "outcome",
  "summary",
  "changed_paths",
  "verification",
  "artifacts",
  "priority",
  "severity",
  "state_revision_after",
]

optional = [
  "blockers",
  "warnings",
  "suggested_next_actions",
]

[delivery]
persist_before_enqueue = true
enqueue_parent_inbox = true
trigger_parent_rerun = true
include_full_transcript = false

[tool_state]
reset_tool_choice = true
reset_forced_tool = true
reset_parallel_batch = true
reset_pending_invocation = true
default_next_choice = "auto"
```

The rerun is a new turn with:

```text
same agent identity
same run-tree binding
same plan binding
new turn ID
new trigger
new context compilation
new resolved tool view
reset tool-choice state
```

---

## 14. Agent Families

A family is a reusable design and policy bundle.

```toml
schema = "harwness.family/v1"
id = "harwness.family.focused-coding@1"
version = "1.0.0"

name = "Focused Coding Family"

[orchestrators]
allowed = [
  "harwness.agent.focused-coding-orchestrator@1",
]

[workers]
allowed = [
  "harwness.agent.focused-pure-coding@1",
  "harwness.agent.focused-coding-planning@1",
  "harwness.agent.focused-deps-doc-artifact-researcher@1",
  "harwness.agent.focused-docs-web-researcher@1",
  "harwness.agent.focused-in-code-docs-writer@1",
  "harwness.agent.focused-docs-update@1",
  "harwness.agent.focused-obsidian-vault-update@1",
  "harwness.agent.focused-verification@1",
]

[defaults]
context_policy = "harwness.context.coding-orchestrator@1"
verification_profile = "harwness.verification.rust-workspace@1"

[invariants]
research_before_greenfield_plan = true
contracts_before_fanout = true
disjoint_write_sets = true
workers_are_pure_implementers = true
workspace_barrier_required = true
```

Users can extend the family:

```toml
schema = "harwness.family/v1"
id = "acme.family.crypt-guard-coding@1"
version = "1.0.0"

extends = "harwness.family.focused-coding@1"

[patch.workers.allowed]
append = [
  "acme.agent.rust-pqc-pure-coder@1",
  "acme.agent.crypto-verification@1",
]

[patch.invariants]
append = [
  "secret_types_never_clone",
  "zeroize_on_drop",
  "fips_mapping_evidence_required",
]
```

---

## 15. Root Organization Templates

A RootOrchestrator may instantiate an organization template.

```toml
schema = "harwness.organization/v1"
id = "harwness.organization.software-project@1"
version = "1.0.0"

name = "Software Project Organization"

[root]
agent = "harwness.agent.focused-coding-orchestrator@1"
family = "harwness.family.focused-coding@1"

[[clans]]
id = "research"
name = "Research Clan"
leader = "harwness.agent.focused-coding-orchestrator@1"
family = "harwness.family.focused-coding@1"
plan_scope = "research/*"
child_depth_cost = 1

[[clans]]
id = "implementation"
name = "Implementation Clan"
leader = "harwness.agent.focused-coding-orchestrator@1"
family = "harwness.family.focused-coding@1"
plan_scope = "implementation/*"
child_depth_cost = 1

[[clans]]
id = "verification"
name = "Verification Clan"
leader = "harwness.agent.verification-orchestrator@1"
family = "harwness.family.verification@1"
plan_scope = "verification/*"
child_depth_cost = 1

[[cells]]
id = "provider-adapters"
clan = "implementation"
kind = "fanout"
barrier = "all-terminal"
write_partition = "required"
members_from_plan = "implementation/providers/*"
```

A user may build on it:

```toml
schema = "harwness.organization/v1"
id = "acme.organization.harwness-development@1"
version = "1.0.0"

extends = "harwness.organization.software-project@1"

[[patch.clans.append]]
id = "memory"
name = "Memory Systems Clan"
leader = "harwness.agent.focused-coding-orchestrator@1"
family = "acme.family.harwness-memory@1"
plan_scope = "memory/*"
child_depth_cost = 1

[[patch.clans.append]]
id = "agent-runtime"
name = "Agent Runtime Clan"
leader = "harwness.agent.focused-coding-orchestrator@1"
family = "harwness.family.focused-coding@1"
plan_scope = "agents/*"
child_depth_cost = 1
```

Organization templates may create only descendants inside one Root run-tree.

They may never declare additional roots.

---

## 16. Dynamic User Creation

Users should be able to create definitions through:

- direct TOML editing;
- CLI commands;
- TUI forms;
- an authorized UIA workflow;
- imported agent packs;
- project templates.

Example CLI:

```text
harwness agent new acme.agent.rust-api-worker@1 \
  --extends harwness.agent.focused-pure-coding@1

harwness agent validate acme.agent.rust-api-worker@1

harwness family new acme.family.backend@1 \
  --extends harwness.family.focused-coding@1

harwness org instantiate acme.organization.harwness-development@1 \
  --goal goal_01J...
```

The UIA may help author definitions, but it only proposes file changes.

The runtime validates and persists them.

---

## 17. Definition Compiler Pipeline

TOML definitions are not used directly at runtime.

They pass through a compiler:

```text
discover
→ parse
→ resolve IDs and versions
→ load base
→ apply ordered mixins
→ apply explicit patches
→ validate schemas
→ validate role compatibility
→ validate authority monotonicity
→ validate organization graph
→ resolve policies
→ freeze snapshot
→ compute definition hash
→ compile typed definition
```

Rust representations:

```rust
pub struct RawAgentDefinition {
    pub schema: String,
    pub id: DefinitionId,
    pub version: Version,
    pub extends: Option<DefinitionRef>,
    pub mixins: Vec<DefinitionRef>,
    pub role: AgentRoleId,
    pub specialization: String,
    pub tables: toml::Table,
}

pub struct ResolvedAgentDefinition {
    pub id: DefinitionId,
    pub version: Version,
    pub provenance: ResolutionTrace,
    pub role: AgentRoleId,
    pub specialization: SpecializationId,
    pub context: ResolvedContextPolicy,
    pub tools: ResolvedToolPolicy,
    pub models: ResolvedModelPolicy,
    pub return_contract: ResolvedReturnContract,
    pub authority: AuthorityCeiling,
}

pub struct CompiledAgentDefinition<R, S>
where
    R: AgentRole,
    S: AgentSpecialization<R>,
{
    pub identity: DefinitionIdentity,
    pub config: S::Config,
    pub policies: CompiledPolicies<R>,
    _role: PhantomData<R>,
}
```

At the dynamic boundary:

```rust
pub enum AnyCompiledAgentDefinition {
    UserInterface(CompiledAgentDefinition<UserInterfaceAgent, UserInterfaceSpecialization>),
    RootCoding(CompiledAgentDefinition<RootOrchestrator, FocusedCodingOrchestrator>),
    ChildCoding(CompiledAgentDefinition<ChildOrchestrator, FocusedCodingOrchestrator>),
    PureCoding(CompiledAgentDefinition<Worker, FocusedPureCoding>),
    Planning(CompiledAgentDefinition<Worker, FocusedCodingPlanning>),
    // registered variants or erased validated plugin definitions
}
```

---

## 18. Frozen Run Snapshots

A running agent must not silently change when a user edits its TOML.

At admission, the runtime stores:

```text
definition ID
resolved version
definition hash
full resolved snapshot
policy hashes
model-routing snapshot
organization revision
plan revision
```

Changes affect new instances by default.

An active run may upgrade only through an explicit migration:

```text
propose definition upgrade
→ compute semantic diff
→ identify affected agents
→ validate compatibility
→ pause or drain affected work
→ create new organization revision
→ resume with migrated definitions
```

---

## 19. User Overrides and Safety

Users may override:

- names and descriptions;
- model preferences;
- context budgets within system limits;
- tool subsets;
- verification commands;
- return verbosity;
- family membership;
- organization templates;
- clan structure;
- worker specializations;
- project-specific invariants;
- custom metadata.

Users may not override:

- the closed role set;
- the Root admission rule;
- the spawn matrix;
- parent bindings;
- run-tree identity;
- role-required lifecycle transitions;
- authority ceilings;
- lease and fencing requirements;
- persist-before-rerun ordering;
- tool-choice reset after terminal returns;
- runtime validation.

---

## 20. Diagnostics

Validation errors should be precise and source-aware.

Example:

```text
error[HARW-AUTH-004]:
  definition `acme.agent.experimental-worker@1` attempts to append
  capability `agent.spawn.child-orchestrator`

  workers may not spawn durable agents

  inherited from:
    harwness.agent.worker-base@1

  invalid patch:
    agents/experimental-worker.toml:27:1

  allowed fix:
    remove the capability, or change the definition to a permitted
    child-orchestrator specialization
```

Example organization error:

```text
error[HARW-ORG-011]:
  clan `provider-research` declares a RootOrchestrator as leader

  organizations may contain only the existing root plus descendants;
  a clan leader must be a ChildOrchestrator
```

---

## 21. Recommended Repository Layout

```text
.harwness/
├── agents/
│   ├── rust-pqc-pure-coder.toml
│   ├── provider-researcher.toml
│   └── obsidian-update.toml
├── families/
│   ├── focused-coding.toml
│   └── crypt-guard.toml
├── organizations/
│   ├── software-project.toml
│   └── harwness-development.toml
├── policies/
│   ├── contexts/
│   ├── returns/
│   ├── tools/
│   ├── models/
│   └── verification/
├── mixins/
└── overlays/
    ├── development.toml
    └── release.toml
```

---

## 22. Central Design Invariants

1. Roles are closed and enforced by Rust.
2. User definitions are extensible through composition, not authority invention.
3. Every definition has a stable namespaced ID and version.
4. Each definition has at most one structural base.
5. Mixins are ordered and role-constrained.
6. Merge behavior is explicit.
7. Authority-bearing fields may only narrow inherited authority.
8. Specializations determine work contracts, not role authority.
9. Families are reusable design bundles.
10. Clans are run-local organizational subtrees.
11. Cells are temporary fan-out batches.
12. Organizations never create additional roots.
13. Only the UIA may request Root admission.
14. Workers never spawn durable agents.
15. Worker AgentTools remain bounded tool invocations.
16. Context is compiled per role and turn trigger.
17. Terminal returns persist before parent rerun.
18. Every rerun resets transient tool-choice state.
19. Running agents use frozen resolved snapshots.
20. Configuration changes require explicit migration for active runs.
21. User extensions may add metadata, policies, and templates.
22. Plugins may add registered specializations but cannot bypass role rules.
23. Validation errors must explain inheritance and source provenance.
24. The runtime, not TOML, owns authority, lifecycle, leases, and fencing.

---

## Conclusion

The Harwness Agent Definition DSL should feel expressive and pleasant:

```toml
extends = "harwness.agent.focused-pure-coding@1"

mixins = [
  "harwness.mixin.rust-coding@1",
  "acme.mixin.cryptographic-zeroize@1",
]

[patch.context.must_include]
append = ["project.cryptographic_invariants"]

[patch.tools]
remove = ["web-search"]
```

But the pleasant syntax compiles into hard Rust guarantees:

```text
User configuration
→ validated composition
→ authority reduction
→ frozen definition snapshot
→ typed agent instance
```

Users may build entire ecosystems of custom agents, families, clans, cells, and organization templates on top of Harwness.

They may extend how agents work.

They may not extend what the role hierarchy is allowed to do.
