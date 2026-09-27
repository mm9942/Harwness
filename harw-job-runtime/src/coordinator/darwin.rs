//! Minimal Darwin/macOS executor over `harw-job-darwin`.
//!
//! What it does: spawn the job as leader of its own process group
//! (optionally under `sandbox-exec`), watch its exit on a dedicated thread,
//! terminate the group on cancel. What it does **not** do, honestly
//! reported: no resource limits (`resource_limits` is `not_enforced` when
//! any limit is requested), no stdout/stderr capture (the streams go to
//! `/dev/null`), and no reattach after a restart — Darwin cannot verify a
//! recovered process identity without `unsafe`, so a recovered attempt is
//! always `Lost` and never signalled.

use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use harw_job_core::{EnforcementState, ExitOutcome, JobSpec, SandboxReport, SandboxRequirement};
use harw_job_darwin::{
    DarwinProcess, DarwinRecoveryIdentity, DarwinSandbox, IdentityCheck, ProcessError,
    TerminationPolicy,
};

use super::error::RuntimeError;
use super::executor::{
    AttemptContext, AttemptControl, AttemptEvent, AttemptEvents, AttemptRun, Executor, Probe,
    StartedAttempt, check_requirement, requests_resource_limits, unsandboxed_report,
};

/// How often the watcher thread checks for a cancellation request.
const POLL: Duration = Duration::from_millis(100);

/// Darwin [`Executor`] (see the module docs).
#[derive(Debug, Clone)]
pub struct DarwinExecutor {
    sandbox: DarwinSandbox,
    termination: TerminationPolicy,
}

impl Default for DarwinExecutor {
    fn default() -> Self {
        Self {
            sandbox: DarwinSandbox::unsandboxed(),
            termination: TerminationPolicy::default(),
        }
    }
}

impl DarwinExecutor {
    /// An executor with the given sandbox mode and termination policy.
    #[must_use]
    pub fn new(sandbox: DarwinSandbox, termination: TerminationPolicy) -> Self {
        Self {
            sandbox,
            termination,
        }
    }
}

struct DarwinControl {
    cancel: Arc<AtomicBool>,
}

impl AttemptControl for DarwinControl {
    fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

fn process_error(program: &str, error: ProcessError) -> RuntimeError {
    match error {
        ProcessError::Spawn(source) => RuntimeError::Spawn {
            program: program.to_owned(),
            source,
        },
        other => RuntimeError::Os {
            operation: "darwin process",
            detail: other.to_string(),
        },
    }
}

impl Executor for DarwinExecutor {
    type Identity = DarwinRecoveryIdentity;

    fn start(
        &self,
        spec: &JobSpec,
        ctx: &AttemptContext,
    ) -> Result<StartedAttempt<Self::Identity>, RuntimeError> {
        spec.validate()?;
        let resource_limits = if requests_resource_limits(spec) {
            EnforcementState::NotEnforced
        } else {
            EnforcementState::Enforced
        };
        let sandboxed = spec.sandbox != SandboxRequirement::None;
        let report = if sandboxed {
            SandboxReport {
                resource_limits,
                ..self.sandbox.report_for(&spec.sandbox_profile)
            }
        } else {
            unsandboxed_report(resource_limits)
        };
        check_requirement(spec.sandbox, &report)?;

        let mut base = Command::new(&spec.program);
        base.args(&spec.args)
            .env_clear()
            .current_dir(ctx.workspace_root.join(spec.working_dir.as_str()));
        for (name, value) in &spec.env {
            base.env(name, value);
        }
        let mut command = if sandboxed {
            self.sandbox
                .wrap(&spec.sandbox_profile, &base)
                .map_err(|error| RuntimeError::Sandbox {
                    detail: error.to_string(),
                })?
        } else {
            base
        };
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let (mut process, _stdio) = DarwinProcess::spawn(&mut command)
            .map_err(|error| process_error(&spec.program, error))?;
        let identity = process.recovery_identity();
        let pid = process.pid();

        let (sender, events) = AttemptEvents::channel(8);
        let cancel = Arc::new(AtomicBool::new(false));
        let watcher_cancel = Arc::clone(&cancel);
        let policy = self.termination;
        std::thread::Builder::new()
            .name(format!("harw-job-{pid}"))
            .spawn(move || {
                let outcome = loop {
                    if watcher_cancel.load(Ordering::SeqCst) {
                        break match process.terminate_group(&policy) {
                            Ok(report) => report.outcome.unwrap_or(ExitOutcome::Unknown),
                            Err(error) => {
                                let _ =
                                    sender.send_blocking(AttemptEvent::Error(error.to_string()));
                                ExitOutcome::Unknown
                            }
                        };
                    }
                    match process.wait_timeout(POLL) {
                        Ok(Some(outcome)) => break outcome,
                        Ok(None) => {}
                        Err(error) => {
                            let _ = sender.send_blocking(AttemptEvent::Error(error.to_string()));
                            break ExitOutcome::Unknown;
                        }
                    }
                };
                let _ = sender.send_blocking(AttemptEvent::Exited {
                    outcome,
                    sandbox: None,
                });
            })
            .map_err(|source| RuntimeError::Spawn {
                program: "watcher thread".to_owned(),
                source,
            })?;
        Ok(StartedAttempt {
            identity: Some(identity),
            sandbox: Some(report),
            pid: Some(pid),
            run: AttemptRun::new(events, Arc::new(DarwinControl { cancel })),
        })
    }

    fn probe(&self, identity: &Self::Identity) -> Result<Probe, RuntimeError> {
        Ok(match identity.check() {
            Ok(IdentityCheck::Gone) => Probe::Exited,
            Ok(IdentityCheck::Unverifiable { reason }) => Probe::Unverifiable(reason.to_string()),
            Ok(other) => Probe::Unverifiable(format!("{other:?}")),
            Err(error) => Probe::Unverifiable(error.to_string()),
        })
    }

    fn reattach(
        &self,
        identity: &Self::Identity,
        _ctx: &AttemptContext,
    ) -> Result<AttemptRun, RuntimeError> {
        Err(RuntimeError::IdentityMismatch {
            pid: identity.pid,
            detail: "Darwin cannot verify a recovered process identity".to_owned(),
        })
    }
}
