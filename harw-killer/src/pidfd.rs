//! Safe kernel process handles; ownership belongs to `OwnedFd` inside `PidFd`.
//!
//! Delegates system calls to rustix, never to numeric-PID kill. No threads or
//! locks are created. Kernel permission/availability failures become `Error::Io`.
//! Binary-crate documentation examples are illustrative, not Cargo doctests.
//! # Examples
//! ```no_run
//! let status = std::process::Command::new("killer").args(["--pid", "1234", "--dry-run"]).status()?;
//! # Ok::<(), std::io::Error>(())
//! ```
use crate::error::{Error, Result};
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    process::{
        Pid, PidfdFlags, PidfdGetfdFlags, Signal, pidfd_getfd, pidfd_open, pidfd_send_signal,
    },
};
use std::os::fd::{AsRawFd, OwnedFd};

/// Owned handle to exactly one kernel process instance.
/// The private descriptor stays open until this non-cloneable value is dropped.
pub(crate) struct PidFd(OwnedFd);

/// Opens, shares with the authorized helper, polls and signals owned handles.
impl PidFd {
    /// Open a positive PID, taking ownership of a new kernel handle.
    /// `pid` is copied and must be within `1..=i32::MAX`; callers additionally
    /// enforce application protection for PID 1 and ancestry. Returns an owned
    /// handle whose drop closes the descriptor. Performs one kernel operation.
    /// # Errors
    /// InvalidInput for zero/out-of-range PID; Io for kernel denial or missing PID.
    /// # Examples
    /// ```no_run
    /// let handle = crate::pidfd::PidFd::open(std::process::id())?;
    /// # Ok::<(), crate::error::Error>(())
    /// ```
    pub(crate) fn open(pid: u32) -> Result<Self> {
        let raw = i32::try_from(pid).map_err(|_| Error::InvalidInput {
            field: "pid",
            reason: "out of range".to_owned(),
        })?;
        let pid = Pid::from_raw(raw).ok_or_else(|| Error::InvalidInput {
            field: "pid",
            reason: "must be positive".to_owned(),
        })?;
        pidfd_open(pid, PidfdFlags::empty())
            .map(Self)
            .map_err(|e| Error::io("pidfd_open", None, e.into()))
    }
    /// Duplicate a descriptor deliberately held by the calling parent process.
    /// `parent` is borrowed; `fd` was obtained through AsRawFd and its owner must
    /// remain alive until this returns. The caller waits for the helper to exit.
    /// # Errors
    /// Io if pidfd_getfd is unavailable or ptrace/security policy denies access.
    /// # Returns
    /// A newly owned duplicate. The caller must supply a descriptor known to be
    /// a pidfd; this method does not independently validate its descriptor type.
    /// # Examples
    /// ```no_run
    /// let parent = crate::pidfd::PidFd::open(1234)?;
    /// let target = crate::pidfd::PidFd::duplicate_from(&parent, 7)?;
    /// # Ok::<(), crate::error::Error>(())
    /// ```
    pub(crate) fn duplicate_from(parent: &Self, fd: i32) -> Result<Self> {
        pidfd_getfd(&parent.0, fd, PidfdGetfdFlags::empty())
            .map(Self)
            .map_err(|e| {
                Error::io(
                    "pidfd_getfd (requires ptrace permission; no fallback)",
                    None,
                    e.into(),
                )
            })
    }
    /// Borrow the descriptor number for the helper protocol; ownership stays here.
    /// Returns a nonnegative copied descriptor number, valid only while `self`
    /// remains alive. Infallible; no kernel call, locking or ownership transfer.
    /// # Examples
    /// ```no_run
    /// let handle = crate::pidfd::PidFd::open(std::process::id())?;
    /// let descriptor = handle.raw();
    /// assert!(descriptor >= 0);
    /// # Ok::<(), crate::error::Error>(())
    /// ```
    pub(crate) fn raw(&self) -> i32 {
        self.0.as_raw_fd()
    }
    /// Send a typed signal to this instance, without resolving a numeric PID.
    /// # Errors
    /// Io for permission denial or a process that exited before signalling.
    /// # Arguments and returns
    /// Borrows `self` and copies the typed `signal`. `Ok(())` means signal
    /// submission succeeded, not that the target has exited. No wait is performed.
    /// # Examples
    /// ```no_run
    /// let handle = crate::pidfd::PidFd::open(1234)?;
    /// handle.send(rustix::process::Signal::Term)?;
    /// # Ok::<(), crate::error::Error>(())
    /// ```
    pub(crate) fn send(&self, signal: Signal) -> Result<()> {
        pidfd_send_signal(&self.0, signal)
            .map_err(|e| Error::io("pidfd_send_signal", None, e.into()))
    }
    /// Observe exit without reaping or blocking; zombies count as exited.
    /// # Errors
    /// Io for kernel polling errors; Helper for an invalid descriptor event.
    /// # Arguments and returns
    /// Borrows `self`. `Ok(true)` observes exit readiness; `Ok(false)` means no
    /// exit event was observed. Poll timeout is zero; interrupted polls retry.
    /// # Examples
    /// ```no_run
    /// let handle = crate::pidfd::PidFd::open(std::process::id())?;
    /// let observed_exit = handle.exited()?;
    /// # Ok::<(), crate::error::Error>(())
    /// ```
    pub(crate) fn exited(&self) -> Result<bool> {
        let mut fds = [PollFd::new(&self.0, PollFlags::IN)];
        loop {
            match poll(
                &mut fds,
                Some(&Timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                }),
            ) {
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(Error::io("poll pidfd", None, e.into())),
            }
            let events = fds
                .first()
                .ok_or_else(|| Error::Helper {
                    reason: "poll array is empty".to_owned(),
                })?
                .revents();
            if events.intersects(PollFlags::ERR | PollFlags::NVAL) {
                return Err(Error::Helper {
                    reason: "invalid pidfd poll event".to_owned(),
                });
            }
            return Ok(events.intersects(PollFlags::IN | PollFlags::HUP));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Invalid IDs must never enter kernel signalling semantics.
    #[test]
    fn test_open_rejects_invalid_pid() {
        assert!(matches!(PidFd::open(0), Err(Error::InvalidInput { .. })));
        assert!(matches!(
            PidFd::open(u32::MAX),
            Err(Error::InvalidInput { .. })
        ));
    }
    // Kernel resources require opt-in according to R183.
    #[test]
    #[ignore = "requires Linux procfs and pidfd kernel support"]
    fn test_open_current_process_is_alive() {
        let fd = PidFd::open(std::process::id()).unwrap();
        assert!(!fd.exited().unwrap());
        assert!(fd.raw() >= 0);
    }
}
