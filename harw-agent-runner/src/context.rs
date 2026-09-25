//! [`RunnerContext`]: everything one runner invocation carries once the
//! embedded artifact is verified and its rights are narrowed.
//!
//! # Description
//! Built once in [`crate::run_embedded`]/[`crate::run_from_current_exe`],
//! then handed by value to whichever interface (`iface::cli::run`,
//! `iface::mcp::run`, …) or `crate::child::run_child` was chosen. An
//! interface builds its [`harwness_sdk::Harwness`] from
//! [`RunnerContext::harwness`] rather than constructing one itself, so every
//! interface embeds the same agent the same way.

use std::sync::Arc;

use harw_agent_artifact::Bundle;
use harw_agent_dsl::ir_v2::AgentIr;
use harw_runtime::EmbeddedAgent;
use harwness_sdk::Harwness;

use crate::args::RunnerArgs;
use crate::error::RunnerError;

/// One runner invocation: the verified bundle, the embedded agent built
/// from it (with rights already narrowed by the command line), and the
/// parsed arguments.
pub struct RunnerContext {
    /// The verified bundle the embedded artifact decoded to (root IR plus
    /// the delegation closure's agent entries and payload pool).
    pub bundle: Bundle,
    /// The embedded agent: the bundle's root, with
    /// [`harw_runtime::embedded::EffectiveRights`] already narrowed by
    /// [`RunnerArgs::flags`].
    pub agent: Arc<EmbeddedAgent>,
    /// The parsed command line.
    pub args: RunnerArgs,
}

impl RunnerContext {
    /// The root agent's IR (the bundle's own copy, same value as
    /// `self.agent.root_ir()`).
    #[must_use]
    pub fn root_ir(&self) -> &AgentIr {
        self.agent.root_ir()
    }

    /// Builds the embedding SDK's entry point for this invocation:
    /// `Harwness::builder().embedded(self.agent.clone())`, the current
    /// working directory, and nothing else — the interface that runs the
    /// session sets its own approval handler, model overrides and event
    /// sink on top.
    ///
    /// # Errors
    /// [`RunnerError::Sdk`] if the current working directory cannot be
    /// read, or [`harwness_sdk::HarwnessBuilder::build`] rejects the
    /// embedded agent (an SDK-level configuration problem, not a rights or
    /// artifact one — both of those already failed earlier).
    pub fn harwness(&self) -> Result<Harwness, RunnerError> {
        let cwd = std::env::current_dir().map_err(|source| RunnerError::Io {
            context: "read the current working directory",
            source,
        })?;
        Harwness::builder()
            .embedded(self.agent.clone())
            .cwd(cwd)
            .build()
            .map_err(RunnerError::from)
    }
}

#[cfg(test)]
mod tests {
    // `RunnerContext::harwness` needs a real `EmbeddedAgent`, which only
    // exists once `harw-runtime`'s `embedded` module (agent 3, wave 3) is
    // implemented; `crate::verify` and `crate::lib` tests build a
    // `RunnerContext` end to end from a fake artifact and cover this path
    // there instead of duplicating the fixture here.
}
