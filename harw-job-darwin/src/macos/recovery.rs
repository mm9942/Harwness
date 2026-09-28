//! Recovery checks on macOS: liveness only, never a verified identity (see
//! [`crate::recovery`] for why).

use rustix::io::Errno;
use rustix::process::test_kill_process;

use super::to_pid;
use crate::error::ProcessError;
use crate::recovery::{DarwinRecoveryIdentity, IdentityCheck};
use crate::signal::SignalKind;

impl DarwinRecoveryIdentity {
    /// Checks the persisted PID with `kill(pid, 0)` (no signal is sent).
    ///
    /// Returns [`IdentityCheck::Gone`] if no process has this PID, otherwise
    /// [`IdentityCheck::Unverifiable`]: the PID may meanwhile belong to an
    /// unrelated process, and Darwin offers this crate no safe way to tell.
    /// `EPERM` means "exists, owned by someone else" and is also
    /// `Unverifiable`.
    ///
    /// # Errors
    /// [`ProcessError::InvalidPid`] for PID `0` or above `i32::MAX`,
    /// [`ProcessError::Os`] for unexpected `kill(2)` errors.
    pub fn check(&self) -> Result<IdentityCheck, ProcessError> {
        let pid = to_pid(self.pid)?;
        match test_kill_process(pid) {
            Ok(()) => Ok(self.unverifiable()),
            Err(errno) if errno == Errno::PERM => Ok(self.unverifiable()),
            Err(errno) if errno == Errno::SRCH => Ok(IdentityCheck::Gone),
            Err(errno) => Err(ProcessError::from_errno(
                "kill(0)",
                self.pid,
                errno.raw_os_error(),
            )),
        }
    }

    /// Would signal the recovered process — and on this build always refuses.
    ///
    /// A recovered PID without verified identity is never signalled
    /// ("PID is never sole authority"). Since Darwin identity verification is
    /// not available (see module docs), every call returns an error and
    /// **no signal is sent**; `_signal` documents what the caller wanted.
    ///
    /// # Errors
    /// [`ProcessError::AlreadyExited`] if the PID is gone,
    /// [`ProcessError::IdentityUnverifiable`] otherwise, plus the errors of
    /// [`DarwinRecoveryIdentity::check`].
    pub fn signal(&self, _signal: SignalKind) -> Result<(), ProcessError> {
        match self.check()? {
            IdentityCheck::Gone => Err(ProcessError::AlreadyExited { pid: self.pid }),
            IdentityCheck::Unverifiable { reason } => Err(ProcessError::IdentityUnverifiable {
                pid: self.pid,
                reason,
            }),
        }
    }

    const fn unverifiable(&self) -> IdentityCheck {
        IdentityCheck::Unverifiable {
            reason: self.unverifiable_reason(),
        }
    }
}
