//! The platform-neutral executor contract (Job-Runtime-Doc §14).
//!
//! An [`Executor`] turns a validated [`JobSpec`] into one running attempt
//! and reports what happens to it as [`AttemptEvent`]s. It owns no durable
//! state: the coordinator decides every lifecycle transition from the
//! events (Job-Runtime-Doc §13, "events, not state").
//!
//! Recovery (§15) is part of the contract: [`Executor::start`] returns a
//! persistable identity, and after a restart the coordinator asks
//! [`Executor::probe`] what became of it and — only for a verified live
//! process — [`Executor::reattach`]es. An executor must never signal a
//! process whose identity it could not prove.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use harw_job_core::{
    AttemptId, EnforcementState, ExitOutcome, JobSpec, RunnerId, SandboxReport, SandboxRequirement,
};
use harw_types::WorkId;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::mpsc;

use super::error::RuntimeError;

/// Everything an executor needs to know about the attempt besides the spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptContext {
    /// The job the attempt belongs to.
    pub job_id: WorkId,
    /// The attempt (unique per claim; derived from the lease epoch).
    pub attempt_id: AttemptId,
    /// The runner that holds the lease.
    pub runner_id: RunnerId,
    /// The lease's fencing epoch the attempt runs under (executors that
    /// leave artefacts behind label them with it so a stale runner's
    /// leftovers are told apart from the current owner's).
    pub lease_epoch: u64,
    /// Absolute workspace root; `JobSpec::working_dir` is relative to it.
    pub workspace_root: PathBuf,
}

/// What happened to a running attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AttemptEvent {
    /// A chunk of standard output (arbitrary boundaries).
    Stdout(Vec<u8>),
    /// A chunk of standard error (arbitrary boundaries).
    Stderr(Vec<u8>),
    /// A non-terminal supervision failure (signal or read error).
    Error(String),
    /// The primary process ended. Always the last event.
    Exited {
        /// How it ended. [`ExitOutcome::Unknown`] for a reattached process
        /// (only the parent can read the status).
        outcome: ExitOutcome,
        /// Enforcement report known only after the process ran (e.g. the
        /// one the Landlock trampoline wrote before `exec`).
        sandbox: Option<SandboxReport>,
    },
}

/// Sending half of an attempt's event stream (for executor implementors).
#[derive(Debug, Clone)]
pub struct AttemptEventSender {
    sender: mpsc::Sender<AttemptEvent>,
}

impl AttemptEventSender {
    /// Sends one event; `false` once the coordinator stopped listening.
    pub async fn send(&self, event: AttemptEvent) -> bool {
        self.sender.send(event).await.is_ok()
    }

    /// Sends one event from a blocking (non-async) thread; `false` once the
    /// coordinator stopped listening. Must not be called on an async task.
    #[must_use]
    pub fn send_blocking(&self, event: AttemptEvent) -> bool {
        self.sender.blocking_send(event).is_ok()
    }
}

/// Receiving half of an attempt's event stream.
#[derive(Debug)]
pub struct AttemptEvents {
    receiver: mpsc::Receiver<AttemptEvent>,
}

impl AttemptEvents {
    /// Creates a bounded event channel (capacity at least 1).
    #[must_use]
    pub fn channel(capacity: usize) -> (AttemptEventSender, Self) {
        let (sender, receiver) = mpsc::channel(capacity.max(1));
        (AttemptEventSender { sender }, Self { receiver })
    }

    /// The next event; `None` once the executor dropped its sender.
    pub async fn next(&mut self) -> Option<AttemptEvent> {
        self.receiver.recv().await
    }
}

/// Control over a running attempt.
pub trait AttemptControl: Send + Sync {
    /// Requests termination (graceful signal, then hard kill after the
    /// executor's grace period). Idempotent; the attempt still ends with
    /// [`AttemptEvent::Exited`].
    fn cancel(&self);
}

/// A running attempt: its event stream plus its control.
pub struct AttemptRun {
    events: AttemptEvents,
    control: Arc<dyn AttemptControl>,
}

impl fmt::Debug for AttemptRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttemptRun")
            .field("events", &self.events)
            .finish_non_exhaustive()
    }
}

impl AttemptRun {
    /// Pairs an event stream with its control.
    #[must_use]
    pub fn new(events: AttemptEvents, control: Arc<dyn AttemptControl>) -> Self {
        Self { events, control }
    }

    /// The next event; `None` if the executor ended without `Exited`.
    pub async fn next_event(&mut self) -> Option<AttemptEvent> {
        self.events.next().await
    }

    /// Requests termination (see [`AttemptControl::cancel`]).
    pub fn cancel(&self) {
        self.control.cancel();
    }
}

/// Result of [`Executor::start`].
#[derive(Debug)]
pub struct StartedAttempt<I> {
    /// Persistable identity of the primary process; `None` when it could
    /// not be captured (the attempt is then unrecoverable after a restart).
    pub identity: Option<I>,
    /// Enforcement known before the body ran; `None` when the process
    /// itself reports it later (trampoline, see [`AttemptEvent::Exited`]).
    pub sandbox: Option<SandboxReport>,
    /// Diagnostic PID of the primary.
    pub pid: Option<u32>,
    /// The running attempt.
    pub run: AttemptRun,
}

/// What a persisted identity corresponds to now (Job-Runtime-Doc §15).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Probe {
    /// The persisted process is verifiably still running.
    Alive,
    /// The persisted process has terminated.
    Exited,
    /// The PID belongs to something else now. Never signal it.
    Mismatch(String),
    /// Identity cannot be established on this platform/permission level.
    /// Never signal it.
    Unverifiable(String),
}

/// A platform execution backend.
///
/// All methods are called from within a Tokio runtime; implementations may
/// spawn tasks but must not block for long (process spawn and cgroup setup
/// are acceptable).
pub trait Executor: Send + Sync + 'static {
    /// Persistable recovery identity of a started process.
    type Identity: Clone + fmt::Debug + Serialize + DeserializeOwned + Send + Sync + 'static;

    /// Starts one attempt of `spec`.
    ///
    /// When the enforcement is known before spawning and `spec.sandbox` is
    /// [`SandboxRequirement::Required`] but not satisfied, the executor must
    /// return [`RuntimeError::SandboxRequirementNotMet`] **without** running
    /// the job body (see [`check_requirement`]).
    ///
    /// # Errors
    /// Spawn, sandbox, cgroup and requirement failures.
    fn start(
        &self,
        spec: &JobSpec,
        ctx: &AttemptContext,
    ) -> Result<StartedAttempt<Self::Identity>, RuntimeError>;

    /// Checks what became of a persisted identity. Must not signal.
    ///
    /// # Errors
    /// Inspection failures; the coordinator treats them as unverifiable.
    fn probe(&self, identity: &Self::Identity) -> Result<Probe, RuntimeError>;

    /// Resumes supervision of a verified live process after a restart.
    /// Implementations re-verify the identity while holding the handle.
    ///
    /// # Errors
    /// [`RuntimeError::IdentityMismatch`] (nothing was signalled),
    /// [`RuntimeError::ProcessAlreadyExited`], [`RuntimeError::Unsupported`].
    fn reattach(
        &self,
        identity: &Self::Identity,
        ctx: &AttemptContext,
    ) -> Result<AttemptRun, RuntimeError>;

    /// The exit status an attempt recorded out of band (e.g. through an
    /// exit-status shim), for processes whose status the runtime could not
    /// observe directly (reattached or exited while the runner was down).
    fn recorded_exit(&self, ctx: &AttemptContext) -> Option<ExitOutcome> {
        let _ = ctx;
        None
    }

    /// Called once the attempt's outcome is durable; releases per-attempt
    /// artefacts (status files, ...).
    fn finished(&self, ctx: &AttemptContext) {
        let _ = ctx;
    }
}

/// The report of an attempt without any sandbox: every sandbox dimension
/// [`EnforcementState::NotEnforced`], resource limits as given.
#[must_use]
pub fn unsandboxed_report(resource_limits: EnforcementState) -> SandboxReport {
    SandboxReport {
        resource_limits,
        ..SandboxReport::uniform(EnforcementState::NotEnforced)
    }
}

/// Whether the spec asks for any limit that needs a resource domain (cgroup
/// or rlimits). The wall-clock timeout is enforced by the coordinator.
#[must_use]
pub fn requests_resource_limits(spec: &JobSpec) -> bool {
    let resources = &spec.resources;
    resources.memory_max.is_some() || resources.cpu_weight.is_some() || resources.pids_max.is_some()
}

/// Checks `report` against `requirement` before anything runs.
///
/// # Errors
/// [`RuntimeError::SandboxRequirementNotMet`] naming the shortfalls.
pub fn check_requirement(
    requirement: SandboxRequirement,
    report: &SandboxReport,
) -> Result<(), RuntimeError> {
    if report.satisfies(requirement) {
        Ok(())
    } else {
        Err(RuntimeError::SandboxRequirementNotMet {
            report: *report,
            shortfalls: report.shortfalls(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{AttemptEvent, AttemptEvents, check_requirement, unsandboxed_report};
    use crate::coordinator::error::RuntimeError;
    use harw_job_core::{EnforcementState, ExitOutcome, SandboxReport, SandboxRequirement};

    #[test]
    fn requirement_check_fails_closed_only_for_required() {
        let report = unsandboxed_report(EnforcementState::Enforced);
        assert!(check_requirement(SandboxRequirement::None, &report).is_ok());
        assert!(check_requirement(SandboxRequirement::BestEffort, &report).is_ok());
        assert!(matches!(
            check_requirement(SandboxRequirement::Required, &report),
            Err(RuntimeError::SandboxRequirementNotMet { ref shortfalls, .. })
                if shortfalls.as_slice() == ["filesystem", "network", "no_new_privs", "capabilities"]
        ));
        let full = SandboxReport::uniform(EnforcementState::Enforced);
        assert!(check_requirement(SandboxRequirement::Required, &full).is_ok());
    }

    #[tokio::test]
    async fn event_channel_delivers_in_order_and_ends() {
        let (sender, mut events) = AttemptEvents::channel(0);
        let producer = tokio::spawn(async move {
            let _ = sender.send(AttemptEvent::Stdout(b"a".to_vec())).await;
            let _ = sender
                .send(AttemptEvent::Exited {
                    outcome: ExitOutcome::Exited(0),
                    sandbox: None,
                })
                .await;
        });
        assert_eq!(
            events.next().await,
            Some(AttemptEvent::Stdout(b"a".to_vec()))
        );
        assert_eq!(
            events.next().await,
            Some(AttemptEvent::Exited {
                outcome: ExitOutcome::Exited(0),
                sandbox: None
            })
        );
        assert!(producer.await.is_ok());
        assert_eq!(events.next().await, None);
    }
}
