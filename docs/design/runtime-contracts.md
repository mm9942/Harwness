# Runtime assembly contracts

> Status: implemented · Last reviewed: 2026-09-24

This is the binding contract for `harw-runtime`, the crate that assembles
every `harw` entry point (TUI, one-shot CLI, web, MCP server, durable jobs,
gateways) into one `RuntimeAssembly`. Code comments across the workspace
refer to sections here as `§runtime`, `§runtime-spec`, `§principal` and
`§extension` — those are the exact anchors below. Signatures are simplified
for readability; the source files are the source of truth.

## runtime

`harw-runtime` sits at dependency layer L12: it may depend on the rest of
the workspace, but the rest of the workspace may not depend on it. Its job
is to be the **one** place that turns a `RuntimeSpec` into a fully wired
`RuntimeAssembly` — no entry point (`harw-cli`, `harw-tui`) is allowed to
assemble sandbox, registry, spawner or approval chain by hand.

```rust
pub struct RuntimeAssembly {
    spec: RuntimeSpec,
    profile: EntryProfile,
    config: Arc<ResolvedConfig>,
    trust_report: ConfigTrustReport,
    project: ProjectContext,
    sandbox: SandboxSpec,
    ceiling: ContextCeiling,
    spawn_context: SpawnContext,
    budget: RootBudget,
    // ... registry, services, stores, approval chain, spawner
}
```

`RuntimeAssembly` is built through `RuntimeAssemblyBuilder`, which the
`assembly` module exposes together with `RootSession`, `RuntimeStores`,
`SessionLifecycleHook`, `TurnLimits` and `default_approval_mode`. Accessors
on `RuntimeAssembly` expose `spec`, `profile`, `config`, `trust_report`,
`project`, `principal`, `spawn_context`, `sandbox`, `ceiling`,
`root_activation`, `budget`, `turn_limits`, `network_scope`, `operations`,
`services`, `model`, `state_store`, `job_store`, `approval_store`,
`spawner`, `root_session_id`, `plan_services`, `memory` and
`approval_mode` (an `ApprovalModeCell`, mutable in place, shared by every
consumer of the assembly). `op_context(surface, session, turn, sandbox)`
builds a per-operation context; `new_root_session` opens exactly one root
session per assembly (registry is handed out once) and only wires an
approval responder when `AskResolution::Interactive` applies. `close_session`
tears the session down; `rights_snapshot()` produces the `RightsSnapshot`
described under `§runtime-spec`.

### Module map

| Module | Responsibility |
|---|---|
| `spec` | `RuntimeSpec`, `EntryKind::profile` (the **only** reduction table), `RightsSnapshot` — `§runtime-spec` |
| `error` | `RuntimeError`, one variant per assembly phase |
| `assembly` | `RuntimeAssembly`, `RuntimeAssemblyBuilder`, `RuntimeNarrowing` |
| `contributors` | `AssemblyContributor`, the extension point — `§extension` |
| `config` | `load_config`, `ConfigTrustReport` — the one place that loads config for every entry |
| `budget` | `RootBudget::from_config`, `child_limits` |
| `sandbox` | `root_sandbox`, `plan_node_sandbox`, `permissions_for_tier` |
| `ceiling` | `root_ceiling` — the context/config-section ceiling |
| `approval` | `ApprovalChain`, `AskResolutionPolicy` |
| `children` | `RuntimeChildRegistryFactory` |
| `services` | `RuntimeServices`, `PlanServices`, `ServiceSurface` |
| `model` | `ModelSource`, `build_root_model[_with_resolver]` |
| `trace` | `new_root_trace` |

The remaining modules (`agent_background_wiring`, `agent_messaging_wiring`,
`agent_result_wiring`, `auto_classifier`, `diary_wiring`, `dream_run`,
`guard_wiring`, `handoff`, `host_escalation_wiring`, `job_ledger`,
`mcp_wiring`, `memory_wiring`, `permission_rules`, `session_title`,
`task_context`, `uia_worker_routing`) wire individual subsystems (memory,
diary, dreams, MCP, guards, handoffs, session titles, host-mode escalation)
into the assembly; they follow the same contributor discipline as
`§extension` and are not separately normative here.

## principal

A `Principal` (`harw-types::principal`) is the trusted identity of whoever
is driving a run: *who*, through *which ingress surface*, at *which
permission tier*.

```rust
pub enum PermissionTier { Observer, Operator, Maintainer, Owner } // totally ordered
pub enum PrincipalKind { Human, Model, Operation, Channel }
pub enum IngressSurface { Tui, Cli, Web, Mcp, Telegram, Gateway, JobWorker, Child }

#[derive(Clone, Debug, PartialEq, Eq, Serialize)] // deliberately NO Deserialize
pub struct Principal { /* kind, id, surface, tier — all private */ }

impl Principal {
    pub fn trusted_ingress(kind: PrincipalKind, id: impl Into<String>, surface: IngressSurface, tier: PermissionTier) -> Self;
    pub fn kind(&self) -> PrincipalKind;
    pub fn id(&self) -> &str;
    pub fn surface(&self) -> IngressSurface;
    pub fn tier(&self) -> PermissionTier;
    /// Derives a child principal: kind `Model`, surface `Child`, tier = min(parent tier, Operator).
    pub fn child_of(&self, role: &str) -> Principal;
    /// Human+Tui → "local-tui", Human+Cli → "local-cli", Human+Web → "owner",
    /// Channel+Mcp → Operator{id}, everything else → None.
    pub fn approval_actor(&self) -> Option<ApprovalActor>;
}
```

A `Principal` is created only at a trusted ingress boundary
(`trusted_ingress`) or derived from a parent via `child_of`, which can only
lower the tier, never raise it. There is deliberately no `Deserialize`: a
principal must never be reconstructed from wire, configuration or model
data — it is minted once at the boundary that authenticated it and carried
by value from there on.

## runtime-spec

`EntryKind` names the *kind* of entry into the runtime; everything it is
allowed to do follows from exactly one function, `EntryKind::profile`. No
other code path may derive sandbox permissions, registry profile, ask
resolution, spawner policy or context ceiling from an entry kind — this
table is authoritative, tested for both declaration (`spec.rs`'s own tests)
and effect (`harw-runtime/tests/rights_matrix.rs`, which builds a real
assembly per entry and inspects what it actually grants).

```rust
pub enum EntryKind {
    Tui, OneShot, LocalEcho, Analyze, Doctor, Web,
    McpServe, JobPrompt, JobPlanNode, GatewayTelegram, GatewayDream,
}
pub enum AskResolution { Interactive, RejectTurn, BlockJob, Fail }
pub enum SpawnerPolicy { None, BuiltinRoles }
pub enum CeilingPolicy { LocalRoot, Closed }
pub enum OperationSurface { AllWithModelTools, CommandsOnly, None }

pub struct EntryProfile {
    pub permissions: PermissionSet,
    pub registry_profile: RegistryProfile,
    pub operations: OperationSurface,
    pub ask: AskResolution,
    pub spawner: SpawnerPolicy,
    pub ceiling: CeilingPolicy,
    /// Whether the project doc cascade (`HARW.md`/`AGENTS.md`/`CLAUDE.md`) and
    /// the host's `project_root`/`cwd` reach the root registry's model
    /// context. `false` for entries whose output is read by a remote or not
    /// locally trusted submitter — the assembly then hands the model a
    /// project context with no docs and a neutral placeholder instead of
    /// host paths.
    pub project_context: bool,
}

impl EntryKind { pub fn profile(self) -> EntryProfile; } // the ONLY reduction table
```

### The reduction table

| Entry | Permissions | Registry / Ops | Ask | Spawner | Ceiling | Project context |
|---|---|---|---|---|---|---|
| Tui | `{R, W, X, N}` | Full + AllWithModelTools | Interactive | BuiltinRoles | LocalRoot | yes |
| OneShot | `{R, W, X, N}` | Full + AllWithModelTools | RejectTurn | BuiltinRoles | LocalRoot | yes |
| LocalEcho | `{R, W, X}` | Full + None | Fail | None | LocalRoot | yes |
| Analyze | `{R, W, X}` | Full + CommandsOnly | Fail | BuiltinRoles | LocalRoot | yes |
| Doctor | `{R, W, X}` | Full + AllWithModelTools | Fail | None | LocalRoot | yes |
| Web | `{R}` | Full + CommandsOnly | Fail | None | Closed | no |
| McpServe | `{}` | NoTools + None | BlockJob | None | Closed | no |
| JobPrompt | `{}` | NoTools + None | BlockJob | None | Closed | no |
| JobPlanNode | `{R, W}` | Full + None | Fail | None | LocalRoot | yes |
| GatewayTelegram | `{R, W}` | WorkspaceEdit + None | Interactive | None | Closed | no |
| GatewayDream | `{}` | NoTools + None | Fail | None | Closed | no |

`R` = `ReadWorkspace`, `W` = `WriteWorkspace`, `X` = `ExecuteProcess`, `N` =
`NetworkAccess`. Only `Tui` and `OneShot` ever carry `N`, and it is
egress-bound: the permission is only the ceiling, the actual allowed hosts
come from `root_network_scope` reading the configured egress allowlist; with
no allowlist the scope is empty and assembly strips the permission again
(fail-closed). The UIA root itself never registers `web.*` tools — network
access is pure, egress-bound pass-through to its children, whose network is
never wider than the root's.

For `Web`, `{R}` is the spec ceiling; the tier-dependent narrowing comes from
`permissions_for_tier`. For `JobPlanNode`, `{R, W}` is the ceiling that the
plan-node contract may narrow further. `GatewayTelegram` reads and writes
inside the chat's bound workspace, with no shell and no network; every
approval is answered by the human via approval buttons in the chat
(`Interactive`), and this entry's approval mode is always `ask` regardless of
configuration. No entry gets secret, plugin or registry-write access.

```rust
pub struct RootBudget { pub max_model_rounds: u32, pub max_total_tokens: u64, pub max_wall: Duration }

pub struct RuntimeSpec {
    pub entry: EntryKind,
    pub home: PathBuf,           // resolved root space (`~/.harw` or `HARW_HOME`)
    pub cwd: PathBuf,
    pub principal: Principal,
    /// The assembly reads NO configuration default for mode: the caller
    /// resolves flag + config itself and sets the result here; `None` leaves
    /// the session's default mode unchanged.
    pub mode_override: Option<InteractionMode>,
    /// As with mode, the caller resolves `--agent` and any config default
    /// itself; `None` means "no named agent", the assembly adds none.
    pub active_agent: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
    /// `Some` takes precedence over `[permissions].default_mode` and the
    /// entry kind's default; `None` lets configuration decide.
    pub approval_override: Option<ApprovalMode>,
    /// Explicit model choice (e.g. `--model`) as a key, model id or alias.
    /// `load_config` validates it and derives the run's default model and
    /// provider from it; an unknown model is a configuration error.
    pub model_override: Option<String>,
}

pub struct RightsSnapshot {
    pub entry: EntryKind,
    pub principal: Principal,
    pub approval_actor: Option<ApprovalActor>,
    pub permissions: Vec<String>,
    pub tools: Vec<String>,
    pub approval_chain: Vec<(&'static str, ApprovalHandlerKind)>,
    /// Tools the config policy (`[policy].require_approval_for`) requires
    /// approval for, sorted and deduplicated.
    pub config_policy_tools: Vec<String>,
    pub ceiling_sections: Vec<String>,
    pub spawner_roles: Vec<String>,
    pub budget: RootBudget,
    pub untrusted_repo: Option<PathBuf>,
}

#[derive(HarwError)]
pub enum RuntimeError { Config, Trust, Discovery, Registry, Sandbox, Provider, Store, Spawner }
```

Every `RuntimeError` variant names one assembly phase and carries a
human-readable `detail`; call sites translate the underlying crate's error
into this shape. `RightsSnapshot` is the introspection surface `harw doctor`
and tests read to confirm what a *built* assembly actually grants — as
opposed to what `EntryKind::profile` merely declares.

## extension

Assembly is deliberately closed: it only knows the building blocks named in
`§runtime-spec`, not an open-ended plugin list. `AssemblyContributor`
(`harw-runtime::contributors`) is the one seam where a subsystem outside
`harw-runtime`'s dependency layer (network policy, a DoD chain, the browser
host, the web UI) can add tools, operations or lifecycle hooks without
`assembly.rs` knowing that crate.

```rust
pub struct AssemblyInputs<'a> {
    pub spec: &'a RuntimeSpec,
    pub profile: &'a EntryProfile,       // from EntryKind::profile — read-only
    pub config: &'a ResolvedConfig,
    pub trust_report: &'a ConfigTrustReport,
    pub project: &'a ProjectContext,     // discovered exactly once
    pub spawn_context: &'a SpawnContext,
    pub budget: &'a RootBudget,
    pub turn_limits: &'a TurnLimits,
    pub approval_mode: &'a ApprovalModeCell,
}

pub struct AssemblyParts {
    pub registry: /* open registry builder */ ExtensionRegistryBuilder,
    pub operations: /* the operation registry so far */ OperationRegistry,
    // ... lifecycle hooks, network scope
}

pub trait AssemblyContributor {
    fn contribute(&self, inputs: &AssemblyInputs<'_>, parts: &mut AssemblyParts) -> RuntimeResult<()>;
}
```

A contribution is **additive and only narrowable, never rights-expanding**:
`AssemblyParts` carries no sandbox, no permissions, no context ceiling and no
approval mode. Everything that grants rights comes exclusively from
`EntryKind::profile`; a contributor can offer tools and register hooks, but
cannot bypass the rights matrix. Contributors run in exactly the order they
were registered on the builder (`RuntimeAssemblyBuilder::contributor`), after
registry, operations and spawner are set up, and **before** `AssemblyParts`
is turned into the final `ExtensionRegistry` and `RuntimeServices` — so a
contributor sees every contribution registered before it.

### `RuntimeNarrowing` — caller-side narrowing, not a rights grant

A caller that needs a *tighter* assembly than an entry's default profile
(e.g. a plan-node job that should only see a read-only tool surface) passes
a `RuntimeNarrowing` to the builder:

```rust
pub struct RuntimeNarrowing {
    /// Desired tool set; must lie within the entry profile's whitelist.
    pub registry_profile: RegistryProfile,
    /// Replaces the registry's default identity entirely; a set
    /// `RuntimeSpec::active_agent` still wins for the agent name.
    pub identity: IdentityOverrides,
    /// Ceiling on sandbox permissions; the root sandbox is INTERSECTED with
    /// this, never replaced.
    pub permissions: PermissionSet,
    /// Binds the root sandbox to exactly this directory instead of the
    /// discovered project root. Must be canonically equal to, or a
    /// descendant of, the discovered root — otherwise `RuntimeError::Sandbox`.
    /// Project discovery, configuration and the trust report stay bound to
    /// the discovered project regardless.
    pub workspace_root: Option<PathBuf>,
}

impl RuntimeAssemblyBuilder {
    pub fn narrowing(self, narrowing: RuntimeNarrowing) -> Self;
}
```

Build semantics are fail-closed (`RuntimeError::Registry` on violation): an
entry whose profile is `Full` only accepts a narrowing to
`Full | ReadOnlyExplore | NoTools`; an entry whose profile is `NoTools` only
accepts `NoTools`. `ReadOnlyExplore` is itself checked to be a subset of
`Full` (including its dependency tools). The resulting sandbox is always a
subset of the profile's permissions. Activation, spawner policy and the
approval chain are untouched by narrowing — it can only shrink the tool
surface, identity and filesystem root, never the entry's rights.

## Configuration and trust

`load_config` (`harw-runtime::config`) is the one place that loads
configuration for every entry. It composes: the trusted layers (root space,
active profile, an explicitly trusted `<cwd>/.harw`) plus a report on any
present-but-untrusted repo-local `.harw`; the project's `settings.toml` (or
the legacy `config.toml`) as the strongest trusted layer;
`discover_config_with_restricted_and_project_settings` to merge the trusted
layers while an untrusted repo layer may only narrow; and
`ResolvedConfig::validate` for reference and plaintext-secret checks. It
returns a `ConfigTrustReport` so the rest of assembly (rights,
context ceiling, `RightsSnapshot::untrusted_repo`) never has to re-derive
trust and no entry can accidentally omit it.

## Budget

`RootBudget` bounds what the **root** agent of a run may spend (model
rounds, tokens, wall-clock time); `child_limits` bounds how many children an
agent may hold concurrently. `ResolvedConfig` currently contributes no
budget fields of its own — `RootBudget::from_config` accepts the
configuration without yet reading a limit from it, so a future `[budget]`
config section has a stable attachment point without changing any call
site. Until then, values come from `RootBudget::from_config`'s own table
(local-trusted root sessions: a fixed round/token/wall-clock ceiling per
`EntryKind`).
