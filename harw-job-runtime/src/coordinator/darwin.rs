//! Minimal Darwin/macOS executor over `harw-job-darwin`.
//!
//! What it does: spawn the job as leader of its own process group
//! (optionally under `sandbox-exec`), watch its exit on a dedicated thread,
//! terminate the group on cancel. What it does **not** do, honestly
//! reported: no resource limits (`resource_limits` is `not_enforced` when
//! any limit is requested; a job that sets `require_rlimits` is refused), and
//! no reattach after a restart — Darwin cannot verify a
//! recovered process identity without `unsafe`, so a recovered attempt is
//! always `Lost` and never signalled.
//!
//! Output: stdout and stderr are read on reader threads and delivered as
//! attempt events, appended to files when the submitter asked for them
//! ([`AttemptContext::output_files`]), or handed over as pipes together with
//! stdin ([`AttemptContext::stdio_handoff`]) — the same contract as the Linux
//! executor.

use std::io::Read;
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
    AttemptContext, AttemptControl, AttemptEvent, AttemptEventSender, AttemptEvents, AttemptRun,
    Executor, HandedStdio, Probe, StartedAttempt, check_requirement, requests_resource_limits,
    unsandboxed_report,
};

/// How often the watcher thread checks for a cancellation request.
const POLL: Duration = Duration::from_millis(100);

/// How long output is drained after the primary exited.
const DRAIN: Duration = Duration::from_secs(2);

/// Opens an output file for appending (created `0600`).
fn open_append(path: &std::path::Path) -> Result<Stdio, RuntimeError> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .map(Stdio::from)
        .map_err(|error| RuntimeError::Os {
            operation: "open output file",
            detail: format!("{}: {error}", path.display()),
        })
}

/// Reads `source` on its own thread and delivers every chunk as an event.
fn forward_output<R: Read + Send + 'static>(
    mut source: R,
    sender: AttemptEventSender,
    wrap: fn(Vec<u8>) -> AttemptEvent,
) -> Result<std::thread::JoinHandle<()>, RuntimeError> {
    std::thread::Builder::new()
        .name("harw-job-output".to_owned())
        .spawn(move || {
            let mut buffer = [0_u8; 8 * 1024];
            loop {
                match source.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        if !sender.send_blocking(wrap(buffer[..count].to_vec())) {
                            break;
                        }
                    }
                }
            }
        })
        .map_err(|source| RuntimeError::Spawn {
            program: "output reader".to_owned(),
            source,
        })
}

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
        if spec.resources.require_rlimits && spec.resources.requests_rlimits() {
            return Err(RuntimeError::Unsupported {
                operation: "rlimits",
                detail: "the job requires per-process rlimits, which Darwin does not enforce"
                    .to_owned(),
            });
        }

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
        let handoff = ctx.stdio_handoff.as_ref();
        command.stdin(if handoff.is_some_and(|handoff| handoff.stdin) {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        match &ctx.output_files {
            Some(files) => {
                command
                    .stdout(open_append(&files.stdout)?)
                    .stderr(open_append(&files.stderr)?);
            }
            None => {
                command.stdout(Stdio::piped()).stderr(Stdio::piped());
            }
        }
        if handoff.is_some_and(|handoff| handoff.stdout) {
            command.stdout(Stdio::piped());
        }
        let (mut process, mut stdio) = DarwinProcess::spawn(&mut command)
            .map_err(|error| process_error(&spec.program, error))?;
        let identity = process.recovery_identity();
        let pid = process.pid();
        if let Some(handoff) = handoff {
            // The submitter owns these pipes from here on.
            handoff.deliver(HandedStdio {
                stdin: if handoff.stdin {
                    stdio.stdin.take()
                } else {
                    None
                },
                stdout: if handoff.stdout {
                    stdio.stdout.take()
                } else {
                    None
                },
            });
        }

        let (sender, events) = AttemptEvents::channel(8);
        let mut readers = Vec::new();
        if let Some(stdout) = stdio.stdout.take() {
            readers.push(forward_output(
                stdout,
                sender.clone(),
                AttemptEvent::Stdout,
            )?);
        }
        if let Some(stderr) = stdio.stderr.take() {
            readers.push(forward_output(
                stderr,
                sender.clone(),
                AttemptEvent::Stderr,
            )?);
        }
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
                // Let the readers deliver what is left before the final event;
                // a descendant that keeps a pipe open must not hold it back.
                let drain_until = std::time::Instant::now() + DRAIN;
                while readers.iter().any(|reader| !reader.is_finished())
                    && std::time::Instant::now() < drain_until
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
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
