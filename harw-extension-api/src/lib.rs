//! `harw-extension-api` — Contributor-Traits + Capabilities + Registry.
//!
//! Das Herz des Agents-SDK-Stils: Der Runtime-Core komponiert gegen diese
//! Traits, nicht gegen konkrete Features. Zwei Achsen — **Capabilities**
//! (was eine Extension DARF) und **Contributors** (wo sie sich EINKLINKT).

#![forbid(unsafe_code)]

pub mod allow_rules;
pub mod approval_mode;
pub mod capabilities;
pub mod contributors;
pub mod error;
pub mod lenient;
pub mod registry;
pub mod types;
pub mod v1_compat;

// Re-export harw-tools types so extensions see the same vocabulary
pub use harw_tools::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolName, ToolOutput,
    ToolSpec,
};

// Reexport, damit `#[context_provider]` die Vertrauensklasse nennen kann,
// ohne dass jede Crate, die das Makro benutzt, selbst an `harw-context`
// hängen muss. Der Trait `ContextProvider` trägt `max_trust()` -- der
// Typ gehört damit zur Fläche dieser Crate, nicht zu ihrer Innerei.
pub use harw_context::TrustClass;
pub use allow_rules::{AllowRuleSet, ApprovalRule, RuleDecision, RuleScope, derive_shell_rule};
pub use approval_mode::ApprovalMode;
pub use capabilities::{AgentSpawnError, AgentSpawner, SpawnFuture, SpawnInput};
pub use contributors::{
    ApprovalDecision, ApprovalHandler, ContextProvider, ExtFuture, InstructionsProvider,
    ToolProvider, TurnObserver,
};
pub use error::{ExtensionError, ExtensionResult};
pub use registry::{ExtensionRegistry, ExtensionRegistryBuilder, empty_extension_registry};
pub use types::{
    ContextFragment, LoadedInstructions, TurnInputContext, TurnStartInput, TurnStopInput,
};
pub use v1_compat::fragment_from_v1;
