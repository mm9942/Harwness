//! macOS-only implementation: live process supervision, recovery checks and
//! rlimit application. Compiled only for `target_os = "macos"`.

mod process;
mod recovery;
mod rlimit;

pub use process::{ChildStdio, DarwinProcess};
pub use rlimit::apply_rlimits_to_current_process;

use rustix::process::{Pid, Signal};

use crate::error::ProcessError;
use crate::signal::SignalKind;

/// Validates a diagnostic PID and converts it for rustix.
pub(crate) fn to_pid(raw: u32) -> Result<Pid, ProcessError> {
    i32::try_from(raw)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or(ProcessError::InvalidPid { pid: raw })
}

/// Maps the closed signal set to rustix.
pub(crate) const fn to_rustix(signal: SignalKind) -> Signal {
    match signal {
        SignalKind::Term => Signal::TERM,
        SignalKind::Kill => Signal::KILL,
        SignalKind::Int => Signal::INT,
        SignalKind::Hup => Signal::HUP,
        SignalKind::Quit => Signal::QUIT,
        SignalKind::Usr1 => Signal::USR1,
        SignalKind::Usr2 => Signal::USR2,
        SignalKind::Stop => Signal::STOP,
        SignalKind::Cont => Signal::CONT,
    }
}

#[cfg(test)]
mod tests {
    use super::{to_pid, to_rustix};
    use crate::error::ProcessError;
    use crate::signal::SignalKind;

    #[test]
    fn test_signal_numbers_match_the_macos_headers() {
        for kind in [
            SignalKind::Term,
            SignalKind::Kill,
            SignalKind::Int,
            SignalKind::Hup,
            SignalKind::Quit,
            SignalKind::Usr1,
            SignalKind::Usr2,
            SignalKind::Stop,
            SignalKind::Cont,
        ] {
            assert_eq!(kind.as_raw(), to_rustix(kind).as_raw(), "{}", kind.name());
        }
    }

    #[test]
    fn test_to_pid_rejects_zero_and_out_of_range() {
        assert!(matches!(
            to_pid(0),
            Err(ProcessError::InvalidPid { pid: 0 })
        ));
        assert!(matches!(
            to_pid(u32::MAX),
            Err(ProcessError::InvalidPid { .. })
        ));
        assert!(to_pid(1).is_ok());
    }
}
