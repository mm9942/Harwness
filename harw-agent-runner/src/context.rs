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

use std::io;
use std::sync::Arc;

use harw_agent_artifact::Bundle;
use harw_agent_dsl::ir_v2::{AgentIr, ChildExecution};
use harw_runtime::EmbeddedAgent;
use harwness_sdk::{Harwness, HarwnessBuilder};

use crate::args::RunnerArgs;
use crate::error::RunnerError;
use crate::job_child_backend::JobChildBackend;

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
    /// working directory, and — for a root process whose manifest chose the
    /// job-backed child execution (`[binary] child_execution`, `Job` by
    /// default) — a wired [`JobChildBackend`] (#22 wave 3C), so every child
    /// this run admits starts as its own job instead of in-process. Nothing
    /// else: the interface that runs the session sets its own approval
    /// handler, model overrides and event sink on top.
    ///
    /// # Description
    /// Nested child orchestrators use the same job backend, so the entire
    /// bundled delegation closure keeps process isolation at every depth.
    ///
    /// # Errors
    /// [`RunnerError::Sdk`] if [`harwness_sdk::HarwnessBuilder::build`]
    /// rejects the embedded agent (an SDK-level configuration problem, not a
    /// rights or artifact one — both of those already failed earlier);
    /// [`RunnerError::Io`] if the current working directory or the root
    /// space (for the job manager's state directory) cannot be resolved.
    pub fn harwness(&self) -> Result<Harwness, RunnerError> {
        self.builder()?.build().map_err(RunnerError::from)
    }

    /// The common [`HarwnessBuilder`] every interface starts from:
    /// `.embedded(self.agent.clone())`, the current working directory, and
    /// the same conditional [`JobChildBackend`] wiring [`Self::harwness`]
    /// documents. `crate::iface::approval::build_harwness` uses this instead
    /// of repeating the wiring, then adds `.approval_handler_arc(..)` before
    /// `.build()`.
    ///
    /// # Errors
    /// See [`Self::harwness`].
    pub(crate) fn builder(&self) -> Result<HarwnessBuilder, RunnerError> {
        let cwd = std::env::current_dir().map_err(|source| RunnerError::Io {
            context: "read the current working directory",
            source,
        })?;
        let mut builder = Harwness::builder().embedded(self.agent.clone()).cwd(cwd);
        if self.root_ir().binary.child_execution == ChildExecution::Job {
            builder = builder.child_backend(self.job_child_backend()?);
        }
        if let Ok(reply) = std::env::var("HARW_OFFLINE_ECHO") {
            builder = builder.offline_echo(reply);
        }
        Ok(builder)
    }

    /// Builds the job-backed [`harw_core::child_backend::ChildBackend`] for
    /// [`Self::harwness`]: a [`harw_tool_job::JobManager`] rooted at the
    /// resolved root space's state directory (the same one an unset
    /// [`harwness_sdk::HarwnessBuilder::home`] would resolve to), with no
    /// notifier — the job's own progress reaches the parent through the
    /// child protocol frames [`crate::child::run_child`] writes, not through
    /// job notifications.
    ///
    /// # Errors
    /// [`RunnerError::Io`] if the root space cannot be resolved or the job
    /// manager's state directory cannot be created.
    pub(crate) fn job_child_backend(
        &self,
    ) -> Result<Arc<dyn harw_core::child_backend::ChildBackend>, RunnerError> {
        let home = harw_home::home_dir().map_err(|source| RunnerError::Io {
            context: "resolve the root space for the job-backed child backend",
            source: io::Error::other(source),
        })?;
        let config = harw_tool_job::JobManagerConfig::new(home);
        let manager = harw_tool_job::JobManager::new(config, Arc::new(harw_tool_job::NoopNotifier))
            .map_err(|source| RunnerError::Io {
                context: "start the job manager for the job-backed child backend",
                source,
            })?;
        Ok(Arc::new(JobChildBackend::new(manager)))
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
