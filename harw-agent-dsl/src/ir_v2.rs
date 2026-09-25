//! Agent IR v2 — the typed, serializable, versioned result of lowering
//! (`docs/design/agent-ir-v1.md` §6, ADR 0001, #22 wave 1).
//!
//! [`AgentIr`] is the complete lowering of a resolved agent definition. It
//! carries schema [`AGENT_IR_SCHEMA`], derives `Serialize`/`Deserialize` and
//! rejects unknown fields on every struct (`#[serde(deny_unknown_fields)]`).
//! [`crate::lower_v2::lower_v2`] produces it; [`crate::ExecutableAgentIr`]
//! is a view built from it (`From<&AgentIr>`), so existing consumers keep
//! working.
//!
//! # Opaque strings
//! Only values that are genuinely open stay strings: tool labels, capability
//! labels, selector patterns, work modes, provider/model IDs, validator
//! labels. Closed vocabularies are enums: [`Effort`], [`ReturnContract`],
//! [`Interface`], [`NetworkMode`], section strength/detail/trust.
//!
//! # Snapshot hash v7
//! [`AgentIr::compute_snapshot`] hashes the canonical serialization
//! ([`AgentIr::canonical_json`]) with BLAKE3 under the domain tag
//! [`AGENT_IR_SNAPSHOT_DOMAIN`]. Canonical means: compact JSON, struct fields
//! in declaration order, set-valued lists sorted (authority capabilities,
//! admitted and forbidden tools; the permission manifest is sorted by
//! construction), order-carrying lists in declaration order (validators,
//! context sections and selectors, skills, interfaces), and `trace` plus
//! `snapshot` left out. There are no maps in the IR, so no map ordering can
//! leak into the hash. Unlike v6 the hash covers the definition version, the
//! instruction text hash, the skill content hashes, the binary settings and
//! the permission manifest.
//!
//! # Concurrency
//! All types are `Send + Sync` and immutable after construction.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::context_program::SectionStrength;
use crate::diagnostics::Diagnostic;
use crate::ids::{DefinitionId, Version};
use crate::roles::AgentRoleId;

/// Schema label of [`AgentIr`].
pub const AGENT_IR_SCHEMA: &str = "harwness.agent-ir/v2";

/// Domain tag of the v7 snapshot hash.
pub const AGENT_IR_SNAPSHOT_DOMAIN: &str = "harwness.agent-ir.snapshot/v7";

/// Reasoning-effort level (`[models] effort`, `reasoning_effort`,
/// `[spawn.budget] effort_cap`).
///
/// # Description
/// Mirrors `harw_types::ReasoningEffort` by label; this crate does not depend
/// on `harw-types`. An unknown label is `HARW-MODEL-001`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    /// `minimal`
    Minimal,
    /// `low`
    Low,
    /// `medium`
    Medium,
    /// `high`
    High,
    /// `xhigh`
    Xhigh,
    /// `max`
    Max,
}

impl Effort {
    /// Every level, lowest first.
    pub const ALL: [Effort; 6] = [
        Effort::Minimal,
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Xhigh,
        Effort::Max,
    ];

    /// The label (`"low"`, …).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Effort::Minimal => "minimal",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }

    /// Parses an exact label; `None` for anything else.
    #[must_use]
    pub fn parse(label: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|effort| effort.as_str() == label)
    }
}

/// The known return contracts (`[return] contract`).
///
/// # Description
/// Serialized as the contract label. The first three are validated
/// structurally by `harw_core_bridge::ChildReturnContract`
/// (research finding, envelope, security verdict); the others are
/// text contracts whose label documents the expected shape. A label outside
/// this list is `HARW-RETURN-001` — it no longer falls back to free text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReturnContract {
    /// `harwness.return.research-finding@1`
    #[serde(rename = "harwness.return.research-finding@1")]
    ResearchFinding,
    /// `harwness.return.envelope@1`
    #[serde(rename = "harwness.return.envelope@1")]
    Envelope,
    /// `harwness.security-verdict/v1`
    #[serde(rename = "harwness.security-verdict/v1")]
    SecurityVerdict,
    /// `harwness.return.execution-summary@1`
    #[serde(rename = "harwness.return.execution-summary@1")]
    ExecutionSummary,
    /// `harwness.return.plan-proposal@1`
    #[serde(rename = "harwness.return.plan-proposal@1")]
    PlanProposal,
    /// `harwness.return.coding-task@1`
    #[serde(rename = "harwness.return.coding-task@1")]
    CodingTask,
    /// `harwness.matrix.game-report@1`
    #[serde(rename = "harwness.matrix.game-report@1")]
    MatrixGameReport,
    /// `harwness.matrix.market-estimate@1`
    #[serde(rename = "harwness.matrix.market-estimate@1")]
    MatrixMarketEstimate,
    /// `harwness.matrix.player-move@1`
    #[serde(rename = "harwness.matrix.player-move@1")]
    MatrixPlayerMove,
    /// `harwness.matrix.red-cell-objection@1`
    #[serde(rename = "harwness.matrix.red-cell-objection@1")]
    MatrixRedCellObjection,
    /// `harwness.matrix.umpire-ruling@1`
    #[serde(rename = "harwness.matrix.umpire-ruling@1")]
    MatrixUmpireRuling,
}

impl ReturnContract {
    /// Every known contract.
    pub const ALL: [ReturnContract; 11] = [
        ReturnContract::ResearchFinding,
        ReturnContract::Envelope,
        ReturnContract::SecurityVerdict,
        ReturnContract::ExecutionSummary,
        ReturnContract::PlanProposal,
        ReturnContract::CodingTask,
        ReturnContract::MatrixGameReport,
        ReturnContract::MatrixMarketEstimate,
        ReturnContract::MatrixPlayerMove,
        ReturnContract::MatrixRedCellObjection,
        ReturnContract::MatrixUmpireRuling,
    ];

    /// The contract label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ReturnContract::ResearchFinding => "harwness.return.research-finding@1",
            ReturnContract::Envelope => "harwness.return.envelope@1",
            ReturnContract::SecurityVerdict => "harwness.security-verdict/v1",
            ReturnContract::ExecutionSummary => "harwness.return.execution-summary@1",
            ReturnContract::PlanProposal => "harwness.return.plan-proposal@1",
            ReturnContract::CodingTask => "harwness.return.coding-task@1",
            ReturnContract::MatrixGameReport => "harwness.matrix.game-report@1",
            ReturnContract::MatrixMarketEstimate => "harwness.matrix.market-estimate@1",
            ReturnContract::MatrixPlayerMove => "harwness.matrix.player-move@1",
            ReturnContract::MatrixRedCellObjection => "harwness.matrix.red-cell-objection@1",
            ReturnContract::MatrixUmpireRuling => "harwness.matrix.umpire-ruling@1",
        }
    }

    /// Parses an exact label; `None` for an unknown contract.
    #[must_use]
    pub fn parse(label: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|contract| contract.as_str() == label)
    }

    /// `true` for contracts the runtime validates structurally
    /// (research finding, envelope, security verdict).
    #[must_use]
    pub const fn is_structured(self) -> bool {
        matches!(
            self,
            ReturnContract::ResearchFinding
                | ReturnContract::Envelope
                | ReturnContract::SecurityVerdict
        )
    }
}

/// The known return validators (`[return] validators`, #22 wave 1B).
///
/// # Description
/// A validator is a cheap structural check the runtime runs on a child's
/// final answer before the contract is evaluated
/// (`harw_core_bridge::return_validators`). The IR keeps validators as
/// labels (declaration order is semantic); lowering rejects a label outside
/// [`ReturnValidator::ALL`] with `HARW-RETURN-002`, so a compiled agent never
/// names a check the runtime cannot run.
///
/// | Label | Check |
/// |---|---|
/// | `non-empty` | the answer contains at least one non-whitespace character |
/// | `json` | the answer (optionally inside one ```` ``` ```` fence) is valid JSON |
/// | `json-object` | like `json`, and the top-level value is an object |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReturnValidator {
    /// `non-empty`
    NonEmpty,
    /// `json`
    Json,
    /// `json-object`
    JsonObject,
}

impl ReturnValidator {
    /// Every known validator.
    pub const ALL: [ReturnValidator; 3] = [
        ReturnValidator::NonEmpty,
        ReturnValidator::Json,
        ReturnValidator::JsonObject,
    ];

    /// The label (`"non-empty"`, …).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ReturnValidator::NonEmpty => "non-empty",
            ReturnValidator::Json => "json",
            ReturnValidator::JsonObject => "json-object",
        }
    }

    /// Parses an exact label; `None` for an unknown validator.
    #[must_use]
    pub fn parse(label: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|validator| validator.as_str() == label)
    }

    /// The known labels joined by `", "` (for help texts).
    #[must_use]
    pub fn known_labels() -> String {
        Self::ALL
            .iter()
            .map(|validator| validator.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// An interface a compiled agent binary can expose (`[binary] interfaces`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Interface {
    /// One-shot command line.
    Cli,
    /// Interactive loop in the terminal.
    Repl,
    /// MCP server.
    Mcp,
    /// HTTP/JSON API.
    Http,
    /// Mini TUI.
    Tui,
}

impl Interface {
    /// Every interface.
    pub const ALL: [Interface; 5] = [
        Interface::Cli,
        Interface::Repl,
        Interface::Mcp,
        Interface::Http,
        Interface::Tui,
    ];

    /// The label (`"cli"`, …).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Interface::Cli => "cli",
            Interface::Repl => "repl",
            Interface::Mcp => "mcp",
            Interface::Http => "http",
            Interface::Tui => "tui",
        }
    }

    /// Parses an exact label.
    #[must_use]
    pub fn parse(label: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|interface| interface.as_str() == label)
    }
}

/// How a compiled binary runs its child agents (`[binary] child_execution`).
///
/// # Description
/// `Job` starts each child as a separate process through the job system
/// (`harw-tool-job`); this is the default for compiled binaries. `InProcess`
/// keeps the legacy in-process spawner, which is how harw itself always
/// runs its children regardless of this setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChildExecution {
    /// Each child runs as a separate job-managed process (the default).
    #[default]
    Job,
    /// Children run in-process, like inside harw itself.
    InProcess,
}

impl ChildExecution {
    /// The label (`"job"`, `"in-process"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ChildExecution::Job => "job",
            ChildExecution::InProcess => "in-process",
        }
    }

    /// Parses an exact label.
    #[must_use]
    pub fn parse(label: &str) -> Option<Self> {
        match label {
            "job" => Some(ChildExecution::Job),
            "in-process" => Some(ChildExecution::InProcess),
            _ => None,
        }
    }

    /// `true` for [`ChildExecution::Job`], the default that the snapshot
    /// hash and the golden IR JSON must not see (`#[serde(skip_serializing_if)]`).
    #[must_use]
    pub const fn is_default(&self) -> bool {
        matches!(self, ChildExecution::Job)
    }
}

/// Network mode of the permission manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkMode {
    /// No network tool is admitted.
    Off,
    /// Network tools are admitted; only [`NetworkPermissions::hosts`] may be
    /// reached (an empty list reaches nothing unless the runtime's own egress
    /// policy is narrower still — the rule is min(manifest, runtime)).
    Allowlist,
}

/// The authority ceiling (`[authority] capabilities`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Authority {
    /// Capability labels in resolution order (sorted only for hashing).
    pub capabilities: Vec<String>,
}

/// The instruction text of the agent.
///
/// # Description
/// From `instructions_file` (relative to the agent directory of the file
/// that sets it) or, without that key, from `system.md` next to the
/// definition if the loader finds one. `source` is the relative file name as
/// declared, never an absolute path (the hash must not depend on where the
/// definition lives). `blake3` is the hex digest of `text`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instructions {
    /// The instruction text; empty if the agent has none.
    pub text: String,
    /// Relative source file name, `None` without instructions.
    pub source: Option<String>,
    /// BLAKE3 hex digest of `text`.
    pub blake3: String,
}

impl Instructions {
    /// Builds instructions and computes their digest.
    #[must_use]
    pub fn new(text: String, source: Option<String>) -> Self {
        let blake3 = blake3::hash(text.as_bytes()).to_hex().to_string();
        Self {
            text,
            source,
            blake3,
        }
    }

    /// No instructions (empty text, no source).
    #[must_use]
    pub fn none() -> Self {
        Self::new(String::new(), None)
    }
}

/// Admitted and forbidden tools (`[tools]`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSurface {
    /// Admitted tool labels, declaration order.
    pub admitted: Vec<String>,
    /// Forbidden tool labels, declaration order.
    pub forbidden: Vec<String>,
}

/// Resource budget of a spawned session (`[spawn.budget]`).
///
/// # Description
/// `None` means "the definition makes no statement" — the runtime applies
/// its own conservative default, never "unlimited".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    /// Model tokens over the whole session.
    pub max_tokens: Option<u64>,
    /// Tool calls over the whole session.
    pub max_tool_calls: Option<u32>,
    /// Wall-clock seconds.
    pub max_wall_secs: Option<u64>,
    /// Highest effort level.
    pub effort_cap: Option<Effort>,
}

/// Spawn parameters (`[spawn]`, `[delegation]`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnContract {
    /// Maximum spawn depth below this agent (`None`: runtime limit).
    pub max_depth: Option<u32>,
    /// Exact child-orchestrator role names this agent may create.
    pub child_orchestrators: Vec<String>,
    /// `[delegation] targets`: named delegation targets (a filter over the
    /// visible targets, never an extension); `None` without the table.
    pub delegation_targets: Option<Vec<String>>,
    /// Opaque workspace hint.
    pub workspace_hint: Option<String>,
    /// `None` if `[spawn.budget]` is absent; `Some` (possibly all-`None`)
    /// if it is present — two different statements.
    pub budget: Option<Budget>,
}

/// Shape of the work unit (`[job]`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    /// Symbolic goal kind.
    pub goal_kind: Option<String>,
}

/// Lifecycle transitions (`[lifecycle]`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lifecycle {
    /// Whether the agent may pause (default `false`, the fail-closed side).
    pub allow_pause: bool,
    /// Whether the agent may be re-run.
    pub allow_rerun: bool,
    /// Total attempts including the first (`None`: runtime limit).
    pub max_attempts: Option<u32>,
}

/// One section of a bound context program.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextSection {
    /// Section name, e.g. `"history.tail"`.
    pub name: String,
    /// Strength (normal or must-include).
    pub strength: SectionStrength,
    /// Rendering detail.
    pub detail: harw_context::DetailMode,
    /// Minimum trust class.
    pub trust: harw_context::TrustClass,
}

/// Context assembly (`[context]` plus the bound program).
///
/// # Description
/// `must_include`/`exclude` are the effective selectors the runtime checks
/// at child start (program sections inside the ceiling, then inline
/// selectors). `sections` is the complete bound program — strength, detail
/// and trust of every section, which the legacy lowering dropped; `deferred`
/// names the program's must-include sections outside the ceiling.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextProgram {
    /// Bound library name (`"explore"`), `None` without a binding.
    pub program: Option<String>,
    /// Canonical ID of the bound program.
    pub program_id: Option<String>,
    /// Effective context policy label.
    pub policy: Option<String>,
    /// All sections of the bound program, declaration order.
    pub sections: Vec<ContextSection>,
    /// Effective must-include selectors, declaration order.
    pub must_include: Vec<String>,
    /// Effective exclusions, declaration order.
    pub exclude: Vec<String>,
    /// Must-include sections of the program outside the ceiling.
    pub deferred: Vec<String>,
}

/// Return handling (`[return]`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReturnPipeline {
    /// The return contract; `None` if the definition names none.
    pub contract: Option<ReturnContract>,
    /// Validator labels, declaration order.
    pub validators: Vec<String>,
}

/// A `provider/model` reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRef {
    /// Provider ID.
    pub provider: String,
    /// Model ID at that provider.
    pub model: String,
}

/// Model preferences (`[models]`, DSL §8.1).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Models {
    /// Preferred provider ID.
    pub provider: Option<String>,
    /// Preferred model ID.
    pub model: Option<String>,
    /// Preferred effort.
    pub effort: Option<Effort>,
    /// Fallbacks in order.
    pub fallbacks: Vec<ModelRef>,
    /// Environment variables the agent needs (names only).
    pub required_env: Vec<String>,
}

/// Execution limits (`[limits]`, DSL §8).
///
/// # Description
/// No built-in definition sets `[limits]` today; the keys are the ones the
/// DSL documents (§7, §8).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Tool calls.
    pub max_tool_calls: Option<u32>,
    /// Agent-tool calls.
    pub max_agent_tool_calls: Option<u32>,
    /// Wall-clock seconds.
    pub max_wall_time_seconds: Option<u64>,
    /// Context tokens.
    pub max_context_tokens: Option<u64>,
}

/// Work contract (`[work]`), exactly the keys the bundled definitions use.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Work {
    /// Work mode label (open vocabulary, e.g. `"analysis"`).
    pub mode: Option<String>,
    /// May write code.
    pub may_write_code: Option<bool>,
    /// May research the web.
    pub may_research_web: Option<bool>,
    /// May change the plan.
    pub may_change_plan: Option<bool>,
    /// Aggregates child returns.
    pub aggregates_child_returns: Option<bool>,
}

/// Research contract (`[research]`), exactly the keys the bundled
/// definitions use.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Research {
    /// Source classes, declaration order.
    pub sources: Vec<String>,
    /// Freshness requirement label.
    pub freshness: Option<String>,
    /// Whether primary sources are required.
    pub primary_sources_required: Option<bool>,
    /// Output format label.
    pub output: Option<String>,
    /// Package ecosystems.
    pub ecosystems: Vec<String>,
    /// May write code.
    pub may_write_code: Option<bool>,
    /// Tradecraft methods.
    pub tradecraft: Vec<String>,
}

/// Verification settings (`[verification]`, DSL §8/§19).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verification {
    /// Verification profile label.
    pub profile: Option<String>,
    /// Verification commands, declaration order.
    pub commands: Vec<String>,
}

/// One skill reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillEntry {
    /// Skill name (`[a-z0-9-]{1,64}`).
    pub name: String,
    /// Content hash, filled by the compiler (`ResolveSkills`); `None` after
    /// lowering.
    pub hash: Option<String>,
}

/// Skills of the agent, resolution order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Skills {
    /// Entries, resolution order.
    pub entries: Vec<SkillEntry>,
}

impl Skills {
    /// The skill names, resolution order.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| entry.name.clone())
            .collect()
    }
}

/// Settings of a compiled binary (`[binary]`, DSL §8.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binary {
    /// Binary name; defaults to the specialization.
    pub name: String,
    /// Interfaces built in by default, declaration order, no duplicates.
    pub interfaces: Vec<Interface>,
    /// Interface used when none is chosen; one of `interfaces`.
    pub default_interface: Interface,
    /// How the binary runs its child agents; `Job` by default.
    #[serde(default, skip_serializing_if = "ChildExecution::is_default")]
    pub child_execution: ChildExecution,
}

/// Filesystem part of the permission manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemPermissions {
    /// Any workspace-reading tool is admitted.
    pub read: bool,
    /// Any workspace-writing tool (`fs.write`, `fs.edit`) is admitted.
    pub write: bool,
    /// Where writes may land: the spawn workspace hint, else the symbolic
    /// [`WORKSPACE_WRITE_PATH`]; empty without write tools.
    pub write_paths: Vec<String>,
    /// Admitted tools that write outside the workspace file tree
    /// (definition/skill proposals, …), sorted.
    pub other_write_tools: Vec<String>,
}

/// Network part of the permission manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkPermissions {
    /// Mode.
    pub mode: NetworkMode,
    /// Admitted network tools, sorted.
    pub tools: Vec<String>,
    /// Reachable hosts from `[network] hosts`, sorted.
    pub hosts: Vec<String>,
}

/// Spawn part of the permission manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnPermissions {
    /// Maximum depth; `0` if the definition states none (conservative).
    pub max_depth: u32,
    /// Child orchestrators, sorted.
    pub child_orchestrators: Vec<String>,
    /// Delegation targets, sorted (empty without `[delegation]`).
    pub delegation_targets: Vec<String>,
}

/// The rights manifest derived from the IR (explicit and conservative).
///
/// # Description
/// Derived, never declared: tools are the admitted minus the forbidden ones,
/// network, write, shell and host flags follow from the tool labels, the
/// budget and spawn limits from `[spawn]`, required environment variables
/// from `[models]`. A runtime grants min(manifest, flags, local rules).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    /// Effective tools (admitted, not forbidden), sorted, unique.
    pub tools: Vec<String>,
    /// Forbidden tools, sorted, unique.
    pub forbidden_tools: Vec<String>,
    /// Authority capabilities, sorted, unique.
    pub capabilities: Vec<String>,
    /// Filesystem access.
    pub filesystem: FilesystemPermissions,
    /// Network access.
    pub network: NetworkPermissions,
    /// A shell or process-starting tool is admitted.
    pub shell: bool,
    /// A host-level tool (outside the sandbox) is admitted.
    pub host: bool,
    /// Spawn limits.
    pub spawn: SpawnPermissions,
    /// Budget.
    pub budget: Option<Budget>,
    /// Required environment variables, sorted, unique.
    pub required_env: Vec<String>,
}

/// Symbolic write path meaning "the session's workspace root".
pub const WORKSPACE_WRITE_PATH: &str = "$WORKSPACE";

/// One resolution step (mirror of [`crate::resolved::ResolutionStep`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceStep {
    /// Canonical ID of the applied definition.
    pub source: String,
    /// `"base"`, `"mixin"` or `"patch"`.
    pub kind: String,
    /// Time of application (RFC 3339).
    #[serde(with = "time::serde::rfc3339")]
    pub applied_at: OffsetDateTime,
}

/// Provenance: resolution steps and non-error diagnostics. Not hashed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trace {
    /// Resolution steps.
    pub steps: Vec<TraceStep>,
    /// Warnings and notes from lowering.
    pub diagnostics: Vec<Diagnostic>,
}

impl Trace {
    /// `true` without steps and diagnostics.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty() && self.diagnostics.is_empty()
    }
}

/// A computed v7 snapshot identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentIrSnapshot {
    /// Always [`AGENT_IR_SNAPSHOT_DOMAIN`] for snapshots of this version.
    pub domain: String,
    /// Lowercase hex BLAKE3 digest.
    pub digest: String,
}

/// The typed, serializable Agent IR v2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentIr {
    /// Always [`AGENT_IR_SCHEMA`].
    pub schema: String,
    /// Definition ID.
    pub id: DefinitionId,
    /// Definition version (part of the hash).
    pub version: Version,
    /// Display name.
    pub name: Option<String>,
    /// Description.
    pub description: Option<String>,
    /// Authoritative role.
    pub role: AgentRoleId,
    /// Non-empty specialization.
    pub specialization: String,
    /// Default reasoning effort.
    pub reasoning_effort: Option<Effort>,
    /// Authority ceiling.
    pub authority: Authority,
    /// Instructions.
    pub instructions: Instructions,
    /// Tool surface.
    pub tools: ToolSurface,
    /// Spawn contract.
    pub spawn: SpawnContract,
    /// Job template.
    pub job: Job,
    /// Lifecycle.
    pub lifecycle: Lifecycle,
    /// Context program.
    pub context: ContextProgram,
    /// Return pipeline (serialized as `return`).
    #[serde(rename = "return")]
    pub return_pipeline: ReturnPipeline,
    /// `[models]`, if present.
    pub models: Option<Models>,
    /// `[limits]`, if present.
    pub limits: Option<Limits>,
    /// `[work]`, if present.
    pub work: Option<Work>,
    /// `[research]`, if present.
    pub research: Option<Research>,
    /// `[verification]`, if present.
    pub verification: Option<Verification>,
    /// Skills.
    pub skills: Skills,
    /// Binary settings.
    pub binary: Binary,
    /// Rights manifest.
    pub permissions: Permissions,
    /// Provenance (not hashed).
    #[serde(default, skip_serializing_if = "Trace::is_empty")]
    pub trace: Trace,
    /// The v7 snapshot of this IR (not hashed itself).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<AgentIrSnapshot>,
}

impl AgentIr {
    /// The form that is hashed: `trace` and `snapshot` removed, set-valued
    /// lists sorted.
    #[must_use]
    fn canonical_form(&self) -> AgentIr {
        let mut canonical = self.clone();
        canonical.trace = Trace::default();
        canonical.snapshot = None;
        canonical.authority.capabilities.sort();
        canonical.tools.admitted.sort();
        canonical.tools.forbidden.sort();
        canonical
    }

    /// The canonical serialization (§ module docs, "Snapshot hash v7").
    ///
    /// # Description
    /// Compact JSON of [`Self::canonical_form`]. Serialization cannot fail
    /// for this type (no maps, no fallible custom serializers among the
    /// hashed fields); should it ever fail, the result is empty and
    /// [`Self::verify_snapshot`] reports a mismatch rather than panicking.
    #[must_use]
    pub fn canonical_json(&self) -> Vec<u8> {
        serde_json::to_vec(&self.canonical_form()).unwrap_or_default()
    }

    /// Computes the v7 snapshot over the canonical serialization.
    #[must_use]
    pub fn compute_snapshot(&self) -> AgentIrSnapshot {
        let mut hasher = blake3::Hasher::new();
        let domain = AGENT_IR_SNAPSHOT_DOMAIN.as_bytes();
        let length = u32::try_from(domain.len()).unwrap_or(u32::MAX);
        hasher.update(&length.to_le_bytes());
        hasher.update(domain);
        hasher.update(&self.canonical_json());
        AgentIrSnapshot {
            domain: AGENT_IR_SNAPSHOT_DOMAIN.to_owned(),
            digest: hasher.finalize().to_hex().to_string(),
        }
    }

    /// Returns `self` with [`Self::snapshot`] set to the computed snapshot.
    #[must_use]
    pub fn with_snapshot(mut self) -> Self {
        self.snapshot = Some(self.compute_snapshot());
        self
    }

    /// `true` if [`Self::snapshot`] is present and equals a fresh
    /// computation (same domain, same digest).
    #[must_use]
    pub fn verify_snapshot(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| *snapshot == self.compute_snapshot())
    }

    /// The skill names, resolution order.
    #[must_use]
    pub fn skill_names(&self) -> Vec<String> {
        self.skills.names()
    }

    /// Clamps this IR under `ceiling` (#22 wave 1B).
    ///
    /// # Description
    /// Same rules as [`crate::ExecutableAgentIr::clamped_to`], applied to the
    /// typed IR so the runtime can keep one clamped `AgentIr` per custom
    /// agent (roster) and derive the legacy view from it:
    /// - tools: admitted only if the ceiling admits them, forbidden is the
    ///   union of both sides;
    /// - `spawn.max_depth`, every budget field: the minimum; the effort cap
    ///   the lower level ([`Effort::ALL`] order);
    /// - `spawn.child_orchestrators`: the intersection;
    /// - `authority`: the intersection if the ceiling names any capability
    ///   (an empty ceiling makes no statement);
    /// - `lifecycle`: pause and rerun only if both sides allow them,
    ///   `max_attempts` the minimum.
    ///
    /// Identity, instructions, skills, context, return pipeline, models and
    /// the other descriptive sections stay those of `self`. The permission
    /// manifest is derived again from the clamped sections and the v7
    /// snapshot recomputed.
    #[must_use]
    pub fn clamped_to(&self, ceiling: &AgentIr) -> AgentIr {
        self.clamped_to_with(ceiling, &[])
    }

    /// Like [`Self::clamped_to`], but treats `extra_allowed` as if the
    /// ceiling admitted (and did not forbid) these tools. Never widens
    /// `self`: a tool `self` does not admit or forbids stays out.
    #[must_use]
    pub fn clamped_to_with(&self, ceiling: &AgentIr, extra_allowed: &[&str]) -> AgentIr {
        let is_extra = |tool: &String| extra_allowed.contains(&tool.as_str());
        let mut forbidden = self.tools.forbidden.clone();
        for tool in &ceiling.tools.forbidden {
            if !forbidden.contains(tool) && !is_extra(tool) {
                forbidden.push(tool.clone());
            }
        }
        let mut admitted: Vec<String> = Vec::new();
        for tool in &self.tools.admitted {
            let allowed = ceiling.tools.admitted.contains(tool) || is_extra(tool);
            if allowed && !forbidden.contains(tool) && !admitted.contains(tool) {
                admitted.push(tool.clone());
            }
        }
        let budget = match (&self.spawn.budget, &ceiling.spawn.budget) {
            (own, None) => own.clone(),
            (None, Some(limit)) => Some(limit.clone()),
            (Some(own), Some(limit)) => Some(Budget {
                max_tokens: min_option(own.max_tokens, limit.max_tokens),
                max_tool_calls: min_option(own.max_tool_calls, limit.max_tool_calls),
                max_wall_secs: min_option(own.max_wall_secs, limit.max_wall_secs),
                effort_cap: lower_effort(own.effort_cap, limit.effort_cap),
            }),
        };
        let child_orchestrators: Vec<String> = self
            .spawn
            .child_orchestrators
            .iter()
            .filter(|name| ceiling.spawn.child_orchestrators.contains(name))
            .cloned()
            .collect();
        let capabilities = if ceiling.authority.capabilities.is_empty() {
            self.authority.capabilities.clone()
        } else {
            let mut kept: Vec<String> = self
                .authority
                .capabilities
                .iter()
                .filter(|capability| ceiling.authority.capabilities.contains(capability))
                .cloned()
                .collect();
            kept.sort();
            kept.dedup();
            kept
        };
        let mut clamped = self.clone();
        clamped.tools = ToolSurface {
            admitted,
            forbidden,
        };
        clamped.spawn.max_depth = min_option(self.spawn.max_depth, ceiling.spawn.max_depth);
        clamped.spawn.budget = budget;
        clamped.spawn.child_orchestrators = child_orchestrators;
        clamped.authority = Authority { capabilities };
        clamped.lifecycle = Lifecycle {
            allow_pause: self.lifecycle.allow_pause && ceiling.lifecycle.allow_pause,
            allow_rerun: self.lifecycle.allow_rerun && ceiling.lifecycle.allow_rerun,
            max_attempts: min_option(self.lifecycle.max_attempts, ceiling.lifecycle.max_attempts),
        };
        clamped.rederive()
    }

    /// Adds `tools` to the admitted tools unless `self` forbids them.
    ///
    /// # Description
    /// For cross-cutting catalogue tools a custom definition need not list
    /// itself; the caller passes only tools the base role admits, so the
    /// ceiling is never exceeded (mirror of
    /// [`crate::ExecutableAgentIr::with_additional_admitted`]).
    #[must_use]
    pub fn with_additional_admitted(&self, tools: &[&str]) -> AgentIr {
        let mut ir = self.clone();
        for tool in tools {
            let tool = (*tool).to_owned();
            if !ir.tools.admitted.contains(&tool) && !ir.tools.forbidden.contains(&tool) {
                ir.tools.admitted.push(tool);
            }
        }
        ir.rederive()
    }

    /// Caps `spawn.max_depth` at `max_depth` (mirror of
    /// [`crate::ExecutableAgentIr::with_max_depth_at_most`]).
    #[must_use]
    pub fn with_max_depth_at_most(&self, max_depth: u32) -> AgentIr {
        let mut ir = self.clone();
        ir.spawn.max_depth = Some(
            ir.spawn
                .max_depth
                .map_or(max_depth, |own| own.min(max_depth)),
        );
        ir.rederive()
    }

    /// Derives the permission manifest again from the current sections
    /// (keeping the declared network hosts) and recomputes the snapshot.
    fn rederive(mut self) -> AgentIr {
        let hosts = self.permissions.network.hosts.clone();
        self.permissions = crate::lower_v2::derive_permissions(
            &self.tools,
            &self.authority,
            &self.spawn,
            self.models.as_ref(),
            &hosts,
        );
        self.with_snapshot()
    }
}

/// Minimum of two optional upper bounds; a missing side is no bound.
fn min_option<T: Ord + Copy>(left: Option<T>, right: Option<T>) -> Option<T> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

/// The lower of two optional effort caps ([`Effort::ALL`] order).
fn lower_effort(own: Option<Effort>, limit: Option<Effort>) -> Option<Effort> {
    let rank = |effort: Effort| {
        Effort::ALL
            .iter()
            .position(|candidate| *candidate == effort)
            .unwrap_or(usize::MAX)
    };
    match (own, limit) {
        (Some(own), Some(limit)) => Some(if rank(own) < rank(limit) { own } else { limit }),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_effort_labels_roundtrip() {
        for effort in Effort::ALL {
            assert_eq!(Effort::parse(effort.as_str()), Some(effort));
        }
        assert_eq!(Effort::parse("High"), None);
        assert_eq!(Effort::parse("ultra"), None);
    }

    #[test]
    fn test_return_contract_labels_roundtrip_and_serde_uses_the_label() {
        for contract in ReturnContract::ALL {
            assert_eq!(ReturnContract::parse(contract.as_str()), Some(contract));
            let json = serde_json::to_string(&contract).unwrap_or_default();
            assert_eq!(json, format!("\"{}\"", contract.as_str()));
        }
        assert_eq!(ReturnContract::parse("harwness.return.nope@1"), None);
        assert!(ReturnContract::SecurityVerdict.is_structured());
        assert!(!ReturnContract::ExecutionSummary.is_structured());
    }

    #[test]
    fn test_interface_labels_roundtrip() {
        for interface in Interface::ALL {
            assert_eq!(Interface::parse(interface.as_str()), Some(interface));
        }
        assert_eq!(Interface::parse("grpc"), None);
    }

    #[test]
    fn test_instructions_hash_is_blake3_of_the_text() {
        let instructions = Instructions::new("hello".to_owned(), Some("system.md".to_owned()));
        assert_eq!(
            instructions.blake3,
            blake3::hash(b"hello").to_hex().to_string()
        );
        assert_ne!(Instructions::none().blake3, instructions.blake3);
    }
}
