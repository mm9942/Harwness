//! The compiler passes, in pipeline order (ADR 0001 §2.3):
//!
//! | Pass | Does |
//! |---|---|
//! | [`ValidateRoles`] | the role can be compiled; workers do not delegate; orchestrators have targets |
//! | [`RightsCheck`] | re-classifies the manifest from the capability catalog; manifest ≤ base role ≤ author ceiling |
//! | [`ResolveSkills`] | embeds every skill from the skill index and fills `skills.entries[].hash` |
//! | [`ReachableTools`] | the tool providers (and runner features) the manifest needs; unknown tools |
//! | [`PruneUnusedTools`] | drops tools the compiled agent can never use |
//! | [`ResolveModels`] | required environment variables into the manifest |
//! | [`ChildClosure`] | compiles and embeds every agent the orchestrator can start; child ≤ parent |
//!
//! Each pass is a small struct with [`Pass::name`] and [`Pass::run`]; it
//! reports diagnostics and may change the [`CompileUnit`]. The driver
//! recomputes the v7 snapshot once after the last pass.

mod children;
mod models;
mod prune;
mod reachable;
mod rights;
mod roles;
mod skills;

pub use children::{
    ChildClosure, ChildResolver, ResolvedChild, child_problems,
};
pub use models::{HTTP_TOKEN_ENV, ResolveModels, provider_env};
pub use prune::PruneUnusedTools;
pub use reachable::{ReachableTools, collect_providers};
pub use rights::RightsCheck;
pub use roles::{COMPILABLE_ROLES, ValidateRoles, role_label};
pub use skills::{ResolveSkills, skill_payload_path};

use harw_agent_dsl::Diagnostics;

use crate::unit::CompileUnit;

/// One compiler pass.
pub trait Pass {
    /// Stable pass name (`"rights-check"`, …).
    fn name(&self) -> &'static str;

    /// Runs the pass over `unit`; returns its diagnostics (errors stop the
    /// build after the pass).
    fn run(&self, unit: &mut CompileUnit) -> Diagnostics;
}

/// The pass names in pipeline order.
pub const PASS_NAMES: &[&str] = &[
    "validate-roles",
    "rights-check",
    "resolve-skills",
    "reachable-tools",
    "prune-unused-tools",
    "resolve-models",
    "child-closure",
];
