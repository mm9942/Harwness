//! Execute a bounded KILL, wait, KILL retry sequence against stable process handles.
//!
//! [`terminate`] borrows pidfds and returns owned per-target reports. The private
//! [`SignalTarget`] boundary separates sequencing from kernel effects for pure
//! tests. Errors stay local to the affected target. Runtime waiting blocks the
//! calling thread in at most 20 ms polling intervals; no threads or locks are
//! created. Structured diagnostics go through tracing, never stdout.
//!
//! # Examples
//!
//! ```no_run
//! let status = std::process::Command::new("killer")
//!     .args(["--pid", "1234", "--timeout", "3", "--kill-wait", "1", "--yes"])
//!     .status()?;
//! assert!(status.success());
//! # Ok::<(), std::io::Error>(())
//! ```

use crate::error::Result;
use crate::pidfd::PidFd;
use crate::types::{Completion, Outcome};
use rustix::process::Signal;
use std::thread;
use std::time::{Duration, Instant};

// Keep kernel effects behind a narrow statically dispatched test boundary.
trait SignalTarget {
    // Send one signal to the held identity, preserving typed failures.
    fn send(&self, signal: Signal) -> Result<()>;
    // Observe exit without blocking or reaping the process.
    fn exited(&self) -> Result<bool>;
}

// Delegate production operations to the existing safe pidfd wrapper.
impl SignalTarget for PidFd {
    // Preserve the descriptor's signal and error contracts.
    fn send(&self, signal: Signal) -> Result<()> {
        PidFd::send(self, signal)
    }

    // Preserve nonblocking exit observation on the same held identity.
    fn exited(&self) -> Result<bool> {
        PidFd::exited(self)
    }
}

// Retain each borrowed target until a terminal report is assigned.
struct Pending<'a, T> {
    // Original numeric identifier is used only in reporting.
    pid: u32,
    // The borrowed signal backend remains alive throughout termination.
    target: &'a T,
    // None means the target still participates in the current phase.
    result: Option<Outcome>,
}

// Observe only unresolved targets and keep failures isolated per target.
fn check<T: SignalTarget>(items: &mut [Pending<'_, T>], completion: Completion) {
    for item in items.iter_mut().filter(|item| item.result.is_none()) {
        match item.target.exited() {
            Ok(true) => {
                tracing::debug!(pid = item.pid, ?completion, "process exit observed");
                item.result = Some(Outcome {
                    pid: item.pid,
                    outcome: completion,
                    detail: None,
                });
            }
            Ok(false) => {}
            Err(error) if error.is_gone() => {
                item.result = Some(Outcome {
                    pid: item.pid,
                    outcome: Completion::AlreadyExited,
                    detail: None,
                });
            }
            Err(error) => {
                tracing::error!(pid = item.pid, %error, "exit observation failed");
                item.result = Some(Outcome::error(item.pid, &error));
            }
        }
    }
}

// Poll against elapsed time, avoiding Instant overflow for a large duration.
fn wait<T: SignalTarget>(items: &mut [Pending<'_, T>], timeout: Duration, completion: Completion) {
    let started = Instant::now();
    loop {
        check(items, completion);
        let elapsed = started.elapsed();
        if items.iter().all(|item| item.result.is_some()) || elapsed >= timeout {
            break;
        }
        thread::sleep(Duration::from_millis(20).min(timeout.saturating_sub(elapsed)));
    }
}

// Send a typed signal to every unresolved target before beginning its shared wait.
fn send<T: SignalTarget>(items: &mut [Pending<'_, T>], signal: Signal) {
    for item in items.iter_mut().filter(|item| item.result.is_none()) {
        tracing::debug!(pid = item.pid, ?signal, "sending process signal");
        if let Err(error) = item.target.send(signal) {
            item.result = Some(if error.is_gone() {
                Outcome {
                    pid: item.pid,
                    outcome: Completion::AlreadyExited,
                    detail: None,
                }
            } else {
                tracing::error!(pid = item.pid, %error, "signal delivery failed");
                Outcome::error(item.pid, &error)
            });
        }
    }
}

/// Terminate the borrowed target identities and return reports in input order.
///
/// `targets` borrows PID labels and held pidfds without taking ownership. Sends
/// KILL to all live targets, waits `timeout`, and sends KILL again to unresolved
/// targets followed by `kill_wait`. Both durations may be zero.
/// Waiting blocks only the calling thread; no locks or worker threads are used.
///
/// # Errors
/// Per-target signal and polling failures are captured as [`Completion::Error`]
/// with diagnostic details. Missing processes count as already exited. A timeout
/// becomes [`Completion::Survived`], not a claim that KILL was ignored.
///
/// # Examples
///
/// ```no_run
/// let status = std::process::Command::new("killer")
///     .args(["--pid", "1234", "--timeout", "0", "--yes"])
///     .status()?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub(crate) fn terminate(
    targets: &[(u32, &PidFd)],
    timeout: Duration,
    kill_wait: Duration,
) -> Vec<Outcome> {
    terminate_with(targets, timeout, kill_wait)
}

// Share the production state machine with pure in-memory signal backends.
fn terminate_with<T: SignalTarget>(
    targets: &[(u32, &T)],
    timeout: Duration,
    kill_wait: Duration,
) -> Vec<Outcome> {
    let span = tracing::info_span!("termination", count = targets.len());
    let _entered = span.enter();
    let mut pending: Vec<_> = targets
        .iter()
        .map(|(pid, target)| Pending {
            pid: *pid,
            target: *target,
            result: None,
        })
        .collect();
    check(&mut pending, Completion::AlreadyExited);
    tracing::info!(
        timeout_seconds = timeout.as_secs_f64(),
        "starting first KILL phase"
    );
    send(&mut pending, Signal::KILL);
    wait(&mut pending, timeout, Completion::Killed);
    tracing::info!(
        wait_seconds = kill_wait.as_secs_f64(),
        "starting KILL retry phase"
    );
    send(&mut pending, Signal::KILL);
    wait(&mut pending, kill_wait, Completion::KilledAfterRetry);
    pending
        .into_iter()
        .map(|item| match item.result {
            Some(outcome) => outcome,
            None => {
                tracing::warn!(
                    pid = item.pid,
                    "process exit not observed within wait bounds"
                );
                Outcome {
                    pid: item.pid,
                    outcome: Completion::Survived,
                    detail: Some(
                        "Exit not observed after repeated KILL (possibly uninterruptible sleep)"
                            .to_owned(),
                    ),
                }
            }
        })
        .collect()
}

// Test sequencing and error isolation without processes, files or actual waits.
#[cfg(test)]
mod tests {
    use super::{SignalTarget, terminate_with};
    use crate::error::{Error, Result};
    use crate::types::Completion;
    use rustix::process::Signal;
    use std::cell::{Cell, RefCell};
    use std::time::Duration;

    // Enumerate deterministic process reactions used by the stub backend.
    enum Reaction {
        // Exit before any signal is sent.
        AlreadyExited,
        // Exit immediately on the first signal.
        ExitOnFirst,
        // Exit only after receiving a second signal.
        ExitAfterSecond,
        // Remain alive after every signal.
        Survive,
        // Deny signal delivery.
        SendError,
        // Report that the target vanished during signal delivery.
        GoneOnSend,
        // Fail exit observation before a signal can be delivered.
        PollError,
    }

    // Store signal observations and simulated exit without external resources.
    struct Stub {
        // Fixed behavior for this test target.
        reaction: Reaction,
        // Mutable exit state mirrors effects of successful signals.
        exited: Cell<bool>,
        // Ordered signal history verifies retry and skipped targets.
        signals: RefCell<Vec<Signal>>,
    }

    // Construct fresh independent target states for each test case.
    impl Stub {
        // Take ownership of the requested deterministic reaction.
        fn new(reaction: Reaction) -> Self {
            Self {
                reaction,
                exited: Cell::new(false),
                signals: RefCell::new(Vec::new()),
            }
        }
    }

    // Replace kernel calls with deterministic in-memory outcomes.
    impl SignalTarget for Stub {
        // Record all signal attempts, then apply this target's configured reaction.
        fn send(&self, signal: Signal) -> Result<()> {
            self.signals.borrow_mut().push(signal);
            match self.reaction {
                Reaction::ExitOnFirst => {
                    self.exited.set(true);
                    Ok(())
                }
                Reaction::ExitAfterSecond => {
                    if self.signals.borrow().len() >= 2 {
                        self.exited.set(true);
                    }
                    Ok(())
                }
                Reaction::SendError => Err(Error::Permission {
                    reason: "stub denied",
                }),
                Reaction::GoneOnSend => Err(Error::io(
                    "stub send",
                    None,
                    std::io::Error::from_raw_os_error(3),
                )),
                Reaction::AlreadyExited | Reaction::Survive | Reaction::PollError => Ok(()),
            }
        }

        // Report simulated exit, or inject an observation failure.
        fn exited(&self) -> Result<bool> {
            match self.reaction {
                Reaction::AlreadyExited => Ok(true),
                Reaction::PollError => Err(Error::Permission {
                    reason: "stub poll denied",
                }),
                Reaction::ExitOnFirst
                | Reaction::ExitAfterSecond
                | Reaction::Survive
                | Reaction::SendError
                | Reaction::GoneOnSend => Ok(self.exited.get()),
            }
        }
    }

    // A still-running target receives a second KILL and reports exit after retry.
    #[test]
    fn test_terminate_kill_retry() {
        let target = Stub::new(Reaction::ExitAfterSecond);
        let result = terminate_with(&[(42, &target)], Duration::ZERO, Duration::ZERO);
        assert_eq!(
            target.signals.borrow().as_slice(),
            &[Signal::KILL, Signal::KILL]
        );
        assert!(matches!(result.as_slice(), [outcome]
            if outcome.pid == 42 && outcome.outcome == Completion::KilledAfterRetry));
    }

    // An exit observed after the first KILL skips the second signal.
    #[test]
    fn test_terminate_first_kill_exit() {
        let target = Stub::new(Reaction::ExitOnFirst);
        let result = terminate_with(&[(42, &target)], Duration::ZERO, Duration::ZERO);
        assert_eq!(target.signals.borrow().as_slice(), &[Signal::KILL]);
        assert!(matches!(result.as_slice(), [outcome] if outcome.outcome == Completion::Killed));
    }

    // Delivery is not equivalent to observed exit even after KILL.
    #[test]
    fn test_terminate_survives_kill_wait() {
        let target = Stub::new(Reaction::Survive);
        let result = terminate_with(&[(42, &target)], Duration::ZERO, Duration::ZERO);
        assert_eq!(
            target.signals.borrow().as_slice(),
            &[Signal::KILL, Signal::KILL]
        );
        assert!(matches!(result.as_slice(), [outcome] if outcome.outcome == Completion::Survived));
    }

    // An exit found during initial inspection causes no signal attempts.
    #[test]
    fn test_terminate_already_exited() {
        let target = Stub::new(Reaction::AlreadyExited);
        let result = terminate_with(&[(42, &target)], Duration::ZERO, Duration::ZERO);
        assert!(target.signals.borrow().is_empty());
        assert!(
            matches!(result.as_slice(), [outcome] if outcome.outcome == Completion::AlreadyExited)
        );
    }

    // A delivery race ending in ESRCH counts as exit, not an authorization failure.
    #[test]
    fn test_terminate_gone_during_send() {
        let target = Stub::new(Reaction::GoneOnSend);
        let result = terminate_with(&[(42, &target)], Duration::ZERO, Duration::ZERO);
        assert_eq!(target.signals.borrow().as_slice(), &[Signal::KILL]);
        assert!(
            matches!(result.as_slice(), [outcome] if outcome.outcome == Completion::AlreadyExited)
        );
    }

    // One target's failure must not suppress a different target's observed exit.
    #[test]
    fn test_terminate_send_error_isolated() {
        let denied = Stub::new(Reaction::SendError);
        let cooperative = Stub::new(Reaction::ExitOnFirst);
        let result = terminate_with(
            &[(42, &denied), (43, &cooperative)],
            Duration::ZERO,
            Duration::ZERO,
        );
        assert_eq!(denied.signals.borrow().as_slice(), &[Signal::KILL]);
        assert_eq!(cooperative.signals.borrow().as_slice(), &[Signal::KILL]);
        assert!(
            matches!(result.as_slice(), [first, second] if first.pid == 42
            && first.outcome == Completion::Error && first.detail.is_some()
            && second.pid == 43 && second.outcome == Completion::Killed)
        );
    }

    // Poll errors stop signalling that target and retain its diagnostic.
    #[test]
    fn test_terminate_poll_error() {
        let target = Stub::new(Reaction::PollError);
        let result = terminate_with(&[(42, &target)], Duration::ZERO, Duration::ZERO);
        assert!(target.signals.borrow().is_empty());
        assert!(
            matches!(result.as_slice(), [outcome] if outcome.outcome == Completion::Error && outcome.detail.is_some())
        );
    }

    // Empty selection yields an empty report without an artificial error target.
    #[test]
    fn test_terminate_empty_targets() {
        let result = terminate_with::<Stub>(&[], Duration::ZERO, Duration::ZERO);
        assert!(result.is_empty());
    }
}
